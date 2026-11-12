//! Pure geometry for the scaling pass.
//!
//! Nothing here touches the GPU, which is what makes it the one part of the
//! paint path that can be tested directly. These are also the calculations that
//! produced the crop bug.

/// Where the video sits in its canvas, and how much of its width shows.
///
/// **The card is the video's own shape, so there are no bars inside it.** Its
/// height fills the canvas, and its width follows the video:
///
/// - canvas wider than the video: the card is narrower than the canvas and
///   centred, with nothing cropped;
/// - canvas narrower: the card fills the width and the video's **sides are
///   cropped** to fit;
/// - canvas narrower than square: cropping stops at square, and the card is
///   `width × width`, centred vertically. Past that point, cropping further
///   would leave a sliver of face.
///
/// Everything outside the card is left unpainted, so the page shows through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Card {
    /// Offset of the card's top-left corner inside the canvas, in pixels.
    pub x: u32,
    pub y: u32,
    /// The card's size, in pixels.
    pub width: u32,
    pub height: u32,
    /// The fraction of the video's width that shows, at most 1. The shader
    /// reads it as a uv scale below 1, which crops evenly from both sides.
    pub crop_x: f32,
}

/// Lays out the card for `src` video in a `dst` canvas. See [`Card`].
pub(super) fn card(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Card {
    let whole = Card {
        x: 0,
        y: 0,
        width: dst_w,
        height: dst_h,
        crop_x: 1.0,
    };
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return whole;
    }
    let video = src_w as f64 / src_h as f64;
    let canvas = dst_w as f64 / dst_h as f64;

    if canvas >= video {
        // Wider than the video: narrow the card, crop nothing.
        let width = ((dst_h as f64 * video).round() as u32).clamp(1, dst_w);
        Card {
            x: (dst_w - width) / 2,
            width,
            ..whole
        }
    } else if canvas >= 1.0 {
        // Narrower than the video but not than square: crop the sides.
        Card {
            crop_x: (canvas / video) as f32,
            ..whole
        }
    } else {
        // Narrower than square: a square card, centred vertically.
        Card {
            y: (dst_h - dst_w) / 2,
            height: dst_w,
            crop_x: (1.0 / video) as f32,
            ..whole
        }
    }
}

/// How a stream fills its box.
///
/// A camera fills its card, its sides cropped as far as square ([`card`]). A
/// shared screen must never be cropped: text at its edge is still text someone
/// is reading ([`contain`]). Fixed for a stream's lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// A camera: see [`card`].
    #[default]
    Card,
    /// A shared screen: all of it, as large as fits.
    Contain,
}

/// Lays out `src` video whole inside a `dst` canvas: as large as fits both
/// axes, centred, nothing cropped. The bars either side are left unpainted, so
/// the page shows through, as [`card`] leaves its sides.
pub(super) fn contain(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Card {
    let whole = Card {
        x: 0,
        y: 0,
        width: dst_w,
        height: dst_h,
        crop_x: 1.0,
    };
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return whole;
    }
    let video = src_w as f64 / src_h as f64;
    let canvas = dst_w as f64 / dst_h as f64;
    if canvas >= video {
        // Wider than the video: bars left and right.
        let width = ((dst_h as f64 * video).round() as u32).clamp(1, dst_w);
        Card {
            x: (dst_w - width) / 2,
            width,
            ..whole
        }
    } else {
        // Taller than the video: bars above and below.
        let height = ((dst_w as f64 / video).round() as u32).clamp(1, dst_h);
        Card {
            y: (dst_h - height) / 2,
            height,
            ..whole
        }
    }
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

/// Whether to flip the video horizontally.
///
/// A presentation choice for the local self-view, not a property of the video.
/// People expect their own preview mirrored and nobody else's, and what we
/// encode and send must never be mirrored -- otherwise everyone sees everyone
/// backwards and any writing held up to a camera reads reversed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mirror {
    /// The local preview of your own camera.
    SelfView,
    /// Remote participants, screen shares, and anything bound for an encoder.
    None,
}

impl Mirror {
    /// `preview.wgsl` flips when this lands above 0.5 in `fit.z`.
    pub(super) fn as_uniform(self) -> f32 {
        match self {
            Self::SelfView => 1.0,
            Self::None => 0.0,
        }
    }
}

