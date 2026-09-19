//! A reusable anchored overlay dropdown (D7 — the automations Parsing
//! picker's shape): an anchor control that, while open, floats arbitrary
//! content beneath itself through [`iced::advanced::overlay`], so the list
//! escapes any enclosing scrollable instead of being clipped by it.
//!
//! Open state lives in the application, not the widget: the anchor toggles
//! it, `on_dismiss` fires on click-outside and Escape, and `on_key` maps
//! keyboard navigation (up/down/enter) to application messages while open —
//! the part a stock `pick_list` gives for free that a custom list must not
//! lose.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, keyboard, mouse, touch};

use crate::theme::Theme;

/// The gap between the anchor and the floated content.
const GAP: f32 = 4.0;

pub struct Dropdown<'a, Message, Renderer = iced::Renderer> {
    /// `[anchor]` closed, `[anchor, content]` open.
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    on_dismiss: Message,
    #[allow(clippy::type_complexity)]
    on_key: Option<Box<dyn Fn(&keyboard::Key) -> Option<Message> + 'a>>,
}

impl<'a, Message, Renderer> Dropdown<'a, Message, Renderer> {
    /// An anchored dropdown: `content` is `Some` while open.
    pub fn new(
        anchor: impl Into<Element<'a, Message, Theme, Renderer>>,
        content: Option<Element<'a, Message, Theme, Renderer>>,
        on_dismiss: Message,
    ) -> Self {
        let mut children = vec![anchor.into()];
        children.extend(content);
        Self {
            children,
            on_dismiss,
            on_key: None,
        }
    }

    /// Maps a key press to a message while the dropdown is open (arrow
    /// navigation and Enter). Escape is always a dismiss and needs no
    /// mapping.
    #[must_use]
    pub fn on_key(mut self, on_key: impl Fn(&keyboard::Key) -> Option<Message> + 'a) -> Self {
        self.on_key = Some(Box::new(on_key));
        self
    }

    fn is_open(&self) -> bool {
        self.children.len() > 1
    }
}

