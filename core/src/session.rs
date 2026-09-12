use derive_more::{Add, Display, From, Into};
use futures::Stream;
use runtime::RuntimeAction;
use smudgy_cloud::{AreaId, AtlasId, Mapper};
use std::{
    fmt::Debug,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use styled_line::StyledLine;
use system_row::SystemRow;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    models::hotkeys::HotkeyDefinition,
    session::runtime::input::InputOp,
    session::runtime::line_operation::LineOperation,
    session::runtime::pane::{
        PaneDef, PaneKey, PanePlacement, PaneScrollRequest, SplitDirection, TabPosition,
    },
};

pub mod config;
pub mod connection;
pub mod registry;
pub mod runtime;
pub mod styled_line;
pub mod system_row;
pub mod ui_command;

#[derive(From, Into, Display, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Add)]
#[repr(transparent)]
pub struct SessionId(u32);

#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// The runtime has finished loading modules/packages and dispatching their synchronous
    /// setup actions, so callers may immediately send input without racing registrations.
    RuntimeReady(UnboundedSender<RuntimeAction>),
    Connected,
    Disconnected,
    UpdateBuffer(Arc<Vec<BufferUpdate>>),
    ClearHotkeys,
    RegisterHotkey(HotkeyId, HotkeyDefinition),
    UnregisterHotkey(HotkeyId),
    PerformLineOperation {
        line_number: usize,
        operation: Box<LineOperation>,
    },
    SetCurrentLocation(AreaId, Option<i32>),
    /// A mapper navigation op resolved a destination in this area (speedwalk /
    /// find-nearest) — the UI daemon treats it as demonstrated navigation
    /// intent for per-server map scoping (bind-on-use). Advisory; no state
    /// change beyond the daemon's scope bookkeeping.
    MapperNavigated(AreaId),
    /// A room is already mapped on a *different* server entry — the daemon
    /// raises the cross-entry "show here too?" rescue offer for its atlas
    /// (checked before the auto-mapper mints ephemeral rooms).
    OfferMapRescue {
        area_id: AreaId,
        atlas_id: Option<AtlasId>,
        atlas_name: Option<String>,
    },
    /// A script created a non-ephemeral (cloud-tier) area; the daemon
    /// associates it with this session's server entry.
    MapAreaCreated(AreaId),
    /// A script created or promoted a cloud atlas; the daemon associates it
    /// with this session's server entry.
    MapAtlasCreated(AtlasId),
    /// A pane was created in this session's registry. Travels the same
    /// ordered channel as buffer updates, so the UI always sees the open
    /// before the first `AppendTo` for the key. `placement` tells the hosting
    /// window where to split the new pane in.
    PaneOpened {
        def: PaneDef,
        placement: PanePlacement,
    },
    /// A pane was closed. Emitted *after* flushing any buffered updates, so
    /// it arrives behind every `AppendTo` that preceded it — a UI-side
    /// `AppendTo` miss is therefore a bug (warn and drop), never a race.
    PaneClosed(PaneKey),
    /// The ordered UI command already removed this pane from its layout. This
    /// event trails every preceding `AppendTo` and retires display state.
    PaneClosedOrdered(PaneKey),
    /// An existing pane's def changed in place — a def-state field
    /// (`title_bar`, `hidden`, `font_size`) via a `split()` naming an
    /// existing pane with an explicit field, the `hide`/`show`/`setFontSize`
    /// ops, or the echo of a user eyeball toggle. Pure display-state
    /// refresh: no placement, no buffer implications. The UI applies the
    /// whole def (idempotent for the window whose eyeball click originated
    /// it).
    PaneUpdated(PaneDef),
    /// A script asked to resize a pane (`pane.resize`, panes.md placement
    /// commands): adjust the nearest ancestor divider on each given axis to
    /// make the pane `width`/`height` px, writing the edge back to a
    /// script-owned px sizing (a later user drag re-owns it). Best-effort: a
    /// pane spanning the full extent of an axis no-ops on that axis; a
    /// retired key drops the event whole (warn, like a missed `AppendTo`).
    PaneResize {
        key: PaneKey,
        width: Option<f32>,
        height: Option<f32>,
    },
    /// A script changed a terminal pane's vertical viewport.
    PaneScroll {
        key: PaneKey,
        request: PaneScrollRequest,
    },
    /// A script asked to move a pane next to `reference` (`pane.relocate`):
    /// the synthetic version of a manual drop — re-parent the pane's leaf on
    /// the `direction` side of the reference in whichever window hosts it,
    /// with an optional initial extent like a split's. Cross-window moves
    /// (including re-dock from a torn-out window) ride the same path.
    PaneRelocate {
        key: PaneKey,
        reference: PaneKey,
        direction: SplitDirection,
        size_px: Option<f32>,
    },
    PaneGroupWith {
        key: PaneKey,
        reference_session: SessionId,
        reference: PaneKey,
        position: TabPosition,
        selected: bool,
    },
    PaneSelect {
        key: PaneKey,
    },
    /// A script asked to move a pane into a fresh dedicated window
    /// (`pane.tearOut`): the drag tear-out flow minus the drag. `width`/
    /// `height` size the new window (floored by the window minimum); omitted
    /// dimensions default to ~the pane's current rect plus the toolbar band.
    PaneTearOut {
        key: PaneKey,
        width: Option<f32>,
        height: Option<f32>,
    },
    /// Atomically exchange this pane's layout leaf with another pane's leaf.
    /// Pane payloads move; the destination split geometry stays in place.
    PaneSwap {
        key: PaneKey,
        other_session: SessionId,
        other_key: PaneKey,
    },
    /// A session-store flush updated widget-binding cells
    /// (`docs/interop.md` §7). Pure repaint wake: the cells already
    /// hold the new values and the widget render closures read them lock-free,
    /// so the UI needs no state change — processing the message redraws the view.
    StoreBindingsChanged,
    /// A sandboxed package successfully prepared its first online Web Audio
    /// context for this isolate generation. The UI records the versionless
    /// package observation in `smudgy.lock.json` and may then expose its
    /// package-specific gain row. Best-effort and advisory: playback itself
    /// never waits for persistence.
    PackageAudioUsed {
        owner: Arc<str>,
        name: Arc<str>,
    },
    /// A lazy script-link tooltip resolved into the shared cell held by the
    /// rendered line. Pure repaint wake; no UI-owned state changes.
    LinkTooltipChanged,
    /// Apply one scripted input mutation to the input of pane `key`
    /// (`docs/input.md` §3.4). Travels the ordered channel, so ops
    /// land in the order scripts issued them.
    InputOp {
        key: PaneKey,
        op: InputOp,
    },
    /// The session thread has flagged input-mirror interest: start sending
    /// `RuntimeAction::InputStateChanged` on input changes, and push the
    /// current state immediately so the mirror warms up.
    InputMirrorInterest,
    /// The session thread has flagged pane-size-mirror interest (a `pane.size`
    /// read or a `pane:resize` subscription): start sending
    /// `RuntimeAction::PaneDisplayChanged` on settled pane layout changes, and
    /// push every pane's current size immediately so the mirror warms up.
    PaneMirrorInterest,
    /// A script asked for this session's window footprint to be saved as
    /// the named layout (`layout.save`). The daemon owns the capture and
    /// the per-server store; the name arrives already validated.
    LayoutSave {
        name: String,
    },
    /// A script asked for a named layout to be applied (`layout.apply`).
    /// Layout-only: the daemon rebinds what exists, scoped to this
    /// session's server, and never spawns, closes, prompts, or touches OS
    /// windows.
    LayoutApply {
        name: String,
    },
    /// The merged completion word sets for the input of pane `key`
    /// (`docs/input.md` §3.8): every creator's registered
    /// suggestions in merge order (creators in first-contribution order,
    /// words in insertion order, deduplicated case-insensitively) and the
    /// union blacklist (lowercase-folded; blacklist filtering is
    /// case-insensitive). Replaces the UI's previous copy for that input
    /// wholesale — Tab completion consults this beside the scrollback scan.
    InputWordSets {
        key: PaneKey,
        suggestions: Arc<Vec<Arc<String>>>,
        blacklist: Arc<std::collections::HashSet<String>>,
    },
    /// The names of every enabled Command-kind alias, sorted and deduplicated,
    /// replaced wholesale whenever the alias set changes. Host-derived (not a
    /// script word set): the main input offers these on Tab in *command
    /// position* — where everything between the caret's word start and the
    /// nearest boundary (start of input, or an occurrence of the command
    /// separator) is whitespace — ahead of the registered suggestion sets and
    /// the scrollback scan. The `InputWordSets` blacklist filters these too.
    CommandNames {
        names: Arc<Vec<Arc<String>>>,
    },
    /// The server's `observed.json` sidecar was rewritten (a successful
    /// connect stamped `last_connected_at`, or a fresh MSSP payload merged).
    /// A pure refresh nudge for whatever displays observed server state (the
    /// Connect screen's metadata band): no payload — the consumer re-reads
    /// the file, so this message is the only signal and no file watching is
    /// ever needed.
    ObservedServerChanged,
    /// A plain connection's server advertised a usable TLS port and the whole
    /// upgrade-offer guard set passed (unencrypted transport; a sane port
    /// distinct from the one dialed; a non-IP-literal dialed host; any
    /// advertised `HOSTNAME` naming that host; no persisted refusal; first
    /// offer this connection). The UI shows the in-session offer banner:
    /// accepting persists `tls` + the port to `server.json` and reconnects,
    /// declining persists the per-server refusal in `observed.json`.
    /// Advisory like all MSSP — nothing changes without the user's explicit
    /// choice, and the offer dies with the connection (a plain dismissal
    /// persists nothing, so the next connect may re-offer).
    OfferTlsUpgrade {
        port: u16,
    },
    /// The server's telnet ECHO state (RFC 857): `enabled` means the server
    /// has taken over echoing — the classic password-prompt signal — and the
    /// UI should mask the main input (subject to the user's auto-mask
    /// preference); `false` releases that mask. The telnet cause composes
    /// with a script-set mask UI-side: the input stays masked while either
    /// is active (`docs/input.md` §3.10). Also sent with `false` on
    /// disconnect, since the option dies with the connection.
    ServerEcho {
        enabled: bool,
    },
}
#[derive(Debug, Clone)]
pub struct TaggedSessionEvent {
    pub session_id: SessionId,
    pub event: SessionEvent,
}
/// Factory for extra deno extensions the embedder wants installed in every
/// script engine (e.g. the UI's JSX bridge). Called once per engine
/// construction, including on session reload, and once *per isolate*, since the
/// engine owns an isolate set (`script/PACKAGE-ISOLATES.md`).
pub type ScriptExtensionFactory = Arc<dyn Fn() -> Vec<deno_core::Extension> + Send + Sync>;

