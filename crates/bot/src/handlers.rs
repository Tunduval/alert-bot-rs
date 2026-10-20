//! Command handlers: each `Command` variant maps to one async function.

use std::sync::Arc;

use chrono::Utc;
use chrono_tz::Tz;
use teloxide::prelude::*;
use teloxide::types::{
    BotCommand, BotCommandScope, ChatKind, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, ParseMode,
    PublicChatKind, Recipient,
};

use botcore::{AlertScope, ChatType, Language, NewAlert, Schedule};
use parser::ParseContext;
use storage::PgStore;

use crate::commands::Command;
use crate::limits::{self, CommandRateLimiter};
use crate::messages as m;
use crate::render;

pub type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub async fn dispatch(
    bot: Bot,
    msg: Message,
    cmd: Command,
    store: Arc<PgStore>,
    limiter: Arc<CommandRateLimiter>,
) -> HandlerResult {
    let user_id = match msg.from.as_ref() {
        Some(u) => u.id.0 as i64,
        None => return Ok(()), // anonymous channel posts: ignore
    };
    let detected_lang = Language::from_telegram_code(
        msg.from.as_ref().and_then(|u| u.language_code.as_deref()),
    );

    // Rate-limit before touching the DB. A spammer who blew past their
    // per-minute budget shouldn't get free upsert_user calls.
    if !limiter.check(user_id) {
        bot.send_message(msg.chat.id, m::rate_limited(detected_lang))
            .await
            .ok();
        return Ok(());
    }

    let user = store.upsert_user(user_id, detected_lang).await?;

    match cmd {
        Command::Start => start(bot, msg, user.language).await?,
        Command::Help => help(bot, msg, user.language).await?,
        Command::Alert(text) => {
            create_alert(&bot, &msg, &store, &user, AlertScope::Private, &text).await?
        }
        Command::Galert(text) => {
            galert(&bot, &msg, &store, &user, &text).await?;
        }
        Command::List => list(&bot, &msg, &store, user.language, user.timezone).await?,
        Command::Cancel(id) => cancel(&bot, &msg, &store, user_id, user.language, id).await?,
        Command::Tz(zone) => set_tz(&bot, &msg, &store, user_id, user.language, &zone).await?,
        Command::Lang(lang) => set_lang(&bot, &msg, &store, user_id, &lang).await?,
    }

    Ok(())
}

