//! Colours that more than one kind of drawing must agree on.
//!
//! **The host colour lives in two places that cannot share a variable**: the
//! Tailwind input (`tailwind.css`, `--color-host`), which gives the tile
//! borders their literal classes (`border-host`, `border-host/40`), and the
//! constant here, for inline SVG and styles, which get no Tailwind rule. A
//! test reads the Tailwind input and fails if the two drift, so changing the
//! colour is one edit in each, caught if either is forgotten.
//!
//! Plain sRGB hex, not a palette shade: inline SVG on this renderer needs a
//! plain colour (Tailwind v4's palette is `oklch()`).

/// Someone who can host, or carries the call: sky blue, which nothing else
/// on a tile uses (emerald is talking and a good link, amber a stall or a
/// weak link, red a muted microphone or a poor link).
pub const HOST: &str = "#38bdf8";

/// The quiet strength (can host, carries nothing for us), as an alpha over
/// [`HOST`]: the `/40` in `border-host/40`.
#[allow(dead_code)] // for inline SVG and styles, which nothing draws in it yet
pub const HOST_QUIET_ALPHA: f32 = 0.4;

/// A `#rrggbb` colour's three bytes, for painting it with something that
/// takes numbers rather than CSS (the room's map in the Topology tab).
pub fn rgb(hex: &str) -> Option<[u8; 3]> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value Tailwind's `@theme` gives `name`, read from the input file.
    fn theme_colour(css: &str, name: &str) -> Option<String> {
        let at = css.find(&format!("{name}:"))?;
        let rest = &css[at + name.len() + 1..];
        Some(rest[..rest.find(';')?].trim().to_string())
    }

    #[test]
    fn the_tailwind_theme_and_the_svg_constant_agree_on_the_host_colour() {
        let css = include_str!("../../tailwind.css");
        assert!(css.contains("@theme"), "the host colour is a theme token");
        assert_eq!(theme_colour(css, "--color-host").as_deref(), Some(HOST));
    }

    #[test]
    fn the_host_colour_reads_as_bytes_and_nonsense_does_not() {
        assert_eq!(rgb(HOST), Some([0x38, 0xbd, 0xf8]));
        for bad in ["38bdf8", "#38bdf", "#38bdf8ff", "#zzzzzz", "#\u{e9}\u{e9}\u{e9}"] {
            assert_eq!(rgb(bad), None, "{bad}");
        }
    }

    #[test]
    fn quiet_is_the_alpha_the_tiles_used_before() {
        // `#38bdf866`: 0x66 of 0xff is exactly 0.4.
        assert_eq!((HOST_QUIET_ALPHA * 255.0).round() as u32, 0x66);
    }
}
