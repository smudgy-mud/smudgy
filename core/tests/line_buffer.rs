//! The unified `Line` type (line / buffer.line(n)) reads text AND styles,
//! the buffer-write-through keeps the session-side ring consistent with the screen, styles
//! round-trip through the write color API, and the find-first methods return real booleans.
//!
//! These exercise the genuine session runtime: a module registers triggers, and the test
//! feeds real `HandleIncomingLine`s (so there is a current line + an emitted-line ring) and
//! reads the echoed sentinels back off the buffer stream.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::{Style, StyledLine, VtSpan};
use smudgy_core::session::connection::vt_processor::Color;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// A module that registers the triggers. Each trigger handler echoes a single sentinel
/// encoding its pass/fail so the test asserts on the buffer transcript.
///
/// - `style`: fires on an incoming line we feed with a known RGB span. The handler reads
///   `line.text`, `line.number`, `line.styles` (must reflect the RGB color), then proves the
///   styles value round-trips by passing the first span's `fg` straight into `highlightAt`. It
///   stores the line number in `vars` so the next trigger can address the now-emitted line.
/// - `findfirst`: fires on a separate incoming line and asserts the find-first methods return
///   real booleans (`true` on a hit, `false` on a miss).
/// - `buf`: fires on a third incoming line; by then the `style` line has been emitted into the
///   ring. It reads `buffer.line(N).text`/`.styles`, edits it via `buffer.line(N).replace(...)`
///   (write-through), confirms the edit is visible in a subsequent `buffer.line(N).text`, and
///   confirms a line number outside the window reads as `undefined`.
const LINE_BUFFER_TS: &str = r#"
import { createTrigger, echo, line, buffer, vars } from "smudgy:core";

// The `style` handler must NOT echo: an echo from inside a trigger emits depth-first ahead of
// the incoming line itself, which would shift the incoming line's number off the value
// `line.number` predicted. By echoing nothing, the incoming "STYLE here" line is the very next
// emit and lands on exactly the captured number. Its findings are stashed in `vars` and
// reported later by the `buf` handler.
createTrigger("^STYLE (.+)$", () => {
    const text = line.text;
    const num = line.number;
    const styles = line.styles;
    const firstFg = (styles && styles.length > 0) ? styles[0].fg : null;
    const isRgb = firstFg !== null && typeof firstFg === "object"
        && firstFg.r === 10 && firstFg.g === 20 && firstFg.b === 30;
    // Round-trip: the fg we just read must be a legal value for the write color API.
    if (isRgb) {
        line.highlightAt(0, 1, { fg: firstFg });
    }
    vars.styleLineNumber = num;
    vars.styleOk = isRgb && typeof text === "string" && text.indexOf("STYLE") === 0;
});

createTrigger("^FIND$", () => {
    // find-first reads the LIVE current-line text (pending edits are queued, not yet applied),
    // so every probe targets a substring of the original "FIND" line.
    const hit = line.replace("FIND", "FOUND");
    const miss = line.replace("NOPE", "x");
    const hlHit = line.highlight("FIND", { fg: "red" });
    const hlMiss = line.highlight("zzz", { fg: "red" });
    const rmMiss = line.remove("qqq");
    echo((hit === true && miss === false && hlHit === true && hlMiss === false && rmMiss === false)
        ? "FIND_OK"
        : ("FIND_FAIL hit=" + hit + " miss=" + miss + " hlHit=" + hlHit + " hlMiss=" + hlMiss + " rmMiss=" + rmMiss));
});

