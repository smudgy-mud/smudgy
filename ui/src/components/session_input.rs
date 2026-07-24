use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use crate::keymap::{HotkeyKeys, MaybePhysicalKey};
use crate::terminal_buffer::TerminalBuffer;
use crate::theme::{Element, builtins};
use crate::update::Update;
use crate::widgets::hotkey_matching_input::{CaretState, HotkeyMatchingInput};
use iced::advanced::widget::operation::{Focusable, Operation};
use iced::widget::{Id, Space, operation, row, text_input};
use iced::{Alignment, Length, Task, keyboard};
use smudgy_core::models::hotkeys::HotkeyDefinition;
use smudgy_core::models::settings::CommandInputBehavior;
use smudgy_core::session::HotkeyId;
use smudgy_core::session::runtime::input::{InputOp, InputSnapshot, InputSource};
use unicode_segmentation::UnicodeSegmentation;

/// The UTF-16 code-unit index (the script-facing unit: JS string indexing)
/// of the boundary before grapheme `index` in `value`. An `index` past the
/// last grapheme lands on the end of the string.
fn grapheme_to_utf16(value: &str, index: usize) -> usize {
    value
        .graphemes(true)
        .take(index)
        .map(|g| g.encode_utf16().count())
        .sum()
}

/// The grapheme index in `value` whose boundary sits at (or, for a position
/// inside a grapheme, immediately before) UTF-16 code-unit position `utf16`.
/// Positions past the end clamp to the last boundary — a grapheme is never
/// split.
fn utf16_to_grapheme(value: &str, utf16: usize) -> usize {
    let mut units = 0;
    let mut graphemes = 0;
    for g in value.graphemes(true) {
        let next = units + g.encode_utf16().count();
        if next > utf16 {
            break;
        }
        units = next;
        graphemes += 1;
    }
    graphemes
}

/// An [`Operation`] that unfocuses exactly the widget carrying `target`'s id.
/// The stock `focusable::unfocus` releases whatever holds focus anywhere in
/// the tree; a scripted `blur()` must never do that — when focus has already
/// moved on to another widget, this lands on nothing.
struct UnfocusTarget {
    target: Id,
}

impl<T> Operation<T> for UnfocusTarget {
    fn focusable(&mut self, id: Option<&Id>, _bounds: iced::Rectangle, state: &mut dyn Focusable) {
        if id == Some(&self.target) {
            state.unfocus();
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        operate(self);
    }
}

/// A [`Task`] running [`UnfocusTarget`] against `target`.
fn unfocus_target<T: Send + 'static>(target: Id) -> Task<T> {
    iced::advanced::widget::operate(UnfocusTarget { target })
}

/// A component for inputting text in a session with advanced features
#[derive(Debug, Clone)]
pub struct SessionInput {
    /// The current input value
    value: String,
    /// History of previously submitted commands, newest first. Entries are
    /// shared (`Arc`) so the session-thread history mirror snapshots them
    /// without copying.
    history: VecDeque<Arc<String>>,
    /// Current position in history navigation (None = not navigating)
    history_index: Option<usize>,
    /// Maximum number of history entries to keep
    max_history: usize,
    /// Bumped on every actual history change (a submission or scripted push
    /// entering it, a scripted clear emptying it), so the parent can feed the
    /// session-thread history mirror exactly when there is something new —
    /// never per keystroke (`docs/input.md` §3.9).
    history_revision: u64,
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
    /// The caret (focus + raw cursor) as last observed on the widget, feeding
    /// the session-thread input mirror. Raw: positions are clamped against
    /// the current value only when a snapshot is built.
    caret: CaretState,
    /// Whether the session thread wants mirror updates. The caret observer is
    /// attached only while set — a session that never reads its input from a
    /// script publishes no per-caret-move messages.
    mirror_interest: bool,
    /// What caused the most recent state change, riding the next mirror
    /// update (coalescing means last-mutation-wins, as documented).
    last_source: InputSource,
    /// The attribution for the next observed caret change: the echo of a
    /// caret-moving task carries its cause; an unheralded caret move is the
    /// user's.
    pending_caret_echo: Option<InputSource>,
    /// Masked (password) mode — the EFFECTIVE state every suppression reads.
    /// While masked: submissions skip history, tab completion and history
    /// recall are off, the mirror snapshot carries no content, and the
    /// submission's echo is masked. Derived from the two causes below: the
    /// input is masked while EITHER is active, so a telnet unmask can never
    /// release a script-set mask (or vice versa).
    masked: bool,
    /// The script cause: `input.masked = true` (`InputOp::SetMasked`).
    masked_by_script: bool,
    /// The telnet cause: the server holds ECHO (`SessionEvent::ServerEcho`,
    /// pref-gated by the parent — `docs/input.md` §3.10).
    masked_by_telnet: bool,
    /// The masked eye affordance: reveal the glyphs on screen. Rendering-only
    /// — every masked suppression stays in force while revealed.
    masked_reveal: bool,
    /// The pre-mask stash (`docs/input.md` §3.10): a leftover or
    /// in-progress command captured when masked mode engaged, restored
    /// (select-all'd, matching the post-submit state) when it releases. UI
    /// side only; it never crosses to the session thread and no script can
    /// read it.
    stash: Option<String>,
    /// Whether the buffer currently holds a just-submitted command left in
    /// place by the select-all post-submit behaviors. Component-owned so the
    /// pre-mask stash heuristic never depends on live caret state; any edit
    /// to the value clears it.
    post_submit_selected: bool,
    /// Script-registered completion suggestions, merged across every
    /// contributor by the session (creators in first-contribution order,
    /// words in insertion order, deduplicated case-insensitively). Offered
    /// before the scrollback scan on Tab; replaced wholesale by each
    /// `SessionEvent::InputWordSets`.
    suggestions: Arc<Vec<Arc<String>>>,
    /// The merged completion blacklist, lowercase-folded. Filters BOTH
    /// completion sources (suggestions and the scrollback scan),
    /// case-insensitively.
    blacklist: Arc<HashSet<String>>,
    /// Hint text shown while the input is empty. Empty for the main input;
    /// pane inputs carry their spec's `placeholder`.
    placeholder: String,
    /// Whether Escape moves focus to the session's main input (the pane-input
    /// convention). The component only reports the request
    /// ([`Event::FocusMain`]); the parent owns the main input's id.
    escape_to_main: bool,
}

#[derive(Debug, Clone)]
struct CompletionState {
    /// The original text before completion started
    original_text: String,
    /// Current completion prefix
    prefix: String,
    /// Every word offered this cycling run, exactly as inserted — the
    /// scrollback scan's skip set, so cycling never re-offers the same
    /// string. Exact-match on purpose: differently-cased scrollback words
    /// ("Zurek"/"zurek") are distinct candidates and cycle in turn.
    suggested_words: HashSet<String>,
    /// Lowercase folds of the offered **registered** suggestions only —
    /// scrollback offers never land here. Skips both sources: the suggestion
    /// scan does not re-offer them, and the scrollback scan does not offer a
    /// registered word back under another casing ("Hello" offered must not
    /// return as a scrollback "hello").
    suggested_folded: HashSet<String>,
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
    /// Escape pressed in a pane input: hand focus back to the main input.
    EscapePressed,
    /// The widget's caret (focus/cursor) changed.
    CaretChanged(CaretState),
    /// The masked eye affordance: toggle the on-screen reveal.
    ToggleMaskedReveal,
}

