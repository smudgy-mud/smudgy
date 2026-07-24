//! The scripting-facing command-input surface (`docs/input.md`).
//!
//! The authoritative text buffer lives in the UI's input widget; scripts see a
//! **session-thread mirror** of it and mutate it through **operations**:
//!
//! - Writes: an input op queues [`super::RuntimeAction::InputApply`], whose dispatch
//!   arm forwards it to the UI as [`crate::session::SessionEvent::InputOp`]; the UI
//!   applies it to the live widget.
//! - Reads: sync ops against the [`InputMirror`], fed by the UI's coalesced
//!   [`super::RuntimeAction::InputStateChanged`] messages. Reads reflect the last
//!   delivered change (eventually consistent).
//! - The mirror is **interest-gated**: the UI sends state messages only after the
//!   session thread has flagged interest (the first mirror read; writes never
//!   flag it), so sessions that never read the input pay nothing per keystroke.
//!
//! Inputs are addressed by [`PaneKey`] — the main input is the input of
//! [`super::pane::MAIN_PANE_KEY`] — so pane-hosted inputs share the same
//! vocabulary.
//!
//! Script-facing cursor and selection positions — the mirror snapshot fields
//! and the [`InputOp::SetCursor`]/[`InputOp::Select`] arguments — count
//! **UTF-16 code units** into the value as a JavaScript string, the unit JS
//! string indexing uses. The UI converts to and from the widget's own
//! grapheme indices at its boundary.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;

use super::origin::{IsolateId, Origin};
use super::pane::PaneKey;
use super::script_engine::FunctionId;

/// One scripted mutation of an input, applied by the UI to the live widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOp {
    /// Replace the whole buffer with `text` (cursor moves to the end).
    Replace(Arc<String>),
    /// Append `text` to the buffer (cursor moves to the end).
    Append(Arc<String>),
    /// Empty the buffer.
    Clear,
    /// Replace with `text` and select it all, so the user's next keystroke
    /// discards the proposal.
    Propose(Arc<String>),
    /// Place the cursor at a position in UTF-16 code units (clamped to the
    /// buffer by the UI).
    SetCursor(usize),
    /// Select the range `start..end` in UTF-16 code units (clamped to the
    /// buffer by the UI).
    Select(usize, usize),
    SelectAll,
    Focus,
    Blur,
    /// Submit the current contents exactly as if the user pressed Enter.
    Submit,
    /// Engage or release masked (password) mode, with the pre-mask
    /// stash/restore semantics the UI owns (`docs/input.md` §3.10).
    SetMasked(bool),
    /// Add a line to the input's real history without sending it, with the
    /// same semantics as a typed submission entering history: deduplicated,
    /// newest first, capped (`docs/input.md` §3.9).
    HistoryPush(Arc<String>),
    /// Empty the input's real history.
    HistoryClear,
}

/// What caused an input state change — carried on every mirror update so
/// change observers can distinguish typing from programmatic stuffing. When
/// one update coalesces mixed causes, the last mutation wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    /// The user typed or edited directly.
    User,
    /// A script handle op (replace/append/propose/clear/…).
    Script,
    /// A link action stuffed the box.
    Link,
    /// Anything else: history recall, completion cycling, post-submit behavior.
    Other,
}

impl InputSource {
    /// The script-facing tag, as carried on the `input:change` payload
    /// (`docs/input.md` §3.5).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Script => "script",
            Self::Link => "link",
            Self::Other => "other",
        }
    }
}

/// One input's mirrored state — also the wire shape the read op hands to JS
/// (`selection` crosses as `[start, end]` or null). While masked the UI sends
/// (and the mirror stores) **no content**: `value` is empty and
/// `cursor`/`selection` are zeroed, so masked keystrokes never reach the
/// session thread.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSnapshot {
    #[serde(serialize_with = "serialize_arc_str")]
    pub value: Arc<String>,
    /// Cursor position in UTF-16 code units of `value` as a JS string.
    pub cursor: usize,
    /// Selected range `(start, end)` in UTF-16 code units with
    /// `start <= end`; `None` when nothing is selected.
    pub selection: Option<(usize, usize)>,
    pub focused: bool,
    pub masked: bool,
}

/// Serialize the shared value as a plain string (spares the snapshot a serde
/// `rc`-feature dependency).
fn serialize_arc_str<S: serde::Serializer>(
    value: &Arc<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value)
}

impl InputSnapshot {
    /// The masked form of `self`: the flags survive, the content does not.
    /// Applied defensively on the session side too, so a misbehaving state
    /// message can never land masked content in the mirror.
    #[must_use]
    pub fn content_suppressed(&self) -> Self {
        Self {
            value: Arc::new(String::new()),
            cursor: 0,
            selection: None,
            focused: self.focused,
            masked: self.masked,
        }
    }
}

/// The session-thread mirror of every scripted input's state, plus the
/// interest flag that gates the UI's per-keystroke state messages. Session-
/// scoped (it survives engine reloads, like the pane registry): interest is a
/// session fact — the UI keeps feeding the mirror across a reload, and handle
/// reads in the rebuilt engine stay warm.
///
/// Also holds each input's mirrored **history** (`docs/input.md`
/// §3.9). History is not interest-gated: it changes only when a submission or
/// a scripted push/clear lands (never per keystroke), so the UI feeds it
/// unconditionally and history reads are exact with respect to the last
/// submission. Masked submissions never enter the UI's history, so they never
/// reach this mirror either.
#[derive(Debug, Default)]
pub struct InputMirror {
    states: HashMap<PaneKey, InputSnapshot>,
    /// Each input's history entries, newest first — the UI's `VecDeque` in
    /// its documented order, shared as one `Arc` per delivered update.
    histories: HashMap<PaneKey, Arc<Vec<Arc<String>>>>,
    interest: bool,
}

impl InputMirror {
    /// The mirrored state of one input; an input never reported on reads as
    /// the default (empty, unfocused) snapshot.
    #[must_use]
    pub fn snapshot(&self, key: PaneKey) -> InputSnapshot {
        self.states.get(&key).cloned().unwrap_or_default()
    }

