#!/usr/bin/env node
// "Watch the meeting" end to end, with the real desktop code and the real relay — but no
// portal, no capture and no GUI app: the phone page (headless Chrome) pairs with
// `phone_stream_cli --meeting-fixture --null`, which sends ffmpeg's test pattern and a tone
// through the app's own meeting session (ffmpeg encoders → str0m). Unlike the smoke scripts
// this one talks to the live relay (through serve-phone.mjs): it makes one pairing and the
// CLI revokes it on exit.
//
//   cd src-tauri && cargo build --example phone_stream_cli
//   node remote-web/dev/e2e-meeting.mjs
//
// It reports whether sound and picture arrived at the page, whether Mute silences the
// element (and nothing else), and what the CLI said. Exit status 0 only if all of it held.
// Nothing touches a camera: the CLI's output is --null and the page never calls getUserMedia.
// Env: PLAYWRIGHT_CORE, CHROME, CLI (path of the built example), UPSTREAM.
import { loadChromium } from "./playwright.mjs";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const CLI = process.env.CLI || path.join(HERE, "../../src-tauri/target/debug/examples/phone_stream_cli");
if (!fs.existsSync(CLI)) { console.error(`no CLI at ${CLI}: run \`cargo build --example phone_stream_cli\` in src-tauri/`); process.exit(2); }
const chromium = await loadChromium();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const must = (cond, msg) => { if (!cond) throw new Error(msg); };

