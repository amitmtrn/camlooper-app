# Third-party components

CamLooper itself is licensed under [PolyForm Noncommercial 1.0.0](LICENSE). The components
below are **not** — they keep their own licenses, which are more permissive than CamLooper's.
Nothing here restricts what you may do with those components; it exists so you know what is
inside the installers and how to comply when redistributing them.

## Bundled in the installers

| Component | Version | License | Where |
|---|---|---|---|
| [softcam](https://github.com/tshino/softcam) — DirectShow virtual camera filter | 1.8.1 | MIT | `src-tauri/drivers/windows/softcam.dll`, license text in `softcam-LICENSE.txt`. Windows only. CI rebuilds it from source; see `src-tauri/drivers/windows/README.md`. |
| [FFmpeg](https://ffmpeg.org/) (BtbN LGPL shared build) | master (LGPL configuration) | LGPL-3.0-or-later | `src-tauri/drivers/windows/ffmpeg/`, fetched by `scripts/fetch-windows-ffmpeg.sh`, license text in `ffmpeg/LICENSE.txt`. Windows only. |

### FFmpeg and the LGPL

FFmpeg is used as **separate, dynamically linked shared libraries** and a standalone
`ffmpeg.exe`, never statically linked into the app and never modified. That is what keeps
the LGPL obligation satisfiable: you can replace the bundled DLLs with your own build of the
same FFmpeg version and the app keeps working. The build used is BtbN's **LGPL** configuration
(no GPL-only components such as libx264), and its source is available from
<https://github.com/BtbN/FFmpeg-Builds>.

On **macOS** the app bundles FFmpeg 7.1 compiled from unmodified upstream source by
`scripts/build-macos-ffmpeg.sh`, in FFmpeg's default **LGPL-2.1-or-later** configuration
(no `--enable-gpl`, no `--enable-nonfree`; the script refuses a build that has either). It
is shipped the same way as on Windows: separate shared libraries in `Contents/Frameworks`
and a standalone `Contents/MacOS/ffmpeg`, so a user can replace them. The exact configure
line is shipped as `Contents/Resources/ffmpeg/BUILDCONF.txt` beside the license texts.
[libvpx](https://github.com/webmproject/libvpx) (BSD-3-Clause) is linked into `libavcodec`
for VP9 recording.

On **Linux** FFmpeg is not bundled at all — it is a system dependency declared by the
`.deb` and `.rpm`, so those installers ship no FFmpeg code.

## Build-time and runtime dependencies

Not redistributed as separate files, but compiled or bundled into the app:

| Component | License |
|---|---|
| [Tauri](https://tauri.app/) v2 (framework, plugins, CLI) | MIT OR Apache-2.0 |
| [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next) / `ffmpeg-sys-next` (Rust bindings, non-Windows) | WTFPL |
| [`windows`](https://crates.io/crates/windows) crate (Win32 DirectShow / Media Foundation) | MIT OR Apache-2.0 |
| [`v4l`](https://crates.io/crates/v4l) (Linux virtual camera) | MIT |
| Other Rust crates (tokio, serde, image, uuid, mp4, …) | MIT OR Apache-2.0 (see `src-tauri/Cargo.lock`) |
| [React](https://react.dev/), [Radix UI](https://www.radix-ui.com/), [shadcn/ui](https://ui.shadcn.com/), [Tailwind CSS](https://tailwindcss.com/), [Vite](https://vite.dev/) | MIT |
| Other npm packages | MIT / ISC / BSD (see `package-lock.json`) |

To regenerate this inventory:

```bash
cargo install cargo-license && (cd src-tauri && cargo license)
npx license-checker --summary
```

No GPL-licensed code is linked into CamLooper. If you add a dependency, check its license
first — a GPL dependency would be incompatible with distributing CamLooper under its own
license.
