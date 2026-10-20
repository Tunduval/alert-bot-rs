//! Delivery worker: claims due alerts and pushes them to Telegram.
//!
//! The loop:
//!  1. Compute next wake-up = `MIN(fire_at) WHERE state = pending`, or far
//!     future if there are no pending alerts.
//!  2. Wait for whichever happens first: that timestamp, a `NOTIFY` from the
//!     DB (new alert / cancellation), or shutdown.
//!  3. Atomically claim one due alert via `FOR UPDATE SKIP LOCKED`.
//!  4. For each: send via Bot API, mark sent. On 429 reschedule, on 4xx other
//!     than 429 mark failed, on transient error retry up to MAX_ATTEMPTS.
//!     A recurring alert then skips only the failed occurrence.
//!     If the group became a supergroup, move its alerts and retry.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgListener, PgNotification};
use teloxide::prelude::*;
use teloxide::ApiError;
use tokio_util::sync::CancellationToken;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use botcore::{Alert, Language};
use storage::PgStore;

use crate::handlers;
use crate::messages as m;
use crate::render;

const MAX_ATTEMPTS: i16 = 5;
const STALE_CLAIM_SECS: i64 = 60;
const REAPER_INTERVAL: Duration = Duration::from_secs(30);
// Do not reserve a queue whose entries could expire before we send them.
const CLAIM_BATCH: i64 = 1;
/// A delivery is "delayed" if it fires more than this far behind its
/// scheduled time. Below this we treat it as normal latency.
const DELAYED_THRESHOLD_SECS: i64 = 60;

pub async fn run(bot: Bot, store: Arc<PgStore>, shutdown: CancellationToken) -> anyhow::Result<()> {
    let mut listener = PgListener::connect_with(store.pool()).await?;
    listener.listen("alerts_changed").await?;
    info!("worker listening on `alerts_changed`");

    let reaper_store = store.clone();
    let reaper_shutdown = shutdown.clone();
    tokio::spawn(async move {
        reaper_loop(reaper_store, reaper_shutdown).await;
    });

    loop {
        let next_wake = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(()),
            next = compute_next_wake(&store) => next,
        };

        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                info!("worker shutting down");
                return Ok(());
            }
            notification = listener.recv() => {
                require_notification(notification)?;
                debug!("worker woke on NOTIFY");
            }
            _ = sleep_until(next_wake) => {
                debug!("worker woke on timer");
            }
        }

        if let Err(e) = process_due(&bot, &store).await {
            error!(error = ?e, "process_due failed");
        }
    }
}

async fn process_due(bot: &Bot, store: &PgStore) -> anyhow::Result<()> {
    let claimed = store.claim_due_alerts(CLAIM_BATCH).await?;
    for alert in claimed {
        deliver(bot, store, alert).await;
    }
    Ok(())
}

