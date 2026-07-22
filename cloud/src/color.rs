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

    // `color_art` accepts `#rgba`/`#rrggbbaa` but silently discards the alpha digits
    // (`parsed.alpha()` reports 1.0), so hex-with-alpha spellings peel the alpha off here
    // and delegate the plain-hex remainder.
    if let Some(hex) = color.strip_prefix('#')
        && matches!(hex.len(), 4 | 8)
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let (rgb, alpha) = hex.split_at(hex.len() - hex.len() / 4);
        let alpha = u8::from_str_radix(alpha, 16).ok()?;
        // A single alpha nibble expands like CSS short hex: `x` means `xx`.
        let alpha = if hex.len() == 4 { alpha * 17 } else { alpha };
        let base = parse_css_color(&format!("#{rgb}"))?;
        return Some(iced::Color {
            a: f32::from(alpha) / 255.0,
            ..base
        });
    }

    let parsed = panic::catch_unwind(AssertUnwindSafe(|| color_art::Color::from_str(color).ok()))
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

/// Parses any supported CSS spelling and returns the stable map-wire form:
/// uppercase `#RRGGBB` (or `#RRGGBBAA` when translucent). Empty input is a
/// caller-level reset and is therefore not accepted here.
#[must_use]
pub fn canonicalize_css_color(color: &str) -> Option<String> {
    let parsed = parse_css_color(color.trim())?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (r, g, b, a) = (
        channel(parsed.r),
        channel(parsed.g),
        channel(parsed.b),
        channel(parsed.a),
    );
    if a == u8::MAX {
        Some(format!("#{r:02X}{g:02X}{b:02X}"))
    } else {
        Some(format!("#{r:02X}{g:02X}{b:02X}{a:02X}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{canonicalize_css_color, parse_css_color};

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
            assert!(
                parse_css_color(input).is_none(),
                "{input:?} should not parse"
            );
        }
    }

    /// Hex-with-alpha spellings carry their alpha through (`color_art` alone
    /// drops it), and the short form expands nibbles like CSS.
    #[test]
    fn hex_alpha_is_preserved() {
        assert_eq!(parse_css_color("#ff000080").unwrap().a, 128.0 / 255.0);
        assert_eq!(parse_css_color("#ffd54a00").unwrap().a, 0.0);
        assert_eq!(parse_css_color("#ffd54aff").unwrap().a, 1.0);
        let short = parse_css_color("#f008").unwrap();
        assert_eq!(short.a, 136.0 / 255.0);
        assert_eq!((short.r, short.g, short.b), (1.0, 0.0, 0.0));
        // The rgb digits still parse as before.
        let opaque = parse_css_color("#ffd54a33").unwrap();
        let plain = parse_css_color("#ffd54a").unwrap();
        assert_eq!((opaque.r, opaque.g, opaque.b), (plain.r, plain.g, plain.b));
        assert_eq!(opaque.a, 51.0 / 255.0);
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

    #[test]
    fn canonicalizes_supported_spellings() {
        assert_eq!(canonicalize_css_color("red").as_deref(), Some("#FF0000"));
        assert_eq!(
            canonicalize_css_color("rgba(255, 0, 0, 0.5)").as_deref(),
            Some("#FF000080")
        );
    }
}
