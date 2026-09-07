//! End-to-end: state exposures on saved automations through a real session runtime. A
//! trigger, an alias, and a hotkey expose GMCP `Char.Vitals` and a user `createState('foo')`
//! handle; injected GMCP messages and a module-side store write drive the values, and the
//! session's output shows what Send text expanded to and what the JavaScript bodies saw. A
//! second session loads its exposing triggers from disk and reloads its engine, so the
//! rebuilt engine has to bind them again.
//!
//! Turn separation comes from the action queue itself: every line, message, and write is a
//! dispatched action, and the store flushes before the next one dispatches, so a trigger on
//! a later line sees an earlier message's value with no timers involved.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{Stream, StreamExt};
use smudgy_core::models::ScriptLang;
use smudgy_core::models::aliases::AliasDefinition;
use smudgy_core::models::hotkeys::HotkeyDefinition;
use smudgy_core::models::state_exposure::StateExposure;
use smudgy_core::models::triggers::TriggerDefinition;
use smudgy_core::session::runtime::{IsolateId, Origin, RuntimeAction};
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{
    BufferUpdate, HotkeyId, SessionEvent, SessionId, SessionParams, TaggedSessionEvent, spawn,
};
use tokio::sync::mpsc::UnboundedSender;

const COMPLETION_TIMEOUT: Duration = Duration::from_mins(1);

type Events = Pin<Box<dyn Stream<Item = TaggedSessionEvent>>>;

/// The module owning the user handle `foo`, plus a trigger that rewrites it mid-test.
const MODULE_TS: &str = r#"
import { createState, createTrigger } from "smudgy:core";
const foo = createState<any>('foo');
foo.set({ bar: "baz", nested: { n: 1 } });
createTrigger(/^WRITE_FOO$/, () => { foo.set("bar", "qux"); });
"#;

/// The exposing JavaScript body: reads both names as identifiers, records the `gmcp`
/// snapshot object for the identity checks, and shows the state object shadows the `gmcp`
/// control object in this body only. `Char.Vitals` and `char.Status` are exposed together,
/// so the keys show both meet at one `Char` intermediate.
const JS_BODY: &str = r#"
const vitals = gmcp.Char.Vitals;
globalThis.__seen = globalThis.__seen ?? [];
globalThis.__seen.push(vitals);
echo("JS:" + vitals.hp + ":" + foo.bar + ":" + typeof gmcp.send + ":" + Object.isFrozen(vitals)
    + ":" + Object.isFrozen(gmcp) + ":" + Object.keys(gmcp).join() + ":" + Object.keys(gmcp.Char).join()
    + ":" + ("Room" in gmcp) + ":" + typeof globalThis.foo);
void 0;
"#;

fn gmcp(paths: &[&str]) -> StateExposure {
    StateExposure {
        producer: "gmcp".to_string(),
        handle: None,
        name_override: None,
        paths: paths.iter().map(ToString::to_string).collect(),
    }
}

fn gmcp_vitals() -> StateExposure {
    gmcp(&["Char.Vitals"])
}

fn user_foo(name_override: Option<&str>) -> StateExposure {
    StateExposure {
        producer: "user".to_string(),
        handle: Some("foo".to_string()),
        name_override: name_override.map(str::to_string),
        paths: vec!["bar".to_string()],
    }
}

fn trigger_definition(
    pattern: &str,
    language: ScriptLang,
    body: &str,
    state: Vec<StateExposure>,
) -> TriggerDefinition {
    TriggerDefinition {
        patterns: Some(vec![pattern.to_string()]),
        script: Some(body.to_string()),
        language,
        state,
        ..TriggerDefinition::default()
    }
}

fn trigger(
    name: &str,
    pattern: &str,
    language: ScriptLang,
    body: &str,
    state: Vec<StateExposure>,
) -> RuntimeAction {
    RuntimeAction::AddTrigger {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: Arc::new(name.to_string()),
        trigger: Box::new(trigger_definition(pattern, language, body, state)),
        fire_limit: None,
        line_limit: None,
    }
}

fn hotkey(name: &str, key: &str, language: ScriptLang, body: &str) -> RuntimeAction {
    RuntimeAction::AddHotkey {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: Arc::new(name.to_string()),
        hotkey: Box::new(HotkeyDefinition {
            key: key.to_string(),
            modifiers: Vec::new(),
            script: Some(body.to_string()),
            package: None,
            language,
            enabled: true,
            state: vec![gmcp_vitals(), user_foo(None)],
        }),
        function_id: None,
    }
}

