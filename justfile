# vid -- common commands. `just` lists them.
#
# Room URLs carry join tokens, so they live in `.env` (gitignored), not here:
#   VID_ROOM=https://relay.example.com:4443/<room>?jwt=<token>

set dotenv-load

app    := justfile_directory() / "app"
bundle := app / "target/dx/vdr/release/macos/Vdr.app/Contents/MacOS/vdr"
log    := env("RUST_LOG", "vdr=info,moq_audio=info")

default:
    @just --list

# Hot-reloading dev window (debug build). Flags: just dev --loopback
dev *args:
    cd {{app}} && dx serve --renderer native {{ if args == "" { "" } else { "--args=\"" + args + "\"" } }}

# Release bundle: styled, and the build to measure anything on.
build:
    cd {{app}} && dx build --renderer native --release

# Run the release bundle with flags, logging to the terminal.
run *args: build
    RUST_BACKTRACE=1 RUST_LOG={{log}} {{bundle}} {{args}}

# In-process camera -> H.264 -> screen round trip, audio included.
loopback: (run "--loopback")

# Join the room in .env's VID_ROOM; output also goes to ~/vid-call.log.
call room=env("VID_ROOM", ""): build
    @test -n "{{room}}" || { echo "set VID_ROOM in .env, or: just call '<url>'"; exit 1; }
    RUST_BACKTRACE=1 RUST_LOG={{log}} {{bundle}} --relay '{{room}}' 2>&1 | tee ~/vid-call.log

# Join a room on a local relay (start one with `just relay`).
local room="dev": (run "--relay http://localhost:4443/" + room)

# The stock moq relay on localhost:4443, self-signed, anonymous.
relay:
    cd ~/projects/demos/moq && cargo build --release --bin moq-relay
    cd ~/projects/demos/moq/demo/relay && RUST_LOG=info ../../target/release/moq-relay localhost.toml

# Lints and unit tests.
check:
    cd {{app}} && cargo clippy --all-targets && cargo test

# Regenerate the stylesheet by hand, to see errors dx would hide.
tailwind:
    cd {{app}} && ~/.dx/tools/tailwindcss-v4.1.5/tailwindcss --input tailwind.css --output assets/tailwind.css

# Did a call meet S7? Pass each participant's --stats file.
report +files:
    cargo run --manifest-path {{app}}/Cargo.toml --release --bin s7-report -- {{ files }}

# Mac zip into dist/.
package-mac:
    deploy/client/package-macos.sh

# Portable Linux tarball, built on `desktop`, copied into dist/.
package-linux:
    #!/usr/bin/env bash
    set -euo pipefail
    commit=$(git rev-parse --short HEAD)
    rsync -a --delete --exclude '/app/target' --exclude '/target' --exclude '/dist' \
      --exclude '/.claude' --exclude '/doc/dstack' --exclude '/.env' ./ desktop:projects/vid/
    ssh desktop "cd ~/projects/vid && VID_COMMIT=$commit deploy/client/build-linux-portable.sh"
    scp -q "desktop:projects/vid/dist/vid-linux-x86_64-$commit.tar.gz" dist/
    echo "dist/vid-linux-x86_64-$commit.tar.gz"

# Both packages, after clearing older ones out of dist/.
package:
    rm -f dist/vid-*.zip dist/vid-*.tar.gz
    just package-mac package-linux

# Follow this machine's app log (what testers send after a bad call).
log:
    tail -f ~/Library/Application\ Support/vdr/vdr.log
