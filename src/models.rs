//! Shared domain types: parsed listings and search-filter parameters.

use serde::{Deserialize, Serialize};
use url::Url;

/// Base URL of the search page. Centralised here so both URL building and URL
/// resolution in the parser use the same source of truth.
pub const SEARCH_URL: &str = "https://www.polovniautomobili.com/auto-oglasi/pretraga";

/// The site's sort-`<select>` value for "datumu objave silazno" — publish
/// date, newest first. Pinned on every search URL (#25): the whole bot rests
/// on "new listings surface on page 1", and the site's default ordering
/// (relevance-flavoured) doesn't guarantee that.
const SORT_NEWEST_FIRST: &str = "renewDate_desc";

/// A single car listing as parsed from a polovniautomobili.com search page.
///
/// Field rationale:
///
/// * `id` is `u64`. The site uses numeric IDs (e.g. `27312553`), monotonically growing.
///   `u64` is overkill today but free, and keeps us safe from any future overflow worries.
/// * `title`, `url`, `city` are `String` (owned) rather than `&str` because a `Listing`
///   outlives the HTML buffer it was parsed from — we hand them to storage and to
///   Telegram on different code paths, so borrowing is a non-starter.
/// * `price_text` is `Option<String>` and intentionally **raw** (e.g. `"8.999 €"`).
///   The site supports `without_price=1` searches, so price can be absent; and
///   structured price parsing (currency, taxes, leasing) is a rabbit hole we don't
///   need for v1 — humans read the message and click through anyway.
/// * `year` and `mileage_km` are `Option<…>` because parsing can fail on weird
///   markup; we'd rather drop just the field than drop the whole listing.
///
/// `Serialize` / `Deserialize` are derived now so we can later (a) persist snapshots
/// to SQLite as JSON if we want, and (b) write listings into the Telegram payload
/// helpers without a separate DTO. Zero cost when unused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub price_text: Option<String>,
    pub city: Option<String>,
    pub year: Option<u16>,
    pub mileage_km: Option<u32>,
}

/// "Show old / new / all" toggle, mapped to the site's `showOldNew=…` query param.
///
/// `#[derive(Default)]` together with `#[default]` on a variant gives us
/// `ShowOldNew::default() == All` for free — handy when building filters
/// piecemeal from `.env` where the user might omit the option entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShowOldNew {
    #[default]
    All,
    Old,
    New,
}

impl ShowOldNew {
    /// Returns the literal string the site expects in the `showOldNew` query param.
    /// `&'static str` because the value lives in the binary's read-only data —
    /// no allocation, no lifetime worries.
    pub fn as_param(self) -> &'static str {
        match self {
            ShowOldNew::All => "all",
            ShowOldNew::Old => "old",
            ShowOldNew::New => "new",
        }
    }
}

/// All search parameters we know how to forward to polovniautomobili.com.
///
/// Why so many `Option`s and `Vec`s rather than one big `HashMap<String, String>`?
/// Because the *type* documents what we support, gives us compile-time spelling
/// checks, and lets us validate ranges (`u16` can't hold "9999999"). A map would
/// push validation everywhere.
///
/// `#[derive(Default)]` gives `SearchFilter::default()` with `None`s and empty
/// `Vec`s — the equivalent of "no filters at all" — which is convenient for
/// tests and for `..` struct-update syntax in builder-ish code.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub brand: Option<String>,
    /// PHP-style array param: each entry becomes `model[]=<value>` in the URL.
    pub models: Vec<String>,
    /// Body-type codes (`chassis[]=2634` is Kabriolet/Roadster on this site).
    pub chassis: Vec<u32>,
    /// Gearbox codes (`gearbox[]=10795` is Automatski/poluautomatski).
    /// Same PHP-array param style as `chassis`.
    pub gearbox: Vec<u32>,
    pub price_from: Option<u32>,
    pub price_to: Option<u32>,
    pub year_from: Option<u16>,
    pub year_to: Option<u16>,
    /// Maps to `without_price=1` (include listings with no price).
    pub without_price: bool,
    pub show_old_new: ShowOldNew,
}

impl SearchFilter {
    /// Builds the search URL for the first page. Kept as the short name
    /// because most callers (status link, /diag, startup probe) only ever
    /// care about page 1.
    pub fn to_url(&self) -> Url {
        self.to_url_for_page(1)
    }

