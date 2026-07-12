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
use smudgy_core::session::{BufferUpdate, HotkeyId, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// An Arctic-style module: top-level checks emit a single sentinel, plus a
/// function-alias whose body calls `send()`. `digest` is the sha3-512/base64url
/// of "abc" (the algorithm Arctic's mapper/hash.ts uses).
const HARNESS_TS: &str = r#"
import { EventEmitter } from "node:events";
import { createHash } from "node:crypto";
// The convenience surface is not ambient in modules (globalThis is
// minimal). A module imports what it uses from smudgy:core: `createAlias`/`echo`/
// `send` as named exports, and the current-session facade as the default export
// (`session`) for live accessors like `reload`/`vars`. `mapper` remains a global
// (its own extension owns that object), so it stays ambient here.
import session, { createAlias, echo, send, vars } from "smudgy:core";

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

    tx.send(RuntimeAction::Send(Arc::new("first x".to_string()))).unwrap();

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
        lines.iter().any(|l| l == "EMITCHECK v=undefined has=false nan=NULL"),
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
// `mapper` is a live accessor on the default-export facade (not a named export).
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

// Round-trip the current location through set/getCurrentLocation. The area id is a
// [u64, u64] pair; room 42. getCurrentLocation should read it straight back.
const area = [1n, 2n];
mapper.setCurrentLocation(area, 42);
const here = mapper.getCurrentLocation();
// The area id pair round-trips as a [u64, u64]; serde may surface small ids as plain numbers,
// so coerce both sides through Number for the comparison.
const locOk = here !== undefined &&
    Number(here.area[0]) === 1 && Number(here.area[1]) === 2 && here.room === 42;
echo(locOk ? "LOCATION_OK" : ("LOCATION_FAIL " + JSON.stringify(here)));
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

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Drive the session: when the hotkey registers, fire it (ExecHotkey) so the handler echoes.
    let mut lines = Vec::new();
    let mut hotkey_id: Option<HotkeyId> = None;
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

// The retired name-first shape keeps working through the 0.4 deprecation shim: the
// positional name lands in options.name (so it is the handle's name and registry
// identity), and a [deprecated] notice is echoed once per function -- the second
// createAlias call below must not add a second notice.
const oldAlias = createAlias("oldname", "^__old_a__$", () => {});
createAlias("oldname2", "^__old_b__$", () => {});
const oldTrigger = createTrigger("oldtrig", "^__old_c__$", () => {});
const oldTimer = createTimer("oldticker", { intervalMs: 1000000, repeat: false }, () => {});
const oldHotkey = createHotkey("oldhk", { key: "F3" }, () => {});
const shimmed =
    oldAlias.name === "oldname" &&
    oldTrigger.name === "oldtrig" &&
    oldTimer.name === "oldticker" &&
    oldHotkey.name === "oldhk";
echo(shimmed ? "OLD_FORM_SHIMMED" : ("OLD_FORM_BROKEN a=" + oldAlias.name + " t=" + oldTrigger.name
    + " ti=" + oldTimer.name + " hk=" + oldHotkey.name));

// Both names at once is a contradiction the shim refuses rather than guesses about.
let conflictThrew = false;
try { createAlias("oldboth", "^__old_d__$", () => {}, { name: "otherboth" }); }
catch (_e) { conflictThrew = true; }
echo(conflictThrew ? "OLD_FORM_CONFLICT_THROWS" : "OLD_FORM_CONFLICT_ACCEPTED");
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
    // The name-first deprecation shim (removed in 0.5; see the tripwire in
    // script_typings.rs): old-form calls register under their positional name, each of the
    // four functions notices exactly once per script, and a positional-name/options.name
    // conflict throws.
    assert!(
        lines.iter().any(|l| l == "OLD_FORM_SHIMMED"),
        "name-first create* calls must keep working through the deprecation shim, with the \
         positional name as the automation's name.\nTranscript:\n{transcript}"
    );
    for func in ["createAlias", "createTrigger", "createTimer", "createHotkey"] {
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("[deprecated]") && l.contains(func))
                .count(),
            1,
            "{func} must emit exactly one deprecation notice per script, however many \
             old-form calls it makes.\nTranscript:\n{transcript}"
        );
    }
    assert!(
        lines.iter().any(|l| l == "OLD_FORM_CONFLICT_THROWS"),
        "a positional name combined with options.name must throw.\nTranscript:\n{transcript}"
    );
}
