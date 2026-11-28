#!/usr/bin/env node
// "Watch the meeting" smoke test, fully local: the phone page receives sound and a picture
// from a fake "desktop" RTCPeerConnection in a second page (which sends Chrome's fake
// camera and microphone); the relay WebSocket and /v1 calls are faked with Playwright routes
// (same setup as smoke-record.mjs).
//  - Watch the meeting is off until hello_ok lists "meeting"; a computer without it (an
//    older one, or Windows / macOS for now) gets "Update CamLooper on your computer to watch
//    a meeting here…" and no request is ever sent; Start streaming and Record are unaffected
//  - the consent reminder comes before anything is sent ("…will hear what this computer
//    plays, and will see the window you pick. CamLooper does not tell the others…"); Cancel
//    sends nothing, Continue sends stream_request{mode:"meeting"}
//  - the phone never calls getUserMedia for a meeting: no camera, no microphone
//  - the offer is recvonly audio + video (H.264, no VP8) plus the "camlooper-transcript"
//    data channel; nothing in it sends
//  - live: sound and picture really arrive (inbound RTP, a playing <video> that is not
//    muted), "sound only" vs "the shared window" follows the computer's status, Switch
//    camera and the microphone button are hidden, the captions panel says "later"
//  - Mute the meeting silences the element and the received track at once and back, at any
//    time (also before the connection is up), and sends nothing to the computer
//  - Stop → stream_stop{user}; the computer's stop, a decline and "unsupported" have their
//    own words; Try again asks for a meeting again
//  - no CSP violation and no page error in any scenario
// Saves dev/shot-meeting.png. Env: PLAYWRIGHT_CORE, CHROME.
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
  args: ["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream", "--disable-features=WebRtcHideLocalIpsWithMdns", "--allow-loopback-in-peer-connection", "--autoplay-policy=no-user-gesture-required"],
});
const json = (body, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
const CONSENT = "Your phone will hear what this computer plays, and will see the window you pick. CamLooper does not tell the others in the meeting.";
const UPDATE = "Update CamLooper on your computer to watch a meeting here. (Linux computers only, for now.)";

// The fake desktop. `plan` decides how it answers hello and a request, and what it sends.
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
    const R = window.R = { plan, requests: [], offer: "", stops: [], channels: [] };
    window.sendStatus = (video) => window.toPhone({ kind: "status", sessionId: R.sid, state: "live", mode: "meeting", ...(video ? { width: 1280, height: 720, fps: 30, codec: "H264" } : {}) });
    window.stop = (reason) => window.rawToPhone({ type: "stream_stop", data: { sessionId: R.sid, reason } });
    window.onStop = (data) => { R.stops.push(data.reason); };
    window.onSig = async (d) => {
      if (d.kind === "stream_request") {
        R.requests.push(d);
        R.sid = d.sessionId;
        if (R.plan.reject) { window.toPhone({ kind: "stream_reject", sessionId: d.sessionId, reason: R.plan.reject }); return; }
        if (R.plan.acceptDelay) await new Promise((r) => setTimeout(r, R.plan.acceptDelay));
        window.toPhone({ kind: "stream_accept", sessionId: d.sessionId });
        window.toPhone({ kind: "status", sessionId: d.sessionId, state: "connecting", mode: "meeting" });
        return;
      }
      if (d.kind === "offer") {
        R.offer = d.sdp;
        const pc = window.pc = new RTCPeerConnection({ iceServers: [] });
        pc.ondatachannel = (ev) => {
          R.channels.push(ev.channel.label);
          ev.channel.onopen = () => ev.channel.send(JSON.stringify({ type: "captions", state: "unavailable" }));
        };
        pc.onconnectionstatechange = () => { if (pc.connectionState === "connected") window.sendStatus(!!R.plan.video); };
        await pc.setRemoteDescription({ type: "offer", sdp: d.sdp });
        // What the computer plays and the picked window, stood in for by Chrome's fake devices.
        const media = await navigator.mediaDevices.getUserMedia({ audio: true, video: !!R.plan.video });
        for (const tr of pc.getTransceivers()) {
          const track = media.getTracks().find((t) => t.kind === tr.receiver.track.kind);
          if (track) { tr.direction = "sendonly"; await tr.sender.replaceTrack(track); }
        }
        await pc.setLocalDescription(await pc.createAnswer());
        await new Promise((r) => { if (pc.iceGatheringState === "complete") r(); else pc.onicegatheringstatechange = () => pc.iceGatheringState === "complete" && r(); });
        R.answer = pc.localDescription.sdp;
        window.toPhone({ kind: "answer", sessionId: R.sid, sdp: R.answer });
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
      frames.push(m.type === "signal" ? (m.data.kind === "stream_request" ? `stream_request:${m.data.mode || "live"}` : m.data.kind) : `${m.type}:${m.data && m.data.reason}`);
      if (m.type === "stream_stop") { await desk.evaluate((d) => window.onStop(d), m.data).catch(() => {}); return; }
      if (m.type !== "signal") return;
      if (m.data.kind === "hello") {
        ctl.hellos.push(m.data);
        const h = plan.hello === undefined ? { appProtocol: 3, features: ["live", "record", "file", "meeting"] } : plan.hello;
        if (!h) return; // a computer that does not know hello
        const answer = () => ctl.toPhone({ kind: "hello_ok", sessionId: m.data.sessionId, appProtocol: h.appProtocol, features: h.features });
        if (plan.helloDelay) setTimeout(answer, plan.helloDelay); else answer();
        return;
      }
      if (m.data.kind === "loop_control") { ctl.toPhone({ kind: "loop_status", sessionId: m.data.sessionId, playing: false, fromPhone: false }); return; }
      await desk.evaluate((d) => window.onSig(d), m.data).catch(() => {}); // the scenario may be over
    });
  });
  // Every way the page could reach for the camera or the microphone is counted.
  await page.addInitScript(() => {
    localStorage.setItem("camlooper-remote-phone-v1", JSON.stringify({ pairId: "p_fake", mobileToken: "t_fake", deviceName: "Pixel 8", linked: true, desktopName: "Fake desk" }));
    window.__capture = [];
    const md = navigator.mediaDevices;
    for (const fn of ["getUserMedia", "getDisplayMedia"]) {
      const real = md && md[fn] && md[fn].bind(md);
      if (real) md[fn] = (...a) => { window.__capture.push(fn); return real(...a); };
    }
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
    console.log("     streamMsg:", await page.textContent("#streamMsg").catch(() => "?"), "| streamFail:", await page.textContent("#streamFail").catch(() => "?"), "| mtgMsg:", await page.textContent("#mtgMsg").catch(() => "?"), "| chip:", await page.textContent("#chip").catch(() => "?"));
    return false;
  } finally { await ctx.close(); }
}

