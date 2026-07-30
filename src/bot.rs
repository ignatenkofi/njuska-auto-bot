//! The polling loop: orchestrates fetch -> dump -> parse -> dedup -> send,
//! once every `config.poll_interval`, until `Ctrl+C`.
//!
//! Design notes:
//!
//! * Errors inside a cycle are **logged, not propagated** — one bad cycle
//!   never kills the loop. That's the whole reason the bot exists as a
//!   long-running service rather than a cron job.
//! * Two separate failure modes drive observability:
//!   * Network/process errors (fetch failed, transaction failed, send failed)
//!     are surfaced via `error!`/`warn!` in the cycle and forgotten.
//!   * **Zero-listings-N-times-in-a-row** is treated specially — that's the
//!     signature of "site changed, parser broken", which won't fix itself.
//!     We send a one-off Telegram alert per streak, then go quiet until the
//!     streak breaks.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, error, info, warn};

use crate::config::{RuntimeConfig, StaticConfig};
use crate::models::{Listing, SearchFilter};
use crate::scraper;
use crate::signals::shutdown_signal;
use crate::storage::{SavedFilter, Storage};
use crate::telegram::{self, Notifier, TelegramClient, TelegramError};

/// Pause between same-cycle page fetches (#25): deeper pages are extra load
/// we impose within one cycle, so space them out like a human paging through
/// results would instead of bursting requests back-to-back.
const PAGE_FETCH_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// State that has to survive across cycles. Kept as a separate struct so we
/// can hand it to a pure `update_streak` helper that's testable in isolation.
#[derive(Default)]
pub struct LoopState {
    /// Consecutive cycles where `parse_listings` returned 0.
    zero_streak: u32,
    /// True once we've fired a TG alert for the current streak. Reset when
    /// the streak breaks. Prevents one outage from flooding the chat.
    streak_alerted: bool,
    /// Consecutive cycles where `fetch_search` itself failed (network, CF
    /// 403, curl error). Parallel to `zero_streak` but one step earlier in
    /// the pipeline — a broken *fetch* also deserves an alert (#12), and it
    /// stretches the effective sleep (see [`effective_sleep`]).
    fetch_error_streak: u32,
    /// Same one-alert-per-streak latch as `streak_alerted`.
    fetch_error_alerted: bool,
    /// Day the last DB maintenance (retention prune + VACUUM) ran, so it
    /// happens once per calendar day regardless of poll cadence (#17).
    /// `None` on startup — the first cycle always runs maintenance.
    last_maintenance: Option<NaiveDate>,
}

/// One thing a cycle polls (#10, stage 2).
///
/// The legacy target keeps the pre-#10 bot byte-for-byte: until the /filter
/// wizard (stage 3) ships nobody can create `filters` rows, so an empty
/// table must mean "exactly yesterday's behavior". An enum with two
/// data-carrying variants (rather than an `Option<i64>` threaded around)
/// lets `match` force every call site to handle both dedup scopes — the
/// compiler, not review, guards against mixing them up.
enum PollTarget {
    /// `RuntimeConfig.search` with the global `seen_listings` dedup.
    Legacy(SearchFilter),
    /// A `filters` row: filter-scoped dedup, `[name]`-tagged messages.
    Saved(SavedFilter),
}

impl PollTarget {
    fn search(&self) -> &SearchFilter {
        match self {
            PollTarget::Legacy(f) => f,
            PollTarget::Saved(s) => &s.filter,
        }
    }

    /// Saved-filter name for message tags; the legacy target has none.
    fn name(&self) -> Option<&str> {
        match self {
            PollTarget::Legacy(_) => None,
            PollTarget::Saved(s) => Some(s.name.as_str()),
        }
    }

    /// Marker for dump filenames; `None` keeps the legacy layout.
    fn dump_id(&self) -> Option<i64> {
        match self {
            PollTarget::Legacy(_) => None,
            PollTarget::Saved(s) => Some(s.id),
        }
    }
}

/// What [`poll_one_target`] reports back to the cycle loop.
enum TargetOutcome {
    /// All pages processed; `parsed` feeds the cycle-level zero-streak.
    Completed { parsed: usize },
    /// The page-1 fetch failed: the site (or proxy) is unreachable and the
    /// fetch-error streak is already updated. Polling the remaining targets
    /// would hammer a host that just refused us — the cycle stops here.
    FetchAborted,
}

