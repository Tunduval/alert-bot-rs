//! Domain types shared between parser, storage, and bot crates.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

pub mod recurrence;

pub use recurrence::{Schedule, ScheduleError};

/// Telegram chat type as reported by the Bot API. We persist the raw string
/// so that future Telegram additions don't require a migration.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum ChatType {
    Private,
    Group,
    Supergroup,
    Channel,
}

impl ChatType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
            Self::Supergroup => "supergroup",
            Self::Channel => "channel",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "private" => Some(Self::Private),
            "group" => Some(Self::Group),
            "supergroup" => Some(Self::Supergroup),
            "channel" => Some(Self::Channel),
            _ => None,
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self, Self::Group | Self::Supergroup)
    }
}

/// Who is allowed to edit/delete an alert.
///
/// `Private` — only the creator. Used by `/alert` regardless of chat type.
/// `Shared`  — any member of the chat. Used by `/galert` (group-only).
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum AlertScope {
    Private,
    Shared,
}

impl AlertScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Shared => "shared",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "private" => Some(Self::Private),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }
}

/// Lifecycle state of an alert.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum AlertState {
    Pending,
    Claimed,
    Sending,
    Sent,
    Failed,
    Cancelled,
}

impl AlertState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "claimed" => Some(Self::Claimed),
            "sending" => Some(Self::Sending),
            "sent" => Some(Self::Sent),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// User profile. `language` and `timezone` drive parsing and rendering.
#[derive(Debug, Clone)]
pub struct User {
    pub telegram_id: i64,
    pub timezone: Tz,
    pub language: Language,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum Language {
    De,
    En,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::De => "de",
            Self::En => "en",
        }
    }

    /// Best-effort detection from Telegram's `language_code` field.
    /// Falls back to German because that's the bot's home audience.
    pub fn from_telegram_code(code: Option<&str>) -> Self {
        match code.map(|c| c.split('-').next().unwrap_or("")) {
            Some("en") => Self::En,
            _ => Self::De,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "de" => Some(Self::De),
            "en" => Some(Self::En),
            _ => None,
        }
    }
}

/// Persisted alert.
///
/// `fire_at` is the *next* fire time, mutated after each delivery for
/// recurring alerts. `schedule` carries the immutable anchor + RRULE that
/// the worker uses to compute subsequent fires.
#[derive(Debug, Clone)]
pub struct Alert {
    pub id: i64,
    pub user_id: i64,
    pub chat_id: i64,
    pub chat_type: ChatType,
    pub scope: AlertScope,
    pub text: String,
    pub fire_at: DateTime<Utc>,
    pub schedule: Schedule,
    pub state: AlertState,
    pub attempts: i16,
    pub last_error: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    /// Monotonically increases on every claim, including after successful occurrences.
    pub claim_generation: i64,
    pub fired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Alert {
    /// Permission check for edit/delete operations.
    ///
    /// `Private` alerts are owned by the creator. `Shared` alerts permit any
    /// requester after the caller has established the relevant chat context.
    /// This method does not itself prove current Telegram membership.
    pub fn can_edit(&self, requester_user_id: i64) -> bool {
        match self.scope {
            AlertScope::Private => self.user_id == requester_user_id,
            AlertScope::Shared => true,
        }
    }
}

/// New-alert payload as constructed from a parsed command. For new alerts,
/// `schedule.dtstart == fire_at` (they're set to the same value at creation).
#[derive(Debug, Clone)]
pub struct NewAlert {
    pub user_id: i64,
    pub chat_id: i64,
    pub chat_type: ChatType,
    pub scope: AlertScope,
    pub text: String,
    pub fire_at: DateTime<Utc>,
    pub schedule: Schedule,
}
