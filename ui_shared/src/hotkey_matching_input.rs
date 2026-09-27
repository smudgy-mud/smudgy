use std::{collections::HashMap, marker::PhantomData};

use iced::{
    advanced::{
        Widget,
        widget::{Tree, tree},
    },
    keyboard::{
        self,
        key::{self},
    },
    widget::{Id, TextInput, text_input},
};
use smudgy_session_model::HotkeyId;

use crate::keymap::MaybePhysicalKey;

/// The wrapped input's caret state as observed after an event: focus plus the
/// widget's raw [`text_input::cursor::Cursor`] (grapheme-indexed and
/// unclamped — `select_all` parks an endpoint at `usize::MAX`). Published raw
/// through `on_caret_change` whenever it differs from the previous event's
/// reading; the consumer clamps against its own copy of the value when it
/// needs positions, so an observation made in the same event that edited the
/// text is never clamped against the stale pre-edit string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaretState {
    pub focused: bool,
    pub cursor: text_input::cursor::Cursor,
}

/// Whether a key press is one of the wrapped input's clipboard-write
/// shortcuts (copy or cut): `command`+`c`/`x` by the same latin-key dispatch
/// the `TextInput` itself uses.
fn is_clipboard_write_shortcut(
    key: &keyboard::Key,
    physical_key: key::Physical,
    modifiers: keyboard::Modifiers,
) -> bool {
    modifiers.command() && matches!(key.to_latin(physical_key), Some('c' | 'x'))
}

fn fallback_key_matches(
    expected: &keyboard::Key,
    pressed: &keyboard::Key,
    physical_key: key::Physical,
) -> bool {
    if expected == pressed {
        return true;
    }
    let (keyboard::Key::Character(expected_str), keyboard::Key::Character(pressed_str)) =
        (expected, pressed)
    else {
        return false;
    };
    if expected_str.eq_ignore_ascii_case(pressed_str) {
        return true;
    }
    // A non-Latin layout reports a chord like ctrl+F with a non-Latin logical
    // key. Single-letter bindings dispatch through the same latin-key mapping
    // the wrapped input uses for its clipboard shortcuts, so the chord stays
    // reachable from any layout.
    let mut expected_chars = expected_str.chars();
    match (expected_chars.next(), expected_chars.next()) {
        (Some(expected_char), None) => pressed
            .to_latin(physical_key)
            .is_some_and(|latin| latin.eq_ignore_ascii_case(&expected_char)),
        _ => false,
    }
}

enum ShortcutMatch<'a, Message> {
    Hotkey(HotkeyId),
    Fallback(&'a Message),
}

/// Reserved application chord that opens and focuses the audio panel. It is
/// excluded from user hotkey dispatch so a focused command input cannot
/// double-fire a script automation and the application action.
#[cfg(feature = "web-audio-cpal")]
pub(crate) fn is_audio_panel_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
    modifiers == (keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT)
        && matches!(key, keyboard::Key::Character(value) if value.eq_ignore_ascii_case("a"))
}

pub struct HotkeyMatchingInput<'a, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: text_input::Catalog,
    Renderer: iced::advanced::text::Renderer,
{
    hotkeys: &'a HashMap<MaybePhysicalKey, Vec<(keyboard::Modifiers, HotkeyId)>>,
    hooks: HashMap<keyboard::Key, Message>,
    shortcut_fallbacks: Vec<(keyboard::Key, keyboard::Modifiers, Message)>,
    text_input: TextInput<'a, Message, Theme, Renderer>,
    on_match: Option<Box<dyn Fn(HotkeyId) -> Message>>,
    on_focus: Option<Message>,
    on_unfocus: Option<Message>,
    on_caret_change: Option<Box<dyn Fn(CaretState) -> Message>>,
    #[cfg(feature = "web-audio-cpal")]
    on_audio_panel_shortcut: Option<Message>,
    suppress_clipboard_writes: bool,
    _p: PhantomData<(Message, Theme, Renderer)>,
}

