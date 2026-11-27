#!/usr/bin/env node
// "Loop a video" smoke test, fully local: the phone page sends a file over a WebRTC
// DataChannel to a fake "desktop" RTCPeerConnection in a second page; the relay WebSocket and
// /v1 calls are faked with Playwright routes (same setup as smoke-stream.mjs).
//  - happy path: stream_request{mode:"file",file}, datachannel-only offer, meta == request,
//    pieces <= 64 KiB, at most ~4 MiB buffered, byte-identical on arrival, progress UI,
//    "Preparing loop…", "Looping on your computer ✓", then Stop loop / Play loop
//  - loop_control status is asked once the computer is online
//  - over 300 MiB / not a video / a 0-byte file: refused on the phone, nothing sent
//  - a dismissed picker (change with no file) changes nothing
//  - a File whose type is "" (Android, .mov / .mkv) is sent with the type of its extension
//  - declined / too_large / unsupported rejections, an error from the computer mid-transfer
//  - Cancel sends stream_stop{reason:user}
//  - a file sent right after a camera stream: "busy" is retried, then it goes through
//  - no CSP violation and no page error in any scenario
// Saves dev/shot-file-*.png. Env: PLAYWRIGHT_CORE, CHROME.
import { loadChromium } from "./playwright.mjs";
import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const chromium = await loadChromium();
const MIB = 1024 * 1024;

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
const json = (body, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });

// Test files: real bytes for the transfers, a sparse one for the size limit.
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), "camlooper-file-"));
const mk = (name, bytes) => { const p = path.join(TMP, name); fs.writeFileSync(p, crypto.randomBytes(bytes)); return p; };
const BIG = mk("holiday clip.mp4", 24 * MIB + 12345); // not a multiple of the piece size
const SMALL = mk("small.webm", 300 * 1024);
const NOEXT_MIME = mk("clip.mkv", 70 * 1024); // Chrome reports no video/* type for it on some systems
const TEXT = mk("notes.txt", 2000);
const HUGE = path.join(TMP, "huge.mp4");
fs.closeSync(fs.openSync(HUGE, "w")); fs.truncateSync(HUGE, 301 * MIB);
const sha = (p) => crypto.createHash("sha256").update(fs.readFileSync(p)).digest("hex");

