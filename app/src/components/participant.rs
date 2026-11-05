use dioxus_native::CustomWidgetAttr;
use dioxus_native::prelude::*;

use crate::camera::{use_pipeline_cache, FrameSlot, Mirror, VideoPaintSource};

/// One video tile: whatever is publishing into `slot`, scaled to fit.
///
/// Deliberately knows nothing about cameras. A local capture fills the slot
/// today; a decoder will fill a remote participant's.
///
/// **One instance per stream, never a loop of hooks.** The widget is made in a
/// hook, so making one per participant inside a `for` would make the hook count
/// vary with the list -- someone joins, every later hook reads the wrong slot.
/// Each stream gets its own component instance so its hook count is always one.
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
    let widget = use_hook({
        let label = props.label.clone();
        let slot = props.slot.clone();
        let mirror = props.mirror;
        move || CustomWidgetAttr::new(VideoPaintSource::new(label, slot, mirror, cache))
    });

    rsx! {
        // **The video is taken out of layout, deliberately.** As an ordinary
        // flex child its own pixel size counted toward the box it sits in, so
        // in a grid that shares out space a resize fed back into the layout and
        // resized it again -- every frame, without settling (hundreds of
        // resizes in ten seconds, sizes swinging by a third). Positioned
        // absolutely over its box it can only ever take the size it is given.
        //
        // No aspect ratio of its own either: the video is letterboxed inside
        // the canvas (`fit::contain`), so the box can be whatever shape its
        // parent gives it -- which is what lets a grid cell decide.
        //
        // An `object` carrying a widget, which is what replaced `canvas` +
        // `CustomPaintSource` in blitz beta.4. Found on the old pin; kept on the
        // new one until it is shown to be unnecessary there.
        // Positioned by the caller (`absolute inset-0` inside a tile), which also
        // makes it the containing block for the video below. Not `relative`
        // here: two `position` classes on one element, and Tailwind's order
        // would quietly pick `relative`.
        div { class: "{props.class}",
            object { class: "absolute inset-0", "data": widget.clone() }
        }
    }
}
