//! The in-session pending-write store: every mapper content mutation waits
//! here as a CAS envelope until the backend acknowledges it.
//!
//! One queue per area aggregate, strictly ordered; independent areas sync in
//! parallel. The displayed cache is the confirmed state plus these pending
//! operations (the optimistic overlay is applied at enqueue time and
//! rebuilt from a fresh fetch after conflicts). Nothing here is durable:
//! pending work is in-session by design, and quitting with unsent
//! operations warns and attempts a final flush instead of promising
//! restart recovery.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::{Notify, broadcast};

use crate::mutation::{AreaMutation, OperationId};
use crate::{AreaId, AreaWithDetails, RoomNumber};

/// Base delay of the transport-failure backoff schedule.
pub const BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Ceiling of the backoff schedule.
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Automatic attempts before a transport failure parks as `CouldNotSave`.
pub const MAX_TRANSPORT_ATTEMPTS: u32 = 8;

/// One queued mutation: the envelope body plus its user-facing intent.
#[derive(Debug, Clone)]
pub struct PendingEnvelope {
    pub operation_id: OperationId,
    pub ops: Vec<AreaMutation>,
    /// The whole gesture's name, undo-stack style ("Create room 17 and
    /// bidirectional link"), for conflict/failure surfacing.
    pub description: String,
    /// Client-only facts that distinguished a create from an update when
    /// the wire operation itself is an upsert. They are checked after a
    /// revision-conflict refetch and never serialized to the API.
    pub(crate) structural_preconditions: Vec<StructuralPrecondition>,
    /// Transport attempts so far.
    pub attempts: u32,
}

impl PendingEnvelope {
    /// Whether this envelope still has the same create/update meaning on a
    /// freshly fetched projection. Ordinary operation applicability is
    /// checked separately by the shared mutation applier.
    pub(crate) fn structural_preconditions_hold(&self, fresh: &AreaWithDetails) -> bool {
        self.structural_preconditions
            .iter()
            .all(|precondition| match precondition {
                StructuralPrecondition::RoomAbsent(room_number) => fresh
                    .rooms
                    .iter()
                    .all(|room| room.room_number != *room_number),
                StructuralPrecondition::RoomPresent(room_number) => fresh
                    .rooms
                    .iter()
                    .any(|room| room.room_number == *room_number),
            })
    }
}

/// A structural fact inferred from the optimistic base but absent from the
/// mirrored wire contract. This keeps an `UpsertRoom` that created a room
/// from silently becoming an update after conflict rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StructuralPrecondition {
    RoomAbsent(RoomNumber),
    RoomPresent(RoomNumber),
}

/// Why an area's queue is not currently sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AreaPhase {
    /// Head may send as soon as the worker reaches it.
    Ready,
    /// Head is on the wire.
    InFlight,
    /// Transport failure; retry when the deadline passes.
    Backoff { until: Instant },
    /// A pending operation failed the structural sanity check after a
    /// conflict refetch; paused for Keep mine / Keep theirs. The phase
    /// names the *failing* operation — not necessarily the head — so
    /// review and discard target exactly the envelope that no longer
    /// applies.
    Conflict { operation_id: OperationId },
    /// Validation/authorization/permanent failure; paused for
    /// Retry / Discard / Details.
    Failed { message: String },
}

/// The §5.6 area-specific save status the editor surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AreaSaveStatus {
    /// No pending operations.
    Saved,
    /// Queued or sending.
    Saving(usize),
    /// Retryable transport failure, retrying with backoff.
    Offline(usize),
    /// Queue paused at a failed sanity check.
    ConflictNeedsReview,
    /// Validation/auth/permanent failure awaiting user action.
    CouldNotSave(String),
}

/// Queue lifecycle events for UI subscription.
#[derive(Debug, Clone)]
pub enum MapperEvent {
    /// The backend accepted an envelope.
    MutationAcknowledged {
        area_id: AreaId,
        operation_id: OperationId,
    },
    /// A pending operation failed the post-refetch sanity check and the
    /// area's queue paused for a Keep mine / Keep theirs decision.
    /// `operation_id`/`description` name the failing envelope itself,
    /// which need not be the queue head.
    MutationConflict {
        area_id: AreaId,
        operation_id: OperationId,
        description: String,
    },
    /// An envelope failed permanently (validation/auth) and awaits
    /// Retry / Discard.
    MutationFailed {
        area_id: AreaId,
        operation_id: OperationId,
        message: String,
    },
    /// Any change to an area's save status.
    AreaStatusChanged { area_id: AreaId },
    /// The server requires a newer client; cloud syncing paused without
    /// discarding the session's pending queues.
    UpgradePaused,
}

