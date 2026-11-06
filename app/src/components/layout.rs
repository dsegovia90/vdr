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
//! **Grouped by subroom, the table holds per arrangement, not per count.** Each
//! subroom is its own run of rows under a label, so a group's last row can end
//! early: two in Room 1 and one in main is two rows at `xl`, where three
//! ungrouped tiles are one. The columns still come from the total, so every
//! card has the same size; the rows and labels are counted from the groups.
//! With one group and no labels this is exactly the table above.
//!
//! **Every class here is a whole literal.** Tailwind finds classes by scanning
//! the source, so one assembled with `format!` would never get a rule, and
//! would look exactly like a layout this renderer does not support. The
//! numbers that vary go through the container's inline `style` as custom
//! properties instead, which Stylo reads directly and [`TILE`] divides by.

use std::collections::BTreeMap;

use super::subroom::room_order;

/// The container the cards wrap inside.
pub(super) const CONTAINER: &str =
    "flex w-full min-h-0 flex-1 flex-row flex-wrap content-center justify-center gap-x-3";

/// Columns at each width: narrow, `md`, `xl`, from the total number of tiles.
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

/// Every card's classes, whatever the arrangement; [`sizing`] supplies the
/// numbers.
///
/// The height is one row's share of what the labels leave, less the 12px of
/// vertical margin each card carries (`my-1.5`, standing in for a row gap,
/// which a line break would otherwise double). The width follows from
/// `aspect-video`, capped at one column's share less the `gap-x-3`s, and a
/// capped card is narrower than 16:9: the video's sides are cropped to fit
/// (`camera/paint/fit.rs`).
pub(super) const TILE: &str = "relative my-1.5 aspect-video h-[calc((100%-var(--labels)*24px)/var(--rows-n)-12px)] md:h-[calc((100%-var(--labels)*24px)/var(--rows-md)-12px)] xl:h-[calc((100%-var(--labels)*24px)/var(--rows-xl)-12px)] max-w-[calc((100%-(var(--cols-n)-1)*12px)/var(--cols-n))] md:max-w-[calc((100%-(var(--cols-md)-1)*12px)/var(--cols-md))] xl:max-w-[calc((100%-(var(--cols-xl)-1)*12px)/var(--cols-xl))]";

/// A label row's height in pixels: `h-6`, and the `24px` in [`TILE`]. The
/// three must change together, or rows and labels stop adding up to the area
/// and `content-center` spills the excess past both edges. Only the tests
/// read it, to hold the two classes to it.
#[cfg(test)]
const LABEL_PX: usize = 24;

/// A subroom's label row, full width so it also ends the row before it.
/// Names come off the wire, hence `truncate`.
pub(super) const LABEL: &str = "basis-full h-6 truncate px-1 text-center text-xs leading-6 text-neutral-400";

/// The label of the group we are in, brighter, so you can see where you are.
pub(super) const LABEL_MINE: &str = "basis-full h-6 truncate px-1 text-center text-xs leading-6 text-neutral-200";

/// One subroom's tiles: `None` is the main room. `members` index the ids the
/// groups were built from.
#[derive(Debug, PartialEq)]
pub(super) struct Group {
    pub room: Option<String>,
    pub members: Vec<usize>,
}

/// The tiles in `ids`, grouped by subroom: the main room first, then the
/// subrooms in the menu's order. Inside a group the order of `ids` is kept.
///
/// Someone missing from `rooms` -- a tile that has not sampled yet -- is in
/// the main room. Groups are built from members, so none is ever empty.
pub(super) fn groups(ids: &[&str], rooms: &BTreeMap<String, Option<String>>) -> Vec<Group> {
    let mut main = Vec::new();
    let mut subrooms: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, id) in ids.iter().enumerate() {
        match rooms.get(*id).and_then(Option::as_deref) {
            Some(room) => subrooms.entry(room).or_default().push(index),
            None => main.push(index),
        }
    }
    let mut subrooms: Vec<_> = subrooms.into_iter().collect();
    subrooms.sort_by(|(a, _), (b, _)| room_order(a).cmp(&room_order(b)));
    let main = (!main.is_empty()).then_some(Group { room: None, members: main });
    main.into_iter()
        .chain(subrooms.into_iter().map(|(room, members)| Group {
            room: Some(room.to_string()),
            members,
        }))
        .collect()
}

