#!/usr/bin/env bash
#
# Rotate the CF Worker's shared secret (#20).
#
# Run from the cf-proxy/ directory on the machine where you use wrangler
# (your laptop, not the VM). Generates a fresh secret, pushes it to the
# Worker, and prints the exact follow-up steps for the bot's .env — the
# rotation is only complete once BOTH sides have the new value.
#
# Order matters: the Worker starts enforcing the new secret the moment
# `wrangler secret put` lands, so the bot will get 403s from that moment
# until its .env is updated (a few minutes of missed polls; the bot's
# fetch-error alerting will complain if you take too long — that's fine).
#
# Usage:
#   ./rotate-secret.sh            # generate a random secret
#   ./rotate-secret.sh <secret>   # use a secret you generated elsewhere

set -euo pipefail

cd "$(dirname "$0")"

command -v wrangler >/dev/null 2>&1 || {
    echo "error: wrangler not found. npm install -g wrangler && wrangler login" >&2
    exit 1
}

if [ $# -ge 1 ]; then
    SECRET="$1"
else
    # hex output: alphanumeric-only, so it needs no escaping anywhere it
    # travels (curl config, .env, systemd EnvironmentFile).
    SECRET=$(openssl rand -hex 32)
fi

# Worker name from wrangler.toml, for the confirmation message.
WORKER_NAME=$(sed -n 's/^name *= *"\(.*\)"/\1/p' wrangler.toml | head -1)

echo "==> Pushing new PROXY_SECRET to Worker '${WORKER_NAME:-<unknown>}'..."
printf '%s' "$SECRET" | wrangler secret put PROXY_SECRET

echo
echo "==> Worker updated. Now update the bot — until you do, its fetches 403:"
echo
echo "  1. On the VM, edit /opt/njuska-auto-bot/.env and set:"
echo
echo "       CF_PROXY_SECRET=$SECRET"
echo
echo "  2. Restart the bot:"
echo
echo "       sudo systemctl restart njuska-auto-bot"
echo
echo "  3. Verify: the startup log must say 'CF Worker proxy probe OK'"
echo "     (a secret mismatch makes the bot refuse to start with a 403 hint):"
echo
echo "       sudo journalctl -u njuska-auto-bot -n 20 --no-pager"
echo
echo "Then discard this terminal's scrollback if others can see it."
