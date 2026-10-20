//! Anti-spam limits: rate limiter + per-user/per-chat alert quotas + text length cap.
//!
//! These are *soft* guards meant to keep accidental floods and casual abuse
//! at bay. They're not a substitute for proper Telegram-side moderation (e.g.
//! removing the bot from a hostile group); they just keep the DB bounded and
//! the worker responsive when a single client sends a burst.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Maximum reminder-text length in characters. Telegram's own message limit
/// is 4096, but real reminders almost never need more than ~80; anything
/// past 500 is overwhelmingly junk or a DB-bloat probe.
pub const MAX_TEXT_LEN: usize = 500;

/// Maximum active (pending or claimed) alerts per Telegram user. Power users
/// can hit this with legitimate recurring schedules; that's fine — they can
/// `/cancel` something stale before adding more.
pub const MAX_ALERTS_PER_USER: i64 = 100;

/// Maximum active alerts pinned to a single chat (DM, group, or supergroup).
/// Defends against "bot added to hostile group, someone spams /galert".
pub const MAX_ALERTS_PER_CHAT: i64 = 50;

/// Commands accepted per Telegram user per minute. Bucket starts full so a
/// user can send 6 commands instantly (e.g. setting up several reminders
/// in one sitting), then trickles at one command every 10s.
///
/// Real-world pace is 1-3/min in any normal session. 6/min has a small
/// headroom for bulk-setup bursts; a spammer hits the wall immediately.
const COMMANDS_PER_MINUTE: u32 = 6;

/// Commands accepted across all Telegram users per minute after their
/// individual check passed. Forged webhook updates can choose any user and
/// chat ID, so the per-user and per-chat quotas alone would not bound
/// persistent work after a webhook-secret compromise. The ceiling sits far
/// above real traffic, so a group of real accounts at their individual limit
/// cannot lock everyone else out.
const GLOBAL_COMMANDS_PER_MINUTE: u32 = 3_000;

/// Maximum user identities retained by the in-memory limiter. A forged
/// webhook update cannot allocate state beyond this fixed ceiling.
const MAX_TRACKED_USERS: usize = 4_096;

/// An inactive identity may be replaced after every possible token bucket for
/// it has fully recovered. This is deliberately longer than the one-minute
/// per-user quota window.
const TRACKED_USER_TTL: Duration = Duration::from_secs(120);

pub type UserId = i64;

/// Outcome of one rate-limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimit {
    Allowed,
    /// This user's own budget is exhausted.
    UserBudgetExhausted,
    /// The global budget or the identity ceiling rejected the request. Both
    /// are shared by all users, so forged updates can reach them in bulk.
    SharedCeiling,
}

impl RateLimit {
    pub fn is_allowed(self) -> bool {
        self == Self::Allowed
    }
}

/// In-memory keyed token-bucket limiter. The dispatcher applies it once to
/// commands and callbacks before any persistent DB write.
///
/// Restart resets the limiter (it's in-memory by design). That's acceptable
/// — abusers usually move on once the first attempt is throttled, and the
/// soft DB quotas catch persistent ones across restarts.
#[derive(Debug, Clone, Copy)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn full(capacity: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(capacity),
            last_refill: now,
        }
    }

    fn refilled_tokens(self, capacity: u32, now: Instant) -> f64 {
        let refill = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64()
            * f64::from(capacity)
            / 60.0;
        (self.tokens + refill).min(f64::from(capacity))
    }
}

#[derive(Debug, Clone, Copy)]
struct UserBudget {
    bucket: TokenBucket,
    last_seen: Instant,
}

struct LimiterState {
    users: HashMap<UserId, UserBudget>,
    global: TokenBucket,
}

pub struct CommandRateLimiter {
    state: Mutex<LimiterState>,
}

impl CommandRateLimiter {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            state: Mutex::new(LimiterState {
                users: HashMap::new(),
                global: TokenBucket::full(GLOBAL_COMMANDS_PER_MINUTE, now),
            }),
        }
    }

    /// Decides whether the user may issue this command now, and if not,
    /// which budget rejected it.
    pub fn check(&self, user_id: UserId) -> RateLimit {
        self.check_at(user_id, Instant::now())
    }

    fn check_at(&self, user_id: UserId, now: Instant) -> RateLimit {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if state.users.len() >= MAX_TRACKED_USERS {
            state
                .users
                .retain(|_, budget| now.duration_since(budget.last_seen) < TRACKED_USER_TTL);
        }
        if !state.users.contains_key(&user_id) && state.users.len() >= MAX_TRACKED_USERS {
            return RateLimit::SharedCeiling;
        }

        let global_tokens = state
            .global
            .refilled_tokens(GLOBAL_COMMANDS_PER_MINUTE, now);
        let user = state.users.entry(user_id).or_insert_with(|| UserBudget {
            bucket: TokenBucket::full(COMMANDS_PER_MINUTE, now),
            last_seen: now,
        });
        let user_tokens = user.bucket.refilled_tokens(COMMANDS_PER_MINUTE, now);
        let decision = if user_tokens < 1.0 {
            RateLimit::UserBudgetExhausted
        } else if global_tokens < 1.0 {
            RateLimit::SharedCeiling
        } else {
            RateLimit::Allowed
        };

        let consumed = if decision.is_allowed() { 1.0 } else { 0.0 };
        user.bucket.tokens = user_tokens - consumed;
        user.bucket.last_refill = now;
        user.last_seen = now;
        state.global.tokens = global_tokens - consumed;
        state.global.last_refill = now;

        decision
    }
}