fn line(text: &str) -> RuntimeAction {
    RuntimeAction::HandleIncomingLine(Arc::new(StyledLine::new(text, Vec::new())))
}

fn echo(text: &str) -> RuntimeAction {
    RuntimeAction::Echo(Arc::new(text.to_string()))
}

fn gmcp_message(name: &str, data: &str) -> RuntimeAction {
    RuntimeAction::GmcpMessage {
        name: Arc::from(name),
        data: Some(Arc::from(data)),
    }
}

fn apply_settings() -> RuntimeAction {
    RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: false,
        bold_is_bright: false,
        script_settings: Box::default(),
    }
}

/// The smudgy home every test in this binary shares: the override is process-wide, so each
/// test keeps to its own server directory beneath it.
fn test_home() -> PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("create temp home");
        let path = home.path().to_path_buf();
        std::mem::forget(home);
        smudgy_core::set_smudgy_home(&path);
        smudgy_core::get_smudgy_home().expect("smudgy home")
    })
    .clone()
}

/// Lay out `server`'s directory with the module that owns `foo`.
fn create_server(server: &str) {
    let server_dir = test_home().join(server);
    for subdir in ["modules", "logs", "triggers"] {
        std::fs::create_dir_all(server_dir.join(subdir)).unwrap();
    }
    std::fs::write(
        server_dir.join("modules").join("state_values.ts"),
        MODULE_TS,
    )
    .unwrap();
}

