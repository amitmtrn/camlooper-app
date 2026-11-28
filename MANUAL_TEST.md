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

### Record a loop on the phone (remote v2, phase 3b): phone page and desktop UI
_Added 2026-11-28 · All · `remote-web/index.html`, `src/components/PhoneStream.tsx`, `src/hooks/use-phone-stream.ts`, `src/App.tsx`, locales_

The phone page no longer offers "Choose a video / Send & loop". Next to **Start streaming**
there is **Record a loop**: the phone films up to 1 minute through the normal live path, the
computer saves it, and after Stop the computer loops the take as its camera. The phone can
then be locked or closed.

**Already automated, don't repeat by hand:** `node remote-web/dev/smoke-record.mjs` (needs
`PLAYWRIGHT_CORE`, see `remote-web/dev/README.md`) covers the phone page against a fake
computer: the file card is never visible; `hello` is sent once with `appProtocol: 3`; Record
a loop is disabled until `hello_ok` (appProtocol ≥ 3 with `record`), and with no answer in
5 s / an old answer the page shows "Update CamLooper on your computer to record a loop." and
sends no request; `stream_request` carries `mode: "record"`; `REC 0:0x / 1:00`; Switch camera
disabled; with Quality set to 480 a take is still the 720 preset, sent with
`scaleResolutionDownBy: 1`, `maxBitrate` 2500000 and `degradationPreference:
"maintain-resolution"`; Stop sends `stream_stop{user}`; the phone stops by itself at 1:00; too-short,
failed, cancelled-on-the-computer, declined / busy / timeout / unsupported messages; Stop
loop / Play loop afterwards. What is left for eyes is below.

**Setup:** a build of this branch on the computer (desktop and phone page must both be 3b),
the virtual camera on, a phone linked (see "Phone remote: linking with a code"), both on the
same Wi-Fi. A camera viewer on the computer (Zoom's video preview, or `ffplay /dev/video10`
on Linux).

**Steps:**
1. Open the phone page. Wait for "Online".
2. Look at the "Use this phone as the camera" card.
3. Tap **Record a loop**. On the computer, read the dialog, then click **Accept**.
4. Watch the phone and the top of the CamLooper window for about 10 seconds.
5. On the phone tap **Stop**.
6. Lock the phone (or close the app) and watch the camera viewer for 30 seconds.
7. Unlock the phone, tap **Stop loop**, then **Play loop**.
8. Tap **Record a loop** again, accept, and this time click **Cancel** in the computer's
   banner after about 5 seconds.
9. Tap **Record a loop**, accept, and tap **Stop** on the phone within one second.
10. Tap **Record a loop**, accept, and leave it alone for 60 seconds.
11. Tap **Start streaming**, accept, then stop it from the phone.
12. Old computer: link the phone to a CamLooper build from before 3b (or quit CamLooper,
    start the older build) and reopen the phone page.

**Expected:**
- Step 2: two buttons side by side, **Start streaming** and **Record a loop** (red dot).
  No "Choose a video", no "Send & loop" anywhere on the page. Record a loop becomes
  tappable within a second of "Online".
- Step 3: the dialog title is "Record a loop from <phone name>?" and the text says the
  phone records up to 60 seconds, the recording then loops on the camera like a local video,
  and the phone can be closed afterwards. Accept is greyed for about a second; Escape and
  Decline decline.
- Step 4: phone: red chip `REC 0:03 / 1:00` counting up, the camera preview, **Switch
  camera** greyed out, "Keep this page open and the screen on while recording." Computer: a
  red banner "Recording a loop from <phone name> — 0:03" counting up with a **Cancel**
  button; the camera viewer shows the phone's camera live.
- Step 5: the phone returns to the home screen with a "Loop on your computer" card showing
  a spinner and "Preparing loop…"; both start buttons are greyed until it resolves. The
  computer's banner reads "Preparing loop…". Then: phone shows "Recording done ✓", a green
  dot with "Looping on your computer now", the line "You can lock this phone or close the
  app. The loop keeps playing." and a **Stop loop** button; the computer shows the toast
  "Now looping the recording from <phone name>." and the banner is gone; the video card
  shows the take as the loaded video.
- Step 6: the camera viewer keeps playing the take on repeat.
- Step 7: the viewer freezes/blanks on Stop loop and the button becomes **Play loop**; it
  plays again after Play loop.
