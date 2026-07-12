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

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};

/// Time the collector waits for the next buffer event before declaring the session idle.
const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// One `smudgy://owner/name` test package: its version and module sources (entry is `index.js`).
struct TestPackage {
    owner: &'static str,
    name: &'static str,
    version: &'static str,
    modules: Vec<(&'static str, String)>,
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
            enabled: true,
        }
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
                manifest: PackageManifest::parse(&format!(
                    "{{ \"name\": \"{}\", \"version\": \"{}\" }}",
                    pkg.name, pkg.version
                ))
                .expect("valid manifest"),
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
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn_with_package_provider(params, factory));
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
    tx.send(RuntimeAction::Shutdown).ok();

    assert!(sent, "gate sentinel {gate:?} (x{gate_count}) was never observed; transcript:\n{lines:#?}");
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
        lines.iter().any(|l| l.starts_with("[package] broken failed to load")),
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
        session_id: SessionId::from(session_id),
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
    tx.send(RuntimeAction::Shutdown).ok();

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
