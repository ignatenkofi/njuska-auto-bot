//! Telegram client — wraps `teloxide::Bot` for our send-only outbound path.
//!
//! In v1 this module hand-rolled HTTP via `reqwest`. In v2 we adopted
//! `teloxide` for inbound commands; since `teloxide::Bot` is the canonical
//! handle on the Bot API, we use it here too — one Bot instance shared
//! between the send path (this module) and the command dispatcher
//! (`commands.rs`). teloxide's `Bot::clone()` is cheap (it's `Arc` inside).

use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode};
use tracing::debug;

use crate::models::Listing;

/// Errors that can leak out of [`TelegramClient::send_message`].
///
/// teloxide already classifies failures into a rich enum (`RequestError`)
/// covering network, JSON, API errors, etc. We carve **one** case off:
/// `RateLimited`. The poll loop wants to match on 429-with-retry-after
/// without inspecting variants under `Request(_)` — a top-level variant
/// makes that one-line.
#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    #[error("Telegram request failed: {0}")]
    Request(teloxide::RequestError),

    /// Carved off from `Request` because the caller almost always wants to
    /// pattern-match this specifically (sleep N seconds then retry).
    #[error("Telegram rate limit; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
}

/// Send-only Telegram facade.
///
/// We deliberately do **not** derive `Debug` — printing a `TelegramClient`
/// would dump the bot token through whatever logger picked it up. `Clone` is
/// safe to derive though: `Bot` is internally `Arc`-shared (clone bumps a
/// refcount), `ChatId` is `Copy`. So `TelegramClient.clone()` is essentially
/// free — passing it to multiple spawned tasks is fine.
///
/// `bot()` exposes the underlying `teloxide::Bot` so the command dispatcher
/// (in `commands.rs`) can use the same handle.
#[derive(Clone)]
pub struct TelegramClient {
    bot: Bot,
    chat_id: ChatId,
}

impl TelegramClient {
    /// Infallible — `Bot::new` doesn't perform any I/O (no connection probe).
    /// Errors only surface on the first request.
    pub fn new(bot_token: String, chat_id: i64) -> Self {
        Self {
            bot: Bot::new(bot_token),
            chat_id: ChatId(chat_id),
        }
    }

    /// Underlying teloxide handle. Shared with the command dispatcher.
    /// Returns a clone (cheap — Bot is `Arc` internally) so callers don't
    /// keep a borrow on `self`.
    pub fn bot(&self) -> Bot {
        self.bot.clone()
    }

    /// Sends a single message with HTML parse mode.
    ///
    /// **The caller is responsible for escaping untrusted text** — see
    /// [`escape_html`] and [`format_listing_html`]. Telegram's HTML parser is
    /// strict-ish: unescaped `<` or `>` in user-supplied strings will produce
    /// a 400 error.
    pub async fn send_message(&self, html: &str) -> Result<(), TelegramError> {
        debug!(
            chat_id = self.chat_id.0,
            bytes = html.len(),
            "sending telegram message"
        );

        match self
            .bot
            .send_message(self.chat_id, html)
            .parse_mode(ParseMode::Html)
            .await
        {
            Ok(_) => Ok(()),
            Err(teloxide::RequestError::RetryAfter(after)) => Err(TelegramError::RateLimited {
                // `Seconds` is a thin tuple-struct around `u32`. Convert to u64
                // to match our `RateLimited.retry_after_secs` field and the
                // sleep API in `bot::send_batch`.
                retry_after_secs: u64::from(after.seconds()),
            }),
            Err(e) => Err(TelegramError::Request(e)),
        }
    }
}

