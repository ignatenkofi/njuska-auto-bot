//! Telegram command dispatcher — the "incoming" side of the bot in v2.
//!
//! Long-polls `getUpdates` via teloxide, routes recognised commands to
//! handler functions. The poll loop in [`crate::bot`] keeps doing its thing
//! in parallel; both share the same [`RuntimeConfig`] via `Arc<RwLock<…>>`.
//!
//! ## Module layout
//!
//! Split per the ~300-line rule (#24):
//!
//! * `mod.rs` (this file) — [`Command`] enum, [`CommandContext`],
//!   [`run_command_loop`], and the two routing endpoints
//!   (`handle_command`, `handle_callback`).
//! * `catalog` — pure data: brands, models, body types, preset ranges.
//! * `keyboards` — inline-keyboard builders + callback-data constants.
//! * `handlers` — per-command handlers, `apply_*` state changes, formatters.
//!
//! ## Authorization
//!
//! Anyone who finds the bot can talk to it. We **silently drop** messages
//! from non-authorized users (logged at `warn` level so probing shows up
//! in the operator log). A "you're not authorized" reply would leak the
//! existence of the bot to bystanders.

mod catalog;
mod handlers;
mod keyboards;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use teloxide::Bot;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardMarkup, ParseMode};
use teloxide::utils::command::BotCommands;
use tokio::sync::{Notify, RwLock};
use tracing::{info, warn};

use crate::config::{MIN_POLL_INTERVAL_SECS, RuntimeConfig, StaticConfig};
use crate::signals::shutdown_signal;
use crate::storage::Storage;

use catalog::{PRICE_RANGES, YEAR_RANGES, models_for_brand};
use handlers::{
    apply_brand, apply_chassis, apply_gearbox, apply_interval, apply_models, apply_price_range,
    apply_reset_all, apply_year_range, format_cancel, format_filter_menu_body, format_filter_ru,
    format_start, format_status, handle_clear, handle_clear_confirm, handle_diag, handle_dump,
    handle_filter_start, handle_interval, handle_pause, handle_resume, handle_set_brand,
    models_picker_title,
};
use keyboards::{
    CB_FILTER_BRAND_CLEAR, CB_FILTER_BRAND_CUSTOM_HINT, CB_FILTER_BRAND_PICKER,
    CB_FILTER_BRAND_SET_PREFIX, CB_FILTER_CHASSIS_PICKER, CB_FILTER_CHASSIS_SAVE,
    CB_FILTER_CHASSIS_TOGGLE_PREFIX, CB_FILTER_DONE, CB_FILTER_GEARBOX_PICKER,
    CB_FILTER_GEARBOX_SAVE, CB_FILTER_GEARBOX_TOGGLE_PREFIX, CB_FILTER_INTERVAL_PICKER,
    CB_FILTER_INTERVAL_SET_PREFIX, CB_FILTER_MENU, CB_FILTER_MODELS_PICKER, CB_FILTER_MODELS_SAVE,
    CB_FILTER_MODELS_TOGGLE_PREFIX, CB_FILTER_PRICE_PICKER, CB_FILTER_RANGE_SET_PREFIX,
    CB_FILTER_RESET_APPLY, CB_FILTER_RESET_CONFIRM, CB_FILTER_TODO, CB_FILTER_YEAR_PICKER,
    brand_picker_keyboard, chassis_picker_keyboard, filter_menu_keyboard, gearbox_picker_keyboard,
    interval_picker_keyboard, model_picker_keyboard, range_picker_keyboard, reset_confirm_keyboard,
};

