//! The scripted transport surface (`session.connect()` / `session.disconnect()`) and what
//! happens to a command that cannot be sent: the typed line comes back to the input, a
//! notice in the client's own voice says what the client is doing about it, and a session
//! that still means to be online is reconnected.
//!
//! Connecting reaches the daemon as a session event rather than acting on the socket
//! directly: the server and profile configurations are re-read on every connect (and the
//! `$PASSWORD` token re-substituted from the keyring), and the session's online intent lives
//! with the daemon too. These tests therefore assert on the events the runtime emits, and
//! stand in for the daemon where a case needs its answer.

use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::settings::ScriptSettings;
use smudgy_core::session::connection::{InboundCompression, TlsMode};
use smudgy_core::session::runtime::input::InputOp;
use smudgy_core::session::runtime::pane::MAIN_PANE_KEY;
use smudgy_core::session::runtime::{RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};
use tokio::sync::mpsc::UnboundedSender;

const TIMEOUT: Duration = Duration::from_mins(1);

/// The module every case loads: aliases that drive the transport verbs, each followed by a
/// marker echo, plus the end-of-case marker. The alias and the echo ride the same ordered
/// queue as the action the verb routes, so seeing a marker means the verb's own dispatch has
/// already happened — which is what lets a case assert that *no* event was emitted.
///
/// The markers are echoes rather than the sent text itself: a send that fails reports the
/// failure and stops, so it never reaches the local echo a marker would depend on.
const MODULE: &str = r#"
import session, { createAlias, echo, sendRaw } from "smudgy:core";
createAlias("^probe-connect$", () => { session.session.connect(); echo("CONNECT_PROBED"); });
createAlias("^probe-disconnect$", () => { session.session.disconnect(); echo("DISCONNECT_PROBED"); });
createAlias("^transport-done$", () => { echo("TRANSPORT_DONE"); });
createAlias("^script-send$", () => { sendRaw("scripted payload"); echo("SCRIPT_SENT"); });
createAlias("^kd$", () => { sendRaw("kill dragon"); echo("KD_RAN"); });
"#;

/// What a case cares about, flattened out of the event stream in arrival order.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Line(String),
    /// A row in the client's own voice, appended.
    System(String),
    /// A row in the client's own voice, rewritten in place.
    SystemReplaced(String),
    Connected,
    Disconnected,
    ConnectRequested {
        only_if_intended: bool,
    },
    DisconnectRequested,
    Propose(String),
}

fn collect(event: &SessionEvent, seen: &mut Vec<Seen>) {
    match event {
        SessionEvent::UpdateBuffer(updates) => {
            for update in updates.iter() {
                match update {
                    BufferUpdate::Append(line) => seen.push(Seen::Line(line.text.clone())),
                    BufferUpdate::AppendSystem(row) => {
                        seen.push(Seen::System(row.line.text.clone()));
                    }
                    BufferUpdate::ReplaceSystem(row) => {
                        seen.push(Seen::SystemReplaced(row.line.text.clone()));
                    }
                    _ => {}
                }
            }
        }
        SessionEvent::Connected => seen.push(Seen::Connected),
        SessionEvent::Disconnected => seen.push(Seen::Disconnected),
        SessionEvent::ConnectRequested { only_if_intended } => seen.push(Seen::ConnectRequested {
            only_if_intended: *only_if_intended,
        }),
        SessionEvent::DisconnectRequested => seen.push(Seen::DisconnectRequested),
        SessionEvent::InputOp {
            key,
            op: InputOp::Propose(text),
        } if *key == MAIN_PANE_KEY => seen.push(Seen::Propose(text.to_string())),
        _ => {}
    }
}

fn has_line(seen: &[Seen], needle: &str) -> bool {
    seen.iter()
        .any(|entry| matches!(entry, Seen::Line(line) if line.contains(needle)))
}

