//! Autosave scheduling and the runtime↔durable identity mirror.
//!
//! The workspace-persistence performance contract (`docs/panes.md` §17),
//! mechanically: a workspace mutation costs
//! exactly two boolean stores ([`Schedule::mark`]) — no DTO building, no
//! allocation, and no syscall ever happens on the mutation path. Snapshots
//! are built only when the trailing debounce settles or the max-interval
//! checkpoint fires, and the atomic write always runs on the writer worker.
//!
//! The debounce is tick-based rather than timestamp-based so even reading a
//! clock stays off the mutation path: a debounce tick that saw churn since
//! the previous tick clears the churn flag and waits; a quiet tick fires.
//! Sustained churn therefore never fires the debounce, and the checkpoint
//! bounds both crash loss and write volume to one write per interval.
//!
//! The checkpoint is also the **geometry dirty signal**: the idle event
//! filter deliberately drops `Moved` events (every mapped event repaints all
//! windows), so a window move alone never produces a message. Instead the
//! checkpoint polls `window::position`/`size`/`scale_factor` unconditionally
//! and change-detects the answers into the dirty flag — bounding placement
//! loss to one checkpoint interval without re-enabling move tracking.
//!
//! [`Mirror`] owns the run-scoped identity maps: stable workspace window ids
//! in creation order (`window::Id` permutes within a run and is meaningless
//! across restarts), stable session-slot ids, the polled geometry cache, and
//! the snapshot generation counter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use iced::window;
use smudgy_core::session::SessionId;

use super::dto;

/// The trailing settle interval: a snapshot builds after roughly one to two
/// quiet ticks (2–4 s after the last mutation).
pub const DEBOUNCE_TICK: Duration = Duration::from_secs(2);

/// The max-interval checkpoint: bounds crash loss AND write volume under
/// sustained churn, and drives the unconditional geometry poll.
pub const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(60);

/// Dirty/churn/shutdown state — the entire cost of a mutation lives here.
///
/// There is no publish-arming latch: per-server layouts are written only
/// while a session is active. The empty connect surface saves window
/// preferences separately and leaves the per-server layouts intact.
#[derive(Debug, Default)]
pub struct Schedule {
    /// The live workspace differs (or may differ) from the newest snapshot.
    dirty: bool,
    /// A mutation landed since the previous debounce tick.
    churned: bool,
    /// The quit flush was taken: nothing may mark or build after it, so a
    /// teardown event can never overwrite the final snapshot with an
    /// emptied workspace.
    shutting_down: bool,
}

impl Schedule {
    /// Record a workspace mutation. O(1): two boolean stores.
    pub fn mark(&mut self) {
        if !self.shutting_down {
            self.dirty = true;
            self.churned = true;
        }
    }

    /// Handle one debounce tick; `true` means the churn has settled and the
    /// snapshot should be taken now.
    pub fn debounce_settled(&mut self) -> bool {
        if self.shutting_down || !self.dirty {
            return false;
        }
        if self.churned {
            self.churned = false;
            return false;
        }
        true
    }

    /// A snapshot was built from the live model; the mirror is current.
    pub fn snapshot_taken(&mut self) {
        self.dirty = false;
        self.churned = false;
    }

    /// Latch the shutdown guard. Returns whether this call latched it (the
    /// quit path runs its final flush exactly once).
    pub fn begin_shutdown(&mut self) -> bool {
        let first = !self.shutting_down;
        self.shutting_down = true;
        first
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }
}

/// One answer from the asynchronous geometry poll.
#[derive(Debug, Clone, Copy)]
pub enum GeometrySample {
    /// The window's outer position, when the platform reports one.
    Position(Option<iced::Point>),
    Size(iced::Size),
    Scale(f32),
}

/// What one recorded sample means for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleOutcome {
    /// The sample changed the cached geometry — the workspace is dirty.
    pub changed: bool,
    /// This sample completed the current poll: time to build if dirty.
    pub poll_complete: bool,
}

/// An in-flight geometry poll: its identity (stale samples from a
/// superseded poll are ignored) and how many answers are still due.
#[derive(Debug)]
struct Poll {
    id: u64,
    pending: usize,
}

