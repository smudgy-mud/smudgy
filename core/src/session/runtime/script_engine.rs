use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs,
    rc::Rc,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    time::Instant,
};

use rustc_hash::FxHashSet;

use deno_core::{
    error::{CoreError, CoreErrorKind, JsError, JsStackFrame},
    v8::{self, Global, script_compiler::Source},
};

use derive_more::{Display, Into};
use futures::channel::mpsc::Sender;
use smudgy_cloud::{Mapper, PackageApiClient};
use smudgy_script::{
    ImportPolicy, InspectorConfig, LoadReport, LoadedModuleKind, ModulePolicy, ModuleSet,
    PackagePermissions, PackageProvider, Permissions, PermissionsContainer, PermissionsOptions,
    ScriptRuntime, ScriptRuntimeOptions, SmudgySpecifier, parse_canonical,
    permission_descriptor_parser,
};

use crate::{
    get_smudgy_home,
    session::{
        BufferUpdate, PackageProviderFactory, ScriptExtensionFactory, SessionEvent, SessionId,
        TaggedSessionEvent,
        runtime::{
            ActionResult, IsolateId, SingletonRegistry,
            line_operation::LineOperation,
            script_engine::ops::{Capture, Fallthrough},
            trigger::{Manager, MatchCapture, SharedAutomationRegistry},
        },
        styled_line::StyledLine,
    },
};

use anyhow::{Result, anyhow, bail};
use deno_core::url::Url;

mod mapper_api;
mod ops;
mod package_cache;
mod package_provider;
mod package_solver;

use package_provider::build_package_provider;

/// Bind a v8 inspector in dev builds, or whenever `SMUDGY_SCRIPT_INSPECTOR` is set,
/// so the bundled `smudgy_inspector` helper (or any CDP client) can attach. The
/// inspector can only be created at runtime construction; it merely listens until a
/// client attaches, so an idle inspector costs only a socket (the V8 deopt cost is
/// paid only once a debugger actually attaches). Port 0 = OS-assigned (the bound
/// address is logged + recorded in the registry for the UI to spawn the helper).
///
/// Enabled when any of:
/// - the `advanced_scripting_features` setting is on — the user's explicit,
///   persisted opt-in. Its docs promise it "unlocks ... the script inspector" and
///   the UI gates the Inspect button on the same flag, so honoring it here is what
///   makes Inspect work in shipped **release / release-candidate** builds (not just
///   dev). This setting was previously ignored here, so the button showed but did
///   nothing in an RC/release DMG — the bug this closes.
/// - this is a [`Dev`](crate::models::settings::BuildChannel::Dev) build (always on
///   for local iteration, regardless of the setting).
/// - the `SMUDGY_SCRIPT_INSPECTOR` env override is present (CI / headless).
///
/// Otherwise `None`: a plain build with advanced features off opens no port.
fn inspector_config() -> Option<InspectorConfig> {
    // Ordered cheapest-first: the const dev-build check and the env probe short-circuit
    // before `load_settings()` touches disk (it's called once per runtime construction).
    if crate::models::settings::is_dev_build()
        || std::env::var_os("SMUDGY_SCRIPT_INSPECTOR").is_some()
        || crate::models::settings::load_settings().advanced_scripting_features
    {
        Some(InspectorConfig {
            address: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
        })
    } else {
        None
    }
}

#[derive(Display, Debug, Clone, Copy, PartialEq, Eq, Hash, Into)]
pub struct ScriptId(usize);

#[derive(Display, Debug, Clone, Copy, PartialEq, Eq, Hash, Into)]
pub struct FunctionId(pub(crate) usize);

#[cfg(feature = "bench-api")]
impl FunctionId {
    /// Mints a handler id from a raw index so the `smudgy_bench` crate can register store
    /// watchers with no live script engine behind them: watch delivery only *carries* the id
    /// (queued inside `RuntimeAction::CallJavascriptFunction`); nothing dereferences it until
    /// an engine dispatches the action.
    #[must_use]
    pub const fn from_raw(id: usize) -> Self {
        Self(id)
    }
}

pub struct ScriptEngineParams<'a> {
    pub session_id: SessionId,
    pub server_name: &'a Arc<String>,
    pub ui_tx: Sender<TaggedSessionEvent>,
    pub spawned_actions: super::ActionQueue,
    pub pending_line_operations: &'a Rc<RefCell<Vec<LineOperation>>>,
    pub emitted_line_count: std::rc::Weak<Cell<usize>>,
    /// Ring of recently-emitted lines, shared into every isolate's read ops so
    /// `buffer.line(n)` can read text/styles for the last `RECENT_LINES` lines.
    pub recent_lines: super::RecentLines,
    /// Current mapper location, shared into every isolate's `getCurrentLocation` read op.
    pub current_location: super::CurrentLocation,
    /// Script-visible settings snapshot, shared into every isolate's `getSettings()` read op.
    pub settings_snapshot: super::SettingsSnapshot,
    /// The session's pane registry, shared into every isolate's pane ops (mutated
    /// synchronously in the op; preserved across reloads by the runtime).
    pub pane_registry: super::SharedPaneRegistry,
    /// Per-line routing state (gag/redirect/copy), shared into every isolate's ops beside
    /// `pending_line_operations`.
    pub line_routing: super::SharedLineRouting,
    /// The input mirror (`docs/input.md` §3.3), shared into every isolate's input
    /// read ops. Written by the runtime's `InputStateChanged` dispatch arm; session-scoped.
    pub input_mirror: super::SharedInputMirror,
    /// The in-flight typed submission (`docs/input.md` §3.5), shared into every
    /// isolate's submission ops. Installed/consumed by the runtime's `SubmitInput`/
    /// `CompleteInputSubmission` dispatch arms; the ambient `submission` acts on it.
    pub input_submission: super::SharedInputSubmission,
    /// The completion word sets (`docs/input.md` §3.8), shared into every
    /// isolate's registry ops (synchronous mutation + exact reads). The runtime's
    /// `InputWordSetsChanged` dispatch arm builds the UI's merged view from the same cell.
    pub input_word_sets: super::SharedInputWordSets,
    /// The pane-input `onSubmit` registry (`docs/input.md` §3.7), shared into
    /// every isolate's pane ops (the registration op writes it). The runtime's
    /// `PaneInputSubmit` dispatch arm resolves through the same cell; reset by the
    /// runtime before each rebuild (handler addresses are engine facts).
    pub pane_input_callbacks: super::SharedPaneInputCallbacks,
    /// The session store, shared into every isolate's store ops (writes journal there; the
    /// runtime flushes per turn). Outlives this engine — reloads keep the committed tree.
    pub session_store: super::SharedSessionStore,
    /// The message bus (`docs/interop.md` §6), shared into every isolate's message
    /// ops. Outlives this engine — pending posts survive a reload (queue-briefly); receivers
    /// are reset by the runtime before each rebuild.
    pub message_bus: super::SharedMessageBus,
    /// The runtime catalogue (`docs/interop.md` §10), shared into every isolate's
    /// ops; this engine registers its statically-extracted handle declarations into it at
    /// construction. Outlives the engine — samples are session history.
    pub catalogue: super::SharedCatalogue,
    /// The GMCP enabled flag (`docs/gmcp.md` §3.4), shared into every isolate's
    /// `gmcp.enabled` read op. Written by the runtime's GMCP producer; session-scoped.
    pub gmcp_enabled: super::gmcp::SharedGmcpEnabled,
    pub mapper: Option<Mapper>,
    /// Cloud client for `smudgy://` package resolution. `None` disables `smudgy://`
    /// imports for this session (e.g. when logged out / no backend).
    pub package_client: Option<PackageApiClient>,
    /// Optional alternate package resolver, built on the session thread. When `Some`, the
    /// engine resolves `smudgy://` imports through it instead of the cloud-backed provider
    /// built from `package_client` (the sandboxed-isolate tests inject an in-memory
    /// provider here). The session-wide solve / auto-update notices are cloud-provider-only,
    /// so they are skipped when an override is supplied.
    pub package_provider_override: Option<PackageProviderFactory>,
    pub extra_script_extensions: ScriptExtensionFactory,
    pub tokio_runtime: Rc<tokio::runtime::Runtime>,
    /// Introspection mirror shared with the trigger `Manager`. Seeded into every isolate's
    /// `OpState` so the `get`/`list`/`exists` ops read the live automation set without crossing
    /// into the (non-`OpState`) `Manager`.
    pub automation_registry: SharedAutomationRegistry,
}

/// Shared "which isolates have a pending wakeup" set for the per-isolate waker demux
/// (`EVENT-LOOP-READINESS-DEMUX.md`). `Send + Sync` because `std::task::Wake` requires it to
/// build a `Waker` at all (not because off-thread wakes are observed — on this stack the
/// `DemuxWaker` fires on the session thread). Holds only `IsolateId`s; no v8 handle ever crosses.
type ReadySet = Arc<Mutex<FxHashSet<IsolateId>>>;

/// The session task's latest waker, stored so a per-isolate `DemuxWaker` can re-arm the task.
type ParentSlot = Arc<Mutex<Option<Waker>>>;

/// One per isolate, built at insert and held for the isolate's whole life. When the owning
/// isolate's pending op/timer completes, `deno_core` invokes this; it records the isolate's id into
/// the engine `ready` set and re-arms the session task, so the next pump polls ONLY this isolate.
/// Captures no `Isolate`/v8 state (it may fire after a pump returns) — only `Arc` handles.
struct DemuxWaker {
    id: IsolateId,
    ready: ReadySet,
    parent: ParentSlot,
}

impl Wake for DemuxWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // (i) Record which isolate woke. Dedup is free (`HashSet`): several ops on one isolate
        // completing before the next pump collapse to a single entry.
        self.ready
            .lock()
            .expect("ready-set poisoned")
            .insert(self.id.clone());
        // (ii) Re-arm the session task. Clone-then-drop-the-lock-then-wake so the parent lock is
        // never held across `Waker::wake`. If `parent` is `None`, no task is parked on us; the id
        // is already in `ready`, so the next pump (which re-stores `parent` first) catches it.
        let parent = self.parent.lock().expect("parent slot poisoned").clone();
        if let Some(parent) = parent {
            parent.wake();
        }
    }
}

/// Build an isolate's `DemuxWaker`, wired to the engine's shared `ready`/`parent` handles.
fn build_demux_waker(id: IsolateId, ready: &ReadySet, parent: &ParentSlot) -> Waker {
    Waker::from(Arc::new(DemuxWaker {
        id,
        ready: ready.clone(),
        parent: parent.clone(),
    }))
}

/// Allocator for [`Isolate::instance`] nonces. Process-wide (not per-engine, not per-session)
/// so no two isolate instantiations ever share a nonce, however engines are rebuilt or
/// sessions interleave. Starts at 1: 0 is [`super::origin::NO_ISOLATE_INSTANCE`], the inert
/// value a malformed widget token parses to, and must never be allocated.
static NEXT_ISOLATE_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// One V8 isolate and everything bound to it. `v8::Global` handles are isolate-bound, so
/// the registries that hold them (`script_functions`, `compiled_scripts`) live *here*, one
/// set per isolate, and never leave (`PACKAGE-ISOLATES-ENGINE.md`). `script_functions`
/// is the same `Rc` this isolate's `smudgy_ops` extension holds, so the creation ops push
/// into the right registry.
///
/// Each sandboxed package isolate is a full `Isolate` bundle of its own — its own
/// `ScriptRuntime` (own `MainWorker` + loader), its own function/script registries, and its
/// own restricted `PermissionsContainer` + `SmudgyGrants`. The isolation boundary is the
/// separate heap + registries.
struct Isolate {
    runtime: ScriptRuntime,
    /// This instantiation's nonce (from [`NEXT_ISOLATE_INSTANCE`]). An `IsolateId` names a
    /// *role* that an engine reload recreates; this names the exact heap. Widget callbacks
    /// carry it in their routing token, and [`ScriptEngine::execute_javascript_function`]
    /// rejects a mismatch — the callback's `v8::Global` is bound to a disposed predecessor,
    /// and materializing it would abort the session thread.
    instance: u64,
    script_functions: Rc<RefCell<Vec<v8::Global<v8::Function>>>>,
    compiled_scripts: Vec<v8::Global<v8::Script>>,
    /// This isolate's `DemuxWaker`, built once at insert (`EVENT-LOOP-READINESS-DEMUX.md`).
    /// `poll_event_loop` polls the isolate through a `Context` built from this, so `deno_core`
    /// registers it against the isolate's pending ops — a completion then re-enters only this
    /// isolate. `Send + Sync` itself, which is fine on an otherwise non-`Send` `Isolate`; drops
    /// with the isolate.
    waker: Waker,
}

/// The auto-load set partitioned by target isolate (`build_isolate_plan`): the main isolate's
/// modules (local `modules/` + trusted packages) and one specifier per installed-untrusted
/// package, each of which gets its own sandboxed isolate.
struct IsolatePlan {
    main: ModuleSet,
    sandboxed: Vec<String>,
    /// Each installed package's interop **home** (`docs/interop.md` §3), derived
    /// from the same lockfile partition as the module sets: trusted → main, untrusted → its
    /// own sandbox. Built *before* any module evaluates so top-level store writes already
    /// pass the home gate; a package absent here (uninstalled — e.g. a copy embedded in
    /// another package's closure) is home nowhere and cannot write.
    homes: HashMap<(String, String), crate::session::runtime::store::HomeIsolate>,
    /// The installed packages' typing materialization — carried out of the plan so the same
    /// static extraction that fed the typings also registers the catalogue's tier-1 declared
    /// index (interop.md §10) without a second parse.
    installed_typings: Vec<crate::models::script_typings::InstalledPackageTypes>,
}

pub struct ScriptEngine<'a> {
    // Foundational session context retained on the engine (e.g. for emitting notices /
    // mapper access). These same values are also wired into the ops extensions at construction.
    #[allow(dead_code)]
    session_id: SessionId,
    /// The session's isolate set. `main` is always present (trusted, allow-all); each
    /// installed-untrusted package gets its own sandboxed entry. The host pumps the event loop of each
    /// isolate the demux queued into `ready` (seeded on insert and after synchronous JS runs on
    /// it; re-queued by its `DemuxWaker` when an op/timer completes), and routes each v8 call
    /// into the owning isolate.
    isolates: HashMap<IsolateId, Isolate>,
    /// The session-global event bus (`PACKAGE-EVENTS.md`), kept so the host can deliver `sys:`/`map:`
    /// events to subscribers (the same `Rc` cloned into every isolate's ops).
    event_registry: ops::EventRegistry,
    /// The runtime catalogue (`docs/interop.md` §10), kept so [`Self::host_emit`]
    /// can sample platform events at its choke point (the same `Rc` cloned into every
    /// isolate's ops, where package emits/posts sample).
    catalogue: crate::session::runtime::SharedCatalogue,
    /// Interned catalogue key strings for the platform (`sys:`/`map:`) events
    /// [`Self::host_emit`] samples, keyed by the full stamped name (`"sys:receive"`).
    /// The platform producer/name set is closed — bounded by host call sites — so the
    /// table is a fixed-size cache resolved on first emission, and a hit samples with
    /// refcount bumps only (the per-line `sys:receive` path allocates no key strings).
    platform_event_keys: RefCell<HashMap<String, PlatformEventKeys>>,
    current_line: Rc<RefCell<Weak<StyledLine>>>,
    /// The current-line staleness scope (see [`ops::LineScope`]): `current` is bumped by
    /// [`Self::set_current_line`] per installed line; `armed` is set/restored by the
    /// user-JS entry points below around every synchronous entry, and checked by the
    /// ambient `line` mutators. One cell shared into every isolate's ops.
    line_scope: ops::LineScopeCell,
    #[allow(dead_code)]
    pending_line_operations: &'a Rc<RefCell<Vec<LineOperation>>>,
    #[allow(dead_code)]
    server_name: &'a Arc<String>,
    #[allow(dead_code)]
    ui_tx: Sender<TaggedSessionEvent>,
    #[allow(dead_code)]
    mapper: Option<Mapper>,
    /// Per-isolate waker demux (`EVENT-LOOP-READINESS-DEMUX.md`): ids of isolates whose
    /// `DemuxWaker` has fired (a completed op/timer) or that were seeded (on insert, or after
    /// synchronous dispatch) since the last pump. `poll_event_loop` drains this and polls ONLY
    /// these isolates, so one op completion re-enters O(1) isolates, not O(N).
    ready: ReadySet,
    /// Latest session-task waker, refreshed every pump; the per-isolate `DemuxWaker`s re-arm the
    /// session task through this slot when their isolate makes progress.
    parent: ParentSlot,
}

/// The shared (`Arc`) catalogue key strings for one platform event, resolved once on its
/// first emission (see `ScriptEngine::platform_event_keys`): the folded producer key, the
/// name pair `sample_interned` takes, and the host-stamped sender (the producer's original
/// spelling).
struct PlatformEventKeys {
    producer: Arc<str>,
    name: Arc<str>,
    name_folded: Arc<str>,
    sender: Arc<str>,
}

impl Drop for ScriptEngine<'_> {
    fn drop(&mut self) {
        // Each isolate is left "exited" between operations (Model B), but rusty_v8's
        // `OwnedIsolate::Drop` still does real v8 teardown + an `exit()` that require the isolate
        // to be the thread's *current* one. So enter each isolate immediately before dropping it;
        // order is then irrelevant (validated for deno_core 0.395 / v8 147). A plain drop of the
        // map would tear an isolate down while another was current → the misleading "Cannot
        // create a handle without a HandleScope" abort. Do NOT `exit` here — `OwnedIsolate::Drop`
        // performs the single matching exit.
        for (_id, mut isolate) in self.isolates.drain() {
            // SAFETY: enter makes this isolate current for its own drop, which exits exactly
            // once. The other isolates are exited (off the enter-stack), so order doesn't matter.
            unsafe {
                isolate.runtime.deno_runtime().v8_isolate().enter();
            }
            drop(isolate);
        }
        info!("Dropping script engine");
    }
}

