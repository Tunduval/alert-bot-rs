//! Time-expression parser for the alert bot.
//!
//! Parses German and English shorthand into a concrete `DateTime<Utc>` plus
//! the leftover reminder text. The contract lives in `RULES.md`; if this
//! file disagrees with that file, this file is wrong.
//!
//! High-level strategy order for one-shots:
//! relative (compact `1Y2M15d8h40m20s` or longform `30 minuten`) →
//! absolute date (`30.4.26`, `2026-04-30`) → bare clock-time (`22:00`) →
//! named day (`morgen`, `do`).
//!
//! Fuzzy matching applies to keywords only (weekdays, "morgen", "uhr", longform
//! units). Numbers and the reminder text are never fuzzy-matched.

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use thiserror::Error;

mod grammar;
mod keywords;

pub use botcore::recurrence::MIN_INTERVAL_SECONDS;
pub use botcore::{Language, Schedule, ScheduleError};

/// Default time-of-day for absolute / named-day specs without an explicit
/// clock (e.g. `morgen Arzt` → tomorrow at 09:00 local time).
pub const DEFAULT_TIME: NaiveTime = match NaiveTime::from_hms_opt(9, 0, 0) {
    Some(t) => t,
    None => unreachable!(),
};

/// Maximum offset for *relative* one-shot specs (e.g. `100Y` is rejected).
/// Recurring intervals have no upper cap.
pub const MAX_RELATIVE_YEARS: i32 = 50;

#[derive(Debug, Clone)]
pub struct ParseContext {
    pub now_utc: DateTime<Utc>,
    pub tz: Tz,
    pub language: Language,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Parsed {
    /// Resolved schedule. For one-shots, `schedule.dtstart` is the only fire
    /// time. For recurring, `dtstart` is the first occurrence; subsequent
    /// fires are derived via the embedded RRULE.
    pub schedule: Schedule,
    /// Reminder text — everything after the time expression, trimmed.
    pub text: String,
    /// Edge-case notes the user should see *once* at creation time. Typed so
    /// the bot crate owns the user-facing strings.
    pub notes: Vec<ParseNote>,
}

/// Edge-case flags emitted at parse time. Localized in `bot::messages`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ParseNote {
    /// `1Y` added to a Feb-29 date landed in a non-leap target year, so we
    /// clamped to Feb 28.
    LeapClampedToFeb28 { new_year: i32 },
    /// `NM` added past the end of the target month, so the day got clamped
    /// down (e.g. Jan 31 + 1M → Feb 28/29).
    MonthDayClamped { before_day: u32, after_day: u32 },
    /// `*31.` — in months shorter than 31 days, the alert fires on the last
    /// day of the month.
    MonthlyLastDay,
    /// `*NM` created on the 29th–31st — in shorter months, the alert fires
    /// on the last day of the month.
    MonthlyLastDayFrom { day: u8 },
    /// `*29.2` — in non-leap years, the alert fires on Feb 28.
    YearlyFeb29Fallback,
}

impl Parsed {
    /// Convenience for handlers that still think in terms of the first fire.
    pub fn fire_at(&self) -> DateTime<Utc> {
        self.schedule.dtstart
    }
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ParseError {
    #[error("empty input")]
    Empty,
    #[error("could not recognise a time expression at the start of the input")]
    NoTimeExpression,
    #[error("time expression is in the past")]
    InPast,
    #[error("missing reminder text")]
    MissingText,
    #[error("invalid date: {0}")]
    InvalidDate(String),
    #[error("invalid time: {0}")]
    InvalidTime(String),
    #[error("recurring interval too short (minimum {0} seconds)")]
    IntervalTooShort(i64),
    #[error("this kind of spec can't recur")]
    InvalidRecurrenceSpec,
    /// `/alert heute ...` — user should write the bare clock-time instead.
    #[error("'heute'/'today' is not a recognised spec — use a clock time directly")]
    HeuteRejected,
    /// `/alert 15h 11:00 ...` — override only allowed when the relative spec
    /// has no sub-day components.
    #[error("clock-time override is only allowed when the relative spec has no sub-day components")]
    SubDayRelWithOverride,
    /// `/alert 100Y ...` — over the `MAX_RELATIVE_YEARS` cap.
    #[error("relative offset exceeds the {0}-year cap")]
    RelTooFar(i32),
    /// Compact REL components in wrong order or duplicated (e.g. `30m2h`).
    #[error("relative spec components out of order or duplicated")]
    InvalidRelSpec,
    #[error("rrule error: {0}")]
    Rrule(String),
}

impl From<ScheduleError> for ParseError {
    fn from(e: ScheduleError) -> Self {
        match e {
            ScheduleError::Invalid(s) if s.starts_with("interval ") => {
                ParseError::IntervalTooShort(MIN_INTERVAL_SECONDS)
            }
            ScheduleError::IntervalTooLong(_) => ParseError::InvalidRecurrenceSpec,
            other => ParseError::Rrule(other.to_string()),
        }
    }
}

/// Top-level entry point.
pub fn parse(input: &str, ctx: &ParseContext) -> Result<Parsed, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    grammar::parse_command(trimmed, ctx)
}