async fn start(bot: Bot, msg: Message, lang: Language) -> HandlerResult {
    bot.send_message(msg.chat.id, m::welcome(lang))
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

async fn help(bot: Bot, msg: Message, lang: Language) -> HandlerResult {
    bot.send_message(msg.chat.id, m::help(lang))
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

async fn create_alert(
    bot: &Bot,
    msg: &Message,
    store: &PgStore,
    user: &botcore::User,
    scope: AlertScope,
    text: &str,
) -> HandlerResult {
    let ctx = ParseContext {
        now_utc: Utc::now(),
        tz: user.timezone,
        language: user.language,
    };
    let parsed = match parser::parse(text, &ctx) {
        Ok(p) => p,
        Err(e) => {
            let reply: String = match e {
                parser::ParseError::Empty | parser::ParseError::MissingText => {
                    m::parse_error_missing_text(user.language).to_string()
                }
                parser::ParseError::InPast => m::parse_error_in_past(user.language).to_string(),
                parser::ParseError::HeuteRejected => {
                    m::parse_error_heute_rejected(user.language).to_string()
                }
                parser::ParseError::SubDayRelWithOverride => {
                    m::parse_error_subday_override(user.language).to_string()
                }
                parser::ParseError::RelTooFar(y) => {
                    m::parse_error_rel_too_far(user.language, y)
                }
                parser::ParseError::InvalidRelSpec => {
                    m::parse_error_invalid_rel_spec(user.language).to_string()
                }
                _ => m::parse_error_unknown(user.language).to_string(),
            };
            bot.send_message(msg.chat.id, reply)
                .parse_mode(ParseMode::Html)
                .await?;
            return Ok(());
        }
    };

    // Anti-spam guards. Order matters: text-length is the cheapest check, do
    // it first so we don't even count rows for an obvious junk payload.
    if parsed.text.chars().count() > limits::MAX_TEXT_LEN {
        bot.send_message(
            msg.chat.id,
            m::text_too_long(user.language, limits::MAX_TEXT_LEN),
        )
        .await?;
        return Ok(());
    }
    let user_count = store.count_active_for_user(user.telegram_id).await?;
    if user_count >= limits::MAX_ALERTS_PER_USER {
        bot.send_message(
            msg.chat.id,
            m::user_quota_exceeded(user.language, user_count, limits::MAX_ALERTS_PER_USER),
        )
        .await?;
        return Ok(());
    }
    let chat_count = store.count_active_for_chat(msg.chat.id.0).await?;
    if chat_count >= limits::MAX_ALERTS_PER_CHAT {
        bot.send_message(
            msg.chat.id,
            m::chat_quota_exceeded(user.language, chat_count, limits::MAX_ALERTS_PER_CHAT),
        )
        .await?;
        return Ok(());
    }

    let chat_type = chat_type_from_msg(msg);
    let new = NewAlert {
        user_id: user.telegram_id,
        chat_id: msg.chat.id.0,
        chat_type,
        scope,
        text: parsed.text.clone(),
        fire_at: parsed.fire_at(),
        schedule: parsed.schedule.clone(),
    };
    let alert = store.create_alert(new).await?;

    let when = render::format_local_short(alert.fire_at, user.timezone, user.language);
    let recurrence_short = render::format_recurrence_short(&alert.schedule, user.language);
    let recurrence_str = recurrence_short.as_deref();
    let mut reply = match scope {
        AlertScope::Private => m::alert_confirmation(user.language, &when, alert.id, recurrence_str),
        AlertScope::Shared => {
            let handle = msg
                .from
                .as_ref()
                .and_then(|u| u.username.as_ref())
                .map(|h| format!("@{h}"))
                .unwrap_or_else(|| {
                    msg.from
                        .as_ref()
                        .map(|u| u.first_name.clone())
                        .unwrap_or_default()
                });
            m::galert_confirmation(
                user.language,
                &when,
                alert.id,
                &html_escape(&handle),
                recurrence_str,
            )
        }
    };

    for note in &parsed.notes {
        reply.push('\n');
        reply.push_str(&html_escape(&m::parse_note(*note, user.language)));
    }

    bot.send_message(msg.chat.id, reply)
        .reply_markup(action_keyboard(user.language, alert.id))
        .await?;
    Ok(())
}

fn action_keyboard(lang: Language, id: i64) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        m::button_cancel(lang),
        format!("cancel:{id}"),
    )]])
}

/// Inline keyboard attached to each *delivered* reminder. Always three snooze
/// buttons; an extra ✗ Serie beenden appears only for recurring alerts, so
/// users can kill the whole series without typing `/cancel`.
pub fn delivery_keyboard(lang: Language, alert_id: i64, is_recurring: bool) -> InlineKeyboardMarkup {
    let mut row = vec![
        InlineKeyboardButton::callback(
            m::button_snooze_5m(lang),
            format!("snooze:300:{alert_id}"),
        ),
        InlineKeyboardButton::callback(
            m::button_snooze_15m(lang),
            format!("snooze:900:{alert_id}"),
        ),
        InlineKeyboardButton::callback(
            m::button_snooze_1h(lang),
            format!("snooze:3600:{alert_id}"),
        ),
    ];
    if is_recurring {
        row.push(InlineKeyboardButton::callback(
            m::button_stop_series(lang),
            format!("stop_series:{alert_id}"),
        ));
    }
    InlineKeyboardMarkup::new(vec![row])
}

async fn galert(
    bot: &Bot,
    msg: &Message,
    store: &PgStore,
    user: &botcore::User,
    text: &str,
) -> HandlerResult {
    let chat_type = chat_type_from_msg(msg);
    if !chat_type.is_group() {
        bot.send_message(msg.chat.id, m::galert_dm_rejected(user.language))
            .parse_mode(ParseMode::Html)
            .await?;
        return Ok(());
    }
    create_alert(bot, msg, store, user, AlertScope::Shared, text).await
}

