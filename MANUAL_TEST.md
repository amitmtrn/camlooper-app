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

### Phone remote: linking with a code, and opening a meeting sent from the phone
_Added 2026-11-08 · All · `src-tauri/src/remote/`, `src/components/RemoteCard.tsx`_

The phone app ("CamLooper Remote") shows a 6-digit code; typing it into the desktop app
links the two. Until that moment the desktop app makes **no network connection at all**
(the ad frames are the separate, pre-existing exception). After linking, a meeting link
sent from the phone brings the window to the front, starts the virtual camera and opens
the link. Unlinking returns the app to silence.

**Setup:** a build with network access; the CamLooper Remote app on a phone (or, for a
local relay, `npx wrangler@4 dev --port 8787` in `../server` and the app started with
`CAMLOOPER_API_URL=http://127.0.0.1:8787`); one short clip ready to load.

**Steps:**
1. Launch CamLooper. Open **Advanced → Debug logs** and read the first `remote:` line.
   On Linux also run `ss -tnp | grep "pid=$(pgrep -x camlooper)"`.
2. On the phone tap **Get a code**. On the desktop open **Advanced → Phone remote** and
   type the six digits into the boxes.
3. Type a deliberately wrong code (e.g. `000000`) first, then the real one.
4. Record or load a clip so the Loop step is reached, then turn the virtual camera **off**
   under Advanced → Virtual Camera. Minimise CamLooper.
5. On the phone paste `https://zoom.us/j/123456789` and tap **Open on desktop**.
6. On the phone send `http://zoom.us/j/1`, then `https://example.com/`.
7. Click **Unlink phone** on the desktop. Repeat the `ss` command from step 1.
8. Check the pairing file: Linux `~/.local/share/com.camlooper.app/remote.json`,
   Windows `%APPDATA%\com.camlooper.app\remote.json`, macOS
   `~/Library/Application Support/com.camlooper.app/remote.json`.

**Expected:**
- Step 1: the log reads `remote: idle, no network` and `ss` prints nothing (the webview's
  ad iframe runs in a separate WebKit network process and is not listed).
- Step 3: the wrong code shows "That code didn't work…" under the boxes and the input is
  cleared; `ss` still prints nothing. The real code flips the card to "Linked with
  <phone>" with a green **Connected** badge within ~2 s, two toasts appear ("Linked with
  <phone>" and the start-at-login notice), the **Start at login** switch is on, and `ss`
  now lists exactly one connection to the relay host. The phone's screen moves on to the
  link field by itself.
- Step 5: the CamLooper window comes to the front and un-minimises, the virtual-camera
  badge turns **Active** (the Store build may raise its one-time UAC prompt first), the
  default browser opens the Zoom link, and a toast reads "Opening Zoom meeting from
  <phone>". The phone shows "Opened on <computer>".
- Step 6: nothing opens on the desktop. The phone rejects both links itself; if a link
  does get through, the desktop shows one "Ignored a link from your phone" toast.
- Step 7: the card returns to the code boxes, the **Start at login** entry is removed,
  `ss` prints nothing again, and the phone returns to its "Get a code" screen.
- Step 8: the file exists after step 3 (mode `-rw-------` on Linux/macOS) and is gone
  after step 7.

**Known gotchas:**
- Codes expire five minutes after the phone requested them; "That code didn't work" on a
  code that looks right usually means it is old — get a new one.
- With the network pulled the badge reads **Offline** and reconnects with growing delays
  (1 s → 60 s). That is backoff, not a hang.
- If no clip is loaded the meeting still opens and a "No loop is loaded" hint appears —
  intended.
- Debug builds only: `CAMLOOPER_REMOTE_LINK_CODE=123456` links automatically ~2 s after
  launch, for scripted tests. Release builds ignore it.

### Tray icon, close-to-tray and hidden start at login
_Added 2026-11-08 · All · `src-tauri/src/tray.rs`, `tauri-plugin-autostart`, `tauri-plugin-single-instance`_

A tray icon is always present. Closing the window hides to the tray **only while a phone
is linked**; unlinked, close quits as before. Linking turns on "Start at login", which
launches the app hidden (`--hidden`) so it waits in the tray.

**Setup:** a release build. On GNOME the AppIndicator extension must be enabled (Ubuntu
default) or no tray icon can be drawn.

