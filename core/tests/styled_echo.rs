//! Styled echo end-to-end: the `style` tagged-template surface flattens fragments into
//! the styled-echo op, and the runtime appends `StyledLine`s whose spans carry the
//! declared colors, tile the text gap-free, and default unstyled text to the echo role.
//!
//! The module below runs at load and echoes a fixed set of styled lines; the test reads
//! the appended `StyledLine`s off the buffer stream and asserts their spans exactly.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::vt_processor::{AnsiColor, Color};
use smudgy_core::session::runtime::{IsolateId, RuntimeAction};
use smudgy_core::session::styled_line::{LinkAction, LinkSpan, Style, StyledLine};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const STYLED_ECHO_TS: &str = r#"
import { echo, style } from "smudgy:core";

// 1: literal parts + a named-color fragment; unstyled text takes the echo role.
echo(style`plain ${style.red`red`} tail`);
// 2: echo used directly as a tag; exact-RGB fg + named bg; a number interpolates as text.
echo`T ${style.fg({ r: 10, g: 20, b: 30 }).bgBlue`x`}${42}`;
// 3: lexical inheritance; the inner fragment keeps the enclosing background.
echo(style.bgBlue`x${style.red`y`}z`);
// 4: a fragment spanning a newline echoes as two whole lines.
echo(style.green`one\ntwo`);
// 5: unknown color names are loud TypeErrors, not silent output corruption.
try {
    echo(style.fg("nope" as any)`bad`);
    echo("ERR_MISSED");
} catch (e) {
    echo((e instanceof TypeError) ? "ERR_OK" : "ERR_WRONG");
}
// 6: "default" as a BACKGROUND means the default background, not the foreground color.
echo(style.bg("default")`bgdef`);
// 7: an illegal escape in a tagged template falls back to the raw text.
echo(style.red`C:\utils`);
echo("SE_DONE");
"#;

const ECHO_STYLE: Style = Style {
    fg: Color::Echo,
    bg: Color::DefaultBackground,
};

const fn bright(color: AnsiColor) -> Color {
    Color::Ansi { color, bold: true }
}

/// The renderer tiles `text[span]` per span in order; assert the invariant, rebuilding
/// the text from the slices so an overlap or gap cannot hide behind matching offsets.
fn assert_tiles(line: &StyledLine) {
    let mut cursor = 0;
    let mut rendered = String::new();
    for span in &line.spans {
        assert_eq!(
            span.begin_pos, cursor,
            "gap/overlap before span in {:?} of {:?}",
            line.spans, line.text
        );
        assert!(span.end_pos >= span.begin_pos);
        rendered.push_str(&line.text[span.begin_pos..span.end_pos]);
        cursor = span.end_pos;
    }
    assert_eq!(cursor, line.text.len(), "spans do not cover {:?}", line.text);
    assert_eq!(rendered, line.text, "spans do not tile {:?}", line.text);
}

fn spans_of(line: &StyledLine) -> Vec<(usize, usize, Style)> {
    line.spans
        .iter()
        .map(|s| (s.begin_pos, s.end_pos, s.style))
        .collect()
}

#[tokio::test]
async fn styled_echo_spans_reach_the_buffer() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "StyledEcho";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("styled_echo.ts"), STYLED_ECHO_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7106),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));

    // Collect appended lines until the module's final sentinel arrives (the buffer
    // channel is ordered, so everything echoed before it has been observed).
    let mut lines: Vec<Arc<StyledLine>> = Vec::new();
    'outer: loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for the styled-echo transcript")
            .expect("event stream ended before SE_DONE");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.clone());
                    if line.text == "SE_DONE" {
                        break 'outer;
                    }
                }
            }
        }
    }

    let transcript = lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let find = |text: &str| {
        lines
            .iter()
            .find(|l| l.text == text)
            .unwrap_or_else(|| panic!("no line {text:?} in transcript:\n{transcript}"))
    };

    for line in &lines {
        assert_tiles(line);
    }

    // 1: unstyled text = echo role; the named fragment is bright red.
    let one = find("plain red tail");
    assert_eq!(
        spans_of(one),
        vec![
            (0, 6, ECHO_STYLE),
            (
                6,
                9,
                Style {
                    fg: bright(AnsiColor::Red),
                    bg: Color::DefaultBackground
                }
            ),
            (9, 14, ECHO_STYLE),
        ]
    );

    // 2: echo-as-tag; exact RGB fg over a named bg; the number interpolated as plain text.
    let two = find("T x42");
    assert_eq!(
        spans_of(two),
        vec![
            (0, 2, ECHO_STYLE),
            (
                2,
                3,
                Style {
                    fg: Color::Rgb { r: 10, g: 20, b: 30 },
                    bg: bright(AnsiColor::Blue)
                }
            ),
            (3, 5, ECHO_STYLE),
        ]
    );

    // 3: the inner red fragment inherits the enclosing blue background.
    let three = find("xyz");
    let on_blue = |fg: Color| Style {
        fg,
        bg: bright(AnsiColor::Blue),
    };
    assert_eq!(
        spans_of(three),
        vec![
            (0, 1, on_blue(Color::Echo)),
            (1, 2, on_blue(bright(AnsiColor::Red))),
            (2, 3, on_blue(Color::Echo)),
        ]
    );

    // 4: the newline split both whole lines, each keeping the fragment's color.
    for text in ["one", "two"] {
        let line = find(text);
        assert_eq!(
            spans_of(line),
            vec![(
                0,
                text.len(),
                Style {
                    fg: bright(AnsiColor::Green),
                    bg: Color::DefaultBackground
                }
            )]
        );
    }

    // 6: bg "default" resolves to the default background, not a foreground paint.
    let bgdef = find("bgdef");
    assert_eq!(
        spans_of(bgdef),
        vec![(0, 5, ECHO_STYLE)],
        "bg \"default\" must resolve to DefaultBackground"
    );

    // 7: the `C:\utils` template's cooked strings are undefined (illegal \u escape);
    // the raw text must be echoed instead.
    let raw = find("C:\\utils");
    assert_eq!(
        spans_of(raw),
        vec![(
            0,
            8,
            Style {
                fg: bright(AnsiColor::Red),
                bg: Color::DefaultBackground
            }
        )]
    );

    // 5: the bad color name threw a TypeError before anything was echoed.
    assert!(
        lines.iter().any(|l| l.text == "ERR_OK"),
        "unknown color must throw TypeError.\nTranscript:\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l.text == "ERR_MISSED" || l.text == "bad"),
        "the failed echo must not deliver.\nTranscript:\n{transcript}"
    );
}

