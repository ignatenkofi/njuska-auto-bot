// Cloudflare Worker — proxy for polovniautomobili.com
//
// Why this exists: Cloudflare challenges direct HTTP requests from non-
// macOS network stacks (Linux VM + curl gets 403, despite same source IP
// as Mac which gets 200). Routing the fetch through a CF Worker bypasses
// that — CF doesn't challenge its own infrastructure.

const TARGET_BASE = "https://www.polovniautomobili.com";
const UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) " +
           "AppleWebKit/605.1.15 (KHTML, like Gecko) " +
           "Version/17.4 Safari/605.1.15";

export default {
  async fetch(request, env) {
    // Auth: only the bot with the shared secret can use this proxy.
    // PROXY_SECRET is set via `wrangler secret put PROXY_SECRET`.
    const provided = request.headers.get("x-proxy-secret");
    if (!provided || provided !== env.PROXY_SECRET) {
      return new Response("Forbidden", { status: 403 });
    }

    // We only need GET — defensive against being abused as an open proxy.
    if (request.method !== "GET") {
      return new Response("Method not allowed", { status: 405 });
    }

    // Forward path + query to polovni; ignore the worker's hostname.
    const incoming = new URL(request.url);
    const targetUrl = TARGET_BASE + incoming.pathname + incoming.search;

    let upstream;
    try {
      upstream = await fetch(targetUrl, {
        method: "GET",
        headers: {
          "User-Agent": UA,
          "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
          "Accept-Language": "en-US,en;q=0.9",
        },
      });
    } catch (e) {
      return new Response(`upstream error: ${e.message}`, { status: 502 });
    }

    // Stream body through. Strip CF-specific response headers from upstream —
    // they refer to polovni's CF tenancy, not ours, and would confuse our
    // downstream debugging.
    return new Response(upstream.body, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: {
        "content-type":
          upstream.headers.get("content-type") ?? "text/html; charset=utf-8",
      },
    });
  },
};
