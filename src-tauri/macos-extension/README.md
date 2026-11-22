# macOS Virtual Camera — CMIO Camera Extension

This directory contains the macOS **CMIO Camera Extension** ("CamLooper Virtual Camera")
that makes the app's video appear as a camera in Zoom / Teams / Meet / OBS on macOS 13+.

> **Status:** the extension, the app-side bridge and the dev install script build on a Mac.
> The signed end-to-end path (activation, frames arriving in Zoom) has **not yet been run**:
> see `MANUAL_TEST.md` → macOS for the acceptance steps. Distribution signing
> (Developer ID + notarisation) is not set up.

## How the pieces fit

```
camlooper.app (Rust/Tauri)                       CameraExtension.systemextension (Swift)
  ffmpeg ─ BGRA 1280x720 ─► Rust                   one device, two streams
        src/macos_virtual_camera.rs                  sink   ◄── the app writes here
        macos/camera_bridge.m                        source ──► Zoom / Meet / Photo Booth
          │  CMSampleBuffer (IOSurface)                 ▲
          └── CoreMediaIO sink stream ──────────────────┘  sink frames are forwarded
   activates via OSSystemExtensionRequest                  to the source unchanged
```

- The transport is a **CoreMediaIO sink stream**, not XPC. CoreMediaIO already carries
  IOSurface-backed sample buffers across the sandbox, so the app needs no Mach service and
  no shared App Group.
- While the app is not feeding the sink, the source shows a dark placeholder with a slow
  purple bar, so the camera is always a valid device.
- The extension advertises **one format: 1280x720 BGRA at 30 fps**. The app renders to that
  on macOS whatever the Resolution setting says.
- Extension bundle id **must** nest under the app id: `com.camlooper.app.CameraExtension`.

## Files

