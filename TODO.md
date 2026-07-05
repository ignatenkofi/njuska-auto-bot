# TODO

The backlog now lives in [GitHub Issues](https://github.com/ignatenkofi/njuska-auto-bot/issues)
(#1–#33, filed 2026-07-05). This file is an index, not the source of truth —
full context, implementation sketches and acceptance criteria are in the
issues. The "Accepted, not fixing" section at the bottom stays here: those
are decisions, not tasks.

## Features

- [#9](https://github.com/ignatenkofi/njuska-auto-bot/issues/9) Multi-user authorization (`AUTHORIZED_USER_ID` as a list)
- [#10](https://github.com/ignatenkofi/njuska-auto-bot/issues/10) Multiple filter sets with filter-scoped dedup
- [#1](https://github.com/ignatenkofi/njuska-auto-bot/issues/1) `/version` command + version in `/status`
- [#2](https://github.com/ignatenkofi/njuska-auto-bot/issues/2) `/diag` command — end-to-end fetch diagnostic
- [#3](https://github.com/ignatenkofi/njuska-auto-bot/issues/3) Show effective search URL in `/status`
- [#6](https://github.com/ignatenkofi/njuska-auto-bot/issues/6) Wizard "back" navigation
- [#11](https://github.com/ignatenkofi/njuska-auto-bot/issues/11) Dynamic brand/model catalog
- [#5](https://github.com/ignatenkofi/njuska-auto-bot/issues/5) Daily (24h) poll interval preset
- [#4](https://github.com/ignatenkofi/njuska-auto-bot/issues/4) Body types as labels, not codes
- [#7](https://github.com/ignatenkofi/njuska-auto-bot/issues/7) Transmission filter (manual / automatic)
- [#8](https://github.com/ignatenkofi/njuska-auto-bot/issues/8) Shrink / restructure the command surface

## Robustness & ops

- [#12](https://github.com/ignatenkofi/njuska-auto-bot/issues/12) Alert on persistent fetch error
- [#13](https://github.com/ignatenkofi/njuska-auto-bot/issues/13) Startup health-check on the Worker
- [#18](https://github.com/ignatenkofi/njuska-auto-bot/issues/18) `update.sh` rollback on startup failure
- [#16](https://github.com/ignatenkofi/njuska-auto-bot/issues/16) HTML dump size cap (`DUMP_MAX_TOTAL_MB`)
- [#17](https://github.com/ignatenkofi/njuska-auto-bot/issues/17) SQLite vacuum + retention
- [#15](https://github.com/ignatenkofi/njuska-auto-bot/issues/15) Retry on Telegram 5xx, not only 429
- [#20](https://github.com/ignatenkofi/njuska-auto-bot/issues/20) CF Worker secret rotation runbook + script
- [#19](https://github.com/ignatenkofi/njuska-auto-bot/issues/19) Backups via systemd timer

## Code quality & tests

- [#22](https://github.com/ignatenkofi/njuska-auto-bot/issues/22) End-to-end poll-loop test
- [#26](https://github.com/ignatenkofi/njuska-auto-bot/issues/26) Telegram delivery adversarial tests
- [#27](https://github.com/ignatenkofi/njuska-auto-bot/issues/27) Debian 12 smoke test in CI + tests before publish
- [#23](https://github.com/ignatenkofi/njuska-auto-bot/issues/23) Lint `unwrap`/`expect` in `src/`

## Docs

- [#30](https://github.com/ignatenkofi/njuska-auto-bot/issues/30) Screenshots in README
- [#31](https://github.com/ignatenkofi/njuska-auto-bot/issues/31) CONTRIBUTING.md
- [#32](https://github.com/ignatenkofi/njuska-auto-bot/issues/32) SECURITY.md
- [#33](https://github.com/ignatenkofi/njuska-auto-bot/issues/33) Localization (sr / ru UI strings)

## Found during the 2026-07-05 project review (not in the original TODO)

- [#14](https://github.com/ignatenkofi/njuska-auto-bot/issues/14) Bot keeps running headless if the command dispatcher dies
- [#25](https://github.com/ignatenkofi/njuska-auto-bot/issues/25) Scraper: pagination + explicit newest-first sort
- [#24](https://github.com/ignatenkofi/njuska-auto-bot/issues/24) Split `commands.rs` (1810 lines) into a module directory
- [#21](https://github.com/ignatenkofi/njuska-auto-bot/issues/21) Don't pass the proxy secret on the curl command line
- [#28](https://github.com/ignatenkofi/njuska-auto-bot/issues/28) Dependabot + RustSec audit in CI
- [#29](https://github.com/ignatenkofi/njuska-auto-bot/issues/29) Warn on unparseable `runtime_settings` values

## Accepted, not fixing

- `cf-proxy/.wrangler/cache/wrangler-account.json` is in commit `4f63695`
  (before the .gitignore caught it). Contains a CF account id — not a
  secret. We're not rewriting history.
- Polling interval is hard-floored at 60 s (politeness to upstream).
- macOS direct fetch works without the CF Worker via TCP-fingerprint
  accident. If Apple ships a kernel change that lines up with Linux's
  fingerprint, dev on Mac would also need the Worker.
