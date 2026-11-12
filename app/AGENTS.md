# vdr — read this before changing dependencies

This file used to be generic Dioxus 0.7 boilerplate. Every dependency
instruction in it was wrong for this repo, and following it breaks the build, so
it has been replaced with a pointer.

**This is a peer-to-peer video calling app on `dioxus-native` (Blitz: Stylo for
CSS, Vello for painting). There is no webview and no JavaScript engine.**

Four facts that break the build if you get them wrong:

1. **There is no `dioxus` umbrella crate here, deliberately.** Depend on
   `dioxus-native`, pinned to a blitz git tag (`v0.3.0-beta.4`), and import from
   `dioxus_native::prelude` — never `dioxus::prelude`, `dioxus::launch`, or the
   `web` / `webview` / `server` features.
2. **The pin is load-bearing.** Blitz 0.2.x ships stylo 0.8, which fails to
   compile once `derive_more`'s features are unified into the graph, and both
   `hang` and `iroh` pull `derive_more` in on their own. Unpinning breaks the
   build outright.
3. **`wgpu` is 30**, fixed by that tag. Our textures are handed to the renderer,
   so the versions must match. Run `cargo tree -i wgpu` before touching any renderer
   dependency.
4. **Work from `app/`, and run with `dx`, not `cargo`.** `dx serve --renderer
   native`. `cargo run` comes up unstyled because `asset!` only resolves inside a dx
   bundle.

The full picture — layout, the paint path, and notes on what this renderer does
and does not support — is in **`app/README.md`**.
