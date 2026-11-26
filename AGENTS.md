# vdr — guide for coding agents (and the people driving them)

A peer-to-peer video calling app. Rust + Dioxus 0.7 on **dioxus-native**
(Blitz: Stylo for CSS, Vello for painting), with camera capture via `cameras`
and the network path via the `moq` stack (`moq-net`/`moq-mux`/`moq-video`/
`moq-audio`/`moq-tokio`) over iroh + MoQ. **No webview, no JavaScript engine.**

This file is the things that cost time when you do not know them. The user
guide is `README.md`; the renderer's quirks are in `app/README.md` under
"Notes for future work". Read those bullets before touching layout or the
paint path.

---

## Contributing

- **Every change is a pull request against `master`.** CI runs
  `cargo clippy --all-targets -- -D warnings` and `cargo test` on Ubuntu 22.04
  and macOS, plus a licence check. Run `just check` before pushing.
- **Commits: one short lowercase line**, matching the history (`display names`,
  `stable id'ing`). Pull requests are squash-merged.
- **Say in the PR what you ran and what you did not.** Most of this app can
  only be judged by eye and ear, so a PR that changes behaviour ends with a
  hand-test checklist, ticked only for what a person actually tried. "Compiles
  and tests pass" is not "works": a crash on the first key press got through
  review and unit tests and was found in thirty seconds by hand.
- **Explain why in comments, not what.** Module docs here record the reason a
  thing is the way it is, especially when it looks wrong. Do not reference
  issue or ticket numbers in code comments.
- **Where a claim is inferred rather than observed, say so.** Read the
  dependency's source before stating how it behaves; that habit has changed the
  plan nearly every time.
- **A dependency change needs `just notices`** (regenerates
  `THIRD_PARTY_LICENSES`; CI fails if it is stale) and usually should not
  happen at all: see the pins below. Never run a bare `cargo update`.
- Releases: bump `version` in `app/Cargo.toml`, merge, push a `vX.Y.Z` tag.
  `.github/workflows/release.yml` builds both packages. See README, "Builds
  and releases".

### Running the app while testing, considerately

The person at the keyboard may be on a call. An instance you launch can open
their camera, play their microphone through their speakers, and take keyboard
focus.

- **Always `--ephemeral`** for a test instance: a throwaway identity and data
  folder under the system temp directory. Without it a second copy rotates the
  real app's log away and shares its settings.
- **`--profile <dir>`** instead, when a test must survive a restart (an
  instance that remembers its rooms, their hosts, and its own key): its own
  folder for settings, `rooms.json`, the log and the key (kept 0600 like the
  real one). It never uses the real data folder; one that names it, or names
  nothing, falls back to a throwaway folder. Like `--ephemeral`, such a run never
  updates itself, and `--ephemeral` wins if both are given.
- **`--camera-off`** unless the camera is what you are testing. A second app
  opening the camera can change its format under another app that is using it.
- **`--test-pattern`** when a test needs video but not the camera: generated
  frames (640x360, 30 fps, moving, with a frame counter) enter where a
  camera's do (the self-view slot and the encoder's tap), so everything after
  capture runs for real, and no camera is enumerated or opened. It says the
  camera is on, as a camera would; with `--camera-off` it starts off. Judge a
  run from `--stats`: `camera_sent_fps`/`camera_sent_kbps` on the sender's
  `"link": true` line, `fps` on each viewer's line for that peer.
- **`--loopback` plays the microphone back through the speakers.** Do not use
  it unasked.
- **`--no-audio`** opens neither the microphone nor the speaker. Use it for
  every test instance unless audio is what you are testing, and always when
  running several on one machine: each would otherwise take the microphone and
  play the others through the speakers.
- Do not send synthetic keystrokes or take screenshots of someone's screen
  without being asked. Build, run the unit tests, and hand the visual check to a
  person with a list of exactly what to look at.

---

## Operational facts that cost time otherwise

