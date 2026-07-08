//! Configuration loaded from environment variables (typically `.env` + `dotenvy`).
//!
//! In v2 the config splits into two pieces:
//!
//! * [`StaticConfig`] — values that come from `.env` and **never change** during
//!   the bot's lifetime (TG token, chat id, paths, fixed thresholds). Passed
//!   around by ordinary reference. No synchronisation needed.
//! * [`RuntimeConfig`] — values the user can change at runtime via TG commands
//!   (`/pause`, `/interval`, `/filter`). Persisted to the `runtime_settings`
//!   table so changes survive restarts. Wrapped in `Arc<RwLock<…>>` at the
//!   call site once the command loop comes online (session-1 chunk C).
//!
//! Loading order: `StaticConfig::from_env` → open Storage → `RuntimeConfig::load`
//! (env defaults, then overridden by DB).

use std::env;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tracing::warn;
use url::Url;

use crate::models::{SearchFilter, ShowOldNew};
use crate::storage::Storage;

// ---------------------------------------------------------------------------
// Setting keys used inside `runtime_settings`. Centralised here so command
// handlers and `RuntimeConfig::load` agree on the spelling.
// ---------------------------------------------------------------------------
pub const SETTING_PAUSED: &str = "paused";
pub const SETTING_POLL_INTERVAL_SECS: &str = "poll_interval_secs";
/// Brand slug as used in the site's URL (e.g. "mini", "bmw"). An empty value
/// means "user explicitly cleared the brand via `/filter`"; an *absent* key
/// means "use the env default" (`SEARCH_BRAND`). The distinction matters —
/// see [`RuntimeConfig::load`].
pub const SETTING_SEARCH_BRAND: &str = "search_brand";
/// Body-type codes as comma-separated `u32`s (e.g. "2634" or "2634,2632").
/// Same key-absent / key-empty / key-set semantics as `SETTING_SEARCH_BRAND`.
pub const SETTING_SEARCH_CHASSIS: &str = "search_chassis";
/// Model slugs, comma-separated (e.g. "cooper" or "cooper,countryman").
/// Slug values are brand-specific and ultimately fed to the site's
/// `model[]=...` query param. Same three-state convention.
pub const SETTING_SEARCH_MODELS: &str = "search_models";
/// Price range bounds. Stored as plain decimal strings; an empty string means
/// "no bound on this side" (user explicitly cleared via `/filter`). Absent key
/// → use env default.
pub const SETTING_SEARCH_PRICE_FROM: &str = "search_price_from";
pub const SETTING_SEARCH_PRICE_TO: &str = "search_price_to";
/// Year range bounds. Same shape as price.
pub const SETTING_SEARCH_YEAR_FROM: &str = "search_year_from";
pub const SETTING_SEARCH_YEAR_TO: &str = "search_year_to";

// ---------------------------------------------------------------------------
// StaticConfig
// ---------------------------------------------------------------------------

/// Knobs that come from `.env` and are fixed for the bot's lifetime.
#[derive(Clone)]
pub struct StaticConfig {
    pub database_path: PathBuf,
    pub telegram_token: String,
    pub telegram_chat_id: i64,
    /// Telegram user-ids authorised to send commands (#9) — a small group
    /// (family / co-renters) can share the bot. `AUTHORIZED_USER_ID` accepts
    /// a comma-separated list; a single id keeps working. Other users'
    /// messages are logged and dropped. Never empty — enforced at load.
    pub authorized_user_ids: Vec<i64>,
    pub save_raw_html: bool,
    pub zero_results_alert_threshold: u32,
    /// Consecutive `fetch_search` failures before alerting to Telegram.
    /// Parallel to `zero_results_alert_threshold` but for the *fetch* leg
    /// (network, Cloudflare 403, proxy misconfig) rather than the parser.
    pub fetch_errors_alert_threshold: u32,
    pub dumps_dir: PathBuf,
    /// Delete HTML dump folders older than this many days. `0` disables rotation.
    pub dump_retention_days: u32,
    /// Cap on the *total* size of all HTML dumps, in MiB; oldest files are
    /// deleted first once it's exceeded (checked after date rotation).
    /// `0` disables the cap. Complements `dump_retention_days`: retention
    /// bounds age, this bounds bytes — a short poll interval can produce a
    /// lot of HTML within the retention window.
    pub dump_max_total_mb: u64,
    /// Delete `seen_listings` rows older than this many days (dedup memory).
    /// `0` = keep forever. Default 180 (~6 months). See `.env.example` for
    /// the re-notification caveat.
    pub seen_retention_days: u32,
    /// Optional Cloudflare Worker proxy. When `Some`, all `polovniautomobili.com`
    /// fetches go through this Worker (which forwards them on CF's own
    /// infrastructure). Bypasses CF's direct-fetch challenge — needed when
    /// the bot runs from a Linux network stack (homelab VMs, most VPS).
    /// Configure via `CF_PROXY_URL` + `CF_PROXY_SECRET`; see `cf-proxy/README.md`.
    pub cf_proxy: Option<ProxyConfig>,
}

