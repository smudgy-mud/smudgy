//! Tests for terminal pane scroll commands and script validation.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::pane::{MAIN_PANE_KEY, PaneScrollRequest};
use smudgy_core::session::ui_command::{PaneCommand, UiCommand};
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, spawn_with_ui_commands,
};

const SCROLL_TS: &str = r#"
import { echo, session } from "smudgy:core";

const log = session.mainPane.split("right", { name: "log" });
const hud = session.mainPane.split("right", { name: "hud", terminal: false });

session.mainPane.scrollTo("start");
session.mainPane.scrollTo("end");
session.mainPane.scrollTo(7);
log.scrollBy({ pages: -2 });
log.scrollBy({ lines: 3 });
session.mainPane.scrollBy({ pages: 0 });

let errors = 0;
for (const call of [
  () => session.mainPane.scrollTo(0),
  () => session.mainPane.scrollTo("middle" as any),
  () => session.mainPane.scrollBy({ pages: 1, lines: 1 } as any),
  () => session.mainPane.scrollBy({ lines: 1.5 }),
  () => hud.scrollTo("end"),
  () => hud.scrollBy({ lines: 0 }),
]) {
  try { call(); } catch { errors += 1; }
}
echo("SCROLL_ERRORS=" + errors);
"#;

#[tokio::test]
async fn pane_scroll_commands_keep_order_and_validate_arguments() {
    let home = tempfile::tempdir().expect("create temporary home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("get smudgy home");

    let server = "PaneScroll";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("scroll.ts"), SCROLL_TS).unwrap();

    let session_id = SessionId::from(7_240);
    let params = Arc::new(SessionParams {
        session_id,
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
        graphics_decoder: None,
    });

    let (bus, mut commands) = smudgy_core::session::ui_command::channel();
    let mut events = Box::pin(spawn_with_ui_commands(params, bus));
    let mut ready = false;
    let mut error_count = None;
    let mut output = Vec::new();
    while !ready || error_count.is_none() {
        let tagged = tokio::time::timeout(Duration::from_mins(2), events.next())
            .await
            .expect("wait for session event")
            .expect("session event stream ended");
        match tagged.event {
            SessionEvent::RuntimeReady(_) => ready = true,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update
                        && let Some(value) = line.text.strip_prefix("SCROLL_ERRORS=")
                    {
                        error_count = value.parse::<usize>().ok();
                    }
                    if let BufferUpdate::Append(line) = update {
                        output.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(error_count, Some(6), "session output: {output:#?}");

    let mut received = Vec::new();
    for _ in 0..7 {
        let envelope = tokio::time::timeout(Duration::from_secs(5), commands.next())
            .await
            .expect("wait for pane command")
            .expect("pane command stream ended");
        assert_eq!(envelope.origin, session_id);
        assert_eq!(
            envelope.origin_seq,
            u64::try_from(received.len()).expect("command count fits in u64")
        );
        let UiCommand::Pane(command) = envelope.command;
        received.push(command);
    }

    let log_key = match &received[0] {
        PaneCommand::Open { def, .. } if def.name.as_ref() == "log" => def.key,
        command => panic!("expected log pane open, got {command:?}"),
    };
    assert!(matches!(
        &received[1],
        PaneCommand::Open { def, .. } if def.name.as_ref() == "hud"
    ));
    assert!(matches!(
        &received[2],
        PaneCommand::Scroll {
            key: MAIN_PANE_KEY,
            request: PaneScrollRequest::Start,
            ..
        }
    ));
    assert!(matches!(
        &received[3],
        PaneCommand::Scroll {
            key: MAIN_PANE_KEY,
            request: PaneScrollRequest::End,
            ..
        }
    ));
    assert!(matches!(
        &received[4],
        PaneCommand::Scroll {
            key: MAIN_PANE_KEY,
            request: PaneScrollRequest::Line(7),
            ..
        }
    ));
    assert!(matches!(
        &received[5],
        PaneCommand::Scroll {
            key,
            request: PaneScrollRequest::Pages(-2),
            ..
        } if *key == log_key
    ));
    assert!(matches!(
        &received[6],
        PaneCommand::Scroll {
            key,
            request: PaneScrollRequest::Lines(3),
            ..
        } if *key == log_key
    ));
}
