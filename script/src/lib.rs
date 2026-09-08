//! Thin JS/TS scripting runtime for smudgy.

#[cfg(feature = "web-audio")]
pub mod audio_file;
pub mod interop_extract;
pub mod language_service;
mod language_service_engine;
pub mod language_service_worker;
mod module_loader;
mod npm_resolver;
mod package_resolver;
mod transpiler;
mod web_workers;

use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{Context as AnyhowContext, Result};
use deno_core::error::CoreError;
use deno_core::{JsRuntime, ModuleSpecifier, PollEventLoopOptions};
use deno_error::JsErrorBox;
use deno_fetch::dns::Resolver;
use deno_fs::RealFs;
use deno_permissions::RuntimePermissionDescriptorParser;
// Re-exported so callers (the per-package isolate factory) build a restricted
// `PermissionsContainer` against the exact `deno_permissions` version this crate pins,
// without depending on it directly. The recipe is `Permissions::from_options(&*parser,
// &opts)` + `PermissionsContainer::new`, with the parser from
// [`permission_descriptor_parser`].
pub use deno_permissions::{
    OpenAccessKind, PermissionDescriptorParser, Permissions, PermissionsContainer,
    PermissionsOptions,
};
use deno_resolver::npm::{DenoInNpmPackageChecker, NpmResolver};
use deno_runtime::deno_inspector_server::{
    InspectPublishUid, InspectorServer, create_inspector_server,
};
use deno_runtime::worker::{MainWorker, WorkerOptions, WorkerServiceOptions};
pub use deno_web::InMemoryBroadcastChannel;
use npm_resolver::SmudgyNpmServices;
use sys_traits::impls::RealSys;

pub use module_loader::{ImportProvider, ScriptModuleLoader};
pub use package_resolver::{
    CANONICAL_SCHEME, CanonicalCoords, EVENTS_SCHEME, ImportPolicy, InMemoryPackageProvider,
    IpcEntry, IpcEntryIssue, IpcGrants, MARKER_SCHEME, PARAMS_SCHEME, PackageDependency,
    PackageError, PackageKey, PackageManifest, PackageModuleSource, PackageParameter,
    PackagePermissions, PackageProvider, ParamKind, ParamOption, ReferrerRef, ResolvedPackage,
    STATE_SCHEME, SmudgyCapabilities, SmudgySpecifier, SmudgySpecifierError, canonical_url,
    is_any_host_net_entry, is_local_transport_net_entry, is_windows_pipe_namespace_entry,
    native_ipc_grants, native_ipc_path, params_module_url, parse_canonical, parse_params_url,
    platform_event_catalog, platform_state_producer,
};
pub use web_workers::{WorkerMode, worker_host_denied_extension};

/// Publish-time TypeScript `.d.ts` generation via the vendored, embedded tsc.
pub mod dts;

// Each smudgy_script build embeds one startup snapshot. V8 uses one shared heap
// for all live isolates, so every isolate in the process must use that snapshot.
// A base build freezes deno_runtime extension state. A Web Audio build also
// freezes deno_audio operation and object metadata.
//
// Snapshot selection is build-wide, but Web Audio exposure is runtime-specific.
// An audio runtime supplies deno_audio at custom index 0 and evaluates its ESM.
// Other runtimes supply compatibility extensions that preserve the recorded extension
// sequence, skip the deno_audio ESM entry point, and disable native audio operations.
//
// The snapshot also prevents runtime reads from absolute build-machine paths.
// Smudgy adds its stateful extensions after the sequence recorded by the snapshot.
#[cfg(not(feature = "web-audio"))]
static STARTUP_SNAPSHOT: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/SMUDGY_RUNTIME_SNAPSHOT.bin"));
#[cfg(feature = "web-audio")]
static WEB_AUDIO_STARTUP_SNAPSHOT: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/SMUDGY_RUNTIME_AUDIO_SNAPSHOT.bin"
));

