//! User-facing strings, keyed by `Language`.
//!
//! Kept as a flat module of small functions rather than a `fluent` bundle —
//! the surface area is small enough that compile-time-checked Rust strings
//! beat the indirection of an FTL file plus runtime lookups.

use botcore::Language;
use parser::ParseNote;

/// Localized command-menu entries shown in the Telegram client's `/`-picker.
/// Returned in the order they should appear.
pub fn command_menu(lang: Language) -> Vec<(&'static str, &'static str)> {
    match lang {
        Language::De => vec![
            ("start", "begrüßt dich und richtet dein Profil ein"),
            ("help", "zeigt alle Befehle und Beispiele"),
            ("alert", "Reminder — nur du kannst ihn löschen (DM und Gruppen)"),
            ("galert", "Gruppen-Reminder — jedes aktuelle Mitglied kann ihn löschen"),
            ("list", "listet aktive Reminder im aktuellen Chat"),
            ("cancel", "löscht einen Reminder per ID"),
            ("tz", "setzt deine Zeitzone, z.B. Europe/Berlin"),
            ("lang", "wechselt die Sprache (de oder en)"),
        ],
        Language::En => vec![
            ("start", "say hi and set up your profile"),
            ("help", "show all commands and examples"),
            ("alert", "reminder — only you can cancel it (DMs and groups)"),
            ("galert", "group reminder — any current member can cancel it"),
            ("list", "show active reminders in the current chat"),
            ("cancel", "delete a reminder by id"),
            ("tz", "set your timezone, e.g. Europe/Berlin"),
            ("lang", "switch language (de or en)"),
        ],
    }
}

pub fn welcome(lang: Language) -> &'static str {
    match lang {
        Language::De => "Hi! Ich bin dein Alert-Bot. Schick mir z.B.\n\
            /alert 5m Kaffee fertig\n\
            /alert 30.4.26 Steuererklärung\n\
            /alert do 14:00 Standup\n\n\
            In Gruppen funktionieren beide: /alert für deine eigenen Reminder, \
            /galert wenn ihn jedes aktuelle Mitglied verwalten können soll. \
            Für /galert muss der Bot Gruppenadmin sein.\n\
            /help zeigt alle Befehle.",
        Language::En => "Hi! I'm your alert bot. Try messages like\n\
            /alert 5m coffee ready\n\
            /alert 30.4.26 file taxes\n\
            /alert thu 14:00 standup\n\n\
            Both work in groups: /alert for your own reminders, /galert when \
            any current member should be able to manage it. \
            /galert requires the bot to be a group administrator.\n\
            /help lists every command.",
    }
}