- Step 8: the phone leaves the recording screen by itself; the take (longer than 2 s) still
  becomes the loop, with the same success texts as step 5.
- Step 9: phone: "That recording was too short to loop. Record at least 2 seconds." No
  error banner on the computer, and whatever was looping before is unchanged.
- Step 10: at `REC 1:00 / 1:00` the phone stops on its own and step 5's sequence follows.
- Step 11: exactly as before 3b: the "Use your phone as the camera?" dialog, the "Phone
  camera live" banner with **Stop**, Switch camera usable on the phone.
- Step 12: about 5 seconds after "Online" the phone shows "Update CamLooper on your
  computer to record a loop." under the buttons; Record a loop stays greyed; tapping Start
  streaming shows "Update CamLooper on your computer to use this phone as a camera." and
  nothing appears on the computer.

**Known gotchas:**
- The phone's clock starts when the connection is up and is corrected to the computer's own
  count a moment later (the computer skips the first half second), so it can jump back by
  up to a second right at the start.
- A new phone page with an old computer cannot stream live either. That is intended: both
  sides are updated together.
- While "Preparing loop…" shows, the phone waits up to 45 s for the computer. If the
  computer never answers it says "The computer didn't confirm the loop. Check CamLooper on
  your computer." and then shows whatever the computer reports as playing.
- The desktop strings in the 17 non-English locales are machine translated.

