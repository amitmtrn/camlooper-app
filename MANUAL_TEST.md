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

### Auto-join: CamLooper opens the meeting in its own browser window and clicks Join
_Added 2026-11-09 · All · `src-tauri/src/meeting/`, `src/hooks/use-meeting.ts`, `src/components/RemoteCard.tsx`_

With a phone linked, a meeting link sent from the phone no longer just opens in the default
browser. CamLooper launches Google Chrome, Microsoft Edge, Brave or Chromium (the first one
found, in that order) with its **own profile** (`<app data>/meeting-browser`), lets the
browser grant camera and microphone by itself, and clicks through the pre-join screen
(guest name, camera on, "Join now"). The join is driven over the browser's DevTools port on
127.0.0.1; nothing new is sent to api.camlooper.com.

**Setup:** a linked phone (next section); Chrome, Edge or Brave installed; a real meeting you
can host from another device or your normal browser (Google Meet is the quickest). Phone-free
alternative on a debug build:
`CAMLOOPER_TEST_OPEN_MEETING='https://teams.microsoft.com/l/meetup-join/…' npm run tauri dev`
drives the join 3 s after startup.

**Steps:**
1. Open **Advanced → Phone remote** (linked). Read the **Join meetings automatically** block.
2. Toggle the switch off and on. Type a name under "Your name in meetings". Restart the app.
3. Click **Open meeting browser**.
4. Close that window. Start a Meet meeting in your normal browser and, from the phone, send its
   link with **Open on desktop**.
5. In the host's browser, admit the guest when asked.
6. Send the same link again while the CamLooper browser window is still open.
7. Send a link to a meeting that does not exist (e.g. `https://meet.google.com/abc-defg-hij`).
8. Turn the switch **off** and send a link.
9. With the switch on, make the browser unavailable (rename the binary, or test on a machine
   without Chrome/Edge/Brave) and send a link.
10. Open **Advanced → Debug logs** after each send.

**Expected:**
- Step 1: the block names the browser found ("…separate Google Chrome window…"); the switch
  is on by default.
- Step 2: both preferences survive the restart (stored on this computer only).
- Step 3: a browser window opens with three tabs (Meet, Teams, Zoom sign-in) in a profile that
  has none of your regular browser's sign-ins. Signing in there is remembered for later joins.
- Step 4: toast "Joining Google Meet meeting from <phone> in Google Chrome…". A browser window
  opens on the meeting; the camera/microphone prompt is **not** shown; the name field is filled
  with your name; "Ask to join" (or "Join now" when signed in) is clicked without you touching
  anything. Toast "Waiting to be admitted" while the host has to let you in, then "Joined the
  Google Meet meeting" once you are in.
- Step 5: you are in the call with the camera on. Pick the CamLooper camera once in Meet's
  settings; the profile remembers it.
- Step 6: the link opens as a new tab in the existing CamLooper browser window (no second
  window) and is driven the same way.
- Step 7: toast "Google Meet didn't let CamLooper in" with the page's message; no retry loop.
- Step 8: the link opens in your default browser as before; nothing is clicked for you.
- Step 9: toast "Opened in your default browser" with "Needs Google Chrome, Microsoft Edge or
  Brave…"; the switch is disabled with the same note.
- Step 10: `meeting: opening <provider> meeting in <browser> (new|running instance)`, then
  `meeting: <provider> join step: waiting / clicked_join / joined` (or `blocked`). If the join
  gives up after 3 minutes, toast "Couldn't click Join automatically" and the log ends with
  `meeting: gave up … visible controls: …` listing the page's buttons — keep that line for the
  bug report, it is what fixes a renamed selector.

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
8. Linux only, no tray: disable the AppIndicator extension
   (`gnome-extensions disable ubuntu-appindicators@ubuntu.com`, or use vanilla GNOME), then
   run `camlooper --hidden` from a terminal.

**Expected:** step 8 still opens the main window (the app does not exit and does not stay
invisible), and the terminal prints
`tray: could not create tray icon, showing window instead: …`. If the tray is created
anyway (some desktops create it with no host), the window stays hidden — that is not this
fix failing. Step 1 shows a menu with **Show CamLooper**, **Link phone…**, **Start at
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

### Phone camera stream: Windows Firewall and the "couldn't connect" message
_Added 2026-11-24 · Windows, direct-download and Store builds alike (nothing here happens at install time) · `src/hooks/use-phone-stream.ts`, `src/i18n/locales/*.json` (`phoneStream.errors.ice_failed_windows`)_

The phone connects *to* the computer over UDP. If Windows Firewall blocks inbound traffic
for CamLooper, the connection fails after 15 s; on Windows the error message then also
points at the firewall. The dialog, banner, stop and tray behaviour are the Linux entry
"Phone camera stream into v4l2…" (part A) — run those steps on Windows too; this entry only
covers what differs.

**Setup:** a Windows build installed (see
[`../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md`](../manual-testing/VIRTUALBOX_WINDOWS_TESTING.md);
in VirtualBox the VM needs a **bridged** adapter so the phone can reach it — with NAT the
connection can never succeed), a phone linked under **Advanced → Phone remote**, phone and
computer on the same Wi-Fi, the network set to **Private** in Windows. An elevated
PowerShell.

