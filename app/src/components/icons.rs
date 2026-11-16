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

/// A head and shoulders: someone whose camera is off, and the settings'
/// Profile tab at the rail's 16 px.
#[component]
pub fn PersonIcon(#[props(default = 32)] size: u32) -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "{size}",
            height: "{size}",
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

/// A monitor; its screen filled in while we are sharing it.
#[component]
pub fn ScreenIcon(on: bool) -> Element {
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
            rect { x: "2", y: "3", width: "20", height: "14", rx: "2" }
            path { d: "M8 21h8" }
            path { d: "M12 17v4" }
            if on {
                rect { x: "5", y: "6", width: "14", height: "8", rx: "1", fill: "currentColor", stroke: "none" }
            }
        }
    }
}

/// A cog: the settings.
#[component]
pub fn GearIcon() -> Element {
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
            circle { cx: "12", cy: "12", r: "3" }
            path { d: "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" }
        }
    }
}

/// A keyboard: the shortcuts tab.
#[component]
pub fn KeyboardIcon() -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            rect { x: "2", y: "5", width: "20", height: "14", rx: "2" }
            path { d: "M6 9h.01M10 9h.01M14 9h.01M18 9h.01M6 13h.01M18 13h.01M8 16h8" }
            path { d: "M10 13h4" }
        }
    }
}

/// An "i" in a circle: the about tab.
#[component]
pub fn InfoIcon() -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            circle { cx: "12", cy: "12", r: "10" }
            path { d: "M12 16v-4" }
            path { d: "M12 8h.01" }
        }
    }
}

/// A cross: closes the settings window.
#[component]
pub fn CloseIcon() -> Element {
    rsx! {
        svg {
            // Needed: blitz hands the serialized subtree to usvg, which only
            // accepts a root in the SVG namespace.
            xmlns: "http://www.w3.org/2000/svg",
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M18 6 6 18" }
            path { d: "M6 6l12 12" }
        }
    }
}