const srv = spawn(process.execPath, [path.join(HERE, "serve-phone.mjs")], { env: { ...process.env, PORT: "0" }, stdio: ["ignore", "pipe", "inherit"] });
const port = await new Promise((resolve, reject) => {
  let out = ""; srv.stdout.on("data", (d) => { out += d; const m = /127\.0\.0\.1:(\d+)/.exec(out); if (m) resolve(Number(m[1])); });
  srv.on("exit", () => reject(new Error("server exited")));
});
const browser = await chromium.launch({
  executablePath: process.env.CHROME || "/usr/bin/google-chrome", headless: true,
  args: ["--disable-features=WebRtcHideLocalIpsWithMdns", "--autoplay-policy=no-user-gesture-required"],
});
let cli = null;
const events = [];
let failed = false;
try {
  // No camera or microphone permission is granted: a meeting must not need one.
  const ctx = await browser.newContext({ viewport: { width: 412, height: 915 }, isMobile: true, hasTouch: true });
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push("pageerror: " + e.message));
  page.on("console", (m) => { if (m.type() === "error" && !/WebSocket|Failed to load resource/.test(m.text())) errors.push("console: " + m.text()); });
  await page.addInitScript(() => {
    window.__capture = [];
    const md = navigator.mediaDevices;
    for (const fn of ["getUserMedia", "getDisplayMedia"]) {
      const real = md && md[fn] && md[fn].bind(md);
      if (real) md[fn] = (...a) => { window.__capture.push(fn); return real(...a); };
    }
  });
  await page.goto(`http://127.0.0.1:${port}/`);
  await page.fill("#deviceName", "E2E viewer");
  await page.click("#btnGetCode");
  await page.waitForFunction(() => /^\d{6}$/.test(document.getElementById("code").textContent.replace(/\s/g, "")), null, { timeout: 20000 });
  const code = (await page.textContent("#code")).replace(/\s/g, "");
  console.log("paired a code with the relay; starting the CLI");

  cli = spawn(CLI, ["--code", code, "--null", "--meeting-fixture", "--seconds", "90"], { stdio: ["ignore", "pipe", "pipe"] });
  let buf = "";
  cli.stdout.on("data", (d) => {
    buf += d;
    for (let i; (i = buf.indexOf("\n")) >= 0; buf = buf.slice(i + 1)) { try { events.push(JSON.parse(buf.slice(0, i))); } catch {} }
  });
  let cliErr = "";
  cli.stderr.on("data", (d) => { cliErr += d; if (process.env.VERBOSE) process.stderr.write(d); });
  const cliExit = new Promise((r) => cli.on("exit", (status, signal) => r({ status, signal })));

  await page.waitForSelector("#scrHome:not(.hidden)", { timeout: 30000 });
  await page.waitForFunction(() => !document.getElementById("btnMeeting").disabled, null, { timeout: 20000 });
  must(events.some((e) => e.event === "meeting_source" && e.source === "fixture"), "the CLI is not on the fixture source");
  console.log("the real desktop offers \"meeting\" in hello_ok; Watch the meeting is on");

  await page.click("#btnMeeting");
  await page.waitForSelector("#mtgConsent:not(.hidden)");
  await page.click("#btnMtgGo");
  await page.waitForFunction(() => /live|failed/.test(document.getElementById("chip").className), null, { timeout: 45000 });
  must(await page.evaluate(() => document.getElementById("chip").classList.contains("live")), "not live: " + (await page.textContent("#streamFail")));
  await page.waitForFunction(() => /see the shared window/.test(document.getElementById("streamMsg").textContent), null, { timeout: 20000 });
  await page.waitForFunction(() => { const v = document.getElementById("preview"); return v.videoWidth > 0 && v.currentTime > 1 && !v.paused; }, null, { timeout: 20000 });
  await sleep(3000);

  const read = () => page.evaluate(async () => {
    const v = document.getElementById("preview");
    const o = { muted: v.muted, paused: v.paused, time: v.currentTime, size: `${v.videoWidth}x${v.videoHeight}`, audioEnabled: v.srcObject.getAudioTracks().map((t) => t.enabled), audio: 0, video: 0, frames: 0, energy: 0, codecs: [], path: "" };
    const stats = await session.pc.getStats();
    stats.forEach((x) => {
      if (x.type === "inbound-rtp" && x.kind === "audio") { o.audio = x.packetsReceived; o.energy = x.totalAudioEnergy || 0; }
      if (x.type === "inbound-rtp" && x.kind === "video") { o.video = x.packetsReceived; o.frames = x.framesDecoded || 0; o.fps = x.framesPerSecond || 0; }
      if (x.type === "codec") o.codecs.push(x.mimeType);
      if (x.type === "candidate-pair" && x.nominated && x.state === "succeeded") { const l = stats.get(x.localCandidateId), r = stats.get(x.remoteCandidateId); o.path = `${l && l.candidateType}/${r && r.candidateType}`; }
    });
    return o;
  });
  const a = await read();
  console.log(`arrived at the page: audio ${a.audio} packets (energy ${a.energy.toFixed(3)}), video ${a.video} packets / ${a.frames} frames decoded at ${a.size}, ${a.fps} fps; codecs ${[...new Set(a.codecs)].join(", ")}; path ${a.path}`);
  must(a.audio > 100 && a.energy > 0, "sound did not arrive");
  must(a.video > 100 && a.frames > 60 && a.size === "1280x720", "the picture did not arrive as 1280x720");
  must(!a.muted && !a.paused && a.audioEnabled.join() === "true", "the element is not playing with sound: " + JSON.stringify(a));

  await page.click("#btnMtgMute");
  await sleep(1500);
  const m = await read();
  console.log(`Mute: element.muted=${m.muted}, audio track enabled=${m.audioEnabled.join()}, still arriving (audio ${a.audio}→${m.audio}, frames ${a.frames}→${m.frames}), playing=${!m.paused}`);
  must(m.muted && m.audioEnabled.join() === "false", "Mute did not silence the element");
  must(m.audio > a.audio && m.frames > a.frames && !m.paused, "the meeting stopped arriving while muted");
  await page.click("#btnMtgMute");
  const u = await read();
  console.log(`Unmute: element.muted=${u.muted}, audio track enabled=${u.audioEnabled.join()}`);
  must(!u.muted && u.audioEnabled.join() === "true", "Unmute did not bring the sound back");
  must((await page.textContent("#captionsText")) === "Captions come in a later update.", "captions panel");
  const reached = await page.evaluate(() => window.__capture);
  console.log(`camera / microphone calls on the phone: ${reached.length}`);
  must(reached.length === 0, "the phone reached for a camera or microphone: " + reached.join());

  const live = events.filter((e) => e.event === "status" && e.mode === "meeting").pop();
  console.log("the CLI's last status:", JSON.stringify(live));
  must(live && live.state === "live" && live.size === "1280x720" && live.frames > 300, "the CLI did not report a live meeting with a picture");

  await page.click("#btnStop");
  await page.waitForSelector("#scrHome:not(.hidden)");
  await sleep(2500);
  must(events.some((e) => e.event === "state" && e.state === "idle"), "the CLI did not go back to idle after the phone's Stop");
  cli.kill("SIGTERM");
  const exit = await Promise.race([cliExit, sleep(15000).then(() => null)]);
  console.log("the CLI exited:", JSON.stringify(exit), "| unpaired:", JSON.stringify(events.find((e) => e.event === "unpaired")));
  must(exit && exit.status === 0, "the CLI did not exit cleanly");
  must(/meeting share: ended \((user|peer_stopped)\)/.test(cliErr), "the meeting session did not end as a stop: " + cliErr.split("\n").filter((l) => /meeting share/.test(l)).join(" | "));
  if (errors.length) throw new Error("page errors: " + errors.join(" | "));
  console.log("meeting e2e OK");
} catch (e) {
  failed = true;
  console.log("meeting e2e FAILED:", e.message);
  console.log("CLI events:", events.map((e) => e.event + (e.state ? `:${e.state}` : "")).join(", "));
} finally {
  if (cli && cli.exitCode === null) { cli.kill("SIGTERM"); await sleep(3000); if (cli.exitCode === null) cli.kill("SIGKILL"); }
  await browser.close();
  srv.kill();
}
process.exit(failed ? 1 : 0);
