use crate::Theme;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell, overlay};
use iced::{
    Border, Color, Element, Event, Length, Padding, Rectangle, Size, Vector, mouse, widget::button,
};

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
        button::Status::Active | button::Status::Pressed | button::Status::Disabled => {
            button::Style {
                background: None,
                text_color: theme.styles.text.normal,
                ..Default::default()
            }
        }
        button::Status::Hovered => button::Style {
            background: Some(Color::from_rgba8(255, 255, 255, 0.1).into()),
            text_color: theme.styles.text.normal,
            ..Default::default()
        },
    }
}

#[must_use]
pub fn list_item_selected(theme: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Active | button::Status::Pressed | button::Status::Disabled => {
            button::Style {
                background: Some(Color::from_rgba8(255, 255, 255, 0.15).into()),
                text_color: theme.styles.text.normal,
                ..Default::default()
            }
        }
        button::Status::Hovered => button::Style {
            background: Some(Color::from_rgba8(255, 255, 255, 0.2).into()),
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

/// A quiet text link: muted at rest, full-strength on hover, no background.
/// Pair with [`underlined`] so the label carries the standard link rule.
#[must_use]
pub fn quiet_link(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: None,
        border: Border::default(),
        text_color: match status {
            button::Status::Active => theme.styles.text.normal.scale_alpha(0.55),
            button::Status::Hovered => theme.styles.text.normal,
            button::Status::Pressed => theme.styles.text.normal.scale_alpha(0.8),
            button::Status::Disabled => theme.styles.text.normal.scale_alpha(0.25),
        },
        ..Default::default()
    }
}

/// The destructive variant of [`quiet_link`]: the error hue, lifted on hover.
#[must_use]
pub fn danger_link(theme: &Theme, status: button::Status) -> button::Style {
    let base = theme.styles.text.error;
    let lift = Color {
        r: base.r + (1.0 - base.r) * 0.25,
        g: base.g + (1.0 - base.g) * 0.25,
        b: base.b + (1.0 - base.b) * 0.25,
        a: base.a,
    };
    button::Style {
        background: None,
        border: Border::default(),
        text_color: match status {
            button::Status::Active => base,
            button::Status::Hovered => lift,
            button::Status::Pressed => base.scale_alpha(0.8),
            button::Status::Disabled => base.scale_alpha(0.3),
        },
        ..Default::default()
    }
}

// ---- the link underline rule ------------------------------------------------

/// The gap between a link label's bottom edge and its underline rule.
const UNDERLINE_OFFSET: f32 = 3.0;
/// The underline rule's thickness.
const UNDERLINE_THICKNESS: f32 = 1.0;

/// Wraps a link label so a 1px rule draws [`UNDERLINE_OFFSET`] beneath it in
/// the inherited text color — the color the enclosing [`button`] resolved for
/// its status, so the rule tracks hover, press, and the destructive variant
/// with no second style. This is the pane-wide link underline (D8); use it for
/// every text link, the destructive delete link included.
pub struct Underlined<'a, Message> {
    content: Element<'a, Message, Theme>,
}

/// Wraps `content` (typically a [`iced::widget::text`] label without an
/// explicit color, inside a [`button`] styled [`quiet_link`] or
/// [`danger_link`]) with the standard link underline.
pub fn underlined<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> Underlined<'a, Message> {
    Underlined {
        content: content.into(),
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Underlined<'_, Message> {
    fn tag(&self) -> widget::tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> widget::tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        let size = self.content.as_widget().size();
        Size {
            width: size.width,
            height: Length::Shrink,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let extra = UNDERLINE_OFFSET + UNDERLINE_THICKNESS;
        let child = self.content.as_widget_mut().layout(
            tree,
            renderer,
            &limits.shrink(Padding {
                top: 0.0,
                right: 0.0,
                bottom: extra,
                left: 0.0,
            }),
        );
        let size = child.size();
        layout::Node::with_children(Size::new(size.width, size.height + extra), vec![child])
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree,
            event,
            layout.children().next().expect("underline child layout"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content.as_widget_mut().operate(
            tree,
            layout.children().next().expect("underline child layout"),
            renderer,
            operation,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            tree,
            layout.children().next().expect("underline child layout"),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let child = layout.children().next().expect("underline child layout");
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, child, cursor, viewport);
        let bounds = child.bounds();
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x,
                    y: bounds.y + bounds.height + UNDERLINE_OFFSET,
                    width: bounds.width,
                    height: UNDERLINE_THICKNESS,
                },
                ..renderer::Quad::default()
            },
            style.text_color,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            tree,
            layout.children().next().expect("underline child layout"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Underlined<'a, Message>> for Element<'a, Message, Theme> {
    fn from(underlined: Underlined<'a, Message>) -> Self {
        Element::new(underlined)
    }
}
