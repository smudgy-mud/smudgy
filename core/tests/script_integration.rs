//! End-to-end integration: a real session loads an Arctic-style module via the
//! `smudgy_script` runtime and exercises the genuine smudgy scripting surface —
//! module transpilation + auto-load, `node:events`, `node:crypto`
//! (`createHash('sha3-512')`, as Arctic's mapper/hash.ts does), `localStorage`,
//! and a JS-function alias calling `send()`.
//!
//! Unlike the `smudgy_script` crate tests (which exercise the raw runtime), this
//! runs through `ScriptEngine` with the real session ops, covering the smudgy-domain
//! integration end to end.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::runtime::input::{InputSnapshot, InputSource};
use smudgy_core::session::runtime::pane::MAIN_PANE_KEY;
use smudgy_core::session::{BufferUpdate, HotkeyId, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// An Arctic-style module: top-level checks emit a single sentinel, plus a
/// function-alias whose body calls `send()`. `digest` is the sha3-512/base64url
/// of "abc" (the algorithm Arctic's mapper/hash.ts uses).
const HARNESS_TS: &str = r#"
import { EventEmitter } from "node:events";
import { createHash } from "node:crypto";
// The convenience surface is not ambient in modules (globalThis is minimal). A
// module imports what it uses from smudgy:core: mapper values and
// `createAlias`/`echo`/`send` as named exports, and the current-session facade as
// the default export (`session`) for live accessors like `reload`/`vars`.
import session, { Area, createAlias, echo, mapper, send, vars } from "smudgy:core";

// A JS-function alias that calls reload() exercises the
// `op_smudgy_session_reload` op (own-session route). Reloading rebuilds the
// engine, which re-runs this module top-level and re-emits HARNESS_OK.

let evVal = 0;
const ee = new EventEmitter();
ee.on("e", (n) => { evVal = n; });
ee.emit("e", 7);

const digest = createHash("sha3-512").update("abc").digest("base64url");

localStorage.setItem("harness_key", "persisted");
const ls = localStorage.getItem("harness_key");

// `vars` (imported from smudgy:core, not a global) round-trips through its
// localStorage-backed store, including a deep write-back via the persist proxy.
vars.harness = { nested: { n: 1 } };
vars.harness.nested.n = 9;
const varsOk = vars.harness.nested.n === 9 && globalThis.vars === undefined;

const ok =
    evVal === 7 &&
    digest === "t1GFCxpXFopWk82SS2sJbgj2IYJ0RPcNiE9dAkDScS4Q4RbpGSrzyRp-xXZH45NAVzQLTPQI1aVlkvgnTuxT8A" &&
    ls === "persisted" &&
    varsOk;

echo(ok ? "HARNESS_OK" : ("HARNESS_FAIL ev=" + evVal + " ls=" + ls + " digest=" + digest + " vars=" + varsOk));

const mapperOpResult =
    mapper.listRoomsByTitleDescriptionAndVisibleExits("No title", "No description", []);
const mapperOpOk = Array.isArray(mapperOpResult) && mapperOpResult.length === 0;
echo(mapperOpOk ? "MAPPER_OP_OK" : "MAPPER_OP_FAIL");

const mapperGlobalsGone =
    !("mapper" in globalThis) &&
    !("Area" in globalThis) &&
    !("__smudgy_install_mapper" in globalThis) &&
    typeof Area === "function";
echo(mapperGlobalsGone ? "MAPPER_GLOBALS_GONE" : "MAPPER_GLOBALS_LEAKED");

createAlias("^greet$", () => { send("hello world"); });
createAlias("^dorel$", () => { session.reload(); });
"#;

/// A module registering an alias whose handler inspects the numeric/named `matches`
/// object. The pattern has an unnamed group ($1) and a named group (`who`). The handler
/// asserts `matches[0]` (whole match), `matches[1]` (group one), `matches.who` (named
/// group), and that the legacy `matches["$1"]` string key is gone (undefined). It echoes
/// a single sentinel encoding the result so the test can assert on it.
///
/// A second alias proves that named groups named after `Object.prototype` members
/// (`length`, `toString`, …) still read back as their captures: the object carries the
/// normal prototype, but the groups are own data properties, so they shadow the inherited
/// members for reads. The object is a plain record (`Object.getPrototypeOf(m) === Object.prototype`).
const CAPTURES_TS: &str = r#"
import { createAlias, echo } from "smudgy:core";

createAlias("^cap (\\w+) (?<who>\\w+)$", (m) => {
    const ok =
        m[0] === "cap one two" &&
        m[1] === "one" &&
        m.who === "two" &&
        m["$1"] === undefined &&
        m["$0"] === undefined;
    echo(ok
        ? "CAPTURES_OK"
        : ("CAPTURES_FAIL 0=" + m[0] + " 1=" + m[1] + " who=" + m.who + " $1=" + m["$1"]));
});

createAlias("^collide (?<length>\\w+) (?<toString>\\w+)$", (m) => {
    const ok =
        Object.getPrototypeOf(m) === Object.prototype &&
        typeof m.hasOwnProperty === "function" &&
        m.length === "a" &&
        m.toString === "b" &&
        m[1] === "a" &&
        m[2] === "b";
    echo(ok
        ? "COLLIDE_OK"
        : ("COLLIDE_FAIL proto=" + Object.getPrototypeOf(m) + " length=" + m.length + " toString=" + m.toString));
});
"#;

/// Drive `CAPTURES_TS` and assert the alias handler saw the numeric/named
/// `matches` object (and that `matches["$1"]` is `undefined`).
#[tokio::test]
async fn capture_matches_object_is_numeric_and_named() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    // The smudgy home override is a process-global `OnceLock` (first setter in the binary
    // wins), so re-read it after setting and scope everything under a unique server name.
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "Captures";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("captures.ts"), CAPTURES_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7003),
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
                    // The module registers the alias at top-level; once the runtime is
                    // quiescent enough to have emitted any line, send the matching input.
                    if !sent {
                        tx.send(RuntimeAction::Send(Arc::new("cap one two".to_string())))
                            .unwrap();
                        tx.send(RuntimeAction::Send(Arc::new("collide a b".to_string())))
                            .unwrap();
                        sent = true;
                    }
                }
            }
        }
    }

    // If nothing was emitted at load, the loop above never sends. Send once more to be safe
    // and drain a little longer.
    if !sent {
        tx.send(RuntimeAction::Send(Arc::new("cap one two".to_string())))
            .unwrap();
        tx.send(RuntimeAction::Send(Arc::new("collide a b".to_string())))
            .unwrap();
        while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
            if let SessionEvent::UpdateBuffer(updates) = event.event {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "CAPTURES_OK"),
        "alias handler must receive a numeric/named matches object with no legacy \"$1\" key.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "COLLIDE_OK"),
        "named groups (`length`, `toString`, …) must read back as their captures via own data properties.\nTranscript:\n{transcript}"
    );
}

