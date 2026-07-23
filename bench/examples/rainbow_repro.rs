//! Headless repro of the `dorainbow` styled-echo benchmark (`rainbow-bench/` at
//! the container root): drives the REAL session engine — V8, the styled-echo op
//! boundary, dispatch, and the session event channel — with the exact workload
//! of `rainbow-bench/rainbow-bench.ts`, minus the iced UI. Three cells isolate
//! where a styled echo's time goes:
//!
//! - `plain`: `echo(string)` of the same 90-char phrase — the op + dispatch floor.
//! - `styled1`: a styled fragment that is ONE 90-char run — the styled path's
//!   fixed per-call cost (flatten, serde boundary, validate, line build).
//! - `rainbow90`: the real rainbow — 90 single-char runs per line — adds the
//!   per-run scaling (the untagged `ColorWire` deserialize, per-run `String`s,
//!   span construction).
//!
//! Two timings per cell:
//! - `js`: the loop time reported by `performance.now()` INSIDE the script —
//!   exactly what the in-app `dorainbow` alias prints. It covers the JS flatten
//!   + the op call (serde, validate, `StyledLine` build) + the action-queue
//!     push. It does NOT cover dispatch/flush/UI.
//! - `wall`: feed-to-drained wall time, which adds dispatch, the per-echo
//!   buffer flush, and the event-channel hop (our drain stands in for the UI).
//!
//! Alloc counts are whole-process around an otherwise-quiesced session; the
//! drain side contributes ~1 alloc/line (one `String` clone per collected line).
//!
//! Run: `cargo run --release -p smudgy_bench --example rainbow_repro`
//! Env: `RAINBOW_TOTAL=n` — lines per timed pass (default `20_000`).

use std::time::Instant;

use smudgy_bench::alloc;
use smudgy_bench::session::{BenchSession, bench_runtime, styled};

#[global_allocator]
static ALLOC: alloc::CountingAllocator = alloc::CountingAllocator;

const DEFAULT_TOTAL: usize = 20_000;

/// Mirrors `rainbow-bench/rainbow-bench.ts` (same phrase, same 120-phase hue
/// cycle, fragments precomputed at load), driven by triggers instead of an
/// alias so the harness can feed one line per pass.
const MODULE: &str = r#"
import { createTrigger, echo, style } from "smudgy:core";

const PHRASE = "The quick brown fox jumps over the lazy dog. ".repeat(2);
const CYCLE = 120;

function hsv(h: number): { r: number; g: number; b: number } {
    const x = Math.round(255 * (1 - Math.abs(((h / 60) % 2) - 1)));
    if (h < 60) return { r: 255, g: x, b: 0 };
    if (h < 120) return { r: x, g: 255, b: 0 };
    if (h < 180) return { r: 0, g: 255, b: x };
    if (h < 240) return { r: 0, g: x, b: 255 };
    if (h < 300) return { r: x, g: 0, b: 255 };
    return { r: 255, g: 0, b: x };
}

const lines = Array.from({ length: CYCLE }, (_, phase) => {
    let frag = style``;
    for (let i = 0; i < PHRASE.length; i++) {
        const hue = (phase * 3 + (i * 360) / PHRASE.length) % 360;
        frag = style`${frag}${style.fg(hsv(hue))`${PHRASE[i]}`}`;
    }
    return frag;
});

const oneRun = style.fg({ r: 255, g: 136, b: 0 })`${PHRASE}`;

function bench(total: number, marker: string, emit: (n: number) => void) {
    const start = performance.now();
    for (let n = 0; n < total; n++) emit(n);
    echo(`${marker} ${(performance.now() - start).toFixed(1)}`);
}

createTrigger(/^ZZPLAIN (\d+)$/, (m) => {
    bench(Number(m[1]), "ZZPLDONE", () => echo(PHRASE));
}, { name: "plain" });

createTrigger(/^ZZONERUN (\d+)$/, (m) => {
    bench(Number(m[1]), "ZZORDONE", () => echo(oneRun));
}, { name: "onerun" });

createTrigger(/^ZZRAINBOW (\d+)$/, (m) => {
    bench(Number(m[1]), "ZZRBDONE", (n) => echo(lines[n % CYCLE]));
}, { name: "rainbow" });
"#;

struct Cell {
    name: &'static str,
    command: &'static str,
    marker: &'static str,
}

const CELLS: &[Cell] = &[
    Cell {
        name: "plain",
        command: "ZZPLAIN",
        marker: "ZZPLDONE",
    },
    Cell {
        name: "styled1",
        command: "ZZONERUN",
        marker: "ZZORDONE",
    },
    Cell {
        name: "rainbow90",
        command: "ZZRAINBOW",
        marker: "ZZRBDONE",
    },
];

/// The `js` milliseconds the module printed on its marker line.
fn parse_js_ms(texts: &[String], marker: &str) -> f64 {
    let line = texts
        .iter()
        .find(|t| t.starts_with(marker))
        .unwrap_or_else(|| panic!("no {marker} line in the pass transcript"));
    line[marker.len()..]
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("unparsable timing line: {line:?}"))
}

#[allow(clippy::cast_precision_loss)]
fn main() {
    let total: usize = std::env::var("RAINBOW_TOTAL")
        .ok()
        .map_or(DEFAULT_TOTAL, |v| {
            v.parse().expect("RAINBOW_TOTAL must be a number")
        });

    let rt = bench_runtime();
    let mut session = BenchSession::start(
        &rt,
        "ZZRainbowRepro",
        9301,
        &[("rainbow.ts", MODULE.to_string())],
        &[],
    );

    // Warmup: run every cell once at one cycle's worth of lines so module load,
    // JIT tiers, and each fragment's first crossing are behind us; sanity-check
    // that a rainbow line's display text survived the styled path intact.
    let phrase = "The quick brown fox jumps over the lazy dog. ".repeat(2);
    for cell in CELLS {
        let mut texts = Vec::new();
        session.feed(&styled(&format!("{} 120", cell.command)));
        assert!(
            rt.block_on(session.drain_collect_until(cell.marker, &mut texts)),
            "timed out warming {}",
            cell.name
        );
        assert!(
            texts.contains(&phrase),
            "sanity: {} warmup never displayed the phrase",
            cell.name
        );
    }

    println!("rainbow_repro: {total} lines per pass (RAINBOW_TOTAL to change)");
    println!(
        "{:<10} {:>10} {:>12} {:>10} {:>12} {:>12} {:>14}",
        "cell", "js ms", "js µs/line", "wall ms", "wall µs/line", "allocs/line", "bytes/line"
    );
    for cell in CELLS {
        session.drain_stragglers();
        let mut texts = Vec::new();
        let before = alloc::snapshot();
        let start = Instant::now();
        session.feed(&styled(&format!("{} {total}", cell.command)));
        assert!(
            rt.block_on(session.drain_collect_until(cell.marker, &mut texts)),
            "timed out draining {}",
            cell.name
        );
        let wall = start.elapsed();
        let delta = alloc::since(before);
        let js_ms = parse_js_ms(&texts, cell.marker);
        let wall_ms = wall.as_secs_f64() * 1e3;
        let lines = total as f64;
        println!(
            "{:<10} {:>10.1} {:>12.2} {:>10.1} {:>12.2} {:>12.1} {:>14.0}",
            cell.name,
            js_ms,
            js_ms * 1e3 / lines,
            wall_ms,
            wall_ms * 1e3 / lines,
            delta.count as f64 / lines,
            delta.bytes as f64 / lines,
        );
    }
}
