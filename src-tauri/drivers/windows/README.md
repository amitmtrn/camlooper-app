# Windows virtual-camera driver (softcam)

The Windows build bundles [softcam](https://github.com/tshino/softcam) (MIT licensed),
a shared-memory DirectShow virtual camera. The app writes frames to it and consumer apps
(Zoom/Teams/OBS) read from the registered filter.

`softcam.dll` and `softcam-LICENSE.txt` are **not committed** — they are built from source
in CI (see `.github/workflows/build-cross-platform.yml`, "Build softcam driver" step) and
dropped into this folder before `tauri build`, then bundled via `tauri.windows.conf.json`
`bundle.resources` and registered by the NSIS hook (`src-tauri/windows/hooks.nsh`).

To build locally on Windows:

```pwsh
git clone --depth 1 --branch v1.8.1 https://github.com/tshino/softcam.git
msbuild softcam\softcam.sln /p:Configuration=Release /p:Platform=x64 /t:softcam
copy softcam\dist\bin\x64\softcam.dll  src-tauri\drivers\windows\softcam.dll
copy softcam\LICENSE                   src-tauri\drivers\windows\softcam-LICENSE.txt
```
