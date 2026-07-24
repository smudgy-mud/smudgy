use crate::models::ScriptLang;
use crate::models::aliases::AliasDefinition;
use crate::models::hotkeys::HotkeyDefinition;
use crate::models::triggers::TriggerDefinition;
use crate::session::connection::vt_processor::AnsiColor;
use crate::session::runtime::line_operation::{LineOperation, SpliceRun};
use crate::session::runtime::pane;
use crate::session::runtime::script_engine::FunctionId;
use crate::session::runtime::store;
use crate::session::runtime::trigger::MatchCapture;
use crate::session::runtime::trigger::SharedAutomationRegistry;
use crate::session::runtime::{
    ActionQueue, AutomationKind, IsolateId, MAX_EVENT_DEPTH, Origin, RuntimeAction, SingletonKey,
    SingletonRegistry,
};
use crate::session::styled_line::{Color, LinkAction, Style, StyledLine, sanitize_display_text};
use crate::session::{SessionId, registry};

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use anyhow::{Context, Error as AnyError, bail};
use deno_core::OpState;
use deno_core::op2;
use deno_core::v8;
use smudgy_cloud::{AreaId, Uuid, WidgetIsolate, WidgetsEnabled};
use smudgy_script::SmudgyCapabilities;

deno_core::extension!(
  smudgy_ops,
  ops = [
    op_smudgy_get_current_session,
    op_smudgy_get_session_character,
    op_smudgy_get_sessions,
    op_smudgy_session_echo,
    op_smudgy_session_echo_styled,
    op_smudgy_session_reload,
    op_smudgy_session_send,
    op_smudgy_session_send_raw,
    op_smudgy_create_simple_alias,
    op_smudgy_create_simple_trigger,
    op_smudgy_create_javascript_function_trigger,
    op_smudgy_create_javascript_function_alias,
    op_smudgy_set_alias_enabled,
    op_smudgy_set_trigger_enabled,
    op_smudgy_remove_alias,
    op_smudgy_remove_trigger,
    op_smudgy_create_hotkey,
    op_smudgy_remove_hotkey,
    op_smudgy_get_alias,
    op_smudgy_get_trigger,
    op_smudgy_list_aliases,
    op_smudgy_list_triggers,
    op_smudgy_alias_exists,
    op_smudgy_trigger_exists,
    op_smudgy_line_insert,
    op_smudgy_line_replace,
    op_smudgy_splice,
    op_smudgy_line_splice,
    op_smudgy_line_highlight,
    op_smudgy_line_remove,
    op_smudgy_insert,
    op_smudgy_replace,
    op_smudgy_highlight,
    op_smudgy_remove,
    op_smudgy_gag,
    op_smudgy_redirect,
    op_smudgy_copy,
    op_smudgy_pane_split,
    op_smudgy_pane_input_on_submit,
    op_smudgy_pane_close,
    op_smudgy_pane_echo,
    op_smudgy_pane_echo_styled,
    op_smudgy_pane_clear,
    op_smudgy_pane_list,
    op_smudgy_pane_resolve,
    op_smudgy_input_get,
    op_smudgy_input_apply,
    op_smudgy_input_submission_generation,
    op_smudgy_input_submission_text,
    op_smudgy_input_submission_replace,
    op_smudgy_input_submission_cancel,
    op_smudgy_input_words_mutate,
    op_smudgy_input_words_query,
    op_smudgy_input_history_mutate,
    op_smudgy_input_history_list,
    op_smudgy_get_current_line,
    op_smudgy_get_current_line_number,
    op_smudgy_get_current_line_styles,
    op_smudgy_buffer_get_text,
    op_smudgy_buffer_get_styles,
    op_smudgy_mapper_set_current_location,
    op_smudgy_mapper_get_current_location,
    op_smudgy_capture,
    op_smudgy_fallthrough,
    op_smudgy_param_get,
    op_smudgy_get_settings,
    op_smudgy_save_user_alias,
    op_smudgy_save_user_trigger,
    op_smudgy_save_user_hotkey,
    op_smudgy_delete_user_alias,
    op_smudgy_delete_user_trigger,
    op_smudgy_delete_user_hotkey,
    op_smudgy_get_user_alias,
    op_smudgy_get_user_trigger,
    op_smudgy_get_user_hotkey,
    op_smudgy_list_user_aliases,
    op_smudgy_list_user_triggers,
    op_smudgy_list_user_hotkeys,
    op_smudgy_validate_name,
    op_smudgy_on,
    op_smudgy_off,
    op_smudgy_emit,
    op_smudgy_gmcp_enabled,
    op_smudgy_gmcp_send,
    op_smudgy_gmcp_enable_module,
    op_smudgy_gmcp_disable_module,
    op_smudgy_gmcp_merge_keys,
    op_smudgy_interop_resolve_creator,
    op_smudgy_interop_resolve_producer_root,
    op_smudgy_interop_resolve_consumer_root,
    op_smudgy_interop_resolve_previous_root,
    op_smudgy_interop_resolve_event,
    op_smudgy_store_set,
    op_smudgy_store_get,
    op_smudgy_store_get_tagged,
    op_smudgy_store_keys,
    op_smudgy_store_previous_get,
    op_smudgy_store_previous_get_tagged,
    op_smudgy_store_previous_keys,
    op_smudgy_store_watch,
    op_smudgy_store_unwatch,
    op_smudgy_store_bind,
    op_smudgy_procedure_on,
    op_smudgy_procedure_post,
    op_smudgy_interop_declare,
    op_smudgy_data_dir,
    ],
  esm_entry_point = "ext:smudgy_ops/smudgy.ts",
  esm = [ dir "src/session/runtime/js", "smudgy.ts" ],
  options = {
    session_id: SessionId,
    server_name: Arc<String>,
    script_functions: Rc<RefCell<Vec<v8::Global<v8::Function>>>>,
    spawned_actions: ActionQueue,
    pending_line_operations: Rc<RefCell<Vec<LineOperation>>>,
    current_line: Rc<RefCell<Weak<StyledLine>>>,
    // The current-line staleness scope (see [`LineScope`]): one cell shared engine-wide,
    // armed by the engine's user-JS entry points and checked by the ambient `line`
    // mutators, so a stale async continuation cannot edit/route a line it wasn't run for.
    line_scope: LineScopeCell,
    emitted_line_count: std::rc::Weak<Cell<usize>>,
    // Ring of recently-emitted lines (UI line number + the same `Arc` the UI holds),
    // shared with the runtime. `op_smudgy_buffer_get_text`/`_styles` read it for `buffer.line(n)`.
    recent_lines: crate::session::runtime::RecentLines,
    // Current mapper location, shared with the runtime. `op_smudgy_mapper_set_current_location`
    // writes it (alongside emitting the UI marker action) and `op_smudgy_mapper_get_current_location`
    // reads it — a current-session read (the value lives on this thread, not the `Mapper`).
    current_location: crate::session::runtime::CurrentLocation,
    // The script-visible settings snapshot, shared with the runtime. `op_smudgy_get_settings`
    // reads it for `getSettings()`; the `ApplySettings` dispatch handler is the writer.
    settings_snapshot: crate::session::runtime::SettingsSnapshot,
    // The GMCP enabled flag (`docs/gmcp.md` §3.4), shared with the runtime's GMCP
    // producer (the writer). `op_smudgy_gmcp_enabled` reads it for `gmcp.enabled`/`gmcp.onReady`.
    gmcp_enabled: crate::session::runtime::gmcp::SharedGmcpEnabled,
    // The session's pane registry, shared with the runtime. Own-session pane ops mutate it
    // synchronously here in the op (get-or-create is race-free locally and
    // `const p = pane.split(...); line.redirect(p)` works within one trigger body); the
    // isolate's pane namespace derives from `isolate_id`.
    pane_registry: crate::session::runtime::SharedPaneRegistry,
    // Per-line routing state (gag/redirect/copy), shared with the runtime beside
    // `pending_line_operations` and consumed once per line event.
    line_routing: crate::session::runtime::SharedLineRouting,
    // The input mirror (`docs/input.md` §3.3), shared with the runtime (whose
    // `InputStateChanged` dispatch arm writes it). The input read op consults it
    // synchronously and flags interest on it; writes leave it untouched.
    input_mirror: crate::session::runtime::SharedInputMirror,
    // The in-flight typed submission (`docs/input.md` §3.5), shared with the
    // runtime (whose `SubmitInput`/`CompleteInputSubmission` dispatch arms install and
    // consume it). The ambient `submission` ops read and mutate it while a `sys:input`
    // handler splice is live; outside one they throw.
    input_submission: crate::session::runtime::SharedInputSubmission,
    // The completion word sets (`docs/input.md` §3.8), shared with the runtime
    // (whose `InputWordSetsChanged` dispatch arm builds the UI's merged view). The
    // registry ops mutate and read it synchronously, scoped by the caller's
    // `(isolate, origin)` — the automation-keying creator identity.
    input_word_sets: crate::session::runtime::SharedInputWordSets,
    // The pane-input onSubmit registry (`docs/input.md` §3.7), shared with the
    // runtime (whose `PaneInputSubmit` dispatch arm resolves through it). The registration
    // op writes handler addresses — `(isolate, instance, function id)` — synchronously.
    pane_input_callbacks: crate::session::runtime::SharedPaneInputCallbacks,
    // The isolate these ops run in. The creation/enable ops stamp it onto the actions
    // they emit so the trigger Manager keys automations by `(IsolateId, Origin, name)`
    // (see `PACKAGE-ISOLATES.md`). `script_functions` above is *this* isolate's registry.
    isolate_id: IsolateId,
    // This isolate's instance nonce (`ScriptEngine`'s process-wide counter): names the exact
    // instantiation, where `isolate_id` names only the role a reload recreates. Baked into the
    // widget routing token below so dispatch can reject callbacks minted by a previous
    // instantiation (their v8 handles are bound to the disposed heap).
    isolate_instance: u64,
    // The session-global `singleton` reservation set (see `PACKAGE-ISOLATES.md`). The SAME
    // `Rc` is handed to every isolate's ops, so a `createAlias(.., {singleton:true})` in any
    // isolate dedupes session-wide. Legal because all isolates share the one session thread.
    singleton_registry: SingletonRegistry,
    // Introspection mirror, shared with the trigger `Manager` (the writer). The
    // `get`/`list`/`exists` ops read it for the caller's OWN `(isolate, origin)` namespace.
    automation_registry: SharedAutomationRegistry,
    // The smudgy op-capabilities this isolate may use (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`).
    // `all()` for the main/trusted isolate; built from the package's CONSENTED smudgy capability
    // set for a sandboxed isolate (∅ ⇒ all-false ⇒ every gated op throws `NotCapable`). The gated
    // ops below read it from `OpState`; the `widgets` bit is mirrored as a `WidgetsEnabled` flag
    // the (separate-crate) `smudgy_widgets` ops read — see [`SmudgyGrants`] / `smudgy_cloud::WidgetsEnabled`.
    smudgy_grants: SmudgyGrants,
    // The session-global event bus (`PACKAGE-EVENTS.md`): canonical event name -> subscribers. The
    // SAME `Rc` is handed to every isolate's ops (like `singleton_registry`), so an `emit` in one
    // isolate reaches subscribers in another — cross-isolate delivery routes through the host.
    event_registry: EventRegistry,
    // The session store (`docs/interop.md`): the SAME `Rc` for every isolate. Writes
    // journal into it synchronously; the runtime flushes the journal per turn.
    session_store: crate::session::runtime::SharedSessionStore,
    // The message bus (`docs/interop.md` §6): the SAME `Rc` for every isolate, so a
    // consumer's `post` in one isolate reaches the producer's receiver in another through the
    // host action queue (like the event bus above).
    message_bus: crate::session::runtime::SharedMessageBus,
    // The runtime catalogue (`docs/interop.md` §10): the SAME `Rc` for every
    // isolate. The emit/post ops record samples at their choke points; the store-set op notes
    // observed-but-undeclared keys; the handle constructors confirm declarations at runtime.
    catalogue: crate::session::runtime::SharedCatalogue,
    // The interop home registry (`docs/interop.md` §3): installed package -> home
    // isolate, checked (with origin) by the store/emit write gates. The SAME `Rc` for every
    // isolate; read-only after engine construction.
    home_registry: crate::session::runtime::store::HomeRegistry,
    // The session store's widget-binding registry (interop.md §7): binding-token id -> shared value
    // cell. Parked in `OpState` because its READERS are the `smudgy_widgets` build ops, which
    // live in a leaf crate that cannot name the store (same bridge pattern as `WidgetIsolate`);
    // `op_smudgy_store_bind` mints the ids through the store, which keeps this in lockstep.
    store_bindings: smudgy_cloud::StoreBindings,
    // This isolate's own data dir (`$DATA`): where its `read`/`write` grants resolve. Read by
    // `op_smudgy_data_dir` for the script's `getDataDir()`. A sandboxed package's
    // `.isolate-storage/<slug>/data`; the shared server dir for the main isolate.
    data_dir: std::path::PathBuf,
  },
  state = |state, options| {
    state.put::<SessionId>(options.session_id);
    state.put::<ServerName>(ServerName(options.server_name));
    state.put::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>(options.script_functions);
    state.put::<ActionQueue>(options.spawned_actions);
    state.put::<Rc<RefCell<Vec<LineOperation>>>>(options.pending_line_operations);
    state.put::<Rc<RefCell<Weak<StyledLine>>>>(options.current_line);
    state.put::<LineScopeCell>(options.line_scope);
    state.put::<std::rc::Weak<Cell<usize>>>(options.emitted_line_count);
    state.put::<crate::session::runtime::RecentLines>(options.recent_lines);
    state.put::<crate::session::runtime::CurrentLocation>(options.current_location);
    state.put::<crate::session::runtime::SettingsSnapshot>(options.settings_snapshot);
    state.put::<crate::session::runtime::gmcp::SharedGmcpEnabled>(options.gmcp_enabled);
    state.put::<crate::session::runtime::SharedPaneRegistry>(options.pane_registry);
    state.put::<crate::session::runtime::SharedLineRouting>(options.line_routing);
    state.put::<crate::session::runtime::SharedInputMirror>(options.input_mirror);
    state.put::<crate::session::runtime::SharedInputSubmission>(options.input_submission);
    state.put::<crate::session::runtime::SharedInputWordSets>(options.input_word_sets);
    state.put::<crate::session::runtime::SharedPaneInputCallbacks>(options.pane_input_callbacks);
    // The registration op stamps handler addresses with this instantiation's nonce, so
    // dispatch can drop a submission whose handler outlived an engine rebuild (the same
    // staleness rule the widget routing token carries in string form).
    state.put::<IsolateInstance>(IsolateInstance(options.isolate_instance));
    // Park this isolate's routing token where the leaf `smudgy_widgets` button op can read it (it
    // cannot name `IsolateId`); a tagged widget callback is dispatched back into this isolate —
    // and only into THIS instantiation of it, via the instance nonce baked into the token.
    // Link callbacks carry the same token; their per-echo contexts clone the shared `Arc` form.
    let widget_token = options.isolate_id.to_widget_token(options.isolate_instance);
    state.put::<LinkIsolateToken>(LinkIsolateToken(std::sync::Arc::from(widget_token.as_str())));
    state.put::<WidgetIsolate>(WidgetIsolate(widget_token));
    state.put::<IsolateId>(options.isolate_id);
    state.put::<SingletonRegistry>(options.singleton_registry);
    state.put::<SharedAutomationRegistry>(options.automation_registry);
    state.put::<SmudgyGrants>(options.smudgy_grants);
    // Bridge the `widgets` grant to the `smudgy_widgets` ops, which live in a leaf crate that cannot
    // name `SmudgyGrants` (`smudgy_cloud` is the crate both share — see its `WidgetsEnabled`).
    state.put::<WidgetsEnabled>(WidgetsEnabled(options.smudgy_grants.widgets));
    state.put::<EventRegistry>(options.event_registry);
    state.put::<crate::session::runtime::SharedSessionStore>(options.session_store);
    state.put::<crate::session::runtime::SharedMessageBus>(options.message_bus);
    state.put::<crate::session::runtime::SharedCatalogue>(options.catalogue);
    state.put::<crate::session::runtime::store::HomeRegistry>(options.home_registry);
    state.put::<smudgy_cloud::StoreBindings>(options.store_bindings);
    state.put::<PackageDataDir>(PackageDataDir(options.data_dir));
    state.put::<Capture>(Capture(false));
    state.put::<Fallthrough>(Fallthrough(None));
    // The per-isolate interop identity table (`docs/interop.md` §3): interned
    // creators/roots/events, resolved once at handle construction, addressed by id per call.
    state.put::<InteropIdentities>(InteropIdentities::default());
    // This isolate's link-callback registry (see [`LinkCallbacks`]): the styled-echo ops
    // register callbacks here; the engine resolves clicked ids back through it. Living in
    // `OpState` ties every `v8::Global` to its isolate's teardown.
    state.put::<SharedLinkCallbacks>(Rc::new(RefCell::new(LinkCallbacks::default())));
  },
);

/// `gmcp.enabled` (`docs/gmcp.md` §3.4): whether GMCP is negotiated on for the live
/// connection. Gated by `interop:read` like every other read of the `gmcp` producer — the
/// flag is protocol state a consumer observes, same as the tree.
#[op2(fast)]
fn op_smudgy_gmcp_enabled(state: &OpState) -> Result<bool, NotCapable> {
    ensure(grants(state).interop_read, "interop-read")?;
    Ok(state
        .borrow::<crate::session::runtime::gmcp::SharedGmcpEnabled>()
        .get())
}

/// Validate a script-supplied GMCP message/module name for the wire (`docs/gmcp.md`
/// §6.3): the name travels verbatim inside the subnegotiation, so a space would truncate
/// it server-side and control bytes have no business there. Loud, like every author-input
/// gate.
fn validate_gmcp_name(name: &str) -> Result<&str, StoreOpError> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(StoreOpError(format!(
            "smudgy: {name:?} is not a valid GMCP name (non-empty, no whitespace or \
             control characters)"
        )));
    }
    Ok(name)
}

/// `gmcp.send(name, data?)` (`docs/gmcp.md` §6.3) — ⟂ `gmcp:send`. `data` arrives
/// pre-serialized from the JS shim (`None` = no data part). The frame is written by the
/// dispatch arm, on the same socket queue as ordinary sends.
#[op2]
fn op_smudgy_gmcp_send(
    state: &mut OpState,
    #[string] name: &str,
    #[string] data: Option<String>,
) -> Result<(), StoreOpError> {
    ensure(grants(state).gmcp_send, "gmcp-send")?;
    let name = validate_gmcp_name(name)?;
    queue_own_action(
        state,
        RuntimeAction::GmcpSend {
            name: Arc::from(name),
            data: data.map(Arc::from),
        },
    );
    Ok(())
}

/// `gmcp.enableModule(name, version?)` — ⟂ `gmcp:send` (`docs/gmcp.md` §6.2). The
/// ref is keyed to the calling isolate, so a package reload releases its own refs and no
/// package can drop a module another still uses.
#[op2(fast)]
fn op_smudgy_gmcp_enable_module(
    state: &mut OpState,
    #[string] module: &str,
    #[smi] version: u32,
) -> Result<(), StoreOpError> {
    ensure(grants(state).gmcp_send, "gmcp-send")?;
    let module = validate_gmcp_name(module)?;
    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::GmcpEnableModule {
            isolate,
            module: Arc::from(module),
            version: version.max(1),
        },
    );
    Ok(())
}

/// `gmcp.disableModule(name)` — ⟂ `gmcp:send`.
#[op2(fast)]
fn op_smudgy_gmcp_disable_module(
    state: &mut OpState,
    #[string] module: &str,
) -> Result<(), StoreOpError> {
    ensure(grants(state).gmcp_send, "gmcp-send")?;
    let module = validate_gmcp_name(module)?;
    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::GmcpDisableModule {
            isolate,
            module: Arc::from(module),
        },
    );
    Ok(())
}

/// `gmcp.mergeKeys(...names)` — ⟂ `gmcp:send` (`docs/gmcp.md` §4.3): merge keys
/// change the retained-value semantics every consumer reads, a write-side act.
#[op2]
fn op_smudgy_gmcp_merge_keys(
    state: &mut OpState,
    #[serde] names: Vec<String>,
) -> Result<(), StoreOpError> {
    ensure(grants(state).gmcp_send, "gmcp-send")?;
    let mut validated = Vec::with_capacity(names.len());
    for name in &names {
        validated.push(validate_gmcp_name(name)?.to_string());
    }
    if !validated.is_empty() {
        queue_own_action(state, RuntimeAction::GmcpAddMergeKeys(Arc::new(validated)));
    }
    Ok(())
}

/// This isolate's `$DATA` dir, placed in `OpState` for [`op_smudgy_data_dir`] (`getDataDir()`).
pub struct PackageDataDir(pub std::path::PathBuf);

/// `getDataDir()`: the absolute path of this isolate's own data dir (`$DATA`) — where its
/// `read`/`write` grants resolve (`.isolate-storage/<pkg-slug>/data` for a sandboxed package, the
/// shared server dir for the main isolate). Ungated: knowing the path is not access; the fs ops
/// that use it are gated by the `read`/`write` permissions.
#[op2]
#[string]
fn op_smudgy_data_dir(state: &mut OpState) -> String {
    state
        .borrow::<PackageDataDir>()
        .0
        .to_string_lossy()
        .into_owned()
}

/// The smudgy op-capabilities granted to one isolate (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`) —
/// a `Copy` mirror of [`SmudgyCapabilities`], placed in the isolate's `OpState` and read by the
/// gated ops. The main/trusted isolate gets [`Self::all`]; a sandboxed isolate gets the grants
/// built from its consented capability set ([`Self::from_capabilities`]); the default (all-false)
/// denies every gated op (the `∅`/un-consented case).
// A flat set of independent capability flags (the taxonomy), not a state machine — the
// `struct_excessive_bools` suggestion (enums / state machine) doesn't fit.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SmudgyGrants {
    pub create_aliases: bool,
    pub create_triggers: bool,
    /// `session_send` (aliased / re-triggerable).
    pub send: bool,
    /// `session_send_raw` (raw, bypassing aliases).
    pub send_direct: bool,
    pub echo: bool,
    /// `get_sessions` + cross-session routing (acting on a non-own session).
    pub reach_others: bool,
    /// gag/insert/replace/highlight/remove + the `line_*` buffer variants.
    pub change_display: bool,
    pub mapper_read: bool,
    pub mapper_write: bool,
    pub widgets: bool,
    /// `interop: ["read"]` — read/watch session-store state + subscribe to any event
    /// (`sys:`/`map:`/package). Aliased by the legacy `events: ["subscribe"]`.
    pub interop_read: bool,
    /// `interop: ["write"]` — publish session-store state + emit events on the package's own
    /// namespace. Aliased by the legacy `events: ["emit"]`.
    pub interop_write: bool,
    /// `panes: ["create"]` — create/close/write session panes and route lines into them.
    pub panes: bool,
    /// `gmcp: ["send"]` — outbound GMCP: `gmcp.send`, module enable/disable, merge keys
    /// (`docs/gmcp.md` §6.3). The moral equivalent of `send`; rides with neither
    /// interop grant.
    pub gmcp_send: bool,
    /// `input: ["access"]` — the command-input surface (`docs/input.md` §3.6):
    /// read the input's mirrored state, rewrite/focus/mask/submit it, and manage its
    /// tab-completion word sets. One capability covers the whole surface — a package
    /// that wants any of it can see and rewrite what the user types.
    pub input: bool,
}

impl SmudgyGrants {
    /// Every capability granted — for the main isolate and trusted packages (ungated).
    #[must_use]
    pub fn all() -> Self {
        Self {
            create_aliases: true,
            create_triggers: true,
            send: true,
            send_direct: true,
            echo: true,
            reach_others: true,
            change_display: true,
            mapper_read: true,
            mapper_write: true,
            widgets: true,
            interop_read: true,
            interop_write: true,
            panes: true,
            gmcp_send: true,
            input: true,
        }
    }

    /// Build the runtime grant from a package's consented [`SmudgyCapabilities`] — a field-for-field
    /// projection (`mapper_write`'s implied `mapper_read` was already normalized at parse).
    #[must_use]
    pub fn from_capabilities(caps: &SmudgyCapabilities) -> Self {
        Self {
            create_aliases: caps.create_aliases,
            create_triggers: caps.create_triggers,
            send: caps.send,
            send_direct: caps.send_direct,
            echo: caps.echo,
            reach_others: caps.reach_others,
            change_display: caps.change_display,
            mapper_read: caps.mapper_read,
            mapper_write: caps.mapper_write,
            widgets: caps.widgets,
            interop_read: caps.interop_read,
            interop_write: caps.interop_write,
            panes: caps.panes,
            gmcp_send: caps.gmcp_send,
            input: caps.input,
        }
    }
}

