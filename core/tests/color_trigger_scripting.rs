//! End-to-end coverage for script-authored trigger style predicates.
//!
//! This deliberately enters through a real TypeScript module and the public
//! `smudgy:core` surface. The input side uses `StyledLine` directly so every
//! assertion controls the terminal style at an exact UTF-8 byte boundary.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::vt_processor::{AnsiColor, Color, VtProcessor};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::{
    Blink, Style, StyledLine, TextAttributes, Underline, VtSpan,
};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};
use tokio::sync::mpsc::unbounded_channel;
use vtparse::VTParser;

const QUIET_PERIOD: Duration = Duration::from_millis(900);

const COLOR_TRIGGERS_TS: &str = r#"
import {
    command,
    createAlias,
    createTrigger,
    createTriggers,
    echo,
    link,
    line,
    pattern,
    style,
    userAutomations,
} from "smudgy:core";

const missed: string[] = [];

// The first text occurrence is intentionally allowed to fail its style
// predicate. Captures must come from the later occurrence that qualifies.
createTrigger(style.red(/hit (?<word>\w+)/), (matches) => {
    echo(`OCCURRENCE:${matches.word}:${matches[0]}`);
}, { name: "styled-occurrence" });

createTrigger(
    style.fg.range({ r: 255, g: 0, b: 0 }, { r: 255, g: 255, b: 0 })(/^RANGE$/),
    () => echo("RANGE_OK"),
    { name: "hsv-range" },
);

// A constrained bare builder is the color-only spelling. Capture zero is the
// empty string at the qualifying span start, including an empty line's final
// cursor style.
createTrigger(style.italic, (matches) => {
    echo(`COLOR_ONLY:${JSON.stringify(line.text)}:${matches[0] === ""}:${matches[1] === undefined}`);
}, { name: "color-only" });
createTrigger(style.crossedOut, "EMPTY_BODY <$0>", {
    name: "color-only-plaintext",
});

// A plaintext trigger body still takes the same style metadata path. Angle
// brackets make the expansion boundaries visible in the disconnected session.
createTrigger(style.yellow(/^PLAIN (\w+)$/), "PLAIN_BODY <$1>", {
    name: "styled-plaintext-body",
});

createTrigger({
    patterns: [style.green(/^ANTI/)],
    antiPatterns: [style.red(/blocked/)],
}, () => echo("ANTI_OK"), { name: "styled-anti" });

// Styled anti-patterns inspect displayed spans even when a raw positive was
// responsible for the candidate. Plain anti-patterns remain pass-relative.
createTrigger({
    rawPatterns: ["RAW-STYLED"],
    antiPatterns: [style.red(/blocked/)],
}, () => echo("RAW_STYLED_ANTI_OK"), { name: "raw-with-styled-anti" });
createTrigger({
    rawPatterns: ["RAW-PLAIN"],
    antiPatterns: ["\\x1b\\[31m"],
}, () => echo("RAW_PLAIN_ANTI_BAD"), { name: "raw-with-plain-anti" });

createTriggers({
    batchBackground: {
        patterns: [style.bg({ r: 12, g: 34, b: 56 })(/^BATCH$/)],
        script: () => echo("BATCH_OK"),
    },
});

createTrigger(style.magenta(pattern`TAG {value}`), (matches) => {
    echo(`TAG:${matches.value}`);
}, { name: "styled-pattern-tag" });
createTrigger(style.blue("^STRING (\\w+)$"), (matches) => {
    echo(`STRING:${matches[1]}`);
}, { name: "styled-string-pattern" });
createTrigger(style.red(/^UTF8 café$/), () => echo("FRAGMENTED_VT_OK"), {
    name: "fragmented-vt-style",
});
createTrigger(style.fg({ r: 255, g: 0, b: 0 })(/^XTERM$/), () => echo("XTERM_RGB_OK"), {
    name: "xterm-rgb-identity",
});

// An unconstrained decoration is a composition no-op and retains ordinary
// text-trigger behavior and identity.
createTrigger(style(/^NOOP$/), () => echo("NOOP_OK"), { name: "noop-style" });

