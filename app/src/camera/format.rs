//! Choosing what to ask the camera for.
//!
//! Cameras only offer discrete modes, so a requested resolution is a wish, not
//! an instruction. Probing turns it into a mode the device actually has.

use cameras::{CameraSource, Resolution, StreamConfig};

/// Narrows `wanted` to a mode the device actually offers.
///
/// Returns `wanted` unchanged if the device cannot be probed, which keeps the
/// old behaviour rather than failing to open.
pub fn negotiate(source: &CameraSource, wanted: StreamConfig) -> StreamConfig {
    // Irrefutable only because `cameras`' `rtsp` feature is off; enabling it
    // adds `Rtsp`, which has no `probe`.
    #[allow(irrefutable_let_patterns)]
    let CameraSource::Usb(device) = source
    else {
        return wanted;
    };

    let capabilities = match cameras::probe(device) {
        Ok(capabilities) => capabilities,
        Err(error) => {
            tracing::warn!("could not probe {}: {error}", device.name);
            return wanted;
        }
    };

    tracing::debug!(
        "{} offers {}",
        device.name,
        capabilities
            .formats
            .iter()
            .map(|f| format!(
                "{}x{} {:?} {:.0}-{:.0}fps",
                f.resolution.width,
                f.resolution.height,
                f.pixel_format,
                f.framerate_range.min,
                f.framerate_range.max
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Resolution is chosen across every format, never within one: `probe`
    // reports *native* modes, and the macOS backend delivers BGRA from a device
    // that offers none. Narrowing to one format first discards resolutions the
    // device really has.
    let offered: Vec<Resolution> = capabilities
        .formats
        .iter()
        .map(|format| format.resolution)
        .collect();
    let Some(resolution) = pick_resolution(&offered, wanted.resolution) else {
        return wanted;
    };

    // What each format can sustain at that resolution. Asking for a format the
    // device does not have is not a no-op: the driver substitutes one of its
    // own and we inherit *its* ceiling. Observed on v4l2 with a camera offering
    // YUYV 1280x720 at 10fps and MJPEG 1280x720 at 30 -- a request for BGRA
    // came back as YUYV, so the preview ran at 10fps while the negotiated
    // framerate said 30.
    let candidates: Vec<(cameras::PixelFormat, u32)> = capabilities
        .formats
        .iter()
        .filter(|format| format.resolution == resolution)
        .map(|format| {
            let range = (format.framerate_range.min, format.framerate_range.max);
            (format.pixel_format, pick_framerate(&[range], wanted.framerate))
        })
        .collect();

    let (pixel_format, framerate) =
        pick_format(&candidates, wanted.framerate).unwrap_or((wanted.pixel_format, wanted.framerate));

    tracing::info!(
        "{}: asked {}x{}@{} {:?}, opening {}x{}@{} {:?}",
        device.name,
        wanted.resolution.width,
        wanted.resolution.height,
        wanted.framerate,
        wanted.pixel_format,
        resolution.width,
        resolution.height,
        framerate,
        pixel_format,
    );

    StreamConfig {
        resolution,
        framerate,
        pixel_format,
    }
}

/// The format to open at the chosen resolution: falling short of the requested
/// framerate is what disqualifies a candidate first, then distance from the
/// request, then how cheap it is to get onto the GPU.
///
/// Rate beats cost because a format that cannot carry the rate is the defect
/// this function exists for, and no conversion saving makes up for a third of
/// the frames.
///
/// **Shortfall rather than distance**, which is where this parts company with
/// [`pick_framerate`]: 5fps is nearer to 30 than 60 is, and a 5fps preview is
/// useless where a 60fps one merely costs frames the tap then drops.
fn pick_format(candidates: &[(cameras::PixelFormat, u32)], want: u32) -> Option<(cameras::PixelFormat, u32)> {
    candidates.iter().copied().min_by_key(|(format, framerate)| {
        (
            want.saturating_sub(*framerate),
            framerate.abs_diff(want),
            cost(*format),
        )
    })
}

/// What a format costs us per frame, cheapest first.
///
/// `Bgra8` and `Rgba8` reach the texture untouched; everything else is
/// converted on the CPU by `frames::from_capture`, and MJPEG pays a JPEG
/// decode before that conversion.
///
/// **Deliberately not NV12 first.** NV12 is the better capture format on
/// paper, because it is cheaper to *encode* and uploads as planes rather than
/// 4 bytes per pixel. Neither saving exists yet: captured NV12 goes through the
/// same CPU conversion to RGBA as everything else, so preferring it here would
/// cost frames today for a benefit that needs an NV12 upload path. Revisit
/// when capture NV12 has a path that is not `to_rgba8`.
fn cost(format: cameras::PixelFormat) -> u8 {
    match format {
        cameras::PixelFormat::Bgra8 | cameras::PixelFormat::Rgba8 => 0,
        cameras::PixelFormat::Nv12 | cameras::PixelFormat::Yuyv | cameras::PixelFormat::Rgb8 => 1,
        cameras::PixelFormat::Mjpeg => 2,
        // `PixelFormat` is `#[non_exhaustive]`: an unknown format is usable,
        // since `to_rgba8` is the path for everything here, but it is the last
        // resort rather than a silent default.
        _ => 3,
    }
}

/// Smallest offered resolution that still covers `want` on both axes.
///
/// Not `cameras::best_format`: it minimises width+height delta, so it picks a
/// mode *below* the request whenever that is nearer. Never upscaling matters
/// more here than being nearest.
fn pick_resolution(offered: &[Resolution], want: Resolution) -> Option<Resolution> {
    let area = |r: &Resolution| (r.width as u64) * (r.height as u64);
    offered
        .iter()
        .filter(|r| r.width >= want.width && r.height >= want.height)
        .min_by_key(|r| area(r))
        .or_else(|| offered.iter().max_by_key(|r| area(r)))
        .copied()
}

/// Framerate to request at the chosen resolution: `want` if any offered range
/// covers it, otherwise the nearest bound.
///
/// Compared with a tolerance because backends derive these from frame
/// durations, so an integral 60fps arrives as 60.000000000000007.
fn pick_framerate(ranges: &[(f64, f64)], want: u32) -> u32 {
    const EPS: f64 = 1e-3;
    let target = want as f64;
    if ranges
        .iter()
        .any(|(min, max)| target >= min - EPS && target <= max + EPS)
    {
        return want;
    }
    ranges
        .iter()
        .flat_map(|(min, max)| [*min, *max])
        .map(|bound| bound.round().max(1.0) as u32)
        .min_by_key(|fps| fps.abs_diff(want))
        .unwrap_or(want)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(width: u32, height: u32) -> Resolution {
        Resolution { width, height }
    }

    #[test]
    fn picks_the_exact_mode_when_offered() {
        let offered = [res(640, 480), res(1280, 720), res(1920, 1080)];
        assert_eq!(pick_resolution(&offered, res(1280, 720)), Some(res(1280, 720)));
    }

    #[test]
    fn steps_up_rather_than_down() {
        // Nearer by raw delta, but upscaled -- which is what `best_format`
        // would have picked.
        let offered = [res(1024, 576), res(1920, 1080)];
        assert_eq!(pick_resolution(&offered, res(1280, 720)), Some(res(1920, 1080)));
    }

    #[test]
    fn picks_the_smallest_covering_mode_not_the_largest() {
        let offered = [res(1280, 720), res(1920, 1080), res(3840, 2160)];
        assert_eq!(pick_resolution(&offered, res(1280, 720)), Some(res(1280, 720)));
    }

    #[test]
    fn requires_both_axes_to_cover() {
        let offered = [res(1920, 480), res(1280, 960)];
        assert_eq!(pick_resolution(&offered, res(1280, 720)), Some(res(1280, 960)));
    }

    #[test]
    fn falls_back_to_the_largest_when_nothing_covers() {
        let offered = [res(320, 240), res(640, 480)];
        assert_eq!(pick_resolution(&offered, res(1280, 720)), Some(res(640, 480)));
    }

    #[test]
    fn handles_a_device_that_offers_nothing() {
        assert_eq!(pick_resolution(&[], res(1280, 720)), None);
    }

    #[test]
    fn framerate_is_kept_when_a_range_covers_it() {
        assert_eq!(pick_framerate(&[(5.0, 30.0)], 30), 30);
        assert_eq!(pick_framerate(&[(30.0, 30.0)], 30), 30);
    }

    #[test]
    fn framerate_tolerates_near_integral_bounds() {
        // The 61fps bug: an integral 60fps arrives a hair above 60.
        assert_eq!(pick_framerate(&[(60.0001, 60.0001)], 60), 60);
        assert_eq!(pick_framerate(&[(29.9999, 30.0001)], 30), 30);
    }

    #[test]
    fn framerate_falls_back_to_the_nearest_bound() {
        // 720p on the C922 is 60fps-only, so a request for 30 has to move.
        assert_eq!(pick_framerate(&[(60.0, 60.0)], 30), 60);
        assert_eq!(pick_framerate(&[(5.0, 10.0), (24.0, 30.0)], 15), 10);
    }

    #[test]
    fn framerate_survives_a_resolution_with_no_ranges() {
        assert_eq!(pick_framerate(&[], 30), 30);
    }

    use cameras::PixelFormat::{Bgra8, Mjpeg, Nv12, Yuyv};

    #[test]
    fn takes_the_format_that_carries_the_rate() {
        // The defect this was written for: a v4l2 camera whose 1280x720 YUYV
        // mode tops out at 10fps while its MJPEG mode does 30. Asking for BGRA
        // got YUYV's ceiling.
        let candidates = [(Yuyv, 10), (Mjpeg, 30)];
        assert_eq!(pick_format(&candidates, 30), Some((Mjpeg, 30)));
    }

    #[test]
    fn prefers_the_cheaper_format_at_equal_rates() {
        // A JPEG decode per frame is only worth paying for frames it buys.
        let candidates = [(Mjpeg, 30), (Bgra8, 30), (Nv12, 30)];
        assert_eq!(pick_format(&candidates, 30), Some((Bgra8, 30)));
    }

    #[test]
    fn falls_back_to_the_nearest_rate_when_nothing_reaches_it() {
        let candidates = [(Yuyv, 10), (Mjpeg, 15)];
        assert_eq!(pick_format(&candidates, 30), Some((Mjpeg, 15)));
    }

    #[test]
    fn overshooting_the_rate_beats_undershooting_it_by_more() {
        // 60fps-only modes exist (the C922 at 720p), and 60 is nearer 30 than
        // 5 is.
        let candidates = [(Yuyv, 5), (Mjpeg, 60)];
        assert_eq!(pick_format(&candidates, 30), Some((Mjpeg, 60)));
    }

    #[test]
    fn a_resolution_with_no_formats_leaves_the_choice_to_the_caller() {
        assert_eq!(pick_format(&[], 30), None);
    }
}