/// The error a gated smudgy op throws when its capability was not granted to this package
/// (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`). Names the missing capability so an author sees
/// exactly what to declare — parity with deno's net/fs `NotCapable` denials. Throwing
/// (not a silent no-op) is deliberate: a denied call is an author bug they should fix by declaring
/// the capability.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
#[class(generic)]
#[error("smudgy: this package did not request the '{0}' capability")]
pub struct NotCapable(pub &'static str);

/// Throw [`NotCapable`] naming `cap` unless `allowed`. The gated ops below copy the relevant
/// boolean out of [`SmudgyGrants`] (it is `Copy`) and pass it here.
fn ensure(allowed: bool, cap: &'static str) -> Result<(), NotCapable> {
    if allowed {
        Ok(())
    } else {
        Err(NotCapable(cap))
    }
}

/// Read this isolate's grants out of `OpState` (always present — the `smudgy_ops` extension seeds
/// it at construction for every isolate).
fn grants(state: &OpState) -> SmudgyGrants {
    *state.borrow::<SmudgyGrants>()
}

/// Gate a session-targeting op (`echo`/`send`/`send_raw`): require the op's own capability
/// `primary` (named `primary_cap`), and additionally `reach_others` when `target` isn't the
/// caller's own session — cross-session action is the `reach-others` capability
/// (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`: "any route to a non-own session").
fn ensure_session_target(
    state: &OpState,
    target: SessionId,
    primary: bool,
    primary_cap: &'static str,
) -> Result<(), NotCapable> {
    ensure(primary, primary_cap)?;
    if target != *state.borrow::<SessionId>() {
        ensure(grants(state).reach_others, "reach-others")?;
    }
    Ok(())
}

pub struct Capture(pub bool);

/// The current function automation's fallthrough decision. `None` outside an alias/trigger
/// handler makes accidental async or top-level calls fail instead of mutating a future frame.
pub struct Fallthrough(pub Option<bool>);

#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
#[class(generic)]
#[error("fallthrough() may only be called inside an alias or trigger handler")]
struct FallthroughContextError;

/// The calling isolate's instantiation nonce (`ScriptEngine`'s process-wide counter),
/// parked in `OpState` at construction. The pane-input registration op stamps it onto
/// handler addresses; dispatch rejects a mismatch, so a handler can never be invoked
/// under an instantiation other than the one that registered it.
struct IsolateInstance(u64);

/// The session's server name, used to scope package param reads to `<server>/`.
pub struct ServerName(pub Arc<String>);

/// Read a package's param value by its specifier (`smudgy://owner/name`) and key.
/// Non-secret values come from `smudgy.params.json`, secrets from the OS keyring.
/// Returns null when the key is unset.
///
/// Sandboxed-package reads are gated to the caller's OWN namespace. The per-importer
/// `smudgy:params` virtual module bakes in the calling package's specifier, but the op
/// receives a plain string a hand-crafted call could forge, so it independently checks the
/// specifier against [`current_isolate`] (see [`param_read_allowed`]): a package isolate may
/// read only `smudgy://<its-owner>/<its-name>`, and any other specifier reads as unset — no
/// oracle for whether another package's key exists. The Main isolate (user scripts, local
/// modules, trusted packages) is allow-all and may read any specifier. Without this gate the
/// op was a cross-package confidentiality hole: a zero-capability sandboxed package could read
/// another same-server package's params and keyring secrets just by passing the victim's
/// specifier.
///
/// Manifest-blind (the op sees only specifier + key): it reads the non-secret store first,
/// then the keyring. A manifest `default` is not applied here. If a key exists in BOTH stores
/// (e.g. a param flips `secret` across versions and stale plaintext lingers), the non-secret
/// value shadows the secret; the stores are expected to stay mutually exclusive. A secret read
/// does synchronous keyring I/O per call with no caching, so a hot-path `get()` of a secret can
/// add latency on slow keyrings (fast on Windows).
#[op2]
#[serde]
fn op_smudgy_param_get(
    state: &mut OpState,
    #[string] specifier: String,
    #[string] key: String,
) -> Option<serde_json::Value> {
    if !param_read_allowed(&current_isolate(state), &specifier) {
        return None;
    }
    let server = state.borrow::<ServerName>().0.clone();
    crate::models::shared_packages::get_param_value(&server, &specifier, &key).or_else(|| {
        crate::models::shared_packages::load_secret_param(&server, &specifier, &key)
            .map(serde_json::Value::String)
    })
}

/// Whether the `isolate` making an [`op_smudgy_param_get`] call may read `specifier`'s params.
/// The Main isolate is trusted and may read any namespace; a sandboxed package may read only its
/// own `smudgy://owner/name` — the exact string the `smudgy:params` binding bakes in for it (its
/// owner/name, never the resolved version). This is the confidentiality boundary the per-importer
/// binding only *approximates*; enforcing it in the op is what makes the boundary real.
fn param_read_allowed(isolate: &IsolateId, specifier: &str) -> bool {
    match isolate {
        IsolateId::Main => true,
        IsolateId::Package { owner, name, .. } => specifier == format!("smudgy://{owner}/{name}"),
    }
}

/// Read the script-visible app settings (`getSettings()`): the command separator, raw-line
/// prefix, fonts, theme name, command-input behavior, and the resolved terminal palette.
/// Ungated — these are non-secret display/behavior settings, a benign read of the caller's own
/// context like [`op_smudgy_get_current_session`]. The snapshot is seeded at construction and
/// refreshed by `ApplySettings`, so the value reflects the latest applied settings.
#[op2]
#[serde]
fn op_smudgy_get_settings(state: &OpState) -> crate::models::settings::ScriptSettings {
    state
        .borrow::<crate::session::runtime::SettingsSnapshot>()
        .borrow()
        .clone()
}

// ============================================================================
// Persisted user-side automations: create/edit the regular, UI-visible aliases/triggers/hotkeys
// saved under `<server>/{aliases,triggers,hotkeys}/` (the same files the automations window edits),
// distinct from the ephemeral `createAlias`/`createTrigger`/`createHotkey` runtime automations.
// A write persists to disk AND reloads the affected sessions (the path the automations window
// uses) so the change takes effect. Gated to the MAIN isolate: only the user's own scripts, local
// modules, and TRUSTED packages may rewrite the user's saved config — a sandboxed (untrusted)
// package never can. Editing saved config is a trust-level power, not a per-capability one.
// ============================================================================

/// The error a persisted user-automation op throws: the caller isn't the Main isolate, the name is
/// rejected by the shared naming rule, or the disk read/write failed.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
#[class(generic)]
#[error("smudgy: {0}")]
pub struct UserAutomationError(String);

/// Gate a persisted user-automation op to the Main isolate. A sandboxed package isolate is
/// denied — rewriting the user's saved automations is reserved for code the user runs directly
/// (their own scripts/modules) or has explicitly trusted (trusted packages run on Main).
fn ensure_user_automation_access(state: &OpState) -> Result<(), UserAutomationError> {
    match current_isolate(state) {
        IsolateId::Main => Ok(()),
        IsolateId::Package { .. } => Err(UserAutomationError(
            "editing user automations is only available to your own scripts and trusted packages"
                .to_string(),
        )),
    }
}

/// Validate a persisted automation name against the same rule the automations window applies
/// ([`crate::models::naming::validate_name`]) — names become filenames, so an illegal one is
/// rejected exactly as the editor would reject it.
fn validate_user_automation_name(name: &str) -> Result<(), UserAutomationError> {
    crate::models::naming::validate_name(name)
        .map_err(|message| UserAutomationError(format!("invalid automation name: {message}")))
}

/// A disk read/write failure surfaces to the JS side as the op error.
impl From<AnyError> for UserAutomationError {
    fn from(error: AnyError) -> Self {
        Self(format!("failed to persist automation: {error:#}"))
    }
}

/// Ensure `<server>/<kind>/` exists before a `save_*` (which errors on a missing directory) — a
/// kind the user has never configured won't have its directory yet.
fn ensure_automation_dir(server: &str, kind: &str) -> Result<(), AnyError> {
    let dir = crate::get_smudgy_home()?.join(server).join(kind);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(())
}

/// The current session's server name; the persisted automations live under `<server>/`.
fn op_server_name(state: &OpState) -> String {
    state.borrow::<ServerName>().0.as_str().to_string()
}

/// Reload every OTHER live session on `server` so it re-loads its automations from disk — the
/// same effect the automations window's `ScriptsChanged` fan-out produces. The calling session is
/// deliberately skipped: reloading the session a script is running in would re-run all its modules
/// (and, for a top-level `save_*`, could cascade), so the change is persisted but the caller's own
/// session picks it up on its next reload (or via an explicit `reload()`). Best-effort: a
/// shut-down session's send is ignored and a poisoned registry lock is skipped rather than
/// panicking inside an op.
fn reload_other_sessions_for_server(state: &OpState, server: &str) {
    let own = *state.borrow::<SessionId>();
    if let Ok(sessions) = registry::get_registry().lock() {
        for (id, runtime) in sessions.iter() {
            if *id != own && runtime.server_name.as_str() == server {
                let _ = runtime.tx.send(RuntimeAction::Reload);
            }
        }
    }
}

/// Upsert `(name, def)` into the per-server alias map, persisting only when it actually changed.
/// Returns `true` when the saved set changed (a new entry or a different definition).
fn save_user_alias(server: &str, name: &str, def: AliasDefinition) -> Result<bool, AnyError> {
    let mut map = crate::models::aliases::load_aliases(server)?;
    if map.get(name) == Some(&def) {
        return Ok(false);
    }
    map.insert(name.to_string(), def);
    ensure_automation_dir(server, "aliases")?;
    crate::models::aliases::save_aliases(server, &map)?;
    Ok(true)
}

fn save_user_trigger(server: &str, name: &str, def: TriggerDefinition) -> Result<bool, AnyError> {
    let mut map = crate::models::triggers::load_triggers(server)?;
    if map.get(name) == Some(&def) {
        return Ok(false);
    }
    map.insert(name.to_string(), def);
    ensure_automation_dir(server, "triggers")?;
    crate::models::triggers::save_triggers(server, &map)?;
    Ok(true)
}

fn save_user_hotkey(server: &str, name: &str, def: HotkeyDefinition) -> Result<bool, AnyError> {
    let mut map = crate::models::hotkeys::load_hotkeys(server)?;
    if map.get(name) == Some(&def) {
        return Ok(false);
    }
    map.insert(name.to_string(), def);
    ensure_automation_dir(server, "hotkeys")?;
    crate::models::hotkeys::save_hotkeys(server, &map)?;
    Ok(true)
}

/// Remove `name` from the per-server map; `true` when an entry was actually removed.
fn delete_user_alias(server: &str, name: &str) -> Result<bool, AnyError> {
    let mut map = crate::models::aliases::load_aliases(server)?;
    if map.remove(name).is_none() {
        return Ok(false);
    }
    ensure_automation_dir(server, "aliases")?;
    crate::models::aliases::save_aliases(server, &map)?;
    Ok(true)
}

fn delete_user_trigger(server: &str, name: &str) -> Result<bool, AnyError> {
    let mut map = crate::models::triggers::load_triggers(server)?;
    if map.remove(name).is_none() {
        return Ok(false);
    }
    ensure_automation_dir(server, "triggers")?;
    crate::models::triggers::save_triggers(server, &map)?;
    Ok(true)
}

fn delete_user_hotkey(server: &str, name: &str) -> Result<bool, AnyError> {
    let mut map = crate::models::hotkeys::load_hotkeys(server)?;
    if map.remove(name).is_none() {
        return Ok(false);
    }
    ensure_automation_dir(server, "hotkeys")?;
    crate::models::hotkeys::save_hotkeys(server, &map)?;
    Ok(true)
}

/// `userAutomations.saveAlias` — create or replace a persisted alias. Returns `true` when the
/// saved set changed (a no-op equal save returns `false`). Besides persisting, the alias is made
/// live in the calling session immediately (an upsert under the disk keying `(Main, User, name)`,
/// so it coexists with and replaces a UI-loaded one) and the server's other sessions are reloaded.
#[op2]
fn op_smudgy_save_user_alias(
    state: &mut OpState,
    #[string] name: String,
    #[serde] def: AliasDefinition,
) -> Result<bool, UserAutomationError> {
    ensure_user_automation_access(state)?;
    validate_user_automation_name(&name)?;
    let server = op_server_name(state);
    let changed = save_user_alias(&server, &name, def.clone())?;
    if changed {
        reload_other_sessions_for_server(state, &server);
    }
    queue_own_action(
        state,
        RuntimeAction::AddAlias {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: Arc::new(name),
            alias: def,
            fire_limit: None,
        },
    );
    Ok(changed)
}

/// `userAutomations.saveTrigger` — create or replace a persisted trigger (also live in the
/// calling session, with the server's other sessions reloaded).
#[op2]
fn op_smudgy_save_user_trigger(
    state: &mut OpState,
    #[string] name: String,
    #[serde] def: TriggerDefinition,
) -> Result<bool, UserAutomationError> {
    ensure_user_automation_access(state)?;
    validate_user_automation_name(&name)?;
    let server = op_server_name(state);
    let changed = save_user_trigger(&server, &name, def.clone())?;
    if changed {
        reload_other_sessions_for_server(state, &server);
    }
    queue_own_action(
        state,
        RuntimeAction::AddTrigger {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: Arc::new(name),
            trigger: def,
            fire_limit: None,
            line_limit: None,
        },
    );
    Ok(changed)
}

/// `userAutomations.saveHotkey` — create or replace a persisted hotkey (also live in the calling
/// session, with the server's other sessions reloaded).
#[op2]
fn op_smudgy_save_user_hotkey(
    state: &mut OpState,
    #[string] name: String,
    #[serde] def: HotkeyDefinition,
) -> Result<bool, UserAutomationError> {
    ensure_user_automation_access(state)?;
    validate_user_automation_name(&name)?;
    let server = op_server_name(state);
    let changed = save_user_hotkey(&server, &name, def.clone())?;
    if changed {
        reload_other_sessions_for_server(state, &server);
    }
    queue_own_action(
        state,
        RuntimeAction::AddHotkey {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: Arc::new(name),
            hotkey: def,
            // The disk/inline-string path: dispatch compiles `hotkey.script` itself.
            function_id: None,
        },
    );
    Ok(changed)
}

/// `userAutomations.deleteAlias` — remove a persisted alias (and drop it from the calling
/// session's live set; the server's other sessions reload). Returns `true` when one existed.
#[op2(fast)]
fn op_smudgy_delete_user_alias(
    state: &mut OpState,
    #[string] name: String,
) -> Result<bool, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    let removed = delete_user_alias(&server, &name)?;
    if removed {
        reload_other_sessions_for_server(state, &server);
    }
    queue_own_action(
        state,
        RuntimeAction::RemoveAlias(IsolateId::Main, Origin::User, Arc::new(name)),
    );
    Ok(removed)
}

/// `userAutomations.deleteTrigger` — remove a persisted trigger (and drop it live).
#[op2(fast)]
fn op_smudgy_delete_user_trigger(
    state: &mut OpState,
    #[string] name: String,
) -> Result<bool, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    let removed = delete_user_trigger(&server, &name)?;
    if removed {
        reload_other_sessions_for_server(state, &server);
    }
    queue_own_action(
        state,
        RuntimeAction::RemoveTrigger(IsolateId::Main, Origin::User, Arc::new(name)),
    );
    Ok(removed)
}

/// `userAutomations.deleteHotkey` — remove a persisted hotkey (and drop it live).
#[op2(fast)]
fn op_smudgy_delete_user_hotkey(
    state: &mut OpState,
    #[string] name: String,
) -> Result<bool, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    let removed = delete_user_hotkey(&server, &name)?;
    if removed {
        reload_other_sessions_for_server(state, &server);
    }
    queue_own_action(
        state,
        RuntimeAction::RemoveHotkey(IsolateId::Main, Origin::User, Arc::new(name)),
    );
    Ok(removed)
}

/// `userAutomations.aliases.get` — read a persisted alias's definition, or null when absent.
#[op2]
#[serde]
fn op_smudgy_get_user_alias(
    state: &mut OpState,
    #[string] name: String,
) -> Result<Option<AliasDefinition>, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    Ok(crate::models::aliases::load_aliases(&server)?.remove(&name))
}

/// `userAutomations.triggers.get` — read a persisted trigger's definition, or null when absent.
#[op2]
#[serde]
fn op_smudgy_get_user_trigger(
    state: &mut OpState,
    #[string] name: String,
) -> Result<Option<TriggerDefinition>, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    Ok(crate::models::triggers::load_triggers(&server)?.remove(&name))
}

/// `userAutomations.hotkeys.get` — read a persisted hotkey's definition, or null when absent.
#[op2]
#[serde]
fn op_smudgy_get_user_hotkey(
    state: &mut OpState,
    #[string] name: String,
) -> Result<Option<HotkeyDefinition>, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    Ok(crate::models::hotkeys::load_hotkeys(&server)?.remove(&name))
}

/// `userAutomations.aliases.list` — the names of all persisted aliases (sorted).
#[op2]
#[serde]
fn op_smudgy_list_user_aliases(state: &mut OpState) -> Result<Vec<String>, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    let mut names: Vec<String> = crate::models::aliases::load_aliases(&server)?
        .into_keys()
        .collect();
    names.sort();
    Ok(names)
}

/// `userAutomations.triggers.list` — the names of all persisted triggers (sorted).
#[op2]
#[serde]
fn op_smudgy_list_user_triggers(state: &mut OpState) -> Result<Vec<String>, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    let mut names: Vec<String> = crate::models::triggers::load_triggers(&server)?
        .into_keys()
        .collect();
    names.sort();
    Ok(names)
}

/// `userAutomations.hotkeys.list` — the names of all persisted hotkeys (sorted).
#[op2]
#[serde]
fn op_smudgy_list_user_hotkeys(state: &mut OpState) -> Result<Vec<String>, UserAutomationError> {
    ensure_user_automation_access(state)?;
    let server = op_server_name(state);
    let mut names: Vec<String> = crate::models::hotkeys::load_hotkeys(&server)?
        .into_keys()
        .collect();
    names.sort();
    Ok(names)
}

/// Validate an automation name against the SAME rule the UI applies
/// ([`crate::models::naming::validate_name`]) — the one source of truth, so a name a
/// script gives an alias/trigger/hotkey/timer is accepted exactly when the same name
/// typed into the automations editor would be (spaces and friendly punctuation are
/// fine; only what is illegal/unsafe as a filename is rejected).
///
/// Pure (no `OpState`, no capability gate): it only inspects the string, so it is safe
/// for every isolate, sandboxed packages included. Returns `None` when the name is
/// acceptable, or `Some(message)` with the human-readable reason it is not — which the
/// JS wrapper rethrows as a `TypeError`.
#[op2]
#[string]
fn op_smudgy_validate_name(#[string] name: &str) -> Option<String> {
    crate::models::naming::validate_name(name).err()
}

// ============================================================================
// Events (`PACKAGE-EVENTS.md`): a host-routed pub/sub bus. `on` registers a handler (a FunctionId
// in the caller's isolate); `emit` stamps the caller's namespace onto the event name (so it is
// unforgeable) and queues a `CallJavascriptFunction` for every subscriber, depth-first.
// ============================================================================

// The event-delivery recursion cap (`MAX_EVENT_DEPTH`, analogous to the trigger depth guard)
// lives in the runtime module: the session store's watch dispatch shares it, since both ride
// the same host-routed delivery mechanism.

/// The current event-delivery depth, stashed into an isolate's `OpState` by
/// `call_javascript_function` before it runs a handler, so a re-emit from inside the handler queues
/// its subscribers one level deeper and the chain terminates at [`MAX_EVENT_DEPTH`].
#[derive(Clone, Copy, Default)]
pub struct EventDepth(pub u32);

/// One event subscriber: which isolate registered the handler and its `FunctionId` in that
/// isolate's `script_functions`. Cloned out before queueing so the registry borrow is released.
#[derive(Clone)]
pub struct EventSubscriber {
    pub isolate: IsolateId,
    pub function_id: FunctionId,
}

/// Session-global event bus: canonical event name -> subscribers. The SAME `Rc` is handed to every
/// isolate's ops (like [`SingletonRegistry`]), so delivery crosses isolates through the host. Built
/// per `ScriptEngine`, so a reload clears all subscriptions (the `PACKAGE-EVENTS.md` teardown gap).
pub type EventRegistry = Rc<RefCell<HashMap<String, Vec<EventSubscriber>>>>;

/// The uniform ASCII case fold (`docs/interop.md` §2) applied everywhere names are
/// structural — here, canonical event names at both registration and emission, so
/// `on("user#Ping")` hears `emit("ping")`. Already-lowercase names — the common case, and
/// every host event name — borrow through with no allocation; only a name with uppercase
/// ASCII pays for the folded copy.
pub(crate) fn fold_name(name: &str) -> std::borrow::Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(name.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

/// `on(event, handler)` — register a handler for a full canonical event name (`sys:connect`,
/// `map:room`, `smudgy://owner/name#x`, `user#x`), matched case-insensitively (the uniform fold).
/// Open pub/sub: any `interop:read`-granted package may listen to any emitter — except the
/// input surface: `sys:input` delivers (and lets handlers rewrite) what the user types, and
/// `input:change`/`input:focus` observe it, so those additionally require the `input`
/// capability, like the rest of the input surface. Subscribing to the observe events also
/// flags input-mirror interest (exactly like a mirror read): their emit site derives from
/// the mirror feed, so without interest the UI would never send the state changes the
/// subscriber is waiting on.
/// Registers the v8 function in this isolate's registry exactly like the trigger ops, then
/// records the subscription in the session-global bus. Returns the handler's `FunctionId`
/// index as a token the JS sugar hands back to [`op_smudgy_off`] to unsubscribe.
#[op2(fast)]
fn op_smudgy_on<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    #[string] event: &str,
    f: v8::Local<'s, v8::Function>,
) -> Result<u32, NotCapable> {
    ensure(grants(state).interop_read, "interop:read")?;
    match fold_name(event).as_ref() {
        "sys:input" => ensure(grants(state).input, "input")?,
        "input:change" | "input:focus" => {
            ensure(grants(state).input, "input")?;
            let mirror = state
                .borrow::<crate::session::runtime::SharedInputMirror>()
                .clone();
            let flipped = mirror.borrow_mut().flag_interest();
            if flipped {
                queue_own_action(state, RuntimeAction::InputMirrorInterest);
            }
        }
        _ => {}
    }
    let f = v8::Global::new(scope, f);
    let function_id = {
        let mut script_functions = state
            .borrow::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>()
            .borrow_mut();
        let id = FunctionId(script_functions.len());
        script_functions.push(f);
        id
    };
    let isolate = current_isolate(state);
    state
        .borrow::<EventRegistry>()
        .borrow_mut()
        .entry(fold_name(event).into_owned())
        .or_default()
        .push(EventSubscriber {
            isolate,
            function_id,
        });
    // The function index doubles as the subscription token. An isolate will never register
    // `u32::MAX` functions in a session (it would exhaust memory first), so the saturating
    // conversion is unreachable — it only keeps the cast truncation-clean for clippy::pedantic.
    Ok(u32::try_from(function_id.0).unwrap_or(u32::MAX))
}

/// `off(event, id)` — cancel a subscription created by [`op_smudgy_on`]. `id` is the token `on`
/// returned (the handler's `FunctionId` index). Removal is scoped to `(current_isolate, id)`, so a
/// package can only drop its OWN subscriptions: another isolate's `id` addresses that isolate's
/// `script_functions`, so it can never match here. Idempotent — unsubscribing an already-removed (or
/// never-registered) handler is a no-op. Gated on `interop:read`, symmetric with `on`.
///
/// Only the registry entry is dropped; the handler's `v8::Global` stays in the append-only
/// `script_functions` (like the trigger/hotkey remove ops, it is never reclaimed mid-session — that is
/// what keeps `FunctionId` tokens stable so a stale `off` can't cancel a later subscription). It is
/// reclaimed when the engine is rebuilt on reload.
#[op2(fast)]
fn op_smudgy_off(
    state: &mut OpState,
    #[string] event: &str,
    function_id: u32,
) -> Result<(), NotCapable> {
    ensure(grants(state).interop_read, "interop:read")?;
    let isolate = current_isolate(state);
    // `usize: From<u32>` doesn't exist (usize may be 16-bit); `try_from` always succeeds on real
    // targets, and an `id` that doesn't fit can't name any subscriber, so bail to a no-op.
    let Ok(target) = usize::try_from(function_id).map(FunctionId) else {
        return Ok(());
    };
    if let Some(subscribers) = state
        .borrow::<EventRegistry>()
        .borrow_mut()
        .get_mut(fold_name(event).as_ref())
    {
        subscribers.retain(|sub| !(sub.isolate == isolate && sub.function_id == target));
    }
    Ok(())
}

/// `emit(eventId, payloadJson)` — broadcast an event by its interned identity: the stamp
/// (`<producer>#<name>`, so a producer can only emit its own), the routing fold, and the
/// interop **home gate** verdict (`docs/interop.md` §3) were all resolved at handle
/// construction. Only the emitter's home instance may emit, so a code-imported copy running
/// under the right origin in the wrong isolate broadcasts nothing (a no-op with a one-time
/// teaching diagnostic, not a throw — the copy's code is usually a library the importer
/// wants, not a bug site). Queues a `CallJavascriptFunction` for each subscriber
/// depth-first (the payload was pre-stringified by the JS sugar). Capped at
/// [`MAX_EVENT_DEPTH`] to break emit cycles.
#[op2(fast)]
fn op_smudgy_emit(
    state: &mut OpState,
    event_id: u32,
    #[string] payload_json: &str,
) -> Result<(), StoreOpError> {
    ensure(grants(state).interop_write, "interop:write")?;
    let event = interned_event(state, event_id)?;
    if !event.is_home {
        let isolate = current_isolate(state);
        warn_non_home_write(state, &event.producer, isolate, "emit");
        return Ok(());
    }
    // Tier-2 catalogue sample at the emission choke point (`docs/interop.md`
    // §10): a bounded insert, recorded whether or not anyone subscribes — presence and
    // history never depend on listeners. The sender of an event is its producer; every key
    // string is shared from the interned entry (refcount bumps, no per-sample allocation).
    state
        .borrow::<crate::session::runtime::SharedCatalogue>()
        .borrow_mut()
        .sample_interned(
            &event.producer_spec,
            crate::session::runtime::catalogue::CatalogueKind::Event,
            &event.name,
            &event.name_folded,
            &event.producer_spec,
            payload_json,
        );
    let depth = state.try_borrow::<EventDepth>().map_or(0, |d| d.0);
    if depth >= MAX_EVENT_DEPTH {
        log::warn!(
            "smudgy: event recursion limit reached emitting {}; dropping",
            event.canonical
        );
        return Ok(());
    }
    // Clone the subscriber list out so the registry borrow is released before queueing (the queue
    // drain + a subscriber's handler may both touch the registry).
    let subscribers: Vec<EventSubscriber> = state
        .borrow::<EventRegistry>()
        .borrow()
        .get(&event.canonical)
        .map_or_else(Vec::new, Clone::clone);
    if subscribers.is_empty() {
        return Ok(());
    }
    // One capture list shared across the whole fan-out: the captures are identical for
    // every subscriber (only the target isolate/function differ), so the stamped-name and
    // payload copies are made once per emit, never once per subscriber. Handlers receive
    // the ORIGINAL stamped spelling — a script that branches on the event name must see the
    // name as emitted, not the lowercased routing key.
    let matches = Arc::new(vec![
        MatchCapture {
            name: Some(std::borrow::Cow::Borrowed("event")),
            value: event.stamped.clone(),
        },
        MatchCapture {
            name: Some(std::borrow::Cow::Borrowed("payload")),
            value: payload_json.to_string(),
        },
    ]);
    for sub in subscribers {
        queue_own_action(
            state,
            RuntimeAction::CallJavascriptFunction {
                isolate: sub.isolate,
                id: sub.function_id,
                matches: Arc::clone(&matches),
                depth: depth + 1,
                is_captured: None,
            },
        );
    }
    Ok(())
}

