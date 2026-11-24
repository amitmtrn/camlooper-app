#!/usr/bin/env bash
# Dev only: serve the phone page + same-origin /v1 proxy locally and expose it over a
# cloudflared quick tunnel (random, short-lived https://*.trycloudflare.com URL).
# Env: PORT (default 0 = any free port; 8787 is also wrangler dev's default), UPSTREAM (default https://api.camlooper.com),
#      CLOUDFLARED (default ~/.local/bin/cloudflared, then cloudflared in PATH).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PORT="${PORT:-0}"
export PORT

if [[ -n "${CLOUDFLARED:-}" ]]; then CF="$CLOUDFLARED"
elif [[ -x "$HOME/.local/bin/cloudflared" ]]; then CF="$HOME/.local/bin/cloudflared"
elif command -v cloudflared >/dev/null 2>&1; then CF="$(command -v cloudflared)"
else echo "cloudflared not found (set CLOUDFLARED=/path/to/cloudflared)" >&2; exit 1; fi
command -v node >/dev/null 2>&1 || { echo "node (>= 18) not found" >&2; exit 1; }

LOGDIR="$(mktemp -d)"
SERVE_PID="" CF_PID=""
cleanup() {
  trap - INT TERM EXIT
  [[ -n "$CF_PID" ]] && kill "$CF_PID" 2>/dev/null || true
  [[ -n "$SERVE_PID" ]] && kill "$SERVE_PID" 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$LOGDIR"
  echo; echo "Stopped."
}
trap cleanup INT TERM EXIT

# The port is taken from serve-phone's own "listening on" line, so the tunnel can never
# publish some other process that happens to hold the port (e.g. EADDRINUSE + wrangler dev).
node "$HERE/serve-phone.mjs" > >(tee "$LOGDIR/serve.log") 2>&1 &
SERVE_PID=$!
LISTEN_PORT=""
for _ in $(seq 1 50); do
  kill -0 "$SERVE_PID" 2>/dev/null || { echo "serve-phone.mjs failed to start (port $PORT in use?)" >&2; exit 1; }
  LISTEN_PORT="$(grep -oE '^serve-phone: listening on http://127\.0\.0\.1:[0-9]+' "$LOGDIR/serve.log" | head -n1 | grep -oE '[0-9]+$' || true)"
  [[ -n "$LISTEN_PORT" ]] && break
  sleep 0.1
done
[[ -n "$LISTEN_PORT" ]] || { echo "serve-phone.mjs did not report its port" >&2; exit 1; }
kill -0 "$SERVE_PID" 2>/dev/null || { echo "serve-phone.mjs exited" >&2; exit 1; }

# cloudflared's own log goes to a private temp file (it never sees tickets: they are
# inside the TLS-terminated requests it forwards, and it does not log URLs at info level).
"$CF" tunnel --no-autoupdate --url "http://127.0.0.1:$LISTEN_PORT" >"$LOGDIR/cloudflared.log" 2>&1 &
CF_PID=$!

URL=""
for _ in $(seq 1 300); do
  URL="$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$LOGDIR/cloudflared.log" | head -n1 || true)"
  [[ -n "$URL" ]] && break
  kill -0 "$CF_PID" 2>/dev/null || { echo "cloudflared exited:" >&2; tail -n 20 "$LOGDIR/cloudflared.log" >&2; exit 1; }
  sleep 0.1
done
[[ -n "$URL" ]] || { echo "Timed out waiting for the tunnel URL" >&2; exit 1; }

echo
echo "=================================================================="
echo "  Open on the phone (Android Chrome):"
echo
echo "     $URL"
echo
echo "=================================================================="
if command -v qrencode >/dev/null 2>&1; then qrencode -t ANSIUTF8 "$URL"; fi
echo "(The tunnel can take ~10-30 s to start answering. Ctrl-C stops everything.)"
echo

# Stop everything as soon as either process exits (portable: no `wait -n` on macOS bash 3.2).
while kill -0 "$SERVE_PID" 2>/dev/null && kill -0 "$CF_PID" 2>/dev/null; do sleep 1; done
kill -0 "$SERVE_PID" 2>/dev/null || echo "serve-phone.mjs exited; stopping the tunnel." >&2
kill -0 "$CF_PID" 2>/dev/null || echo "cloudflared exited; stopping." >&2
