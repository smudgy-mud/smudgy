//! A local line can commit a displayed prompt before the server completes it.
//! The completion must show only bytes that the terminal has not shown.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::styled_line::StyledLine;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const PROMPT: &str = "Choice: ";
const SUFFIX: &str = "accepted";
const ECHO: &str = "trigger output";
const COMMAND: &str = "north";
const SECRET: &str = "swordfish";
const MASK: &str = "********";
const BEGIN: &str = "__FRAGMENTED_PROMPT_TEST_BEGIN__";
const END: &str = "__FRAGMENTED_PROMPT_TEST_END__";

const PROMPT_TRIGGER_TS: &str = r#"
import { buffer, createTrigger, echo, line, vars } from "smudgy:core";
import { receive } from "smudgy:events/sys";

createTrigger(/^MATCH$/, () => echo("PROMPT:" + line.text), { prompt: true });

createTrigger(/^prefix MATCH done$/, () => {
    vars.completeSubject = line.text;
});

receive.on(({ text }) => {
    if (text === "prefix MATCH done") {
        echo("SUBJECTS:" + vars.completeSubject + "|" + text);
    }
});

createTrigger(/^CHECK$/, () => {
    const n = line.number;
    echo("BUFFER:" + JSON.stringify([
        buffer.line(n - 5).text,
        buffer.line(n - 4).text,
        buffer.line(n - 3).text,
        buffer.line(n - 2).text,
        buffer.line(n - 1).text,
    ]));
});

echo("PROMPT_TRIGGER_READY");
"#;

const GAPPED_PARTIAL_TS: &str = r#"
import { createTrigger, echo, line, vars } from "smudgy:core";
import { receive } from "smudgy:events/sys";

vars.gapCompleteSubject = "missing";
vars.gapReceiveSubject = "missing";

createTrigger(/^Q$/, () => line.gag(), {
    name: "gag-middle-partial",
    prompt: true,
});

createTrigger(/^PQRS$/, () => {
    vars.gapCompleteSubject = line.text;
}, { name: "observe-gapped-completion" });

receive.on(({ text }) => {
    if (text === "PQRS") {
        vars.gapReceiveSubject = text;
    }
});

createTrigger(/^CHECK_GAP$/, () => {
    echo("GAP_SUBJECTS:" + vars.gapCompleteSubject + "|" + vars.gapReceiveSubject);
}, { name: "report-gapped-subjects" });

echo("GAPPED_PARTIAL_READY");
"#;

const BOUNDARY_GAP_TS: &str = r#"
import { createTrigger, echo, line } from "smudgy:core";

createTrigger(/^Q$/, () => line.gag(), {
    name: "gag-middle-prompt-fragment",
    prompt: true,
});

echo("BOUNDARY_GAP_READY");
"#;

const COMMITTED_INSERT_TS: &str = r#"
import { createTrigger, echo, line, vars } from "smudgy:core";
import { receive } from "smudgy:events/sys";

vars.insertTriggerSubject = "missing";
vars.insertReceiveSubject = "missing";

createTrigger(/^AAQ$/, () => {
    vars.insertTriggerSubject = line.text;
    line.insert("A", 0);
}, { name: "insert-before-committed-prefix" });

receive.on(({ text }) => {
    if (text === "AAQ") {
        vars.insertReceiveSubject = text;
    }
});

createTrigger(/^CHECK_INSERT$/, () => {
    echo("INSERT_SUBJECTS:" + vars.insertTriggerSubject + "|" + vars.insertReceiveSubject);
}, { name: "report-insert-subjects" });

echo("COMMITTED_INSERT_READY");
"#;

const PROMPT_RELOAD_BOUNDARY_TS: &str = r#"
import session, { createTrigger } from "smudgy:core";

createTrigger(/^P$/, () => session.reload(), {
    name: "reload-inside-prompt-trigger",
    prompt: true,
});
"#;

#[derive(Clone, Copy, Debug)]
enum LocalOutput {
    Echo,
    Send,
    SendWithRedactions,
}

#[derive(Debug, PartialEq, Eq)]
enum ObservedUpdate {
    Append(String),
    BeginOpenLineReplacement,
    FinishOpenLineReplacement(Option<String>),
    EnsureNewLine,
    PromptBoundary,
    AppendTo(String),
    RetractOpenLine,
    Clear,
}

