# Manual test steps

How to verify CamLooper's behaviour by hand, feature by feature. Every entry is
written to be followed by someone with a build in front of them and no knowledge
of how the change was implemented.

**Maintained by agents:** after completing a task that changes app behaviour, the
agent adds or updates the relevant section here. See `CLAUDE.md`.

For how to get a Windows build running in a VM in the first place, see
[`../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md`](../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md).

**Check what is already automated before writing steps here.** The harness at
[`../manual-testing/`](../manual-testing/README.md) covers install verification, bundled
resources, softcam registration (including that the Store build must *not* pre-register),
the camera actually streaming the right frames in the right order, looping, and uninstall
cleanup. Steps in this file should cover what the harness cannot: anything visual, the
SmartScreen and UAC flows, the installer language picker, and behaviour that needs human
judgement. Duplicating an automated check here just means it rots.

---

## How to write an entry

Each section is one feature or fix, newest at the top of its platform group:

```markdown
### <Feature or fix name>
_Added <YYYY-MM-DD> · <platform: Windows / Linux / macOS / All> · <commit or PR ref>_

**Setup:** what must exist before starting (a build, a test video, a clean VM…).

**Steps:**
1. Concrete, numbered actions. No "verify it works" — say what to click.
2. ...

**Expected:** exactly what a passing run looks like.

**Known gotchas:** anything that looks like a failure but isn't
(SmartScreen warnings, first-run UAC prompt, slow framerate in a VM…).
```

Rules of thumb:
- Steps must be runnable without reading the source.
- State expected values, not vibes — "loops without a visible seam at 00:10",
  not "looping works".
- If a check has a command-line equivalent (a registry query, a log line),
  include it — it beats squinting at a UI.
- When a change makes an existing section wrong, edit that section rather than
  appending a second, contradictory one.

---

## Core smoke test

_All platforms._ Run this after any change before moving on to the specific
sections below.

**Setup:** a fresh install of the current build; one short `.mp4` (10–30 s) with
recognisable motion.

**Steps:**
1. Launch CamLooper.
2. Drag the `.mp4` onto the upload area.
3. Confirm the video appears with a thumbnail and its duration.
4. Enable the virtual camera.
5. Open a camera consumer — Windows Camera app, or OBS → *Video Capture Device*.
6. Select the CamLooper virtual camera as the device.
7. Watch the feed for at least twice the clip's length.
8. Disable the camera in CamLooper.

**Expected:** the device is listed as "DirectShow Softcam" on Windows; the feed
shows the uploaded video, moving continuously with no frozen or black frame; the
device stops producing frames after step 8.

Natural motion is on by default, so the clip does **not** replay in order — it
drifts forwards and backwards, and there is no restart to watch for. That is
correct behaviour, not a stuck or stuttering feed. To smoke-test straight
looping instead, turn *Natural motion* off under **Advanced → Loop settings**
and restart the loop. See "Natural motion" below.

**Known gotchas:** framerate is low in a VM (no GPU acceleration) — judge frame
correctness, not smoothness. On the Microsoft Store build the first camera
enable raises a single UAC prompt to register the driver; that is by design.

---

## All platforms

<!-- Newest entries first. -->

### Natural motion (random-walk playback)
_Added 2026-09-05 · All · `src-tauri/src/frame_walk.rs`_

Playback no longer replays the clip in order. It walks the frame index back and
forth at random — `1 2 3 2 1 2 1 2 3 4 3 …` — so the loop has no seam and never
repeats. On by default.

Most of this is asserted by the VM harness (`frames-adjacent`,
`playback-continues`, `natural-motion-reverses` in
[`../manual-testing/guest/check-camera.ps1`](../manual-testing/guest/check-camera.ps1)).
What needs eyes is whether the result actually *looks* like a live person, which
no assertion covers.

**Setup:** a build of the current version; a clip of a person sitting still with
small movements (a nod, a glance) — the real use case. Also have a clip with
unmistakable one-way motion (a hand crossing the frame) to make direction changes
obvious, and one clip longer than 60 s.

**Steps:**
1. Load the one-way-motion clip and start the loop.
2. Watch the preview for 30 seconds. Note when the motion changes direction.
3. Open **Advanced → Loop settings**. Confirm *Natural motion* is on.
4. Load the person clip. Watch the preview for a full minute.
5. Turn *Natural motion* off, then stop and restart the loop.
6. Watch for another 30 seconds.
7. Leaving the switch **off**, close CamLooper and relaunch it. Reopen
   **Advanced → Loop settings**.
8. Turn *Natural motion* back on. Load the >60 s clip and start the loop.

**Expected:**
- Step 2: motion reverses direction every second or so. Reversals are smooth —
  the picture keeps moving through the turn. A single-frame twitch, a freeze, or a
  jump to an unrelated part of the clip is a failure.
- Step 4: no seam and no point where the clip visibly "starts over". The footage
  should read as someone sitting there, not as a video on repeat.
- Step 6: with natural motion off, the old behaviour returns — the clip plays in
  order with a visible cut back to the first frame at the end of each pass.
- Step 7: the switch is still **off** after the relaunch. This is the assertion
  that matters — natural motion defaults to on, so a setting that failed to
  persist would silently turn itself back on and look like nothing was wrong.
