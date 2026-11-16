//! Display names: what people call themselves, and what we call them.
//!
//! **A name is a claim, not an identity.** Anyone can type any name, two people
//! can type the same one, and a peer can send whatever bytes it likes. So
//! everything stays keyed on the endpoint id, the name is only ever shown, and
//! [`clean`] runs on both sides of the wire: on ours so we send what we would
//! accept, and on theirs because a sender is never trusted to have done it.

/// The longest name kept, in `char`s. Long enough for a first and last name,
/// short enough for a tile's label on a small card.
pub const MAX: usize = 32;

/// How many characters of an endpoint id stand for it on screen.
const SHORT: usize = 8;

/// A name as it may be shown, or `None` for no name.
///
/// Whitespace of any kind (newlines and tabs included) becomes a single space
/// between words, other control characters are dropped, and so are the
/// direction overrides that could make a label read backwards or reorder the
/// text after it. Then it is cut at [`MAX`] characters -- `char`s, so a
/// multi-byte character is kept whole or not at all -- and trimmed again, so a
/// cut never leaves a trailing space. Nothing left is no name.
pub fn clean(raw: &str) -> Option<String> {
    let kept: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| !c.is_control() && !is_direction_override(c))
        .collect();
    let collapsed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = collapsed.chars().take(MAX).collect();
    let capped = capped.trim_end();
    (!capped.is_empty()).then(|| capped.to_string())
}

/// The bidirectional embedding, override and isolate controls, and the two
/// marks. Format characters rather than control characters, so
/// `char::is_control` lets them through.
fn is_direction_override(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Enough of an endpoint id to tell two people apart on screen and in a log,
/// without carrying 64 hex characters around.
pub fn short(id: &str) -> String {
    id.chars().take(SHORT).collect()
}

/// What to call someone: their name if they have one, else their short id.
pub fn label(name: Option<&str>, id: &str) -> String {
    match name {
        Some(name) => name.to_string(),
        None => short(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_is_kept() {
        assert_eq!(clean("Ada Lovelace").as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn whitespace_is_trimmed_and_collapsed() {
        assert_eq!(clean("  Ada \t\t  Lovelace  ").as_deref(), Some("Ada Lovelace"));
        assert_eq!(clean("Ada\nLovelace").as_deref(), Some("Ada Lovelace"));
        assert_eq!(clean("Ada\r\n\r\nLovelace\n").as_deref(), Some("Ada Lovelace"));
        // A no-break space is whitespace too.
        assert_eq!(clean("Ada\u{a0}\u{a0}L").as_deref(), Some("Ada L"));
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(clean("A\u{0}d\u{7}a\u{1b}[31m").as_deref(), Some("Ada[31m"));
        assert_eq!(clean("Ada\u{7f}\u{85}").as_deref(), Some("Ada"));
    }

    #[test]
    fn direction_overrides_are_dropped() {
        assert_eq!(clean("\u{202E}adA").as_deref(), Some("adA"));
        assert_eq!(clean("A\u{2066}d\u{2069}a\u{200F}").as_deref(), Some("Ada"));
    }

    #[test]
    fn nothing_left_is_no_name() {
        for raw in ["", "   ", "\n\t\r", "\u{0}\u{1}", "\u{202E}", " \u{7f} "] {
            assert_eq!(clean(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn the_cap_is_in_characters() {
        let long = "a".repeat(100);
        assert_eq!(clean(&long).map(|name| name.chars().count()), Some(MAX));
        let exact = "b".repeat(MAX);
        assert_eq!(clean(&exact).as_deref(), Some(exact.as_str()));
    }

    #[test]
    fn multi_byte_characters_are_never_split() {
        // Four bytes each: a byte cap at 32 would hold eight, a char cap 32.
        let emoji = "\u{1F600}".repeat(40);
        let cleaned = clean(&emoji).unwrap();
        assert_eq!(cleaned.chars().count(), MAX);
        assert!(cleaned.chars().all(|c| c == '\u{1F600}'));
        // Mixed widths across the cap.
        let mixed = format!("{}é\u{1F600}ü", "x".repeat(MAX - 2));
        assert_eq!(clean(&mixed).unwrap(), format!("{}é\u{1F600}", "x".repeat(MAX - 2)));
    }

    #[test]
    fn a_cut_never_leaves_a_trailing_space() {
        let name = format!("{} tail", "a".repeat(MAX - 1));
        assert_eq!(clean(&name).unwrap(), "a".repeat(MAX - 1));
    }

    #[test]
    fn whitespace_counts_after_collapsing() {
        // Runs of spaces collapse first, so they do not eat into the cap.
        let name = format!("Ada{}Lovelace", " ".repeat(100));
        assert_eq!(clean(&name).as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn cleaning_twice_changes_nothing() {
        for raw in ["  Ada\nL ", &"\u{1F600}".repeat(40), "x\u{202E}y", &"a ".repeat(40)] {
            let once = clean(raw);
            assert_eq!(once.as_deref().and_then(clean), once, "{raw:?}");
        }
    }

    #[test]
    fn a_name_wins_over_the_short_id() {
        let id = "3929461daf6e4354aaaa";
        assert_eq!(label(Some("Ada"), id), "Ada");
        assert_eq!(label(None, id), "3929461d");
    }

    #[test]
    fn a_short_id_is_eight_characters() {
        assert_eq!(short("3929461daf6e4354"), "3929461d");
        assert_eq!(short("abc"), "abc");
    }
}
