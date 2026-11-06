# vid's patch to moq-vaapi 0.1.1

The crate as published on crates.io (MIT OR Apache-2.0, see `Cargo.toml`),
vendored unchanged in its own commit. Every change is in `src/encode.rs` and
marked `vid patch`.

## The fix: a green band along the bottom of hardware-encoded video

**Seen:** Seb (Intel Panther Lake, iHD 26.3.5) encoding with VA-API showed a
green band across the full width of the bottom ~5% of his picture, to everyone
watching. It went away with `--encoder software` (2026-11-06).

**Cause (read from the source; the fix is not yet verified on his machine):**
H.264 codes whole 16x16 macroblocks, so 640x360 is coded as 640x368 and the
SPS crops the extra 8 rows back off (that part was right). But the encoder's
surfaces were allocated at 640x360, and the CPU upload wrote only 360 rows. The
driver's extra rows were never written: zeros, which are green in YUV.
Prediction and deblocking across the last macroblock row (rows 352–367) pulled
them into the visible bottom rows. That is about 16 of 360 rows, matching the
band's size.

**The fix, as ffmpeg's VA-API encoder does it:**
- The context, the reconstructed surfaces, and a new CPU-upload surface are
  allocated at the macroblock-aligned size.
- `upload_nv12` fills everything past the picture by repeating its last column
  and last row (luma, and UV pairs for chroma).
- The DMA-BUF path keeps its picture-sized `input` surface, since its video
  processor scales into whatever surface it is given. vid does not use that
  path.

## Leaving

Report upstream (moq-dev/moq, `rs/moq-vaapi`) with the before/after from a
Panther Lake machine. Drop this vendoring and the `[patch.crates-io]` entry
once a release carries a fix.