**Steps:**
1. Launch CamLooper unlinked. Find its icon in the tray / menu bar; right-click it.
2. Click the window's close button.
3. Launch again, link a phone (previous section), then click the close button.
4. Left-click the tray icon.
5. While the window is visible, launch CamLooper a second time from the apps menu.
6. Quit via the tray menu, then start from a terminal with the flag: `camlooper --hidden`
   (AppImage: `./camlooper_*.AppImage --hidden`; Windows: `camlooper.exe --hidden`).
   Wait ten seconds.
7. Choose **Quit** from the tray menu.

**Expected:** step 1 shows a menu with **Show CamLooper**, **Link phone…**, **Start at
login** (checkbox) and **Quit**, in the app's UI language; step 2 quits the process
(`pgrep camlooper` empty); step 3 hides the window but the process stays alive; step 4
brings the window back at 1200×760, focused; step 5 opens no second window — the existing
one comes to the front; step 6 shows no window and **no browser popup**, only the tray icon
(and the Debug logs, once the window is opened, show the welcome popup did not fire until
then); step 7 exits.

**Known gotchas:** on Windows the window may only flash in the taskbar if another app
holds the foreground — a Windows rule, not a bug, as long as clicking the taskbar entry
shows it. On macOS the Dock icon stays; clicking it re-shows the window. `--hidden` while
**unlinked** shows the window anyway (nothing to wait for).

### Welcome page opens in the browser at launch
_Added 2026-09-10 · All · `src/hooks/use-welcome-popup.ts`_

Two seconds after the app starts, it opens `camlooper.com/ads/popup` in the default
browser, at most once every six hours. The throttle is persisted, so restarting the
app repeatedly does not reopen it.

**Setup:** a **release** build — the hook is guarded by `import.meta.env.PROD` and
does nothing under `npm run tauri dev`. Set the app UI language to something other
than English (say German) under Settings before starting. Launch normally (not from
the start-at-login entry).

**Steps:**
1. Open devtools on the running app and run
   `localStorage.removeItem("camlooper.lastWelcomePopupShown")`, then quit.
2. Launch CamLooper and watch the screen for the first five seconds.
3. Read the URL in the browser's address bar and the language of the page copy.
4. Quit the app and launch it again immediately.
5. In devtools, run `localStorage.getItem("camlooper.lastWelcomePopupShown")`.
6. Repeat step 1, then start the app with the network cable pulled / wifi off.

**Expected:**
- Step 2: the app window paints first, then a browser tab opens about two seconds
  later. The app window is still running behind it, not closed or crashed.
- Step 3: the URL is `https://camlooper.com/ads/popup?lng=de` and the heading is the
  German welcome copy — the `lng` must match the app's UI language, including when
  that language was auto-detected from the OS rather than picked in Settings.
- Step 4: **no** browser tab opens.
- Step 5: a millisecond timestamp within the last few minutes.
- Step 6: the app starts normally and does not hang or show an error; the timestamp
  is written anyway, so the next launch stays quiet.

**Known gotchas:**
- Nothing happens in `tauri dev` — that is the dev guard, not a broken build.
- The page may show either an Adsterra ad or the built-in house ad; both are passes.
  Localhost and ad blockers force the house ad.
- The browser takes focus away from the app you just opened. That is the intended
  behaviour of this feature, not a focus bug.
- Six hours is a floor, not a schedule — the app never opens the page while running,
  only at launch.
- When the app is started hidden at login (phone remote, `--hidden`), the page does not
  open until the window is first shown; the two-second timer starts then.

### Aspect ratio: sources are letterboxed, not stretched
_Added 2026-09-05 · All · single-ffmpeg pipeline_

A clip whose shape does not match the selected output resolution now gets black
bars instead of being distorted. This is most visible on clips recorded **in the
app**, because the recorder captures 4:3 while the camera defaults to 16:9 — every
one of those used to be stretched horizontally.

**Setup:** a build, plus a 4:3 or vertical/portrait video (a phone clip is ideal).
Output resolution left at 720p.

**Steps:**
1. Load the 4:3 or portrait clip and start the virtual camera.
2. Open the camera in any consumer — Zoom's video preview, OBS, or on Linux
   `ffplay /dev/video0`.
3. Look at a circular or square object in the frame, or at any on-screen text.
4. Switch the Resolution setting in Advanced to 1080p and repeat.

**Expected:** the picture is centred with black bars on the left and right (or top
and bottom for a portrait clip). Circles stay circular and faces are not widened.
The bars are pure black and the same width on both sides. Nothing is cropped —
the whole frame is visible.

