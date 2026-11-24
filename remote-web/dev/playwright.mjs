// Finds playwright-core for the smoke scripts. In order:
//   1. $PLAYWRIGHT_CORE — the path of a playwright-core package directory
//      (…/node_modules/playwright-core), for reusing one that is installed elsewhere;
//   2. a normal `import("playwright-core")`, i.e. whatever Node resolves from this directory
//      upwards (`npm i --no-save playwright-core` in the repo root puts it there).
// playwright-core ships no browser; the scripts drive the system Chrome ($CHROME).
import { createRequire } from "node:module";
import path from "node:path";

export async function loadChromium() {
  const override = process.env.PLAYWRIGHT_CORE;
  try {
    if (override) {
      const require = createRequire(import.meta.url);
      return require(path.resolve(override)).chromium;
    }
    const mod = await import("playwright-core");
    return mod.chromium ?? mod.default.chromium;
  } catch (err) {
    const where = override ? `PLAYWRIGHT_CORE=${override}` : "node_modules";
    console.error(`Cannot load playwright-core from ${where}: ${err.message}`);
    console.error("Install it with `npm i --no-save playwright-core` in the repo root,");
    console.error("or point PLAYWRIGHT_CORE at an existing …/node_modules/playwright-core directory.");
    process.exit(2);
  }
}