/// Commands the bot understands.
///
/// `#[derive(BotCommands)]` generates: a `Command::parse` that matches incoming
/// `/foo` strings → variants, **and** `Command::descriptions()` which renders
/// the `/help` text (using `description` attributes below).
///
/// `rename_rule = "lowercase"` means `Status` enum variant ↔ `/status` command.
// **Gotcha to remember**: teloxide's `BotCommands` derive treats any `///`
// doc-comment on a variant as the **command description** for the bot's /help
// menu, *overriding* `#[command(description = "...")]`. We learned this the
// hard way — a long developer-facing docstring on `Interval` pushed the
// generated description over Telegram's 256-byte cap for setMyCommands.
//
// Rule of thumb: keep `///` doc-comments OFF Command variants. Put dev notes
// in regular `//` comments outside the enum, like this one.
//
// Also: `rename_rule = "snake_case"` (vs "lowercase") makes `ClearConfirm`
// generate the command name `/clear_confirm` rather than the wordsmushed
// `/clearconfirm`. Cosmetic but more readable.
#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "snake_case", description = "Команды NjuskaAutoBot:")]
pub enum Command {
    #[command(description = "Приветствие и краткая справка.")]
    Start,
    #[command(description = "Список команд.")]
    Help,
    #[command(description = "Текущая конфигурация и состояние.")]
    Status,
    #[command(description = "Приостановить поллинг.")]
    Pause,
    #[command(description = "Возобновить поллинг.")]
    Resume,
    #[command(description = "Интервал поллинга в секундах (≥60). Пример: /interval 300")]
    Interval(u64),
    #[command(description = "Подготовить очистку истории.")]
    Clear,
    #[command(description = "Подтвердить очистку (30 сек после /clear).")]
    ClearConfirm,
    #[command(description = "Настроить фильтры поиска через диалог.")]
    Filter,
    #[command(description = "Установить марку вручную (slug). Пример: /setbrand smart")]
    SetBrand(String),
    #[command(description = "Показать последние N сохранённых объявлений (1-25). Пример: /dump 10")]
    Dump(u32),
    #[command(description = "Пояснение: команд без режима не нужно отменять.")]
    Cancel,
    #[command(description = "Разовая проверка фетча: сеть, прокси, парсер.")]
    Diag,
    #[command(description = "Версия бота (crate + git SHA).")]
    Version,
}

/// How long after `/clear` a matching `/clear_confirm` is still accepted.
/// After this, the user has to start over — protects against forgotten-but-
/// confirmed-later scenarios.
const CLEAR_CONFIRM_WINDOW: Duration = Duration::from_secs(30);

/// Commands surfaced in the Telegram client's `/`-autocomplete menu, in
/// display order. Deliberately a small subset (#8): a new user opening the
/// bot should see the daily-driver commands, not a wall of fourteen.
/// Everything else (`/dump`, `/diag`, `/clear`, …) still works when typed —
/// `setMyCommands` only controls the menu, not parsing — and stays
/// discoverable via `/help`.
const MENU_COMMANDS: &[&str] = &["filter", "status", "pause", "resume", "help"];

/// The curated `setMyCommands` payload: [`MENU_COMMANDS`] resolved against
/// the derive-generated full list, so descriptions never drift from the
/// `#[command(description = …)]` attributes. Silently skipping a typo'd
/// name here would shrink the menu unnoticed — the unit test pins the count.
fn menu_commands() -> Vec<teloxide::types::BotCommand> {
    let all = Command::bot_commands();
    MENU_COMMANDS
        .iter()
        .filter_map(|name| {
            all.iter()
                .find(|c| c.command.trim_start_matches('/') == *name)
                .cloned()
        })
        .collect()
}

/// State each command handler needs. Cloned per handler invocation (cheap
/// — every field is `Arc<…>` or the bot, which has internal `Arc`).
#[derive(Clone)]
pub struct CommandContext {
    pub static_cfg: Arc<StaticConfig>,
    pub runtime: Arc<RwLock<RuntimeConfig>>,
    pub storage: Arc<Storage>,
    /// Wakes the poll loop's sleep when a runtime setting changes
    /// (poll_interval, search filter, …). Without this nudge, a change made
    /// during a long sleep wouldn't be observed until the sleep finished.
    /// Notify is edge-triggered: one `notify_one()` wakes one waiter (or
    /// "queues" the signal if no one's waiting yet, so the next `notified()`
    /// returns immediately) — perfectly matched to our "change should be
    /// noticed at most once" semantic.
    pub runtime_changed: Arc<Notify>,
    /// In-flight chassis selections during the `/filter → Кузов` flow,
    /// keyed by the sender's user id (#9): with several authorized users,
    /// two people toggling at once must not corrupt each other's picks.
    /// Absent key = picker isn't open for that user; present = toggling.
    /// The entry is removed on Save (after persisting) or on Back/menu.
    pub chassis_draft: Arc<Mutex<HashMap<i64, Vec<u32>>>>,
    /// In-flight model selections. Same shape as `chassis_draft` but with
    /// `String` slugs (since model slugs are textual).
    pub models_draft: Arc<Mutex<HashMap<i64, Vec<String>>>>,
    /// In-flight gearbox selections (#7). Same shape and lifecycle as
    /// `chassis_draft` — numeric codes, per-user, discarded on Back.
    pub gearbox_draft: Arc<Mutex<HashMap<i64, Vec<u32>>>>,
    /// Timestamp of the most recent `/clear` that's still awaiting confirmation.
    /// `None` means no pending request. `Some(t)` is valid for
    /// `CLEAR_CONFIRM_WINDOW` after `t`; afterwards the next `/clear_confirm`
    /// treats it as stale.
    ///
    /// Plain `std::sync::Mutex` (not `tokio::sync::Mutex`) — we never hold
    /// it across `.await`, and ops are nanoseconds.
    pub pending_clear: Arc<Mutex<Option<Instant>>>,
}