    /// Store one delivered state update. Masked updates are content-
    /// suppressed here regardless of what the message carried.
    ///
    /// Returns the snapshot this update replaced. `None` means the input had
    /// never reported — the update is its **baseline**, distinct from a prior
    /// report that happened to equal the default snapshot. The dispatch arm
    /// leans on the distinction: the UI's warm-up push (sent when interest is
    /// flagged, carrying whatever state already exists) must seed the mirror
    /// without reading as a change/focus edge.
    pub fn apply(&mut self, key: PaneKey, snapshot: InputSnapshot) -> Option<InputSnapshot> {
        let snapshot = if snapshot.masked {
            snapshot.content_suppressed()
        } else {
            snapshot
        };
        self.states.insert(key, snapshot)
    }

    /// The mirrored history of one input, newest first; an input never
    /// reported on reads as empty.
    #[must_use]
    pub fn history(&self, key: PaneKey) -> Arc<Vec<Arc<String>>> {
        self.histories.get(&key).cloned().unwrap_or_default()
    }

    /// Store one delivered history update (sent by the UI whenever an input's
    /// real history changes — no interest gate, see the type docs).
    pub fn apply_history(&mut self, key: PaneKey, entries: Arc<Vec<Arc<String>>>) {
        self.histories.insert(key, entries);
    }

    /// Drop `key`'s mirrored state and history — the pane closed. `PaneKey`s
    /// are never reused, so the entries could never be read again; evicting
    /// them keeps the mirror from growing under split/close churn.
    pub fn remove(&mut self, key: PaneKey) {
        self.states.remove(&key);
        self.histories.remove(&key);
    }

    /// Whether the session thread has asked the UI for input state.
    #[must_use]
    pub fn interest(&self) -> bool {
        self.interest
    }

    /// Flag interest; returns `true` on the flip (the caller then tells the
    /// UI once). Interest never clears — handles are ambient, so the first
    /// mirror read marks the session as one that reads its input.
    pub fn flag_interest(&mut self) -> bool {
        let flipped = !self.interest;
        self.interest = true;
        flipped
    }
}

/// The mirror handle shared between the runtime (whose dispatch arm writes
/// delivered state) and every isolate's ops (which read it synchronously).
pub(crate) type SharedInputMirror = Rc<RefCell<InputMirror>>;

/// One in-flight typed submission, alive while its `sys:input` handlers run.
/// The dispatch arm installs it before the handler splice and consumes it in
/// the completion action that follows; the ambient `submission` object's ops
/// read and mutate it in between. Handlers compose through the shared text —
/// a later handler's `text()` sees an earlier handler's `replace()` — and
/// cancellation is sticky, so a cancel wins over any replace regardless of
/// handler order.
///
/// `generation` is the staleness nonce (the widget-token instance-nonce
/// pattern): every installed submission gets a fresh one, the `sys:input`
/// delivery wrapper captures it for the duration of each synchronous handler
/// call, and the submission ops refuse a mismatch — so an async handler
/// continuation that outlives its submission throws instead of cancelling or
/// rewriting a later, unrelated one.
#[derive(Debug)]
pub struct InputSubmission {
    text: Arc<String>,
    cancelled: bool,
    generation: u32,
}

impl InputSubmission {
    #[must_use]
    pub fn new(text: Arc<String>, generation: u32) -> Self {
        Self {
            text,
            cancelled: false,
            generation,
        }
    }

    /// The staleness nonce this submission was installed under.
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The line as it currently stands: the text as typed until a handler
    /// replaces it.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Substitute what enters the pipeline when the submission completes.
    pub fn replace(&mut self, text: &str) {
        self.text = Arc::new(text.to_string());
    }

    /// Swallow the submission: nothing enters the pipeline.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// The final text, for the completion arm's pipeline hand-off.
    #[must_use]
    pub fn into_text(self) -> Arc<String> {
        self.text
    }
}

/// The submission slot shared between the runtime's dispatch arms (which
/// install and consume the live submission) and every isolate's submission
/// ops (which act on it). The live cell is `None` outside a `sys:input`
/// handler splice — the ops throw then. Single-flight by construction: the
/// handler splice and its completion drain depth-first before the channel can
/// deliver another submission. The generation counter lives here beside the
/// cell it stamps, so the two can never drift; it is session-scoped (an
/// engine reload clears the live submission but not the counter).
#[derive(Debug, Default)]
pub struct InputSubmissionSlot {
    next_generation: u32,
    live: Option<InputSubmission>,
}

impl InputSubmissionSlot {
    /// Install a fresh submission, stamped with the next generation.
    /// Generations start at 1 and skip 0 on wrap: 0 is the script side's
    /// "no delivery in scope" sentinel, so it must never name a submission.
    pub fn install(&mut self, text: Arc<String>) {
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        self.live = Some(InputSubmission::new(text, self.next_generation));
    }

    /// The live submission, for the ops to act on.
    pub fn live_mut(&mut self) -> Option<&mut InputSubmission> {
        self.live.as_mut()
    }

    /// The live submission's generation; 0 when none is live (never a valid
    /// generation, see [`Self::install`]).
    #[must_use]
    pub fn live_generation(&self) -> u32 {
        self.live.as_ref().map_or(0, InputSubmission::generation)
    }

    /// Consume the live submission (the completion arm's read, and the
    /// reload teardown's clear).
    pub fn take(&mut self) -> Option<InputSubmission> {
        self.live.take()
    }
}

/// The shared handle to the session's [`InputSubmissionSlot`].
pub(crate) type SharedInputSubmission = Rc<RefCell<InputSubmissionSlot>>;

/// Which of an input's two word sets a registry call addresses
/// (`docs/input.md` §3.8): the completion suggestions offered before
/// the scrollback scan, or the blacklist that filters both sources. Doubles
/// as its own wire shape (the registry ops receive it by name, like the
/// origin descriptor's serde forms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WordSetKind {
    Suggestions,
    Blacklist,
}

/// The identity a word-set contribution is scoped by: the same
/// `(isolate, origin)` pair that namespaces automations, so user scripts and
/// each package own separate contribution sets even where they share an
/// isolate.
pub type WordSetCreator = (IsolateId, Origin);

