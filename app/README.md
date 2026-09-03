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
- **`wgpu` is pinned to 26** to match dioxus-native 0.7.10 -> anyrender_vello 0.6.
  Textures are handed to vello, so both must be the same wgpu version.
- **No `<select>`.** Blitz ships no interactive select widget, hence
  `components/dropdown.rs`.
- **A canvas has zero intrinsic size** in blitz and its parent height reads as
  indefinite, so percentage heights collapse it to 0px and it is never painted.
  Size it with flex stretch (see `.preview` in `assets/styling/main.css`).
- **Tailwind v4 renders correctly**, including `@layer`, `@property`,
  `color-mix()` and `oklch()` colours — Blitz styles through Stylo, which is
  Firefox's engine, so the modern CSS surface is well covered.
- **There are no stacking contexts.** Blitz sorts `z-index` among direct
  siblings only (`blitz-dom` `layout/damage.rs`), so a `z-index` deep inside one
  subtree never competes with content in another. An overlay has to carry its
  z-index on whichever ancestor is a sibling of what it must cover — see the
  controls row in `components/camera.rs`. The same ordering drives hit-testing,
  so getting this wrong sends clicks to the wrong element too.
- **`border-radius` does not clip the canvas.** `rounded-lg` on the video
  container leaves the frame's own corners square.
- **BGRA frames skip the CPU entirely** — they upload straight into a
  `Bgra8Unorm` texture and the shader samples them as RGBA. Other formats
  (NV12, YUYV, MJPEG) still go through `cameras::to_rgba8` on the CPU.
- **The preview sits at ~24fps** at 1080p, and that is the *camera's* delivery
  rate, not a renderer limit. With one stream the counters read
  `camera 24.0 fps -> preview 24.0 fps (0 dropped)` — nothing is being dropped.
  Run with `RUST_LOG=vdr=debug` to see it.
- **Multi-stream is where it gets expensive.** Three streams roughly halves the
  paint rate: each paint does 3x (8MB upload + render pass + submit) at full
  1080p to fill ~700px tiles. Capturing nearer to display size is the fix — see
  `tasklist.md` task 15.
