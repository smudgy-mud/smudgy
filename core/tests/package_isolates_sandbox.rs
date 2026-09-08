//! A session spawns a **real** sandboxed isolate per installed-untrusted
//! package, and firing routes into the owning isolate (`script/PACKAGE-ISOLATES-SANDBOX.md`).
//! Unlike `command_ordering.rs` (which proves the ordering invariant with a *synthetic*
//! second isolate key and plaintext aliases), these drive **JS-function aliases**, so a match
//! actually calls `call_javascript_function` into the sandboxed isolate's own heap + registry.
//!
//! Packages are injected via an in-memory [`PackageProvider`] (the `spawn_with_package_provider`
//! seam) so a real second isolate can be exercised without the cloud backend. The lockfile marks
//! each as untrusted (the default), so the engine gives it its own isolate.
//!
//! Covers boundary/coexistence, cross-isolate depth-first ordering, and function isolation across
//! isolates, plus singleton dedupe across isolates and versions.

use std::pin::Pin;
use std::rc::Rc;
#[cfg(feature = "web-audio")]
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(feature = "web-audio")]
use std::thread;
use std::time::Duration;

use futures::{Stream, StreamExt};
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::{RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread};
#[cfg(feature = "web-audio")]
use smudgy_core::session::spawn_with_package_provider_and_audio;
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    TaggedSessionEvent, spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};

/// Time the collector waits for the next buffer event before declaring the session idle.
const QUIET_PERIOD: Duration = Duration::from_millis(900);

// V8 snapshot deserialization is not safe when this Windows test binary starts several session
// runtimes concurrently. Every runtime helper holds this through stream teardown and the runtime
// thread join, so no isolate destruction can overlap the next snapshot deserialize.
static PACKAGE_ISOLATE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(feature = "web-audio")]
fn retained_online_context_module(label: &str, count: usize) -> String {
    r#"
        import { echo } from "smudgy:core";

        if (Object.prototype.hasOwnProperty.call(globalThis, "__full_reload_heap_brand")) {
          throw new Error("an old engine heap survived full-session reload");
        }
        globalThis.__full_reload_heap_brand = Symbol("__LABEL__-heap");

        const held = [];
        for (let index = 0; index < __COUNT__; index += 1) {
          const context = new AudioContext({ sampleRate: 48_000, sinkId: "" });
          const gain = context.createGain();
          const oscillator = context.createOscillator();
          if (!(context instanceof AudioContext)
              || Object.getPrototypeOf(context) !== AudioContext.prototype
              || !(gain instanceof GainNode)
              || Object.getPrototypeOf(gain) !== GainNode.prototype
              || !(oscillator instanceof OscillatorNode)
              || Object.getPrototypeOf(oscillator) !== OscillatorNode.prototype) {
            throw new Error("replacement Web Audio objects have stale or foreign brands");
          }
          gain.gain.value = 0;
          oscillator.connect(gain);
          gain.connect(context.destination);
          oscillator.start();
          held.push({ context, gain, oscillator, graphBrand: Object.freeze({ index }) });
        }
        globalThis.__full_reload_retained_online_contexts = held;

        const cycleKey = "smudgy-full-reload-__LABEL__-cycle";
        const previous = Number(localStorage.getItem(cycleKey) ?? "0");
        const cycle = previous + 1;
        localStorage.setItem(cycleKey, String(cycle));
        echo("FULL_RELOAD___LABEL___READY:" + cycle);
    "#
    .replace("__LABEL__", label)
    .replace("__COUNT__", &count.to_string())
}

#[cfg(feature = "web-audio")]
fn prove_full_script_bus_reuse(bus: &smudgy_audio::MixerScriptBusHandle) {
    let reservations = (0..smudgy_audio::INPUTS_PER_BUS)
        .map(|_| {
            bus.try_reserve_input()
                .expect("every Script slot is reusable at the engine boundary")
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        bus.try_reserve_input(),
        Err(smudgy_audio::MixerControlError::InputCapacity)
    ));
    for reservation in reservations {
        let retirement = futures::executor::block_on(reservation.abort())
            .expect("boundary Script reservation retires exactly");
        assert!(retirement.is_clean());
    }
}

#[cfg(feature = "web-audio")]
fn prove_script_bus_occupancy(bus: &smudgy_audio::MixerScriptBusHandle, occupied: usize) {
    let available = smudgy_audio::INPUTS_PER_BUS
        .checked_sub(occupied)
        .expect("test occupancy fits the fixed Script bus");
    let reservations = (0..available)
        .map(|_| {
            bus.try_reserve_input()
                .expect("every unoccupied Script slot is reservable")
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        bus.try_reserve_input(),
        Err(smudgy_audio::MixerControlError::InputCapacity)
    ));
    for reservation in reservations {
        let retirement = futures::executor::block_on(reservation.abort())
            .expect("occupancy probe reservation retires exactly");
        assert!(retirement.is_clean());
    }
}