// `RESIDUAL_LAZY_JS_SOURCES` / `RESIDUAL_LAZY_ESM_SOURCES`: lazy_loaded_*
// sources the snapshot did not consume, embedded here (generated — see
// build.rs) and registered via `WorkerOptions::residual_lazy_*_sources`.
#[cfg(not(feature = "web-audio"))]
include!(concat!(env!("OUT_DIR"), "/residual_lazy_sources.rs"));
#[cfg(feature = "web-audio")]
include!(concat!(env!("OUT_DIR"), "/residual_lazy_audio_sources.rs"));

fn preflight_web_audio_extension(extensions: &[deno_core::Extension]) -> Result<bool> {
    let positions: Vec<_> = extensions
        .iter()
        .enumerate()
        .filter_map(|(index, extension)| (extension.name == "deno_audio").then_some(index))
        .collect();
    match positions.as_slice() {
        [] => Ok(false),
        [0] => {
            #[cfg(feature = "web-audio")]
            {
                Ok(true)
            }
            #[cfg(not(feature = "web-audio"))]
            {
                Err(anyhow::anyhow!(
                    "the deno_audio extension requires the smudgy_script web-audio feature"
                ))
            }
        }
        _ => Err(anyhow::anyhow!(
            "invalid deno_audio extension layout: expected zero instances or exactly one at custom-extension index 0; found positions {positions:?}"
        )),
    }
}

/// Defers deno_audio ESM evaluation until deno_runtime completes bootstrap.
///
/// deno_audio imports the URL and Event globals that deno_runtime installs during
/// bootstrap. The build-generated residual table supplies their source text at runtime.
#[cfg(feature = "web-audio")]
#[doc(hidden)]
pub fn prepare_deferred_web_audio_extension(extension: &mut deno_core::Extension) {
    let deferred = std::mem::take(extension.esm_files.to_mut());

    // Move the complete graph to the lazy-loaded set. Static deno_audio imports
    // continue to resolve through the internal ext: module map.
    extension.lazy_loaded_esm_files.to_mut().extend(deferred);
    extension.esm_entry_point = None;
}

#[cfg(feature = "web-audio")]
#[derive(Debug)]
struct NoAudioPermissions;

#[cfg(feature = "web-audio")]
impl deno_audio::AudioPermissions for NoAudioPermissions {
    fn check_playback(&self, api_name: &'static str) -> Result<(), JsErrorBox> {
        Err(JsErrorBox::generic(format!(
            "{api_name} is unavailable in a runtime without Web Audio"
        )))
    }

    fn check_capture(&self, api_name: &'static str) -> Result<(), JsErrorBox> {
        Err(JsErrorBox::generic(format!(
            "{api_name} is unavailable in a runtime without Web Audio"
        )))
    }
}

/// Creates a deno_audio extension without device permissions or physical output.
#[cfg(feature = "web-audio")]
pub(crate) fn audio_snapshot_compatibility_extension() -> deno_core::Extension {
    let options = deno_audio::AudioExtensionOptions::default()
        .permissions(Arc::new(NoAudioPermissions))
        .output_factory(Arc::new(deno_audio::SilentAudioOutput::new()));
    let mut audio = deno_audio::deno_audio::init(options);
    prepare_deferred_web_audio_extension(&mut audio);
    audio
}

/// Creates snapshot-compatibility extensions for a runtime without Web Audio.
///
/// The deno_audio extension matches the sequence recorded by the Web Audio snapshot.
/// The guard disables every regular operation and native-object method from deno_audio.
/// The runtime does not evaluate the ESM entry point, so it exposes no Web Audio API.
#[cfg(feature = "web-audio")]
fn audio_snapshot_compatibility_extensions() -> [deno_core::Extension; 2] {
    let audio = audio_snapshot_compatibility_extension();
    let guard = deno_core::Extension {
        name: "smudgy_audio_snapshot_compatibility_guard",
        context_middleware_fn: Some(Box::new(|context, op| {
            if context.extension_name == "deno_audio" {
                op.disable()
            } else {
                op
            }
        })),
        ..Default::default()
    };
    [audio, guard]
}

