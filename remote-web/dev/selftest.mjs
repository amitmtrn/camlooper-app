#!/usr/bin/env node
// Self-test for serve-phone.mjs against a local fake upstream (http + minimal WebSocket echo).
// Never touches the real api.camlooper.com. Run: node dev/selftest.mjs
import http from "node:http";
import net from "node:net";
import crypto from "node:crypto";
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import fs from "node:fs";
import { scriptHash, cspHash } from "./update-csp.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SECRET_TICKET = "TICKET_" + crypto.randomBytes(12).toString("hex");
const SECRET_QS = "QS_" + crypto.randomBytes(8).toString("hex");
const seen = []; // requests observed by the fake upstream
const leaked = []; // bytes the fake upstream received on a refused (non-101) upgrade socket

// ---- fake upstream --------------------------------------------------------------
const upstream = http.createServer((req, res) => {
  const chunks = [];
  req.on("data", (c) => chunks.push(c));
  req.on("end", () => {
    seen.push({ method: req.method, url: req.url, headers: req.headers, body: Buffer.concat(chunks).toString() });
    res.setHeader("Access-Control-Allow-Origin", "https://camlooper.com");
    res.setHeader("Vary", "Origin");
    res.setHeader("X-Upstream", "fake");
    res.setHeader("Set-Cookie", "up=1; Path=/");
    res.setHeader("Location", "https://elsewhere.example/");
    if (req.url === "/v1/health") return res.end(JSON.stringify({ ok: true, service: "fake", protocol: 1 }));
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ echo: req.method, len: Buffer.concat(chunks).length }));
  });
});

upstream.on("upgrade", (req, socket) => {
  seen.push({ method: "UPGRADE", url: req.url, headers: req.headers });
  socket.on("error", () => {});
  if (req.url.includes("ticket=hang")) return; // never answers: proxy must time out
  if (req.url.includes("ticket=bad")) {
    // Refuse, but keep the socket open (keep-alive) and record anything else that arrives.
    socket.write('HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 24\r\nSet-Cookie: x=1\r\nConnection: keep-alive\r\n\r\n{"error":"unauthorized"}');
    socket.on("data", (d) => leaked.push(d.toString("latin1")));
    return;
  }
  const accept = crypto.createHash("sha1").update(req.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  let buf = Buffer.alloc(0);
  socket.on("data", (d) => {
    buf = Buffer.concat([buf, d]);
    const f = parseFrame(buf);
    if (!f) return;
    buf = buf.subarray(f.total);
    socket.write(frame(f.payload, false)); // echo back (server frames are unmasked)
  });
  socket.on("error", () => {});
});

// Minimal RFC 6455 frame helpers (single, unfragmented text frames < 64 KB).
function frame(payload, mask) {
  const p = Buffer.from(payload);
  const hdr = [0x81];
  if (p.length < 126) hdr.push((mask ? 0x80 : 0) | p.length);
  else hdr.push((mask ? 0x80 : 0) | 126, p.length >> 8, p.length & 255);
  if (!mask) return Buffer.concat([Buffer.from(hdr), p]);
  const key = crypto.randomBytes(4);
  const masked = Buffer.from(p.map((b, i) => b ^ key[i % 4]));
  return Buffer.concat([Buffer.from(hdr), key, masked]);
}
function parseFrame(b) {
  if (b.length < 2) return null;
  const masked = (b[1] & 0x80) !== 0;
  let len = b[1] & 0x7f, off = 2;
  if (len === 126) { if (b.length < 4) return null; len = b.readUInt16BE(2); off = 4; }
  const keyOff = off; if (masked) off += 4;
  if (b.length < off + len) return null;
  let payload = b.subarray(off, off + len);
  if (masked) { const key = b.subarray(keyOff, keyOff + 4); payload = Buffer.from(payload.map((x, i) => x ^ key[i % 4])); }
  return { payload: payload.toString(), total: off + len };
}

// ---- helpers ----------------------------------------------------------------------
const listen = (srv) => new Promise((r) => srv.listen(0, "127.0.0.1", () => r(srv.address().port)));

function request(port, method, p, headers = {}, body) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: "127.0.0.1", port, method, path: p, headers }, (res) => {
      const chunks = []; res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks).toString() }));
    });
    req.on("error", reject);
    if (body) req.write(body);
    req.end();
  });
}