/// What the parent should do in response to an update.
#[derive(Debug, Clone)]
pub enum Event {
    /// A command was submitted (by the user, or a script's `submit()`).
    /// `masked` tells the parent to send it with a masked echo.
    Submit { text: Arc<String>, masked: bool },
    /// A registered hotkey was triggered.
    HotkeyTriggered(HotkeyId),
    /// The user pressed Escape in a pane input; the parent should focus the
    /// session's main input.
    FocusMain,
}

/// Why an input is masked. The two causes are tracked separately and the
/// input renders masked while either is active, so a telnet unmask cannot
/// release a script-set mask (and vice versa) — see
/// [`SessionInput::set_mask_cause`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaskCause {
    /// A script set `input.masked` (`InputOp::SetMasked`).
    Script,
    /// The server holds the telnet ECHO option (`SessionEvent::ServerEcho`).
    Telnet,
}

impl SessionInput {
    /// Create a new session input component
    pub fn new() -> Self {
        Self {
            value: String::new(),
            history: VecDeque::new(),
            history_index: None,
            max_history: 100,
            history_revision: 0,
            completion_state: None,
            terminal_buffer: None,
            hotkeys: HashMap::new(),
            hotkey_lookup: HashMap::new(),
            input_id: Id::unique(),
            caret: CaretState::default(),
            mirror_interest: false,
            last_source: InputSource::Other,
            pending_caret_echo: None,
            masked: false,
            masked_by_script: false,
            masked_by_telnet: false,
            masked_reveal: false,
            stash: None,
            post_submit_selected: false,
            suggestions: Arc::new(Vec::new()),
            blacklist: Arc::new(HashSet::new()),
            placeholder: String::new(),
            escape_to_main: false,
        }
    }

    /// Set the terminal buffer for tab completion
    pub fn with_terminal_buffer(mut self, buffer: Rc<RefCell<TerminalBuffer>>) -> Self {
        self.terminal_buffer = Some(buffer);
        self
    }