pub fn help(lang: Language) -> &'static str {
    match lang {
        Language::De => "<b>Befehle</b>\n\
            /alert &lt;zeit&gt; &lt;text&gt; — Reminder. Nur du kannst ihn löschen.\n\
            /galert &lt;zeit&gt; &lt;text&gt; — Gruppen-Reminder. Jedes aktuelle Mitglied kann löschen. Bot muss Gruppenadmin sein.\n\
            /list — aktive Reminder anzeigen\n\
            /cancel &lt;id&gt; — Reminder löschen\n\
            /tz &lt;zone&gt; — Zeitzone (z.B. Europe/Berlin)\n\
            /lang &lt;de oder en&gt; — Sprache umschalten\n\n\
            <b>Einmalig</b>  <i>(antippen zum Ausklappen)</i>\n\
            <blockquote expandable><i>Relativ ab jetzt</i>\n\
            5m Kaffee — in 5 Minuten\n\
            2h Pause — in 2 Stunden\n\
            30d Abo kündigen — in 30 Tagen\n\
            1w Miete — in 1 Woche\n\
            2M Versicherung — in 2 Monaten (selber Monatstag)\n\
            1Y Geburtstag — in 1 Jahr (selbes Datum)\n\
            7h30m Pause — Stunden + Minuten kombiniert\n\
            1Y2M15d Test — Reihenfolge Y &gt; M &gt; w &gt; d &gt; h &gt; m &gt; s\n\n\
            <i>Datum</i>\n\
            30.4.26 Termin — am 30.04.2026 um 09:00\n\
            30.04.2026 14:30 Arzt — Datum + Uhrzeit\n\
            30.4 Geburtstag — nächstes Auftreten\n\
            2026-12-31 Sylvester — ISO-Form\n\n\
            <i>Uhrzeit (nächstes Auftreten)</i>\n\
            22:00 Bier — heute 22:00, sonst morgen\n\
            14 Uhr Treffen — kurz für 14:00\n\
            2pm Meeting — am/pm geht auch\n\n\
            <i>Benannte Tage</i>\n\
            morgen 9 Uhr Arzt\n\
            übermorgen Treffen\n\
            do 14:00 Standup — nächster Donnerstag\n\
            fr Putzen — nächster Freitag (09:00 Default)\n\n\
            <i>Mit Override-Uhrzeit (nur ohne Stunden/Min/Sek)</i>\n\
            2d 11:00 Vitamin — übermorgen 11:00\n\
            1w 18:00 Zahlen — in 1 Woche 18:00\n\
            1Y 9:00 Jubiläum — nächstes Jahr 09:00</blockquote>\n\n\
            <b>Wiederkehrend</b>  <i>(Prefix * oder jeden/alle)</i>\n\
            <blockquote expandable><i>Intervall</i>\n\
            *30m Wasser — alle 30 Minuten\n\
            alle 2h Pause — synonym zu *2h\n\
            *1d Vitamin — täglich zur Erstellungs-Uhrzeit\n\
            *2d 11:00 Tabletten — alle 2 Tage 11:00\n\n\
            <i>Wochentage</i>\n\
            *do 14:00 Standup — jeden Donnerstag\n\
            *mo,mi,fr 9 Yoga — Mo/Mi/Fr um 09:00\n\
            jeden montag yoga — Langform\n\n\
            <i>Monatlich</i>\n\
            *1. Miete — jeden 1. um 09:00\n\
            *15. 18:00 Abrechnung — jeden 15. um 18:00\n\
            *31. Bilanz — letzter Tag des Monats (Hinweis bei Erstellung)\n\n\
            <i>Jährlich</i>\n\
            *24.12 Heiligabend — jedes Jahr\n\
            *29.2 Schaltjahr — Feb 28/29 (Hinweis bei Erstellung)</blockquote>\n\n\
            <b>Gut zu wissen</b>\n\
            • <b>m</b> = Minuten, <b>M</b> = Monate (Groß/klein zählt!)\n\
            • Tippfehler werden korrigiert: donnerstah → Donnerstag\n\
            • Mindestintervall 30 Min, Max einmalig 50 Jahre\n\
            • <i>heute</i>/<i>today</i> ist absichtlich raus — nimm direkt die Uhrzeit",
        Language::En => "<b>Commands</b>\n\
            /alert &lt;time&gt; &lt;text&gt; — reminder. Only you can cancel it.\n\
            /galert &lt;time&gt; &lt;text&gt; — group reminder. Any current member can cancel. The bot must be a group administrator.\n\
            /list — show active reminders\n\
            /cancel &lt;id&gt; — delete a reminder\n\
            /tz &lt;zone&gt; — set timezone (e.g. Europe/Berlin)\n\
            /lang &lt;de or en&gt; — switch language\n\n\
            <b>One-shot</b>  <i>(tap to expand)</i>\n\
            <blockquote expandable><i>Relative to now</i>\n\
            5m coffee — in 5 minutes\n\
            2h break — in 2 hours\n\
            30d cancel sub — in 30 days\n\
            1w rent — in 1 week\n\
            2M insurance — in 2 months (same day-of-month)\n\
            1Y birthday — in 1 year (same date)\n\
            7h30m break — hours + minutes combined\n\
            1Y2M15d test — order Y &gt; M &gt; w &gt; d &gt; h &gt; m &gt; s\n\n\
            <i>Date</i>\n\
            30.4.26 appointment — on 30 Apr 2026 at 09:00\n\
            30.04.2026 14:30 doctor — date + time\n\
            30.4 birthday — next occurrence\n\
            2026-12-31 new year — ISO form\n\n\
            <i>Clock-time (next occurrence)</i>\n\
            22:00 beer — today 22:00, else tomorrow\n\
            14 Uhr meeting — short for 14:00\n\
            2pm meeting — am/pm works too\n\n\
            <i>Named day</i>\n\
            tomorrow 9 doctor\n\
            thu 14:00 standup — next Thursday\n\
            fri cleanup — next Friday (09:00 default)\n\n\
            <i>With override clock (only without hours/min/sec)</i>\n\
            2d 11:00 vitamin — day after tomorrow 11:00\n\
            1w 18:00 pay — in 1 week 18:00\n\
            1Y 9:00 anniversary — in 1 year 09:00</blockquote>\n\n\
            <b>Recurring</b>  <i>(prefix * or every)</i>\n\
            <blockquote expandable><i>Interval</i>\n\
            *30m water — every 30 minutes\n\
            every 2h break — synonym for *2h\n\
            *1d vitamin — daily at the creation time\n\
            *2d 11:00 pills — every 2 days at 11:00\n\n\
            <i>Weekdays</i>\n\
            *thu 14:00 standup — every Thursday\n\
            *mon,wed,fri 9 yoga — Mon/Wed/Fri at 09:00\n\
            every monday yoga — long form\n\n\
            <i>Monthly</i>\n\
            *1. rent — every 1st at 09:00\n\
            *15. 18:00 billing — every 15th at 18:00\n\
            *31. balance — last day of month (one-time hint at creation)\n\n\
            <i>Yearly</i>\n\
            *24.12 christmas — every year\n\
            *29.2 leap day — Feb 28/29 (one-time hint at creation)</blockquote>\n\n\
            <b>Good to know</b>\n\
            • <b>m</b> = minutes, <b>M</b> = months (case matters!)\n\
            • Typos are auto-corrected: thurzday → Thursday\n\
            • Minimum interval 30 min, max one-shot offset 50 years\n\
            • <i>today</i>/<i>heute</i> is deliberately rejected — use the clock time directly",
    }
}

