# Changelog

All notable changes to NjuskaAutoBot are documented here.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/)
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases come from tag pushes (`v*`) — see [README → Cutting a release](README.md#cutting-a-release).

---

## [Unreleased]

Nothing yet.

---

## [0.1.0] — 2026-05-19

First production release.

### Added — scraper / poll loop (v1)

- HTML scraper for polovniautomobili.com search results — parses `<article class="classified ad-…">` blocks via CSS selectors into typed `Listing` records.
- Async poll loop with `tokio` — fetches every `POLL_INTERVAL_SECS`, default 10 min, configurable down to 60 s.
- SQLite-backed dedup via `rusqlite` (bundled SQLite, no system dependency). Listings are remembered across restarts in the `seen_listings` table.
- Mark-after-send semantics — listings only get marked "seen" after a successful Telegram send. Failed sends stay in the unseen set and retry the next cycle.
- Zero-results-streak detector — if `parse_listings` returns 0 listings `ZERO_RESULTS_ALERT_THRESHOLD` times in a row (default 3), the bot sends a self-alert to Telegram. Designed to catch "polovni changed their HTML, parser is broken" silently.
- Raw HTML dump capture under `./dumps/YYYY-MM-DD/HHMMSS.html`, with day-bucketed rotation (`DUMP_RETENTION_DAYS`, default 7). Used to debug parser breakage against the exact snapshot that triggered it.
- Graceful shutdown on SIGINT *and* SIGTERM (`signals.rs`). Drains in-flight cycles cleanly; no half-committed DB transactions.

### Added — Telegram delivery (v1)

- Send-only Telegram client using `teloxide::Bot`. Per-listing message with HTML markup, link preview, and a one-line metadata footer (price · year · mileage · city).
- HTML escaping for untrusted listing text (titles, cities) so a `<script>` or `&` in a real listing doesn't break the parse.
- Retry on Telegram 429 — sleep the server-suggested `retry_after` (capped at 60 s) and retry once. Subsequent 429s stop the batch; next poll cycle picks up where we left off.

### Added — interactive UI (v2)

- Long-polling Telegram command dispatcher via `teloxide`, running concurrently with the poll loop. Shared mutable state via `Arc<RwLock<RuntimeConfig>>`.
- Commands: `/start`, `/help`, `/status`, `/pause`, `/resume`, `/interval N`, `/clear` + `/clear_confirm`, `/filter`, `/setbrand <slug>`, `/dump N`, `/cancel`.
- `/filter` wizard with inline keyboards covering all main search-filter fields:
  - **Brand** — 20-item single-select picker, plus `/setbrand <slug>` for catalog gaps.
  - **Models** — multi-select per brand, with brand-specific catalogs for 18 brands (~6-8 models each).
  - **Body type** — multi-select toggle from 6 codes (sedan, hatchback, coupe, kombi, SUV, kabriolet).
  - **Price** — 6 single-tap range presets in EUR.
  - **Year** — 6 single-tap range presets.
  - **Interval** — 6 single-tap presets from 1 min to 2 hours; non-presets via `/interval N`.
  - **Reset** — two-step confirmed wipe of all filter fields.
- Auto-clear of `models` when `brand` changes (the lists are brand-specific).
- Authorization via `AUTHORIZED_USER_ID` env var. Anyone else's commands are logged at `warn` level and dropped.
- Persistence — all `/filter`-changed values stored in the SQLite `runtime_settings` table. Bot resumes the user's settings across restarts; env defaults apply only on fresh DB.

### Added — deployment infrastructure

- Cloudflare Worker proxy (`cf-proxy/`) — ~30-line JavaScript that forwards GET requests from the bot to polovniautomobili.com from CF's own network. Side-steps CF Managed Challenge that flags Linux network stacks. Free tier covers 100k requests/day; we use ~150. Wired to bot via `CF_PROXY_URL` + `CF_PROXY_SECRET` env vars.
- Systemd unit (`deploy/njuska-auto-bot.service`) — dedicated `njuska` user, hardened (`NoNewPrivileges`, `ProtectSystem=strict`, `ReadWritePaths=/opt/njuska-auto-bot`, `SystemCallFilter=@system-service`, `MemoryMax=200M`), auto-restart on failure.
- Deployment walkthrough (`deploy/DEPLOY.md`) — Proxmox VM setup end-to-end: VM sizing, user, binary fetch, .env, systemd install, ops, troubleshooting.
- `deploy/update.sh` — single-command in-place upgrade for the VM. Downloads the latest binary from GitHub Releases, atomically swaps it in, restarts the service, tails recent logs.

### Added — CI/CD

- `ci.yml` — runs on every push to `main` and on every PR: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo build --release`. Uses `Swatinem/rust-cache` for fast warm builds. Concurrency-grouped to cancel stale in-flight runs.
- `release.yml` — triggered **only on `v*` tag push** (and manually via `workflow_dispatch`). Builds a stripped Linux x86_64 binary on `ubuntu-22.04` (older glibc → portable to Debian 12+), publishes it to a GitHub Release with auto-generated notes from commit history.
- VM runtime needs only `curl` + `git` — no Rust toolchain. Binary comes from `releases/latest/download/njuska_auto_bot`.

### Added — docs and licensing

- Public GitHub repo with README, dual MIT/Apache-2.0 license files.
- `cf-proxy/README.md` — 5-minute Worker setup, including secret rotation and `wrangler tail` debugging.
- `CHANGELOG.md` (this file).

### Stack

- Async runtime: `tokio` 1.x
- Telegram: `teloxide` 0.17 (long-polling, dispatcher, inline keyboards)
- HTTP client: `reqwest` (Telegram API only, polovni goes through curl)
- HTML parsing: `scraper` (CSS selectors via the `selectors` crate)
- Database: `rusqlite` 0.32 with bundled SQLite
- Logging: `tracing` + `tracing-subscriber` with `env-filter`
- Errors: `anyhow` at boundaries, `thiserror` in library-shaped modules
- Time: `chrono` (file rotation + DB timestamps)
- URL: `url` (search-URL builder, proxy URL parsing)

### Known limitations

- Direct fetches from Linux network stacks are blocked by Cloudflare. The Worker proxy is required for Linux deployments. macOS direct fetch still works.
- Model catalog covers 18 brands. Brands outside the catalog (Smart, Alfa Romeo, Suzuki, etc.) fall through to `/setbrand <slug>` typed-command or `SEARCH_MODEL` in `.env`.
- Polling interval has a hard floor of 60 s (`MIN_POLL_INTERVAL_SECS`) — politeness to the upstream site. Trying to go below this via `/interval N` or `.env` is rejected with a clear error.
- Single-user-scoped commands. If you want family/coworkers to also send `/pause` etc., extend `AUTHORIZED_USER_ID` to a list — currently it's one user.
