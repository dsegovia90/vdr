# vid's patch to blitz-dom v0.3.0-beta.4

blitz-dom at the git tag `v0.3.0-beta.4` (commit `92b11f1`), MIT OR Apache-2.0,
vendored unchanged in its own commit. The only other change in that commit is
`Cargo.toml`: blitz's workspace-inherited settings written out, with blitz's
own crates pointed at the same git tag so nothing is duplicated. Every code
change is marked `vid patch`.

## The fix: hit-testing a node that was just removed

`Node::hit_inner` walks two caches, the stacking context's hoisted children
and `paint_children`, which layout refreshes. Between a DOM change and the
next layout they can still name a removed node, and the walk unwrapped each
id (`Node::with` is `get(id).unwrap()`; `hoisted_child_position` indexes the
tree). A pointer event in that window panicked the UI thread:

- `node.rs:1472` (hoisted): picking a device in a menu removed its z-indexed
  layer and panel (vid ticket 42).
- `node.rs:1483` (`paint_children`): a tile appearing as someone joined, and
  Leave clearing every tile (vid 26, real call).

- `layout/paint_tree.rs` `hoisted_content_bbox`: the helper both walks
  call, indexing the same hoisted children (`tree[child.node_id]`). A peer
  leaving while sharing removed their two tiles; the next pointer move
  panicked here (2026-11-10, screen share live check).

All three now skip an id the tree no longer has. A node that is gone cannot be hit,
and the next layout refreshes the caches. vid's own workaround for the menu
(kept mounted, hidden) stays; it is harmless.

## Upstream has the same fix, unreleased

blitz `main` commit `f102663` (PR #1244, 2026-10-23, "Skip nodes dropped
since the last resolve during hit testing") changes the same three places
the same way. It is after `v0.3.0-beta.4` (2026-10-16), the newest
`blitz-dom` on crates.io as of 2026-11-10, and every `dioxus-native`
release pins beta.4 or older. Drop this vendoring and the `[patch]` in
`app/Cargo.toml` once dioxus-native takes a blitz-dom released after that
commit.