// Empty unstyled patterns retain their historical inert registration
// behavior. The exact empty string becomes color-only only when it carries a
// surviving style predicate.
createTrigger("", () => echo("EMPTY_PLAIN_BAD"), { name: "empty-plain-inert" });
createTrigger({ rawPatterns: [""] }, () => echo("EMPTY_RAW_BAD"), {
    name: "empty-raw-inert",
});
createTrigger(style.red(""), () => echo("EMPTY_STYLED_OK"), {
    name: "empty-styled-color-only",
});
createTrigger(style.red(new RegExp("")), () => echo("ZERO_WIDTH_OK"), {
    name: "decorated-zero-width",
});

// Duplicate sources remain distinct positional leaves, and an unfiltered
// alternative can still win after an earlier styled duplicate rejects.
createTrigger({
    patterns: [style.red(/^DUP$/), style.blue(/^DUP$/)],
}, () => echo("DUPLICATE_OK"), { name: "duplicate-styled-sources" });
createTrigger({
    patterns: [style.red(/^MIX$/), /^MIX$/],
}, () => echo("MIXED_OK"), { name: "mixed-plain-styled" });

// A raw-pass veto does not mark the trigger fired. The displayed-text pass can
// subsequently qualify when the pass-relative plain anti no longer matches.
createTrigger({
    patterns: [/^RAW-NORMAL$/],
    rawPatterns: ["RAW-NORMAL"],
    antiPatterns: ["\\x1b\\[31m"],
}, () => echo("RAW_VETO_NORMAL_OK"), { name: "raw-veto-normal-pass" });

createTrigger(style.green(/^LIMIT$/), () => echo("LIMIT_OK"), {
    name: "styled-fire-limit",
    fireLimit: 1,
});
const deleted = createTrigger(style.red(/^DELETED$/), () => echo("DELETED_BAD"), {
    name: "styled-delete",
});
deleted.delete();
createTrigger(style.red(/^REPLACED$/), () => echo("REPLACED_OLD_BAD"), {
    name: "styled-replacement",
});
createTrigger(style.blue(/^REPLACED$/), () => echo("REPLACED_NEW_OK"), {
    name: "styled-replacement",
});

createTrigger(style({ attributes: {
    bold: true,
    faint: true,
    italic: true,
    underline: "double",
    blink: "slow",
    crossedOut: true,
    reverse: true,
} })(/^ATTR_ALL$/), () => echo("ATTR_ALL_OK"), { name: "all-positive-attributes" });
createTrigger(style({ attributes: {
    underline: "single",
    blink: "fast",
} })(/^ATTR_FAST$/), () => echo("ATTR_FAST_OK"), { name: "remaining-attributes" });

// Derived identities are semantic and deterministic. Equivalent spellings
// and a range later replaced by an exact color resolve to the same name; a
// no-op decoration preserves the historical plain name.
const derivedPattern = /^NEVER_DERIVED_NAME$/;
const derivedRed = createTrigger(style.red(derivedPattern), () => {}).name;
const derivedPalette = createTrigger(
    style.fg({ color: "red", bold: true })(derivedPattern),
    () => {},
).name;
const derivedOverwrittenRange = createTrigger(
    style
        .fg.range({ r: 0, g: 0, b: 0 }, { r: 255, g: 255, b: 255 })
        .fg("red")(derivedPattern),
    () => {},
).name;
const derivedNoop = createTrigger(style(/^NEVER_NOOP_NAME$/), () => {}).name;
const derivedPlain = createTrigger(/^NEVER_NOOP_NAME$/, () => {}).name;
const derivedAnti = createTrigger({
    patterns: ["UNLIKELY_A|UNLIKELY_B"],
    antiPatterns: [style.bg({ r: 1, g: 2, b: 3 })(/x;y/)],
}, () => {}).name;
echo(`DERIVED_EQ:${derivedRed === derivedPalette && derivedRed === derivedOverwrittenRange}`);
echo(`DERIVED_RED:${derivedRed}`);
echo(`DERIVED_NOOP:${derivedNoop === derivedPlain}:${derivedPlain}`);
echo(`DERIVED_ANTI:${derivedAnti}`);

