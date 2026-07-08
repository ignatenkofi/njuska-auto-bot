//! Telegram command dispatcher — the "incoming" side of the bot in v2.
//!
//! Long-polls `getUpdates` via teloxide, routes recognised commands to
//! handler functions. The poll loop in [`crate::bot`] keeps doing its thing
//! in parallel; both share the same [`RuntimeConfig`] via `Arc<RwLock<…>>`.
//!
//! ## Authorization
//!
//! Anyone who finds the bot can talk to it. We **silently drop** messages
//! from non-authorized users (logged at `warn` level so probing shows up
//! in the operator log). A "you're not authorized" reply would leak the
//! existence of the bot to bystanders.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use teloxide::Bot;
use teloxide::dispatching::UpdateFilterExt;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode};
use teloxide::utils::command::BotCommands;
use tokio::sync::{Notify, RwLock};
use tracing::{info, warn};

use crate::config::{
    MIN_POLL_INTERVAL_SECS, RuntimeConfig, SETTING_PAUSED, SETTING_POLL_INTERVAL_SECS,
    SETTING_SEARCH_BRAND, SETTING_SEARCH_CHASSIS, SETTING_SEARCH_MODELS, SETTING_SEARCH_PRICE_FROM,
    SETTING_SEARCH_PRICE_TO, SETTING_SEARCH_YEAR_FROM, SETTING_SEARCH_YEAR_TO, StaticConfig,
};
use crate::models::SearchFilter;
use crate::signals::shutdown_signal;
use crate::storage::Storage;

// ---------------------------------------------------------------------------
// Catalog data for the /filter wizard
// ---------------------------------------------------------------------------

/// Brand catalog: `(url_slug, display_name)`. The slug is what we pass to the
/// site as `?brand=...`; the display is what the user sees on a button.
///
/// Hardcoded for now — the site has more brands, but these are the 20 most
/// common in Serbia. If the user wants a brand not here, they can fall back
/// to setting `SEARCH_BRAND` in `.env`. Session 3.5 would add a "Другая
/// (ввести)" button using ForceReply, but that's not in scope today.
/// Body-type catalog: `(numeric_code, display_name)`. Codes are the ones the
/// site uses internally in `chassis[]=...`. Display names are in Russian
/// (Cyrillic) for friendlier UI; the original Serbian names (Kabriolet,
/// Limuzina, …) read close enough but the Russian spellings feel more native.
///
/// Subset of what polovni offers — these are the 6 a personal-shopper would
/// realistically tick. If you need a more exotic body type (Minivan, Pickup),
/// fall back to `SEARCH_CHASSIS` in `.env`.
const CHASSIS: &[(u32, &str)] = &[
    (2627, "Универсал"),
    (2628, "Купе"),
    (2629, "Хэтчбек"),
    (2631, "Седан"),
    (2632, "Внедорожник"),
    (2634, "Кабриолет"),
];

/// Predefined poll-interval presets in seconds. The minimum (60s) matches
/// `MIN_POLL_INTERVAL_SECS` so the picker never offers an illegal value.
/// For non-preset intervals, the user can still type `/interval N`.
const INTERVAL_PRESETS: &[(u64, &str)] = &[
    (60, "1 мин"),
    (300, "5 мин"),
    (600, "10 мин"),
    (1800, "30 мин"),
    (3600, "1 час"),
    (7200, "2 часа"),
];

/// Predefined price ranges, in EUR. `(from, to, display)` — `0` on either
/// side means "no bound there". The list is intentionally short — six
/// buckets cover practically every used-car shopping intent. Users who
/// need a custom range fall back to `.env` (`SEARCH_PRICE_FROM`, `SEARCH_PRICE_TO`).
const PRICE_RANGES: &[(u32, u32, &str)] = &[
    (0, 5_000, "До 5 000 €"),
    (5_000, 10_000, "5–10 000 €"),
    (10_000, 15_000, "10–15 000 €"),
    (15_000, 25_000, "15–25 000 €"),
    (25_000, 50_000, "25–50 000 €"),
    (50_000, 0, "Более 50 000 €"),
];

/// Predefined year ranges. Same shape as price ranges but the numbers happen
/// to fit `u16` — they're stored that way in `SearchFilter`. We use `u32`
/// in the catalog for callback-data uniformity with prices.
const YEAR_RANGES: &[(u32, u32, &str)] = &[
    (2024, 0, "2024 и новее"),
    (2020, 2023, "2020–2023"),
    (2015, 2019, "2015–2019"),
    (2010, 2014, "2010–2014"),
    (2005, 2009, "2005–2009"),
    (0, 2004, "До 2005"),
];

