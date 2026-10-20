use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide::dispatching::ShutdownToken;
use teloxide::update_listeners::webhooks;
use teloxide::prelude::*;
use teloxide::types::{BotCommand, Me, UpdateKind};
use teloxide::utils::command::BotCommands;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use url::Url;

use storage::PgStore;

mod admin;
mod commands;
mod handlers;
mod identity;
mod limits;
mod messages;
mod render;
mod stats;
mod worker;

use admin::AdminNotifier;
use commands::Command;
use limits::{CommandRateLimiter, RateLimit};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DISPATCH_SHARDS: i64 = 16;
const DISPATCH_WORKER_QUEUE_SIZE: usize = 8;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();

    let config = Config::from_env()?;
    info!(?config.transport, "starting alert-bot");

    let store = Arc::new(PgStore::connect(&config.database_url, 10).await?);
    store.migrate().await?;
    info!("migrations applied");

    let bot = Bot::with_client(&config.bot_token, build_http_client()?);
    register_command_menus(&bot).await;

    let notifier = AdminNotifier::new(bot.clone(), config.admin_chat_id);
    let transport_label = match config.transport {
        Transport::Webhook { .. } => "webhook",
        Transport::Polling => "polling",
    };
    notifier
        .notify(&format!(
            "✅ <b>alert-bot v{VERSION}</b> gestartet · transport={transport_label}"
        ))
        .await;

    let shutdown = CancellationToken::new();

    let worker_handle = {
        let bot = bot.clone();
        let store = store.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move { worker::run(bot, store, shutdown).await })
    };

    // Opt-in read-only stats endpoint (Homepage widget). Separate axum server
    // on its own LAN-only port — doesn't touch the webhook listener below.
    if let Some(addr) = config.stats_listen {
        let store = store.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = stats::run(store, addr, shutdown).await {
                tracing::error!(error = ?e, "stats server exited with error");
            }
        });
    }

    // Single shared rate limiter — DashMap-backed, cheap to clone (it's an Arc
    // internally). One instance per process; restart resets the window.
    let rate_limiter = Arc::new(CommandRateLimiter::new());

    let handler = dptree::entry()
        // Reject unsupported update kinds, and commands or callbacks from
        // exhausted users, before the persistent deduplication write.
        // Otherwise edited/channel updates or a command flood can grow
        // processed_updates without reaching a rate-limited handler.
        .filter_async(|update: Update, bot: Bot, me: Me, store: Arc<PgStore>, limiter: Arc<CommandRateLimiter>| async move {
            let Some(user_id) = ingress_user_id(&update) else {
                return false;
            };
            if is_rate_limited(&update, &me) {
                match limiter.check(user_id) {
                    RateLimit::Allowed => {}
                    // Rate-limited commands stay silent, but an unanswered
                    // callback leaves the button's loading indicator spinning.
                    RateLimit::UserBudgetExhausted => {
                        if let UpdateKind::CallbackQuery(query) = &update.kind {
                            handlers::answer_rate_limited_callback(&bot, query).await;
                        }
                        return false;
                    }
                    // Forged updates with fresh IDs reach the shared ceilings
                    // in bulk; answering them would cost one outbound call each.
                    RateLimit::SharedCeiling => return false,
                }
            }

            // Telegram retries webhook deliveries on timeout. Drop duplicates
            // by update_id. On a deduplication error, continue rather than
            // silently losing an otherwise valid update.
            match store.try_register_update(update.id.0 as i64).await {
                Ok(fresh) => fresh,
                Err(e) => {
                    tracing::error!(error = ?e, update_id = update.id.0, "try_register_update failed");
                    true
                }
            }
        })
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handlers::dispatch),
        )
        .branch(Update::filter_callback_query().endpoint(handlers::callback_dispatch))
        .branch(Update::filter_message().endpoint(handlers::handle_unhandled));

    // No `.enable_ctrlc_handler()` — that one only listens for SIGINT.
    // We install our own handler below that covers both SIGTERM and SIGINT
    // and feeds the dispatcher's ShutdownToken on either.
    let mut dispatcher = Dispatcher::builder(bot.clone(), handler)
        .dependencies(dptree::deps![store.clone(), rate_limiter.clone()])
        // Teloxide otherwise creates one worker and queue per sender-chosen
        // chat id. Fixed shards preserve per-chat ordering while bounding the
        // total workers and making webhook backpressure reach the HTTP queue.
        .distribution_function(dispatch_shard)
        .worker_queue_size(DISPATCH_WORKER_QUEUE_SIZE)
        // Root filters intentionally drop unsupported and rate-limited input.
        // Teloxide's default handler logs the complete Debug update, including
        // user text, so a rejected flood would also flood the journal.
        .default_handler(|_| async {})
        .build();

    spawn_signal_handler(
        shutdown.clone(),
        dispatcher.shutdown_token(),
        notifier.clone(),
    );

    let dispatch = async {
        match config.transport {
            Transport::Webhook {
                listen,
                url,
                secret,
            } => {
                let mut options = webhooks::Options::new(listen, url);
                if let Some(s) = secret {
                    options = options.secret_token(s);
                }
                let listener = webhooks::axum(bot.clone(), options)
                    .await
                    .context("setting up webhook listener")?;
                dispatcher
                    .dispatch_with_listener(
                        listener,
                        LoggingErrorHandler::with_custom_text("webhook listener error"),
                    )
                    .await;
            }
            Transport::Polling => {
                // Local-dev fallback. Requires no public endpoint.
                dispatcher.dispatch().await;
            }
        }
        Ok(())
    };
    let result = supervise(dispatch, worker_handle, shutdown).await;
    if let Err(e) = &result {
        tracing::error!(error = ?e, "bot stopped after task failure");
        notifier
            .notify(&format!(
                "💥 alert-bot failed: {}",
                teloxide::utils::html::escape(&format!("{e:#}"))
            ))
            .await;
    }
    notifier.notify("🛑 alert-bot stopped").await;
    result
}

