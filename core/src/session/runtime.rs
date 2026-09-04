use anyhow::Result;
use smudgy_cloud::Mapper;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{
    sync::{Arc, Condvar, Mutex, OnceLock, RwLock},
    task::Poll,
    thread::{self},
};

use tokio::{
    select,
    sync::{
        broadcast,
        mpsc::{UnboundedReceiver, UnboundedSender},
    },
};

mod matcher;
mod trigger;
#[cfg(not(feature = "bench-api"))]
use trigger::Manager;
// Expose the trigger engine to the `smudgy_bench` crate without widening the
// normal public API. The module itself stays private; the re-exported items
// (already `pub` at the item level) become reachable only under the feature.
// `MatchCapture` rides along so benches can unpack the captures carried by the
// `RuntimeAction::CallJavascriptFunction` deliveries the store flush queues.
#[cfg(feature = "bench-api")]
pub use trigger::{
    BenchActionQueue, Manager, MatchCapture, PushTriggerParams, SharedAutomationRegistry,
};
pub mod catalogue;
mod gmcp;
pub mod image_assets;
pub mod input;
pub mod line_operation;
mod message_bus;
mod msdp;
mod mssp;
pub mod pane;
mod remote_interop;
mod script_action;
mod script_engine;
mod store;

pub(crate) use catalogue::SharedCatalogue;
use catalogue::{CadenceDecision, CatalogueCadence, CatalogueEvent, RuntimeCatalogue};
use input::InputMirror;
pub(crate) use input::{
    SharedInputMirror, SharedInputSubmission, SharedInputWordSets, SharedPaneInputCallbacks,
};
use line_operation::LineOperation;
use message_bus::MessageBus;
pub(crate) use message_bus::SharedMessageBus;
use pane::{MAIN_PANE_KEY, PaneKey, PaneRegistry};
pub(crate) use remote_interop::SharedRemoteStateRegistry;

pub use script_action::ScriptAction;
pub use script_engine::layout_fold;
// The persistent package cache is public API: published versions are immutable, so the
// cache is a first-class local source of package content — session load serves from it
// cache-first, and out-of-session consumers (the update checker, cache-sourced package
// copies) read and warm the same store.
pub use script_engine::package_cache;
use script_engine::{ScriptEngine, ScriptEngineParams};
#[cfg(not(feature = "bench-api"))]
use store::SessionStore;
// Expose the session store's flush/fanout machinery to the `smudgy_bench` crate (the same
// pattern as the trigger re-export above): `SessionStore` plus every type its public method
// signatures carry. A watcher holds an `IsolateId` + a `FunctionId`, and delivery only
// *queues* `RuntimeAction::CallJavascriptFunction` values — nothing dereferences the id
// until an engine dispatches it — so a bench can drive set → flush → fanout with no script
// engine behind it. `FunctionId` itself is re-exported below; benches mint synthetic ids
// via `FunctionId::from_raw`.
#[cfg(feature = "bench-api")]
pub use script_engine::FunctionId;
pub(crate) use store::SharedSessionStore;
#[cfg(feature = "bench-api")]
pub use store::{
    BudgetExceeded, PathError, PlatformProducer, ProducerKey, SessionStore, SetOutcome,
    StoreBudgets, StorePath, Usage, WatchCadence,
};

use crate::get_smudgy_home;
use crate::models::settings::load_settings;
use crate::session::{
    HotkeyId, PackageProviderFactory, ScriptExtensionFactory, registry,
    ui_command::{UiCommandBus, UiCommandProducer},
};

use super::{
    SessionId, TaggedSessionEvent,
    connection::Connection,
    styled_line::{LineFragments, StyledLine},
};

use super::{BufferUpdate, SessionEvent};
use futures::{SinkExt, channel::mpsc::Sender};
mod action;
mod dispatch;
mod origin;

pub use action::RuntimeAction;
pub(crate) use action::{ActionQueue, ActionResult, RunAction};
pub use origin::{
    AutomationBody, AutomationDelta, AutomationEvent, AutomationKind, AutomationSummary, IsolateId,
    Origin, SingletonKey, SingletonOrigin, SingletonRegistry,
};

/// Cap on host-routed delivery recursion (event emit chains and session-store watch chains
/// alike — the store's watch dispatch deliberately shares the event system's depth cap): a
/// handler at this depth that would queue further deliveries has them dropped + logged rather
/// than looping forever.
pub(crate) const MAX_EVENT_DEPTH: u32 = 64;

/// How many of the most-recently-emitted lines the session
/// keeps a readable copy of, in [`Inner::recent_lines`]. This is a deliberate, documented
/// bound — `buffer.line(n)` reads (text + styles) and write-through resolve within this
/// window only; a line number older than the window reads as `undefined` from script. The
/// stored copies are the *same* `Arc<StyledLine>` already handed to the UI, so the window
/// costs one `Arc` clone + a `VecDeque` push/pop per emit (no data duplication, no silent
/// unlimited scrollback). 1000 covers any realistic "edit a line I just saw" use without
/// pinning the whole UI scrollback (10k) on the session thread.
const RECENT_LINES: usize = 1000;

/// Echo arms append display updates without flushing; the run loop delivers them
/// coalesced — at the drain point (before parking) and, during a long dispatch
/// cascade, whenever this many updates have accumulated. Bounds both the number of
/// UI events an echo storm produces (a 100k-line storm sends ~50 events instead of
/// 100k) and the size of any single event. Two updates per line (`Append` +
/// `EnsureNewLine`), so this is ~2k lines per batch.
const PENDING_UPDATE_FLUSH_THRESHOLD: usize = 4096;

/// The session-side bounded ring of recently-emitted lines. Each entry is the UI
/// line number paired with the same `Arc<StyledLine>` the UI holds. Shared (the same `Rc`)
/// into every isolate's ops so `op_smudgy_buffer_get_text`/`_styles` read it, and written by
/// [`Inner::record_emitted_line`] / the `buffer` write-through at emit time. Bounded to
/// [`RECENT_LINES`]; oldest entries are popped off the front.
pub(crate) type RecentLines = Rc<RefCell<VecDeque<(usize, Arc<StyledLine>)>>>;

/// The session's last-known mapper location backing `getCurrentLocation`. `setCurrentLocation`
/// is otherwise write-only (it fans out a UI marker), so the runtime mirrors the most recent
/// value here on the session thread; the same `Rc` is bound into every isolate's ops, which
/// read it back. It is a CURRENT-session read: the value lives on this thread, not
/// in the `Mapper` cache, and is not addressable cross-session. `None` until a location is set;
/// the inner `Option<i32>` is the room number (a location can name an area with no specific room).
pub(crate) type CurrentLocation = Rc<RefCell<Option<(smudgy_cloud::AreaId, Option<i32>)>>>;

/// The script-visible settings snapshot backing `getSettings()`. Seeded from disk at
/// construction and refreshed by [`RuntimeAction::ApplySettings`]; the same `Rc` is bound
/// into every isolate's ops, which read it back. Preserved across reload (cloned below) so a
/// settings value a script reads stays available through an engine rebuild.
pub(crate) type SettingsSnapshot = Rc<RefCell<crate::models::settings::ScriptSettings>>;

/// The session's data-only pane registry. It is lock-protected so same-server
/// runtimes can resolve foreign pane handles synchronously without moving any
/// V8 state across threads. UI mutations still travel through the owning
/// runtime's ordered action queue.
pub(crate) type SharedPaneRegistry = Arc<Mutex<PaneRegistry>>;

/// The pane-size mirror (`docs/panes.md` placement read-back), shared into
/// every isolate's ops like the input mirror: read synchronously by
/// `pane.size`, written by the `PaneDisplayChanged` dispatch arm, interest
/// flagged by the first read or a `pane:resize` subscription. Session-scoped
/// (survives reload) like the registry itself.
pub(crate) type SharedPaneSizeMirror = Arc<Mutex<pane::PaneSizeMirror>>;

/// Per-line suppression/routing state, cleared per line event. Transforms
/// (insert/replace/highlight/remove) stay in `pending_line_operations`;
/// gag/redirect/copy live here so transforms always apply to every sink —
/// `line.gag(); line.replace(...)` now replaces on the routed copies where
/// the old gag `LineOperation` short-circuited the pipeline.
#[derive(Debug, Default)]
pub struct LineRouting {
    /// Hide the line from the main buffer.
    pub gag: bool,
    /// Deliver to this pane *instead of* main (repeated calls: last wins).
    pub redirect: Option<PaneKey>,
    /// Additionally deliver to these panes (deduplicated at routing time).
    pub copies: Vec<PaneKey>,
}

impl LineRouting {
    fn take(&mut self) -> LineRouting {
        std::mem::take(self)
    }

    fn is_default(&self) -> bool {
        !self.gag && self.redirect.is_none() && self.copies.is_empty()
    }
}

/// The routing state cell, shared into every isolate's ops beside
/// `pending_line_operations`.
pub(crate) type SharedLineRouting = Rc<RefCell<LineRouting>>;

/// Fixed-width mask substituted for each redacted secret in echoed/logged output.
/// Fixed width so it doesn't leak the secret's length.
const REDACTION_MASK: &str = "********";

/// What the main pane currently contains from a fragmented inbound logical line.
///
/// `main_open_line` describes the terminal's physical tail. This state describes the
/// server line that can span that tail, committed rows, and local output. The distinction
/// lets a completion append only unseen server text after local output commits its prefix.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum MainPrefixDisposition {
    /// No fragmented inbound line is active.
    #[default]
    None,
    /// Main contains every partial fragment in one open row that can still be replaced.
    Replaceable,
    /// Main contains every partial fragment, but local output committed at least one row.
    Committed,
    /// Main has an immutable source prefix, followed by at least one undisplayed fragment.
    /// Later partials stay deferred so completion can restore the remaining source in order.
    CommittedGap,
    /// Main does not contain every partial fragment, so completion needs the assembled whole.
    Incomplete,
}

impl MainPrefixDisposition {
    fn note_visible_partial(&mut self) {
        if matches!(self, Self::None) {
            *self = Self::Replaceable;
        }
    }

    fn note_hidden_partial(&mut self) {
        *self = match self {
            Self::Committed | Self::CommittedGap => Self::CommittedGap,
            Self::None | Self::Replaceable | Self::Incomplete => Self::Incomplete,
        };
    }

    fn defers_partial_main(self) -> bool {
        matches!(self, Self::CommittedGap | Self::Incomplete)
    }
}

/// Replaces every (non-empty) literal `redactions` substring in `text` with
/// [`REDACTION_MASK`]. Used to keep secrets (e.g. a substituted `$PASSWORD`) out of
/// the client's view and the session log while still sending them to the server.
fn redact(text: &str, redactions: &[String]) -> String {
    let mut out = text.to_string();
    for secret in redactions {
        if !secret.is_empty() {
            out = out.replace(secret.as_str(), REDACTION_MASK);
        }
    }
    out
}

/// Stop accepting external work and terminally fail any tooltip resolutions
/// already queued behind shutdown. Once the receiver is closed, concurrent
/// forwards take the send-failure path instead.
fn close_runtime_action_queue(receiver: &mut UnboundedReceiver<RuntimeAction>) {
    receiver.close();
    while let Ok(action) = receiver.try_recv() {
        if let RuntimeAction::ResolveLinkTooltip { state, .. } = action {
            state.resolve(None);
        }
    }
}

#[cfg(test)]
mod runtime_helper_tests {
    use std::sync::Arc;

    use super::{IsolateId, RuntimeAction, close_runtime_action_queue, redact};
    use crate::session::SessionId;
    use crate::session::styled_line::LinkTooltipState;

    #[test]
    fn masks_each_secret_but_leaves_other_text() {
        let out = redact("connect Gandalf s3cret", &["s3cret".to_string()]);
        assert_eq!(out, "connect Gandalf ********");
        assert!(!out.contains("s3cret"));
    }

    #[test]
    fn empty_or_no_secrets_are_left_untouched() {
        // An empty redaction string must never panic or mask everything.
        assert_eq!(redact("hello", &[String::new()]), "hello");
        assert_eq!(redact("hello", &[]), "hello");
    }

    #[test]
    fn shutdown_fails_tooltips_already_queued_behind_it() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::new(LinkTooltipState::default());
        assert!(state.begin_request());
        tx.send(RuntimeAction::ResolveLinkTooltip {
            session: SessionId::from(7),
            isolate: IsolateId::Main,
            instance: 1,
            id: 2,
            token: Arc::new(crate::session::styled_line::LinkToken::default()),
            state: Arc::clone(&state),
        })
        .expect("open runtime queue");

        close_runtime_action_queue(&mut rx);

        assert!(!state.is_loading());
        assert!(state.text().is_none());
        assert!(tx.send(RuntimeAction::Noop).is_err());
    }
}

/// Rewind a provisional open line off the end of the log file: the open line
/// (a resting prompt) was written to disk on a flush tick for crash
/// durability, and a committed write now needs to replace it. Truncating back
/// to the committed length and re-seeking there lets the completed or
/// retracted line be rewritten without duplication. The `BufWriter` is flushed
/// first so the underlying `File` cursor is authoritative before the seek.
fn rewind_provisional_open_line(
    log_file: &mut BufWriter<File>,
    committed_len: u64,
) -> std::io::Result<()> {
    log_file.flush()?;
    let file = log_file.get_mut();
    file.set_len(committed_len)?;
    file.seek(SeekFrom::Start(committed_len))?;
    Ok(())
}

pub struct Runtime {
    pub session_id: SessionId,
    pub server_name: Arc<String>,
    pub profile_name: Arc<String>,
    pub profile_subtext: Arc<String>,
    pub ui_tx: Sender<TaggedSessionEvent>,
    pub tx: UnboundedSender<RuntimeAction>,
    /// Per-session automation broadcast; the automations window subscribes via
    /// [`Runtime::subscribe_automations`] to render script-created aliases/triggers.
    pub automation_tx: broadcast::Sender<AutomationEvent>,
    /// Per-session runtime-catalogue broadcast (`docs/interop.md` §10); the
    /// automations window's store tab subscribes via [`Runtime::subscribe_catalogue`].
    pub catalogue_tx: broadcast::Sender<CatalogueEvent>,
    /// Transport state read by script-visible `Session.connected` handles.
    pub connected: Arc<std::sync::atomic::AtomicBool>,
    /// Latest committed immutable store roots for same-server directed reads.
    pub published_store: Arc<RwLock<store::PublishedStore>>,
    /// Cross-session, data-only script surfaces. These contain no V8 handles;
    /// foreign callers use them for exact synchronous resolution/readback and
    /// route effects through `tx` below.
    pub(crate) pane_registry: SharedPaneRegistry,
    pub(crate) input_mirror: SharedInputMirror,
    pub(crate) pane_size_mirror: SharedPaneSizeMirror,
    pub(crate) input_word_sets: SharedInputWordSets,
    pub(crate) pane_input_callbacks: SharedPaneInputCallbacks,
    /// The worker waits on this one-shot gate until the fully-constructed
    /// runtime has been inserted into the global session registry.
    start_tx: Mutex<Option<std::sync::mpsc::Sender<std::sync::mpsc::Receiver<()>>>>,
}

/// Second phase of runtime publication. The worker has received this permit's
/// paired receiver but cannot construct scripts until registration commits it.
pub(crate) struct RuntimeStartPermit(std::sync::mpsc::Sender<()>);

/// The exact runtime thread could not be created after its session id was
/// reserved. The reservation remains as a result-bearing `SpawnFailed`
/// tombstone for the lifecycle owner to consume.
#[derive(Debug, thiserror::Error)]
#[error("failed to spawn runtime thread for session {session_id}: {source}")]
pub struct RuntimeThreadSpawnError {
    pub session_id: SessionId,
    #[source]
    pub source: std::io::Error,
}

/// Primary reason a spawned runtime could not be transactionally published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeThreadPublicationFailure {
    /// The live session map already contained this id.
    DuplicateSession,
    /// A contained panic interrupted post-insert publication work.
    PublicationUnwound,
    /// The worker disappeared before reaching the second barrier.
    StartGateClosed,
    /// A `created` send unwound; attempted targets received tombstones.
    CreatedBroadcastUnwound,
    /// The worker disappeared after `created` but before script admission.
    CommitGateClosed,
}

/// Publication failure paired with the result of its synchronous cleanup
/// attempt. Only a matching clean join proves worker rollback; absence or a
/// failed join remains an unclean report.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "runtime publication failed for session {session_id}: {failure:?}; exact worker cleanup: {cleanup:?}"
)]
pub struct RuntimeThreadPublicationError {
    session_id: SessionId,
    failure: RuntimeThreadPublicationFailure,
    cleanup: RuntimeThreadJoinOutcome,
}

impl RuntimeThreadPublicationError {
    pub(crate) const fn new(
        session_id: SessionId,
        failure: RuntimeThreadPublicationFailure,
        cleanup: RuntimeThreadJoinOutcome,
    ) -> Self {
        Self {
            session_id,
            failure,
            cleanup,
        }
    }

    /// Session whose publication failed.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Primary publication failure, independent of worker cleanup.
    #[must_use]
    pub const fn failure(&self) -> RuntimeThreadPublicationFailure {
        self.failure
    }

    /// Read-only result of the one-shot worker cleanup attempt.
    #[must_use]
    pub const fn cleanup(&self) -> RuntimeThreadJoinOutcome {
        self.cleanup
    }

    /// Consume the publication error and transfer its one-shot cleanup report.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        SessionId,
        RuntimeThreadPublicationFailure,
        RuntimeThreadJoinOutcome,
    ) {
        (self.session_id, self.failure, self.cleanup)
    }
}

/// Exact, non-panicking result of consuming one runtime-thread join authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeThreadJoinOutcome {
    /// The runtime thread returned normally.
    Clean { session_id: SessionId },
    /// The runtime thread panicked. Its opaque payload is deliberately leaked
    /// so a hostile payload destructor cannot make the join API panic.
    Panicked { session_id: SessionId },
    /// The OS thread could not be spawned after the session id was reserved.
    SpawnFailed { session_id: SessionId },
    /// No join authority is available for this id. It was never spawned, or a
    /// concurrent/earlier exact or all-session join already consumed it.
    /// This is an absence report, not proof that a runtime shut down.
    NotTrackedOrAlreadyJoined { session_id: SessionId },
}

