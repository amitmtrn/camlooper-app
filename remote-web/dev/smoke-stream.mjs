#!/usr/bin/env node
// End-to-end streaming smoke test, fully local: the phone page (fake camera) streams to a
// fake "desktop" RTCPeerConnection in a second page; the relay WebSocket and /v1 calls
// are faked with Playwright routes. Checks stream_request → accept → offer/candidates →
// answer → Live, H.264 negotiated first, offer trimmed (H.264 CB / VP8 / rtx, Opus), mic
// note + mute toggle, stats shown, Stop sends stream_stop{reason:user}.
// Saves dev/shot-stream.png. Env: PLAYWRIGHT_CORE, CHROME.
import { loadChromium } from "./playwright.mjs";
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const chromium = await loadChromium();

const srv = spawn(process.execPath, [path.join(HERE, "serve-phone.mjs")], { env: { ...process.env, PORT: "0", UPSTREAM: "http://127.0.0.1:9" }, stdio: ["ignore", "pipe", "inherit"] });
const port = await new Promise((resolve, reject) => {
  let out = ""; srv.stdout.on("data", (d) => { out += d; const m = /127\.0\.0\.1:(\d+)/.exec(out); if (m) resolve(Number(m[1])); });
  srv.on("exit", () => reject(new Error("server exited")));
});
const BASE = `http://127.0.0.1:${port}`;

const browser = await chromium.launch({
  executablePath: process.env.CHROME || "/usr/bin/google-chrome", headless: true,
  args: ["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream", "--disable-features=WebRtcHideLocalIpsWithMdns", "--allow-loopback-in-peer-connection"],
});
const ctx = await browser.newContext({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, permissions: ["camera", "microphone"] });
const frames = []; // phone -> relay frames (kinds only)
let phoneWs = null;
const errors = [];

// Fake desktop: plain page with an RTCPeerConnection, driven from Node.
const desk = await ctx.newPage();
await desk.goto(BASE + "/"); // same origin, any content
await desk.exposeFunction("toPhone", (data) => phoneWs && phoneWs.send(JSON.stringify({ type: "signal", from: "desktop", data })));
await desk.evaluate(() => {
  window.onSig = async (d) => {
    if (d.kind === "stream_request") { window.sid = d.sessionId; window.toPhone({ kind: "stream_accept", sessionId: d.sessionId }); return; }
    if (d.kind === "offer") {
      const pc = window.pc = new RTCPeerConnection({ iceServers: [] });
      pc.ontrack = () => {};
      pc.onconnectionstatechange = () => {
        if (pc.connectionState === "connected") window.toPhone({ kind: "status", sessionId: window.sid, state: "live", width: 1280, height: 720, fps: 30, codec: "H264" });
      };
      await pc.setRemoteDescription({ type: "offer", sdp: d.sdp });
      const ans = await pc.createAnswer();
      await pc.setLocalDescription(ans);
      await new Promise((r) => { if (pc.iceGatheringState === "complete") r(); else pc.onicegatheringstatechange = () => pc.iceGatheringState === "complete" && r(); });
      window.offerSdp = d.sdp;
      window.toPhone({ kind: "answer", sessionId: window.sid, sdp: pc.localDescription.sdp });
      return;
    }
    if (d.kind === "candidate" && d.candidate && window.pc) await window.pc.addIceCandidate({ candidate: d.candidate, sdpMid: d.sdpMid, sdpMLineIndex: d.sdpMLineIndex });
  };
});