impl<'a, Message, Theme, Renderer> HotkeyMatchingInput<'a, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: text_input::Catalog,
    Renderer: iced::advanced::text::Renderer,
{
    /// Create a new HotkeyInput widget with the given keys
    pub fn new(
        hotkeys: &'a HashMap<MaybePhysicalKey, Vec<(keyboard::Modifiers, HotkeyId)>>,
        placeholder: &'a str,
        value: &'a str,
    ) -> Self {
        Self {
            hotkeys,
            hooks: HashMap::new(),
            shortcut_fallbacks: Vec::new(),
            text_input: TextInput::<'a, Message, Theme, Renderer>::new(placeholder, value),
            on_match: None,
            on_focus: None,
            on_unfocus: None,
            on_caret_change: None,
            #[cfg(feature = "web-audio-cpal")]
            on_audio_panel_shortcut: None,
            suppress_clipboard_writes: false,
            _p: PhantomData,
        }
    }

    /// Set the callback for when a hotkey is captured
    pub fn on_match(mut self, f: impl Fn(HotkeyId) -> Message + 'static) -> Self {
        self.on_match = Some(Box::new(f));
        self
    }

    /// Set the message published when the input transitions from unfocused to
    /// focused.
    pub fn on_focus(mut self, message: Message) -> Self {
        self.on_focus = Some(message);
        self
    }

    /// Set the message published when the input transitions from focused to
    /// unfocused (e.g. the user clicks away or the window loses focus).
    pub fn on_unfocus(mut self, message: Message) -> Self {
        self.on_unfocus = Some(message);
        self
    }

    /// Set the callback published whenever the wrapped input's caret state
    /// (focus, cursor, selection) differs from the previous event's reading.
    pub fn on_caret_change(mut self, f: impl Fn(CaretState) -> Message + 'static) -> Self {
        self.on_caret_change = Some(Box::new(f));
        self
    }

    /// Reserve the application audio-panel chord ahead of both user hotkeys
    /// and iced's built-in command+A text selection.
    #[cfg(feature = "web-audio-cpal")]
    pub fn on_audio_panel_shortcut(mut self, message: Message) -> Self {
        self.on_audio_panel_shortcut = Some(message);
        self
    }

    /// Render the wrapped input in secure (password) mode: glyphs are masked
    /// and clipboard/word-selection affordances are disabled by the widget.
    pub fn secure(mut self, is_secure: bool) -> Self {
        self.text_input = self.text_input.secure(is_secure);
        self
    }

    /// Capture the clipboard-write shortcuts (copy/cut) before the wrapped
    /// input sees them. The `TextInput` only withholds clipboard writes while
    /// rendering secure; a masked input revealed on screen is
    /// rendering-insecure yet still holds a secret, so the wrapper suppresses
    /// the writes for the whole masked lifetime.
    pub fn suppress_clipboard_writes(mut self, suppress: bool) -> Self {
        self.suppress_clipboard_writes = suppress;
        self
    }

    pub fn font(mut self, font: Renderer::Font) -> Self {
        self.text_input = self.text_input.font(font);
        self
    }

    pub fn size(mut self, size: impl Into<iced::Pixels>) -> Self {
        self.text_input = self.text_input.size(size);
        self
    }

    pub fn id(mut self, id: Id) -> Self {
        self.text_input = self.text_input.id(id);
        self
    }

    pub fn on_input(mut self, f: impl Fn(String) -> Message + 'static) -> Self {
        self.text_input = self.text_input.on_input(f);
        self
    }

    pub fn on_key_pressed(mut self, key: keyboard::Key, f: Message) -> Self {
        self.hooks.insert(key, f);
        self
    }

    /// Handle an exact key/modifier chord when no user-defined hotkey matches.
    /// Used for editor-owned commands that remain available as fallbacks while
    /// the command input is focused.
    pub fn on_fallback_key_pressed(
        mut self,
        key: keyboard::Key,
        modifiers: keyboard::Modifiers,
        message: Message,
    ) -> Self {
        self.shortcut_fallbacks.push((key, modifiers, message));
        self
    }

    pub fn on_submit(mut self, f: Message) -> Self {
        self.text_input = self.text_input.on_submit(f);
        self
    }

    pub fn style(
        mut self,
        style: impl Fn(&Theme, iced::widget::text_input::Status) -> iced::widget::text_input::Style + 'a,
    ) -> Self
    where
        Theme::Class<'a>: From<text_input::StyleFn<'a, Theme>>,
    {
        self.text_input = self.text_input.style(style);
        self
    }

    pub fn width(mut self, width: iced::Length) -> Self {
        self.text_input = self.text_input.width(width);
        self
    }

    fn check_hotkey(
        &self,
        key: &keyboard::Key,
        physical_key: &key::Physical,
        modifiers: &keyboard::Modifiers,
    ) -> Option<HotkeyId> {
        #[cfg(feature = "web-audio-cpal")]
        if is_audio_panel_shortcut(key, *modifiers) {
            return None;
        }
        // Create a MaybePhysicalKey from the incoming key for lookup
        let maybe_key = MaybePhysicalKey::Key(key.clone());
        let maybe_physical_key = MaybePhysicalKey::Physical(*physical_key);

        if let Some(modifier_entries) = self.hotkeys.get(&maybe_key) {
            for (required_modifiers, hotkey_id) in modifier_entries {
                if modifiers == required_modifiers {
                    return Some(*hotkey_id);
                }
            }
        }
        if let Some(modifier_entries) = self.hotkeys.get(&maybe_physical_key) {
            for (required_modifiers, hotkey_id) in modifier_entries {
                if modifiers == required_modifiers {
                    return Some(*hotkey_id);
                }
            }
        }
        None
    }

    fn check_shortcut(
        &self,
        key: &keyboard::Key,
        physical_key: &key::Physical,
        modifiers: &keyboard::Modifiers,
    ) -> Option<ShortcutMatch<'_, Message>> {
        if let Some(hotkey_id) = self.check_hotkey(key, physical_key, modifiers) {
            return Some(ShortcutMatch::Hotkey(hotkey_id));
        }

        self.shortcut_fallbacks
            .iter()
            .find(|(fallback_key, fallback_modifiers, _)| {
                fallback_key_matches(fallback_key, key, *physical_key)
                    && fallback_modifiers == modifiers
            })
            .map(|(_, _, message)| ShortcutMatch::Fallback(message))
    }
}

