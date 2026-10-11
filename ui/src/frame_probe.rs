//! Opt-in native redraw cadence capture; no subscription or disk I/O when disabled.
//! This records redraw events, not GPU execution or display scan-out timing.
use iced::{Subscription, time::Instant};
use std::{
    io::{BufWriter, Write},
    sync::{Mutex, OnceLock},
};
struct Probe {
    writer: BufWriter<std::fs::File>,
    first: Option<Instant>,
    last: Option<Instant>,
    next_flush: Option<Instant>,
    count: u64,
}
static PROBE: OnceLock<Option<Mutex<Probe>>> = OnceLock::new();
fn probe() -> Option<&'static Mutex<Probe>> {
    PROBE
        .get_or_init(|| {
            let path = std::env::var_os("SMUDGY_FRAME_PROBE")?;
            let mut writer = BufWriter::new(std::fs::File::create(path).ok()?);
            writeln!(
                writer,
                "frame,elapsed_ms,interval_ms,text_effects.inputs,text_effects.uploads,text_effects.resident_bytes"
            )
            .ok()?;
            Some(Mutex::new(Probe {
                writer,
                first: None,
                last: None,
                next_flush: None,
                count: 0,
            }))
        })
        .as_ref()
}
pub(super) fn subscription() -> Subscription<super::Message> {
    if probe().is_none() {
        return Subscription::none();
    }
    iced::window::frames().filter_map(|now| {
        if let Some(probe) = probe() {
            let mut p = probe.lock().unwrap();
            if p.count < 100_000 {
                let first = *p.first.get_or_insert(now);
                let elapsed = now.saturating_duration_since(first).as_secs_f64() * 1000.0;
                let interval = p.last.map_or(0.0, |last| {
                    now.saturating_duration_since(last).as_secs_f64() * 1000.0
                });
                p.count += 1;
                let count = p.count;
                let metrics = smudgy_ui_shared::profiling::snapshot().text_effects;
                let inputs = metrics.admitted_inputs;
                let stats = metrics.captures;
                let _ = writeln!(
                    p.writer,
                    "{count},{elapsed:.4},{interval:.4},{inputs},{},{}",
                    stats.uploads, stats.resident_bytes
                );
                p.last = Some(now);
                if p.next_flush.is_none_or(|next| now >= next) {
                    let _ = p.writer.flush();
                    p.next_flush = Some(now + std::time::Duration::from_secs(1));
                }
            }
        }
        // Observing a frame never injects an application message or forces another frame.
        None
    })
}
