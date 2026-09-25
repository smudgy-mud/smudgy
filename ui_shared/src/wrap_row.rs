//! A horizontal flow layout shared by desktop and browser UI.

use iced::advanced::layout::{self, Layout, Node};
use iced::advanced::widget::Tree;
use iced::advanced::{Clipboard, Shell, Widget, mouse};
use iced::{Element, Event, Length, Point, Rectangle, Size};

pub struct WrapRow<'a, Message, Theme, Renderer> {
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    spacing_x: f32,
    spacing_y: f32,
}

impl<'a, Message, Theme, Renderer> WrapRow<'a, Message, Theme, Renderer> {
    #[must_use]
    pub fn new(children: Vec<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            children,
            spacing_x: 0.0,
            spacing_y: 0.0,
        }
    }

    #[must_use]
    pub fn spacing(mut self, horizontal: f32, vertical: f32) -> Self {
        self.spacing_x = horizontal;
        self.spacing_y = vertical;
        self
    }
}

#[must_use]
pub fn wrap_row<Message, Theme, Renderer>(
    children: Vec<Element<'_, Message, Theme, Renderer>>,
) -> WrapRow<'_, Message, Theme, Renderer> {
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
        Node::with_children(
            Size::new(content_width.min(max_width), y + row_height),
            nodes,
        )
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
            .max()
            .unwrap_or_default()
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