async fn deliver(bot: &Bot, store: &PgStore, alert: Alert) {
    // Look up creator language for delay-prefix localisation. Failure to
    // find the user is non-fatal — fall back to German (the bot's default).
    let lang = match store.get_user(alert.user_id).await {
        Ok(Some(u)) => u.language,
        // User row gone (e.g. /start never completed) — non-fatal, use the default.
        Ok(None) => Language::De,
        // A DB error here shouldn't block delivery, but it's not the same as a
        // missing user — surface it so a flapping connection doesn't hide behind
        // the German fallback.
        Err(e) => {
            warn!(user_id = alert.user_id, error = %e, "user lookup failed during delivery; defaulting language to German");
            Language::De
        }
    };

    let now = Utc::now();
    let delay_secs = (now - alert.fire_at).num_seconds();
    let body = if delay_secs > DELAYED_THRESHOLD_SECS {
        let human = render::format_duration_human(delay_secs);
        format!("{}{}", m::delayed_prefix(lang, &human), alert.text)
    } else {
        alert.text.clone()
    };

    let keyboard = handlers::delivery_keyboard(lang, alert.id, alert.schedule.is_recurring());
    let send = bot
        .send_message(ChatId(alert.chat_id), &body)
        .reply_markup(keyboard)
        .send()
        .await;

    match send {
        Ok(message) => {
            match store
                .record_alert_delivery(
                    alert.id,
                    alert.claim_generation,
                    alert.chat_id,
                    i64::from(message.id.0),
                )
                .await
            {
                Ok(true) => {}
                Ok(false) => warn!(
                    id = alert.id,
                    generation = alert.claim_generation,
                    "delivery has no snooze receipt; callback will fail closed"
                ),
                Err(e) => error!(
                    id = alert.id,
                    generation = alert.claim_generation,
                    error = ?e,
                    "record_alert_delivery failed; callback will fail closed"
                ),
            }
            if let Err(e) = finalise_after_send(store, &alert).await {
                error!(id = alert.id, error = ?e, "finalise_after_send failed");
            }
        }
        Err(teloxide::RequestError::RetryAfter(seconds)) => {
            // Telegram global/group rate limit. Reschedule and retry.
            let new_fire = Utc::now() + chrono::Duration::seconds(seconds.seconds() as i64 + 1);
            warn!(
                id = alert.id,
                retry_in = seconds.seconds(),
                "telegram 429, rescheduling"
            );
            if let Err(e) = store
                .reschedule(alert.id, alert.claim_generation, new_fire)
                .await
            {
                error!(id = alert.id, error = ?e, "reschedule failed");
            }
        }
        Err(teloxide::RequestError::MigrateToChatId(new_chat_id)) => {
            info!(
                id = alert.id,
                from = alert.chat_id,
                to = new_chat_id.0,
                "group migrated to a supergroup"
            );
            if let Err(e) = store.migrate_chat(alert.chat_id, new_chat_id.0).await {
                error!(id = alert.id, error = ?e, "chat migration failed");
            }
            if let Err(e) = store
                .reschedule(alert.id, alert.claim_generation, Utc::now())
                .await
            {
                error!(id = alert.id, error = ?e, "reschedule failed");
            }
        }
        Err(teloxide::RequestError::Api(ApiError::BotBlocked))
        | Err(teloxide::RequestError::Api(ApiError::ChatNotFound))
        | Err(teloxide::RequestError::Api(ApiError::UserDeactivated)) => {
            // Permanent: nobody to deliver to.
            warn!(id = alert.id, "permanent delivery failure");
            if let Err(e) = store
                .mark_failed(
                    alert.id,
                    alert.claim_generation,
                    "permanent: delivery target unreachable",
                )
                .await
            {
                error!(id = alert.id, error = ?e, "mark_failed failed");
            }
        }
        Err(e) => {
            // Transient — retry with exponential backoff up to MAX_ATTEMPTS.
            if alert.attempts >= MAX_ATTEMPTS {
                let msg = format!("max attempts: {e}");
                warn!(id = alert.id, attempts = alert.attempts, "giving up");
                if let Err(e) = give_up_occurrence(store, &alert, &msg).await {
                    error!(id = alert.id, error = ?e, "giving up failed");
                }
            } else {
                let backoff = 5_i64 << alert.attempts.min(6);
                let new_fire = Utc::now() + chrono::Duration::seconds(backoff);
                warn!(
                    id = alert.id,
                    attempt = alert.attempts,
                    backoff,
                    "transient error, retrying"
                );
                if let Err(e) = store
                    .reschedule(alert.id, alert.claim_generation, new_fire)
                    .await
                {
                    error!(id = alert.id, error = ?e, "reschedule failed");
                }
            }
        }
    }
}