const flagged = /^flag (?<value>.+)$/gis;
flagged.lastIndex = 7;
const flaggedTrigger = createTrigger(style.red(flagged), (matches) => {
    echo(`FLAGS_OK:${matches.value}`);
});
echo(`FLAGS_NAME:${flaggedTrigger.name}`);
echo(`FLAGS_LAST_INDEX:${flagged.lastIndex}`);

const reusableMatch = style.blue(/^REUSE$/);
const reusableSnapshot = (reusableMatch as any)[Symbol.for("smudgy.styleMatch")];
if (!Object.isFrozen(reusableMatch) || !Object.isFrozen(reusableSnapshot)
    || !Object.isFrozen(reusableSnapshot.style)) {
    missed.push("style-match-not-deeply-frozen");
}
if (style.red !== style.red || style.red.bold !== style.red.bold) {
    missed.push("style-builder-memoization");
}
createTrigger(reusableMatch, () => echo("REUSE_ONE"), { name: "reuse-one" });
createTrigger(reusableMatch, () => echo("REUSE_TWO"), { name: "reuse-two" });

// This prompt must work on the very first partial line after registration;
// there is no completed-line priming event before it.
createTrigger(style.cyan(/^PROMPT>$/), () => echo("PROMPT_OK"), {
    name: "first-styled-prompt",
    prompt: true,
});
createTrigger(style.red, () => echo("EMPTY_PROMPT_OK"), {
    name: "empty-styled-prompt",
    prompt: true,
});

createTrigger(/^DONE$/, () => echo("COLOR_E2E_DONE"), { name: "done" });

const mustReject = (name: string, f: () => unknown, ...expected: string[]) => {
    try {
        f();
        missed.push(name);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        for (const fragment of expected) {
            if (!message.includes(fragment)) {
                missed.push(`${name}-message(${fragment}):${message}`);
            }
        }
    }
};
const forgeStyleMatch = (source: string, styleWire: unknown) => {
    const forged: Record<PropertyKey, unknown> = {};
    Object.defineProperty(forged, "__smudgyStyleMatch", { value: true });
    Object.defineProperty(forged, Symbol.for("smudgy.styleMatch"), {
        value: { source, style: styleWire, summary: "forged" },
    });
    return forged;
};

createTrigger(/^SINGLETON$/, () => echo("SINGLETON_ORIGINAL"), {
    name: "singleton-preserved",
    singleton: true,
});
try {
    const duplicate = createTrigger(/(?=x)x/, () => echo("SINGLETON_REPLACED"), {
        name: "singleton-preserved",
        singleton: true,
    });
    if (duplicate.created !== false) missed.push("singleton-invalid-skip");
} catch {
    missed.push("singleton-invalid-skip");
}

createTrigger(/^REPLACEMENT$/, () => echo("REPLACEMENT_ORIGINAL"), {
    name: "replacement-preserved",
});
mustReject("invalid-replacement", () => createTrigger(
    /(?=x)x/,
    () => echo("REPLACEMENT_BAD"),
    { name: "replacement-preserved" },
), "invalid regex");

mustReject("theme-role", () => style.echo(/x/), "output theme role");
mustReject("nested-style", () => style.red(style.blue(/x/) as any), "already style-qualified");
mustReject(
    "whole-pattern-set",
    () => style.red({ patterns: [/x/] } as any),
    "individual trigger leaves",
);
mustReject("styled-raw", () => createTrigger(
    { rawPatterns: [style.red(/x/) as any] },
    () => {},
    { name: "invalid-styled-raw" },
), "rawPatterns[0]", "cannot be styled");
mustReject("unconstrained-bare", () => createTrigger(style as any, () => {}, {
    name: "invalid-unconstrained",
}), "patterns[0]", "unconstrained style chain");
mustReject("sparse-patterns", () => {
    const patterns: any[] = [];
    patterns.length = 1;
    createTrigger({ patterns }, () => {}, { name: "invalid-sparse" });
}, "patterns[0]", "sparse hole");
mustReject("bare-pattern-array", () => createTrigger([/x/] as any, () => {}, {
    name: "invalid-bare-array",
}), "bare trigger pattern arrays");
mustReject("null-pattern-leaf", () => createTrigger(
    { patterns: [null as any] },
    () => {},
    { name: "invalid-null-leaf" },
), "patterns[0]", "received null");
mustReject("anti-only", () => createTrigger(
    { antiPatterns: [style.red(/x/)] },
    () => {},
    { name: "invalid-anti-only" },
), "At least one pattern");
mustReject("engine-regex", () => createTrigger(/(?=x)x/, () => {}, {
    name: "invalid-rust-regex",
}), "invalid regex");
mustReject("forged-empty-style", () => createTrigger(
    forgeStyleMatch("", {}) as any,
    () => {},
    { name: "invalid-empty-wire" },
), "normal[0].style", "must constrain");
mustReject("forged-nonempty-empty-style", () => createTrigger(
    forgeStyleMatch("x", {}) as any,
    () => {},
    { name: "invalid-nonempty-empty-wire" },
), "normal[0].style", "must constrain");
mustReject("forged-ansi-index", () => createTrigger(
    forgeStyleMatch("x", { foreground: { kind: "ansi", index: 16 } }) as any,
    () => {},
    { name: "invalid-ansi-wire" },
), "between 0 and 15");
const matcherOnly = style
    .fg.range({ r: 255, g: 0, b: 0 }, { r: 255, g: 255, b: 0 })
    .fg("red");
