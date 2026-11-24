#!/usr/bin/env node
// Recomputes the script-src hash in ../index.html's CSP <meta> from its inline <script>.
// Run after every edit to the page script: node dev/update-csp.mjs  (--check: exit 1 if stale)
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const PAGE = process.env.PAGE ? path.resolve(process.env.PAGE) : path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "index.html");

export function scriptHash(html) {
  const m = /<script>([\s\S]*?)<\/script>/.exec(html);
  if (!m) throw new Error("no inline <script> found");
  return "sha256-" + crypto.createHash("sha256").update(m[1], "utf8").digest("base64");
}
export function cspHash(html) {
  const m = /script-src '(sha256-[^']+)'/.exec(html);
  return m ? m[1] : null;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const html = fs.readFileSync(PAGE, "utf8");
  const want = scriptHash(html), have = cspHash(html);
  if (!have) { console.error("CSP meta with script-src 'sha256-…' not found"); process.exit(1); }
  if (want === have) { console.log("CSP hash up to date"); process.exit(0); }
  if (process.argv.includes("--check")) { console.error(`CSP hash stale: have ${have}, want ${want}`); process.exit(1); }
  fs.writeFileSync(PAGE, html.replace(`'${have}'`, `'${want}'`));
  console.log(`CSP hash updated -> ${want}`);
}