struct State {
    /// Whether the wrapped input held focus as of the previous event, so a
    /// focused→unfocused edge can fire `on_unfocus` exactly once.
    was_focused: bool,
    /// iced retains a text input's internal focus when its OS window becomes
    /// inactive. Track the window half separately so effective focus loses
    /// and regains an edge across an OS focus round trip.
    window_focused: bool,
    /// The caret state as of the previous event, so `on_caret_change` fires
    /// only on an actual change.
    last_caret: Option<CaretState>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            was_focused: false,
            // A newly mounted widget receives no initial window-state query;
            // the hosting window is the safe default until an Unfocused event.
            window_focused: true,
            last_caret: None,
        }
    }
}

impl State {
    fn observe_window_event(&mut self, event: &iced::Event) {
        match event {
            iced::Event::Window(iced::window::Event::Focused) => self.window_focused = true,
            iced::Event::Window(iced::window::Event::Unfocused) => self.window_focused = false,
            _ => {}
        }
    }

    fn effective_focus(&self, widget_focused: bool) -> bool {
        self.window_focused && widget_focused
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for HotkeyMatchingInput<'a, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: text_input::Catalog,
    Renderer: iced::advanced::text::Renderer,
{
    fn children(&self) -> Vec<tree::Tree> {
        vec![Tree::new(
            &self.text_input as &dyn Widget<Message, Theme, Renderer>,
        )]
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn diff(&self, _tree: &mut Tree) {}

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> iced::Size<iced::Length> {
        Widget::<Message, Theme, Renderer>::size(&self.text_input)
    }

    fn size_hint(&self) -> iced::Size<iced::Length> {
        Widget::<Message, Theme, Renderer>::size_hint(&self.text_input)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        Widget::<Message, Theme, Renderer>::layout(
            &mut self.text_input,
            &mut tree.children[0],
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
        Widget::<Message, Theme, Renderer>::draw(
            &self.text_input,
            &tree.children[0],
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
        tree: &Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &Renderer,
    ) -> iced::advanced::mouse::Interaction {
        self.text_input.mouse_interaction(
            tree.children.first().unwrap(),
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let is_focused = {
            let text_input_state = tree.children[0]
                .state
                .downcast_ref::<text_input::State<Renderer::Paragraph>>();

            text_input_state.is_focused()
        };

        if is_focused
            && let iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) = event
        {
            if let Some(shortcut_match) = self.check_shortcut(key, physical_key, modifiers) {
                match shortcut_match {
                    ShortcutMatch::Hotkey(hotkey_id) => {
                        if let Some(on_match) = self.on_match.as_ref() {
                            shell.publish(on_match(hotkey_id));
                        }
                    }
                    ShortcutMatch::Fallback(message) => shell.publish(message.clone()),
                }
                shell.capture_event();
                return;
            }

            #[cfg(feature = "web-audio-cpal")]
            if is_audio_panel_shortcut(key, *modifiers) {
                if !event_key_repeat(event)
                    && let Some(message) = self.on_audio_panel_shortcut.as_ref()
                {
                    shell.publish(message.clone());
                }
                // Capture before TextInput sees command+A. This preserves the
                // command value/caret and makes one typed component event the
                // sole owner of opening the panel.
                shell.capture_event();
                return;
            }

            if let Some(hook) = self.hooks.get(key)
                && modifiers.is_empty()
            {
                shell.publish(hook.clone());
                shell.capture_event();
                return;
            }

            // Swallow copy/cut before the wrapped input can service them
            // (see `suppress_clipboard_writes`). Nothing is published:
            // the keystroke simply lands on nothing.
            if self.suppress_clipboard_writes
                && is_clipboard_write_shortcut(key, *physical_key, *modifiers)
            {
                shell.capture_event();
                return;
            }
        }

        self.text_input.update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        // Publish effective focus transitions after the wrapped widget
        // processes the event. iced keeps the child internally focused while
        // its OS window is inactive, so window focus is an explicit half of
        // the state; this produces both the alt-tab loss and matching gain.
        if self.on_focus.is_some() || self.on_unfocus.is_some() {
            let widget_focused = tree.children[0]
                .state
                .downcast_ref::<text_input::State<Renderer::Paragraph>>()
                .is_focused();
            let state = tree.state.downcast_mut::<State>();
            state.observe_window_event(event);
            let now_focused = state.effective_focus(widget_focused);
            if !state.was_focused
                && now_focused
                && let Some(on_focus) = self.on_focus.as_ref()
            {
                shell.publish(on_focus.clone());
            }
            if state.was_focused
                && !now_focused
                && let Some(on_unfocus) = self.on_unfocus.as_ref()
            {
                shell.publish(on_unfocus.clone());
            }
            state.was_focused = now_focused;
        }

        // Observe the wrapped input's caret after the event settled: focus
        // plus the raw cursor, published as-is (see [`CaretState`]). Only a
        // change publishes, so idle event traffic costs one compare.
        if let Some(on_caret_change) = self.on_caret_change.as_ref() {
            let (widget_focused, cursor) = {
                let input_state = tree.children[0]
                    .state
                    .downcast_ref::<text_input::State<Renderer::Paragraph>>();
                (input_state.is_focused(), input_state.cursor())
            };
            let state = tree.state.downcast_mut::<State>();
            // Keep this path correct even when no focus callbacks were bound.
            state.observe_window_event(event);
            let caret = CaretState {
                focused: state.effective_focus(widget_focused),
                cursor,
            };
            if state.last_caret != Some(caret) {
                state.last_caret = Some(caret);
                shell.publish(on_caret_change(caret));
            }
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.text_input
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
}

#[cfg(feature = "web-audio-cpal")]
fn event_key_repeat(event: &iced::Event) -> bool {
    matches!(
        event,
        iced::Event::Keyboard(keyboard::Event::KeyPressed { repeat: true, .. })
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::{Code, Physical};

    #[test]
    fn effective_focus_round_trips_with_the_os_window() {
        let mut state = State::default();
        assert!(state.effective_focus(true));

        state.observe_window_event(&iced::Event::Window(iced::window::Event::Unfocused));
        assert!(
            !state.effective_focus(true),
            "internal widget focus is inactive with the window"
        );

        state.observe_window_event(&iced::Event::Window(iced::window::Event::Focused));
        assert!(
            state.effective_focus(true),
            "reactivating the window restores effective input focus"
        );
    }

    /// The clipboard-write gate `suppress_clipboard_writes` hangs on: copy
    /// and cut (command-modified) are write shortcuts; paste and unmodified
    /// keys are not (reading the clipboard into a masked box is the user's
    /// own business).
    #[test]
    fn clipboard_write_shortcut_classification() {
        let c = keyboard::Key::Character("c".into());
        let x = keyboard::Key::Character("x".into());
        let v = keyboard::Key::Character("v".into());
        let command = keyboard::Modifiers::COMMAND;

        assert!(is_clipboard_write_shortcut(
            &c,
            Physical::Code(Code::KeyC),
            command
        ));
        assert!(is_clipboard_write_shortcut(
            &x,
            Physical::Code(Code::KeyX),
            command
        ));
        assert!(!is_clipboard_write_shortcut(
            &v,
            Physical::Code(Code::KeyV),
            command
        ));
        assert!(!is_clipboard_write_shortcut(
            &c,
            Physical::Code(Code::KeyC),
            keyboard::Modifiers::empty()
        ));
    }

    #[test]
    fn fallback_character_hooks_ignore_ascii_case() {
        assert!(fallback_key_matches(
            &keyboard::Key::Character("f".into()),
            &keyboard::Key::Character("F".into()),
            Physical::Code(Code::KeyF)
        ));
        assert!(!fallback_key_matches(
            &keyboard::Key::Character("f".into()),
            &keyboard::Key::Character("g".into()),
            Physical::Code(Code::KeyG)
        ));
    }

    /// A non-Latin layout reports a chord's logical key in its own script;
    /// single-letter fallbacks dispatch by the physical latin key instead, so
    /// e.g. ctrl+F opens search from a Cyrillic layout.
    #[test]
    fn fallback_character_hooks_dispatch_by_latin_key_on_non_latin_layouts() {
        // The Ukrainian layout's F key produces Cyrillic 'а'.
        assert!(fallback_key_matches(
            &keyboard::Key::Character("f".into()),
            &keyboard::Key::Character("\u{0430}".into()),
            Physical::Code(Code::KeyF)
        ));
        assert!(!fallback_key_matches(
            &keyboard::Key::Character("f".into()),
            &keyboard::Key::Character("\u{0430}".into()),
            Physical::Code(Code::KeyG)
        ));
    }

    #[test]
    fn user_hotkeys_precede_shortcut_fallbacks() {
        let key = keyboard::Key::Character("f".into());
        let physical_key = Physical::Code(Code::KeyF);
        let modifiers = keyboard::Modifiers::CTRL;
        let hotkey_id = HotkeyId::default();
        let hotkeys = HashMap::from([(
            MaybePhysicalKey::Key(key.clone()),
            vec![(modifiers, hotkey_id)],
        )]);
        let input =
            HotkeyMatchingInput::<u8, smudgy_theme::Theme, iced::Renderer>::new(&hotkeys, "", "")
                .on_fallback_key_pressed(key.clone(), modifiers, 1);

        assert!(matches!(
            input.check_shortcut(&key, &physical_key, &modifiers),
            Some(ShortcutMatch::Hotkey(matched)) if matched == hotkey_id
        ));
    }
}
