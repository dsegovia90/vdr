# vid's patch to moq-video 0.1.21

The crate as published on crates.io (MIT OR Apache-2.0, see `Cargo.toml`),
vendored unchanged in its own commit. Every change is in
`src/capture/pipewire.rs` and marked `vid patch`, so the diff against that
commit is the whole patch.

## The bug: Wayland screen share never starts on Hyprland

**Symptom.** Click Share on Hyprland (seen on Omarchy, 4K screen) and the log
repeats, forever, with no frame ever sent:

```
WARN moq_video::capture::pipewire: DMA-BUF chunk does not contain a complete frame required=33177600 available=9
```

**Cause.** A compositor cannot always know a DMA-BUF's byte size, and
xdg-desktop-portal-hyprland says so in its own source
(`src/portals/Screencopy.cpp`, master):

```cpp
spaData[plane].maxsize     = PBUFFER->size[plane];   // 0 for DMA-BUF
spaData[plane].chunk->size = PBUFFER->size[plane];
...
// clients have implemented to check chunk->size if the buffer is valid instead
// of using the flags. Until they are patched we should use some arbitrary value.
if (PBUFFER->isDMABUF && spaData[plane].chunk->size == 0) {
    spaData[plane].chunk->size = 9; // This was choosen by a fair d20.
}
```

It marks a bad frame with `SPA_CHUNK_FLAG_CORRUPTED` instead. moq-video's
process callback required `chunk.size >= frame size` for every DMA-BUF
(`dma_buf_chunk_contains`), so 9 bytes against a 33 MB frame dropped them all.

## The patch

For a DMA-BUF the chunk size is advisory. `dma_buf_chunk_contains` became
`dma_buf_maxsize_contains(required, maxsize)`: a frame is rejected here only
when the producer states a nonzero `maxsize` and the frame does not fit it.
The real checks are unchanged: `chunk_kind` (CORRUPTED and EMPTY flags) before
it, and the fd's own allocation size (`dma_buf_allocation_size`, `SEEK_END`)
after it. The chunk size is not used to read a DMA-BUF frame (the import and
the CPU download go by the layout, the offset and that allocation size), so a
placeholder cannot truncate anything. The warning now names the bound that
failed (`maxsize`).

## Verified

Unit tests beside upstream's in `pipewire.rs`: Hyprland's shape (`size` 9,
`maxsize` 0, 4K BGRx `required` 33177600) is accepted, a CORRUPTED chunk is
still rejected, and a real `maxsize` smaller than the frame still rejects it.
Run on Linux with `--features pipewire`; the Linux package builds with it.
Not yet seen working on a Hyprland machine.

## Leaving

Not fixed upstream as of 0.1.23 (2026-11-11): `pipewire.rs` is byte-identical
in 0.1.21, 0.1.22 and 0.1.23. Report it (moq-dev/moq, `rs/moq-video`) with the
xdph source as the reference:
<https://github.com/hyprwm/xdg-desktop-portal-hyprland/blob/master/src/portals/Screencopy.cpp>.
Drop the vendored copy and the `[patch.crates-io]` entry in `app/Cargo.toml`
once a release carries a fix.
