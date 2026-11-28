# CamLooper Remote — Android wrapper (debug, phase 1 testing)

A minimal native Android app (Java, one Activity, `androidx.webkit`) that wraps the phone page
`../remote-web/index.html` so Amit can test the phone remote as an installable app.

**How it reaches the live relay without a relay change:** the page is copied into the APK at
build time and served by `WebViewAssetLoader` under `https://camlooper.com/remote-app/`. Those
requests are answered inside the app (never fetched from the real site; any other camlooper.com
request is answered 404 locally), so the page's origin is `https://camlooper.com`, which relay v2
(`api.camlooper.com`) already allows for CORS and WebSocket upgrades. The relay API and WebRTC
media go to the network normally. Because the origin is stable, the pairing persists.

Camera/mic: `CAMERA` + `RECORD_AUDIO` runtime permissions are asked when the page calls
`getUserMedia`; `onPermissionRequest` grants only for the camlooper.com origin (video-only if the
mic is refused). The screen is kept on while the app is open. Debug builds enable WebView
debugging (`chrome://inspect`).

## Build
```
export ANDROID_HOME=~/Android/Sdk   # platform 35 + build-tools; JDK 17
cd android-remote && ./gradlew assembleDebug
# → app/build/outputs/apk/debug/app-debug.apk  (package com.camlooper.remote.debug)
adb install -r app/build/outputs/apk/debug/app-debug.apk
```
After editing `remote-web/index.html`'s script, run `node remote-web/dev/update-csp.mjs` (CSP hash)
and rebuild. Not for release: debug-signed, no store listing. Production plan remains
https://camlooper.com/remote (or a dedicated origin).

## Debug signing

Debug APKs are signed with a **stable project key**, not the machine-local
`~/.android/debug.keystore` (that key changes between the box and the XPS, and
`adb install -r` then fails with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`).

The keystore lives outside the repo, in one of:

1. `$CAMLOOPER_DEBUG_KEYSTORE_DIR`
2. `~/Projects/camlooper-remote-test/keystore`
3. `/workspace/camlooper/remote-v2/keystore`

Each of those directories must contain `debug.keystore` and `keystore.properties`
(`storePassword`, `keyPassword`, `keyAlias`). Do not commit either file.
`assembleDebug` fails with a short instruction if they are missing, rather than
silently signing with a new key.

After uninstalling an APK that was signed with an old key, install this one and
later updates with the same key can use `adb install -r` again.
