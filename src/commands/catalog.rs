//! Catalog data for the /filter wizard — brands, models, body types, and
//! the preset ranges the pickers offer.
//!
//! Pure data + lookup helpers, no I/O and no Telegram types. Kept separate
//! so growing the catalogs (the most common edit) never touches handler
//! logic.

/// Body-type catalog: `(numeric_code, display_name)`. Codes are the ones the
/// site uses internally in `chassis[]=...`. Display names are in Russian
/// (Cyrillic) for friendlier UI; the original Serbian names (Kabriolet,
/// Limuzina, …) read close enough but the Russian spellings feel more native.
///
/// Subset of what polovni offers — these are the 6 a personal-shopper would
/// realistically tick. If you need a more exotic body type (Minivan, Pickup),
/// fall back to `SEARCH_CHASSIS` in `.env`.
pub(super) const CHASSIS: &[(u32, &str)] = &[
    (2627, "Универсал"),
    (2628, "Купе"),
    (2629, "Хэтчбек"),
    (2631, "Седан"),
    (2632, "Внедорожник"),
    (2634, "Кабриолет"),
];

/// Gearbox catalog: `(numeric_code, display_name)`. Codes come from the
/// site's `<select name="gearbox[]">` filter — unlike chassis this is the
/// **complete** list, so there's no `.env`-only exotic tail here (the env
/// escape hatch `SEARCH_GEARBOX` still exists for symmetry with the other
/// filters). Russian labels, same rationale as [`CHASSIS`].
pub(super) const GEARBOX: &[(u32, &str)] = &[
    (3210, "Механика (4 ст.)"),
    (3211, "Механика (5 ст.)"),
    (3212, "Механика (6 ст.)"),
    (10795, "Автомат / полуавтомат"),
];

/// Predefined poll-interval presets in seconds. The minimum (60s) matches
/// `MIN_POLL_INTERVAL_SECS` so the picker never offers an illegal value.
/// For non-preset intervals, the user can still type `/interval N`.
pub(super) const INTERVAL_PRESETS: &[(u64, &str)] = &[
    (60, "1 мин"),
    (300, "5 мин"),
    (600, "10 мин"),
    (1800, "30 мин"),
    (3600, "1 час"),
    (7200, "2 часа"),
    (86_400, "сутки"),
];

/// Predefined price ranges, in EUR. `(from, to, display)` — `0` on either
/// side means "no bound there". The list is intentionally short — six
/// buckets cover practically every used-car shopping intent. Users who
/// need a custom range fall back to `.env` (`SEARCH_PRICE_FROM`, `SEARCH_PRICE_TO`).
pub(super) const PRICE_RANGES: &[(u32, u32, &str)] = &[
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
pub(super) const YEAR_RANGES: &[(u32, u32, &str)] = &[
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

/// Reverse lookup: chassis code → human label from the [`CHASSIS`] catalog.
/// Codes set via `SEARCH_CHASSIS` in `.env` may be outside the catalog —
/// those render as the raw number so nothing is silently hidden (issue #4).
pub(super) fn chassis_label(code: u32) -> String {
    CHASSIS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| code.to_string())
}

/// Reverse lookup: gearbox code → human label from the [`GEARBOX`] catalog.
/// Same raw-number fallback as [`chassis_label`] for out-of-catalog codes
/// coming from `SEARCH_GEARBOX` in `.env`.
pub(super) fn gearbox_label(code: u32) -> String {
    GEARBOX
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| code.to_string())
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
    fn gearbox_label_maps_known_codes_and_passes_through_unknown() {
        assert_eq!(gearbox_label(3211), "Механика (5 ст.)");
        assert_eq!(gearbox_label(10795), "Автомат / полуавтомат");
        // Out-of-catalog code from SEARCH_GEARBOX in .env — raw code.
        assert_eq!(gearbox_label(9999), "9999");
    }

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
