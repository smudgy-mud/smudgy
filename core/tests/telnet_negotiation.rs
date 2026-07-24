//! Live-socket coverage of the telnet negotiation responders
//! (`docs/telnet.md` Phase 1): a real [`Connection`] against a local
//! listener, asserting the bytes the server actually receives — the `WILL` answers, the
//! TTYPE/MTTS `IS` cycle, the immediate NAWS report, and the size-change wakeup path
//! through the shared size cell (`notify_window_size`). Complements the in-process ingest
//! tests in `connection.rs`, which cover the same responders without a socket or the
//! connect task's write arm.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::time::{Duration, Instant};

use smudgy_core::session::connection::{Connection, responders};
use smudgy_core::session::runtime::RuntimeAction;

/// Telnet bytes the assertions build from.
const IAC: u8 = 255;
const SB: u8 = 250;
const SE: u8 = 240;
const WILL: u8 = 251;
const DO: u8 = 253;
const TTYPE: u8 = 24;
const NAWS: u8 = 31;

/// Read from `sock` until `collected` contains `needle` (or panic at the deadline),
/// returning the offset just past the match. Bytes may arrive split across reads.
fn read_until(sock: &mut TcpStream, collected: &mut Vec<u8>, needle: &[u8], what: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(pos) = collected
            .windows(needle.len())
            .position(|window| window == needle)
        {
            return pos + needle.len();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; received so far: {collected:02x?}"
        );
        let mut buf = [0_u8; 1024];
        match sock.read(&mut buf) {
            Ok(0) => panic!("socket closed while waiting for {what}"),
            Ok(n) => collected.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("read failed while waiting for {what}: {e}"),
        }
    }
}

/// The `IAC SB TTYPE IS <name> IAC SE` frame for one cycle entry.
fn ttype_is(name: &str) -> Vec<u8> {
    let mut frame = vec![IAC, SB, TTYPE, 0];
    frame.extend_from_slice(name.as_bytes());
    frame.extend_from_slice(&[IAC, SE]);
    frame
}

/// The `IAC SB NAWS c c r r IAC SE` frame (no 0xFF dims in this test, so no doubling).
fn naws_report(cols: u16, rows: u16) -> Vec<u8> {
    let c = cols.to_be_bytes();
    let r = rows.to_be_bytes();
    vec![IAC, SB, NAWS, c[0], c[1], r[0], r[1], IAC, SE]
}

#[test]
fn responders_answer_ttype_and_naws_over_a_live_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, _ui_rx) = futures::channel::mpsc::channel(64);
    let window_size = Arc::new(AtomicU32::new(responders::pack_dims(100, 30)));
    let mut connection = Connection::new(
        runtime_tx,
        ui_tx,
        Arc::new(AtomicBool::new(false)),
        window_size.clone(),
    );
    connection.connect(
        "127.0.0.1",
        port,
        None,
        None,
        true,
        smudgy_core::session::connection::TlsMode::Off,
    );

    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut rx = Vec::new();

    // DO NAWS: expect WILL NAWS followed by the immediate report of the cell's size
    // (RFC 1073 requires the report right after the WILL).
    sock.write_all(&[IAC, DO, NAWS]).expect("send DO NAWS");
    read_until(&mut sock, &mut rx, &[IAC, WILL, NAWS], "WILL NAWS");
    read_until(
        &mut sock,
        &mut rx,
        &naws_report(100, 30),
        "initial NAWS report",
    );

    // DO TTYPE + three SENDs: the MTTS cycle, with the bitvector repeated verbatim.
    sock.write_all(&[IAC, DO, TTYPE]).expect("send DO TTYPE");
    read_until(&mut sock, &mut rx, &[IAC, WILL, TTYPE], "WILL TTYPE");
    let send = [IAC, SB, TTYPE, 1, IAC, SE];
    for expected in [
        ttype_is(responders::CLIENT_NAME),
        ttype_is(responders::TERMINAL_TYPE),
        ttype_is(&format!("MTTS {}", responders::mtts::bitvector(false))),
        ttype_is(&format!("MTTS {}", responders::mtts::bitvector(false))),
    ] {
        sock.write_all(&send).expect("send TTYPE SEND");
        read_until(&mut sock, &mut rx, &expected, "TTYPE IS reply");
    }

    // A size change: store into the shared cell (as the runtime's dispatch arm does),
    // then wake the socket task — a fresh report with the new size must arrive.
    window_size.store(
        responders::pack_dims(120, 40),
        std::sync::atomic::Ordering::Relaxed,
    );
    connection.notify_window_size();
    let consumed = read_until(
        &mut sock,
        &mut rx,
        &naws_report(120, 40),
        "resized NAWS report",
    );
    rx.drain(..consumed);

    // A wakeup without a change is swallowed. Prove it with a sentinel: the next TTYPE
    // reply must be the next bytes on the wire, with no NAWS frame before it.
    connection.notify_window_size();
    sock.write_all(&send).expect("send sentinel TTYPE SEND");
    let end = read_until(
        &mut sock,
        &mut rx,
        &ttype_is("MTTS 269"),
        "sentinel IS reply",
    );
    let before_sentinel = &rx[..end - ttype_is("MTTS 269").len()];
    assert!(
        !before_sentinel
            .windows(3)
            .any(|window| window == [IAC, SB, NAWS]),
        "an unchanged wakeup must not emit a NAWS report; got {before_sentinel:02x?}"
    );

    // The connection stayed healthy throughout (Connected observed, no Disconnected).
    connection.disconnect();
    let mut saw_connected = false;
    while let Ok(action) = runtime_rx.try_recv() {
        if matches!(action, RuntimeAction::Connected) {
            saw_connected = true;
        }
    }
    assert!(
        saw_connected,
        "the connect task must have reported Connected"
    );
}

