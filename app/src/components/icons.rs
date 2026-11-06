//! Small line icons for the control bar, drawn inline as SVG.
//!
//! Inline rather than as image files so they take `currentColor`: a button
//! recolours its icon by setting its own text colour, the same way it
//! recolours a label. 24-unit grid, 2-unit strokes, drawn at 18px.
//!
//! **Give the button a plain colour, not a Tailwind palette shade.** Blitz
//! swaps `currentColor` for the computed colour and hands the SVG to usvg,
//! and Tailwind v4's palette is `oklch()`, which that path drops: a button
//! with `text-neutral-100` showed no icon at all, while `text-white` did.

use dioxus_native::prelude::*;

/// A microphone; struck through while muted.
#[component]
pub fn MicIcon(off: bool) -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "18",
            height: "18",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            rect { x: "9", y: "2", width: "6", height: "12", rx: "3" }
            path { d: "M5 10v1a7 7 0 0 0 14 0v-1" }
            path { d: "M12 18v4" }
            if off {
                path { d: "M3 3l18 18" }
            }
        }
    }
}

/// A video camera; struck through while off.
#[component]
pub fn CameraIcon(off: bool) -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "18",
            height: "18",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            rect { x: "2", y: "6", width: "14", height: "12", rx: "2" }
            path { d: "M16 10l6-3v10l-6-3" }
            if off {
                path { d: "M3 3l18 18" }
            }
        }
    }
}

/// A door with an arrow leaving it.
#[component]
pub fn LeaveIcon() -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "18",
            height: "18",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" }
            path { d: "M16 17l5-5-5-5" }
            path { d: "M21 12H9" }
        }
    }
}

/// A head and shoulders: someone whose camera is off.
#[component]
pub fn PersonIcon() -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "32",
            height: "32",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.75",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            circle { cx: "12", cy: "8", r: "4" }
            path { d: "M4 21a8 8 0 0 1 16 0" }
        }
    }
}

/// A speaker with sound coming out; struck through while muted.
#[component]
pub fn SpeakerIcon(off: bool) -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "18",
            height: "18",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M11 5L6 9H2v6h4l5 4V5z" }
            path { d: "M15.5 8.5a5 5 0 0 1 0 7" }
            path { d: "M19 5a10 10 0 0 1 0 14" }
            if off {
                path { d: "M3 3l18 18" }
            }
        }
    }
}