/// The main entrypoint. Returns when Ctrl+C / SIGTERM is received (or never,
/// if the process is sent SIGKILL — which we cannot intercept).
///
/// All shared state arrives via `Arc<…>` so the same handles can live in the
/// command dispatcher task too (see [`crate::commands::run_command_loop`]).
/// `TelegramClient` derives `Clone` and is itself cheap to clone (the inner
/// `Bot` is `Arc`-internal), so no extra wrapper there.
///
/// `runtime_changed` is a [`Notify`] handle shared with the command loop.
/// When `/interval N` updates `runtime.poll_interval`, it calls
/// `runtime_changed.notify_one()`, which wakes this loop out of its sleep
/// so the new (possibly shorter) interval takes effect immediately. Without
/// that nudge, a `/interval 60` issued during a 10-minute sleep would not
/// be observed for up to 10 minutes — confusing UX.
///
/// ## Why sleep-loop, not `tokio::time::interval`?
///
/// v1 used `tokio::time::interval` which is great for *fixed* cadences.
/// Once we made the interval runtime-mutable, the cleaner pattern is
/// `loop { cycle; sleep current_interval }` — every iteration re-reads
/// the interval, no rebuild ceremony. MissedTickBehavior::Delay's semantics
/// fall out for free (next sleep starts *after* the cycle ends).
pub async fn run(
    static_cfg: Arc<StaticConfig>,
    runtime: Arc<RwLock<RuntimeConfig>>,
    storage: Arc<Storage>,
    telegram: TelegramClient,
    runtime_changed: Arc<Notify>,
) -> Result<()> {
    let mut state = LoopState::default();

    // Read the field OUTSIDE the `info!` macro. The macro internally builds
    // `format_args!` which is `!Send`; if an `.await` happens inside the macro
    // call, the `Arguments` value is held across the await and the whole
    // future loses `Send`. Pulling the read out keeps the future Send-safe.
    let starting_interval_secs = runtime.read().await.poll_interval.as_secs();
    info!(
        interval_secs = starting_interval_secs,
        "poll loop started; send SIGINT (Ctrl+C) or SIGTERM to stop"
    );

    // Production fetch seam: the real scraper, routed through the proxy when
    // configured. Takes the filter by value (and clones the proxy config —
    // a Url + String, pennies) so the returned future owns everything it
    // touches; borrowing through a generic `Fn` would force higher-ranked
    // lifetimes on every caller for zero benefit.
    let cf_proxy = static_cfg.cf_proxy.clone();
    let fetch = move |filter: crate::models::SearchFilter, page: u32| {
        let proxy = cf_proxy.clone();
        async move { scraper::fetch_search(&filter, proxy.as_ref(), page).await }
    };

    loop {
        // Run a cycle first — so we poll immediately on bot startup, not
        // after waiting an interval.
        if let Err(e) = run_one_cycle(
            &static_cfg,
            &runtime,
            &storage,
            &telegram,
            &mut state,
            &fetch,
        )
        .await
        {
            // The cycle's own error path is `warn!` / `error!`; this catches
            // `?` propagations from places we couldn't recover. The loop
            // continues regardless.
            error!(error = ?e, "poll cycle returned an error");
        }

        // Sleep until: the configured interval elapses, OR /interval changes
        // the interval, OR a shutdown signal arrives. Whichever comes first.
        // A growing fetch-error streak stretches the sleep (capped at 4x) so
        // we don't hammer a site or proxy that's actively rejecting us.
        let base_interval = runtime.read().await.poll_interval;
        let sleep_dur = effective_sleep(base_interval, state.fetch_error_streak);
        if sleep_dur > base_interval {
            warn!(
                streak = state.fetch_error_streak,
                sleep_secs = sleep_dur.as_secs(),
                "stretching sleep due to fetch-error streak"
            );
        }
        tokio::select! {
            _ = tokio::time::sleep(sleep_dur) => {}
            _ = runtime_changed.notified() => {
                info!("interval changed; cycling now");
            }
            _ = shutdown_signal() => {
                return Ok(());
            }
        }
    }
}

/// One iteration of the poll loop. Returning `Err` is logged in [`run`] —
/// it does **not** stop the loop. Most error paths inside this function
/// instead log at `warn!`/`error!` and continue, so the cycle completes
/// even when one sub-step fails.
///
/// `pub` with two injected seams (#22) so `tests/poll_cycle.rs` can run the
/// real cycle — dedup, mark-after-send, streak bookkeeping — against a
/// fixture and a collector instead of live HTTP:
///
/// * `fetch` — anything `(SearchFilter, page) -> Future<Result<String,
///   ScraperError>>`. Production passes a closure over
///   [`scraper::fetch_search`]; tests serve per-page fixtures.
/// * `notifier` — the outbound-send seam ([`Notifier`]); production is the
///   real [`TelegramClient`].
pub async fn run_one_cycle<N, F, Fut>(
    static_cfg: &StaticConfig,
    runtime: &Arc<RwLock<RuntimeConfig>>,
    storage: &Storage,
    notifier: &N,
    state: &mut LoopState,
    fetch: &F,
) -> Result<()>
where
    N: Notifier,
    F: Fn(crate::models::SearchFilter, u32) -> Fut,
    Fut: std::future::Future<Output = Result<String, scraper::ScraperError>>,
{
    // DB maintenance first, *before* the pause check — a bot paused for a
    // month should still honor the retention policy.
    maybe_run_daily_maintenance(static_cfg, storage, state);

    // Snapshot what we need under a brief read lock. We deliberately don't
    // hold the lock across any `.await` — the cycle takes ~1s incl. HTTP,
    // and a writer (a `/pause` command) blocked for 1s on every cycle would
    // be noticeable. Cloning `SearchFilter` is cheap (a few Strings/Vecs).
    let (paused, search) = {
        let r = runtime.read().await;
        (r.paused, r.search.clone())
    };

    if paused {
        debug!("poll cycle skipped: bot is paused");
        return Ok(());
    }

    // Which filters this cycle polls (#10, stage 2). An empty `filters`
    // table means the pre-#10 single-filter bot: `RuntimeConfig.search`
    // against the global dedup. A non-empty table takes over completely —
    // enabled rows only, each in its own dedup scope. Deliberately no mixed
    // mode: mixing the global and per-filter seen-sets in one cycle would
    // make "why did/didn't it notify?" unanswerable.
    let saved = storage.list_filters().context("loading saved filters")?;
    let targets: Vec<PollTarget> = if saved.is_empty() {
        vec![PollTarget::Legacy(search)]
    } else {
        saved
            .into_iter()
            .filter(|f| f.enabled)
            .map(PollTarget::Saved)
            .collect()
    };
    if targets.is_empty() {
        // Every saved filter is disabled — an explicit user choice, not a
        // parser problem: nothing is fetched and the zero-streak stays put,
        // same stance as `paused`.
        debug!("all saved filters disabled; nothing to poll");
        return Ok(());
    }

    let mut total_parsed = 0usize;
    for (i, target) in targets.iter().enumerate() {
        if i > 0 {
            // Same politeness pause between filters as between pages — from
            // the site's side both are just "one more request from us".
            tokio::time::sleep(PAGE_FETCH_DELAY).await;
        }
        match poll_one_target(static_cfg, storage, notifier, state, fetch, target).await? {
            TargetOutcome::Completed { parsed } => total_parsed += parsed,
            TargetOutcome::FetchAborted => return Ok(()),
        }
    }

    // Housekeeping once per cycle, after all filters' pages are on disk.
    if static_cfg.save_raw_html {
        if static_cfg.dump_retention_days > 0
            && let Err(e) =
                rotate_dumps(&static_cfg.dumps_dir, static_cfg.dump_retention_days).await
        {
            warn!(error = %e, "couldn't rotate old HTML dumps");
        }
        // Size cap runs *after* date rotation so it only has to clean up
        // what retention left behind (#16).
        if static_cfg.dump_max_total_mb > 0
            && let Err(e) = enforce_dump_size_cap(
                &static_cfg.dumps_dir,
                static_cfg.dump_max_total_mb * 1024 * 1024,
            )
            .await
        {
            warn!(error = %e, "couldn't enforce dump size cap");
        }
    }

    // Update the zero-streak state and *maybe* fire an alert. "Zero" means
    // zero across ALL filters and pages this cycle — one narrow filter
    // returning nothing is normal life, every filter returning nothing is
    // the same "site probably changed" signal as in the single-filter era.
    if let Some(alert) = update_streak(
        state,
        total_parsed == 0,
        static_cfg.zero_results_alert_threshold,
    ) {
        if let Err(e) = notifier.send_html(&alert).await {
            warn!(error = %e, "couldn't send zero-streak alert");
        } else {
            state.streak_alerted = true;
        }
    }
    Ok(())
}