/// Materializes each installed package's typings into
/// `<server>/.smudgy/packages/<owner>/<name>/` so the editor can type
/// `import … from "smudgy://owner/name"`, returning the list for the tsconfig `paths` map
/// and the interop-handle typings generator.
///
/// Two artifact sets per package: its shipped `.d.ts` files, and its entry **source**
/// (interop.md §5) — a `.d.ts` carries no initializers, so the handle name
/// literals only exist in source, and pointing the specifier at the source makes
/// jump-to-definition land in the producer's real declarations. When the entry source
/// materializes, its `.d.ts` twin is skipped (two same-basename module files would race
/// for the same import resolution); packages that ship only declarations fall back to the
/// entry `.d.ts` with no handle typings.
///
/// Best-effort and pure-disk: reads the on-disk package cache, and runs once per session in
/// [`ScriptEngine::build_isolate_plan`] **before** any isolate forking — so there is no
/// cross-isolate write race. A package whose files aren't cached yet (never resolved on
/// this machine) is skipped until a later session.
///
/// LOCAL DEV-OVERRIDE: a lock entry under the account's own owner segment whose authored
/// folder exists at `<server>/packages/<name>/` is typed from that **live source** — the
/// same shadowing the resolver applies (`PackageProvider::try_local_override`) — never
/// from the cached published copy, which is the *previous* version of the package. Nothing
/// is materialized for it (the tsconfig points straight at the folder), so payload types
/// track the author's edits without a session restart.
///
/// FRESHNESS: because this reads the lock + cache at session start, a package installed
/// *mid-session* is only typed on the next session start; there is no immediate
/// editor-type refresh on install/uninstall. (Handle *names* added to a local package
/// also appear at the next session start; their payload types are live, per above.)
fn materialize_installed_typings(
    server_name: &str,
) -> Vec<crate::models::script_typings::InstalledPackageTypes> {
    let Ok(home) = get_smudgy_home() else {
        return Vec::new();
    };
    let packages_root = home.join(server_name).join(".smudgy").join("packages");
    let Ok(cache) = package_cache::PackageCache::new() else {
        return Vec::new();
    };
    let lock = match crate::models::shared_packages::load_lock(server_name) {
        Ok(lock) => lock,
        Err(e) => {
            warn!("script typings: load lock for {server_name}: {e:#}");
            return Vec::new();
        }
    };
    // The owner segment local packages run under (the resolver's `is_local_owner_segment`):
    // the account nickname when signed in, plus the reserved `local` placeholder always.
    let account_nickname = crate::models::auth::load_account().and_then(|a| a.nickname);

    let mut materialized = Vec::new();
    let mut keep: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();

    for pkg in &lock.packages {
        if !pkg.enabled {
            continue;
        }
        let Ok(spec) = smudgy_script::SmudgySpecifier::parse(&pkg.specifier) else {
            continue;
        };

        // Local dev-override first, before any version/cache gate: a local entry may have
        // no `last_resolved_version` at all (authored, never published) and must still be
        // typed. When the folder loads, it shadows the install unconditionally; when it is
        // missing or unreadable, fall through to the cached install (resolver parity).
        if (spec.owner == crate::models::local_packages::LOCAL_OWNER
            || account_nickname.as_deref() == Some(spec.owner.as_str()))
            && let Ok(Some(local)) =
                crate::models::local_packages::load_local_package(server_name, &spec.name)
        {
            if let Some(types) = local_package_types(&spec.owner, &local) {
                materialized.push(types);
            }
            continue;
        }

        let Some(version) = pkg.last_resolved_version.as_deref() else {
            continue;
        };
        let Some(meta) = cache.read_meta(&spec.package_key(), version) else {
            continue;
        };

        let entry = meta.manifest.entry.as_deref().unwrap_or("index.ts");
        let module_subpaths: Vec<&str> = meta.modules.iter().map(|m| m.subpath.as_str()).collect();
        let entry_source = entry_source_subpath(entry, &module_subpaths);
        let entry_dts = entry_dts_subpath(entry);

        let pkg_dir = packages_root.join(&spec.owner).join(&spec.name);
        let mut wrote_any = false;
        let mut entry_text: Option<String> = None;
        for module in &meta.modules {
            let is_entry_source = entry_source.as_deref() == Some(module.subpath.as_str());
            // Ship .d.ts files, plus the entry source; when the source materializes, skip
            // its declaration twin (same basename, one resolution winner).
            if !is_entry_source
                && (!is_dts_subpath(&module.subpath)
                    || (entry_source.is_some() && module.subpath == entry_dts))
            {
                continue;
            }
            let Some(text) = cache.read_blob(&module.content_hash) else {
                continue;
            };
            let path = pkg_dir.join(&module.subpath);
            if path.parent().is_some_and(|p| fs::create_dir_all(p).is_ok())
                && fs::write(&path, &text).is_ok()
            {
                wrote_any = true;
                if is_entry_source {
                    entry_text = Some(text);
                }
            }
        }
        if !wrote_any {
            continue;
        }
        keep.insert(pkg_dir.clone());

        // The specifier resolves to the entry source when it materialized (handle name
        // literals + typeof aliases live there), else the entry `.d.ts`.
        let (entry_module, handles) = match (entry_source, entry_text) {
            (Some(source), Some(text)) => {
                let handles = extract_entry_handles(&spec.owner, &spec.name, &source, &text);
                (source, handles)
            }
            _ => (entry_dts, Vec::new()),
        };
        if pkg_dir.join(&entry_module).is_file() {
            materialized.push(crate::models::script_typings::InstalledPackageTypes {
                owner: spec.owner,
                name: spec.name,
                entry_module,
                handles,
                local: false,
            });
        }
    }

    prune_orphan_typings(&packages_root, &keep);
    materialized
}

/// The [`InstalledPackageTypes`](crate::models::script_typings::InstalledPackageTypes) for
/// a local dev-override package, typed from the live folder's files: handles are extracted
/// from the authored entry source, and `local: true` points the generated paths at the
/// folder itself. `None` when the folder ships no resolvable entry module (nothing for the
/// editor to type — matching the materialized path's is-file gate).
fn local_package_types(
    owner: &str,
    local: &crate::models::local_packages::LocalPackage,
) -> Option<crate::models::script_typings::InstalledPackageTypes> {
    let entry = local.manifest.entry.as_deref().unwrap_or("index.ts");
    let subpaths: Vec<&str> = local.modules.iter().map(|m| m.subpath.as_str()).collect();
    let (entry_module, handles) = match entry_source_subpath(entry, &subpaths) {
        Some(source) => {
            let handles = local
                .modules
                .iter()
                .find(|m| m.subpath == source)
                .and_then(|m| std::str::from_utf8(&m.content).ok())
                .map(|text| extract_entry_handles(owner, &local.name, &source, text))
                .unwrap_or_default();
            (source, handles)
        }
        // Declarations-only folder: type the entry `.d.ts` with no handles, like the
        // materialized path.
        None => (entry_dts_subpath(entry), Vec::new()),
    };
    subpaths.contains(&entry_module.as_str()).then(|| {
        crate::models::script_typings::InstalledPackageTypes {
            owner: owner.to_string(),
            name: local.name.clone(),
            entry_module,
            handles,
            local: true,
        }
    })
}

/// Statically extract a materialized entry source's interop handle declarations for the
/// typings generator. Best-effort: a parse failure types the package with no handles (the
/// runtime scheme loader reports the same failure loudly at import).
fn extract_entry_handles(
    owner: &str,
    name: &str,
    subpath: &str,
    text: &str,
) -> Vec<smudgy_script::interop_extract::InteropHandle> {
    let Ok(url) =
        deno_core::ModuleSpecifier::parse(&format!("smudgy-pkg:///{owner}/{name}/0.0.0/{subpath}"))
    else {
        return Vec::new();
    };
    match smudgy_script::interop_extract::extract_interop_handles(&url, text) {
        Ok(extraction) => {
            if !extraction.duplicates.is_empty() {
                warn!(
                    "script typings: package {owner}/{name} declares duplicate interop handle name(s): {}",
                    extraction.duplicates.join(", ")
                );
            }
            for diagnostic in &extraction.export_diagnostics {
                warn!("script typings: package {owner}/{name}: {diagnostic}");
            }
            extraction.handles
        }
        Err(e) => {
            warn!("script typings: parse {owner}/{name} entry for handles: {e}");
            Vec::new()
        }
    }
}

/// The entry module's *source* file among the package's module subpaths: the manifest
/// entry when present verbatim, else the loader's own entry candidates (`index.*` /
/// `mod.ts`). `None` when the package ships no entry source (declarations-only).
fn entry_source_subpath(entry: &str, subpaths: &[&str]) -> Option<String> {
    let has = |subpath: &str| subpaths.contains(&subpath);
    if !is_dts_subpath(entry) && has(entry) {
        return Some(entry.to_string());
    }
    for candidate in ["index.ts", "index.tsx", "index.js", "index.jsx", "mod.ts"] {
        if has(candidate) {
            return Some((*candidate).to_string());
        }
    }
    None
}

/// Whether `subpath` is a TypeScript declaration file (`*.d.ts`).
fn is_dts_subpath(subpath: &str) -> bool {
    let path = std::path::Path::new(subpath);
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ts"))
        && path
            .file_stem()
            .map(std::path::Path::new)
            .and_then(|stem| stem.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("d"))
}

/// The declaration file for an entry module: `index.ts` → `index.d.ts` (preserving any
/// subdirectory), forward-slashed for the tsconfig `paths` entry.
fn entry_dts_subpath(entry: &str) -> String {
    let path = std::path::Path::new(entry);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(entry);
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => format!("{}/{stem}.d.ts", dir.to_string_lossy().replace('\\', "/")),
        None => format!("{stem}.d.ts"),
    }
}

/// Removes materialized `<packages_root>/<owner>/<name>` directories not in `keep` (a
/// package that was uninstalled, disabled, or whose `.d.ts` are no longer cached).
fn prune_orphan_typings(
    packages_root: &std::path::Path,
    keep: &std::collections::HashSet<std::path::PathBuf>,
) {
    let Ok(owners) = fs::read_dir(packages_root) else {
        return;
    };
    for owner in owners.flatten() {
        if !owner.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(names) = fs::read_dir(owner.path()) else {
            continue;
        };
        for name in names.flatten() {
            let dir = name.path();
            if name.file_type().is_ok_and(|t| t.is_dir()) && !keep.contains(&dir) {
                let _ = fs::remove_dir_all(&dir);
            }
        }
    }
}