const STYLED_SPLICE_TS: &str = r#"
import { createTrigger, echo, style, link, line } from "smudgy:core";

// The flagship use case: turn an exit name in a MUD line into a styled command link.
createTrigger("^You see exits", () => {
    const replaced = line.replace("north", link("north")`${style.cyan`north`}`);
    if (!replaced) echo("SPLICE_MISS");
});

// A styled insert whose options act as the inheritance base for its unset colors.
createTrigger("^ITEM", () => {
    line.insert(style`[${style.red`hot`}]`, 0, 0, { fg: "yellow" });
});

// Splicing a fragment containing a newline is a loud error, and the line survives.
createTrigger("^BADSPLICE$", () => {
    try {
        line.replace("BADSPLICE", style.red`two\nlines`);
        echo("NL_MISSED");
    } catch (e) {
        echo((e instanceof TypeError) ? "NL_OK" : "NL_WRONG");
    }
});

echo("SP_READY");
"#;

/// Styled line edits end to end: a trigger splices styled/linked fragments into
/// real incoming lines, and the completed lines carry the expected spans and links.
#[tokio::test]
async fn styled_splice_edits_incoming_lines() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "StyledSplice";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("styled_splice.ts"), STYLED_SPLICE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7108),
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

    // The incoming lines carry a known RGB style so inheritance is observable.
    let rgb = Style {
        fg: Color::Rgb { r: 10, g: 20, b: 30 },
        bg: Color::DefaultBackground,
    };
    let incoming = |text: &str| {
        Arc::new(StyledLine::new(
            text,
            vec![smudgy_core::session::styled_line::VtSpan {
                style: rgb,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        ))
    };

    let mut lines: Vec<Arc<StyledLine>> = Vec::new();
    let mut sent = false;
    'done: loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for the splice transcript")
            .expect("event stream ended early");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.clone());
                    if !sent && line.text == "SP_READY" {
                        sent = true;
                        for text in ["You see exits: north and south", "ITEM thing", "BADSPLICE"]
                        {
                            tx.send(RuntimeAction::HandleIncomingLine(incoming(text)))
                                .unwrap();
                            tx.send(RuntimeAction::RequestRepaint).unwrap();
                        }
                    }
                    // A trigger's echo emits depth-first AHEAD of the incoming line it
                    // fired on, so the BADSPLICE line itself is the last append.
                    if line.text == "BADSPLICE" {
                        break 'done;
                    }
                }
            }
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let find = |text: &str| {
        lines
            .iter()
            .find(|l| l.text == text)
            .unwrap_or_else(|| panic!("no line {text:?} in transcript:\n{transcript}"))
    };
    let style_at = |line: &StyledLine, at: usize| {
        line.spans
            .iter()
            .find(|span| span.begin_pos <= at && at < span.end_pos)
            .unwrap_or_else(|| panic!("no span at {at} in {:?}", line.spans))
            .style
    };

    // replace(): "north" (bytes 15..20) became a cyan command link; the rest of the
    // line keeps the server's RGB style, and the spans still tile.
    let exits = find("You see exits: north and south");
    assert_tiles(exits);
    assert_eq!(
        exits.links,
        vec![smudgy_core::session::styled_line::LinkSpan {
            begin_pos: 15,
            end_pos: 20,
            action: LinkAction::Send(Arc::from("north")),
        }]
    );
    assert_eq!(style_at(exits, 14), rgb);
    assert_eq!(
        style_at(exits, 15).fg,
        Color::Ansi { color: AnsiColor::Cyan, bold: true }
    );
    assert_eq!(style_at(exits, 20), rgb);

    // insert() with options: "[" and "]" take the options base (yellow), "hot" is red.
    let item = find("[hot]ITEM thing");
    assert_tiles(item);
    let yellow = Color::Ansi { color: AnsiColor::Yellow, bold: true };
    assert_eq!(style_at(item, 0).fg, yellow);
    assert_eq!(
        style_at(item, 1).fg,
        Color::Ansi { color: AnsiColor::Red, bold: true }
    );
    assert_eq!(style_at(item, 4).fg, yellow);
    assert_eq!(style_at(item, 5), rgb);

    // The newline splice threw a TypeError and the line survived unedited.
    assert!(
        lines.iter().any(|l| l.text == "NL_OK"),
        "newline splice must throw TypeError.\nTranscript:\n{transcript}"
    );
    let bad = find("BADSPLICE");
    assert_eq!(bad.text, "BADSPLICE");
}

