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

use smudgy_core::session::connection::{Connection, InboundCompression, responders};
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

/// Compress one payload as a complete RFC 1950 zlib stream.
fn finished_zlib(payload: &[u8]) -> Vec<u8> {
    let mut z = flate2::Compress::new(flate2::Compression::default(), true);
    let mut compressed = Vec::with_capacity(payload.len() + 64);
    z.compress_vec(payload, &mut compressed, flate2::FlushCompress::Finish)
        .expect("compress zlib stream");
    assert!(!compressed.is_empty(), "zlib stream must emit wire bytes");
    compressed
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
        InboundCompression::ALL,
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
    let sentinel = ttype_is(&format!("MTTS {}", responders::mtts::bitvector(false)));
    let end = read_until(&mut sock, &mut rx, &sentinel, "sentinel IS reply");
    let before_sentinel = &rx[..end - sentinel.len()];
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
        InboundCompression::ALL,
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
    // Plain again after the finished stream (MCCP2 permits a later stream restart).
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
            Ok(RuntimeAction::HandleIncomingLines(batch)) => {
                lines.extend(batch.iter().map(|line| line.text.clone()));
            }
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

/// A copyover may keep the TCP socket and MCCP2 option agreement, finish the old zlib stream,
/// briefly resume plaintext, then send a bare start marker for a fresh stream.
#[test]
fn mccp2_copyover_restarts_without_will_do_renegotiation() {
    const MCCP2: u8 = 86;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (mut connection, mut runtime_rx) = compression_test_connection(port);
    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();

    let mut replies = Vec::new();
    sock.write_all(&[IAC, WILL, MCCP2]).expect("offer MCCP2");
    read_until(&mut sock, &mut replies, &[IAC, DO, MCCP2], "DO MCCP2");

    let marker = [IAC, SB, MCCP2, IAC, SE];
    sock.write_all(&marker).expect("start pre-copyover stream");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));
    let mut before_copyover = finished_zlib(b"before copyover\r\n");
    before_copyover.extend_from_slice(b"copyover plaintext\r\n");
    sock.write_all(&before_copyover)
        .expect("finish old stream and resume plaintext");

    let (first_lines, first_echoes) = wait_for_lines(&mut runtime_rx, 2);
    assert_eq!(
        first_lines,
        vec![
            "before copyover".to_string(),
            "copyover plaintext".to_string()
        ],
        "the first stream must end cleanly; echoes={first_echoes:?}"
    );

    // No second WILL/DO exchange: the original Telnet agreement remains in force.
    // Coalesce marker + stream + plain tail to cover every transition in one socket burst.
    let mut after_copyover = marker.to_vec();
    after_copyover.extend_from_slice(&finished_zlib(b"after copyover\r\n"));
    after_copyover.extend_from_slice(b"plain after restart\r\n");
    sock.write_all(&after_copyover)
        .expect("restart MCCP2 and send its fresh stream plus plaintext tail");

    let (second_lines, second_echoes) = wait_for_lines(&mut runtime_rx, 2);
    assert_eq!(
        second_lines,
        vec![
            "after copyover".to_string(),
            "plain after restart".to_string()
        ],
        "the bare marker must start an independent inflater; echoes={second_echoes:?}"
    );
    assert!(
        !second_echoes
            .iter()
            .any(|echo| echo.contains("Compression error")),
        "a valid copyover restart must not desynchronize; echoes={second_echoes:?}"
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
        InboundCompression::ALL,
        smudgy_core::session::connection::TlsMode::Off,
    );

    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut rx = Vec::new();

    sock.write_all(&[IAC, WILL, MCCP2])
        .expect("send WILL MCCP2");
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
            Ok(RuntimeAction::HandleIncomingLines(batch)) => {
                lines.extend(batch.iter().map(|line| line.text.clone()));
            }
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
/// `zstd,deflate`; the server begins a `zstd` stream via `BEGIN_ENCODING`, sends two
/// concatenated frames, then terminates with plaintext `WONT MCCPX`. The decompressed and
/// following plaintext lines all flow through the full ingest pipeline.
#[test]
fn mccpx_zstd_stream_decodes_end_to_end() {
    const WONT: u8 = 252;
    const MCCPX: u8 = 88;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (mut connection, mut runtime_rx) = compression_test_connection(port);
    let (mut sock, _) = listener.accept().expect("accept");
    negotiate_mccpx_zstd(&mut sock);

    let first_frame = zstd::bulk::compress(b"zstd frame one\r\n", 3).expect("compress first");
    let second_frame = zstd::bulk::compress(b"zstd frame two\r\n", 3).expect("compress second");
    let mut stream = first_frame;
    stream.extend_from_slice(&second_frame);
    stream.extend_from_slice(&[IAC, WONT, MCCPX]);
    stream.extend_from_slice(b"plain after mccpx\r\n");
    sock.write_all(&stream)
        .expect("send concatenated zstd frames and plaintext shutdown");

    let (lines, echoes) = wait_for_lines(&mut runtime_rx, 3);
    assert_eq!(
        lines,
        vec![
            "zstd frame one".to_string(),
            "zstd frame two".to_string(),
            "plain after mccpx".to_string(),
        ],
        "concatenated frames and the plain tail must decode in order; echoes: {echoes:?}"
    );

    connection.disconnect();
}

