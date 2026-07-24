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
    sync::{Arc, Mutex},
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
pub use trigger::{Manager, MatchCapture, PushTriggerParams, SharedAutomationRegistry};
pub mod catalogue;
pub mod input;
pub mod line_operation;
mod message_bus;
pub mod pane;
mod gmcp;
mod msdp;
mod script_action;
mod script_engine;
mod store;

use catalogue::{CadenceDecision, CatalogueCadence, CatalogueEvent, RuntimeCatalogue};
pub(crate) use catalogue::SharedCatalogue;
use input::InputMirror;
pub(crate) use input::{
    SharedInputMirror, SharedInputSubmission, SharedInputWordSets, SharedPaneInputCallbacks,
};
use line_operation::LineOperation;
use message_bus::MessageBus;
pub(crate) use message_bus::SharedMessageBus;
use pane::{PaneKey, PaneRegistry, MAIN_PANE_KEY};

pub use script_action::ScriptAction;
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
pub use store::{
    BudgetExceeded, PathError, PlatformProducer, ProducerKey, SessionStore, SetOutcome,
    StoreBudgets, StorePath, Usage, WatchCadence,
};
#[cfg(feature = "bench-api")]
pub use script_engine::FunctionId;
pub(crate) use store::SharedSessionStore;

use crate::get_smudgy_home;
use crate::models::settings::load_settings;
use crate::session::{HotkeyId, PackageProviderFactory, ScriptExtensionFactory, registry};

use super::{SessionId, TaggedSessionEvent, connection::Connection, styled_line::StyledLine};

use super::{BufferUpdate, SessionEvent};
use futures::{SinkExt, channel::mpsc::Sender};
mod action;
mod dispatch;
mod origin;

pub use action::RuntimeAction;
pub use origin::{
    AutomationBody, AutomationDelta, AutomationEvent, AutomationKind, AutomationSummary, IsolateId,
    Origin,
    SingletonKey, SingletonOrigin, SingletonRegistry,
};
pub(crate) use action::{ActionQueue, ActionResult, RunAction};

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

/// The session's pane registry, shared (the same `Rc`) into every isolate's
/// ops so pane ops mutate it synchronously in the op (get-or-create is
/// race-free locally, and `const p = pane.split(...); line.redirect(p)` works
/// within one trigger body). Preserved across script reloads exactly like
/// [`RecentLines`], which is what makes "panes survive script reloads" true.
pub(crate) type SharedPaneRegistry = Rc<RefCell<PaneRegistry>>;

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

#[cfg(test)]
mod redact_tests {
    use super::redact;

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
}

static RUNTIME_THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// # Panics
///
/// Panics if the `RUNTIME_THREADS` mutex is poisoned, or if a joined runtime
/// thread itself panicked.
pub fn join_runtime_threads() {
    let mut runtime_threads = RUNTIME_THREADS.lock().unwrap();
    while let Some(join_handle) = runtime_threads.pop() {
        join_handle.join().unwrap();
    }
}

