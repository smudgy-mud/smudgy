//! Phase-2 pane exit criteria (`docs/flexible-panes-plan.md` §3): the routing
//! matrix (gag/redirect/copy × complete/partial lines, copy-then-gag,
//! gag-then-replace, redirect-twice, redirect+copy dedup,
//! redirect-after-partial-flush retraction), numbering parity under pane-echo
//! interleaving, split-then-redirect within one trigger, get-or-create with
//! case folding, name-id stability across close/recreate, reload survival of
//! the registry plus the reload sweep (panes no script re-claims are closed),
//! the pane cap, and kind-mismatch errors.
//!
//! Exercises the genuine session runtime: a module creates panes and
//! registers triggers, and the test feeds real complete/partial lines and
//! asserts on the full `SessionEvent` stream (buffer updates + pane events).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::runtime::pane::{
    MAIN_PANE_KEY, PaneKey, PaneNameId, PanePlacement, SplitDirection,
};
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const PANES_TS: &str = r#"
import { createTrigger, echo, line, session, vars } from "smudgy:core";

const chat = session.mainPane.split("right", { name: "Chat", width: 300 });
const again = session.mainPane.split("right", { name: "chat" });
const info = chat.split("bottom", { name: "info", height: 120 });
echo("SETUP created=" + chat.created
    + " again=" + again.created
    + " display=" + again.name
    + " kind=" + chat.kind
    + " exists=" + session.panes.exists("CHAT")
    + " dot=" + (session.panes.chat !== undefined)
    + " count=" + session.panes.list().length);

createTrigger("^GTELL ", () => { line.redirect(chat); });
createTrigger("^COPY ", () => { line.copy(chat); });
createTrigger("^COPYGAG ", () => { line.copy(chat); line.gag(); });
createTrigger("^GAGREP ", () => {
    // Gag no longer short-circuits: the replace must still apply to the
    // routed copy.
    line.gag();
    line.copy(chat);
    line.replace("GAGREP", "XFORMED");
});
createTrigger("^TWICE ", () => { line.redirect(info); line.redirect(chat); });
createTrigger("^SAMEPANE ", () => { line.redirect(chat); line.copy(chat); line.copy("chat"); });
createTrigger("^SPLITREDIR$", () => {
    const fresh = session.mainPane.split("right", { name: "fresh" });
    line.redirect(fresh);
});
createTrigger("^REDIR-REST$", () => { line.redirect(chat); });
createTrigger("^NOISE$", () => { chat.echo("noise-a\nnoise-b"); });
createTrigger("^PARITY ", () => { vars.parityNumber = line.number; });
createTrigger("^PARITYCHECK$", () => { echo("PARITY_AT=" + vars.parityNumber); });
createTrigger("^RECREATE$", () => {
    session.panes.get("info").close();
    const back = session.mainPane.split("right", { name: "INFO" });
    echo("RECREATED created=" + back.created + " name=" + back.name);
});
createTrigger("^BADKIND$", () => {
    let threw = false;
    try { session.mainPane.split("right", { name: "chat", terminal: false }); } catch (e) { threw = true; }
    echo("KIND_THROWN=" + threw);
});
createTrigger("^CAPFILL$", () => {
    let thrown = false;
    try {
        for (let i = 0; i < 20; i++) {
            session.mainPane.split("right", { name: "cap" + i });
        }
    } catch (e) { thrown = true; }
    echo("CAP_THROWN=" + thrown + " count=" + session.panes.list().length);
});
createTrigger("^PANECOUNT$", () => {
    echo("COUNT=" + session.panes.list().length);
});
echo("PR_READY");
"#;

fn sl(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(text, Vec::new()))
}

/// One entry of the flattened, ordered stream the assertions run over.
#[derive(Debug)]
enum Seen {
    Append(String),
    EnsureNewLine,
    AppendTo(PaneKey, String),
    Retract,
    Opened {
        name: String,
        key: PaneKey,
        name_id: PaneNameId,
        placement: PanePlacement,
    },
    Closed(PaneKey),
}

