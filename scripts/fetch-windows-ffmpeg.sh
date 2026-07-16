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
unzip -j -o "$TMP/ffmpeg-win.zip" \
    'ffmpeg-master-latest-win64-lgpl-shared/bin/ffmpeg.exe' \
    'ffmpeg-master-latest-win64-lgpl-shared/bin/av*.dll' \
    'ffmpeg-master-latest-win64-lgpl-shared/bin/sw*.dll' \
    'ffmpeg-master-latest-win64-lgpl-shared/LICENSE.txt' \
    -d "$DEST"

echo "Extracted to $DEST:"
du -sh "$DEST"
