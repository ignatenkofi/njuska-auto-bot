//! Shared OS-signal future for clean shutdown.
//!
//! Both the poll loop ([`bot::run`]) and the command dispatcher
//! ([`commands::run_command_loop`]) `select!` on this future to bail out
//! when the operator interrupts the process.
//!
//! Each call to [`shutdown_signal`] returns a **fresh** future — calling it
//! from N async contexts is fine, each gets its own future that resolves
//! when the signal fires. No coordination between them needed.
//!
//! Besides OS signals there is an **internal** shutdown path: when one of
//! the two long-running tasks dies unexpectedly, `main` calls
//! [`request_shutdown`] so the surviving task stops too instead of running
//! headless (issue #14). A `watch` channel (not `Notify`) carries the
//! request because it's *level*-triggered: once flipped to `true`, any
//! future — even one created after the flip — resolves immediately. An
//! edge-triggered `Notify::notify_waiters` would miss a task that happened
//! to be mid-cycle rather than parked on `shutdown_signal()`.

use std::sync::LazyLock;

use tokio::sync::watch;
use tracing::info;

/// Process-wide "please shut down" flag. The `Sender` lives in the static so
/// it's never dropped — receivers can always subscribe.
static SHUTDOWN_REQUESTED: LazyLock<watch::Sender<bool>> =
    LazyLock::new(|| watch::channel(false).0);

/// Ask every task parked on (or about to call) [`shutdown_signal`] to stop.
/// Idempotent; used by `main` when one background task dies so the other
/// doesn't keep running headless.
pub fn request_shutdown() {
    // `send_replace`, not `send`: `send` refuses to store the value when no
    // receiver currently exists, and a task that subscribes a moment later
    // would then wait forever. `send_replace` stores unconditionally.
    SHUTDOWN_REQUESTED.send_replace(true);
}

/// Future that resolves on the first OS shutdown signal we care about, or
/// when [`request_shutdown`] has been called.
pub async fn shutdown_signal() {
    let mut internal = SHUTDOWN_REQUESTED.subscribe();
    tokio::select! {
        _ = os_shutdown_signal() => {}
        // `wait_for` checks the current value first, so a request that fired
        // before we subscribed still resolves immediately. The Result is
        // only Err when the Sender drops — impossible for a static.
        _ = internal.wait_for(|&requested| requested) => {
            info!("internal shutdown requested");
        }
    }
}

/// OS half: SIGINT (Ctrl+C) **or** SIGTERM (systemd's default, Docker's
/// default). Without the SIGTERM handler, `systemctl stop njuska` would kill
/// the process abruptly — DB transactions in flight wouldn't get a chance
/// to commit cleanly.
///
/// On non-Unix targets (Windows), we only have Ctrl+C. The `cfg` split is
/// the standard pattern for "platform feature only on Unix".
// Justified expect: if we can't install a SIGTERM handler we can't shut
// down gracefully anyway — crashing at startup is the honest outcome.
#[allow(clippy::expect_used)]
async fn os_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        // `expect` is acceptable here: signal-handler install only fails on
        // truly unusual systems (sandbox without signal syscall, etc.). If
        // we can't install handlers, we can't gracefully shut down anyway.
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received SIGINT (Ctrl+C), shutting down");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("received Ctrl+C, shutting down");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // fine in tests
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_signal_resolves_after_internal_request() {
        // Level-triggered: request first, subscribe after — must still fire.
        request_shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(1), shutdown_signal())
            .await
            .expect("shutdown_signal should resolve after request_shutdown");
    }
}
