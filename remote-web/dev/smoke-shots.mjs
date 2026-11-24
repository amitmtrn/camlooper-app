#!/usr/bin/env node
// Smoke render of index.html in headless Chrome at 412x915 (Pixel-ish), saving
// dev/shot-pair.png, dev/shot-code.png and dev/shot-home.png. All /v1 traffic is
// faked with Playwright routes (never reaches api.camlooper.com).
// Env: PLAYWRIGHT_CORE (path to playwright-core), CHROME (default /usr/bin/google-chrome).
import { loadChromium } from "./playwright.mjs";
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const chromium = await loadChromium();

// Local page server; upstream is an unroutable local port (all /v1 calls are intercepted anyway).
const srv = spawn(process.execPath, [path.join(HERE, "serve-phone.mjs")], { env: { ...process.env, PORT: "0", UPSTREAM: "http://127.0.0.1:9" }, stdio: ["ignore", "pipe", "inherit"] });
const port = await new Promise((resolve, reject) => {
  let out = ""; srv.stdout.on("data", (d) => { out += d; const m = /127\.0\.0\.1:(\d+)/.exec(out); if (m) resolve(Number(m[1])); });
  srv.on("exit", () => reject(new Error("server exited")));
});
const BASE = `http://127.0.0.1:${port}`;

const browser = await chromium.launch({ executablePath: process.env.CHROME || "/usr/bin/google-chrome", headless: true });
const errors = [];
async function newPage() {
  const ctx = await browser.newContext({ viewport: { width: 412, height: 915 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true,
    userAgent: "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36" });
  const page = await ctx.newPage();
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
  const json = (body, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  await page.route("**/v1/**", (route) => {
    const u = new URL(route.request().url());
    const exp = new Date(Date.now() + 272000).toISOString();
    switch (u.pathname) {
      case "/v1/mobile/pair/start": return route.fulfill(json({ pairId: "p_fake", mobileToken: "t_fake", code: "482913", expiresAt: exp }, 201));
      case "/v1/mobile/status": return route.fulfill(json({ linked: false, desktopOnline: false, desktopName: null, pending: null, lastAck: null }));
      case "/v1/mobile/ws-ticket": return route.fulfill(json({ ticket: "k_fake", expiresAt: exp }));
      default: return route.fulfill(json({ error: "not_found" }, 404));
    }
  });
  await page.routeWebSocket(/\/v1\/mobile\/ws/, (ws) => {
    ws.send(JSON.stringify(page.__linked
      ? { type: "hello", protocol: 2, linked: true, desktopOnline: true, desktopName: "Studio PC <b>&</b>", desktopProtocol: 2 }
      : { type: "hello", protocol: 2, linked: false, desktopOnline: false }));
    ws.onMessage(() => {});
  });
  return page;
}

try {
  // Pair screen
  let page = await newPage();
  await page.goto(BASE + "/");
  await page.waitForSelector("#scrPair:not(.hidden)");
  await page.screenshot({ path: path.join(HERE, "shot-pair.png") });
  // Code screen
  await page.click("#btnGetCode");
  await page.waitForSelector("#pairCode:not(.hidden)");
  await page.screenshot({ path: path.join(HERE, "shot-code.png") });
  const code = await page.textContent("#code");
  if (code !== "482913") throw new Error("code not shown");
  await page.context().close();

  // Home screen (fake linked state in localStorage)
  page = await newPage();
  page.__linked = true;
  await page.addInitScript(() => localStorage.setItem("camlooper-remote-phone-v1", JSON.stringify({ pairId: "p_fake", mobileToken: "t_fake", deviceName: "Pixel 8", linked: true, desktopName: "Studio PC <b>&</b>" })));
  await page.goto(BASE + "/");
  await page.waitForSelector("#scrHome:not(.hidden)");
  await page.waitForFunction(() => document.getElementById("deskState").textContent === "Online");
  const name = await page.textContent("#deskName");
  const hasB = await page.evaluate(() => !!document.querySelector("#deskName b"));
  if (name !== "Studio PC <b>&</b>" || hasB) throw new Error("desktopName not rendered as text");
  await page.screenshot({ path: path.join(HERE, "shot-home.png"), fullPage: true });
  await page.context().close();
  if (errors.length) throw new Error("page errors: " + errors.join(" | "));
  console.log("smoke OK: shot-pair.png, shot-code.png, shot-home.png");
} finally {
  await browser.close();
  srv.kill();
}
