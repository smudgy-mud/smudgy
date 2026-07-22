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
use smudgy_core::session::runtime::RuntimeAction;
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
    prepare_server(server);
    shared_packages::install_package(server, spec, UpdateMode::Auto, true).unwrap();
    if let Some(consent) = consent {
        shared_packages::record_consent(server, spec, &consent).unwrap();
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
    tx.send(RuntimeAction::Shutdown).ok();
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

    assert!(has_line(&lines, "SEND_OK"), "the granted `send` must work; transcript:\n{lines:#?}");
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
        import session, { createAlias, createTrigger, send, sendRaw, echo, line } from "smudgy:core";
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
        !has_line(&denied, "GAG_OK") && has_line(&denied, "GAG_DENIED:") && has_line(&denied, "change-display"),
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
        import { echo } from "smudgy:core";
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
        !has_line(&denied, "MAPPER_OK") && has_line(&denied, "MAPPER_DENIED:") && has_line(&denied, "mapper-write"),
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
        has_line(&allowed, "MAPPER_OK"),
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
        !has_line(&denied, "TOGGLE_OK") && has_line(&denied, "TOGGLE_ERR:") && has_line(&denied, "aliases"),
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
