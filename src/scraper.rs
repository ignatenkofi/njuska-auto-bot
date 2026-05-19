//! HTTP fetch + HTML parsing for polovniautomobili.com search-result pages.
//!
//! Two clean halves with a single seam:
//!
//! * [`fetch_search`] does I/O — async, may fail with a typed [`ScraperError`].
//! * [`parse_listings`] is a **pure** `&str -> Vec<Listing>` so it's trivially
//!   testable against saved fixtures in `tests/fixtures/`.

use std::sync::LazyLock;

use scraper::{ElementRef, Html, Selector};
use tokio::process::Command;
use tracing::debug;
use url::Url;

use crate::config::ProxyConfig;
use crate::models::{Listing, SearchFilter};

/// Base URL used to resolve relative listing links into absolute URLs.
pub const SITE_BASE_URL: &str = "https://www.polovniautomobili.com";

/// User-Agent we send via curl. Any modern browser UA works.
/// If polovniautomobili tightens its bot detection, bump to a current Safari/Chrome.
const USER_AGENT_STRING: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";

/// Per-request timeout, in seconds. Passed to `curl --max-time`.
const FETCH_TIMEOUT_SECS: u32 = 30;

/// Errors `scraper::fetch_search` can produce.
///
/// Why `thiserror` here when we use `anyhow` in `main`? Because callers of
/// `fetch_search` (the poll loop, retry logic, the "zero-results streak"
/// detector) will want to **match on the kind of failure** — `Status(429)`
/// deserves a longer backoff than a transient `Curl` socket reset.
/// A `thiserror`-derived enum gives us pattern-matchable variants while still
/// implementing `std::error::Error`, so `?` into an `anyhow::Result` keeps working.
#[derive(Debug, thiserror::Error)]
pub enum ScraperError {
    /// Spawning the `curl` process failed: binary not in PATH, permission
    /// denied, kernel OOM, etc. `#[from]` auto-derives the conversion so a
    /// bare `?` after `Command::output().await` works.
    #[error("failed to invoke curl: {0}")]
    Spawn(#[from] std::io::Error),

    /// `curl` ran but exited non-zero for a *non-HTTP* reason — TLS handshake
    /// (exit 35), DNS resolution (exit 6), timeout (exit 28), etc.
    /// We keep stderr for diagnostics; the poll loop can decide whether
    /// "DNS broken" is worth alerting on vs. "transient timeout".
    #[error("curl exited {exit}: {stderr}")]
    Curl { exit: i32, stderr: String },

    /// curl ran successfully but the status code it reported wasn't a 3-digit
    /// number. Shouldn't happen — typed errors > silent unwraps.
    #[error("malformed HTTP status from curl: {0:?}")]
    MalformedStatus(String),

    /// Non-success HTTP status. Variant kept for retry-policy decisions
    /// (429 -> long backoff, 5xx -> short, other 4xx -> give up this poll).
    #[error("non-success HTTP status: {0}")]
    Status(u16),

    /// 2xx but empty body.
    #[error("empty response body")]
    EmptyBody,

    /// Response body wasn't valid UTF-8. polovniautomobili.com declares
    /// `charset=UTF-8` in its HTML, so this would mean something went very
    /// wrong upstream.
    #[error("response body is not valid UTF-8")]
    NotUtf8,
}

/// Fetches the search-results page for the given filter and returns the raw HTML.
///
/// **Implementation: shells out to system `curl`.** Why not `reqwest`?
///
/// polovniautomobili.com sits behind Cloudflare Managed Challenge. Empirically
/// confirmed: any reqwest+hyper request — over HTTP/2 *or* forced HTTP/1.1,
/// with rustls *or* native-tls, with title-case or lowercase headers — receives
/// a 403 with `cf-mitigated: challenge`. Bare `curl --http1.1` from the same
/// machine and IP gets 200 on **macOS**, but **also 403 on Linux** (even with
/// curl-impersonate; the differentiator is somewhere in the TCP/TLS stack
/// below curl's control). Workaround: see `proxy` parameter below.
///
/// Trade-offs of the curl approach:
/// - **+** Works reliably with zero ongoing maintenance on macOS.
/// - **+** Defers TLS to a battle-tested implementation we don't own.
/// - **-** Runtime dep: `curl` must be in `PATH` (it is on macOS/Linux/Win10+).
/// - **-** Linux + CF often = 403; needs the `proxy` arg.
///
/// `proxy = Some(…)` routes the request through a Cloudflare Worker that we
/// host (`cf-proxy/` in the repo). The Worker forwards the request to polovni
/// from CF's own infrastructure, which CF doesn't challenge. Required for
/// Linux deployments behind CF; harmless on macOS where direct works too.
///
/// stderr/stdout split trick: `curl -w "%{stderr}%{http_code}"` writes the
/// HTTP status code to **stderr** (no newline), leaving stdout as a clean
/// stream of the response body. Captured separately by `output().await`.
pub async fn fetch_search(
    filter: &SearchFilter,
    proxy: Option<&ProxyConfig>,
) -> Result<String, ScraperError> {
    let original_url = filter.to_url();
    // When a proxy is configured, swap host+scheme with the Worker's URL but
    // keep path + query intact — that's what the Worker forwards verbatim.
    let request_url = match proxy {
        Some(p) => {
            let mut u = p.url.clone();
            u.set_path(original_url.path());
            u.set_query(original_url.query());
            u
        }
        None => original_url,
    };

    debug!(
        url = %request_url,
        via_proxy = proxy.is_some(),
        "fetching search page via curl"
    );

    let mut cmd = Command::new("curl");
    cmd.arg("--http1.1")
        .arg("-s") // silent: no progress bar
        .arg("-A")
        .arg(USER_AGENT_STRING)
        .arg("--max-time")
        .arg(FETCH_TIMEOUT_SECS.to_string())
        .arg("-w")
        .arg("%{stderr}%{http_code}"); // status code -> stderr, body -> stdout

    // Authenticate to our Worker. Header value is treated as opaque on the
    // CLI; no shell-escaping concerns since `secret` from env is alphanumeric.
    if let Some(p) = proxy {
        cmd.arg("-H").arg(format!("x-proxy-secret: {}", p.secret));
    }

    cmd.arg(request_url.as_str());

    let output = cmd.output().await?;

    // curl itself failed (network/TLS error, not HTTP error).
    if !output.status.success() {
        let exit = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(ScraperError::Curl { exit, stderr });
    }

    // Status code on stderr (e.g. "200" or "403"), no newline.
    let status_str = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let status: u16 = status_str
        .parse()
        .map_err(|_| ScraperError::MalformedStatus(status_str))?;

    if !(200..300).contains(&status) {
        return Err(ScraperError::Status(status));
    }

    // `String::from_utf8` is the strict variant (returns error on invalid bytes).
    // Use it rather than `from_utf8_lossy` so we *notice* if the site ever
    // starts serving a non-UTF-8 encoding — better to fail fast than to silently
    // garble characters our parser later trips on.
    let body = String::from_utf8(output.stdout).map_err(|_| ScraperError::NotUtf8)?;
    if body.is_empty() {
        return Err(ScraperError::EmptyBody);
    }
    Ok(body)
}

// --- Selectors ---
//
// We parse each CSS selector exactly once and reuse it across calls.
// `LazyLock` is the post-`lazy_static!` idiom (stable since Rust 1.80): the closure
// runs on first access, then the cached value is reused. Selector parsing is cheap
// but not free, and clippy flags re-parsing per call.
//
// `.expect("...")` here is fine because the strings are programmer-supplied
// constants — a panic means **we** wrote a bad selector and the test suite
// catches it immediately, not at runtime in prod.

static SEL_LISTING: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("article.classified").expect("listing selector"));
static SEL_TITLE_LINK: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("h2 a.ga-title").expect("title selector"));
static SEL_CITY: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".city").expect("city selector"));
static SEL_INFO_TOP: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".info .setInfo .top").expect("info-top selector"));