/// Run-scoped identity and geometry state behind the snapshot builder.
#[derive(Debug)]
pub struct Mirror {
    pub schedule: Schedule,
    /// Live smudgy windows in creation order, each with its stable
    /// workspace window id — the ordinal source named layouts will match
    /// against, never `window::Id` map order.
    windows: Vec<(window::Id, u64)>,
    next_window_id: u64,
    /// Stable session-slot ids, assigned when a session first enters a
    /// snapshot and held for the session's lifetime.
    slots: HashMap<SessionId, u64>,
    next_slot_id: u64,
    /// The last polled geometry per window — what snapshots serialize.
    /// Never read back from the live windows synchronously; the poll is the
    /// only source, which is what bounds a missed move to one checkpoint.
    geometry: HashMap<window::Id, dto::Geometry>,
    poll: Option<Poll>,
    next_poll_id: u64,
    generation: u64,
    /// The previous snapshot's serialized bytes: an identical rebuild is
    /// recognized and skipped, so spurious dirty marks cost no disk write.
    last_bytes: Option<Arc<[u8]>>,
}

impl Default for Mirror {
    fn default() -> Self {
        Self {
            schedule: Schedule::default(),
            windows: Vec::new(),
            next_window_id: 1,
            slots: HashMap::new(),
            next_slot_id: 1,
            geometry: HashMap::new(),
            poll: None,
            next_poll_id: 1,
            generation: 0,
            last_bytes: None,
        }
    }
}

impl Mirror {
    /// Track a freshly opened smudgy window, minting its stable id.
    /// Idempotent — re-announcing a known window changes nothing.
    pub fn register_window(&mut self, id: window::Id) {
        if self.windows.iter().any(|&(known, _)| known == id) {
            return;
        }
        self.windows.push((id, self.next_window_id));
        self.next_window_id += 1;
    }

    /// Forget a closed window (its stable id retires with it).
    pub fn forget_window(&mut self, id: window::Id) {
        self.windows.retain(|&(known, _)| known != id);
        self.geometry.remove(&id);
    }

    /// Live windows in creation order with their stable ids.
    pub fn window_entries(&self) -> impl Iterator<Item = (window::Id, u64)> + '_ {
        self.windows.iter().copied()
    }

    /// The stable slot id for a session, minted on first sight.
    pub fn slot_id(&mut self, session: SessionId) -> u64 {
        if let Some(&id) = self.slots.get(&session) {
            return id;
        }
        let id = self.next_slot_id;
        self.next_slot_id += 1;
        self.slots.insert(session, id);
        id
    }

    /// Drop a closed session's slot assignment.
    pub fn forget_session(&mut self, session: SessionId) {
        self.slots.remove(&session);
    }

    /// The last polled geometry for `window`, if any poll has answered.
    #[must_use]
    pub fn geometry_of(&self, window: window::Id) -> Option<&dto::Geometry> {
        self.geometry.get(&window)
    }

    /// Begin a geometry poll expecting `samples` answers, superseding any
    /// poll still in flight (its late samples will be ignored). Returns the
    /// poll id to stamp into the query tasks.
    pub fn begin_poll(&mut self, samples: usize) -> u64 {
        let id = self.next_poll_id;
        self.next_poll_id += 1;
        self.poll = Some(Poll {
            id,
            pending: samples,
        });
        id
    }

    /// Record one geometry answer. `poll` is the id the query was issued
    /// under, or `None` for a seed (window open) that neither dirties nor
    /// counts toward a poll. Change detection marks the schedule dirty —
    /// this is the geometry dirty signal.
    pub fn record_sample(
        &mut self,
        poll: Option<u64>,
        window: window::Id,
        sample: GeometrySample,
    ) -> SampleOutcome {
        let counted = match (&mut self.poll, poll) {
            (Some(current), Some(id)) if current.id == id => {
                current.pending = current.pending.saturating_sub(1);
                true
            }
            (_, Some(_)) => {
                // A superseded poll's answer: stale by definition. The
                // current poll re-queries everything, so nothing is lost.
                return SampleOutcome {
                    changed: false,
                    poll_complete: false,
                };
            }
            (_, None) => false,
        };

        let entry = self.geometry.entry(window);
        let changed = match entry {
            std::collections::hash_map::Entry::Occupied(mut cached) => {
                apply_sample(cached.get_mut(), sample)
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                // Geometry is cached only for registered windows, so a
                // sample landing after `forget_window` (a close racing an
                // in-flight poll) cannot resurrect an entry for a window no
                // snapshot will ever serialize. Every occupied entry is
                // therefore a registered window's — forgetting removes both.
                if self.windows.iter().any(|&(known, _)| known == window) {
                    // First sight of this window is the baseline, not a
                    // change: nothing older exists to have drifted from.
                    let mut geometry = dto::Geometry {
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                        scale: 1.0,
                    };
                    apply_sample(&mut geometry, sample);
                    vacant.insert(geometry);
                } else {
                    log::info!(
                        "[workspace] ignoring a geometry sample for unregistered window {window:?}"
                    );
                }
                false
            }
        };
        if changed && poll.is_some() {
            self.schedule.mark();
        }

        let poll_complete = counted
            && self
                .poll
                .as_ref()
                .is_some_and(|current| current.pending == 0);
        if poll_complete {
            self.poll = None;
        }
        SampleOutcome {
            changed,
            poll_complete,
        }
    }

    /// Adopt freshly serialized snapshot bytes: `None` when they are
    /// byte-identical to the previous snapshot (nothing to write), else the
    /// newly minted generation. `force` skips the identity check — the quit
    /// flush publishes unconditionally so its awaited ack always has a
    /// write behind it.
    pub fn adopt_bytes(&mut self, bytes: &Arc<[u8]>, force: bool) -> Option<u64> {
        if !force
            && self
                .last_bytes
                .as_ref()
                .is_some_and(|last| last.as_ref() == bytes.as_ref())
        {
            self.schedule.snapshot_taken();
            return None;
        }
        self.last_bytes = Some(Arc::clone(bytes));
        self.generation += 1;
        self.schedule.snapshot_taken();
        Some(self.generation)
    }
}

