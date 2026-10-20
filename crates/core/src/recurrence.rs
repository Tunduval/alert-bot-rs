//! Schedule = one anchor + optional RRULE.
//!
//! Replaces the v0.2 custom `RecurrencePattern` enum. We keep the RRULE body
//! as a string (the canonical RFC-5545 form, minus the `RRULE:` prefix); it
//! gets re-parsed and re-validated on every expansion call. Cheap, and avoids
//! the typestate dance of holding `RRule<Validated>` across operations that
//! conceptually want to rebind `dtstart`.

use chrono::{DateTime, NaiveTime, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use rrule::{Frequency, NWeekday, RRule, RRuleSet, Unvalidated};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Minimum allowed interval for recurring alerts. Below this we'd be a
/// notification firehose; users can always re-add later if the limit lifts.
pub const MIN_INTERVAL_SECONDS: i64 = 1800;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    /// Anchor instant. For one-shots, this *is* the fire time. For recurring,
    /// it's the original first occurrence (immutable after creation).
    pub dtstart: DateTime<Utc>,
    /// Timezone the RRULE's clock-times are interpreted in. Stored per-alert
    /// so that a future user-tz change doesn't drift existing weekly alerts.
    pub tz: Tz,
    /// RFC-5545 RRULE body (without the `RRULE:` prefix). `None` = one-shot.
    pub rrule: Option<String>,
}

#[derive(Debug, Error)]
pub enum ScheduleError {
    #[error("invalid rrule: {0}")]
    Invalid(String),
    /// The requested interval doesn't fit RRULE's u16 INTERVAL field. Kept
    /// distinct from `Invalid` so the parser can't confuse it with the
    /// "interval too short" case (both would otherwise be free-text strings).
    #[error("interval too large ({0}s exceeds the supported maximum)")]
    IntervalTooLong(i64),
    #[error("rrule library error: {0}")]
    Library(#[from] rrule::RRuleError),
}

impl Schedule {
    /// One-shot schedule. `next_after` returns `Some(dtstart)` while it's in
    /// the future and `None` afterwards.
    pub fn one_shot(at: DateTime<Utc>, tz: Tz) -> Self {
        Self { dtstart: at, tz, rrule: None }
    }

    /// Build a recurring schedule from a constructor-built RRule, validating
    /// it against `dtstart` and freezing the canonical string form.
    pub fn recurring(
        dtstart: DateTime<Utc>,
        tz: Tz,
        rule: RRule<Unvalidated>,
    ) -> Result<Self, ScheduleError> {
        let dts_rrule = dtstart.with_timezone(&rrule::Tz::from(tz));
        let validated = rule.validate(dts_rrule)?;
        Ok(Self { dtstart, tz, rrule: Some(validated.to_string()) })
    }

    /// Reconstruct from DB columns.
    pub fn from_db(
        dtstart: DateTime<Utc>,
        tz: Tz,
        rrule_str: Option<&str>,
    ) -> Result<Self, ScheduleError> {
        match rrule_str {
            None | Some("") => Ok(Self::one_shot(dtstart, tz)),
            Some(s) => Ok(Self { dtstart, tz, rrule: Some(s.to_owned()) }),
        }
    }

    /// First fire strictly after `now`. For one-shot, returns dtstart if it's
    /// still in the future, else `None`. For recurring, expands the rule.
    pub fn next_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let Some(rrule_str) = self.rrule.as_deref() else {
            return (self.dtstart > now).then_some(self.dtstart);
        };

        let rule: RRule<Unvalidated> = rrule_str.parse().ok()?;
        let dts = self.dtstart.with_timezone(&rrule::Tz::from(self.tz));
        let validated = rule.validate(dts).ok()?;
        let now_tz = now.with_timezone(&rrule::Tz::from(self.tz));

        // rrule's `after()` is *inclusive* — passing now=T would return the
        // T occurrence itself. We want strictly-after, so fetch a couple and
        // filter. limit=4 is enough to skip the exact-match plus give buffer.
        let set = RRuleSet::new(dts).rrule(validated).after(now_tz);
        set.all(4)
            .dates
            .into_iter()
            .map(|d| d.with_timezone(&Utc))
            .find(|d| *d > now)
    }

