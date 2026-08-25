//! A sandboxed package isolate's **smudgy ops** are gated by the package's
//! consented op-capability set (`script/PACKAGE-ISOLATES-OP-CAPABILITIES.md`). A gated op a
//! package did not request throws `NotCapable` (naming the capability); the baseline
//! (`get_current_line`) stays ungated; cross-session reach is its own `reach-others` capability; and
//! `set_*_enabled` is own-origin-scoped. The deno-native net/fs/env gating is tested in
//! `package_isolates_enforcement.rs`; the pure `is_within`/`added_since` over the capability set is
//! unit-tested in `smudgy_script::package_resolver`.
//!
//! Each test installs ONE untrusted package (→ its own sandboxed isolate), records an explicit
//! consent (the smudgy capability subset under test), runs a script at the package's module top
//! level, and reports its outcome via `echo` — which is itself gated, so every reporting package is
//! consented `echo`. A package consented NOTHING cannot echo; it reports via the engine's
//! `[package] … failed to load — …` notice (an uncaught gated-op throw), the same out-of-band channel
//! `package_isolates_enforcement.rs`'s `None`-consent test uses.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::{RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread};
use smudgy_core::session::{
    BufferUpdate, PackageProviderFactory, SessionEvent, SessionId, SessionParams,
    spawn_with_package_provider,
};
use smudgy_script::{
    InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, PackagePermissions,
    PackageProvider, ResolvedPackage, SmudgyCapabilities,
};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Build a single-module (`index.js`) package whose manifest is `{ "name", "version" }` (no deno
/// permissions — these tests gate smudgy ops, not net/fs).
fn make_package(owner: &str, name: &str, version: &str, src: &str) -> ResolvedPackage {
    let manifest_json = format!(r#"{{ "name": "{name}", "version": "{version}" }}"#);
    ResolvedPackage {
        key: PackageKey {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        resolved_version: version.to_string(),
        manifest: PackageManifest::parse(&manifest_json).expect("valid manifest"),
        integrity: format!("test-{name}-{version}"),
        modules: vec![PackageModuleSource {
            subpath: "index.js".to_string(),
            text: src.to_string(),
        }],
    }
}

/// Like [`make_package`], but the manifest also DECLARES a `permissions.smudgy` block — for tests
/// whose interest includes the manifest wire shape (the recorded consent still drives enforcement).
fn make_package_declaring(
    owner: &str,
    name: &str,
    version: &str,
    smudgy_json: &str,
    src: &str,
) -> ResolvedPackage {
    let manifest_json = format!(
        r#"{{ "name": "{name}", "version": "{version}", "permissions": {{ "smudgy": {smudgy_json} }} }}"#
    );
    ResolvedPackage {
        key: PackageKey {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        resolved_version: version.to_string(),
        manifest: PackageManifest::parse(&manifest_json).expect("valid manifest"),
        integrity: format!("test-{name}-{version}"),
        modules: vec![PackageModuleSource {
            subpath: "index.js".to_string(),
            text: src.to_string(),
        }],
    }
}

fn factory_for(packages: Vec<ResolvedPackage>) -> PackageProviderFactory {
    Arc::new(move || {
        let mut provider = InMemoryPackageProvider::new();
        for pkg in &packages {
            provider.insert(pkg.clone());
        }
        let provider: Rc<dyn PackageProvider> = Rc::new(provider);
        provider
    })
}

/// First-setter-wins process-global smudgy home; create `<home>/<server>/{modules,logs}`.
fn prepare_server(server: &str) {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let server_dir = home.join(server);
    std::fs::create_dir_all(server_dir.join("modules")).unwrap();
    std::fs::create_dir_all(server_dir.join("logs")).unwrap();
}

/// A consent granting `echo` (the reporting channel) plus whatever `extra` adds. Tests pass a
/// closure that flips the capabilities under test.
fn consent_with(extra: impl FnOnce(&mut SmudgyCapabilities)) -> PackagePermissions {
    let mut smudgy = SmudgyCapabilities {
        echo: true,
        ..Default::default()
    };
    extra(&mut smudgy);
    PackagePermissions {
        smudgy,
        ..Default::default()
    }
}

/// Install `spec` untrusted (→ sandbox), record `consent` (`Some`) or none (`None` ⇒ deny-all), run
/// the package, and collect every appended buffer line (incl. engine notices) until the session is
/// quiet.
async fn run_capability_case(
    session_id: u32,
    server: &str,
    spec: &str,
    consent: Option<PackagePermissions>,
    pkg: ResolvedPackage,
) -> Vec<String> {
    let session_id = SessionId::from(session_id);
    prepare_server(server);
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    if let Some(consent) = consent {
        shared_packages::record_consent(server, spec, &consent).unwrap();
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

    let mut events = Box::pin(spawn_with_package_provider(params, factory_for(vec![pkg])));
    let mut lines: Vec<String> = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
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
    lines
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

fn has_line(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `send` granted, `send-direct` NOT: `send(..)` works (no throw); `sendRaw(..)` throws `NotCapable`
/// naming `send-direct`. The canonical example that gating is per-capability, not all-or-nothing
/// across the `session` group.
#[tokio::test]
async fn send_granted_but_send_direct_denied() {
    let src = r#"
        import { send, sendRaw, echo } from "smudgy:core";
        try { send("look"); echo("SEND_OK"); }
        catch (e) { echo("SEND_ERR:" + (e?.message ?? String(e))); }
        try { sendRaw("raw"); echo("SENDRAW_OK"); }
        catch (e) { echo("SENDRAW_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let lines = run_capability_case(
        9601,
        "pi_caps_send",
        "smudgy://wbk/sender",
        Some(consent_with(|s| s.send = true)),
        make_package("wbk", "sender", "1.0.0", src),
    )
    .await;

    assert!(
        has_line(&lines, "SEND_OK"),
        "the granted `send` must work; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "SENDRAW_OK") && has_line(&lines, "SENDRAW_DENIED:"),
        "the un-granted `sendRaw` must throw; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "send-direct"),
        "the denial must name the missing 'send-direct' capability; transcript:\n{lines:#?}"
    );
}

/// A package with no smudgy block is denied EVERY gated op, including `echo` itself.
///
/// Part A grants `echo` ONLY (the reporting channel) and confirms every *other* gated op throws:
/// `createAlias` / `createTrigger` (automations), `send` / `sendRaw` (session), `line.gag`
/// (display), `sessions` (reach-others), `mapper.setCurrentLocation` (mapper). Part B confirms `echo`
/// itself is gated by contrast: the same `echo("HELLO")` produces output when `echo` is granted and
/// produces NONE when nothing is granted (the denied echo throws before emitting).
#[tokio::test]
async fn no_smudgy_block_denies_every_gated_op() {
    // Part A — echo granted, everything else denied: each gated op is caught and reported.
    let probe_src = r#"
        import session, { createAlias, createTrigger, send, sendRaw, echo, line, mapper } from "smudgy:core";
        const probe = (name, fn) => {
            try { fn(); echo(name + ":NO_THROW"); }
            catch (e) { echo(name + ":DENIED:" + (e?.message ?? String(e))); }
        };
        probe("alias",   () => createAlias("^a$", "noop"));
        probe("trigger", () => createTrigger("^t$", "noop"));
        probe("send",    () => send("x"));
        probe("sendraw", () => sendRaw("x"));
        probe("gag",     () => line.gag());
        probe("reach",   () => { const _ = session.getSessions().length; });
        probe("mapper",  () => mapper.setCurrentLocation([0, 0], 1));
        echo("DONE");
    "#;
    let lines = run_capability_case(
        9602,
        "pi_caps_only_echo",
        "smudgy://wbk/probe",
        Some(consent_with(|_| {})), // echo only
        make_package("wbk", "probe", "1.0.0", probe_src),
    )
    .await;
    for (probe, cap) in [
        ("alias", "aliases"),
        ("trigger", "triggers"),
        ("send", "'send'"),
        ("sendraw", "send-direct"),
        ("gag", "change-display"),
        ("reach", "reach-others"),
        ("mapper", "mapper-write"),
    ] {
        assert!(
            !has_line(&lines, &format!("{probe}:NO_THROW")),
            "with no smudgy block, `{probe}` must throw; transcript:\n{lines:#?}"
        );
        assert!(
            has_line(&lines, &format!("{probe}:DENIED:")) && has_line(&lines, cap),
            "the `{probe}` denial must name the {cap} capability; transcript:\n{lines:#?}"
        );
    }

    // Part B — `echo` itself is gated, shown by contrast: the same source emits "HELLO" with `echo`
    // granted, and emits nothing with nothing granted (the denied echo throws before emitting).
    let echo_src = r#"import { echo } from "smudgy:core"; echo("HELLO");"#;
    let granted = run_capability_case(
        9603,
        "pi_caps_echo_yes",
        "smudgy://wbk/echoer",
        Some(consent_with(|_| {})), // echo only
        make_package("wbk", "echoer", "1.0.0", echo_src),
    )
    .await;
    assert!(
        has_line(&granted, "HELLO"),
        "with echo granted the package emits HELLO; transcript:\n{granted:#?}"
    );
    let denied = run_capability_case(
        9613,
        "pi_caps_echo_no",
        "smudgy://wbk/echoer",
        None, // nothing granted
        make_package("wbk", "echoer", "1.0.0", echo_src),
    )
    .await;
    assert!(
        !has_line(&denied, "HELLO"),
        "with nothing granted, echo is denied so HELLO never appears; transcript:\n{denied:#?}"
    );
}

/// Ungated baseline: reading the package's own execution context (`get_current_line`, here
/// `line.text`) needs no capability — it works for a package granted only `echo`, while a gated op
/// (`send`) on the same package throws. Proves the baseline is carved out of the gate, not granted.
#[tokio::test]
async fn get_current_line_is_ungated_baseline() {
    let src = r#"
        import { line, send, echo } from "smudgy:core";
        try { const t = line.text; echo("LINE_OK:[" + t + "]"); }
        catch (e) { echo("LINE_ERR:" + (e?.message ?? String(e))); }
        try { send("x"); echo("SEND_OK"); }
        catch (e) { echo("SEND_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let lines = run_capability_case(
        9604,
        "pi_caps_baseline",
        "smudgy://wbk/reader",
        Some(consent_with(|_| {})), // echo only
        make_package("wbk", "reader", "1.0.0", src),
    )
    .await;

    assert!(
        has_line(&lines, "LINE_OK:"),
        "reading the current line must work ungated; transcript:\n{lines:#?}"
    );
    assert!(
        has_line(&lines, "SEND_DENIED:") && has_line(&lines, "'send'"),
        "a gated op (send) on the same package must still throw; transcript:\n{lines:#?}"
    );
}

/// `reach-others` gates `get_sessions` (the `sessions` global): a package without it throws when it
/// enumerates sessions; a package with it succeeds. (Cross-session *routing* — `send`/`echo` to a
/// non-own session — rides the same `ensure_session_target` gate.)
#[tokio::test]
async fn reach_others_gates_get_sessions() {
    let src = r#"
        import session, { echo } from "smudgy:core";
        try { const n = session.getSessions().length; echo("SESSIONS_OK:" + n); }
        catch (e) { echo("SESSIONS_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    // Without reach-others: throws.
    let denied = run_capability_case(
        9605,
        "pi_caps_reach_deny",
        "smudgy://wbk/peeker",
        Some(consent_with(|_| {})),
        make_package("wbk", "peeker", "1.0.0", src),
    )
    .await;
    assert!(
        !has_line(&denied, "SESSIONS_OK:") && has_line(&denied, "SESSIONS_DENIED:"),
        "without reach-others, enumerating sessions must throw; transcript:\n{denied:#?}"
    );
    assert!(
        has_line(&denied, "reach-others"),
        "the denial must name the missing 'reach-others' capability; transcript:\n{denied:#?}"
    );

    // With reach-others: works (one session in this harness).
    let allowed = run_capability_case(
        9606,
        "pi_caps_reach_allow",
        "smudgy://wbk/peeker",
        Some(consent_with(|s| s.reach_others = true)),
        make_package("wbk", "peeker", "1.0.0", src),
    )
    .await;
    assert!(
        has_line(&allowed, "SESSIONS_OK:"),
        "with reach-others, enumerating sessions must work; transcript:\n{allowed:#?}"
    );
}

/// Every newly opened Session child surface re-checks `reach-others` at the
/// op boundary. Reflection constructs a foreign handle without using the
/// already-gated enumerator, proving input, pane lookup, and two-pane swap do
/// not rely on `getSessions()` as their security boundary.
#[tokio::test]
async fn reach_others_gates_foreign_input_panes_and_swap_directly() {
    let src = r#"
        import session, { echo } from "smudgy:core";
        const current = session.session;
        const SessionClass = Object.getPrototypeOf(current).constructor;
        const foreign = new SessionClass(current.id + 1);
        const probe = (name, fn) => {
            try { fn(); echo(name + ":NO_THROW"); }
            catch (e) { echo(name + ":ERR:" + (e?.message ?? String(e))); }
        };
        probe("input", () => foreign.input.focus());
        probe("panes", () => foreign.panes.list());
        probe("swap", () => current.mainPane.swap(foreign.mainPane));
        echo("DONE");
    "#;

    let denied = run_capability_case(
        9644,
        "pi_caps_foreign_children_deny",
        "smudgy://wbk/remote-controller",
        Some(consent_with(|s| {
            s.input = true;
            s.panes = true;
        })),
        make_package("wbk", "remote-controller", "1.0.0", src),
    )
    .await;
    for probe in ["input", "panes", "swap"] {
        assert!(
            !has_line(&denied, &format!("{probe}:NO_THROW"))
                && denied.iter().any(|line| {
                    line.contains(&format!("{probe}:ERR:")) && line.contains("reach-others")
                }),
            "foreign {probe} must be denied at its own op boundary; transcript:\n{denied:#?}"
        );
    }

    let allowed = run_capability_case(
        9645,
        "pi_caps_foreign_children_allow",
        "smudgy://wbk/remote-controller",
        Some(consent_with(|s| {
            s.input = true;
            s.panes = true;
            s.reach_others = true;
        })),
        make_package("wbk", "remote-controller", "1.0.0", src),
    )
    .await;
    assert!(
        !has_line(&allowed, "reach-others")
            && ["input", "panes", "swap"].iter().all(|probe| allowed
                .iter()
                .any(|line| { line.contains(&format!("{probe}:ERR:smudgy: no live session")) })),
        "with reach granted, each child surface must advance to live-target validation; transcript:\n{allowed:#?}"
    );
}

/// `get_session_character` is the ungated baseline for the OWN session, but reading ANOTHER
/// session's character is cross-session access gated on `reach-others` (closing the foreign-character
/// read a package could otherwise do by id). A package with only `echo` reads its own character but
/// is denied a foreign one (constructed here by reflecting the `Session` class).
#[tokio::test]
async fn get_session_character_gates_foreign_session() {
    let src = r#"
        import session, { echo } from "smudgy:core";
        const currentSession = session.session;
        try { const c = currentSession.profile; echo("OWN_CHAR_OK:" + (c?.name ?? "")); }
        catch (e) { echo("OWN_CHAR_ERR:" + (e?.message ?? String(e))); }
        // Build a FOREIGN session object (own id + 1) by reflecting the Session class — without
        // reach-others the gate must throw before any lookup.
        const SessionClass = Object.getPrototypeOf(currentSession).constructor;
        try {
            const c = new SessionClass(currentSession.id + 1).profile;
            echo("FOREIGN_CHAR_OK");
        } catch (e) { echo("FOREIGN_CHAR_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let lines = run_capability_case(
        9614,
        "pi_caps_char",
        "smudgy://wbk/charreader",
        Some(consent_with(|_| {})), // echo only — no reach-others
        make_package("wbk", "charreader", "1.0.0", src),
    )
    .await;
    assert!(
        has_line(&lines, "OWN_CHAR_OK:"),
        "reading the OWN session's character needs no capability (ungated baseline); transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "FOREIGN_CHAR_OK")
            && has_line(&lines, "FOREIGN_CHAR_DENIED:")
            && has_line(&lines, "reach-others"),
        "reading a FOREIGN session's character must be gated on reach-others; transcript:\n{lines:#?}"
    );
}

/// Every `layout.*` op requires BOTH `panes` and `reach-others`,
/// unconditionally — layout authority is workspace-wide, so the gate never
/// varies with the footprint, and the op acts only on the calling session's
/// own server (there is no cross-session form to gate differently). With
/// either capability missing the op throws naming the missing one; with
/// both, each op advances past the gate to its own validation (list
/// returns, save queues, apply fails on the unknown name — never silently).
#[tokio::test]
async fn layout_ops_require_panes_and_reach_others_unconditionally() {
    let src = r#"
        import session, { echo } from "smudgy:core";
        const probe = (name, fn) => {
            try { fn(); echo(name + ":NO_THROW"); }
            catch (e) { echo(name + ":ERR:" + (e?.message ?? String(e))); }
        };
        probe("list", () => session.layout.list());
        probe("save", () => session.layout.save("gate probe"));
        probe("apply", () => session.layout.apply("gate probe"));
        echo("DONE");
    "#;

    // `panes` alone is not enough: the second grant is unconditional even
    // though every layout op targets the caller's own server.
    let panes_only = run_capability_case(
        9660,
        "pi_caps_layout_panes_only",
        "smudgy://wbk/layouter",
        Some(consent_with(|s| s.panes = true)),
        make_package("wbk", "layouter", "1.0.0", src),
    )
    .await;
    for probe in ["list", "save", "apply"] {
        assert!(
            !has_line(&panes_only, &format!("{probe}:NO_THROW"))
                && panes_only.iter().any(|line| {
                    line.contains(&format!("{probe}:ERR:")) && line.contains("reach-others")
                }),
            "layout.{probe} with only panes must be denied naming reach-others; transcript:\n{panes_only:#?}"
        );
    }

    // `reach-others` alone is not enough either: `panes` is the op's own
    // capability and is checked first.
    let reach_only = run_capability_case(
        9661,
        "pi_caps_layout_reach_only",
        "smudgy://wbk/layouter",
        Some(consent_with(|s| s.reach_others = true)),
        make_package("wbk", "layouter", "1.0.0", src),
    )
    .await;
    for probe in ["list", "save", "apply"] {
        assert!(
            !has_line(&reach_only, &format!("{probe}:NO_THROW"))
                && reach_only.iter().any(|line| {
                    line.contains(&format!("{probe}:ERR:")) && line.contains("'panes'")
                }),
            "layout.{probe} with only reach-others must be denied naming panes; transcript:\n{reach_only:#?}"
        );
    }

    // Both grants: past the gate, each op reaches its own validation. The
    // store is empty in this harness, so list succeeds, save queues (its
    // capture runs on the UI daemon, absent here), and apply throws the
    // unknown-layout error rather than a capability one.
    let allowed = run_capability_case(
        9662,
        "pi_caps_layout_allow",
        "smudgy://wbk/layouter",
        Some(consent_with(|s| {
            s.panes = true;
            s.reach_others = true;
        })),
        make_package("wbk", "layouter", "1.0.0", src),
    )
    .await;
    assert!(
        has_line(&allowed, "list:NO_THROW") && has_line(&allowed, "save:NO_THROW"),
        "with both grants, list and save must pass the gate; transcript:\n{allowed:#?}"
    );
    assert!(
        allowed
            .iter()
            .any(|line| line.contains("apply:ERR:") && line.contains("no layout named")),
        "apply of an unknown layout must fail on the name, not the gate; transcript:\n{allowed:#?}"
    );
}

/// `display:change` gates the line-manipulation ops (`line.gag()` here): denied for an echo-only
/// package naming the capability. When `change_display` IS consented, the capability gate opens
/// and the same top-level call fails on the NEXT gate instead — the current-line window (module
/// top level runs with no line in flight, so a gag there could only leak onto a later line). The
/// two refusals are distinct and ordered: capability first, staleness second. In-window gags are
/// exercised by the trigger/`sys:receive` suites (`pane_routing`, `sys_receive_event`).
#[tokio::test]
async fn change_display_gates_line_manipulation() {
    let src = r#"
        import { line, echo } from "smudgy:core";
        try { line.gag(); echo("GAG_OK"); }
        catch (e) { echo("GAG_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let denied = run_capability_case(
        9607,
        "pi_caps_display_deny",
        "smudgy://wbk/gagger",
        Some(consent_with(|_| {})),
        make_package("wbk", "gagger", "1.0.0", src),
    )
    .await;
    assert!(
        !has_line(&denied, "GAG_OK")
            && has_line(&denied, "GAG_DENIED:")
            && has_line(&denied, "change-display"),
        "without change-display, gag must throw naming the capability; transcript:\n{denied:#?}"
    );

    let allowed = run_capability_case(
        9608,
        "pi_caps_display_allow",
        "smudgy://wbk/gagger",
        Some(consent_with(|s| s.change_display = true)),
        make_package("wbk", "gagger", "1.0.0", src),
    )
    .await;
    assert!(
        !has_line(&allowed, "GAG_OK")
            && has_line(&allowed, "GAG_DENIED:")
            && has_line(&allowed, "current line")
            && !has_line(&allowed, "change-display"),
        "with change-display consented the capability gate opens; the top-level call (no line \
         in flight) must fail on the current-line window instead; transcript:\n{allowed:#?}"
    );
}

/// `mapper:write` gates `mapper.setCurrentLocation` (a map mutation): denied for an echo-only
/// package, granted when `mapper_write` is consented. The gate runs before the op touches the
/// (absent, in this harness) `Mapper`, so the denial is a clean `NotCapable`, not "mapper not
/// enabled".
#[tokio::test]
async fn mapper_write_gates_set_current_location() {
    let src = r#"
        import { echo, mapper } from "smudgy:core";
        const mapperGlobalLeak =
            "mapper" in globalThis ||
            "Area" in globalThis ||
            "__smudgy_install_mapper" in globalThis;
        echo(mapperGlobalLeak ? "MAPPER_GLOBAL_LEAK" : "MAPPER_GLOBALS_GONE");
        try { mapper.setCurrentLocation([0, 0], 1); echo("MAPPER_OK"); }
        catch (e) { echo("MAPPER_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let denied = run_capability_case(
        9609,
        "pi_caps_mapper_deny",
        "smudgy://wbk/cartographer",
        Some(consent_with(|_| {})),
        make_package("wbk", "cartographer", "1.0.0", src),
    )
    .await;
    assert!(
        !has_line(&denied, "MAPPER_GLOBAL_LEAK")
            && has_line(&denied, "MAPPER_GLOBALS_GONE")
            && !has_line(&denied, "MAPPER_OK")
            && has_line(&denied, "MAPPER_DENIED:")
            && has_line(&denied, "mapper-write"),
        "without mapper-write, setCurrentLocation must throw naming the capability; transcript:\n{denied:#?}"
    );

    let allowed = run_capability_case(
        9610,
        "pi_caps_mapper_allow",
        "smudgy://wbk/cartographer",
        Some(consent_with(|s| s.mapper_write = true)),
        make_package("wbk", "cartographer", "1.0.0", src),
    )
    .await;
    assert!(
        !has_line(&allowed, "MAPPER_GLOBAL_LEAK")
            && has_line(&allowed, "MAPPER_GLOBALS_GONE")
            && has_line(&allowed, "MAPPER_OK"),
        "with mapper-write, setCurrentLocation must work; transcript:\n{allowed:#?}"
    );
}

/// `set_*_enabled` is gated on create-aliases AND own-origin-scoped: a package granted
/// `create_aliases` can create its own alias and toggle it (the toggle is keyed by
/// `(this isolate, this package's origin, name)`, so it can only ever reach the package's OWN
/// automations — never the user's or another package's, which live in different isolates). A package
/// WITHOUT the capability can't create the alias in the first place.
#[tokio::test]
async fn set_enabled_is_gated_and_own_origin_scoped() {
    let src = r#"
        import { createAlias, echo } from "smudgy:core";
        try {
            const a = createAlias("^mine$", "noop");
            a.enabled = false; // own-origin toggle of the package's own alias
            echo("TOGGLE_OK");
        } catch (e) { echo("TOGGLE_ERR:" + (e?.message ?? String(e))); }
        echo("DONE");
    "#;
    let granted = run_capability_case(
        9611,
        "pi_caps_toggle_ok",
        "smudgy://wbk/automator",
        Some(consent_with(|s| s.create_aliases = true)),
        make_package("wbk", "automator", "1.0.0", src),
    )
    .await;
    assert!(
        has_line(&granted, "TOGGLE_OK"),
        "with create-aliases a package can create + toggle its OWN alias; transcript:\n{granted:#?}"
    );

    // Without create-aliases the create itself throws (so there is nothing to toggle), echoed.
    let denied = run_capability_case(
        9612,
        "pi_caps_toggle_deny",
        "smudgy://wbk/automator",
        Some(consent_with(|_| {})),
        make_package("wbk", "automator", "1.0.0", src),
    )
    .await;
    assert!(
        !has_line(&denied, "TOGGLE_OK")
            && has_line(&denied, "TOGGLE_ERR:")
            && has_line(&denied, "aliases"),
        "without create-aliases the alias create must throw naming the capability; transcript:\n{denied:#?}"
    );
}

/// `panes` gates the pane surface: without it every pane op throws naming the
/// capability; with it a package creates/writes panes in its OWN namespace,
/// while `line.redirect` additionally requires `change-display` (it alters
/// what the main display shows — the same class as gag).
#[tokio::test]
async fn panes_capability_gates_pane_ops_and_routing() {
    // Part A — echo only: every pane op throws, naming 'panes'.
    let denied_src = r#"
        import { session, echo, line } from "smudgy:core";
        const probe = (name, fn) => {
            try { fn(); echo(name + ":NO_THROW"); }
            catch (e) { echo(name + ":DENIED:" + (e?.message ?? String(e))); }
        };
        probe("split",    () => session.mainPane.split("right", { name: "p" }));
        probe("plist",    () => session.panes.list());
        probe("redirect", () => line.redirect("p"));
        echo("DONE");
    "#;
    let denied = run_capability_case(
        9640,
        "pi_caps_panes_denied",
        "smudgy://wbk/paneless",
        Some(consent_with(|_| {})),
        make_package("wbk", "paneless", "1.0.0", denied_src),
    )
    .await;
    for probe in ["split", "plist", "redirect"] {
        assert!(
            !has_line(&denied, &format!("{probe}:NO_THROW"))
                && has_line(&denied, &format!("{probe}:DENIED:")),
            "without `panes` the `{probe}` op must throw; transcript:\n{denied:#?}"
        );
    }
    assert!(
        has_line(&denied, "panes"),
        "the denial must name the missing 'panes' capability; transcript:\n{denied:#?}"
    );

    // Part B — `panes` granted (no `display: ["change"]`): split + pane echo
    // work in the package's own namespace; redirect still throws, naming
    // change-display.
    let granted_src = r#"
        import { session, echo, line } from "smudgy:core";
        try {
            const p = session.mainPane.split("right", { name: "pkg-pane" });
            p.echo("into the pane");
            echo("SPLIT_OK created=" + p.created + " count=" + session.panes.list().length);
        } catch (e) { echo("SPLIT_ERR:" + (e?.message ?? String(e))); }
        try { line.redirect("pkg-pane"); echo("REDIR_NO_THROW"); }
        catch (e) { echo("REDIR_DENIED:" + (e?.message ?? String(e))); }
        echo("DONE2");
    "#;
    let granted = run_capability_case(
        9641,
        "pi_caps_panes_granted",
        "smudgy://wbk/paney",
        Some(consent_with(|s| s.panes = true)),
        make_package("wbk", "paney", "1.0.0", granted_src),
    )
    .await;
    assert!(
        has_line(&granted, "SPLIT_OK created=true count=2"),
        "with `panes` a package creates a pane in its own namespace (main + its pane); transcript:\n{granted:#?}"
    );
    assert!(
        !has_line(&granted, "REDIR_NO_THROW")
            && has_line(&granted, "REDIR_DENIED:")
            && has_line(&granted, "change-display"),
        "`line.redirect` must additionally require change-display; transcript:\n{granted:#?}"
    );
}

/// The outbound GMCP verbs (`gmcp.send` / `enableModule` / `disableModule` / `mergeKeys`)
/// are gated by their own `gmcp:send` capability — it rides with neither interop grant
/// (`docs/gmcp.md` §6.3) — while `gmcp.enabled` reads under `interop:read` like the
/// rest of the gmcp consumer surface.
#[tokio::test]
async fn gmcp_send_is_its_own_capability() {
    let probe_src = r#"
        import { gmcp, echo } from "smudgy:core";
        const probe = (name, fn) => {
            try { fn(); echo(name + ":NO_THROW"); }
            catch (e) { echo(name + ":DENIED:" + (e?.message ?? String(e))); }
        };
        probe("gsend",    () => gmcp.send("Char.Items.Inv"));
        probe("gmodule",  () => gmcp.enableModule("IRE.Rift"));
        probe("gunmodule",() => gmcp.disableModule("IRE.Rift"));
        probe("gmerge",   () => gmcp.mergeKeys("Char.Defences"));
        probe("genabled", () => { const _ = gmcp.enabled; });
        echo("DONE");
    "#;
    let denied = run_capability_case(
        9642,
        "pi_caps_gmcp_denied",
        "smudgy://wbk/gmcpprobe",
        // echo only — and notably interop_write, which must NOT satisfy gmcp:send.
        Some(consent_with(|s| s.interop_write = true)),
        make_package("wbk", "gmcpprobe", "1.0.0", probe_src),
    )
    .await;
    for probe in ["gsend", "gmodule", "gunmodule", "gmerge"] {
        assert!(
            !has_line(&denied, &format!("{probe}:NO_THROW"))
                && has_line(&denied, &format!("{probe}:DENIED:")),
            "`{probe}` must throw without gmcp:send (interop:write does not cover it); \
             transcript:\n{denied:#?}"
        );
    }
    assert!(
        has_line(&denied, "gmcp-send"),
        "the denial names the missing 'gmcp-send' capability; transcript:\n{denied:#?}"
    );
    assert!(
        !has_line(&denied, "genabled:NO_THROW")
            && has_line(&denied, "genabled:DENIED:")
            && has_line(&denied, "interop-read"),
        "`gmcp.enabled` reads under interop:read; transcript:\n{denied:#?}"
    );

    // Granted: the same calls pass the gate (their frames drop harmlessly with no live
    // connection — gating, not wire delivery, is under test here).
    let granted_src = r#"
        import { gmcp, echo } from "smudgy:core";
        try {
            gmcp.send("Char.Items.Inv");
            gmcp.enableModule("Room");
            gmcp.disableModule("Room");
            gmcp.mergeKeys("Char.Defences");
            echo("GMCP_OK");
        } catch (e) { echo("GMCP_ERR:" + (e?.message ?? String(e))); }
        try { gmcp.send(""); echo("BADNAME_NO_THROW"); }
        catch (e) { echo("BADNAME_DENIED:" + (e?.message ?? String(e))); }
    "#;
    let granted = run_capability_case(
        9643,
        "pi_caps_gmcp_granted",
        "smudgy://wbk/gmcpsender",
        Some(consent_with(|s| s.gmcp_send = true)),
        make_package("wbk", "gmcpsender", "1.0.0", granted_src),
    )
    .await;
    assert!(
        has_line(&granted, "GMCP_OK"),
        "with gmcp:send the outbound verbs pass the gate; transcript:\n{granted:#?}"
    );
    assert!(
        !has_line(&granted, "BADNAME_NO_THROW") && has_line(&granted, "BADNAME_DENIED:"),
        "an invalid GMCP name is rejected loudly at the op; transcript:\n{granted:#?}"
    );
}

/// The `workers` capability (`workers: ["spawn"]`, declared in the manifest's wire shape and
/// consented) gates Web Worker construction. Granted: a data:-URL module worker constructs,
/// round-trips one `postMessage`, and terminates. Ungated: `new Worker` is the facade's catchable
/// `TypeError`, naming the capability.
#[tokio::test]
async fn workers_capability_gates_worker_construction() {
    // A worker boot spawns a thread and initializes a fresh realm, which can outlast the
    // harness's quiet window — the keepalive echo holds the collection loop open until the
    // reply (or the 15s cap) lands.
    let granted_src = r#"
        import { echo } from "smudgy:core";
        const body = "onmessage = (e) => { postMessage('pong:' + e.data); };";
        try {
            const worker = new Worker(
                "data:text/javascript," + encodeURIComponent(body),
                { type: "module" },
            );
            const keepalive = setInterval(() => echo("WAITING"), 200);
            const done = () => clearInterval(keepalive);
            setTimeout(done, 15000);
            worker.onmessage = (e) => {
                done();
                echo("WORKER_REPLY:" + e.data);
                worker.terminate();
            };
            worker.onerror = (e) => {
                e.preventDefault();
                done();
                echo("WORKER_ONERROR:" + e.message);
            };
            worker.postMessage("ping");
            echo("WORKER_SPAWNED");
        } catch (e) { echo("WORKER_THROW:" + (e?.message ?? String(e))); }
    "#;
    let granted = run_capability_case(
        9646,
        "pi_caps_workers_granted",
        "smudgy://wbk/workerful",
        Some(consent_with(|s| s.workers = true)),
        make_package_declaring(
            "wbk",
            "workerful",
            "1.0.0",
            r#"{ "workers": ["spawn"], "session": ["echo"] }"#,
            granted_src,
        ),
    )
    .await;
    assert!(
        has_line(&granted, "WORKER_SPAWNED") && !has_line(&granted, "WORKER_THROW:"),
        "with workers:spawn the constructor must not throw; transcript:\n{granted:#?}"
    );
    assert!(
        has_line(&granted, "WORKER_REPLY:pong:ping"),
        "the worker must echo the sentinel back through postMessage; transcript:\n{granted:#?}"
    );

    // Ungated: the facade shadows the constructor with a TypeError naming the capability.
    let denied_src = r#"
        import { echo } from "smudgy:core";
        try {
            new Worker("data:text/javascript,", { type: "module" });
            echo("WORKER_NO_THROW");
        } catch (e) {
            echo("WORKER_DENIED:" + (e instanceof TypeError) + ":" + (e?.message ?? String(e)));
        }
    "#;
    let denied = run_capability_case(
        9647,
        "pi_caps_workers_denied",
        "smudgy://wbk/workless",
        Some(consent_with(|_| {})), // echo only
        make_package("wbk", "workless", "1.0.0", denied_src),
    )
    .await;
    assert!(
        !has_line(&denied, "WORKER_NO_THROW") && has_line(&denied, "WORKER_DENIED:true:"),
        "without workers:spawn the constructor throws a TypeError; transcript:\n{denied:#?}"
    );
    assert!(
        has_line(&denied, "workers:spawn"),
        "the denial names the missing 'workers:spawn' capability; transcript:\n{denied:#?}"
    );
}

/// Every constructor alias reaches `deno_runtime`'s native quota gate: the public
/// relative-specifier facade, its recoverable native superclass, and
/// `node:worker_threads`. The small-limit rejection itself is covered in
/// `smudgy_script` without allocating 129 OS threads/V8 isolates.
#[tokio::test]
async fn worker_constructor_aliases_share_the_native_host_path() {
    let src = r#"
        import { echo } from "smudgy:core";
        import { Worker as NodeWorker } from "node:worker_threads";
        const workers = [];
        try {
            workers.push(new Worker("data:text/javascript,", { type: "module" }));
            const NativeWorker = Object.getPrototypeOf(globalThis.Worker);
            workers.push(new NativeWorker("data:text/javascript,", { type: "module" }));
            workers.push(new NodeWorker("", { eval: true }));
            echo("WORKER_ALIASES_OK:" + workers.length);
        } catch (e) {
            echo("WORKER_ALIAS_UNEXPECTED:" + (e?.message ?? String(e)));
        } finally {
            for (const w of workers) w.terminate();
        }
    "#;
    let lines = run_capability_case(
        9648,
        "pi_caps_workers_cap",
        "smudgy://wbk/workcap",
        Some(consent_with(|s| s.workers = true)),
        make_package("wbk", "workcap", "1.0.0", src),
    )
    .await;
    assert!(
        has_line(&lines, "WORKER_ALIASES_OK:3"),
        "all worker aliases should reach the native host path; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "WORKER_ALIAS_UNEXPECTED:"),
        "no constructor alias should bypass host initialization; transcript:\n{lines:#?}"
    );
}

/// A relative Worker specifier resolves against the CALLING module, not the synthetic
/// `file:///smudgy-main.js` location `deno_runtime` would otherwise use: a local module's
/// `new Worker("./workers/worker.ts")` names its on-disk sibling. This is the trusted
/// main isolate (local modules), with no packages installed.
#[tokio::test]
async fn worker_relative_specifier_resolves_against_the_calling_module() {
    let session_id = SessionId::from(9649);
    let server = "pi_caps_workers_relative";
    prepare_server(server);
    let modules_dir = smudgy_core::get_smudgy_home()
        .expect("smudgy home")
        .join(server)
        .join("modules");
    std::fs::create_dir_all(modules_dir.join("workers")).expect("create workers dir");
    std::fs::write(
        modules_dir.join("workertest.ts"),
        r#"
            import { echo } from "smudgy:core";
            const worker = new Worker("./workers/worker.ts", { type: "module" });
            const keepalive = setInterval(() => echo("WAITING"), 200);
            const done = () => clearInterval(keepalive);
            setTimeout(done, 15000);
            worker.onmessage = (e) => {
                done();
                echo("WORKER_REL_REPLY:" + e.data);
                worker.terminate();
            };
            worker.onerror = (e) => {
                e.preventDefault();
                done();
                echo("WORKER_REL_ERROR:" + e.message);
            };
            worker.postMessage("ping");
        "#,
    )
    .expect("write workertest module");
    std::fs::write(
        modules_dir.join("workers").join("worker.ts"),
        "onmessage = (e: MessageEvent) => { postMessage('pong:' + e.data); };\n",
    )
    .expect("write worker module");

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

    let mut events = Box::pin(spawn_with_package_provider(params, factory_for(Vec::new())));
    let mut lines: Vec<String> = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
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
        has_line(&lines, "WORKER_REL_REPLY:pong:ping"),
        "a caller-relative worker module loads and echoes; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "WORKER_REL_ERROR:"),
        "no worker error on the relative path; transcript:\n{lines:#?}"
    );
}

/// A sandboxed package's relative worker URL resolves to `smudgy-pkg:` and loads from the
/// immutable source snapshot published after the parent graph load. The worker entry is not
/// imported by the parent graph, and it imports another package-relative TypeScript module, so
/// this covers the full fetched archive rather than only modules V8 already instantiated.
///
/// The request/reply shape mirrors an async layout planner: request IDs correlate multiple jobs
/// sent to one persistent worker, and model-like `Map`/`Set`/`bigint` values survive structured
/// clone in both directions without sharing mutations with the parent copy. Explicit termination
/// followed by the harness's clean runtime join covers the package worker teardown path.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn sandboxed_package_worker_preserves_layout_protocol_values_across_requests() {
    let package_src = r#"
        import { echo } from "smudgy:core";
        const worker = new Worker("./workers/worker.ts", { type: "module" });
        const keepalive = setInterval(() => echo("WAITING"), 200);
        const pending = new Map();
        let nextRequestId = 1;

        const done = () => {
            clearInterval(keepalive);
            clearTimeout(deadline);
        };
        const failPending = (error) => {
            for (const request of pending.values()) request.reject(error);
            pending.clear();
        };

        worker.onmessage = (e) => {
            const reply = e.data;
            const request = pending.get(reply.id);
            if (!request) return;
            pending.delete(reply.id);
            if (reply.ok) request.resolve(reply);
            else request.reject(new Error(reply.error));
        };
        worker.onerror = (e) => {
            e.preventDefault();
            failPending(new Error("worker error: " + e.message));
        };

        const deadline = setTimeout(
            () => failPending(new Error("worker timed out")),
            15000,
        );
        const plan = (model) => new Promise((resolve, reject) => {
            const id = nextRequestId++;
            pending.set(id, { resolve, reject });
            worker.postMessage({ id, kind: "plan", model });
        });

        const firstModel = {
            rooms: new Map([["alpha", { x: 1, y: 2 }]]),
            visited: new Set(["alpha"]),
            seed: 40n,
        };
        const secondModel = {
            rooms: new Map([
                ["beta", { x: 3, y: 4 }],
                ["gamma", { x: 5, y: 6 }],
            ]),
            visited: new Set(["beta", "gamma"]),
            seed: 100n,
        };

        try {
            const [first, second] = await Promise.all([
                plan(firstModel),
                plan(secondModel),
            ]);
            const cloneTypesSurvive =
                first.result.positions instanceof Map &&
                first.result.visited instanceof Set &&
                typeof first.result.score === "bigint" &&
                second.result.positions instanceof Map &&
                second.result.visited instanceof Set &&
                typeof second.result.score === "bigint";
            const valuesSurvive =
                first.result.positions.get("alpha").x === 1 &&
                first.result.positions.get("worker").x === 0 &&
                first.result.visited.has("worker") &&
                first.result.score === 41n &&
                second.result.positions.get("gamma").y === 6 &&
                second.result.positions.get("worker").x === 0 &&
                second.result.visited.has("worker") &&
                second.result.score === 102n;
            const copiesStayIsolated =
                firstModel.rooms.size === 1 &&
                !firstModel.rooms.has("worker") &&
                !firstModel.visited.has("worker") &&
                first.result.positions.get("alpha") !== firstModel.rooms.get("alpha");
            const oneWorkerHandledBoth = first.handled === 1 && second.handled === 2;
            echo(
                "PKG_LAYOUT_PROTOCOL_OK:" +
                [cloneTypesSurvive, valuesSurvive, copiesStayIsolated, oneWorkerHandledBoth]
                    .join(":"),
            );
        } catch (e) {
            echo("PKG_LAYOUT_PROTOCOL_ERROR:" + (e?.message ?? String(e)));
        } finally {
            done();
            worker.terminate();
            echo("PKG_LAYOUT_WORKER_TERMINATED");
        }
    "#;
    let mut package = make_package_declaring(
        "wbk",
        "workerarchive",
        "1.0.0",
        r#"{ "workers": ["spawn"], "session": ["echo"] }"#,
        package_src,
    );
    package.modules.extend([
        PackageModuleSource {
            subpath: "workers/worker.ts".to_string(),
            text: r#"
                import { plan } from "./planner.ts";

                let handled = 0;
                onmessage = (e: MessageEvent) => {
                    const { id, kind, model } = e.data;
                    try {
                        if (kind !== "plan") throw new Error(`unknown request: ${kind}`);
                        handled += 1;
                        postMessage({ id, ok: true, result: plan(model), handled });
                    } catch (error) {
                        postMessage({
                            id,
                            ok: false,
                            error: error instanceof Error ? error.message : String(error),
                        });
                    }
                };
            "#
            .to_string(),
        },
        PackageModuleSource {
            subpath: "workers/planner.ts".to_string(),
            text: r#"
                interface LayoutModel {
                    rooms: Map<string, { x: number; y: number }>;
                    visited: Set<string>;
                    seed: bigint;
                }

                export function plan(model: LayoutModel) {
                    if (!(model.rooms instanceof Map)) throw new Error("rooms lost Map type");
                    if (!(model.visited instanceof Set)) throw new Error("visited lost Set type");
                    if (typeof model.seed !== "bigint") throw new Error("seed lost bigint type");

                    const positions = new Map(model.rooms);
                    positions.set("worker", { x: Number(model.seed % 10n), y: model.rooms.size });
                    const visited = new Set(model.visited);
                    visited.add("worker");
                    return {
                        positions,
                        visited,
                        score: model.seed + BigInt(model.rooms.size),
                    };
                }
            "#
            .to_string(),
        },
    ]);

    let lines = run_capability_case(
        9650,
        "pi_caps_workers_package_relative",
        "smudgy://wbk/workerarchive",
        Some(consent_with(|s| s.workers = true)),
        package,
    )
    .await;

    assert!(
        has_line(&lines, "PKG_LAYOUT_PROTOCOL_OK:true:true:true:true"),
        "a sandboxed package worker should clone layout values and serve two correlated requests from one persistent realm; transcript:\n{lines:#?}"
    );
    assert!(
        !has_line(&lines, "PKG_LAYOUT_PROTOCOL_ERROR:")
            && has_line(&lines, "PKG_LAYOUT_WORKER_TERMINATED"),
        "the package worker should terminate explicitly without a protocol error; transcript:\n{lines:#?}"
    );
}