/// Run the command listener until shutdown signal.
///
/// teloxide's `Dispatcher::dispatch` runs forever on its own; we wrap it
/// in a `select!` against [`shutdown_signal`] and use the dispatcher's own
/// `shutdown_token` to drain gracefully.
pub async fn run_command_loop(bot: Bot, ctx: CommandContext) -> Result<()> {
    info!("command listener starting");

    // Set the bot's `/`-autocomplete menu in the Telegram client. Curated
    // subset only (#8) — every command still *parses*, the menu is just the
    // storefront. The `setMyCommands` call is idempotent and best-effort;
    // failure isn't fatal — the bot still works without the menu, just no
    // auto-complete in the TG client.
    if let Err(e) = bot.set_my_commands(menu_commands()).await {
        warn!(error = ?e, "couldn't register /help menu (continuing anyway)");
    }

    // The handler tree has **three branches**:
    //   1. Update is a Message AND parses as a `Command` → `handle_command`.
    //   2. Update is a Message that *looks* like a command (starts with `/`)
    //      but didn't parse — wrong args (`/interval abc`), missing args
    //      (bare `/dump`), or a typo'd name → `handle_unparsed_command` (#8).
    //      Without this branch such messages fell through to teloxide's
    //      "Unhandled update" warning and the user got no reply at all.
    //   3. Update is a CallbackQuery (inline-keyboard tap) → `handle_callback`.
    //
    // teloxide tries them in order; the first one that matches wins.
    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handle_command),
        )
        .branch(
            Update::filter_message()
                .filter(|msg: Message| msg.text().is_some_and(|t| t.starts_with('/')))
                .endpoint(handle_unparsed_command),
        )
        .branch(Update::filter_callback_query().endpoint(handle_callback));

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![ctx])
        .build();

    // `shutdown_token()` returns a handle we can use from anywhere to ask the
    // dispatcher to stop accepting new updates and finish in-flight ones.
    let shutdown_token = dispatcher.shutdown_token();

    tokio::select! {
        () = dispatcher.dispatch() => {
            // Dispatcher exited on its own — usually means a network error
            // tearing down the long-poll. Returning Err (not Ok) lets main
            // tell "task died" apart from "clean signal shutdown" and exit
            // non-zero so systemd's Restart=on-failure kicks in (#14).
            warn!("command dispatcher exited unexpectedly");
            return Err(anyhow::anyhow!("command dispatcher exited unexpectedly"));
        }
        _ = shutdown_signal() => {
            info!("command listener received shutdown signal");
            if let Err(e) = shutdown_token.shutdown() {
                warn!(error = ?e, "couldn't trigger dispatcher shutdown");
            }
        }
    }
    info!("command listener stopped");
    Ok(())
}