async fn list(
    bot: &Bot,
    msg: &Message,
    store: &PgStore,
    lang: Language,
    tz: Tz,
) -> HandlerResult {
    let alerts = store.list_active_for_chat(msg.chat.id.0).await?;
    if alerts.is_empty() {
        bot.send_message(msg.chat.id, m::list_empty(lang))
            .parse_mode(ParseMode::Html)
            .await?;
        return Ok(());
    }

    let mut lines = Vec::with_capacity(alerts.len());
    for a in &alerts {
        let scope_icon = match a.scope {
            AlertScope::Private => "🔒",
            AlertScope::Shared => "👥",
        };
        let when = render::format_local_compact(a.fire_at, tz, lang);
        let text = truncate_chars(&a.text, limits::MAX_TEXT_LEN);
        let raw_line = match render::format_recurrence_short(&a.schedule, lang) {
            Some(rec_short) => {
                format!("{scope_icon}🔁 #{} · {when} · {rec_short} · {text}", a.id)
            }
            None => format!("{scope_icon} #{} · {when} · {text}", a.id),
        };
        lines.push(ListLine {
            visible_len: telegram_text_len(&raw_line),
            html: html_escape(&raw_line),
            raw: raw_line,
        });
    }

    let pages = paginate_list(m::list_header(lang), &lines, 4000);
    if pages.len() == 1 {
        bot.send_message(msg.chat.id, &pages[0].html)
            .parse_mode(ParseMode::Html)
            .await?;
    } else {
        // Large lists would fan one inbound command out into many Telegram API
        // calls and can trip the chat-wide send limit. A single text document
        // keeps every line in one call.
        let mut document = String::from(m::list_header(lang));
        for line in &lines {
            document.push('\n');
            document.push_str(&line.raw);
        }
        bot.send_document(
            msg.chat.id,
            InputFile::memory(document.into_bytes()).file_name("alerts.txt"),
        )
        .await?;
    }
    Ok(())
}

async fn cancel(
    bot: &Bot,
    msg: &Message,
    store: &PgStore,
    requester: i64,
    lang: Language,
    id: i64,
) -> HandlerResult {
    let Some(alert) = store.get_alert(id).await? else {
        bot.send_message(msg.chat.id, m::cancel_not_found(lang)).await?;
        return Ok(());
    };
    if alert.chat_id != msg.chat.id.0 {
        // Don't reveal alerts from other chats.
        bot.send_message(msg.chat.id, m::cancel_not_found(lang)).await?;
        return Ok(());
    }
    if !alert.can_edit(requester) {
        bot.send_message(msg.chat.id, m::cancel_not_allowed(lang)).await?;
        return Ok(());
    }
    let ok = store.cancel_alert(id, requester).await?;
    let reply = if ok {
        m::cancel_ok(lang, id)
    } else {
        m::cancel_not_found(lang).to_string()
    };
    bot.send_message(msg.chat.id, reply).await?;
    Ok(())
}

async fn set_tz(
    bot: &Bot,
    msg: &Message,
    store: &PgStore,
    user_id: i64,
    lang: Language,
    zone: &str,
) -> HandlerResult {
    let trimmed = zone.trim();
    let parsed: Result<Tz, _> = trimmed.parse();
    match parsed {
        Ok(tz) => {
            store.set_timezone(user_id, tz).await?;
            bot.send_message(msg.chat.id, m::tz_set(lang, tz.name())).await?;
        }
        Err(_) => {
            bot.send_message(msg.chat.id, m::tz_invalid(lang))
                .parse_mode(ParseMode::Html)
                .await?;
        }
    }
    Ok(())
}

async fn set_lang(
    bot: &Bot,
    msg: &Message,
    store: &PgStore,
    user_id: i64,
    lang_code: &str,
) -> HandlerResult {
    let cur_lang = store
        .get_user(user_id)
        .await?
        .map(|u| u.language)
        .unwrap_or(Language::De);
    let Some(new) = Language::parse(lang_code.trim()) else {
        bot.send_message(msg.chat.id, m::lang_invalid(cur_lang))
            .parse_mode(ParseMode::Html)
            .await?;
        return Ok(());
    };
    store.set_language(user_id, new).await?;
    update_chat_command_menu_if_dm(bot, msg, new).await;
    bot.send_message(msg.chat.id, m::lang_set(new)).await?;
    Ok(())
}