/// Rows at each width, every group starting a row of its own.
pub(super) fn rows(groups: &[Group], cols: [usize; 3]) -> [usize; 3] {
    cols.map(|cols| groups.iter().map(|group| group.members.len().div_ceil(cols)).sum())
}

/// The container's inline `style`: the numbers [`TILE`] divides by.
pub(super) fn sizing(rows: [usize; 3], cols: [usize; 3], labels: usize) -> String {
    // Never zero: a division by zero would invalidate the declaration, and
    // the card would lose its size.
    let [rn, rm, rx] = rows.map(|n| n.max(1));
    let [cn, cm, cx] = cols.map(|n| n.max(1));
    format!(
        "--rows-n:{rn};--rows-md:{rm};--rows-xl:{rx};--cols-n:{cn};--cols-md:{cm};--cols-xl:{cx};--labels:{labels}"
    )
}

/// The line break that goes after the card at `position` (1-based) of a group
/// of `count`, if a row ends there at any width; the classes show it only at
/// the widths where it does. None after a group's last card: the next group's
/// label, or the end of the area, ends that row.
pub(super) fn break_after(cols: [usize; 3], count: usize, position: usize) -> Option<&'static str> {
    if position == 0 || position >= count {
        return None;
    }
    let [narrow, md, xl] = cols.map(|cols| position.is_multiple_of(cols));
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
    fn row_lengths(tiles: usize, width: usize) -> Vec<usize> {
        let cols = columns(tiles);
        let mut rows = vec![0];
        for position in 1..=tiles {
            *rows.last_mut().unwrap() += 1;
            if break_after(cols, tiles, position).is_some() && position.is_multiple_of(cols[width]) {
                rows.push(0);
            }
        }
        rows
    }

    fn map(entries: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        entries
            .iter()
            .map(|(id, room)| (id.to_string(), room.map(str::to_string)))
            .collect()
    }

    fn rooms_of(groups: &[Group]) -> Vec<Option<&str>> {
        groups.iter().map(|group| group.room.as_deref()).collect()
    }

    #[test]
    fn four_people_are_two_by_two_from_md_up() {
        assert_eq!(row_lengths(4, 1), vec![2, 2]);
        assert_eq!(row_lengths(4, 2), vec![2, 2]);
        assert_eq!(row_lengths(4, 0), vec![1, 1, 1, 1]);
    }

    #[test]
    fn a_short_last_row_is_just_shorter() {
        // Centred by `justify-center`, so nothing else is needed for it.
        assert_eq!(row_lengths(3, 1), vec![2, 1]);
        assert_eq!(row_lengths(5, 2), vec![3, 2]);
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
        let cols = columns(3);
        assert_eq!(break_after(cols, 3, 1), Some("basis-full h-0 md:hidden"));
        assert_eq!(break_after(cols, 3, 2), Some("basis-full h-0 xl:hidden"));
        // Never after the last card.
        assert_eq!(break_after(cols, 3, 3), None);
    }

    #[test]
    fn breaks_follow_the_global_columns_inside_a_group() {
        // Three tiles in all, two of them in a group: at xl the global three
        // columns mean no break inside the pair, at narrow one after the first.
        let cols = columns(3);
        assert_eq!(break_after(cols, 2, 1), Some("basis-full h-0 md:hidden"));
        assert_eq!(break_after(cols, 2, 2), None);
    }

    #[test]
    fn one_group_matches_the_old_table() {
        // Each card's height share, narrow / md / xl, copied from the per-count
        // class table this replaced, before it was deleted.
        let old: [(usize, [f64; 3]); 12] = [
            (1, [100.0, 100.0, 100.0]),
            (2, [50.0, 100.0, 100.0]),
            (3, [33.333, 50.0, 100.0]),
            (4, [25.0, 50.0, 50.0]),
            (5, [33.333, 33.333, 50.0]),
            (6, [33.333, 33.333, 50.0]),
            (7, [25.0, 33.333, 33.333]),
            (8, [25.0, 33.333, 33.333]),
            (9, [20.0, 33.333, 33.333]),
            (10, [25.0, 25.0, 25.0]),
            (11, [25.0, 25.0, 25.0]),
            (12, [25.0, 25.0, 25.0]),
        ];
        for (tiles, shares) in old {
            let ids: Vec<String> = (0..tiles).map(|i| format!("{i}")).collect();
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let groups = groups(&ids, &BTreeMap::new());
            assert_eq!(groups.len(), 1);
            let rows = rows(&groups, columns(tiles));
            for (width, share) in shares.into_iter().enumerate() {
                let derived = 100.0 / rows[width] as f64;
                assert!((derived - share).abs() < 0.01, "{tiles} tiles at width {width}: {derived} vs {share}");
            }
        }
    }

    #[test]
    fn main_comes_first_then_subrooms_by_number() {
        let rooms = map(&[
            ("a", Some("Room 10")),
            ("b", Some("Kitchen")),
            ("c", Some("Room 2")),
            ("d", None),
        ]);
        let groups = groups(&["a", "b", "c", "d"], &rooms);
        assert_eq!(rooms_of(&groups), [None, Some("Room 2"), Some("Room 10"), Some("Kitchen")]);
    }

    #[test]
    fn our_tile_is_in_our_group_not_first() {
        let rooms = map(&[("me", Some("Room 1")), ("p", None)]);
        let groups = groups(&["me", "p"], &rooms);
        assert_eq!(groups[0], Group { room: None, members: vec![1] });
        assert_eq!(groups[1], Group { room: Some("Room 1".into()), members: vec![0] });
    }

    #[test]
    fn arrival_order_is_kept_inside_a_group() {
        let rooms = map(&[("me", Some("Room 1")), ("x", Some("Room 1")), ("y", None), ("z", Some("Room 1"))]);
        let groups = groups(&["me", "z", "y", "x"], &rooms);
        assert_eq!(groups[1].members, [0, 1, 3]);
    }

    #[test]
    fn someone_not_yet_sampled_is_in_main() {
        let rooms = map(&[("a", Some("Room 1"))]);
        let groups = groups(&["a", "new"], &rooms);
        assert_eq!(groups[0], Group { room: None, members: vec![1] });
    }

    #[test]
    fn no_group_is_empty() {
        let rooms = map(&[("a", Some("Room 1")), ("b", Some("Room 1"))]);
        let groups = groups(&["a", "b"], &rooms);
        assert_eq!(rooms_of(&groups), [Some("Room 1")], "no main group when it has nobody");
        assert!(groups.iter().all(|group| !group.members.is_empty()));
    }

    #[test]
    fn a_group_boundary_adds_a_row() {
        // A and B in Room 1, C in main: at xl three columns, but two rows.
        let rooms = map(&[("a", Some("Room 1")), ("b", Some("Room 1")), ("c", None)]);
        let groups = groups(&["a", "b", "c"], &rooms);
        assert_eq!(rows(&groups, columns(3)), [3, 2, 2]);
    }

    #[test]
    fn the_label_height_is_the_one_the_tile_subtracts() {
        // `h-6` is 1.5rem, 24px at the root size.
        assert_eq!(LABEL_PX, 24);
        assert!(LABEL.contains("basis-full h-6 "));
        assert!(LABEL_MINE.contains("basis-full h-6 "));
        assert_eq!(TILE.matches(&format!("var(--labels)*{LABEL_PX}px")).count(), 3);
    }

    #[test]
    fn sizing_never_divides_by_zero() {
        assert_eq!(
            sizing([0, 2, 1], [1, 2, 0], 0),
            "--rows-n:1;--rows-md:2;--rows-xl:1;--cols-n:1;--cols-md:2;--cols-xl:1;--labels:0"
        );
    }
}
