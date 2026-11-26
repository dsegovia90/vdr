# vdr

[![CI](https://github.com/dsegovia90/vdr/actions/workflows/ci.yml/badge.svg)](https://github.com/dsegovia90/vdr/actions/workflows/ci.yml)

A video room that a small team leaves open all day. Cameras stay on, the way
desks in one office are in view of each other, so you can see whether someone
is around, lean over and ask a thing, or step into a side room together for a
conversation that does not pull everyone else in. The main room is where
light work and the start of the day happen; when someone needs to
concentrate they move to a subroom of their own, and the team reads that as
"heads down", not as gone. It was built by a developer team for its own use.

There is no hosted service. Each team runs its own relay, and "Running your
own relay" below is the guide. Two people can also call each other directly.

It is not a monitoring tool. Nothing is recorded, nothing is stored on the
relay, and there is no history, status log or screenshot anywhere: what
anyone can see is what they would see standing in the room, and only while
they are in it. Camera and microphone are yours to turn off, and how loud
everyone outside your subroom is, is yours to set.

Written in Rust. The UI is
[Dioxus](https://dioxuslabs.com) 0.7 on dioxus-native
([Blitz](https://github.com/DioxusLabs/blitz): Stylo for CSS, Vello for
painting), with no webview and no JavaScript engine. Media goes over
[MoQ](https://github.com/moq-dev/moq) on QUIC: H.264 video, Opus audio with
echo cancellation, and screen sharing.

The binary is `vdr`. The project is pre-1.0; expect things to change. The MoQ
crates are pinned to exact versions, and two of them are vendored with fixes
under `app/vendor/` (each has a `PATCH.md`).

## How a call connects

- **Through a relay, as a room.** This is the everyday mode: everyone
  connects to the same MoQ relay, and the URL's path names the room. Each
  participant sends once and the relay fans out, so a room holds as many
  people as the relay's bandwidth allows. A room URL is one of:
  - `https://relay.example.com:4443/<room>` -- a relay with a real certificate;
  - `iroh://<relay-endpoint-id>/<room>` -- a relay with iroh enabled, reached by
    its key: no certificate, no DNS name, and the relay needs no open port
    (it works from behind a home router). Everyone finds the relay through
    iroh's public relay servers and then talks to it directly where NAT
    allows; where it does not, iroh carries that person's traffic through
    those servers instead, today without telling you;
  - `http://localhost:4443/<room>` -- a local development relay (the client
    fetches and pins its self-signed certificate).

  A relay that requires tokens takes one as `…/<room>?jwt=<token>`.
  "Running your own relay" below is how to set one up.
- **Directly, over [iroh](https://iroh.computer).** For two people with no
  relay of their own: each instance has a key, one side waits, the other
  pastes that key and connects. The two machines find each other and punch
  through NAT with the help of iroh's public relay servers, then talk
  directly; today, if no direct path can be made, iroh carries the call
  through those servers instead. It is the quickest way to try the app: run
  it on two machines (or twice on one, with `--camera` to pick different
  cameras), copy the key from one window into the other, and you are on a
  call with nothing else to set up.

- **Hosted by one of you, as a room (experimental).** For a room with no
  relay at all: one person runs `vdr --host <room>`, and their app becomes the
  room's meeting place. It prints an invite to the terminal, once:

  ```sh
  vdr --host team
  # Hosting team. Guests join with:
  #   vdr --relay 'iroh://<host key>/team?door=<secret>'
  ```

  Guests join with that link like any room (`--relay`, or paste it into the
  field), and see and hear each other through the host, who is in the room
  too. The `door` is a random secret made fresh each time the host starts, so
  each start needs a new invite; a dial without it, with the wrong one, or for
  another room is turned away. The invite is printed to the terminal and never
  written to the log, so start the host from a terminal. Everything everyone
  sends passes through the host's upload, so it suits a few people; when the
  host leaves, the room ends for everyone. Hosts are not announced or
  remembered yet, and the room name is letters, digits, `-`, `_` and `.`.

Paste a room URL or a key into the field at the top of the window, or pass it
on the command line (`--relay <url>`, `--dial <key>`, `--listen`, `--host
<room>`; the module docs in `app/src/args.rs` list the rest).

**Recent rooms.** Rooms you have joined are listed under the field while you
are not in a call, newest first, with one-click *Join*. Pin one to keep it at
the top; otherwise the ten most recent are kept. They are stored on your
machine only, in `rooms.json` in the data folder, **including any access
token in the room's link**, so that rejoining needs no pasting; the file is
readable only by your user, and the token is never shown or logged. Remove an
entry with its ×, or delete the file to forget them all. Direct calls by key
are not remembered.

## In a call

- **Subrooms** are how you step aside without leaving. Two people who want to
  talk open one and go in, and the rest of the room still hears them faintly;
  one person goes in alone to work undisturbed, and anyone can pop in with a
  question and out again. The menu beside Leave lists the open subrooms and
  who is in each: one click joins one, *New subroom* opens another (they are
  named after rooms of a house), *Back to main* leaves. A subroom exists
  while someone is in it; nobody creates or closes it. The same menu has the
  **"Everyone outside my subroom"** volume: people outside your subroom are
  still heard at the level you set there, from muted up to full, and it is
  remembered across launches. Tiles are grouped under their subroom's label
  and the ones outside yours are dimmed.
- **Tile menu.** A person's name under their tile (or their short id, if
  they have not set a name) is a menu: join their subroom, invite them to
  yours, or ask them to mute. When they have a name, the menu also shows
  their short id, so two people with the same name can be told apart. An
  invite shows up in the other person's subroom menu as a one-click *Join*;
  it never moves anyone. A mute runs their own mute, so they see it and can
  unmute.
- **Screen sharing.** Share from the bar; a shared screen is one more tile in
  the room, beside the sharer's camera. While you share, *Sharpen* beside
  the Share button lets a large screen (1080p and up) use up to 6 Mbps of
  upload instead of 2.5, for crisper text; press it again to go back. Every
  share starts without it.
- **Mute and camera off** are in the bar and shown on your tile to the room.

### Settings and keyboard shortcuts

The gear at the right of the bar opens the settings (also Cmd+, on macOS,
Ctrl+, elsewhere). Its *Profile* tab sets your display name, shown under your
video to everyone in the call in place of your short id; a change reaches a
call you are already in. Names are not verified: anyone can choose any name,
and two people can choose the same one. Its *Keyboard* tab lists the
shortcuts and changes them: click a key, then press the new one; Backspace
removes it, Esc gives up. Its *Updates* tab is described below. Its
*Network* tab opens on a map of the room: relays as squares, people as
circles (a blue ring on anyone who could host), and a line for every link,
split in the middle. The half next to someone is white when they send media
along it and grey when they do not, so a line white all along carries both
ways, white at one end only carries one way from that end, and all grey is
a link held with nothing on it; this comes from what each person's app says
about how everyone else's media reaches it, asked for only while the map is
open. Click someone to dim everything off their paths and read where their
media goes and comes from, including where what they say and what others
see disagree; a relay nobody reaches is drawn faint and dashed. Everyone sits on one ring, you at the top and
the relays together at the bottom, so a line between two people never
passes through a relay; a line that would brush a node it does not end at
is drawn curved around it. A direct call is one white line between the two
of you. Under the map it charts the last minute of a
call: what the camera sent against
its cap and its slice of the link and, while a screen is shared and for a
minute after, the same for the share plus everything sent together against
the link's whole estimate (video only; audio is not counted). The lines are a
five-second average. The estimate is divided between the microphone, the
screen share and the camera, in that order, and each video stream follows its
slice, so the last chart is where it shows whether everything together fit.
Under the charts, while the connection report is on and someone else is
in the call, *People* lists everyone, you first, with what each says about
their own connection: the link's estimate, round trip, recent loss, the frame
rate they send, whether their camera and share are at their caps or held by
the link, and on iroh whether the path is direct or relayed. A person whose
build does not report, or who turned it off, is listed as not reporting.
Everyone else also gets a chart of the last 2 minutes built from their
reports, one point every 2 seconds. Then a grid of who gets what: rows send,
columns receive, and each cell is what that column's person gets from that
row's (frames a second, and how much later than its best the video arrives),
coloured by quality; the diagonal is each person's own connection. A row
mostly poor is marked "their upload?", a column mostly poor "their
download?"; a single bad cell stays a single cell.
Its *Experimental* tab has a switch per feature still being tried out. *Connection report*, on by default: with it on, your
presence tells the room how your connection is doing and what you get from
each person, and you see what they say. Each video's name label then carries
signal bars: three green bars is good, two amber is held by the link (it
cannot carry everything being sent), one red is poor, three hollow grey ones
is not known yet. A person not reporting has none, and neither does a shared
screen; the person's menu says it in a line. Turning the switch off stops
telling the room about your connection and hides everyone else's. The
thresholds are first guesses. Whatever the switches, your presence says
where your media goes (the relay or a host) and which hosts you hold a
connection to, by their ids, never a hosted room's door.
*Mark hosts on their videos*, off by default, is a debugging aid: everyone
who could host the room has a faint blue border around their video, and a
host carrying the call for you, because your video goes through them or
someone's reaches you through them, a bright, thicker one. Your own video
has the bright border while you pass other people's video on. It follows
the routes announced as best, which the video follows within a moment; the
log says those routes in full.
*Keep a spare route*, on by default, does something only in a room with
hosts: your video is also offered through a second way into the room, priced
so that nobody uses it while the first works, so when the relay stops
everyone switches to it at once instead of your video leaving their screens
for a moment. It uses no upload until it is needed. A relay that freezes
without closing still freezes the call until its connection times out.
*Move the call*, off by default, is a test instrument for rooms with hosts:
with it on, a host's menu offers "Move the call here", and everyone else who
has the switch on follows the move, sending their video through that host's
computer until someone chooses "Move the call back to the relay", in that
host's menu or in the same tab (a host still carries the call while the relay
is unreachable). People who cannot reach the host stay on the relay, and the
host carries them across.
Its *About* tab shows the version and where the
log is, which is what to send after a call that went wrong.

### Updates

Shortly after it starts, and once a day after that, vdr asks GitHub whether
there is a newer release. When there is, a dot shows on the gear and on the
*Updates* tab, and a banner above the connect field says so outside a call
(*Dismiss* hides it until a still newer release). That is all a check does. Nothing is downloaded until you press *Update and restart* in the
*Updates* tab, which downloads the release, checks it, replaces the app and
restarts it. It cannot be pressed during a call. *Check now* checks at once,
and a switch turns the automatic check off.

Releases are signed. Before anything is unpacked, vdr checks the release's
signature against a key built into the app and the download's checksum
against the signed list, and refuses a package that does not match.

vdr replaces itself only where it was installed from a release: on macOS a
`Vdr.app` you can write to (not one opened straight from Downloads or a disk
image; move it to Applications first), on Linux the folder the tarball
unpacked to. Anywhere else the tab says why and gives the release page, and
on Linux it can still download and check the package for you to install by
hand.

| Action | macOS | Linux |
|---|---|---|
| Toggle microphone | `m` | `m` |
| Toggle camera | `v` | `v` |
| Share screen (stops a share, or opens the list) | `s` | `s` |
| Leave or cancel the call | `cmd+shift+l` | `ctrl+shift+l` |
| Open settings | `cmd+,` | `ctrl+,` |

Shortcuts work while the vdr window is in front, and pause while you type in
a text field or have the settings open. Esc closes the settings or an open
menu, and cannot be rebound.

Changed shortcuts are saved in `settings.json` in the data folder (shown in
*About*) under `hotkeys`, by action, and only the ones you changed:

```json
"hotkeys": {
  "toggle_mic": "ctrl+alt+m",
  "share_screen": ""
}
```

An empty string means no key. The actions are `toggle_mic`, `toggle_camera`,
`share_screen`, `leave` and `open_settings`; a key is written in lowercase,
modifiers first (`cmd`, `ctrl`, `alt`, `shift`) joined by `+`, with named
keys such as `space`, `enter`, `left`, `f5` and `plus`. An entry that does
not parse is ignored and that action keeps its default.

## Building and running

The crate is in `app/`; `app/README.md` has the developer notes (styling, the
video paint path, renderer quirks). You need a Rust toolchain and the Dioxus CLI:

```sh
cargo install dioxus-cli --version 0.7.10 --locked
cd app
dx serve --renderer native                       # hot-reloading debug window
dx serve --renderer native --args="--loopback"   # camera -> H.264 -> screen, no network
```

To run several instances on one machine without touching its camera or audio,
give each a throwaway identity and leave the devices closed:
`vdr --ephemeral --camera-off --no-audio …` (`--no-audio` opens neither the
microphone nor the speaker). `--name <name>` gives a run a display name
without saving it, so the windows can be told apart. For a test that must survive a restart, use
`--profile <dir>` instead of `--ephemeral`: the instance keeps its own folder
(settings, rooms, log and key) there, never the real one, and never updates
itself.

To send real, moving video without a camera, add `--test-pattern`: a
generated 640x360 picture at 30 fps (a sliding colour field, a white block
crossing it, and the frame number in the corner) goes in where the camera's
frames would, so the encoder, rate control, publishing and your own tile all
run as with a camera, and no camera is listed or opened. With `--camera-off`
too it starts switched off, and the camera button turns it on. With
`--stats <file>`, the sender's `"link": true` line carries `camera_sent_fps`,
`camera_sent_kbps` and `camera_cap_kbps`, and each viewer's line for that
sender (`"peer": <id>`) carries the received `fps` and `camera`.

To test moving a call between the relay and a room's hosts from a script,
start the instances with `--move-calls` (the *Move the call* switch, unsaved),
and one of them with `--bounce <secs>`, which moves the call round the relay
and each host it knows every so often (10 seconds at least, 30 if no number
is given). `--move-call relay|<host id>` moves it once, as the join starts.
Each move is logged, and each viewer logs `<id> left` and `<id> back after N
ms` for a tile that went and came back. `--show-routes` turns on the host
borders for that run without saving it; each viewer logs `<id> now arrives …`
whenever someone's route changes, whether or not the borders are on, and,
with them on, `[tile] <id> carries the call for us now` when a border turns
bright (and `no longer` when it fades).
`--no-spare-route` (or `--spare-route`) turns the *Keep a spare route* switch
off (or on) for that run without saving it, to compare the two; with it on,
the log says where the spare is (`a spare offer on host <id>`) and when it
takes over (`the spare on host <id> now carries our media`).

Run it with `dx`, not `cargo run`: the stylesheet only resolves inside a dx
bundle, and `cargo run` comes up unstyled. `cargo clippy` and `cargo test` work
as usual. Measure anything performance-related on a release build: a debug
build encodes far too slowly to tell you anything.

The root `justfile` has the common commands (`just` lists them): `just build`
for a release bundle, `just loopback`, `just relay` for a local relay, and
`just package-mac` for a zip of the macOS app. `deploy/client/JOINING.md` is the
page for someone who was sent a build and a link.

## Contributing

Changes come in as pull requests against `master`. [`AGENTS.md`](AGENTS.md) is
the working guide to this codebase: how to run and test it, which dependency
pins are load-bearing and why, and what a pull request should say. It is
written so that a coding agent can be pointed at it, and it reads the same for
a person.

## Builds and releases

Every pull request and every push to `master` runs CI
(`.github/workflows/ci.yml`): `cargo clippy` with warnings as errors and
`cargo test`, on Ubuntu 22.04 and macOS, and a check that
`THIRD_PARTY_LICENSES` still matches `Cargo.lock` (`just notices` regenerates
it).

Releases are built by `.github/workflows/release.yml`, from the same scripts
as a local package (`deploy/client/package-macos.sh` and
`deploy/client/build-linux.sh`). To cut one:

1. Set `version` in `app/Cargo.toml` to the new version, and merge that.
2. Tag the merge commit and push the tag:
   `git tag v0.2.0 && git push origin v0.2.0`.

The workflow refuses a tag that does not match `app/Cargo.toml`, then builds
`vdr-macos-universal-<tag>.zip` (Apple Silicon and Intel in one app) and
`vdr-linux-x86_64-<tag>.tar.gz` (built on Ubuntu 22.04, so it runs on that
and newer distributions) and attaches both to a GitHub Release with notes
generated from the merged pull requests. Downloads are on the
[Releases page](https://github.com/dsegovia90/vdr/releases).

To get packages without a release, use *Run workflow* on the Release workflow
in the Actions tab: both archives are attached to that run as artifacts. A pull
request that changes the workflow or `deploy/client/` builds them the same way.

The macOS app is signed with the project's own certificate, not with an Apple
Developer ID. That keeps its camera, microphone and screen permissions across
updates, but macOS still does not know the publisher, so it will
not open it with a double-click the first time: right-click `Vdr.app`, choose
*Open*, then *Open* again. If macOS calls it "damaged", run
`xattr -dr com.apple.quarantine /Applications/Vdr.app` once.
`deploy/client/JOINING.md`, inside both archives, says the same for whoever
receives a build.

## Platforms

- **macOS** (Apple Silicon and Intel). The camera, microphone and
  screen-recording permissions are requested by the bundled app; its
  `Info.plist` keys come from `app/Dioxus.toml`.
- **Linux** (X11 and Wayland). Build it on Linux, it cannot be cross-built:
  `deploy/client/build-linux.sh` on the machine itself, or
  `deploy/client/build-linux-portable.sh` in an Ubuntu 22.04 container with
  podman for a binary that runs on older distributions (`just package-linux`
  runs that over ssh on `LINUX_HOST`). Release packages are built by CI; see
  "Builds and releases" above. Runtime needs ALSA, PipeWire, and Vulkan.
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
secret = "iroh-secret.key"      # created on first run (pre-create it as 64 hex
                                # chars if the config dir is read-only); keeps
                                # the relay's endpoint id stable
bind_v4 = "0.0.0.0:4444"        # optional: a fixed UDP port you can forward, so
                                # peers reach the relay directly without hole punching
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
