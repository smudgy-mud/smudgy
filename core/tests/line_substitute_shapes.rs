//! Reproduction harness for the substitute shape tt2smudgy and the tintin-emulator
//! package generate: an UNANCHORED trigger pattern whose handler replaces
//! `matches[0]` — the matched text itself — with plain or styled text. The
//! anchored/hardcoded-needle case is covered by `line_edit_all_occurrences.rs`;
//! this exercises the matched-text needle and the styled-splice write path.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::vt_processor::{AnsiColor, Color};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::{Style, StyledLine, VtSpan};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// Exactly the shapes the converters emit for `#substitute`.
const SUBSTITUTE_TS: &str = r#"
import { createTrigger, echo, line, style } from "smudgy:core";

// tt2smudgy plain substitute: unanchored pattern, needle = matches[0].
createTrigger("a dragon", (matches) => {
    if (!line.replace(matches[0], "the wyrm")) echo("PLAIN_MISS " + JSON.stringify(matches[0]));
});

// tt2smudgy styled substitute: replacement is StyledText.
const red = style({ fg: "red" });
createTrigger("gold coin", (matches) => {
    if (!line.replace(matches[0], red`shiny coin`)) echo("STYLED_MISS " + JSON.stringify(matches[0]));
});

// Capture-bearing pattern, like `#substitute {%1 hits you} {...}`.
createTrigger("(\\w+) hits you", (matches) => {
    if (!line.replace(matches[0], `${matches[1]} misses`)) echo("CAP_MISS " + JSON.stringify(matches[0]));
});

echo("SUBSHAPES_READY");
"#;

fn plain_line(text: &str) -> Arc<StyledLine> {
    let span = VtSpan {
        style: Style {
            fg: Color::DefaultForeground { bold: false },
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
        },
        begin_pos: 0,
        end_pos: text.len(),
    };
    Arc::new(StyledLine::new(text, vec![span]))
}

#[tokio::test]
async fn substitute_shapes_replace_matched_text() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "LineSubstituteShapes";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("substitute_shapes.ts"), SUBSTITUTE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7009),
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

    let plain_incoming = "You see a dragon here.";
    let plain_expected = "You see the wyrm here.";
    let styled_incoming = "A gold coin lies here.";
    let styled_expected = "A shiny coin lies here.";
    let cap_incoming = "The orc hits you hard.";
    let cap_expected = "The orc misses hard.";

    let mut lines: Vec<Arc<StyledLine>> = Vec::new();
    let mut sent = false;
    loop {
        let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.clone());
                    if !sent && line.text == "SUBSHAPES_READY" {
                        for incoming in [plain_incoming, styled_incoming, cap_incoming] {
                            tx.send(RuntimeAction::HandleIncomingLine(plain_line(incoming)))
                                .unwrap();
                        }
                        tx.send(RuntimeAction::RequestRepaint).unwrap();
                        sent = true;
                    }
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        lines.iter().any(|l| l.text == plain_expected),
        "plain matched-text substitute must replace.\nexpected: {plain_expected:?}\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l.text == styled_expected),
        "styled matched-text substitute must replace.\nexpected: {styled_expected:?}\nTranscript:\n{transcript}"
    );
    // The styled replacement must actually carry its color.
    let styled_line = lines.iter().find(|l| l.text == styled_expected);
    if let Some(styled_line) = styled_line {
        let red_texts: Vec<&str> = styled_line
            .spans
            .iter()
            .filter(|span| {
                matches!(
                    span.style.fg,
                    Color::Ansi {
                        color: AnsiColor::Red,
                        ..
                    }
                )
            })
            .map(|span| &styled_line.text[span.begin_pos..span.end_pos])
            .collect();
        assert_eq!(
            red_texts,
            ["shiny coin"],
            "the styled splice must recolor exactly the replacement.\nTranscript:\n{transcript}"
        );
    }
    assert!(
        lines.iter().any(|l| l.text == cap_expected),
        "capture-bearing substitute must replace using matches[0].\nexpected: {cap_expected:?}\nTranscript:\n{transcript}"
    );
}
