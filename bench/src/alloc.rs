//! A counting global allocator for benches that report **allocations per
//! operation** alongside wall time. µs-scale ops under Windows timer jitter
//! can hide a real 30% win inside criterion's noise band; an allocation count
//! is exact and reproducible, so before/after claims like "`set` for a
//! package producer: 11 allocs → 2" survive a noisy machine.
//!
//! Opt in per bench target:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: smudgy_bench::alloc::CountingAllocator = smudgy_bench::alloc::CountingAllocator;
//! ```
//!
//! then bracket the measured call with [`snapshot`] and report the
//! [`AllocDelta`]. Counters are process-global atomics: exact for
//! same-thread pure-Rust benches (`identity_tax`), and a *whole-process*
//! figure when session threads are live — meaningful only around a quiesced
//! session, and stated as such wherever it is printed.
//!
//! Only `alloc`/`realloc` count (a realloc is one new placement); `dealloc`
//! is free and untracked. `Relaxed` ordering: the counters are statistics,
//! not synchronization, and the measured code's own ordering is untouched.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

/// Forwards to [`System`], counting every allocation and allocated byte.
pub struct CountingAllocator;

// SAFETY: pure pass-through to `System` with side-effect-free atomic bumps;
// every safety property is `System`'s own.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// A point-in-time reading of the counters.
#[derive(Clone, Copy, Debug)]
pub struct AllocSnapshot {
    count: u64,
    bytes: u64,
}

/// Allocations and bytes between two [`snapshot`] calls.
#[derive(Clone, Copy, Debug)]
pub struct AllocDelta {
    pub count: u64,
    pub bytes: u64,
}

/// Read the counters now.
#[must_use]
pub fn snapshot() -> AllocSnapshot {
    AllocSnapshot {
        count: ALLOC_COUNT.load(Ordering::Relaxed),
        bytes: ALLOC_BYTES.load(Ordering::Relaxed),
    }
}

/// The counter movement since `earlier`.
#[must_use]
pub fn since(earlier: AllocSnapshot) -> AllocDelta {
    let now = snapshot();
    AllocDelta {
        count: now.count.saturating_sub(earlier.count),
        bytes: now.bytes.saturating_sub(earlier.bytes),
    }
}

/// Run `f` `iters` times and report the average allocations/bytes per call.
/// Same-thread exact; see the module doc for the multi-thread caveat.
pub fn per_call<F: FnMut()>(iters: u64, mut f: F) -> AllocDelta {
    assert!(iters > 0, "per_call needs at least one iteration");
    let before = snapshot();
    for _ in 0..iters {
        f();
    }
    let delta = since(before);
    AllocDelta {
        count: delta.count / iters,
        bytes: delta.bytes / iters,
    }
}
