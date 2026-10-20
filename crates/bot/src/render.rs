//! Locale-aware date/time rendering for confirmation and listing messages.

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
use rrule::{Frequency, NWeekday, RRule, Unvalidated};

use botcore::{Language, Schedule};

/// Compact form for `/list` rows.
pub fn format_local_compact(dt: DateTime<Utc>, tz: Tz, _lang: Language) -> String {
    let local = dt.with_timezone(&tz);
    format!(
        "{:02}.{:02}. {:02}:{:02}",
        local.day(),
        local.month(),
        local.hour(),
        local.minute()
    )
}

/// Short, year-included form for confirmation lines.
/// e.g. `Fr 8.5.2026 14:03` / `Fri 8 May 2026 14:03`.
pub fn format_local_short(dt: DateTime<Utc>, tz: Tz, lang: Language) -> String {
    let local = dt.with_timezone(&tz);
    let weekday = weekday_short(local.weekday(), lang);
    match lang {
        Language::De => format!(
            "{weekday} {}.{}.{} {:02}:{:02}",
            local.day(),
            local.month(),
            local.year(),
            local.hour(),
            local.minute()
        ),
        Language::En => format!(
            "{weekday} {} {} {} {:02}:{:02}",
            local.day(),
            month_name_short(local.month(), lang),
            local.year(),
            local.hour(),
            local.minute()
        ),
    }
}

fn weekday_short(d: chrono::Weekday, lang: Language) -> &'static str {
    use chrono::Weekday::*;
    match (d, lang) {
        (Mon, Language::De) => "Mo",
        (Tue, Language::De) => "Di",
        (Wed, Language::De) => "Mi",
        (Thu, Language::De) => "Do",
        (Fri, Language::De) => "Fr",
        (Sat, Language::De) => "Sa",
        (Sun, Language::De) => "So",
        (Mon, Language::En) => "Mon",
        (Tue, Language::En) => "Tue",
        (Wed, Language::En) => "Wed",
        (Thu, Language::En) => "Thu",
        (Fri, Language::En) => "Fri",
        (Sat, Language::En) => "Sat",
        (Sun, Language::En) => "Sun",
    }
}

/// Compact, human-readable description of a Schedule's recurrence pattern,
/// e.g. `alle 30min`, `täglich 09:00`, `Mo,Mi,Fr 09:00`, `jeden 1. 09:00`.
/// Returns `None` for one-shot schedules.
pub fn format_recurrence_short(schedule: &Schedule, lang: Language) -> Option<String> {
    let rrule_str = schedule.rrule.as_deref()?;
    let rule: RRule<Unvalidated> = rrule_str.parse().ok()?;

    let hour = rule.get_by_hour().first().copied().unwrap_or(0);
    let minute = rule.get_by_minute().first().copied().unwrap_or(0);
    let hm = format!("{hour:02}:{minute:02}");
    let interval = rule.get_interval();

    Some(match rule.get_freq() {
        Frequency::Secondly => {
            let dur = format_duration_human(interval as i64);
            match lang {
                Language::De => format!("alle {dur}"),
                Language::En => format!("every {dur}"),
            }
        }
        Frequency::Minutely => {
            let dur = format_duration_human(interval as i64 * 60);
            match lang {
                Language::De => format!("alle {dur}"),
                Language::En => format!("every {dur}"),
            }
        }
        Frequency::Hourly => {
            let dur = format_duration_human(interval as i64 * 3600);
            match lang {
                Language::De => format!("alle {dur}"),
                Language::En => format!("every {dur}"),
            }
        }
        Frequency::Daily => match (interval, lang) {
            (1, Language::De) => format!("täglich {hm}"),
            (1, Language::En) => format!("daily {hm}"),
            (n, Language::De) => format!("alle {n}d {hm}"),
            (n, Language::En) => format!("every {n}d {hm}"),
        },
        Frequency::Weekly => {
            let by_weekday = rule.get_by_weekday();
            let mut day_strs: Vec<&str> = by_weekday
                .iter()
                .map(|nw| match nw {
                    NWeekday::Every(d) => weekday_short(*d, lang),
                    NWeekday::Nth(_, d) => weekday_short(*d, lang),
                })
                .collect();
            if day_strs.is_empty() {
                day_strs.push("?");
            }
            if day_strs.len() == 7 {
                match lang {
                    Language::De => format!("täglich {hm}"),
                    Language::En => format!("daily {hm}"),
                }
            } else {
                format!("{} {hm}", day_strs.join(","))
            }
        }
        Frequency::Monthly => {
            // `*NM` from the 29th lists 28..=day with BYSETPOS=-1; show the
            // requested day.
            let day = rule.get_by_month_day().iter().copied().max().unwrap_or(1);
            match (interval, lang) {
                (1, Language::De) if day < 0 => format!("jeden letzten Tag {hm}"),
                (1, Language::En) if day < 0 => format!("monthly on the last day {hm}"),
                (1, Language::De) => format!("jeden {day}. {hm}"),
                (1, Language::En) => format!("monthly on {day}. {hm}"),
                (n, Language::De) => format!("alle {n} Monate, {day}. {hm}"),
                (n, Language::En) => format!("every {n} months on {day}. {hm}"),
            }
        }
        Frequency::Yearly => {
            let month = rule.get_by_month().first().copied().unwrap_or(1);
            let day = rule.get_by_month_day().first().copied().unwrap_or(1);
            match (interval, lang) {
                (1, Language::De) if day < 0 => format!(
                    "jährlich am letzten Tag im {} {hm}",
                    month_name_short(month as u32, lang)
                ),
                (1, Language::En) if day < 0 => format!(
                    "yearly on the last day of {} {hm}",
                    month_name_short(month as u32, lang)
                ),
                (1, Language::De) => format!("jährlich {day}.{month}. {hm}"),
                (1, Language::En) => {
                    format!("yearly {day} {} {hm}", month_name_short(month as u32, lang))
                }
                (n, Language::De) if day < 0 => format!(
                    "alle {n} Jahre am letzten Tag im {} {hm}",
                    month_name_short(month as u32, lang)
                ),
                (n, Language::En) if day < 0 => format!(
                    "every {n} years on the last day of {} {hm}",
                    month_name_short(month as u32, lang)
                ),
                (n, Language::De) => format!("alle {n} Jahre, {day}.{month}. {hm}"),
                (n, Language::En) => format!(
                    "every {n} years on {day} {} {hm}",
                    month_name_short(month as u32, lang)
                ),
            }
        }
    })
}

