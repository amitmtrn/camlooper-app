#!/usr/bin/env node
// Dev-only server for the CamLooper Remote phone page.
//
// Serves ../index.html and proxies an allow-list of the /v1 routes the phone page uses
// (HTTP + the /v1/mobile/ws WebSocket) to UPSTREAM on the SAME origin, stripping browser-identifying headers (Origin,
// Referer, Cookie, CF-*, X-Forwarded-*) so the relay sees a native-like client.
// Put a cloudflared quick tunnel in front of it to get an https URL for the phone.
//
// Env: PORT (default 8787; 0 = pick a free port), UPSTREAM (default https://api.camlooper.com),
//      PAGE (default ../index.html relative to this file).
// Logs one line per request: method, pathname, status, duration. Never query strings
// (ws tickets live there), never headers or bodies.
// Node >= 18, built-ins only.

import http from "node:http";
import https from "node:https";
import net from "node:net";
import tls from "node:tls";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PORT = process.env.PORT === undefined || process.env.PORT === "" ? 8787 : Number(process.env.PORT);
const PAGE = process.env.PAGE ? path.resolve(process.env.PAGE) : path.join(HERE, "..", "index.html");
const UPSTREAM = new URL(process.env.UPSTREAM || "https://api.camlooper.com");
if (UPSTREAM.protocol !== "https:" && UPSTREAM.protocol !== "http:") throw new Error("UPSTREAM must be http(s)");
const UP_TLS = UPSTREAM.protocol === "https:";
const UP_PORT = Number(UPSTREAM.port) || (UP_TLS ? 443 : 80);
const UP_HOST = UPSTREAM.host; // includes the port when non-default
const BODY_CAP = 64 * 1024;
const WS_PATH = "/v1/mobile/ws";
const WS_HANDSHAKE_MS = Number(process.env.WS_HANDSHAKE_MS) || 10000; // upstream connect + 101
const WS_MAX = Number(process.env.WS_MAX) || 8;                        // concurrent upgrades/tunnels
const WS_HEAD_CAP = 16 * 1024;                                        // upstream response head cap

// Only what the phone page calls. Everything else under /v1 (desktop/*, admin/*, …) is 404,
// so a leaked tunnel URL can't be used to reach other relay routes from this machine's IP.
const ALLOWED = new Map([
  ["/v1/health", ["GET", "HEAD"]],
  ["/v1/ice-servers", ["GET"]],
  ["/v1/mobile/pair/start", ["POST"]],
  ["/v1/mobile/pair", ["DELETE"]],
  ["/v1/mobile/status", ["GET"]],
  ["/v1/mobile/commands", ["POST"]],
  ["/v1/mobile/ws-ticket", ["POST"]],
]);
function isAllowed(method, pathname) {
  const m = ALLOWED.get(pathname);
  return !!m && m.includes(method);
}

// Host header must be loopback or a cloudflared quick-tunnel name.
function hostAllowed(req) {
  const raw = String(req.headers.host || "").toLowerCase();
  const host = raw.startsWith("[") ? raw.slice(0, raw.indexOf("]") + 1) : raw.replace(/:\d+$/, "");
  return host === "127.0.0.1" || host === "localhost" || host === "[::1]" || /^[a-z0-9-]+\.trycloudflare\.com$/.test(host);
}

// Upstream response headers never passed to the browser.
function dropResponseHeader(lower) {
  return HOP_BY_HOP.has(lower) || lower.startsWith("access-control-") || lower === "set-cookie" || lower === "location";
}

const HOP_BY_HOP = new Set([
  "connection", "keep-alive", "proxy-connection", "proxy-authenticate", "proxy-authorization",
  "te", "trailer", "trailers", "transfer-encoding", "upgrade",
]);

function stripRequestHeader(name) {
  return (
    name === "host" || name === "origin" || name === "referer" || name === "cookie" ||
    name === "forwarded" || name === "x-real-ip" || name === "true-client-ip" || name === "cdn-loop" ||
    name.startsWith("cf-") || name.startsWith("x-forwarded-")
  );
}

/** Filtered upstream request headers (from rawHeaders, keeps duplicates/case). */
function upstreamHeaders(req, { keepUpgrade = false } = {}) {
  const out = [];
  const connTokens = new Set(String(req.headers.connection || "").toLowerCase().split(",").map((s) => s.trim()).filter(Boolean));
  for (let i = 0; i < req.rawHeaders.length; i += 2) {
    const name = req.rawHeaders[i], lower = name.toLowerCase(), value = req.rawHeaders[i + 1];
    if (stripRequestHeader(lower)) continue;
    if (keepUpgrade && (lower === "connection" || lower === "upgrade")) { out.push([name, value]); continue; }
    if (HOP_BY_HOP.has(lower) || connTokens.has(lower) || lower === "content-length") continue;
    out.push([name, value]);
  }
  out.push(["Host", UP_HOST]);
  return out;
}