**Steps:**
1. With CamLooper running, list its firewall rules:
   `Get-NetFirewallApplicationFilter -Program (Get-Process camlooper).Path | Get-NetFirewallRule | ft DisplayName,Direction,Action,Profile,Enabled`
   and remove them (append `| Remove-NetFirewallRule` instead of `| ft …`) so Windows asks again.
2. On the phone tap **Start streaming**; in CamLooper click **Accept**.
3. When the "Windows Security — Windows Defender Firewall has blocked some features of this
   app" prompt appears, click **Cancel**.
4. Wait for the banner to change, read the message, then run the command from step 1 again.
5. Remove the rules again (step 1), repeat step 2, and this time tick **Private networks**
   and click **Allow access**.
6. Switch the app language to **Deutsch** and repeat steps 1–4.

**Expected:**
- Step 3: the prompt names CamLooper. It appears once per missing rule, the first time the
  app opens its UDP sockets for a stream (not at install, not at launch).
- Step 4: "Connecting to <phone>…" for up to ~15 s, then the red message: "Phone camera
  stopped. Couldn't connect directly to the phone. Put the phone and computer on the same
  Wi-Fi, or connect the computer to the phone's hotspot. **Also make sure Windows Firewall
  allows CamLooper on private networks.**" The PowerShell command now lists inbound rules for
  CamLooper with `Action Block`.
- Step 5: live within a few seconds, as on Linux. The command lists `Action Allow`,
  `Profile Private`.
- Step 6: the same message in German, ending "…dass die Windows-Firewall CamLooper in
  privaten Netzwerken zulässt."

**Known gotchas:**
- No prompt appears when a rule for this exact program path already exists (allow *or*
  block) — hence step 1. Each install location (direct build, Store NSIS build, a dev build)
  is a different path with its own rules.
- The firewall sentence is shown on Windows for *every* `ice_failed`, also when the real
  cause is the phone being on mobile data. It is a hint, not a diagnosis. Linux and macOS
  show the message without it.
- It can still connect with inbound traffic blocked when the phone exposes a real LAN
  address (the computer's outbound checks open the path); the block bites when the phone
  only offers `.local` (mDNS) addresses. So "blocked but it worked" is not a failure of
  this entry; "blocked, failed, and no firewall sentence" is.
- On the Store build the first Accept may also show one UAC prompt (softcam registration).
  That is unrelated to the firewall prompt; answer it first.
- These steps were written from the code and Windows Defender Firewall's documented
  behaviour, not from a run on a Windows machine. If the prompt wording or the PowerShell
  output differs, fix this entry.

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

### Phone camera stream into v4l2: confirm dialog, live banner, stop, tray, headless CLI (remote v2, phase 1)
_Added 2026-11-24, dialog safety steps (15–21) added the same day · Linux · `src-tauri/src/remote/stream/`, `src-tauri/src/tray.rs`, `src-tauri/examples/phone_stream_cli.rs`, `src/components/PhoneStream.tsx`, `src/hooks/use-phone-stream.ts`_

A phone streams its camera over WebRTC and the stream becomes the v4l2loopback camera. In
the app the user must accept every request in a dialog; the headless CLI (part B) accepts
the first request by itself and has no UI. The dialog, banner and tray item are the same
code on Windows and macOS; only the camera sink differs.

**Already automated, don't repeat by hand:** `cargo test --lib` covers the 60 s decision
timeout, the 15 s quiet period after a decline (and only after a decline), at most 5 dialogs
a minute, stale/cancelled requests, a `stream_stop`
without the session's id being ignored once accepted, the relay-flap grace period while live,
the stop watchdog (10 s; 125 s while the camera is still being prepared) ending as an error
that keeps new requests `busy` until the old session has closed, a phone reusing its
`sessionId`, the camera handoff being released only by the session that holds it, the
prepare timeout being reported while a system prompt is still open, queued relay frames not
crossing over to a newly linked phone, the candidate cap and filter, the error code behind the error banner
(`last_error_code` = `ice_failed` etc.), that a panic in the session still hands the camera
back, and that the tray-label payload accepts the new `stopPhone`/`phoneLive`/`phoneConnecting`
fields. It also covers: an Accept less than 800 ms after the request being refused
(`too_soon`), the status carrying `expiresAt` while a request is pending and a `seq` that
grows with every change, a second Stop ≥ 5 s into a hung stop forcing it (ending as an error,
not idle), the tray tooltip
saying "live" only when live, and the phone's name being cut to 40 characters with control
and bidi-override characters removed.
`cargo test --test phone_stream_loopback` covers the phone closing its connection (ends idle,
no error) and junk datagrams sent at the receiver's ports. Part A covers what needs eyes.

#### Part A — in the app

**Setup:**
- A dev build of the app (`npm run tauri:dev`) or an installed build, v4l2loopback loaded as
  in part B's setup, and a phone linked under **Advanced → Phone remote** (the card says
  "Linked with <phone>").
- On the phone, the CamLooper Remote page (`remote-web/`, not deployed yet): run
  `remote-web/dev/start-phone-tunnel.sh` and open the printed `https://….trycloudflare.com`
  URL in Chrome on the phone. Phone and computer on the **same Wi-Fi**.