createTrigger("^BUF$", () => {
    echo(vars.styleOk === true
        ? ("STYLE_OK num=" + vars.styleLineNumber)
        : "STYLE_FAIL");

    const n = vars.styleLineNumber;
    const before = buffer.line(n).text;
    const stylesReadable = Array.isArray(buffer.line(n).styles);
    // The write-through (op -> PerformLineOperation) is applied to the ring AFTER this
    // synchronous handler returns, so we cannot observe the edit in the same handler. The
    // `check` trigger (a later incoming line) reads it back once the op has run.
    const replaced = buffer.line(n).replace("STYLE", "EDITED");
    // A line number far outside the recent-lines window reads as undefined.
    const outOfWindow = buffer.line(0).text;
    echo((typeof before === "string" && before.indexOf("STYLE") !== -1
            && stylesReadable === true
            && replaced === true
            && buffer.line(0).styles === undefined
            && outOfWindow === "")
        ? "BUF_OK"
        : ("BUF_FAIL before=" + before + " replaced=" + replaced
            + " readable=" + stylesReadable + " oow=" + JSON.stringify(outOfWindow)));
});

createTrigger("^CHECK$", () => {
    // By now the `buf` handler's PerformLineOperation has been applied to the ring entry, so
    // the edit is visible in a fresh read -- proving the ring and the on-screen buffer stayed
    // consistent through the write-through.
    const n = vars.styleLineNumber;
    const text = buffer.line(n).text;
    echo((text.indexOf("EDITED") !== -1 && text.indexOf("STYLE ") === -1)
        ? "CHECK_OK"
        : ("CHECK_FAIL text=" + text));
});

echo("LB_READY");
"#;

/// Build an incoming server line carrying a single known RGB(10,20,30) foreground span over the
/// whole text, so the `style` trigger's `line.styles` read has a concrete, round-trippable color.
fn rgb_line(text: &str) -> Arc<StyledLine> {
    let span = VtSpan {
        style: Style {
            fg: Color::Rgb { r: 10, g: 20, b: 30 },
            bg: Color::DefaultBackground,
        },
        begin_pos: 0,
        end_pos: text.len(),
    };
    Arc::new(StyledLine::new(text, vec![span]))
}

#[tokio::test]
async fn line_buffer_unified_read_styles_writethrough_and_booleans() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "LineBuffer";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("line_buffer.ts"), LINE_BUFFER_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7005),
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

    // Drive the three incoming lines in sequence, each gated on the prior sentinel so the
    // triggers are registered and (for BUF) the STYLE line has been emitted into the ring.
    let mut lines = Vec::new();
    let mut sent_style = false;
    let mut sent_find = false;
    let mut sent_buf = false;
    let mut sent_check = false;
    loop {
        let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                    // `HandleIncomingLine` only stages the line into the pending buffer; the
                    // live path flushes via a follow-up `RequestRepaint` (the vt_processor sends
                    // one after each read batch), so the test does the same after each line.
                    if !sent_style && line.text == "LB_READY" {
                        tx.send(RuntimeAction::HandleIncomingLine(rgb_line("STYLE here")))
                            .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent_style = true;
                    }
                    // The `style` handler echoes nothing (to keep the incoming line's number
                    // stable), so gate the next step on the incoming server line itself.
                    if sent_style && !sent_find && line.text == "STYLE here" {
                        tx.send(RuntimeAction::HandleIncomingLine(rgb_line("FIND")))
                            .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent_find = true;
                    }
                    if !sent_buf && line.text == "FIND_OK" {
                        tx.send(RuntimeAction::HandleIncomingLine(rgb_line("BUF")))
                            .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent_buf = true;
                    }
                    if !sent_check && line.text == "BUF_OK" {
                        tx.send(RuntimeAction::HandleIncomingLine(rgb_line("CHECK")))
                            .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent_check = true;
                    }
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l.starts_with("STYLE_OK")),
        "line.styles must reflect the server RGB color and round-trip into highlightAt.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "FIND_OK"),
        "find-first replace/highlight/remove must return real booleans.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "BUF_OK"),
        "buffer.line(n) must read text/styles within the window and return undefined beyond it.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "CHECK_OK"),
        "a buffer.line(n) write-through must be visible in a later buffer.line(n).text read (ring/screen consistency).\nTranscript:\n{transcript}"
    );
}
