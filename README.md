# NjuskaAutoBot

A Telegram bot, written in Rust, that watches
[polovniautomobili.com](https://www.polovniautomobili.com/) — Serbia's largest
used-car classifieds site — for new listings matching your filters and forwards
them to a Telegram chat or channel.

Built as a personal bot but works fine for friend groups or small expat
communities: invite the bot to a channel as admin, configure filters via the
bot itself, and everyone subscribed sees new MINI / Golf / BMW / whatever
listings the moment they appear.

## Features

- **Long-running poll loop.** Every N minutes (default 10), fetches the search
  page, parses listings, dedups against a local SQLite file, sends new ones to
  Telegram with a preview card per listing.
- **Configuration through the bot itself.** No restart, no `.env` editing.
  `/filter` opens an interactive menu with inline keyboards for brand, models,
  body type (multi-select), price range, year range, and poll interval.
  Everything persists across restarts.
- **Resilient.** Network blips, Cloudflare hiccups, parser glitches — all
  logged and retried, never crash the process. A "zero listings N times in a
  row" detector pings you in TG when the site's HTML changes and selectors go
  stale.
- **Forensic-friendly.** Every fetched HTML page is saved to `./dumps/` (with
  daily rotation) so you can replay a broken parser against the exact snapshot.
- **Mark-after-send semantics.** If Telegram is down, unsent listings stay in
  the "unseen" set and retry next poll — no lost notifications.

## Commands

| Command | What it does |
| --- | --- |
| `/start` `/help` | Welcome + commands list |
| `/status` | Current config, paused state, DB row count |
| `/pause` `/resume` | Toggle the poll loop |
| `/interval N` | Set polling interval in seconds (≥ 60) |
| `/filter` | **The main UI** — inline menu for brand / models / body / price / year / interval, with single-tap presets and multi-select toggles |
| `/setbrand <slug>` | Set a brand not in the catalog (e.g. `/setbrand alfa-romeo`) |
| `/clear` + `/clear_confirm` | Wipe the seen-listings dedup table (e.g. after changing filters significantly) |
| `/dump N` | Show the last N saved listings as a compact list |
| `/cancel` | Informational — there's nothing to cancel, this is the answer |

All commands except `/start`, `/help`, `/cancel` are restricted to the
`AUTHORIZED_USER_ID` Telegram user; everyone else's input is silently dropped.

## Architecture

```
                              ┌─────────────────────────────┐
                              │  Telegram                   │
                              │  bot + channel + commands   │
                              └──┬──────────────────────▲───┘
                                 │   getUpdates         │ sendMessage
                                 │   (teloxide)         │ (teloxide)
                                 ▼                      │
   ┌───────────────────────────────────────────────────┐│
   │  command dispatcher  │   poll loop                ││
   │  (commands.rs)       │   (bot.rs)                 ││
   │                      │                            ││
   │  /filter wizard ─────┼──► RuntimeConfig (RwLock) ─┼┘
   │                      │           │
   │                      │           ▼
   │                      │   scraper (curl + selectors)
   │                      │           │
   │                      │           ▼
   │                      │   storage (SQLite)
   │                      │      • seen_listings (dedup)
   │                      │      • runtime_settings (user config)
   └──────────────────────┴────────────────────────────┘
```

### Modules

| File | Responsibility |
| --- | --- |
| `main.rs` | Entry — load env, init tracing, spawn poll + command loops, `tokio::join!` |
| `config.rs` | `StaticConfig` (env-only) + `RuntimeConfig` (env defaults overridden by DB) |
| `models.rs` | Shared types — `Listing`, `SearchFilter`, `ShowOldNew` |
| `scraper.rs` | curl shell-out (CF bypass) + HTML parsing via CSS selectors |
| `storage.rs` | SQLite via `rusqlite` — dedup + runtime settings |
| `telegram.rs` | teloxide-based send-only client |
| `commands.rs` | teloxide dispatcher — commands + inline keyboards + callback routing |
| `signals.rs` | SIGINT/SIGTERM handler shared between both loops |
| `bot.rs` | The poll loop — fetch / dedup / send + zero-streak detector + dump rotation |

## Getting started

### Prerequisites

- Rust 1.95 or newer
- `curl` in `PATH` (preinstalled on macOS, Linux, Windows 10+)
- A Telegram bot from [@BotFather](https://t.me/BotFather)
- Your Telegram user ID — message [@userinfobot](https://t.me/userinfobot) and
  it'll reply with your numeric ID

### Setup

```bash
git clone https://github.com/<you>/njuska-auto-bot
cd njuska-auto-bot

cp .env.example .env
# Edit .env — at minimum: TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID, AUTHORIZED_USER_ID

cargo run --release
```

Then in Telegram:
1. Open your bot, send `/start` once (so the bot is allowed to message you).
2. Send `/filter` and tap your way through the wizard.

### Telegram channel mode

To broadcast to a channel instead of a personal chat:
1. Create a channel in Telegram.
2. Add your bot as admin (with "Post Messages" permission).
3. Find the channel's numeric chat id (starts with `-100`). Easy way: add
   @userinfobot as admin temporarily, it'll print the id.
4. Set `TELEGRAM_CHAT_ID=-100xxxxxxxxxx` in `.env`.
5. Leave `AUTHORIZED_USER_ID` as **your** user id (commands still come from a
   person, not the channel).

## Configuration

All knobs live in `.env`. See `.env.example` for the full annotated list.

The split:

- **Static** (env-only, set once): TG token, chat id, authorized user, DB
  path, dumps path, zero-results threshold.
- **Runtime** (env *defaults*, overridden by `/filter` & friends): search
  filter, poll interval, paused state. Stored in SQLite so settings survive
  restarts.

## Development

```bash
cargo build
cargo test                                  # 37 unit + integration tests
cargo clippy --all-targets -- -D warnings   # must be clean
cargo fmt
RUST_LOG=njuska_auto_bot=debug cargo run    # verbose logs while developing
```

## Tech notes

A few decisions worth knowing about, in case you read the source:

- **Why curl, not reqwest?** polovniautomobili.com is behind Cloudflare
  Managed Challenge that fingerprints `reqwest+hyper` and responds with 403,
  regardless of HTTP version, TLS backend, or headers. Bare `curl --http1.1`
  from the same machine passes cleanly. We shell out per-fetch
  (~10 ms; invisible at a 10-minute poll cadence). teloxide for the
  Telegram API still uses `reqwest` — no CF in front of `api.telegram.org`.
- **No headless browser.** Listings are server-rendered HTML; CSS selectors
  do the trick.
- **Mutex-wrapped SQLite Connection.** `rusqlite::Connection` is `Send`
  but not `Sync`, which makes `Arc<Storage>` not-`Send` for `tokio::spawn`
  without a `Mutex`. Critical sections are microseconds; executor never
  notices.

## Contributing

PRs welcome. The bot is intentionally minimal — if you want to add a new
filter section, follow the pattern of an existing one in `commands.rs`
(brand picker for single-select, chassis picker for multi-select, range
picker for from/to ranges).

If polovniautomobili.com changes its HTML structure and the parser breaks,
the bot will send a "0 listings N times in a row" alert. The HTML dumps in
`./dumps/YYYY-MM-DD/` are saved exactly for that case — open one, find the
new selectors, update `src/scraper.rs`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
