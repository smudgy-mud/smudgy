//! Input barrier for a full-window modal layer in a stack. The content receives events first;
//! unhandled input cannot reach focused controls below it. Child overlays (including tooltips)
//! remain available, and Escape requests dismissal.

use iced::advanced::{Clipboard, Shell, Widget, layout, overlay, renderer, widget};
use iced::{Element, Event, Length, Rectangle, Size, Vector, keyboard, mouse};

pub struct ModalLayer<'a, Message, Theme = crate::Theme, Renderer = iced::Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    dismiss: Message,
}

impl<'a, Message, Theme, Renderer> ModalLayer<'a, Message, Theme, Renderer> {
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        dismiss: Message,
    ) -> Self {
        Self {
            content: content.into(),
            dismiss,
        }
    }
}

impl<Message: Clone, Theme, Renderer: renderer::Renderer> Widget<Message, Theme, Renderer>
    for ModalLayer<'_, Message, Theme, Renderer>
{
    fn tag(&self) -> widget::tree::Tag {
        self.content.as_widget().tag()
    }
    fn state(&self) -> widget::tree::State {
        self.content.as_widget().state()
    }
    fn children(&self) -> Vec<widget::Tree> {
        self.content.as_widget().children()
    }
    fn diff(&self, tree: &mut widget::Tree) {
        self.content.as_widget().diff(tree);
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: layout::Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
        if !shell.is_event_captured()
            && matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                })
            )
        {
            shell.publish(self.dismiss.clone());
        }
        if matches!(
            event,
            Event::Mouse(_) | Event::Touch(_) | Event::Keyboard(_) | Event::InputMethod(_)
        ) {
            shell.capture_event();
        }
    }
    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction = self
            .content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer);
        if interaction == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }
    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut widget::Tree,
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

impl<'a, Message: Clone + 'a, Theme: 'a, Renderer: renderer::Renderer + 'a>
    From<ModalLayer<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
{
    fn from(layer: ModalLayer<'a, Message, Theme, Renderer>) -> Self {
        Element::new(layer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::Tree;
    use iced::widget::{Space, mouse_area, stack};
    use iced::{Point, touch};

    struct FocusedInput;
    impl Widget<u8, (), ()> for FocusedInput {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Fill, Length::Fill)
        }
        fn layout(&mut self, _: &mut Tree, _: &(), limits: &layout::Limits) -> layout::Node {
            layout::Node::new(limits.max())
        }
        fn update(
            &mut self,
            _: &mut Tree,
            _: &Event,
            _: layout::Layout<'_>,
            _: mouse::Cursor,
            _: &(),
            _: &mut dyn Clipboard,
            shell: &mut Shell<'_, u8>,
            _: &Rectangle,
        ) {
            shell.publish(1);
        }
        fn mouse_interaction(
            &self,
            _: &Tree,
            _: layout::Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
            _: &(),
        ) -> mouse::Interaction {
            mouse::Interaction::Text
        }
        fn draw(
            &self,
            _: &Tree,
            _: &mut (),
            _: &(),
            _: &renderer::Style,
            _: layout::Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
        ) {
        }
    }

    fn key(key: keyboard::Key) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyA),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn modal_blocks_underlying_clicks_scroll_typing_touch_and_text_cursor() {
        let base = Element::new(FocusedInput);
        let modal: Element<'_, u8, (), ()> =
            ModalLayer::new(Space::new().width(Length::Fill).height(Length::Fill), 9).into();
        let mut layers: Element<'_, u8, (), ()> = stack([base, modal]).into();
        let mut tree = Tree::new(layers.as_widget());
        let bounds = Size::new(800.0, 600.0);
        let viewport = Rectangle::with_size(bounds);
        let node =
            layers
                .as_widget_mut()
                .layout(&mut tree, &(), &layout::Limits::new(Size::ZERO, bounds));
        let cursor = mouse::Cursor::Available(Point::new(40.0, 40.0));
        assert_eq!(
            layers.as_widget().mouse_interaction(
                &tree,
                layout::Layout::new(&node),
                cursor,
                &viewport,
                &()
            ),
            mouse::Interaction::Idle
        );
        for event in [
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(41.0, 41.0),
            }),
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
            }),
            Event::Touch(touch::Event::FingerPressed {
                id: touch::Finger(0),
                position: Point::new(40.0, 40.0),
            }),
            key(keyboard::Key::Character("a".into())),
            key(keyboard::Key::Named(keyboard::key::Named::Tab)),
            key(keyboard::Key::Named(keyboard::key::Named::Escape)),
        ] {
            let mut messages = vec![];
            let mut shell = Shell::new(&mut messages);
            layers.as_widget_mut().update(
                &mut tree,
                &event,
                layout::Layout::new(&node),
                cursor,
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut shell,
                &viewport,
            );
            assert!(shell.is_event_captured());
            assert!(
                !messages.contains(&1),
                "input leaked to the focused control below the modal"
            );
            if matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                })
            ) {
                assert_eq!(messages, vec![9]);
            }
        }
    }

    #[test]
    fn modal_content_still_receives_clicks_and_its_own_cursor() {
        let content = mouse_area(Space::new().width(200).height(100))
            .on_press(2)
            .interaction(mouse::Interaction::Pointer);
        let mut modal: Element<'_, u8, (), ()> = ModalLayer::new(content, 9).into();
        let mut tree = Tree::new(modal.as_widget());
        let viewport = Rectangle::with_size(Size::new(800.0, 600.0));
        let node = modal.as_widget_mut().layout(
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let cursor = mouse::Cursor::Available(Point::new(30.0, 30.0));
        assert_eq!(
            modal.as_widget().mouse_interaction(
                &tree,
                layout::Layout::new(&node),
                cursor,
                &viewport,
                &()
            ),
            mouse::Interaction::Pointer
        );
        let mut messages = vec![];
        modal.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            layout::Layout::new(&node),
            cursor,
            &(),
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        assert_eq!(messages, vec![2]);
    }
}