/// On successful delivery: if recurring, compute the next fire time relative
/// to *now* (skipping any missed occurrences during downtime so we don't fire
/// the past N intervals back-to-back) and reschedule. Otherwise mark sent.
async fn finalise_after_send(store: &PgStore, alert: &Alert) -> anyhow::Result<()> {
    let applied = if !alert.schedule.is_recurring() {
        store.mark_sent(alert.id, alert.claim_generation).await?
    } else {
        let now = Utc::now();
        match alert.schedule.next_after(now) {
            Some(next) => {
                store
                    .advance_to_next_occurrence(alert.id, alert.claim_generation, next)
                    .await?
            }
            None => store.mark_sent(alert.id, alert.claim_generation).await?,
        }
    };
    if !applied {
        // The claim guard rejected the write: the reaper released our claim
        // while the send was in flight and the row was re-claimed or cancelled
        // meanwhile. Don't resurrect it — just note the wasted double-send.
        warn!(
            id = alert.id,
            "delivered but claim was no longer ours; not finalising"
        );
    }
    Ok(())
}

/// A transient outage must not end a recurring series. Give up on this
/// occurrence only and queue the next one; one-shots are marked failed.
async fn give_up_occurrence(store: &PgStore, alert: &Alert, error: &str) -> anyhow::Result<()> {
    let next = if alert.schedule.is_recurring() {
        alert.schedule.next_after(Utc::now())
    } else {
        None
    };
    match next {
        Some(next) => {
            store
                .advance_to_next_occurrence(alert.id, alert.claim_generation, next)
                .await?;
        }
        None => {
            store
                .mark_failed(alert.id, alert.claim_generation, error)
                .await?;
        }
    }
    Ok(())
}

async fn compute_next_wake(store: &PgStore) -> DateTime<Utc> {
    match store.next_pending_fire_at().await {
        Ok(Some(t)) => t,
        Ok(None) => Utc::now() + chrono::Duration::hours(1),
        Err(e) => {
            error!(error = ?e, "next_pending_fire_at failed");
            Utc::now() + chrono::Duration::seconds(30)
        }
    }
}

async fn sleep_until(target: DateTime<Utc>) {
    let now = Utc::now();
    if target <= now {
        return;
    }
    let delta = (target - now).to_std().unwrap_or(Duration::from_secs(0));
    sleep(delta).await;
}

async fn reaper_loop(store: Arc<PgStore>, shutdown: CancellationToken) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = sleep(REAPER_INTERVAL) => {}
        }

        match store.release_stale_claims(STALE_CLAIM_SECS).await {
            Ok(n) if n > 0 => warn!(reaped = n, "released stale claims"),
            Ok(_) => {}
            Err(e) => error!(error = ?e, "reaper failed"),
        }
        match store.purge_old_updates(86_400).await {
            Ok(_) => {}
            Err(e) => error!(error = ?e, "purge_old_updates failed"),
        }
    }
}

fn require_notification(notification: Result<PgNotification, sqlx::Error>) -> anyhow::Result<()> {
    notification
        .map(|_| ())
        .context("receiving alerts_changed notification")
}