fn line(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(text, Vec::new()))
}

fn test_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();

    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("create test home");
        let path = home.path().to_path_buf();
        std::mem::forget(home);
        smudgy_core::set_smudgy_home(&path);
        path
    })
}

fn observe(update: &BufferUpdate) -> ObservedUpdate {
    match update {
        BufferUpdate::Append(line) => ObservedUpdate::Append(line.text.clone()),
        BufferUpdate::BeginOpenLineReplacement => ObservedUpdate::BeginOpenLineReplacement,
        BufferUpdate::FinishOpenLineReplacement(line) => {
            ObservedUpdate::FinishOpenLineReplacement(line.as_ref().map(|line| line.text.clone()))
        }
        BufferUpdate::EnsureNewLine => ObservedUpdate::EnsureNewLine,
        BufferUpdate::PromptBoundary => ObservedUpdate::PromptBoundary,
        BufferUpdate::AppendTo(_, line) => ObservedUpdate::AppendTo(line.text.clone()),
        BufferUpdate::RetractOpenLine => ObservedUpdate::RetractOpenLine,
        BufferUpdate::Clear(_) => ObservedUpdate::Clear,
    }
}

async fn run_case(
    session_number: u32,
    local_output: LocalOutput,
    completion_fragment: &str,
) -> Vec<ObservedUpdate> {
    let server = format!("FragmentedPromptLocalOutput{session_number}");
    std::fs::create_dir_all(test_home().join(&server).join("logs"))
        .expect("create test log directory");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_number),
        server_name: Arc::new(server),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Start after a committed marker. Startup output cannot affect the case.
    tx.send(RuntimeAction::Echo(Arc::new(BEGIN.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    'begin: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the begin marker")
            .expect("event stream ended before the begin marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == BEGIN) {
                    break 'begin;
                }
            }
        }
    }

    tx.send(RuntimeAction::HandleIncomingPartialLine(line(PROMPT)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut observed = Vec::new();
    'prompt: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the displayed prompt")
            .expect("event stream ended before the displayed prompt");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                observed.push(observe(update));
                if matches!(update, BufferUpdate::Append(line) if line.text == PROMPT) {
                    break 'prompt;
                }
            }
        }
    }

    match local_output {
        LocalOutput::Echo => tx
            .send(RuntimeAction::Echo(Arc::new(ECHO.to_string())))
            .unwrap(),
        LocalOutput::Send => tx
            .send(RuntimeAction::SendRaw(Arc::new(COMMAND.to_string())))
            .unwrap(),
        LocalOutput::SendWithRedactions => tx
            .send(RuntimeAction::SendWithRedactions {
                text: Arc::new(SECRET.to_string()),
                redactions: Arc::new(vec![SECRET.to_string()]),
            })
            .unwrap(),
    }
    tx.send(RuntimeAction::HandleIncomingFragmentedLine {
        line: line(&format!("{PROMPT}{completion_fragment}")),
        completion_fragment: line(completion_fragment),
    })
    .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new(END.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    'events: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the end marker")
            .expect("event stream ended before the end marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == END) {
                    break 'events;
                }
                observed.push(observe(update));
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();
    observed
}

fn rendered_rows(updates: &[ObservedUpdate]) -> Vec<String> {
    let mut rows = Vec::new();
    let mut open = String::new();

    for update in updates {
        match update {
            ObservedUpdate::Append(text) => open.push_str(text),
            ObservedUpdate::EnsureNewLine => rows.push(std::mem::take(&mut open)),
            ObservedUpdate::PromptBoundary => {}
            unexpected => panic!("unexpected presentation update: {unexpected:?}"),
        }
    }

    assert!(
        open.is_empty(),
        "the case left an unterminated row: {open:?}"
    );
    rows
}

#[tokio::test]
async fn echo_then_nonempty_completion_does_not_replay_the_prompt() {
    let updates = run_case(9211, LocalOutput::Echo, SUFFIX).await;

    assert_eq!(
        updates,
        vec![
            ObservedUpdate::Append(PROMPT.to_string()),
            ObservedUpdate::EnsureNewLine,
            ObservedUpdate::Append(ECHO.to_string()),
            ObservedUpdate::EnsureNewLine,
            ObservedUpdate::Append(SUFFIX.to_string()),
            ObservedUpdate::EnsureNewLine,
        ]
    );
    assert_eq!(rendered_rows(&updates), [PROMPT, ECHO, SUFFIX]);
}

