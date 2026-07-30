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
/// teloxide already classifies failures into a rich enum (`RequestError`);
/// we regroup it by **what the caller should do** (#15):
///
/// * `RateLimited` — sleep the server-suggested time, then retry.
/// * `Retryable` — transport-level trouble (connection reset, timeout, a
///   garbled 502-HTML response). One quick retry is worth it.
/// * `Permanent` — Telegram understood us and said no (400 bad request,
///   403 bot blocked, …). Retrying the same payload can't succeed.
#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    /// Telegram rejected the request. Don't retry — fix the payload/config.
    #[error("Telegram request failed (permanent): {0}")]
    Permanent(teloxide::RequestError),

    /// Transport-level failure (network, I/O, unparseable response — often a
    /// 5xx from Telegram's proxies wearing an HTML body). Retry once.
    #[error("Telegram request failed (retryable): {0}")]
    Retryable(teloxide::RequestError),

    /// Carved off separately because the caller wants the server-supplied
    /// wait time (sleep N seconds then retry).
    #[error("Telegram rate limit; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
}

/// Maps teloxide's transport-oriented error enum onto our action-oriented
/// one. Kept as a standalone fn so the policy is unit-testable.
fn classify(e: teloxide::RequestError) -> TelegramError {
    use teloxide::RequestError as RE;
    match e {
        // `Seconds` is a thin tuple-struct around `u32`; convert to u64 to
        // match the sleep API in `bot::send_batch`.
        RE::RetryAfter(after) => TelegramError::RateLimited {
            retry_after_secs: u64::from(after.seconds()),
        },
        // Transport-level failures. InvalidJson counts as retryable because
        // a 502/504 from Telegram's frontend arrives as an HTML body that
        // fails JSON parsing — transient by nature.
        RE::Network(_) | RE::Io(_) | RE::InvalidJson { .. } => TelegramError::Retryable(e),
        // Api errors (incl. 400s), MigrateToChatId, and anything teloxide
        // adds later: treat as permanent. Erring towards "don't retry" can
        // never cause a retry storm.
        _ => TelegramError::Permanent(e),
    }
}

/// Seam for the poll cycle's outbound messages (#22): production code sends
/// through [`TelegramClient`]; integration tests plug in a collector and
/// assert on what *would* have been sent, without any network.
///
/// RPITIT (`-> impl Future … + Send`) rather than `async fn` in the trait:
/// the explicit `Send` bound keeps the poll-loop future spawnable via
/// `tokio::spawn` without the "async fn in public trait" auto-trait caveat.
pub trait Notifier {
    fn send_html(
        &self,
        html: &str,
    ) -> impl std::future::Future<Output = Result<(), TelegramError>> + Send;
}

impl Notifier for TelegramClient {
    fn send_html(
        &self,
        html: &str,
    ) -> impl std::future::Future<Output = Result<(), TelegramError>> + Send {
        self.send_message(html)
    }
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
            Err(e) => Err(classify(e)),
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

/// [`format_listing_html`] plus a trailing `[filter-name]` tag (#10, stage 2).
///
/// A footer rather than a prefix: the listing card stays scannable, the tag
/// answers "which of my filters matched this?". Bracketed name instead of a
/// localized "filter:" label keeps the formatter out of i18n. The name is
/// user input (the /filter wizard, stage 3) — escaped like everything else.
pub fn format_listing_html_tagged(l: &Listing, filter_name: &str) -> String {
    format!(
        "{}\n<i>[{}]</i>",
        format_listing_html(l),
        escape_html(filter_name)
    )
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
///
/// `pub(crate)` for the same reason as [`escape_html`] — `/dump` and `/status`
/// build `href` attributes too and must not roll their own copy (#24).
pub(crate) fn escape_html_attr(s: &str) -> String {
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

    #[test]
    fn escape_html_double_escapes_pre_escaped_input() {
        // Correct behavior: we escape *content*, so text that already looks
        // like an entity is displayed literally ("&amp;" shows as "&amp;").
        // Anything smarter would let crafted titles smuggle entities through.
        assert_eq!(escape_html("&amp;"), "&amp;amp;");
        assert_eq!(escape_html("&lt;b&gt;"), "&amp;lt;b&amp;gt;");
    }

    #[test]
    fn escape_html_passes_rtl_zero_width_and_emoji_through() {
        // Unicode trickery is not HTML-special; it must survive unchanged
        // (stripping/mangling would corrupt legitimate Serbian/emoji titles).
        let s = "🚗 BMW \u{202E}ok\u{200B}done";
        assert_eq!(escape_html(s), s);
    }

    #[test]
    fn format_listing_html_escapes_quotes_and_gt_in_urls() {
        let mut l = sample_listing();
        l.url = "https://example.com/a?q=\"x\"&r=<y>".into();
        let html = format_listing_html(&l);
        assert!(html.contains("&quot;x&quot;"), "{html}");
        assert!(html.contains("&lt;y&gt;"), "{html}");
        assert!(html.contains("&amp;r="), "{html}");
        // The raw quote must not survive inside the href attribute value.
        let href_start = html.find("href=\"").unwrap() + 6;
        let href_end = html[href_start..].find('"').unwrap() + href_start;
        assert!(!html[href_start..href_end].contains('<'), "{html}");
    }

    #[test]
    fn format_listing_html_handles_bare_ampersand_and_long_titles() {
        let mut l = sample_listing();
        l.title = format!("Trap & Sons {}", "x".repeat(600));
        let html = format_listing_html(&l);
        assert!(html.contains("Trap &amp; Sons"), "{html}");
        // Notification cards don't truncate — one card is nowhere near the
        // 4096 limit even with a 600-char title. Pin that assumption.
        assert!(html.chars().count() < 1200, "{}", html.chars().count());
    }

    #[test]
    fn classify_maps_io_and_json_errors_to_retryable() {
        use std::sync::Arc;
        use teloxide::RequestError as RE;

        let io = RE::Io(Arc::new(std::io::Error::other("conn reset")));
        assert!(matches!(classify(io), TelegramError::Retryable(_)));

        // A 502 from Telegram's frontend arrives as HTML → JSON parse error.
        let json_err = serde_json::from_str::<serde_json::Value>("<html>").unwrap_err();
        let invalid = RE::InvalidJson {
            source: Arc::new(json_err),
            raw: "<html>502</html>".into(),
        };
        assert!(matches!(classify(invalid), TelegramError::Retryable(_)));
    }

    #[test]
    fn classify_maps_api_errors_to_permanent() {
        use teloxide::ApiError;
        use teloxide::RequestError as RE;

        // A 400-style error: message text couldn't be parsed as HTML.
        let bad_request = RE::Api(ApiError::CantParseEntities("bad html".into()));
        assert!(matches!(classify(bad_request), TelegramError::Permanent(_)));
    }

    #[test]
    fn classify_carves_off_rate_limit_with_wait_time() {
        use teloxide::RequestError as RE;
        use teloxide::types::Seconds;

        let limited = RE::RetryAfter(Seconds::from_seconds(17));
        match classify(limited) {
            TelegramError::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, 17),
            other => panic!("expected RateLimited, got {other:?}"),
        }
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
    fn format_listing_html_tagged_appends_escaped_filter_name() {
        let html = format_listing_html_tagged(&sample_listing(), "bmw <3 & co");
        // The card itself is unchanged, the tag is a separate last line.
        assert!(html.contains(">MINI Cooper 1.6d CaBRiO</a></b>"), "{html}");
        assert!(
            html.ends_with("<i>[bmw &lt;3 &amp; co]</i>"),
            "tag must be the escaped last line: {html}"
        );
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