// ============================================================================
// Session store (`docs/interop.md` §2): a host-held, session-scoped tree of
// JSON values. `set` journals a write for the calling producer (turn-batched; the runtime
// flushes per turn); `get` reads a snapshot synchronously with read-your-writes; `watch` is the
// turn-coalesced change subscription. Writes pass three gates at this one choke point: the
// `interop:write` capability, the home-instance gate (origin AND home isolate), and the
// per-producer budgets.
// ============================================================================

/// The error a store op throws: a capability denial, a malformed path/producer/value (author
/// bugs — loud, never silent), or a budget breach (the diagnostic names the producer).
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
#[class(generic)]
#[error("{0}")]
pub struct StoreOpError(String);

impl From<NotCapable> for StoreOpError {
    fn from(err: NotCapable) -> Self {
        Self(err.to_string())
    }
}

/// Resolve a consumer-side producer spec (`"user"`, `"gmcp"`, or `"smudgy://owner/name"`) or
/// throw.
fn parse_producer(spec: &str) -> Result<store::ProducerKey, StoreOpError> {
    store::ProducerKey::parse(spec).ok_or_else(|| {
        StoreOpError(format!(
            "smudgy: unknown store producer {spec:?} (expected \"user\", \"gmcp\", or \
             \"smudgy://owner/name\")"
        ))
    })
}

/// Parse a store path or throw the grammar error verbatim.
fn parse_path(path: &str) -> Result<store::StorePath, StoreOpError> {
    store::StorePath::parse(path).map_err(|err| StoreOpError(format!("smudgy: {err}")))
}

// ============================================================================
// Interop op identity (`docs/interop.md` §3): the per-call constants of the
// interop ops — the parsed creator descriptor, the producer key, the root path, the event
// stamp+fold, the home-gate verdict — are interned ONCE where the JS handles are
// constructed, and the per-call ops address them by u32 id. The table lives in
// per-isolate `OpState`; the JS closures holding its ids live in the same isolate; an
// engine rebuild destroys both sides atomically, so a stale id is structurally impossible
// and the ids carry no generation nonce (nonces stay scoped to engine-escaping tokens:
// widget bindings). Resolution is ungated on both seats: an id grants nothing — every op
// that consumes one still enforces its own capability and (for writes) the home gate per
// call. Because the resolves are ungated, the table itself is hardened: interning is
// **deduped** (resolving an identity again returns its existing id, so per-construction
// resolution retains no additional host memory) and **capped** per isolate
// ([`MAX_INTEROP_IDENTITIES`] — a capability-free caller looping distinct specs cannot grow
// host memory without bound). Root ids and event ids live in disjoint tagged spaces
// ([`EVENT_ID_TAG`]), so an id presented to the wrong op family fails loudly instead of
// resolving whatever happens to sit at that index in the other table.
// ============================================================================

/// The producer seat a creator descriptor resolves to: the automation-keying [`Origin`]
/// plus the home-gate verdict for THIS isolate, cached at resolve time. Caching the verdict
/// is sound because the home registry is fixed per engine run (see [`store::HomeRegistry`],
/// whose doc ties the deferred runtime home registration to invalidating this cache).
#[derive(Clone)]
struct CreatorSeat {
    origin: Origin,
    is_home: bool,
}

/// One interned root — what a state handle, procedure handle, or the store glue addresses
/// per call. The entry is a **root**, not merely a producer: a producer's live head or its
/// retained previous generation ([`RootView`]), addressed at a constant `root_path` prefix;
/// host-pinned views join the same id space later
/// (`docs/interop.md` §14).
struct InteropRoot {
    producer: store::ProducerKey,
    /// The producer's display spec (`"user"` / `"smudgy://owner/name"`), interned as the
    /// catalogue's shared key form (`Arc<str>`) so the catalogue and diagnostic paths key
    /// off it with refcount bumps instead of re-allocating `to_string()` per call.
    producer_spec: Arc<str>,
    /// The constant path prefix under the producer subtree (empty = the subtree root); the
    /// per-call subpath — the genuinely dynamic part — joins onto it.
    root_path: store::StorePath,
    /// Present when the root was resolved from a creator descriptor (the producer seat);
    /// `None` on consumer-spec and previous-generation roots, which the write-shaped ops
    /// refuse.
    seat: Option<CreatorSeat>,
    /// Which state the read ops resolve against (writes are head-only by construction:
    /// previous roots carry no seat).
    view: RootView,
}

/// The store state an interned root addresses (`docs/interop.md` §2). The id
/// addresses the *role*, not a pinned `Arc`: a previous-view read resolves the calling
/// isolate's diff base at that moment, through the two-armed, seat-aware anchor
/// (`store::SessionStore::previous_anchor`). For the isolate mid-batch (its own writes for
/// this producer in the journal) the anchor is that batch's committed base; it moves at the
/// isolate's *first write of a new batch* — the arms switch from the retained generation to
/// the committed head — and again at the flush, where the base it read becomes the newly
/// retained generation. For every other reader the open journal is invisible, so the anchor
/// is always the retained generation and moves only at a flush that commits this producer's
/// writes. A previous-view proxy held across either boundary re-resolves against the new
/// base: it follows the newest batch rather than pinning the generation it was minted over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootView {
    /// The producer's live head, under `get`'s read-your-writes visibility.
    Head,
    /// The producer's previous generation: the state before the newest write batch the
    /// *reader* can observe — resolved seat-aware, per read
    /// (`store::SessionStore::previous_anchor`).
    Previous,
}

/// One interned event identity: the `(original stamped, folded canonical)` pair plus the
/// cached home verdict, so `emit(event_id, payload)` skips the per-call `format!` + fold.
struct InteropEvent {
    producer: store::ProducerKey,
    producer_spec: Arc<str>,
    is_home: bool,
    /// The handle-local name, original spelling (what the catalogue displays).
    name: Arc<str>,
    /// The handle-local name's ASCII fold — the catalogue's per-sample key, interned here
    /// so `emit`'s sample costs refcount bumps, not per-call key allocation.
    name_folded: Arc<str>,
    /// The stamped canonical name `<producer>#<name>`, original spelling (what handlers
    /// receive).
    stamped: String,
    /// The folded routing key the subscriber registry is keyed by.
    canonical: String,
}

/// The dedup key of one interned root — exactly the inputs its resolve op interns from, so
/// resolving the same identity twice yields the same id and the same entry. Path spellings
/// are kept distinct on purpose: the store records first-published casing, so two spellings
/// of one folded path are semantically distinct roots (both bounded by distinct call-site
/// strings).
#[derive(PartialEq, Eq, Hash)]
enum RootIdentity {
    /// A creator descriptor, keyed by its parsed [`Origin`] (spelling-independent — two JSON
    /// spellings of one origin are one identity).
    Creator(Origin),
    /// A producer state root: the creator's interned id plus the exact root-path spelling.
    ProducerRoot {
        creator: u32,
        path: store::StorePath,
    },
    /// A consumer root: the folded producer key plus the exact root-path spelling.
    Consumer {
        producer: store::ProducerKey,
        path: store::StorePath,
    },
    /// The previous-generation view of an already-interned root, keyed by that root's id
    /// (ids are stable for the isolate's life, so the key is as canonical as the entry).
    Previous { base: u32 },
}

/// Hard cap on interned identities per isolate (roots and events combined). Resolution is
/// deliberately ungated (an id grants nothing) and deduped (repeated construction of the
/// same handle interns nothing), so only unbounded *distinct* identities — dynamically
/// minted handle names, or a capability-free caller looping the resolve ops over fresh
/// specs — can approach this. Generous by orders of magnitude: real sessions intern a
/// handful of identities per module. At the cap, new identities are refused with a teaching
/// error while every already-interned handle keeps resolving.
const MAX_INTEROP_IDENTITIES: usize = 16_384;

/// Tag bit distinguishing event ids from root ids. Both are plain JS numbers on the wire, so
/// without the tag an id passed to the wrong op family would silently resolve whenever the
/// index happened to be in range in the other table — a misroute, not an error. Event ids
/// carry this bit; root ids never do ([`MAX_INTEROP_IDENTITIES`] keeps every index far below
/// it).
const EVENT_ID_TAG: u32 = 1 << 31;

/// The per-isolate interning table, seeded empty at extension construction. Entries are
/// `Rc` so an op clones its entry out with a refcount bump and releases the `OpState`
/// borrow before doing its work. Append-only, but deduped and capped (see
/// [`MAX_INTEROP_IDENTITIES`]), so its size is bounded by distinct identities, never by
/// call volume.
#[derive(Default)]
struct InteropIdentities {
    roots: Vec<Rc<InteropRoot>>,
    events: Vec<Rc<InteropEvent>>,
    /// Dedup index over `roots`, keyed by the resolve inputs.
    root_ids: HashMap<RootIdentity, u32>,
    /// Dedup index over `events`, keyed by `(creator root id, exact event name)`. Exact
    /// (unfolded) names stay distinct because handlers receive the original spelling as
    /// emitted; the duplicate-handle echo already flags case-fold twins.
    event_ids: HashMap<(u32, String), u32>,
}

impl InteropIdentities {
    /// Refuse a new entry once the table holds [`MAX_INTEROP_IDENTITIES`] identities. The
    /// teaching error names the pattern: identities intern once per distinct handle, so
    /// hitting the cap means names/paths are being minted dynamically without bound.
    fn ensure_capacity(&self) -> Result<(), StoreOpError> {
        if self.roots.len() + self.events.len() >= MAX_INTEROP_IDENTITIES {
            return Err(StoreOpError(format!(
                "smudgy: this isolate's interop identity table is full \
                 ({MAX_INTEROP_IDENTITIES} distinct handles/roots) \u{2014} handle identities \
                 intern once per distinct (creator, name/path) and repeated construction \
                 reuses the entry, so reaching the cap means handle names or paths are \
                 being minted dynamically without bound; reuse a fixed set of handles"
            )));
        }
        Ok(())
    }
}

/// Look an interned root up by id. An unknown id is a forged or corrupted call — the
/// host-minted ids JS handles carry are always in range — and an event-tagged id is a call
/// wired to the wrong id family.
fn interned_root(state: &OpState, id: u32) -> Result<Rc<InteropRoot>, StoreOpError> {
    if id & EVENT_ID_TAG != 0 {
        return Err(StoreOpError(format!(
            "smudgy: interop id {id} names an event, not a root"
        )));
    }
    usize::try_from(id)
        .ok()
        .and_then(|index| {
            state
                .borrow::<InteropIdentities>()
                .roots
                .get(index)
                .cloned()
        })
        .ok_or_else(|| StoreOpError(format!("smudgy: unknown interop root id {id}")))
}

/// Event counterpart of [`interned_root`].
fn interned_event(state: &OpState, id: u32) -> Result<Rc<InteropEvent>, StoreOpError> {
    if id & EVENT_ID_TAG == 0 {
        return Err(StoreOpError(format!(
            "smudgy: interop id {id} names a root, not an event"
        )));
    }
    usize::try_from(id & !EVENT_ID_TAG)
        .ok()
        .and_then(|index| {
            state
                .borrow::<InteropIdentities>()
                .events
                .get(index)
                .cloned()
        })
        .ok_or_else(|| StoreOpError(format!("smudgy: unknown interop event id {id}")))
}

/// The error for a root id that names a consumer root where a creator (producer-seated)
/// root is required.
fn not_a_creator(id: u32) -> StoreOpError {
    StoreOpError(format!(
        "smudgy: interop id {id} does not name a creator-seated root"
    ))
}

/// The interned creator [`Origin`] for the automation ops, which key automations by
/// `(isolate, origin, name)`. The origin arrives pre-parsed by
/// [`op_smudgy_interop_resolve_creator`] — strict, at construction — so a malformed
/// creator fails loudly before any automation op runs (unreachable in practice: the
/// creator JSON is host-minted per module).
fn creator_origin(state: &OpState, creator_id: u32) -> Result<Origin, StoreOpError> {
    let root = interned_root(state, creator_id)?;
    match &root.seat {
        Some(seat) => Ok(seat.origin.clone()),
        None => Err(not_a_creator(creator_id)),
    }
}

/// Intern `root` under `identity`, returning its id: the existing id when the identity is
/// already interned (resolution is idempotent), a fresh one otherwise. Errs at
/// [`MAX_INTEROP_IDENTITIES`].
fn intern_root(
    state: &mut OpState,
    identity: RootIdentity,
    root: InteropRoot,
) -> Result<u32, StoreOpError> {
    let identities = &mut *state.borrow_mut::<InteropIdentities>();
    if let Some(&id) = identities.root_ids.get(&identity) {
        return Ok(id);
    }
    identities.ensure_capacity()?;
    // The cap bounds the table far below u32 range (and below EVENT_ID_TAG), so the index
    // always fits untagged; the saturation keeps the cast truncation-clean for
    // clippy::pedantic.
    let id = u32::try_from(identities.roots.len()).unwrap_or(u32::MAX);
    identities.roots.push(Rc::new(root));
    identities.root_ids.insert(identity, id);
    Ok(id)
}

/// Event counterpart of [`intern_root`]: intern `event` under `(creator id, exact name)`,
/// returning its [`EVENT_ID_TAG`]-tagged id.
fn intern_event(
    state: &mut OpState,
    identity: (u32, String),
    event: InteropEvent,
) -> Result<u32, StoreOpError> {
    let identities = &mut *state.borrow_mut::<InteropIdentities>();
    if let Some(&id) = identities.event_ids.get(&identity) {
        return Ok(id);
    }
    identities.ensure_capacity()?;
    // See `intern_root` for why the saturation is unreachable.
    let id = EVENT_ID_TAG | u32::try_from(identities.events.len()).unwrap_or(u32::MAX);
    identities.events.push(Rc::new(event));
    identities.event_ids.insert(identity, id);
    Ok(id)
}

/// `resolveCreator(creatorJson) -> id` — intern a creator descriptor, called once per
/// module/API construction (`__smudgy_make_api`), never per call. The strict parse makes a
/// malformed creator fail loudly **at construction** on every copy, before any per-call op
/// runs (`docs/interop.md` §3's fail-loud rule). The returned id is also a root: the
/// creator's producer head at the subtree root, which is exactly what the store glue's
/// producer-relative calls address.
#[op2(fast)]
fn op_smudgy_interop_resolve_creator(
    state: &mut OpState,
    #[string] creator: &str,
) -> Result<u32, StoreOpError> {
    let origin = Origin::try_from_creator_json(creator)
        .map_err(|e| StoreOpError(format!("smudgy: malformed interop creator: {e}")))?;
    let producer = store::ProducerKey::from_origin(&origin);
    let isolate = current_isolate(state);
    let is_home = store::is_home(state.borrow::<store::HomeRegistry>(), &producer, &isolate);
    let identity = RootIdentity::Creator(origin.clone());
    let root = InteropRoot {
        producer_spec: Arc::from(producer.to_string()),
        producer,
        root_path: store::StorePath::root(),
        seat: Some(CreatorSeat { origin, is_home }),
        view: RootView::Head,
    };
    intern_root(state, identity, root)
}

/// `resolveProducerRoot(creatorId, rootPath) -> id` — intern a producer state handle's
/// `(producer, root path)` pair at construction; the per-call ops then take
/// `(root_id, subpath)`.
#[op2(fast)]
fn op_smudgy_interop_resolve_producer_root(
    state: &mut OpState,
    creator_id: u32,
    #[string] root_path: &str,
) -> Result<u32, StoreOpError> {
    let creator = interned_root(state, creator_id)?;
    if creator.seat.is_none() {
        return Err(not_a_creator(creator_id));
    }
    let path = parse_path(root_path)?;
    let identity = RootIdentity::ProducerRoot {
        creator: creator_id,
        path: path.clone(),
    };
    let root = InteropRoot {
        producer: creator.producer.clone(),
        producer_spec: creator.producer_spec.clone(),
        root_path: path,
        seat: creator.seat.clone(),
        view: RootView::Head,
    };
    intern_root(state, identity, root)
}

/// `resolveConsumerRoot(producerSpec, rootPath) -> id` — the consumer-side resolve feeding
/// the same table: consumer handles address a producer's head by spec with no creator, so
/// their roots carry no seat and the write-shaped ops refuse them. Ungated like every
/// resolve (addressing is not reading; the read ops gate `interop:read` per call).
#[op2(fast)]
fn op_smudgy_interop_resolve_consumer_root(
    state: &mut OpState,
    #[string] producer: &str,
    #[string] root_path: &str,
) -> Result<u32, StoreOpError> {
    let producer = parse_producer(producer)?;
    let path = parse_path(root_path)?;
    let identity = RootIdentity::Consumer {
        producer: producer.clone(),
        path: path.clone(),
    };
    let root = InteropRoot {
        producer_spec: Arc::from(producer.to_string()),
        producer,
        root_path: path,
        seat: None,
        view: RootView::Head,
    };
    intern_root(state, identity, root)
}

/// `resolvePreviousRoot(baseRootId) -> id` — intern the previous-generation view of an
/// already-interned root (`docs/interop.md` §2): same producer, same constant
/// path prefix, no seat (the view is read-only on both seats — the write-shaped ops refuse
/// it), resolving reads against the reader's previous anchor ([`RootView::Previous`])
/// instead of the head. Ungated like every resolve; deduped per base root.
#[op2(fast)]
fn op_smudgy_interop_resolve_previous_root(
    state: &mut OpState,
    base_id: u32,
) -> Result<u32, StoreOpError> {
    let base = interned_root(state, base_id)?;
    if base.view == RootView::Previous {
        // One generation is retained; "previous of previous" names nothing deeper and would
        // silently alias the same view — refuse it so the id space stays one-to-one.
        return Err(StoreOpError(format!(
            "smudgy: interop root id {base_id} already names a previous-generation view"
        )));
    }
    let identity = RootIdentity::Previous { base: base_id };
    let root = InteropRoot {
        producer: base.producer.clone(),
        producer_spec: base.producer_spec.clone(),
        root_path: base.root_path.clone(),
        seat: None,
        view: RootView::Previous,
    };
    intern_root(state, identity, root)
}

/// `resolveEvent(creatorId, name) -> id` — intern an event producer handle's identity at
/// construction (stamp, fold, home verdict), so each `emit` is an index lookup.
#[op2(fast)]
fn op_smudgy_interop_resolve_event(
    state: &mut OpState,
    creator_id: u32,
    #[string] name: &str,
) -> Result<u32, StoreOpError> {
    let creator = interned_root(state, creator_id)?;
    let Some(seat) = creator.seat.clone() else {
        return Err(not_a_creator(creator_id));
    };
    // The canonical event name is `<producer>#<local>` (`user#…` or `smudgy://owner/name#…`).
    // Routing uses the fold (case-insensitive matching), but handlers receive the ORIGINAL
    // spelling — a script that branches on the event name must see the name as emitted.
    let stamped = format!("{}#{name}", creator.producer);
    let name_arc: Arc<str> = Arc::from(name);
    // The catalogue's per-sample key form, shared with the display spelling when the fold
    // is the identity (the common case).
    let name_folded = match fold_name(name) {
        std::borrow::Cow::Borrowed(_) => Arc::clone(&name_arc),
        std::borrow::Cow::Owned(folded) => Arc::from(folded),
    };
    let event = InteropEvent {
        producer: creator.producer.clone(),
        producer_spec: creator.producer_spec.clone(),
        is_home: seat.is_home,
        name: name_arc,
        name_folded,
        canonical: fold_name(&stamped).into_owned(),
        stamped,
    };
    intern_event(state, (creator_id, name.to_string()), event)
}

/// Combine an interned root's constant path prefix with a call's dynamic subpath.
fn resolve_root_path(root: &InteropRoot, subpath: &str) -> Result<store::StorePath, StoreOpError> {
    let sub = parse_path(subpath)?;
    root.root_path
        .joined(sub)
        .map_err(|err| StoreOpError(format!("smudgy: {err}")))
}

/// The one-time teaching diagnostic for a home-gated interop write that was refused: echoed to
/// the session (once per producer × isolate per engine run) and logged every time. A refused
/// write is a **no-op, not a throw** — the code making it is usually a code-imported copy the
/// importer wants as a library, and interop is the one part that must not run twice.
fn warn_non_home_write(
    state: &mut OpState,
    producer: &store::ProducerKey,
    isolate: IsolateId,
    verb: &str,
) {
    log::warn!(
        "smudgy: interop {verb} by {producer} ignored in {isolate:?}: not its home instance"
    );
    let first = state
        .borrow::<crate::session::runtime::SharedSessionStore>()
        .borrow_mut()
        .note_non_home_write(producer.clone(), isolate);
    if first {
        queue_own_action(
            state,
            RuntimeAction::Echo(Arc::new(format!(
                "[interop] {producer}: {verb} ignored \u{2014} this copy of the package is not \
                 its installed (home) instance, so it can read shared state but not publish. \
                 If you code-imported it, import types only or consume its published state instead."
            ))),
        );
    }
}

/// The shared interop **write** gate for `set` and procedure receipt (emit carries its own
/// cached verdict on the event entry): the strict creator parse runs at construction
/// ([`op_smudgy_interop_resolve_creator`]), so per call this is the interned seat's cached
/// home verdict — an index + bool with semantics identical to a per-call
/// parse-plus-registry-lookup (`docs/interop.md` §3). `Ok(true)` ⇒ proceed;
/// `Ok(false)` ⇒ the write is refused as non-home (a no-op; the one-time teaching diagnostic
/// was already emitted); `Err` ⇒ the root carries no producer seat (a consumer root — only a
/// forged call can present one here, since the handles never write through consumer roots).
///
/// The residual this gate does **not** close: on the main isolate the host
/// cannot tell which code is calling — user scripts, local modules, and trusted packages
/// share one isolate — so any main-isolate caller may present a trusted package's creator
/// and pass as that package. That is attribution forgery, not a capability escape (main is
/// already allow-all), and it is inherent to the shared main isolate rather than a gap this
/// gate could plug (`interop.md` §3).
fn gate_interop_write(
    state: &mut OpState,
    root: &InteropRoot,
    verb: &str,
) -> Result<bool, StoreOpError> {
    if root.view == RootView::Previous {
        return Err(StoreOpError(format!(
            "smudgy: interop {verb} through a read-only previous-generation view"
        )));
    }
    let Some(seat) = &root.seat else {
        return Err(StoreOpError(format!(
            "smudgy: interop {verb} through a read-only consumer root"
        )));
    };
    if seat.is_home {
        Ok(true)
    } else {
        let isolate = current_isolate(state);
        warn_non_home_write(state, &root.producer, isolate, verb);
        Ok(false)
    }
}

/// `set(rootId, subpath, valueJson)` — replace the subtree at the interned root's path +
/// `subpath` in that root's producer subtree (set-at-path is the only write op). The write
/// lands in the turn's journal: same-isolate reads see it immediately (read-your-writes);
/// everyone else sees it after the end-of-turn flush, which always precedes the next
/// dispatched action. Budget breaches throw, naming the producer; non-home copies no-op
/// with a teaching diagnostic.
#[op2(fast)]
fn op_smudgy_store_set(
    state: &mut OpState,
    root_id: u32,
    #[string] subpath: &str,
    #[string] value_json: &str,
) -> Result<(), StoreOpError> {
    ensure(grants(state).interop_write, "interop:write")?;
    let root = interned_root(state, root_id)?;
    // Validate the path and value BEFORE the home gate, so a malformed path/value fails loudly on
    // every copy — including a non-home (e.g. code-imported) one an author develops against.
    // Otherwise the gate's early no-op return would mask the bug until the package is installed
    // and becomes home, i.e. first in end users' sessions.
    let path = resolve_root_path(&root, subpath)?;
    let value: serde_json::Value = serde_json::from_str(value_json)
        .map_err(|err| StoreOpError(format!("smudgy: store value must be JSON: {err}")))?;
    if !gate_interop_write(state, &root, "state write")? {
        return Ok(());
    }
    // Tier-1 catalogue observation (`docs/interop.md` §10): an ad-hoc store key
    // is catalogued at first write with provenance "undeclared" (an insert-if-absent — a
    // declared handle's entry already exists). The key is the write's root segment; a
    // whole-subtree root write catalogues each top-level key it publishes. The producer
    // spec is borrowed from the interned root — no per-write re-allocation.
    {
        let mut catalogue = state
            .borrow::<crate::session::runtime::SharedCatalogue>()
            .borrow_mut();
        if let Some(first) = path.segments().first() {
            catalogue.observe_state_key(&root.producer_spec, first);
        } else if let Some(map) = value.as_object() {
            for key in map.keys() {
                catalogue.observe_state_key(&root.producer_spec, key);
            }
        }
    }
    let isolate = current_isolate(state);
    let depth = state.try_borrow::<EventDepth>().map_or(0, |d| d.0);
    let outcome = state
        .borrow::<crate::session::runtime::SharedSessionStore>()
        .borrow_mut()
        .set(root.producer.clone(), path, value, isolate, depth)
        .map_err(|err| {
            log::warn!("smudgy: {err}");
            StoreOpError(format!("smudgy: {err}"))
        })?;
    if outcome.first_duplicate_key_collapse {
        queue_own_action(
            state,
            RuntimeAction::Echo(Arc::new(format!(
                "[interop] {}: a published object contained two case-fold-equal \
                 spellings of one key (keys are case-insensitive); the later value won",
                root.producer_spec
            ))),
        );
    }
    Ok(())
}

/// Debug-build guard against a previous-generation root id reaching a head read op. The
/// head ops answer only [`RootView::Head`] — the previous view has its own op family
/// (`op_smudgy_store_previous_get` and siblings) — and the glue always routes previous ids
/// to the previous ops, so only a miswired call can trip this. A `debug_assert!` rather
/// than a release check: the head-read regression the op split removes is a codegen/layout
/// effect, and even a never-taken compare-and-refuse arm in the op body re-introduces
/// measurable per-read cost, so the release bodies stay single-call with no view arm — and
/// a release misroute yields a head read of state a consumer root already grants, not an
/// escalation (writes keep their hard `RootView::Previous` refusal in
/// [`gate_interop_write`]).
fn debug_assert_head_root(root: &InteropRoot, verb: &str) {
    debug_assert!(
        root.view == RootView::Head,
        "smudgy: interop {verb} presented a previous-generation root id to a head read op \
         \u{2014} previousValue reads route through the previous-view ops"
    );
}