/// A module registering aliases that exercise `RegExp` flag translation: `/…/i` must
/// match case-insensitively (the flag is baked into the engine-facing source as a
/// `(?i:…)` wrapper), the wrapper must not disturb capture-group numbering, and a
/// flagless pattern must stay case-sensitive.
const FLAGS_TS: &str = r#"
import { createAlias, echo } from "smudgy:core";

createAlias(/^flagged (\w+) (?<who>\w+)$/i, (m) => {
    const ok = m[0] === "FLAGGED One Two" && m[1] === "One" && m.who === "Two";
    echo(ok
        ? "FLAGS_OK"
        : ("FLAGS_FAIL 0=" + m[0] + " 1=" + m[1] + " who=" + m.who));
});

createAlias("^plain (\\w+)$", () => { echo("PLAIN_FIRED"); });
"#;

/// Drive `FLAGS_TS`: an `/…/i` alias fires on differently-cased input with intact
/// group numbering, while a flagless pattern stays case-sensitive.
#[tokio::test]
async fn regexp_flags_are_honored() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "Flags";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("flags.ts"), FLAGS_TS).unwrap();

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

    tx.send(RuntimeAction::Send(Arc::new("FLAGGED One Two".to_string())))
        .unwrap();
    tx.send(RuntimeAction::Send(Arc::new("PLAIN x".to_string())))
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
        lines.iter().any(|l| l == "FLAGS_OK"),
        "an /i alias must fire on differently-cased input with intact group numbering.\nTranscript:\n{transcript}"
    );
    assert!(
        !lines.iter().any(|l| l == "PLAIN_FIRED"),
        "a flagless pattern must stay case-sensitive.\nTranscript:\n{transcript}"
    );
}

/// A module exercising the `pattern` tagged template end to end: anchor
/// forms, escaped interpolation, raw-string islands, the display side
/// properties, compile-error throwing, and a lockstep probe that echoes the
/// JS compiler's output for shapes the Rust compiler
/// (`models::matchers::compile_pattern`) owns.
const PATTERN_TAG_TS: &str = r#"
import { createAlias, echo, pattern } from "smudgy:core";

createAlias(pattern`greet {person}`, (m) => { echo("TAG_GREET who=" + m.person); });
createAlias(pattern.contains`buy`, () => { echo("TAG_CONTAINS"); });

// Interpolation is escaped: metacharacters in the value are literal text.
const target = "a.b*c";
createAlias(pattern`kill ${target}`, () => { echo("TAG_ESCAPED"); });

// Raw strings: an island's backslash survives to the engine.
createAlias(pattern`lvl /\d+/`, () => { echo("TAG_ISLAND"); });

const p = pattern`greet {person}`;
echo("TAG_PROPS anchors=" + (p as any).patternAnchors + " src=" + (p as any).patternSource
    + " enum=" + JSON.stringify(Object.keys(p)));

try {
    pattern`hit {1}`;
    echo("TAG_ERR_MISSED");
} catch (e) {
    echo("TAG_ERR " + String((e as Error).message).startsWith("Write {} instead of {1}."));
}

const shapes: RegExp[] = [
    pattern`greet {person}`,
    pattern`greet {person} warmly`,
    pattern`greet {person?}`,
    pattern`hit {} for {}`,
    pattern`you gain {n:number} exp`,
    pattern`/\s*/{person} bows`,
    pattern`greet {not a name}`,
    pattern`you are *`,
    pattern.startsWith`You are`,
    pattern.endsWith`is hungry.`,
    pattern.contains`is hun`,
];
shapes.forEach((s, i) => echo("TAGSRC " + i + " " + s.source));
"#;