/// Hook the embedder supplies to reset state of its own that is coupled to one script-engine
/// generation. Called on the session thread immediately before every engine construction —
/// the initial build and each reload — after the previous engine's isolates (if any) are
/// disposed and before any module code runs. The UI uses this to clear its mounted script
/// widgets: their render closures hold `v8::Global` callbacks minted by the isolates that
/// just died, and the reloading modules re-mount theirs into the fresh engine.
pub type EngineResetHook = Arc<dyn Fn() + Send + Sync>;

/// Factory for an alternate `smudgy://` package provider, invoked on the session thread at
/// engine construction. The default session path builds the cloud-backed provider from
/// `package_client`; this seam lets a caller supply a different resolver (the sandboxed-
/// isolate integration tests inject an in-memory provider so a real second isolate can be
/// spawned without the cloud backend). Mirrors [`ScriptExtensionFactory`]: the provider is
/// `!Send`, so it can't cross the spawn boundary directly — the `Send + Sync` factory builds
/// it in place. See [`spawn_with_package_provider`].
pub type PackageProviderFactory =
    Arc<dyn Fn() -> std::rc::Rc<dyn smudgy_script::PackageProvider> + Send + Sync>;

#[cfg(feature = "web-audio")]
pub(crate) type RuntimeAudioScope = smudgy_audio_web::SessionAudioScope;
#[cfg(not(feature = "web-audio"))]
pub(crate) type RuntimeAudioScope = ();