    pub fn is_recurring(&self) -> bool {
        self.rrule.is_some()
    }
}

// ===========================================================================
// Constructors that map our domain patterns onto RRULE
// ===========================================================================

impl Schedule {
    /// Sub-day interval (`*30m`, `*3h`) → `FREQ={MINUTELY|HOURLY};INTERVAL=N`.
    /// Rejects intervals shorter than `MIN_INTERVAL_SECONDS`.
    pub fn interval_seconds(
        dtstart: DateTime<Utc>,
        tz: Tz,
        seconds: i64,
    ) -> Result<Self, ScheduleError> {
        if seconds < MIN_INTERVAL_SECONDS {
            return Err(ScheduleError::Invalid(format!(
                "interval {seconds}s below minimum {MIN_INTERVAL_SECONDS}s"
            )));
        }
        // RRULE's INTERVAL is a u16; a plain `as u16` would silently wrap
        // (e.g. 66_270 min → 734), firing at a wildly wrong cadence with no
        // error. Convert checked instead.
        let fit = |n: i64| u16::try_from(n).map_err(|_| ScheduleError::IntervalTooLong(seconds));
        let rule = if seconds % 3600 == 0 {
            RRule::new(Frequency::Hourly).interval(fit(seconds / 3600)?)
        } else if seconds % 60 == 0 {
            RRule::new(Frequency::Minutely).interval(fit(seconds / 60)?)
        } else {
            RRule::new(Frequency::Secondly).interval(fit(seconds)?)
        };
        Self::recurring(dtstart, tz, rule)
    }

    /// Daily at fixed wall-clock time, every N days (DST-safe).
    /// `*1d 09:00` = `every_n_days=1, time=09:00`.
    pub fn daily_at(
        dtstart: DateTime<Utc>,
        tz: Tz,
        every_n_days: u16,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        let rule = RRule::new(Frequency::Daily)
            .interval(every_n_days)
            .by_hour(vec![time.hour() as u8])
            .by_minute(vec![time.minute() as u8]);
        Self::recurring(dtstart, tz, rule)
    }

    /// Weekly on given weekday(s) at fixed wall-clock time.
    pub fn weekly(
        dtstart: DateTime<Utc>,
        tz: Tz,
        days: &[Weekday],
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        let by_weekday: Vec<NWeekday> = days.iter().map(|w| NWeekday::Every(*w)).collect();
        let rule = RRule::new(Frequency::Weekly)
            .by_weekday(by_weekday)
            .by_hour(vec![time.hour() as u8])
            .by_minute(vec![time.minute() as u8]);
        Self::recurring(dtstart, tz, rule)
    }

    /// Monthly on a specific day-of-month at fixed wall-clock time. Strict:
    /// months without that day are skipped (so `day=31` skips Feb/Apr/Jun/
    /// Sep/Nov). For "last day of month with fallback" semantics — what
    /// `*31.` means per RULES.md — use [`Self::monthly_last_day`] instead.
    pub fn monthly(
        dtstart: DateTime<Utc>,
        tz: Tz,
        day: u8,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        Self::monthly_every(dtstart, tz, 1, day, time)
    }

    /// Every `interval` months on a specific day-of-month (`*3M` → INTERVAL=3).
    /// Same strict day semantics as [`Self::monthly`].
    pub fn monthly_every(
        dtstart: DateTime<Utc>,
        tz: Tz,
        interval: u16,
        day: u8,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        let rule = RRule::new(Frequency::Monthly)
            .interval(interval)
            .by_month_day(vec![day as i8])
            .by_hour(vec![time.hour() as u8])
            .by_minute(vec![time.minute() as u8]);
        Self::recurring(dtstart, tz, rule)
    }