fn ingress_user_id(update: &Update) -> Option<i64> {
    match &update.kind {
        UpdateKind::Message(message)
            if message.sender_chat.is_none() && !is_direct_messages_chat_id(message.chat.id.0) =>
        {
            message
                .from
                .as_ref()
                .and_then(|user| identity::user_id(user.id.0))
        }
        UpdateKind::CallbackQuery(query)
            if query
                .message
                .as_ref()
                .is_none_or(|message| !is_direct_messages_chat_id(message.chat().id.0)) =>
        {
            identity::user_id(query.from.id.0)
        }
        _ => None,
    }
}

/// Only commands for this bot and callbacks use the rate limit, as before.
/// Other group messages are ignored by the handlers and must not consume a
/// member's or the global budget.
fn is_rate_limited(update: &Update, me: &Me) -> bool {
    match &update.kind {
        UpdateKind::CallbackQuery(_) => true,
        UpdateKind::Message(message) => message
            .text()
            .is_some_and(|text| Command::parse(text, me.username()).is_ok()),
        _ => false,
    }
}

fn is_direct_messages_chat_id(chat_id: i64) -> bool {
    (-4_000_000_000_000..=-2_002_147_483_649).contains(&chat_id)
}

fn dispatch_shard(update: &Update) -> Option<u8> {
    let key = update
        .chat()
        .map(|chat| chat.id.0)
        .or_else(|| ingress_user_id(update))
        .unwrap_or(i64::from(update.id.0));
    Some(key.rem_euclid(DISPATCH_SHARDS) as u8)
}