- Step 8: a "Preparing…" badge appears on the preview for a moment before the
  first frame, then a note under the switch reading "Clip is too long to buffer —
  natural motion uses its first 60s." Playback then behaves as in step 4.

**Known gotchas:**
- A change to the switch does not affect a loop that is already running — it
  applies the next time you start. The panel says so while playback is active.
- "Preparing…" appears only on the first start for a given clip. The buffer is
  kept, so pause and resume on the same clip is instant. Loading a different clip
  (or re-recording) buffers again.
- On a clip shorter than about a second the walk has very little room and will
  look like a fast shuffle. That is the clip's fault, not the feature's.
- The loop counter measures elapsed playback in clip-lengths, since there are no
  passes to count. "Loop 3/10" means three clip-durations of playback have gone by.

### Fixed (non-resizable) main window
_Added 2026-09-05 · All · `tauri.conf.json` window config_

**Setup:** a build of the current version, launched on a display at least
1280×800 so the 1200×760 window fits.

**Steps:**
1. Launch CamLooper.
2. Drag each edge and each corner of the window in turn.
3. Double-click the title bar.
4. Look at the window controls in the title bar.
5. On Windows, press `Win`+`↑`; on Linux (GNOME/KDE), press `Super`+`↑`.
6. Drag the window by its title bar to the top edge of the screen.
7. Move the window around the screen by its title bar.

**Expected:** the window stays 1200×760 throughout. No resize cursor appears on
any edge or corner (step 2); the double-click does nothing (step 3); the
maximize button is absent or greyed out (step 4); the maximize/snap shortcut and
the top-edge drag leave the size unchanged (steps 5–6); the window still moves
normally (step 7).

**Known gotchas:** on a display shorter than ~830 px of usable height the window
no longer fits and can no longer be shrunk to fit — that is the expected
consequence of this change, not a regression. Some tiling window managers on
Linux (i3, sway, Hyprland) ignore the non-resizable hint and will tile the window
anyway; test on a floating desktop (GNOME, KDE, XFCE).

---

## Windows

<!-- Newest entries first. -->

### Virtual camera frame rate, and the output resolution picker
_Added 2026-09-05 · Windows (resolution picker: All) · unreleased_

The virtual camera used to deliver about 10 fps on Windows. Every frame was decoded
and rescaled in hand-written Rust — a scalar bilinear upscale from 640×360 to
1920×1080, measured at ~99 ms per frame against a 33 ms budget — so most frame
deadlines were missed. That work now goes through the bundled ffmpeg, and the source
render and camera output are the same size, so nothing is upscaled at all.

**Setup:** a Windows build installed in the VM (see
[`../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md`](../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md)),
and both fixtures present in `vm-shared/fixtures/` (`./manual-testing/make-fixture.sh`
builds them).

**Steps:**
1. Launch CamLooper, load `Z:\fixtures\motion-10s.mp4`, and enable the virtual camera.
2. From the host, measure the real update rate:
   ```bash
   ./manual-testing/run-windows-test.sh --channel direct --camera manual --fps
   ```
   Or, inside the guest, directly:
   ```powershell
   powershell -File Z:\guest\check-camera.ps1 -Mode fps
   ```
3. Read `unique_fps` from `vm-shared/results/check-camera-fps.json`.
4. Open **Advanced → Virtual Camera**. Set **Resolution** to each of 480p, 720p and
   1080p in turn. The control is disabled while the camera is on, so switch the camera
   off before each change and back on after.
5. For each setting, confirm the **Resolution:** line in the same card reports the
   matching size, and re-run step 2.
6. Toggle the virtual camera off and on three times, then re-run step 2.
7. Close CamLooper, reopen it, and check the Resolution control still shows your last
   choice.

**Expected:**
- `unique_fps` is far above the pre-change baseline on the same VM. Compare the
  *ratio*, not the absolute number — roughly 3× is the shape of it.
- Step 5: the status line reads `854x480`, `1280x720`, `1920x1080` to match, and
  `unique_fps` holds up at every setting.
- Step 6: `unique_fps` after three toggles matches the first run. It used to fall on
  every toggle, because each one left the previous frame pump running.
- Step 7: the resolution persists across restarts; a fresh profile defaults to 720p.
- In the Windows **Camera** app the picture is upright and the colours are correct.

**Known gotchas:**
- Absolute framerate in the VM is low regardless — the guest has no GPU acceleration.
  Only the before/after ratio on the same machine means anything.
- `delivered_fps` in the same JSON stays near 30 even when the camera is badly
  starved. That is not the number you want: softcam hands consumers the last frame
  again when nothing new has arrived, so delivered frames count duplicates.
  `unique_fps` is the real signal.
- The fps check needs `motion-10s.mp4`, not the colour fixture. Every frame within a
  second of the colour clip is identical, so duplicate detection would collapse it and
  report about 1 fps on a perfectly healthy camera.
- SmartScreen warns on unsigned builds; the Store build prompts for UAC on first
  camera use. Both are expected.

---

## Linux

<!-- Newest entries first. -->

_No feature-specific entries yet._

---

## macOS

<!-- Newest entries first. -->

_No feature-specific entries yet._
