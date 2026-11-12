# vid

A video calling app for small groups, written in Rust. The UI is
[Dioxus](https://dioxuslabs.com) 0.7 on dioxus-native
([Blitz](https://github.com/DioxusLabs/blitz): Stylo for CSS, Vello for
painting), with no webview and no JavaScript engine. Media goes over
[MoQ](https://github.com/moq-dev/moq) on QUIC: H.264 video, Opus audio with
echo cancellation, and screen sharing.

The binary is `vdr`. The project is pre-1.0; expect things to change. The MoQ
crates are pinned to exact versions, and two of them are vendored with fixes
under `app/vendor/` (each has a `PATCH.md`).

## How a call connects

- **Peer to peer, over [iroh](https://iroh.computer).** Each instance has a
  key. One side waits, the other pastes that key and connects; nothing else
  needs to run. This is for two people.
- **Through a relay, as a room.** Everyone connects to the same MoQ relay, and
  the URL's path names the room. Each participant sends once and the relay fans
  out. A room URL is one of:
  - `https://relay.example.com:4443/<room>` -- a relay with a real certificate;
  - `iroh://<relay-endpoint-id>/<room>` -- a relay with iroh enabled, reached by
    its key;
  - `http://localhost:4443/<room>` -- a local development relay (the client
    fetches and pins its self-signed certificate).

  A relay that requires tokens takes one as `…/<room>?jwt=<token>`.

Paste a key or a room URL into the field at the top of the window, or pass it
on the command line (`--dial <key>`, `--listen`, `--relay <url>`; `--help`
lists the rest).

## Building and running

The crate is in `app/`; `app/README.md` has the developer notes (styling, the
video paint path, renderer quirks). You need a Rust toolchain and the Dioxus CLI:

```sh
cargo install dioxus-cli --version 0.7.10 --locked
cd app
dx serve --renderer native                       # hot-reloading debug window
dx serve --renderer native --args="--loopback"   # camera -> H.264 -> screen, no network
```

Run it with `dx`, not `cargo run`: the stylesheet only resolves inside a dx
bundle, and `cargo run` comes up unstyled. `cargo clippy` and `cargo test` work
as usual. Measure anything performance-related on a release build: a debug
build encodes far too slowly to tell you anything.

The root `justfile` has the common commands (`just` lists them): `just build`
for a release bundle, `just loopback`, `just relay` for a local relay, and
`just package-mac` for a zip of the macOS app. `deploy/client/JOINING.md` is the
page for someone who was sent a build and a link.

## Platforms

- **macOS** (Apple Silicon and Intel). The camera, microphone and
  screen-recording permissions are requested by the bundled app; its
  `Info.plist` keys come from `app/Dioxus.toml`.
- **Linux** (X11 and Wayland). Build it on Linux, it cannot be cross-built:
  `deploy/client/build-linux.sh` on the machine itself, or
  `deploy/client/build-linux-portable.sh` in an Ubuntu 22.04 container with
  podman for a binary that runs on older distributions (`just package-linux`
  runs that over ssh on `LINUX_HOST`). Runtime needs ALSA, PipeWire, and Vulkan.
  Screen sharing on Wayland goes through xdg-desktop-portal with your
  desktop's backend, and needs the `wayland-share` feature, which the package
  scripts turn on.

Video uses the hardware encoder where there is one (VideoToolbox on macOS,
VA-API or NVENC on Linux). The software fallback is Cisco's openh264, compiled
from source into the binary; if you redistribute builds, H.264 patent licensing
is your responsibility.

## Running your own relay

Rooms use the stock `moq-relay` from <https://github.com/moq-dev/moq>. Install
the release this app is tested against; newer relays (and moq `main` since
2026-11) have dropped the iroh transport, so a newer one serves `https://`
rooms only:

```sh
cargo install moq-relay --version 0.14.15 --locked
cargo install moq-token-cli --version 0.5.46 --locked
moq-relay relay.toml
```

A minimal `relay.toml`:

```toml
[server]
bind = "[::]:4443"              # UDP: QUIC only

[server.tls]
cert = "/path/to/fullchain.pem"   # a certificate for the relay's hostname
key = "/path/to/privkey.pem"

[auth]
public = ""                     # anyone may join any room; see below for tokens

[iroh]
enabled = true                  # also accept iroh:// rooms
secret = "iroh-secret.key"      # created on first run; keeps the relay's key stable
```

The relay logs its iroh key at startup (`iroh listening endpoint_id=…`); that is
the `<relay-endpoint-id>` in an `iroh://` room URL. For a quick local relay
with a self-signed certificate, `just relay` builds a moq checkout and runs its
`demo/relay/localhost.toml`; join it with `http://localhost:4443/<room>`.

**Tokens.** To keep a relay private, give it a signing key and drop the
`public` line, so a connection without a valid token is refused. The token tool
is `moq-token` (installed above):

```sh
moq-token generate --out root.jwk                 # once; keep it secret
moq-token sign --key root.jwk --root <room> --publish "" --subscribe "" \
  --expires $(( $(date +%s) + 7 * 86400 ))        # a week in <room>
```

```toml
[auth]
key = "root.jwk"
```

Join with `https://relay.example.com:4443/<room>?jwt=<token>`. Anyone holding
that URL can join the room until the token expires.
