//! End-to-end MSSP producer coverage: runtime actions in — exactly what the connection
//! task's telnet bridge enqueues — and the script-facing surface out: the read-only
//! `smudgy:state/mssp` consumer handle, the `smudgy:events/mssp` `updated` event (fired
//! after each merge, so handlers read the merged snapshot), last-write-wins merging
//! across payloads, raw spec names with spaces as bracket-quoted keys, and the
//! wire-order guarantee inherited from the GMCP path.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::mssp::MsspValue;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const MSSP_TS: &str = r#"
import mssp from "smudgy:state/mssp";
import { updated } from "smudgy:events/mssp";
import { createTrigger, echo } from "smudgy:core";

// Fired after each merge: the handler reads the already-flushed snapshot, spaced
// spec names included ("MINIMUM AGE" is one key, spelled bracket-quoted).
updated.on(() => {
    echo("UPDATED:" + mssp.value?.PLAYERS + ":" + (mssp.value as any)?.["MINIMUM AGE"]);
});

// An array variable (several MSSP_VALs on the wire) reads as a string array.
mssp.watch("PORT", (ports: any) => echo("PORT_WATCH:" + JSON.stringify(ports)));

// The ordering guarantee: this trigger fires on the welcome line the test sends AFTER
// the variables, and must read the already-flushed name.
createTrigger(/^Welcome to Arctic$/, () => {
    echo("TRIGGER_NAME:" + mssp.value?.NAME);
});
"#;

fn pairs(entries: &[(&str, MsspValue)]) -> Arc<Vec<(String, MsspValue)>> {
    Arc::new(
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect(),
    )
}

fn text(value: &str) -> MsspValue {
    MsspValue::Text(value.to_string())
}

async fn run_mssp_session() -> Vec<String> {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let modules_dir = home.join("MsspTest").join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home.join("MsspTest").join("logs")).unwrap();
    std::fs::write(modules_dir.join("mssp_test.ts"), MSSP_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9323_u32),
        server_name: Arc::new("MsspTest".to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
        graphics_decoder: None,
    });

    let mut events = Box::pin(spawn(params));
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

    // The sequence the telnet bridge would enqueue: the server's status block, the
    // welcome line, and a re-sent payload updating one variable (last write wins;
    // untouched variables survive).
    tx.send(RuntimeAction::MsspVariables {
        connection_generation: 0,
        pairs: pairs(&[
            ("NAME", text("ArcticMUD")),
            ("PLAYERS", text("52")),
            (
                "PORT",
                MsspValue::List(vec!["2700".to_string(), "6667".to_string()]),
            ),
            ("MINIMUM AGE", text("13")),
        ]),
    })
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
        StyledLine::new("Welcome to Arctic", Vec::new()),
    )))
    .unwrap();
    tx.send(RuntimeAction::MsspVariables {
        connection_generation: 0,
        pairs: pairs(&[("PLAYERS", text("53"))]),
    })
    .unwrap();

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

#[tokio::test]
async fn mssp_pipeline_end_to_end() {
    let lines = run_mssp_session().await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "UPDATED:52:13"),
        "the updated event fires after the first merge and the handler reads the \
         flushed snapshot, spaced names included.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == r#"PORT_WATCH:["2700","6667"]"#),
        "a multi-VAL variable reads as a string array.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "TRIGGER_NAME:ArcticMUD"),
        "a trigger on the line that followed the variables on the wire reads the \
         flushed value (the ordering guarantee).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "UPDATED:53:13"),
        "a re-sent payload merges last-write-wins: the updated count changes, the \
         untouched MINIMUM AGE survives.\n{transcript}"
    );
}