/// MCCP2 end to end over a live socket: negotiation, the mid-buffer switchover at the start
/// marker, decompressed lines flowing through the full ingest pipeline, an orderly stream
/// end reverting to plain telnet, and plain lines continuing afterward.
#[test]
fn mccp2_compresses_the_stream_and_reverts_on_stream_end() {
    const IAC: u8 = 255;
    const SB: u8 = 250;
    const SE: u8 = 240;
    const WILL: u8 = 251;
    const DO: u8 = 253;
    const MCCP2: u8 = 86;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, _ui_rx) = futures::channel::mpsc::channel(64);
    let mut connection = Connection::new(
        runtime_tx,
        ui_tx,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU32::new(responders::pack_dims(80, 24))),
    );
    connection.connect(
        "127.0.0.1",
        port,
        None,
        None,
        true,
        smudgy_core::session::connection::TlsMode::Off,
    );

    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut rx = Vec::new();

    // Negotiate compression and wait for the DO before sending compressed bytes.
    sock.write_all(&[IAC, WILL, MCCP2])
        .expect("send WILL MCCP2");
    read_until(&mut sock, &mut rx, &[IAC, DO, MCCP2], "DO MCCP2");

    // Two writes with a delay between them — the realistic wire shape a real server
    // produces: the start marker flushed in ITS OWN segment, the compressed stream
    // following separately. This exercises the marker-at-buffer-end switchover (the bug a
    // single coalesced burst hides): the marker read must arm the inflater so the *next*
    // read's zlib bytes decompress instead of feeding the parser as plaintext.
    let mut z = flate2::Compress::new(flate2::Compression::default(), true);
    // `compress_vec` writes only into spare capacity — reserve enough for the tiny payload.
    let mut compressed = Vec::with_capacity(256);
    z.compress_vec(
        b"compressed line one\r\ncompressed line two\r\n",
        &mut compressed,
        flate2::FlushCompress::Finish,
    )
    .expect("compress");
    assert!(
        !compressed.is_empty(),
        "the compressed segment must be real"
    );

    let mut marker = b"plain before\r\n".to_vec();
    marker.extend_from_slice(&[IAC, SB, MCCP2, IAC, SE]);
    sock.write_all(&marker).expect("send marker");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));

    let mut tail = compressed;
    // Plain again after the finished stream (MCCP2 permits later renegotiation).
    tail.extend_from_slice(b"plain after\r\n");
    sock.write_all(&tail).expect("send compressed tail");

    // Collect emitted complete lines until all four arrive (or time out). Echoes ride
    // along for the failure diagnostics (a compression error surfaces as one).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut lines: Vec<String> = Vec::new();
    let mut echoes: Vec<String> = Vec::new();
    while lines.len() < 4 && Instant::now() < deadline {
        match runtime_rx.try_recv() {
            Ok(RuntimeAction::HandleIncomingLine(line)) => lines.push(line.text.clone()),
            Ok(RuntimeAction::Echo(text)) => echoes.push(text.to_string()),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert_eq!(
        lines,
        vec![
            "plain before".to_string(),
            "compressed line one".to_string(),
            "compressed line two".to_string(),
            "plain after".to_string(),
        ],
        "the stream must decode across the compression boundaries; echoes: {echoes:?}"
    );

    connection.disconnect();
}

/// Regression: a nested compression-start marker embedded in the DECOMPRESSED bytes of the
/// same chunk that ends the stream must not survive `inflow.end()` and re-enter compression
/// on the plain tail (a protocol-violating server should not be able to trigger a spurious
/// disconnect). The connection must stay up and the plain tail must render.
#[test]
fn nested_marker_at_stream_end_does_not_strand_the_latch() {
    const IAC: u8 = 255;
    const SB: u8 = 250;
    const SE: u8 = 240;
    const WILL: u8 = 251;
    const DO: u8 = 253;
    const MCCP2: u8 = 86;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, _ui_rx) = futures::channel::mpsc::channel(64);
    let mut connection = Connection::new(
        runtime_tx,
        ui_tx,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU32::new(responders::pack_dims(80, 24))),
    );
    connection.connect(
        "127.0.0.1",
        port,
        None,
        None,
        true,
        smudgy_core::session::connection::TlsMode::Off,
    );

    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut rx = Vec::new();

    sock.write_all(&[IAC, WILL, MCCP2]).expect("send WILL MCCP2");
    read_until(&mut sock, &mut rx, &[IAC, DO, MCCP2], "DO MCCP2");

    // A deflate stream that decompresses to a line, then a nested MCCP2 start marker, all
    // finished in ONE frame (Z_FINISH) — the latch-arming marker rides the stream-end chunk.
    let mut payload = b"before nested\r\n".to_vec();
    payload.extend_from_slice(&[IAC, SB, MCCP2, IAC, SE]);
    let mut z = flate2::Compress::new(flate2::Compression::default(), true);
    let mut compressed = Vec::with_capacity(256);
    z.compress_vec(&payload, &mut compressed, flate2::FlushCompress::Finish)
        .expect("compress");

    let mut marker = b"plain start\r\n".to_vec();
    marker.extend_from_slice(&[IAC, SB, MCCP2, IAC, SE]);
    sock.write_all(&marker).expect("send start marker");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));
    let mut tail = compressed;
    tail.extend_from_slice(b"plain after\r\n"); // plain, after the finished stream
    sock.write_all(&tail).expect("send compressed + plain tail");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut lines: Vec<String> = Vec::new();
    let mut echoes: Vec<String> = Vec::new();
    while lines.len() < 3 && Instant::now() < deadline {
        match runtime_rx.try_recv() {
            Ok(RuntimeAction::HandleIncomingLine(line)) => lines.push(line.text.clone()),
            Ok(RuntimeAction::Echo(text)) => echoes.push(text.to_string()),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert!(
        !echoes.iter().any(|e| e.contains("Compression error")),
        "the nested marker must NOT trigger a compression error; echoes={echoes:?}"
    );
    assert_eq!(
        lines,
        vec![
            "plain start".to_string(),
            "before nested".to_string(),
            "plain after".to_string(),
        ],
        "the plain tail after the stream end must render; echoes={echoes:?}"
    );

    connection.disconnect();
}

/// MCCPX (draft) end to end over a live socket: `WILL MCCPX` → we reply `DO` and offer
/// `zstd,deflate`; the server begins a `zstd` stream via `BEGIN_ENCODING`, and the
/// decompressed lines flow through the full ingest pipeline. The marker and the compressed
/// stream arrive in separate segments (the real wire shape).
#[test]
fn mccpx_zstd_stream_decodes_end_to_end() {
    const IAC: u8 = 255;
    const SB: u8 = 250;
    const SE: u8 = 240;
    const WILL: u8 = 251;
    const DO: u8 = 253;
    const MCCPX: u8 = 88;
    const BEGIN_ENCODING: u8 = 2;
    const ACCEPT_ENCODING: u8 = 1;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, _ui_rx) = futures::channel::mpsc::channel(64);
    let mut connection = Connection::new(
        runtime_tx,
        ui_tx,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU32::new(responders::pack_dims(80, 24))),
    );
    connection.connect(
        "127.0.0.1",
        port,
        None,
        None,
        true,
        smudgy_core::session::connection::TlsMode::Off,
    );

    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut rx = Vec::new();

    // Offer compression; expect DO then the ACCEPT_ENCODING offer listing zstd,deflate.
    sock.write_all(&[IAC, WILL, MCCPX])
        .expect("send WILL MCCPX");
    read_until(&mut sock, &mut rx, &[IAC, DO, MCCPX], "DO MCCPX");
    let mut offer = vec![IAC, SB, MCCPX, ACCEPT_ENCODING];
    offer.extend_from_slice(b"zstd,deflate");
    offer.extend_from_slice(&[IAC, SE]);
    read_until(&mut sock, &mut rx, &offer, "ACCEPT_ENCODING offer");

    // BEGIN_ENCODING zstd in its own segment, then the zstd frame separately.
    let mut marker = vec![IAC, SB, MCCPX, BEGIN_ENCODING];
    marker.extend_from_slice(b"zstd");
    marker.extend_from_slice(&[IAC, SE]);
    sock.write_all(&marker).expect("send BEGIN zstd");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));

    let frame = zstd::bulk::compress(b"zstd line one\r\nzstd line two\r\n", 3).expect("compress");
    sock.write_all(&frame).expect("send zstd frame");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut lines: Vec<String> = Vec::new();
    let mut echoes: Vec<String> = Vec::new();
    while lines.len() < 2 && Instant::now() < deadline {
        match runtime_rx.try_recv() {
            Ok(RuntimeAction::HandleIncomingLine(line)) => lines.push(line.text.clone()),
            Ok(RuntimeAction::Echo(text)) => echoes.push(text.to_string()),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert_eq!(
        lines,
        vec!["zstd line one".to_string(), "zstd line two".to_string()],
        "the zstd stream must decode through the pipeline; echoes: {echoes:?}"
    );

    connection.disconnect();
}