/// Override the per-chat command menu so the slash-picker matches the bot's
/// reply language. Only effective in DMs — group menus stay on the global
/// default because the chat is shared between users with different langs.
async fn update_chat_command_menu_if_dm(bot: &Bot, msg: &Message, lang: Language) {
    if !matches!(msg.chat.kind, ChatKind::Private(_)) {
        return;
    }
    let cmds: Vec<BotCommand> = m::command_menu(lang)
        .into_iter()
        .map(|(n, d)| BotCommand::new(n, d))
        .collect();
    let scope = BotCommandScope::Chat {
        chat_id: Recipient::Id(msg.chat.id),
    };
    if let Err(e) = bot.set_my_commands(cmds).scope(scope).await {
        tracing::warn!(error = ?e, "per-chat set_my_commands failed");
    }
}

// ---------------------------------------------------------------------------
// Callback queries (inline keyboard button clicks)
// ---------------------------------------------------------------------------

pub async fn callback_dispatch(
    bot: Bot,
    query: CallbackQuery,
    store: Arc<PgStore>,
    limiter: Arc<CommandRateLimiter>,
) -> HandlerResult {
    let user_id = query.from.id.0 as i64;
    let detected_lang = Language::from_telegram_code(query.from.language_code.as_deref());

    // Callbacks count against the same per-user budget as commands —
    // otherwise a scripted client could spam +5m on every button forever.
    if !limiter.check(user_id) {
        bot.answer_callback_query(query.id.clone())
            .text(m::rate_limited(detected_lang))
            .show_alert(false)
            .await
            .ok();
        return Ok(());
    }

    let user = store.upsert_user(user_id, detected_lang).await?;

    let data = query.data.as_deref().unwrap_or("");
    let parts: Vec<&str> = data.split(':').collect();
    match parts.as_slice() {
        ["cancel", id_str] => {
            if let Ok(id) = id_str.parse::<i64>() {
                handle_cancel_callback(&bot, &store, &query, user.language, id).await?;
            } else {
                bot.answer_callback_query(query.id.clone()).await?;
            }
        }
        ["snooze", offset_str, id_str] => {
            match (offset_str.parse::<i64>(), id_str.parse::<i64>()) {
                (Ok(offset), Ok(id)) => {
                    handle_snooze_callback(&bot, &store, &query, &user, offset, id).await?;
                }
                _ => {
                    bot.answer_callback_query(query.id.clone()).await?;
                }
            }
        }
        ["stop_series", id_str] => {
            if let Ok(id) = id_str.parse::<i64>() {
                handle_stop_series_callback(&bot, &store, &query, user.language, id).await?;
            } else {
                bot.answer_callback_query(query.id.clone()).await?;
            }
        }
        _ => {
            bot.answer_callback_query(query.id.clone()).await?;
        }
    }
    Ok(())
}

/// Callback payloads are attacker-controlled: any client can send arbitrary
/// `callback_data` (`cancel:123`, `snooze:300:123`, `stop_series:123`) with a
/// guessed alert id — the ids are sequential BIGSERIAL, so trivial to enumerate.
/// The `/cancel` *command* already checks the alert's chat matches the sender's
/// (see `cancel`), but the callback paths did not — and `Alert::can_edit`
/// returns `true` for any `Shared` alert. Only honour a callback if it comes
/// from a message in the alert's own chat. `query.message == None` (very old
/// messages, or forged queries with no origin) → reject.
async fn callback_alert_in_origin_chat(
    store: &PgStore,
    query: &CallbackQuery,
    alert_id: i64,
) -> Result<Option<botcore::Alert>, storage::StorageError> {
    let Some(alert) = store.get_alert(alert_id).await? else {
        return Ok(None);
    };
    let Some(msg) = query.message.as_ref() else {
        return Ok(None);
    };
    if msg.chat().id.0 != alert.chat_id {
        return Ok(None);
    }
    Ok(Some(alert))
}

async fn handle_cancel_callback(
    bot: &Bot,
    store: &PgStore,
    query: &CallbackQuery,
    lang: Language,
    alert_id: i64,
) -> HandlerResult {
    if callback_alert_in_origin_chat(store, query, alert_id)
        .await?
        .is_none()
    {
        bot.answer_callback_query(query.id.clone())
            .text(m::cancel_not_allowed(lang))
            .show_alert(true)
            .await?;
        return Ok(());
    }
    let requester = query.from.id.0 as i64;
    let ok = store.cancel_alert(alert_id, requester).await?;

    if ok {
        if let Some(msg) = query.message.as_ref() {
            bot.edit_message_text(msg.chat().id, msg.id(), m::cancelled_inline(lang, alert_id))
                .await
                .ok();
        }
        bot.answer_callback_query(query.id.clone())
            .text(m::callback_cancelled(lang))
            .await?;
    } else {
        bot.answer_callback_query(query.id.clone())
            .text(m::cancel_not_allowed(lang))
            .show_alert(true)
            .await?;
    }
    Ok(())
}

