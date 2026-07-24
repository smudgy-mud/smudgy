//! Shared visual building blocks for the Automations window: the status dot,
//! the consistent pill enable-switch, badges, cards, and the toast.

use iced::alignment::Vertical;
use iced::gradient::Linear;
use iced::widget::{column, container, mouse_area, row, text};
use iced::{Background, Border, Color, Gradient, Length, Padding};

use crate::assets::fonts;
use crate::theme::{Element as ThemedElement, Theme};

use super::Message;
use super::model::NodeStatus;

// ---- Text color styles (usable directly as `.style(..)` closures) ----------

pub fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.55)),
    }
}

pub fn faint(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.38)),
    }
}

pub fn regular(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal),
    }
}

pub fn accent(theme: &Theme) -> text::Style {
    // The accent purple reads dark on the surface; lift it for legible accents.
    let a = theme.styles.general.accent;
    text::Style {
        color: Some(lighten(a, 0.45)),
    }
}

pub fn success(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.success),
    }
}

pub fn danger(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.error),
    }
}

/// Amber "needs attention, not broken" text — matches the [`NodeStatus::Warning`] dot.
pub fn warning(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(status_color(theme, NodeStatus::Warning)),
    }
}

/// The color of a status dot for `status`.
pub fn status_color(theme: &Theme, status: NodeStatus) -> Color {
    match status {
        NodeStatus::Ok => theme.styles.text.success,
        NodeStatus::Error => theme.styles.text.error,
        // The theme has no dedicated warn slot; a warm amber reads as "attention, not broken"
        // against both light and dark surfaces.
        NodeStatus::Warning => Color::from_rgb8(0xE0, 0x8A, 0x1E),
        NodeStatus::Disabled => theme.styles.text.normal.scale_alpha(0.4),
    }
}

/// A small colored status dot (green/red/grey) — used everywhere a node appears.
///
/// Drawn as a real rounded container rather than a font glyph so it is pixel-
/// crisp, perfectly round, and consistently sized/aligned across every row
/// (a `●` glyph rendered at different sizes by the body font is neither). The
/// fill is the status color with a darker same-hue stroke, giving a lit-core-
/// with-bezel "LED" look rather than a flat disc.
pub fn status_dot<'a>(status: NodeStatus) -> ThemedElement<'a, Message> {
    container(text(""))
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |theme: &Theme| {
            let fill = status_color(theme, status);
            container::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    color: darken(fill, 0.75),
                    width: 2.0,
                    radius: 5.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

// ---- The consistent enable pill-switch --------------------------------------

fn track_style(enabled: bool, locked: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme: &Theme| {
        let on = theme.styles.general.accent;
        let bg = if enabled {
            if locked {
                lighten(on, 0.1)
            } else {
                lighten(on, 0.25)
            }
        } else {
            theme.styles.text.normal.scale_alpha(0.18)
        };
        container::Style {
            background: Some(Background::Color(bg)),
            border: Border::default().rounded(11.0),
            ..Default::default()
        }
    }
}

fn knob_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.styles.text.normal)),
        border: Border::default().rounded(8.0),
        ..Default::default()
    }
}

/// The word `Enabled`/`Disabled` + a pill switch. Interactive when `on_toggle`
/// is `Some` and not `locked`; otherwise rendered non-interactive.
pub fn pill_switch<'a>(
    enabled: bool,
    locked: bool,
    on_toggle: Option<Message>,
) -> ThemedElement<'a, Message> {
    let knob = container(text(""))
        .width(Length::Fixed(16.0))
        .height(Length::Fixed(16.0))
        .style(knob_style);
    let inner: ThemedElement<'a, Message> = if enabled {
        row![iced::widget::space::horizontal(), knob]
            .align_y(Vertical::Center)
            .into()
    } else {
        row![knob, iced::widget::space::horizontal()]
            .align_y(Vertical::Center)
            .into()
    };
    let track = container(inner)
        .width(Length::Fixed(40.0))
        .height(Length::Fixed(22.0))
        .padding(3)
        .style(track_style(enabled, locked));

    let label = if enabled {
        crate::i18n::ts!("state-enabled")
    } else {
        crate::i18n::ts!("state-disabled")
    };
    let body = row![text(label).size(13.0).style(muted), track]
        .spacing(8.0)
        .align_y(Vertical::Center);

    match on_toggle {
        Some(msg) if !locked => mouse_area(body).on_press(msg).into(),
        _ => body.into(),
    }
}

// ---- Badges, tags, cards ---------------------------------------------------

fn outline_box_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(
            theme.styles.text.normal.scale_alpha(0.04),
        )),
        border: Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    }
}

