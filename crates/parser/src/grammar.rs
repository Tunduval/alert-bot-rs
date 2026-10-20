//! Grammar implementations.
//!
//! `parse_command` is the dispatch point — strip a recurring prefix, then hand
//! off to one of the two top-level strategies. One-shot parsing tries four
//! disambiguated strategies in order: relative → absolute date → bare clock →
//! named day. Recurring reuses the relative scanner and adds weekday /
//! day-of-month / month-day strategies.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Timelike, Utc, Weekday};

use crate::keywords::{
    compact_unit, is_at_prefix, is_in_prefix, is_recurring_keyword, is_uhr, match_longform_unit,
    match_named_day, NamedDay, TimeUnit,
};
use crate::{
    local_to_utc, ParseContext, ParseError, ParseNote, Parsed, Schedule, DEFAULT_TIME,
    MAX_RELATIVE_YEARS, MIN_INTERVAL_SECONDS,
};

/// (text, byte-offset where the token starts in the original input).
type Token<'a> = (&'a str, usize);

fn tokenize(input: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (idx, ch) in input.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((&input[s..idx], s));
            }
        } else if start.is_none() {
            start = Some(idx);
        }
    }
    if let Some(s) = start {
        out.push((&input[s..], s));
    }
    out
}

fn token_end(tok: Token<'_>) -> usize {
    tok.1 + tok.0.len()
}

pub(crate) fn parse_command(input: &str, ctx: &ParseContext) -> Result<Parsed, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }

    let (recurring, rest) = strip_recurring_prefix(trimmed);
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Err(ParseError::Empty);
    }

    if recurring {
        parse_recurring(rest, ctx)
    } else {
        parse_one_shot(rest, ctx)
    }
}

fn strip_recurring_prefix(input: &str) -> (bool, &str) {
    if let Some(rest) = input.strip_prefix('*') {
        return (true, rest);
    }
    let (first, rest_with_space) = match input.split_once(char::is_whitespace) {
        Some((f, r)) => (f, r),
        None => (input, ""),
    };
    if is_recurring_keyword(first) {
        return (true, rest_with_space);
    }
    (false, input)
}

// ===========================================================================
// One-shot
// ===========================================================================

fn parse_one_shot(input: &str, ctx: &ParseContext) -> Result<Parsed, ParseError> {
    let tokens = tokenize(input);
    if tokens.is_empty() {
        return Err(ParseError::Empty);
    }

    // `heute`/`today` is intentionally not a valid spec — point user at
    // bare-clock-time. Done before other strategies so it can't be
    // accidentally swallowed by a fuzzy match.
    if let Some(first) = tokens.first() {
        if matches!(match_named_day(first.0), Some(NamedDay::Today)) {
            return Err(ParseError::HeuteRejected);
        }
    }

    let (fire_at, consumed_until, notes) = try_one_shot_strategies(&tokens, ctx)?;

    let text = input[consumed_until..].trim().to_string();
    if text.is_empty() {
        return Err(ParseError::MissingText);
    }
    if fire_at <= ctx.now_utc {
        return Err(ParseError::InPast);
    }

    Ok(Parsed {
        schedule: Schedule::one_shot(fire_at, ctx.tz),
        text,
        notes,
    })
}

