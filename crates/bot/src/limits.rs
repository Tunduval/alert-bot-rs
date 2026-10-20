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
/// individual check passed. This caps persistent work after a webhook-secret
/// compromise without letting one exhausted user consume everyone else's
/// budget.
const GLOBAL_COMMANDS_PER_MINUTE: u32 = 120;

/// Maximum user identities retained by the in-memory limiter. A forged
/// webhook update cannot allocate state beyond this fixed ceiling.
const MAX_TRACKED_USERS: usize = 4_096;

/// An inactive identity may be replaced after every possible token bucket for
/// it has fully recovered. This is deliberately longer than the one-minute
/// per-user quota window.
const TRACKED_USER_TTL: Duration = Duration::from_secs(120);

pub type UserId = i64;

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

    /// Returns `true` if the user may issue this command, `false` if they've
    /// blown through their budget for the current window.
    pub fn check(&self, user_id: UserId) -> bool {
        self.check_at(user_id, Instant::now())
    }

    fn check_at(&self, user_id: UserId, now: Instant) -> bool {
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
            return false;
        }

        let global_tokens = state
            .global
            .refilled_tokens(GLOBAL_COMMANDS_PER_MINUTE, now);
        let user = state.users.entry(user_id).or_insert_with(|| UserBudget {
            bucket: TokenBucket::full(COMMANDS_PER_MINUTE, now),
            last_seen: now,
        });
        let user_tokens = user.bucket.refilled_tokens(COMMANDS_PER_MINUTE, now);
        let accepted = user_tokens >= 1.0 && global_tokens >= 1.0;

        let consumed = if accepted { 1.0 } else { 0.0 };
        user.bucket.tokens = user_tokens - consumed;
        user.bucket.last_refill = now;
        user.last_seen = now;
        state.global.tokens = global_tokens - consumed;
        state.global.last_refill = now;

        accepted
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
            assert!(l.check(42), "burst within quota");
        }
    }

    #[test]
    fn rate_limiter_blocks_after_burst() {
        let l = CommandRateLimiter::new();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check(7));
        }
        // The next command in the same instant must fail.
        assert!(!l.check(7), "post-burst request blocked");
    }

    #[test]
    fn rate_limiter_is_per_user() {
        let l = CommandRateLimiter::new();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check(1));
        }
        assert!(!l.check(1), "user 1 exhausted");
        assert!(l.check(2), "user 2 still has full budget");
    }

    #[test]
    fn one_user_cannot_exhaust_other_users_budget() {
        let l = CommandRateLimiter::new();
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(l.check(7));
        }
        for _ in 0..256 {
            assert!(!l.check(7));
        }
        assert!(l.check(8), "another user must retain an independent budget");
    }

    #[test]
    fn rate_limiter_has_a_hard_identity_ceiling() {
        let l = CommandRateLimiter::new();
        for user_id in 0..MAX_TRACKED_USERS {
            let _ = l.check(user_id as i64);
        }

        assert!(!l.check(MAX_TRACKED_USERS as i64));
        assert_eq!(
            l.state.lock().unwrap().users.len(),
            MAX_TRACKED_USERS,
            "blocked user ID must not allocate limiter state"
        );
    }

    #[test]
    fn fresh_identities_share_a_bounded_persistent_work_budget() {
        let l = CommandRateLimiter::new();
        for user_id in 0..GLOBAL_COMMANDS_PER_MINUTE {
            assert!(l.check(i64::from(user_id)), "global burst within quota");
        }
        assert!(!l.check(i64::from(GLOBAL_COMMANDS_PER_MINUTE)));
    }

    #[test]
    fn globally_rejected_users_keep_their_bucket_tracking() {
        let limiter = CommandRateLimiter::new();
        for user_id in 0..GLOBAL_COMMANDS_PER_MINUTE {
            assert!(limiter.check(i64::from(user_id)));
        }
        let expired = Instant::now() - TRACKED_USER_TTL - Duration::from_secs(1);
        limiter
            .state
            .lock()
            .unwrap()
            .users
            .get_mut(&0)
            .unwrap()
            .last_seen = expired;
        assert!(!limiter.check(0));
        assert!(
            limiter.state.lock().unwrap().users[&0].last_seen > expired,
            "a rejected access must retain its tracking entry"
        );
        for user_id in GLOBAL_COMMANDS_PER_MINUTE..MAX_TRACKED_USERS as u32 * 2 {
            let _ = limiter.check(i64::from(user_id));
        }
        assert_eq!(limiter.state.lock().unwrap().users.len(), MAX_TRACKED_USERS);
    }

    #[test]
    fn globally_rejected_requests_do_not_consume_user_budget() {
        let limiter = CommandRateLimiter::new();
        let now = Instant::now();
        for user_id in 0..GLOBAL_COMMANDS_PER_MINUTE {
            assert!(limiter.check_at(i64::from(user_id), now));
        }
        for _ in 0..COMMANDS_PER_MINUTE {
            assert!(!limiter.check_at(0, now));
        }

        assert!(limiter.check_at(0, now + Duration::from_millis(500)));
    }
}
