# TODO

Things we noticed while building — mostly during the CF Worker / release
pipeline / docs session. Grouped roughly; nothing here is urgent.

## Features

- [ ] **Multi-user authorization.** `AUTHORIZED_USER_ID` is single-id today;
  accept a comma-separated list so a small group (family/co-renters) can
  all `/pause` and edit filters.
- [ ] **Multiple filter sets.** One bot, one filter today. Want: "BMW 3
  ≤ 2018 ≤ 8k EUR" *and* "any kombi ≤ 5k EUR" running side by side, with
  filter-scoped dedup. Likely a `filters` table with a name + a join key
  on `seen_listings`.
- [ ] **`/version` command + version in `/status`.** Bake `CARGO_PKG_VERSION`
  + git SHA at build time, expose via `/version`. Today there's no way to
  tell from Telegram what build is running on the VM.
- [ ] **`/diag` command.** Runs the same fetch the poll loop does, reports
  proxy reachable / auth ok / listings parsed. Would have saved an hour
  during today's `CF_PROXY_SECRET` placeholder mishap.
- [ ] **Show effective search URL in `/status`.** Copy-paste-able URL the
  bot is actually hitting — lets you sanity-check what the `/filter` wizard
  built.
- [ ] **Wizard "back" navigation.** Inline-keyboard sections are leaves
  today — to change brand after picking a model you re-run `/filter`.
  Add a `← back` row.
- [ ] **Dynamic brand/model catalog.** The 18 hardcoded brands are a
  maintenance pothole. Periodically fetch the dropdowns from polovni and
  persist to DB, retiring the `/setbrand <slug>` fallback.
- [ ] **Daily poll interval preset.** Add `сутки` / 24h as a one-tap option
  in the `/filter` interval picker. Current presets cap at 2 hours, but for
  "слежу за рынком в целом" use case 24h is the right cadence.
- [ ] **Body types as labels, not codes.** Selected body types are rendered
  as the numeric polovni codes (e.g. `1, 4, 6`) in `/status` / filter
  summary — should reverse-lookup against the same catalog the picker uses
  and show `sedan, kombi, SUV`. Cosmetic but actively confusing.
- [ ] **Transmission filter (manual / automatic).** First user feedback:
  needs a gearbox-type filter in the `/filter` wizard. Single-select toggle,
  same shape as body type but smaller catalog.
- [ ] **Shrink / restructure the command surface.** 11 commands today is
  too many for the TG `/`-autocomplete menu. Options: collapse `/clear`,
  `/clear_confirm`, `/dump`, `/cancel` under an `/admin` (or similar)
  sub-menu; promote `/filter` as the main entry; hide debug commands.
  Goal: new user opens the bot and sees 3-4 commands, not a wall.

## Robustness & ops

- [ ] **Alert on persistent fetch error,** not just on persistent zero-
  listings. A wrong `CF_PROXY_SECRET` today silently 403'd for several
  cycles before the zero-streak detector fired. Track consecutive
  `fetch_search` errors separately and alert faster.
- [ ] **Startup health-check on the Worker.** At bot start, do one ping
  through the proxy; if it 403s, refuse to start and print a clear hint
  about `CF_PROXY_SECRET`. Beats discovering at minute 30.
- [ ] **`update.sh` rollback on startup failure.** Keep `njuska_auto_bot.bak`,
  restore-and-restart if the new binary crashes within N seconds.
- [ ] **HTML dump size cap.** Today only `DUMP_RETENTION_DAYS` (date) — add
  `DUMP_MAX_TOTAL_MB` so a listing spike can't fill the disk before day
  rotation kicks in.
- [ ] **SQLite vacuum + retention.** `seen_listings` only grows. Periodic
  `VACUUM` + "forget rows older than 6 months".
- [ ] **Retry on Telegram 5xx,** not only 429. A transient `502 Bad Gateway`
  drops the listing for that cycle.
- [ ] **CF Worker secret rotation runbook + script.** Today it's a 4-step
  manual dance (`wrangler secret put` → edit `.env` → `systemctl restart`
  → verify). Document in DEPLOY.md, ideally a `cf-proxy/rotate-secret.sh`.
- [ ] **Backups via cron + systemd timer.** `deploy/DEPLOY.md` suggests a
  tar command; nothing actually schedules it.

## Code quality & tests

- [ ] **End-to-end poll-loop test.** Mock the fetch with a fixture, run one
  cycle against a temp SQLite, assert N would-have-been-sent listings.
  Scraper and dedup have coverage; the wiring between them doesn't.
- [ ] **Telegram delivery tests.** `format_listing_html` + `escape_html`
  deserve adversarial unit tests (`<script>`, `&amp;amp;`, RTL chars,
  500-char titles).
- [ ] **Debian 12 smoke test in CI.** `release.yml` builds on `ubuntu-22.04`
  for glibc 2.35 portability but doesn't *run* the binary on Debian 12.
  Add a job: download artifact, `--version` inside a `debian:12` container.
- [ ] **Lint `unwrap`/`expect` in `src/`.** CLAUDE.md forbids them in
  production paths; add `clippy::unwrap_used` + `clippy::expect_used` as
  project lints (denied in `src/`, allowed in `tests/`).

## Docs

- [ ] **Screenshots in README.** The `/filter` wizard is the main UX surface
  and impossible to picture from text. Add 3–4 screenshots: filter menu,
  models multi-select, status output.
- [ ] **CONTRIBUTING.md.** README's "Contributing" section is 4 lines; spin
  it out with dev loop, branch conventions, PR expectations.
- [ ] **SECURITY.md.** Document where to report vulns (privately; not via
  GitHub issues).
- [ ] **Localization (sr / ru UI strings).** Audience is mostly Russian-
  speaking expats in Serbia; English-only command help is friction.

## Accepted, not fixing

- `cf-proxy/.wrangler/cache/wrangler-account.json` is in commit `4f63695`
  (before the .gitignore caught it). Contains a CF account id — not a
  secret. We're not rewriting history.
- Polling interval is hard-floored at 60 s (politeness to upstream).
- macOS direct fetch works without the CF Worker via TCP-fingerprint
  accident. If Apple ships a kernel change that lines up with Linux's
  fingerprint, dev on Mac would also need the Worker.
