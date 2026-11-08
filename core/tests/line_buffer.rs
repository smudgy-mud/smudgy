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
use smudgy_core::session::connection::vt_processor::{AnsiColor, Color};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::{
    Blink, Style, StyledLine, TextAttributes, Underline, VtSpan,
};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const COMPLETION_TIMEOUT: Duration = Duration::from_mins(1);

/// A module that registers the triggers. Each trigger handler echoes a single sentinel
/// encoding its pass/fail so the test asserts on the buffer transcript.
///
/// - `style`: fires on an incoming line carrying every text attribute. The handler reads
///   `line.text`, `line.number`, `line.styles`, then proves the
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
    const first = (styles && styles.length > 0) ? styles[0] : null;
    const fg = first?.fg;
    const bg = first?.bg;
    const defaultFg = styles?.[1]?.fg;
    const brightDefaultFg = styles?.[2]?.fg;
    const attributes = first?.attributes;
    const styleExact = fg !== null && typeof fg === "object"
        && fg.color === "red" && fg.bold === true && fg.paletteBright === false
        && attributes?.bold === true && attributes.faint === true
        && attributes.italic === true && attributes.underline === "double"
        && attributes.blink === "fast" && attributes.crossedOut === true
        && attributes.reverse === true
        && typeof bg === "object" && bg.color === "blue"
        && bg.bold === false && bg.paletteBright === false
        // Keep the released StyleSpan contract: a bold default foreground is
        // still the string token; font weight lives in attributes.bold.
        && defaultFg === "default" && styles[1].attributes.bold === true
        && brightDefaultFg === "default"
        && styles[2].attributes.bold === false
        && styles[2].foregroundPaletteBright === true;
    // A complete StyleSpan is itself valid write options. paletteBright keeps
    // the legacy conflated fg.bold from repainting this dim palette slot bright.
    if (styleExact) {
        line.highlightAt(0, 1, first);
        line.highlightAt(styles[1].begin, styles[1].end, styles[1]);
        line.highlightAt(styles[2].begin, styles[2].end, styles[2]);
    }
    vars.styleLineNumber = num;
    vars.styleOk = styleExact && typeof text === "string" && text.indexOf("STYLE") === 0;
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
    const readback = buffer.line(n).styles;
    const first = readback?.[0];
    const brightDefault = readback?.find((span) => span.foregroundPaletteBright === true);
    const roundTrip = typeof first?.fg === "object"
        && first.fg.color === "red" && first.fg.bold === true
        && first.fg.paletteBright === false
        && first.attributes.bold === true
        && first.attributes.underline === "double"
        && first.attributes.blink === "fast"
        && brightDefault?.fg === "default"
        && brightDefault.attributes.bold === false;
    echo((text.indexOf("EDITED") !== -1 && text.indexOf("STYLE ") === -1 && roundTrip)
        ? "CHECK_OK"
        : ("CHECK_FAIL text=" + text + " style=" + JSON.stringify(first)));
});

echo("LB_READY");
"#;

/// Build a line that separates legacy effective bold, raw palette brightness,
/// and font weight, and exercise every lossless text attribute.
fn attributed_line(text: &str) -> Arc<StyledLine> {
    let attributes = TextAttributes {
        bold: true,
        faint: true,
        italic: true,
        underline: Underline::Double,
        blink: Blink::Fast,
        crossed_out: true,
        reverse: true,
    };
    Arc::new(StyledLine::new(
        text,
        vec![
            VtSpan {
                style: Style {
                    fg: Color::Ansi {
                        color: AnsiColor::Red,
                        bold: false,
                    },
                    bg: Color::Ansi {
                        color: AnsiColor::Blue,
                        bold: false,
                    },
                    attributes,
                },
                begin_pos: 0,
                end_pos: 1,
            },
            VtSpan {
                style: Style {
                    fg: Color::DefaultForeground { bold: false },
                    bg: Color::DefaultBackground,
                    attributes,
                },
                begin_pos: 1,
                end_pos: text.len().saturating_sub(1),
            },
            VtSpan {
                style: Style {
                    fg: Color::DefaultForeground { bold: true },
                    bg: Color::DefaultBackground,
                    attributes: TextAttributes::DEFAULT,
                },
                begin_pos: text.len().saturating_sub(1),
                end_pos: text.len(),
            },
        ],
    ))
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

    // Drive the four incoming lines in sequence, each gated on the prior sentinel so the
    // triggers are registered and (for BUF) the STYLE line has been emitted into the ring.
    // The loop ends on a terminal sentinel (or the overall deadline), never on stream
    // quiescence: a loaded runner can hold the runtime silent far longer than any fixed
    // quiet period while the isolate is still compiling or dispatching.
    let mut lines: Vec<String> = Vec::new();
    let mut sent_style = false;
    let mut sent_find = false;
    let mut sent_buf = false;
    let mut sent_check = false;
    let terminal = |line: &String| {
        line.as_str() == "CHECK_OK"
            || line.starts_with("CHECK_FAIL")
            || line.starts_with("FIND_FAIL")
            || line.starts_with("BUF_FAIL")
    };
    let deadline = tokio::time::Instant::now() + COMPLETION_TIMEOUT;
    while !lines.iter().any(terminal) {
        let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.next()).await else {
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
                        tx.send(RuntimeAction::HandleIncomingLine(attributed_line(
                            "STYLE here",
                        )))
                        .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent_style = true;
                    }
                    // The `style` handler echoes nothing (to keep the incoming line's number
                    // stable), so gate the next step on the incoming server line itself.
                    if sent_style && !sent_find && line.text == "STYLE here" {
                        tx.send(RuntimeAction::HandleIncomingLine(attributed_line("FIND")))
                            .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent_find = true;
                    }
                    if !sent_buf && line.text == "FIND_OK" {
                        tx.send(RuntimeAction::HandleIncomingLine(attributed_line("BUF")))
                            .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent_buf = true;
                    }
                    if !sent_check && line.text == "BUF_OK" {
                        tx.send(RuntimeAction::HandleIncomingLine(attributed_line("CHECK")))
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
        "line.styles must expose and losslessly round-trip terminal attributes.\nTranscript:\n{transcript}"
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
