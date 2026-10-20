//! Read-only HTTP stats endpoint for external dashboards (e.g. Homepage).
//!
//! Deliberately a *separate* axum server on its own port rather than bolted
//! onto teloxide's webhook listener:
//!   * the Telegram hot path stays untouched (zero blast radius there),
//!   * it runs in both webhook and polling mode, and
//!   * it has an independent authentication and capacity boundary.
//!
//! Opt-in: only starts when `STATS_LISTEN` is set (see `Config::from_env`).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::info;

use storage::PgStore;

const MAX_IN_FLIGHT_REQUESTS: usize = 4;
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct StatsState {
    store: Arc<PgStore>,
    token: Arc<str>,
    in_flight: Arc<Semaphore>,
}

/// Serve `GET /stats` until `shutdown` is notified.
pub async fn run(
    store: Arc<PgStore>,
    listen: SocketAddr,
    token: String,
    shutdown: CancellationToken,
) -> Result<()> {
    let state = StatsState {
        store,
        token: token.into(),
        in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS)),
    };
    let app = Router::new()
        .route("/stats", get(stats_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding stats listener on {listen}"))?;
    info!(%listen, "stats endpoint listening on /stats");

    crate::bounded_http::serve(listener, app, async move { shutdown.cancelled().await })
        .await
        .context("stats server error")?;
    Ok(())
}

/// JSON snapshot of a few aggregate counts. Hand-built (integers only, nothing
/// to escape) to avoid pulling serde into this crate just for four numbers.
async fn stats_handler(State(state): State<StatsState>, headers: HeaderMap) -> Response {
    let response = stats_response(state, &headers).await;
    no_store(response)
}

async fn stats_response(state: StatsState, headers: &HeaderMap) -> Response {
    if !has_valid_token(headers, state.token.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(_permit) = state.in_flight.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };

    match tokio::time::timeout(QUERY_TIMEOUT, state.store.stats()).await {
        Ok(Ok(stats)) => {
            let body = format!(
                r#"{{"users":{},"alerts_active":{},"alerts_sent":{},"alerts_total":{}}}"#,
                stats.users, stats.active, stats.sent, stats.total
            );
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        Ok(Err(error)) => {
            tracing::error!(error = ?error, "stats query failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "stats unavailable").into_response()
        }
        Err(_) => {
            tracing::warn!("stats query timed out");
            (StatusCode::GATEWAY_TIMEOUT, "stats unavailable").into_response()
        }
    }
}

fn has_valid_token(headers: &HeaderMap, expected: &[u8]) -> bool {
    let Some(actual) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.as_bytes().strip_prefix(b"Bearer "))
    else {
        return false;
    };
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

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn accepts_only_the_configured_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer 0123456789abcdef0123456789abcdef"),
        );
        assert!(has_valid_token(&headers, TOKEN.as_bytes()));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer 0123456789abcdef0123456789abcdeg"),
        );
        assert!(!has_valid_token(&headers, TOKEN.as_bytes()));
        headers.remove(header::AUTHORIZATION);
        assert!(!has_valid_token(&headers, TOKEN.as_bytes()));
    }

    #[test]
    fn marks_every_response_as_non_cacheable() {
        let response = no_store(StatusCode::UNAUTHORIZED.into_response());
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }
}
