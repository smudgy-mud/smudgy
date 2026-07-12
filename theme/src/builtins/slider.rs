use crate::Theme;
use iced::widget::slider;
use iced::{Background, Border, Color};

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, slider::Status) -> slider::Style + 'a>;

impl slider::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>, status: slider::Status) -> slider::Style {
        class(self, status)
    }
}

/// Nudges a color toward white by `amount` (0..=1) for hover/drag emphasis.
fn emphasize(color: Color, amount: f32) -> Color {
    Color {
        r: (1.0 - color.r).mul_add(amount, color.r),
        g: (1.0 - color.g).mul_add(amount, color.g),
        b: (1.0 - color.b).mul_add(amount, color.b),
        a: color.a,
    }
}

/// Accent-filled rail over the theme border color, with a circular accent
/// handle that brightens subtly on hover and drag.
#[must_use]
pub fn default(theme: &Theme, status: slider::Status) -> slider::Style {
    let accent = match status {
        slider::Status::Active => theme.styles.general.accent,
        slider::Status::Hovered => emphasize(theme.styles.general.accent, 0.15),
        slider::Status::Dragged => emphasize(theme.styles.general.accent, 0.3),
    };

    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(accent),
                Background::Color(theme.styles.general.border),
            ),
            width: 4.0,
            border: Border {
                radius: 2.0.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 6.0 },
            background: Background::Color(accent),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
        },
    }
}
