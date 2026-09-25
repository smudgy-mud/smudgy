//! Panic-safe CSS color parsing without a UI dependency.

use std::panic::{self, AssertUnwindSafe};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CssRgba {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: f32,
}

/// Parse a CSS color accepted by Smudgy's maps and OSC links. `color_art`
/// can panic on some syntactically valid inputs, so invalid colors always
/// resolve to `None` at this trust boundary.
#[must_use]
pub fn parse_css_color(color: &str) -> Option<CssRgba> {
    if color.is_empty() {
        return None;
    }
    if let Some(hex) = color.strip_prefix('#')
        && matches!(hex.len(), 4 | 8)
        && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        let (rgb, alpha) = hex.split_at(hex.len() - hex.len() / 4);
        let alpha = u8::from_str_radix(alpha, 16).ok()?;
        let alpha = if hex.len() == 4 { alpha * 17 } else { alpha };
        return Some(CssRgba {
            alpha: f32::from(alpha) / 255.0,
            ..parse_css_color(&format!("#{rgb}"))?
        });
    }
    let parsed = panic::catch_unwind(AssertUnwindSafe(|| color_art::Color::from_str(color).ok()))
        .ok()
        .flatten()?;
    #[allow(clippy::cast_possible_truncation)]
    let alpha = parsed.alpha() as f32;
    Some(CssRgba {
        red: parsed.red(),
        green: parsed.green(),
        blue: parsed.blue(),
        alpha,
    })
}