### Record a loop on the phone (remote v2, phase 3b): what the computer does with the take
_Added 2026-11-28, resolution ramp (step 3's size check) added the same day · All · `src-tauri/src/remote/stream/record.rs`, `runner.rs`, `machine.rs`, `signal.rs`, `handoff.rs`_

The computer's half of "Record a loop": while the phone's camera is live on the virtual
camera, the same video is saved to a private file; on Stop it is repackaged (not re-encoded)
and looped by the normal loop engine. The buttons and texts are covered by the entry above;
this one is about the result.

**Already automated, don't repeat by hand:** `cd src-tauri && cargo test --lib && cargo test
--test phone_stream_loopback` (needs `ffmpeg`/`ffprobe` on PATH) covers: `hello` →
`hello_ok` and its rate limit; `mode: "record"` never being treated as live, unknown modes
answered `unsupported`; a real WebRTC stream (H.264 and VP8) recorded, remuxed to MP4 / WebM
with the take's duration, handed to the loop engine, `stream_stop{done}`, and the loop
outliving the phone; the 2 s minimum (`too_short`, file deleted); the 60 s cap; cancel while
processing; a take that is mostly stalls or that the loader refuses (`incomplete`, file
deleted, the earlier loop restored); the real loop engine playing a recorded take; and the
encoder's resolution ramp — the take waits until the picture size has been the same for 1 s
(3 s at most), keeps nothing of a smaller prelude, and starts over if the picture still
grows in its first 3 s (`record::tests`, `runner::tests::a_take_that_grows_…`).

**Setup:** as in the entry above. A camera viewer on the computer. On Linux, a terminal
ready with `ls -la ~/.local/share/com.camlooper.app/phone-videos/` (Windows:
`dir %APPDATA%\com.camlooper.app\phone-videos`); if the folder is elsewhere on your build,
it is `phone-videos` inside the app's data folder.

**Steps:**
1. With a local video looping in CamLooper, tap **Record a loop** on the phone and accept.
2. Wave a hand in front of the phone for about 10 seconds while watching the camera viewer,
   and list the `phone-videos` folder during that time.
3. Tap **Stop** on the phone. When the phone says "Looping on your computer now", list the
   folder again and watch the viewer for 30 seconds. Then, on Linux:
   `ffprobe -v error -select_streams v:0 -show_entries stream=width,height:format=duration -of csv=p=0 ~/.local/share/com.camlooper.app/phone-videos/phone-*.mp4`
4. Turn the phone's Wi-Fi off (or close the app). Watch the viewer for another 30 seconds.
5. In CamLooper, load a video from the computer with the file picker. List the folder.
6. Record again, and after 5 seconds turn the phone's Wi-Fi off instead of tapping Stop.
   Wait 15 seconds, then list the folder.

**Expected:**
- Step 2: the viewer shows the phone's camera live (not the local video), without visible
  stutter from the recording. The folder holds exactly one file, `phone-<16 hex>.part`,
  growing.
- Step 3: within a few seconds the viewer switches from the live picture to the take. It
  shows the hand-waving from about a second after "live" up to the Stop, plays past its
  own length without a freeze or jump back to a black frame (natural motion), and the
  picture is as sharp as the live one was (it is the same video, not a re-encode). The
  folder holds exactly one file, `phone-<16 hex>.mp4` (`.webm` if the phone sent VP8); no
  `.part`. The video card in CamLooper shows "Phone recording.mp4". The `ffprobe` line
  prints `1280,720` (or `720,1280` for a phone held upright; the camera's own 16:9 size if
  it has no 720p) and a duration within about 2 seconds of how long you filmed — not
  `320,180` and not a 5-second clip of a 10-second take, which is what a take cut short by
  the encoder's ramp looked like.
- Step 4: nothing changes in the viewer. No error appears in CamLooper.
- Step 5: the folder is empty.
- Step 6: within about 15 seconds the recording banner disappears, as it does for a live
  stream whose phone vanished; nothing of the take is kept (a take is only looped when it is
  stopped, not when the connection is lost); the video from step 5 is looping again; the
  folder is empty.

**Known gotchas:**
- The loop starts about a second into what the phone filmed (up to 3.5 s on a phone or
  network that keeps changing the picture size): the first moments, while the phone's
  encoder is still ramping up its quality, are skipped on purpose. The REC clock on the
  phone starts when the take does, so it is not behind.
- If the REC clock on the phone jumps back to 0:00 once in the first 3 seconds, the picture
  grew just after the take had started and the take was started again at the larger size.
  The terminal the app runs in says "the picture grew to WxH; the take starts again".
- On a bad network a take may look choppier than a live stream at the same moment: a take
  keeps its size and gives up frames, a live stream does the opposite.
- If the phone films at a low frame rate (a dark room), the loop has that frame rate too.
- A take stopped under 2 seconds leaves no error on the computer at all — the message is on
  the phone.
- Windows: a `.part` or `.mp4` that could not be deleted because ffmpeg still had it open is
  removed the next time a video is loaded, or at the next start.

### Loop a video sent from the phone (remote v2, phase 3)
_Added 2026-11-27, Video info / instant Stop-Play follow-up and the security-review fixes (steps 12–15) the same day · All · `remote-web/index.html`, `src/components/PhoneStream.tsx`, `src/hooks/use-phone-stream.ts`, `src/App.tsx`, `src-tauri/src/remote/stream/`_

> **Since 3b (2026-11-28) the phone page no longer shows this card** (see "Record a loop on
> the phone" above), and `remote-web/dev/smoke-file.mjs` was replaced by `smoke-record.mjs`.
> The phone-page steps below cannot be run from the page any more; the desktop side of a
> `mode: "file"` session is unchanged.

On the phone page the "Loop a video" card sends one video file straight to the computer
(WebRTC DataChannel, no upload to a server). The computer asks before taking it, shows the
transfer in a banner, then loads the video and loops it on the virtual camera exactly like a
file picked on the computer. The phone can then stop and restart that loop.

**Already automated, don't repeat by hand:** `node remote-web/dev/smoke-file.mjs` (needs
`playwright-core` ≥ 1.57, see `remote-web/dev/README.md`) drives the phone page against a fake
desktop: the `stream_request` with `mode: "file"`, a datachannel-only offer, the `meta`
message equal to the request, pieces of at most 64 KiB, at most ~4 MiB buffered, a
byte-identical 24 MB file on arrival, the percentage / "Preparing loop…" / "Looping on your
computer ✓" sequence, Stop loop / Play loop, the 300 MB limit, non-video files and a 0-byte
file refused on the phone, a dismissed picker changing nothing, a file whose type is empty
(as Android reports some `.mov` / `.mkv`) sent with the type of its extension, every rejection
reason, an error from the computer mid-transfer, Cancel, a video sent right after a camera
stream (the `busy` answer is retried), and no CSP violation.
`node remote-web/dev/update-csp.mjs --check` fails if the page's CSP hash is stale. The
receiving side is covered by `cargo test --lib`: storage (a private real directory, free
space, partial-file cleanup, the sweep), the size limit, the minimum transfer rate, the
content check (a playlist, noise or a WebM sent as MP4 is refused at its first bytes),
ffmpeg held to one demuxer and the `file` protocol for phone videos, the 4096×4096 / 10 min
limits, Cancel while the video is loading, a load that hangs, a video that cannot be read
leaving the earlier loop playing, and the phone's Stop / Play loop (once a second, never
turning the camera on, refused during a session). The smoke script also covers the phone
page ignoring a `loop_status` it did not ask for and not showing success on a bare
`stream_stop done`.
The two real halves together, without the app's window, are covered on Linux by the headless
CLI (see "Headless CLI: loop a video from the phone" under Linux). What is left for the steps
below is the app's own window and dialogs.

**Setup:** a build of this branch on the computer with a phone already linked (see "Phone
remote: linking with a code"); phone and computer on the same Wi-Fi; the phone page open in
Android Chrome (or the Android wrapper app). On the phone: one normal video of 20–100 MB
(MP4), and one larger than 300 MB. A video already looping in CamLooper, and a meeting app's
camera preview showing "CamLooper Virtual Camera".

**Steps:**
1. On the phone, scroll to **Loop a video** and tap **Choose a video**. Pick the file over
   300 MB.
2. Tap **Choose a different video** and pick the normal video. Tap **Send & loop**.
3. On the computer, read the dialog, then click **Decline**.
4. On the phone tap **Send & loop** again. On the computer click **Accept** (it is enabled
   after about a second).
5. Watch the computer's banner and the phone's card until the transfer ends.
6. On the computer, look at the main window and at the virtual camera.
7. On the phone tap **Stop loop**, wait two seconds, then tap **Play loop**.
8. Send the same video again and, at about 30 %, click **Cancel** in the computer's banner.
9. Send it again and, at about 30 %, tap **Cancel** on the phone.
10. On the phone tap **Start streaming**, accept on the computer, wait for "Live", tap
    **Stop**, and immediately send the video again (accept on the computer).
11. Reload the phone page.
12. Send the video once more and, as soon as the computer's banner says "Preparing loop…",
    click **Cancel** in the banner. (Use a long video with natural motion on, or a computer
    whose virtual camera is off so that a system prompt holds "Preparing loop…" open.)
13. On the computer, turn the virtual camera **off**. On the phone tap **Stop loop** if it
    is playing, then **Play loop**.
14. On the phone, send a file that is not a video but is named like one: copy any PDF or
    photo to `not-a-video.mp4` and pick it. Accept on the computer.
15. While a video picked **on the computer** is looping, send a truncated video from the
    phone (the first 200 KB of a real MP4 recorded by a phone camera:
    `head -c 200000 clip.mp4 > cut.mp4`; a "web optimised" MP4 keeps its index at the
    front and may still play — use a camera recording). Accept.

**Expected:**
1. The card shows the file's name and size and, in red, "This video is NNN MB. The largest
   video you can send is 300 MB. …". There is no **Send & loop** button and the computer
   shows nothing.
2. Name and size are shown; the phone says "Waiting for the computer to accept…" with a
   **Cancel** button, and **Start streaming** above is greyed out.
3. The dialog is titled "Loop a video from <phone name>?" and shows the file name and its
   size in MB (within 0.1 of what the phone shows). After Decline the phone says "The
   computer declined the video." and offers **Send & loop** again.
4. The dialog closes. The video that was already looping on the computer keeps playing, and
   the video and camera controls stay usable (they are not greyed out as they are for a
   phone camera stream).
5. Computer: a banner "Receiving video from <phone name> — NN %" with the file name, a
   filling bar and **Cancel**, then "Preparing loop…", then the banner disappears and a toast
   says "Now looping the video from <phone name>." Phone: a large percentage, a bar, and
   "X MB sent · Y MB confirmed · Z MB in total" (sent runs ahead of confirmed), then
   "Preparing loop…", then green "Looping on your computer ✓". The phone's screen does not
   dim or lock during the transfer.
6. The main window is on the Loop step with the phone's video playing (the preview moves).
   Advanced → Video info shows the phone's file name, its duration, **its resolution (for
   example `1280x720`), frame rate and format** — the same rows, with the same values, as
   when that file is picked on the computer with "Loop an existing video". The virtual
   camera shows the phone's video, looping.
7. The phone card shows "Looping on your computer now", the file name and **Stop loop**.
   After Stop loop: the label becomes "Loaded on your computer, not playing", the button
   becomes **Play loop**, and the computer's main window shows the video as stopped (the
   preview clears, the Play button is back) at the same moment the phone's label changes —
   no visible delay. After Play loop both go back, and the computer's preview is moving again.
8. The computer's banner disappears; the phone says "The transfer was cancelled on the
   computer." The loop from step 6 is still the one loaded.
9. The phone says "Cancelled." and shows the picker again; the computer's banner disappears
   and a toast says the phone cancelled the video.
10. The phone shows "Stopping the camera stream…" for a moment, then the normal transfer,
    and ends with "Looping on your computer ✓" — never "The computer is busy…".
11. After the reload the card again shows "Looping on your computer now" with the file name
    and **Stop loop**, without sending anything.

12. The banner disappears within a second or two. One of two things, and the computer's
    window, the virtual camera and the phone always agree on which: either the earlier video
    is still the one loaded and playing and the phone says "The transfer was cancelled on
    the computer." (the cancel came in time), or the phone's video plays, the main window
    shows it as loaded and playing, and the phone's card says "Looping on your computer
    now" with its name (the cancel came too late to prevent the load). Never the phone's
    video on the camera with the old one shown in the window.
13. **Stop loop** works as before. **Play loop** shows, in red on the phone, "The virtual
    camera is off on your computer. Turn it on in CamLooper, then try again." The button
    stays **Play loop**, the computer shows no system prompt (no polkit / UAC dialog) and
    its virtual camera stays off. After turning the camera on in CamLooper, **Play loop**
    works. Tapping Stop / Play faster than once a second shows "That was too fast. Wait a
    second and try again."
14. The computer's banner appears and goes away at once; the phone shows "The computer
    couldn't use this video: This file is not a video the computer can loop." The video
    that was looping keeps looping, and `phone-videos` holds no new file.
15. The phone shows "The computer couldn't use this video: The video could not be played on
    the computer." The computer's own video **keeps playing without a gap** — it is not
    stopped first — and the main window still shows it.

**Known gotchas:**
- On Windows the resolution of a video the loader cannot parse (`.webm`, `.mkv`) is an
  estimate from the file size, exactly as it is for the same file picked on the computer.
- With natural motion on, "Preparing loop…" can last many seconds for a long video while it
  is decoded; that is not a hang.
- If the phone page goes to the background (switching apps, locking the screen), Android may
  pause the transfer; the computer gives up after 20 s without data and the phone then shows
  "The connection to your computer was lost before the video arrived."
- A computer running an older CamLooper treats the request as a camera stream: its dialog
  says "Use your phone as the camera?" and the phone ends with "Update CamLooper on your
  computer…". Update the computer.
- If the virtual camera was off when a video arrives that you accepted, the computer turns
  it on by itself, as pressing Play does. On Linux without the v4l2loopback module loaded
  that is one polkit prompt; on the Windows **Store** build before the driver was ever
  registered it is one UAC prompt (the direct-download build registered it at install time
  and asks nothing). "Preparing loop…" stays up until the prompt is answered (at most two
  minutes); declining it ends in "The video could not be played on the computer.", with the
  earlier video still loaded and playing. The phone's **Play loop** never does this: it
  only starts the loop on a camera that is already on (step 13).
- A video is refused, after the transfer, if it is larger than 4096 pixels in either
  direction or longer than 10 minutes ("The video could not be played on the computer.").
- A transfer slower than 64 KB in 20 seconds ends with "The transfer was too slow."
- With less free disk space than the video's size plus 64 MB, the phone shows "There is not
  enough free disk space on the computer for this video." right after Accept.
- During the transfer the tray tooltip says the phone is connecting, never "live", and its
  **Stop phone camera** item cancels the transfer.
- The received file is kept only while it is the loaded video. To check: during a transfer
  the folder holds one `phone-<16 hex>.part`; while it loops, exactly one
  `phone-<16 hex>.<ext>` (never the phone's file name; the extension is the kind of video
  the content is); after a cancel or a failure, after loading a video on the computer, and
  after quitting CamLooper, it is empty. On Linux and macOS the folder is `drwx------`
  (`ls -ld`). Linux: `ls ~/.local/share/com.camlooper.app/phone-videos/`;
  Windows: `dir "%APPDATA%\com.camlooper.app\phone-videos"`. On Windows a file that was
  still playing at quit can survive until the next start, which removes it.

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

### Headless CLI: record a loop from the phone (remote v2, phase 3b)
_Added 2026-11-28, exit on SIGINT / SIGTERM and the size check added the same day · Linux · `src-tauri/examples/phone_stream_cli.rs`, `src-tauri/src/remote/stream/record.rs`_

`phone_stream_cli` accepts the phone page's "Record a loop" by itself and runs the app's own
path: live to `--device`, the take teed into its private temp folder, remuxed, and looped by
the real loop engine — which keeps looping after the phone has gone. Dev tool; nothing here
ships.

**Setup:** as in the file-mode entry below (`/dev/video10`, `ffmpeg`, `ffplay`, the phone page
showing a 6-digit code, `cargo build --example phone_stream_cli` in `src-tauri/`).

**Steps:**
1. `./target/debug/examples/phone_stream_cli --code <6 digits> --device /dev/video10 --seconds 120 2>/tmp/cli.err | tee /tmp/cli.jsonl`
2. `ffplay -f v4l2 /dev/video10` in a second terminal.
3. On the phone tap **Record a loop**, film for about 8 seconds, tap **Stop**.
4. Close the phone page (or turn its Wi-Fi off). Keep watching ffplay for 20 seconds.
5. `ls /tmp/camlooper-cli-*/phone-videos/`, then
   `ffprobe -v error -select_streams v:0 -show_entries stream=width,height:format=duration -of csv=p=0 /tmp/camlooper-cli-*/phone-videos/phone-*.mp4`
6. Press Ctrl-C in the first terminal, then `ls -d /tmp/camlooper-cli-* 2>/dev/null; grep -vc '^{' /tmp/cli.jsonl`.
7. Run steps 1 and 3 again, and when the `loop` lines are coming, from another terminal:
   `pkill -TERM -f examples/phone_stream_cli; sleep 2; pgrep -f examples/phone_stream_cli; tail -3 /tmp/cli.jsonl`

**Expected:**
- Step 1: `{"event":"connected","protocol":2,"appProtocol":3}`; the phone's Record a loop
  button becomes tappable (a `send` of `hello_ok` is logged).
- Step 3: `{"event":"stream_request",…,"mode":"record","autoAccept":true}`, state lines up to
  `live` with `"mode":"record"`, per-second `status` lines, then
  `{"event":"recording","elapsedMs":…,"capMs":60000}` about a second into the take. ffplay
  shows the phone's camera. After Stop: `{"event":"processing","elapsedMs":<about 7500>}`,
  `{"event":"file_saved","path":"/tmp/camlooper-cli-…/phone-videos/phone-<16 hex>.mp4",…}`,
  `{"event":"video_loaded","video":{…,"filename":"Phone recording.mp4",…},"playing":true}`,
  `{"event":"looping","name":"Phone recording.mp4"}`, a `send` of `stream_stop(done)`,
  `{"event":"loop_status","playing":true,"fromPhone":true,"name":"Phone recording.mp4",…}`,
  and from then on one `{"event":"loop","playing":true,…}` line per second. ffplay shows the
  take, moving.
- Step 4: the `loop` lines and the picture in ffplay go on unchanged; the CLI does not exit.
- Step 5: exactly one file, `phone-<16 hex>.mp4`; `ffprobe` prints the size of the last
  `status` lines of the take (`1280,720` from a phone on a normal network — not `320,180`)
  and a duration within half a second of the `elapsedMs` of the `processing` line.
- Step 6: `{"event":"signal","signal":"SIGINT"}`, then within a second
  `{"event":"cleanup","deleted":1,"dirRemoved":true,"loopStopped":true,…}`,
  `unpaired`, exit status 0; the `ls` prints nothing and the `grep` prints `0`.
- Step 7: `pgrep` prints nothing; the last lines are `{"event":"signal","signal":"SIGTERM"}`,
  `cleanup` and `unpaired`.

**Known gotchas:**
- `recording` comes one to two seconds after `live` (the take waits for the picture size to
  settle), and `"elapsedMs"` counts from there: an 8-second film gives about 7 s.
- A harness that stops reading the CLI's stdout stalls the CLI on its next JSON line until
  it reads again (it is a pipe). A signal sent meanwhile is acted on as soon as the CLI
  runs again; before this fix it was lost, and the CLI then never exited.
- Tapping Stop less than 2 seconds into the take prints no `processing`; the phone is sent
  `stream_stop(too_short)`, the temp folder is empty again, and the CLI keeps running until
  `--seconds` (as after a file session), so the phone can try again.
- If `--seconds` runs out during a take, the take is finished and looped first, then the CLI
  cleans up and exits.
- A phone that sends VP8 gives `phone-<16 hex>.webm` and "Phone recording.webm".

### Headless CLI: loop a video from the phone (remote v2, phase 3)
_Added 2026-11-27 · Linux · `src-tauri/examples/phone_stream_cli.rs`, `src-tauri/src/remote/stream/loop_engine.rs`, `src-tauri/src/virtual_camera.rs`_

`phone_stream_cli` now also takes the phone page's "Loop a video": it receives the file with
the app's own code into a private temp folder and loops it with the app's own loop engine
(natural motion on, loop forever) on the device given with `--device`. This is the dev tool
behind the Linux end-to-end check; nothing here ships.

**Setup:**
- A v4l2loopback device, e.g. `/dev/video10` as in the phase 1 entry below; `ffmpeg`, `ffplay`.
- The phone page open (a phone on the same Wi-Fi, or desktop Chrome on
  `node remote-web/dev/serve-phone.mjs`), showing a 6-digit code. One MP4 of a few MB on it;
  note its checksum on the computer if you have the same file there: `sha256sum clip.mp4`.
- In `src-tauri/`: `cargo build --example phone_stream_cli`.

**Steps:**
1. `./target/debug/examples/phone_stream_cli --code <6 digits> --device /dev/video10 --seconds 150 2>/tmp/cli.err | tee /tmp/cli.jsonl`
2. On the phone: **Start streaming**, wait for "Live", tap **Stop**.
3. Straight away (within 10 s): **Choose a video**, pick the MP4, **Send & loop**.
4. `ffplay -f v4l2 /dev/video10` for longer than the clip lasts.
5. On the phone tap **Stop loop**, wait two seconds, tap **Play loop**.
6. While it loops: `ls /tmp/camlooper-cli-*/phone-videos/`.
7. Press Ctrl-C in the first terminal. Then `ls -d /tmp/camlooper-cli-* 2>/dev/null; grep -vc '^{' /tmp/cli.jsonl`.
8. Run step 1 again with a new code, do step 2 only, and wait.

**Expected:**
- Step 2: as in the phase 1 entry (state `live`, then `idle`), followed by
  `{"event":"live_ended","exitInSeconds":10}`. The CLI does not exit yet.
- Step 3: `{"event":"stream_request",…,"mode":"file","file":{"mime":"video/mp4","name":"<your file>","size":<bytes>},"autoAccept":true}`,
  then `file_progress` lines from `"received":0` up to `"received"` = `"size"`, then
  `{"event":"file_saved","path":"/tmp/camlooper-cli-…/phone-videos/phone-<16 hex>.mp4","sha256":"…","size":…}`
  — the `sha256` equals `sha256sum` of the original — then
  `{"event":"video_loaded","video":{"width":…,"height":…,"fps":…,"duration":…,"filename":"<your file>",…},"playing":true}`,
  `{"event":"looping","name":"<your file>"}`, a `send` of `stream_stop(done)` and
  `{"event":"loop_status","playing":true,"fromPhone":true,"name":"<your file>",…}`. From then
  on one `{"event":"loop","playing":true,…}` line per second. The phone ends on
  "Looping on your computer ✓".
- Step 4: the phone's video plays, moving, and keeps moving past the clip's length (natural
  motion wanders through the clip; there is no freeze at the end).
- Step 5: `{"event":"loop_changed","playing":false}` and `{"event":"loop_status","playing":false,…}`,
  the picture in ffplay stops moving; then `loop_changed` / `loop_status` with `"playing":true`
  and ffplay moves again.
- Step 6: exactly one file, `phone-<16 hex>.mp4`.
- Step 7: `{"event":"cleanup","deleted":1,"dirRemoved":true,"loopStopped":true,…}`, then
  `{"event":"unpaired","revoked":true}`, exit status 0. The `ls` prints nothing (the folder is
  gone) and the `grep` prints `0`: stdout holds JSON lines only.
- Step 8: `live_ended`, and ten seconds later `cleanup` with `"deleted":0`, `unpaired`, exit 0.

**Known gotchas:**
- stderr (`/tmp/cli.err`) also carries what the app prints on stdout ("Virtual camera
  started…", "Natural motion: playing from N frames…"); that is deliberate, to keep stdout
  machine-readable.
- `--device` may be any v4l2loopback device, whatever its label. Without `--device` and
  `--null` the CLI uses the app's own virtual camera (one polkit prompt if the module is not
  loaded).
- If another program (the CamLooper app itself) is writing to the same device, the picture
  is a mix of both. Use a device of its own for the CLI.
- A live stream started while the video loops pauses the loop and resumes it when the stream
  ends; the CLI does not exit after such a stream.


### Phone camera stream into v4l2: confirm dialog, live banner, stop, tray, headless CLI (remote v2, phase 1)
_Added 2026-11-24, dialog safety steps (15–21) and steps 22–24 added the same day · Linux · `src-tauri/src/remote/stream/`, `src-tauri/src/tray.rs`, `src-tauri/examples/phone_stream_cli.rs`, `src/components/PhoneStream.tsx`, `src/hooks/use-phone-stream.ts`_

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
and bidi-override characters removed. Also: the tray being updated without its lock held (and
a language change during the update being applied), a relink while the relay socket is down
being noticed, a session released before its camera handoff began not claiming the camera
afterwards, the status carrying `releasing` (and counting as busy) after a give-up, and a
pending request ending when the wall clock passes its `expiresAt`.
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
22. Dropdown open. Open the language picker at the top of the window (the list is showing)
    and leave it open. Tap **Start streaming** on the phone. Click **Decline** with the
    mouse. Repeat with the output resolution picker under **Advanced**, and answer with
    **Tab** and **Enter** on **Decline** instead of the mouse.
23. Escape after a failed accept. Repeat step 13 up to the first click on **Accept** (computer
    offline, toast shown). Without clicking anything else, press **Tab** twice, then **Esc**.
24. Suspend. Tap **Start streaming**; with the dialog up, suspend the computer
    (`systemctl suspend`), wait at least 70 s, and wake it. Run the app from a terminal so
    its stderr is visible, and note the time of the wake.

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
- Step 22: the dropdown list closes the moment the dialog appears, and stays closed after
  the dialog is gone (it does not pop open again). **Decline** reacts to the first click and
  to the keyboard; focus never sits in the closed list.
- Step 23: after the toast, **Tab** still moves between **Decline** and **Accept** and
  **Esc** closes the dialog (the phone shows "declined"). The keys are not dead.
- Step 24: after the wake the dialog is gone within a second or two, the taskbar entry
  stops flashing, and the phone shows that the request timed out within a few seconds of the
  wake — not up to a minute later. A new **Start streaming** right away brings a new dialog.
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
  itself. Until then the playback and camera controls stay grey with the "phone" note under
  them although the banner is the red error, and **Quit** takes up to 3 s: both intended.
  This needs a hung driver to see and is covered by `cargo test --lib` instead.
- Not reproducible by hand with the current phone page: a phone that sends a second request
  with the **same** session id (the page makes a new id each time), and a relink done while
  the relay socket is down with a stream still live. The first is a UI rule with no
  automated check (the dialog is keyed by session id plus `expiresAt`); if you have a test
  client that can reuse an id, decline request "X", wait 15 s, send "X" again: a fresh dialog
  must appear, with both buttons working.
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
  `{"event":"live_ended","exitInSeconds":10}` and, ten seconds later (the time the phone has
  to send a video instead, see the phase 3 CLI entry above), a `cleanup` line and
  `{"event":"unpaired","revoked":true}`; the CLI exits with status 0 and the phone page
  returns to its unlinked screen. stderr shows
  `phone stream: receiver closed (closed by the phone)` and `phone stream: ended (user)` or
  `ended (peer_stopped)` — not `ended (error): the connection was closed`.
- Step 7: within ~10–20 s the CLI logs `phone stream: ended (peer_offline)` or `(timeout)`,
  state goes to `idle` (or `error` if the relay had not yet reported the phone offline), and
  the CLI exits ten seconds later.
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