impl RuntimeThreadJoinOutcome {
    /// Session identity whose join was requested or completed.
    #[must_use]
    pub const fn session_id(self) -> SessionId {
        match self {
            Self::Clean { session_id }
            | Self::Panicked { session_id }
            | Self::SpawnFailed { session_id }
            | Self::NotTrackedOrAlreadyJoined { session_id } => session_id,
        }
    }

    /// Whether this outcome proves a normal runtime-thread return.
    #[must_use]
    pub const fn is_clean(self) -> bool {
        matches!(self, Self::Clean { .. })
    }
}

enum RuntimeThreadEntry {
    /// The id is linearized before `Builder::spawn`; exact and drain-all joins
    /// wait until it becomes `Running` or `SpawnFailed`.
    Reserved,
    /// The one result-bearing join authority for this exact session id.
    Running(JoinHandle<()>),
    /// One caller owns the OS join while every other observer shares its
    /// completion. No observer may report missing/already-joined until this
    /// proof is published.
    Joining(Arc<RuntimeThreadJoinCompletion>),
    /// Publication failed after reservation. Retained until a join observes it.
    SpawnFailed,
}

#[derive(Default)]
struct RuntimeThreadJoinCompletionState {
    outcome: Option<RuntimeThreadJoinOutcome>,
    #[cfg(test)]
    waiters: usize,
}

#[derive(Default)]
struct RuntimeThreadJoinCompletion {
    state: Mutex<RuntimeThreadJoinCompletionState>,
    ready: Condvar,
}

impl RuntimeThreadJoinCompletion {
    fn publish(&self, outcome: RuntimeThreadJoinOutcome) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(state.outcome.is_none());
        state.outcome = Some(outcome);
        self.ready.notify_all();
    }

    fn wait(&self) -> RuntimeThreadJoinOutcome {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(test)]
        {
            state.waiters += 1;
            self.ready.notify_all();
        }
        while state.outcome.is_none() {
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        #[cfg(test)]
        {
            state.waiters -= 1;
        }
        state
            .outcome
            .expect("runtime-thread completion published an outcome")
    }

    #[cfg(test)]
    fn wait_for_waiters(&self, expected: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.waiters != expected {
            let (next, timeout) = self
                .ready
                .wait_timeout(state, Duration::from_secs(2))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(!timeout.timed_out(), "join observer did not begin waiting");
            state = next;
        }
    }
}

#[derive(Default)]
struct RuntimeThreadRegistryState {
    entries: HashMap<SessionId, RuntimeThreadEntry>,
    /// Temporarily stops new reservations while drain-all resolves every
    /// already-reserved id into a result-bearing state.
    drainers: usize,
    #[cfg(test)]
    reservation_waiters: usize,
}

#[derive(Default)]
struct RuntimeThreadRegistry {
    state: Mutex<RuntimeThreadRegistryState>,
    changed: Condvar,
    #[cfg(test)]
    drain_snapshot_hook: Mutex<Option<Arc<TestDrainSnapshotHook>>>,
}

#[cfg(test)]
struct TestDrainSnapshotHook {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Arc<std::sync::Barrier>,
    armed: std::sync::atomic::AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeThreadReservationError {
    AlreadyTracked,
}

struct RuntimeThreadReservation {
    registry: Arc<RuntimeThreadRegistry>,
    session_id: SessionId,
    resolved: bool,
}

enum RuntimeThreadJoinClaim {
    Owner {
        session_id: SessionId,
        handle: JoinHandle<()>,
        completion: Arc<RuntimeThreadJoinCompletion>,
    },
    SpawnFailedOwner {
        session_id: SessionId,
        completion: Arc<RuntimeThreadJoinCompletion>,
    },
    Observer {
        session_id: SessionId,
        completion: Arc<RuntimeThreadJoinCompletion>,
    },
}

impl RuntimeThreadJoinClaim {
    const fn session_id(&self) -> SessionId {
        match self {
            Self::Owner { session_id, .. }
            | Self::SpawnFailedOwner { session_id, .. }
            | Self::Observer { session_id, .. } => *session_id,
        }
    }
}

impl RuntimeThreadRegistry {
    fn reserve(
        self: &Arc<Self>,
        session_id: SessionId,
    ) -> Result<RuntimeThreadReservation, RuntimeThreadReservationError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.drainers != 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if state.entries.contains_key(&session_id) {
            return Err(RuntimeThreadReservationError::AlreadyTracked);
        }
        state
            .entries
            .insert(session_id, RuntimeThreadEntry::Reserved);
        drop(state);
        Ok(RuntimeThreadReservation {
            registry: Arc::clone(self),
            session_id,
            resolved: false,
        })
    }

    fn join_exact(&self, session_id: SessionId) -> RuntimeThreadJoinOutcome {
        let claim = loop {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.entries.get_mut(&session_id) {
                Some(RuntimeThreadEntry::Reserved) => {
                    #[cfg(test)]
                    {
                        state.reservation_waiters += 1;
                        self.changed.notify_all();
                    }
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    #[cfg(test)]
                    {
                        state.reservation_waiters -= 1;
                    }
                    drop(state);
                }
                Some(entry @ RuntimeThreadEntry::Running(_)) => {
                    let completion = Arc::new(RuntimeThreadJoinCompletion::default());
                    let RuntimeThreadEntry::Running(handle) = std::mem::replace(
                        entry,
                        RuntimeThreadEntry::Joining(Arc::clone(&completion)),
                    ) else {
                        unreachable!("matched running runtime-thread entry")
                    };
                    self.changed.notify_all();
                    break RuntimeThreadJoinClaim::Owner {
                        session_id,
                        handle,
                        completion,
                    };
                }
                Some(RuntimeThreadEntry::Joining(completion)) => {
                    break RuntimeThreadJoinClaim::Observer {
                        session_id,
                        completion: Arc::clone(completion),
                    };
                }
                Some(entry @ RuntimeThreadEntry::SpawnFailed) => {
                    let completion = Arc::new(RuntimeThreadJoinCompletion::default());
                    *entry = RuntimeThreadEntry::Joining(Arc::clone(&completion));
                    self.changed.notify_all();
                    break RuntimeThreadJoinClaim::SpawnFailedOwner {
                        session_id,
                        completion,
                    };
                }
                None => {
                    return RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined { session_id };
                }
            }
        };
        self.resolve_claim(claim)
    }

    fn join_all(&self) -> Vec<RuntimeThreadJoinOutcome> {
        let mut claims = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.drainers = state.drainers.saturating_add(1);
            self.changed.notify_all();
            while state
                .entries
                .values()
                .any(|entry| matches!(entry, RuntimeThreadEntry::Reserved))
            {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }

            let mut claims = Vec::new();
            for (session_id, entry) in &mut state.entries {
                match entry {
                    RuntimeThreadEntry::Running(_) => {
                        let completion = Arc::new(RuntimeThreadJoinCompletion::default());
                        let RuntimeThreadEntry::Running(handle) = std::mem::replace(
                            entry,
                            RuntimeThreadEntry::Joining(Arc::clone(&completion)),
                        ) else {
                            unreachable!("matched running runtime-thread entry")
                        };
                        claims.push(RuntimeThreadJoinClaim::Owner {
                            session_id: *session_id,
                            handle,
                            completion,
                        });
                    }
                    RuntimeThreadEntry::Joining(completion) => {
                        claims.push(RuntimeThreadJoinClaim::Observer {
                            session_id: *session_id,
                            completion: Arc::clone(completion),
                        });
                    }
                    RuntimeThreadEntry::SpawnFailed => {
                        let completion = Arc::new(RuntimeThreadJoinCompletion::default());
                        *entry = RuntimeThreadEntry::Joining(Arc::clone(&completion));
                        claims.push(RuntimeThreadJoinClaim::SpawnFailedOwner {
                            session_id: *session_id,
                            completion,
                        });
                    }
                    RuntimeThreadEntry::Reserved => {}
                }
            }
            // The snapshot is now stable through Joining completions. Release
            // the map lock before finishing this drain admission so concurrent
            // drainers compose without blocking one another's observation.
            drop(state);
            #[cfg(test)]
            self.pause_after_drain_snapshot();
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.drainers = state.drainers.saturating_sub(1);
            if state.drainers == 0 {
                self.changed.notify_all();
            }
            claims
        };

        claims.sort_by_key(RuntimeThreadJoinClaim::session_id);
        let mut outcomes = claims
            .drain(..)
            .map(|claim| self.resolve_claim(claim))
            .collect::<Vec<_>>();
        outcomes.sort_by_key(|outcome| outcome.session_id());
        outcomes
    }

    #[cfg(test)]
    fn pause_after_drain_snapshot(&self) {
        let hook = self
            .drain_snapshot_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook
            && hook.armed.swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            hook.entered.send(()).unwrap();
            hook.release.wait();
        }
    }

    fn resolve_claim(&self, claim: RuntimeThreadJoinClaim) -> RuntimeThreadJoinOutcome {
        match claim {
            RuntimeThreadJoinClaim::Owner {
                session_id,
                handle,
                completion,
            } => {
                let outcome = join_runtime_handle(session_id, handle);
                completion.publish(outcome);
                self.remove_joining(session_id, &completion);
                outcome
            }
            RuntimeThreadJoinClaim::SpawnFailedOwner {
                session_id,
                completion,
            } => {
                let outcome = RuntimeThreadJoinOutcome::SpawnFailed { session_id };
                completion.publish(outcome);
                self.remove_joining(session_id, &completion);
                outcome
            }
            RuntimeThreadJoinClaim::Observer { completion, .. } => completion.wait(),
        }
    }

    fn remove_joining(&self, session_id: SessionId, completion: &Arc<RuntimeThreadJoinCompletion>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.entries.get(&session_id).is_some_and(|entry| {
            matches!(entry, RuntimeThreadEntry::Joining(current) if Arc::ptr_eq(current, completion))
        }) {
            state.entries.remove(&session_id);
            self.changed.notify_all();
        }
    }

    #[cfg(test)]
    fn contains(&self, session_id: SessionId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .contains_key(&session_id)
    }
}

impl RuntimeThreadReservation {
    fn publish(mut self, handle: JoinHandle<()>) {
        let mut state = self
            .registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry @ RuntimeThreadEntry::Reserved) = state.entries.get_mut(&self.session_id)
        else {
            drop(state);
            let _ = join_runtime_handle(self.session_id, handle);
            panic!("runtime-thread reservation disappeared before publication");
        };
        *entry = RuntimeThreadEntry::Running(handle);
        self.resolved = true;
        self.registry.changed.notify_all();
    }

    fn fail(mut self) {
        self.publish_failure();
    }

    fn publish_failure(&mut self) {
        if self.resolved {
            return;
        }
        let mut state = self
            .registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry @ RuntimeThreadEntry::Reserved) = state.entries.get_mut(&self.session_id)
        {
            *entry = RuntimeThreadEntry::SpawnFailed;
        }
        self.resolved = true;
        self.registry.changed.notify_all();
    }
}

impl Drop for RuntimeThreadReservation {
    fn drop(&mut self) {
        self.publish_failure();
    }
}

fn join_runtime_handle(session_id: SessionId, handle: JoinHandle<()>) -> RuntimeThreadJoinOutcome {
    match handle.join() {
        Ok(()) => RuntimeThreadJoinOutcome::Clean { session_id },
        Err(payload) => {
            // Panic payloads are arbitrary user types; dropping one may panic.
            std::mem::forget(payload);
            RuntimeThreadJoinOutcome::Panicked { session_id }
        }
    }
}

static RUNTIME_THREADS: OnceLock<Arc<RuntimeThreadRegistry>> = OnceLock::new();

fn runtime_threads() -> Arc<RuntimeThreadRegistry> {
    Arc::clone(RUNTIME_THREADS.get_or_init(|| Arc::new(RuntimeThreadRegistry::default())))
}

/// Consume and join the exact runtime thread tracked for `session_id`.
///
/// The caller must request that exact runtime's shutdown before joining; a live
/// runtime is allowed to keep this call blocked. A
/// [`RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined`] result is not
/// shutdown proof and must not authorize dependent resource retirement.
///
/// A reservation still being published is awaited. The map mutex is never
/// held while the OS thread is joined, and thread panics are returned rather
/// than propagated.
#[must_use]
pub fn join_runtime_thread(session_id: SessionId) -> RuntimeThreadJoinOutcome {
    runtime_threads().join_exact(session_id)
}

/// Drain and join every runtime thread whose reservation preceded this call.
///
/// Callers must first request shutdown for every runtime in their lifecycle
/// domain; live runtimes are allowed to keep this call blocked.
///
/// Drain-all first prevents new reservations and resolves every in-flight
/// reservation to `Running` or `SpawnFailed`. It then reopens registration and
/// releases the map lock before joining any thread. Outcomes are sorted by
/// session id and no thread panic is propagated.
#[must_use]
pub fn join_all_runtime_threads() -> Vec<RuntimeThreadJoinOutcome> {
    runtime_threads().join_all()
}

/// Compatibility join-all entry point retaining the former fail-fast behavior.
///
/// New lifecycle coordination should use [`join_all_runtime_threads`] and
/// inspect every returned outcome. Existing callers that have not migrated
/// still panic if any runtime failed, rather than silently treating a crash as
/// clean shutdown.
///
/// # Panics
///
/// Panics after all tracked threads have been joined if any outcome is not
/// [`RuntimeThreadJoinOutcome::Clean`].
pub fn join_runtime_threads() {
    let outcomes = join_all_runtime_threads();
    if let Some(failure) = outcomes.into_iter().find(|outcome| !outcome.is_clean()) {
        panic!("runtime thread did not stop cleanly: {failure:?}");
    }
}

#[cfg(test)]
pub(super) fn runtime_thread_is_tracked(session_id: SessionId) -> bool {
    runtime_threads().contains(session_id)
}

#[cfg(test)]
mod runtime_thread_registry_tests {
    use super::*;
    use std::panic::panic_any;
    use std::sync::{Barrier, mpsc};
    use std::time::Duration;

