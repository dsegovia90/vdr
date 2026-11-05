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
    /// Size and placement, from `layout::tile_class`: the box *is* the card.
    pub class: &'static str,
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
        "whitespace-nowrap text-xs text-amber-400"
    } else {
        "whitespace-nowrap text-xs text-neutral-200"
    };

    rsx! {
        // **The box is the card.** It is sized by the layout to the video's
        // shape, so the label can sit on the picture itself and moves with it,
        // rather than under a cell the card may not fill.
        div { class: props.class,
            ParticipantVideo {
                label: props.label.clone(),
                slot: props.slot.clone(),
                mirror: props.mirror,
                class: "absolute inset-0",
            }
            // Over the video's bottom-left corner. It needs no clicks, so being
            // laid over the video costs nothing even where overlays misbehave.
            div { class: "absolute bottom-2 left-2 flex flex-row items-center gap-2 rounded-md bg-black/50 px-2 py-1",
                // Always rendered, so the label does not shift sideways when
                // someone starts talking.
                div {
                    class: if now.talking { "h-2 w-2 shrink-0 rounded-full bg-emerald-400" } else { "h-2 w-2 shrink-0 rounded-full bg-neutral-500" },
                }
                span { class: text, "{props.label}{fps}" }
            }
        }
    }
}
