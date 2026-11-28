#!/usr/bin/env node
// "Record a loop" smoke test, fully local: the phone page (fake camera) records a take to a
// fake "desktop" RTCPeerConnection in a second page; the relay WebSocket and /v1 calls are
// faked with Playwright routes (same setup as smoke-stream.mjs).
//  - the Phase 3 "send a file" card is never shown (picker, Send & loop, transfer progress)
//  - app protocol hello: sent once with appProtocol 3; Record a loop is off until hello_ok
//    (appProtocol >= 3, features with "record"); no answer in 5 s, an old appProtocol, a
//    missing "record" or a forged hello_ok → "Update CamLooper on your computer to record a
//    loop.", Record stays off and Start streaming says "update" without sending a request
//  - a take: stream_request{mode:"record"}, the live media offer (video), REC 0:0x / 1:00,
//    Switch camera off, Stop → stream_stop{user}, camera released, "Preparing loop…",
//    then the computer's news (stream_stop done / status record.looping / loop_status) →
//    "Looping on your computer now", lock-or-close copy, Stop loop / Play loop
//  - the phone stops by itself at 1:00 (clock taken from the computer's record.elapsedMs)
//  - too short, failed on the computer, cancelled on the computer, declined / busy / unsupported
//  - Start streaming after a hello_ok still sends a request without a mode
//  - no CSP violation and no page error in any scenario
// Saves dev/shot-record-*.png. Env: PLAYWRIGHT_CORE, CHROME.
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
const json = (body, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
const UPDATE = "Update CamLooper on your computer to record a loop.";

// The fake desktop. `plan` decides how it answers hello, a request and the end of a take.
async function scenario(name, plan, check) {
  const ctx = await browser.newContext({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, permissions: ["camera", "microphone"] });
  const errors = [], frames = [];
  const ctl = { ws: null, frames, hellos: [] };
  ctl.toPhone = (data) => ctl.ws && ctl.ws.send(JSON.stringify({ type: "signal", from: "desktop", data }));
  ctl.rawToPhone = (frame) => ctl.ws && ctl.ws.send(JSON.stringify(frame));

  const desk = ctl.desk = await ctx.newPage();
  await desk.goto(BASE + "/");
  await desk.exposeFunction("toPhone", ctl.toPhone);
  await desk.exposeFunction("rawToPhone", ctl.rawToPhone);
  await desk.evaluate((plan) => {
    const R = window.R = { plan, requests: [], loopControls: [], loop: { playing: false, name: undefined, fromPhone: false }, offer: "", stops: [] };
    const stop = (reason) => window.rawToPhone({ type: "stream_stop", data: { sessionId: R.sid, reason } });
    const status = (record) => window.toPhone({ kind: "status", sessionId: R.sid, state: "live", width: 1280, height: 720, fps: 30, codec: "H264", mode: "record", record: { capMs: 60000, ...record } });
    // The take has ended (the phone's stop, or this computer's own): prepare, then report.
    window.finish = async () => {
      const end = R.plan.end || "done";
      if (end === "silent") return;
      await new Promise((r) => setTimeout(r, R.plan.processMs || 500));
      if (end === "failed") { status({ phase: "failed", elapsedMs: 3000, error: "The recording was too incomplete to loop. Try again." }); stop("error"); return; }
      if (end === "too_short") { stop("too_short"); return; }
      R.loop = { playing: true, name: "Recording from Pixel 8.mp4", fromPhone: true };
      if (end === "status") status({ phase: "looping", elapsedMs: 3000 });
      else if (end === "loop_status") window.toPhone({ kind: "loop_status", sessionId: R.sid, ...R.loop });
      else stop("done");
    };
    window.onStop = (data) => { R.stops.push(data.reason); if (data.sessionId === R.sid) window.finish(); };
    window.onSig = async (d) => {
      if (d.kind === "loop_control") {
        R.loopControls.push(d.action);
        if (d.action === "stop") R.loop.playing = false;
        if (d.action === "start" && R.loop.name) R.loop.playing = true;
        window.toPhone({ kind: "loop_status", sessionId: d.sessionId, ...R.loop });
        return;
      }
      if (d.kind === "stream_request") {
        R.requests.push(d);
        R.sid = d.sessionId;
        if (R.plan.reject) { window.toPhone({ kind: "stream_reject", sessionId: d.sessionId, reason: R.plan.reject }); return; }
        window.toPhone({ kind: "stream_accept", sessionId: d.sessionId });
        return;
      }
      if (d.kind === "offer") {
        R.offer = d.sdp;
        const record = R.requests[R.requests.length - 1].mode === "record";
        const pc = window.pc = new RTCPeerConnection({ iceServers: [] });
        pc.ontrack = () => {};
        pc.onconnectionstatechange = () => {
          if (pc.connectionState !== "connected") return;
          if (!record) { window.toPhone({ kind: "status", sessionId: R.sid, state: "live", width: 1280, height: 720, fps: 30, codec: "H264" }); return; }
          status({ phase: "recording", elapsedMs: R.plan.elapsedMs || 0 });
          // This computer ends the take itself: its own limit / Stop, or its Cancel.
          if (R.plan.deskEnds === "processing") setTimeout(() => { status({ phase: "processing", elapsedMs: 60000 }); window.finish(); }, 1200);
          if (R.plan.deskEnds === "cancel") setTimeout(() => stop("user"), 800);
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
      if (m.type === "stream_stop") { await desk.evaluate((d) => window.onStop(d), m.data).catch(() => {}); return; }
      if (m.type !== "signal") return;
      if (m.data.kind === "hello") {
        ctl.hellos.push(m.data);
        const h = plan.hello === undefined ? { appProtocol: 3, features: ["live", "record"] } : plan.hello;
        if (!h) return; // a computer that does not know hello
        const answer = () => ctl.toPhone({ kind: "hello_ok", sessionId: h.forge ? "forged-hello-0001" : m.data.sessionId, appProtocol: h.appProtocol, features: h.features });
        if (plan.helloDelay) setTimeout(answer, plan.helloDelay); else answer();
        return;
      }
      await desk.evaluate((d) => window.onSig(d), m.data).catch(() => {}); // the scenario may be over
    });
  });
  await page.addInitScript(() => localStorage.setItem("camlooper-remote-phone-v1", JSON.stringify({ pairId: "p_fake", mobileToken: "t_fake", deviceName: "Pixel 8", linked: true, desktopName: "Fake desk" })));
  try {
    await page.goto(BASE + "/" + (plan.debug ? "?debug=1" : ""));
    await page.waitForFunction(() => document.getElementById("deskState").textContent === "Online");
    await check(page, ctl);
    if (errors.length) throw new Error("page errors: " + errors.join(" | "));
    console.log("ok  ", name);
    return true;
  } catch (e) {
    console.log("FAIL", name, "\n    ", e.message, "\n     frames:", frames.join(", "));
    console.log("     takeMsg:", await page.textContent("#takeMsg").catch(() => "?"), "| streamFail:", await page.textContent("#streamFail").catch(() => "?"), "| camMsg:", await page.textContent("#camMsg").catch(() => "?"));
    return false;
  } finally { await ctx.close(); }
}

const text = (page, id) => page.textContent("#" + id);
const visible = (page, id) => page.evaluate((id) => { const el = document.getElementById(id); return !!el && el.getClientRects().length > 0; }, id);
const disabled = (page, id) => page.evaluate((id) => document.getElementById(id).disabled, id);
const must = (cond, msg) => { if (!cond) throw new Error(msg); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const FILE_UI = ["fileCard", "fileInput", "filePick", "btnPick", "btnSend", "fileInfo", "fileXfer", "xferProg", "btnXferCancel", "fileMsg"];
const noFileUi = async (page, when) => { for (const id of FILE_UI) must(!(await visible(page, id)), `#${id} is visible ${when}`); };
const recordReady = (page) => page.waitForFunction(() => !document.getElementById("btnRecord").disabled, null, { timeout: 10000 });
// Record a loop → the take is running. false: no ICE path in this environment.
let noIce = false;
async function startTake(page) {
  await recordReady(page);
  await page.click("#btnRecord");
  await page.waitForFunction(() => /rec|failed/.test(document.getElementById("chip").className), null, { timeout: 40000 });
  if (await page.evaluate(() => document.getElementById("chip").classList.contains("rec"))) return true;
  noIce = true; console.log("     (no ICE path here: the take itself is skipped)");
  return false;
}
const home = (page) => page.waitForSelector("#scrHome:not(.hidden)");
const takeSays = (page, re, timeout = 15000) => page.waitForFunction((src) => new RegExp(src).test(document.getElementById("takeMsg").textContent), re.source, { timeout });

const results = [];

results.push(await scenario("no file card; hello once; Record off until hello_ok; a take loops (stream_stop done); loop controls", { debug: true, helloDelay: 700 }, async (page, ctl) => {
  await noFileUi(page, "on the home screen");
  must(await visible(page, "btnRecord") && (await text(page, "btnRecord")) === "Record a loop", "Record a loop missing");
  must(await visible(page, "btnStart"), "Start streaming missing");
  const [a, b] = await page.evaluate(() => ["btnStart", "btnRecord"].map((id) => document.getElementById(id).getBoundingClientRect().top));
  must(Math.abs(a - b) < 2, "Record a loop is not next to Start streaming");
  must(await disabled(page, "btnRecord"), "Record enabled before hello_ok");
  must(!(await visible(page, "loopCard")), "loop card shown with nothing loaded");
  await recordReady(page);
  must((await text(page, "helloNote")) === "", "update note shown for a 3b computer");
  const h = ctl.hellos[0];
  must(ctl.hellos.length === 1 && h.appProtocol === 3 && /^[A-Za-z0-9_-]{16,64}$/.test(h.sessionId) && Object.keys(h).sort().join() === "appProtocol,kind,sessionId", "bad hello: " + JSON.stringify(ctl.hellos));
  if (!(await startTake(page))) return;
  must(/^REC 0:0\d \/ 1:00$/.test(await text(page, "chip")), "bad take clock: " + (await text(page, "chip")));
  must(await disabled(page, "btnSwitch"), "Switch camera enabled during a take");
  must(/while recording/.test(await text(page, "keepOpen")), "keep-open copy: " + (await text(page, "keepOpen")));
  must(await page.evaluate(() => !!document.getElementById("preview").srcObject), "no preview");
  const R = await ctl.desk.evaluate(() => window.R);
  must(R.requests.length === 1 && R.requests[0].mode === "record" && !("file" in R.requests[0]) && R.requests[0].deviceLabel === "Pixel 8", "bad stream_request: " + JSON.stringify(R.requests));
  must(/^m=video [1-9]/m.test(R.offer) && !/^m=application /m.test(R.offer), "the offer is not the live media offer");
  await page.waitForFunction(() => /^REC 0:0[2-9]/.test(document.getElementById("chip").textContent), null, { timeout: 10000 });
  await page.screenshot({ path: path.join(HERE, "shot-record-take.png") });
  await page.click("#btnStop");
  await home(page);
  must(await visible(page, "takeBusy") && /Preparing loop…/.test(await text(page, "takeBusy")), "no Preparing loop…");
  must(await disabled(page, "btnRecord") && await disabled(page, "btnStart"), "a new session is offered while the loop is prepared");
  must(await page.evaluate(() => !document.getElementById("preview").srcObject), "camera not released after Stop");
  await takeSays(page, /^Recording done ✓$/);
  must(ctl.frames.filter((f) => f.startsWith("stream_stop")).join() === "stream_stop:user", "expected one stream_stop user: " + ctl.frames);
  must(await visible(page, "loopNow") && (await text(page, "loopNowState")) === "Looping on your computer now", "not shown as looping: " + (await text(page, "loopNowState")));
  must(await visible(page, "loopAway") && /lock this phone or close the app. The loop keeps playing/.test(await text(page, "loopAway")), "lock/close copy missing");
  must(!(await visible(page, "takeBusy")), "still preparing");
  must(!(await disabled(page, "btnRecord")) && !(await disabled(page, "btnStart")), "buttons not back");
  await page.waitForFunction(() => document.getElementById("loopNowName").textContent === "Recording from Pixel 8.mp4", null, { timeout: 10000 });
  await page.screenshot({ path: path.join(HERE, "shot-record-looping.png"), fullPage: true });
  await page.click("#btnLoopToggle");
  await page.waitForFunction(() => document.getElementById("btnLoopToggle").textContent === "Play loop");
  must(/not playing/.test(await text(page, "loopNowState")) && !(await visible(page, "loopAway")), "stopped state not shown");
  await page.click("#btnLoopToggle");
  await page.waitForFunction(() => document.getElementById("btnLoopToggle").textContent === "Stop loop");
  must(ctl.frames.slice(-2).join() === "loop_control:stop,loop_control:start", "loop_control not sent: " + ctl.frames);
  must(ctl.hellos.length === 1, "hello sent more than once: " + ctl.hellos.length);
  await noFileUi(page, "after a take");
}));

for (const [what, hello] of [
  ["no hello_ok within 5 s", null],
  ["hello_ok with appProtocol 2", { appProtocol: 2, features: ["live", "record"] }],
  ["hello_ok without the record feature", { appProtocol: 3, features: ["live", "file"] }],
  ["a hello_ok for another id", { appProtocol: 3, features: ["live", "record"], forge: true }],
]) {
  results.push(await scenario(`old computer (${what}): update note, Record off, Start streaming says update`, { hello }, async (page, ctl) => {
    const t0 = Date.now();
    await page.waitForFunction((want) => document.getElementById("helloNote").textContent === want, UPDATE, { timeout: 9000 });
    const waited = Date.now() - t0;
    if (!hello || hello.forge) must(waited > 3500 && waited < 7500, `update note after ${waited} ms, expected about 5 s`);
    must(await visible(page, "helloNote"), "update note not visible");
    must(await disabled(page, "btnRecord"), "Record enabled for an old computer");
    await page.click("#btnStart");
    await page.waitForFunction(() => /Update CamLooper on your computer/.test(document.getElementById("camMsg").textContent));
    await sleep(300);
    must(!ctl.frames.includes("stream_request"), "a request went to an old computer: " + ctl.frames);
    must(await visible(page, "scrHome"), "left the home screen");
    await noFileUi(page, "with an old computer");
  }));
}

for (const end of ["status", "loop_status"]) {
  results.push(await scenario(`a take loops when the computer says so with ${end}`, { end }, async (page, ctl) => {
    if (!(await startTake(page))) return;
    await page.waitForFunction(() => /^REC 0:0[2-9]/.test(document.getElementById("chip").textContent), null, { timeout: 10000 });
    await page.click("#btnStop");
    await takeSays(page, /^Recording done ✓$/);
    must((await text(page, "loopNowState")) === "Looping on your computer now" && (await text(page, "btnLoopToggle")) === "Stop loop", "not shown as looping");
  }));
}

results.push(await scenario("the phone stops by itself at 1:00 (clock follows the computer's elapsedMs)", { elapsedMs: 57500 }, async (page, ctl) => {
  if (!(await startTake(page))) return;
  await page.waitForFunction(() => /^REC 0:5[789] \/ 1:00$/.test(document.getElementById("chip").textContent), null, { timeout: 5000 });
  const t0 = Date.now();
  await home(page);
  must(Date.now() - t0 < 5000, "did not stop at 1:00");
  await takeSays(page, /^Recording done ✓$/);
  must(ctl.frames.filter((f) => f.startsWith("stream_stop")).join() === "stream_stop:user", "expected one stream_stop user: " + ctl.frames);
}));

results.push(await scenario("Stop within 2 s: too short, nothing to wait for", { end: "silent" }, async (page, ctl) => {
  if (!(await startTake(page))) return;
  await page.click("#btnStop");
  await home(page);
  await takeSays(page, /too short to loop/);
  must(!(await visible(page, "takeBusy")), "waiting for a take that is too short");
  must(!(await disabled(page, "btnRecord")), "cannot record again");
  must(ctl.frames.includes("stream_stop:user"), "no stream_stop user: " + ctl.frames);
  must(!(await visible(page, "loopNow")), "loop controls after a take that was too short");
}));

for (const [end, re] of [["failed", /too incomplete to loop\. Try again\./], ["too_short", /too short to loop/]]) {
  results.push(await scenario(`the computer could not loop the take: ${end}`, { end }, async (page, ctl) => {
    if (!(await startTake(page))) return;
    await page.waitForFunction(() => /^REC 0:0[2-9]/.test(document.getElementById("chip").textContent), null, { timeout: 10000 });
    await page.click("#btnStop");
    await takeSays(page, re);
    must(!(await visible(page, "takeBusy")) && !(await visible(page, "loopNow")), "shown as looping or still preparing");
    must(!(await disabled(page, "btnRecord")), "cannot record again");
  }));
}

results.push(await scenario("the computer ends the take itself (processing) and loops it", { deskEnds: "processing" }, async (page, ctl) => {
  if (!(await startTake(page))) return;
  await home(page);
  await takeSays(page, /^Recording done ✓$/);
  must(!ctl.frames.some((f) => f.startsWith("stream_stop")), "phone answered the computer's own end with a stop: " + ctl.frames);
  must(await page.evaluate(() => !document.getElementById("preview").srcObject), "camera not released");
}));

results.push(await scenario("Cancel on the computer right away: said so, not an error", { deskEnds: "cancel", end: "silent" }, async (page, ctl) => {
  if (!(await startTake(page))) return;
  await home(page);
  await takeSays(page, /cancelled on the computer/);
  must(!(await page.evaluate(() => document.getElementById("takeMsg").classList.contains("err"))), "a cancel is shown as an error");
  must(!ctl.frames.some((f) => f.startsWith("stream_stop")), "phone answered a stop with a stop: " + ctl.frames);
}));

for (const [reason, re] of [["declined", /declined the recording/], ["busy", /busy with another phone session/], ["timeout", /didn't answer in time/], ["unsupported", /Update CamLooper on your computer to record a loop/]]) {
  results.push(await scenario(`rejected: ${reason}; Try again asks for a take again`, { reject: reason }, async (page, ctl) => {
    await recordReady(page);
    await page.click("#btnRecord");
    await page.waitForFunction(() => !document.getElementById("streamFail").classList.contains("hidden"), null, { timeout: 10000 });
    must(re.test(await text(page, "streamFail")), "wrong message: " + (await text(page, "streamFail")));
    await page.click("#btnRetry");
    await sleep(800);
    const modes = await ctl.desk.evaluate(() => window.R.requests.map((r) => r.mode));
    must(modes.length === 2 && modes.every((m) => m === "record"), "Try again did not ask for a take: " + JSON.stringify(modes));
    must(!ctl.frames.some((f) => f === "offer"), "an offer was sent after a rejection");
  }));
}

results.push(await scenario("Start streaming with a 3b computer: a live request without a mode, Switch camera on", {}, async (page, ctl) => {
  await recordReady(page);
  await page.click("#btnStart");
  await page.waitForFunction(() => /live|failed/.test(document.getElementById("chip").className), null, { timeout: 40000 });
  const req = (await ctl.desk.evaluate(() => window.R.requests))[0];
  must(req && !("mode" in req), "live request carries a mode: " + JSON.stringify(req));
  if (!(await page.evaluate(() => document.getElementById("chip").classList.contains("live")))) { console.log("     (no ICE path here: live part skipped)"); return; }
  must(/^Live/.test(await text(page, "chip")), "not live: " + (await text(page, "chip")));
  must(!(await disabled(page, "btnSwitch")), "Switch camera disabled in a live stream");
  must(/while streaming/.test(await text(page, "keepOpen")), "keep-open copy: " + (await text(page, "keepOpen")));
  await page.click("#btnStop");
  await home(page);
  must(!(await visible(page, "takeBusy")) && (await text(page, "takeMsg")) === "", "a live stream ended like a take");
}));

await browser.close();
srv.kill();
if (results.every(Boolean)) console.log(`record smoke OK${noIce ? " (no ICE path: takes skipped)" : ""}`); else { console.log("record smoke FAILED"); process.exit(1); }
