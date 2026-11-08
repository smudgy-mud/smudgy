//! The text-search line edits (`line.replace`, `line.remove`, `line.highlight`) act on
//! EVERY occurrence of the search string, not just the first. The replace case grows the
//! text and puts a multi-byte character before the first match, so it fails loudly if the
//! occurrences are spliced in the wrong order or addressed by UTF-16 offsets. Runs through
//! the real session runtime, exactly like `line_replace_non_ascii.rs`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::vt_processor::{AnsiColor, Color};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::{LinkAction, Style, StyledLine, VtSpan};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// Triggers that edit in-place and echo nothing, so each edited incoming line is emitted
/// straight to the buffer for the test to read back.
const LINE_EDIT_ALL_TS: &str = r#"
import { createTrigger, echo, line, link, style } from "smudgy:core";

createTrigger("^REP ", () => {
    if (!line.replace("orc", "<orc!>")) echo("REP_MISS");
});

createTrigger("^RM ", () => {
    if (!line.remove(" gone")) echo("RM_MISS");
});

createTrigger("^HL ", () => {
    if (!line.highlight("orc", { fg: "red" })) echo("HL_MISS");
});

createTrigger("^HLB ", () => {
    // A style chain used directly as the options: red + the bold attribute.
    if (!line.highlight("orc", style.red.bold)) echo("HLB_MISS");
});

createTrigger("^HLK ", () => {
    // Restyle AND linkify each match in place.
    if (!line.highlight("orc", { fg: "red", link: link("kill orc") })) echo("HLK_MISS");
});

createTrigger("\\[AUTOLOOT\\]", (matches) => {
    line.highlight(matches[0], style.red.bgWhite);
});

createTrigger("^ERR ", () => {
    // Bad inputs are loud at the call site: an attribute typo, a bad color
    // on the plain path, and a bad color on a LINKED highlight (which must
    // not silently drop the link along with the style).
    let threw = 0;
    try { line.highlight("orc", { attributes: { blod: true } }); } catch { threw += 1; }
    try { line.highlight("orc", { fg: "bogus" }); } catch { threw += 1; }
    try { line.highlight("orc", { fg: "bogus", link: link("kill orc") }); } catch { threw += 1; }
    echo(threw === 3 ? "ERR_OK" : "ERR_FAIL threw=" + threw);
});

echo("EDITALL_READY");
"#;