/// Model catalog, keyed by the brand's `slug` (matches the `0`-th column of
/// `BRANDS`). Per brand, a list of `(model_slug, display)` entries.
///
/// Model slugs are **educated guesses** at what polovniautomobili.com accepts
/// in `model[]=...`. We've confirmed `mini → cooper` works from the user's
/// reference URL; other slugs are conventional lowercase-hyphen forms.
/// If a particular slug doesn't match what the site expects, the next poll
/// just returns no listings (zero-streak alert eventually fires) — recoverable.
///
/// Brands not in this catalog (Honda, Volvo, …) fall through to "no model
/// catalog for this brand" UI, and the user can still set `SEARCH_MODEL` in
/// `.env`. Worth adding more brands here as users hit them.
const MODELS_BY_BRAND: &[(&str, &[(&str, &str)])] = &[
    (
        "audi",
        &[
            ("a1", "A1"),
            ("a3", "A3"),
            ("a4", "A4"),
            ("a5", "A5"),
            ("a6", "A6"),
            ("q3", "Q3"),
            ("q5", "Q5"),
            ("q7", "Q7"),
        ],
    ),
    (
        "bmw",
        &[
            ("1", "1 серия"),
            ("2", "2 серия"),
            ("3", "3 серия"),
            ("5", "5 серия"),
            ("x1", "X1"),
            ("x3", "X3"),
            ("x5", "X5"),
            ("x6", "X6"),
        ],
    ),
    (
        "citroen",
        &[
            ("c1", "C1"),
            ("c3", "C3"),
            ("c4", "C4"),
            ("c5", "C5"),
            ("berlingo", "Berlingo"),
            ("ds3", "DS3"),
        ],
    ),
    (
        "fiat",
        &[
            ("500", "500"),
            ("500l", "500L"),
            ("500x", "500X"),
            ("panda", "Panda"),
            ("punto", "Punto"),
            ("tipo", "Tipo"),
        ],
    ),
    (
        "ford",
        &[
            ("fiesta", "Fiesta"),
            ("focus", "Focus"),
            ("mondeo", "Mondeo"),
            ("kuga", "Kuga"),
            ("ecosport", "Ecosport"),
            ("mustang", "Mustang"),
        ],
    ),
    (
        "hyundai",
        &[
            ("i10", "i10"),
            ("i20", "i20"),
            ("i30", "i30"),
            ("tucson", "Tucson"),
            ("santa-fe", "Santa Fe"),
            ("kona", "Kona"),
        ],
    ),
    (
        "kia",
        &[
            ("picanto", "Picanto"),
            ("rio", "Rio"),
            ("ceed", "Ceed"),
            ("sportage", "Sportage"),
            ("sorento", "Sorento"),
            ("stonic", "Stonic"),
        ],
    ),
    (
        "mazda",
        &[
            ("2", "2"),
            ("3", "3"),
            ("6", "6"),
            ("cx-3", "CX-3"),
            ("cx-5", "CX-5"),
            ("mx-5", "MX-5"),
        ],
    ),
    (
        "mercedes-benz",
        &[
            ("a-klasa", "A класс"),
            ("b-klasa", "B класс"),
            ("c-klasa", "C класс"),
            ("e-klasa", "E класс"),
            ("s-klasa", "S класс"),
            ("gla", "GLA"),
            ("glc", "GLC"),
            ("gle", "GLE"),
        ],
    ),
    (
        "mini",
        &[
            ("cooper", "Cooper"),
            ("cooper-s", "Cooper S"),
            ("countryman", "Countryman"),
            ("clubman", "Clubman"),
            ("one", "One"),
            ("john-cooper-works", "JCW"),
        ],
    ),
    (
        "nissan",
        &[
            ("juke", "Juke"),
            ("qashqai", "Qashqai"),
            ("x-trail", "X-Trail"),
            ("micra", "Micra"),
            ("note", "Note"),
        ],
    ),
    (
        "opel",
        &[
            ("corsa", "Corsa"),
            ("astra", "Astra"),
            ("insignia", "Insignia"),
            ("mokka", "Mokka"),
            ("crossland", "Crossland"),
            ("grandland", "Grandland"),
        ],
    ),
    (
        "peugeot",
        &[
            ("208", "208"),
            ("308", "308"),
            ("508", "508"),
            ("2008", "2008"),
            ("3008", "3008"),
            ("5008", "5008"),
        ],
    ),
    (
        "renault",
        &[
            ("clio", "Clio"),
            ("megane", "Megane"),
            ("captur", "Captur"),
            ("kadjar", "Kadjar"),
            ("koleos", "Koleos"),
            ("talisman", "Talisman"),
        ],
    ),
    (
        "seat",
        &[
            ("ibiza", "Ibiza"),
            ("leon", "Leon"),
            ("arona", "Arona"),
            ("ateca", "Ateca"),
            ("tarraco", "Tarraco"),
        ],
    ),
    (
        "skoda",
        &[
            ("fabia", "Fabia"),
            ("octavia", "Octavia"),
            ("superb", "Superb"),
            ("kodiaq", "Kodiaq"),
            ("karoq", "Karoq"),
            ("rapid", "Rapid"),
        ],
    ),
    (
        "toyota",
        &[
            ("yaris", "Yaris"),
            ("corolla", "Corolla"),
            ("camry", "Camry"),
            ("rav4", "RAV4"),
            ("c-hr", "C-HR"),
            ("avensis", "Avensis"),
        ],
    ),
    (
        "volkswagen",
        &[
            ("polo", "Polo"),
            ("golf", "Golf"),
            ("passat", "Passat"),
            ("tiguan", "Tiguan"),
            ("touran", "Touran"),
            ("t-roc", "T-Roc"),
        ],
    ),
];

/// Lookup helper: returns the `(model_slug, display)` list for a given brand,
/// or `None` if the brand isn't in the catalog. Linear scan over ~20 entries
/// — fast enough that a `HashMap` would be a premature optimisation.
fn models_for_brand(brand_slug: &str) -> Option<&'static [(&'static str, &'static str)]> {
    MODELS_BY_BRAND
        .iter()
        .find(|(b, _)| *b == brand_slug)
        .map(|(_, m)| *m)
}

const BRANDS: &[(&str, &str)] = &[
    ("audi", "Audi"),
    ("bmw", "BMW"),
    ("citroen", "Citroen"),
    ("fiat", "Fiat"),
    ("ford", "Ford"),
    ("honda", "Honda"),
    ("hyundai", "Hyundai"),
    ("kia", "Kia"),
    ("mazda", "Mazda"),
    ("mercedes-benz", "Mercedes"),
    ("mini", "MINI"),
    ("nissan", "Nissan"),
    ("opel", "Opel"),
    ("peugeot", "Peugeot"),
    ("renault", "Renault"),
    ("seat", "Seat"),
    ("skoda", "Skoda"),
    ("toyota", "Toyota"),
    ("volkswagen", "VW"),
    ("volvo", "Volvo"),
];

// ---------------------------------------------------------------------------
// Callback-data namespaces
//
// Every InlineKeyboardButton::callback carries a `data: String`. Telegram caps
// this at 64 bytes. We namespace everything with `f:` for "filter", and parse
// in `handle_callback` by splitting on `:`. Keeping data short and structured
// means we never need a per-user state machine — the button itself encodes
// "what the user wants to happen next".
// ---------------------------------------------------------------------------

/// Show the top-level filter menu.
const CB_FILTER_MENU: &str = "f:menu";
/// Show the brand picker.
const CB_FILTER_BRAND_PICKER: &str = "f:brand_picker";
/// Clear the brand filter (set to None).
const CB_FILTER_BRAND_CLEAR: &str = "f:brand_clear";
/// Prefix for "set the brand to this slug" buttons: `f:brand_set:bmw`.
const CB_FILTER_BRAND_SET_PREFIX: &str = "f:brand_set:";
/// Show a hint asking the user to type `/setbrand <slug>` for brands not in
/// the catalog. Pure UI guidance — no state change.
const CB_FILTER_BRAND_CUSTOM_HINT: &str = "f:brand_custom_hint";
/// Close the filter menu — last edit leaves a "saved" state, no keyboard.
const CB_FILTER_DONE: &str = "f:done";
/// Placeholder for sections not yet implemented (models, price, year).
/// Currently unused — all sections are real after sessions 3.1-3.4. Kept as
/// the scaffold for any future filter section that lands in stages.
#[allow(dead_code)]
const CB_FILTER_TODO: &str = "f:todo";

// Interval picker — single-tap commit from a preset list. The picker only
// offers values ≥ MIN_POLL_INTERVAL_SECS, so we don't need to re-validate
// at the callback layer. Custom values still go through `/interval N`.
const CB_FILTER_INTERVAL_PICKER: &str = "f:interval_picker";
const CB_FILTER_INTERVAL_SET_PREFIX: &str = "f:interval_set:";

// "Reset all filters" two-step. Same pattern as `/clear` / `/clear_confirm`
// but inline-keyboard driven: tap [🧹 Сбросить] → confirmation prompt with
// [✅ Да] [↩️ Отмена]; only [✅ Да] (= `CB_FILTER_RESET_APPLY`) actually wipes.
const CB_FILTER_RESET_CONFIRM: &str = "f:reset_confirm";
const CB_FILTER_RESET_APPLY: &str = "f:reset_apply";