/// Observe the worker while dispatching. A dead delivery task must fail the
/// process so the service manager can restart it. During a requested shutdown,
/// let both tasks finish their in-flight work.
async fn supervise(
    dispatch: impl std::future::Future<Output = Result<()>>,
    mut worker: tokio::task::JoinHandle<Result<()>>,
    shutdown: CancellationToken,
) -> Result<()> {
    tokio::pin!(dispatch);
    tokio::select! {
        result = &mut dispatch => {
            shutdown.cancel();
            let worker_result = worker.await.context("worker task failed")?;
            result?;
            worker_result.context("worker exited with error")
        }
        result = &mut worker => {
            let stopping = shutdown.is_cancelled();
            shutdown.cancel();
            result.context("worker task failed")?.context("worker exited with error")?;
            if !stopping {
                anyhow::bail!("worker exited unexpectedly");
            }
            dispatch.await
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

// No Debug: holds BOT_TOKEN and DATABASE_URL.
struct Config {
    bot_token: String,
    database_url: String,
    transport: Transport,
    admin_chat_id: Option<i64>,
    /// Optional LAN-only address for the read-only `/stats` endpoint. `None`
    /// (env unset) means the stats server doesn't start at all.
    stats_listen: Option<SocketAddr>,
}

enum Transport {
    Webhook {
        listen: SocketAddr,
        url: Url,
        secret: Option<String>,
    },
    Polling,
}

// Hand-written so the startup log never prints WEBHOOK_SECRET: the deploy
// job tails the journal into public GitHub Actions logs.
impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Webhook { listen, url, secret } => f
                .debug_struct("Webhook")
                .field("listen", listen)
                .field("url", &url.as_str())
                .field("secret", &secret.as_ref().map(|_| "<redacted>"))
                .finish(),
            Transport::Polling => f.write_str("Polling"),
        }
    }
}

impl Config {
    fn from_env() -> Result<Self> {
        let bot_token = std::env::var("BOT_TOKEN").context("BOT_TOKEN is required")?;
        let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;

        let transport = match std::env::var("WEBHOOK_URL").ok().filter(|s| !s.is_empty()) {
            None => Transport::Polling,
            Some(url) => {
                let url: Url = url.parse().context("WEBHOOK_URL is not a valid URL")?;
                let listen: SocketAddr = std::env::var("WEBHOOK_LISTEN")
                    .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
                    .parse()
                    .context("WEBHOOK_LISTEN is not a valid socket address")?;
                let secret = std::env::var("WEBHOOK_SECRET").ok().filter(|s| !s.is_empty());
                Transport::Webhook { listen, url, secret }
            }
        };

        let admin_chat_id = match std::env::var("ADMIN_CHAT_ID").ok().filter(|s| !s.is_empty()) {
            None => None,
            Some(s) => Some(
                s.parse::<i64>()
                    .context("ADMIN_CHAT_ID must be a numeric Telegram chat id")?,
            ),
        };

        let stats_listen = match std::env::var("STATS_LISTEN").ok().filter(|s| !s.is_empty()) {
            None => None,
            Some(s) => Some(
                s.parse::<SocketAddr>()
                    .context("STATS_LISTEN is not a valid socket address")?,
            ),
        };

        Ok(Self {
            bot_token,
            database_url,
            transport,
            admin_chat_id,
            stats_listen,
        })
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,alert_bot=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();
}

/// Reqwest client tuned for outbound HTTPS to api.telegram.org.
///
/// Three knobs, in order of how much they actually matter:
///
/// 1. **Force IPv4** via `local_address(0.0.0.0)`. Our LXC has a broken
///    IPv6 path — `getaddrinfo` returns the AAAA first, hyper tries it,
///    and the 5s `connect_timeout` expires before the IPv4 fallback gets
///    a turn (hyper 0.14 applies `connect_timeout` to the *entire* address
///    iteration, not per attempt). Symptom was `hyper::Error(Connect,
///    TimedOut)` on the first request after any idle period.
/// 2. **TCP keepalive** so half-dead pooled connections get detected
///    proactively instead of on the next user-facing send.
/// 3. **Short `pool_idle_timeout`** so connections recycle before any
///    NAT/conntrack timeouts on the path can silently kill them.
fn build_http_client() -> anyhow::Result<reqwest::Client> {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;
    let client = reqwest::Client::builder()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(17))
        .tcp_nodelay(true)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .pool_idle_timeout(Some(Duration::from_secs(60)))
        .build()
        .context("building reqwest client for teloxide")?;
    Ok(client)
}

