//! A handler that cannot observe `matches` is not given one, and every handler that can
//! observe it — however indirectly — still receives exactly what it always did. The
//! elision is decided once at registration (`script_engine::matches`), so this drives real
//! trigger dispatch for each shape that decision has to get right.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_core::{
    models::{ScriptLang, triggers::TriggerDefinition},
    session::{
        BufferUpdate, SessionEvent, SessionId, SessionParams,
        runtime::{IsolateId, Origin, RuntimeAction},
        spawn,
        styled_line::{Style, StyledLine, VtSpan},
    },
};

const MODULE: &str = r#"
import { createTrigger, echo } from "smudgy:core";
globalThis.elisionFailures = [];
globalThis.fired = 0;
globalThis.check = (ok, reason) => { if (!ok) elisionFailures.push(reason); };

// Elided: declares nothing and reaches for nothing. The elision is invisible from JS by
// design, so what is pinned here is that an elided handler stays an ordinary handler — it
// fires, and its returned string is still sent. Naming `arguments` to observe the elision
// directly would suppress it.
createTrigger(/^BARE (\w+)$/, () => {
    fired++;
    return "BARE_OUTPUT";
});

// Kept: declares a parameter. The ordinary case, and the one the elision must never touch.
createTrigger(/^PARAM (\w+)$/, (m) => {
    check(m[0] === "PARAM alpha" && m[1] === "alpha", "declared parameter lost its captures");
    fired++;
});

// Kept: declares nothing but reads `arguments`. `length` is 0 here, so arity alone would
// wrongly elide this one.
createTrigger(/^ARGUMENTS (\w+)$/, function () {
    check(arguments.length === 1, "arguments handler was called with no argument");
    check(arguments[0][1] === "beta", "arguments handler lost its captures");
    fired++;
});

// Kept: a rest parameter also reports length 0.
createTrigger(/^REST (\w+)$/, (...rest) => {
    check(rest.length === 1 && rest[0][1] === "gamma", "rest handler lost its captures");
    fired++;
});

// Kept: a defaulted parameter reports length 0 and has no empty parameter list.
createTrigger(/^DEFAULTED (\w+)$/, (m = undefined) => {
    check(m !== undefined && m[1] === "delta", "defaulted handler lost its captures");
    fired++;
});

// Kept: a destructured parameter reports length 1, but leaves no `()` either.
createTrigger(/^DESTRUCTURED (\w+)$/, ({ 1: first }) => {
    check(first === "epsilon", "destructured handler lost its captures");
    fired++;
});

// Kept: direct `eval` can reach `arguments` on the handler's behalf.
createTrigger(/^EVAL (\w+)$/, function () {
    check(eval("arguments.length") === 1, "eval handler was called with no argument");
    fired++;
});

createTrigger("^VERIFY_ELISION$", () => {
    check(fired === 11, "fire count: " + fired);
    Promise.resolve().then(() => echo(
        elisionFailures.length ? "ELISION_FAIL:" + elisionFailures.join(",") : "ELISION_DONE"));
});
echo("ELISION_READY");
"#;

