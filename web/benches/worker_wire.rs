use std::{fmt::Write as _, hint::black_box};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use smudgy_engine::{ConnectionState, TerminalBuffer, TerminalDeltaCursor};
use smudgy_web_client::wire;

const BATCH_LINES: usize = 1_000;

fn wire_batch(first: usize) -> Vec<u8> {
    let mut input = String::with_capacity(BATCH_LINES * 80);
    for row in first..first + BATCH_LINES {
        writeln!(
            input,
            "\x1b[38;5;{}m{row:06}\x1b[0m Smudgy worker wire benchmark row",
            33 + row % 180
        )
        .unwrap();
    }
    input.into_bytes()
}

fn terminal() -> TerminalBuffer {
    let mut terminal = TerminalBuffer::default();
    terminal.feed(&wire_batch(0));
    terminal
}

fn worker_wire(c: &mut Criterion) {
    let terminal = terminal();
    let mut cursor = TerminalDeltaCursor::default();
    let frame = wire::SessionFrame {
        sequence: 1,
        connection: ConnectionState::Connected,
        status: "Connected",
        server_echo: false,
        gmcp_messages: 42,
        max_lines: terminal.max_lines(),
        delta: cursor.take(&terminal),
    };
    let encoded = wire::encode(&frame).unwrap();

    let mut group = c.benchmark_group("worker_wire");
    group.throughput(Throughput::Elements(BATCH_LINES as u64));
    group.bench_function("encode_borrowed_delta", |b| {
        b.iter(|| black_box(wire::encode(black_box(&frame)).unwrap()));
    });
    group.bench_function("decode_owned_update", |b| {
        b.iter(|| black_box(wire::decode(black_box(&encoded)).unwrap()));
    });

    let mut full = TerminalBuffer::default();
    for batch in 0..100 {
        full.feed(&wire_batch(batch * BATCH_LINES));
    }
    let mut full_cursor = TerminalDeltaCursor::default();
    let _ = full_cursor.take(&full);
    full.feed(&wire_batch(100 * BATCH_LINES));
    let steady_frame = wire::SessionFrame {
        sequence: 2,
        connection: ConnectionState::Connected,
        status: "Connected",
        server_echo: false,
        gmcp_messages: 42,
        max_lines: full.max_lines(),
        delta: full_cursor.take(&full),
    };
    assert!(!steady_frame.delta.reset);
    assert_eq!(steady_frame.delta.rows.len(), BATCH_LINES);
    let steady_encoded = wire::encode(&steady_frame).unwrap();
    group.bench_function("encode_at_100k_scrollback", |b| {
        b.iter(|| black_box(wire::encode(black_box(&steady_frame)).unwrap()));
    });
    group.bench_function("decode_at_100k_scrollback", |b| {
        b.iter(|| black_box(wire::decode(black_box(&steady_encoded)).unwrap()));
    });
    group.finish();
}

criterion_group!(benches, worker_wire);
criterion_main!(benches);