/// Register one command list per supported language plus an English default
/// for any user Telegram doesn't have an explicit translation for.
async fn register_command_menus(bot: &Bot) {
    let to_bot_commands = |lang| {
        messages::command_menu(lang)
            .into_iter()
            .map(|(n, d)| BotCommand::new(n, d))
            .collect::<Vec<_>>()
    };

    let de = to_bot_commands(botcore::Language::De);
    let en = to_bot_commands(botcore::Language::En);

    let (default, en_res, de_res) = tokio::join!(
        bot.set_my_commands(en.clone()).send(),
        bot.set_my_commands(en).language_code("en").send(),
        bot.set_my_commands(de).language_code("de").send(),
    );
    if let Err(e) = default {
        tracing::warn!(error = ?e, "set_my_commands (default) failed");
    }
    if let Err(e) = en_res {
        tracing::warn!(error = ?e, "set_my_commands (en) failed");
    }
    if let Err(e) = de_res {
        tracing::warn!(error = ?e, "set_my_commands (de) failed");
    }
}

fn spawn_signal_handler(
    shutdown: CancellationToken,
    dispatcher_token: ShutdownToken,
    notifier: AdminNotifier,
) {
    tokio::spawn(async move {
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = ?e, "failed to install SIGTERM handler");
                return;
            }
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = ?e, "failed to install SIGINT handler");
                return;
            }
        };
        let signal_name = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
        };
        info!("{signal_name} received");
        notifier
            .notify(&format!("🛑 {signal_name} received, shutting down"))
            .await;

        // Tell the teloxide dispatcher to stop accepting updates. Without
        // this, `Dispatcher::dispatch{,_with_listener}` ignores SIGTERM
        // (teloxide's built-in ctrlc handler only catches SIGINT), so
        // systemd has to SIGKILL us after TimeoutStopSec — see the May 11
        // production log where a `systemctl restart` hung for 90s before
        // SIGKILL fired and the new process came up.
        if let Err(e) = dispatcher_token.shutdown() {
            tracing::warn!(error = ?e, "dispatcher shutdown failed (already idle?)");
        }
        // Wake the worker too. The dispatcher's awaiter in main() will
        // resolve on its own once `shutdown()` above propagates.
        shutdown.cancel();
    });
}

#[cfg(test)]
mod supervision_tests {
    use super::*;
    use std::future::pending;
    use std::time::Duration;