/// Polls one target: fetch pages → dump → parse → dedup (in the target's
/// scope) → send → mark. This is the old single-filter cycle body, extracted
/// so the multi-filter loop (#10, stage 2) stays readable; dedup and
/// mark-seen dispatch on the target so the two scopes can't be mixed up.
async fn poll_one_target<N, F, Fut>(
    static_cfg: &StaticConfig,
    storage: &Storage,
    notifier: &N,
    state: &mut LoopState,
    fetch: &F,
    target: &PollTarget,
) -> Result<TargetOutcome>
where
    N: Notifier,
    F: Fn(SearchFilter, u32) -> Fut,
    Fut: std::future::Future<Output = Result<String, scraper::ScraperError>>,
{
    // Tracing's `%field` wants a Display value and `Option<&str>` isn't one;
    // a plain default keeps every log line greppable by filter.
    let label = target.name().unwrap_or("config");

    // --- Fetch pages, newest first, until nothing new shows up (#25) ---
    //
    // The pagination loop lives here rather than in `scraper` on purpose:
    // the early-stop rule needs storage ("does this page still contain
    // unseen ids?"), and pushing that into the scraper would couple it to
    // the database. This function already owns both halves, so it composes
    // them; the scraper stays a fetch-one-page + parse-pure pair.
    let max_pages = static_cfg.max_search_pages.max(1);
    let mut total_parsed = 0usize;
    let mut unseen: Vec<Listing> = Vec::new();
    // Promoted ads repeat at the top of every page — remember ids already
    // collected for this target so a repeat can't be sent twice. Per-target,
    // not per-cycle: the same listing genuinely goes to every filter it
    // matches (that's the point of #10), only within one filter it's a dupe.
    let mut cycle_ids: HashSet<u64> = HashSet::new();

    for page in 1..=max_pages {
        if page > 1 {
            tokio::time::sleep(PAGE_FETCH_DELAY).await;
        }

        let html = match fetch(target.search().clone(), page).await {
            Ok(h) => {
                // Fetch works again — clear the error streak and re-arm the alert.
                update_fetch_error_streak(state, false, static_cfg.fetch_errors_alert_threshold);
                h
            }
            Err(e) if page > 1 => {
                // A deeper-page failure loses only the tail of this target —
                // process what earlier pages already gave us instead of
                // discarding them. The fetch-error streak stays untouched:
                // it answers "can we reach the site at all?", and page 1
                // just proved we can.
                warn!(error = %e, filter = %label, page, "page fetch failed; stopping pagination this cycle");
                break;
            }
            Err(e) => {
                // Fetch failures are usually transient (network blip) so they're
                // kept out of the zero-streak/parser detector. But a *persistent*
                // fetch failure (dead proxy, rotated CF_PROXY_SECRET, CF blocking
                // us) won't fix itself either — that gets its own streak counter
                // and a one-off Telegram alert per streak (#12).
                warn!(
                    error = %e,
                    filter = %label,
                    streak = state.fetch_error_streak + 1,
                    "fetch failed; will retry next cycle"
                );
                if update_fetch_error_streak(state, true, static_cfg.fetch_errors_alert_threshold) {
                    let alert = format!(
                        "⚠️ <b>NjuskaAutoBot alert</b>\n\n\
                         Fetching the search page failed <b>{}</b> times in a row.\n\
                         Last error: {}",
                        state.fetch_error_streak,
                        telegram::escape_html(&describe_fetch_error(
                            &e,
                            static_cfg.cf_proxy.is_some()
                        )),
                    );
                    if let Err(send_err) = notifier.send_html(&alert).await {
                        warn!(error = %send_err, "couldn't send fetch-error alert");
                    } else {
                        state.fetch_error_alerted = true;
                    }
                }
                return Ok(TargetOutcome::FetchAborted);
            }
        };

        if static_cfg.save_raw_html
            && let Err(e) =
                save_html_dump(&static_cfg.dumps_dir, &html, page, target.dump_id()).await
        {
            // Dump-on-disk is a debugging convenience; failing to write to disk
            // shouldn't fail the bot. Log and carry on.
            warn!(error = %e, "couldn't write HTML dump");
        }

        let listings = scraper::parse_listings(&html);
        debug!(filter = %label, page, parsed = listings.len(), "page parsed");
        total_parsed += listings.len();
        if listings.is_empty() {
            // Either past the last page of results, or the site changed under
            // us — the zero-streak detector in the caller judges the total.
            break;
        }

        let fresh: Vec<Listing> = listings
            .into_iter()
            .filter(|l| cycle_ids.insert(l.id))
            .collect();
        let page_unseen = match target {
            PollTarget::Legacy(_) => storage.unseen(&fresh).context("dedup")?,
            PollTarget::Saved(f) => storage
                .unseen_for_filter(f.id, &fresh)
                .with_context(|| format!("filter-scoped dedup ({})", f.name))?,
        };
        let page_exhausted = page_unseen.is_empty();
        unseen.extend(page_unseen);
        if page_exhausted {
            // Sort is pinned newest-first, so a page with nothing unseen means
            // deeper pages are older still — nothing new lives past here. In
            // steady state this fires on page 1: one request per filter, the
            // same traffic profile as before pagination existed.
            break;
        }
    }

    // Mark-after-send: `unseen` was identified read-only above; now send,
    // THEN persist only those that actually got through. A network failure
    // mid-batch leaves the unsent ones in the unseen set so the next cycle
    // picks them up. The previous "filter_new" (mark-then-send) lost them.
    info!(filter = %label, total = total_parsed, unseen = unseen.len(), "filter parsed");

    let sent = send_batch(notifier, &unseen, target.name()).await;
    let attempted = unseen.len();
    let sent_count = sent.len();
    match target {
        PollTarget::Legacy(_) => storage
            .mark_seen(&sent)
            .context("marking successfully-sent listings")?,
        PollTarget::Saved(f) => storage
            .mark_seen_for_filter(f.id, &sent)
            .with_context(|| format!("marking sent listings for filter {}", f.name))?,
    }
    info!(
        filter = %label,
        attempted,
        sent = sent_count,
        failed = attempted - sent_count,
        "filter done"
    );
    Ok(TargetOutcome::Completed {
        parsed: total_parsed,
    })
}

