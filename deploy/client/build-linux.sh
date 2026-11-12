#!/usr/bin/env bash
# Builds vid on Linux (Fedora or Debian/Ubuntu) and packs it for sending.
#
#   deploy/client/build-linux.sh            # writes dist/vid-linux-<arch>-<commit>.tar.gz
#
# Run it on Linux: the app links the system's camera (V4L2), audio (ALSA) and
# graphics (Vulkan, X11/Wayland) libraries, so it cannot be cross-built from a
# Mac. It needs no screen or camera to *build* -- a VM or container will do --
# but running it does.
#
# UNVERIFIED: the package list is the set this kind of app usually needs, plus
# what the first Linux build (2026-09-15) recorded. This blitz version
# (v0.3.0-beta.4) has not been built on Linux yet. If a build step fails on a
# missing library, add its -dev package here. First Fedora build (2026-11-05):
# fontconfig was the only one missing -- blitz uses it to find system fonts.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"

# Root already (as in a container) needs no sudo, and may not have it.
sudo=""
[ "$(id -u)" -eq 0 ] || sudo="sudo"

# System packages need root; set SKIP_PACKAGES=1 once they are installed.
# PipeWire is for the `wayland-share` feature (Wayland screen sharing). Fedora's
# is new enough; Debian/Ubuntu get meson and ninja to build it, see below.
# Build headers only: Vulkan *drivers* are a runtime need, and asking for
# Fedora's mesa-vulkan-drivers conflicts with RPM Fusion's freeworld build, which
# provides the same thing.
if [ -z "${SKIP_PACKAGES:-}" ]; then
  if command -v dnf >/dev/null; then
    $sudo dnf install -y \
      gcc gcc-c++ pkgconf-pkg-config cmake clang git curl \
      alsa-lib-devel systemd-devel libv4l-devel fontconfig-devel \
      libX11-devel libXcursor-devel libXrandr-devel libXi-devel libxkbcommon-devel \
      wayland-devel vulkan-loader-devel pipewire-devel \
      openssl-devel
  elif command -v apt-get >/dev/null; then
    $sudo apt-get update
    DEBIAN_FRONTEND=noninteractive $sudo apt-get install -y --no-install-recommends \
      build-essential pkg-config cmake clang curl git \
      libasound2-dev libudev-dev libv4l-dev libfontconfig1-dev \
      libx11-dev libxcursor-dev libxrandr-dev libxi-dev libxkbcommon-dev \
      libwayland-dev libvulkan-dev meson ninja-build \
      libssl-dev
  fi
fi

# Whether cargo *runs*, not whether it exists: rustup's launcher can be present
# with no toolchain behind it (a cache that kept ~/.cargo but not ~/.rustup).
if ! cargo --version >/dev/null 2>&1; then
  if command -v rustup >/dev/null; then
    rustup default stable
  else
    curl https://sh.rustup.rs -sSf | sh -s -- -y
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
fi
# The dx version this repo is built with (see app/README.md).
command -v dx >/dev/null || cargo install dioxus-cli --version 0.7.10 --locked

# PipeWire headers for `wayland-share`. The Rust bindings (pipewire/libspa 0.10)
# need 0.3.65 or newer: `spa_video_info_raw.flags`, its `modifier` as uint64_t,
# and `spa_meta_first`/`spa_meta_region_is_valid` as inline functions. Ubuntu
# 22.04, the portable build's base for its glibc 2.35, ships 0.3.48. So build a
# newer PipeWire from source for headers and a library to link against, and
# change nothing at runtime: PipeWire keeps the `libpipewire-0.3.so.0` soname
# and its ABI, so the binary loads whatever the recipient's distro has. The
# prefix is only ever on PKG_CONFIG_PATH, never an rpath.
#
# 1.0.9: the newest of the oldest 1.x line (0.3.65 would also satisfy the
# headers), and meson 0.61, which 22.04 has, is enough to build it. Everything
# optional is off: no daemon modules, plugins, session manager, docs or tests.
pw_version=1.0.9
pw_sha256=a295b3856bc7f55f70be5273f6bad4410cc7893f9ae376f0c04c64357ff824ce
if ! pkg-config --atleast-version=0.3.65 libpipewire-0.3 2>/dev/null; then
  # Under app/target: a cached volume in the container, gitignored on a host.
  pw_prefix="$root/app/target/deps/pipewire-$pw_version"
  if [ ! -f "$pw_prefix/.done" ]; then
    # Built beside the prefix, not in /tmp: meson's install copies extended
    # attributes, and under rootless podman on SELinux a file made in the
    # container's /tmp carries that one container's label, so the next build's
    # container cannot read the cached headers.
    pw_src="$pw_prefix.build"
    rm -rf "$pw_src" && mkdir -p "$pw_src"
    curl -sSfL -o "$pw_src/pw.tar.gz" \
      "https://gitlab.freedesktop.org/pipewire/pipewire/-/archive/$pw_version/pipewire-$pw_version.tar.gz"
    echo "$pw_sha256  $pw_src/pw.tar.gz" | sha256sum -c -
    tar -xzf "$pw_src/pw.tar.gz" -C "$pw_src"
    rm -rf "$pw_prefix"
    # auto_features only reaches `auto` options; these default to `enabled`.
    # spa-plugins, audioconvert and support stay on: the modules' meson files
    # reference audioconvert unconditionally, and none of the three needs a
    # library 22.04 lacks.
    pw_on_by_default="examples tests systemd-user-service pipewire-jack
      pipewire-v4l2 audiomixer bluez5-backend-hsp-native
      bluez5-backend-hfp-native bluez5-backend-ofono bluez5-backend-hsphfpd
      control audiotestsrc dbus videoconvert videotestsrc flatpak"
    meson setup "$pw_src/build" "$pw_src/pipewire-$pw_version" \
      --prefix="$pw_prefix" --buildtype=release \
      -Dauto_features=disabled -Dsession-managers=[] \
      -Dlegacy-rtkit=false -Drlimits-install=false \
      $(printf -- '-D%s=disabled ' $pw_on_by_default)
    ninja -C "$pw_src/build" install
    touch "$pw_prefix/.done"
    rm -rf "$pw_src"
  fi
  PKG_CONFIG_PATH="$(dirname "$(find "$pw_prefix" -name libpipewire-0.3.pc | head -1)")${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
  export PKG_CONFIG_PATH
fi

cd "$root/app"
dx build --renderer native --release --features wayland-share

# dx puts the Linux build under target/dx/vdr/release/linux/; find the binary
# and its assets folder rather than hard-coding a layout not yet seen here.
bin="$(find target/dx/vdr/release/linux -type f -name vdr -perm -u+x | head -1)"
[ -n "$bin" ] || { echo "no vdr binary under target/dx/vdr/release/linux" >&2; exit 1; }
dir="$(dirname "$bin")"

arch="$(uname -m)"
# The commit names the package. VID_COMMIT wins when set: a worktree copied to
# another machine has a .git file pointing at a path that does not exist there,
# so rev-parse would fail and stop the build.
commit="${VID_COMMIT:-$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)}"
out="$root/dist/vid-linux-$arch-$commit.tar.gz"
mkdir -p "$root/dist"
cp "$root/deploy/client/JOINING.md" "$dir/JOINING.md"
tar -czf "$out" -C "$(dirname "$dir")" "$(basename "$dir")"
echo "$out"
echo "Recipients need ALSA, the Vulkan loader and a Vulkan driver, and X11 or Wayland;"
echo "sharing on Wayland needs PipeWire and an xdg-desktop-portal backend."
