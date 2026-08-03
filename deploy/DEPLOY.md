# Deployment: Proxmox VM + systemd

This walks through running NjuskaAutoBot as a long-lived service on a Linux
VM, hosted in Proxmox (but the steps work for any Debian/Ubuntu host).

The bot is *tiny* — 50 MB RAM idle, one HTTP request every ~10 minutes, a
few MB of disk for SQLite + HTML dumps. The smallest VM you can spin up
will be massively over-provisioned. **1 vCPU, 512 MB RAM, 5 GB disk** is
overkill but cheap.

## Step 1 — Create the VM

### From code (preferred)

Run **on the Proxmox node**:

```bash
git clone --depth 1 https://github.com/ignatenkofi/njuska-auto-bot /tmp/njuska
sudo bash /tmp/njuska/deploy/provision-vm.sh
```

That creates the VM from a Debian 12 cloud image and hands it
`deploy/cloud-init/njuska-vm.yaml`, which does Steps 2, 3, 4 and 6 of this
document on first boot: packages, the `njuska` user, the release binary
(with a `--version` preflight), and the systemd units. Steps 5 and 7 stay
manual — see below.

Tunables are environment variables; defaults are in the script's header.

**Where the VM lands, and why.** The defaults put it on VLAN 41 at
`192.168.41.200/24` — the segment that hosts the test polygon. That looks
odd for a production service until you compare the alternatives on this
network: VLAN 10 is `vlan10_MGMT`, home *and* hardware management sharing
one L2 (all three routers plus the hypervisor's admin interface), and this
bot parses HTML from a public site, i.e. it digests input an attacker
controls. VLAN 41 is the only existing segment that is deny-by-default, and
what it permits outbound — 443, 53, 123 — is exactly this bot's appetite.
The untrusted-workload segment that *ought* to hold it (VLAN 20) does not
exist yet.

Two consequences the script handles for you:

- **Static addressing.** VLAN 41 has no DHCP; the address comes from the
  polygon's plan (`.200`, the start of its reserved range) via
  `--ipconfig0`, and DNS points at public resolvers because the router's
  own resolver is unreachable from that segment by design.
- **Second echelon.** The polygon's VMs get the `polygon` PVE security
  group on their NIC from OpenTofu. This VM is not OpenTofu-managed, so the
  script attaches the same group by hand — otherwise it would sit in the
  polygon's segment without the polygon's protection. If the group is
  missing the script says so loudly rather than proceeding unprotected.

Also note that `apt` inside that segment must use HTTPS mirrors — port 80
is not permitted outbound — which the cloud-init config sets up before the
first `apt` call.

To deploy somewhere else, override: `VLAN_TAG=`, `IP_CIDR=`, `SEC_GROUP=`.

**Why this exists.** Until 2026-07 the VM was hand-built and therefore
un-restorable: when the hypervisor was wiped, the bot went with it and
nothing in the repo described how to get it back (#70). A VM you cannot
recreate from the repository is inventory, not deployment.

**The one thing cloud-init will not do is secrets.** On Proxmox the
user-data lives as a snippet in a datastore and is readable by anyone with
access to that storage, so the Telegram token and the CF Worker secret must
not go near it. They arrive on the running VM in Step 5.

The unit is `enable`d but not started on first boot: without `.env` the bot
exits immediately and systemd would restart-loop it every 10 seconds,
filling the journal before you have a chance to place the file. Future
reboots start it normally.

### By hand (fallback)

If you are deploying somewhere other than Proxmox — a VPS, a Raspberry Pi,
anything systemd-capable — build the box yourself and continue from Step 2:

- **OS**: Debian 12 (bookworm) or Ubuntu 22.04+ — anything with systemd
- **vCPU**: 1
- **RAM**: 512 MB (or 256 MB; really doesn't matter)
- **Disk**: 5 GB (the SQLite file grows at maybe 1 KB per listing, so 5 GB
  fits hundreds of thousands of dedup rows)
- **Network**: bridged onto your LAN, give it a static IP via DHCP
  reservation so you can SSH reliably

## Step 2 — Install prerequisites on the VM

SSH into the VM as a sudo-capable user:

```bash
ssh you@<vm-ip>
sudo apt update
sudo apt install -y curl git
```

That's almost the entire runtime dependency list. The bot binary statically
links rusqlite (bundled SQLite), so there's no `libsqlite3` to install. Its
Telegram HTTP stack, however, comes from teloxide → reqwest with the default
`native-tls` backend, so the binary *does* dynamically link the system's
`libssl`/`libcrypto` (OpenSSL). Debian 12's base install already ships
`libssl3` — `apt` itself depends on it — so there's nothing extra to install,
but `ldd` will show OpenSSL alongside libc, not libc alone. Beyond that the
bot needs only `curl` (for fetching polovni).

> **No Rust toolchain on the VM.** We build the binary on GitHub Actions
> and the VM just downloads it from the latest tagged release. Saves a few GB
> of disk and a few minutes per upgrade.

**Downloaded binaries are checksum-verified before they are made
executable.** The release publishes `SHA256SUMS` alongside the binary;
`update.sh` and the cloud-init config both check it and refuse to install on
a mismatch, a missing entry, or a missing sums file.

Be clear about what that buys: it catches truncated downloads, a CDN serving
the wrong bytes, and `latest` resolving to an asset that does not match its
own release. It does **not** make the binary tamper-proof — the sums file
travels with the artifact, so whoever can replace one can replace both. That
needs a signature checked against a key from somewhere else, which is tracked
in devsecops-pipeline#18.

Releases tagged before checksums existed have no `SHA256SUMS`. `update.sh`
fails closed on those; `ALLOW_UNVERIFIED=1` re-runs without the check and
exists only until the oldest release anyone still installs carries one.

## Step 3 — Create a dedicated user

We run the bot as its own user (`njuska`) so a bug in the service can't
touch anything outside its own dir. No login shell needed.

```bash
sudo useradd --system --create-home --home-dir /opt/njuska-auto-bot --shell /usr/sbin/nologin njuska
```

## Step 4 — Download the latest binary

```bash
cd /opt/njuska-auto-bot

sudo -u njuska curl -L --fail \
    https://github.com/ignatenkofi/njuska-auto-bot/releases/latest/download/njuska_auto_bot \
    -o njuska_auto_bot

sudo -u njuska curl -L --fail \
    https://github.com/ignatenkofi/njuska-auto-bot/releases/latest/download/SHA256SUMS \
    -o SHA256SUMS

sha256sum -c SHA256SUMS && sudo -u njuska rm -f SHA256SUMS

sudo -u njuska chmod +x njuska_auto_bot

# Sanity-check
./njuska_auto_bot --version 2>/dev/null || file njuska_auto_bot
```

Verify first, `chmod +x` second — same order the scripted paths use, and for
the same reason: an unverified file should not become executable. If
`sha256sum -c` fails, stop; do not chmod anything.

`releases/latest/download/...` is GitHub's canonical "newest release" URL —
it redirects to whichever tagged version was most recently published. To
pin to a specific version, use `releases/download/v0.2.0/njuska_auto_bot`
instead.

Releases are produced only when a `v*` tag is pushed (e.g. `git tag v0.2.0
&& git push origin v0.2.0`). Commits to `main` run tests but **do not**
publish binaries — the separation lets unfinished work coexist on `main`
without leaking into prod.

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

# (Equivalent manually — note the checksum step: it is not optional, and
#  this block is read exactly when the scripted path already failed.)
# R=https://github.com/ignatenkofi/njuska-auto-bot/releases/latest/download
# D=/opt/njuska-auto-bot/njuska_auto_bot
# sudo systemctl stop njuska-auto-bot
# # Download to a sibling so a failed download cannot trash the running binary
# sudo -u njuska curl -L --fail "$R/njuska_auto_bot" -o "$D.new"
# sudo -u njuska curl -L --fail "$R/SHA256SUMS"     -o "$D.sums"
# # Verify BEFORE chmod +x — an unverified file must not become executable
# ( cd /opt/njuska-auto-bot \
#   && awk '$2 ~ /^\*?njuska_auto_bot$/ {print $1"  njuska_auto_bot.new"}' \
#        njuska_auto_bot.sums | sha256sum -c - ) || {
#     echo "checksum mismatch or missing entry — refusing to install" >&2
#     sudo -u njuska rm -f "$D.new" "$D.sums"; exit 1; }
# sudo -u njuska rm -f "$D.sums"
# sudo -u njuska chmod +x "$D.new"
# sudo -u njuska mv "$D.new" "$D"
# sudo systemctl start njuska-auto-bot
```

## Backup & restore

Everything stateful lives in `/opt/njuska-auto-bot`:
- `.env` — config + secrets
- `njuska.db` — dedup DB + runtime settings
- `dumps/` — optional, auto-rotated, regenerable

### Automatic daily backups

`deploy/njuska-backup.service` + `.timer` snapshot the DB and `.env` into
`/opt/njuska-auto-bot/backups/` every night at 04:30 and keep the newest
**14 copies** of each. The DB snapshot goes through `sqlite3 .backup` —
SQLite's own backup API — so it's **consistent even while the bot is
running mid-transaction**; a plain `cp` of a live DB can capture a torn
page.

Install:

```bash
sudo apt install -y sqlite3
sudo install -m 0644 \
    /opt/njuska-auto-bot/src/deploy/njuska-backup.service \
    /opt/njuska-auto-bot/src/deploy/njuska-backup.timer \
    /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now njuska-backup.timer

# Verify: next scheduled run + take one backup right now
systemctl list-timers njuska-backup.timer
sudo systemctl start njuska-backup.service
ls -la /opt/njuska-auto-bot/backups/
```

Backups are local to the VM. For real disaster tolerance, also pull them
off-box occasionally (Proxmox VM snapshots, or an rsync of `backups/` to
your workstation).

### Restore

```bash
# 1. Stop the bot so nothing writes to the DB mid-restore.
sudo systemctl stop njuska-auto-bot

# 2. Put the chosen snapshot back (pick the date you want).
sudo -u njuska cp /opt/njuska-auto-bot/backups/njuska-2026-07-01.db \
                  /opt/njuska-auto-bot/njuska.db

# 3. If .env was lost too:
sudo -u njuska cp /opt/njuska-auto-bot/backups/env-2026-07-01 \
                  /opt/njuska-auto-bot/.env
sudo chmod 600 /opt/njuska-auto-bot/.env

# 4. Start and check.
sudo systemctl start njuska-auto-bot
sudo journalctl -u njuska-auto-bot -n 20 --no-pager
```

Restoring an older DB means the bot forgets listings first seen *after*
that snapshot — expect a one-time burst of re-notifications for anything
still live on the site.

## Rotating the CF Worker secret

Rotate `CF_PROXY_SECRET` whenever it may have leaked (pasted in a chat,
committed by accident, left in a terminal scrollback) — or just
periodically; it's a two-minute operation.

**On your laptop** (where `wrangler` is set up), from the repo:

```bash
cd cf-proxy
./rotate-secret.sh
```

The script generates a fresh secret (`openssl rand -hex 32`), pushes it to
the Worker via `wrangler secret put PROXY_SECRET`, and prints the new value
with follow-up steps. **From that moment the Worker rejects the old secret**,
so finish the rotation on the VM promptly:

```bash
sudo -u njuska nano /opt/njuska-auto-bot/.env    # set CF_PROXY_SECRET=<new value>
sudo systemctl restart njuska-auto-bot
sudo journalctl -u njuska-auto-bot -n 20 --no-pager
```

The log must show `CF Worker proxy probe OK`. If you mistype the secret,
the bot refuses to start with an explicit `CF_PROXY_SECRET` hint (startup
health-check) — fix `.env` and restart. The few polls missed between the
two steps are harmless; the poll loop just resumes.

To use a secret from your own password manager instead of a generated one:
`./rotate-secret.sh '<your-secret>'` (alphanumeric secrets avoid any
escaping questions).

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
