# macOS Virtual Camera — CMIO Camera Extension

This directory contains the macOS **CMIO Camera Extension** ("CamLooper Virtual Camera")
that makes the app's video appear as a camera in Zoom / Teams / OBS on macOS 13+.

> **Status: scaffold.** These sources were authored on Linux and have **not** been compiled
> or run. They are a concrete starting point to build, sign, and iterate on a real Mac with
> Xcode. Treat every stage below as "build → run → fix" on-device. The rest of the app
> (Windows/Linux) does not depend on anything in this folder.

## How the pieces fit

```
CamLooper.app (Rust/Tauri)                 CameraExtension.systemextension (Swift)
  produces JPEG frames                       CMIOExtensionProvider/Device/Stream
        │  decode → BGRA                        ▲   publishes "CamLooper Virtual Camera"
        │  wrap in IOSurface-backed             │   emits CMSampleBuffers to consumers
        │  CVPixelBuffer                        │
        └── XPC (Mach service, App Group) ──────┘   (Zoom/Teams/OBS read the device)
   activates via OSSystemExtensionRequest
```

- Extension bundle id **must** nest under the app id: `com.camlooper.app.CameraExtension`.
- Frames travel app→extension over **XPC** as **IOSurface** handles (zero-copy). Until that
  IPC is wired, the extension emits a **test pattern** so you can validate the CMIO plumbing
  first (Stage C1).

## Files

| File | Purpose |
|------|---------|
| `project.yml` | [XcodeGen](https://github.com/yonaskolb/XcodeGen) spec → `xcodegen generate` produces `CameraExtension.xcodeproj`. (Or create a "Camera Extension" target in Xcode and drop these sources in.) |
| `CameraExtension/main.swift` | Extension entry point (`CMIOExtensionProvider.startService`). |
| `CameraExtension/CameraExtensionProvider.swift` | Provider + Device + Stream; test-pattern generator + hook for real frames. |
| `CameraExtension/FrameReceiver.swift` | XPC listener that receives IOSurface frames from the app (Stage C2). |
| `CameraExtension/Info.plist` | `NSExtensionPointIdentifier = com.apple.cmio-dal-assistant`, bundle id, versions. |
| `CameraExtension/CameraExtension.entitlements` | app-sandbox + App Group. |
| `build.sh` | `xcodebuild` the extension (Release, Developer ID) for CI/local. |

Shared constants (bundle ids, App Group, Mach service name) live in
`CameraExtension/Shared.swift` — **edit `TEAMID` / ids there and in the plists/entitlements**.

## Stage C0 — Apple portal prerequisites (do first, one-time)

1. Apple Developer Program membership.
2. In the portal, create/confirm **App IDs**: `com.camlooper.app` and
   `com.camlooper.app.CameraExtension`.
3. Enable the **App Groups** capability on both App IDs; create App Group
   `group.com.camlooper.shared` and assign it to both.
4. Create **Developer ID Application** provisioning profiles for both App IDs (the extension
   needs the **System Extension** capability). Download them.
5. Note your **Team ID** — put it in `Shared.swift` (and anywhere `TEAMID` appears).

## Stage C1 — Extension shows a test pattern (real Mac, dev mode)

```bash
brew install xcodegen                 # once
cd src-tauri/macos-extension
xcodegen generate                     # -> CameraExtension.xcodeproj
# In Xcode: set your Team, select Developer ID signing, build the extension.
systemextensionsctl developer on      # allow unsigned/dev extensions (needs SIP dev mode)
```
Embed the built `.systemextension` in a host app (or the CamLooper `.app`, see Stage C4),
launch it, approve in **System Settings → Privacy & Security**, then open **Photo Booth** or
**QuickTime → New Movie Recording → camera picker** and confirm "CamLooper Virtual Camera"
shows the moving test pattern. Debug with:
```bash
systemextensionsctl list
log stream --predicate 'subsystem == "com.apple.cmio"' --level debug
```

## Stage C2 — Real frames over XPC (real Mac)

Wire `FrameReceiver.swift` (extension side) to the app. On the app side add
`src-tauri/src/macos_camera_bridge.rs` that: decodes JPEG → BGRA, fills a pooled
IOSurface-backed `CVPixelBuffer`, and sends the IOSurface handle over XPC to the extension's
Mach service (`Shared.machServiceName`). Replace the stub in
`src-tauri/src/virtual_camera.rs` (`start_platform_camera`, macOS, ~L520-545) to drive it.
Validate the app's real video reaches Photo Booth (check BGRA order, stride, orientation, fps).

## Stage C3 — Activation UX (real Mac)

Use `OSSystemExtensionRequest.activationRequest` (via the `objc2`/`objc2-system-extensions`
crate, or a tiny ObjC shim compiled with `cc` in `build.rs`) so the app installs/activates the
extension and prompts the user to approve it in System Settings. Surface status to the React UI.

## Stage C4 — Embed + sign + notarize in CI

The extension must live at `CamLooper.app/Contents/Library/SystemExtensions/` and be signed
**before** the outer app (signatures are inside-out). Because `tauri-action` signs the app in
one pass, CI must **take over macOS signing**:

1. `build.sh` → signed `CameraExtension.systemextension` (Developer ID + hardened runtime +
   its provisioning profile).
2. `tauri build` **unsigned** (omit `APPLE_SIGNING_IDENTITY`).
3. Copy the extension into `…/Contents/Library/SystemExtensions/`.
4. `codesign` the outer `.app` (extension already signed; do **not** use `--deep`).
5. Build the `.dmg`, `xcrun notarytool submit`, `xcrun stapler staple`.

Re-enable the `APPLE_*` secrets block in `.github/workflows/build-cross-platform.yml` and add
the two provisioning profiles as base64 secrets. See that file's macOS comments.

## Stage C5 — Clean-Mac acceptance

Install the notarized `.dmg` on a fresh Mac, approve the extension, confirm the camera works
in **Zoom, Teams, and OBS**.