impl<Message: Clone, Renderer: renderer::Renderer> Widget<Message, Theme, Renderer>
    for Dropdown<'_, Message, Renderer>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        self.children[0].as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // Only the anchor participates in normal layout; the content is laid
        // out by the overlay against the whole window.
        let anchor =
            self.children[0]
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits);
        let size = anchor.size();
        layout::Node::with_children(size, vec![anchor])
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.children[0].as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("dropdown anchor layout"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if !self.is_open() || shell.is_event_captured() {
            return;
        }
        match event {
            // A press nothing consumed — not the anchor, not the floated
            // content — is a click outside: dismiss.
            Event::Mouse(mouse::Event::ButtonPressed(_))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                shell.publish(self.on_dismiss.clone());
                shell.capture_event();
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => {
                if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                    shell.publish(self.on_dismiss.clone());
                    shell.capture_event();
                } else if let Some(on_key) = &self.on_key
                    && let Some(message) = on_key(key)
                {
                    shell.publish(message);
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children[0].as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("dropdown anchor layout"),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.children[0].as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().expect("dropdown anchor layout"),
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.children[0].as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("dropdown anchor layout"),
            renderer,
            operation,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        if self.children.len() > 1 {
            let (anchor_trees, content_trees) = tree.children.split_at_mut(1);
            let _ = anchor_trees;
            let bounds = layout.bounds();
            Some(overlay::Element::new(Box::new(DropdownOverlay {
                content: &mut self.children[1],
                tree: &mut content_trees[0],
                anchor: Rectangle {
                    x: bounds.x + translation.x,
                    y: bounds.y + translation.y,
                    ..bounds
                },
            })))
        } else {
            self.children[0].as_widget_mut().overlay(
                &mut tree.children[0],
                layout.children().next().expect("dropdown anchor layout"),
                renderer,
                viewport,
                translation,
            )
        }
    }
}

impl<'a, Message: Clone + 'a, Renderer: renderer::Renderer + 'a>
    From<Dropdown<'a, Message, Renderer>> for Element<'a, Message, Theme, Renderer>
{
    fn from(dropdown: Dropdown<'a, Message, Renderer>) -> Self {
        Element::new(dropdown)
    }
}

struct DropdownOverlay<'a, 'b, Message, Renderer> {
    content: &'b mut Element<'a, Message, Theme, Renderer>,
    tree: &'b mut Tree,
    anchor: Rectangle,
}

impl<Message, Renderer: renderer::Renderer> overlay::Overlay<Message, Theme, Renderer>
    for DropdownOverlay<'_, '_, Message, Renderer>
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(Size::ZERO, bounds);
        let node = self
            .content
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let size = node.size();
        // Below the anchor, flipping above when it would clip the bottom, and
        // nudged left as needed to stay inside the window.
        let x = (self.anchor.x).min(bounds.width - size.width).max(0.0);
        let below = self.anchor.y + self.anchor.height + GAP;
        let y = if below + size.height > bounds.height {
            (self.anchor.y - size.height - GAP).max(0.0)
        } else {
            below
        };
        node.move_to(Point::new(x, y))
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let bounds = layout.bounds();
        self.content.as_widget_mut().update(
            self.tree, event, layout, cursor, renderer, clipboard, shell, &bounds,
        );
        // Swallow presses on the surface itself (row padding included) so a
        // click inside the open list never doubles as a click-outside.
        if !shell.is_event_captured()
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(touch::Event::FingerPressed { .. })
            )
            && cursor.is_over(bounds)
        {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            self.tree,
            layout,
            cursor,
            &layout.bounds(),
            renderer,
        )
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.content.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &layout.bounds(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::Space;

    #[test]
    fn overlay_follows_its_anchor_inside_translated_and_scrolled_parents() {
        let mut dropdown: Element<'_, u8, Theme, ()> = Dropdown::new(
            Space::new().width(40).height(30),
            Some(Space::new().width(200).height(160).into()),
            0,
        )
        .into();
        let mut tree = Tree::new(dropdown.as_widget());
        let bounds = Size::new(1500.0, 800.0);
        let viewport = Rectangle::with_size(bounds);
        let node = dropdown
            .as_widget_mut()
            .layout(&mut tree, &(), &layout::Limits::new(Size::ZERO, bounds))
            .move_to(Point::new(1200.0, 540.0));
        assert_eq!(
            node.size(),
            Size::new(40.0, 30.0),
            "menu width must not enlarge the anchor"
        );
        let mut overlay = dropdown
            .as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&node),
                &(),
                &viewport,
                Vector::new(30.0, -220.0),
            )
            .unwrap();
        let node = overlay.as_overlay_mut().layout(&(), bounds);
        assert_eq!(
            node.bounds(),
            Rectangle {
                x: 1230.0,
                y: 354.0,
                width: 200.0,
                height: 160.0
            }
        );
    }

    #[test]
    fn overlay_flips_above_and_stays_inside_the_right_edge() {
        let mut dropdown: Element<'_, u8, Theme, ()> = Dropdown::new(
            Space::new().width(40).height(30),
            Some(Space::new().width(200).height(160).into()),
            0,
        )
        .into();
        let mut tree = Tree::new(dropdown.as_widget());
        let bounds = Size::new(1500.0, 800.0);
        let viewport = Rectangle::with_size(bounds);
        let node = dropdown
            .as_widget_mut()
            .layout(&mut tree, &(), &layout::Limits::new(Size::ZERO, bounds))
            .move_to(Point::new(1440.0, 750.0));
        let mut overlay = dropdown
            .as_widget_mut()
            .overlay(&mut tree, Layout::new(&node), &(), &viewport, Vector::ZERO)
            .unwrap();
        let node = overlay.as_overlay_mut().layout(&(), bounds);
        assert_eq!(node.bounds().position(), Point::new(1300.0, 586.0));
    }
}