#[derive(Debug, Clone, Default)]
pub struct ModulePolicy {
    pub allow_https: bool,
    /// Per-isolate `import` policy — how far outside the smudgy ecosystem this isolate may download
    /// code from (`npm:`/`jsr:`/`https:`). The trusted main isolate uses [`ImportPolicy::Any`] (user
    /// scripts import freely); each sandboxed package isolate uses its consented level. The default
    /// [`ImportPolicy::None`] denies every external import. The loader enforces this in `resolve()`;
    /// it is a separate axis from a `net` grant (runtime connections vs. downloading code to run).
    /// See [`PackagePermissions`](crate::PackagePermissions).
    pub import_policy: ImportPolicy,
}

#[derive(Debug, Clone)]
pub struct InspectorConfig {
    pub address: SocketAddr,
}

/// The synthetic entry module every isolate's module set loads through
/// ([`ScriptRuntime::load_modules`]): one generated module importing each local module and
/// package root so they share instances. It is machinery, not authored code — referrer
/// classifiers (the user-code-import record behind the interop attribution warning) must
/// not treat its auto-imports as a user script's.
pub const SYNTHETIC_ENTRY_SPECIFIER: &str = "file:///smudgy-modules.js";

pub struct ScriptRuntimeOptions {
    pub extensions: Vec<deno_core::Extension>,
    pub data_dir: PathBuf,
    /// Overrides the Web Storage (`localStorage`) origin dir. `None` defaults to
    /// `data_dir/webstorage`. Sandboxed packages pass a per-(owner, name) path so a package's
    /// `localStorage` survives its own version updates (each update lands in a new per-version
    /// `data_dir`, which would otherwise wipe it).
    pub webstorage_dir: Option<PathBuf>,
    pub module_policy: ModulePolicy,
    pub inspector: Option<InspectorConfig>,
    pub tokio: Rc<tokio::runtime::Runtime>,
    /// Resolves `smudgy://` shared packages. `None` disables `smudgy://` imports
    /// (the test harness and any session without cloud credentials pass `None`).
    pub package_provider: Option<Rc<dyn PackageProvider>>,
    /// Permission container for this worker isolate. `None` ⇒ `PermissionsContainer::allow_all`,
    /// which is what the *main* (trusted) isolate keeps: user scripts, local modules, and
    /// trusted packages share an allow-all isolate. The per-package isolate factory passes a
    /// *restricted* container here — built via [`permission_descriptor_parser`] +
    /// `Permissions::from_options` from the package's manifest-union — to sandbox a
    /// sandboxed-package isolate's net/fs/env/run/ffi/sys.
    pub permissions: Option<PermissionsContainer>,
    /// Server-entry scoped BroadcastChannel backend. `None` creates an
    /// isolated backend (primarily for standalone runtime tests).
    pub broadcast_channel: Option<InMemoryBroadcastChannel>,
    /// Web `Worker` policy for this isolate. The default [`WorkerMode::Disabled`]
    /// disables the worker-host ops so `new Worker(...)` — including through the
    /// `node:worker_threads` shim — throws a catchable error instead of reaching
    /// deno_runtime's panicking default callback. The enabled modes make
    /// workers real: reduced-op, off-thread compute realms with a
    /// none-permissions container whose only I/O is the message bridge. They
    /// distinguish trusted local worker modules from sandboxed package
    /// snapshots because module loading is embedder authority (see
    /// `web_workers.rs`).
    pub workers: WorkerMode,
    /// Optional lower ceiling for tests or memory-constrained embedders. This
    /// can reduce, but never raise, Smudgy's native 128-worker policy.
    pub max_live_workers_override: Option<usize>,
}

pub struct ScriptRuntime {
    /// `ManuallyDrop` so [`Drop for ScriptRuntime`](Self::drop) can tear the worker down
    /// *inside* `_tokio`'s context — see that impl for why. Never dropped anywhere else;
    /// all other access goes through `Deref`/`DerefMut`.
    worker: ManuallyDrop<MainWorker>,
    inspector_address: Option<SocketAddr>,
    _inspector_server: Option<Arc<InspectorServer>>,
    _tokio: Rc<tokio::runtime::Runtime>,
    /// A clone of the loader's package provider so [`Self::load_modules`] can build a
    /// [`LoadReport`] (resolved versions, declared parameters) after evaluation.
    package_provider: Option<Rc<dyn PackageProvider>>,
    /// Owned package-source handoff captured by this isolate's off-thread worker callback.
    /// The `!Send` provider fills it exactly once after graph loading and before evaluation;
    /// worker realms see only this immutable map, never the provider.
    worker_module_sources: web_workers::WorkerModuleSourceChannel,
}