/// Drive `PATTERN_TAG_TS`: tag-built aliases fire with the pattern grammar's
/// semantics, and the JS compiler's output is byte-identical to the Rust
/// compiler's for every probed shape.
#[tokio::test]
async fn pattern_tag_end_to_end() {
    use smudgy_core::models::matchers::compile_pattern;

    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "PatternTag";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("tag.ts"), PATTERN_TAG_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7017),
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

    for input in [
        "greet Mira Bob",
        "please buy now",
        "kill a.b*c",
        "kill aXbYc",
        "lvl 42",
        "lvl xx",
    ] {
        tx.send(RuntimeAction::Send(Arc::new(input.to_string())))
            .unwrap();
    }

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
    let has = |needle: &str| lines.iter().any(|l| l == needle);
    assert!(
        has("TAG_GREET who=Mira Bob"),
        "a tag alias fires with wildcard-hole semantics.\nTranscript:\n{transcript}"
    );
    assert!(
        has("TAG_CONTAINS"),
        "pattern.contains matches anywhere.\nTranscript:\n{transcript}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.as_str() == "TAG_ESCAPED").count(),
        1,
        "interpolation is literal text: `a.b*c` fires, `aXbYc` does not.\nTranscript:\n{transcript}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.as_str() == "TAG_ISLAND").count(),
        1,
        "an island's \\d survives the raw-string tag.\nTranscript:\n{transcript}"
    );
    assert!(
        has("TAG_PROPS anchors=both src=greet {person} enum=[]"),
        "side properties are present and non-enumerable.\nTranscript:\n{transcript}"
    );
    assert!(
        has("TAG_ERR true"),
        "a numbered hole throws the editor's message.\nTranscript:\n{transcript}"
    );

    // The two compilers must agree byte for byte.
    let expected = [
        compile_pattern("greet {person}", true, true),
        compile_pattern("greet {person} warmly", true, true),
        compile_pattern("greet {person?}", true, true),
        compile_pattern("hit {} for {}", true, true),
        compile_pattern("you gain {n:number} exp", true, true),
        compile_pattern(r"/\s*/{person} bows", true, true),
        compile_pattern("greet {not a name}", true, true),
        compile_pattern("you are *", true, true),
        compile_pattern("You are", true, false),
        compile_pattern("is hungry.", false, true),
        compile_pattern("is hun", false, false),
    ];
    for (i, compiled) in expected.iter().enumerate() {
        assert!(
            compiled.errors.is_empty(),
            "probe {i} failed to compile in Rust"
        );
        let needle = format!("TAGSRC {i} {}", compiled.source);
        assert!(
            has(&needle),
            "JS and Rust compilers disagree on probe {i}: wanted {needle:?}.\nTranscript:\n{transcript}"
        );
    }
}

/// A module exercising the `command` tagged template end to end: a function
/// body reading named arguments, a string body substituting them, an
/// interpolated command word, the parsed value's display surface, usage
/// probes for the Rust lockstep check, and the parse errors.
const COMMAND_TAG_TS: &str = r#"
import { aliases, command, createAlias, echo } from "smudgy:core";

createAlias(command`greet <person> [greeting] [words...]`, (m) => {
    echo("CMD_GREET person=" + m.person
        + " greeting=" + (m.greeting === "" ? "(none)" : m.greeting)
        + " words=" + (m.words === "" ? "(none)" : m.words)
        + " line=" + m[0]);
});

// A string body substitutes arguments by name.
createAlias(command`gt [words...]`, "guildtell $words");

// The word may be interpolated -- literal text, never syntax. By fire time
// registration has landed, so the handler can check the derived names: each
// command alias is named after its word.
const word = "hail";
createAlias(command`${word} <target>`, (m) => {
    const named = aliases.exists("hail") && aliases.exists("greet") && aliases.exists("gt");
    echo("CMD_HAIL target=" + m.target + " named=" + named);
});

// The parsed value's display surface. `cast` is never registered as an alias,
// so it must NOT tab-complete below.
const c = command`cast <spell> [target]`;
echo("CMD_PROPS word=" + c.word + " str=" + String(c) + " args=" + JSON.stringify(c.args));

// Usage probes for the Rust lockstep check.
[
    command`greet <person> [greeting] [words...]`,
    command`gt [words...]`,
    command`cast <spell> [target]`,
].forEach((probe, i) => echo("CMDUSAGE " + i + " " + probe.usage));

// Parse errors throw.
try { command`multi word <x>`; echo("CMD_ERR_BAREWORD missed"); }
catch { echo("CMD_ERR_BAREWORD ok"); }
try { command`foo [rest...] <after>`; echo("CMD_ERR_RESTLAST missed"); }
catch { echo("CMD_ERR_RESTLAST ok"); }
try { command`foo <${"sp ace"}>`; echo("CMD_ERR_SPLICEWS missed"); }
catch { echo("CMD_ERR_SPLICEWS ok"); }
// A splice ending in dots must not quietly become a rest argument.
try { command`foo [${"x..."}]`; echo("CMD_ERR_SPLICEDOTS missed"); }
catch { echo("CMD_ERR_SPLICEDOTS ok"); }
"#;

