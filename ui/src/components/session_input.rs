use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use crate::keymap::{HotkeyKeys, MaybePhysicalKey};
use crate::theme::{Element, builtins};
use crate::update::Update;
use crate::widgets::hotkey_matching_input::HotkeyMatchingInput;
use iced::widget::{Id, operation};
use iced::{Length, Task, keyboard};
use smudgy_core::models::hotkeys::HotkeyDefinition;
use smudgy_core::models::settings::CommandInputBehavior;
use smudgy_core::session::HotkeyId;
use crate::terminal_buffer::TerminalBuffer;

/// A component for inputting text in a session with advanced features
#[derive(Debug, Clone)]
pub struct SessionInput {
    /// The current input value
    value: String,
    /// History of previously submitted commands
    history: VecDeque<String>,
    /// Current position in history navigation (None = not navigating)
    history_index: Option<usize>,
    /// Maximum number of history entries to keep
    max_history: usize,
    /// Current partial completion state
    completion_state: Option<CompletionState>,
    /// Reference to terminal buffer for tab completion
    terminal_buffer: Option<Rc<RefCell<TerminalBuffer>>>,
    /// Active hotkey definitions (pre-processed for efficiency)
    hotkeys: HashMap<HotkeyId, HotkeyKeys>,
    /// Fast lookup table: key -> vec of (modifiers, hotkey_id) pairs
    hotkey_lookup: HashMap<MaybePhysicalKey, Vec<(keyboard::Modifiers, HotkeyId)>>,
    /// Unique ID for the input component
    input_id: Id,
}

#[derive(Debug, Clone)]
struct CompletionState {
    /// The original text before completion started
    original_text: String,
    /// Current completion prefix
    prefix: String,
    /// Words we've already suggested to avoid duplicates
    suggested_words: HashSet<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Input value changed
    InputChanged(String),
    /// Submit the current input
    Submit,
    /// Hotkey triggered
    HotkeyTriggered(HotkeyId),
    /// Navigate history up
    NavigateHistoryUp,
    /// Navigate history down
    NavigateHistoryDown,
    /// Handle tab completion
    HandleTabCompletion,
    /// The input lost focus (used by the clear-on-blur behavior).
    FocusLost,
}

/// What the parent should do in response to an update.
#[derive(Debug, Clone)]
pub enum Event {
    /// The user submitted a command.
    Submit(Arc<String>),
    /// A registered hotkey was triggered.
    HotkeyTriggered(HotkeyId),
}

impl SessionInput {
    /// Create a new session input component
    pub fn new() -> Self {
        Self {
            value: String::new(),
            history: VecDeque::new(),
            history_index: None,
            max_history: 100,
            completion_state: None,
            terminal_buffer: None,
            hotkeys: HashMap::new(),
            hotkey_lookup: HashMap::new(),
            input_id: Id::unique(),
        }
    }

    /// Set the terminal buffer for tab completion
    pub fn with_terminal_buffer(mut self, buffer: Rc<RefCell<TerminalBuffer>>) -> Self {
        self.terminal_buffer = Some(buffer);
        self
    }

    /// Get the current input value
    // Part of the input component's public accessor API; not currently read by
    // the session pane but kept for callers that need the live value.
    #[allow(dead_code)]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Get the unique input ID
    pub fn input_id(&self) -> Id {
        self.input_id.clone()
    }

    /// Clear the input value (and reset completion / history navigation).
    pub fn clear(&mut self) {
        self.value.clear();
        self.completion_state = None;
        self.history_index = None;
    }