#[cfg(feature = "web-audio")]
async fn wait_for_quiescent_audio_usage(
    application: &smudgy_audio_web::ApplicationAudioOwner,
    online_contexts: usize,
    description: &str,
) -> deno_audio::AudioHostUsage {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut previous = None;
    let mut stable_samples = 0;
    loop {
        let usage = application.usage();
        let quiescent = usage.online_contexts() == online_contexts
            && usage.queued_control_commands() == 0
            && usage.queued_events() == 0;
        if quiescent && previous == Some(usage) {
            stable_samples += 1;
            if stable_samples >= 3 {
                return usage;
            }
        } else {
            previous = Some(usage);
            stable_samples = 0;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {description}; last usage: {usage:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[cfg(feature = "web-audio")]
async fn wait_for_exact_audio_usage(
    application: &smudgy_audio_web::ApplicationAudioOwner,
    expected: deno_audio::AudioHostUsage,
    description: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let usage = application.usage();
        if usage == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {description}; expected {expected:?}, got {usage:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[cfg(feature = "web-audio")]
async fn wait_for_audio_markers(
    events: &mut Pin<Box<dyn Stream<Item = TaggedSessionEvent>>>,
    transcript: &mut Vec<String>,
    expected: &[String],
    description: &str,
) {
    let mut observed = expected
        .iter()
        .filter(|marker| transcript.contains(marker))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let expected = expected
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    tokio::time::timeout(Duration::from_mins(1), async {
        while observed != expected {
            let event = events.next().await.expect("audio session remains live");
            if let SessionEvent::UpdateBuffer(updates) = event.event {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        transcript.push(line.text.clone());
                        if expected.contains(&line.text) {
                            observed.insert(line.text.clone());
                        }
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for {description}; observed {observed:?}; transcript: {transcript:#?}"
        )
    });
}

#[cfg(feature = "web-audio")]
fn s4c_package_module(label: &str, foreign_label: &str) -> String {
    r#"
        import { createAlias, echo } from "smudgy:core";

        const label = "__LABEL__";
        const cycleKey = "smudgy-s4c-__LABEL__-cycle";
        const generation = Number(localStorage.getItem(cycleKey) ?? "0") + 1;
        localStorage.setItem(cycleKey, String(generation));
        if (Object.prototype.hasOwnProperty.call(globalThis, "__s4c_package_heap_brand")) {
          throw new Error("an old package heap survived full-session reload");
        }
        if (globalThis.__s4c_main_context !== undefined
            || globalThis.__s4c___FOREIGN___context !== undefined) {
          throw new Error("a package can inspect another isolate's graph");
        }
        globalThis.__s4c_package_heap_brand = Symbol(label + "-generation-" + generation);

        const context = new AudioContext({ sampleRate: 48_000, sinkId: "" });
        if (!(context instanceof AudioContext)
            || Object.getPrototypeOf(context) !== AudioContext.prototype
            || context.sinkId !== "") {
          throw new Error("package received a stale or foreign AudioContext brand");
        }
        globalThis.__s4c___LABEL___context = context;
        if (generation === 1) {
          context.onstatechange = () => {
            if (context.state !== "closed") return;
            const current = Number(localStorage.getItem(cycleKey) ?? "0");
            echo(current > generation
              ? "S4C_FORBIDDEN_OLD_" + label + "_STATE_AFTER_REPLACEMENT"
              : "S4C_" + label + "_OLD_STATE_DURING_RETIREMENT:1");
            context.resume().then(
              () => echo("S4C_FORBIDDEN_OLD_" + label + "_CONTEXT_REOPENED"),
              () => {},
            );
          };
          const oldSource = context.createOscillator();
          oldSource.connect(context.destination);
          oldSource.onended = () => {
            globalThis.__s4c_old_package_waiter = (async () => {
              echo("S4C_" + label + "_OLD_HANDLER_ENTERED:1");
              await new Promise(() => {});
              if (Number(localStorage.getItem(cycleKey) ?? "0") > generation) {
                echo("S4C_FORBIDDEN_OLD_" + label + "_ENDED_AFTER_REPLACEMENT");
              }
              await context.resume();
              echo("S4C_FORBIDDEN_OLD_" + label + "_CONTEXT_REOPENED");
            })();
          };
          oldSource.start();
          oldSource.stop(context.currentTime + 0.02);
        } else {
          context.onstatechange = () => {
            if (context.state === "closed") {
              echo("S4C_" + label + "_CLOSED:2");
            }
          };
        }

        createAlias("^s4c_package_status$", () => {
          echo("S4C_" + label + "_STATE:" + context.state);
        });
        __ALPHA_CONTROLS__
        echo("S4C_" + label + "_READY:" + generation);
    "#
    .replace("__LABEL__", label)
    .replace("__FOREIGN__", foreign_label)
    .replace(
        "__ALPHA_CONTROLS__",
        if label == "ALPHA" {
            r#"
            createAlias("^s4c_alpha_suspend$", async () => {
              await context.suspend();
              echo("S4C_ALPHA_SUSPENDED:" + context.state);
            });
            createAlias("^s4c_alpha_resume$", async () => {
              await context.resume();
              echo("S4C_ALPHA_RESUMED:" + context.state);
            });
            "#
        } else {
            ""
        },
    )
}

/// One `smudgy://owner/name` test package: its version and module sources (entry is `index.js`).
struct TestPackage {
    owner: &'static str,
    name: &'static str,
    version: &'static str,
    modules: Vec<(&'static str, String)>,
    dependencies: Vec<&'static str>,
    /// Whether the lock entry is installed enabled. A disabled install is still *resolvable* (it
    /// stays in the in-memory provider) but the engine must skip it when building the isolate set.
    enabled: bool,
}

impl TestPackage {
    fn new(owner: &'static str, name: &'static str, version: &'static str, entry: &str) -> Self {
        Self {
            owner,
            name,
            version,
            modules: vec![("index.js", entry.to_string())],
            dependencies: Vec::new(),
            enabled: true,
        }
    }

    #[cfg(feature = "web-audio")]
    fn dependency(mut self, specifier: &'static str) -> Self {
        self.dependencies.push(specifier);
        self
    }

    fn manifest(&self) -> PackageManifest {
        PackageManifest::parse(&format!(
            "{{ \"name\": {:?}, \"version\": {:?}, \"dependencies\": {} }}",
            self.name,
            self.version,
            serde_json::to_string(&self.dependencies).expect("static dependencies serialize")
        ))
        .expect("valid test package manifest")
    }

    /// Mark this package installed-but-disabled (the user's "install, don't enable" choice).
    fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// Spin up a headless session whose `smudgy://` packages resolve from an in-memory provider,
/// each installed untrusted (so the engine spawns it a sandboxed isolate). Wait until the `gate`
/// sentinel line has been observed `gate_count` times — module/package automations register
/// through the FIFO action queue, so a sentinel echoed *after* a `createAlias` proves that alias
/// is live — then send `input` and collect every appended line until the session goes quiet.
#[allow(clippy::too_many_lines)]
async fn run_scenario(
    session_id: u32,
    server: &str,
    local_modules: &[(&str, &str)],
    packages: Vec<TestPackage>,
    gate: &str,
    gate_count: usize,
    input: &str,
) -> Vec<String> {
    run_scenario_inner(
        session_id,
        server,
        local_modules,
        packages,
        gate,
        gate_count,
        input,
        None,
    )
    .await
}

#[cfg(feature = "web-audio")]
#[allow(clippy::too_many_arguments)]
async fn run_scenario_with_audio(
    session_id: u32,
    server: &str,
    local_modules: &[(&str, &str)],
    packages: Vec<TestPackage>,
    gate: &str,
    gate_count: usize,
    input: &str,
    audio_scope: smudgy_audio_web::SessionAudioScope,
) -> Vec<String> {
    run_scenario_inner(
        session_id,
        server,
        local_modules,
        packages,
        gate,
        gate_count,
        input,
        Some(audio_scope),
    )
    .await
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn run_scenario_inner(
    session_id: u32,
    server: &str,
    local_modules: &[(&str, &str)],
    packages: Vec<TestPackage>,
    gate: &str,
    gate_count: usize,
    input: &str,
    #[cfg(feature = "web-audio")] audio_scope: Option<smudgy_audio_web::SessionAudioScope>,
    #[cfg(not(feature = "web-audio"))] _audio_scope: Option<()>,
) -> Vec<String> {
    let _test_guard = PACKAGE_ISOLATE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let session_id = SessionId::from(session_id);

    // The smudgy home override is a process-global `OnceLock` (first setter in the binary wins),
    // so re-read it after setting and scope everything under a unique server name per test.
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let server_dir = home.join(server);
    let modules_dir = server_dir.join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(server_dir.join("logs")).unwrap();
    for (name, source) in local_modules {
        std::fs::write(modules_dir.join(name), source).unwrap();
    }
    // Install each package untrusted (the default) → its own sandboxed isolate, honoring its
    // `enabled` flag so a disabled package is installed-but-skipped by the engine.
    for pkg in &packages {
        let spec = format!("smudgy://{}/{}", pkg.owner, pkg.name);
        shared_packages::install_package(server, &spec, UpdateMode::Auto, pkg.enabled).unwrap();
        // The smudgy ops these isolate-boundary/ordering tests rely on (`createAlias` /
        // `createTriggers` / `echo` / `send`) are capability-gated. They don't exercise capability gating (that's
        // `package_isolates_enforcement.rs`), so grant the full smudgy capability set at install —
        // without a consent record a sandboxed package would be denied every smudgy op and these
        // tests couldn't run. Deno perms stay empty (none of these packages touch net/fs).
        shared_packages::record_consent(
            server,
            &spec,
            &PackagePermissions {
                smudgy: SmudgyCapabilities::all(),
                ..Default::default()
            },
        )
        .unwrap();
    }

    // Inject the in-memory resolver in place of a cloud client. Rebuilt per engine construction
    // (incl. reload); each isolate's own loader compiles the same source into its own heap.
    let factory: PackageProviderFactory = Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        for pkg in &packages {
            provider.insert(ResolvedPackage {
                key: PackageKey {
                    owner: pkg.owner.to_string(),
                    name: pkg.name.to_string(),
                },
                resolved_version: pkg.version.to_string(),
                manifest: pkg.manifest(),
                integrity: format!("test-{}-{}", pkg.name, pkg.version),
                modules: pkg
                    .modules
                    .iter()
                    .map(|(subpath, text)| PackageModuleSource {
                        subpath: (*subpath).to_string(),
                        text: text.clone(),
                    })
                    .collect(),
            });
        }
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    });

    let params = Arc::new(SessionParams {
        session_id,
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>> = {
        #[cfg(feature = "web-audio")]
        if let Some(audio_scope) = audio_scope {
            Box::pin(
                spawn_with_package_provider_and_audio(params, factory, audio_scope)
                    .expect("audio scope matches package session"),
            )
        } else {
            Box::pin(spawn_with_package_provider(params, factory))
        }
        #[cfg(not(feature = "web-audio"))]
        {
            Box::pin(spawn_with_package_provider(params, factory))
        }
    };
    // Collect from the very first event: engine notices (e.g. "[package] X failed to load") are
    // emitted during construction, before `RuntimeReady`, so they'd otherwise be consumed by the
    // wait loop and lost.
    let mut lines: Vec<String> = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };

    // Pin the command separator so the test is environment-independent.
    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: true,
        bold_is_bright: false,
        script_settings: Box::new(smudgy_core::models::settings::ScriptSettings::default()),
    })
    .unwrap();

    let mut seen_gate = 0usize;
    let mut sent = false;
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                    if !sent && line.text == gate {
                        seen_gate += 1;
                        if seen_gate >= gate_count {
                            tx.send(RuntimeAction::Send(Arc::new(input.to_string())))
                                .unwrap();
                            sent = true;
                        }
                    }
                }
            }
        }
    }
    tx.send(RuntimeAction::Shutdown)
        .expect("runtime accepts shutdown");
    drop(tx);
    drop(events);
    let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
        .await
        .expect("runtime join task does not panic");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });

    assert!(
        sent,
        "gate sentinel {gate:?} (x{gate_count}) was never observed; transcript:\n{lines:#?}"
    );
    lines
}

/// Boundary/coexistence with a real second isolate: a local module (main isolate) and a
/// sandboxed package each register a same-named JS alias; one input matches both and **both
/// fire**, because the trigger Manager keys by `(IsolateId, Origin, name)`. Each handler runs in
/// its own isolate's heap (`call_javascript_function` routes by id). To make the *isolate boundary*
/// load-bearing (the two also have distinct `Origin`s, so coexistence alone wouldn't require a
/// second isolate), the package reports `from_pkg` only if it CANNOT see a `globalThis` marker the
/// main module set in main's heap — so a no-op sandbox (one shared heap) would report `from_pkg_LEAK`
/// and fail this test.
#[tokio::test]
async fn coexists_across_main_and_sandboxed_isolate() {
    // Marker lives in main's heap only; the package seeing it would mean a shared isolate.
    let main_mod = r#"
        import { createAlias, echo } from "smudgy:core";
        globalThis.__leak_marker = "MAIN";
        createAlias("^dup$", () => { echo("from_main"); });
    "#;
    let pkg = TestPackage::new(
        "wbk",
        "inc",
        "1.0.0",
        r#"
        import { createAlias, echo } from "smudgy:core";
        createAlias("^dup$", () => {
            echo(globalThis.__leak_marker ? "from_pkg_LEAK" : "from_pkg");
        });
        echo("PKG_READY");
        "#,
    );

    let lines = run_scenario(
        9201,
        "pi_sandbox_boundary",
        &[("main_mod.ts", main_mod)],
        vec![pkg],
        "PKG_READY",
        1,
        "dup",
    )
    .await;

    assert!(
        lines.iter().any(|l| l == "from_main"),
        "main-isolate alias must fire; transcript:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "from_pkg"),
        "sandboxed-package alias must fire AND be isolated (no `from_pkg_LEAK`) — coexistence across a real isolate boundary; transcript:\n{lines:#?}"
    );
}

/// Main, two sandbox roots, and one imported dependency render constant signals
/// into one fake physical mixer. The exact numeric composition proves that the
/// dependency inherits its importer's versionless root gain, the sibling root
/// is isolated, and Main bypasses package gain while retaining session gain.
#[cfg(feature = "web-audio")]
#[tokio::test]
#[allow(clippy::too_many_lines)] // one numeric probe keeps all composition phases on one mixer
async fn sandboxed_package_can_render_web_audio() {
    let constant_source = |label: &str| {
        r#"
        import { echo } from "smudgy:core";
        import { createFileSource, AudioFileSourceNode } from "smudgy:media";
        if (typeof createFileSource !== "function" || typeof AudioFileSourceNode !== "function") {
          throw new Error("Smudgy media module missing in package isolate");
        }
        if ("createFileSource" in AudioContext.prototype || "AudioFileSourceNode" in globalThis) {
          throw new Error("Smudgy media extended Web Audio globals");
        }
        const context = new AudioContext({ sampleRate: 48_000, sinkId: "" });
        const buffer = context.createBuffer(2, 128, 48_000);
        buffer.getChannelData(0).fill(0.1);
        buffer.getChannelData(1).fill(0.1);
        const source = context.createBufferSource();
        source.buffer = buffer;
        source.loop = true;
        source.connect(context.destination);
        source.start();
        globalThis.__retained_package_audio = { context, source };
        echo("PACKAGE_AUDIO_SCOPE_READY:" + "__LABEL__");
        "#
        .replace("__LABEL__", label)
    };
    let helper = TestPackage::new("a11y", "helper", "3.0.0", &constant_source("HELPER")).disabled();
    let alpha = TestPackage::new(
        "a11y",
        "earcon",
        "1.0.0",
        &format!(
            r#"
        import "smudgy://a11y/helper";
        {}
        "#,
            constant_source("ALPHA")
        ),
    )
    .dependency("smudgy://a11y/helper");
    let beta = TestPackage::new("sound", "beta", "7.4.0", &constant_source("BETA"));

    let session_id = 9_211;
    let (service, probe) = smudgy_audio::test_support::start_test_mixer(
        48_000,
        smudgy_audio::test_support::TestDriverConfig::default(),
    )
    .expect("headless process mixer starts");
    let mixer_owner = service
        .add_session(smudgy_audio::AudioSessionId(u64::from(session_id)))
        .expect("package session joins process mixer");
    let master_gain = service.master_gain_authority();
    let application = smudgy_audio_web::ApplicationAudioOwner::new(
        deno_audio::AudioHostLimits::unlimited()
            .max_online_contexts(Some(8))
            .max_live_audio_bytes(Some(32 * 1024 * 1024))
            .max_graph_nodes(Some(512))
            .max_graph_connections(Some(512))
            .max_scheduled_sources(Some(256))
            .max_automation_events(Some(1_024))
            .max_queued_control_commands(Some(256))
            .max_queued_events(Some(256))
            .max_decode_jobs(Some(2))
            .max_offline_render_jobs(Some(2)),
    );
    let registration = application
        .registrar()
        .register_session(mixer_owner)
        .expect("package session audio registration succeeds");
    let render_done = Arc::new(AtomicBool::new(false));
    let expected_bits = Arc::new(AtomicU32::new(f32::NAN.to_bits()));
    let expected_epoch = Arc::new(AtomicUsize::new(0));
    let observed_epoch = Arc::new(AtomicUsize::new(0));
    let render_thread = {
        let render_done = Arc::clone(&render_done);
        let expected_bits = Arc::clone(&expected_bits);
        let expected_epoch = Arc::clone(&expected_epoch);
        let observed_epoch = Arc::clone(&observed_epoch);
        let probe = probe.clone();
        thread::spawn(move || {
            let mut output = [0.0; 256];
            while !render_done.load(Ordering::Acquire) {
                let _ = probe.render(&mut output, 2);
                let epoch = expected_epoch.load(Ordering::Acquire);
                let expected = f32::from_bits(expected_bits.load(Ordering::Acquire));
                if epoch != 0
                    && expected.is_finite()
                    && output
                        .iter()
                        .all(|sample| (*sample - expected).abs() < 1.0e-4)
                {
                    observed_epoch.store(epoch, Ordering::Release);
                }
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let mut scenario = Box::pin(run_scenario_with_audio(
        session_id,
        "pi_sandbox_web_audio",
        &[(
            "trusted-web-audio.ts",
            r#"
            import { echo } from "smudgy:core";
            const context = new AudioContext({ sampleRate: 48_000, sinkId: "" });
            globalThis.__main_audio_context = context;
            const buffer = context.createBuffer(2, 128, 48_000);
            buffer.getChannelData(0).fill(0.1);
            buffer.getChannelData(1).fill(0.1);
            const source = context.createBufferSource();
            source.buffer = buffer;
            source.loop = true;
            source.connect(context.destination);
            source.start();
            globalThis.__retained_main_audio = { context, source };
            echo("PACKAGE_AUDIO_SCOPE_READY:MAIN");
            "#,
        )],
        vec![alpha, helper, beta],
        "PACKAGE_AUDIO_SCOPE_READY:MAIN",
        1,
        "noop",
        registration.scope(),
    ));
    let mut controls_applied = false;
    let mut composition_phase = 0_u8;
    let lines = loop {
        tokio::select! {
            lines = &mut scenario => break lines,
            () = tokio::time::sleep(Duration::from_millis(5)), if composition_phase < 2 => {
                if !controls_applied {
                    let alpha = registration.package_control_key("A11Y", "EARCON");
                    let beta = registration.package_control_key("sound", "beta");
                    if let (Ok(alpha), Ok(beta)) = (alpha, beta) {
                        assert_eq!(
                            registration.package_control_key("a11y", "helper"),
                            Err(smudgy_audio_web::PackageAudioControlError::UnknownPackage),
                            "an imported dependency inherits the root scope rather than registering one"
                        );
                        registration.set_package_gain_linear(&alpha, 0.5).unwrap();
                        registration.set_package_gain_linear(&beta, 0.25).unwrap();
                        // Main: .1; alpha root + dependency: (.1 + .1) * .5;
                        // beta: .1 * .25. A dependency-local or shared-root bug
                        // produces a different exact mix.
                        expected_bits.store(0.225_f32.to_bits(), Ordering::Release);
                        expected_epoch.store(1, Ordering::Release);
                        controls_applied = true;
                    }
                } else if composition_phase == 0
                    && observed_epoch.load(Ordering::Acquire) >= 1
                {
                        registration.set_gain_linear(0.5).unwrap();
                        expected_bits.store(0.1125_f32.to_bits(), Ordering::Release);
                        expected_epoch.store(2, Ordering::Release);
                        composition_phase = 1;
                } else if composition_phase == 1
                    && observed_epoch.load(Ordering::Acquire) >= 2
                {
                        master_gain.set_linear(0.5).unwrap();
                        expected_bits.store(0.05625_f32.to_bits(), Ordering::Release);
                        expected_epoch.store(3, Ordering::Release);
                        composition_phase = 2;
                }
            }
        }
    };
    render_done.store(true, Ordering::Release);
    render_thread.join().expect("fake physical renderer joins");

    assert!(
        controls_applied,
        "sandbox-root controls never became active; transcript:\n{lines:#?}"
    );
    assert!(
        composition_phase == 2 && observed_epoch.load(Ordering::Acquire) >= 3,
        "master x session x package composition never reached every exact phase; phase={composition_phase}; transcript:\n{lines:#?}"
    );
    assert_eq!(probe.start_count(), 1);
    assert_eq!(probe.play_count(), 1);
    drop(registration);
    assert!(service.shutdown().clean);
}

/// A live reload replaces the complete script engine: trusted main and every sandboxed package
/// isolate. This stress keeps every old online context intentionally open and makes the runtime's
/// generation retirement, rather than script-authored `close()`, return the exact application
/// permits and Script slots before the replacement can construct anything.
#[cfg(feature = "web-audio")]
#[tokio::test]
#[allow(clippy::await_holding_lock, clippy::too_many_lines)]
#[allow(clippy::float_cmp)] // exact package-control snapshots are the reload contract
async fn repeated_full_session_reload_returns_exact_audio_baseline() {
    let _test_guard = PACKAGE_ISOLATE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let reload_server = "pi_full_session_audio_reload";
    let sibling_server = "pi_full_reload_sibling";
    for server in [reload_server, sibling_server] {
        std::fs::create_dir_all(home.join(server).join("modules"))
            .expect("create module directory");
        std::fs::create_dir_all(home.join(server).join("logs")).expect("create log directory");
    }

    std::fs::write(
        home.join(sibling_server).join("modules/sibling.ts"),
        r#"
        import { createAlias, echo } from "smudgy:core";
        const context = new AudioContext({ sampleRate: 48_000, sinkId: "" });
        const gain = context.createGain();
        const oscillator = context.createOscillator();
        gain.gain.value = 0.025;
        oscillator.connect(gain);
        gain.connect(context.destination);
        oscillator.start();
        globalThis.__full_reload_sibling_audio = { context, gain, oscillator };
        createAlias("^full_reload_ping$", () => echo("FULL_RELOAD_SIBLING_PONG"));
        echo("FULL_RELOAD_SIBLING_READY");
        "#,
    )
    .expect("write sibling module");
    std::fs::write(
        home.join(reload_server).join("modules/main.ts"),
        retained_online_context_module("MAIN", 5),
    )
    .expect("write reloader main module");

    let packages = vec![
        TestPackage::new(
            "reload",
            "alpha",
            "1.0.0",
            &retained_online_context_module("ALPHA", 5),
        ),
        TestPackage::new(
            "reload",
            "bravo",
            "1.0.0",
            &retained_online_context_module("BRAVO", 5),
        ),
        TestPackage::new(
            "reload",
            "charlie",
            "1.0.0",
            &retained_online_context_module("CHARLIE", 5),
        ),
        TestPackage::new(
            "reload",
            "delta",
            "1.0.0",
            &retained_online_context_module("DELTA", 5),
        ),
        TestPackage::new(
            "reload",
            "echo",
            "1.0.0",
            &retained_online_context_module("ECHO", 5),
        ),
        TestPackage::new(
            "reload",
            "zulu",
            "1.0.0",
            &retained_online_context_module("ZULU", 2),
        ),
    ];
    for package in &packages {
        let specifier = format!("smudgy://{}/{}", package.owner, package.name);
        shared_packages::install_package(reload_server, &specifier, UpdateMode::Auto, true)
            .expect("install reload package");
        shared_packages::record_consent(
            reload_server,
            &specifier,
            &PackagePermissions {
                smudgy: SmudgyCapabilities::all(),
                ..Default::default()
            },
        )
        .expect("grant package echo capability");
    }
    let package_factory: PackageProviderFactory = Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        for package in &packages {
            provider.insert(ResolvedPackage {
                key: PackageKey {
                    owner: package.owner.to_string(),
                    name: package.name.to_string(),
                },
                resolved_version: package.version.to_string(),
                manifest: package.manifest(),
                integrity: format!("reload-{}-{}", package.name, package.version),
                modules: package
                    .modules
                    .iter()
                    .map(|(subpath, text)| PackageModuleSource {
                        subpath: (*subpath).to_string(),
                        text: text.clone(),
                    })
                    .collect(),
            });
        }
        Rc::new(provider) as Rc<dyn PackageProvider>
    });

    let (service, probe) = smudgy_audio::test_support::start_test_mixer(
        48_000,
        smudgy_audio::test_support::TestDriverConfig::default(),
    )
    .expect("headless process mixer starts once");
    let render_done = Arc::new(AtomicBool::new(false));
    let rendered_quanta = Arc::new(AtomicUsize::new(0));
    let non_silent_quanta = Arc::new(AtomicUsize::new(0));
    let render_thread = {
        let render_done = Arc::clone(&render_done);
        let rendered_quanta = Arc::clone(&rendered_quanta);
        let non_silent_quanta = Arc::clone(&non_silent_quanta);
        let probe = probe.clone();
        thread::spawn(move || {
            let mut output = [0.0; 256];
            while !render_done.load(Ordering::Acquire) {
                if probe.render(&mut output, 2).is_ok() {
                    rendered_quanta.fetch_add(1, Ordering::AcqRel);
                    if output.iter().any(|sample| sample.abs() > f32::EPSILON) {
                        non_silent_quanta.fetch_add(1, Ordering::AcqRel);
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
        })
    };

    let mut application = Arc::new(smudgy_audio_web::ApplicationAudioOwner::new(
        deno_audio::AudioHostLimits::unlimited()
            .max_online_contexts(Some(33))
            .max_live_audio_bytes(Some(32 * 1024 * 1024))
            .max_graph_nodes(Some(1_024))
            .max_graph_connections(Some(1_024))
            .max_scheduled_sources(Some(512))
            .max_automation_events(Some(1_024))
            .max_queued_control_commands(Some(2_048))
            .max_queued_events(Some(2_048))
            .max_decode_jobs(Some(2))
            .max_offline_render_jobs(Some(2)),
    ));
    let empty_baseline = application.usage();
    assert_eq!(empty_baseline, deno_audio::AudioHostUsage::default());

    let sibling_numeric_id = 9_220;
    let sibling_id = SessionId::from(sibling_numeric_id);
    let sibling_owner = service
        .add_session(smudgy_audio::AudioSessionId(u64::from(sibling_numeric_id)))
        .expect("sibling joins process mixer");
    let sibling_registration = application
        .registrar()
        .register_session(sibling_owner)
        .expect("sibling audio registration succeeds");
    let sibling_params = Arc::new(SessionParams {
        session_id: sibling_id,
        server_name: Arc::new(sibling_server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut sibling_events = Box::pin(
        smudgy_core::session::spawn_with_audio(sibling_params, sibling_registration.scope())
            .expect("sibling audio scope matches"),
    );
    let mut sibling_tx = None;
    tokio::time::timeout(Duration::from_mins(1), async {
        let mut ready = false;
        while sibling_tx.is_none() || !ready {
            let event = sibling_events.next().await.expect("sibling remains live");
            match event.event {
                SessionEvent::RuntimeReady(tx) => sibling_tx = Some(tx),
                SessionEvent::UpdateBuffer(updates) => {
                    ready |= updates.iter().any(|update| {
                        matches!(update, BufferUpdate::Append(line) if line.text == "FULL_RELOAD_SIBLING_READY")
                    });
                }
                _ => {}
            }
        }
    })
    .await
    .expect("sibling starts");
    let sibling_baseline =
        wait_for_quiescent_audio_usage(&application, 1, "sibling-only host baseline").await;
    assert_eq!(sibling_baseline.graph_nodes(), 5);
    assert_eq!(sibling_baseline.graph_connections(), 2);
    assert_eq!(sibling_baseline.scheduled_sources(), 1);

    let reload_numeric_id = 9_221;
    let reload_id = SessionId::from(reload_numeric_id);
    let reload_owner = service
        .add_session(smudgy_audio::AudioSessionId(u64::from(reload_numeric_id)))
        .expect("reloader joins the same process mixer");
    let reload_script_bus = reload_owner.script_bus();
    let reload_native_bus = reload_owner.native_bus();
    let reload_speech_bus = reload_owner.speech_bus();
    let reload_registration = application
        .registrar()
        .register_session(reload_owner)
        .expect("reloader audio registration succeeds");
    let reload_scope = reload_registration.scope();
    let scope_debug = format!("{reload_scope:?}");
    let rebuild_count = Arc::new(AtomicUsize::new(0));
    let on_engine_rebuild: Arc<dyn Fn() + Send + Sync> = {
        let application = Arc::clone(&application);
        let reload_script_bus = reload_script_bus.clone();
        let rebuild_count = Arc::clone(&rebuild_count);
        Arc::new(move || {
            assert_eq!(
                application.usage(),
                sibling_baseline,
                "old main/package contexts must drain before replacement construction"
            );
            prove_full_script_bus_reuse(&reload_script_bus);
            assert_eq!(application.usage(), sibling_baseline);
            rebuild_count.fetch_add(1, Ordering::AcqRel);
        })
    };
    let reload_params = Arc::new(SessionParams {
        session_id: reload_id,
        server_name: Arc::new(reload_server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: Some(Arc::clone(&on_engine_rebuild)),
    });
    let mut reload_events = Box::pin(
        spawn_with_package_provider_and_audio(reload_params, package_factory, reload_scope.clone())
            .expect("reloader audio scope matches"),
    );
    let mut reload_tx = None;
    let labels = ["MAIN", "ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "ZULU"];
    let mut transcript = Vec::new();
    let mut loaded_baseline = None;
    let mut cross_reload_render_threshold = None;
    let mut previous_alpha_key: Option<smudgy_audio_web::PackageAudioControlKey> = None;

    for cycle in 1..=4 {
        let expected = labels
            .iter()
            .map(|label| format!("FULL_RELOAD_{label}_READY:{cycle}"))
            .collect::<std::collections::BTreeSet<_>>();
        let mut observed = std::collections::BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(90), async {
            while observed != expected {
                let event = reload_events.next().await.expect("reloader remains live");
                match event.event {
                    SessionEvent::RuntimeReady(tx) => reload_tx = Some(tx),
                    SessionEvent::UpdateBuffer(updates) => {
                        for update in updates.iter() {
                            if let BufferUpdate::Append(line) = update {
                                transcript.push(line.text.clone());
                                if expected.contains(&line.text) {
                                    observed.insert(line.text.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "generation {cycle} did not reconstruct every isolate; observed {observed:?}; transcript: {transcript:#?}"
            )
        });
        if let Some((rendered_before_reload, non_silent_before_reload)) =
            cross_reload_render_threshold.take()
        {
            assert!(
                rendered_quanta.load(Ordering::Acquire) > rendered_before_reload
                    && non_silent_quanta.load(Ordering::Acquire) > non_silent_before_reload,
                "sibling physical output did not advance across reload cycle {cycle}"
            );
        }
        assert_eq!(rebuild_count.load(Ordering::Acquire), cycle);
        assert_eq!(format!("{reload_scope:?}"), scope_debug);
        assert_eq!(reload_scope.session_id(), u64::from(reload_numeric_id));

        let alpha_key = reload_registration
            .package_control_key("RELOAD", "alpha")
            .expect("the successfully loaded folded sandbox root is controllable");
        if let Some(previous) = previous_alpha_key.replace(alpha_key.clone()) {
            assert_eq!(
                reload_registration.set_package_gain_muted(&previous, false),
                Err(smudgy_audio_web::PackageAudioControlError::StalePackage),
                "a full RuntimeAction::Reload must stale the old root lease"
            );
            let restored = reload_registration
                .set_package_gain_muted(&alpha_key, false)
                .expect("the replacement root lease is active");
            assert_eq!(
                restored.linear(),
                0.25,
                "versionless package gain survives the complete engine reload"
            );
        } else {
            let applied = reload_registration
                .set_package_gain_linear(&alpha_key, 0.25)
                .expect("first sandbox-root gain applies");
            assert_eq!(applied.linear(), 0.25);
        }

        let loaded = wait_for_quiescent_audio_usage(
            &application,
            33,
            &format!("loaded Web Audio generation {cycle}"),
        )
        .await;
        assert!(matches!(
            reload_script_bus.try_reserve_input(),
            Err(smudgy_audio::MixerControlError::InputCapacity)
        ));
        if let Some(expected_loaded) = loaded_baseline {
            assert_eq!(loaded, expected_loaded);
        } else {
            assert_eq!(
                loaded.online_contexts() - sibling_baseline.online_contexts(),
                32
            );
            assert_eq!(loaded.graph_nodes() - sibling_baseline.graph_nodes(), 160);
            assert_eq!(
                loaded.graph_connections() - sibling_baseline.graph_connections(),
                64
            );
            assert_eq!(
                loaded.scheduled_sources() - sibling_baseline.scheduled_sources(),
                32
            );
            assert_eq!(
                loaded.live_audio_bytes(),
                sibling_baseline.live_audio_bytes()
            );
            assert_eq!(
                loaded.automation_events(),
                sibling_baseline.automation_events()
            );
            assert_eq!(
                loaded.queued_control_commands(),
                sibling_baseline.queued_control_commands()
            );
            assert_eq!(loaded.queued_events(), sibling_baseline.queued_events());
            assert_eq!(loaded.decode_jobs(), sibling_baseline.decode_jobs());
            assert_eq!(
                loaded.offline_render_jobs(),
                sibling_baseline.offline_render_jobs()
            );
            loaded_baseline = Some(loaded);
        }

        let native_gains = [0.55, 0.60, 0.65, 0.70];
        let speech_gains = [0.70, 0.65, 0.60, 0.55];
        reload_native_bus
            .set_gain(native_gains[cycle - 1])
            .expect("the exact Native bus survives reload");
        reload_speech_bus
            .set_gain(speech_gains[cycle - 1])
            .expect("the exact Speech bus survives reload");
        let rendered_before = rendered_quanta.load(Ordering::Acquire);
        let non_silent_before = non_silent_quanta.load(Ordering::Acquire);
        let render_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while rendered_quanta.load(Ordering::Acquire) == rendered_before
            || non_silent_quanta.load(Ordering::Acquire) == non_silent_before
        {
            assert!(
                tokio::time::Instant::now() < render_deadline,
                "sibling rendering stopped during reload cycle {cycle}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        sibling_tx
            .as_ref()
            .expect("sibling published its runtime")
            .send(RuntimeAction::Send(Arc::new(
                "full_reload_ping".to_string(),
            )))
            .expect("sibling accepts a per-cycle action");
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let event = sibling_events.next().await.expect("sibling remains responsive");
                if let SessionEvent::UpdateBuffer(updates) = event.event
                    && updates.iter().any(|update| {
                        matches!(update, BufferUpdate::Append(line) if line.text == "FULL_RELOAD_SIBLING_PONG")
                    })
                {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("sibling stopped responding during reload cycle {cycle}"));
        assert_eq!(application.usage(), loaded);
        assert_eq!(probe.start_count(), 1);
        assert_eq!(probe.play_count(), 1);

        if cycle < 4 {
            cross_reload_render_threshold = Some((
                rendered_quanta.load(Ordering::Acquire),
                non_silent_quanta.load(Ordering::Acquire),
            ));
            reload_tx
                .as_ref()
                .expect("reloader published its runtime")
                .send(RuntimeAction::Reload)
                .expect("reloader accepts full-session reload");
        }
    }

    reload_tx
        .take()
        .expect("reloader published its runtime")
        .send(RuntimeAction::Shutdown)
        .expect("reloader accepts shutdown");
    drop(reload_events);
    let reload_join = tokio::task::spawn_blocking(move || join_runtime_thread(reload_id))
        .await
        .expect("reloader join task does not panic");
    assert_eq!(
        reload_join,
        RuntimeThreadJoinOutcome::Clean {
            session_id: reload_id
        }
    );
    assert_eq!(
        reload_registration.set_package_gain_muted(
            previous_alpha_key
                .as_ref()
                .expect("the final generation published a package key"),
            false,
        ),
        Err(smudgy_audio_web::PackageAudioControlError::StalePackage),
        "dropping the final sandbox isolate deactivates its exact lease"
    );
    wait_for_exact_audio_usage(
        &application,
        sibling_baseline,
        "final reloader generation retirement",
    )
    .await;
    prove_full_script_bus_reuse(&reload_script_bus);
    reload_registration
        .retire()
        .await
        .expect("reloader mixer session retires exactly");

    sibling_tx
        .take()
        .expect("sibling published its runtime")
        .send(RuntimeAction::Shutdown)
        .expect("sibling accepts shutdown");
    drop(sibling_events);
    let sibling_join = tokio::task::spawn_blocking(move || join_runtime_thread(sibling_id))
        .await
        .expect("sibling join task does not panic");
    assert_eq!(
        sibling_join,
        RuntimeThreadJoinOutcome::Clean {
            session_id: sibling_id
        }
    );
    wait_for_exact_audio_usage(
        &application,
        empty_baseline,
        "empty application host baseline",
    )
    .await;
    sibling_registration
        .retire()
        .await
        .expect("sibling mixer session retires exactly");

    drop(reload_scope);
    drop(reload_script_bus);
    drop(reload_native_bus);
    drop(reload_speech_bus);
    drop(on_engine_rebuild);
    assert!(
        Arc::get_mut(&mut application)
            .expect("all scoped application authorities are retired")
            .seal()
    );
    assert_eq!(application.usage(), empty_baseline);

    render_done.store(true, Ordering::Release);
    render_thread.join().expect("fake physical renderer joins");
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(probe.start_count(), 1);
    assert_eq!(probe.play_count(), 1);
    assert_eq!(probe.close_count(), 1);
}

/// Full-session replacement destroys old main/package heaps before their Script slots can be
/// reused. A later process-output death closes every replacement default context exactly once,
/// while a device-free sibling remains independently controllable in its own runtime.
#[cfg(feature = "web-audio")]
#[tokio::test]
#[allow(clippy::await_holding_lock, clippy::too_many_lines)]
async fn full_reload_device_death_preserves_event_and_isolate_boundaries() {
    let _test_guard = PACKAGE_ISOLATE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let target_server = "pi_s4c_failure_security";
    let sibling_server = "pi_s4c_silent_sibling";
    for server in [target_server, sibling_server] {
        std::fs::create_dir_all(home.join(server).join("modules"))
            .expect("create module directory");
        std::fs::create_dir_all(home.join(server).join("logs")).expect("create log directory");
    }

    std::fs::write(
        home.join(sibling_server).join("modules/sibling.ts"),
        r#"
        import { createAlias, echo } from "smudgy:core";
        const context = new AudioContext({ sampleRate: 48_000, sinkId: "none" });
        if (context.sinkId !== "none") throw new Error("silent sibling did not keep its sink");
        createAlias("^s4c_sibling_status$", () => {
          echo("S4C_SIBLING_STATE:" + context.state);
        });
        createAlias("^s4c_sibling_ping$", () => echo("S4C_SIBLING_PONG"));
        createAlias("^s4c_sibling_close$", async () => {
          await context.close();
          echo("S4C_SIBLING_CLOSED:" + context.state);
        });
        echo("S4C_SIBLING_READY");
        "#,
    )
    .expect("write silent sibling module");
    std::fs::write(
        home.join(target_server).join("modules/main.ts"),
        r#"
        import { createAlias, echo } from "smudgy:core";

        const cycleKey = "smudgy-s4c-main-cycle";
        const generation = Number(localStorage.getItem(cycleKey) ?? "0") + 1;
        localStorage.setItem(cycleKey, String(generation));
        if (Object.prototype.hasOwnProperty.call(globalThis, "__s4c_main_heap_brand")) {
          throw new Error("the old main heap survived full-session reload");
        }
        globalThis.__s4c_main_heap_brand = Symbol("main-generation-" + generation);
        globalThis.addEventListener("error", (event) => {
          if (event.error?.message === "intentional S4c event-handler failure") {
            echo("S4C_THROWING_HANDLER_REPORTED");
            event.preventDefault();
          }
        });

        const primary = new AudioContext({ sampleRate: 48_000, sinkId: "" });
        const eventContext = new AudioContext({ sampleRate: 48_000, sinkId: "" });
        for (const context of [primary, eventContext]) {
          if (!(context instanceof AudioContext)
              || Object.getPrototypeOf(context) !== AudioContext.prototype) {
            throw new Error("main received a stale or foreign AudioContext brand");
          }
        }
        globalThis.__s4c_main_context = primary;

        if (generation === 1) {
          primary.onstatechange = () => {
            if (primary.state !== "closed") return;
            const current = Number(localStorage.getItem(cycleKey) ?? "0");
            echo(current > generation
              ? "S4C_FORBIDDEN_OLD_STATE_AFTER_REPLACEMENT"
              : "S4C_MAIN_OLD_STATE_DURING_RETIREMENT:1");
            primary.resume().then(
              () => echo("S4C_FORBIDDEN_OLD_CONTEXT_REOPENED"),
              () => {},
            );
          };
          const oldSource = primary.createOscillator();
          oldSource.connect(primary.destination);
          oldSource.onended = () => {
            // Root a cancelled JS waiter which captures the old context. Runtime replacement,
            // rather than script-authored close or promise observation, must release it.
            globalThis.__s4c_old_waiter = (async () => {
              echo("S4C_MAIN_OLD_HANDLER_ENTERED:1");
              await new Promise(() => {});
              if (Number(localStorage.getItem(cycleKey) ?? "0") > generation) {
                echo("S4C_FORBIDDEN_OLD_ENDED_AFTER_REPLACEMENT");
              }
              await primary.resume();
              echo("S4C_FORBIDDEN_OLD_CONTEXT_REOPENED");
            })();
          };
          oldSource.start();
          oldSource.stop(primary.currentTime + 0.02);
        } else {
          primary.onstatechange = () => {
            if (primary.state === "closed") echo("S4C_MAIN_PRIMARY_CLOSED:2");
          };
          eventContext.onstatechange = () => {
            if (eventContext.state === "closed") echo("S4C_MAIN_EVENT_CLOSED:2");
          };

          const gain = eventContext.createGain();
          gain.gain.value = 0;
          gain.connect(eventContext.destination);
          const throwing = eventContext.createOscillator();
          const following = eventContext.createOscillator();
          throwing.connect(gain);
          following.connect(gain);
          throwing.onended = () => {
            echo("S4C_THROWING_HANDLER_ENTERED");
            throw new Error("intentional S4c event-handler failure");
          };
          following.onended = () => echo("S4C_HANDLER_AFTER_THROW");
          throwing.start();
          following.start();
          const end = eventContext.currentTime + 0.02;
          throwing.stop(end);
          following.stop(end + 0.02);
        }

        createAlias("^s4c_main_status$", () => {
          echo("S4C_MAIN_PRIMARY_STATE:" + primary.state);
          echo("S4C_MAIN_EVENT_STATE:" + eventContext.state);
        });
        echo("S4C_MAIN_READY:" + generation);
        "#,
    )
    .expect("write target main module");

    let packages = vec![
        TestPackage::new(
            "s4c",
            "alpha",
            "1.0.0",
            &s4c_package_module("ALPHA", "BETA"),
        ),
        TestPackage::new("s4c", "beta", "1.0.0", &s4c_package_module("BETA", "ALPHA")),
    ];
    for package in &packages {
        let specifier = format!("smudgy://{}/{}", package.owner, package.name);
        shared_packages::install_package(target_server, &specifier, UpdateMode::Auto, true)
            .expect("install S4c package");
        shared_packages::record_consent(
            target_server,
            &specifier,
            &PackagePermissions {
                smudgy: SmudgyCapabilities::all(),
                ..Default::default()
            },
        )
        .expect("grant package test capabilities");
    }
    let package_factory: PackageProviderFactory = Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        for package in &packages {
            provider.insert(ResolvedPackage {
                key: PackageKey {
                    owner: package.owner.to_string(),
                    name: package.name.to_string(),
                },
                resolved_version: package.version.to_string(),
                manifest: package.manifest(),
                integrity: format!("s4c-{}-{}", package.name, package.version),
                modules: package
                    .modules
                    .iter()
                    .map(|(subpath, text)| PackageModuleSource {
                        subpath: (*subpath).to_string(),
                        text: text.clone(),
                    })
                    .collect(),
            });
        }
        Rc::new(provider) as Rc<dyn PackageProvider>
    });

    let (service, probe) = smudgy_audio::test_support::start_test_mixer(
        48_000,
        smudgy_audio::test_support::TestDriverConfig::default(),
    )
    .expect("fake process mixer starts");
    let render_done = Arc::new(AtomicBool::new(false));
    let render_thread = {
        let render_done = Arc::clone(&render_done);
        let probe = probe.clone();
        thread::spawn(move || {
            let mut output = [0.0; 256];
            while !render_done.load(Ordering::Acquire) {
                let _ = probe.render(&mut output, 2);
                thread::yield_now();
            }
        })
    };

    let mut application = Arc::new(smudgy_audio_web::ApplicationAudioOwner::new(
        deno_audio::AudioHostLimits::unlimited()
            .max_online_contexts(Some(5))
            .max_live_audio_bytes(Some(8 * 1024 * 1024))
            .max_graph_nodes(Some(128))
            .max_graph_connections(Some(128))
            .max_scheduled_sources(Some(32))
            .max_automation_events(Some(128))
            .max_queued_control_commands(Some(128))
            .max_queued_events(Some(128))
            .max_decode_jobs(Some(1))
            .max_offline_render_jobs(Some(1)),
    ));
    let empty_baseline = application.usage();
    assert_eq!(empty_baseline, deno_audio::AudioHostUsage::default());

    let sibling_numeric_id = 9_223;
    let sibling_id = SessionId::from(sibling_numeric_id);
    let sibling_owner = service
        .add_session(smudgy_audio::AudioSessionId(u64::from(sibling_numeric_id)))
        .expect("silent sibling joins mixer ownership");
    let sibling_registration = application
        .registrar()
        .register_session(sibling_owner)
        .expect("silent sibling audio registration succeeds");
    let sibling_scope = sibling_registration.scope();
    let sibling_params = Arc::new(SessionParams {
        session_id: sibling_id,
        server_name: Arc::new(sibling_server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut sibling_events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>> = Box::pin(
        smudgy_core::session::spawn_with_audio(sibling_params, sibling_scope.clone())
            .expect("silent sibling audio scope matches"),
    );
    let mut sibling_transcript = Vec::new();
    let sibling_tx = loop {
        let event = sibling_events.next().await.expect("silent sibling starts");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        sibling_transcript.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };
    wait_for_audio_markers(
        &mut sibling_events,
        &mut sibling_transcript,
        &["S4C_SIBLING_READY".to_string()],
        "silent sibling readiness",
    )
    .await;
    let sibling_baseline =
        wait_for_quiescent_audio_usage(&application, 1, "silent sibling baseline").await;
    assert_eq!(sibling_baseline.online_contexts(), 1);
    assert_eq!(sibling_baseline.scheduled_sources(), 0);

    let target_numeric_id = 9_224;
    let target_id = SessionId::from(target_numeric_id);
    let target_owner = service
        .add_session(smudgy_audio::AudioSessionId(u64::from(target_numeric_id)))
        .expect("target joins mixer ownership");
    let target_script_bus = target_owner.script_bus();
    let target_registration = application
        .registrar()
        .register_session(target_owner)
        .expect("target audio registration succeeds");
    let target_scope = target_registration.scope();
    let rebuild_count = Arc::new(AtomicUsize::new(0));
    let on_engine_rebuild: Arc<dyn Fn() + Send + Sync> = {
        let application = Arc::clone(&application);
        let target_script_bus = target_script_bus.clone();
        let rebuild_count = Arc::clone(&rebuild_count);
        Arc::new(move || {
            assert_eq!(
                application.usage(),
                sibling_baseline,
                "old target generation must return every one of the ten host counters"
            );
            prove_full_script_bus_reuse(&target_script_bus);
            rebuild_count.fetch_add(1, Ordering::AcqRel);
        })
    };
    let target_params = Arc::new(SessionParams {
        session_id: target_id,
        server_name: Arc::new(target_server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: Some(Arc::clone(&on_engine_rebuild)),
    });
    let mut target_events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>> = Box::pin(
        spawn_with_package_provider_and_audio(target_params, package_factory, target_scope.clone())
            .expect("target audio scope matches"),
    );
    let mut target_transcript = Vec::new();
    let target_tx = loop {
        let event = target_events.next().await.expect("target starts");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        target_transcript.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };

    wait_for_audio_markers(
        &mut target_events,
        &mut target_transcript,
        &[
            "S4C_MAIN_READY:1".to_string(),
            "S4C_ALPHA_READY:1".to_string(),
            "S4C_BETA_READY:1".to_string(),
            "S4C_MAIN_OLD_HANDLER_ENTERED:1".to_string(),
            "S4C_ALPHA_OLD_HANDLER_ENTERED:1".to_string(),
            "S4C_BETA_OLD_HANDLER_ENTERED:1".to_string(),
        ],
        "first generation and rooted old event waiter",
    )
    .await;
    assert_eq!(rebuild_count.load(Ordering::Acquire), 1);
    wait_for_quiescent_audio_usage(&application, 5, "first loaded generation").await;
    prove_script_bus_occupancy(&target_script_bus, 4);

    target_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_alpha_suspend".to_string(),
        )))
        .expect("target accepts alpha suspend");
    wait_for_audio_markers(
        &mut target_events,
        &mut target_transcript,
        &["S4C_ALPHA_SUSPENDED:suspended".to_string()],
        "alpha-only suspend",
    )
    .await;
    target_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_package_status".to_string(),
        )))
        .expect("target accepts package status");
    wait_for_audio_markers(
        &mut target_events,
        &mut target_transcript,
        &[
            "S4C_ALPHA_STATE:suspended".to_string(),
            "S4C_BETA_STATE:running".to_string(),
        ],
        "independent package control state",
    )
    .await;
    target_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_alpha_resume".to_string(),
        )))
        .expect("target accepts alpha resume");
    wait_for_audio_markers(
        &mut target_events,
        &mut target_transcript,
        &["S4C_ALPHA_RESUMED:running".to_string()],
        "alpha-only resume",
    )
    .await;

    target_tx
        .send(RuntimeAction::Reload)
        .expect("target accepts full-session reload");
    wait_for_audio_markers(
        &mut target_events,
        &mut target_transcript,
        &[
            "S4C_MAIN_READY:2".to_string(),
            "S4C_ALPHA_READY:2".to_string(),
            "S4C_BETA_READY:2".to_string(),
            "S4C_THROWING_HANDLER_ENTERED".to_string(),
            "S4C_THROWING_HANDLER_REPORTED".to_string(),
            "S4C_HANDLER_AFTER_THROW".to_string(),
        ],
        "replacement generation and contained throwing handler",
    )
    .await;
    assert_eq!(rebuild_count.load(Ordering::Acquire), 2);
    let throwing_index = target_transcript
        .iter()
        .position(|line| line == "S4C_THROWING_HANDLER_ENTERED")
        .expect("throwing handler marker exists");
    let following_index = target_transcript
        .iter()
        .position(|line| line == "S4C_HANDLER_AFTER_THROW")
        .expect("following handler marker exists");
    assert!(
        throwing_index < following_index,
        "a thrown event handler must not block the following handler; transcript: {target_transcript:#?}"
    );
    assert!(
        !target_transcript
            .iter()
            .any(|line| line.starts_with("S4C_FORBIDDEN_")),
        "old-generation event/context authority escaped replacement: {target_transcript:#?}"
    );
    wait_for_quiescent_audio_usage(&application, 5, "replacement loaded generation").await;
    prove_script_bus_occupancy(&target_script_bus, 4);

    sibling_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_sibling_status".to_string(),
        )))
        .expect("sibling accepts pre-failure status");
    wait_for_audio_markers(
        &mut sibling_events,
        &mut sibling_transcript,
        &["S4C_SIBLING_STATE:running".to_string()],
        "pre-failure silent sibling state",
    )
    .await;

    assert!(probe.fail_output());
    wait_for_audio_markers(
        &mut target_events,
        &mut target_transcript,
        &[
            "S4C_MAIN_PRIMARY_CLOSED:2".to_string(),
            "S4C_ALPHA_CLOSED:2".to_string(),
            "S4C_BETA_CLOSED:2".to_string(),
        ],
        "physical failure closure of observable main and package contexts",
    )
    .await;
    // The intentionally throwing context has a deliberately lossy JS consumer: a later callback
    // cannot be cleanup authority. The exact five-to-one host transition proves that context's
    // native endpoint, graph, event queue, and permits retired along with the observable three.
    wait_for_exact_audio_usage(
        &application,
        sibling_baseline,
        "all physical contexts settled while sink:none stayed live",
    )
    .await;

    target_tx
        .send(RuntimeAction::Send(Arc::new("s4c_main_status".to_string())))
        .expect("target runtime accepts post-failure status");
    target_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_package_status".to_string(),
        )))
        .expect("package runtimes accept post-failure status");
    wait_for_audio_markers(
        &mut target_events,
        &mut target_transcript,
        &[
            "S4C_MAIN_PRIMARY_STATE:closed".to_string(),
            "S4C_ALPHA_STATE:closed".to_string(),
            "S4C_BETA_STATE:closed".to_string(),
        ],
        "post-failure public context state",
    )
    .await;
    assert!(
        !target_transcript
            .iter()
            .any(|line| line.starts_with("S4C_FORBIDDEN_")),
        "a delayed old-generation event/context authority escaped replacement: {target_transcript:#?}"
    );
    sibling_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_sibling_status".to_string(),
        )))
        .expect("silent sibling accepts post-failure status");
    sibling_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_sibling_ping".to_string(),
        )))
        .expect("silent sibling accepts post-failure ping");
    wait_for_audio_markers(
        &mut sibling_events,
        &mut sibling_transcript,
        &[
            "S4C_SIBLING_STATE:running".to_string(),
            "S4C_SIBLING_PONG".to_string(),
        ],
        "silent sibling continuity after physical failure",
    )
    .await;
    sibling_tx
        .send(RuntimeAction::Send(Arc::new(
            "s4c_sibling_close".to_string(),
        )))
        .expect("silent sibling accepts independent close");
    wait_for_audio_markers(
        &mut sibling_events,
        &mut sibling_transcript,
        &["S4C_SIBLING_CLOSED:closed".to_string()],
        "independent silent sibling close",
    )
    .await;
    wait_for_exact_audio_usage(&application, empty_baseline, "empty final host baseline").await;
    let final_usage = application.usage();
    assert_eq!(final_usage.online_contexts(), 0);
    assert_eq!(final_usage.live_audio_bytes(), 0);
    assert_eq!(final_usage.graph_nodes(), 0);
    assert_eq!(final_usage.graph_connections(), 0);
    assert_eq!(final_usage.scheduled_sources(), 0);
    assert_eq!(final_usage.automation_events(), 0);
    assert_eq!(final_usage.queued_control_commands(), 0);
    assert_eq!(final_usage.queued_events(), 0);
    assert_eq!(final_usage.decode_jobs(), 0);
    assert_eq!(final_usage.offline_render_jobs(), 0);

    target_tx
        .send(RuntimeAction::Shutdown)
        .expect("target accepts shutdown");
    sibling_tx
        .send(RuntimeAction::Shutdown)
        .expect("silent sibling accepts shutdown");
    drop(target_tx);
    drop(sibling_tx);
    drop(target_events);
    drop(sibling_events);
    for (session_id, description) in [(target_id, "target"), (sibling_id, "silent sibling")] {
        let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
            .await
            .unwrap_or_else(|_| panic!("{description} runtime join task panicked"));
        assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });
    }

    // The stopped owner no longer accepts retirement requests, but its joined terminal cleanup
    // resolves the exact generation receipts for both the failed and silent sessions.
    assert_eq!(target_registration.retire().await, Ok(()));
    assert_eq!(sibling_registration.retire().await, Ok(()));
    drop(target_scope);
    drop(sibling_scope);
    drop(target_script_bus);
    drop(on_engine_rebuild);
    assert!(
        Arc::get_mut(&mut application)
            .expect("all application audio scopes retired")
            .seal()
    );
    assert_eq!(application.usage(), empty_baseline);

    render_done.store(true, Ordering::Release);
    render_thread.join().expect("fake renderer joins");
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(
        shutdown.failure,
        Some(smudgy_audio::MixerOutputFailure::BackendFailure)
    );
    assert_eq!(probe.start_count(), 1);
    assert_eq!(probe.play_count(), 1);
    assert_eq!(probe.close_count(), 1);
}

