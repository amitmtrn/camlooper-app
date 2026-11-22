#!/usr/bin/env bash
# Builds the direct-download macOS release: a notarised CamLooper .dmg that opens on any
# Mac (Apple Silicon, macOS 13+) without Gatekeeper warnings.
#
#   DEVELOPMENT_TEAM=<Team ID> \
#   APP_PROFILE=/path/to/CamLooper_Developer_ID.provisionprofile \
#   EXT_PROFILE=/path/to/CamLooper_Camera_Extension_Developer_ID.provisionprofile \
#   NOTARY_KEY_ID=<App Store Connect API key id> NOTARY_ISSUER=<issuer id> \
#   scripts/macos-dmg-build.sh
#
# Needs a "Developer ID Application" certificate (with its private key) in the login
# keychain, two *Developer ID* provisioning profiles (the app's must carry the System
# Extension capability), and the API key file at
# ~/.appstoreconnect/private_keys/AuthKey_<NOTARY_KEY_ID>.p8.
# Set SKIP_NOTARIZE=1 to stop after signing (the result then only opens on this Mac).
#
# Unlike the Mac App Store build this one is not sandboxed: Developer ID does not require
# it, and it keeps the camera permission simple for the bundled ffmpeg. It is signed with
# the hardened runtime, which notarisation requires. The Camera Extension is always
# sandboxed; macOS requires that of every CMIO extension.
set -euo pipefail

: "${DEVELOPMENT_TEAM:?Set DEVELOPMENT_TEAM to your 10-character Apple Team ID}"
: "${APP_PROFILE:?Set APP_PROFILE to the Developer ID profile for com.camlooper.app}"
: "${EXT_PROFILE:?Set EXT_PROFILE to the Developer ID profile for com.camlooper.app.CameraExtension}"
[ -f "$APP_PROFILE" ] || { echo "No such profile: $APP_PROFILE" >&2; exit 1; }
[ -f "$EXT_PROFILE" ] || { echo "No such profile: $EXT_PROFILE" >&2; exit 1; }
if [ "${SKIP_NOTARIZE:-}" != "1" ]; then
    : "${NOTARY_KEY_ID:?Set NOTARY_KEY_ID (or SKIP_NOTARIZE=1)}"
    : "${NOTARY_ISSUER:?Set NOTARY_ISSUER (or SKIP_NOTARIZE=1)}"
    NOTARY_KEY="$HOME/.appstoreconnect/private_keys/AuthKey_$NOTARY_KEY_ID.p8"
    [ -f "$NOTARY_KEY" ] || { echo "No API key at $NOTARY_KEY" >&2; exit 1; }
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXT_DIR="$ROOT/src-tauri/macos-extension"
EXT_ID="com.camlooper.app.CameraExtension"
OUT="$ROOT/src-tauri/target/dmg"

IDENTITY="$(security find-identity -v -p codesigning | sed -n "s/.*\"\\(Developer ID Application: .*($DEVELOPMENT_TEAM)\\)\".*/\\1/p" | head -1)"
[ -n "$IDENTITY" ] || { echo "No 'Developer ID Application' certificate for team $DEVELOPMENT_TEAM in the keychain." >&2; exit 1; }

check_profile() { # <file> <bundle id>
    local plist appid
    plist="$(security cms -D -i "$1" 2>/dev/null)"
    appid="$(echo "$plist" | plutil -extract Entitlements.com\\.apple\\.application-identifier raw - 2>/dev/null || true)"
    [ "$appid" = "$DEVELOPMENT_TEAM.$2" ] || { echo "$1 is for '$appid', expected '$DEVELOPMENT_TEAM.$2'." >&2; exit 1; }
    echo "$plist" | grep -q "<key>ProvisionsAllDevices</key>" || { echo "$1 is not a Developer ID profile." >&2; exit 1; }
}
check_profile "$APP_PROFILE" "com.camlooper.app"
check_profile "$EXT_PROFILE" "$EXT_ID"

