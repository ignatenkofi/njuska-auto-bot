//! Catalog data for the /filter wizard — brands, models, body types, and
//! the preset ranges the pickers offer.
//!
//! Pure data + lookup helpers, no I/O and no Telegram types. Kept separate
//! so growing the catalogs (the most common edit) never touches handler
//! logic.

/// Body-type codes offered in the chassis picker, in display order. The codes
/// are what the site expects in `chassis[]=...`; their localized labels live in
/// [`crate::i18n::Lang::chassis_label`] (#33 moved display text out of the
/// catalog once it became language-aware).
///
/// Subset of what polovni offers — the 6 a personal-shopper would realistically
/// tick. For an exotic body type (Minivan, Pickup), fall back to
/// `SEARCH_CHASSIS` in `.env`.
pub(super) const CHASSIS_CODES: &[u32] = &[2627, 2628, 2629, 2631, 2632, 2634];

/// Gearbox codes offered in the gearbox picker (#7), in display order. Unlike
/// chassis this is the site's **complete** list (the `SEARCH_GEARBOX` env
/// escape hatch still exists for symmetry). Labels:
/// [`crate::i18n::Lang::gearbox_label`].
pub(super) const GEARBOX_CODES: &[u32] = &[3210, 3211, 3212, 10795];

/// Predefined poll-interval presets in seconds, in display order. The minimum
/// (60s) matches `MIN_POLL_INTERVAL_SECS` so the picker never offers an illegal
/// value; labels come from [`crate::i18n::Lang::interval_label`]. Non-preset
/// intervals still go through `/interval N`.
pub(super) const INTERVAL_SECS: &[u64] = &[60, 300, 600, 1800, 3600, 7200, 86_400];

/// Predefined price ranges in EUR as `(from, to)` bounds — `0` on either side
/// means "no bound there". Six buckets cover practically every used-car intent;
/// custom ranges fall back to `.env` (`SEARCH_PRICE_FROM`/`SEARCH_PRICE_TO`).
/// Localized labels: [`crate::i18n::Lang::price_range_label`].
pub(super) const PRICE_RANGES: &[(u32, u32)] = &[
    (0, 5_000),
    (5_000, 10_000),
    (10_000, 15_000),
    (15_000, 25_000),
    (25_000, 50_000),
    (50_000, 0),
];

/// Predefined year ranges as `(from, to)` bounds, same convention as
/// [`PRICE_RANGES`] (the numbers fit `u16`, but we keep `u32` for
/// callback-data uniformity with prices). Labels:
/// [`crate::i18n::Lang::year_range_label`].
pub(super) const YEAR_RANGES: &[(u32, u32)] = &[
    (2024, 0),
    (2020, 2023),
    (2015, 2019),
    (2010, 2014),
    (2005, 2009),
    (0, 2004),
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
pub(super) const MODELS_BY_BRAND: &[(&str, &[(&str, &str)])] = &[
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
pub(super) fn models_for_brand(
    brand_slug: &str,
) -> Option<&'static [(&'static str, &'static str)]> {
    MODELS_BY_BRAND
        .iter()
        .find(|(b, _)| *b == brand_slug)
        .map(|(_, m)| *m)
}

/// Hardcoded brand list, exposed for `crate::dyncatalog` to return when the
/// site fetch fails or the cache is empty (#11) — the guaranteed fallback that
/// keeps the /filter wizard from ever rendering a blank brand picker.
pub(crate) fn fallback_brands() -> &'static [(&'static str, &'static str)] {
    BRANDS
}

/// Hardcoded model list for `brand_slug`, or `None` when we never guessed any
/// for it. Same fallback role as [`fallback_brands`]; a thin re-export of
/// [`models_for_brand`] under a name that reads as "fallback" at the call site.
pub(crate) fn fallback_models(brand_slug: &str) -> Option<&'static [(&'static str, &'static str)]> {
    models_for_brand(brand_slug)
}

/// Brand catalog: `(url_slug, display_name)`. The slug is what we pass to the
/// site as `?brand=...`; the display is what the user sees on a button.
///
/// Hardcoded for now — the site has more brands, but these are the 20 most
/// common in Serbia. If the user wants a brand not here, they can fall back
/// to setting `SEARCH_BRAND` in `.env`. Session 3.5 would add a "Другая
/// (ввести)" button using ForceReply, but that's not in scope today.
pub(super) const BRANDS: &[(&str, &str)] = &[
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[test]
    fn models_for_brand_finds_known_and_rejects_unknown() {
        let mini = models_for_brand("mini").unwrap();
        assert!(mini.iter().any(|(slug, _)| *slug == "cooper"));
        // Brand in BRANDS but without a model catalog — the picker shows the
        // "no catalog" hint, so the lookup must be None, not empty.
        assert!(models_for_brand("volvo").is_none());
        assert!(models_for_brand("definitely-not-a-brand").is_none());
    }

    #[test]
    fn every_model_catalog_brand_is_in_the_brand_picker() {
        // A model catalog for a brand the picker can't select is dead data;
        // catch the drift when someone extends one list but not the other.
        for (brand, _) in MODELS_BY_BRAND {
            assert!(
                BRANDS.iter().any(|(slug, _)| slug == brand),
                "brand {brand} has models but no BRANDS entry"
            );
        }
    }

    #[test]
    fn catalog_slugs_are_url_safe() {
        // Slugs are embedded raw into callback data and search URLs — the
        // same alphabet /setbrand validates (a-z, 0-9, hyphen).
        let ok = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        for (slug, _) in BRANDS {
            assert!(ok(slug), "brand slug {slug}");
        }
        for (brand, models) in MODELS_BY_BRAND {
            for (slug, _) in *models {
                assert!(ok(slug), "model slug {slug} of {brand}");
            }
        }
    }
}