mustReject("range-render", () => matcherOnly`not renderable`, "trigger-only");
mustReject("range-echo", () => echo(matcherOnly as any), "trigger-only");
mustReject("range-interpolation", () => echo`${matcherOnly as any}`, "trigger-only");
mustReject(
    "range-line-options",
    () => line.insert("x", 0, 0, matcherOnly as any),
    "trigger-only",
);
mustReject(
    "negative-attribute",
    () => style({ attributes: { bold: false } })(/x/),
    "attributes.bold",
);
mustReject("invalid-range-endpoint", () => (style.fg.range as any)("red", {
    r: 255,
    g: 0,
    b: 0,
}), "range() from");
mustReject("invalid-range-arity", () => (style.fg.range as any)(
    { r: 255, g: 0, b: 0 },
    { r: 255, g: 255, b: 0 },
    { r: 0, g: 0, b: 0 },
), "exactly two");
mustReject("scalar-patterns", () => createTrigger(
    { patterns: /x/ as any },
    () => {},
    { name: "invalid-scalar-patterns" },
), "patterns must be an array");
mustReject("boxed-pattern", () => createTrigger(
    { patterns: [new String("x") as any] },
    () => {},
    { name: "invalid-boxed-pattern" },
), "patterns[0]");
mustReject("function-pattern", () => createTrigger(
    { patterns: [(() => {}) as any] },
    () => {},
    { name: "invalid-function-pattern" },
), "patterns[0]", "received function");
mustReject("undefined-pattern", () => createTrigger(
    { patterns: [undefined as any] },
    () => {},
    { name: "invalid-undefined-pattern" },
), "patterns[0]", "received undefined");
mustReject("command-as-style-pattern", () => style.red(command`look` as any), "alias matchers");
mustReject("link-as-style-pattern", () => style.red(link("look") as any), "link tag");
mustReject("styled-text-as-pattern", () => style.red(style.blue`text` as any), "styled output");
mustReject("styled-alias-array", () => createAlias(
    [/x/, style.red(/y/) as any],
    () => {},
), "patterns[1]", "style-qualified");
mustReject("saved-style", () => userAutomations.triggers.save("invalid-styled-save", {
    patterns: [style.red(/x/) as any],
    script: "look",
}), "patterns[0]", "cannot persist");

echo(missed.length === 0 ? "VALIDATION_OK" : `VALIDATION_MISSED:${missed.join(",")}`);
echo("COLOR_E2E_READY");
"#;

