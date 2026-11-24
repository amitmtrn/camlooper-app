#!/usr/bin/env node
// Behaviour smoke for review fixes, fully local (mocked relay socket + /v1, fake camera):
//  - mic is announced and can be muted (item 6)
//  - a stale stream_stop without sessionId is ignored after accept (item 18)
//  - remote reason "constructor" doesn't render Object.prototype (item 19)
//  - desktop answers with video inactive -> codec-specific error (item 9)
//  - relay error too_large while connecting -> fails fast with a generic error (item 10)
//  - socket replaced (4000) while requesting -> errReplaced (item 16)
//  - "Try again" with the desktop offline shows the message on the Stream screen (item 7)
// Env: PLAYWRIGHT_CORE, CHROME.
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
  args: ["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"],
});
const json = (body, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });

// One phone page per scenario. onSignal(m, ctl) decides what the fake relay/desktop does.
async function scenario(name, onSignal, check) {
  const ctx = await browser.newContext({ viewport: { width: 412, height: 915 }, isMobile: true, hasTouch: true, permissions: ["camera", "microphone"] });
  const page = await ctx.newPage();
  const errors = [], frames = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => { if (m.type() === "error" && !/WebSocket/.test(m.text())) errors.push(m.text()); });
  await page.route("**/v1/**", (route) => {
    const u = new URL(route.request().url());
    if (u.pathname === "/v1/mobile/ws-ticket") return route.fulfill(json({ ticket: "k_fake", expiresAt: new Date(Date.now() + 30000).toISOString() }));
    if (u.pathname === "/v1/ice-servers") return route.fulfill(json({ iceServers: [], turn: "disabled", ttl: 900 }));
    return route.fulfill(json({ error: "not_found" }, 404));
  });
  const ctl = { ws: null, desk: null, page };
  ctl.toPhone = (data) => ctl.ws.send(JSON.stringify({ type: "signal", from: "desktop", data }));
  await page.routeWebSocket(/\/v1\/mobile\/ws/, (ws) => {
    ctl.ws = ws;
    ws.send(JSON.stringify({ type: "hello", protocol: 2, linked: true, desktopOnline: true, desktopName: "Fake desk", desktopProtocol: 2 }));
    ws.onMessage(async (msg) => {
      const m = JSON.parse(String(msg));
      if (m.type === "ping") return ws.send('{"type":"pong"}');
      frames.push(m.type === "signal" ? m.data.kind : `${m.type}:${m.data && m.data.reason}`);
      await onSignal(m, ctl);
    });
  });
  await page.addInitScript(() => localStorage.setItem("camlooper-remote-phone-v1", JSON.stringify({ pairId: "p_fake", mobileToken: "t_fake", deviceName: "Pixel 8", linked: true, desktopName: "Fake desk" })));
  try {
    await page.goto(BASE + "/");
    await page.waitForFunction(() => document.getElementById("deskState").textContent === "Online");
    await check(page, ctl, frames);
    if (errors.length) throw new Error("page errors: " + errors.join(" | "));
    console.log(`ok   ${name}`);
  } catch (e) {
    console.log(`FAIL ${name}\n     ${e.message}\n     frames: ${frames.join(", ")}`);
    process.exitCode = 1;
  } finally { await ctx.close(); }
}
const failText = (page) => page.waitForFunction(() => !document.getElementById("streamFail").classList.contains("hidden"), null, { timeout: 10000 }).then(() => page.textContent("#streamFail"));