/// Parse all listings out of a search-results page.
///
/// Returns `Vec<Listing>` rather than `Result<…>` on purpose: a malformed individual
/// listing is **dropped silently with a `debug!`**, not propagated, because (a) one
/// broken card shouldn't lose the other 13, and (b) the upstream "got zero results
/// N times in a row" detector is what catches the "site changed, all selectors broken"
/// case. Two layers of resilience, each at the right level.
pub fn parse_listings(html: &str) -> Vec<Listing> {
    let doc = Html::parse_document(html);
    doc.select(&SEL_LISTING).filter_map(parse_one).collect()
}

fn parse_one(article: ElementRef<'_>) -> Option<Listing> {
    // `?` on `Option` short-circuits to `None`. If any required field is missing
    // we drop the whole listing — but we'll never panic.
    let id: u64 = article.value().attr("data-classifiedid")?.parse().ok()?;

    let title_a = article.select(&SEL_TITLE_LINK).next()?;
    let title = collapse_whitespace(&title_a.text().collect::<String>());
    if title.is_empty() {
        debug!(id, "skipping listing with empty title");
        return None;
    }

    let href = title_a.value().attr("href")?;
    let url = absolutize_and_clean(href)?;

    // `data-price` may be missing on `without_price=1` listings. That's fine.
    let price_text = article
        .value()
        .attr("data-price")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    let city = article
        .select(&SEL_CITY)
        .next()
        .map(|el| collapse_whitespace(&el.text().collect::<String>()))
        .filter(|s| !s.is_empty());

    // `.info .setInfo .top` collects ALL `.top` divs across all `.setInfo` groups,
    // in document order: [0]=year+body, [1]=mileage, [2]=gearbox.
    // We pull what we need and ignore the rest.
    let info_tops: Vec<ElementRef<'_>> = article.select(&SEL_INFO_TOP).collect();
    let year = info_tops
        .first()
        .and_then(|el| parse_year(&el.text().collect::<String>()));
    let mileage_km = info_tops
        .get(1)
        .and_then(|el| parse_mileage_km(&el.text().collect::<String>()));

    Some(Listing {
        id,
        title,
        url,
        price_text,
        city,
        year,
        mileage_km,
    })
}

