# Windows virtual-camera driver (softcam)

The Windows build bundles [softcam](https://github.com/tshino/softcam) (MIT licensed),
a shared-memory DirectShow virtual camera. The app writes frames to it and consumer apps
(Zoom/Teams/OBS) read from the registered filter.

`softcam.dll` and `softcam-LICENSE.txt` are committed here (built from v1.8.1 with
MSBuild x64 Release). Tauri bundles them via `tauri.windows.conf.json` `bundle.resources`
and the NSIS hook (`src-tauri/windows/hooks.nsh`) registers the DLL at install time.

To rebuild from source on Windows:

```pwsh
git clone --depth 1 --branch v1.8.1 https://github.com/tshino/softcam.git
msbuild softcam\softcam.sln /p:Configuration=Release /p:Platform=x64 /t:softcam
copy softcam\dist\bin\x64\softcam.dll  src-tauri\drivers\windows\softcam.dll
copy softcam\LICENSE                   src-tauri\drivers\windows\softcam-LICENSE.txt
```
