#!/usr/bin/env bash
#
# Update the deployed bot to the latest nightly binary from GitHub Releases.
#
# Run on the Linux VM where the bot lives. Idempotent — safe to re-run any
# time. Does not touch .env, the SQLite DB, or HTML dumps. Only swaps the
# binary and restarts systemd.
#
# Usage:
#   curl -L https://github.com/ignatenkofi/njuska-auto-bot/raw/main/deploy/update.sh | bash
# or after a `git clone` of the repo on the VM:
#   ./deploy/update.sh

set -euo pipefail

BINARY_URL="https://github.com/ignatenkofi/njuska-auto-bot/releases/download/nightly/njuska_auto_bot"
DEST="/opt/njuska-auto-bot/njuska_auto_bot"
SERVICE="njuska-auto-bot"

echo "==> Downloading latest nightly binary..."
# Download to a sibling file first so a failed download doesn't trash the
# currently-running binary. `--fail` makes curl exit non-zero on HTTP 4xx/5xx
# instead of silently writing the error body.
sudo -u njuska curl -L --fail "$BINARY_URL" -o "$DEST.new"
sudo -u njuska chmod +x "$DEST.new"

echo "==> Stopping $SERVICE..."
sudo systemctl stop "$SERVICE"

echo "==> Swapping binary..."
sudo -u njuska mv "$DEST.new" "$DEST"

echo "==> Starting $SERVICE..."
sudo systemctl start "$SERVICE"

# Give it a moment to either come up healthy or crash on startup.
sleep 2

echo
echo "==> Recent logs (Ctrl+C to exit if it keeps tailing):"
sudo journalctl -u "$SERVICE" -n 20 --no-pager