    /// Register a new hotkey with the given ID
    ///
    /// If a hotkey with the same ID already exists, it will be replaced.
    ///
    /// # Arguments
    /// * `id` - Unique identifier for the hotkey
    /// * `hotkey_def` - The hotkey definition containing key combinations
    pub fn register_hotkey(&mut self, id: HotkeyId, hotkey_def: HotkeyDefinition) {
        // Get the existing hotkey's main key if it exists
        let existing_main_key = self.hotkeys.get(&id).map(|h| h.main_key.clone());

        // Remove existing hotkey from lookup if it exists
        if let Some(main_key) = existing_main_key {
            self.remove_from_lookup(&main_key, &id);
        }

        let hotkey_keys: HotkeyKeys = hotkey_def.into();

        self.hotkey_lookup
            .entry(hotkey_keys.main_key.clone())
            .or_default()
            .push((hotkey_keys.modifiers, id));

        self.hotkeys.insert(id, hotkey_keys);
    }

    /// Unregister a hotkey by name
    ///
    /// # Arguments
    /// * `id` - The ID of the hotkey to remove
    ///
    /// # Returns
    /// `true` if a hotkey was removed, `false` if no hotkey with that ID existed
    pub fn unregister_hotkey(&mut self, id: &HotkeyId) -> bool {
        if let Some(hotkey_keys) = self.hotkeys.remove(id) {
            self.remove_from_lookup(&hotkey_keys.main_key, id);
            true
        } else {
            false
        }
    }

    /// Clear all registered hotkeys
    pub fn clear_hotkeys(&mut self) {
        self.hotkeys.clear();
        self.hotkey_lookup.clear();
    }

    /// Remove a hotkey from the lookup table
    fn remove_from_lookup(&mut self, main_key: &MaybePhysicalKey, id: &HotkeyId) {
        if let Some(entries) = self.hotkey_lookup.get_mut(main_key) {
            entries.retain(|(_, entry_id)| entry_id != id);
            if entries.is_empty() {
                self.hotkey_lookup.remove(main_key);
            }
        }
    }

    /// Add a command to history
    fn add_to_history(&mut self, command: String) {
        if command.trim().is_empty() {
            return;
        }

        // Remove existing entry if it exists
        if let Some(pos) = self.history.iter().position(|x| x == &command) {
            self.history.remove(pos);
        }

        // Add to front
        self.history.push_front(command);

        // Limit history size
        while self.history.len() > self.max_history {
            self.history.pop_back();
        }

        self.history_index = None;
    }

    /// Navigate history up (to older commands)
    fn navigate_history_up(&mut self) -> Task<Message> {
        if self.history.is_empty() {
            return Task::none();
        }

        let new_index = match self.history_index {
            None => 0,
            Some(i) if i < self.history.len() - 1 => i + 1,
            Some(_) => return Task::none(), // At the end
        };

        self.history_index = Some(new_index);
        self.value = self.history[new_index].clone();
        self.completion_state = None;

        // Select all the text that was filled in
        operation::select_all(self.input_id.clone())
    }

    /// Navigate history down (to newer commands)
    fn navigate_history_down(&mut self) -> Task<Message> {
        match self.history_index {
            None => {
                if self.completion_state.is_some() {
                    self.add_to_history(self.value.clone());
                    self.value = self
                        .completion_state
                        .as_ref()
                        .unwrap()
                        .original_text
                        .clone();
                    self.completion_state = None;
                    Task::none()
                } else {
                    Task::none()
                }
            }
            Some(0) => {
                self.history_index = None;
                self.value.clear();
                // No selection needed for empty text
                Task::none()
            }
            Some(i) => {
                let new_index = i - 1;
                self.history_index = Some(new_index);
                self.value = self.history[new_index].clone();
                self.completion_state = None;

                // Select all the text that was filled in
                operation::select_all(self.input_id.clone())
            }
        }
    }

    /// Handle tab completion
    fn handle_tab_completion(&mut self) -> Task<Message> {
        let Some(buffer_ref) = &self.terminal_buffer else {
            return Task::none();
        };

        // Find the word at cursor position
        let cursor_pos = self.value.len(); // Assuming cursor is at end
        let word_start = self.value[..cursor_pos]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);

        if word_start >= cursor_pos {
            return Task::none();
        }

        let word_prefix = &self.value[word_start..cursor_pos];
        if word_prefix.is_empty() {
            return Task::none();
        }

