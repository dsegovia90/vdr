#!/usr/bin/env bash
# Builds the macOS app and zips it for sending to someone.
#
#   deploy/client/package-macos.sh      # writes dist/vid-macos-arm64-<commit>.zip
#
# `dx build`, not `dx bundle`: `dx bundle` hands off to a bundler that writes
# its own minimal Info.plist and drops the camera and microphone usage keys,
# and macOS kills an app that lacks them on first use (CLAUDE.md).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root/app"

dx build --renderer native --release

app="target/dx/vdr/release/macos/Vdr.app"
plist="$app/Contents/Info.plist"
for key in NSCameraUsageDescription NSMicrophoneUsageDescription; do
  /usr/libexec/PlistBuddy -c "Print :$key" "$plist" >/dev/null \
    || { echo "missing $key in $plist -- refusing to package an app that would crash" >&2; exit 1; }
done

arch="$(lipo -archs "$app/Contents/MacOS/vdr" | tr ' ' '+')"
commit="$(git -C "$root" rev-parse --short HEAD)"
out="$root/dist/vid-macos-$arch-$commit.zip"
mkdir -p "$root/dist"
rm -f "$out"
# ditto keeps the bundle's structure and attributes intact; plain zip can not.
ditto -c -k --keepParent "$app" "$out"
cp "$root/deploy/client/JOINING.md" "$root/dist/JOINING.md"
echo "$out"
