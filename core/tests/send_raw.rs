//! Script-to-socket coverage of the `sendRaw` migration and byte contract.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::{InboundCompression, TlsMode};
use smudgy_core::session::runtime::{
    IsolateId, Origin, RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread,
};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const TIMEOUT: Duration = Duration::from_mins(1);

fn prepare(id: u32) -> (String, Arc<SessionParams>) {
    let home = tempfile::tempdir().unwrap();
    smudgy_core::set_smudgy_home(home.path());
    // The process-wide home retains its first value across parallel tests.
    std::mem::forget(home);
    let server = format!("test_send_raw_{id}");
    let root = smudgy_core::get_smudgy_home().unwrap().join(&server);
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::create_dir_all(root.join("modules")).unwrap();
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(id),
        server_name: Arc::new(server.clone()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    (server, params)
}

fn collect(event: &SessionEvent, lines: &mut Vec<String>) {
    if let SessionEvent::UpdateBuffer(updates) = event {
        for update in updates.iter() {
            if let BufferUpdate::Append(line) = update {
                lines.push(line.text.clone());
            }
        }
    }
}

async fn join_session(id: u32) {
    let session_id = SessionId::from(id);
    assert_eq!(
        tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
            .await
            .unwrap(),
        RuntimeThreadJoinOutcome::Clean { session_id }
    );
}

async fn socket_case(
    id: u32,
    body: &str,
    expected: Vec<u8>,
    encoding: Option<&str>,
) -> Vec<String> {
    let (server_name, params) = prepare(id);
    let root = smudgy_core::get_smudgy_home().unwrap().join(&server_name);
    std::fs::write(
        root.join("modules/sendraw.ts"),
        format!(
            r#"
import session, {{ send, sendRaw, echo, createAlias, createTrigger }} from "smudgy:core";
import {{ Buffer }} from "node:buffer";
createAlias("^alias-target$", () => {{ echo("UNEXPECTED_ALIAS"); }});
createTrigger("^SENDRAW_GO$", () => {{
    try {{
        {body}
    }} catch (error) {{
        echo("RAW_TEST_FAILURE: " + error.stack);
    }}
    sendRaw(Uint8Array.of(0x7e));
}});
"#
        ),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        socket.write_all(b"SENDRAW_GO\r\n").unwrap();
        let mut expected = expected;
        expected.push(0x7e);
        let mut received = vec![0; expected.len()];
        socket.read_exact(&mut received).unwrap();
        assert_eq!(received, expected, "exact ordered bytes on the socket");
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let error = socket
            .read(&mut [0])
            .expect_err("no trailing bytes or CRLF");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        socket.write_all(b"RAW_SERVER_DONE\r\n").unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });
    let mut events = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap()
            .unwrap();
        collect(&event.event, &mut lines);
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };
    tx.send(RuntimeAction::Connect {
        host: Arc::new("127.0.0.1".to_string()),
        port,
        send_on_connect: None,
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: encoding.map(|s| Arc::new(s.to_string())),
        compression: InboundCompression::NONE,
        tls: TlsMode::Off,
    })
    .unwrap();
    loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out: {lines:#?}"))
            .unwrap();
        collect(&event.event, &mut lines);
        assert!(
            !lines.iter().any(|line| line.contains("RAW_TEST_FAILURE")),
            "{lines:#?}"
        );
        if lines.iter().any(|line| line.contains("RAW_SERVER_DONE")) {
            break;
        }
    }
    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(events);
    join_session(id).await;
    server.join().unwrap();
    lines
}