function wsRoundTrip(port, p, headers, message) {
  return new Promise((resolve, reject) => {
    const sock = net.connect(port, "127.0.0.1");
    const key = crypto.randomBytes(16).toString("base64");
    let buf = Buffer.alloc(0), upgraded = false, statusLine = "";
    const t = setTimeout(() => { sock.destroy(); reject(new Error("ws timeout")); }, 5000);
    sock.on("connect", () => {
      let h = `GET ${p} HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n`;
      for (const [k, v] of Object.entries(headers)) h += `${k}: ${v}\r\n`;
      sock.write(h + "\r\n");
    });
    sock.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      if (!upgraded) {
        const i = buf.indexOf("\r\n\r\n"); if (i < 0) return;
        statusLine = buf.subarray(0, buf.indexOf("\r\n")).toString();
        buf = buf.subarray(i + 4);
        if (!/ 101 /.test(statusLine)) { clearTimeout(t); sock.destroy(); return resolve({ statusLine, echo: null }); }
        upgraded = true;
        sock.write(frame(message, true));
      }
      const f = parseFrame(buf);
      if (f) { clearTimeout(t); sock.destroy(); resolve({ statusLine, echo: f.payload }); }
    });
    sock.on("error", (e) => { clearTimeout(t); reject(e); });
    sock.on("close", () => { clearTimeout(t); if (!upgraded) resolve({ statusLine, echo: null }); });
  });
}

// Raw upgrade: sends `raw` in one write, returns everything received until close (or 3 s).
function rawExchange(port, raw) {
  return new Promise((resolve) => {
    const sock = net.connect(port, "127.0.0.1");
    let buf = Buffer.alloc(0);
    const t = setTimeout(() => { sock.destroy(); resolve({ data: buf.toString("latin1"), closed: false }); }, 3000);
    sock.on("connect", () => sock.write(raw));
    sock.on("data", (d) => { buf = Buffer.concat([buf, d]); });
    sock.on("error", () => {});
    sock.on("close", () => { clearTimeout(t); resolve({ data: buf.toString("latin1"), closed: true }); });
  });
}
const upgradeReq = (port, p) => `GET ${p} HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${crypto.randomBytes(16).toString("base64")}\r\nSec-WebSocket-Version: 13\r\nOrigin: https://camlooper.com\r\n\r\n`;

const browserish = {
  Origin: "https://evil.example",
  Referer: "https://evil.example/page",
  Cookie: "session=abc",
  "CF-Connecting-IP": "203.0.113.9",
  "CF-Ray": "123",
  "X-Forwarded-For": "203.0.113.9",
  "X-Forwarded-Proto": "https",
};
function assertStripped(h, what) {
  for (const k of Object.keys(h)) {
    assert.ok(!["origin", "referer", "cookie"].includes(k) && !k.startsWith("cf-") && !k.startsWith("x-forwarded-"), `${what}: header ${k} leaked upstream`);
  }
}

// ---- run --------------------------------------------------------------------------
const upPort = await listen(upstream);
const proxy = spawn(process.execPath, [path.join(HERE, "serve-phone.mjs")], {
  env: { ...process.env, PORT: "0", UPSTREAM: `http://127.0.0.1:${upPort}`, WS_HANDSHAKE_MS: "800", WS_MAX: "3" },
  stdio: ["ignore", "pipe", "pipe"],
});
let log = "";
proxy.stdout.on("data", (d) => { log += d; });
proxy.stderr.on("data", (d) => { log += d; });
const port = await new Promise((resolve, reject) => {
  const t = setTimeout(() => reject(new Error("proxy did not start:\n" + log)), 5000);
  const check = () => { const m = /listening on http:\/\/127\.0\.0\.1:(\d+)/.exec(log); if (m) { clearTimeout(t); resolve(Number(m[1])); } };
  proxy.stdout.on("data", check);
  proxy.on("exit", (c) => reject(new Error(`proxy exited ${c}:\n${log}`)));
});

