//! A firehose log server for end-to-end client ingest benchmarks: loads a raw
//! MUD log into memory once, then blasts the whole thing at every client that
//! connects, as fast as the socket will take it. Connect smudgy (or any other
//! client) to `localhost:5000` and measure how it handles several hours of
//! session traffic arriving in one burst.
//!
//! The hot path is deliberately trivial — `TCP_NODELAY`, one `write_all` of
//! the preloaded buffer, then a write-side shutdown.
//!
//! Each connection gets its own thread; per-connection wall time and throughput go
//! to stderr.
//!
//! Run (from the workspace root):
//! `cargo run --release -p smudgy_bench --example log_server -- bench/logs/synthetic-long-session.log`
//!
//! An optional second argument overrides the port (default 5000).

use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

const DEFAULT_PORT: u16 = 5000;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: log_server <logfile> [port]");
        return ExitCode::FAILURE;
    };
    let port = if let Some(raw) = args.next() {
        let Ok(port) = raw.parse() else {
            eprintln!("invalid port: {raw}");
            return ExitCode::FAILURE;
        };
        port
    } else {
        DEFAULT_PORT
    };

    let times: u16 = args.next().map_or(1, |arg| arg.parse().unwrap_or(1));

    let data: Arc<[u8]> = match std::fs::read(&path) {
        Ok(bytes) => bytes.into(),
        Err(err) => {
            eprintln!("failed to read {path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("failed to bind 127.0.0.1:{port}: {err}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("serving {path} ({} bytes) on 127.0.0.1:{port}", data.len());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let data = Arc::clone(&data);
                std::thread::spawn(move || serve(&stream, &data, times));
            }
            Err(err) => eprintln!("accept failed: {err}"),
        }
    }
    unreachable!("listener.incoming() never yields None");
}

fn serve(mut stream: &TcpStream, data: &[u8], times: u16) {
    let peer = stream
        .peer_addr()
        .map_or_else(|_| String::from("<unknown>"), |addr| addr.to_string());
    if let Err(err) = stream.set_nodelay(true) {
        eprintln!("[{peer}] set_nodelay failed: {err}");
    }

    let start = Instant::now();
    if let Err(err) = stream.write_all("BEGIN MEASURING\r\n".as_bytes()) {
        eprintln!("[{peer}] write failed after {:?}: {err}", start.elapsed());
        return;
    }
    for _ in 0..times {
        if let Err(err) = stream.write_all(data) {
            eprintln!("[{peer}] write failed after {:?}: {err}", start.elapsed());
            return;
        }
    }
    if let Err(err) = stream.write_all("END MEASURING\r\n".as_bytes()) {
        eprintln!("[{peer}] write failed after {:?}: {err}", start.elapsed());
        return;
    }
    let _ = stream.shutdown(Shutdown::Write);

    let elapsed = start.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let mib_per_sec = ("BEGIN MEASURING\r\nEND MEASURING\r\n".len()
        + ((times as usize) * data.len())) as f64
        / (1024.0 * 1024.0)
        / elapsed.as_secs_f64();
    eprintln!(
        "[{peer}] sent {} bytes in {elapsed:?} ({mib_per_sec:.1} MiB/s)",
        data.len()
    );
}
