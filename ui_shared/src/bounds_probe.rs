//! A transparent wrapper that reports its child's laid-out bounds at draw
//! time — the geometry mirror feeding drag hit-testing.
//!
//! The drag controller classifies targets against live geometry without
//! walking the widget tree per cursor move: strips and tabs are wrapped in
//! probes whose draw pass writes the current bounds into window-owned
//! mirrors (interior-mutable caches, the `grid_area` pattern). Every mapped
//! drag message repaints all windows, so mirrors are at most one frame
//! stale mid-drag.
//!
//! Coordinates are whatever space the parent draws in: window-absolute
//! outside any scrollable; content-space (un-scrolled, anchored at the
//! scrollable's viewport origin) inside one. Callers pair a probe inside a
//! scrollable with the scrollable's own offset mirror.

use iced::advanced::widget::{Operation, tree};
use iced::advanced::{Widget, layout, overlay};
use iced::{Element, Event as IcedEvent, Length, Rectangle, Size, Vector, mouse};

/// The wrapper. Fully transparent to layout, events, drawing, and overlays.
pub struct BoundsProbe<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_bounds: Box<dyn Fn(Rectangle) + 'a>,
}

impl<'a, Message, Theme, Renderer> BoundsProbe<'a, Message, Theme, Renderer> {
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        on_bounds: impl Fn(Rectangle) + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            on_bounds: Box::new(on_bounds),
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for BoundsProbe<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<tree::Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut tree::Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut tree::Tree,
        layout: layout::Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &IcedEvent,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        (self.on_bounds)(layout.bounds());
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &tree::Tree,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut tree::Tree,
        layout: layout::Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message, Theme, Renderer> From<BoundsProbe<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(probe: BoundsProbe<'a, Message, Theme, Renderer>) -> Self {
        Element::new(probe)
    }
}