/// Fold one sample into a cached geometry; `true` when a field changed.
fn apply_sample(geometry: &mut dto::Geometry, sample: GeometrySample) -> bool {
    match sample {
        GeometrySample::Position(Some(point)) => {
            let changed = geometry.x != point.x || geometry.y != point.y;
            geometry.x = point.x;
            geometry.y = point.y;
            changed
        }
        // A platform without position reporting keeps whatever the cache
        // holds; restore clamps anyway.
        GeometrySample::Position(None) => false,
        GeometrySample::Size(size) => {
            let changed = geometry.width != size.width || geometry.height != size.height;
            geometry.width = size.width;
            geometry.height = size.height;
            changed
        }
        GeometrySample::Scale(scale) => {
            let changed = (geometry.scale - scale).abs() > f32::EPSILON;
            geometry.scale = scale;
            changed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mutation_then_one_quiet_tick_settles() {
        let mut schedule = Schedule::default();
        schedule.mark();
        // The tick right after the mutation sees churn and waits.
        assert!(!schedule.debounce_settled());
        // The next quiet tick fires.
        assert!(schedule.debounce_settled());
        schedule.snapshot_taken();
        assert!(!schedule.is_dirty());
        // Clean: ticks do nothing.
        assert!(!schedule.debounce_settled());
    }

    #[test]
    fn sustained_churn_never_fires_the_debounce() {
        let mut schedule = Schedule::default();
        for _ in 0..100 {
            schedule.mark();
            assert!(
                !schedule.debounce_settled(),
                "churn arriving every tick must keep deferring"
            );
        }
        // The checkpoint path reads the dirty flag directly — under churn
        // it is what bounds loss and write volume.
        assert!(schedule.is_dirty());
        schedule.snapshot_taken();
        assert!(!schedule.is_dirty());
    }

    #[test]
    fn shutdown_latches_and_freezes_the_schedule() {
        let mut schedule = Schedule::default();
        schedule.mark();
        assert!(schedule.begin_shutdown(), "first latch wins");
        assert!(!schedule.begin_shutdown(), "second quit path is a no-op");
        // Teardown mutations after the latch change nothing.
        schedule.mark();
        assert!(!schedule.debounce_settled());
        assert!(schedule.is_shutting_down());
    }

    #[test]
    fn window_registration_is_ordered_and_stable() {
        let mut mirror = Mirror::default();
        let (a, b, c) = (
            window::Id::unique(),
            window::Id::unique(),
            window::Id::unique(),
        );
        mirror.register_window(a);
        mirror.register_window(b);
        mirror.register_window(a); // idempotent
        let entries: Vec<_> = mirror.window_entries().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, a);
        assert_eq!(entries[1].0, b);
        assert!(
            entries[0].1 < entries[1].1,
            "stable ids follow creation order"
        );

        // Closing a window retires its id; a later window continues the
        // sequence rather than reusing it.
        mirror.forget_window(a);
        mirror.register_window(c);
        let entries: Vec<_> = mirror.window_entries().collect();
        assert_eq!(entries[0].0, b);
        assert_eq!(entries[1].0, c);
        assert!(entries[1].1 > entries[0].1);
    }

    #[test]
    fn slot_ids_are_stable_per_session() {
        let mut mirror = Mirror::default();
        let first = mirror.slot_id(SessionId::from(7));
        let second = mirror.slot_id(SessionId::from(9));
        assert_ne!(first, second);
        assert_eq!(mirror.slot_id(SessionId::from(7)), first);
        mirror.forget_session(SessionId::from(7));
        assert_ne!(
            mirror.slot_id(SessionId::from(7)),
            first,
            "a reopened session is a new slot"
        );
    }

    #[test]
    fn geometry_polling_change_detects_into_the_dirty_flag() {
        let mut mirror = Mirror::default();
        let win = window::Id::unique();
        mirror.register_window(win);

        // Seed (no poll): populates the cache without dirtying.
        let outcome = mirror.record_sample(
            None,
            win,
            GeometrySample::Position(Some(iced::Point::new(10.0, 20.0))),
        );
        assert!(!outcome.changed && !outcome.poll_complete);
        assert!(!mirror.schedule.is_dirty());

        // A checkpoint poll returning identical geometry stays clean.
        let poll = mirror.begin_poll(2);
        let outcome = mirror.record_sample(
            Some(poll),
            win,
            GeometrySample::Position(Some(iced::Point::new(10.0, 20.0))),
        );
        assert!(!outcome.changed && !outcome.poll_complete);
        let outcome = mirror.record_sample(
            Some(poll),
            win,
            GeometrySample::Size(iced::Size::new(0.0, 0.0)),
        );
        assert!(outcome.poll_complete, "second answer completes the poll");
        assert!(!mirror.schedule.is_dirty());

        // The window moved since: the poll IS the dirty signal.
        let poll = mirror.begin_poll(1);
        let outcome = mirror.record_sample(
            Some(poll),
            win,
            GeometrySample::Position(Some(iced::Point::new(300.0, 20.0))),
        );
        assert!(outcome.changed && outcome.poll_complete);
        assert!(mirror.schedule.is_dirty());
    }

    #[test]
    fn samples_for_unregistered_windows_leave_no_geometry_behind() {
        let mut mirror = Mirror::default();
        let win = window::Id::unique();

        // Never registered: the answer still counts toward its poll (the
        // poll's bookkeeping must complete either way) but caches nothing.
        let poll = mirror.begin_poll(1);
        let outcome = mirror.record_sample(
            Some(poll),
            win,
            GeometrySample::Size(iced::Size::new(800.0, 600.0)),
        );
        assert!(outcome.poll_complete);
        assert!(!outcome.changed);
        assert!(mirror.geometry_of(win).is_none());
        assert!(!mirror.schedule.is_dirty());

        // Forgotten mid-poll: the late answer must not resurrect the entry.
        mirror.register_window(win);
        mirror.record_sample(None, win, GeometrySample::Scale(1.0));
        assert!(mirror.geometry_of(win).is_some());
        let poll = mirror.begin_poll(1);
        mirror.forget_window(win);
        let outcome = mirror.record_sample(Some(poll), win, GeometrySample::Scale(2.0));
        assert!(outcome.poll_complete);
        assert!(mirror.geometry_of(win).is_none());
    }

    #[test]
    fn superseded_poll_answers_are_ignored() {
        let mut mirror = Mirror::default();
        let win = window::Id::unique();
        mirror.register_window(win);
        let stale = mirror.begin_poll(3);
        let fresh = mirror.begin_poll(1);

        // The stale poll's late answer neither counts nor dirties, even
        // with drifted geometry.
        let outcome = mirror.record_sample(
            Some(stale),
            win,
            GeometrySample::Position(Some(iced::Point::new(999.0, 999.0))),
        );
        assert_eq!(
            outcome,
            SampleOutcome {
                changed: false,
                poll_complete: false
            }
        );
        assert!(!mirror.schedule.is_dirty());

        let outcome = mirror.record_sample(Some(fresh), win, GeometrySample::Scale(1.0));
        assert!(outcome.poll_complete);
    }

    #[test]
    fn identical_snapshots_mint_no_generation() {
        let mut mirror = Mirror::default();
        let bytes: Arc<[u8]> = Arc::from(&b"snapshot"[..]);
        assert_eq!(mirror.adopt_bytes(&bytes, false), Some(1));
        mirror.schedule.mark();
        // Byte-identical rebuild: recognized, settled, unwritten.
        assert_eq!(mirror.adopt_bytes(&Arc::clone(&bytes), false), None);
        assert!(!mirror.schedule.is_dirty());
        // Changed bytes mint the next generation.
        let other: Arc<[u8]> = Arc::from(&b"different"[..]);
        assert_eq!(mirror.adopt_bytes(&other, false), Some(2));
        // The quit flush always publishes, identical or not.
        assert_eq!(mirror.adopt_bytes(&other, true), Some(3));
    }
}
