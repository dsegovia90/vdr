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
  -> VideoPaintSource     src/camera/paint/      CustomPaintSource, uploads to wgpu
  -> vello                                       composites the texture
```

`use_wgpu` registers the paint source and returns a `u64`, which RSX references as
`canvas { "src": paint_source_id }`.

## Notes for future work

- **Why not `dioxus-cameras`.** It renders previews by running a loopback HTTP
  server and injecting a WebGL2 `<script>`. Blitz has no JS engine, so that path
  cannot work here. Its `Registry` also exposes no public frame *reader* — only
  `publish_frame` — so its frames are unreachable except over that HTTP server.
  We depend on the underlying `cameras` crate directly instead.
- **`wgpu` is 27**, fixed by the pinned blitz revision -> anyrender_vello 0.7.
  Textures are handed to vello, so both must be the same wgpu version.
- **No `<select>`.** Blitz ships no interactive select widget, hence
  `components/dropdown.rs`.
- **A canvas has zero intrinsic size** in blitz and its parent height reads as
  indefinite, so percentage heights collapse it to 0px and it is never painted.
  Size it with flex stretch (see `.preview` in `assets/styling/main.css`).
- **Tailwind v4 renders correctly**, including `@layer`, `@property`,
  `color-mix()` and `oklch()` colours — Blitz styles through Stylo, which is
  Firefox's engine, so the modern CSS surface is well covered.
- **Stacking contexts work.** `Node::is_stacking_context_root` implements the
  usual rules (opacity, position with a z-index, transforms), painting hoists
  negative and positive z-index children around the regular ones, and
  hit-testing consults the same hoisted set — so ordering and clicks agree. An
  earlier revision sorted `z-index` among direct siblings only, and that is what
  the z-index classes in `components/camera.rs` were written for.
- **An absolutely positioned overlay paints but never receives clicks** when it
  overflows its parent's box — `Node::hit` will not descend into it. This is why
  the dropdown menu renders in normal flow and pushes the content below it down
  while open, which looks like a bug and is not. Five alternatives were tried.
- **`border-radius` does not clip the canvas.** `rounded-lg` on the video
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
  negotiated against a fixed request in `components/camera.rs`.
