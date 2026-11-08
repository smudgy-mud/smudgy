//! Regression coverage for producer-identified carriage-return replacement.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

fn line(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(text, Vec::new()))
}

#[tokio::test]
async fn carriage_return_transaction_survives_flush_and_intervening_echo() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("resolve test home");
    let server = "OpenLineReplacement";
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7113),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
        graphics_decoder: None,
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

    tx.send(RuntimeAction::HandleIncomingPartialLine(line("10%")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the open line")
            .expect("event stream ended before the open line");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates
                .iter()
                .any(|update| matches!(update, BufferUpdate::Append(line) if line.text == "10%"))
        {
            break;
        }
    }

    // Force the producer's begin marker into its own UI batch.
    tx.send(RuntimeAction::RetractIncomingPartialLine).unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for replacement begin")
            .expect("event stream ended before replacement begin");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates
                .iter()
                .any(|update| matches!(update, BufferUpdate::BeginOpenLineReplacement))
        {
            assert!(
                !updates
                    .iter()
                    .any(|update| matches!(update, BufferUpdate::RetractOpenLine))
            );
            break;
        }
    }

    tx.send(RuntimeAction::Echo(Arc::new("trigger output".to_string())))
        .unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(line("20%")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut saw_echo = false;
    let mut saw_finish = false;
    while !saw_finish {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for replacement finish")
            .expect("event stream ended before replacement finish");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                match update {
                    BufferUpdate::Append(line) if line.text == "trigger output" => saw_echo = true,
                    BufferUpdate::FinishOpenLineReplacement(Some(line)) if line.text == "20%" => {
                        assert!(
                            saw_echo,
                            "intervening output must precede replacement finish"
                        );
                        saw_finish = true;
                    }
                    _ => {}
                }
            }
        }
    }

    // Finishing that provisional replacement without a line transform must
    // keep the fragmented-line suffix-only delivery path: the runtime already
    // has "20%" on screen, so it appends only the unseen suffix and commits.
    tx.send(RuntimeAction::HandleIncomingFragmentedLine {
        line: line("20% done"),
        completion_fragment: line(" done"),
    })
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    let mut saw_suffix = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for fragmented completion")
            .expect("event stream ended before fragmented completion");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            assert!(
                !updates.iter().any(|update| matches!(
                    update,
                    BufferUpdate::BeginOpenLineReplacement
                        | BufferUpdate::FinishOpenLineReplacement(_)
                )),
                "an untransformed fragmented line must not replace its displayed prefix"
            );
            saw_suffix |= updates
                .iter()
                .any(|update| matches!(update, BufferUpdate::Append(line) if line.text == " done"));
            if saw_suffix
                && updates
                    .iter()
                    .any(|update| matches!(update, BufferUpdate::EnsureNewLine))
            {
                break;
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn abandoned_replacements_end_before_disconnect_reload_and_the_next_line() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("resolve test home");
    let server = "OpenLineReplacementAbort";
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    let modules = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::write(
        modules.join("reload-trigger.ts"),
        r#"
import session, { createTrigger } from "smudgy:core";
createTrigger("^reload-now$", () => session.reload());
"#,
    )
    .unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7114),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
        graphics_decoder: None,
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

    // A disconnect abandons the replacement frame. Its empty finish must be
    // delivered before the next connection's first line can be mistaken for
    // that replacement.
    tx.send(RuntimeAction::HandleIncomingPartialLine(line(
        "disconnect-old",
    )))
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for disconnect-old")
            .expect("event stream ended before disconnect-old");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "disconnect-old"),
            )
        {
            break;
        }
    }
    tx.send(RuntimeAction::RetractIncomingPartialLine).unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for disconnect replacement begin")
            .expect("event stream ended before disconnect replacement begin");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates
                .iter()
                .any(|update| matches!(update, BufferUpdate::BeginOpenLineReplacement))
        {
            break;
        }
    }

    tx.send(RuntimeAction::Disconnected {
        connection_generation: 0,
    })
    .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for disconnect replacement abort")
            .expect("event stream ended before disconnect replacement abort");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates
                .iter()
                .any(|update| matches!(update, BufferUpdate::FinishOpenLineReplacement(None)))
        {
            break;
        }
    }

    tx.send(RuntimeAction::HandleIncomingLine(line("after-disconnect")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the post-disconnect line")
            .expect("event stream ended before the post-disconnect line");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            assert!(
                !updates.iter().any(|update| matches!(
                    update,
                    BufferUpdate::FinishOpenLineReplacement(Some(line))
                        if line.text == "after-disconnect"
                )),
                "the next connection's line must not finish the abandoned replacement"
            );
            if updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "after-disconnect"),
            ) {
                break;
            }
        }
    }

    // A reload invoked inside a trigger cascade drops that cascade's local
    // completion frame. Verify the rebuild starts with no transaction to
    // inherit.
    tx.send(RuntimeAction::HandleIncomingPartialLine(line("reload-old")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for reload-old")
            .expect("event stream ended before reload-old");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "reload-old"),
            )
        {
            break;
        }
    }
    tx.send(RuntimeAction::RetractIncomingPartialLine).unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for reload replacement begin")
            .expect("event stream ended before reload replacement begin");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates
                .iter()
                .any(|update| matches!(update, BufferUpdate::BeginOpenLineReplacement))
        {
            break;
        }
    }

    // The trigger queues Reload ahead of CompleteLineTriggersProcessed in the
    // same depth-first frame. The reload therefore abandons this replacement
    // line's normal completion action.
    tx.send(RuntimeAction::HandleIncomingLine(line("reload-now")))
        .unwrap();
    let mut saw_abort = false;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the rebuilt runtime")
            .expect("event stream ended before the rebuilt runtime");
        match event.event {
            SessionEvent::UpdateBuffer(updates) => {
                saw_abort |= updates
                    .iter()
                    .any(|update| matches!(update, BufferUpdate::FinishOpenLineReplacement(None)));
            }
            SessionEvent::RuntimeReady(_) if saw_abort => break,
            _ => {}
        }
    }

    tx.send(RuntimeAction::HandleIncomingLine(line("after-reload")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the post-reload line")
            .expect("event stream ended before the post-reload line");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            assert!(
                !updates.iter().any(|update| matches!(
                    update,
                    BufferUpdate::FinishOpenLineReplacement(Some(line))
                        if line.text == "after-reload"
                )),
                "the rebuilt runtime's first line must not finish the abandoned replacement"
            );
            if updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "after-reload"),
            ) {
                break;
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();
}