    /// Set the hint text shown while the input is empty.
    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.to_string();
        self
    }

    /// Make Escape hand focus back to the session's main input (the pane-
    /// input convention; the main input itself never sets this).
    pub fn with_escape_to_main(mut self) -> Self {
        self.escape_to_main = true;
        self
    }

    /// Adopt another input's registered hotkey tables — the seed for a pane
    /// input created after the session's hotkeys registered, so session
    /// hotkeys keep firing while the pane input is focused. Later
    /// registrations fan out to every input, keeping the copies in step.
    pub fn copy_hotkeys_from(&mut self, other: &SessionInput) {
        self.hotkeys = other.hotkeys.clone();
        self.hotkey_lookup = other.hotkey_lookup.clone();
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

    /// The session thread wants mirror updates from now on. Sticky, like the
    /// mirror's own interest flag; attaches the caret observer in `view()`.
    pub fn set_mirror_interest(&mut self) {
        self.mirror_interest = true;
    }

    /// Replace the script-registered completion word sets (the merged view the
    /// session pushes on every registration change — and re-pushes after a
    /// script reload, so words whose registering script did not come back are
    /// dropped here too). `blacklist` arrives lowercase-folded.
    pub fn set_word_sets(
        &mut self,
        suggestions: Arc<Vec<Arc<String>>>,
        blacklist: Arc<HashSet<String>>,
    ) {
        self.suggestions = suggestions;
        self.blacklist = blacklist;
    }

    /// What caused the most recent state change (rides the mirror update).
    pub fn last_change_source(&self) -> InputSource {
        self.last_source
    }

    /// The shared bookkeeping for any edit to `value`: completion and history
    /// navigation restart from the new text, and the post-submit selected
    /// state is no longer in force.
    fn note_value_edited(&mut self) {
        self.completion_state = None;
        self.history_index = None;
        self.post_submit_selected = false;
    }

    /// Clear the input value (and reset completion / history navigation).
    pub fn clear(&mut self) {
        self.value.clear();
        self.note_value_edited();
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

    /// Add a command to history: deduplicated, pushed to the front, capped.
    /// A typed submission and a scripted `history.push()` share this path, so
    /// their semantics can never drift. Bumps the revision only when the
    /// entries actually changed (re-submitting the front entry is a no-op).
    fn add_to_history(&mut self, command: Arc<String>) {
        if command.trim().is_empty() {
            return;
        }

        self.history_index = None;

        // Already the newest entry: dedup + push-front would change nothing.
        if self.history.front() == Some(&command) {
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

        self.history_revision += 1;
    }

    /// Remove every history entry (the scripted `history.clear()`). Bumps the
    /// revision only when there were entries to remove; the recall position
    /// resets either way — a stale index into a gone list must not survive.
    fn clear_history(&mut self) {
        if !self.history.is_empty() {
            self.history.clear();
            self.history_revision += 1;
        }
        self.history_index = None;
    }

    /// The history revision (see the field docs): compare against the last
    /// value synced to know whether a fresh [`Self::history_snapshot`] is due.
    pub fn history_revision(&self) -> u64 {
        self.history_revision
    }

    /// The history entries, newest first, for the session-thread mirror.
    /// Clones only the `Arc`s; built once per actual history change.
    pub fn history_snapshot(&self) -> Arc<Vec<Arc<String>>> {
        Arc::new(self.history.iter().cloned().collect())
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
        self.value = self.history[new_index].as_str().to_string();
        self.completion_state = None;

        // Select all the text that was filled in
        self.pending_caret_echo = Some(InputSource::Other);
        operation::select_all(self.input_id.clone())
    }

    /// Navigate history down (to newer commands)
    fn navigate_history_down(&mut self) -> Task<Message> {
        match self.history_index {
            None => {
                if self.completion_state.is_some() {
                    self.add_to_history(Arc::new(self.value.clone()));
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
                self.value = self.history[new_index].as_str().to_string();
                self.completion_state = None;

                // Select all the text that was filled in
                self.pending_caret_echo = Some(InputSource::Other);
                operation::select_all(self.input_id.clone())
            }
        }
    }

    /// Handle tab completion
    fn handle_tab_completion(&mut self) -> Task<Message> {
        // Completion is off while masked: cycling scrollback words into a
        // password box makes no sense, and the mechanism must never observe
        // the secret prefix.
        if self.masked {
            return Task::none();
        }

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
                suggested_folded: HashSet::new(),
            });

        // Candidate order: script-registered suggestions first, in merge
        // order (creators in first-contribution order, words in insertion
        // order), then the scrollback recency scan. The blacklist filters
        // both sources; prefix matching and blacklisting are
        // case-insensitive, and a registered word is inserted with its
        // registered casing. The scrollback scan skips offered words by
        // exact match, folding only against the blacklist and the offered
        // REGISTERED suggestions — with empty word sets, cycling is the
        // plain scrollback behavior, casing pairs and all.
        let folded_prefix = completion_state.prefix.to_lowercase();
        let mut candidate = self.suggestions.iter().find_map(|word| {
            let folded = word.to_lowercase();
            (folded.starts_with(&folded_prefix)
                && !self.blacklist.contains(&folded)
                && !completion_state.suggested_folded.contains(&folded))
            .then(|| word.as_str().to_string())
        });
        // The scrollback scan needs a buffer; an input without one (a
        // widgets-only pane's) completes from the suggestion sets alone.
        let from_suggestions = candidate.is_some();
        if candidate.is_none()
            && let Some(buffer_ref) = &self.terminal_buffer
            && let Ok(buffer_ref) = buffer_ref.try_borrow()
        {
            candidate = buffer_ref.find_recent_word_by_prefix(
                &completion_state.prefix,
                Some(&completion_state.suggested_words),
                &[&*self.blacklist, &completion_state.suggested_folded],
                1000, // Search last 1000 lines
            );
        }

        if let Some(word) = candidate {
            completion_state.suggested_words.insert(word.clone());
            if from_suggestions {
                completion_state
                    .suggested_folded
                    .insert(word.to_lowercase());
            }

            // Replace the current word with the completion
            let mut new_value = String::with_capacity(self.value.len() + word.len());
            new_value.push_str(&self.value[..word_start]);
            new_value.push_str(&word);
            new_value.push_str(&self.value[cursor_pos..]);

            // Calculate selection range: from end of ORIGINAL prefix to end of completed word
            let original_prefix_end = word_start + completion_state.prefix.len();
            let completion_end = word_start + word.len();

            self.value = new_value;
            self.post_submit_selected = false;

            // Select only the newly completed portion
            if completion_end > original_prefix_end {
                self.pending_caret_echo = Some(InputSource::Other);
                return operation::select_range(
                    self.input_id.clone(),
                    original_prefix_end,
                    completion_end,
                );
            }
        }

        Task::none()
    }

    /// The full submit path — the user's Enter and a script's `submit()` take
    /// exactly this route. Masked submissions skip history; the configured
    /// post-submit behavior applies either way.
    fn submit(&mut self) -> Update<Message, Event> {
        let command = Arc::new(self.value.clone());
        if !self.masked && !command.trim().is_empty() {
            self.add_to_history(command.clone());
        }
        let masked = self.masked;
        // Post-submit behavior mutates the box, whoever submitted.
        self.last_source = InputSource::Other;

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
                self.post_submit_selected = true;
                self.pending_caret_echo = Some(InputSource::Other);
                operation::select_all(self.input_id.clone())
            }
        };

        Update::new(
            task,
            Some(Event::Submit {
                text: command,
                masked,
            }),
        )
    }

    /// Record one mask cause and settle the effective state on its edges
    /// (`docs/input.md` §3.10): the input is masked while EITHER
    /// cause is active, so releasing one cause while the other holds changes
    /// nothing — a server `WONT ECHO` never unmasks a script-set mask, and a
    /// script's `masked = false` never unmasks a telnet-held one. The
    /// engage/release effects (stash, restore, reveal reset) run only on the
    /// effective edges.
    fn set_mask_cause(&mut self, cause: MaskCause, engaged: bool) -> Task<Message> {
        match cause {
            MaskCause::Script => self.masked_by_script = engaged,
            MaskCause::Telnet => self.masked_by_telnet = engaged,
        }
        if self.masked_by_script || self.masked_by_telnet {
            self.engage_mask();
            Task::none()
        } else {
            self.release_mask()
        }
    }

    /// The telnet cause of masked mode, driven by the server's ECHO option
    /// (the parent applies the user's auto-mask preference before calling).
    /// Composes with a script-set mask via [`Self::set_mask_cause`].
    pub fn set_telnet_mask(&mut self, engaged: bool) -> Update<Message, Event> {
        self.last_source = InputSource::Other;
        Update::with_task(self.set_mask_cause(MaskCause::Telnet, engaged))
    }

    /// Engage masked mode (`docs/input.md` §3.10). A nonempty buffer
    /// is triaged: a leftover/in-progress command — sitting in the
    /// post-submit select-all state (the component-owned flag; never live
    /// caret state, which can be stale or unobserved) or matching a history
    /// entry — is stashed and the box cleared; anything else is an
    /// early-typed secret prefix and stays in the now-masked box, never
    /// stashed and never restored.
    fn engage_mask(&mut self) {
        if self.masked {
            return;
        }
        self.masked = true;
        self.masked_reveal = false;
        if self.value.is_empty() {
            return;
        }
        let leftover = self.post_submit_selected
            || self
                .history
                .iter()
                .any(|entry| entry.as_str() == self.value);
        if leftover {
            self.stash = Some(std::mem::take(&mut self.value));
            self.note_value_edited();
        }
    }

    /// Release masked mode. Unsubmitted masked content is cleared BEFORE the
    /// stash restores — unmasking must never reveal what was typed while
    /// masked (a bare `masked = false` would otherwise hand the secret to the
    /// mirror). A restored stash comes back fully selected, matching the
    /// post-submit state it was captured in, so typing replaces it.
    fn release_mask(&mut self) -> Task<Message> {
        if !self.masked {
            return Task::none();
        }
        self.masked = false;
        self.masked_reveal = false;
        self.value.clear();
        self.note_value_edited();
        if let Some(stash) = self.stash.take() {
            self.value = stash;
            self.post_submit_selected = true;
            self.pending_caret_echo = Some(InputSource::Script);
            return operation::select_all(self.input_id.clone());
        }
        Task::none()
    }

    /// Replace the buffer from a script write, resetting completion/history
    /// navigation like typed input does.
    fn set_value_from_script(&mut self, text: &str) {
        self.value = text.to_string();
        self.note_value_edited();
    }

    /// Apply one scripted input mutation (`SessionEvent::InputOp`). Value and
    /// completion state change here; caret effects ride the returned iced
    /// operation, and the widget's own caret observation feeds them back into
    /// the mirror — reads are eventually consistent by contract, with the
    /// observer as the sole caret feeder.
    pub fn apply_script_op(&mut self, op: &InputOp) -> Update<Message, Event> {
        self.last_source = InputSource::Script;
        match op {
            InputOp::Replace(text) => {
                self.set_value_from_script(text);
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(operation::move_cursor_to_end(self.input_id.clone()))
            }
            InputOp::Append(text) => {
                self.value.push_str(text);
                self.note_value_edited();
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(operation::move_cursor_to_end(self.input_id.clone()))
            }
            InputOp::Clear => {
                self.clear();
                Update::none()
            }
            InputOp::Propose(text) => {
                self.set_value_from_script(text);
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(operation::select_all(self.input_id.clone()))
            }
            InputOp::SetCursor(pos) => {
                // Script positions are UTF-16 code units; the widget speaks
                // graphemes. Clamped by the conversion.
                let pos = utf16_to_grapheme(&self.value, *pos);
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(operation::move_cursor_to(self.input_id.clone(), pos))
            }
            InputOp::Select(start, end) => {
                let start = utf16_to_grapheme(&self.value, *start);
                let end = utf16_to_grapheme(&self.value, *end);
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(operation::select_range(self.input_id.clone(), start, end))
            }
            InputOp::SelectAll => {
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(operation::select_all(self.input_id.clone()))
            }
            InputOp::Focus => {
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(operation::focus(self.input_id.clone()))
            }
            InputOp::Blur => {
                // Targeted: only this input is released, and only if it still
                // holds focus — never whatever else focus moved on to.
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(unfocus_target(self.input_id.clone()))
            }
            InputOp::Submit => self.submit(),
            InputOp::HistoryPush(text) => {
                // The scripted half of history entry: exactly a typed
                // submission's dedup/push-front/cap, without sending. The
                // buffer, caret, and completion state are untouched.
                self.add_to_history(text.clone());
                Update::none()
            }
            InputOp::HistoryClear => {
                self.clear_history();
                Update::none()
            }
            InputOp::SetMasked(masked) => {
                Update::with_task(self.set_mask_cause(MaskCause::Script, *masked))
            }
        }
    }

    /// The state the session-thread mirror should hold for this input. While
    /// masked the snapshot carries no content — the flags travel, the secret
    /// does not. Caret positions are clamped against the current value here
    /// (the observation is raw) and converted to UTF-16 code units, the
    /// script-facing unit.
    pub fn mirror_snapshot(&self) -> InputSnapshot {
        if self.masked {
            return InputSnapshot {
                value: Arc::new(String::new()),
                cursor: 0,
                selection: None,
                focused: self.caret.focused,
                masked: true,
            };
        }
        let value = text_input::Value::new(&self.value);
        let cursor = match self.caret.cursor.state(&value) {
            text_input::cursor::State::Index(index) => index,
            text_input::cursor::State::Selection { end, .. } => end,
        };
        let selection = self.caret.cursor.selection(&value).map(|(start, end)| {
            (
                grapheme_to_utf16(&self.value, start),
                grapheme_to_utf16(&self.value, end),
            )
        });
        InputSnapshot {
            value: Arc::new(self.value.clone()),
            cursor: grapheme_to_utf16(&self.value, cursor),
            selection,
            focused: self.caret.focused,
            masked: false,
        }
    }

    /// Update the component state based on messages
    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        match message {
            Message::InputChanged(value) => {
                self.value = value;
                self.note_value_edited();
                self.last_source = InputSource::User;
                self.pending_caret_echo = None;
                Update::none()
            }
            Message::Submit => self.submit(),
            Message::FocusLost => {
                self.last_source = InputSource::Other;
                // Only the default mode wipes the line (sent-and-selected, or
                // half-typed) when the input loses focus; the others leave
                // it. Never while masked — clicking the reveal eye blurs the
                // input for a moment, and that must not cost the user the
                // secret (or the stash discipline its buffer).
                if !self.masked
                    && crate::prefs::current().command_input_behavior
                        == CommandInputBehavior::SelectAllClearOnBlur
                {
                    self.clear();
                }
                Update::none()
            }
            Message::HotkeyTriggered(hotkey_id) => {
                Update::with_event(Event::HotkeyTriggered(hotkey_id))
            }
            Message::EscapePressed => Update::with_event(Event::FocusMain),
            // History recall is off while masked: cycling past commands into
            // a password box both loses the secret and hands old commands to
            // whatever asked for a password.
            Message::NavigateHistoryUp if self.masked => Update::none(),
            Message::NavigateHistoryDown if self.masked => Update::none(),
            Message::NavigateHistoryUp => {
                self.last_source = InputSource::Other;
                Update::with_task(self.navigate_history_up())
            }
            Message::NavigateHistoryDown => {
                self.last_source = InputSource::Other;
                Update::with_task(self.navigate_history_down())
            }
            Message::HandleTabCompletion => {
                self.last_source = InputSource::Other;
                Update::with_task(self.handle_tab_completion())
            }
            Message::CaretChanged(caret) => {
                self.caret = caret;
                self.last_source = self.pending_caret_echo.take().unwrap_or(InputSource::User);
                Update::none()
            }
            Message::ToggleMaskedReveal => {
                self.masked_reveal = !self.masked_reveal;
                self.last_source = InputSource::Other;
                self.pending_caret_echo = Some(InputSource::Other);
                // Clicking the eye moved focus onto the button; hand it
                // straight back so typing continues without a re-click.
                Update::with_task(operation::focus(self.input_id.clone()))
            }
        }
    }

    /// Render the component
    pub fn view(&self) -> Element<'_, Message> {
        let prefs = crate::prefs::current();

        let input = HotkeyMatchingInput::<Message, crate::theme::Theme, iced::Renderer>::new(
            &self.hotkey_lookup,
            &self.placeholder,
            &self.value,
        )
        .font(prefs.font)
        .size(prefs.font_size)
        .id(self.input_id.clone())
        .secure(self.masked && !self.masked_reveal)
        .suppress_clipboard_writes(self.masked)
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

        // Pane inputs hand focus back to the main input on Escape.
        let input = if self.escape_to_main {
            input.on_key_pressed(
                keyboard::Key::Named(keyboard::key::Named::Escape),
                Message::EscapePressed,
            )
        } else {
            input
        };

        // The caret observer costs a message per caret move, so it is
        // attached only while the session thread wants mirror state.
        let input = if self.mirror_interest {
            input.on_caret_change(Message::CaretChanged)
        } else {
            input
        };

        // The eye slot: the built-in show/hide affordance while masked (a
        // rendering toggle, never an unmask — every masked suppression stays
        // in force, so the user can check their own typing without opening a
        // script-visible window), a zero-size placeholder otherwise.
        let eye_slot: Element<'_, Message> = if self.masked {
            crate::session_store::title_bar_icon_button(
                if self.masked_reveal {
                    crate::assets::hero_icons::EYE_SLASH.clone()
                } else {
                    crate::assets::hero_icons::EYE.clone()
                },
                Message::ToggleMaskedReveal,
            )
        } else {
            Space::new().into()
        };

        // One element shape in both modes: the same row, the input always at
        // child index 0. iced pairs child state positionally and recreates a
        // subtree when its tag changes, so swapping a bare input for a row
        // across a mask toggle would destroy the text input's focus/cursor
        // state at exactly the password-prompt moment.
        row![Element::new(input), eye_slot]
            .spacing(if self.masked { 4 } else { 0 })
            .align_y(Alignment::Center)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_core::session::styled_line::StyledLine;

    /// Submit a command unmasked (seeding history) via the real submit path.
    fn submit_unmasked(input: &mut SessionInput, text: &str) {
        let _ = input.update(Message::InputChanged(text.to_string()));
        let update = input.update(Message::Submit);
        match update.event {
            Some(Event::Submit { masked, .. }) => assert!(!masked),
            other => panic!("expected a Submit event, got {other:?}"),
        }
    }

    #[test]
    fn utf16_conversions_handle_emoji() {
        // '\u{1F44D}' (thumbs up) is one grapheme, two UTF-16 code units.
        let value = "a\u{1F44D}b";
        assert_eq!(grapheme_to_utf16(value, 0), 0);
        assert_eq!(grapheme_to_utf16(value, 1), 1);
        assert_eq!(grapheme_to_utf16(value, 2), 3);
        assert_eq!(grapheme_to_utf16(value, 3), 4);

        assert_eq!(utf16_to_grapheme(value, 0), 0);
        assert_eq!(utf16_to_grapheme(value, 1), 1);
        // A position inside the surrogate pair snaps back to its start.
        assert_eq!(utf16_to_grapheme(value, 2), 1);
        assert_eq!(utf16_to_grapheme(value, 3), 2);
        // Past the end clamps to the last boundary.
        assert_eq!(utf16_to_grapheme(value, 400), 3);
    }

    #[test]
    fn utf16_conversions_handle_zwj_clusters_and_combining_marks() {
        // A ZWJ family: one grapheme spanning 8 UTF-16 code units
        // (2 + 1 + 2 + 1 + 2).
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(grapheme_to_utf16(family, 1), 8);
        assert_eq!(utf16_to_grapheme(family, 8), 1);
        // Positions anywhere inside the cluster never split it.
        for utf16 in 1..8 {
            assert_eq!(utf16_to_grapheme(family, utf16), 0);
        }

        // A combining acute: "e" + U+0301 is one grapheme, two code units.
        let combining = "e\u{301}x";
        assert_eq!(grapheme_to_utf16(combining, 1), 2);
        assert_eq!(grapheme_to_utf16(combining, 2), 3);
        assert_eq!(utf16_to_grapheme(combining, 1), 0);
        assert_eq!(utf16_to_grapheme(combining, 2), 1);
    }

    /// The targeted unfocus operation releases exactly the widget with the
    /// target id — an unrelated focus holder (where focus may have moved
    /// after a stale `blur()` was issued) is left alone.
    #[test]
    fn unfocus_target_releases_only_the_target() {
        struct FakeFocusable {
            focused: bool,
        }
        impl Focusable for FakeFocusable {
            fn is_focused(&self) -> bool {
                self.focused
            }
            fn focus(&mut self) {
                self.focused = true;
            }
            fn unfocus(&mut self) {
                self.focused = false;
            }
        }

        let target = Id::unique();
        let other = Id::unique();
        let mut op = UnfocusTarget {
            target: target.clone(),
        };

        let mut unrelated = FakeFocusable { focused: true };
        Operation::<()>::focusable(
            &mut op,
            Some(&other),
            iced::Rectangle::default(),
            &mut unrelated,
        );
        assert!(unrelated.focused, "an unrelated widget keeps focus");

        let mut anonymous = FakeFocusable { focused: true };
        Operation::<()>::focusable(&mut op, None, iced::Rectangle::default(), &mut anonymous);
        assert!(anonymous.focused, "an id-less widget keeps focus");

        let mut targeted = FakeFocusable { focused: true };
        Operation::<()>::focusable(
            &mut op,
            Some(&target),
            iced::Rectangle::default(),
            &mut targeted,
        );
        assert!(!targeted.focused, "the target is released");
    }

    #[test]
    fn masked_submission_is_excluded_from_history() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "look");
        assert!(input.history.iter().any(|entry| entry.as_str() == "look"));

        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.update(Message::InputChanged("hunter2".to_string()));
        let update = input.update(Message::Submit);
        match update.event {
            Some(Event::Submit { text, masked }) => {
                assert_eq!(text.as_str(), "hunter2");
                assert!(masked, "a masked submission reports itself masked");
            }
            other => panic!("expected a Submit event, got {other:?}"),
        }
        assert!(
            !input
                .history
                .iter()
                .any(|entry| entry.as_str() == "hunter2"),
            "a masked submission must never enter history"
        );
    }

    /// The history snapshot for a `SessionInput`, as plain strings.
    fn history_entries(input: &SessionInput) -> Vec<String> {
        input
            .history_snapshot()
            .iter()
            .map(|entry| entry.as_str().to_string())
            .collect()
    }

    #[test]
    fn history_snapshot_is_newest_first_and_revision_tracks_changes() {
        let mut input = SessionInput::new();
        assert_eq!(input.history_revision(), 0);
        assert!(input.history_snapshot().is_empty());

        submit_unmasked(&mut input, "first");
        submit_unmasked(&mut input, "second");
        assert_eq!(
            history_entries(&input),
            vec!["second", "first"],
            "the snapshot lists entries newest first"
        );

        // Re-submitting the newest entry changes nothing: no revision bump,
        // so no mirror message would go out.
        let rev = input.history_revision();
        submit_unmasked(&mut input, "second");
        assert_eq!(
            input.history_revision(),
            rev,
            "re-submitting the front entry is not a history change"
        );

        // Re-submitting an older entry moves it to the front (dedup) and is a
        // real change.
        submit_unmasked(&mut input, "first");
        assert!(input.history_revision() > rev);
        assert_eq!(history_entries(&input), vec!["first", "second"]);
    }

    /// A scripted `history.push()` and a typed submission share
    /// `add_to_history`, so dedup, ordering, the whitespace skip, and the cap
    /// behave identically — and the pushed entry is recallable with Up.
    #[test]
    fn scripted_history_push_matches_typed_submission_semantics() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "kill rat");
        let value_before = input.value.clone();

        let _ = input.apply_script_op(&InputOp::HistoryPush(Arc::new("drink potion".to_string())));
        assert_eq!(
            history_entries(&input),
            vec!["drink potion", "kill rat"],
            "a pushed entry becomes the newest"
        );
        assert_eq!(
            input.value, value_before,
            "push touches history only, never the buffer"
        );

        // Dedup parity: pushing an existing entry moves it, no duplicate.
        let _ = input.apply_script_op(&InputOp::HistoryPush(Arc::new("kill rat".to_string())));
        assert_eq!(history_entries(&input), vec!["kill rat", "drink potion"]);

        // Whitespace-only parity: dropped silently, exactly like a typed
        // whitespace submission (the op layer already rejects empty strings).
        let rev = input.history_revision();
        let _ = input.apply_script_op(&InputOp::HistoryPush(Arc::new("   ".to_string())));
        assert_eq!(input.history_revision(), rev);
        assert_eq!(history_entries(&input), vec!["kill rat", "drink potion"]);

        // Cap parity: history holds at most 100 entries, oldest falling off.
        for i in 0..150 {
            let _ = input.apply_script_op(&InputOp::HistoryPush(Arc::new(format!("cmd{i}"))));
        }
        let entries = history_entries(&input);
        assert_eq!(entries.len(), 100, "the cap applies to pushed entries too");
        assert_eq!(entries[0], "cmd149", "newest first after the burst");
        assert!(
            !entries.iter().any(|e| e == "kill rat"),
            "the oldest entries fell off the back"
        );

        // A pushed entry is recallable exactly like a typed one.
        let _ = input.update(Message::InputChanged(String::new()));
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "cmd149", "Up recalls the pushed entry");
    }

    #[test]
    fn masked_submissions_never_reach_the_history_snapshot() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "look");
        let rev = input.history_revision();

        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.update(Message::InputChanged("hunter2".to_string()));
        let _ = input.update(Message::Submit);

        assert_eq!(
            input.history_revision(),
            rev,
            "a masked submission is not a history change, so nothing would sync"
        );
        assert_eq!(
            history_entries(&input),
            vec!["look"],
            "the snapshot reflects the masked exclusion naturally"
        );
    }

    #[test]
    fn scripted_history_clear_empties_and_disarms_recall() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "look");
        submit_unmasked(&mut input, "north");
        let rev = input.history_revision();

        let _ = input.apply_script_op(&InputOp::HistoryClear);
        assert!(input.history_snapshot().is_empty());
        assert!(input.history_revision() > rev, "a real clear is a change");

        // Clearing an empty history is a no-op: no revision bump, no sync.
        let rev = input.history_revision();
        let _ = input.apply_script_op(&InputOp::HistoryClear);
        assert_eq!(input.history_revision(), rev);

        // Nothing left to recall.
        let _ = input.update(Message::InputChanged(String::new()));
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "", "Up finds nothing after a clear");
    }

    #[test]
    fn tab_completion_is_disabled_while_masked() {
        let buffer = Rc::new(RefCell::new(TerminalBuffer::new_with_max_lines(
            std::num::NonZeroUsize::new(100).unwrap(),
        )));
        {
            let mut buffer = buffer.borrow_mut();
            buffer.extend_line(Arc::new(StyledLine::from_echo_str("hunterodon appears")));
            buffer.commit_current_line();
        }
        let mut input = SessionInput::new().with_terminal_buffer(buffer);

        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.update(Message::InputChanged("hunt".to_string()));
        let _ = input.update(Message::HandleTabCompletion);
        assert_eq!(
            input.value, "hunt",
            "masked input must not complete against the scrollback"
        );
        assert!(input.completion_state.is_none());

        // The same buffer completes once unmasked (the box was cleared by the
        // unmask, so type the prefix again).
        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        let _ = input.update(Message::InputChanged("hunt".to_string()));
        let _ = input.update(Message::HandleTabCompletion);
        assert_eq!(
            input.value, "hunterodon",
            "the completion mechanism itself works when not masked"
        );
    }

    /// A terminal buffer holding the given committed lines.
    fn buffer_with_lines(lines: &[&str]) -> Rc<RefCell<TerminalBuffer>> {
        let buffer = Rc::new(RefCell::new(TerminalBuffer::new_with_max_lines(
            std::num::NonZeroUsize::new(100).unwrap(),
        )));
        {
            let mut buffer = buffer.borrow_mut();
            for line in lines {
                buffer.extend_line(Arc::new(StyledLine::from_echo_str(line)));
                buffer.commit_current_line();
            }
        }
        buffer
    }

    fn set_words(input: &mut SessionInput, suggestions: &[&str], blacklist: &[&str]) {
        input.set_word_sets(
            Arc::new(
                suggestions
                    .iter()
                    .map(|w| Arc::new((*w).to_string()))
                    .collect(),
            ),
            Arc::new(blacklist.iter().map(|w| w.to_lowercase()).collect()),
        );
    }

    /// One Tab press; returns the input's value afterwards.
    fn press_tab(input: &mut SessionInput) -> String {
        let _ = input.update(Message::HandleTabCompletion);
        input.value.clone()
    }

    #[test]
    fn tab_offers_registered_suggestions_before_scrollback_words() {
        let buffer = buffer_with_lines(&["a nostrum sits here"]);
        let mut input = SessionInput::new().with_terminal_buffer(buffer);
        set_words(&mut input, &["north", "note"], &[]);

        let _ = input.update(Message::InputChanged("no".to_string()));
        // Registered words cycle first, in merge order; the scrollback word
        // follows once the suggestions are exhausted.
        assert_eq!(press_tab(&mut input), "north");
        assert_eq!(press_tab(&mut input), "note");
        assert_eq!(press_tab(&mut input), "nostrum");
        // Nothing left: the value stays.
        assert_eq!(press_tab(&mut input), "nostrum");
    }

    #[test]
    fn blacklist_filters_both_completion_sources() {
        let buffer = buffer_with_lines(&["the Hunterodon appears"]);
        let mut input = SessionInput::new().with_terminal_buffer(buffer);
        // Case-insensitive on both sides: a lowercase blacklist entry hides a
        // capitalized scrollback word and a capitalized registered word.
        set_words(&mut input, &["Hunter", "hush"], &["hunterodon", "hunter"]);

        let _ = input.update(Message::InputChanged("hu".to_string()));
        assert_eq!(
            press_tab(&mut input),
            "hush",
            "blacklisted suggestion and scrollback word are both skipped"
        );
        assert_eq!(press_tab(&mut input), "hush", "no further candidates");
    }

    #[test]
    fn suggestion_inserts_its_registered_casing() {
        let buffer = buffer_with_lines(&[]);
        let mut input = SessionInput::new().with_terminal_buffer(buffer);
        set_words(&mut input, &["Fjord"], &[]);

        let _ = input.update(Message::InputChanged("fj".to_string()));
        assert_eq!(
            press_tab(&mut input),
            "Fjord",
            "completion inserts the registered casing, matching case-insensitively"
        );
    }

    #[test]
    fn suggestion_offered_once_is_not_reoffered_from_scrollback() {
        // The same word (differently cased) exists in scrollback; once the
        // registered form is offered, cycling moves past it instead of
        // re-offering the scrollback casing.
        let buffer = buffer_with_lines(&["a fjord and a fjar"]);
        let mut input = SessionInput::new().with_terminal_buffer(buffer);
        set_words(&mut input, &["Fjord"], &[]);

        let _ = input.update(Message::InputChanged("fj".to_string()));
        assert_eq!(press_tab(&mut input), "Fjord");
        assert_eq!(
            press_tab(&mut input),
            "fjar",
            "the scrollback copy of an offered suggestion is skipped case-insensitively"
        );
    }

    #[test]
    fn scrollback_casing_pairs_still_cycle_with_empty_word_sets() {
        // With no registered words, cycling is untouched by the word-set
        // machinery: a casing pair in scrollback offers BOTH casings (the
        // scrollback skip is exact-match).
        let buffer = buffer_with_lines(&["zurek parries", "Zurek attacks"]);
        let mut input = SessionInput::new().with_terminal_buffer(buffer);

        let _ = input.update(Message::InputChanged("zu".to_string()));
        assert_eq!(
            press_tab(&mut input),
            "Zurek",
            "most recent line scans first"
        );
        assert_eq!(
            press_tab(&mut input),
            "zurek",
            "the other casing cycles next"
        );
        assert_eq!(press_tab(&mut input), "zurek", "exhausted: the value stays");
    }

    #[test]
    fn registered_offer_folds_scrollback_but_scrollback_offers_do_not() {
        // A REGISTERED word, once offered, never returns as a differently-
        // cased scrollback word — while scrollback-sourced offers keep the
        // exact-match skip, so an unrelated scrollback casing pair cycles.
        let buffer = buffer_with_lines(&["zurek and Ogre and ogre wait"]);
        let mut input = SessionInput::new().with_terminal_buffer(buffer);
        set_words(&mut input, &["Zurek"], &[]);

        let _ = input.update(Message::InputChanged("zu".to_string()));
        assert_eq!(
            press_tab(&mut input),
            "Zurek",
            "the registered word offers first"
        );
        assert_eq!(
            press_tab(&mut input),
            "Zurek",
            "scrollback's 'zurek' is folded away: the registered offer covers it"
        );

        let _ = input.update(Message::InputChanged("og".to_string()));
        assert_eq!(
            press_tab(&mut input),
            "Ogre",
            "first scrollback match offers"
        );
        assert_eq!(
            press_tab(&mut input),
            "ogre",
            "the scrollback pair still cycles both casings"
        );
    }

    #[test]
    fn mirror_snapshot_carries_no_content_while_masked() {
        let mut input = SessionInput::new();
        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.update(Message::InputChanged("hunter2".to_string()));
        let _ = input.update(Message::CaretChanged(CaretState {
            focused: true,
            ..CaretState::default()
        }));

        let snapshot = input.mirror_snapshot();
        assert_eq!(snapshot.value.as_str(), "");
        assert_eq!(snapshot.cursor, 0);
        assert_eq!(snapshot.selection, None);
        assert!(snapshot.focused, "focus is not content; it still mirrors");
        assert!(snapshot.masked);

        // The eye reveal is rendering-only: the snapshot stays suppressed.
        let _ = input.update(Message::ToggleMaskedReveal);
        assert!(input.masked_reveal);
        let snapshot = input.mirror_snapshot();
        assert_eq!(snapshot.value.as_str(), "");
        assert!(snapshot.masked);
    }

    #[test]
    fn mirror_snapshot_reports_utf16_positions() {
        // The raw caret from the widget parks "cursor at end" at usize::MAX
        // (grapheme units); the snapshot must clamp against the current value
        // and convert to UTF-16 code units.
        let mut input = SessionInput::new();
        let _ = input.update(Message::InputChanged("a\u{1F44D}".to_string()));
        let _ = input.update(Message::CaretChanged(CaretState {
            focused: true,
            ..CaretState::default()
        }));

        let snapshot = input.mirror_snapshot();
        assert_eq!(snapshot.value.as_str(), "a\u{1F44D}");
        // The default raw cursor sits at index 0 — clamped, converted: 0.
        assert_eq!(snapshot.cursor, 0);
        assert_eq!(snapshot.selection, None);
        assert!(snapshot.focused);
    }

    #[test]
    fn post_submit_leftover_is_stashed_and_restored_selected() {
        let mut input = SessionInput::new();
        // The post-submit select-all leftover state (the default behavior
        // leaves the sent text in the box, flagged — no caret involved).
        submit_unmasked(&mut input, "kill rat");
        assert_eq!(input.value, "kill rat");
        assert!(input.post_submit_selected);

        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        assert_eq!(
            input.value, "",
            "the leftover is stashed out of the masked box"
        );

        // The secret typed while masked must not survive the unmask.
        let _ = input.update(Message::InputChanged("s3cret".to_string()));
        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        assert_eq!(input.value, "kill rat", "the stash restores on unmask");
        assert!(
            input.post_submit_selected,
            "the restore re-enters the fully-selected state (select-all rides the task)"
        );
        assert!(!input.masked);
    }

    #[test]
    fn history_matching_leftover_is_stashed() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "kill rat");
        // Re-typed text equal to a history entry: still a leftover command,
        // even though the post-submit flag was cleared by the edit.
        let _ = input.update(Message::InputChanged("kill rat".to_string()));
        assert!(!input.post_submit_selected);

        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        assert_eq!(input.value, "");
        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        assert_eq!(input.value, "kill rat");
    }

    #[test]
    fn early_typed_secret_prefix_stays_masked_and_never_restores() {
        let mut input = SessionInput::new();
        // Half a password typed before the mask engaged: not post-submit
        // state, not a history entry.
        let _ = input.update(Message::InputChanged("hun".to_string()));

        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        assert_eq!(
            input.value, "hun",
            "an early-typed secret prefix stays in the masked box"
        );
        assert!(input.stash.is_none(), "the prefix is never stashed");

        let _ = input.update(Message::InputChanged("hunter2".to_string()));
        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        assert_eq!(
            input.value, "",
            "unmasking clears masked content instead of revealing it"
        );
    }

    #[test]
    fn masked_submission_consumes_buffer_and_stash_still_restores() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "kill rat");
        let _ = input.apply_script_op(&InputOp::SetMasked(true));

        let _ = input.update(Message::InputChanged("hunter2".to_string()));
        let update = input.update(Message::Submit);
        match update.event {
            Some(Event::Submit { text, masked }) => {
                assert_eq!(text.as_str(), "hunter2");
                assert!(masked);
            }
            other => panic!("expected a Submit event, got {other:?}"),
        }

        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        assert_eq!(
            input.value, "kill rat",
            "the pre-mask stash restores even after a masked submission"
        );
    }

    /// The mask-cause compose rule (`docs/input.md` §3.10): the
    /// input is masked while EITHER the script or the telnet cause is active,
    /// so releasing one cause while the other holds changes nothing.
    #[test]
    fn mask_causes_compose_and_release_independently() {
        // Telnet unmask must not release a script-set mask.
        let mut input = SessionInput::new();
        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.set_telnet_mask(false);
        assert!(input.masked, "WONT ECHO must not unmask a script-set mask");

        // Script unmask must not release a telnet-held mask.
        let mut input = SessionInput::new();
        let _ = input.set_telnet_mask(true);
        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        assert!(input.masked, "a script must not unmask a telnet-held mask");

        // Both causes held: releasing one keeps the mask, releasing the
        // second finally unmasks.
        let mut input = SessionInput::new();
        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.set_telnet_mask(true);
        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        assert!(input.masked, "the telnet cause still holds");
        let _ = input.set_telnet_mask(false);
        assert!(!input.masked, "both causes released: the mask lifts");
    }

    /// A telnet-engaged mask carries the full Phase 1 semantics: the stash
    /// captures a leftover command at engage (once, on the effective edge —
    /// a script cause joining later must not re-triage the masked buffer),
    /// the secret typed under it never survives the release, and the stash
    /// restores when the LAST cause releases.
    #[test]
    fn telnet_mask_inherits_stash_restore_across_cause_changes() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "kill rat");

        let _ = input.set_telnet_mask(true);
        assert_eq!(input.value, "", "the leftover is stashed at telnet engage");
        let _ = input.update(Message::InputChanged("hunter2".to_string()));

        // A script cause joining mid-mask is not a fresh engage: nothing is
        // re-stashed, the secret stays in the masked box.
        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        assert_eq!(input.value, "hunter2");

        // The telnet release keeps the mask (script still holds); the script
        // release lifts it, clearing the secret and restoring the stash.
        let _ = input.set_telnet_mask(false);
        assert!(input.masked);
        let _ = input.apply_script_op(&InputOp::SetMasked(false));
        assert!(!input.masked);
        assert_eq!(
            input.value, "kill rat",
            "the stash restores when the last cause releases; the secret is gone"
        );
    }

    /// A submission while telnet-masked reports itself masked, so the parent
    /// routes it down the redaction path exactly like a script-masked one.
    #[test]
    fn telnet_masked_submission_reports_masked() {
        let mut input = SessionInput::new();
        let _ = input.set_telnet_mask(true);
        let _ = input.update(Message::InputChanged("hunter2".to_string()));
        let update = input.update(Message::Submit);
        match update.event {
            Some(Event::Submit { text, masked }) => {
                assert_eq!(text.as_str(), "hunter2");
                assert!(
                    masked,
                    "a telnet-masked submission must ride the redaction path"
                );
            }
            other => panic!("expected a Submit event, got {other:?}"),
        }
        assert!(
            !input
                .history
                .iter()
                .any(|entry| entry.as_str() == "hunter2"),
            "a telnet-masked submission must never enter history"
        );
    }

    #[test]
    fn focus_lost_never_clears_a_masked_input() {
        let mut input = SessionInput::new();
        // Unmasked, the default behavior clears on blur.
        let _ = input.update(Message::InputChanged("half a command".to_string()));
        let _ = input.update(Message::FocusLost);
        assert_eq!(input.value, "", "the unmasked default clears on blur");

        // Masked, the same blur (e.g. clicking the reveal eye) keeps the
        // secret in progress.
        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.update(Message::InputChanged("hunter2".to_string()));
        let _ = input.update(Message::FocusLost);
        assert_eq!(
            input.value, "hunter2",
            "a masked input survives losing focus"
        );
    }

    #[test]
    fn history_navigation_is_disabled_while_masked() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "look");
        let _ = input.update(Message::InputChanged(String::new()));

        let _ = input.apply_script_op(&InputOp::SetMasked(true));
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "",
            "history recall must not paste old commands into a masked box"
        );
    }

    #[test]
    fn script_ops_edit_value() {
        let mut input = SessionInput::new();
        let _ = input.apply_script_op(&InputOp::Replace(Arc::new("north".to_string())));
        assert_eq!(input.value, "north");
        assert_eq!(input.last_change_source(), InputSource::Script);

        let _ = input.apply_script_op(&InputOp::Append(Arc::new(";look".to_string())));
        assert_eq!(input.value, "north;look");

        let _ = input.apply_script_op(&InputOp::Propose(Arc::new("say hi".to_string())));
        assert_eq!(input.value, "say hi");

        let _ = input.apply_script_op(&InputOp::Clear);
        assert_eq!(input.value, "");

        // A script submit takes the full submit path, history included.
        let _ = input.apply_script_op(&InputOp::Replace(Arc::new("look".to_string())));
        let update = input.apply_script_op(&InputOp::Submit);
        match update.event {
            Some(Event::Submit { text, masked }) => {
                assert_eq!(text.as_str(), "look");
                assert!(!masked);
            }
            other => panic!("expected a Submit event, got {other:?}"),
        }
        assert!(input.history.iter().any(|entry| entry.as_str() == "look"));
    }

    /// Escape reports the focus-main request only on inputs that opted in
    /// (pane inputs); the main input never sets the flag, so its view never
    /// binds the key.
    #[test]
    fn escape_requests_main_focus_on_pane_inputs() {
        let mut pane_input = SessionInput::new().with_escape_to_main();
        assert!(pane_input.escape_to_main);
        let update = pane_input.update(Message::EscapePressed);
        assert!(matches!(update.event, Some(Event::FocusMain)));

        let main_input = SessionInput::new();
        assert!(!main_input.escape_to_main, "the main input never opts in");
    }

    #[test]
    fn placeholder_is_stored_for_the_view() {
        let input = SessionInput::new().with_placeholder("group tell...");
        assert_eq!(input.placeholder, "group tell...");
        assert_eq!(SessionInput::new().placeholder, "");
    }

    /// Pane inputs share the session's hotkeys: a copy seeded from the main
    /// input carries the registered tables, and per-instance state stays
    /// isolated otherwise.
    #[test]
    fn copy_hotkeys_from_seeds_the_session_tables() {
        // `HotkeyId`s are host-minted (no public constructor); Default gives
        // the same id the runtime's counter starts from.
        let id = HotkeyId::default();
        let mut main_input = SessionInput::new();
        main_input.register_hotkey(
            id,
            HotkeyDefinition {
                key: "F1".to_string(),
                modifiers: vec![],
                script: None,
                package: None,
                language: smudgy_core::models::ScriptLang::Plaintext,
                enabled: true,
            },
        );

        let mut pane_input = SessionInput::new();
        pane_input.copy_hotkeys_from(&main_input);
        assert!(pane_input.hotkeys.contains_key(&id));
        assert_eq!(pane_input.hotkey_lookup.len(), 1);

        // A later unregister fans out separately; the copy is independent.
        assert!(pane_input.unregister_hotkey(&id));
        assert!(main_input.hotkeys.contains_key(&id));
    }

    /// An input with no terminal buffer (a widgets-only pane's) still
    /// completes from the registered suggestion sets.
    #[test]
    fn suggestions_complete_without_a_terminal_buffer() {
        let mut input = SessionInput::new();
        set_words(&mut input, &["north", "note"], &[]);

        let _ = input.update(Message::InputChanged("no".to_string()));
        assert_eq!(press_tab(&mut input), "north");
        assert_eq!(press_tab(&mut input), "note");
        assert_eq!(
            press_tab(&mut input),
            "note",
            "no scrollback source to fall back to"
        );
    }

    /// The source attribution the mirror update carries: typing is `User`,
    /// script ops are `Script`, the caret echo that follows a script op is
    /// `Script` too (not the user), and an unheralded caret move is `User`.
    #[test]
    fn change_sources_are_attributed_by_mutation_site() {
        let mut input = SessionInput::new();

        let _ = input.update(Message::InputChanged("north".to_string()));
        assert_eq!(input.last_change_source(), InputSource::User);

        let _ = input.apply_script_op(&InputOp::Propose(Arc::new("say hi".to_string())));
        assert_eq!(input.last_change_source(), InputSource::Script);

        // The caret echo the propose's select-all triggers reports Script...
        let _ = input.update(Message::CaretChanged(CaretState::default()));
        assert_eq!(input.last_change_source(), InputSource::Script);

        // ...and a later, unheralded caret move is the user's.
        let _ = input.update(Message::CaretChanged(CaretState {
            focused: true,
            ..CaretState::default()
        }));
        assert_eq!(input.last_change_source(), InputSource::User);

        // Post-submit behavior is Other, whoever submitted.
        let _ = input.update(Message::InputChanged("look".to_string()));
        let _ = input.update(Message::Submit);
        assert_eq!(input.last_change_source(), InputSource::Other);
    }
}