/// The longest registrable completion word, in `char`s. Completion inserts
/// single tokens; anything longer is not a word the user would Tab toward,
/// and the cap bounds what a registrant can make the UI hold and scan.
pub const MAX_WORD_CHARS: usize = 64;

/// How many words one creator may hold per set on one input. Suggestion sets
/// are hand-curated or data-driven (a player list, spell names); the cap
/// bounds a runaway or hostile registrant without pinching real use.
pub const MAX_WORDS_PER_CREATOR: usize = 512;

/// An `add()` batch was refused because it would push the creator's set past
/// [`MAX_WORDS_PER_CREATOR`]. Nothing was registered — the batch validates as
/// a whole before any word lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordSetCapacityError;

/// One word set's per-creator contributions. Creators are seated in the order
/// they first contributed and keep their seat through `clear()`/deletions, so
/// the merged order is stable across re-registration; within a seat, words
/// keep insertion order. Both levels are `IndexMap`s: insertion order is the
/// documented order, existing-key inserts keep their position (the in-place
/// casing update), and probes are O(1).
#[derive(Debug, Default)]
struct CreatorWordSets {
    /// Seat → the creator's words, each keyed by its case-insensitive
    /// identity (the Unicode-lowercased fold) mapping to the casing it was
    /// registered with (what completion inserts).
    seats: IndexMap<WordSetCreator, IndexMap<String, Arc<String>>>,
}

impl CreatorWordSets {
    /// Register a batch of words for `creator`. Word identity is
    /// case-insensitive: re-adding an already-registered word updates its
    /// stored casing in place (position kept). Atomic against the size cap —
    /// the resulting count is checked before any word lands, so a refused
    /// batch registers nothing. Returns whether anything changed.
    ///
    /// Seating happens here, on first contribution: lookups and deletes by a
    /// creator that never added anything mint no seat.
    fn add(
        &mut self,
        creator: &WordSetCreator,
        words: &[String],
    ) -> Result<bool, WordSetCapacityError> {
        if words.is_empty() {
            // An empty batch is a no-op, not a contribution: no seat.
            return Ok(false);
        }
        let folded: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
        let seated = self.seats.get(creator);
        let mut resulting = seated.map_or(0, IndexMap::len);
        let mut batch_new: HashSet<&str> = HashSet::new();
        for fold in &folded {
            if seated.is_none_or(|seat| !seat.contains_key(fold)) && batch_new.insert(fold) {
                resulting += 1;
            }
        }
        if resulting > MAX_WORDS_PER_CREATOR {
            return Err(WordSetCapacityError);
        }

        let seat = self.seats.entry(creator.clone()).or_default();
        let mut changed = false;
        for (word, fold) in words.iter().zip(folded) {
            match seat.entry(fold) {
                indexmap::map::Entry::Occupied(mut entry) => {
                    if entry.get().as_str() != word {
                        // In-place casing update: the entry keeps its position.
                        entry.insert(Arc::new(word.clone()));
                        changed = true;
                    }
                }
                indexmap::map::Entry::Vacant(entry) => {
                    entry.insert(Arc::new(word.clone()));
                    changed = true;
                }
            }
        }
        Ok(changed)
    }

    /// Remove `word` (case-insensitively) from `creator`'s contributions.
    fn delete(&mut self, creator: &WordSetCreator, word: &str) -> bool {
        self.seats.get_mut(creator).is_some_and(|seat| {
            // `shift_remove` keeps the remaining words' insertion order.
            seat.shift_remove(&word.to_lowercase()).is_some()
        })
    }

    fn has(&self, creator: &WordSetCreator, word: &str) -> bool {
        self.seats
            .get(creator)
            .is_some_and(|seat| seat.contains_key(&word.to_lowercase()))
    }

    /// `creator`'s own words, insertion order, registered casing.
    fn list(&self, creator: &WordSetCreator) -> Vec<String> {
        self.seats.get(creator).map_or_else(Vec::new, |seat| {
            seat.values().map(|word| word.as_str().to_string()).collect()
        })
    }

    /// Empty `creator`'s contributions (the seat itself survives, keeping the
    /// creator's merge position for a later re-add).
    fn clear(&mut self, creator: &WordSetCreator) -> bool {
        match self.seats.get_mut(creator) {
            Some(seat) if !seat.is_empty() => {
                seat.clear();
                true
            }
            _ => false,
        }
    }

    /// Drop every seat owned by `isolate` (a failed isolate load leaves its
    /// already-landed contributions with no live owner to clear them).
    /// Returns whether any dropped seat still held words.
    fn purge_isolate(&mut self, isolate: &IsolateId) -> bool {
        let mut dropped_words = false;
        self.seats.retain(|(seat_isolate, _), words| {
            if seat_isolate == isolate {
                dropped_words |= !words.is_empty();
                false
            } else {
                true
            }
        });
        dropped_words
    }

    fn is_empty(&self) -> bool {
        self.seats.values().all(IndexMap::is_empty)
    }
}

/// One input's suggestion set + blacklist, both per-creator.
#[derive(Debug, Default)]
struct InputWordSetEntry {
    suggestions: CreatorWordSets,
    blacklist: CreatorWordSets,
}

impl InputWordSetEntry {
    fn set(&self, kind: WordSetKind) -> &CreatorWordSets {
        match kind {
            WordSetKind::Suggestions => &self.suggestions,
            WordSetKind::Blacklist => &self.blacklist,
        }
    }

    fn set_mut(&mut self, kind: WordSetKind) -> &mut CreatorWordSets {
        match kind {
            WordSetKind::Suggestions => &mut self.suggestions,
            WordSetKind::Blacklist => &mut self.blacklist,
        }
    }
}

/// The merged, UI-facing view of one input's word sets: every creator's
/// suggestions in merge order, and the union blacklist (folded to lowercase —
/// blacklist filtering is case-insensitive and needs no display casing).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergedWordSets {
    /// Suggestions in merge order — creators in the order they first
    /// contributed, words in insertion order within a creator — deduplicated
    /// case-insensitively (the first registration of a word wins).
    pub suggestions: Vec<Arc<String>>,
    /// The union of every creator's blacklist, lowercase-folded.
    pub blacklist: HashSet<String>,
}

