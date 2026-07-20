# CLAUDE.md — working rules for this project

This file is loaded into Claude's context every session. Keep it tight and current.

## About the user

First practical Rust project after reading the first chapters of the Rust Book. Theory is there (ownership, borrowing, `Result`/`Option`, traits) — idiomatic experience is not. Explain idioms inline as short asides: *why* `&str` vs `String` here, what `?` desugars to, *why* `anyhow` at the binary boundary vs `thiserror` in a module. One or two sentences, not a tutorial.

When the user proposes something C++/Python-shaped, push back gently and explain the Rust idiom instead of silently rewriting.

## Pacing

- One module or one feature per step. After a meaningful change, stop and let the user read it before moving on. No 500-line drops.
- Run `cargo build`, `cargo clippy`, `cargo test` **yourself**. Don't ask the user to paste output.
- Surface warnings — don't ignore them. Clippy lints are not optional in this project.

## Code style

- `cargo clippy --all-targets -- -D warnings` must pass before any change is considered done.
- `cargo fmt` before commit.
- No unwrap/expect in production paths. In tests they're fine.
- Prefer `?` over `match` for error propagation.
- Errors:
  - `anyhow::Result` at the binary boundary and orchestration code (`main.rs`, the poll loop).
  - `thiserror`-derived enums in modules that need callers to match on variants (e.g. scraper distinguishing `Http` vs `ParseFailed` vs `RateLimited`).
- Comments explain *why*, not *what*. Don't restate the code.
- No emoji in code or commits unless explicitly asked.
- Don't add backwards-compatibility shims unless asked. We're pre-1.0.

## Logging

Use `tracing`. Levels:

- `error!` — something we can't recover from in this iteration but the process keeps running.
- `warn!` — recoverable anomalies (retry happened, zero-results streak, etc.).
- `info!` — lifecycle events (started polling, sent N messages).
- `debug!` — per-listing detail, parsed counts, URL being fetched.
- `trace!` — only when actively chasing a bug.

Default filter (`.env`): `njuska_auto_bot=debug,info`.

## Secrets

Never hardcode tokens, chat IDs, or any credential. Always read from env (`.env` via `dotenvy`). `.env` is gitignored — verify before committing.

## Tests

- Unit tests live alongside their module in a `#[cfg(test)] mod tests` block.
- The scraper's parsing logic must be tested against saved HTML fixtures (live network in tests is forbidden). Fixtures go in `tests/fixtures/`.
- Integration tests in `tests/` use a temp SQLite file, never the real one.
- Run `cargo test` after every change that touches non-trivial logic.

## Resilience invariants

These were agreed at project start — don't quietly drop them in a refactor:

1. The poll loop never panics out. All errors in one iteration are logged and the loop continues.
2. If the scraper returns zero listings `ZERO_RESULTS_ALERT_THRESHOLD` times in a row, send an alert to Telegram (the site likely changed).
3. While `SAVE_RAW_HTML=true`, every fetched search page is saved under `./dumps/YYYY-MM-DD/<timestamp>-p<page>.html` (the `-p<page>` suffix keeps multi-page cycles from overwriting themselves).
4. Network/parse retries use exponential backoff with a cap. No tight retry loops.

## Project layout

Modules under `src/` (promote a module to a directory with `mod.rs` when it
exceeds ~300 lines or splits naturally — `commands/` already has, per #24):

- `main.rs` — entry: load env, init tracing, spawn the poll loop + command listener, then supervise them (`tokio::select!`, restart-on-death).
- `lib.rs` — library facade re-exporting the modules so integration tests can link against them.
- `bot.rs` — the poll loop: fetch → dump → parse → dedup → send, plus the zero-results streak detector and dump rotation.
- `config.rs` — env -> `StaticConfig` (fixed) + `RuntimeConfig` (env defaults merged with DB overrides) + `ProxyConfig`.
- `models.rs` — `Listing`, `SearchFilter`, `ShowOldNew`, shared types.
- `scraper.rs` — curl shell-out (optional CF Worker proxy) + HTML parse via CSS selectors.
- `storage.rs` — SQLite via `rusqlite` (`seen_listings` dedup + `runtime_settings`).
- `telegram.rs` — teloxide-based send-only client + HTML escaping.
- `commands/` — teloxide command dispatcher: `mod.rs` (routing/auth, `Command`, `CommandContext`), `catalog.rs` (brand/model/body-type data), `handlers.rs` (per-command handlers + `apply_*` + formatters), `keyboards.rs` (inline keyboards + callback-data constants).
- `signals.rs` — shared SIGINT/SIGTERM future + internal `request_shutdown` path.
- `version.rs` — compile-time `VERSION` string (`CARGO_PKG_VERSION` + git SHA from `build.rs`).

## Useful commands

```bash
cargo build
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo test
RUST_LOG=njuska_auto_bot=debug cargo run
```
