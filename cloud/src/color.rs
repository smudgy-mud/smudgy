//! Panic-safe CSS color parsing.

use std::panic::{self, AssertUnwindSafe};
use std::str::FromStr;

/// Parses a CSS-style color string (`#rrggbb`, `rgb(..)`, `hsl(..)`, named
/// colors, ...) into an iced color, treating anything unparseable as
/// "not a color".
///
/// `color_art` panics on inputs its own validator accepts — e.g.
/// `hsl(360, 50%, 50%)`: hue 360 passes the inclusive range validation,
/// but the hue-sextant match in the conversion only covers `0..360` and
/// hits a bare `panic!()`. Color strings reach this parser from
/// keystroke-by-keystroke editor input and from cloud-synced data written
/// by other clients, so the parse runs under `catch_unwind` and a panic is
/// reported as `None` like any other parse failure.
#[must_use]
pub fn parse_css_color(color: &str) -> Option<iced::Color> {
    if color.is_empty() {
        return None;
    }

    let parsed = panic::catch_unwind(AssertUnwindSafe(|| {
        color_art::Color::from_str(color).ok()
    }))
    .ok()
    .flatten()?;

    #[allow(clippy::cast_possible_truncation)]
    let alpha = parsed.alpha() as f32;

    Some(iced::Color::from_rgba8(
        parsed.red(),
        parsed.green(),
        parsed.blue(),
        alpha,
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_css_color;

    #[test]
    fn parses_common_formats() {
        for input in [
            "#fff",
            "#ff0000",
            "#ff000080",
            "rgb(255, 0, 0)",
            "rgba(255, 0, 0, 0.5)",
            "hsl(120, 50%, 50%)",
            "deeppink",
        ] {
            assert!(parse_css_color(input).is_some(), "{input:?} should parse");
        }
    }

    #[test]
    fn rejects_garbage_cleanly() {
        for input in [
            "",
            "#",
            "#1",
            "#12345",
            "not a color",
            "rgb(",
            "rgb(1, 2)",
            "hsl(120, 50%)",
            "()",
            "%",
            "-",
        ] {
            assert!(parse_css_color(input).is_none(), "{input:?} should not parse");
        }
    }

    /// `color_art`'s validator accepts hue 360 but its conversions panic
    /// on it; the unwind guard must absorb that (and any similar edge).
    #[test]
    fn survives_color_art_panic_edges() {
        for input in [
            "hsl(360, 50%, 50%)",
            "hsv(360, 50%, 50%)",
            "hsi(360, 50%, 50%)",
            "hwb(360, 50%, 50%)",
        ] {
            let _ = parse_css_color(input);
        }
    }

    /// Editors feed every prefix of a color string through the parser as
    /// the user types; none of them may panic.
    #[test]
    fn survives_every_typing_prefix() {
        for target in [
            "#ff8800",
            "rgb(255, 128, 0)",
            "rgba(255, 128, 0, 0.5)",
            "hsl(360, 100%, 50%)",
            "hsv(360, 100%, 100%)",
            "hsi(360, 100%, 100%)",
            "deeppink",
        ] {
            for end in 1..=target.len() {
                let _ = parse_css_color(&target[..end]);
            }
        }
    }
}