/// `get(rootId, subpath)` — synchronous snapshot at the interned root's path + `subpath`,
/// as JSON text (`null`-the-value and absent are distinguishable: absent returns no string
/// at all). Head roots overlay the caller's own unflushed writes (read-your-writes); other
/// isolates' pending writes stay invisible until their turn's flush. Head-only
/// ([`debug_assert_head_root`]): previous-view reads go through
/// [`op_smudgy_store_previous_get`], keeping this body a single store call on the hot
/// proxy path.
#[op2]
#[string]
fn op_smudgy_store_get(
    state: &mut OpState,
    root_id: u32,
    #[string] subpath: &str,
) -> Result<Option<String>, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let root = interned_root(state, root_id)?;
    debug_assert_head_root(&root, "get");
    let path = resolve_root_path(&root, subpath)?;
    let store = state.borrow::<crate::session::runtime::SharedSessionStore>();
    let isolate = current_isolate(state);
    Ok(store.borrow().get_json(&root.producer, &path, &isolate))
}

/// `previousGet(rootId, subpath)` — [`op_smudgy_store_get`] over the previous generation
/// the caller observes: the state before the newest write batch the reading isolate can
/// see, resolved seat-aware per read ([`store::SessionStore::previous_get_json`]). Absent
/// (including before the producer's first commit) returns no string at all. A separate op
/// rather than an arm in the head op so the head read body carries no previous machinery.
#[op2]
#[string]
fn op_smudgy_store_previous_get(
    state: &mut OpState,
    root_id: u32,
    #[string] subpath: &str,
) -> Result<Option<String>, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let root = interned_root(state, root_id)?;
    let path = resolve_root_path(&root, subpath)?;
    let store = state.borrow::<crate::session::runtime::SharedSessionStore>();
    let isolate = current_isolate(state);
    Ok(store
        .borrow()
        .previous_get_json(&root.producer, &path, &isolate))
}

/// `getTagged(rootId, subpath)` — the leaf-aware read
/// (`docs/interop.md` §4a): the kind at the interned root's path + `subpath`
/// under exactly [`op_smudgy_store_get`]'s visibility (read-your-writes overlay, no home
/// gate), crossing the boundary as a tagged string — first byte `o` (an object, **no
/// payload**), `a` (an array, its JSON follows), or `v` (a scalar, its JSON follows).
/// Absent crosses as no string at all, preserving absent-vs-`null` (`null` is the scalar
/// `vnull`). Objects crossing payload-free is the point: the `.value` and `.previousValue`
/// proxies walk deeper one tagged read at a time, so a leaf read costs O(answer) instead of
/// O(published tree). Head-only, like [`op_smudgy_store_get`]: previous-view roots classify
/// through [`op_smudgy_store_previous_get_tagged`].
#[op2]
#[string]
fn op_smudgy_store_get_tagged(
    state: &mut OpState,
    root_id: u32,
    #[string] subpath: &str,
) -> Result<Option<String>, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let root = interned_root(state, root_id)?;
    debug_assert_head_root(&root, "getTagged");
    let path = resolve_root_path(&root, subpath)?;
    let store = state.borrow::<crate::session::runtime::SharedSessionStore>();
    let isolate = current_isolate(state);
    Ok(store
        .borrow()
        .get_tagged(&root.producer, &path, &isolate)
        .map(tagged_wire))
}

/// `previousGetTagged(rootId, subpath)` — [`op_smudgy_store_get_tagged`]'s boundary form
/// over the previous generation the caller observes
/// ([`store::SessionStore::previous_get_tagged`]): same wire tags, same absent-vs-`null`
/// distinction, anchored at the state before the reader's newest observable write batch.
#[op2]
#[string]
fn op_smudgy_store_previous_get_tagged(
    state: &mut OpState,
    root_id: u32,
    #[string] subpath: &str,
) -> Result<Option<String>, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let root = interned_root(state, root_id)?;
    let path = resolve_root_path(&root, subpath)?;
    let store = state.borrow::<crate::session::runtime::SharedSessionStore>();
    let isolate = current_isolate(state);
    Ok(store
        .borrow()
        .previous_get_tagged(&root.producer, &path, &isolate)
        .map(tagged_wire))
}

/// Prefix a serialized payload with its one-byte kind tag (the wire form of
/// [`op_smudgy_store_get_tagged`]).
fn tag_payload(tag: char, json: &str) -> String {
    let mut wire = String::with_capacity(json.len() + 1);
    wire.push(tag);
    wire.push_str(json);
    wire
}

/// Map a classified node onto the tagged-get wire form shared by
/// [`op_smudgy_store_get_tagged`] and [`op_smudgy_store_previous_get_tagged`]: `o` (object,
/// no payload), `a<json>` (array), `v<json>` (scalar).
fn tagged_wire(tagged: store::TaggedSnapshot) -> String {
    match tagged {
        store::TaggedSnapshot::Object => "o".to_string(),
        store::TaggedSnapshot::Array(json) => tag_payload('a', &json),
        store::TaggedSnapshot::Scalar(json) => tag_payload('v', &json),
    }
}

/// `keys(rootId, subpath)` — own keys of the object at the interned root's path + `subpath`
/// as a JSON array in first-published casing and publish order, under exactly
/// [`op_smudgy_store_get`]'s visibility; no string at all when the node is absent or not an
/// object (arrays are addressed whole). The `.value` proxy's enumeration trap reads this
/// instead of materializing the subtree it is about to walk key by key. The store lends the
/// keys and the serializer writes them straight into the one crossing string — no per-key
/// clone on this hot trap path. Head-only, like [`op_smudgy_store_get`]: previous-view
/// roots enumerate through [`op_smudgy_store_previous_keys`].
#[op2]
#[string]
fn op_smudgy_store_keys(
    state: &mut OpState,
    root_id: u32,
    #[string] subpath: &str,
) -> Result<Option<String>, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let root = interned_root(state, root_id)?;
    debug_assert_head_root(&root, "keys");
    let path = resolve_root_path(&root, subpath)?;
    let store = state.borrow::<crate::session::runtime::SharedSessionStore>();
    let store = store.borrow();
    let isolate = current_isolate(state);
    Ok(store.keys(&root.producer, &path, &isolate).map(|keys| {
        serde_json::to_string(&keys).expect("a slice of borrowed strings always serializes")
    }))
}

/// `previousKeys(rootId, subpath)` — [`op_smudgy_store_keys`] over the previous generation
/// the caller observes ([`store::SessionStore::previous_keys`]): own keys of the object
/// there (first-published casing, publish order), or no string at all when the node is
/// absent or not an object.
#[op2]
#[string]
fn op_smudgy_store_previous_keys(
    state: &mut OpState,
    root_id: u32,
    #[string] subpath: &str,
) -> Result<Option<String>, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let root = interned_root(state, root_id)?;
    let path = resolve_root_path(&root, subpath)?;
    let store = state.borrow::<crate::session::runtime::SharedSessionStore>();
    let store = store.borrow();
    let isolate = current_isolate(state);
    Ok(store
        .previous_keys(&root.producer, &path, &isolate)
        .map(|keys| {
            serde_json::to_string(&keys).expect("a slice of borrowed strings always serializes")
        }))
}

/// `watch(producerSpec, path, handler, perWrite)` — change subscription on
/// `(producer, path)` in one of the two cadences (`docs/interop.md` §2).
/// Coalesced (`perWrite = false`): one delivery per flushed turn that wrote a comparable
/// path (at, above, or below the watched one), invoking `handler` with a `{ snapshot }`
/// capture holding the watched path's final state as JSON text. Per-write (`perWrite =
/// true`, the JS `onWrite` verb): a replay of the flushed journal — one delivery per
/// set-at-path in write order, value-identical writes included, with a `{ path, snapshot }`
/// capture carrying the written path (canonical spelling) and the value that write
/// published. Returns a token for [`op_smudgy_store_unwatch`]. Deliveries arrive on a later
/// pump (like events), never synchronously inside the writing turn.
#[op2(fast)]
fn op_smudgy_store_watch<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    #[string] producer: &str,
    #[string] path: &str,
    f: v8::Local<'s, v8::Function>,
    per_write: bool,
) -> Result<u32, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let producer = parse_producer(producer)?;
    let path = parse_path(path)?;
    let f = v8::Global::new(scope, f);
    let function_id = {
        let mut script_functions = state
            .borrow::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>()
            .borrow_mut();
        let id = FunctionId(script_functions.len());
        script_functions.push(f);
        id
    };
    let isolate = current_isolate(state);
    let cadence = if per_write {
        store::WatchCadence::PerWrite
    } else {
        store::WatchCadence::Coalesced
    };
    Ok(state
        .borrow::<crate::session::runtime::SharedSessionStore>()
        .borrow_mut()
        .watch(producer, path, isolate, function_id, cadence))
}

/// `bind(producerSpec, path)` — mint a widget-binding id on `(producer, path)`
/// (`docs/interop.md` §7). The id is what a `handle.bind(...)` token carries; the
/// widget build ops resolve it to a shared value cell and repaint host-side on store flushes,
/// with no JS in the update path. A read-side subscription (no home gate, like `watch`);
/// deduped per folded path, so re-running a widget build reuses the same cell.
#[op2(fast)]
fn op_smudgy_store_bind(
    state: &mut OpState,
    #[string] producer: &str,
    #[string] path: &str,
) -> Result<u32, StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let producer = parse_producer(producer)?;
    let path = parse_path(path)?;
    Ok(state
        .borrow::<crate::session::runtime::SharedSessionStore>()
        .borrow_mut()
        .bind(producer, path))
}

/// `unwatch(token)` — cancel a watch created by [`op_smudgy_store_watch`]. Scoped to the
/// registering isolate (a foreign token can't cancel it); idempotent. As with event `off`, the
/// handler's `v8::Global` stays in the append-only `script_functions` until the engine rebuilds.
#[op2(fast)]
fn op_smudgy_store_unwatch(state: &mut OpState, token: u32) -> Result<(), StoreOpError> {
    ensure(grants(state).interop_read, "interop:read")?;
    let isolate = current_isolate(state);
    state
        .borrow::<crate::session::runtime::SharedSessionStore>()
        .borrow_mut()
        .unwatch(token, &isolate);
    Ok(())
}

// ============================================================================
// Procedures (`docs/interop.md` §6): directed, fire-and-forget delivery of asks to a
// package's home instance. Receipt (registering the implementation) is an interop write
// (interop.md §3), so it passes the same home gate as `set`/`emit`; posting stamps the
// poster's origin host-side (unforgeable), rides the action queue, and shares the event
// system's depth cap. A post with no implementation on an addressable procedure is buffered
// briefly (bounded) and drained when the implementation registers — see `message_bus.rs`.
// ============================================================================

/// The canonical routing key for `(producer, procedure name)` — the folded
/// `<producer>#<name>` form, same shape as stamped event names (a separate registry keeps
/// the namespaces apart). The stamped form is a fresh temporary, so it is folded in place —
/// a borrowing fold saves nothing here.
fn canonical_procedure(producer: &store::ProducerKey, name: &str) -> String {
    let mut canonical = format!("{producer}#{name}");
    canonical.make_ascii_lowercase();
    canonical
}

/// `procedureOn(creatorId, name, impl)` — register the producer's implementation for its
/// procedure `name` (interop.md §6). Home-gated like every interop write (receipt is the
/// producer's seat, and only the home instance may hold it — the verdict cached on the
/// interned creator); a non-home registration is a no-op with the one-time teaching
/// diagnostic. A procedure has exactly one implementer: the bus REPLACES any prior
/// registration. Pending posts buffered before registration are drained FIFO and queued
/// for delivery.
#[op2(fast)]
fn op_smudgy_procedure_on<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    creator_id: u32,
    #[string] name: &str,
    f: v8::Local<'s, v8::Function>,
) -> Result<(), StoreOpError> {
    ensure(grants(state).interop_write, "interop:write")?;
    let root = interned_root(state, creator_id)?;
    if !gate_interop_write(state, &root, "procedure implement")? {
        return Ok(());
    }
    let f = v8::Global::new(scope, f);
    let function_id = {
        let mut script_functions = state
            .borrow::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>()
            .borrow_mut();
        let id = FunctionId(script_functions.len());
        script_functions.push(f);
        id
    };
    let isolate = current_isolate(state);
    let canonical = canonical_procedure(&root.producer, name);
    let drained = state
        .borrow::<crate::session::runtime::SharedMessageBus>()
        .borrow_mut()
        .subscribe(
            canonical,
            crate::session::runtime::message_bus::MessageReceiver {
                isolate: isolate.clone(),
                function_id,
            },
        );
    // Deliver the buffered posts on later pumps, in post order. They left their original
    // turns behind while buffered, so they run at the registration's own depth + 1 — still
    // inside the cycle cap if the registration itself happened deep in a handler chain.
    let depth = state.try_borrow::<EventDepth>().map_or(0, |d| d.0);
    if depth < MAX_EVENT_DEPTH {
        for post in drained {
            let matches = Arc::new(vec![
                MatchCapture {
                    name: Some(std::borrow::Cow::Borrowed("payload")),
                    value: post.payload,
                },
                MatchCapture {
                    name: Some(std::borrow::Cow::Borrowed("sender")),
                    value: post.sender,
                },
            ]);
            queue_own_action(
                state,
                RuntimeAction::CallJavascriptFunction {
                    isolate: isolate.clone(),
                    id: function_id,
                    matches,
                    depth: depth + 1,
                    is_captured: None,
                },
            );
        }
    }
    Ok(())
}

/// `procedurePost(rootId, name, argsJson)` — post a directed, fire-and-forget ask to the
/// interned root's producer's procedure `name`. The sender the implementation sees is
/// derived from the calling *isolate* host-side — `"user"` for main (user scripts, local
/// modules, and trusted packages share it, the accepted §1 residual), the package's own
/// spec for a sandbox — so a sandboxed package can never pose as another. Depth-capped like
/// `emit`. With no registered receiver, an addressable (installed-producer) message is
/// buffered briefly (D1's queue-briefly), anything else is dropped with a log. The
/// addressable check is deliberately per-call — it inspects the TARGET producer's home, not
/// the caller's cached verdict.
#[op2(fast)]
fn op_smudgy_procedure_post(
    state: &mut OpState,
    root_id: u32,
    #[string] name: &str,
    #[string] payload_json: &str,
) -> Result<(), StoreOpError> {
    ensure(grants(state).interop_write, "interop:write")?;
    let root = interned_root(state, root_id)?;
    let isolate = current_isolate(state);
    let sender = match &isolate {
        IsolateId::Main => "user".to_string(),
        IsolateId::Package { owner, name, .. } => {
            format!(
                "smudgy://{}/{}",
                owner.to_ascii_lowercase(),
                name.to_ascii_lowercase()
            )
        }
    };
    // Tier-2 catalogue sample at the post choke point; the sender is the poster, the
    // producer key is shared from the interned root, and the genuinely dynamic name is
    // folded per call.
    state
        .borrow::<crate::session::runtime::SharedCatalogue>()
        .borrow_mut()
        .sample_dynamic(
            &root.producer_spec,
            crate::session::runtime::catalogue::CatalogueKind::Procedure,
            name,
            &sender,
            payload_json,
        );
    let depth = state.try_borrow::<EventDepth>().map_or(0, |d| d.0);
    if depth >= MAX_EVENT_DEPTH {
        log::warn!(
            "smudgy: message recursion limit reached posting to {}#{name}; dropping",
            root.producer_spec
        );
        return Ok(());
    }
    let canonical = canonical_procedure(&root.producer, name);
    let receivers = state
        .borrow::<crate::session::runtime::SharedMessageBus>()
        .borrow()
        .receivers(&canonical);
    if receivers.is_empty() {
        // Queue-briefly (D1): only for a producer that can ever receive — one with a home
        // (`user` always has one). A post to an uninstalled producer can never deliver, so
        // buffering it would just hoard garbage.
        let addressable = match &root.producer {
            store::ProducerKey::User => true,
            // Platform producers never receive procedures (the host is not a procedure
            // implementer); buffering a post to one would hoard garbage.
            store::ProducerKey::Platform(_) => false,
            store::ProducerKey::Package { owner, name } => state
                .borrow::<store::HomeRegistry>()
                .borrow()
                .contains_key(&(owner.clone(), name.clone())),
        };
        if addressable {
            let dropped_oldest = state
                .borrow::<crate::session::runtime::SharedMessageBus>()
                .borrow_mut()
                .push_pending(
                    canonical,
                    crate::session::runtime::message_bus::PendingPost {
                        payload: payload_json.to_string(),
                        sender,
                    },
                );
            if dropped_oldest {
                log::warn!(
                    "smudgy: pending-message buffer for {}#{name} overflowed; dropped the oldest post",
                    root.producer_spec
                );
            }
        } else {
            log::warn!(
                "smudgy: message post to {}#{name} dropped: the producer is not installed",
                root.producer_spec
            );
        }
        return Ok(());
    }
    // One capture list shared across the whole fan-out (see `op_smudgy_emit`): the payload
    // and sender copies are made once per post, never once per receiver.
    let matches = Arc::new(vec![
        MatchCapture {
            name: Some(std::borrow::Cow::Borrowed("payload")),
            value: payload_json.to_string(),
        },
        MatchCapture {
            name: Some(std::borrow::Cow::Borrowed("sender")),
            value: sender,
        },
    ]);
    for receiver in receivers {
        queue_own_action(
            state,
            RuntimeAction::CallJavascriptFunction {
                isolate: receiver.isolate,
                id: receiver.function_id,
                matches: Arc::clone(&matches),
                depth: depth + 1,
                is_captured: None,
            },
        );
    }
    Ok(())
}

/// `interopDeclare(creatorId, kind, name)` — tier-1 runtime confirmation for the catalogue
/// (`docs/interop.md` §10): the handle constructor ran, which is also how
/// dynamically-created handles surface. Informational (no gate beyond the interned
/// attribution): presence in the catalogue grants nothing.
#[op2(fast)]
fn op_smudgy_interop_declare(
    state: &mut OpState,
    creator_id: u32,
    #[string] kind: &str,
    #[string] name: &str,
) -> Result<(), StoreOpError> {
    let root = interned_root(state, creator_id)?;
    let kind = match kind {
        "state" => crate::session::runtime::catalogue::CatalogueKind::State,
        "event" => crate::session::runtime::catalogue::CatalogueKind::Event,
        "procedure" => crate::session::runtime::catalogue::CatalogueKind::Procedure,
        other => {
            return Err(StoreOpError(format!(
                "smudgy: unknown interop handle kind {other:?}"
            )));
        }
    };
    state
        .borrow::<crate::session::runtime::SharedCatalogue>()
        .borrow_mut()
        .confirm_runtime(&root.producer_spec, kind, name);
    Ok(())
}

/// Queue an action for this session's own runtime; it executes in emission
/// order within the current expansion (depth-first).
fn queue_own_action(state: &mut OpState, action: RuntimeAction) {
    state.borrow::<ActionQueue>().borrow_mut().push_back(action);
}

/// The isolate these ops are running in (seeded into `OpState` at construction). Stamped
/// onto every automation the creation/enable ops emit so the trigger Manager keys them by
/// `(IsolateId, Origin, name)`. Take this into a local *before* calling
/// [`queue_own_action`], which needs `&mut state`.
fn current_isolate(state: &OpState) -> IsolateId {
    state.borrow::<IsolateId>().clone()
}

/// Reserve a `singleton` automation's session-wide identity (`PACKAGE-ISOLATES.md`).
/// Returns `true` when the calling op should go on to register the automation — either it
/// isn't a singleton, or it is the first copy (in any isolate, at any version) to claim
/// `(origin-sans-version, kind, name)`. Returns `false` when an earlier copy already holds the
/// identity, so the caller must NOT queue the create (first-writer-wins). Check-and-insert is
/// atomic because all ops run synchronously on the one session thread.
fn reserve_singleton(
    state: &OpState,
    singleton: bool,
    origin: &Origin,
    kind: AutomationKind,
    name: &str,
) -> bool {
    if !singleton {
        return true;
    }
    let key = SingletonKey {
        origin: origin.singleton_origin(),
        kind,
        name: Arc::from(name),
    };
    state.borrow::<SingletonRegistry>().borrow_mut().insert(key)
}

/// Route a session-targeted action: the current session's actions join the
/// in-flight spawned-action queue (preserving depth-first order); other
/// sessions receive it at the back of their main queue, like new input.
fn route_session_action(state: &mut OpState, session_id: SessionId, action: RuntimeAction) {
    if *state.borrow::<SessionId>() == session_id {
        queue_own_action(state, action);
    } else if let Some(runtime) = registry::get_runtime(session_id) {
        if runtime.tx.send(action).is_err() {
            log::warn!("Dropping action for session {session_id}: runtime has shut down");
        }
    } else {
        log::warn!("Dropping action for unknown session {session_id}");
    }
}

#[op2(fast)]
fn op_smudgy_get_current_session(state: &mut OpState) -> u32 {
    u32::from(*state.borrow::<SessionId>())
}

#[op2]
fn op_smudgy_get_sessions<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
) -> Result<v8::Local<'s, v8::Array>, NotCapable> {
    // Enumerating the user's other sessions is the `reach-others` capability.
    ensure(grants(state).reach_others, "reach-others")?;
    let session_ids = registry::get_all_session_ids();

    let sessions: Vec<v8::Local<v8::Value>> = session_ids
        .iter()
        .map(|&session_id| v8::Integer::new_from_unsigned(scope, u32::from(session_id)).into())
        .collect();

    Ok(v8::Array::new_with_elements(scope, sessions.as_slice()))
}

#[op2]
fn op_smudgy_get_session_character<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    session_id: u32,
) -> Result<v8::Local<'s, v8::Object>, NotCapable> {
    // Convert the session_id to our SessionId type
    let session_id = SessionId::from(session_id);

    // Reading the OWN session's character is the ungated baseline (read-own-context); reading
    // ANOTHER session's character is cross-session access, the `reach-others` capability — the
    // same gate cross-session send/echo use. Without it a package could read a foreign session's
    // character (name/subtext) by id even though it can't enumerate the ids.
    if session_id != *state.borrow::<SessionId>() {
        ensure(grants(state).reach_others, "reach-others")?;
    }

    // Get the runtime for this session
    let runtime = match registry::get_runtime(session_id) {
        Some(runtime) => runtime,
        None => return Ok(v8::Object::new(scope)), // Return empty object if session not found
    };

    // Create the return object
    let ret = v8::Object::new(scope);

    let name_k = v8::String::new(scope, "name").unwrap().into();
    let name_v = v8::String::new(scope, &runtime.profile_name)
        .expect("Unable to create v8 string from character name")
        .into();

    let subtext_k = v8::String::new(scope, "subtext").unwrap().into();
    let subtext_v = v8::String::new(scope, &runtime.profile_subtext)
        .expect("Unable to create v8 string from character subtext")
        .into();

    ret.create_data_property(scope, name_k, name_v);
    ret.create_data_property(scope, subtext_k, subtext_v);

    Ok(ret)
}

#[op2(fast)]
fn op_smudgy_session_echo(
    state: &mut OpState,
    session_id: u32,
    #[string] line: &str,
) -> Result<(), NotCapable> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).echo, "echo")?;
    route_session_action(
        state,
        target,
        RuntimeAction::Echo(Arc::new(line.to_string())),
    );
    Ok(())
}

// ============================================================================
// Styled text (the `style`/`link` tagged-template surface)
// ============================================================================

/// Per-isolate registry of link-callback functions: the `v8::Global` handles stay
/// HERE, inside their isolate's `OpState` (dropped with the isolate, on the session
/// thread), and echoed lines carry only a `(token, id)` address — a scrollback line
/// must never own a v8 handle, or its eventual drop on the UI thread aborts the
/// process. Ids are monotonic; a capped ring evicts the oldest, so a trigger echoing
/// callback links on every line cannot grow the registry unboundedly. A click on an
/// evicted (or reload-stale) id is a defined no-op.
#[derive(Default)]
pub struct LinkCallbacks {
    /// The id of `items[0]`; slot `i` holds id `base + i`. Ids are monotonic and
    /// `u64` — they cannot wrap within any real session, so a stale id can only
    /// miss, never alias a newer callback.
    base: u64,
    items: std::collections::VecDeque<v8::Global<v8::Function>>,
}

impl LinkCallbacks {
    /// Generous for real use; tiny next to the heap a script could grow anyway.
    const CAP: usize = 8192;

    fn insert(&mut self, function: v8::Global<v8::Function>) -> u64 {
        if self.items.len() == Self::CAP {
            self.items.pop_front();
            self.base += 1;
        }
        self.items.push_back(function);
        self.base + (self.items.len() as u64 - 1)
    }

    #[must_use]
    pub fn get(&self, id: u64) -> Option<&v8::Global<v8::Function>> {
        // An evicted id (`id < base`) wraps to a huge offset and misses.
        let offset = id.wrapping_sub(self.base);
        usize::try_from(offset)
            .ok()
            .and_then(|offset| self.items.get(offset))
    }
}

pub type SharedLinkCallbacks = Rc<RefCell<LinkCallbacks>>;

/// The wire shape of a run's link: a command to send, or an index into the
/// callbacks array travelling beside the payload.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum LinkWire {
    Send { send: String },
    Callback { cb: u32 },
}

/// This isolate's widget-routing token pre-built as a shared `Arc<str>` (the same
/// value [`WidgetIsolate`] carries as a `String`), so per-echo link contexts clone a
/// refcount instead of copying the string.
pub struct LinkIsolateToken(pub Arc<str>);

/// Everything needed to turn wire links into [`LinkAction`]s: the callback ids the
/// op registered from the payload's function array, plus the address a click routes
/// back to (this session + this isolate instantiation).
struct LinkContext {
    session: SessionId,
    isolate_token: Arc<str>,
    callback_ids: Vec<u64>,
}

/// Register the payload's callback functions (if any) into this isolate's
/// [`LinkCallbacks`] and build the [`LinkContext`] the wire conversion resolves
/// `{ cb }` links through.
fn link_context(
    scope: &mut v8::PinScope,
    state: &OpState,
    callbacks: v8::Local<v8::Array>,
) -> Result<LinkContext, StyledTextOpError> {
    let len = callbacks.length();
    let mut callback_ids = Vec::with_capacity(len as usize);
    if len > 0 {
        let registry = state.borrow::<SharedLinkCallbacks>().clone();
        let mut registry = registry.borrow_mut();
        for i in 0..len {
            let function: v8::Local<v8::Function> = callbacks
                .get_index(scope, i)
                .and_then(|value| value.try_into().ok())
                .ok_or_else(|| {
                    StyledTextOpError::Invalid("link callback must be a function".to_string())
                })?;
            callback_ids.push(registry.insert(v8::Global::new(scope, function)));
        }
    }
    Ok(LinkContext {
        session: *state.borrow::<SessionId>(),
        isolate_token: state.borrow::<LinkIsolateToken>().0.clone(),
        callback_ids,
    })
}

