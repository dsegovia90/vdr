use dioxus_native::use_wgpu;
use dioxus_native::prelude::*;

use crate::camera::{use_pipeline_cache, FrameSlot, Mirror, VideoPaintSource};

/// One video tile: whatever is publishing into `slot`, scaled to fit.
///
/// Deliberately knows nothing about cameras. A local capture fills the slot
/// today; a decoder will fill a remote participant's.
///
/// **One instance per stream, never a loop of hooks.** `use_wgpu` is a hook, so
/// calling it once per participant inside a `for` would make the hook count vary
/// with the list -- someone joins, every later hook reads the wrong slot. Each
/// stream gets its own component instance so its hook count is always one, and
/// `use_drop` inside `use_wgpu` cleans up when the instance goes.
///
/// Callers rendering several of these **must** key them by a stable participant
/// id. Without a key dioxus may reuse the wrong instance when the list
/// reorders, and two participants' video swap places.
#[derive(Clone, PartialEq, Props)]
pub struct ParticipantVideoProps {
    /// Identifies this stream in logs; several may run at once.
    pub label: String,
    /// Frames to render. One slot per stream.
    pub slot: FrameSlot,
    /// Self-view mirrors; everyone else must not.
    pub mirror: Mirror,
    /// Sizing and placement, which belong to whoever lays the tiles out.
    #[props(default)]
    pub class: String,
}

#[component]
pub fn ParticipantVideo(props: ParticipantVideoProps) -> Element {
    let cache = use_pipeline_cache();
    let paint_source_id = use_wgpu({
        let label = props.label.clone();
        let slot = props.slot.clone();
        let mirror = props.mirror;
        move || VideoPaintSource::new(label, slot, mirror, cache)
    });

    rsx! {
        // A canvas has no intrinsic size and blitz reads the parent height as
        // indefinite, so a percentage height would collapse it to 0px and it
        // would never be painted. Flex stretch sizes it without percentages.
        div { class: "flex aspect-video rounded-lg bg-black {props.class}",
            canvas { class: "flex-1 self-stretch", "src": paint_source_id }
        }
    }
}