let passed = 0;
async function test(name, fn) {
  try { await fn(); passed++; console.log(`ok   ${name}`); }
  catch (e) { console.log(`FAIL ${name}\n     ${e.message}`); process.exitCode = 1; }
}

try {
  await test("page served at / and /index.html with no-cache", async () => {
    for (const p of ["/", "/index.html", "/?debug=1"]) {
      const r = await request(port, "GET", p);
      assert.equal(r.status, 200, p);
      assert.match(r.headers["content-type"], /text\/html/);
      assert.match(r.headers["cache-control"], /no-cache|no-store/);
      assert.match(r.body, /CamLooper Remote/);
    }
  });

  await test("/v1/health proxied, browser headers stripped, Host rewritten, ACAO dropped", async () => {
    seen.length = 0;
    const r = await request(port, "GET", "/v1/health", browserish);
    assert.equal(r.status, 200);
    assert.equal(JSON.parse(r.body).service, "fake");
    assert.equal(r.headers["x-upstream"], "fake");
    assert.equal(r.headers["access-control-allow-origin"], undefined, "Access-Control-* must not pass through");
    assert.equal(seen.length, 1);
    assertStripped(seen[0].headers, "health");
    assert.equal(seen[0].headers.host, `127.0.0.1:${upPort}`);
  });

  await test("query string preserved upstream", async () => {
    seen.length = 0;
    const r = await request(port, "GET", `/v1/mobile/status?x=${SECRET_QS}`, { Authorization: "Bearer TOKEN_SHOULD_PASS" });
    assert.equal(r.status, 200);
    assert.equal(seen[0].url, `/v1/mobile/status?x=${SECRET_QS}`);
    assert.equal(seen[0].headers.authorization, "Bearer TOKEN_SHOULD_PASS");
  });

  await test("POST body proxied; OPTIONS not forwarded (same origin, no preflight)", async () => {
    seen.length = 0;
    const body = JSON.stringify({ deviceName: "Pixel" });
    const r = await request(port, "POST", "/v1/mobile/pair/start", { "Content-Type": "application/json", Origin: "https://x.example" }, body);
    assert.equal(r.status, 200);
    assert.equal(seen[0].body, body);
    assert.equal(seen[0].headers["content-type"], "application/json");
    assertStripped(seen[0].headers, "post");
    const o = await request(port, "OPTIONS", "/v1/mobile/commands", { Origin: "https://x.example", "Access-Control-Request-Method": "POST" });
    assert.equal(o.status, 404);
    assert.equal(seen.length, 1, "OPTIONS must not reach upstream");
  });

  await test("allow-list: only the phone's routes + methods are proxied", async () => {
    const allowed = [["GET", "/v1/health"], ["GET", "/v1/ice-servers"], ["POST", "/v1/mobile/pair/start"], ["DELETE", "/v1/mobile/pair"],
      ["GET", "/v1/mobile/status"], ["POST", "/v1/mobile/commands"], ["POST", "/v1/mobile/ws-ticket"]];
    for (const [m, p] of allowed) {
      seen.length = 0;
      const r = await request(port, m, p, m === "POST" ? { "Content-Type": "application/json" } : {}, m === "POST" ? "{}" : undefined);
      assert.equal(r.status, 200, `${m} ${p}`);
      assert.equal(seen.length, 1, `${m} ${p} should reach upstream`);
    }
    const denied = [["POST", "/v1/desktop/pair/claim"], ["GET", "/v1/admin/pairs"], ["POST", "/v1/admin/revoke"], ["GET", "/v1/desktop/ws"],
      ["GET", "/v1/mobile/pair/start"], ["POST", "/v1/mobile/pair"], ["GET", "/v1/mobile/pair"], ["PUT", "/v1/mobile/commands"],
      ["DELETE", "/v1/mobile/status"], ["GET", "/v1/mobile/other"], ["GET", "/v1/health/"], ["GET", "/v1/mobile//status"], ["GET", "/V1/health"]];
    for (const [m, p] of denied) {
      seen.length = 0;
      const r = await request(port, m, p, m === "POST" ? { "Content-Type": "application/json" } : {}, m === "POST" ? "{}" : undefined);
      assert.equal(r.status, 404, `${m} ${p}`);
      assert.equal(seen.length, 0, `${m} ${p} must not reach upstream`);
    }
  });

  await test("Host allow-list: loopback + *.trycloudflare.com only", async () => {
    for (const h of ["localhost:1234", "127.0.0.1", "abc-def-123.trycloudflare.com"]) {
      const r = await request(port, "GET", "/", { Host: h });
      assert.equal(r.status, 200, h);
    }
    for (const h of ["evil.example", "trycloudflare.com", "a.b.trycloudflare.com", "x.trycloudflare.com.evil.example"]) {
      seen.length = 0;
      const r = await request(port, "GET", "/v1/health", { Host: h });
      assert.equal(r.status, 421, h);
      assert.equal(seen.length, 0);
    }
    const w = await rawExchange(port, upgradeReq(port, "/v1/mobile/ws?ticket=x").replace(`Host: 127.0.0.1:${port}`, "Host: evil.example"));
    assert.match(w.data, /^HTTP\/1\.1 421 /);
  });

  await test("Set-Cookie and Location from upstream are dropped", async () => {
    const r = await request(port, "GET", "/v1/mobile/status");
    assert.equal(r.status, 200);
    assert.equal(r.headers["set-cookie"], undefined);
    assert.equal(r.headers.location, undefined);
  });

  await test("body over 64 KB rejected with 413", async () => {
    seen.length = 0;
    const r = await request(port, "POST", "/v1/mobile/commands", { "Content-Type": "application/json" }, "x".repeat(65 * 1024))
      .catch((e) => ({ status: e.code })); // the proxy closes the connection after the 413
    assert.ok(r.status === 413 || r.status === "ECONNRESET" || r.status === "EPIPE", `got ${r.status}`);
    assert.equal(seen.length, 0);
    // streamed (no Content-Length) bodies are cut off too
    const r2 = await new Promise((resolve) => {
      const req = http.request({ host: "127.0.0.1", port, method: "POST", path: "/v1/mobile/commands", headers: { "Transfer-Encoding": "chunked" } }, (res) => { res.resume(); resolve({ status: res.statusCode }); });
      req.on("error", (e) => resolve({ status: e.code }));
      for (let i = 0; i < 100; i++) req.write("y".repeat(8192));
      req.end();
    });
    assert.ok(r2.status === 413 || r2.status === "ECONNRESET" || r2.status === "EPIPE", `got ${r2.status}`);
    assert.equal(seen.length, 0);
  });

  await test("WebSocket upgrade piped (echo), Origin stripped, query preserved", async () => {
    seen.length = 0;
    const msg = JSON.stringify({ type: "ping", n: 42 });
    const r = await wsRoundTrip(port, `/v1/mobile/ws?pair=p123&ticket=${SECRET_TICKET}`, { Origin: "https://evil.example", "X-Forwarded-For": "1.2.3.4" }, msg);
    assert.match(r.statusLine, / 101 /);
    assert.equal(r.echo, msg);
    const up = seen.find((s) => s.method === "UPGRADE");
    assert.ok(up, "upstream saw the upgrade");
    assert.equal(up.url, `/v1/mobile/ws?pair=p123&ticket=${SECRET_TICKET}`);
    assertStripped(up.headers, "ws");
    assert.equal(up.headers.host, `127.0.0.1:${upPort}`);
  });

  await test("WebSocket non-101: refusal relayed, sockets closed, pipelined bytes never forwarded", async () => {
    seen.length = 0; leaked.length = 0;
    const smuggled = `GET /v1/admin/pairs HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://evil.example\r\n\r\n`;
    const r = await rawExchange(port, upgradeReq(port, "/v1/mobile/ws?ticket=bad") + smuggled);
    assert.match(r.data, /^HTTP\/1\.1 401 /, "401 relayed to the client");
    assert.match(r.data, /\{"error":"unauthorized"\}$/, "body relayed");
    assert.match(r.data, /\r\nConnection: close\r\n/i);
    assert.doesNotMatch(r.data, /set-cookie/i);
    assert.ok(r.closed, "client socket closed by the proxy");
    await new Promise((res) => setTimeout(res, 200));
    assert.equal(leaked.join(""), "", "bytes after a refused upgrade reached upstream");
    assert.equal(seen.filter((x) => x.method === "UPGRADE").length, 1);
    assert.ok(!seen.some((x) => x.url && x.url.includes("/v1/admin")), "smuggled request reached upstream");
  });

  await test("WebSocket handshake timeout -> 504, slot released", async () => {
    const r = await rawExchange(port, upgradeReq(port, "/v1/mobile/ws?ticket=hang"));
    assert.match(r.data, /^HTTP\/1\.1 504 /);
    assert.ok(r.closed);
  });

  await test("WebSocket concurrent upgrade cap (WS_MAX=3) -> 503", async () => {
    const hanging = [1, 2, 3].map(() => rawExchange(port, upgradeReq(port, "/v1/mobile/ws?ticket=hang")));
    await new Promise((res) => setTimeout(res, 150));
    const over = await rawExchange(port, upgradeReq(port, "/v1/mobile/ws?ticket=hang"));
    assert.match(over.data, /^HTTP\/1\.1 503 /);
    await Promise.all(hanging);
    // after the timeouts the slots are free again
    const msg = JSON.stringify({ type: "ping" });
    const ok = await wsRoundTrip(port, `/v1/mobile/ws?pair=p&ticket=${SECRET_TICKET}`, {}, msg);
    assert.equal(ok.echo, msg);
  });

  await test("WebSocket upgrade on other paths refused", async () => {
    seen.length = 0;
    const r = await wsRoundTrip(port, "/v1/desktop/ws?ticket=zzz", {}, "x");
    assert.match(r.statusLine, / 404 /);
    assert.equal(seen.length, 0);
  });

  await test("non-/v1 paths are 404", async () => {
    for (const p of ["/foo", "/.env", "/dev/serve-phone.mjs", "/v1", "/v1/../dev/selftest.mjs", "/index.html/x"]) {
      seen.length = 0;
      const r = await request(port, "GET", p);
      assert.equal(r.status, 404, p);
      assert.equal(seen.length, 0, `${p} must not reach upstream`);
    }
  });

  await test("page CSP script-src hash matches the inline script", async () => {
    const html = fs.readFileSync(path.join(HERE, "..", "index.html"), "utf8");
    assert.equal(cspHash(html), scriptHash(html), "run: node dev/update-csp.mjs");
    assert.match(html, /connect-src 'self' https:\/\/api\.camlooper\.com wss:\/\/api\.camlooper\.com wss:\/\/\*\.trycloudflare\.com/);
  });

  await test("proxy log: method + pathname only, no query strings / secrets / headers", async () => {
    await new Promise((r) => setTimeout(r, 100));
    assert.match(log, /GET \/v1\/health 200/);
    assert.match(log, /GET\(ws\) \/v1\/mobile\/ws 101/);
    for (const secret of [SECRET_TICKET, SECRET_QS, "TOKEN_SHOULD_PASS", "evil.example", "session=abc", "?"]) {
      const lines = log.split("\n").filter((l) => !l.startsWith("serve-phone:"));
      assert.ok(!lines.some((l) => l.includes(secret)), `log contains ${JSON.stringify(secret)}`);
    }
  });
} finally {
  proxy.kill("SIGTERM");
  upstream.close();
}

console.log(`\n${passed} passed${process.exitCode ? ", some FAILED" : ""}`);
if (process.exitCode) console.log("--- proxy log ---\n" + log);
process.exit(process.exitCode || 0);
