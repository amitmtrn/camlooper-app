# remote-web — CamLooper Remote phone page

`index.html` is the page the phone opens to link with CamLooper, send meetings to the computer
and stream its camera to the desktop app (remote v2). It is a single file with vanilla JS and
inline CSS, no build step.

- **Production target:** `https://camlooper.com/remote`. **Not deployed yet.**
- **Development:** `dev/start-phone-tunnel.sh` serves the page with a same-origin `/v1` proxy to
  the live relay and gives the phone an https URL through a cloudflared quick tunnel. Details,
  self-tests and smoke scripts: [`dev/README.md`](dev/README.md).
- Protocol between this page and the app: [`../docs/remote-protocol.md`](../docs/remote-protocol.md).

This folder is **not** part of the desktop app: Vite only builds the root `index.html` into
`dist/`, and ESLint ignores `remote-web/`. Screenshots written by the smoke scripts
(`dev/*.png`) are git-ignored.