/// Settings for routing scraper fetches through a Cloudflare Worker.
/// Loaded from `CF_PROXY_URL` + `CF_PROXY_SECRET`. Both env vars must be set
/// together — providing only one is treated as "no proxy".
#[derive(Clone)]
pub struct ProxyConfig {
    /// Worker URL, e.g. `https://nau-proxy.<your-subdomain>.workers.dev`.
    pub url: Url,
    /// Shared secret sent as `x-proxy-secret` header. Must match the
    /// `PROXY_SECRET` env var set on the Worker via `wrangler secret put`.
    pub secret: String,
}

// Manual `Debug` so the shared secret doesn't accidentally leak via
// `info!("{:?}", config)`. URL is fine to print.
impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("url", &self.url.as_str())
            .field("secret", &"<redacted>")
            .finish()
    }
}

// Manual `Debug` to redact `telegram_token`. `#[derive(Debug)]` would dump it.
impl std::fmt::Debug for StaticConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticConfig")
            .field("database_path", &self.database_path)
            .field("telegram_token", &"<redacted>")
            .field("telegram_chat_id", &self.telegram_chat_id)
            .field("authorized_user_ids", &self.authorized_user_ids)
            .field("save_raw_html", &self.save_raw_html)
            .field(
                "zero_results_alert_threshold",
                &self.zero_results_alert_threshold,
            )
            .field(
                "fetch_errors_alert_threshold",
                &self.fetch_errors_alert_threshold,
            )
            .field("dumps_dir", &self.dumps_dir)
            .field("dump_retention_days", &self.dump_retention_days)
            .field("dump_max_total_mb", &self.dump_max_total_mb)
            .field("seen_retention_days", &self.seen_retention_days)
            .field("cf_proxy", &self.cf_proxy)
            .finish()
    }
}

impl StaticConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_path: load_database_path(),
            telegram_token: req_string("TELEGRAM_BOT_TOKEN")?,
            telegram_chat_id: req_parsed::<i64>("TELEGRAM_CHAT_ID")?,
            authorized_user_ids: req_csv_parsed::<i64>("AUTHORIZED_USER_ID")?,
            save_raw_html: opt_bool("SAVE_RAW_HTML")?.unwrap_or(true),
            zero_results_alert_threshold: opt_parsed::<u32>("ZERO_RESULTS_ALERT_THRESHOLD")?
                .unwrap_or(3),
            fetch_errors_alert_threshold: opt_parsed::<u32>("FETCH_ERRORS_ALERT_THRESHOLD")?
                .unwrap_or(3),
            dumps_dir: opt_string("DUMPS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./dumps")),
            dump_retention_days: opt_parsed::<u32>("DUMP_RETENTION_DAYS")?.unwrap_or(7),
            dump_max_total_mb: opt_parsed::<u64>("DUMP_MAX_TOTAL_MB")?.unwrap_or(0),
            seen_retention_days: opt_parsed::<u32>("SEEN_RETENTION_DAYS")?.unwrap_or(180),
            cf_proxy: load_cf_proxy()?,
        })
    }

    /// Whether this Telegram user-id may issue commands. Linear scan — the
    /// list is a handful of family members, not a user base.
    pub fn is_authorized(&self, user_id: i64) -> bool {
        self.authorized_user_ids.contains(&user_id)
    }
}