/// Single endpoint for every recognised command — we dispatch on the
/// `Command` variant inside. teloxide returns `ResponseResult<()>` here,
/// which is `Result<(), teloxide::RequestError>` — `?` after `bot.send_message`
/// just works.
async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    ctx: CommandContext,
) -> ResponseResult<()> {
    // Authorisation: ignore commands from anyone not on the configured list.
    // We compare against the *sender's* user id, not chat id — chat id may be
    // a channel/group (impersonal), but the human pressing the command always
    // has a stable personal user id.
    let user_id: Option<i64> = msg.from.as_ref().map(|u| u.id.0 as i64);
    let authorized = user_id.is_some_and(|id| ctx.static_cfg.is_authorized(id));
    if !authorized {
        warn!(
            ?user_id,
            chat_id = msg.chat.id.0,
            cmd = ?cmd,
            "unauthorized command attempt; ignoring"
        );
        // Silent reject — see module docs.
        return Ok(());
    }

    // Trace every accepted command so we can debug "did the command reach us?"
    // independently of whether the handler ends up changing any state. Without
    // this, read-only commands (/start, /status, /help) leave no log trail at all.
    info!(?cmd, user_id = ?user_id, "command received");

    let reply: String = match cmd {
        Command::Start => format_start(),
        Command::Help => Command::descriptions().to_string(),
        Command::Status => format_status(&ctx).await,
        Command::Pause => handle_pause(&ctx).await,
        Command::Resume => handle_resume(&ctx).await,
        Command::Interval(secs) => handle_interval(&ctx, secs).await,
        Command::Clear => handle_clear(&ctx),
        Command::ClearConfirm => handle_clear_confirm(&ctx).await,
        Command::Filter => {
            // /filter has a different reply shape than the rest — it sends a
            // message with an inline keyboard. We do that directly here
            // (rather than returning a String like other handlers) so
            // `handle_filter_start` controls both text *and* markup.
            return handle_filter_start(bot, &msg, &ctx).await;
        }
        Command::SetBrand(slug) => handle_set_brand(&ctx, slug).await,
        Command::Dump(n) => handle_dump(&ctx, n).await,
        Command::Cancel => format_cancel(),
        Command::Diag => handle_diag(&ctx).await,
        Command::Version => format!("🤖 NjuskaAutoBot <b>{}</b>", crate::version::VERSION),
    };

    bot.send_message(msg.chat.id, reply)
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

/// Endpoint for messages that look like a command but failed `Command::parse`
/// (#8): wrong or missing arguments, or an unknown name. Replies with a short
/// usage hint instead of the previous silence.
///
/// Same authorization posture as `handle_command` — strangers get the silent
/// drop, not a hint that the bot is alive.
async fn handle_unparsed_command(
    bot: Bot,
    msg: Message,
    ctx: CommandContext,
) -> ResponseResult<()> {
    let user_id: Option<i64> = msg.from.as_ref().map(|u| u.id.0 as i64);
    let authorized = user_id.is_some_and(|id| ctx.static_cfg.is_authorized(id));
    let Some(text) = msg.text() else {
        return Ok(());
    };
    if !authorized {
        warn!(
            ?user_id,
            chat_id = msg.chat.id.0,
            text,
            "unauthorized unparseable command; ignoring"
        );
        return Ok(());
    }

    info!(text, ?user_id, "command-shaped message failed to parse");
    bot.send_message(msg.chat.id, handlers::usage_hint(text))
        .parse_mode(ParseMode::Html)
        .await?;
    Ok(())
}

/// Locks a `std::sync::Mutex`, treating poisoning as a bug to surface loudly
/// (a previous holder panicked mid-update), not a runtime error to handle.
/// Centralised so the justified `expect` lives in exactly one place — the
/// crate denies `clippy::expect_used` everywhere else (#23).
#[allow(clippy::expect_used)]
fn lock_unpoisoned<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().expect("mutex poisoned")
}

/// Discards `user_id`'s in-flight picker drafts. "Leaving a picker without
/// saving must not commit toggles" is the wizard's core invariant (#6) —
/// every navigation away from a multi-select picker (menu, done,
/// back-to-brands) funnels through this one helper so a new back target
/// can't quietly skip the cleanup.
///
/// Per-user (#9): user A backing out of their picker must not wipe user B's
/// half-built selection.
fn discard_drafts(ctx: &CommandContext, user_id: i64) {
    lock_unpoisoned(&ctx.chassis_draft).remove(&user_id);
    lock_unpoisoned(&ctx.models_draft).remove(&user_id);
    lock_unpoisoned(&ctx.gearbox_draft).remove(&user_id);
}

