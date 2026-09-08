//! End-to-end behavior of inner triggers through the script API: creation, the `outer`
//! argument, watching, `skipInner()`, `stopWatching()`, `line.sequence`, and the
//! creation-time checks.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const INNER_TS: &str = r#"
import { createTrigger, echo, line, skipInner, stopWatching } from "smudgy:core";

// Same-line gate with the outer's values in the second argument.
const hit = createTrigger(/^You hit (?<who>\w+)/);
hit.createInnerTrigger(/for (?<dmg>\d+) damage/, ({ dmg }, outer) => {
    echo(`HIT ${outer.who} ${dmg} ${outer[1]}`);
});

// A watch with a range and once; the text body reads $outer.
const heading = createTrigger(/^Items:$/, () => echo("HEADING"));
heading.createInnerTrigger(/^ (?<item>.+)$/, ({ item }) => echo(`ITEM ${item} seq=${line.sequence}`), {
    withinLines: 3,
    sameLine: false,
});
heading.createInnerTrigger(/^ (?<first>.+)$/, "ITEMTEXT $first of $outer.0", {
    withinLines: 3,
    sameLine: false,
    once: true,
});

// skipInner from the outer's body.
const gate = createTrigger(/^gate (?<mode>\w+)$/, ({ mode }) => {
    if (mode === "closed") skipInner();
});
gate.createInnerTrigger(/^probe$/, () => echo("PROBE"), { withinLines: 2 });

// stopWatching from an inner body.
const blind = createTrigger(/^(?<who>\w+) is blinded!$/);
blind.createInnerTrigger(/^(?<name>\w+) can see again$/, ({ name }, { who }) => {
    if (name !== who) return;
    echo(`SEES ${who}`);
    stopWatching();
}, { withinLines: Infinity, overlap: "each" });
blind.createInnerTrigger(/^tick$/, () => echo("TICK"), { withinLines: Infinity });

// A nested chain merges named values outward.
const a = createTrigger(/^A (?<x>\d)$/);
const b = a.createInnerTrigger(/^B (?<y>\d)$/, { withinLines: 5, once: true });
b.createInnerTrigger(/^C (?<z>\d)$/, ({ z }, outer) => echo(`CHAIN ${outer.x}${outer.y}${z}`), { withinLines: 5, once: true });

// Batch form and handle introspection.
const batch = createTrigger(/^batch$/);
const made = batch.createInnerTriggers({
    one: { patterns: [/^one$/], script: () => echo("BATCH_ONE"), withinLines: 2 },
    two: { patterns: [/^two$/], script: "BATCH_TWO", withinLines: 2 },
});

// Creation-time checks.
try {
    hit.createInnerTrigger(/(?<who>x)/, () => {});
    echo("COLLISION_ALLOWED");
} catch (e) {
    echo(`COLLISION ${e.message}`);
}
try {
    hit.createInnerTrigger(/(?<outer>x)/, () => {});
    echo("RESERVED_ALLOWED");
} catch (e) {
    echo(`RESERVED ${e.message}`);
}
try {
    hit.createInnerTrigger(/x/, () => {}, { sameLine: false });
    echo("NEVER_ALLOWED");
} catch (e) {
    echo(`NEVER ${e.message}`);
}
try {
    createTrigger(/x/, () => {}, { withinLines: 3 });
    echo("TOPLEVEL_REACH_ALLOWED");
} catch (e) {
    echo(`TOPLEVEL_REACH ${e.message}`);
}
try {
    stopWatching();
    echo("STOP_ALLOWED");
} catch (e) {
    echo("STOP_THROWS");
}
createTrigger(/^plain$/, () => {
    try {
        stopWatching();
        echo("PLAIN_STOP_ALLOWED");
    } catch (e) {
        echo("PLAIN_STOP_THROWS");
    }
    skipInner();
    echo(`PLAIN seq=${line.sequence} last=${hit.lastFiredSequence}`);
    // Registration is queued behind the creating script, so handles read it back later.
    echo(`INTROSPECT ${batch.inner.join(",")} ${made.one.outer} ${made.two.name}`);
});

