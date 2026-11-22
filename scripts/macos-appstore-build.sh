#!/usr/bin/env bash
# Builds the Mac App Store package: camlooper.app with the Camera Extension embedded,
# sandboxed, signed for distribution, wrapped in a signed installer .pkg.
#
#   DEVELOPMENT_TEAM=<Team ID> \
#   APP_PROFILE=/path/to/CamLooper_Mac_App_Store.provisionprofile \
#   EXT_PROFILE=/path/to/CamLooper_Extension_Mac_App_Store.provisionprofile \
#   BUILD_NUMBER=<n> scripts/macos-appstore-build.sh
#
# BUILD_NUMBER becomes the app's CFBundleVersion. App Store Connect refuses an upload
# whose build number is not higher than every earlier upload, so raise it each time
# (a plain increasing integer: 2, 3, 4 ...). The marketing version stays tauri.conf.json's.
#
# Needs in the login keychain (create both in Xcode > Settings > Accounts > Manage
# Certificates so the private keys are on this Mac):
#   - "Apple Distribution"                 signs the app, the extension, ffmpeg
#   - "3rd Party Mac Developer Installer"  signs the .pkg ("Mac Installer Distribution")
# and two *Mac App Store* provisioning profiles from the developer portal, one for
# com.camlooper.app (with the System Extension capability) and one for
# com.camlooper.app.CameraExtension.
#
# It does not upload. Afterwards:
#   xcrun altool --validate-app -f <pkg> -t macos --apiKey ... --apiIssuer ...
#   xcrun altool --upload-app   -f <pkg> -t macos --apiKey ... --apiIssuer ...
# or drag the .pkg into the Transporter app.
#
# Apple Silicon only: the bundled FFmpeg is built for arm64. The store accepts that
# because the minimum macOS (13.0, tauri.macos.conf.json) is 12 or later.
set -euo pipefail