/// An explicit audio scope did not belong to the session being spawned.
#[cfg(feature = "web-audio")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("session {session_id} cannot use audio scope for session {audio_session_id}")]
pub struct SessionAudioScopeMismatch {
    /// Core session requested by the caller.
    pub session_id: SessionId,
    /// Numeric mixer session carried by the opaque audio scope.
    pub audio_session_id: u64,
}

/// Typed failure for the UI's transactional audio-session spawn boundary.
#[cfg(feature = "web-audio")]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AudioSessionSpawnError {
    /// The supplied Web Audio scope belongs to another session.
    #[error(transparent)]
    Scope(#[from] SessionAudioScopeMismatch),
    /// The operating system refused to create the runtime thread.
    #[error(transparent)]
    RuntimeThread(#[from] runtime::RuntimeThreadSpawnError),
    /// The spawned worker could not be published and carries its cleanup result.
    #[error(transparent)]
    RuntimePublication(#[from] runtime::RuntimeThreadPublicationError),
}

#[derive(Debug, thiserror::Error)]
enum SessionRuntimeSpawnError {
    #[error(transparent)]
    Thread(#[from] runtime::RuntimeThreadSpawnError),
    #[error(transparent)]
    Publication(#[from] runtime::RuntimeThreadPublicationError),
}

pub struct SessionParams {
    pub session_id: SessionId,
    pub server_name: Arc<String>,
    pub profile_name: Arc<String>,
    pub profile_subtext: Arc<String>,
    pub mapper: Option<Mapper>,
    /// Cloud client for `smudgy://` package resolution (`None` = disabled).
    pub package_client: Option<smudgy_cloud::PackageApiClient>,
    pub extra_script_extensions: ScriptExtensionFactory,
    /// Reset hook for embedder state coupled to one engine generation (see [`EngineResetHook`]);
    /// `None` when the embedder holds no such state (tests, headless).
    pub on_engine_rebuild: Option<EngineResetHook>,
}

/// The UI subscription owns a session runtime's lifetime. Dropping the
/// subscription can happen before [`SessionEvent::RuntimeReady`] gives the UI
/// its normal control channel, so the stream itself retains a shutdown sender.
/// Without this guard, a session closed while its scripts were still loading
/// remained in the global registry with no hosted pane or event receiver.
struct SessionEventStream {
    events: futures::channel::mpsc::Receiver<TaggedSessionEvent>,
    shutdown_tx: UnboundedSender<RuntimeAction>,
}

impl Stream for SessionEventStream {
    type Item = TaggedSessionEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.events).poll_next(cx)
    }
}

impl Drop for SessionEventStream {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(RuntimeAction::Shutdown);
    }
}

impl Debug for SessionParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionParams")
            .field("session_id", &self.session_id)
            .field("server_name", &self.server_name)
            .field("profile_name", &self.profile_name)
            .field("profile_subtext", &self.profile_subtext)
            .finish_non_exhaustive()
    }
}