- **`just` lists the common commands** (`dev`, `loopback`, `local`, `relay`,
  `check`, `package`, …). Room URLs with tokens go in the gitignored `.env` as
  `VDR_ROOM`, never in the justfile or a commit.
- **Work from `app/`.** The binary is `vdr`.
- **Run the app with `dx`, not `cargo`.** `cargo run` builds and runs but comes
  up unstyled: `asset!` only resolves inside a dx bundle. `main.rs` logs an
  error at startup when this happens.

  ```sh
  dx serve --renderer native
  RUST_LOG=vdr=debug dx serve --renderer native   # per-stream fps, texture allocs
  dx serve --renderer native --args="--loopback"  # the `=` matters, see below
  ```

  **App flags go through `--args`, and the `=` is required.** `--args "--x"`
  fails with *unexpected argument*, because clap reads a value starting with
  `--` as another flag.

  **To script two instances, build with `dx` and run the binary from inside the
  bundle**:

  ```sh
  dx build --renderer native --release
  RUST_LOG=vdr=debug ./target/dx/vdr/release/macos/Vdr.app/Contents/MacOS/vdr --ephemeral --listen
  ```

  That path is styled *and* logs to stdout, which `dx serve` and a
  double-clicked `.app` each give only half of. Flags are passed directly here,
  no `--args`.

- **`cargo clippy` / `cargo test` need `app/assets/tailwind.css` to exist**,
  because `asset!` checks the file at compile time, and it is generated and
  gitignored. Any `dx` build creates it; on a fresh clone without one, run
  `just tailwind`.