/// Cross-isolate depth-first ordering with a real second isolate: a main-isolate alias
/// `send`s a command that a sandboxed package's alias matches and expands. Depth-first order must
/// hold across the boundary (the package's expansion completes before the main alias's sibling
/// command), exactly as `command_ordering.rs` asserts for synthetic isolates — but here the package
/// handler genuinely executes in the second isolate via `call_javascript_function`. The package's
/// handler only emits `deep_a`/`deep_b` when it CANNOT see main's `globalThis` marker, so a no-op
/// sandbox (one shared heap) would emit `LEAK` instead and break the ordering assertion — making the
/// real-second-isolate requirement load-bearing rather than incidental.
#[tokio::test]
async fn depth_first_ordering_holds_across_real_isolate() {
    let main_mod = r#"
        import { createAlias, send } from "smudgy:core";
        globalThis.__leak_marker = "MAIN";
        createAlias("^outer$", () => { send("deep"); send("tail"); });
    "#;
    let pkg = TestPackage::new(
        "wbk",
        "inc",
        "1.0.0",
        r#"
        import { createAlias, send, echo } from "smudgy:core";
        createAlias("^deep$", () => {
            if (globalThis.__leak_marker) { send("LEAK"); }
            else { send("deep_a"); send("deep_b"); }
        });
        echo("PKG_READY");
        "#,
    );

    let lines = run_scenario(
        9202,
        "pi_sandbox_ordering",
        &[("main_mod.ts", main_mod)],
        vec![pkg],
        "PKG_READY",
        1,
        "outer",
    )
    .await;

    let order: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|l| matches!(*l, "deep_a" | "deep_b" | "tail"))
        .collect();
    assert_eq!(
        order,
        vec!["deep_a", "deep_b", "tail"],
        "the sandboxed package's expansion must finish before the main alias's sibling command; transcript:\n{lines:#?}"
    );
}

