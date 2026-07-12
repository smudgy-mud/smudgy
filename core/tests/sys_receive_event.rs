//! End-to-end: the host `sys:receive` event (`smudgy:events/sys` `receive` handle). It fires once
//! per complete incoming line, *post-trigger but pre-display*, so a subscriber sees the original
//! text (trigger edits are deferred to the line's transform/route step) and can `gag()` the ambient
//! `line` before it ever reaches the screen — the same authority a trigger has. This locks in the
//! subtle ordering the dispatch arm arranges (trigger cascade → `sys:receive` handlers → `Complete`).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

// A trigger edits every line (`hello` → `HELLO`); a `sys:receive` handler echoes the payload text
// and gags any line mentioning `SECRET`. The trigger's edit is staged, not applied, when
// `sys:receive` runs — so the handler must observe the *original* `hello world`, while the displayed
// line still shows the trigger's `HELLO world` (proving `sys:receive` neither sees nor blocks
// trigger work). The `SECRET` line is gagged from a `sys:receive` handler and must never appear.
const SYS_RECEIVE_TS: &str = r#"
import { echo, line, createTrigger } from "smudgy:core";
import { receive } from "smudgy:events/sys";

createTrigger(/hello/, () => { line.replace("hello", "HELLO"); });

receive.on((payload) => {
    echo("GOT:" + payload.text);
    if (payload.text.includes("SECRET")) {
        line.gag();
    }
});
"#;

#[tokio::test]
async fn sys_receive_fires_post_trigger_sees_original_and_can_gag() {
    // Hermetic smudgy home (this test binary is its own process).
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);

    let server = "SysReceiveTest";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("sys_receive.ts"), SYS_RECEIVE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7013),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    let feed = |text: &str| {
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(StyledLine::new(
            text,
            Vec::new(),
        ))))
        .unwrap();
    };
    feed("hello world");
    feed("SECRET password");

    let mut lines = Vec::new();
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
    // Fires per line, and sees the ORIGINAL text even though a trigger staged an edit on it.
    assert!(
        lines.iter().any(|l| l == "GOT:hello world"),
        "sys:receive must fire with the original text (not the trigger-edited HELLO).\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "GOT:SECRET password"),
        "sys:receive must fire for every complete line.\nTranscript:\n{transcript}"
    );
    // The trigger's edit still lands on the displayed line — sys:receive neither saw nor blocked it.
    assert!(
        lines.iter().any(|l| l == "HELLO world"),
        "the trigger's edit must still reach the screen (sys:receive does not disturb triggers).\nTranscript:\n{transcript}"
    );
    // A gag from a sys:receive handler removes the line before it is shown.
    assert!(
        !lines.iter().any(|l| l == "SECRET password"),
        "a sys:receive handler's gag() must hide the line before display.\nTranscript:\n{transcript}"
    );
}