/// One color in the wire shape the public `Color` type serializes to: a name, an exact
/// RGB triple, or an ANSI name with an explicit brightness flag.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ColorWire {
    Name(String),
    Rgb { r: u8, g: u8, b: u8 },
    AnsiBold { color: String, bold: bool },
}

impl ColorWire {
    /// Resolve to a `Color`, or name the offending value.
    fn to_color(&self) -> Result<Color, StyledTextOpError> {
        match self {
            Self::Name(name) => color_by_name(name)
                .ok_or_else(|| StyledTextOpError::Invalid(format!("unknown color \"{name}\""))),
            Self::Rgb { r, g, b } => Ok(Color::Rgb {
                r: *r,
                g: *g,
                b: *b,
            }),
            Self::AnsiBold { color, bold } => ansi_color_by_name(color)
                .map(|color| Color::Ansi { color, bold: *bold })
                .ok_or_else(|| {
                    StyledTextOpError::Invalid(format!("unknown ANSI color \"{color}\""))
                }),
        }
    }
}

/// One run of a styled line: its text plus the colors it set. `None` means the run
/// left that channel unset, so it takes the delivery default (the echo role for
/// echoes). `link` makes the run's text clickable.
#[derive(serde::Deserialize)]
struct StyledRunWire {
    text: String,
    fg: Option<ColorWire>,
    bg: Option<ColorWire>,
    #[serde(default)]
    link: Option<LinkWire>,
}

// ---- Packed styled-echo payload -------------------------------------------------
//
// A styled echo crosses the boundary PACKED: one string carrying every text piece
// plus one `Uint32Array` record table, built by smudgy.ts `__styled_echo_packed`.
// This replaces a serde object graph whose per-run cost (a V8 property walk per
// field and an untagged color probe per channel) dominated styled-echo time.
// The two sides are a matched pair — change them together.
//
// records:
//   [0] line count L
//   [1] send-link count S
//   L × { run count R, R × 4: [text length (UTF-16 units), fg, bg, link] }
//   S × 1: send text length (UTF-16 code units)  -- at the TAIL because the
//          encoder discovers sends while walking runs, in one pass
//
// text: the S send strings in order, then every run's text in line/run order.
//
// color u32: 0 = unset (delivery default). Else the top byte is a tag:
//   1 = RGB   (low 24 bits: r<<16 | g<<8 | b)
//   2 = ANSI  (bit 3: bold; bits 0-2: black..white index)
//   3 = role  (0 default, 1 echo, 2 output, 3 warn)
//
// link u32: 0 = none. Else the top 2 bits are a tag over a 30-bit index:
//   1 = send (index into the send strings), 2 = callback (index into the
//   callbacks array travelling beside the payload).

/// Bounds-checked cursor over the packed record table.
struct PackedReader<'a> {
    records: &'a [u32],
    pos: usize,
}

impl PackedReader<'_> {
    fn next(&mut self) -> Result<u32, StyledTextOpError> {
        let value = self
            .records
            .get(self.pos)
            .copied()
            .ok_or_else(|| packed_invalid("truncated styled payload records"))?;
        self.pos += 1;
        Ok(value)
    }
}

fn packed_invalid(message: &str) -> StyledTextOpError {
    StyledTextOpError::Invalid(message.to_string())
}

/// Decode one packed color. `0` is unset (`None`); an unknown tag or stray bits
/// are a malformed payload, not a default.
fn packed_color(value: u32) -> Result<Option<Color>, StyledTextOpError> {
    if value == 0 {
        return Ok(None);
    }
    let payload = value & 0x00ff_ffff;
    match value >> 24 {
        1 => Ok(Some(Color::Rgb {
            r: ((payload >> 16) & 0xff) as u8,
            g: ((payload >> 8) & 0xff) as u8,
            b: (payload & 0xff) as u8,
        })),
        2 => {
            if payload > 0xf {
                return Err(packed_invalid("unknown ANSI color index"));
            }
            let color = match payload & 0x7 {
                0 => AnsiColor::Black,
                1 => AnsiColor::Red,
                2 => AnsiColor::Green,
                3 => AnsiColor::Yellow,
                4 => AnsiColor::Blue,
                5 => AnsiColor::Magenta,
                6 => AnsiColor::Cyan,
                _ => AnsiColor::White,
            };
            Ok(Some(Color::Ansi {
                color,
                bold: payload & 0x8 != 0,
            }))
        }
        3 => Ok(Some(match payload {
            0 => Color::DefaultForeground { bold: false },
            1 => Color::Echo,
            2 => Color::Output,
            3 => Color::Warn,
            _ => return Err(packed_invalid("unknown role color")),
        })),
        _ => Err(packed_invalid("unknown color tag")),
    }
}

/// One packed link, decoded but not yet resolved against the send/callback tables.
enum PackedLink {
    None,
    Send(usize),
    Callback(usize),
}

fn packed_link(value: u32) -> Result<PackedLink, StyledTextOpError> {
    match value >> 30 {
        0 if value == 0 => Ok(PackedLink::None),
        1 => Ok(PackedLink::Send((value & 0x3fff_ffff) as usize)),
        2 => Ok(PackedLink::Callback((value & 0x3fff_ffff) as usize)),
        _ => Err(packed_invalid("unknown link tag")),
    }
}

/// Split `rest` at exactly `units` UTF-16 code units. Rejects a piece that would
/// split a surrogate pair (the JS side counts whole code points, so that is a
/// corrupt payload) and, for run text, an embedded newline — the flattener splits
/// on `\n`, so one surviving to here is a flattener bug and should be loud. Other
/// control characters are dirty data handled by `sanitize_display_text` at build.
fn packed_take_units(
    rest: &str,
    units: u32,
    forbid_newline: bool,
) -> Result<(&str, &str), StyledTextOpError> {
    let mut remaining = units as usize;
    let mut end = 0;
    if remaining > 0 {
        for (idx, c) in rest.char_indices() {
            if forbid_newline && c == '\n' {
                return Err(packed_invalid("styled run may not contain a newline"));
            }
            let width = c.len_utf16();
            if width > remaining {
                return Err(packed_invalid("styled payload splits a surrogate pair"));
            }
            remaining -= width;
            if remaining == 0 {
                end = idx + c.len_utf8();
                break;
            }
        }
        if remaining > 0 {
            return Err(packed_invalid(
                "styled payload text shorter than its records",
            ));
        }
    }
    Ok((&rest[..end], &rest[end..]))
}

/// Validate a whole packed payload BEFORE any side effect: structure, color and
/// link tags, index bounds, text coverage, and the no-newline rule — so a rejected
/// payload cannot consume link-registry slots (evicting older live links for
/// nothing). Walks the same path as [`packed_echo_lines`] without allocating.
fn packed_validate(
    text: &str,
    records: &[u32],
    callback_count: u32,
) -> Result<(), StyledTextOpError> {
    let (line_records, send_lengths) = packed_split(records)?;
    let send_count = send_lengths.len();
    let mut rest = text;
    for units in send_lengths {
        rest = packed_take_units(rest, *units, false)?.1;
    }
    let mut reader = PackedReader {
        records: line_records,
        pos: 2,
    };
    let line_count = line_records[0];
    for _ in 0..line_count {
        let run_count = reader.next()?;
        for _ in 0..run_count {
            let units = reader.next()?;
            packed_color(reader.next()?)?;
            packed_color(reader.next()?)?;
            match packed_link(reader.next()?)? {
                PackedLink::Send(index) if index >= send_count => {
                    return Err(packed_invalid("link send index out of range"));
                }
                PackedLink::Callback(index) if index >= callback_count as usize => {
                    return Err(packed_invalid("link callback index out of range"));
                }
                _ => {}
            }
            rest = packed_take_units(rest, units, true)?.1;
        }
    }
    if reader.pos != line_records.len() {
        return Err(packed_invalid("styled payload has trailing records"));
    }
    if !rest.is_empty() {
        return Err(packed_invalid("styled payload has trailing text"));
    }
    Ok(())
}

/// Split the record table into its line region and the tail of send lengths,
/// bounds-checking the header.
fn packed_split(records: &[u32]) -> Result<(&[u32], &[u32]), StyledTextOpError> {
    if records.len() < 2 {
        return Err(packed_invalid("truncated styled payload records"));
    }
    let send_count = records[1] as usize;
    let split = records
        .len()
        .checked_sub(send_count)
        .filter(|split| *split >= 2)
        .ok_or_else(|| packed_invalid("truncated styled payload records"))?;
    Ok(records.split_at(split))
}

/// Build the payload's `StyledLine`s in one pass: run text is sliced straight out
/// of the crossing string (borrowed unless `sanitize_display_text` had to strip
/// something), unset colors resolve to the delivery defaults, and links resolve
/// through the send table / [`LinkContext`]. Spans tile by construction
/// (`StyledLine::from_styled_runs`). Call [`packed_validate`] first; this re-walks
/// the same structure and only reports errors that pass missed nothing of.
fn packed_echo_lines(
    text: &str,
    records: &[u32],
    links: &LinkContext,
    default_style: Style,
) -> Result<Vec<Arc<StyledLine>>, StyledTextOpError> {
    let (line_records, send_lengths) = packed_split(records)?;
    let mut rest = text;
    let mut sends: Vec<Arc<str>> = Vec::with_capacity(send_lengths.len());
    for units in send_lengths {
        let (piece, tail) = packed_take_units(rest, *units, false)?;
        sends.push(Arc::from(piece));
        rest = tail;
    }
    let mut reader = PackedReader {
        records: line_records,
        pos: 2,
    };
    let line_count = line_records[0] as usize;
    let mut lines = Vec::with_capacity(line_count);
    for _ in 0..line_count {
        let run_count = reader.next()? as usize;
        let mut runs: Vec<(std::borrow::Cow<str>, Style, Option<LinkAction>)> =
            Vec::with_capacity(run_count);
        for _ in 0..run_count {
            let units = reader.next()?;
            let style = Style {
                fg: packed_color(reader.next()?)?.unwrap_or(default_style.fg),
                bg: packed_color(reader.next()?)?.map_or(default_style.bg, normalize_bg),
            };
            let link = match packed_link(reader.next()?)? {
                PackedLink::None => None,
                PackedLink::Send(index) => {
                    Some(LinkAction::Send(sends.get(index).cloned().ok_or_else(
                        || packed_invalid("link send index out of range"),
                    )?))
                }
                PackedLink::Callback(index) => {
                    let id = links
                        .callback_ids
                        .get(index)
                        .copied()
                        .ok_or_else(|| packed_invalid("link callback index out of range"))?;
                    Some(LinkAction::Callback {
                        session: links.session,
                        isolate_token: links.isolate_token.clone(),
                        id,
                    })
                }
            };
            let (piece, tail) = packed_take_units(rest, units, true)?;
            rest = tail;
            runs.push((sanitize_display_text(piece), style, link));
        }
        let refs: Vec<(&str, Style, Option<LinkAction>)> = runs
            .iter()
            .map(|(text, style, link)| (text.as_ref(), *style, link.clone()))
            .collect();
        lines.push(Arc::new(StyledLine::from_styled_runs(&refs, default_style)));
    }
    Ok(lines)
}

/// The error a styled-text op throws: a capability denial, or a malformed payload (an
/// unknown color name, or a run smuggling a newline past the flattener's split).
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
pub enum StyledTextOpError {
    #[class(inherit)]
    #[error(transparent)]
    NotCapable(#[from] NotCapable),
    #[class(inherit)]
    #[error(transparent)]
    Pane(#[from] PaneOpError),
    #[class(generic)]
    #[error("smudgy: {0}")]
    Invalid(String),
    #[class(inherit)]
    #[error(transparent)]
    NotCurrent(#[from] LineNotCurrent),
}

/// The current-line staleness scope, one cell shared engine-wide (every
/// isolate's `OpState` plus the engine's user-JS entry points). `current` is
/// the generation stamped on the line in flight (`ScriptEngine::set_current_line`
/// bumps it as dispatch installs each line); `armed` is the generation the
/// running synchronous user-JS entry captured on its way in — 0 outside any
/// entry, and 0 for entries made while no line was in flight. The ambient
/// `line` mutators require `armed == current` (see [`ensure_current_line`]):
/// an async continuation that outlives its line can only resume between
/// entries (deno runs microtasks in the event-loop pump, never inside a
/// synchronous call), where `armed` is 0, so it throws instead of editing
/// whatever line is current by then. The submission ops solve the same
/// staleness for `sys:input` with a script-side capture ([`with_submission`]);
/// `line` is armed host-side because triggers and aliases enter user JS with
/// no script-side wrapper to capture in.
#[derive(Debug, Clone, Copy, Default)]
pub struct LineScope {
    pub current: u64,
    pub armed: u64,
}

/// The shared cell form of [`LineScope`].
pub type LineScopeCell = Rc<Cell<LineScope>>;

/// The staleness refusal for the ambient `line`'s mutators: the write arrived
/// outside the window in which its line was in flight — no line at all (a
/// hotkey, a timer, a module top level), or an async continuation that
/// outlived its line. Reads stay graceful (`""`/`undefined`); writes are
/// refused loudly because a leaked write would land on whatever line the
/// per-line routing/transform cells are consumed for NEXT.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
#[class(generic)]
#[error("smudgy: the current line is only mutable inside a trigger or sys:receive handler for it")]
pub struct LineNotCurrent;

/// The current-line staleness gate shared by the ambient `line` mutators
/// (gag/redirect/copy and the current-line transforms): the armed entry scope
/// must name the line in flight. See [`LineScope`].
fn ensure_current_line(state: &OpState) -> Result<(), LineNotCurrent> {
    let scope = state.borrow::<LineScopeCell>().get();
    if scope.armed != 0 && scope.armed == scope.current {
        Ok(())
    } else {
        Err(LineNotCurrent)
    }
}

/// Errors a current-line transform/routing op can throw: the capability
/// denial or the staleness refusal.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
pub enum LineCallError {
    #[class(inherit)]
    #[error(transparent)]
    NotCapable(#[from] NotCapable),
    #[class(inherit)]
    #[error(transparent)]
    NotCurrent(#[from] LineNotCurrent),
}

/// Resolve a splice run's wire link to its [`LinkAction`]. (The echo path resolves
/// links from the packed table in [`packed_echo_lines`].)
fn wire_link_to_action(
    link: &Option<LinkWire>,
    links: &LinkContext,
) -> Result<Option<LinkAction>, StyledTextOpError> {
    match link {
        None => Ok(None),
        Some(LinkWire::Send { send }) => Ok(Some(LinkAction::Send(Arc::from(send.as_str())))),
        Some(LinkWire::Callback { cb }) => {
            let id = links
                .callback_ids
                .get(*cb as usize)
                .copied()
                .ok_or_else(|| {
                    StyledTextOpError::Invalid("link callback index out of range".to_string())
                })?;
            Ok(Some(LinkAction::Callback {
                session: links.session,
                isolate_token: links.isolate_token.clone(),
                id,
            }))
        }
    }
}

impl StyledRunWire {
    /// Validate a run before ANY side effect — the ops call this over the whole
    /// payload before registering its callbacks, so a rejected payload cannot
    /// consume link-registry slots (evicting older live links for nothing).
    fn validate(&self, callback_count: u32) -> Result<(), StyledTextOpError> {
        if self.text.contains('\n') {
            return Err(StyledTextOpError::Invalid(
                "styled run may not contain a newline".to_string(),
            ));
        }
        if let Some(fg) = &self.fg {
            fg.to_color()?;
        }
        if let Some(bg) = &self.bg {
            bg.to_color()?;
        }
        if let Some(LinkWire::Callback { cb }) = &self.link
            && *cb >= callback_count
        {
            return Err(StyledTextOpError::Invalid(
                "link callback index out of range".to_string(),
            ));
        }
        Ok(())
    }

    /// Convert to the owned run a [`LineOperation::Splice`] carries: colors are
    /// resolved here but left `None` when unset — the splice point's style is only
    /// knowable when the operation applies to its line. Control characters are
    /// stripped with the same rule the echo conversion uses.
    fn into_splice_run(self, links: &LinkContext) -> Result<SpliceRun, StyledTextOpError> {
        Ok(SpliceRun {
            fg: self.fg.as_ref().map(ColorWire::to_color).transpose()?,
            bg: self
                .bg
                .as_ref()
                .map(|bg| bg.to_color().map(normalize_bg))
                .transpose()?,
            link: wire_link_to_action(&self.link, links)?,
            text: match sanitize_display_text(&self.text) {
                std::borrow::Cow::Borrowed(_) => self.text,
                std::borrow::Cow::Owned(cleaned) => cleaned,
            },
        })
    }
}

/// The delivery defaults an echoed line's unset colors resolve to.
const ECHO_DEFAULT_STYLE: Style = Style {
    fg: Color::Echo,
    bg: Color::DefaultBackground,
};

/// Convert and register the splice payload the two splice ops share: the runs of ONE
/// line (validated BEFORE the callbacks register, so a rejected payload consumes no
/// registry slots; unset colors stay unset for splice-point inheritance).
fn splice_runs(
    scope: &mut v8::PinScope,
    state: &OpState,
    runs: Vec<StyledRunWire>,
    callbacks: v8::Local<v8::Array>,
) -> Result<Arc<Vec<SpliceRun>>, StyledTextOpError> {
    runs.iter()
        .try_for_each(|run| run.validate(callbacks.length()))?;
    let links = link_context(scope, state, callbacks)?;
    Ok(Arc::new(
        runs.into_iter()
            .map(|run| run.into_splice_run(&links))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

/// Splice styled (possibly linked) runs into the CURRENT line — the write path for
/// `line.insert`/`replaceAt`/`replace` given styled text; the styled sibling of
/// [`op_smudgy_insert`], behind the same `change-display` gate.
#[op2]
fn op_smudgy_splice(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    #[serde] runs: Vec<StyledRunWire>,
    begin: u32,
    end: u32,
    callbacks: v8::Local<v8::Array>,
) -> Result<(), StyledTextOpError> {
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    let runs = splice_runs(scope, state, runs, callbacks)?;
    let pending_ops = state.borrow::<Rc<RefCell<Vec<LineOperation>>>>();
    pending_ops.borrow_mut().push(LineOperation::Splice {
        runs,
        begin: begin as usize,
        end: end as usize,
    });
    Ok(())
}

/// [`op_smudgy_splice`] for an already-emitted buffer line — the styled sibling of
/// [`op_smudgy_line_insert`].
#[op2]
fn op_smudgy_line_splice(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    line_number: u32,
    #[serde] runs: Vec<StyledRunWire>,
    begin: u32,
    end: u32,
    callbacks: v8::Local<v8::Array>,
) -> Result<(), StyledTextOpError> {
    ensure(grants(state).change_display, "change-display")?;
    let runs = splice_runs(scope, state, runs, callbacks)?;
    queue_own_action(
        state,
        RuntimeAction::PerformLineOperation {
            line_number: line_number as usize,
            operation: LineOperation::Splice {
                runs,
                begin: begin as usize,
                end: end as usize,
            },
        },
    );
    Ok(())
}

/// The styled sibling of [`op_smudgy_session_echo`]: same `echo` gate and routing, but
/// the payload carries pre-flattened styled runs per line, PACKED (one text string +
/// one u32 record table — see the packed-payload layout above `PackedReader`). Spans
/// are built and validated here at the boundary; dispatch just appends. `callbacks`
/// carries the payload's link-callback functions (indexed by the records' callback
/// links) — they are registered into THIS isolate's registry, never shipped with the
/// line.
#[op2(fast)]
fn op_smudgy_session_echo_styled(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    session_id: u32,
    #[string] text: &str,
    #[buffer] records: &[u32],
    callbacks: v8::Local<v8::Array>,
) -> Result<(), StyledTextOpError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).echo, "echo")?;
    packed_validate(text, records, callbacks.length())?;
    let links = link_context(scope, state, callbacks)?;
    let lines = packed_echo_lines(text, records, &links, ECHO_DEFAULT_STYLE)?;
    route_session_action(state, target, RuntimeAction::EchoStyled(lines));
    Ok(())
}

#[op2(fast)]
fn op_smudgy_session_send(
    state: &mut OpState,
    session_id: u32,
    #[string] line: &str,
) -> Result<(), NotCapable> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).send, "send")?;
    route_session_action(
        state,
        target,
        RuntimeAction::Send(Arc::new(line.to_string())),
    );
    Ok(())
}

#[op2(fast)]
fn op_smudgy_session_send_raw(
    state: &mut OpState,
    session_id: u32,
    #[string] line: &str,
) -> Result<(), NotCapable> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).send_direct, "send-direct")?;
    route_session_action(
        state,
        target,
        RuntimeAction::SendRaw(Arc::new(line.to_string())),
    );
    Ok(())
}

/// Reload a session's scripting engine. `reload` is a payload-free routable command:
/// own-session reload rebuilds this thread's engine;
/// a foreign reload routes `RuntimeAction::Reload` to the target's back-of-queue (best-effort,
/// like fresh input). There is no dedicated `reload` capability — reloading the OWN session is the
/// ungated baseline, so `primary` is `true` here and only the cross-session `reach_others` gate in
/// [`ensure_session_target`] applies when `target != own`.
#[op2(fast)]
fn op_smudgy_session_reload(state: &mut OpState, session_id: u32) -> Result<(), NotCapable> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, true, "reload")?;
    route_session_action(state, target, RuntimeAction::Reload);
    Ok(())
}

/// Helper function to convert a V8 array to Vec<String>
fn v8_array_to_vec_str(scope: &mut v8::PinScope, arr: &v8::Array) -> Result<Vec<String>, AnyError> {
    (0..arr.length())
        .map(|i| {
            arr.get_index(scope, i).map_or_else(
                || bail!("Unable to get element {} from array", i),
                |val| Ok(val.to_rust_string_lossy(scope)),
            )
        })
        .collect()
}

/// Convert a `0`-means-unbounded self-limit count (the wire convention: the JS create
/// functions pass `0` for an absent `fireLimit`/`lineLimit`, since `op2(fast)` can't take an
/// `Option<u32>`) into the `Option<u32>` the runtime carries.
fn self_limit(count: u32) -> Option<u32> {
    (count != 0).then_some(count)
}

/// Normalizes a function's `toString()` (passed in good faith from JS-land) into the optional
/// display-only body source carried on the create actions: an empty string ⇒ `None`.
fn script_source_arc(source: String) -> Option<std::sync::Arc<str>> {
    (!source.is_empty()).then(|| std::sync::Arc::from(source))
}

/// Errors an automation op can throw: the capability denial, or an interop identity lookup
/// failure (an unknown/forged creator id — the host-minted ids the facade carries are
/// always valid, and a malformed creator descriptor already failed loudly at
/// [`op_smudgy_interop_resolve_creator`]).
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
pub enum AutomationOpError {
    #[class(inherit)]
    #[error(transparent)]
    NotCapable(#[from] NotCapable),
    #[class(inherit)]
    #[error(transparent)]
    Identity(#[from] StoreOpError),
}

/// Returns whether the alias was created (`true`) or skipped because an equal singleton
/// identity was already reserved (`false`); a non-singleton create always returns `true`.
#[allow(clippy::too_many_arguments)]
#[op2(fast)]
fn op_smudgy_create_simple_alias(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
    patterns: &v8::Array,
    #[string] script: String,
    singleton: bool,
    priority: i32,
    fallthrough: bool,
    fire_limit: u32,
) -> Result<bool, AutomationOpError> {
    ensure(grants(state).create_aliases, "aliases")?;
    // Convert patterns array to Vec<String>
    let patterns_vec = match v8_array_to_vec_str(scope, patterns) {
        Ok(patterns) => patterns,
        Err(_) => return Ok(false), // Silently fail on error
    };

    let origin = creator_origin(state, creator_id)?;
    // First-writer-wins: a singleton whose identity is already reserved no-ops here.
    if !reserve_singleton(state, singleton, &origin, AutomationKind::Alias, &name) {
        return Ok(false);
    }

    // Create AliasDefinition
    let alias_def = AliasDefinition {
        pattern: patterns_vec.into_iter().next().unwrap_or_default(),
        script: Some(script),
        package: None,
        enabled: true,
        priority,
        fallthrough,
        language: ScriptLang::Plaintext,
    };

    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::AddAlias {
            isolate,
            origin,
            name: Arc::new(name),
            alias: alias_def,
            fire_limit: self_limit(fire_limit),
        },
    );
    Ok(true)
}

/// Returns whether the trigger was created (`true`) or skipped because an equal singleton
/// identity was already reserved (`false`); a non-singleton create always returns `true`.
#[allow(clippy::too_many_arguments)]
#[op2(fast)]
fn op_smudgy_create_simple_trigger(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
    patterns: &v8::Array,
    raw_patterns: &v8::Array,
    anti_patterns: &v8::Array,
    #[string] script: String,
    prompt: bool,
    enabled: bool,
    singleton: bool,
    priority: i32,
    fallthrough: bool,
    fire_limit: u32,
    line_limit: u32,
) -> Result<bool, AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    // Convert all pattern arrays to Vec<String>
    let patterns_vec = match v8_array_to_vec_str(scope, patterns) {
        Ok(patterns) => {
            if patterns.is_empty() {
                None
            } else {
                Some(patterns)
            }
        }
        Err(_) => return Ok(false), // Silently fail on error
    };

    let raw_patterns_vec = match v8_array_to_vec_str(scope, raw_patterns) {
        Ok(patterns) => {
            if patterns.is_empty() {
                None
            } else {
                Some(patterns)
            }
        }
        Err(_) => return Ok(false), // Silently fail on error
    };

    let anti_patterns_vec = match v8_array_to_vec_str(scope, anti_patterns) {
        Ok(patterns) => {
            if patterns.is_empty() {
                None
            } else {
                Some(patterns)
            }
        }
        Err(_) => return Ok(false), // Silently fail on error
    };

    let origin = creator_origin(state, creator_id)?;
    // First-writer-wins: a singleton whose identity is already reserved no-ops here.
    if !reserve_singleton(state, singleton, &origin, AutomationKind::Trigger, &name) {
        return Ok(false);
    }

