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

    // Resolution only, never pixel format: `probe` reports *native* modes, and
    // the macOS backend delivers BGRA from a device that offers none. Filtering
    // on format here discards every resolution it has.
    let offered: Vec<Resolution> = capabilities
        .formats
        .iter()
        .map(|format| format.resolution)
        .collect();
    let Some(resolution) = pick_resolution(&offered, wanted.resolution) else {
        return wanted;
    };

    let ranges: Vec<(f64, f64)> = capabilities
        .formats
        .iter()
        .filter(|format| format.resolution == resolution)
        .map(|format| (format.framerate_range.min, format.framerate_range.max))
        .collect();
    let framerate = pick_framerate(&ranges, wanted.framerate);

    tracing::info!(
        "{}: asked {}x{}@{}, opening {}x{}@{}",
        device.name,
        wanted.resolution.width,
        wanted.resolution.height,
        wanted.framerate,
        resolution.width,
        resolution.height,
        framerate,
    );

    StreamConfig {
        resolution,
        framerate,
        pixel_format: wanted.pixel_format,
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
}
