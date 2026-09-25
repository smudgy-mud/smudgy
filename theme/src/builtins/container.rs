use crate::Theme;
use iced::{Border, Color, border::Radius, widget::container};

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> container::Style + 'a>;

impl container::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>) -> container::Style {
        class(self)
    }
}

#[must_use]
pub fn default(_theme: &Theme) -> container::Style {
    container::Style {
        ..Default::default()
    }
}

#[must_use]
pub fn opaque(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.general.background.into()),
        ..Default::default()
    }
}

/// Corner radius of the main window's self-drawn frame (Linux Wayland),
/// matching the neighborhood of GNOME's headerbar rounding.
pub const WINDOW_CORNER_RADIUS: f32 = 12.0;

/// The main window's self-drawn frame on Linux Wayland: the window surface is
/// transparent and this container paints the actual window. Top corners are
/// rounded (the GNOME client-side-decoration convention); the bottom corners
/// stay square because pane content reaches the bottom edge and iced cannot
/// clip children to a rounded rect — the same shape GTK3 headerbar apps and
/// GNOME Terminal present. The 1px hairline stands in for a window border
/// where the compositor draws no frame of its own.
#[must_use]
pub fn window_frame(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.general.background.into()),
        border: Border {
            color: theme.styles.text.normal.scale_alpha(0.2),
            width: 1.0,
            radius: Radius {
                top_left: WINDOW_CORNER_RADIUS,
                top_right: WINDOW_CORNER_RADIUS,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
        },
        ..Default::default()
    }
}

#[must_use]
pub fn overlay(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(
            theme.styles.general.overlay_background,
        )),
        ..Default::default()
    }
}

/// [`overlay`] with the main-window frame's top rounding: the modal dim layer
/// of a window that draws its own rounded corners, so the dim fill doesn't
/// paint square pixels into the transparent region outside the corner arcs.
#[must_use]
pub fn overlay_rounded_top(theme: &Theme) -> container::Style {
    container::Style {
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: Radius {
                top_left: WINDOW_CORNER_RADIUS,
                top_right: WINDOW_CORNER_RADIUS,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
        },
        ..overlay(theme)
    }
}

/// The floating chip behind a script widget's text tooltip: the overlay surface with a
/// soft text-derived border, so tooltips read as the same material as the other floating
/// layers and follow theme remaps.
#[must_use]
pub fn tooltip(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(
            theme.styles.general.overlay_background,
        )),
        border: Border {
            color: theme.styles.text.normal.scale_alpha(0.25),
            width: 1.0,
            radius: Radius::from(4),
        },
        ..Default::default()
    }
}

#[must_use]
pub fn modal_container(theme: &Theme) -> container::Style {
    container::Style {
        shadow: theme.styles.modal.shadow,
        ..Default::default()
    }
}

#[must_use]
pub fn modal_title_bar(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.modal.title_bar_background),
        border: theme.styles.modal.title_bar_border.rounded(Radius {
            top_left: 5.0,
            top_right: 5.0,
            bottom_right: 0.0,
            bottom_left: 0.0,
        }),
        ..Default::default()
    }
}

#[must_use]
pub fn modal_body(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.modal.body_background),
        border: theme.styles.modal.body_border.rounded(Radius {
            top_left: 0.0,
            top_right: 0.0,
            bottom_right: 5.0,
            bottom_left: 5.0,
        }),
        ..Default::default()
    }
}

/// Inline purple notice strip — the single vehicle for inline warnings/disclosures
/// (e.g. the plain-text auto-login disclosure in the profile form). Reuses the
/// modal title bar's purple gradient, but rounded on all corners so it reads as a
/// standalone banner embedded in a form rather than a panel header.
#[must_use]
pub fn notice(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.modal.title_bar_background),
        border: theme.styles.modal.title_bar_border.rounded(4.0),
        ..Default::default()
    }
}

/// Pane title bar: a faint text-color tint that marks the drag-handle band
/// of a `pane_grid` pane without competing with the pane body. Derived from the
/// palette so it stays visible when the user remaps the theme colors.
#[must_use]
pub fn pane_title_bar(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.text.normal.scale_alpha(0.02).into()),
        ..Default::default()
    }
}

/// [`pane_title_bar`] for panes of the window's active session: the same
/// band with a stronger tint, so the active session reads at the header
/// (the pre-pane UI carried this distinction on the session tab).
#[must_use]
pub fn pane_title_bar_active(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.text.normal.scale_alpha(0.06).into()),
        ..Default::default()
    }
}

/// Veil over a pane the user toggled hidden while it still renders (the
/// toolbar is expanded, so the grid shows every pane): a translucent wash of
/// the window background that mutes the pane without erasing what it is.
#[must_use]
pub fn pane_hidden_overlay(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.general.background.scale_alpha(0.75).into()),
        ..Default::default()
    }
}

/// Muted circular chip that sits behind a single glyph in empty-state headers
/// (e.g. the connect/lightning glyph on the empty session view). A translucent
/// white fill with a large radius; pair it with a fixed square size to render a
/// circle.
#[must_use]
pub fn icon_chip(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgba8(255, 255, 255, 0.06).into()),
        border: Border {
            radius: Radius::from(100.0),
            ..Border::default()
        },
        ..Default::default()
    }
}

/// Opaque, fully-rounded card for standalone pop-ups (e.g. the upgrade prompt)
/// that place content directly in the container rather than filling it with the
/// opaque title-bar + body panels the Connect modal uses. Without an explicit
/// background the bare `modal_container` is transparent and the window shows
/// through, so this gives the card a solid surface, border, and shadow.
#[must_use]
pub fn modal_card(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.modal.body_background),
        border: theme.styles.modal.body_border.rounded(Radius {
            top_left: 6.0,
            top_right: 6.0,
            bottom_right: 6.0,
            bottom_left: 6.0,
        }),
        shadow: theme.styles.modal.shadow,
        ..Default::default()
    }
}