// The fake desktop. `plan` decides how it treats a file request.
async function scenario(name, plan, check) {
  const ctx = await browser.newContext({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, permissions: ["camera", "microphone"] });
  const errors = [], frames = [];
  const ctl = { ws: null, frames, requests: [] };
  ctl.toPhone = (data) => ctl.ws && ctl.ws.send(JSON.stringify({ type: "signal", from: "desktop", data }));
  ctl.rawToPhone = (frame) => ctl.ws && ctl.ws.send(JSON.stringify(frame));

  const desk = ctl.desk = await ctx.newPage();
  await desk.goto(BASE + "/");
  await desk.exposeFunction("toPhone", ctl.toPhone);
  await desk.exposeFunction("rawToPhone", ctl.rawToPhone);
  await desk.evaluate((plan) => {
    const R = window.R = { plan, requests: [], loopControls: [], loop: { playing: false, name: undefined, fromPhone: false }, bytes: 0, maxPiece: 0, pieces: [], texts: [], offer: "", meta: null };
    const say = (ch, o) => { try { ch.send(JSON.stringify(o)); } catch {} };
    window.onSig = async (d) => {
      if (d.kind === "loop_control") {
        R.loopControls.push(d.action);
        if (d.action === "stop") R.loop.playing = false;
        if (d.action === "start" && R.loop.name) R.loop.playing = true;
        if (R.plan.loopError && d.action !== "status") { window.toPhone({ kind: "loop_status", sessionId: d.sessionId, ...R.loop, playing: false, error: R.plan.loopError }); R.loop.playing = false; return; }
        if (!R.plan.muteLoop) window.toPhone({ kind: "loop_status", sessionId: d.sessionId, ...R.loop });
        return;
      }
      if (d.kind === "stream_request") {
        R.requests.push(d);
        R.sid = d.sessionId;
        if (d.mode !== "file") { window.toPhone({ kind: "stream_accept", sessionId: d.sessionId }); return; }
        if (R.plan.busyFirst && R.requests.filter((r) => r.mode === "file").length <= R.plan.busyFirst) { window.toPhone({ kind: "stream_reject", sessionId: d.sessionId, reason: "busy" }); return; }
        if (R.plan.reject) { window.toPhone({ kind: "stream_reject", sessionId: d.sessionId, reason: R.plan.reject }); return; }
        if (R.plan.hold) return;
        window.toPhone({ kind: "stream_accept", sessionId: d.sessionId });
        return;
      }
      if (d.kind === "offer") {
        R.offer = d.sdp;
        const pc = window.pc = new RTCPeerConnection({ iceServers: [] });
        pc.onconnectionstatechange = () => {
          if (pc.connectionState === "connected" && !/m=application/.test(d.sdp)) window.toPhone({ kind: "status", sessionId: R.sid, state: "live", width: 1280, height: 720, fps: 30, codec: "H264" });
        };
        pc.ondatachannel = (ev) => {
          const ch = ev.channel; ch.binaryType = "arraybuffer";
          R.label = ch.label; R.ordered = ch.ordered;
          let last = 0;
          ch.onmessage = async (m) => {
            if (typeof m.data !== "string") {
              const n = m.data.byteLength;
              R.bytes += n; R.maxPiece = Math.max(R.maxPiece, n); R.pieces.push(m.data);
              if (R.plan.errorAfter && R.bytes >= R.plan.errorAfter && !R.failed) {
                R.failed = true;
                say(ch, { t: "error", message: "Not enough free disk space" });
                setTimeout(() => window.rawToPhone({ type: "stream_stop", data: { sessionId: R.sid, reason: "error" } }), 300);
                return;
              }
              const now = Date.now();
              if (now - last >= 250) { last = now; say(ch, { t: "progress", received: R.bytes }); }
              return;
            }
            const t = JSON.parse(m.data); R.texts.push(t.t);
            if (t.t === "meta") { R.meta = t; return; }
            if (t.t !== "end") return;
            say(ch, { t: "progress", received: R.bytes });
            const buf = await new Blob(R.pieces).arrayBuffer(); R.pieces = [];
            R.sha = [...new Uint8Array(await crypto.subtle.digest("SHA-256", buf))].map((b) => b.toString(16).padStart(2, "0")).join("");
            say(ch, { t: "saved" });
            say(ch, { t: "processing" });
            await new Promise((r) => setTimeout(r, R.plan.processMs || 600));
            R.loop = { playing: true, name: R.meta.name, fromPhone: true };
            // A relay that says "done" by itself: no `looping` on the channel, no loop_status.
            if (R.plan.doneWithoutLooping) { window.rawToPhone({ type: "stream_stop", data: { sessionId: R.sid, reason: "done" } }); return; }
            say(ch, { t: "looping", name: R.meta.name });
            window.toPhone({ kind: "loop_status", sessionId: R.sid, ...R.loop });
            setTimeout(() => window.rawToPhone({ type: "stream_stop", data: { sessionId: R.sid, reason: "done" } }), 200);
          };
        };
        await pc.setRemoteDescription({ type: "offer", sdp: d.sdp });
        await pc.setLocalDescription(await pc.createAnswer());
        await new Promise((r) => { if (pc.iceGatheringState === "complete") r(); else pc.onicegatheringstatechange = () => pc.iceGatheringState === "complete" && r(); });
        window.toPhone({ kind: "answer", sessionId: R.sid, sdp: pc.localDescription.sdp });
        return;
      }
      if (d.kind === "candidate" && d.candidate && window.pc) await window.pc.addIceCandidate({ candidate: d.candidate, sdpMid: d.sdpMid, sdpMLineIndex: d.sdpMLineIndex });
    };
  }, plan);

  const page = ctl.page = await ctx.newPage();
  page.on("pageerror", (e) => errors.push("pageerror: " + e.message));
  page.on("console", (m) => { if (m.type() === "error" && !/WebSocket/.test(m.text())) errors.push("console: " + m.text()); if (process.env.VERBOSE) console.log("  [phone]", m.text()); });
  await page.route("**/v1/**", (route) => {
    const u = new URL(route.request().url());
    if (u.pathname === "/v1/mobile/ws-ticket") return route.fulfill(json({ ticket: "k_fake", expiresAt: new Date(Date.now() + 30000).toISOString() }));
    if (u.pathname === "/v1/ice-servers") return route.fulfill(json({ iceServers: [], turn: "disabled", ttl: 900 }));
    return route.fulfill(json({ error: "not_found" }, 404));
  });
  await page.routeWebSocket(/\/v1\/mobile\/ws/, (ws) => {
    ctl.ws = ws;
    ws.send(JSON.stringify({ type: "hello", protocol: 2, linked: true, desktopOnline: true, desktopName: "Fake desk", desktopProtocol: 2 }));
    ws.onMessage(async (msg) => {
      const m = JSON.parse(String(msg));
      if (m.type === "ping") return ws.send('{"type":"pong"}');
      frames.push(m.type === "signal" ? (m.data.kind === "loop_control" ? `loop_control:${m.data.action}` : m.data.kind) : `${m.type}:${m.data && m.data.reason}`);
      if (m.type === "signal") await desk.evaluate((d) => window.onSig(d), m.data);
    });
  });
  await page.addInitScript(() => {
    localStorage.setItem("camlooper-remote-phone-v1", JSON.stringify({ pairId: "p_fake", mobileToken: "t_fake", deviceName: "Pixel 8", linked: true, desktopName: "Fake desk" }));
    // Highest bufferedAmount seen right after a send, on any data channel.
    window.__maxBuffered = 0;
    const send = RTCDataChannel.prototype.send;
    RTCDataChannel.prototype.send = function (x) { const r = send.call(this, x); if (this.bufferedAmount > window.__maxBuffered) window.__maxBuffered = this.bufferedAmount; return r; };
  });
  try {
    await page.goto(BASE + "/" + (plan.debug ? "?debug=1" : ""));
    await page.waitForFunction(() => document.getElementById("deskState").textContent === "Online");
    await check(page, ctl);
    if (errors.length) throw new Error("page errors: " + errors.join(" | "));
    console.log("ok  ", name);
    return true;
  } catch (e) {
    console.log("FAIL", name, "\n    ", e.message, "\n     frames:", frames.join(", "));
    console.log("     fileMsg:", await page.textContent("#fileMsg").catch(() => "?"), "| xferMsg:", await page.textContent("#xferMsg").catch(() => "?"));
    return false;
  } finally { await ctx.close(); }
}