const phone = await ctx.newPage();
phone.on("pageerror", (e) => errors.push(e.message));
const json = (body, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
await phone.route("**/v1/**", (route) => {
  const u = new URL(route.request().url());
  if (u.pathname === "/v1/mobile/ws-ticket") return route.fulfill(json({ ticket: "k_fake", expiresAt: new Date(Date.now() + 30000).toISOString() }));
  if (u.pathname === "/v1/ice-servers") return route.fulfill(json({ iceServers: [], turn: "disabled", ttl: 900 }));
  return route.fulfill(json({ error: "not_found" }, 404));
});
await phone.routeWebSocket(/\/v1\/mobile\/ws/, (ws) => {
  phoneWs = ws;
  ws.send(JSON.stringify({ type: "hello", protocol: 2, linked: true, desktopOnline: true, desktopName: "Fake desk", desktopProtocol: 2 }));
  ws.onMessage(async (msg) => {
    const m = JSON.parse(String(msg));
    if (m.type === "ping") { ws.send('{"type":"pong"}'); return; }
    if (m.type === "signal" && m.data.kind === "loop_control") return; // the page asks for the loop state on load; not part of a stream
    frames.push(m.type === "signal" ? `signal:${m.data.kind}${m.data.kind === "candidate" && m.data.candidate === "" ? "(end)" : ""}` : `${m.type}:${m.data && m.data.reason}`);
    if (m.type === "signal") await desk.evaluate((d) => window.onSig(d), m.data);
  });
});
await phone.addInitScript(() => localStorage.setItem("camlooper-remote-phone-v1", JSON.stringify({ pairId: "p_fake", mobileToken: "t_fake", deviceName: "Pixel 8", linked: true, desktopName: "Fake desk" })));

try {
  const consoleText = [];
  phone.on("console", (m) => { consoleText.push(m.text()); if (process.env.VERBOSE) console.log("  [phone]", m.text()); });
  await phone.goto(BASE + "/?debug=1");
  await phone.waitForFunction(() => document.getElementById("deskState").textContent === "Online");
  await phone.click("#btnStart");
  await phone.waitForFunction(() => /live|failed/.test(document.getElementById("chip").className), null, { timeout: 40000 });
  const live = await phone.evaluate(() => document.getElementById("chip").classList.contains("live"));
  const firstVideoCodec = await desk.evaluate(() => { const m = /m=video \S+ \S+ (\d+)/.exec(window.offerSdp || ""); if (!m) return "?"; const pt = m[1]; return (new RegExp(`a=rtpmap:${pt} ([^/]+)`).exec(window.offerSdp) || [])[1] + " " + ((new RegExp(`a=fmtp:${pt} (.*)`).exec(window.offerSdp) || [])[1] || ""); });
  console.log("first video codec in offer:", firstVideoCodec.trim());
  if (!/^H264/i.test(firstVideoCodec)) throw new Error("H.264 not preferred");
  // Offer is trimmed: H.264 constrained baseline (pm=1) / VP8 / rtx only; audio Opus only.
  const offered = await desk.evaluate(() => {
    const sdp = window.offerSdp || "";
    const sec = (k) => { const i = sdp.indexOf(`m=${k} `); if (i < 0) return ""; const j = sdp.indexOf("\r\nm=", i + 1); return sdp.slice(i, j < 0 ? undefined : j); };
    const names = (txt) => [...txt.matchAll(/a=rtpmap:(\d+) ([^/\r\n]+)/g)].map((m) => ({ pt: m[1], name: m[2], fmtp: (new RegExp(`a=fmtp:${m[1]} (.*)`).exec(txt) || [])[1] || "" }));
    return { video: names(sec("video")), audio: names(sec("audio")), bytes: sdp.length };
  });
  const bad = offered.video.filter((c) => !(/^(VP8|rtx)$/i.test(c.name) || (/^H264$/i.test(c.name) && /packetization-mode=1/.test(c.fmtp) && /profile-level-id=42(e0|00)1f/i.test(c.fmtp))));
  console.log("offer:", offered.bytes, "bytes; video:", [...new Set(offered.video.map((c) => c.name))].join("/"), "; audio:", [...new Set(offered.audio.map((c) => c.name))].join("/"));
  if (bad.length) throw new Error("unexpected video codecs offered: " + bad.map((c) => c.name + " " + c.fmtp).join(", "));
  if (offered.audio.some((c) => !/^opus$/i.test(c.name))) throw new Error("non-Opus audio offered");
  if (offered.bytes > 12000) throw new Error("offer too large for the relay frame cap: " + offered.bytes);
  const order = (k) => frames.indexOf(k);
  if (!(order("signal:stream_request") === 0 && order("signal:offer") > 0 && order("signal:candidate(end)") > order("signal:offer"))) throw new Error("bad frame order");
  if (live) {
    // mic is announced and can be muted
    if (!/microphone/i.test(await phone.textContent("#micNote"))) throw new Error("mic note missing");
    await phone.click("#btnMic");
    if (await phone.evaluate(() => document.getElementById("btnMic").getAttribute("aria-pressed")) !== "true") throw new Error("mic toggle failed");
    if (!/muted/i.test(await phone.textContent("#micNote"))) throw new Error("mic muted note missing");
    await phone.waitForFunction(() => /\d+×\d+/.test(document.getElementById("stRes").textContent) && /kbps/.test(document.getElementById("stRate").textContent), null, { timeout: 15000 });
    const stats = await phone.evaluate(() => Object.fromEntries(["stRes", "stFps", "stRate", "stCodec", "stPath", "stRtt"].map((k) => [k, document.getElementById(k).textContent])));
    console.log("LIVE path. stats:", JSON.stringify(stats), "| desktop:", await phone.textContent("#deskRecv"));
    await phone.screenshot({ path: path.join(HERE, "shot-stream.png") });
    await phone.click("#btnStop");
    await phone.waitForSelector("#scrHome:not(.hidden)");
    await new Promise((r) => setTimeout(r, 200));
    if (frames[frames.length - 1] !== "stream_stop:user") throw new Error("no stream_stop user");
  } else {
    // No usable ICE candidates (e.g. a container where Chrome can't enumerate interfaces):
    // verify the 20 s connect-deadline (ICE failure) path instead.
    const fail = await phone.textContent("#streamFail");
    console.log("ICE-FAILED path (no network path in this environment). message:", fail);
    await phone.screenshot({ path: path.join(HERE, "shot-stream-failed.png") });
    if (frames[frames.length - 1] !== "stream_stop:ice_failed") throw new Error("no stream_stop ice_failed");
    if (!/same Wi-Fi/.test(fail)) throw new Error("ice failure message missing");
    if (await phone.evaluate(() => !!document.getElementById("preview").srcObject)) throw new Error("camera not released");
    await phone.click("#btnBack");
    await phone.waitForSelector("#scrHome:not(.hidden)");
  }
  console.log("frames:", [...new Set(frames)].join(", "));
  const all = consoleText.join("\n");
  for (const secret of ["t_fake", "k_fake", "v=0", "a=candidate", "typ host"]) if (all.includes(secret)) throw new Error(`debug console leaked ${secret}`);
  if (!all.includes("[camlooper]")) throw new Error("debug logging not active");
  if (errors.length) throw new Error("page errors: " + errors.join(" | "));
  console.log(`stream smoke OK (${live ? "live" : "ice_failed"} path)`);
} catch (e) {
  console.log("frames so far:", frames.join(", "));
  console.log("phone msg:", await phone.textContent("#streamMsg").catch(() => "?"), "|", await phone.textContent("#streamFail").catch(() => "?"));
  throw e;
} finally {
  await browser.close();
  srv.kill();
}
