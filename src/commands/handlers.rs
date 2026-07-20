//! Per-command handlers, the `apply_*` state-change helpers behind the
//! /filter wizard, and the message formatters.
//!
//! Split of responsibilities with `mod.rs`: routing (which update goes
//! where, authorization) lives there; *what each command actually does*
//! lives here. Handlers return `String` replies where possible so they
//! stay unit-testable without a live `Bot`.

use std::time::{Duration, Instant};

use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tracing::{info, warn};

use crate::config::{
    MAX_POLL_INTERVAL_SECS, MIN_POLL_INTERVAL_SECS, SETTING_PAUSED, SETTING_POLL_INTERVAL_SECS,
    SETTING_SEARCH_BRAND, SETTING_SEARCH_CHASSIS, SETTING_SEARCH_GEARBOX, SETTING_SEARCH_MODELS,
    SETTING_SEARCH_PRICE_FROM, SETTING_SEARCH_PRICE_TO, SETTING_SEARCH_YEAR_FROM,
    SETTING_SEARCH_YEAR_TO,
};
use crate::models::SearchFilter;
use crate::telegram::{escape_html, escape_html_attr};

use super::catalog::{chassis_label, gearbox_label};
use super::keyboards::filter_menu_keyboard;
use super::{CLEAR_CONFIRM_WINDOW, CommandContext, lock_unpoisoned};

pub(super) fn format_start() -> String {
    "👋 <b>Привет!</b> Я NjuskaAutoBot — слежу за объявлениями на \
     polovniautomobili.com и кидаю тебе сюда новые.\n\n\
     /help — список команд\n\
     /status — текущая конфигурация\n\
     /pause /resume — поставить на паузу и возобновить"
        .into()
}

pub(super) async fn format_status(ctx: &CommandContext) -> String {
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
    let search_url = escape_html_attr(search.to_url().as_str());

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
/// Kept in the commands module because it's a UI concern, not part of the
/// `SearchFilter` type itself — same struct could be rendered differently
/// for logs or for another locale.
pub(super) fn format_filter_ru(f: &SearchFilter) -> String {
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
        "• КПП: <code>{}</code>",
        if f.gearbox.is_empty() {
            "—".to_string()
        } else {
            f.gearbox
                .iter()
                .map(|g| gearbox_label(*g))
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

pub(super) async fn handle_pause(ctx: &CommandContext) -> String {
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
pub(super) async fn handle_set_brand(ctx: &CommandContext, raw_slug: String) -> String {
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
pub(super) async fn handle_dump(ctx: &CommandContext, n: u32) -> String {
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
    // Pass the *post-cap* `limit`, not the raw `n` (#45). The header decides
    // "showing all rows" by comparing `listings.len()` against this value; with
    // the raw `n`, a `/dump 100` against a >25-row store made 25 < 100 true and
    // falsely printed "Все 25 объявлений в БД" — the MAX cap, not the DB size.
    format_dump_message(&listings, limit)
}

/// Pure formatter behind `/dump` — separated from the handler so the
/// budget/escaping behavior is unit-testable with adversarial data (#26).
///
/// Compact one-line-per-listing format (different from
/// `telegram::format_listing_html`, the per-card notification layout).
/// Two defenses against Telegram's 4096-char sendMessage limit:
/// titles are hard-truncated, and lines stop (with a "… ещё N" note) once
/// the running total approaches the limit — 25 normal listings fit, 25
/// pathological ones degrade gracefully instead of triggering a 400.
///
/// `limit` is the **post-cap** row budget the caller asked storage for (`MAX`
/// at most), not the user's raw `/dump N`. The "showing all rows" header only
/// fires when the store returned *fewer* rows than that budget — otherwise a
/// `/dump 100` truncated by the cap would falsely claim the DB holds 25 (#45).
fn format_dump_message(listings: &[crate::models::Listing], limit: u32) -> String {
    /// Telegram's hard cap on message length, in characters.
    const TG_MESSAGE_LIMIT: usize = 4096;
    /// Slack reserved for the "… ещё N" tail note.
    const TAIL_RESERVE: usize = 64;
    /// Titles longer than this (pre-escaping) get an ellipsis.
    const MAX_TITLE_CHARS: usize = 120;

    let header = if listings.len() < limit as usize {
        format!(
            "📋 Все <b>{}</b> объявлений в БД (запрошено {}):",
            listings.len(),
            limit
        )
    } else {
        format!(
            "📋 Последние <b>{}</b> объявлений (новейшие сверху):",
            listings.len()
        )
    };

    let mut out = header;
    out.push('\n');
    let mut used = out.chars().count();
    for (i, l) in listings.iter().enumerate() {
        let price = l.price_text.as_deref().unwrap_or("—");
        let year = l.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into());
        let city = l.city.as_deref().unwrap_or("?");
        let mut title: String = l.title.chars().take(MAX_TITLE_CHARS).collect();
        if title.chars().count() < l.title.chars().count() {
            title.push('…');
        }
        let line = format!(
            "{n}. <a href=\"{url}\">{title}</a> — {price} · {year} · {city}",
            n = i + 1,
            url = escape_html_attr(&l.url),
            title = escape_html(&title),
            price = escape_html(price),
            year = year,
            city = escape_html(city),
        );
        let line_len = line.chars().count() + 1; // +1 for the newline
        if used + line_len > TG_MESSAGE_LIMIT - TAIL_RESERVE {
            out.push_str(&format!(
                "\n… и ещё {} — не влезло в одно сообщение.",
                listings.len() - i
            ));
            break;
        }
        out.push('\n');
        out.push_str(&line);
        used += line_len;
    }
    out
}

/// `/diag` — one-shot end-to-end fetch diagnostic (#2). Runs the exact same
/// fetch + parse pipeline the poll loop uses (proxy included) and reports
/// each leg in human terms, so "why is the bot quiet?" is answerable from
/// the chat without ssh-ing into the box.
pub(super) async fn handle_diag(ctx: &CommandContext) -> String {
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
        format!("URL: <code>{}</code>", escape_html(url.as_str())),
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
                escape_html(&crate::bot::describe_fetch_error(&e, proxy.is_some()))
            ));
        }
    }
    lines.join("\n")
}