#[cfg(test)]
mod tests {
    use super::*;
    use botcore::{AlertScope, AlertState, ChatType, NewAlert, Schedule};
    use sqlx::{ConnectOptions, PgPool};
    use tokio::sync::Notify;

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn shutdown_during_delivery_finishes_the_send_and_exits(pool: PgPool) {
        let store = Arc::new(
            PgStore::connect(pool.connect_options().to_url_lossy().as_str(), 5)
                .await
                .unwrap(),
        );
        store.upsert_user(42, Language::En).await.unwrap();
        let at = Utc::now() - chrono::Duration::seconds(1);
        let alert = store
            .create_alert(NewAlert {
                user_id: 42,
                chat_id: 42,
                chat_type: ChatType::Private,
                scope: AlertScope::Private,
                text: "test".into(),
                fire_at: at,
                schedule: Schedule::one_shot(at, chrono_tz::UTC),
            })
            .await
            .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let app = axum::Router::new().fallback({
            let entered = entered.clone();
            let release = release.clone();
            move || {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    r#"{"ok":true,"result":{"message_id":1,"date":0,"chat":{"id":42,"type":"private","first_name":"Test"},"text":"test"}}"#
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let shutdown = CancellationToken::new();
        let worker = tokio::spawn(run(
            Bot::new("123:test").set_api_url(url),
            store.clone(),
            shutdown.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        // Cancel while the worker is busy, with no cancellation waiter installed.
        shutdown.cancel();
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_alert(alert.id).await.unwrap().unwrap().state,
            AlertState::Sent
        );
        server.abort();
        store.pool().close().await;
    }

    #[test]
    fn listener_errors_stop_the_worker_instead_of_spinning() {
        let error = require_notification(Err(sqlx::Error::PoolClosed)).unwrap_err();
        assert!(error
            .to_string()
            .contains("receiving alerts_changed notification"));
    }

    async fn deliver_after_last_transient_failure(
        pool: PgPool,
        schedule: impl Fn(DateTime<Utc>) -> Schedule,
    ) -> Alert {
        let store = PgStore::connect(pool.connect_options().to_url_lossy().as_str(), 5)
            .await
            .unwrap();
        store.upsert_user(42, Language::En).await.unwrap();
        let at = Utc::now() - chrono::Duration::seconds(1);
        let alert = store
            .create_alert(NewAlert {
                user_id: 42,
                chat_id: 42,
                chat_type: ChatType::Private,
                scope: AlertScope::Private,
                text: "test".into(),
                fire_at: at,
                schedule: schedule(at),
            })
            .await
            .unwrap();
        sqlx::query("UPDATE alerts SET attempts = $2 WHERE id = $1")
            .bind(alert.id)
            .bind(MAX_ATTEMPTS - 1)
            .execute(store.pool())
            .await
            .unwrap();
        let app = axum::Router::new().fallback(|| async {
            r#"{"ok":false,"error_code":500,"description":"Internal Server Error"}"#
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        process_due(&Bot::new("123:test").set_api_url(url), &store)
            .await
            .unwrap();
        server.abort();
        let after = store.get_alert(alert.id).await.unwrap().unwrap();
        store.pool().close().await;
        after
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn transient_failures_skip_only_the_current_recurring_occurrence(pool: PgPool) {
        let after = deliver_after_last_transient_failure(pool, |at| {
            Schedule::daily_at(at, chrono_tz::UTC, 1, at.time()).unwrap()
        })
        .await;
        assert_eq!(after.state, AlertState::Pending);
        assert_eq!(after.attempts, 0);
        assert!(after.fire_at > Utc::now());
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn transient_failures_still_fail_one_shot_alerts(pool: PgPool) {
        let after =
            deliver_after_last_transient_failure(pool, |at| Schedule::one_shot(at, chrono_tz::UTC))
                .await;
        assert_eq!(after.state, AlertState::Failed);
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn supergroup_migration_moves_the_alert_and_retries(pool: PgPool) {
        let store = PgStore::connect(pool.connect_options().to_url_lossy().as_str(), 5)
            .await
            .unwrap();
        store.upsert_user(42, Language::En).await.unwrap();
        let at = Utc::now() - chrono::Duration::seconds(1);
        let alert = store
            .create_alert(NewAlert {
                user_id: 42,
                chat_id: -100,
                chat_type: ChatType::Group,
                scope: AlertScope::Shared,
                text: "standup".into(),
                fire_at: at,
                schedule: Schedule::one_shot(at, chrono_tz::UTC),
            })
            .await
            .unwrap();
        let app = axum::Router::new().fallback(|| async {
            r#"{"ok":false,"error_code":400,"description":"Bad Request: group chat was upgraded to a supergroup chat","parameters":{"migrate_to_chat_id":-1001234}}"#
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        process_due(&Bot::new("123:test").set_api_url(url), &store)
            .await
            .unwrap();
        server.abort();
        let after = store.get_alert(alert.id).await.unwrap().unwrap();
        assert_eq!(after.chat_id, -1_001_234);
        assert_eq!(after.chat_type, ChatType::Supergroup);
        assert_eq!(after.state, AlertState::Pending);
        assert!(after.fire_at <= Utc::now());
        store.pool().close().await;
    }
}