/// Snooze: build a *new* one-shot alert with the same text in the same chat,
/// fire_at = now + offset. The original alert is never mutated — that's the
/// key design choice so recurring alerts keep their schedule untouched.
/// The new alert is owned by whoever tapped (so they can cancel it).
async fn handle_snooze_callback(
    bot: &Bot,
    store: &PgStore,
    query: &CallbackQuery,
    user: &botcore::User,
    offset_secs: i64,
    alert_id: i64,
) -> HandlerResult {
    let Some(fire_at) = snooze_fire_at(offset_secs, Utc::now()) else {
        bot.answer_callback_query(query.id.clone())
            .text(m::snooze_invalid_offset(user.language))
            .await?;
        return Ok(());
    };
    let Some(original) = callback_alert_in_origin_chat(store, query, alert_id).await? else {
        bot.answer_callback_query(query.id.clone())
            .text(m::snooze_gone(user.language))
            .show_alert(false)
            .await?;
        return Ok(());
    };

    // Snooze creates a new alert, so it counts against both quotas the same
    // way as a fresh /alert command would. The errors get rendered as a
    // callback toast (no separate chat message) since the user is mid-tap.
    let user_count = store.count_active_for_user(user.telegram_id).await?;
    if user_count >= limits::MAX_ALERTS_PER_USER {
        bot.answer_callback_query(query.id.clone())
            .text(m::user_quota_exceeded(
                user.language,
                user_count,
                limits::MAX_ALERTS_PER_USER,
            ))
            .show_alert(true)
            .await?;
        return Ok(());
    }
    let chat_count = store.count_active_for_chat(original.chat_id).await?;
    if chat_count >= limits::MAX_ALERTS_PER_CHAT {
        bot.answer_callback_query(query.id.clone())
            .text(m::chat_quota_exceeded(
                user.language,
                chat_count,
                limits::MAX_ALERTS_PER_CHAT,
            ))
            .show_alert(true)
            .await?;
        return Ok(());
    }

    let new = NewAlert {
        user_id: user.telegram_id,
        chat_id: original.chat_id,
        chat_type: original.chat_type.clone(),
        scope: AlertScope::Private,
        text: original.text.clone(),
        fire_at,
        schedule: Schedule::one_shot(fire_at, original.schedule.tz),
    };
    let created = store.create_alert(new).await?;

    // Strip the buttons off the originally-delivered message so the user
    // can't double-tap +5m and stack snoozes. The freshly-created one-shot
    // will get its own button row when it fires.
    clear_inline_buttons(bot, query).await;

    let when = render::format_local_compact(created.fire_at, user.timezone, user.language);
    let chat_id_for_reply = query
        .message
        .as_ref()
        .map(|m| m.chat().id)
        .unwrap_or(ChatId(original.chat_id));
    bot.send_message(chat_id_for_reply, m::snooze_confirmation(user.language, &when))
        .await?;
    bot.answer_callback_query(query.id.clone()).await?;
    Ok(())
}

/// Callback data is untrusted, even when the originating chat is valid.
fn snooze_fire_at(offset_secs: i64, now: chrono::DateTime<Utc>) -> Option<chrono::DateTime<Utc>> {
    if !matches!(offset_secs, 300 | 900 | 3600) {
        return None;
    }
    now.checked_add_signed(chrono::Duration::seconds(offset_secs))
}

