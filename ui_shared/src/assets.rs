//! Shared assets used by both native and browser main-window surfaces.

use iced::Font;
use std::sync::LazyLock;

pub const GEIST_BYTES: &[u8] = include_bytes!("../../assets/fonts/Geist[wght].ttf");
pub const GEIST_ITALIC_BYTES: &[u8] = include_bytes!("../../assets/fonts/Geist-Italic[wght].ttf");
pub const GEIST: Font = Font::with_name("Geist");
pub const GEIST_MONO_BYTES: &[u8] = include_bytes!("../../assets/fonts/GeistMono[wght].ttf");
pub const GEIST_MONO_ITALIC_BYTES: &[u8] =
    include_bytes!("../../assets/fonts/GeistMono-Italic[wght].ttf");
pub const GEIST_MONO: Font = Font::with_name("Geist Mono");
pub const BOOTSTRAP_ICONS_BYTES: &[u8] = include_bytes!("../../assets/fonts/bootstrap-icons.ttf");
pub const BOOTSTRAP_ICONS: Font = Font::with_name("bootstrap-icons");

pub const BARS_3_BYTES: &[u8] =
    include_bytes!("../../assets/heroicons/optimized/16/solid/bars-3.svg");

pub mod fonts {
    pub use super::{BOOTSTRAP_ICONS, GEIST as GEIST_VF, GEIST_MONO as GEIST_MONO_VF};
}

pub mod bootstrap_icons {
    pub const CHEVRON_DOWN: &str = "\u{F282}";
    pub const CHEVRON_UP: &str = "\u{F286}";
    pub const SEARCH: &str = "\u{F52A}";
}

pub mod hero_icons {
    use super::LazyLock;
    use iced::widget::svg;

    const EYE_BYTES: &[u8] = include_bytes!("../../assets/heroicons/optimized/16/solid/eye.svg");
    const EYE_SLASH_BYTES: &[u8] =
        include_bytes!("../../assets/heroicons/optimized/16/solid/eye-slash.svg");
    const X_MARK_BYTES: &[u8] =
        include_bytes!("../../assets/heroicons/optimized/16/solid/x-mark.svg");

    pub static EYE: LazyLock<svg::Handle> = LazyLock::new(|| svg::Handle::from_memory(EYE_BYTES));
    pub static EYE_SLASH: LazyLock<svg::Handle> =
        LazyLock::new(|| svg::Handle::from_memory(EYE_SLASH_BYTES));
    pub static X_MARK: LazyLock<svg::Handle> =
        LazyLock::new(|| svg::Handle::from_memory(X_MARK_BYTES));
}