/// A **disabled** install is excluded from the rebuilt isolate set (`build_isolate_plan` skips
/// `!enabled` roots — `PACKAGE-ISOLATES-CONSENT-TRUST.md`): the package is
/// still resolvable (present in the provider), but the engine must neither evaluate its modules nor
/// register its automations. An *enabled* sibling supplies the gate; the disabled one shares the
/// alias name `ping`, so if it had loaded its `DEAD` handler would also fire. Asserting `DEAD_READY`
/// (its top-level echo) and `DEAD` (its alias) are absent proves the disabled isolate never built.
#[tokio::test]
async fn disabled_package_is_excluded_from_the_isolate_set() {
    let live = TestPackage::new(
        "wbk",
        "live",
        "1.0.0",
        r#"
        import { createAlias, echo } from "smudgy:core";
        createAlias("^ping$", () => { echo("PONG"); });
        echo("LIVE_READY");
        "#,
    );
    let dead = TestPackage::new(
        "wbk",
        "dead",
        "1.0.0",
        r#"
        import { createAlias, echo } from "smudgy:core";
        createAlias("^ping$", () => { echo("DEAD"); });
        echo("DEAD_READY");
        "#,
    )
    .disabled();

    let lines = run_scenario(
        9203,
        "pi_sandbox_disabled",
        &[],
        vec![live, dead],
        "LIVE_READY",
        1,
        "ping",
    )
    .await;

    assert!(
        lines.iter().any(|l| l == "PONG"),
        "the enabled package's alias must fire; transcript:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l == "DEAD_READY"),
        "the disabled package's module must never evaluate; transcript:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l == "DEAD"),
        "the disabled package's alias must never register or fire; transcript:\n{lines:#?}"
    );
}