| File | Purpose |
|------|---------|
| `project.yml` | [XcodeGen](https://github.com/yonaskolb/XcodeGen) spec. Reads `DEVELOPMENT_TEAM` from the environment. |
| `CameraExtension/CameraExtensionProvider.swift` | Provider, device, source + sink streams, idle placeholder. |
| `CameraExtension/Shared.swift` | Camera name, stable device/stream ids, geometry. |
| `CameraExtension/Info.plist` | `CMIOExtensionMachServiceName`, versions. |
| `CameraExtension/CameraExtension.entitlements` | App Sandbox + the team-prefixed App Group the Mach service name must live under. |
| `SigningHost/` | A stub app with bundle id `com.camlooper.app`. Never shipped; it exists so Xcode provisions a profile carrying the System Extension capability, which `dev-install.sh` reuses for the cargo-built app. |
| `dev-install.sh` | Build + sign the extension, build the Tauri app, embed, sign, install to `/Applications`. |
| `../macos/camera_bridge.m` | App side: activation request + CoreMediaIO sink writer. Compiled by `build.rs`. |
| `../src/macos_virtual_camera.rs` | Rust wrapper used by `virtual_camera.rs`. |

## Development build (your own Mac)

Prerequisites:

1. A paid Apple Developer Program team. Free personal teams cannot sign system extensions.
2. Xcode signed in to that team: **Xcode → Settings → Accounts**. If `xcodebuild` reports
   "Unable to log in with account …", the saved session has expired — sign in again there.
3. `brew install xcodegen pkgconf`.

```bash
cd src-tauri/macos-extension
DEVELOPMENT_TEAM=<10-char Team ID> ./dev-install.sh
```

The first run also compiles the bundled FFmpeg (`scripts/build-macos-ffmpeg.sh`, a few
minutes); later runs reuse it. The installed app contains its own `ffmpeg` and FFmpeg
libraries and needs nothing from Homebrew.

The first run registers the App IDs `com.camlooper.app` and
`com.camlooper.app.CameraExtension` on the team, registers this Mac as a development device
(a development profile only covers registered devices), and creates development profiles.

Then open `/Applications/camlooper.app`, start the virtual camera, and approve the extension
under **System Settings → General → Login Items & Extensions → Camera Extensions**. Start
the camera again; "CamLooper Virtual Camera" appears in camera pickers (restart Zoom if it
was already open).

Once the extension is installed, `npm run tauri:dev` can feed it too: an unbundled dev
binary cannot *install* the extension, but it can write to one that is already there.

Notes:

- **Updating the extension:** macOS only replaces an installed extension when its
  `CFBundleVersion` / `CFBundleShortVersionString` changes. Bump `CFBundleVersion` in
  `CameraExtension/Info.plist`, rerun `dev-install.sh`, and start the camera; the app
  resubmits the activation request when the bundled version differs from the one it last
  activated.
- **Removing it:** delete `/Applications/camlooper.app`; macOS removes the extension with
  the app. `systemextensionsctl list` shows what is installed.
- The dev build runs in the App Sandbox (camera + outbound network; the bundled ffmpeg
  inherits it). `scripts/macos-sandbox-test.sh` exercises the backend inside it. It is not
  signed with the hardened runtime, which only notarised Developer ID builds need.

Debugging:

```bash
systemextensionsctl list
log stream --predicate 'subsystem == "com.camlooper.app.CameraExtension"' --level debug
log stream --predicate 'subsystem == "com.apple.cmio"' --level debug
```

## Not done yet: distribution

To ship this to users the app and extension need **Developer ID Application** signing with
hardened runtime, Developer ID provisioning profiles for both App IDs, and notarisation.
The extension must sit at `camlooper.app/Contents/Library/SystemExtensions/` and be signed
**before** the outer app (inside-out, no `--deep`), so CI has to take over signing from
`tauri-action`:

1. Build and sign the extension (Developer ID + hardened runtime + its profile).
2. `tauri build` unsigned.
3. Copy the extension in, add the app's profile, `codesign` the outer app.
4. Build the `.dmg`, `xcrun notarytool submit`, `xcrun stapler staple`.

### Direct download (the shipping route)

`scripts/macos-dmg-build.sh` builds the notarised `.dmg` users download: Developer ID
signed, hardened runtime, not sandboxed, with ads as in every other build. Its header lists
what it needs. The Developer ID profiles are kept, untracked, in `profiles/`:

```bash
P=src-tauri/macos-extension/profiles
DEVELOPMENT_TEAM=<Team ID> \
APP_PROFILE=$PWD/$P/CamLooper_Developer_ID.provisionprofile \
EXT_PROFILE=$PWD/$P/CamLooper_Camera_Extension_Developer_ID.provisionprofile \
NOTARY_KEY_ID=<API key id> NOTARY_ISSUER=<issuer id> \
scripts/macos-dmg-build.sh           # -> src-tauri/target/dmg/CamLooper-<version>.dmg
```

Raise `CFBundleVersion` in `CameraExtension/Info.plist` whenever the extension changes:
macOS keeps the installed extension if the number is the same.

### Mac App Store (not used)

The App Store Connect record was removed on 2026-11-19; the store build would have had to
drop the ads. The script is kept in case that changes.

`scripts/macos-appstore-build.sh` builds the Mac App Store `.pkg` (its header lists the
certificates and profiles it needs). The two Mac App Store profiles are kept, untracked, in
`profiles/`:

```bash
P=src-tauri/macos-extension/profiles
DEVELOPMENT_TEAM=<Team ID> BUILD_NUMBER=<higher than the last upload> \
APP_PROFILE=$PWD/$P/CamLooper_Mac_App_Store.provisionprofile \
EXT_PROFILE=$PWD/$P/CamLooper_Camera_Extension_Mac_App_Store.provisionprofile \
scripts/macos-appstore-build.sh      # -> src-tauri/target/appstore/CamLooper-<version>-<build>.pkg
```

Validate, then upload, with an App Store Connect API key (App Manager role) saved as
`~/.appstoreconnect/private_keys/AuthKey_<KEY_ID>.p8`; the issuer ID is shown under
Users and Access > Integrations:

```bash
xcrun altool --validate-app -f <pkg> -t macos --apiKey <KEY_ID> --apiIssuer <ISSUER_ID>
xcrun altool --upload-app   -f <pkg> -t macos --apiKey <KEY_ID> --apiIssuer <ISSUER_ID>
```

Each upload needs a higher `BUILD_NUMBER` than the last one (build 2 was uploaded on 2026-11-19).

A store-signed app does not launch when copied to a Mac by hand; test it through
TestFlight. Use `dev-install.sh` for local runs. The Mac App Store additionally requires "Apple Distribution"
signing with Mac App Store profiles, and a signed `.pkg` uploaded through App Store Connect.