    fn message_update(chat_id: i64, chat_type: &str) -> Update {
        serde_json::from_str(&format!(
            r#"{{
                "update_id": 1,
                "message": {{
                    "message_id": 1,
                    "date": 1,
                    "chat": {{ "id": {chat_id}, "type": "{chat_type}" }},
                    "from": {{ "id": 7, "is_bot": false, "first_name": "Ada" }},
                    "text": "/help"
                }}
            }}"#
        ))
        .unwrap()
    }

    fn callback_update(chat_id: i64) -> Update {
        serde_json::from_str(&format!(
            r#"{{
                "update_id": 2,
                "callback_query": {{
                    "id": "callback-id",
                    "from": {{ "id": 7, "is_bot": false, "first_name": "Ada" }},
                    "chat_instance": "instance-id",
                    "message": {{
                        "message_id": 2,
                        "date": 1,
                        "chat": {{ "id": {chat_id}, "type": "supergroup" }},
                        "text": "alert"
                    }},
                    "data": "cancel:1"
                }}
            }}"#
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn worker_failures_interrupt_a_running_dispatcher() {
        for outcome in 0..3 {
            let shutdown = CancellationToken::new();
            let worker = tokio::spawn(async move {
                match outcome {
                    0 => anyhow::bail!("listener connection failed"),
                    1 => panic!("worker panic"),
                    _ => Ok(()),
                }
            });
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                supervise(pending(), worker, shutdown.clone()),
            )
            .await
            .unwrap();
            assert!(result.is_err());
            assert!(shutdown.is_cancelled());
        }
    }

    #[tokio::test]
    async fn dispatcher_exit_cancels_and_joins_worker() {
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let worker = tokio::spawn(async move {
            token.cancelled().await;
            Ok(())
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            supervise(async { Ok(()) }, worker, shutdown),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn requested_shutdown_waits_for_dispatcher_to_drain() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let worker = tokio::spawn(async { Ok(()) });
        let (done, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(supervise(
            async {
                receiver.await?;
                Ok(())
            },
            worker,
            shutdown,
        ));
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        done.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn dispatcher_uses_a_fixed_number_of_shards() {
        let first: Update = serde_json::from_str(
            r#"{"update_id":1,"message":{"message_id":1,"date":0,"chat":{"id":-42,"type":"group"},"text":"/help"}}"#,
        )
        .unwrap();
        let second: Update = serde_json::from_str(
            r#"{"update_id":2,"message":{"message_id":2,"date":0,"chat":{"id":-42,"type":"group"},"text":"/list"}}"#,
        )
        .unwrap();
        let future: Update =
            serde_json::from_str(r#"{"update_id":4294967295,"future_update":{}}"#).unwrap();

        assert_eq!(dispatch_shard(&first), dispatch_shard(&second));
        for update in [&first, &second, &future] {
            assert!(dispatch_shard(update).unwrap() < DISPATCH_SHARDS as u8);
        }
    }

    #[test]
    fn direct_messages_chat_id_range_is_rejected_at_both_boundaries() {
        for chat_id in [-4_000_000_000_000, -2_002_147_483_649] {
            assert_eq!(
                ingress_user_id(&message_update(chat_id, "supergroup")),
                None
            );
        }
    }

    #[test]
    fn chat_ids_just_outside_direct_messages_range_are_preserved() {
        for chat_id in [-4_000_000_000_001, -2_002_147_483_648] {
            assert_eq!(
                ingress_user_id(&message_update(chat_id, "supergroup")),
                Some(7)
            );
        }
    }

    #[test]
    fn ordinary_private_group_and_supergroup_messages_are_preserved() {
        for (chat_id, chat_type) in [
            (7, "private"),
            (-42, "group"),
            (-1_000_000_000_001, "supergroup"),
        ] {
            assert_eq!(
                ingress_user_id(&message_update(chat_id, chat_type)),
                Some(7)
            );
        }
    }

    #[test]
    fn sender_chat_messages_remain_rejected() {
        let update: Update = serde_json::from_str(
            r#"{
                "update_id": 3,
                "message": {
                    "message_id": 3,
                    "date": 1,
                    "chat": { "id": -42, "type": "group" },
                    "from": { "id": 7, "is_bot": false, "first_name": "Ada" },
                    "sender_chat": { "id": -100, "type": "channel" },
                    "text": "/help"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(ingress_user_id(&update), None);
    }

    fn me() -> Me {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "is_bot": true,
            "first_name": "Alert",
            "username": "alert_bot",
            "can_join_groups": true,
            "can_read_all_group_messages": false,
            "supports_inline_queries": false,
            "can_connect_to_business": false,
            "has_main_web_app": false
        }))
        .unwrap()
    }

    fn text_update(text: &str) -> Update {
        // Update deserializes reliably only from text, not from a Value.
        serde_json::from_str(
            &serde_json::json!({
            "update_id": 4,
            "message": {
                "message_id": 4,
                "date": 1,
                "chat": { "id": -42, "type": "group" },
                "from": { "id": 7, "is_bot": false, "first_name": "Ada" },
                "text": text
            }
        })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn only_commands_and_callbacks_use_the_rate_limit() {
        let me = me();
        assert!(is_rate_limited(&message_update(-42, "group"), &me));
        assert!(is_rate_limited(&text_update("/list@alert_bot"), &me));
        assert!(is_rate_limited(&callback_update(-42), &me));
        assert!(!is_rate_limited(&text_update("good morning"), &me));
        assert!(!is_rate_limited(&text_update("/list@other_bot"), &me));
    }

    #[test]
    fn callbacks_from_direct_messages_chats_are_rejected() {
        assert_eq!(ingress_user_id(&callback_update(-2_002_147_483_649)), None);
        assert_eq!(
            ingress_user_id(&callback_update(-1_000_000_000_001)),
            Some(7)
        );
    }
}
