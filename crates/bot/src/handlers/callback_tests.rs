use super::*;
use sqlx::{ConnectOptions, PgPool};
use std::sync::Mutex;

/// Returns the captured Telegram requests as (path, body) pairs.
async fn assert_unrelated_message_is_preserved(
    pool: PgPool,
    action: &str,
) -> Vec<(String, String)> {
    let store = PgStore::connect(pool.connect_options().to_url_lossy().as_str(), 5)
        .await
        .unwrap();
    store.upsert_user(42, Language::De).await.unwrap();
    store.upsert_user(43, Language::En).await.unwrap();
    let at = Utc::now() + chrono::Duration::hours(1);
    let make = |user_id, text: &str| NewAlert {
        user_id,
        chat_id: -100,
        chat_type: ChatType::Group,
        scope: AlertScope::Private,
        text: text.into(),
        fire_at: at,
        schedule: botcore::Schedule::one_shot(at, chrono_tz::UTC),
    };
    let own = store.create_alert(make(42, "own reminder")).await.unwrap();
    let other = store
        .create_alert(make(43, "other reminder"))
        .await
        .unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = axum::Router::new().fallback({
        let captured = captured.clone();
        move |request: axum::extract::Request| {
            let captured = captured.clone();
            async move {
                let path = request.uri().path().to_owned();
                let is_send = path.ends_with("/SendMessage");
                let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                    .await
                    .unwrap();
                captured
                    .lock()
                    .unwrap()
                    .push((path, String::from_utf8_lossy(&body).into_owned()));
                if is_send {
                    r#"{"ok":true,"result":{"message_id":1000,"date":1,"chat":{"id":-100,"type":"group"},"text":"confirmed"}}"#
                } else {
                    r#"{"ok":true,"result":true}"#
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let query: CallbackQuery = serde_json::from_value(serde_json::json!({
        "id": "altered-callback",
        "from": {"id": 42, "is_bot": false, "first_name": "Test"},
        "chat_instance": "test",
        "data": format!("{action}:{}", own.id),
        "message": {
            "message_id": 999, "date": 1,
            "chat": {"id": -100, "type": "group"},
            "text": "Other reminder",
            "reply_markup": {"inline_keyboard": [[{
                "text": "Cancel",
                "callback_data": format!("cancel:{}", other.id)
            }]]}
        }
    }))
    .unwrap();
    callback_dispatch(
        Bot::new("123:test").set_api_url(url),
        query,
        Arc::new(store.clone()),
    )
    .await
    .unwrap();
    let requests = captured.lock().unwrap().clone();
    server.abort();
    assert!(requests
        .iter()
        .any(|(path, _)| path.ends_with("/AnswerCallbackQuery")));
    assert!(
        !requests
            .iter()
            .any(|(path, _)| path.contains("/EditMessage")),
        "an altered callback must not edit the unrelated source message: {requests:?}"
    );
    assert_eq!(
        store.get_alert(own.id).await.unwrap().unwrap().state,
        botcore::AlertState::Cancelled
    );
    assert_eq!(
        store.get_alert(other.id).await.unwrap().unwrap().state,
        botcore::AlertState::Pending
    );
    store.pool().close().await;
    requests
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
async fn cancel_preserves_another_users_message(pool: PgPool) {
    assert_unrelated_message_is_preserved(pool, "cancel").await;
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
async fn cancel_answers_in_the_requesters_language(pool: PgPool) {
    let requests = assert_unrelated_message_is_preserved(pool, "cancel").await;
    let (_, answer) = requests
        .iter()
        .find(|(path, _)| path.ends_with("/AnswerCallbackQuery"))
        .unwrap();
    assert!(
        answer.contains(m::callback_cancelled(Language::De)),
        "{answer}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
async fn stop_series_preserves_another_users_buttons(pool: PgPool) {
    assert_unrelated_message_is_preserved(pool, "stop_series").await;
}