fn try_one_shot_strategies(
    tokens: &[Token<'_>],
    ctx: &ParseContext,
) -> Result<(DateTime<Utc>, usize, Vec<ParseNote>), ParseError> {
    // 1. Relative (compact, inline-longform, or two-token longform).
    if let Some((spec, end_idx, default_end)) = try_relative_match(tokens)? {
        let (override_time, end_offset) =
            consume_time_after_spec(tokens, end_idx, &spec).unwrap_or((None, default_end));
        let (fire_at, notes) = spec.apply(ctx.now_utc, ctx.tz, override_time)?;
        return Ok((fire_at, end_offset, notes));
    }

    // 2. Absolute date.
    if let Some(first) = tokens.first().copied() {
        let (override_time, end_offset) =
            consume_optional_time(tokens, 1).unwrap_or((None, token_end(first)));
        let time = override_time.unwrap_or(DEFAULT_TIME);
        if let Some(date) = parse_date_token(first.0, time, ctx) {
            let fire_at = local_to_utc(date, time, ctx.tz)?;
            return Ok((fire_at, end_offset, Vec::new()));
        }
    }

    // 3. Bare clock-time → next occurrence (today or tomorrow).
    if let Some((time, end_offset)) = try_bare_clock(tokens) {
        let fire_at = resolve_bare_clock(time, ctx)?;
        return Ok((fire_at, end_offset, Vec::new()));
    }

    // 4. Named day (morgen / übermorgen / weekday).
    if let Some((day, override_time, end_offset)) = try_named_day(tokens) {
        let time = override_time.unwrap_or(DEFAULT_TIME);
        let date = resolve_named_day(day, time, ctx);
        let fire_at = local_to_utc(date, time, ctx.tz)?;
        return Ok((fire_at, end_offset, Vec::new()));
    }

    Err(ParseError::NoTimeExpression)
}

// ---------------------------------------------------------------------------
// Relative: combined `RelSpec` covering both compact (`1Y2M15d8h40m20s`) and
// longform single-component (`30 minuten`, `5min`).
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
struct RelSpec {
    years: u32,
    months: u32,
    weeks: u32,
    days: u32,
    hours: u32,
    minutes: u32,
    seconds: u32,
}

impl RelSpec {
    fn has_date_part(&self) -> bool {
        self.years > 0 || self.months > 0 || self.weeks > 0 || self.days > 0
    }
    fn has_sub_day(&self) -> bool {
        self.hours > 0 || self.minutes > 0 || self.seconds > 0
    }
    fn is_zero(&self) -> bool {
        !self.has_date_part() && !self.has_sub_day()
    }
    fn set(&mut self, unit: TimeUnit, n: u32) {
        match unit {
            TimeUnit::Year => self.years = n,
            TimeUnit::Month => self.months = n,
            TimeUnit::Week => self.weeks = n,
            TimeUnit::Day => self.days = n,
            TimeUnit::Hour => self.hours = n,
            TimeUnit::Minute => self.minutes = n,
            TimeUnit::Second => self.seconds = n,
        }
    }

    /// Parse compact spec: `1Y2M15d8h40m20s`, `7h30m`, `2d12h`, `5m`, `5M`.
    ///
    /// Three outcomes:
    /// - `Ok(Some(spec))` — clean parse.
    /// - `Ok(None)` — doesn't look like compact at all (e.g. `5x`, `5`).
    /// - `Err(InvalidRelSpec)` — looked like compact (consumed at least one
    ///   valid pair) but is out-of-order or incomplete (e.g. `30m2h`, `1Y2`).
    fn parse_compact(s: &str) -> Result<Option<Self>, ParseError> {
        if s.is_empty() {
            return Ok(None);
        }
        let bytes = s.as_bytes();
        let mut spec = Self::default();
        let mut i = 0;
        let mut last_rank: Option<u8> = None;
        let mut consumed_any = false;
        while i < bytes.len() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == start {
                return if consumed_any {
                    Err(ParseError::InvalidRelSpec)
                } else {
                    Ok(None)
                };
            }
            let num: u32 = match std::str::from_utf8(&bytes[start..i])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Ok(None),
            };
            if i >= bytes.len() {
                return if consumed_any {
                    Err(ParseError::InvalidRelSpec)
                } else {
                    Ok(None)
                };
            }
            let suffix = bytes[i] as char;
            let unit = match compact_unit(suffix) {
                Some(u) => u,
                None => {
                    return if consumed_any {
                        Err(ParseError::InvalidRelSpec)
                    } else {
                        Ok(None)
                    };
                }
            };
            i += 1;
            let rank = unit_rank(unit);
            if let Some(last) = last_rank {
                if rank <= last {
                    return Err(ParseError::InvalidRelSpec);
                }
            }
            last_rank = Some(rank);
            spec.set(unit, num);
            consumed_any = true;
        }
        if spec.is_zero() {
            return Ok(None);
        }
        Ok(Some(spec))
    }

    /// Apply this spec to a base time. With `override_time`, the spec's date
    /// part is computed and the resulting date is combined with the override
    /// (sub-day in spec is rejected). Without an override:
    ///   - pure d/w/h/m/s → raw seconds added to base (DST-naive, exact arithmetic)
    ///   - involves Y or M → calendar-aware date math, wall-clock preserved on
    ///     the resulting date, then any extra w/d/h/m/s added as raw seconds.
    fn apply(
        self,
        base_utc: DateTime<Utc>,
        tz: chrono_tz::Tz,
        override_time: Option<NaiveTime>,
    ) -> Result<(DateTime<Utc>, Vec<ParseNote>), ParseError> {
        let mut notes = Vec::new();
        if self.is_zero() {
            return Err(ParseError::NoTimeExpression);
        }
        if override_time.is_some() && self.has_sub_day() {
            return Err(ParseError::SubDayRelWithOverride);
        }
        // Pre-check the obvious case (`100Y`) before doing any calendar math.
        // A post-check below still catches weird combos like `100000d`.
        if self.years > MAX_RELATIVE_YEARS as u32 {
            return Err(ParseError::RelTooFar(MAX_RELATIVE_YEARS));
        }

        if override_time.is_none() && self.years == 0 && self.months == 0 {
            let secs = (self.weeks as i64 * 7 + self.days as i64) * 86_400
                + self.hours as i64 * 3600
                + self.minutes as i64 * 60
                + self.seconds as i64;
            let offset = Duration::seconds(secs);
            if offset > Duration::days(MAX_RELATIVE_YEARS as i64 * 366) {
                return Err(ParseError::RelTooFar(MAX_RELATIVE_YEARS));
            }
            let result = base_utc
                .checked_add_signed(offset)
                .ok_or(ParseError::RelTooFar(MAX_RELATIVE_YEARS))?;
            return Ok((result, notes));
        }

        let local_base = base_utc.with_timezone(&tz);
        let base_time = local_base.time();
        let mut target_date = local_base.date_naive();

        if self.years > 0 {
            let new_year = target_date
                .year()
                .checked_add(self.years as i32)
                .ok_or_else(|| ParseError::InvalidDate(format!("year overflow on {target_date}")))?;
            target_date = match target_date.with_year(new_year) {
                Some(d) => d,
                None => {
                    notes.push(ParseNote::LeapClampedToFeb28 { new_year });
                    NaiveDate::from_ymd_opt(new_year, target_date.month(), 28).ok_or_else(|| {
                        ParseError::InvalidDate(format!(
                            "Y add fallback failed for {target_date} -> {new_year}"
                        ))
                    })?
                }
            };
        }
        if self.months > 0 {
            let before = target_date;
            target_date = target_date
                .checked_add_months(chrono::Months::new(self.months))
                .ok_or_else(|| {
                    ParseError::InvalidDate(format!("M add: {target_date} + {}M", self.months))
                })?;
            if before.day() > target_date.day() {
                notes.push(ParseNote::MonthDayClamped {
                    before_day: before.day(),
                    after_day: target_date.day(),
                });
            }
        }
        if self.weeks > 0 {
            target_date = target_date
                .checked_add_signed(Duration::days(self.weeks as i64 * 7))
                .ok_or(ParseError::RelTooFar(MAX_RELATIVE_YEARS))?;
        }
        if self.days > 0 {
            target_date = target_date
                .checked_add_signed(Duration::days(self.days as i64))
                .ok_or(ParseError::RelTooFar(MAX_RELATIVE_YEARS))?;
        }

        let time = override_time.unwrap_or(base_time);
        let mut result = local_to_utc(target_date, time, tz)?;

        if override_time.is_none() && self.has_sub_day() {
            let secs = self.hours as i64 * 3600
                + self.minutes as i64 * 60
                + self.seconds as i64;
            result = result
                .checked_add_signed(Duration::seconds(secs))
                .ok_or(ParseError::RelTooFar(MAX_RELATIVE_YEARS))?;
        }
        // Post-check: catches huge non-Y specs like `100000d` (~274 years).
        if result - base_utc > Duration::days(MAX_RELATIVE_YEARS as i64 * 366) {
            return Err(ParseError::RelTooFar(MAX_RELATIVE_YEARS));
        }
        Ok((result, notes))
    }
}