/// Verdict of [`PendingQueue::transport_failure`]. All terminal accounting
/// keys off this returned verdict — never off a later status re-read, which
/// a concurrent resolution could race.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportVerdict {
    /// The head backed off (or the queue was empty); it will retry
    /// automatically and nothing is terminally counted.
    BackedOff,
    /// The attempt budget is spent and the head parked as permanently
    /// failed. The caller owns exactly one terminal count per park.
    Parked,
}

/// Outcome of [`PendingQueue::resolve_conflict`].
#[derive(Debug)]
pub struct ConflictResolution {
    /// Whether a conflict-paused area was actually resolved (`false` means
    /// nothing was paused and the call was a no-op).
    pub resolved: bool,
    /// The conflicted envelope removed by Keep theirs; `None` on Keep mine,
    /// or when the conflicted envelope had already left the queue.
    pub discarded: Option<PendingEnvelope>,
}

/// Outcome of [`PendingQueue::resolve_failure`].
#[derive(Debug)]
pub struct FailureResolution {
    /// Whether a parked (permanently-failed) area was actually un-parked
    /// (`false` means nothing was parked and the call was a no-op).
    pub unparked: bool,
    /// The parked head removed by a discard; `None` on a retry.
    pub discarded: Option<PendingEnvelope>,
}

#[derive(Debug, Default)]
struct AreaQueue {
    /// Last backend-acknowledged projected revision (mutation results, sync
    /// rows, and fresh fetches update it; optimistic cache revs never do).
    confirmed_rev: Option<i64>,
    /// Access fingerprint accompanying `confirmed_rev`, when known.
    fingerprint: Option<String>,
    queue: VecDeque<PendingEnvelope>,
    phase: AreaPhase,
}

impl Default for AreaPhase {
    fn default() -> Self {
        AreaPhase::Ready
    }
}

#[derive(Debug, Default)]
struct State {
    areas: HashMap<AreaId, AreaQueue>,
    /// Set on a 426: every cloud queue pauses, nothing is discarded.
    upgrade_paused: bool,
    /// Bumped on every enqueue. Display rebuilds compare it across their
    /// snapshot→swap window: a bump means an envelope (whose optimistic
    /// effect predates the swap) arrived mid-fold, so the fold must run
    /// again or its edit would vanish from the display.
    enqueue_epoch: u64,
}

/// The store. Cheap to share; all transitions run under one mutex and wake
/// the worker through `notify`.
pub struct PendingQueue {
    state: Mutex<State>,
    pub(crate) notify: Notify,
    events: broadcast::Sender<MapperEvent>,
}