**Known gotchas:** black bars are the *fix*, not a regression — before this change
the picture filled the frame by being stretched. A 16:9 source into a 16:9 camera
has no bars at all, so test with something that is not 16:9.

### Natural motion starts immediately
_Added 2026-09-05 · All · incremental walk store_

Natural motion used to decode the entire buffered window before playing anything.
On a long clip that was seconds of "Preparing…". It now starts on about one
second of video and fills the rest in behind playback.

**Setup:** a build, natural motion ON, and a clip of at least 30 seconds. Watch the
Advanced panel so the buffered-seconds readout is visible.

**Steps:**
1. Load the long clip and press Play. Start counting as you click.
2. Watch the "Preparing…" badge.
3. With the Advanced panel open, watch the buffered-seconds readout for the next
   ~30 seconds while playback continues.
4. Press Stop, then Play again on the same clip.

**Expected:** the preview shows motion within a moment of clicking — the
"Preparing…" badge appears only briefly rather than for seconds. Playback is
smooth from the first frame and never stalls or jumps while the readout climbs.
The readout rises as more of the clip is buffered and settles at either the clip
length or the window cap. After Stop → Play the clip is decoded again, so the
brief badge reappears; after Pause → Play it does not.

**Known gotchas:** the walk cannot outrun the decoder by construction, so a stutter
just after starting is not expected — if you see one, note the clip's resolution and
frame rate. Stop deliberately frees the buffer (that is the memory fix), which is
why only Stop → Play re-decodes.

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

### Start at login (direct, Store NSIS and MSIX) for the phone remote
_Added 2026-11-08 · Windows · `tauri-plugin-autostart`, `windows/hooks.nsh`, `windows/hooks-store.nsh`_

Linking a phone writes an HKCU Run entry; unlinking or uninstalling removes it.

**Setup:** a clean VM (see VIRTUALBOX_WINDOWS_TESTING.md) with the direct installer, the
Store installer and the `.msix` from `npm run package:msix`.

**Steps (repeat per channel):**
1. Install, launch, link a phone (All platforms section).
2. In PowerShell: `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v camlooper`.
3. Open Task Manager → **Startup apps**; find CamLooper.
4. Sign out and back in. Wait 15 s. Run `Get-Process camlooper`.
5. Turn **Start at login** off under Advanced → Phone remote; repeat step 2. Turn it on again.
6. Click **Unlink phone**; repeat step 2.
7. Link again, then uninstall; repeat step 2.

**Expected:** step 2 prints the exe path followed by ` --hidden` (direct:
`C:\Program Files\CamLooper\camlooper.exe --hidden`; Store NSIS: a path under
`%LOCALAPPDATA%`); step 3 shows CamLooper **Enabled**; step 4: no window, tray icon
present, one `camlooper` process, no browser popup; steps 5, 6 and 7: the value is gone
(step 7 via the `NSIS_HOOK_POSTUNINSTALL` hook).

**Known gotchas:** on the **MSIX** build step 2 may print "unable to find the specified
registry key" even though the card's switch is on — registry virtualization. That is the
signal to add `RegistryWriteVirtualization=disabled` in `scripts/package-msix.ps1` (or a
`windows.startupTask` extension) and retest. Disabling the entry in Task Manager flips
the switch off at the next launch; that is the plugin honouring the user.

### Thank-you page opens when the installer finishes
_Added 2026-09-10 · Windows · `src-tauri/windows/hooks.nsh`, `windows/hooks-store.nsh`_

The NSIS installer opens `camlooper.com/thank-you` in the default browser at the end
of a normal install. It must open as the logged-in user even though the direct
installer is elevated, and must stay quiet during a silent (`/S`) install.

**Setup:** a clean Windows VM and both installers — `camlooper_<v>_x64-setup.exe`
(direct) and `camlooper_<v>_x64-store-setup.exe` (Store).

**Steps:**
1. Run the direct installer. In the language dialog pick **Hebrew**, then finish the
   install.
2. When the browser opens, open Task Manager → **Details**, add the **Elevated**
   column, and find the browser process.
3. Read the address bar and the page copy.
4. Uninstall, then run `camlooper_<v>_x64-setup.exe /S` from an elevated
   `cmd`. Wait for it to finish (`Get-Process msiexec,camlooper -ErrorAction Ignore`
   is not the signal; just wait ~30 s and check `C:\Program Files\CamLooper`).