// One end-to-end scenario deliberately drives the whole routing matrix through
// a single live session, so the setup and the ordered event assertions read as
// one narrative rather than a dozen near-duplicate harnesses.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn pane_routing_matrix_parity_and_registry_semantics() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "PaneRouting";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("panes.ts"), PANES_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7100),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));

    let mut seen: Vec<Seen> = Vec::new();
    let mut tx = None;

    // The scripted sequence: each step fires once its gate marker appears
    // anywhere in the stream. Complete lines go through the real
    // HandleIncomingLine path; the retraction case flushes a genuine partial
    // first.
    let mut step = 0usize;
    let mut reloaded = false;
    let mut done = false;

    'outer: loop {
        let timeout = if done {
            // Drain whatever is still in flight, then stop.
            Duration::from_secs(2)
        } else {
            Duration::from_mins(2)
        };
        let Ok(Some(event)) = tokio::time::timeout(timeout, events.next()).await else {
            break 'outer;
        };
        match event.event {
            SessionEvent::RuntimeReady(ready_tx) => {
                tx = Some(ready_tx);
            }
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    match update {
                        BufferUpdate::Append(line) => seen.push(Seen::Append(line.text.clone())),
                        BufferUpdate::EnsureNewLine => seen.push(Seen::EnsureNewLine),
                        BufferUpdate::AppendTo(key, line) => {
                            seen.push(Seen::AppendTo(*key, line.text.clone()));
                        }
                        BufferUpdate::RetractOpenLine => seen.push(Seen::Retract),
                        BufferUpdate::Clear(_) => {}
                    }
                }
            }
            SessionEvent::PaneOpened { def, placement } => {
                seen.push(Seen::Opened {
                    name: def.name.to_string(),
                    key: def.key,
                    name_id: def.name_id,
                    placement,
                });
            }
            SessionEvent::PaneClosed(key) => seen.push(Seen::Closed(key)),
            _ => {}
        }

        let Some(tx) = tx.as_ref() else { continue };

        // Advance the script whenever the current step's gate is satisfied.
        let has_append = |needle: &str| {
            seen.iter().any(
                |s| matches!(s, Seen::Append(text) if text.starts_with(needle) || text == needle),
            )
        };
        let has_append_to = |needle: &str| {
            seen.iter()
                .any(|s| matches!(s, Seen::AppendTo(_, text) if text == needle))
        };
        let send_line = |text: &str| {
            tx.send(RuntimeAction::HandleIncomingLine(sl(text))).unwrap();
            tx.send(RuntimeAction::RequestRepaint).unwrap();
        };

        let advanced = match step {
            0 if has_append("PR_READY") => {
                send_line("NOISE");
                true
            }
            1 if has_append_to("noise-b") => {
                send_line("PARITY hello");
                true
            }
            2 if has_append("PARITY hello") => {
                send_line("GTELL hello");
                true
            }
            3 if has_append_to("GTELL hello") => {
                // Redirect decided AFTER a prefix flushed to main as a
                // partial: core must retract the stranded open fragment and
                // deliver the assembled whole line to the pane.
                tx.send(RuntimeAction::HandleIncomingPartialLine(sl("PROMPT>")))
                    .unwrap();
                tx.send(RuntimeAction::RequestRepaint).unwrap();
                send_line("REDIR-REST");
                true
            }
            4 if has_append_to("PROMPT>REDIR-REST") => {
                send_line("COPY both");
                true
            }
            5 if has_append_to("COPY both") => {
                send_line("COPYGAG hidden");
                true
            }
            6 if has_append_to("COPYGAG hidden") => {
                send_line("GAGREP secret");
                true
            }
            7 if has_append_to("XFORMED secret") => {
                send_line("TWICE x");
                true
            }
            8 if has_append_to("TWICE x") => {
                send_line("SAMEPANE x");
                true
            }
            9 if has_append_to("SAMEPANE x") => {
                send_line("SPLITREDIR");
                true
            }
            10 if has_append_to("SPLITREDIR") => {
                send_line("RECREATE");
                true
            }
            11 if has_append("RECREATED ") => {
                send_line("BADKIND");
                true
            }
            12 if has_append("KIND_THROWN=") => {
                send_line("CAPFILL");
                true
            }
            13 if has_append("CAP_THROWN=") => {
                send_line("PARITYCHECK");
                true
            }
            14 if has_append("PARITY_AT=") && !reloaded => {
                // Reload survival: the registry (and its interning) must
                // persist through a full engine rebuild — and the reload
                // sweep must then close every pane the reloaded module did
                // not re-claim (fresh + the cap panes were trigger-created,
                // so nothing re-splits them at load).
                reloaded = true;
                tx.send(RuntimeAction::Reload).unwrap();
                true
            }
            15 => {
                // The second SETUP echo (from the reloaded module) means the
                // load finished; the sweep is queued behind it, so a count
                // probe sent now observes the post-sweep registry.
                let setups = seen
                    .iter()
                    .filter(|s| matches!(s, Seen::Append(text) if text.starts_with("SETUP ")))
                    .count();
                if setups >= 2 {
                    send_line("PANECOUNT");
                    true
                } else {
                    false
                }
            }
            16 if has_append("COUNT=") => {
                done = true;
                false
            }
            _ => false,
        };
        if advanced {
            step += 1;
        }
    }

    if let Some(tx) = tx.as_ref() {
        tx.send(RuntimeAction::Shutdown).ok();
    }

    let transcript = format!("{seen:#?}");

    // ---- Registry semantics reported by the script -------------------------
    let setups: Vec<&String> = seen
        .iter()
        .filter_map(|s| match s {
            Seen::Append(text) if text.starts_with("SETUP ") => Some(text),
            _ => None,
        })
        .collect();
    assert!(setups.len() >= 2, "expected two SETUP lines (initial + reload).\n{transcript}");
    assert!(
        setups[0].contains("created=true")
            && setups[0].contains("again=false")
            && setups[0].contains("display=Chat")
            && setups[0].contains("kind=terminal")
            && setups[0].contains("exists=true")
            && setups[0].contains("dot=true")
            && setups[0].contains("count=3"),
        "get-or-create + case folding + dot access on first load: {}\n{transcript}",
        setups[0]
    );
    // After the reload the registry persisted: every split is a
    // get-or-create hit and all panes (chat/INFO/fresh + the 13 cap panes +
    // main) are still there.
    assert!(
        setups[1].contains("created=false") && setups[1].contains("count=17"),
        "the pane registry must survive a script reload: {}\n{transcript}",
        setups[1]
    );

    // ---- Pane placement metadata -------------------------------------------
    let opened: Vec<(&String, PaneKey, PaneNameId, PanePlacement)> = seen
        .iter()
        .filter_map(|s| match s {
            Seen::Opened { name, key, name_id, placement } => {
                Some((name, *key, *name_id, *placement))
            }
            _ => None,
        })
        .collect();
    let find_opened = |wanted: &str| {
        opened
            .iter()
            .find(|(name, ..)| name.as_str() == wanted)
            .unwrap_or_else(|| panic!("no PaneOpened for '{wanted}'.\n{transcript}"))
    };
    let chat = find_opened("Chat");
    let info = find_opened("info");
    assert_eq!(chat.3.reference, MAIN_PANE_KEY, "Chat splits off main");
    assert!(matches!(chat.3.direction, SplitDirection::Right));
    assert_eq!(chat.3.size_px, Some(300.0), "width honored on a right split");
    assert_eq!(info.3.reference, chat.1, "info splits off Chat");
    assert!(matches!(info.3.direction, SplitDirection::Bottom));
    assert_eq!(info.3.size_px, Some(120.0), "height honored on a bottom split");
    let chat_key = chat.1;
    let info_key = info.1;

    // ---- Name-id stability across close/recreate (fresh PaneKey) ----------
    let recreated = find_opened("INFO");
    assert!(
        seen.iter().any(|s| matches!(s, Seen::Closed(k) if *k == info_key)),
        "closing 'info' must emit PaneClosed for its key.\n{transcript}"
    );
    assert_eq!(
        recreated.2, info.2,
        "close-then-recreate keeps the interned name id (widget re-attach identity).\n{transcript}"
    );
    assert_ne!(
        recreated.1, info_key,
        "a recreated pane must mint a fresh, never-reused PaneKey.\n{transcript}"
    );

    // ---- Reload sweep -------------------------------------------------------
    // The reloaded module re-splits Chat and info at top level (re-claiming
    // them); 'fresh' and the cap panes were trigger-created mid-session, so
    // the sweep behind the reload must close exactly those.
    let closed: Vec<PaneKey> = seen
        .iter()
        .filter_map(|s| match s {
            Seen::Closed(key) => Some(*key),
            _ => None,
        })
        .collect();
    let fresh_key = find_opened("fresh").1;
    assert!(
        closed.contains(&fresh_key),
        "the reload sweep must close the unclaimed 'fresh' pane.\n{transcript}"
    );
    for i in 0..13 {
        let cap_key = find_opened(&format!("cap{i}")).1;
        assert!(
            closed.contains(&cap_key),
            "the reload sweep must close the unclaimed 'cap{i}' pane.\n{transcript}"
        );
    }
    assert!(
        !closed.contains(&chat_key),
        "a pane the reloaded module re-claims must survive the sweep.\n{transcript}"
    );
    assert!(
        !closed.contains(&recreated.1),
        "the re-claimed INFO pane must survive the sweep.\n{transcript}"
    );
    // The post-sweep probe sees only main + the two re-claimed panes.
    assert!(
        seen.iter()
            .any(|s| matches!(s, Seen::Append(text) if text.as_str() == "COUNT=3")),
        "post-sweep registry must hold main + Chat + info only.\n{transcript}"
    );

    // ---- The routing matrix -------------------------------------------------
    let main_appends: Vec<&String> = seen
        .iter()
        .filter_map(|s| match s {
            Seen::Append(text) => Some(text),
            _ => None,
        })
        .collect();
    let append_tos: Vec<(PaneKey, &String)> = seen
        .iter()
        .filter_map(|s| match s {
            Seen::AppendTo(key, text) => Some((*key, text)),
            _ => None,
        })
        .collect();

    for hidden in [
        "GTELL hello",
        "REDIR-REST",
        "COPYGAG hidden",
        "GAGREP secret",
        "XFORMED secret",
        "TWICE x",
        "SAMEPANE x",
        "SPLITREDIR",
    ] {
        assert!(
            !main_appends.iter().any(|text| text.as_str() == hidden),
            "'{hidden}' must not reach the main buffer.\n{transcript}"
        );
    }
    for (expected_key, text) in [
        (chat_key, "GTELL hello"),
        (chat_key, "PROMPT>REDIR-REST"),
        (chat_key, "COPY both"),
        (chat_key, "COPYGAG hidden"),
        (chat_key, "XFORMED secret"),
        (chat_key, "TWICE x"),
        (chat_key, "noise-a"),
        (chat_key, "noise-b"),
    ] {
        assert!(
            append_tos
                .iter()
                .any(|(key, t)| *key == expected_key && t.as_str() == text),
            "expected AppendTo({expected_key}, '{text}').\n{transcript}"
        );
    }
    // A copy delivers ADDITIONALLY: the line still reaches main.
    assert!(
        main_appends.iter().any(|text| text.as_str() == "COPY both"),
        "a copied line still reaches main.\n{transcript}"
    );
    // Redirect-twice: last wins — nothing lands in the first target.
    assert!(
        !append_tos
            .iter()
            .any(|(key, t)| *key == info_key && t.as_str() == "TWICE x"),
        "redirect-twice must deliver to the LAST target only.\n{transcript}"
    );
    // Redirect + copy of the same pane (by handle and by name): one delivery.
    assert_eq!(
        append_tos
            .iter()
            .filter(|(_, t)| t.as_str() == "SAMEPANE x")
            .count(),
        1,
        "redirect+copy of the same pane must dedupe to one delivery.\n{transcript}"
    );
    // Retraction: the flushed partial prefix was withdrawn from main.
    assert!(
        main_appends.iter().any(|text| text.as_str() == "PROMPT>"),
        "the partial prefix must have flushed to main first.\n{transcript}"
    );
    assert!(
        seen.iter().any(|s| matches!(s, Seen::Retract)),
        "redirect-after-partial-flush must retract main's open line.\n{transcript}"
    );

    // ---- split-then-redirect within one trigger body ------------------------
    let fresh = find_opened("fresh");
    let fresh_opened_at = seen
        .iter()
        .position(|s| matches!(s, Seen::Opened { name, .. } if name == "fresh"))
        .unwrap();
    let fresh_append_at = seen
        .iter()
        .position(|s| matches!(s, Seen::AppendTo(key, text) if *key == fresh.1 && text == "SPLITREDIR"))
        .unwrap_or_else(|| panic!("the fresh pane must receive the redirected line.\n{transcript}"));
    assert!(
        fresh_opened_at < fresh_append_at,
        "PaneOpened must precede the first AppendTo for the key.\n{transcript}"
    );

    // ---- Cap + kind mismatch -------------------------------------------------
    assert!(
        main_appends.iter().any(|text| text.as_str() == "KIND_THROWN=true"),
        "a kind mismatch on get-or-create must throw.\n{transcript}"
    );
    assert!(
        main_appends
            .iter()
            .any(|text| text.starts_with("CAP_THROWN=true") && text.contains("count=17")),
        "the 16-non-main-pane cap must throw and hold.\n{transcript}"
    );

    // ---- Numbering parity under pane-echo interleaving -----------------------
    // Replay the update stream exactly the way the UI TerminalBuffer numbers
    // lines, and compare against the number the script observed for the
    // "PARITY hello" line. AppendTo/pane echoes must not shift it.
    let mut ui_number = 0usize;
    let mut lines_len = 0usize;
    let mut terminated = false;
    let mut parity_ui_number = None;
    for s in &seen {
        match s {
            Seen::Append(text) => {
                if terminated || lines_len == 0 {
                    ui_number += 1;
                    lines_len += 1;
                    terminated = false;
                }
                if text == "PARITY hello" {
                    parity_ui_number = Some(ui_number);
                }
            }
            Seen::EnsureNewLine => terminated = true,
            Seen::Retract if !terminated && lines_len > 0 => {
                lines_len -= 1;
                ui_number -= 1;
                terminated = true;
            }
            _ => {}
        }
    }
    let parity_ui_number =
        parity_ui_number.unwrap_or_else(|| panic!("PARITY line never displayed.\n{transcript}"));
    let reported: usize = main_appends
        .iter()
        .find_map(|text| text.strip_prefix("PARITY_AT="))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no PARITY_AT report.\n{transcript}"));
    assert_eq!(
        reported, parity_ui_number,
        "core's line number and the UI's must agree for the PARITY line (pane deliveries must not count).\n{transcript}"
    );
}

