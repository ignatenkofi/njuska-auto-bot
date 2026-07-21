//! Dynamic brand/model catalog fetched from polovniautomobili.com (#11).
//!
//! The hardcoded catalog in [`crate::commands::catalog`] carries model slugs
//! that are, per its own comment, "educated guesses" — a wrong slug silently
//! yields zero results, which is a nasty failure mode to debug. This module
//! fetches the real brand/model `<select>` dropdowns from the site, caches them
//! in SQLite with a weekly TTL, and **always** falls back to the hardcoded
//! catalog so the wizard can never render an empty picker.
//!
//! Three layers, cleanly separated:
//!
//! * **parse** — pure `&str -> Vec<(slug, display)>`, fixture-tested (no
//!   network), mirroring [`crate::scraper::parse_listings`].
//! * **fetch** — async, rides [`crate::scraper::fetch_url`] so it shares the
//!   exact curl + CF-Worker-proxy path (and its Cloudflare workaround) with the
//!   listing scraper. Returns a typed [`ScraperError`].
//! * **resolve** — storage-backed: refresh-if-stale, then read the cache, then
//!   fall back to the hardcoded catalog on any gap.
//!
//! The `/filter` wizard's brand and model pickers call [`brands_or_fallback`] /
//! [`models_or_fallback`] to render instantly, and fire [`refresh_brands_if_stale`]
//! / [`refresh_models_if_stale`] in the background so a tap never waits on the
//! network. Both pickers paginate (site catalogs run long) — see
//! `crate::commands::keyboards`.

use std::sync::LazyLock;

use scraper::{Html, Selector};
use tracing::{info, warn};

use crate::config::ProxyConfig;
use crate::models::SearchFilter;
use crate::scraper::ScraperError;
use crate::storage::Storage;

/// How long a cached catalog stays authoritative before we re-fetch. Brands and
/// models change on the scale of new model years, so a week is generous — the
/// issue calls weekly "plenty".
pub const CATALOG_TTL_DAYS: u32 = 7;

// --- Selectors ---
//
// One `<option>` per brand/model, keyed off the select's id. Parsed once and
// cached, same rationale (and justified `expect`) as `scraper.rs`.

/// Parses a programmer-supplied CSS selector. The input is a string constant;
/// a bad one is caught by the first test run, never at runtime — the same
/// justified `expect` pattern `scraper::sel` uses.
#[allow(clippy::expect_used)]
fn sel(css: &str) -> Selector {
    Selector::parse(css).expect("static selector must parse")
}

static SEL_BRAND_OPTIONS: LazyLock<Selector> = LazyLock::new(|| sel("select#brand option"));
static SEL_MODEL_OPTIONS: LazyLock<Selector> = LazyLock::new(|| sel("select#model option"));

/// Parse the brand dropdown into `(slug, display)` pairs.
pub fn parse_brands(html: &str) -> Vec<(String, String)> {
    parse_options(html, &SEL_BRAND_OPTIONS)
}

/// Parse the (brand-scoped) model dropdown into `(slug, display)` pairs.
pub fn parse_models(html: &str) -> Vec<(String, String)> {
    parse_options(html, &SEL_MODEL_OPTIONS)
}

/// Shared `<option>` extractor. Skips the leading placeholder (`value=""`,
/// e.g. "Sve marke"/"Svi modeli") and de-dups on slug so a value the site
/// happens to repeat can't produce two identical picker buttons.
fn parse_options(html: &str, selector: &Selector) -> Vec<(String, String)> {
    let doc = Html::parse_document(html);
    let mut out: Vec<(String, String)> = Vec::new();
    for opt in doc.select(selector) {
        // A `<option>` with no `value` or an empty one is the "any" placeholder,
        // not a real choice — the site sends nothing for it.
        let Some(slug) = opt
            .value()
            .attr("value")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let display = collapse_whitespace(&opt.text().collect::<String>());
        if display.is_empty() {
            continue;
        }
        if out.iter().any(|(s, _)| s == slug) {
            continue;
        }
        out.push((slug.to_owned(), display));
    }
    out
}

