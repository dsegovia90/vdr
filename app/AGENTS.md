# vid / `vdr` — read this before changing dependencies

This file used to be generic Dioxus 0.7 boilerplate. Every dependency
instruction in it was wrong for this repo, and following it breaks the build, so
it has been replaced with a pointer.

**This is a peer-to-peer video calling app on `dioxus-native` (Blitz: Stylo for
CSS, Vello for painting). There is no webview and no JavaScript engine.**

Four facts that break the build if you get them wrong:

1. **There is no `dioxus` umbrella crate here, deliberately.** Depend on
   `dioxus-native`, pinned to a blitz git revision, and import from
   `dioxus_native::prelude` — never `dioxus::prelude`, `dioxus::launch`, or the
   `web` / `webview` / `server` features.
2. **The pin is load-bearing.** Blitz 0.2.x ships stylo 0.8, which fails to
   compile once `derive_more`'s features are unified into the graph, and both
   `hang` and `iroh` pull `derive_more` in on their own. Unpinning breaks the
   build outright.
3. **`wgpu` is 27**, fixed by that revision. Our textures are handed to vello, so
   the versions must match. Run `cargo tree -i wgpu` before touching any renderer
   dependency.
4. **Work from `app/`, and run with `dx`, not `cargo`.** `dx serve --renderer
   native`. The crate at the repo root is an unrelated `cargo new` stub, and
   `cargo run` comes up unstyled because `asset!` only resolves inside a dx
   bundle.

The full picture lives in two places:

- **`app/README.md`** — layout, the paint path, and notes on what this renderer
  does and does not support.
- **`CLAUDE.md` at the repo root** — build commands, the load-bearing pins, and
  the behaviours that look like bugs and are not. It is deliberately untracked,
  so a fresh clone will not have it.