/// Compact human-readable duration, e.g. "5min", "2h 30min", "1d 30min".
/// Every non-zero unit is shown, so an interval reads exactly as scheduled.
pub fn format_duration_human(secs: i64) -> String {
    let secs = secs.max(0);
    if secs == 0 {
        return "0s".to_string();
    }
    let parts = [
        (secs / 86_400, "d"),
        (secs / 3600 % 24, "h"),
        (secs / 60 % 60, "min"),
        (secs % 60, "s"),
    ];
    parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn month_name_short(m: u32, lang: Language) -> &'static str {
    let table_de = [
        "Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez",
    ];
    let table_en = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let idx = (m as usize).saturating_sub(1).min(11);
    match lang {
        Language::De => table_de[idx],
        Language::En => table_en[idx],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveTime, TimeZone};

    #[test]
    fn monthly_fallback_rule_shows_the_requested_day() {
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        let dtstart = tz
            .with_ymd_and_hms(2027, 2, 28, 9, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let time = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let schedule = Schedule::monthly_every_or_last_day(dtstart, tz, 1, 30, time).unwrap();
        assert_eq!(
            format_recurrence_short(&schedule, Language::De).as_deref(),
            Some("jeden 30. 09:00")
        );
    }

    #[test]
    fn durations_keep_every_unit() {
        assert_eq!(format_duration_human(0), "0s");
        assert_eq!(format_duration_human(45), "45s");
        assert_eq!(format_duration_human(30 * 60), "30min");
        assert_eq!(format_duration_human(30 * 60 + 15), "30min 15s");
        assert_eq!(format_duration_human(2 * 3600 + 30 * 60), "2h 30min");
        assert_eq!(format_duration_human(86_400), "1d");
        assert_eq!(format_duration_human(86_400 + 30 * 60), "1d 30min");
        assert_eq!(format_duration_human(25 * 3600 + 30 * 60), "1d 1h 30min");
    }

    #[test]
    fn minutely_rule_renders_the_full_interval() {
        let (dtstart, tz, _) = anchor();
        let s = Schedule::from_db(dtstart, tz, Some("FREQ=MINUTELY;INTERVAL=1470")).unwrap();
        assert_eq!(
            format_recurrence_short(&s, Language::De).as_deref(),
            Some("alle 1d 30min")
        );
    }

    fn anchor() -> (DateTime<Utc>, Tz, NaiveTime) {
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        let dtstart = tz.with_ymd_and_hms(2026, 5, 31, 9, 0, 0).unwrap();
        (
            dtstart.with_timezone(&Utc),
            tz,
            NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        )
    }

    #[test]
    fn monthly_last_day_renders_as_last_day() {
        let (dtstart, tz, time) = anchor();
        let s = Schedule::monthly_last_day(dtstart, tz, time).unwrap();
        assert_eq!(
            format_recurrence_short(&s, Language::De).as_deref(),
            Some("jeden letzten Tag 09:00")
        );
        assert_eq!(
            format_recurrence_short(&s, Language::En).as_deref(),
            Some("monthly on the last day 09:00")
        );
    }

    #[test]
    fn yearly_last_day_of_february_renders_as_last_day() {
        let (dtstart, tz, time) = anchor();
        let s = Schedule::yearly_last_day_of_month(dtstart, tz, 2, time).unwrap();
        assert_eq!(
            format_recurrence_short(&s, Language::De).as_deref(),
            Some("jährlich am letzten Tag im Feb 09:00")
        );
        assert_eq!(
            format_recurrence_short(&s, Language::En).as_deref(),
            Some("yearly on the last day of Feb 09:00")
        );
    }

    #[test]
    fn yearly_interval_last_day_renders_as_last_day() {
        let (dtstart, tz, _) = anchor();
        let rule = "FREQ=YEARLY;INTERVAL=2;BYMONTH=2;BYMONTHDAY=-1;BYHOUR=9;BYMINUTE=0";
        let s = Schedule::from_db(dtstart, tz, Some(rule)).unwrap();
        assert_eq!(
            format_recurrence_short(&s, Language::De).as_deref(),
            Some("alle 2 Jahre am letzten Tag im Feb 09:00")
        );
        assert_eq!(
            format_recurrence_short(&s, Language::En).as_deref(),
            Some("every 2 years on the last day of Feb 09:00")
        );
    }
}