- Start a looping video in the app (step 3, "Loop") so you can see it pause and resume.

**Steps:**
1. Open **Advanced** and look at the Phone remote card.
2. Close the window (it hides to the tray because a phone is linked). On the phone tap
   **Start streaming** and allow camera access.
3. In the dialog, watch the countdown for 5 s, then click **Decline**. Within 10 s tap
   **Start streaming** on the phone again.
4. Wait until 15 s have passed since the Decline, tap **Start streaming** again; this time
   click nothing for 60 s.
5. Wait 15 s, tap **Start streaming** again and click **Accept**.
6. While it is live, look at the step-3 card (**Jump in live** switch, **Record a new loop**)
   and the **Advanced → Virtual camera → Enable virtual camera** switch. Open
   `ffplay -f v4l2 /dev/video10`.
7. Hover the tray icon and open the tray menu.
8. Click **Stop** in the red banner.
9. Repeat step 5, then stop from the tray menu with **Stop phone camera**.
10. Put the phone on mobile data (Wi-Fi off), tap **Start streaming**, and click **Accept**.
    Then, separately, repeat step 5 on Wi-Fi and switch the phone's Wi-Fi off while live.
11. Switch the app language to **עברית** (Hebrew) and to **العربية** (Arabic) and repeat step 5.
12. Repeat step 5, and once it is live tap **Stop** *on the phone*. Run the app from a
    terminal for this one so you can read its stderr.
13. Tap **Start streaming**; when the dialog is up, disconnect the *computer* from the
    network (Wi-Fi off / cable out), wait ~5 s and click **Accept**. Reconnect and click
    **Accept** again before the countdown ends.
14. Repeat step 5, and while live put the phone's browser in the background for ~5 s (home
    button), then return to the page.
15. Stray keystroke. Put a text editor in front of CamLooper (CamLooper visible behind it or
    hidden in the tray) and keep typing, pressing **Enter** and **Space** repeatedly, while
    you tap **Start streaming** on the phone. Keep typing for 3 s after the dialog appears.
    Then click into the CamLooper window and immediately click **Accept** as fast as you can.
16. Keyboard. Tap **Start streaming** (15 s after the last decline). With the dialog up, press
    **Tab** six times, then **Shift+Tab** twice, watching the focus ring. Then press **Esc**.
17. Close with the dialog open. After 15 s tap **Start streaming**, and click the window's
    close button (✕) while the dialog is showing. Open the window again from the tray.
18. Quit while live. Repeat step 5, and once live choose **Quit** from the tray menu. Run
    `pgrep -a ffmpeg` straight after.
19. Long and odd names. On the phone page set the device name to
    `WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW` (60 × W) and tap
    **Start streaming**; decline. After 15 s set it to a Hebrew name (`הטלפון של עמית`), tap
    **Start streaming**, **Accept**, and read the live banner with the app in English and
    again in **עברית**.
20. Shrink the window to its smallest size and repeat step 5 up to the dialog.
21. Dismissed error. Produce the red message of step 10 (mobile data), click its **×**, then
    click another window and back on CamLooper, and hide/show it from the tray.

**Expected:**
- Step 1: under "Linked with <phone>" a small grey line: "Stream your phone's camera: open
  the CamLooper Remote page on your phone and tap Start streaming." No other change to the card.
- Step 2: within ~3 s the main window reappears with a dialog "Use your phone as the
  camera?" — body "<phone name> wants to stream its camera to CamLooper. Your current video
  will be paused while the phone is live." — **Decline** and **Accept**, and "Declines
  automatically in 5x s" counting down once a second with no number skipped or shown twice.
  **Accept is greyed out for about a second** after the dialog appears (and again for about
  a second each time the window gains focus), and **neither button has keyboard focus**. The
  rest of the window is dimmed and cannot be clicked or reached with Tab. The taskbar / dock
  entry asks for attention. stderr shows `phone stream: <phone> asks to stream its camera`.
- Step 3: the dialog closes; the phone shows that the computer declined. The loop keeps
  playing. The second tap is declined on the phone straight away and **no dialog appears and
  the window is not raised** on the computer: for 15 s after a Decline new requests are
  declined silently. (A dialog that timed out, or a request the phone cancelled, does not
  start this quiet period; instead at most 5 dialogs are raised per minute.)
- Step 4: the countdown reaches 0 and the dialog closes by itself (within ~1 s of 0); the
  phone shows that the request timed out. Clicking nothing else is needed.
- Step 5: the dialog closes, a red banner appears at the very top: spinner + "Connecting to
  <phone>…" with a **Stop** button, then within a few seconds a pulsing red dot and "Phone
  camera live · <phone> · 1280×720 · 30 fps · H.264 · 00:01", the clock counting up each
  second. Resolution may start lower and climb (see gotchas); fps shows 24–31. The loop preview
  stops updating.
- Step 6: those controls are greyed out and the card shows "Your phone is using the
  CamLooper camera. Stop the phone camera to use these controls." (Steps 1, 2 and the review
  card do the same for **Start camera**, **Loop an existing video**, **Start recording** and
  **Use this clip**.) ffplay shows the phone.