pub fn alert_confirmation(
    lang: Language,
    when_short: &str,
    id: i64,
    recurrence_short: Option<&str>,
) -> String {
    let verb = match lang {
        Language::De => "Gespeichert",
        Language::En => "Saved",
    };
    match recurrence_short {
        Some(rec) => format!("✓ {verb} · #{id} 🔁 · {when_short} · {rec}"),
        None => format!("✓ {verb} · #{id} · {when_short}"),
    }
}

/// `creator_handle` is already HTML-escaped by the caller.
pub fn galert_confirmation(
    lang: Language,
    when_short: &str,
    id: i64,
    creator_handle: &str,
    recurrence_short: Option<&str>,
) -> String {
    let (verb, by) = match lang {
        Language::De => ("Gespeichert", "von"),
        Language::En => ("Saved", "by"),
    };
    match recurrence_short {
        Some(rec) => format!(
            "👥 {verb} · #{id} 🔁 · {when_short} · {rec} · {by} {creator_handle}"
        ),
        None => format!("👥 {verb} · #{id} · {when_short} · {by} {creator_handle}"),
    }
}

pub fn cancelled_inline(_lang: Language, id: i64) -> String {
    format!("✗ #{id} · cancelled")
}

pub fn button_cancel(lang: Language) -> &'static str {
    match lang {
        Language::De => "✗ Löschen",
        Language::En => "✗ Cancel",
    }
}

/// Snooze labels on delivered-reminder messages. Same in both languages —
/// the suffix notation is universal and stays compact on mobile.
pub fn button_snooze_5m(_lang: Language) -> &'static str {
    "+5m"
}

pub fn button_snooze_15m(_lang: Language) -> &'static str {
    "+15m"
}

pub fn button_snooze_1h(_lang: Language) -> &'static str {
    "+1h"
}

pub fn button_stop_series(lang: Language) -> &'static str {
    match lang {
        Language::De => "✗ Serie beenden",
        Language::En => "✗ Stop series",
    }
}