fn compression_test_connection(
    port: u16,
) -> (
    Connection,
    tokio::sync::mpsc::UnboundedReceiver<RuntimeAction>,
) {
    let (runtime_tx, runtime_rx) = tokio::sync::mpsc::unbounded_channel();
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
        InboundCompression::ALL,
        smudgy_core::session::connection::TlsMode::Off,
    );
    (connection, runtime_rx)
}

fn negotiate_mccpx_zstd(sock: &mut TcpStream) {
    const MCCPX: u8 = 88;
    const BEGIN_ENCODING: u8 = 2;
    const ACCEPT_ENCODING: u8 = 1;

    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut replies = Vec::new();
    sock.write_all(&[IAC, WILL, MCCPX])
        .expect("send WILL MCCPX");
    read_until(sock, &mut replies, &[IAC, DO, MCCPX], "DO MCCPX");
    let mut offer = vec![IAC, SB, MCCPX, ACCEPT_ENCODING];
    offer.extend_from_slice(b"zstd,deflate");
    offer.extend_from_slice(&[IAC, SE]);
    read_until(sock, &mut replies, &offer, "ACCEPT_ENCODING offer");

    let mut marker = vec![IAC, SB, MCCPX, BEGIN_ENCODING];
    marker.extend_from_slice(b"zstd");
    marker.extend_from_slice(&[IAC, SE]);
    sock.write_all(&marker).expect("send BEGIN zstd");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));
}