/// Pure function: mutates `state` and decides whether to fire an alert *now*.
/// Returning `Some(text)` means "send this message"; the caller does the
/// actual send and flips `streak_alerted` on success.
///
/// Pulled out specifically so we can unit-test the state machine without
/// involving HTTP, SQLite or tokio.
fn update_streak(state: &mut LoopState, listings_empty: bool, threshold: u32) -> Option<String> {
    if listings_empty {
        state.zero_streak += 1;
        warn!(streak = state.zero_streak, "parsed 0 listings");
        if state.zero_streak >= threshold && !state.streak_alerted {
            return Some(format!(
                "⚠️ <b>NjuskaAutoBot alert</b>\n\n\
                 Got 0 listings <b>{}</b> times in a row.\n\
                 The site's HTML structure may have changed — the parser is \
                 likely broken. Check the dumps in <code>./dumps/</code> and \
                 the selectors in <code>src/scraper.rs</code>.",
                state.zero_streak
            ));
        }
        None
    } else {
        // Streak broke. Reset both counters and log if we were in a streak.
        if state.zero_streak > 0 {
            info!(
                previous_streak = state.zero_streak,
                "listings recovered; streak cleared"
            );
        }
        state.zero_streak = 0;
        state.streak_alerted = false;
        None
    }
}

/// Pure state machine for the fetch-error streak, mirroring [`update_streak`].
/// Returns `true` when the caller should fire the alert *now* (threshold hit
/// and not yet alerted for this streak). The caller flips
/// `fetch_error_alerted` after a successful send — same contract as the
/// zero-streak path.
fn update_fetch_error_streak(state: &mut LoopState, fetch_failed: bool, threshold: u32) -> bool {
    if fetch_failed {
        state.fetch_error_streak += 1;
        state.fetch_error_streak >= threshold && !state.fetch_error_alerted
    } else {
        if state.fetch_error_streak > 0 {
            info!(
                previous_streak = state.fetch_error_streak,
                "fetch recovered; error streak cleared"
            );
        }
        state.fetch_error_streak = 0;
        state.fetch_error_alerted = false;
        false
    }
}

/// How long to actually sleep given the configured interval and the current
/// fetch-error streak: 1x, 2x, then capped at 4x. Pure so the doubling/cap
/// policy is unit-testable without a runtime.
fn effective_sleep(base: std::time::Duration, error_streak: u32) -> std::time::Duration {
    let factor = match error_streak {
        0 => 1,
        1 => 2,
        _ => 4,
    };
    // `Duration * u32` panics on overflow, and this multiplication runs in
    // [`run`] *outside* `run_one_cycle`'s error catch — such a panic would
    // kill the loop for good (invariant 1, #54). The interval ceiling in
    // config makes overflow unreachable today; saturating keeps invariant 1
    // from depending on validation elsewhere staying airtight.
    base.saturating_mul(factor)
}

