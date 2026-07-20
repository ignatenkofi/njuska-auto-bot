```
███╗   ██╗     ██╗██╗   ██╗███████╗██╗  ██╗ █████╗
████╗  ██║     ██║██║   ██║██╔════╝██║ ██╔╝██╔══██╗
██╔██╗ ██║     ██║██║   ██║███████╗█████╔╝ ███████║
██║╚██╗██║██   ██║██║   ██║╚════██║██╔═██╗ ██╔══██║
██║ ╚████║╚█████╔╝╚██████╔╝███████║██║  ██╗██║  ██║
╚═╝  ╚═══╝ ╚════╝  ╚═════╝ ╚══════╝╚═╝  ╚═╝╚═╝  ╚═╝
                  AUTO BOT
       polovniautomobili.com  →  Telegram
```

A Telegram bot, written in Rust, that watches
[polovniautomobili.com](https://www.polovniautomobili.com/) — Serbia's largest
used-car classifieds site — for new listings matching your filters and forwards
them to a Telegram chat or channel.

Built as a personal bot but works fine for friend groups or small expat
communities: invite the bot to a channel as admin, configure filters via the
bot itself (inline keyboards), and everyone subscribed sees fresh listings
the moment they appear.

- **Repo:** [github.com/ignatenkofi/njuska-auto-bot](https://github.com/ignatenkofi/njuska-auto-bot)
- **Latest release:** [`v0.1.0`](https://github.com/ignatenkofi/njuska-auto-bot/releases/latest)
- **Changes:** [CHANGELOG.md](CHANGELOG.md)
- **License:** MIT or Apache-2.0 (your choice)

## Features

- **Long-running poll loop.** Every N minutes (default 10), fetches the search
  page, parses listings, dedups against a local SQLite file, sends new ones to
  Telegram with a preview card per listing.
- **Configuration through the bot itself.** No restart, no `.env` editing.
  `/filter` opens an interactive menu with inline keyboards for brand, models,
  body type (multi-select), price range, year range, and poll interval.
  Everything persists across restarts in a `runtime_settings` SQLite table.
- **Resilient.** Network blips, Cloudflare hiccups, parser glitches — all
  logged and retried, never crash the process. A "zero listings N times in a
  row" detector pings you in TG when the site's HTML changes and selectors go
  stale.
- **Forensic-friendly.** Every fetched HTML page is saved under `./dumps/`
  (daily rotation, configurable) so you can replay a broken parser against
  the exact snapshot.
- **Mark-after-send semantics.** If Telegram is down, unsent listings stay
  in the "unseen" set and retry next poll — no lost notifications.
- **Cloudflare-friendly.** Optional Worker proxy bypasses CF's bot detection
  for Linux deployments (see [Architecture](#architecture)).

## Commands

| Command | What it does |
| --- | --- |
| `/start` `/help` | Welcome + commands list |
| `/status` | Current config, paused state, DB row count |
| `/pause` `/resume` | Toggle the poll loop |
| `/interval N` | Set polling interval in seconds (≥ 60) |
| `/filter` | **The main UI** — inline menu for brand / models / body / price / year / interval, with single-tap presets and multi-select toggles |
| `/setbrand <slug>` | Set a brand not in the hardcoded catalog (e.g. `/setbrand alfa-romeo`) |
| `/clear` + `/clear_confirm` | Wipe the seen-listings dedup table |
| `/dump N` | Show the last N saved listings as a compact list |
| `/cancel` | Informational — there's nothing to cancel, this is the answer |

All state-changing commands require the sender's Telegram user-id to be in
`AUTHORIZED_USER_ID` from `.env` (a single id or a comma-separated list).
Everyone else's input is logged and dropped.

## Architecture

### Runtime — every poll cycle

```
                                                  ┌────────────────────────────┐
                                                  │ Cloudflare Worker          │
   ┌──────────────────────────────────────────┐   │ nau-proxy.<you>.workers.dev│
   │                                          │   │                            │
   │  Bot on Linux VM (systemd)               │   │ • whitelisted by CF        │
   │  ┌────────────────────────────────────┐  │   │ • forwards GET requests    │
   │  │ poll loop (every N min)            │──┼──►│ • auth via x-proxy-secret  │
   │  │   • snapshot RuntimeConfig         │  │   │                            │
   │  │   • curl through CF Worker         │  │   └─────────────┬──────────────┘
   │  │   • parse HTML (CSS selectors)     │  │                 │
   │  │   • dedup vs SQLite seen_listings  │  │                 ▼
   │  │   • send new ones to TG            │  │   ┌────────────────────────────┐
   │  │   • mark seen ONLY after send      │  │   │ polovniautomobili.com      │
   │  └────────────────────────────────────┘  │   │ (behind Cloudflare)        │
   │                                          │   └────────────────────────────┘
   │  ┌────────────────────────────────────┐  │
   │  │ command listener (teloxide)        │  │   ┌────────────────────────────┐
   │  │   • /filter wizard                 │◄─┼──►│ Telegram                   │
   │  │   • /pause /resume /interval ...   │  │   │   • bot (commands)         │
   │  │   • persists changes to SQLite     │  │   │   • channel (subscribers)  │
   │  └────────────────────────────────────┘  │   └────────────────────────────┘
   │                                          │
   │  state:                                  │
   │  • RuntimeConfig in Arc<RwLock<…>>       │
   │  • SQLite (seen_listings + runtime_settings)
   │  • HTML dumps under ./dumps/YYYY-MM-DD/  │
   └──────────────────────────────────────────┘
```

**Why a CF Worker proxy?**
polovniautomobili.com is behind Cloudflare Managed Challenge. Direct HTTP
requests from a Linux network stack (most VMs/VPSes) get challenged and
return 403, even with curl-impersonate. macOS curl passes; Linux curl
doesn't — the differentiator is below the TLS layer (kernel TCP fingerprint,
probably). Routing fetches through a Cloudflare Worker side-steps the
problem entirely: CF doesn't challenge requests from its own infrastructure.
The Worker is a ~30-line JavaScript file in [`cf-proxy/`](cf-proxy/),
free tier covers 100k requests/day (we use ~150).

### Build & release — when you tag

```
       Mac (dev)                  GitHub Actions             GitHub Releases
   ┌──────────────┐  push       ┌────────────────────┐    ┌────────────────┐
   │ code, tests, │  to main    │  ci.yml            │    │                │
   │ commits      │────────────►│  fmt+clippy+test   │    │                │
   │              │             │  (no artifact)     │    │                │
   │              │             └────────────────────┘    │                │
   │              │                                       │                │
   │ git tag v*   │             ┌────────────────────┐    │                │
   │ git push     │  push        │  release.yml       │    │ v0.1.0         │
   │   v0.1.0    ─┼────────────►│  build x86_64-     │───►│ ↘ "latest"    │
   │              │  the tag    │  unknown-linux-gnu │    │ + binary asset │
   └──────────────┘             │  strip + publish   │    │                │
                                └────────────────────┘    └────────┬───────┘
                                                                   │
                                                                   │ curl
                                                                   ▼
                                                          ┌────────────────┐
                                                          │ Linux VM       │
                                                          │ deploy/update.sh │
                                                          │ → systemctl    │
                                                          │   restart      │
                                                          └────────────────┘
```

**Why tag-triggered, not push-triggered?**
`main` may have WIP / refactor / typo-fix commits between releases. Only
explicit semver tags (`v0.1.0`, `v0.2.0`, …) should ship to prod. Every
commit still gets tested via `ci.yml`; only tagged commits get a binary.

### Modules

Modules live flat under `src/`; a module is promoted to a directory with
`mod.rs` when it crosses ~300 lines or splits naturally — `commands/` is the
one that has (per #24).

| Module | Responsibility |
| --- | --- |
| `main.rs` | Entry — load env, init tracing, spawn poll + command loops, then supervise them via `tokio::select!` |
| `lib.rs` | Library facade re-exporting the modules so integration tests can link against them |
| `config.rs` | `StaticConfig` (env-only) + `RuntimeConfig` (env defaults + DB overrides) + `ProxyConfig` |
| `models.rs` | Shared types — `Listing`, `SearchFilter`, `ShowOldNew` |
| `scraper.rs` | curl shell-out (optional CF Worker proxy) + HTML parsing via CSS selectors |
| `storage.rs` | SQLite via `rusqlite` — `seen_listings` (dedup) + `runtime_settings` (user config) |
| `telegram.rs` | teloxide-based send-only client (`format_listing_html`, `escape_html`) |
| `commands/` | teloxide dispatcher (directory): `mod.rs` (routing/auth), `catalog.rs` (brands/models/body-type data), `handlers.rs` (per-command handlers + `apply_*` + formatters), `keyboards.rs` (inline keyboards + callback-data constants) |
| `signals.rs` | SIGINT/SIGTERM handler shared between both loops |
| `version.rs` | Compile-time `VERSION` string (`CARGO_PKG_VERSION` + git SHA from `build.rs`) |
| `bot.rs` | The poll loop — fetch / dedup / send + zero-streak detector + dump rotation |

## Getting started

### Quick local run (Mac/Linux dev)

```bash
git clone https://github.com/ignatenkofi/njuska-auto-bot
cd njuska-auto-bot

cp .env.example .env
# Edit .env — at minimum:
#   TELEGRAM_BOT_TOKEN (from @BotFather)
#   TELEGRAM_CHAT_ID   (your user id from @userinfobot, or a channel id)
#   AUTHORIZED_USER_ID (your user id, or a comma-separated list of ids
#                       allowed to issue commands)

cargo run --release
```

On Mac the direct fetch works without the CF Worker. On Linux you'll need
the Worker — see [Production deployment](#production-deployment) below.

Then in Telegram:
1. Open your bot, send `/start` once (so the bot is allowed to message you).
2. Send `/filter` and tap your way through the wizard.

### Production deployment

For a long-running deployment on a Linux server (Proxmox VM, Hetzner VPS,
Raspberry Pi, anything systemd-capable):

➡ **Follow [deploy/DEPLOY.md](deploy/DEPLOY.md)** end-to-end.

Highlights:
- The VM needs only `curl` + `git`. No Rust toolchain — binary comes from
  GitHub Releases.
- 5-min CF Worker setup ([cf-proxy/README.md](cf-proxy/README.md)) for the
  Cloudflare bypass.
- systemd unit with hardening (`NoNewPrivileges`, `ProtectSystem=strict`,
  `MemoryMax=200M`, syscall filter).
- Dedicated `njuska` user. State (`.env`, `njuska.db`, `dumps/`) under
  `/opt/njuska-auto-bot/` for easy backup.

### Updating a deployment

```bash
sudo bash /opt/njuska-auto-bot/src/deploy/update.sh
```

The script downloads the latest binary from
`releases/latest/download/njuska_auto_bot`, atomically swaps it in, restarts
the systemd service, and tails recent logs. Idempotent — re-run any time.

### Telegram channel mode

To broadcast to a channel instead of a personal chat:
1. Create a channel in Telegram.
2. Add your bot as admin (with "Post Messages" permission).
3. Find the channel's numeric chat id (starts with `-100`). Easy way: add
   @userinfobot as admin temporarily, it'll print the id.
4. Set `TELEGRAM_CHAT_ID=-100xxxxxxxxxx` in `.env`.
5. Leave `AUTHORIZED_USER_ID` as **your** user id — or a comma-separated
   list of ids (commands still come from people, not the channel).

## Configuration

All knobs live in `.env`. See `.env.example` for the full annotated list.

The split:

- **Static** (env-only, set once): TG token, chat id, authorized user, DB
  path, dumps path, zero-results threshold, CF Worker proxy URL+secret.
- **Runtime** (env *defaults*, overridden via `/filter`/`/interval`/`/pause`):
  search filter, poll interval, paused state. Stored in SQLite so settings
  survive restarts.

## Development

```bash
cargo build
cargo test                                  # 105 unit + 7 integration tests
cargo clippy --all-targets -- -D warnings   # must be clean
cargo fmt
RUST_LOG=njuska_auto_bot=debug cargo run    # verbose logs while developing
```

CI on GitHub Actions does all of these (plus `cargo build --release`) on
every push to main and on every PR. See [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

### Cutting a release

```bash
git tag v0.2.0
git push origin v0.2.0
# release.yml builds a stripped Linux binary on ubuntu-22.04 and publishes
# it to a GitHub Release named after the tag. The `releases/latest/...`
# redirect now points at v0.2.0.
```

For pre-release versions, use `v0.2.0-rc1` or similar — same workflow,
same publish, but you'd mark the Release as a pre-release in the UI if
you want it excluded from the `releases/latest/` redirect.

## Tech notes

A few decisions worth knowing about, in case you read the source:

- **Why curl, not reqwest, for polovni?** polovniautomobili.com is behind
  Cloudflare Managed Challenge that fingerprints `reqwest+hyper` and
  responds with 403, regardless of HTTP version, TLS backend, or headers.
  Bare `curl --http1.1` from macOS passes cleanly. We shell out per-fetch
  (~10 ms; invisible at a 10-min poll cadence). teloxide for the Telegram
  API still uses `reqwest` — no CF in front of `api.telegram.org`.
- **Why a CF Worker for Linux deploys?** Linux curl gets 403 too (the
  differentiator is below TLS — kernel TCP fingerprint, probably). The
  Worker side-steps it by originating the request from CF's own infra,
  which CF doesn't challenge.
- **No headless browser.** Listings are server-rendered HTML; CSS selectors
  do the trick.
- **Mutex-wrapped SQLite Connection.** `rusqlite::Connection` is `Send`
  but not `Sync`, which makes `Arc<Storage>` not-`Send` for `tokio::spawn`
  without a `Mutex`. Critical sections are microseconds; executor never
  notices.

## Contributing

PRs welcome. The bot is intentionally minimal — if you want to add a new
filter section, follow the pattern of an existing one in the `commands/`
module (brand picker for single-select, chassis picker for multi-select, range
picker for from/to ranges). See `CONTRIBUTING.md` for the step-by-step.

If polovniautomobili.com changes its HTML structure and the parser breaks,
the bot sends a "0 listings N times in a row" alert. The HTML dumps in
`./dumps/YYYY-MM-DD/` are saved exactly for that case — open one, find the
new selectors, update `src/scraper.rs`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