/// Regression module for two routing-safety fixes:
/// - `line.redirect`/`line.copy` to a widgets-only pane must THROW (it has no
///   terminal buffer, so the routed line would be gagged from main and then
///   silently dropped by the UI).
/// - a `pane.echo()` issued before a `pane.close()` in the same body must still
///   be delivered: the own-session op resolves the key synchronously, so the
///   later close (which retires the registry entry) cannot strand it.
const PANES_FIX_TS: &str = r#"
import { createTrigger, echo, line, session } from "smudgy:core";

const hud = session.mainPane.split("right", { name: "hud", terminal: false });
session.mainPane.split("right", { name: "log" });
echo("FIXSETUP");

createTrigger("^REDIRWID$", () => {
    try {
        line.redirect(hud);
        echo("REDIR_OK");
    } catch (_e) {
        echo("REDIR_THREW");
    }
});

createTrigger("^ECHOCLOSE$", () => {
    const p = session.panes.get("log");
    p.echo("ECHO-BEFORE-CLOSE");
    p.close();
});
"#;

#[tokio::test]
async fn redirect_to_widgets_throws_and_echo_before_close_delivers() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("smudgy home");

    let server = "PaneRoutingFix";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("panes.ts"), PANES_FIX_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7200),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut seen: Vec<Seen> = Vec::new();
    let mut tx = None;
    let mut log_key: Option<PaneKey> = None;
    let mut step = 0usize;
    let mut done = false;

    'outer: loop {
        let timeout = if done {
            Duration::from_secs(2)
        } else {
            Duration::from_mins(2)
        };
        let Ok(Some(event)) = tokio::time::timeout(timeout, events.next()).await else {
            break 'outer;
        };
        match event.event {
            SessionEvent::RuntimeReady(ready_tx) => tx = Some(ready_tx),
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    match update {
                        BufferUpdate::Append(line) => seen.push(Seen::Append(line.text.clone())),
                        BufferUpdate::EnsureNewLine => seen.push(Seen::EnsureNewLine),
                        BufferUpdate::AppendTo(key, line) => {
                            seen.push(Seen::AppendTo(*key, line.text.clone()));
                        }
                        BufferUpdate::RetractOpenLine => seen.push(Seen::Retract),
                        BufferUpdate::Clear(_) => {}
                    }
                }
            }
            SessionEvent::PaneOpened { def, .. } => {
                if def.name.as_ref() == "log" {
                    log_key = Some(def.key);
                }
            }
            SessionEvent::PaneClosed(key) => seen.push(Seen::Closed(key)),
            _ => {}
        }

        let Some(tx) = tx.as_ref() else { continue };
        let has_append = |needle: &str| {
            seen.iter()
                .any(|s| matches!(s, Seen::Append(text) if text == needle))
        };
        let send_line = |text: &str| {
            tx.send(RuntimeAction::HandleIncomingLine(sl(text))).unwrap();
            tx.send(RuntimeAction::RequestRepaint).unwrap();
        };

        let advanced = match step {
            0 if has_append("FIXSETUP") => {
                send_line("REDIRWID");
                true
            }
            1 if has_append("REDIR_THREW") || has_append("REDIR_OK") => {
                send_line("ECHOCLOSE");
                true
            }
            2 if log_key.is_some_and(|k| seen.iter().any(|s| matches!(s, Seen::Closed(c) if *c == k))) => {
                done = true;
                false
            }
            _ => false,
        };
        if advanced {
            step += 1;
        }
    }

    if let Some(tx) = tx.as_ref() {
        tx.send(RuntimeAction::Shutdown).ok();
    }

    let transcript = format!("{seen:#?}");
    let log_key = log_key.unwrap_or_else(|| panic!("no PaneOpened for 'log'.\n{transcript}"));

    // Redirect to a widgets-only pane throws (and never silently loses the line).
    assert!(
        seen.iter().any(|s| matches!(s, Seen::Append(t) if t == "REDIR_THREW")),
        "line.redirect to a widgets-only pane must throw.\n{transcript}"
    );
    assert!(
        !seen.iter().any(|s| matches!(s, Seen::Append(t) if t == "REDIR_OK")),
        "line.redirect to a widgets-only pane must NOT succeed.\n{transcript}"
    );

    // Echo-before-close: the AppendTo lands, and it precedes the PaneClosed for
    // the same key (the flush-before-close ordering guarantee holds).
    let echo_idx = seen.iter().position(
        |s| matches!(s, Seen::AppendTo(k, t) if *k == log_key && t == "ECHO-BEFORE-CLOSE"),
    );
    let close_idx = seen
        .iter()
        .position(|s| matches!(s, Seen::Closed(k) if *k == log_key));
    let echo_idx = echo_idx
        .unwrap_or_else(|| panic!("echo issued before close() must still be delivered.\n{transcript}"));
    let close_idx = close_idx
        .unwrap_or_else(|| panic!("the 'log' pane must have been closed.\n{transcript}"));
    assert!(
        echo_idx < close_idx,
        "the pre-close echo must arrive before the PaneClosed.\n{transcript}"
    );
}