#[tokio::test]
async fn strings_normalize_without_extra_commands_and_bytes_are_exact() {
    let body = r#"
sendRaw("alias-target");
session.session.sendRaw("a;b\nc\r\n\n");
sendRaw("\r");
sendRaw("");
send("ordinary");
const mutable = Uint8Array.of(0, 10, 13, 255, 241);
sendRaw(mutable);
mutable.fill(42);
const backing = Uint8Array.of(99, 65, 66, 88);
sendRaw(backing.subarray(1, 3));
sendRaw(new DataView(backing.buffer, 2, 1));
sendRaw(Uint8Array.of(0xfe, 0xff).buffer);
sendRaw(new Uint16Array(Uint8Array.of(1, 2, 3, 4).buffer));
sendRaw(Buffer.from([88, 89]).subarray(1));
sendRaw(new Uint8Array());
sendRaw(new TextEncoder().encode("BINARY_HIDDEN"));
sendRaw("end\n");
"#;
    let mut expected = b"alias-target\r\na;b\r\nc\r\n\r\n\r\r\n\r\nordinary\r\n".to_vec();
    expected.extend_from_slice(&[0, 10, 13, 255, 241, 65, 66, 66, 0xfe, 0xff, 1, 2, 3, 4]);
    expected.extend_from_slice(b"YBINARY_HIDDENend\r\n");
    let lines = socket_case(9810, body, expected, None).await;
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("sendRaw() warning"))
            .count(),
        1
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("sendRaw() warning") && line.contains("sendraw.ts"))
    );
    assert!(lines.iter().any(|line| line == "a;b"));
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("BINARY_HIDDEN") || line.contains("UNEXPECTED_ALIAS"))
    );
}

#[tokio::test]
async fn binary_bypasses_legacy_encoding_and_string_iac_is_escaped() {
    let body = r#"
sendRaw("café ÿ\n");
sendRaw(new TextEncoder().encode("ÿ"));
sendRaw(Uint8Array.of(0xff, 0xf1, 0xff, 0xff));
sendRaw("representable prefix\nunrepresentable: →\n");
sendRaw("after\n");
"#;
    let expected = b"caf\xe9 \xff\xff\r\n\xc3\xbf\xff\xf1\xff\xffafter\r\n".to_vec();
    let lines = socket_case(9811, body, expected, Some("windows-1252")).await;
    assert!(!lines.iter().any(|line| line.contains("sendRaw() warning")));
    assert!(lines.iter().any(|line| line.contains("Send error:")));
}

#[tokio::test]
async fn invalid_binary_input_is_rejected_without_coercion_or_iteration() {
    let body = r#"
const detached = new ArrayBuffer(2);
const detachedView = new Uint8Array(detached);
const detachedDataView = new DataView(detached);
detached.transfer();
if (!detached.detached) throw new Error("Test buffer was not detached");
let evaluated = false;
const invalid = [null, undefined, 42, [65], {}, new String("x"),
    new SharedArrayBuffer(1), new Uint8Array(new SharedArrayBuffer(1)),
    detached, detachedView, detachedDataView,
    { toString() { evaluated = true; return "x"; } },
    { *[Symbol.iterator]() { evaluated = true; yield 65; } },
    { async *[Symbol.asyncIterator]() { evaluated = true; yield 65; } }];
for (const [index, value] of invalid.entries()) {
    let rejected = false;
    try { sendRaw(value); } catch (error) { rejected = error instanceof TypeError; }
    if (!rejected) throw new Error("Invalid input was accepted at index " + index);
}
if (evaluated) throw new Error("Invalid input was evaluated");
for (const value of [new Uint8Array(16 * 1024 * 1024 + 1), "\n".repeat(8 * 1024 * 1024 + 1)]) {
    let limited = false;
    try { sendRaw(value); } catch (error) {
        if (!error.message.includes("bytes of sends")) throw error;
        limited = true;
    }
    if (!limited) throw new Error("Send byte limit was not enforced");
}
sendRaw(new TextEncoder().encode("VALID"));
echo("VALIDATION_OK");
"#;
    let lines = socket_case(9812, body, b"VALID".to_vec(), None).await;
    assert!(lines.iter().any(|line| line == "VALIDATION_OK"));
    assert!(!lines.iter().any(|line| line.contains("sendRaw() warning")));
}