#[tokio::test]
async fn echo_then_empty_completion_does_not_add_a_blank_or_replay_the_prompt() {
    let updates = run_case(9212, LocalOutput::Echo, "").await;

    assert_eq!(
        updates,
        vec![
            ObservedUpdate::Append(PROMPT.to_string()),
            ObservedUpdate::EnsureNewLine,
            ObservedUpdate::Append(ECHO.to_string()),
            ObservedUpdate::EnsureNewLine,
        ]
    );
    assert_eq!(rendered_rows(&updates), [PROMPT, ECHO]);
}

#[tokio::test]
async fn send_stays_glued_to_the_prompt_and_completion_does_not_replay_it() {
    let updates = run_case(9213, LocalOutput::Send, SUFFIX).await;

    assert_eq!(
        updates,
        vec![
            ObservedUpdate::Append(PROMPT.to_string()),
            ObservedUpdate::Append(COMMAND.to_string()),
            ObservedUpdate::EnsureNewLine,
            ObservedUpdate::Append(SUFFIX.to_string()),
            ObservedUpdate::EnsureNewLine,
        ]
    );
    assert_eq!(
        rendered_rows(&updates),
        [format!("{PROMPT}{COMMAND}"), SUFFIX.to_string()]
    );
}

#[tokio::test]
async fn send_then_empty_completion_does_not_add_a_blank_or_replay_the_prompt() {
    let updates = run_case(9214, LocalOutput::Send, "").await;

    assert_eq!(
        updates,
        vec![
            ObservedUpdate::Append(PROMPT.to_string()),
            ObservedUpdate::Append(COMMAND.to_string()),
            ObservedUpdate::EnsureNewLine,
        ]
    );
    assert_eq!(rendered_rows(&updates), [format!("{PROMPT}{COMMAND}")]);
}

#[tokio::test]
async fn redacted_send_stays_glued_and_does_not_leak_or_replay_the_prompt() {
    let updates = run_case(9215, LocalOutput::SendWithRedactions, SUFFIX).await;

    assert_eq!(
        updates,
        vec![
            ObservedUpdate::Append(PROMPT.to_string()),
            ObservedUpdate::Append(MASK.to_string()),
            ObservedUpdate::EnsureNewLine,
            ObservedUpdate::Append(SUFFIX.to_string()),
            ObservedUpdate::EnsureNewLine,
        ]
    );
    assert_eq!(
        rendered_rows(&updates),
        [format!("{PROMPT}{MASK}"), SUFFIX.to_string()]
    );
    assert!(
        !format!("{updates:?}").contains(SECRET),
        "the secret must not reach a buffer update"
    );
}