/// Resolve a relative href against `SITE_BASE_URL` and drop the `attp=…` tracking
/// param the site appends to its own ad links. We pass the *cleaned* URL on to
/// Telegram so the user clicks through without our internal attribution tag.
fn absolutize_and_clean(href: &str) -> Option<String> {
    let base = Url::parse(SITE_BASE_URL).ok()?;
    let mut url = base.join(href).ok()?;

    // Collect the pairs we want to keep, then write them back. The `url` crate's
    // query API is awkward; this is the simplest way to drop a single param.
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != "attp")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        let mut q = url.query_pairs_mut();
        q.clear();
        for (k, v) in &kept {
            q.append_pair(k, v);
        }
        // Drop the borrow before we return.
        drop(q);
    }

    Some(url.into())
}

/// Extracts the leading 4-digit year from strings like `"2013. Kabriolet/Roadster"`.
fn parse_year(s: &str) -> Option<u16> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let y: u16 = digits.parse().ok()?;
    // Sanity: cars start ~1900; reject obvious garbage.
    (1900..=2100).contains(&y).then_some(y)
}

/// Parses Serbian-formatted mileage like `"144.857 km"` into kilometres as `u32`.
///
/// Note: in `sr-RS` locale the dot is the *thousands* separator (not a decimal).
/// Stripping all non-digits is the simplest correct thing.
fn parse_mileage_km(s: &str) -> Option<u32> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Collapses runs of whitespace (incl. newlines/tabs) into single spaces and trims.
/// The site's HTML is full of indentation-noise inside text nodes.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/search_mini_cooper_cabrio.html");

    #[test]
    fn parses_expected_number_of_listings() {
        let listings = parse_listings(FIXTURE);
        // Verified manually: the fixture has 14 visible `<article class="classified …">`.
        assert_eq!(
            listings.len(),
            14,
            "expected 14 listings, got {}",
            listings.len()
        );
    }

    #[test]
    fn ids_are_unique_and_numeric() {
        let listings = parse_listings(FIXTURE);
        let mut ids: Vec<u64> = listings.iter().map(|l| l.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 14);
    }

    #[test]
    fn one_known_listing_parses_correctly() {
        let listings = parse_listings(FIXTURE);
        let l = listings
            .iter()
            .find(|l| l.id == 27_312_553)
            .expect("known listing 27312553 missing");

        assert_eq!(l.title, "MINI Cooper 1.6d CaBRiO");
        assert_eq!(l.price_text.as_deref(), Some("8.999 €"));
        assert_eq!(l.city.as_deref(), Some("Kovin"));
        assert_eq!(l.year, Some(2013));
        assert_eq!(l.mileage_km, Some(144_857));

        // URL is absolute, points to the canonical detail page, and the `attp`
        // tracking param has been stripped.
        assert!(
            l.url
                .starts_with("https://www.polovniautomobili.com/auto-oglasi/27312553/"),
            "url should be absolute: {}",
            l.url
        );
        assert!(
            !l.url.contains("attp="),
            "attp tracking param should be stripped: {}",
            l.url
        );
    }

    #[test]
    fn every_listing_has_required_core_fields() {
        // Even if year/mileage/city/price fail to parse on some card, the *core*
        // fields (id, title, url) must always be present — those are what we dedup
        // and link on. A listing missing any of those is dropped by `parse_one`.
        for l in parse_listings(FIXTURE) {
            assert!(l.id > 0, "id must be set");
            assert!(
                !l.title.is_empty(),
                "title must be non-empty for id {}",
                l.id
            );
            assert!(
                l.url.starts_with("https://"),
                "url must be absolute for id {}",
                l.id
            );
        }
    }

    #[test]
    fn year_parser_handles_typical_input() {
        assert_eq!(parse_year("2013. Kabriolet/Roadster"), Some(2013));
        assert_eq!(parse_year("Kabriolet"), None);
        assert_eq!(parse_year(""), None);
        assert_eq!(parse_year("9999"), None); // sanity-band rejects it
    }

    #[test]
    fn mileage_parser_handles_serbian_thousand_separator() {
        assert_eq!(parse_mileage_km("144.857 km"), Some(144_857));
        assert_eq!(parse_mileage_km("1.234.567 km"), Some(1_234_567));
        assert_eq!(parse_mileage_km("0 km"), Some(0));
        assert_eq!(parse_mileage_km("nepoznato"), None);
    }
}