/// Confirmation sent after a snooze tap. `when_compact` is the resolved fire
/// time rendered in the clicker's timezone.
pub fn snooze_confirmation(lang: Language, when_compact: &str) -> String {
    match lang {
        Language::De => format!("💤 verschoben · {when_compact}"),
        Language::En => format!("💤 snoozed · {when_compact}"),
    }
}

pub fn snooze_gone(lang: Language) -> &'static str {
    match lang {
        Language::De => "Konnte nicht verschieben — Reminder existiert nicht mehr.",
        Language::En => "Couldn't snooze — reminder is gone.",
    }
}

pub fn snooze_invalid_offset(lang: Language) -> &'static str {
    match lang {
        Language::De => "Bitte wähle +5min, +15min oder +1h.",
        Language::En => "Please choose +5min, +15min or +1h.",
    }
}

pub fn series_stopped(lang: Language, id: i64) -> String {
    match lang {
        Language::De => format!("✗ Serie #{id} beendet."),
        Language::En => format!("✗ Series #{id} stopped."),
    }
}

// ---------------------------------------------------------------------------
// Anti-spam / quota errors. Kept short so they also fit Telegram's ~200-char
// callback-query toast limit.
// ---------------------------------------------------------------------------

pub fn rate_limited(lang: Language) -> &'static str {
    match lang {
        Language::De => "Zu viele Befehle in kurzer Zeit. Probier's gleich nochmal.",
        Language::En => "Too many commands too fast. Try again in a moment.",
    }
}

pub fn user_quota_exceeded(lang: Language, current: i64, max: i64) -> String {
    match lang {
        Language::De => format!(
            "Du hast {current} aktive Reminder (Limit {max}). Lösch ein paar mit /cancel <id>, dann geht's weiter."
        ),
        Language::En => format!(
            "You have {current} active reminders (limit {max}). Cancel some with /cancel <id> to make room."
        ),
    }
}

pub fn chat_quota_exceeded(lang: Language, current: i64, max: i64) -> String {
    match lang {
        Language::De => format!(
            "Dieser Chat hat schon {current} aktive Reminder (Limit {max}). /list zeigt sie."
        ),
        Language::En => format!(
            "This chat already has {current} active reminders (limit {max}). /list shows them."
        ),
    }
}

pub fn text_too_long(lang: Language, max: usize) -> String {
    match lang {
        Language::De => format!("Reminder-Text ist zu lang (max {max} Zeichen)."),
        Language::En => format!("Reminder text is too long (max {max} chars)."),
    }
}

/// Prepended to a reminder when the worker delivered it noticeably late
/// (typically because the bot was offline). `delay_human` is something like
/// "5min", "2h", "1d 3h".
pub fn delayed_prefix(lang: Language, delay_human: &str) -> String {
    match lang {
        Language::De => format!("⚠️ Verzögert um {delay_human} (Bot war offline)\n"),
        Language::En => format!("⚠️ Delayed by {delay_human} (bot was offline)\n"),
    }
}

pub fn nudge_to_help(lang: Language) -> &'static str {
    match lang {
        Language::De => "Probier /alert 5m text oder /help.",
        Language::En => "Try /alert 5m text or /help.",
    }
}

pub fn callback_cancelled(lang: Language) -> &'static str {
    match lang {
        Language::De => "Gelöscht",
        Language::En => "Cancelled",
    }
}

pub fn parse_error_unknown(lang: Language) -> &'static str {
    match lang {
        Language::De => "Konnte die Zeit nicht erkennen. /help zeigt Beispiele.",
        Language::En => "Couldn't parse the time. Try /help for examples.",
    }
}

pub fn parse_error_in_past(lang: Language) -> &'static str {
    match lang {
        Language::De => "Der Zeitpunkt liegt in der Vergangenheit.",
        Language::En => "That time is in the past.",
    }
}

pub fn parse_error_missing_text(lang: Language) -> &'static str {
    match lang {
        Language::De => "Was soll dich denn erinnern? Beispiel: /alert 5m Kaffee",
        Language::En => "Reminder text missing. Example: /alert 5m coffee",
    }
}

