//! Pure geometry for the scaling pass.
//!
//! Nothing here touches the GPU, which is what makes it the one part of the
//! paint path that can be tested directly. These are also the calculations that
//! produced the crop bug.

/// uv scale that fits `src` inside `dst` while preserving aspect ratio.
///
/// One axis comes out at 1.0 and the other above it, which the shader reads as
/// "sample beyond the edge here", producing letterbox bars.
pub(super) fn contain(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> [f32; 2] {
    let (src_w, src_h) = (src_w as f32, src_h as f32);
    let (dst_w, dst_h) = (dst_w as f32, dst_h as f32);
    if src_w <= 0.0 || src_h <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
        return [1.0, 1.0];
    }
    let scale = (dst_w / src_w).min(dst_h / src_h);
    [dst_w / (src_w * scale), dst_h / (src_h * scale)]
}

/// Bytes per row of a tightly packed RGBA buffer, tolerating the row padding
/// `to_rgba8` leaves in place for frames that were already RGBA.
pub(super) fn row_stride(len: usize, width: u32, height: u32) -> u32 {
    let packed = width * 4;
    if height == 0 {
        return packed;
    }
    let stride = len / height as usize;
    if len.is_multiple_of(height as usize) && stride >= packed as usize {
        stride as u32
    } else {
        packed
    }
}

pub(super) fn bytemuck_cast(values: &[f32; 4]) -> &[u8] {
    // SAFETY: `f32` has no padding or invalid bit patterns, so any `[f32; 4]`
    // is valid to read as the same number of bytes.
    unsafe {
        std::slice::from_raw_parts(
            values.as_ptr().cast::<u8>(),
            std::mem::size_of::<[f32; 4]>(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tolerance for one f32 divide-then-multiply round trip.
    const EPS: f32 = 1e-6;

    fn assert_close(actual: [f32; 2], expected: [f32; 2]) {
        assert!(
            (actual[0] - expected[0]).abs() < EPS && (actual[1] - expected[1]).abs() < EPS,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn contain_matching_aspect_has_no_bars() {
        // Both sides exactly 16:9, so the video fills the canvas and neither
        // axis samples past the edge. Asserted exactly, not approximately:
        // the divide-then-multiply round trip does land on 1.0 for these, and
        // a change that only *nearly* does is worth failing on.
        for (dw, dh) in [(736, 414), (1280, 720), (160, 90), (1920, 1080)] {
            assert_eq!(contain(1920, 1080, dw, dh), [1.0, 1.0], "{dw}x{dh}");
        }
    }

    #[test]
    fn contain_wide_canvas_pillarboxes() {
        // Canvas is 2.00:1 against the video's 1.78:1, so the x axis samples
        // 12.5% beyond the texture and the bars land left and right.
        assert_close(contain(1920, 1080, 800, 400), [1.125, 1.0]);
    }

    #[test]
    fn contain_tall_canvas_letterboxes() {
        // The mirror image: a square canvas overshoots on y instead.
        assert_close(contain(1920, 1080, 400, 400), [1.0, 1920.0 / 1080.0]);
    }

    #[test]
    fn contain_pins_exactly_one_axis_to_one() {
        // The invariant the shader depends on. Both axes above 1.0 would mean
        // sampling past the edge on all four sides -- the crop bug's signature.
        for (sw, sh) in [(1920, 1080), (640, 480), (1080, 1920), (1, 1000)] {
            for (dw, dh) in [(736, 414), (800, 400), (400, 400), (200, 113), (1, 1)] {
                let [x, y] = contain(sw, sh, dw, dh);
                assert!(x >= 1.0 - EPS && y >= 1.0 - EPS, "{sw}x{sh} -> {dw}x{dh}");
                assert!(
                    (x - 1.0).abs() < EPS || (y - 1.0).abs() < EPS,
                    "neither axis pinned to 1.0 for {sw}x{sh} -> {dw}x{dh}: [{x}, {y}]"
                );
            }
        }
    }

    #[test]
    fn contain_survives_zero_dimensions() {
        // A canvas is zero-sized before first layout, and `render` is called
        // then. Dividing through would be a NaN uv scale, not a crash, which
        // is worse -- it paints nothing and explains nothing.
        for args in [(0, 1080, 736, 414), (1920, 0, 736, 414), (1920, 1080, 0, 414), (1920, 1080, 736, 0)] {
            let (sw, sh, dw, dh) = args;
            assert_eq!(contain(sw, sh, dw, dh), [1.0, 1.0], "{args:?}");
        }
    }

    #[test]
    fn row_stride_tightly_packed() {
        assert_eq!(row_stride(640 * 4 * 480, 640, 480), 2560);
    }

    #[test]
    fn row_stride_keeps_real_padding() {
        // Rows padded past width*4: the real stride has to reach wgpu, or
        // every row after the first is read at the wrong offset and the image
        // shears.
        assert_eq!(row_stride(2600 * 480, 640, 480), 2600);
    }

    #[test]
    fn row_stride_falls_back_when_len_is_not_whole_rows() {
        // 1234567 is not divisible by 480, so no stride explains the buffer.
        assert_eq!(row_stride(1234567, 640, 480), 2560);
    }

    #[test]
    fn row_stride_falls_back_when_buffer_is_too_small() {
        // Divides evenly but is narrower than one packed row, so it cannot be
        // a stride. `upload` rejects the frame afterwards on the length check.
        assert_eq!(row_stride(100 * 480, 640, 480), 2560);
    }

    #[test]
    fn row_stride_survives_zero_height() {
        assert_eq!(row_stride(0, 640, 0), 2560);
    }
}

