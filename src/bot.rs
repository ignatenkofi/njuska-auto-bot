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

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, error, info, warn};

use crate::config::{RuntimeConfig, StaticConfig};
use crate::scraper;
use crate::signals::shutdown_signal;
use crate::storage::Storage;
use crate::telegram::{self, TelegramClient, TelegramError};

/// State that has to survive across cycles. Kept as a separate struct so we
/// can hand it to a pure `update_streak` helper that's testable in isolation.
#[derive(Default)]
pub(crate) struct LoopState {
    /// Consecutive cycles where `parse_listings` returned 0.
    zero_streak: u32,
    /// True once we've fired a TG alert for the current streak. Reset when
    /// the streak breaks. Prevents one outage from flooding the chat.
    streak_alerted: bool,
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

    loop {
        // Run a cycle first — so we poll immediately on bot startup, not
        // after waiting an interval.
        if let Err(e) = run_one_cycle(&static_cfg, &runtime, &storage, &telegram, &mut state).await
        {
            // The cycle's own error path is `warn!` / `error!`; this catches
            // `?` propagations from places we couldn't recover. The loop
            // continues regardless.
            error!(error = ?e, "poll cycle returned an error");
        }

        // Sleep until: the configured interval elapses, OR /interval changes
        // the interval, OR a shutdown signal arrives. Whichever comes first.
        let sleep_dur = runtime.read().await.poll_interval;
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
async fn run_one_cycle(
    static_cfg: &StaticConfig,
    runtime: &Arc<RwLock<RuntimeConfig>>,
    storage: &Storage,
    telegram: &TelegramClient,
    state: &mut LoopState,
) -> Result<()> {
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

    let html = match scraper::fetch_search(&search, static_cfg.cf_proxy.as_ref()).await {
        Ok(h) => h,
        Err(e) => {
            // Fetch failures (CF blocked us, network blip, curl bug) are
            // *transient* by nature — they're not the "parser broken" signal
            // we want the zero-streak detector to react to. So we don't
            // touch the streak counter here. Just log and bail this cycle.
            warn!(error = %e, "fetch failed; will retry next cycle");
            return Ok(());
        }
    };

    if static_cfg.save_raw_html {
        if let Err(e) = save_html_dump(&static_cfg.dumps_dir, &html).await {
            // Dump-on-disk is a debugging convenience; failing to write to disk
            // shouldn't fail the bot. Log and carry on.
            warn!(error = %e, "couldn't write HTML dump");
        }
        if static_cfg.dump_retention_days > 0
            && let Err(e) =
                rotate_dumps(&static_cfg.dumps_dir, static_cfg.dump_retention_days).await
        {
            warn!(error = %e, "couldn't rotate old HTML dumps");
        }
    }

    let listings = scraper::parse_listings(&html);

    // Update the zero-streak state and *maybe* fire an alert.
    if let Some(alert) = update_streak(
        state,
        listings.is_empty(),
        static_cfg.zero_results_alert_threshold,
    ) {
        if let Err(e) = telegram.send_message(&alert).await {
            warn!(error = %e, "couldn't send zero-streak alert");
        } else {
            state.streak_alerted = true;
        }
    }

    // Mark-after-send: first identify unseen listings (read-only), THEN send,
    // THEN persist only those that actually got through. A network failure
    // mid-batch leaves the unsent ones in the unseen set so the next cycle
    // picks them up. The previous "filter_new" (mark-then-send) lost them.
    let unseen = storage.unseen(&listings).context("dedup")?;
    info!(
        total = listings.len(),
        unseen = unseen.len(),
        "cycle parsed"
    );

    let sent = send_batch(telegram, &unseen).await;
    let attempted = unseen.len();
    let sent_count = sent.len();
    storage
        .mark_seen(&sent)
        .context("marking successfully-sent listings")?;
    info!(
        attempted,
        sent = sent_count,
        failed = attempted - sent_count,
        "cycle done"
    );
    Ok(())
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

/// Sends each new listing to Telegram. Returns the listings that **successfully**
/// went through — caller is responsible for marking those as seen.
///
/// Listings whose send fails (network error, individual Api error) are
/// silently dropped from the returned slice — they'll re-appear in the next
/// cycle's `unseen` set and we'll retry them then.
///
/// On a 429 we sleep for the server-supplied `retry_after` and retry the
/// same message **once**. If the retry also fails (429 again or otherwise),
/// we log and stop the batch — the next cycle (in `poll_interval`) picks
/// up where we left off, by which point any rate-limit window has expired.
///
/// Cap on sleep: Telegram has been known to suggest very large `retry_after`
/// values for "spam-flagged" bots. We clamp to 60s so a misconfigured run
/// can't freeze the cycle for half an hour.
async fn send_batch(
    telegram: &TelegramClient,
    listings: &[crate::models::Listing],
) -> Vec<crate::models::Listing> {
    /// Hard ceiling on how long we'll sleep waiting out a 429.
    const MAX_BACKOFF_SECS: u64 = 60;

    let mut sent: Vec<crate::models::Listing> = Vec::with_capacity(listings.len());

    for l in listings {
        let text = telegram::format_listing_html(l);
        match telegram.send_message(&text).await {
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

                match telegram.send_message(&text).await {
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
            Err(e) => {
                warn!(id = l.id, error = %e, "telegram send failed");
            }
        }
    }
    sent
}

/// Writes the raw HTML response under `<dumps_dir>/YYYY-MM-DD/HHMMSS.html`.
/// Creates intermediate directories if needed. Day-bucketed so daily
/// rotation is trivial (drop yesterday's folder).
async fn save_html_dump(dumps_dir: &Path, html: &str) -> std::io::Result<()> {
    let now = Local::now();
    let day_dir = dumps_dir.join(now.format("%Y-%m-%d").to_string());
    tokio::fs::create_dir_all(&day_dir).await?;
    let path = day_dir.join(format!("{}.html", now.format("%H%M%S")));
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

#[cfg(test)]
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
    async fn save_html_dump_writes_a_file_at_the_expected_layout() {
        let dir = tempfile::tempdir().unwrap();
        save_html_dump(dir.path(), "<html>hi</html>").await.unwrap();

        // Walk: <dir>/YYYY-MM-DD/HHMMSS.html — find the single file.
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

        let body = std::fs::read_to_string(html_files[0].path()).unwrap();
        assert_eq!(body, "<html>hi</html>");
    }
}