pub fn parse_error_heute_rejected(lang: Language) -> &'static str {
    match lang {
        Language::De => "„heute“ verstehe ich nicht — schreib direkt die Uhrzeit, z.B. /alert 22:00 Bier.",
        Language::En => "I don’t take „today“ — just use the time directly, e.g. /alert 22:00 beer.",
    }
}

pub fn parse_error_subday_override(lang: Language) -> &'static str {
    match lang {
        Language::De => "Uhrzeit-Override geht nur, wenn die relative Angabe keine Stunden/Minuten/Sekunden hat. Beispiele: /alert 2d 11:00 text, /alert 1Y 9:00 text.",
        Language::En => "Clock-time override only works when the relative spec has no sub-day components. Examples: /alert 2d 11:00 text, /alert 1Y 9:00 text.",
    }
}

pub fn parse_error_rel_too_far(lang: Language, years: i32) -> String {
    match lang {
        Language::De => format!("Das ist mehr als {years} Jahre in der Zukunft — so weit gehen einmalige Reminder nicht."),
        Language::En => format!("That's more than {years} years out — one-shot reminders are capped at {years}."),
    }
}

pub fn parse_error_invalid_rel_spec(lang: Language) -> &'static str {
    match lang {
        Language::De => "Die relative Angabe sieht falsch aus — Reihenfolge ist Y>M>w>d>h>m>s, ohne Leerzeichen. Beispiel: 1Y2M15d8h30m.",
        Language::En => "That relative spec looks off — the order is Y>M>w>d>h>m>s with no spaces. Example: 1Y2M15d8h30m.",
    }
}

pub fn parse_note(note: ParseNote, lang: Language) -> String {
    match (note, lang) {
        (ParseNote::LeapClampedToFeb28 { new_year }, Language::De) => {
            format!("Hinweis: 29.2. → 28.2. in {new_year} (kein Schaltjahr).")
        }
        (ParseNote::LeapClampedToFeb28 { new_year }, Language::En) => {
            format!("Note: Feb 29 → Feb 28 in {new_year} (not a leap year).")
        }
        (ParseNote::MonthDayClamped { before_day, after_day }, Language::De) => format!(
            "Hinweis: Tag {before_day} gibt es im Zielmonat nicht — angepasst auf {after_day}."
        ),
        (ParseNote::MonthDayClamped { before_day, after_day }, Language::En) => format!(
            "Note: day {before_day} doesn't exist in the target month — clamped to {after_day}."
        ),
        (ParseNote::MonthlyLastDay, Language::De) => {
            "Hinweis: in Monaten ohne 31. feuert der Reminder am letzten Tag des Monats.".into()
        }
        (ParseNote::MonthlyLastDay, Language::En) => {
            "Note: in months without a 31st, this fires on the last day of the month.".into()
        }
        (ParseNote::MonthlyLastDayFrom { day }, Language::De) => format!(
            "Hinweis: in Monaten ohne {day}. feuert der Reminder am letzten Tag des Monats."
        ),
        (ParseNote::MonthlyLastDayFrom { day }, Language::En) => {
            format!("Note: in months without day {day}, this fires on the last day of the month.")
        }
        (ParseNote::YearlyFeb29Fallback, Language::De) => {
            "Hinweis: in Nicht-Schaltjahren feuert dieser Reminder am 28.2..".into()
        }
        (ParseNote::YearlyFeb29Fallback, Language::En) => {
            "Note: in non-leap years this fires on Feb 28.".into()
        }
    }
}

pub fn list_empty(lang: Language) -> &'static str {
    match lang {
        Language::De => "Keine aktiven Reminder.",
        Language::En => "No active reminders.",
    }
}

pub fn list_header(lang: Language) -> &'static str {
    match lang {
        Language::De => "Aktive Reminder",
        Language::En => "Active reminders",
    }
}

pub fn cancel_ok(lang: Language, id: i64) -> String {
    match lang {
        Language::De => format!("✗ #{id} gelöscht."),
        Language::En => format!("✗ #{id} cancelled."),
    }
}