const text = (page, id) => page.textContent("#" + id);
const visible = (page, id) => page.evaluate((id) => { const el = document.getElementById(id); return !!el && el.getClientRects().length > 0; }, id);
const must = (cond, msg) => { if (!cond) throw new Error(msg); };
const pick = async (page, file) => { await page.setInputFiles("#fileInput", file); };
// What a picker hands over, built in the page: the name, type and size are exactly these,
// whatever this system's MIME database would say about the extension. `null`: dismissed.
const pickFake = (page, spec) => page.evaluate((spec) => {
  const input = document.getElementById("fileInput"), dt = new DataTransfer();
  if (spec) {
    const bytes = new Uint8Array(spec.size);
    for (let i = 0; i < bytes.length; i += 65536) crypto.getRandomValues(bytes.subarray(i, Math.min(bytes.length, i + 65536)));
    dt.items.add(new File([bytes], spec.name, { type: spec.type }));
  }
  input.files = dt.files;
  input.dispatchEvent(new Event("change", { bubbles: true }));
  return input.files.length ? { name: input.files[0].name, type: input.files[0].type, size: input.files[0].size } : null;
}, spec);
const settled = (page, re) => page.waitForFunction((src) => new RegExp(src).test(document.getElementById("fileMsg").textContent), re.source, { timeout: 60000 });

const results = [];