impl<'a> ScriptEngine<'a> {
    /// Partition the auto-load set across isolates (`PACKAGE-ISOLATES-SANDBOX.md`): the
    /// server's local module files (`<server>/modules/*.{js,ts,jsx,tsx}`) and any *trusted*
    /// installed packages share the trusted main isolate; each *untrusted* installed package
    /// becomes its own sandboxed isolate. Trust is the per-profile `LockedPackage::trusted`
    /// flag (default `false` — sandboxed until the user trusts it). Loading itself is done by
    /// [`ScriptRuntime::load_modules`] into each target isolate.
    fn build_isolate_plan(server_name: &str) -> IsolatePlan {
        // Best-effort: refresh the managed VS Code TypeScript project so authors get
        // `smudgy:core` types — plus the `.d.ts` shipped by installed `smudgy://` packages —
        // in their editor. Pure-disk; never blocks session start.
        let installed_typings = materialize_installed_typings(server_name);
        if let Err(e) = crate::models::script_typings::ensure_script_tsconfig_with_packages(
            server_name,
            &installed_typings,
        ) {
            warn!("Failed to write script tsconfig for {server_name}: {e:#}");
        }

        let mut local_modules = Vec::new();
        if let Ok(smudgy_dir) = get_smudgy_home() {
            let modules_dir = smudgy_dir.join(server_name).join("modules");
            match fs::read_dir(&modules_dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        if !entry.file_type().is_ok_and(|file_type| file_type.is_file()) {
                            continue;
                        }
                        let file_name = entry.file_name();
                        let file_name = file_name.to_string_lossy();
                        if [".js", ".ts", ".jsx", ".tsx"]
                            .iter()
                            .any(|ext| file_name.ends_with(ext))
                        {
                            match Url::from_file_path(entry.path()) {
                                Ok(url) => local_modules.push(url),
                                Err(()) => {
                                    warn!("Skipping module with non-file path: {file_name}");
                                }
                            }
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!(
                    "Could not read modules directory {}: {e}",
                    modules_dir.display()
                ),
            }
        }

        let mut trusted = Vec::new();
        let mut sandboxed = Vec::new();
        let mut homes = HashMap::new();
        match crate::models::shared_packages::load_lock(server_name) {
            Ok(lock) => {
                for pkg in lock.packages {
                    // A disabled install is skipped entirely — neither a trusted nor a sandboxed
                    // root, so it never loads. Its dependencies still load if an *enabled* package's
                    // closure needs them (the engine resolves closures from enabled roots), matching
                    // the hub's dependency-graph "effectively enabled" semantics. The user can
                    // install + consent, review the code, then enable it (no re-consent) to run.
                    // It also registers no interop home: nothing runs as it, and a copy of it
                    // embedded in another package's closure must not write in its name.
                    if !pkg.enabled {
                        continue;
                    }
                    if let Ok(spec) = SmudgySpecifier::parse(&pkg.specifier) {
                        homes.insert(
                            (
                                spec.owner.to_ascii_lowercase(),
                                spec.name.to_ascii_lowercase(),
                            ),
                            if pkg.trusted {
                                crate::session::runtime::store::HomeIsolate::Main
                            } else {
                                crate::session::runtime::store::HomeIsolate::OwnSandbox
                            },
                        );
                    }
                    if pkg.trusted {
                        trusted.push(pkg.specifier);
                    } else {
                        sandboxed.push(pkg.specifier);
                    }
                }
            }
            Err(e) => warn!("Failed to load package lockfile for {server_name}: {e:#}"),
        }

        IsolatePlan {
            main: ModuleSet {
                local_modules,
                packages: trusted,
            },
            sandboxed,
            homes,
            installed_typings,
        }
    }

    /// Emit a client-side notice line into the session output buffer (used for the
    /// package auto-update nudge). Best-effort: if the UI channel is full the notice is
    /// dropped rather than blocking session startup.
    fn emit_session_notice(
        ui_tx: &Sender<TaggedSessionEvent>,
        session_id: SessionId,
        emitted_line_count: &std::rc::Weak<Cell<usize>>,
        message: &str,
    ) {
        let mut updates = Vec::new();
        for line in message.split('\n') {
            updates.push(BufferUpdate::Append(Arc::new(StyledLine::from_echo_str(
                line,
            ))));
            updates.push(BufferUpdate::EnsureNewLine);
            if let Some(count) = emitted_line_count.upgrade() {
                count.set(count.get() + 1);
            }
        }
        let mut ui_tx = ui_tx.clone();
        let _ = ui_tx.try_send(TaggedSessionEvent {
            session_id,
            event: SessionEvent::UpdateBuffer(Arc::new(updates)),
        });
    }

    /// The code-import stumble diagnostic (`docs/interop.md` §3): after an isolate's
    /// modules evaluate, every package its loader served whose interop **home** is a different
    /// isolate is a code-imported copy of an installed package — its module side effects
    /// duplicate the home instance's, and the home gate will refuse its interop writes. One
    /// teaching notice per package, emitted here at load so the wrong import is never silent.
    /// Uninstalled packages (no home entry) get no notice: consuming a pure library by import is
    /// the intended path. Covers the load-time module graph; a later dynamic `import()` of a
    /// homed package is only caught at its first refused write.
    #[allow(clippy::too_many_arguments)]
    fn emit_stumble_notices(
        loader: &Rc<dyn PackageProvider>,
        homes: &crate::session::runtime::store::HomeRegistry,
        loaded_into: &IsolateId,
        handle_packages: &std::collections::HashSet<(String, String)>,
        ui_tx: &Sender<TaggedSessionEvent>,
        session_id: SessionId,
        emitted_line_count: &std::rc::Weak<Cell<usize>>,
    ) {
        let fold = |key: &smudgy_script::PackageKey| {
            (
                key.owner.to_ascii_lowercase(),
                key.name.to_ascii_lowercase(),
            )
        };
        let scrubbed: std::collections::HashSet<(String, String)> =
            loader.scrubbed_packages().iter().map(fold).collect();
        for key in loader.loaded_packages() {
            let folded = fold(&key);
            let producer = crate::session::runtime::store::ProducerKey::Package {
                owner: folded.0.clone(),
                name: folded.1.clone(),
            };
            let installed = homes.borrow().contains_key(&folded);
            if scrubbed.contains(&folded) {
                // The scrub (interop.md §3) removed handle exports from this non-home copy;
                // an import of one of those names fails at LINK with V8's "does not provide
                // an export named …" — this notice is the dressing that names the fix.
                Self::emit_session_notice(
                    ui_tx,
                    session_id,
                    emitted_line_count,
                    &format!(
                        "[interop] smudgy://{owner}/{name} was code-imported, so its interop \
                         handle exports were removed from this copy \u{2014} import them from \
                         smudgy:state/{owner}/{name}, smudgy:events/{owner}/{name}, or \
                         smudgy:procedures/{owner}/{name} instead.",
                        owner = key.owner,
                        name = key.name
                    ),
                );
            } else if installed
                && !crate::session::runtime::store::is_home(homes, &producer, loaded_into)
            {
                Self::emit_session_notice(
                    ui_tx,
                    session_id,
                    emitted_line_count,
                    &format!(
                        "[interop] you code-imported smudgy://{}/{}, which is installed \u{2014} \
                         this copy duplicates the installed instance's side effects and cannot \
                         publish state or events. Import types only, or read its published state.",
                        key.owner, key.name
                    ),
                );
            }
        }
        // On main, a trusted package's home load can't be scrubbed (one module map, one
        // instance): a user script's code import hands out LIVE producer handles whose
        // writes publish as the package — the accepted interop.md §1 residual, warned so the
        // attribution is never a surprise.
        if matches!(loaded_into, IsolateId::Main) {
            for key in loader.user_code_imports() {
                let folded = fold(&key);
                let main_home = homes.borrow().get(&folded)
                    == Some(&crate::session::runtime::store::HomeIsolate::Main);
                if main_home && handle_packages.contains(&folded) {
                    Self::emit_session_notice(
                        ui_tx,
                        session_id,
                        emitted_line_count,
                        &format!(
                            "[interop] a user script or local module code-imported \
                             smudgy://{owner}/{name}, which declares interop handles. The import \
                             works (this is the package's home), but writes through those handles \
                             publish AS the package \u{2014} prefer smudgy:state/{owner}/{name} \
                             (and events/procedures) unless that attribution is intended.",
                            owner = key.owner,
                            name = key.name
                        ),
                    );
                }
            }
        }
    }

    /// Required-param load-gate for one isolate's installs, run over the params the last
    /// `solve_closure` collected on `provider`. An install whose required params are unset gets a
    /// session notice and is returned in the blocked set so the caller drops it (a blocked package
    /// must not evaluate misconfigured). Per-isolate: main and each sandboxed isolate gate their
    /// own closures (`PACKAGE-ISOLATES-RESOLUTION.md`).
    fn blocked_by_required_params(
        provider: &package_provider::SmudgyPackageProvider,
        server_name: &str,
        ui_tx: &Sender<TaggedSessionEvent>,
        session_id: SessionId,
        emitted_line_count: &std::rc::Weak<Cell<usize>>,
    ) -> std::collections::HashSet<smudgy_script::PackageKey> {
        let mut blocked = std::collections::HashSet::new();
        for (specifier, declared) in provider.installed_params() {
            let missing = crate::models::shared_packages::missing_required_params(
                server_name,
                &specifier,
                &declared,
            );
            if let (false, Ok(spec)) = (missing.is_empty(), SmudgySpecifier::parse(&specifier)) {
                Self::emit_session_notice(
                    ui_tx,
                    session_id,
                    emitted_line_count,
                    &format!(
                        "[package] {} not loaded: required param(s) {} are unset \u{2014} configure them in settings",
                        spec.name,
                        missing.join(", ")
                    ),
                );
                blocked.insert(spec.package_key());
            }
        }
        blocked
    }

    /// Version-floor load-gate for one isolate's installs, run over the `min_smudgy_version`
    /// declarations the last `solve_closure` collected on `provider`. An install floored above
    /// this smudgy gets a session notice and is returned in the blocked set so the caller drops
    /// it — loading it would evaluate scripts against APIs this smudgy doesn't have, and
    /// without the pre-gate the resolution-time refusal would fail the whole isolate load with
    /// one coarse line. Root floors only: a transitive dependency's floor is caught by the
    /// resolution-time gate in `SmudgyPackageProvider::resolve_package`.
    fn blocked_by_min_smudgy_version(
        provider: &package_provider::SmudgyPackageProvider,
        ui_tx: &Sender<TaggedSessionEvent>,
        session_id: SessionId,
        emitted_line_count: &std::rc::Weak<Cell<usize>>,
    ) -> std::collections::HashSet<smudgy_script::PackageKey> {
        let running = crate::models::shared_packages::running_smudgy_release();
        let mut blocked = std::collections::HashSet::new();
        for (specifier, min) in provider.installed_min_versions() {
            let Ok(spec) = SmudgySpecifier::parse(&specifier) else {
                continue;
            };
            // A local dev-override is exempt, mirroring the sandboxed branch and the
            // loader's early return: the collected floor came from the PUBLISHED copy's
            // manifest (the solve resolves over the wire), but the code that would load is
            // the author's on-disk override.
            if provider.is_local_override(&spec.package_key()) {
                continue;
            }
            let mut floor = crate::models::shared_packages::SmudgyVersionFloor::default();
            floor.fold(&spec.name, Some(&min));
            if let Some(reason) = floor.refusal(&running) {
                Self::emit_session_notice(
                    ui_tx,
                    session_id,
                    emitted_line_count,
                    &format!("[package] {} not loaded \u{2014} {reason}.", spec.name),
                );
                blocked.insert(spec.package_key());
            }
        }
        blocked
    }

    pub fn new(params: ScriptEngineParams<'a>) -> Self {
        let smudgy_dir = get_smudgy_home().unwrap();
        let server_path = smudgy_dir.join(params.server_name.as_str());

        let current_line = Rc::new(RefCell::new(Weak::new()));
        let line_scope: ops::LineScopeCell = Rc::new(Cell::new(ops::LineScope::default()));

        // Per-isolate waker demux state (`EVENT-LOOP-READINESS-DEMUX.md`): `ready` records which
        // isolates have a pending wakeup; `parent` holds the session task's waker so a per-isolate
        // `DemuxWaker` can re-arm it. Each isolate is seeded into `ready` on insert (below) so its
        // first pump arms its own waker.
        let ready: ReadySet = Arc::new(Mutex::new(FxHashSet::default()));
        let parent: ParentSlot = Arc::new(Mutex::new(None));

        // Shared session state every isolate's ops bind into (legal because all isolates live
        // on the one session thread). Captured as locals so the per-isolate extension builder
        // below borrows *these* rather than `params`, whose fields move into `Self`.
        let session_id = params.session_id;
        let server_name = Arc::clone(params.server_name);
        let spawned_actions = params.spawned_actions.clone();
        let pending_ops = params.pending_line_operations.clone();
        let emitted_line_count = params.emitted_line_count.clone();
        // The same ring `Rc` is bound into every isolate's read ops.
        let recent_lines = params.recent_lines.clone();
        // The same current-location `Rc` is bound into every isolate's read op.
        let current_location = params.current_location.clone();
        // The same settings snapshot `Rc` is bound into every isolate's `getSettings()` read op.
        let settings_snapshot = params.settings_snapshot.clone();
        // The same GMCP enabled cell is bound into every isolate's `gmcp.enabled` read op.
        let gmcp_enabled = params.gmcp_enabled.clone();
        // The same pane registry + per-line routing `Rc`s are bound into every isolate's
        // pane/routing ops (all isolates share the one session thread).
        let pane_registry = params.pane_registry.clone();
        let line_routing = params.line_routing.clone();
        // The same input mirror `Rc` is bound into every isolate's input read ops.
        let input_mirror = params.input_mirror.clone();
        // The same submission cell `Rc` is bound into every isolate's submission ops.
        let input_submission = params.input_submission.clone();
        // The same word-set cell `Rc` is bound into every isolate's registry ops.
        let input_word_sets = params.input_word_sets.clone();
        // The same pane-input handler registry `Rc` is bound into every isolate's pane ops.
        let pane_input_callbacks = params.pane_input_callbacks.clone();
        let mapper = params.mapper.clone();
        let extra_extensions = params.extra_script_extensions.clone();
        let current_line_for_ext = current_line.clone();
        let line_scope_for_ext = line_scope.clone();
        // The same introspection mirror the `Manager` writes; bound into every isolate's ops.
        let automation_registry = params.automation_registry.clone();
        // One session-global `singleton` reservation set, shared (the same `Rc`) into every
        // isolate's ops below so `createAlias(.., {singleton:true})` dedupes session-wide
        // regardless of which isolate the copy runs in (`PACKAGE-ISOLATES.md`). A reload
        // rebuilds the whole engine, so this set — and thus every reservation — resets with it.
        let singleton_registry: SingletonRegistry =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        // The session-global event bus, shared (same `Rc`) into every isolate's ops like
        // `singleton_registry`, so `emit` reaches subscribers across isolates (`PACKAGE-EVENTS.md`).
        let event_registry: ops::EventRegistry =
            Rc::new(RefCell::new(std::collections::HashMap::new()));
        // The session store, shared (same `Rc`) into every isolate's ops; owned by the runtime,
        // which flushes it per turn (`docs/interop.md` §2).
        let session_store = params.session_store.clone();
        // The message bus + runtime catalogue, likewise session-owned and shared into every
        // isolate's ops (D1 routing; §10 sampling/declaration).
        let message_bus = params.message_bus.clone();
        let catalogue = params.catalogue.clone();

        // Partition the auto-load set across isolates. Up front — before the per-isolate
        // extension builder below — because the plan also carries the interop home registry,
        // which every isolate's ops receive and which must be complete before ANY module
        // evaluates (a trusted package's top-level `set()` passes the home gate only if its
        // entry is already registered).
        let plan = Self::build_isolate_plan(params.server_name.as_str());
        let home_registry: crate::session::runtime::store::HomeRegistry =
            Rc::new(std::cell::RefCell::new(plan.homes));

        // Register the statically-extracted handle declarations into the runtime catalogue
        // (interop.md §10 tier 1 + 3) for this engine generation — the runtime cleared the previous
        // generation's flags before this rebuild. Constructors confirm them at evaluation via
        // `op_smudgy_interop_declare`.
        {
            let mut cat = catalogue.borrow_mut();
            for pkg in &plan.installed_typings {
                let producer = crate::session::runtime::store::ProducerKey::Package {
                    owner: pkg.owner.to_ascii_lowercase(),
                    name: pkg.name.to_ascii_lowercase(),
                }
                .to_string();
                for handle in &pkg.handles {
                    let kind = match handle.kind {
                        smudgy_script::interop_extract::InteropKind::State => {
                            crate::session::runtime::catalogue::CatalogueKind::State
                        }
                        smudgy_script::interop_extract::InteropKind::Event => {
                            crate::session::runtime::catalogue::CatalogueKind::Event
                        }
                        smudgy_script::interop_extract::InteropKind::Procedure => {
                            crate::session::runtime::catalogue::CatalogueKind::Procedure
                        }
                    };
                    cat.declare(
                        &producer,
                        kind,
                        &handle.name,
                        handle.type_alias.as_deref(),
                        handle.declared_shape.as_deref(),
                    );
                }
            }
        }
        // The installed packages that declare interop handles (folded keys) — the stumble
        // pass warns when USER code code-imports one of these on main (interop.md §1/§3).
        let interop_handle_packages: std::collections::HashSet<(String, String)> = plan
            .installed_typings
            .iter()
            .filter(|pkg| !pkg.handles.is_empty())
            .map(|pkg| {
                (
                    pkg.owner.to_ascii_lowercase(),
                    pkg.name.to_ascii_lowercase(),
                )
            })
            .collect();

        // Build the deno extension set for one isolate: its own `script_functions` registry
        // (the v8 globals the creation ops push into — isolate-bound, so never shared) plus the
        // shared session ops stamped with this isolate's id, the mapper bridge, and the
        // embedder's extras. Returns the registry so the caller wraps the isolate around it,
        // plus the instance nonce allocated here (the ops bake it into the widget routing
        // token, and the caller records it on the `Isolate` so dispatch can compare the two).
        let make_extensions = |isolate_id: IsolateId,
                               smudgy_grants: ops::SmudgyGrants,
                               data_dir: std::path::PathBuf| {
            let instance = NEXT_ISOLATE_INSTANCE.fetch_add(1, Ordering::Relaxed);
            let script_functions: Rc<RefCell<Vec<v8::Global<v8::Function>>>> =
                Rc::new(RefCell::new(Vec::new()));
            let mut extensions = vec![
                ops::smudgy_ops::init(
                    session_id,
                    Arc::clone(&server_name),
                    script_functions.clone(),
                    spawned_actions.clone(),
                    pending_ops.clone(),
                    current_line_for_ext.clone(),
                    // The current-line staleness scope the ambient `line` mutators check
                    // (armed by the engine's user-JS entry points).
                    line_scope_for_ext.clone(),
                    emitted_line_count.clone(),
                    // The read ops resolve `buffer.line(n)` against this ring.
                    recent_lines.clone(),
                    // The `getCurrentLocation` read op resolves against this shared cell.
                    current_location.clone(),
                    // The `getSettings` read op resolves against this shared snapshot.
                    settings_snapshot.clone(),
                    // The `gmcp.enabled` read op resolves against this shared cell.
                    gmcp_enabled.clone(),
                    // The pane ops mutate this shared registry synchronously; the isolate's
                    // namespace is derived from `isolate_id` inside the ops.
                    pane_registry.clone(),
                    // The current-line routing ops (redirect/copy/gag) write here.
                    line_routing.clone(),
                    // The input read op resolves `input.value`/`cursor`/… against this
                    // mirror and flags interest on it; writes bypass it.
                    input_mirror.clone(),
                    // The ambient `submission` ops act on this shared cell while a
                    // `sys:input` handler splice is live.
                    input_submission.clone(),
                    // The completion word-set registry ops mutate and read this shared
                    // cell synchronously, scoped by the caller's (isolate, origin).
                    input_word_sets.clone(),
                    // The pane-input registration op records onSubmit handler addresses
                    // here; the runtime's `PaneInputSubmit` dispatch arm resolves them.
                    pane_input_callbacks.clone(),
                    // The ops stamp this id onto every automation they create, so the trigger
                    // Manager keys them under `(isolate, …)` — coexistence across isolates.
                    isolate_id,
                    // The instantiation nonce the widget routing token carries (see above).
                    instance,
                    // Same `Rc` for every isolate: the singleton dedupe is session-wide.
                    singleton_registry.clone(),
                    // The introspection mirror the `get`/`list`/`exists` ops read.
                    automation_registry.clone(),
                    // The smudgy op-capabilities this isolate may use
                    // (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`). `all()` for main/trusted; the
                    // consented set for a sandbox. The gated ops read it from `OpState`, and its
                    // `widgets` bit is mirrored to the `smudgy_widgets` ops via `WidgetsEnabled`.
                    smudgy_grants,
                    // Same `Rc` for every isolate: the event bus is session-wide (`PACKAGE-EVENTS.md`).
                    event_registry.clone(),
                    // Same `Rc` for every isolate: the session store is session-wide; writes
                    // journal here and the runtime flushes per turn.
                    session_store.clone(),
                    // Same `Rc` for every isolate: the message bus routes posts to the
                    // producer's home instance across isolates (`interop.md` §6).
                    message_bus.clone(),
                    // Same `Rc` for every isolate: the runtime catalogue samples emissions/
                    // posts and records declarations (`docs/interop.md` §10).
                    catalogue.clone(),
                    // Same `Rc` for every isolate: the interop home registry the store/emit
                    // write gates check (`docs/interop.md` §3).
                    home_registry.clone(),
                    // The store's widget-binding cell registry (interop.md §7), parked in `OpState`
                    // for the leaf `smudgy_widgets` build ops to resolve binding tokens.
                    session_store.borrow().bindings(),
                    // This isolate's `$DATA` dir, exposed to the script as `getDataDir()`.
                    data_dir,
                ),
                mapper_api::smudgy_mapper::init(mapper.clone()),
            ];
            extensions.extend((extra_extensions)());
            (extensions, script_functions, instance)
        };

        // `smudgy://` resolution is PER-ISOLATE (`PACKAGE-ISOLATES-RESOLUTION.md`):
        // each isolate gets its own provider so it solves its own closure independently — main may
        // land `util@1.4` while a sandboxed isolate lands `util@1.2`, with no cross-isolate
        // collapse. The cloud-backed provider is built once as the MAIN isolate's provider (its
        // solve state is main's closure); each sandboxed isolate `fork`s it below, sharing only the
        // HTTP client, the disk cache, and the per-server lockfile. A test override supplies a
        // fresh resolver per isolate via its factory and skips the solve + auto-update notices
        // (cloud-provider-only). Either way each isolate's own loader compiles the package source
        // into *its* heap.
        let smudgy_provider =
            build_package_provider(params.package_client, Arc::clone(params.server_name));
        // Whether sandboxed isolates have any way to resolve `smudgy://` — a cloud base to fork, or
        // a test override factory. With neither, the untrusted installs can't load (handled below).
        let have_resolver = smudgy_provider.is_some() || params.package_provider_override.is_some();
        // The MAIN isolate's loader provider: the override factory's fresh resolver, else the cloud
        // base (which is main's own provider).
        let main_isolate_loader: Option<Rc<dyn PackageProvider>> =
            match &params.package_provider_override {
                Some(factory) => Some(factory()),
                None => smudgy_provider
                    .clone()
                    .map(|provider| -> Rc<dyn PackageProvider> { provider }),
            };
        let mut main_set = plan.main;
        let sandboxed = plan.sandboxed;

        // Pre-pass for the MAIN isolate: solve the cross-tree collapse/coexistence over
        // main's OWN closure (its trusted packages) and run the required-param load-gate. Each
        // sandboxed isolate solves its own closure independently below
        // (`PACKAGE-ISOLATES-RESOLUTION.md`); this walks only main's installs.
        // Cloud-provider-only (an override has no params/solve).
        if let Some(provider) = &smudgy_provider {
            params.tokio_runtime.block_on(async {
                provider.solve_closure(&main_set.packages).await;
            });
            // Drop any trusted install whose required params are unset (it must not run
            // misconfigured); a notice naming the keys is emitted by the gate.
            let mut blocked = Self::blocked_by_required_params(
                provider,
                params.server_name.as_str(),
                &params.ui_tx,
                params.session_id,
                &params.emitted_line_count,
            );
            // And any trusted install whose `min_smudgy_version` floor is above this smudgy
            // (per-package refusal + notice; trusted packages get no cap_version hold-back
            // walk, so a too-new floor refuses rather than falling back to an older version —
            // pin an older version or update smudgy).
            blocked.extend(Self::blocked_by_min_smudgy_version(
                provider,
                &params.ui_tx,
                params.session_id,
                &params.emitted_line_count,
            ));
            if !blocked.is_empty() {
                // Prune the interop home registry of every blocked package: it is not going to
                // load, so nothing runs as it, and a code-imported copy in main must not pass the
                // home gate in its name (a blocked *trusted* package would otherwise keep
                // `home = Main` and let any main-isolate code publish/emit as it). Version-blind,
                // ASCII-folded key like the registry itself.
                {
                    let mut homes = home_registry.borrow_mut();
                    for key in &blocked {
                        homes.remove(&(
                            key.owner.to_ascii_lowercase(),
                            key.name.to_ascii_lowercase(),
                        ));
                    }
                }
                main_set.packages.retain(|specifier| {
                    SmudgySpecifier::parse(specifier)
                        .ok()
                        .is_none_or(|spec| !blocked.contains(&spec.package_key()))
                });
            }
        }

        // Tell main's loader which packages are home there (interop.md §3): every other
        // package's modules get interop-handle entry exports scrubbed on load, so a
        // code-importing consumer fails at link instead of receiving a live producer handle.
        // Read AFTER the blocked-package prune above: a blocked trusted package is home
        // nowhere, so even a later dynamic `import()` of it serves the scrubbed copy.
        if let Some(loader) = &main_isolate_loader {
            let main_homes: Vec<smudgy_script::PackageKey> = home_registry
                .borrow()
                .iter()
                .filter(|(_, home)| {
                    matches!(home, crate::session::runtime::store::HomeIsolate::Main)
                })
                .map(|((owner, name), _)| smudgy_script::PackageKey {
                    owner: owner.clone(),
                    name: name.clone(),
                })
                .collect();
            loader.set_home_packages(main_homes);
        }

        // Accumulated across all isolates for the "Loaded N …" session echoes. A main-load
        // failure surfaces its own cause line above and leaves both tallies at zero, so the
        // per-line emission below prints nothing misleading when nothing actually loaded.
        let mut local_count = 0usize;
        let mut package_lines: Vec<String> = Vec::new();
        let mut isolates: HashMap<IsolateId, Isolate> = HashMap::new();
        // The concrete forked provider for each sandboxed isolate, held so its per-isolate
        // auto-update + duplicate-version notices can be drained after all loads (cloud only).
        let mut sandbox_providers: Vec<Rc<package_provider::SmudgyPackageProvider>> = Vec::new();

        // --- Main isolate: local modules + trusted packages, allow-all, inspector-armed. ---
        // Main (and trusted packages, which live in main) get every smudgy capability — ungated
        // (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`).
        let (main_extensions, main_script_functions, main_instance) =
            // Main-isolate `getDataDir()` returns the shared server data dir.
            make_extensions(IsolateId::Main, ops::SmudgyGrants::all(), server_path.clone());
        let mut main_runtime = build_script_runtime(
            main_extensions,
            server_path.clone(),
            // Main-isolate webstorage stays at `<server>/webstorage` (data_dir default).
            None,
            inspector_config(),
            main_isolate_loader.clone(),
            // The main isolate (user scripts, local modules, trusted packages) stays allow-all,
            // and imports freely (`ImportPolicy::Any`).
            None,
            ImportPolicy::Any,
            params.tokio_runtime.clone(),
        )
        .expect("Failed to create JS runtime");
        // Surface the v8 inspector endpoint (main only) so it can be debugged via the bundled
        // `smudgy_inspector` helper (or any CDP client).
        if let Some(addr) = main_runtime.inspector_address() {
            info!(
                "Script inspector for session {} listening at {addr} -- debug with: smudgy_inspector {addr}",
                params.session_id
            );
            crate::session::registry::set_inspector_address(params.session_id, addr);
        }
        let main_load = {
            // Model B: bracket the v8 work — `load_modules` evaluates JS, so main must be the
            // thread's current isolate while it loads, then released.
            let _entered = EnteredIsolate::enter(main_runtime.deno_runtime());
            params
                .tokio_runtime
                .block_on(async { main_runtime.load_modules(&main_set).await })
        };
        match main_load {
            Ok(report) => {
                info!(
                    "Loaded {} module(s) into main for session {}",
                    report.modules.len(),
                    params.session_id
                );
                fold_load_report(&report, &mut local_count, &mut package_lines);
            }
            Err(e) => {
                warn!("Failed to load main modules: {e:?}");
                // One broken module fails the whole synthetic entry, so surface the cause
                // (path-named) in the session instead of swallowing it into the log.
                Self::emit_session_notice(
                    &params.ui_tx,
                    params.session_id,
                    &params.emitted_line_count,
                    &format!("[packages] failed to load modules \u{2014} {e:#}"),
                );
            }
        }
        // Code-import stumble check over what main's loader actually served (partial loads
        // included — a resolved copy evaluated even if a later module failed the load).
        if let Some(loader) = &main_isolate_loader {
            Self::emit_stumble_notices(
                loader,
                &home_registry,
                &IsolateId::Main,
                &interop_handle_packages,
                &params.ui_tx,
                params.session_id,
                &params.emitted_line_count,
            );
        }
        let main_waker = build_demux_waker(IsolateId::Main, &ready, &parent);
        isolates.insert(
            IsolateId::Main,
            Isolate {
                runtime: main_runtime,
                instance: main_instance,
                script_functions: main_script_functions,
                compiled_scripts: Vec::new(),
                waker: main_waker,
            },
        );
        // Seed: arm `Main` on the first pump. At construction `parent` is still
        // `None` (no task parked yet), so no parent-wake is needed — the run loop's first
        // `poll_event_loop` provides the pump that arms `Main`'s `DemuxWaker`.
        ready
            .lock()
            .expect("ready-set poisoned")
            .insert(IsolateId::Main);

        // --- One sandboxed isolate per installed-untrusted package (`PACKAGE-ISOLATES-SANDBOX.md`).
        // Each package gets its own `ScriptRuntime`/loader/heap + own data dir, loaded via a single
        // synthetic import of just its root, with firing + the event loop routed per-isolate, and
        // its OWN provider (a fork of the cloud base, or a fresh override resolver) so it resolves
        // its closure independently of main and siblings.
        if have_resolver {
            // A sandboxed isolate's grant is the package's CONSENTED permission union,
            // not the live manifest closure union (`PACKAGE-ISOLATES-CONSENT-TRUST.md`). Read
            // the lockfile once here; each install's consent record is looked up below. A `None`
            // (or missing) record yields the empty union → deny-all ("must consent"), and an
            // unaccepted update escalation stays withheld because the consented union still holds
            // the old set. Best-effort: an unreadable lock degrades to deny-all, the safe default.
            let consent_lock =
                crate::models::shared_packages::load_lock(params.server_name.as_str())
                    .unwrap_or_default();
            for specifier in sandboxed {
                let Ok(spec) = SmudgySpecifier::parse(&specifier) else {
                    warn!("Skipping sandboxed package with malformed specifier {specifier}");
                    continue;
                };
                // This isolate's provider + loader. The override factory takes precedence (tests);
                // otherwise fork the cloud base. `pkg_cloud` is the concrete handle for the
                // per-isolate solve + notice drains (`None` under an override, which has neither).
                let (pkg_cloud, loader): (
                    Option<Rc<package_provider::SmudgyPackageProvider>>,
                    Rc<dyn PackageProvider>,
                ) = if let Some(factory) = &params.package_provider_override {
                    (None, factory())
                } else {
                    // `have_resolver` guarantees a cloud base here (no override → must be cloud).
                    let forked = Rc::new(
                        smudgy_provider
                            .as_ref()
                            .expect("have_resolver implies a cloud base without an override")
                            .fork(),
                    );
                    let loader: Rc<dyn PackageProvider> = forked.clone();
                    (Some(forked), loader)
                };
                // This sandbox is home to exactly its own package (interop.md §3): every
                // dependency's modules — a closure-embedded copy is never home — get their
                // interop-handle exports scrubbed on load.
                loader.set_home_packages(vec![spec.package_key()]);

                // Cloud: solve THIS isolate's closure (just its root) and gate its required params
                // before loading. An override has neither.
                if let Some(provider) = &pkg_cloud {
                    // Permission-capped resolution (`PACKAGE-ISOLATES-CONSENT-TRUST.md`): a sandboxed
                    // package loads at the highest version whose CLOSURE permission union fits the
                    // user's consented grant — never a newer version that demands more access. If no
                    // version fits, refuse to load it (the user must review + grant the update). A
                    // local dev-override (the author's own package) is NOT version-capped — it loads
                    // the manifest version on disk — and is sandboxed to that manifest's permissions
                    // (the enforced-grant source below), not allow-all.
                    if provider.is_local_override(&spec.package_key()) {
                        params.tokio_runtime.block_on(async {
                            provider
                                .solve_closure(std::slice::from_ref(&specifier))
                                .await;
                        });
                    } else {
                        let consented = consent_lock
                            .find(&specifier)
                            .and_then(|locked| locked.consented_permissions.clone())
                            .unwrap_or_default();
                        let capped = params
                            .tokio_runtime
                            .block_on(async { provider.cap_version(&specifier, &consented).await });
                        let capped = match capped {
                            Ok(capped) => capped,
                            Err(package_provider::CapRefusal::Permissions) => {
                                Self::emit_session_notice(
                                    &params.ui_tx,
                                    params.session_id,
                                    &params.emitted_line_count,
                                    &format!(
                                        "[package] {} not loaded \u{2014} the available versions need more \
                                         permissions than you've granted. Open Automations to review and \
                                         grant the update.",
                                        spec.name
                                    ),
                                );
                                continue;
                            }
                            Err(package_provider::CapRefusal::NeedsNewerSmudgy(reason)) => {
                                Self::emit_session_notice(
                                    &params.ui_tx,
                                    params.session_id,
                                    &params.emitted_line_count,
                                    &format!(
                                        "[package] {} not loaded \u{2014} {reason}.",
                                        spec.name
                                    ),
                                );
                                continue;
                            }
                            // A grant can't fix this one: nothing was found to load. The usual
                            // cause is a stale install — e.g. a `smudgy://local/…` entry whose
                            // local package folder was deleted — though a never-loaded install can
                            // also land here while offline.
                            Err(package_provider::CapRefusal::NoVersions) => {
                                Self::emit_session_notice(
                                    &params.ui_tx,
                                    params.session_id,
                                    &params.emitted_line_count,
                                    &format!(
                                        "[package] {} not loaded \u{2014} no version of it could be found \
                                         (it may have been deleted or unpublished, or the cloud is \
                                         unreachable). Open Automations to remove or reinstall it.",
                                        spec.name
                                    ),
                                );
                                continue;
                            }
                        };
                        params.tokio_runtime.block_on(async {
                            provider
                                .solve_closure_capped(&[(specifier.clone(), capped)])
                                .await;
                        });
                    }
                    let blocked = Self::blocked_by_required_params(
                        provider,
                        params.server_name.as_str(),
                        &params.ui_tx,
                        params.session_id,
                        &params.emitted_line_count,
                    );
                    if !blocked.is_empty() {
                        // The root itself is blocked (its notice is already emitted); skip it.
                        continue;
                    }
                }

                // Resolve the root's version via THIS isolate's provider for its isolate id.
                let version = match params
                    .tokio_runtime
                    .block_on(async { loader.resolve_package(&spec.package_key(), None).await })
                {
                    Ok(pkg) => pkg.resolved_version.clone(),
                    Err(e) => {
                        Self::emit_session_notice(
                            &params.ui_tx,
                            params.session_id,
                            &params.emitted_line_count,
                            &format!("[package] {} not loaded \u{2014} {e}", spec.name),
                        );
                        continue;
                    }
                };
                let isolate_id = IsolateId::Package {
                    owner: Arc::from(spec.owner.as_str()),
                    name: Arc::from(spec.name.as_str()),
                    version: Arc::from(version.as_str()),
                };
                // The enforced grant drives BOTH the smudgy op-capabilities and the deno permission
                // container below. For a cloud install it's the CONSENTED closure union the user
                // granted at install, persisted in the lockfile (∅/`None` ⇒ deny-all; an unaccepted
                // update escalation keeps the OLD set, withholding the new asks). For a local
                // dev-override — the author's own package under `<server>/packages/` — it's the
                // package's OWN manifest permissions, read from disk: the manifest IS the grant
                // table for a local package (no consent record), so the author edits the manifest
                // and reloads to test the exact sandbox an installer will get. To develop allow-all
                // (e.g. for `ffi`/`run`, which a sandbox can never grant) the author *trusts* the
                // package, which promotes it to the main isolate above and never reaches here.
                let is_override = pkg_cloud
                    .as_ref()
                    .is_some_and(|provider| provider.is_local_override(&spec.package_key()));
                let effective = if is_override {
                    local_manifest_permissions(params.server_name.as_str(), &spec.name)
                } else {
                    consent_lock
                        .find(&specifier)
                        .and_then(|locked| locked.consented_permissions.clone())
                        .unwrap_or_default()
                };
                // The smudgy op-capabilities this isolate may call
                // (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`); the gated ops throw `NotCapable` for the
                // rest (e.g. a package that never requested `send`).
                let smudgy_grants = ops::SmudgyGrants::from_capabilities(&effective.smudgy);
                // This package's OWN persistent, update-surviving data dir (isolated from other
                // packages and the shared server root): where its `$DATA` `read`/`write` grants
                // resolve AND what `getDataDir()` returns. Create it so a `$DATA` write has a parent.
                let fs_data_dir = sandbox_fs_data_dir(&server_path, &spec.owner, &spec.name);
                let _ = fs::create_dir_all(&fs_data_dir);
                let (extensions, script_functions, instance) =
                    make_extensions(isolate_id.clone(), smudgy_grants, fs_data_dir.clone());
                let data_dir = sandbox_data_dir(&server_path, &spec.owner, &spec.name, &version);
                // Sandbox this isolate with a restricted container built from the enforced union
                // (`PACKAGE-ISOLATES-ENFORCEMENT.md`); `$DATA` in `read`/`write` expands to `fs_data_dir`.
                let permissions = match build_restricted_container(&effective, &fs_data_dir) {
                    Ok(container) => Some(container),
                    Err(e) => {
                        Self::emit_session_notice(
                            &params.ui_tx,
                            params.session_id,
                            &params.emitted_line_count,
                            &format!(
                                "[package] {} failed to build its permission sandbox \u{2014} {e:#}",
                                spec.name
                            ),
                        );
                        continue;
                    }
                };
                let mut runtime = match build_script_runtime(
                    extensions,
                    data_dir,
                    // Persistent (localStorage) storage keyed by (owner, name) so it survives
                    // this package's own version updates; version-specific cache/node_modules
                    // stay under `data_dir`.
                    Some(sandbox_storage_dir(&server_path, &spec.owner, &spec.name)),
                    None,
                    // Cloned: `loader` is read again after the load for the stumble check.
                    Some(loader.clone()),
                    permissions,
                    // The module loader gates remote imports to this isolate's consented `import`
                    // level (`None` = smudgy:// only, `Registries` = + npm/jsr, `Any` = + the web).
                    effective.import,
                    params.tokio_runtime.clone(),
                ) {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        Self::emit_session_notice(
                            &params.ui_tx,
                            params.session_id,
                            &params.emitted_line_count,
                            &format!(
                                "[package] {} failed to start its isolate \u{2014} {e:#}",
                                spec.name
                            ),
                        );
                        continue;
                    }
                };
                let set = ModuleSet {
                    local_modules: Vec::new(),
                    packages: vec![specifier.clone()],
                };
                let load = {
                    // Model B: this sandboxed isolate is the current one while its single
                    // synthetic entry evaluates, then released.
                    let _entered = EnteredIsolate::enter(runtime.deno_runtime());
                    params
                        .tokio_runtime
                        .block_on(async { runtime.load_modules(&set).await })
                };
                match load {
                    Ok(report) => {
                        fold_load_report(&report, &mut local_count, &mut package_lines);
                        // Stumble check: a dependency embedded in this sandbox's closure that is
                        // ALSO installed in its own right runs here as a code-imported copy.
                        Self::emit_stumble_notices(
                            &loader,
                            &home_registry,
                            &isolate_id,
                            &interop_handle_packages,
                            &params.ui_tx,
                            params.session_id,
                            &params.emitted_line_count,
                        );
                    }
                    Err(e) => {
                        Self::emit_session_notice(
                            &params.ui_tx,
                            params.session_id,
                            &params.emitted_line_count,
                            &format!("[package] {} failed to load \u{2014} {e:#}", spec.name),
                        );
                        // Partial evaluation before the throw can have queued automation/script
                        // actions (a top-level `createTrigger`, or a code-imported dependency's
                        // registrations) into the shared spawned-action queue, all stamped with
                        // this isolate's id. Since the isolate is discarded below, those actions
                        // would register triggers keyed to a dead isolate — every later fire then
                        // trips dispatch's liveness `debug_assert` (a panic in debug, an
                        // `Isolate not found` echo per matching line in release). Drop them here so
                        // the failed load leaves nothing behind; the reload re-runs modules cleanly.
                        spawned_actions
                            .borrow_mut()
                            .retain(|action| action.target_isolate() != Some(&isolate_id));
                        // An input-bearing pane of this package would survive as a
                        // live-looking input whose submissions vanish silently: the
                        // handler-required design leaves nothing to deliver to, and
                        // only this package's own re-split could ever re-register a
                        // handler. Close such panes through the normal close
                        // machinery — registry close, the per-pane input-state
                        // purge, and a queued `PaneClosed` (what the reload sweep
                        // sends) — so the UI and registry stay consistent.
                        // Output-only panes are left open, like every other pane
                        // whose creating script is gone: a pane outlives its
                        // package until a reload's sweep or an explicit close.
                        {
                            let namespace = super::pane::PaneNamespace::Package {
                                owner: Arc::from(spec.owner.as_str()),
                                name: Arc::from(spec.name.as_str()),
                            };
                            let doomed: Vec<(Arc<str>, super::pane::PaneKey)> = pane_registry
                                .borrow()
                                .list(&namespace)
                                .into_iter()
                                .filter(|def| !def.is_main && def.input.is_some())
                                .map(|def| (def.name.clone(), def.key))
                                .collect();
                            for (name, key) in doomed {
                                if pane_registry.borrow_mut().close(&namespace, &name).is_ok() {
                                    super::input::purge_pane_input_state(
                                        &input_mirror,
                                        &input_word_sets,
                                        &pane_input_callbacks,
                                        key,
                                    );
                                    spawned_actions
                                        .borrow_mut()
                                        .push_back(super::RuntimeAction::PaneClosed { key });
                                }
                            }
                        }
                        // Completion word-set contributions land synchronously (the registry
                        // ops mutate the shared cell directly), so the partial evaluation may
                        // also have seated words under this now-dead isolate — unclearable by
                        // anything short of a full reload, and merged into every Tab push in
                        // the meantime. Purge its seats and flag the affected inputs so the
                        // UI's merged copy drops them. (The load's own push action was
                        // retained above — it names no isolate — and reads the purged sets at
                        // dispatch; the flag check keeps this from queueing a duplicate.)
                        {
                            let mut sets = input_word_sets.borrow_mut();
                            for key in sets.purge_isolate(&isolate_id) {
                                if sets.flag_push(key) {
                                    spawned_actions.borrow_mut().push_back(
                                        super::RuntimeAction::InputWordSetsChanged { key },
                                    );
                                }
                            }
                        }
                        // Pane-input onSubmit registrations land synchronously too; a
                        // handler seated under this dead isolate could only ever be a
                        // warn-and-drop at dispatch, so purge it with the isolate.
                        pane_input_callbacks.borrow_mut().purge_isolate(&isolate_id);
                        // This isolate is NOT being moved into `isolates`, so it drops here.
                        // Model B left it off the enter-stack (and the load bracket already
                        // released it), but `OwnedIsolate::Drop` requires it be the thread's
                        // current isolate — so enter before dropping in place, mirroring
                        // `Drop for ScriptEngine`. Without this, a package that throws/doesn't
                        // compile at load would abort the whole client instead of being
                        // skipped. (The failed load may also have left live resources behind —
                        // e.g. an open `connectTls` connection whose teardown spawns a tokio
                        // task; `Drop for ScriptRuntime` enters the session runtime itself, so
                        // no `block_on`/`enter` bracket is needed at this drop site.)
                        // SAFETY: makes this isolate current for its own drop, which
                        // performs the single matching exit.
                        unsafe {
                            runtime.deno_runtime().v8_isolate().enter();
                        }
                        drop(runtime);
                        continue;
                    }
                }
                let package_waker = build_demux_waker(isolate_id.clone(), &ready, &parent);
                // Seed before insert (set order is irrelevant): arm this isolate on the first
                // pump. Construction-time insert (no parked task yet), so like `Main`
                // it needs no parent-wake; a mid-run sandbox insert into a PARKED session
                // must additionally wake `parent` after seeding.
                ready
                    .lock()
                    .expect("ready-set poisoned")
                    .insert(isolate_id.clone());
                isolates.insert(
                    isolate_id,
                    Isolate {
                        runtime,
                        instance,
                        script_functions,
                        compiled_scripts: Vec::new(),
                        waker: package_waker,
                    },
                );
                // Hold the concrete provider so its per-isolate notices can be drained after load.
                if let Some(provider) = pkg_cloud {
                    sandbox_providers.push(provider);
                }
            }
        } else {
            // No resolver, but the lockfile lists untrusted installs: they can't load.
            for specifier in sandboxed {
                let name = SmudgySpecifier::parse(&specifier)
                    .map_or_else(|_| specifier.clone(), |spec| spec.name);
                Self::emit_session_notice(
                    &params.ui_tx,
                    params.session_id,
                    &params.emitted_line_count,
                    &format!("[package] {name} not loaded: no package backend for this session"),
                );
            }
        }

        // Confirm what auto-loaded across all isolates, so the user sees their modules +
        // installed packages took effect. Modules and packages are different things, so they get
        // separate lines, and a zero count of either is noise the user shouldn't read — each line
        // appears only when it has something to report. A clean profile with neither prints
        // nothing; a main-load failure (which surfaces its own cause line above) leaves both
        // counts at zero here, so nothing falsely claims a clean empty profile.
        if local_count > 0 {
            Self::emit_session_notice(
                &params.ui_tx,
                params.session_id,
                &params.emitted_line_count,
                &format!(
                    "Loaded {local_count} script module{}.",
                    if local_count == 1 { "" } else { "s" }
                ),
            );
        }
        if !package_lines.is_empty() {
            Self::emit_session_notice(
                &params.ui_tx,
                params.session_id,
                &params.emitted_line_count,
                &format!(
                    "Loaded {} package{}: {}.",
                    package_lines.len(),
                    if package_lines.len() == 1 { "" } else { "s" },
                    package_lines.join(", ")
                ),
            );
        }
        // Auto-update + duplicate-version notices are cloud-provider-only and PER-ISOLATE: main
        // and each sandboxed isolate solved its own closure, so draining each provider in turn means
        // the duplicate-version warning is an INTRA-isolate collision only — a cross-isolate
        // duplicate (`util@1.4` in main, `util@1.2` in a sandbox) lives in two different providers'
        // closures and never warns (`PACKAGE-ISOLATES-RESOLUTION.md`).
        if let Some(main_provider) = &smudgy_provider {
            // Version changes first (across all isolates), then duplicate warnings — preserving the
            // prior emission grouping.
            for provider in std::iter::once(main_provider).chain(sandbox_providers.iter()) {
                for (specifier, from, to) in provider.take_version_changes() {
                    let name = SmudgySpecifier::parse(&specifier)
                        .map_or_else(|_| specifier.clone(), |spec| spec.name);
                    Self::emit_session_notice(
                        &params.ui_tx,
                        params.session_id,
                        &params.emitted_line_count,
                        &format!("[package] {name} updated {from} \u{2192} {to}"),
                    );
                }
            }
            // A package loaded at ≥2 coexisting versions WITHIN one isolate can collide
            // (double-registered automations, split state). Fork or pin.
            for provider in std::iter::once(main_provider).chain(sandbox_providers.iter()) {
                for (package, versions) in provider.take_duplicate_warnings() {
                    Self::emit_session_notice(
                        &params.ui_tx,
                        params.session_id,
                        &params.emitted_line_count,
                        &format!(
                            "[package] warning: {} loaded at {} coexisting versions ({}) \u{2014} side effects may collide; consider forking or pinning",
                            package.name,
                            versions.len(),
                            versions.join(", ")
                        ),
                    );
                }
            }
        }

        // Reclaim per-version `.isolates/<slug>` scratch dirs (cache/node_modules) for package
        // versions with no copy left, and per-(owner, name) `.isolate-storage/<slug>` persistent
        // stores (webstorage/localStorage) for packages that are fully uninstalled. The version
        // keep-set is every live sandboxed isolate plus every still-installed lockfile version
        // (enabled OR disabled, and any pinned target); the storage keep-set is every (owner, name)
        // with any live isolate or lockfile entry, so a package's persisted data survives its own
        // updates and is reclaimed only when it leaves the lockfile entirely. Same-server sessions
        // share the lockfile, so a still-installed package is always kept regardless of who sweeps.
        let mut keep_isolate_slugs: std::collections::HashSet<String> = isolates
            .keys()
            .filter_map(|id| match id {
                IsolateId::Package {
                    owner,
                    name,
                    version,
                } => Some(isolate_slug(owner, name, version)),
                IsolateId::Main => None,
            })
            .collect();
        let mut keep_storage_slugs: std::collections::HashSet<String> = isolates
            .keys()
            .filter_map(|id| match id {
                IsolateId::Package { owner, name, .. } => Some(sandbox_storage_slug(owner, name)),
                IsolateId::Main => None,
            })
            .collect();
        if let Ok(lock) = crate::models::shared_packages::load_lock(params.server_name.as_str()) {
            for pkg in &lock.packages {
                let Ok(spec) = SmudgySpecifier::parse(&pkg.specifier) else {
                    continue;
                };
                keep_storage_slugs.insert(sandbox_storage_slug(&spec.owner, &spec.name));
                if let Some(version) = &pkg.last_resolved_version {
                    keep_isolate_slugs.insert(isolate_slug(&spec.owner, &spec.name, version));
                }
                if let crate::models::shared_packages::UpdateMode::Pinned { version } = &pkg.mode {
                    keep_isolate_slugs.insert(isolate_slug(&spec.owner, &spec.name, version));
                }
            }
        }
        prune_orphan_isolate_dirs(&server_path, &keep_isolate_slugs);
        prune_orphan_isolate_storage_dirs(&server_path, &keep_storage_slugs);

        Self {
            session_id: params.session_id,
            isolates,
            event_registry,
            catalogue,
            platform_event_keys: RefCell::new(HashMap::new()),
            server_name: params.server_name,
            ui_tx: params.ui_tx,
            pending_line_operations: params.pending_line_operations,
            current_line,
            line_scope,
            mapper: params.mapper,
            ready,
            parent,
        }
    }