- **Rooms (`--relay`) need a relay running.** `just relay` builds and runs the
  stock `moq-relay` from a checkout of [moq-dev/moq](https://github.com/moq-dev/moq)
  (the recipe expects it at `~/projects/demos/moq`; adjust to yours). It
  listens on 4443, self-signed, anonymous. Join with
  `--relay http://localhost:4443/<room>` (`http` fetches and pins the
  self-signed certificate; the path is the room), or `just local <room>`.
  README, "Running your own relay", covers certificates, tokens and iroh.
- **A refused token takes about 30 seconds to fail.** That is not a hang.
- **Measure on `--release`, always.** A debug build encodes 1080p at about
  6 fps against 30 in release, so any comparison of size, bitrate or frame rate
  made in debug measures the encoder. Debug is still the better build for
  *scheduling* faults, which it exposes and release hides (see the microphone
  note below).
- **"reconnect stopped" with no other reason is usually a panic** inside
  moq-tokio's reconnect task. The panic text goes to stderr, not to tracing, so
  grep the raw log for `panicked`. The first one found was reqwest needing a
  process rustls crypto provider for `http://` relays, now installed in
  `main.rs`. **Do not remove that call**: both `aws-lc-rs` and `ring` are in
  the tree, so rustls will not choose a provider on its own.
- **`RUST_LOG=vdr=debug`** prints per-stream frame rates, texture allocation,
  which physical device was opened, and every keyboard shortcut dispatched.
- **Logs go to a file by default**: `vdr.log` in the data folder
  (`~/Library/Application Support/vdr/` on macOS, `~/.local/share/vdr/` on
  Linux), with the last three runs beside it. The settings window's About tab
  shows the path. The startup line `logging to …` says where an instance's log
  is.
- **Every privacy-sensitive API needs its own key in `app/Dioxus.toml`'s
  `[macos.plist]`, added in the same change as the code.** macOS does not
  prompt or deny a bundled app that lacks one. It kills it (`TCC`, `SIGABRT`)
  on the first call. **This is invisible in development**: running the binary
  from a terminal makes the terminal the responsible process, and it already
  holds the grant, so every local test passes. It surfaces the moment someone
  else opens the `.app`. Crash reports are at
  `~/Library/Logs/DiagnosticReports/vdr-*.ips`; the `termination` field names
  the missing key. Note `dx build` reads `[macos.plist]` and `dx bundle` does
  **not**.

## Load-bearing pins — do not "clean these up"

- **There is no `dioxus` umbrella crate, deliberately.** Depend on
  `dioxus-native` and import from `dioxus_native::prelude`, never
  `dioxus::prelude`, `dioxus::launch`, or the `web`/`webview`/`server`
  features. If a generator rewrites a file here to say otherwise, it is wrong
  for this repo.
- **`dioxus-native` is pinned to the blitz git tag `v0.3.0-beta.4`**, and
  `blitz-dom` is a direct dependency at the **same tag**. Blitz 0.2.x ships
  stylo 0.8, which fails to compile once `derive_more`'s features are unified
  into the graph, and `hang` (via the moq crates) and `iroh` each pull
  `derive_more` in. Check `cargo tree -i derive_more` before concluding the
  pin is no longer needed.
- **Three crates are vendored and patched** under `app/vendor/`, wired in with
  `[patch]` in `app/Cargo.toml`. Each has a `PATCH.md` saying what and why. Do
  not drop one until an upstream release carries its fix:
  - `moq-audio`: fixes echo cancellation (the canceller never received the
    speaker signal, so every call echoed) and exposes its statistics.
  - `moq-video`: its PipeWire capture dropped every DMA-BUF frame from
    Hyprland's portal.
  - `blitz-dom`: hit-testing a node that was just removed panicked. It also keeps
    scrollbar thumbs visible wherever content overflows (blitz fades them out
    until a scroll); the `scrollbars` feature on `dioxus-native` turns them on,
    and a `scrollbar-color` with two colours paints the track.
- **The moq stack is moq-video/moq-audio 0.1.21, moq-net 0.3, moq-tokio 0.19.**
  moq-video's `vaapi` and `nvidia` features are on for Linux only, `openh264`
  (the software fallback) everywhere. The video encoder is not `Send`: it is
  built and flushed on the encode thread. moq-tokio refuses an iroh key file
  others can read, so `settings::identity_path` makes it 0600 first.
- **`wgpu` is 30**, fixed by the blitz tag. Our textures are handed to the
  renderer, so the versions must match. Run `cargo tree -i wgpu` before
  touching any renderer dependency. (`Cargo.lock` also has a wgpu 29; it is
  target-gated and never on the host.)
- **`tinyvec` is pinned to 1.12.0** in `Cargo.lock`. 1.13.0 does not compile.
- **Colours are oversaturated on wide-gamut (P3) screens, knowingly.** wgpu
  30's Metal backend applies no colour matching, so sRGB goes raw to the
  panel. The fix is merged upstream and waiting on a release. Do not
  compensate in our shader: it would double-correct once the fix ships.
- **The updater's release interface** (`update/`). Installed copies look
  for exactly `vdr-macos-universal-<tag>.zip`, `vdr-linux-x86_64-<tag>.tar.gz`,
  `SHA256SUMS` (`sha256sum` format) and `SHA256SUMS.sig` (a raw 64-byte
  ed25519 signature over the exact bytes of `SHA256SUMS`) on the latest
  GitHub release. Renaming any of them, or changing the zip's `vdr/Vdr.app`
  or the tarball's one-folder layout, strands every copy already installed
  on the release it has. A release without the two signature files is
  offered as a link only.
- **The release public key in `update/verify.rs` never changes casually.**
  Every installed copy trusts that key and no other; a release signed with a
  new one is refused by all of them. Moving to a new key takes a release
  signed with the old one that carries the new one.
- **Testing the updater end to end** needs a build with
  `VDR_UPDATE_TEST_PUBKEY=<64 hex>` set at compile time: only such a build
  trusts that key instead, and only it reads `VDR_UPDATE_FEED` at runtime (a
  URL answering with GitHub's latest-release JSON; its asset URLs may be
  `http`). Release builds set neither. The installer refuses a copy under a
  `target/` folder, an `--ephemeral` or `--profile` run and a translocated macOS app, so test
  on a copy of the bundle moved elsewhere.
- **The `wayland-share` feature is off by default** so that clippy and tests
  need no PipeWire headers; the Linux package turns it on.

## Deliberate, though it looks like a bug

- **The video is positioned absolutely inside its tile** (`participant.rs`).
  As a normal flex child its own pixel size fed back into the layout: hundreds
  of resizes a second, never settling. Likewise the page root is
  `h-screen overflow-hidden`, not `h-full`: a percentage height on the root
  never resolves in blitz.
- **There is one tokio runtime**, a `OnceLock` static in `net/runtime.rs`,
  never dropped. Dropping a `Runtime` blocks until its tasks park, which on the
  UI thread is a freeze.
- **`FrameTap` is one frame deep and overwrites.** A correctness requirement,
  not an optimisation: the bounded queue it replaced made calls publish at
  2 fps, and the symptom looked entirely like a network fault.
- **Two processes can share one camera on macOS.** `--camera` tells two
  windows apart; it is not a workaround for `DeviceInUse`.
- **The microphone capture driver runs on its own thread, and must.** It is
  `!Send`, so it cannot be `tokio::spawn`ed, which makes awaiting it beside the
  video encode loop in one `select!` the obvious move. That compiles and
  silently does not work: **the encode loop never yields while frames are
  arriving** (its only `.await` is the sleep it reaches when there is no
  frame), and two futures in one `select!` are one task. The symptom is not
  scheduling-shaped: audio sets up perfectly and then nothing arrives. **And it
  looks fixed in a release build**, where the work fits between frames.
- **Audio is not video's shape.** Video is newest-wins end to end; audio is
  buffered and loss-tolerant, and the two are never aligned to each other.
  `net/audio.rs` sets the decoder's `max_age` explicitly because the default
  (zero) is the video posture; that value is load-bearing. Catch-up
  (`audio/catchup.rs`) bounds a peer's lag, and its thresholds are measured
  against the sink's buffer just before each write.
- **A display name is a claim, not an identity.** Everything is keyed on the
  endpoint id; names are cleaned (`name::clean`) on the way out *and* on the
  way in, because a sender is never trusted to have done it.
- **Presence fields all have serde defaults**, so a peer on an older build
  ignores what it does not know and a newer one fills in what is missing. Keep
  that when adding a field, with a test both ways.
- **The connection report (`link`, `seen` in presence) is behind the
  Experimental tab's switch for it, and off means neither publish nor show**: the fields
  are absent, so the document is byte-for-byte an older build's, and no bars or
  People section appear. Both fields are read leniently (`quality::lenient_*`),
  because one parse error in a presence document ends listening to that peer.
- **`via` and `held` in presence are said always, behind no switch**: where
  our media goes (`"relay"`, `"guests"`, or a host's endpoint id) and the
  sessions we hold (hosts by id only, never a door, plus `"relay"` while the
  session to where the room meets is up; at most `web::HELD_MAX`). A peer
  that says neither is drawn with an assumed line to the meeting place. They are what the Network tab's map of the room is built
  from (`net/web.rs`), so they go out at once when they change, in the
  announcer's existing poll, not at the clock refresh. Read leniently and
  cleaned with the sender's id, like `seen`. **"relay" means wherever the room
  meets**, which in a room hosted with `--host` is its creator: a guest there
  cannot tell its far side from a relay server, so the map works it out from
  `meets_at` and who is in the room. The map takes people's word over the
  routes we see (`arrival`), which name the last hop towards us, not the
  sender's carrier.
- **Which way media travels on each link comes from `routes.json`**, a
  track of its own in the control broadcast beside presence, **asked for only
  while someone has the map open**. Each receiver says the path every other
  person's media takes to it (from the whole hop chain, `arrival::path_of`),
  its own origin and gate hops, and the hops it knows to be the meeting
  place; a viewer names every hop from everyone's claims (`web::build_reported`).
  Costs nothing unseen, and keep it so: the map's mount sets
  `PeerStats::set_map_open`, and only then does `attend` keep a
  `follow_routes` reader per peer; the writer (`publish_routes`) waits on the
  track's `used()` and writes nothing, and `note_routes` works nothing out,
  while nobody is subscribed. The log says `following N people's routes` /
  `following nobody's routes` and `our routes are (no longer) asked for`. An
  older build neither asks nor publishes, and is drawn from `via`/`held` as
  before. The document is capped at 16 KB before parsing and read field by
  field (`web::clean_reported`).

## Layout, paint and input gotchas

- **A video tile that measures 0×0 never paints**; the widget returns early,
  silently. Check the log for `camera target resized to WxH` (the tile) and
  `source now WxH` (the video), not just the window.
- **The host colour is one theme token**, `--color-host` in `tailwind.css`
  (classes `border-host`, `border-host/40`), and `components/colour.rs`
  holds the same hex for inline SVG and styles, which no class reaches. A
  test fails if the two differ: change both.
- **Inline SVG icons need two things** (`components/icons.rs`): an `xmlns`
  attribute on the `<svg>` (blitz hands the subtree to usvg, which rejects a
  root outside the SVG namespace, and logs it only under `blitz_dom`), and a
  **plain colour** on the parent (`text-white`, `text-[rgb(…)]`), because
  `currentColor` becomes the computed colour and Tailwind v4's palette is
  `oklch()`.
- **The Network tab's map is a widget, not an inline SVG**
  (`components/settings/room_map.rs`): blitz re-parses an SVG subtree on any
  change and its children take no clicks. Blitz calls every widget's `paint`
  on every window paint, so the map caches its `Scene` and rebuilds it only
  when the drawing or the size changes. Its square is two boxes: on one flex
  item with `w-full max-w-[…] aspect-square`, taffy derives the height from
  the unclamped width (read in taffy's source, not seen).
- **The map shows direction by motion**: a carrying line is one light line
  and white dashes drift along it from sender to receiver, two lanes for a
  link carrying both ways (`web_layout::dashes`, `lanes`). Half-white,
  half-grey lines were tried and read as broken lines. The lines and nodes
  stay cached; only the dash layer is built per frame, between them. Frames
  come from a `Ticker` component in `room_map.rs` whose thread calls
  `schedule_update` every 83 ms (about 12 fps), the same wake a video frame
  uses; its thread **parks** while the map's Animate switch is off
  (`Settings::animate_map`) or no line moves, and ends when the map is
  dropped. Over `web_layout::MOVING_MAX` carrying lines only the focus's
  paths move. Keep `requires_redraw` false here too.
- **Video tiles are flex rows, not a grid** (`components/layout.rs`), with
  zero-height line breaks per breakpoint. The box *is* the card; the paint code
  crops the sides when a box is narrower than the video.
- **Overlays take clicks** on this blitz tag, so menus and the settings window
  are real overlays. Things with a positive z-index stay mounted and are hidden
  rather than removed.
- **A key press only reaches us while something of ours has focus.** Blitz
  sends a key to the focused element, and with none to `<html>`, above every
  handler we can attach. The call screen's root element holds focus for the
  shortcuts and takes it back on blur (`components/keys.rs`). A text field
  must set `KeyFocus::set_typing` on mousedown *and* focus, and clear it on
  blur, or shortcuts fire while someone types.
- **Move focus only from an event handler or `onmounted`.** From a spawned
  task or an effect, `set_focus` can run while Dioxus holds the document and
  panics ("RefCell already borrowed").
- **Do not hold a signal borrow across a write to the same signal.**
  `if let Some(x) = *sig.peek() { … sig.set(…) … }` keeps the read guard alive
  for the whole block and panics at the write. Read into a local first.
- **Signals are `!Send`.** What a worker thread discovers reaches the interface
  as messages over the existing channel (`channel.rs`), never as shared
  signals.
- **Any wake of the page repaints the whole window**, whether or not anything
  changed: blitz asks for a redraw whenever the virtual DOM had work, with or
  without mutations. So a polling thread sends a message only when its value
  differs from the last one it sent (`components/microphone.rs` is the
  pattern), and a value that never repeats exactly (an audio level) is reduced
  to what is drawn (a bar width) before comparing. Measured on macOS: the app
  idled at over half a core before this and at about 1% after.
- **The video widget does not ask for continuous redraws.**
  `VideoPaintSource::requires_redraw` is `false`; a `FrameSlot` watcher wakes
  the tile when a frame is published or cleared (`components/participant.rs`).
  Returning `true` there repaints at the display's refresh rate with no video
  on screen. Anything that moves is driven the same way, from a thread that
  wakes one component at the rate it needs and parks when nothing moves (the
  room map's `Ticker`). A wake of one component still repaints the whole
  window: blitz-shell's `redraw` resolves the document, calls every widget's
  `paint` and renders the one scene (read in the source).
- **`overflow-y-auto` makes the other axis scroll too**, and scrollbar thumbs
  are always visible here, so a pane a pixel too narrow for its content grows
  a bar along the bottom. Add `overflow-x-hidden` to a pane that should only
  scroll vertically.
- **No tooltips**: blitz ignores `title`.

## Conventions

- **Tailwind v4 via dx's built-in compiler** (no npm). `tailwind.css` is the
  input; `assets/tailwind.css` is generated and gitignored. Never edit or
  commit it.
- **Every class is a whole string literal.** Tailwind finds classes by scanning
  source text, so one assembled with `format!` silently never generates a rule.
- **If a class appears to do nothing, check the generated file before blaming
  the renderer**: `grep '\.the-class' app/assets/tailwind.css`. A utility that
  generated no rule looks exactly like one this renderer does not support.
  Three things worth knowing about `dx` 0.7.10:
  - It runs the Tailwind binary on every build, but discards its output and
    result, so a failed run leaves the previous file in place **silently**.
    `just tailwind` shows what it would have said.
  - An unchanged rebuild does not rewrite the file: an old mtime means "output
    identical", not "step skipped".
  - `dx serve` leaves a `tailwindcss --watch` process writing that same file,
    and the watcher lags *deletions*.
- **Settings code never panics.** A missing file is the first launch, a
  corrupt one is a log line and defaults, and one bad field costs only itself.
  Changes go through `Settings::update`, a read-modify-write off the UI
  thread.
- **Buttons use `components/button.rs`**; its `shrink-0 whitespace-nowrap` are
  load-bearing, not decorative.
- **Nothing secret in logs or on screen.** A room URL can carry a token
  (`?jwt=…`) or a hosted room's door (`?door=…`); `net/call.rs` has the
  helper that redacts both for a log line. A new secret parameter goes in its
  list. `--host` prints its invite with `println!`, never through `tracing`,
  because the log is a file people attach to bug reports.

## Layout of `app/src/`

| Path | Role |
|---|---|
| `main.rs`, `args.rs` | entry point, CLI (`--relay`, `--listen`, `--dial`, `--host`, `--loopback`, `--camera`, `--camera-off`, `--test-pattern`, `--no-audio`, `--host-rooms`, `--move-calls`, `--move-call`, `--bounce`, `--show-routes`, `--spare-route`, `--no-spare-route`, `--ephemeral`, `--profile`, `--quality`, `--encoder`, …) |
| `settings.rs` | what the app remembers between launches, the data folder, the log path |
| `hotkeys.rs` | keyboard shortcuts as a pure table: actions, chords, the keymap. No UI in it |
| `name.rs` | display names: cleaning, the short id, which label to show |
| `rates.rs` | the last minute of what each outgoing stream sent, and the Network tab's chart scale |
| `net/quality.rs` | the connection report's wire shape, rounding and cleaning, what good/held/poor mean, the grid's `blame`, a peer's report history |
| `net/report.rs` | gathers our report from `Metrics` and each peer's entry, once a second from the attendance loop |
| `recent.rs` | recent rooms: the list, pin/remove/cap, a room's identity, relative times, `rooms.json` (owner-only, holds room tokens: never display or log a stored link) |
| `awake.rs` | keeps the machine awake during a call |
| `update/` | checking for, verifying and installing a newer release: `release.rs` and `verify.rs` are pure, `fetch.rs` the network, `install.rs` the swap and restart |
| `channel.rs` | shared single-slot channel primitive |
| `audio/` | `Controls` (mute, chosen devices, level), device enumeration, `catchup.rs` |
| `camera/hooks/` | chosen device, on/off, persistence, the stream and its watchdog |
| `camera/pattern.rs` | the test pattern (`--test-pattern`): generated frames, and the thread that feeds them in where a camera's go |
| `camera/frames.rs` | `FrameSlot`: newest-wins handoff, with its own neutral `Frame`/`Pixels` |
| `camera/paint/` | `VideoPaintSource` (a blitz `Widget`), wgpu upload, fit math |
| `net/call.rs` | a call over iroh + MoQ: rooms on a relay, direct calls, hosting a room (`--host`), presence |
| `net/follow.rs` | reading what a peer offers (video, voice, screen) for as long as they are here: follows the catalog, asks again for a broadcast whose read ended with a change of route |
| `net/sessions.rs` | a join that dials the relay and the room's saved hosts together: which session carries our media, and moving it |
| `net/arrival.rs` | through whom each person's video arrives, read off its route's hop chain, and where ours goes, for the log and the host borders on tiles (`read` and `through` are pure) |
| `net/moves.rs` | moving the call to a host, an experiment: the presence field, which choice wins, the bounce order (pure) |
| `net/web.rs` | the room as a web of nodes for the Network tab: the `via`/`held` presence fields and the `routes.json` document (wire form, lenient reading, cleaning, id prefixes), naming hops from everyone's claims, and the pure graph built from every entry, with each link's streams counted per direction and where a sender's word and its receivers part |
| `web_layout.rs` | where the Network tab's map puts each node (one ring with the relays at six o'clock, a thin inner ring for people past about ninety; in a unit square, from counts and order only) and its name (radially outside it), which lines it draws, how a line bows round a node it does not end at, where its drifting dashes are and which lines have them, what a selection keeps lit, and the made-up room of `--fake-web` (reports included) |
| `net/door.rs` | a hosted room's door: its name, the secret guests present as `?door=`, the check (pure) |
| `net/audio.rs` | the audio half: Opus out, each peer's voice in, echo cancelled |
| `net/share.rs` | screen sharing: capture and its own broadcast |
| `net/loopback.rs` | the in-process H.264 round trip |
| `net/runtime.rs` | the single tokio runtime |
| `components/` | `call_screen.rs` (the page and its key handler), `layout.rs`, `tile.rs`, `tile_menu.rs`, `participant.rs`, `bar.rs`, `microphone.rs`, `share.rs`, `subroom.rs`, `device_menu.rs`, `connect.rs`, `recent.rs` (the recent rooms list), `keys.rs`, `icons.rs`, `button.rs`, `stats.rs`, `signal.rs` (signal bars) |
| `components/settings/` | the settings window: a `Tab` variant plus a component per tab (`profile.rs`, `keyboard.rs`, `updates.rs`, `network.rs`, `experimental.rs`, `about.rs`); `room_map.rs` is the Network tab's map, a blitz `Widget` painting lines and shapes with names and click targets over it |

Frames never touch the DOM:
`cameras::pump` → `FrameSlot` → `VideoPaintSource` → vello.

`cargo test` runs the unit tests; the pure modules (`hotkeys.rs`, `name.rs`, `recent.rs`, `rates.rs`, `net/quality.rs`,
`camera/paint/fit.rs`, `audio/catchup.rs`, `components/layout.rs`,
`update/release.rs`, `update/verify.rs`, `net/door.rs`, `net/moves.rs`, `net/arrival.rs`, `net/web.rs`, `web_layout.rs`) are where
logic that can be tested without a window belongs.
