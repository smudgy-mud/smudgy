use crate::Theme;
use iced::Color;
use iced::widget::toggler;

impl toggler::Catalog for Theme {
    type Class<'a> = toggler::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>, status: toggler::Status) -> toggler::Style {
        class(self, status)
    }
}

#[must_use]
pub fn default(theme: &Theme, status: toggler::Status) -> toggler::Style {
    let (on, disabled) = match status {
        toggler::Status::Active { is_toggled } | toggler::Status::Hovered { is_toggled } => {
            (is_toggled, false)
        }
        toggler::Status::Disabled { is_toggled } => (is_toggled, true),
    };
    let alpha = if disabled { 0.45 } else { 1.0 };
    toggler::Style {
        background: (if on {
            theme.styles.general.accent
        } else {
            theme.styles.general.border
        })
        .scale_alpha(alpha)
        .into(),
        foreground: theme.styles.text.normal.scale_alpha(alpha).into(),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.14,
    }
}