fn ansi(color: AnsiColor, bright: bool) -> Style {
    Style {
        fg: Color::Ansi {
            color,
            bold: bright,
        },
        ..Style::DEFAULT
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Style {
    Style {
        fg: Color::Rgb { r, g, b },
        ..Style::DEFAULT
    }
}

fn rgb_background(r: u8, g: u8, b: u8) -> Style {
    Style {
        bg: Color::Rgb { r, g, b },
        ..Style::DEFAULT
    }
}

fn italic() -> Style {
    Style {
        attributes: TextAttributes {
            italic: true,
            ..TextAttributes::DEFAULT
        },
        ..Style::DEFAULT
    }
}

fn crossed_out() -> Style {
    Style {
        attributes: TextAttributes {
            crossed_out: true,
            ..TextAttributes::DEFAULT
        },
        ..Style::DEFAULT
    }
}

fn all_attributes() -> Style {
    Style {
        attributes: TextAttributes {
            bold: true,
            faint: true,
            italic: true,
            underline: Underline::Double,
            blink: Blink::Slow,
            crossed_out: true,
            reverse: true,
        },
        ..Style::DEFAULT
    }
}

fn fast_blink_single_underline() -> Style {
    Style {
        attributes: TextAttributes {
            underline: Underline::Single,
            blink: Blink::Fast,
            ..TextAttributes::DEFAULT
        },
        ..Style::DEFAULT
    }
}

fn styled(parts: &[(&str, Style)]) -> Arc<StyledLine> {
    let mut text = String::new();
    let mut spans = Vec::with_capacity(parts.len());
    for (part, style) in parts {
        let begin_pos = text.len();
        text.push_str(part);
        spans.push(VtSpan {
            style: *style,
            begin_pos,
            end_pos: text.len(),
        });
    }
    Arc::new(StyledLine::new(&text, spans))
}

fn styled_with_raw(parts: &[(&str, Style)], raw: &[u8]) -> Arc<StyledLine> {
    let mut text = String::new();
    let mut spans = Vec::with_capacity(parts.len());
    for (part, style) in parts {
        let begin_pos = text.len();
        text.push_str(part);
        spans.push(VtSpan {
            style: *style,
            begin_pos,
            end_pos: text.len(),
        });
    }
    Arc::new(StyledLine::new_with_raw(&text, spans, Some(raw)))
}

fn cursor_styled(style: Style) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(
        "",
        vec![VtSpan {
            style,
            begin_pos: 0,
            end_pos: 0,
        }],
    ))
}

/// Feed packet-sized fragments through the same persistent VT parser state a
/// connection uses. The cuts deliberately bisect both an SGR sequence and the
/// two-byte UTF-8 encoding of `é`.
fn vt_fragmented_line(chunks: &[&[u8]]) -> Arc<StyledLine> {
    let (tx, mut rx) = unbounded_channel();
    let mut parser = VTParser::new();
    let mut processor = VtProcessor::new(tx);
    for chunk in chunks {
        for &byte in *chunk {
            if byte != b'\r' && byte != b'\n' {
                processor.push_raw_incoming_byte(byte);
            }
            parser.parse_byte(byte, &mut processor);
        }
        processor.notify_end_of_buffer();
    }
    let mut completed = None;
    while let Ok(action) = rx.try_recv() {
        match action {
            RuntimeAction::HandleIncomingLine(line)
            | RuntimeAction::HandleIncomingFragmentedLine { line, .. } => completed = Some(line),
            _ => {}
        }
    }
    completed.expect("fragmented VT stream must commit one line")
}

