//! The drop-target feedback layer for a pane drag.
//!
//! [`TargetOverlay`] is an inert widget that fills its slot and paints
//! feedback rectangles — stacked permanently over each window's pane grid
//! (and its empty state). The layer is always mounted and empty when no drag
//! hovers the window: only its *content* toggles. Swapping the element type
//! at this tree position would make iced rebuild the sibling subtree and
//! erase in-flight widget state — the measured failure mode that loses
//! mid-drag releases.
//!
//! The rectangles come from the daemon's classified drag target, so what the
//! overlay shows is exactly what the drop will do. Rectangles carry semantic
//! roles rather than colors; the theme's tab palette resolves them at draw
//! time.

use iced::advanced::renderer::Quad;
use iced::advanced::{Renderer as _, Widget, layout, widget::tree};
use iced::{Length, Rectangle, Size, border, mouse};

use smudgy_theme::{Element, Theme};

/// What a feedback rectangle represents — the theme picks its paint.
#[derive(Debug, Clone, Copy)]
pub enum OverlayRole {
    /// The surface the drop will occupy: filled, with an emphasis outline.
    Target,
    /// The insertion caret within a header band (merge/reorder targets).
    Caret,
}

/// One feedback rectangle, relative to the overlay's own origin.
#[derive(Debug, Clone, Copy)]
pub struct OverlayRect {
    pub bounds: Rectangle,
    pub role: OverlayRole,
}

/// The inert feedback layer. It neither lays out children nor consumes
/// events, so whatever it covers behaves exactly as if it were absent.
pub struct TargetOverlay {
    rects: Vec<OverlayRect>,
}

impl TargetOverlay {
    pub fn new(rects: Vec<OverlayRect>) -> Self {
        Self { rects }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for TargetOverlay {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut tree::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fill, Length::Fill)
    }

    fn draw(
        &self,
        _tree: &tree::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        _style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let tabs = &theme.styles.tabs;
        let origin = layout.bounds().position();
        for rect in &self.rects {
            let bounds = Rectangle {
                x: origin.x + rect.bounds.x,
                y: origin.y + rect.bounds.y,
                ..rect.bounds
            };
            let (fill, border) = match rect.role {
                OverlayRole::Target => (
                    tabs.drop_highlight,
                    border::rounded(2).color(tabs.drop_marker).width(2.0),
                ),
                OverlayRole::Caret => (tabs.drop_marker, border::rounded(2)),
            };
            renderer.fill_quad(
                Quad {
                    bounds,
                    border,
                    ..Default::default()
                },
                fill,
            );
        }
    }
}

impl<'a, Message> From<TargetOverlay> for Element<'a, Message> {
    fn from(overlay: TargetOverlay) -> Self {
        Element::new(overlay)
    }
}
