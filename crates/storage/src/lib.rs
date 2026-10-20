//! Postgres-backed storage layer.
//!
//! v1 uses runtime-validated `sqlx::query` rather than the compile-time-checked
//! `query!` macros, so the workspace builds without a live database. Once the
//! schema has stabilised, swap call sites for the macros and add a `.sqlx/`
//! offline cache to keep CI fast. Validation today happens through the
//! isolated PostgreSQL tests in this module (`cargo test -- --ignored`).

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgPool, Row};
use thiserror::Error;

use botcore::{Alert, AlertScope, AlertState, ChatType, Language, NewAlert, Schedule, User};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("invalid timezone in db: {0}")]
    InvalidTimezone(String),
    #[error("invalid language in db: {0}")]
    InvalidLanguage(String),
    #[error("invalid enum value: {0}")]
    InvalidEnum(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            // Ping every connection before handing it out. Cheap (single
            // round-trip), avoids the "first request after idle silently
            // fails because Postgres / NAT killed the conn" class of bugs.
            .test_before_acquire(true)
            // Recycle idle connections after 10 min so they don't outlive
            // postgres's own idle-in-transaction or NAT keepalive limits.
            .idle_timeout(std::time::Duration::from_secs(600))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

impl PgStore {
    /// Insert-or-update by `telegram_id`. Existing rows keep their timezone
    /// and language so user-set preferences aren't clobbered by every message.
    pub async fn upsert_user(&self, telegram_id: i64, default_language: Language) -> Result<User> {
        let row = sqlx::query(
            r#"
            INSERT INTO users (telegram_id, language)
            VALUES ($1, $2)
            ON CONFLICT (telegram_id) DO UPDATE SET updated_at = now()
            RETURNING telegram_id, timezone, language, created_at, updated_at
            "#,
        )
        .bind(telegram_id)
        .bind(default_language.as_str())
        .fetch_one(&self.pool)
        .await?;

        row_to_user(row)
    }

    pub async fn get_user(&self, telegram_id: i64) -> Result<Option<User>> {
        let row = sqlx::query(
            r#"
            SELECT telegram_id, timezone, language, created_at, updated_at
            FROM users WHERE telegram_id = $1
            "#,
        )
        .bind(telegram_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(row_to_user).transpose()
    }

    pub async fn set_timezone(&self, telegram_id: i64, tz: Tz) -> Result<()> {
        sqlx::query("UPDATE users SET timezone = $1, updated_at = now() WHERE telegram_id = $2")
            .bind(tz.name())
            .bind(telegram_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_language(&self, telegram_id: i64, language: Language) -> Result<()> {
        sqlx::query("UPDATE users SET language = $1, updated_at = now() WHERE telegram_id = $2")
            .bind(language.as_str())
            .bind(telegram_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

impl PgStore {
    pub async fn create_alert(&self, new: NewAlert) -> Result<Alert> {
        let row = sqlx::query(
            r#"
            INSERT INTO alerts (
                user_id, chat_id, chat_type, scope, text, fire_at,
                dtstart, rrule, tz
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, user_id, chat_id, chat_type, scope, text, fire_at,
                      dtstart, rrule, tz,
                      state, attempts, last_error, claimed_at, claim_generation, fired_at, created_at, updated_at
            "#,
        )
        .bind(new.user_id)
        .bind(new.chat_id)
        .bind(new.chat_type.as_str())
        .bind(new.scope.as_str())
        .bind(&new.text)
        .bind(new.fire_at)
        .bind(new.schedule.dtstart)
        .bind(new.schedule.rrule.as_deref())
        .bind(new.schedule.tz.name())
        .fetch_one(&self.pool)
        .await?;

        row_to_alert(row)
    }

    pub async fn get_alert(&self, id: i64) -> Result<Option<Alert>> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, chat_id, chat_type, scope, text, fire_at,
                   dtstart, rrule, tz,
                   state, attempts, last_error, claimed_at, claim_generation, fired_at, created_at, updated_at
            FROM alerts WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_alert).transpose()
    }

    /// Active alerts visible in `chat_id` — used by `/list`.
    /// In a DM `chat_id == user_id`, in a group chat all alerts pinned to the
    /// group are returned regardless of who created them.
    pub async fn list_active_for_chat(&self, chat_id: i64) -> Result<Vec<Alert>> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_id, chat_id, chat_type, scope, text, fire_at,
                   dtstart, rrule, tz,
                   state, attempts, last_error, claimed_at, claim_generation, fired_at, created_at, updated_at
            FROM alerts
            WHERE chat_id = $1 AND state IN ('pending', 'claimed')
            ORDER BY fire_at ASC
            "#,
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_alert).collect()
    }

    /// Mark `cancelled`, but only if `requester_user_id` is allowed to.
    /// Returns `true` if a row was actually transitioned, `false` if the alert
    /// doesn't exist, isn't pending, or the requester lacks permission.
    pub async fn cancel_alert(&self, id: i64, requester_user_id: i64) -> Result<bool> {
        // Fetch first to enforce the can_edit predicate in Rust — keeps the
        // permission rule colocated with the domain type and out of SQL.
        let Some(alert) = self.get_alert(id).await? else {
            return Ok(false);
        };
        if !alert.can_edit(requester_user_id) {
            return Ok(false);
        }
        if !matches!(alert.state, AlertState::Pending) {
            return Ok(false);
        }
        let res = sqlx::query(
            "UPDATE alerts SET state = 'cancelled', updated_at = now()
             WHERE id = $1 AND state = 'pending'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Atomically claim up to `limit` due alerts. Multiple workers can call
    /// this concurrently — `FOR UPDATE SKIP LOCKED` guarantees no row is
    /// claimed twice. `attempts` saturates at the `SMALLINT` maximum, so an
    /// alert that keeps being rescheduled cannot make the claim query fail.
    pub async fn claim_due_alerts(&self, limit: i64) -> Result<Vec<Alert>> {
        let rows = sqlx::query(
            r#"
            UPDATE alerts
            SET state = 'claimed',
                attempts = LEAST(attempts::integer + 1, 32767),
                claim_generation = claim_generation + 1,
                claimed_at = now(),
                updated_at = now()
            WHERE id IN (
                SELECT id FROM alerts
                WHERE state = 'pending' AND fire_at <= now()
                ORDER BY fire_at ASC
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            RETURNING id, user_id, chat_id, chat_type, scope, text, fire_at,
                      dtstart, rrule, tz,
                      state, attempts, last_error, claimed_at, claim_generation, fired_at, created_at, updated_at
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_alert).collect()
    }

    /// Only the current claim generation may finish or retry a delivery.
    /// State alone is insufficient: the reaper can release a claim and a new
    /// worker can claim the same row before the original worker completes.
    pub async fn mark_sent(&self, id: i64, generation: i64) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE alerts SET state = 'sent', fired_at = now(), updated_at = now()
             WHERE id = $1 AND state = 'claimed' AND claim_generation = $2",
        )
        .bind(id)
        .bind(generation)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn mark_failed(&self, id: i64, generation: i64, error: &str) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE alerts SET state = 'failed', last_error = $3, updated_at = now()
             WHERE id = $1 AND state = 'claimed' AND claim_generation = $2",
        )
        .bind(id)
        .bind(generation)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Re-queue: bumps `state` back to `pending` with a new `fire_at`.
    /// Used by retry logic when Telegram returns 429 (rate-limited).
    /// Deliberately keeps `attempts` — it's the same occurrence being retried.
    pub async fn reschedule(
        &self,
        id: i64,
        generation: i64,
        fire_at: DateTime<Utc>,
    ) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE alerts SET state = 'pending', fire_at = $3, claimed_at = NULL, updated_at = now()
             WHERE id = $1 AND state = 'claimed' AND claim_generation = $2",
        )
        .bind(id)
        .bind(generation)
        .bind(fire_at)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Successful delivery of a recurring alert: queue the next occurrence and
    /// reset the retry counter. `attempts` counts retries *per occurrence*, not
    /// over the alert's lifetime — without the reset a long-lived series would
    /// hit MAX_ATTEMPTS after enough successful deliveries and die on the first
    /// transient error (and eventually overflow the SMALLINT column).
    pub async fn advance_to_next_occurrence(
        &self,
        id: i64,
        generation: i64,
        fire_at: DateTime<Utc>,
    ) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE alerts SET state = 'pending', fire_at = $3, claimed_at = NULL,
                               attempts = 0, updated_at = now()
             WHERE id = $1 AND state = 'claimed' AND claim_generation = $2",
        )
        .bind(id)
        .bind(generation)
        .bind(fire_at)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Reaper: rows stuck in `claimed` longer than the timeout were owned by
    /// a worker that crashed mid-send. Reset them so another worker picks up.
    pub async fn release_stale_claims(&self, timeout_secs: i64) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE alerts
            SET state = 'pending', claimed_at = NULL, updated_at = now()
            WHERE state = 'claimed'
              AND claimed_at < now() - make_interval(secs => $1)
            "#,
        )
        .bind(timeout_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Count of active (pending or claimed) alerts for one user. Used by the
    /// quota check in `/alert` and `/galert`. Hits the partial index
    /// `alerts_user_active_idx` so this stays sub-ms even at 10k+ users.
    pub async fn count_active_for_user(&self, user_id: i64) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            r#"
            SELECT count(*) FROM alerts
            WHERE user_id = $1 AND state IN ('pending', 'claimed')
            "#,
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Count of active alerts pinned to one chat (DM, group, or supergroup).
    /// Uses the existing partial index `alerts_chat_idx`.
    pub async fn count_active_for_chat(&self, chat_id: i64) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            r#"
            SELECT count(*) FROM alerts
            WHERE chat_id = $1 AND state IN ('pending', 'claimed')
            "#,
        )
        .bind(chat_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Telegram replaces a group's chat id when the group becomes a
    /// supergroup. Move every alert of the old chat to the new id.
    pub async fn migrate_chat(&self, from_chat_id: i64, to_chat_id: i64) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE alerts SET chat_id = $2, chat_type = 'supergroup', updated_at = now()
             WHERE chat_id = $1",
        )
        .bind(from_chat_id)
        .bind(to_chat_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Earliest pending fire time, used by the worker to compute its next
    /// wake-up timer. `None` means no pending alerts at all.
    pub async fn next_pending_fire_at(&self) -> Result<Option<DateTime<Utc>>> {
        let row: Option<(Option<DateTime<Utc>>,)> =
            sqlx::query_as("SELECT MIN(fire_at) FROM alerts WHERE state = 'pending'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.0))
    }
}

// ---------------------------------------------------------------------------
// Idempotency
// ---------------------------------------------------------------------------

impl PgStore {
    /// Returns `true` if `update_id` was new and is now recorded; `false` if
    /// it was a duplicate (= we already processed this Telegram update).
    pub async fn try_register_update(&self, update_id: i64) -> Result<bool> {
        let res = sqlx::query(
            "INSERT INTO processed_updates (update_id) VALUES ($1) ON CONFLICT DO NOTHING",
        )
        .bind(update_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    pub async fn purge_old_updates(&self, older_than_secs: i64) -> Result<u64> {
        let res = sqlx::query(
            "DELETE FROM processed_updates WHERE received_at < now() - make_interval(secs => $1)",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

// ---------------------------------------------------------------------------
// Stats (read-only snapshot for external dashboards, e.g. Homepage)
// ---------------------------------------------------------------------------

/// Aggregate counts for the stats endpoint. All `count(*)` values → `i64`.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub users: i64,
    /// pending + claimed — everything not yet delivered, cancelled, or failed.
    pub active: i64,
    pub sent: i64,
    pub total: i64,
}

impl PgStore {
    /// One-shot aggregate snapshot for dashboards. Cheap: four counts in a
    /// single round-trip.
    pub async fn stats(&self) -> Result<Stats> {
        let row: (i64, i64, i64, i64) = sqlx::query_as(
            r#"
            SELECT
                (SELECT count(*) FROM users),
                (SELECT count(*) FROM alerts WHERE state IN ('pending', 'claimed')),
                (SELECT count(*) FROM alerts WHERE state = 'sent'),
                (SELECT count(*) FROM alerts)
            "#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(Stats {
            users: row.0,
            active: row.1,
            sent: row.2,
            total: row.3,
        })
    }
}

// ---------------------------------------------------------------------------
// Row → domain mapping
// ---------------------------------------------------------------------------

fn row_to_user(row: PgRow) -> Result<User> {
    let tz_name: String = row.try_get("timezone")?;
    let lang_str: String = row.try_get("language")?;
    let timezone: Tz = tz_name
        .parse()
        .map_err(|_| StorageError::InvalidTimezone(tz_name.clone()))?;
    let language =
        Language::parse(&lang_str).ok_or_else(|| StorageError::InvalidLanguage(lang_str))?;
    Ok(User {
        telegram_id: row.try_get("telegram_id")?,
        timezone,
        language,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn row_to_alert(row: PgRow) -> Result<Alert> {
    let chat_type_str: String = row.try_get("chat_type")?;
    let scope_str: String = row.try_get("scope")?;
    let state_str: String = row.try_get("state")?;

    let chat_type =
        ChatType::parse(&chat_type_str).ok_or_else(|| StorageError::InvalidEnum(chat_type_str))?;
    let scope = AlertScope::parse(&scope_str).ok_or_else(|| StorageError::InvalidEnum(scope_str))?;
    let state = AlertState::parse(&state_str).ok_or_else(|| StorageError::InvalidEnum(state_str))?;

    let dtstart: DateTime<Utc> = row.try_get("dtstart")?;
    let tz_name: String = row.try_get("tz")?;
    let tz: Tz = tz_name
        .parse()
        .map_err(|_| StorageError::InvalidTimezone(tz_name.clone()))?;
    let rrule_str: Option<String> = row.try_get("rrule")?;

    // A populated `rrule` means recurring; absent (or empty) means one-shot.
    let schedule = Schedule::from_db(dtstart, tz, rrule_str.as_deref())
        .map_err(|e| StorageError::InvalidEnum(e.to_string()))?;

    Ok(Alert {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        chat_id: row.try_get("chat_id")?,
        chat_type,
        scope,
        text: row.try_get("text")?,
        fire_at: row.try_get("fire_at")?,
        schedule,
        state,
        attempts: row.try_get("attempts")?,
        last_error: row.try_get("last_error")?,
        claimed_at: row.try_get("claimed_at")?,
        claim_generation: row.try_get("claim_generation")?,
        fired_at: row.try_get("fired_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn due_alert(store: &PgStore, text: &str) -> Alert {
        store.upsert_user(42, Language::De).await.unwrap();
        let at = Utc::now() - chrono::Duration::minutes(1);
        store
            .create_alert(NewAlert {
                user_id: 42,
                chat_id: 42,
                chat_type: ChatType::Private,
                scope: AlertScope::Private,
                text: text.into(),
                fire_at: at,
                schedule: Schedule::one_shot(at, chrono_tz::UTC),
            })
            .await
            .unwrap()
    }

    async fn expire_claim(store: &PgStore, id: i64) {
        sqlx::query("UPDATE alerts SET claimed_at = now() - interval '61 seconds' WHERE id = $1")
            .bind(id)
            .execute(store.pool())
            .await
            .unwrap();
        assert_eq!(store.release_stale_claims(60).await.unwrap(), 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn stale_generation_cannot_mutate_a_reassigned_claim(pool: PgPool) {
        let store = PgStore { pool };
        let original = due_alert(&store, "first").await;
        let waiting = due_alert(&store, "second").await;
        let old = store.claim_due_alerts(1).await.unwrap().remove(0);
        assert_eq!(old.id, original.id);
        assert_eq!(
            store.get_alert(waiting.id).await.unwrap().unwrap().state,
            AlertState::Pending
        );
        assert_eq!(store.list_active_for_chat(42).await.unwrap().len(), 2);
        expire_claim(&store, old.id).await;
        let current = store.claim_due_alerts(1).await.unwrap().remove(0);
        assert_eq!(current.id, old.id);
        assert!(current.claim_generation > old.claim_generation);
        let next = Utc::now() + chrono::Duration::hours(1);
        assert!(!store.mark_sent(old.id, old.claim_generation).await.unwrap());
        assert!(!store
            .mark_failed(old.id, old.claim_generation, "late failure")
            .await
            .unwrap());
        assert!(!store
            .reschedule(old.id, old.claim_generation, next)
            .await
            .unwrap());
        assert!(!store
            .advance_to_next_occurrence(old.id, old.claim_generation, next)
            .await
            .unwrap());
        let unchanged = store.get_alert(old.id).await.unwrap().unwrap();
        assert_eq!(unchanged.state, AlertState::Claimed);
        assert_eq!(unchanged.fire_at, current.fire_at);
        assert_eq!(unchanged.attempts, current.attempts);
        assert_eq!(unchanged.last_error, None);
        assert!(store
            .advance_to_next_occurrence(current.id, current.claim_generation, next)
            .await
            .unwrap());
        let advanced = store.get_alert(old.id).await.unwrap().unwrap();
        assert_eq!(advanced.state, AlertState::Pending);
        assert_eq!(advanced.attempts, 0);
        assert_eq!(advanced.claim_generation, current.claim_generation);
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn late_worker_cannot_resurrect_a_cancelled_alert(pool: PgPool) {
        let store = PgStore { pool };
        due_alert(&store, "cancel me").await;
        let old = store.claim_due_alerts(1).await.unwrap().remove(0);
        expire_claim(&store, old.id).await;
        assert!(store.cancel_alert(old.id, 42).await.unwrap());
        assert!(!store.mark_sent(old.id, old.claim_generation).await.unwrap());
        assert!(!store
            .mark_failed(old.id, old.claim_generation, "late")
            .await
            .unwrap());
        assert!(!store
            .reschedule(old.id, old.claim_generation, Utc::now())
            .await
            .unwrap());
        assert!(!store
            .advance_to_next_occurrence(old.id, old.claim_generation, Utc::now())
            .await
            .unwrap());
        assert_eq!(
            store.get_alert(old.id).await.unwrap().unwrap().state,
            AlertState::Cancelled
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn concurrent_claims_and_retry_transitions_keep_ownership(pool: PgPool) {
        let store = PgStore { pool };
        due_alert(&store, "one").await;
        due_alert(&store, "two").await;
        let (a, b) = tokio::join!(store.claim_due_alerts(1), store.claim_due_alerts(1));
        let (a, b) = (a.unwrap().remove(0), b.unwrap().remove(0));
        assert_ne!(a.id, b.id);
        assert!(store.mark_sent(a.id, a.claim_generation).await.unwrap());
        assert!(!store.mark_sent(a.id, a.claim_generation).await.unwrap());
        assert!(store
            .reschedule(
                b.id,
                b.claim_generation,
                Utc::now() - chrono::Duration::seconds(1)
            )
            .await
            .unwrap());
        let retry = store.claim_due_alerts(1).await.unwrap().remove(0);
        assert_eq!(retry.id, b.id);
        assert!(retry.claim_generation > b.claim_generation);
        assert_eq!(retry.attempts, b.attempts + 1);
        assert!(store
            .mark_failed(retry.id, retry.claim_generation, "failed")
            .await
            .unwrap());
        assert_eq!(
            store
                .get_alert(retry.id)
                .await
                .unwrap()
                .unwrap()
                .last_error
                .as_deref(),
            Some("failed")
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn claim_saturates_attempts_without_blocking_other_alerts(pool: PgPool) {
        let store = PgStore { pool };
        let exhausted = due_alert(&store, "saturated").await;
        let healthy = due_alert(&store, "still deliver").await;
        sqlx::query("UPDATE alerts SET attempts = 32767 WHERE id = $1")
            .bind(exhausted.id)
            .execute(store.pool())
            .await
            .unwrap();

        let claimed = store.claim_due_alerts(2).await.unwrap();
        assert_eq!(claimed.len(), 2);
        let saturated = claimed.iter().find(|a| a.id == exhausted.id).unwrap();
        assert_eq!(saturated.attempts, i16::MAX);
        assert!(claimed.iter().any(|a| a.id == healthy.id));
    }

    #[sqlx::test(migrations = "../../migrations")]
    #[ignore = "requires a dedicated PostgreSQL DATABASE_URL"]
    async fn chat_migration_moves_only_alerts_of_the_old_chat(pool: PgPool) {
        let store = PgStore { pool };
        store.upsert_user(42, Language::De).await.unwrap();
        let at = Utc::now() + chrono::Duration::hours(1);
        let make = |chat_id| NewAlert {
            user_id: 42,
            chat_id,
            chat_type: ChatType::Group,
            scope: AlertScope::Shared,
            text: "standup".into(),
            fire_at: at,
            schedule: Schedule::one_shot(at, chrono_tz::UTC),
        };
        let first = store.create_alert(make(-100)).await.unwrap();
        let second = store.create_alert(make(-100)).await.unwrap();
        let other = store.create_alert(make(-200)).await.unwrap();

        assert_eq!(store.migrate_chat(-100, -1_001_234).await.unwrap(), 2);

        for id in [first.id, second.id] {
            let moved = store.get_alert(id).await.unwrap().unwrap();
            assert_eq!(moved.chat_id, -1_001_234);
            assert_eq!(moved.chat_type, ChatType::Supergroup);
        }
        let untouched = store.get_alert(other.id).await.unwrap().unwrap();
        assert_eq!(untouched.chat_id, -200);
        assert_eq!(untouched.chat_type, ChatType::Group);
        assert_eq!(store.count_active_for_chat(-1_001_234).await.unwrap(), 2);
    }
}
