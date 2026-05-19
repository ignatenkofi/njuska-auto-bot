//! Shared OS-signal future for clean shutdown.
//!
//! Both the poll loop ([`bot::run`]) and the command dispatcher
//! ([`commands::run_command_loop`]) `select!` on this future to bail out
//! when the operator interrupts the process.
//!
//! Each call to [`shutdown_signal`] returns a **fresh** future — calling it
//! from N async contexts is fine, each gets its own future that resolves
//! when the signal fires. No coordination between them needed.

use tracing::info;

/// Future that resolves on the first OS shutdown signal we care about.
///
/// On Unix that's SIGINT (Ctrl+C) **or** SIGTERM (systemd's default, Docker's
/// default). Without the SIGTERM handler, `systemctl stop njuska` would kill
/// the process abruptly — DB transactions in flight wouldn't get a chance
/// to commit cleanly.
///
/// On non-Unix targets (Windows), we only have Ctrl+C. The `cfg` split is
/// the standard pattern for "platform feature only on Unix".
pub async fn shutdown_signal() {
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
