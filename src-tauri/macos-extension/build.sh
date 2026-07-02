#!/bin/sh
# Build the CamLooper Camera Extension (Release) into ./build.
# Prereqs: Xcode, xcodegen (brew install xcodegen), and DEVELOPMENT_TEAM set to your Team ID.
set -e
cd "$(dirname "$0")"

command -v xcodegen >/dev/null 2>&1 || { echo "Install xcodegen: brew install xcodegen"; exit 1; }

xcodegen generate

xcodebuild \
  -project CameraExtension.xcodeproj \
  -scheme CameraExtension \
  -configuration Release \
  -derivedDataPath build \
  DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:-TEAMID}" \
  CODE_SIGN_IDENTITY="${CODE_SIGN_IDENTITY:-Developer ID Application}" \
  clean build

EXT="$(find build -name '*.systemextension' -maxdepth 8 2>/dev/null | head -1)"
echo "Built extension: ${EXT:-NOT FOUND}"
