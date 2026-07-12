//! `line.replace(oldStr, newStr)` must address the line by UTF-8 byte offset even though
//! the match position/length it derives from `indexOf`/`.length` are UTF-16 code units.
//! This feeds an incoming line whose match is preceded by a multi-byte character, so a
//! UTF-16 offset used as a byte offset would splice at the wrong place (or on the wrong
//! char boundary). Runs through the real session runtime, exactly like `line_buffer.rs`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::vt_processor::Color;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::{Style, StyledLine, VtSpan};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// A trigger that wraps the item name in-place. The handler echoes nothing, so the wrapped
/// incoming line is emitted straight to the buffer for the test to read back.
const LINE_REPLACE_TS: &str = r#"
import { createTrigger, line } from "smudgy:core";

createTrigger("roasted turkey leg", () => {
    line.replace("a roasted turkey leg", "<a roasted turkey leg>");
});

// `echo` from smudgy:core, used only as a readiness sentinel.
import { echo } from "smudgy:core";
echo("REPLACE_READY");
"#;

/// A plain incoming server line: one default-styled span over the whole (byte-length) text.
fn plain_line(text: &str) -> Arc<StyledLine> {
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
async fn line_replace_uses_utf8_byte_offsets_after_multibyte_prefix() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "LineReplaceUtf8";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("line_replace.ts"), LINE_REPLACE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7007),
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

    // `caf\u{e9}` ("café") puts a 2-byte UTF-8 char before the match, so the match's UTF-16
    // index (6) is one less than its byte offset (7). A correct replace preserves the whole
    // line; a UTF-16-as-bytes replace would clip the leading space and the trailing " here".
    let incoming = "caf\u{e9}: a roasted turkey leg here";
    let expected = "caf\u{e9}: <a roasted turkey leg> here";

    let mut lines = Vec::new();
    let mut sent = false;
    loop {
        let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                    if !sent && line.text == "REPLACE_READY" {
                        tx.send(RuntimeAction::HandleIncomingLine(plain_line(incoming)))
                            .unwrap();
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent = true;
                    }
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == expected),
        "line.replace after a multi-byte prefix must splice on UTF-8 byte offsets.\n\
         expected: {expected:?}\nTranscript:\n{transcript}"
    );
}
