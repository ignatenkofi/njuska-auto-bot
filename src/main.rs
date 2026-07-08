//! NjuskaAutoBot — entrypoint.
//!
//! Thin top-level: load env, init tracing, build the collaborators,
//! spawn two concurrent tasks (poll loop and command listener), wait for
//! either to finish. Both ends gracefully on SIGINT/SIGTERM via
//! [`signals::shutdown_signal`].

mod bot;
mod commands;
mod config;
mod models;
mod scraper;
mod signals;
mod storage;
mod telegram;
mod version;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::{Notify, RwLock};
use tracing::{error, info};

use crate::commands::CommandContext;
use crate::config::{RuntimeConfig, StaticConfig};
use crate::storage::Storage;
use crate::telegram::TelegramClient;

#[tokio::main]
async fn main() -> Result<()> {
    // `--version` is the cheap startup path: print and exit before touching
    // .env, tracing, or the database. Deployment scripts and the Debian
    // smoke test rely on this working with zero configuration present.
    if std::env::args()
        .skip(1)
        .any(|a| a == "--version" || a == "-V")
    {
        println!("njuska_auto_bot {}", version::VERSION);
        return Ok(());
    }

    // Load .env first so RUST_LOG etc. are visible to tracing_subscriber below.
    // `.ok()` because a missing .env is fine in environments that inject env vars
    // directly (containers, systemd unit, CI).
    let _ = dotenvy::dotenv();

    init_tracing();

    info!(version = version::VERSION, "njuska_auto_bot starting");

    // Two-step config load: static first (everything we need to *open* storage,
    // including DB path), then runtime (which depends on storage being open
    // because it merges env defaults with DB-persisted overrides).
    let static_cfg = Arc::new(StaticConfig::from_env().context("loading static config from env")?);
    info!(
        save_raw_html = static_cfg.save_raw_html,
        zero_results_threshold = static_cfg.zero_results_alert_threshold,
        dump_retention_days = static_cfg.dump_retention_days,
        "static config loaded"
    );

    let storage = Arc::new(Storage::new(&static_cfg.database_path).context("opening storage")?);

    let runtime_initial = RuntimeConfig::load(&storage).context("loading runtime config")?;
    info!(
        poll_interval_secs = runtime_initial.poll_interval.as_secs(),
        paused = runtime_initial.paused,
        brand = ?runtime_initial.search.brand,
        models = ?runtime_initial.search.models,
        chassis = ?runtime_initial.search.chassis,
        "runtime config loaded"
    );
    let runtime = Arc::new(RwLock::new(runtime_initial));

    let telegram = TelegramClient::new(
        static_cfg.telegram_token.clone(),
        static_cfg.telegram_chat_id,
    );

    // Shared signal: `/interval N` notifies, poll loop's sleep wakes early.
    // Notify is *edge*-triggered: one `notify_one()` ≈ one wake; if no waiter
    // is parked at the moment of notify, the signal is stored and the next
    // `notified()` returns immediately. That's exactly the "don't miss the
    // change even if the loop is mid-cycle" property we want.
    let runtime_changed = Arc::new(Notify::new());
    // Ephemeral state for the `/clear` → `/clear_confirm` two-step. Lives in
    // RAM only — there's no persistence story for "user typed /clear 25
    // seconds ago"; if the bot restarts, the pending request is lost (which
    // is correct: a fresh process means a fresh chance to reconsider).
    let pending_clear = Arc::new(Mutex::new(None));
    // Same shape — in-flight chassis selection during `/filter → Кузов`.
    // `None` = picker not open. Becomes `None` again on Save or Back.
    let chassis_draft = Arc::new(Mutex::new(None));
    // Same shape for models. Decoupled draft slots make it cheap to add more
    // multi-select pickers later (e.g. fuel types) without state collisions.
    let models_draft = Arc::new(Mutex::new(None));

    // ----- Spawn the poll loop -----
    let poll_task = tokio::spawn({
        let static_cfg = static_cfg.clone();
        let runtime = runtime.clone();
        let storage = storage.clone();
        let telegram = telegram.clone();
        let runtime_changed = runtime_changed.clone();
        async move {
            if let Err(e) = bot::run(static_cfg, runtime, storage, telegram, runtime_changed).await
            {
                error!(error = ?e, "poll loop exited with error");
            }
        }
    });

    // ----- Spawn the command dispatcher -----
    let cmd_task = tokio::spawn({
        // teloxide takes the `Bot` directly; we clone it out of our wrapper
        // (cheap — `Bot` is Arc-internal).
        let bot = telegram.bot();
        let ctx = CommandContext {
            static_cfg: static_cfg.clone(),
            runtime: runtime.clone(),
            storage: storage.clone(),
            runtime_changed: runtime_changed.clone(),
            pending_clear: pending_clear.clone(),
            chassis_draft: chassis_draft.clone(),
            models_draft: models_draft.clone(),
        };
        async move {
            if let Err(e) = commands::run_command_loop(bot, ctx).await {
                error!(error = ?e, "command loop exited with error");
            }
        }
    });

    // Both tasks subscribe to `shutdown_signal()` independently. On Ctrl+C /
    // SIGTERM each one's `select!` resolves and they shut themselves down.
    // We just wait for both to finish before returning from `main`.
    let _ = tokio::join!(poll_task, cmd_task);

    info!("njuska_auto_bot exited cleanly");
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    // EnvFilter::try_from_default_env reads RUST_LOG. Fall back to a sensible
    // default if it's unset/invalid so the bot still produces logs.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("njuska_auto_bot=info,warn"));

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();
}