impl Default for PendingQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingQueue {
    #[must_use]
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            state: Mutex::new(State::default()),
            notify: Notify::new(),
            events,
        }
    }

    /// Subscribe to queue lifecycle events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<MapperEvent> {
        self.events.subscribe()
    }

    pub(crate) fn emit(&self, event: MapperEvent) {
        let _ = self.events.send(event);
    }

    /// Appends an envelope to its area's queue and wakes the worker.
    pub fn enqueue(&self, area_id: AreaId, envelope: PendingEnvelope) {
        {
            let mut state = self.state.lock();
            state.enqueue_epoch += 1;
            let area = state.areas.entry(area_id).or_default();
            area.queue.push_back(envelope);
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
    }

    /// The enqueue epoch; see [`State::enqueue_epoch`].
    pub(crate) fn enqueue_epoch(&self) -> u64 {
        self.state.lock().enqueue_epoch
    }

    /// Records a backend-truth projected revision (fetch, sync row, or
    /// mutation result) for preconditions.
    ///
    /// Monotonicity (§2.2): within one projection class the confirmed
    /// revision never moves backward. Reports can arrive out of order — a
    /// sync row fetched before an acknowledgement can land after it — and a
    /// stale report regressing the precondition base would only manufacture
    /// spurious revision conflicts, so same-class reports fold with `max`.
    /// A *different* access fingerprint means the caller's capabilities
    /// (and therefore which counter the projected revision names —
    /// full vs public) changed; those counters are not mutually ordered,
    /// so a class-crossing report is accepted outright.
    pub fn note_confirmed_rev(&self, area_id: AreaId, rev: i64, fingerprint: Option<String>) {
        let mut state = self.state.lock();
        let area = state.areas.entry(area_id).or_default();
        let same_class = match (area.fingerprint.as_deref(), fingerprint.as_deref()) {
            (Some(old), Some(new)) => old == new,
            // An absent fingerprint (mutation results, legacy rows) cannot
            // prove a class change; treat it as same-class and clamp.
            _ => true,
        };
        area.confirmed_rev = Some(match area.confirmed_rev {
            Some(current) if same_class => current.max(rev),
            _ => rev,
        });
        if fingerprint.is_some() {
            area.fingerprint = fingerprint;
        }
    }

    /// The confirmed revision + fingerprint for building an envelope's
    /// precondition; `None` when no backend truth has been recorded yet.
    #[must_use]
    pub fn confirmed_rev(&self, area_id: AreaId) -> (Option<i64>, Option<String>) {
        let state = self.state.lock();
        state
            .areas
            .get(&area_id)
            .map_or((None, None), |a| (a.confirmed_rev, a.fingerprint.clone()))
    }

    /// The next sendable envelope across all areas, marking it in flight.
    /// Also reports the earliest backoff deadline when nothing is sendable
    /// yet, so the worker can sleep exactly long enough.
    pub(crate) fn take_ready(
        &self,
        now: Instant,
    ) -> (
        Option<(AreaId, PendingEnvelope, Option<i64>, Option<String>)>,
        Option<Instant>,
    ) {
        let mut state = self.state.lock();
        if state.upgrade_paused {
            return (None, None);
        }
        let mut earliest: Option<Instant> = None;
        // Iteration order is arbitrary; per-area order is what the contract
        // serializes. In-flight areas are skipped, so independent areas
        // still interleave across worker passes.
        let candidates: Vec<AreaId> = state.areas.keys().copied().collect();
        for area_id in candidates {
            let area = state.areas.get_mut(&area_id).expect("key just listed");
            match &area.phase {
                AreaPhase::Ready => {}
                AreaPhase::Backoff { until } => {
                    if *until > now {
                        earliest = Some(earliest.map_or(*until, |e| e.min(*until)));
                        continue;
                    }
                    area.phase = AreaPhase::Ready;
                }
                AreaPhase::InFlight | AreaPhase::Conflict { .. } | AreaPhase::Failed { .. } => {
                    continue;
                }
            }
            if let Some(envelope) = area.queue.front().cloned() {
                area.phase = AreaPhase::InFlight;
                let rev = area.confirmed_rev;
                let fingerprint = area.fingerprint.clone();
                return (Some((area_id, envelope, rev, fingerprint)), earliest);
            }
        }
        (None, earliest)
    }

    /// Acknowledges the in-flight head: pops it, records the resulting
    /// revision, and readies the queue.
    pub(crate) fn acknowledge(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
        new_rev: Option<i64>,
    ) {
        {
            let mut state = self.state.lock();
            if let Some(area) = state.areas.get_mut(&area_id) {
                if area
                    .queue
                    .front()
                    .is_some_and(|e| e.operation_id == operation_id)
                {
                    area.queue.pop_front();
                }
                if let Some(rev) = new_rev {
                    // An acknowledgement can echo a replayed idempotency
                    // receipt, whose revision is that of the *original*
                    // application and may predate fresher backend truth.
                    // §2.2: remove the operation, but never move the
                    // confirmed aggregate backward.
                    area.confirmed_rev =
                        Some(area.confirmed_rev.map_or(rev, |current| current.max(rev)));
                }
                area.phase = AreaPhase::Ready;
            }
        }
        self.emit(MapperEvent::MutationAcknowledged {
            area_id,
            operation_id,
        });
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
    }

    /// Readies the queue for the post-conflict resend: the head goes out
    /// again under the new confirmed revision (same operation id, so the
    /// server's idempotency receipt keeps the retry single-apply). Two
    /// callers: the reconcile path when every pending envelope passed the
    /// structural sanity check (phase still `InFlight`), and the Keep-mine
    /// resolution after its display rebuild (phase still `Conflict` — held
    /// paused so the resend can never race the rebuild's fold).
    pub(crate) fn ready_resend(&self, area_id: AreaId) {
        {
            let mut state = self.state.lock();
            if let Some(area) = state.areas.get_mut(&area_id)
                && matches!(area.phase, AreaPhase::InFlight | AreaPhase::Conflict { .. })
            {
                area.phase = AreaPhase::Ready;
            }
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
    }

    /// Parks the in-flight head after a transport failure, or fails it
    /// permanently once the attempt budget is spent. The returned verdict
    /// is the caller's sole accounting signal: exactly one `Parked` is
    /// returned per park, from the same lock that performed it.
    pub(crate) fn transport_failure(&self, area_id: AreaId, now: Instant) -> TransportVerdict {
        let mut failed: Option<(OperationId, String)> = None;
        {
            let mut state = self.state.lock();
            if let Some(area) = state.areas.get_mut(&area_id) {
                if let Some(head) = area.queue.front_mut() {
                    head.attempts += 1;
                    if head.attempts >= MAX_TRANSPORT_ATTEMPTS {
                        let message = "could not reach the map service".to_string();
                        failed = Some((head.operation_id, message.clone()));
                        area.phase = AreaPhase::Failed { message };
                    } else {
                        let exp = head.attempts.min(16);
                        let delay = BACKOFF_BASE
                            .saturating_mul(2u32.saturating_pow(exp))
                            .min(BACKOFF_CAP);
                        let jitter = Duration::from_millis(u64::from(fastrand_ms()) % 100);
                        area.phase = AreaPhase::Backoff {
                            until: now + delay + jitter,
                        };
                    }
                } else {
                    area.phase = AreaPhase::Ready;
                }
            }
        }
        let verdict = if failed.is_some() {
            TransportVerdict::Parked
        } else {
            TransportVerdict::BackedOff
        };
        if let Some((operation_id, message)) = failed {
            self.emit(MapperEvent::MutationFailed {
                area_id,
                operation_id,
                message,
            });
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
        verdict
    }

    /// Pauses an area's queue after a failed sanity check, targeting the
    /// envelope that failed — the whole queue holds (per-area order is the
    /// contract), but review and discard address exactly that operation.
    /// If the targeted envelope has already left the queue (an interleaved
    /// cancel), there is nothing to review and the queue reopens instead.
    pub(crate) fn pause_conflict(&self, area_id: AreaId, operation_id: OperationId) {
        let conflicted = {
            let mut state = self.state.lock();
            state.areas.get_mut(&area_id).and_then(|area| {
                let envelope = area
                    .queue
                    .iter()
                    .find(|e| e.operation_id == operation_id)
                    .cloned();
                area.phase = if envelope.is_some() {
                    AreaPhase::Conflict { operation_id }
                } else {
                    AreaPhase::Ready
                };
                envelope
            })
        };
        if let Some(envelope) = conflicted {
            self.emit(MapperEvent::MutationConflict {
                area_id,
                operation_id: envelope.operation_id,
                description: envelope.description,
            });
        } else {
            self.notify.notify_one();
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
    }

    /// Parks the in-flight head as permanently failed (validation/auth).
    pub(crate) fn permanent_failure(&self, area_id: AreaId, message: String) {
        let operation_id = {
            let mut state = self.state.lock();
            state.areas.get_mut(&area_id).and_then(|area| {
                area.phase = AreaPhase::Failed {
                    message: message.clone(),
                };
                area.queue.front().map(|e| e.operation_id)
            })
        };
        if let Some(operation_id) = operation_id {
            self.emit(MapperEvent::MutationFailed {
                area_id,
                operation_id,
                message,
            });
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
    }

    /// Pauses every queue on a 426 without discarding anything.
    pub(crate) fn pause_for_upgrade(&self) {
        {
            let mut state = self.state.lock();
            state.upgrade_paused = true;
            for area in state.areas.values_mut() {
                if area.phase == AreaPhase::InFlight {
                    area.phase = AreaPhase::Ready;
                }
            }
        }
        self.emit(MapperEvent::UpgradePaused);
    }

    /// Resumes queues paused by an upgrade requirement (a newer client
    /// signed in, or the floor moved).
    pub fn resume_after_upgrade(&self) {
        self.state.lock().upgrade_paused = false;
        self.notify.notify_one();
    }

    /// Keep mine: keep every pending operation (a deliberate overwrite of
    /// the fresher remote state). The queue stays paused — the caller
    /// rebuilds the display first and then calls [`Self::ready_resend`],
    /// so the resent head can never race the rebuild's fold. Keep theirs:
    /// discard exactly the conflicted envelope (queue order of the rest is
    /// preserved); later operations that depended on it will pause again
    /// at their own sanity checks.
    #[must_use]
    pub fn resolve_conflict(&self, area_id: AreaId, keep_mine: bool) -> ConflictResolution {
        const NOOP: ConflictResolution = ConflictResolution {
            resolved: false,
            discarded: None,
        };
        let discarded = {
            let mut state = self.state.lock();
            let Some(area) = state.areas.get_mut(&area_id) else {
                return NOOP;
            };
            let AreaPhase::Conflict { operation_id } = &area.phase else {
                return NOOP;
            };
            let operation_id = *operation_id;
            if keep_mine {
                None
            } else {
                area.phase = AreaPhase::Ready;
                area.queue
                    .iter()
                    .position(|e| e.operation_id == operation_id)
                    .and_then(|position| area.queue.remove(position))
            }
        };
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        if !keep_mine {
            self.notify.notify_one();
        }
        ConflictResolution {
            resolved: true,
            discarded,
        }
    }

    /// Retry a permanently-failed head, or discard it. The returned
    /// resolution is the caller's sole accounting signal: `unparked` comes
    /// from the same lock that performed the transition, so no status
    /// re-read can race a concurrent park.
    #[must_use]
    pub fn resolve_failure(&self, area_id: AreaId, retry: bool) -> FailureResolution {
        const NOOP: FailureResolution = FailureResolution {
            unparked: false,
            discarded: None,
        };
        let discarded = {
            let mut state = self.state.lock();
            let Some(area) = state.areas.get_mut(&area_id) else {
                return NOOP;
            };
            let AreaPhase::Failed { .. } = &area.phase else {
                return NOOP;
            };
            area.phase = AreaPhase::Ready;
            if retry {
                if let Some(head) = area.queue.front_mut() {
                    head.attempts = 0;
                }
                None
            } else {
                area.queue.pop_front()
            }
        };
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
        FailureResolution {
            unparked: true,
            discarded,
        }
    }

    /// Cancels a queued-but-unsent envelope (local undo of unacknowledged
    /// work). The head can cancel only while its queue is idle (`Ready`):
    /// an in-flight head is on the wire, and a parked head (backoff,
    /// conflict, failure) belongs to its resolution flow — cancelling it
    /// there would strand the park's terminal accounting and its phase.
    pub fn cancel(&self, area_id: AreaId, operation_id: OperationId) -> Option<PendingEnvelope> {
        let removed = {
            let mut state = self.state.lock();
            let area = state.areas.get_mut(&area_id)?;
            let position = area
                .queue
                .iter()
                .position(|e| e.operation_id == operation_id)?;
            if position == 0 && area.phase != AreaPhase::Ready {
                return None;
            }
            let removed = area.queue.remove(position);
            // Cancelling the (non-head) envelope a conflict pause targets
            // leaves nothing to review; reopen the queue.
            if let AreaPhase::Conflict {
                operation_id: conflicted,
            } = area.phase
                && conflicted == operation_id
            {
                area.phase = AreaPhase::Ready;
            }
            removed
        };
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        if removed.is_some() {
            self.notify.notify_one();
        }
        removed
    }

    /// The pending envelopes for an area, in order (for conflict previews
    /// and replay).
    #[must_use]
    pub fn pending_for(&self, area_id: AreaId) -> Vec<PendingEnvelope> {
        self.state
            .lock()
            .areas
            .get(&area_id)
            .map(|a| a.queue.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The operation currently paused for conflict review, if any.
    #[must_use]
    pub fn conflicted_operation_id(&self, area_id: AreaId) -> Option<OperationId> {
        let state = self.state.lock();
        let area = state.areas.get(&area_id)?;
        match area.phase {
            AreaPhase::Conflict { operation_id } => Some(operation_id),
            _ => None,
        }
    }

    /// Whether an operation is still queued for this area.
    #[must_use]
    pub fn contains_operation(&self, area_id: AreaId, operation_id: OperationId) -> bool {
        self.state.lock().areas.get(&area_id).is_some_and(|area| {
            area.queue
                .iter()
                .any(|envelope| envelope.operation_id == operation_id)
        })
    }

    /// Total pending envelopes across all areas.
    #[must_use]
    pub fn total_pending(&self) -> usize {
        self.state
            .lock()
            .areas
            .values()
            .map(|a| a.queue.len())
            .sum()
    }

    /// The §5.6 save status for one area.
    #[must_use]
    pub fn save_status(&self, area_id: AreaId) -> AreaSaveStatus {
        let state = self.state.lock();
        let Some(area) = state.areas.get(&area_id) else {
            return AreaSaveStatus::Saved;
        };
        let pending = area.queue.len();
        if pending == 0 {
            return AreaSaveStatus::Saved;
        }
        match &area.phase {
            AreaPhase::Conflict { .. } => AreaSaveStatus::ConflictNeedsReview,
            AreaPhase::Failed { message } => AreaSaveStatus::CouldNotSave(message.clone()),
            AreaPhase::Backoff { .. } => AreaSaveStatus::Offline(pending),
            AreaPhase::Ready | AreaPhase::InFlight => AreaSaveStatus::Saving(pending),
        }
    }
}

/// Millisecond jitter without a real RNG dependency: the low bits of the
/// monotonic clock are unpredictable enough to de-synchronize retry storms.
#[allow(clippy::cast_possible_truncation)]
fn fastrand_ms() -> u32 {
    (Instant::now().elapsed().subsec_nanos() ^ std::process::id()) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoomNumber;
    use uuid::Uuid;

    fn envelope(desc: &str) -> PendingEnvelope {
        PendingEnvelope {
            operation_id: Uuid::new_v4(),
            ops: vec![AreaMutation::DeleteRoom {
                room_number: RoomNumber(1),
            }],
            description: desc.to_string(),
            structural_preconditions: Vec::new(),
            attempts: 0,
        }
    }

    #[test]
    fn queues_serialize_per_area_and_interleave_across_areas() {
        let q = PendingQueue::new();
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());
        q.enqueue(area_a, envelope("a1"));
        q.enqueue(area_a, envelope("a2"));
        q.enqueue(area_b, envelope("b1"));
        q.enqueue(area_b, envelope("b2"));

        let now = Instant::now();
        let (first, _) = q.take_ready(now);
        let (area1, env1, _, _) = first.expect("head available");
        // The same area cannot send its second envelope while the first is
        // in flight, but the other area can.
        let (second, _) = q.take_ready(now);
        let (area2, _, _, _) = second.expect("other area available");
        assert_ne!(area1, area2);
        let (third, _) = q.take_ready(now);
        assert!(third.is_none(), "both areas in flight");

        // Acknowledging one area readies exactly that area's next envelope,
        // at the newly confirmed revision.
        q.acknowledge(area1, env1.operation_id, Some(5));
        let (fourth, _) = q.take_ready(now);
        let (area4, env4, rev4, _) = fourth.expect("second envelope for the acked area");
        assert_eq!(area4, area1);
        assert_eq!(rev4, Some(5));
        assert_ne!(env4.operation_id, env1.operation_id);
    }

    #[test]
    fn transport_failures_back_off_then_park() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        q.enqueue(area, envelope("op"));
        let now = Instant::now();
        for _ in 0..MAX_TRANSPORT_ATTEMPTS {
            let (taken, _) = q.take_ready(now + Duration::from_hours(1));
            assert!(taken.is_some(), "retry becomes ready after backoff");
            q.transport_failure(area, now);
        }
        assert!(matches!(
            q.save_status(area),
            AreaSaveStatus::CouldNotSave(_)
        ));
        // Nothing was dropped.
        assert_eq!(q.pending_for(area).len(), 1);
        // Retry re-arms the attempts budget and reports the un-park.
        let resolution = q.resolve_failure(area, true);
        assert!(resolution.unparked);
        assert!(resolution.discarded.is_none());
        assert!(matches!(q.save_status(area), AreaSaveStatus::Saving(1)));
        // Resolving an area that is not parked is a no-op.
        assert!(!q.resolve_failure(area, true).unparked);
    }

    #[test]
    fn transport_failure_reports_its_park_verdict_exactly_once() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        q.enqueue(area, envelope("op"));
        let now = Instant::now();
        for attempt in 1..=MAX_TRANSPORT_ATTEMPTS {
            let _ = q.take_ready(now + Duration::from_hours(1));
            let verdict = q.transport_failure(area, now);
            if attempt == MAX_TRANSPORT_ATTEMPTS {
                assert_eq!(
                    verdict,
                    TransportVerdict::Parked,
                    "the budget-spending failure parks"
                );
            } else {
                assert_eq!(verdict, TransportVerdict::BackedOff);
            }
        }
    }

    #[test]
    fn conflict_resolution_keeps_or_discards_the_conflicted_envelope() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        q.enqueue(area, envelope("mine"));
        let operation_id = q.pending_for(area)[0].operation_id;
        assert!(q.contains_operation(area, operation_id));
        assert_eq!(q.conflicted_operation_id(area), None);
        let (taken, _) = q.take_ready(Instant::now());
        let (_, env, _, _) = taken.expect("head");
        q.pause_conflict(area, env.operation_id);
        assert_eq!(q.save_status(area), AreaSaveStatus::ConflictNeedsReview);
        assert_eq!(q.conflicted_operation_id(area), Some(env.operation_id));

        // Keep mine: everything stays, and the queue stays paused until the
        // resolver's display rebuild releases the resend.
        let resolution = q.resolve_conflict(area, true);
        assert!(resolution.resolved);
        assert!(resolution.discarded.is_none());
        assert_eq!(q.pending_for(area).len(), 1);
        let (held, _) = q.take_ready(Instant::now());
        assert!(held.is_none(), "paused until ready_resend");
        q.ready_resend(area);

        let (retaken, _) = q.take_ready(Instant::now());
        assert_eq!(retaken.expect("resent").1.operation_id, env.operation_id);
        q.pause_conflict(area, env.operation_id);
        // Keep theirs: the conflicted envelope is discarded.
        let resolution = q.resolve_conflict(area, false);
        assert!(resolution.resolved);
        let discarded = resolution.discarded.expect("discarded");
        assert_eq!(discarded.operation_id, env.operation_id);
        assert_eq!(q.save_status(area), AreaSaveStatus::Saved);
        assert!(!q.contains_operation(area, env.operation_id));
        assert_eq!(q.conflicted_operation_id(area), None);
        // Resolving an area that is not conflict-paused is a no-op.
        assert!(!q.resolve_conflict(area, false).resolved);
    }

    #[test]
    fn conflict_pause_targets_the_failing_envelope_not_the_head() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let first = envelope("sane head");
        let second = envelope("failing follower");
        let first_id = first.operation_id;
        let second_id = second.operation_id;
        q.enqueue(area, first);
        q.enqueue(area, second);
        let mut events = q.subscribe();
        let _ = q.take_ready(Instant::now());

        // The sanity check failed on the follower, not the head.
        q.pause_conflict(area, second_id);
        let event = loop {
            if let MapperEvent::MutationConflict {
                operation_id,
                description,
                ..
            } = events.try_recv().expect("conflict event emitted")
            {
                break (operation_id, description);
            }
        };
        assert_eq!(event.0, second_id, "the event names the failing envelope");
        assert_eq!(event.1, "failing follower");

        // Keep theirs discards exactly the failing envelope; the sane head
        // survives in place and resumes.
        let resolution = q.resolve_conflict(area, false);
        assert_eq!(
            resolution.discarded.expect("discarded").operation_id,
            second_id
        );
        let remaining = q.pending_for(area);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].operation_id, first_id);
        let (retaken, _) = q.take_ready(Instant::now());
        assert_eq!(retaken.expect("head resumes").1.operation_id, first_id);
    }

    #[test]
    fn pausing_on_a_vanished_envelope_reopens_the_queue() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let first = envelope("head");
        let first_id = first.operation_id;
        q.enqueue(area, first);
        let _ = q.take_ready(Instant::now());
        // The targeted envelope is no longer queued (an interleaved cancel):
        // nothing to review, so the queue must not stick in Conflict.
        q.pause_conflict(area, Uuid::new_v4());
        let (retaken, _) = q.take_ready(Instant::now());
        assert_eq!(retaken.expect("queue reopened").1.operation_id, first_id);
    }

    #[test]
    fn cancel_removes_only_unsent_envelopes() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let first = envelope("sent");
        let second = envelope("unsent");
        let first_id = first.operation_id;
        let second_id = second.operation_id;
        q.enqueue(area, first);
        q.enqueue(area, second);
        let _ = q.take_ready(Instant::now());
        // Head is in flight: cannot cancel.
        assert!(q.cancel(area, first_id).is_none());
        // Queued follower cancels fine.
        assert!(q.cancel(area, second_id).is_some());
        assert_eq!(q.pending_for(area).len(), 1);
    }

    #[test]
    fn cancel_refuses_a_parked_head_but_allows_an_idle_one() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let head = envelope("parked");
        let head_id = head.operation_id;
        q.enqueue(area, head);
        let now = Instant::now();
        for _ in 0..MAX_TRANSPORT_ATTEMPTS {
            let _ = q.take_ready(now + Duration::from_hours(1));
            let _ = q.transport_failure(area, now);
        }
        assert!(matches!(
            q.save_status(area),
            AreaSaveStatus::CouldNotSave(_)
        ));
        // A parked head belongs to its resolution flow, whose park already
        // counted terminally — cancelling it would double-count.
        assert!(q.cancel(area, head_id).is_none());
        assert_eq!(q.pending_for(area).len(), 1);
        // Un-parking returns the queue to Ready, where the head may cancel.
        assert!(q.resolve_failure(area, true).unparked);
        assert!(q.cancel(area, head_id).is_some());
        assert_eq!(q.save_status(area), AreaSaveStatus::Saved);
    }

    #[test]
    fn confirmed_rev_never_regresses_within_a_projection_class() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let fp = || Some("fp-a".to_string());

        // The load lands, then an acknowledgement advances past it.
        q.note_confirmed_rev(area, 1, fp());
        q.enqueue(area, envelope("op"));
        let (taken, _) = q.take_ready(Instant::now());
        let (_, env, rev, _) = taken.expect("head");
        assert_eq!(rev, Some(1));
        q.acknowledge(area, env.operation_id, Some(2));
        assert_eq!(q.confirmed_rev(area).0, Some(2));

        // A sync row fetched before the acknowledgement lands after it:
        // the stale same-class report must not regress the base.
        q.note_confirmed_rev(area, 1, fp());
        assert_eq!(q.confirmed_rev(area).0, Some(2));

        // A replayed receipt echoing an old revision cannot regress either.
        q.enqueue(area, envelope("op2"));
        let (taken, _) = q.take_ready(Instant::now());
        let (_, env2, _, _) = taken.expect("head");
        q.acknowledge(area, env2.operation_id, Some(1));
        assert_eq!(q.confirmed_rev(area).0, Some(2));

        // A changed fingerprint switches the projection class, whose
        // counters are not mutually ordered: accept the report outright.
        q.note_confirmed_rev(area, 1, Some("fp-b".to_string()));
        assert_eq!(q.confirmed_rev(area).0, Some(1));
    }

    #[test]
    fn enqueue_bumps_the_epoch_and_nothing_else_does() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let before = q.enqueue_epoch();
        q.enqueue(area, envelope("op"));
        let after = q.enqueue_epoch();
        assert_ne!(before, after);
        let (taken, _) = q.take_ready(Instant::now());
        let (_, env, _, _) = taken.expect("head");
        q.acknowledge(area, env.operation_id, Some(2));
        assert_eq!(q.enqueue_epoch(), after, "acknowledge leaves the epoch");
    }

    #[test]
    fn upgrade_pause_holds_everything_without_loss() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        q.enqueue(area, envelope("op"));
        q.pause_for_upgrade();
        let (taken, _) = q.take_ready(Instant::now());
        assert!(taken.is_none());
        assert_eq!(q.pending_for(area).len(), 1);
        q.resume_after_upgrade();
        let (taken, _) = q.take_ready(Instant::now());
        assert!(taken.is_some());
    }
}