    /// Every `interval` months on `day`, or on the month's last day when the
    /// month is shorter (`*1M` created on the 30th fires on Feb 28/29).
    /// `BYMONTHDAY=28..=day;BYSETPOS=-1` picks the latest of those days that
    /// exists in each month. Days up to 28 exist in every month and use the
    /// strict rule. Built as a canonical string like [`Self::monthly_last_day`].
    pub fn monthly_every_or_last_day(
        dtstart: DateTime<Utc>,
        tz: Tz,
        interval: u16,
        day: u8,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        if day <= 28 {
            return Self::monthly_every(dtstart, tz, interval, day, time);
        }
        let days: Vec<String> = (28..=day).map(|d| d.to_string()).collect();
        let rrule_str = format!(
            "FREQ=MONTHLY;INTERVAL={interval};BYMONTHDAY={};BYSETPOS=-1;BYHOUR={};BYMINUTE={}",
            days.join(","),
            time.hour(),
            time.minute()
        );
        Self::from_canonical_string(dtstart, tz, rrule_str)
    }

    /// Monthly on the last day of every month (`BYMONTHDAY=-1`). This is
    /// what `*31.` resolves to per RULES.md — in February it lands on the
    /// 28th/29th, otherwise the 30th/31st.
    ///
    /// rrule 0.14's `Display` impl drops negative-BYMONTHDAY values during
    /// serialization (it only emits the positive `by_month_day` field, not the
    /// `by_n_month_day` sibling that validation moves them into). So we build
    /// the canonical string ourselves and validate it by re-parsing.
    pub fn monthly_last_day(
        dtstart: DateTime<Utc>,
        tz: Tz,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        let rrule_str = format!(
            "FREQ=MONTHLY;BYMONTHDAY=-1;BYHOUR={};BYMINUTE={}",
            time.hour(),
            time.minute()
        );
        Self::from_canonical_string(dtstart, tz, rrule_str)
    }

    /// Yearly on a fixed month + day-of-month at fixed wall-clock time.
    /// Strict: non-existent dates (e.g. `Feb 29` in non-leap years) are
    /// skipped. For `*29.2` semantics — last day of February every year —
    /// use [`Self::yearly_last_day_of_month`].
    pub fn yearly(
        dtstart: DateTime<Utc>,
        tz: Tz,
        month: u8,
        day: u8,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        Self::yearly_every(dtstart, tz, 1, month, day, time)
    }

    /// Every `interval` years on a fixed month + day (`*2Y` → INTERVAL=2).
    /// Same strict date semantics as [`Self::yearly`].
    pub fn yearly_every(
        dtstart: DateTime<Utc>,
        tz: Tz,
        interval: u16,
        month: u8,
        day: u8,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        let chrono_month = chrono::Month::try_from(month)
            .map_err(|_| ScheduleError::Invalid(format!("invalid month {month}")))?;
        let rule = RRule::new(Frequency::Yearly)
            .interval(interval)
            .by_month(&[chrono_month])
            .by_month_day(vec![day as i8])
            .by_hour(vec![time.hour() as u8])
            .by_minute(vec![time.minute() as u8]);
        Self::recurring(dtstart, tz, rule)
    }

    /// Yearly on the last day of a specific month (`BYMONTH=M;BYMONTHDAY=-1`).
    /// Used for `*29.2` — every year on Feb 28/29. Same upstream-string-bug
    /// workaround as [`Self::monthly_last_day`].
    pub fn yearly_last_day_of_month(
        dtstart: DateTime<Utc>,
        tz: Tz,
        month: u8,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        if !(1..=12).contains(&month) {
            return Err(ScheduleError::Invalid(format!("invalid month {month}")));
        }
        let rrule_str = format!(
            "FREQ=YEARLY;BYMONTH={month};BYMONTHDAY=-1;BYHOUR={};BYMINUTE={}",
            time.hour(),
            time.minute()
        );
        Self::from_canonical_string(dtstart, tz, rrule_str)
    }