function logLine(method, pathname, status, start) {
  // Only method + pathname (+ status/duration). Never the query string or headers.
  const safePath = pathname.replace(/[^\x21-\x7e]/g, "?").slice(0, 200);
  console.log(`${new Date().toISOString()} ${method} ${safePath} ${status} ${Date.now() - start}ms`);
}

function sendPlain(res, status, body, type = "text/plain; charset=utf-8") {
  res.writeHead(status, { "Content-Type": type, "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff" });
  res.end(body);
}

function parseUrl(raw) {
  try { return new URL(raw || "/", "http://localhost"); } catch { return null; }
}

function servePage(req, res, done) {
  fs.readFile(PAGE, (err, buf) => {
    if (err) { sendPlain(res, 500, "page missing\n"); return done(500); }
    res.writeHead(200, {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-cache, no-store, must-revalidate",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer",
      "X-Frame-Options": "DENY",
      "Permissions-Policy": "camera=(self), microphone=(self)",
    });
    res.end(req.method === "HEAD" ? undefined : buf);
    done(200);
  });
}

// 413 and stop reading: close the connection once the response is out (no unbounded drain).
function tooLarge(req, res, done) {
  res.shouldKeepAlive = false;
  res.on("finish", () => setTimeout(() => req.destroy(), 50));
  sendPlain(res, 413, '{"error":"payload_too_large"}', "application/json");
  done(413);
}

function proxyHttp(req, res, url, done) {
  const declared = Number(req.headers["content-length"] || 0);
  if (declared > BODY_CAP) return tooLarge(req, res, done);
  const chunks = []; let size = 0, aborted = false;
  req.on("data", (c) => {
    if (aborted) return;
    size += c.length;
    if (size > BODY_CAP) { aborted = true; chunks.length = 0; tooLarge(req, res, done); return; }
    chunks.push(c);
  });
  req.on("error", () => { aborted = true; done(499); });
  req.on("end", () => {
    if (aborted) return;
    const body = Buffer.concat(chunks);
    const headers = upstreamHeaders(req);
    if (body.length || !["GET", "HEAD", "OPTIONS", "DELETE"].includes(req.method)) headers.push(["Content-Length", String(body.length)]);
    const mod = UP_TLS ? https : http;
    const up = mod.request({
      protocol: UPSTREAM.protocol, hostname: UPSTREAM.hostname, port: UP_PORT, method: req.method,
      path: url.pathname + url.search, headers: Object.fromEntries(groupHeaders(headers)), servername: UPSTREAM.hostname,
      timeout: 30000,
    }, (ur) => {
      const out = {};
      for (const [k, v] of Object.entries(ur.headers)) {
        const lower = k.toLowerCase();
        if (dropResponseHeader(lower)) continue;
        out[k] = v;
      }
      res.writeHead(ur.statusCode || 502, out);
      ur.pipe(res);
      ur.on("end", () => done(ur.statusCode));
      ur.on("error", () => { res.destroy(); done(502); });
    });
    up.on("timeout", () => up.destroy(new Error("upstream timeout")));
    res.on("close", () => { if (!up.destroyed) up.destroy(); }); // client went away: abort upstream
    up.on("error", () => {
      if (!res.headersSent) sendPlain(res, 502, '{"error":"bad_gateway","message":"upstream unreachable"}', "application/json");
      else res.destroy();
      done(502);
    });
    up.end(body);
  });
}

function groupHeaders(list) {
  const m = new Map();
  for (const [k, v] of list) {
    const key = k.toLowerCase();
    if (m.has(key)) { const cur = m.get(key); m.set(key, Array.isArray(cur) ? [...cur, v] : [cur, v]); }
    else m.set(key, v);
  }
  return m;
}

const server = http.createServer((req, res) => {
  const start = Date.now();
  const url = parseUrl(req.url);
  let logged = false;
  const done = (status) => { if (!logged) { logged = true; logLine(req.method, url ? url.pathname : "<bad-url>", status, start); } };
  if (!url) { sendPlain(res, 400, "bad request\n"); return done(400); }
  if (!hostAllowed(req)) { sendPlain(res, 421, "misdirected request\n"); return done(421); }
  if (url.pathname === "/" || url.pathname === "/index.html") {
    if (req.method !== "GET" && req.method !== "HEAD") { sendPlain(res, 405, "method not allowed\n"); return done(405); }
    return servePage(req, res, done);
  }
  if (isAllowed(req.method, url.pathname)) return proxyHttp(req, res, url, done);
  sendPlain(res, 404, "not found\n");
  done(404);
});

let wsActive = 0;

