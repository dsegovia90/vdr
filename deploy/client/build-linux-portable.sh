#!/usr/bin/env bash
# Builds the Linux package inside Ubuntu 22.04, so it runs on most distributions.
#
#   deploy/client/build-linux-portable.sh   # on a Linux machine with podman
#
# **Why a container:** a binary needs at least the glibc it was built against.
# Built straight on Fedora 44 it demanded GLIBC_2.43 and would not start on
# Ubuntu 24.04 (2.39), Debian 12 (2.36) or older. Ubuntu 22.04's 2.35 is old
# enough for nearly every current desktop, and glibc runs older builds fine.
#
# `git config` runs from `/`: git reads the current directory's repo first even
# for `--global`, and a worktree copied off the Mac has a dangling `.git` file.
#
# Rootless podman, no sudo. The container's build output and cargo cache live
# in named volumes, apart from any build on the host.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
podman run --rm \
  -v "$root:/src:Z" \
  -v vid-linux-target:/src/app/target \
  -v vid-linux-cargo:/root/.cargo \
  -w /src \
  -e PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  -e VID_COMMIT="${VID_COMMIT:-}" \
  docker.io/library/ubuntu:22.04 \
  bash -c 'apt-get update -qq && apt-get install -y -qq git curl ca-certificates >/dev/null && (cd / && git config --global --add safe.directory /src) && deploy/client/build-linux.sh'