    /// Every `interval` years on the last day of `month`. Used for `*1Y`
    /// created on Feb 29, which fires on Feb 28 in non-leap years.
    pub fn yearly_every_last_day_of_month(
        dtstart: DateTime<Utc>,
        tz: Tz,
        interval: u16,
        month: u8,
        time: NaiveTime,
    ) -> Result<Self, ScheduleError> {
        if !(1..=12).contains(&month) {
            return Err(ScheduleError::Invalid(format!("invalid month {month}")));
        }
        let rrule_str = format!(
            "FREQ=YEARLY;INTERVAL={interval};BYMONTH={month};BYMONTHDAY=-1;BYHOUR={};BYMINUTE={}",
            time.hour(),
            time.minute()
        );
        Self::from_canonical_string(dtstart, tz, rrule_str)
    }

    /// Validate a hand-built RRULE string by parsing + validating it, then
    /// keep the original string verbatim (since rrule 0.14's `Display` is lossy
    /// for `by_n_month_day`). Returns Err if parsing or validation fails.
    fn from_canonical_string(
        dtstart: DateTime<Utc>,
        tz: Tz,
        rrule_str: String,
    ) -> Result<Self, ScheduleError> {
        let rule: RRule<Unvalidated> = rrule_str
            .parse()
            .map_err(|e| ScheduleError::Invalid(format!("parse {rrule_str:?}: {e}")))?;
        let dts_rrule = dtstart.with_timezone(&rrule::Tz::from(tz));
        rule.validate(dts_rrule)?;
        Ok(Self {
            dtstart,
            tz,
            rrule: Some(rrule_str),
        })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn berlin() -> Tz {
        "Europe/Berlin".parse().unwrap()
    }
    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0).unwrap()
    }
    fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        berlin()
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
            .with_timezone(&Utc)
    }
    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    // ---- One-shot ----

    #[test]
    fn one_shot_future_returns_dtstart() {
        let s = Schedule::one_shot(utc(2026, 5, 9, 14, 0), berlin());
        assert_eq!(s.next_after(utc(2026, 5, 9, 13, 0)), Some(utc(2026, 5, 9, 14, 0)));
    }

    #[test]
    fn one_shot_past_returns_none() {
        let s = Schedule::one_shot(utc(2026, 5, 9, 14, 0), berlin());
        assert_eq!(s.next_after(utc(2026, 5, 9, 15, 0)), None);
    }

    // ---- Interval ----

    #[test]
    fn interval_30m_next_is_dtstart_plus_30() {
        let dts = utc(2026, 5, 9, 10, 0);
        let s = Schedule::interval_seconds(dts, berlin(), 1800).unwrap();
        // strictly after dts → next slot is dts + 30m
        assert_eq!(s.next_after(dts), Some(utc(2026, 5, 9, 10, 30)));
    }

    #[test]
    fn interval_below_minimum_rejected() {
        let dts = utc(2026, 5, 9, 10, 0);
        let err = Schedule::interval_seconds(dts, berlin(), 60).unwrap_err();
        assert!(matches!(err, ScheduleError::Invalid(_)));
    }

    #[test]
    fn interval_above_u16_minutes_rejected_not_wrapped() {
        // 46d30m = 3_976_200s → 66_270 min > u16::MAX (65_535). Must be a clean
        // error, not a silent wrap to 734 min.
        let dts = utc(2026, 5, 9, 10, 0);
        let err = Schedule::interval_seconds(dts, berlin(), 3_976_200).unwrap_err();
        assert!(matches!(err, ScheduleError::IntervalTooLong(3_976_200)));
    }

    // ---- Daily ----

    #[test]
    fn daily_picks_today_if_time_still_ahead() {
        let dts = local(2026, 5, 9, 9, 0);
        let s = Schedule::daily_at(dts, berlin(), 1, t(9, 0)).unwrap();
        // Asking at 08:00 today → today 09:00 (which equals dtstart)
        assert_eq!(s.next_after(local(2026, 5, 9, 8, 0)), Some(local(2026, 5, 9, 9, 0)));
    }

    #[test]
    fn daily_picks_tomorrow_if_time_passed() {
        let dts = local(2026, 5, 9, 9, 0);
        let s = Schedule::daily_at(dts, berlin(), 1, t(9, 0)).unwrap();
        // Asking at 10:00 → tomorrow 09:00
        assert_eq!(
            s.next_after(local(2026, 5, 9, 10, 0)),
            Some(local(2026, 5, 10, 9, 0))
        );
    }

    #[test]
    fn every_2_days_at_11() {
        let dts = local(2026, 5, 9, 11, 0);
        let s = Schedule::daily_at(dts, berlin(), 2, t(11, 0)).unwrap();
        // Right after dtstart → +2 days
        assert_eq!(
            s.next_after(local(2026, 5, 9, 11, 1)),
            Some(local(2026, 5, 11, 11, 0))
        );
    }

    // ---- Weekly ----

    #[test]
    fn weekly_mon_wed_fri() {
        // Anchor on a Friday (2026-05-08); next "after Fri 10:00" with rule
        // Mon/Wed/Fri 09:00 is next Monday (2026-05-11).
        let dts = local(2026, 5, 8, 9, 0);
        let s = Schedule::weekly(
            dts,
            berlin(),
            &[Weekday::Mon, Weekday::Wed, Weekday::Fri],
            t(9, 0),
        )
        .unwrap();
        assert_eq!(
            s.next_after(local(2026, 5, 8, 10, 0)),
            Some(local(2026, 5, 11, 9, 0))
        );
    }

    // ---- Monthly last-day ----

    #[test]
    fn monthly_last_day_skips_short_months_correctly() {
        // Anchor on Jan 31. Next fire after Jan 31 should be Feb 28 (2026 not leap).
        let dts = local(2026, 1, 31, 9, 0);
        let s = Schedule::monthly_last_day(dts, berlin(), t(9, 0)).unwrap();
        assert!(s.rrule.as_deref().unwrap().contains("BYMONTHDAY=-1"));
        assert_eq!(
            s.next_after(local(2026, 1, 31, 10, 0)),
            Some(local(2026, 2, 28, 9, 0))
        );
    }

    #[test]
    fn monthly_from_day_30_falls_back_to_the_last_day() {
        let dts = local(2027, 2, 28, 9, 0);
        let s = Schedule::monthly_every_or_last_day(dts, berlin(), 1, 30, t(9, 0)).unwrap();
        let mut fires = Vec::new();
        let mut after = local(2027, 2, 28, 10, 0);
        for _ in 0..3 {
            after = s.next_after(after).unwrap();
            fires.push(after);
        }
        assert_eq!(
            fires,
            [
                local(2027, 3, 30, 9, 0),
                local(2027, 4, 30, 9, 0),
                local(2027, 5, 30, 9, 0)
            ]
        );
        assert_eq!(
            s.next_after(local(2028, 1, 30, 10, 0)),
            Some(local(2028, 2, 29, 9, 0))
        );
    }

    #[test]
    fn monthly_from_day_31_fires_in_every_interval() {
        let dts = local(2027, 4, 30, 9, 0);
        let s = Schedule::monthly_every_or_last_day(dts, berlin(), 3, 31, t(9, 0)).unwrap();
        assert_eq!(
            s.next_after(local(2027, 4, 30, 10, 0)),
            Some(local(2027, 7, 31, 9, 0))
        );
        assert_eq!(
            s.next_after(local(2027, 7, 31, 10, 0)),
            Some(local(2027, 10, 31, 9, 0))
        );
    }

    #[test]
    fn monthly_up_to_day_28_stays_strict() {
        let dts = local(2027, 2, 28, 9, 0);
        let s = Schedule::monthly_every_or_last_day(dts, berlin(), 1, 28, t(9, 0)).unwrap();
        assert!(!s.rrule.as_deref().unwrap().contains("BYSETPOS"));
    }

    #[test]
    fn yearly_every_last_day_of_feb_fires_every_year() {
        let dts = local(2029, 2, 28, 9, 0);
        let s = Schedule::yearly_every_last_day_of_month(dts, berlin(), 1, 2, t(9, 0)).unwrap();
        assert_eq!(
            s.next_after(local(2029, 2, 28, 10, 0)),
            Some(local(2030, 2, 28, 9, 0))
        );
        assert_eq!(
            s.next_after(local(2031, 2, 28, 10, 0)),
            Some(local(2032, 2, 29, 9, 0))
        );
    }

    #[test]
    fn yearly_last_day_of_feb_uses_feb_29_in_leap() {
        // Anchor in 2027 (non-leap) → Feb 28; advance to 2028 (leap) → Feb 29.
        let dts = local(2027, 2, 28, 9, 0);
        let s = Schedule::yearly_last_day_of_month(dts, berlin(), 2, t(9, 0)).unwrap();
        assert!(s.rrule.as_deref().unwrap().contains("BYMONTH=2"));
        assert!(s.rrule.as_deref().unwrap().contains("BYMONTHDAY=-1"));
        assert_eq!(
            s.next_after(local(2027, 2, 28, 10, 0)),
            Some(local(2028, 2, 29, 9, 0))
        );
    }

    // ---- Yearly ----

    #[test]
    fn yearly_christmas() {
        let dts = local(2026, 12, 24, 9, 0);
        let s = Schedule::yearly(dts, berlin(), 12, 24, t(9, 0)).unwrap();
        // Right after this year's → next year's
        assert_eq!(
            s.next_after(local(2026, 12, 24, 10, 0)),
            Some(local(2027, 12, 24, 9, 0))
        );
    }

    #[test]
    fn yearly_every_two_years_emits_interval() {
        let s = Schedule::yearly_every(local(2026, 5, 8, 9, 0), berlin(), 2, 5, 8, t(9, 0)).unwrap();
        let rule = s.rrule.as_deref().unwrap();
        assert!(rule.contains("FREQ=YEARLY"));
        assert!(rule.contains("INTERVAL=2"));
        // 2026 → 2028, not 2027
        assert_eq!(
            s.next_after(local(2026, 5, 8, 10, 0)),
            Some(local(2028, 5, 8, 9, 0))
        );
    }

    #[test]
    fn monthly_every_three_months_emits_interval() {
        let s = Schedule::monthly_every(local(2026, 5, 8, 9, 0), berlin(), 3, 8, t(9, 0)).unwrap();
        let rule = s.rrule.as_deref().unwrap();
        assert!(rule.contains("FREQ=MONTHLY"));
        assert!(rule.contains("INTERVAL=3"));
        // May 8 → Aug 8, skipping Jun/Jul
        assert_eq!(
            s.next_after(local(2026, 5, 8, 10, 0)),
            Some(local(2026, 8, 8, 9, 0))
        );
    }

    // ---- RRULE round-trip via string ----

    #[test]
    fn rrule_string_is_valid_rfc5545_subset() {
        let s = Schedule::weekly(
            local(2026, 5, 8, 9, 0),
            berlin(),
            &[Weekday::Mon, Weekday::Wed, Weekday::Fri],
            t(9, 0),
        )
        .unwrap();
        let rule_str = s.rrule.as_deref().unwrap();
        assert!(rule_str.starts_with("FREQ=WEEKLY"));
        assert!(rule_str.contains("BYDAY=MO,WE,FR"));
        assert!(rule_str.contains("BYHOUR=9"));
        assert!(rule_str.contains("BYMINUTE=0"));
    }

    #[test]
    fn schedule_from_db_round_trips() {
        let original = Schedule::daily_at(local(2026, 5, 9, 9, 0), berlin(), 2, t(11, 0)).unwrap();
        let restored = Schedule::from_db(
            original.dtstart,
            original.tz,
            original.rrule.as_deref(),
        )
        .unwrap();
        // Both should give the same next_after.
        let probe = local(2026, 5, 9, 11, 1);
        assert_eq!(original.next_after(probe), restored.next_after(probe));
    }
}