/// Stop the recurring series: reuses the existing `cancel_alert` flow (which
/// enforces `Alert::can_edit`). For shared galert anyone may stop; for private
/// alerts only the creator. Falls back to a `not allowed` toast otherwise.
async fn handle_stop_series_callback(
    bot: &Bot,
    store: &PgStore,
    query: &CallbackQuery,
    lang: Language,
    alert_id: i64,
) -> HandlerResult {
    if callback_alert_in_origin_chat(store, query, alert_id)
        .await?
        .is_none()
    {
        bot.answer_callback_query(query.id.clone())
            .text(m::cancel_not_allowed(lang))
            .show_alert(true)
            .await?;
        return Ok(());
    }
    let requester = query.from.id.0 as i64;
    let ok = store.cancel_alert(alert_id, requester).await?;
    if ok {
        // Strip buttons first — the series is gone, so the +5m / Serie beenden
        // controls on this delivered message would all no-op or surprise the user.
        clear_inline_buttons(bot, query).await;
        if let Some(msg) = query.message.as_ref() {
            bot.send_message(msg.chat().id, m::series_stopped(lang, alert_id))
                .await?;
        }
        bot.answer_callback_query(query.id.clone())
            .text(m::callback_cancelled(lang))
            .await?;
    } else {
        bot.answer_callback_query(query.id.clone())
            .text(m::cancel_not_allowed(lang))
            .show_alert(true)
            .await?;
    }
    Ok(())
}

/// Remove the inline keyboard from the message that originated this callback.
/// Best-effort: silently ignores edit failures (most common one being a
/// message older than Telegram's 48h edit window).
async fn clear_inline_buttons(bot: &Bot, query: &CallbackQuery) {
    if let Some(msg) = query.message.as_ref() {
        let empty: InlineKeyboardMarkup =
            InlineKeyboardMarkup::new(Vec::<Vec<InlineKeyboardButton>>::new());
        bot.edit_message_reply_markup(msg.chat().id, msg.id())
            .reply_markup(empty)
            .await
            .ok();
    }
}

// ---------------------------------------------------------------------------
// Catch-all for messages that weren't a command.
// In DMs we point the user at /help; in groups we silently drop them so the
// bot doesn't pester everyone in the chat.
// ---------------------------------------------------------------------------

