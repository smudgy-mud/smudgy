//! End-to-end GMCP producer coverage (`docs/gmcp-plan.md` §10): runtime actions in —
//! exactly what the connection task's telnet bridge enqueues — and the script-facing
//! surface out: the `smudgy:state/gmcp` consumer handle (`.value`/`watch`/`onWrite`),
//! the `smudgy:events/gmcp` readiness events, the `gmcp` namespace on `smudgy:core`,
//! and the §3.3 ordering guarantee (a message is readable in the store by triggers on
//! any line that followed it on the wire).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// The consumer module: subscribes before GMCP "negotiates", so the enable + messages the
/// test injects afterward exercise the full pipeline.
const GMCP_TS: &str = r#"
import gmcp from "smudgy:state/gmcp";
import { ready, closed } from "smudgy:events/gmcp";
import { gmcp as gmcpCtl, createTrigger, echo } from "smudgy:core";

echo("ENABLED_AT_LOAD:" + gmcpCtl.enabled);
gmcpCtl.onReady(() => {
    echo("ONREADY:" + gmcpCtl.enabled);
    // GMCP is enabled here, so a nested onReady calls back before returning.
    let sync = false;
    gmcpCtl.onReady(() => { sync = true; });
    echo("ONREADY_SYNC:" + sync);
});
ready.on(() => echo("READY_EVENT"));
closed.on(() => echo("CLOSED_EVENT"));

// Coalesced cadence + a leaf read through the live view after the flush.
gmcp.watch("Char.Vitals", (v: any) => {
    echo("WATCH:" + JSON.stringify(v));
    echo("LEAF:" + gmcp.value?.Char?.Vitals?.hp);
});
// Sub-message granularity: watch a path INSIDE the payload -- a Char.Vitals message is an
// ancestor write into the scoped path, so this fires with the bare scalar at that path.
gmcp.watch("Char.Vitals.hp", (hp: any) => echo("HP_WATCH:" + typeof hp + ":" + hp));
// Per-write cadence with the handle-relative delivered path (case preserved as sent).
gmcp.onWrite("Comm", (path: string, v: any) => echo("ONWRITE:" + path + ":" + JSON.stringify(v)));

// The ordering guarantee: this trigger fires on the room-title line the test sends AFTER
// the Room.Info message, and must read the already-flushed room number.
createTrigger(/^The Fen of Sorrows$/, () => {
    echo("TRIGGER_ROOM:" + gmcp.value?.Room?.Info?.num);
});

// Outbound surface (docs/gmcp-plan.md 6.3), main isolate (allow-all): a send before
// negotiation drops with a one-time notice; mergeKeys extends the deep-merge set, observed
// through the Char.Defences deltas the test injects after enabling.
gmcpCtl.send("Char.Items.Inv");
gmcpCtl.mergeKeys("Char.Defences");
gmcpCtl.enableModule("IRE.Rift");
gmcp.watch("Char.Defences", (v: any) => echo("DEFENCES:" + JSON.stringify(v)));
"#;

async fn run_gmcp_session() -> Vec<String> {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let modules_dir = home.join("GmcpTest").join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home.join("GmcpTest").join("logs")).unwrap();
    std::fs::write(modules_dir.join("gmcp_test.ts"), GMCP_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9321_u32),
        server_name: Arc::new("GmcpTest".to_string()),
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

    // The sequence the telnet bridge would enqueue: negotiation on, a vitals message, a
    // chat occurrence (twice, value-identical — two occurrences), a Room.Info immediately
    // followed by the room's text line, and finally the option dropping.
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(RuntimeAction::GmcpMessage {
        name: Arc::from("Char.Vitals"),
        data: Some(Arc::from(r#"{ "hp": 4123, "maxhp": 6500 }"#)),
    })
    .unwrap();
    tx.send(RuntimeAction::GmcpMessage {
        name: Arc::from("Comm.Channel.Text"),
        data: Some(Arc::from(r#"{ "chan": "newbie", "msg": "hi" }"#)),
    })
    .unwrap();
    tx.send(RuntimeAction::GmcpMessage {
        name: Arc::from("Comm.Channel.Text"),
        data: Some(Arc::from(r#"{ "chan": "newbie", "msg": "hi" }"#)),
    })
    .unwrap();
    tx.send(RuntimeAction::GmcpMessage {
        name: Arc::from("Room.Info"),
        data: Some(Arc::from(r#"{ "num": 32519, "name": "The Fen of Sorrows" }"#)),
    })
    .unwrap();
    // Two Char.Defences deltas: the module's mergeKeys("Char.Defences") makes the second
    // merge into the first instead of replacing it.
    tx.send(RuntimeAction::GmcpMessage {
        name: Arc::from("Char.Defences"),
        data: Some(Arc::from(r#"{ "shield": true }"#)),
    })
    .unwrap();
    tx.send(RuntimeAction::GmcpMessage {
        name: Arc::from("Char.Defences"),
        data: Some(Arc::from(r#"{ "armor": 5 }"#)),
    })
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(Arc::new(StyledLine::new(
        "The Fen of Sorrows",
        Vec::new(),
    ))))
    .unwrap();
    tx.send(RuntimeAction::GmcpDisabled).unwrap();

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
async fn gmcp_pipeline_end_to_end() {
    let lines = run_gmcp_session().await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|l| l == "ENABLED_AT_LOAD:false"),
        "gmcp.enabled reads false before negotiation.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ONREADY:true"),
        "onReady fires after negotiation, with enabled already true.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ONREADY_SYNC:true"),
        "onReady calls the callback synchronously (before it returns) when GMCP \
         is already enabled.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "READY_EVENT"),
        "the gmcp ready event delivers.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("WATCH:") && l.contains("4123")),
        "a scoped watch on Char.Vitals delivers the parsed payload.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "LEAF:4123"),
        "a leaf read through gmcp.value sees the flushed message.\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "HP_WATCH:number:4123"),
        "a watch scoped BELOW the message name delivers the bare scalar at that path \
         (sub-message granularity, docs/gmcp-plan.md \u{a7}3.2).\n{transcript}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("ONWRITE:Comm.Channel.Text:"))
            .count(),
        2,
        "per-write delivery is loss-free for value-identical occurrences \
         (the parse memo must not suppress the write).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "TRIGGER_ROOM:32519"),
        "a trigger on the line following Room.Info reads the flushed room number \
         (the wire-order guarantee, docs/gmcp-plan.md \u{a7}3.3).\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CLOSED_EVENT"),
        "the gmcp closed event delivers when the option drops.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("GMCP: outbound message dropped")),
        "a gmcp.send before negotiation drops with the one-time notice.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("DEFENCES:") && l.contains("shield") && l.contains("armor")),
        "gmcp.mergeKeys makes the second Char.Defences delta merge, not replace \
         (docs/gmcp-plan.md \u{a7}4.3).\n{transcript}"
    );
}