/// Drive `COMMAND_TAG_TS`: tag-built command aliases fire with the editor's
/// Command semantics (named captures, quote grouping, the usage echo on a
/// missing required argument), string bodies substitute by name, the words
/// join tab completion, and the JS tag's usage lines match the Rust
/// `usage_line` renderer shape for shape.
#[tokio::test]
async fn command_tag_end_to_end() {
    use smudgy_core::models::matchers::{ArgKind, ArgSpec, usage_line};

    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "CommandTag";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("cmd.ts"), COMMAND_TAG_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7018),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));

    // The completion push can land while the engine is still loading, ahead of
    // `RuntimeReady` on the stream, so the wait loop collects it too.
    let mut command_names: Vec<String> = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::CommandNames { names } => {
                command_names = names.iter().map(|n| n.as_str().to_string()).collect();
            }
            _ => {}
        }
    };

    for input in [
        "greet Mira",
        "greet Mira warmly and well",
        "greet \"big ugly troll\"",
        "greet",
        "gt hello there",
        "hail Bob",
    ] {
        tx.send(RuntimeAction::Send(Arc::new(input.to_string())))
            .unwrap();
    }

    let mut lines = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        match event.event {
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            SessionEvent::CommandNames { names } => {
                command_names = names.iter().map(|n| n.as_str().to_string()).collect();
            }
            _ => {}
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    let has = |needle: &str| lines.iter().any(|l| l == needle);
    assert!(
        has("CMD_GREET person=Mira greeting=(none) words=(none) line=greet Mira"),
        "required-only input fires with empty optionals.\nTranscript:\n{transcript}"
    );
    assert!(
        has("CMD_GREET person=Mira greeting=warmly words=and well line=greet Mira warmly and well"),
        "optional and rest arguments assign in order; rest keeps the raw remainder.\nTranscript:\n{transcript}"
    );
    assert!(
        has(
            "CMD_GREET person=big ugly troll greeting=(none) words=(none) line=greet \"big ugly troll\""
        ),
        "quotes group a multi-word argument.\nTranscript:\n{transcript}"
    );
    assert!(
        has("Usage: greet <person> [greeting] [words...]"),
        "a missing required argument echoes the usage line.\nTranscript:\n{transcript}"
    );
    assert!(
        has("guildtell hello there"),
        "a string body substitutes $words.\nTranscript:\n{transcript}"
    );
    assert!(
        has("CMD_HAIL target=Bob named=true"),
        "an interpolated word matches literally, and command aliases are named after their word.\nTranscript:\n{transcript}"
    );
    assert!(
        has(concat!(
            "CMD_PROPS word=cast str=cast <spell> [target] ",
            "args=[{\"name\":\"spell\",\"kind\":\"required\"},{\"name\":\"target\",\"kind\":\"optional\"}]"
        )),
        "the parsed value exposes word/usage/args.\nTranscript:\n{transcript}"
    );
    for sentinel in [
        "CMD_ERR_BAREWORD ok",
        "CMD_ERR_RESTLAST ok",
        "CMD_ERR_SPLICEWS ok",
        "CMD_ERR_SPLICEDOTS ok",
    ] {
        assert!(
            has(sentinel),
            "a malformed tag body must throw ({sentinel}).\nTranscript:\n{transcript}"
        );
    }

    // The registered words (and only those) tab-complete.
    assert!(
        command_names.iter().any(|n| n == "greet")
            && command_names.iter().any(|n| n == "gt")
            && command_names.iter().any(|n| n == "hail"),
        "tag-created command words join completion; got {command_names:?}"
    );
    assert!(
        !command_names.iter().any(|n| n == "cast"),
        "an unregistered command value must not complete; got {command_names:?}"
    );

    // The JS tag's usage lines and the Rust renderer must agree shape for shape.
    let spec = |name: &str, kind: ArgKind| ArgSpec {
        name: name.to_string(),
        kind,
    };
    let expected = [
        usage_line(
            "greet",
            &[
                spec("person", ArgKind::Required),
                spec("greeting", ArgKind::Optional),
                spec("words", ArgKind::Rest),
            ],
        ),
        usage_line("gt", &[spec("words", ArgKind::Rest)]),
        usage_line(
            "cast",
            &[
                spec("spell", ArgKind::Required),
                spec("target", ArgKind::Optional),
            ],
        ),
    ];
    for (i, usage) in expected.iter().enumerate() {
        let needle = format!("CMDUSAGE {i} {usage}");
        assert!(
            has(&needle),
            "JS and Rust disagree on usage probe {i}: wanted {needle:?}.\nTranscript:\n{transcript}"
        );
    }
}

/// One alias, two patterns. Sending `first x` fires pattern one, where `a`
/// participates, `opt` (an optional group of the fired pattern) does not, and
/// `b` belongs to the pattern that did not fire. The handler reports what each
/// reads as, then emits the absent value so the round trip through the host's
/// event bus is pinned too.
const ABSENT_GROUPS_TS: &str = r#"
import { createAlias, createEvent, events, echo } from "smudgy:core";

const probe = createEvent("probe");
events.lookup("user", "probe").on((p: any) => {
    const v = p.v;
    echo("EMITCHECK v=" + (v === null ? "NULL" : typeof v) + " has=" + ("v" in p)
        + " nan=" + (p.n === null ? "NULL" : typeof p.n));
});

createAlias(
    ["^first (?<a>\\w+)(?: (?<opt>\\w+))?$", "^second (?<b>\\w+)$"],
    (m: any) => {
        const opt = m.opt;
        const b = m.b;
        echo("OPTCHECK " + (opt === "" ? "EMPTY" : (opt === null ? "NULL" : String(opt))));
        echo("ABSENTCHECK " + (b === undefined ? "UNDEF" : (b === null ? "NULL" : String(b))) + " in=" + ("b" in m));
        probe.emit({ v: b, n: parseInt(opt) });
    },
);
"#;

/// Pins what capture groups read as in a handler: a non-participating group of
/// the fired pattern, a group of a pattern that did not fire, and the absent
/// value after an `emit` round trip.
#[tokio::test]
async fn absent_and_empty_capture_groups() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "AbsentGroups";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("absent.ts"), ABSENT_GROUPS_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7013),
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

    tx.send(RuntimeAction::Send(Arc::new("first x".to_string())))
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
        lines.iter().any(|l| l == "OPTCHECK EMPTY"),
        "a non-participating group of the fired pattern reads as the empty string.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ABSENTCHECK UNDEF in=false"),
        "a group of a pattern that did not fire is absent (reads as undefined).\nTranscript:\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "EMITCHECK v=undefined has=false nan=NULL"),
        "emit payloads travel as JSON: undefined-valued properties are dropped and NaN \
         arrives as null.\nTranscript:\n{transcript}"
    );
}

