//! Signal bars: a connection's quality as three small bars, drawn with divs.
//!
//! **The count of bars is the cue, not only the colour**: three for good,
//! two for held by the link, one for poor, and three hollow ones for unknown,
//! so the mark still reads for someone who does not tell red from green.
//!
//! Divs rather than an SVG, so nothing goes through usvg and the palette's
//! `oklch()` colours are fine here. Every class is a whole literal.

use dioxus_native::prelude::*;

use crate::net::quality::Quality;

#[component]
pub fn SignalBars(quality: Quality) -> Element {
    rsx! {
        div { class: "flex h-3 shrink-0 flex-row items-end gap-[2px]",
            div { class: bar(quality, 0) }
            div { class: bar(quality, 1) }
            div { class: bar(quality, 2) }
        }
    }
}

/// How many bars are lit for `quality`; unknown lights none.
pub(super) fn lit(quality: Quality) -> usize {
    match quality {
        Quality::Good => 3,
        Quality::Held => 2,
        Quality::Poor => 1,
        Quality::Unknown => 0,
    }
}

/// Bar `index` (0 the shortest) for `quality`.
fn bar(quality: Quality, index: usize) -> &'static str {
    const GOOD: [&str; 3] = [
        "h-[4px] w-[3px] rounded-[1px] bg-emerald-400",
        "h-[8px] w-[3px] rounded-[1px] bg-emerald-400",
        "h-[12px] w-[3px] rounded-[1px] bg-emerald-400",
    ];
    const HELD: [&str; 3] = [
        "h-[4px] w-[3px] rounded-[1px] bg-amber-400",
        "h-[8px] w-[3px] rounded-[1px] bg-amber-400",
        "h-[12px] w-[3px] rounded-[1px] bg-amber-400",
    ];
    const POOR: [&str; 3] = [
        "h-[4px] w-[3px] rounded-[1px] bg-red-500",
        "h-[8px] w-[3px] rounded-[1px] bg-red-500",
        "h-[12px] w-[3px] rounded-[1px] bg-red-500",
    ];
    const DARK: [&str; 3] = [
        "h-[4px] w-[3px] rounded-[1px] bg-neutral-600",
        "h-[8px] w-[3px] rounded-[1px] bg-neutral-600",
        "h-[12px] w-[3px] rounded-[1px] bg-neutral-600",
    ];
    const HOLLOW: [&str; 3] = [
        "h-[4px] w-[3px] rounded-[1px] border border-neutral-500",
        "h-[8px] w-[3px] rounded-[1px] border border-neutral-500",
        "h-[12px] w-[3px] rounded-[1px] border border-neutral-500",
    ];
    let index = index.min(2);
    let lit_set = match quality {
        Quality::Good => GOOD,
        Quality::Held => HELD,
        Quality::Poor => POOR,
        Quality::Unknown => return HOLLOW[index],
    };
    if index < lit(quality) { lit_set[index] } else { DARK[index] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_of_bars_is_the_cue() {
        assert_eq!([Quality::Good, Quality::Held, Quality::Poor, Quality::Unknown].map(lit), [3, 2, 1, 0]);
        assert!(bar(Quality::Held, 1).contains("amber"));
        assert!(bar(Quality::Held, 2).contains("neutral-600"));
        assert!(bar(Quality::Poor, 1).contains("neutral-600"));
        assert!(bar(Quality::Unknown, 2).contains("border"));
    }
}