server.on("upgrade", (req, socket, head) => {
  const start = Date.now();
  const url = parseUrl(req.url);
  const pathname = url ? url.pathname : "<bad-url>";
  socket.on("error", () => {});
  const refuse = (code, text) => {
    socket.end(`HTTP/1.1 ${code} ${text}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`);
    logLine(`${req.method}(ws)`, pathname, code, start);
  };
  if (!hostAllowed(req)) return refuse(421, "Misdirected Request");
  if (!url || url.pathname !== WS_PATH || req.method !== "GET" || String(req.headers.upgrade || "").toLowerCase() !== "websocket") return refuse(404, "Not Found");
  if (wsActive >= WS_MAX) return refuse(503, "Service Unavailable");
  wsActive++;

  // Client bytes (head + anything later) are NOT forwarded until upstream answers 101.
  // The client socket has no 'data' listener yet, so Node buffers it (paused).
  const headers = upstreamHeaders(req, { keepUpgrade: true });
  let state = "connecting"; // connecting -> handshake -> open | closed
  let upBuf = Buffer.alloc(0);
  const upstream = UP_TLS
    ? tls.connect({ host: UPSTREAM.hostname, port: UP_PORT, servername: UPSTREAM.hostname, ALPNProtocols: ["http/1.1"] })
    : net.connect({ host: UPSTREAM.hostname, port: UP_PORT });
  upstream.setNoDelay(true); socket.setNoDelay(true);

  let released = false;
  const kill = () => {
    if (!released) { released = true; wsActive--; }
    state = "closed"; clearTimeout(timer);
    upstream.destroy(); socket.destroy();
  };
  const fail = (code, text) => {
    if (state === "closed") return;
    if (state !== "open") refuse(code, text);
    state = "closed"; clearTimeout(timer);
    setTimeout(kill, 50); // let the refusal flush
  };
  const timer = setTimeout(() => fail(504, "Gateway Timeout"), WS_HANDSHAKE_MS);

  upstream.once(UP_TLS ? "secureConnect" : "connect", () => {
    if (state !== "connecting") return;
    state = "handshake";
    let head1 = `GET ${url.pathname}${url.search} HTTP/1.1\r\n`;
    for (const [k, v] of headers) head1 += `${k}: ${String(v).replace(/[\r\n]/g, "")}\r\n`;
    upstream.write(head1 + "\r\n");
  });

  const onUpData = (d) => {
    if (state !== "handshake") return;
    upBuf = Buffer.concat([upBuf, d]);
    const end = upBuf.indexOf("\r\n\r\n");
    if (end < 0) { if (upBuf.length > WS_HEAD_CAP) fail(502, "Bad Gateway"); return; }
    const headText = upBuf.subarray(0, end).toString("latin1");
    const lines = headText.split("\r\n");
    const m = /^HTTP\/1\.[01] (\d{3})/.exec(lines[0]);
    const status = m ? Number(m[1]) : 0;
    const kept = [lines[0], ...lines.slice(1).filter((l) => {
      const name = l.slice(0, l.indexOf(":")).trim().toLowerCase();
      return name !== "set-cookie" && name !== "location";
    })];
    const rest = upBuf.subarray(end + 4);
    if (status !== 101) {
      // Relay the refusal (headers + body up to Content-Length, capped) and close both sides.
      const cl = Number((/^content-length:\s*(\d+)/im.exec(headText) || [])[1]);
      const finish = (body) => {
        if (state === "closed") return;
        logLine("GET(ws)", pathname, status || 502, start);
        const out = kept.filter((l) => !/^(connection|keep-alive|transfer-encoding):/i.test(l));
        out.push("Connection: close");
        if (!/^content-length:/im.test(headText)) out.push(`Content-Length: ${body.length}`);
        state = "closed"; clearTimeout(timer);
        socket.end(Buffer.concat([Buffer.from(out.join("\r\n") + "\r\n\r\n", "latin1"), body]));
        upstream.destroy();
        setTimeout(kill, 50);
      };
      if (Number.isFinite(cl) && cl <= WS_HEAD_CAP && rest.length < cl) {
        state = "refusing";
        let body = rest;
        upstream.on("data", (x) => { body = Buffer.concat([body, x]); if (body.length >= cl) finish(body.subarray(0, cl)); });
        upstream.on("end", () => finish(body));
        return;
      }
      return finish(Number.isFinite(cl) ? rest.subarray(0, cl) : Buffer.alloc(0));
    }
    state = "open"; clearTimeout(timer);
    logLine("GET(ws)", pathname, 101, start);
    upstream.off("data", onUpData);
    socket.write(Buffer.from(kept.join("\r\n") + "\r\n\r\n", "latin1"));
    if (rest.length) socket.write(rest);
    if (head && head.length) upstream.write(head);
    upstream.pipe(socket); socket.pipe(upstream);
  };
  upstream.on("data", onUpData);
  upstream.on("error", () => fail(502, "Bad Gateway"));
  upstream.on("close", () => { if (state === "connecting" || state === "handshake") fail(502, "Bad Gateway"); else if (state !== "refusing") kill(); });
  socket.on("close", kill);
});

server.listen(PORT, "127.0.0.1", () => {
  const { port } = server.address();
  console.log(`serve-phone: listening on http://127.0.0.1:${port} -> ${UPSTREAM.origin} (page ${PAGE})`);
});

const shutdown = () => { server.close(); process.exit(0); };
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
