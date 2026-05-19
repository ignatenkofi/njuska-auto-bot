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
3. While `SAVE_RAW_HTML=true`, fetched HTML is saved under `./dumps/YYYY-MM-DD/<timestamp>.html`.
4. Network/parse retries use exponential backoff with a cap. No tight retry loops.

## Project layout

Flat modules under `src/`:

- `main.rs` — entry, tracing init, poll loop.
- `config.rs` — env -> typed `Config`.
- `models.rs` — `Listing`, `SearchFilter`, shared types.
- `scraper.rs` — fetch + parse.
- `storage.rs` — SQLite via `rusqlite`.
- `telegram.rs` — send-only Bot API client.

Promote a module to a directory with `mod.rs` only when it exceeds ~300 lines or splits naturally into submodules.

## Useful commands

```bash
cargo build
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo test
RUST_LOG=njuska_auto_bot=debug cargo run
```
