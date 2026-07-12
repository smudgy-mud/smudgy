//! Live, palette-derived colors for the Markdown widget.
//!
//! `smudgy_widgets` renders Markdown but cannot reach the active terminal color
//! scheme: that lives in `smudgy_ui::prefs`, which depends on this crate, not the
//! other way round. So the UI resolves the scheme's Markdown colors and pushes
//! them here with [`set`] on every prefs change (and at startup); the widget reads
//! them with [`current`] each time it renders, so Markdown tracks the scheme —
//! including light schemes and live theme switches — without a rebuild.
//!
//! The defaults match the stock dark scheme so a build that never calls [`set`]
//! (e.g. a headless test) still renders sensibly.

use std::sync::{Arc, LazyLock};

use arc_swap::ArcSwap;
use iced::Color;

/// The colors the Markdown widget paints with, all resolved from the active
/// terminal palette so prose, links, and code blocks stay coherent with the
/// terminal beside them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkdownColors {
    /// Plain body (and heading) text. Pinned to the terminal's default
    /// foreground so Markdown prose matches server text rather than the brighter
    /// chrome text color.
    pub body: Color,
    /// Link text. A distinct, readable accent: links render as "command chips"
    /// (this color + [`link_background`](Self::link_background) + a monospace
    /// font + an underline), a strong affordance for a keyboard-first client.
    pub link: Color,
    /// The chip fill behind a link. Kept translucent so it reads over any
    /// surface without having to track the background.
    pub link_background: Color,
    /// Inline-code and code-block background. Deliberately a dark grey panel,
    /// even under light schemes.
    pub code_background: Color,
    /// Inline-code and code-block text. A light grey that reads on
    /// [`code_background`](Self::code_background).
    pub code_foreground: Color,
}

impl Default for MarkdownColors {
    fn default() -> Self {
        let link = Color::from_rgb8(120, 200, 230);
        Self {
            body: Color::from_rgb8(204, 204, 204),
            link,
            link_background: Color { a: 0.14, ..link },
            code_background: Color::from_rgb8(34, 34, 34),
            code_foreground: Color::from_rgb8(208, 208, 208),
        }
    }
}

static COLORS: LazyLock<ArcSwap<MarkdownColors>> =
    LazyLock::new(|| ArcSwap::from_pointee(MarkdownColors::default()));

/// The current Markdown colors (lock-free).
#[must_use]
pub fn current() -> Arc<MarkdownColors> {
    COLORS.load_full()
}

/// Swaps in new Markdown colors; the next Markdown render picks them up.
pub fn set(colors: MarkdownColors) {
    COLORS.store(Arc::new(colors));
}