// Chassis multi-select picker:
//   open       → init draft from runtime, show picker
//   toggle:N   → flip N in the draft, redraw picker
//   save       → write draft to DB+runtime, return to menu
//   (CB_FILTER_MENU = Back, also clears the draft)
const CB_FILTER_CHASSIS_PICKER: &str = "f:chassis_picker";
const CB_FILTER_CHASSIS_TOGGLE_PREFIX: &str = "f:chassis_toggle:";
const CB_FILTER_CHASSIS_SAVE: &str = "f:chassis_save";

// Price + year range pickers. Single-tap commits — no draft state because each
// button carries the *complete* new range, unlike chassis where the user
// builds up a multi-select set.
//
// Callback format: `f:range_set:<field>:<from>:<to>` where `field` is "price"
// or "year" and `from`/`to` are decimal `u32` (0 = "no bound on this side").
// We use one shared prefix and one shared handler — both ranges behave
// identically apart from the field name and the bound type (u32 vs u16).
const CB_FILTER_PRICE_PICKER: &str = "f:price_picker";
const CB_FILTER_YEAR_PICKER: &str = "f:year_picker";
const CB_FILTER_RANGE_SET_PREFIX: &str = "f:range_set:";

// Models multi-select picker. Same shape as chassis (toggle + save + back),
// but the option set depends on the currently-selected brand.
const CB_FILTER_MODELS_PICKER: &str = "f:models_picker";
const CB_FILTER_MODELS_TOGGLE_PREFIX: &str = "f:models_toggle:";
const CB_FILTER_MODELS_SAVE: &str = "f:models_save";

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
    /// In-flight chassis selection during the `/filter → Кузов` flow.
    /// `None` = picker isn't open. `Some(Vec)` = user is toggling.
    /// Becomes `None` again on Save (after persisting) or on Back/menu.
    ///
    /// One slot, not a HashMap-per-user — we have exactly one authorised user
    /// (`AUTHORIZED_USER_ID`). If we ever go multi-user, swap to a
    /// `HashMap<UserId, Vec<u32>>` keyed on the sender's id.
    pub chassis_draft: Arc<Mutex<Option<Vec<u32>>>>,
    /// In-flight model selection. Same shape as `chassis_draft` but with
    /// `String` slugs (since model slugs are textual).
    pub models_draft: Arc<Mutex<Option<Vec<String>>>>,
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

    // Set the bot's `/help` menu in the Telegram client. The `setMyCommands`
    // call is idempotent and best-effort; failure isn't fatal — the bot still
    // works without the menu, just no auto-complete in the TG client.
    if let Err(e) = bot.set_my_commands(Command::bot_commands()).await {
        warn!(error = ?e, "couldn't register /help menu (continuing anyway)");
    }

    // The handler tree has **two branches** in v2 session 3:
    //   1. Update is a Message AND parses as a `Command` → `handle_command`.
    //   2. Update is a CallbackQuery (inline-keyboard tap) → `handle_callback`.
    //
    // teloxide tries them in order; the first one that matches wins. Updates
    // that match neither branch are dropped with an "Unhandled update" warning
    // (you can see those in the logs as a debug hint when something doesn't
    // wire up correctly).
    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handle_command),
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
    // Authorisation: ignore commands from anyone but the configured user.
    // We compare against the *sender's* user id, not chat id — chat id may be
    // a channel/group (impersonal), but the human pressing the command always
    // has a stable personal user id.
    let user_id: Option<i64> = msg.from.as_ref().map(|u| u.id.0 as i64);
    let authorized = user_id == Some(ctx.static_cfg.authorized_user_id);
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

// ---------------------------------------------------------------------------
// Per-command handlers (or formatters)
// ---------------------------------------------------------------------------

fn format_start() -> String {
    "👋 <b>Привет!</b> Я NjuskaAutoBot — слежу за объявлениями на \
     polovniautomobili.com и кидаю тебе сюда новые.\n\n\
     /help — список команд\n\
     /status — текущая конфигурация\n\
     /pause /resume — поставить на паузу и возобновить"
        .into()
}

async fn format_status(ctx: &CommandContext) -> String {
    // Snapshot runtime under a brief read lock; we don't hold across the
    // storage call below.
    let (paused, poll_interval_secs, search) = {
        let r = ctx.runtime.read().await;
        (r.paused, r.poll_interval.as_secs(), r.search.clone())
    };

    let count = ctx.storage.seen_count().unwrap_or(0);

    // `Url` serialises query params percent-encoded, but `&` between pairs
    // stays raw — Telegram's HTML parser chokes on a bare `&` inside an
    // attribute, so it must become `&amp;` (issue #3).
    let search_url = escape_html_attr_for_dump(search.to_url().as_str());

    format!(
        "<b>Текущая конфигурация</b>\n\n\
         {status_icon} Поллинг: <b>{status_text}</b>, интервал <b>{poll_interval_secs}</b> сек\n\n\
         <b>Фильтры поиска</b>\n{filter}\n\
         🔗 <a href=\"{search_url}\">Открыть этот поиск на сайте</a>\n\n\
         <b>База</b>: {count} объявлений в seen_listings\n\
         <b>Версия</b>: <code>{version}</code>",
        status_icon = if paused { "⏸" } else { "▶️" },
        status_text = if paused {
            "на паузе"
        } else {
            "работает"
        },
        filter = format_filter_ru(&search),
        version = crate::version::VERSION,
    )
}