fn wait_for_lines(
    runtime_rx: &mut tokio::sync::mpsc::UnboundedReceiver<RuntimeAction>,
    count: usize,
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut lines = Vec::new();
    let mut echoes = Vec::new();
    while lines.len() < count && Instant::now() < deadline {
        match runtime_rx.try_recv() {
            Ok(RuntimeAction::HandleIncomingLine(line)) => lines.push(line.text.clone()),
            Ok(RuntimeAction::HandleIncomingLines(batch)) => {
                lines.extend(batch.iter().map(|line| line.text.clone()));
            }
            Ok(RuntimeAction::Echo(text)) => echoes.push(text.to_string()),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    (lines, echoes)
}

fn wait_for_disconnect(runtime_rx: &mut tokio::sync::mpsc::UnboundedReceiver<RuntimeAction>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match runtime_rx.try_recv() {
            Ok(RuntimeAction::Disconnected { .. }) => return,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    panic!("timed out waiting for connection-loss teardown");
}

fn unfinished_zstd_frame(payload: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).expect("zstd encoder");
    encoder.write_all(payload).expect("compress payload");
    encoder.flush().expect("flush open frame");
    let wire = encoder.get_ref().clone();
    assert!(!wire.is_empty(), "an open zstd frame must emit wire bytes");
    wire
}

/// The continuation frame's magic and the final plaintext WONT may each straddle arbitrary
/// socket reads. Neither prefix may leak into the Telnet parser or be rejected prematurely.
#[test]
fn mccpx_zstd_boundary_markers_may_span_socket_reads() {
    const WONT: u8 = 252;
    const MCCPX: u8 = 88;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (mut connection, mut runtime_rx) = compression_test_connection(port);
    let (mut sock, _) = listener.accept().expect("accept");
    negotiate_mccpx_zstd(&mut sock);

    let first = zstd::bulk::compress(b"first split test frame\r\n", 3).expect("compress first");
    let second = zstd::bulk::compress(b"second split test frame\r\n", 3).expect("compress second");
    sock.write_all(&first).expect("send first frame");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));

    for part in [&second[..1], &second[1..3], &second[3..]] {
        sock.write_all(part).expect("send split second frame");
        sock.flush().ok();
        std::thread::sleep(Duration::from_millis(150));
    }

    sock.write_all(&[IAC]).expect("send split WONT IAC");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));
    sock.write_all(&[WONT]).expect("send split WONT command");
    sock.flush().ok();
    std::thread::sleep(Duration::from_millis(150));
    let mut final_part = vec![MCCPX];
    final_part.extend_from_slice(b"plain after split markers\r\n");
    sock.write_all(&final_part)
        .expect("finish WONT and plain tail");

    let (lines, echoes) = wait_for_lines(&mut runtime_rx, 3);
    assert_eq!(
        lines,
        vec![
            "first split test frame".to_string(),
            "second split test frame".to_string(),
            "plain after split markers".to_string(),
        ],
        "split boundary markers must preserve wire order; echoes={echoes:?}"
    );
    assert!(
        !echoes.iter().any(|echo| echo.contains("Compression error")),
        "valid split markers must not fail compression; echoes={echoes:?}"
    );

    connection.disconnect();
}

