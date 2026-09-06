//! Capture objects have different lifetimes in function arguments and classic scripts.
//! Exercise both through real trigger dispatch, together with Rust interpolation.

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
import { createTrigger, echo, line } from "smudgy:core";
globalThis.captureFailures = [];
globalThis.retainedCaptures = [];
globalThis.scriptCaptures = [];
globalThis.captureChecks = 0;
globalThis.IntrinsicObject = Object;
globalThis.assertCapture = (ok, reason) => { if (!ok) captureFailures.push(reason); };
globalThis.inspectCapture = (m, prefix) => {
    assertCapture(Object.getPrototypeOf(m) === Object.prototype, "prototype");
    assertCapture(JSON.stringify(Object.keys(m)) === JSON.stringify(["0","1","2","3","4","__proto__","constructor","toString","length"]), "keys");
    assertCapture(m[0] === prefix + " éé a" && m[1] === "éé" && m.__proto__ === "éé" && m.constructor === "a", "values");
    assertCapture(m[3] === "" && m[4] === "" && !("5" in m) && !("other" in m), "empty versus absent");
    for (const key of Object.keys(m)) {
        const d = Object.getOwnPropertyDescriptor(m, key);
        assertCapture(d.enumerable && d.writable && d.configurable && !("get" in d), "descriptor:" + key);
    }
    assertCapture(!retainedCaptures.includes(m), "fresh object");
    retainedCaptures.push(m);
};
createTrigger({ patterns: [/^FN (?<__proto__>é+) (?<constructor>a)(?<toString>b)?(?<length>c?)$/, /^OTHER (?<other>x)$/] }, function(m) {
    assertCapture(this === undefined && arguments.length === 1, "function call shape");
    inspectCapture(m, "FN");
    if (scriptCaptures.length) assertCapture(globalThis.matches === scriptCaptures.at(-1), "function changed global");
    m[1] = "mutated";
    createTrigger("^NEVER_MATCH$", () => {}); // Registry borrows must be released before calling JS.
    return "FUNCTION_OUTPUT";
});
createTrigger("^MUTATE_OBJECT$", () => {
    globalThis.numericSetterCalls = 0;
    IntrinsicObject.defineProperty(IntrinsicObject.prototype, "1", {
        configurable: true,
        set() { globalThis.numericSetterCalls++; }
    });
    globalThis.Object = function ReplacedObject() { throw new Error("consulted mutable global Object"); };
});
globalThis.inspectSmallCapture = (m) => {
    delete IntrinsicObject.prototype[1];
    globalThis.Object = IntrinsicObject;
    assertCapture(numericSetterCalls === 0, "inherited numeric setter");
    assertCapture(Object.getPrototypeOf(m) === Object.prototype && m.__proto__ === "x" && m[1] === "x", "small object shape");
    const d = Object.getOwnPropertyDescriptor(m, "__proto__");
    assertCapture(d.writable && d.configurable && d.enumerable && !("get" in d), "small own property");
    captureChecks++;
};
createTrigger(/^SMALL_FN (?<__proto__>x)$/, m => inspectSmallCapture(m));
createTrigger(/^EDIT (?<word>old)$/, () => { assertCapture(line.replace("old", "new"), "line edit applied"); }, { name: "edit-first", priority: 20 });
createTrigger(/^EDIT (?<word>old)$/, m => {
    assertCapture(m.word === "old" && m[0] === "EDIT old", "capture survives line edit");
    captureChecks++;
}, { name: "edit-later" });
createTrigger(/^REPLACE x$/, () => {
    createTrigger(/^NO_REPLACE (?<new_name>x)$/, () => {
        assertCapture(false, "replacement handler ran for old match");
    }, { name: "replace-later" });
}, { name: "replace-first", priority: 20 });
createTrigger(/^REPLACE (?<old>x)$/, m => {
    assertCapture(m.old === "x" && !("new_name" in m), "capture survives replacement");
    captureChecks++;
}, { name: "replace-later" });
createTrigger(new RegExp("^WIDE " + Array.from({ length: 40 }, (_, i) => `(?<g${i}>x)`).join("") + "$"), m => {
    assertCapture(m[40] === "x" && m.g39 === "x" && !("41" in m), "large capture fallback");
    captureChecks++;
});
createTrigger("^SET_GLOBAL_SETTER$", () => {
    globalThis.matchesSetterCalls = 0;
    const previous = globalThis.matches;
    Object.defineProperty(globalThis, "matches", {
        configurable: true,
        get() { return previous; },
        set(value) { globalThis.matchesSetterCalls++; globalThis.setterCapture = value; }
    });
});
createTrigger("^VERIFY_DELIVERY$", () => {
    assertCapture(captureChecks === 5, "additional capture paths");
    assertCapture(retainedCaptures.length === 5, "call count");
    assertCapture(retainedCaptures.filter(m => m[1] === "mutated").length === 3, "retained mutations");
    assertCapture(scriptCaptures.length === 2 && scriptCaptures.every(m => m[1] === "éé"), "retained script values");
    assertCapture(globalThis.matches === scriptCaptures[1], "persistent global");
    assertCapture(matchesSetterCalls === 1 && setterCapture[0] === "SETTER", "ordinary global assignment");
    delete globalThis.matches;
    Promise.resolve().then(() => echo(captureFailures.length ? "CAPTURE_FAIL:" + captureFailures.join(",") : "CAPTURE_DONE"));
});
echo("CAPTURE_READY");
"#;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one session preserves ordering and retained objects across all deliveries"
)]
async fn functions_scripts_and_templates_preserve_capture_delivery() {
    let home = tempfile::tempdir().unwrap();
    smudgy_core::set_smudgy_home(home.path());
    let root = smudgy_core::get_smudgy_home().unwrap();
    let server = "CaptureDeliveryContract";
    std::fs::create_dir_all(root.join(server).join("logs")).unwrap();
    let modules = root.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::write(modules.join("delivery.ts"), MODULE).unwrap();
    let mut events = Box::pin(spawn(Arc::new(SessionParams {
        session_id: SessionId::from(7190),
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
                    sender
                        .send(RuntimeAction::HandleIncomingLine(Arc::new(
                            StyledLine::new("START_DELIVERY", Vec::new()),
                        )))
                        .unwrap();
                    tx = Some(sender);
                }
                SessionEvent::UpdateBuffer(updates) => {
                    for update in updates.iter() {
                        if let BufferUpdate::Append(line) = update {
                            transcript.push(line.text.clone());
                        }
                    }
                    if transcript.iter().any(|line| line == "CAPTURE_READY") {
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
    for (name, pattern, language, body) in [
        (
            "small script",
            r"^SMALL_SCRIPT (?<__proto__>x)$",
            ScriptLang::JS,
            "inspectSmallCapture(matches); void 0;",
        ),
        (
            "script",
            r"^SCRIPT (?<__proto__>é+) (?<constructor>a)(?<toString>b)?(?<length>c?)$",
            ScriptLang::JS,
            "inspectCapture(matches, 'SCRIPT'); scriptCaptures.push(matches); 'SCRIPT_OUTPUT';",
        ),
        (
            "setter",
            "^SETTER$",
            ScriptLang::JS,
            "assertCapture(matches === scriptCaptures[1], 'setter retains prior global'); void 0;",
        ),
        (
            "template",
            r"^TEXT (?<word>.*)$",
            ScriptLang::Plaintext,
            "OUT:$word;INDEX:$10:${1}:$$:${missing}",
        ),
    ] {
        tx.send(RuntimeAction::AddTrigger {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: Arc::new(name.to_string()),
            trigger: TriggerDefinition {
                patterns: Some(vec![pattern.to_string()]),
                script: Some(body.to_string()),
                language,
                ..TriggerDefinition::default()
            },
            fire_limit: None,
            line_limit: None,
        })
        .unwrap();
    }
    let wide = format!("WIDE {}", "x".repeat(40));
    for input in [
        "MUTATE_OBJECT",
        "SMALL_FN x",
        "MUTATE_OBJECT",
        "SMALL_SCRIPT x",
        "EDIT old",
        "REPLACE x",
        &wide,
        "FN éé a",
        "FN éé a",
        "SCRIPT éé a",
        "SCRIPT éé a",
        "FN éé a",
        "TEXT $1;TAIL",
        "SET_GLOBAL_SETTER",
        "SETTER",
        "VERIFY_DELIVERY",
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
                    .any(|line| line == "CAPTURE_DONE" || line.starts_with("CAPTURE_FAIL:"))
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
        transcript.iter().any(|line| line == "CAPTURE_DONE"),
        "{transcript:?}"
    );
    for (output, count) in [
        ("EDIT new", 1),
        ("FUNCTION_OUTPUT", 3),
        ("SCRIPT_OUTPUT", 2),
        ("OUT:$1", 1),
        ("TAIL", 1),
        ("INDEX:$1", 1),
        ("TAIL0:$1", 1),
        ("TAIL:$:", 1),
    ] {
        assert_eq!(
            transcript
                .iter()
                .filter(|line| line.as_str() == output)
                .count(),
            count,
            "output {output}: {transcript:?}"
        );
    }
    std::mem::forget(home); // The runtime may still be closing its log after Shutdown.
}
