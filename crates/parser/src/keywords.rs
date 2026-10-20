//! Keyword tables and matchers for the time-expression parser.
//!
//! Two flavors of unit-matching co-exist:
//!
//! - **Compact suffixes** — single-character tags glued onto a number, like
//!   `5m` or `1Y2M15d`. These are *case-sensitive* (`m`≠`M`, `y` is not a
//!   thing — use `Y`). Only the seven characters listed in `compact_unit`
//!   are valid.
//! - **Longforms** — whole-word units like `minuten`, `stunden`, `monate`.
//!   Case-insensitive exact matches, used as a separate token after a number
//!   (`30 minuten`). Not used inside combined REL specs.
//!
//! All other matchers (named days, "uhr", "in", "at", recurring keywords)
//! are case-insensitive whole-token matches.

use chrono::Weekday;
use strsim::levenshtein;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NamedDay {
    /// Kept as an explicit variant so the grammar can reject `heute`/`today`
    /// with a dedicated hint pointing the user at bare-clock-time.
    Today,
    Tomorrow,
    DayAfterTomorrow,
    Weekday(Weekday),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TimeUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

/// Adaptive Levenshtein threshold — short keywords don't get fuzzy slack
/// because they collide with reminder words too easily.
fn max_distance_for(alias_len: usize) -> usize {
    match alias_len {
        0..=3 => 0,
        4..=5 => 1,
        _ => 2,
    }
}

fn best_fuzzy<T: Copy>(token: &str, table: &[(T, &[&str])]) -> Option<T> {
    let mut best: Option<(usize, T)> = None;
    for (canonical, aliases) in table {
        for alias in *aliases {
            let dist = levenshtein(token, alias);
            let max = max_distance_for(alias.len());
            if dist <= max {
                if best.is_none_or(|(d, _)| dist < d) {
                    best = Some((dist, *canonical));
                }
                if dist == 0 {
                    return Some(*canonical);
                }
            }
        }
    }
    best.map(|(_, c)| c)
}

// ---------------------------------------------------------------------------
// Named days
// ---------------------------------------------------------------------------

const NAMED_DAY_TABLE: &[(NamedDay, &[&str])] = &[
    (NamedDay::Today, &["heute", "today"]),
    (NamedDay::Tomorrow, &["morgen", "tomorrow", "tomo"]),
    (NamedDay::DayAfterTomorrow, &["übermorgen", "uebermorgen"]),
    (
        NamedDay::Weekday(Weekday::Mon),
        &["montag", "monday", "mo", "mon"],
    ),
    (
        NamedDay::Weekday(Weekday::Tue),
        &["dienstag", "tuesday", "di", "die", "tue", "tues"],
    ),
    (
        NamedDay::Weekday(Weekday::Wed),
        &["mittwoch", "wednesday", "mi", "mit", "wed"],
    ),
    (
        NamedDay::Weekday(Weekday::Thu),
        &["donnerstag", "thursday", "do", "don", "thu", "thur", "thurs"],
    ),
    (
        NamedDay::Weekday(Weekday::Fri),
        &["freitag", "friday", "fr", "fre", "fri"],
    ),
    (
        NamedDay::Weekday(Weekday::Sat),
        &["samstag", "saturday", "sa", "sam", "sat"],
    ),
    (
        NamedDay::Weekday(Weekday::Sun),
        &["sonntag", "sunday", "so", "son", "sun"],
    ),
];

pub fn match_named_day(token: &str) -> Option<NamedDay> {
    let lower = token.to_lowercase();
    best_fuzzy(&lower, NAMED_DAY_TABLE)
}

// ---------------------------------------------------------------------------
// Compact suffixes: single char, CASE-SENSITIVE.
// ---------------------------------------------------------------------------

/// `m` ≠ `M` (minute vs month) — that collision is the whole reason suffixes
/// are case-sensitive. Year has no such clash, so both `Y` and `y` are
/// accepted (`1y` reads naturally, cf. "22yo"). `j` is still not a suffix.
pub fn compact_unit(c: char) -> Option<TimeUnit> {
    match c {
        's' => Some(TimeUnit::Second),
        'm' => Some(TimeUnit::Minute),
        'h' => Some(TimeUnit::Hour),
        'd' => Some(TimeUnit::Day),
        'w' => Some(TimeUnit::Week),
        'M' => Some(TimeUnit::Month),
        'Y' | 'y' => Some(TimeUnit::Year),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Longforms: whole-word units after a number (`30 minuten`).
// Case-insensitive, exact: a fuzzy match would read reminder words after a
// bare hour as units (`9 Kunden` → `9 sekunden`, `8 take` → `8 tage`).
// ---------------------------------------------------------------------------

const LONGFORM_TABLE: &[(TimeUnit, &[&str])] = &[
    (
        TimeUnit::Second,
        &["sek", "sec", "secs", "sekunde", "sekunden", "second", "seconds"],
    ),
    (
        TimeUnit::Minute,
        &["min", "mins", "minute", "minuten", "minutes"],
    ),
    (
        TimeUnit::Hour,
        &["std", "hr", "hrs", "stunde", "stunden", "hour", "hours"],
    ),
    (TimeUnit::Day, &["tag", "tage", "day", "days"]),
    (TimeUnit::Week, &["woche", "wochen", "week", "weeks"]),
    (TimeUnit::Month, &["monat", "monate", "month", "months"]),
    (TimeUnit::Year, &["jahr", "jahre", "year", "years"]),
];

pub fn match_longform_unit(token: &str) -> Option<TimeUnit> {
    let lower = token.to_lowercase();
    LONGFORM_TABLE
        .iter()
        .find(|(_, aliases)| aliases.contains(&lower.as_str()))
        .map(|(unit, _)| *unit)
}

// ---------------------------------------------------------------------------
// Single-token connectors and triggers.
// ---------------------------------------------------------------------------

const UHR_ALIASES: &[&str] = &["uhr", "o'clock", "oclock"];
const IN_ALIASES: &[&str] = &["in"];
const AT_ALIASES: &[&str] = &["um", "at"];
/// Exact match only — fuzzy would risk swallowing reminder words like "alle"
/// when used as a plural.
const RECURRING_KEYWORDS: &[&str] = &["every", "alle", "jeden", "jede"];

pub fn is_uhr(token: &str) -> bool {
    let lower = token.to_lowercase();
    UHR_ALIASES
        .iter()
        .any(|a| levenshtein(&lower, a) <= max_distance_for(a.len()))
}

pub fn is_in_prefix(token: &str) -> bool {
    let lower = token.to_lowercase();
    IN_ALIASES.iter().any(|a| lower == *a)
}

pub fn is_at_prefix(token: &str) -> bool {
    let lower = token.to_lowercase();
    AT_ALIASES.iter().any(|a| lower == *a)
}

pub fn is_recurring_keyword(token: &str) -> bool {
    let lower = token.to_lowercase();
    RECURRING_KEYWORDS.iter().any(|a| lower == *a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_day_exact_de_en() {
        assert_eq!(match_named_day("Montag"), Some(NamedDay::Weekday(Weekday::Mon)));
        assert_eq!(match_named_day("monday"), Some(NamedDay::Weekday(Weekday::Mon)));
        assert_eq!(match_named_day("morgen"), Some(NamedDay::Tomorrow));
        assert_eq!(match_named_day("heute"), Some(NamedDay::Today));
        assert_eq!(match_named_day("today"), Some(NamedDay::Today));
    }

    #[test]
    fn named_day_fuzzy() {
        assert_eq!(match_named_day("donnerstah"), Some(NamedDay::Weekday(Weekday::Thu)));
        assert_eq!(match_named_day("morgne"), Some(NamedDay::Tomorrow));
    }

    #[test]
    fn named_day_short_aliases_exact_only() {
        assert_eq!(match_named_day("do"), Some(NamedDay::Weekday(Weekday::Thu)));
        // "da" would be distance 1 from "do" — but short aliases require distance 0.
        assert_eq!(match_named_day("da"), None);
    }

    #[test]
    fn named_day_unrelated() {
        assert_eq!(match_named_day("pizza"), None);
    }

    #[test]
    fn compact_units_are_case_sensitive() {
        assert_eq!(compact_unit('m'), Some(TimeUnit::Minute));
        assert_eq!(compact_unit('M'), Some(TimeUnit::Month));
        assert_eq!(compact_unit('h'), Some(TimeUnit::Hour));
        assert_eq!(compact_unit('H'), None);
        assert_eq!(compact_unit('Y'), Some(TimeUnit::Year));
        // Year is the one case-insensitive suffix — no minute/month-style clash.
        assert_eq!(compact_unit('y'), Some(TimeUnit::Year));
        assert_eq!(compact_unit('j'), None);
        assert_eq!(compact_unit('x'), None);
    }

    #[test]
    fn longform_units_match() {
        assert_eq!(match_longform_unit("minuten"), Some(TimeUnit::Minute));
        assert_eq!(match_longform_unit("MINUTEN"), Some(TimeUnit::Minute));
        assert_eq!(match_longform_unit("stunden"), Some(TimeUnit::Hour));
        assert_eq!(match_longform_unit("jahre"), Some(TimeUnit::Year));
        // single-char "m" is *not* a longform — that's the compact path.
        assert_eq!(match_longform_unit("m"), None);
    }

    #[test]
    fn longform_units_reject_near_misses() {
        assert_eq!(match_longform_unit("kunden"), None);
        assert_eq!(match_longform_unit("lage"), None);
        assert_eq!(match_longform_unit("socken"), None);
        assert_eq!(match_longform_unit("take"), None);
        assert_eq!(match_longform_unit("gear"), None);
    }

    #[test]
    fn uhr_aliases() {
        assert!(is_uhr("Uhr"));
        assert!(is_uhr("o'clock"));
    }
}