#[tokio::test]
async fn arctic_style_module_loads_and_runs_in_session() {
    // Hermetic smudgy home so the test never touches the user's real data dir.
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    // Leak the TempDir: the runtime thread may flush its session log slightly
    // after the test returns, and we don't want cleanup to race that write.
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    // The smudgy home override is a process-global `OnceLock` (first setter in the binary
    // wins), so re-read it after setting and scope everything under a unique server name.
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "Arctic";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("harness.ts"), HARNESS_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7001),
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

    // Collect buffer text until the session goes quiet. The module queues its
    // `echo("HARNESS_OK")` immediately before its `createAlias("^greet$", …)`, so
    // we only send `greet` once HARNESS_OK is observed — that guarantees the
    // alias registration is already ahead of `greet` in the FIFO action queue
    // (otherwise `greet` would be sent literally before the alias exists).
    let mut lines = Vec::new();
    let mut sent_greet = false;
    let mut sent_reload = false;
    // A reload tears down and rebuilds the v8 engine and re-transpiles the module,
    // which can exceed the quiet period, so once `dorel` is sent we keep waiting on
    // a longer per-event timeout until the second HARNESS_OK (the post-reload
    // re-run) arrives, bounded by an overall deadline.
    let reload_budget = Duration::from_secs(30);
    loop {
        let harness_ok_count = lines.iter().filter(|l| *l == "HARNESS_OK").count();
        let timeout = if sent_reload && harness_ok_count < 2 {
            reload_budget
        } else {
            QUIET_PERIOD
        };
        let Ok(Some(event)) = tokio::time::timeout(timeout, events.next()).await else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                    if !sent_greet && line.text == "HARNESS_OK" {
                        tx.send(RuntimeAction::Send(Arc::new("greet".to_string())))
                            .unwrap();
                        sent_greet = true;
                    }
                    // Once the alias has fired, exercise reload() via the
                    // `dorel` alias. A successful reload rebuilds the engine,
                    // re-runs the module top-level, and emits a SECOND HARNESS_OK.
                    if sent_greet && !sent_reload && line.text == "hello world" {
                        tx.send(RuntimeAction::Send(Arc::new("dorel".to_string())))
                            .unwrap();
                        sent_reload = true;
                    }
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "HARNESS_OK"),
        "module top-level checks (node:events, node:crypto sha3-512, localStorage) must pass.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "MAPPER_OP_OK"),
        "real mapper op must be registered and callable.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "MAPPER_GLOBALS_GONE"),
        "mapper and Area must be module exports, not public globals.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "hello world"),
        "function-alias send() must fire on `greet`.\nTranscript:\n{transcript}"
    );
    // reload() must rebuild the engine and re-run the module, so HARNESS_OK
    // appears twice (initial load + post-reload load). One occurrence would mean
    // the `op_smudgy_session_reload` op never routed `RuntimeAction::Reload`.
    assert!(
        lines.iter().filter(|l| *l == "HARNESS_OK").count() >= 2,
        "reload() must rebuild the engine and re-run the module (expected >=2 HARNESS_OK).\nTranscript:\n{transcript}"
    );
}

/// Module exercising a managed self-limiting repeating timer, a script hotkey whose handler
/// echoes when fired, and a `setCurrentLocation` -> `getCurrentLocation` round-trip.
const TIMERS_HOTKEYS_MAPPER_TS: &str = r#"
import core, { createTimer, createHotkey, timers, hotkeys, echo } from "smudgy:core";
// Exercise the default-export facade's live mapper accessor; modules may also import it by name.
const mapper = core.mapper;

// A repeating timer that self-removes after 3 fires, named via options. Each tick echoes
// a sentinel.
createTimer({ intervalMs: 30, repeat: true, fireLimit: 3, name: "ticker" }, () => {
    echo("TIMER_TICK");
});
echo(timers.exists("ticker") ? "TIMER_REGISTERED" : "TIMER_MISSING");

// An unnamed script hotkey: its registry identity is the derived key-combination name
// (lowercased sorted modifiers + key).
createHotkey({ key: "F1", modifiers: ["Control"] }, () => {
    echo("HOTKEY_FIRED");
});
echo(hotkeys.exists("control+F1") ? "HOTKEY_REGISTERED" : "HOTKEY_MISSING");

// Round-trip the current location through set/getCurrentLocation. An area id is a canonical
// UUID string, so it compares with `===` and survives JSON.stringify.
const area = "67e55044-10b1-426f-9247-bb680e5fe0c8";
mapper.setCurrentLocation(area, 42);
const here = mapper.getCurrentLocation();
const locOk = here !== undefined && here.area === area && here.room === 42 &&
    JSON.parse(JSON.stringify(here)).area === area;
echo(locOk ? "LOCATION_OK" : ("LOCATION_FAIL " + JSON.stringify(here)));

// The old [hi, lo] spelling is gone: it must be refused, not quietly reinterpreted.
let rejected = false;
try { mapper.setCurrentLocation([1n, 2n] as any, 43); } catch { rejected = true; }
echo(rejected ? "LEGACY_ID_REJECTED" : "LEGACY_ID_ACCEPTED");
"#;