type SentSessionEvent<'a> =
    futures::sink::Send<'a, Sender<TaggedSessionEvent>, TaggedSessionEvent>;

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
    /// Panics if the current-thread tokio runtime fails to build, or if the
    /// `RUNTIME_THREADS` mutex is poisoned.
    pub fn new(
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
        ui_tx: Sender<TaggedSessionEvent>,
    ) -> Self {
        let (session_runtime_tx, session_runtime_rx) =
            tokio::sync::mpsc::unbounded_channel::<RuntimeAction>();

        let local_session_runtime_tx = session_runtime_tx.clone();

        let local_server_name = server_name.clone();
        let local_profile_name = profile_name.clone();
        let local_ui_tx = ui_tx.clone();
        let (automation_tx, _) =
            broadcast::channel::<AutomationEvent>(AUTOMATION_BROADCAST_CAPACITY);
        let local_automation_tx = automation_tx.clone();
        let (catalogue_tx, _) = broadcast::channel::<CatalogueEvent>(CATALOGUE_BROADCAST_CAPACITY);
        let local_catalogue_tx = catalogue_tx.clone();

        let thread = thread::spawn(move || {
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

            // The pane registry: pane ops mutate it synchronously via `OpState`; preserved
            // across reload (like `recent_lines`) so panes survive an engine rebuild.
            let pane_registry: SharedPaneRegistry = Rc::new(RefCell::new(PaneRegistry::new()));

            // Per-line routing state (gag/redirect/copy), cleared per line event; shared into
            // every isolate's ops beside `pending_line_operations`.
            let line_routing: SharedLineRouting = Rc::new(RefCell::new(LineRouting::default()));

            // The input mirror (`docs/input.md` §3.3): read synchronously by every
            // isolate's input ops, written by the `InputStateChanged` dispatch arm. Session-
            // scoped (survives reload) like the pane registry — interest is a session fact.
            let input_mirror: SharedInputMirror = Rc::new(RefCell::new(InputMirror::default()));

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
            let input_word_sets: SharedInputWordSets =
                Rc::new(RefCell::new(input::InputWordSets::default()));

            // The pane-input onSubmit registry (`docs/input.md` §3.7): written by
            // the registration op, resolved by the `PaneInputSubmit` dispatch arm. Session-
            // scoped cell, engine-scoped contents — handlers name functions of the engine
            // that registered them, so the reload path below resets it like the word sets.
            let pane_input_callbacks: SharedPaneInputCallbacks =
                Rc::new(RefCell::new(input::PaneInputCallbacks::default()));

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
            let settings_snapshot: SettingsSnapshot =
                Rc::new(RefCell::new(crate::models::settings::ScriptSettings::from(
                    &load_settings(),
                )));

            let spawned_actions: ActionQueue = Rc::new(RefCell::new(VecDeque::new()));

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
                spawned_actions: spawned_actions.clone(),
                pending_line_operations: &pending_line_operations,
                emitted_line_count: Rc::downgrade(&emitted_line_count),
                recent_lines: recent_lines.clone(),
                current_location: current_location.clone(),
                settings_snapshot: settings_snapshot.clone(),
                pane_registry: pane_registry.clone(),
                line_routing: line_routing.clone(),
                input_mirror: input_mirror.clone(),
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
            });

            // Seed runtime-relevant settings from disk; the UI live-updates
            // them later via `RuntimeAction::ApplySettings`.
            let settings = load_settings();
            let command_separator = Arc::new(settings.command_separator);

            let trigger_manager = Manager::new(
                spawned_actions.clone(),
                command_separator.clone(),
                automation_registry,
            );

            let mut inner = Inner {
                log_file: None,
                log_enabled: settings.logging.enabled,
                last_log_flush: Instant::now(),
                session_id,
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
                automation_tx: local_automation_tx.clone(),
                last_automation_receivers: 0,
                catalogue_tx: local_catalogue_tx.clone(),
                last_catalogue_receivers: 0,
                catalogue_cadence: CatalogueCadence::default(),
                catalogue_resend_at: None,
                connection: None,
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
                input_submission: input_submission.clone(),
                input_word_sets: input_word_sets.clone(),
                pane_input_callbacks: pane_input_callbacks.clone(),
                session_store: session_store.clone(),
                catalogue: catalogue.clone(),
                gmcp: gmcp::GmcpProducer::new(gmcp_enabled.clone()),
                msdp: msdp::MsdpProducer::new(),
                // The spawn-time "Loading session..." append left the main
                // buffer's tail line open — unless an engine-construction
                // session notice (emitted directly on ui_tx, each ending in
                // EnsureNewLine) already committed it, which the notice's
                // count bump records.
                main_open_line: emitted_line_count.get() == 0,
                open_line: None,
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
                // (a reload can land mid-server-line).
                let old_main_open_line = inner.main_open_line;
                let old_open_line = inner.open_line.take();
                let old_connection = inner.connection.take();
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

                runtime.block_on(async move {
                    drop(inner);
                });

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
                let word_set_resyncs = input_word_sets.borrow_mut().reset_engine_state();

                // Pane-input onSubmit handlers are engine facts too: their function ids
                // index the disposed isolates' registries. Drop them all; the reloading
                // scripts re-register theirs beside their re-claiming splits, and a pane
                // nobody re-claims is closed by the sweep queued below anyway.
                pane_input_callbacks.borrow_mut().reset_engine_state();

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
                pane_registry.borrow_mut().begin_claim_epoch();

                let new_script_engine = ScriptEngine::new(ScriptEngineParams {
                    session_id,
                    server_name: &local_server_name,
                    ui_tx: local_ui_tx.clone(),
                    spawned_actions: spawned_actions.clone(),
                    pending_line_operations: &pending_line_operations,
                    emitted_line_count: Rc::downgrade(&emitted_line_count),
                    recent_lines: recent_lines.clone(),
                    current_location: current_location.clone(),
                    settings_snapshot: settings_snapshot.clone(),
                    pane_registry: pane_registry.clone(),
                    line_routing: line_routing.clone(),
                    input_mirror: input_mirror.clone(),
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

                let mut new_trigger_manager = Manager::new(
                    spawned_actions.clone(),
                    command_separator.clone(),
                    automation_registry,
                );
                new_trigger_manager.adopt_raw_wanted_flag(old_raw_wanted);

                // Replace with the new inner struct
                inner = Inner {
                    log_file: None, // Will restart logging
                    log_enabled: settings.logging.enabled,
                    last_log_flush: Instant::now(),
                    session_id,
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
                    automation_tx: local_automation_tx.clone(),
                    last_automation_receivers: 0,
                    catalogue_tx: local_catalogue_tx.clone(),
                    last_catalogue_receivers: 0,
                    catalogue_cadence: CatalogueCadence::default(),
                    catalogue_resend_at: None,
                    connection: old_connection, // Preserve the connection
                    window_size: old_window_size,
                    pending_buffer_updates: Vec::new(),
                    pending_line_operations: pending_line_operations.clone(), // Preserve the shared operations
                    emitted_line_count: emitted_line_count.clone(),
                    recent_lines: recent_lines.clone(), // Preserve the recent-lines ring across reload
                    current_location: current_location.clone(), // Preserve current location across reload
                    pane_registry: pane_registry.clone(), // Panes survive script reloads
                    line_routing: line_routing.clone(),
                    input_mirror: input_mirror.clone(), // Mirror + interest survive reload
                    input_submission: input_submission.clone(), // Cleared above; the cell itself is session-scoped
                    input_word_sets: input_word_sets.clone(), // Contributions reset above; the cell itself is session-scoped
                    pane_input_callbacks: pane_input_callbacks.clone(), // Handlers reset above; the cell itself is session-scoped
                    session_store: session_store.clone(), // Committed store state survives reload
                    catalogue: catalogue.clone(),         // Samples are session history
                    gmcp: old_gmcp, // Session-scoped: enabled tracks the surviving connection
                    msdp: old_msdp, // Same: server facts, no engine facts
                    main_open_line: old_main_open_line
                        && emitted_line_count.get() == count_before_rebuild,
                    open_line: old_open_line,
                    log_open_line: Vec::new(), // The reload flushed the old log; the new file starts a fresh line
                    log_committed_len: 0, // A new log file is opened on reconnect
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
            let runtime = Rc::try_unwrap(runtime).unwrap_or_else(|runtime| {
                panic!(
                    "session Tokio runtime still has {} owners after Inner teardown",
                    Rc::strong_count(&runtime)
                )
            });
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);

            info!("Runtime thread shutting down");
        });

        RUNTIME_THREADS.lock().unwrap().push(thread);

        Self {
            session_id,
            server_name,
            profile_name,
            profile_subtext,
            ui_tx,
            tx: session_runtime_tx,
            automation_tx,
            catalogue_tx,
        }
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
    trigger_manager: trigger::Manager,
    script_engine: ScriptEngine<'a>,
    server_name: &'a Arc<String>,
    profile_name: &'a Arc<String>,
    session_runtime_rx: UnboundedReceiver<RuntimeAction>,
    session_runtime_tx: UnboundedSender<RuntimeAction>,
    spawned_actions: ActionQueue,
    ui_tx: Sender<TaggedSessionEvent>,
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
    /// Whether the main buffer's tail line is open (an uncommitted partial). Replaces the
    /// old `pending_buffer_updates.last()` peek — which `AppendTo` entries would confuse —
    /// and, unlike the peek, survives a flush. Drives the echo commit-first rule and
    /// `RetractOpenLine` emission; never touched by pane deliveries.
    main_open_line: bool,
    /// The in-flight server line's transformed fragments, accumulated so a non-main sink can
    /// receive one WHOLE line at routing time (complete-line events only carry the remainder
    /// since the last partial flush). Cleared when the line completes; consumed early when a
    /// partial-line routing delivers the line-so-far.
    open_line: Option<Arc<StyledLine>>,
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
            let _ = self.automation_tx.send(AutomationEvent::Reset(Arc::new(reset)));
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
        let registry = self.pane_registry.borrow();

        let mut redirect = routing.redirect;
        let mut redirected_to_main = false;
        if redirect == Some(MAIN_PANE_KEY) {
            redirect = None;
            redirected_to_main = true;
        }

        let mut main_included =
            (!routing.gag && routing.redirect.is_none()) || redirected_to_main;

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
        let (main_included, sinks) = if routing.is_default() {
            (true, Vec::new())
        } else {
            self.resolve_sinks(routing)
        };

        if !sinks.is_empty() {
            // Non-main sinks receive one WHOLE line: the accumulated partial
            // prefix (if any) glued to this completion fragment.
            let whole = match self.open_line.as_ref() {
                Some(prefix) => Arc::new(prefix.append(&processed)),
                None => processed.clone(),
            };
            for key in &sinks {
                self.pending_buffer_updates
                    .push(BufferUpdate::AppendTo(*key, whole.clone()));
            }
        }

        if main_included {
            self.main_open_line = false;
            self.emitted_line_count
                .set(self.emitted_line_count.get() + 1);
            self.record_emitted_line(&processed);
            self.pending_buffer_updates
                .push(BufferUpdate::Append(processed));
            self.pending_buffer_updates
                .push(BufferUpdate::EnsureNewLine);
        } else if self.main_open_line {
            self.pending_buffer_updates
                .push(BufferUpdate::RetractOpenLine);
            self.main_open_line = false;
        }

        self.open_line = None;
    }

    /// Route one **partial** (prompt) fragment. A redirect/copy decided on a partial routes
    /// the line-so-far the same way a complete line would; delivering to a pane consumes the
    /// accumulator, so a later routing on the same line's completion delivers only the
    /// remainder (never duplicated text).
    fn route_partial_line(&mut self, processed: Arc<StyledLine>, routing: &LineRouting) {
        if routing.is_default() {
            // Fast path: no routing on this fragment. The whole-line
            // accumulator exists only to feed pane sinks, so with no non-main
            // panes it is dead weight — skip the per-fragment deep copy
            // (`StyledLine::append`) entirely. A stale accumulator can't be
            // consumed (sinks require live panes) and is cleared at completion.
            if self.pane_registry.borrow().has_non_main_panes() {
                self.open_line = Some(match self.open_line.take() {
                    Some(prev) => Arc::new(prev.append(&processed)),
                    None => processed.clone(),
                });
            } else {
                self.open_line = None;
            }
            self.pending_buffer_updates
                .push(BufferUpdate::Append(processed));
            self.main_open_line = true;
            return;
        }

        // Routing decided on this partial: assemble the whole line so far so
        // the pane sink receives it as one line.
        let accumulated = match self.open_line.take() {
            Some(prev) => Arc::new(prev.append(&processed)),
            None => processed.clone(),
        };

        let (main_included, sinks) = self.resolve_sinks(routing);

        if sinks.is_empty() {
            self.open_line = Some(accumulated);
        } else {
            for key in &sinks {
                self.pending_buffer_updates
                    .push(BufferUpdate::AppendTo(*key, accumulated.clone()));
            }
            // Consumed: the delivered prefix never re-routes.
            self.open_line = None;
        }

        if main_included {
            self.pending_buffer_updates
                .push(BufferUpdate::Append(processed));
            self.main_open_line = true;
        } else if self.main_open_line {
            self.pending_buffer_updates
                .push(BufferUpdate::RetractOpenLine);
            self.main_open_line = false;
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
                .push(BufferUpdate::RetractOpenLine);
            self.main_open_line = false;
        }
        self.open_line = None;
    }

    /// If the main buffer's tail line is open (an uncommitted partial), commit it: the
    /// committed line takes the next number. Echo paths call this so an echo never glues
    /// onto an open prompt line; the send paths deliberately do NOT (the echoed command
    /// gluing onto the prompt is classic MUD-client behavior).
    #[inline]
    fn commit_open_main_line(&mut self) {
        if self.main_open_line {
            self.pending_buffer_updates
                .push(BufferUpdate::EnsureNewLine);
            self.emitted_line_count
                .set(self.emitted_line_count.get() + 1);
            self.main_open_line = false;
        }
    }

    /// Append one whole line to the main buffer with the numbering bookkeeping every
    /// counted echo path shares: the Append + EnsureNewLine pair, the emitted-line
    /// count, and the recent-lines ring record.
    #[inline]
    fn append_counted_line(&mut self, styled_line: Arc<StyledLine>) {
        self.pending_buffer_updates
            .push(BufferUpdate::Append(styled_line.clone()));
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.emitted_line_count
            .set(self.emitted_line_count.get() + 1);
        self.record_emitted_line(&styled_line);
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

    fn send<'s>(&'s mut self, line: &str) -> Result<Option<SentSessionEvent<'s>>, anyhow::Error> {
        let mut socket_str = String::with_capacity(line.len() + 2);
        socket_str.push_str(line);
        socket_str.push_str("\r\n");
        let arc_socket_str = Arc::new(socket_str);

        let styled_line = Arc::new(StyledLine::from_output_str(line));

        // Deliberately no commit-first: an echoed command gluing onto an open
        // prompt line is classic MUD-client behavior. The EnsureNewLine below
        // commits whatever line it lands on.
        self.pending_buffer_updates
            .push(BufferUpdate::Append(styled_line.clone()));
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.main_open_line = false;

        self.emitted_line_count
            .set(self.emitted_line_count.get() + 1);
        self.record_emitted_line(&styled_line);

        if let Some(ref connection) = self.connection
            && let Err(e) = connection.write(arc_socket_str) {
                warn!("Error writing to connection: {e:?}");
                self.echo_warn_str(format!("Send error: {e:?}").as_str())?;
            }

        self.flush_buffer_updates()
    }

    /// Like [`Self::send`], but the copy echoed to the client view and written to
    /// the session log has each secret substring masked. The server still receives
    /// the unmodified `line` (the secret reaches the wire, never the screen/log).
    fn send_with_redactions<'s>(
        &'s mut self,
        line: &str,
        redactions: &[String],
    ) -> Result<Option<SentSessionEvent<'s>>, anyhow::Error> {
        let mut socket_str = String::with_capacity(line.len() + 2);
        socket_str.push_str(line);
        socket_str.push_str("\r\n");
        let arc_socket_str = Arc::new(socket_str);

        let display = redact(line, redactions);
        let styled_line = Arc::new(StyledLine::from_output_str(&display));

        self.pending_buffer_updates
            .push(BufferUpdate::Append(styled_line.clone()));
        self.pending_buffer_updates
            .push(BufferUpdate::EnsureNewLine);
        self.main_open_line = false;

        self.emitted_line_count
            .set(self.emitted_line_count.get() + 1);
        self.record_emitted_line(&styled_line);

        if let Some(ref connection) = self.connection
            && let Err(e) = connection.write(arc_socket_str) {
                warn!("Error writing to connection: {e:?}");
                self.echo_warn_str(format!("Send error: {e:?}").as_str())?;
            }

        self.flush_buffer_updates()
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

    fn flush_buffer_updates(
        &mut self,
    ) -> Result<Option<SentSessionEvent<'_>>, anyhow::Error> {
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
                    BufferUpdate::Append(line) => {
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
                    BufferUpdate::RetractOpenLine => {
                        if self.log_open_on_disk {
                            rewind_provisional_open_line(log_file, self.log_committed_len)?;
                            self.log_open_on_disk = false;
                        }
                        self.log_open_line.clear();
                    }
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
                                event: SessionEvent::RuntimeReady(
                                    self.session_runtime_tx.clone(),
                                ),
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
                        break;
                    }
                    Ok(ActionResult::Reload) => {
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