    /// Builds the full search URL with all configured query parameters, the
    /// pinned newest-first sort, and — beyond the first page — the site's
    /// `page` pagination param.
    ///
    /// Parameters that are `None`/empty are simply omitted — the site treats
    /// absent and empty filters as "no constraint", just like we want.
    // Justified expect: SEARCH_URL is a compile-time constant; a parse
    // failure is a typo the test suite catches immediately, not a runtime
    // condition.
    #[allow(clippy::expect_used)]
    pub fn to_url_for_page(&self, page: u32) -> Url {
        let mut url = Url::parse(SEARCH_URL).expect("static search URL parses");

        // `query_pairs_mut()` returns a writer that *borrows* the URL mutably.
        // We need to drop it before reading the URL again (or returning it),
        // so we contain it in an inner scope.
        {
            let mut q = url.query_pairs_mut();

            if let Some(brand) = self.brand.as_deref() {
                q.append_pair("brand", brand);
            }
            for m in &self.models {
                q.append_pair("model[]", m);
            }
            for c in &self.chassis {
                q.append_pair("chassis[]", &c.to_string());
            }
            for g in &self.gearbox {
                q.append_pair("gearbox[]", &g.to_string());
            }
            if let Some(p) = self.price_from {
                q.append_pair("price_from", &p.to_string());
            }
            if let Some(p) = self.price_to {
                q.append_pair("price_to", &p.to_string());
            }
            if let Some(y) = self.year_from {
                q.append_pair("year_from", &y.to_string());
            }
            if let Some(y) = self.year_to {
                q.append_pair("year_to", &y.to_string());
            }
            if self.without_price {
                q.append_pair("without_price", "1");
            }
            q.append_pair("showOldNew", self.show_old_new.as_param());
            // Explicit sort — never rely on the site's default ordering.
            q.append_pair("sort", SORT_NEWEST_FIRST);
            // The site numbers pages from 1 and treats an absent `page` as
            // page 1, so we omit it there — the page-1 URL stays identical
            // to the hand-crafted reference the filters were validated
            // against.
            if page > 1 {
                q.append_pair("page", &page.to_string());
            }
        }

        url
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    /// The hand-crafted URL from the user's browser session is the gold reference.
    /// We don't compare byte-for-byte (param order isn't guaranteed), but we
    /// check the set of pairs matches.
    #[test]
    fn to_url_produces_mini_cooper_cabrio_query() {
        let filter = SearchFilter {
            brand: Some("mini".into()),
            models: vec!["cooper".into()],
            chassis: vec![2634],
            without_price: true,
            ..Default::default()
        };

        let url = filter.to_url();
        assert_eq!(url.path(), "/auto-oglasi/pretraga");

        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        assert!(pairs.contains(&("brand".into(), "mini".into())));
        assert!(pairs.contains(&("model[]".into(), "cooper".into())));
        assert!(pairs.contains(&("chassis[]".into(), "2634".into())));
        assert!(pairs.contains(&("without_price".into(), "1".into())));
        assert!(pairs.contains(&("showOldNew".into(), "all".into())));
        // Newest-first sort is pinned on every URL (#25).
        assert!(pairs.contains(&("sort".into(), "renewDate_desc".into())));

        // Empty range filters must NOT appear at all — passing `price_to=`
        // would be harmless on this site but cleaner to omit.
        assert!(pairs.iter().all(|(k, _)| k != "price_from"));
        assert!(pairs.iter().all(|(k, _)| k != "price_to"));
        assert!(pairs.iter().all(|(k, _)| k != "year_from"));
        assert!(pairs.iter().all(|(k, _)| k != "year_to"));
        // Page 1 carries no page param — same URL shape as the reference.
        assert!(pairs.iter().all(|(k, _)| k != "page"));
    }

    #[test]
    fn to_url_renders_gearbox_codes_as_array_params() {
        let filter = SearchFilter {
            gearbox: vec![3212, 10795],
            ..Default::default()
        };
        let pairs: Vec<(String, String)> = filter
            .to_url()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        // Same PHP-array style as chassis: one `gearbox[]` pair per code,
        // in selection order.
        assert!(pairs.contains(&("gearbox[]".into(), "3212".into())));
        assert!(pairs.contains(&("gearbox[]".into(), "10795".into())));
        assert_eq!(pairs.iter().filter(|(k, _)| k == "gearbox[]").count(), 2);

        // And an empty selection emits nothing at all.
        let no_gearbox = SearchFilter::default().to_url();
        assert!(no_gearbox.query_pairs().all(|(k, _)| k != "gearbox[]"));
    }

    #[test]
    fn to_url_for_page_appends_page_only_beyond_the_first() {
        let filter = SearchFilter::default();

        let page1 = filter.to_url_for_page(1);
        assert_eq!(page1, filter.to_url(), "to_url must be the page-1 URL");
        assert!(page1.query_pairs().all(|(k, _)| k != "page"));

        let page3 = filter.to_url_for_page(3);
        let pairs: Vec<(String, String)> = page3
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(pairs.contains(&("page".into(), "3".into())));
        // Sort stays pinned on deeper pages too — otherwise "older pages are
        // strictly older listings" (the early-stop invariant) breaks.
        assert!(pairs.contains(&("sort".into(), "renewDate_desc".into())));
    }

    #[test]
    fn show_old_new_param_strings() {
        // Documents the exact strings the site expects and incidentally keeps
        // the `Old`/`New` variants alive from the compiler's dead-code POV.
        assert_eq!(ShowOldNew::All.as_param(), "all");
        assert_eq!(ShowOldNew::Old.as_param(), "old");
        assert_eq!(ShowOldNew::New.as_param(), "new");
        assert_eq!(ShowOldNew::default(), ShowOldNew::All);
    }

    #[test]
    fn empty_filter_still_produces_minimal_url() {
        let url = SearchFilter::default().to_url();
        // Only the `showOldNew=all` default and the pinned sort are present.
        let pairs: Vec<_> = url.query_pairs().collect();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "showOldNew");
        assert_eq!(pairs[0].1, "all");
        assert_eq!(pairs[1].0, "sort");
        assert_eq!(pairs[1].1, "renewDate_desc");
    }
}
