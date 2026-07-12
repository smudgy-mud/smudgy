use iced::Color;

use super::Theme;

#[must_use]
pub fn secondary() -> Theme {
    let mut base = super::smudgy::smudgy();
    // The tool windows (Automations, Settings, Map/Script editors) share the
    // primary theme's darker surfaces — sampled from the mockup as rgb(15,15,14)
    // base / rgb(7,7,6) panels. Only the border stays distinct: a solid grey
    // that reads a touch crisper than the primary theme's near-invisible warm
    // hairline.
    base.styles.general.border = Color::from_rgb8(35, 35, 35);
    base
}