/// The set of modules to load into the shared isolate on session start: local
/// profile module files plus installed `smudgy://` packages. Both import into one
/// synthetic entry module so packages and scripts compose as one program (see
/// `DESIGN.md`).
#[derive(Debug, Default, Clone)]
pub struct ModuleSet {
    /// Local module files (e.g. `<server>/modules/*.ts`), as `file://` URLs.
    pub local_modules: Vec<ModuleSpecifier>,
    /// Installed package specifiers to auto-import, e.g. `smudgy://wbk#4098/mapper`.
    pub packages: Vec<String>,
}

impl ModuleSet {
    /// Whether there is nothing to load.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.local_modules.is_empty() && self.packages.is_empty()
    }
}

/// What [`ScriptRuntime::load_modules`] loaded, surfaced to the host for install-time
/// option prompts and provenance (see `DESIGN.md`). The `permissions` carried here are
/// the per-package *declared* set (for host display); the deno-native fields are also
/// enforced per sandboxed isolate, unioned across the closure (see
/// `script/PACKAGE-ISOLATES-ENFORCEMENT.md`).
#[derive(Debug, Default)]
pub struct LoadReport {
    pub modules: Vec<LoadedModuleInfo>,
}

/// One entry in a [`LoadReport`].
#[derive(Debug)]
pub struct LoadedModuleInfo {
    /// User specifier (`smudgy://…`) for packages, file URL for local modules.
    pub specifier: String,
    pub kind: LoadedModuleKind,
    /// Present for packages: resolved version, declared parameters, integrity, hosts.
    pub package: Option<LoadedPackageInfo>,
}

/// Whether a loaded module is a local file or a shared package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadedModuleKind {
    LocalFile,
    Package,
}

/// Package metadata surfaced after load (the host prompts for any required-but-unset
/// parameters at install time — see `DESIGN.md`).
#[derive(Debug, Clone)]
pub struct LoadedPackageInfo {
    pub resolved_version: String,
    pub integrity: String,
    pub params: Vec<PackageParameter>,
    pub hosts: Vec<String>,
    /// Requested permissions — the package's declared set, surfaced for host display.
    /// The deno-native fields are enforced per sandboxed package isolate, unioned across
    /// the dependency closure (see `script/PACKAGE-ISOLATES-ENFORCEMENT.md`).
    pub permissions: PackagePermissions,
}

/// The descriptor parser smudgy's runtime uses to interpret `net`/`read`/`write`/`env`/
/// `run`/`ffi`/`sys` permission descriptors — `RealSys`-backed, identical to the one
/// [`ScriptRuntime::new`] builds for the default `allow_all` container. Exposed so the
/// per-package isolate factory builds a *restricted* container the same way the runtime
/// parses descriptors, without naming the pinned `deno_permissions`/`sys_traits`
/// versions itself:
///
/// ```ignore
/// use smudgy_script::{permission_descriptor_parser, Permissions, PermissionsContainer, PermissionsOptions};
/// let opts = PermissionsOptions { allow_net: Some(vec!["host:443".into()]), prompt: false, ..Default::default() };
/// let parser = permission_descriptor_parser();
/// let perms = Permissions::from_options(&*parser, &opts)?;
/// let container = PermissionsContainer::new(parser, perms); // hand to ScriptRuntimeOptions::permissions
/// ```
#[must_use]
pub fn permission_descriptor_parser() -> Arc<dyn PermissionDescriptorParser> {
    Arc::new(RuntimePermissionDescriptorParser::new(RealSys))
}

