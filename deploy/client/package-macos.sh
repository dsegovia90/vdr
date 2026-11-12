#!/usr/bin/env bash
# Builds the macOS app and zips it for sending to someone.
#
#   deploy/client/package-macos.sh      # writes dist/vdr-macos-<arch>-<commit>.zip
#
# `dx build`, not `dx bundle`: `dx bundle` hands off to a bundler that writes
# its own minimal Info.plist and drops the camera and microphone usage keys,
# and macOS kills an app that lacks them on first use.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root/app"

app="target/dx/vdr/release/macos/Vdr.app"

# **Universal: one app for Apple Silicon and Intel Macs.** Intel is a plain
# cross-compile here (same SDK, no extra toolchain beyond the rustup target),
# and each target keeps its own cargo output, so both binaries survive and
# `lipo` joins them. Intel first, so the bundle dx leaves behind is the native
# one, then the binary inside it is replaced with the joined one.
rustup target add x86_64-apple-darwin >/dev/null
dx build --renderer native --release --target x86_64-apple-darwin
dx build --renderer native --release --target aarch64-apple-darwin
lipo -create \
  target/aarch64-apple-darwin/desktop-release/vdr \
  target/x86_64-apple-darwin/desktop-release/vdr \
  -output "$app/Contents/MacOS/vdr"
# Licences inside the bundle, so they travel with the app on its own too.
# Before signing: the signature seals Contents/Resources.
for f in LICENSE-MIT LICENSE-APACHE THIRD_PARTY_LICENSES; do
  cp "$root/$f" "$app/Contents/Resources/$f"
done
# Joining drops the linker's ad-hoc signature, and Apple Silicon will not run
# an unsigned binary at all. Re-sign ad-hoc: the same as before, nothing more.
codesign --force --sign - "$app"
plist="$app/Contents/Info.plist"
for key in NSCameraUsageDescription NSMicrophoneUsageDescription NSScreenCaptureUsageDescription; do
  /usr/libexec/PlistBuddy -c "Print :$key" "$plist" >/dev/null \
    || { echo "missing $key in $plist -- refusing to package an app that would crash" >&2; exit 1; }
done

arch="$(lipo -archs "$app/Contents/MacOS/vdr" | tr ' ' '+')"
[ "$arch" = "x86_64+arm64" ] && arch=universal
# The commit names the package; VDR_COMMIT wins when set (see build-linux.sh).
commit="${VDR_COMMIT:-$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)}"
out="$root/dist/vdr-macos-$arch-$commit.zip"
mkdir -p "$root/dist"
rm -f "$out"
# The app and the guide together, so the one download is all anyone needs.
# ditto keeps the bundle's structure and attributes intact; plain zip can not.
stage="$(mktemp -d)/vdr"
mkdir -p "$stage"
ditto "$app" "$stage/Vdr.app"
cp "$root/deploy/client/JOINING.md" "$stage/JOINING.md"
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/THIRD_PARTY_LICENSES" "$stage/"
ditto -c -k --keepParent "$stage" "$out"
rm -rf "$(dirname "$stage")"
echo "$out"
