//! The per-isolate call state around a handler call. A handler that throws must leave the
//! isolate in the same state as a handler that returns: the next dispatch starts from the
//! between-dispatch baseline, and `fallthrough()` from a later async continuation throws
//! because no handler is running.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const HARNESS_TS: &str = r#"
import { createAlias, createTrigger, echo, fallthrough, send } from "smudgy:core";

// Decides fallthrough, schedules a continuation that calls fallthrough() after the
// handler is gone, then throws.
createAlias("^boom$", () => {
    fallthrough(false);
    setTimeout(() => {
        try {
            fallthrough(true);
            echo("ASYNC_DID_NOT_THROW");
        } catch {
            echo("ASYNC_THROW");
        }
    }, 20);
    throw new Error("boom failed");
});
createAlias("^boom$", () => echo("BOOM_LOW_RAN"), { name: "boom-low", priority: -10 });

// Runs after the throwing handler; its own bracket must be intact.
createAlias("^after$", () => {
    fallthrough(true);
    send("nested");
    echo("AFTER_OK");
});
createAlias("^nested$", () => echo("NESTED_OK"));

// A returned string is sent as a command one level deeper.
createTrigger("^ping$", () => "pong");
createAlias("^pong$", () => echo("PONG_OK"));

echo("HARNESS_READY");
"#;

#[tokio::test]
async fn throwing_handler_leaves_the_call_state_restored() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "CallStateBracket";
    let modules = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules.join("harness.ts"), HARNESS_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7133),
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
            .unwrap_or_else(|_| panic!("timed out waiting for HARNESS_READY; lines={lines:?}"))
            .expect("event stream ended before HARNESS_READY");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
        if lines.iter().any(|line| line == "HARNESS_READY") {
            break;
        }
    }

    tx.send(RuntimeAction::Send(Arc::new("boom".to_string())))
        .unwrap();
    tx.send(RuntimeAction::Send(Arc::new("after".to_string())))
        .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
        StyledLine::new("ping", Vec::new()),
    )))
    .unwrap();

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
    assert!(
        lines.iter().any(|line| line.contains("boom failed")),
        "the exception must be echoed\n{transcript}"
    );
    assert!(
        !has("BOOM_LOW_RAN"),
        "fallthrough(false) decided before the throw must still stop the frame\n{transcript}"
    );
    assert!(
        has("AFTER_OK") && has("NESTED_OK"),
        "the next handler must run with its own bracket and nested send\n{transcript}"
    );
    assert!(
        has("ASYNC_THROW") && !has("ASYNC_DID_NOT_THROW"),
        "fallthrough() from a continuation must throw after the handler is gone\n{transcript}"
    );
    assert!(
        has("PONG_OK"),
        "a returned string must still be sent as a nested command\n{transcript}"
    );
}
