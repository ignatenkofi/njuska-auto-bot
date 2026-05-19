//! SQLite-backed dedup store for listing IDs.
//!
//! The store has a single job: tell the poll loop *which listings from this
//! batch we haven't seen before*, and remember that we've now seen them — in
//! the same atomic step. Anything that splits "check" and "remember" across
//! two calls invites a race where a crash between the two re-notifies the
//! user on the next run.
//!
//! ## Why sync rusqlite under tokio?
//!
//! rusqlite is a synchronous wrapper over the SQLite C library. Strictly
//! speaking, blocking the tokio executor on file I/O is impolite. In practice
//! we poll every 10+ minutes with a batch of ~15 IDs — the whole `filter_new`
//! call runs in *microseconds* on local NVMe. There is no other task running
//! that would notice the stall.
//!
//! If this ever grows (a different schema, a long full-table scan), wrap the
//! Storage calls in `tokio::task::spawn_blocking` at the call site. Don't
//! bake `spawn_blocking` *into* this module — it just complicates everything
//! with no payoff today.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use tracing::{debug, info};

use crate::models::Listing;

/// Schema migrations. Each entry runs once, in order, via `execute_batch`.
/// Adding a new column: append a new `ALTER TABLE …` statement here — never
/// edit an existing one.
///
/// We keep migrations inline (not in `.sql` files) because they're short and
/// shipping them as `include_str!` adds a small build-time wrinkle we don't need.
const MIGRATIONS: &[&str] = &[
    // v1: the dedup table itself.
    "CREATE TABLE IF NOT EXISTS seen_listings (
        id          INTEGER PRIMARY KEY,
        title       TEXT NOT NULL,
        url         TEXT NOT NULL,
        price_text  TEXT,
        city        TEXT,
        year        INTEGER,
        mileage_km  INTEGER,
        first_seen  TEXT NOT NULL DEFAULT (datetime('now'))
    );",
    // v2: schemaless key/value store for runtime-mutable config.
    // Used by `/pause`, `/interval`, `/filter` (in later sessions) to persist
    // user changes across bot restarts. Values are TEXT so callers serialise/
    // deserialise on the way in/out — keeps storage dumb.
    "CREATE TABLE IF NOT EXISTS runtime_settings (
        key        TEXT PRIMARY KEY,
        value      TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    );",
];

/// Wraps a single SQLite [`Connection`].
///
/// We keep the connection as a private field and expose only the operations
/// the rest of the bot actually needs. That keeps SQL out of `main.rs` and
/// `scraper.rs` entirely — they don't know rusqlite exists, so swapping to
/// sled / Postgres / a flat file later is a local edit.
///
/// ## Why `Mutex<Connection>` instead of just `Connection`?
///
/// In v2 the bot runs two concurrent tasks (the poll loop and the command
/// dispatcher), and `tokio::spawn` requires futures to be `Send + 'static`.
/// `rusqlite::Connection` is `Send` but **not `Sync`** — so `Arc<Storage>`
/// (with a bare Connection inside) wouldn't be `Send` either, and we couldn't
/// share it across tasks.
///
/// Wrapping the Connection in a `std::sync::Mutex` makes `Storage: Send + Sync`,
/// which makes `Arc<Storage>: Send`. The lock is acquired only for the duration
/// of one synchronous SQLite operation (microseconds) — we never hold it across
/// an `.await`, so the executor isn't blocked.
pub struct Storage {
    conn: Mutex<Connection>,
}

impl Storage {
    /// Opens (or creates) the SQLite file and applies migrations.
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening SQLite at {}", path.display()))?;

        for sql in MIGRATIONS {
            conn.execute_batch(sql)
                .context("applying schema migration")?;
        }

