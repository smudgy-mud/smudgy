//! A vertical stack in which one child absorbs the pane's spare height.
//!
//! A `scrollable` hands its content an unbounded height, so a `Length::Fill`
//! child inside one has nothing to fill. Editor panes want the opposite of
//! that: fixed chrome above and below, and an editor that takes every pixel
//! the viewport still has once the chrome is laid out, falling back to a
//! floor height (and the scrollbar) when the viewport is too short. The
//! caller passes the viewport height it learned outside the scrollable.
//!
//! Bounded dialogs instead use the parent's height limit. Their body can
//! shrink to its content, but leaves room for the header and footer first.

use iced::advanced::layout::{self, Layout, Node};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Shell, Widget, mouse, overlay};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};

/// Children stacked top to bottom, with one child's height calculated after
/// reserving space for all the others.
pub struct GrowColumn<'a, Message, Theme, Renderer> {
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    grow: usize,
    height: BodyHeight,
    spacing: f32,
}

enum BodyHeight {
    Viewport { minimum: f32, available: f32 },
    Bounded,
}

impl<'a, Message, Theme, Renderer> GrowColumn<'a, Message, Theme, Renderer> {
    /// Stack `children`, letting the one at index `grow` absorb whatever of
    /// `available_height` the others leave, but never less than
    /// `min_grow_height`.
    ///
    /// # Panics
    /// Panics if `grow` is not a valid child index.
    #[must_use]
    pub fn new(
        children: Vec<Element<'a, Message, Theme, Renderer>>,
        grow: usize,
        min_grow_height: f32,
        available_height: f32,
    ) -> Self {
        assert!(grow < children.len(), "grow index out of range");
        Self {
            children,
            grow,
            height: BodyHeight::Viewport {
                minimum: min_grow_height,
                available: available_height,
            },
            spacing: 0.0,
        }
    }

    /// Reserve the other children's height before laying out `body` within
    /// the remaining parent limits. A shrinking scrollable stays compact
    /// when its content fits and scrolls when it does not. Unlike `new`, this
    /// is used outside a scrollable, where the parent supplies finite limits.
    ///
    /// # Panics
    /// Panics if `body` is not a valid child index.
    #[must_use]
    pub fn bounded(children: Vec<Element<'a, Message, Theme, Renderer>>, body: usize) -> Self {
        assert!(body < children.len(), "body index out of range");
        Self {
            children,
            grow: body,
            height: BodyHeight::Bounded,
            spacing: 0.0,
        }
    }

    /// The vertical gap between children.
    #[must_use]
    pub fn spacing(mut self, spacing: f32) -> Self {
        self.spacing = spacing;
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for GrowColumn<'_, Message, Theme, Renderer>
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
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> Node {
        let width = limits.max().width;
        let chrome_limits = layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY));

        // Every fixed child first, so the grow child sees exactly what is left.
        let mut nodes: Vec<Option<Node>> = (0..self.children.len()).map(|_| None).collect();
        let mut chrome_height = 0.0_f32;
        for (index, (child, child_tree)) in
            self.children.iter_mut().zip(&mut tree.children).enumerate()
        {
            if index == self.grow {
                continue;
            }
            let node = child
                .as_widget_mut()
                .layout(child_tree, renderer, &chrome_limits);
            chrome_height += node.size().height;
            nodes[index] = Some(node);
        }
        let gaps = self.spacing * (self.children.len().saturating_sub(1)) as f32;
        let (minimum, maximum) = match self.height {
            BodyHeight::Viewport { minimum, available } => {
                let height = (available - chrome_height - gaps).max(minimum);
                (height, height)
            }
            BodyHeight::Bounded => (0.0, (limits.max().height - chrome_height - gaps).max(0.0)),
        };
        let grow_limits = layout::Limits::new(Size::new(0.0, minimum), Size::new(width, maximum));
        nodes[self.grow] = Some(self.children[self.grow].as_widget_mut().layout(
            &mut tree.children[self.grow],
            renderer,
            &grow_limits,
        ));