/// Whether any system row — as appended or as later rewritten — contained `needle`.
fn has_system(seen: &[Seen], needle: &str) -> bool {
    seen.iter().any(|entry| {
        matches!(entry, Seen::System(text) | Seen::SystemReplaced(text) if text.contains(needle))
    })
}

fn count_system_appends(seen: &[Seen], needle: &str) -> usize {
    seen.iter()
        .filter(|entry| matches!(entry, Seen::System(text) if text.contains(needle)))
        .count()
}

fn count_connect_requests(seen: &[Seen]) -> usize {
    seen.iter()
        .filter(|entry| matches!(entry, Seen::ConnectRequested { .. }))
        .count()
}

fn proposals(seen: &[Seen]) -> Vec<&str> {
    seen.iter()
        .filter_map(|entry| match entry {
            Seen::Propose(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn prepare(id: u32) -> Arc<SessionParams> {
    let home = tempfile::tempdir().unwrap();
    smudgy_core::set_smudgy_home(home.path());
    // The process-wide home retains its first value across parallel tests.
    std::mem::forget(home);
    let server = format!("test_transport_{id}");
    let root = smudgy_core::get_smudgy_home().unwrap().join(&server);
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::create_dir_all(root.join("modules")).unwrap();
    std::fs::write(root.join("modules/transport.ts"), MODULE).unwrap();
    Arc::new(SessionParams {
        session_id: SessionId::from(id),
        server_name: Arc::new(server),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })
}

fn apply_settings(tx: &UnboundedSender<RuntimeAction>, reconnect_on_send_error: bool) {
    tx.send(RuntimeAction::ApplySettings {
        command_separator: Arc::new(";".to_string()),
        raw_line_prefix: Arc::new("\\".to_string()),
        log_enabled: false,
        bold_is_bright: false,
        reconnect_on_send_error,
        script_settings: Box::new(ScriptSettings::default()),
    })
    .unwrap();
}

/// What the daemon sends when it answers a connect: the loaded configuration as a `Connect`.
fn connect_action(port: u16, tls: TlsMode) -> RuntimeAction {
    RuntimeAction::Connect {
        host: Arc::new("127.0.0.1".to_string()),
        port,
        send_on_connect: None,
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: None,
        compression: InboundCompression::NONE,
        tls,
    }
}

/// One case: bring a session up (against a real socket, unless `connect_first` is off — an
/// offline session), hand control to `drive`, and return everything the session emitted
/// from `RuntimeReady` onward.
async fn transport_case<F, Fut>(id: u32, connect_first: bool, drive: F) -> Vec<Seen>
where
    F: FnOnce(UnboundedSender<RuntimeAction>, Arc<TestServer>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let params = prepare(id);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // An offline case never connects, so no peer waits on an accept that
    // would never come.
    let server = Arc::new(if connect_first {
        TestServer::spawn(listener)
    } else {
        TestServer::idle()
    });

    let mut events = Box::pin(spawn(params));
    let mut seen = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap()
            .unwrap();
        collect(&event.event, &mut seen);
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    if connect_first {
        tx.send(connect_action(port, TlsMode::Off)).unwrap();
    }

    // `drive` only queues actions and tells the peer when to drop, so it runs beside the
    // pump below; the pump is what actually advances the stream. Its final marker is the
    // stop condition, which is what lets a case assert that some event never arrived.
    let driver = tokio::spawn({
        let tx = tx.clone();
        let server = Arc::clone(&server);
        async move { drive(tx, server).await }
    });

    loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for the next event: {seen:#?}"))
            .unwrap();
        collect(&event.event, &mut seen);
        if has_line(&seen, "TRANSPORT_DONE") {
            break;
        }
    }
    driver.await.unwrap();

    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(events);
    let session_id = SessionId::from(id);
    assert_eq!(
        tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
            .await
            .unwrap(),
        RuntimeThreadJoinOutcome::Clean { session_id }
    );
    server.join();
    seen
}

/// The peer: accepts one connection, drains it, and drops the socket on request.
struct TestServer {
    drop_tx: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    handle: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl TestServer {
    fn spawn(listener: TcpListener) -> Self {
        let (drop_tx, drop_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let mut sink = [0u8; 1024];
            loop {
                // Drain whatever the client sends so a full buffer can never be what
                // wedges a write; the only teardown here is the requested one.
                if let Ok(0) = socket.read(&mut sink) {
                    return;
                }
                if drop_rx.try_recv().is_ok() {
                    socket.shutdown(std::net::Shutdown::Both).ok();
                    return;
                }
            }
        });
        Self {
            drop_tx: std::sync::Mutex::new(Some(drop_tx)),
            handle: std::sync::Mutex::new(Some(handle)),
        }
    }

    /// A peer nobody connects to.
    fn idle() -> Self {
        Self {
            drop_tx: std::sync::Mutex::new(None),
            handle: std::sync::Mutex::new(None),
        }
    }

    fn drop_connection(&self) {
        if let Some(tx) = self.drop_tx.lock().unwrap().take() {
            tx.send(()).ok();
        }
    }

    fn join(&self) {
        self.drop_connection();
        if let Some(handle) = self.handle.lock().unwrap().take() {
            handle.join().unwrap();
        }
    }
}

/// A line arriving as `Send`: a link click, a cross-session `session.send()`, the auto-login.
fn send(tx: &UnboundedSender<RuntimeAction>, line: &str) {
    tx.send(RuntimeAction::Send(Arc::new(line.to_string())))
        .unwrap();
}

/// A line the user typed and submitted with Enter.
fn submit(tx: &UnboundedSender<RuntimeAction>, line: &str) {
    tx.send(RuntimeAction::SubmitInput(Arc::new(line.to_string())))
        .unwrap();
}

/// Let the session thread work through everything queued so far. The driver runs beside
/// the case's event pump, so this is simply time for the runtime to catch up; each assertion
/// is anchored on a marker rather than on this delay.
async fn settle() {
    settle_for(250).await;
}

async fn settle_for(millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

/// `connect()` on a connected session and `disconnect()` on a disconnected one both do
/// nothing: the verbs are safe to call unconditionally.
#[tokio::test]
async fn connect_and_disconnect_are_no_ops_against_the_state_already_held() {
    let seen = transport_case(9871, true, |tx, server| async move {
        // Connected: `connect()` must not ask the daemon for anything.
        settle().await;
        send(&tx, "probe-connect");
        settle().await;

        // Still connected: `disconnect()` must ask exactly once.
        send(&tx, "probe-disconnect");
        settle().await;
        // Stand in for the daemon's answer, and let the peer go.
        tx.send(RuntimeAction::Disconnect).unwrap();
        server.drop_connection();
        settle().await;

        // Disconnected: `disconnect()` must not ask again, `connect()` must.
        send(&tx, "probe-disconnect");
        settle().await;
        send(&tx, "probe-connect");
        settle().await;

        send(&tx, "transport-done");
    })
    .await;

    let markers: Vec<&Seen> = seen
        .iter()
        .filter(|entry| {
            matches!(entry, Seen::Line(line)
                if line.contains("PROBED") || line.contains("TRANSPORT_DONE"))
                || !matches!(
                    entry,
                    Seen::Line(_) | Seen::System(_) | Seen::SystemReplaced(_)
                )
        })
        .collect();

    assert_eq!(
        markers,
        vec![
            &Seen::Connected,
            // connect() while connected: nothing between the marker and the one before it.
            &Seen::Line("CONNECT_PROBED".to_string()),
            &Seen::DisconnectRequested,
            &Seen::Line("DISCONNECT_PROBED".to_string()),
            &Seen::Disconnected,
            // disconnect() while disconnected: nothing.
            &Seen::Line("DISCONNECT_PROBED".to_string()),
            &Seen::ConnectRequested {
                only_if_intended: false
            },
            &Seen::Line("CONNECT_PROBED".to_string()),
            &Seen::Line("TRANSPORT_DONE".to_string()),
        ],
        "transcript:\n{seen:#?}"
    );
}

/// A typed line that fails after the connection is gone comes back to the input fully
/// selected, the notice says the client is reconnecting, and the daemon is asked to —
/// conditionally, so it can still refuse on behalf of a user who disconnected deliberately.
/// The bare "Send error" line is gone.
#[tokio::test]
async fn a_failed_typed_send_comes_back_and_reconnects() {
    let seen = transport_case(9873, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        submit(&tx, "kill dragon");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert_eq!(
        proposals(&seen),
        vec!["kill dragon"],
        "the typed line must come back to the input, once; transcript:\n{seen:#?}"
    );
    assert!(
        seen.contains(&Seen::ConnectRequested {
            only_if_intended: true
        }),
        "the reconnect must defer to the session's online intent; transcript:\n{seen:#?}"
    );
    assert!(
        has_system(
            &seen,
            "You were disconnected. kill dragon could not be sent. Reconnecting"
        ),
        "the notice names the line and says what is happening; transcript:\n{seen:#?}"
    );
    assert!(
        !has_line(&seen, "Send error"),
        "the raw error line is replaced by the notice; transcript:\n{seen:#?}"
    );
}

/// Every command on a separator-split line is sent on its own, so one dead connection
/// produces one failure per command. What comes back is the line as typed — the whole of
/// it, once — and the daemon is asked once; the later failures keep the pending notice as
/// it is rather than stacking rows or downgrading it.
#[tokio::test]
async fn a_line_of_several_commands_comes_back_whole() {
    let seen = transport_case(9877, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        submit(&tx, "north;south;east");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert_eq!(
        proposals(&seen),
        vec!["north;south;east"],
        "the whole typed line comes back, once; transcript:\n{seen:#?}"
    );
    assert_eq!(
        count_connect_requests(&seen),
        1,
        "one dial for the line; transcript:\n{seen:#?}"
    );
    assert_eq!(
        count_system_appends(&seen, "could not be sent"),
        1,
        "one notice for the line; transcript:\n{seen:#?}"
    );
    assert!(
        !has_system(&seen, "Not connected"),
        "the later failures must not downgrade the pending notice; transcript:\n{seen:#?}"
    );
    assert!(
        has_system(&seen, "north;south;east could not be sent"),
        "the notice names the whole typed line; transcript:\n{seen:#?}"
    );
}

/// An alias the user typed whose body sends fails inside the typed submission, so what
/// comes back is the alias line the user typed — never the body's payload.
#[tokio::test]
async fn a_typed_alias_whose_script_send_fails_hands_back_the_alias_line() {
    let seen = transport_case(9879, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        submit(&tx, "script-send");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert!(has_line(&seen, "SCRIPT_SENT"), "{seen:#?}");
    assert_eq!(
        proposals(&seen),
        vec!["script-send"],
        "what the user typed comes back, not what the alias sent; transcript:\n{seen:#?}"
    );
}

/// A send outside any typed submission — an automation on a timer, here the raw send action
/// an automation body produces — was never in the input and never goes there. It still
/// reports, and still reconnects, but the notice does not claim a command is in the input.
#[tokio::test]
async fn a_send_outside_any_submission_never_touches_the_input() {
    let seen = transport_case(9881, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        tx.send(RuntimeAction::SendRaw(Arc::new(
            "scripted payload".to_string(),
        )))
        .unwrap();
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert!(
        proposals(&seen).is_empty(),
        "a script's payload must never be proposed into the input; transcript:\n{seen:#?}"
    );
    assert!(
        seen.contains(&Seen::ConnectRequested {
            only_if_intended: true
        }),
        "the reconnect still happens; transcript:\n{seen:#?}"
    );
    assert!(
        has_system(
            &seen,
            "A script tried to send scripted payload, but it was dropped. Reconnecting"
        ),
        "the notice names the script's text; transcript:\n{seen:#?}"
    );
}

/// Every send that fails while the client reconnects gets a row of its own, in the order the
/// sends failed, and all of them settle together when the attempt ends — a trigger firing
/// three times during a reconnect is three lines, not one. None of them dials again.
#[tokio::test]
async fn sends_failing_during_a_reconnect_stack_in_order_and_settle_together() {
    let seen = transport_case(9897, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        for payload in ["first", "second", "third"] {
            tx.send(RuntimeAction::SendRaw(Arc::new(payload.to_string())))
                .unwrap();
        }
        settle().await;
        // The attempt the first failure asked for comes up.
        tx.send(RuntimeAction::Connected).unwrap();
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    let appended: Vec<&str> = seen
        .iter()
        .filter_map(|entry| match entry {
            Seen::System(text) if text.starts_with("A script tried to send") => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        appended,
        vec![
            "A script tried to send first, but it was dropped. Reconnecting\u{2026}",
            "A script tried to send second, but it was dropped. Reconnecting\u{2026}",
            "A script tried to send third, but it was dropped. Reconnecting\u{2026}",
        ],
        "one row per failed send, in order; transcript:\n{seen:#?}"
    );
    let settled: Vec<&str> = seen
        .iter()
        .filter_map(|entry| match entry {
            Seen::SystemReplaced(text) if text.starts_with("A script tried to send") => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        settled,
        vec![
            "A script tried to send first, but it was dropped.",
            "A script tried to send second, but it was dropped.",
            "A script tried to send third, but it was dropped.",
        ],
        "every pending row settles, in order; transcript:\n{seen:#?}"
    );
    assert_eq!(count_connect_requests(&seen), 1, "transcript:\n{seen:#?}");
}

/// A second line typed while the client is still reconnecting for the first stacks its own
/// row and does not start a second attempt.
#[tokio::test]
async fn a_second_enter_during_a_reconnect_stacks_without_a_second_dial() {
    let seen = transport_case(9899, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        submit(&tx, "north");
        settle().await;
        submit(&tx, "south");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert_eq!(
        proposals(&seen),
        vec!["north", "south"],
        "transcript:\n{seen:#?}"
    );
    assert_eq!(
        count_system_appends(&seen, "could not be sent"),
        2,
        "transcript:\n{seen:#?}"
    );
    assert!(
        has_system(
            &seen,
            "You were disconnected. south could not be sent. Reconnecting"
        ),
        "transcript:\n{seen:#?}"
    );
    assert_eq!(count_connect_requests(&seen), 1, "transcript:\n{seen:#?}");
}

/// An alias is a user command however it is reached. A send that fails inside an alias's
/// expansion hands the command that matched the alias back to the input even when nothing
/// was typed (a trigger's or link's `send("kd")`), while the same send with no alias in the
/// chain is reported as a script's and leaves the input alone.
#[tokio::test]
async fn an_alias_reached_without_typing_still_hands_back_its_command() {
    let seen = transport_case(9895, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        // Not typed: the route a trigger's or a link's send takes.
        send(&tx, "kd");
        settle().await;
        send(&tx, "look");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert!(has_line(&seen, "KD_RAN"), "transcript:\n{seen:#?}");
    assert_eq!(
        proposals(&seen),
        vec!["kd"],
        "the alias's command comes back, the plain send does not; transcript:\n{seen:#?}"
    );
    assert!(
        has_system(
            &seen,
            "You were disconnected. kd could not be sent. Reconnecting"
        ),
        "transcript:\n{seen:#?}"
    );
    assert!(
        !has_system(&seen, "A script tried to send kd") && !has_system(&seen, "kill dragon"),
        "the alias line is named, not its body's payload; transcript:\n{seen:#?}"
    );
}

/// The client dials on its own once per stretch of failures, and only the user's Enter (or a
/// send getting through) lets it dial again. A peer that accepts, greets, then resets fails
/// the profile's auto-login on every attempt; the transport coming up therefore does not
/// release the latch, so that cannot become an unbounded redial loop — while the user
/// pressing Enter again does, so a dead server is redialed at the user's pace.
#[tokio::test]
async fn the_client_dials_once_per_submission() {
    let seen = transport_case(9883, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        // An automatic send fails: one dial.
        tx.send(RuntimeAction::SendRaw(Arc::new("auto-login".to_string())))
            .unwrap();
        settle().await;
        // The transport comes up (a stand-in for the socket task's report)...
        tx.send(RuntimeAction::Connected).unwrap();
        settle().await;
        // ...and the next automatic send fails again: no second dial.
        tx.send(RuntimeAction::SendRaw(Arc::new("auto-login".to_string())))
            .unwrap();
        settle().await;
        // The user presses Enter: that is the pace.
        submit(&tx, "look");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    let requests: Vec<usize> = seen
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            matches!(entry, Seen::ConnectRequested { .. }).then_some(index)
        })
        .collect();
    let reconnected = seen
        .iter()
        .position(|entry| {
            matches!(entry, Seen::SystemReplaced(text)
                if text == "A script tried to send auto-login, but it was dropped.")
        })
        .expect("the transport coming up settles the notice on the fact, with nothing more to say");
    // The reconnect opened a fresh "Connected to …" rule under the settled
    // notice, so this failure's notice is a new row rather than a rewrite.
    let declined = seen
        .iter()
        .position(|entry| {
            matches!(entry, Seen::System(text) | Seen::SystemReplaced(text)
                if text.contains("Not connected"))
        })
        .expect("the second automatic failure reports without dialing");
    assert_eq!(requests.len(), 2, "transcript:\n{seen:#?}");
    assert!(
        requests[0] < reconnected && reconnected < declined && declined < requests[1],
        "one dial, settled, then no dial for the automatic send, then one for the user's Enter; transcript:\n{seen:#?}"
    );
    assert_eq!(proposals(&seen), vec!["look"], "transcript:\n{seen:#?}");
}

/// A raw-prefixed line reaches the wire with its prefix stripped, but what comes back to
/// the input is what the user typed: handing back the stripped text would turn a verbatim
/// line into one that separator-splits and alias-matches on re-submission.
#[tokio::test]
async fn a_raw_prefixed_line_comes_back_as_typed() {
    let seen = transport_case(9885, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        submit(&tx, r"\say a;b");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert_eq!(
        proposals(&seen),
        vec![r"\say a;b"],
        "transcript:\n{seen:#?}"
    );
}

/// With the preference off, the typed line still comes back — losing it would help nobody —
/// but nothing dials: the notice offers the reconnect as a link instead.
#[tokio::test]
async fn with_the_preference_off_the_line_comes_back_but_nothing_dials() {
    let seen = transport_case(9875, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, false);
        server.drop_connection();
        settle().await;
        submit(&tx, "kill dragon");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert_eq!(
        proposals(&seen),
        vec!["kill dragon"],
        "transcript:\n{seen:#?}"
    );
    assert_eq!(count_connect_requests(&seen), 0, "transcript:\n{seen:#?}");
    assert!(
        has_system(&seen, "Not connected. kill dragon was not sent. Reconnect"),
        "the notice offers the reconnect as a link; transcript:\n{seen:#?}"
    );
}

/// A session the user disconnected stays disconnected: the typed line comes back with a
/// Reconnect link, and the client does not dial on its own.
#[tokio::test]
async fn a_session_the_user_disconnected_stays_offline() {
    let seen = transport_case(9887, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        // The title-bar Disconnect, as the daemon relays it.
        tx.send(RuntimeAction::Disconnect).unwrap();
        server.drop_connection();
        settle().await;
        submit(&tx, "look");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert_eq!(proposals(&seen), vec!["look"], "transcript:\n{seen:#?}");
    assert_eq!(count_connect_requests(&seen), 0, "transcript:\n{seen:#?}");
    assert!(
        has_system(&seen, "Not connected. look was not sent. Reconnect"),
        "transcript:\n{seen:#?}"
    );
}

/// A session opened offline keeps behaving as it always has: a typed command is echoed as
/// if sent, nothing is handed back, no notice appears, and nothing dials — the offline
/// session is a place to work on maps and automations without a live server.
#[tokio::test]
async fn a_session_opened_offline_keeps_echoing_commands() {
    let seen = transport_case(9889, false, |tx, _server| async move {
        settle().await;
        apply_settings(&tx, true);
        submit(&tx, "look");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert!(has_line(&seen, "look"), "transcript:\n{seen:#?}");
    assert!(proposals(&seen).is_empty(), "transcript:\n{seen:#?}");
    assert_eq!(count_connect_requests(&seen), 0, "transcript:\n{seen:#?}");
    assert!(!has_system(&seen, "not be sent"), "transcript:\n{seen:#?}");
}

/// When the reconnect the notice waited on fails, the notice settles in place with a Try
/// again link, and the connection rule that attempt opened reads "Reconnecting to …". The
/// next Enter dials again: a dead server is redialed at the user's pace, never faster.
#[tokio::test]
async fn a_failed_reconnect_settles_the_notice_and_the_next_enter_dials_again() {
    let seen = transport_case(9891, true, |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        submit(&tx, "look");
        settle().await;
        // The daemon answers with a connect to a port nobody listens on.
        let refused = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = refused.local_addr().unwrap().port();
        drop(refused);
        tx.send(connect_action(port, TlsMode::Off)).unwrap();
        // A refused loopback connect takes Windows about a second to report.
        settle_for(3000).await;
        submit(&tx, "look");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;

    assert!(
        has_system(&seen, "Reconnecting to 127.0.0.1:"),
        "the attempt's rule says it is a reconnect; transcript:\n{seen:#?}"
    );
    assert!(
        seen.iter()
            .any(|entry| matches!(entry, Seen::SystemReplaced(text)
            if text.contains("Couldn\u{2019}t reconnect. look was not sent.")
                && text.contains("Try again"))),
        "the notice settles in place with the retry link; transcript:\n{seen:#?}"
    );
    assert_eq!(
        count_connect_requests(&seen),
        2,
        "the second Enter dials again; transcript:\n{seen:#?}"
    );
}

/// A send that fails while a connection attempt is still under way waits on that attempt
/// rather than starting another (a fresh connect would cancel the one in flight), and so
/// does `session.connect()`. The attempt here is a TLS handshake against a peer that accepts
/// and never answers, which holds the dial open for as long as the case needs.
#[tokio::test]
async fn a_send_during_a_connection_attempt_waits_on_it() {
    // Held for the whole case: the accept queue completes the TCP connect, and the absent
    // ServerHello leaves the handshake — and so the attempt — in flight.
    let stall = TcpListener::bind("127.0.0.1:0").unwrap();
    let stall_port = stall.local_addr().unwrap().port();
    let seen = transport_case(9893, true, move |tx, server| async move {
        settle().await;
        apply_settings(&tx, true);
        server.drop_connection();
        settle().await;
        tx.send(connect_action(stall_port, TlsMode::NoVerify))
            .unwrap();
        settle().await;
        submit(&tx, "look");
        settle().await;
        send(&tx, "probe-connect");
        settle().await;
        send(&tx, "transport-done");
    })
    .await;
    drop(stall);

    assert_eq!(proposals(&seen), vec!["look"], "transcript:\n{seen:#?}");
    assert_eq!(
        count_connect_requests(&seen),
        0,
        "neither the failed send nor connect() may restart the attempt; transcript:\n{seen:#?}"
    );
    assert!(has_line(&seen, "CONNECT_PROBED"), "transcript:\n{seen:#?}");
    assert!(
        has_system(&seen, "Still connecting. look could not be sent."),
        "transcript:\n{seen:#?}"
    );
}