/// The authoritative per-creator completion word sets
/// (`docs/input.md` §3.8), keyed by input. The registry ops mutate
/// and read it synchronously on the session thread (reads are exact, no
/// staleness); the UI holds only the [merged](Self::merged) copy, re-pushed
/// whenever a mutation lands. Engine-scoped contents in a session-scoped
/// cell: a reload clears every contribution (words die with their creator's
/// isolate generation and reappear as the reloaded scripts re-register), like
/// hotkeys.
#[derive(Debug, Default)]
pub struct InputWordSets {
    inputs: HashMap<PaneKey, InputWordSetEntry>,
    /// Inputs whose mutations have not yet been pushed to the UI. The flag
    /// coalesces a burst of registry calls into one push: the first mutation
    /// queues the push action, later ones ride it, and the dispatch arm
    /// clears the flag when it sends the merged view.
    pending_push: HashSet<PaneKey>,
}

impl InputWordSets {
    /// Register `words` for `creator`; returns whether anything changed.
    /// Atomic against [`MAX_WORDS_PER_CREATOR`]: a batch that would overflow
    /// the creator's set registers nothing.
    ///
    /// # Errors
    /// [`WordSetCapacityError`] when the batch would push `creator`'s set
    /// past the cap.
    pub fn add(
        &mut self,
        key: PaneKey,
        kind: WordSetKind,
        creator: &WordSetCreator,
        words: &[String],
    ) -> Result<bool, WordSetCapacityError> {
        self.inputs
            .entry(key)
            .or_default()
            .set_mut(kind)
            .add(creator, words)
    }

    /// Remove one word from `creator`'s contributions; returns whether it was
    /// there.
    pub fn delete(
        &mut self,
        key: PaneKey,
        kind: WordSetKind,
        creator: &WordSetCreator,
        word: &str,
    ) -> bool {
        self.inputs
            .get_mut(&key)
            .is_some_and(|entry| entry.set_mut(kind).delete(creator, word))
    }

    #[must_use]
    pub fn has(&self, key: PaneKey, kind: WordSetKind, creator: &WordSetCreator, word: &str) -> bool {
        self.inputs
            .get(&key)
            .is_some_and(|entry| entry.set(kind).has(creator, word))
    }

    /// `creator`'s own words, insertion order, registered casing.
    #[must_use]
    pub fn list(&self, key: PaneKey, kind: WordSetKind, creator: &WordSetCreator) -> Vec<String> {
        self.inputs
            .get(&key)
            .map_or_else(Vec::new, |entry| entry.set(kind).list(creator))
    }

    /// Empty `creator`'s contributions; returns whether any word was dropped.
    pub fn clear(&mut self, key: PaneKey, kind: WordSetKind, creator: &WordSetCreator) -> bool {
        self.inputs
            .get_mut(&key)
            .is_some_and(|entry| entry.set_mut(kind).clear(creator))
    }

    /// Flag `key` as needing a UI push; returns `true` on the flip (the
    /// caller then queues exactly one push action).
    pub fn flag_push(&mut self, key: PaneKey) -> bool {
        self.pending_push.insert(key)
    }

    /// The dispatch arm's half of the coalescing: clear the pending flag as
    /// the merged view goes out.
    pub fn take_push(&mut self, key: PaneKey) {
        self.pending_push.remove(&key);
    }

    /// Build `key`'s merged view for the UI (see [`MergedWordSets`] for the
    /// documented order).
    #[must_use]
    pub fn merged(&self, key: PaneKey) -> MergedWordSets {
        let Some(entry) = self.inputs.get(&key) else {
            return MergedWordSets::default();
        };
        let mut seen = HashSet::new();
        let mut suggestions = Vec::new();
        for words in entry.suggestions.seats.values() {
            for (folded, display) in words {
                if seen.insert(folded.clone()) {
                    suggestions.push(display.clone());
                }
            }
        }
        let blacklist = entry
            .blacklist
            .seats
            .values()
            .flat_map(|words| words.keys().cloned())
            .collect();
        MergedWordSets {
            suggestions,
            blacklist,
        }
    }

    /// Drop every contribution — the reload teardown's half of the word-set
    /// lifecycle (contents are engine facts; the rebuilt engine's modules
    /// re-register theirs). Returns the inputs to resync, each flagged
    /// pending so the caller queues one push per input behind the rebuild:
    /// re-registered words go out merged, and an input nobody re-claimed goes
    /// out empty rather than lingering stale in the UI.
    ///
    /// The resync list is the inputs that held words **plus** any input whose
    /// pending flag was still up: its queued push action died with the old
    /// engine (the reload drops spawned actions), so leaving the flag standing
    /// with no action behind it would wedge the coalescing — `flag_push` would
    /// report "already queued" forever. Every returned key gets a fresh flag
    /// and a fresh push.
    pub fn reset_engine_state(&mut self) -> Vec<PaneKey> {
        let mut resync: Vec<PaneKey> = self
            .inputs
            .iter()
            .filter(|(_, entry)| {
                !entry.suggestions.is_empty() || !entry.blacklist.is_empty()
            })
            .map(|(key, _)| *key)
            .collect();
        for key in self.pending_push.drain() {
            if !resync.contains(&key) {
                resync.push(key);
            }
        }
        self.inputs.clear();
        for key in &resync {
            self.pending_push.insert(*key);
        }
        resync
    }

    /// Drop everything held for `key`'s input — contributions from every
    /// creator plus any pending push flag — because the pane closed. No UI
    /// push is owed: the pane's widget (input included) is being removed.
    /// Evicting the entry also keeps [`Self::reset_engine_state`] from naming
    /// the dead key, so a reload after a close never resyncs it.
    pub fn remove_input(&mut self, key: PaneKey) {
        self.inputs.remove(&key);
        self.pending_push.remove(&key);
    }