        let mut y = 0.0_f32;
        let mut placed = Vec::with_capacity(nodes.len());
        for node in nodes.into_iter().flatten() {
            let height = node.size().height;
            placed.push(node.move_to(Point::new(0.0, y)));
            y += height + self.spacing;
        }
        let total = (y - self.spacing).max(0.0);
        Node::with_children(Size::new(width, total), placed)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), layout)| {
                    child
                        .as_widget_mut()
                        .operate(state, layout, renderer, operation);
                });
        });
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
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
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

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<GrowColumn<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(widget: GrowColumn<'a, Message, Theme, Renderer>) -> Self {
        Element::new(widget)
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::widget::Tree;
    use iced::advanced::{Widget, layout};
    use iced::widget::Space;
    use iced::{Element, Length, Size};

    use super::GrowColumn;

    type TestElement<'a> = Element<'a, (), iced::Theme, ()>;

    fn column(available: f32) -> GrowColumn<'static, (), iced::Theme, ()> {
        let children: Vec<TestElement<'static>> = vec![
            Space::new().width(Length::Fill).height(40.0).into(),
            Space::new().width(Length::Fill).height(Length::Fill).into(),
            Space::new().width(Length::Fill).height(30.0).into(),
        ];
        GrowColumn::new(children, 1, 100.0, available).spacing(10.0)
    }

    fn child_heights(column: &mut GrowColumn<'static, (), iced::Theme, ()>) -> Vec<f32> {
        let mut tree = Tree::new(&*column as &dyn Widget<(), iced::Theme, ()>);
        let node = column.layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(500.0, f32::INFINITY)),
        );
        node.children()
            .iter()
            .map(|child| child.size().height)
            .collect()
    }

    #[test]
    fn grow_child_takes_the_viewport_room_the_chrome_leaves() {
        let mut column = column(400.0);
        // 400 - 40 - 30 - two 10px gaps.
        assert_eq!(child_heights(&mut column), vec![40.0, 310.0, 30.0]);
    }

    #[test]
    fn grow_child_never_shrinks_below_its_floor() {
        let mut column = column(120.0);
        assert_eq!(child_heights(&mut column), vec![40.0, 100.0, 30.0]);
    }

    #[test]
    fn bounded_body_reserves_footer_space_and_keeps_short_content_compact() {
        for (content, expected) in [(30.0, 120.0), (900.0, 300.0)] {
            let children: Vec<TestElement<'static>> = vec![
                Space::new().height(40.0).into(),
                Space::new().height(content).into(),
                Space::new().height(30.0).into(),
            ];
            let mut column = GrowColumn::bounded(children, 1).spacing(10.0);
            let mut tree = Tree::new(&column as &dyn Widget<(), iced::Theme, ()>);
            let node = column.layout(
                &mut tree,
                &(),
                &layout::Limits::new(Size::ZERO, Size::new(400.0, 300.0)),
            );
            assert_eq!(node.size().height, expected);
            assert_eq!(node.children()[2].bounds().height, 30.0);
            assert_eq!(node.children()[2].bounds().y + 30.0, expected);
        }
    }

    #[test]
    fn the_hovered_child_decides_the_cursor_even_after_an_indifferent_sibling() {
        use iced::advanced::{Layout, mouse};
        use iced::widget::button;
        use iced::{Point, Rectangle};

        let children: Vec<TestElement<'static>> = vec![
            Space::new().width(Length::Fill).height(40.0).into(),
            button(Space::new().width(Length::Fill).height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .on_press(())
                .into(),
            Space::new().width(Length::Fill).height(30.0).into(),
        ];
        let mut column = GrowColumn::new(children, 1, 100.0, 400.0).spacing(10.0);
        let mut tree = Tree::new(&column as &dyn Widget<(), iced::Theme, ()>);
        let node = column.layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(500.0, f32::INFINITY)),
        );
        let viewport = Rectangle::new(Point::ORIGIN, Size::new(500.0, 400.0));
        let over_button = mouse::Cursor::Available(Point::new(20.0, 100.0));
        assert_eq!(
            column.mouse_interaction(&tree, Layout::new(&node), over_button, &viewport, &()),
            mouse::Interaction::Pointer
        );
        let over_space = mouse::Cursor::Available(Point::new(20.0, 10.0));
        assert_eq!(
            column.mouse_interaction(&tree, Layout::new(&node), over_space, &viewport, &()),
            mouse::Interaction::None
        );
    }

    #[test]
    fn children_are_stacked_with_spacing_and_the_column_spans_the_width() {
        let mut column = column(400.0);
        let mut tree = Tree::new(&column as &dyn Widget<(), iced::Theme, ()>);
        let node = column.layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(500.0, f32::INFINITY)),
        );
        let tops: Vec<f32> = node
            .children()
            .iter()
            .map(|child| child.bounds().y)
            .collect();
        assert_eq!(tops, vec![0.0, 50.0, 370.0]);
        assert_eq!(node.size(), Size::new(500.0, 400.0));
    }
}
