//! End-to-end poll-cycle integration test (#22).
//!
//! Runs the *real* `bot::run_one_cycle` — dedup, mark-after-send, cycle
//! bookkeeping — with the two injected seams: a fetch closure that serves
//! the saved HTML fixture (no network, per project test rules) and a
//! collecting [`Notifier`] instead of live Telegram. Storage is a temp
//! SQLite file that dies with the tempdir.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::RwLock;

use njuska_auto_bot::bot::{self, LoopState};
use njuska_auto_bot::config::{RuntimeConfig, StaticConfig};
use njuska_auto_bot::models::SearchFilter;
use njuska_auto_bot::scraper::ScraperError;
use njuska_auto_bot::storage::Storage;
use njuska_auto_bot::telegram::{Notifier, TelegramError};

/// The same fixture the scraper unit tests use: 14 listings.
const FIXTURE: &str = include_str!("fixtures/search_mini_cooper_cabrio.html");
const FIXTURE_LISTINGS: usize = 14;

/// Test double for the outbound-send seam: records every message; optionally
/// fails (permanently, so `send_batch` doesn't retry/sleep) any message
/// containing a marker substring — that's how the mark-after-send test
/// knocks out one specific listing.
struct Collector {
    sent: Mutex<Vec<String>>,
    fail_when_contains: Option<String>,
}

impl Collector {
    fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            fail_when_contains: None,
        }
    }

    fn failing_on(marker: &str) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            fail_when_contains: Some(marker.to_string()),
        }
    }

    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl Notifier for Collector {
    fn send_html(
        &self,
        html: &str,
    ) -> impl std::future::Future<Output = Result<(), TelegramError>> + Send {
        let res = if self
            .fail_when_contains
            .as_deref()
            .is_some_and(|marker| html.contains(marker))
        {
            // Permanent (an Api error): send_batch must NOT retry it and
            // must NOT include it in the "sent" result.
            Err(TelegramError::Permanent(teloxide::RequestError::Api(
                teloxide::ApiError::CantParseEntities("injected test failure".into()),
            )))
        } else {
            self.sent.lock().unwrap().push(html.to_string());
            Ok(())
        };
        std::future::ready(res)
    }
}

/// A StaticConfig that touches nothing outside the tempdir. Dump saving is
/// off so the cycle exercises fetch -> parse -> dedup -> send only.
fn test_static_cfg(dir: &std::path::Path) -> StaticConfig {
    StaticConfig {
        database_path: dir.join("test.db"),
        telegram_token: "000:test-token-never-used".into(),
        telegram_chat_id: 1,
        authorized_user_id: 1,
        save_raw_html: false,
        zero_results_alert_threshold: 3,
        fetch_errors_alert_threshold: 3,
        dumps_dir: dir.join("dumps"),
        dump_retention_days: 0,
        dump_max_total_mb: 0,
        seen_retention_days: 0,
        cf_proxy: None,
    }
}

fn test_runtime() -> Arc<RwLock<RuntimeConfig>> {
    Arc::new(RwLock::new(RuntimeConfig {
        search: SearchFilter::default(),
        poll_interval: Duration::from_secs(600),
        paused: false,
    }))
}

/// Fetch seam serving the fixture. Every "request" succeeds with the same
/// page — exactly what two consecutive polls against an unchanged site see.
fn fixture_fetch(
    _filter: SearchFilter,
) -> impl std::future::Future<Output = Result<String, ScraperError>> {
    std::future::ready(Ok(FIXTURE.to_string()))
}

#[tokio::test]
async fn first_cycle_sends_everything_second_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let static_cfg = test_static_cfg(dir.path());
    let runtime = test_runtime();
    let storage = Storage::new(&static_cfg.database_path).unwrap();
    let mut state = LoopState::default();

    // Cycle 1: empty DB, so all 14 fixture listings are new.
    let collector = Collector::new();
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &collector,
        &mut state,
        &fixture_fetch,
    )
    .await
    .unwrap();

    let sent = collector.sent();
    assert_eq!(sent.len(), FIXTURE_LISTINGS, "all listings sent first time");
    assert_eq!(storage.seen_count().unwrap(), FIXTURE_LISTINGS as u64);
    // Spot-check the payload is the real formatted message, not junk.
    assert!(
        sent.iter().any(|m| m.contains("MINI Cooper 1.6d CaBRiO")),
        "known listing title must appear in some message"
    );

    // Cycle 2: same page again — dedup must suppress everything.
    let collector2 = Collector::new();
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &collector2,
        &mut state,
        &fixture_fetch,
    )
    .await
    .unwrap();

    assert!(
        collector2.sent().is_empty(),
        "second cycle must send nothing"
    );
    assert_eq!(storage.seen_count().unwrap(), FIXTURE_LISTINGS as u64);
}

#[tokio::test]
async fn failed_send_is_not_marked_seen_and_retries_next_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let static_cfg = test_static_cfg(dir.path());
    let runtime = test_runtime();
    let storage = Storage::new(&static_cfg.database_path).unwrap();
    let mut state = LoopState::default();

    // Knock out one known listing by its ID (present in the message URL).
    const VICTIM_ID: &str = "27312553";
    let collector = Collector::failing_on(VICTIM_ID);
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &collector,
        &mut state,
        &fixture_fetch,
    )
    .await
    .unwrap();

    // Mark-after-send: the failed listing must stay OUT of seen_listings.
    assert_eq!(collector.sent().len(), FIXTURE_LISTINGS - 1);
    assert_eq!(storage.seen_count().unwrap(), (FIXTURE_LISTINGS - 1) as u64);

    // Next cycle with a healthy notifier: exactly the victim is re-sent.
    let recovered = Collector::new();
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &recovered,
        &mut state,
        &fixture_fetch,
    )
    .await
    .unwrap();

    let resent = recovered.sent();
    assert_eq!(
        resent.len(),
        1,
        "only the previously-failed listing resends"
    );
    assert!(resent[0].contains(VICTIM_ID), "{}", resent[0]);
    assert_eq!(storage.seen_count().unwrap(), FIXTURE_LISTINGS as u64);
}

#[tokio::test]
async fn paused_cycle_fetches_and_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let static_cfg = test_static_cfg(dir.path());
    let runtime = test_runtime();
    runtime.write().await.paused = true;
    let storage = Storage::new(&static_cfg.database_path).unwrap();
    let mut state = LoopState::default();

    // A fetch seam that panics if called — paused must short-circuit first.
    let must_not_fetch = |_f: SearchFilter| async {
        panic!("paused cycle must not fetch");
        #[allow(unreachable_code)]
        Ok::<String, ScraperError>(String::new())
    };

    let collector = Collector::new();
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &collector,
        &mut state,
        &must_not_fetch,
    )
    .await
    .unwrap();

    assert!(collector.sent().is_empty());
    assert_eq!(storage.seen_count().unwrap(), 0);
}