/// Loads `CF_PROXY_URL` + `CF_PROXY_SECRET`. Both must be set; either missing
/// → `None` (direct fetch). A malformed URL → hard error (clearer than
/// silently falling back to direct fetch and confusing the operator).
fn load_cf_proxy() -> Result<Option<ProxyConfig>> {
    let (Some(url_str), Some(secret)) = (opt_string("CF_PROXY_URL"), opt_string("CF_PROXY_SECRET"))
    else {
        return Ok(None);
    };
    let url = Url::parse(&url_str)
        .with_context(|| format!("CF_PROXY_URL={url_str:?} is not a valid URL"))?;
    Ok(Some(ProxyConfig { url, secret }))
}

// ---------------------------------------------------------------------------
// RuntimeConfig
// ---------------------------------------------------------------------------

/// Knobs the user can change at runtime via TG commands.
///
/// No secrets here — derives `Debug` outright. In chunk C this will be wrapped
/// in `Arc<RwLock<RuntimeConfig>>` so the command loop can mutate it while the
/// poll loop reads it.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub search: SearchFilter,
    pub poll_interval: Duration,
    /// When true, the poll loop skips fetch+send. Toggled via `/pause` /
    /// `/resume`. Persisted in `runtime_settings` so a paused bot stays paused
    /// across restarts.
    pub paused: bool,
}