const text = (page, id) => page.textContent("#" + id);
const visible = (page, id) => page.evaluate((id) => { const el = document.getElementById(id); return !!el && el.getClientRects().length > 0; }, id);
const disabled = (page, id) => page.evaluate((id) => document.getElementById(id).disabled, id);
const must = (cond, msg) => { if (!cond) throw new Error(msg); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const home = (page) => page.waitForSelector("#scrHome:not(.hidden)");
const meetingReady = (page) => page.waitForFunction(() => !document.getElementById("btnMeeting").disabled, null, { timeout: 10000 });
const captured = (page) => page.evaluate(() => window.__capture.slice());
// What the phone is playing right now.
const playback = (page) => page.evaluate(async () => {
  const v = document.getElementById("preview"), st = v.srcObject;
  const stats = session && session.pc ? await session.pc.getStats() : new Map();
  const inbound = { audio: 0, video: 0, energy: 0, frames: 0 };
  stats.forEach((x) => {
    if (x.type !== "inbound-rtp") return;
    inbound[x.kind] = x.packetsReceived || 0;
    if (x.kind === "audio") inbound.energy = x.totalAudioEnergy || 0;
    if (x.kind === "video") inbound.frames = x.framesDecoded || 0;
  });
  return {
    muted: v.muted, paused: v.paused, time: v.currentTime, width: v.videoWidth,
    audioTracks: st ? st.getAudioTracks().map((t) => ({ enabled: t.enabled, state: t.readyState })) : [],
    videoTracks: st ? st.getVideoTracks().length : 0,
    senders: session && session.pc ? session.pc.getSenders().filter((x) => x.track).length : -1,
    inbound,
  };
});
let noIce = false;
// Watch the meeting → Continue → live. false: no ICE path in this environment.
async function watch(page) {
  await meetingReady(page);
  await page.click("#btnMeeting");
  await page.waitForSelector("#mtgConsent:not(.hidden)");
  await page.click("#btnMtgGo");
  await page.waitForFunction(() => /live|failed/.test(document.getElementById("chip").className), null, { timeout: 40000 });
  if (await page.evaluate(() => document.getElementById("chip").classList.contains("live"))) return true;
  noIce = true; console.log("     (no ICE path here: the live part is skipped)");
  return false;
}

const results = [];

results.push(await scenario("off until hello_ok lists meeting; consent first; recvonly offer; sound and picture arrive; mute; captions panel; Stop", { debug: true, helloDelay: 700, video: true }, async (page, ctl) => {
  must(await visible(page, "btnMeeting") && (await text(page, "btnMeeting")) === "Watch the meeting", "Watch the meeting missing");
  must(await disabled(page, "btnMeeting"), "Watch the meeting enabled before hello_ok");
  await meetingReady(page);
  must((await text(page, "mtgNote")) === "", "update note shown for a computer that shares meetings");
  must(!(await visible(page, "mtgConsent")), "consent shown before the button was tapped");

  // The reminder comes first, and Cancel sends nothing.
  await page.click("#btnMeeting");
  await page.waitForSelector("#mtgConsent:not(.hidden)");
  must((await text(page, "mtgConsentText")) === CONSENT, "consent copy: " + (await text(page, "mtgConsentText")));
  await page.click("#btnMtgCancel");
  await sleep(300);
  must(!(await visible(page, "mtgConsent")), "consent still shown after Cancel");
  must(!ctl.frames.some((f) => f.startsWith("stream_request")), "a request was sent without consent: " + ctl.frames.join());
  must(await visible(page, "scrHome"), "left the home screen without consent");

  if (!(await watch(page))) return;
  const R = await ctl.desk.evaluate(() => window.R);
  must(R.requests.length === 1 && R.requests[0].mode === "meeting" && !("file" in R.requests[0]) && R.requests[0].deviceLabel === "Pixel 8", "bad stream_request: " + JSON.stringify(R.requests));
  must((await captured(page)).length === 0, "the phone reached for a camera or microphone: " + (await captured(page)).join());

  // The offer: audio and video, both recvonly, the transcript channel; nothing that sends.
  const sections = R.offer.split(/^m=/m).slice(1);
  const kinds = sections.map((s) => s.split(" ")[0]);
  must(kinds.join() === "audio,video,application", "offer sections: " + kinds.join());
  for (const s of sections.slice(0, 2)) must(/^a=recvonly\r?$/m.test(s) && !/^a=(sendrecv|sendonly)\r?$/m.test(s), "a media section is not recvonly: " + s.split("\n")[0]);
  must(!/a=ssrc:/.test(R.offer) && !/a=msid:[^-\s]/.test(R.offer.replace(/a=msid:- /g, "")), "the offer announces a sending stream");
  const video = sections[1];
  must(/a=rtpmap:\d+ H264\/90000/.test(video) && /profile-level-id=42e01f/.test(video), "H.264 constrained baseline not offered");
  must(!/a=rtpmap:\d+ (VP8|VP9|AV1)\//.test(video), "video codecs the computer never sends are offered");
  must(/a=rtpmap:\d+ opus\/48000/.test(sections[0]), "Opus not offered");
  must(R.channels.join() === "camlooper-transcript", "data channels: " + R.channels.join());
  must(R.answer && (R.answer.match(/^a=sendonly\r?$/gm) || []).length === 2, "the fake computer does not send both tracks");

  // Live, as a viewer.
  must(/^Live/.test(await text(page, "chip")), "not live: " + (await text(page, "chip")));
  for (const id of ["btnSwitch", "btnMic", "stats", "hiddenNote"]) must(!(await visible(page, id)), `#${id} is visible while watching a meeting`);
  must(await visible(page, "btnMtgMute") && (await text(page, "btnMtgMute")) === "Mute the meeting", "mute button: " + (await text(page, "btnMtgMute")));
  must(await visible(page, "btnStop"), "Stop missing");
  must(/screen on/.test(await text(page, "keepOpen")) && /not your own microphone/.test(await text(page, "keepOpen")), "keep-open copy: " + (await text(page, "keepOpen")));
  must(await page.evaluate(() => document.getElementById("videoWrap").classList.contains("meeting") && !document.getElementById("preview").classList.contains("mirror")), "the viewer is laid out like the camera preview");
  await page.waitForFunction(() => /see the shared window/.test(document.getElementById("streamMsg").textContent), null, { timeout: 10000 });
  must(!(await visible(page, "soundOnly")), "\"Sound only\" shown while a window is shared");
  await page.waitForFunction(() => document.getElementById("captionsText").textContent === "Captions come in a later update.");
  must(await visible(page, "captions") && (await text(page, "captions")).includes("Live captions"), "captions panel missing");

  // Sound and picture really arrive and play, with sound.
  await page.waitForFunction(() => { const v = document.getElementById("preview"); return v.videoWidth > 0 && v.currentTime > 0.5 && !v.paused; }, null, { timeout: 15000 });
  await sleep(1500);
  const a = await playback(page);
  console.log("     arrived:", JSON.stringify(a.inbound), `| element: ${a.width}px wide, t=${a.time.toFixed(1)}s, muted=${a.muted}`);
  must(a.inbound.audio > 20 && a.inbound.video > 20 && a.inbound.frames > 5, "media did not arrive: " + JSON.stringify(a.inbound));
  must(a.inbound.energy > 0, "the received sound is silence: " + JSON.stringify(a.inbound));
  must(!a.muted && !a.paused && a.audioTracks.length === 1 && a.audioTracks[0].enabled && a.videoTracks === 1, "not playing with sound: " + JSON.stringify(a));
  must(a.senders === 0, "the phone sends a track: " + a.senders);
  await page.screenshot({ path: path.join(HERE, "shot-meeting.png") });

  // Mute: at once, local, and back. Nothing is signalled.
  const before = ctl.frames.length;
  await page.click("#btnMtgMute");
  let m = await playback(page);
  must(m.muted && m.audioTracks[0].enabled === false, "Mute did not silence the element and the track: " + JSON.stringify(m));
  must((await text(page, "btnMtgMute")) === "Unmute the meeting" && (await page.getAttribute("#btnMtgMute", "aria-pressed")) === "true", "mute button state");
  must(/Muted on this phone/.test(await text(page, "streamMsg")), "muted copy: " + (await text(page, "streamMsg")));
  await sleep(700);
  const during = await playback(page);
  must(!during.paused && during.time > a.time && during.inbound.audio > m.inbound.audio, "the meeting stopped arriving while muted: " + JSON.stringify(during));
  await page.click("#btnMtgMute");
  m = await playback(page);
  must(!m.muted && m.audioTracks[0].enabled === true && (await text(page, "btnMtgMute")) === "Mute the meeting", "Unmute did not bring the sound back: " + JSON.stringify(m));
  must(/see the shared window/.test(await text(page, "streamMsg")), "copy after unmute: " + (await text(page, "streamMsg")));
  await sleep(300);
  must(ctl.frames.length === before, "muting sent something to the computer: " + ctl.frames.slice(before).join());
  must(await ctl.desk.evaluate(() => window.pc.signalingState === "stable" && window.pc.connectionState === "connected"), "muting renegotiated");

  // The window is closed on the computer: sound only; and shared again.
  await ctl.desk.evaluate(() => window.sendStatus(false));
  await page.waitForFunction(() => /sound only/.test(document.getElementById("streamMsg").textContent));
  must(await visible(page, "soundOnly"), "\"Sound only\" not shown");
  await ctl.desk.evaluate(() => window.sendStatus(true));
  await page.waitForFunction(() => /see the shared window/.test(document.getElementById("streamMsg").textContent));

  await page.click("#btnStop");
  await home(page);
  await sleep(300);
  must(ctl.frames.includes("stream_stop:user"), "Stop did not tell the computer: " + ctl.frames.join());
  must(await page.evaluate(() => { const v = document.getElementById("preview"); return v.srcObject === null && v.muted === true && !document.getElementById("videoWrap").classList.contains("meeting"); }), "the viewer was not put away");
  must((await captured(page)).length === 0, "the phone reached for a camera or microphone: " + (await captured(page)).join());
  must(!(await disabled(page, "btnMeeting")) && !(await disabled(page, "btnStart")), "the buttons did not come back");
}));

results.push(await scenario("no window picked: sound only; mute works before the connection is up; the computer stops sharing", { video: false, acceptDelay: 900 }, async (page, ctl) => {
  await meetingReady(page);
  await page.click("#btnMeeting");
  await page.click("#btnMtgGo");
  await page.waitForSelector("#scrStream:not(.hidden)");
  must(/Accept on your computer/.test(await text(page, "streamMsg")), "waiting copy: " + (await text(page, "streamMsg")));
  // Muted while the computer has not even accepted: it stays muted when the sound arrives.
  await page.click("#btnMtgMute");
  must((await text(page, "btnMtgMute")) === "Unmute the meeting", "mute before the connection");
  await page.waitForFunction(() => /live|failed/.test(document.getElementById("chip").className), null, { timeout: 40000 });
  if (!(await page.evaluate(() => document.getElementById("chip").classList.contains("live")))) { noIce = true; console.log("     (no ICE path here: the live part is skipped)"); return; }
  await page.waitForFunction(() => session && session.remote.getAudioTracks().length === 1, null, { timeout: 10000 });
  await sleep(1200);
  let p = await playback(page);
  must(p.muted && p.audioTracks[0].enabled === false, "the sound arrived unmuted after an early Mute: " + JSON.stringify(p));
  must(p.inbound.audio > 10 && p.videoTracks <= 1 && p.inbound.video === 0, "sound-only session: " + JSON.stringify(p.inbound));
  must(/Muted on this phone/.test(await text(page, "streamMsg")), "muted copy: " + (await text(page, "streamMsg")));
  await page.click("#btnMtgMute");
  p = await playback(page);
  must(!p.muted && p.audioTracks[0].enabled === true, "Unmute: " + JSON.stringify(p));
  must(/sound only/.test(await text(page, "streamMsg")) && await visible(page, "soundOnly"), "sound-only copy: " + (await text(page, "streamMsg")));
  must((await captured(page)).length === 0, "the phone reached for a camera or microphone");

  await ctl.desk.evaluate(() => window.stop("user"));
  await home(page);
  must(/stopped on the computer/.test(await text(page, "homeMsg")), "stop copy: " + (await text(page, "homeMsg")));
  must(!ctl.frames.some((f) => f.startsWith("stream_stop")), "the phone answered the computer's stop with one of its own");
}));

for (const [hello, what] of [
  [{ appProtocol: 3, features: ["live", "record", "file"] }, "a computer without the meeting feature (Windows, macOS, or not updated)"],
  [null, "a computer that does not answer hello"],
]) {
  results.push(await scenario(`${what}: Watch the meeting stays off, says update, sends nothing`, { hello }, async (page, ctl) => {
    await page.waitForFunction((t) => document.getElementById("mtgNote").textContent === t, UPDATE, { timeout: 10000 });
    must(await disabled(page, "btnMeeting"), "Watch the meeting is enabled");
    await page.evaluate(() => document.getElementById("btnMeeting").click());
    await sleep(300);
    must(!(await visible(page, "mtgConsent")), "consent shown for a computer that cannot share");
    must(!ctl.frames.some((f) => f.startsWith("stream_request")), "a request was sent: " + ctl.frames.join());
    // Record a loop is its own feature: on with "record", whatever "meeting" says.
    must((await disabled(page, "btnRecord")) === !hello, "Record a loop follows the wrong feature");
  }));
}

for (const [reject, re] of [["declined", /declined to share the meeting/], ["unsupported", /can't share a meeting/], ["busy", /busy with another phone session/]]) {
  results.push(await scenario(`the computer answers ${reject}; Try again asks for a meeting again`, { reject }, async (page, ctl) => {
    await meetingReady(page);
    await page.click("#btnMeeting");
    await page.click("#btnMtgGo");
    await page.waitForSelector("#streamFail:not(.hidden)");
    must(re.test(await text(page, "streamFail")), "reject copy: " + (await text(page, "streamFail")));
    must(!(await visible(page, "btnMtgMute")) && !(await visible(page, "captions")), "viewer controls left on the failure screen");
    await page.click("#btnRetry");
    await sleep(800);
    const modes = await ctl.desk.evaluate(() => window.R.requests.map((r) => r.mode));
    must(modes.length === 2 && modes.every((m) => m === "meeting"), "Try again did not ask for a meeting: " + JSON.stringify(modes));
    must(!ctl.frames.some((f) => f === "offer"), "an offer was sent after a rejection");
    must((await captured(page)).length === 0, "the phone reached for a camera or microphone");
  }));
}

results.push(await scenario("after watching a meeting, Start streaming is the camera again (preview muted, mirrored, Switch camera back)", { video: true }, async (page, ctl) => {
  if (!(await watch(page))) return;
  await page.click("#btnStop");
  await home(page);
  await page.click("#btnStart");
  await page.waitForFunction(() => /live|failed|connecting|requesting/i.test(document.getElementById("chip").className + document.getElementById("chip").textContent), null, { timeout: 20000 });
  await page.waitForFunction(() => !!document.getElementById("preview").srcObject, null, { timeout: 10000 });
  must(ctl.frames.includes("stream_request:live"), "no live request: " + ctl.frames.join());
  must((await captured(page)).includes("getUserMedia"), "the camera was not opened for a live stream");
  must(await page.evaluate(() => { const v = document.getElementById("preview"); return v.muted && v.classList.contains("mirror") && !document.getElementById("videoWrap").classList.contains("meeting"); }), "the camera preview kept the viewer's settings");
  must(await visible(page, "btnSwitch") && !(await visible(page, "btnMtgMute")) && !(await visible(page, "captions")), "the camera screen kept the viewer's controls");
}));

await browser.close();
srv.kill();
if (results.every(Boolean)) console.log(`meeting smoke OK${noIce ? " (no ICE path: live parts skipped)" : ""}`); else { console.log("meeting smoke FAILED"); process.exit(1); }
