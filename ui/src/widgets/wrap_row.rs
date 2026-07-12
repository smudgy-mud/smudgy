//! A horizontal flow layout that wraps onto new lines when its children exceed
//! the available width. iced 0.14 has no flex-wrap container, and the usual
//! `responsive` helper misbehaves inside a vertical `scrollable` (it is handed
//! an unbounded height), so the dashboard's "Create" tiles use this to reflow
//! as the window narrows instead of overflowing the pane.

use iced::advanced::layout::{self, Layout, Node};
use iced::advanced::widget::Tree;
use iced::advanced::{Clipboard, Shell, Widget, mouse};
use iced::{Element, Event, Length, Point, Rectangle, Size};

/// A row of widgets that wraps onto additional rows when the available width is
/// too small to fit them all. Children keep their own size; only the line
/// breaks (and the gaps between items/rows) are computed here. Rows are packed
/// left-to-right, top-aligned, and the block left-aligns within its parent.
pub struct WrapRow<'a, Message, Theme, Renderer> {
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    spacing_x: f32,
    spacing_y: f32,
}

impl<'a, Message, Theme, Renderer> WrapRow<'a, Message, Theme, Renderer> {
    /// Wrap `children` into a flowing row with no spacing.
    #[must_use]
    pub fn new(children: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            children,
            spacing_x: 0.0,
            spacing_y: 0.0,
        }
    }

    /// Set the horizontal gap between items and the vertical gap between rows.
    #[must_use]
    pub fn spacing(mut self, horizontal: f32, vertical: f32) -> Self {
        self.spacing_x = horizontal;
        self.spacing_y = vertical;
        self
    }
}

/// Convenience constructor mirroring iced's `row`/`column` free functions.
#[must_use]
pub fn wrap_row<'a, Message, Theme, Renderer>(
    children: Vec<Element<'a, Message, Theme, Renderer>>,
) -> WrapRow<'a, Message, Theme, Renderer> {
    WrapRow::new(children)
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for WrapRow<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> Node {
        let max_width = limits.max().width;
        let child_limits = limits.loose();

        let mut nodes = Vec::with_capacity(self.children.len());
        let mut x = 0.0_f32;
        let mut y = 0.0_f32;
        let mut row_height = 0.0_f32;
        let mut content_width = 0.0_f32;

        for (child, child_tree) in self.children.iter_mut().zip(&mut tree.children) {
            let node = child
                .as_widget_mut()
                .layout(child_tree, renderer, &child_limits);
            let size = node.size();

            // Break to a new row when this child would overflow the line — but
            // never leave a row empty, so a child wider than the line still
            // gets its own row rather than looping forever.
            if x > 0.0 && x + size.width > max_width {
                x = 0.0;
                y += row_height + self.spacing_y;
                row_height = 0.0;
            }

            nodes.push(node.move_to(Point::new(x, y)));
            x += size.width + self.spacing_x;
            content_width = content_width.max(x - self.spacing_x);
            row_height = row_height.max(size.height);
        }

        let size = Size::new(content_width.min(max_width), y + row_height);
        Node::with_children(size, nodes)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
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
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .fold(mouse::Interaction::Idle, |acc, interaction| {
                if acc == mouse::Interaction::Idle {
                    interaction
                } else {
                    acc
                }
            })
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
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }
}

impl<'a, Message, Theme, Renderer> From<WrapRow<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(widget: WrapRow<'a, Message, Theme, Renderer>) -> Self {
        Element::new(widget)
    }
}