#[derive(Display, Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct HotkeyId(usize);

impl std::hash::Hash for SessionParams {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.session_id.hash(state);
    }
}

#[derive(Debug)]
pub enum BufferUpdate {
    /// Text for the main buffer. Fragment semantics: a logical line may
    /// arrive as several appends glued by the UI, terminated by
    /// [`BufferUpdate::EnsureNewLine`].
    Append(Arc<StyledLine>),
    /// Begin a carriage-return replacement of the main buffer's open line.
    /// The matching [`BufferUpdate::FinishOpenLineReplacement`] may arrive in
    /// a later UI batch and may have unrelated trigger output before it.
    BeginOpenLineReplacement,
    /// Finish the carriage-return replacement started by
    /// [`BufferUpdate::BeginOpenLineReplacement`]. `Some` supplies the exact
    /// replacement frame after triggers/transforms; `None` means routing
    /// gagged or redirected it away from main.
    FinishOpenLineReplacement(Option<Arc<StyledLine>>),
    /// Commit the main buffer's open line.
    EnsureNewLine,
    /// A telnet GA/EOR prompt boundary. Carries no text; terminal panes use it
    /// to advance OSC 8 visibility expiry without conflating partial socket
    /// flushes with real prompts.
    PromptBoundary,
    /// One WHOLE line for a non-main pane. Routing is decided per logical
    /// line, so pane buffers never receive fragments — core assembles the
    /// full line before queuing this.
    AppendTo(PaneKey, Arc<StyledLine>),
    /// Drop the main buffer's unterminated tail line. Emitted only when routing
    /// excludes main (gag/redirect) after a prefix of the line already flushed
    /// as a partial; affects only the uncommitted line, so line numbering
    /// parity holds.
    RetractOpenLine,
    /// Clear a terminal pane's scrollback (`pane.clear()`); the main pane is
    /// addressed by [`runtime::pane::MAIN_PANE_KEY`]. Line numbering
    /// continues from where it was — clearing never resets parity.
    Clear(PaneKey),
    /// One whole main-buffer row in the client's own voice (see
    /// [`system_row`]). Counts as exactly one committed line; the row's
    /// projection is the line every text consumer sees. Never a fragment:
    /// producers commit any open tail row first.
    AppendSystem(Arc<SystemRow>),
    /// Replace the system row appended under the same id — an in-progress
    /// notice finishing in place. The row keeps its line number; the ledger,
    /// the log and the terminal all swap the projection. A row no longer
    /// held (scrolled out, cleared) is appended instead, so the finished
    /// text is never lost.
    ReplaceSystem(Arc<SystemRow>),
}

