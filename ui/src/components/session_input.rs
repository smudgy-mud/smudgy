use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::keymap::{HotkeyKeys, MaybePhysicalKey};
use crate::terminal_buffer::selection::{BufferPosition, Selection};
use crate::terminal_buffer::{TerminalBuffer, TerminalTextMatch};
use crate::theme::{Element, builtins};
use crate::update::Update;
use crate::widgets::hotkey_matching_input::{CaretState, HotkeyMatchingInput};
use crate::widgets::split_terminal_pane::{ScrollRequest, TerminalViewHandle};
use iced::advanced::widget::operation::{Focusable, Operation, Outcome};
use iced::widget::{Id, Space, button, column, operation, row, text, text_input};
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

/// The byte index (for slicing `value` itself, a plain `String`) of the
/// boundary before grapheme `index`. An `index` past the last grapheme
/// lands on the end of the string.
fn grapheme_to_byte(value: &str, index: usize) -> usize {
    value.graphemes(true).take(index).map(str::len).sum()
}

/// The *unselected* prefix of `value`, given its current selection (a
/// grapheme-index `(start, end)` pair, `start <= end`) if any: the text
/// before the selection's start, or the whole value when there's no
/// selection at all. A selection starting at 0 (select-all, in particular)
/// yields an empty prefix.
fn prefix_from_selection(value: &str, selection: Option<(usize, usize)>) -> String {
    match selection {
        Some((start, _end)) => value[..grapheme_to_byte(value, start)].to_string(),
        None => value.to_string(),
    }
}

/// How many graphemes at the start of `entry` are covered by `prefix`, or
/// `None` when `entry` does not start with it. An empty `prefix` matches every
/// entry, covering nothing.
///
/// The count is what anchors the selection left behind on a recalled entry, so
/// it is measured by consuming both strings a grapheme at a time rather than
/// by taking the prefix's own length: under a case-insensitive match the two
/// are not the same text, case mapping is not always length-preserving, and a
/// byte length is not a grapheme index at all. Walking in lockstep also keeps
/// the common path allocation-free — equal graphemes settle by comparison, and
/// only a differing pair pays for case folding — so a match costs no more than
/// the prefix is long however long the entry is.
fn prefix_match_len(entry: &str, prefix: &str, case_sensitive: bool) -> Option<usize> {
    let mut entry_graphemes = entry.graphemes(true);
    let mut covered = 0;
    for wanted in prefix.graphemes(true) {
        let found = entry_graphemes.next()?;
        if found != wanted && (case_sensitive || found.to_lowercase() != wanted.to_lowercase()) {
            return None;
        }
        covered += 1;
    }
    Some(covered)
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
    found: bool,
}

impl Operation<bool> for UnfocusTarget {
    fn focusable(&mut self, id: Option<&Id>, _bounds: iced::Rectangle, state: &mut dyn Focusable) {
        if id == Some(&self.target) {
            self.found = true;
            state.unfocus();
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<bool>)) {
        operate(self);
    }

    fn finish(&self) -> Outcome<bool> {
        Outcome::Some(self.found)
    }
}

/// Run [`UnfocusTarget`] against `target`, reporting whether the widget was
/// actually found. This lets the model publish focus loss after the operation
/// instead of waiting for another event that an obscured tab may never see.
pub fn unfocus_target(target: Id) -> Task<bool> {
    iced::advanced::widget::operate(UnfocusTarget {
        target,
        found: false,
    })
}

/// An [`Operation`] that gives focus to exactly the widget carrying
/// `target`'s id, releasing every other focusable it visits (keyboard focus
/// is exclusive). Unlike the stock `focusable::focus`, a target that already
/// holds focus is left untouched: a text input's `focus()` moves its caret
/// to the end, so re-focusing the focus holder would cost the user their
/// caret position and selection for no state change.
struct FocusTarget {
    target: Id,
    found: bool,
}

impl Operation<bool> for FocusTarget {
    fn focusable(&mut self, id: Option<&Id>, _bounds: iced::Rectangle, state: &mut dyn Focusable) {
        if id == Some(&self.target) {
            self.found = true;
            if !state.is_focused() {
                state.focus();
            }
        } else {
            state.unfocus();
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<bool>)) {
        operate(self);
    }

    fn finish(&self) -> Outcome<bool> {
        Outcome::Some(self.found)
    }
}

/// A [`Task`] running [`FocusTarget`] against `target` — the caret-friendly
/// focus transfer for chrome-driven moves (tab selection, activation).
pub fn focus_target(target: Id) -> Task<bool> {
    iced::advanced::widget::operate(FocusTarget {
        target,
        found: false,
    })
}

/// Schedule a follow-up after the widget tree has reflected a model change
/// that mounts or unmounts a focus target.
fn after_widget_tree_update(message: Message) -> Task<Message> {
    Task::perform(
        async { tokio::time::sleep(Duration::from_millis(1)).await },
        move |()| message.clone(),
    )
}