impl RuntimeConfig {
    /// Loads runtime knobs in two passes:
    ///
    /// 1. **Env defaults** via the same loaders [`StaticConfig`] uses.
    /// 2. **DB overrides** from `runtime_settings` — values the user set via
    ///    commands. DB takes precedence over env.
    ///
    /// Loading order matters: env is the "fresh deployment" path; DB is the
    /// "user touched this in the bot" path. New deployments inherit env;
    /// later changes win.
    pub fn load(storage: &Storage) -> Result<Self> {
        let mut search = load_search_filter()?;

        // `brand` may be overridden via `/filter` command. Three cases:
        //   key absent  → use env value (already loaded into `search.brand`)
        //   key empty   → user explicitly cleared the brand (no filter)
        //   key non-empty → user picked this brand
        // Other filter fields (models, chassis, price, year) will get the
        // same treatment in subsequent sessions.
        if let Some(s) = storage.get_setting(SETTING_SEARCH_BRAND)? {
            search.brand = if s.is_empty() { None } else { Some(s) };
        }

        // Same three-state convention for chassis. The on-disk form is a
        // comma-separated list of `u32`s. Empty string → no filter; absent →
        // env-default; non-empty → these codes.
        if let Some(s) = storage.get_setting(SETTING_SEARCH_CHASSIS)? {
            search.chassis = if s.is_empty() {
                Vec::new()
            } else {
                s.split(',')
                    .filter_map(|p| {
                        let part = p.trim();
                        // A bad code in the DB (manual edit, older buggy
                        // version) is dropped, but loudly (#29) — a silently
                        // vanishing filter is maddening to debug.
                        let parsed = part.parse::<u32>().ok();
                        if parsed.is_none() {
                            warn!(
                                key = SETTING_SEARCH_CHASSIS,
                                value = part,
                                "ignoring unparseable chassis code in runtime_settings"
                            );
                        }
                        parsed
                    })
                    .collect()
            };
        }

        // Models: same shape as chassis but with `String` slugs rather than
        // `u32` codes. Empty string → no filter; absent → env-default;
        // non-empty → these slugs.
        if let Some(s) = storage.get_setting(SETTING_SEARCH_MODELS)? {
            search.models = if s.is_empty() {
                Vec::new()
            } else {
                s.split(',')
                    .map(|p| p.trim().to_owned())
                    .filter(|p| !p.is_empty())
                    .collect()
            };
        }

        // For each range bound: empty string → `None` (explicitly cleared);
        // non-empty parseable → `Some(value)`; absent key → leave env value.
        // The little helper closure keeps the four lookups uniform. Parse
        // failures degrade to "no bound" but warn (#29).
        let bound_u32 = |key: &str| -> Result<Option<Option<u32>>> {
            Ok(storage
                .get_setting(key)?
                .map(|s| parse_stored_bound::<u32>(key, &s)))
        };
        let bound_u16 = |key: &str| -> Result<Option<Option<u16>>> {
            Ok(storage
                .get_setting(key)?
                .map(|s| parse_stored_bound::<u16>(key, &s)))
        };

        if let Some(v) = bound_u32(SETTING_SEARCH_PRICE_FROM)? {
            search.price_from = v;
        }
        if let Some(v) = bound_u32(SETTING_SEARCH_PRICE_TO)? {
            search.price_to = v;
        }
        if let Some(v) = bound_u16(SETTING_SEARCH_YEAR_FROM)? {
            search.year_from = v;
        }
        if let Some(v) = bound_u16(SETTING_SEARCH_YEAR_TO)? {
            search.year_to = v;
        }

        let mut poll_interval = load_poll_interval()?;
        if let Some(s) = storage.get_setting(SETTING_POLL_INTERVAL_SECS)? {
            match s.parse::<u64>() {
                Ok(secs) if secs >= MIN_POLL_INTERVAL_SECS => {
                    poll_interval = Duration::from_secs(secs);
                }
                Ok(secs) => warn!(
                    key = SETTING_POLL_INTERVAL_SECS,
                    value = secs,
                    min = MIN_POLL_INTERVAL_SECS,
                    "stored poll interval below minimum; keeping env/default value"
                ),
                Err(_) => warn!(
                    key = SETTING_POLL_INTERVAL_SECS,
                    value = %s,
                    "unparseable poll interval in runtime_settings; keeping env/default value"
                ),
            }
        }

        // `paused` is pure-runtime: no env knob, defaults to false.
        let paused = match storage.get_setting(SETTING_PAUSED)? {
            Some(s) => s.parse::<bool>().unwrap_or_else(|_| {
                warn!(
                    key = SETTING_PAUSED,
                    value = %s,
                    "unparseable paused flag in runtime_settings; defaulting to false"
                );
                false
            }),
            None => false,
        };

        Ok(Self {
            search,
            poll_interval,
            paused,
        })
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parses a stored range bound from `runtime_settings`. Empty string means
/// "explicitly cleared" → `None`; a non-empty unparseable value also becomes
/// `None` but logs a `warn!` (#29) so corrupt DB values don't silently drop
/// a filter the user believes is active.
fn parse_stored_bound<T>(key: &str, s: &str) -> Option<T>
where
    T: FromStr,
{
    if s.is_empty() {
        return None;
    }
    let parsed = s.parse::<T>().ok();
    if parsed.is_none() {
        warn!(
            key,
            value = s,
            "ignoring unparseable numeric bound in runtime_settings"
        );
    }
    parsed
}

/// Hard floor: anything below a minute is impolite to the upstream site and
/// serves no real purpose (listings don't appear that fast). Enforced in
/// three places — env loader, DB-override merging, and the `/interval N`
/// command handler — so a user can't sneak below it from any direction.
pub const MIN_POLL_INTERVAL_SECS: u64 = 60;

fn load_database_path() -> PathBuf {
    // `PathBuf::from` is infallible — invalid paths only surface when
    // `Storage::new` opens the file.
    opt_string("DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./njuska.db"))
}

fn load_search_filter() -> Result<SearchFilter> {
    Ok(SearchFilter {
        brand: opt_string("SEARCH_BRAND"),
        models: opt_csv("SEARCH_MODEL"),
        chassis: opt_csv_parsed::<u32>("SEARCH_CHASSIS")?,
        price_from: opt_parsed::<u32>("SEARCH_PRICE_FROM")?,
        price_to: opt_parsed::<u32>("SEARCH_PRICE_TO")?,
        year_from: opt_parsed::<u16>("SEARCH_YEAR_FROM")?,
        year_to: opt_parsed::<u16>("SEARCH_YEAR_TO")?,
        without_price: opt_bool("SEARCH_WITHOUT_PRICE")?.unwrap_or(false),
        show_old_new: parse_show_old_new()?,
    })
}

fn load_poll_interval() -> Result<Duration> {
    let secs = opt_parsed::<u64>("POLL_INTERVAL_SECS")?.unwrap_or(600);
    if secs < MIN_POLL_INTERVAL_SECS {
        bail!("POLL_INTERVAL_SECS={secs} is below the minimum {MIN_POLL_INTERVAL_SECS}");
    }
    Ok(Duration::from_secs(secs))
}

fn parse_show_old_new() -> Result<ShowOldNew> {
    let Some(s) = opt_string("SEARCH_SHOW_OLD_NEW") else {
        return Ok(ShowOldNew::default());
    };
    match s.to_ascii_lowercase().as_str() {
        "all" => Ok(ShowOldNew::All),
        "old" => Ok(ShowOldNew::Old),
        "new" => Ok(ShowOldNew::New),
        other => bail!("SEARCH_SHOW_OLD_NEW={other:?}: expected one of `all`, `old`, `new`"),
    }
}

// ---------------------------------------------------------------------------
// env-parsing helpers
//
// All follow the same convention: a variable that is **unset OR empty** is
// treated as "not provided". Shell users habitually `FOO=` to "blank out" a
// value; making that equivalent to `unset FOO` removes a footgun.
// ---------------------------------------------------------------------------

/// Returns `Some(value)` if the env var is set and non-empty, else `None`.
fn opt_string(key: &str) -> Option<String> {
    env::var(key).ok().filter(|s| !s.is_empty())
}

/// Like `opt_string` but errors out (with the key name) when missing. Use for
/// fields the bot can't start without.
fn req_string(key: &str) -> Result<String> {
    opt_string(key).ok_or_else(|| anyhow!("{key} is required but not set in env"))
}

/// `req_string` + `FromStr::parse`. Same error-wrapping convention as
/// `opt_parsed` so failure messages cite the offending key.
fn req_parsed<T>(key: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let s = req_string(key)?;
    s.parse::<T>().map_err(|e| anyhow!("{key}={s:?}: {e}"))
}

/// Parses a comma-separated string into a `Vec<String>`. Empty list if unset.
fn opt_csv(key: &str) -> Vec<String> {
    opt_string(key)
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Parses a comma-separated list of values into `Vec<T>`. Empty list if unset.
fn opt_csv_parsed<T>(key: &str) -> Result<Vec<T>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let Some(raw) = opt_string(key) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.parse::<T>()
                .map_err(|e| anyhow!("{key}: failed to parse `{p}`: {e}"))
        })
        .collect()
}

