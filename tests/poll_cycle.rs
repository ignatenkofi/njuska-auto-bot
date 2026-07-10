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

/// Derived "page 2" (#25): the same markup with every listing id shifted by
/// +50_000_000 — see the fixture's header comment. Also 14 listings.
const FIXTURE_PAGE2: &str = include_str!("fixtures/search_mini_cooper_cabrio_page2.html");

/// A structurally-plausible results page with zero listing cards — what the
/// site serves past its last page of results.
const EMPTY_PAGE: &str = "<html><body><div class=\"uk-container\"></div></body></html>";

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
/// Single-page by default — the pagination tests raise `max_search_pages`
/// explicitly.
fn test_static_cfg(dir: &std::path::Path) -> StaticConfig {
    StaticConfig {
        database_path: dir.join("test.db"),
        telegram_token: "000:test-token-never-used".into(),
        telegram_chat_id: 1,
        authorized_user_ids: vec![1],
        save_raw_html: false,
        max_search_pages: 1,
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
    _page: u32,
) -> impl std::future::Future<Output = Result<String, ScraperError>> {
    std::future::ready(Ok(FIXTURE.to_string()))
}

/// Paginated fetch seam: page 1 = the real fixture, page 2 = the derived
/// fixture, everything deeper = an empty results page. Records the pages
/// requested so tests can assert exactly which requests the cycle made.
fn paged_fetch(
    pages_fetched: Arc<Mutex<Vec<u32>>>,
) -> impl Fn(SearchFilter, u32) -> std::future::Ready<Result<String, ScraperError>> {
    move |_filter, page| {
        pages_fetched.lock().unwrap().push(page);
        let body = match page {
            1 => FIXTURE,
            2 => FIXTURE_PAGE2,
            _ => EMPTY_PAGE,
        };
        std::future::ready(Ok(body.to_string()))
    }
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

// `start_paused`: tokio's paused clock auto-advances through the between-page
// politeness sleep, so multi-page cycles run instantly.
#[tokio::test(start_paused = true)]
async fn two_page_burst_sends_both_pages_and_stops_on_the_empty_third() {
    let dir = tempfile::tempdir().unwrap();
    let mut static_cfg = test_static_cfg(dir.path());
    static_cfg.max_search_pages = 3;
    let runtime = test_runtime();
    let storage = Storage::new(&static_cfg.database_path).unwrap();
    let mut state = LoopState::default();

    let pages_fetched = Arc::new(Mutex::new(Vec::new()));
    let collector = Collector::new();
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &collector,
        &mut state,
        &paged_fetch(pages_fetched.clone()),
    )
    .await
    .unwrap();

    // Page 2 was all-new, so the cycle went on to page 3; the empty page 3
    // stopped it (and never counted toward anything).
    assert_eq!(*pages_fetched.lock().unwrap(), vec![1, 2, 3]);
    assert_eq!(collector.sent().len(), 2 * FIXTURE_LISTINGS);
    assert_eq!(storage.seen_count().unwrap(), 2 * FIXTURE_LISTINGS as u64);
    // A known page-2 listing (derived id = 27312553 + 50M) really went out.
    assert!(
        collector.sent().iter().any(|m| m.contains("77312553")),
        "page-2 listing must be sent"
    );
}

#[tokio::test(start_paused = true)]
async fn pagination_stops_at_the_first_page_with_nothing_unseen() {
    let dir = tempfile::tempdir().unwrap();
    let mut static_cfg = test_static_cfg(dir.path());
    static_cfg.max_search_pages = 3;
    let runtime = test_runtime();
    let storage = Storage::new(&static_cfg.database_path).unwrap();
    let mut state = LoopState::default();

    // Page-2 listings are already known (say, sent during an earlier deep
    // cycle). Page 1 is all-new; page 2 must end the pagination.
    let page2_listings = njuska_auto_bot::scraper::parse_listings(FIXTURE_PAGE2);
    storage.mark_seen(&page2_listings).unwrap();

    let pages_fetched = Arc::new(Mutex::new(Vec::new()));
    let collector = Collector::new();
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &collector,
        &mut state,
        &paged_fetch(pages_fetched.clone()),
    )
    .await
    .unwrap();

    assert_eq!(
        *pages_fetched.lock().unwrap(),
        vec![1, 2],
        "a fully-seen page must stop pagination before the cap"
    );
    assert_eq!(collector.sent().len(), FIXTURE_LISTINGS, "page 1 only");
    assert_eq!(storage.seen_count().unwrap(), 2 * FIXTURE_LISTINGS as u64);
}

#[tokio::test]
async fn max_pages_one_keeps_the_single_page_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let static_cfg = test_static_cfg(dir.path()); // max_search_pages: 1
    let runtime = test_runtime();
    let storage = Storage::new(&static_cfg.database_path).unwrap();
    let mut state = LoopState::default();

    // With MAX_SEARCH_PAGES=1 the cycle must never ask for page 2, even
    // though page 1 came back entirely new (which would trigger a deeper
    // fetch in multi-page mode).
    let fetch = |_f: SearchFilter, page: u32| {
        assert_eq!(page, 1, "single-page mode must only ever fetch page 1");
        std::future::ready(Ok::<String, ScraperError>(FIXTURE.to_string()))
    };

    let collector = Collector::new();
    bot::run_one_cycle(
        &static_cfg,
        &runtime,
        &storage,
        &collector,
        &mut state,
        &fetch,
    )
    .await
    .unwrap();

    assert_eq!(collector.sent().len(), FIXTURE_LISTINGS);
    assert_eq!(storage.seen_count().unwrap(), FIXTURE_LISTINGS as u64);
}

#[tokio::test(start_paused = true)]
async fn zero_listings_across_all_pages_counts_as_one_zero_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let mut static_cfg = test_static_cfg(dir.path());
    static_cfg.max_search_pages = 2;
    let runtime = test_runtime();
    let storage = Storage::new(&static_cfg.database_path).unwrap();
    let mut state = LoopState::default();

    // An empty page 1 ends pagination immediately — deeper pages can't
    // contain results either — and the whole cycle counts as ONE zero-result
    // increment, so three cycles hit the threshold of 3 exactly once.
    let pages_fetched = Arc::new(Mutex::new(Vec::new()));
    let fetch = {
        let pages_fetched = pages_fetched.clone();
        move |_f: SearchFilter, page: u32| {
            pages_fetched.lock().unwrap().push(page);
            std::future::ready(Ok::<String, ScraperError>(EMPTY_PAGE.to_string()))
        }
    };

    let collector = Collector::new();
    for _ in 0..3 {
        bot::run_one_cycle(
            &static_cfg,
            &runtime,
            &storage,
            &collector,
            &mut state,
            &fetch,
        )
        .await
        .unwrap();
    }

    assert_eq!(*pages_fetched.lock().unwrap(), vec![1, 1, 1]);
    let sent = collector.sent();
    assert_eq!(
        sent.len(),
        1,
        "exactly one alert at the threshold: {sent:?}"
    );
    assert!(sent[0].contains("0 listings"), "{}", sent[0]);
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
    let must_not_fetch = |_f: SearchFilter, _page: u32| async {
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
