use derive_more::{Add, Display, From, Into};
use futures::Stream;
use runtime::RuntimeAction;
use smudgy_cloud::{AreaId, AtlasId, Mapper};
use std::{fmt::Debug, sync::Arc};
use styled_line::StyledLine;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    models::hotkeys::HotkeyDefinition,
    session::runtime::input::InputOp,
    session::runtime::line_operation::LineOperation,
    session::runtime::pane::{PaneDef, PaneKey, PanePlacement},
};

pub mod config;
pub mod connection;
pub mod registry;
pub mod runtime;
pub mod styled_line;

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
        operation: LineOperation,
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
    /// An existing pane's def changed in place — today only its `title_bar`
    /// policy, via a `split()` naming an existing pane with an explicit
    /// `titleBar` (the only way to set the main pane's). Pure display-state
    /// refresh: no placement, no buffer implications.
    PaneUpdated(PaneDef),
    /// A session-store flush updated widget-binding cells
    /// (`docs/interop.md` §7). Pure repaint wake: the cells already
    /// hold the new values and the widget render closures read them lock-free,
    /// so the UI needs no state change — processing the message redraws the view.
    StoreBindingsChanged,
    /// Apply one scripted input mutation to the input of pane `key`
    /// (`docs/input.md` §3.4). Travels the ordered channel, so ops
    /// land in the order scripts issued them.
    InputOp { key: PaneKey, op: InputOp },
    /// The session thread has flagged input-mirror interest: start sending
    /// `RuntimeAction::InputStateChanged` on input changes, and push the
    /// current state immediately so the mirror warms up.
    InputMirrorInterest,
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
    /// The server's telnet ECHO state (RFC 857): `enabled` means the server
    /// has taken over echoing — the classic password-prompt signal — and the
    /// UI should mask the main input (subject to the user's auto-mask
    /// preference); `false` releases that mask. The telnet cause composes
    /// with a script-set mask UI-side: the input stays masked while either
    /// is active (`docs/input.md` §3.10). Also sent with `false` on
    /// disconnect, since the option dies with the connection.
    ServerEcho { enabled: bool },
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
    /// Commit the main buffer's open line.
    EnsureNewLine,
    /// One WHOLE line for a non-main pane. Routing is decided per logical
    /// line, so pane buffers never receive fragments — core assembles the
    /// full line before queuing this.
    AppendTo(PaneKey, Arc<StyledLine>),
    /// Drop the main buffer's unterminated tail line. Emitted when routing
    /// excludes main (gag/redirect) after a prefix of the line already
    /// flushed as a partial; affects only the uncommitted line, so line
    /// numbering parity holds.
    RetractOpenLine,
    /// Clear a terminal pane's scrollback (`pane.clear()`); the main pane is
    /// addressed by [`runtime::pane::MAIN_PANE_KEY`]. Line numbering
    /// continues from where it was — clearing never resets parity.
    Clear(PaneKey),
}

pub fn spawn(params: Arc<SessionParams>) -> impl Stream<Item = TaggedSessionEvent> {
    spawn_inner(params, None)
}

/// Like [`spawn`], but resolves `smudgy://` packages through `package_provider` instead of
/// the cloud client carried on [`SessionParams`]. The sandboxed-isolate integration tests
/// use this to inject an in-memory provider so the engine spawns a real per-package isolate
/// without a cloud backend; an embedder could supply a custom resolver the same way.
pub fn spawn_with_package_provider(
    params: Arc<SessionParams>,
    package_provider: PackageProviderFactory,
) -> impl Stream<Item = TaggedSessionEvent> {
    spawn_inner(params, Some(package_provider))
}

fn spawn_inner(
    params: Arc<SessionParams>,
    package_provider_override: Option<PackageProviderFactory>,
) -> impl Stream<Item = TaggedSessionEvent> {
    let (mut ui_tx, ui_rx) = futures::channel::mpsc::channel::<TaggedSessionEvent>(1024);

    if let Err(e) = ui_tx.try_send(TaggedSessionEvent {
        session_id: params.session_id,
        event: SessionEvent::UpdateBuffer(Arc::new(vec![BufferUpdate::Append(Arc::new(
            StyledLine::from_echo_str("Loading session..."),
        ))])),
    }) {
        error!("Failed to send initial buffer update: {e:?}");
    }

    let runtime = runtime::Runtime::new(
        params.session_id,
        params.server_name.clone(),
        params.profile_name.clone(),
        params.profile_subtext.clone(),
        params.mapper.clone(),
        params.package_client.clone(),
        package_provider_override,
        params.extra_script_extensions.clone(),
        params.on_engine_rebuild.clone(),
        ui_tx,
    );

    // Register the runtime in the global registry
    registry::register_session(params.session_id, runtime.into());

    ui_rx
}