/// Function isolation: the *same* package runs in two isolates (the sandboxed install,
/// and a copy a local module pulled into main by importing it), each with its own module-global.
/// Bumping the main copy's counter must not be visible to the sandboxed copy — proving the
/// coexistence is real heap isolation, not a shared instance.
#[tokio::test]
async fn module_global_is_isolated_between_copies() {
    // The local module imports the package (→ a copy of it runs in main), bumps that copy's
    // counter, and exposes the bump under its own alias name.
    let main_mod = r#"
        import { createAlias, echo } from "smudgy:core";
        import { bump } from "smudgy://wbk/inc";
        createAlias("^main_bump$", () => { bump(); echo("bumped"); });
    "#;
    // The package keeps a module-global counter and reports it under a coexisting alias name.
    let pkg = TestPackage::new(
        "wbk",
        "inc",
        "1.0.0",
        r#"
        import { createAlias, echo } from "smudgy:core";
        let n = 0;
        export function bump() { n += 1; return n; }
        createAlias("^inc_report$", () => { echo("inc=" + n); });
        echo("INC_READY");
        "#,
    );

    // `inc` loads into two isolates (main, via the import, and its own sandbox), so it echoes
    // INC_READY twice; wait for both so all three aliases are registered before driving.
    let lines = run_scenario(
        9203,
        "pi_sandbox_isolation",
        &[("main_mod.ts", main_mod)],
        vec![pkg],
        "INC_READY",
        2,
        "main_bump;inc_report",
    )
    .await;

    assert!(
        lines.iter().any(|l| l == "inc=1"),
        "the main copy's counter must read 1 after its bump; transcript:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "inc=0"),
        "the sandboxed copy's counter must still read 0 (separate heap); transcript:\n{lines:#?}"
    );
}

