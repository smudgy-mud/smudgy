//! Captures a body cannot read are never extracted, and every body that can read them —
//! its own, or an inner trigger's view of its outer's — still gets exactly what it did.
//!
//! `matches_elision.rs` pins the V8 side of the same decision: that a handler which cannot
//! observe the object is not given one. This pins what the trigger engine does behind it,
//! where the decision now also skips the capture search and the payload. The elision is
//! invisible to correct JavaScript by construction, so what a test can assert is that
//! nothing observable moved — above all for the one shape that must NOT elide: a trigger
//! whose own body reads nothing but which has triggers inside it, whose payload becomes
//! their `outer`.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, runtime::RuntimeAction, spawn,
    styled_line::StyledLine,
};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const MODULE: &str = r#"
import { createTrigger, echo } from "smudgy:core";

// Elidable and top-level: nothing can read these captures, so none are built. What is
// pinned is that it is still an ordinary trigger — it fires, in order, and its returned
// string is still sent.
createTrigger(/^BARE (\w+)$/, () => "BARE_RAN");

// Elidable body, but with triggers inside it. Its captures become their `outer`, so this
// one must keep building them. Both inner shapes read the outer: a handler through its
// second argument, and a text body through `$outer`.
const outer = createTrigger(/^OUTER (?<who>\w+)$/, () => echo("OUTER_RAN"));
outer.createInnerTrigger(/^inner (?<what>\w+)$/, ({ what }, o) => {
    echo(`INNER ${o.who} ${what} ${o[1]}`);
}, { withinLines: 3, sameLine: false });
outer.createInnerTrigger(/^text (?<what>\w+)$/, "TEXTINNER $what of $outer.who", {
    withinLines: 3,
    sameLine: false,
});

// The neighbouring case: a body that does read its captures is untouched by any of this.
createTrigger(/^READS (\w+)$/, (m) => echo(`READS_GOT ${m[1]} ${m[0]}`));

echo("ELISION_READY");
"#;

/// Feed `lines`, then collect echoes until the session goes quiet.
async fn run(lines: &[&str]) -> Vec<String> {
    let home = tempfile::tempdir().unwrap();
    smudgy_core::set_smudgy_home(home.path());
    let root = smudgy_core::get_smudgy_home().unwrap();
    let server = "CaptureElisionContract";
    std::fs::create_dir_all(root.join(server).join("logs")).unwrap();
    let modules = root.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::write(modules.join("elision.ts"), MODULE).unwrap();

    let mut events = Box::pin(spawn(Arc::new(SessionParams {
        session_id: SessionId::from(7291),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })));

    let mut tx = None;
    let mut transcript: Vec<String> = Vec::new();
    tokio::time::timeout(Duration::from_mins(1), async {
        while let Some(event) = events.next().await {
            match event.event {
                SessionEvent::RuntimeReady(sender) => tx = Some(sender),
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
    transcript.clear();
    for text in lines {
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
            StyledLine::new(text, Vec::new()),
        )))
        .unwrap();
    }

    loop {
        match tokio::time::timeout(QUIET_PERIOD, events.next()).await {
            Ok(Some(event)) => {
                if let SessionEvent::UpdateBuffer(updates) = event.event {
                    for update in updates.iter() {
                        if let BufferUpdate::Append(line) = update {
                            transcript.push(line.text.clone());
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    transcript
}

#[tokio::test]
async fn captures_survive_exactly_where_something_can_read_them() {
    // One session for every shape: `set_smudgy_home` and the runtime-thread registry are
    // process-wide, so a second test in this binary would race this one.
    let transcript = run(&[
        "OUTER goblin",
        "inner bite",
        "text claw",
        "BARE alpha",
        "READS beta",
    ])
    .await;

    // The outer's own body reads nothing, so its captures exist for one reason only: the
    // triggers inside it. Both spellings of that read must survive.
    assert!(
        transcript.iter().any(|l| l == "INNER goblin bite goblin"),
        "inner handler lost its outer's captures: {transcript:?}"
    );
    assert!(
        transcript.iter().any(|l| l == "TEXTINNER claw of goblin"),
        "inner text body lost its outer's captures: {transcript:?}"
    );
    assert!(
        transcript.iter().any(|l| l == "OUTER_RAN"),
        "the outer's own body did not run: {transcript:?}"
    );

    // The elided trigger is still an ordinary trigger, and its reading neighbour on the
    // same line population keeps every capture.
    assert!(
        transcript.iter().any(|l| l.contains("BARE_RAN")),
        "an elided handler's returned string was not sent: {transcript:?}"
    );
    assert!(
        transcript.iter().any(|l| l == "READS_GOT beta READS beta"),
        "a reading handler lost its captures: {transcript:?}"
    );
}