fn effective_font_size(pane_font_size: Option<f32>, global_font_size: f32) -> f32 {
    pane_font_size.unwrap_or(global_font_size)
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
    /// The prefix used to reach the current `history_index`. Stored so that
    /// the user can quickly go through history entries regardless of whether
    /// a previous Up/Down press was properly applied to/shown in the input
    /// field before a next Up/Down press. Only `Some` while `history_index`
    /// is `Some`; see `history_search_prefix`.
    history_prefix: Option<String>,
    /// Bumped on every actual history change (a submission or scripted push
    /// entering it, a scripted clear emptying it), so the parent can feed the
    /// session-thread history mirror exactly when there is something new —
    /// never per keystroke (`docs/input.md` §3.9).
    history_revision: u64,
    /// Current partial completion state
    completion_state: Option<CompletionState>,
    /// Reference to terminal buffer for tab completion
    terminal_buffer: Option<Rc<RefCell<TerminalBuffer>>>,
    /// The terminal selection and viewport controllers paired with this input.
    /// A widgets-only pane has no target and therefore no search/scroll mode.
    terminal_view: Option<TerminalViewHandle>,
    /// Active terminal-search state shown in its own row above the command.
    /// The game editor remains mounted and independently editable.
    search: Option<SearchState>,
    /// Active hotkey definitions (pre-processed for efficiency)
    hotkeys: HashMap<HotkeyId, HotkeyKeys>,
    /// Fast lookup table: key -> vec of (modifiers, hotkey_id) pairs
    hotkey_lookup: HashMap<MaybePhysicalKey, Vec<(keyboard::Modifiers, HotkeyId)>>,
    /// Unique ID for the input component
    input_id: Id,
    /// Independent widget ID for the temporary terminal-search editor.
    search_input_id: Id,
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
    /// The names of every enabled Command-kind alias, host-derived and
    /// replaced wholesale by `SessionEvent::CommandNames`. Offered ahead of
    /// the registered suggestions, but only while the word under the caret is
    /// in command position (start of input, or right after the command
    /// separator). Main input only; the blacklist filters these too.
    command_names: Arc<Vec<Arc<String>>>,
    /// The session's command separator, for the command-position test. May be
    /// multi-character; empty means only the start of input qualifies.
    command_separator: String,
    /// Hint text shown while the input is empty. Empty for the main input;
    /// pane inputs carry their spec's `placeholder`.
    placeholder: String,
    /// Whether Escape moves focus to the session's main input (the pane-input
    /// convention). The component only reports the request
    /// ([`Event::FocusMain`]); the parent owns the main input's id.
    escape_to_main: bool,
    /// One-shot: the next [`Message::FocusLost`] is a tab switch obscuring
    /// this input (its subtree stays mounted behind another tab), not the
    /// user abandoning the line, so the clear-on-blur behavior must leave
    /// the in-progress text alone. Armed by [`Self::note_obscured`];
    /// consumed by the next `FocusLost` — which the obscured widget defers
    /// to the moment its tab is re-selected, since only the rendered
    /// subtree receives events, so one-shot consumption lands on exactly
    /// the blur the switch inflicted.
    obscured_blur_pending: bool,
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
struct SearchState {
    query: String,
    matches: Vec<TerminalTextMatch>,
    current: Option<usize>,
    previous_selection: Selection,
}

#[derive(Debug, Clone, Copy)]
enum SearchDirection {
    /// Move toward older terminal output.
    Previous,
    /// Move back toward newer terminal output.
    Next,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Input value changed
    InputChanged(String),
    /// Terminal-search query changed.
    SearchChanged(String),
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
    /// Enter terminal search without disturbing the pending command.
    EnterSearch,
    /// Focus the search editor after its row has been mounted.
    FocusSearch,
    /// Leave terminal search and clear its terminal decorations.
    ExitSearch,
    /// Focus the game editor after the search row has been dismissed.
    FocusGameInput,
    /// Move to the next older terminal match.
    SearchPrevious,
    /// Move back to the next newer terminal match.
    SearchNext,
    /// Completion of a chrome-driven focus return to the search editor.
    SearchFocusSettled(bool),
    /// Scroll the associated terminal by one viewport.
    ScrollPageUp,
    ScrollPageDown,
    /// Scroll the associated terminal to its oldest/newest page.
    ScrollHome,
    ScrollEnd,
    /// The input lost focus (used by the clear-on-blur behavior).
    FocusLost,
    /// The input gained focus; the parent clears stale terminal link focus.
    FocusGained,
    /// A targeted widget operation completed. `found` distinguishes a real
    /// focus mutation (or an already-satisfied target) from an unlaid/stale
    /// target that the operation could not reach.
    FocusSettled {
        focused: bool,
        found: bool,
    },
    /// Escape pressed in a pane input: hand focus back to the main input.
    EscapePressed,
    /// The widget's caret (focus/cursor) changed.
    CaretChanged(CaretState),
    /// The masked eye affordance: toggle the on-screen reveal.
    ToggleMaskedReveal,
    /// Reserved keyboard entry into the application audio panel.
    #[cfg(feature = "web-audio-cpal")]
    OpenAudioPanel,
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
    /// The command editor received keyboard focus.
    FocusGained,
    /// Open the audio panel in this input's hosting main window.
    #[cfg(feature = "web-audio-cpal")]
    OpenAudioPanel,
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
            history_prefix: None,
            history_revision: 0,
            completion_state: None,
            terminal_buffer: None,
            terminal_view: None,
            search: None,
            hotkeys: HashMap::new(),
            hotkey_lookup: HashMap::new(),
            input_id: Id::unique(),
            search_input_id: Id::unique(),
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
            command_names: Arc::new(Vec::new()),
            command_separator: String::new(),
            placeholder: String::new(),
            escape_to_main: false,
            obscured_blur_pending: false,
        }
    }

    /// Set the terminal buffer for tab completion
    pub fn with_terminal_buffer(mut self, buffer: Rc<RefCell<TerminalBuffer>>) -> Self {
        self.terminal_buffer = Some(buffer);
        self
    }

    /// Associate the terminal's rendered selection and viewport controllers.
    /// The buffer itself is supplied separately by [`Self::with_terminal_buffer`]
    /// because completion-only tests and callers do not mount a terminal.
    pub fn with_terminal_view(mut self, view: TerminalViewHandle) -> Self {
        self.terminal_view = Some(view);
        self
    }

    /// Seeds recent history loaded for this session, newest first.
    ///
    /// Loading is deliberately quieter than [`Self::add_to_history`]: it is
    /// initial state, not a new history mutation, so it does not bump the
    /// revision. The runtime-ready resync sends the complete initial snapshot
    /// unconditionally.
    pub fn with_history(mut self, entries: Vec<String>) -> Self {
        let max_history = Self::max_history();
        for entry in entries {
            if max_history.is_some_and(|max| self.history.len() >= max) {
                break;
            }
            if entry.trim().is_empty() {
                continue;
            }
            let entry = Arc::new(entry);
            if !self.history.contains(&entry) {
                self.history.push_back(entry);
            }
        }
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

    /// Replace the Command-alias name list (`SessionEvent::CommandNames`).
    pub fn set_command_names(&mut self, names: Arc<Vec<Arc<String>>>) {
        self.command_names = names;
    }

    /// The session's command separator, for the command-position test.
    pub fn set_command_separator(&mut self, separator: String) {
        self.command_separator = separator;
    }

    /// Whether the word starting at byte `word_start` is in command position:
    /// everything between the nearest preceding boundary — the start of the
    /// input, or an occurrence of the command separator — and the word start
    /// is whitespace.
    fn in_command_position(&self, word_start: usize) -> bool {
        let before = &self.value[..word_start];
        let boundary_end = if self.command_separator.is_empty() {
            0
        } else {
            before
                .rfind(&self.command_separator)
                .map_or(0, |i| i + self.command_separator.len())
        };
        before[boundary_end..].chars().all(char::is_whitespace)
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
        self.history_prefix = None;
        self.post_submit_selected = false;
    }

    /// Clear the input value (and reset completion / history navigation).
    pub fn clear(&mut self) {
        self.value.clear();
        self.note_value_edited();
    }

    /// Mark this input as being obscured by a tab switch: its subtree stays
    /// mounted behind another tab, and the blur the switch inflicts is an
    /// obscure, not the user abandoning the line. The next
    /// [`Message::FocusLost`] consumes the mark and skips the clear-on-blur
    /// behavior, so the in-progress text is waiting when the tab is
    /// re-selected. The inactive subtree cannot deliver that widget event
    /// promptly, so update the component-side focus mirror here as well: a
    /// script must never continue seeing an obscured input as focused.
    /// Idempotent across repeated switches while no blur has landed.
    pub fn note_obscured(&mut self) {
        self.obscured_blur_pending = true;
        self.note_focus_state(false);
    }

    /// Reconcile the component-side focus mirror after an explicit widget
    /// operation. A successful focus gain also settles any deferred obscure:
    /// the input returned before its hidden subtree could publish the blur,
    /// so that stale marker must not excuse a later genuine focus loss.
    pub fn note_focus_state(&mut self, focused: bool) {
        self.caret.focused = focused;
        self.last_source = InputSource::Other;
        if focused {
            self.obscured_blur_pending = false;
        }
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
        if let Some(max_history) = Self::max_history() {
            while self.history.len() > max_history {
                self.history.pop_back();
            }
        }

        self.history_revision += 1;
    }

    /// The configured history cap, read live from prefs (not cached at
    /// construction, so a settings change takes effect immediately) --
    /// `None` means unlimited, the `0` sentinel in `TerminalPrefs::max_history`.
    fn max_history() -> Option<usize> {
        match crate::prefs::current().max_history {
            0 => None,
            n => Some(n),
        }
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

    /// The *prefix* the next Up/Down history search matches against: whatever
    /// part of the current value is not selected, read as a prefix (the text
    /// before the selection's start). No selection at all means the whole
    /// value is unselected — typing "gt " with a bare caret searches for
    /// "gt " — while a selection starting at 0 (a select-all, or the span a
    /// previous match left behind) yields an empty prefix, which matches every
    /// entry: an empty box and a fully selected box both browse all of history.
    ///
    /// `self.caret` arrives only as the widget's `CaretChanged` echo, which is
    /// published while handling a widget *event* — never from the `operate`
    /// pass that applies a `select_range`. Between issuing a caret operation
    /// and the next event, `self.caret` therefore still describes the caret
    /// from *before* that operation, reinterpreted against a value that has
    /// already moved on. Reading a selection out of it in that window searches
    /// for the wrong text: repeated Up presses stop advancing, and an Up
    /// straight after a submission filters by the whole line just sent instead
    /// of browsing everything.
    ///
    /// `pending_caret_echo` is exactly that in-flight bit — armed when an
    /// operation is issued, disarmed only by the echo — so the live selection
    /// is read only while no echo is outstanding. Otherwise the prefix comes
    /// from state this component sets synchronously, which cannot race:
    /// `history_prefix` while a search is running, and before one starts,
    /// either the empty prefix a post-submit select-all is about to produce or
    /// the whole value, nothing being selected.
    fn history_search_prefix(&self) -> String {
        let value = text_input::Value::new(&self.value);
        let selection = self.caret.cursor.selection(&value);

        // A fresh echo answers the question at the start of a search, and
        // mid-search whenever it carries a real selection — our own
        // `select_range` echo, or a span the user picked to redirect the
        // search. Mid-search with no selection (the user collapsed it) keeps
        // the search that is already running rather than reading the whole
        // recalled entry as a new prefix.
        if self.pending_caret_echo.is_none()
            && (self.history_index.is_none() || selection.is_some())
        {
            return prefix_from_selection(&self.value, selection);
        }

        self.history_prefix.clone().unwrap_or_else(|| {
            if self.post_submit_selected {
                String::new()
            } else {
                self.value.clone()
            }
        })
    }

    /// Leave the first `covered` graphemes of the current value unselected and
    /// select the rest, so the matched prefix stays put and the next Up/Down
    /// press continues the same search.
    ///
    /// Both endpoints are grapheme indices, the unit `select_range` counts in
    /// (see [`CaretState`]); byte offsets would land mid-string on any value
    /// that is not pure ASCII, and are silently clamped rather than rejected.
    fn select_after_prefix(&mut self, covered: usize) -> Task<Message> {
        self.pending_caret_echo = Some(InputSource::Other);
        operation::select_range(
            self.input_id.clone(),
            covered,
            self.value.graphemes(true).count(),
        )
    }

    /// Navigate history up (to older commands)
    fn navigate_history_up(&mut self) -> Task<Message> {
        if self.history.is_empty() {
            return Task::none();
        }

        let prefix = self.history_search_prefix();
        let case_sensitive = crate::prefs::current().history_case_sensitive_match;
        let start = self.history_index.map_or(0, |i| i + 1);
        let Some((new_index, covered)) = (start..self.history.len()).find_map(|i| {
            prefix_match_len(&self.history[i], &prefix, case_sensitive).map(|len| (i, len))
        }) else {
            return Task::none(); // No (more) matching entries.
        };

        self.history_index = Some(new_index);
        self.history_prefix = Some(prefix);
        self.value = self.history[new_index].as_str().to_string();
        self.completion_state = None;

        self.select_after_prefix(covered)
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
            Some(i) => {
                let prefix = self.history_search_prefix();
                let case_sensitive = crate::prefs::current().history_case_sensitive_match;
                let newer = (0..i).rev().find_map(|j| {
                    prefix_match_len(&self.history[j], &prefix, case_sensitive).map(|len| (j, len))
                });
                match newer {
                    Some((new_index, covered)) => {
                        self.history_index = Some(new_index);
                        self.history_prefix = Some(prefix);
                        self.value = self.history[new_index].as_str().to_string();
                        self.completion_state = None;

                        self.select_after_prefix(covered)
                    }
                    None => {
                        // Down past the newest match ends the search where it
                        // began: the text being searched for, caret after it,
                        // ready to keep typing. Discarding it would throw away
                        // what the user typed to get here — an empty prefix
                        // (an empty or fully selected box) still empties the
                        // box, as it always did.
                        self.history_index = None;
                        self.history_prefix = None;
                        self.value = prefix;
                        let end = self.value.graphemes(true).count();
                        self.pending_caret_echo = Some(InputSource::Other);
                        operation::move_cursor_to(self.input_id.clone(), end)
                    }
                }
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

        // Find the word at cursor position. A word breaks at whitespace and
        // also at the command separator, so `north;gre` completes `gre` —
        // without the separator break, separator-adjacent command position
        // could never offer anything.
        let cursor_pos = self.value.len(); // Assuming cursor is at end
        let ws_start = self.value[..cursor_pos]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let sep_start = if self.command_separator.is_empty() {
            0
        } else {
            self.value[..cursor_pos]
                .rfind(&self.command_separator)
                .map(|i| i + self.command_separator.len())
                .unwrap_or(0)
        };
        let word_start = ws_start.max(sep_start);

        if word_start >= cursor_pos {
            return Task::none();
        }

        let word_prefix = &self.value[word_start..cursor_pos];
        if word_prefix.is_empty() {
            return Task::none();
        }

        // The position test reads the buffer, so it runs before the
        // completion state takes its borrow.
        let command_position = self.in_command_position(word_start);

        // Initialize or update completion state
        let completion_state = self
            .completion_state
            .get_or_insert_with(|| CompletionState {
                original_text: self.value.clone(),
                prefix: word_prefix.to_string(),
                suggested_words: HashSet::new(),
                suggested_folded: HashSet::new(),
            });

        // Candidate order: Command-alias names first (only in command
        // position), then script-registered suggestions in merge order
        // (creators in first-contribution order, words in insertion order),
        // then the scrollback recency scan. The blacklist filters every
        // source; prefix matching and blacklisting are case-insensitive, and
        // a registered word is inserted with its registered casing. The
        // scrollback scan skips offered words by exact match, folding only
        // against the blacklist and the offered REGISTERED words — with empty
        // word sets, cycling is the plain scrollback behavior, casing pairs
        // and all.
        let folded_prefix = completion_state.prefix.to_lowercase();
        let mut candidate = command_position
            .then(|| {
                self.command_names.iter().find_map(|word| {
                    let folded = word.to_lowercase();
                    (folded.starts_with(&folded_prefix)
                        && !self.blacklist.contains(&folded)
                        && !completion_state.suggested_folded.contains(&folded))
                    .then(|| word.as_str().to_string())
                })
            })
            .flatten();
        if candidate.is_none() {
            candidate = self.suggestions.iter().find_map(|word| {
                let folded = word.to_lowercase();
                (folded.starts_with(&folded_prefix)
                    && !self.blacklist.contains(&folded)
                    && !completion_state.suggested_folded.contains(&folded))
                .then(|| word.as_str().to_string())
            });
        }
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

    /// Open an independent terminal-search editor above the pending command.
    fn enter_search(&mut self) -> Task<Message> {
        if self.search.is_some() {
            return after_widget_tree_update(Message::FocusSearch);
        }
        if self.terminal_buffer.is_none() {
            return Task::none();
        }
        let Some(view) = self.terminal_view.as_ref() else {
            return Task::none();
        };

        let previous_selection = view.selection.borrow().clone();
        view.search_selection.set(true);
        *view.selection.borrow_mut() = Selection::None;
        self.search = Some(SearchState {
            query: String::new(),
            matches: Vec::new(),
            current: None,
            previous_selection,
        });
        // The new text input does not exist in the widget tree until the next
        // view pass. A message boundary lets it mount before focus traversal.
        after_widget_tree_update(Message::FocusSearch)
    }

    /// Leave search mode, clear terminal decorations, and return focus to the
    /// pending game command without altering it.
    fn exit_search(&mut self) -> Task<Message> {
        let Some(search) = self.search.take() else {
            return Task::none();
        };
        if let Some(view) = self.terminal_view.as_ref() {
            // The flag doubles as ownership of the shared selection: a mouse
            // press in the terminal cleared it, and a selection the user
            // dragged while searching survives dismissal instead of
            // reverting to the pre-search snapshot.
            if view.search_selection.get() {
                *view.selection.borrow_mut() = search.previous_selection;
            }
            view.search_selection.set(false);
        }
        // As above, cross a message boundary so the disappearing search row
        // has settled before focus returns to the persistent game editor.
        after_widget_tree_update(Message::FocusGameInput)
    }

    /// Re-scan the live buffer, select the active match, and scroll it into
    /// view. A re-scan on every navigation keeps newly-arrived output in the
    /// cycle.
    fn refresh_search(&mut self, direction: Option<SearchDirection>) {
        let Some(search) = self.search.as_ref() else {
            return;
        };
        let Some(buffer) = self.terminal_buffer.as_ref() else {
            return;
        };

        let anchor = search
            .current
            .and_then(|index| search.matches.get(index))
            .cloned();
        let matches = buffer.borrow().find_text_matches(&search.query);
        let current = if matches.is_empty() {
            None
        } else if let (Some(direction), Some(anchor)) = (direction, anchor) {
            let anchor_index = matches.iter().position(|candidate| candidate == &anchor);
            Some(match (direction, anchor_index) {
                (SearchDirection::Previous, Some(index)) => (index + 1) % matches.len(),
                (SearchDirection::Next, Some(0)) => matches.len() - 1,
                (SearchDirection::Next, Some(index)) => index - 1,
                (_, None) => 0,
            })
        } else {
            Some(0)
        };

        let active = current.and_then(|index| matches.get(index)).cloned();
        if let Some(search) = self.search.as_mut() {
            search.matches = matches;
            search.current = current;
        }

        let Some(view) = self.terminal_view.as_ref() else {
            return;
        };
        // Writing the selection re-claims it for search after any manual
        // drag released it (the terminal press cleared the flag).
        view.search_selection.set(true);
        if let Some(found) = active {
            *view.selection.borrow_mut() = Selection::Selected {
                from: BufferPosition {
                    line: found.line,
                    column: found.start,
                },
                to: BufferPosition {
                    line: found.line,
                    column: found.end,
                },
            };
            view.scroll.request(ScrollRequest::RevealLine(found.line));
        } else {
            *view.selection.borrow_mut() = Selection::None;
        }
    }

    fn refocus_search(&self) -> Task<Message> {
        focus_target(self.search_input_id.clone()).map(Message::SearchFocusSettled)
    }

    fn request_scroll(&self, request: ScrollRequest) {
        if let Some(view) = self.terminal_view.as_ref() {
            view.scroll.request(request);
        }
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
            // clear-on-blur half of that mode lives in `FocusLost`.
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
                Update::with_task(focus_target(self.input_id.clone()).map(|found| {
                    Message::FocusSettled {
                        focused: true,
                        found,
                    }
                }))
            }
            InputOp::Blur => {
                // Targeted: only this input is released, and only if it still
                // holds focus — never whatever else focus moved on to.
                self.pending_caret_echo = Some(InputSource::Script);
                Update::with_task(unfocus_target(self.input_id.clone()).map(|found| {
                    Message::FocusSettled {
                        focused: false,
                        found,
                    }
                }))
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
            Message::SearchChanged(value) => {
                if let Some(search) = self.search.as_mut() {
                    search.query = value;
                }
                self.refresh_search(None);
                Update::none()
            }
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
                // A blur inflicted by a tab switch is an obscure, not an
                // abandon (see `note_obscured`). The mark is consumed
                // unconditionally so it can never leak into a later,
                // genuine blur.
                let obscured = std::mem::take(&mut self.obscured_blur_pending);
                // Only the default mode wipes the line (sent-and-selected, or
                // half-typed) when the input loses focus; the others leave
                // it. Never while masked — clicking the reveal eye blurs the
                // input for a moment, and that must not cost the user the
                // secret (or the stash discipline its buffer).
                if self.search.is_none()
                    && !obscured
                    && !self.masked
                    && crate::prefs::current().command_input_behavior
                        == CommandInputBehavior::SelectAllClearOnBlur
                {
                    self.clear();
                }
                Update::none()
            }
            Message::FocusGained => Update::with_event(Event::FocusGained),
            Message::FocusSettled { focused, found } => {
                if !found {
                    return Update::none();
                }
                self.note_focus_state(focused);
                if focused {
                    Update::with_event(Event::FocusGained)
                } else {
                    Update::none()
                }
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
            Message::EnterSearch => Update::with_task(self.enter_search()),
            Message::FocusSearch if self.search.is_some() => Update::with_task(
                focus_target(self.search_input_id.clone()).map(Message::SearchFocusSettled),
            ),
            Message::FocusSearch => Update::none(),
            Message::ExitSearch => Update::with_task(self.exit_search()),
            Message::FocusGameInput if self.search.is_none() => {
                self.pending_caret_echo = Some(InputSource::Other);
                Update::with_task(focus_target(self.input_id.clone()).map(|found| {
                    Message::FocusSettled {
                        focused: true,
                        found,
                    }
                }))
            }
            Message::FocusGameInput => Update::none(),
            Message::SearchPrevious => {
                self.refresh_search(Some(SearchDirection::Previous));
                Update::with_task(self.refocus_search())
            }
            Message::SearchNext => {
                self.refresh_search(Some(SearchDirection::Next));
                Update::with_task(self.refocus_search())
            }
            Message::SearchFocusSettled(_found) => Update::none(),
            Message::ScrollPageUp => {
                self.request_scroll(ScrollRequest::PageUp);
                Update::none()
            }
            Message::ScrollPageDown => {
                self.request_scroll(ScrollRequest::PageDown);
                Update::none()
            }
            Message::ScrollHome => {
                self.request_scroll(ScrollRequest::Home);
                Update::none()
            }
            Message::ScrollEnd => {
                self.request_scroll(ScrollRequest::End);
                Update::none()
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
                Update::with_task(focus_target(self.input_id.clone()).map(|found| {
                    Message::FocusSettled {
                        focused: true,
                        found,
                    }
                }))
            }
            #[cfg(feature = "web-audio-cpal")]
            Message::OpenAudioPanel => Update::with_event(Event::OpenAudioPanel),
        }
    }

    /// Render the component using a pane font-size override when present.
    ///
    /// Pane font size is display state for the whole pane, not only its
    /// scrollback. Keeping the override at this boundary also applies it to
    /// widgets-only pane inputs, which have no terminal from which to inherit.
    pub fn view_with_font_size(&self, font_size: Option<f32>) -> Element<'_, Message> {
        let prefs = crate::prefs::current();
        let searching = self.search.is_some();
        let effective_size = effective_font_size(font_size, prefs.font_size);

        let game_input = HotkeyMatchingInput::<Message, crate::theme::Theme, iced::Renderer>::new(
            &self.hotkey_lookup,
            &self.placeholder,
            &self.value,
        )
        .font(prefs.font)
        .size(effective_size)
        .id(self.input_id.clone())
        .secure(self.masked && !self.masked_reveal)
        .suppress_clipboard_writes(self.masked)
        .on_input(Message::InputChanged)
        .on_submit(Message::Submit)
        .on_focus(Message::FocusGained)
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
        #[cfg(feature = "web-audio-cpal")]
        let game_input = game_input.on_audio_panel_shortcut(Message::OpenAudioPanel);
        let game_input = if searching {
            game_input.on_fallback_key_pressed(
                keyboard::Key::Named(keyboard::key::Named::Escape),
                keyboard::Modifiers::empty(),
                Message::ExitSearch,
            )
        } else if self.escape_to_main {
            // Pane inputs hand focus back to the main input on Escape.
            game_input.on_key_pressed(
                keyboard::Key::Named(keyboard::key::Named::Escape),
                Message::EscapePressed,
            )
        } else {
            game_input
        };

        // Terminal navigation belongs to the viewport even while the text
        // editor owns focus. User-created hotkeys take precedence; these
        // bindings handle otherwise-unclaimed key presses. Plain Home/End are
        // claimed by the wrapped editor for caret movement whenever there is
        // text to edit, so only an empty input cedes them to the viewport.
        let game_input = if self.terminal_view.is_some() {
            let game_input = game_input
                .on_fallback_key_pressed(
                    keyboard::Key::Character("f".into()),
                    keyboard::Modifiers::CTRL,
                    Message::EnterSearch,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::PageUp),
                    keyboard::Modifiers::empty(),
                    Message::ScrollPageUp,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::PageDown),
                    keyboard::Modifiers::empty(),
                    Message::ScrollPageDown,
                );
            if self.value.is_empty() {
                game_input
                    .on_fallback_key_pressed(
                        keyboard::Key::Named(keyboard::key::Named::Home),
                        keyboard::Modifiers::empty(),
                        Message::ScrollHome,
                    )
                    .on_fallback_key_pressed(
                        keyboard::Key::Named(keyboard::key::Named::End),
                        keyboard::Modifiers::empty(),
                        Message::ScrollEnd,
                    )
            } else {
                game_input
            }
        } else {
            game_input
        };

        // Beware: `history_search_prefix` *needs* `self.caret` to never go
        // stale relative to `self.value`. Never let this go stale.
        let game_input = game_input.on_caret_change(Message::CaretChanged);

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

        let game_row = row![Element::new(game_input), eye_slot]
            .spacing(if self.masked { 4 } else { 0 })
            .align_y(Alignment::Center)
            .width(Length::Fill);

        let search_slot: Element<'_, Message> = if let Some(search) = self.search.as_ref() {
            let search_input =
                HotkeyMatchingInput::<Message, crate::theme::Theme, iced::Renderer>::new(
                    &self.hotkey_lookup,
                    crate::i18n::ts!("terminal-search-placeholder"),
                    &search.query,
                )
                .font(prefs.font)
                .size(effective_size)
                .id(self.search_input_id.clone())
                .on_input(Message::SearchChanged)
                .on_submit(Message::SearchPrevious)
                .style(builtins::text_input::borderless)
                .width(Length::Fill)
                .on_match(Message::HotkeyTriggered)
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::Escape),
                    keyboard::Modifiers::empty(),
                    Message::ExitSearch,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp),
                    keyboard::Modifiers::empty(),
                    Message::SearchPrevious,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown),
                    keyboard::Modifiers::empty(),
                    Message::SearchNext,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::Enter),
                    keyboard::Modifiers::SHIFT,
                    Message::SearchNext,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Character("f".into()),
                    keyboard::Modifiers::CTRL,
                    Message::EnterSearch,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::PageUp),
                    keyboard::Modifiers::empty(),
                    Message::ScrollPageUp,
                )
                .on_fallback_key_pressed(
                    keyboard::Key::Named(keyboard::key::Named::PageDown),
                    keyboard::Modifiers::empty(),
                    Message::ScrollPageDown,
                );
            // As with the game input, plain Home/End edit the query when one
            // exists and scroll the viewport only from an empty search box.
            let search_input = if search.query.is_empty() {
                search_input
                    .on_fallback_key_pressed(
                        keyboard::Key::Named(keyboard::key::Named::Home),
                        keyboard::Modifiers::empty(),
                        Message::ScrollHome,
                    )
                    .on_fallback_key_pressed(
                        keyboard::Key::Named(keyboard::key::Named::End),
                        keyboard::Modifiers::empty(),
                        Message::ScrollEnd,
                    )
            } else {
                search_input
            };
            #[cfg(feature = "web-audio-cpal")]
            let search_input = search_input.on_audio_panel_shortcut(Message::OpenAudioPanel);

            let result = text(format!(
                "{} / {}",
                search.current.map_or(0, |index| index + 1),
                search.matches.len()
            ))
            .size((effective_size * 0.75).max(10.0));
            let previous = button(
                text(crate::assets::bootstrap_icons::CHEVRON_UP)
                    .font(crate::assets::fonts::BOOTSTRAP_ICONS)
                    .size((effective_size * 0.8).max(11.0)),
            )
            .style(builtins::button::link)
            .padding(3);
            let next = button(
                text(crate::assets::bootstrap_icons::CHEVRON_DOWN)
                    .font(crate::assets::fonts::BOOTSTRAP_ICONS)
                    .size((effective_size * 0.8).max(11.0)),
            )
            .style(builtins::button::link)
            .padding(3);
            let (previous, next) = if search.matches.is_empty() {
                (previous, next)
            } else {
                (
                    previous.on_press(Message::SearchPrevious),
                    next.on_press(Message::SearchNext),
                )
            };

            row![
                text(crate::assets::bootstrap_icons::SEARCH)
                    .font(crate::assets::fonts::BOOTSTRAP_ICONS)
                    .size((effective_size * 0.8).max(11.0)),
                Element::new(search_input),
                result,
                previous,
                next,
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .into()
        } else {
            Space::new().height(0).into()
        };

        // Keep the game row at child index 1 in both modes. iced pairs widget
        // state positionally, so the zero-height search slot preserves the
        // game's focus/cursor when the real search row appears above it.
        column![search_slot, game_row]
            .spacing(if searching { 4 } else { 0 })
            .width(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_buffer::selection::{BufferPosition, Selection};
    use smudgy_core::session::styled_line::StyledLine;

    #[test]
    fn pane_font_size_overrides_the_global_input_size() {
        assert_eq!(effective_font_size(Some(23.0), 14.0), 23.0);
        assert_eq!(effective_font_size(None, 14.0), 14.0);
    }

    /// The command-position table from the design plan's fixtures §12,
    /// asserted at the observable level: does Tab offer a Command-alias name
    /// for this input? (Everything between the caret's word start and the
    /// nearest boundary — start of input, or the command separator — must be
    /// whitespace.)
    #[test]
    fn command_names_complete_only_in_command_position() {
        let mut input = SessionInput::new();
        input.set_command_names(Arc::new(vec![Arc::new("greet".to_string())]));
        let mut offers = |separator: &str, value: &str| -> bool {
            input.set_command_separator(separator.to_string());
            input.completion_state = None;
            input.value = value.to_string();
            let _ = input.handle_tab_completion();
            // With no scrollback buffer and no word sets, the only possible
            // completion source is the command-name list.
            input.value != value
        };

        assert!(offers(";", "gre"));
        assert!(!offers(";", "say gre"));
        assert!(offers(";", "north;gre"));
        assert!(
            offers(";", "north; gre"),
            "whitespace after the boundary is fine"
        );
        assert!(offers(";;", "north;;gre"));
        assert!(!offers(";;", "north;gre"), "the separator is ;;, not ;");
        assert!(
            !offers("", "north;gre"),
            "empty separator: only the start qualifies"
        );
        assert!(offers("", "gre"));
    }

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
            found: false,
        };

        let mut unrelated = FakeFocusable { focused: true };
        Operation::<bool>::focusable(
            &mut op,
            Some(&other),
            iced::Rectangle::default(),
            &mut unrelated,
        );
        assert!(unrelated.focused, "an unrelated widget keeps focus");

        let mut anonymous = FakeFocusable { focused: true };
        Operation::<bool>::focusable(&mut op, None, iced::Rectangle::default(), &mut anonymous);
        assert!(anonymous.focused, "an id-less widget keeps focus");

        let mut targeted = FakeFocusable { focused: true };
        Operation::<bool>::focusable(
            &mut op,
            Some(&target),
            iced::Rectangle::default(),
            &mut targeted,
        );
        assert!(!targeted.focused, "the target is released");
        assert!(matches!(op.finish(), Outcome::Some(true)));
    }

    /// The focus-transfer operation focuses the target and releases every
    /// other focusable it visits (keyboard focus is exclusive). An
    /// already-focused target is left untouched: a text input's `focus()`
    /// moves its caret to the end, so handing focus to the widget that
    /// already holds it must not call it.
    #[test]
    fn focus_target_focuses_the_target_without_refocusing_a_holder() {
        struct FakeFocusable {
            focused: bool,
            focus_calls: usize,
        }
        impl Focusable for FakeFocusable {
            fn is_focused(&self) -> bool {
                self.focused
            }
            fn focus(&mut self) {
                self.focused = true;
                self.focus_calls += 1;
            }
            fn unfocus(&mut self) {
                self.focused = false;
            }
        }

        let target = Id::unique();
        let other = Id::unique();
        let mut op = FocusTarget {
            target: target.clone(),
            found: false,
        };

        let mut unfocused_target = FakeFocusable {
            focused: false,
            focus_calls: 0,
        };
        Operation::<bool>::focusable(
            &mut op,
            Some(&target),
            iced::Rectangle::default(),
            &mut unfocused_target,
        );
        assert!(unfocused_target.focused, "the target gains focus");
        assert_eq!(unfocused_target.focus_calls, 1);

        let mut focused_target = FakeFocusable {
            focused: true,
            focus_calls: 0,
        };
        Operation::<bool>::focusable(
            &mut op,
            Some(&target),
            iced::Rectangle::default(),
            &mut focused_target,
        );
        assert!(focused_target.focused, "the target keeps focus");
        assert_eq!(
            focused_target.focus_calls, 0,
            "a focus holder is not re-focused (its caret would move)"
        );

        let mut holder = FakeFocusable {
            focused: true,
            focus_calls: 0,
        };
        Operation::<bool>::focusable(
            &mut op,
            Some(&other),
            iced::Rectangle::default(),
            &mut holder,
        );
        assert!(!holder.focused, "every other focusable is released");
        assert!(matches!(op.finish(), Outcome::Some(true)));
    }

    /// An Aâ†’Bâ†’A tab round trip can complete entirely through widget
    /// operations, without either hidden input receiving a normal event.
    /// Operation settlement must therefore restore the target's focus mirror
    /// and retire the old obscure marker; otherwise A misses its second gain
    /// and its next genuine blur is incorrectly excused.
    #[test]
    fn tab_round_trip_focus_settlement_restores_exactly_one_active_input() {
        let mut a = SessionInput::new();
        let mut b = SessionInput::new();
        let _ = a.update(Message::InputChanged("unfinished command".to_string()));
        a.note_focus_state(true);

        // A â†’ B: window selection publishes A's synthetic loss, then the
        // successful focus operation settles B's gain.
        a.note_obscured();
        let b_gain = b.update(Message::FocusSettled {
            focused: true,
            found: true,
        });
        assert!(matches!(b_gain.event, Some(Event::FocusGained)));
        assert!(!a.mirror_snapshot().focused);
        assert!(b.mirror_snapshot().focused);

        // B â†’ A before A ever receives its deferred widget blur. Settlement
        // is authoritative and clears A's obsolete obscure marker.
        b.note_obscured();
        let a_gain = a.update(Message::FocusSettled {
            focused: true,
            found: true,
        });
        assert!(matches!(a_gain.event, Some(Event::FocusGained)));
        assert!(a.mirror_snapshot().focused);
        assert!(!b.mirror_snapshot().focused);

        assert!(!a.obscured_blur_pending);
    }

    /// Switching tabs must preserve every input's text — TabHost keeps
    /// inactive subtrees mounted precisely so ephemeral state survives
    /// (ui/src/widgets/tab_host.rs module docs). The switch's focus
    /// transfer blurs the obscured tab's input, and the widget publishes
    /// that blur only once its tab is re-selected (an obscured subtree
    /// receives no events); under the `SelectAllClearOnBlur`
    /// preference a cause-blind `FocusLost` would clear the model-owned
    /// value. The tab-switch path marks the input as obscured first, and
    /// the one-shot mark rides exactly that deferred blur.
    #[test]
    fn tab_switch_blur_preserves_in_progress_text() {
        let mut input = SessionInput::new();
        let _ = input.update(Message::InputChanged("kill troll with axe".to_string()));
        // What `select_tab` does before its focus operations reach the
        // obscured subtree through TabHost::operate.
        input.note_obscured();
        assert!(
            !input.mirror_snapshot().focused,
            "an obscured input is unfocused immediately"
        );
        // What the obscured input's next update publishes once its tab is
        // re-selected.
        let _ = input.update(Message::FocusLost);
        assert_eq!(
            input.value(),
            "kill troll with axe",
            "a tab switch must not cost the user their in-progress command"
        );
        assert!(
            !input.obscured_blur_pending,
            "the obscure mark must not outlive the blur it excused"
        );
    }

    /// A genuine blur — clicking away, with no tab switch marking the input
    /// — still clears under the `SelectAllClearOnBlur` preference.
    #[test]
    fn genuine_blur_clears_under_the_clear_on_blur_preference() {
        let settings = smudgy_core::models::settings::Settings {
            command_input_behavior: CommandInputBehavior::SelectAllClearOnBlur,
            ..Default::default()
        };
        with_prefs(settings, || {
            let mut input = SessionInput::new();
            let _ = input.update(Message::InputChanged("kill troll with axe".to_string()));
            let _ = input.update(Message::FocusLost);
            assert_eq!(input.value(), "", "an unmarked blur clears the line");
        });
    }

    #[test]
    fn genuine_blur_preserves_text_under_the_default_preference() {
        with_prefs(Default::default(), || {
            let mut input = SessionInput::new();
            let _ = input.update(Message::InputChanged("kill troll with axe".to_string()));
            let _ = input.update(Message::FocusLost);
            assert_eq!(
                input.value(),
                "kill troll with axe",
                "the default select-all mode leaves text alone on blur"
            );
        });
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

    #[test]
    fn loaded_history_is_sanitized_capped_and_not_a_new_mutation() {
        let mut entries = vec!["newest".to_string(), " ".to_string(), "newest".to_string()];
        entries.extend((0..1010).map(|i| format!("command-{i}")));

        let input = SessionInput::new().with_history(entries);

        let loaded = history_entries(&input);
        assert_eq!(loaded.len(), 1000);
        assert_eq!(loaded[0], "newest");
        assert_eq!(loaded[1], "command-0");
        assert_eq!(loaded[99], "command-98");
        assert_eq!(loaded[999], "command-998");
        assert_eq!(input.history_revision(), 0);
        assert!(input.history_index.is_none());
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

        // Cap parity: history holds at most 1000 entries, oldest falling off.
        for i in 0..1050 {
            let _ = input.apply_script_op(&InputOp::HistoryPush(Arc::new(format!("cmd{i}"))));
        }
        let entries = history_entries(&input);
        assert_eq!(entries.len(), 1000, "the cap applies to pushed entries too");
        assert_eq!(entries[0], "cmd1049", "newest first after the burst");
        assert!(
            !entries.iter().any(|e| e == "kill rat"),
            "the oldest entries fell off the back"
        );

        // A pushed entry is recallable exactly like a typed one.
        let _ = input.update(Message::InputChanged(String::new()));
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "cmd1049", "Up recalls the pushed entry");
    }

    /// Apply `settings` to the global `crate::prefs` snapshot for the duration
    /// of `body`, then restore it - serialized against any other test doing
    /// the same via [`crate::prefs::lock_prefs_test`], as `PREFS` is
    /// process-wide and `cargo test` runs in parallel threads.
    fn with_prefs<R>(
        settings: smudgy_core::models::settings::Settings,
        body: impl FnOnce() -> R,
    ) -> R {
        /// Restores the defaults on the way out however `body` leaves — a
        /// panicking assertion would otherwise leak its settings into whatever
        /// test takes the lock next, turning one failure into several.
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::prefs::apply(&smudgy_core::models::settings::Settings::default());
            }
        }

        // Declared after the guard, so it drops *before* it: the defaults are
        // back in place by the time the next test can take the lock.
        let _guard = crate::prefs::lock_prefs_test();
        let _restore = Restore;
        crate::prefs::apply(&settings);
        body()
    }

    /// As prefix-matching is always turned on, an empty input box should
    /// display the full history.
    #[test]
    fn empty_input_matches_every_history_entry() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "gt foo");
        submit_unmasked(&mut input, "bash mob");
        let _ = input.update(Message::InputChanged(String::new()));

        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "bash mob",
            "Up with nothing typed jumps to the newest entry"
        );
    }

    /// Up straight after a submission browses the whole history, not just the
    /// entries starting with the line just sent.
    ///
    /// The default post-submit behavior leaves the sent text in the box and
    /// selects all of it, which is an empty prefix — but the `select_all` is a
    /// `Task`, and its `CaretChanged` echo has not landed when Up is pressed
    /// straight afterwards (never, in a unit test). Reading the live caret
    /// there sees the pre-submit cursor: no selection, so the whole line as
    /// the prefix. `post_submit_selected` is the synchronous truth that keeps
    /// the answer the same however fast the keys arrive.
    #[test]
    fn up_straight_after_a_submission_browses_the_whole_history() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "gt foo");
        submit_unmasked(&mut input, "bash mob");
        assert_eq!(input.value, "bash mob", "the sent line is left in the box");
        assert!(input.post_submit_selected, "and is fully selected");

        // No InputChanged in between: exactly the Enter-then-Up sequence.
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "bash mob", "Up recalls the newest entry");
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "gt foo",
            "a second Up keeps browsing, rather than filtering by \"bash mob\""
        );
    }

    /// A recalled entry that is not pure ASCII anchors its selection by
    /// grapheme count, and keeps searching for the same prefix afterwards.
    #[test]
    fn history_search_handles_non_ascii_entries() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "gö north");
        submit_unmasked(&mut input, "bash mob");
        submit_unmasked(&mut input, "gö south");
        let _ = input.update(Message::InputChanged("gö ".to_string()));

        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "gö south");
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "gö north",
            "the search continues past the non-matching \"bash mob\""
        );

        // The prefix the next press searches for is the typed text, not a
        // byte-offset slice of the recalled entry ("gö n" would be the
        // symptom of anchoring the selection by byte length).
        assert_eq!(input.history_prefix.as_deref(), Some("gö "));
    }

    /// Up finds the newest history entry starting with the typed text,
    /// skipping past any other more-recent non-matching entries.
    #[test]
    fn history_search_up_skips_a_non_matching_newer_entry() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "gt foo");
        submit_unmasked(&mut input, "bash mob");
        // Look backwards for a history entry starting with "gt", skipping the
        // newer "bash mob".
        let _ = input.update(Message::InputChanged("gt ".to_string()));

        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "gt foo",
            "Up skips the newer, non-matching \"bash mob\" to find \"gt foo\""
        );
    }

    /// No matching entry at all: Up is a no-op, the typed text is left
    /// untouched.
    #[test]
    fn history_search_up_is_a_no_op_when_nothing_matches() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "bash mob");
        let _ = input.update(Message::InputChanged("gt ".to_string()));

        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "gt ");
    }

    /// Regression test for a real bug: repeated Up presses stopped
    /// advancing after the first match. The widget's `CaretChanged` echo of
    /// the first press's `select_range` is not guaranteed to have arrived
    /// by the time the second press is processed -- a bare unit test never
    /// executes that `Task` at all, which is exactly the same "no fresh
    /// selection yet" situation the real app hit under fast repeated
    /// presses. `history_prefix`'s synchronous fallback (see
    /// `history_search_prefix`) is what makes this pass either way.
    #[test]
    fn history_search_continues_the_same_prefix_across_presses_without_a_caret_echo() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "gt foo");
        submit_unmasked(&mut input, "bash mob");
        submit_unmasked(&mut input, "gt bar");
        let _ = input.update(Message::InputChanged("gt ".to_string()));

        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "gt bar",
            "first Up finds the newest matching entry"
        );

        // This second `Up` press should still find the next entry, "gt foo".
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "gt foo",
            "second Up continues searching for \"gt \" and skips \"bash mob\""
        );

        // No more matches: a further Up is a no-op, value unchanged.
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "gt foo");

        // Down retraces the matches, then hands back the text the search
        // started from rather than an empty box.
        let _ = input.update(Message::NavigateHistoryDown);
        assert_eq!(input.value, "gt bar");
        let _ = input.update(Message::NavigateHistoryDown);
        assert_eq!(
            input.value, "gt ",
            "Down past the newest match restores what was being searched for"
        );
        assert!(input.history_index.is_none());
        assert!(input.history_prefix.is_none());

        // And the search is genuinely over: Up starts again from the restored
        // text, finding the newest match rather than resuming mid-history.
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "gt bar");
    }

    /// An empty box (nothing typed at all) still browses the whole history
    /// across repeated presses, the same fallback path as the prefix-search
    /// continuation above.
    #[test]
    fn empty_prefix_history_browsing_continues_across_presses() {
        let mut input = SessionInput::new();
        submit_unmasked(&mut input, "gt foo");
        submit_unmasked(&mut input, "bash mob");
        let _ = input.update(Message::InputChanged(String::new()));

        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(input.value, "bash mob");
        let _ = input.update(Message::NavigateHistoryUp);
        assert_eq!(
            input.value, "gt foo",
            "second Up still advances to the older entry"
        );
    }

    /// Test for the `history_case_sensitive_match` setting: off (the
    /// default) matches regardless of letter case; on requires an exact
    /// case match.
    #[test]
    fn history_case_sensitive_match_toggle() {
        use smudgy_core::models::settings::Settings;
        with_prefs(Settings::default(), || {
            let mut input = SessionInput::new();
            submit_unmasked(&mut input, "Gt foo");
            let _ = input.update(Message::InputChanged("GT ".to_string()));

            let _ = input.update(Message::NavigateHistoryUp);
            assert_eq!(
                input.value, "Gt foo",
                "case-insensitive (default) match finds the differently-cased entry, \
                 displayed with its own original casing"
            );
        });
        with_prefs(
            Settings {
                history_case_sensitive_match: true,
                ..Settings::default()
            },
            || {
                let mut input = SessionInput::new();
                submit_unmasked(&mut input, "gt foo");
                let _ = input.update(Message::InputChanged("GT ".to_string()));

                let _ = input.update(Message::NavigateHistoryUp);
                assert_eq!(
                    input.value, "GT ",
                    "case-sensitive match: no match, Up is a no-op"
                );
            },
        );
    }

    /// Test for the `max_history` setting: a configured cap evicts the
    /// oldest entry once exceeded, and `0` means unlimited -- nothing is
    /// ever evicted.
    #[test]
    fn max_history_caps_by_configured_value_and_zero_means_unlimited() {
        use smudgy_core::models::settings::Settings;

        with_prefs(
            Settings {
                max_history: 2,
                ..Settings::default()
            },
            || {
                let mut input = SessionInput::new();
                submit_unmasked(&mut input, "one");
                submit_unmasked(&mut input, "two");
                submit_unmasked(&mut input, "three");
                assert_eq!(
                    input.history.len(),
                    2,
                    "the oldest entry is evicted once the cap is exceeded"
                );
                assert_eq!(
                    input.history.back().map(|entry| entry.as_str()),
                    Some("two")
                );
                assert_eq!(
                    input.history.front().map(|entry| entry.as_str()),
                    Some("three")
                );
            },
        );

        with_prefs(
            Settings {
                max_history: 0,
                ..Settings::default()
            },
            || {
                let mut input = SessionInput::new();
                for i in 0..250 {
                    submit_unmasked(&mut input, &format!("command {i}"));
                }
                assert_eq!(
                    input.history.len(),
                    250,
                    "0 means unlimited -- nothing is evicted"
                );
            },
        );
    }

    #[test]
    fn prefix_from_selection_uses_text_before_the_selection_start() {
        assert_eq!(
            prefix_from_selection("gt beware!", None),
            "gt beware!",
            "no selection -- the whole value is the prefix"
        );
        assert_eq!(
            prefix_from_selection("gt beware!", Some((0, 10))),
            "",
            "select-all -- an empty prefix, matching every entry"
        );
        assert_eq!(
            prefix_from_selection("gt beware!", Some((3, 10))),
            "gt ",
            "a trailing selection leaves only the text before it"
        );
        assert_eq!(
            prefix_from_selection("", None),
            "",
            "an empty box has an empty prefix either way"
        );
    }

    #[test]
    fn prefix_from_selection_converts_grapheme_offsets_to_bytes() {
        // "café" is 4 graphemes but 5 bytes (é is 2 bytes in UTF-8); a
        // selection starting at grapheme 3 must land after "caf", not
        // panic on a byte boundary inside "é".
        assert_eq!(prefix_from_selection("café bar", Some((3, 8))), "caf");
    }

    #[test]
    fn prefix_match_len_empty_prefix_matches_everything_covering_nothing() {
        assert_eq!(prefix_match_len("anything at all", "", true), Some(0));
        assert_eq!(prefix_match_len("anything at all", "", false), Some(0));
    }

    #[test]
    fn prefix_match_len_respects_case_sensitivity() {
        assert_eq!(prefix_match_len("GT foo", "gt", false), Some(2));
        assert_eq!(prefix_match_len("GT foo", "gt", true), None);
        assert_eq!(prefix_match_len("GT foo", "GT", true), Some(2));
        assert_eq!(prefix_match_len("bash mob", "gt", false), None);
    }

    /// The covered length is a grapheme count, the unit `select_range` takes —
    /// not a byte length. They differ for anything outside ASCII, and feeding
    /// bytes to `select_range` mis-anchors the selection left on the recalled
    /// entry (silently, since it clamps rather than rejects).
    #[test]
    fn prefix_match_len_counts_graphemes_not_bytes() {
        // "gö " is 3 graphemes but 4 bytes (ö is 2 bytes in UTF-8).
        assert_eq!(prefix_match_len("gö north", "gö ", false), Some(3));
        assert_ne!("gö ".len(), 3, "the byte length is the wrong anchor");

        // A grapheme cluster counts once however many code points it spans.
        assert_eq!(prefix_match_len("👨‍👩‍👧 home", "👨‍👩‍👧", true), Some(1));
    }

    /// Case folding is applied per grapheme, so the covered length is valid
    /// for the *entry* as well as the prefix even when the two differ in case.
    #[test]
    fn prefix_match_len_covers_the_entrys_own_graphemes_when_case_differs() {
        assert_eq!(prefix_match_len("Gö North", "gö ", false), Some(3));
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

    fn searchable_input(
        lines: &[&str],
    ) -> (SessionInput, Rc<RefCell<Selection>>, TerminalViewHandle) {
        let buffer = buffer_with_lines(lines);
        let view = TerminalViewHandle::default();
        let selection = view.selection.clone();
        let input = SessionInput::new()
            .with_terminal_buffer(buffer)
            .with_terminal_view(view.clone());
        (input, selection, view)
    }

    #[test]
    fn terminal_search_keeps_command_editable_and_restores_selection_on_exit() {
        let (mut input, selection, view) = searchable_input(&["dragon old", "quiet", "DRAGON new"]);
        let previous_selection = Selection::Selected {
            from: BufferPosition { line: 2, column: 0 },
            to: BufferPosition { line: 2, column: 1 },
        };
        *selection.borrow_mut() = previous_selection.clone();
        let _ = input.update(Message::InputChanged("say hello".to_string()));

        let _ = input.update(Message::EnterSearch);
        assert!(view.search_selection.get());
        let _ = input.update(Message::SearchChanged("dragon".to_string()));
        assert_eq!(input.value(), "say hello");
        assert_eq!(
            input.search.as_ref().map(|search| search.query.as_str()),
            Some("dragon")
        );
        let _ = input.update(Message::InputChanged("say goodbye".to_string()));
        assert_eq!(input.value(), "say goodbye");
        assert_eq!(
            input.search.as_ref().map(|search| search.query.as_str()),
            Some("dragon"),
            "editing the still-mounted game input must not replace the search query"
        );
        assert_eq!(
            *selection.borrow(),
            Selection::Selected {
                from: BufferPosition { line: 3, column: 0 },
                to: BufferPosition { line: 3, column: 6 },
            }
        );
        assert_eq!(
            input.search.as_ref().and_then(|search| search.current),
            Some(0)
        );
        assert_eq!(
            input.search.as_ref().map(|search| search.matches.len()),
            Some(2)
        );

        let _ = input.update(Message::SearchPrevious);
        assert_eq!(
            input.search.as_ref().and_then(|search| search.current),
            Some(1)
        );
        assert_eq!(
            *selection.borrow(),
            Selection::Selected {
                from: BufferPosition { line: 1, column: 0 },
                to: BufferPosition { line: 1, column: 6 },
            }
        );

        let _ = input.update(Message::SearchNext);
        assert_eq!(
            input.search.as_ref().and_then(|search| search.current),
            Some(0)
        );
        assert_eq!(
            *selection.borrow(),
            Selection::Selected {
                from: BufferPosition { line: 3, column: 0 },
                to: BufferPosition { line: 3, column: 6 },
            }
        );
        assert_eq!(
            view.scroll.take_requests().into_iter().collect::<Vec<_>>(),
            vec![
                ScrollRequest::RevealLine(3),
                ScrollRequest::RevealLine(1),
                ScrollRequest::RevealLine(3),
            ]
        );

        let _ = input.update(Message::ExitSearch);
        assert!(input.search.is_none());
        assert!(!view.search_selection.get());
        assert_eq!(input.value(), "say goodbye");
        assert_eq!(*selection.borrow(), previous_selection);
    }

    #[test]
    fn terminal_navigation_keys_queue_viewport_requests() {
        let (mut input, _selection, view) = searchable_input(&["one"]);

        let _ = input.update(Message::ScrollPageUp);
        let _ = input.update(Message::ScrollPageDown);
        let _ = input.update(Message::ScrollHome);
        let _ = input.update(Message::ScrollEnd);

        assert_eq!(
            view.scroll.take_requests().into_iter().collect::<Vec<_>>(),
            vec![
                ScrollRequest::PageUp,
                ScrollRequest::PageDown,
                ScrollRequest::Home,
                ScrollRequest::End,
            ]
        );
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
        let settings = smudgy_core::models::settings::Settings {
            command_input_behavior: CommandInputBehavior::SelectAllClearOnBlur,
            ..Default::default()
        };
        with_prefs(settings, || {
            let mut input = SessionInput::new();
            // Unmasked, this mode clears on blur.
            let _ = input.update(Message::InputChanged("half a command".to_string()));
            let _ = input.update(Message::FocusLost);
            assert_eq!(input.value, "", "the unmasked input clears on blur");

            // Masked, the same blur (e.g. clicking the reveal eye) keeps the
            // secret in progress.
            let _ = input.apply_script_op(&InputOp::SetMasked(true));
            let _ = input.update(Message::InputChanged("hunter2".to_string()));
            let _ = input.update(Message::FocusLost);
            assert_eq!(
                input.value, "hunter2",
                "a masked input survives losing focus"
            );
        });
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
    fn focus_gain_is_reported_to_the_parent() {
        let mut input = SessionInput::new();
        let update = input.update(Message::FocusGained);
        assert!(matches!(update.event, Some(Event::FocusGained)));
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
