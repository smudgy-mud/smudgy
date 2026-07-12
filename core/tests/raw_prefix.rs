//! Regression test for the raw-line prefix.
//!
//! The contract under test: a line starting with the configured raw prefix is
//! sent verbatim — the remainder bypasses BOTH command-separator splitting and
//! alias matching (it behaves exactly like `RuntimeAction::SendRaw`).
//!
//! The harness has no view of the socket (the session never connects), so
//! outgoing lines are observed through the echo the runtime appends to the
//! terminal buffer for every line it sends — the same approach as
//! `command_ordering.rs`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::{ScriptLang, aliases::AliasDefinition};
use smudgy_core::session::runtime::{IsolateId, Origin, RuntimeAction};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const EVENT_QUIET_PERIOD: Duration = Duration::from_millis(750);

#[tokio::test]
async fn raw_prefix_bypasses_splitting_and_alias_matching() {
    // The session log directory has to exist for the same reason documented
    // in command_ordering.rs.
    let server_name = "test_raw_prefix".to_string();
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().unwrap();
    std::fs::create_dir_all(home.join(&server_name).join("logs")).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9101u32),
        server_name: Arc::new(server_name),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // The runtime self-seeds separator/prefix from the machine's
    // settings.json; pin both so the test is environment-independent.
    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: true,
        script_settings: Box::new(smudgy_core::models::settings::ScriptSettings::default()),
    })
    .unwrap();

    // An alias whose pattern matches both the unsplit remainder
    // ("alias_name;foo", if only splitting were bypassed) and the first
    // would-be fragment ("alias_name", if splitting happened). If it fires
    // at all, "alias_fired" shows up in the buffer.
    tx.send(RuntimeAction::AddAlias {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: Arc::new("test_alias".to_string()),
        alias: AliasDefinition {
            pattern: "^alias_name".to_string(),
            script: Some("alias_fired".to_string()),
            package: None,
            enabled: true,
            language: ScriptLang::Plaintext,
        },
        fire_limit: None,
    })
    .unwrap();

    tx.send(RuntimeAction::Send(Arc::new("\\alias_name;foo".to_string())))
        .unwrap();

    // There is no "done" signal; collect until the session goes quiet.
    let tokens = ["alias_name;foo", "alias_fired", "alias_name", "foo"];
    let mut seen = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(EVENT_QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update
                    && tokens.contains(&line.text.as_str())
                {
                    seen.push(line.text.clone());
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    // Exactly one outgoing line, with the prefix stripped and the separator
    // intact — and the alias never fired.
    assert_eq!(seen, vec!["alias_name;foo"]);
}
