//! The runtime's action vocabulary: everything a session can be asked to do,
//! and the queue scripts/triggers use to emit actions mid-execution.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use deno_core::v8;
use smudgy_cloud::{AreaId, AtlasId};

use crate::models::aliases::AliasDefinition;
use crate::models::hotkeys::HotkeyDefinition;
use crate::models::triggers::TriggerDefinition;
use crate::session::styled_line::StyledLine;
use crate::session::{HotkeyId, SessionId};

use super::line_operation::LineOperation;
use super::origin::{IsolateId, Origin};
use super::pane::{
    PaneDef, PaneKey, PaneKind, PaneNamespace, PanePlacement, SplitDirection, TitleBarPolicy,
};
use super::script_engine::{FunctionId, ScriptId};
use super::trigger::MatchCapture;

#[derive(Clone, Debug)]
pub enum RuntimeAction {
    Connect {
        host: Arc<String>,
        port: u16,
        send_on_connect: Option<Arc<String>>,
        /// Literal substrings of `send_on_connect` to mask from the client's echo
        /// and the session log (e.g. a substituted `$PASSWORD`). Empty ⇒ the
        /// auto-login text is sent with ordinary `Send` semantics.
        send_on_connect_redactions: Arc<Vec<String>>,
    },
    /// Tears down the active TCP connection (if any) at the user's request. The
    /// socket task then emits [`RuntimeAction::Disconnected`] like any other
    /// drop. A no-op when there is no live connection.
    Disconnect,
    HandleIncomingLine(Arc<StyledLine>),
    HandleIncomingPartialLine(Arc<StyledLine>),
    /// A carriage-return overprint superseded the incoming open line: drop any
    /// prefix already delivered as a partial (the text after the `\r` replaces
    /// it). Emitted by the VT layer before the replacement frame's bytes.
    RetractIncomingPartialLine,
    CompleteLineTriggersProcessed(Arc<StyledLine>),
    PartialLineTriggersProcessed(Arc<StyledLine>),
    PerformLineOperation {
        line_number: usize,
        operation: LineOperation,
    },
    Send(Arc<String>),
    SendRaw(Arc<String>),
    /// Sends `text` to the server verbatim (split on `\n`, like `SendRaw`), but
    /// echoes the copy shown in the client's view and written to the session log
    /// with each (non-empty) literal `redactions` substring masked — so secrets
    /// such as a substituted `$PASSWORD` reach the wire but never the screen or
    /// logs.
    SendWithRedactions {
        text: Arc<String>,
        redactions: Arc<Vec<String>>,
    },
    SendRawUnless(Arc<AtomicBool>, Arc<String>),
    ProcessOutgoingLine(Arc<String>),
    Echo(Arc<String>),
    /// Echo pre-styled whole lines (a styled `echo`): each element is one on-screen
    /// line whose spans were built — tiling, gap-free — at the op boundary. Takes the
    /// same counted Append path as [`RuntimeAction::Echo`].
    EchoStyled(Vec<Arc<StyledLine>>),
    EvalJavascript {
        /// The isolate whose `compiled_scripts[id]` to run (`id` is a bare index into
        /// *that* isolate's registry; the pair travels together — see `PACKAGE-ISOLATES-ENGINE.md`).
        isolate: IsolateId,
        id: ScriptId,
        matches: Arc<Vec<MatchCapture>>,
        depth: u32,
        is_captured: Option<Arc<AtomicBool>>,
    },
    /// Runs a raw v8 function handle (a `smudgy_widgets` widget callback). The handle is
    /// isolate-bound; `isolate` + `instance` name the exact isolate *instantiation* that
    /// created the callback, and the engine invokes the handle only under that instantiation.
    /// A mismatch — the widget outlived an engine rebuild, so the handle's host heap is
    /// disposed — is dropped at dispatch without touching v8.
    ExecuteJavascriptFunction {
        /// The isolate role that created the callback (its v8 handle is bound there).
        isolate: IsolateId,
        /// The creating isolate's instantiation nonce, parsed from the widget routing token
        /// alongside `isolate` ([`IsolateId::from_widget_token`]).
        instance: u64,
        function: Arc<v8::Global<v8::Function>>,
        /// Positional string arguments forwarded to the JS function: empty for a no-arg
        /// `onPress`, a single clicked URL for a `Markdown` `onLink`.
        args: Vec<String>,
    },
    /// A click on a callback link (`LinkAction::Callback`): run slot `id` of the named
    /// isolate instantiation's link-callback registry. The UI sends this to the session
    /// owning the clicked pane; dispatch forwards it when `session` names another
    /// session (a fragment echoed cross-session runs its callback at home). The
    /// instance nonce drops clicks that outlived an engine rebuild, like widget
    /// callbacks — and unlike them the line carries no v8 handle, only this address,
    /// so it is deliberately NOT in [`Self::references_engine_state`]: purging it with
    /// the queue owner's reload would swallow forwarded clicks bound for another
    /// session's live engine, and staleness is already a defined no-op at dispatch
    /// (nonce mismatch, gone isolate, evicted id).
    InvokeLinkCallback {
        session: SessionId,
        isolate: IsolateId,
        instance: u64,
        id: u64,
        shift: bool,
        ctrl: bool,
        alt: bool,
    },
    CallJavascriptFunction {
        /// The isolate owning `script_functions[id]` (see `EvalJavascript::isolate`).
        isolate: IsolateId,
        id: FunctionId,
        matches: Arc<Vec<MatchCapture>>,
        depth: u32,
        is_captured: Option<Arc<AtomicBool>>,
    },
    /// Register (or upsert) a hotkey under its `(isolate, origin, name)` key. Disk-loaded
    /// hotkeys use `(IsolateId::Main, Origin::User)`; script-created ones (`createHotkey`)
    /// carry their creator's provenance so `delete()` is origin-scoped. Re-adding the same key
    /// replaces the prior binding (upsert).
    ///
    /// `function_id` is `Some` for a `createHotkey(.., handler)` whose handler is a JS function
    /// already registered in `isolate`'s `script_functions`; the hotkey fires it via
    /// `CallJavascriptFunction`. `None` is the disk/inline-string path, where `hotkey.script` is
    /// compiled and sent directly.
    AddHotkey {
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        hotkey: HotkeyDefinition,
        function_id: Option<FunctionId>,
    },
    /// Remove a script-created hotkey by its `(isolate, origin, name)` key (`delete()`),
    /// unregistering it from the UI. A `delete()` of an unknown key is a no-op.
    RemoveHotkey(IsolateId, Origin, Arc<String>),
    AddAlias {
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        alias: AliasDefinition,
        /// Self-limit: auto-remove after this many fires. `None` ⇒ no limit;
        /// `Some(1)` ⇒ one-shot. Aliases ignore `line_limit` (they match input, not
        /// server lines), so only `fire_limit` is carried here.
        fire_limit: Option<u32>,
    },
    AddJavascriptFunctionAlias {
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        patterns: Arc<Vec<String>>,
        function_id: FunctionId,
        fire_limit: Option<u32>,
        /// The handler's `toString()`, passed in good faith from JS-land for the read-only
        /// detail pane. `None` when the caller supplied no source. Display-only.
        script_source: Option<Arc<str>>,
    },
    AddTrigger {
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        trigger: TriggerDefinition,
        /// Self-limits: auto-remove after `fire_limit` fires OR `line_limit`
        /// tested lines, whichever comes first. `None` ⇒ that limit is unbounded.
        fire_limit: Option<u32>,
        line_limit: Option<u32>,
    },
    AddJavascriptFunctionTrigger {
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        patterns: Arc<Vec<String>>,
        raw_patterns: Arc<Vec<String>>,
        anti_patterns: Arc<Vec<String>>,
        function_id: FunctionId,
        prompt: bool,
        enabled: bool,
        fire_limit: Option<u32>,
        line_limit: Option<u32>,
        /// The handler's `toString()`, passed in good faith from JS-land for the read-only
        /// detail pane. `None` when the caller supplied no source. Display-only.
        script_source: Option<Arc<str>>,
    },
    EnableAlias(IsolateId, Origin, Arc<String>, bool),
    EnableTrigger(IsolateId, Origin, Arc<String>, bool),
    /// Remove an alias by its `(isolate, origin, name)` key — an explicit `delete()`
    /// or a `fireLimit` self-limit hit. Drops it from the `Vec` and rebuilds the
    /// alias `PatternSet` so its matcher slot is freed.
    RemoveAlias(IsolateId, Origin, Arc<String>),
    /// Remove a trigger by its `(isolate, origin, name)` key — `delete()` or a
    /// `fireLimit`/`lineLimit` self-limit hit.
    RemoveTrigger(IsolateId, Origin, Arc<String>),
    ExecHotkey {
        id: HotkeyId,
    },
    SetCurrentLocation(AreaId, Option<i32>),
    /// A mapper navigation op (speedwalk / find-nearest) resolved a destination
    /// in this area — a demonstrated navigation intent the UI daemon weighs for
    /// per-server map scoping (bind-on-use). Advisory only; carries no map
    /// mutation. Emitted from the read-side nav ops, translated to
    /// `SessionEvent::MapperNavigated`.
    NoteMapperNavigation(AreaId),
    /// A room the auto-mapper was about to create is already mapped on a
    /// *different* server entry (a scope-excluded area). The UI daemon raises
    /// the cross-entry "show here too?" rescue offer. Translated to
    /// `SessionEvent::OfferMapRescue`.
    OfferMapRescue {
        area_id: AreaId,
        atlas_id: Option<AtlasId>,
        atlas_name: Option<String>,
    },
    /// A script created a non-ephemeral (cloud-tier) area in this session; the
    /// UI daemon associates it with this session's server entry so nothing
    /// user-created starts unassigned. Translated to
    /// `SessionEvent::MapAreaCreated`.
    AssociateCreatedArea(AreaId),
    /// Emit `SessionEvent::PaneOpened` for a pane the split op already
    /// created synchronously in the registry. Queued by the op so the event
    /// leaves on the ordered UI channel ahead of any `AppendTo` for the key.
    PaneOpened {
        def: PaneDef,
        placement: PanePlacement,
    },
    /// Emit `SessionEvent::PaneClosed` for a pane the close op already
    /// retired from the registry. The dispatch handler flushes buffered
    /// updates first, so the event trails every `AppendTo` that preceded it.
    PaneClosed { key: PaneKey },
    /// Emit `SessionEvent::PaneUpdated` for a def the split op already
    /// mutated in place (an explicit `titleBar` on an existing pane). A pure
    /// display refresh — no placement, so no ordering constraints beyond the
    /// channel itself.
    PaneUpdated { def: PaneDef },
    /// Close every pane no script re-claimed during a reload. The reload loop
    /// queues this *behind* the freshly loaded modules' own spawned actions,
    /// so load-time deliveries to a doomed pane still land before its
    /// `PaneClosed` (the dispatch arm also flushes buffered updates first).
    PaneReloadSweep,
    /// Cross-session pane create (`reach-others`): carries names, not keys —
    /// the target registry lives on another session's thread — and resolves
    /// at dispatch on the owning runtime (last-writer-wins in queue order).
    PaneSplitRemote {
        namespace: PaneNamespace,
        name: Arc<str>,
        kind: PaneKind,
        title_bar: Option<TitleBarPolicy>,
        reference: Option<Arc<str>>,
        direction: SplitDirection,
        size_px: Option<f32>,
    },
    /// Cross-session pane close, by name (see [`RuntimeAction::PaneSplitRemote`]).
    /// A name that is not live is a silent no-op (idempotent, best-effort).
    PaneCloseRemote {
        namespace: PaneNamespace,
        name: Arc<str>,
    },
    /// Echo `text` into the named pane as whole lines (split on `\n`).
    /// Name-carrying so it is routable cross-session; resolved at dispatch.
    /// Pane echoes skip `emitted_line_count`/`record_emitted_line` and the
    /// main open-line heuristic entirely.
    ///
    /// `key` is the pane the own-session op already resolved *synchronously*
    /// at call time: carrying it means an echo issued before a `close()` in
    /// the same script body still lands (the close retires the registry entry
    /// before this action dispatches, so a name re-resolve would miss). `None`
    /// is the cross-session path — the target registry lives on another thread
    /// and resolves by name on the owning runtime.
    PaneEcho {
        key: Option<PaneKey>,
        namespace: PaneNamespace,
        name: Arc<str>,
        text: Arc<String>,
    },
    /// Styled `pane.echo`: pre-styled whole lines into the named pane, with the same
    /// key/name resolution semantics as [`RuntimeAction::PaneEcho`]. Main-pane delivery
    /// takes the counted Append path, like [`RuntimeAction::EchoStyled`].
    PaneEchoStyled {
        key: Option<PaneKey>,
        namespace: PaneNamespace,
        name: Arc<str>,
        lines: Vec<Arc<StyledLine>>,
    },
    /// Clear the named terminal pane's scrollback (`"main"` clears the main
    /// buffer). `key` is the own-session pre-resolved pane, like
    /// [`RuntimeAction::PaneEcho`]; `None` resolves by name cross-session.
    PaneClear {
        key: Option<PaneKey>,
        namespace: PaneNamespace,
        name: Arc<str>,
    },
    /// Live-applies global settings the runtime cares about (separator, raw
    /// prefix, logging). Sent by the UI when settings change; the runtime also
    /// seeds the same values itself from `load_settings()` at construction.
    /// `script_settings` carries the script-visible view (including the
    /// UI-resolved palette) refreshed into the `getSettings()` snapshot.
    ApplySettings {
        command_separator: Arc<String>,
        raw_line_prefix: Arc<String>,
        log_enabled: bool,
        script_settings: Box<crate::models::settings::ScriptSettings>,
    },
    RequestRepaint,
    Connected,
    Disconnected,
    /// One inbound GMCP message (`docs/gmcp-plan.md` §3): the dotted message name and the
    /// raw data part exactly as received — unparsed; the dispatch arm parses on the session
    /// thread and writes the `gmcp` store subtree. Enqueued by the connection task at the
    /// exact stream position the subnegotiation occupied — the same channel as
    /// `HandleIncomingLine`, which is what makes a message readable in the store by every
    /// consumer of any line that followed it on the wire (the §3.3 ordering guarantee).
    GmcpMessage {
        name: Arc<str>,
        data: Option<Arc<str>>,
    },
    /// GMCP negotiated on; the connection task has already framed the `Core.Hello` +
    /// baseline `Core.Supports.Set` handshake onto the reply buffer. The dispatch arm
    /// clears the `gmcp` subtree (fresh server, fresh truth) and emits `gmcp:ready`.
    GmcpEnabled,
    /// GMCP negotiated off mid-connection (`WONT`); disconnect-while-enabled takes the
    /// `Disconnected` arm's path instead. Emits `gmcp:closed` if it was enabled.
    GmcpDisabled,
    /// One outbound GMCP message from a script (`gmcp.send`, ⟂ `gmcp:send` — enforced at
    /// the op). Framed and written by the dispatch arm; dropped with a one-time notice
    /// when GMCP is not negotiated on the connection.
    GmcpSend {
        name: Arc<str>,
        data: Option<Arc<str>>,
    },
    /// `gmcp.enableModule`: register the calling isolate's ref on a GMCP module
    /// (`docs/gmcp-plan.md` §6.2). 0→1 (while negotiated) sends `Core.Supports.Add`.
    GmcpEnableModule {
        isolate: IsolateId,
        module: Arc<str>,
        version: u32,
    },
    /// `gmcp.disableModule`: release the calling isolate's ref; last-ref-out sends
    /// `Core.Supports.Remove`.
    GmcpDisableModule {
        isolate: IsolateId,
        module: Arc<str>,
    },
    /// `gmcp.mergeKeys`: extend the deep-merge message-name set (`docs/gmcp-plan.md` §4.3).
    GmcpAddMergeKeys(Arc<Vec<String>>),
    /// One inbound MSDP subnegotiation, raw bytes exactly as received (MSDP frames are
    /// control-marked, so no text decode happens on the connection task); the dispatch
    /// arm decodes on the session thread and writes the `msdp` store subtree. Rides the
    /// same channel as `HandleIncomingLine`, so the GMCP §3.3 wire-order guarantee holds
    /// for MSDP identically.
    MsdpMessage { payload: Arc<[u8]> },
    /// MSDP negotiated on; the connection task has already framed the `LIST` + baseline
    /// `REPORT` handshake onto the reply buffer. The dispatch arm clears the `msdp`
    /// subtree (fresh server, fresh truth) and emits `msdp:ready`.
    MsdpEnabled,
    /// MSDP negotiated off mid-connection (`WONT`); disconnect-while-enabled takes the
    /// `Disconnected` arm's path instead. Emits `msdp:closed` if it was enabled.
    MsdpDisabled,
    Reload,
    Shutdown,
    Noop,
}

