use std::{convert::Infallible, net::SocketAddr, pin::Pin, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header::HeaderName, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use teloxide::{
    prelude::*,
    requests::Request as _,
    stop::{mk_stop_token, StopFlag, StopToken},
    types::{AllowedUpdate, Update, UpdateKind},
    update_listeners::{AsUpdateStream, UpdateListener},
};
use tokio::{
    sync::{mpsc, Semaphore},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;
use url::Url;

const QUEUE_CAPACITY: usize = 128;
const MAX_IN_FLIGHT_REQUESTS: usize = 32;
const MAX_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const WEBHOOK_DELETE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const WEBHOOK_DELETE_TIMEOUT: Duration = Duration::from_millis(100);
const SECRET_HEADER: HeaderName = HeaderName::from_static("x-telegram-bot-api-secret-token");

pub struct WebhookListener {
    updates: ReceiverStream<Result<Update, Infallible>>,
    stop_token: StopToken,
}

impl<'a> AsUpdateStream<'a> for WebhookListener {
    type StreamErr = Infallible;
    type Stream = Pin<&'a mut ReceiverStream<Result<Update, Infallible>>>;

    fn as_stream(&'a mut self) -> Self::Stream {
        Pin::new(&mut self.updates)
    }
}

impl UpdateListener for WebhookListener {
    type Err = Infallible;

    fn stop_token(&mut self) -> StopToken {
        self.stop_token.clone()
    }
}

pub async fn start(
    bot: Bot,
    listen: SocketAddr,
    url: Url,
    secret: String,
) -> Result<(WebhookListener, JoinHandle<Result<()>>)> {
    let tcp_listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding webhook listener on {listen}"))?;

    bot.set_webhook(url.clone())
        .max_connections(8)
        .allowed_updates([AllowedUpdate::Message, AllowedUpdate::CallbackQuery])
        .secret_token(secret.clone())
        .send()
        .await
        .context("registering Telegram webhook")?;

    let (updates_tx, updates_rx) = mpsc::channel(QUEUE_CAPACITY);
    let (stop_token, stop_flag) = mk_stop_token();
    let state = WebhookState {
        secret,
        updates_tx,
        stop_flag: stop_flag.clone(),
        in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS)),
    };
    let path = url.path().to_owned();
    let app = Router::new()
        .route(&path, post(receive_update))
        .with_state(state);

    let server = tokio::spawn(async move {
        let server_result = crate::bounded_http::serve(tcp_listener, app, stop_flag)
            .await
            .context("serving webhook requests");
        // Telegram stops delivering to a dead endpoint on its own, so a failed
        // delete must not turn a clean shutdown into a crash.
        match tokio::time::timeout(WEBHOOK_DELETE_TIMEOUT, bot.delete_webhook().send()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "deleting Telegram webhook failed"),
            Err(_) => tracing::warn!("timed out deleting Telegram webhook"),
        }
        server_result
    });

    Ok((
        WebhookListener {
            updates: ReceiverStream::new(updates_rx),
            stop_token,
        },
        server,
    ))
}

#[derive(Clone)]
struct WebhookState {
    secret: String,
    updates_tx: mpsc::Sender<Result<Update, Infallible>>,
    stop_flag: StopFlag,
    in_flight: Arc<Semaphore>,
}