    /// Deliver a host-native (`sys:`/`map:`) event to its subscribers: one `CallJavascriptFunction`
    /// per subscriber at depth 0 (a host event is a top-level expansion). The caller (dispatch)
    /// splices these depth-first via `ActionResult::Run`; empty when nobody is listening.
    /// (`PACKAGE-EVENTS.md`.)
    #[must_use]
    pub fn host_emit(&self, event: &str, payload_json: &str) -> Vec<super::RuntimeAction> {
        // Tier-2 catalogue sample for the platform producers (`docs/interop.md`
        // §10): `sys:connect` samples as producer `sys`, name `connect`. The platform is its
        // own sender. Non-prefixed names (none today) are skipped, not misfiled. The key
        // strings never vary per event, so they are interned on first emission
        // (`platform_event_keys`) and a hit — every `sys:receive` on the per-line path —
        // samples with refcount bumps, no key allocation.
        if let Some((producer, name)) = event.split_once(':') {
            let interned = self.platform_event_keys.borrow();
            if let Some(keys) = interned.get(event) {
                self.catalogue.borrow_mut().sample_interned(
                    &keys.producer,
                    super::catalogue::CatalogueKind::Event,
                    &keys.name,
                    &keys.name_folded,
                    &keys.sender,
                    payload_json,
                );
            } else {
                drop(interned);
                let keys = PlatformEventKeys {
                    producer: Arc::from(producer.to_ascii_lowercase()),
                    name: Arc::from(name),
                    name_folded: Arc::from(ops::fold_name(name).as_ref()),
                    sender: Arc::from(producer),
                };
                self.catalogue.borrow_mut().sample_interned(
                    &keys.producer,
                    super::catalogue::CatalogueKind::Event,
                    &keys.name,
                    &keys.name_folded,
                    &keys.sender,
                    payload_json,
                );
                self.platform_event_keys
                    .borrow_mut()
                    .insert(event.to_string(), keys);
            }
        }
        // Subscriptions are keyed folded (the uniform ASCII fold, `on()` at ops::fold_name), so the
        // lookup must fold too or a host event name with any uppercase would find no subscribers
        // while `on()` registrations folded to a different key — the case-insensitive matching
        // `PACKAGE-EVENTS.md` promises. Today's host names are all lowercase, so this is a
        // forward guard; the delivered `event` keeps its original spelling either way.
        let subscribers = self
            .event_registry
            .borrow()
            .get(ops::fold_name(event).as_ref())
            .map_or_else(Vec::new, Clone::clone);
        subscribers
            .into_iter()
            .map(|sub| super::RuntimeAction::CallJavascriptFunction {
                isolate: sub.isolate,
                id: sub.function_id,
                matches: Arc::new(vec![
                    MatchCapture {
                        name: Some(std::borrow::Cow::Borrowed("event")),
                        value: event.to_string(),
                    },
                    MatchCapture {
                        name: Some(std::borrow::Cow::Borrowed("payload")),
                        value: payload_json.to_string(),
                    },
                ]),
                depth: 0,
                is_captured: None,
            })
            .collect()
    }

