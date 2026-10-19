//! Anti-spam limits: rate limiter + per-user/per-chat alert quotas + text length cap.
//!
//! These are *soft* guards meant to keep accidental floods and casual abuse
//! at bay. They're not a substitute for proper Telegram-side moderation (e.g.
//! removing the bot from a hostile group); they just keep the DB bounded and
//! the worker responsive when a single client sends a burst.

use std::collections::{hash_map::Entry, HashMap};
use std::num::NonZeroU32;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use governor::clock::DefaultClock;
use governor::state::keyed::DashMapStateStore;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};

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

/// Maximum active alerts one user may pin to one chat. This prevents one
/// group member from consuming the entire chat-wide quota.
pub const MAX_ALERTS_PER_USER_CHAT: i64 = 10;

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
/// supported message and callback updates before any persistent DB write.
///
/// Restart resets the limiter (it's in-memory by design). That's acceptable
/// — abusers usually move on once the first attempt is throttled, and the
/// soft DB quotas catch persistent ones across restarts.
pub struct CommandRateLimiter {
    limiter: RateLimiter<UserId, DashMapStateStore<UserId>, DefaultClock>,
    global_limiter: DefaultDirectRateLimiter,
    tracked_users: Mutex<HashMap<UserId, Instant>>,
}

impl CommandRateLimiter {
    pub fn new() -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(COMMANDS_PER_MINUTE).unwrap());
        let global_quota = Quota::per_minute(NonZeroU32::new(GLOBAL_COMMANDS_PER_MINUTE).unwrap());
        Self {
            limiter: RateLimiter::dashmap(quota),
            global_limiter: RateLimiter::direct(global_quota),
            tracked_users: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the user may issue this command, `false` if they've
    /// blown through their budget for the current window.
    pub fn check(&self, user_id: UserId) -> bool {
        let now = Instant::now();
        let mut tracked = self
            .tracked_users
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if tracked.len() >= MAX_TRACKED_USERS {
            self.limiter.retain_recent();
            self.limiter.shrink_to_fit();
            tracked.retain(|_, last_seen| now.duration_since(*last_seen) < TRACKED_USER_TTL);
        }
        let has_capacity = tracked.len() < MAX_TRACKED_USERS;
        match tracked.entry(user_id) {
            Entry::Occupied(_) => {}
            Entry::Vacant(entry) if has_capacity => {
                entry.insert(now);
            }
            Entry::Vacant(_) => return false,
        }

        if self.limiter.check_key(&user_id).is_err() || self.global_limiter.check().is_err() {
            return false;
        }

        tracked.insert(user_id, now);
        true
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
            l.limiter.len(),
            MAX_TRACKED_USERS,
            "blocked user ID must not allocate keyed limiter state"
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
}