const STYLED_LINKS_TS: &str = r#"
import { echo, style, link } from "smudgy:core";

// A command link wrapping a styled fragment: the link range covers the fragment.
echo`Exit: ${link("north")`${style.cyan`north`}`}.`;
// A callback link; the handler reports the click's modifiers.
echo`${link((click) => echo("CLICKED shift=" + click.shift))`Click me`}`;
echo("SL_READY");
"#;

/// Links end to end minus the pointer: the echoed lines carry the expected
/// `LinkSpan`s, a simulated click (the exact action the UI sends) runs the callback
/// in its isolate, and a stale instance nonce or unknown id is a silent no-op.
#[tokio::test]
async fn styled_links_carry_spans_and_callbacks_fire() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "StyledLinks";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("styled_links.ts"), STYLED_LINKS_TS).unwrap();

    let session_id = SessionId::from(7107);
    let params = Arc::new(SessionParams {
        session_id,
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

    let mut lines: Vec<Arc<StyledLine>> = Vec::new();
    'ready: loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for the link transcript")
            .expect("event stream ended before SL_READY");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.clone());
                    if line.text == "SL_READY" {
                        break 'ready;
                    }
                }
            }
        }
    }

    let transcript = lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");

    // The command link covers exactly the styled "north" fragment.
    let exit = lines
        .iter()
        .find(|l| l.text == "Exit: north.")
        .unwrap_or_else(|| panic!("no exit line in transcript:\n{transcript}"));
    assert_eq!(
        exit.links,
        vec![LinkSpan {
            begin_pos: 6,
            end_pos: 11,
            action: LinkAction::Send(Arc::from("north")),
        }]
    );

    // The callback link carries this session + a resolvable isolate token + an id.
    let clickable = lines
        .iter()
        .find(|l| l.text == "Click me")
        .unwrap_or_else(|| panic!("no clickable line in transcript:\n{transcript}"));
    assert_eq!(clickable.links.len(), 1);
    assert_eq!(clickable.links[0].begin_pos, 0);
    assert_eq!(clickable.links[0].end_pos, 8);
    let LinkAction::Callback {
        session,
        ref isolate_token,
        id,
    } = clickable.links[0].action
    else {
        panic!("expected a callback link, got {:?}", clickable.links[0].action);
    };
    assert_eq!(session, session_id);
    let (isolate, instance) = IsolateId::from_widget_token(isolate_token);

    // Click it, exactly as the UI would: the handler runs in its isolate and sees
    // the modifiers.
    tx.send(RuntimeAction::InvokeLinkCallback {
        session,
        isolate: isolate.clone(),
        instance,
        id,
        shift: true,
        ctrl: false,
        alt: false,
    })
    .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the click echo")
            .expect("event stream ended waiting for the click echo");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            if updates.iter().any(|update| {
                matches!(update, BufferUpdate::Append(line) if line.text == "CLICKED shift=true")
            }) {
                break;
            }
        }
    }

    // A stale instance nonce (an engine rebuild happened) and an unknown id are
    // both silent no-ops: nothing new appears on the buffer.
    tx.send(RuntimeAction::InvokeLinkCallback {
        session,
        isolate: isolate.clone(),
        instance: instance + 1,
        id,
        shift: false,
        ctrl: false,
        alt: false,
    })
    .unwrap();
    tx.send(RuntimeAction::InvokeLinkCallback {
        session,
        isolate,
        instance,
        id: id + 1000,
        shift: false,
        ctrl: false,
        alt: false,
    })
    .unwrap();
    let mut extra = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(900), events.next()).await
    {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    extra.push(line.text.clone());
                }
            }
        }
    }
    assert!(
        extra.iter().all(|text| !text.starts_with("CLICKED")),
        "stale/unknown link clicks must be silent no-ops, got: {extra:?}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}