    /// Whether any handler is subscribed to `event`. Folds the name the same way
    /// [`Self::host_emit`] keys its lookup, so hot-path emitters (e.g. `sys:receive`, fired per
    /// incoming line) can skip building a payload and sampling the catalogue when nobody is
    /// listening.
    #[must_use]
    pub fn has_event_subscribers(&self, event: &str) -> bool {
        self.event_registry
            .borrow()
            .get(ops::fold_name(event).as_ref())
            .is_some_and(|subs| !subs.is_empty())
    }

    pub fn set_current_line(&mut self, line: Option<Weak<StyledLine>>) {
        match line {
            Some(line) => {
                *self.current_line.borrow_mut() = line;
                // Stamp the installed line with a fresh generation: the staleness
                // nonce the entry points below capture and the ambient `line`
                // mutators check (see `ops::LineScope`).
                let mut scope = self.line_scope.get();
                scope.current = scope.current.wrapping_add(1);
                self.line_scope.set(scope);
            }
            None => {
                *self.current_line.borrow_mut() = Weak::new();
            }
        }
    }

    /// Arm the current-line scope for one synchronous user-JS entry: capture the
    /// in-flight line's generation (0 when no line is in flight — the line's `Arc`
    /// is held by its queued completion action, so a dead `Weak` means the line
    /// already finished). Returns the prior armed value for the paired
    /// [`Self::restore_line_scope`] — save/restore exactly like `EventDepth` at the
    /// same call sites. Async continuations resume in the event-loop pump, outside
    /// any armed entry, which is what makes a stale `line.gag()` detectable.
    fn arm_line_scope(
        line_scope: &ops::LineScopeCell,
        current_line: &Rc<RefCell<Weak<StyledLine>>>,
    ) -> u64 {
        let mut scope = line_scope.get();
        let prior = scope.armed;
        scope.armed = if current_line.borrow().strong_count() > 0 {
            scope.current
        } else {
            0
        };
        line_scope.set(scope);
        prior
    }

    /// The restore half of [`Self::arm_line_scope`].
    fn restore_line_scope(line_scope: &ops::LineScopeCell, prior: u64) {
        let mut scope = line_scope.get();
        scope.armed = prior;
        line_scope.set(scope);
    }

    /// Slow safety-net interval for the session run loop's idle `select!`. Readiness driving
    /// (the `poll_fn`-over-`poll_event_loop` branch in `runtime.rs`) is the *primary* re-entry
    /// into Phase 1: a resolved promise / elapsed timer / async module load wakes the parked task
    /// directly. This tick exists only to bound worst-case latency if a waker were ever missed —
    /// e.g. a future `deno_core`/`v8` bump that changes wake semantics (re-validate per
    /// `PACKAGE-ISOLATES-LIFECYCLE.md`). The demux pump polls every queued isolate and collects
    /// errors instead of early-returning, so no isolate's waker is left un-armed by an earlier
    /// isolate's error. At 500ms it costs ~2 idle wakeups/sec, and is NOT the engine driver. See
    /// `EVENT-LOOP-READINESS.md`.
    pub fn tick_interval() -> tokio::time::Interval {
        let mut tick_interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick_interval
    }

