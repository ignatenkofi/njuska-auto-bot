# Deployment: Proxmox VM + systemd

This walks through running NjuskaAutoBot as a long-lived service on a Linux
VM, hosted in Proxmox (but the steps work for any Debian/Ubuntu host).

The bot is *tiny* — 50 MB RAM idle, one HTTP request every ~10 minutes, a
few MB of disk for SQLite + HTML dumps. The smallest VM you can spin up
will be massively over-provisioned. **1 vCPU, 512 MB RAM, 5 GB disk** is
overkill but cheap.

## Step 1 — Create the VM in Proxmox

Defaults aside, pick:
- **OS**: Debian 12 (bookworm) or Ubuntu 22.04+ — anything with systemd
- **vCPU**: 1
- **RAM**: 512 MB (or 256 MB; really doesn't matter)
- **Disk**: 5 GB (the SQLite file grows at maybe 1 KB per listing, so 5 GB
  fits hundreds of thousands of dedup rows)
- **Network**: bridged onto your LAN, give it a static IP via DHCP
  reservation so you can SSH reliably

Install the OS as usual.

## Step 2 — Install prerequisites on the VM

SSH into the VM as a sudo-capable user:

```bash
ssh you@<vm-ip>
sudo apt update
sudo apt install -y curl git
```

That's the entire runtime dependency list. The bot binary itself is
statically-linked against rusqlite (bundled SQLite) and rustls (no system
OpenSSL), so the only thing it needs from the OS is `curl` (for fetching)
and libc.

> **No Rust toolchain on the VM.** We build the binary on GitHub Actions
> and the VM just downloads it from the `nightly` release. Saves a few GB of
> disk and a few minutes per upgrade.

## Step 3 — Create a dedicated user

We run the bot as its own user (`njuska`) so a bug in the service can't
touch anything outside its own dir. No login shell needed.

```bash
sudo useradd --system --create-home --home-dir /opt/njuska-auto-bot --shell /usr/sbin/nologin njuska
```

## Step 4 — Download the latest binary

```bash
sudo -u njuska curl -L --fail \
    https://github.com/ignatenkofi/njuska-auto-bot/releases/download/nightly/njuska_auto_bot \
    -o /opt/njuska-auto-bot/njuska_auto_bot

sudo -u njuska chmod +x /opt/njuska-auto-bot/njuska_auto_bot

# Sanity-check
/opt/njuska-auto-bot/njuska_auto_bot --version 2>/dev/null || \
    file /opt/njuska-auto-bot/njuska_auto_bot
```

`releases/download/nightly/...` is a rolling URL — it always points at the
latest binary built from `main`. GitHub Actions rebuilds and updates that
release on every push that touches Rust source.

You also need a local copy of the repo (for `deploy/update.sh`, `.env.example`,
and the systemd unit). Lightweight clone — no toolchain involved:

```bash
sudo -u njuska git clone https://github.com/ignatenkofi/njuska-auto-bot.git /opt/njuska-auto-bot/src
```

## Step 4.5 — Set up the Cloudflare Worker proxy (REQUIRED for Linux VMs)

polovniautomobili.com is behind Cloudflare. Direct HTTP fetches from a Linux
network stack are challenged and return 403 — even with `curl-impersonate`.
The workaround is a tiny CF Worker we host that proxies the fetch from CF's
own infrastructure (which CF doesn't challenge).

**On your laptop (not the VM)**, follow [cf-proxy/README.md](../cf-proxy/README.md).
It takes about 5 minutes:

1. Install `wrangler` (`brew install node && npm install -g wrangler`), `wrangler login`
2. From the `cf-proxy/` directory: generate a random secret and `wrangler secret put PROXY_SECRET`
3. `wrangler deploy`
4. Note the URL it prints + the secret

You'll plug those two values into the VM's `.env` in the next step.

## Step 5 — Configure `.env`

```bash
sudo -u njuska cp /opt/njuska-auto-bot/src/.env.example /opt/njuska-auto-bot/.env
sudo -u njuska nano /opt/njuska-auto-bot/.env
```

Set at minimum:

- `TELEGRAM_BOT_TOKEN` — from @BotFather
- `TELEGRAM_CHAT_ID` — your user id, OR `-100xxxxxxxxxx` for a channel
- `AUTHORIZED_USER_ID` — your user id (commands only)
- `CF_PROXY_URL` — Worker URL from step 4.5
- `CF_PROXY_SECRET` — Worker secret from step 4.5

The search filters can be left empty here — you'll configure them through
the bot's `/filter` wizard.

```bash
sudo chmod 600 /opt/njuska-auto-bot/.env
sudo chown njuska:njuska /opt/njuska-auto-bot/.env
```

The 600 permission keeps the bot token out of any other user's reach.

## Step 6 — Install the systemd unit

```bash
sudo install -m 0644 \
    /opt/njuska-auto-bot/src/deploy/njuska-auto-bot.service \
    /etc/systemd/system/njuska-auto-bot.service

sudo systemctl daemon-reload
sudo systemctl enable --now njuska-auto-bot
sudo systemctl status njuska-auto-bot
```

You should see `active (running)` and a few "starting" log lines.

## Step 7 — Verify in Telegram

1. Open your bot in TG, send `/start` once (so it can reply to you).
2. Send `/status` — you should get the configuration dump.
3. Send `/filter` — the wizard should open.

## Operations

```bash
# Follow logs in real time
sudo journalctl -u njuska-auto-bot -f

# Show recent logs only
sudo journalctl -u njuska-auto-bot -n 100 --no-pager

# Restart after editing .env
sudo systemctl restart njuska-auto-bot

# Stop / start
sudo systemctl stop njuska-auto-bot
sudo systemctl start njuska-auto-bot

# Disable autostart on boot
sudo systemctl disable njuska-auto-bot

# Upgrade — pull new binary, restart
sudo /opt/njuska-auto-bot/src/deploy/update.sh

# (Equivalent manually:)
# sudo systemctl stop njuska-auto-bot
# sudo -u njuska curl -L --fail \
#     https://github.com/ignatenkofi/njuska-auto-bot/releases/download/nightly/njuska_auto_bot \
#     -o /opt/njuska-auto-bot/njuska_auto_bot
# sudo -u njuska chmod +x /opt/njuska-auto-bot/njuska_auto_bot
# sudo systemctl start njuska-auto-bot
```

## Backup

Everything stateful lives in `/opt/njuska-auto-bot`:
- `.env` — config + secrets
- `njuska.db` — dedup DB + runtime settings
- `dumps/` — optional, auto-rotated, regenerable

A nightly `tar czf njuska-$(date +%F).tgz /opt/njuska-auto-bot/{.env,njuska.db}`
covers everything you can't reproduce from git.

## Troubleshooting

**Service won't start** — `journalctl -u njuska-auto-bot -n 30 --no-pager`
will print the panic / error. Common culprits:
- `AUTHORIZED_USER_ID is required` → fill it in `.env`
- `failed to invoke curl: ...` → `apt install curl`
- `failed to open SQLite at ...` → check ownership of `/opt/njuska-auto-bot/njuska.db`

**Service runs but TG doesn't get messages** — check the bot has `/start`
been sent to it by the configured user/chat. Telegram blocks bots from
initiating conversations.

**403 from polovniautomobili.com** — most commonly, you skipped Step 4.5
(CF Worker proxy) or the `CF_PROXY_URL`/`CF_PROXY_SECRET` env vars aren't
set correctly. Check `journalctl -u njuska-auto-bot -n 50` for a line like
`fetching search page via curl url=… via_proxy=true`. If `via_proxy=false`,
the bot is fetching directly and CF is challenging it.

If `via_proxy=true` but still 403, the Worker itself is returning the 403
(secret mismatch). Verify `CF_PROXY_SECRET` matches what's set in the Worker
via `wrangler secret put PROXY_SECRET`.