/// Flips `item` in `user_id`'s draft (creating an empty draft on first use)
/// and returns a snapshot for the keyboard redraw. Generic because chassis
/// drafts hold `u32` codes and model drafts hold `String` slugs — the toggle
/// logic is identical.
fn toggle_in_draft<T: PartialEq + Clone>(
    drafts: &Mutex<HashMap<i64, Vec<T>>>,
    user_id: i64,
    item: T,
) -> Vec<T> {
    let mut map = lock_unpoisoned(drafts);
    let v = map.entry(user_id).or_default();
    if let Some(pos) = v.iter().position(|x| *x == item) {
        v.remove(pos);
    } else {
        v.push(item);
    }
    v.clone()
}

/// Single endpoint for every inline-keyboard tap. We dispatch on the
/// `callback_data` string.
///
/// Key behaviours required by Telegram:
/// * `answer_callback_query()` MUST be called within ~10 seconds — otherwise
///   the TG client shows a permanent spinner on the button. We do it up front.
/// * Edits to the message use `edit_message_text` with the same chat_id +
///   message_id from the original. `reply_markup` either updates the keyboard
///   or — if omitted — clears it.
async fn handle_callback(bot: Bot, q: CallbackQuery, ctx: CommandContext) -> ResponseResult<()> {
    // Authorization — same posture as commands: ignore foreign clicks.
    let user_id = q.from.id.0 as i64;
    if !ctx.static_cfg.is_authorized(user_id) {
        warn!(user_id, "unauthorized callback; ignoring");
        // Still answer the callback so the user's spinner doesn't hang.
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    }

    // Acknowledge ASAP so the TG client's loading spinner disappears.
    // Empty body = no toast message. We could pass `.text("Saved!")` to show
    // a small popup, but the message edit below is more informative anyway.
    bot.answer_callback_query(q.id.clone()).await?;

    let Some(data) = q.data.as_deref() else {
        return Ok(());
    };
    info!(?data, ?user_id, "callback received");

    // Pull chat + message ids from the original. `q.message` is `Option<MaybeInaccessibleMessage>`
    // — `Inaccessible` happens when the message is too old (>48h) to edit.
    // For our /filter sessions that's never an issue, but we handle it gracefully.
    let Some(orig) = q.message else {
        return Ok(());
    };
    let chat_id = orig.chat().id;
    let message_id = orig.id();

    let (text, markup): (String, Option<InlineKeyboardMarkup>) = match data {
        CB_FILTER_MENU => {
            // Returning to the menu always discards any in-flight picker
            // edits — "back without saving" semantics.
            discard_drafts(&ctx, user_id);

            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_BRAND_PICKER => {
            // Reachable both from the menu and via "back" from the models
            // picker (#6) — the latter leaves an unsaved models draft
            // behind, so this navigation discards too.
            discard_drafts(&ctx, user_id);
            ("Выбери марку:".to_string(), Some(brand_picker_keyboard()))
        }
        CB_FILTER_BRAND_CUSTOM_HINT => (
            "💬 <b>Бренд вне каталога?</b>\n\n\
             Отправь команду <code>/setbrand &lt;slug&gt;</code>\n\
             Slug — это значение из URL сайта polovniautomobili.com, \
             в параметре <code>brand=</code>.\n\n\
             Примеры: <code>/setbrand smart</code>, <code>/setbrand suzuki</code>, \
             <code>/setbrand alfa-romeo</code>.\n\n\
             Если потом захочешь обратно в каталог — открой ✏️ Марка."
                .into(),
            Some(brand_picker_keyboard()),
        ),
        CB_FILTER_BRAND_CLEAR => {
            apply_brand(&ctx, None).await;
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_CHASSIS_PICKER => {
            // Initialise this user's draft = current runtime.chassis so the
            // picker opens with existing selections checked.
            let initial = ctx.runtime.read().await.search.chassis.clone();
            lock_unpoisoned(&ctx.chassis_draft).insert(user_id, initial.clone());
            (
                "Выбери типы кузова (можно несколько):".to_string(),
                Some(chassis_picker_keyboard(&initial)),
            )
        }
        CB_FILTER_CHASSIS_SAVE => {
            // Commit this user's draft to runtime + DB. The draft becomes the
            // new selection, even if empty (= "no chassis filter").
            let draft = lock_unpoisoned(&ctx.chassis_draft)
                .remove(&user_id)
                .unwrap_or_default();
            apply_chassis(&ctx, draft).await;
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_GEARBOX_PICKER => {
            // Same open-with-current-selection dance as the chassis picker.
            let initial = ctx.runtime.read().await.search.gearbox.clone();
            lock_unpoisoned(&ctx.gearbox_draft).insert(user_id, initial.clone());
            (
                "Выбери типы КПП (можно несколько):".to_string(),
                Some(gearbox_picker_keyboard(&initial)),
            )
        }
        CB_FILTER_GEARBOX_SAVE => {
            let draft = lock_unpoisoned(&ctx.gearbox_draft)
                .remove(&user_id)
                .unwrap_or_default();
            apply_gearbox(&ctx, draft).await;
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_DONE => {
            // Same "leave dialog" cleanup as CB_FILTER_MENU: discard any
            // hanging drafts. Without this, an abandoned picker edit would
            // sit in memory until the bot restarted.
            discard_drafts(&ctx, user_id);

            let search = ctx.runtime.read().await.search.clone();
            // Final state: text only, no keyboard. `markup = None` clears it.
            (
                format!("✅ Фильтры сохранены.\n\n{}", format_filter_ru(&search)),
                None,
            )
        }
        CB_FILTER_TODO => (
            "🔧 Эта секция фильтра будет в следующих сессиях.\n\nПока возвращаемся в меню."
                .to_string(),
            Some(filter_menu_keyboard()),
        ),
        CB_FILTER_RESET_CONFIRM => {
            // Show current filters in the confirmation so the user sees what
            // they're about to lose. No state change yet — that's only on
            // `CB_FILTER_RESET_APPLY` below.
            let search = ctx.runtime.read().await.search.clone();
            (
                format!(
                    "⚠️ <b>Сбросить все фильтры?</b>\n\n\
                     Сейчас стоит:\n{}\n\n\
                     После сброса бот будет видеть весь каталог.",
                    format_filter_ru(&search)
                ),
                Some(reset_confirm_keyboard()),
            )
        }
        CB_FILTER_INTERVAL_PICKER => {
            let current_secs = ctx.runtime.read().await.poll_interval.as_secs();
            (
                "Выбери интервал поллинга:".to_string(),
                Some(interval_picker_keyboard(current_secs)),
            )
        }
        s if s.starts_with(CB_FILTER_INTERVAL_SET_PREFIX) => {
            // Picker only offers values ≥ MIN_POLL_INTERVAL_SECS, so we trust
            // the input. `unwrap_or(MIN_POLL_INTERVAL_SECS)` gives a safe
            // fallback if a malformed callback somehow sneaks through.
            let secs = s[CB_FILTER_INTERVAL_SET_PREFIX.len()..]
                .parse::<u64>()
                .unwrap_or(MIN_POLL_INTERVAL_SECS);
            let _ = apply_interval(&ctx, secs).await;
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_RESET_APPLY => {
            apply_reset_all(&ctx).await;
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format!(
                    "🧹 <b>Фильтры сброшены.</b>\n\n{}",
                    format_filter_menu_body(&search, interval_secs)
                ),
                Some(filter_menu_keyboard()),
            )
        }
        s if s.starts_with(CB_FILTER_BRAND_SET_PREFIX) => {
            let slug = &s[CB_FILTER_BRAND_SET_PREFIX.len()..];
            apply_brand(&ctx, Some(slug.to_string())).await;
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_PRICE_PICKER => {
            let cur = {
                let r = ctx.runtime.read().await;
                (r.search.price_from, r.search.price_to)
            };
            (
                "Выбери диапазон цены:".to_string(),
                Some(range_picker_keyboard("price", PRICE_RANGES, cur)),
            )
        }
        CB_FILTER_YEAR_PICKER => {
            let cur = {
                let r = ctx.runtime.read().await;
                // year_from/to are u16 in SearchFilter; widen to u32 for the
                // shared picker helper which uses u32 across both ranges.
                (
                    r.search.year_from.map(u32::from),
                    r.search.year_to.map(u32::from),
                )
            };
            (
                "Выбери диапазон года выпуска:".to_string(),
                Some(range_picker_keyboard("year", YEAR_RANGES, cur)),
            )
        }
        s if s.starts_with(CB_FILTER_RANGE_SET_PREFIX) => {
            // Format: f:range_set:<field>:<from>:<to>
            // We strip the prefix and split the remainder on `:`.
            let tail = &s[CB_FILTER_RANGE_SET_PREFIX.len()..];
            let parts: Vec<&str> = tail.splitn(3, ':').collect();
            let [field, from_str, to_str] = parts.as_slice() else {
                warn!(unknown = s, "malformed range_set callback");
                return Ok(());
            };
            let from = from_str.parse::<u32>().unwrap_or(0);
            let to = to_str.parse::<u32>().unwrap_or(0);
            match *field {
                "price" => apply_price_range(&ctx, from, to).await,
                "year" => apply_year_range(&ctx, from, to).await,
                other => {
                    warn!(field = other, "range_set for unknown field");
                    return Ok(());
                }
            }
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_MODELS_PICKER => {
            // Models need a brand context — the catalog is per-brand. Three
            // outcomes: no brand set, brand set but absent from catalog
            // (env override or unsupported brand), or brand-in-catalog (open).
            //
            // We use a `match` rather than `let Some(..) = .. else { tuple }`
            // because `let-else` requires the else branch to *diverge*
            // (return / break / panic). Returning a `(String, Markup)` value
            // doesn't qualify — that pattern only works when the else does
            // some control flow.
            let (brand, initial_models) = {
                let r = ctx.runtime.read().await;
                (r.search.brand.clone(), r.search.models.clone())
            };
            match brand {
                // No brand yet → don't bounce back to the menu; open the
                // brand picker right here (#6). Picking a brand lands on the
                // menu, from where models is one tap — and the picker's own
                // back row keeps the menu reachable.
                None => (
                    "❌ Сначала выбери марку — без неё список моделей не известен:".into(),
                    Some(brand_picker_keyboard()),
                ),
                Some(brand_slug) => match models_for_brand(&brand_slug) {
                    Some(model_list) => {
                        lock_unpoisoned(&ctx.models_draft).insert(user_id, initial_models.clone());
                        (
                            models_picker_title(&brand_slug),
                            Some(model_picker_keyboard(model_list, &initial_models)),
                        )
                    }
                    None => (
                        format!(
                            "🤷 Для марки <code>{}</code> у меня нет каталога моделей.\n\n\
                             Поставь модели через <code>SEARCH_MODEL</code> в <code>.env</code>, \
                             либо смени марку через ✏️ Марка.",
                            crate::telegram::escape_html(&brand_slug)
                        ),
                        Some(filter_menu_keyboard()),
                    ),
                },
            }
        }
        CB_FILTER_MODELS_SAVE => {
            let draft = lock_unpoisoned(&ctx.models_draft)
                .remove(&user_id)
                .unwrap_or_default();
            apply_models(&ctx, draft).await;
            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        s if s.starts_with(CB_FILTER_MODELS_TOGGLE_PREFIX) => {
            let slug = &s[CB_FILTER_MODELS_TOGGLE_PREFIX.len()..];
            if slug.is_empty() {
                return Ok(());
            }
            // Need the brand to know which keyboard layout to redraw.
            let brand = ctx.runtime.read().await.search.brand.clone();
            let Some(brand_slug) = brand else {
                // Shouldn't happen if user reached the toggle from the picker,
                // but defend gracefully.
                return Ok(());
            };
            let Some(model_list) = models_for_brand(&brand_slug) else {
                return Ok(());
            };
            let new_state = toggle_in_draft(&ctx.models_draft, user_id, slug.to_owned());
            (
                models_picker_title(&brand_slug),
                Some(model_picker_keyboard(model_list, &new_state)),
            )
        }
        s if s.starts_with(CB_FILTER_CHASSIS_TOGGLE_PREFIX) => {
            let Ok(code) = s[CB_FILTER_CHASSIS_TOGGLE_PREFIX.len()..].parse::<u32>() else {
                warn!(unknown = s, "chassis toggle with non-numeric code");
                return Ok(());
            };
            // Toggle inside the lock, then take a snapshot for redraw —
            // never hold the Mutex past the lock scope.
            let new_state = toggle_in_draft(&ctx.chassis_draft, user_id, code);
            (
                "Выбери типы кузова (можно несколько):".to_string(),
                Some(chassis_picker_keyboard(&new_state)),
            )
        }
        s if s.starts_with(CB_FILTER_GEARBOX_TOGGLE_PREFIX) => {
            let Ok(code) = s[CB_FILTER_GEARBOX_TOGGLE_PREFIX.len()..].parse::<u32>() else {
                warn!(unknown = s, "gearbox toggle with non-numeric code");
                return Ok(());
            };
            let new_state = toggle_in_draft(&ctx.gearbox_draft, user_id, code);
            (
                "Выбери типы КПП (можно несколько):".to_string(),
                Some(gearbox_picker_keyboard(&new_state)),
            )
        }
        other => {
            warn!(unknown = other, "callback with unknown data; ignoring");
            return Ok(());
        }
    };

    // `edit_message_text` returns the edited Message on success. Errors here
    // are usually "message is not modified" (same content twice) — benign.
    let mut req = bot
        .edit_message_text(chat_id, message_id, text)
        .parse_mode(ParseMode::Html);
    if let Some(m) = markup {
        req = req.reply_markup(m);
    }
    if let Err(e) = req.await {
        warn!(error = %e, "edit_message_text failed");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[test]
    fn menu_registers_exactly_the_curated_commands_in_order() {
        // filter_map in menu_commands would silently drop a typo'd name —
        // pin the exact list so the menu can't shrink unnoticed.
        let menu: Vec<String> = menu_commands()
            .iter()
            .map(|c| c.command.trim_start_matches('/').to_string())
            .collect();
        assert_eq!(menu, MENU_COMMANDS);
    }

    #[test]
    fn menu_descriptions_come_from_the_derive() {
        for cmd in menu_commands() {
            assert!(
                !cmd.description.is_empty(),
                "{} has no description",
                cmd.command
            );
        }
    }

    #[test]
    fn draft_toggle_is_isolated_per_user() {
        // #9: two authorized users editing pickers concurrently must not
        // corrupt each other's selections.
        let drafts: Mutex<HashMap<i64, Vec<u32>>> = Mutex::new(HashMap::new());

        assert_eq!(toggle_in_draft(&drafts, 111, 2634), vec![2634]);
        // User 222 starts from their own empty draft, not user 111's.
        assert_eq!(toggle_in_draft(&drafts, 222, 2627), vec![2627]);
        // User 111's draft is untouched by 222's toggle.
        assert_eq!(toggle_in_draft(&drafts, 111, 2632), vec![2634, 2632]);
        // Toggling an already-present item removes it (per user).
        assert_eq!(toggle_in_draft(&drafts, 111, 2634), vec![2632]);
        assert_eq!(toggle_in_draft(&drafts, 222, 2627), Vec::<u32>::new());
    }

    #[test]
    fn draft_toggle_works_for_string_slugs_too() {
        // Model drafts use String items through the same generic helper.
        let drafts: Mutex<HashMap<i64, Vec<String>>> = Mutex::new(HashMap::new());
        assert_eq!(
            toggle_in_draft(&drafts, 111, "cooper".to_string()),
            vec!["cooper".to_string()]
        );
        assert_eq!(
            toggle_in_draft(&drafts, 111, "cooper".to_string()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn hidden_commands_still_parse() {
        // The whole point of #8: hiding from the menu must not hide from the
        // parser. /dump is menu-hidden but must keep working when typed.
        use teloxide::utils::command::BotCommands;
        let cmd = Command::parse("/dump 10", "TestBot").unwrap();
        assert!(matches!(cmd, Command::Dump(10)));
        let cmd = Command::parse("/diag", "TestBot").unwrap();
        assert!(matches!(cmd, Command::Diag));
        // And the fallback trigger case really does fail to parse.
        assert!(Command::parse("/interval abc", "TestBot").is_err());
        assert!(Command::parse("/dump", "TestBot").is_err());
    }
}