unsafe impl Send for RuntimeAction {}
unsafe impl Sync for RuntimeAction {}

impl RuntimeAction {
    /// Whether this action names a specific script/function of the *current* engine — a
    /// `ScriptId`/`FunctionId` index into an isolate's registry, or a raw `v8::Global` handle.
    /// Such an action is only meaningful to the engine that minted it: after an engine rebuild
    /// (reload) the id indexes the fresh registry and would invoke an unrelated handler (or
    /// error). These actions transit the session channel — watch deliveries and async event
    /// forwards ride it — so any left queued behind a `Reload` must be dropped during the
    /// rebuild rather than dispatched into the new engine (`interop.md` §3, reload hygiene).
    /// `ExecuteJavascriptFunction` carries its own instantiation nonce and is already dropped at
    /// dispatch on mismatch, but it is filtered here too so the rule has one home.
    pub(crate) fn references_engine_state(&self) -> bool {
        matches!(
            self,
            Self::EvalJavascript { .. }
                | Self::CallJavascriptFunction { .. }
                | Self::ExecuteJavascriptFunction { .. }
        )
    }

    /// The isolate an automation/script action registers into or runs in, when it names one.
    /// Used to purge actions a *failed* isolate load left in the spawned-action queue before its
    /// isolate is discarded: those actions would otherwise register automations keyed to a
    /// now-dead isolate (or dispatch scripts into it), which the trigger dispatch assumes is
    /// impossible (`set_is_captured`'s liveness `debug_assert`). Non-isolate actions return
    /// `None` and are always retained.
    pub(crate) fn target_isolate(&self) -> Option<&IsolateId> {
        match self {
            Self::EvalJavascript { isolate, .. }
            | Self::ExecuteJavascriptFunction { isolate, .. }
            | Self::CallJavascriptFunction { isolate, .. }
            | Self::AddHotkey { isolate, .. }
            | Self::AddAlias { isolate, .. }
            | Self::AddJavascriptFunctionAlias { isolate, .. }
            | Self::AddTrigger { isolate, .. }
            | Self::AddJavascriptFunctionTrigger { isolate, .. }
            | Self::RemoveHotkey(isolate, ..)
            | Self::EnableAlias(isolate, ..)
            | Self::EnableTrigger(isolate, ..)
            | Self::RemoveAlias(isolate, ..)
            | Self::RemoveTrigger(isolate, ..) => Some(isolate),
            _ => None,
        }
    }
}

/// Actions emitted synchronously by scripts and triggers while the runtime is
/// processing an action. The main loop drains this after every action and
/// splices the contents in ahead of already-queued siblings (depth-first
/// expansion), and after script event-loop polling, where the contents have
/// no position in any expansion and are forwarded to the back of the main
/// queue like new input.
pub(crate) type ActionQueue = Rc<RefCell<VecDeque<RuntimeAction>>>;


pub(crate) enum ActionResult {
    None,
    Echo(String),
    Reload,
    CloseSession,
    Run(Vec<RuntimeAction>),
}

pub(crate) enum RunAction {
    None,
    Reload,
}