/// A plain incoming server line: one default-styled span over the whole (byte-length) text.
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
async fn line_text_search_edits_apply_to_every_occurrence() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "LineEditAllOccurrences";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("line_edit_all.ts"), LINE_EDIT_ALL_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7008),
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

    // The multi-byte `caf\u{e9}` shifts bytes ahead of every match, and the replacement is
    // longer than the needle, so left-to-right splicing on stale offsets would mangle the
    // second and third occurrences.
    let rep_incoming = "REP caf\u{e9} orc and orc and orc";
    let rep_expected = "REP caf\u{e9} <orc!> and <orc!> and <orc!>";
    let rm_incoming = "RM one gone two gone three gone";
    let rm_expected = "RM one two three";
    let hl_incoming = "HL an orc and an orc";
    let hlb_incoming = "HLB an orc and an orc";
    let hlk_incoming = "HLK an orc and an orc";
    let err_incoming = "ERR an orc";
    let fragmented_incoming = "[AUTOLOOT] You retrieve a silver disc.";
    let fragmented_completion = "TOLOOT] You retrieve a silver disc.";

    let mut lines: Vec<Arc<StyledLine>> = Vec::new();
    let mut sent = false;
    loop {
        let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line)
                | BufferUpdate::FinishOpenLineReplacement(Some(line)) = update
                {
                    lines.push(line.clone());
                    if !sent && line.text == "EDITALL_READY" {
                        for incoming in [
                            rep_incoming,
                            rm_incoming,
                            hl_incoming,
                            hlb_incoming,
                            hlk_incoming,
                            err_incoming,
                        ] {
                            tx.send(RuntimeAction::HandleIncomingLine(plain_line(incoming)))
                                .unwrap();
                        }
                        // Model the exact producer contract for a socket read
                        // boundary inside one logical line: the prefix is
                        // provisionally displayed, then the newline-bearing
                        // completion carries both the assembled match subject
                        // and the unseen suffix.
                        tx.send(RuntimeAction::HandleIncomingPartialLine(plain_line("[AU")))
                            .unwrap();
                        tx.send(RuntimeAction::HandleIncomingFragmentedLine {
                            line: plain_line(fragmented_incoming),
                            completion_fragment: plain_line(fragmented_completion),
                        })
                        .unwrap();
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
        lines.iter().any(|l| l.text == rep_expected),
        "line.replace must replace every occurrence, splicing right to left.\n\
         expected: {rep_expected:?}\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l.text == rm_expected),
        "line.remove must remove every occurrence.\n\
         expected: {rm_expected:?}\nTranscript:\n{transcript}"
    );

    // The highlight line keeps its text; every "orc" range must have been recolored red.
    let highlighted = lines
        .iter()
        .find(|l| l.text == hl_incoming)
        .unwrap_or_else(|| {
            panic!("the HL line must still be delivered.\nTranscript:\n{transcript}")
        });
    let red_texts: Vec<&str> = highlighted
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
        .map(|span| &highlighted.text[span.begin_pos..span.end_pos])
        .collect();
    assert_eq!(
        red_texts,
        ["orc", "orc"],
        "line.highlight must recolor every occurrence.\nTranscript:\n{transcript}"
    );

    // The style-chain form: `style.red.bold` recolors AND sets the bold
    // attribute on the matched ranges, while everything the chain left unset
    // (here: the rest of each span's attributes, and all other spans) is
    // untouched.
    let chain_highlighted = lines
        .iter()
        .find(|l| l.text == hlb_incoming)
        .unwrap_or_else(|| {
            panic!("the HLB line must still be delivered.\nTranscript:\n{transcript}")
        });
    let (red_spans, plain_spans): (Vec<&VtSpan>, Vec<&VtSpan>) =
        chain_highlighted.spans.iter().partition(|span| {
            matches!(
                span.style.fg,
                Color::Ansi {
                    color: AnsiColor::Red,
                    ..
                }
            )
        });
    let red_texts: Vec<&str> = red_spans
        .iter()
        .map(|span| &chain_highlighted.text[span.begin_pos..span.end_pos])
        .collect();
    assert_eq!(
        red_texts,
        ["orc", "orc"],
        "a style chain as highlight options must recolor every occurrence.\nTranscript:\n{transcript}"
    );
    assert!(
        red_spans.iter().all(|span| span.style.attributes.bold),
        "style.red.bold must set the bold attribute on the matched ranges"
    );
    assert!(
        plain_spans
            .iter()
            .all(|span| !span.style.attributes.bold && !span.style.attributes.italic),
        "spans outside the matches must be untouched"
    );

    let fragmented = lines
        .iter()
        .find(|line| line.text == fragmented_incoming)
        .unwrap_or_else(|| {
            panic!(
                "the fragmented AUTOLOOT line must be atomically replaced.\nTranscript:\n{transcript}"
            )
        });
    assert!(
        fragmented.spans.iter().any(|span| {
            &fragmented.text[span.begin_pos..span.end_pos] == "[AUTOLOOT]"
                && matches!(
                    span.style.fg,
                    Color::Ansi {
                        color: AnsiColor::Red,
                        ..
                    }
                )
                && matches!(
                    span.style.bg,
                    Color::Ansi {
                        color: AnsiColor::White,
                        ..
                    }
                )
        }),
        "a normal trigger must match and highlight across an arbitrary read-batch boundary"
    );

    // A link in the highlight options recolors AND linkifies each match in
    // place: one link span per occurrence, over otherwise-untouched text.
    let linkified = lines
        .iter()
        .find(|l| l.text == hlk_incoming)
        .unwrap_or_else(|| {
            panic!("the HLK line must still be delivered.\nTranscript:\n{transcript}")
        });
    let link_texts: Vec<&str> = linkified
        .links
        .iter()
        .map(|link| &linkified.text[link.begin_pos..link.end_pos])
        .collect();
    assert_eq!(
        link_texts,
        ["orc", "orc"],
        "highlight with a link tag must linkify every occurrence.\nTranscript:\n{transcript}"
    );
    assert!(
        linkified
            .links
            .iter()
            .all(|link| matches!(&link.action, LinkAction::Send(cmd) if &**cmd == "kill orc")),
        "each link must carry the tag's send action"
    );
    let red_link_texts: Vec<&str> = linkified
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
        .map(|span| &linkified.text[span.begin_pos..span.end_pos])
        .collect();
    assert_eq!(
        red_link_texts,
        ["orc", "orc"],
        "the linked highlight must still recolor the matches"
    );

    assert!(
        lines.iter().any(|l| l.text == "ERR_OK"),
        "attribute typos and linked-highlight color errors must throw.\nTranscript:\n{transcript}"
    );
}