/// Renders a `SearchFilter` as a Telegram-HTML bullet list in Russian.
/// Kept inline in this file because it's a UI concern, not part of the
/// `SearchFilter` type itself — same struct could be rendered differently
/// for logs or for another locale.
fn format_filter_ru(f: &SearchFilter) -> String {
    let or_dash = |s: Option<String>| s.unwrap_or_else(|| "—".to_string());

    let mut lines = Vec::new();
    lines.push(format!(
        "• Марка: <code>{}</code>",
        or_dash(f.brand.clone())
    ));
    lines.push(format!(
        "• Модели: <code>{}</code>",
        if f.models.is_empty() {
            "—".to_string()
        } else {
            f.models.join(", ")
        }
    ));
    lines.push(format!(
        "• Кузов: <code>{}</code>",
        if f.chassis.is_empty() {
            "—".to_string()
        } else {
            f.chassis
                .iter()
                .map(|c| chassis_label(*c))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    lines.push(format!(
        "• Цена: <code>{} – {}</code>",
        or_dash(f.price_from.map(|p| p.to_string())),
        or_dash(f.price_to.map(|p| p.to_string())),
    ));
    lines.push(format!(
        "• Год: <code>{} – {}</code>",
        or_dash(f.year_from.map(|y| y.to_string())),
        or_dash(f.year_to.map(|y| y.to_string())),
    ));
    lines.push(format!(
        "• Без цены: <code>{}</code>",
        if f.without_price { "да" } else { "нет" }
    ));
    lines.join("\n")
}

/// Reverse lookup: chassis code → human label from the [`CHASSIS`] catalog.
/// Codes set via `SEARCH_CHASSIS` in `.env` may be outside the catalog —
/// those render as the raw number so nothing is silently hidden (issue #4).
fn chassis_label(code: u32) -> String {
    CHASSIS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| code.to_string())
}

async fn handle_pause(ctx: &CommandContext) -> String {
    // Pre-check (read lock only) so the "already paused" case doesn't need
    // a write lock or a DB roundtrip.
    if ctx.runtime.read().await.paused {
        return "ℹ️ Поллинг уже на паузе.".into();
    }

    // Persist FIRST, then mutate runtime. If the DB write fails, we leave
    // both state pieces consistent (paused stays false everywhere). The
    // alternative — mutate first, persist second — risks "RAM says paused,
    // DB says not paused → restart resets the pause" on a DB write failure.
    if let Err(e) = ctx.storage.set_setting(SETTING_PAUSED, "true") {
        warn!(error = %e, "couldn't persist paused=true; refusing to flip state");
        return "❌ Не смог записать в БД, состояние не меняю.".into();
    }

    ctx.runtime.write().await.paused = true;
    info!("paused via command");
    "⏸ Поллинг остановлен.".into()
}

/// `/setbrand <slug>` — set the brand via typed command, for slugs that
/// aren't in our hardcoded catalog (e.g. niche brands like `smart`,
/// `alfa-romeo`, `suzuki`). Reuses `apply_brand` which has the side-effect
/// of clearing models when the brand changes — preserving the same invariant
/// as the inline picker.
async fn handle_set_brand(ctx: &CommandContext, raw_slug: String) -> String {
    let slug = raw_slug.trim().to_ascii_lowercase();
    if slug.is_empty() {
        return "❌ Не указан slug. Пример: <code>/setbrand smart</code>".into();
    }
    // Sanity-check the slug: polovni's URL slugs are alphanumerics + hyphens.
    // Anything else is almost certainly a typo, and silently passing
    // weird characters would just result in zero search results.
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return format!(
            "❌ Slug может содержать только буквы a-z, цифры и дефис.\n\
             Получил: <code>{slug}</code>"
        );
    }
    apply_brand(ctx, Some(slug.clone())).await;
    format!(
        "✅ Марка установлена: <b>{slug}</b>\n\n\
         (Если для этой марки нет каталога моделей — пользуйся \
         <code>SEARCH_MODEL</code> в <code>.env</code>.)"
    )
}

/// `/dump N` — show the most recent N entries from `seen_listings` in a
/// single compact message. Useful for "what has the bot been catching"
/// without scrolling through the chat.
async fn handle_dump(ctx: &CommandContext, n: u32) -> String {
    // Cap at 25 — each listing takes ~150 chars in our compact format, and
    // 25 × 150 ≈ 3750 chars, comfortably under Telegram's 4096-byte
    // sendMessage limit.
    const MAX: u32 = 25;
    if n == 0 {
        return "❌ N должно быть > 0.".into();
    }
    let limit = n.min(MAX);

    let listings = match ctx.storage.last_seen(limit) {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "last_seen query failed");
            return "❌ Не смог прочитать БД, глянь логи.".into();
        }
    };
    if listings.is_empty() {
        return "📋 База пустая — пока ничего не было сохранено.".into();
    }

    // Compact one-line-per-listing format — different from
    // `telegram::format_listing_html` which is the per-card layout used for
    // notifications. Here we want density.
    let lines: Vec<String> = listings
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let price = l.price_text.as_deref().unwrap_or("—");
            let year = l.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into());
            let city = l.city.as_deref().unwrap_or("?");
            format!(
                "{n}. <a href=\"{url}\">{title}</a> — {price} · {year} · {city}",
                n = i + 1,
                url = escape_html_attr_for_dump(&l.url),
                title = escape_html_text_for_dump(&l.title),
                price = escape_html_text_for_dump(price),
                year = year,
                city = escape_html_text_for_dump(city),
            )
        })
        .collect();

    let header = if listings.len() < n as usize {
        format!(
            "📋 Все <b>{}</b> объявлений в БД (запрошено {}):",
            listings.len(),
            n
        )
    } else {
        format!(
            "📋 Последние <b>{}</b> объявлений (новейшие сверху):",
            listings.len()
        )
    };
    format!("{header}\n\n{}", lines.join("\n"))
}

/// `/diag` — one-shot end-to-end fetch diagnostic (#2). Runs the exact same
/// fetch + parse pipeline the poll loop uses (proxy included) and reports
/// each leg in human terms, so "why is the bot quiet?" is answerable from
/// the chat without ssh-ing into the box.
async fn handle_diag(ctx: &CommandContext) -> String {
    let search = ctx.runtime.read().await.search.clone();
    let proxy = ctx.static_cfg.cf_proxy.as_ref();
    let url = search.to_url();

    let mut lines = vec![
        "🩺 <b>Диагностика фетча</b>".to_string(),
        format!(
            "Прокси: <b>{}</b>",
            if proxy.is_some() {
                "настроен (CF Worker)"
            } else {
                "нет — прямой fetch"
            }
        ),
        format!(
            "URL: <code>{}</code>",
            escape_html_text_for_dump(url.as_str())
        ),
    ];

    // One-shot (attempts = 1): /diag reports the *current* state; retries
    // would blur the picture. The startup health-check is the retrying user
    // of the same helper.
    match crate::scraper::fetch_with_retries(&search, proxy, 1).await {
        Ok(html) => {
            let listings = crate::scraper::parse_listings(&html);
            lines.push(format!("HTTP: <b>2xx OK</b>, тело {} байт", html.len()));
            lines.push(format!("Распарсено объявлений: <b>{}</b>", listings.len()));
            lines.push(if listings.is_empty() {
                "⚠️ 0 объявлений: либо фильтр слишком узкий, либо селекторы \
                 устарели (проверь dumps)."
                    .to_string()
            } else {
                "✅ Весь конвейер работает.".to_string()
            });
        }
        Err(e) => {
            lines.push(format!(
                "❌ Фетч упал: {}",
                escape_html_text_for_dump(&crate::bot::describe_fetch_error(&e, proxy.is_some()))
            ));
        }
    }
    lines.join("\n")
}

