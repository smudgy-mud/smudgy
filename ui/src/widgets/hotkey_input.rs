use std::marker::PhantomData;

use iced::{
    Element,
    advanced::{
        Widget,
        widget::{Tree, tree},
    },
    keyboard::{
        Key,
        key::Named,
    },
    widget::{Text, text},
};

use crate::keymap::MaybePhysicalKey;

/// A widget for capturing hotkey combinations
pub struct HotkeyInput<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer + iced::advanced::text::Renderer<Font = iced::Font> + 'a,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'a,
{
    keys: &'a Vec<MaybePhysicalKey>,
    height: iced::Length,
    physical: bool,
    on_action: Option<Box<dyn Fn(Vec<MaybePhysicalKey>) -> Message>>,
    _p: PhantomData<(Message, Theme, Renderer)>,
}

impl<'a, Message, Theme, Renderer> HotkeyInput<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer + iced::advanced::text::Renderer<Font = iced::Font> + 'a,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'a,
{
    /// Create a new HotkeyInput widget with the given keys
    pub fn new(keys: &'a Vec<MaybePhysicalKey>, physical: bool) -> Self {
        Self {
            keys,
            height: iced::Length::Shrink,
            physical,
            on_action: None,
            _p: PhantomData,
        }
    }

    /// Set the height of the widget
    pub fn height(mut self, height: impl Into<iced::Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Set the callback for when a hotkey is captured
    pub fn on_action(mut self, f: impl Fn(Vec<MaybePhysicalKey>) -> Message + 'static) -> Self {
        self.on_action = Some(Box::new(f));
        self
    }

    /// Create the appropriate text widget based on current state
    fn create_text(&self, listening: bool) -> Text<'a, Theme, Renderer> {
        let content = if self.keys.is_empty() {
            if listening {
                "listening...".to_string()
            } else {
                "click to record".to_string()
            }
        } else {
            self.keys
                .iter()
                .map(|k| match k {
                    MaybePhysicalKey::Key(key) => format!("{:?}", key),
                    MaybePhysicalKey::Physical(physical) => format!("{:?}", physical),
                })
                .collect::<Vec<String>>()
                .join(" + ")
        };

        Text::new(content).height(self.height)
    }
}

#[derive(Default)]
struct State {
    listening: bool,
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for HotkeyInput<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer + iced::advanced::text::Renderer<Font = iced::Font> + 'a,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'a,
{
    fn children(&self) -> Vec<tree::Tree> {
        vec![Tree::new(Element::<Message, Theme, Renderer>::new(text(
            "".to_string(),
        )))]
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn diff(&self, _tree: &mut Tree) {
        // No diffing needed as we create text dynamically
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> iced::Size<iced::Length> {
        // Create a temporary text widget to get size information
        let temp_text = self.create_text(false);
        Widget::<Message, Theme, Renderer>::size(&temp_text)
    }

    fn size_hint(&self) -> iced::Size<iced::Length> {
        // Create a temporary text widget to get size hint
        let temp_text = self.create_text(false);
        Widget::<Message, Theme, Renderer>::size_hint(&temp_text)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        let state = tree.state.downcast_ref::<State>();
        let mut text_widget = self.create_text(state.listening);

        Widget::<Message, Theme, Renderer>::layout(
            &mut text_widget,
            tree.children.get_mut(0).unwrap(),
            renderer,
            limits,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let text_widget = self.create_text(state.listening);

        Widget::<Message, Theme, Renderer>::draw(
            &text_widget,
            tree.children.first().unwrap(),
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        )
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        _viewport: &iced::Rectangle,
        _renderer: &Renderer,
    ) -> iced::advanced::mouse::Interaction {
        if cursor.position_in(layout.bounds()).is_some() {
            iced::advanced::mouse::Interaction::Pointer
        } else {
            iced::advanced::mouse::Interaction::default()
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        _viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();

        match event {
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left))
                if cursor.is_over(layout.bounds()) =>
            {
                state.listening = true;
                if let Some(f) = self.on_action.as_ref() {
                    shell.publish(f(vec![]));
                }
                shell.capture_event();
            }
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key,
                modified_key: _,
                physical_key,
                location: _,
                modifiers,
                text: _,
                repeat: _,
            }) if state.listening => { match key {
                Key::Named(Named::Control)
                | Key::Named(Named::Shift)
                | Key::Named(Named::Alt)
                | Key::Named(Named::Super) => {
                    if let Some(f) = self.on_action.as_ref() {
                        let mut ret: Vec<MaybePhysicalKey> = Vec::new();
                        if modifiers.control() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Control)));
                        }
                        if modifiers.alt() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Alt)));
                        }
                        if modifiers.shift() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Shift)));
                        }
                        if modifiers.logo() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Super)));
                        }
                        shell.publish(f(ret));
                    }
                    shell.capture_event();
                }
                _ => {
                    state.listening = false;
                    if let Some(f) = self.on_action.as_ref() {
                        let mut ret: Vec<MaybePhysicalKey> = Vec::new();
                        if modifiers.control() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Control)));
                        }
                        if modifiers.alt() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Alt)));
                        }
                        if modifiers.shift() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Shift)));
                        }
                        if modifiers.logo() {
                            ret.push(MaybePhysicalKey::Key(Key::Named(Named::Super)));
                        }

                        // For the main key, use physical or logical based on the flag
                        if self.physical {
                            ret.push(MaybePhysicalKey::Physical(*physical_key));
                        } else {
                            ret.push(MaybePhysicalKey::Key(key.clone()));
                        }

                        shell.publish(f(ret));
                    };
                }
            } },
            _ => {}
        }
    }
}