/// deno's default `FeatureChecker` callback is `std::process::exit(70)`, and
/// unstable-gated ops consult the checker BEFORE any permission check — the vsock net
/// ops (compiled on Linux/Android/macOS) reach it straight from
/// `Deno.connect({ transport: "vsock" })`. Script code must never be able to
/// terminate the host process via a feature gate, so this checker enables no unstable
/// features and swaps the exit callback for a log: the op falls through to the
/// deny-by-default permission check (or its own unsupported-platform error) and
/// surfaces as a catchable JS error instead of killing the app. Shared by the main
/// runtime and every worker realm (each worker builds its own on its own thread).
pub(crate) fn quiet_feature_checker() -> Arc<deno_runtime::FeatureChecker> {
    let mut checker = deno_runtime::FeatureChecker::default();
    checker.set_exit_cb(Box::new(|feature, api_name| {
        log::warn!("smudgy: denied unstable Deno feature '{feature}' requested by '{api_name}'");
    }));
    Arc::new(checker)
}

/// Install the process-global rustls `CryptoProvider` exactly once. deno_tls is
/// built against aws-lc-rs, so we install that provider; calling more than once
/// (multiple sessions) is a no-op via `Once`. Errors from a competing install are
/// ignored — any installed provider is fine for our use.
fn install_default_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