/// Human-readable one-liner for a fetch failure, used in the Telegram alert
/// and by `/diag`. The 403-through-proxy case gets a targeted hint because
/// it's almost always a rotated/mismatched `CF_PROXY_SECRET`.
pub(crate) fn describe_fetch_error(e: &scraper::ScraperError, via_proxy: bool) -> String {
    use crate::scraper::ScraperError;
    match e {
        ScraperError::Status(403) if via_proxy => "HTTP 403 through the CF Worker proxy — \
             check that CF_PROXY_SECRET matches the Worker's PROXY_SECRET \
             (wrangler secret put PROXY_SECRET)"
            .to_string(),
        ScraperError::Status(403) => "HTTP 403 — Cloudflare is challenging direct fetches; \
             consider the CF Worker proxy (see cf-proxy/README.md)"
            .to_string(),
        ScraperError::Status(s) => format!("non-success HTTP status {s}"),
        ScraperError::Curl { exit, .. } => {
            format!("curl failed with exit code {exit} (network/DNS/TLS-level error)")
        }
        ScraperError::Spawn(io) => format!("couldn't run curl: {io}"),
        other => other.to_string(),
    }
}

/// Sends each new listing to Telegram. Returns the listings that **successfully**
/// went through — caller is responsible for marking those as seen.
///
/// Listings whose send fails (network error, individual Api error) are
/// silently dropped from the returned slice — they'll re-appear in the next
/// cycle's `unseen` set and we'll retry them then.
///
/// Retry policy per error class (#15):
///
/// * **429** — sleep the server-supplied `retry_after` (clamped to 60s;
///   Telegram has been known to suggest huge values for spam-flagged bots)
///   and retry **once**. A second failure stops the batch — the next cycle
///   picks up where we left off, past any rate-limit window.
/// * **Retryable** (network/5xx/garbled response) — sleep a short fixed
///   backoff and retry **once**. A second failure also stops the batch:
///   if the network is down, every remaining send would fail too.
/// * **Permanent** (400 bad request, …) — logged at `error!` and never
///   retried; the payload won't get better by resending it.
async fn send_batch<N: Notifier>(
    notifier: &N,
    listings: &[Listing],
    filter_name: Option<&str>,
) -> Vec<Listing> {
    /// Hard ceiling on how long we'll sleep waiting out a 429.
    const MAX_BACKOFF_SECS: u64 = 60;
    /// Fixed pause before retrying a transport-level failure.
    const RETRYABLE_BACKOFF_SECS: u64 = 3;

    let mut sent: Vec<Listing> = Vec::with_capacity(listings.len());

    for l in listings {
        // Saved filters tag their messages so the user can tell which of
        // their filters matched (#10); the legacy single filter stays untagged.
        let text = match filter_name {
            Some(name) => telegram::format_listing_html_tagged(l, name),
            None => telegram::format_listing_html(l),
        };
        match notifier.send_html(&text).await {
            Ok(()) => {
                info!(id = l.id, title = %l.title, "sent");
                sent.push(l.clone());
            }
            Err(TelegramError::RateLimited { retry_after_secs }) => {
                let sleep_secs = retry_after_secs.min(MAX_BACKOFF_SECS);
                warn!(
                    id = l.id,
                    suggested = retry_after_secs,
                    sleeping = sleep_secs,
                    "rate-limited; sleeping then retrying once"
                );
                tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;

                match notifier.send_html(&text).await {
                    Ok(()) => {
                        info!(id = l.id, title = %l.title, "sent (after 429 retry)");
                        sent.push(l.clone());
                    }
                    Err(retry_err) => {
                        warn!(
                            id = l.id,
                            error = %retry_err,
                            "retry after 429 still failed; stopping batch"
                        );
                        break;
                    }
                }
            }
            Err(TelegramError::Retryable(e)) => {
                warn!(
                    id = l.id,
                    error = %e,
                    backoff_secs = RETRYABLE_BACKOFF_SECS,
                    "transient telegram send failure; retrying once"
                );
                tokio::time::sleep(std::time::Duration::from_secs(RETRYABLE_BACKOFF_SECS)).await;

                match notifier.send_html(&text).await {
                    Ok(()) => {
                        info!(id = l.id, title = %l.title, "sent (after transient-error retry)");
                        sent.push(l.clone());
                    }
                    Err(retry_err) => {
                        warn!(
                            id = l.id,
                            error = %retry_err,
                            "retry after transient error still failed; stopping batch"
                        );
                        break;
                    }
                }
            }
            Err(e @ TelegramError::Permanent(_)) => {
                // error!, not warn!: a permanent rejection (usually 400 from
                // bad HTML in the payload) is a bug in our formatting, not
                // weather. It will recur every cycle until someone looks.
                error!(id = l.id, error = %e, "permanent telegram send failure; not retrying");
            }
        }
    }
    sent
}