/// Regression — a sandboxed package that throws at module-eval must be **skipped** (its isolate
/// dropped, a notice emitted) and the session must keep running. The isolate is left "exited"
/// between ops (Model B), so dropping it on the load-failure path without first making it the
/// thread's current isolate would trip `rusty_v8`'s `OwnedIsolate::Drop` assert and **abort the whole
/// process** — which here would crash the test binary rather than fail gracefully. So a clean run
/// (main's `ping`→`pong` still fires, plus the failure notice) is the proof the failure path is
/// safe. (`core/src/session/runtime/script_engine.rs` enters the isolate before dropping it.)
#[tokio::test]
async fn failing_sandboxed_package_is_skipped_without_aborting() {
    let main_mod = r#"
        import { createAlias, echo } from "smudgy:core";
        createAlias("^ping$", () => { echo("pong"); });
        echo("MAIN_READY");
    "#;
    // A static import of a package the provider doesn't have → graph load fails synchronously, so
    // `load_modules` returns Err during construction and the engine drops this isolate in place
    // (the path that aborts without the enter-before-drop fix). A *runtime* top-level throw instead
    // surfaces as an async rejection during pumping (a separate, non-aborting path).
    let broken = TestPackage::new(
        "wbk",
        "broken",
        "1.0.0",
        r#"import "smudgy://wbk/no_such_dependency";"#,
    );

    let lines = run_scenario(
        9204,
        "pi_sandbox_failload",
        &[("main_mod.ts", main_mod)],
        vec![broken],
        "MAIN_READY",
        1,
        "ping",
    )
    .await;

    assert!(
        lines.iter().any(|l| l == "pong"),
        "the session must survive a failing sandboxed package and keep firing main aliases; transcript:\n{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("[package] broken failed to load")),
        "the failing package must be reported (skipped, not silently ignored); transcript:\n{lines:#?}"
    );
}