impl ScriptRuntime {
    pub fn new(options: ScriptRuntimeOptions) -> Result<Self> {
        // rustls 0.23 requires a process-global CryptoProvider before any TLS use;
        // deno_tls (Deno.connectTls / https module imports) reads the global default.
        // Without this, the first TLS handshake panics: "no process-level
        // CryptoProvider available". Install once across all sessions.
        install_default_crypto_provider();

        let initialize_web_audio = preflight_web_audio_extension(&options.extensions)?;
        let worker_mode = options.workers;
        let mut extensions = options.extensions;
        #[cfg(feature = "web-audio")]
        if !initialize_web_audio {
            let [audio, guard] = audio_snapshot_compatibility_extensions();
            // Keep deno_audio at custom index 0 to match the snapshot. The guard
            // follows it and disables every operation owned by deno_audio.
            extensions.insert(0, audio);
            extensions.push(guard);
        }
        // Append the general guards after the snapshot-matching extension sequence.
        extensions.push(web_workers::embedded_process_guard_extension());
        if worker_mode == WorkerMode::Disabled {
            extensions.push(web_workers::worker_host_denied_extension());
        }
        let data_dir = options.data_dir;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create script data dir {}", data_dir.display()))?;
        let origin_storage_dir = options
            .webstorage_dir
            .unwrap_or_else(|| data_dir.join("webstorage"));
        let cache_storage_dir = data_dir.join("cache");
        std::fs::create_dir_all(&origin_storage_dir).with_context(|| {
            format!(
                "failed to create script webstorage dir {}",
                origin_storage_dir.display()
            )
        })?;
        std::fs::create_dir_all(&cache_storage_dir).with_context(|| {
            format!(
                "failed to create script cache dir {}",
                cache_storage_dir.display()
            )
        })?;

        let main_module = ModuleSpecifier::parse("file:///smudgy-main.js")
            .context("failed to parse smudgy main module specifier")?;
        let (npm_services, node_services) = SmudgyNpmServices::new(data_dir.clone())?;
        let package_provider = options.package_provider.clone();
        let worker_module_sources = Arc::new(std::sync::OnceLock::new());
        // Allow-all is the default for trusted code (user scripts, local modules, trusted
        // packages). A caller may instead pass a *restricted* container to sandbox this
        // isolate — that is how a package's manifest permissions are enforced: the isolate
        // factory picks `allow_all` for the main isolate vs. `Permissions::from_options(...)`
        // for a sandboxed-package isolate. Built lazily so a supplied container skips the
        // descriptor-parser allocation.
        let permissions = options.permissions.unwrap_or_else(|| {
            PermissionsContainer::allow_all(Arc::new(RuntimePermissionDescriptorParser::new(
                RealSys,
            )))
        });
        let loader = Rc::new(ScriptModuleLoader::with_npm_and_packages(
            std::env::current_dir().context("failed to get current directory")?,
            options.module_policy,
            npm_services.clone(),
            package_provider.clone(),
            permissions.clone(),
        ));
        let fs = Arc::new(RealFs);

        let feature_checker = quiet_feature_checker();

        let services =
            WorkerServiceOptions::<DenoInNpmPackageChecker, NpmResolver<RealSys>, RealSys> {
                blob_store: Arc::new(deno_web::BlobStore::default()),
                broadcast_channel: options.broadcast_channel.unwrap_or_default(),
                deno_rt_native_addon_loader: None,
                feature_checker,
                fs,
                module_loader: loader,
                node_services: Some(node_services),
                npm_process_state_provider: None,
                permissions,
                root_cert_store_provider: None,
                fetch_dns_resolver: Resolver::default(),
                shared_array_buffer_store: None,
                compiled_wasm_module_store: None,
                v8_code_cache: None,
                bundle_provider: None,
            };

        let (inspector_server, inspector_address) = if let Some(inspector) = options.inspector {
            let server = create_inspector_server(
                inspector.address,
                "smudgy",
                InspectPublishUid {
                    console: false,
                    http: true,
                },
            )
            .with_context(|| {
                format!(
                    "failed to start script inspector server at {}",
                    inspector.address
                )
            })?;
            let address = server.host;
            (Some(server), Some(address))
        } else {
            (None, None)
        };

        // The compile-time feature selects the snapshot. `initialize_web_audio`
        // controls API exposure only and does not select a different snapshot.
        #[cfg(feature = "web-audio")]
        let (startup_snapshot, residual_lazy_js_sources, residual_lazy_esm_sources) = (
            WEB_AUDIO_STARTUP_SNAPSHOT,
            RESIDUAL_LAZY_AUDIO_JS_SOURCES,
            RESIDUAL_LAZY_AUDIO_ESM_SOURCES,
        );
        #[cfg(not(feature = "web-audio"))]
        let (startup_snapshot, residual_lazy_js_sources, residual_lazy_esm_sources) = (
            STARTUP_SNAPSHOT,
            RESIDUAL_LAZY_JS_SOURCES,
            RESIDUAL_LAZY_ESM_SOURCES,
        );

        let mut worker_options = WorkerOptions {
            extensions,
            startup_snapshot: Some(startup_snapshot),
            residual_lazy_js_sources,
            residual_lazy_esm_sources,
            origin_storage_dir: Some(origin_storage_dir),
            cache_storage_dir: Some(cache_storage_dir),
            ..Default::default()
        };
        worker_options.max_live_workers =
            Some(worker_mode.max_live_workers(options.max_live_workers_override));
        worker_options.bootstrap.location = Some(main_module.clone());
        // MUST stay false: the npm stack resolves from the global cache
        // (`<data_dir>/npm`, `maybe_node_modules_path: None` in npm_resolver.rs).
        // `true` flips deno_node's require() into local node_modules-walking mode,
        // which disables the global-cache lookup (`op_require_resolve_deno_dir`) —
        // an npm package then can't require its own dependencies (e.g.
        // `npm:discord.js` → "Cannot find module '@discordjs/util'"). A stale
        // empty `<data_dir>/node_modules` from earlier builds may exist on disk;
        // it is unused and must not be sniffed here.
        worker_options.bootstrap.has_node_modules_dir = false;
        if worker_mode.enabled() {
            // The callback uses the same build-selected snapshot for each worker.
            worker_options.create_web_worker_cb = web_workers::create_web_worker_callback(
                Arc::clone(&worker_module_sources),
                worker_mode,
            );
        }
        worker_options.bootstrap.inspect = inspector_server.is_some();
        worker_options.should_break_on_first_statement = false;
        worker_options.should_wait_for_inspector_session = false;

        // Bootstrap must run inside the Tokio runtime context. Node's stdio setup
        // (deno_node's `tty_wrap`) wraps each terminal fd in a `tokio::io::AsyncFd`,
        // which registers with a reactor on the *current thread* via
        // `Handle::current()`. `bootstrap_from_options` performs that setup
        // synchronously, but callers construct the runtime outside any `block_on`
        // (the event loop is driven later) — so without entering here, the first
        // bootstrap against a real terminal panics with "there is no reactor
        // running", and because it unwinds through a V8 op callback that panic
        // aborts the process. Entering `options.tokio` makes `Handle::current()`
        // resolve so the fds register; the event loop on that same runtime then
        // drives them. This only bites when stdout is a TTY (e.g. `cargo run` from
        // a terminal) — under `cargo test` or a windowed build stdout is a pipe, so
        // the stdio handles aren't TTYs and `uv_tty_init` is never reached.
        let mut worker = {
            let _tokio_guard = options.tokio.enter();
            MainWorker::bootstrap_from_options(&main_module, services, worker_options)
        };

        // Evaluate deno_audio only after deno_runtime installs the URL and Event globals.
        // The matching runtime extension registered native state before bootstrap.
        if initialize_web_audio {
            #[cfg(feature = "web-audio")]
            deno_audio::install_audio_file_resolver(
                &mut worker.js_runtime.op_state().borrow_mut(),
                Arc::new(audio_file::SmudgyAudioFileResolver),
            );
            let specifier = ModuleSpecifier::parse("file:///__smudgy_web_audio_bootstrap.js")
                .context("failed to parse the Web Audio bootstrap module")?;
            let module_id = options
                .tokio
                .block_on(worker.js_runtime.load_side_es_module_from_code(
                    &specifier,
                    "import 'ext:deno_audio/99_main.js';",
                ))
                .context("failed to load the Web Audio extension entry point")?;
            options
                .tokio
                .block_on(worker.evaluate_module(module_id))
                .context("failed to initialize the Web Audio extension")?;
            // This module owns Smudgy's custom file-source API. Load from trusted
            // embedded source before user modules; its ext: imports stay private.
            #[cfg(feature = "web-audio")]
            {
                let media = ModuleSpecifier::parse("smudgy-media:main")?;
                let module_id = options
                    .tokio
                    .block_on(
                        worker
                            .js_runtime
                            .load_side_es_module_from_code(&media, include_str!("media.js")),
                    )
                    .context("failed to load smudgy:media")?;
                options
                    .tokio
                    .block_on(worker.evaluate_module(module_id))
                    .context("failed to initialize smudgy:media")?;
            }
        }

        Ok(Self {
            worker: ManuallyDrop::new(worker),
            inspector_address,
            _inspector_server: inspector_server,
            _tokio: options.tokio,
            package_provider,
            worker_module_sources,
        })
    }