impl BufferUpdate {
    /// The text this update adds to the main buffer, if any: an appended
    /// fragment, or a system row's whole projection (on append and on its
    /// finishing replacement alike). Replacement-of-open-line and pane
    /// deliveries are not main-buffer additions and read as `None`.
    #[must_use]
    pub fn main_text(&self) -> Option<&Arc<StyledLine>> {
        match self {
            Self::Append(line) => Some(line),
            Self::AppendSystem(row) | Self::ReplaceSystem(row) => Some(&row.line),
            Self::BeginOpenLineReplacement
            | Self::FinishOpenLineReplacement(_)
            | Self::EnsureNewLine
            | Self::PromptBoundary
            | Self::AppendTo(..)
            | Self::RetractOpenLine
            | Self::Clear(_) => None,
        }
    }
}

pub fn spawn(params: Arc<SessionParams>) -> impl Stream<Item = TaggedSessionEvent> {
    spawn_inner(&params, None, None, None)
}

/// Spawn a session with one exact application-owned Web Audio scope.
///
/// # Errors
///
/// Rejects a session-id mismatch before constructing or publishing a runtime
/// thread. The scope is reused unchanged by replacement engine generations.
#[cfg(feature = "web-audio")]
pub fn spawn_with_audio(
    params: Arc<SessionParams>,
    audio_scope: smudgy_audio_web::SessionAudioScope,
) -> Result<impl Stream<Item = TaggedSessionEvent>, SessionAudioScopeMismatch> {
    validate_audio_scope(&params, &audio_scope)?;
    Ok(spawn_inner(&params, None, None, Some(audio_scope)))
}

