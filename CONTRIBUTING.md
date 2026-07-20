# Contributing to njuska-auto-bot

Small hobby project, PRs welcome. This file is the practical "how do I work
on this" guide; architecture background lives in `README.md`, day-to-day
working rules in `CLAUDE.md`.

## Dev loop

```bash
cp .env.example .env            # fill in TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID,
                                # AUTHORIZED_USER_ID at minimum
cargo build
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo test
RUST_LOG=njuska_auto_bot=debug cargo run
```

A change is "done" only when **all four** of build / clippy / fmt / test are
green. Clippy runs with `-D warnings` — warnings are errors here, and the
crate additionally denies `clippy::unwrap_used` / `clippy::expect_used`
(see below).

The crate is a **lib + thin binary**: modules live in `src/lib.rs`'s tree so
integration tests can link against them; `src/main.rs` only wires them
together. Keep new code in the library side.

## Testing rules

- Unit tests live next to their module in a `#[cfg(test)] mod tests` block,
  annotated `#[allow(clippy::unwrap_used, clippy::expect_used)]` — unwrap is
  fine in tests.
- **No live network in tests, ever.** The parser is tested against saved
  HTML fixtures in `tests/fixtures/`. If the site's markup changes, save a
  fresh dump (the bot writes them to `./dumps/YYYY-MM-DD/` while
  `SAVE_RAW_HTML=true`) and add/update a fixture.
- Integration tests live in `tests/` and use a **temp SQLite file**
  (`tempfile::tempdir()`), never the real `njuska.db`.
  `tests/poll_cycle.rs` shows the pattern: the real `bot::run_one_cycle`
  with a fixture-serving fetch closure and a collecting `Notifier`.
- Run `cargo test` after every change that touches non-trivial logic.

## Error handling conventions

- `anyhow::Result` at the binary boundary and orchestration code
  (`main.rs`, the poll loop). `thiserror`-derived enums in modules whose
  callers need to match on variants (`ScraperError`, `TelegramError`).
- **No `unwrap`/`expect` in production paths** — enforced by
  `[lints.clippy]` in `Cargo.toml`. The few justified uses (poisoned-mutex
  expects, compile-time constants) carry an explicit `#[allow]` with a
  why-comment; add new ones only with the same justification standard.
- Prefer `?` over `match` for propagation.
- The poll loop **never panics out**: errors inside a cycle are logged
  (`warn!`/`error!`) and the loop continues. Don't propagate a per-cycle
  error into a crash.
- Logging levels (via `tracing`): `error!` = unrecoverable this iteration,
  `warn!` = recoverable anomaly (retry, streak), `info!` = lifecycle,
  `debug!` = per-listing detail.

## Adding a filter section to `/filter`

Follow the pattern of an existing section in the `src/commands/` module —
pick the closest shape:

- **single-select** → brand picker (`CB_FILTER_BRAND_*`)
- **multi-select** → chassis picker (`CB_FILTER_CHASSIS_*`, draft slot in
  `CommandContext`)
- **from/to range** → the shared range picker (`CB_FILTER_RANGE_SET_PREFIX`)

`commands/` is split (per #24): pure catalog data in `catalog.rs`, inline
keyboards + callback-data constants in `keyboards.rs`, per-command handlers /
`apply_*` / formatters in `handlers.rs`, and routing (`handle_command`,
`handle_callback`) + `Command`/`CommandContext` in `mod.rs`.

The steps are always the same:

1. Catalog constant (`const FOO: &[…]`) in `commands/catalog.rs`.
2. Callback-data constants in the `f:` namespace, in `commands/keyboards.rs`
   (Telegram caps callback data at 64 bytes — keep them short).
3. Keyboard builder fn in `commands/keyboards.rs` + branch(es) in
   `handle_callback` (`commands/mod.rs`).
4. An `apply_foo()` in `commands/handlers.rs` that **persists to
   `runtime_settings` first, then mutates `RuntimeConfig`, then calls
   `runtime_changed.notify_one()`** — persist-first is the invariant that
   keeps RAM and DB consistent on a failed write.
5. A `SETTING_…` key in `config.rs` plus the three-state merge in
   `RuntimeConfig::load` (absent key = env default, empty = explicitly
   cleared, value = user's choice).
6. Render the field in `format_filter_ru` (`commands/handlers.rs`), escape
   anything user-supplied with `telegram::escape_html`.
7. Add `.env` fallback support in `config.rs` and document it in
   `.env.example`.

## Branches, PRs, releases

- Work on a branch, open a PR against `main`. CI (`ci.yml`) runs
  fmt/clippy/tests/release-build plus a RustSec audit; everything must be
  green to merge.
- Commit messages: imperative summary line, reference the issue
  (`… (#12)`). No emoji.
- Never commit secrets. `.env` is gitignored — check `git status` before
  committing anyway.
- **Releases are tag-driven.** Nothing ships from `main` automatically:

  ```bash
  git tag v0.2.0
  git push origin v0.2.0
  ```

  `release.yml` then runs tests, builds a stripped Linux binary on
  ubuntu-22.04, smoke-tests it in a Debian 12 container (`--version`), and
  publishes a GitHub Release. Deployment boxes pull
  `releases/latest/download/njuska_auto_bot` via `deploy/update.sh`.

## When the parser breaks

If polovniautomobili.com changes its HTML, the bot alerts "0 listings N
times in a row". The raw dumps in `./dumps/YYYY-MM-DD/` exist exactly for
this: open the newest one, find the changed selectors, update
`src/scraper.rs`, and refresh the fixture in `tests/fixtures/` so the tests
pin the new markup.