pub fn cancel_not_allowed(lang: Language) -> &'static str {
    match lang {
        Language::De => "Du darfst diesen Reminder nicht löschen.",
        Language::En => "You can't cancel this reminder.",
    }
}

pub fn shared_membership_required(lang: Language) -> &'static str {
    match lang {
        Language::De => "Nur aktuelle Gruppenmitglieder dürfen diesen Reminder ändern.",
        Language::En => "Only current group members can change this reminder.",
    }
}

pub fn shared_membership_check_failed(lang: Language) -> &'static str {
    match lang {
        Language::De => {
            "Ich konnte deine Gruppenmitgliedschaft nicht prüfen. Der Reminder bleibt unverändert."
        }
        Language::En => "I couldn't verify your group membership. The reminder was not changed.",
    }
}

pub fn group_alert_bot_admin_required(lang: Language) -> &'static str {
    match lang {
        Language::De => {
            "Ich muss Gruppenadmin sein, damit ich vor jeder Änderung die Mitgliedschaft prüfen kann. Es wurde nichts angelegt."
        }
        Language::En => {
            "I must be a group administrator so I can check membership before every change. Nothing was created."
        }
    }
}

pub fn group_alert_bot_admin_check_failed(lang: Language) -> &'static str {
    match lang {
        Language::De => {
            "Ich konnte meine Gruppenadmin-Rolle nicht prüfen. Es wurde nichts angelegt."
        }
        Language::En => "I couldn't verify my group administrator role. Nothing was created.",
    }
}

pub fn cancel_not_found(lang: Language) -> &'static str {
    match lang {
        Language::De => "Reminder nicht gefunden.",
        Language::En => "Reminder not found.",
    }
}

pub fn galert_dm_rejected(lang: Language) -> &'static str {
    match lang {
        Language::De => "/galert funktioniert nur in Gruppen. Im Privatchat nimm /alert.",
        Language::En => "/galert only works in groups. In DMs use /alert.",
    }
}

pub fn tz_set(lang: Language, tz: &str) -> String {
    match lang {
        Language::De => format!("Zeitzone gesetzt: {tz}"),
        Language::En => format!("Timezone set: {tz}"),
    }
}

pub fn tz_invalid(lang: Language) -> &'static str {
    match lang {
        Language::De => "Unbekannte Zeitzone. Beispiel: /tz Europe/Berlin",
        Language::En => "Unknown timezone. Example: /tz Europe/Berlin",
    }
}

pub fn lang_set(lang: Language) -> &'static str {
    match lang {
        Language::De => "Sprache: Deutsch",
        Language::En => "Language: English",
    }
}

pub fn lang_invalid(lang: Language) -> &'static str {
    match lang {
        Language::De => "Verfügbare Sprachen: de, en",
        Language::En => "Available languages: de, en",
    }
}

pub fn snooze_already_created(lang: Language) -> &'static str {
    match lang {
        Language::De => "Dieser Reminder wurde bereits verschoben.",
        Language::En => "This reminder was already snoozed.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Telegram's `sendMessage` rejects payloads longer than 4096 chars.
    /// `/help` is the longest static string we send, so guard it directly.
    #[test]
    fn help_fits_telegram_limit() {
        const TELEGRAM_LIMIT: usize = 4096;
        for lang in [Language::De, Language::En] {
            let len = help(lang).chars().count();
            assert!(
                len < TELEGRAM_LIMIT,
                "help({lang:?}) is {len} chars, Telegram limit is {TELEGRAM_LIMIT}"
            );
        }
    }

    /// `*1d` without an override fires at the creation time (RULES.md), so the
    /// help example must not promise 09:00.
    #[test]
    fn help_relative_daily_example_does_not_promise_default_time() {
        for lang in [Language::De, Language::En] {
            let line = help(lang)
                .lines()
                .find(|l| l.starts_with("*1d "))
                .unwrap_or_else(|| panic!("no *1d example in help({lang:?})"));
            assert!(!line.contains("09:00"), "help({lang:?}): {line}");
        }
    }
}