/// Resolve a (date, time) pair in the user's tz to UTC.
/// Falls back to the next valid local time if the naive datetime falls into a
/// DST gap, and picks the earlier of two ambiguous instants.
pub(crate) fn local_to_utc(
    date: NaiveDate,
    time: NaiveTime,
    tz: Tz,
) -> Result<DateTime<Utc>, ParseError> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earlier, _) => Ok(earlier.with_timezone(&Utc)),
        chrono::LocalResult::None => {
            let bumped = naive
                .with_hour(naive.hour().saturating_add(1))
                .ok_or_else(|| ParseError::InvalidTime(format!("{naive}")))?;
            tz.from_local_datetime(&bumped)
                .single()
                .map(|dt| dt.with_timezone(&Utc))
                .ok_or_else(|| ParseError::InvalidTime(format!("{naive}")))
        }
    }
}

// ===========================================================================
// Tests — fixed reference time 2026-05-08 12:00 Europe/Berlin (a Friday).
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ctx(lang: Language) -> ParseContext {
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        let now_local = tz.with_ymd_and_hms(2026, 5, 8, 12, 0, 0).unwrap();
        ParseContext {
            now_utc: now_local.with_timezone(&Utc),
            tz,
            language: lang,
        }
    }

    fn p(input: &str) -> Parsed {
        parse(input, &ctx(Language::De)).expect("parse failed")
    }
    fn p_en(input: &str) -> Parsed {
        parse(input, &ctx(Language::En)).expect("parse failed")
    }
    fn err(input: &str) -> ParseError {
        parse(input, &ctx(Language::De)).unwrap_err()
    }
    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        ctx(Language::De)
            .tz
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .unwrap()
            .with_timezone(&Utc)
    }
    fn parse_at(input: &str, now_utc: DateTime<Utc>) -> Result<Parsed, ParseError> {
        let ctx = ParseContext {
            now_utc,
            ..ctx(Language::De)
        };
        parse(input, &ctx)
    }

    // ---- Relative simple ----

    #[test]
    fn rel_minutes_short() {
        let r = p("5m Kaffee fertig");
        assert_eq!(r.text, "Kaffee fertig");
        assert_eq!(r.fire_at(), ctx(Language::De).now_utc + chrono::Duration::minutes(5));
    }

    #[test]
    fn rel_minutes_longform_de() {
        let r = p("30 minuten Pizza");
        assert_eq!(r.text, "Pizza");
        assert_eq!(r.fire_at(), ctx(Language::De).now_utc + chrono::Duration::minutes(30));
    }

    #[test]
    fn rel_with_in_prefix() {
        let r = p("in 2 stunden trinken");
        assert_eq!(r.text, "trinken");
        assert_eq!(r.fire_at(), ctx(Language::De).now_utc + chrono::Duration::hours(2));
    }

    #[test]
    fn rel_days() {
        let r = p("30d abo kündigen");
        assert_eq!(r.text, "abo kündigen");
        assert_eq!(r.fire_at(), ctx(Language::De).now_utc + chrono::Duration::days(30));
    }

    #[test]
    fn rel_weeks_en() {
        let r = p_en("2w pay rent");
        assert_eq!(r.text, "pay rent");
        assert_eq!(r.fire_at(), ctx(Language::De).now_utc + chrono::Duration::weeks(2));
    }

    #[test]
    fn rel_compact_uppercase_m_is_months_not_minutes() {
        // Case-sensitive: `5M` is months, `5m` is minutes.
        let r = p("5M something");
        // 2026-05-08 12:00 + 5 calendar months = 2026-10-08 12:00 local.
        assert_eq!(r.fire_at(), local(2026, 10, 8, 12, 0));
    }

    #[test]
    fn rel_compact_uppercase_y() {
        let r = p("1Y birthday");
        assert_eq!(r.fire_at(), local(2027, 5, 8, 12, 0));
    }

    #[test]
    fn rel_lowercase_y_accepted_as_year() {
        // `y` and `Y` both mean year — no minute/month-style collision, so we
        // accept the lowercase form (`5y` reads naturally).
        let r = p("5y text");
        assert_eq!(r.fire_at(), local(2031, 5, 8, 12, 0));
    }

    // ---- Relative combined ----

    #[test]
    fn rel_combined_h_m() {
        let r = p("7h30m Pause");
        assert_eq!(r.text, "Pause");
        assert_eq!(
            r.fire_at(),
            ctx(Language::De).now_utc + chrono::Duration::hours(7) + chrono::Duration::minutes(30)
        );
    }

    #[test]
    fn rel_combined_d_h() {
        let r = p("2d12h text");
        // wall-time preserved on the date part (5/8 + 2d = 5/10), then +12h.
        // 2026-05-10 12:00 local + 12h = 2026-05-11 00:00 local.
        assert_eq!(r.fire_at(), local(2026, 5, 11, 0, 0));
    }

    #[test]
    fn rel_combined_maximal() {
        // Just check it parses and lands in the future; exact value validated
        // in unit tests on RelSpec::apply.
        let r = p("1Y2M15d8h40m20s test");
        assert!(r.fire_at() > ctx(Language::De).now_utc);
    }

    #[test]
    fn rel_combined_wrong_order_rejected() {
        let e = err("30m2h text");
        assert_eq!(e, ParseError::InvalidRelSpec);
    }

    // ---- Relative + override ----

    #[test]
    fn rel_override_multi_day() {
        let r = p("2d 11:00 vitamin");
        assert_eq!(r.text, "vitamin");
        // 5/8 + 2d = 5/10, override 11:00 local.
        assert_eq!(r.fire_at(), local(2026, 5, 10, 11, 0));
    }

    #[test]
    fn rel_override_week() {
        let r = p("1w 18:00 zahlen");
        assert_eq!(r.fire_at(), local(2026, 5, 15, 18, 0));
    }

    #[test]
    fn rel_override_year() {
        let r = p("1Y 9:00 jubilaeum");
        assert_eq!(r.fire_at(), local(2027, 5, 8, 9, 0));
    }

    #[test]
    fn rel_subday_override_rejected() {
        let e = err("15h 11:00 text");
        assert_eq!(e, ParseError::SubDayRelWithOverride);
    }

    #[test]
    fn rel_mixed_subday_override_rejected() {
        let e = err("2d8h 11:00 text");
        assert_eq!(e, ParseError::SubDayRelWithOverride);
    }

    #[test]
    fn rel_subday_followed_by_number_keeps_number_as_text() {
        let now = ctx(Language::De).now_utc;
        for (input, offset, text) in [
            (
                "30m 2 Eier kochen",
                chrono::Duration::minutes(30),
                "2 Eier kochen",
            ),
            (
                "1h 3 Mails beantworten",
                chrono::Duration::hours(1),
                "3 Mails beantworten",
            ),
            (
                "in 10 minuten 4 Tabletten",
                chrono::Duration::minutes(10),
                "4 Tabletten",
            ),
        ] {
            let r = p(input);
            assert_eq!(r.text, text, "{input}");
            assert_eq!(r.fire_at(), now + offset, "{input}");
        }
    }

    #[test]
    fn rel_50_year_cap() {
        let e = err("100Y text");
        assert_eq!(e, ParseError::RelTooFar(MAX_RELATIVE_YEARS));
    }

    // ---- Absolute date ----

    #[test]
    fn abs_short_year() {
        let r = p("30.4.27 scheidung einreichen");
        assert_eq!(r.text, "scheidung einreichen");
        assert_eq!(r.fire_at(), local(2027, 4, 30, 9, 0));
    }

    #[test]
    fn abs_full_year_with_time() {
        let r = p("30.04.2027 14:30 Termin");
        assert_eq!(r.text, "Termin");
        assert_eq!(r.fire_at(), local(2027, 4, 30, 14, 30));
    }

    #[test]
    fn abs_no_year_assumes_next_occurrence() {
        let r = p("10.5 Geburtstag");
        assert_eq!(r.fire_at(), local(2026, 5, 10, 9, 0));
    }

    #[test]
    fn abs_trailing_dot() {
        let r = p("30.4. Termin");
        assert_eq!(r.fire_at(), local(2027, 4, 30, 9, 0)); // past this year → next
    }

    #[test]
    fn abs_no_year_today_with_time_passed_rolls_to_next_year() {
        // Reference 2026-05-08 12:00: today's 09:00 default and an explicit
        // 11:00 are past, so the next occurrence is next year.
        assert_eq!(p("8.5 Termin").fire_at(), local(2027, 5, 8, 9, 0));
        assert_eq!(p("8.5. 11:00 Termin").fire_at(), local(2027, 5, 8, 11, 0));
        // Still ahead today → stays this year.
        assert_eq!(p("8.5 15:00 Termin").fire_at(), local(2026, 5, 8, 15, 0));
    }

    #[test]
    fn abs_no_year_leap_day_waits_for_the_next_leap_year() {
        // Sent in 2029: neither 2029 nor 2030 has Feb 29, the next is 2032.
        let r = parse_at("29.2 Schalttag", local(2029, 1, 15, 12, 0)).unwrap();
        assert_eq!(r.fire_at(), local(2032, 2, 29, 9, 0));
    }

    #[test]
    fn abs_iso_date_future() {
        let r = p("2026-12-31 sylvester");
        assert_eq!(r.fire_at(), local(2026, 12, 31, 9, 0));
    }

    #[test]
    fn abs_iso_date_short_month_day() {
        let r = p("2026-7-1 quartal");
        assert_eq!(r.fire_at(), local(2026, 7, 1, 9, 0));
    }

    #[test]
    fn abs_iso_date_in_past_rejected() {
        assert_eq!(err("2020-01-01 foo"), ParseError::InPast);
    }

    // ---- Bare clock-time ----

    #[test]
    fn bare_clock_today_future() {
        // Reference 12:00 → "15:00" is later today.
        let r = p("15:00 nap");
        assert_eq!(r.fire_at(), local(2026, 5, 8, 15, 0));
        assert_eq!(r.text, "nap");
    }

    #[test]
    fn bare_clock_today_passed_picks_tomorrow() {
        let r = p("09:00 morgenmuffel");
        assert_eq!(r.fire_at(), local(2026, 5, 9, 9, 0));
    }

    #[test]
    fn bare_clock_hour_only() {
        let r = p("14 Uhr text");
        assert_eq!(r.fire_at(), local(2026, 5, 8, 14, 0));
    }

    #[test]
    fn bare_clock_am_pm() {
        let r = p("2pm meeting");
        assert_eq!(r.fire_at(), local(2026, 5, 8, 14, 0));
        let r2 = p("9am workout");
        // 9am today is past (ref 12:00) → tomorrow.
        assert_eq!(r2.fire_at(), local(2026, 5, 9, 9, 0));
    }

    #[test]
    fn am_pm_12_edges() {
        // 12am = midnight, 12pm = noon (per RULES.md).
        let r_noon = p("12pm lunch"); // ref 12:00, "12pm" = today 12:00 — equal, so tomorrow.
        assert_eq!(r_noon.fire_at(), local(2026, 5, 9, 12, 0));
        let r_mid = p("12am owl");
        assert_eq!(r_mid.fire_at(), local(2026, 5, 9, 0, 0));
    }

    // ---- Named day ----

    #[test]
    fn named_morgen_with_time() {
        let r = p("morgen 9 Uhr Arzt");
        assert_eq!(r.fire_at(), local(2026, 5, 9, 9, 0));
        assert_eq!(r.text, "Arzt");
    }

    #[test]
    fn named_morgen_default_time() {
        let r = p("morgen Arzt");
        assert_eq!(r.fire_at(), local(2026, 5, 9, 9, 0));
    }

    #[test]
    fn named_uebermorgen() {
        let r = p("übermorgen 14:00 Treffen");
        assert_eq!(r.fire_at(), local(2026, 5, 10, 14, 0));
    }

    #[test]
    fn named_weekday_short() {
        let r = p("do 14:00 Standup");
        assert_eq!(r.fire_at(), local(2026, 5, 14, 14, 0));
    }

    #[test]
    fn named_weekday_long_en() {
        let r = p_en("monday 10:00 standup");
        assert_eq!(r.fire_at(), local(2026, 5, 11, 10, 0));
    }

    #[test]
    fn named_weekday_today_time_ahead_picks_today() {
        let r = p("fr 14:00 workout");
        assert_eq!(r.fire_at(), local(2026, 5, 8, 14, 0));
    }

    #[test]
    fn named_weekday_today_time_passed_next_week() {
        let r = p("fr 09:00 routine");
        assert_eq!(r.fire_at(), local(2026, 5, 15, 9, 0));
    }

    #[test]
    fn named_weekday_default_time_passed_next_week() {
        let r = p("fr putzen");
        assert_eq!(r.fire_at(), local(2026, 5, 15, 9, 0));
    }

    #[test]
    fn named_weekday_in_dst_gap_keeps_weekday_and_bumps_time() {
        // Europe/Berlin springs forward on Sunday 2027-03-28: 02:30 does not
        // exist. Sent on the Saturday before, `so 2:30` must stay on Sunday.
        let r = parse_at("so 2:30 x", local(2027, 3, 27, 12, 0)).unwrap();
        assert_eq!(r.fire_at(), local(2027, 3, 28, 3, 30));
    }

    #[test]
    fn heute_rejected_with_hint() {
        let e = err("heute 22:00 text");
        assert_eq!(e, ParseError::HeuteRejected);
        let e2 = err("today 22:00 text");
        assert_eq!(e2, ParseError::HeuteRejected);
    }

    // ---- Fuzzy ----

    #[test]
    fn fuzzy_typo_morgen() {
        let r = p("morgne 10 Uhr Arzt");
        assert_eq!(r.text, "Arzt");
    }

    #[test]
    fn fuzzy_does_not_eat_unrelated_words() {
        assert_eq!(err("Pizza essen"), ParseError::NoTimeExpression);
    }

    // ---- Errors ----

    #[test]
    fn err_no_text() {
        assert_eq!(err("5m"), ParseError::MissingText);
    }

    #[test]
    fn err_in_past_absolute() {
        assert_eq!(err("01.01.2020 foo"), ParseError::InPast);
    }

    #[test]
    fn err_no_time_expression() {
        assert_eq!(err("hallo welt"), ParseError::NoTimeExpression);
    }

    #[test]
    fn err_invalid_suffix() {
        // `5x` isn't a valid compact suffix and `x` isn't a longform either.
        assert_eq!(err("5x text"), ParseError::NoTimeExpression);
    }

    // ============================================================
    // Recurring — RRULE string + dtstart shape checks.
    // ============================================================

    fn rrule(r: &Parsed) -> &str {
        r.schedule.rrule.as_deref().expect("expected recurring")
    }

    #[test]
    fn rec_short_interval_30m() {
        let r = p("*30m wasser trinken");
        assert!(rrule(&r).contains("FREQ=MINUTELY"));
        assert!(rrule(&r).contains("INTERVAL=30"));
    }

    #[test]
    fn rec_long_interval_30m() {
        let r = p("alle 30m wasser trinken");
        assert!(rrule(&r).contains("FREQ=MINUTELY"));
        assert!(rrule(&r).contains("INTERVAL=30"));
    }

    #[test]
    fn rec_too_short() {
        assert_eq!(err("*5m wasser"), ParseError::IntervalTooShort(1800));
    }

    #[test]
    fn rec_interval_days_overflow_rejected() {
        // *70000d would wrap to 4464 days as a bare u16 cast. Reject cleanly.
        assert_eq!(err("*70000d text"), ParseError::InvalidRecurrenceSpec);
    }

    #[test]
    fn rec_interval_subday_overflow_rejected() {
        // *46d30m = 66_270 minutes > u16::MAX → must not silently wrap.
        assert_eq!(err("*46d30m text"), ParseError::InvalidRecurrenceSpec);
    }

    #[test]
    fn rec_daily_short() {
        // Relative recurrence fires at creation time (reference clock = 12:00),
        // not the 09:00 date-default.
        let r = p("*1d vitamin");
        let s = rrule(&r);
        assert!(s.contains("FREQ=DAILY"));
        assert!(s.contains("BYHOUR=12"));
    }

    #[test]
    fn rec_daily_anchor_is_one_full_interval_ahead() {
        // Regression: `*6d` used to anchor on *tomorrow* (the anchor search
        // walked day by day and the creation time has always just passed), so
        // the first fire came a day later and only then settled into the
        // 6-day rhythm. First fire must be 6 days out.
        let r = p("*6d Tirz");
        assert!(rrule(&r).contains("INTERVAL=6"));
        assert_eq!(r.schedule.dtstart, local(2026, 5, 14, 12, 0));
    }

    #[test]
    fn rec_daily_interval_one_anchors_tomorrow() {
        // `*1d` without override: today's 12:00 slot is the creation instant
        // itself, so the first fire is tomorrow — one full interval.
        let r = p("*1d vitamin");
        assert_eq!(r.schedule.dtstart, local(2026, 5, 9, 12, 0));
    }

    #[test]
    fn rec_daily_override_takes_today_when_still_ahead() {
        // `*1d 14:00` at 12:00 → tonight, not tomorrow.
        let r = p("*1d 14:00 vitamin");
        assert_eq!(r.schedule.dtstart, local(2026, 5, 8, 14, 0));
    }

    #[test]
    fn rec_daily_override_skips_full_interval_when_passed() {
        // `*2d 11:00` at 12:00 → 11:00 is gone, so +2 days, not +1.
        let r = p("*2d 11:00 Vitamin");
        assert_eq!(r.schedule.dtstart, local(2026, 5, 10, 11, 0));
    }

    #[test]
    fn rec_weekly_via_days_anchor_is_one_interval_ahead() {
        // `*1w` = 7 days → same daily rule with INTERVAL=7.
        let r = p("*1w Muell");
        assert!(rrule(&r).contains("INTERVAL=7"));
        assert_eq!(r.schedule.dtstart, local(2026, 5, 15, 12, 0));
    }

    #[test]
    fn rec_monthly_interval_anchor_skips_full_interval() {
        // `*2M` on 8 May at 12:00 → 8 July, not 8 June.
        let r = p("*2M abrechnung");
        assert!(rrule(&r).contains("INTERVAL=2"));
        assert_eq!(r.schedule.dtstart, local(2026, 7, 8, 12, 0));
    }

    #[test]
    fn rec_yearly_interval_anchor_skips_full_interval() {
        // `*2Y` in 2026 → 2028, not 2027.
        let r = p("*2Y tuv");
        assert!(rrule(&r).contains("INTERVAL=2"));
        assert_eq!(r.schedule.dtstart, local(2028, 5, 8, 12, 0));
    }

    #[test]
    fn rec_relative_anchor_drops_creation_seconds() {
        // Seconds of the creation instant must not be frozen into the rule —
        // otherwise every occurrence for years fires at :37.
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        let ctx = ParseContext {
            now_utc: tz
                .with_ymd_and_hms(2026, 5, 8, 12, 0, 37)
                .unwrap()
                .with_timezone(&Utc),
            tz,
            language: Language::De,
        };
        let r = parse("*6d Tirz", &ctx).expect("parse failed");
        assert_eq!(r.schedule.dtstart, local(2026, 5, 14, 12, 0));
    }

    #[test]
    fn rec_daily_short_with_time_override() {
        // `*2d 11:00` fires every 2 days at 11:00 — the override wins over the
        // 12:00 creation time and must not leak into the reminder text.
        let r = p("*2d 11:00 Vitamin");
        let s = rrule(&r);
        assert!(s.contains("FREQ=DAILY"));
        assert!(s.contains("INTERVAL=2"));
        assert!(s.contains("BYHOUR=11"), "expected 11:00 override, got {s}");
        assert_eq!(r.text, "Vitamin");
    }

    #[test]
    fn rec_daily_keyword_with_time_override() {
        // Keyword form `alle 2d 11:00` must behave identically to `*2d 11:00`.
        let r = p("alle 2d 11:00 Vitamin");
        let s = rrule(&r);
        assert!(s.contains("FREQ=DAILY"));
        assert!(s.contains("BYHOUR=11"), "expected 11:00 override, got {s}");
        assert_eq!(r.text, "Vitamin");
    }

    #[test]
    fn rec_monthly_short_with_time_override() {
        let r = p("*3M 18:00 abrechnung");
        let s = rrule(&r);
        assert!(s.contains("FREQ=MONTHLY"));
        assert!(s.contains("INTERVAL=3"));
        assert!(s.contains("BYHOUR=18"), "expected 18:00 override, got {s}");
        assert_eq!(r.text, "abrechnung");
    }

    #[test]
    fn rec_yearly_short_with_time_override() {
        let r = p("*1Y 9:00 tuv");
        let s = rrule(&r);
        assert!(s.contains("FREQ=YEARLY"));
        assert!(s.contains("BYHOUR=9"), "expected 09:00 override, got {s}");
        assert_eq!(r.text, "tuv");
    }

    #[test]
    fn rec_subday_with_time_override_rejected() {
        // `*30m 11:00` — an interval that also wants a fixed clock is
        // contradictory, same as one-shot relative.
        assert_eq!(err("*30m 11:00 wasser"), ParseError::SubDayRelWithOverride);
    }

    #[test]
    fn rec_subday_followed_by_number_keeps_number_as_text() {
        let r = p("*30m 2 Gläser Wasser");
        assert_eq!(r.text, "2 Gläser Wasser");
        assert!(rrule(&r).contains("FREQ=MINUTELY"));
    }

    #[test]
    fn rec_subday_interval_is_exact_across_dst() {
        // Europe/Berlin springs forward at 2027-03-28 01:00 UTC. Sub-day
        // intervals must stay exactly N apart on the UTC timeline.
        for (input, step) in [
            ("*3h x", chrono::Duration::hours(3)),
            ("*30m x", chrono::Duration::minutes(30)),
        ] {
            let now = local(2027, 3, 27, 22, 0);
            let r = parse_at(input, now).unwrap();
            let mut prev = r.fire_at();
            assert_eq!(prev, now + step, "{input}");
            for _ in 0..8 {
                let next = r.schedule.next_after(prev).unwrap();
                assert_eq!(next - prev, step, "{input}: {prev} -> {next}");
                prev = next;
            }
        }
    }

    #[test]
    fn rec_weekly_single_day_long() {
        let r = p("jeden donnerstag 14:00 standup");
        let s = rrule(&r);
        assert!(s.contains("FREQ=WEEKLY"));
        assert!(s.contains("BYDAY=TH"));
        assert!(s.contains("BYHOUR=14"));
    }

    #[test]
    fn rec_weekly_short_with_time() {
        let r = p("*do 14:00 standup");
        let s = rrule(&r);
        assert!(s.contains("FREQ=WEEKLY"));
        assert!(s.contains("BYDAY=TH"));
    }

    #[test]
    fn rec_weekly_multi_day_comma() {
        let r = p("*mo,mi,fr 9 yoga");
        let s = rrule(&r);
        assert!(s.contains("FREQ=WEEKLY"));
        assert!(s.contains("BYDAY=MO,WE,FR"));
    }

    #[test]
    fn rec_weekly_default_time() {
        let r = p("jeden montag yoga");
        let s = rrule(&r);
        assert!(s.contains("FREQ=WEEKLY"));
        assert!(s.contains("BYDAY=MO"));
        assert!(s.contains("BYHOUR=9"));
    }

    #[test]
    fn rec_monthly_first() {
        let r = p("*1. miete bezahlen");
        let s = rrule(&r);
        assert!(s.contains("FREQ=MONTHLY"));
        assert!(s.contains("BYMONTHDAY=1"));
    }

    #[test]
    fn rec_monthly_31_uses_last_day_and_emits_note() {
        let r = p("*31. miete");
        let s = rrule(&r);
        assert!(s.contains("FREQ=MONTHLY"));
        assert!(s.contains("BYMONTHDAY=-1"));
        assert!(!r.notes.is_empty(), "expected a last-day hint");
    }

    #[test]
    fn rec_monthly_with_time() {
        let r = p("jeden 15. 18:00 abrechnung");
        let s = rrule(&r);
        assert!(s.contains("FREQ=MONTHLY"));
        assert!(s.contains("BYMONTHDAY=15"));
        assert!(s.contains("BYHOUR=18"));
    }

    #[test]
    fn rec_yearly_short() {
        let r = p("*24.12 heiligabend");
        let s = rrule(&r);
        assert!(s.contains("FREQ=YEARLY"));
        assert!(s.contains("BYMONTH=12"));
        assert!(s.contains("BYMONTHDAY=24"));
    }

    #[test]
    fn rec_yearly_29_feb_uses_last_day_and_emits_note() {
        let r = p("*29.2 schaltjahr");
        let s = rrule(&r);
        assert!(s.contains("FREQ=YEARLY"));
        assert!(s.contains("BYMONTH=2"));
        assert!(s.contains("BYMONTHDAY=-1"));
        assert!(!r.notes.is_empty(), "expected a leap-day hint");
    }

    #[test]
    fn rec_yearly_long_en() {
        let r = p_en("every 24.12 christmas");
        let s = rrule(&r);
        assert!(s.contains("FREQ=YEARLY"));
        assert!(s.contains("BYMONTH=12"));
    }

    // ---- Recurring relative Y/M (calendar frequency) ----

    #[test]
    fn rec_relative_yearly() {
        // *1Y on May 8 == yearly on 8 May at creation time (12:00). First fire
        // is next year — today's 12:00 is the creation instant, not future.
        let r = p("*1Y FIT-Test machen");
        let s = rrule(&r);
        assert!(s.contains("FREQ=YEARLY"));
        assert!(s.contains("BYMONTH=5"));
        assert!(s.contains("BYMONTHDAY=8"));
        assert!(s.contains("BYHOUR=12"));
        assert_eq!(r.fire_at(), local(2027, 5, 8, 12, 0));
    }

    #[test]
    fn rec_relative_yearly_lowercase_y() {
        // y and Y both mean year — no collision (unlike m/M).
        let r = p("*1y vorsorge");
        assert!(rrule(&r).contains("FREQ=YEARLY"));
    }

    #[test]
    fn rec_relative_every_two_years() {
        let r = p("*2Y tüv");
        let s = rrule(&r);
        assert!(s.contains("FREQ=YEARLY"));
        assert!(s.contains("INTERVAL=2"));
    }

    #[test]
    fn rec_relative_monthly() {
        // *1M on the 8th == monthly on the 8th at creation time (12:00). First
        // fire next month — today's 12:00 is the creation instant.
        let r = p("*1M zählerstand");
        let s = rrule(&r);
        assert!(s.contains("FREQ=MONTHLY"));
        assert!(s.contains("BYMONTHDAY=8"));
        assert!(s.contains("BYHOUR=12"));
        assert_eq!(r.fire_at(), local(2026, 6, 8, 12, 0));
    }

    #[test]
    fn rec_relative_every_three_months() {
        let r = p("*3M quartalsbericht");
        let s = rrule(&r);
        assert!(s.contains("FREQ=MONTHLY"));
        assert!(s.contains("INTERVAL=3"));
    }

    #[test]
    fn rec_relative_mixed_year_month_rejected() {
        assert_eq!(err("*1Y2M unsinn"), ParseError::InvalidRecurrenceSpec);
        // Lowercase y mixes the same way — case of the year suffix is irrelevant
        // to the family check.
        assert_eq!(err("*1y2M unsinn"), ParseError::InvalidRecurrenceSpec);
    }

    #[test]
    fn rec_relative_mixed_year_subday_rejected() {
        assert_eq!(err("*1Y3d unsinn"), ParseError::InvalidRecurrenceSpec);
    }

    #[test]
    fn rec_missing_text() {
        assert_eq!(err("*30m"), ParseError::MissingText);
    }

    #[test]
    fn rec_no_recognised_spec_after_keyword() {
        assert_eq!(err("every gibberish"), ParseError::NoTimeExpression);
    }

    fn p_at(input: &str, y: i32, m: u32, d: u32) -> Parsed {
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        let now_local = tz.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap();
        let ctx = ParseContext {
            now_utc: now_local.with_timezone(&Utc),
            tz,
            language: Language::De,
        };
        parse(input, &ctx).expect("parse failed")
    }

    #[test]
    fn rec_relative_monthly_from_day_31_keeps_short_months() {
        // *1M on 31 Jan → 28 Feb, not 31 Mar.
        let r = p_at("*1M miete", 2027, 1, 31);
        assert!(rrule(&r).contains("BYSETPOS=-1"));
        assert_eq!(r.fire_at(), local(2027, 2, 28, 12, 0));
        assert_eq!(r.notes, vec![ParseNote::MonthlyLastDayFrom { day: 31 }]);
    }

    #[test]
    fn rec_relative_every_three_months_from_day_31() {
        // *3M on 31 Jan → 30 Apr, not 31 Jul.
        let r = p_at("*3M bericht", 2027, 1, 31);
        assert_eq!(r.fire_at(), local(2027, 4, 30, 12, 0));
    }

    #[test]
    fn rec_relative_yearly_from_feb_29_uses_feb_28() {
        // *1Y on 29 Feb 2028 → 28 Feb 2029, not 29 Feb 2032.
        let r = p_at("*1Y jubiläum", 2028, 2, 29);
        assert!(rrule(&r).contains("BYMONTHDAY=-1"));
        assert_eq!(r.fire_at(), local(2029, 2, 28, 12, 0));
        assert_eq!(r.notes, vec![ParseNote::YearlyFeb29Fallback]);
    }
}