try {
  await scenario("mic announced + mute toggle; stale/bogus stream_stop ignored", async (m, ctl) => {
    if (m.type === "signal" && m.data.kind === "stream_request") {
      ctl.sid = m.data.sessionId;
      ctl.toPhone({ kind: "stream_accept", sessionId: ctl.sid });
    }
  }, async (page, ctl) => {
    if (!/microphone/i.test(await page.textContent("#scrHome"))) throw new Error("Home doesn't mention the mic");
    await page.click("#btnStart");
    await page.waitForFunction(() => /Microphone on/.test(document.getElementById("micNote").textContent));
    await page.click("#btnMic");
    await page.waitForFunction(() => /muted/i.test(document.getElementById("micNote").textContent) && document.getElementById("btnMic").getAttribute("aria-pressed") === "true");
    const enabled = await page.evaluate(() => document.getElementById("preview").srcObject.getAudioTracks().map((t) => t.enabled));
    if (enabled.length !== 1 || enabled[0] !== false) throw new Error("audio track not muted: " + JSON.stringify(enabled));
    await page.waitForFunction(() => /Connecting/.test(document.getElementById("chip").textContent));
    ctl.ws.send(JSON.stringify({ type: "stream_stop", data: { reason: "user" } }));            // no sessionId, after accept
    ctl.ws.send(JSON.stringify({ type: "stream_stop", data: { sessionId: "old", reason: "user" } }));
    await new Promise((r) => setTimeout(r, 300));
    if (await page.isHidden("#scrStream")) throw new Error("stale stream_stop ended the session");
    ctl.ws.send(JSON.stringify({ type: "stream_stop", data: { sessionId: ctl.sid, reason: "constructor" } }));
    await page.waitForSelector("#scrHome:not(.hidden)");
    const msg = await page.textContent("#homeMsg");
    if (msg !== "Streaming stopped.") throw new Error("bad stop message: " + msg);
  });

  await scenario("desktop rejects video -> codec-specific error", async (m, ctl) => {
    if (m.type !== "signal") return;
    if (m.data.kind === "stream_request") { ctl.sid = m.data.sessionId; ctl.toPhone({ kind: "stream_accept", sessionId: ctl.sid }); }
    if (m.data.kind === "offer") {
      const sdp = await ctl.page.evaluate(async (offer) => {
        const pc = new RTCPeerConnection();
        await pc.setRemoteDescription({ type: "offer", sdp: offer });
        for (const t of pc.getTransceivers()) if (t.receiver.track.kind === "video") t.direction = "inactive";
        const a = await pc.createAnswer(); await pc.setLocalDescription(a);
        return pc.localDescription.sdp;
      }, m.data.sdp);
      ctl.toPhone({ kind: "answer", sessionId: ctl.sid, sdp });
    }
  }, async (page, ctl, frames) => {
    await page.click("#btnStart");
    const t = await failText(page);
    if (!/video format \(H\.264.*VP8|video format \(VP8/.test(t)) throw new Error("no codec error: " + t);
    await new Promise((r) => setTimeout(r, 200));
    if (!frames.includes("stream_stop:error")) throw new Error("no stream_stop:error");
  });

  await scenario("relay too_large while connecting -> fails fast", async (m, ctl) => {
    if (m.type !== "signal") return;
    if (m.data.kind === "stream_request") { ctl.sid = m.data.sessionId; ctl.toPhone({ kind: "stream_accept", sessionId: ctl.sid }); }
    if (m.data.kind === "offer") ctl.ws.send(JSON.stringify({ type: "error", error: "too_large" }));
  }, async (page) => {
    const t0 = Date.now();
    await page.click("#btnStart");
    const t = await failText(page);
    if (t !== "Something went wrong. Please try again.") throw new Error("bad message: " + t);
    if (Date.now() - t0 > 8000) throw new Error("not fast");
  });

  await scenario("socket replaced (4000) while requesting -> errReplaced", async (m, ctl) => {
    if (m.type === "signal" && m.data.kind === "stream_request") ctl.ws.close({ code: 4000, reason: "replaced" });
  }, async (page) => {
    await page.click("#btnStart");
    const t = await failText(page);
    if (!/another tab/.test(t)) throw new Error("bad message: " + t);
  });

  await scenario("Try again with desktop offline -> message on the Stream screen", async (m, ctl) => {
    if (m.type === "signal" && m.data.kind === "stream_request") ctl.toPhone({ kind: "stream_reject", sessionId: m.data.sessionId, reason: "toString" });
  }, async (page, ctl) => {
    await page.click("#btnStart");
    const t = await failText(page);
    if (t !== "The computer couldn't start the camera.") throw new Error("bad reject message: " + t);
    ctl.ws.send(JSON.stringify({ type: "peer", role: "desktop", online: false }));
    await page.waitForFunction(() => document.getElementById("deskState").textContent === "Offline");
    await page.click("#btnRetry");
    await page.waitForFunction(() => /computer is offline/.test(document.getElementById("streamFail").textContent));
    if (await page.isHidden("#scrStream")) throw new Error("left the Stream screen");
  });
} finally {
  await browser.close();
  srv.kill();
}
console.log(process.exitCode ? "behaviour smoke FAILED" : "behaviour smoke OK");