    /// Pump the event loop of each isolate the demux queued into `ready` — those whose
    /// per-isolate `DemuxWaker` fired (a completed op/timer) or that were seeded (on insert, or
    /// after synchronous JS ran on them) — polling each with its OWN waker so a later completion
    /// re-enters ONLY that isolate (`EVENT-LOOP-READINESS-DEMUX.md`). Returns `Ready(Ok)` if any
    /// polled isolate made progress (so the caller polls again), `Pending` when none did, and
    /// surfaces the first isolate error after re-queueing the un-polled remainder. With one
    /// isolate this is equivalent to polling it directly.
    pub fn poll_event_loop(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), CoreError>> {
        // Refresh `parent` to THIS poll's task waker first — the task identity can
        // change across polls, and a `DemuxWaker` re-arms the task through this slot.
        *self.parent.lock().expect("parent slot poisoned") = Some(cx.waker().clone());

        // Drain the ready-set BEFORE polling (lost-wake safety): a wake landing *during* a
        // poll re-inserts its id (and re-wakes `parent`), so it survives to the next pump.
        let todo: Vec<IsolateId> = {
            let mut ready = self.ready.lock().expect("ready-set poisoned");
            ready.drain().collect()
        };

        let mut any_ready = false;
        let mut first_err: Option<CoreError> = None;
        for id in todo {
            // A queued id can be stale if its isolate was torn down after queueing; skip it.
            let Some(isolate) = self.isolates.get_mut(&id) else {
                continue;
            };
            // Make this isolate current while its loop is pumped (Model B), then release it (RAII
            // exit). The demux changes *which* isolates are polled, never *how* — the bracket is
            // unchanged and per *polled* isolate.
            let _entered = EnteredIsolate::enter(isolate.runtime.deno_runtime());
            // Poll with the isolate's OWN waker (cloned — building the `Context` from
            // `&isolate.waker` would hold that borrow across `&mut isolate.runtime`)
            // so deno_core registers the `DemuxWaker` against its pending ops, NOT the session-task
            // waker. That is what makes the next completion re-enter ONLY this isolate.
            let w = isolate.waker.clone();
            let mut isolate_cx = Context::from_waker(&w);
            match isolate.runtime.poll_event_loop(&mut isolate_cx) {
                Poll::Ready(Ok(())) => any_ready = true,
                Poll::Ready(Err(err)) => {
                    // Poll EVERY queued isolate this pass — do NOT early-return — so a
                    // seeded-but-not-yet-armed sibling still gets its first poll (which arms its own
                    // `DemuxWaker`) instead of being shadowed forever by a persistently-erroring
                    // isolate that happens to sort ahead of it in `todo`. Polling all and surfacing
                    // the FIRST error after the loop avoids that shadow. (Early-return + re-queue does
                    // NOT: a persistently erroring isolate wins the drain race every pump and
                    // re-strands the sibling.)
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
                Poll::Pending => {}
            }
        }

        // Surface the first error (the run loop's `Ready(Err)` arm in `runtime.rs` warns/echoes and
        // keeps the session alive); else report progress so the caller drains again,
        // else park. Every isolate in `todo` was polled regardless of an earlier error, so none is
        // left un-polled to strand.
        if let Some(err) = first_err {
            Poll::Ready(Err(err))
        } else if any_ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    /// Seed `id` into the demux ready-set so the next `poll_event_loop` polls it. Called when
    /// synchronous JS runs on an isolate (alias/trigger/hotkey dispatch), because that JS can
    /// schedule async work (timers, promise continuations) WITHOUT a pump to arm the isolate's own
    /// waker — so the demux must be told to poll it, or the work is stranded (purely trusting the
    /// demux for the dispatch isolate is unsafe).
    /// No parent-wake: dispatch runs on the session thread with the run loop active, so the next
    /// Phase 1 pump drains this seed before parking.
    fn mark_isolate_ready(&self, id: &IsolateId) {
        self.ready
            .lock()
            .expect("ready-set poisoned")
            .insert(id.clone());
    }

    /// Mutable access to one isolate's bundle. Errors if the id isn't present, which is a
    /// routing bug: every `IsolateId` reaching the engine was produced by the ops of an
    /// isolate that was constructed (and never torn down mid-run).
    fn isolate_mut(&mut self, isolate: &IsolateId) -> Result<&mut Isolate> {
        self.isolates
            .get_mut(isolate)
            .ok_or_else(|| anyhow!("Isolate {isolate:?} not found"))
    }

    #[inline]
    pub fn call_javascript_function(
        &mut self,
        trigger_manager: &Manager,
        isolate: &IsolateId,
        function_id: FunctionId,
        matches: &Arc<Vec<MatchCapture>>,
        depth: u32,
    ) -> Result<ActionResult> {
        // Per-line timing is TRACE-only; skip the clock entirely above TRACE so the
        // hot path pays just a level check (compiled out when TRACE is statically off).
        let started = log_enabled!(log::Level::Trace).then(Instant::now);

        // Demux: this isolate is about to run JS synchronously. Async work it schedules without a
        // `poll_event_loop` pass must still be serviced — a microtask (`Promise.then`) in
        // particular does NOT wake the runtime on its own (unlike a `setTimeout`, whose timer op
        // wakes deno's registered waker), so it would be stranded. Seed this isolate so the next
        // pump polls it and runs/arms that work (proven by the `demux_async_dispatch`
        // integration test, which strands the continuation if this seed is removed).
        self.mark_isolate_ready(isolate);

        // Cloned out before `isolate_mut` borrows `self`; armed below beside `EventDepth`.
        let line_scope = self.line_scope.clone();
        let current_line = self.current_line.clone();

        let bundle = self.isolate_mut(isolate)?;
        // Clone the `v8::Global` out and drop the registry `Ref` *before* the v8 call.
        // `script_functions` is the same `Rc<RefCell<…>>` this isolate's create ops
        // (`op_smudgy_create_javascript_function_*`) `borrow_mut` to register new
        // functions, so holding the `Ref` across `f.call(…)` would panic if the called
        // handler synchronously registers another automation (a `createAlias`/
        // `createTrigger` whose action is a function — a normal scripting pattern).
        // Cloning a `Global` is just a handle ref-count bump; it also releases the
        // `&mut bundle` borrow so `bundle.runtime` can be taken below.
        let f = {
            let script_functions = bundle.script_functions.borrow();
            match script_functions.get(usize::from(function_id)) {
                Some(f) => f.clone(),
                None => bail!("Function {} not found", function_id),
            }
        };

        let deno = bundle.runtime.deno_runtime();
        // Record this delivery's depth so an `emit` (or store `set`) from inside the handler
        // queues its subscribers/watchers one level deeper, terminating cycles at the cap
        // (`PACKAGE-EVENTS.md`). `EventDepth` is per-isolate `OpState` shared by everything that
        // later runs in this isolate, so it is SAVED and RESTORED around the call: leaving this
        // handler's depth behind would make async continuations (timers, resolved promises) and
        // the next dispatch on this isolate journal store writes at the wrong depth, ratcheting
        // watch deliveries to the cap with no real recursion. The `Rc` is cloned so it outlives
        // the v8 borrows of `deno` below.
        let op_state = deno.op_state();
        let prior_depth = op_state
            .borrow()
            .try_borrow::<ops::EventDepth>()
            .map_or(0, |d| d.0);
        op_state.borrow_mut().put(ops::EventDepth(depth));
        // Arm the current-line scope for this entry (save/restore like `EventDepth`):
        // trigger and `sys:receive` handlers run for the line in flight, so its
        // generation is captured here and the ambient `line` mutators compare it.
        let prior_line_scope = Self::arm_line_scope(&line_scope, &current_line);
        // Make the owning isolate current for this call (it usually isn't — Model B leaves the
        // enter-stack empty between ops); released after the scope on the way out.
        let _entered = EnteredIsolate::enter(deno);
        let context = deno.main_context();
        let isolate = deno.v8_isolate();
        v8::scope_with_context!(let scope, isolate, context);

        let result = {
            v8::tc_scope!(let try_catch, scope);

            // Numeric/named `matches` object: integer keys `0..n` (0 = whole match, 1.. =
            // groups in pattern order) plus a property per named group. No `"$0"`/`"$1"`
            // string keys (so `matches["$1"]` is `undefined`).
            //
            // This is an ordinary object with the normal `Object.prototype` — named groups
            // are own data properties, so `m.toString === "b"` reads the capture (own props
            // shadow inherited ones). A group named after an `Object.prototype` member only
            // matters when that group is ABSENT for a given match: then `m.toString` reads
            // back the inherited method instead of `undefined`. That edge is accepted in
            // exchange for the object behaving like a plain record (inspectable, methods, etc.).
            let matches_object = v8::Object::new(try_catch);
            for (index, capture) in matches.iter().enumerate() {
                let value = v8::String::new(try_catch, &capture.value).unwrap();
                // A numeric-string key (`"0"`, `"1"`, …) is stored by V8 as an array-index
                // property, so it reads back as `matches[0]` / `matches[1]` from JS.
                let key = v8::String::new(try_catch, &index.to_string()).unwrap();
                matches_object.create_data_property(try_catch, key.into(), value.into());
                if let Some(name) = &capture.name {
                    let name_key = v8::String::new(try_catch, name).unwrap();
                    matches_object.create_data_property(try_catch, name_key.into(), value.into());
                }
            }

            let f = v8::Local::new(try_catch, &f);
            let f_this = v8::undefined(try_catch).into();

            let result = f.call(try_catch, f_this, &[matches_object.into()]);

            if try_catch.has_caught() {
                let ex = try_catch.exception().unwrap();
                let exc = ex.to_string(try_catch).unwrap();
                let exc = exc.to_rust_string_lossy(try_catch);
                Ok(ActionResult::Echo(exc))
            } else if let Some(value) = result {
                if value.is_string() {
                    let output = value.to_rust_string_lossy(try_catch);
                    trigger_manager.process_nested_outgoing_line(output.as_str(), depth + 1)?;
                    Ok(ActionResult::None)
                } else {
                    Ok(ActionResult::None)
                }
            } else {
                Ok(ActionResult::None)
            }
        };

        // Restore the enclosing depth (0 at the outermost dispatch) now the handler has returned.
        op_state.borrow_mut().put(ops::EventDepth(prior_depth));
        Self::restore_line_scope(&line_scope, prior_line_scope);

        if let Some(started) = started {
            trace!(
                "Script execution on {} took {:?}",
                matches.first().map_or("unknown", |c| c.value.as_str()),
                started.elapsed()
            );
        }

        result
    }

    pub fn execute_javascript_function(
        &mut self,
        isolate_id: &IsolateId,
        instance: u64,
        function: &v8::Global<v8::Function>,
        args: &[String],
    ) -> Result<ActionResult> {
        // `smudgy_widgets` widget callbacks arrive as a raw v8 handle from the UI thread. The
        // handle is isolate-bound, so we dispatch it into its OWN isolate (`isolate_id`, threaded
        // from the button op via `WidgetIsolate`): a sandboxed package's `onPress` runs in its own
        // isolate, not main, avoiding a cross-isolate handle use. An isolate that
        // has since been dropped surfaces as an `isolate_mut` error, not a crash.
        // The role can also resolve to a LIVE isolate that is not the one that minted the
        // callback: a reload rebuilds every isolate under the same `IsolateId`, and a widget
        // mounted before the reload (or a press already in flight across it) still carries the
        // old instantiation's handle — whose host isolate is disposed, so even materializing a
        // `Local` from it aborts the thread. The instance nonce names the exact instantiation;
        // a mismatch is dropped here, before any v8 access.
        let live = self.isolate_mut(isolate_id)?.instance;
        if live != instance {
            warn!(
                "Dropping widget callback into {isolate_id:?}: minted by isolate instance \
                 {instance}, live instance is {live} (widget outlived an engine rebuild)"
            );
            return Ok(ActionResult::None);
        }
        // Demux: a widget/hotkey callback can schedule async work synchronously; seed the target
        // isolate so the next pump arms it.
        self.mark_isolate_ready(isolate_id);
        // Cloned out before `isolate_mut` borrows `self`; armed below beside `EventDepth`.
        let line_scope = self.line_scope.clone();
        let current_line = self.current_line.clone();
        let deno = self.isolate_mut(isolate_id)?.runtime.deno_runtime();
        // A widget/hotkey callback is a top-level dispatch (depth 0); stamp it so a store `set`
        // inside the callback journals at depth 0 rather than inheriting a stale `EventDepth`
        // from an earlier handler on this isolate. No restore needed — 0 is the between-dispatch
        // baseline the save/restore in the other dispatch paths returns to.
        deno.op_state().borrow_mut().put(ops::EventDepth(0));
        // Arm the current-line scope for this entry (paired restore below): a callback
        // dispatched while a line is in flight may act on that line, one dispatched
        // between lines may not.
        let prior_line_scope = Self::arm_line_scope(&line_scope, &current_line);
        // Make the target isolate current for this callback (Model B), released after the scope.
        let _entered = EnteredIsolate::enter(deno);
        let context = deno.main_context();
        let isolate = deno.v8_isolate();
        v8::scope_with_context!(let scope, isolate, context);

        let result = {
            v8::tc_scope!(let try_catch, scope);
            let function = v8::Local::new(try_catch, function);
            let this = v8::undefined(try_catch).into();
            // Positional args (e.g. a `Markdown` `onLink`'s clicked URL). Script-controlled strings:
            // a string too large for v8 to allocate falls back to `undefined` rather than panicking.
            let call_args: Vec<v8::Local<v8::Value>> = args
                .iter()
                .map(|arg| {
                    v8::String::new(try_catch, arg)
                        .map_or_else(|| v8::undefined(try_catch).into(), Into::into)
                })
                .collect();
            function.call(try_catch, this, &call_args);

            if try_catch.has_caught() {
                let ex = try_catch.exception().unwrap();
                let exc = ex.to_string(try_catch).unwrap();
                let exc = exc.to_rust_string_lossy(try_catch);
                Ok(ActionResult::Echo(exc))
            } else {
                Ok(ActionResult::None)
            }
        };

        Self::restore_line_scope(&line_scope, prior_line_scope);
        result
    }

    /// Run a clicked link's callback: resolve `id` in the target isolate's
    /// [`ops::LinkCallbacks`] registry and call it with the click info. A line can
    /// outlive its engine in scrollback, so every stale form of the address is a
    /// defined SILENT no-op: a gone isolate (package uninstalled), a stale instance
    /// nonce (engine rebuilt — checked first, like
    /// [`Self::execute_javascript_function`], whose v8 choreography this mirrors and
    /// must stay in lockstep with), and an evicted id (the registry is a capped ring).
    pub fn invoke_link_callback(
        &mut self,
        isolate_id: &IsolateId,
        instance: u64,
        id: u64,
        shift: bool,
        ctrl: bool,
        alt: bool,
    ) -> Result<ActionResult> {
        let Ok(bundle) = self.isolate_mut(isolate_id) else {
            warn!(
                "Dropping link callback into {isolate_id:?}: the isolate no longer exists \
                 (the line outlived its package)"
            );
            return Ok(ActionResult::None);
        };
        let live = bundle.instance;
        if live != instance {
            warn!(
                "Dropping link callback into {isolate_id:?}: minted by isolate instance \
                 {instance}, live instance is {live} (the line outlived an engine rebuild)"
            );
            return Ok(ActionResult::None);
        }
        // Demux: a link callback can schedule async work synchronously; seed the
        // target isolate so the next pump arms it.
        self.mark_isolate_ready(isolate_id);
        // Cloned out before `isolate_mut` borrows `self`; armed below beside `EventDepth`.
        let line_scope = self.line_scope.clone();
        let current_line = self.current_line.clone();
        let deno = self.isolate_mut(isolate_id)?.runtime.deno_runtime();
        let function = {
            let op_state = deno.op_state();
            let op_state = op_state.borrow();
            let registry = op_state.borrow::<ops::SharedLinkCallbacks>().clone();
            let function = registry.borrow().get(id).cloned();
            function
        };
        let Some(function) = function else {
            return Ok(ActionResult::None);
        };
        // Top-level dispatch (depth 0), like a widget callback.
        deno.op_state().borrow_mut().put(ops::EventDepth(0));
        // Arm the current-line scope for this entry (paired restore below), like the
        // widget-callback path this mirrors.
        let prior_line_scope = Self::arm_line_scope(&line_scope, &current_line);
        let _entered = EnteredIsolate::enter(deno);
        let context = deno.main_context();
        let isolate = deno.v8_isolate();
        v8::scope_with_context!(let scope, isolate, context);

        let result = {
            v8::tc_scope!(let try_catch, scope);
            let function = v8::Local::new(try_catch, &function);
            let this = v8::undefined(try_catch).into();
            let click = v8::Object::new(try_catch);
            for (key, value) in [("shift", shift), ("ctrl", ctrl), ("alt", alt)] {
                let key = v8::String::new(try_catch, key).unwrap().into();
                let value = v8::Boolean::new(try_catch, value).into();
                click.create_data_property(try_catch, key, value);
            }
            function.call(try_catch, this, &[click.into()]);

            if try_catch.has_caught() {
                let ex = try_catch.exception().unwrap();
                let exc = ex.to_string(try_catch).unwrap();
                let exc = exc.to_rust_string_lossy(try_catch);
                Ok(ActionResult::Echo(exc))
            } else {
                Ok(ActionResult::None)
            }
        };

        Self::restore_line_scope(&line_scope, prior_line_scope);
        result
    }

    /// Deliver a pane-input submission to its registered `onSubmit` handler
    /// (`docs/input.md` §3.7): resolve `function_id` in the target isolate's
    /// `script_functions` and call it with the submitted text. Every stale form of the
    /// address is a defined no-op, in lockstep with [`Self::invoke_link_callback`]: a gone
    /// isolate, a stale instance nonce (engine rebuilt under the pane — the handler died
    /// with its generation and re-registers when the reloaded script re-splits), and an
    /// out-of-range id. The handler fully owns the text — a returned value is ignored,
    /// never sent (unlike the trigger dispatch paths).
    pub fn invoke_pane_input_submit(
        &mut self,
        isolate_id: &IsolateId,
        instance: u64,
        function_id: FunctionId,
        text: &str,
    ) -> Result<ActionResult> {
        let Ok(bundle) = self.isolate_mut(isolate_id) else {
            warn!(
                "Dropping pane-input submission into {isolate_id:?}: the isolate no longer \
                 exists (the pane outlived its package)"
            );
            return Ok(ActionResult::None);
        };
        let live = bundle.instance;
        if live != instance {
            warn!(
                "Dropping pane-input submission into {isolate_id:?}: handler registered by \
                 isolate instance {instance}, live instance is {live} (the pane outlived an \
                 engine rebuild; a re-split re-registers its handler)"
            );
            return Ok(ActionResult::None);
        }
        let function = bundle.script_functions.borrow().get(function_id.0).cloned();
        let Some(function) = function else {
            warn!(
                "Dropping pane-input submission into {isolate_id:?}: no handler at {function_id}"
            );
            return Ok(ActionResult::None);
        };
        // The nonce is verified against the live instantiation above, so the shared
        // choreography (depth-0 stamp, line-scope arm, isolate entry) applies as-is.
        self.execute_javascript_function(isolate_id, instance, &function, &[text.to_string()])
    }

    #[inline]
    pub fn run_script(
        &mut self,
        trigger_manager: &Manager,
        isolate: &IsolateId,
        script_id: ScriptId,
        matches: &Arc<Vec<MatchCapture>>,
        depth: u32,
    ) -> Result<ActionResult> {
        // Per-line timing is TRACE-only; skip the clock entirely above TRACE so the
        // hot path pays just a level check (compiled out when TRACE is statically off).
        let started = log_enabled!(log::Level::Trace).then(Instant::now);

        // Demux: see `call_javascript_function` — string-script dispatch can also schedule async
        // work synchronously, so seed this isolate for the next pump.
        self.mark_isolate_ready(isolate);

        // Cloned out before `isolate_mut` borrows `self`; armed below beside `EventDepth`.
        let line_scope = self.line_scope.clone();
        let current_line = self.current_line.clone();

        let bundle = self.isolate_mut(isolate)?;
        // Get the script before creating the mutable scope to avoid borrowing conflicts
        let script = bundle
            .compiled_scripts
            .get(usize::from(script_id))
            .ok_or_else(|| anyhow::anyhow!("Script {} not found", script_id))?
            .clone();

        let deno = bundle.runtime.deno_runtime();
        // Stamp this eval's dispatch depth (save/restore like `call_javascript_function`): an
        // inline trigger/alias script's store `set` must journal at its own depth, not the stale
        // `EventDepth` a previous function dispatch left in this isolate's `OpState`. Without this
        // an ordinary trigger-driven write loop would ratchet watch deliveries to the cap with no
        // real recursion, and a genuinely nested eval would write at a stale (often 0) depth,
        // defeating the cap it should inherit.
        let op_state = deno.op_state();
        let prior_depth = op_state
            .borrow()
            .try_borrow::<ops::EventDepth>()
            .map_or(0, |d| d.0);
        op_state.borrow_mut().put(ops::EventDepth(depth));
        // Arm the current-line scope for this eval (save/restore like `EventDepth`):
        // an inline trigger body runs for the line in flight; the ambient `line`
        // mutators compare against the generation captured here.
        let prior_line_scope = Self::arm_line_scope(&line_scope, &current_line);
        // Make the owning isolate current for this eval (Model B), released after the scope.
        let _entered = EnteredIsolate::enter(deno);
        let context = deno.main_context();
        let isolate = deno.v8_isolate();
        v8::scope_with_context!(let scope, isolate, context);
        let result = {
            v8::tc_scope!(let try_catch, scope);

            // Numeric/named `matches` object (same shape as the function-handler path):
            // integer keys + named-group properties, no `"$0"`/`"$1"` string keys. An ordinary
            // object with the normal `Object.prototype` — see the comment in
            // `call_javascript_function` for the named-group/prototype interaction.
            let matches_object = v8::Object::new(try_catch);
            for (index, capture) in matches.iter().enumerate() {
                let value = v8::String::new(try_catch, &capture.value).unwrap();
                // A numeric-string key (`"0"`, `"1"`, …) is stored by V8 as an array-index
                // property, so it reads back as `matches[0]` / `matches[1]` from JS.
                let key = v8::String::new(try_catch, &index.to_string()).unwrap();
                matches_object.create_data_property(try_catch, key.into(), value.into());
                if let Some(name) = &capture.name {
                    let name_key = v8::String::new(try_catch, name).unwrap();
                    matches_object.create_data_property(try_catch, name_key.into(), value.into());
                }
            }

            let matches_name = v8::String::new(try_catch, "matches").unwrap();

            try_catch.get_current_context().global(try_catch).set(
                try_catch,
                matches_name.into(),
                matches_object.into(),
            );

            let result = v8::Local::new(try_catch, script).run(try_catch);

            if try_catch.has_caught() {
                let ex = try_catch.exception().unwrap();
                let exc = ex.to_string(try_catch).unwrap();
                let exc = exc.to_rust_string_lossy(try_catch);
                Ok(ActionResult::Echo(exc))
            } else if let Some(value) = result {
                if value.is_string() {
                    let output = value.to_rust_string_lossy(try_catch);
                    trigger_manager.process_nested_outgoing_line(output.as_str(), depth + 1)?;

                    Ok(ActionResult::None)
                } else {
                    Ok(ActionResult::None)
                }
            } else {
                Ok(ActionResult::None)
            }
        };

        // Restore the enclosing depth now the eval has returned (see the save above).
        op_state.borrow_mut().put(ops::EventDepth(prior_depth));
        Self::restore_line_scope(&line_scope, prior_line_scope);

        if let Some(started) = started {
            trace!(
                "Script execution on {} took {:?}",
                matches.first().map_or("unknown", |c| c.value.as_str()),
                started.elapsed()
            );
        }
        result
    }

    pub fn add_script(&mut self, isolate: &IsolateId, source: &str) -> Result<ScriptId> {
        // Inline alias/trigger scripts (disk-authored JS) are classic scripts that run in
        // the shared global scope. The creation functions are not globals (ESM modules import
        // them from `smudgy:core`), so inject the user-bound creation API as a lexical scope via
        // `with`. This keeps `createAlias`/`createTrigger` working for
        // inline scripts — attributed to the user namespace — while keeping them off
        // `globalThis`, and (unlike a function wrapper) preserves the script's completion
        // value, which `run_script` forwards as auto-sent output. Compiled into (and indexed
        // by) the target isolate's own `compiled_scripts`.
        let wrapped = format!("with (globalThis.__smudgy_user_api) {{\n{source}\n}}");
        let bundle = self.isolate_mut(isolate)?;
        let script = compile_javascript(bundle.runtime.deno_runtime(), &wrapped)?;
        let script_id = ScriptId(bundle.compiled_scripts.len());
        bundle.compiled_scripts.push(script);
        Ok(script_id)
    }

    pub fn set_is_captured(&mut self, isolate: &IsolateId, value: bool) {
        // `Capture` is per-isolate `OpState`: `op_smudgy_capture` writes the *calling*
        // isolate's flag, so the get/set bracket in dispatch must target the isolate that
        // ran the script. A missing isolate never happens for a live action (the only ids
        // that reach here came from a constructed isolate); the `debug_assert` makes a
        // future routing/lifecycle bug fail loud in tests instead of silently no-op'ing.
        debug_assert!(
            self.isolates.contains_key(isolate),
            "set_is_captured on unknown isolate {isolate:?}"
        );
        let Ok(bundle) = self.isolate_mut(isolate) else {
            return;
        };
        let state = bundle.runtime.deno_runtime().op_state();
        let mut guard = state.borrow_mut();
        let captured = guard.borrow_mut::<Capture>();
        captured.0 = value;
    }

    pub fn get_is_captured(&mut self, isolate: &IsolateId) -> bool {
        // See `set_is_captured`. A missing isolate returning `false` here would silently
        // un-suppress a line a capturing script meant to swallow, so fail loud in debug.
        debug_assert!(
            self.isolates.contains_key(isolate),
            "get_is_captured on unknown isolate {isolate:?}"
        );
        let Ok(bundle) = self.isolate_mut(isolate) else {
            return false;
        };
        let state = bundle.runtime.deno_runtime().op_state();
        let guard = state.borrow();
        let captured = guard.borrow::<Capture>();
        captured.0
    }

    /// Enter a synchronous alias/trigger function handler with its declarative default.
    pub fn begin_fallthrough(&mut self, isolate: &IsolateId, value: bool) {
        debug_assert!(
            self.isolates.contains_key(isolate),
            "begin_fallthrough on unknown isolate {isolate:?}"
        );
        let Ok(bundle) = self.isolate_mut(isolate) else {
            return;
        };
        let state = bundle.runtime.deno_runtime().op_state();
        state.borrow_mut().borrow_mut::<Fallthrough>().0 = Some(value);
    }

    /// Leave the current function handler and return its final fallthrough decision. Clearing the
    /// slot is what makes calls from top-level or later async continuations throw.
    pub fn end_fallthrough(&mut self, isolate: &IsolateId) -> bool {
        debug_assert!(
            self.isolates.contains_key(isolate),
            "end_fallthrough on unknown isolate {isolate:?}"
        );
        let Ok(bundle) = self.isolate_mut(isolate) else {
            return true;
        };
        let state = bundle.runtime.deno_runtime().op_state();
        state
            .borrow_mut()
            .borrow_mut::<Fallthrough>()
            .0
            .take()
            .unwrap_or(true)
    }
}

/// RAII guard that makes `runtime`'s v8 isolate the thread's *current* isolate for the guard's
/// lifetime, restoring the previously-entered one on drop. `rusty_v8` enters an isolate when its
/// `OwnedIsolate` is constructed and keeps it entered until dropped, so with more than one isolate
/// on the session thread (`PACKAGE-ISOLATES.md`) the one we want to run JS in is usually *not*
/// current — and `ContextScope::new` panics unless the scope's isolate is the current one. Every
/// v8 entry point (`call_javascript_function`, `run_script`, the event-loop pump, compilation, …)
/// brackets its work with this. The raw pointer stays valid for the guard's lifetime because the
/// caller keeps the owning `JsRuntime` borrowed across it.
struct EnteredIsolate(*mut v8::OwnedIsolate);

impl EnteredIsolate {
    fn enter(runtime: &mut deno_core::JsRuntime) -> Self {
        let isolate: *mut v8::OwnedIsolate = runtime.v8_isolate();
        // SAFETY: `isolate` is a valid, live isolate owned by `runtime`; `enter`/`exit` are
        // balanced by this guard's `Drop`. Re-entering an already-current isolate is allowed.
        unsafe {
            (*isolate).enter();
        }
        Self(isolate)
    }
}

impl Drop for EnteredIsolate {
    fn drop(&mut self) {
        // SAFETY: balanced with `enter`; nothing entered after this guard is still entered (v8
        // entry points bracket their work, never interleaving isolates), so this is the current
        // isolate, as `exit` requires.
        unsafe {
            (*self.0).exit();
        }
    }
}

/// Construct one isolate's `ScriptRuntime` (its own `MainWorker` + module loader). Shared by
/// the trusted main isolate and each sandboxed package isolate; the caller picks the extension
/// set (which carries the `IsolateId`), the data dir, whether to arm an inspector, the loader's
/// package provider, the permission container, and the `import` policy. The main isolate passes
/// `permissions: None` (allow-all) and `ImportPolicy::Any` (imports freely); a sandboxed package
/// isolate passes `Some(restricted)` built from its closure's manifest-permission union plus its
/// consented `import` level, so the module loader gates remote imports to that level
/// (`PACKAGE-ISOLATES-ENFORCEMENT.md`).
fn build_script_runtime(
    extensions: Vec<deno_core::Extension>,
    data_dir: std::path::PathBuf,
    webstorage_dir: Option<std::path::PathBuf>,
    inspector: Option<InspectorConfig>,
    package_provider: Option<Rc<dyn PackageProvider>>,
    permissions: Option<PermissionsContainer>,
    import_policy: ImportPolicy,
    tokio_runtime: Rc<tokio::runtime::Runtime>,
) -> Result<ScriptRuntime> {
    let mut runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions,
        // `allow_https` permits the http/https module SCHEME at all; the per-isolate `import_policy`
        // is the actual gate on WHICH external code may be downloaded (npm/jsr/arbitrary web).
        module_policy: ModulePolicy {
            allow_https: true,
            import_policy,
        },
        inspector,
        tokio: tokio_runtime,
        package_provider,
        permissions,
        data_dir,
        webstorage_dir,
    })?;
    // rusty_v8 enters the isolate when its `OwnedIsolate` is constructed and leaves it current.
    // With an isolate *set* on one thread that would make every isolate but the last "current"
    // out of order, so leave the enter-stack empty between operations: exit the just-built
    // isolate now. Every later v8 op re-enters it via [`EnteredIsolate`], and teardown enters it
    // once more right before dropping (see `Drop for ScriptEngine`). This is the "Model B"
    // lifecycle validated for deno_core 0.395 / v8 147 — teardown is order-independent.
    // SAFETY: balances the construct-time enter; the new isolate is the current one here.
    unsafe {
        runtime.deno_runtime().v8_isolate().exit();
    }
    Ok(runtime)
}

