# Security Policy

## Reporting a vulnerability

**Do not open a public issue for security problems.**

Report them privately through
[GitHub's private vulnerability reporting](https://github.com/amitmtrn/camlooper-app/security/advisories/new),
or by email to amit7000@gmail.com. Expect an acknowledgement within 7 days. This is a
small project maintained by one person, so please allow reasonable time for a fix before
disclosing publicly.

Useful details: affected version and OS, reproduction steps, and what an attacker gains.

## Supported versions

Only the latest release gets security fixes. Older versions are not patched — upgrade first
if you are reporting against an old build.

## Where the risk is

CamLooper processes untrusted media and installs system-level camera components, so the
areas most worth scrutiny are:

- **Video decoding** — files are handed to FFmpeg; malformed input is the classic attack path
- **The virtual camera drivers** — the DirectShow filter on Windows (`softcam.dll`,
  registered with elevation) and the `v4l2loopback` kernel module on Linux
- **Installer behaviour** — the Windows NSIS installer runs elevated and registers the driver
- **The embedded ad banner** — an `iframe` to `camlooper.com/ads/banner`, which is the only
  network request the app makes by default (see `src/lib/ads.ts`)

## What is out of scope

- Vulnerabilities in FFmpeg, Tauri or other upstream dependencies — report those upstream,
  though telling us which version we ship is helpful
- Unsigned installers triggering SmartScreen or Gatekeeper warnings: a known, documented
  state, not a vulnerability
- Anything requiring an attacker to already have administrator access to the machine