/// `singleton` dedupe across isolates: the SAME package runs in two isolates
/// (its sandboxed install, and a copy a local module pulled into main by importing it) and each
/// copy calls `createAlias("^dup$", …, { singleton: true })`. The singleton identity (here the
/// derived name, i.e. the pattern source) drops the
/// isolate dimension and the version (`PACKAGE-ISOLATES.md`), so exactly ONE `dup` registers
/// session-wide: the first copy's op returns `created === true`, the second returns `false` and
/// no-ops. Firing `dup` then echoes once. Without the flag (second scenario) the two copies
/// coexist and both fire — the default behavior.
#[tokio::test]
async fn singleton_dedupes_same_package_across_isolates() {
    // --- With `{ singleton: true }`: exactly one copy registers; the other reports it existed. ---
    let pkg = TestPackage::new(
        "wbk",
        "widget",
        "1.0.0",
        r#"
        import { createAlias, echo } from "smudgy:core";
        const a = createAlias("^dup$", () => { echo("fired_dup"); }, { singleton: true });
        echo(a.created ? "created_true" : "created_false");
        echo("PKG_READY");
        "#,
    );
    let lines = run_scenario(
        9211,
        "pi_singleton_dedupe",
        // Importing the package pulls a second copy of it into the main isolate.
        &[("main_mod.ts", r#"import "smudgy://wbk/widget";"#)],
        vec![pkg],
        "PKG_READY",
        2,
        "dup",
    )
    .await;

    assert_eq!(
        lines.iter().filter(|l| *l == "created_true").count(),
        1,
        "exactly one copy must win the singleton reservation; transcript:\n{lines:#?}"
    );
    assert_eq!(
        lines.iter().filter(|l| *l == "created_false").count(),
        1,
        "the second copy's singleton create must report it already existed; transcript:\n{lines:#?}"
    );
    assert_eq!(
        lines.iter().filter(|l| *l == "fired_dup").count(),
        1,
        "only the one registered `dup` alias may fire; transcript:\n{lines:#?}"
    );

    // --- Without the flag: the two copies coexist and BOTH fire (the default). ---
    let pkg = TestPackage::new(
        "wbk",
        "gadget",
        "1.0.0",
        r#"
        import { createAlias, echo } from "smudgy:core";
        createAlias("^dup$", () => { echo("fired_dup"); });
        echo("PKG_READY");
        "#,
    );
    let lines = run_scenario(
        9212,
        "pi_singleton_coexist",
        &[("main_mod.ts", r#"import "smudgy://wbk/gadget";"#)],
        vec![pkg],
        "PKG_READY",
        2,
        "dup",
    )
    .await;

    assert_eq!(
        lines.iter().filter(|l| *l == "fired_dup").count(),
        2,
        "without `singleton` the two coexisting copies must both fire; transcript:\n{lines:#?}"
    );
}

/// Cross-isolate `off` is unforgeable: package A and package B each subscribe to the SAME event
/// (`user#evt`, emitted by a main-isolate module). Because each package's `on` is the FIRST function
/// it registers, both subscribers get the SAME numeric token (`FunctionId(0)`) in their own
/// per-isolate `script_functions`. B then `off()`s — passing token `0` — which must drop ONLY B's own
/// subscription (the `(isolate, function_id)` scoping in `op_smudgy_off`); A's identically-numbered
/// subscription lives in a different isolate, so it survives. If the guard matched on the raw token
/// alone, B's `off(0)` would also remove A's `0` and `A_FIRED` would vanish — so A still firing is the
/// proof the isolate dimension is load-bearing. (Also exercises main-isolate `emit` → package-isolate
/// `on` cross-boundary delivery, and `off` on the `op_smudgy_emit` path.)
#[tokio::test]
async fn off_token_is_scoped_to_its_isolate() {
    // Main isolate: an alias that emits `user#evt` (a module's event handle is stamped to
    // `user#…`).
    let main_mod = r#"
        import { createAlias, createEvent, echo } from "smudgy:core";
        const evt = createEvent("evt");
        createAlias("^fire$", () => { evt.emit({}); });
        echo("READY");
    "#;
    // Package A subscribes and stays subscribed. Its `.on` is its first registered function →
    // FunctionId 0.
    let pkg_a = TestPackage::new(
        "wbk",
        "alpha",
        "1.0.0",
        r#"
        import { events, echo } from "smudgy:core";
        events.lookup("user", "evt").on(() => { echo("A_FIRED"); });
        echo("READY");
        "#,
    );
    // Package B subscribes (also FunctionId 0 in its OWN isolate) then immediately unsubscribes
    // itself with that same token. Its `off(user#evt, 0)` must not touch A's `0` in another isolate.
    let pkg_b = TestPackage::new(
        "wbk",
        "beta",
        "1.0.0",
        r#"
        import { events, echo } from "smudgy:core";
        const sub = events.lookup("user", "evt").on(() => { echo("B_FIRED"); });
        sub.off();
        echo("READY");
        "#,
    );

    // Gate on all three modules signalling READY (main + A + B) so every subscription / B's
    // unsubscription and the `fire` alias are live before the event is emitted.
    let lines = run_scenario(
        9221,
        "pi_off_forgery",
        &[("main_mod.ts", main_mod)],
        vec![pkg_a, pkg_b],
        "READY",
        3,
        "fire",
    )
    .await;

    assert!(
        lines.iter().any(|l| l == "A_FIRED"),
        "package A's subscription must survive B's same-token off() (isolate-scoped removal); transcript:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l == "B_FIRED"),
        "package B unsubscribed itself, so its handler must not fire; transcript:\n{lines:#?}"
    );
}

/// Like [`run_scenario`] but the caller supplies the package-provider `factory` directly (plus the
/// specifiers to install untrusted), instead of one built from a fixed `TestPackage` set. Needed
/// when the two isolates must resolve the *same* package key to *different* versions — which a
/// single fixed set can't express (`InMemoryPackageProvider` keeps one "latest" per key), but a
/// call-order-stateful factory can.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn run_with_factory(
    session_id: u32,
    server: &str,
    local_modules: &[(&str, &str)],
    install_specifiers: &[&str],
    factory: PackageProviderFactory,
    gate: &str,
    gate_count: usize,
    input: &str,
) -> Vec<String> {
    let _test_guard = PACKAGE_ISOLATE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let session_id = SessionId::from(session_id);

    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let server_dir = home.join(server);
    let modules_dir = server_dir.join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(server_dir.join("logs")).unwrap();
    for (name, source) in local_modules {
        std::fs::write(modules_dir.join(name), source).unwrap();
    }
    for spec in install_specifiers {
        shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
        // Grant the full smudgy capability set so the sandboxed packages can use the gated
        // ops these singleton/coexistence tests rely on (`createAlias` / `echo`). See `run_scenario`.
        shared_packages::record_consent(
            server,
            spec,
            &PackagePermissions {
                smudgy: SmudgyCapabilities::all(),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let params = Arc::new(SessionParams {
        session_id,
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn_with_package_provider(params, factory));
    let mut lines: Vec<String> = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };

    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: true,
        bold_is_bright: false,
        script_settings: Box::new(smudgy_core::models::settings::ScriptSettings::default()),
    })
    .unwrap();

    let mut seen_gate = 0usize;
    let mut sent = false;
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                    if !sent && line.text == gate {
                        seen_gate += 1;
                        if seen_gate >= gate_count {
                            tx.send(RuntimeAction::Send(Arc::new(input.to_string())))
                                .unwrap();
                            sent = true;
                        }
                    }
                }
            }
        }
    }
    tx.send(RuntimeAction::Shutdown)
        .expect("runtime accepts shutdown");
    drop(tx);
    drop(events);
    let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
        .await
        .expect("runtime join task does not panic");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });

    assert!(
        sent,
        "gate sentinel {gate:?} (x{gate_count}) was never observed; transcript:\n{lines:#?}"
    );
    lines
}

