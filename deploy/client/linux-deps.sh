#!/usr/bin/env bash
# Installs the system packages vdr needs to build on Linux (Fedora or
# Debian/Ubuntu). build-linux.sh runs it, and so does CI, so the list lives in
# one place.
#
#   deploy/client/linux-deps.sh
#
# PipeWire is for the `wayland-share` feature (Wayland screen sharing). Fedora's
# is new enough; Debian/Ubuntu get meson and ninja so build-linux.sh can build
# it. Build headers only: Vulkan *drivers* are a runtime need, and asking for
# Fedora's mesa-vulkan-drivers conflicts with RPM Fusion's freeworld build, which
# provides the same thing.
set -euo pipefail

# Root already (as in a container) needs no sudo, and may not have it.
sudo=""
[ "$(id -u)" -eq 0 ] || sudo="sudo"

if command -v dnf >/dev/null; then
  $sudo dnf install -y \
    gcc gcc-c++ pkgconf-pkg-config cmake clang git curl \
    alsa-lib-devel systemd-devel libv4l-devel fontconfig-devel \
    libX11-devel libXcursor-devel libXrandr-devel libXi-devel libxkbcommon-devel \
    wayland-devel vulkan-loader-devel pipewire-devel \
    openssl-devel
elif command -v apt-get >/dev/null; then
  $sudo apt-get update
  $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential pkg-config cmake clang curl git \
    libasound2-dev libudev-dev libv4l-dev libfontconfig1-dev \
    libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxkbcommon-dev \
    libwayland-dev libvulkan-dev meson ninja-build \
    libssl-dev
else
  echo "no dnf or apt-get: install the equivalents of the packages in $0 by hand" >&2
fi
