#!/usr/bin/env bash
# Downloads the Windows ffmpeg shared build (BtbN LGPL) and unpacks the exe +
# runtime DLLs into src-tauri/drivers/windows/ffmpeg/ so tauri build can bundle
# them as resources for the NSIS installer. Skips ffprobe.exe / ffplay.exe.
#
# Only needed when producing a Windows build (cargo-xwin cross-compile, or
# native Windows CI). Not needed on Linux/macOS.
set -euo pipefail

URL='https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl-shared.zip'
DEST="$(cd "$(dirname "$0")/.." && pwd)/src-tauri/drivers/windows/ffmpeg"

if [ -f "$DEST/ffmpeg.exe" ]; then
    echo "ffmpeg already present at $DEST/ffmpeg.exe — skipping download."
    exit 0
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "Downloading ffmpeg (~64 MB)..."
curl -L --fail -o "$TMP/ffmpeg-win.zip" "$URL"

mkdir -p "$DEST"

# Windows CI runners ship 7-Zip but not necessarily unzip, so support both.
if command -v unzip >/dev/null 2>&1; then
    unzip -j -o "$TMP/ffmpeg-win.zip" \
        'ffmpeg-master-latest-win64-lgpl-shared/bin/ffmpeg.exe' \
        'ffmpeg-master-latest-win64-lgpl-shared/bin/av*.dll' \
        'ffmpeg-master-latest-win64-lgpl-shared/bin/sw*.dll' \
        'ffmpeg-master-latest-win64-lgpl-shared/LICENSE.txt' \
        -d "$DEST"
elif command -v 7z >/dev/null 2>&1; then
    7z e -y -o"$DEST" "$TMP/ffmpeg-win.zip" \
        'ffmpeg-master-latest-win64-lgpl-shared/bin/ffmpeg.exe' \
        'ffmpeg-master-latest-win64-lgpl-shared/bin/av*.dll' \
        'ffmpeg-master-latest-win64-lgpl-shared/bin/sw*.dll' \
        'ffmpeg-master-latest-win64-lgpl-shared/LICENSE.txt'
else
    echo "Need either unzip or 7z to extract $TMP/ffmpeg-win.zip" >&2
    exit 1
fi

# tauri.windows.conf.json bundles these DLLs by exact soname. BtbN's "latest" build
# tracks FFmpeg master, so a major bump renames them (avcodec-63 -> avcodec-64) and
# `tauri build` then fails on a missing resource — update that file to match.
echo "Extracted to $DEST:"
ls "$DEST"