#[tokio::test]
async fn timers_hotkeys_and_mapper_location() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "TimersHotkeysMapper";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("thm.ts"), TIMERS_HOTKEYS_MAPPER_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7004),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut hotkey_id: Option<HotkeyId> = None;

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            // Module setup is now fully dispatched before RuntimeReady, so UI-facing setup
            // events can correctly precede the readiness boundary. Preserve the id for the
            // post-ready ExecHotkey probe instead of discarding it.
            SessionEvent::RegisterHotkey(id, _def) => hotkey_id = Some(id),
            _ => {}
        }
    };

    // Drive the session: when the hotkey registers, fire it (ExecHotkey) so the handler echoes.
    let mut lines = Vec::new();
    let mut fired = false;
    loop {
        let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await else {
            break;
        };
        match event.event {
            SessionEvent::RegisterHotkey(id, _def) => {
                hotkey_id = Some(id);
            }
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
        // Once the hotkey is registered (and the registration sentinel observed), fire it once.
        if !fired
            && let Some(id) = hotkey_id
            && lines.iter().any(|l| l == "HOTKEY_REGISTERED")
        {
            tx.send(RuntimeAction::ExecHotkey { id }).unwrap();
            fired = true;
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    // The timer registered and fired (self-limited to 3, but >=2 proves it repeats).
    assert!(
        lines.iter().any(|l| l == "TIMER_REGISTERED"),
        "createTimer must register in the timers registry.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().filter(|l| *l == "TIMER_TICK").count() >= 2,
        "a repeating managed timer must fire multiple times.\nTranscript:\n{transcript}"
    );
    // The hotkey registered and its handler fired on ExecHotkey.
    assert!(
        lines.iter().any(|l| l == "HOTKEY_REGISTERED"),
        "createHotkey must register in the hotkeys registry.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "HOTKEY_FIRED"),
        "a script hotkey's handler must fire on ExecHotkey.\nTranscript:\n{transcript}"
    );
    // setCurrentLocation -> getCurrentLocation round-trips.
    assert!(
        lines.iter().any(|l| l == "LEGACY_ID_REJECTED"),
        "the old [hi, lo] id spelling must be refused, not reinterpreted.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "LOCATION_OK"),
        "getCurrentLocation must round-trip the value set by setCurrentLocation.\nTranscript:\n{transcript}"
    );
}

/// Explicit automation names (`options.name`) follow the SAME rule as the automations UI
/// (`naming::validate_name`, via `op_smudgy_validate_name`): they accept anything the UI
/// accepts (hyphens, spaces, parens, interior dots), not just `/^\w+$/`. A package
/// naming an automation `arctic-prompt` (a hyphen!) must therefore load rather
/// than throw `Name must be ... alphanumeric characters and underscores`. Derived names
/// (no `options.name`) skip the rule entirely: they are pattern text, full of characters
/// the filename-safe rule rejects. Everything below runs at module top-level, so a
/// regression makes the calls throw, aborting load and dropping `RELAXED_NAMES_OK`.
const RELAXED_NAMES_TS: &str = r#"
import { createAlias, createTrigger, createHotkey, createTimer, echo } from "smudgy:core";

// Explicit names the old script rule rejected, but the UI's validate_name accepts:
// hyphens, spaces, parentheses, and an interior dot. Patterns never match.
createAlias("^__never_a__$", () => {}, { name: "arctic-prompt" });
createTrigger("^__never_b__$", () => {}, { name: "HP Bar (low)" });
createHotkey({ key: "F2", modifiers: ["Control"] }, () => {}, { name: "save game" });
// repeat:false + huge interval so it never fires during the test.
createTimer({ intervalMs: 1000000, repeat: false, name: "v1.2 ticker" }, () => {});
echo("RELAXED_NAMES_OK");

// A derived name is the pattern source verbatim, including characters the explicit-name
// rule rejects (the "\" and ":" here), because it never becomes a filename. (Registry
// lookups by derived name are covered in handle_crud.rs, where they run after the queued
// registration is applied; here at module top-level the queue hasn't drained yet.)
const derived = createAlias("^go (north|south): (\\w+)$", () => {});
echo(derived.name === "^go (north|south): (\\w+)$"
    ? "DERIVED_NAME_OK" : ("DERIVED_NAME_FAIL name=" + derived.name));

// Explicit names still illegal/unsafe as filenames (or empty/whitespace/reserved/dot-edges)
// must STILL throw, exactly as the UI rejects them.
let rejected = 0;
for (const bad of ["bad/name", "a\\b", "a:b", "", "   ", "CON", ".hidden"]) {
    try { createAlias("^__never_x__$", () => {}, { name: bad }); }
    catch (_e) { rejected++; }
}
echo(rejected === 7 ? "ILLEGAL_STILL_REJECTED" : ("ILLEGAL_LEAK rejected=" + rejected));

// Name-first calls were accepted only through 0.4. In 0.5 all four canonical entry
// points reject the displaced arguments through their normal TypeError validation.
const oldForms = [
    () => createAlias("oldname", "^__old_a__$", () => {}),
    () => createTrigger("oldtrig", "^__old_b__$", () => {}),
    () => createTimer("oldticker", { intervalMs: 1000000, repeat: false }, () => {}),
    () => createHotkey("oldhk", { key: "F3" }, () => {}),
];
let oldFormTypeErrors = 0;
for (const oldForm of oldForms) {
    try { oldForm(); }
    catch (error) { if (error instanceof TypeError) oldFormTypeErrors++; }
}
echo(oldFormTypeErrors === oldForms.length
    ? "OLD_FORMS_REJECTED"
    : `OLD_FORMS_ACCEPTED count=${oldForms.length - oldFormTypeErrors}`);
"#;

#[tokio::test]
async fn script_automation_names_match_ui_rules() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "RelaxedNames";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("names.ts"), RELAXED_NAMES_TS).unwrap();

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

    // Everything is emitted at module top-level; just drain until the session is quiet.
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
        lines.iter().any(|l| l == "RELAXED_NAMES_OK"),
        "createAlias/createTrigger/createHotkey/createTimer must accept UI-legal names \
         (hyphens, spaces, parens, interior dots) instead of the old /^\\w+$/.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "DERIVED_NAME_OK"),
        "an unnamed automation must take its pattern source as its name, exempt from the \
         filename-safe rule explicit names follow.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ILLEGAL_STILL_REJECTED"),
        "filesystem-illegal/empty/reserved/dot-edge names must still be rejected, matching the UI.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "OLD_FORMS_REJECTED"),
        "name-first createAlias/createTrigger/createTimer/createHotkey calls must all throw \
         TypeError in 0.5.\nTranscript:\n{transcript}"
    );
}

const CROSS_SESSION_TS: &str = r#"
import {
  byId,
  byName,
  createAlias,
  createEvent,
  createState,
  echo,
  events,
  getSessions,
  session,
} from "smudgy:core";
import {
  connected,
  created,
  destroyed,
  disconnected,
} from "smudgy:events/sessions";

const crossSession = createEvent("crossSession");
const orderedState = createState<{ answer: number }>("orderedState");
const orderedConsumer = (globalThis as any).__smudgy_interop_consumer("user").state("orderedState");
const channel = new BroadcastChannel("smudgy-cross-session-test");