/// Writes one page's raw HTML under
/// `<dumps_dir>/YYYY-MM-DD/HHMMSS[-fID]-pN.html`. Creates intermediate
/// directories if needed. Day-bucketed so daily rotation is trivial (drop
/// yesterday's folder).
///
/// The `-pN` suffix keeps a multi-page cycle from overwriting itself within
/// one second; the `-fID` segment (#10, stage 2) does the same for two
/// filters fetching within the same second, and makes "which filter's page
/// broke the parser?" answerable from the filename. Lexicographic order
/// still matches chronological order across seconds (`HHMMSS-…` < next
/// second), which the size-cap sweep's oldest-first sort relies on; within
/// one second filter ids may sort out of fetch order — a sub-second slack
/// the sweep doesn't care about.
async fn save_html_dump(
    dumps_dir: &Path,
    html: &str,
    page: u32,
    filter_id: Option<i64>,
) -> std::io::Result<()> {
    let now = Local::now();
    let day_dir = dumps_dir.join(now.format("%Y-%m-%d").to_string());
    tokio::fs::create_dir_all(&day_dir).await?;
    // `map(...).unwrap_or_default()`: the `None` arm becomes "" — cheaper to
    // read than an if/else building two format strings.
    let tag = filter_id.map(|id| format!("-f{id}")).unwrap_or_default();
    let path = day_dir.join(format!("{}{tag}-p{page}.html", now.format("%H%M%S")));
    tokio::fs::write(&path, html).await?;
    debug!(path = %path.display(), "saved HTML dump");
    Ok(())
}

/// Deletes day-bucketed subfolders of `dumps_dir` older than `retention_days`.
///
/// The structure that [`save_html_dump`] writes makes this trivial: parse each
/// subfolder name as a `YYYY-MM-DD` date, compare against today, drop the
/// whole folder if it's outside the window. Folders that don't match the
/// date format are ignored — won't trip on e.g. a `.DS_Store`.
///
/// Note: at our cadence this runs ~144 times/day, scanning a few dozen entries.
/// Cost is negligible — no need to debounce or schedule separately.
async fn rotate_dumps(dumps_dir: &Path, retention_days: u32) -> std::io::Result<()> {
    // If the dumps dir doesn't exist yet (first run, nothing dumped), that's
    // not an error — nothing to rotate.
    let mut entries = match tokio::fs::read_dir(dumps_dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let cutoff = Local::now().date_naive() - chrono::Duration::days(i64::from(retention_days));

    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Ok(folder_date) = NaiveDate::parse_from_str(name_str, "%Y-%m-%d") else {
            continue; // not a date-named folder, leave alone
        };
        if folder_date < cutoff {
            tokio::fs::remove_dir_all(entry.path()).await?;
            info!(removed = %name_str, "rotated out old dump folder");
        }
    }
    Ok(())
}

/// Once-a-day DB housekeeping (#17): prune `seen_listings` rows older than
/// the retention window, then `VACUUM` so the file actually shrinks.
///
/// Synchronous on purpose — both operations are sub-millisecond at our row
/// counts (see the module docs in `storage.rs` on sync rusqlite under tokio).
/// Errors are logged, never propagated: a failed VACUUM must not cost us a
/// poll cycle. We stamp `last_maintenance` *before* doing the work so a
/// persistent failure retries tomorrow, not every 10 minutes all day.
fn maybe_run_daily_maintenance(
    static_cfg: &StaticConfig,
    storage: &Storage,
    state: &mut LoopState,
) {
    let today = Local::now().date_naive();
    if state.last_maintenance == Some(today) {
        return;
    }
    state.last_maintenance = Some(today);

    if static_cfg.seen_retention_days > 0 {
        match storage.prune_seen_older_than(static_cfg.seen_retention_days) {
            Ok(0) => debug!("retention prune: nothing to remove"),
            Ok(n) => info!(
                pruned = n,
                retention_days = static_cfg.seen_retention_days,
                "pruned old seen_listings rows"
            ),
            Err(e) => warn!(error = %e, "couldn't prune seen_listings"),
        }
    }

    if let Err(e) = storage.vacuum() {
        warn!(error = %e, "daily VACUUM failed");
    } else {
        debug!("daily VACUUM done");
    }
}