    pub fn deno_runtime(&mut self) -> &mut JsRuntime {
        &mut self.worker.js_runtime
    }

    pub fn inspector_address(&self) -> Option<SocketAddr> {
        self.inspector_address
    }

    pub fn poll_event_loop(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), CoreError>> {
        self.worker
            .js_runtime
            .poll_event_loop(cx, PollEventLoopOptions::default())
    }

    /// Load a profile's local modules and installed `smudgy://` packages into the
    /// shared isolate, returning per-module metadata (resolved versions, declared
    /// options) for the host to act on (see `DESIGN.md`).
    ///
    /// All modules import into one synthetic entry module so they share instances
    /// (see `DESIGN.md`); evaluation is all-or-nothing, matching how local modules
    /// already load.
    ///
    /// # Errors
    /// Returns an error if the synthetic module fails to compile or evaluate.
    // The synthetic entry's specifier is [`SYNTHETIC_ENTRY_SPECIFIER`]: consumers that
    // classify referrers (the user-code-import record in the module loader) must not
    // mistake its auto-imports for authored user code.
    pub async fn load_modules(&mut self, set: &ModuleSet) -> Result<LoadReport> {
        if set.is_empty() {
            return Ok(LoadReport::default());
        }

        let mut imports = Vec::with_capacity(set.local_modules.len() + set.packages.len());
        for url in &set.local_modules {
            imports.push(format!("import '{url}';"));
        }
        for specifier in &set.packages {
            imports.push(format!("import '{specifier}';"));
        }
        let code = imports.join("\n");

        let main = ModuleSpecifier::parse(SYNTHETIC_ENTRY_SPECIFIER)
            .context("failed to parse smudgy modules URL")?;
        let deno = &mut self.worker.js_runtime;
        let module_id = deno
            .load_main_es_module_from_code(&main, code)
            .await
            .context("failed to load smudgy module set")?;
        // Graph loading has now resolved and fetched every statically imported package, but
        // none of its top-level code has evaluated yet. Freeze the complete fetched archives
        // here so a package may construct `new Worker('./worker.ts')` during evaluation: the
        // callback runs on another OS thread and can read this owned map without capturing the
        // per-isolate provider (`!Send`). Each ScriptRuntime loads one module set, so the
        // single-assignment channel also makes the worker view immutable for the isolate's life.
        let sources = self
            .package_provider
            .as_ref()
            .map_or_else(HashMap::new, |provider| provider.snapshot_module_sources());
        self.worker_module_sources
            .set(sources)
            .expect("worker module sources are published once per ScriptRuntime");
        let mut receiver = deno.mod_evaluate(module_id);
        let evaluation = tokio::select! {
            biased;
            result = &mut receiver => result,
            loop_result = deno.run_event_loop(PollEventLoopOptions::default()) => {
                loop_result?;
                receiver.await
            }
        };
        evaluation.context("failed to evaluate smudgy module set")?;

        Ok(self.build_load_report(set))
    }