echo(`BOOT_SESSION:${getSessions().some(peer => peer.id === session.id)}`);

const remoteEvents = events.lookup("user", "crossSession")
  .fromAll({ includeSelf: false });
remoteEvents.on((payload, source) => {
    echo(`EVENT:${source.profile.name}:${payload.answer}`);
    echo(`ORDERED_STATE:${orderedConsumer.from(source).value?.answer}`);
});
remoteEvents.once().then((payload) => {
  echo(`ONCE_PAYLOAD:${payload.answer}:${Array.isArray(payload)}`);
});

channel.onmessage = (event) => {
  echo(`BROADCAST:${event.data.from}:${event.data.answer}`);
};

created.on((affected, source) => {
  if (affected.profile.name === "Beta") {
    echo(`CREATED:${source.profile.name}:${affected.connected}`);
  }
});
connected.on((affected, source) => {
  if (affected.profile.name === "Beta") {
    echo(`CONNECTED:${source.profile.name}`);
  }
});
disconnected.on((affected, source) => {
  if (affected.profile.name === "Beta") {
    echo(`DISCONNECTED:${source.profile.name}:${affected.connected}`);
  }
});
destroyed.on((affected, source) => {
  if (affected.profile.name === "Beta") {
    echo(`DESTROYED:${source.profile.name}:${affected.connected}`);
  }
});

createAlias("^fire-cross-session$", () => {
  const peers = getSessions();
  echo(`ENUM:${peers.length}:${byName("Alpha")?.id !== undefined}:${byName("Gamma") === undefined}:${peers.map(peer => peer.id).join(",")}`);
  const alpha = byName("Alpha")!;
  const seededValue = alpha.input.value;
  const seededHistory = alpha.input.history.list()[0];
  alpha.input.propose("remote draft");
  alpha.input.focus();
  alpha.input.history.push("remote history");
  alpha.input.completion.add("remoteWord");
  const remote = alpha.mainPane.split("right", {
    name: "Remote",
    width: 240,
    input: { onSubmit: (text) => echo(`PANE_SUBMIT:${text}`) },
  });
  echo(`SURFACE:${byId(alpha.id)?.profile.name}:${alpha.panes.exists("remote")}:${alpha.panes.list().length}:${remote.input !== undefined}:${alpha.input.completion.has("remoteword")}:${seededValue}:${seededHistory}`);
  remote.swap(session.mainPane);
  orderedState.set({ answer: 42 });
  crossSession.emit({ answer: 42 });
  channel.postMessage({ from: session.profile.name, answer: 42 });
});
"#;

