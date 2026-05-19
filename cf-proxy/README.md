# Cloudflare Worker proxy

A tiny ~30-line JavaScript Worker that the bot uses to forward HTTP fetches
to polovniautomobili.com.

## Why this exists

Cloudflare's Managed Challenge fingerprints non-macOS HTTP clients at the
TCP/TLS layer and returns 403. **Linux VMs and most VPS providers get blocked
even with `curl` + perfect browser headers + curl-impersonate.** macOS curl
gets through cleanly (different network stack).

Routing the bot's fetches through a Cloudflare Worker side-steps the problem:
the request to polovni originates from CF's own infrastructure, which CF
doesn't challenge.

If you're deploying on macOS (e.g. via launchd on a Mac mini), you don't
need this — direct fetch works. For Proxmox VMs, Hetzner/DO/Oracle VPSes,
etc., this is the recommended path.

## Cost

Free tier: 100,000 requests/day. The bot polls every 10 minutes by default
(144 requests/day). Comfortable headroom.

## Setup (one-time, ~5 min)

### 1. Prerequisites on your laptop

```bash
brew install node          # macOS; or `apt install nodejs` on Debian
npm install -g wrangler    # Cloudflare's official Worker CLI
wrangler login             # opens a browser for OAuth — free CF account
```

### 2. Pick a unique name

Edit `wrangler.toml` and change `name = "nau-proxy"` to something unique
within Cloudflare's namespace, e.g. `name = "yourname-cf-proxy"`. Your URL
will end up being `https://<that-name>.<your-cf-subdomain>.workers.dev`.

### 3. Set up the shared secret

The Worker checks an `x-proxy-secret` header on every request — anyone
hitting the URL without the right secret gets a 403. This keeps your proxy
from being abused as an open relay.

```bash
SECRET=$(openssl rand -hex 32)
echo "Save this somewhere: $SECRET"

printf "%s" "$SECRET" | wrangler secret put PROXY_SECRET
```

(The `printf "%s"` instead of `echo` avoids a trailing newline being
included in the secret value.)

### 4. Deploy

```bash
wrangler deploy
```

You'll see the URL printed at the end. Note it.

### 5. Wire to the bot

In your bot's `.env`:

```
CF_PROXY_URL=https://<that-name>.<your-cf-subdomain>.workers.dev
CF_PROXY_SECRET=<the hex from step 3>
```

Restart the bot (`systemctl restart njuska-auto-bot`). Check logs — should
now see `200 OK` instead of 403.

## Operations

```bash
# Re-deploy after editing src/index.js
wrangler deploy

# Rotate the shared secret (replace old → set new in CF + update bot's .env)
printf "%s" "$(openssl rand -hex 32)" | wrangler secret put PROXY_SECRET

# Tail Worker logs (useful for debugging)
wrangler tail

# Delete the Worker
wrangler delete
```

## Troubleshooting

**Bot still gets 403 after wiring up the proxy.**
→ Verify the bot is actually using the proxy: look in journalctl for
`via_proxy=true` in the `fetching search page via curl` debug log. If
`via_proxy=false`, both env vars aren't set or one is empty.

**Worker returns 403.**
→ The shared secret on the Worker side and `CF_PROXY_SECRET` in the bot's
`.env` don't match. Re-set both, restart.

**Worker returns 502.**
→ Worker timed out or had an error reaching polovni. Check `wrangler tail`
for the upstream error. May be a transient CF/polovni issue — usually
resolves itself.

**100k/day limit looms.**
→ Bump the bot's poll interval (`/interval 1800` for 30-min) to reduce
requests. Or upgrade to CF Workers Paid tier ($5/month, 10M req).
