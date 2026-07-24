//! The line-structured session log (`docs/panes.md` §2.5): the
//! transcript is the union of all sinks in line-completion order. Main
//! fragments accumulate and are written as whole lines on commit; a routed
//! (`AppendTo`) line is written whole; a retraction never leaves duplicated
//! prefix text (the prefix re-appears only inside the routed whole line);
//! fully-gagged lines stay unlogged.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::settings::{ScriptSettings, Settings};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const LOGGING_TS: &str = r#"
import { createTrigger, echo, line, session } from "smudgy:core";

const chat = session.mainPane.split("right", { name: "chat" });
createTrigger("^REDIR-REST$", () => { line.redirect(chat); });
createTrigger("^GAGME$", () => { line.gag(); });
echo("LOG_READY");
"#;

fn sl(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(text, Vec::new()))
}

fn apply_settings(log_enabled: bool) -> RuntimeAction {
    let settings = Settings::default();
    RuntimeAction::ApplySettings {
        command_separator: Arc::new(settings.command_separator.clone()),
        raw_line_prefix: Arc::new(settings.raw_line_prefix.clone()),
        log_enabled,
        script_settings: Box::new(ScriptSettings::from(&settings)),
    }
}

#[tokio::test]
async fn session_log_is_line_structured_under_redirects_and_gags() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "PaneLogging";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    let logs_dir = home_path.join(server).join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();
    std::fs::write(modules_dir.join("panes_log.ts"), LOGGING_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7101),
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
        let event = tokio::time::timeout(Duration::from_mins(2), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Turn logging on (a fresh timestamped file appears in logs/), then drive:
    // a partial prefix that gets redirected on completion (retraction), a
    // gagged line, and a plain line.
    tx.send(apply_settings(true)).unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(sl("PROMPT>")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(sl("REDIR-REST")))
        .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(sl("GAGME"))).unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(sl("AFTER"))).unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    // Wait until the last line has flowed through the pipeline (observed on
    // the event stream), then toggle logging off — which drains the open-line
    // accumulator and flushes the BufWriter.
    let mut saw_after = false;
    while !saw_after {
        let Ok(Some(event)) = tokio::time::timeout(Duration::from_mins(1), events.next()).await
        else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update
                    && line.text == "AFTER"
                {
                    saw_after = true;
                }
            }
        }
    }
    assert!(saw_after, "the AFTER line never reached the buffer stream");

    tx.send(apply_settings(false)).unwrap();

    // Give the runtime a moment to process the toggle, then read the log.
    let mut log_text = String::new();
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Some(entry) = std::fs::read_dir(&logs_dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().is_some_and(|ext| ext == "log"))
        else {
            continue;
        };
        log_text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        if log_text.contains("AFTER") {
            break;
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let lines: Vec<&str> = log_text.lines().collect();
    // The routed line is one whole line: the retracted partial prefix appears
    // only inside it, never as stranded text or a duplicate.
    assert!(
        lines.contains(&"PROMPT>REDIR-REST"),
        "the redirected line must be logged whole (union of sinks).\nLog:\n{log_text}"
    );
    assert_eq!(
        log_text.matches("PROMPT>").count(),
        1,
        "the retracted prefix must not be duplicated in the transcript.\nLog:\n{log_text}"
    );
    assert!(
        lines.contains(&"AFTER"),
        "a plain line is logged as its own line.\nLog:\n{log_text}"
    );
    assert!(
        !log_text.contains("GAGME"),
        "a fully-gagged line stays unlogged.\nLog:\n{log_text}"
    );
}

/// An open line (a resting prompt) must reach disk *provisionally* before it
/// completes, so an abnormal kill — force-close, WM_ENDSESSION exit, V8 abort,
/// none of which run the teardown flush — doesn't lose it. On completion the
/// provisional bytes are rewound and the whole line rewritten exactly once.
#[tokio::test]
async fn session_log_persists_open_line_before_completion() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "PaneLoggingDurable";
    std::fs::create_dir_all(home_path.join(server).join("modules")).unwrap();
    let logs_dir = home_path.join(server).join("logs");
    std::fs::create_dir_all(&logs_dir).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7102),
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
        let event = tokio::time::timeout(Duration::from_mins(2), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Wait until an Append with `needle` has flowed through the event stream.
    async fn drain_until<S>(events: &mut S, needle: &str)
    where
        S: futures::Stream<Item = smudgy_core::session::TaggedSessionEvent> + Unpin,
    {
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_mins(1), events.next()).await
        {
            if let SessionEvent::UpdateBuffer(updates) = event.event
                && updates
                    .iter()
                    .any(|u| matches!(u, BufferUpdate::Append(l) if l.text == needle))
            {
                return;
            }
        }
    }

    let read_log = || {
        let entry = std::fs::read_dir(&logs_dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().is_some_and(|ext| ext == "log"))?;
        std::fs::read_to_string(entry.path()).ok()
    };

    tx.send(apply_settings(true)).unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(sl("PROMPT>")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    drain_until(&mut events, "PROMPT>").await;

    // Let the flush interval elapse, then nudge the open line with another
    // fragment: that flush persists the open line provisionally.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    tx.send(RuntimeAction::HandleIncomingPartialLine(sl("MORE")))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    drain_until(&mut events, "MORE").await;

    // Read the log WITHOUT any clean teardown: the open line must already be on
    // disk (this is the abnormal-kill durability the fix restores).
    let mut on_disk = String::new();
    for _ in 0..30 {
        if let Some(text) = read_log()
            && text.contains("PROMPT>MORE")
        {
            on_disk = text;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        on_disk.contains("PROMPT>MORE"),
        "an open line must be persisted provisionally before completion.\nLog:\n{on_disk}"
    );

    // Complete the line: the provisional bytes are rewound and the whole line
    // written once — no duplicated prefix.
    tx.send(RuntimeAction::HandleIncomingLine(sl("DONE"))).unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    drain_until(&mut events, "DONE").await;
    tx.send(apply_settings(false)).unwrap();

    let mut final_text = String::new();
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(text) = read_log()
            && text.lines().any(|l| l == "PROMPT>MOREDONE")
        {
            final_text = text;
            break;
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    assert!(
        final_text.lines().any(|l| l == "PROMPT>MOREDONE"),
        "the completed line must be logged whole.\nLog:\n{final_text}"
    );
    assert_eq!(
        final_text.matches("PROMPT>").count(),
        1,
        "the provisional open line must be rewound, not duplicated.\nLog:\n{final_text}"
    );
}