#[tokio::test]
async fn redacted_send_then_empty_completion_does_not_add_a_blank() {
    let updates = run_case(9216, LocalOutput::SendWithRedactions, "").await;

    assert_eq!(
        updates,
        vec![
            ObservedUpdate::Append(PROMPT.to_string()),
            ObservedUpdate::Append(MASK.to_string()),
            ObservedUpdate::EnsureNewLine,
        ]
    );
    assert_eq!(rendered_rows(&updates), [format!("{PROMPT}{MASK}")]);
    assert!(
        !format!("{updates:?}").contains(SECRET),
        "the secret must not reach a buffer update"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn prompt_trigger_echo_keeps_trigger_subjects_and_physical_rows_separate() {
    const SERVER: &str = "FragmentedPromptTriggerEcho";
    const FIRST: &str = "prefix ";
    const MATCH: &str = "MATCH";
    const REST: &str = " done";
    const WHOLE: &str = "prefix MATCH done";
    const PROMPT_ECHO: &str = "PROMPT:MATCH";
    const SUBJECTS: &str = "SUBJECTS:prefix MATCH done|prefix MATCH done";
    const BUFFER_ROWS: &str = concat!(
        "BUFFER:[\"prefix \",\"PROMPT:MATCH\",\"MATCH\",",
        "\"SUBJECTS:prefix MATCH done|prefix MATCH done\",\" done\"]"
    );

    let modules = test_home().join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create trigger module directory");
    std::fs::create_dir_all(test_home().join(SERVER).join("logs"))
        .expect("create trigger log directory");
    std::fs::write(modules.join("prompt-trigger.ts"), PROMPT_TRIGGER_TS)
        .expect("write trigger module");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9217),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the trigger module")
            .expect("event stream ended before the trigger module");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "PROMPT_TRIGGER_READY"),
            )
        {
            break;
        }
    }

    let mut observed = Vec::new();
    for fragment in [FIRST, MATCH] {
        tx.send(RuntimeAction::HandleIncomingPartialLine(line(fragment)))
            .unwrap();
        tx.send(RuntimeAction::RequestRepaint).unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(30), events.next())
                .await
                .expect("timed out waiting for a prompt fragment")
                .expect("event stream ended before a prompt fragment");
            if let SessionEvent::UpdateBuffer(updates) = event.event {
                let mut found = false;
                for update in updates.iter() {
                    observed.push(observe(update));
                    found |= matches!(update, BufferUpdate::Append(line) if line.text == fragment);
                }
                if found {
                    break;
                }
            }
        }
    }

    tx.send(RuntimeAction::HandleIncomingFragmentedLine {
        line: line(WHOLE),
        completion_fragment: line(REST),
    })
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(line("CHECK")))
        .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new(END.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    'events: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the trigger test end marker")
            .expect("event stream ended before the trigger test end marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == END) {
                    break 'events;
                }
                observed.push(observe(update));
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let rows = rendered_rows(&observed);
    assert_eq!(
        rows,
        [
            FIRST,
            PROMPT_ECHO,
            MATCH,
            SUBJECTS,
            REST,
            BUFFER_ROWS,
            "CHECK"
        ]
    );
    assert!(
        !observed
            .iter()
            .any(|update| matches!(update, ObservedUpdate::Append(text) if text == WHOLE)),
        "the assembled logical line must not be replayed on main: {observed:?}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn hidden_middle_partial_defers_later_fragments_without_replaying_committed_text() {
    const SERVER: &str = "FragmentedPromptHiddenMiddle";
    const FIRST: &str = "P";
    const HIDDEN: &str = "Q";
    const LATER: &str = "R";
    const REST: &str = "S";
    const WHOLE: &str = "PQRS";
    const LOCAL_ECHO: &str = "local echo";
    const SUBJECTS: &str = "GAP_SUBJECTS:PQRS|PQRS";

    let modules = test_home().join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create gapped-partial module directory");
    std::fs::create_dir_all(test_home().join(SERVER).join("logs"))
        .expect("create gapped-partial log directory");
    std::fs::write(modules.join("gapped-partial.ts"), GAPPED_PARTIAL_TS)
        .expect("write gapped-partial module");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9218),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the gapped-partial module")
            .expect("event stream ended before the gapped-partial module");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "GAPPED_PARTIAL_READY"),
            )
        {
            break;
        }
    }

    tx.send(RuntimeAction::HandleIncomingPartialLine(line(FIRST)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut observed = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the visible first partial")
            .expect("event stream ended before the visible first partial");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            let mut found = false;
            for update in updates.iter() {
                observed.push(observe(update));
                found |= matches!(update, BufferUpdate::Append(line) if line.text == FIRST);
            }
            if found {
                break;
            }
        }
    }

    // The local line commits P. Q then creates a visibility gap. R would normally return
    // to main, but it must remain deferred until completion can emit the unseen QRS once.
    tx.send(RuntimeAction::Echo(Arc::new(LOCAL_ECHO.to_string())))
        .unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(line(HIDDEN)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(line(LATER)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    tx.send(RuntimeAction::HandleIncomingFragmentedLine {
        line: line(WHOLE),
        completion_fragment: line(REST),
    })
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(line("CHECK_GAP")))
        .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new(END.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    'events: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the gapped-partial end marker")
            .expect("event stream ended before the gapped-partial end marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == END) {
                    break 'events;
                }
                observed.push(observe(update));
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let rows = rendered_rows(&observed);
    assert_eq!(rows, [FIRST, LOCAL_ECHO, "QRS", SUBJECTS, "CHECK_GAP"]);
    assert_eq!(
        rows.iter().filter(|row| row.as_str() == FIRST).count(),
        1,
        "the committed prefix must remain visible exactly once: {observed:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.as_str() == LOCAL_ECHO).count(),
        1,
        "the local echo must remain visible exactly once: {observed:?}"
    );
    assert!(
        !observed.iter().any(|update| matches!(
            update,
            ObservedUpdate::Append(text) if text == HIDDEN || text == LATER || text == WHOLE
        )),
        "hidden and deferred fragments must appear only in the completed unseen row: {observed:?}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn external_reload_does_not_make_a_committed_prefix_replay_on_completion() {
    const SERVER: &str = "FragmentedPromptExternalReload";
    const FIRST: &str = "P";
    const REST: &str = "S";
    const WHOLE: &str = "PS";
    const LOCAL_ECHO: &str = "local echo before reload";

    std::fs::create_dir_all(test_home().join(SERVER).join("logs"))
        .expect("create external-reload log directory");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9219),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let mut tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Commit startup output so P begins a physical row whose updates the test owns.
    tx.send(RuntimeAction::Echo(Arc::new(BEGIN.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    'begin: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the external-reload begin marker")
            .expect("event stream ended before the external-reload begin marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == BEGIN) {
                    break 'begin;
                }
            }
        }
    }

    tx.send(RuntimeAction::HandleIncomingPartialLine(line(FIRST)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut before_completion = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the external-reload partial")
            .expect("event stream ended before the external-reload partial");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            let mut found = false;
            for update in updates.iter() {
                before_completion.push(observe(update));
                found |= matches!(update, BufferUpdate::Append(line) if line.text == FIRST);
            }
            if found {
                break;
            }
        }
    }

    tx.send(RuntimeAction::Echo(Arc::new(LOCAL_ECHO.to_string())))
        .unwrap();
    tx.send(RuntimeAction::Reload).unwrap();

    // The old runtime flushes its local output and reload notice before publishing the new
    // sender. Preserve those rows for the prefix-count assertion, but do not require a notice.
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for post-reload RuntimeReady")
            .expect("event stream ended before post-reload RuntimeReady");
        match event.event {
            SessionEvent::UpdateBuffer(updates) => {
                before_completion.extend(updates.iter().map(observe));
            }
            SessionEvent::RuntimeReady(reloaded_tx) => {
                tx = reloaded_tx;
                break;
            }
            _ => {}
        }
    }

    let rows_before_completion = rendered_rows(&before_completion);
    assert_eq!(
        rows_before_completion
            .iter()
            .filter(|row| row.as_str() == FIRST)
            .count(),
        1,
        "reload must not replay the committed prefix: {before_completion:?}"
    );
    assert_eq!(
        rows_before_completion
            .iter()
            .filter(|row| row.as_str() == LOCAL_ECHO)
            .count(),
        1,
        "reload must preserve the local echo exactly once: {before_completion:?}"
    );

    tx.send(RuntimeAction::HandleIncomingFragmentedLine {
        line: line(WHOLE),
        completion_fragment: line(REST),
    })
    .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new(END.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut completion_updates = Vec::new();
    'events: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the external-reload end marker")
            .expect("event stream ended before the external-reload end marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == END) {
                    break 'events;
                }
                completion_updates.push(observe(update));
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    assert_eq!(rendered_rows(&completion_updates), [REST]);
    assert!(
        !completion_updates
            .iter()
            .any(|update| matches!(update, ObservedUpdate::Append(text) if text == WHOLE)),
        "completion after reload must emit only the unseen suffix: {completion_updates:?}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn prompt_boundary_exposes_only_visible_fragments_after_a_hidden_gap() {
    const SERVER: &str = "FragmentedPromptBoundaryGap";
    const FIRST: &str = "P";
    const HIDDEN: &str = "Q";
    const LATER: &str = "R";
    const LOCAL_ECHO: &str = "local echo before prompt gap";
    const CHECK: &str = "commit exposed prompt tail";

    let modules = test_home().join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create boundary-gap module directory");
    std::fs::create_dir_all(test_home().join(SERVER).join("logs"))
        .expect("create boundary-gap log directory");
    std::fs::write(modules.join("boundary-gap.ts"), BOUNDARY_GAP_TS)
        .expect("write boundary-gap module");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9220),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the boundary-gap module")
            .expect("event stream ended before the boundary-gap module");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "BOUNDARY_GAP_READY"),
            )
        {
            break;
        }
    }

    tx.send(RuntimeAction::HandleIncomingPartialLine(line(FIRST)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut observed = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the first boundary-gap partial")
            .expect("event stream ended before the first boundary-gap partial");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            let mut found = false;
            for update in updates.iter() {
                observed.push(observe(update));
                found |= matches!(update, BufferUpdate::Append(line) if line.text == FIRST);
            }
            if found {
                break;
            }
        }
    }

    tx.send(RuntimeAction::Echo(Arc::new(LOCAL_ECHO.to_string())))
        .unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(line(HIDDEN)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    tx.send(RuntimeAction::HandleIncomingPartialLine(line(LATER)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    tx.send(RuntimeAction::PromptBoundary).unwrap();
    // This echo commits the exposed R tail. Without it, R deliberately remains open.
    tx.send(RuntimeAction::Echo(Arc::new(CHECK.to_string())))
        .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new(END.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    'events: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the boundary-gap end marker")
            .expect("event stream ended before the boundary-gap end marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == END) {
                    break 'events;
                }
                observed.push(observe(update));
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let rows = rendered_rows(&observed);
    assert_eq!(rows, [FIRST, LOCAL_ECHO, LATER, CHECK]);
    assert_eq!(
        rows.iter().filter(|row| row.as_str() == FIRST).count(),
        1,
        "the committed prefix must survive the boundary exactly once: {observed:?}"
    );
    assert!(
        !observed
            .iter()
            .any(|update| matches!(update, ObservedUpdate::Append(text) if text == HIDDEN)),
        "the gagged fragment must stay hidden at the prompt boundary: {observed:?}"
    );
    assert_eq!(
        observed
            .iter()
            .filter(|update| matches!(update, ObservedUpdate::Append(text) if text == LATER))
            .count(),
        1,
        "the later main-visible fragment must be exposed once: {observed:?}"
    );

    let later_index = observed
        .iter()
        .position(|update| matches!(update, ObservedUpdate::Append(text) if text == LATER))
        .expect("the later fragment must be exposed");
    let boundary_index = observed
        .iter()
        .position(|update| matches!(update, ObservedUpdate::PromptBoundary))
        .expect("the prompt boundary must reach the terminal");
    let commit_index = observed
        .iter()
        .enumerate()
        .skip(boundary_index + 1)
        .find_map(|(index, update)| {
            matches!(update, ObservedUpdate::EnsureNewLine).then_some(index)
        })
        .expect("the check echo must commit the exposed prompt tail");
    assert!(
        later_index < boundary_index && boundary_index < commit_index,
        "R must remain an open tail until the check echo commits it: {observed:?}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn complete_line_insert_does_not_replay_a_committed_prefix() {
    const SERVER: &str = "FragmentedPromptCommittedInsert";
    const FIRST: &str = "A";
    const REST: &str = "AQ";
    const WHOLE: &str = "AAQ";
    const TRANSFORMED_WHOLE: &str = "AAAQ";
    const LOCAL_ECHO: &str = "local echo before insert";
    const SUBJECTS: &str = "INSERT_SUBJECTS:AAQ|AAQ";

    let modules = test_home().join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create committed-insert module directory");
    std::fs::create_dir_all(test_home().join(SERVER).join("logs"))
        .expect("create committed-insert log directory");
    std::fs::write(modules.join("committed-insert.ts"), COMMITTED_INSERT_TS)
        .expect("write committed-insert module");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9221),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the committed-insert module")
            .expect("event stream ended before the committed-insert module");
        if let SessionEvent::UpdateBuffer(updates) = event.event
            && updates.iter().any(
                |update| matches!(update, BufferUpdate::Append(line) if line.text == "COMMITTED_INSERT_READY"),
            )
        {
            break;
        }
    }

    tx.send(RuntimeAction::HandleIncomingPartialLine(line(FIRST)))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let mut observed = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the committed prefix")
            .expect("event stream ended before the committed prefix");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            let mut found = false;
            for update in updates.iter() {
                observed.push(observe(update));
                found |= matches!(update, BufferUpdate::Append(line) if line.text == FIRST);
            }
            if found {
                break;
            }
        }
    }

    tx.send(RuntimeAction::Echo(Arc::new(LOCAL_ECHO.to_string())))
        .unwrap();
    tx.send(RuntimeAction::HandleIncomingFragmentedLine {
        line: line(WHOLE),
        completion_fragment: line(REST),
    })
    .unwrap();
    tx.send(RuntimeAction::HandleIncomingLine(line("CHECK_INSERT")))
        .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new(END.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    'events: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the committed-insert end marker")
            .expect("event stream ended before the committed-insert end marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == END) {
                    break 'events;
                }
                observed.push(observe(update));
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let rows = rendered_rows(&observed);
    assert_eq!(rows, [FIRST, LOCAL_ECHO, REST, SUBJECTS, "CHECK_INSERT"]);
    assert_eq!(
        rows.iter().filter(|row| row.as_str() == FIRST).count(),
        1,
        "the inserted A must not replay the committed A: {observed:?}"
    );
    assert!(
        !observed.iter().any(|update| matches!(
            update,
            ObservedUpdate::Append(text) if text == WHOLE || text == TRANSFORMED_WHOLE
        )),
        "main must receive only the original unseen suffix: {observed:?}"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn prompt_trigger_reload_recovers_the_in_flight_partial_before_prompt_boundary() {
    const SERVER: &str = "FragmentedPromptTriggerReloadBoundary";
    const PARTIAL: &str = "P";
    const CHECK: &str = "observe recovered prompt";

    let modules = test_home().join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create prompt-reload module directory");
    std::fs::create_dir_all(test_home().join(SERVER).join("logs"))
        .expect("create prompt-reload log directory");
    std::fs::write(
        modules.join("prompt-reload-boundary.ts"),
        PROMPT_RELOAD_BOUNDARY_TS,
    )
    .expect("write prompt-reload module");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9222),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events = Box::pin(spawn(params));
    let mut tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // Commit startup output without adding any module output that could run again on reload.
    tx.send(RuntimeAction::Echo(Arc::new(BEGIN.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    'begin: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the prompt-reload begin marker")
            .expect("event stream ended before the prompt-reload begin marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == BEGIN) {
                    break 'begin;
                }
            }
        }
    }

    // PromptBoundary is queued behind the partial. The prompt trigger inserts Reload ahead
    // of PartialLineTriggersProcessed, so recovery must consume partial_line_in_flight itself.
    tx.send(RuntimeAction::HandleIncomingPartialLine(line(PARTIAL)))
        .unwrap();
    tx.send(RuntimeAction::PromptBoundary).unwrap();

    let mut observed = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for post-reload RuntimeReady")
            .expect("event stream ended before post-reload RuntimeReady");
        match event.event {
            SessionEvent::UpdateBuffer(updates) => {
                observed.extend(updates.iter().map(observe));
            }
            SessionEvent::RuntimeReady(reloaded_tx) => {
                tx = reloaded_tx;
                break;
            }
            _ => {}
        }
    }

    // The preserved PromptBoundary is ahead of these actions. CHECK and END supply a
    // deterministic observation barrier after the rebuilt runtime processes it.
    tx.send(RuntimeAction::Echo(Arc::new(CHECK.to_string())))
        .unwrap();
    tx.send(RuntimeAction::Echo(Arc::new(END.to_string())))
        .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    'events: loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the prompt-reload end marker")
            .expect("event stream ended before the prompt-reload end marker");
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if matches!(update, BufferUpdate::Append(line) if line.text == END) {
                    break 'events;
                }
                observed.push(observe(update));
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let rows = rendered_rows(&observed);
    assert_eq!(
        rows.iter().filter(|row| row.as_str() == PARTIAL).count(),
        1,
        "the in-flight partial must be recovered exactly once: {observed:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.as_str() == CHECK).count(),
        1,
        "the check echo must be displayed exactly once: {observed:?}"
    );

    let partial_index = observed
        .iter()
        .position(|update| matches!(update, ObservedUpdate::Append(text) if text == PARTIAL))
        .expect("the recovered partial must reach main");
    let boundary_index = observed
        .iter()
        .position(|update| matches!(update, ObservedUpdate::PromptBoundary))
        .expect("the queued prompt boundary must survive reload");

    assert!(
        partial_index < boundary_index,
        "the in-flight partial must be routed once before the preserved PromptBoundary: {observed:?}"
    );
}
