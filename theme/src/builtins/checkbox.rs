use crate::Theme;
use iced::widget::checkbox;
use iced::{Background, Border};

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, checkbox::Status) -> checkbox::Style + 'a>;

impl checkbox::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>, status: checkbox::Status) -> checkbox::Style {
        class(self, status)
    }
}

#[must_use]
pub fn default(theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let (is_checked, is_hovered) = match status {
        checkbox::Status::Active { is_checked } | checkbox::Status::Disabled { is_checked } => {
            (is_checked, false)
        }
        checkbox::Status::Hovered { is_checked } => (is_checked, true),
    };

    let background = if is_checked {
        Background::Color(theme.styles.general.accent)
    } else {
        Background::Color(theme.styles.general.container_background)
    };

    let border_color = if is_hovered {
        theme.styles.general.accent
    } else {
        theme.styles.general.border
    };

    checkbox::Style {
        background,
        icon_color: theme.styles.text.normal,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 2.0.into(),
        },
        text_color: Some(theme.styles.text.normal),
    }
}