async fn receive_update(State(state): State<WebhookState>, request: Request) -> Response {
    if !has_valid_secret(request.headers(), state.secret.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if state.stop_flag.is_stopped() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Ok(_permit) = state.in_flight.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };

    match tokio::time::timeout(REQUEST_TIMEOUT, accept_update(state, request.into_body())).await {
        Ok(status) => status.into_response(),
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

async fn accept_update(state: WebhookState, body: Body) -> StatusCode {
    let body = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE,
    };
    let update = match serde_json::from_slice::<Update>(&body) {
        Ok(update) => update,
        Err(error) => {
            tracing::warn!(
                line = error.line(),
                column = error.column(),
                "rejected invalid webhook JSON"
            );
            return StatusCode::BAD_REQUEST;
        }
    };
    if matches!(&update.kind, UpdateKind::Error(_)) {
        tracing::warn!(
            update_id = update.id.0,
            "discarded unsupported webhook update"
        );
        return StatusCode::OK;
    }

    match state.updates_tx.try_send(Ok(update)) {
        Ok(()) => StatusCode::OK,
        Err(mpsc::error::TrySendError::Full(_)) => StatusCode::TOO_MANY_REQUESTS,
        Err(mpsc::error::TrySendError::Closed(_)) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn has_valid_secret(headers: &HeaderMap, expected: &[u8]) -> bool {
    let Some(actual) = headers.get(&SECRET_HEADER) else {
        return false;
    };
    let actual = actual.as_bytes();
    if actual.len() != expected.len() {
        return false;
    }

    actual
        .iter()
        .zip(expected)
        .fold(0_u8, |difference, (actual, expected)| {
            difference | (actual ^ expected)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn state() -> (WebhookState, mpsc::Receiver<Result<Update, Infallible>>) {
        let (updates_tx, updates_rx) = mpsc::channel(QUEUE_CAPACITY);
        let (_, stop_flag) = mk_stop_token();
        (
            WebhookState {
                secret: SECRET.to_owned(),
                updates_tx,
                stop_flag,
                in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS)),
            },
            updates_rx,
        )
    }

    fn request(secret: &str, body: impl Into<Body>) -> Request {
        HttpRequest::post("/")
            .header(SECRET_HEADER.clone(), secret)
            .body(body.into())
            .unwrap()
    }

    fn valid_update(id: u32) -> String {
        format!(
            r#"{{"update_id":{id},"message":{{"message_id":1,"date":0,"chat":{{"id":1,"type":"private"}},"text":"/help"}}}}"#
        )
    }

    /// Start the listener against a fake Telegram API that answers
    /// `deleteWebhook` with `delete_response` (`None` never answers) and
    /// acknowledges every other method, then stop it.
    async fn stop_listener(delete_response: Option<(StatusCode, &'static str)>) -> Result<()> {
        let api = Router::new().fallback(move |request: Request| async move {
            if !request.uri().path().ends_with("/DeleteWebhook") {
                return (StatusCode::OK, r#"{"ok":true,"result":true}"#).into_response();
            }
            match delete_response {
                Some(response) => response.into_response(),
                None => std::future::pending().await,
            }
        });
        let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_url = format!("http://{}/", api_listener.local_addr().unwrap())
            .parse()
            .unwrap();
        tokio::spawn(async move { axum::serve(api_listener, api).await.unwrap() });

        let bot = Bot::new("123:test").set_api_url(api_url);
        let (mut listener, server) = start(
            bot,
            "127.0.0.1:0".parse().unwrap(),
            "https://example.test/hook".parse().unwrap(),
            SECRET.to_owned(),
        )
        .await
        .unwrap();
        listener.stop_token().stop();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("webhook shutdown must be bounded")
            .unwrap()
    }

    #[tokio::test]
    async fn shuts_down_cleanly_when_delete_webhook_fails() {
        let failure = r#"{"ok":false,"error_code":400,"description":"Bad Request: test failure"}"#;
        stop_listener(Some((StatusCode::BAD_REQUEST, failure)))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shuts_down_cleanly_when_delete_webhook_times_out() {
        stop_listener(None).await.unwrap();
    }

    #[tokio::test]
    async fn rejects_wrong_secret_before_reading_body() {
        let (state, mut updates_rx) = state();
        let response = receive_update(
            State(state),
            request("wrong", vec![b'x'; MAX_BODY_BYTES + 1]),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(updates_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn rejects_oversized_body() {
        let (state, mut updates_rx) = state();
        let response = receive_update(
            State(state),
            request(SECRET, vec![b'x'; MAX_BODY_BYTES + 1]),
        )
        .await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(updates_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn rejects_invalid_json_and_discards_unknown_updates() {
        let (state, mut updates_rx) = state();
        let invalid = receive_update(State(state.clone()), request(SECRET, "{".to_owned())).await;
        let unknown = receive_update(
            State(state),
            request(
                SECRET,
                r#"{"update_id":2,"future_update":{"data":"ignored"}}"#,
            ),
        )
        .await;

        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert_eq!(unknown.status(), StatusCode::OK);
        assert!(updates_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn returns_too_many_requests_when_queue_is_full() {
        let (state, _updates_rx) = state();
        for id in 0..QUEUE_CAPACITY {
            let update = serde_json::from_str(&valid_update(id as u32)).unwrap();
            state.updates_tx.try_send(Ok(update)).unwrap();
        }

        let response = receive_update(State(state), request(SECRET, valid_update(999))).await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn queues_a_valid_update() {
        let (state, mut updates_rx) = state();
        let response = receive_update(State(state), request(SECRET, valid_update(42))).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(updates_rx.recv().await.unwrap().unwrap().id.0, 42);
    }

    #[tokio::test]
    async fn rejects_updates_after_stop() {
        let (mut state, mut updates_rx) = state();
        let (stop_token, stop_flag) = mk_stop_token();
        state.stop_flag = stop_flag;
        stop_token.stop();

        let response = receive_update(State(state), request(SECRET, valid_update(1))).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(updates_rx.try_recv().is_err());
    }
}