/// After a clean frame boundary, anything other than another standard zstd frame or the
/// plaintext MCCPX WONT is a protocol violation. It must not be guessed as application text.
#[test]
fn mccpx_zstd_invalid_frame_boundary_disconnects() {
    const DONT: u8 = 254;
    const MCCPX: u8 = 88;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (mut connection, mut runtime_rx) = compression_test_connection(port);
    let (mut sock, _) = listener.accept().expect("accept");
    negotiate_mccpx_zstd(&mut sock);

    let mut invalid =
        zstd::bulk::compress(b"line before invalid boundary\r\n", 3).expect("compress");
    invalid.push(b'X');
    sock.write_all(&invalid).expect("send invalid boundary");

    let mut replies = Vec::new();
    read_until(
        &mut sock,
        &mut replies,
        &[IAC, DONT, MCCPX],
        "DONT MCCPX after invalid boundary",
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut line_delivered = false;
    let mut compression_error = false;
    let mut disconnected = false;
    while Instant::now() < deadline {
        match runtime_rx.try_recv() {
            Ok(RuntimeAction::HandleIncomingLine(line))
                if line.text == "line before invalid boundary" =>
            {
                line_delivered = true;
            }
            Ok(RuntimeAction::HandleIncomingLines(batch))
                if batch
                    .iter()
                    .any(|line| line.text == "line before invalid boundary") =>
            {
                line_delivered = true;
            }
            Ok(RuntimeAction::Echo(text)) if text.contains("Compression error") => {
                compression_error = true;
            }
            Ok(RuntimeAction::Disconnected { .. }) => disconnected = true,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
        if line_delivered && compression_error && disconnected {
            break;
        }
    }
    assert!(
        line_delivered,
        "valid data before the boundary must be committed"
    );
    assert!(compression_error, "the invalid boundary must be reported");
    assert!(disconnected, "the invalid boundary must drop the socket");

    connection.disconnect();
}

/// MCCP4 requires WONT to be plaintext after the final frame. A compressed WONT may update
/// Telnet state while inflating, but it cannot turn the following bytes into a valid boundary.
#[test]
fn mccpx_zstd_compressed_wont_does_not_end_the_stream() {
    const WONT: u8 = 252;
    const MCCPX: u8 = 88;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (mut connection, mut runtime_rx) = compression_test_connection(port);
    let (mut sock, _) = listener.accept().expect("accept");
    negotiate_mccpx_zstd(&mut sock);

    let mut invalid = zstd::bulk::compress(&[IAC, WONT, MCCPX], 3).expect("compress WONT");
    invalid.push(b'X');
    sock.write_all(&invalid)
        .expect("send compressed WONT and invalid plaintext boundary");

    wait_for_disconnect(&mut runtime_rx);
    connection.disconnect();
}

/// Decoder state, an incomplete concatenated frame, and the MCCPX option claim all belong to
/// one TCP connection. A reconnect starts in plaintext and may then negotiate a fresh session
/// whose first zstd frame has no dependency on the lost connection's window.
#[test]
fn mccpx_loss_then_reconnect_starts_plain_and_renegotiates_a_fresh_frame() {
    const WONT: u8 = 252;
    const MCCPX: u8 = 88;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (mut connection, mut runtime_rx) = compression_test_connection(port);

    let (mut first, _) = listener.accept().expect("accept first connection");
    negotiate_mccpx_zstd(&mut first);
    let mut old_stream =
        zstd::bulk::compress(b"complete old frame\r\n", 3).expect("compress old frame");
    old_stream.extend_from_slice(&unfinished_zstd_frame(b"unfinished old frame\r\n"));
    first
        .write_all(&old_stream)
        .expect("send complete and unfinished old frames");

    let (old_lines, old_echoes) = wait_for_lines(&mut runtime_rx, 2);
    assert_eq!(
        old_lines,
        vec![
            "complete old frame".to_string(),
            "unfinished old frame".to_string(),
        ],
        "both old frames must decode before loss; echoes={old_echoes:?}"
    );

    drop(first);
    wait_for_disconnect(&mut runtime_rx);

    connection.connect(
        "127.0.0.1",
        port,
        None,
        None,
        InboundCompression::ALL,
        smudgy_core::session::connection::TlsMode::Off,
    );
    let (mut second, _) = listener.accept().expect("accept reconnect");
    second
        .write_all(b"plain after reconnect\r\n")
        .expect("send reconnect plaintext");
    let (plain_lines, plain_echoes) = wait_for_lines(&mut runtime_rx, 1);
    assert_eq!(
        plain_lines,
        vec!["plain after reconnect".to_string()],
        "reconnection must start in plaintext; echoes={plain_echoes:?}"
    );

    negotiate_mccpx_zstd(&mut second);
    let mut fresh_stream =
        zstd::bulk::compress(b"fresh frame after reconnect\r\n", 3).expect("compress fresh frame");
    fresh_stream.extend_from_slice(&[IAC, WONT, MCCPX]);
    second
        .write_all(&fresh_stream)
        .expect("send fresh frame and terminate MCCPX");
    let (fresh_lines, fresh_echoes) = wait_for_lines(&mut runtime_rx, 1);
    assert_eq!(
        fresh_lines,
        vec!["fresh frame after reconnect".to_string()],
        "renegotiation must start an independent zstd frame; echoes={fresh_echoes:?}"
    );
    assert!(
        !fresh_echoes
            .iter()
            .any(|echo| echo.contains("Compression error")),
        "fresh renegotiation must not reuse the old decoder; echoes={fresh_echoes:?}"
    );

    connection.disconnect();
}