/// Like [`spawn`], but resolves `smudgy://` packages through `package_provider` instead of
/// the cloud client carried on [`SessionParams`]. The sandboxed-isolate integration tests
/// use this to inject an in-memory provider so the engine spawns a real per-package isolate
/// without a cloud backend; an embedder could supply a custom resolver the same way.
pub fn spawn_with_package_provider(
    params: Arc<SessionParams>,
    package_provider: PackageProviderFactory,
) -> impl Stream<Item = TaggedSessionEvent> {
    spawn_inner(&params, Some(package_provider), None, None)
}

/// Like [`spawn_with_package_provider`], with one exact Web Audio scope.
///
/// # Errors
///
/// Rejects a session-id mismatch before runtime-thread publication.
#[cfg(feature = "web-audio")]
pub fn spawn_with_package_provider_and_audio(
    params: Arc<SessionParams>,
    package_provider: PackageProviderFactory,
    audio_scope: smudgy_audio_web::SessionAudioScope,
) -> Result<impl Stream<Item = TaggedSessionEvent>, SessionAudioScopeMismatch> {
    validate_audio_scope(&params, &audio_scope)?;
    Ok(spawn_inner(
        &params,
        Some(package_provider),
        None,
        Some(audio_scope),
    ))
}

/// Spawn a session attached to the UI daemon's ordered command bus.
///
/// Headless embedders and existing integration tests may keep using [`spawn`];
/// their pane placement commands continue to arrive as `SessionEvent`s.
pub fn spawn_with_ui_commands(
    params: Arc<SessionParams>,
    ui_commands: ui_command::UiCommandBus,
) -> impl Stream<Item = TaggedSessionEvent> {
    spawn_inner(&params, None, Some(ui_commands), None)
}

/// Like [`spawn_with_ui_commands`], with one exact Web Audio scope.
///
/// # Errors
///
/// Rejects a session-id mismatch before runtime-thread publication.
#[cfg(feature = "web-audio")]
pub fn spawn_with_ui_commands_and_audio(
    params: Arc<SessionParams>,
    ui_commands: ui_command::UiCommandBus,
    audio_scope: smudgy_audio_web::SessionAudioScope,
) -> Result<impl Stream<Item = TaggedSessionEvent>, SessionAudioScopeMismatch> {
    validate_audio_scope(&params, &audio_scope)?;
    Ok(spawn_inner(
        &params,
        None,
        Some(ui_commands),
        Some(audio_scope),
    ))
}

/// Fallible production UI variant. Unlike compatibility spawn entry points,
/// an OS-thread creation failure is returned before a session is published.
/// Its exact `SpawnFailed` join tombstone remains available to the caller's
/// lifecycle coordinator.
///
/// # Errors
///
/// Returns [`AudioSessionSpawnError::Scope`] when the supplied audio scope is
/// not bound to this session, [`AudioSessionSpawnError::RuntimeThread`] when
/// the operating system refuses the runtime thread, or
/// [`AudioSessionSpawnError::RuntimePublication`] when the worker was spawned
/// but could not be published. The publication error owns its one-shot cleanup
/// report and must be consumed by the lifecycle coordinator.
#[cfg(feature = "web-audio")]
pub fn try_spawn_with_ui_commands_and_audio(
    params: &Arc<SessionParams>,
    ui_commands: ui_command::UiCommandBus,
    audio_scope: smudgy_audio_web::SessionAudioScope,
) -> Result<impl Stream<Item = TaggedSessionEvent> + use<>, AudioSessionSpawnError> {
    validate_audio_scope(params, &audio_scope)?;
    try_spawn_inner(params, None, Some(ui_commands), Some(audio_scope)).map_err(|error| match error
    {
        SessionRuntimeSpawnError::Thread(error) => AudioSessionSpawnError::RuntimeThread(error),
        SessionRuntimeSpawnError::Publication(error) => {
            AudioSessionSpawnError::RuntimePublication(error)
        }
    })
}