fn append_lines(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

/// Wait for the runtime's channel: a fresh session's, or the rebuilt engine's after a reload.
async fn wait_for_ready(
    events: &mut Events,
    lines: &mut Vec<String>,
) -> UnboundedSender<RuntimeAction> {
    loop {
        let event = tokio::time::timeout(COMPLETION_TIMEOUT, events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => append_lines(&updates, lines),
            _ => {}
        }
    }
}

/// Spawn a session on `server` and wait for its runtime channel.
async fn start(
    server: &str,
    session_id: u32,
    lines: &mut Vec<String>,
) -> (Events, UnboundedSender<RuntimeAction>) {
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events: Events = Box::pin(spawn(params));
    let tx = wait_for_ready(&mut events, lines).await;
    (events, tx)
}

/// Collect output lines and hotkey registrations until `done` says the transcript is
/// complete.
async fn collect_until(
    events: &mut Events,
    lines: &mut Vec<String>,
    hotkeys: &mut Vec<HotkeyId>,
    done: impl Fn(&[String], &[HotkeyId]) -> bool,
) {
    let deadline = tokio::time::Instant::now() + COMPLETION_TIMEOUT;
    while !done(lines, hotkeys) {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out; output so far: {lines:?}"))
            .unwrap_or_else(|| panic!("event stream ended; output so far: {lines:?}"));
        match event.event {
            SessionEvent::UpdateBuffer(updates) => append_lines(&updates, lines),
            SessionEvent::RegisterHotkey(id, _) => hotkeys.push(id),
            _ => {}
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn exposed_state_reaches_send_text_and_javascript_bodies() {
    let server = "StateValuesTest";
    create_server(server);
    let mut lines: Vec<String> = Vec::new();
    let mut hotkeys: Vec<HotkeyId> = Vec::new();
    let (mut events, tx) = start(server, 9501, &mut lines).await;

    tx.send(apply_settings()).unwrap();
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(gmcp_message(
        "Char.Vitals",
        r#"{ "hp": 4123, "maxhp": 6500 }"#,
    ))
    .unwrap();

    // Send text: bare and braced references, folded segments, a whole object, a renamed
    // handle.
    tx.send(trigger(
        "prompt",
        "^PROMPT$",
        ScriptLang::Plaintext,
        "say hp $gmcp.char.vitals.hp/${gmcp.Char.Vitals.maxhp} and $stats.bar and ${gmcp.Char.Vitals}",
        vec![gmcp_vitals(), user_foo(Some("stats"))],
    ))
    .unwrap();
    // JavaScript: both names in scope, frozen snapshots, `gmcp` shadowed inside this body,
    // and a second path whose intermediate differs only by case from the first's.
    let js_exposures = || vec![gmcp(&["Char.Vitals", "char.Status"]), user_foo(None)];
    tx.send(trigger(
        "js",
        "^JSFIRE$",
        ScriptLang::JS,
        JS_BODY,
        js_exposures(),
    ))
    .unwrap();
    // A body without exposures: the `gmcp` control object, no `foo` anywhere.
    tx.send(trigger(
        "plain",
        "^PLAIN$",
        ScriptLang::JS,
        r#"echo("PLAIN:" + typeof gmcp.send + ":" + typeof foo + ":" + typeof globalThis.foo); void 0;"#,
        Vec::new(),
    ))
    .unwrap();
    // Identity across fires: the same object until the value changes, then a new one.
    tx.send(trigger(
        "verify",
        "^VERIFY$",
        ScriptLang::JS,
        r#"const s = globalThis.__seen; echo("IDENTITY:" + (s[0] === s[1]) + ":" + (s[1] === s[2]) + ":" + s[2].hp + ":" + (s[0] === globalThis.__seen[0])); void 0;"#,
        Vec::new(),
    ))
    .unwrap();
    // Identity across a re-registration of the exposing trigger in the same engine: the
    // rebound cell is the same cell, so the unchanged leaf is the same object.
    tx.send(trigger(
        "verify-rebind",
        "^VERIFY_REBIND$",
        ScriptLang::JS,
        r#"const s = globalThis.__seen; echo("REBIND:" + (s[2] === s[3]) + ":" + s.length); void 0;"#,
        Vec::new(),
    ))
    .unwrap();
    // An invalid exposure is dropped with a warning; the valid one beside it still binds.
    tx.send(trigger(
        "warned",
        "^WARNED$",
        ScriptLang::Plaintext,
        "say warned $gmcp.Char.Vitals.hp",
        vec![
            StateExposure {
                producer: "nope".to_string(),
                handle: None,
                name_override: None,
                paths: vec![String::new()],
            },
            gmcp_vitals(),
        ],
    ))
    .unwrap();
    tx.send(RuntimeAction::AddAlias {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: Arc::new("hpalias".to_string()),
        alias: Box::new(AliasDefinition {
            pattern: "^hp$".to_string(),
            script: Some("say alias $gmcp.Char.Vitals.hp $foo.bar".to_string()),
            package: None,
            enabled: true,
            priority: 0,
            fallthrough: true,
            allow_self_match: false,
            language: ScriptLang::Plaintext,
            matcher: None,
            state: vec![gmcp_vitals(), user_foo(None)],
        }),
        fire_limit: None,
    })
    .unwrap();
    tx.send(hotkey(
        "hk-text",
        "F5",
        ScriptLang::Plaintext,
        "say hk $gmcp.Char.Vitals.maxhp;say hk2 $foo.bar",
    ))
    .unwrap();
    tx.send(hotkey(
        "hk-js",
        "F6",
        ScriptLang::JS,
        r#"echo("HK:" + gmcp.Char.Vitals.hp + ":" + foo.bar); void 0;"#,
    ))
    .unwrap();
    collect_until(&mut events, &mut lines, &mut hotkeys, |_, hotkeys| {
        hotkeys.len() == 2
    })
    .await;

    for action in [
        line("PROMPT"),
        line("JSFIRE"),
        line("JSFIRE"),
        gmcp_message("Char.Vitals", r#"{ "hp": 4000, "maxhp": 6500 }"#),
        line("JSFIRE"),
        line("VERIFY"),
        line("PLAIN"),
        line("WRITE_FOO"),
        // The same trigger registered again (a re-save) rebinds the same cells; only the
        // `foo` leaf changed since its last fire.
        trigger("js", "^JSFIRE$", ScriptLang::JS, JS_BODY, js_exposures()),
        line("JSFIRE"),
        line("VERIFY_REBIND"),
        line("WARNED"),
        RuntimeAction::Send(Arc::new("hp".to_string())),
        RuntimeAction::ExecHotkey { id: hotkeys[0] },
        RuntimeAction::ExecHotkey { id: hotkeys[1] },
        echo("DONE"),
    ] {
        tx.send(action).unwrap();
    }
    collect_until(&mut events, &mut lines, &mut hotkeys, |lines, _| {
        lines.iter().any(|line| line == "DONE")
    })
    .await;
    tx.send(RuntimeAction::Shutdown).ok();

    let has = |expected: &str| lines.iter().any(|line| line == expected);
    for expected in [
        r#"say hp 4123/6500 and baz and {"hp":4123,"maxhp":6500}"#,
        "JS:4123:baz:undefined:true:true:Char:Vitals,Status:false:undefined",
        "JS:4000:baz:undefined:true:true:Char:Vitals,Status:false:undefined",
        "IDENTITY:true:false:4000:true",
        "PLAIN:function:undefined:undefined",
        "JS:4000:qux:undefined:true:true:Char:Vitals,Status:false:undefined",
        "REBIND:true:4",
        "say warned 4000",
        "say alias 4000 qux",
        "say hk 6500",
        "say hk2 qux",
        "HK:4000:qux",
    ] {
        assert!(has(expected), "missing {expected:?} in {lines:?}");
    }
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str()
                == "JS:4123:baz:undefined:true:true:Char:Vitals,Status:false:undefined")
            .count(),
        2,
        "two fires before the change: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line
            .contains("trigger 'warned': state exposure 1 ignored: unknown producer \"nope\"")),
        "the invalid exposure was not reported: {lines:?}"
    );
}

/// Exposing triggers saved on disk bind when the session loads them and bind again when a
/// reload rebuilds the engine: the store's committed tree survives the reload, so the fresh
/// cells seed from it, and the module's re-run rewrites `foo`.
#[tokio::test]
async fn exposures_rebind_after_an_engine_reload() {
    let server = "StateValuesReloadTest";
    create_server(server);
    let exposures = || vec![gmcp_vitals(), user_foo(None)];
    let mut stored = HashMap::new();
    stored.insert(
        "disk-text".to_string(),
        trigger_definition(
            "^DISK$",
            ScriptLang::Plaintext,
            "say disk $gmcp.Char.Vitals.hp $foo.bar",
            exposures(),
        ),
    );
    stored.insert(
        "disk-js".to_string(),
        trigger_definition(
            "^DISKJS$",
            ScriptLang::JS,
            r#"echo("DISKJS:" + gmcp.Char.Vitals.hp + ":" + foo.bar); void 0;"#,
            exposures(),
        ),
    );
    smudgy_core::models::triggers::save_triggers(server, &stored).unwrap();

    let mut lines: Vec<String> = Vec::new();
    let mut hotkeys: Vec<HotkeyId> = Vec::new();
    let (mut events, tx) = start(server, 9502, &mut lines).await;
    tx.send(apply_settings()).unwrap();
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(gmcp_message("Char.Vitals", r#"{ "hp": 4123 }"#))
        .unwrap();
    // What the UI sends once the runtime is ready: load the saved automations.
    tx.send(RuntimeAction::SyncUserAutomations).unwrap();
    for action in [line("DISK"), line("DISKJS"), echo("FIRST")] {
        tx.send(action).unwrap();
    }
    collect_until(&mut events, &mut lines, &mut hotkeys, |lines, _| {
        lines.iter().any(|line| line == "FIRST")
    })
    .await;

    // A value that changes before the reload shows the rebound cells seed from the
    // committed tree rather than carrying the old engine's cells over.
    tx.send(gmcp_message("Char.Vitals", r#"{ "hp": 77 }"#))
        .unwrap();
    tx.send(RuntimeAction::Reload).unwrap();
    let tx = wait_for_ready(&mut events, &mut lines).await;
    tx.send(RuntimeAction::SyncUserAutomations).unwrap();
    for action in [line("DISK"), line("DISKJS"), echo("SECOND")] {
        tx.send(action).unwrap();
    }
    collect_until(&mut events, &mut lines, &mut hotkeys, |lines, _| {
        lines.iter().any(|line| line == "SECOND")
    })
    .await;
    tx.send(RuntimeAction::Shutdown).ok();

    let count = |expected: &str| {
        lines
            .iter()
            .filter(|line| line.as_str() == expected)
            .count()
    };
    for expected in [
        "say disk 4123 baz",
        "DISKJS:4123:baz",
        "say disk 77 baz",
        "DISKJS:77:baz",
    ] {
        assert_eq!(count(expected), 1, "expected one {expected:?} in {lines:?}");
    }
}
