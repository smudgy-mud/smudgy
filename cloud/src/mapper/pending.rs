//! The pending-write store: every mapper content mutation waits here as a CAS
//! envelope until the backend acknowledges it. Cloud envelopes are committed
//! to a server- and viewer-scoped write-ahead journal before their optimistic
//! effects become visible.
//!
//! One queue per area aggregate, strictly ordered; independent areas sync in
//! parallel. The displayed cache is the confirmed state plus these pending
//! operations (the optimistic overlay is applied at enqueue time and
//! rebuilt from a fresh fetch after conflicts). Cloud work is restart-durable;
//! local and ephemeral backends still use only the in-session queue.

use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(not(windows))]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use log::warn;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, broadcast, watch};
use uuid::Uuid;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

use crate::mutation::{AreaMutation, OperationId};
use crate::{AreaId, AreaWithDetails, CloudError, CloudResult, RoomNumber};

/// Base delay of the transport-failure backoff schedule.
pub const BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Ceiling of the backoff schedule.
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Automatic attempts before a transport failure parks as `CouldNotSave`.
pub const MAX_TRANSPORT_ATTEMPTS: u32 = 8;
const TERMINAL_COMPLETION_HISTORY: usize = 1024;
const JOURNAL_SCHEMA_VERSION: u32 = 3;
const LEGACY_JOURNAL_SCHEMA_VERSION: u32 = 2;
const COMMIT_SCHEMA_VERSION: u32 = 1;
const DELETE_TOMBSTONE_SCHEMA_VERSION: u32 = 1;
const RECEIPT_RETENTION_DAYS: i64 = 60;

fn sync_directory(directory: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let _ = directory;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        File::open(directory)?.sync_all()
    }
}

/// Rename whose return is the durable commit point. NTFS needs
/// `MOVEFILE_WRITE_THROUGH`; Unix needs an fsync of the containing directory.
fn durable_rename(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let source = fs::canonicalize(source)?;
        let destination = destination
            .parent()
            .ok_or_else(|| std::io::Error::other("rename destination has no parent"))
            .and_then(fs::canonicalize)?
            .join(
                destination
                    .file_name()
                    .ok_or_else(|| std::io::Error::other("rename destination has no file name"))?,
            );
        let source: Vec<u16> = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: both buffers are NUL-terminated and live for the call.
        let renamed = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        if renamed == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)?;
        if let Some(parent) = destination.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }
}

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
    /// Authenticated cloud viewer this work belongs to. `None` for local and
    /// ephemeral tiers.
    pub(crate) viewer_id: Option<Uuid>,
    /// Local on-disk areas use the same write-ahead boundary as cloud edits,
    /// but live in a server-local journal namespace. Ephemeral areas remain
    /// intentionally session-only.
    pub(crate) local_durable: bool,
    /// Credential generation authorized for the next dispatch. Restored
    /// records are rebound only after `/me` proves the same viewer.
    pub(crate) auth_generation: u64,
    /// Durable FIFO order within this server-scoped journal.
    pub(crate) sequence: u64,
    /// Used to park work beyond the server receipt-deduplication window.
    pub(crate) queued_at: DateTime<Utc>,
    /// Immutable record backing this envelope, when cloud-durable.
    pub(crate) journal_path: Option<PathBuf>,
    /// Restored work outside the server's idempotency-receipt window is never
    /// automatically dispatched or retryable.
    pub(crate) receipt_expired: bool,
    /// A staged envelope is deliberately invisible to the worker until the
    /// caller has installed its optimistic projection and accounting. Restored
    /// committed records start published.
    pub(crate) published: bool,
    /// Durable transaction whose commit marker makes this record replayable.
    pub(crate) journal_batch_id: Option<Uuid>,
    /// Restored under a durable pre-delete intent. The queue stays frozen
    /// until an authoritative fetch proves whether deletion committed.
    pub(crate) delete_intent: bool,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// The backend has already accepted the head, but its replayable `.json`
    /// body could not yet be atomically moved to cleanup-only `.ack`.
    /// Retrying this phase must never resend the mutation.
    AwaitingRetirement {
        operation_id: OperationId,
        new_rev: Option<i64>,
        attempts: u32,
        until: Instant,
    },
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
    Failed {
        operation_id: Option<OperationId>,
        message: String,
        retryable: bool,
    },
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
    CouldNotSave {
        message: String,
        /// Whether retrying can ever help. Restored edits outside the
        /// server's idempotency-receipt window are discard/export only.
        retryable: bool,
    },
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
    /// Last backend-acknowledged revision (mutation results, sync
    /// rows, and fresh fetches update it; optimistic cache revs never do).
    confirmed_rev: Option<i64>,
    /// Access fingerprint accompanying `confirmed_rev`, when known.
    fingerprint: Option<String>,
    queue: VecDeque<PendingEnvelope>,
    phase: AreaPhase,
    /// Restored journal work has not yet been folded over a freshly fetched
    /// projection for this viewer.
    requires_recovery_base: bool,
    /// The recovery base could not be fetched (normally because the area was
    /// deleted or access was revoked). A later successful fetch reopens this
    /// queue without disturbing unrelated permanent failures.
    recovery_base_failed: bool,
}

impl Default for AreaPhase {
    fn default() -> Self {
        AreaPhase::Ready
    }
}

fn retry_delay(attempts: u32) -> Duration {
    BACKOFF_BASE
        .saturating_mul(2u32.saturating_pow(attempts.min(16)))
        .min(BACKOFF_CAP)
}