    // Create TriggerDefinition
    let trigger_def = TriggerDefinition {
        patterns: patterns_vec,
        raw_patterns: raw_patterns_vec,
        anti_patterns: anti_patterns_vec,
        script: Some(script),
        package: None,
        language: ScriptLang::Plaintext,
        enabled,
        prompt,
        priority,
        fallthrough,
    };

    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::AddTrigger {
            isolate,
            origin,
            name: Arc::new(name),
            trigger: trigger_def,
            fire_limit: self_limit(fire_limit),
            line_limit: self_limit(line_limit),
        },
    );
    Ok(true)
}

/// Returns whether the trigger was created (`true`) or skipped because an equal singleton
/// identity was already reserved (`false`); a non-singleton create always returns `true`.
#[allow(clippy::too_many_arguments)]
#[op2(fast)]
fn op_smudgy_create_javascript_function_trigger<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
    patterns: &v8::Array,
    raw_patterns: &v8::Array,
    anti_patterns: &v8::Array,
    f: v8::Local<'s, v8::Function>,
    prompt: bool,
    enabled: bool,
    singleton: bool,
    priority: i32,
    fallthrough: bool,
    fire_limit: u32,
    line_limit: u32,
    #[string] script_source: String,
) -> Result<bool, AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    // Convert all pattern arrays to Vec<String>
    let patterns_vec = match v8_array_to_vec_str(scope, patterns) {
        Ok(patterns) => patterns,
        Err(_) => return Ok(false), // Silently fail on error
    };
    let raw_patterns_vec = match v8_array_to_vec_str(scope, raw_patterns) {
        Ok(patterns) => patterns,
        Err(_) => return Ok(false), // Silently fail on error
    };
    let anti_patterns_vec = match v8_array_to_vec_str(scope, anti_patterns) {
        Ok(patterns) => patterns,
        Err(_) => return Ok(false), // Silently fail on error
    };

    let origin = creator_origin(state, creator_id)?;
    // First-writer-wins: a singleton whose identity is already reserved no-ops here — and we
    // skip registering the function so it doesn't leak into this isolate's registry.
    if !reserve_singleton(state, singleton, &origin, AutomationKind::Trigger, &name) {
        return Ok(false);
    }

    let f = v8::Global::new(scope, f);
    // Store the function and get the function_id
    let function_id = {
        let mut script_functions = state
            .borrow::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>()
            .borrow_mut();
        let function_id = FunctionId(script_functions.len());
        script_functions.push(f);
        function_id
    };

    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::AddJavascriptFunctionTrigger {
            isolate,
            origin,
            name: Arc::new(name),
            patterns: Arc::new(patterns_vec),
            raw_patterns: Arc::new(raw_patterns_vec),
            anti_patterns: Arc::new(anti_patterns_vec),
            function_id,
            prompt,
            enabled,
            priority,
            fallthrough,
            fire_limit: self_limit(fire_limit),
            line_limit: self_limit(line_limit),
            script_source: script_source_arc(script_source),
        },
    );
    Ok(true)
}

/// Returns whether the alias was created (`true`) or skipped because an equal singleton
/// identity was already reserved (`false`); a non-singleton create always returns `true`.
#[allow(clippy::inline_always, clippy::too_many_arguments)]
#[op2(fast)]
fn op_smudgy_create_javascript_function_alias<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
    patterns: &v8::Array,
    f: v8::Local<'s, v8::Function>,
    singleton: bool,
    priority: i32,
    fallthrough: bool,
    fire_limit: u32,
    #[string] script_source: String,
) -> Result<bool, AutomationOpError> {
    ensure(grants(state).create_aliases, "aliases")?;
    // Convert patterns array to Vec<String>
    let patterns_vec = match v8_array_to_vec_str(scope, patterns) {
        Ok(patterns) => patterns,
        Err(_) => return Ok(false), // Silently fail on error
    };

    let origin = creator_origin(state, creator_id)?;
    // First-writer-wins: a singleton whose identity is already reserved no-ops here — and we
    // skip registering the function so it doesn't leak into this isolate's registry.
    if !reserve_singleton(state, singleton, &origin, AutomationKind::Alias, &name) {
        return Ok(false);
    }

    let f = v8::Global::new(scope, f);
    // Store the function and get the function_id
    let function_id = {
        let mut script_functions = state
            .borrow::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>()
            .borrow_mut();
        let function_id = FunctionId(script_functions.len());
        script_functions.push(f);
        function_id
    };

    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::AddJavascriptFunctionAlias {
            isolate,
            origin,
            name: Arc::new(name),
            patterns: Arc::new(patterns_vec),
            function_id,
            priority,
            fallthrough,
            fire_limit: self_limit(fire_limit),
            script_source: script_source_arc(script_source),
        },
    );
    Ok(true)
}

#[allow(clippy::inline_always)]
#[op2(fast)]
fn op_smudgy_set_alias_enabled(
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
    enabled: bool,
) -> Result<(), AutomationOpError> {
    // Toggling an alias rides the create-aliases capability. Own-origin-scoped: the action is
    // keyed by `(current_isolate, origin, name)`, and `origin` is this package's bound creator (the
    // `smudgy:core` module bakes it in per importer), so it can only target THIS isolate's own
    // automations — never the user's (main isolate) or another package's (a different isolate).
    ensure(grants(state).create_aliases, "aliases")?;
    let origin = creator_origin(state, creator_id)?;
    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::EnableAlias(isolate, origin, Arc::new(name), enabled),
    );
    Ok(())
}

#[allow(clippy::inline_always)]
#[op2(fast)]
fn op_smudgy_set_trigger_enabled(
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
    enabled: bool,
) -> Result<(), AutomationOpError> {
    // Own-origin-scoped like `set_alias_enabled` above; gated on create-triggers.
    ensure(grants(state).create_triggers, "triggers")?;
    let origin = creator_origin(state, creator_id)?;
    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::EnableTrigger(isolate, origin, Arc::new(name), enabled),
    );
    Ok(())
}

/// Remove an alias by name (`delete()`). Origin-scoped exactly like the enable op: keyed by
/// `(current_isolate, this package's bound creator origin, name)`, so a package can only remove
/// its OWN aliases. Gated on `create-aliases` (the same capability that created it). A `delete()`
/// of an unknown name is a no-op in the `Manager`.
#[allow(clippy::inline_always)]
#[op2(fast)]
fn op_smudgy_remove_alias(
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
) -> Result<(), AutomationOpError> {
    ensure(grants(state).create_aliases, "aliases")?;
    let origin = creator_origin(state, creator_id)?;
    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::RemoveAlias(isolate, origin, Arc::new(name)),
    );
    Ok(())
}

/// Remove a trigger by name (`delete()`). Origin-scoped like `remove_alias`; gated on
/// `create-triggers`.
#[allow(clippy::inline_always)]
#[op2(fast)]
fn op_smudgy_remove_trigger(
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
) -> Result<(), AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    let origin = creator_origin(state, creator_id)?;
    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::RemoveTrigger(isolate, origin, Arc::new(name)),
    );
    Ok(())
}

/// Create (or upsert) a script hotkey (`createHotkey`). The `handler` is a JS function
/// registered in THIS isolate's `script_functions`; the resulting `AddHotkey` carries its
/// `function_id` so the hotkey fires the function on keypress (via `CallJavascriptFunction`),
/// stamped with the caller's `(isolate, origin)` so `delete()` and re-create are origin-scoped
/// (current-session-only — keyed by the source isolate's `FunctionId`). `key` +
/// `modifiers` mirror [`HotkeyDefinition`] (the UI maps them to `iced` keys). Gated on
/// `create-triggers` (a hotkey runs author script on a keypress, the same trust the trigger/alias
/// create gate covers — there is no separate hotkey grant).
#[op2]
fn op_smudgy_create_hotkey<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
    #[string] key: String,
    #[serde] modifiers: Vec<String>,
    f: v8::Local<'s, v8::Function>,
) -> Result<(), AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    let origin = creator_origin(state, creator_id)?;

    let f = v8::Global::new(scope, f);
    let function_id = {
        let mut script_functions = state
            .borrow::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>()
            .borrow_mut();
        let function_id = FunctionId(script_functions.len());
        script_functions.push(f);
        function_id
    };

    let hotkey = HotkeyDefinition {
        key,
        modifiers,
        // The body lives as a registered function handle (`function_id`), not inline script.
        script: None,
        package: None,
        language: ScriptLang::JS,
        enabled: true,
    };

    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::AddHotkey {
            isolate,
            origin,
            name: Arc::new(name),
            hotkey,
            function_id: Some(function_id),
        },
    );
    Ok(())
}

/// Remove a hotkey by name (`delete()`). Origin-scoped like `remove_trigger`: keyed by
/// `(current_isolate, creator-origin, name)`, so a package can only remove its own hotkeys.
/// Gated on `create-triggers`. Removing an unknown name is a no-op in dispatch.
#[allow(clippy::inline_always)]
#[op2(fast)]
fn op_smudgy_remove_hotkey(
    state: &mut OpState,
    creator_id: u32,
    #[string] name: String,
) -> Result<(), AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    let origin = creator_origin(state, creator_id)?;
    let isolate = current_isolate(state);
    queue_own_action(
        state,
        RuntimeAction::RemoveHotkey(isolate, origin, Arc::new(name)),
    );
    Ok(())
}

/// One introspected automation, returned by the `get` ops. `name`/`pattern`/
/// `enabled` mirror what the JS handle exposes.
#[derive(serde::Serialize)]
struct AutomationView {
    name: String,
    pattern: String,
    enabled: bool,
    priority: i32,
    fallthrough: bool,
}

/// Read one alias from the introspection mirror, scoped to the caller's own `(isolate, origin)`
/// namespace (a package never sees another origin's automations). `None` when absent.
/// Gated on `create-aliases`: introspecting aliases rides the same capability that creates them.
#[op2]
#[serde]
fn op_smudgy_get_alias(
    state: &OpState,
    creator_id: u32,
    #[string] name: &str,
) -> Result<Option<AutomationView>, AutomationOpError> {
    ensure(grants(state).create_aliases, "aliases")?;
    Ok(lookup_automation(
        state,
        AutomationKind::Alias,
        creator_id,
        name,
    )?)
}

/// Trigger counterpart of [`op_smudgy_get_alias`]; gated on `create-triggers`.
#[op2]
#[serde]
fn op_smudgy_get_trigger(
    state: &OpState,
    creator_id: u32,
    #[string] name: &str,
) -> Result<Option<AutomationView>, AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    Ok(lookup_automation(
        state,
        AutomationKind::Trigger,
        creator_id,
        name,
    )?)
}

/// List the caller's own alias names (`triggers.list()`/`aliases.list()`). Origin-scoped.
#[op2]
#[serde]
fn op_smudgy_list_aliases(
    state: &OpState,
    creator_id: u32,
) -> Result<Vec<String>, AutomationOpError> {
    ensure(grants(state).create_aliases, "aliases")?;
    Ok(list_automations(state, AutomationKind::Alias, creator_id)?)
}

/// List the caller's own trigger names; gated on `create-triggers`.
#[op2]
#[serde]
fn op_smudgy_list_triggers(
    state: &OpState,
    creator_id: u32,
) -> Result<Vec<String>, AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    Ok(list_automations(
        state,
        AutomationKind::Trigger,
        creator_id,
    )?)
}

/// Whether the caller owns an alias by this name (`aliases.exists(name)`). Origin-scoped.
#[op2(fast)]
fn op_smudgy_alias_exists(
    state: &OpState,
    creator_id: u32,
    #[string] name: &str,
) -> Result<bool, AutomationOpError> {
    ensure(grants(state).create_aliases, "aliases")?;
    Ok(lookup_automation(state, AutomationKind::Alias, creator_id, name)?.is_some())
}

/// Whether the caller owns a trigger by this name; gated on `create-triggers`.
#[op2(fast)]
fn op_smudgy_trigger_exists(
    state: &OpState,
    creator_id: u32,
    #[string] name: &str,
) -> Result<bool, AutomationOpError> {
    ensure(grants(state).create_triggers, "triggers")?;
    Ok(lookup_automation(state, AutomationKind::Trigger, creator_id, name)?.is_some())
}

/// Look one automation up in the shared introspection mirror, scoped to the caller's
/// `(current_isolate, interned creator-origin)` key. Returns `None` when absent.
fn lookup_automation(
    state: &OpState,
    kind: AutomationKind,
    creator_id: u32,
    name: &str,
) -> Result<Option<AutomationView>, StoreOpError> {
    let key = (current_isolate(state), creator_origin(state, creator_id)?);
    let registry = state.borrow::<SharedAutomationRegistry>().borrow();
    // Only aliases/triggers are mirrored here; hotkeys are introspected JS-side, so this
    // helper is never called with `Hotkey`.
    let map = match kind {
        AutomationKind::Alias => &registry.aliases,
        AutomationKind::Trigger => &registry.triggers,
        AutomationKind::Hotkey => return Ok(None),
    };
    Ok(map
        .get(&key)
        .and_then(|namespace| namespace.get(name))
        .map(|entry| AutomationView {
            name: name.to_string(),
            pattern: entry.pattern.clone(),
            enabled: entry.enabled,
            priority: entry.priority,
            fallthrough: entry.fallthrough,
        }))
}

/// List the caller's own automation names for `kind`, scoped to its `(isolate, origin)` key.
fn list_automations(
    state: &OpState,
    kind: AutomationKind,
    creator_id: u32,
) -> Result<Vec<String>, StoreOpError> {
    let key = (current_isolate(state), creator_origin(state, creator_id)?);
    let registry = state.borrow::<SharedAutomationRegistry>().borrow();
    let map = match kind {
        AutomationKind::Alias => &registry.aliases,
        AutomationKind::Trigger => &registry.triggers,
        AutomationKind::Hotkey => return Ok(Vec::new()),
    };
    Ok(map
        .get(&key)
        .map(|namespace| namespace.keys().cloned().collect())
        .unwrap_or_default())
}

/// Map one of the 8 base ANSI color names to its `AnsiColor` (brightness is carried
/// separately as `bold`).
fn ansi_color_by_name(name: &str) -> Option<AnsiColor> {
    Some(match name {
        "black" => AnsiColor::Black,
        "red" => AnsiColor::Red,
        "green" => AnsiColor::Green,
        "yellow" => AnsiColor::Yellow,
        "blue" => AnsiColor::Blue,
        "magenta" => AnsiColor::Magenta,
        "cyan" => AnsiColor::Cyan,
        "white" => AnsiColor::White,
        _ => return None,
    })
}

/// Map a color NAME the write color APIs accept — an ANSI name (meaning the bright
/// variant) or a theme role (`default`/`echo`/`output`/`warn`) — to its `Color`.
fn color_by_name(name: &str) -> Option<Color> {
    if let Some(color) = ansi_color_by_name(name) {
        return Some(Color::Ansi { color, bold: true });
    }
    Some(match name {
        "default" => Color::DefaultForeground { bold: false },
        "echo" => Color::Echo,
        "output" => Color::Output,
        "warn" => Color::Warn,
        _ => return None,
    })
}

/// `"default"` means the theme default of the CHANNEL it is used on: name resolution
/// yields the foreground variant, so a background position flips it here — otherwise
/// `bg: "default"` would paint the theme's foreground color as a solid background.
fn normalize_bg(color: Color) -> Color {
    if matches!(color, Color::DefaultForeground { .. }) {
        Color::DefaultBackground
    } else {
        color
    }
}

/// Helper function to parse a color from a JavaScript value
fn parse_color_from_js(
    scope: &mut v8::PinScope,
    color_val: v8::Local<v8::Value>,
) -> Result<Color, AnyError> {
    if color_val.is_string() {
        let color_str = color_val.to_rust_string_lossy(scope);
        match color_by_name(&color_str) {
            Some(color) => Ok(color),
            None => bail!("Unknown color: {}", color_str),
        }
    } else if color_val.is_object() {
        let obj = color_val.to_object(scope).unwrap();

        // Check if it's an RGB color
        let r_key = v8::String::new(scope, "r").unwrap().into();
        let g_key = v8::String::new(scope, "g").unwrap().into();
        let b_key = v8::String::new(scope, "b").unwrap().into();

        if let (Some(r_val), Some(g_val), Some(b_val)) = (
            obj.get(scope, r_key),
            obj.get(scope, g_key),
            obj.get(scope, b_key),
        ) && r_val.is_number()
            && g_val.is_number()
            && b_val.is_number()
        {
            let r = r_val.number_value(scope).unwrap_or(0.0) as u8;
            let g = g_val.number_value(scope).unwrap_or(0.0) as u8;
            let b = b_val.number_value(scope).unwrap_or(0.0) as u8;
            return Ok(Color::Rgb { r, g, b });
        }

        // Check if it's an ANSI color with bold
        let color_key = v8::String::new(scope, "color").unwrap().into();
        let bold_key = v8::String::new(scope, "bold").unwrap().into();

        if let Some(color_val) = obj.get(scope, color_key) {
            let bold = obj
                .get(scope, bold_key)
                .is_some_and(|v| v.boolean_value(scope));
            let color_str = color_val.to_rust_string_lossy(scope);
            let Some(ansi_color) = ansi_color_by_name(&color_str) else {
                bail!("Unknown ANSI color: {}", color_str);
            };
            return Ok(Color::Ansi {
                color: ansi_color,
                bold,
            });
        }

        bail!("Invalid color object")
    } else {
        bail!("Color must be a string or object")
    }
}

/// Helper function to create a Style from JavaScript values
fn parse_style_from_js(
    scope: &mut v8::PinScope,
    fg_val: Option<v8::Local<v8::Value>>,
    bg_val: Option<v8::Local<v8::Value>>,
) -> Result<Style, AnyError> {
    let fg = match fg_val {
        Some(val) => parse_color_from_js(scope, val)?,
        None => Color::DefaultForeground { bold: false },
    };

    let bg = match bg_val {
        Some(val) => normalize_bg(parse_color_from_js(scope, val)?),
        None => Color::DefaultBackground,
    };

    Ok(Style { fg, bg })
}

#[op2(fast)]
fn op_smudgy_insert(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    #[string] text: String,
    begin: u32,
    end: u32,
    fg_color: v8::Local<v8::Value>,
    bg_color: v8::Local<v8::Value>,
) -> Result<(), LineCallError> {
    // Inserting into the current line changes what the user sees (→ change-display).
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    // Parse the style
    let style = match parse_style_from_js(
        scope,
        if fg_color.is_null_or_undefined() {
            None
        } else {
            Some(fg_color)
        },
        if bg_color.is_null_or_undefined() {
            None
        } else {
            Some(bg_color)
        },
    ) {
        Ok(style) => style,
        Err(_) => return Ok(()), // Silently fail on error
    };

    // Get pending line operations from state
    let pending_ops = state.borrow::<Rc<RefCell<Vec<LineOperation>>>>();
    let mut ops = pending_ops.borrow_mut();

    // Add the insert operation
    ops.push(LineOperation::Insert {
        str: Arc::new(text),
        begin: begin as usize,
        end: end as usize,
        style,
    });
    Ok(())
}

#[op2(fast)]
fn op_smudgy_replace(
    state: &mut OpState,
    #[string] text: String,
    begin: u32,
    end: u32,
) -> Result<(), LineCallError> {
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    // Get pending line operations from state
    let pending_ops = state.borrow::<Rc<RefCell<Vec<LineOperation>>>>();
    let mut ops = pending_ops.borrow_mut();

    // Add the replace operation
    ops.push(LineOperation::Replace {
        str: Arc::new(text),
        begin: begin as usize,
        end: end as usize,
    });
    Ok(())
}

#[op2(fast)]
fn op_smudgy_highlight(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    begin: u32,
    end: u32,
    fg_color: v8::Local<v8::Value>,
    bg_color: v8::Local<v8::Value>,
) -> Result<(), LineCallError> {
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    // Parse the style
    let style = match parse_style_from_js(
        scope,
        if fg_color.is_null_or_undefined() {
            None
        } else {
            Some(fg_color)
        },
        if bg_color.is_null_or_undefined() {
            None
        } else {
            Some(bg_color)
        },
    ) {
        Ok(style) => style,
        Err(_) => return Ok(()), // Silently fail on error
    };

    // Get pending line operations from state
    let pending_ops = state.borrow::<Rc<RefCell<Vec<LineOperation>>>>();
    let mut ops = pending_ops.borrow_mut();

    // Add the highlight operation
    ops.push(LineOperation::Highlight {
        begin: begin as usize,
        end: end as usize,
        style,
    });
    Ok(())
}

#[op2(fast)]
fn op_smudgy_remove(state: &mut OpState, begin: u32, end: u32) -> Result<(), LineCallError> {
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    // Get pending line operations from state
    let pending_ops = state.borrow::<Rc<RefCell<Vec<LineOperation>>>>();
    let mut ops = pending_ops.borrow_mut();

    // Add the remove operation
    ops.push(LineOperation::Remove {
        begin: begin as usize,
        end: end as usize,
    });
    Ok(())
}

#[op2(fast)]
fn op_smudgy_gag(state: &mut OpState) -> Result<(), LineCallError> {
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    // Suppression is routing state, not a transform: transforms queued before or after
    // still apply to any routed copies of the line (`line.gag(); line.replace(...)`
    // replaces on the copies; gag only removes main from the sink set).
    state
        .borrow::<crate::session::runtime::SharedLineRouting>()
        .borrow_mut()
        .gag = true;
    Ok(())
}

// ============================================================================
// Panes: session-owned, name-keyed output panes (docs/panes.md).
// Own-session ops mutate the shared `PaneRegistry` synchronously; cross-session
// ops (gated by `reach-others` like every cross-session route) carry NAMES —
// the target registry lives on another session's thread — and resolve at
// dispatch on the owning runtime. Line routing (`redirect`/`copy`) is
// current-session by construction (triggers run in the session).
// ============================================================================

/// The error a pane op throws: a registry rule refusal ([`pane::PaneError`])
/// or an op-level constraint (wrong pane kind, cross-session introspection).
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
#[class(generic)]
#[error("smudgy: {0}")]
pub struct PaneOpError(String);

impl From<pane::PaneError> for PaneOpError {
    fn from(err: pane::PaneError) -> Self {
        Self(err.to_string())
    }
}

