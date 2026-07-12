use crate::Theme;
use iced::{
    Background, Border,
    widget::text_input::{self, Status, Style},
};

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

impl text_input::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>, status: text_input::Status) -> text_input::Style {
        class(self, status)
    }
}

#[must_use]
pub fn default(theme: &Theme, status: Status) -> Style {
    let active = Style {
        background: Background::Color(theme.styles.general.background),
        border: Border {
            radius: 2.0.into(),
            width: 1.0,
            color: theme.styles.general.border,
        },
        icon: theme.styles.buttons.primary.text,
        // Muted so placeholders read as hints, not as entered values
        // (distinct from `value`, which uses the off-white button text).
        placeholder: theme.styles.text.normal.scale_alpha(0.4),
        value: theme.styles.buttons.primary.text,
        selection: theme.styles.general.accent,
    };

    match status {
        Status::Active => active,
        Status::Hovered => Style {
            border: Border {
                color: theme.styles.general.border,
                ..active.border
            },
            ..active
        },
        Status::Focused { .. } => Style {
            border: Border {
                color: theme.styles.general.accent,
                ..active.border
            },
            ..active
        },
        Status::Disabled => Style {
            background: Background::Color(theme.styles.general.overlay_background),
            value: active.placeholder,
            ..active
        },
    }
}

#[must_use]
pub fn borderless(theme: &Theme, status: Status) -> Style {
    let active = Style {
        background: Background::Color(theme.styles.general.input_background),
        border: Border {
            radius: 2.0.into(),
            ..Default::default()
        },
        icon: theme.styles.general.input_text,
        placeholder: theme.styles.general.input_text.scale_alpha(0.5),
        value: theme.styles.general.input_text,
        selection: theme.styles.general.accent,
    };

    match status {
        Status::Active => active,
        Status::Hovered => Style {
            border: Border {
                color: theme.styles.general.border,
                width: 1.0,
                ..active.border
            },
            ..active
        },
        Status::Focused { .. } => active,
        Status::Disabled => Style {
            background: Background::Color(theme.styles.general.overlay_background),
            value: active.placeholder,
            ..active
        },
    }
}
