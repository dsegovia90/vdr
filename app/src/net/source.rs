//! Presenting the camera tap as something an encoder will accept.

use moq_media::format::{PixelFormat, VideoFormat, VideoFrame};
use moq_media::traits::VideoSource;

use crate::camera::FrameFeed;

/// A [`VideoSource`] over the pump's fan-out tap.
///
/// `moq-media`'s encoder pipeline pulls from a `VideoSource`, and its own
/// implementations (`rusty-capture`'s camera and screen capturers) each own the
/// device they read from. Ours does not own anything: the camera is opened and
/// closed by `camera::use_camera_stream` on its own terms, and this only reads
/// whatever that pump chose to hand over. So `start` and `stop` are no-ops --
/// there is nothing here to start.
///
/// The consequence worth naming: an idle camera is indistinguishable from a
/// slow one here, and that is correct. The pipeline treats an empty
/// `pop_frame` as "not yet" and backs off; only every tap being dropped ends
/// it. Camera-off is therefore a pause, not a teardown, and turning it back on
/// resumes the same encoder rather than renegotiating one.
pub struct TapSource {
    name: String,
    feed: FrameFeed,
    format: VideoFormat,
}

impl TapSource {
    /// Blocks until the first frame arrives, then describes the source from it.
    ///
    /// The format cannot be assumed from what we asked the camera for. On macOS
    /// the device overrides the resolution request outright (see
    /// `camera::format`), so a 1280x720 request routinely delivers 1920x1080 --
    /// and the encoder sizes itself off this. A frame is the only evidence.
    ///
    /// Returns `None` if every tap was dropped before a frame arrived, which is
    /// what happens when the app shuts down before a camera ever opened.
    ///
    /// That first frame is **discarded** rather than queued for the encoder.
    /// Minutes can pass between it and a peer subscribing, and the publish path
    /// anchors its pacing on the first frame it is given -- so encoding this one
    /// would peg the stream's clock to a moment long gone and stall it. One
    /// frame at startup is a cheap price for that.
    pub fn wait_for_first(name: impl Into<String>, mut feed: FrameFeed) -> Option<Self> {
        let first = feed.wait()?;
        let format = VideoFormat {
            pixel_format: pixel_format(&first),
            dimensions: first.dimensions,
        };
        Some(Self {
            name: name.into(),
            feed,
            format,
        })
    }
}

/// The frame's pixel format, as far as `VideoFormat` can describe it.
///
/// `VideoFormat` only carries a packed `PixelFormat`, so planar frames have no
/// honest answer here. Capture frames are always packed -- `camera::from_capture`
/// converts anything else to RGBA -- so the fallback is unreachable today, and
/// says RGBA because that is what that conversion produces.
fn pixel_format(frame: &VideoFrame) -> PixelFormat {
    match &frame.data {
        moq_media::format::FrameData::Packed { pixel_format, .. } => *pixel_format,
        _ => PixelFormat::Rgba,
    }
}

impl VideoSource for TapSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn format(&self) -> VideoFormat {
        self.format.clone()
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        self.feed.take()
    }

    fn start(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use super::*;
    use crate::camera::frame_tap;

    fn frame(width: u32, height: u32) -> VideoFrame {
        let data = Bytes::from(vec![0u8; (width * height * 4) as usize]);
        VideoFrame::new_packed(data, width, height, PixelFormat::Bgra, Duration::ZERO)
    }

    #[test]
    fn the_format_comes_from_the_frame_not_the_request() {
        // The macOS case: we ask for 1280x720 and the device delivers 1080p.
        let (tap, feed) = frame_tap();
        tap.publish(frame(1920, 1080));
        let source = TapSource::wait_for_first("test", feed).expect("a frame was published");
        assert_eq!(source.format().dimensions, [1920, 1080]);
        assert_eq!(source.format().pixel_format, PixelFormat::Bgra);
    }

    #[test]
    fn the_frame_the_format_was_read_from_is_not_encoded() {
        // It is however old the camera-to-peer gap made it, and the publish
        // path would anchor its pacing on that.
        let (tap, feed) = frame_tap();
        tap.publish(frame(64, 48));
        let mut source = TapSource::wait_for_first("test", feed).expect("a frame was published");
        assert!(source.pop_frame().unwrap().is_none(), "nothing queued");
    }

    #[test]
    fn a_backlog_collapses_to_its_newest_frame() {
        let (tap, feed) = frame_tap();
        tap.publish(frame(64, 48));
        let mut source = TapSource::wait_for_first("test", feed).expect("a frame was published");
        tap.publish(frame(64, 48));
        tap.publish(frame(64, 48));
        tap.publish(frame(320, 180));
        let popped = source.pop_frame().unwrap().expect("a frame is queued");
        assert_eq!(popped.dimensions, [320, 180], "the newest, not the oldest");
        assert!(
            source.pop_frame().unwrap().is_none(),
            "the rest are discarded"
        );
    }

    #[test]
    fn a_dropped_tap_ends_the_source() {
        let (tap, feed) = frame_tap();
        tap.publish(frame(64, 48));
        let mut source = TapSource::wait_for_first("test", feed).expect("a frame was published");
        tap.publish(frame(64, 48));
        drop(tap);
        // What is queued still comes out; the error only follows once there is
        // genuinely nothing left.
        assert!(source.pop_frame().unwrap().is_some());
        assert!(
            source.pop_frame().is_err(),
            "a closed tap must be an error, not an empty queue -- \
             the encoder pipeline only stops on the error"
        );
    }

    #[test]
    fn no_frame_ever_means_no_source() {
        let (tap, feed) = frame_tap();
        drop(tap);
        assert!(TapSource::wait_for_first("test", feed).is_none());
    }
}