/// Collapses runs of whitespace into single spaces and trims — the site's
/// option text is padded with indentation newlines. (Local copy of the same
/// helper in `scraper.rs`; too small to be worth a shared module.)
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- Fetch (shares the scraper's curl + CF-proxy path) ---

/// Fetch the brand dropdown from the plain search page.
pub async fn fetch_brands(
    proxy: Option<&ProxyConfig>,
) -> Result<Vec<(String, String)>, ScraperError> {
    let url = SearchFilter::default().to_url();
    let html = crate::scraper::fetch_url(&url, proxy).await?;
    Ok(parse_brands(&html))
}

/// Fetch the model dropdown for one brand. Requesting the search page with
/// `brand=<slug>` set is what makes the site render that brand's model options
/// server-side — so we reuse [`SearchFilter`] rather than guessing an endpoint.
pub async fn fetch_models(
    brand_slug: &str,
    proxy: Option<&ProxyConfig>,
) -> Result<Vec<(String, String)>, ScraperError> {
    let filter = SearchFilter {
        brand: Some(brand_slug.to_owned()),
        ..Default::default()
    };
    let html = crate::scraper::fetch_url(&filter.to_url(), proxy).await?;
    Ok(parse_models(&html))
}

// --- Resolve (storage-backed, hardcoded fallback) ---

/// Brand list for the wizard: refreshed from the site if the cache is stale,
/// then read from the cache, falling back to the hardcoded catalog. Never empty.
///
/// Returns owned `String`s (not `&'static str`) because the values may come
/// from the DB at runtime — there's no `'static` lifetime to borrow from.
pub async fn brands(storage: &Storage, proxy: Option<&ProxyConfig>) -> Vec<(String, String)> {
    refresh_brands_if_stale(storage, proxy).await;
    brands_or_fallback(storage)
}

/// Model list for `brand_slug`, same refresh-then-read-then-fallback flow as
/// [`brands`]. May be empty when neither the site nor the hardcoded catalog
/// knows the brand — the wizard shows its "no catalog" hint in that case.
pub async fn models(
    storage: &Storage,
    proxy: Option<&ProxyConfig>,
    brand_slug: &str,
) -> Vec<(String, String)> {
    refresh_models_if_stale(storage, proxy, brand_slug).await;
    models_or_fallback(storage, brand_slug)
}

/// Refresh the cached brands if missing or older than the TTL. Fetch/parse
/// failures and empty results are logged and swallowed — we keep whatever the
/// cache (or fallback) already offers rather than blanking it. Returns whether
/// fresh data was actually persisted.
pub async fn refresh_brands_if_stale(storage: &Storage, proxy: Option<&ProxyConfig>) -> bool {
    if storage
        .catalog_is_fresh("brand", "", CATALOG_TTL_DAYS)
        .unwrap_or(false)
    {
        return false;
    }
    match fetch_brands(proxy).await {
        Ok(brands) if !brands.is_empty() => match storage.replace_catalog_brands(&brands) {
            Ok(()) => {
                info!(count = brands.len(), "refreshed brand catalog from site");
                true
            }
            Err(e) => {
                warn!(error = %e, "persisting brand catalog failed; keeping previous");
                false
            }
        },
        Ok(_) => {
            warn!("brand catalog fetch returned zero options; keeping previous/fallback");
            false
        }
        Err(e) => {
            warn!(error = %e, "brand catalog fetch failed; keeping previous/fallback");
            false
        }
    }
}

/// Per-brand analogue of [`refresh_brands_if_stale`].
pub async fn refresh_models_if_stale(
    storage: &Storage,
    proxy: Option<&ProxyConfig>,
    brand_slug: &str,
) -> bool {
    if storage
        .catalog_is_fresh("model", brand_slug, CATALOG_TTL_DAYS)
        .unwrap_or(false)
    {
        return false;
    }
    match fetch_models(brand_slug, proxy).await {
        Ok(models) if !models.is_empty() => {
            match storage.replace_catalog_models(brand_slug, &models) {
                Ok(()) => {
                    info!(
                        brand = brand_slug,
                        count = models.len(),
                        "refreshed model catalog"
                    );
                    true
                }
                Err(e) => {
                    warn!(brand = brand_slug, error = %e, "persisting model catalog failed");
                    false
                }
            }
        }
        Ok(_) => {
            warn!(
                brand = brand_slug,
                "model catalog fetch returned zero options"
            );
            false
        }
        Err(e) => {
            warn!(brand = brand_slug, error = %e, "model catalog fetch failed");
            false
        }
    }
}

/// Cached brands, or the hardcoded fallback when the cache is empty/unreadable.
/// Pure (no network) so the wizard and tests can call it directly.
pub fn brands_or_fallback(storage: &Storage) -> Vec<(String, String)> {
    match storage.catalog_brands() {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => hardcoded_brands(),
        Err(e) => {
            warn!(error = %e, "reading brand catalog failed; using hardcoded fallback");
            hardcoded_brands()
        }
    }
}

/// Cached models for `brand_slug`, or the hardcoded fallback. Empty when both
/// are empty (brand the wizard can pick but has no model catalog for).
pub fn models_or_fallback(storage: &Storage, brand_slug: &str) -> Vec<(String, String)> {
    match storage.catalog_models(brand_slug) {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => hardcoded_models(brand_slug),
        Err(e) => {
            warn!(brand = brand_slug, error = %e, "reading model catalog failed; using fallback");
            hardcoded_models(brand_slug)
        }
    }
}

fn hardcoded_brands() -> Vec<(String, String)> {
    to_owned_pairs(crate::commands::catalog::fallback_brands())
}

fn hardcoded_models(brand_slug: &str) -> Vec<(String, String)> {
    crate::commands::catalog::fallback_models(brand_slug)
        .map(to_owned_pairs)
        .unwrap_or_default()
}

/// Widen a `&[(&str, &str)]` catalog slice into owned pairs so it lines up with
/// the DB-sourced shape the resolver returns.
fn to_owned_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/catalog_dropdowns.html");

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::new(&dir.path().join("catalog.db")).unwrap();
        (s, dir)
    }

    #[test]
    fn parses_brands_skipping_the_placeholder() {
        let brands = parse_brands(FIXTURE);
        assert_eq!(
            brands,
            vec![
                ("audi".to_owned(), "Audi".to_owned()),
                ("bmw".to_owned(), "BMW".to_owned()),
                ("mercedes-benz".to_owned(), "Mercedes Benz".to_owned()),
                ("mini".to_owned(), "MINI".to_owned()),
                ("volkswagen".to_owned(), "Volkswagen".to_owned()),
            ]
        );
        // The empty-value "Sve marke" placeholder must never become a button.
        assert!(brands.iter().all(|(slug, _)| !slug.is_empty()));
    }

    #[test]
    fn parses_models_for_the_selected_brand() {
        let models = parse_models(FIXTURE);
        let slugs: Vec<&str> = models.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(slugs, ["a3", "a4", "a6", "q5"]);
        // Multi-line option text ("\n MINI \n" style) is collapsed and trimmed.
        assert!(models.iter().all(|(_, d)| !d.contains('\n')));
    }

    #[test]
    fn parser_collapses_whitespace_in_display_names() {
        // The `MINI` option in the fixture is wrapped across lines on purpose.
        let mini = parse_brands(FIXTURE)
            .into_iter()
            .find(|(s, _)| s == "mini")
            .expect("mini brand present");
        assert_eq!(mini.1, "MINI");
    }

    #[test]
    fn parser_dedups_repeated_slugs() {
        let html = r#"
            <select id="brand">
              <option value="">any</option>
              <option value="audi">Audi</option>
              <option value="audi">Audi (dup)</option>
            </select>"#;
        assert_eq!(
            parse_brands(html),
            vec![("audi".to_owned(), "Audi".to_owned())]
        );
    }

    #[test]
    fn parser_returns_empty_when_the_select_is_missing() {
        // The "site changed / selector broke" case: no panic, just empty, which
        // the resolver turns into a fallback.
        assert!(parse_brands("<html><body>no dropdown here</body></html>").is_empty());
    }

    #[test]
    fn brands_fall_back_to_hardcoded_when_cache_empty() {
        let (s, _dir) = temp_storage();
        let brands = brands_or_fallback(&s);
        // The hardcoded catalog leads with Audi; the point is only that it's
        // non-empty so the wizard never blanks.
        assert!(!brands.is_empty());
        assert!(brands.iter().any(|(slug, _)| slug == "audi"));
    }

    #[test]
    fn cached_brands_take_precedence_over_hardcoded() {
        let (s, _dir) = temp_storage();
        // A cache that deliberately differs from the hardcoded list proves the
        // resolver reads the DB, not the constant.
        let cached = vec![("tesla".to_owned(), "Tesla".to_owned())];
        s.replace_catalog_brands(&cached).unwrap();
        assert_eq!(brands_or_fallback(&s), cached);
    }

    #[test]
    fn models_fall_back_to_hardcoded_for_known_brand() {
        let (s, _dir) = temp_storage();
        // `mini` has a hardcoded model catalog; the resolver must surface it.
        let models = models_or_fallback(&s, "mini");
        assert!(models.iter().any(|(slug, _)| slug == "cooper"));
    }

    #[test]
    fn models_are_empty_for_brand_without_any_catalog() {
        let (s, _dir) = temp_storage();
        // `volvo` is pickable but has no hardcoded models and nothing cached.
        assert!(models_or_fallback(&s, "volvo").is_empty());
    }

    #[test]
    fn cached_models_take_precedence_over_hardcoded() {
        let (s, _dir) = temp_storage();
        let cached = vec![("cooper-se".to_owned(), "Cooper SE".to_owned())];
        s.replace_catalog_models("mini", &cached).unwrap();
        assert_eq!(models_or_fallback(&s, "mini"), cached);
    }
}