#[derive(Debug, Default)]
struct State {
    areas: HashMap<AreaId, AreaQueue>,
    /// Areas under an acknowledged delete. New envelopes are rejected and
    /// queued envelopes cannot begin sending until the delete commits or
    /// aborts.
    deleting: HashSet<AreaId>,
    /// Delete requests that were durably prepared before a prior process
    /// reset. They are resolved against backend truth before WAL replay.
    delete_intents: HashSet<AreaId>,
    /// Set on a 426: every cloud queue pauses, nothing is discarded.
    upgrade_paused: bool,
    /// Bumped on every enqueue. Display rebuilds compare it across their
    /// snapshot→swap window: a bump means an envelope (whose optimistic
    /// effect predates the swap) arrived mid-fold, so the fold must run
    /// again or its edit would vanish from the display.
    /// Cloud journal namespace currently authorized by a successful identity
    /// resolution. Other viewers' records stay dormant on disk.
    active_viewer: Option<(Uuid, u64)>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DurablePendingBody {
    schema_version: u32,
    server_namespace: String,
    viewer_id: Uuid,
    auth_generation_at_enqueue: u64,
    area_id: AreaId,
    sequence: u64,
    queued_at: DateTime<Utc>,
    operation_id: OperationId,
    ops: Vec<AreaMutation>,
    description: String,
    structural_preconditions: Vec<StructuralPrecondition>,
    /// Absent only on schema-v2 records written before batch commit markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    batch_id: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DurablePendingRecord {
    checksum: String,
    #[serde(flatten)]
    body: DurablePendingBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct DurableCommitMember {
    server_namespace: String,
    viewer_id: Uuid,
    sequence: u64,
    operation_id: OperationId,
    record_checksum: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DurableCommitBody {
    schema_version: u32,
    batch_id: Uuid,
    members: Vec<DurableCommitMember>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DurableCommitRecord {
    checksum: String,
    #[serde(flatten)]
    body: DurableCommitBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CommittedRecordKey {
    batch_id: Uuid,
    member: DurableCommitMember,
}

#[derive(Debug, Serialize, Deserialize)]
struct DurableDeleteTombstoneBody {
    schema_version: u32,
    server_namespace: String,
    viewer_id: Uuid,
    area_id: AreaId,
    deleted_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DurableDeleteTombstoneRecord {
    checksum: String,
    #[serde(flatten)]
    body: DurableDeleteTombstoneBody,
}

#[derive(Debug)]
struct LoadedPending {
    area_id: AreaId,
    envelope: PendingEnvelope,
}

#[derive(Debug)]
pub(crate) struct PendingPublication {
    entries: Vec<(AreaId, OperationId)>,
}

#[derive(Debug, Default)]
pub(crate) struct ViewerActivation {
    pub removed: HashMap<AreaId, u64>,
    pub added: HashMap<AreaId, u64>,
    pub added_operations: Vec<OperationId>,
    pub removed_operations: Vec<OperationId>,
    pub expired_operations: Vec<(AreaId, OperationId)>,
}

#[derive(Debug, Default)]
struct CompletionRegistry {
    operations: HashMap<OperationId, watch::Sender<Option<Result<(), String>>>>,
    terminal_order: VecDeque<OperationId>,
}

/// The store. Cheap to share; all transitions run under one mutex and wake
/// the worker through `notify`.
pub struct PendingQueue {
    state: Mutex<State>,
    completions: Mutex<CompletionRegistry>,
    journal_root: PathBuf,
    server_namespace: String,
    next_sequence: AtomicU64,
    recovery_errors: Mutex<Vec<String>>,
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
        Self::with_journal_namespace(PathBuf::new(), "non-cloud".to_string())
    }

    #[must_use]
    pub fn with_journal(journal_root: PathBuf) -> Self {
        Self::with_journal_namespace(journal_root, "test-backend".to_string())
    }

    #[must_use]
    pub fn with_journal_namespace(journal_root: PathBuf, server_namespace: String) -> Self {
        let (events, _) = broadcast::channel(256);
        let queue = Self {
            state: Mutex::new(State::default()),
            completions: Mutex::new(CompletionRegistry::default()),
            journal_root,
            server_namespace,
            next_sequence: AtomicU64::new(1),
            recovery_errors: Mutex::new(Vec::new()),
            notify: Notify::new(),
            events,
        };
        queue.cleanup_retired_records();
        queue.restore_local_records();
        queue
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
    pub fn enqueue(&self, area_id: AreaId, envelope: PendingEnvelope) -> CloudResult<()> {
        let publication = self.enqueue_staged(area_id, envelope)?;
        self.publish_staged(publication);
        Ok(())
    }

    /// Write-ahead half of enqueue. A cloud record is fsynced before it
    /// becomes visible in memory; the caller publishes the optimistic cache
    /// and then calls [`Self::publish_enqueued`] to wake the worker.
    pub(crate) fn enqueue_staged(
        &self,
        area_id: AreaId,
        envelope: PendingEnvelope,
    ) -> CloudResult<PendingPublication> {
        self.enqueue_many_staged(vec![(area_id, envelope)])
    }

    /// Writes every journal body, then durably commits one manifest before
    /// staging any queue entry. A reset before the manifest cannot replay a
    /// prefix; a reset after it recovers every still-active member. Entries
    /// remain worker-invisible until [`Self::publish_staged`].
    pub(crate) fn enqueue_many_staged(
        &self,
        entries: Vec<(AreaId, PendingEnvelope)>,
    ) -> CloudResult<PendingPublication> {
        if entries.is_empty() {
            return Ok(PendingPublication {
                entries: Vec::new(),
            });
        }

        let validate = |state: &State, area_id: AreaId, envelope: &PendingEnvelope| {
            if state.deleting.contains(&area_id) || state.delete_intents.contains(&area_id) {
                return Err(CloudError::PendingOperations(
                    "this area is being deleted".to_string(),
                ));
            }
            if let Some(viewer_id) = envelope.viewer_id
                && state.active_viewer != Some((viewer_id, envelope.auth_generation))
            {
                return Err(CloudError::PendingOperations(
                    "cloud map identity is not ready for this edit".to_string(),
                ));
            }
            Ok(())
        };

        {
            let state = self.state.lock();
            for (area_id, envelope) in &entries {
                validate(&state, *area_id, envelope)?;
            }
        }

        let batch_id = Uuid::new_v4();
        let mut staged = Vec::with_capacity(entries.len());
        let mut commit_members = Vec::new();
        for (area_id, mut envelope) in entries {
            envelope.sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
            envelope.queued_at = Utc::now();
            envelope.published = false;
            let write = if envelope.viewer_id.is_some() || envelope.local_durable {
                self.write_journal_record(area_id, &envelope, batch_id)
            } else {
                Ok(None)
            };
            match write {
                Ok(Some((path, member))) => {
                    envelope.journal_path = Some(path);
                    envelope.journal_batch_id = Some(batch_id);
                    commit_members.push(member);
                }
                Ok(None) => {}
                Err(error) => {
                    for (_, written) in &staged {
                        if let Err(cleanup_error) = self.remove_uncommitted_record(written) {
                            warn!(
                                "failed to roll back staged mapper journal record: {cleanup_error}"
                            );
                        }
                    }
                    return Err(error);
                }
            }
            staged.push((area_id, envelope));
        }

        let operation_ids: Vec<_> = staged
            .iter()
            .map(|(area_id, envelope)| (*area_id, envelope.operation_id))
            .collect();
        {
            let mut state = self.state.lock();
            for (area_id, envelope) in &staged {
                if let Err(error) = validate(&state, *area_id, envelope) {
                    drop(state);
                    for (_, written) in &staged {
                        if let Err(cleanup_error) = self.remove_uncommitted_record(written) {
                            warn!(
                                "failed to roll back rejected mapper journal record: {cleanup_error}"
                            );
                        }
                    }
                    return Err(error);
                }
            }
            // Hold the state lock across the durable commit marker and memory
            // insertion. Identity/delete fencing therefore cannot invalidate
            // the transaction between its final validation and publication.
            if !commit_members.is_empty() {
                if let Err(error) = self.write_commit_marker(batch_id, &commit_members) {
                    drop(state);
                    for (_, written) in &staged {
                        if let Err(cleanup_error) = self.remove_uncommitted_record(written) {
                            warn!(
                                "failed to remove an uncommitted mapper journal record: {cleanup_error}"
                            );
                        }
                    }
                    return Err(error);
                }
            }
            for (area_id, envelope) in staged {
                state
                    .areas
                    .entry(area_id)
                    .or_default()
                    .queue
                    .push_back(envelope);
            }
        }
        let mut completions = self.completions.lock();
        for (_, operation_id) in &operation_ids {
            completions
                .operations
                .entry(*operation_id)
                .or_insert_with(|| watch::channel(None).0);
        }
        Ok(PendingPublication {
            entries: operation_ids,
        })
    }

    /// Atomically makes every staged member visible to the worker. The mapper
    /// calls this only after the optimistic cache and pending counters cover
    /// the entire transaction.
    pub(crate) fn publish_staged(&self, publication: PendingPublication) {
        if publication.entries.is_empty() {
            return;
        }
        let operation_ids: HashSet<_> = publication
            .entries
            .iter()
            .map(|(_, operation_id)| *operation_id)
            .collect();
        let mut published = 0;
        {
            let mut state = self.state.lock();
            for area in state.areas.values_mut() {
                for envelope in &mut area.queue {
                    if operation_ids.contains(&envelope.operation_id) && !envelope.published {
                        envelope.published = true;
                        published += 1;
                    }
                }
            }
        }
        debug_assert_eq!(
            published,
            operation_ids.len(),
            "every staged mapper operation must still be queued at publication"
        );
        for area_id in publication
            .entries
            .iter()
            .map(|(area_id, _)| *area_id)
            .collect::<HashSet<_>>()
        {
            self.emit(MapperEvent::AreaStatusChanged { area_id });
        }
        self.notify.notify_one();
    }

    fn checksum(body: &impl Serialize) -> CloudResult<String> {
        let bytes = serde_json::to_vec(body)?;
        let digest = Sha256::digest(bytes);
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "{byte:02x}");
        }
        Ok(encoded)
    }

    fn namespace_key(&self) -> String {
        Self::namespace_key_for(&self.server_namespace)
    }

    fn namespace_key_for(server_namespace: &str) -> String {
        let digest = Sha256::digest(server_namespace.as_bytes());
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "{byte:02x}");
        }
        encoded
    }

    fn viewer_directory(&self, viewer_id: Uuid) -> PathBuf {
        self.journal_root
            .join("servers")
            .join(self.namespace_key())
            .join("viewers")
            .join(viewer_id.to_string())
    }

    fn local_directory(&self) -> PathBuf {
        self.journal_root.join("local").join("active")
    }

    fn commit_directory(&self) -> PathBuf {
        self.journal_root.join("commits")
    }

    fn active_member_path(&self, member: &DurableCommitMember) -> PathBuf {
        let directory = if member.server_namespace == "local" && member.viewer_id == Uuid::nil() {
            self.local_directory()
        } else {
            self.journal_root
                .join("servers")
                .join(Self::namespace_key_for(&member.server_namespace))
                .join("viewers")
                .join(member.viewer_id.to_string())
        };
        directory.join(format!(
            "{:020}-{}.json",
            member.sequence, member.operation_id
        ))
    }

    fn retire_commit_marker_if_settled(&self, batch_id: Uuid) -> CloudResult<()> {
        if self.journal_root.as_os_str().is_empty() {
            return Ok(());
        }
        let marker = self.commit_directory().join(format!("{batch_id}.commit"));
        if !marker.exists() {
            return Ok(());
        }
        let record: DurableCommitRecord = serde_json::from_slice(&fs::read(&marker)?)?;
        if record
            .body
            .members
            .iter()
            .map(|member| self.active_member_path(member))
            .any(|path| path.exists())
        {
            return Ok(());
        }
        let retired = self.commit_directory().join("retired");
        fs::create_dir_all(&retired)?;
        sync_directory(&retired)?;
        let target = retired.join(format!("{batch_id}.retired"));
        durable_rename(&marker, &target)?;
        match fs::remove_file(&target) {
            Ok(()) => {
                if let Err(error) = sync_directory(&retired) {
                    warn!("failed to sync retired mapper commit markers: {error}");
                }
            }
            Err(error) => warn!(
                "failed to remove retired mapper commit marker {}: {error}; startup cleanup will retry",
                target.display()
            ),
        }
        Ok(())
    }

    fn write_commit_marker(
        &self,
        batch_id: Uuid,
        members: &[DurableCommitMember],
    ) -> CloudResult<()> {
        if self.journal_root.as_os_str().is_empty() || members.is_empty() {
            return Ok(());
        }
        let body = DurableCommitBody {
            schema_version: COMMIT_SCHEMA_VERSION,
            batch_id,
            members: members.to_vec(),
        };
        let record = DurableCommitRecord {
            checksum: Self::checksum(&body)?,
            body,
        };
        let directory = self.commit_directory();
        fs::create_dir_all(&directory)?;
        let final_path = directory.join(format!("{batch_id}.commit"));
        let temporary_path = directory.join(format!("{batch_id}.tmp-{}", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)?;
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.sync_all()?;
        drop(file);
        durable_rename(&temporary_path, &final_path)?;
        Ok(())
    }

    fn committed_record_keys(&self) -> HashSet<CommittedRecordKey> {
        if self.journal_root.as_os_str().is_empty() {
            return HashSet::new();
        }
        let entries = match fs::read_dir(self.commit_directory()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HashSet::new(),
            Err(error) => {
                self.recovery_errors
                    .lock()
                    .push(format!("failed to read mapper commit markers: {error}"));
                return HashSet::new();
            }
        };
        let mut committed = HashSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("commit") {
                if path.is_file() {
                    self.quarantine_record(
                        &path,
                        "uncommitted or unrecognized mapper commit marker".to_string(),
                    );
                }
                continue;
            }
            let record = match fs::read(&path).map_err(CloudError::from).and_then(|bytes| {
                serde_json::from_slice::<DurableCommitRecord>(&bytes).map_err(Into::into)
            }) {
                Ok(record) => record,
                Err(error) => {
                    self.quarantine_record(&path, format!("invalid commit marker: {error}"));
                    continue;
                }
            };
            if record.body.schema_version != COMMIT_SCHEMA_VERSION {
                self.quarantine_record(
                    &path,
                    format!(
                        "unsupported commit marker schema {}",
                        record.body.schema_version
                    ),
                );
                continue;
            }
            match Self::checksum(&record.body) {
                Ok(checksum) if checksum == record.checksum => {}
                Ok(_) => {
                    self.quarantine_record(&path, "commit marker checksum mismatch".to_string());
                    continue;
                }
                Err(error) => {
                    self.quarantine_record(
                        &path,
                        format!("commit marker checksum failed: {error}"),
                    );
                    continue;
                }
            }
            if !record
                .body
                .members
                .iter()
                .map(|member| self.active_member_path(member))
                .any(|path| path.exists())
            {
                if let Err(error) = self.retire_commit_marker_if_settled(record.body.batch_id) {
                    warn!(
                        "failed to garbage-collect settled mapper commit marker {}: {error}",
                        record.body.batch_id
                    );
                }
                continue;
            }
            committed.extend(
                record
                    .body
                    .members
                    .into_iter()
                    .map(|member| CommittedRecordKey {
                        batch_id: record.body.batch_id,
                        member,
                    }),
            );
        }
        committed
    }

    fn remove_uncommitted_record(&self, envelope: &PendingEnvelope) -> CloudResult<()> {
        let Some(path) = &envelope.journal_path else {
            return Ok(());
        };
        match fs::remove_file(path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// Atomically moves a backend-acknowledged WAL body out of the replayable
    /// `.json` namespace before any fallible cleanup. A reset or account
    /// reactivation can therefore never redispatch it.
    fn mark_journal_acknowledged(&self, envelope: &mut PendingEnvelope) -> CloudResult<()> {
        let Some(path) = envelope.journal_path.clone() else {
            return Ok(());
        };
        if path.extension().and_then(|extension| extension.to_str()) == Some("ack") {
            return Ok(());
        }
        let acknowledged = path.with_extension("ack");
        if path.exists() {
            durable_rename(&path, &acknowledged)?;
        } else if !acknowledged.exists() {
            return Err(CloudError::InternalError(format!(
                "mapper WAL record disappeared before acknowledgement: {}",
                path.display()
            )));
        }
        envelope.journal_path = Some(acknowledged);
        Ok(())
    }

    /// `.ack` bodies are already outside the replay namespace; startup can
    /// remove them directly and retry harmlessly after a reset.
    fn cleanup_acknowledged_record(&self, path: &Path) {
        match fs::remove_file(path) {
            Ok(()) => {
                if let Some(parent) = path.parent()
                    && let Err(error) = sync_directory(parent)
                {
                    warn!(
                        "failed to sync mapper journal after acknowledged-record cleanup: {error}"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                "failed to remove acknowledged mapper journal record {}: {error}; startup cleanup will retry",
                path.display()
            ),
        }
    }

    /// A record becomes non-replayable at the durable rename into `retired`.
    /// A crash may happen between that commit point and the best-effort
    /// unlink, so each startup removes any leftovers from this server
    /// namespace before loading active records.
    fn cleanup_retired_records(&self) {
        if self.journal_root.as_os_str().is_empty() {
            return;
        }
        let namespace_directory = self.journal_root.join("servers").join(self.namespace_key());
        let retired = namespace_directory.join("retired");
        match fs::remove_dir_all(&retired) {
            Ok(()) => {
                if let Err(error) = sync_directory(&namespace_directory) {
                    warn!(
                        "failed to sync mapper journal namespace after retired-record cleanup: {error}"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                "failed to remove retired mapper journal records from {}: {error}",
                retired.display()
            ),
        }
        let local_retired = self.journal_root.join("local").join("retired");
        match fs::remove_dir_all(&local_retired) {
            Ok(()) => {
                if let Some(parent) = local_retired.parent()
                    && let Err(error) = sync_directory(parent)
                {
                    warn!(
                        "failed to sync local mapper journal after retired-record cleanup: {error}"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                "failed to remove retired local mapper journal records from {}: {error}",
                local_retired.display()
            ),
        }
        let commit_retired = self.commit_directory().join("retired");
        match fs::remove_dir_all(&commit_retired) {
            Ok(()) => {
                if let Err(error) = sync_directory(&self.commit_directory()) {
                    warn!(
                        "failed to sync mapper commit directory after retired-marker cleanup: {error}"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                "failed to remove retired mapper commit markers from {}: {error}",
                commit_retired.display()
            ),
        }
    }

    fn write_journal_record(
        &self,
        area_id: AreaId,
        envelope: &PendingEnvelope,
        batch_id: Uuid,
    ) -> CloudResult<Option<(PathBuf, DurableCommitMember)>> {
        if self.journal_root.as_os_str().is_empty() {
            return Ok(None);
        }
        let (directory, viewer_id, server_namespace, auth_generation_at_enqueue) =
            if let Some(viewer_id) = envelope.viewer_id {
                (
                    self.viewer_directory(viewer_id),
                    viewer_id,
                    self.server_namespace.clone(),
                    envelope.auth_generation,
                )
            } else if envelope.local_durable {
                (self.local_directory(), Uuid::nil(), "local".to_string(), 0)
            } else {
                return Ok(None);
            };
        let body = DurablePendingBody {
            schema_version: JOURNAL_SCHEMA_VERSION,
            server_namespace,
            viewer_id,
            auth_generation_at_enqueue,
            area_id,
            sequence: envelope.sequence,
            queued_at: envelope.queued_at,
            operation_id: envelope.operation_id,
            ops: envelope.ops.clone(),
            description: envelope.description.clone(),
            structural_preconditions: envelope.structural_preconditions.clone(),
            batch_id: Some(batch_id),
        };
        let record = DurablePendingRecord {
            checksum: Self::checksum(&body)?,
            body,
        };
        fs::create_dir_all(&directory)?;
        let basename = format!("{:020}-{}.json", envelope.sequence, envelope.operation_id);
        let final_path = directory.join(&basename);
        let temporary_path = directory.join(format!("{basename}.tmp-{}", Uuid::new_v4()));
        let bytes = serde_json::to_vec(&record)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        durable_rename(&temporary_path, &final_path)?;
        Ok(Some((
            final_path,
            DurableCommitMember {
                server_namespace: record.body.server_namespace,
                viewer_id: record.body.viewer_id,
                sequence: record.body.sequence,
                operation_id: record.body.operation_id,
                record_checksum: record.checksum,
            },
        )))
    }

    fn quarantine_record(&self, path: &Path, message: String) {
        let report = format!("{}: {message}", path.display());
        warn!("mapper pending journal recovery: {report}");
        self.recovery_errors.lock().push(report);
        if self.journal_root.as_os_str().is_empty() {
            return;
        }
        let quarantine = self.journal_root.join("quarantine");
        if fs::create_dir_all(&quarantine).is_ok() {
            let name = path
                .file_name()
                .map_or_else(|| "record".into(), |name| name.to_owned());
            let target = quarantine.join(format!("{}-{}", Uuid::new_v4(), name.to_string_lossy()));
            if let Err(error) = fs::rename(path, target) {
                warn!(
                    "failed to quarantine mapper journal record {}: {error}",
                    path.display()
                );
            }
        }
    }

    fn record_is_committed(
        record: &DurablePendingRecord,
        committed: &HashSet<CommittedRecordKey>,
    ) -> bool {
        match record.body.schema_version {
            LEGACY_JOURNAL_SCHEMA_VERSION => record.body.batch_id.is_none(),
            JOURNAL_SCHEMA_VERSION => record.body.batch_id.is_some_and(|batch_id| {
                committed.contains(&CommittedRecordKey {
                    batch_id,
                    member: DurableCommitMember {
                        server_namespace: record.body.server_namespace.clone(),
                        viewer_id: record.body.viewer_id,
                        sequence: record.body.sequence,
                        operation_id: record.body.operation_id,
                        record_checksum: record.checksum.clone(),
                    },
                })
            }),
            _ => false,
        }
    }

    fn delete_marker_areas(
        &self,
        directory: &Path,
        expected_namespace: &str,
        expected_viewer: Uuid,
        extension: &str,
    ) -> HashSet<AreaId> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HashSet::new(),
            Err(error) => {
                self.recovery_errors.lock().push(format!(
                    "failed to read mapper deletion tombstones in {}: {error}",
                    directory.display()
                ));
                return HashSet::new();
            }
        };
        let mut deleted = HashSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some(extension) {
                continue;
            }
            let record = match fs::read(&path).map_err(CloudError::from).and_then(|bytes| {
                serde_json::from_slice::<DurableDeleteTombstoneRecord>(&bytes).map_err(Into::into)
            }) {
                Ok(record) => record,
                Err(error) => {
                    self.quarantine_record(&path, format!("invalid deletion {extension}: {error}"));
                    continue;
                }
            };
            if record.body.schema_version != DELETE_TOMBSTONE_SCHEMA_VERSION
                || record.body.server_namespace != expected_namespace
                || record.body.viewer_id != expected_viewer
            {
                self.quarantine_record(
                    &path,
                    format!("mapper deletion {extension} namespace mismatch"),
                );
                continue;
            }
            match Self::checksum(&record.body) {
                Ok(checksum) if checksum == record.checksum => {
                    deleted.insert(record.body.area_id);
                }
                Ok(_) => {
                    self.quarantine_record(
                        &path,
                        format!("mapper deletion {extension} checksum mismatch"),
                    );
                }
                Err(error) => {
                    self.quarantine_record(
                        &path,
                        format!("mapper deletion {extension} checksum failed: {error}"),
                    );
                }
            }
        }
        deleted
    }

    fn write_delete_marker(
        &self,
        directory: &Path,
        server_namespace: String,
        viewer_id: Uuid,
        area_id: AreaId,
        extension: &str,
    ) -> CloudResult<()> {
        if self.journal_root.as_os_str().is_empty() {
            return Ok(());
        }
        fs::create_dir_all(directory)?;
        let final_path = directory.join(format!("deleted-{area_id}.{extension}"));
        if final_path.exists() {
            return Ok(());
        }
        let body = DurableDeleteTombstoneBody {
            schema_version: DELETE_TOMBSTONE_SCHEMA_VERSION,
            server_namespace,
            viewer_id,
            area_id,
            deleted_at: Utc::now(),
        };
        let record = DurableDeleteTombstoneRecord {
            checksum: Self::checksum(&body)?,
            body,
        };
        let temporary_path = directory.join(format!(
            "deleted-{area_id}.{extension}.tmp-{}",
            Uuid::new_v4()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)?;
        file.write_all(&serde_json::to_vec(&record)?)?;
        file.sync_all()?;
        drop(file);
        durable_rename(&temporary_path, &final_path)?;
        Ok(())
    }

    fn durable_scopes(&self, envelopes: &[PendingEnvelope]) -> HashSet<(PathBuf, String, Uuid)> {
        let mut scopes = HashSet::new();
        for envelope in envelopes {
            if envelope.journal_path.is_none() {
                continue;
            }
            let scope = if let Some(viewer_id) = envelope.viewer_id {
                (
                    self.viewer_directory(viewer_id),
                    self.server_namespace.clone(),
                    viewer_id,
                )
            } else if envelope.local_durable {
                (self.local_directory(), "local".to_string(), Uuid::nil())
            } else {
                continue;
            };
            scopes.insert(scope);
        }
        scopes
    }

    fn write_delete_intents(
        &self,
        area_id: AreaId,
        envelopes: &[PendingEnvelope],
    ) -> CloudResult<()> {
        for (directory, namespace, viewer_id) in self.durable_scopes(envelopes) {
            self.write_delete_marker(&directory, namespace, viewer_id, area_id, "intent")?;
        }
        Ok(())
    }

    fn commit_delete_intents(
        &self,
        area_id: AreaId,
        envelopes: &[PendingEnvelope],
    ) -> CloudResult<()> {
        for (directory, namespace, viewer_id) in self.durable_scopes(envelopes) {
            let intent = directory.join(format!("deleted-{area_id}.intent"));
            let tombstone = directory.join(format!("deleted-{area_id}.tombstone"));
            if tombstone.exists() {
                continue;
            }
            if intent.exists() {
                durable_rename(&intent, &tombstone)?;
            } else {
                self.write_delete_marker(&directory, namespace, viewer_id, area_id, "tombstone")?;
            }
        }
        Ok(())
    }

    fn abort_delete_intents(
        &self,
        area_id: AreaId,
        envelopes: &[PendingEnvelope],
    ) -> CloudResult<()> {
        for (directory, _, _) in self.durable_scopes(envelopes) {
            let intent = directory.join(format!("deleted-{area_id}.intent"));
            match fs::remove_file(&intent) {
                Ok(()) => sync_directory(&directory)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn load_viewer_records(&self, viewer_id: Uuid, auth_generation: u64) -> Vec<LoadedPending> {
        if self.journal_root.as_os_str().is_empty() {
            return Vec::new();
        }
        let directory = self.viewer_directory(viewer_id);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                self.recovery_errors.lock().push(format!(
                    "failed to read mapper journal {}: {error}",
                    directory.display()
                ));
                return Vec::new();
            }
        };

        let committed = self.committed_record_keys();
        let deleted =
            self.delete_marker_areas(&directory, &self.server_namespace, viewer_id, "tombstone");
        let delete_intents =
            self.delete_marker_areas(&directory, &self.server_namespace, viewer_id, "intent");
        let mut loaded = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("ack") {
                self.cleanup_acknowledged_record(&path);
                continue;
            }
            if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("tombstone" | "intent")
            ) {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                if path.is_file() {
                    self.quarantine_record(
                        &path,
                        "uncommitted or unrecognized journal file".to_string(),
                    );
                }
                continue;
            }
            let record = match fs::read(&path).map_err(CloudError::from).and_then(|bytes| {
                serde_json::from_slice::<DurablePendingRecord>(&bytes).map_err(Into::into)
            }) {
                Ok(record) => record,
                Err(error) => {
                    self.quarantine_record(&path, format!("invalid record: {error}"));
                    continue;
                }
            };
            if !matches!(
                record.body.schema_version,
                LEGACY_JOURNAL_SCHEMA_VERSION | JOURNAL_SCHEMA_VERSION
            ) {
                self.quarantine_record(
                    &path,
                    format!("unsupported schema version {}", record.body.schema_version),
                );
                continue;
            }
            if !Self::record_is_committed(&record, &committed) {
                self.quarantine_record(
                    &path,
                    "journal record has no durable batch commit".to_string(),
                );
                continue;
            }
            if deleted.contains(&record.body.area_id) {
                if let Err(error) = fs::remove_file(&path) {
                    warn!(
                        "failed to remove mapper journal record suppressed by area deletion {}: {error}",
                        path.display()
                    );
                }
                continue;
            }
            if record.body.viewer_id != viewer_id {
                self.quarantine_record(&path, "viewer namespace mismatch".to_string());
                continue;
            }
            if record.body.server_namespace != self.server_namespace {
                self.quarantine_record(&path, "server namespace mismatch".to_string());
                continue;
            }
            match Self::checksum(&record.body) {
                Ok(checksum) if checksum == record.checksum => {}
                Ok(_) => {
                    self.quarantine_record(&path, "checksum mismatch".to_string());
                    continue;
                }
                Err(error) => {
                    self.quarantine_record(&path, format!("checksum failed: {error}"));
                    continue;
                }
            }
            let age = Utc::now().signed_duration_since(record.body.queued_at);
            let receipt_expired = age.num_days() >= RECEIPT_RETENTION_DAYS;
            loaded.push(LoadedPending {
                area_id: record.body.area_id,
                envelope: PendingEnvelope {
                    operation_id: record.body.operation_id,
                    ops: record.body.ops,
                    description: record.body.description,
                    structural_preconditions: record.body.structural_preconditions,
                    attempts: 0,
                    viewer_id: Some(viewer_id),
                    local_durable: false,
                    auth_generation,
                    sequence: record.body.sequence,
                    queued_at: record.body.queued_at,
                    journal_path: Some(path),
                    receipt_expired,
                    published: true,
                    journal_batch_id: record.body.batch_id,
                    delete_intent: delete_intents.contains(&record.body.area_id),
                },
            });
        }
        loaded.sort_by_key(|record| record.envelope.sequence);
        loaded
    }

    fn load_local_records(&self) -> Vec<LoadedPending> {
        if self.journal_root.as_os_str().is_empty() {
            return Vec::new();
        }
        let directory = self.local_directory();
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                self.recovery_errors.lock().push(format!(
                    "failed to read local mapper journal {}: {error}",
                    directory.display()
                ));
                return Vec::new();
            }
        };

        let committed = self.committed_record_keys();
        let deleted = self.delete_marker_areas(&directory, "local", Uuid::nil(), "tombstone");
        let delete_intents = self.delete_marker_areas(&directory, "local", Uuid::nil(), "intent");
        let mut loaded = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("ack") {
                self.cleanup_acknowledged_record(&path);
                continue;
            }
            if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("tombstone" | "intent")
            ) {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                if path.is_file() {
                    self.quarantine_record(
                        &path,
                        "uncommitted or unrecognized local journal file".to_string(),
                    );
                }
                continue;
            }
            let record = match fs::read(&path).map_err(CloudError::from).and_then(|bytes| {
                serde_json::from_slice::<DurablePendingRecord>(&bytes).map_err(Into::into)
            }) {
                Ok(record) => record,
                Err(error) => {
                    self.quarantine_record(&path, format!("invalid local record: {error}"));
                    continue;
                }
            };
            if !matches!(
                record.body.schema_version,
                LEGACY_JOURNAL_SCHEMA_VERSION | JOURNAL_SCHEMA_VERSION
            ) || record.body.viewer_id != Uuid::nil()
                || record.body.server_namespace != "local"
            {
                self.quarantine_record(&path, "local journal namespace mismatch".to_string());
                continue;
            }
            if !Self::record_is_committed(&record, &committed) {
                self.quarantine_record(
                    &path,
                    "local journal record has no durable batch commit".to_string(),
                );
                continue;
            }
            if deleted.contains(&record.body.area_id) {
                if let Err(error) = fs::remove_file(&path) {
                    warn!(
                        "failed to remove local mapper journal record suppressed by area deletion {}: {error}",
                        path.display()
                    );
                }
                continue;
            }
            match Self::checksum(&record.body) {
                Ok(checksum) if checksum == record.checksum => {}
                Ok(_) => {
                    self.quarantine_record(&path, "local journal checksum mismatch".to_string());
                    continue;
                }
                Err(error) => {
                    self.quarantine_record(
                        &path,
                        format!("local journal checksum failed: {error}"),
                    );
                    continue;
                }
            }
            loaded.push(LoadedPending {
                area_id: record.body.area_id,
                envelope: PendingEnvelope {
                    operation_id: record.body.operation_id,
                    ops: record.body.ops,
                    description: record.body.description,
                    structural_preconditions: record.body.structural_preconditions,
                    attempts: 0,
                    viewer_id: None,
                    local_durable: true,
                    auth_generation: 0,
                    sequence: record.body.sequence,
                    queued_at: record.body.queued_at,
                    journal_path: Some(path),
                    receipt_expired: false,
                    published: true,
                    journal_batch_id: record.body.batch_id,
                    delete_intent: delete_intents.contains(&record.body.area_id),
                },
            });
        }
        loaded.sort_by_key(|record| record.envelope.sequence);
        loaded
    }

    fn restore_local_records(&self) {
        let records = self.load_local_records();
        if records.is_empty() {
            return;
        }
        let mut state = self.state.lock();
        let mut completions = self.completions.lock();
        for record in records {
            self.next_sequence
                .fetch_max(record.envelope.sequence.saturating_add(1), Ordering::AcqRel);
            let operation_id = record.envelope.operation_id;
            if record.envelope.delete_intent {
                state.delete_intents.insert(record.area_id);
            }
            let area = state.areas.entry(record.area_id).or_default();
            area.requires_recovery_base = true;
            area.queue.push_back(record.envelope);
            completions
                .operations
                .entry(operation_id)
                .or_insert_with(|| watch::channel(None).0);
        }
    }

    #[must_use]
    pub(crate) fn recovered_local_operations(&self) -> Vec<(AreaId, OperationId)> {
        self.state
            .lock()
            .areas
            .iter()
            .flat_map(|(area_id, area)| {
                area.queue
                    .iter()
                    .filter(|envelope| envelope.local_durable)
                    .map(|envelope| (*area_id, envelope.operation_id))
            })
            .collect()
    }

    /// Activates exactly one authenticated viewer's cloud journal. Queues for
    /// the previous viewer become dormant on disk; local/ephemeral in-session
    /// queues remain active.
    pub(crate) fn activate_viewer(
        &self,
        viewer: Option<Uuid>,
        auth_generation: u64,
    ) -> ViewerActivation {
        let active = viewer.map(|viewer_id| (viewer_id, auth_generation));
        if self.state.lock().active_viewer == active {
            return ViewerActivation::default();
        }
        let records = viewer.map_or_else(Vec::new, |viewer_id| {
            self.load_viewer_records(viewer_id, auth_generation)
        });
        let mut activation = ViewerActivation::default();
        let mut changed = std::collections::HashSet::new();
        {
            let mut state = self.state.lock();
            let cloud_areas: Vec<_> = state
                .areas
                .iter()
                .filter_map(|(area_id, area)| {
                    area.queue
                        .front()
                        .and_then(|envelope| envelope.viewer_id.is_some().then_some(*area_id))
                })
                .collect();
            for area_id in cloud_areas {
                if let Some(area) = state.areas.remove(&area_id) {
                    state.delete_intents.remove(&area_id);
                    activation.removed.insert(area_id, area.queue.len() as u64);
                    activation
                        .removed_operations
                        .extend(area.queue.iter().map(|envelope| envelope.operation_id));
                    changed.insert(area_id);
                }
            }
            state.active_viewer = active;
            for record in records {
                self.next_sequence
                    .fetch_max(record.envelope.sequence.saturating_add(1), Ordering::AcqRel);
                if record.envelope.delete_intent {
                    state.delete_intents.insert(record.area_id);
                }
                let area = state.areas.entry(record.area_id).or_default();
                area.requires_recovery_base = true;
                activation
                    .added
                    .entry(record.area_id)
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                activation
                    .added_operations
                    .push(record.envelope.operation_id);
                if record.envelope.receipt_expired {
                    activation
                        .expired_operations
                        .push((record.area_id, record.envelope.operation_id));
                }
                area.queue.push_back(record.envelope);
                changed.insert(record.area_id);
            }
            for area_id in &changed {
                if let Some(area) = state.areas.get_mut(area_id) {
                    Self::park_expired_head(area);
                }
            }
        }
        for operation_id in &activation.removed_operations {
            self.complete_operation(
                *operation_id,
                Err(
                    "map credential changed; this durable edit remains saved for its original account"
                        .to_string(),
                ),
            );
        }
        for operation_id in &activation.added_operations {
            let terminal = self
                .completions
                .lock()
                .operations
                .get(operation_id)
                .is_some_and(|sender| sender.borrow().is_some());
            if terminal {
                self.reset_completion(*operation_id);
            } else {
                self.completions
                    .lock()
                    .operations
                    .entry(*operation_id)
                    .or_insert_with(|| watch::channel(None).0);
            }
        }
        for (area_id, operation_id) in &activation.expired_operations {
            let message = format!(
                "saved map edit {operation_id} is older than the server's {RECEIPT_RETENTION_DAYS}-day replay window and cannot be retried automatically"
            );
            self.complete_operation(*operation_id, Err(message.clone()));
            self.emit(MapperEvent::MutationFailed {
                area_id: *area_id,
                operation_id: *operation_id,
                message,
            });
        }
        for area_id in changed {
            self.emit(MapperEvent::AreaStatusChanged { area_id });
        }
        self.notify.notify_one();
        activation
    }

    fn park_expired_head(area: &mut AreaQueue) {
        if let Some(head) = area.queue.front()
            && head.receipt_expired
        {
            area.phase = AreaPhase::Failed {
                operation_id: Some(head.operation_id),
                message: format!(
                    "saved map edit {} is older than the server's {RECEIPT_RETENTION_DAYS}-day replay window and cannot be retried automatically",
                    head.operation_id
                ),
                retryable: false,
            };
        }
    }

    #[must_use]
    pub fn recovery_errors(&self) -> Vec<String> {
        self.recovery_errors.lock().clone()
    }

    pub(crate) fn take_recovery_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.recovery_errors.lock())
    }

    #[must_use]
    pub(crate) fn active_viewer(&self) -> Option<(Uuid, u64)> {
        self.state.lock().active_viewer
    }

    #[must_use]
    pub(crate) fn requires_recovery_base(&self, area_id: AreaId) -> bool {
        self.state
            .lock()
            .areas
            .get(&area_id)
            .is_some_and(|area| area.requires_recovery_base)
    }

    #[must_use]
    pub(crate) fn has_delete_intent(&self, area_id: AreaId) -> bool {
        self.state.lock().delete_intents.contains(&area_id)
    }

    /// Whether an in-session delete/move fence or a recovered durable delete
    /// intent currently makes metadata writes unsafe for this area.
    pub(crate) fn is_delete_fenced(&self, area_id: AreaId) -> bool {
        let state = self.state.lock();
        state.deleting.contains(&area_id) || state.delete_intents.contains(&area_id)
    }

    #[must_use]
    pub(crate) fn delete_intent_is_cloud(&self, area_id: AreaId) -> bool {
        self.state
            .lock()
            .areas
            .get(&area_id)
            .and_then(|area| area.queue.front())
            .is_some_and(|envelope| envelope.viewer_id.is_some())
    }

    #[must_use]
    pub(crate) fn recovery_area_ids(&self) -> Vec<AreaId> {
        self.state
            .lock()
            .areas
            .iter()
            .filter_map(|(area_id, area)| area.requires_recovery_base.then_some(*area_id))
            .collect()
    }

    pub(crate) fn recovery_base_loaded(&self, area_id: AreaId) -> bool {
        let reopened = {
            let mut state = self.state.lock();
            let Some(area) = state.areas.get_mut(&area_id) else {
                return false;
            };
            area.requires_recovery_base = false;
            if area.recovery_base_failed {
                area.recovery_base_failed = false;
                area.phase = AreaPhase::Ready;
                area.queue.front().map(|envelope| envelope.operation_id)
            } else {
                None
            }
        };
        if let Some(operation_id) = reopened {
            self.reset_completion(operation_id);
            self.emit(MapperEvent::AreaStatusChanged { area_id });
            self.notify.notify_one();
        }
        reopened.is_some()
    }

    /// Parks a restored queue whose authoritative base cannot currently be
    /// fetched. The durable record remains intact and a future successful
    /// sync reopens it through [`Self::recovery_base_loaded`].
    pub(crate) fn recovery_base_unavailable(&self, area_id: AreaId, message: String) -> bool {
        let operation_id = {
            let mut state = self.state.lock();
            let Some(area) = state.areas.get_mut(&area_id) else {
                return false;
            };
            if !area.requires_recovery_base || area.recovery_base_failed {
                return false;
            }
            let Some(operation_id) = area.queue.front().map(|envelope| envelope.operation_id)
            else {
                return false;
            };
            area.recovery_base_failed = true;
            area.phase = AreaPhase::Failed {
                operation_id: Some(operation_id),
                message: message.clone(),
                retryable: true,
            };
            operation_id
        };
        self.complete_operation(operation_id, Err(message.clone()));
        self.recovery_errors
            .lock()
            .push(format!("area {area_id}: {message}"));
        self.emit(MapperEvent::MutationFailed {
            area_id,
            operation_id,
            message,
        });
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        true
    }

    /// Marks an exact-generation dispatch as stale without failing or
    /// discarding it. Clearing the active viewer makes every durable cloud
    /// queue dormant until the sync engine resolves `/me` for the new
    /// credential generation.
    pub(crate) fn credential_changed(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
        auth_generation: u64,
    ) {
        {
            let mut state = self.state.lock();
            let is_current = state.areas.get(&area_id).is_some_and(|area| {
                area.phase == AreaPhase::InFlight
                    && area
                        .queue
                        .front()
                        .is_some_and(|envelope| envelope.operation_id == operation_id)
            });
            if !is_current {
                return;
            }
            if state
                .active_viewer
                .is_some_and(|(_, active_generation)| active_generation == auth_generation)
            {
                state.active_viewer = None;
            }
            if let Some(area) = state.areas.get_mut(&area_id) {
                area.phase = AreaPhase::Ready;
            }
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
    }

    fn complete_operation(&self, operation_id: OperationId, outcome: Result<(), String>) {
        let mut completions = self.completions.lock();
        let sender = completions
            .operations
            .entry(operation_id)
            .or_insert_with(|| watch::channel(None).0);
        sender.send_replace(Some(outcome));
        completions.terminal_order.push_back(operation_id);
        while completions.terminal_order.len() > TERMINAL_COMPLETION_HISTORY {
            if let Some(expired) = completions.terminal_order.pop_front() {
                completions.operations.remove(&expired);
            }
        }
    }

    /// Starts a fresh completion epoch for an operation that remains durable
    /// after a terminal session-local outcome (account switch or temporarily
    /// unavailable recovery base). Existing receivers retain the old result;
    /// new waiters observe the reactivated attempt.
    fn reset_completion(&self, operation_id: OperationId) {
        let mut completions = self.completions.lock();
        completions
            .terminal_order
            .retain(|terminal| *terminal != operation_id);
        completions
            .operations
            .insert(operation_id, watch::channel(None).0);
    }

    /// Durably removes a record from the replay namespace. The write-through
    /// rename is the commit point; the retired body is then unlinked so
    /// acknowledged/discarded map content is not retained indefinitely.
    fn retire_journal(&self, envelope: &PendingEnvelope) -> CloudResult<()> {
        let Some(path) = &envelope.journal_path else {
            return Ok(());
        };
        if path.extension().and_then(|extension| extension.to_str()) == Some("ack") {
            // The durable rename into `.ack` already made this operation
            // non-replayable. Everything after that commit point is detached
            // housekeeping and must not hold its waiter/counters pending.
            self.cleanup_acknowledged_record(path);
            if let Some(batch_id) = envelope.journal_batch_id
                && let Err(error) = self.retire_commit_marker_if_settled(batch_id)
            {
                warn!(
                    "failed to garbage-collect mapper commit marker {batch_id} after acknowledgement: {error}"
                );
            }
            return Ok(());
        }
        let retired = if let Some(viewer_id) = envelope.viewer_id {
            self.journal_root
                .join("servers")
                .join(self.namespace_key())
                .join("retired")
                .join(viewer_id.to_string())
        } else if envelope.local_durable {
            self.journal_root.join("local").join("retired")
        } else {
            return Err(CloudError::InternalError(
                "journal record has no durable scope".to_string(),
            ));
        };
        if path.exists() {
            fs::create_dir_all(&retired)?;
            sync_directory(&retired)?;
            let target = retired.join(format!(
                "{:020}-{}.retired",
                envelope.sequence, envelope.operation_id
            ));
            durable_rename(path, &target)?;
            match fs::remove_file(&target) {
                Ok(()) => {
                    if let Err(error) = sync_directory(&retired) {
                        warn!(
                            "failed to sync retired mapper journal directory after cleanup: {error}"
                        );
                    }
                }
                Err(error) => warn!(
                    "failed to remove retired mapper journal body {}: {error}; startup cleanup will retry",
                    target.display()
                ),
            }
        }
        if let Some(batch_id) = envelope.journal_batch_id
            && let Err(error) = self.retire_commit_marker_if_settled(batch_id)
        {
            warn!(
                "failed to garbage-collect mapper commit marker {batch_id} after record retirement: {error}"
            );
        }
        Ok(())
    }

    /// Wait until one queued operation is acknowledged or reaches a
    /// user-action terminal state. Terminal outcomes are retained briefly so
    /// an immediate acknowledgement cannot race past a script waiter.
    pub async fn wait_for_completion(&self, operation_id: OperationId) -> Result<(), String> {
        let mut receiver = {
            let completions = self.completions.lock();
            completions
                .operations
                .get(&operation_id)
                .ok_or_else(|| format!("unknown mapper operation {operation_id}"))?
                .subscribe()
        };

        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return outcome;
            }
            receiver
                .changed()
                .await
                .map_err(|_| "mapper operation completion channel closed".to_string())?;
        }
    }

    /// Records a backend-truth revision (fetch, sync row, or
    /// mutation result) for preconditions.
    ///
    /// Reports can arrive out of order: a sync row fetched before an
    /// acknowledgement may land after it. Every reader sees the same true
    /// revision, so unordered hints always fold with `max`; only an
    /// authoritative document reconstruction may deliberately rewind it.
    pub fn note_confirmed_rev(&self, area_id: AreaId, rev: i64, fingerprint: Option<String>) {
        let mut state = self.state.lock();
        let area = state.areas.entry(area_id).or_default();
        area.confirmed_rev = Some(match area.confirmed_rev {
            Some(current) => current.max(rev),
            None => rev,
        });
        if fingerprint.is_some() {
            area.fingerprint = fingerprint;
        }
    }

    /// Adopts a revision from an authoritative area document after the
    /// caller has ruled out an in-flight stale response. Unlike unordered
    /// row and acknowledgement hints, a reconstruction may legitimately
    /// move this value downward.
    pub(crate) fn adopt_confirmed_rev(
        &self,
        area_id: AreaId,
        rev: i64,
        fingerprint: Option<String>,
    ) {
        let mut state = self.state.lock();
        let area = state.areas.entry(area_id).or_default();
        area.confirmed_rev = Some(rev);
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
        let active_viewer = state.active_viewer;
        for area_id in candidates {
            if state.deleting.contains(&area_id) || state.delete_intents.contains(&area_id) {
                continue;
            }
            let area = state.areas.get_mut(&area_id).expect("key just listed");
            if area.requires_recovery_base {
                continue;
            }
            match &area.phase {
                AreaPhase::Ready => {}
                AreaPhase::Backoff { until } => {
                    if *until > now {
                        earliest = Some(earliest.map_or(*until, |e| e.min(*until)));
                        continue;
                    }
                    area.phase = AreaPhase::Ready;
                }
                AreaPhase::AwaitingRetirement { until, .. } => {
                    earliest = Some(earliest.map_or(*until, |earliest| earliest.min(*until)));
                    continue;
                }
                AreaPhase::InFlight | AreaPhase::Conflict { .. } | AreaPhase::Failed { .. } => {
                    continue;
                }
            }
            if let Some(envelope) = area.queue.front().cloned() {
                if !envelope.published {
                    continue;
                }
                if envelope.viewer_id.is_some()
                    && envelope.viewer_id.zip(Some(envelope.auth_generation)) != active_viewer
                {
                    // The credential changed after this queue was activated.
                    // Leave it dormant until the identity tick reactivates the
                    // matching viewer namespace.
                    continue;
                }
                area.phase = AreaPhase::InFlight;
                let rev = area.confirmed_rev;
                let fingerprint = area.fingerprint.clone();
                return (Some((area_id, envelope, rev, fingerprint)), earliest);
            }
        }
        (None, earliest)
    }

    /// Retries only the atomic `.json` → `.ack` transition for a
    /// backend-successful operation. The mutation itself is never dispatched
    /// again; cleanup after `.ack` is best-effort and does not enter this phase.
    pub(crate) fn retry_ready_retirement(
        &self,
        now: Instant,
    ) -> (Option<(AreaId, OperationId)>, Option<Instant>) {
        let mut earliest = None;
        let mut settled = None;
        {
            let mut state = self.state.lock();
            let candidates: Vec<_> = state.areas.keys().copied().collect();
            for area_id in candidates {
                if state.deleting.contains(&area_id) || state.delete_intents.contains(&area_id) {
                    continue;
                }
                let area = state.areas.get_mut(&area_id).expect("key just listed");
                let AreaPhase::AwaitingRetirement {
                    operation_id,
                    new_rev,
                    attempts,
                    until,
                } = area.phase.clone()
                else {
                    continue;
                };
                if until > now {
                    earliest =
                        Some(earliest.map_or(until, |deadline: Instant| deadline.min(until)));
                    continue;
                }
                let Some(envelope) = area
                    .queue
                    .front_mut()
                    .filter(|envelope| envelope.operation_id == operation_id)
                else {
                    area.phase = AreaPhase::Ready;
                    continue;
                };
                let retirement = self
                    .mark_journal_acknowledged(envelope)
                    .and_then(|()| self.retire_journal(envelope));
                match retirement {
                    Ok(()) => {
                        let _ = area.queue.pop_front();
                        if let Some(rev) = new_rev {
                            area.confirmed_rev =
                                Some(area.confirmed_rev.map_or(rev, |current| current.max(rev)));
                        }
                        area.phase = AreaPhase::Ready;
                        Self::park_expired_head(area);
                        settled = Some((area_id, operation_id));
                        break;
                    }
                    Err(error) => {
                        let attempts = attempts.saturating_add(1);
                        let until = now + retry_delay(attempts);
                        warn!(
                            "mapper WAL acknowledgement transition retry {attempts} failed for {operation_id}: {error}"
                        );
                        area.phase = AreaPhase::AwaitingRetirement {
                            operation_id,
                            new_rev,
                            attempts,
                            until,
                        };
                        earliest =
                            Some(earliest.map_or(until, |deadline: Instant| deadline.min(until)));
                    }
                }
            }
        }
        if let Some((area_id, operation_id)) = settled {
            self.emit(MapperEvent::MutationAcknowledged {
                area_id,
                operation_id,
            });
            self.complete_operation(operation_id, Ok(()));
            self.emit(MapperEvent::AreaStatusChanged { area_id });
            self.notify.notify_one();
        }
        (settled, earliest)
    }

    /// Acknowledges the in-flight head: pops it, records the resulting
    /// revision, and readies the queue.
    pub(crate) fn acknowledge(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
        new_rev: Option<i64>,
    ) -> bool {
        let removed = {
            let mut state = self.state.lock();
            let mut removed = None;
            if let Some(area) = state.areas.get_mut(&area_id) {
                if area
                    .queue
                    .front()
                    .is_some_and(|e| e.operation_id == operation_id)
                {
                    let envelope = area.queue.front_mut().expect("front just checked");
                    let retirement = self
                        .mark_journal_acknowledged(envelope)
                        .and_then(|()| self.retire_journal(envelope));
                    if let Err(error) = retirement {
                        warn!(
                            "failed to durably move acknowledged mapper journal record {} out of the replay namespace: {error}; retrying that local transition without resending the mutation",
                            envelope.operation_id
                        );
                        let attempts = 1;
                        area.phase = AreaPhase::AwaitingRetirement {
                            operation_id,
                            new_rev,
                            attempts,
                            until: Instant::now() + retry_delay(attempts),
                        };
                        drop(state);
                        self.emit(MapperEvent::AreaStatusChanged { area_id });
                        self.notify.notify_one();
                        return false;
                    }
                    removed = area.queue.pop_front();
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
                Self::park_expired_head(area);
            }
            removed
        };
        if removed.is_none() {
            return false;
        }
        self.emit(MapperEvent::MutationAcknowledged {
            area_id,
            operation_id,
        });
        self.complete_operation(operation_id, Ok(()));
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
        true
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
    pub(crate) fn transport_failure(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
        now: Instant,
    ) -> TransportVerdict {
        let mut failed: Option<(OperationId, String)> = None;
        {
            let mut state = self.state.lock();
            if let Some(area) = state.areas.get_mut(&area_id) {
                if let Some(head) = area.queue.front_mut() {
                    if head.operation_id != operation_id || area.phase != AreaPhase::InFlight {
                        return TransportVerdict::BackedOff;
                    }
                    head.attempts += 1;
                    if head.attempts >= MAX_TRANSPORT_ATTEMPTS {
                        let message = "could not reach the map service".to_string();
                        failed = Some((head.operation_id, message.clone()));
                        area.phase = AreaPhase::Failed {
                            operation_id: Some(operation_id),
                            message,
                            retryable: true,
                        };
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
            self.complete_operation(operation_id, Err(message.clone()));
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
            self.complete_operation(
                envelope.operation_id,
                Err("map edit requires conflict review".to_string()),
            );
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
    pub(crate) fn permanent_failure(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
        message: String,
    ) -> bool {
        let parked = {
            let mut state = self.state.lock();
            state.areas.get_mut(&area_id).and_then(|area| {
                let is_current = area.phase == AreaPhase::InFlight
                    && area
                        .queue
                        .front()
                        .is_some_and(|envelope| envelope.operation_id == operation_id);
                if !is_current {
                    return None;
                }
                area.phase = AreaPhase::Failed {
                    operation_id: Some(operation_id),
                    message: message.clone(),
                    retryable: true,
                };
                Some(operation_id)
            })
        };
        if let Some(operation_id) = parked {
            self.complete_operation(operation_id, Err(message.clone()));
            self.emit(MapperEvent::MutationFailed {
                area_id,
                operation_id,
                message,
            });
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        parked.is_some()
    }

    #[must_use]
    pub(crate) fn is_in_flight_at_generation(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
        viewer_id: Option<Uuid>,
        auth_generation: u64,
    ) -> bool {
        let state = self.state.lock();
        state.areas.get(&area_id).is_some_and(|area| {
            area.phase == AreaPhase::InFlight
                && area.queue.front().is_some_and(|envelope| {
                    envelope.operation_id == operation_id
                        && envelope.viewer_id == viewer_id
                        && envelope.auth_generation == auth_generation
                })
                && (viewer_id.is_none()
                    || state.active_viewer == viewer_id.map(|viewer| (viewer, auth_generation)))
        })
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
    pub fn resolve_conflict(
        &self,
        area_id: AreaId,
        keep_mine: bool,
    ) -> CloudResult<ConflictResolution> {
        const NOOP: ConflictResolution = ConflictResolution {
            resolved: false,
            discarded: None,
        };
        let discarded = {
            let mut state = self.state.lock();
            let Some(area) = state.areas.get_mut(&area_id) else {
                return Ok(NOOP);
            };
            let AreaPhase::Conflict { operation_id } = &area.phase else {
                return Ok(NOOP);
            };
            let operation_id = *operation_id;
            if keep_mine {
                None
            } else {
                if let Some(envelope) = area
                    .queue
                    .iter()
                    .find(|envelope| envelope.operation_id == operation_id)
                {
                    self.retire_journal(envelope)?;
                }
                area.phase = AreaPhase::Ready;
                area.queue
                    .iter()
                    .position(|e| e.operation_id == operation_id)
                    .and_then(|position| area.queue.remove(position))
            }
        };
        if let Some(envelope) = &discarded {
            self.complete_operation(
                envelope.operation_id,
                Err("map edit was discarded during conflict review".to_string()),
            );
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        if !keep_mine {
            self.notify.notify_one();
        }
        Ok(ConflictResolution {
            resolved: true,
            discarded,
        })
    }

    /// Retry a permanently-failed head, or discard it. The returned
    /// resolution is the caller's sole accounting signal: `unparked` comes
    /// from the same lock that performed the transition, so no status
    /// re-read can race a concurrent park.
    #[must_use]
    pub fn resolve_failure(&self, area_id: AreaId, retry: bool) -> CloudResult<FailureResolution> {
        const NOOP: FailureResolution = FailureResolution {
            unparked: false,
            discarded: None,
        };
        let discarded = {
            let mut state = self.state.lock();
            let Some(area) = state.areas.get_mut(&area_id) else {
                return Ok(NOOP);
            };
            let AreaPhase::Failed {
                operation_id,
                retryable,
                ..
            } = area.phase
            else {
                return Ok(NOOP);
            };
            if retry && !retryable {
                return Err(CloudError::PendingOperations(
                    "this saved edit is outside the server replay window; discard or export it instead"
                        .to_string(),
                ));
            }
            let failed_operation =
                operation_id.or_else(|| area.queue.front().map(|envelope| envelope.operation_id));
            if retry {
                area.recovery_base_failed = false;
                area.phase = AreaPhase::Ready;
                if let Some(operation_id) = failed_operation
                    && let Some(envelope) = area
                        .queue
                        .iter_mut()
                        .find(|envelope| envelope.operation_id == operation_id)
                {
                    envelope.attempts = 0;
                }
                None
            } else {
                let discarded = failed_operation.and_then(|operation_id| {
                    area.queue
                        .iter()
                        .position(|envelope| envelope.operation_id == operation_id)
                        .and_then(|position| area.queue.get(position))
                        .cloned()
                });
                if let Some(envelope) = &discarded {
                    self.retire_journal(envelope)?;
                }
                let removed = failed_operation.and_then(|operation_id| {
                    area.queue
                        .iter()
                        .position(|envelope| envelope.operation_id == operation_id)
                        .and_then(|position| area.queue.remove(position))
                });
                // Retirement is the discard commit point. Keep the area
                // failed until it succeeds so an I/O error cannot wake and
                // resend an edit the user chose to discard.
                area.phase = AreaPhase::Ready;
                Self::park_expired_head(area);
                removed
            }
        };
        if let Some(envelope) = &discarded {
            self.complete_operation(
                envelope.operation_id,
                Err("failed map edit was discarded".to_string()),
            );
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
        Ok(FailureResolution {
            unparked: true,
            discarded,
        })
    }

    /// Fences an area for an acknowledged delete. The caller serializes this
    /// with mutation compilation; the queue-level marker also guards against
    /// direct or late enqueue attempts and prevents followers from sending.
    pub(crate) fn begin_delete(&self, area_id: AreaId) -> CloudResult<()> {
        let mut state = self.state.lock();
        if state.delete_intents.contains(&area_id) {
            return Err(CloudError::PendingOperations(
                "this area's interrupted delete is still being reconciled".to_string(),
            ));
        }
        if !state.deleting.insert(area_id) {
            return Err(CloudError::PendingOperations(
                "this area is already being deleted".to_string(),
            ));
        }
        drop(state);
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
        Ok(())
    }

    /// Durably records the delete decision before the backend request begins.
    /// A reset after this point freezes the WAL until an authoritative fetch
    /// determines whether the backend deletion committed.
    pub(crate) fn prepare_delete(&self, area_id: AreaId) -> CloudResult<()> {
        let snapshot = {
            let state = self.state.lock();
            if !state.deleting.contains(&area_id) {
                return Err(CloudError::PendingOperations(
                    "this area is not fenced for deletion".to_string(),
                ));
            }
            state
                .areas
                .get(&area_id)
                .map(|area| area.queue.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        self.write_delete_intents(area_id, &snapshot)?;
        let mut state = self.state.lock();
        if !state.deleting.contains(&area_id) {
            return Err(CloudError::PendingOperations(
                "this area's delete fence changed while it was prepared".to_string(),
            ));
        }
        state.delete_intents.insert(area_id);
        if let Some(area) = state.areas.get_mut(&area_id) {
            for envelope in &mut area.queue {
                envelope.delete_intent = true;
            }
        }
        Ok(())
    }

    /// Waits for the one request that may already be on the wire. Followers
    /// are held by the delete fence; parked, conflicted, and backed-off work
    /// is already quiescent and need not be retried before deletion.
    pub(crate) async fn wait_until_delete_quiescent(&self, area_id: AreaId) {
        loop {
            let in_flight = self
                .state
                .lock()
                .areas
                .get(&area_id)
                .is_some_and(|area| area.phase == AreaPhase::InFlight);
            if !in_flight {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Commits a successful area delete by retiring every unsent journal
    /// record and terminating exact-operation waiters.
    pub(crate) fn commit_delete(&self, area_id: AreaId) -> CloudResult<Vec<PendingEnvelope>> {
        let snapshot = self
            .state
            .lock()
            .areas
            .get(&area_id)
            .map(|area| area.queue.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        // Renaming the pre-request intent to a tombstone is the deletion's
        // local durability commit point. It suppresses every surviving active
        // record even if later cleanup fails or the process resets.
        self.commit_delete_intents(area_id, &snapshot)?;
        let removed = {
            let mut state = self.state.lock();
            state.deleting.remove(&area_id);
            state.delete_intents.remove(&area_id);
            state
                .areas
                .remove(&area_id)
                .map(|area| area.queue.into_iter().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        for envelope in &removed {
            if let Err(error) = self.retire_journal(envelope) {
                warn!(
                    "failed to retire mapper journal record {} after deleting area {area_id}: {error}",
                    envelope.operation_id
                );
            }
            self.complete_operation(
                envelope.operation_id,
                Err("map edit was discarded because the area was deleted".to_string()),
            );
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
        Ok(removed)
    }

    /// Reopens a delete fence when the request fails or its future is
    /// cancelled. The original durable queue remains intact.
    pub(crate) fn abort_delete(&self, area_id: AreaId) -> CloudResult<()> {
        let snapshot = self
            .state
            .lock()
            .areas
            .get(&area_id)
            .map(|area| area.queue.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        self.abort_delete_intents(area_id, &snapshot)?;
        let changed = {
            let mut state = self.state.lock();
            // Both flags must clear (no short-circuit): an abort after
            // `prepare_delete` holds the fence AND the durable intent, and a
            // surviving intent would keep the area fenced forever.
            let removed_fence = state.deleting.remove(&area_id);
            let removed_intent = state.delete_intents.remove(&area_id);
            if let Some(area) = state.areas.get_mut(&area_id) {
                for envelope in &mut area.queue {
                    envelope.delete_intent = false;
                }
            }
            removed_fence || removed_intent
        };
        if changed {
            self.emit(MapperEvent::AreaStatusChanged { area_id });
            self.notify.notify_one();
        }
        Ok(())
    }

    /// A DELETE request was issued but its response did not prove whether the
    /// backend committed. Keep the durable intent, release the in-session
    /// request fence, and force an authoritative recovery fetch before WAL
    /// replay.
    pub(crate) fn mark_delete_ambiguous(&self, area_id: AreaId) {
        {
            let mut state = self.state.lock();
            state.deleting.remove(&area_id);
            state.delete_intents.insert(area_id);
            if let Some(area) = state.areas.get_mut(&area_id) {
                area.requires_recovery_base = true;
                for envelope in &mut area.queue {
                    envelope.delete_intent = true;
                }
            }
        }
        self.emit(MapperEvent::AreaStatusChanged { area_id });
        self.notify.notify_one();
    }

    /// Resolves a recovered pre-delete intent when the area still exists.
    /// The intent is removed durably before the WAL is allowed to replay.
    pub(crate) fn abort_recovered_delete(&self, area_id: AreaId) -> CloudResult<()> {
        if !self.has_delete_intent(area_id) {
            return Ok(());
        }
        self.abort_delete(area_id)
    }

    /// Resolves a recovered pre-delete intent when the area is authoritatively
    /// absent. The intent becomes a tombstone before its queued edits are
    /// discarded, so another reset cannot resurrect them.
    pub(crate) fn commit_recovered_delete(
        &self,
        area_id: AreaId,
    ) -> CloudResult<Vec<PendingEnvelope>> {
        if !self.has_delete_intent(area_id) {
            return Ok(Vec::new());
        }
        self.commit_delete(area_id)
    }

    /// Cancels a queued-but-unsent envelope (local undo of unacknowledged
    /// work). The head can cancel only while its queue is idle (`Ready`):
    /// an in-flight head is on the wire, and a parked head (backoff,
    /// conflict, failure) belongs to its resolution flow — cancelling it
    /// there would strand the park's terminal accounting and its phase.
    pub fn cancel(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
    ) -> CloudResult<Option<PendingEnvelope>> {
        let removed = {
            let mut state = self.state.lock();
            let Some(area) = state.areas.get_mut(&area_id) else {
                return Ok(None);
            };
            let Some(position) = area
                .queue
                .iter()
                .position(|e| e.operation_id == operation_id)
            else {
                return Ok(None);
            };
            if position == 0 && area.phase != AreaPhase::Ready {
                return Ok(None);
            }
            self.retire_journal(&area.queue[position])?;
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
            self.complete_operation(
                operation_id,
                Err("map edit was canceled before acknowledgement".to_string()),
            );
            self.notify.notify_one();
        }
        Ok(removed)
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

    #[must_use]
    pub(crate) fn area_identity(&self, area_id: AreaId) -> Option<(Option<Uuid>, u64)> {
        self.state
            .lock()
            .areas
            .get(&area_id)
            .and_then(|area| area.queue.front())
            .map(|envelope| (envelope.viewer_id, envelope.auth_generation))
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

    /// The operation currently paused after a permanent delivery failure.
    #[must_use]
    pub fn failed_operation_id(&self, area_id: AreaId) -> Option<OperationId> {
        let state = self.state.lock();
        let area = state.areas.get(&area_id)?;
        match area.phase {
            AreaPhase::Failed { operation_id, .. } => {
                operation_id.or_else(|| area.queue.front().map(|envelope| envelope.operation_id))
            }
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
            AreaPhase::Failed {
                message, retryable, ..
            } => AreaSaveStatus::CouldNotSave {
                message: message.clone(),
                retryable: *retryable,
            },
            AreaPhase::Backoff { .. } => AreaSaveStatus::Offline(pending),
            AreaPhase::Ready | AreaPhase::InFlight | AreaPhase::AwaitingRetirement { .. } => {
                AreaSaveStatus::Saving(pending)
            }
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
            viewer_id: None,
            local_durable: false,
            auth_generation: 0,
            sequence: 0,
            queued_at: Utc::now(),
            journal_path: None,
            receipt_expired: false,
            published: true,
            journal_batch_id: None,
            delete_intent: false,
        }
    }

    #[test]
    fn queues_serialize_per_area_and_interleave_across_areas() {
        let q = PendingQueue::new();
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());
        q.enqueue(area_a, envelope("a1")).expect("enqueue");
        q.enqueue(area_a, envelope("a2")).expect("enqueue");
        q.enqueue(area_b, envelope("b1")).expect("enqueue");
        q.enqueue(area_b, envelope("b2")).expect("enqueue");

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
        let operation = envelope("op");
        let operation_id = operation.operation_id;
        q.enqueue(area, operation).expect("enqueue");
        let now = Instant::now();
        for _ in 0..MAX_TRANSPORT_ATTEMPTS {
            let (taken, _) = q.take_ready(now + Duration::from_hours(1));
            assert!(taken.is_some(), "retry becomes ready after backoff");
            q.transport_failure(area, operation_id, now);
        }
        assert!(matches!(
            q.save_status(area),
            AreaSaveStatus::CouldNotSave { .. }
        ));
        // Nothing was dropped.
        assert_eq!(q.pending_for(area).len(), 1);
        // Retry re-arms the attempts budget and reports the un-park.
        let resolution = q.resolve_failure(area, true).expect("resolve");
        assert!(resolution.unparked);
        assert!(resolution.discarded.is_none());
        assert!(matches!(q.save_status(area), AreaSaveStatus::Saving(1)));
        // Resolving an area that is not parked is a no-op.
        assert!(
            !q.resolve_failure(area, true)
                .expect("no-op resolve")
                .unparked
        );
    }

    #[test]
    fn transport_failure_reports_its_park_verdict_exactly_once() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let operation = envelope("op");
        let operation_id = operation.operation_id;
        q.enqueue(area, operation).expect("enqueue");
        let now = Instant::now();
        for attempt in 1..=MAX_TRANSPORT_ATTEMPTS {
            let _ = q.take_ready(now + Duration::from_hours(1));
            let verdict = q.transport_failure(area, operation_id, now);
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
        q.enqueue(area, envelope("mine")).expect("enqueue");
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
        let resolution = q.resolve_conflict(area, true).expect("keep mine");
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
        let resolution = q.resolve_conflict(area, false).expect("keep theirs");
        assert!(resolution.resolved);
        let discarded = resolution.discarded.expect("discarded");
        assert_eq!(discarded.operation_id, env.operation_id);
        assert_eq!(q.save_status(area), AreaSaveStatus::Saved);
        assert!(!q.contains_operation(area, env.operation_id));
        assert_eq!(q.conflicted_operation_id(area), None);
        // Resolving an area that is not conflict-paused is a no-op.
        assert!(
            !q.resolve_conflict(area, false)
                .expect("no-op resolve")
                .resolved
        );
    }

    #[test]
    fn conflict_pause_targets_the_failing_envelope_not_the_head() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let first = envelope("sane head");
        let second = envelope("failing follower");
        let first_id = first.operation_id;
        let second_id = second.operation_id;
        q.enqueue(area, first).expect("enqueue");
        q.enqueue(area, second).expect("enqueue");
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
        let resolution = q.resolve_conflict(area, false).expect("keep theirs");
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
        q.enqueue(area, first).expect("enqueue");
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
        q.enqueue(area, first).expect("enqueue");
        q.enqueue(area, second).expect("enqueue");
        let _ = q.take_ready(Instant::now());
        // Head is in flight: cannot cancel.
        assert!(q.cancel(area, first_id).expect("cancel check").is_none());
        // Queued follower cancels fine.
        assert!(
            q.cancel(area, second_id)
                .expect("cancel follower")
                .is_some()
        );
        assert_eq!(q.pending_for(area).len(), 1);
    }

    #[tokio::test]
    async fn delete_fence_waits_for_head_and_discards_followers() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let first = envelope("in flight");
        let second = envelope("follower");
        let first_id = first.operation_id;
        let second_id = second.operation_id;
        q.enqueue(area, first).expect("enqueue head");
        q.enqueue(area, second).expect("enqueue follower");
        let _ = q.take_ready(Instant::now()).0.expect("take head");

        q.begin_delete(area).expect("fence");
        assert!(
            q.enqueue(area, envelope("late")).is_err(),
            "new edits must be rejected while delete is pending"
        );
        q.acknowledge(area, first_id, Some(2));
        q.wait_until_delete_quiescent(area).await;
        assert!(
            q.take_ready(Instant::now()).0.is_none(),
            "the follower must not start after the in-flight head settles"
        );

        q.prepare_delete(area).expect("prepare delete");
        let discarded = q.commit_delete(area).expect("delete commit");
        assert_eq!(discarded.len(), 1);
        assert_eq!(discarded[0].operation_id, second_id);
        assert!(q.pending_for(area).is_empty());
        assert!(q.wait_for_completion(second_id).await.is_err());
    }

    #[test]
    fn cancel_refuses_a_parked_head_but_allows_an_idle_one() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let head = envelope("parked");
        let head_id = head.operation_id;
        q.enqueue(area, head).expect("enqueue");
        let now = Instant::now();
        for _ in 0..MAX_TRANSPORT_ATTEMPTS {
            let _ = q.take_ready(now + Duration::from_hours(1));
            let _ = q.transport_failure(area, head_id, now);
        }
        assert!(matches!(
            q.save_status(area),
            AreaSaveStatus::CouldNotSave { .. }
        ));
        // A parked head belongs to its resolution flow, whose park already
        // counted terminally — cancelling it would double-count.
        assert!(q.cancel(area, head_id).expect("cancel check").is_none());
        assert_eq!(q.pending_for(area).len(), 1);
        // Un-parking returns the queue to Ready, where the head may cancel.
        assert!(q.resolve_failure(area, true).expect("unpark").unparked);
        assert!(q.cancel(area, head_id).expect("cancel idle").is_some());
        assert_eq!(q.save_status(area), AreaSaveStatus::Saved);
    }

    #[test]
    fn confirmed_rev_never_regresses_when_access_changes() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let fp = || Some("fp-a".to_string());

        // The load lands, then an acknowledgement advances past it.
        q.note_confirmed_rev(area, 1, fp());
        q.enqueue(area, envelope("op")).expect("enqueue");
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
        q.enqueue(area, envelope("op2")).expect("enqueue");
        let (taken, _) = q.take_ready(Instant::now());
        let (_, env2, _, _) = taken.expect("head");
        q.acknowledge(area, env2.operation_id, Some(1));
        assert_eq!(q.confirmed_rev(area).0, Some(2));

        // A changed fingerprint can change redacted content, but all viewers
        // share the same revision counter, so it cannot regress the base.
        q.note_confirmed_rev(area, 1, Some("fp-b".to_string()));
        assert_eq!(q.confirmed_rev(area).0, Some(2));
    }

    #[test]
    fn upgrade_pause_holds_everything_without_loss() {
        let q = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        q.enqueue(area, envelope("op")).expect("enqueue");
        q.pause_for_upgrade();
        let (taken, _) = q.take_ready(Instant::now());
        assert!(taken.is_none());
        assert_eq!(q.pending_for(area).len(), 1);
        q.resume_after_upgrade();
        let (taken, _) = q.take_ready(Instant::now());
        assert!(taken.is_some());
    }

    fn journal_test_root() -> PathBuf {
        std::env::temp_dir().join(format!("smudgy-pending-journal-test-{}", Uuid::new_v4()))
    }

    fn durable_envelope(desc: &str, viewer_id: Uuid, auth_generation: u64) -> PendingEnvelope {
        PendingEnvelope {
            viewer_id: Some(viewer_id),
            auth_generation,
            ..envelope(desc)
        }
    }

    fn local_durable_envelope(desc: &str) -> PendingEnvelope {
        PendingEnvelope {
            local_durable: true,
            ..envelope(desc)
        }
    }

    #[test]
    fn multi_stage_journal_failure_publishes_no_prefix() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let queue = PendingQueue::with_journal(root.clone());
        queue.activate_viewer(Some(viewer), 1);
        let cloud_directory = queue.viewer_directory(viewer);
        fs::create_dir_all(cloud_directory.parent().expect("viewer parent"))
            .expect("create viewer parent");
        fs::write(&cloud_directory, b"blocks directory creation").expect("create blocker");

        let local_area = AreaId(Uuid::new_v4());
        let cloud_area = AreaId(Uuid::new_v4());
        let result = queue.enqueue_many_staged(vec![
            (local_area, local_durable_envelope("written first")),
            (cloud_area, durable_envelope("fails second", viewer, 1)),
        ]);

        assert!(result.is_err());
        assert_eq!(queue.total_pending(), 0);
        let active_local_records = fs::read_dir(queue.local_directory())
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| {
                        entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
                    })
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(
            active_local_records, 0,
            "the successfully written prefix must be retired"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn staged_batch_is_worker_invisible_until_atomic_publication() {
        let queue = PendingQueue::new();
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());
        let publication = queue
            .enqueue_many_staged(vec![(area_a, envelope("a")), (area_b, envelope("b"))])
            .expect("stage");

        assert!(
            queue.take_ready(Instant::now()).0.is_none(),
            "the worker must not observe a staged member"
        );
        queue.publish_staged(publication);
        assert!(queue.take_ready(Instant::now()).0.is_some());
        assert!(queue.take_ready(Instant::now()).0.is_some());
    }

    #[test]
    fn batch_without_commit_marker_recovers_no_prefix() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        {
            let queue = PendingQueue::with_journal(root.clone());
            let mut pending = durable_envelope("uncommitted prefix", viewer, 1);
            pending.sequence = 1;
            pending.queued_at = Utc::now();
            queue
                .write_journal_record(area, &pending, Uuid::new_v4())
                .expect("write prepared member")
                .expect("durable member");
            // Simulate power loss before write_commit_marker.
        }

        let recovered = PendingQueue::with_journal(root.clone());
        recovered.activate_viewer(Some(viewer), 2);
        assert_eq!(
            recovered.total_pending(),
            0,
            "a prepared batch prefix is never replayable"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn committed_unpublished_batch_recovers_every_member_after_reset() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());
        {
            let queue = PendingQueue::with_journal(root.clone());
            queue.activate_viewer(Some(viewer), 1);
            let _publication = queue
                .enqueue_many_staged(vec![
                    (area_a, durable_envelope("a", viewer, 1)),
                    (area_b, durable_envelope("b", viewer, 1)),
                ])
                .expect("durable batch commit");
            assert!(queue.take_ready(Instant::now()).0.is_none());
            // Simulate reset after the WAL commit but before optimistic publish.
        }

        let recovered = PendingQueue::with_journal(root.clone());
        let activation = recovered.activate_viewer(Some(viewer), 2);
        assert_eq!(activation.added.values().sum::<u64>(), 2);
        assert_eq!(recovered.total_pending(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_queue_recovers_only_for_the_proven_viewer_and_rebinds_generation() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let other_viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let operation_id;
        {
            let queue = PendingQueue::with_journal(root.clone());
            queue.activate_viewer(Some(viewer), 4);
            let envelope = durable_envelope("durable edit", viewer, 4);
            operation_id = envelope.operation_id;
            queue
                .enqueue(area, envelope)
                .expect("write-ahead journal record");
            assert_eq!(queue.pending_for(area).len(), 1);
        }

        let queue = PendingQueue::with_journal(root.clone());
        queue.activate_viewer(Some(other_viewer), 6);
        assert!(
            queue.pending_for(area).is_empty(),
            "another account cannot load this viewer's records"
        );

        let activation = queue.activate_viewer(Some(viewer), 8);
        assert_eq!(activation.added.get(&area), Some(&1));
        let recovered = queue.pending_for(area);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].operation_id, operation_id);
        assert_eq!(recovered[0].viewer_id, Some(viewer));
        assert_eq!(
            recovered[0].auth_generation, 8,
            "identity proof rebinds the restored request to the current credential"
        );

        assert!(
            queue.take_ready(Instant::now()).0.is_none(),
            "restored work waits for structural replay over a confirmed base"
        );
        queue.recovery_base_loaded(area);
        let (ready, _) = queue.take_ready(Instant::now());
        let (_, ready, _, _) = ready.expect("matching viewer work is sendable");
        assert!(queue.acknowledge(area, ready.operation_id, Some(2)));
        assert!(
            queue
                .viewer_directory(viewer)
                .read_dir()
                .expect("viewer journal")
                .all(|entry| entry
                    .map(
                        |entry| entry.path().extension().and_then(|ext| ext.to_str())
                            != Some("json")
                    )
                    .unwrap_or(false)),
            "an acknowledged idempotency receipt retires the durable record"
        );
        let retired = root
            .join("servers")
            .join(queue.namespace_key())
            .join("retired");
        assert!(
            !retired.exists()
                || retired
                    .read_dir()
                    .expect("retired directory")
                    .all(|entry| entry.map(|entry| !entry.path().is_file()).unwrap_or(false)),
            "retired journal bodies must not retain acknowledged map content"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn viewer_switch_resolves_waiters_and_reactivation_starts_a_fresh_epoch() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let other_viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let queue = PendingQueue::with_journal(root.clone());
        queue.activate_viewer(Some(viewer), 1);
        let pending = durable_envelope("switch-safe", viewer, 1);
        let operation_id = pending.operation_id;
        queue.enqueue(area, pending).expect("enqueue");

        let activation = queue.activate_viewer(Some(other_viewer), 2);
        assert_eq!(activation.removed_operations, vec![operation_id]);
        let switched = queue
            .wait_for_completion(operation_id)
            .await
            .expect_err("old-account waiter must terminate");
        assert!(switched.contains("original account"));

        queue.activate_viewer(Some(viewer), 3);
        queue.recovery_base_loaded(area);
        let (ready, _) = queue.take_ready(Instant::now());
        assert_eq!(
            ready.expect("reactivated edit").1.operation_id,
            operation_id
        );
        assert!(queue.acknowledge(area, operation_id, Some(2)));
        queue
            .wait_for_completion(operation_id)
            .await
            .expect("reactivation replaces the old terminal completion");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn expired_restored_record_is_non_retryable_and_completes_its_waiter() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let operation_id;
        {
            let queue = PendingQueue::with_journal(root.clone());
            queue.activate_viewer(Some(viewer), 1);
            let pending = durable_envelope("too old", viewer, 1);
            operation_id = pending.operation_id;
            queue.enqueue(area, pending).expect("enqueue");
            let path = queue.pending_for(area)[0]
                .journal_path
                .clone()
                .expect("journal path");
            let mut record: DurablePendingRecord =
                serde_json::from_slice(&fs::read(&path).expect("read journal")).expect("record");
            record.body.queued_at = Utc::now() - chrono::Duration::days(RECEIPT_RETENTION_DAYS + 1);
            record.checksum = PendingQueue::checksum(&record.body).expect("checksum");
            fs::write(&path, serde_json::to_vec(&record).expect("serialize")).expect("age record");
            let batch_id = record.body.batch_id.expect("schema-v3 batch id");
            let commit_path = queue.commit_directory().join(format!("{batch_id}.commit"));
            let mut commit: DurableCommitRecord =
                serde_json::from_slice(&fs::read(&commit_path).expect("read commit marker"))
                    .expect("commit marker");
            let member = commit
                .body
                .members
                .iter_mut()
                .find(|member| member.operation_id == operation_id)
                .expect("committed operation");
            member.record_checksum.clone_from(&record.checksum);
            commit.checksum = PendingQueue::checksum(&commit.body).expect("commit checksum");
            fs::write(
                commit_path,
                serde_json::to_vec(&commit).expect("serialize commit marker"),
            )
            .expect("age commit marker");
        }

        let queue = PendingQueue::with_journal(root.clone());
        let activation = queue.activate_viewer(Some(viewer), 2);
        assert_eq!(activation.expired_operations, vec![(area, operation_id)]);
        assert!(matches!(
            queue.save_status(area),
            AreaSaveStatus::CouldNotSave {
                retryable: false,
                ..
            }
        ));
        let message = queue
            .wait_for_completion(operation_id)
            .await
            .expect_err("expired restored waiter is terminal");
        assert!(message.contains("replay window"));
        assert!(
            queue.resolve_failure(area, true).is_err(),
            "retry must stay disabled"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn unavailable_recovery_base_surfaces_and_can_reopen() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let operation_id;
        {
            let queue = PendingQueue::with_journal(root.clone());
            queue.activate_viewer(Some(viewer), 1);
            let pending = durable_envelope("needs base", viewer, 1);
            operation_id = pending.operation_id;
            queue.enqueue(area, pending).expect("enqueue");
        }

        let queue = PendingQueue::with_journal(root.clone());
        queue.activate_viewer(Some(viewer), 2);
        assert!(queue.recovery_base_unavailable(
            area,
            "saved edits cannot be restored while the area is unavailable".to_string()
        ));
        assert!(matches!(
            queue.save_status(area),
            AreaSaveStatus::CouldNotSave {
                retryable: true,
                ..
            }
        ));
        assert!(
            queue
                .wait_for_completion(operation_id)
                .await
                .expect_err("recovery waiter")
                .contains("unavailable")
        );

        assert!(queue.recovery_base_loaded(area));
        let (ready, _) = queue.take_ready(Instant::now());
        assert_eq!(
            ready.expect("reopened recovery").1.operation_id,
            operation_id
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn startup_removes_crash_leftovers_from_retired_namespace() {
        let root = journal_test_root();
        let namespace = "test-backend".to_string();
        let namespace_key = {
            let digest = Sha256::digest(namespace.as_bytes());
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let retired = root
            .join("servers")
            .join(namespace_key)
            .join("retired")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&retired).expect("retired fixture directory");
        fs::write(retired.join("body.retired"), b"sensitive map body").expect("retired fixture");

        let _queue = PendingQueue::with_journal(root.clone());
        assert!(!retired.exists(), "startup removes retired crash leftovers");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn credential_change_dormants_durable_work_until_identity_reactivation() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let queue = PendingQueue::with_journal(root.clone());
        queue.activate_viewer(Some(viewer), 2);
        queue
            .enqueue(area, durable_envelope("edit", viewer, 2))
            .expect("enqueue");

        let operation_id = queue.pending_for(area)[0].operation_id;
        let _ = queue.take_ready(Instant::now());
        queue.credential_changed(area, operation_id, 2);
        assert!(queue.take_ready(Instant::now()).0.is_none());
        queue.activate_viewer(Some(viewer), 4);
        assert!(
            queue.take_ready(Instant::now()).0.is_none(),
            "reactivated disk work still requires a confirmed base"
        );
        queue.recovery_base_loaded(area);
        assert!(
            queue.take_ready(Instant::now()).0.is_some(),
            "the same proven viewer resumes the retained record"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_durable_discard_stays_parked_until_retirement_succeeds() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let queue = PendingQueue::with_journal(root.clone());
        queue.activate_viewer(Some(viewer), 2);
        let envelope = durable_envelope("discard me", viewer, 2);
        let operation_id = envelope.operation_id;
        queue.enqueue(area, envelope).expect("enqueue");

        let now = Instant::now();
        for _ in 0..MAX_TRANSPORT_ATTEMPTS {
            let _ = queue.take_ready(now + Duration::from_hours(1));
            queue.transport_failure(area, operation_id, now);
        }
        assert!(matches!(
            queue.save_status(area),
            AreaSaveStatus::CouldNotSave { .. }
        ));

        let retired = root
            .join("servers")
            .join(queue.namespace_key())
            .join("retired");
        fs::write(&retired, b"blocks retired directory").expect("block retirement directory");
        assert!(
            queue.resolve_failure(area, false).is_err(),
            "durable retirement failure must be returned"
        );
        assert!(matches!(
            queue.save_status(area),
            AreaSaveStatus::CouldNotSave { .. }
        ));
        assert_eq!(queue.pending_for(area).len(), 1);
        assert!(
            queue
                .take_ready(Instant::now() + Duration::from_hours(2))
                .0
                .is_none(),
            "a failed discard must not re-arm the parked edit"
        );

        fs::remove_file(&retired).expect("unblock retirement directory");
        let resolution = queue
            .resolve_failure(area, false)
            .expect("discard succeeds after storage recovers");
        assert_eq!(
            resolution.discarded.map(|envelope| envelope.operation_id),
            Some(operation_id)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn journal_records_are_scoped_to_the_cloud_origin() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let operation_id;
        {
            let queue =
                PendingQueue::with_journal_namespace(root.clone(), "https://one.example".into());
            queue.activate_viewer(Some(viewer), 1);
            let envelope = durable_envelope("origin one", viewer, 1);
            operation_id = envelope.operation_id;
            queue.enqueue(area, envelope).expect("enqueue");
        }

        let other_origin =
            PendingQueue::with_journal_namespace(root.clone(), "https://two.example".into());
        other_origin.activate_viewer(Some(viewer), 2);
        assert!(
            other_origin.pending_for(area).is_empty(),
            "a viewer's edit must never cross cloud origins"
        );

        let original_origin =
            PendingQueue::with_journal_namespace(root.clone(), "https://one.example".into());
        original_origin.activate_viewer(Some(viewer), 2);
        assert_eq!(
            original_origin.pending_for(area)[0].operation_id,
            operation_id
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_cancel_cannot_reappear_after_restart() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let operation_id;
        {
            let queue = PendingQueue::with_journal(root.clone());
            queue.activate_viewer(Some(viewer), 1);
            let envelope = durable_envelope("cancel me", viewer, 1);
            operation_id = envelope.operation_id;
            queue.enqueue(area, envelope).expect("enqueue");
            assert!(
                queue
                    .cancel(area, operation_id)
                    .expect("durable retirement")
                    .is_some()
            );
        }

        let recovered = PendingQueue::with_journal(root.clone());
        recovered.activate_viewer(Some(viewer), 2);
        assert!(
            recovered.pending_for(area).is_empty(),
            "a durably canceled edit must not replay after restart"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_durable_queue_recovers_without_an_account_and_retires_on_ack() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        let operation_id;
        {
            let queue = PendingQueue::with_journal(root.clone());
            let pending = local_durable_envelope("local edit");
            operation_id = pending.operation_id;
            queue.enqueue(area, pending).expect("local journal enqueue");
            assert_eq!(
                queue.pending_for(area)[0]
                    .journal_path
                    .as_ref()
                    .and_then(|path| path.parent()),
                Some(queue.local_directory().as_path())
            );
        }

        let recovered = PendingQueue::with_journal(root.clone());
        assert_eq!(
            recovered.recovered_local_operations(),
            vec![(area, operation_id)]
        );
        assert!(
            recovered.take_ready(Instant::now()).0.is_none(),
            "local recovery also waits for a fresh file-backed base"
        );
        recovered.recovery_base_loaded(area);
        let (ready, _) = recovered.take_ready(Instant::now());
        assert_eq!(
            ready.expect("local edit ready").1.operation_id,
            operation_id
        );
        assert!(recovered.acknowledge(area, operation_id, Some(2)));

        let restarted = PendingQueue::with_journal(root.clone());
        assert!(
            restarted.recovered_local_operations().is_empty(),
            "acknowledged local work cannot replay after restart"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn acknowledgement_waits_only_for_durable_nonreplayable_transition() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        let queue = PendingQueue::with_journal(root.clone());
        let pending = local_durable_envelope("receipt-backed");
        let operation_id = pending.operation_id;
        queue.enqueue(area, pending).expect("enqueue");
        let _ = queue.take_ready(Instant::now()).0.expect("take");

        let acknowledged = queue.pending_for(area)[0]
            .journal_path
            .as_ref()
            .expect("journal path")
            .with_extension("ack");
        fs::create_dir(&acknowledged).expect("block acknowledgement rename");
        assert!(
            !queue.acknowledge(area, operation_id, Some(2)),
            "an ACK is not terminal until its WAL record leaves the replay namespace"
        );
        assert_eq!(queue.pending_for(area).len(), 1);
        assert!(
            queue.take_ready(Instant::now()).0.is_none(),
            "acknowledgement-transition failure cannot advance followers"
        );

        fs::remove_dir(&acknowledged).expect("unblock acknowledgement rename");
        assert!(
            queue
                .take_ready(Instant::now() + Duration::from_hours(1))
                .0
                .is_none(),
            "a backend-accepted mutation must never be dispatched again"
        );
        let (settled, _) = queue.retry_ready_retirement(Instant::now() + Duration::from_hours(1));
        assert_eq!(settled, Some((area, operation_id)));
        assert!(queue.pending_for(area).is_empty());
        assert!(
            PendingQueue::with_journal(root.clone())
                .recovered_local_operations()
                .is_empty()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_tombstone_suppresses_records_when_retirement_fails() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        {
            let queue = PendingQueue::with_journal(root.clone());
            queue
                .enqueue(area, local_durable_envelope("discarded by delete"))
                .expect("enqueue");
            queue.begin_delete(area).expect("delete fence");
            queue.prepare_delete(area).expect("durable delete intent");
            let retired = root.join("local").join("retired");
            fs::write(&retired, b"blocks retirement directory").expect("block retirement");
            assert_eq!(
                queue.commit_delete(area).expect("tombstone commits").len(),
                1
            );
        }

        let recovered = PendingQueue::with_journal(root.clone());
        assert!(
            recovered.recovered_local_operations().is_empty(),
            "the deletion tombstone suppresses the surviving active WAL"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovered_delete_intent_reopens_wal_when_area_still_exists() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        let operation_id;
        {
            let queue = PendingQueue::with_journal(root.clone());
            let pending = local_durable_envelope("survives an uncommitted delete");
            operation_id = pending.operation_id;
            queue.enqueue(area, pending).expect("enqueue");
            queue.begin_delete(area).expect("delete fence");
            queue.prepare_delete(area).expect("durable delete intent");
        }

        let recovered = PendingQueue::with_journal(root.clone());
        assert!(recovered.has_delete_intent(area));
        assert!(
            recovered.take_ready(Instant::now()).0.is_none(),
            "the WAL stays frozen while backend truth is unknown"
        );
        recovered
            .abort_recovered_delete(area)
            .expect("authoritative area presence aborts the intent");
        recovered.recovery_base_loaded(area);
        let ready = recovered
            .take_ready(Instant::now())
            .0
            .expect("the original edit reopens");
        assert_eq!(ready.1.operation_id, operation_id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ambiguous_live_delete_stays_frozen_until_area_presence_is_confirmed() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        let queue = PendingQueue::with_journal(root.clone());
        queue
            .enqueue(
                area,
                local_durable_envelope("kept across a lost delete response"),
            )
            .expect("enqueue");
        queue.begin_delete(area).expect("delete fence");
        queue.prepare_delete(area).expect("durable delete intent");
        queue.mark_delete_ambiguous(area);

        assert!(queue.has_delete_intent(area));
        assert!(queue.recovery_area_ids().contains(&area));
        assert!(
            queue.take_ready(Instant::now()).0.is_none(),
            "an ambiguous DELETE outcome cannot reopen the WAL"
        );

        queue
            .abort_recovered_delete(area)
            .expect("point GET proved the area still exists");
        queue.recovery_base_loaded(area);
        assert!(
            queue.take_ready(Instant::now()).0.is_some(),
            "only confirmed area presence reopens the WAL"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovered_delete_intent_discards_wal_when_area_is_absent() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        {
            let queue = PendingQueue::with_journal(root.clone());
            queue
                .enqueue(
                    area,
                    local_durable_envelope("discarded after committed delete"),
                )
                .expect("enqueue");
            queue.begin_delete(area).expect("delete fence");
            queue.prepare_delete(area).expect("durable delete intent");
        }

        let recovered = PendingQueue::with_journal(root.clone());
        assert_eq!(
            recovered
                .commit_recovered_delete(area)
                .expect("authoritative absence commits the delete")
                .len(),
            1
        );
        assert!(
            PendingQueue::with_journal(root.clone())
                .recovered_local_operations()
                .is_empty(),
            "a committed recovered delete cannot resurrect the WAL"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn batch_commit_marker_is_retired_after_its_last_member() {
        let root = journal_test_root();
        let queue = PendingQueue::with_journal(root.clone());
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());
        let publication = queue
            .enqueue_many_staged(vec![
                (area_a, local_durable_envelope("first")),
                (area_b, local_durable_envelope("second")),
            ])
            .expect("commit batch");
        queue.publish_staged(publication);
        let batch_id = queue.pending_for(area_a)[0]
            .journal_batch_id
            .expect("durable batch id");
        let marker = root.join("commits").join(format!("{batch_id}.commit"));
        assert!(marker.exists());

        let first = queue.take_ready(Instant::now()).0.expect("first member");
        let second = queue.take_ready(Instant::now()).0.expect("second member");
        assert!(queue.acknowledge(first.0, first.1.operation_id, Some(2)));
        assert!(
            marker.exists(),
            "the marker is needed while any member remains active"
        );
        assert!(queue.acknowledge(second.0, second.1.operation_id, Some(2)));
        assert!(
            !marker.exists(),
            "the marker is garbage-collected after its last active member"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn acknowledged_wal_never_replays_after_restart_before_cleanup() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        {
            let queue = PendingQueue::with_journal(root.clone());
            let pending = local_durable_envelope("accepted before reset");
            queue.enqueue(area, pending).expect("enqueue");
            let _ = queue.take_ready(Instant::now()).0.expect("take");
            let mut state = queue.state.lock();
            let envelope = state
                .areas
                .get_mut(&area)
                .and_then(|area| area.queue.front_mut())
                .expect("in-flight envelope");
            queue
                .mark_journal_acknowledged(envelope)
                .expect("durable acknowledgement transition");
            // Simulate reset at the exact boundary before detached cleanup.
        }

        let restarted = PendingQueue::with_journal(root.clone());
        assert!(
            restarted.recovered_local_operations().is_empty(),
            "an acknowledged body is cleanup-only after restart"
        );
        assert!(restarted.take_ready(Instant::now()).0.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn acknowledged_cloud_wal_never_replays_after_account_reactivation() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let other_viewer = Uuid::new_v4();
        let area = AreaId(Uuid::new_v4());
        let queue = PendingQueue::with_journal_namespace(root.clone(), "https://maps.test".into());
        queue.activate_viewer(Some(viewer), 1);
        let pending = durable_envelope("accepted before account switch", viewer, 1);
        queue.enqueue(area, pending).expect("enqueue");
        let _ = queue.take_ready(Instant::now()).0.expect("take");
        {
            let mut state = queue.state.lock();
            let envelope = state
                .areas
                .get_mut(&area)
                .and_then(|area| area.queue.front_mut())
                .expect("in-flight envelope");
            queue
                .mark_journal_acknowledged(envelope)
                .expect("durable acknowledgement transition");
        }

        queue.activate_viewer(Some(other_viewer), 2);
        queue.activate_viewer(Some(viewer), 3);
        assert!(
            queue.pending_for(area).is_empty(),
            "reactivation treats the .ack body as cleanup-only"
        );
        assert!(queue.take_ready(Instant::now()).0.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ephemeral_envelopes_remain_session_only() {
        let root = journal_test_root();
        let area = AreaId(Uuid::new_v4());
        {
            let queue = PendingQueue::with_journal(root.clone());
            let pending = envelope("ephemeral edit");
            queue.enqueue(area, pending).expect("in-session enqueue");
            assert!(queue.pending_for(area)[0].journal_path.is_none());
        }
        let restarted = PendingQueue::with_journal(root.clone());
        assert!(restarted.recovered_local_operations().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_expired_follower_parks_only_when_it_reaches_the_head() {
        let queue = PendingQueue::new();
        let area = AreaId(Uuid::new_v4());
        let first = envelope("first");
        let first_id = first.operation_id;
        let mut expired = envelope("expired follower");
        expired.receipt_expired = true;
        let expired_id = expired.operation_id;
        queue.enqueue(area, first).expect("first");
        queue.enqueue(area, expired).expect("expired follower");

        let (ready, _) = queue.take_ready(Instant::now());
        let (_, ready, _, _) = ready.expect("non-expired head should dispatch");
        assert_eq!(ready.operation_id, first_id);
        assert!(queue.acknowledge(area, first_id, Some(2)));
        let phase = queue
            .state
            .lock()
            .areas
            .get(&area)
            .expect("area queue")
            .phase
            .clone();
        assert!(matches!(
            phase,
            AreaPhase::Failed {
                operation_id: Some(operation_id),
                retryable: false,
                ..
            } if operation_id == expired_id
        ));
        assert!(
            queue.resolve_failure(area, true).is_err(),
            "expired receipts cannot be retried outside the server window"
        );
        assert_eq!(queue.pending_for(area)[0].operation_id, expired_id);
    }

    #[test]
    fn corrupt_journal_records_are_quarantined_instead_of_replayed() {
        let root = journal_test_root();
        let viewer = Uuid::new_v4();
        let queue = PendingQueue::with_journal(root.clone());
        let directory = queue.viewer_directory(viewer);
        fs::create_dir_all(&directory).expect("journal directory");
        fs::write(
            directory.join("00000000000000000001-broken.json"),
            b"{broken",
        )
        .expect("corrupt fixture");

        queue.activate_viewer(Some(viewer), 2);
        assert_eq!(queue.total_pending(), 0);
        assert_eq!(queue.recovery_errors().len(), 1);
        assert!(
            root.join("quarantine")
                .read_dir()
                .expect("quarantine directory")
                .next()
                .is_some()
        );
        let _ = fs::remove_dir_all(root);
    }
}