/// Minimal HTML-attr escape for `/dump` URLs. We don't pull the helpers from
/// `telegram.rs` because those are `pub(crate)`-style internals there; copying
/// the few characters is cheaper than refactoring across modules just for
/// /dump's sake.
fn escape_html_attr_for_dump(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn escape_html_text_for_dump(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `/cancel` — informational no-op. Useful for users who instinctively type
/// /cancel when they got lost; we explain the correct way out.
fn format_cancel() -> String {
    "ℹ️ У меня нет режима, который надо отменять.\n\n\
     Если ты внутри диалога <code>/filter</code> — жми <b>↩️ Назад</b> или \
     <b>✅ Готово</b>.\n\
     Если ждёшь подтверждения <code>/clear</code> — просто не отправляй \
     <code>/clear_confirm</code>, истечёт через 30 секунд."
        .into()
}

/// Core "change poll interval" logic — used by both the `/interval N` command
/// and the inline-keyboard interval picker.
///
/// Returns `Err(human_text)` when the change can't be applied (validation or
/// DB write failure); the caller decides how to surface that — `/interval`
/// embeds it into a reply, the inline picker just logs and redraws the menu.
async fn apply_interval(ctx: &CommandContext, secs: u64) -> Result<u64, String> {
    if secs < MIN_POLL_INTERVAL_SECS {
        return Err(format!(
            "Минимум <b>{MIN_POLL_INTERVAL_SECS}</b> секунд — это вежливость к сайту."
        ));
    }

    // Persist first (same pattern as /pause): if DB write fails, leave both
    // RAM and DB at the old value — invariant preserved on the failure path.
    if let Err(e) = ctx
        .storage
        .set_setting(SETTING_POLL_INTERVAL_SECS, &secs.to_string())
    {
        warn!(error = %e, "couldn't persist poll_interval_secs");
        return Err("Не смог записать в БД, состояние не меняю.".into());
    }

    let old_secs = {
        let mut r = ctx.runtime.write().await;
        let old = r.poll_interval.as_secs();
        r.poll_interval = Duration::from_secs(secs);
        old
    };

    // Wake the poll loop so the new interval is *picked up immediately*.
    // Without this, if the loop is mid-sleep on the old (longer) interval,
    // the change would only take effect at the next natural wake-up. With it,
    // the bot kicks off the next cycle right now.
    ctx.runtime_changed.notify_one();

    info!(old_secs, new_secs = secs, "interval changed");
    Ok(old_secs)
}

/// Thin wrapper around `apply_interval` that formats a reply for the
/// `/interval N` command.
async fn handle_interval(ctx: &CommandContext, secs: u64) -> String {
    match apply_interval(ctx, secs).await {
        Ok(_) => format!("✅ Интервал поллинга: <b>{secs}</b> сек. Применилось сразу."),
        Err(reason) => format!("❌ {reason}"),
    }
}

async fn handle_resume(ctx: &CommandContext) -> String {
    if !ctx.runtime.read().await.paused {
        return "ℹ️ Поллинг уже работает.".into();
    }

    if let Err(e) = ctx.storage.set_setting(SETTING_PAUSED, "false") {
        warn!(error = %e, "couldn't persist paused=false; refusing to flip state");
        return "❌ Не смог записать в БД, состояние не меняю.".into();
    }

    ctx.runtime.write().await.paused = false;
    info!("resumed via command");
    "▶️ Поллинг возобновлён.".into()
}

/// Locks a `std::sync::Mutex`, treating poisoning as a bug to surface loudly
/// (a previous holder panicked mid-update), not a runtime error to handle.
/// Centralised so the justified `expect` lives in exactly one place — the
/// crate denies `clippy::expect_used` everywhere else (#23).
#[allow(clippy::expect_used)]
fn lock_unpoisoned<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().expect("mutex poisoned")
}

/// Two-step destructive op: `/clear` arms a pending state; `/clear_confirm`
/// within [`CLEAR_CONFIRM_WINDOW`] actually wipes. Without this gate, a
/// fat-finger near the input bar could nuke the whole dedup set.
///
/// `handle_clear` is synchronous — we only touch the Mutex<Instant>, no I/O.
fn handle_clear(ctx: &CommandContext) -> String {
    // `lock_unpoisoned` mirrors storage's stance: poisoning means a holder
    // panicked, which is a bug to surface loudly, not a runtime error.
    let mut pending = lock_unpoisoned(&ctx.pending_clear);
    *pending = Some(Instant::now());

    let count = ctx.storage.seen_count().unwrap_or(0);
    format!(
        "⚠️ <b>Опасная операция</b>\n\n\
         Сейчас в seen_listings <b>{count}</b> объявлений.\n\
         Команда <code>/clear</code> сотрёт их все — после этого следующий \
         цикл поллинга снова посчитает текущую выдачу новой и зальёт её в чат.\n\n\
         Чтобы подтвердить, в течение <b>30 секунд</b> отправь \
         <code>/clear_confirm</code>.\n\n\
         Иначе ничего не произойдёт."
    )
}

async fn handle_clear_confirm(ctx: &CommandContext) -> String {
    // Read & consume the pending timestamp in one critical section.
    // Even if the deletion below failed, we still consume the pending state —
    // a failure here means a real DB problem the user needs to retry from
    // scratch, not auto-arm a second wipe attempt.
    let pending = {
        let mut p = lock_unpoisoned(&ctx.pending_clear);
        p.take()
    };

    let Some(started_at) = pending else {
        return "ℹ️ Нет ожидающего /clear. Сначала отправь /clear.".into();
    };
    if started_at.elapsed() > CLEAR_CONFIRM_WINDOW {
        return format!(
            "⏱ Время ожидания истекло ({} сек). Начни заново — /clear.",
            CLEAR_CONFIRM_WINDOW.as_secs()
        );
    }

    match ctx.storage.clear_seen() {
        Ok(deleted) => {
            info!(deleted, "seen_listings cleared via command");
            format!("✅ Удалено <b>{deleted}</b> объявлений из seen_listings.")
        }
        Err(e) => {
            warn!(error = %e, "clear_seen failed");
            "❌ Удаление сорвалось — глянь логи бота.".into()
        }
    }
}

// ===========================================================================
// /filter wizard — inline-keyboard driven
// ===========================================================================

/// `/filter` command: send a fresh menu message anchored to the user's chat.
/// All subsequent steps EDIT this message (rather than spamming new ones),
/// so the chat stays clean. teloxide doesn't care that the message keeps
/// the same `message_id` — only that the `chat_id` is right.
async fn handle_filter_start(bot: Bot, msg: &Message, ctx: &CommandContext) -> ResponseResult<()> {
    let (search, interval_secs) = {
        let r = ctx.runtime.read().await;
        (r.search.clone(), r.poll_interval.as_secs())
    };
    bot.send_message(msg.chat.id, format_filter_menu_body(&search, interval_secs))
        .parse_mode(ParseMode::Html)
        .reply_markup(filter_menu_keyboard())
        .await?;
    Ok(())
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
    if user_id != ctx.static_cfg.authorized_user_id {
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
            // edits. "Back without saving" semantics belong here, central,
            // rather than duplicated in every picker's Back path.
            *lock_unpoisoned(&ctx.chassis_draft) = None;
            *lock_unpoisoned(&ctx.models_draft) = None;

            let (search, interval_secs) = {
                let r = ctx.runtime.read().await;
                (r.search.clone(), r.poll_interval.as_secs())
            };
            (
                format_filter_menu_body(&search, interval_secs),
                Some(filter_menu_keyboard()),
            )
        }
        CB_FILTER_BRAND_PICKER => ("Выбери марку:".to_string(), Some(brand_picker_keyboard())),
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
            // Initialise draft = current runtime.chassis so the picker opens
            // with existing selections checked.
            let initial = ctx.runtime.read().await.search.chassis.clone();
            *lock_unpoisoned(&ctx.chassis_draft) = Some(initial.clone());
            (
                "Выбери типы кузова (можно несколько):".to_string(),
                Some(chassis_picker_keyboard(&initial)),
            )
        }
        CB_FILTER_CHASSIS_SAVE => {
            // Commit the draft to runtime + DB. The draft becomes the new
            // selection, even if empty (= "no chassis filter").
            let draft = lock_unpoisoned(&ctx.chassis_draft)
                .take()
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
        CB_FILTER_DONE => {
            // Same "leave dialog" cleanup as CB_FILTER_MENU: discard any
            // hanging drafts. Without this, an abandoned picker edit would
            // sit in memory until the bot restarted.
            *lock_unpoisoned(&ctx.chassis_draft) = None;
            *lock_unpoisoned(&ctx.models_draft) = None;

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
                None => (
                    "❌ Сначала выбери марку — без неё список моделей не известен.\n\n\
                     Возвращаюсь в меню."
                        .into(),
                    Some(filter_menu_keyboard()),
                ),
                Some(brand_slug) => match models_for_brand(&brand_slug) {
                    Some(model_list) => {
                        *lock_unpoisoned(&ctx.models_draft) = Some(initial_models.clone());
                        (
                            format!("Модели для <b>{brand_slug}</b> (можно несколько):"),
                            Some(model_picker_keyboard(model_list, &initial_models)),
                        )
                    }
                    None => (
                        format!(
                            "🤷 Для марки <code>{brand_slug}</code> у меня нет каталога моделей.\n\n\
                             Поставь модели через <code>SEARCH_MODEL</code> в <code>.env</code>, \
                             либо смени марку через ✏️ Марка."
                        ),
                        Some(filter_menu_keyboard()),
                    ),
                },
            }
        }
        CB_FILTER_MODELS_SAVE => {
            let draft = lock_unpoisoned(&ctx.models_draft)
                .take()
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
            let new_state = {
                let mut draft = lock_unpoisoned(&ctx.models_draft);
                let v = draft.get_or_insert_with(Vec::new);
                if let Some(pos) = v.iter().position(|s| s == slug) {
                    v.remove(pos);
                } else {
                    v.push(slug.to_owned());
                }
                v.clone()
            };
            (
                format!("Модели для <b>{brand_slug}</b> (можно несколько):"),
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
            let new_state = {
                let mut draft = lock_unpoisoned(&ctx.chassis_draft);
                let v = draft.get_or_insert_with(Vec::new);
                if let Some(pos) = v.iter().position(|&x| x == code) {
                    v.remove(pos);
                } else {
                    v.push(code);
                }
                v.clone()
            };
            (
                "Выбери типы кузова (можно несколько):".to_string(),
                Some(chassis_picker_keyboard(&new_state)),
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

/// Writes price range to DB + runtime + wakes the poll loop.
/// `0` (the wire encoding) means "no bound" — stored as empty string in DB
/// and `None` in `SearchFilter`.
async fn apply_price_range(ctx: &CommandContext, from: u32, to: u32) {
    let from_opt = (from > 0).then_some(from);
    let to_opt = (to > 0).then_some(to);
    let from_str = from_opt.map(|v| v.to_string()).unwrap_or_default();
    let to_str = to_opt.map(|v| v.to_string()).unwrap_or_default();

    // Two-key transaction: persist both bounds. Each `set_setting` is its own
    // SQLite transaction; if the second one fails we'd be half-applied. For
    // our scale (microsecond writes on local NVMe) this is extremely
    // unlikely. If it ever bit us, we'd add a multi-key transactional method
    // to Storage. Pragmatic for now.
    if let Err(e) = ctx
        .storage
        .set_setting(SETTING_SEARCH_PRICE_FROM, &from_str)
    {
        warn!(error = %e, "couldn't persist price_from");
        return;
    }
    if let Err(e) = ctx.storage.set_setting(SETTING_SEARCH_PRICE_TO, &to_str) {
        warn!(error = %e, "couldn't persist price_to");
        return;
    }

    let (old_from, old_to) = {
        let mut r = ctx.runtime.write().await;
        let old = (r.search.price_from, r.search.price_to);
        r.search.price_from = from_opt;
        r.search.price_to = to_opt;
        old
    };
    ctx.runtime_changed.notify_one();
    info!(?old_from, ?old_to, new_from = ?from_opt, new_to = ?to_opt, "price changed via /filter dialog");
}

/// Writes year range to DB + runtime + wakes the poll loop.
/// Same pattern as `apply_price_range` but with `u16` typing on the
/// `SearchFilter` side (year fits comfortably).
async fn apply_year_range(ctx: &CommandContext, from: u32, to: u32) {
    let from_opt = (from > 0).then_some(from as u16);
    let to_opt = (to > 0).then_some(to as u16);
    let from_str = from_opt.map(|v| v.to_string()).unwrap_or_default();
    let to_str = to_opt.map(|v| v.to_string()).unwrap_or_default();

    if let Err(e) = ctx.storage.set_setting(SETTING_SEARCH_YEAR_FROM, &from_str) {
        warn!(error = %e, "couldn't persist year_from");
        return;
    }
    if let Err(e) = ctx.storage.set_setting(SETTING_SEARCH_YEAR_TO, &to_str) {
        warn!(error = %e, "couldn't persist year_to");
        return;
    }

    let (old_from, old_to) = {
        let mut r = ctx.runtime.write().await;
        let old = (r.search.year_from, r.search.year_to);
        r.search.year_from = from_opt;
        r.search.year_to = to_opt;
        old
    };
    ctx.runtime_changed.notify_one();
    info!(?old_from, ?old_to, new_from = ?from_opt, new_to = ?to_opt, "year changed via /filter dialog");
}

/// Wipes every user-tunable filter field — brand, models, chassis, price, year.
/// Settings like `paused` / `poll_interval` are kept. `show_old_new` and
/// `without_price` aren't really "filters" in the everyday sense (they're
/// search-display options), so we leave those alone too.
async fn apply_reset_all(ctx: &CommandContext) {
    // Persist-first: write empty values for every filter key. We do this
    // serially because Storage doesn't have a multi-key transaction API.
    // If one write fails we bail without touching RAM — partial DB / clean
    // RAM is the safer half-state to recover from (next restart loads what
    // succeeded).
    for key in [
        SETTING_SEARCH_BRAND,
        SETTING_SEARCH_MODELS,
        SETTING_SEARCH_CHASSIS,
        SETTING_SEARCH_PRICE_FROM,
        SETTING_SEARCH_PRICE_TO,
        SETTING_SEARCH_YEAR_FROM,
        SETTING_SEARCH_YEAR_TO,
    ] {
        if let Err(e) = ctx.storage.set_setting(key, "") {
            warn!(error = %e, key, "couldn't clear filter setting during reset");
            return;
        }
    }

    {
        let mut r = ctx.runtime.write().await;
        r.search.brand = None;
        r.search.models.clear();
        r.search.chassis.clear();
        r.search.price_from = None;
        r.search.price_to = None;
        r.search.year_from = None;
        r.search.year_to = None;
    }

    ctx.runtime_changed.notify_one();
    info!("all filters reset via /filter dialog");
}

/// Writes models list to DB + runtime + wakes the poll loop.
/// Same pattern as `apply_chassis` but with `String` slugs.
async fn apply_models(ctx: &CommandContext, new_models: Vec<String>) {
    let stored_value = new_models.join(",");
    if let Err(e) = ctx
        .storage
        .set_setting(SETTING_SEARCH_MODELS, &stored_value)
    {
        warn!(error = %e, "couldn't persist search_models");
        return;
    }
    let old_models = {
        let mut r = ctx.runtime.write().await;
        let old = r.search.models.clone();
        r.search.models = new_models.clone();
        old
    };
    ctx.runtime_changed.notify_one();
    info!(
        ?old_models,
        ?new_models,
        "models changed via /filter dialog"
    );
}

/// Writes the new chassis list to DB + runtime + wakes the poll loop.
/// Same persistence-first pattern as `apply_brand`.
///
/// On-disk format: comma-separated `u32`s (`"2634,2632"`). An empty list
/// stores as `""` — the loader treats that as "no filter" rather than
/// "use env default".
async fn apply_chassis(ctx: &CommandContext, new_chassis: Vec<u32>) {
    let stored_value = new_chassis
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if let Err(e) = ctx
        .storage
        .set_setting(SETTING_SEARCH_CHASSIS, &stored_value)
    {
        warn!(error = %e, "couldn't persist search_chassis");
        return;
    }
    let old_chassis = {
        let mut r = ctx.runtime.write().await;
        let old = r.search.chassis.clone();
        r.search.chassis = new_chassis.clone();
        old
    };
    ctx.runtime_changed.notify_one();
    info!(
        ?old_chassis,
        ?new_chassis,
        "chassis changed via /filter dialog"
    );
}

/// Writes the new brand to DB + runtime + wakes the poll loop.
/// Same persistence-first pattern as `/pause` and `/interval`.
///
/// **Side effect**: when the brand *changes to a different value* (not just
/// re-confirmed), the model list is wiped too. Models are brand-specific —
/// leaving "Cooper" set after switching from MINI to Volvo produces a URL
/// like `brand=volvo&model[]=cooper` which polovni rightfully returns
/// zero results for. Better to surprise the user with a cleared model list
/// than with mysteriously empty search results.
async fn apply_brand(ctx: &CommandContext, new_brand: Option<String>) {
    let stored_value: &str = new_brand.as_deref().unwrap_or("");
    if let Err(e) = ctx.storage.set_setting(SETTING_SEARCH_BRAND, stored_value) {
        warn!(error = %e, "couldn't persist search_brand");
        return; // RAM stays at old value; menu redraw will show the old brand
    }

    // Snapshot the old brand so we can decide *whether* to clear models.
    let old_brand = ctx.runtime.read().await.search.brand.clone();
    let brand_actually_changed = old_brand != new_brand;

    // If the brand actually changed, persist a cleared models list FIRST
    // (same persistence-first rule). If that write fails, the brand change
    // also gets abandoned — invariant: brand-and-models stay consistent.
    if brand_actually_changed && let Err(e) = ctx.storage.set_setting(SETTING_SEARCH_MODELS, "") {
        warn!(error = %e, "couldn't clear search_models on brand change");
        return;
    }

    {
        let mut r = ctx.runtime.write().await;
        r.search.brand = new_brand.clone();
        if brand_actually_changed {
            r.search.models.clear();
        }
    }
    ctx.runtime_changed.notify_one();
    info!(
        ?old_brand,
        ?new_brand,
        models_cleared = brand_actually_changed,
        "brand changed via /filter dialog"
    );
}

/// Top-level menu body: short status + "what to edit" hint.
/// The buttons themselves carry the per-field current values for at-a-glance.
/// Takes `interval_secs` so the body can show the poll cadence (interval lives
/// in `RuntimeConfig`, not `SearchFilter`, so we accept it separately rather
/// than threading the whole config through).
fn format_filter_menu_body(f: &SearchFilter, interval_secs: u64) -> String {
    format!(
        "🎛 <b>Фильтры и настройки</b>\n\n\
         ⏱ Интервал поллинга: <b>{interval_secs}</b> сек\n\n\
         <b>Фильтры</b>\n{filter}\n\n\
         Жми кнопку для секции, которую хочешь поменять, или <b>Готово</b> когда всё хорошо.",
        filter = format_filter_ru(f),
    )
}

/// Top-level menu keyboard: one button per filter section, plus Done.
/// Each button label is just the section name — values are in the message
/// body. (Putting values on labels would make them long and ugly.)
fn filter_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("✏️ Марка", CB_FILTER_BRAND_PICKER),
            InlineKeyboardButton::callback("✏️ Модели", CB_FILTER_MODELS_PICKER),
        ],
        vec![
            InlineKeyboardButton::callback("✏️ Кузов", CB_FILTER_CHASSIS_PICKER),
            InlineKeyboardButton::callback("✏️ Цена", CB_FILTER_PRICE_PICKER),
        ],
        vec![
            InlineKeyboardButton::callback("✏️ Год", CB_FILTER_YEAR_PICKER),
            InlineKeyboardButton::callback("⏱ Интервал", CB_FILTER_INTERVAL_PICKER),
        ],
        vec![
            InlineKeyboardButton::callback("🧹 Сбросить", CB_FILTER_RESET_CONFIRM),
            InlineKeyboardButton::callback("✅ Готово", CB_FILTER_DONE),
        ],
    ])
}