/// Errors a pane op can throw: the capability denial, a pane rule refusal, or
/// (for the line-routing ops) the current-line staleness refusal.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
pub enum PaneCallError {
    #[class(inherit)]
    #[error(transparent)]
    NotCapable(#[from] NotCapable),
    #[class(inherit)]
    #[error(transparent)]
    Pane(#[from] PaneOpError),
    #[class(inherit)]
    #[error(transparent)]
    NotCurrent(#[from] LineNotCurrent),
}

impl From<pane::PaneError> for PaneCallError {
    fn from(err: pane::PaneError) -> Self {
        Self::Pane(err.into())
    }
}

/// The pane namespace the calling isolate creates/resolves panes in: user
/// scripts and the Main isolate share [`pane::PaneNamespace::User`]; each
/// package isolate gets its owner/name pair — version deliberately excluded,
/// so the namespace is stable across package upgrades/reloads.
fn pane_namespace(state: &OpState) -> pane::PaneNamespace {
    match state.borrow::<IsolateId>() {
        IsolateId::Main => pane::PaneNamespace::User,
        IsolateId::Package { owner, name, .. } => pane::PaneNamespace::Package {
            owner: owner.clone(),
            name: name.clone(),
        },
    }
}

fn pane_registry(state: &OpState) -> crate::session::runtime::SharedPaneRegistry {
    state
        .borrow::<crate::session::runtime::SharedPaneRegistry>()
        .clone()
}

/// The own-session half of a terminal-pane delivery op: resolve (and kind-check) the
/// pane synchronously and return its key, so a delivery issued before a `close()` in
/// the same script body still lands on that incarnation. Cross-session targets return
/// `None` — the registry lives on another thread, so the name resolves at dispatch on
/// the owning runtime instead.
fn resolve_own_terminal_pane(
    state: &OpState,
    target: SessionId,
    namespace: &pane::PaneNamespace,
    name: &str,
) -> Result<Option<pane::PaneKey>, PaneOpError> {
    if target != *state.borrow::<SessionId>() {
        return Ok(None);
    }
    let registry = pane_registry(state);
    let registry = registry.borrow();
    let def = registry
        .resolve(namespace, name)
        .ok_or_else(|| PaneOpError(format!("no pane named '{name}'")))?;
    if def.kind != pane::PaneKind::Terminal {
        return Err(PaneOpError(format!("pane '{name}' has no terminal")));
    }
    Ok(Some(def.key))
}

/// The wire shape of one pane handed back to JS (`Pane` handles are built
/// from this in the facade). `name_id` is the interned per-session name
/// identity the per-line fast path uses; `None` on a cross-session optimistic
/// handle (a foreign registry's ids are meaningless here).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneInfo {
    name: String,
    kind: &'static str,
    is_main: bool,
    name_id: Option<u32>,
    created: bool,
    /// Whether the pane hosts its own input line (`docs/input.md`
    /// §3.7). Backs `pane.input` returning a handle vs `undefined`.
    has_input: bool,
}

impl PaneInfo {
    fn from_def(def: &pane::PaneDef, created: bool) -> Self {
        Self {
            name: def.name.to_string(),
            kind: match def.kind {
                pane::PaneKind::Terminal => "terminal",
                pane::PaneKind::Widgets => "widgets",
            },
            is_main: def.is_main,
            name_id: Some(def.name_id.as_u32()),
            created,
            has_input: def.input.is_some(),
        }
    }
}

// ============================================================================
// The command input (`docs/input.md`): reads against the
// session-thread mirror, writes as operations forwarded to the UI widget.
// Inputs are addressed like pane deliveries — by name in the caller's
// namespace — and every input op is own-session only: the mirror lives on this
// thread, and a foreign session's input is not addressable.
// ============================================================================

/// The shared back half of resolving the caller's own pane input by name:
/// resolution in the caller's namespace, the `panes` capability (the pane is
/// the caller's own surface — namespacing already isolates cross-package
/// access), and the pane-hosts-an-input rule. A pane without an input —
/// either kind — is refused here; the UI's warn-and-drop on a key with no
/// input is defense in depth behind this check. The own-session rule and the
/// `main` fork stay at the call sites (the input surface admits main under
/// the `input` capability; the registration op refuses it outright).
fn resolve_own_pane_input(state: &mut OpState, name: &str) -> Result<pane::PaneKey, PaneCallError> {
    let namespace = pane_namespace(state);
    let registry = pane_registry(state);
    let registry = registry.borrow();
    let def = registry
        .resolve(&namespace, name)
        .ok_or_else(|| PaneOpError(format!("no pane named '{name}'")))?;
    ensure(grants(state).panes, "panes")?;
    if def.input.is_none() {
        return Err(PaneOpError(format!("pane '{name}' has no input")).into());
    }
    Ok(def.key)
}

/// The shared front half of every input op: the own-session rule, name→key
/// resolution, and a capability gate that depends on the target
/// (`docs/input.md` §3.6). The MAIN input is the session-global
/// surface a package can read and rewrite the user's typing through, so it
/// requires the `input` capability; a pane input rides
/// [`resolve_own_pane_input`]'s `panes` gate.
fn resolve_input_target(
    state: &mut OpState,
    session_id: u32,
    name: &str,
) -> Result<pane::PaneKey, PaneCallError> {
    let target = SessionId::from(session_id);
    if target != *state.borrow::<SessionId>() {
        return Err(PaneOpError(
            "input handles are own-session only (another session's input is not addressable)"
                .to_string(),
        )
        .into());
    }
    if pane::is_main_pane_name(name) {
        ensure(grants(state).input, "input")?;
        return Ok(pane::MAIN_PANE_KEY);
    }
    resolve_own_pane_input(state, name)
}

/// Read one input's mirrored state. Eventually consistent: reads reflect the
/// last state update the UI delivered (the first read after interest is
/// flagged may see the default empty state until the UI's snapshot lands).
/// Empty of content while masked.
///
/// Reads — and only reads — flag mirror interest: a session that merely
/// writes (a `propose()`-only script, say) never subscribes itself to
/// per-keystroke state traffic. The one-time interest notification for the UI
/// is queued on the flip.
#[op2]
#[serde]
fn op_smudgy_input_get(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
) -> Result<crate::session::runtime::input::InputSnapshot, PaneCallError> {
    let key = resolve_input_target(state, session_id, name)?;
    let mirror = state
        .borrow::<crate::session::runtime::SharedInputMirror>()
        .clone();
    let flipped = mirror.borrow_mut().flag_interest();
    if flipped {
        queue_own_action(state, RuntimeAction::InputMirrorInterest);
    }
    let snapshot = mirror.borrow().snapshot(key);
    Ok(snapshot)
}

/// The wire shape of one scripted input mutation, tagged by verb — every
/// `input.*` write verb funnels through the one apply op with one of these.
/// Positions count UTF-16 code units (the JS string indexing unit); the UI
/// clamps them to the buffer.
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum InputApplyWire {
    Replace { text: String },
    Append { text: String },
    Clear,
    Propose { text: String },
    SetCursor { pos: u32 },
    Select { start: u32, end: u32 },
    SelectAll,
    Focus,
    Blur,
    Submit,
    SetMasked { masked: bool },
}

impl From<InputApplyWire> for crate::session::runtime::input::InputOp {
    fn from(wire: InputApplyWire) -> Self {
        use crate::session::runtime::input::InputOp;
        match wire {
            InputApplyWire::Replace { text } => InputOp::Replace(Arc::new(text)),
            InputApplyWire::Append { text } => InputOp::Append(Arc::new(text)),
            InputApplyWire::Clear => InputOp::Clear,
            InputApplyWire::Propose { text } => InputOp::Propose(Arc::new(text)),
            InputApplyWire::SetCursor { pos } => InputOp::SetCursor(pos as usize),
            InputApplyWire::Select { start, end } => InputOp::Select(start as usize, end as usize),
            InputApplyWire::SelectAll => InputOp::SelectAll,
            InputApplyWire::Focus => InputOp::Focus,
            InputApplyWire::Blur => InputOp::Blur,
            InputApplyWire::Submit => InputOp::Submit,
            InputApplyWire::SetMasked { masked } => InputOp::SetMasked(masked),
        }
    }
}

/// Apply one scripted input mutation (the write half of the input surface):
/// resolve and check the target synchronously, then queue the op for the UI
/// to apply to the live widget. Writes never flag mirror interest.
#[op2]
fn op_smudgy_input_apply(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
    #[serde] op: InputApplyWire,
) -> Result<(), PaneCallError> {
    let key = resolve_input_target(state, session_id, name)?;
    queue_own_action(state, RuntimeAction::InputApply { key, op: op.into() });
    Ok(())
}

/// Errors a submission op can throw: the capability denial, or use of the
/// ambient `submission` object outside the `sys:input` handler call it was
/// delivered to (never delivered, delivered but already completed, or a stale
/// async continuation whose submission has since been replaced by another).
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
pub enum SubmissionCallError {
    #[class(inherit)]
    #[error(transparent)]
    NotCapable(#[from] NotCapable),
    #[class(generic)]
    #[error("smudgy: submission is only usable inside a sys:input handler")]
    NoActiveSubmission,
}

/// The shared front half of the submission ops: the `input` capability gate
/// (the same gate as the rest of the input surface — and as `sys:input`
/// subscription itself, so an isolate that could not subscribe cannot act
/// either), then the live-submission requirement. The live cell holds a
/// submission only while the `SubmitInput` dispatch arm's handler splice is
/// running, and `generation` must name THAT submission: the script side
/// captures it at delivery (scoped to the synchronous handler call), so an
/// async handler continuation that outlives its submission arrives with a
/// generation the slot no longer holds — 0 outside any handler call, or its
/// own dead submission's while a later one is live — and throws instead of
/// acting on a submission it was never delivered.
fn with_submission<T>(
    state: &mut OpState,
    generation: u32,
    f: impl FnOnce(&mut crate::session::runtime::input::InputSubmission) -> T,
) -> Result<T, SubmissionCallError> {
    ensure(grants(state).input, "input")?;
    let cell = state
        .borrow::<crate::session::runtime::SharedInputSubmission>()
        .clone();
    let mut slot = cell.borrow_mut();
    match slot.live_mut() {
        Some(submission) if submission.generation() == generation => Ok(f(submission)),
        _ => Err(SubmissionCallError::NoActiveSubmission),
    }
}

/// The live submission's generation, or 0 when none is live. Read by the
/// `sys:input` delivery wrapper at splice time — during a delivery the live
/// submission is the delivered one — and scoped to the synchronous handler
/// call; the submission ops then require the value back (see
/// [`with_submission`]).
#[op2(fast)]
fn op_smudgy_input_submission_generation(state: &mut OpState) -> Result<u32, NotCapable> {
    ensure(grants(state).input, "input")?;
    let cell = state
        .borrow::<crate::session::runtime::SharedInputSubmission>()
        .clone();
    let generation = cell.borrow().live_generation();
    Ok(generation)
}

/// `submission.text` — the submitted line as it currently stands (an earlier
/// handler's `replace()` is visible to a later handler's read).
#[op2]
#[string]
fn op_smudgy_input_submission_text(
    state: &mut OpState,
    generation: u32,
) -> Result<String, SubmissionCallError> {
    with_submission(state, generation, |submission| {
        submission.text().to_string()
    })
}

/// `submission.replace(text)` — substitute what enters the pipeline when the
/// handlers finish.
#[op2(fast)]
fn op_smudgy_input_submission_replace(
    state: &mut OpState,
    generation: u32,
    #[string] text: &str,
) -> Result<(), SubmissionCallError> {
    with_submission(state, generation, |submission| submission.replace(text))
}

/// `submission.cancel()` — swallow the submission entirely. Sticky: a cancel
/// wins over any replace, from any handler, in any order.
#[op2(fast)]
fn op_smudgy_input_submission_cancel(
    state: &mut OpState,
    generation: u32,
) -> Result<(), SubmissionCallError> {
    with_submission(
        state,
        generation,
        crate::session::runtime::input::InputSubmission::cancel,
    )
}

/// The wire shape of one word-set mutation, tagged by verb — every
/// `WordSetRegistry` write verb funnels through the one mutate op with one of
/// these, the target set riding as a field
/// ([`input::WordSetKind`](crate::session::runtime::input::WordSetKind)
/// deserializes itself).
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum WordSetMutateWire {
    Add {
        set: crate::session::runtime::input::WordSetKind,
        words: Vec<String>,
    },
    Delete {
        set: crate::session::runtime::input::WordSetKind,
        word: String,
    },
    Clear {
        set: crate::session::runtime::input::WordSetKind,
    },
}

/// The wire shape of one word-set read, mirroring [`WordSetMutateWire`].
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum WordSetQueryWire {
    Has {
        set: crate::session::runtime::input::WordSetKind,
        word: String,
    },
    List {
        set: crate::session::runtime::input::WordSetKind,
    },
}

/// Errors a word-set registry op can throw: anything the shared input-target
/// gate refuses (own-session only; the `input` capability for the main
/// input, the `panes` capability for the caller's own pane input; a pane
/// without an input), a creator-identity failure (unreachable in practice —
/// the ids are host-minted), a rejected word, or a full set.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
pub enum WordSetCallError {
    #[class(inherit)]
    #[error(transparent)]
    Call(#[from] PaneCallError),
    #[class(inherit)]
    #[error(transparent)]
    Store(#[from] StoreOpError),
    #[class(generic)]
    #[error("{0}")]
    InvalidWord(String),
    #[class(generic)]
    #[error(
        "smudgy: a script may register at most {} completion words per set on an input; \
         this add() would exceed that, so nothing was registered. delete() or clear() \
         words you no longer need",
        crate::session::runtime::input::MAX_WORDS_PER_CREATOR
    )]
    SetFull,
}

/// A registered completion word is one token: non-empty, no whitespace, at
/// most [`MAX_WORD_CHARS`](crate::session::runtime::input::MAX_WORD_CHARS)
/// characters. Enforced at registration (`add`) — the author-input gate is
/// loud, like every other one; the lookup verbs simply miss on such a string.
fn validate_word_set_word(word: &str) -> Result<(), WordSetCallError> {
    if word.is_empty() {
        return Err(WordSetCallError::InvalidWord(
            "smudgy: a completion word must be a non-empty string".to_string(),
        ));
    }
    if word.chars().any(char::is_whitespace) {
        return Err(WordSetCallError::InvalidWord(format!(
            "smudgy: a completion word is one token with no whitespace; \
             add({word:?}) should be add() calls for each word"
        )));
    }
    let max = crate::session::runtime::input::MAX_WORD_CHARS;
    if word.chars().count() > max {
        return Err(WordSetCallError::InvalidWord(format!(
            "smudgy: a completion word is at most {max} characters; completion \
             inserts single words, not phrases or payloads"
        )));
    }
    Ok(())
}

/// The caller's word-set creator identity: the same `(isolate, origin)` pair
/// that keys automations, so `delete()`/`clear()`/`list()` see only the
/// caller's own contributions.
fn word_set_creator(
    state: &OpState,
    creator_id: u32,
) -> Result<crate::session::runtime::input::WordSetCreator, StoreOpError> {
    Ok((current_isolate(state), creator_origin(state, creator_id)?))
}

/// Mutate the caller's contribution to one input's suggestion set or
/// blacklist (the write half of the `WordSetRegistry` surface,
/// `docs/input.md` §3.8). Word identity within a contribution is
/// case-insensitive with the registered casing preserved (a re-add updates
/// the casing in place). `add` is atomic: the whole batch is validated —
/// each word a bounded single token, the resulting count within the
/// per-creator cap — before anything lands. Returns `delete`'s hit verdict;
/// `add`/`clear` return null. A mutation that changed anything queues one
/// coalesced merged-view push for the UI.
#[op2]
#[serde]
fn op_smudgy_input_words_mutate(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
    creator_id: u32,
    #[serde] op: WordSetMutateWire,
) -> Result<serde_json::Value, WordSetCallError> {
    let key = resolve_input_target(state, session_id, name)?;
    let creator = word_set_creator(state, creator_id)?;
    let sets = state
        .borrow::<crate::session::runtime::SharedInputWordSets>()
        .clone();
    let (changed, verdict) = match op {
        WordSetMutateWire::Add { set, words } => {
            for word in &words {
                validate_word_set_word(word)?;
            }
            let changed = sets
                .borrow_mut()
                .add(key, set, &creator, &words)
                .map_err(|_| WordSetCallError::SetFull)?;
            (changed, serde_json::Value::Null)
        }
        WordSetMutateWire::Delete { set, word } => {
            let hit = sets.borrow_mut().delete(key, set, &creator, &word);
            (hit, serde_json::Value::Bool(hit))
        }
        WordSetMutateWire::Clear { set } => {
            let changed = sets.borrow_mut().clear(key, set, &creator);
            (changed, serde_json::Value::Null)
        }
    };
    if changed && sets.borrow_mut().flag_push(key) {
        queue_own_action(state, RuntimeAction::InputWordSetsChanged { key });
    }
    Ok(verdict)
}

/// Read the caller's contribution to one input's suggestion set or blacklist
/// (the read half of the `WordSetRegistry` surface). Exact, not mirrored: the
/// authoritative sets live on this thread, so `has`/`list` see every earlier
/// mutation. `list` returns the caller's own words in insertion order,
/// registered casing.
#[op2]
#[serde]
fn op_smudgy_input_words_query(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
    creator_id: u32,
    #[serde] op: WordSetQueryWire,
) -> Result<serde_json::Value, WordSetCallError> {
    let key = resolve_input_target(state, session_id, name)?;
    let creator = word_set_creator(state, creator_id)?;
    let sets = state
        .borrow::<crate::session::runtime::SharedInputWordSets>()
        .clone();
    let sets = sets.borrow();
    Ok(match op {
        WordSetQueryWire::Has { set, word } => {
            serde_json::Value::Bool(sets.has(key, set, &creator, &word))
        }
        WordSetQueryWire::List { set } => serde_json::Value::Array(
            sets.list(key, set, &creator)
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    })
}

/// The wire shape of one history mutation, tagged by verb
/// (`docs/input.md` §3.9). The history verbs follow the word-set op
/// pair — one serde-tagged mutate op beside a plain read — rather than the
/// `InputApplyWire`/`InputSnapshot` funnel: the read half returns the
/// mirrored history entries, not the state snapshot, so `op_smudgy_input_get`
/// stays lean for the per-read value/cursor traffic.
#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum InputHistoryMutateWire {
    Push { text: String },
    Clear,
}

/// Errors a history op can throw: anything the shared input-target gate
/// refuses (own-session only; the `input` capability for the main input,
/// the `panes` capability for the caller's own pane input; a pane without
/// an input), or a rejected entry.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
pub enum InputHistoryCallError {
    #[class(inherit)]
    #[error(transparent)]
    Call(#[from] PaneCallError),
    #[class(generic)]
    #[error("{0}")]
    InvalidEntry(String),
}

/// A pushed history entry is one line: non-blank, no `\n`/`\r` (history
/// recall pastes an entry back into a single-line input). No length cap
/// beyond what history itself enforces. Loud at the op boundary, like every
/// other author-input gate — the UI's history keeps a trim check of its own,
/// which would otherwise swallow a whitespace-only push silently.
fn validate_history_entry(text: &str) -> Result<(), InputHistoryCallError> {
    if text.trim().is_empty() {
        return Err(InputHistoryCallError::InvalidEntry(
            "smudgy: a history entry must be a non-blank string".to_string(),
        ));
    }
    if text.contains(['\n', '\r']) {
        return Err(InputHistoryCallError::InvalidEntry(
            "smudgy: a history entry is a single line; push() each command separately".to_string(),
        ));
    }
    Ok(())
}

/// Mutate one input's real history (the write half of the `input.history`
/// surface, `docs/input.md` §3.9): a validated push or a clear
/// crosses to the UI as an [`InputOp`](crate::session::runtime::input::InputOp)
/// like every other input mutation, and the UI's confirming history update
/// comes back unconditionally once the change lands. History is user-owned
/// and session-global per input — there is no creator scoping.
#[op2]
fn op_smudgy_input_history_mutate(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
    #[serde] op: InputHistoryMutateWire,
) -> Result<(), InputHistoryCallError> {
    use crate::session::runtime::input::InputOp;
    let key = resolve_input_target(state, session_id, name)?;
    let op = match op {
        InputHistoryMutateWire::Push { text } => {
            validate_history_entry(&text)?;
            InputOp::HistoryPush(Arc::new(text))
        }
        InputHistoryMutateWire::Clear => InputOp::HistoryClear,
    };
    queue_own_action(state, RuntimeAction::InputApply { key, op });
    Ok(())
}

/// Read one input's mirrored history, newest first. Exact with respect to the
/// last submission: the UI sends every history change unconditionally (it
/// changes per submission, never per keystroke), so unlike the state snapshot
/// this read flags no mirror interest. Masked submissions never enter the
/// UI's history, so they never appear here.
#[op2]
#[serde]
fn op_smudgy_input_history_list(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
) -> Result<Vec<String>, PaneCallError> {
    let key = resolve_input_target(state, session_id, name)?;
    let mirror = state
        .borrow::<crate::session::runtime::SharedInputMirror>()
        .clone();
    let entries = mirror.borrow().history(key);
    Ok(entries
        .iter()
        .map(|entry| entry.as_str().to_string())
        .collect())
}

/// The display half of a `PaneSpec.input` (`docs/input.md` §3.7).
/// The `onSubmit` handler itself never rides the serde spec — the facade
/// registers it through [`op_smudgy_pane_input_on_submit`] right after the
/// split, since a v8 function cannot cross a serde boundary.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneInputSpecJs {
    placeholder: Option<String>,
}

/// The script-facing split spec (`PaneSpec`), parsed from JSON.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneSpecJs {
    name: String,
    width: Option<f64>,
    height: Option<f64>,
    /// `false` ⇒ a widgets-only pane (no terminal scrollback); default `true`.
    /// Every pane hosts widgets either way.
    terminal: Option<bool>,
    /// `'normal' | 'always-show'`; omitted leaves an existing pane's policy
    /// alone and defaults a new pane to `'normal'`.
    title_bar: Option<String>,
    /// The pane's own input line. Own-session only: the `onSubmit` handler
    /// runs in the creating isolate, which a cross-session split cannot name.
    input: Option<PaneInputSpecJs>,
}

/// Get-or-create a pane and (on creation) place it by splitting off `ref_name`
/// toward `direction`. Own-session calls mutate the registry synchronously and
/// return the real pane; cross-session calls (`reach-others`) queue a
/// name-carrying action to the owning runtime (last-writer-wins) and return an
/// optimistic handle with no `name_id`.
#[op2]
#[serde]
fn op_smudgy_pane_split(
    state: &mut OpState,
    session_id: u32,
    #[string] ref_name: &str,
    #[string] direction: &str,
    #[serde] spec: PaneSpecJs,
) -> Result<PaneInfo, PaneCallError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).panes, "panes")?;

    let direction = pane::SplitDirection::parse(direction)
        .ok_or_else(|| PaneOpError(format!("invalid split direction '{direction}'")))?;
    let kind = if spec.terminal.unwrap_or(true) {
        pane::PaneKind::Terminal
    } else {
        pane::PaneKind::Widgets
    };
    let title_bar = spec
        .title_bar
        .as_deref()
        .map(|raw| {
            pane::TitleBarPolicy::parse(raw)
                .ok_or_else(|| PaneOpError(format!("invalid titleBar '{raw}'")))
        })
        .transpose()?;
    // The initial size is measured along the split axis; the off-axis
    // dimension is ignored (documented).
    #[allow(clippy::cast_possible_truncation)]
    let size_px = match direction {
        pane::SplitDirection::Left | pane::SplitDirection::Right => spec.width,
        pane::SplitDirection::Top | pane::SplitDirection::Bottom => spec.height,
    }
    .map(|px| px as f32)
    .filter(|px| px.is_finite() && *px > 0.0);

    let namespace = pane_namespace(state);

    let input = spec.input.map(|input| pane::PaneInputDef {
        placeholder: input.placeholder.map(Arc::from),
    });

    if target == *state.borrow::<SessionId>() {
        let registry = pane_registry(state);
        let outcome = registry
            .borrow_mut()
            .split(&namespace, &spec.name, kind, title_bar, input)?;
        if outcome.created {
            let reference = registry
                .borrow()
                .resolve(&namespace, ref_name)
                .map_or(pane::MAIN_PANE_KEY, |def| def.key);
            queue_own_action(
                state,
                RuntimeAction::PaneOpened {
                    def: outcome.def.clone(),
                    placement: pane::PanePlacement {
                        reference,
                        direction,
                        size_px,
                    },
                },
            );
        } else if outcome.title_bar_changed {
            queue_own_action(
                state,
                RuntimeAction::PaneUpdated {
                    def: outcome.def.clone(),
                },
            );
        }
        Ok(PaneInfo::from_def(&outcome.def, outcome.created))
    } else {
        // A pane input's onSubmit runs in the CREATING isolate, which lives on
        // this session's thread — a foreign session's pane could never deliver
        // to it, so the spec is refused rather than silently dropped.
        if input.is_some() {
            return Err(PaneOpError(
                "pane inputs are own-session only (another session's pane cannot deliver \
                 submissions to your onSubmit handler)"
                    .to_string(),
            )
            .into());
        }
        route_session_action(
            state,
            target,
            RuntimeAction::PaneSplitRemote {
                namespace,
                name: Arc::from(spec.name.as_str()),
                kind,
                title_bar,
                reference: Some(Arc::from(ref_name)),
                direction,
                size_px,
            },
        );
        Ok(PaneInfo {
            kind: match kind {
                pane::PaneKind::Terminal => "terminal",
                pane::PaneKind::Widgets => "widgets",
            },
            name: spec.name,
            is_main: false,
            name_id: None,
            created: false,
            has_input: false,
        })
    }
}

/// Register (or replace) the `onSubmit` handler for a pane input
/// (`docs/input.md` §3.7). The facade calls this right after the
/// split that carried `input` in its spec — a v8 function cannot ride the
/// serde spec — and again on every re-claiming split, which is what brings a
/// handler back after a reload (handler addresses are engine facts and die
/// with their isolate generation, like widget callbacks). Own-session only,
/// gated by `panes` like the split itself: the pane is the caller's own
/// surface, in its own namespace. `main` is refused with the split's
/// [`pane::PaneError::MainInput`] teaching — its fused input is
/// `session.input`, and typed submissions are intercepted through
/// `sys:input`, never an `onSubmit`.
#[op2(fast)]
fn op_smudgy_pane_input_on_submit<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
    f: v8::Local<'s, v8::Function>,
) -> Result<(), PaneCallError> {
    ensure(grants(state).panes, "panes")?;
    let target = SessionId::from(session_id);
    if target != *state.borrow::<SessionId>() {
        return Err(PaneOpError(
            "pane inputs are own-session only (another session's pane cannot deliver \
             submissions to your onSubmit handler)"
                .to_string(),
        )
        .into());
    }
    if pane::is_main_pane_name(name) {
        return Err(PaneOpError(format!(
            "{}; intercept typed submissions with the sys:input event instead",
            pane::PaneError::MainInput
        ))
        .into());
    }
    let key = resolve_own_pane_input(state, name)?;
    // The handler lives in this isolate's `script_functions` registry (append-only,
    // reclaimed with the engine), like event subscribers; only its address crosses
    // to the session-side registry the dispatch arm resolves through.
    let f = v8::Global::new(scope, f);
    let function_id = {
        let mut script_functions = state
            .borrow::<Rc<RefCell<Vec<v8::Global<v8::Function>>>>>()
            .borrow_mut();
        let id = FunctionId(script_functions.len());
        script_functions.push(f);
        id
    };
    let callback = crate::session::runtime::input::PaneInputCallback {
        isolate: current_isolate(state),
        instance: state.borrow::<IsolateInstance>().0,
        function_id,
    };
    state
        .borrow::<crate::session::runtime::SharedPaneInputCallbacks>()
        .borrow_mut()
        .register(key, callback);
    Ok(())
}

/// Close a pane by name. Closing main throws; closing an already-closed name
/// is a no-op (idempotent). Cross-session closes are best-effort by name.
#[op2(fast)]
fn op_smudgy_pane_close(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
) -> Result<(), PaneCallError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).panes, "panes")?;
    let namespace = pane_namespace(state);

    if target == *state.borrow::<SessionId>() {
        let closed = pane_registry(state).borrow_mut().close(&namespace, name);
        match closed {
            Ok(key) => {
                // The closed pane's input state — mirror, word sets, onSubmit
                // registration — dies with it (keys are never reused, so none
                // of it could be read again).
                crate::session::runtime::input::purge_pane_input_state(
                    state.borrow::<crate::session::runtime::SharedInputMirror>(),
                    state.borrow::<crate::session::runtime::SharedInputWordSets>(),
                    state.borrow::<crate::session::runtime::SharedPaneInputCallbacks>(),
                    key,
                );
                queue_own_action(state, RuntimeAction::PaneClosed { key });
                Ok(())
            }
            Err(pane::PaneError::NoSuchPane(_)) => Ok(()),
            Err(err) => Err(err.into()),
        }
    } else {
        route_session_action(
            state,
            target,
            RuntimeAction::PaneCloseRemote {
                namespace,
                name: Arc::from(name),
            },
        );
        Ok(())
    }
}

/// Echo whole lines into a terminal pane. Own-session calls validate the pane
/// (exists + terminal) synchronously; the delivery itself resolves at
/// dispatch by name either way.
#[op2(fast)]
fn op_smudgy_pane_echo(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
    #[string] text: &str,
) -> Result<(), PaneCallError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).panes, "panes")?;
    let namespace = pane_namespace(state);
    let key = resolve_own_terminal_pane(state, target, &namespace, name)?;
    route_session_action(
        state,
        target,
        RuntimeAction::PaneEcho {
            key,
            namespace,
            name: Arc::from(name),
            text: Arc::new(text.to_string()),
        },
    );
    Ok(())
}

/// The styled sibling of [`op_smudgy_pane_echo`]: same gate and key/name resolution,
/// with the packed styled payload of [`op_smudgy_session_echo_styled`].
#[op2(fast)]
fn op_smudgy_pane_echo_styled(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
    #[string] text: &str,
    #[buffer] records: &[u32],
    callbacks: v8::Local<v8::Array>,
) -> Result<(), StyledTextOpError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).panes, "panes")?;
    let namespace = pane_namespace(state);
    let key = resolve_own_terminal_pane(state, target, &namespace, name)?;
    packed_validate(text, records, callbacks.length())?;
    let links = link_context(scope, state, callbacks)?;
    let lines = packed_echo_lines(text, records, &links, ECHO_DEFAULT_STYLE)?;
    route_session_action(
        state,
        target,
        RuntimeAction::PaneEchoStyled {
            key,
            namespace,
            name: Arc::from(name),
            lines,
        },
    );
    Ok(())
}

