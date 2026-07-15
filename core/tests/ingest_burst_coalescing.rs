//! A socket burst queues one `HandleIncomingLine` per server line, so between
//! consecutive lines the run loop's action stack is empty even though it is
//! nowhere near parking. The drain-point fast path must take the next queued
//! external action WITHOUT the before-park flush, so a burst's lines coalesce
//! into batched `UpdateBuffer` events (delivered by the reader's per-chunk
//! `RequestRepaint` and the storm threshold) instead of one awaited UI event
//! per line — the difference between sub-second and multi-second ingest of a
//! large log replay.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::vt_processor::Color;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::{Style, StyledLine, VtSpan};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const BURST_LINES: usize = 1000;

/// A syntactically valid incoming server line: one default-styled span tiling
/// the whole text (spans must tile gap-free).
fn server_line(text: &str) -> Arc<StyledLine> {
    let span = VtSpan {
        style: Style {
            fg: Color::DefaultForeground { bold: false },
            bg: Color::DefaultBackground,
        },
        begin_pos: 0,
        end_pos: text.len(),
    };
    Arc::new(StyledLine::new(text, vec![span]))
}

#[tokio::test]
async fn ingest_burst_coalesces_buffer_updates() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "IngestBurst";
    std::fs::create_dir_all(home_path.join(server).join("modules")).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7006),
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

    // One chunk's worth of ingest: N line actions then the end-of-buffer
    // repaint, exactly as the connection reader queues them.
    for i in 0..BURST_LINES {
        tx.send(RuntimeAction::HandleIncomingLine(server_line(&format!(
            "burst line {i}"
        ))))
        .unwrap();
    }
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut appended = 0usize;
    let mut events_with_burst_lines = 0usize;
    while appended < BURST_LINES {
        let Ok(Some(event)) = tokio::time::timeout(Duration::from_mins(1), events.next()).await
        else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            let n = updates
                .iter()
                .filter(|u| {
                    matches!(u, BufferUpdate::Append(line) if line.text.starts_with("burst line "))
                })
                .count();
            if n > 0 {
                events_with_burst_lines += 1;
                appended += n;
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    assert_eq!(
        appended, BURST_LINES,
        "every burst line must reach the UI buffer"
    );
    // The exact event count depends on how the test task's sends interleave
    // with the session thread's drain, but coalescing must hold: without the
    // fast path every line flushes its own event (~{BURST_LINES} events).
    assert!(
        events_with_burst_lines <= BURST_LINES / 10,
        "a {BURST_LINES}-line burst must coalesce into batched UpdateBuffer events, \
         got {events_with_burst_lines} events"
    );
}