    /// Drop every contribution seated under `isolate` — the failed-isolate-
    /// load cleanup's half of the lifecycle. A sandboxed isolate that throws
    /// during load is discarded, but its top-level registrations already
    /// landed here synchronously; with no live owner they could never be
    /// cleared short of a full reload. Returns the inputs that lost words
    /// (the caller flags each for a UI push so the merged view drops them).
    pub fn purge_isolate(&mut self, isolate: &IsolateId) -> Vec<PaneKey> {
        self.inputs
            .iter_mut()
            .filter_map(|(key, entry)| {
                let suggestions = entry.suggestions.purge_isolate(isolate);
                let blacklist = entry.blacklist.purge_isolate(isolate);
                (suggestions || blacklist).then_some(*key)
            })
            .collect()
    }
}

/// The shared handle to the session's [`InputWordSets`], bound into every
/// isolate's registry ops and held by the runtime (whose push arm builds the
/// merged view and whose reload path resets the contents).
pub(crate) type SharedInputWordSets = Rc<RefCell<InputWordSets>>;

/// The address of one pane input's registered `onSubmit` handler
/// (`docs/input.md` §3.7): the creating isolate, the exact isolate
/// *instantiation* that minted the handler (the widget-token nonce pattern),
/// and the handler's slot in that isolate's `script_functions` registry. No
/// v8 handle lives here — the function stays in its isolate's registry, so
/// teardown order can never touch a disposed heap; dispatch re-checks the
/// instance nonce before resolving the slot, exactly like link callbacks.
#[derive(Debug, Clone)]
pub struct PaneInputCallback {
    pub isolate: IsolateId,
    pub instance: u64,
    pub function_id: FunctionId,
}

/// The registered `onSubmit` handler per pane input, keyed by the pane's
/// incarnation key. Session-scoped cell, engine-scoped contents: handlers
/// name functions of the engine that registered them, die with it on reload
/// ([`Self::reset_engine_state`]), and reappear when the reloaded script
/// re-splits its pane (the split's facade re-registers the handler). A
/// submission arriving with no live handler is dropped with a warning at
/// dispatch — the widget-callback lifecycle, applied to pane inputs.
#[derive(Debug, Default)]
pub struct PaneInputCallbacks {
    callbacks: HashMap<PaneKey, PaneInputCallback>,
}

impl PaneInputCallbacks {
    /// Register (or replace) the handler for `key`'s input. A re-split of a
    /// live pane re-registers, so the newest registration wins.
    pub fn register(&mut self, key: PaneKey, callback: PaneInputCallback) {
        self.callbacks.insert(key, callback);
    }

    /// The handler address for `key`'s input, if one is registered.
    #[must_use]
    pub fn get(&self, key: PaneKey) -> Option<PaneInputCallback> {
        self.callbacks.get(&key).cloned()
    }

    /// Drop `key`'s handler — the pane closed (`PaneKey`s are never reused,
    /// so a retired entry could never be hit again; this is hygiene).
    pub fn remove(&mut self, key: PaneKey) {
        self.callbacks.remove(&key);
    }

    /// Drop every handler — the reload teardown's half of the lifecycle
    /// (function ids index the dead engine's registries; the reloaded
    /// scripts re-register theirs beside their re-claiming splits).
    pub fn reset_engine_state(&mut self) {
        self.callbacks.clear();
    }

    /// Drop every handler registered by `isolate` — the failed-isolate-load
    /// cleanup, like the word sets': a sandboxed isolate that throws during
    /// load already landed its registrations synchronously, and they would
    /// otherwise dangle with no live owner.
    pub fn purge_isolate(&mut self, isolate: &IsolateId) {
        self.callbacks.retain(|_, cb| cb.isolate != *isolate);
    }
}

/// The shared handle to the session's [`PaneInputCallbacks`], bound into
/// every isolate's pane ops (the registration op writes it) and held by the
/// runtime (whose `PaneInputSubmit` dispatch arm resolves through it and
/// whose reload path resets it).
pub(crate) type SharedPaneInputCallbacks = Rc<RefCell<PaneInputCallbacks>>;