: "${DEVELOPMENT_TEAM:?Set DEVELOPMENT_TEAM to your 10-character Apple Team ID}"
: "${APP_PROFILE:?Set APP_PROFILE to the Mac App Store profile for com.camlooper.app}"
: "${EXT_PROFILE:?Set EXT_PROFILE to the Mac App Store profile for com.camlooper.app.CameraExtension}"
: "${BUILD_NUMBER:?Set BUILD_NUMBER to an integer higher than the last uploaded build}"
case "$BUILD_NUMBER" in *[!0-9]*|'') echo "BUILD_NUMBER must be an integer." >&2; exit 1 ;; esac
[ -f "$APP_PROFILE" ] || { echo "No such profile: $APP_PROFILE" >&2; exit 1; }
[ -f "$EXT_PROFILE" ] || { echo "No such profile: $EXT_PROFILE" >&2; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXT_DIR="$ROOT/src-tauri/macos-extension"
EXT_ID="com.camlooper.app.CameraExtension"
OUT="$ROOT/src-tauri/target/appstore"

identity() { security find-identity -v | sed -n "s/.*\"\\($1: .*($DEVELOPMENT_TEAM)\\)\".*/\\1/p" | head -1; }
APP_IDENTITY="$(identity 'Apple Distribution')"
PKG_IDENTITY="$(identity '3rd Party Mac Developer Installer')"
[ -n "$APP_IDENTITY" ] || { echo "No 'Apple Distribution' certificate with a private key for team $DEVELOPMENT_TEAM in the keychain." >&2; exit 1; }
[ -n "$PKG_IDENTITY" ] || { echo "No '3rd Party Mac Developer Installer' certificate for team $DEVELOPMENT_TEAM in the keychain." >&2; exit 1; }

# A profile must be an App Store one for the right App ID: no device list, no get-task-allow.
check_profile() { # <file> <bundle id>
    local plist; plist="$(security cms -D -i "$1" 2>/dev/null)"
    local appid; appid="$(echo "$plist" | plutil -extract Entitlements.com\\.apple\\.application-identifier raw - 2>/dev/null || true)"
    [ "$appid" = "$DEVELOPMENT_TEAM.$2" ] || { echo "$1 is for '$appid', expected '$DEVELOPMENT_TEAM.$2'." >&2; exit 1; }
    if echo "$plist" | plutil -extract ProvisionedDevices raw - >/dev/null 2>&1; then
        echo "$1 is a development profile (it lists devices); a Mac App Store profile is needed." >&2; exit 1
    fi
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
# The extension was built with signing off, which leaves $(TeamIdentifierPrefix) empty in
# its Info.plist. The Mach service name must carry the team prefix (it has to sit under
# the App Group), or the camera never appears.
/usr/libexec/PlistBuddy -c "Set :CMIOExtension:CMIOExtensionMachServiceName $DEVELOPMENT_TEAM.$EXT_ID" "$EXT/Contents/Info.plist"
cp "$APP_PROFILE" "$APP/Contents/embedded.provisionprofile"

/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NUMBER" "$APP/Contents/Info.plist"

# Files downloaded with a browser (the provisioning profiles) carry com.apple.quarantine,
# and copies inherit it. App Store Connect rejects a package containing it (ITMS-91109).
xattr -cr "$APP"

rm -rf "$OUT"; mkdir -p "$OUT"
ent() { # <file> <keys/values as raw plist body>
    printf '<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>\n%s\n</dict></plist>\n' "$2" > "$1"
}
T='<true/>'
ent "$OUT/ext.entitlements" "
<key>com.apple.application-identifier</key><string>$DEVELOPMENT_TEAM.$EXT_ID</string>
<key>com.apple.developer.team-identifier</key><string>$DEVELOPMENT_TEAM</string>
<key>com.apple.security.app-sandbox</key>$T
<key>com.apple.security.application-groups</key><array><string>$DEVELOPMENT_TEAM.com.camlooper.app</string></array>"
ent "$OUT/helper.entitlements" "
<key>com.apple.security.app-sandbox</key>$T
<key>com.apple.security.inherit</key>$T"
# Keep the sandbox keys in step with dev-install.sh and macos-sandbox-test.sh.
ent "$OUT/app.entitlements" "
<key>com.apple.application-identifier</key><string>$DEVELOPMENT_TEAM.com.camlooper.app</string>
<key>com.apple.developer.team-identifier</key><string>$DEVELOPMENT_TEAM</string>
<key>com.apple.developer.system-extension.install</key>$T
<key>com.apple.security.app-sandbox</key>$T
<key>com.apple.security.device.camera</key>$T
<key>com.apple.security.network.client</key>$T"

# Inside-out, never --deep.
codesign --force --sign "$APP_IDENTITY" "$APP"/Contents/Frameworks/*.dylib
codesign --force --sign "$APP_IDENTITY" --entitlements "$OUT/helper.entitlements" "$APP/Contents/MacOS/ffmpeg"
codesign --force --sign "$APP_IDENTITY" --entitlements "$OUT/ext.entitlements" "$EXT"
codesign --force --sign "$APP_IDENTITY" --entitlements "$OUT/app.entitlements" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if otool -L "$APP/Contents/MacOS/camlooper" "$APP/Contents/MacOS/ffmpeg" "$APP"/Contents/Frameworks/*.dylib \
    | grep -E '/opt/homebrew|/usr/local'; then
    echo "The app links against a Homebrew library (above)." >&2; exit 1
fi
if xattr -lr "$APP" | grep -q com.apple.quarantine; then
    echo "A file in the app still carries com.apple.quarantine." >&2; exit 1
fi
SERVICE="$(/usr/libexec/PlistBuddy -c 'Print :CMIOExtension:CMIOExtensionMachServiceName' "$EXT/Contents/Info.plist")"
[ "$SERVICE" = "$DEVELOPMENT_TEAM.$EXT_ID" ] || { echo "Extension Mach service name is '$SERVICE'." >&2; exit 1; }
if codesign -d --entitlements - --xml "$APP" "$EXT" 2>/dev/null | grep -q get-task-allow; then
    echo "A development entitlement (get-task-allow) is still present." >&2; exit 1
fi

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
PKG="$OUT/CamLooper-$VERSION-$BUILD_NUMBER.pkg"
echo "==> Building $PKG"
productbuild --component "$APP" /Applications --sign "$PKG_IDENTITY" "$PKG"
pkgutil --check-signature "$PKG" | head -3
echo
echo "Built $PKG — validate and upload it with altool or Transporter (see the header)."
