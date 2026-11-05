//! How the call's video cards are arranged: rows that hug the middle.
//!
//! **Flex, not grid.** A grid gives every card a cell, and a card narrower than
//! its cell leaves a gap on both sides, so neighbours drift apart. In a
//! wrapping flex row centred with `justify-center`, each card is its own width
//! and the row closes up around the middle: small gaps between cards, the
//! spare space at the window's edges. A short last row centres itself too.
//!
//! What flex cannot do on its own is end a row after exactly N cards, so the
//! page puts a zero-height, full-width **line break** between cards wherever a
//! row should end. Which breaks show depends on the window's width, and is
//! decided by breakpoint classes, the only place the width is known.
//!
//! The table, by people on screen (you included) and window width:
//!
//! | Tiles | Narrow  | `md` (≥768 px) | `xl` (≥1280 px) |
//! |-------|---------|----------------|-----------------|
//! | 1     | 1       | 1              | 1               |
//! | 2     | 1       | 2              | 2               |
//! | 3     | 1       | 2              | 3               |
//! | 4     | 1       | 2              | 2 (2×2 beats 3+1) |
//! | 5–6   | 2       | 2              | 3               |
//! | 7–8   | 2       | 3              | 3               |
//! | 9     | 2       | 3              | 3               |
//! | 10+   | 3       | 3              | 3               |
//!
//! From five up there is never a single column: in a narrow window that would
//! stack them into slivers.
//!
//! **Every class here is a whole literal.** Tailwind finds classes by scanning
//! the source, so one assembled with `format!` would never get a rule, and
//! would look exactly like a layout this renderer does not support.

/// The container the cards wrap inside.
pub(super) const CONTAINER: &str =
    "flex w-full min-h-0 flex-1 flex-row flex-wrap content-center justify-center gap-x-3";

/// Columns at each width: narrow, `md`, `xl`. Must agree with [`tile_class`].
pub(super) fn columns(tiles: usize) -> [usize; 3] {
    match tiles {
        0 | 1 => [1, 1, 1],
        2 => [1, 2, 2],
        3 => [1, 2, 3],
        4 => [1, 2, 2],
        5 | 6 => [2, 2, 3],
        7..=9 => [2, 3, 3],
        _ => [3, 3, 3],
    }
}

/// Size classes for every card when there are `tiles` of them.
///
/// The height is one row's share of the area, less the 12px of vertical margin
/// each card carries (`my-1.5`, standing in for a row gap, which a line break
/// would otherwise double). The width follows from `aspect-video`, capped at
/// one column's share, and a capped card is narrower than 16:9: the video's
/// sides are cropped to fit (`camera/paint/fit.rs`).
pub(super) fn tile_class(tiles: usize) -> &'static str {
    match tiles {
        0 | 1 => "relative my-1.5 aspect-video h-[calc(100%-12px)] max-w-full",
        2 => {
            "relative my-1.5 aspect-video h-[calc(50%-12px)] max-w-full md:h-[calc(100%-12px)] md:max-w-[calc(50%-6px)]"
        }
        3 => {
            "relative my-1.5 aspect-video h-[calc(33.333%-12px)] max-w-full md:h-[calc(50%-12px)] md:max-w-[calc(50%-6px)] xl:h-[calc(100%-12px)] xl:max-w-[calc(33.333%-8px)]"
        }
        4 => {
            "relative my-1.5 aspect-video h-[calc(25%-12px)] max-w-full md:h-[calc(50%-12px)] md:max-w-[calc(50%-6px)]"
        }
        5 | 6 => {
            "relative my-1.5 aspect-video h-[calc(33.333%-12px)] max-w-[calc(50%-6px)] xl:h-[calc(50%-12px)] xl:max-w-[calc(33.333%-8px)]"
        }
        7 | 8 => {
            "relative my-1.5 aspect-video h-[calc(25%-12px)] max-w-[calc(50%-6px)] md:h-[calc(33.333%-12px)] md:max-w-[calc(33.333%-8px)]"
        }
        9 => {
            "relative my-1.5 aspect-video h-[calc(20%-12px)] max-w-[calc(50%-6px)] md:h-[calc(33.333%-12px)] md:max-w-[calc(33.333%-8px)]"
        }
        _ => "relative my-1.5 aspect-video h-[calc(25%-12px)] max-w-[calc(33.333%-8px)]",
    }
}

/// The line break that goes after the card at `position` (1-based), if a row
/// ends there at any width; the classes show it only at the widths where it
/// does.
pub(super) fn break_after(tiles: usize, position: usize) -> Option<&'static str> {
    if position == 0 || position >= tiles {
        return None;
    }
    let [narrow, md, xl] = columns(tiles).map(|cols| position.is_multiple_of(cols));
    match (narrow, md, xl) {
        (true, true, true) => Some("basis-full h-0"),
        (true, true, false) => Some("basis-full h-0 xl:hidden"),
        (true, false, true) => Some("basis-full h-0 md:hidden xl:block"),
        (true, false, false) => Some("basis-full h-0 md:hidden"),
        (false, true, true) => Some("hidden md:block basis-full h-0"),
        (false, true, false) => Some("hidden md:block xl:hidden basis-full h-0"),
        (false, false, true) => Some("hidden xl:block basis-full h-0"),
        (false, false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many cards land in each row at one width, given the breaks.
    fn rows(tiles: usize, width: usize) -> Vec<usize> {
        let cols = columns(tiles)[width];
        let mut rows = vec![0];
        for position in 1..=tiles {
            *rows.last_mut().unwrap() += 1;
            if position < tiles && position.is_multiple_of(cols) {
                rows.push(0);
            }
        }
        rows
    }

    #[test]
    fn four_people_are_two_by_two_from_md_up() {
        assert_eq!(rows(4, 1), vec![2, 2]);
        assert_eq!(rows(4, 2), vec![2, 2]);
        assert_eq!(rows(4, 0), vec![1, 1, 1, 1]);
    }

    #[test]
    fn a_short_last_row_is_just_shorter() {
        // Centred by `justify-center`, so nothing else is needed for it.
        assert_eq!(rows(3, 1), vec![2, 1]);
        assert_eq!(rows(5, 2), vec![3, 2]);
    }

    #[test]
    fn five_or_more_never_stack_into_one_column() {
        for tiles in 5..=12 {
            assert!(columns(tiles)[0] >= 2, "{tiles} tiles");
        }
    }

    #[test]
    fn breaks_show_exactly_where_a_row_ends() {
        // Three tiles: narrow breaks after 1 and 2, md after 2, xl never.
        assert_eq!(break_after(3, 1), Some("basis-full h-0 md:hidden"));
        assert_eq!(break_after(3, 2), Some("basis-full h-0 xl:hidden"));
        // Never after the last card.
        assert_eq!(break_after(3, 3), None);
    }

    #[test]
    fn the_tile_size_has_one_height_per_width_it_changes_at() {
        // A cheap guard that the two tables were edited together: a width
        // whose column count differs from the narrower one needs its own size.
        for tiles in 1..=12 {
            let [narrow, md, xl] = columns(tiles);
            let class = tile_class(tiles);
            assert_eq!(md != narrow, class.contains("md:h-"), "{tiles} tiles at md");
            assert_eq!(xl != md, class.contains("xl:h-"), "{tiles} tiles at xl");
        }
    }
}