#[cfg(feature = "web-audio")]
fn validate_audio_scope(
    params: &SessionParams,
    audio_scope: &smudgy_audio_web::SessionAudioScope,
) -> Result<(), SessionAudioScopeMismatch> {
    let session_id = u32::from(params.session_id);
    if u64::from(session_id) == audio_scope.session_id() {
        Ok(())
    } else {
        Err(SessionAudioScopeMismatch {
            session_id: params.session_id,
            audio_session_id: audio_scope.session_id(),
        })
    }
}

fn spawn_inner(
    params: &SessionParams,
    package_provider_override: Option<PackageProviderFactory>,
    ui_commands: Option<ui_command::UiCommandBus>,
    audio_scope: Option<RuntimeAudioScope>,
) -> impl Stream<Item = TaggedSessionEvent> + use<> {
    try_spawn_inner(params, package_provider_override, ui_commands, audio_scope)
        .unwrap_or_else(|error| panic!("{error}"))
}

fn try_spawn_inner(
    params: &SessionParams,
    package_provider_override: Option<PackageProviderFactory>,
    ui_commands: Option<ui_command::UiCommandBus>,
    audio_scope: Option<RuntimeAudioScope>,
) -> Result<impl Stream<Item = TaggedSessionEvent> + use<>, SessionRuntimeSpawnError> {
    let (mut ui_tx, ui_rx) = futures::channel::mpsc::channel::<TaggedSessionEvent>(1024);

    if let Err(e) = ui_tx.try_send(TaggedSessionEvent {
        session_id: params.session_id,
        event: SessionEvent::UpdateBuffer(Arc::new(vec![BufferUpdate::AppendSystem(
            SystemRow::loading_session(),
        )])),
    }) {
        error!("Failed to send initial buffer update: {e:?}");
    }

    let runtime = runtime::Runtime::try_new(
        params.session_id,
        params.server_name.clone(),
        params.profile_name.clone(),
        params.profile_subtext.clone(),
        params.mapper.clone(),
        params.package_client.clone(),
        package_provider_override,
        params.extra_script_extensions.clone(),
        params.on_engine_rebuild.clone(),
        audio_scope,
        ui_tx,
        ui_commands,
    )?;
    let shutdown_tx = runtime.tx();

    // Register the runtime in the global registry
    registry::try_register_session(params.session_id, runtime.into())?;

    Ok(SessionEventStream {
        events: ui_rx,
        shutdown_tx,
    })
}

#[cfg(test)]
mod publication_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn try_spawn_returns_exact_joined_publication_unwind_without_evaluating_scripts() {
        let session_id = SessionId::from(92_500);
        let rebuilds = Arc::new(AtomicUsize::new(0));
        let rebuild_count = Arc::clone(&rebuilds);
        let params = SessionParams {
            session_id,
            server_name: Arc::new("publication-unwind".to_string()),
            profile_name: Arc::new("test".to_string()),
            profile_subtext: Arc::new(String::new()),
            mapper: None,
            package_client: None,
            extra_script_extensions: Arc::new(Vec::new),
            on_engine_rebuild: Some(Arc::new(move || {
                rebuild_count.fetch_add(1, Ordering::AcqRel);
            })),
        };

        registry::inject_next_publication_unwind();
        let result = try_spawn_inner(&params, None, None, None);
        let SessionRuntimeSpawnError::Publication(error) = result
            .err()
            .expect("injected registration unwind rejects spawn")
        else {
            panic!("registration unwind returned the wrong spawn error");
        };
        assert_eq!(
            error.failure(),
            runtime::RuntimeThreadPublicationFailure::PublicationUnwound
        );
        assert_eq!(
            error.cleanup(),
            runtime::RuntimeThreadJoinOutcome::Clean { session_id }
        );
        assert_eq!(rebuilds.load(Ordering::Acquire), 0, "scripts never built");
        assert!(registry::get_runtime(session_id).is_none());
        assert!(!runtime::runtime_thread_is_tracked(session_id));
        assert_eq!(
            runtime::join_runtime_thread(session_id),
            runtime::RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined { session_id },
            "the publication transaction consumed the exact join once"
        );
    }
}