/// Packs the shader uniforms into the bytes `write_buffer` wants.
pub(super) fn uniform_bytes(values: [f32; 4]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values) {
        chunk.copy_from_slice(&value.to_ne_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tolerance for one f32 divide-then-multiply round trip.
    const EPS: f32 = 1e-6;

    #[test]
    fn card_in_a_wide_cell_is_narrower_and_centred() {
        // 16:9 video in a 1000x360 cell: 640 wide, centred, nothing cropped.
        let c = card(1280, 720, 1000, 360);
        assert_eq!((c.x, c.y, c.width, c.height), (180, 0, 640, 360));
        assert_eq!(c.crop_x, 1.0);
    }

    #[test]
    fn card_matching_the_video_fills_the_cell() {
        let c = card(1280, 720, 640, 360);
        assert_eq!((c.x, c.y, c.width, c.height), (0, 0, 640, 360));
        assert_eq!(c.crop_x, 1.0);
    }

    #[test]
    fn card_in_a_narrow_cell_crops_the_sides() {
        // 4:3 cell, 16:9 video: show 3/4 of the width.
        let c = card(1280, 720, 480, 360);
        assert_eq!((c.x, c.y, c.width, c.height), (0, 0, 480, 360));
        assert!((c.crop_x - 0.75).abs() < EPS);
    }

    #[test]
    fn card_at_exactly_square_crops_to_square() {
        let c = card(1280, 720, 360, 360);
        assert_eq!((c.x, c.y, c.width, c.height), (0, 0, 360, 360));
        assert!((c.crop_x - 0.5625).abs() < EPS);
    }

    #[test]
    fn card_never_goes_narrower_than_square() {
        // A tall cell: the card stays square and sits in the middle.
        let c = card(1280, 720, 300, 500);
        assert_eq!((c.x, c.y, c.width, c.height), (0, 100, 300, 300));
        assert!((c.crop_x - 0.5625).abs() < EPS, "cropped to square, no further");
    }

    #[test]
    fn card_survives_zero_dimensions() {
        let c = card(0, 720, 640, 360);
        assert_eq!((c.width, c.height, c.crop_x), (640, 360, 1.0));
        let c = card(1280, 720, 0, 360);
        assert_eq!(c.crop_x, 1.0);
    }

    #[test]
    fn contain_letterboxes_a_wide_source_in_a_square() {
        let c = contain(1920, 1080, 400, 400);
        assert_eq!((c.x, c.y, c.width, c.height), (0, 87, 400, 225));
        assert_eq!(c.crop_x, 1.0);
    }

    #[test]
    fn contain_pillarboxes_a_4_3_source_in_16_9() {
        let c = contain(1024, 768, 1280, 720);
        assert_eq!((c.x, c.y, c.width, c.height), (160, 0, 960, 720));
        assert_eq!(c.crop_x, 1.0);
    }

    #[test]
    fn contain_never_crops() {
        for (sw, sh, dw, dh) in [(1710, 1106, 300, 500), (1280, 720, 100, 1000), (800, 1200, 1600, 300), (16, 9, 16, 9)] {
            let c = contain(sw, sh, dw, dh);
            assert_eq!(c.crop_x, 1.0, "{sw}x{sh} in {dw}x{dh}");
            assert!(c.x + c.width <= dw && c.y + c.height <= dh, "{sw}x{sh} in {dw}x{dh}: {c:?}");
        }
    }

    #[test]
    fn contain_survives_zero_dimensions() {
        let c = contain(0, 720, 640, 360);
        assert_eq!((c.width, c.height, c.crop_x), (640, 360, 1.0));
        let c = contain(1280, 720, 0, 360);
        assert_eq!(c.crop_x, 1.0);
        let c = contain(1280, 720, 640, 0);
        assert_eq!(c.crop_x, 1.0);
    }

    #[test]
    fn row_stride_tightly_packed() {
        assert_eq!(row_stride(640 * 4 * 480, 640, 480), 2560);
    }

    #[test]
    fn row_stride_keeps_real_padding() {
        // The real stride has to reach wgpu, or every row after the first is
        // read at the wrong offset and the image shears.
        assert_eq!(row_stride(2600 * 480, 640, 480), 2600);
    }

    #[test]
    fn row_stride_falls_back_when_len_is_not_whole_rows() {
        // 1234567 is not divisible by 480, so no stride explains the buffer.
        assert_eq!(row_stride(1234567, 640, 480), 2560);
    }

    #[test]
    fn row_stride_falls_back_when_buffer_is_too_small() {
        // Divides evenly but is narrower than one packed row, so it cannot
        // be a stride.
        assert_eq!(row_stride(100 * 480, 640, 480), 2560);
    }

    #[test]
    fn mirror_matches_the_shader_threshold() {
        // `preview.wgsl` reads `if (uniforms.fit.z > 0.5)`. Pinned here because
        // the two halves of that contract live in different languages.
        assert!(Mirror::SelfView.as_uniform() > 0.5);
        assert!(Mirror::None.as_uniform() <= 0.5);
    }

    #[test]
    fn uniform_bytes_keeps_the_values_in_order() {
        // A transposition here is invisible until the picture is wrong, since
        // every component is a plausible float.
        let packed = uniform_bytes([1.0, 2.0, 3.0, 4.0]);
        for (index, value) in [1.0f32, 2.0, 3.0, 4.0].iter().enumerate() {
            let start = index * 4;
            assert_eq!(&packed[start..start + 4], &value.to_ne_bytes());
        }
    }

    #[test]
    fn row_stride_survives_zero_height() {
        assert_eq!(row_stride(0, 640, 0), 2560);
    }
}

