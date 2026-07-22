//! End-to-end MSDP producer coverage (`docs/gmcp-mapping.md` §9 item 3): runtime
//! actions in — exactly what the connection task's telnet bridge enqueues — and the
//! script-facing surface out: the `smudgy:state/msdp` consumer handle, the
//! `smudgy:events/msdp` readiness events, and the wire-order guarantee inherited from the
//! GMCP path (a variable is readable in the store by triggers on any line that followed
//! it on the wire).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const MSDP_TS: &str = r#"
import msdp from "smudgy:state/msdp";
import { ready, closed } from "smudgy:events/msdp";
import { createTrigger, echo } from "smudgy:core";

ready.on(() => echo("READY_EVENT"));
closed.on(() => echo("CLOSED_EVENT"));

// The composite ROOM table, watched at and below the variable name -- MSDP values are
// wire-strings, so the vnum arrives as "14100", not 14100.
msdp.watch("ROOM", (room: any) => {
    echo("ROOM_WATCH:" + JSON.stringify(room?.EXITS));
    echo("LEAF:" + msdp.value?.ROOM?.VNUM);
});
msdp.watch("ROOM.VNUM", (vnum: any) => echo("VNUM_WATCH:" + typeof vnum + ":" + vnum));
// A flat KaVir-style variable lands beside the composite table.
msdp.watch("ROOM_VNUM", (vnum: any) => echo("FLAT_WATCH:" + vnum));

// The ordering guarantee: this trigger fires on the room-title line the test sends AFTER
// the ROOM variable, and must read the already-flushed vnum.
createTrigger(/^A Small Island Beach$/, () => {
    echo("TRIGGER_ROOM:" + msdp.value?.ROOM?.VNUM);
});
"#;

// MSDP structure markers.
const VAR: u8 = 1;
const VAL: u8 = 2;
const TABLE_OPEN: u8 = 3;
const TABLE_CLOSE: u8 = 4;

fn payload(parts: &[&[u8]]) -> Arc<[u8]> {
    Arc::from(parts.concat())
}

async fn run_msdp_session() -> Vec<String> {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let modules_dir = home.join("MsdpTest").join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home.join("MsdpTest").join("logs")).unwrap();
    std::fs::write(modules_dir.join("msdp_test.ts"), MSDP_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9322_u32),
        server_name: Arc::new("MsdpTest".to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
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

    // The sequence the telnet bridge would enqueue: negotiation on, the golden's
    // composite ROOM table (docs/gmcp-mapping.md §4.2) alongside a flat variable,
    // the room's text line, and the option dropping.
    tx.send(RuntimeAction::MsdpEnabled).unwrap();
    tx.send(RuntimeAction::MsdpMessage {
        payload: payload(&[
            &[VAR],
            b"ROOM",
            &[VAL, TABLE_OPEN, VAR],
            b"VNUM",
            &[VAL],
            b"14100",
            &[VAR],
            b"NAME",
            &[VAL],
            b"A Small Island Beach",
            &[VAR],
            b"EXITS",
            &[VAL, TABLE_OPEN, VAR],
            b"east",
            &[VAL],
            b"14101",
            &[TABLE_CLOSE, TABLE_CLOSE, VAR],
            b"ROOM_VNUM",
            &[VAL],
            b"14100",
        ]),
    })
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(Arc::new(StyledLine::new(
        "A Small Island Beach",
        Vec::new(),
    ))))
    .unwrap();
    tx.send(RuntimeAction::MsdpDisabled).unwrap();

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
async fn msdp_pipeline_end_to_end() {
    let lines = run_msdp_session().await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "READY_EVENT"),
        "the msdp ready event delivers.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("ROOM_WATCH:") && l.contains(r#""east":"14101""#)),
        "a scoped watch on ROOM delivers the decoded table with its exits.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "LEAF:14100"),
        "a leaf read through msdp.value sees the flushed variable.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "VNUM_WATCH:string:14100"),
        "a watch scoped below the variable name delivers the wire-string scalar.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "FLAT_WATCH:14100"),
        "flat KaVir-style variables land beside the composite table.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "TRIGGER_ROOM:14100"),
        "a trigger on the line that followed the ROOM variable on the wire reads the \
         flushed value (the ordering guarantee).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CLOSED_EVENT"),
        "the msdp closed event delivers when the option drops.\n{transcript}"
    );
}