/// `singleton` collapses across *versions*: `wbk/mapper@1` runs in one isolate
/// and `wbk/mapper@2` in another, and each singleton-registers `heal`. Because the singleton key
/// drops the version (`PACKAGE-ISOLATES.md` — dedupe scope is `(owner, name)`, not
/// `(owner, name, major)`), the two collapse to a single live `heal`. The in-memory provider keeps
/// only one "latest" per key, so the factory hands its two construction-order calls (main first,
/// then the sandbox) different versions; each version's source echoes its own version + whether it
/// won the reservation.
#[tokio::test]
async fn singleton_collapses_across_versions() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Call #0 (main's loader) → mapper@1; call #1 (the sandbox) → mapper@2. Each version echoes
    // its own number and whether its singleton `heal` was created or already existed.
    let call = Arc::new(AtomicUsize::new(0));
    let factory: PackageProviderFactory = Arc::new(move || {
        let n = call.fetch_add(1, Ordering::SeqCst);
        let version = if n == 0 { "1.0.0" } else { "2.0.0" };
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(ResolvedPackage {
            key: PackageKey {
                owner: "wbk".to_string(),
                name: "mapper".to_string(),
            },
            resolved_version: version.to_string(),
            manifest: PackageManifest::parse(&format!(
                "{{ \"name\": \"mapper\", \"version\": \"{version}\" }}"
            ))
            .expect("valid manifest"),
            integrity: format!("test-mapper-{version}"),
            modules: vec![PackageModuleSource {
                subpath: "index.js".to_string(),
                text: format!(
                    r#"
                    import {{ createAlias, echo }} from "smudgy:core";
                    const a = createAlias("^heal$", () => {{ echo("healed"); }}, {{ singleton: true }});
                    echo("mapper {version}: " + (a.created ? "created" : "existed"));
                    echo("MAPPER_READY");
                    "#
                ),
            }],
        });
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    });

    let lines = run_with_factory(
        9213,
        "pi_singleton_versions",
        // Importing mapper pulls a (different-versioned) copy into the main isolate.
        &[("main_mod.ts", r#"import "smudgy://wbk/mapper";"#)],
        &["smudgy://wbk/mapper"],
        factory,
        "MAPPER_READY",
        2,
        "heal",
    )
    .await;

    // Both versions really evaluated (two heaps), proving these are two versions, not two copies.
    assert!(
        lines.iter().any(|l| l.starts_with("mapper 1.0.0:")),
        "mapper@1 must have loaded; transcript:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("mapper 2.0.0:")),
        "mapper@2 must have loaded; transcript:\n{lines:#?}"
    );
    // Exactly one version won the version-independent singleton reservation; the other no-oped.
    assert_eq!(
        lines.iter().filter(|l| l.ends_with(": created")).count(),
        1,
        "exactly one version may register the singleton heal; transcript:\n{lines:#?}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.ends_with(": existed")).count(),
        1,
        "the other version's singleton heal must no-op; transcript:\n{lines:#?}"
    );
    // The single surviving `heal` fires exactly once.
    assert_eq!(
        lines.iter().filter(|l| *l == "healed").count(),
        1,
        "exactly one heal alias may be registered session-wide; transcript:\n{lines:#?}"
    );
}

/// The bulk `createTriggers` helper must forward `{ singleton: true }` too, so a
/// package that batch-registers its load-time automations with `singleton`
/// actually dedupes through that path, not just via the single `createTrigger`/`createAlias`. The same
/// package runs in two isolates (its sandboxed install + a copy a local module imported into main); each
/// calls `createTriggers({ dup: { …, singleton: true } })`. The forwarded flag drives the op exactly as
/// the single-create path does, so exactly ONE registers: the first copy's handle reports
/// `created === true`, the second `false`. (Triggers fire on *received* lines, not sent commands, so this
/// asserts the reservation via the returned handle's `created` rather than via firing.) The `sink` alias
/// in the main module just absorbs the harness's post-gate input so it never reaches the (absent) socket.
#[tokio::test]
async fn singleton_dedupes_via_create_triggers_across_isolates() {
    let pkg = TestPackage::new(
        "wbk",
        "batch",
        "1.0.0",
        r#"
        import { createTriggers, echo } from "smudgy:core";
        const t = createTriggers({
            dup: { patterns: ["^dup$"], script: () => { echo("fired_dup"); }, singleton: true },
        });
        echo(t.dup.created ? "created_true" : "created_false");
        echo("PKG_READY");
        "#,
    );
    let lines = run_scenario(
        9214,
        "pi_singleton_create_triggers",
        &[(
            "main_mod.ts",
            r#"
            import { createAlias } from "smudgy:core";
            import "smudgy://wbk/batch";
            createAlias("^sink$", () => {});
            "#,
        )],
        vec![pkg],
        "PKG_READY",
        2,
        "sink",
    )
    .await;

    assert_eq!(
        lines.iter().filter(|l| *l == "created_true").count(),
        1,
        "exactly one createTriggers copy must win the singleton reservation; transcript:\n{lines:#?}"
    );
    assert_eq!(
        lines.iter().filter(|l| *l == "created_false").count(),
        1,
        "the second createTriggers copy's singleton must report it already existed — the flag must be \
         forwarded through the bulk helper, not dropped; transcript:\n{lines:#?}"
    );
}