echo("INNER_READY");
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inner_triggers_work_end_to_end() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "InnerTriggers";
    let modules = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules.join("inner.ts"), INNER_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7130),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let mut lines = Vec::new();
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

    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for INNER_READY; lines={lines:?}"))
            .expect("event stream ended before INNER_READY");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
        if lines.iter().any(|line| line == "INNER_READY") {
            break;
        }
    }

    let receive = |text: &str| {
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
            StyledLine::new(text, Vec::new()),
        )))
        .unwrap();
    };
    let prompt = |text: &str| {
        tx.send(RuntimeAction::HandleIncomingPartialLine(Arc::new(
            StyledLine::new(text, Vec::new()),
        )))
        .unwrap();
    };

    // Same-line gate.
    receive("You hit orc for 12 damage");
    receive("I hit orc for 12 damage");
    // Watching.
    receive("Items:");
    receive(" a sword");
    receive(" a shield");
    receive("not an item");
    receive(" too late");
    // skipInner.
    receive("gate closed");
    receive("probe");
    receive("gate open");
    receive("probe");
    // stopWatching, per firing.
    receive("Frodo is blinded!");
    receive("Sam is blinded!");
    receive("tick");
    receive("Frodo can see again");
    receive("tick");
    receive("Sam can see again");
    receive("tick");
    // Nested chain.
    receive("A 1");
    receive("B 2");
    receive("C 3");
    // Batch.
    receive("batch");
    receive("one");
    receive("two");
    // The plain trigger's checks.
    receive("plain");
    prompt("> ");

    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    let has = |needle: &str| lines.iter().any(|line| line == needle);
    let count = |needle: &str| lines.iter().filter(|line| *line == needle).count();
    let starting = |prefix: &str| lines.iter().filter(|line| line.starts_with(prefix)).count();

    assert_eq!(
        count("HIT orc 12 orc"),
        1,
        "same-line gate with outer values\n{transcript}"
    );
    assert_eq!(count("HEADING"), 1, "{transcript}");
    assert!(
        has("ITEM a sword seq=4") && has("ITEM a shield seq=5"),
        "watching\n{transcript}"
    );
    assert_eq!(
        starting("ITEM "),
        2,
        "the range ran out before ' too late'\n{transcript}"
    );
    assert_eq!(
        count("ITEMTEXT a sword of Items:"),
        1,
        "text body with $outer\n{transcript}"
    );
    assert_eq!(starting("ITEMTEXT"), 1, "once\n{transcript}");
    assert_eq!(
        count("PROBE"),
        1,
        "skipInner closed the first gate\n{transcript}"
    );
    assert_eq!(count("SEES Frodo"), 1, "{transcript}");
    assert_eq!(count("SEES Sam"), 1, "{transcript}");
    assert_eq!(
        count("TICK"),
        2,
        "stopWatching ends the firing for every inner trigger; the third tick has nothing left\n{transcript}"
    );
    assert_eq!(
        count("CHAIN 123"),
        1,
        "nested chain merged x and y\n{transcript}"
    );
    assert!(has("BATCH_ONE") && has("BATCH_TWO"), "{transcript}");
    assert!(
        has("INTROSPECT one,two ^batch$ two"),
        "handles read the registry back
{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("COLLISION who is already captured")),
        "{transcript}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("RESERVED ")),
        "{transcript}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("NEVER ")),
        "{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("TOPLEVEL_REACH Unexpected option")),
        "{transcript}"
    );
    assert!(
        has("STOP_THROWS") && has("PLAIN_STOP_THROWS"),
        "{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("PLAIN seq=") && l.ends_with("last=1")),
        "line.sequence and lastFiredSequence\n{transcript}"
    );
}
