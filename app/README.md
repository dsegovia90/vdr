# vid

A camera preview built on [Dioxus 0.7](https://dioxuslabs.com/learn/0.7) running on
**dioxus-native** ([Blitz](https://github.com/DioxusLabs/blitz)) — Stylo for CSS,
Vello for painting. There is no webview and no JavaScript engine in this build.

## Running

```sh
dx serve --renderer native       # regenerates Tailwind, then builds
dx build --renderer native       # one-shot build
RUST_LOG=vdr=debug dx serve --renderer native    # frame rate + texture logging
```

**`cargo run` is not a supported entry point.** It builds and runs, but the
window comes up unstyled -- see Styling. The app logs an error at startup when
that happens, so it is at least never a mystery. `cargo build`, `cargo clippy`
and `cargo test` are all fine; it is only *running* that needs dx.

## Styling

Tailwind v4, via dx's built-in Tailwind (no npm needed). `tailwind.css` next to
`Cargo.toml` is the input; dx compiles it to `assets/tailwind.css` and bundles
that under a content-hashed name, which `main.rs` references through
`asset!` + `document::Stylesheet`. `assets/tailwind.css` is generated and
gitignored -- do not edit or commit it.

Under `dx serve` a class change hot-reloads. Under `dx build` it needs a
rebuild.

**Why `cargo run` cannot work.** `asset!` compiles to a placeholder string that
only dx substitutes with the real bundled path. Nothing downstream treats the
leftover placeholder as an error: `dioxus_asset_resolver` answers the bogus path
with a 404 whose *body* is the text "Not Found", dioxus-native logs that as a
success, and Blitz parses those nine bytes as a stylesheet. The only symptom
would be an unstyled window, so `main.rs` checks the asset path at startup and
logs an error instead. `RUST_LOG=dioxus_native=trace` shows the path actually
requested, which is the fastest way to tell a bundling problem from a CSS one.

## How the video path works

Frames never touch a DOM. The chain is:

```
cameras::pump (worker thread)
  -> FrameSlot            src/camera/frames.rs   newest-frame-wins handoff
  -> VideoPaintSource     src/camera/paint/      a blitz Widget, uploads to wgpu
  -> vello (hybrid)                              composites the texture
```

`CustomWidgetAttr::new(VideoPaintSource::new(..))` wraps the widget, and RSX
mounts it as `object { "data": widget }` (blitz `v0.3.0-beta.4`; the old pin
used `use_wgpu` and `canvas { "src": id }`).

## Notes for future work

- **Why not `dioxus-cameras`.** It renders previews by running a loopback HTTP
  server and injecting a WebGL2 `<script>`. Blitz has no JS engine, so that path
  cannot work here. Its `Registry` also exposes no public frame *reader* — only
  `publish_frame` — so its frames are unreachable except over that HTTP server.
  We depend on the underlying `cameras` crate directly instead.
- **`wgpu` is 30**, fixed by the pinned blitz tag (`v0.3.0-beta.4`) ->
  anyrender_vello_hybrid 0.11. Textures are handed to the renderer, so both must
  be the same wgpu version. On wide-gamut screens colours are oversaturated
  until an upstream fix ships (ticket 0).
- **No `<select>`.** Blitz ships no interactive select widget, hence
  `components/dropdown.rs`.
- **The video element is positioned absolutely inside its tile.** It has no
  useful intrinsic size, and as an ordinary flex/grid child its own pixel size
  fed back into the grid, resizing every frame. Out of layout, it can only take
  the size its tile is given. The app root is `h-screen` rather than `h-full`,
  because a percentage height read as indefinite on the old pin and collapsed
  the tiles to a few pixels.
- **Tailwind v4 renders correctly**, including `@layer`, `@property`,
  `color-mix()` and `oklch()` colours — Blitz styles through Stylo, which is
  Firefox's engine, so the modern CSS surface is well covered.
- **Stacking contexts work.** `Node::is_stacking_context_root` implements the
  usual rules (opacity, position with a z-index, transforms), painting hoists
  negative and positive z-index children around the regular ones, and
  hit-testing consults the same hoisted set — so ordering and clicks agree. An
  earlier revision sorted `z-index` among direct siblings only.
- **Overlays take clicks** since `v0.3.0-beta.4`. The old pin's `Node::hit`
  would not descend into an absolutely positioned child that overflowed its
  parent, so the dropdown menu used to render in normal flow and push content
  down. It floats again (`absolute left-0 top-full z-10`).
- **`border-radius` does not clip the video** (checked on the old pin). `rounded-lg` on the video
  container leaves the frame's own corners square.
- **BGRA frames skip the CPU entirely** — they upload straight into a
  `Bgra8Unorm` texture and the shader samples them as RGBA. Other formats
  (NV12, YUYV, MJPEG) still go through `cameras::to_rgba8` on the CPU.
- **The preview runs at whatever the camera delivers**, not at a renderer limit.
  The counters say which: `camera 30.0 fps -> preview 30.0 fps (0 dropped)` under
  `RUST_LOG=vdr=debug`. A low camera number is capture; a low preview number
  against a healthy camera number is the paint path.
- **Capture size is what makes multi-stream expensive.** Each paint uploads every
  visible stream in full, so capturing far above the size of the tile it fills is
  the cost that matters. Frames reaching the encoder are downscaled by an integer
  factor first (`camera/frames.rs`, `encode_size`); capture itself is still
  negotiated against a fixed request in `camera/hooks/choice.rs`.