/// Renders a `Listing` for the Telegram message body.
///
/// Layout:
///
/// ```text
/// <b><a href="…">MINI Cooper 1.6d CaBRiO</a></b>
/// 8.999 € · 2013 · 144857 km · Kovin
/// ```
///
/// Missing optional fields are simply skipped from the second line. If every
/// optional field is missing, the second line is dropped entirely.
pub fn format_listing_html(l: &Listing) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(2);
    lines.push(format!(
        "<b><a href=\"{href}\">{title}</a></b>",
        href = escape_html_attr(&l.url),
        title = escape_html(&l.title),
    ));

    let mut meta: Vec<String> = Vec::with_capacity(4);
    if let Some(p) = &l.price_text {
        meta.push(escape_html(p));
    }
    if let Some(y) = l.year {
        meta.push(y.to_string());
    }
    if let Some(km) = l.mileage_km {
        meta.push(format!("{km} km"));
    }
    if let Some(c) = &l.city {
        meta.push(escape_html(c));
    }
    if !meta.is_empty() {
        // `·` (middle dot, U+00B7) is fine raw — it's a literal, not a special HTML char.
        lines.push(meta.join(" · "));
    }
    lines.join("\n")
}

/// Escapes the three characters Telegram's HTML parser treats specially in
/// element *content*: `&`, `<`, `>`.
///
/// Order matters: `&` first, otherwise the `&` we just introduced via `&lt;`
/// would get rewritten on the next pass.
///
/// `pub(crate)` so other modules embedding untrusted text into HTML messages
/// (alerts in `bot.rs`, command replies) reuse this instead of rolling their own.
pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Same as [`escape_html`] plus `"` -> `&quot;`. Use inside `href="..."`
/// attributes; safe (if slightly redundant) anywhere else.
fn escape_html_attr(s: &str) -> String {
    escape_html(s).replace('"', "&quot;")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[test]
    fn escape_html_handles_all_three_specials() {
        assert_eq!(escape_html("plain"), "plain");
        assert_eq!(escape_html("a < b"), "a &lt; b");
        assert_eq!(escape_html("a > b"), "a &gt; b");
        assert_eq!(escape_html("R&D"), "R&amp;D");
        // Order check: `<` inside an already-escaped `&amp;` mustn't be touched.
        assert_eq!(escape_html("<&>"), "&lt;&amp;&gt;");
    }

    #[test]
    fn escape_html_attr_also_handles_quote() {
        assert_eq!(escape_html_attr("a\"b"), "a&quot;b");
    }

    fn sample_listing() -> Listing {
        Listing {
            id: 27_312_553,
            title: "MINI Cooper 1.6d CaBRiO".into(),
            url: "https://www.polovniautomobili.com/auto-oglasi/27312553/mini-cooper-16d-cabrio"
                .into(),
            price_text: Some("8.999 €".into()),
            city: Some("Kovin".into()),
            year: Some(2013),
            mileage_km: Some(144_857),
        }
    }

    #[test]
    fn format_listing_html_full() {
        let html = format_listing_html(&sample_listing());
        // First line: link with bold title.
        assert!(html.contains("<b><a href=\""), "{html}");
        assert!(html.contains(">MINI Cooper 1.6d CaBRiO</a></b>"), "{html}");
        // Second line: meta separated by middle-dot.
        assert!(html.contains("8.999 €"), "{html}");
        assert!(html.contains("2013"), "{html}");
        assert!(html.contains("144857 km"), "{html}");
        assert!(html.contains("Kovin"), "{html}");
        assert!(html.contains(" · "), "{html}");
    }

    #[test]
    fn format_listing_html_skips_missing_meta() {
        let mut l = sample_listing();
        l.price_text = None;
        l.year = None;
        l.mileage_km = None;
        l.city = None;
        let html = format_listing_html(&l);
        // Should have just the title line, no second meta line.
        assert!(!html.contains('\n'), "expected no second line, got: {html}");
    }

    #[test]
    fn format_listing_html_escapes_hostile_title() {
        let mut l = sample_listing();
        l.title = "BMW <script> & Co.".into();
        let html = format_listing_html(&l);
        // The dangerous chars must be replaced by entities.
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains("&amp; Co"), "{html}");
        // And the raw `<script>` must NOT be present.
        assert!(!html.contains("<script>"), "{html}");
    }
}
