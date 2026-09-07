//! The readiness seed after an engine reload. A reload rebuilds every isolate. A synchronous
//! handler on the rebuilt isolate that schedules a promise microtask must still get its
//! continuation run: the seed lives on the isolate bundle, so a rebuilt isolate must be seeded
//! by the dispatch that runs on it, not by any state of its predecessor.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const HARNESS_TS: &str = r#"
import { createAlias, echo } from "smudgy:core";

createAlias("^chain$", () => {
    Promise.resolve()
        .then(() => Promise.resolve(1))
        .then((n) => Promise.resolve(n + 1))
        .then((n) => { echo("CHAIN_FIRED depth=" + n); });
});

echo("MODULE_READY");
"#;

#[tokio::test]
async fn promise_chain_drains_on_the_rebuilt_isolate() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);

    let server = "DemuxSeedAfterReload";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("harness.ts"), HARNESS_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7003),
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

    // First MODULE_READY: fire the chain once on the original isolate. First CHAIN_FIRED:
    // reload. Second MODULE_READY (the rebuilt isolate): fire the chain again.
    let mut lines = Vec::new();
    let mut ready_count = 0;
    let mut fired_count = 0;
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                    if line.text == "MODULE_READY" {
                        ready_count += 1;
                        tx.send(RuntimeAction::Send(Arc::new("chain".to_string())))
                            .unwrap();
                    } else if line.text.starts_with("CHAIN_FIRED") {
                        fired_count += 1;
                        if fired_count == 1 {
                            tx.send(RuntimeAction::Reload).unwrap();
                        }
                    }
                }
            }
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert_eq!(
        ready_count, 2,
        "the module must evaluate once per engine generation\n{transcript}"
    );
    assert_eq!(
        fired_count, 2,
        "the microtask chain must drain on the original and on the rebuilt isolate\n{transcript}"
    );
}