/// Clear a terminal pane's scrollback (`"main"` clears the main buffer).
#[op2(fast)]
fn op_smudgy_pane_clear(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
) -> Result<(), PaneCallError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).panes, "panes")?;
    let namespace = pane_namespace(state);
    let key = resolve_own_terminal_pane(state, target, &namespace, name)?;
    route_session_action(
        state,
        target,
        RuntimeAction::PaneClear {
            key,
            namespace,
            name: Arc::from(name),
        },
    );
    Ok(())
}

/// List the live panes visible to the caller's namespace (its own panes plus
/// main). Own-session only: a foreign registry lives on another thread.
#[op2]
#[serde]
fn op_smudgy_pane_list(
    state: &mut OpState,
    session_id: u32,
) -> Result<Vec<PaneInfo>, PaneCallError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).panes, "panes")?;
    if target != *state.borrow::<SessionId>() {
        return Err(PaneOpError(
            "cross-session pane introspection is not supported (panes.list/get/exists are own-session only)"
                .to_string(),
        )
        .into());
    }
    let namespace = pane_namespace(state);
    Ok(pane_registry(state)
        .borrow()
        .list(&namespace)
        .iter()
        .map(|def| PaneInfo::from_def(def, false))
        .collect())
}

/// Resolve a name to its live pane in the caller's namespace, or null. Backs
/// `panes.get`/`panes.exists` and the `createWidget` `pane`-option mount check
/// (any pane kind is a valid widget target, so that check is existence only).
/// Own-session only, like [`op_smudgy_pane_list`].
#[op2]
#[serde]
fn op_smudgy_pane_resolve(
    state: &mut OpState,
    session_id: u32,
    #[string] name: &str,
) -> Result<Option<PaneInfo>, PaneCallError> {
    let target = SessionId::from(session_id);
    ensure_session_target(state, target, grants(state).panes, "panes")?;
    if target != *state.borrow::<SessionId>() {
        return Err(PaneOpError(
            "cross-session pane introspection is not supported (panes.list/get/exists are own-session only)"
                .to_string(),
        )
        .into());
    }
    let namespace = pane_namespace(state);
    Ok(pane_registry(state)
        .borrow()
        .resolve(&namespace, name)
        .map(|def| PaneInfo::from_def(def, false)))
}

/// Resolve a line-routing target to its live key. `name_id >= 0` is the
/// integer fast path a `Pane` handle uses (one `live` lookup, no folding or
/// hashing under trigger spam); a negative id falls back to folding `name` in
/// the caller's namespace. Throws if the target is not live — routing to a
/// closed pane is an author bug, surfaced like any other pane rule.
fn resolve_routing_target(
    state: &OpState,
    name_id: i32,
    name: &str,
) -> Result<pane::PaneKey, PaneCallError> {
    let namespace = pane_namespace(state);
    let registry = pane_registry(state);
    let registry = registry.borrow();
    let def = if name_id >= 0 {
        #[allow(clippy::cast_sign_loss)]
        registry.live_by_name_id(pane::PaneNameId::from_u32(name_id as u32))
    } else {
        registry.resolve(&namespace, name)
    };
    let def = def.ok_or_else(|| PaneOpError(format!("no pane named '{name}'")))?;
    // The name_id fast path bypasses the namespace fold, so re-check ownership
    // here: a pane is only a valid routing target from its own namespace
    // (main resolves in every namespace, matching the bare-name path). Without
    // this, a package could route its line into another namespace's pane by
    // passing a guessed name_id integer.
    if !def.is_main && def.namespace != namespace {
        return Err(PaneOpError(format!("no pane named '{name}'")).into());
    }
    // A widgets-only pane has no terminal buffer; routing a line there would
    // gag it from main and then silently drop it in the UI (whole-line loss).
    // Refuse, exactly as pane.echo/pane.clear do.
    if def.kind != pane::PaneKind::Terminal {
        return Err(PaneOpError(format!("pane '{name}' has no terminal to route lines to")).into());
    }
    Ok(def.key)
}

/// `line.redirect(pane)`: gag the current line from main and deliver it to
/// the pane instead (repeated calls: last wins). Current-line, one-shot, like
/// gag. Requires `panes` plus `change-display` (it alters what the main
/// display shows — the same class as gag).
#[op2(fast)]
fn op_smudgy_redirect(
    state: &mut OpState,
    name_id: i32,
    #[string] name: &str,
) -> Result<(), PaneCallError> {
    ensure(grants(state).panes, "panes")?;
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    let key = resolve_routing_target(state, name_id, name)?;
    state
        .borrow::<crate::session::runtime::SharedLineRouting>()
        .borrow_mut()
        .redirect = Some(key);
    Ok(())
}

/// `line.copy(pane)`: additionally deliver the current line to the pane
/// (sinks are a deduplicated set). Gated like [`op_smudgy_redirect`].
#[op2(fast)]
fn op_smudgy_copy(
    state: &mut OpState,
    name_id: i32,
    #[string] name: &str,
) -> Result<(), PaneCallError> {
    ensure(grants(state).panes, "panes")?;
    ensure(grants(state).change_display, "change-display")?;
    ensure_current_line(state)?;
    let key = resolve_routing_target(state, name_id, name)?;
    let routing = state.borrow::<crate::session::runtime::SharedLineRouting>();
    let mut routing = routing.borrow_mut();
    if !routing.copies.contains(&key) {
        routing.copies.push(key);
    }
    Ok(())
}

#[op2]
fn op_smudgy_get_current_line<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
) -> v8::Local<'s, v8::String> {
    let current_line = state.borrow::<Rc<RefCell<Weak<StyledLine>>>>();
    if let Some(line) = Weak::upgrade(&current_line.borrow()) {
        return v8::String::new(scope, &line.text).unwrap();
    }
    v8::String::new(scope, "").unwrap()
}

#[op2]
fn op_smudgy_get_current_line_number<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
) -> v8::Local<'s, v8::Number> {
    let emitted_line_count = state.borrow::<std::rc::Weak<Cell<usize>>>();
    if let Some(line) = std::rc::Weak::upgrade(emitted_line_count) {
        return v8::Number::new(scope, (1 + line.get()) as f64);
    }
    v8::Number::new(scope, 0.0)
}

/// The ANSI palette slot's lowercase token (`"red"`, …), the same set [`parse_color_from_js`]
/// accepts on the write side.
fn ansi_color_token(color: AnsiColor) -> &'static str {
    match color {
        AnsiColor::Black => "black",
        AnsiColor::Red => "red",
        AnsiColor::Green => "green",
        AnsiColor::Yellow => "yellow",
        AnsiColor::Blue => "blue",
        AnsiColor::Magenta => "magenta",
        AnsiColor::Cyan => "cyan",
        AnsiColor::White => "white",
    }
}

/// Serialize a [`Color`] to a JS value in the SAME shape [`parse_color_from_js`] reads, so a
/// value read from `line.styles` / `buffer.line(n).styles` round-trips straight back into
/// `highlightAt`/`insert`. RGB -> `{ r, g, b }`; ANSI -> `{ color, bold }` (the object
/// form, so `bold: false` survives — the bare `"red"` token implies bold); the special slots
/// and the theme default -> their string tokens (`"echo"`/`"output"`/`"warn"`/`"default"`).
fn color_to_js<'s>(scope: &mut v8::PinScope<'s, '_>, color: Color) -> v8::Local<'s, v8::Value> {
    match color {
        Color::Rgb { r, g, b } => {
            let obj = v8::Object::new(scope);
            for (k, v) in [("r", r), ("g", g), ("b", b)] {
                let key = v8::String::new(scope, k).unwrap().into();
                let val = v8::Integer::new_from_unsigned(scope, u32::from(v)).into();
                obj.create_data_property(scope, key, val);
            }
            obj.into()
        }
        Color::Ansi { color, bold } => {
            let obj = v8::Object::new(scope);
            let color_key = v8::String::new(scope, "color").unwrap().into();
            let color_val = v8::String::new(scope, ansi_color_token(color))
                .unwrap()
                .into();
            obj.create_data_property(scope, color_key, color_val);
            let bold_key = v8::String::new(scope, "bold").unwrap().into();
            let bold_val = v8::Boolean::new(scope, bold).into();
            obj.create_data_property(scope, bold_key, bold_val);
            obj.into()
        }
        Color::Echo => v8::String::new(scope, "echo").unwrap().into(),
        Color::Output => v8::String::new(scope, "output").unwrap().into(),
        Color::Warn => v8::String::new(scope, "warn").unwrap().into(),
        Color::DefaultForeground { .. } | Color::DefaultBackground => {
            v8::String::new(scope, "default").unwrap().into()
        }
    }
}

/// Serialize a line's spans to the styles array: `[{ begin, end, fg, bg }]`, each color in
/// the round-trippable shape from [`color_to_js`]. Built lazily only when a script reads
/// `styles`, so the common (never-read-styles) path pays nothing.
fn spans_to_js<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    line: &StyledLine,
) -> v8::Local<'s, v8::Array> {
    let elements: Vec<v8::Local<v8::Value>> = line
        .spans
        .iter()
        .map(|span| {
            let obj = v8::Object::new(scope);
            let begin_key = v8::String::new(scope, "begin").unwrap().into();
            let begin_val = v8::Integer::new_from_unsigned(
                scope,
                u32::try_from(span.begin_pos).unwrap_or(u32::MAX),
            )
            .into();
            obj.create_data_property(scope, begin_key, begin_val);
            let end_key = v8::String::new(scope, "end").unwrap().into();
            let end_val = v8::Integer::new_from_unsigned(
                scope,
                u32::try_from(span.end_pos).unwrap_or(u32::MAX),
            )
            .into();
            obj.create_data_property(scope, end_key, end_val);
            let fg_key = v8::String::new(scope, "fg").unwrap().into();
            let fg_val = color_to_js(scope, span.style.fg);
            obj.create_data_property(scope, fg_key, fg_val);
            let bg_key = v8::String::new(scope, "bg").unwrap().into();
            let bg_val = color_to_js(scope, span.style.bg);
            obj.create_data_property(scope, bg_key, bg_val);
            obj.into()
        })
        .collect();
    v8::Array::new_with_elements(scope, elements.as_slice())
}

/// `line.styles`: the current in-flight line's spans as `[{ begin, end, fg, bg }]`.
/// Empty array when there is no current line. Current-session-only: it reads the
/// caller's in-flight line on this thread, so there is no target-session arg.
#[op2]
fn op_smudgy_get_current_line_styles<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
) -> v8::Local<'s, v8::Array> {
    let current_line = state.borrow::<Rc<RefCell<Weak<StyledLine>>>>().clone();
    if let Some(line) = Weak::upgrade(&current_line.borrow()) {
        return spans_to_js(scope, &line);
    }
    v8::Array::new(scope, 0)
}

/// Find a line by its UI line number in the recent-lines ring (`buffer.line(n)`). The ring is at most
/// `RECENT_LINES` entries, so this linear scan is bounded and cheap.
fn ring_line(state: &OpState, line_number: usize) -> Option<Arc<StyledLine>> {
    state
        .borrow::<crate::session::runtime::RecentLines>()
        .borrow()
        .iter()
        .find(|(n, _)| *n == line_number)
        .map(|(_, line)| line.clone())
}

/// `buffer.line(n).text`: the text of the emitted line `line_number`, or `undefined`
/// (null) when it is outside the recent-lines window. Current-session-only: reads this
/// session's own ring.
#[op2]
fn op_smudgy_buffer_get_text<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    line_number: u32,
) -> v8::Local<'s, v8::Value> {
    match ring_line(state, line_number as usize) {
        Some(line) => {
            v8::String::new(scope, &line.text).map_or_else(|| v8::null(scope).into(), Into::into)
        }
        None => v8::null(scope).into(),
    }
}

/// `buffer.line(n).styles`: the styles array for emitted line `line_number`, or
/// `undefined` (null) when it is outside the recent-lines window.
#[op2]
fn op_smudgy_buffer_get_styles<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &mut OpState,
    line_number: u32,
) -> v8::Local<'s, v8::Value> {
    match ring_line(state, line_number as usize) {
        Some(line) => spans_to_js(scope, &line).into(),
        None => v8::null(scope).into(),
    }
}

#[op2(fast)]
fn op_smudgy_line_insert(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    line_number: u32,
    #[string] text: String,
    begin: u32,
    end: u32,
    fg_color: v8::Local<v8::Value>,
    bg_color: v8::Local<v8::Value>,
) -> Result<(), NotCapable> {
    // Editing an already-emitted buffer line also changes what the user sees.
    ensure(grants(state).change_display, "change-display")?;
    let style = match parse_style_from_js(
        scope,
        if fg_color.is_null_or_undefined() {
            None
        } else {
            Some(fg_color)
        },
        if bg_color.is_null_or_undefined() {
            None
        } else {
            Some(bg_color)
        },
    ) {
        Ok(style) => style,
        Err(_) => return Ok(()), // Silently fail on error
    };

    queue_own_action(
        state,
        RuntimeAction::PerformLineOperation {
            line_number: line_number as usize,
            operation: (LineOperation::Insert {
                str: Arc::new(text),
                begin: begin as usize,
                end: end as usize,
                style,
            }),
        },
    );
    Ok(())
}

#[op2(fast)]
fn op_smudgy_line_replace(
    state: &mut OpState,
    line_number: u32,
    #[string] text: String,
    begin: u32,
    end: u32,
) -> Result<(), NotCapable> {
    ensure(grants(state).change_display, "change-display")?;
    queue_own_action(
        state,
        RuntimeAction::PerformLineOperation {
            line_number: line_number as usize,
            operation: (LineOperation::Replace {
                str: Arc::new(text),
                begin: begin as usize,
                end: end as usize,
            }),
        },
    );
    Ok(())
}

#[op2(fast)]
fn op_smudgy_line_highlight(
    scope: &mut v8::PinScope,
    line_number: u32,
    state: &mut OpState,
    begin: u32,
    end: u32,
    fg_color: v8::Local<v8::Value>,
    bg_color: v8::Local<v8::Value>,
) -> Result<(), NotCapable> {
    ensure(grants(state).change_display, "change-display")?;
    // Parse the style
    let style = match parse_style_from_js(
        scope,
        if fg_color.is_null_or_undefined() {
            None
        } else {
            Some(fg_color)
        },
        if bg_color.is_null_or_undefined() {
            None
        } else {
            Some(bg_color)
        },
    ) {
        Ok(style) => style,
        Err(_) => return Ok(()), // Silently fail on error
    };

    queue_own_action(
        state,
        RuntimeAction::PerformLineOperation {
            line_number: line_number as usize,
            operation: (LineOperation::Highlight {
                begin: begin as usize,
                end: end as usize,
                style,
            }),
        },
    );
    Ok(())
}

#[op2(fast)]
fn op_smudgy_line_remove(
    state: &mut OpState,
    line_number: u32,
    begin: u32,
    end: u32,
) -> Result<(), NotCapable> {
    ensure(grants(state).change_display, "change-display")?;
    queue_own_action(
        state,
        RuntimeAction::PerformLineOperation {
            line_number: line_number as usize,
            operation: (LineOperation::Remove {
                begin: begin as usize,
                end: end as usize,
            }),
        },
    );
    Ok(())
}

#[allow(clippy::inline_always)]
#[op2]
fn op_smudgy_mapper_set_current_location(
    state: &mut OpState,
    #[serde] area_id: (u64, u64),
    room_number: Option<i32>,
) -> Result<(), NotCapable> {
    // Setting the mapper's current location is a map mutation (→ mapper-write).
    ensure(grants(state).mapper_write, "mapper-write")?;
    let area_id = AreaId(Uuid::from_u64_pair(area_id.0, area_id.1));
    // Mirror the location on the session thread so `getCurrentLocation` can read it back
    // (the UI marker the action fans out is otherwise write-only and not readable cross-thread).
    *state
        .borrow::<crate::session::runtime::CurrentLocation>()
        .borrow_mut() = Some((area_id, room_number));
    queue_own_action(
        state,
        RuntimeAction::SetCurrentLocation(area_id, room_number),
    );
    Ok(())
}

/// A mapper location as serialized to JS: the area id `u64` pair plus an optional room
/// number (`None` when the location names an area without a specific room).
type JsLocation = ((u64, u64), Option<i32>);

/// `getCurrentLocation()`: the session's last-set mapper location as `{ area, room }`, or
/// `undefined` when none has been set. A CURRENT-session read: the value lives in
/// this session's own shared cell (mirrored from `setCurrentLocation`), not in the `Mapper`
/// cache, so it is never addressable cross-session. Gated on `mapper-read`. `area` is the
/// `[u64, u64]` id pair (the same shape every other mapper op uses); `room` is `null` when the
/// location names an area without a specific room.
#[op2]
#[serde]
fn op_smudgy_mapper_get_current_location(
    state: &OpState,
) -> Result<Option<JsLocation>, NotCapable> {
    ensure(grants(state).mapper_read, "mapper-read")?;
    Ok(state
        .borrow::<crate::session::runtime::CurrentLocation>()
        .borrow()
        .map(|(area_id, room)| (area_id.0.as_u64_pair(), room)))
}

#[op2(fast)]
fn op_smudgy_capture(state: &mut OpState, value: bool) {
    let captured = state.borrow_mut::<Capture>();
    captured.0 = value;
}

#[op2(fast)]
fn op_smudgy_fallthrough(state: &mut OpState, value: bool) -> Result<(), FallthroughContextError> {
    let fallthrough = state.borrow_mut::<Fallthrough>();
    let Some(current) = fallthrough.0.as_mut() else {
        return Err(FallthroughContextError);
    };
    *current = value;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AnsiColor, Color, ECHO_DEFAULT_STYLE, IsolateId, LinkContext, SessionId,
        canonical_procedure, fold_name, packed_echo_lines, packed_validate, param_read_allowed,
    };
    use crate::session::runtime::store::ProducerKey;
    use crate::session::styled_line::LinkAction;
    use std::sync::Arc;

    // ---- Packed styled-echo payload ----------------------------------------------

    /// Colors as the JS encoder packs them (see `__styled_encode_color`).
    const RGB_123: u32 = 0x0101_0203; // rgb(1, 2, 3)
    const ANSI_RED_BRIGHT: u32 = 0x0200_0009;
    const ROLE_DEFAULT: u32 = 0x0300_0000;
    const LINK_SEND_0: u32 = 0x4000_0000;
    const LINK_CB_0: u32 = 0x8000_0000;

    fn link_ctx(callback_ids: Vec<u64>) -> LinkContext {
        LinkContext {
            session: SessionId::from(1u32),
            isolate_token: Arc::from("test-isolate"),
            callback_ids,
        }
    }

    /// Validate-then-build, the exact sequence the ops run.
    fn decode(
        text: &str,
        records: &[u32],
        callback_count: u32,
        callback_ids: Vec<u64>,
    ) -> Result<Vec<Arc<crate::session::styled_line::StyledLine>>, super::StyledTextOpError> {
        packed_validate(text, records, callback_count)?;
        packed_echo_lines(text, records, &link_ctx(callback_ids), ECHO_DEFAULT_STYLE)
    }

    #[test]
    fn packed_two_lines_roundtrip() {
        // Line 1: "Hi" in rgb(1,2,3) + "!" unset; line 2: "ok" bright red on a
        // "default" background (which must normalize to DefaultBackground).
        let records = [
            2,
            0, // 2 lines, 0 sends
            2,
            2,
            RGB_123,
            0,
            0,
            1,
            0,
            0,
            0, // line 1: 2 runs
            1,
            2,
            ANSI_RED_BRIGHT,
            ROLE_DEFAULT,
            0, // line 2: 1 run
        ];
        let lines = decode("Hi!ok", &records, 0, Vec::new()).expect("valid payload");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Hi!");
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].style.fg, Color::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(lines[0].spans[0].style.bg, Color::DefaultBackground);
        assert_eq!(
            (lines[0].spans[0].begin_pos, lines[0].spans[0].end_pos),
            (0, 2)
        );
        assert_eq!(lines[0].spans[1].style.fg, Color::Echo);
        assert_eq!(
            (lines[0].spans[1].begin_pos, lines[0].spans[1].end_pos),
            (2, 3)
        );
        assert_eq!(lines[1].text, "ok");
        assert_eq!(
            lines[1].spans[0].style.fg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: true
            }
        );
        assert_eq!(lines[1].spans[0].style.bg, Color::DefaultBackground);
    }

    #[test]
    fn packed_empty_fragment_is_one_empty_line() {
        let lines = decode("", &[1, 0, 0], 0, Vec::new()).expect("valid payload");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "");
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].style, ECHO_DEFAULT_STYLE);
    }

    #[test]
    fn packed_links_resolve_through_send_table_and_callback_ids() {
        // Send strings ride at the FRONT of the text with lengths at the TAIL of
        // the records; callbacks resolve through the registered id list.
        let records = [
            1,
            1, // 1 line, 1 send
            2,
            4,
            0,
            0,
            LINK_SEND_0,
            2,
            0,
            0,
            LINK_CB_0, // "go n" send-linked, "ok" cb-linked
            5,         // send "north" is 5 units
        ];
        let lines = decode("northgo nok", &records, 1, vec![42]).expect("valid payload");
        assert_eq!(lines[0].text, "go nok");
        assert_eq!(lines[0].links.len(), 2);
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::Send(Arc::from("north"))
        );
        assert_eq!(
            (lines[0].links[0].begin_pos, lines[0].links[0].end_pos),
            (0, 4)
        );
        let LinkAction::Callback { id, .. } = &lines[0].links[1].action else {
            panic!("second link must be a callback");
        };
        assert_eq!(*id, 42);
    }

    #[test]
    fn packed_non_bmp_text_maps_utf16_units_to_byte_offsets() {
        // "\u{1F600}!" is 3 UTF-16 units and 5 UTF-8 bytes; the differently-colored
        // "?" pins the span boundary at the byte (not unit) offset.
        let records = [1, 0, 2, 3, 0, 0, 0, 1, RGB_123, 0, 0];
        let lines = decode("\u{1F600}!?", &records, 0, Vec::new()).expect("valid payload");
        assert_eq!(lines[0].text, "\u{1F600}!?");
        assert_eq!(
            (lines[0].spans[0].begin_pos, lines[0].spans[0].end_pos),
            (0, 5)
        );
        assert_eq!(
            (lines[0].spans[1].begin_pos, lines[0].spans[1].end_pos),
            (5, 6)
        );
    }

    #[test]
    fn packed_control_characters_are_stripped_not_fatal() {
        // "a\rb" is dirty data: the \r is stripped, the run still lands.
        let lines = decode("a\rb", &[1, 0, 1, 3, 0, 0, 0], 0, Vec::new()).expect("valid");
        assert_eq!(lines[0].text, "ab");
    }

    #[test]
    fn packed_validate_rejects_malformed_payloads() {
        let cases: &[(&str, &str, Vec<u32>, u32)] = &[
            ("newline in a run", "a\nb", vec![1, 0, 1, 3, 0, 0, 0], 0),
            (
                "unknown color tag",
                "x",
                vec![1, 0, 1, 1, 0x0400_0000, 0, 0],
                0,
            ),
            (
                "unknown role color",
                "x",
                vec![1, 0, 1, 1, 0x0300_0004, 0, 0],
                0,
            ),
            (
                "unknown ansi index",
                "x",
                vec![1, 0, 1, 1, 0x0200_0010, 0, 0],
                0,
            ),
            (
                "unknown link tag",
                "x",
                vec![1, 0, 1, 1, 0, 0, 0xC000_0000],
                0,
            ),
            ("nonzero link with tag 0", "x", vec![1, 0, 1, 1, 0, 0, 1], 0),
            (
                "callback index out of range",
                "x",
                vec![1, 0, 1, 1, 0, 0, LINK_CB_0],
                0,
            ),
            (
                "send index out of range",
                "x",
                vec![1, 0, 1, 1, 0, 0, LINK_SEND_0],
                0,
            ),
            ("truncated records", "x", vec![1, 0, 1, 1, 0], 0),
            (
                "text shorter than records",
                "x",
                vec![1, 0, 1, 2, 0, 0, 0],
                0,
            ),
            ("trailing text", "xy", vec![1, 0, 1, 1, 0, 0, 0], 0),
            ("trailing records", "x", vec![1, 0, 1, 1, 0, 0, 0, 0], 0),
            ("send count past table", "x", vec![1, 9, 1, 1, 0, 0, 0], 0),
            ("surrogate split", "\u{1F600}", vec![1, 0, 1, 1, 0, 0, 0], 0),
        ];
        for (what, text, records, callback_count) in cases {
            assert!(
                packed_validate(text, records, *callback_count).is_err(),
                "expected {what} to be rejected"
            );
        }
    }

    fn package(owner: &str, name: &str) -> IsolateId {
        IsolateId::Package {
            owner: Arc::from(owner),
            name: Arc::from(name),
            version: Arc::from("1.0.0"),
        }
    }

    #[test]
    fn package_isolate_reads_only_its_own_param_namespace() {
        let me = package("wbk", "mapper");
        // Its own namespace — the string the `smudgy:params` binding bakes in for it.
        assert!(param_read_allowed(&me, "smudgy://wbk/mapper"));
        // Another package on the same server: the cross-package secret-read hole this gate closes.
        assert!(!param_read_allowed(&me, "smudgy://cor/combat"));
        // Same owner, different package is still a different namespace.
        assert!(!param_read_allowed(&me, "smudgy://wbk/other"));
        // The resolved version never appears in a param specifier; a spoofed one is rejected.
        assert!(!param_read_allowed(&me, "smudgy://wbk/mapper@1.0.0"));
    }

    #[test]
    fn main_isolate_is_trusted_for_any_namespace() {
        assert!(param_read_allowed(
            &IsolateId::Main,
            "smudgy://anyone/anything"
        ));
        assert!(param_read_allowed(&IsolateId::Main, "smudgy://wbk/mapper"));
    }

    #[test]
    fn fold_name_borrows_lowercase_and_owns_mixed_case() {
        // The common case — an already-lowercase name — folds with no allocation.
        assert!(matches!(
            fold_name("smudgy://kapusniak/arctic-prompt#prompt"),
            std::borrow::Cow::Borrowed(_)
        ));
        let folded = fold_name("user#Ping");
        assert!(matches!(folded, std::borrow::Cow::Owned(_)));
        assert_eq!(folded, "user#ping");
    }

    #[test]
    fn canonical_procedure_is_the_folded_stamp() {
        let producer = ProducerKey::parse("smudgy://wbk/tracker").unwrap();
        assert_eq!(
            canonical_procedure(&producer, "Refresh"),
            "smudgy://wbk/tracker#refresh"
        );
    }
}