/// `/cancel` — informational no-op. Useful for users who instinctively type
/// /cancel when they got lost; we explain the correct way out.
pub(super) fn format_cancel() -> String {
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
pub(super) async fn apply_interval(ctx: &CommandContext, secs: u64) -> Result<u64, String> {
    if secs < MIN_POLL_INTERVAL_SECS {
        return Err(format!(
            "Минимум <b>{MIN_POLL_INTERVAL_SECS}</b> секунд — это вежливость к сайту."
        ));
    }
    if secs > MAX_POLL_INTERVAL_SECS {
        return Err(format!(
            "Максимум <b>{MAX_POLL_INTERVAL_SECS}</b> секунд (неделя) — больше похоже на опечатку."
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
pub(super) async fn handle_interval(ctx: &CommandContext, secs: u64) -> String {
    match apply_interval(ctx, secs).await {
        Ok(_) => format!("✅ Интервал поллинга: <b>{secs}</b> сек. Применилось сразу."),
        Err(reason) => format!("❌ {reason}"),
    }
}

pub(super) async fn handle_resume(ctx: &CommandContext) -> String {
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

/// Two-step destructive op: `/clear` arms a pending state; `/clear_confirm`
/// within [`CLEAR_CONFIRM_WINDOW`] actually wipes. Without this gate, a
/// fat-finger near the input bar could nuke the whole dedup set.
///
/// `handle_clear` is synchronous — we only touch the map, no I/O. Keyed by the
/// sending `user_id` (#42) so each authorized user arms their own slot.
pub(super) fn handle_clear(ctx: &CommandContext, user_id: i64) -> String {
    // `lock_unpoisoned` mirrors storage's stance: poisoning means a holder
    // panicked, which is a bug to surface loudly, not a runtime error.
    let mut pending = lock_unpoisoned(&ctx.pending_clear);
    pending.insert(user_id, Instant::now());

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

pub(super) async fn handle_clear_confirm(ctx: &CommandContext, user_id: i64) -> String {
    // Read & consume *this user's* pending timestamp in one critical section
    // (#42): only the id that armed the `/clear` can consume it, so a second
    // authorized user's `/clear_confirm` can't wipe on someone else's behalf.
    // Even if the deletion below failed, we still consume the pending state —
    // a failure here means a real DB problem the user needs to retry from
    // scratch, not auto-arm a second wipe attempt.
    let pending = {
        let mut p = lock_unpoisoned(&ctx.pending_clear);
        p.remove(&user_id)
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

/// `/filter` command: send a fresh menu message anchored to the user's chat.
/// All subsequent steps EDIT this message (rather than spamming new ones),
/// so the chat stays clean. teloxide doesn't care that the message keeps
/// the same `message_id` — only that the `chat_id` is right.
pub(super) async fn handle_filter_start(
    bot: Bot,
    msg: &Message,
    ctx: &CommandContext,
) -> ResponseResult<()> {
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

/// Writes price range to DB + runtime + wakes the poll loop.
/// `0` (the wire encoding) means "no bound" — stored as empty string in DB
/// and `None` in `SearchFilter`.
pub(super) async fn apply_price_range(ctx: &CommandContext, from: u32, to: u32) {
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
pub(super) async fn apply_year_range(ctx: &CommandContext, from: u32, to: u32) {
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
pub(super) async fn apply_reset_all(ctx: &CommandContext) {
    // Persist-first: write empty values for every filter key. We do this
    // serially because Storage doesn't have a multi-key transaction API.
    // If one write fails we bail without touching RAM — partial DB / clean
    // RAM is the safer half-state to recover from (next restart loads what
    // succeeded).
    for key in [
        SETTING_SEARCH_BRAND,
        SETTING_SEARCH_MODELS,
        SETTING_SEARCH_CHASSIS,
        SETTING_SEARCH_GEARBOX,
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
        r.search.gearbox.clear();
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
pub(super) async fn apply_models(ctx: &CommandContext, new_models: Vec<String>) {
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
pub(super) async fn apply_chassis(ctx: &CommandContext, new_chassis: Vec<u32>) {
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

/// Writes the new gearbox list to DB + runtime + wakes the poll loop.
/// Same on-disk format and persistence-first pattern as `apply_chassis`.
pub(super) async fn apply_gearbox(ctx: &CommandContext, new_gearbox: Vec<u32>) {
    let stored_value = new_gearbox
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if let Err(e) = ctx
        .storage
        .set_setting(SETTING_SEARCH_GEARBOX, &stored_value)
    {
        warn!(error = %e, "couldn't persist search_gearbox");
        return;
    }
    let old_gearbox = {
        let mut r = ctx.runtime.write().await;
        let old = r.search.gearbox.clone();
        r.search.gearbox = new_gearbox.clone();
        old
    };
    ctx.runtime_changed.notify_one();
    info!(
        ?old_gearbox,
        ?new_gearbox,
        "gearbox changed via /filter dialog"
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
pub(super) async fn apply_brand(ctx: &CommandContext, new_brand: Option<String>) {
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
pub(super) fn format_filter_menu_body(f: &SearchFilter, interval_secs: u64) -> String {
    format!(
        "🎛 <b>Фильтры и настройки</b>\n\n\
         ⏱ Интервал поллинга: <b>{interval_secs}</b> сек\n\n\
         <b>Фильтры</b>\n{filter}\n\n\
         Жми кнопку для секции, которую хочешь поменять, или <b>Готово</b> когда всё хорошо.",
        filter = format_filter_ru(f),
    )
}

/// Title line of the model picker. Brand slugs from our catalog and the
/// validated `/setbrand` path are tame, but `SEARCH_BRAND` in `.env` is
/// free-form — escape rather than trust, or a slug like `a<b` would 400
/// every picker render (#26).
pub(super) fn models_picker_title(brand_slug: &str) -> String {
    format!(
        "Модели для <b>{}</b> (можно несколько):",
        escape_html(brand_slug)
    )
}

/// Builds the reply for a command-shaped message that failed `Command::parse`
/// (#8): `/interval abc`, bare `/dump`, or an unknown `/frobnicate`.
///
/// Two cases:
/// * The base command exists → almost certainly an argument problem; repeat
///   that command's own description (they embed examples where args exist).
/// * Unknown command → point at `/help`.
///
/// Pure function of the message text so it's unit-testable; the descriptions
/// come from the same derive that feeds `/help`, so hints never drift.
pub(super) fn usage_hint(text: &str) -> String {
    use teloxide::utils::command::BotCommands;

    // First whitespace token is the command; `@BotName` suffixes are how TG
    // disambiguates commands in group chats — strip before matching.
    let first = text.split_whitespace().next().unwrap_or(text);
    let name = first.trim_start_matches('/');
    let name = name.split('@').next().unwrap_or(name);

    let known = super::Command::bot_commands();
    match known
        .iter()
        .find(|c| c.command.trim_start_matches('/') == name)
    {
        Some(c) => format!(
            "❌ Не понял аргументы для <code>/{name}</code>.\n{}",
            escape_html(&c.description)
        ),
        None => format!(
            "🤷 Не знаю команду <code>{}</code>. Список команд — /help.",
            escape_html(first)
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use tokio::sync::{Notify, RwLock};

    use crate::config::{RuntimeConfig, StaticConfig};
    use crate::storage::Storage;

    use super::*;

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
    fn filter_summary_renders_gearbox_as_labels() {
        // Known codes render as their Russian labels; out-of-catalog codes
        // (env escape hatch) pass through raw — same contract as chassis.
        let f = SearchFilter {
            gearbox: vec![10795, 8888],
            ..Default::default()
        };
        let s = format_filter_ru(&f);
        assert!(
            s.contains("КПП: <code>Автомат / полуавтомат, 8888</code>"),
            "{s}"
        );
    }

    #[test]
    fn filter_summary_renders_dashes_for_empty_filter() {
        let s = format_filter_ru(&SearchFilter::default());
        // Every unset field shows an em-dash placeholder, not an empty gap.
        assert!(s.contains("Марка: <code>—</code>"), "{s}");
        assert!(s.contains("Модели: <code>—</code>"), "{s}");
        assert!(s.contains("КПП: <code>—</code>"), "{s}");
        assert!(s.contains("Цена: <code>— – —</code>"), "{s}");
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
        let escaped = escape_html_attr(f.to_url().as_str());
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

    #[test]
    fn models_picker_title_escapes_hostile_brand_slug() {
        // SEARCH_BRAND in .env is free-form; a hostile/typo'd slug must not
        // break the picker's HTML (#26).
        let title = models_picker_title("a<b>&\"c");
        assert!(title.contains("a&lt;b&gt;&amp;\"c"), "{title}");
        assert!(!title.contains("<b>&"), "{title}");
        // Sane slugs render unchanged.
        assert!(models_picker_title("mini").contains("<b>mini</b>"));
    }

    #[test]
    fn usage_hint_for_known_command_repeats_its_description() {
        // Bad args on a real command → its own description, which carries
        // the example.
        let hint = usage_hint("/interval abc");
        assert!(hint.contains("<code>/interval</code>"), "{hint}");
        assert!(hint.contains("/interval 300"), "{hint}");
        // Bare /dump (missing required arg) is the same case.
        let hint = usage_hint("/dump");
        assert!(hint.contains("<code>/dump</code>"), "{hint}");
        assert!(hint.contains("/dump 10"), "{hint}");
    }

    #[test]
    fn usage_hint_strips_bot_mention_before_matching() {
        // Group-chat form: /interval@SomeBot 5x — still the known-command path.
        let hint = usage_hint("/interval@NjuskaAutoBot abc");
        assert!(hint.contains("<code>/interval</code>"), "{hint}");
    }

    #[test]
    fn usage_hint_for_unknown_command_points_at_help() {
        let hint = usage_hint("/frobnicate now");
        assert!(hint.contains("/help"), "{hint}");
        assert!(hint.contains("frobnicate"), "{hint}");
    }

    #[test]
    fn usage_hint_escapes_hostile_input() {
        // The echoed command name is user-controlled — must be HTML-safe.
        let hint = usage_hint("/x<script>&");
        assert!(hint.contains("&lt;script&gt;"), "{hint}");
        assert!(!hint.contains("<script>"), "{hint}");
    }

    #[test]
    fn filter_menu_body_shows_interval_and_filters() {
        let body = format_filter_menu_body(&SearchFilter::default(), 300);
        assert!(body.contains("<b>300</b>"), "{body}");
        assert!(body.contains("Марка"), "{body}");
    }

    fn dump_listing(id: u64, title: &str, url: &str) -> crate::models::Listing {
        crate::models::Listing {
            id,
            title: title.into(),
            url: url.into(),
            price_text: Some("1.000 €".into()),
            city: Some("Beograd".into()),
            year: Some(2015),
            mileage_km: Some(100_000),
        }
    }

    #[test]
    fn dump_message_normal_case_lists_everything_without_truncation() {
        let listings: Vec<_> = (1..=25)
            .map(|i| {
                dump_listing(
                    i,
                    &format!("Car {i}"),
                    &format!("https://example.com/auto-oglasi/{i}/car"),
                )
            })
            .collect();
        let msg = format_dump_message(&listings, 25);
        assert!(msg.contains("25. "), "{msg}");
        assert!(!msg.contains("не влезло"), "{msg}");
        assert!(msg.chars().count() <= 4096);
    }

    #[test]
    fn dump_message_stays_under_telegram_limit_with_monster_titles() {
        // Adversarial: 25 listings with 500-char titles full of specials.
        let monster = "α<>&\"🚗".repeat(84); // ~504 chars, multibyte + specials
        let listings: Vec<_> = (1..=25)
            .map(|i| {
                dump_listing(
                    i,
                    &monster,
                    &format!("https://example.com/auto-oglasi/{i}/x?a=1&b=\"2\""),
                )
            })
            .collect();
        let msg = format_dump_message(&listings, 25);
        assert!(
            msg.chars().count() <= 4096,
            "must fit Telegram's limit, got {} chars",
            msg.chars().count()
        );
        // Degrades gracefully: tail note instead of a 400 from Telegram.
        assert!(msg.contains("не влезло"), "{msg}");
        // Raw specials must never survive into element content.
        assert!(!msg.contains("<>&"), "{msg}");
        // Quotes in URLs must be entity-escaped inside href.
        assert!(msg.contains("&quot;2&quot;"), "{msg}");
    }

    #[test]
    fn dump_message_truncates_a_single_huge_title_but_keeps_the_line() {
        let huge = "X".repeat(3000);
        let listings = vec![dump_listing(1, &huge, "https://example.com/1")];
        let msg = format_dump_message(&listings, 1);
        assert!(msg.chars().count() <= 4096);
        // Title capped with an ellipsis rather than dropping the listing.
        assert!(msg.contains("X…"), "{msg}");
        assert!(msg.contains("<a href="), "{msg}");
    }

    #[test]
    fn dump_message_passes_rtl_and_zero_width_through_untouched() {
        // RTL override and zero-width space are display-level nuisances, not
        // HTML specials — escaping must not mangle them (Telegram renders
        // them inert inside a link).
        let sneaky = "BMW \u{202E}looc\u{200B} car";
        let listings = vec![dump_listing(1, sneaky, "https://example.com/1")];
        let msg = format_dump_message(&listings, 1);
        assert!(msg.contains('\u{202E}'), "{msg}");
        assert!(msg.contains('\u{200B}'), "{msg}");
    }

    #[tokio::test]
    async fn dump_over_cap_with_large_store_does_not_claim_all_rows() {
        // #45: >25 stored and `/dump 100`. The 25-item cap truncates, but the
        // header must NOT print "Все 25 … в БД" — that count is the cap, not the
        // DB size. handle_dump has to pass the post-cap limit to the formatter.
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(&dir.path().join("cmd.db"));

        let stored: Vec<_> = (1..=30)
            .map(|i| {
                dump_listing(
                    i,
                    &format!("Car {i}"),
                    &format!("https://example.com/auto-oglasi/{i}/car"),
                )
            })
            .collect();
        ctx.storage.mark_seen(&stored).unwrap();

        let msg = handle_dump(&ctx, 100).await;
        assert!(
            !msg.contains("Все"),
            "must not claim the DB holds only 25: {msg}"
        );
        assert!(msg.contains("Последние"), "{msg}");

        // A store genuinely smaller than the request still says "Все".
        let dir2 = tempfile::tempdir().unwrap();
        let ctx2 = test_ctx(&dir2.path().join("cmd.db"));
        ctx2.storage.mark_seen(&stored[..3]).unwrap();
        let small = handle_dump(&ctx2, 100).await;
        assert!(small.contains("Все <b>3</b>"), "{small}");
    }

    /// A real `CommandContext` over a temp SQLite file (never the real DB),
    /// runtime seeded at 600s — enough to exercise the `apply_*` helpers
    /// without a live `Bot`.
    fn test_ctx(db_path: &std::path::Path) -> CommandContext {
        CommandContext {
            static_cfg: Arc::new(StaticConfig {
                database_path: db_path.to_path_buf(),
                telegram_token: "t".into(),
                telegram_chat_id: 1,
                authorized_user_ids: vec![111],
                save_raw_html: false,
                max_search_pages: 1,
                zero_results_alert_threshold: 3,
                fetch_errors_alert_threshold: 3,
                dumps_dir: PathBuf::from("/tmp"),
                dump_retention_days: 0,
                dump_max_total_mb: 0,
                seen_retention_days: 0,
                cf_proxy: None,
            }),
            runtime: Arc::new(RwLock::new(RuntimeConfig {
                search: SearchFilter::default(),
                poll_interval: Duration::from_secs(600),
                paused: false,
            })),
            storage: Arc::new(Storage::new(db_path).unwrap()),
            runtime_changed: Arc::new(Notify::new()),
            chassis_draft: Arc::new(Mutex::new(HashMap::new())),
            models_draft: Arc::new(Mutex::new(HashMap::new())),
            gearbox_draft: Arc::new(Mutex::new(HashMap::new())),
            pending_clear: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn interval_out_of_range_is_rejected_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(&dir.path().join("cmd.db"));

        // Above the ceiling (#54): a human reply naming the limit, not a
        // silent clamp — the user should learn the rule, not guess it.
        let err = apply_interval(&ctx, MAX_POLL_INTERVAL_SECS + 1)
            .await
            .unwrap_err();
        assert!(err.contains(&MAX_POLL_INTERVAL_SECS.to_string()), "{err}");

        // Below the floor: same contract, pinned here alongside the ceiling.
        let err = apply_interval(&ctx, MIN_POLL_INTERVAL_SECS - 1)
            .await
            .unwrap_err();
        assert!(err.contains(&MIN_POLL_INTERVAL_SECS.to_string()), "{err}");

        // "Changes nothing" means both halves: RAM still at the seed value,
        // and nothing persisted for the next restart to trip over.
        assert_eq!(
            ctx.runtime.read().await.poll_interval,
            Duration::from_secs(600)
        );
        assert_eq!(
            ctx.storage.get_setting(SETTING_POLL_INTERVAL_SECS).unwrap(),
            None
        );

        // The ceiling itself is legal — guards the bound check against
        // an off-by-one — and goes through RAM + DB like any valid value.
        let old = apply_interval(&ctx, MAX_POLL_INTERVAL_SECS).await.unwrap();
        assert_eq!(old, 600);
        assert_eq!(
            ctx.runtime.read().await.poll_interval,
            Duration::from_secs(MAX_POLL_INTERVAL_SECS)
        );
        assert_eq!(
            ctx.storage
                .get_setting(SETTING_POLL_INTERVAL_SECS)
                .unwrap()
                .as_deref(),
            Some(MAX_POLL_INTERVAL_SECS.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn clear_confirm_is_isolated_per_user() {
        // #42 (analogous to `draft_toggle_is_isolated_per_user`): the two-step
        // `/clear` gate must be scoped to the user who armed it, so a second
        // authorized user's `/clear_confirm` can't consume someone else's
        // pending wipe within the 30s window.
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(&dir.path().join("cmd.db"));

        // User A arms a pending /clear.
        let armed = handle_clear(&ctx, 111);
        assert!(armed.contains("Опасная операция"), "{armed}");

        // User B's /clear_confirm sees no pending request of their own — it
        // must NOT consume A's slot.
        let b = handle_clear_confirm(&ctx, 222).await;
        assert!(b.contains("Нет ожидающего"), "{b}");

        // A's request survived B's attempt: A can still confirm and wipe.
        let a = handle_clear_confirm(&ctx, 111).await;
        assert!(a.contains("Удалено"), "{a}");

        // A's slot is now consumed — a repeat finds nothing to confirm.
        let a_again = handle_clear_confirm(&ctx, 111).await;
        assert!(a_again.contains("Нет ожидающего"), "{a_again}");
    }

    #[tokio::test]
    async fn clear_confirm_rejects_absent_and_expired_requests() {
        // #44: window arithmetic in `handle_clear_confirm`. Absent slot and a
        // slot older than `CLEAR_CONFIRM_WINDOW` are both refused without
        // wiping.
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(&dir.path().join("cmd.db"));

        // Nothing armed yet.
        let absent = handle_clear_confirm(&ctx, 111).await;
        assert!(absent.contains("Нет ожидающего"), "{absent}");

        // Arm a stale request by back-dating the timestamp past the window.
        let stale = Instant::now()
            .checked_sub(CLEAR_CONFIRM_WINDOW + Duration::from_secs(1))
            .unwrap();
        lock_unpoisoned(&ctx.pending_clear).insert(111, stale);
        let expired = handle_clear_confirm(&ctx, 111).await;
        assert!(expired.contains("истекло"), "{expired}");

        // Expired confirm still consumes the slot (no auto-armed second wipe).
        assert!(!lock_unpoisoned(&ctx.pending_clear).contains_key(&111));
    }

    #[tokio::test]
    async fn apply_brand_clears_models_only_when_brand_changed() {
        // #44: brand-change-clears-models invariant. Re-confirming the same
        // brand must keep the model list; switching brands must wipe it (stale
        // models produce empty polovni results).
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(&dir.path().join("cmd.db"));

        apply_brand(&ctx, Some("mini".into())).await;
        apply_models(&ctx, vec!["cooper".into()]).await;

        // Re-confirm the identical brand: models survive.
        apply_brand(&ctx, Some("mini".into())).await;
        {
            let r = ctx.runtime.read().await;
            assert_eq!(r.search.brand.as_deref(), Some("mini"));
            assert_eq!(r.search.models, vec!["cooper".to_string()]);
        }

        // Switch brand: models are cleared in RAM and persisted empty.
        apply_brand(&ctx, Some("volvo".into())).await;
        {
            let r = ctx.runtime.read().await;
            assert_eq!(r.search.brand.as_deref(), Some("volvo"));
            assert!(r.search.models.is_empty());
        }
        assert_eq!(
            ctx.storage.get_setting(SETTING_SEARCH_MODELS).unwrap(),
            Some(String::new())
        );
    }

    #[tokio::test]
    async fn apply_brand_failed_persist_leaves_ram_untouched() {
        // #44: the persist-DB-before-mutating-RAM rule CONTRIBUTING.md names.
        // We force `set_setting` to fail by dropping the settings table from a
        // second connection to the same file, then assert RAM is unchanged.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cmd.db");
        let ctx = test_ctx(&db_path);

        apply_brand(&ctx, Some("mini".into())).await;
        apply_models(&ctx, vec!["cooper".into()]).await;

        // Break the DB out from under Storage: the next INSERT hits a missing
        // table and `set_setting` returns Err.
        rusqlite::Connection::open(&db_path)
            .unwrap()
            .execute("DROP TABLE runtime_settings", [])
            .unwrap();

        apply_brand(&ctx, Some("volvo".into())).await;

        // Persist failed on the very first write, so RAM stayed at the old
        // brand and models — the failure path preserved the invariant.
        let r = ctx.runtime.read().await;
        assert_eq!(r.search.brand.as_deref(), Some("mini"));
        assert_eq!(r.search.models, vec!["cooper".to_string()]);
    }
}
