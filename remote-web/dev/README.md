# CamLooper Remote phone page: dev serving harness

`../index.html` is the phone web page: a single file with vanilla JS and inline CSS, no build step.
It links the phone, opens meetings on the computer, and streams the phone camera to the desktop
over WebRTC. This folder lets you test it on a real phone against the **live** relay
(`https://api.camlooper.com`) without deploying the site or changing the relay.

## Why a proxy?

The live relay only accepts browser requests from `Origin: https://camlooper.com`, and Android
Chrome only allows camera access on an https page. In production the page will be served at
`https://camlooper.com/remote` and will call `https://api.camlooper.com` directly, with no proxy.

For development, `serve-phone.mjs` serves the page and a **same-origin** `/v1` proxy from your
computer. A cloudflared quick tunnel then gives the phone an https URL:

```
phone (Chrome) ──https──▶ *.trycloudflare.com ──▶ 127.0.0.1:8787 serve-phone.mjs ──https/wss──▶ api.camlooper.com
                                                    ├─ /, /index.html → ../index.html
                                                    └─ allow-listed /v1 routes + /v1/mobile/ws → proxied
```

The proxy removes `Origin`, `Referer`, `Cookie`, `CF-*` and `X-Forwarded-*` from each request, so
the relay sees a native client (the same path the Expo app uses). It rewrites `Host`, and drops
hop-by-hop and `Access-Control-*` response headers (CORS isn't needed on the same origin).
The page works out its API base like this: `window.CAMLOOPER_API`, else `https://api.camlooper.com`
on camlooper.com, else its own origin. So the same file works in both setups.

## Usage

```bash
./start-phone-tunnel.sh          # starts serve-phone.mjs + cloudflared, prints the URL (+ QR if qrencode is installed)
# open the printed https://….trycloudflare.com URL in Chrome on the phone; Ctrl-C stops both
```

Env: `PORT` (script default 0 = any free port, so it can't collide with `wrangler dev` on 8787;
the tunnel uses the port serve-phone reports, and stops if serve-phone exits), `UPSTREAM` (default `https://api.camlooper.com`), `CLOUDFLARED`
(default `~/.local/bin/cloudflared`, then `cloudflared` in `PATH`). Add `?debug=1` to the page URL
for verbose console logs. Tokens, tickets, SDP and candidates are still redacted.

You can also run the server alone: `node serve-phone.mjs` (Node ≥ 18, no npm dependencies; default
port 8787). Extra server env: `WS_HANDSHAKE_MS` (upstream connect + 101 deadline, default 10000),
`WS_MAX` (concurrent WebSocket upgrades, default 8).

After **any** edit to the page's inline script, run `node update-csp.mjs` to refresh the CSP
`script-src` hash. Otherwise the browser blocks the script; `selftest.mjs` fails if the hash is stale.

## Tests

```bash
node selftest.mjs         # proxy vs. a local fake upstream (HTTP + raw WebSocket); allow-list, Host check,
                          # WS non-101 relay/close + no pipelined bytes, handshake timeout, upgrade cap, CSP hash
node smoke-shots.mjs      # headless Chrome 412×915 → shot-pair.png, shot-code.png, shot-home.png
node smoke-stream.mjs     # phone page (fake camera) → fake desktop peer through a mocked relay socket
node smoke-behaviors.mjs  # mic note/mute, stale stream_stop, codec rejection, relay too_large, 4000, retry msg
node smoke-record.mjs     # Record a loop: app-protocol hello gating, a take (REC clock, Stop, 1:00 auto-stop), outcomes, loop controls; the old file card stays hidden → shot-record-*.png
```

The smoke scripts need `playwright-core` (the library only; it downloads no browser) and
drive the system Chrome, `/usr/bin/google-chrome` (override with `CHROME=`). It is not a
dependency of the app, so install it without touching `package.json`:

```bash
npm i --no-save playwright-core        # in the repo root; lands in the git-ignored node_modules/
```

or reuse a copy installed elsewhere:
`PLAYWRIGHT_CORE=/path/to/node_modules/playwright-core node smoke-stream.mjs`. The lookup is
in `playwright.mjs`: `$PLAYWRIGHT_CORE` if set, otherwise a normal `import("playwright-core")`.
Without either, the scripts exit with status 2 and say so. All `/v1` traffic is
mocked, so they never contact api.camlooper.com. `smoke-stream.mjs` checks the signaling order
(`stream_request` → accept → `offer` → trickled candidates + end-of-candidates → answer), that
H.264 42e01f is offered first, that the offer only carries H.264 constrained baseline / VP8 / rtx and
Opus, and that debug logs leak no secrets. If ICE connects, it checks
the Live path, the stats, and that Stop sends `stream_stop{reason:"user"}`. If it can't connect
(e.g. in a container where Chrome gathers no candidates), it checks the `ice_failed` path (20 s
deadline from `stream_accept`).

## Security notes

- **Dev only.** Production must not use this proxy. The real page will run on camlooper.com, and the
  relay's Origin allow-list will do its job there.
- The server listens on `127.0.0.1` only. Your phone reaches it only through the tunnel. The
  quick-tunnel URL is random and lasts only as long as `start-phone-tunnel.sh` runs, but anyone who
  has the URL can load the page and use the proxy while it's up. Don't share it.
- The proxy forwards only what the phone page calls: `GET /v1/health`, `GET /v1/ice-servers`,
  `POST /v1/mobile/pair/start`, `DELETE /v1/mobile/pair`, `GET /v1/mobile/status`,
  `POST /v1/mobile/commands`, `POST /v1/mobile/ws-ticket` and the `/v1/mobile/ws` upgrade. Everything
  else (`/v1/desktop/*`, `/v1/admin/*`, other methods, OPTIONS) returns 404, so a leaked tunnel URL
  can't be used to brute-force desktop claims from your IP. `Host` must be `127.0.0.1`, `localhost`
  or `*.trycloudflare.com` (else 421). Request bodies are capped at 64 KB; the connection is closed
  after a 413.
- WebSocket: no client bytes are forwarded until upstream answers `101`. Any other status is relayed
  to the client and both sockets are closed, so a request pipelined after the upgrade never reaches
  the relay. Connect + handshake has a 10 s deadline (504) and concurrent upgrades are capped (503).
- Upstream `Set-Cookie`, `Location`, hop-by-hop and `Access-Control-*` headers are dropped.
- The proxy logs one line per request: method, pathname, status and duration. It never logs query
  strings (ws tickets travel in `?ticket=`), headers (`Authorization` holds the mobile token) or
  bodies. `selftest.mjs` checks this.
- The page stores `{pairId, mobileToken, deviceName}` in `localStorage` for the tunnel origin.
  Each new quick-tunnel URL is a new origin, so you'll need to link again. Unpair when you're done.
- Chrome prints the full `wss://…?ticket=` URL to the console when a handshake fails. The ticket is
  single-use and already burned, but don't paste raw console output into bug reports.

## Production origin and CSP (recommendation, not done here)

- The page carries a `<meta http-equiv="Content-Security-Policy">`: `default-src 'none'`, the script
  pinned by its sha256 hash, `connect-src 'self' https://api.camlooper.com wss://api.camlooper.com
  wss://*.trycloudflare.com` (`'self'` doesn't cover `wss:` in every browser, and the tunnel host is
  random, hence the wildcard), `media-src mediastream: blob:`, `base-uri 'none'`. The same file
  works on camlooper.com and behind this proxy. `window.CAMLOOPER_API` pointing at another host
  needs a matching `connect-src` entry.
- **Recommended:** serve the page from a dedicated origin (e.g. `https://remote.camlooper.com`, added
  to the relay's `ALLOWED_ORIGINS`) instead of `camlooper.com/remote`. The mobile token lives in
  `localStorage`, so on the marketing origin any XSS anywhere on camlooper.com could read it.
- Production must also send these as **headers** (a `<meta>` CSP can't do `frame-ancestors`):
  `Content-Security-Policy` (same policy + `frame-ancestors 'none'`; the `wss://*.trycloudflare.com`
  entry can be dropped there), `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff`, `Permissions-Policy: camera=(self), microphone=(self)`,
  `Cache-Control: no-cache`. These match what `serve-phone.mjs` sends.

## Receiver (desktop / str0m) expectations: verify on the Rust side

The page relies on the desktop doing the following:

- **End-of-candidates** arrives as `{kind:"candidate", candidate:""}` (empty string, no
  `sdpMid`/`sdpMLineIndex`). Ignore it, or treat it as end-of-candidates; never error.
- **Candidates only after the offer**: the page never sends a local candidate before its `offer`.
  Remote candidates the desktop sends before its `answer` are buffered by the page.
- **Mid-stream resolution changes**: switching cameras (`replaceTrack`) and Chrome's bitrate
  adaptation change the encoded size without renegotiation. Each keyframe can carry a new SPS/PPS
  (H.264) or size (VP8).
- **Orientation**: Chrome sends the `urn:3gpp:video-orientation` header extension when the phone
  rotates (if negotiated). Either apply it or don't negotiate it (then Chrome rotates the frames itself).
- **Codecs offered**: H.264 constrained baseline only (`profile-level-id=42e01f` / `42001f`,
  `packetization-mode=1`), VP8, rtx. Audio: Opus only. The answer must keep the video m-line
  (port ≠ 0, not `inactive`), otherwise the phone shows a codec-specific error.
- **Opus m-line**: an audio m-line (sendonly from the phone) is always offered when the mic is
  allowed. Accept it (phase 2 uses it) or answer it as rejected/inactive; don't fail the session.
  The user can mute (the track keeps sending silence).
- **Teardown without a stop**: `stream_stop` is queued while the phone's socket reconnects, but it
  can still be lost (page killed). The desktop must tear down on its own on `peer` offline and on an
  ICE/DTLS failure or timeout.
- **`stream_stop` must carry `sessionId`**: once a session is accepted, the phone ignores a stop
  without a matching `sessionId`.
- **Later: ICE restart.** Not implemented (review item 21). A Wi-Fi ↔ cellular switch ends in a
  failure after ~20 s. Once the receiver supports a re-offer, add `pc.restartIce()` + a new offer
  on `disconnected`.