/// Interval picker keyboard. Same single-tap-commit pattern as price/year
/// (no draft state); the `✓` highlights the currently-selected interval if
/// it matches a preset.
fn interval_picker_keyboard(current_secs: u64) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = INTERVAL_PRESETS
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(secs, display)| {
                    let prefix = if *secs == current_secs { "✓ " } else { "" };
                    InlineKeyboardButton::callback(
                        format!("{prefix}{display}"),
                        format!("{CB_FILTER_INTERVAL_SET_PREFIX}{secs}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![InlineKeyboardButton::callback(
        "↩️ Назад",
        CB_FILTER_MENU,
    )]);
    InlineKeyboardMarkup::new(rows)
}

/// Two-button confirmation keyboard used for the "Reset all filters?" prompt.
/// `[↩️ Отмена]` reuses `CB_FILTER_MENU` so the central back-to-menu logic
/// (drafts cleanup, menu redraw) handles it without a new branch.
fn reset_confirm_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Да, стереть", CB_FILTER_RESET_APPLY),
        InlineKeyboardButton::callback("↩️ Отмена", CB_FILTER_MENU),
    ]])
}

/// Renders a range picker for either price or year.
///
/// Each button represents a complete `(from, to)` range — single-tap commits.
/// The button matching the current filter (if any preset matches) gets a `✓`
/// prefix; the catch-all "Без фильтра" gets `✓` if the current range is
/// `(None, None)`. Custom ranges set via env get no highlight — that's fine,
/// they're an escape hatch, not a UI state.
fn range_picker_keyboard(
    field: &str,
    catalog: &[(u32, u32, &str)],
    current: (Option<u32>, Option<u32>),
) -> InlineKeyboardMarkup {
    let mark = |btn_from: u32, btn_to: u32| -> &'static str {
        let cf = if btn_from == 0 { None } else { Some(btn_from) };
        let ct = if btn_to == 0 { None } else { Some(btn_to) };
        if (cf, ct) == current { "✓ " } else { "" }
    };

    // 2 buttons per row for the presets.
    let mut rows: Vec<Vec<InlineKeyboardButton>> = catalog
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(from, to, display)| {
                    InlineKeyboardButton::callback(
                        format!("{}{}", mark(*from, *to), display),
                        format!("{CB_FILTER_RANGE_SET_PREFIX}{field}:{from}:{to}"),
                    )
                })
                .collect()
        })
        .collect();

    // Trailing row: "no filter" + back.
    let no_filter_label = format!("{}Без фильтра", mark(0, 0));
    rows.push(vec![
        InlineKeyboardButton::callback(
            no_filter_label,
            format!("{CB_FILTER_RANGE_SET_PREFIX}{field}:0:0"),
        ),
        InlineKeyboardButton::callback("↩️ Назад", CB_FILTER_MENU),
    ]);
    InlineKeyboardMarkup::new(rows)
}