fn unit_rank(unit: TimeUnit) -> u8 {
    match unit {
        TimeUnit::Year => 0,
        TimeUnit::Month => 1,
        TimeUnit::Week => 2,
        TimeUnit::Day => 3,
        TimeUnit::Hour => 4,
        TimeUnit::Minute => 5,
        TimeUnit::Second => 6,
    }
}

/// Match a relative spec at the head of the token stream.
///
/// Returns `Ok(Some(spec, next_idx, end_offset))` on a clean match,
/// `Ok(None)` if the head doesn't look relative at all (so the caller can try
/// the next strategy), and `Err(InvalidRelSpec)` when the head clearly *meant*
/// to be a compact spec but is malformed (`30m2h`, `1Y2`).
fn try_relative_match(
    tokens: &[Token<'_>],
) -> Result<Option<(RelSpec, usize, usize)>, ParseError> {
    let mut i = 0;
    if tokens.get(i).is_some_and(|t| is_in_prefix(t.0)) {
        i += 1;
    }
    let Some(first) = tokens.get(i).copied() else {
        return Ok(None);
    };

    if let Some(spec) = parse_rel_single_token(first.0)? {
        return Ok(Some((spec, i + 1, token_end(first))));
    }

    // Two-token form: bare-number + longform unit (`30 minuten`).
    let Some((num, suffix)) = split_leading_digits(first.0) else {
        return Ok(None);
    };
    if !suffix.is_empty() {
        return Ok(None);
    }
    let Some(next) = tokens.get(i + 1).copied() else {
        return Ok(None);
    };
    let Some(unit) = match_longform_unit(next.0) else {
        return Ok(None);
    };
    let mut spec = RelSpec::default();
    let n: u32 = num.try_into().map_err(|_| ParseError::InvalidRelSpec)?;
    spec.set(unit, n);
    Ok(Some((spec, i + 2, token_end(next))))
}

fn parse_rel_single_token(s: &str) -> Result<Option<RelSpec>, ParseError> {
    if let Some(spec) = RelSpec::parse_compact(s)? {
        return Ok(Some(spec));
    }
    // Inline longform: leading digits then a longform unit alias glued on.
    let Some(split) = s.find(|c: char| !c.is_ascii_digit()) else {
        return Ok(None);
    };
    if split == 0 {
        return Ok(None);
    }
    let num: u32 = match s[..split].parse() {
        Ok(n) => n,
        Err(_) => return Ok(None),
    };
    let Some(unit) = match_longform_unit(&s[split..]) else {
        return Ok(None);
    };
    let mut spec = RelSpec::default();
    spec.set(unit, num);
    Ok(Some(spec))
}

/// `"5m"` → `(5, "m")`, `"42"` → `(42, "")`, `"abc"` → `None`.
fn split_leading_digits(s: &str) -> Option<(i64, &str)> {
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if split == 0 {
        return None;
    }
    let num: i64 = s[..split].parse().ok()?;
    Some((num, &s[split..]))
}

// ---------------------------------------------------------------------------
// Absolute date: DD.MM[.YY|.YYYY|.] and ISO YYYY-MM-DD.
// ---------------------------------------------------------------------------

fn parse_date_token(s: &str, time: NaiveTime, ctx: &ParseContext) -> Option<NaiveDate> {
    if let Some(d) = parse_iso_date(s) {
        return Some(d);
    }
    parse_dotted_date(s, time, ctx)
}

fn parse_iso_date(s: &str) -> Option<NaiveDate> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    if !(1000..=9999).contains(&y) {
        return None;
    }
    NaiveDate::from_ymd_opt(y, m, d)
}