/// A small outlined pill badge (type badge, Public/Private, etc.).
pub fn badge<'a>(label: impl Into<String>) -> ThemedElement<'a, Message> {
    container(text(label.into()).size(11.0).style(muted))
        .padding(Padding {
            top: 2.0,
            bottom: 2.0,
            left: 8.0,
            right: 8.0,
        })
        .style(outline_box_style)
        .into()
}

/// The small `DEP` tag shown on nested dependency rows.
pub fn dep_tag<'a>() -> ThemedElement<'a, Message> {
    container(text(crate::i18n::t!("badge-dependency")).size(9.0).style(faint))
        .padding(Padding {
            top: 1.0,
            bottom: 1.0,
            left: 5.0,
            right: 5.0,
        })
        .style(|theme: &Theme| container::Style {
            background: None,
            border: Border {
                color: theme.styles.general.border,
                width: 1.0,
                radius: 3.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// A raised surface card (stat cards, panels).
pub fn card_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.styles.general.container_background)),
        border: Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// The surface used for inset code/source blocks.
pub fn code_surface_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.styles.general.container_background)),
        border: Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

/// A subtle bordered banner (context/safety banners in package panes).
pub fn banner_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(
            theme.styles.text.normal.scale_alpha(0.04),
        )),
        border: Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

// ---- Toast -----------------------------------------------------------------

fn toast_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.styles.modal.body_background),
        border: theme.styles.modal.body_border,
        shadow: theme.styles.modal.shadow,
        ..Default::default()
    }
}

/// The bottom-center toast pill (check glyph + message).
pub fn toast<'a>(message: &str) -> ThemedElement<'a, Message> {
    let pill = container(
        row![
            text("\u{2713}").size(13.0).style(success),
            text(message.to_string()).size(13.0),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center),
    )
    .padding(Padding {
        top: 8.0,
        bottom: 8.0,
        left: 16.0,
        right: 16.0,
    })
    .style(toast_style);

    container(column![iced::widget::space::vertical(), pill].align_x(iced::Alignment::Center))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .into()
}

// ---- helpers ---------------------------------------------------------------

/// The platform-appropriate label for the command-palette shortcut. The binding
/// is `modifiers.command()` + `P` — `Ctrl` on Windows/Linux, `⌘` on macOS — so
/// each platform sees its own key cap.
pub fn palette_shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "\u{2318}P"
    } else {
        "Ctrl+P"
    }
}

/// A quiet section label (small bold, muted).
pub fn section_label<'a>(label: &str) -> ThemedElement<'a, Message> {
    text(label.to_uppercase())
        .size(11.0)
        .font(fonts::GEIST_VF)
        .style(faint)
        .into()
}

/// Lightens a color toward white by `t` (0..1).
pub fn lighten(c: Color, t: f32) -> Color {
    Color {
        r: c.r + (1.0 - c.r) * t,
        g: c.g + (1.0 - c.g) * t,
        b: c.b + (1.0 - c.b) * t,
        a: c.a,
    }
}

/// Darkens a color toward black by `t` (0..1), preserving alpha. Used for the
/// status dot's dimmer same-hue bezel.
pub fn darken(c: Color, t: f32) -> Color {
    Color {
        r: c.r * (1.0 - t),
        g: c.g * (1.0 - t),
        b: c.b * (1.0 - t),
        a: c.a,
    }
}

/// Blends `a` toward `b` by `t` (0..1), keeping `a`'s alpha. Unlike [`lighten`]
/// (which only moves toward white), this mixes toward an arbitrary color — used
/// to tint the dark surfaces toward the accent for the top-of-view glow.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a,
    }
}

/// A short top-down highlight: the translucent `highlight` color composited over
/// `base` at the very top, fading back to `base` within the top ~6% of the
/// bounds. `iced`'s gradient offset 0.0 sits at the *bottom* for `Linear::new(0)`,
/// so this uses `Linear::new(π)` to put offset 0.0 (the highlight) at the top
/// (see the angle math in `iced_core::angle::Radians::to_distance`).
pub fn top_gradient(highlight: Color, base: Color) -> Background {
    // Composite the translucent highlight over the opaque base so the surface
    // stays opaque; the highlight's alpha controls how strongly the glow reads.
    let top = mix(
        base,
        Color {
            a: 1.0,
            ..highlight
        },
        highlight.a,
    );
    Background::Gradient(Gradient::Linear(
        Linear::new(std::f32::consts::PI)
            .add_stop(0.0, top)
            .add_stop(0.06, base)
            .add_stop(1.0, base),
    ))
}