fn vt_empty_styled_prompt(sgr: &[u8]) -> Arc<StyledLine> {
    let (tx, mut rx) = unbounded_channel();
    let mut parser = VTParser::new();
    let mut processor = VtProcessor::new(tx);
    for &byte in sgr {
        processor.push_raw_incoming_byte(byte);
        parser.parse_byte(byte, &mut processor);
    }
    processor.commit_prompt();
    while let Ok(action) = rx.try_recv() {
        if let RuntimeAction::HandleIncomingPartialLine(line) = action {
            return line;
        }
    }
    panic!("SGR-only prompt boundary must emit an empty styled partial")
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn public_script_surface_matches_terminal_styles_end_to_end() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "ColorTriggerScripting";
    let modules = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules.join("color_triggers.ts"), COLOR_TRIGGERS_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7133),
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
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };

    while !lines.iter().any(|line| line == "COLOR_E2E_READY") {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for COLOR_E2E_READY; lines={lines:?}"))
            .expect("event stream ended before COLOR_E2E_READY");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }

    let default = Style::DEFAULT;
    let dim_red = ansi(AnsiColor::Red, false);
    let bright_red = ansi(AnsiColor::Red, true);
    let bright_yellow = ansi(AnsiColor::Yellow, true);
    let bright_green = ansi(AnsiColor::Green, true);
    let bright_blue = ansi(AnsiColor::Blue, true);
    let bright_magenta = ansi(AnsiColor::Magenta, true);
    let bright_cyan = ansi(AnsiColor::Cyan, true);

    // Intentionally precedes every completed line: the prompt PatternSets are
    // still dirty from registration and must rebuild on this path themselves.
    tx.send(RuntimeAction::HandleIncomingPartialLine(styled(&[(
        "PROMPT>",
        bright_cyan,
    )])))
    .unwrap();
    tx.send(RuntimeAction::RetractIncomingPartialLine).unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(
        vt_empty_styled_prompt(b"\x1b[91m"),
    ))
    .unwrap();
    tx.send(RuntimeAction::RetractIncomingPartialLine).unwrap();

    // Start-byte semantics: red inside the match is insufficient. On the next
    // line, the first occurrence is dim red and the second is bright red, so
    // captures must be from "second".
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[
        ("hit ", default),
        ("interior", bright_red),
    ])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[
        ("hit first ", dim_red),
        ("hit second", bright_red),
    ])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "RANGE",
        rgb(255, 128, 0),
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "ITALIC",
        italic(),
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(cursor_styled(italic())))
        .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "CROSSED",
        crossed_out(),
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "PLAIN word",
        bright_yellow,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[
        ("ANTI ", bright_green),
        ("blocked", bright_red),
    ])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "ANTI allowed",
        bright_green,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled_with_raw(
        &[("RAW-STYLED blocked", bright_red)],
        b"\x1b[31mRAW-STYLED blocked\x1b[0m",
    )))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled_with_raw(
        &[("RAW-STYLED allowed", default)],
        b"RAW-STYLED allowed",
    )))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled_with_raw(
        &[("RAW-PLAIN", bright_red)],
        b"\x1b[31mRAW-PLAIN\x1b[0m",
    )))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "BATCH",
        rgb_background(12, 34, 56),
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "TAG value",
        bright_magenta,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "STRING value",
        bright_blue,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(vt_fragmented_line(&[
        &b"\x1b[9"[..],
        &b"1mUTF8 caf\xc3"[..],
        &b"\xa9\x1b[0m\r\n"[..],
    ])))
    .unwrap();
    // Xterm slot 9 remains an ANSI identity even when a palette renders it as
    // red; slot 196 is stored as concrete RGB and qualifies the exact matcher.
    tx.send(RuntimeAction::HandleIncomingLine(vt_fragmented_line(&[
        &b"\x1b[38;5;9mXTERM\x1b[0m\r\n"[..],
    ])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(vt_fragmented_line(&[
        &b"\x1b[38;5;196mXTERM\x1b[0m\r\n"[..],
    ])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "NOOP", default,
    )])))
    .unwrap();
    // The first UTF-8 character is unstyled; the later byte boundary starts a
    // bright-red span. Both the true color-only sentinel and a decorated
    // zero-width RegExp must select that later span without splitting `é`.
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[
        ("é", default),
        ("Z", bright_red),
    ])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "DUP",
        bright_blue,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "MIX", default,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled_with_raw(
        &[("RAW-NORMAL", default)],
        b"\x1b[31mRAW-NORMAL\x1b[0m",
    )))
    .unwrap();
    for _ in 0..2 {
        tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
            "LIMIT",
            bright_green,
        )])))
        .unwrap();
    }
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "DELETED", bright_red,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "REPLACED",
        bright_blue,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "ATTR_ALL",
        all_attributes(),
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "ATTR_FAST",
        fast_blink_single_underline(),
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "FLAG flag-value",
        bright_red,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "REUSE",
        bright_blue,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "SINGLETON",
        default,
    )])))
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "REPLACEMENT",
        default,
    )])))
    .unwrap();

    tx.send(RuntimeAction::HandleIncomingLine(styled(&[(
        "DONE", default,
    )])))
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    while !lines.iter().any(|line| line == "COLOR_E2E_DONE") {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for COLOR_E2E_DONE; lines={lines:?}"))
            .expect("event stream ended before COLOR_E2E_DONE");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }
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
    let count = |needle: &str| lines.iter().filter(|line| *line == needle).count();
    let has = |needle: &str| lines.iter().any(|line| line == needle);

    assert!(
        has("VALIDATION_OK"),
        "validation contract failed\n{transcript}"
    );
    assert_eq!(count("OCCURRENCE:second:hit second"), 1, "{transcript}");
    assert!(has("RANGE_OK"), "{transcript}");
    assert!(has("COLOR_ONLY:\"ITALIC\":true:true"), "{transcript}");
    assert!(has("COLOR_ONLY:\"\":true:true"), "{transcript}");
    assert!(
        has("EMPTY_BODY <>"),
        "color-only $0 was not empty\n{transcript}"
    );
    assert!(has("PLAIN_BODY <word>"), "{transcript}");
    assert_eq!(
        count("ANTI_OK"),
        1,
        "styled anti must veto one line\n{transcript}"
    );
    assert_eq!(
        count("RAW_STYLED_ANTI_OK"),
        1,
        "styled anti must inspect displayed spans on a raw-positive pass\n{transcript}"
    );
    assert!(
        !has("RAW_PLAIN_ANTI_BAD"),
        "plain anti must inspect the raw subject during the raw pass\n{transcript}"
    );
    assert!(has("BATCH_OK"), "{transcript}");
    assert!(has("TAG:value"), "{transcript}");
    assert!(has("STRING:value"), "{transcript}");
    assert!(has("FRAGMENTED_VT_OK"), "{transcript}");
    assert_eq!(count("XTERM_RGB_OK"), 1, "{transcript}");
    assert!(has("NOOP_OK"), "{transcript}");
    assert!(!has("EMPTY_PLAIN_BAD"), "{transcript}");
    assert!(!has("EMPTY_RAW_BAD"), "{transcript}");
    assert!(has("EMPTY_STYLED_OK"), "{transcript}");
    assert!(has("ZERO_WIDTH_OK"), "{transcript}");
    assert!(has("DUPLICATE_OK"), "{transcript}");
    assert!(has("MIXED_OK"), "{transcript}");
    assert_eq!(count("RAW_VETO_NORMAL_OK"), 1, "{transcript}");
    assert_eq!(count("LIMIT_OK"), 1, "{transcript}");
    assert!(!has("DELETED_BAD"), "{transcript}");
    assert!(!has("REPLACED_OLD_BAD"), "{transcript}");
    assert_eq!(count("REPLACED_NEW_OK"), 1, "{transcript}");
    assert!(has("ATTR_ALL_OK"), "{transcript}");
    assert!(has("ATTR_FAST_OK"), "{transcript}");
    assert!(has("DERIVED_EQ:true"), "{transcript}");
    assert!(
        has("DERIVED_RED:^NEVER_DERIVED_NAME$ [fg=ansi:bright-red]"),
        "{transcript}"
    );
    assert!(has("DERIVED_NOOP:true:^NEVER_NOOP_NAME$"), "{transcript}");
    assert!(
        has(r"DERIVED_ANTI:UNLIKELY_A\|UNLIKELY_B ; except x\;y [bg=rgb:#010203]"),
        "{transcript}"
    );
    assert!(
        has(r"FLAGS_NAME:(?is:^flag (?\<value\>.+)$) [fg=ansi:bright-red]"),
        "{transcript}"
    );
    assert!(has("FLAGS_LAST_INDEX:7"), "{transcript}");
    assert!(has("FLAGS_OK:flag-value"), "{transcript}");
    assert_eq!(count("REUSE_ONE"), 1, "{transcript}");
    assert_eq!(count("REUSE_TWO"), 1, "{transcript}");
    assert!(has("SINGLETON_ORIGINAL"), "{transcript}");
    assert!(!has("SINGLETON_REPLACED"), "{transcript}");
    assert!(has("REPLACEMENT_ORIGINAL"), "{transcript}");
    assert!(!has("REPLACEMENT_BAD"), "{transcript}");
    assert!(
        has("PROMPT_OK"),
        "first styled prompt was missed\n{transcript}"
    );
    assert!(
        has("EMPTY_PROMPT_OK"),
        "empty styled prompt was missed\n{transcript}"
    );
}