/// Inline bodies take the other delivery path: `matches` and `outer` are globals there, so
/// a body that never names either is run without them being set.
const BODIES: &[(&str, &str, ScriptLang, &str)] = &[
    // Elided: names neither global.
    (
        "silent script",
        r"^SILENT_SCRIPT (\w+)$",
        ScriptLang::JS,
        "fired++; void 0;",
    ),
    // Kept: names `matches` directly.
    (
        "reading script",
        r"^READING_SCRIPT (\w+)$",
        ScriptLang::JS,
        "check(matches[1] === 'zeta', 'reading script lost its captures'); fired++; void 0;",
    ),
    // Kept: reaches the global dynamically, which the source scan cannot follow and so
    // must refuse to elide. Neither body spells `matches`.
    (
        "dynamic script",
        r"^DYNAMIC_SCRIPT (\w+)$",
        ScriptLang::JS,
        "check(globalThis['mat' + 'ches'][1] === 'eta', 'dynamic script lost its captures'); fired++; void 0;",
    ),
    // Kept: a classic body's `this` is the global object, so it is the same route without
    // the `globalThis` spelling.
    (
        "this script",
        r"^THIS_SCRIPT (\w+)$",
        ScriptLang::JS,
        "check(this['mat' + 'ches'][1] === 'iota', 'this script lost its captures'); fired++; void 0;",
    ),
];

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one session keeps every delivery shape on the same isolate and ordering"
)]
async fn handlers_that_cannot_observe_matches_are_not_given_one() {
    let home = tempfile::tempdir().unwrap();
    smudgy_core::set_smudgy_home(home.path());
    let root = smudgy_core::get_smudgy_home().unwrap();
    let server = "MatchesElisionContract";
    std::fs::create_dir_all(root.join(server).join("logs")).unwrap();
    let modules = root.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::write(modules.join("elision.ts"), MODULE).unwrap();
    let mut events = Box::pin(spawn(Arc::new(SessionParams {
        session_id: SessionId::from(7191),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })));
    let mut tx = None;
    let mut transcript = Vec::new();
    tokio::time::timeout(Duration::from_mins(1), async {
        while let Some(event) = events.next().await {
            match event.event {
                SessionEvent::RuntimeReady(sender) => {
                    tx = Some(sender);
                }
                SessionEvent::UpdateBuffer(updates) => {
                    for update in updates.iter() {
                        if let BufferUpdate::Append(line) = update {
                            transcript.push(line.text.clone());
                        }
                    }
                    if transcript.iter().any(|line| line == "ELISION_READY") {
                        break;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("module startup timed out: {transcript:?}"));
    let tx = tx.expect("runtime ready");
    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: true,
        bold_is_bright: false,
        script_settings: Box::default(),
    })
    .unwrap();
    for (name, pattern, language, body) in BODIES {
        tx.send(RuntimeAction::AddTrigger {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: Arc::new((*name).to_string()),
            trigger: Box::new(TriggerDefinition {
                patterns: Some(vec![(*pattern).to_string()]),
                script: Some((*body).to_string()),
                language: *language,
                ..TriggerDefinition::default()
            }),
            fire_limit: None,
            line_limit: None,
        })
        .unwrap();
    }
    for input in [
        "BARE alpha",
        "PARAM alpha",
        "ARGUMENTS beta",
        "REST gamma",
        "DEFAULTED delta",
        "DESTRUCTURED epsilon",
        "EVAL whatever",
        "SILENT_SCRIPT theta",
        "READING_SCRIPT zeta",
        "DYNAMIC_SCRIPT eta",
        "THIS_SCRIPT iota",
        "VERIFY_ELISION",
    ] {
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
            StyledLine::new(
                input,
                vec![VtSpan {
                    style: Style::default(),
                    begin_pos: 0,
                    end_pos: input.len(),
                }],
            ),
        )))
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = events.next().await {
            if let SessionEvent::UpdateBuffer(updates) = event.event {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        transcript.push(line.text.clone());
                    }
                }
                if transcript
                    .iter()
                    .any(|line| line == "ELISION_DONE" || line.starts_with("ELISION_FAIL:"))
                {
                    break;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("delivery timed out: {transcript:?}"));
    tx.send(RuntimeAction::Shutdown).unwrap();
    assert!(
        transcript.iter().any(|line| line == "ELISION_DONE"),
        "{transcript:?}"
    );
    // An elided handler is still an ordinary handler: its returned string is sent.
    assert_eq!(
        transcript
            .iter()
            .filter(|line| line.as_str() == "BARE_OUTPUT")
            .count(),
        1,
        "{transcript:?}"
    );
    std::mem::forget(home); // The runtime may still be closing its log after Shutdown.
}
