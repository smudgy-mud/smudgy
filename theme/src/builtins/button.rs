use crate::Theme;
use iced::{Border, Color, widget::button};

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, button::Status) -> button::Style + 'a>;

impl button::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(primary)
    }

    fn style(&self, class: &Self::Class<'_>, status: button::Status) -> button::Style {
        class(self, status)
    }
}

#[inline]
fn style(button_theme: &crate::Button, status: button::Status) -> button::Style {
    match status {
        button::Status::Active => button::Style {
            background: Some(button_theme.background),
            border: button_theme.border,
            text_color: button_theme.text,
            ..Default::default()
        },
        button::Status::Hovered => button::Style {
            background: Some(button_theme.background_hover),
            border: button_theme.border,
            text_color: button_theme.text,
            ..Default::default()
        },
        button::Status::Pressed => button::Style {
            background: Some(button_theme.background_pressed),
            border: button_theme.border,
            text_color: button_theme.text,
            ..Default::default()
        },
        button::Status::Disabled => button::Style {
            background: Some(button_theme.background.scale_alpha(0.4)),
            border: button_theme
                .border
                .color(button_theme.border.color.scale_alpha(0.4)),
            text_color: button_theme.text.scale_alpha(0.4),
            ..Default::default()
        },
    }
}

#[must_use]
pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    style(&theme.styles.buttons.primary, status)
}

#[must_use]
pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    style(&theme.styles.buttons.secondary, status)
}

#[must_use]
pub fn list_item(theme: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Active => button::Style {
            background: None,
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
        button::Status::Hovered => button::Style {
            background: Some(Color::from_rgba8(255, 255, 255, 0.1).into()),
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
        button::Status::Pressed => button::Style {
            background: None,
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
        button::Status::Disabled => button::Style {
            background: None,
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
    }
}

#[must_use]
pub fn list_item_selected(theme: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Active => button::Style {
            background: Some(Color::from_rgba8(255, 255, 255, 0.15).into()),
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
        button::Status::Hovered => button::Style {
            background: Some(Color::from_rgba8(255, 255, 255, 0.2).into()),
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
        button::Status::Pressed => button::Style {
            background: Some(Color::from_rgba8(255, 255, 255, 0.15).into()),
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
        button::Status::Disabled => button::Style {
            background: Some(Color::from_rgba8(255, 255, 255, 0.15).into()),
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
    }
}

/// Quiet menu-bar item for the main window toolbar: no chrome at rest, a
/// faint highlight on hover, text that brightens with interaction.
#[must_use]
pub fn toolbar(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: match status {
            button::Status::Hovered => Some(Color::from_rgba8(255, 255, 255, 0.06).into()),
            button::Status::Pressed => Some(Color::from_rgba8(255, 255, 255, 0.04).into()),
            _ => None,
        },
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        text_color: match status {
            button::Status::Active => theme.styles.text.normal.scale_alpha(0.65),
            button::Status::Hovered => theme.styles.text.normal.scale_alpha(0.95),
            button::Status::Pressed => theme.styles.text.normal.scale_alpha(0.8),
            button::Status::Disabled => theme.styles.text.normal.scale_alpha(0.25),
        },
        ..Default::default()
    }
}

/// Low-emphasis filled button: translucent fill with a hairline border.
/// Suits small inline actions (session reconnect, script-spawned overlay
/// buttons) that shouldn't shout like `primary`.
#[must_use]
pub fn subtle(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(
            match status {
                button::Status::Active => Color::from_rgba8(255, 255, 255, 0.06),
                button::Status::Hovered => Color::from_rgba8(255, 255, 255, 0.12),
                button::Status::Pressed => Color::from_rgba8(255, 255, 255, 0.04),
                button::Status::Disabled => Color::from_rgba8(255, 255, 255, 0.03),
            }
            .into(),
        ),
        border: Border {
            color: Color::from_rgba8(255, 255, 255, 0.12),
            width: 1.0,
            radius: 4.0.into(),
        },
        text_color: match status {
            button::Status::Active => theme.styles.text.normal.scale_alpha(0.85),
            button::Status::Hovered | button::Status::Pressed => theme.styles.text.normal,
            button::Status::Disabled => theme.styles.text.normal.scale_alpha(0.3),
        },
        ..Default::default()
    }
}

#[must_use]
pub fn link(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: match status {
            button::Status::Hovered => Some(Color::from_rgba8(255, 255, 255, 0.075).into()),
            _ => None,
        },
        border: Border::default(),
        text_color: match status {
            button::Status::Active => theme.styles.text.normal,
            button::Status::Hovered => theme.styles.text.normal.scale_alpha(0.8),
            button::Status::Pressed => theme.styles.text.normal.scale_alpha(0.6),
            button::Status::Disabled => theme.styles.text.normal.scale_alpha(0.2),
        },
        ..Default::default()
    }
}
