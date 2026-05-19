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

use anyhow::{Result, anyhow, bail};

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
    /// Telegram user-id authorised to send commands. Other users' messages
    /// are logged and dropped.
    pub authorized_user_id: i64,
    pub save_raw_html: bool,
    pub zero_results_alert_threshold: u32,
    pub dumps_dir: PathBuf,
    /// Delete HTML dump folders older than this many days. `0` disables rotation.
    pub dump_retention_days: u32,
}

// Manual `Debug` to redact `telegram_token`. `#[derive(Debug)]` would dump it.
impl std::fmt::Debug for StaticConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticConfig")
            .field("database_path", &self.database_path)
            .field("telegram_token", &"<redacted>")
            .field("telegram_chat_id", &self.telegram_chat_id)
            .field("authorized_user_id", &self.authorized_user_id)
            .field("save_raw_html", &self.save_raw_html)
            .field(
                "zero_results_alert_threshold",
                &self.zero_results_alert_threshold,
            )
            .field("dumps_dir", &self.dumps_dir)
            .field("dump_retention_days", &self.dump_retention_days)
            .finish()
    }
}

impl StaticConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_path: load_database_path(),
            telegram_token: req_string("TELEGRAM_BOT_TOKEN")?,
            telegram_chat_id: req_parsed::<i64>("TELEGRAM_CHAT_ID")?,
            authorized_user_id: req_parsed::<i64>("AUTHORIZED_USER_ID")?,
            save_raw_html: opt_bool("SAVE_RAW_HTML")?.unwrap_or(true),
            zero_results_alert_threshold: opt_parsed::<u32>("ZERO_RESULTS_ALERT_THRESHOLD")?
                .unwrap_or(3),
            dumps_dir: opt_string("DUMPS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./dumps")),
            dump_retention_days: opt_parsed::<u32>("DUMP_RETENTION_DAYS")?.unwrap_or(7),
        })
    }
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
                    .filter_map(|p| p.trim().parse::<u32>().ok())
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
        // The little helper closure keeps the four lookups uniform.
        let bound_u32 = |key: &str| -> Result<Option<Option<u32>>> {
            Ok(storage.get_setting(key)?.map(|s| {
                if s.is_empty() {
                    None
                } else {
                    s.parse::<u32>().ok()
                }
            }))
        };
        let bound_u16 = |key: &str| -> Result<Option<Option<u16>>> {
            Ok(storage.get_setting(key)?.map(|s| {
                if s.is_empty() {
                    None
                } else {
                    s.parse::<u16>().ok()
                }
            }))
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
        if let Some(s) = storage.get_setting(SETTING_POLL_INTERVAL_SECS)?
            && let Ok(secs) = s.parse::<u64>()
            && secs >= MIN_POLL_INTERVAL_SECS
        {
            poll_interval = Duration::from_secs(secs);
        }

        // `paused` is pure-runtime: no env knob, defaults to false.
        let paused = storage
            .get_setting(SETTING_PAUSED)?
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(false);

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
    fn opt_bool_rejects_garbage() {
        unsafe {
            env::set_var("NJUSKA_TEST_BOOL_BAD", "maybe");
        }
        assert!(opt_bool("NJUSKA_TEST_BOOL_BAD").is_err());
    }
}
