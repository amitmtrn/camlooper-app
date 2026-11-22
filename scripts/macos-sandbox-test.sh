#!/usr/bin/env bash
# Runs the backend's real-hardware tests inside the macOS App Sandbox.
#
# The Mac App Store build is sandboxed, and `cargo test` is not, so a plain test run says
# nothing about whether camera capture, the bundled ffmpeg helper, recording to the
# container's temp directory or the virtual camera feed survive the sandbox. This wraps the
# test binary in a throwaway .app with the same sandbox entitlements dev-install.sh gives
# the real app, and runs the ignored hardware tests in it.
#
# Needs: scripts/build-macos-ffmpeg.sh run once, a physical camera, camera permission for
# the terminal, and the CamLooper camera extension installed and approved.
# Turns the camera on for about 20 seconds; takes about 40 in all.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FF="$ROOT/src-tauri/drivers/macos/ffmpeg"
APP="$ROOT/src-tauri/target/SandboxTest.app"
[ -x "$FF/bin/ffmpeg" ] || { echo "Run scripts/build-macos-ffmpeg.sh first." >&2; exit 1; }

cd "$ROOT/src-tauri"
BIN="$(cargo test --lib --no-run 2>&1 | sed -n 's/.*(\(target[^)]*\)).*/\1/p')"
[ -n "$BIN" ] || { echo "Could not find the test binary." >&2; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks"
cp "$BIN" "$APP/Contents/MacOS/sandboxtest"
cp "$FF/bin/ffmpeg" "$APP/Contents/MacOS/ffmpeg"
cp "$FF"/lib/lib*.[0-9]*.dylib "$APP/Contents/Frameworks/"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.camlooper.app.sandboxtest</string>
<key>CFBundleExecutable</key><string>sandboxtest</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSCameraUsageDescription</key><string>Sandbox test of the CamLooper capture pipeline.</string>
</dict></plist>
PLIST

ENT="$(mktemp -d)"
trap 'rm -rf "$ENT"' EXIT
plist() { printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict>\n'; for k in "$@"; do printf '<key>%s</key><true/>\n' "$k"; done; printf '</dict></plist>\n'; }
# Keep these in step with dev-install.sh.
plist com.apple.security.app-sandbox com.apple.security.device.camera com.apple.security.network.client > "$ENT/app.plist"
plist com.apple.security.app-sandbox com.apple.security.inherit > "$ENT/helper.plist"

codesign --force --sign - "$APP"/Contents/Frameworks/*.dylib 2>/dev/null
codesign --force --sign - --entitlements "$ENT/helper.plist" "$APP/Contents/MacOS/ffmpeg" 2>/dev/null
codesign --force --sign - --entitlements "$ENT/app.plist" "$APP" 2>/dev/null

RUN="$APP/Contents/MacOS/sandboxtest"
"$RUN" --ignored --nocapture feeds_the_installed_extension 2>&1 | grep -E "^test |test result"
"$RUN" --ignored --nocapture a_recorded_clip_loops_into_the_virtual_camera 2>&1 | tr '\r' '\n' | grep -E "^(clip:|looping:|test result)"
OUT="$("$RUN" --ignored --nocapture live_preview_keeps_moving_while_recording 2>&1 | tr '\r' '\n')" || { echo "$OUT" | tail -30; exit 1; }
echo "$OUT" | grep -E "^(camera:|preview only|while recording|recording:|after recording|test result)"

# Proof the run really was sandboxed: the recording lands in the container, not $TMPDIR.
REC="$(echo "$OUT" | sed -n 's/^RECORDING_PATH=//p')"
case "$REC" in
    */Library/Containers/com.camlooper.app.sandboxtest/*) echo "sandboxed: yes"; rm -f "$REC" ;;
    *) echo "The test did not run in the sandbox (recording at $REC)." >&2; exit 1 ;;
esac