        // Initialize or update completion state
        let completion_state = self
            .completion_state
            .get_or_insert_with(|| CompletionState {
                original_text: self.value.clone(),
                prefix: word_prefix.to_string(),
                suggested_words: HashSet::new(),
            });

        if let Ok(buffer_ref) = buffer_ref.try_borrow()
            && let Some(word) = buffer_ref.find_recent_word_by_prefix(
                &completion_state.prefix,
                Some(&completion_state.suggested_words),
                1000, // Search last 1000 lines
            ) {
                completion_state.suggested_words.insert(word.clone());

                // Replace the current word with the completion
                let mut new_value = String::with_capacity(self.value.len() + word.len());
                new_value.push_str(&self.value[..word_start]);
                new_value.push_str(&word);
                new_value.push_str(&self.value[cursor_pos..]);

                // Calculate selection range: from end of ORIGINAL prefix to end of completed word
                let original_prefix_end = word_start + completion_state.prefix.len();
                let completion_end = word_start + word.len();

                self.value = new_value;

                // Select only the newly completed portion
                if completion_end > original_prefix_end {
                    return operation::select_range(
                        self.input_id.clone(),
                        original_prefix_end,
                        completion_end,
                    );
                }
            }

        Task::none()
    }

    /// Update the component state based on messages
    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        match message {
            Message::InputChanged(value) => {
                self.value = value;
                self.completion_state = None;
                self.history_index = None;
                Update::none()
            }
            Message::Submit => {
                if !self.value.trim().is_empty() {
                    self.add_to_history(self.value.clone());
                }
                let command = Arc::new(self.value.clone());

                // How the just-sent text is treated is user-configurable.
                let task = match crate::prefs::current().command_input_behavior {
                    CommandInputBehavior::Clear => {
                        self.clear();
                        Task::none()
                    }
                    // Both select-all modes leave the text in place but fully
                    // selected, so the next keystroke overwrites it. The
                    // clear-on-blur half of the default lives in `FocusLost`.
                    CommandInputBehavior::SelectAll | CommandInputBehavior::SelectAllClearOnBlur => {
                        operation::select_all(self.input_id.clone())
                    }
                };

                Update::new(task, Some(Event::Submit(command)))
            }
            Message::FocusLost => {
                // Only the default mode wipes the line (sent-and-selected, or
                // half-typed) when the input loses focus; the others leave it.
                if crate::prefs::current().command_input_behavior
                    == CommandInputBehavior::SelectAllClearOnBlur
                {
                    self.clear();
                }
                Update::none()
            }
            Message::HotkeyTriggered(hotkey_id) => {
                Update::with_event(Event::HotkeyTriggered(hotkey_id))
            }
            Message::NavigateHistoryUp => Update::with_task(self.navigate_history_up()),
            Message::NavigateHistoryDown => Update::with_task(self.navigate_history_down()),
            Message::HandleTabCompletion => Update::with_task(self.handle_tab_completion()),
        }
    }

    /// Render the component
    pub fn view(&self) -> Element<'_, Message> {
        let prefs = crate::prefs::current();

        let input = HotkeyMatchingInput::<Message, crate::theme::Theme, iced::Renderer>::new(
            &self.hotkey_lookup,
            "",
            &self.value,
        )
        .font(prefs.font)
        .size(prefs.font_size)
        .id(self.input_id.clone())
        .on_input(Message::InputChanged)
        .on_submit(Message::Submit)
        .on_unfocus(Message::FocusLost)
        .style(builtins::text_input::borderless)
        .width(Length::Fill)
        .on_match(Message::HotkeyTriggered)
        .on_key_pressed(
            keyboard::Key::Named(keyboard::key::Named::ArrowUp),
            Message::NavigateHistoryUp,
        )
        .on_key_pressed(
            keyboard::Key::Named(keyboard::key::Named::ArrowDown),
            Message::NavigateHistoryDown,
        )
        .on_key_pressed(
            keyboard::Key::Named(keyboard::key::Named::Tab),
            Message::HandleTabCompletion,
        );

        Element::new(input)
    }
}