impl Default for CommandRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_initial_burst() {
        let l = CommandRateLimiter::new();
        // Bucket starts full at COMMANDS_PER_MINUTE — that many in a row pass.
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check(42).is_allowed(), "burst within quota");
        }
    }

    #[test]
    fn rate_limiter_blocks_after_burst() {
        let l = CommandRateLimiter::new();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check(7).is_allowed());
        }
        // The next command in the same instant must fail.
        assert!(!l.check(7).is_allowed(), "post-burst request blocked");
    }

    #[test]
    fn rate_limiter_is_per_user() {
        let l = CommandRateLimiter::new();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check(1).is_allowed());
        }
        assert!(!l.check(1).is_allowed(), "user 1 exhausted");
        assert!(l.check(2).is_allowed(), "user 2 still has full budget");
    }

    #[test]
    fn one_user_cannot_exhaust_other_users_budget() {
        let l = CommandRateLimiter::new();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check(7).is_allowed());
        }
        for _ in 0..256 {
            assert!(!l.check(7).is_allowed());
        }
        assert!(
            l.check(8).is_allowed(),
            "another user must retain an independent budget"
        );
    }

    #[test]
    fn rate_limiter_has_a_hard_identity_ceiling() {
        let l = CommandRateLimiter::new();
        for user_id in 0..MAX_TRACKED_USERS {
            let _ = l.check(user_id as i64);
        }

        assert!(!l.check(MAX_TRACKED_USERS as i64).is_allowed());
        assert_eq!(
            l.state.lock().unwrap().users.len(),
            MAX_TRACKED_USERS,
            "blocked user ID must not allocate limiter state"
        );
    }

    #[test]
    fn fresh_identities_share_a_bounded_persistent_work_budget() {
        let l = CommandRateLimiter::new();
        // A fixed instant keeps the global bucket from refilling mid-loop.
        let now = Instant::now();
        for user_id in 0..GLOBAL_COMMANDS_PER_MINUTE {
            assert!(
                l.check_at(i64::from(user_id), now).is_allowed(),
                "global burst within quota"
            );
        }
        assert!(!l
            .check_at(i64::from(GLOBAL_COMMANDS_PER_MINUTE), now)
            .is_allowed());
    }

    #[test]
    fn globally_rejected_users_keep_their_bucket_tracking() {
        let limiter = CommandRateLimiter::new();
        let now = Instant::now();
        for user_id in 0..GLOBAL_COMMANDS_PER_MINUTE {
            assert!(limiter.check_at(i64::from(user_id), now).is_allowed());
        }
        let expired = now - TRACKED_USER_TTL - Duration::from_secs(1);
        limiter
            .state
            .lock()
            .unwrap()
            .users
            .get_mut(&0)
            .unwrap()
            .last_seen = expired;
        assert!(!limiter.check_at(0, now).is_allowed());
        assert!(
            limiter.state.lock().unwrap().users[&0].last_seen > expired,
            "a rejected access must retain its tracking entry"
        );
        for user_id in GLOBAL_COMMANDS_PER_MINUTE..MAX_TRACKED_USERS as u32 * 2 {
            let _ = limiter.check_at(i64::from(user_id), now);
        }
        assert_eq!(limiter.state.lock().unwrap().users.len(), MAX_TRACKED_USERS);
    }

    #[test]
    fn globally_rejected_requests_do_not_consume_user_budget() {
        let limiter = CommandRateLimiter::new();
        let now = Instant::now();
        for user_id in 0..GLOBAL_COMMANDS_PER_MINUTE {
            assert!(limiter.check_at(i64::from(user_id), now).is_allowed());
        }
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(!limiter.check_at(0, now).is_allowed());
        }

        assert!(limiter
            .check_at(0, now + Duration::from_millis(500))
            .is_allowed());
    }

    #[test]
    fn rejections_name_the_budget_that_ran_out() {
        let l = CommandRateLimiter::new();
        let now = Instant::now();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check_at(7, now).is_allowed());
        }
        assert_eq!(l.check_at(7, now), RateLimit::UserBudgetExhausted);

        for user_id in 8..i64::from(GLOBAL_COMMANDS_PER_MINUTE) + 2 {
            let _ = l.check_at(user_id, now);
        }
        assert_eq!(
            l.check_at(i64::from(GLOBAL_COMMANDS_PER_MINUTE) + 2, now),
            RateLimit::SharedCeiling
        );
    }

    #[test]
    fn identity_ceiling_rejections_are_shared() {
        let l = CommandRateLimiter::new();
        let now = Instant::now();
        for user_id in 0..MAX_TRACKED_USERS {
            let _ = l.check_at(user_id as i64, now);
        }
        assert_eq!(
            l.check_at(MAX_TRACKED_USERS as i64, now),
            RateLimit::SharedCeiling
        );
    }
}