    fn wait_for_state(
        registry: &RuntimeThreadRegistry,
        predicate: impl Fn(&RuntimeThreadRegistryState) -> bool,
    ) {
        let mut state = registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !predicate(&state) {
            let (next, timeout) = registry
                .changed
                .wait_timeout(state, Duration::from_secs(2))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                !timeout.timed_out(),
                "runtime-thread registry state stalled"
            );
            state = next;
        }
    }

    fn publish(
        registry: &Arc<RuntimeThreadRegistry>,
        session_id: SessionId,
        body: impl FnOnce() + Send + 'static,
    ) {
        let reservation = registry.reserve(session_id).unwrap();
        reservation.publish(thread::spawn(body));
    }

    fn wait_for_joining(
        registry: &RuntimeThreadRegistry,
        session_id: SessionId,
    ) -> Arc<RuntimeThreadJoinCompletion> {
        wait_for_state(registry, |state| {
            matches!(
                state.entries.get(&session_id),
                Some(RuntimeThreadEntry::Joining(_))
            )
        });
        let state = registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(RuntimeThreadEntry::Joining(completion)) = state.entries.get(&session_id) else {
            panic!("runtime thread stopped joining before its completion was observed")
        };
        Arc::clone(completion)
    }

    #[test]
    fn exact_join_reports_clean_panic_missing_and_duplicate_without_panicking() {
        struct HostilePanicPayload;
        impl Drop for HostilePanicPayload {
            fn drop(&mut self) {
                panic!("hostile panic-payload destructor");
            }
        }

        let registry = Arc::new(RuntimeThreadRegistry::default());
        let clean_id = SessionId::from(98_001);
        publish(&registry, clean_id, || {});
        assert_eq!(
            registry.join_exact(clean_id),
            RuntimeThreadJoinOutcome::Clean {
                session_id: clean_id
            }
        );
        assert_eq!(
            registry.join_exact(clean_id),
            RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined {
                session_id: clean_id
            }
        );

        let panic_id = SessionId::from(98_002);
        publish(&registry, panic_id, || panic_any(HostilePanicPayload));
        assert_eq!(
            registry.join_exact(panic_id),
            RuntimeThreadJoinOutcome::Panicked {
                session_id: panic_id
            }
        );

        let missing_id = SessionId::from(98_003);
        assert_eq!(
            registry.join_exact(missing_id),
            RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined {
                session_id: missing_id
            }
        );
    }

    #[test]
    fn exact_join_waits_for_reserved_handle_publication() {
        let registry = Arc::new(RuntimeThreadRegistry::default());
        let session_id = SessionId::from(98_004);
        let reservation = registry.reserve(session_id).unwrap();
        let join_registry = Arc::clone(&registry);
        let join = thread::spawn(move || join_registry.join_exact(session_id));
        wait_for_state(&registry, |state| state.reservation_waiters == 1);
        assert!(!join.is_finished());

        reservation.publish(thread::spawn(|| {}));
        assert_eq!(
            join.join().unwrap(),
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
    }

    #[test]
    fn concurrent_exact_joins_share_the_in_progress_proof() {
        let registry = Arc::new(RuntimeThreadRegistry::default());
        let session_id = SessionId::from(98_010);
        let release = Arc::new(Barrier::new(2));
        let worker_release = Arc::clone(&release);
        publish(&registry, session_id, move || {
            worker_release.wait();
        });

        let first_registry = Arc::clone(&registry);
        let first = thread::spawn(move || first_registry.join_exact(session_id));
        let completion = wait_for_joining(&registry, session_id);
        let second_registry = Arc::clone(&registry);
        let second = thread::spawn(move || second_registry.join_exact(session_id));
        completion.wait_for_waiters(1);
        assert!(!first.is_finished());
        assert!(!second.is_finished());

        release.wait();
        let expected = RuntimeThreadJoinOutcome::Clean { session_id };
        assert_eq!(first.join().unwrap(), expected);
        assert_eq!(second.join().unwrap(), expected);
        assert_eq!(
            registry.join_exact(session_id),
            RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined { session_id }
        );
    }

    #[test]
    fn exact_join_and_join_all_share_the_in_progress_proof() {
        let registry = Arc::new(RuntimeThreadRegistry::default());
        let session_id = SessionId::from(98_011);
        let release = Arc::new(Barrier::new(2));
        let worker_release = Arc::clone(&release);
        publish(&registry, session_id, move || {
            worker_release.wait();
        });

        let exact_registry = Arc::clone(&registry);
        let exact = thread::spawn(move || exact_registry.join_exact(session_id));
        let completion = wait_for_joining(&registry, session_id);
        let all_registry = Arc::clone(&registry);
        let all = thread::spawn(move || all_registry.join_all());
        completion.wait_for_waiters(1);
        assert!(!exact.is_finished());
        assert!(!all.is_finished());

        release.wait();
        let expected = RuntimeThreadJoinOutcome::Clean { session_id };
        assert_eq!(exact.join().unwrap(), expected);
        assert_eq!(all.join().unwrap(), vec![expected]);
    }

    #[test]
    fn concurrent_join_all_calls_do_not_reopen_reservation_early() {
        let registry = Arc::new(RuntimeThreadRegistry::default());
        let session_id = SessionId::from(98_012);
        let late_id = SessionId::from(98_013);
        let join_release = Arc::new(Barrier::new(2));
        let worker_release = Arc::clone(&join_release);
        publish(&registry, session_id, move || {
            worker_release.wait();
        });

        let (snapshot_entered, snapshot_entered_rx) = mpsc::sync_channel(1);
        let snapshot_release = Arc::new(Barrier::new(2));
        *registry
            .drain_snapshot_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::new(TestDrainSnapshotHook {
                entered: snapshot_entered,
                release: Arc::clone(&snapshot_release),
                armed: std::sync::atomic::AtomicBool::new(true),
            }));

        let first_registry = Arc::clone(&registry);
        let first = thread::spawn(move || first_registry.join_all());
        snapshot_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first drain did not pause after its snapshot");
        wait_for_state(&registry, |state| state.drainers == 1);

        let completion = wait_for_joining(&registry, session_id);
        let second_registry = Arc::clone(&registry);
        let second = thread::spawn(move || second_registry.join_all());
        completion.wait_for_waiters(1);
        wait_for_state(&registry, |state| state.drainers == 1);

        let (admitted, admitted_rx) = mpsc::sync_channel(1);
        let reserve_registry = Arc::clone(&registry);
        let reserve = thread::spawn(move || {
            let reservation = reserve_registry.reserve(late_id).unwrap();
            admitted.send(()).unwrap();
            reservation.fail();
        });
        assert!(
            matches!(
                admitted_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "the second drainer reopened reservation while the first was paused"
        );

        snapshot_release.wait();
        admitted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reservation did not reopen after every drain snapshot finished");
        reserve.join().unwrap();
        join_release.wait();

        let expected = RuntimeThreadJoinOutcome::Clean { session_id };
        assert_eq!(first.join().unwrap(), vec![expected]);
        assert_eq!(second.join().unwrap(), vec![expected]);
        assert_eq!(
            registry.join_exact(late_id),
            RuntimeThreadJoinOutcome::SpawnFailed {
                session_id: late_id
            }
        );
    }

    #[test]
    fn spawn_failure_is_observed_once_then_the_exact_id_can_be_reused() {
        let registry = Arc::new(RuntimeThreadRegistry::default());
        let session_id = SessionId::from(98_005);
        registry.reserve(session_id).unwrap().fail();
        assert_eq!(
            registry.join_exact(session_id),
            RuntimeThreadJoinOutcome::SpawnFailed { session_id }
        );

        publish(&registry, session_id, || {});
        assert_eq!(
            registry.join_exact(session_id),
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
    }

    #[test]
    fn exact_join_releases_the_map_lock_before_waiting_for_the_thread() {
        let registry = Arc::new(RuntimeThreadRegistry::default());
        let first_id = SessionId::from(98_006);
        let second_id = SessionId::from(98_007);
        let release = Arc::new(Barrier::new(2));
        let (reentered, reentered_rx) = mpsc::sync_channel(1);
        let worker_registry = Arc::clone(&registry);
        let worker_release = Arc::clone(&release);
        publish(&registry, first_id, move || {
            worker_release.wait();
            publish(&worker_registry, second_id, || {});
            reentered.send(()).unwrap();
        });

        let join_registry = Arc::clone(&registry);
        let join = thread::spawn(move || join_registry.join_exact(first_id));
        let _completion = wait_for_joining(&registry, first_id);
        release.wait();
        reentered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("joined thread could not reenter the unlocked registry");
        assert_eq!(
            join.join().unwrap(),
            RuntimeThreadJoinOutcome::Clean {
                session_id: first_id
            }
        );
        assert_eq!(
            registry.join_exact(second_id),
            RuntimeThreadJoinOutcome::Clean {
                session_id: second_id
            }
        );
    }

    #[test]
    fn join_all_seals_and_resolves_reserved_entries_before_draining() {
        let registry = Arc::new(RuntimeThreadRegistry::default());
        let reserved_id = SessionId::from(98_008);
        let failed_id = SessionId::from(98_009);
        let reservation = registry.reserve(reserved_id).unwrap();
        registry.reserve(failed_id).unwrap().fail();

        let join_registry = Arc::clone(&registry);
        let join = thread::spawn(move || join_registry.join_all());
        wait_for_state(&registry, |state| state.drainers != 0);
        assert!(!join.is_finished());
        reservation.publish(thread::spawn(|| {}));

        assert_eq!(
            join.join().unwrap(),
            vec![
                RuntimeThreadJoinOutcome::Clean {
                    session_id: reserved_id
                },
                RuntimeThreadJoinOutcome::SpawnFailed {
                    session_id: failed_id
                },
            ]
        );
        assert!(registry.join_all().is_empty());
    }

    #[test]
    fn public_runtime_new_keeps_its_infallible_compatibility_signature() {
        type InfallibleRuntimeConstructor = fn(
            SessionId,
            Arc<String>,
            Arc<String>,
            Arc<String>,
            Option<Mapper>,
            Option<smudgy_cloud::PackageApiClient>,
            Option<PackageProviderFactory>,
            ScriptExtensionFactory,
            Option<crate::session::EngineResetHook>,
            Option<crate::session::RuntimeAudioScope>,
            Sender<TaggedSessionEvent>,
            Option<UiCommandBus>,
        ) -> Runtime;

        let _: InfallibleRuntimeConstructor = Runtime::new;
    }
}

type SentSessionEvent<'a> = futures::sink::Send<'a, Sender<TaggedSessionEvent>, TaggedSessionEvent>;

/// Minimum time between flushes of the session log's `BufWriter`. Flushing on
/// every buffer update would defeat the 64 KiB write buffer on every network
/// read; instead the log is flushed at most this often, plus explicitly on
/// disconnect, on reload teardown, when logging is toggled off, and by the
/// `BufWriter`'s drop at session end.
const LOG_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// Maximum time a closing session waits for Tokio blocking work to finish.
/// Async tasks are cancelled immediately by runtime shutdown; this bound is for
/// `spawn_blocking` work started by Deno resources or ops, which Tokio otherwise
/// waits for indefinitely when the runtime's last owner is simply dropped.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Consume the session's last Tokio-runtime owner after all runtime-bound state has dropped.
fn shutdown_tokio_runtime(runtime: Rc<tokio::runtime::Runtime>) {
    let runtime = Rc::try_unwrap(runtime).unwrap_or_else(|runtime| {
        panic!(
            "session Tokio runtime still has {} owners after Inner teardown",
            Rc::strong_count(&runtime)
        )
    });
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
}

/// Capacity of the per-session automation broadcast. Each message is one coalesced
/// per-drain batch (not per-automation), so a small buffer is ample; a lagging window
/// skips intermediate batches and gets a fresh reset when it re-subscribes.
const AUTOMATION_BROADCAST_CAPACITY: usize = 256;

/// Capacity of the per-session catalogue broadcast. Each message is a full coalesced
/// snapshot (latest wins), so a lagging window only ever needs the most recent one.
const CATALOGUE_BROADCAST_CAPACITY: usize = 4;

impl Runtime {
    /// Spawn a session's runtime thread and return a handle to it.
    ///
    /// # Panics
    ///
    /// Panics if this session id already has an unconsumed runtime-thread join
    /// authority, if the OS thread cannot be spawned, or if the current-thread
    /// Tokio runtime fails to build.
    pub fn new(
        session_id: SessionId,
        server_name: Arc<String>,
        profile_name: Arc<String>,
        profile_subtext: Arc<String>,
        mapper: Option<Mapper>,
        package_client: Option<smudgy_cloud::PackageApiClient>,
        package_provider_override: Option<PackageProviderFactory>,
        extra_script_extensions: ScriptExtensionFactory,
        on_engine_rebuild: Option<crate::session::EngineResetHook>,
        audio_scope: Option<crate::session::RuntimeAudioScope>,
        ui_tx: Sender<TaggedSessionEvent>,
        ui_commands: Option<UiCommandBus>,
    ) -> Self {
        Self::try_new(
            session_id,
            server_name,
            profile_name,
            profile_subtext,
            mapper,
            package_client,
            package_provider_override,
            extra_script_extensions,
            on_engine_rebuild,
            audio_scope,
            ui_tx,
            ui_commands,
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    /// Fallible runtime-thread constructor used by transactional embedders.
    /// A thread creation failure leaves its exact result-bearing tombstone.
    pub(crate) fn try_new(
        session_id: SessionId,
        server_name: Arc<String>,
        profile_name: Arc<String>,
        profile_subtext: Arc<String>,
        mapper: Option<Mapper>,
        package_client: Option<smudgy_cloud::PackageApiClient>,
        // Optional alternate package resolver, built per engine on the session thread; when
        // `None` the engine builds the cloud-backed provider from `package_client`. The
        // `Arc` factory is cloned for the initial build and each reload.
        package_provider_override: Option<PackageProviderFactory>,
        extra_script_extensions: ScriptExtensionFactory,
        // Embedder reset for engine-generation-coupled state (see `EngineResetHook`), invoked
        // on the session thread before every `ScriptEngine::new` below.
        on_engine_rebuild: Option<crate::session::EngineResetHook>,
        // Opaque application/session Web Audio authority. `None` preserves the
        // legacy no-audio runtime even when the feature is compiled.
        audio_scope: Option<crate::session::RuntimeAudioScope>,
        ui_tx: Sender<TaggedSessionEvent>,
        ui_commands: Option<UiCommandBus>,
    ) -> Result<Self, RuntimeThreadSpawnError> {
        let (session_runtime_tx, session_runtime_rx) =
            tokio::sync::mpsc::unbounded_channel::<RuntimeAction>();

        let local_session_runtime_tx = session_runtime_tx.clone();

        let local_server_name = server_name.clone();
        let local_profile_name = profile_name.clone();
        let local_ui_tx = ui_tx.clone();
        let ui_command_producer = ui_commands.map(|bus| UiCommandProducer::new(session_id, bus));
        let (automation_tx, _) =
            broadcast::channel::<AutomationEvent>(AUTOMATION_BROADCAST_CAPACITY);
        let local_automation_tx = automation_tx.clone();
        let (catalogue_tx, _) = broadcast::channel::<CatalogueEvent>(CATALOGUE_BROADCAST_CAPACITY);
        let local_catalogue_tx = catalogue_tx.clone();

        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let local_connected = Arc::clone(&connected);
        let published_store = Arc::new(RwLock::new(store::PublishedStore::default()));
        let local_published_store = Arc::clone(&published_store);

        // These registries are data-only and deliberately shared outside the
        // session thread. Foreign Session/Pane/Input handles can therefore
        // resolve and read the owning session's live state synchronously;
        // effects continue to enter through that runtime's ordered queue.
        let pane_registry: SharedPaneRegistry = Arc::new(Mutex::new(PaneRegistry::new()));
        let input_mirror: SharedInputMirror = Arc::new(Mutex::new(InputMirror::default()));
        let pane_size_mirror: SharedPaneSizeMirror =
            Arc::new(Mutex::new(pane::PaneSizeMirror::default()));
        let input_word_sets: SharedInputWordSets =
            Arc::new(Mutex::new(input::InputWordSets::default()));
        let pane_input_callbacks: SharedPaneInputCallbacks =
            Arc::new(Mutex::new(input::PaneInputCallbacks::default()));
        let local_pane_registry = Arc::clone(&pane_registry);
        let local_input_mirror = Arc::clone(&input_mirror);
        let local_pane_size_mirror = Arc::clone(&pane_size_mirror);
        let local_input_word_sets = Arc::clone(&input_word_sets);
        let local_pane_input_callbacks = Arc::clone(&pane_input_callbacks);
        let (start_tx, start_rx) = std::sync::mpsc::channel::<std::sync::mpsc::Receiver<()>>();

        // Reserve the exact id before the OS thread can start. Exact and
        // drain-all joins wait on this state until handle publication or an
        // explicit spawn-failure tombstone, so no immediate exit can detach
        // or overwrite its join authority.
        let thread_reservation = runtime_threads().reserve(session_id).unwrap_or_else(
            |RuntimeThreadReservationError::AlreadyTracked| {
                panic!("runtime thread for session {session_id} is already tracked")
            },
        );
        let thread_body = move || {
            // `Runtime::new` must spawn before it can return the registry
            // handle, but script top-level code may consult that registry.
            // Do not construct/evaluate the engine until registration opens
            // this gate.
            let Ok(commit_rx) = start_rx.recv() else {
                return;
            };
            if commit_rx.recv().is_err() {
                return;
            }
            let pending_line_operations = Rc::new(RefCell::new(Vec::new()));

            // We start at 1 because the first line ("Loading session...") is already emitted
            let emitted_line_count = Rc::new(Cell::new(0));

            // The session-side bounded ring of recently-emitted lines. The SAME `Rc` is
            // read by every isolate's `buffer.line(n)` read ops and written at emit time. It is
            // preserved across a reload (like `pending_line_operations`), so the buffer the UI
            // shows and the lines a script can read stay aligned through an engine rebuild.
            let recent_lines: RecentLines = Rc::new(RefCell::new(VecDeque::new()));

            // The session's current mapper location, mirrored here from `SetCurrentLocation`
            // and read back by `getCurrentLocation`. Preserved across reload (cloned below).
            let current_location: CurrentLocation = Rc::new(RefCell::new(None));

            let pane_registry = local_pane_registry;

            // Per-line routing state (gag/redirect/copy), cleared per line event; shared into
            // every isolate's ops beside `pending_line_operations`.
            let line_routing: SharedLineRouting = Rc::new(RefCell::new(LineRouting::default()));

            // The input mirror (`docs/input.md` §3.3): read synchronously by every
            // isolate's input ops, written by the `InputStateChanged` dispatch arm. Session-
            // scoped (survives reload) like the pane registry — interest is a session fact.
            let input_mirror = local_input_mirror;

            // The pane-size mirror (panes.md placement read-back): read synchronously by
            // every isolate's `pane.size` op, written by the `PaneDisplayChanged` dispatch
            // arm. Session-scoped (survives reload) like the input mirror — interest is a
            // session fact.
            let pane_size_mirror = local_pane_size_mirror;

            // The in-flight typed submission `sys:input` handlers act on: installed by the
            // `SubmitInput` dispatch arm, mutated by the submission ops, consumed by the
            // completion arm. Shared into every isolate's ops beside `line_routing`. The
            // slot also owns the generation counter that stamps each installed submission
            // (the staleness nonce the submission ops check).
            let input_submission: SharedInputSubmission =
                Rc::new(RefCell::new(input::InputSubmissionSlot::default()));

            // The completion word sets (`docs/input.md` §3.8): mutated and read
            // synchronously by every isolate's registry ops, merged and pushed to the UI by
            // the `InputWordSetsChanged` dispatch arm. Session-scoped cell, engine-scoped
            // contents — the reload path below resets the contributions like hotkeys.
            let input_word_sets = local_input_word_sets;

            // The pane-input onSubmit registry (`docs/input.md` §3.7): written by
            // the registration op, resolved by the `PaneInputSubmit` dispatch arm. Session-
            // scoped cell, engine-scoped contents — handlers name functions of the engine
            // that registered them, so the reload path below resets it like the word sets.
            let pane_input_callbacks = local_pane_input_callbacks;

            // The session store (`docs/interop.md`): the same `Rc` is bound into
            // every isolate's ops (writes journal here) and held by `Inner` (the run loop
            // flushes the journal per turn). Created once per session — the committed tree
            // survives engine reloads; the per-engine pieces (watchers, journal) are reset
            // below before each rebuild.
            let session_store: SharedSessionStore = Rc::new(RefCell::new(SessionStore::new()));

            // The message bus (`docs/interop.md` §6): the same `Rc` is bound into
            // every isolate's ops. Session-scoped like the store — receivers are reset per
            // engine below, pending posts survive the rebuild (queue-briefly).
            let message_bus: SharedMessageBus = Rc::new(RefCell::new(MessageBus::new()));

            // The runtime catalogue (`docs/interop.md` §10): sampled at the emit/
            // post choke points in the ops, declared-into when each engine builds, snapshotted
            // to subscribed windows at the drain point. Session-scoped like the store. The
            // broadcast handle doubles as the live subscriber probe: the record path reads
            // receiver presence where it changes, so a store tab that subscribes mid-turn is
            // honored for every sample recorded before the next drain (a drain-pushed flag
            // alone would lose a >ring burst in that gap to the all-history carve-out).
            let catalogue: SharedCatalogue = Rc::new(RefCell::new(RuntimeCatalogue::new()));
            catalogue
                .borrow_mut()
                .attach_subscriber_probe(local_catalogue_tx.clone());

            // The GMCP enabled flag (`docs/gmcp.md` §3.4): written by the producer's
            // enable/disable arms, read by every isolate's `gmcp.enabled`/`gmcp.onReady`.
            // Session-scoped like the producer that owns it (survives reload).
            let gmcp_enabled = gmcp::SharedGmcpEnabled::new();

            // Script-visible settings snapshot backing `getSettings()`, seeded from disk before
            // the engine is built so even a module's top-level `getSettings()` sees real values.
            // The UI fills in the resolved palette and refreshes this via `ApplySettings`.
            let settings_snapshot: SettingsSnapshot = Rc::new(RefCell::new(
                crate::models::settings::ScriptSettings::from(&load_settings()),
            ));

            let spawned_actions: ActionQueue = Rc::new(RefCell::default());

            let runtime = Rc::new(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to create tokio runtime"),
            );

            // Introspection mirror: the SAME `Rc` is read by this engine's `get`/`list`/
            // `exists` ops (via `OpState`) and written by the `Manager`. A fresh one per engine,
            // so a reload (which rebuilds both below) starts with an empty registry.
            let automation_registry: trigger::SharedAutomationRegistry = Rc::default();

            // Embedder state coupled to an engine generation is reset before EVERY engine
            // build, this initial one included: the session's widget root outlives the
            // `Runtime` (it lives on the UI's session store), so a re-spawned session would
            // otherwise start with widgets minted by the previous runtime's dead isolates.
            if let Some(reset) = on_engine_rebuild.as_ref() {
                reset();
            }

            let script_engine = ScriptEngine::new(ScriptEngineParams {
                session_id,
                server_name: &local_server_name,
                ui_tx: local_ui_tx.clone(),
                ui_command_producer: ui_command_producer.clone(),
                spawned_actions: spawned_actions.clone(),
                pending_line_operations: &pending_line_operations,
                emitted_line_count: Rc::downgrade(&emitted_line_count),
                recent_lines: recent_lines.clone(),
                current_location: current_location.clone(),
                settings_snapshot: settings_snapshot.clone(),
                pane_registry: pane_registry.clone(),
                line_routing: line_routing.clone(),
                input_mirror: input_mirror.clone(),
                pane_size_mirror: pane_size_mirror.clone(),
                input_submission: input_submission.clone(),
                input_word_sets: input_word_sets.clone(),
                pane_input_callbacks: pane_input_callbacks.clone(),
                session_store: session_store.clone(),
                message_bus: message_bus.clone(),
                catalogue: catalogue.clone(),
                gmcp_enabled: gmcp_enabled.clone(),
                mapper: mapper.clone(),
                package_client: package_client.clone(),
                package_provider_override: package_provider_override.clone(),
                extra_script_extensions: extra_script_extensions.clone(),
                tokio_runtime: runtime.clone(),
                automation_registry: automation_registry.clone(),
                audio_scope: audio_scope.clone(),
            });

            // Seed runtime-relevant settings from disk; the UI live-updates
            // them later via `RuntimeAction::ApplySettings`.
            let settings = load_settings();
            let command_separator = Arc::new(settings.command_separator);

            let mut trigger_manager = Manager::new(
                spawned_actions.clone(),
                command_separator.clone(),
                automation_registry,
            );
            trigger_manager.set_bold_is_bright(settings.terminal_bold_mode.uses_bright_palette());

            let mut inner = Inner {
                log_file: None,
                log_enabled: settings.logging.enabled,
                last_log_flush: Instant::now(),
                session_id,
                user_automations: crate::session::config::UserAutomations::default(),
                trigger_manager,
                hotkeys: BTreeMap::new(),
                next_hotkey_id: HotkeyId(0),
                hotkey_ids: HashMap::new(),
                script_engine,
                server_name: &local_server_name,
                profile_name: &local_profile_name,
                mapper: mapper.clone(),
                session_runtime_rx,
                session_runtime_tx: local_session_runtime_tx.clone(),
                spawned_actions: spawned_actions.clone(),
                ui_tx: local_ui_tx.clone(),
                ui_command_producer: ui_command_producer.clone(),
                automation_tx: local_automation_tx.clone(),
                last_automation_receivers: 0,
                catalogue_tx: local_catalogue_tx.clone(),
                last_catalogue_receivers: 0,
                catalogue_cadence: CatalogueCadence::default(),
                catalogue_resend_at: None,
                connection: None,
                connection_generation: 0,
                connected_at: None,
                pending_send_on_connect: None,
                send_on_connect_armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                window_size: Arc::new(std::sync::atomic::AtomicU32::new(
                    super::connection::responders::pack_dims(
                        super::connection::responders::DEFAULT_DIMS.0,
                        super::connection::responders::DEFAULT_DIMS.1,
                    ),
                )),
                pending_buffer_updates: Vec::new(),
                pending_line_operations: pending_line_operations.clone(),
                emitted_line_count: emitted_line_count.clone(),
                recent_lines: recent_lines.clone(),
                current_location: current_location.clone(),
                pane_registry: pane_registry.clone(),
                line_routing: line_routing.clone(),
                input_mirror: input_mirror.clone(),
                pane_size_mirror: pane_size_mirror.clone(),
                input_submission: input_submission.clone(),
                input_word_sets: input_word_sets.clone(),
                pane_input_callbacks: pane_input_callbacks.clone(),
                session_store: session_store.clone(),
                published_store: Arc::clone(&local_published_store),
                connected: Arc::clone(&local_connected),
                catalogue: catalogue.clone(),
                gmcp: gmcp::GmcpProducer::new(gmcp_enabled.clone()),
                msdp: msdp::MsdpProducer::new(),
                mssp: mssp::MsspProducer::new(),
                // The spawn-time "Loading session..." append left the main
                // buffer's tail line open — unless an engine-construction
                // session notice (emitted directly on ui_tx, each ending in
                // EnsureNewLine) already committed it, which the notice's
                // count bump records.
                main_open_line: emitted_line_count.get() == 0,
                main_prefix_disposition: MainPrefixDisposition::None,
                main_partial_source_len: 0,
                main_committed_source_len: 0,
                main_open_fragments: if emitted_line_count.get() == 0 {
                    LineFragments::One(Arc::new(StyledLine::from_echo_str("Loading session...")))
                } else {
                    LineFragments::None
                },
                main_deferred_fragments: LineFragments::None,
                replacing_main_open_line: false,
                fragmented_completion_in_flight: false,
                partial_line_in_flight: None,
                open_line: LineFragments::None,
                log_open_line: Vec::new(),
                log_committed_len: 0,
                log_open_on_disk: false,
                command_separator,
                raw_line_prefix: Arc::new(settings.raw_line_prefix),
                settings_snapshot: settings_snapshot.clone(),
            };

            while let RunAction::Reload = runtime.block_on(inner.run()) {
                info!("Reloading session runtime...");

                // Rebuilding the engine below (V8 isolate construction + module
                // evaluation) blocks this session thread, so input goes
                // unprocessed until it finishes. Echo a notice on the still-intact
                // old `Inner` and flush it to the UI first, so the user sees why
                // the session briefly stops responding. The flush only enqueues to
                // the UI channel; the separate UI thread renders it independently
                // of this thread's blocking rebuild.
                runtime.block_on(async {
                    if let Ok(Some(fut)) = inner.echo_str("Reloading scripts...") {
                        let _ = fut.await;
                    }
                });

                // Flush the session log before the old Inner is torn down so
                // any write errors get surfaced (drop flushes silently).
                inner.flush_log();

                // Extract the receiver and connection from the old inner before dropping it,
                // plus the line-pipeline state that must survive the rebuild: whether main's
                // tail line is open and the in-flight logical line's accumulated fragments
                // (a reload can land mid-server-line). An active carriage-return replacement
                // does not survive: `run` aborts it before returning `Reload`, because the
                // completion action belonged to the discarded action stack.
                let old_main_open_line = inner.main_open_line;
                let old_main_prefix_disposition = inner.main_prefix_disposition;
                let old_main_partial_source_len = inner.main_partial_source_len;
                let old_main_committed_source_len = inner.main_committed_source_len;
                let mut old_main_open_fragments = std::mem::take(&mut inner.main_open_fragments);
                let old_main_deferred_fragments =
                    std::mem::take(&mut inner.main_deferred_fragments);
                debug_assert!(!inner.replacing_main_open_line);
                let old_open_line = std::mem::take(&mut inner.open_line);
                let old_connection = inner.connection.take();
                let old_connection_generation = inner.connection_generation;
                let old_connected_at = inner.connected_at.take();
                let old_pending_send_on_connect = inner.pending_send_on_connect.take();
                // The surviving connection's VtProcessor holds a clone of this
                // exact cell (like the raw-wanted flag below); the rebuilt
                // Inner must keep writing to it, and the pending send it
                // mirrors survives the reload untouched.
                let old_send_on_connect_armed = inner.send_on_connect_armed.clone();
                // The window-size cell is session-lifetime like the connection: the
                // surviving connection's socket task was seeded from this cell, and
                // the UI only re-reports on actual grid changes.
                let old_window_size = inner.window_size.clone();
                // The surviving connection's VtProcessor holds a clone of the OLD
                // manager's raw-wanted flag; the new manager must keep writing to
                // that same cell or raw capture goes dead across a reload.
                let old_raw_wanted = inner.trigger_manager.raw_wanted_flag();
                // The GMCP producer is session-scoped like the subtree it writes: the
                // enabled flag tracks the (surviving) connection, and merge keys/memo are
                // server facts, not engine facts. Module refs ARE engine facts (isolates
                // die with the engine; the reloading packages re-register) — released
                // here like the store's watchers.
                let mut old_gmcp = std::mem::replace(
                    &mut inner.gmcp,
                    gmcp::GmcpProducer::new(gmcp_enabled.clone()),
                );
                old_gmcp.reset_engine_refs();
                // The MSDP producer holds no engine facts at all; it survives whole.
                let old_msdp = std::mem::replace(&mut inner.msdp, msdp::MsdpProducer::new());
                // The MSSP producer likewise.
                let old_mssp_producer =
                    std::mem::replace(&mut inner.mssp, mssp::MsspProducer::new());
                let mut old_session_runtime_rx =
                    std::mem::replace(&mut inner.session_runtime_rx, {
                        // Create a dummy receiver that will be immediately replaced
                        let (_, rx) = tokio::sync::mpsc::unbounded_channel();
                        rx
                    });

                // Purge engine-bound actions left queued behind the `Reload` (chiefly session-store
                // watch deliveries and async event forwards, which ride this channel). Their
                // `ScriptId`/`FunctionId`/`v8::Global` name the OLD engine's registries; dispatched
                // into the rebuilt engine they would index a fresh registry and invoke an unrelated
                // handler (or error). The reload re-runs every module, so nothing is lost by
                // dropping them. Drain-and-requeue preserves the order of the surviving actions
                // (external input: Connect, HandleIncomingLine, …); the channel is otherwise idle
                // during the synchronous rebuild.
                {
                    let mut kept = Vec::new();
                    while let Ok(action) = old_session_runtime_rx.try_recv() {
                        if !action.references_engine_state() {
                            kept.push(action);
                        }
                    }
                    for action in kept {
                        if local_session_runtime_tx.send(action).is_err() {
                            warn!("Dropping preserved action on reload: runtime channel closed");
                        }
                    }
                }

                // Seal this exact engine generation before disposing its isolates. Every
                // audio-enabled main/package extension shares the paired registrar, so the
                // receipt below covers all online contexts admitted by this generation.
                #[cfg(feature = "web-audio")]
                let audio_retirement = inner.script_engine.retire_audio_generation();

                runtime.block_on(async move {
                    drop(inner);
                });

                // Native audio shutdown is acknowledged out-of-band after isolate disposal.
                // Await that exact generation before constructing its replacement: otherwise
                // a bounded application host can reject the replacement's first AudioContext
                // while a predecessor observer still owns the permit.
                #[cfg(feature = "web-audio")]
                if let Some(retirement) = audio_retirement
                    && let Err(error) = runtime.block_on(retirement)
                {
                    error!("Web Audio generation retirement failed during reload: {error}");

                    let message = format!(
                        "[audio] Script reload stopped because the previous Web Audio generation could not shut down safely: {error}"
                    );
                    let mut failure_ui_tx = local_ui_tx.clone();
                    let failure_event = TaggedSessionEvent {
                        session_id,
                        event: SessionEvent::UpdateBuffer(Arc::new(vec![
                            BufferUpdate::Append(Arc::new(StyledLine::from_warn_str(&message))),
                            BufferUpdate::EnsureNewLine,
                        ])),
                    };
                    if let Err(send_error) = runtime.block_on(failure_ui_tx.send(failure_event)) {
                        warn!("Failed to report Web Audio retirement failure: {send_error:?}");
                    }

                    // These values were extracted only to survive a successful rebuild. On a
                    // failed retirement, dispose them before the Tokio runtime they may use.
                    drop((
                        old_open_line,
                        old_connection,
                        old_pending_send_on_connect,
                        old_send_on_connect_armed,
                        old_window_size,
                        old_raw_wanted,
                        old_gmcp,
                        old_msdp,
                        old_mssp_producer,
                        old_session_runtime_rx,
                    ));
                    registry::unregister_session(session_id);
                    shutdown_tokio_runtime(runtime);
                    info!("Runtime thread shutting down after failed Web Audio retirement");
                    return;
                }

                // Discard anything scripts left behind in the spawned-action
                // queue; the engine they came from is gone.
                spawned_actions.borrow_mut().clear();

                // A submission caught mid-splice by the reload dies with its handlers:
                // the completion action was queued in `spawned_actions` (just cleared),
                // so drop the state it would have consumed.
                input_submission.borrow_mut().take();

                // Completion word sets are engine facts, like hotkeys: drop every
                // contribution before the rebuild (the reloading modules re-register
                // theirs). The inputs that held words — plus any whose pending push
                // action just died in the queues cleared above — get one push action
                // each, queued BEHIND the rebuild below, so the UI's merged copy is
                // refreshed: re-registered words go out merged, an unclaimed input
                // goes out empty.
                let word_set_resyncs = input_word_sets
                    .lock()
                    .unwrap()
                    .reset_engine_state(session_id);

                // Pane-input onSubmit handlers are engine facts too: their function ids
                // index the disposed isolates' registries. Drop them all; the reloading
                // scripts re-register theirs beside their re-claiming splits, and a pane
                // nobody re-claims is closed by the sweep queued below anyway.
                pane_input_callbacks
                    .lock()
                    .unwrap()
                    .reset_engine_state(session_id);
                for other in registry::get_runtimes_for_server(local_server_name.as_str()) {
                    if other.session_id != session_id {
                        for key in input::purge_session_input_interop(
                            &other.input_word_sets,
                            &other.pane_input_callbacks,
                            session_id,
                        ) {
                            let _ = other.tx.send(RuntimeAction::InputWordSetsChanged { key });
                        }
                    }
                }

                // Drop the store's engine-scoped state (watchers hold function ids into the
                // disposed isolates; any unflushed journal belongs to the dead run) while the
                // committed tree survives — reloads don't drop session state. Before the new
                // engine is built, so module top-level writes journal into a clean slate.
                session_store.borrow_mut().reset_engine_state();

                // Likewise the message bus (receivers hold function ids; pending posts survive
                // the rebuild — queue-briefly, D1) and the catalogue (declared/confirmed flags
                // are per-engine facts the rebuilt engine re-registers; samples are history).
                message_bus.borrow_mut().reset_engine_state();
                catalogue.borrow_mut().reset_engine_state();

                // Reset embedder engine-generation state (the UI's mounted widgets) between
                // the old engine's teardown and the new engine's module loads: the old
                // isolates are disposed, so the entries' `v8::Global` callbacks drop as
                // no-ops, and the reloading modules re-mount theirs into the fresh engine.
                if let Some(reset) = on_engine_rebuild.as_ref() {
                    reset();
                }

                // Create completely new Inner struct with fresh ScriptEngine and TriggerManager
                // This avoids any V8 isolate replacement issues
                // Fresh introspection mirror for the rebuilt engine (clears every entry).
                let automation_registry: trigger::SharedAutomationRegistry = Rc::default();

                // Engine-construction session notices commit the main open
                // line behind Inner's back (they end in EnsureNewLine and
                // bump the count); detect that to keep the open-line flag
                // honest across the rebuild.
                let count_before_rebuild = emitted_line_count.get();

                // New claim epoch: every `split()` the reloading scripts make
                // during (or after) the rebuild re-claims its pane; the sweep
                // queued below then closes whatever nothing re-claimed (e.g.
                // a disabled package's leftover panel). Placement of the
                // survivors is untouched — existence is the only thing swept.
                pane_registry.lock().unwrap().begin_claim_epoch();

                let new_script_engine = ScriptEngine::new(ScriptEngineParams {
                    session_id,
                    server_name: &local_server_name,
                    ui_tx: local_ui_tx.clone(),
                    ui_command_producer: ui_command_producer.clone(),
                    spawned_actions: spawned_actions.clone(),
                    pending_line_operations: &pending_line_operations,
                    emitted_line_count: Rc::downgrade(&emitted_line_count),
                    recent_lines: recent_lines.clone(),
                    current_location: current_location.clone(),
                    settings_snapshot: settings_snapshot.clone(),
                    pane_registry: pane_registry.clone(),
                    line_routing: line_routing.clone(),
                    input_mirror: input_mirror.clone(),
                    pane_size_mirror: pane_size_mirror.clone(),
                    input_submission: input_submission.clone(),
                    input_word_sets: input_word_sets.clone(),
                    pane_input_callbacks: pane_input_callbacks.clone(),
                    session_store: session_store.clone(),
                    message_bus: message_bus.clone(),
                    catalogue: catalogue.clone(),
                    gmcp_enabled: gmcp_enabled.clone(),
                    mapper: mapper.clone(),
                    package_client: package_client.clone(),
                    package_provider_override: package_provider_override.clone(),
                    extra_script_extensions: extra_script_extensions.clone(),
                    tokio_runtime: runtime.clone(),
                    automation_registry: automation_registry.clone(),
                    audio_scope: audio_scope.clone(),
                });

                // The engine constructor blocked until every isolate's
                // top-level code ran, so the claims are in. Queue the sweep
                // BEHIND whatever actions those modules spawned: a doomed
                // pane's last load-time deliveries land before its close.
                spawned_actions
                    .borrow_mut()
                    .push_back(RuntimeAction::PaneReloadSweep);

                // The word-set resyncs queued behind the modules' own spawned actions:
                // the pushes read the live sets at dispatch, so each carries whatever
                // the reloaded scripts re-registered (or the empty view).
                {
                    let mut spawned = spawned_actions.borrow_mut();
                    for key in word_set_resyncs {
                        spawned.push_back(RuntimeAction::InputWordSetsChanged { key });
                    }
                }

                // Reload rebuilds Inner, so re-seed settings from disk; this
                // also picks up settings edits made while the session ran.
                let settings = load_settings();
                // Refresh the script-visible snapshot too (the UI re-sends the resolved palette
                // on the post-reload `RuntimeReady`).
                *settings_snapshot.borrow_mut() =
                    crate::models::settings::ScriptSettings::from(&settings);
                let command_separator = Arc::new(settings.command_separator);
                let rebuilt_main_open_line =
                    old_main_open_line && emitted_line_count.get() == count_before_rebuild;
                if !rebuilt_main_open_line {
                    old_main_open_fragments.clear();
                }

                let mut new_trigger_manager = Manager::new(
                    spawned_actions.clone(),
                    command_separator.clone(),
                    automation_registry,
                );
                new_trigger_manager
                    .set_bold_is_bright(settings.terminal_bold_mode.uses_bright_palette());
                new_trigger_manager.adopt_raw_wanted_flag(old_raw_wanted);

                // Replace with the new inner struct
                inner = Inner {
                    log_file: None, // Will restart logging
                    log_enabled: settings.logging.enabled,
                    last_log_flush: Instant::now(),
                    session_id,
                    user_automations: crate::session::config::UserAutomations::default(),
                    trigger_manager: new_trigger_manager,
                    hotkeys: BTreeMap::new(), // Reset hotkeys - they'll be re-registered by modules
                    next_hotkey_id: HotkeyId(0),
                    hotkey_ids: HashMap::new(),
                    script_engine: new_script_engine,
                    server_name: &local_server_name,
                    profile_name: &local_profile_name,
                    session_runtime_rx: old_session_runtime_rx,
                    session_runtime_tx: local_session_runtime_tx.clone(),
                    spawned_actions: spawned_actions.clone(),
                    ui_tx: local_ui_tx.clone(),
                    ui_command_producer: ui_command_producer.clone(),
                    automation_tx: local_automation_tx.clone(),
                    last_automation_receivers: 0,
                    catalogue_tx: local_catalogue_tx.clone(),
                    last_catalogue_receivers: 0,
                    catalogue_cadence: CatalogueCadence::default(),
                    catalogue_resend_at: None,
                    connection: old_connection, // Preserve the connection
                    connection_generation: old_connection_generation,
                    connected_at: old_connected_at,
                    pending_send_on_connect: old_pending_send_on_connect,
                    send_on_connect_armed: old_send_on_connect_armed,
                    window_size: old_window_size,
                    pending_buffer_updates: Vec::new(),
                    pending_line_operations: pending_line_operations.clone(), // Preserve the shared operations
                    emitted_line_count: emitted_line_count.clone(),
                    recent_lines: recent_lines.clone(), // Preserve the recent-lines ring across reload
                    current_location: current_location.clone(), // Preserve current location across reload
                    pane_registry: pane_registry.clone(),       // Panes survive script reloads
                    line_routing: line_routing.clone(),
                    input_mirror: input_mirror.clone(), // Mirror + interest survive reload
                    pane_size_mirror: pane_size_mirror.clone(),
                    input_submission: input_submission.clone(), // Cleared above; the cell itself is session-scoped
                    input_word_sets: input_word_sets.clone(), // Contributions reset above; the cell itself is session-scoped
                    pane_input_callbacks: pane_input_callbacks.clone(), // Handlers reset above; the cell itself is session-scoped
                    session_store: session_store.clone(), // Committed store state survives reload
                    published_store: Arc::clone(&local_published_store),
                    connected: Arc::clone(&local_connected),
                    catalogue: catalogue.clone(), // Samples are session history
                    gmcp: old_gmcp, // Session-scoped: enabled tracks the surviving connection
                    msdp: old_msdp, // Same: server facts, no engine facts
                    mssp: old_mssp_producer, // Same
                    main_open_line: rebuilt_main_open_line,
                    main_prefix_disposition: old_main_prefix_disposition,
                    main_partial_source_len: old_main_partial_source_len,
                    main_committed_source_len: old_main_committed_source_len,
                    main_open_fragments: old_main_open_fragments,
                    main_deferred_fragments: old_main_deferred_fragments,
                    replacing_main_open_line: false,
                    fragmented_completion_in_flight: false,
                    partial_line_in_flight: None,
                    open_line: old_open_line,
                    log_open_line: Vec::new(), // The reload flushed the old log; the new file starts a fresh line
                    log_committed_len: 0,      // A new log file is opened on reconnect
                    log_open_on_disk: false,
                    mapper: mapper.clone(),
                    command_separator,
                    raw_line_prefix: Arc::new(settings.raw_line_prefix),
                    settings_snapshot: settings_snapshot.clone(),
                };

                info!("Session runtime reloaded successfully");
            }

            info!("Dropping inner");
            runtime.block_on(async move {
                drop(inner);
            });

            info!("Unregistering session");
            registry::unregister_session(session_id);

            // This is the last owner after `Inner` (and therefore every script
            // isolate) has been dropped above. Consume the Tokio runtime explicitly:
            // dropping it implicitly at closure return waits forever for a stuck
            // `spawn_blocking` task, which in turn makes the UI's
            // `join_runtime_threads()` hang after the main window is already gone.
            // A bounded shutdown cancels async work immediately and caps the wait for
            // blocking Deno resources/ops.
            shutdown_tokio_runtime(runtime);

            info!("Runtime thread shutting down");
        };
        let thread = thread::Builder::new()
            .name(format!("smudgy-session-{session_id}"))
            .spawn(thread_body);

        match thread {
            Ok(thread) => thread_reservation.publish(thread),
            Err(error) => {
                thread_reservation.fail();
                return Err(RuntimeThreadSpawnError {
                    session_id,
                    source: error,
                });
            }
        }

        Ok(Self {
            session_id,
            server_name,
            profile_name,
            profile_subtext,
            ui_tx,
            tx: session_runtime_tx,
            automation_tx,
            catalogue_tx,
            connected,
            published_store,
            pane_registry,
            input_mirror,
            pane_size_mirror,
            input_word_sets,
            pane_input_callbacks,
            start_tx: Mutex::new(Some(start_tx)),
        })
    }

    /// Move the worker to the second publication barrier without admitting
    /// engine construction.
    pub(crate) fn prepare_start(&self) -> Option<RuntimeStartPermit> {
        let start_tx = self
            .start_tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let start_tx = start_tx?;
        let (commit_tx, commit_rx) = std::sync::mpsc::channel();
        start_tx
            .send(commit_rx)
            .is_ok()
            .then_some(RuntimeStartPermit(commit_tx))
    }

    /// Open the prepared second-phase barrier after the `created` lifecycle
    /// occurrence has been attempted for every staged target.
    pub(crate) fn commit_start(permit: RuntimeStartPermit) -> bool {
        let RuntimeStartPermit(commit_tx) = permit;
        commit_tx.send(()).is_ok()
    }

    /// Cancel a staged worker without admitting engine construction. Dropping
    /// the one-shot sender wakes `start_rx` into the worker's clean pre-script
    /// return path.
    pub(crate) fn cancel_start(&self) {
        drop(
            self.start_tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
    }

    #[must_use]
    pub fn tx(&self) -> UnboundedSender<RuntimeAction> {
        self.tx.clone()
    }

    /// Subscribe to this session's automation broadcast (the automations window streams it
    /// to render script-created aliases/triggers). The runtime auto-sends a reset when a new
    /// subscriber appears and records deltas only while ≥1 window is subscribed.
    #[must_use]
    pub fn subscribe_automations(&self) -> broadcast::Receiver<AutomationEvent> {
        self.automation_tx.subscribe()
    }

    /// Subscribe to this session's runtime-catalogue broadcast (the automations window's
    /// store tab streams it to render the live store tree + event/message samples). The
    /// runtime sends a fresh full snapshot when a new subscriber appears and a coalesced
    /// snapshot per drain while anything interop-shaped changed; nothing is built while no
    /// window is subscribed.
    #[must_use]
    pub fn subscribe_catalogue(&self) -> broadcast::Receiver<CatalogueEvent> {
        self.catalogue_tx.subscribe()
    }
}

struct Inner<'a> {
    session_id: SessionId,
    user_automations: crate::session::config::UserAutomations,
    trigger_manager: trigger::Manager,
    script_engine: ScriptEngine<'a>,
    server_name: &'a Arc<String>,
    profile_name: &'a Arc<String>,
    session_runtime_rx: UnboundedReceiver<RuntimeAction>,
    session_runtime_tx: UnboundedSender<RuntimeAction>,
    spawned_actions: ActionQueue,
    ui_tx: Sender<TaggedSessionEvent>,
    ui_command_producer: Option<UiCommandProducer>,
    automation_tx: broadcast::Sender<AutomationEvent>,
    /// Receiver count last seen at the drain point; an increase means a new window
    /// subscribed and needs a fresh reset broadcast.
    last_automation_receivers: usize,
    /// Per-session catalogue broadcast; see [`Runtime::subscribe_catalogue`].
    catalogue_tx: broadcast::Sender<CatalogueEvent>,
    /// Receiver count last seen at the drain point (the catalogue twin of
    /// `last_automation_receivers`): an increase means a new store tab needs a snapshot.
    last_catalogue_receivers: usize,
    /// The catalogue broadcast's leading-edge/trailing-coalesce cadence state
    /// ([`catalogue::CATALOGUE_SEND_WINDOW`]); fed at the drain point.
    catalogue_cadence: CatalogueCadence,
    /// Deadline of the armed trailing-edge catalogue send: `Some` exactly while a dirty
    /// snapshot is deferred inside the send window, driving a transient one-shot
    /// `sleep_until` arm in the idle `select!` so a burst's final state lands within the
    /// window instead of waiting for the 500 ms safety tick.
    catalogue_resend_at: Option<tokio::time::Instant>,
    connection: Option<Connection>,
    /// Monotonic id assigned to each connection attempt. Inbound packet
    /// completion markers carry this id so a late marker from a replaced
    /// socket cannot affect the current connection.
    connection_generation: u64,
    /// The runtime's own connection clock: the generation whose `Connected`
    /// was last dispatched, and when. `DisconnectNotice` reads it to word the
    /// "after …" duration and clears it. Runtime-side on purpose — it measures
    /// how long the session was live from the user's seat, including the time
    /// spent working through lines a fast socket had already delivered.
    /// Session-lifetime like the connection: a reload mid-connection carries it
    /// over.
    connected_at: Option<(u64, std::time::Instant)>,
    /// Profile text held until the current connection's first fully processed
    /// inbound packet containing non-empty terminal text.
    pending_send_on_connect: Option<RuntimeAction>,
    /// Mirror of `pending_send_on_connect.is_some()`, shared with each
    /// connection's `VtProcessor` so packet-completion markers are only
    /// emitted while a deferred send could consume one. Session-lifetime like
    /// the connection: the live socket task holds a clone of this exact cell,
    /// so a reload must carry it over rather than mint a fresh one.
    send_on_connect_armed: Arc<std::sync::atomic::AtomicBool>,
    /// The session's current main-pane character grid, packed with
    /// `connection::responders::pack_dims`. Updated by
    /// `RuntimeAction::WindowSizeChanged` and handed to every [`Connection`] this
    /// session creates, so a connect after a resize seeds its NAWS responder with
    /// the real size. Session-lifetime (survives reloads, like the connection).
    window_size: Arc<std::sync::atomic::AtomicU32>,
    pending_buffer_updates: Vec<BufferUpdate>,
    hotkeys: BTreeMap<HotkeyId, (IsolateId, ScriptAction)>,
    next_hotkey_id: HotkeyId,
    /// Name index for script-created/disk hotkeys: maps a hotkey's `(isolate, origin, name)`
    /// key to its assigned [`HotkeyId`], so a re-`AddHotkey` upserts (unregistering the prior
    /// binding) and `RemoveHotkey`/`delete()` can find the id to unregister.
    hotkey_ids: HashMap<(IsolateId, Origin, Arc<String>), HotkeyId>,
    log_file: Option<BufWriter<File>>,
    /// Whether the plaintext screen log is enabled (seeded from settings,
    /// live-toggled via `RuntimeAction::ApplySettings`).
    log_enabled: bool,
    /// When the session log was last flushed; see [`LOG_FLUSH_INTERVAL`].
    last_log_flush: Instant,
    pending_line_operations: Rc<RefCell<Vec<LineOperation>>>,
    emitted_line_count: Rc<Cell<usize>>,
    /// Bounded ring of recently-emitted lines (UI line number + the same `Arc` the UI
    /// holds), shared into every isolate's read ops. Written by [`Self::record_emitted_line`]
    /// at emit time and by the `buffer` write-through; bounded to [`RECENT_LINES`].
    recent_lines: RecentLines,
    /// `getCurrentLocation`: the last location pushed via `SetCurrentLocation`, mirrored on
    /// the session thread and shared (the same `Rc`) into every isolate's read op. Preserved
    /// across a reload like the recent-lines ring, so a script can still read where it is after a reload.
    current_location: CurrentLocation,
    /// The pane registry, shared (the same `Rc`) into every isolate's ops. Pane ops mutate it
    /// synchronously in the op; the routing paths below validate sinks against it when
    /// queuing. Preserved across reload — panes survive script reloads.
    pane_registry: SharedPaneRegistry,
    /// Per-line gag/redirect/copy state, shared into every isolate's ops beside
    /// `pending_line_operations` and taken (cleared) once per line event.
    line_routing: SharedLineRouting,
    /// The input mirror, shared (the same `Rc`) into every isolate's input read ops.
    /// Written by the `InputStateChanged` dispatch arm; preserved across reload.
    input_mirror: SharedInputMirror,
    /// The pane-size mirror, shared (the same `Rc`) into every isolate's pane read
    /// ops. Written by the `PaneDisplayChanged` dispatch arm; preserved across
    /// reload like the pane registry itself.
    pane_size_mirror: SharedPaneSizeMirror,
    /// The in-flight typed submission slot, shared into every isolate's submission ops.
    /// Its live cell is `Some` only between the `SubmitInput` dispatch arm's `sys:input`
    /// handler splice and the `CompleteInputSubmission` that consumes it.
    input_submission: SharedInputSubmission,
    /// The completion word sets (`docs/input.md` §3.8), shared into every
    /// isolate's registry ops (which mutate/read them synchronously). The
    /// `InputWordSetsChanged` dispatch arm builds the merged view from here; the reload
    /// path resets the contributions (engine-scoped contents, like hotkeys).
    input_word_sets: SharedInputWordSets,
    /// The pane-input `onSubmit` registry (`docs/input.md` §3.7), shared into
    /// every isolate's pane ops (the registration op writes it). The `PaneInputSubmit`
    /// dispatch arm resolves submissions through it; the reload path resets it (handler
    /// addresses are engine facts, like the word sets).
    pane_input_callbacks: SharedPaneInputCallbacks,
    /// The session store, shared (the same `Rc`) into every isolate's ops. Writes journal in
    /// the ops; [`Self::flush_session_store`] commits the journal once per turn and queues the
    /// coalesced watch deliveries. The committed tree survives reloads (like `recent_lines`).
    session_store: SharedSessionStore,
    published_store: Arc<RwLock<store::PublishedStore>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    /// The runtime catalogue (`docs/interop.md` §10), shared into every isolate's
    /// ops (emit/post sampling) and snapshotted by [`Self::sync_catalogue_broadcast`]. (The
    /// message bus is engine-wired only — the run loop never touches it, so `Inner` doesn't
    /// hold it; the reload arm resets it through the thread-local handle.)
    catalogue: SharedCatalogue,
    /// The host-side GMCP producer (`docs/gmcp.md` §4): merge keys, parse memoization,
    /// and the enabled flag, driven by the `Gmcp*` dispatch arms. Session-scoped like the
    /// store subtree it writes.
    gmcp: gmcp::GmcpProducer,
    /// The host-side MSDP producer (`docs/gmcp-mapping.md` §9 item 3), driven by the
    /// `Msdp*` dispatch arms. Session-scoped like the store subtree it writes; it holds
    /// no engine facts, so reloads carry it across whole.
    msdp: msdp::MsdpProducer,
    /// The host-side MSSP producer, driven by the `MsspVariables` dispatch arm (and reset
    /// by `Connect`). Session-scoped like the snapshot it writes; no engine facts.
    mssp: mssp::MsspProducer,
    /// Whether the main buffer's tail line is open (an uncommitted partial). Replaces the
    /// old `pending_buffer_updates.last()` peek — which `AppendTo` entries would confuse —
    /// and, unlike the peek, survives a flush. Drives the echo commit-first rule and
    /// `RetractOpenLine` emission; never touched by pane deliveries.
    main_open_line: bool,
    /// Whether main has a complete, replaceable, committed, or incomplete view of the
    /// current fragmented inbound logical line.
    main_prefix_disposition: MainPrefixDisposition,
    /// Source-text bytes in partial callbacks that reached their routing step. This is
    /// independent of rendered length because a prompt trigger can transform a fragment.
    main_partial_source_len: usize,
    /// Contiguous source-text bytes represented by immutable main rows. Completion can use
    /// this boundary to recover an undisplayed tail without replaying the committed prefix.
    main_committed_source_len: usize,
    /// Server fragments in the current physical main row. This cold-path accumulator lets
    /// `buffer.line(n)` record the row that the terminal received, not an assembled logical
    /// line that may span local output.
    main_open_fragments: LineFragments,
    /// Main-routed fragments after a gap. They stay hidden until completion so local output
    /// cannot commit them out of source order; a prompt boundary releases the visible tail.
    main_deferred_fragments: LineFragments,
    /// A carriage-return replacement retired the prior main open line and is
    /// moving through triggers. Kept independently of pending UI batches so
    /// the exact transformed replacement can finish the transaction after a
    /// flush or intervening trigger output.
    replacing_main_open_line: bool,
    /// A complete logical line is running normal triggers while a transport-
    /// batch prefix remains provisionally visible. A reload can discard the
    /// local completion frame, so its abort path must retract that prefix just
    /// as it closes a carriage-return replacement transaction.
    fragmented_completion_in_flight: bool,
    /// A partial callback has queued, but not yet reached, its routing step. A reload routes
    /// this retained source before discarding the old engine's action stack.
    partial_line_in_flight: Option<Arc<StyledLine>>,
    /// The in-flight server line's transformed fragments, accumulated so a non-main sink can
    /// receive one WHOLE line at routing time (complete-line events only carry the remainder
    /// since the last partial flush). Cleared when the line completes; consumed early when a
    /// partial-line routing delivers the line-so-far.
    open_line: LineFragments,
    /// The line-structured log's current-line accumulator: main fragments buffer here and
    /// are written as one line on `EnsureNewLine`; `RetractOpenLine` discards it; routed
    /// (`AppendTo`) lines are written whole, in completion order, as they flush.
    log_open_line: Vec<u8>,
    /// File length (bytes) of newline-terminated log content — the floor a
    /// provisional open line is rewound to. An open line (a resting prompt)
    /// gets flushed to disk *provisionally* on the flush tick so an abnormal
    /// kill doesn't lose it; the next committed write truncates back to this
    /// length first, so completion/retraction rewrites cleanly.
    log_committed_len: u64,
    /// Whether a provisional (un-terminated) open line currently sits on disk
    /// past `log_committed_len`, awaiting either completion or a rewind.
    log_open_on_disk: bool,
    mapper: Option<Mapper>,
    /// Separates multiple commands on one outgoing chunk; empty disables
    /// separator splitting ('\n' always splits).
    command_separator: Arc<String>,
    /// Prefix that sends the rest of the line verbatim (no separator
    /// splitting, no alias matching); empty disables the prefix.
    raw_line_prefix: Arc<String>,
    /// Script-visible settings snapshot backing `getSettings()`, shared (the same `Rc`) into
    /// every isolate's ops. Written by the `ApplySettings` dispatch handler so a settings
    /// change (including the resolved palette) is visible to scripts without a reload.
    settings_snapshot: SettingsSnapshot,
}

impl Inner<'_> {
    /// Keep automation recording + the broadcast in step with subscribers: record only while
    /// ≥1 window is listening, and (re)send the full set whenever a new window subscribes
    /// (a broadcast can't replay, so all current watchers re-sync).
    fn sync_automation_recording(&mut self) {
        let count = self.automation_tx.receiver_count();
        if count > self.last_automation_receivers {
            let reset = self.trigger_manager.automation_reset();
            let _ = self
                .automation_tx
                .send(AutomationEvent::Reset(Arc::new(reset)));
        }
        self.last_automation_receivers = count;
        self.trigger_manager.set_recording(count > 0);
    }

    /// The catalogue twin of [`Self::sync_automation_recording`]: while a store tab is
    /// subscribed, send coalesced full snapshots on the leading-edge/trailing-coalesce
    /// cadence ([`catalogue::CATALOGUE_SEND_WINDOW`]) — the first dirty drain after a quiet
    /// spell sends immediately, dirty drains inside the window leave the dirty flag
    /// standing and arm the one-shot trailing wake, and a new subscriber always gets a
    /// fresh snapshot at once. With no subscribers nothing is built (the dirty flag just
    /// accumulates and samples defer parsing), so an unopened tab costs one
    /// `receiver_count` load per drain. Entry-budget refusal notices are echoed from here
    /// regardless of subscription — this drain owns the catalogue's one surfacing path.
    fn sync_catalogue_broadcast(&mut self) {
        let count = self.catalogue_tx.receiver_count();
        let new_subscriber = count > self.last_catalogue_receivers;
        self.last_catalogue_receivers = count;
        let subscribed = count > 0;
        let (dirty, notices) = {
            let mut catalogue = self.catalogue.borrow_mut();
            catalogue.set_subscribed(subscribed);
            (catalogue.is_dirty(), catalogue.take_refusal_notices())
        };
        for notice in notices {
            // Ride the session channel like any queued echo; the run loop picks it up on
            // the next pass.
            if self
                .session_runtime_tx
                .send(RuntimeAction::Echo(Arc::new(notice)))
                .is_err()
            {
                warn!("Dropping catalogue notice: runtime channel closed");
            }
        }
        if !subscribed {
            // Nobody listening: the dirty flag just accumulates, and the cadence needs no
            // clock read — the unopened-tab drain cost stays at loads and stores.
            self.catalogue_resend_at = None;
            return;
        }
        let now = tokio::time::Instant::now();
        match self
            .catalogue_cadence
            .on_drain(dirty, subscribed, new_subscriber, now)
        {
            CadenceDecision::SendNow => {
                let snapshot = {
                    let mut catalogue = self.catalogue.borrow_mut();
                    let _ = catalogue.take_dirty();
                    catalogue.snapshot(&self.session_store.borrow())
                };
                let _ = self
                    .catalogue_tx
                    .send(CatalogueEvent::Snapshot(Arc::new(snapshot)));
                self.catalogue_cadence.sent(now);
                self.catalogue_resend_at = None;
            }
            CadenceDecision::Defer(deadline) => {
                self.catalogue_resend_at = Some(deadline);
            }
            CadenceDecision::Idle => {
                self.catalogue_resend_at = None;
            }
        }
    }

    /// Record a freshly-emitted complete line in the recent-lines ring under its UI line number.
    /// Call this for each `BufferUpdate::Append` of a *complete* line, AFTER bumping
    /// `emitted_line_count` (its post-bump value is the line's UI number — the same number
    /// `op_smudgy_get_current_line_number` reported for it while it was in flight, and the
    /// number the UI's `TerminalBuffer` assigns). Keeps the ring bounded to [`RECENT_LINES`]
    /// by popping the oldest entry. Cost: one `Arc` clone (the bytes are shared with the UI)
    /// plus a `VecDeque` push/pop — no data duplication.
    fn record_emitted_line(&self, line: &Arc<StyledLine>) {
        let line_number = self.emitted_line_count.get();
        let mut ring = self.recent_lines.borrow_mut();
        ring.push_back((line_number, line.clone()));
        while ring.len() > RECENT_LINES {
            ring.pop_front();
        }
    }

    /// Count and record one physical row after its final main-pane update is queued.
    fn count_and_record_emitted_line(&self, line: &Arc<StyledLine>) {
        self.emitted_line_count
            .set(self.emitted_line_count.get() + 1);
        self.record_emitted_line(line);
    }

    fn reset_main_prefix_state(&mut self) {
        self.main_prefix_disposition = MainPrefixDisposition::None;
        self.main_partial_source_len = 0;
        self.main_committed_source_len = 0;
    }

    fn note_local_main_commit(&mut self) {
        if matches!(
            self.main_prefix_disposition,
            MainPrefixDisposition::Replaceable | MainPrefixDisposition::Committed
        ) {
            self.main_prefix_disposition = MainPrefixDisposition::Committed;
            self.main_committed_source_len = self.main_partial_source_len;
        }
    }

    #[cold]
    fn suffix_after_source_prefix(line: &Arc<StyledLine>, prefix_len: usize) -> Arc<StyledLine> {
        if prefix_len == 0 {
            line.clone()
        } else {
            Arc::new(line.remove(0, prefix_len))
        }
    }

    fn route_prompt_boundary(&mut self) {
        if let Some(deferred) = self.main_deferred_fragments.take_joined() {
            self.main_open_fragments.push(deferred.clone());
            self.pending_buffer_updates
                .push(BufferUpdate::Append(deferred));
            self.main_open_line = true;
        }
        self.reset_main_prefix_state();
        self.open_line.clear();
        self.pending_buffer_updates
            .push(BufferUpdate::PromptBoundary);
    }

    /// Applies all pending line **transforms** to the given line and clears the queue.
    /// Suppression/routing is not a transform (see [`LineRouting`]), so this always yields a
    /// processed line — every sink receives the fully-transformed text.
    fn apply_pending_line_operations(&self, line: Arc<StyledLine>) -> Arc<StyledLine> {
        let mut operations = self.pending_line_operations.borrow_mut();

        // If no operations are pending, return the line unchanged
        if operations.is_empty() {
            return line;
        }

        // Collect all operations and clear the queue
        let operations_to_apply: Vec<LineOperation> = operations.drain(..).collect();
        drop(operations); // Release the lock early

        // Apply each operation in sequence
        let mut current_line = line;
        for operation in operations_to_apply {
            current_line = operation.apply(&current_line);
        }

        current_line
    }

    /// Resolve taken routing state into `(main_included, pane_sinks)` for one line.
    ///
    /// The final sink set is deduplicated: main unless gagged or redirected, plus the
    /// redirect target, plus each copy target (deduped against each other, the redirect
    /// target, and main). A redirect/copy aimed at the main pane normalizes to "main
    /// included" — main delivery always keeps fragment semantics (`Append`), never
    /// `AppendTo`, so numbering parity is untouched. Sinks are validated against the live
    /// registry here, at queue time (registry mutations are synchronous), which is what lets
    /// the UI trust `AppendTo` keys; a dangling redirect fails open to main rather than
    /// destroying the line.
    fn resolve_sinks(&self, routing: &LineRouting) -> (bool, Vec<PaneKey>) {
        let registry = self.pane_registry.lock().unwrap();

        let mut redirect = routing.redirect;
        let mut redirected_to_main = false;
        if redirect == Some(MAIN_PANE_KEY) {
            redirect = None;
            redirected_to_main = true;
        }

        let mut main_included = (!routing.gag && routing.redirect.is_none()) || redirected_to_main;

        let mut sinks: Vec<PaneKey> = Vec::new();
        if let Some(key) = redirect {
            if registry.is_live(key) {
                sinks.push(key);
            } else {
                warn!("Dropping redirect to closed {key}; keeping the line on main");
                main_included = !routing.gag;
            }
        }
        for &key in &routing.copies {
            if key == MAIN_PANE_KEY {
                main_included = true;
                continue;
            }
            if sinks.contains(&key) {
                continue;
            }
            if registry.is_live(key) {
                sinks.push(key);
            } else {
                warn!("Dropping copy to closed {key}");
            }
        }
        (main_included, sinks)
    }

    /// Route one **complete** logical line: deliver the assembled whole line to every pane
    /// sink, and the transformed fragment to main (unless gagged/redirected — then retract
    /// any partial prefix already flushed to main, so neither buffer corrupts).
    ///
    /// Numbering parity is sacred here: `emitted_line_count`/`record_emitted_line` count
    /// main appends only — a redirected line is "gagged from main" (not counted, not in
    /// `recent_lines`), and `RetractOpenLine` affects only the uncommitted line.
    fn route_complete_line(&mut self, processed: Arc<StyledLine>, routing: &LineRouting) {
        let replacing_open_line = std::mem::take(&mut self.replacing_main_open_line);
        let (main_included, sinks) = if routing.is_default() {
            (true, Vec::new())
        } else {
            self.resolve_sinks(routing)
        };

        if !sinks.is_empty() {
            // Non-main sinks receive one WHOLE line: the accumulated partial
            // prefix (if any) glued to this completion fragment.
            let whole = self
                .open_line
                .take_joined_with(&processed)
                .unwrap_or_else(|| processed.clone());
            for key in &sinks {
                self.pending_buffer_updates
                    .push(BufferUpdate::AppendTo(*key, whole.clone()));
            }
        }

        if main_included {
            let recorded = if !replacing_open_line && self.main_open_line {
                self.main_open_fragments.take_joined_with(&processed)
            } else {
                self.main_open_fragments.clear();
                None
            };
            self.main_open_line = false;
            if let Some(recorded) = recorded {
                self.count_and_record_emitted_line(&recorded);
            } else {
                self.count_and_record_emitted_line(&processed);
            }
            self.pending_buffer_updates.push(if replacing_open_line {
                BufferUpdate::FinishOpenLineReplacement(Some(processed))
            } else {
                BufferUpdate::Append(processed)
            });
            self.pending_buffer_updates
                .push(BufferUpdate::EnsureNewLine);
        } else if replacing_open_line {
            self.main_open_fragments.clear();
            self.pending_buffer_updates
                .push(BufferUpdate::FinishOpenLineReplacement(None));
        } else if self.main_open_line {
            self.main_open_fragments.clear();
            self.pending_buffer_updates
                .push(BufferUpdate::RetractOpenLine);
            self.main_open_line = false;
        }

        self.reset_main_prefix_state();
        self.main_deferred_fragments.clear();
        self.open_line.clear();
    }

    /// Replace an open main tail, or append a new row, with one assembled whole line.
    fn emit_fragmented_whole_on_main(
        &mut self,
        processed: Arc<StyledLine>,
        replacing_open_line: bool,
    ) {
        if replacing_open_line {
            self.pending_buffer_updates
                .push(BufferUpdate::FinishOpenLineReplacement(Some(
                    processed.clone(),
                )));
        } else if self.main_open_line {
            self.pending_buffer_updates
                .push(BufferUpdate::BeginOpenLineReplacement);
            self.pending_buffer_updates
                .push(BufferUpdate::FinishOpenLineReplacement(Some(
                    processed.clone(),
                )));
        } else {
            self.pending_buffer_updates
                .push(BufferUpdate::Append(processed.clone()));
        }
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.main_open_line = false;
        self.main_open_fragments.clear();
        self.count_and_record_emitted_line(&processed);
    }

    /// Append a completion fragment to the current physical main row and commit that row.
    fn finish_fragmented_main_tail(
        &mut self,
        completion_fragment: Arc<StyledLine>,
        logical_fallback: &Arc<StyledLine>,
    ) {
        let recorded = self
            .main_open_fragments
            .take_joined_with(&completion_fragment)
            .unwrap_or_else(|| logical_fallback.clone());
        self.pending_buffer_updates
            .push(BufferUpdate::Append(completion_fragment));
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.main_open_line = false;
        self.count_and_record_emitted_line(&recorded);
    }

    /// Replace any provisional post-commit tail with the complete unseen source remainder.
    fn emit_fragmented_remainder_on_main(
        &mut self,
        remainder: Arc<StyledLine>,
        replacing_open_line: bool,
    ) {
        if remainder.text.is_empty() {
            if replacing_open_line {
                self.pending_buffer_updates
                    .push(BufferUpdate::FinishOpenLineReplacement(None));
            } else if self.main_open_line {
                self.pending_buffer_updates
                    .push(BufferUpdate::RetractOpenLine);
            }
            self.main_open_line = false;
            self.main_open_fragments.clear();
            return;
        }

        self.emit_fragmented_whole_on_main(remainder, replacing_open_line);
    }

    /// Route a complete logical line whose prefix was already displayed at one or more
    /// transport-batch boundaries. Normal triggers receive `processed`, the assembled whole.
    /// Main receives only unseen text when all prior fragments remain visible.
    #[cold]
    fn route_fragmented_complete_line(
        &mut self,
        original: Arc<StyledLine>,
        processed: Arc<StyledLine>,
        completion_fragment: Arc<StyledLine>,
        transformed: bool,
        preserves_committed_prefix: bool,
        routing: &LineRouting,
    ) {
        let replacing_open_line = std::mem::take(&mut self.replacing_main_open_line);
        let disposition = std::mem::take(&mut self.main_prefix_disposition);
        let (main_included, sinks) = if routing.is_default() {
            (true, Vec::new())
        } else {
            self.resolve_sinks(routing)
        };

        // Unlike the legacy completion-fragment path, the connection has
        // already assembled the exact logical whole. Pane delivery therefore
        // neither folds immutable lines nor depends on the optional routing
        // accumulator.
        for key in &sinks {
            self.pending_buffer_updates
                .push(BufferUpdate::AppendTo(*key, processed.clone()));
        }

        if main_included {
            match disposition {
                MainPrefixDisposition::Replaceable
                    if !transformed && self.main_open_line && !replacing_open_line =>
                {
                    self.finish_fragmented_main_tail(completion_fragment, &processed);
                }
                MainPrefixDisposition::Committed if !transformed => {
                    if self.main_open_line {
                        self.finish_fragmented_main_tail(completion_fragment, &processed);
                    } else {
                        self.emit_fragmented_remainder_on_main(
                            completion_fragment,
                            replacing_open_line,
                        );
                    }
                }
                MainPrefixDisposition::Committed | MainPrefixDisposition::CommittedGap => {
                    // Rows before this source boundary are immutable. If a whole-line transform
                    // preserved that source prefix, its transformed suffix is safe to show.
                    // Otherwise use the original suffix: never replay committed terminal rows.
                    let committed_len = self.main_committed_source_len;
                    let remainder = if transformed && preserves_committed_prefix {
                        Self::suffix_after_source_prefix(&processed, committed_len)
                    } else {
                        Self::suffix_after_source_prefix(&original, committed_len)
                    };
                    self.emit_fragmented_remainder_on_main(remainder, replacing_open_line);
                }
                // A whole-line transform needs the assembled source. An incomplete prefix
                // needs the same fallback so main does not silently lose server text.
                MainPrefixDisposition::None
                | MainPrefixDisposition::Replaceable
                | MainPrefixDisposition::Incomplete => {
                    self.emit_fragmented_whole_on_main(processed, replacing_open_line);
                }
            }
        } else if replacing_open_line {
            self.main_open_fragments.clear();
            self.pending_buffer_updates
                .push(BufferUpdate::FinishOpenLineReplacement(None));
        } else if self.main_open_line {
            self.main_open_fragments.clear();
            self.pending_buffer_updates
                .push(BufferUpdate::RetractOpenLine);
            self.main_open_line = false;
        } else {
            self.main_open_fragments.clear();
        }

        self.reset_main_prefix_state();
        self.main_deferred_fragments.clear();
        self.open_line.clear();
    }

    /// Route one **partial** (prompt) fragment. A redirect/copy decided on a partial routes
    /// the line-so-far the same way a complete line would; delivering to a pane consumes the
    /// accumulator, so a later routing on the same line's completion delivers only the
    /// remainder (never duplicated text).
    fn finish_partial_line_before_reload(&mut self) {
        let Some(line) = self.partial_line_in_flight.take() else {
            return;
        };

        self.script_engine.set_current_line(None);
        let source_len = line.text.len();
        let processed = self.apply_pending_line_operations(line);
        let routing = self.line_routing.borrow_mut().take();
        self.route_partial_line(processed, source_len, &routing);
    }

    fn route_partial_line(
        &mut self,
        processed: Arc<StyledLine>,
        source_len: usize,
        routing: &LineRouting,
    ) {
        self.main_partial_source_len += source_len;
        let replacing_open_line = std::mem::take(&mut self.replacing_main_open_line);
        if replacing_open_line {
            self.main_open_fragments.clear();
        }
        if routing.is_default() {
            // Fast path: no routing on this fragment. The whole-line
            // accumulator exists only to feed pane sinks, so with no non-main
            // panes it is dead weight. With panes, accumulation stores `Arc`s
            // and flattens at most once rather than copying the growing prefix
            // per fragment. A stale accumulator can't be consumed (sinks
            // require live panes) and is cleared at completion.
            if self.pane_registry.lock().unwrap().has_non_main_panes() {
                self.open_line.push(processed.clone());
            } else {
                self.open_line.clear();
            }

            // Once routing hides a source fragment, showing later partials could let local
            // output commit them ahead of that gap. Defer them until the logical line ends.
            if self.main_prefix_disposition.defers_partial_main() {
                self.main_deferred_fragments.push(processed);
                if replacing_open_line {
                    self.main_open_fragments.clear();
                    self.pending_buffer_updates
                        .push(BufferUpdate::FinishOpenLineReplacement(None));
                } else if self.main_open_line {
                    self.main_open_fragments.clear();
                    self.pending_buffer_updates
                        .push(BufferUpdate::RetractOpenLine);
                    self.main_open_line = false;
                }
                return;
            }

            self.main_open_fragments.push(processed.clone());
            self.main_prefix_disposition.note_visible_partial();
            self.pending_buffer_updates.push(if replacing_open_line {
                BufferUpdate::FinishOpenLineReplacement(Some(processed))
            } else {
                BufferUpdate::Append(processed)
            });
            self.main_open_line = true;
            return;
        }

        // Routing decided on this partial: assemble the whole line so far so
        // the pane sink receives it as one line.
        self.open_line.push(processed.clone());
        let accumulated = self
            .open_line
            .take_joined()
            .expect("the just-pushed partial must be present");

        let (main_included, sinks) = self.resolve_sinks(routing);

        if sinks.is_empty() {
            self.open_line.push(accumulated);
        } else {
            for key in &sinks {
                self.pending_buffer_updates
                    .push(BufferUpdate::AppendTo(*key, accumulated.clone()));
            }
            // Consumed: the delivered prefix never re-routes.
            self.open_line.clear();
        }

        if main_included && !self.main_prefix_disposition.defers_partial_main() {
            self.main_open_fragments.push(processed.clone());
            self.main_prefix_disposition.note_visible_partial();
            self.pending_buffer_updates.push(if replacing_open_line {
                BufferUpdate::FinishOpenLineReplacement(Some(processed))
            } else {
                BufferUpdate::Append(processed)
            });
            self.main_open_line = true;
        } else if main_included {
            self.main_deferred_fragments.push(processed);
            if replacing_open_line {
                self.main_open_fragments.clear();
                self.pending_buffer_updates
                    .push(BufferUpdate::FinishOpenLineReplacement(None));
            } else if self.main_open_line {
                self.pending_buffer_updates
                    .push(BufferUpdate::RetractOpenLine);
                self.main_open_line = false;
            }
        } else if replacing_open_line {
            self.main_deferred_fragments.clear();
            self.main_open_fragments.clear();
            self.main_prefix_disposition.note_hidden_partial();
            self.pending_buffer_updates
                .push(BufferUpdate::FinishOpenLineReplacement(None));
        } else if self.main_open_line {
            self.main_deferred_fragments.clear();
            self.main_open_fragments.clear();
            self.main_prefix_disposition.note_hidden_partial();
            self.pending_buffer_updates
                .push(BufferUpdate::RetractOpenLine);
            self.main_open_line = false;
        } else {
            self.main_deferred_fragments.clear();
            self.main_open_fragments.clear();
            self.main_prefix_disposition.note_hidden_partial();
        }
    }

    /// Drop the main buffer's open (uncommitted) partial line: a
    /// carriage-return overprint superseded it and the replacement frame is
    /// on its way. Same rule as the gag/redirect retraction — only the
    /// uncommitted line is affected, so numbering parity holds — plus the
    /// routing accumulator is cleared so the stale frame never re-routes or
    /// reaches a pane sink. A no-op when nothing is open.
    pub(crate) fn retract_incoming_open_line_sync(&mut self) {
        if self.main_open_line {
            self.pending_buffer_updates
                .push(BufferUpdate::BeginOpenLineReplacement);
            self.main_open_line = false;
            self.replacing_main_open_line = true;
        }
        self.reset_main_prefix_state();
        self.main_open_fragments.clear();
        self.main_deferred_fragments.clear();
        self.open_line.clear();
    }

    /// Abandon an incoming-line pipeline that will never reach its normal
    /// `*LineTriggersProcessed` completion action.
    ///
    /// In particular, a delivered [`BufferUpdate::BeginOpenLineReplacement`]
    /// owns a UI-side detached-line transaction. Every abandoned pipeline must
    /// pair it with an empty finish before any unrelated line can be routed.
    /// Trigger state is per-line too, so discard transforms/routing alongside
    /// the replacement and pane accumulator instead of leaking them into the
    /// next server line.
    fn abort_incoming_line_sync(&mut self) {
        self.script_engine.set_current_line(None);
        self.pending_line_operations.borrow_mut().clear();
        self.line_routing.borrow_mut().take();
        self.open_line.clear();
        self.partial_line_in_flight.take();
        self.reset_main_prefix_state();
        self.main_deferred_fragments.clear();
        if std::mem::take(&mut self.fragmented_completion_in_flight) && self.main_open_line {
            self.pending_buffer_updates
                .push(BufferUpdate::RetractOpenLine);
            self.main_open_line = false;
            self.main_open_fragments.clear();
        }
        if std::mem::take(&mut self.replacing_main_open_line) {
            self.pending_buffer_updates
                .push(BufferUpdate::FinishOpenLineReplacement(None));
            self.main_open_fragments.clear();
        }
    }

    /// If the main buffer's tail line is open (an uncommitted partial), commit it: the
    /// committed line takes the next number. Echo paths call this so an echo never glues
    /// onto an open prompt line; the send paths deliberately do NOT (the echoed command
    /// gluing onto the prompt is classic MUD-client behavior).
    #[inline]
    fn commit_open_main_line(&mut self) {
        if self.main_open_line {
            let recorded = self.main_open_fragments.take_joined();
            self.pending_buffer_updates
                .push(BufferUpdate::EnsureNewLine);
            self.emitted_line_count
                .set(self.emitted_line_count.get() + 1);
            if let Some(line) = recorded {
                self.record_emitted_line(&line);
            }
            self.main_open_line = false;
            self.note_local_main_commit();
        }
    }

    /// Append one whole line to the main buffer with the numbering bookkeeping every
    /// counted echo path shares: the Append + EnsureNewLine pair, the emitted-line
    /// count, and the recent-lines ring record.
    #[inline]
    fn append_counted_line(&mut self, styled_line: Arc<StyledLine>) {
        self.main_open_fragments.clear();
        self.pending_buffer_updates
            .push(BufferUpdate::Append(styled_line.clone()));
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.count_and_record_emitted_line(&styled_line);
    }

    #[inline]
    fn echo_warn_str_sync(&mut self, line: &str) {
        self.commit_open_main_line();

        for line in line.split('\n') {
            self.append_counted_line(Arc::new(StyledLine::from_warn_str(line)));
        }
    }

    fn echo_warn_str<'s>(
        &'s mut self,
        line: &str,
    ) -> Result<Option<SentSessionEvent<'s>>, anyhow::Error> {
        self.echo_warn_str_sync(line);
        self.flush_buffer_updates()
    }

    #[inline]
    fn echo_str_sync(&mut self, line: &str) {
        self.commit_open_main_line();

        for line in line.split('\n') {
            self.append_counted_line(Arc::new(StyledLine::from_echo_str(line)));
        }
    }

    fn echo_str<'s>(
        &'s mut self,
        line: &str,
    ) -> Result<Option<SentSessionEvent<'s>>, anyhow::Error> {
        self.echo_str_sync(line);
        self.flush_buffer_updates()
    }

    /// The styled-echo sibling of [`Self::echo_str_sync`]: each element is already one
    /// whole on-screen line (the op boundary split on `\n` and built the spans), so this
    /// appends them counted, exactly like a plain echo's lines.
    #[inline]
    fn echo_styled_lines_sync(&mut self, lines: &[Arc<StyledLine>]) {
        self.commit_open_main_line();

        for styled_line in lines {
            self.append_counted_line(styled_line.clone());
        }
    }

    async fn send(&mut self, line: &str) -> Result<(), anyhow::Error> {
        let mut socket_str = String::with_capacity(line.len() + 2);
        socket_str.push_str(line);
        socket_str.push_str("\r\n");
        let arc_socket_str = Arc::new(socket_str);

        if let Some(ref connection) = self.connection
            && let Err(error) = connection.write(arc_socket_str).await
        {
            warn!("Error writing to connection: {error:?}");
            if let Some(future) = self.echo_warn_str(format!("Send error: {error:?}").as_str())? {
                future.await?;
            }
            return Ok(());
        }

        let styled_line = Arc::new(StyledLine::from_output_str(line));

        let recorded_line = if self.main_open_line {
            self.note_local_main_commit();
            self.main_open_fragments.take_joined_with(&styled_line)
        } else {
            self.main_open_fragments.clear();
            None
        };

        // Deliberately no commit-first: an echoed command gluing onto an open
        // prompt line is classic MUD-client behavior. The EnsureNewLine below
        // commits whatever line it lands on.
        self.pending_buffer_updates
            .push(BufferUpdate::Append(styled_line.clone()));
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.main_open_line = false;
        if let Some(recorded_line) = recorded_line {
            self.count_and_record_emitted_line(&recorded_line);
        } else {
            self.count_and_record_emitted_line(&styled_line);
        }

        if let Some(future) = self.flush_buffer_updates()? {
            future.await?;
        }
        Ok(())
    }

    /// Like [`Self::send`], but the copy echoed to the client view and written to
    /// the session log has each secret substring masked. The server still receives
    /// the unmodified `line` (the secret reaches the wire, never the screen/log).
    async fn send_with_redactions(
        &mut self,
        line: &str,
        redactions: &[String],
    ) -> Result<(), anyhow::Error> {
        let mut socket_str = String::with_capacity(line.len() + 2);
        socket_str.push_str(line);
        socket_str.push_str("\r\n");
        let arc_socket_str = Arc::new(socket_str);

        if let Some(ref connection) = self.connection
            && let Err(error) = connection.write(arc_socket_str).await
        {
            warn!("Error writing to connection: {error:?}");
            if let Some(future) = self.echo_warn_str(format!("Send error: {error:?}").as_str())? {
                future.await?;
            }
            return Ok(());
        }

        let display = redact(line, redactions);
        let styled_line = Arc::new(StyledLine::from_output_str(&display));

        let recorded_line = if self.main_open_line {
            self.note_local_main_commit();
            self.main_open_fragments.take_joined_with(&styled_line)
        } else {
            self.main_open_fragments.clear();
            None
        };

        self.pending_buffer_updates
            .push(BufferUpdate::Append(styled_line.clone()));
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.main_open_line = false;
        if let Some(recorded_line) = recorded_line {
            self.count_and_record_emitted_line(&recorded_line);
        } else {
            self.count_and_record_emitted_line(&styled_line);
        }

        if let Some(future) = self.flush_buffer_updates()? {
            future.await?;
        }
        Ok(())
    }

    /// Flush the session store's write journal: commit this turn's writes to the host tree and
    /// queue the coalesced watch deliveries at the back of the main action queue (each runs as
    /// its own turn on a later pump, like async-continuation actions).
    ///
    /// Called once per run-loop iteration, right after the script-engine pump. That point is
    /// the end of a turn's JS — the pump drained the microtasks of whatever ran last, whether a
    /// dispatched action or an async continuation — and it precedes the next action dispatch,
    /// which is what makes the cross-isolate happens-before hold: if A writes then emits, the
    /// subscriber's `CallJavascriptFunction` is dispatched only after this flush, so it reads
    /// the committed value.
    fn flush_session_store(&mut self) {
        if !self.session_store.borrow().has_pending_writes() {
            return;
        }
        for action in self.session_store.borrow_mut().flush() {
            if self.session_runtime_tx.send(action).is_err() {
                warn!("Dropping session-store watch delivery: runtime channel closed");
            }
        }
        let (published, writes) = {
            let store = self.session_store.borrow();
            (
                Arc::new(store.published()),
                Arc::new(store.last_published_writes()),
            )
        };
        *self.published_store.write().unwrap() = published.as_ref().clone();
        for runtime in registry::get_runtimes_for_server(self.server_name.as_str()) {
            if runtime
                .tx
                .send(RuntimeAction::RemoteStoreFlushed {
                    source: self.session_id,
                    published: Arc::clone(&published),
                    writes: Arc::clone(&writes),
                })
                .is_err()
            {
                warn!(
                    "Dropping directed state flush for session {}",
                    runtime.session_id
                );
            }
        }
        // The committed tree changed; a subscribed store tab needs a fresh snapshot at the
        // next drain (`sync_catalogue_broadcast` — the flag is cheap, the snapshot is not
        // built here).
        self.catalogue.borrow_mut().mark_dirty();
        // The flush wrote widget-binding cells: wake the UI so render closures re-read them
        // (`docs/interop.md` §7 — repaints without a V8 tick). `try_send` because
        // this is a sync path; on a full channel the wake is safely elided — the queued
        // events that filled it already force the same redraw when the UI drains them.
        if self.session_store.borrow_mut().take_bindings_changed()
            && let Err(e) = self.ui_tx.try_send(TaggedSessionEvent {
                session_id: self.session_id,
                event: SessionEvent::StoreBindingsChanged,
            })
            && !e.is_full()
        {
            warn!("Failed to send store-bindings wake: {e:?}");
        }
    }

    /// Between-actions bookkeeping, run every time the action stack drains —
    /// before the next external action is taken, whether or not the loop is
    /// about to park — so the broadcast cadences observe every turn even
    /// mid-burst. Keeps automation recording in step with subscribers (and
    /// re-sends the full set to any newly-attached window), flushes buffered
    /// deltas to the automation broadcast as one coalesced batch, and honors
    /// the catalogue send window's leading/trailing edges. Everything here is
    /// cheap on the idle path: receiver-count loads and empty checks.
    ///
    /// Also deallocates the previous generations the turn's store flushes
    /// displaced (`SessionStore::flush` parks them instead of dropping
    /// inline): here the action stack is empty and the flush's deliveries are
    /// already queued, so a whole delta's worth of blocks returns to the
    /// allocator off the dispatch critical path. Bounded at one root per
    /// producer that committed since the last drain.
    fn drain_point_bookkeeping(&mut self) {
        self.sync_automation_recording();
        if self.trigger_manager.has_automation_deltas() {
            let deltas = self.trigger_manager.take_automation_deltas();
            let _ = self
                .automation_tx
                .send(AutomationEvent::Changed(Arc::new(deltas)));
        }
        self.sync_catalogue_broadcast();
        self.session_store.borrow_mut().drop_retired_generations();
    }

    fn flush_buffer_updates(&mut self) -> Result<Option<SentSessionEvent<'_>>, anyhow::Error> {
        if self.pending_buffer_updates.is_empty() {
            return Ok(None);
        }

        if let Some(log_file) = self.log_file.as_mut() {
            // Line-structured transcript: main fragments accumulate in
            // `log_open_line` and are written as one line on commit; routed
            // (`AppendTo`) lines are written whole, in completion order —
            // where a linear byte replay of the multiplexed queue would
            // interleave pane text into main's open line. The transcript is
            // the union of all sinks, unattributed; fully-gagged lines never
            // appear here at all (no update is queued for them).
            //
            // Any provisional open line written to disk on a prior flush tick
            // (for crash durability) is rewound before committed content so
            // completion/retraction rewrites cleanly.
            for update in &self.pending_buffer_updates {
                match update {
                    BufferUpdate::Append(line)
                    | BufferUpdate::FinishOpenLineReplacement(Some(line)) => {
                        self.log_open_line.extend_from_slice(line.as_bytes());
                    }
                    BufferUpdate::EnsureNewLine => {
                        if self.log_open_on_disk {
                            rewind_provisional_open_line(log_file, self.log_committed_len)?;
                            self.log_open_on_disk = false;
                        }
                        log_file.write_all(&self.log_open_line)?;
                        log_file.write_all(b"\n")?;
                        #[allow(clippy::cast_possible_truncation)]
                        {
                            self.log_committed_len += self.log_open_line.len() as u64 + 1;
                        }
                        self.log_open_line.clear();
                    }
                    BufferUpdate::PromptBoundary => {}
                    BufferUpdate::AppendTo(_, line) => {
                        if self.log_open_on_disk {
                            rewind_provisional_open_line(log_file, self.log_committed_len)?;
                            self.log_open_on_disk = false;
                        }
                        let bytes = line.as_bytes();
                        log_file.write_all(bytes)?;
                        log_file.write_all(b"\n")?;
                        self.log_committed_len += bytes.len() as u64 + 1;
                    }
                    // The retracted prefix re-appears inside the routed whole
                    // line, so dropping the accumulator here is what keeps
                    // the transcript free of duplicated text.
                    BufferUpdate::RetractOpenLine | BufferUpdate::BeginOpenLineReplacement => {
                        if self.log_open_on_disk {
                            rewind_provisional_open_line(log_file, self.log_committed_len)?;
                            self.log_open_on_disk = false;
                        }
                        self.log_open_line.clear();
                    }
                    BufferUpdate::FinishOpenLineReplacement(None) => {}
                    // Display-only; the transcript keeps everything.
                    BufferUpdate::Clear(_) => {}
                }
            }
            if self.last_log_flush.elapsed() >= LOG_FLUSH_INTERVAL {
                // Persist an open line (a resting prompt) provisionally so an
                // abnormal kill — force-close, WM_ENDSESSION exit, V8 abort —
                // doesn't lose it; the next committed write rewinds it first.
                if !self.log_open_on_disk && !self.log_open_line.is_empty() {
                    log_file.write_all(&self.log_open_line)?;
                    self.log_open_on_disk = true;
                }
                log_file.flush()?;
                self.last_log_flush = Instant::now();
            }
        } else {
            // No log: don't let the accumulator grow unbounded.
            self.log_open_line.clear();
        }

        Ok(Some(self.ui_tx.send(TaggedSessionEvent {
            session_id: self.session_id,
            event: SessionEvent::UpdateBuffer(Arc::new(
                self.pending_buffer_updates.drain(..).collect(),
            )),
        })))
    }

    pub async fn run(&mut self) -> RunAction {
        // The UI subscription is the session's lifetime owner. It may be
        // dropped while engine construction is still blocking, before the UI
        // receives RuntimeReady and can send an explicit Shutdown. The stream
        // guard queues that shutdown, while this check prevents any startup
        // actions from running against an already-disconnected event sink.
        if self.ui_tx.is_closed() {
            info!("Session event receiver closed during startup; stopping runtime");
            return RunAction::None;
        }

        let mut script_engine_tick_interval = ScriptEngine::tick_interval();

        // Stack-based action processing
        let mut action_stack: Vec<VecDeque<RuntimeAction>> = Vec::new();
        const MAX_STACK_DEPTH: usize = 100;

        // The logs directory may legitimately be missing; a logging failure
        // must never kill the session.
        if self.log_enabled
            && let Err(err) = self.start_logging()
        {
            warn!("Failed to start session logging: {err:?}");
        }

        info!(
            "Session [{}, {} - {}] Started",
            self.session_id, self.server_name, self.profile_name
        );

        // Bounded like Phase 1 below; hoisted so the start-up pre-drain can share it.
        const MAX_DENO_ITERS: usize = 16;

        // Pre-drain: pump the script engine for the work it scheduled while loading modules and
        // packages — chiefly, surface uncaught exceptions from a package's *top-level* code right
        // here, adjacent to its "Loaded N packages" line, instead of letting them fall out of the
        // first event-loop pump far below (after the maps load, where they'd read as unrelated).
        // Unlike Phase 1, this deliberately keeps draining *past* an error so EVERY broken
        // package's exception surfaces at start-up, not just the first; deno reports each
        // unhandled rejection once, and the MAX_DENO_ITERS bound is the backstop against an
        // isolate that somehow errors on every pump spinning session start.
        std::future::poll_fn(|cx| {
            let mut drained = 0;
            while drained < MAX_DENO_ITERS {
                drained += 1;
                match self.script_engine.poll_event_loop(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Ok(())) => {}
                    Poll::Ready(Err(err)) => {
                        warn!("Error in script engine event loop: {err:?}");
                        self.echo_warn_str_sync(&script_engine::format_script_error(&err));
                    }
                }
            }
            Poll::Ready(())
        })
        .await;

        // Always load: the mapper's local tier serves maps with no credential
        // (and `list_areas` swallows cloud auth errors), so a signed-out
        // session still loads its local maps. Cloud maps join via the sync
        // engine once the user logs in.
        if let Some(mapper) = self.mapper.clone() {
            self.echo_str_sync("Loading maps...");
            let started = Instant::now();
            match mapper.load_all_areas().await {
                Ok(summary) => {
                    let elapsed = started.elapsed();
                    // The per-area detail (id/rev/source/timing) is invaluable when one map is
                    // misbehaving, but 150+ lines of it buries the session start — keep it in the
                    // logs and echo a single summary to the screen.
                    for stat in &summary.areas {
                        debug!(
                            "Loaded map area: {} ({}) rev {} | load={}ms | source={}",
                            stat.name,
                            stat.area_id,
                            stat.revision,
                            stat.load_duration.as_millis(),
                            stat.source
                        );
                    }
                    let total = summary.areas.len();
                    if total == 0 {
                        self.echo_str_sync("No maps to load.");
                    } else {
                        let shared = summary.areas.iter().filter(|s| s.shared).count();
                        let owned = total - shared;
                        let breakdown = if shared > 0 {
                            format!(" ({owned} owned, {shared} shared)")
                        } else {
                            String::new()
                        };
                        self.echo_str_sync(&format!(
                            "Loaded {total} map area{}{breakdown} in {}ms.",
                            if total == 1 { "" } else { "s" },
                            elapsed.as_millis()
                        ));
                    }
                }
                Err(e) if e.is_auth_error() => {
                    self.echo_warn_str_sync(
                        "Maps are unavailable. Sign in or create a smudgy account to use this feature.",
                    );
                }
                Err(e) => {
                    self.echo_warn_str_sync(&format!("Failed to load maps: {e}"));
                }
            }
        }

        // Flush any lines buffered during start-up (the "Loading maps..."/"Loaded N map area(s)"
        // notices, plus any pre-drain script-error warnings) so they paint immediately. This
        // path runs on EVERY `run()`, including the reload that a package install triggers; a
        // reload reuses the existing connection and emits no `RuntimeAction::Echo`/`Connected`,
        // so without this the buffered lines would sit in `pending_buffer_updates` until the next
        // inbound socket byte drives a `RequestRepaint` flush — the ~10s "only updates when
        // unrelated network data arrives" lag. A no-op when nothing is buffered.
        match self.flush_buffer_updates() {
            Ok(Some(fut)) => {
                if let Err(e) = fut.await {
                    warn!("Failed to flush start-up buffer updates: {e:?}");
                }
            }
            Ok(None) => {}
            Err(e) => warn!("Failed to flush start-up buffer updates: {e:?}"),
        }

        // Module/package top-level code emits registration and echo actions into
        // `spawned_actions` while the engine is constructed. Keep those in a startup frame and
        // announce RuntimeReady only after the frame (including its depth-first descendants) has
        // drained. Previously RuntimeReady was sent here, before the normal loop dispatched that
        // work, so immediate socket input could beat trigger/state-watch registrations.
        action_stack.push(self.spawned_actions.borrow_mut().drain(..).collect());
        let mut runtime_ready_pending = true;

        info!("Starting session event loop");

        loop {
            // A receiver can disappear between any two actions. Stop after the
            // first failed delivery instead of leaving a registry-visible
            // runtime that repeatedly executes against a dead UI channel.
            if self.ui_tx.is_closed() {
                info!("Session event receiver closed; stopping runtime");
                break;
            }

            let mut deno_iters = 0;
            // Phase 1: Poll script engine until no more immediate work is available
            std::future::poll_fn(|cx| {
                loop {
                    match self.script_engine.poll_event_loop(cx) {
                        Poll::Ready(Ok(())) => {
                            deno_iters += 1;

                            if deno_iters < MAX_DENO_ITERS {
                                continue;
                            }

                            return Poll::Ready(());
                        }
                        Poll::Ready(Err(err)) => {
                            warn!("Error in script engine event loop: {err:?}");
                            self.echo_warn_str_sync(&script_engine::format_script_error(&err));
                            return Poll::Ready(());
                        }
                        Poll::Pending => {
                            // No more work available right now, continue to action processing
                            return Poll::Ready(());
                        }
                    }
                }
            })
            .await;

            // Phase 1.5: Anything scripts emitted from async continuations
            // (timers, resolved promises) has no position in any in-flight
            // expansion; treat it like new input at the back of the main
            // queue.
            {
                let mut async_spawned = self.spawned_actions.borrow_mut();
                for action in async_spawned.drain(..) {
                    trace!("Queueing async script action: {action:?}");
                    if self.session_runtime_tx.send(action).is_err() {
                        warn!("Dropping async script action: runtime channel closed");
                    }
                }
            }

            // The turn's JS is done (the Phase 1 pump drained its microtasks): flush the session
            // store journal BEFORE the next action is picked, so every dispatch — including the
            // subscriber calls an emit just queued — observes the writes that happened before it
            // (`docs/interop.md` §2, flush-before-dispatch). After Phase 1.5, which
            // moves no JS, so the writing turn's own queued actions (its echoes, its emits'
            // subscriber calls) reach the main queue AHEAD of the watch deliveries this flush
            // appends — a delivery never overtakes the turn that caused it.
            self.flush_session_store();

            // Phase 2: Get next action to process
            let action = if let Some(current_frame) = action_stack.last_mut() {
                if let Some(spawned_action) = current_frame.pop_front() {
                    // Process next spawned action
                    trace!("Handling spawned action: {spawned_action:?}");
                    Some(spawned_action)
                } else {
                    // Current frame is empty, pop it and continue
                    action_stack.pop();
                    if runtime_ready_pending {
                        debug_assert!(action_stack.is_empty());
                        runtime_ready_pending = false;
                        if let Err(e) = self
                            .ui_tx
                            .send(TaggedSessionEvent {
                                session_id: self.session_id,
                                event: SessionEvent::RuntimeReady(self.session_runtime_tx.clone()),
                            })
                            .await
                        {
                            error!("Failed to send runtime ready event: {e:?}");
                        }
                    }
                    trace!(
                        "Completed action frame, stack depth: {}",
                        action_stack.len()
                    );
                    continue;
                }
            } else if let Ok(external_action) = self.session_runtime_rx.try_recv() {
                // More external input is already queued: a socket burst queues one
                // `HandleIncomingLine` per line, so between them the stack is empty
                // without the loop being anywhere near parking. Run the between-actions
                // bookkeeping, then take the next action WITHOUT the before-park flush —
                // that skip is what lets a burst's lines coalesce into batched UI events
                // (bounded by the storm threshold below and the reader's per-read-batch
                // `RequestRepaint`) instead of one awaited UI event per line.
                self.drain_point_bookkeeping();
                trace!("Handling external action: {external_action:?}");
                Some(external_action)
            } else {
                self.drain_point_bookkeeping();
                // About to park: flush any still-buffered lines so they paint now instead of
                // waiting for the next wake. Anything buffered at this point has already been
                // fully drained of in-flight actions, so this can't split a coalesced batch — it
                // only rescues lines that would otherwise be stuck until the next socket byte. A
                // no-op (no event) when nothing is buffered.
                match self.flush_buffer_updates() {
                    Ok(Some(fut)) => {
                        if let Err(e) = fut.await {
                            warn!("Failed to flush buffered output before idle: {e:?}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => warn!("Failed to flush buffered output before idle: {e:?}"),
                }
                // No spawned actions: park until external input arrives OR an isolate's event
                // loop has work (resolved promise / elapsed timer / async module load). The
                // readiness branch re-registers the engine's waker with THIS task each idle poll,
                // so a completion wakes us straight back into Phase 1 — no 100us tick needed.
                let engine = &mut self.script_engine;
                let rx = &mut self.session_runtime_rx;
                // Trailing edge of the catalogue broadcast window: armed only while a dirty
                // snapshot was deferred inside the window (`sync_catalogue_broadcast`), so a
                // burst's final state lands within ~33 ms instead of at the safety tick. A
                // transient one-shot — the Doc-A readiness/tick contract above is untouched.
                let catalogue_resend_at = self.catalogue_resend_at;
                select! {
                    biased;
                    Some(external_action) = rx.recv() => {
                        trace!("Handling external action: {external_action:?}");
                        Some(external_action)
                    }
                    // Resolves the moment any isolate makes progress; Phase 1 then drains/handles it.
                    () = std::future::poll_fn(|cx| match engine.poll_event_loop(cx) {
                        Poll::Ready(_) => Poll::Ready(()),
                        Poll::Pending => Poll::Pending,
                    }) => {
                        trace!("Readiness branch: isolate made progress, re-entering Phase 1");
                        // Yield once before re-entering Phase 1 so a perpetually-`Ready` isolate
                        // (a hot microtask/timer loop, or a script erroring on every poll) cannot
                        // busy-spin this current-thread runtime and starve tasks spawned onto it
                        // (deno op tasks and timers; the socket reader runs on its own runtime).
                        // The branch only resolves on an actual wake, so this
                        // never affects idle parking; it bounds a pathological spin to one extra
                        tokio::task::yield_now().await;
                        continue;
                    }
                    // The catalogue's trailing-edge wake: re-enter the loop so the drain
                    // point sends the deferred snapshot (now past the window's edge).
                    () = tokio::time::sleep_until(
                        catalogue_resend_at.unwrap_or_else(tokio::time::Instant::now),
                    ), if catalogue_resend_at.is_some() => { continue; }
                    // Slow safety net only.
                    _ = script_engine_tick_interval.tick() => { continue; }
                }
            };

            // Phase 3: Process the action if we have one
            if let Some(action) = action {
                let result = self.handle_action(action).await;

                // Actions emitted synchronously by scripts and triggers while
                // handling this action execute next, in emission order, ahead
                // of siblings already queued behind it (depth-first
                // expansion). An explicit Run result (e.g. command splitting,
                // a script's return value) executes after those emissions.
                let mut spawned: Vec<RuntimeAction> =
                    self.spawned_actions.borrow_mut().drain(..).collect();

                match result {
                    Ok(ActionResult::None) => {}
                    Ok(ActionResult::Echo(line)) => {
                        // Append only; the storm-threshold flush below or the
                        // before-park flush delivers it.
                        self.echo_str_sync(line.as_str());
                    }
                    Ok(ActionResult::CloseSession) => {
                        info!(
                            "Session [{}, {} - {}] Closing",
                            self.session_id, self.server_name, self.profile_name
                        );
                        close_runtime_action_queue(&mut self.session_runtime_rx);
                        break;
                    }
                    Ok(ActionResult::Reload) => {
                        // The local action stack is discarded by this return. If a
                        // trigger queued Reload ahead of its line-completion action,
                        // close the replacement transaction now rather than letting
                        // the rebuilt runtime finish the next unrelated server line.
                        self.finish_partial_line_before_reload();
                        if self.replacing_main_open_line || self.fragmented_completion_in_flight {
                            self.abort_incoming_line_sync();
                        }
                        return RunAction::Reload;
                    }
                    Ok(ActionResult::Run(actions)) => {
                        spawned.extend(actions);
                    }
                    Err(err) => {
                        warn!("Error in runtime: {err:?}");
                        self.echo_str_sync(format!("Error in runtime: {err:?}").as_str());
                    }
                }

                // Storm threshold: a long dispatch cascade (an alias echoing tens of
                // thousands of lines, a trigger storm) appends without flushing, so
                // bound the batch — paint stays incremental and no single UI event
                // balloons. Everything below the threshold coalesces into the
                // before-park flush at the drain point.
                if self.pending_buffer_updates.len() >= PENDING_UPDATE_FLUSH_THRESHOLD {
                    match self.flush_buffer_updates() {
                        Ok(Some(fut)) => {
                            if let Err(e) = fut.await {
                                warn!("Failed to flush storm-threshold buffer updates: {e:?}");
                            }
                        }
                        Ok(None) => {}
                        Err(e) => warn!("Failed to flush storm-threshold buffer updates: {e:?}"),
                    }
                }

                if !spawned.is_empty() {
                    if let Some(current_frame) = action_stack.last_mut() {
                        // Splice ahead of queued siblings for depth-first order
                        for spawned_action in spawned.into_iter().rev() {
                            current_frame.push_front(spawned_action);
                        }
                        trace!("Spliced spawned actions into current frame");
                    } else if action_stack.len() >= MAX_STACK_DEPTH {
                        warn!("Maximum action stack depth exceeded: {MAX_STACK_DEPTH}");
                        self.echo_str_sync("Error: Maximum execution depth exceeded");
                    } else {
                        action_stack.push(VecDeque::from(spawned));
                        trace!(
                            "Pushed new action frame, stack depth: {}",
                            action_stack.len()
                        );
                    }
                }
            }
        }

        RunAction::None
    }

    fn start_logging(&mut self) -> Result<()> {
        let path = get_smudgy_home()?
            .join(self.server_name.as_str())
            .join("logs")
            .join(format!(
                "{}-{}.log",
                self.profile_name,
                chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
            ));
        self.log_file = Some(BufWriter::with_capacity(65536, File::create(path)?));
        self.last_log_flush = Instant::now();
        // A fresh file starts empty with no provisional open line on disk.
        self.log_committed_len = 0;
        self.log_open_on_disk = false;
        Ok(())
    }

    /// Live-applies a logging toggle. Enabling starts a fresh timestamped log
    /// file (same semantics as a session reload); disabling flushes and drops
    /// the current one. No-op when the state already matches.
    fn set_log_enabled(&mut self, enabled: bool) {
        if enabled == self.log_enabled {
            return;
        }
        self.log_enabled = enabled;
        if enabled {
            if let Err(err) = self.start_logging() {
                warn!("Failed to start session logging: {err:?}");
            }
        } else {
            self.flush_log();
            self.log_file = None;
        }
    }

    /// Flushes the session log, surfacing (but swallowing) any error. Drains the
    /// current-line accumulator first (without a newline) so teardown paths —
    /// disconnect, reload, logging toggled off — don't lose an open line's text;
    /// a later commit writes only the fragments accumulated after this point.
    fn flush_log(&mut self) {
        if let Some(log_file) = self.log_file.as_mut() {
            // Rewind any provisional open line already on disk so this final
            // write doesn't duplicate it.
            if self.log_open_on_disk {
                if let Err(err) = rewind_provisional_open_line(log_file, self.log_committed_len) {
                    warn!("Failed to rewind the provisional log line: {err:?}");
                }
                self.log_open_on_disk = false;
            }
            if !self.log_open_line.is_empty() {
                if let Err(err) = log_file.write_all(&self.log_open_line) {
                    warn!("Failed to write the open line to the session log: {err:?}");
                }
                self.log_open_line.clear();
            }
            if let Err(err) = log_file.flush() {
                warn!("Failed to flush session log: {err:?}");
            }
            self.last_log_flush = Instant::now();
        }
    }
}