5. Uninstall, then run the **Store** installer by double-clicking it (not `/S`).

**Expected:**
- Step 2: the browser's **Elevated** column reads **No**. A `Yes` here is a failure —
  the installer is running as Administrator and must hand the URL to `explorer.exe`.
- Step 3: the URL is `https://camlooper.com/thank-you?ch=direct&lcid=1037`, and the
  page is in Hebrew, right-to-left. `CamLooper Virtual Camera` stays on one line —
  it must not split or reorder mid-phrase.
- Step 4: the app installs but **no** browser window opens. Store policy 10.2.9.2
  requires a silent install to stay silent, so this must hold.
- Step 5: the page opens with `ch=store` in the URL.

**Known gotchas:**
- In the real Microsoft Store flow the Store runs the installer with `/S`, so the
  thank-you page never appears for Store users. That is deliberate — do not "fix"
  it by removing the `IfSilent` guard.
- The page opens *before* the installer's finish page, so the browser appears while
  the installer is still on screen. Tauri v2 exposes no finish-page hook.
- An unrecognised or missing `lcid` falls back to English copy; that is the intended
  fallback, not a mapping failure. Only a *wrong* language for a supported LCID is.
- The page is `noindex` and not in the sitemap, so it will never appear in search.

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

### Start at login via XDG autostart (deb and AppImage) for the phone remote
_Added 2026-11-08 · Linux · `tauri-plugin-autostart`_

**Setup:** a GNOME or KDE session; the `.deb` and, separately, the AppImage.

**Steps:**
1. Install/launch, link a phone (All platforms section).
2. `cat ~/.config/autostart/camlooper.desktop`.
3. Log out and in; wait 15 s; `pgrep -a camlooper`.
4. Click **Unlink phone**; `ls ~/.config/autostart/`.

**Expected:** step 2 shows `Exec=/usr/bin/camlooper --hidden` for the deb and
`Exec=/path/to/camlooper_<v>_amd64.AppImage --hidden` for the AppImage (a mount path under
`/tmp/.mount_…` is a failure); step 3 lists one process started with `--hidden`, no window,
tray icon visible; step 4 shows no `camlooper.desktop`.

**Known gotchas:** moving the AppImage after linking leaves a stale `Exec=` — toggle the
switch off and on to rewrite it. Fedora/vanilla GNOME has no tray without the
AppIndicator extension; the deb/rpm now depend on the appindicator runtime library.

---

## macOS

<!-- Newest entries first. -->

### Start at login via LaunchAgent for the phone remote
_Added 2026-11-08 · macOS · `tauri-plugin-autostart` (LaunchAgent mode)_

**Setup:** any build (unsigned is fine for this test).

**Steps:**
1. Launch, link a phone (All platforms section).
2. `plutil -p ~/Library/LaunchAgents/camlooper.plist`.
3. Log out and in; wait 15 s; check the menu bar and `pgrep -a camlooper`.
4. Click **Unlink phone**; `ls ~/Library/LaunchAgents/`.

**Expected:** step 2 shows `RunAtLoad => 1` and `ProgramArguments` ending in `--hidden`;
step 3: no window, menu-bar icon present; step 4: the plist is gone.

**Known gotchas:** System Settings → General → Login Items lists it under "Allow in the
Background", not "Open at Login". The virtual camera itself is still unavailable on macOS
(next section), so a meeting sent from the phone opens without a working camera.

### The virtual camera reports itself unavailable
_Added 2026-09-05 · macOS · sink restructure_

macOS has no working virtual camera: the CMIO Camera Extension cannot load until
the app is signed and notarised, and its frame receiver is still a stub. The app
used to accept the toggle anyway — it logged every frame and slept — so the UI
showed a running camera that no application could ever see.

**Setup:** an unsigned macOS build.

**Steps:**
1. Load any video.
2. Turn on the virtual camera.
3. Open Zoom or Photo Booth and look at the camera list.

**Expected:** an error appears saying the virtual camera is not available on this
platform and that macOS needs a signed, notarised build. The toggle does not stay
on. "CamLooper Virtual Camera" does not appear in the camera list — which was
already true, but the app now says so instead of implying otherwise.

**Known gotchas:** this is a deliberate change from silently pretending to work.
Video loading, playback and the in-app preview are unaffected; only the camera
output is refused. Recording still works.
