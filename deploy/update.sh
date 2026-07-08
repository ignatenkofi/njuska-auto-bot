#!/usr/bin/env bash
#
# Update the deployed bot to the latest binary from GitHub Releases.
#
# Run on the Linux VM where the bot lives. Idempotent — safe to re-run any
# time. Does not touch .env, the SQLite DB, or HTML dumps. Only swaps the
# binary, restarts systemd, and verifies the service actually stays up;
# if it doesn't, the previous binary is restored automatically (#18).
#
# Usage:
#   curl -L https://github.com/ignatenkofi/njuska-auto-bot/raw/main/deploy/update.sh | bash
# or after a `git clone` of the repo on the VM:
#   ./deploy/update.sh

set -euo pipefail

# `releases/latest/download/<file>` is GitHub's canonical "the newest
# non-prerelease asset" URL — redirects to whichever version was published
# most recently. Pin to a specific version (e.g. v0.2.0/download/...) if
# you want to opt out of auto-upgrades.
BINARY_URL="https://github.com/ignatenkofi/njuska-auto-bot/releases/latest/download/njuska_auto_bot"
DEST="/opt/njuska-auto-bot/njuska_auto_bot"
BACKUP="$DEST.bak"
SERVICE="njuska-auto-bot"

# How long to watch the freshly-started service before declaring victory.
# Needs to comfortably exceed RestartSec=10s in the unit: a binary that
# crashes on startup looks "active" for a moment, dies, and systemd only
# re-launches it after 10s — checking once after 2s would miss all of that.
HEALTH_WATCH_SECS=15

echo "==> Downloading latest release binary..."
# Download to a sibling file first so a failed download doesn't trash the
# currently-running binary. `--fail` makes curl exit non-zero on HTTP 4xx/5xx
# instead of silently writing the error body.
sudo -u njuska curl -L --fail "$BINARY_URL" -o "$DEST.new"
sudo -u njuska chmod +x "$DEST.new"

# Sanity: does the new binary even execute on this box (glibc, arch)?
# --version needs no config, no network, no DB — perfect preflight.
echo "==> Preflight: $("$DEST.new" --version)"

echo "==> Stopping $SERVICE..."
sudo systemctl stop "$SERVICE"

echo "==> Swapping binary (previous kept at $BACKUP)..."
# Keep exactly one backup generation. `cp` (not `mv`) for the backup so a
# re-run after a successful update still has a valid $DEST to copy; the
# swap itself is an atomic rename.
if [ -f "$DEST" ]; then
    sudo -u njuska cp -f "$DEST" "$BACKUP"
fi
sudo -u njuska mv "$DEST.new" "$DEST"

echo "==> Starting $SERVICE..."
sudo systemctl start "$SERVICE"

# Watch the service for a while: a startup crash (bad config parse, dead
# proxy probe, missing glibc symbol) shows up as is-active flipping to
# "failed"/"activating" within the first restart cycle.
echo "==> Watching service health for ${HEALTH_WATCH_SECS}s..."
healthy=true
for _ in $(seq "$HEALTH_WATCH_SECS"); do
    sleep 1
    if ! sudo systemctl is-active --quiet "$SERVICE"; then
        healthy=false
        break
    fi
done

if [ "$healthy" != true ]; then
    echo
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    echo "!!  UPDATE FAILED — service did not stay up with the new binary  !!"
    echo "!!  Rolling back to the previous binary from $BACKUP"
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    echo
    sudo journalctl -u "$SERVICE" -n 30 --no-pager || true

    if [ -f "$BACKUP" ]; then
        sudo systemctl stop "$SERVICE" || true
        sudo -u njuska cp -f "$BACKUP" "$DEST"
        sudo systemctl start "$SERVICE"
        # Second watch: confirm the rollback itself is healthy so the exit
        # message below is trustworthy.
        sleep 5
        if sudo systemctl is-active --quiet "$SERVICE"; then
            echo "==> Rollback complete; service is running the PREVIOUS binary."
        else
            echo "!!  Rollback started but the service is STILL not active —"
            echo "!!  investigate manually: sudo journalctl -u $SERVICE -n 50"
        fi
    else
        echo "!!  No backup at $BACKUP to roll back to — manual intervention needed."
    fi
    exit 1
fi

echo "==> Service healthy. Now running: $("$DEST" --version)"
echo
echo "==> Recent logs:"
sudo journalctl -u "$SERVICE" -n 20 --no-pager