    /// Build a [`LoadReport`] from a just-loaded [`ModuleSet`], querying the package
    /// provider for each package's resolved version + declared parameters (no I/O).
    fn build_load_report(&self, set: &ModuleSet) -> LoadReport {
        let mut modules = Vec::with_capacity(set.local_modules.len() + set.packages.len());
        for url in &set.local_modules {
            modules.push(LoadedModuleInfo {
                specifier: url.to_string(),
                kind: LoadedModuleKind::LocalFile,
                package: None,
            });
        }
        for specifier in &set.packages {
            let package = SmudgySpecifier::parse(specifier).ok().and_then(|spec| {
                self.package_provider
                    .as_ref()
                    .and_then(|provider| provider.get_resolved(&spec.package_key()))
                    .map(|resolved| LoadedPackageInfo {
                        resolved_version: resolved.resolved_version.clone(),
                        integrity: resolved.integrity.clone(),
                        params: resolved.manifest.params.clone(),
                        hosts: resolved.manifest.hosts.clone(),
                        permissions: resolved.manifest.permissions.clone(),
                    })
            });
            modules.push(LoadedModuleInfo {
                specifier: specifier.clone(),
                kind: LoadedModuleKind::Package,
                package,
            });
        }
        LoadReport { modules }
    }
}

impl Drop for ScriptRuntime {
    fn drop(&mut self) {
        // Dropping the worker drops its resource table, and some resources spawn
        // tokio tasks in THEIR Drop — e.g. rustls-tokio-stream's `TlsStream` spawns a
        // graceful-shutdown task, panicking with "there is no reactor running" when no
        // runtime context is entered. Callers mostly drop us inside `block_on` on the
        // session runtime, but not always (a sandboxed package that fails to load is
        // dropped straight from `ScriptEngine::new`, where a top-level `connectTls`
        // may have left an open TLS connection behind). Rather than police every drop
        // site, enter our own runtime here so the worker teardown always has a
        // reactor. A plain field drop won't do: fields drop only AFTER this body
        // returns, when the guard is gone — hence the `ManuallyDrop` on `worker`.
        //
        // This does NOT absolve callers of the *v8* precondition: the isolate must be
        // the thread's current one when the worker drops (see `Drop for ScriptEngine`
        // in smudgy_core).
        let _tokio_guard = self._tokio.enter();
        // SAFETY: `worker` is dropped exactly here and never used again (`drop` runs
        // once, and no other code path drops it).
        unsafe { ManuallyDrop::drop(&mut self.worker) };
    }
}

fn generic_loader_error(message: impl Into<String>) -> deno_core::error::ModuleLoaderError {
    JsErrorBox::generic(message.into())
}

// npm and jsr are NOT routed through the (sync) ImportProvider. Both are handled in
// ScriptModuleLoader::load via ModuleLoadResponse::Async (driven by deno_core's event loop on
// the session runtime); npm keeps its native services and jsr keeps async metadata state.
