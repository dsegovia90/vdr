//! Camera delivery rate against frames that actually reach the GPU.

/// Compares frames delivered by the camera against frames that actually reach
/// the GPU.
///
/// The difference is the interesting number: the renderer can only upload
/// inside `render()`, so if vello paints less often than the camera captures,
/// frames are dropped in `FrameSlot` and the two rates diverge.
pub(super) struct FrameRate {
    frames: u32,
    since: std::time::Instant,
    published_at_start: u64,
}

impl FrameRate {
    const REPORT_EVERY: u32 = 120;

    pub(super) fn new(published: u64) -> Self {
        Self {
            frames: 0,
            since: std::time::Instant::now(),
            published_at_start: published,
        }
    }

    pub(super) fn tick(&mut self, label: &str, published: u64) {
        self.frames += 1;
        if self.frames < Self::REPORT_EVERY {
            return;
        }
        let elapsed = self.since.elapsed().as_secs_f32();
        if elapsed > 0.0 {
            let captured = published.saturating_sub(self.published_at_start);
            tracing::debug!(
                "[{label}] camera {:.1} fps -> preview {:.1} fps ({} dropped)",
                captured as f32 / elapsed,
                self.frames as f32 / elapsed,
                captured.saturating_sub(self.frames as u64),
            );
        }
        *self = Self::new(published);
    }
}