fn parse_dotted_date(s: &str, time: NaiveTime, ctx: &ParseContext) -> Option<NaiveDate> {
    let s = s.strip_suffix('.').unwrap_or(s);
    let parts: Vec<&str> = s.split('.').collect();
    if !(parts.len() == 2 || parts.len() == 3) {
        return None;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let day: u32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    if parts.len() == 3 {
        let y: i32 = parts[2].parse().ok()?;
        let year = if y < 100 { 2000 + y } else { y };
        return NaiveDate::from_ymd_opt(year, month, day);
    }
    // A year-less date is its next occurrence. Feb 29 can be up to eight
    // years away (2096 to 2104), so search that far. Any later year is
    // already in the future; only this year's date must be checked.
    let current = ctx.now_utc.with_timezone(&ctx.tz).year();
    (current..=current + 8).find_map(|year| {
        NaiveDate::from_ymd_opt(year, month, day).filter(|d| {
            year > current || local_to_utc(*d, time, ctx.tz).is_ok_and(|at| at > ctx.now_utc)
        })
    })
}

// ---------------------------------------------------------------------------
// Bare clock-time + clock parser shared with override-consumption.
// ---------------------------------------------------------------------------

fn try_bare_clock(tokens: &[Token<'_>]) -> Option<(NaiveTime, usize)> {
    let (time, end) = consume_optional_time(tokens, 0)?;
    time.map(|t| (t, end))
}

fn resolve_bare_clock(
    time: NaiveTime,
    ctx: &ParseContext,
) -> Result<DateTime<Utc>, ParseError> {
    let today = ctx.now_utc.with_timezone(&ctx.tz).date_naive();
    let cand = local_to_utc(today, time, ctx.tz)?;
    if cand > ctx.now_utc {
        return Ok(cand);
    }
    let tomorrow = today + Duration::days(1);
    local_to_utc(tomorrow, time, ctx.tz)
}

fn consume_optional_time(
    tokens: &[Token<'_>],
    from: usize,
) -> Option<(Option<NaiveTime>, usize)> {
    if from >= tokens.len() {
        return None;
    }
    let mut i = from;
    if tokens.get(i).is_some_and(|t| is_at_prefix(t.0)) {
        i += 1;
    }
    let clock_tok = *tokens.get(i)?;
    let time = parse_clock(clock_tok.0)?;
    let mut end_offset = token_end(clock_tok);
    i += 1;
    if let Some(uhr) = tokens.get(i) {
        if is_uhr(uhr.0) {
            end_offset = token_end(*uhr);
        }
    }
    Some((Some(time), end_offset))
}

/// Like `consume_optional_time`, but after a sub-day spec a bare number
/// (`30m 2 Eier`) is reminder text, not an override. Explicit clocks (`11:00`,
/// `9 Uhr`, `2pm`) are still consumed so that they get rejected.
fn consume_time_after_spec(
    tokens: &[Token<'_>],
    from: usize,
    spec: &RelSpec,
) -> Option<(Option<NaiveTime>, usize)> {
    let found = consume_optional_time(tokens, from)?;
    let bare_number = tokens
        .get(from)
        .is_some_and(|t| t.0.bytes().all(|b| b.is_ascii_digit()) && token_end(*t) == found.1);
    if spec.has_sub_day() && bare_number {
        return None;
    }
    Some(found)
}

/// Accepts `14:30`, `14`, `2pm`, `2:30pm`, `12am`, `12pm`. Case-insensitive
/// for the am/pm suffix — but we avoid the `to_lowercase()` allocation by
/// matching on ASCII bytes directly, since the digit-only path is by far the
/// hottest one on the parse fast path.
fn parse_clock(s: &str) -> Option<NaiveTime> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let (body, ampm): (&str, Option<bool>) = if n >= 2
        && matches!(bytes[n - 1], b'm' | b'M')
        && matches!(bytes[n - 2], b'a' | b'A' | b'p' | b'P')
    {
        let pm = matches!(bytes[n - 2], b'p' | b'P');
        (&s[..n - 2], Some(pm))
    } else {
        (s, None)
    };
    if body.is_empty() {
        return None;
    }
    let (h, m): (u32, u32) = if let Some((hh, mm)) = body.split_once(':') {
        (hh.parse().ok()?, mm.parse().ok()?)
    } else {
        if !body.chars().all(|c| c.is_ascii_digit()) || body.len() > 2 {
            return None;
        }
        (body.parse().ok()?, 0)
    };
    let h = match ampm {
        Some(false) /* AM */ => match h {
            12 => 0,
            0..=11 => h,
            _ => return None,
        },
        Some(true) /* PM */ => match h {
            12 => 12,
            0..=11 => h + 12,
            _ => return None,
        },
        None => h,
    };
    if h > 23 || m > 59 {
        return None;
    }
    NaiveTime::from_hms_opt(h, m, 0)
}

// ---------------------------------------------------------------------------
// Named day
// ---------------------------------------------------------------------------

fn try_named_day(
    tokens: &[Token<'_>],
) -> Option<(NamedDay, Option<NaiveTime>, usize)> {
    let first = *tokens.first()?;
    let day = match_named_day(first.0)?;
    if matches!(day, NamedDay::Today) {
        // Handled upstream — defensive guard.
        return None;
    }
    let (time, end_offset) =
        consume_optional_time(tokens, 1).unwrap_or((None, token_end(first)));
    Some((day, time, end_offset))
}

fn resolve_named_day(day: NamedDay, time: NaiveTime, ctx: &ParseContext) -> NaiveDate {
    let today = ctx.now_utc.with_timezone(&ctx.tz).date_naive();
    match day {
        NamedDay::Today => today, // unreachable in normal flow
        NamedDay::Tomorrow => today + Duration::days(1),
        NamedDay::DayAfterTomorrow => today + Duration::days(2),
        NamedDay::Weekday(target) => first_future_weekday(today, target, time, ctx),
    }
}

fn first_future_weekday(
    today: NaiveDate,
    target: Weekday,
    time: NaiveTime,
    ctx: &ParseContext,
) -> NaiveDate {
    let days_ahead =
        (target.num_days_from_monday() + 7 - today.weekday().num_days_from_monday()) % 7;
    let first = today + Duration::days(days_ahead as i64);
    if local_to_utc(first, time, ctx.tz).is_ok_and(|at| at > ctx.now_utc) {
        first
    } else {
        first + Duration::days(7)
    }
}

// ===========================================================================
// Recurring
// ===========================================================================

fn parse_recurring(input: &str, ctx: &ParseContext) -> Result<Parsed, ParseError> {
    let tokens = tokenize(input);
    if tokens.is_empty() {
        return Err(ParseError::Empty);
    }

    let matched = try_recurring_relative(&tokens, ctx)?
        .or_else_result(|| try_recurring_weekday(&tokens, ctx))?
        .or_else_result(|| try_recurring_day_of_month(&tokens, ctx))?
        .or_else_result(|| try_recurring_month_day(&tokens, ctx))?;

    let (schedule, end_offset, note) = matched.ok_or(ParseError::NoTimeExpression)?;

    let text = input[end_offset..].trim().to_string();
    if text.is_empty() {
        return Err(ParseError::MissingText);
    }

    Ok(Parsed {
        schedule,
        text,
        notes: note.into_iter().collect(),
    })
}

/// Helper to chain fallible `Option`-returning strategies without nested `if let` chains.
trait OptionOrElseResult<T, E> {
    fn or_else_result(self, f: impl FnOnce() -> Result<Option<T>, E>) -> Result<Option<T>, E>;
}

impl<T, E> OptionOrElseResult<T, E> for Option<T> {
    fn or_else_result(self, f: impl FnOnce() -> Result<Option<T>, E>) -> Result<Option<T>, E> {
        match self {
            Some(v) => Ok(Some(v)),
            None => f(),
        }
    }
}

/// Uniform return type for the four recurring strategies. The optional note
/// carries an edge-case flag (e.g. `*31.` → `MonthlyLastDay`) so the dispatcher
/// can hoist it into `Parsed.notes` without each strategy duplicating the push.
type RecurringMatch = (Schedule, usize, Option<ParseNote>);

fn try_recurring_relative(
    tokens: &[Token<'_>],
    ctx: &ParseContext,
) -> Result<Option<RecurringMatch>, ParseError> {
    // Recurring REL doesn't accept the `in` prefix (`*in 5m` is unidiomatic),
    // so reject upfront if it's there.
    if tokens.first().is_some_and(|t| is_in_prefix(t.0)) {
        return Ok(None);
    }
    let Some((spec, end_idx, spec_end_offset)) = try_relative_match(tokens)? else {
        return Ok(None);
    };

    // An optional clock override may follow the relative spec (`*2d 11:00`,
    // `*3M 18:00`, `*1Y 9:00`, `alle 2d 11:00`). Consume it here so the time
    // token doesn't leak into the reminder text, and so the schedule fires at
    // the requested wall-clock time instead of the creation time.
    let (override_time, end_offset) = match consume_time_after_spec(tokens, end_idx, &spec) {
        Some((Some(t), off)) => (Some(t), off),
        _ => (None, spec_end_offset),
    };

    // Default rule (RULES.md): a relative recurrence with no override repeats
    // at the wall-clock time the command was sent — `*30m`/`*2d`/`*1M`/`*1Y`
    // all fire at creation time, the calendar ones additionally keeping the
    // same day-of-month / day-of-year.
    // Truncated to whole minutes: the seconds of the creation instant would
    // otherwise be frozen into the RRULE (`BYSECOND=27`) and every future
    // occurrence for years would fire at :27.
    let now_local = ctx.now_utc.with_timezone(&ctx.tz);
    let creation_time =
        NaiveTime::from_hms_opt(now_local.hour(), now_local.minute(), 0).unwrap_or(DEFAULT_TIME);
    let time = override_time.unwrap_or(creation_time);

    // A spec maps onto exactly one RRULE FREQ family: yearly, monthly, or the
    // seconds/day interval. Mixing them (`*1Y2M`, `*1Y3d`) has no single FREQ,
    // so reject rather than silently dropping a component.
    let interval_units = spec.weeks > 0 || spec.days > 0 || spec.has_sub_day();
    let families = (spec.years > 0) as u8 + (spec.months > 0) as u8 + interval_units as u8;
    if families > 1 {
        return Err(ParseError::InvalidRecurrenceSpec);
    }

    // A clock override with a sub-day spec is contradictory (`*30m 11:00` — an
    // interval that also wants a fixed time). Same rule as one-shot relative.
    if override_time.is_some() && spec.has_sub_day() {
        return Err(ParseError::SubDayRelWithOverride);
    }

    // *1Y / *2Y → yearly on today's month+day, so `*1Y` == `*8.5` here.
    if spec.years > 0 {
        let interval = u16::try_from(spec.years).map_err(|_| ParseError::InvalidRecurrenceSpec)?;
        let today = today_local(ctx);
        let (month, day) = (today.month() as u8, today.day() as u8);
        let dtstart = next_dtstart_for_yearly(month, day, time, interval, ctx);
        if month == 2 && day == 29 {
            let schedule =
                Schedule::yearly_every_last_day_of_month(dtstart, ctx.tz, interval, month, time)?;
            return Ok(Some((
                schedule,
                end_offset,
                Some(ParseNote::YearlyFeb29Fallback),
            )));
        }
        let schedule = Schedule::yearly_every(dtstart, ctx.tz, interval, month, day, time)?;
        return Ok(Some((schedule, end_offset, None)));
    }
    // *1M / *3M → monthly on today's day-of-month, so `*1M` == `*8.` here.
    if spec.months > 0 {
        let interval = u16::try_from(spec.months).map_err(|_| ParseError::InvalidRecurrenceSpec)?;
        let today = today_local(ctx);
        let day = today.day() as u8;
        let dtstart = next_dtstart_for_monthly_or_last_day(day, time, interval, ctx);
        let schedule = Schedule::monthly_every_or_last_day(dtstart, ctx.tz, interval, day, time)?;
        let note = (day > 28).then_some(ParseNote::MonthlyLastDayFrom { day });
        return Ok(Some((schedule, end_offset, note)));
    }

    let total_secs = spec.weeks as i64 * 7 * 86_400
        + spec.days as i64 * 86_400
        + spec.hours as i64 * 3600
        + spec.minutes as i64 * 60
        + spec.seconds as i64;

    if total_secs < MIN_INTERVAL_SECONDS {
        return Err(ParseError::IntervalTooShort(MIN_INTERVAL_SECONDS));
    }

    // Whole-day multiples use a DST-safe wall-clock daily rule; sub-day
    // intervals use an exact seconds rule anchored at now (so `*30m` stays
    // exactly 30 min apart even across a DST jump). Only the daily rule has a
    // wall-clock time to honour — the sub-day path rejected any override above.
    let schedule = if total_secs % 86_400 == 0 {
        // Guard the u16 cast: `*70000d` would otherwise wrap to 4464 days.
        let every_n_days = u16::try_from(total_secs / 86_400)
            .map_err(|_| ParseError::InvalidRecurrenceSpec)?;
        let dtstart = next_dtstart_for_daily(every_n_days, time, ctx);
        Schedule::daily_at(dtstart, ctx.tz, every_n_days, time)?
    } else {
        let dtstart = ctx.now_utc
            .checked_add_signed(Duration::seconds(total_secs))
            .ok_or(ParseError::InvalidRecurrenceSpec)?;
        // The schedule zone is UTC: hourly/minutely RRULEs iterate in the
        // schedule's zone, so a local zone would drift across DST changes.
        Schedule::interval_seconds(dtstart, chrono_tz::UTC, total_secs)?
    };
    Ok(Some((schedule, end_offset, None)))
}

fn try_recurring_weekday(
    tokens: &[Token<'_>],
    ctx: &ParseContext,
) -> Result<Option<RecurringMatch>, ParseError> {
    let Some(first) = tokens.first().copied() else {
        return Ok(None);
    };
    let Some(days) = match_weekday_list(first.0) else {
        return Ok(None);
    };

    let (maybe_time, end_offset) =
        consume_optional_time(tokens, 1).unwrap_or((None, token_end(first)));
    let time = maybe_time.unwrap_or(DEFAULT_TIME);

    let dtstart = next_dtstart_for_weekly(&days, time, ctx);
    let schedule = Schedule::weekly(dtstart, ctx.tz, &days, time)?;
    Ok(Some((schedule, end_offset, None)))
}

fn match_weekday_list(token: &str) -> Option<Vec<Weekday>> {
    let mut out = Vec::new();
    for part in token.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match match_named_day(part) {
            Some(NamedDay::Weekday(w)) => out.push(w),
            _ => return None,
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn try_recurring_day_of_month(
    tokens: &[Token<'_>],
    ctx: &ParseContext,
) -> Result<Option<RecurringMatch>, ParseError> {
    let Some(first) = tokens.first().copied() else {
        return Ok(None);
    };
    let Some(day) = parse_day_of_month_token(first.0) else {
        return Ok(None);
    };

    let (maybe_time, end_offset) =
        consume_optional_time(tokens, 1).unwrap_or((None, token_end(first)));
    let time = maybe_time.unwrap_or(DEFAULT_TIME);

    let (schedule, note) = if day == 31 {
        let dtstart = next_dtstart_for_monthly_last_day(time, ctx);
        let sch = Schedule::monthly_last_day(dtstart, ctx.tz, time)?;
        (sch, Some(ParseNote::MonthlyLastDay))
    } else {
        let dtstart = next_dtstart_for_monthly(day, time, 1, ctx);
        let sch = Schedule::monthly(dtstart, ctx.tz, day, time)?;
        (sch, None)
    };
    Ok(Some((schedule, end_offset, note)))
}

fn parse_day_of_month_token(s: &str) -> Option<u8> {
    if !s.ends_with('.') {
        return None;
    }
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let day: u8 = digits.parse().ok()?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(day)
}

fn try_recurring_month_day(
    tokens: &[Token<'_>],
    ctx: &ParseContext,
) -> Result<Option<RecurringMatch>, ParseError> {
    let Some(first) = tokens.first().copied() else {
        return Ok(None);
    };
    let Some((day, month)) = parse_month_day_token(first.0) else {
        return Ok(None);
    };

    let (maybe_time, end_offset) =
        consume_optional_time(tokens, 1).unwrap_or((None, token_end(first)));
    let time = maybe_time.unwrap_or(DEFAULT_TIME);

    let (schedule, note) = if day == 29 && month == 2 {
        let dtstart = next_dtstart_for_yearly_last_day_of_month(month, time, ctx);
        let sch = Schedule::yearly_last_day_of_month(dtstart, ctx.tz, month, time)?;
        (sch, Some(ParseNote::YearlyFeb29Fallback))
    } else {
        let dtstart = next_dtstart_for_yearly(month, day, time, 1, ctx);
        let sch = Schedule::yearly(dtstart, ctx.tz, month, day, time)?;
        (sch, None)
    };
    Ok(Some((schedule, end_offset, note)))
}

fn parse_month_day_token(s: &str) -> Option<(u8, u8)> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 2 {
        return None;
    }
    if parts
        .iter()
        .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let day: u8 = parts[0].parse().ok()?;
    let month: u8 = parts[1].parse().ok()?;
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) {
        return None;
    }
    // Sanity-check the combination — `40.13` and friends out.
    NaiveDate::from_ymd_opt(2024, month as u32, day as u32)?;
    Some((day, month))
}

// ---------------------------------------------------------------------------
// dtstart helpers — find the first future occurrence matching the rule.
// ---------------------------------------------------------------------------

/// Find the first candidate date (in user-local tz) that maps to a UTC
/// instant strictly after `ctx.now_utc`. Iterators that produce dates which
/// aren't valid (`NaiveDate::from_ymd_opt` already filtered) just don't yield;
/// dates that fall into a DST gap silently advance.
fn first_future_dtstart(
    candidates: impl IntoIterator<Item = NaiveDate>,
    time: NaiveTime,
    ctx: &ParseContext,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    for date in candidates {
        if let Ok(dt) = local_to_utc(date, time, ctx.tz) {
            if dt > ctx.now_utc {
                return dt;
            }
        }
    }
    fallback
}

/// Iterator over `(year, month)` pairs starting at `(start_y, start_m)`,
/// advancing `step` months at a time for `count` steps. `step` mirrors the
/// RRULE INTERVAL so the anchor search only ever lands on slots the rule
/// itself would produce.
fn month_iter(
    start_y: i32,
    start_m: u32,
    step: u16,
    count: u32,
) -> impl Iterator<Item = (i32, u32)> {
    let step = step.max(1) as u32;
    (0..count).scan((start_y, start_m), move |(y, m), _| {
        let cur = (*y, *m);
        let advanced = *m - 1 + step;
        *y += (advanced / 12) as i32;
        *m = advanced % 12 + 1;
        Some(cur)
    })
}

fn today_local(ctx: &ParseContext) -> NaiveDate {
    ctx.now_utc.with_timezone(&ctx.tz).date_naive()
}

/// Anchor for `FREQ=DAILY;INTERVAL=N`. Two candidates only: today (taken when
/// its wall-clock time is still ahead — `*1d 22:00` sent at 20:00 fires
/// tonight) and today + N days.
///
/// Scanning day by day instead would anchor *every* `*Nd` on tomorrow: without
/// a clock override `time` is the creation time, which has by definition just
/// passed, so the first future candidate is always today + 1 regardless of N.
/// That made `*6d` fire the next day and only then settle into its 6-day
/// rhythm (RULES.md: `*Nd` means the first fire is N days out).
fn next_dtstart_for_daily(
    every_n_days: u16,
    time: NaiveTime,
    ctx: &ParseContext,
) -> DateTime<Utc> {
    let today = today_local(ctx);
    let full_interval_ahead = today + Duration::days(every_n_days as i64);
    first_future_dtstart(
        [today, full_interval_ahead],
        time,
        ctx,
        ctx.now_utc + Duration::days(every_n_days as i64),
    )
}

fn next_dtstart_for_weekly(
    days: &[Weekday],
    time: NaiveTime,
    ctx: &ParseContext,
) -> DateTime<Utc> {
    let today = today_local(ctx);
    let candidates = (0..=7).filter_map(|n: i64| {
        let d = today + Duration::days(n);
        days.contains(&d.weekday()).then_some(d)
    });
    first_future_dtstart(candidates, time, ctx, ctx.now_utc + Duration::days(7))
}

/// Anchor for `FREQ=MONTHLY;INTERVAL=N`. Same rule as the daily anchor: this
/// month's slot if it's still ahead, otherwise N months out — not one.
fn next_dtstart_for_monthly(
    day: u8,
    time: NaiveTime,
    interval: u16,
    ctx: &ParseContext,
) -> DateTime<Utc> {
    let local_now = ctx.now_utc.with_timezone(&ctx.tz);
    let candidates = month_iter(local_now.year(), local_now.month(), interval, 14)
        .filter_map(|(y, m)| NaiveDate::from_ymd_opt(y, m, day as u32));
    first_future_dtstart(candidates, time, ctx, ctx.now_utc + Duration::days(31))
}

/// Like [`next_dtstart_for_monthly`], but a month without `day` contributes
/// its last day, matching [`Schedule::monthly_every_or_last_day`].
fn next_dtstart_for_monthly_or_last_day(
    day: u8,
    time: NaiveTime,
    interval: u16,
    ctx: &ParseContext,
) -> DateTime<Utc> {
    let local_now = ctx.now_utc.with_timezone(&ctx.tz);
    let candidates = month_iter(local_now.year(), local_now.month(), interval, 14).filter_map(
        |(y, m)| NaiveDate::from_ymd_opt(y, m, (day as u32).min(last_day_of_month(y, m))),
    );
    first_future_dtstart(candidates, time, ctx, ctx.now_utc + Duration::days(31))
}

fn next_dtstart_for_monthly_last_day(time: NaiveTime, ctx: &ParseContext) -> DateTime<Utc> {
    let local_now = ctx.now_utc.with_timezone(&ctx.tz);
    let candidates = month_iter(local_now.year(), local_now.month(), 1, 14)
        .filter_map(|(y, m)| NaiveDate::from_ymd_opt(y, m, last_day_of_month(y, m)));
    first_future_dtstart(candidates, time, ctx, ctx.now_utc + Duration::days(31))
}

/// Anchor for `FREQ=YEARLY;INTERVAL=N`. This year's slot if it's still ahead,
/// otherwise N years out — not one.
fn next_dtstart_for_yearly(
    month: u8,
    day: u8,
    time: NaiveTime,
    interval: u16,
    ctx: &ParseContext,
) -> DateTime<Utc> {
    let base_year = ctx.now_utc.with_timezone(&ctx.tz).year();
    let step = interval.max(1) as i32;
    // Feb 29 falls back to Feb 28 in non-leap years.
    let candidates = (0..5).filter_map(|n| {
        let y = base_year + n * step;
        NaiveDate::from_ymd_opt(
            y,
            month as u32,
            (day as u32).min(last_day_of_month(y, month as u32)),
        )
    });
    first_future_dtstart(candidates, time, ctx, ctx.now_utc + Duration::days(365))
}

fn next_dtstart_for_yearly_last_day_of_month(
    month: u8,
    time: NaiveTime,
    ctx: &ParseContext,
) -> DateTime<Utc> {
    let base_year = ctx.now_utc.with_timezone(&ctx.tz).year();
    let candidates = (0..5).filter_map(|n| {
        let y = base_year + n;
        NaiveDate::from_ymd_opt(y, month as u32, last_day_of_month(y, month as u32))
    });
    first_future_dtstart(candidates, time, ctx, ctx.now_utc + Duration::days(365))
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    // Walk back from 31 until a valid date exists. Cheaper than working out
    // leap rules by hand.
    for d in (28..=31).rev() {
        if NaiveDate::from_ymd_opt(year, month, d).is_some() {
            return d;
        }
    }
    28
}