results.push(await scenario("send a video: request, datachannel-only offer, flow control, progress, looping, loop controls", { debug: true }, async (page, ctl) => {
  await page.waitForFunction(() => true);
  must(await visible(page, "loopCard"), "Loop a video card missing");
  must(!(await visible(page, "btnSend")), "Send visible before a file is picked");
  must(!(await visible(page, "loopNow")), "loop controls visible with nothing loaded");
  await pick(page, BIG);
  must((await text(page, "fileName")) === "holiday clip.mp4", "picked name not shown");
  must(/^24\.0 MB$/.test(await text(page, "fileSize")), "picked size not shown: " + (await text(page, "fileSize")));
  await page.click("#btnSend");
  // Progress is shown while it runs.
  await page.waitForFunction(() => /^[1-9]\d? %$/.test(document.getElementById("xferPct").textContent), null, { timeout: 30000 });
  must(/MB sent · .* MB confirmed/.test(await text(page, "xferNums")), "sent/confirmed line missing");
  must(await visible(page, "btnXferCancel"), "Cancel missing during the transfer");
  must(await page.evaluate(() => document.getElementById("btnStart").disabled), "Start streaming not disabled during the transfer");
  await page.screenshot({ path: path.join(HERE, "shot-file-progress.png") });
  await page.waitForFunction(() => document.getElementById("xferMsg").textContent === "Preparing loop…", null, { timeout: 60000 });
  must((await text(page, "xferPct")) === "100 %", "not 100 % while preparing");
  await settled(page, /Looping on your computer ✓/);
  const R = await ctl.desk.evaluate(() => ({ ...window.R, pieces: undefined }));
  const req = R.requests[0];
  must(req.mode === "file" && req.file && req.file.name === "holiday clip.mp4" && req.file.size === fs.statSync(BIG).size && req.file.mime === "video/mp4", "bad stream_request: " + JSON.stringify(req));
  must(typeof req.deviceLabel === "string" && req.deviceLabel, "deviceLabel missing");
  must(/^m=application /m.test(R.offer) && !/^m=(video|audio) /m.test(R.offer), "offer is not datachannel-only");
  must(R.label === "camlooper-file" && R.ordered === true, "wrong channel: " + R.label);
  must(R.texts[0] === "meta" && R.texts[R.texts.length - 1] === "end" && R.texts.length === 2, "bad text messages: " + R.texts);
  must(R.meta.name === req.file.name && R.meta.size === req.file.size && R.meta.mime === req.file.mime, "meta differs from the request");
  must(R.bytes === req.file.size, `received ${R.bytes} of ${req.file.size}`);
  must(R.maxPiece === 65536, "largest piece " + R.maxPiece);
  must(R.sha === sha(BIG), "bytes differ");
  const maxBuffered = await page.evaluate(() => window.__maxBuffered);
  must(maxBuffered <= 4 * MIB + 65536 + 64, "buffered too much: " + maxBuffered);
  console.log(`     ${R.bytes} bytes byte-identical, pieces <= ${R.maxPiece}, max buffered ${(maxBuffered / MIB).toFixed(2)} MiB`);
  // The phone did not stop the session itself: the computer ended it ("done").
  must(!ctl.frames.some((f) => f.startsWith("stream_stop")), "phone sent a stream_stop on success: " + ctl.frames);
  must(ctl.frames[0] === "loop_control:status" && ctl.frames.filter((f) => f === "loop_control:status").length === 1, "loop status not asked exactly once: " + ctl.frames);
  // Back to the picker, with the loop controls.
  must(!(await visible(page, "fileXfer")) && (await visible(page, "btnPick")), "picker not back");
  must(!(await page.evaluate(() => document.getElementById("btnStart").disabled)), "Start streaming still disabled");
  must(await visible(page, "loopNow"), "loop controls missing");
  must((await text(page, "loopNowName")) === "holiday clip.mp4" && (await text(page, "btnLoopToggle")) === "Stop loop", "loop controls wrong");
  await page.screenshot({ path: path.join(HERE, "shot-file-looping.png") });
  await page.click("#btnLoopToggle");
  await page.waitForFunction(() => document.getElementById("btnLoopToggle").textContent === "Play loop");
  must(/not playing/.test(await text(page, "loopNowState")), "stopped state not shown");
  await page.click("#btnLoopToggle");
  await page.waitForFunction(() => document.getElementById("btnLoopToggle").textContent === "Stop loop");
  must(ctl.frames.slice(-2).join() === "loop_control:stop,loop_control:start", "loop_control not sent: " + ctl.frames);
}));

