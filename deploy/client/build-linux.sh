#!/usr/bin/env bash
# Builds vid on Linux (Fedora or Debian/Ubuntu) and packs it for sending.
#
#   deploy/client/build-linux.sh            # writes dist/vid-linux-<arch>-<commit>.tar.gz
#
# Run it on Linux: the app links the system's camera (V4L2), audio (ALSA) and
# graphics (Vulkan, X11/Wayland) libraries, so it cannot be cross-built from a
# Mac. It needs no screen or camera to *build* -- a Proxmox VM or LXC will do --
# but running it does.
#
# UNVERIFIED: the package list is the set this kind of app usually needs, plus
# what ticket 26's first Linux build (2026-09-15) recorded. This blitz version
# (v0.3.0-beta.4) has not been built on Linux yet. If a build step fails on a
# missing library, add its -dev package here.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"

# System packages need root; set SKIP_PACKAGES=1 once they are installed.
# Build headers only: Vulkan *drivers* are a runtime need, and asking for
# Fedora's mesa-vulkan-drivers conflicts with RPM Fusion's freeworld build, which
# provides the same thing.
if [ -z "${SKIP_PACKAGES:-}" ]; then
  if command -v dnf >/dev/null; then
    sudo dnf install -y \
      gcc gcc-c++ pkgconf-pkg-config cmake clang git curl \
      alsa-lib-devel systemd-devel libv4l-devel \
      libX11-devel libXcursor-devel libXrandr-devel libXi-devel libxkbcommon-devel \
      wayland-devel vulkan-loader-devel \
      openssl-devel
  elif command -v apt-get >/dev/null; then
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends \
      build-essential pkg-config cmake clang curl git \
      libasound2-dev libudev-dev libv4l-dev \
      libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxkbcommon-dev \
      libwayland-dev libvulkan-dev \
      libssl-dev
  fi
fi

if ! command -v cargo >/dev/null; then
  curl https://sh.rustup.rs -sSf | sh -s -- -y
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
# The dx version this repo is built with (CLAUDE.md).
command -v dx >/dev/null || cargo install dioxus-cli --version 0.7.10 --locked

cd "$root/app"
dx build --renderer native --release

# dx puts the Linux build under target/dx/vdr/release/linux/; find the binary
# and its assets folder rather than hard-coding a layout not yet seen here.
bin="$(find target/dx/vdr/release/linux -type f -name vdr -perm -u+x | head -1)"
[ -n "$bin" ] || { echo "no vdr binary under target/dx/vdr/release/linux" >&2; exit 1; }
dir="$(dirname "$bin")"

arch="$(uname -m)"
commit="$(git -C "$root" rev-parse --short HEAD)"
out="$root/dist/vid-linux-$arch-$commit.tar.gz"
mkdir -p "$root/dist"
cp "$root/deploy/client/JOINING.md" "$dir/JOINING.md"
tar -czf "$out" -C "$(dirname "$dir")" "$(basename "$dir")"
echo "$out"
echo "Recipients need ALSA, the Vulkan loader and a Vulkan driver, and X11 or Wayland."