echo "==> Building the extension"
(cd "$EXT_DIR" && xcodegen generate >/dev/null && xcodebuild \
    -project CameraExtension.xcodeproj -target CameraExtension -configuration Release \
    SYMROOT="$EXT_DIR/build-appstore" DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" \
    CODE_SIGNING_ALLOWED=NO build | grep -E "error|BUILD")
EXT_BUILT="$EXT_DIR/build-appstore/Release/$EXT_ID.systemextension"
[ -d "$EXT_BUILT" ] || { echo "Extension not built: $EXT_BUILT" >&2; exit 1; }

echo "==> Building the bundled FFmpeg (skipped if already built)"
"$ROOT/scripts/build-macos-ffmpeg.sh"

echo "==> Building the app"
(cd "$ROOT" && npm run tauri -- build --bundles app)
APP="$ROOT/src-tauri/target/release/bundle/macos/camlooper.app"
[ -d "$APP" ] || { echo "App bundle not found: $APP" >&2; exit 1; }

echo "==> Embedding and signing"
EXT="$APP/Contents/Library/SystemExtensions/$EXT_ID.systemextension"
rm -rf "$APP/Contents/Library/SystemExtensions"
mkdir -p "$APP/Contents/Library/SystemExtensions"
cp -R "$EXT_BUILT" "$EXT"
cp "$EXT_PROFILE" "$EXT/Contents/embedded.provisionprofile"
cp "$APP_PROFILE" "$APP/Contents/embedded.provisionprofile"
# Built with signing off, so $(TeamIdentifierPrefix) is empty in the extension's
# Info.plist; the Mach service name must carry the team prefix or the camera never appears.
/usr/libexec/PlistBuddy -c "Set :CMIOExtension:CMIOExtensionMachServiceName $DEVELOPMENT_TEAM.$EXT_ID" "$EXT/Contents/Info.plist"
# Browser-downloaded profiles carry com.apple.quarantine; do not ship it.
xattr -cr "$APP"

rm -rf "$OUT"; mkdir -p "$OUT"
ent() { printf '<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>\n%s\n</dict></plist>\n' "$2" > "$1"; }
T='<true/>'
ent "$OUT/ext.entitlements" "
<key>com.apple.application-identifier</key><string>$DEVELOPMENT_TEAM.$EXT_ID</string>
<key>com.apple.developer.team-identifier</key><string>$DEVELOPMENT_TEAM</string>
<key>com.apple.security.app-sandbox</key>$T
<key>com.apple.security.application-groups</key><array><string>$DEVELOPMENT_TEAM.com.camlooper.app</string></array>"
# Under the hardened runtime each process needs the camera entitlement itself: ffmpeg is
# the one that opens the camera.
ent "$OUT/helper.entitlements" "
<key>com.apple.security.device.camera</key>$T"
ent "$OUT/app.entitlements" "
<key>com.apple.application-identifier</key><string>$DEVELOPMENT_TEAM.com.camlooper.app</string>
<key>com.apple.developer.team-identifier</key><string>$DEVELOPMENT_TEAM</string>
<key>com.apple.developer.system-extension.install</key>$T
<key>com.apple.security.device.camera</key>$T"

# Inside-out, never --deep. --options runtime (hardened runtime) and a secure timestamp
# are both required by notarisation.
S=(--force --options runtime --timestamp --sign "$IDENTITY")
codesign "${S[@]}" "$APP"/Contents/Frameworks/*.dylib
codesign "${S[@]}" --entitlements "$OUT/helper.entitlements" "$APP/Contents/MacOS/ffmpeg"
codesign "${S[@]}" --entitlements "$OUT/ext.entitlements" "$EXT"
codesign "${S[@]}" --entitlements "$OUT/app.entitlements" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if otool -L "$APP/Contents/MacOS/camlooper" "$APP/Contents/MacOS/ffmpeg" "$APP"/Contents/Frameworks/*.dylib \
    | grep -E '/opt/homebrew|/usr/local'; then
    echo "The app links against a Homebrew library (above)." >&2; exit 1
fi
SERVICE="$(/usr/libexec/PlistBuddy -c 'Print :CMIOExtension:CMIOExtensionMachServiceName' "$EXT/Contents/Info.plist")"
[ "$SERVICE" = "$DEVELOPMENT_TEAM.$EXT_ID" ] || { echo "Extension Mach service name is '$SERVICE'." >&2; exit 1; }
if codesign -d --entitlements - --xml "$APP" "$EXT" 2>/dev/null | grep -q get-task-allow; then
    echo "A development entitlement (get-task-allow) is still present." >&2; exit 1
fi

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
DMG="$OUT/CamLooper-$VERSION.dmg"
echo "==> Building $DMG"
STAGE="$OUT/stage"; mkdir -p "$STAGE"
ditto "$APP" "$STAGE/CamLooper.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "CamLooper" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
rm -rf "$STAGE"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"

if [ "${SKIP_NOTARIZE:-}" = "1" ]; then
    echo "Built $DMG (not notarised: it will only open on this Mac)."
    exit 0
fi

echo "==> Notarising (Apple's automated scan; usually a few minutes)"
xcrun notarytool submit "$DMG" --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" --wait | tee "$OUT/notary.log"
grep -q "status: Accepted" "$OUT/notary.log" || {
    ID="$(sed -n 's/^ *id: //p' "$OUT/notary.log" | head -1)"
    echo "Notarisation was not accepted. Log:" >&2
    xcrun notarytool log "$ID" --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" >&2 || true
    exit 1
}
xcrun stapler staple "$DMG"
spctl -a -t open --context context:primary-signature -v "$DMG"
echo
echo "Built and notarised $DMG"