        info!(path = %path.display(), "storage ready");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Acquires the inner mutex. Tiny helper so each public method gets the
    /// same `.expect("storage mutex poisoned")` message — poisoning means a
    /// previous holder panicked while holding the lock, which is a programmer
    /// bug rather than a recoverable runtime error.
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("storage mutex poisoned")
    }

    /// Returns the listings from `batch` whose IDs are **not yet in the store**.
    /// Pure read — does *not* mutate. Pair with [`mark_seen`] after the caller
    /// has done whatever it wants with the unseen ones (e.g. sent them to TG).
    ///
    /// Two-step API instead of the old `filter_new` (which marked atomically
    /// in the same transaction) because we want **mark-after-send semantics**:
    /// if Telegram fails, the listing should *stay* unseen so the next poll
    /// retries it.
    ///
    /// Implementation note: we use a *cached* prepared statement and execute
    /// it once per listing. With `id` as PRIMARY KEY each lookup is an O(log N)
    /// index probe — at our scale (batches of ~25) the overhead is invisible.
    pub fn unseen(&self, batch: &[Listing]) -> Result<Vec<Listing>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }

        // `prepare_cached` parses+plans the statement on first call and reuses
        // the compiled plan on subsequent calls (cache scoped to the connection).
        let conn = self.conn();
        let mut stmt = conn
            .prepare_cached("SELECT 1 FROM seen_listings WHERE id = ?1")
            .context("preparing unseen lookup")?;

        let mut result = Vec::with_capacity(batch.len());
        for l in batch {
            // `query_row` errors with `QueryReturnedNoRows` if no match;
            // `.optional()` turns that into `Ok(None)` so we can treat
            // "no row" as the normal "not seen" path rather than an error.
            let exists: Option<i64> = stmt
                .query_row([l.id as i64], |r| r.get(0))
                .optional()
                .with_context(|| format!("checking listing {}", l.id))?;
            if exists.is_none() {
                result.push(l.clone());
            }
        }
        debug!(
            batch = batch.len(),
            unseen = result.len(),
            "unseen lookup done"
        );
        Ok(result)
    }

    /// Records the given listings as seen. Idempotent — already-known IDs are
    /// left untouched (`ON CONFLICT DO NOTHING`), preserving their original
    /// `first_seen` timestamp.
    ///
    /// Wrapped in a transaction so a mid-batch failure either commits the
    /// whole set or none of it — never a partial run.
    ///
    /// Why does it take `&self`, not `&mut self`? rusqlite's `Connection` is
    /// internally-mutable; mutation goes through SQLite, not Rust's borrow
    /// checker. Public-API-wise this means callers can hold long-lived
    /// `&Storage` references without contention.
    pub fn mark_seen(&self, listings: &[Listing]) -> Result<()> {
        if listings.is_empty() {
            return Ok(());
        }

        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
            .context("starting mark_seen transaction")?;
        {
            // Scope the prepared statement so it drops before `tx.commit()`
            // releases the borrow. Without this, `commit()` won't compile —
            // `stmt` still holds `&tx`.
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO seen_listings
                        (id, title, url, price_text, city, year, mileage_km)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(id) DO NOTHING",
                )
                .context("preparing mark_seen insert")?;
            for l in listings {
                stmt.execute(params![
                    // SQLite stores INTEGER as i64. Polovni IDs are well
                    // under 2^31, so this cast is lossless in practice.
                    l.id as i64,
                    l.title,
                    l.url,
                    l.price_text,
                    l.city,
                    l.year,
                    l.mileage_km,
                ])
                .with_context(|| format!("inserting listing {}", l.id))?;
            }
        }
        tx.commit().context("committing mark_seen transaction")?;
        debug!(count = listings.len(), "mark_seen done");
        Ok(())
    }

    /// Fetches a runtime setting by key. Returns `Ok(None)` when the key has
    /// never been written — the caller decides what default to use.
    ///
    /// Storage is **dumb about types**: everything is text. Whoever owns the
    /// semantic of each key is responsible for serialising into / parsing out
    /// of a string. This keeps the storage layer agnostic about which settings
    /// exist; new keys can be added in the config layer alone.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let v: Option<String> = self
            .conn()
            .query_row(
                "SELECT value FROM runtime_settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .with_context(|| format!("reading runtime_settings[{key}]"))?;
        Ok(v)
    }

    /// Writes (or overwrites) a runtime setting. `updated_at` is refreshed on
    /// every write via the `ON CONFLICT DO UPDATE` clause — handy when debugging
    /// "when did this setting change?".
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO runtime_settings (key, value)
                 VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET
                    value = excluded.value,
                    updated_at = datetime('now')",
                params![key, value],
            )
            .with_context(|| format!("writing runtime_settings[{key}]"))?;
        debug!(key, value, "runtime setting saved");
        Ok(())
    }

    /// Returns the most recent `limit` listings by `first_seen`, descending.
    /// Used by `/dump N` for at-a-glance "what has the bot been seeing lately".
    ///
    /// We sort by `first_seen` *and* `id` so the order is fully deterministic
    /// when two rows share the same `first_seen` second (rare but possible
    /// for bulk-inserted batches).
    pub fn last_seen(&self, limit: u32) -> Result<Vec<Listing>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, title, url, price_text, city, year, mileage_km
                 FROM seen_listings
                 ORDER BY first_seen DESC, id DESC
                 LIMIT ?1",
            )
            .context("preparing last_seen statement")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(Listing {
                    // Mirrors the cast in `mark_seen`: SQLite stores u64 IDs as i64.
                    id: row.get::<_, i64>(0)? as u64,
                    title: row.get(1)?,
                    url: row.get(2)?,
                    price_text: row.get(3)?,
                    city: row.get(4)?,
                    // SQLite gives us Option<i64> for nullable INTEGER columns;
                    // cast to the narrower type from Listing.
                    year: row.get::<_, Option<i64>>(5)?.map(|v| v as u16),
                    mileage_km: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                })
            })
            .context("executing last_seen query")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collecting last_seen rows")
    }

    /// Wipes every row from `seen_listings`. Used by `/clear_confirm`.
    /// Returns the number of rows deleted (handy for the "wiped N entries"
    /// reply).
    ///
    /// We deliberately use `DELETE FROM seen_listings` rather than dropping
    /// and re-creating the table — that keeps the schema migrations sequence
    /// honest and avoids re-applying side effects (indices, defaults).
    pub fn clear_seen(&self) -> Result<u64> {
        let changes = self
            .conn()
            .execute("DELETE FROM seen_listings", [])
            .context("clearing seen_listings")?;
        info!(deleted = changes, "seen_listings cleared");
        Ok(changes as u64)
    }

    /// Total rows in `seen_listings`. Used by `/status` and by tests.
    ///
    /// Renamed from `count` to `seen_count` to be self-documenting now that
    /// we also have `runtime_settings` to count rows in (we don't, but the
    /// name would have ambiguous if we did).
    pub fn seen_count(&self) -> Result<u64> {
        // `query_row` errors with `QueryReturnedNoRows` if there are no rows;
        // `COUNT(*)` always returns exactly one row, so we just propagate
        // with `?` and cast i64 -> u64.
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM seen_listings", [], |r| r.get(0))?;
        Ok(n as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `Listing` with everything required and sensible defaults for
    /// the optional fields. Keeps tests below readable.
    fn listing(id: u64, title: &str) -> Listing {
        Listing {
            id,
            title: title.to_owned(),
            url: format!("https://www.polovniautomobili.com/auto-oglasi/{id}/x"),
            price_text: Some("1.000 €".into()),
            city: Some("Beograd".into()),
            year: Some(2015),
            mileage_km: Some(100_000),
        }
    }

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        // Returning the `TempDir` keeps it alive for the test's lifetime —
        // when it drops at end of scope, the directory (and DB file) is
        // wiped. If we returned only `Storage`, the dir would drop here.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.db");
        let s = Storage::new(&db).unwrap();
        (s, dir)
    }

    #[test]
    fn unseen_on_empty_store_returns_everything() {
        let (s, _dir) = temp_storage();
        let batch = vec![listing(1, "A"), listing(2, "B"), listing(3, "C")];

        let u = s.unseen(&batch).unwrap();
        assert_eq!(u.len(), 3);
        // Critically: `unseen` does NOT mutate the store.
        assert_eq!(s.seen_count().unwrap(), 0, "unseen must not write");
    }

    #[test]
    fn mark_seen_makes_listings_disappear_from_unseen() {
        let (s, _dir) = temp_storage();
        let batch = vec![listing(1, "A"), listing(2, "B")];

        // Step 1: caller observes both are unseen.
        let u = s.unseen(&batch).unwrap();
        assert_eq!(u.len(), 2);

        // Step 2: caller pretends to "send" them and marks them.
        s.mark_seen(&u).unwrap();
        assert_eq!(s.seen_count().unwrap(), 2);

        // Step 3: re-querying now reports zero unseen — that's the dedup
        // guarantee that the bot relies on.
        let u2 = s.unseen(&batch).unwrap();
        assert!(u2.is_empty());
    }

    #[test]
    fn mixed_batch_unseen_picks_only_new_ones() {
        let (s, _dir) = temp_storage();
        s.mark_seen(&[listing(10, "old A"), listing(20, "old B")])
            .unwrap();

        let mixed = vec![
            listing(10, "old A"), // seen
            listing(30, "new C"), // new
            listing(20, "old B"), // seen
            listing(40, "new D"), // new
        ];
        let u = s.unseen(&mixed).unwrap();

        let unseen_ids: Vec<u64> = u.iter().map(|l| l.id).collect();
        assert_eq!(unseen_ids, vec![30, 40]);
    }

    #[test]
    fn mark_seen_is_idempotent() {
        // The whole point of mark-after-send: if a retry succeeds and we
        // re-mark a listing, nothing weird happens. The first_seen timestamp
        // also keeps its original value (verified implicitly — no error and
        // count stays at 1).
        let (s, _dir) = temp_storage();
        s.mark_seen(&[listing(7, "L")]).unwrap();
        s.mark_seen(&[listing(7, "L")]).unwrap();
        s.mark_seen(&[listing(7, "L")]).unwrap();
        assert_eq!(s.seen_count().unwrap(), 1);
    }

    #[test]
    fn mark_seen_handles_empty_slice() {
        // Failed batch -> nothing to mark. Must not error or open a useless
        // transaction.
        let (s, _dir) = temp_storage();
        s.mark_seen(&[]).unwrap();
        assert_eq!(s.seen_count().unwrap(), 0);
    }

    #[test]
    fn unseen_handles_empty_slice() {
        let (s, _dir) = temp_storage();
        let u = s.unseen(&[]).unwrap();
        assert!(u.is_empty());
    }

    #[test]
    fn last_seen_returns_most_recent_first_and_caps_at_limit() {
        let (s, _dir) = temp_storage();
        // mark_seen runs in a single SQLite transaction, so all three rows
        // get the same `first_seen` timestamp. The secondary ORDER BY id DESC
        // then gives us deterministic ordering (3, 2, 1).
        s.mark_seen(&[listing(1, "A"), listing(2, "B"), listing(3, "C")])
            .unwrap();

        let top2 = s.last_seen(2).unwrap();
        let ids: Vec<u64> = top2.iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![3, 2], "newest first, capped at limit");

        // Limit greater than row count: returns everything, no error.
        let all = s.last_seen(99).unwrap();
        assert_eq!(all.len(), 3);

        // Limit 0: empty Vec, not an error.
        let none = s.last_seen(0).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn last_seen_roundtrips_fields() {
        let (s, _dir) = temp_storage();
        let original = listing(42, "Original Title");
        s.mark_seen(std::slice::from_ref(&original)).unwrap();

        let got = s.last_seen(1).unwrap();
        assert_eq!(got.len(), 1);
        // Same field values come back out — confirms the SELECT projection
        // matches the INSERT and our `as i64` ↔ `as u64` casts round-trip.
        assert_eq!(got[0], original);
    }

    #[test]
    fn clear_seen_wipes_everything_and_reports_count() {
        let (s, _dir) = temp_storage();
        s.mark_seen(&[listing(1, "A"), listing(2, "B"), listing(3, "C")])
            .unwrap();
        assert_eq!(s.seen_count().unwrap(), 3);

        let deleted = s.clear_seen().unwrap();
        assert_eq!(deleted, 3);
        assert_eq!(s.seen_count().unwrap(), 0);

        // Idempotent: a second clear is a no-op (0 deleted, no error).
        assert_eq!(s.clear_seen().unwrap(), 0);

        // Critically: runtime_settings is NOT touched. /clear is supposed to
        // wipe the dedup set only — not the user's `paused` / `interval`
        // preferences.
        s.set_setting("paused", "true").unwrap();
        s.clear_seen().unwrap();
        assert_eq!(s.get_setting("paused").unwrap().as_deref(), Some("true"));
    }

    #[test]
    fn settings_round_trip() {
        let (s, _dir) = temp_storage();
        assert_eq!(s.get_setting("never_set").unwrap(), None);

        s.set_setting("paused", "true").unwrap();
        assert_eq!(s.get_setting("paused").unwrap().as_deref(), Some("true"));

        // Overwrite must replace the existing value (not duplicate-key error).
        s.set_setting("paused", "false").unwrap();
        assert_eq!(s.get_setting("paused").unwrap().as_deref(), Some("false"));

        // Different keys are independent.
        s.set_setting("poll_interval_secs", "300").unwrap();
        assert_eq!(s.get_setting("paused").unwrap().as_deref(), Some("false"));
        assert_eq!(
            s.get_setting("poll_interval_secs").unwrap().as_deref(),
            Some("300")
        );
    }

    #[test]
    fn settings_persist_across_storage_instances() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("settings.db");
        {
            let s = Storage::new(&db).unwrap();
            s.set_setting("paused", "true").unwrap();
        }
        let s = Storage::new(&db).unwrap();
        assert_eq!(s.get_setting("paused").unwrap().as_deref(), Some("true"));
    }

    #[test]
    fn persists_across_storage_instances() {
        // Closing and reopening the same DB file must preserve the dedup set —
        // otherwise the bot would re-notify on every process restart.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("persist.db");

        {
            let s = Storage::new(&db).unwrap();
            s.mark_seen(&[listing(100, "P")]).unwrap();
        } // s drops, connection closes

        let s = Storage::new(&db).unwrap();
        let u = s.unseen(&[listing(100, "P")]).unwrap();
        assert!(u.is_empty());
        assert_eq!(s.seen_count().unwrap(), 1);
    }
}