/// Max chars of a slug's human-readable prefix. The [`slug_hash`] suffix guarantees uniqueness, so
/// the readable part is purely cosmetic and is capped to keep on-disk paths within `MAX_PATH`.
const MAX_READABLE_SLUG: usize = 80;

/// The lossy, filesystem-safe, human-readable part of a slug: the `-`-joined components with any
/// char outside `[ascii-alnum, - . _]` flattened to `_`, length-capped. NOT injective — nicknames
/// and names may contain `-`, and names may contain non-ASCII that flattens — so it is always
/// paired with a [`slug_hash`].
fn readable_slug(joined: &str) -> String {
    joined
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_READABLE_SLUG)
        .collect()
}

/// A stable, collision-resistant hash (first 128 bits of SHA-256, as hex) of the exact,
/// UN-sanitized slug components, NUL-joined (a byte no component can contain) so the pre-hash
/// encoding is injective. Appended to a [`readable_slug`] so two distinct packages never share a
/// dir even when their readable slugs collide (e.g. `owner="a-b", name="c"` vs `owner="a",
/// name="b-c"`) — and, being cryptographic, a crafted-nickname attacker cannot force a collision to
/// hijack another package's persisted storage. Stable across client builds, since SHA-256 is fixed.
fn slug_hash(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update([0u8]);
        }
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The `<server>/.isolates/` path component naming one sandboxed package isolate's scratch dir, per
/// (owner, name, version) so each package version's `cache`/`node_modules` is its own. A readable
/// (lossy) prefix plus a collision-resistant [`slug_hash`] of the exact components, so distinct
/// packages never collide even though nicknames, names, and versions may all contain `-`. The
/// orphan sweep ([`prune_orphan_isolate_dirs`]) rebuilds this exact slug for its keep-set, so the
/// two must never drift — hence a single source of truth.
fn isolate_slug(owner: &str, name: &str, version: &str) -> String {
    format!(
        "{}-{}",
        readable_slug(&format!("{owner}-{name}-{version}")),
        slug_hash(&[owner, name, version])
    )
}

/// The data dir for a sandboxed package isolate — a per-(owner, name, version) subdir under the
/// server dir holding that version's `cache`/`node_modules` (version-specific). Persistent
/// `webstorage`/`localStorage` lives elsewhere — see [`sandbox_storage_dir`], keyed WITHOUT the
/// version so it survives updates.
fn sandbox_data_dir(
    server_path: &std::path::Path,
    owner: &str,
    name: &str,
    version: &str,
) -> std::path::PathBuf {
    server_path
        .join(".isolates")
        .join(isolate_slug(owner, name, version))
}

/// The `<server>/.isolate-storage/` component naming a sandboxed package's PERSISTENT storage —
/// per (owner, name), deliberately WITHOUT the version, so a package's `localStorage`/`vars`
/// survive its own updates (each update lands in a fresh per-version [`sandbox_data_dir`]). The
/// orphan sweep ([`prune_orphan_isolate_storage_dirs`]) rebuilds this exact slug for its keep-set,
/// so the two must never drift.
fn sandbox_storage_slug(owner: &str, name: &str) -> String {
    format!(
        "{}-{}",
        readable_slug(&format!("{owner}-{name}")),
        slug_hash(&[owner, name])
    )
}

/// A sandboxed package's PERSISTENT, per-(owner, name) storage root: `<server>/.isolate-storage/
/// <slug>`, where `<slug>` is [`sandbox_storage_slug`] — a readable `owner-name` prefix PLUS a
/// collision-resistant hash suffix, so the component is NOT literally `owner-name` (distinct
/// packages never share this root even when their readable names collide). Version-independent, so
/// everything under it survives the package's own updates; the orphan sweep
/// ([`prune_orphan_isolate_storage_dirs`]) reclaims it only when the package leaves the lockfile.
/// Its children are [`sandbox_storage_dir`] (`webstorage`) and [`sandbox_fs_data_dir`] (`data`).
fn sandbox_storage_root(
    server_path: &std::path::Path,
    owner: &str,
    name: &str,
) -> std::path::PathBuf {
    server_path
        .join(".isolate-storage")
        .join(sandbox_storage_slug(owner, name))
}

/// The persistent Web Storage (`localStorage`) origin dir for a sandboxed package isolate: the
/// `webstorage` child of [`sandbox_storage_root`]. Version-independent; version-specific
/// `cache`/`node_modules` stay under [`sandbox_data_dir`]. `SQLite` locking makes the store safe
/// across concurrent same-server sessions, exactly as the main isolate's `<server>/webstorage` is.
fn sandbox_storage_dir(
    server_path: &std::path::Path,
    owner: &str,
    name: &str,
) -> std::path::PathBuf {
    sandbox_storage_root(server_path, owner, name).join("webstorage")
}

/// The `$DATA` filesystem dir a sandboxed package's `read`/`write` grants resolve to: the `data`
/// child of [`sandbox_storage_root`] (a sibling of `webstorage`). The package's OWN private,
/// update-surviving data dir — a `$DATA` grant reaches only these files, never the shared server
/// root or another package.
fn sandbox_fs_data_dir(
    server_path: &std::path::Path,
    owner: &str,
    name: &str,
) -> std::path::PathBuf {
    sandbox_storage_root(server_path, owner, name).join("data")
}