results.push(await scenario("over 300 MiB, a non-video and a 0-byte file are refused on the phone; a dismissed picker keeps the choice", {}, async (page, ctl) => {
  await pick(page, HUGE);
  must(/301\.0 MB.*300 MB/.test(await text(page, "fileMsg")), "no too-large message: " + (await text(page, "fileMsg")));
  must(!(await visible(page, "btnSend")), "Send offered for a file that is too large");
  must((await text(page, "btnPick")) === "Choose a different video", "pick button label");
  await pick(page, TEXT);
  must(/doesn't look like a video/.test(await text(page, "fileMsg")), "no not-a-video message");
  must(!(await visible(page, "btnSend")), "Send offered for a text file");
  // A file the picker reports as 0 bytes (a WebView that cannot size a content:// URI).
  const empty = await pickFake(page, { name: "VID_0001.mp4", type: "video/mp4", size: 0 });
  must(empty && empty.size === 0, "the fake empty file is not empty");
  must(/This file is empty/.test(await text(page, "fileMsg")), "no empty-file message: " + (await text(page, "fileMsg")));
  must(!(await visible(page, "btnSend")), "Send offered for an empty file");
  must((await text(page, "fileName")) === "VID_0001.mp4" && (await text(page, "fileSize")) === "0.0 MB", "empty file not shown: " + (await text(page, "fileSize")));
  await pick(page, SMALL);
  must((await text(page, "fileMsg")) === "" && (await visible(page, "btnSend")), "a good file after a bad one is not accepted");
  // The picker is opened again and dismissed: the choice stays as it was.
  const before = await page.evaluate(() => document.getElementById("filePick").innerHTML);
  must((await pickFake(page, null)) === null, "the dismissed pick has a file");
  must((await page.evaluate(() => document.getElementById("filePick").innerHTML)) === before, "a dismissed picker changed the card");
  must((await text(page, "fileName")) === "small.webm" && (await visible(page, "btnSend")), "the earlier choice was lost");
  must(!ctl.frames.some((f) => f === "stream_request"), "something was sent");
}));

results.push(await scenario("a dismissed picker with nothing chosen yet does nothing", {}, async (page, ctl) => {
  await pickFake(page, null);
  must(!(await visible(page, "btnSend")) && !(await visible(page, "fileInfo")), "something appeared after a dismissed picker");
  must((await text(page, "fileMsg")) === "" && (await text(page, "btnPick")) === "Choose a video", "the card changed: " + (await text(page, "btnPick")));
  must(!ctl.frames.some((f) => f === "stream_request"), "something was sent");
}));

for (const [name, want] of [["VID 2024.MOV", "video/quicktime"], ["clip.mkv", "video/x-matroska"]]) {
  results.push(await scenario(`a File with an empty type is sent by its extension: ${name}`, {}, async (page, ctl) => {
    const f = await pickFake(page, { name, type: "", size: 200 * 1024 + 7 });
    must(f && f.type === "" && f.size === 200 * 1024 + 7, "the fake file has a type: " + JSON.stringify(f));
    must((await text(page, "fileMsg")) === "" && (await visible(page, "btnSend")), "refused: " + (await text(page, "fileMsg")));
    await page.click("#btnSend");
    await settled(page, /Looping on your computer ✓/);
    const R = await ctl.desk.evaluate(() => ({ ...window.R, pieces: undefined }));
    const req = R.requests[0];
    must(req.file.name === name && req.file.mime === want && req.file.size === f.size, "bad request: " + JSON.stringify(req.file));
    must(R.meta.mime === want && R.meta.name === name && R.meta.size === f.size, "meta differs from the request: " + JSON.stringify(R.meta));
    must(R.bytes === f.size, `received ${R.bytes} of ${f.size}`);
  }));
}

for (const [reason, re] of [["declined", /declined the video/], ["too_large", /too large for the computer/], ["unsupported", /can't play this kind of file/], ["busy", /busy with another phone session/], ["timeout", /didn't answer in time/]]) {
  results.push(await scenario(`rejected: ${reason}`, { reject: reason }, async (page, ctl) => {
    await pick(page, SMALL);
    await page.click("#btnSend");
    await settled(page, re);
    must(await visible(page, "btnSend"), "cannot send again after a rejection");
    must(!ctl.frames.some((f) => f.startsWith("stream_stop") || f === "offer"), "unexpected frames: " + ctl.frames);
  }));
}

results.push(await scenario("Cancel while waiting for the computer sends stream_stop user", { hold: true }, async (page, ctl) => {
  await pick(page, SMALL);
  await page.click("#btnSend");
  await page.waitForFunction(() => document.getElementById("xferMsg").textContent === "Waiting for the computer to accept…");
  await page.click("#btnXferCancel");
  await settled(page, /^Cancelled\.$/);
  await new Promise((r) => setTimeout(r, 200));
  must(ctl.frames.includes("stream_stop:user"), "no stream_stop user: " + ctl.frames);
  must(await visible(page, "btnSend"), "picker not back after Cancel");
}));

results.push(await scenario("an error from the computer mid-transfer is shown in plain words", { errorAfter: 2 * MIB }, async (page, ctl) => {
  await pick(page, BIG);
  await page.click("#btnSend");
  await settled(page, /The computer couldn't use this video: Not enough free disk space/);
  must(!ctl.frames.some((f) => f.startsWith("stream_stop")), "phone answered an error with a stop: " + ctl.frames);
  must(!(await visible(page, "loopNow")), "loop controls shown after a failure");
}));

results.push(await scenario("a file without a video/* type goes by its extension", {}, async (page, ctl) => {
  await pick(page, NOEXT_MIME);
  await page.click("#btnSend");
  await settled(page, /Looping on your computer ✓/);
  const req = (await ctl.desk.evaluate(() => window.R.requests))[0];
  must(/^video\//.test(req.file.mime), "mime not filled in: " + req.file.mime);
}));

results.push(await scenario("camera stream, Stop, then a video straight away: busy is retried", { busyFirst: 2, processMs: 100 }, async (page, ctl) => {
  await page.click("#btnStart");
  await page.waitForFunction(() => /live|failed/.test(document.getElementById("chip").className), null, { timeout: 40000 });
  if (!(await page.evaluate(() => document.getElementById("chip").classList.contains("live")))) { console.log("     (no ICE path here: live part skipped)"); await page.click("#btnBack"); return; }
  await page.click("#btnStop");
  await page.waitForSelector("#scrHome:not(.hidden)");
  await pick(page, SMALL);
  await page.click("#btnSend");
  await page.waitForFunction(() => document.getElementById("xferMsg").textContent === "Stopping the camera stream…");
  await settled(page, /Looping on your computer ✓/);
  const reqs = await ctl.desk.evaluate(() => window.R.requests.map((r) => [r.mode || "live", r.sessionId]));
  must(reqs.length === 4 && reqs.slice(1).every((r) => r[0] === "file") && new Set(reqs.map((r) => r[1])).size === 4, "expected live + 3 file requests with fresh ids: " + JSON.stringify(reqs));
  must(ctl.frames.indexOf("stream_stop:user") < ctl.frames.lastIndexOf("stream_request"), "live not stopped before the file request");
}));

results.push(await scenario("success is only what the computer says on the data channel; unsolicited loop_status is ignored", { doneWithoutLooping: true, processMs: 100 }, async (page, ctl) => {
  // A loop_status nobody asked for (a forged or stray relay frame) changes nothing.
  await page.waitForFunction(() => true);
  await new Promise((r) => setTimeout(r, 300)); // the page's own status request is answered first
  ctl.toPhone({ kind: "loop_status", sessionId: "forged-id-0001", playing: true, name: "evil.mp4", fromPhone: true });
  await new Promise((r) => setTimeout(r, 300));
  must(!(await visible(page, "loopNow")), "an unsolicited loop_status was applied");
  must((await text(page, "loopNowName")) !== "evil.mp4", "an unsolicited loop_status was applied");
  // stream_stop "done" over the relay, without `looping` on the channel: not shown as success.
  await pick(page, SMALL);
  await page.click("#btnSend");
  await settled(page, /The computer ended the transfer/);
  must(!/Looping on your computer ✓/.test(await text(page, "fileMsg")), "relay-only done shown as success");
  // The page then asks, and shows what the computer answers to *its* request.
  await page.waitForFunction(() => document.getElementById("loopNowName").textContent === "small.webm", null, { timeout: 10000 });
  must(ctl.frames.filter((f) => f === "loop_control:status").length === 2, "loop status not asked after the unconfirmed end: " + ctl.frames);
}));

results.push(await scenario("Play loop while the computer's camera is off says so", { loopError: "camera_off", processMs: 100 }, async (page, ctl) => {
  await pick(page, SMALL);
  await page.click("#btnSend");
  await settled(page, /Looping on your computer ✓/);
  await page.click("#btnLoopToggle");
  await page.waitForFunction(() => /virtual camera is off on your computer/.test(document.getElementById("loopMsg").textContent), null, { timeout: 10000 });
  must((await text(page, "btnLoopToggle")) === "Play loop", "button does not reflect the reported state");
}));

await browser.close();
srv.kill();
fs.rmSync(TMP, { recursive: true, force: true });
if (results.every(Boolean)) console.log("file smoke OK"); else { console.log("file smoke FAILED"); process.exit(1); }
