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
sudo apt install -y \
    build-essential \
    curl \
    git \
    pkg-config \
    libssl-dev
```

Install Rust via [rustup](https://rustup.rs):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version    # should report 1.95+
```

Verify `curl` is on PATH (it should be — the bot shells out to it):

```bash
which curl
```

## Step 3 — Create a dedicated user

We run the bot as its own user (`njuska`) so a bug in the service can't
touch anything outside its own dir. No login shell needed.

```bash
sudo useradd --system --create-home --home-dir /opt/njuska-auto-bot --shell /usr/sbin/nologin njuska
```

## Step 4 — Clone & build

```bash
sudo -u njuska -H bash <<'EOF'
cd /opt/njuska-auto-bot
git clone https://github.com/filipp-ignatenko/njuska-auto-bot.git src
cd src
cargo build --release
cp target/release/njuska_auto_bot ..
EOF
```

After this `/opt/njuska-auto-bot/njuska_auto_bot` is the binary the service
will run.

> Note: building on the VM is the simplest path. If you'd rather build
> locally and `scp` the binary, that works too — just match the target
> triple (`x86_64-unknown-linux-gnu` on most Proxmox VMs). With `--release`
> the binary is statically-linked enough to portable across modern Debians.

## Step 5 — Configure `.env`

```bash
sudo -u njuska cp /opt/njuska-auto-bot/src/.env.example /opt/njuska-auto-bot/.env
sudo -u njuska nano /opt/njuska-auto-bot/.env
```

Set at minimum:

- `TELEGRAM_BOT_TOKEN` — from @BotFather
- `TELEGRAM_CHAT_ID` — your user id, OR `-100xxxxxxxxxx` for a channel
- `AUTHORIZED_USER_ID` — your user id (commands only)

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

# Upgrade — pull, rebuild, restart
sudo -u njuska -H bash <<'EOF'
cd /opt/njuska-auto-bot/src
git pull
cargo build --release
cp -f target/release/njuska_auto_bot ..
EOF
sudo systemctl restart njuska-auto-bot
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

**403 from polovniautomobili.com** — Cloudflare may have tightened its
fingerprinting. Open one of the failing dumps (none yet at this stage, but
the bot logs the request URL), reproduce the failure with `curl --http1.1`,
adjust the UA string in `src/scraper.rs` if needed.