/// The single "input state for this pane died" purge, invoked from every
/// pane-close path — the own-session close op, a cross-session close, the
/// reload sweep, and the failed-load close of a sandboxed package's input
/// panes. Drops the pane's mirrored state and history, every creator's
/// word-set contributions, and its `onSubmit` registration. `PaneKey`s are
/// never reused, so none of it could ever be read again; without the purge
/// the maps grow permanently under split/close churn and the reload resync
/// re-pushes word sets for dead keys.
pub(crate) fn purge_pane_input_state(
    mirror: &SharedInputMirror,
    word_sets: &SharedInputWordSets,
    callbacks: &SharedPaneInputCallbacks,
    key: PaneKey,
) {
    mirror.borrow_mut().remove(key);
    word_sets.borrow_mut().remove_input(key);
    callbacks.borrow_mut().remove(key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::runtime::pane::MAIN_PANE_KEY;

    #[test]
    fn unreported_input_reads_as_the_default_snapshot() {
        let mirror = InputMirror::default();
        let snapshot = mirror.snapshot(MAIN_PANE_KEY);
        assert_eq!(snapshot.value.as_str(), "");
        assert_eq!(snapshot.cursor, 0);
        assert_eq!(snapshot.selection, None);
        assert!(!snapshot.focused);
        assert!(!snapshot.masked);
    }

    #[test]
    fn apply_distinguishes_a_first_report_from_a_reported_default() {
        let mut mirror = InputMirror::default();
        // The first-ever report replaces nothing — even when it equals the
        // default snapshot, "never reported" and "reported default" must
        // stay distinguishable (the dispatch arm's baseline/edge call).
        assert_eq!(mirror.apply(MAIN_PANE_KEY, InputSnapshot::default()), None);
        let typed = InputSnapshot {
            value: Arc::new("kill".to_string()),
            cursor: 4,
            ..InputSnapshot::default()
        };
        assert_eq!(
            mirror.apply(MAIN_PANE_KEY, typed.clone()),
            Some(InputSnapshot::default()),
            "later updates report the snapshot they replaced"
        );
        assert_eq!(mirror.apply(MAIN_PANE_KEY, InputSnapshot::default()), Some(typed));

        // The close purge returns the key to never-reported.
        mirror.remove(MAIN_PANE_KEY);
        assert_eq!(mirror.apply(MAIN_PANE_KEY, InputSnapshot::default()), None);
    }

    #[test]
    fn masked_updates_never_land_content_in_the_mirror() {
        let mut mirror = InputMirror::default();
        // A (hypothetically misbehaving) masked update that still carries
        // content must be suppressed on the session side.
        mirror.apply(
            MAIN_PANE_KEY,
            InputSnapshot {
                value: Arc::new("hunter2".to_string()),
                cursor: 7,
                selection: Some((0, 7)),
                focused: true,
                masked: true,
            },
        );
        let snapshot = mirror.snapshot(MAIN_PANE_KEY);
        assert_eq!(snapshot.value.as_str(), "");
        assert_eq!(snapshot.cursor, 0);
        assert_eq!(snapshot.selection, None);
        assert!(snapshot.focused);
        assert!(snapshot.masked);
    }

    #[test]
    fn submission_replace_composes_and_cancel_is_sticky() {
        let mut submission = InputSubmission::new(Arc::new("say hi".to_string()), 1);
        assert_eq!(submission.text(), "say hi");
        assert!(!submission.is_cancelled());

        // A later reader sees an earlier replacement.
        submission.replace("shout hi");
        assert_eq!(submission.text(), "shout hi");

        // Cancel wins over replace regardless of order.
        submission.cancel();
        submission.replace("whisper hi");
        assert!(submission.is_cancelled());
        assert_eq!(submission.into_text().as_str(), "whisper hi");
    }

    #[test]
    fn slot_generations_are_fresh_per_install_and_never_zero() {
        let mut slot = InputSubmissionSlot::default();
        assert_eq!(slot.live_generation(), 0, "empty slot reads as 0");

        slot.install(Arc::new("one".to_string()));
        let first = slot.live_generation();
        assert_ne!(first, 0, "a live submission never wears the 0 sentinel");
        assert_eq!(slot.live_mut().unwrap().generation(), first);

        assert_eq!(slot.take().unwrap().text(), "one");
        assert_eq!(slot.live_generation(), 0, "consumed slot reads as 0");

        slot.install(Arc::new("two".to_string()));
        assert_ne!(
            slot.live_generation(),
            first,
            "each install stamps a fresh generation"
        );
    }

    fn user_creator() -> WordSetCreator {
        (IsolateId::Main, Origin::User)
    }

    fn module_creator() -> WordSetCreator {
        (
            IsolateId::Main,
            Origin::Module {
                subpath: "combat/healer.ts".to_string(),
            },
        )
    }

    fn merged_suggestions(sets: &InputWordSets) -> Vec<String> {
        sets.merged(MAIN_PANE_KEY)
            .suggestions
            .iter()
            .map(|word| word.as_str().to_string())
            .collect()
    }

    fn add_words(sets: &mut InputWordSets, kind: WordSetKind, creator: &WordSetCreator, words: &[&str]) {
        let words: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();
        sets.add(MAIN_PANE_KEY, kind, creator, &words)
            .expect("test batch under the cap");
    }

    #[test]
    fn merged_suggestions_keep_creator_then_insertion_order() {
        let mut sets = InputWordSets::default();
        let first = user_creator();
        let second = module_creator();

        add_words(&mut sets, WordSetKind::Suggestions, &first, &["alpha", "gamma"]);
        add_words(&mut sets, WordSetKind::Suggestions, &second, &["beta"]);
        // A later add by the FIRST creator stays in the first creator's run.
        add_words(&mut sets, WordSetKind::Suggestions, &first, &["delta"]);

        assert_eq!(
            merged_suggestions(&sets),
            vec!["alpha", "gamma", "delta", "beta"],
            "creators merge in first-contribution order; words keep insertion order"
        );
    }

    #[test]
    fn merged_suggestions_dedupe_case_insensitively_first_wins() {
        let mut sets = InputWordSets::default();
        let first = user_creator();
        let second = module_creator();

        add_words(&mut sets, WordSetKind::Suggestions, &first, &["Fjord"]);
        add_words(&mut sets, WordSetKind::Suggestions, &second, &["fjord", "azure"]);

        assert_eq!(
            merged_suggestions(&sets),
            vec!["Fjord", "azure"],
            "a word two creators register appears once, first registration's casing"
        );
    }

    #[test]
    fn per_creator_isolation_for_clear_and_delete() {
        let mut sets = InputWordSets::default();
        let first = user_creator();
        let second = module_creator();

        add_words(&mut sets, WordSetKind::Suggestions, &first, &["alpha", "beta"]);
        add_words(&mut sets, WordSetKind::Suggestions, &second, &["gamma"]);

        // delete() touches only the caller's contributions...
        assert!(!sets.delete(MAIN_PANE_KEY, WordSetKind::Suggestions, &second, "alpha"));
        assert!(sets.has(MAIN_PANE_KEY, WordSetKind::Suggestions, &first, "alpha"));

        // ...and so does clear(); the merged view updates accordingly.
        assert!(sets.clear(MAIN_PANE_KEY, WordSetKind::Suggestions, &first));
        assert_eq!(sets.list(MAIN_PANE_KEY, WordSetKind::Suggestions, &first), Vec::<String>::new());
        assert_eq!(
            sets.list(MAIN_PANE_KEY, WordSetKind::Suggestions, &second),
            vec!["gamma"],
            "another creator's words survive a clear()"
        );
        assert_eq!(merged_suggestions(&sets), vec!["gamma"]);
    }

    #[test]
    fn word_identity_is_case_insensitive_and_casing_is_preserved() {
        let mut sets = InputWordSets::default();
        let creator = user_creator();

        add_words(&mut sets, WordSetKind::Suggestions, &creator, &["McCoy"]);
        assert!(sets.has(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator, "mccoy"));
        assert_eq!(
            sets.list(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator),
            vec!["McCoy"],
            "list() returns the registered casing"
        );

        // Re-adding under another casing updates in place (no duplicate, same position).
        add_words(&mut sets, WordSetKind::Suggestions, &creator, &["MCCOY", "zed"]);
        assert_eq!(
            sets.list(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator),
            vec!["MCCOY", "zed"]
        );

        // delete() matches case-insensitively too.
        assert!(sets.delete(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator, "mcCoy"));
        assert_eq!(
            sets.list(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator),
            vec!["zed"]
        );
    }

    #[test]
    fn merged_blacklist_is_the_folded_union() {
        let mut sets = InputWordSets::default();
        add_words(&mut sets, WordSetKind::Blacklist, &user_creator(), &["Bane"]);
        add_words(&mut sets, WordSetKind::Blacklist, &module_creator(), &["ogre"]);

        let merged = sets.merged(MAIN_PANE_KEY);
        assert!(merged.blacklist.contains("bane"), "blacklist entries fold to lowercase");
        assert!(merged.blacklist.contains("ogre"));
        assert_eq!(merged.blacklist.len(), 2);
    }

    #[test]
    fn push_flag_coalesces_and_reset_flags_populated_inputs() {
        let mut sets = InputWordSets::default();
        assert!(sets.flag_push(MAIN_PANE_KEY), "the first mutation queues a push");
        assert!(!sets.flag_push(MAIN_PANE_KEY), "later mutations ride the queued one");
        sets.take_push(MAIN_PANE_KEY);
        assert!(sets.flag_push(MAIN_PANE_KEY), "a delivered push re-arms the flag");
        sets.take_push(MAIN_PANE_KEY);

        add_words(&mut sets, WordSetKind::Suggestions, &user_creator(), &["alpha"]);
        let populated = sets.reset_engine_state();
        assert_eq!(populated, vec![MAIN_PANE_KEY], "reset names the inputs that held words");
        assert!(
            !sets.flag_push(MAIN_PANE_KEY),
            "reset leaves the populated input flagged for the post-rebuild push"
        );
        assert_eq!(merged_suggestions(&sets), Vec::<String>::new());
        // A second reset before the resync dispatched re-names the input: its
        // pending flag is still up and the second reload dropped the first
        // resync action, so it needs a fresh one.
        assert_eq!(sets.reset_engine_state(), vec![MAIN_PANE_KEY]);
        // Once the resync actually dispatches, a further reset has nothing.
        sets.take_push(MAIN_PANE_KEY);
        assert!(
            sets.reset_engine_state().is_empty(),
            "a delivered resync leaves nothing to flag"
        );
    }

    #[test]
    fn a_failed_delete_is_not_a_contribution_and_does_not_seat() {
        let mut sets = InputWordSets::default();
        let first = user_creator();
        let second = module_creator();

        // The input's entry exists (first contributed to the blacklist), so a
        // seat-minting delete would land in the live suggestions set.
        add_words(&mut sets, WordSetKind::Blacklist, &first, &["noise"]);
        assert!(!sets.delete(MAIN_PANE_KEY, WordSetKind::Suggestions, &second, "ghost"));

        // Seating happens at first add: first contributed suggestions before
        // second did, so first merges first — the failed delete bought
        // second no earlier seat.
        add_words(&mut sets, WordSetKind::Suggestions, &first, &["alpha"]);
        add_words(&mut sets, WordSetKind::Suggestions, &second, &["beta"]);
        assert_eq!(
            merged_suggestions(&sets),
            vec!["alpha", "beta"],
            "merge order follows first CONTRIBUTION, not first lookup"
        );
    }

    #[test]
    fn reset_covers_pending_flags_whose_push_died_with_the_reload() {
        let mut sets = InputWordSets::default();
        let creator = user_creator();

        // A delete that emptied the input flagged a push; the reload then
        // dropped the queued push action (spawned actions die with the
        // engine), leaving the flag with no action behind it.
        add_words(&mut sets, WordSetKind::Suggestions, &creator, &["gone"]);
        assert!(sets.delete(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator, "gone"));
        assert!(sets.flag_push(MAIN_PANE_KEY));

        // Reset must name that input for the post-rebuild resync even though
        // it holds no words...
        assert_eq!(sets.reset_engine_state(), vec![MAIN_PANE_KEY]);
        // ...and once the resync push dispatches, the coalescing is re-armed:
        // the next mutation queues a push instead of riding a ghost.
        sets.take_push(MAIN_PANE_KEY);
        assert!(
            sets.flag_push(MAIN_PANE_KEY),
            "a post-reload mutation must queue a fresh push"
        );
    }

    #[test]
    fn add_batches_are_atomic_against_the_per_creator_cap() {
        let mut sets = InputWordSets::default();
        let creator = user_creator();

        // Fill to one under the cap.
        let bulk: Vec<String> = (0..MAX_WORDS_PER_CREATOR - 1)
            .map(|i| format!("word{i}"))
            .collect();
        assert!(sets.add(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator, &bulk).unwrap());

        // A batch that would land two new words overflows — and lands NOTHING.
        let overflow = vec!["fits".to_string(), "spills".to_string()];
        assert_eq!(
            sets.add(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator, &overflow),
            Err(WordSetCapacityError)
        );
        assert!(!sets.has(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator, "fits"));

        // Exactly reaching the cap is fine, and re-adds (including casing
        // updates and batch-internal duplicates) cost no capacity.
        add_words(&mut sets, WordSetKind::Suggestions, &creator, &["last"]);
        add_words(&mut sets, WordSetKind::Suggestions, &creator, &["LAST", "word0", "last"]);
        assert!(sets.has(MAIN_PANE_KEY, WordSetKind::Suggestions, &creator, "last"));
        assert_eq!(
            sets.add(
                MAIN_PANE_KEY,
                WordSetKind::Suggestions,
                &creator,
                &["straw".to_string()]
            ),
            Err(WordSetCapacityError),
            "the set is full; one more new word is refused"
        );

        // The other set and other creators are unpinched.
        add_words(&mut sets, WordSetKind::Blacklist, &creator, &["blocked"]);
        add_words(&mut sets, WordSetKind::Suggestions, &module_creator(), &["fresh"]);
    }

    #[test]
    fn purge_isolate_drops_only_that_isolates_seats_and_names_affected_inputs() {
        let mut sets = InputWordSets::default();
        let main_creator = user_creator();
        let dead_creator = (
            IsolateId::Package {
                owner: "wbk".into(),
                name: "broken".into(),
                version: "1.0.0".into(),
            },
            Origin::Package {
                owner: "wbk".to_string(),
                name: "broken".to_string(),
                version: "1.0.0".to_string(),
            },
        );

        add_words(&mut sets, WordSetKind::Suggestions, &main_creator, &["alpha"]);
        add_words(&mut sets, WordSetKind::Suggestions, &dead_creator, &["zombie"]);
        add_words(&mut sets, WordSetKind::Blacklist, &dead_creator, &["shade"]);

        let affected = sets.purge_isolate(&dead_creator.0);
        assert_eq!(affected, vec![MAIN_PANE_KEY], "the purge names the inputs that lost words");

        let merged = sets.merged(MAIN_PANE_KEY);
        assert_eq!(
            merged.suggestions.iter().map(|w| w.as_str()).collect::<Vec<_>>(),
            vec!["alpha"],
            "the dead isolate's suggestions are gone, the survivor's stay"
        );
        assert!(merged.blacklist.is_empty(), "its blacklist words are gone too");

        // A second purge finds nothing: no push spam.
        assert!(sets.purge_isolate(&dead_creator.0).is_empty());
    }

    #[test]
    fn pane_input_callbacks_register_replace_and_purge() {
        let mut callbacks = PaneInputCallbacks::default();
        let key = MAIN_PANE_KEY; // Any key works; the registry is key-agnostic.
        assert!(callbacks.get(key).is_none());

        let main_cb = PaneInputCallback {
            isolate: IsolateId::Main,
            instance: 7,
            function_id: FunctionId(3),
        };
        callbacks.register(key, main_cb.clone());
        assert_eq!(callbacks.get(key).unwrap().instance, 7);

        // A re-registration (a re-claiming split) replaces the handler.
        callbacks.register(
            key,
            PaneInputCallback {
                instance: 8,
                ..main_cb.clone()
            },
        );
        assert_eq!(callbacks.get(key).unwrap().instance, 8);

        // remove() is close hygiene; reset drops everything (reload).
        callbacks.remove(key);
        assert!(callbacks.get(key).is_none());
        callbacks.register(key, main_cb.clone());
        callbacks.reset_engine_state();
        assert!(callbacks.get(key).is_none());

        // purge_isolate drops only the failed isolate's registrations.
        let dead = IsolateId::Package {
            owner: "wbk".into(),
            name: "broken".into(),
            version: "1.0.0".into(),
        };
        callbacks.register(key, main_cb.clone());
        let other = PaneKey::from_raw_for_tests(9);
        callbacks.register(
            other,
            PaneInputCallback {
                isolate: dead.clone(),
                instance: 11,
                function_id: FunctionId(0),
            },
        );
        callbacks.purge_isolate(&dead);
        assert!(callbacks.get(key).is_some(), "the survivor stays");
        assert!(callbacks.get(other).is_none(), "the dead isolate's entry is purged");
    }

    #[test]
    fn pane_close_purge_removes_every_map_entry_for_the_key() {
        let mirror: SharedInputMirror = Rc::new(RefCell::new(InputMirror::default()));
        let word_sets: SharedInputWordSets = Rc::new(RefCell::new(InputWordSets::default()));
        let callbacks: SharedPaneInputCallbacks =
            Rc::new(RefCell::new(PaneInputCallbacks::default()));
        let key = PaneKey::from_raw_for_tests(4);
        let other = PaneKey::from_raw_for_tests(5);

        mirror.borrow_mut().apply(
            key,
            InputSnapshot {
                value: Arc::new("draft".to_string()),
                ..InputSnapshot::default()
            },
        );
        mirror
            .borrow_mut()
            .apply_history(key, Arc::new(vec![Arc::new("cmd".to_string())]));
        mirror.borrow_mut().apply(other, InputSnapshot::default());
        word_sets
            .borrow_mut()
            .add(key, WordSetKind::Suggestions, &user_creator(), &["alpha".to_string()])
            .unwrap();
        word_sets.borrow_mut().flag_push(key);
        callbacks.borrow_mut().register(
            key,
            PaneInputCallback {
                isolate: IsolateId::Main,
                instance: 1,
                function_id: FunctionId(0),
            },
        );

        purge_pane_input_state(&mirror, &word_sets, &callbacks, key);

        assert_eq!(mirror.borrow().snapshot(key), InputSnapshot::default());
        assert!(mirror.borrow().history(key).is_empty());
        assert!(word_sets.borrow().merged(key).suggestions.is_empty());
        assert!(callbacks.borrow().get(key).is_none());
        // Another pane's state is untouched.
        assert!(mirror.borrow().states.contains_key(&other));
    }

    #[test]
    fn reset_after_a_pane_close_does_not_resync_the_dead_key() {
        let mut sets = InputWordSets::default();
        let creator = user_creator();
        let dead = PaneKey::from_raw_for_tests(3);

        // The pane's input held words AND had a push pending when it closed —
        // both halves the close purge must cover, or the reload resync names
        // a key the UI no longer knows.
        add_words(&mut sets, WordSetKind::Suggestions, &creator, &["alpha"]);
        sets.add(dead, WordSetKind::Suggestions, &creator, &["ghost".to_string()])
            .unwrap();
        sets.flag_push(dead);
        sets.remove_input(dead);

        assert_eq!(
            sets.reset_engine_state(),
            vec![MAIN_PANE_KEY],
            "only the live input is named for the post-rebuild resync"
        );
        // The coalescing for the dead key is not wedged either: were the key
        // somehow reused (it never is), a push would queue afresh.
        assert!(sets.flag_push(dead));
    }

    #[test]
    fn interest_flips_once_and_sticks() {
        let mut mirror = InputMirror::default();
        assert!(!mirror.interest());
        assert!(mirror.flag_interest(), "the first flag reports the flip");
        assert!(mirror.interest());
        assert!(!mirror.flag_interest(), "later flags are quiet");
        assert!(mirror.interest());
    }
}