/// Removes `<server>/.isolates/<slug>` scratch dirs whose package version no longer has any
/// installed or live copy — a version that was uninstalled, or superseded by a newer one.
/// `keep` holds the slug of every live sandboxed isolate **plus** every still-installed package
/// version recorded in the lockfile (enabled or disabled), so an installed-but-disabled
/// package's persisted `webstorage` is never destroyed — only genuinely orphaned versions are.
///
/// Best-effort, like [`prune_orphan_typings`]: a missing root is a no-op and a dir held open by
/// a concurrent same-server session (a Windows file lock) simply survives to the next sweep
/// rather than aborting session start.
fn prune_orphan_isolate_dirs(
    server_path: &std::path::Path,
    keep: &std::collections::HashSet<String>,
) {
    let Ok(entries) = fs::read_dir(server_path.join(".isolates")) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let slug = entry.file_name();
        if !keep.contains(&*slug.to_string_lossy()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes `<server>/.isolate-storage/<slug>` persistent-storage dirs for packages no longer
/// installed at any version (fully uninstalled). Unlike [`prune_orphan_isolate_dirs`], the keep-set
/// is per (owner, name): a package's persisted `localStorage` is kept across version changes and
/// only reclaimed when the package leaves the lockfile entirely. Best-effort.
fn prune_orphan_isolate_storage_dirs(
    server_path: &std::path::Path,
    keep: &std::collections::HashSet<String>,
) {
    let Ok(entries) = fs::read_dir(server_path.join(".isolate-storage")) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if !keep.contains(&*entry.file_name().to_string_lossy()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The enforced permission set for a LOCAL dev-override package: its own `smudgy.package.json`
/// manifest permissions, read from disk. A local package has no consent record — the author edits
/// the manifest to grant capabilities (the manifest IS the grant table) and reloads to test the
/// exact sandbox an installer will get. A missing/unreadable manifest yields the empty union
/// (deny-all), the safe default; the author can trust the package to develop allow-all instead.
fn local_manifest_permissions(server_name: &str, name: &str) -> PackagePermissions {
    crate::models::local_packages::load_local_package(server_name, name)
        .ok()
        .flatten()
        .map(|pkg| pkg.manifest.permissions)
        .unwrap_or_default()
}

/// Build the restricted [`PermissionsContainer`] for a sandboxed package isolate from its
/// closure's deno-native permission `union` (`PACKAGE-ISOLATES-ENFORCEMENT.md`). `data_dir`
/// is the dir the `$DATA` placeholder in `read`/`write` entries expands to — the package's OWN
/// private, update-surviving data dir ([`sandbox_fs_data_dir`]), so a `$DATA` grant reaches only
/// that package's files, never the shared server root or another package.
///
/// # Errors
/// Returns an error if `deno_permissions` rejects a descriptor (e.g. a malformed `net`
/// `host:port`, an unknown `sys` kind, or an empty `run` program name); the caller skips the
/// package rather than run it ungated.
fn build_restricted_container(
    union: &PackagePermissions,
    data_dir: &std::path::Path,
) -> Result<PermissionsContainer> {
    let opts = PermissionsOptions {
        allow_net: to_allow_list(union.net.clone()),
        allow_read: to_allow_list(expand_data_paths(&union.read, data_dir)),
        allow_write: to_allow_list(expand_data_paths(&union.write, data_dir)),
        allow_env: to_allow_list(union.env.clone()),
        // `run`/`ffi` are sandbox escapes (a subprocess / native library runs outside the
        // permission model entirely) and are only ever granted through explicit consent — the
        // consent window presents any entry here as effectively full access. A bare `run`
        // program name is PATH-resolved by `from_options`; an unresolvable one is logged and
        // dropped (denied), never granted broadly. `ffi` paths get the same `$DATA` expansion
        // (and `..`-escape drop) as `read`/`write`.
        allow_run: to_allow_list(union.run.clone()),
        allow_ffi: to_allow_list(expand_data_paths(&union.ffi, data_dir)),
        // `sys` kinds are validated by `from_options`; an unknown token errors and the caller
        // skips the package (fail-closed) rather than running it ungated.
        allow_sys: to_allow_list(union.sys.clone()),
        // `import` is NOT set here: deno's `allow_import` is inert in this stack (smudgy's module
        // loader fetches remote imports itself and never consults this container). The live gate is
        // the loader's `ImportPolicy` (`union.import`, threaded via `ModulePolicy::import_policy`).
        // No interactive prompter is wired; `prompt:true` would hang the session thread on the
        // first denied access. With `prompt:false` a denied check fails fast (`NotCapable`).
        prompt: false,
        ..Default::default()
    };
    let parser = permission_descriptor_parser();
    let perms = Permissions::from_options(&*parser, &opts)?;
    Ok(PermissionsContainer::new(parser, perms))
}

/// Map an allowlist into `deno_permissions`' `Option<Vec<String>>`. **An empty list becomes
/// `None`, not `Some(vec![])`** — in `deno_permissions` 0.101 `Some(vec![])` sets
/// `granted_global = true` (the bare `--allow-net` semantic = **allow all**), the opposite of
/// the deny-by-default this enforces. `None` (no global grant, no descriptors, `prompt:false`)
/// denies the kind entirely; a non-empty list scopes the grant to exactly those entries.
fn to_allow_list(entries: Vec<String>) -> Option<Vec<String>> {
    (!entries.is_empty()).then_some(entries)
}

/// Expand the `$DATA` placeholder in `read`/`write`/`ffi` path entries to the package's absolute
/// data dir before they reach `deno_permissions` (`PACKAGE-ISOLATES-ENFORCEMENT.md`).
/// Entries whose `$DATA` grant would escape the data dir are dropped (see below).
fn expand_data_paths(entries: &[String], data_dir: &std::path::Path) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| expand_data_placeholder(entry, data_dir))
        .collect()
}

/// Expand a single `$DATA` entry: bare `$DATA` → `data_dir`, a `$DATA/<sub>` (or `$DATA\<sub>`)
/// prefix → `data_dir/<sub>`. An entry not starting with the `$DATA` placeholder (incl. a
/// look-alike like `$DATABASE`) is taken as-is — an already-absolute path the author declared.
/// Intentionally tiny: not a templating language.
///
/// **Containment guardrail:** a `$DATA` subpath containing a `..` component is **dropped**
/// (returns `None`) and logged — a `$DATA` grant must stay within the data dir, and `..` would
/// let a manifest escape it (e.g. `$DATA/../../etc`). Dropping it leaves that path denied. A
/// non-placeholder absolute path is the author's own explicit declaration and is left untouched.
fn expand_data_placeholder(entry: &str, data_dir: &std::path::Path) -> Option<String> {
    let Some(rest) = entry.strip_prefix("$DATA") else {
        return Some(entry.to_string());
    };
    let sub = match rest.chars().next() {
        None => "", // bare `$DATA`
        Some('/' | '\\') => rest.trim_start_matches(['/', '\\']),
        Some(_) => return Some(entry.to_string()), // e.g. `$DATABASE` — not the placeholder
    };
    if sub.split(['/', '\\']).any(|component| component == "..") {
        warn!(
            "dropping package permission entry {entry:?}: a $DATA grant may not escape the data dir with '..'"
        );
        return None;
    }
    Some(if sub.is_empty() {
        data_dir.to_string_lossy().into_owned()
    } else {
        data_dir.join(sub).to_string_lossy().into_owned()
    })
}

/// Fold a just-loaded isolate's [`LoadReport`] into the running session-echo tallies: count
/// local module files and collect each package as `name@version` (or bare `name` when the
/// resolved version is unavailable). Used for both the main and sandboxed isolate loads.
fn fold_load_report(report: &LoadReport, local_count: &mut usize, package_lines: &mut Vec<String>) {
    for module in &report.modules {
        match module.kind {
            LoadedModuleKind::LocalFile => *local_count += 1,
            LoadedModuleKind::Package => {
                let name = SmudgySpecifier::parse(&module.specifier)
                    .map_or_else(|_| module.specifier.clone(), |spec| spec.name);
                package_lines.push(match &module.package {
                    Some(pkg) => format!("{name}@{}", pkg.resolved_version),
                    None => name,
                });
            }
        }
    }
}

/// Render a script-engine error into a compact, user-facing message for the session view.
///
/// A raw `CoreError`'s `Debug` (`CoreError(Js(JsError {{ … }}))`) and even its `Display` (a V8
/// stack littered with smudgy's own `ext:smudgy_ops/…` glue frames) are noise to someone who
/// just wants to know *which of their packages broke and where*. So a JS exception gets the
/// ergonomic treatment — the error class + message, attributed to the offending package/module,
/// with smudgy-internal frames filtered out — while every other error kind keeps its own
/// already-readable `Display`.
pub(crate) fn format_script_error(err: &CoreError) -> String {
    match &*err.0 {
        CoreErrorKind::Js(js) => format_js_error(js),
        _ => err.to_string(),
    }
}

/// Frames in smudgy's own runtime glue (the ops shim, the mapper shim) and deno/V8 internals
/// are never where a user's bug lives, so the ergonomic formatter hides them.
fn is_internal_frame(file_name: &str) -> bool {
    file_name.starts_with("ext:")
        || file_name.starts_with("node:")
        || file_name.starts_with("core:")
        || file_name.starts_with("checkin:")
}

/// The trailing file component of a path or URL (`smudgy-pkg:///a/b/1.0/lib/x.ts` is handled by
/// the caller via [`parse_canonical`]; this is the fallback for plain paths / `file://` URLs).
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

/// The package/module a stack frame belongs to, for headline attribution: a package frame
/// becomes `name@version`; anything else (a local module, a user script) becomes its file name.
fn frame_origin(frame: &JsStackFrame) -> Option<String> {
    let file = frame.file_name.as_deref()?;
    if let Ok(url) = deno_core::ModuleSpecifier::parse(file)
        && let Some(coords) = parse_canonical(&url)
    {
        return Some(format!("{}@{}", coords.key.name, coords.version));
    }
    Some(basename(file).to_string())
}

/// A concise `file:line:col` location for a stack frame's "at …" line. The package is already
/// named in the headline, so a package frame shows only its module file (e.g. `index.ts:22:1`).
fn frame_short_location(frame: &JsStackFrame) -> Option<String> {
    let file = frame.file_name.as_deref()?;
    let name = match deno_core::ModuleSpecifier::parse(file) {
        Ok(url) => parse_canonical(&url).map_or_else(
            || basename(file).to_string(),
            |c| basename(&c.module_subpath).to_string(),
        ),
        Err(_) => basename(file).to_string(),
    };
    let loc = match frame.line_number.filter(|&l| l > 0) {
        Some(line) => match frame.column_number.filter(|&c| c > 0) {
            Some(col) => format!("{name}:{line}:{col}"),
            None => format!("{name}:{line}"),
        },
        None => name,
    };
    Some(loc)
}

/// Strip V8's `Uncaught (in promise) {Class}: ` boilerplate from an exception message, leaving
/// just the human message. Only used when the structured `message` field is absent.
fn strip_exception_boilerplate(exception_message: &str, class: &str) -> String {
    let s = exception_message
        .trim_start_matches("Uncaught ")
        .trim_start_matches("(in promise) ");
    s.strip_prefix(&format!("{class}: "))
        .unwrap_or(s)
        .to_string()
}

fn format_js_error(js: &JsError) -> String {
    let class = js
        .name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("Error");
    let message = js.message.as_deref().filter(|m| !m.is_empty()).map_or_else(
        || strip_exception_boilerplate(&js.exception_message, class),
        str::to_string,
    );
    let uncaught = js.exception_message.contains("Uncaught");

    // A user frame has a non-empty file name that isn't smudgy/deno internal glue. The
    // non-empty guard keeps a stray frame (V8 can omit a file name) from yielding a blank
    // origin or a dangling "at " line.
    let user_frames: Vec<&JsStackFrame> = js
        .frames
        .iter()
        .filter(|f| {
            f.file_name
                .as_deref()
                .is_some_and(|n| !n.is_empty() && !is_internal_frame(n))
        })
        .collect();

    let origin = user_frames
        .first()
        .and_then(|f| frame_origin(f))
        .unwrap_or_else(|| "A script".to_string());

    let headline = if uncaught {
        format!("{origin} \u{2014} uncaught {class}:")
    } else {
        format!("{origin} \u{2014} {class}:")
    };

    let mut lines = vec![headline, format!("  {message}")];
    for frame in user_frames.iter().take(8) {
        if let Some(loc) = frame_short_location(frame) {
            lines.push(format!("  at {loc}"));
        }
    }
    // A stack made entirely of smudgy-internal frames means the fault isn't in user code; keep
    // the topmost raw location so the diagnostic isn't lost to the filter.
    if user_frames.is_empty()
        && let Some(frame) = js.frames.first()
        && let Some(file) = frame.file_name.as_deref().filter(|s| !s.is_empty())
    {
        lines.push(format!("  at {file}:{}", frame.line_number.unwrap_or(0)));
    }
    lines.join("\n")
}

fn compile_javascript(
    runtime: &mut deno_core::JsRuntime,
    source: &str,
) -> Result<v8::Global<v8::Script>> {
    // The target isolate usually isn't current (Model B); make it so for the compile, released
    // on return. `add_script` can run mid-session when another isolate is the last-built one.
    let _entered = EnteredIsolate::enter(runtime);
    let context = runtime.main_context();
    let isolate = runtime.v8_isolate();
    v8::scope_with_context!(let scope, isolate, context);
    let v8_script_source =
        v8::String::new_from_utf8(scope, source.as_bytes(), v8::NewStringType::Normal).unwrap();

    v8::tc_scope!(let try_catch, scope);

    if let Some(unbound_script) = v8::script_compiler::compile_unbound_script(
        try_catch,
        &mut Source::new(v8_script_source, None),
        v8::script_compiler::CompileOptions::NoCompileOptions,
        v8::script_compiler::NoCacheReason::BecauseV8Extension,
    ) {
        let bound_script = unbound_script.bind_to_current_context(try_catch);

        Ok(Global::new(try_catch, bound_script))
    } else if let Some(message) = try_catch.message() {
        Err(anyhow!(
            "Failed to compile script: {}:{} {}",
            message
                .get_script_resource_name(try_catch)
                .map_or("[unknown script]".to_string(), |resource| resource
                    .to_rust_string_lossy(try_catch)),
            message.get_line_number(try_catch).unwrap_or(0),
            try_catch
                .exception()
                .map(|e| e.to_string(try_catch))
                .map_or(Some("[unknown error]".to_string()), |e| e
                    .map(|e| e.to_rust_string_lossy(try_catch)))
                .unwrap_or("[unknown error]".to_string())
        ))
    } else {
        Err(anyhow!("Failed to compile script: unknown error"))
    }
}

#[cfg(test)]
mod demux_tests {
    use super::{ParentSlot, ReadySet, build_demux_waker};
    use crate::session::runtime::IsolateId;
    use rustc_hash::FxHashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Wake, Waker};

    /// A parent waker that records that it was woken (stands in for the session task).
    struct FlagWaker(Arc<AtomicBool>);
    impl Wake for FlagWaker {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn demux_waker_records_id_and_wakes_parent() {
        let ready: ReadySet = Arc::new(Mutex::new(FxHashSet::default()));
        let parent: ParentSlot = Arc::new(Mutex::new(None));
        let woke = Arc::new(AtomicBool::new(false));
        *parent.lock().unwrap() = Some(Waker::from(Arc::new(FlagWaker(woke.clone()))));

        let waker = build_demux_waker(IsolateId::Main, &ready, &parent);
        waker.wake_by_ref();

        assert!(
            ready.lock().unwrap().contains(&IsolateId::Main),
            "wake must record the isolate id into the ready-set"
        );
        assert!(
            woke.load(Ordering::SeqCst),
            "wake must re-arm the stored parent task"
        );
    }

    #[test]
    fn demux_waker_with_no_parent_still_records_id() {
        // A wake with no parent parked (slot is `None`) must not panic and must still record the
        // id, so the next pump (which re-stores the parent first) catches it.
        let ready: ReadySet = Arc::new(Mutex::new(FxHashSet::default()));
        let parent: ParentSlot = Arc::new(Mutex::new(None));

        let waker = build_demux_waker(IsolateId::Main, &ready, &parent);
        waker.wake();

        assert!(ready.lock().unwrap().contains(&IsolateId::Main));
    }

    #[test]
    fn demux_waker_dedups_repeated_wakes() {
        // Several completions on one isolate before a pump collapse to a single ready-set entry.
        let ready: ReadySet = Arc::new(Mutex::new(FxHashSet::default()));
        let parent: ParentSlot = Arc::new(Mutex::new(None));

        let waker = build_demux_waker(IsolateId::Main, &ready, &parent);
        waker.wake_by_ref();
        waker.wake_by_ref();
        waker.wake_by_ref();

        assert_eq!(
            ready.lock().unwrap().len(),
            1,
            "HashSet dedups repeated wakes on the same isolate"
        );
    }
}

#[cfg(test)]
mod error_format_tests {
    use super::format_script_error;
    use deno_core::error::{CoreError, CoreErrorKind, JsError, JsStackFrame};

    fn frame(file: &str, func: Option<&str>, line: i64, col: i64) -> JsStackFrame {
        JsStackFrame {
            type_name: None,
            function_name: func.map(str::to_string),
            method_name: None,
            file_name: Some(file.to_string()),
            line_number: Some(line),
            column_number: Some(col),
            eval_origin: None,
            is_top_level: Some(true),
            is_eval: false,
            is_native: false,
            is_constructor: false,
            is_async: false,
            is_promise_all: false,
            is_wasm: false,
            promise_index: None,
        }
    }

    fn js_error(
        name: &str,
        message: &str,
        exception_message: &str,
        frames: Vec<JsStackFrame>,
    ) -> CoreError {
        let err = JsError {
            name: Some(name.to_string()),
            message: Some(message.to_string()),
            stack: None,
            cause: None,
            exception_message: exception_message.to_string(),
            frames,
            source_line: None,
            source_line_frame_index: None,
            aggregated: None,
            additional_properties: Vec::new(),
        };
        // The tuple field is public; build the boxed kind directly.
        CoreError(Box::new(CoreErrorKind::Js(Box::new(err))))
    }

    #[test]
    fn package_top_level_throw_is_attributed_and_internal_frames_hidden() {
        // Mirrors the real arctic-prompt failure: the user frame is the package's own
        // index.ts, shadowed by smudgy's `ext:` op-shim frames that must be filtered out.
        let err = js_error(
            "TypeError",
            "Name must be a non-empty string using only alphanumeric characters and underscores",
            "Uncaught (in promise) TypeError: Name must be a non-empty string using only alphanumeric characters and underscores",
            vec![
                frame(
                    "ext:smudgy_ops/smudgy.ts",
                    Some("validateCreateTriggerParams"),
                    527,
                    15,
                ),
                frame("ext:smudgy_ops/smudgy.ts", Some("createTrigger"), 473, 20),
                frame(
                    "smudgy-pkg:///kapusniak/arctic-prompt/1.0.5/index.ts",
                    None,
                    22,
                    1,
                ),
            ],
        );
        assert_eq!(
            format_script_error(&err),
            "arctic-prompt@1.0.5 \u{2014} uncaught TypeError:\n  \
             Name must be a non-empty string using only alphanumeric characters and underscores\n  \
             at index.ts:22:1"
        );
    }

    #[test]
    fn synchronous_throw_omits_the_uncaught_word() {
        let err = js_error(
            "RangeError",
            "out of range",
            "RangeError: out of range",
            vec![frame(
                "smudgy-pkg:///wbk/util/2.0.0/lib/math.ts",
                Some("clamp"),
                9,
                3,
            )],
        );
        assert_eq!(
            format_script_error(&err),
            "util@2.0.0 \u{2014} RangeError:\n  out of range\n  at math.ts:9:3"
        );
    }

    #[test]
    fn empty_file_name_frames_do_not_produce_blank_origin_or_dangling_location() {
        // V8 can emit a frame with no file name; it must not leak a blank origin (" — uncaught
        // …") or a dangling "  at " line. Both the user-frame filter and the last-resort fallback
        // skip empty names, so the headline + message stand alone.
        let err = js_error(
            "Error",
            "boom",
            "Uncaught Error: boom",
            vec![frame("", None, 0, 0)],
        );
        assert_eq!(
            format_script_error(&err),
            "A script \u{2014} uncaught Error:\n  boom"
        );
    }

    #[test]
    fn falls_back_to_a_script_when_only_internal_frames() {
        let err = js_error(
            "Error",
            "boom",
            "Uncaught Error: boom",
            vec![frame("ext:smudgy_ops/smudgy.ts", Some("op"), 5, 2)],
        );
        let out = format_script_error(&err);
        assert!(out.starts_with("A script \u{2014} uncaught Error:\n  boom"));
        // The internal frame is kept as a last resort so the diagnostic isn't lost entirely.
        assert!(out.contains("at ext:smudgy_ops/smudgy.ts:5"), "got: {out}");
    }
}

#[cfg(test)]
mod local_typings_tests {
    use super::local_package_types;
    use crate::models::local_packages::{LocalModule, LocalPackage};
    use smudgy_script::PackageManifest;

    fn local(manifest: &str, modules: Vec<LocalModule>) -> LocalPackage {
        LocalPackage {
            name: "arctic-prompt".to_string(),
            manifest: PackageManifest::parse(manifest).expect("valid manifest"),
            readme: None,
            modules,
        }
    }

    fn module(subpath: &str, content: &str) -> LocalModule {
        LocalModule {
            subpath: subpath.to_string(),
            content: content.as_bytes().to_vec(),
        }
    }

    /// A local dev-override package's typings come from the LIVE entry source: handles are
    /// extracted from the folder's declarations (not any cached published copy) and the
    /// result is marked `local` so the generated paths point at the folder.
    #[test]
    fn types_handles_from_the_live_entry_source() {
        let pkg = local(
            r#"{ "version": "2.0.0", "entry": "index.ts" }"#,
            vec![module(
                "index.ts",
                "import { createState, createEvent } from \"smudgy:core\";\n\
                 export interface PromptData { hp: number }\n\
                 const promptState = createState<PromptData>('prompt');\n\
                 export type PromptState = typeof promptState;\n\
                 const promptEvent = createEvent<PromptData>('prompt');\n\
                 export type PromptEvent = typeof promptEvent;\n",
            )],
        );

        let types = local_package_types("kapusniak", &pkg).expect("typed");
        assert!(types.local);
        assert_eq!(types.owner, "kapusniak");
        assert_eq!(types.name, "arctic-prompt");
        assert_eq!(types.entry_module, "index.ts");
        assert_eq!(types.handles.len(), 2);
        assert_eq!(types.handles[0].type_alias.as_deref(), Some("PromptState"));
        assert_eq!(types.handles[1].type_alias.as_deref(), Some("PromptEvent"));
    }

    /// No manifest entry: the loader's entry candidates apply, same as the resolver.
    #[test]
    fn falls_back_to_entry_candidates_without_a_manifest_entry() {
        let pkg = local(
            r#"{ "version": "1.0.0" }"#,
            vec![module("index.tsx", "export {};\n")],
        );
        let types = local_package_types("local", &pkg).expect("typed");
        assert_eq!(types.entry_module, "index.tsx");
        assert!(types.handles.is_empty());
    }

    /// A folder with no resolvable entry module yields nothing to type (parity with the
    /// materialized path's is-file gate).
    #[test]
    fn yields_none_without_a_resolvable_entry() {
        let pkg = local(
            r#"{ "version": "1.0.0", "entry": "index.ts" }"#,
            vec![module("helper.ts", "export {};\n")],
        );
        assert!(local_package_types("local", &pkg).is_none());
    }
}

#[cfg(test)]
mod isolate_dir_tests {
    use super::{
        isolate_slug, prune_orphan_isolate_dirs, sandbox_data_dir, sandbox_storage_dir,
        sandbox_storage_slug,
    };
    use std::collections::HashSet;
    use std::fs;

    /// The cleanup keep-set rebuilds slugs with `isolate_slug`; it MUST equal the final path
    /// component `sandbox_data_dir` writes, or the sweep would delete live dirs. This guards
    /// the two against drifting (including the path-hostile-char flattening).
    #[test]
    fn slug_matches_sandbox_data_dir_component() {
        let root = std::path::Path::new("C:/smudgy/Arctic");
        for (owner, name, version) in [
            ("wbk", "mapper", "1.2.3"),
            ("local", "my-pkg", "0.1.0-dev"),
            ("o/w", "na:me", "1.0.0+build"), // path-hostile chars flatten to '_'
        ] {
            let dir = sandbox_data_dir(root, owner, name, version);
            let component = dir.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(component, isolate_slug(owner, name, version));
        }
        // The hostile-char case flattens (no '/' or ':' survive in the readable prefix); the hash
        // suffix follows, so match the prefix rather than the whole slug.
        assert!(isolate_slug("o/w", "na:me", "1.0.0+build").starts_with("o_w-na_me-1.0.0_build-"));
    }

    /// The storage sweep keeps by `sandbox_storage_slug`; it MUST equal the `.isolate-storage/`
    /// component `sandbox_storage_dir` writes (the parent of the `webstorage` leaf), or the sweep
    /// would delete live persistent stores. Version-independent, unlike the per-version data dir.
    #[test]
    fn storage_slug_matches_sandbox_storage_dir_component() {
        let root = std::path::Path::new("C:/smudgy/Arctic");
        for (owner, name) in [("wbk", "mapper"), ("local", "my-pkg"), ("o/w", "na:me")] {
            let dir = sandbox_storage_dir(root, owner, name);
            // `.../<slug>/webstorage` — the slug is the parent component.
            let component = dir
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert_eq!(component, sandbox_storage_slug(owner, name));
        }
        assert!(sandbox_storage_slug("o/w", "na:me").starts_with("o_w-na_me-"));
    }

    /// Nicknames and package names may both contain '-' (the nickname regex is `[A-Za-z0-9_-]`), so
    /// the readable slug is ambiguous across (owner, name[, version]) tuples. The hash suffix MUST
    /// keep them distinct, or two different owners' packages would share a cache/storage dir.
    #[test]
    fn slugs_disambiguate_dash_ambiguous_tuples() {
        assert_ne!(
            sandbox_storage_slug("a-b", "c"),
            sandbox_storage_slug("a", "b-c")
        );
        assert_ne!(
            isolate_slug("a-b", "c", "1.0.0"),
            isolate_slug("a", "b-c", "1.0.0")
        );
        assert_ne!(
            isolate_slug("a", "b", "1.0.0-c"),
            isolate_slug("a", "b-1.0.0", "c")
        );
        // The same tuple is stable across calls (the orphan sweep relies on this).
        assert_eq!(
            sandbox_storage_slug("a", "b-c"),
            sandbox_storage_slug("a", "b-c")
        );
        assert_eq!(
            isolate_slug("a", "b", "1.0.0"),
            isolate_slug("a", "b", "1.0.0")
        );
    }

    #[test]
    fn prune_removes_only_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let server_path = tmp.path();
        let isolates = server_path.join(".isolates");
        for slug in ["wbk-mapper-1.2.3", "wbk-mapper-1.1.0", "old-pkg-0.0.1"] {
            fs::create_dir_all(isolates.join(slug)).unwrap();
            fs::write(isolates.join(slug).join("data"), b"x").unwrap();
        }
        // A stray non-dir file in the root must be ignored, never removed.
        fs::write(isolates.join("stray.txt"), b"x").unwrap();

        let keep: HashSet<String> = ["wbk-mapper-1.2.3".to_string()].into_iter().collect();
        prune_orphan_isolate_dirs(server_path, &keep);

        assert!(
            isolates.join("wbk-mapper-1.2.3").is_dir(),
            "kept slug must survive"
        );
        assert!(
            !isolates.join("wbk-mapper-1.1.0").exists(),
            "superseded version pruned"
        );
        assert!(
            !isolates.join("old-pkg-0.0.1").exists(),
            "uninstalled package pruned"
        );
        assert!(
            isolates.join("stray.txt").is_file(),
            "non-dir entries are left alone"
        );
    }

    #[test]
    fn prune_with_missing_isolates_root_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        // No `.isolates` dir exists at all — must not panic or error.
        prune_orphan_isolate_dirs(tmp.path(), &HashSet::new());
    }
}
