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

/// Inter-event drain window. Must comfortably outlast the idle test's 150ms
/// timer even when a missed waker defers its callback to the session run
/// loop's 500ms safety tick (worst case ~650ms after the line).
const QUIET_PERIOD: Duration = Duration::from_millis(2000);

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
    // Hermetic smudgy home (first-setter-wins across this binary's tests, so
    // re-read the winner before writing under it).
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

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
        graphics_decoder: None,
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
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
            StyledLine::new(text, Vec::new()),
        )))
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

/// The current-line window: `line` ops are valid until the line in flight
/// finishes processing — for ANY code that runs in that window (event
/// recipients a handler emits to, async continuations that resume during the
/// cascade), not just the trigger/`sys:receive` entry run for the line. The
/// flip side, pinned here: a continuation that outlives its OWN line and
/// resumes while a LATER line is mid-flight acts on that later line. The gate
/// promise is resolved by the handler run for the second line, so the first
/// handler's continuation resumes precisely between that line's handler
/// splice and its completion action (resolved promises pump between actions)
/// — its `gag()` therefore hides the SECOND line. That wrong-line window is
/// the deliberate price of letting a handler's own mid-line `await`
/// continuations act on their line.
const SYS_RECEIVE_STALE_TS: &str = r#"
import { echo, line } from "smudgy:core";
import { receive } from "smudgy:events/sys";

let release: (() => void) | undefined;
const gate = new Promise<void>((resolve) => { release = resolve; });

receive.on(async ({ text }) => {
    if (text === "first line") {
        await gate;
        try {
            line.gag();
            echo("STALE:GAGGED");
        } catch (e) {
            echo("STALE:THREW:" + ((e as any)?.message ?? String(e)));
        }
    } else if (text === "second line") {
        release!();
    }
});
"#;

#[tokio::test]
async fn continuation_resuming_during_a_later_line_acts_on_that_line() {
    // Hermetic smudgy home (first-setter-wins across this binary's tests).
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "SysReceiveStale";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("sys_receive.ts"), SYS_RECEIVE_STALE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7014),
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
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    let feed = |text: &str| {
        tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
            StyledLine::new(text, Vec::new()),
        )))
        .unwrap();
    };
    feed("first line");
    feed("second line");

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
    // The continuation resumed while the second line was in flight, so its gag succeeds…
    assert!(
        lines.iter().any(|l| l == "STALE:GAGGED"),
        "a continuation resuming while a line is in flight may act on it.\nTranscript:\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("STALE:THREW:")),
        "the gag must not throw while a line is in flight.\nTranscript:\n{transcript}"
    );
    // …and gags the line that was in flight when it resumed (the second line).
    assert!(
        !lines.iter().any(|l| l == "second line"),
        "the line in flight at resume time is the one the gag hides.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "first line"),
        "the handler's own line displayed normally.\nTranscript:\n{transcript}"
    );
}

/// With NO line in flight — here, a continuation parked on a timer that
/// fires after every line has been routed — `line.gag()` throws the
/// current-line contract error instead of leaking into the per-line
/// routing/transform cells (where it would silently apply to whatever line
/// arrives next). This also pins dispatch's completion-time
/// `set_current_line(None)` clears as load-bearing: when the timer fires,
/// the processed line is still strongly held by the recent-lines ring, so a
/// gate keyed on `Arc` liveness alone would let this gag through.
const SYS_RECEIVE_IDLE_TS: &str = r#"
import { echo, line } from "smudgy:core";
import { receive } from "smudgy:events/sys";

receive.on(async ({ text }) => {
    if (text !== "only line") return;
    await new Promise<void>((resolve) => setTimeout(resolve, 150));
    try {
        line.gag();
        echo("IDLE:NO_THROW");
    } catch (e) {
        echo("IDLE:THREW:" + ((e as any)?.message ?? String(e)));
    }
});
"#;

#[tokio::test]
async fn continuation_resuming_between_lines_throws() {
    // Hermetic smudgy home (first-setter-wins across this binary's tests).
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "SysReceiveIdle";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("sys_receive.ts"), SYS_RECEIVE_IDLE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7015),
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
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    tx.send(RuntimeAction::HandleIncomingLine(Arc::new(
        StyledLine::new("only line", Vec::new()),
    )))
    .unwrap();

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
    assert!(
        !lines.iter().any(|l| l == "IDLE:NO_THROW"),
        "a gag with no line in flight must not succeed.\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("IDLE:THREW:") && l.contains("current line")),
        "the idle gag must throw the current-line contract error.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "only line"),
        "the handler's own line displayed normally.\nTranscript:\n{transcript}"
    );
}