/// `opt_csv_parsed` + "must not be empty": errors out (citing the key) when
/// the variable is unset, empty, or contains only separators. Use for
/// list-shaped fields the bot can't start without (`AUTHORIZED_USER_ID`).
fn req_csv_parsed<T>(key: &str) -> Result<Vec<T>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let v = opt_csv_parsed::<T>(key)?;
    if v.is_empty() {
        return Err(anyhow!("{key} is required but not set in env"));
    }
    Ok(v)
}

/// Parses a single optional value through `FromStr`. `None` if unset/empty.
fn opt_parsed<T>(key: &str) -> Result<Option<T>>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let Some(s) = opt_string(key) else {
        return Ok(None);
    };
    s.parse::<T>()
        .map(Some)
        .map_err(|e| anyhow!("{key}={s:?}: {e}"))
}

/// Permissive boolean parser: accepts `true`/`1`/`yes`/`on` and their negatives,
/// case-insensitive. Anything else is an error.
fn opt_bool(key: &str) -> Result<Option<bool>> {
    let Some(s) = opt_string(key) else {
        return Ok(None);
    };
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(Some(true)),
        "false" | "0" | "no" | "off" => Ok(Some(false)),
        other => bail!("{key}={other:?}: expected a boolean (`true`/`false`/`1`/`0`/...)"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    // The `env` is process-global; multiple tests touching the same key would
    // race. We use **distinct keys per test** instead of a mutex — simpler
    // and lets `cargo test` parallelise.

    #[test]
    fn opt_string_treats_empty_as_absent() {
        // Safety: env::set_var is unsafe in 2024 edition because it's not
        // thread-safe vs. concurrent env::var reads in other threads. In test
        // code this is acceptable since we know what's running.
        unsafe {
            env::set_var("NJUSKA_TEST_EMPTY_AS_ABSENT", "");
        }
        assert_eq!(opt_string("NJUSKA_TEST_EMPTY_AS_ABSENT"), None);
        assert_eq!(opt_string("NJUSKA_TEST_NEVER_SET"), None);
    }

    #[test]
    fn opt_parsed_reports_key_on_error() {
        unsafe {
            env::set_var("NJUSKA_TEST_BAD_NUM", "potato");
        }
        let err = opt_parsed::<u32>("NJUSKA_TEST_BAD_NUM")
            .unwrap_err()
            .to_string();
        assert!(err.contains("NJUSKA_TEST_BAD_NUM"), "{err}");
        assert!(err.contains("potato"), "{err}");
    }

    #[test]
    fn opt_csv_parsed_drops_empties_and_trims() {
        unsafe {
            env::set_var("NJUSKA_TEST_CSV_NUMS", " 10, , 20 ,30");
        }
        let v: Vec<u16> = opt_csv_parsed("NJUSKA_TEST_CSV_NUMS").unwrap();
        assert_eq!(v, vec![10, 20, 30]);
    }

    #[test]
    fn opt_bool_accepts_common_truthy_values() {
        for (input, expected) in [
            ("true", true),
            ("TRUE", true),
            ("1", true),
            ("yes", true),
            ("on", true),
            ("false", false),
            ("0", false),
            ("no", false),
            ("off", false),
        ] {
            unsafe {
                env::set_var("NJUSKA_TEST_BOOL", input);
            }
            assert_eq!(
                opt_bool("NJUSKA_TEST_BOOL").unwrap(),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn parse_stored_bound_handles_empty_garbage_and_valid() {
        // Empty = explicitly cleared, no warning path.
        assert_eq!(parse_stored_bound::<u32>("k", ""), None);
        // Garbage degrades to None (and warns — not asserted here).
        assert_eq!(parse_stored_bound::<u32>("k", "potato"), None);
        // Valid parses through.
        assert_eq!(parse_stored_bound::<u16>("k", "2015"), Some(2015));
    }

    #[test]
    fn req_csv_parsed_accepts_single_and_multiple_ids() {
        // Single id — the pre-#9 format keeps working unchanged.
        unsafe {
            env::set_var("NJUSKA_TEST_REQ_CSV_ONE", "12345");
        }
        assert_eq!(
            req_csv_parsed::<i64>("NJUSKA_TEST_REQ_CSV_ONE").unwrap(),
            vec![12345]
        );
        // Comma-separated list, whitespace-tolerant.
        unsafe {
            env::set_var("NJUSKA_TEST_REQ_CSV_MANY", "111, 222 ,333");
        }
        assert_eq!(
            req_csv_parsed::<i64>("NJUSKA_TEST_REQ_CSV_MANY").unwrap(),
            vec![111, 222, 333]
        );
    }

    #[test]
    fn req_csv_parsed_rejects_missing_empty_and_garbage() {
        let err = req_csv_parsed::<i64>("NJUSKA_TEST_REQ_CSV_UNSET")
            .unwrap_err()
            .to_string();
        assert!(err.contains("NJUSKA_TEST_REQ_CSV_UNSET"), "{err}");

        // Only separators = effectively empty — still an error, not vec![].
        unsafe {
            env::set_var("NJUSKA_TEST_REQ_CSV_SEPARATORS", " , ,");
        }
        assert!(req_csv_parsed::<i64>("NJUSKA_TEST_REQ_CSV_SEPARATORS").is_err());

        unsafe {
            env::set_var("NJUSKA_TEST_REQ_CSV_GARBAGE", "111,potato");
        }
        let err = req_csv_parsed::<i64>("NJUSKA_TEST_REQ_CSV_GARBAGE")
            .unwrap_err()
            .to_string();
        assert!(err.contains("potato"), "{err}");
    }

    #[test]
    fn is_authorized_matches_any_listed_id() {
        let cfg = StaticConfig {
            database_path: PathBuf::from("/dev/null"),
            telegram_token: "t".into(),
            telegram_chat_id: 1,
            authorized_user_ids: vec![111, 222],
            save_raw_html: false,
            zero_results_alert_threshold: 3,
            fetch_errors_alert_threshold: 3,
            dumps_dir: PathBuf::from("/tmp"),
            dump_retention_days: 0,
            dump_max_total_mb: 0,
            seen_retention_days: 0,
            cf_proxy: None,
        };
        assert!(cfg.is_authorized(111));
        assert!(cfg.is_authorized(222));
        assert!(!cfg.is_authorized(333));
    }

    #[test]
    fn opt_bool_rejects_garbage() {
        unsafe {
            env::set_var("NJUSKA_TEST_BOOL_BAD", "maybe");
        }
        assert!(opt_bool("NJUSKA_TEST_BOOL_BAD").is_err());
    }
}
