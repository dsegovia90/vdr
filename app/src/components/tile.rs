//! One cell of the call grid: someone's video, and a line saying how it is.
//!
//! **The label is in normal flow under the video, not laid over it.** An
//! overlay would only need to paint, which absolutely-positioned content does
//! manage in this blitz revision -- but a row beneath costs nothing, cannot
//! cover the picture, and is one less thing that depends on the renderer.

use std::sync::Arc;
use std::time::Duration;

use dioxus_native::prelude::*;

use super::ParticipantVideo;
use crate::camera::{FrameSlot, Mirror};
use crate::channel::use_channel;
use crate::net::PeerStats;

/// How often a tile re-reads its numbers. Matches the stats row.
const POLL: Duration = Duration::from_millis(500);

/// What a tile's label shows, sampled so rendering never touches the atomics.
#[derive(Clone, Copy, PartialEq, Default)]
struct Reading {
    fps: Option<f32>,
    talking: bool,
}

#[derive(Clone, PartialEq, Props)]
pub struct ParticipantTileProps {
    /// Names the stream in logs, and is the label's text.
    pub label: String,
    /// Frames to render.
    pub slot: FrameSlot,
    /// Self-view mirrors; everyone else must not.
    pub mirror: Mirror,
    /// Their numbers, or `None` for our own preview, which has none worth a
    /// label -- the camera's own rate is on the status line below the grid.
    pub stats: Option<Arc<PeerStats>>,
}

#[component]
pub fn ParticipantTile(props: ParticipantTileProps) -> Element {
    let mut reading = use_signal(Reading::default);

    // A thread per tile, the stats row's pattern: it samples and sends, and it
    // ends on its own when the tile unmounts and the channel's receiver goes.
    // At most a handful of tiles, each asleep nearly all the time.
    let tx = use_channel(move |next: Reading| {
        if *reading.peek() != next {
            reading.set(next);
        }
    });
    use_hook({
        let stats = props.stats.clone();
        move || {
            let Some(stats) = stats else { return };
            let spawned = std::thread::Builder::new()
                .name("tile".into())
                .spawn(move || loop {
                    let next = Reading {
                        fps: stats.video_fps(),
                        talking: stats.talking(),
                    };
                    if tx.unbounded_send(next).is_err() {
                        return;
                    }
                    std::thread::sleep(POLL);
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn a tile's stats thread: {error}");
            }
        }
    });

    let now = reading();
    let fps = match now.fps {
        Some(fps) => format!(" \u{b7} {fps:.0} fps"),
        None => String::new(),
    };
    // A stalled stream is the thing this label exists to show, so zero is
    // coloured rather than merely printed.
    let stalled = now.fps.is_some_and(|fps| fps < 1.0);
    let text = if stalled {
        "text-xs text-amber-400"
    } else {
        "text-xs text-neutral-400"
    };

    rsx! {
        // `min-h-0 min-w-0` let a grid cell shrink below the canvas's own size;
        // without them a cell refuses to get smaller and the page scrolls.
        div { class: "flex flex-col gap-1 min-h-0 min-w-0",
            ParticipantVideo {
                label: props.label.clone(),
                slot: props.slot.clone(),
                mirror: props.mirror,
                class: "flex-1 min-h-0 min-w-0",
            }
            div { class: "flex flex-row items-center gap-2 shrink-0",
                // Always rendered, so the label does not shift sideways when
                // someone starts talking.
                div {
                    class: if now.talking { "h-2 w-2 shrink-0 rounded-full bg-emerald-400" } else { "h-2 w-2 shrink-0 rounded-full bg-neutral-700" },
                }
                span { class: text, "{props.label}{fps}" }
            }
        }
    }
}