/// Model picker keyboard for a specific brand. Same multi-select shape as
/// chassis (✓/⬜ prefixes, toggle callbacks, Save/Back row).
///
/// `brand_slug` is captured into nothing here — the catalog lookup happens
/// at the call site; we only need the per-brand model list to render.
fn model_picker_keyboard(
    models: &[(&'static str, &'static str)],
    selected: &[String],
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = models
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(slug, display)| {
                    let checked = selected.iter().any(|s| s == slug);
                    let prefix = if checked { "✓ " } else { "⬜ " };
                    InlineKeyboardButton::callback(
                        format!("{prefix}{display}"),
                        format!("{CB_FILTER_MODELS_TOGGLE_PREFIX}{slug}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![
        InlineKeyboardButton::callback("💾 Сохранить", CB_FILTER_MODELS_SAVE),
        InlineKeyboardButton::callback("↩️ Назад (без сохранения)", CB_FILTER_MENU),
    ]);
    InlineKeyboardMarkup::new(rows)
}

/// Chassis picker keyboard: a 2-wide grid of body-type buttons with a check
/// mark for items currently in `selected`, plus a Save / Back row at the
/// bottom.
///
/// Rebuilt fresh on every render — there's no clever caching. With 6 items
/// it costs nothing.
fn chassis_picker_keyboard(selected: &[u32]) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = CHASSIS
        .chunks(2)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(code, display)| {
                    // ✓ / ⬜ prefix communicates checked state. The `code` is
                    // baked into callback_data so the handler doesn't need to
                    // look up the label-to-code mapping.
                    let prefix = if selected.contains(code) {
                        "✓ "
                    } else {
                        "⬜ "
                    };
                    InlineKeyboardButton::callback(
                        format!("{prefix}{display}"),
                        format!("{CB_FILTER_CHASSIS_TOGGLE_PREFIX}{code}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![
        InlineKeyboardButton::callback("💾 Сохранить", CB_FILTER_CHASSIS_SAVE),
        InlineKeyboardButton::callback("↩️ Назад (без сохранения)", CB_FILTER_MENU),
    ]);
    InlineKeyboardMarkup::new(rows)
}

/// Brand picker: a 4-wide grid of brand buttons plus "skip" and "back".
///
/// `chunks(4)` cleanly groups the 20 brands into 5 rows of 4. If the list
/// ever grows non-multiple-of-4, the final row will be short — TG handles
/// that fine.
fn brand_picker_keyboard() -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = BRANDS
        .chunks(4)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(slug, display)| {
                    InlineKeyboardButton::callback(
                        *display,
                        format!("{CB_FILTER_BRAND_SET_PREFIX}{slug}"),
                    )
                })
                .collect()
        })
        .collect();
    rows.push(vec![
        InlineKeyboardButton::callback("⏭ Без фильтра", CB_FILTER_BRAND_CLEAR),
        InlineKeyboardButton::callback("💬 Ввести вручную", CB_FILTER_BRAND_CUSTOM_HINT),
    ]);
    rows.push(vec![InlineKeyboardButton::callback(
        "↩️ Назад",
        CB_FILTER_MENU,
    )]);
    InlineKeyboardMarkup::new(rows)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[test]
    fn chassis_label_maps_known_codes_and_passes_through_unknown() {
        assert_eq!(chassis_label(2634), "Кабриолет");
        assert_eq!(chassis_label(2632), "Внедорожник");
        // Not in the catalog (e.g. set via SEARCH_CHASSIS in .env) — raw code.
        assert_eq!(chassis_label(9999), "9999");
    }

    #[test]
    fn filter_summary_renders_chassis_as_labels() {
        let f = SearchFilter {
            chassis: vec![2634, 9999],
            ..Default::default()
        };
        let s = format_filter_ru(&f);
        assert!(s.contains("Кабриолет, 9999"), "{s}");
    }

    #[test]
    fn status_search_url_is_html_escaped() {
        // A multi-param filter produces `&` separators in the query string;
        // inside Telegram HTML they must be escaped to `&amp;`.
        let f = SearchFilter {
            brand: Some("mini".into()),
            models: vec!["cooper".into()],
            ..Default::default()
        };
        let escaped = escape_html_attr_for_dump(f.to_url().as_str());
        assert!(escaped.contains("&amp;"), "{escaped}");
        // No bare `&` left: every `&` must start an entity we produced.
        for (i, _) in escaped.match_indices('&') {
            assert!(
                escaped[i..].starts_with("&amp;")
                    || escaped[i..].starts_with("&lt;")
                    || escaped[i..].starts_with("&gt;")
                    || escaped[i..].starts_with("&quot;"),
                "bare & at {i} in {escaped}"
            );
        }
    }
}
