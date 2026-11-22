#!/usr/bin/env bash
# Builds the FFmpeg that ships inside the macOS app: an LGPL shared build (no GPL or
# nonfree components, so it is acceptable on the Mac App Store) with libvpx for VP9
# recording. Output goes to src-tauri/drivers/macos/ffmpeg/{bin,lib,include}.
#
# macOS has no ready-made LGPL shared build to download (the Windows script uses BtbN's,
# which covers Windows and Linux only), so this compiles from source. Takes a few minutes.
#
# Prereqs: Xcode command line tools, pkg-config (`brew install pkgconf`).
# The result is linked against nothing outside the system and itself, and its libraries
# carry @rpath install names so they work from Contents/Frameworks.
set -euo pipefail

FFMPEG_VERSION=7.1.2      # ffmpeg-next 8.x builds against FFmpeg 7.1 / 8.0, not 9
LIBVPX_VERSION=1.15.2
MIN_MACOS=13.0

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/src-tauri/drivers/macos/ffmpeg"
WORK="$ROOT/src-tauri/target/macos-ffmpeg-build"
JOBS="$(sysctl -n hw.ncpu)"

if [ -x "$DEST/bin/ffmpeg" ] && [ "${1:-}" != "--force" ]; then
    echo "ffmpeg already built at $DEST/bin/ffmpeg — pass --force to rebuild."
    exit 0
fi

command -v pkg-config >/dev/null 2>&1 || { echo "Need pkg-config: brew install pkgconf" >&2; exit 1; }

rm -rf "$WORK" "$DEST"
mkdir -p "$WORK" "$DEST"
cd "$WORK"

echo "==> Downloading sources"
curl -L --fail -o ffmpeg.tar.xz "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
curl -L --fail -o libvpx.tar.gz "https://github.com/webmproject/libvpx/archive/refs/tags/v$LIBVPX_VERSION.tar.gz"
tar -xf ffmpeg.tar.xz
tar -xf libvpx.tar.gz

export MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS"
VPX="$WORK/vpx-install"

echo "==> Building libvpx $LIBVPX_VERSION (static, linked into libavcodec)"
(
    cd "libvpx-$LIBVPX_VERSION"
    ./configure --prefix="$VPX" \
        --disable-shared --enable-static --enable-pic \
        --disable-examples --disable-tools --disable-docs --disable-unit-tests \
        --enable-vp9-highbitdepth \
        --extra-cflags="-mmacosx-version-min=$MIN_MACOS"
    make -j"$JOBS"
    make install
)

echo "==> Building FFmpeg $FFMPEG_VERSION (LGPL, shared)"
(
    cd "ffmpeg-$FFMPEG_VERSION"
    # --disable-autodetect keeps Homebrew libraries (sdl, xcb, lzma, ...) out: anything not
    # named here is not linked, so the result depends only on macOS itself.
    # No --enable-gpl / --enable-nonfree: that is what keeps this LGPL.
    PKG_CONFIG_PATH="$VPX/lib/pkgconfig" ./configure \
        --prefix="$DEST" \
        --install-name-dir=@rpath \
        --enable-shared --disable-static \
        --disable-autodetect \
        --enable-libvpx \
        --enable-avfoundation --enable-videotoolbox --enable-audiotoolbox \
        --enable-zlib \
        --disable-ffplay --disable-ffprobe --disable-doc --disable-debug \
        --pkg-config-flags=--static \
        --extra-cflags="-mmacosx-version-min=$MIN_MACOS" \
        --extra-ldflags="-mmacosx-version-min=$MIN_MACOS" \
        --extra-ldexeflags="-Wl,-rpath,@executable_path/../Frameworks -Wl,-rpath,@executable_path/../lib"
    make -j"$JOBS"
    make install
    cp COPYING.LGPLv2.1 "$DEST/LICENSE-FFmpeg-LGPLv2.1.txt"
    cp "$WORK/libvpx-$LIBVPX_VERSION/LICENSE" "$DEST/LICENSE-libvpx.txt"
)
"$DEST/bin/ffmpeg" -hide_banner -buildconf > "$DEST/BUILDCONF.txt"

rm -rf "$DEST/share"

echo "==> Checking the result"
if "$DEST/bin/ffmpeg" -hide_banner -version | grep -qE -- '--enable-(gpl|nonfree)'; then
    echo "The build enabled GPL or nonfree components — refusing to keep it." >&2
    exit 1
fi
if otool -L "$DEST/bin/ffmpeg" "$DEST"/lib/*.dylib | grep -E '/opt/homebrew|/usr/local'; then
    echo "The build links against a non-system library (above)." >&2
    exit 1
fi
"$DEST/bin/ffmpeg" -hide_banner -version | head -1
echo "Built into $DEST"