/// The public cross-session surface is exercised through two real session threads:
/// lifecycle events are live/non-replaying, event filters name their source, enumeration
/// is server-scoped, and Deno's standard `BroadcastChannel` crosses the shared backend.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn same_server_sessions_share_directed_events_lifecycle_and_broadcast_channel() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "CrossSessionInterop";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("cross-session.ts"), CROSS_SESSION_TS).unwrap();
    let other_server = "CrossSessionInteropOther";
    std::fs::create_dir_all(home_path.join(other_server).join("modules")).unwrap();
    std::fs::create_dir_all(home_path.join(other_server).join("logs")).unwrap();

    let params = |id, name: &str| {
        Arc::new(SessionParams {
            session_id: SessionId::from(id),
            server_name: Arc::new(server.to_string()),
            profile_name: Arc::new(name.to_string()),
            profile_subtext: Arc::new(String::new()),
            mapper: None,
            package_client: None,
            extra_script_extensions: Arc::new(Vec::new),
            on_engine_rebuild: None,
        })
    };

    // Alpha is fully ready before Beta is registered, pinning `created` as a future,
    // non-replayed occurrence rather than a startup snapshot.
    let mut alpha_lines = Vec::new();
    let mut alpha_events = Box::pin(spawn(params(7090, "Alpha")));
    let alpha_tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), alpha_events.next())
            .await
            .expect("timed out waiting for Alpha RuntimeReady")
            .expect("Alpha event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        alpha_lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };

    let mut beta_lines = Vec::new();
    let mut beta_events = Box::pin(spawn(params(7091, "Beta")));
    let beta_tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), beta_events.next())
            .await
            .expect("timed out waiting for Beta RuntimeReady")
            .expect("Beta event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        beta_lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    };

    // A simultaneously-live session on another configured server entry must be
    // absent from Alpha/Beta enumeration and routing.
    let mut gamma_events = Box::pin(spawn(Arc::new(SessionParams {
        session_id: SessionId::from(7092),
        server_name: Arc::new(other_server.to_string()),
        profile_name: Arc::new("Gamma".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })));
    let gamma_tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), gamma_events.next())
            .await
            .expect("timed out waiting for Gamma RuntimeReady")
            .expect("Gamma event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    let mut remote_input_key = None;
    let mut saw_remote_input_ops = 0;
    let mut saw_cross_session_swap = false;

    // Seed Alpha's UI-authoritative mirrors, then use an ordered UI event as
    // the barrier proving both updates landed before Beta reads them.
    alpha_tx
        .send(RuntimeAction::InputStateChanged {
            key: MAIN_PANE_KEY,
            snapshot: InputSnapshot {
                value: Arc::new("seeded-alpha".to_string()),
                cursor: 12,
                selection: None,
                focused: false,
                masked: false,
            },
            source: InputSource::User,
        })
        .unwrap();
    alpha_tx
        .send(RuntimeAction::InputHistoryChanged {
            key: MAIN_PANE_KEY,
            entries: Arc::new(vec![Arc::new("seeded history".to_string())]),
        })
        .unwrap();
    alpha_tx.send(RuntimeAction::InputMirrorInterest).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_mins(1), alpha_events.next())
            .await
            .expect("timed out waiting for Alpha input-mirror barrier")
            .expect("Alpha event stream ended before input-mirror barrier");
        match event.event {
            SessionEvent::InputMirrorInterest => break,
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        alpha_lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    }

    beta_tx.send(RuntimeAction::Connected).unwrap();
    let connected_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < connected_deadline
        && !alpha_lines.iter().any(|line| line == "CONNECTED:Beta")
    {
        if let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, alpha_events.next()).await
            && let SessionEvent::UpdateBuffer(updates) = event.event
        {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    alpha_lines.push(line.text.clone());
                }
            }
        }
    }

    beta_tx
        .send(RuntimeAction::Disconnected {
            connection_generation: 0,
        })
        .unwrap();
    beta_tx
        .send(RuntimeAction::Send(Arc::new(
            "fire-cross-session".to_string(),
        )))
        .unwrap();

    let delivery_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < delivery_deadline
        && !(alpha_lines.iter().any(|line| line == "EVENT:Beta:42")
            && alpha_lines.iter().any(|line| line == "ORDERED_STATE:42")
            && alpha_lines.iter().any(|line| line == "BROADCAST:Beta:42")
            && alpha_lines
                .iter()
                .any(|line| line == "ONCE_PAYLOAD:42:false")
            && beta_lines
                .iter()
                .any(|line| line == "ENUM:2:true:true:7090,7091")
            && beta_lines
                .iter()
                .any(|line| line == "SURFACE:Alpha:true:2:true:true:seeded-alpha:seeded history")
            && beta_lines
                .iter()
                .any(|line| line == "PANE_SUBMIT:from-alpha-ui")
            && saw_remote_input_ops >= 3
            && saw_cross_session_swap)
    {
        tokio::select! {
            event = alpha_events.next() => {
                if let Some(event) = event {
                    match event.event {
                        SessionEvent::UpdateBuffer(updates) => {
                            for update in updates.iter() {
                                if let BufferUpdate::Append(line) = update {
                                    alpha_lines.push(line.text.clone());
                                }
                            }
                        }
                        SessionEvent::InputOp { key, .. } if key == smudgy_core::session::runtime::pane::MAIN_PANE_KEY => {
                            saw_remote_input_ops += 1;
                        }
                        SessionEvent::PaneOpened { def, .. } if def.name.as_ref() == "Remote" => {
                            remote_input_key = Some(def.key);
                            alpha_tx.send(RuntimeAction::PaneInputSubmit {
                                key: def.key,
                                text: Arc::new("from-alpha-ui".to_string()),
                                retry: false,
                            }).unwrap();
                        }
                        SessionEvent::PaneSwap { other_session, .. } if other_session == SessionId::from(7091) => {
                            saw_cross_session_swap = true;
                        }
                        _ => {}
                    }
                }
            }
            event = beta_events.next() => {
                if let Some(event) = event
                    && let SessionEvent::UpdateBuffer(updates) = event.event
                {
                    for update in updates.iter() {
                        if let BufferUpdate::Append(line) = update {
                            beta_lines.push(line.text.clone());
                        }
                    }
                }
            }
            () = tokio::time::sleep(QUIET_PERIOD) => {}
        }
    }

    beta_tx.send(RuntimeAction::Shutdown).ok();
    let destroyed_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < destroyed_deadline
        && !alpha_lines
            .iter()
            .any(|line| line == "DESTROYED:Beta:false")
    {
        if let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, alpha_events.next()).await
            && let SessionEvent::UpdateBuffer(updates) = event.event
        {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    alpha_lines.push(line.text.clone());
                }
            }
        }
    }
    alpha_tx.send(RuntimeAction::Shutdown).ok();
    gamma_tx.send(RuntimeAction::Shutdown).ok();

    let alpha_transcript = alpha_lines.join("\n");
    let beta_transcript = beta_lines.join("\n");
    for expected in [
        "CREATED:Beta:false",
        "CONNECTED:Beta",
        "DISCONNECTED:Beta:false",
        "EVENT:Beta:42",
        "ORDERED_STATE:42",
        "ONCE_PAYLOAD:42:false",
        "BROADCAST:Beta:42",
        "DESTROYED:Beta:false",
    ] {
        assert!(
            alpha_lines.iter().any(|line| line == expected),
            "missing {expected:?} from Alpha transcript:\n{alpha_transcript}\nBeta:\n{beta_transcript}"
        );
    }
    assert!(
        alpha_lines.iter().any(|line| line == "BOOT_SESSION:true")
            && beta_lines.iter().any(|line| line == "BOOT_SESSION:true"),
        "top-level enumeration must include the already-registered current session.\nAlpha:\n{alpha_transcript}\nBeta:\n{beta_transcript}"
    );
    assert!(
        beta_lines
            .iter()
            .any(|line| line == "ENUM:2:true:true:7090,7091"),
        "same-server enumeration/byName failed.\nAlpha:\n{alpha_transcript}\nBeta:\n{beta_transcript}"
    );
    assert!(
        beta_lines
            .iter()
            .any(|line| line == "SURFACE:Alpha:true:2:true:true:seeded-alpha:seeded history"),
        "cross-session pane/input lookup and completion registry failed.\nAlpha:\n{alpha_transcript}\nBeta:\n{beta_transcript}"
    );
    assert!(
        beta_lines
            .iter()
            .any(|line| line == "PANE_SUBMIT:from-alpha-ui"),
        "a foreign pane input must route onSubmit back to its creating runtime (key={remote_input_key:?}, ops={saw_remote_input_ops}, swap={saw_cross_session_swap}).\nAlpha:\n{alpha_transcript}\nBeta:\n{beta_transcript}"
    );
    assert!(remote_input_key.is_some() && saw_remote_input_ops >= 3 && saw_cross_session_swap);
}