pub async fn handle_unhandled(bot: Bot, msg: Message, store: Arc<PgStore>) -> HandlerResult {
    if let Some((from, to)) = chat_migration(&msg) {
        store.migrate_chat(from.0, to.0).await?;
        return Ok(());
    }
    if !matches!(msg.chat.kind, ChatKind::Private(_)) {
        return Ok(());
    }
    if msg.text().is_none() {
        return Ok(());
    }
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0 as i64;
    let detected_lang = Language::from_telegram_code(from.language_code.as_deref());
    let user = store.upsert_user(user_id, detected_lang).await?;

    bot.send_message(msg.chat.id, m::nudge_to_help(user.language))
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Telegram announces a group-to-supergroup upgrade in both chats: the old
/// group gets `migrate_to_chat_id`, the new supergroup `migrate_from_chat_id`.
fn chat_migration(msg: &Message) -> Option<(ChatId, ChatId)> {
    if let Some(to) = msg.migrate_to_chat_id() {
        return Some((msg.chat.id, *to));
    }
    msg.migrate_from_chat_id().map(|from| (*from, msg.chat.id))
}

fn chat_type_from_msg(msg: &Message) -> ChatType {
    match &msg.chat.kind {
        ChatKind::Private(_) => ChatType::Private,
        ChatKind::Public(public) => match public.kind {
            PublicChatKind::Group => ChatType::Group,
            PublicChatKind::Supergroup(_) => ChatType::Supergroup,
            PublicChatKind::Channel(_) => ChatType::Channel,
        },
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[derive(Debug)]
struct ListLine {
    html: String,
    raw: String,
    visible_len: usize,
}

#[derive(Debug)]
struct ListPage {
    html: String,
    visible_len: usize,
}

fn paginate_list(header: &str, lines: &[ListLine], max_chars: usize) -> Vec<ListPage> {
    let mut pages = Vec::new();
    let header_html = format!("<b>{}</b>", html_escape(header));
    let header_len = telegram_text_len(header);
    let mut page = ListPage {
        html: header_html.clone(),
        visible_len: header_len,
    };
    for line in lines {
        let added = 1 + line.visible_len;
        if page.visible_len + added > max_chars && page.visible_len != header_len {
            pages.push(page);
            page = ListPage {
                html: header_html.clone(),
                visible_len: header_len,
            };
        }
        page.html.push('\n');
        page.html.push_str(&line.html);
        page.visible_len += added;
    }
    if page.visible_len != header_len || lines.is_empty() {
        pages.push(page);
    }
    pages
}

fn telegram_text_len(value: &str) -> usize {
    value.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use teloxide::types::InlineKeyboardButtonKind;

    #[test]
    fn snooze_only_accepts_offered_intervals() {
        let now = Utc::now();
        for offset in [i64::MIN, -300, 0, 1, 299, 301, 86400, i64::MAX] {
            assert_eq!(snooze_fire_at(offset, now), None, "offset {offset}");
        }
        for offset in [300, 900, 3600] {
            assert_eq!(snooze_fire_at(offset, now), Some(now + chrono::Duration::seconds(offset)));
        }
        assert_eq!(snooze_fire_at(300, chrono::DateTime::<Utc>::MAX_UTC), None);
    }

    fn callback_data(btn: &InlineKeyboardButton) -> &str {
        match &btn.kind {
            InlineKeyboardButtonKind::CallbackData(s) => s,
            _ => panic!("expected callback button"),
        }
    }

    #[test]
    fn delivery_keyboard_oneshot_has_three_snooze_buttons_only() {
        let kb = delivery_keyboard(Language::De, 42, false);
        assert_eq!(kb.inline_keyboard.len(), 1, "single row expected");
        let row = &kb.inline_keyboard[0];
        assert_eq!(row.len(), 3, "three snooze buttons, no stop_series");
        assert_eq!(callback_data(&row[0]), "snooze:300:42");
        assert_eq!(callback_data(&row[1]), "snooze:900:42");
        assert_eq!(callback_data(&row[2]), "snooze:3600:42");
    }

    #[test]
    fn delivery_keyboard_recurring_adds_stop_series_button() {
        let kb = delivery_keyboard(Language::En, 7, true);
        let row = &kb.inline_keyboard[0];
        assert_eq!(row.len(), 4);
        assert_eq!(callback_data(&row[3]), "stop_series:7");
    }

    fn service_message(chat: serde_json::Value, field: &str, id: i64) -> Message {
        let mut message = serde_json::json!({
            "message_id": 1,
            "date": 0,
            "chat": chat,
            "from": {"id": 42, "is_bot": false, "first_name": "Ada"},
        });
        message[field] = id.into();
        serde_json::from_str(&message.to_string()).unwrap()
    }

    #[test]
    fn chat_migration_is_read_from_both_service_messages() {
        let old = service_message(
            serde_json::json!({"id": -100, "type": "group", "title": "Team"}),
            "migrate_to_chat_id",
            -1_001_234,
        );
        assert_eq!(
            chat_migration(&old),
            Some((ChatId(-100), ChatId(-1_001_234)))
        );
        let new = service_message(
            serde_json::json!({"id": -1_001_234, "type": "supergroup", "title": "Team"}),
            "migrate_from_chat_id",
            -100,
        );
        assert_eq!(
            chat_migration(&new),
            Some((ChatId(-100), ChatId(-1_001_234)))
        );
    }

    #[test]
    fn list_pages_stay_below_telegram_limit_and_keep_lines_whole() {
        let lines: Vec<ListLine> = (0..50)
            .map(|i| {
                let raw = format!("#{i} {}", "&".repeat(500));
                ListLine {
                    visible_len: telegram_text_len(&raw),
                    html: html_escape(&raw),
                    raw,
                }
            })
            .collect();
        let pages = paginate_list("Header", &lines, 4000);
        assert!(pages.len() > 1);
        assert!(
            pages.len() <= 8,
            "escaped entities must not amplify page count"
        );
        assert!(pages.iter().all(|page| page.visible_len <= 4000));
        for line in &lines {
            assert_eq!(
                pages
                    .iter()
                    .filter(|page| page.html.contains(&line.html))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn truncation_happens_before_html_expansion() {
        let raw = format!("{}tail", "&".repeat(limits::MAX_TEXT_LEN));
        let escaped = html_escape(&truncate_chars(&raw, limits::MAX_TEXT_LEN));
        assert_eq!(escaped, "&amp;".repeat(limits::MAX_TEXT_LEN));
    }

    #[test]
    fn list_limit_counts_non_bmp_characters_as_telegram_does() {
        let raw_lines = ["🔒".repeat(1_999), "last".into()];
        let lines: Vec<ListLine> = raw_lines
            .iter()
            .map(|raw| ListLine {
                html: html_escape(raw),
                raw: raw.to_string(),
                visible_len: telegram_text_len(raw),
            })
            .collect();
        let pages = paginate_list("h", &lines, 4000);
        assert_eq!(pages.len(), 2);
        assert!(pages.iter().all(|page| page.visible_len <= 4000));
    }
}
