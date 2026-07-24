//! End-to-end matching-order tests for alias/trigger priority and frame-local fallthrough.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const MATCHING_TS: &str = r#"
import {
    aliases,
    createAlias,
    createTrigger,
    echo,
    fallthrough,
    send,
} from "smudgy:core";

try {
    fallthrough(false);
    echo("CONTEXT_DID_NOT_THROW");
} catch {
    echo("CONTEXT_THROW");
}

createAlias("^priority$", () => echo("ALIAS_LOW"), { name: "priority-low", priority: -10 });
createAlias("^priority$", () => echo("ALIAS_HIGH"), {
    name: "priority-high",
    priority: 20,
    fallthrough: false,
});

createAlias("^override$", () => echo("OVERRIDE_LOW"), { name: "override-low" });
createAlias("^override$", () => {
    echo("OVERRIDE_HIGH");
    fallthrough(true);
}, { name: "override-high", priority: 20, fallthrough: false });

createAlias("^dynamic-stop$", () => echo("DYNAMIC_LOW"), { name: "dynamic-low" });
createAlias("^dynamic-stop$", () => {
    echo("DYNAMIC_HIGH");
    fallthrough(false);
}, { name: "dynamic-high", priority: 20 });

createAlias("^parent$", () => echo("PARENT_LOW"), { name: "parent-low" });
createAlias("^parent$", () => {
    echo("PARENT_HIGH");
    send("child");
}, { name: "parent-high", priority: 20 });
createAlias("^child$", () => echo("CHILD_LOW"), { name: "child-low" });
createAlias("^child$", () => echo("CHILD_HIGH"), {
    name: "child-high",
    priority: 20,
    fallthrough: false,
});

createAlias("^plain$", () => echo("PLAIN_LOW"), { name: "plain-low" });
createAlias("^plain$", "PLAIN_HIGH", {
    name: "plain-high",
    priority: 20,
    fallthrough: false,
});

const blocker = createAlias("^limited$", () => echo("LIMIT_BLOCK"), {
    name: "limit-blocker",
    priority: 20,
    fallthrough: false,
});
createAlias("^limited$", () => echo("LIMITED_FIRED"), {
    name: "limited-once",
    fireLimit: 1,
});
createAlias("^release$", () => {
    blocker.enabled = false;
    echo("RELEASED");
}, { name: "release" });

createAlias("^scope$", () => echo("SCOPE_A"), {
    name: "scope-a",
    priority: 20,
    fallthrough: false,
});

createTrigger("^trigger-priority$", () => echo("TRIGGER_LOW"), {
    name: "trigger-low",
});
createTrigger("^trigger-priority$", () => echo("TRIGGER_HIGH"), {
    name: "trigger-high",
    priority: 20,
    fallthrough: false,
});

createAlias("^from-trigger$", () => echo("BRIDGE_ALIAS_TWO"), { name: "bridge-alias-two" });
createAlias("^from-trigger$", () => echo("BRIDGE_ALIAS_ONE"), {
    name: "bridge-alias-one",
    priority: 20,
});
createTrigger("^bridge$", () => echo("BRIDGE_TRIGGER_LOW"), { name: "bridge-trigger-low" });
createTrigger("^bridge$", () => {
    echo("BRIDGE_TRIGGER_HIGH");
    send("from-trigger");
}, { name: "bridge-trigger-high", priority: 20, fallthrough: false });

createAlias("^inspect$", () => {
    const high = aliases.get("priority-high");
    echo(high?.priority === 20 && high?.fallthrough === false ? "HANDLE_OK" : "HANDLE_BAD");
}, { name: "inspect" });

echo("MATCHING_READY");
"#;

const PEER_TS: &str = r#"
import { createAlias, echo } from "smudgy:core";

createAlias("^scope$", () => echo("SCOPE_B"), { name: "scope-b" });
"#;

#[tokio::test]
async fn priority_and_fallthrough_are_frame_local() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "FallthroughIntegration";
    let modules = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules.join("matching.ts"), MATCHING_TS).unwrap();
    std::fs::write(modules.join("peer.ts"), PEER_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7120),
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
            .unwrap_or_else(|_| panic!("timed out waiting for MATCHING_READY; lines={lines:?}"))
            .expect("event stream ended before MATCHING_READY");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
        if lines.iter().any(|line| line == "MATCHING_READY") {
            break;
        }
    }

    let send = |text: &str| {
        tx.send(RuntimeAction::Send(Arc::new(text.to_string())))
            .unwrap();
    };
    let receive = |text: &str| {
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
            StyledLine::new(text, Vec::new()),
        )))
        .unwrap();
    };

    send("priority");
    send("override");
    send("dynamic-stop");
    send("parent");
    send("plain");
    send("limited");
    send("limited");
    send("release");
    send("limited");
    send("scope");
    send("inspect");
    receive("trigger-priority");
    receive("bridge");

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
    let position = |needle: &str| lines.iter().position(|line| line == needle).unwrap();

    assert!(
        has("CONTEXT_THROW"),
        "top-level fallthrough must throw\n{transcript}"
    );
    assert!(
        has("ALIAS_HIGH") && !has("ALIAS_LOW"),
        "priority stop failed\n{transcript}"
    );
    assert!(
        has("OVERRIDE_HIGH") && has("OVERRIDE_LOW"),
        "true override failed\n{transcript}"
    );
    assert!(
        has("DYNAMIC_HIGH") && !has("DYNAMIC_LOW"),
        "dynamic stop failed\n{transcript}"
    );
    assert!(
        has("PLAIN_HIGH") && !has("PLAIN_LOW"),
        "plaintext stop failed\n{transcript}"
    );
    assert_eq!(
        count("LIMIT_BLOCK"),
        2,
        "blocker should run twice\n{transcript}"
    );
    assert_eq!(
        count("LIMITED_FIRED"),
        1,
        "skips must not consume fireLimit\n{transcript}"
    );
    assert!(
        has("SCOPE_A") && has("SCOPE_B"),
        "one module stopped another\n{transcript}"
    );
    assert!(
        has("HANDLE_OK"),
        "handle fields did not round-trip\n{transcript}"
    );
    assert!(
        has("TRIGGER_HIGH") && !has("TRIGGER_LOW"),
        "trigger stop failed\n{transcript}"
    );
    assert!(
        has("BRIDGE_TRIGGER_HIGH")
            && !has("BRIDGE_TRIGGER_LOW")
            && has("BRIDGE_ALIAS_ONE")
            && has("BRIDGE_ALIAS_TWO"),
        "trigger/alias frames leaked\n{transcript}"
    );
    assert!(
        position("PARENT_HIGH") < position("CHILD_HIGH")
            && position("CHILD_HIGH") < position("PARENT_LOW")
            && !has("CHILD_LOW"),
        "nested alias frame did not remain depth-first and independent\n{transcript}"
    );
}