- Step 7: the tray menu has **Stop phone camera** enabled (it is greyed out when nothing is
  streaming). The tooltip reads "CamLooper — phone camera live" where the desktop shows
  tray tooltips (see gotchas); during "Connecting to <phone>…" it reads "CamLooper —
  connecting to the phone…", not "live".
- Step 8: the banner briefly says "Stopping the phone camera…", then disappears; the controls
  are enabled again, the loop resumes in the preview and in ffplay, the phone returns to its
  start screen.
- Step 9: same as step 8, triggered from the tray; afterwards **Stop phone camera** is greyed
  out again and the tooltip is back to "CamLooper".
- Step 10, mobile data: "Connecting to <phone>…" for up to ~15 s, then the banner is replaced
  by a red message that stays: "Phone camera stopped. Couldn't connect directly to the phone.
  Put the phone and computer on the same Wi-Fi, or connect the computer to the phone's
  hotspot." The **×** dismisses it; a new request also clears it. Wi-Fi off while live:
  the picture freezes and within ~10–20 s the banner just disappears (the relay reported the
  phone offline and the video stopped — not an error). If the relay had not noticed yet, the
  message reads "…The phone stopped sending video…" instead (media timeout). Hovering the
  message shows the technical detail as a tooltip.
- Step 12: the banner disappears (it may say "Stopping the phone camera…" for a moment), a
  plain toast says "Phone stopped streaming.", the controls are enabled and the loop resumes.
  **No red "Phone camera stopped…" message.** stderr has
  `phone stream: receiver closed (closed by the phone)` followed by
  `phone stream: ended (user)` (or `ended (peer_stopped)` if the relay's frame was slow) —
  never `ended (error): the connection was closed`.
- Step 13: the first click shows the toast "CamLooper isn't connected to the phone service
  right now. Try again in a moment." and **the dialog stays open**, still counting down; the
  loop keeps playing. After reconnecting (the Phone remote card is back to "Linked with
  <phone>", a few seconds) the second click behaves like step 5. If the countdown ran out
  meanwhile, the dialog just closes.
- Step 14: the stream either carries on or freezes for those seconds and resumes; the banner
  stays "Phone camera live" throughout. The phone's relay connection dropping for a moment
  does not end a stream that is still delivering video.
- Step 15: nothing you typed reaches the dialog: it is still open and counting down, the
  stream has **not** started (no red banner, the loop still plays). The fast click on
  **Accept** right after focusing the window does nothing — the button is grey for about a
  second — and a second click once it is coloured again accepts.
- Step 16: focus only ever moves between **Decline** and **Accept**; it never lands on a
  control behind the dialog (the language picker, the Advanced switches). **Esc** closes the
  dialog and the phone shows that the computer declined, exactly like clicking **Decline**.
- Step 17: the window hides to the tray and the phone shows "declined" within a second or
  two — not a timeout a minute later. Reopened, the window has no dialog.
- Step 18: the phone returns to its start screen within ~3 s (it is told the stream stopped;
  it does not sit on a frozen picture until its own timeout), the tray icon disappears, and
  `pgrep` prints no CamLooper ffmpeg.
- Step 19: the 60-W name is shown cut to 40 characters and wrapped inside the dialog;
  **Decline** and **Accept** are fully visible. With the Hebrew name the banner reads, in both
  languages, name first then "1280×720 · 30 fps · H.264 · 00:07" with the digits in that
  order (never "720×1280" or "fps 30"), and "Connecting to הטלפון של עמית…" keeps the "…" at
  the end of the sentence.
- Step 20: the dialog scrolls inside the window if it does not fit; both buttons can be
  reached.
- Step 21: the red message stays gone after each of those. (It comes back only for a *new*
  failure.)
- Step 11: dialog and banner text are translated and laid out right-to-left (icon and title
  on the right, buttons on the left); in the banner the "1280×720 · 30 fps · H.264 · 00:12"
  part still reads left-to-right with no scrambled digits.

**Known gotchas:**
- Linux tray icons (AppIndicator) do not show tooltips at all; check the tooltip on Windows
  or macOS. On GNOME without the AppIndicator extension there is no tray: the window then
  never hides, and step 7/9 cannot be run.
- A request deliberately does **not** take keyboard focus. If CamLooper was hidden in the
  tray the window appears (most desktops put it in front); if it was open behind another
  window it may stay behind and only flash in the taskbar / bounce in the Dock (GNOME shows
  "CamLooper is ready"). That is intended, not a failure: click the window and answer.
- There is no separate desktop notification; the window and the attention request are the alert.
- The welcome page in the browser and the ad strip at the top wait while a request is on
  screen (a hidden start followed by a phone request); they appear ~2 s after it is answered.
- After a Decline or a timeout the window that was raised from the tray stays open; close it
  again by hand.
- A stop that hangs: if the banner still says "Stopping the phone camera…" after 5 s, its
  **Stop** button (and the tray item) become clickable again and force the stop. A forced
  stop (or the watchdog giving up by itself) ends in the red error banner, not in a clean
  idle: the old session still holds the camera, so playback may refuse to start and the
  phone's next request is answered "busy" until it has shut down — then the error clears by
  itself. This needs a hung driver to see and is covered by `cargo test --lib` instead.
- Stop pressed while the polkit prompt is open (Linux, first accept after boot): the prompt
  closes by itself within a second or two (stderr: `phone stream: ended (user)`) and the
  banner disappears. On the Windows Store build the UAC prompt cannot be closed by the app:
  the banner says "Stopping the phone camera…" until you answer it (or force the stop after
  5 s, see above).
- Screen readers: the dialog is announced with its text and "Declines automatically in 60 s",
  then again at 30 s and 10 s only. The live banner announces "Phone camera live" once, not
  the clock.
- Accepting may trigger one polkit prompt the first time (loading v4l2loopback), or one UAC
  prompt on the Windows Store build (softcam registration). You have two minutes to answer
  it; the banner says "Connecting…" meanwhile. (The phone page may give up waiting earlier
  and show its own timeout — start again once the prompt is answered.)
- The 15 s quiet period after a Decline is deliberate (a phone cannot keep popping the
  window up). A request during it is declined on the phone at once; it is not lost or stuck.
  The same happens to a sixth request within a minute, whatever became of the first five —
  when repeating steps quickly, leave a minute before deciding that requests are "stuck".
- If you click **Accept** in the last second, you may get the toast "That request has already
  expired…" — the backend timed out first. Expected.
- The phone page itself is English-only, so the translated hint and dialog name the phone's
  button "Start streaming" in English.

#### Part B — headless CLI (no app UI)

**Also automated:** `cargo test --test phone_stream_loopback` runs a
simulated phone through the real receiver and ffmpeg for H.264 and VP8 and requires ≥ 30
decodable frames, also after a staged ffmpeg crash (restart + keyframe). `cargo test --lib` covers the signaling rules (busy, 60 s timeout, stale
session ids) and the ffmpeg arguments per sink. These steps cover what needs a real phone,
a real network and eyes.

**Setup:**
- Linux with v4l2loopback: `sudo modprobe v4l2loopback devices=1 video_nr=10
  card_label="CamLooper Virtual Camera" exclusive_caps=1`. `v4l2-ctl --list-devices` lists
  `CamLooper Virtual Camera` at `/dev/video10`. Also `ffmpeg`/`ffplay` and `v4l2-utils`.
- An Android phone with Chrome on the **same Wi-Fi** as the computer, with the CamLooper phone
  stream page open. It shows a 6-digit code.
- In `src-tauri/`: `cargo build --example phone_stream_cli`.

**Steps:**
1. `./target/debug/examples/phone_stream_cli --code <6 digits> --device /dev/video10 --seconds 120 2> >(tee /tmp/cli-plain.err >&2) | tee /tmp/cli.jsonl`
2. On the phone tap the button that starts streaming and allow camera access.
3. In a second terminal: `ffplay -f v4l2 /dev/video10`. Hold the phone in portrait, then landscape.
4. Watch the `"event":"status"` lines in the first terminal for 30 s.
5. `v4l2-ctl -d /dev/video10 --get-fmt-video` and `ffmpeg -f v4l2 -i /dev/video10 -frames:v 90 -f null - 2>&1 | grep frame=`.
6. Stop streaming on the phone.
7. Run step 1 again with a new code, start streaming, then switch the phone's Wi-Fi off.
8. Run step 1 again with `--debug` added and stderr sent to `/tmp/cli-debug.err` instead; stream
   for 10 s and stop. Then:
   `grep -cE 'a=candidate|[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' /tmp/cli-plain.err /tmp/cli-debug.err` and
   `grep -ciE 'token|bearer' /tmp/cli.jsonl /tmp/cli-plain.err /tmp/cli-debug.err`.

**Expected:**
- Step 1: `{"event":"paired",…}`, `{"event":"connected","protocol":2}` and a `hello` line.
  stderr prints no token.
- Step 2: within ~3 s: `stream_request` (`"autoAccept":true`), state `connecting`,
  `send stream_accept`, `send status`, `send answer`, then state `live`. stderr shows
  `phone stream: live (H264 …)`. Only a phone without H.264 shows `VP8`.
- Step 3: the phone's picture plays in ffplay at 1280x720 within ~1 s of connecting. Portrait
  is **pillarboxed** (black bars left and right), never stretched; landscape fills the frame.
- Step 4: `"fps"` stays between 24 and 31; `"size"` may start small (e.g. `320x180`) and
  should reach `1280x720` (or the phone camera's 16:9 size) within ~20 s on a good network.
  `"frames"` keeps increasing.
- Step 5: `Width/Height 1280/720`, `Pixel Format 'RGB3'`; ffmpeg reads 90 frames in about 3 s.
- Step 6: state `idle` (possibly via `stopping`), **never `error`**, then
  `{"event":"unpaired","revoked":true}`; the CLI exits with status 0 and the phone page
  returns to its unlinked screen. stderr shows
  `phone stream: receiver closed (closed by the phone)` and `phone stream: ended (user)` or
  `ended (peer_stopped)` — not `ended (error): the connection was closed`.
- Step 7: within ~10–20 s the CLI logs `phone stream: ended (peer_offline)` or `(timeout)`,
  state goes to `idle` (or `error` if the relay had not yet reported the phone offline), and
  the CLI exits.
- Step 8: the first grep prints `0` for `cli-plain.err` (no SDP or IP address without
  `--debug`) and a number > 0 for `cli-debug.err`. The second grep prints `0` for every file.

**Known gotchas:**
- A low starting resolution (320x180 for a few seconds) is Chrome ramping its bitrate, not a
  failure. It should climb; if it stays low for a minute, note the network.
- No TURN: phone and computer on different networks (phone on mobile data) may fail with
  `ice_failed` after 15 s. That is expected in phase 1.
- `ffplay` shows the frozen last frame for up to 10 s after the phone vanishes; that is
  the media timeout.
- `--null` instead of `--device` needs no v4l2loopback at all (decode and discard). Without
  either flag the CLI uses the app's own handoff: it finds the CamLooper device and may ask
  polkit to load the module once.
- Codes expire after five minutes.

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

### The notarised DMG installs and runs on a clean Mac
_Added 2026-11-19 · macOS · uncommitted (Developer ID DMG) · signing and launch verified; **camera through the notarised extension not yet verified**_

`scripts/macos-dmg-build.sh` produces `src-tauri/target/dmg/CamLooper-<version>.dmg`:
Developer ID signed, hardened runtime, notarised and stapled. The script itself fails if
notarisation is not accepted or Gatekeeper rejects the result, so those are not repeated
here.

**Setup:** an Apple Silicon Mac on macOS 13 or later that has never had CamLooper
installed (or run `systemextensionsctl list` and confirm no `com.camlooper.app` line).
Copy the DMG to it by AirDrop or a download, so it carries the quarantine flag a real
user's copy would.

**Steps:**
1. Double-click the DMG and drag CamLooper to Applications. Open it from Applications.
2. Approve the camera extension when the app asks (System Settings opens by itself).
3. Click **Start camera**, allow camera access, record ten seconds, continue to Step 3.
4. Open Photo Booth or QuickTime Player → New Movie Recording and pick
   **CamLooper Virtual Camera**.

**Expected:**
- Step 1: the app opens with no "unidentified developer" or "damaged" warning; at most the
  standard "downloaded from the Internet" confirmation.
- Step 2: after approval `systemextensionsctl list` shows
  `com.camlooper.app.CameraExtension ... [activated enabled]`.
- Step 3: live preview moves and keeps moving while recording.
- Step 4: the other app shows the looping clip.
- `spctl -a -vv /Applications/CamLooper.app` prints `source=Notarized Developer ID`.

**Known gotchas:**
- Updating over an older install: macOS only swaps the camera extension when its
  `CFBundleVersion` (in `macos-extension/CameraExtension/Info.plist`) is higher than the
  installed one. With the same number the old extension stays, silently.
- After such a swap the old extension shows as `[terminated waiting to uninstall on
  reboot]`, and on the Mac this was developed on the virtual camera disappeared from the
  camera list until a restart. If the camera is missing after an update, restart before
  investigating.
- Intel Macs cannot run this build; the bundled FFmpeg is arm64 only.

### The app works inside the App Sandbox
_Added 2026-11-19 · macOS · uncommitted (sandboxed build) · backend automated; **UI not yet seen sandboxed**_

`dev-install.sh` now signs the app with the App Sandbox (camera + outbound network) and
the bundled `ffmpeg` as a sandbox-inheriting helper, as the Mac App Store requires.

**Automated:** `scripts/macos-sandbox-test.sh` runs the real-hardware backend tests inside
the sandbox (virtual camera feed, live preview, recording, resume, and a recorded clip
looping into the camera for three passes) and fails unless the recording landed in the
sandbox container. The looping check alone, unsandboxed:
`cd src-tauri && cargo test --lib -- --ignored --nocapture a_recorded_clip_loops_into_the_virtual_camera`. Needs a camera and the approved extension.

**Setup:** `/Applications/camlooper.app` installed by `dev-install.sh`, extension approved.

**Steps (what the script cannot see):**
1. Run `codesign -d --entitlements - /Applications/camlooper.app | grep app-sandbox`.
2. Open the app. Click **Start camera**, record ten seconds, stop, and continue to Step 3
   so the recorded clip loops as the camera.
3. In Step 1, load a video file from disk with the upload control and press play.
4. Open Zoom (restart it if it was open) and select **CamLooper Virtual Camera**.

**Expected:** step 1 prints the `app-sandbox` key. Steps 2–3 behave exactly as before
sandboxing: live preview moves, the recording plays back, the uploaded video plays. Step 4
shows the same picture as the app's preview. `ls ~/Library/Containers/com.camlooper.app`
exists, and recordings are under its `Data/tmp/`, not under `$TMPDIR`.

**Known gotchas:**
- The first sandboxed launch asks macOS to activate the extension again, because the
  "already activated" note moved into the container. It should pass without a new approval.
- The ad banner needs the network; if it is blank, check Console for `Sandbox: camlooper
  deny` lines before assuming an ad-server problem.
- macOS asks for camera permission again the first time: the sandboxed app is a new
  client as far as the permission system is concerned.

### The installed app runs without Homebrew
_Added 2026-11-19 · macOS · uncommitted (bundled LGPL FFmpeg)_

The app used to link Homebrew's FFmpeg libraries and run Homebrew's `ffmpeg`, so it only
worked on a developer's Mac. It now ships its own. `dev-install.sh` already fails the
install if any Homebrew path is linked; these steps check the app actually uses its copy.

**Setup:** `/Applications/camlooper.app` installed by `dev-install.sh`.

**Steps:**
1. Run `otool -L /Applications/camlooper.app/Contents/MacOS/camlooper /Applications/camlooper.app/Contents/MacOS/ffmpeg | grep -E "homebrew|usr/local"`.
2. Run `/Applications/camlooper.app/Contents/MacOS/ffmpeg -hide_banner -buildconf`.
3. Launch the app with Homebrew hidden: `env PATH=/usr/bin:/bin open -n /Applications/camlooper.app`,
   load a video and press play; then use **Start camera** in Step 1.

**Expected:** step 1 prints nothing. Step 2 lists `--enable-libvpx` and neither
`--enable-gpl` nor `--enable-nonfree`. Step 3: the video plays in the preview and the live
camera preview appears, exactly as with Homebrew present.

**Known gotchas:** a Mac with Homebrew ffmpeg installed can mask a missing bundled copy,
because the app falls back to `/opt/homebrew/bin/ffmpeg`. Check
`ls /Applications/camlooper.app/Contents/MacOS` shows both `camlooper` and `ffmpeg`.

### Step 1 camera picker lists the Mac's real cameras
_Added 2026-11-19 · macOS · uncommitted (AVFoundation capture)_

The picker used to show a single entry reading "Virtual camera not supported on this
platform", and Start camera then tried to open `/dev/video0` through V4L2, which does not
exist on macOS. Cameras are now listed and opened through ffmpeg's AVFoundation input.
The parsing and argument building are covered by `cargo test --lib`
(`camera_capture::tests::avfoundation_*`); the steps below cover what needs a real camera.

**Setup:** a Mac with a built-in or USB camera. Run `scripts/build-macos-ffmpeg.sh` once,
then `npm run tauri:dev` from a terminal. Compare against
`ffmpeg -hide_banner -f avfoundation -list_devices true -i ""`.

**Steps:**
1. Launch the app and stay on **Step 1 · Go live**.
2. Open the **Camera** dropdown.
3. Pick the built-in camera and click **Start camera**.
4. Click **Next**, record a few seconds, and stop the recording.

**Expected:** the dropdown has one entry per camera in the "AVFoundation video devices"
section of the ffmpeg listing, shown by name (`MacBook Pro Camera`). No "Capture screen N"
entries, no microphones, no "CamLooper Virtual Camera" and no "not supported" entry. After **Start camera** the camera's
green indicator light comes on, the live preview shows the camera within about two seconds,
and the terminal prints `Spawning ffmpeg (...) with args: ["-f", "avfoundation", ... "-i",
"MacBook Pro Camera:none", ...]`. During step 4 the preview keeps moving for the whole
recording (wave at the camera; the REC timer and the picture must both advance), and a new
`recording_*.webm` appears in `$(getconf DARWIN_USER_TEMP_DIR)`. The recording plays back
in the app.

**Automated equivalent (needs this Mac's camera, so not run in CI):**
`cd src-tauri && cargo test --lib -- --ignored --nocapture live_preview_keeps_moving_while_recording`
runs virtual camera → preview → record → stop → preview against the real backend and
asserts ~30 fps reaches the camera in every phase, the preview image keeps changing, and
the recording file is non-trivial. Prefer it to steps 3–4 unless you need to see the UI.

**Known gotchas:**
- Run step 4 with the virtual camera extension installed and approved as well. macOS moves
  "CamLooper Virtual Camera" to the front of its device list once it is streaming, and a
  preview that freezes the moment recording starts means cameras are being opened by
  position again instead of by name.
- Without the camera extension installed (any `tauri dev` run on a Mac that has never had
  the signed build), clicking Start camera also shows a "Could not start the virtual
  camera" toast. The preview and recording work regardless.
- The first run shows a macOS camera permission prompt. Under `tauri dev` it names your
  terminal app, not CamLooper. If it was denied earlier the preview stays empty and the
  terminal shows an ffmpeg "Input/output error" — re-enable it under System Settings →
  Privacy & Security → Camera.
- Run `scripts/build-macos-ffmpeg.sh` once before building: the app links and bundles the
  FFmpeg it produces. Without it the build falls back to Homebrew's, and Homebrew's
  FFmpeg 9 does not compile with the `ffmpeg-next` crate.
- Not yet verified in a bundled `.app`: it adds `NSCameraUsageDescription` via
  `src-tauri/Info.plist`, but the bundle's sandbox entitlement may still block spawning
  Homebrew ffmpeg.

### Dock icon is the CamLooper logo
_Added 2026-11-19 · macOS · uncommitted (icon.icns regenerated)_

`icons/icon.icns` still contained the stock Tauri logo (yellow and teal interlocking
rings), so the Dock, the app switcher and the `.app` bundle showed that instead of
CamLooper's icon.

**Setup:** a macOS dev run (`npm run tauri:dev`) or a built `.app`.

**Steps:**
1. Launch the app and look at its Dock icon.
2. Press Cmd+Tab and find the app in the switcher.
3. Run `iconutil -c iconset src-tauri/icons/icon.icns -o /tmp/camlooper.iconset && open /tmp/camlooper.iconset`.

**Expected:** a dark rounded square with a purple loop arrow around a camera lens, with
transparent corners (no white box behind it), about the same size as neighbouring Dock
icons. The extracted iconset holds ten PNGs, 16 px through 1024 px, all showing that logo.

**Known gotchas:** macOS caches icons. For a built `.app` that still shows the old logo,
run `killall Dock`; a dev run picks the new icon up on the next rebuild.

### First launch blocks on approving the camera extension
_Added 2026-11-19 · macOS · uncommitted (approval gate) · **overlay not yet seen on a real first run**_

On launch the app asks macOS to install its Camera Extension. If macOS wants the user's
approval, the app opens System Settings itself and covers its whole window with an overlay
until the approval lands. The state mapping is covered by `cargo test --lib`
(`macos_virtual_camera::tests::*`); the flow itself needs a Mac where the extension has
never been approved.

**Setup:** a development-signed install (`src-tauri/macos-extension/dev-install.sh`) on a
Mac where `systemextensionsctl list` shows no `com.camlooper.app.CameraExtension` line, or
shows it as `[activated waiting for user]`. To get back to that state, switch CamLooper off
under System Settings → General → Login Items & Extensions → Camera Extensions, then run
`defaults delete com.camlooper.app CamLooperActivatedExtensionVersion`.

**Steps:**
1. Open `/Applications/camlooper.app`.
2. Without approving anything, try to click the app's buttons behind the overlay.
3. Close System Settings, then click **Open System Settings** on the overlay.
4. Under Extensions, click the ⓘ next to Camera Extensions, switch on CamLooper and
   authenticate.

**Expected:**
- Step 1: within a few seconds System Settings opens at Login Items & Extensions, and the
  app shows a centred card titled "Allow the CamLooper camera" with three numbered steps,
  an **Open System Settings** button and a spinner reading "Waiting for your approval".
- Step 2: nothing behind the overlay responds.
- Step 3: System Settings reopens at the same pane.
- Step 4: within about two seconds the overlay closes by itself and a toast says "CamLooper
  camera approved". `systemextensionsctl list` shows the extension `[activated enabled]`.
  Quitting and reopening the app shows no overlay.

**Known gotchas:**
- macOS may show its own "System Extension Blocked" alert as well; either route works.
- On macOS 13–14 the approval is a button near the bottom of Privacy & Security instead,
  and the overlay's numbered steps (written for macOS 15+) do not match.
- `npm run tauri:dev` never shows the overlay: that build has no extension to approve.
- The overlay text is English in every language for now.

### The virtual camera appears in Zoom and carries the app's video
_Added 2026-11-19 · macOS · uncommitted (CMIO sink-stream camera) · placeholder verified; app video **not yet verified**_

macOS used to refuse the virtual camera outright. It is now a CMIO Camera Extension that
the app installs and feeds through a CoreMediaIO sink stream. Message wording and frame
sizing are covered by `cargo test --lib` (`macos_virtual_camera::tests::*`); everything
below needs a signed build and a human.

**Setup:** a development-signed install made with
`src-tauri/macos-extension/dev-install.sh` (see that directory's README for prerequisites),
on a Mac registered to the signing team. One short `.mp4` with recognisable motion. Zoom or
Photo Booth, **quit** before you start.

**Steps:**
1. Open `/Applications/camlooper.app`, load the video and turn on the virtual camera.
2. First run only: approve the extension when the app asks (see the section above).
3. Press play.
4. Run `systemextensionsctl list`.
5. Open Photo Booth (or Zoom → Settings → Video) and choose **CamLooper Virtual Camera**.
6. Pause playback in CamLooper, wait two seconds, then turn the virtual camera off.
7. Quit CamLooper while Photo Booth still shows the camera.

**Expected:**
- Step 3: no error toast; the camera status shows `1280x720` and the frame counter climbs.
- Step 4: a line for `com.camlooper.app.CameraExtension` ending `[activated enabled]`.
- Step 5: the picker lists "CamLooper Virtual Camera" and shows the playing video —
  correct colours (not blue/red swapped), upright, not mirrored beyond what the viewer
  itself applies, with no visible stutter.
- Steps 6–7: within about a second the picture is replaced by a near-black frame with a
  thin purple bar sliding left to right. The camera stays in the list and does not freeze.

**Known gotchas:**
- The camera always runs at 1280x720 on macOS, whatever Resolution is set to.
- Zoom and browsers only list cameras present when they started; restart them after the
  first approval.
- A camera permission prompt for CamLooper appears on first start: macOS hides every
  camera, including ours, from apps without it.
- Under `npm run tauri:dev` the app can feed an extension that is already installed but
  cannot install one; with none installed the toast says "this build cannot install it".
- After changing extension code, bump `CFBundleVersion` in
  `macos-extension/CameraExtension/Info.plist` or macOS keeps running the old copy.

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
Background", not "Open at Login". A hidden start does not run the camera-extension check
(System Settings must not open at login with no window); it runs the first time the window
is shown. So a meeting sent from the phone only gets the CamLooper camera if the extension
was already approved ("First launch blocks on approving the camera extension" above); an
unsigned build cannot install the extension.
