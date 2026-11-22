#!/bin/sh
# Build, sign and install a development CamLooper.app with the Camera Extension embedded.
#
#   DEVELOPMENT_TEAM=<10-char Team ID> ./dev-install.sh
#
# Prereqs: Xcode signed in to that team (Xcode > Settings > Accounts), a paid Apple
# Developer Program membership, `brew install xcodegen`. See README.md in this directory.
#
# The result is development-signed: it runs on Macs registered to the team, not on
# customers' machines. Distribution needs Developer ID signing + notarisation (README).
set -e

: "${DEVELOPMENT_TEAM:?Set DEVELOPMENT_TEAM to your 10-character Apple Team ID}"
cd "$(dirname "$0")"
HERE="$PWD"
ROOT="$(cd ../.. && pwd)"
EXT_ID="com.camlooper.app.CameraExtension"
INSTALL_PATH="${INSTALL_PATH:-/Applications/camlooper.app}"

command -v xcodegen >/dev/null 2>&1 || { echo "Install xcodegen: brew install xcodegen"; exit 1; }

echo "==> Building and signing the extension (team $DEVELOPMENT_TEAM)"
xcodegen generate
# SigningHost is never shipped: it makes Xcode provision a profile for com.camlooper.app
# that carries the System Extension capability, which we reuse for the Tauri-built app.
# -allowProvisioningDeviceRegistration registers this Mac with the team if it is not
# already: a development profile only covers registered devices, and a team with none
# cannot generate one at all.
xcodebuild \
  -project CameraExtension.xcodeproj \
  -scheme DevInstall \
  -destination "platform=macOS,arch=$(uname -m)" \
  -configuration Release \
  SYMROOT="$HERE/build" \
  DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" \
  -allowProvisioningUpdates \
  -allowProvisioningDeviceRegistration \
  build

EXT="$HERE/build/Release/$EXT_ID.systemextension"
PROFILE="$HERE/build/Release/SigningHost.app/Contents/embedded.provisionprofile"
[ -d "$EXT" ] || { echo "Extension not built: $EXT"; exit 1; }
[ -f "$PROFILE" ] || { echo "No provisioning profile at $PROFILE"; exit 1; }

# Sign the app with the same certificate Xcode chose for the extension.
IDENTITY="$(codesign -dvv "$EXT" 2>&1 | sed -n 's/^Authority=\(Apple Development.*\)$/\1/p' | head -1)"
[ -n "$IDENTITY" ] || { echo "Could not read the signing identity from $EXT"; exit 1; }

echo "==> Building the bundled FFmpeg (skipped if already built)"
"$ROOT/scripts/build-macos-ffmpeg.sh"

echo "==> Building the app"
(cd "$ROOT" && npm run tauri -- build --bundles app)
APP="$ROOT/src-tauri/target/release/bundle/macos/camlooper.app"
[ -d "$APP" ] || { echo "App bundle not found: $APP"; exit 1; }

echo "==> Embedding the extension and signing the app"
rm -rf "$APP/Contents/Library/SystemExtensions"
mkdir -p "$APP/Contents/Library/SystemExtensions"
cp -R "$EXT" "$APP/Contents/Library/SystemExtensions/"
cp "$PROFILE" "$APP/Contents/embedded.provisionprofile"

# The app runs in the App Sandbox, as the Mac App Store requires:
#   device.camera   - capture from the real camera, and see CoreMediaIO devices at all
#                     (the virtual camera's sink stream included)
#   network.client  - the webview's outbound requests (ad banner)
# Recordings and uploads go to the container's temp directory, so no file entitlements.
# The bundled ffmpeg is a helper the app spawns: it must be sandboxed too and carry only
# "inherit", which makes it run inside the app's sandbox.
# No hardened runtime yet: that is only required for notarised Developer ID builds.
HELPER_ENTITLEMENTS="$HERE/build/helper.entitlements"
cat > "$HELPER_ENTITLEMENTS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.security.app-sandbox</key>
	<true/>
	<key>com.apple.security.inherit</key>
	<true/>
</dict>
</plist>
PLIST
ENTITLEMENTS="$HERE/build/app-dev.entitlements"
cat > "$ENTITLEMENTS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.application-identifier</key>
	<string>$DEVELOPMENT_TEAM.com.camlooper.app</string>
	<key>com.apple.developer.team-identifier</key>
	<string>$DEVELOPMENT_TEAM</string>
	<key>com.apple.developer.system-extension.install</key>
	<true/>
	<key>com.apple.security.app-sandbox</key>
	<true/>
	<key>com.apple.security.device.camera</key>
	<true/>
	<key>com.apple.security.network.client</key>
	<true/>
</dict>
</plist>
PLIST
# Inside-out: nested code first (the bundled FFmpeg), then the outer app. The extension is
# already signed by Xcode. Never --deep: it would re-sign the extension without its
# entitlements.
codesign --force --sign "$IDENTITY" "$APP"/Contents/Frameworks/*.dylib
codesign --force --sign "$IDENTITY" --entitlements "$HELPER_ENTITLEMENTS" "$APP/Contents/MacOS/ffmpeg"
codesign --force --sign "$IDENTITY" --entitlements "$ENTITLEMENTS" "$APP"

# The app must not depend on anything outside itself and macOS.
if otool -L "$APP/Contents/MacOS/camlooper" "$APP/Contents/MacOS/ffmpeg" "$APP"/Contents/Frameworks/*.dylib \
    | grep -E '/opt/homebrew|/usr/local'; then
    echo "The app still links against a Homebrew library (above)." >&2
    exit 1
fi
codesign --verify --strict --verbose=2 "$APP"

echo "==> Installing to $INSTALL_PATH"
# macOS only activates system extensions from an app in /Applications.
rm -rf "$INSTALL_PATH"
ditto "$APP" "$INSTALL_PATH"

echo
echo "Installed. Open $INSTALL_PATH, start the virtual camera, and approve the extension"
echo "under System Settings > General > Login Items & Extensions > Camera Extensions."
