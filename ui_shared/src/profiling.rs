//! Optional renderer instrumentation, independent of Cargo build profiles.
//!
//! Enable the `profiling` feature to collect these counters. Snapshots do not
//! request redraws. They measure host capture activity, not GPU execution time.
use std::sync::atomic::{AtomicUsize, Ordering};

/// Process-wide renderer measurements, grouped by subsystem.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub text_effects: TextEffects,
}

#[derive(Debug, Clone, Copy)]
pub struct TextEffects {
    pub admitted_inputs: usize,
    pub captures: CaptureStats,
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        text_effects: TextEffects {
            admitted_inputs: crate::text_effect::admitted_inputs(),
            captures: capture_stats(),
        },
    }
}

#[derive(Debug, Default)]
pub struct CaptureCounters {
    pub(crate) uploads: AtomicUsize,
    pub(crate) cache_hits: AtomicUsize,
    pub(crate) key_builds: AtomicUsize,
    pub(crate) resident_bytes: AtomicUsize,
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureStats {
    pub uploads: usize,
    pub cache_hits: usize,
    pub key_builds: usize,
    /// Global snapshots count physical image/geometry bytes once. Input snapshots
    /// count their referenced footprint, including pixels shared with other inputs.
    pub resident_bytes: usize,
}

impl CaptureCounters {
    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            uploads: self.uploads.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            key_builds: self.key_builds.load(Ordering::Relaxed),
            resident_bytes: self.resident_bytes.load(Ordering::Relaxed),
        }
    }
}

pub(crate) static TOTAL_UPLOADS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TOTAL_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TOTAL_KEYS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TOTAL_BYTES: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn capture_stats() -> CaptureStats {
    CaptureStats {
        uploads: TOTAL_UPLOADS.load(Ordering::Relaxed),
        cache_hits: TOTAL_HITS.load(Ordering::Relaxed),
        key_builds: TOTAL_KEYS.load(Ordering::Relaxed),
        resident_bytes: TOTAL_BYTES.load(Ordering::Relaxed),
    }
}
