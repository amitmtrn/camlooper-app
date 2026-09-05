# CamLooper — agent instructions

## Record manual test steps after every task

When you finish a task that changes how the app behaves, **update
[`MANUAL_TEST.md`](MANUAL_TEST.md)** in this directory before reporting the task
complete. This is part of the task, not an optional extra.

What to write:

- Add a section for the feature or fix, or **edit the existing section** if one
  already covers that behaviour — do not append a second entry that contradicts
  the first.
- Follow the format documented at the top of `MANUAL_TEST.md`: Setup, numbered
  Steps, Expected, Known gotchas. Put it under the right platform heading, newest
  first.
- Write for someone with a build in front of them who has not read your diff.
  Name the buttons they click and the values they should see. "Verify it works"
  is not a test step.
- Prefer a checkable signal over an impression: a log line, a registry query, a
  specific timestamp in the video. Include the command where one exists.
- Note anything that looks like a failure but isn't — SmartScreen warnings on
  unsigned builds, the first-run UAC prompt on the Store build, low framerate in
  a VM.

When to skip it: pure refactors, dependency bumps, docs, and CI changes that a
user cannot observe. If nothing a tester could see has changed, say so in your
summary instead of padding the file.

## Prefer an automated check over a written step

Before adding steps to `MANUAL_TEST.md`, consider whether the behaviour can be asserted
instead:

- **Logic** — add a `#[cfg(test)] mod tests` next to the code and run
  `cd src-tauri && cargo test --lib`. CI runs these on Linux *and* Windows, because much of
  the risky code (softcam registration, DirectShow paths) is `#[cfg(windows)]` and a
  Linux-only run never sees it.
- **Install, driver registration, camera output, uninstall** — extend the VM harness in
  [`../manual-testing/`](../manual-testing/README.md) rather than writing prose. It already
  asserts bundled resources, the softcam CLSID per channel, live frames captured through
  DirectShow, playback continuity (including natural motion's random walk), and uninstall
  cleanup.

Written steps are for what genuinely needs eyes: visual output, SmartScreen and UAC flows,
the installer language picker, layout. A written step that duplicates an automated check
rots, because nothing fails when it drifts.

Windows changes are tested in a local VirtualBox VM — the environment setup and
build/install cycle is documented in
[`../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md`](../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md).
Reference it rather than repeating VM setup steps inside `MANUAL_TEST.md`.

## Installer channels

Windows ships two NSIS builds and they behave differently at install time. If
your change touches installation, driver registration, or first-run behaviour,
your test steps must say which channel they apply to:

- **Direct download** (`tauri.conf.json`, `installMode: perMachine`) — elevated,
  registers `softcam.dll` via `regsvr32` in the installer hook.
- **Microsoft Store** (`tauri.store.conf.json`, `installMode: currentUser`) —
  silent and non-elevated to satisfy Store policies 10.2.9.2 / 10.3.4. It cannot
  register the driver at install time; the app does it on first camera use via
  `ensure_softcam_registered` (one UAC prompt).