#[tokio::test]
async fn warnings_are_per_module_and_inline_script_and_reset_on_reload() {
    let (server, params) = prepare(9813);
    let modules = smudgy_core::get_smudgy_home()
        .unwrap()
        .join(&server)
        .join("modules");
    for name in ["first", "second"] {
        std::fs::write(
            modules.join(format!("{name}.ts")),
            r#"
import { sendRaw } from "smudgy:core";
sendRaw("private payload");
sendRaw("another private payload");
await Promise.resolve();
sendRaw("async private payload");
"#,
        )
        .unwrap();
    }
    let mut events = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap()
            .unwrap();
        collect(&event.event, &mut lines);
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };
    for name in ["first-inline", "second-inline"] {
        tx.send(RuntimeAction::AddAlias {
            isolate: IsolateId::Main,
            origin: Origin::User,
            name: Arc::new(name.to_string()),
            alias: Box::new(smudgy_core::models::aliases::AliasDefinition {
                pattern: format!("^{name}$"),
                script: Some("sendRaw('inline payload'); sendRaw('again');".to_string()),
                language: smudgy_core::models::ScriptLang::JS,
                package: None,
                enabled: true,
                priority: 0,
                fallthrough: true,
                allow_self_match: false,
                matcher: None,
                state: Vec::new(),
            }),
            fire_limit: None,
        })
        .unwrap();
        tx.send(RuntimeAction::Send(Arc::new(name.to_string())))
            .unwrap();
        tx.send(RuntimeAction::Send(Arc::new(name.to_string())))
            .unwrap();
    }
    tx.send(RuntimeAction::Echo(Arc::new("BEFORE_RELOAD".to_string())))
        .unwrap();
    while !lines.iter().any(|line| line == "BEFORE_RELOAD") {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap()
            .unwrap();
        collect(&event.event, &mut lines);
    }
    let warnings: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("sendRaw() warning"))
        .collect();
    assert_eq!(warnings.len(), 4, "{lines:#?}");
    for name in ["first.ts", "second.ts", "first-inline", "second-inline"] {
        assert!(
            warnings.iter().any(|line| line.contains(name)),
            "{warnings:#?}"
        );
    }
    assert!(warnings.iter().all(|line| !line.contains("payload")));
    tx.send(RuntimeAction::Reload).unwrap();
    loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap()
            .unwrap();
        collect(&event.event, &mut lines);
        if lines
            .iter()
            .filter(|line| line.contains("sendRaw() warning"))
            .count()
            == 6
        {
            break;
        }
    }
    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(events);
    join_session(9813).await;
}

/// The 0.6 text dispatch path must not invent a newline in local output either.
#[tokio::test]
async fn partial_text_echo_stays_open_until_an_explicit_crlf() {
    let (_, params) = prepare(9814);
    let mut events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap()
            .unwrap();
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };
    tx.send(RuntimeAction::Echo(Arc::new("PARTIAL_BEGIN".to_string())))
        .unwrap();
    loop {
        let event = tokio::time::timeout(TIMEOUT, events.next())
            .await
            .unwrap()
            .unwrap();
        let mut lines = Vec::new();
        collect(&event.event, &mut lines);
        if lines.iter().any(|line| line == "PARTIAL_BEGIN") {
            break;
        }
    }
    for (text, fragment, commits) in [("part", "part", 0), ("ial\r\n", "ial", 1)] {
        tx.send(RuntimeAction::SendRawText(Arc::new(text.to_string())))
            .unwrap();
        loop {
            let event = tokio::time::timeout(TIMEOUT, events.next())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::UpdateBuffer(updates) = event.event
                && updates.iter().any(
                    |update| matches!(update, BufferUpdate::Append(line) if line.text == fragment),
                )
            {
                assert_eq!(
                    updates
                        .iter()
                        .filter(|update| matches!(update, BufferUpdate::EnsureNewLine))
                        .count(),
                    commits
                );
                break;
            }
        }
    }
    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(events);
    join_session(9814).await;
}