/// Keeps the total size of all HTML dumps under `max_total_bytes` by deleting
/// the **oldest files first** (#16). Ordering comes for free from the layout
/// [`save_html_dump`] writes: day folders sort by date, files inside by
/// `HHMMSS` name. Day folders emptied by the sweep are removed too.
///
/// Files-first (rather than dropping whole day folders) so a single heavy day
/// can't force deleting *today's* dumps wholesale — we trim from the back
/// until we fit.
async fn enforce_dump_size_cap(dumps_dir: &Path, max_total_bytes: u64) -> std::io::Result<()> {
    // Missing dir = nothing dumped yet = nothing to cap.
    let mut entries = match tokio::fs::read_dir(dumps_dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    // Day folders, oldest first. Non-date folders are none of our business —
    // same stance as rotate_dumps.
    let mut day_dirs: Vec<(NaiveDate, std::path::PathBuf)> = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Ok(d) = NaiveDate::parse_from_str(name_str, "%Y-%m-%d") else {
            continue;
        };
        day_dirs.push((d, entry.path()));
    }
    day_dirs.sort();

    // Every dump file with its size, oldest first.
    let mut files: Vec<(std::path::PathBuf, u64)> = Vec::new();
    let mut total: u64 = 0;
    for (_, dir) in &day_dirs {
        let mut in_dir: Vec<(std::ffi::OsString, std::path::PathBuf, u64)> = Vec::new();
        let mut rd = tokio::fs::read_dir(dir).await?;
        while let Some(f) = rd.next_entry().await? {
            if !f.file_type().await?.is_file() {
                continue;
            }
            let len = f.metadata().await?.len();
            in_dir.push((f.file_name(), f.path(), len));
        }
        // read_dir order is unspecified; HHMMSS names sort chronologically.
        in_dir.sort();
        for (_, path, len) in in_dir {
            total += len;
            files.push((path, len));
        }
    }

    if total <= max_total_bytes {
        return Ok(());
    }

    let mut freed: u64 = 0;
    let mut deleted: u32 = 0;
    for (path, len) in files {
        if total - freed <= max_total_bytes {
            break;
        }
        tokio::fs::remove_file(&path).await?;
        freed += len;
        deleted += 1;
    }

    // Sweep away day folders the deletion emptied.
    for (_, dir) in &day_dirs {
        let mut rd = tokio::fs::read_dir(dir).await?;
        if rd.next_entry().await?.is_none() {
            tokio::fs::remove_dir(dir).await?;
        }
    }

    info!(
        deleted_files = deleted,
        freed_bytes = freed,
        total_before = total,
        cap_bytes = max_total_bytes,
        "dump size cap enforced"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[test]
    fn streak_increments_on_empty_and_resets_on_non_empty() {
        let mut s = LoopState::default();

        // Two empty cycles, no alert (threshold = 3).
        assert!(update_streak(&mut s, true, 3).is_none());
        assert_eq!(s.zero_streak, 1);
        assert!(update_streak(&mut s, true, 3).is_none());
        assert_eq!(s.zero_streak, 2);

        // Streak breaks.
        assert!(update_streak(&mut s, false, 3).is_none());
        assert_eq!(s.zero_streak, 0);
    }

    #[test]
    fn streak_fires_alert_once_at_threshold() {
        let mut s = LoopState::default();

        // Threshold of 2 -> first alert at the 2nd empty cycle.
        assert!(update_streak(&mut s, true, 2).is_none());
        let alert = update_streak(&mut s, true, 2);
        assert!(alert.is_some(), "should fire at threshold");

        // Caller's responsibility, but simulate the success-side bookkeeping.
        s.streak_alerted = true;

        // Subsequent empty cycles must NOT re-alert.
        assert!(
            update_streak(&mut s, true, 2).is_none(),
            "no re-alerts during the same streak"
        );
        assert!(
            update_streak(&mut s, true, 2).is_none(),
            "no re-alerts during the same streak"
        );
    }

    #[test]
    fn alert_re_arms_after_streak_breaks() {
        let mut s = LoopState::default();

        // Hit the threshold, alert, mark sent.
        let _ = update_streak(&mut s, true, 1);
        s.streak_alerted = true;
        assert!(update_streak(&mut s, true, 1).is_none()); // suppressed

        // Recovery clears the flags.
        update_streak(&mut s, false, 1);
        assert_eq!(s.zero_streak, 0);
        assert!(!s.streak_alerted);

        // A NEW streak gets a fresh alert.
        let next_alert = update_streak(&mut s, true, 1);
        assert!(next_alert.is_some(), "next streak should re-alert");
    }

    #[test]
    fn fetch_error_streak_alerts_once_at_threshold_and_resets_on_success() {
        let mut s = LoopState::default();

        // Below threshold: no alert.
        assert!(!update_fetch_error_streak(&mut s, true, 3));
        assert!(!update_fetch_error_streak(&mut s, true, 3));
        // At threshold: alert requested.
        assert!(update_fetch_error_streak(&mut s, true, 3));
        assert_eq!(s.fetch_error_streak, 3);

        // Simulate the caller's success-side bookkeeping after sending.
        s.fetch_error_alerted = true;
        // Same streak: no re-alerts.
        assert!(!update_fetch_error_streak(&mut s, true, 3));

        // Success resets streak and re-arms.
        assert!(!update_fetch_error_streak(&mut s, false, 3));
        assert_eq!(s.fetch_error_streak, 0);
        assert!(!s.fetch_error_alerted);

        // A fresh streak alerts again once it reaches the threshold.
        assert!(!update_fetch_error_streak(&mut s, true, 3));
        assert!(!update_fetch_error_streak(&mut s, true, 3));
        assert!(update_fetch_error_streak(&mut s, true, 3));
    }

    #[test]
    fn effective_sleep_doubles_then_caps_at_4x() {
        let base = std::time::Duration::from_secs(600);
        assert_eq!(effective_sleep(base, 0), base);
        assert_eq!(effective_sleep(base, 1), base * 2);
        assert_eq!(effective_sleep(base, 2), base * 4);
        assert_eq!(effective_sleep(base, 3), base * 4, "capped at 4x");
        assert_eq!(effective_sleep(base, 100), base * 4, "capped at 4x");
    }

    #[test]
    fn effective_sleep_saturates_on_absurd_base_instead_of_panicking() {
        // #54 / invariant 1: `Duration * u32` panics on overflow and this
        // runs outside run_one_cycle's error catch, so an absurd interval
        // must saturate, not take the loop down.
        let base = std::time::Duration::from_secs(u64::MAX);
        assert_eq!(effective_sleep(base, 0), base);
        for streak in [1, 2, 3, 100] {
            assert_eq!(effective_sleep(base, streak), std::time::Duration::MAX);
        }
    }

    #[test]
    fn describe_fetch_error_hints_at_proxy_secret_on_403_via_proxy() {
        use crate::scraper::ScraperError;

        let with_proxy = describe_fetch_error(&ScraperError::Status(403), true);
        assert!(with_proxy.contains("CF_PROXY_SECRET"), "{with_proxy}");

        let without_proxy = describe_fetch_error(&ScraperError::Status(403), false);
        assert!(
            !without_proxy.contains("CF_PROXY_SECRET"),
            "{without_proxy}"
        );
        assert!(without_proxy.contains("403"), "{without_proxy}");

        let plain = describe_fetch_error(&ScraperError::Status(500), true);
        assert!(plain.contains("500"), "{plain}");
    }

    #[tokio::test]
    async fn rotate_dumps_drops_old_folders_and_keeps_recent_and_unparseable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Date-named folders relative to today. Using `Local::now()` to match
        // what `rotate_dumps` uses internally — otherwise a UTC vs local
        // mismatch around midnight could flake the test.
        let today = Local::now().date_naive();
        let mk = |offset_days: i64| {
            let d = today - chrono::Duration::days(offset_days);
            let p = root.join(d.format("%Y-%m-%d").to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("dummy.html"), "x").unwrap();
            p
        };
        let recent_today = mk(0);
        let recent_yesterday = mk(1);
        let old_10_days = mk(10);
        let old_30_days = mk(30);

        // A non-date folder must be left strictly alone.
        let other = root.join("notes");
        std::fs::create_dir_all(&other).unwrap();

        rotate_dumps(root, 7).await.unwrap();

        assert!(recent_today.exists(), "today must survive");
        assert!(recent_yesterday.exists(), "yesterday must survive");
        assert!(!old_10_days.exists(), "10-days-old must be gone");
        assert!(!old_30_days.exists(), "30-days-old must be gone");
        assert!(other.exists(), "non-date folder must be left alone");
    }

    #[tokio::test]
    async fn rotate_dumps_is_quiet_when_dir_doesnt_exist() {
        // First-ever run: dumps dir not yet created. rotate must not error.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never_existed");
        rotate_dumps(&missing, 7).await.unwrap();
    }

    #[tokio::test]
    async fn size_cap_deletes_oldest_files_first_and_sweeps_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Three day folders, two 100-byte files each; oldest day first.
        let mk = |day: &str, file: &str| {
            let d = root.join(day);
            std::fs::create_dir_all(&d).unwrap();
            let p = d.join(file);
            std::fs::write(&p, [b'x'; 100]).unwrap();
            p
        };
        let old_a = mk("2026-07-01", "080000.html");
        let old_b = mk("2026-07-01", "090000.html");
        let mid_a = mk("2026-07-02", "080000.html");
        let mid_b = mk("2026-07-02", "090000.html");
        let new_a = mk("2026-07-03", "080000.html");
        let new_b = mk("2026-07-03", "090000.html");
        // A non-date folder must be left strictly alone, whatever its size.
        let other = root.join("notes");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("big.txt"), [b'x'; 10_000]).unwrap();

        // Total dump size = 600; cap at 350 -> the three oldest files
        // (100 each) must go, freeing down to 300.
        enforce_dump_size_cap(root, 350).await.unwrap();

        assert!(!old_a.exists(), "oldest file must be deleted first");
        assert!(!old_b.exists(), "second-oldest must be deleted");
        assert!(!mid_a.exists(), "third-oldest must be deleted");
        assert!(mid_b.exists(), "must stop once under the cap");
        assert!(new_a.exists());
        assert!(new_b.exists());
        // The emptied oldest day folder disappears; the half-full one stays.
        assert!(!root.join("2026-07-01").exists(), "emptied dir swept");
        assert!(root.join("2026-07-02").exists());
        assert!(other.exists(), "non-date folder untouched");
        assert!(other.join("big.txt").exists());
    }

    #[tokio::test]
    async fn size_cap_is_a_noop_under_the_limit_and_on_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Missing dir: fine.
        enforce_dump_size_cap(&root.join("never_existed"), 1)
            .await
            .unwrap();

        // Under the cap: nothing deleted.
        let d = root.join("2026-07-03");
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("080000.html");
        std::fs::write(&f, "small").unwrap();
        enforce_dump_size_cap(root, 1024).await.unwrap();
        assert!(f.exists());
    }

    #[tokio::test]
    async fn save_html_dump_writes_a_file_at_the_expected_layout() {
        let dir = tempfile::tempdir().unwrap();
        save_html_dump(dir.path(), "<html>hi</html>", 2, None)
            .await
            .unwrap();

        // Walk: <dir>/YYYY-MM-DD/HHMMSS-pN.html — find the single file.
        let day_dirs: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(day_dirs.len(), 1, "exactly one day-bucket dir");

        let html_files: Vec<_> = std::fs::read_dir(day_dirs[0].path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("html"))
            .collect();
        assert_eq!(html_files.len(), 1, "exactly one .html file");

        // The page suffix must be part of the name so pages within the same
        // second don't clobber each other; the legacy target adds no -f tag.
        let name = html_files[0].file_name().to_string_lossy().into_owned();
        assert!(name.ends_with("-p2.html"), "{name}");
        assert!(
            !name.contains("-f"),
            "legacy dump must have no filter tag: {name}"
        );

        let body = std::fs::read_to_string(html_files[0].path()).unwrap();
        assert_eq!(body, "<html>hi</html>");
    }

    #[tokio::test]
    async fn save_html_dump_tags_saved_filter_dumps_with_the_filter_id() {
        let dir = tempfile::tempdir().unwrap();
        // Two filters dumping the same page within the same second must land
        // in two files (#10, stage 2) — the -fID segment disambiguates.
        save_html_dump(dir.path(), "a", 1, Some(7)).await.unwrap();
        save_html_dump(dir.path(), "b", 1, Some(8)).await.unwrap();

        let day_dir = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .next()
            .unwrap();
        let mut names: Vec<String> = std::fs::read_dir(day_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "one dump per filter: {names:?}");
        assert!(names[0].ends_with("-f7-p1.html"), "{names:?}");
        assert!(names[1].ends_with("-f8-p1.html"), "{names:?}");
    }
}
