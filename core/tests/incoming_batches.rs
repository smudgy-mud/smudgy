//! Transport batches must have the same observable ordering as individual lines,
//! including mutations, microtasks, inner triggers and reload in the middle.

use std::{path::Path, sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, runtime::RuntimeAction, spawn,
    styled_line::StyledLine,
};

const MODULE: &str = r#"
import { createAlias, createTrigger, echo, line, send, session } from "smudgy:core";
const doomed = createTrigger(/^gone$/, () => echo("TRACE gone"));
createAlias(/^leaf$/, () => echo("TRACE leaf"));
createAlias(/^nested$/, () => { echo("TRACE nested"); send("leaf"); });
createTrigger(/^arm$/, () => {
    echo("TRACE arm");
    send("nested");
    doomed.delete();
    createTrigger(/^expired$/, () => echo("TRACE expired"), { lineLimit: 2 });
    createTrigger(/^once$/, () => echo("TRACE once"), { fireLimit: 1 });
    queueMicrotask(() => {
        echo("TRACE microtask");
        createTrigger(/^late$/, () => echo(`TRACE late ${line.text}`));
    });
});
const heading = createTrigger(/^heading$/, () => echo("TRACE heading"));
heading.createInnerTrigger(/^item$/, () => echo("TRACE inner"), { withinLines: 2 });
createTrigger(/^reload$/, () => { echo("TRACE before-reload"); session.reload(); });
createTrigger(/^after-reload$/, () => echo("TRACE after-reload"));
createTrigger(/^finish$/, () => echo("TRACE finish"));
echo("TRACE module");
"#;

async fn run(home: &Path, batched: bool) -> Vec<String> {
    let server = if batched {
        "BatchedInput"
    } else {
        "SingleInput"
    };
    let modules = home.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::create_dir_all(home.join(server).join("logs")).unwrap();
    std::fs::write(modules.join("ordering.ts"), MODULE).unwrap();
    let mut events = Box::pin(spawn(Arc::new(SessionParams {
        session_id: SessionId::from(if batched { 7151 } else { 7150 }),
        server_name: Arc::new(server.to_owned()),
        profile_name: Arc::new("Test".to_owned()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })));
    let mut sender = None;
    let mut ready = false;
    while sender.is_none() || !ready {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("startup timed out")
            .expect("startup stream closed");
        match event.event {
            SessionEvent::RuntimeReady(tx) => sender = Some(tx),
            SessionEvent::UpdateBuffer(updates) => {
                ready |= updates.iter().any(|update| matches!(update, BufferUpdate::Append(line) if line.text == "TRACE module"));
            }
            _ => {}
        }
    }
    let sender = sender.unwrap();
    let lines = [
        "arm",
        "filler",
        "filler",
        "expired",
        "late",
        "gone",
        "once",
        "once",
        "heading",
        "filler",
        "item",
        "reload",
        "after-reload",
        "finish",
    ]
    .map(|text| Arc::new(StyledLine::from_output_str(text)));
    if batched {
        sender
            .send(RuntimeAction::HandleIncomingLines(lines.into()))
            .unwrap();
    } else {
        for line in lines {
            sender
                .send(RuntimeAction::HandleIncomingLine(line))
                .unwrap();
        }
    }
    let mut transcript = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .unwrap_or_else(|_| panic!("input timed out: {transcript:?}"))
            .expect("input stream closed");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update
                    && line.text.starts_with("TRACE ")
                {
                    transcript.push(line.text.clone());
                }
            }
        }
        if transcript.iter().any(|line| line == "TRACE finish") {
            break;
        }
    }
    sender.send(RuntimeAction::Shutdown).unwrap();
    transcript
}

#[tokio::test]
async fn batches_preserve_line_effects_and_reload_tail() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&path);
    let single = run(&path, false).await;
    let batched = run(&path, true).await;
    assert_eq!(
        single,
        [
            "TRACE arm",
            "TRACE nested",
            "TRACE leaf",
            "TRACE microtask",
            "TRACE late late",
            "TRACE once",
            "TRACE heading",
            "TRACE inner",
            "TRACE before-reload",
            "TRACE module",
            "TRACE after-reload",
            "TRACE finish"
        ]
    );
    assert_eq!(batched, single);
}
