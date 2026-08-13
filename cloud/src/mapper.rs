use crate::backends::{MapperBackend, area_edits};
use crate::error::CloudResult;
use crate::mapper::area_cache::AreaCache;
use crate::mapper::exit_cache::ExitCache;
use crate::mutation::{
    AreaMutation, MAX_MUTATION_OPERATIONS, MutationEnvelope, OperationId, Precondition,
    ResourceKind,
};
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CreateAreaRequest, Exit, ExitArgs, ExitId, ExitUpdates, LabelArgs, LabelId,
    LabelUpdates, MapDestination, MapStorage, RoomNumber, RoomUpdates, ShapeArgs, ShapeId,
    ShapeUpdates,
};

use arc_swap::ArcSwap;
use log::warn;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use uuid::Uuid;

pub mod area_cache;
pub mod atlas_cache;
pub mod exit_cache;
pub mod pending;
pub mod room_cache;
pub mod room_connection;
pub mod sync_engine;
pub use atlas_cache::{AtlasCache, ElsewhereMatch};
pub use pending::{AreaSaveStatus, MapperEvent};
pub use sync_engine::{SyncState, SyncStatus};

use pending::{PendingEnvelope, PendingQueue, StructuralPrecondition, TransportVerdict};

/// How a display rebuild folds a pending envelope that no longer applies
/// to the fresh confirmed projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayMode {
    /// Stop at the first failing envelope: it and everything after it stay
    /// pending but undisplayed. The conflict-detection fold — the caller
    /// pauses the queue targeting the reported envelope.
    StopAtFailure,
    /// Ignore client-only create/update preconditions while still applying
    /// the operations through the shared applier. Used only after an
    /// explicit Keep-mine decision.
    KeepMine,
}

/// Result of compiling a local mapper gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationSubmission {
    /// At least one effective operation was durably queued.
    Queued(OperationId),
    /// Every requested field already had the requested value.
    NoChange,
}

impl MutationSubmission {
    #[must_use]
    pub fn operation_id(self) -> Option<OperationId> {
        match self {
            Self::Queued(operation_id) => Some(operation_id),
            Self::NoChange => None,
        }
    }
}

/// One ordered envelope within a client-side all-or-nothing staging gesture.
/// Each envelope remains an independent server CAS operation; the atomicity
/// here guarantees that validation or local journal I/O cannot publish only
/// a prefix of the user's gesture.
#[derive(Debug, Clone)]
pub struct AreaMutationBatch {
    area_id: AreaId,
    operations: Vec<AreaMutation>,
    description: String,
    paired_policy: PairedExitPolicy,
}

impl AreaMutationBatch {
    #[must_use]
    pub fn strict(
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            area_id,
            operations,
            description: description.into(),
            paired_policy: PairedExitPolicy::Reject,
        }
    }

    /// Allows a real one-sided traversal topology edit to explicitly split
    /// its paired connection before applying the update.
    #[must_use]
    pub fn splitting_paired_exit(
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            area_id,
            operations,
            description: description.into(),
            paired_policy: PairedExitPolicy::Split,
        }
    }
}

/// Whether a compiler may split a paired Connection for a real exit topology
/// change. Generic updates reject; high-level structural gestures opt in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairedExitPolicy {
    Reject,
    Split,
}

/// Composite key for room lookups
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomKey {
    pub area_id: AreaId,
    pub room_number: RoomNumber,
}

impl RoomKey {
    #[must_use]
    pub fn new(area_id: AreaId, room_number: RoomNumber) -> Self {
        Self {
            area_id,
            room_number,
        }
    }
}

/// Canonical form of a room tag: trimmed and UPPERCASE. Tags are
/// case-insensitive, so this is applied at every write and lookup boundary
/// (client cache, sync ops, script ops) — mirroring the server's normalization.
#[must_use]
pub fn normalize_tag(tag: &str) -> String {
    tag.trim().to_uppercase()
}

fn normalize_exit_updates(current: &Exit, mut body: ExitUpdates) -> ExitUpdates {
    if body.from_direction == Some(current.from_direction) {
        body.from_direction = None;
    }
    if body.path.as_deref() == Some(current.path.as_str()) {
        body.path = None;
    }
    if body.is_hidden == Some(current.is_hidden) {
        body.is_hidden = None;
    }
    if body.is_closed == Some(current.is_closed) {
        body.is_closed = None;
    }
    if body.is_locked == Some(current.is_locked) {
        body.is_locked = None;
    }
    if body.weight == Some(current.weight) {
        body.weight = None;
    }
    if body.command.as_deref() == Some(current.command.as_str()) {
        body.command = None;
    }
    if body.is_secret == Some(current.is_secret) {
        body.is_secret = None;
    }

    if body.clear_to == Some(true) {
        // `clear_to` wins over every supplied destination field.
        body.to_area_id = None;
        body.to_room_number = None;
        body.to_direction = None;
        if current.to_area_id.is_none()
            && current.to_room_number.is_none()
            && current.to_direction.is_none()
            && !current.to_unknown
        {
            body.clear_to = None;
        }
    } else {
        body.clear_to = None;
        if body.to_area_id == current.to_area_id {
            body.to_area_id = None;
        }
        if body.to_room_number == current.to_room_number {
            body.to_room_number = None;
        }
        if body.to_direction == current.to_direction {
            body.to_direction = None;
        }
    }

    body
}

fn exit_updates_are_empty(body: &ExitUpdates) -> bool {
    body.from_direction.is_none()
        && body.to_area_id.is_none()
        && body.to_room_number.is_none()
        && body.to_direction.is_none()
        && body.path.is_none()
        && body.is_hidden.is_none()
        && body.is_closed.is_none()
        && body.is_locked.is_none()
        && body.weight.is_none()
        && body.command.is_none()
        && body.is_secret.is_none()
        && body.clear_to.is_none()
}

fn exit_topology_changed(current: &Exit, updated: &ExitCache) -> bool {
    current.from_direction != updated.from_direction
        || current.to_area_id != updated.to_area_id
        || current.to_room_number != updated.to_room_number
        || current.to_direction != updated.to_direction
        || current.to_unknown != updated.to_unknown
}

/// Compiles ordered mapper operations against a scratch document. Exit
/// updates become sparse, and pair splitting is either rejected or expanded
/// into an explicit `Unlink + UpdateExit` according to the high-level
/// gesture's policy. Applying each compiled operation to the scratch document
/// keeps later decisions faithful to earlier operations in the same envelope;
/// final graph validation remains the whole-envelope applier's job.
fn compile_area_mutations(
    details: &AreaWithDetails,
    operations: Vec<AreaMutation>,
    paired_policy: PairedExitPolicy,
) -> CloudResult<Vec<AreaMutation>> {
    let mut scratch = details.clone();
    let mut compiled = Vec::with_capacity(operations.len());

    for operation in operations {
        let expanded = match operation {
            AreaMutation::CreateExit {
                room_number,
                mut body,
            } => {
                let exit_id = body.id.unwrap_or_else(ExitId::new);
                body.id = Some(exit_id);

                if body.connection_id.is_none() && body.new_connection_id.is_none() {
                    // Resolve the legacy auto-pair/create choice once against
                    // the optimistic base, then persist that exact decision.
                    // The server must never independently mint the identity
                    // of a Connection the durable client already references.
                    let mut preview = scratch.clone();
                    area_edits::apply_mutation(
                        &mut preview,
                        &AreaMutation::CreateExit {
                            room_number,
                            body: body.clone(),
                        },
                    )?;
                    let resolved_connection_id = preview
                        .rooms
                        .iter()
                        .flat_map(|room| room.exits.iter())
                        .find(|exit| exit.id == exit_id)
                        .map(|exit| exit.connection_id)
                        .ok_or(CloudError::ExitNotFound(exit_id))?;
                    if scratch
                        .connections
                        .iter()
                        .any(|connection| connection.id == resolved_connection_id)
                    {
                        body.connection_id = Some(resolved_connection_id);
                    } else {
                        body.new_connection_id = Some(resolved_connection_id);
                    }
                }

                vec![AreaMutation::CreateExit { room_number, body }]
            }
            AreaMutation::UpdateExit { exit_id, body } => {
                let current = scratch
                    .rooms
                    .iter()
                    .flat_map(|room| room.exits.iter())
                    .find(|exit| exit.id == exit_id)
                    .cloned()
                    .ok_or(CloudError::ExitNotFound(exit_id))?;
                let body = normalize_exit_updates(&current, body);
                if exit_updates_are_empty(&body) {
                    Vec::new()
                } else {
                    let updated = body.clone().apply(&ExitCache::from(current.clone()));
                    let topology_changed = exit_topology_changed(&current, &updated);
                    let member_count = scratch
                        .rooms
                        .iter()
                        .flat_map(|room| room.exits.iter())
                        .filter(|exit| exit.connection_id == current.connection_id)
                        .count();
                    if topology_changed && member_count == 2 {
                        match paired_policy {
                            PairedExitPolicy::Reject => {
                                return Err(CloudError::StructuralConflict(
                                    "unlink_before_edit".to_string(),
                                ));
                            }
                            PairedExitPolicy::Split => vec![
                                AreaMutation::Unlink {
                                    exit_id,
                                    new_connection_id: crate::ConnectionId::new(),
                                },
                                AreaMutation::UpdateExit { exit_id, body },
                            ],
                        }
                    } else {
                        vec![AreaMutation::UpdateExit { exit_id, body }]
                    }
                }
            }
            other => vec![other],
        };

        for operation in expanded {
            area_edits::apply_mutation(&mut scratch, &operation)?;
            compiled.push(operation);
        }
    }

    Ok(compiled)
}

fn same_exit_destination(left: &Exit, right: &Exit) -> bool {
    left.from_direction == right.from_direction
        && left.to_area_id == right.to_area_id
        && left.to_room_number == right.to_room_number
        && left.to_direction == right.to_direction
}

/// Builds the one-envelope same-area room merge used by script packages.
/// The kept room's metadata wins; the removed room's properties, tags, and
/// external id are deliberately discarded. Traversal exits are rewired or
/// copied with their path, flags, command, weight, and visible secrecy.
fn merge_room_operations(
    details: &AreaWithDetails,
    keep_room_number: RoomNumber,
    remove_room_number: RoomNumber,
) -> CloudResult<Vec<AreaMutation>> {
    if keep_room_number == remove_room_number {
        return Err(CloudError::InvalidInput(
            "a room cannot be merged into itself".to_string(),
        ));
    }
    let area_id = details.area.id;
    let keep = details
        .rooms
        .iter()
        .find(|room| room.room_number == keep_room_number)
        .ok_or_else(|| CloudError::RoomNotFound(RoomKey::new(area_id, keep_room_number)))?;
    let remove = details
        .rooms
        .iter()
        .find(|room| room.room_number == remove_room_number)
        .ok_or_else(|| CloudError::RoomNotFound(RoomKey::new(area_id, remove_room_number)))?;
    if remove
        .exits
        .iter()
        .any(|exit| exit.to_area_id.is_some_and(|target| target != area_id))
    {
        return Err(CloudError::StructuralConflict(
            "merge_cross_area_links".to_string(),
        ));
    }
    if remove.exits.iter().any(|exit| exit.to_unknown) {
        return Err(CloudError::InvalidInput(
            "cannot merge a room whose exits include a redacted destination".to_string(),
        ));
    }

    let can_set_secrets =
        details.area.effective_access().is_owner || details.area.effective_access().include_secrets;
    if !can_set_secrets {
        return Err(CloudError::InvalidInput(
            "room merge requires a full secret-cleared area projection".to_string(),
        ));
    }
    let mut operations = vec![AreaMutation::AssertMergeSafe {
        keep_room_number,
        remove_room_number,
    }];

    // Retarget every same-area inbound edge first. Edges between the two
    // merged rooms disappear; duplicates already reaching the kept room are
    // collapsed instead of manufacturing parallel traversal edges.
    let mut planned_inbound = Vec::new();
    for source in &details.rooms {
        if source.room_number == remove_room_number {
            continue;
        }
        for exit in source.exits.iter().filter(|exit| {
            exit.to_area_id == Some(area_id)
                && exit.to_room_number == Some(remove_room_number)
                && !exit.to_unknown
        }) {
            if source.room_number == keep_room_number {
                operations.push(AreaMutation::DeleteExit { exit_id: exit.id });
                continue;
            }
            let signature = (source.room_number, exit.from_direction, exit.to_direction);
            let duplicate = planned_inbound.contains(&signature)
                || source.exits.iter().any(|candidate| {
                    candidate.id != exit.id
                        && candidate.to_area_id == Some(area_id)
                        && candidate.to_room_number == Some(keep_room_number)
                        && candidate.from_direction == exit.from_direction
                        && candidate.to_direction == exit.to_direction
                });
            if duplicate {
                operations.push(AreaMutation::DeleteExit { exit_id: exit.id });
            } else {
                planned_inbound.push(signature);
                operations.push(AreaMutation::UpdateExit {
                    exit_id: exit.id,
                    body: ExitUpdates {
                        to_area_id: Some(area_id),
                        to_room_number: Some(keep_room_number),
                        ..ExitUpdates::default()
                    },
                });
            }
        }
    }

    // Move outgoing traversal from the removed room. A matching edge on the
    // kept room wins; otherwise consume at most one dangling stub per exit
    // before creating a fresh, client-identified exit.
    let mut consumed_stubs = HashSet::new();
    let mut planned_outgoing: Vec<Exit> = Vec::new();
    for exit in &remove.exits {
        if exit.to_area_id == Some(area_id)
            && matches!(
                exit.to_room_number,
                Some(room) if room == keep_room_number || room == remove_room_number
            )
        {
            continue;
        }
        let already_present = keep
            .exits
            .iter()
            .chain(planned_outgoing.iter())
            .any(|candidate| same_exit_destination(candidate, exit));
        if already_present {
            continue;
        }

        if let Some(stub) = keep.exits.iter().find(|candidate| {
            !consumed_stubs.contains(&candidate.id)
                && candidate.from_direction == exit.from_direction
                && candidate.to_area_id.is_none()
                && candidate.to_room_number.is_none()
                && !candidate.to_unknown
        }) {
            consumed_stubs.insert(stub.id);
            operations.push(AreaMutation::UpdateExit {
                exit_id: stub.id,
                body: ExitUpdates {
                    to_area_id: exit.to_area_id,
                    to_room_number: exit.to_room_number,
                    to_direction: exit.to_direction,
                    path: Some(exit.path.clone()),
                    is_hidden: Some(exit.is_hidden),
                    is_closed: Some(exit.is_closed),
                    is_locked: Some(exit.is_locked),
                    weight: Some(exit.weight),
                    command: Some(exit.command.clone()),
                    is_secret: can_set_secrets.then_some(exit.is_secret),
                    ..ExitUpdates::default()
                },
            });
        } else {
            operations.push(AreaMutation::CreateExit {
                room_number: keep_room_number,
                body: ExitArgs {
                    id: Some(ExitId(Uuid::new_v4())),
                    connection_id: None,
                    new_connection_id: None,
                    from_direction: exit.from_direction,
                    to_area_id: exit.to_area_id,
                    to_room_number: exit.to_room_number,
                    to_direction: exit.to_direction,
                    path: Some(exit.path.clone()),
                    is_hidden: exit.is_hidden,
                    is_closed: exit.is_closed,
                    is_locked: exit.is_locked,
                    weight: exit.weight,
                    command: Some(exit.command.clone()),
                    is_secret: can_set_secrets.then_some(exit.is_secret),
                },
            });
        }
        planned_outgoing.push(exit.clone());
    }
    operations.push(AreaMutation::DeleteRoom {
        room_number: remove_room_number,
    });
    Ok(operations)
}

/// One area document as the JSON import surface accepts it (§8.4),
/// dispatched on `format_version` at deserialization:
///
/// - **absent or 1** — parsed through the explicit
///   [`crate::backends::local_migration::LegacyAreaV1`] DTO and migrated by
///   [`crate::backends::local_migration::migrate_v1`] (which reports
///   reciprocal-looking pairs that stayed one-way through the log channel);
/// - **2** — taken verbatim ([`Mapper::import_areas`] still runs the
///   invariant checks before any write);
/// - **newer** — rejected outright, without a partial import.
///
/// The v2 types themselves never tolerate v1 input; this wrapper is the one
/// place the two formats meet.
#[derive(Debug, Clone)]
pub struct AreaImportDocument(pub AreaWithDetails);

impl AreaImportDocument {
    #[must_use]
    pub fn into_inner(self) -> AreaWithDetails {
        self.0
    }
}

impl<'de> serde::Deserialize<'de> for AreaImportDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        let version = value
            .get("format_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        match version {
            1 => {
                let legacy: crate::backends::local_migration::LegacyAreaV1 =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                Ok(Self(crate::backends::local_migration::migrate_v1(legacy)))
            }
            2 => serde_json::from_value(value)
                .map(Self)
                .map_err(D::Error::custom),
            newer => Err(D::Error::custom(format!(
                "area document format v{newer} is newer than this client \
                 (max v{}); refusing the import",
                crate::AREA_FORMAT_VERSION
            ))),
        }
    }
}

/// The §8.4 v2 import invariants, checked before any write: every exit's
/// `connection_id` resolves in the document's `connections`, and every
/// Connection has one or two member exits. A violation rejects the whole
/// import.
pub(crate) fn validate_import_document(details: &AreaWithDetails) -> CloudResult<()> {
    let mut members: HashMap<crate::ConnectionId, u32> = details
        .connections
        .iter()
        .map(|connection| (connection.id, 0))
        .collect();
    for room in &details.rooms {
        for exit in &room.exits {
            let Some(count) = members.get_mut(&exit.connection_id) else {
                return Err(CloudError::InvalidInput(format!(
                    "import of area {} ({}) rejected: exit {} references connection {}, \
                     which is not in the document",
                    details.area.name, details.area.id, exit.id, exit.connection_id
                )));
            };
            *count += 1;
        }
    }
    if let Some((id, count)) = members.iter().find(|(_, count)| !(1..=2).contains(*count)) {
        return Err(CloudError::InvalidInput(format!(
            "import of area {} ({}) rejected: connection {id} has {count} member exits \
             (a Connection has one or two)",
            details.area.name, details.area.id
        )));
    }
    Ok(())
}

/// Total rooms admitted across a session's ephemeral areas. A guard against a
/// server minting unbounded room ids through an auto-mapper, not a sizing
/// statement — procedural games legitimately reach ~1M rooms, so the cap sits
/// well above that. Updates to existing rooms are never refused.
pub const EPHEMERAL_ROOM_CAP: usize = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaLoadSource {
    Cache,
    Remote,
    Unknown,
}

impl std::fmt::Display for AreaLoadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AreaLoadSource::Cache => write!(f, "cache"),
            AreaLoadSource::Remote => write!(f, "remote"),
            AreaLoadSource::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AreaLoadStat {
    pub area_id: AreaId,
    pub name: String,
    pub revision: i64,
    pub load_duration: Duration,
    pub source: AreaLoadSource,
    /// Whether this area is shared *to* the viewer (owned by someone else) rather than
    /// owned by them. Drives the owned/shared breakdown in the session-start summary.
    pub shared: bool,
}

#[derive(Debug, Clone)]
pub struct LoadMapsSummary {
    pub list_duration: Duration,
    pub areas: Vec<AreaLoadStat>,
}

/// Sync statistics for diagnostics
#[derive(Debug, Default)]
pub struct SyncStats {
    pub operations_sent: AtomicU64,
    pub operations_succeeded: AtomicU64,
    pub operations_failed: AtomicU64,
}

impl SyncStats {
    #[must_use]
    pub fn operations_sent(&self) -> u64 {
        self.operations_sent.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn operations_succeeded(&self) -> u64 {
        self.operations_succeeded.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn operations_failed(&self) -> u64 {
        self.operations_failed.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn pending_operations(&self) -> u64 {
        self.operations_sent()
            .saturating_sub(self.operations_succeeded())
            .saturating_sub(self.operations_failed())
    }
}

/// Tracks an acknowledged (non-journaled) metadata request through shutdown
/// draining and sync-engine refetch deferral. Cancellation counts as failure
/// and always releases the per-area marker.
struct AcknowledgedWrite {
    area_id: AreaId,
    pending_by_area: Arc<Mutex<HashMap<AreaId, u64>>>,
    metadata_writes_by_area: Option<Arc<Mutex<HashMap<AreaId, u64>>>>,
    stats: Arc<SyncStats>,
    settled: bool,
}

impl AcknowledgedWrite {
    fn new(
        area_id: AreaId,
        pending_by_area: Arc<Mutex<HashMap<AreaId, u64>>>,
        stats: Arc<SyncStats>,
    ) -> Self {
        stats.operations_sent.fetch_add(1, Ordering::Relaxed);
        *pending_by_area.lock().entry(area_id).or_insert(0) += 1;
        Self {
            area_id,
            pending_by_area,
            metadata_writes_by_area: None,
            stats,
            settled: false,
        }
    }

    fn new_metadata(
        area_id: AreaId,
        pending_by_area: Arc<Mutex<HashMap<AreaId, u64>>>,
        metadata_writes_by_area: Arc<Mutex<HashMap<AreaId, u64>>>,
        stats: Arc<SyncStats>,
    ) -> Self {
        let mut write = Self::new(area_id, pending_by_area, stats);
        *metadata_writes_by_area.lock().entry(area_id).or_insert(0) += 1;
        write.metadata_writes_by_area = Some(metadata_writes_by_area);
        write
    }

    fn release_metadata_marker(&self) {
        if let Some(writes) = &self.metadata_writes_by_area {
            Inner::decrement_pending(writes, self.area_id);
        }
    }

    fn settle(mut self, succeeded: bool) {
        let counter = if succeeded {
            &self.stats.operations_succeeded
        } else {
            &self.stats.operations_failed
        };
        counter.fetch_add(1, Ordering::Relaxed);
        Inner::decrement_pending(&self.pending_by_area, self.area_id);
        self.release_metadata_marker();
        self.settled = true;
    }
}

impl Drop for AcknowledgedWrite {
    fn drop(&mut self) {
        if !self.settled {
            self.stats.operations_failed.fetch_add(1, Ordering::Relaxed);
            Inner::decrement_pending(&self.pending_by_area, self.area_id);
            self.release_metadata_marker();
        }
    }
}

pub(crate) struct AreaDeleteFence {
    area_id: AreaId,
    pending: Arc<PendingQueue>,
    armed: bool,
    request_started: bool,
}

impl AreaDeleteFence {
    fn begin(area_id: AreaId, pending: Arc<PendingQueue>) -> CloudResult<Self> {
        pending.begin_delete(area_id)?;
        Ok(Self {
            area_id,
            pending,
            armed: true,
            request_started: false,
        })
    }

    fn prepare(&mut self) -> CloudResult<()> {
        self.pending.prepare_delete(self.area_id)
    }

    fn request_started(&mut self) {
        self.request_started = true;
    }

    fn reconcile(mut self) {
        self.pending.mark_delete_ambiguous(self.area_id);
        self.armed = false;
    }

    fn commit(mut self) -> CloudResult<Vec<PendingEnvelope>> {
        match self.pending.commit_delete(self.area_id) {
            Ok(removed) => {
                self.armed = false;
                Ok(removed)
            }
            Err(error) => {
                // The backend deletion already succeeded. Preserve the intent
                // and reconcile instead of reopening edits when its local
                // tombstone transition could not finish.
                self.pending.mark_delete_ambiguous(self.area_id);
                self.armed = false;
                Err(error)
            }
        }
    }
}

impl Drop for AreaDeleteFence {
    fn drop(&mut self) {
        if self.armed {
            if self.request_started {
                self.pending.mark_delete_ambiguous(self.area_id);
            } else if let Err(error) = self.pending.abort_delete(self.area_id) {
                warn!(
                    "failed to durably abort unissued delete fence for area {}: {error}; keeping the area fenced",
                    self.area_id
                );
            }
        }
    }
}

/// In-memory pre-delete fence held while a move copies its destination.
/// Dropping an uncommitted fence reopens the source and its queued WAL.
pub(crate) struct AreaMoveFence {
    area_id: AreaId,
    delete_fence: Option<AreaDeleteFence>,
}

impl AreaMoveFence {
    #[must_use]
    pub(crate) fn area_id(&self) -> AreaId {
        self.area_id
    }

    fn into_delete_fence(mut self) -> AreaDeleteFence {
        self.delete_fence
            .take()
            .expect("a move fence is committed at most once")
    }
}

#[derive(Clone)]
pub struct Mapper {
    inner: Arc<Inner>,
}
pub struct Inner {
    atlas_id: ArcSwap<Option<AtlasId>>,
    atlas_cache: ArcSwap<AtlasCache>,

    backend: Arc<dyn MapperBackend + Send + Sync>,

    // Sync diagnostics
    sync_stats: Arc<SyncStats>,

    // Sync engine state (see mapper::sync_engine)
    sync_status: ArcSwap<SyncStatus>,
    sync_revision: AtomicU64,
    /// Bumped before resolving a changed credential. UI inventories that are
    /// not stored in the atlas cache use this to clear the previous account's
    /// metadata even when `/me` or the subsequent atlas fetch fails.
    auth_projection_revision: AtomicU64,
    sync_notify: Arc<Notify>,
    /// In-flight local write operations per area; the sync engine defers
    /// refetching an area while its count is non-zero.
    pending_by_area: Arc<Mutex<HashMap<AreaId, u64>>>,
    /// Acknowledged area metadata writes (rename/refile) that are not part of
    /// the content WAL. Relocation must not snapshot while one is in flight.
    metadata_writes_by_area: Arc<Mutex<HashMap<AreaId, u64>>>,
    /// Operations already represented in session diagnostics. Reactivating a
    /// dormant viewer journal must not count the same durable edit twice.
    accounted_operations: Mutex<HashSet<OperationId>>,

    /// The CAS pending-write store: every content mutation waits here as an
    /// envelope until the backend acknowledges it (see [`mapper::pending`]).
    ///
    /// [`mapper::pending`]: crate::mapper::pending
    pending: Arc<PendingQueue>,

    /// One teaching warning when the ephemeral room cap refuses a creation.
    ephemeral_cap_warned: AtomicBool,

    /// Initial-load gate for presence-checked imports: `None` until the first
    /// [`Inner::load_all_areas`] completes, then whether it succeeded.
    initial_load: tokio::sync::watch::Sender<Option<bool>>,
    /// Serializes presence-checked imports so two concurrent seeds cannot
    /// both miss (and then both import) the same area name.
    import_gate: tokio::sync::Mutex<()>,
    /// Serializes local mutation compilation, optimistic publication, and
    /// pending-queue order. This becomes the write-ahead journal boundary:
    /// cache order and replay order must never diverge.
    mutation_gate: Mutex<()>,
    /// Room numbers handed out to open scripted mutators but not yet
    /// occupied by a committed room (see [`Mapper::reserve_room_number`]).
    /// Every ambient allocation path consults this through
    /// [`Mapper::next_room_number`] so a draft and a concurrent create can
    /// never receive the same number.
    room_reservations: Mutex<HashMap<AreaId, RoomReservations>>,
}

/// Per-area reservation state: the next number a reservation would take and
/// the tokens (one per open mutator) holding numbers below it. The entry is
/// dropped when the last holder releases, returning allocation to the cache
/// maximum — an aborted mutator's numbers become available again.
#[derive(Debug, Default)]
struct RoomReservations {
    floor: i32,
    holders: HashMap<Uuid, u32>,
}

/// The outcome of a presence-checked import: what was added and what was
/// already present (by name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreasImportedIfAbsent {
    /// Ids of the areas imported by this call.
    pub added: Vec<AreaId>,
    /// Names skipped because a resident area already bears them.
    pub skipped: Vec<String>,
}

impl std::fmt::Debug for Mapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[Mapper]")
    }
}

impl Mapper {
    pub fn new(
        backend: Arc<dyn MapperBackend + Send + Sync>,
        cache_dir: impl Into<PathBuf>,
    ) -> Self {
        let cache_dir = cache_dir.into();
        let journal_dir = cache_dir.join("pending-mutations");
        Self::new_with_journal(backend, cache_dir, journal_dir)
    }

    pub fn new_with_journal(
        backend: Arc<dyn MapperBackend + Send + Sync>,
        cache_dir: impl Into<PathBuf>,
        journal_dir: impl Into<PathBuf>,
    ) -> Self {
        let cache = AtlasCache::new_with_areas(HashMap::new(), Arc::new(HashSet::new()));

        let cache_dir = cache_dir.into();
        if let Err(err) = fs::create_dir_all(&cache_dir) {
            warn!(
                "Failed to create mapper cache directory {}: {err}",
                cache_dir.display()
            );
        }

        let supports_sync = backend.supports_sync();
        let journal_namespace = backend
            .mutation_journal_namespace()
            .unwrap_or_else(|| "non-cloud".to_string());
        let initial_state = if supports_sync {
            SyncState::Idle
        } else {
            SyncState::Disabled
        };
        let pending = Arc::new(PendingQueue::with_journal_namespace(
            journal_dir.into(),
            journal_namespace,
        ));
        let recovered_local = pending.recovered_local_operations();
        let mut pending_by_area = HashMap::new();
        let mut accounted_operations = HashSet::new();
        for (area_id, operation_id) in &recovered_local {
            *pending_by_area.entry(*area_id).or_insert(0) += 1;
            accounted_operations.insert(*operation_id);
        }
        let sync_stats = Arc::new(SyncStats::default());
        sync_stats
            .operations_sent
            .store(recovered_local.len() as u64, Ordering::Relaxed);

        let inner = Inner {
            atlas_id: ArcSwap::from_pointee(None),
            atlas_cache: ArcSwap::from_pointee(cache),
            backend,
            sync_stats,
            sync_status: ArcSwap::from_pointee(SyncStatus {
                state: initial_state,
                last_error: None,
                last_sync: None,
            }),
            sync_revision: AtomicU64::new(0),
            auth_projection_revision: AtomicU64::new(0),
            sync_notify: Arc::new(Notify::new()),
            pending_by_area: Arc::new(Mutex::new(pending_by_area)),
            metadata_writes_by_area: Arc::new(Mutex::new(HashMap::new())),
            accounted_operations: Mutex::new(accounted_operations),
            pending,
            ephemeral_cap_warned: AtomicBool::new(false),
            initial_load: tokio::sync::watch::channel(None).0,
            import_gate: tokio::sync::Mutex::new(()),
            mutation_gate: Mutex::new(()),
            room_reservations: Mutex::new(HashMap::new()),
        };

        let mapper = Self {
            inner: Arc::new(inner),
        };

        Inner::spawn_mutation_worker(&mapper.inner);

        if supports_sync {
            sync_engine::spawn(&mapper.inner);
        }

        mapper
    }

    /// Wake the background sync engine for an immediate tick (no-op when the
    /// backend has no sync support).
    pub fn sync_now(&self) {
        self.inner.sync_notify.notify_one();
    }

    /// Snapshot of the sync engine's current status.
    #[must_use]
    pub fn sync_status(&self) -> SyncStatus {
        SyncStatus::clone(&self.inner.sync_status.load())
    }

    /// Monotonic counter bumped each time the sync engine swaps the atlas
    /// cache; UIs can poll it cheaply to detect background changes.
    #[must_use]
    pub fn sync_revision(&self) -> u64 {
        self.inner.sync_revision.load(Ordering::Acquire)
    }

    /// Monotonic credential-boundary counter. Unlike [`Self::sync_revision`],
    /// this changes only when the authenticated projection is invalidated.
    #[must_use]
    pub fn auth_projection_revision(&self) -> u64 {
        self.inner.auth_projection_revision.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn get_current_atlas(&self) -> Arc<AtlasCache> {
        self.inner.get_current_atlas()
    }

    /// Replaces the set of disabled areas wholesale. Disabled areas drop out
    /// of the room-identification lookup tables and stop being routed
    /// *through* (they stay rendered and explicitly addressable). Ids not yet
    /// in the cache are kept, so an area disabled before sync lands it stays
    /// disabled when it arrives. Full table rebuild; toggles are rare.
    pub fn set_disabled_areas(&self, disabled: HashSet<AreaId>) {
        let disabled = Arc::new(disabled);
        self.inner
            .atlas_cache
            .rcu(|cache| Arc::new(cache.with_disabled_areas(disabled.clone())));
    }

    /// Convenience single-area toggle over [`Self::set_disabled_areas`].
    pub fn set_area_enabled(&self, area_id: AreaId, enabled: bool) {
        self.inner.atlas_cache.rcu(|cache| {
            let mut disabled = HashSet::clone(cache.disabled_areas());
            let changed = if enabled {
                disabled.remove(&area_id)
            } else {
                disabled.insert(area_id)
            };
            if changed {
                Arc::new(cache.with_disabled_areas(Arc::new(disabled)))
            } else {
                cache.clone()
            }
        });
    }

    /// Snapshot of the currently disabled areas.
    #[must_use]
    pub fn disabled_areas(&self) -> HashSet<AreaId> {
        HashSet::clone(self.inner.atlas_cache.load().disabled_areas())
    }

    /// Whether the area is enabled on the manual active/inactive axis only
    /// (true for areas not in the cache). Ignores per-server scope exclusion, so
    /// the editor's per-area active switch reflects only the user's toggle. Use
    /// [`Self::is_area_included`] to ask whether an area actually participates in
    /// room identification/routing.
    #[must_use]
    pub fn is_area_enabled(&self, area_id: &AreaId) -> bool {
        self.inner.atlas_cache.load().is_area_enabled(area_id)
    }

    /// Whether the area participates in room identification and routing: neither
    /// manually disabled nor per-server scope-excluded (true for areas not in
    /// the cache). This is the union enumeration/identification callers honor.
    #[must_use]
    pub fn is_area_included(&self, area_id: &AreaId) -> bool {
        self.inner.atlas_cache.load().is_area_included(area_id)
    }

    /// Cross-entry rescue probe: resolve a server-global external id against the
    /// scope-excluded areas only (maps homed on a *different* server entry).
    /// Returns the matched room plus its atlas id/name, or `None`. Used before
    /// the auto-mapper mints ephemeral rooms so a lagging sibling entry doesn't
    /// produce a duplicate map. See [`AtlasCache::find_room_elsewhere_by_external_id`].
    #[must_use]
    pub fn find_room_elsewhere_by_external_id(&self, external_id: &str) -> Option<ElsewhereMatch> {
        self.inner
            .atlas_cache
            .load()
            .find_room_elsewhere_by_external_id(external_id)
    }

    /// Replaces the per-server scope-exclusion sets wholesale (rcu-swapping the
    /// atlas cache like [`Self::set_disabled_areas`]). Scope-excluded atlases
    /// and atlas-less areas drop out of every room-identification lookup table
    /// and are treated as walls in routing — semantically identical to a manual
    /// disable, but stored on a separate axis so the user's manual toggle stays
    /// intact. Keying by atlas id means an area that later syncs into an
    /// excluded atlas is excluded automatically, with no recomputation. Full
    /// table rebuild; scope changes are rare.
    pub fn set_scope_exclusions(
        &self,
        excluded_atlases: HashSet<AtlasId>,
        excluded_areas: HashSet<AreaId>,
    ) {
        let atlases = Arc::new(excluded_atlases);
        let areas = Arc::new(excluded_areas);
        self.inner
            .atlas_cache
            .rcu(|cache| Arc::new(cache.with_scope_exclusions(atlases.clone(), areas.clone())));
    }

    /// Create an area in the default durable tier — cloud when signed in,
    /// local otherwise — filed into the current recording target when one is
    /// set. This is the storage-less creation surface behind scripts'
    /// `createArea(name)`; callers that need a specific tier or folder use
    /// [`Self::create_area_at`].
    ///
    /// # Errors
    /// Propagates the backend's create error (e.g. unauthorized, network).
    pub fn create_area(&self, name: String) -> impl Future<Output = CloudResult<AreaId>> {
        self.inner.create_area(name)
    }

    /// Create an area in the session-lifetime ephemeral tier: in-memory,
    /// never persisted or synced, gone when the session closes. The default
    /// landing zone for protocol-driven auto-mapping; keeping one is an
    /// explicit [`Self::export_area`] → [`Self::import_areas`] copy.
    ///
    /// # Errors
    /// Propagates the backend's create error (the ephemeral tier itself is
    /// infallible; a non-composite backend without the tier routes this to
    /// its default create path).
    #[deprecated(
        since = "0.5.3",
        note = "use create_area_at with MapStorage::Session; supported through Smudgy 0.5.x and removed in 0.6.0"
    )]
    pub fn create_area_ephemeral(&self, name: String) -> impl Future<Output = CloudResult<AreaId>> {
        self.create_area_at(name, MapDestination::loose(MapStorage::Session))
    }

    /// Create an area at an explicit storage + folder destination.
    ///
    /// This is the canonical creation surface for explicit destinations; the
    /// storage-less [`Self::create_area`] covers the default durable tier.
    /// The deprecated [`Self::create_area_ephemeral`] delegates here only
    /// through 0.5.x and is removed in 0.6.0.
    pub fn create_area_at(
        &self,
        name: String,
        destination: MapDestination,
    ) -> impl Future<Output = CloudResult<AreaId>> {
        self.inner.create_area_at(name, destination)
    }

    /// Whether `area_id` lives in the ephemeral (session-lifetime) tier.
    #[must_use]
    #[deprecated(
        since = "0.5.3",
        note = "use area_storage(area_id) == MapStorage::Session; supported through Smudgy 0.5.x and removed in 0.6.0"
    )]
    pub fn is_ephemeral(&self, area_id: &AreaId) -> bool {
        self.area_storage(area_id) == MapStorage::Session
    }

    /// The authoritative storage tier for a loaded area.
    #[must_use]
    pub fn area_storage(&self, area_id: &AreaId) -> MapStorage {
        if self.inner.backend.ephemeral_area_ids().contains(area_id) {
            MapStorage::Session
        } else if self.inner.backend.local_area_ids().contains(area_id) {
            MapStorage::Local
        } else {
            MapStorage::Cloud
        }
    }

    /// The authoritative storage tier for an owned atlas.
    #[must_use]
    pub fn atlas_storage(&self, atlas_id: &AtlasId) -> MapStorage {
        if self.inner.backend.local_atlas_ids().contains(atlas_id) {
            MapStorage::Local
        } else {
            MapStorage::Cloud
        }
    }

    /// The next free room number for an area, skipping numbers reserved by
    /// open scripted mutators. Every ambient creation path (script
    /// `createRoom`, the map editor's place/paste gestures) must allocate
    /// through this rather than the raw cache maximum, or a concurrent
    /// mutator draft and the ambient create would silently merge into one
    /// room. Returns `None` when the area is not loaded.
    #[must_use]
    pub fn next_room_number(&self, area_id: &AreaId) -> Option<RoomNumber> {
        let base = self
            .inner
            .atlas_cache
            .load()
            .get_area(area_id)?
            .next_room_number()
            .0;
        let reservations = self.inner.room_reservations.lock();
        let floor = reservations.get(area_id).map_or(base, |state| state.floor);
        Some(RoomNumber(base.max(floor)))
    }

    /// Reserve the next free room number for an open scripted mutator.
    /// The number is provisional: no room exists until the mutator's batch
    /// commits, but ambient allocation skips it until every reservation
    /// held under `token` is released. Releasing without committing (an
    /// aborted mutator) returns the numbers to the allocator.
    ///
    /// # Errors
    /// [`CloudError::AreaNotFound`] when the area is not loaded.
    pub fn reserve_room_number(&self, area_id: &AreaId, token: Uuid) -> CloudResult<RoomNumber> {
        let base = self
            .inner
            .atlas_cache
            .load()
            .get_area(area_id)
            .ok_or(CloudError::AreaNotFound(*area_id))?
            .next_room_number()
            .0;
        let mut reservations = self.inner.room_reservations.lock();
        let state = reservations.entry(*area_id).or_default();
        let number = base.max(state.floor);
        state.floor = number + 1;
        *state.holders.entry(token).or_insert(0) += 1;
        Ok(RoomNumber(number))
    }

    /// Release every room-number reservation held under `token` for an
    /// area. Idempotent; when the last holder releases, allocation falls
    /// back to the cache maximum.
    pub fn release_room_reservations(&self, area_id: &AreaId, token: Uuid) {
        let mut reservations = self.inner.room_reservations.lock();
        if let Some(state) = reservations.get_mut(area_id) {
            state.holders.remove(&token);
            if state.holders.is_empty() {
                reservations.remove(area_id);
            }
        }
    }

    /// Area ids in session storage — the set the editor's atlas tree and
    /// per-area preference writes exclude from durable filing.
    #[must_use]
    pub fn session_area_ids(&self) -> HashSet<AreaId> {
        self.inner.backend.ephemeral_area_ids()
    }

    /// Compatibility name for [`Self::session_area_ids`].
    #[must_use]
    #[deprecated(
        since = "0.5.3",
        note = "use session_area_ids; supported through Smudgy 0.5.x and removed in 0.6.0"
    )]
    pub fn ephemeral_area_ids(&self) -> HashSet<AreaId> {
        self.session_area_ids()
    }

    /// Like [`Self::create_area`] but files the new area into `atlas_id`
    /// (`Some`) or leaves it loose (`None`), bypassing the recording target.
    ///
    /// # Errors
    /// Propagates the backend's create error (e.g. unauthorized, network).
    pub fn create_area_in(
        &self,
        name: String,
        atlas_id: Option<AtlasId>,
    ) -> impl Future<Output = CloudResult<AreaId>> {
        self.inner.create_area_in(name, atlas_id)
    }

    /// Import full areas into the local tier (fresh ids), returning their new ids. See
    /// [`Inner::import_areas`].
    ///
    /// # Errors
    /// Propagates the backend's persistence error.
    pub async fn import_areas(&self, areas: Vec<AreaWithDetails>) -> CloudResult<Vec<AreaId>> {
        self.inner.import_areas(areas).await
    }

    /// Presence-checked variant of [`Self::import_areas`]: imports only the areas whose name no
    /// resident area already bears, and reports the rest as skipped. See
    /// [`Inner::import_areas_if_absent`] for the exact contract (initial-load gate, unfiltered
    /// presence check, serialization against concurrent seeds).
    ///
    /// # Errors
    /// Errors when the initial area load failed or the local backend's persistence fails.
    pub async fn import_areas_if_absent(
        &self,
        areas: Vec<AreaWithDetails>,
    ) -> CloudResult<AreasImportedIfAbsent> {
        self.inner.import_areas_if_absent(areas).await
    }

    /// Serialize an area to its full [`AreaWithDetails`]. See [`Inner::export_area`].
    ///
    /// # Errors
    /// Propagates the backend's read error.
    pub async fn export_area(&self, area_id: AreaId) -> CloudResult<AreaWithDetails> {
        self.inner.export_area(area_id).await
    }

    /// Snapshot the displayed area, including durably-journaled optimistic
    /// edits that have not reached their backend yet. Relocation uses this
    /// rather than a backend export so moving a map can never omit edits the
    /// user can already see.
    pub(crate) fn snapshot_area(&self, area_id: AreaId) -> CloudResult<AreaWithDetails> {
        self.inner
            .atlas_cache
            .load()
            .get_area(&area_id)
            .map(|area| area.to_details())
            .ok_or(CloudError::AreaNotFound(area_id))
    }

    /// Freeze source content before a move snapshot. The fence is deliberately
    /// not durable yet: a crash during destination copy must leave the source
    /// and its WAL usable. Existing in-flight content writes are allowed to
    /// finish; future content and metadata writes are rejected until these
    /// guards are committed or dropped.
    pub(crate) fn begin_area_move(&self, area_ids: &[AreaId]) -> CloudResult<Vec<AreaMoveFence>> {
        self.inner.begin_area_move(area_ids)
    }

    pub(crate) async fn wait_area_move_quiescent(&self, fences: &[AreaMoveFence]) {
        for fence in fences {
            self.inner
                .pending
                .wait_until_delete_quiescent(fence.area_id())
                .await;
        }
    }

    /// Delete a move's source after its destination copy is fully
    /// acknowledged. `expected_rev` is the backend revision the move
    /// snapshot was taken against: the delete re-reads the authoritative
    /// revision first and refuses on drift, so a behind-cache client fails
    /// safe into the documented harmless-duplicate outcome instead of
    /// destroying edits it never saw.
    pub(crate) async fn commit_area_move(
        &self,
        fence: AreaMoveFence,
        expected_rev: Option<i64>,
    ) -> CloudResult<()> {
        let area_id = fence.area_id();
        self.inner
            .delete_area_with_fence(area_id, fence.into_delete_fence(), expected_rev)
            .await
    }

    /// The last backend-acknowledged revision for an area, when one is
    /// known. Optimistic cache revisions (which run ahead while envelopes
    /// are queued) never surface here.
    pub(crate) fn confirmed_area_rev(&self, area_id: AreaId) -> Option<i64> {
        self.inner.pending.confirmed_rev(area_id).0
    }

    /// The viewer's effective access for an area, or `None` if it isn't in the current atlas.
    #[must_use]
    pub fn area_effective_access(&self, area_id: AreaId) -> Option<AreaAccess> {
        self.inner.area_effective_access(area_id)
    }

    pub fn load_all_areas(&self) -> impl Future<Output = CloudResult<LoadMapsSummary>> {
        self.inner.load_all_areas()
    }

    /// Rename an area and update the cache only after backend acknowledgement.
    pub async fn rename_area(&self, area_id: AreaId, name: &str) -> CloudResult<()> {
        self.inner.rename_area_and_wait(area_id, name).await
    }

    /// Compatibility alias for automation callers that emphasizes the
    /// acknowledged rename contract.
    pub async fn rename_area_and_wait(&self, area_id: AreaId, name: &str) -> CloudResult<()> {
        self.rename_area(area_id, name).await
    }

    /// Delete an area from the cache only after backend acknowledgement.
    pub async fn delete_area(&self, area_id: AreaId) -> CloudResult<()> {
        self.inner.delete_area_and_wait(area_id).await
    }

    /// Compatibility alias for automation callers that emphasizes the
    /// acknowledged delete contract.
    pub async fn delete_area_and_wait(&self, area_id: AreaId) -> CloudResult<()> {
        self.delete_area(area_id).await
    }

    // === ATLAS (FOLDER) OPERATIONS ===

    /// List the viewer's own atlases (folders). Resolves against whichever
    /// backend(s) this mapper fans across.
    ///
    /// # Errors
    /// Propagates the backend's list error (e.g. unauthorized, network).
    pub fn list_atlases(&self) -> impl Future<Output = CloudResult<Vec<AtlasListItem>>> {
        let backend = self.inner.backend.clone();
        async move { backend.list_atlases().await }
    }

    /// List every area row visible to the viewer, straight from the backend
    /// (not the geometry cache). This is the **only** carrier of the list-only
    /// [`Area::family_token`] — `get_area` and the on-disk cache never include
    /// it — so the map editor calls this to build its per-viewer copy-family
    /// index. Callers must bucket `family_token` in memory for the current
    /// list and never persist it (see the field docs).
    ///
    /// # Errors
    /// Propagates the backend's list error (e.g. unauthorized, network).
    pub fn list_areas(&self) -> impl Future<Output = CloudResult<Vec<Area>>> {
        let backend = self.inner.backend.clone();
        async move { backend.list_areas().await }
    }

    /// Create an empty atlas (folder), routed to the backend's default
    /// durable tier — cloud when signed in, local otherwise. Callers that
    /// need a specific tier use `create_atlas_at`.
    ///
    /// # Errors
    /// Propagates the backend's create error (e.g. unauthorized, network).
    pub fn create_atlas(&self, name: String) -> impl Future<Output = CloudResult<Atlas>> {
        let backend = self.inner.backend.clone();
        async move { backend.create_atlas(&name).await }
    }

    /// Create an empty atlas with an explicit tier preference (`prefer_local`).
    /// A pure-cloud mapper ignores the hint; a two-tier mapper honors it
    /// (falling back to local only when cloud is unavailable).
    ///
    /// # Errors
    /// Propagates the backend's create error (e.g. unauthorized, network).
    pub fn create_atlas_in(
        &self,
        name: String,
        prefer_local: bool,
    ) -> impl Future<Output = CloudResult<Atlas>> {
        let backend = self.inner.backend.clone();
        async move { backend.create_atlas_in(&name, prefer_local).await }
    }

    /// Create an atlas in an explicit durable storage tier.
    pub fn create_atlas_at(
        &self,
        name: String,
        storage: MapStorage,
    ) -> impl Future<Output = CloudResult<Atlas>> {
        let backend = self.inner.backend.clone();
        async move { backend.create_atlas_at(&name, storage).await }
    }

    /// Rename an atlas.
    ///
    /// # Errors
    /// Propagates the backend's rename error (owner-only; uniform 404
    /// otherwise).
    pub fn rename_atlas(
        &self,
        atlas_id: AtlasId,
        name: String,
    ) -> impl Future<Output = CloudResult<Atlas>> {
        let backend = self.inner.backend.clone();
        async move { backend.rename_atlas(&atlas_id, &name).await }
    }

    /// Delete an atlas. Its member areas survive and become loose.
    ///
    /// # Errors
    /// Propagates the backend's delete error (owner-only; uniform 404
    /// otherwise).
    pub fn delete_atlas(&self, atlas_id: AtlasId) -> impl Future<Output = CloudResult<()>> {
        let backend = self.inner.backend.clone();
        async move { backend.delete_atlas(&atlas_id).await }
    }

    /// File an owned area into `atlas_id` (`Some`) or pull it loose
    /// (`None`). The cache regroups only after the backend acknowledges the
    /// move, so a process exit cannot strand an optimistic-only result.
    pub async fn move_area_to_atlas(
        &self,
        area_id: AreaId,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<()> {
        self.inner
            .move_area_to_atlas_and_wait(area_id, atlas_id)
            .await
    }

    pub fn set_area_property(
        &self,
        area_id: AreaId,
        name: String,
        value: String,
    ) -> CloudResult<MutationSubmission> {
        self.inner.set_area_property(area_id, name, value)
    }

    pub fn delete_area_property(
        &self,
        area_id: AreaId,
        name: String,
    ) -> CloudResult<MutationSubmission> {
        self.inner.delete_area_property(area_id, name)
    }

    pub fn upsert_room(
        &self,
        room_key: RoomKey,
        updates: RoomUpdates,
    ) -> CloudResult<MutationSubmission> {
        self.inner.upsert_room(room_key, updates)
    }

    /// Upserts a batch of rooms in one cache update (one index rebuild).
    pub fn upsert_rooms(
        &self,
        area_id: AreaId,
        updates: Vec<(RoomNumber, RoomUpdates)>,
    ) -> CloudResult<Vec<MutationSubmission>> {
        self.inner.upsert_rooms(area_id, updates)
    }

    pub fn delete_room(&self, room_key: RoomKey) -> CloudResult<MutationSubmission> {
        self.inner.delete_room(room_key)
    }

    pub fn set_room_property(
        &self,
        room_key: RoomKey,
        name: String,
        value: String,
    ) -> CloudResult<MutationSubmission> {
        self.inner.set_room_property(room_key, name, value)
    }

    pub fn delete_room_property(
        &self,
        room_key: RoomKey,
        name: String,
    ) -> CloudResult<MutationSubmission> {
        self.inner.delete_room_property(room_key, name)
    }

    pub fn add_room_tag(&self, room_key: RoomKey, tag: String) -> CloudResult<MutationSubmission> {
        self.inner.add_room_tag(room_key, tag)
    }

    pub fn remove_room_tag(
        &self,
        room_key: RoomKey,
        tag: String,
    ) -> CloudResult<MutationSubmission> {
        self.inner.remove_room_tag(room_key, tag)
    }

    /// Updates traversal fields without silently breaking a paired
    /// Connection. Equal fields are removed before enqueue; a real topology
    /// change on a pair returns `unlink_before_edit`.
    pub fn update_exit(
        &self,
        room_key: RoomKey,
        exit_id: ExitId,
        updates: ExitUpdates,
    ) -> CloudResult<MutationSubmission> {
        self.inner
            .update_exit(room_key, exit_id, updates, PairedExitPolicy::Reject)
    }

    /// Explicit structural retarget: if the exit belongs to a pair, split it
    /// first and apply the update in the same atomic envelope.
    pub fn retarget_exit(
        &self,
        room_key: RoomKey,
        exit_id: ExitId,
        updates: ExitUpdates,
    ) -> CloudResult<MutationSubmission> {
        self.inner
            .update_exit(room_key, exit_id, updates, PairedExitPolicy::Split)
    }

    pub fn delete_exit(
        &self,
        room_key: RoomKey,
        exit_id: ExitId,
    ) -> CloudResult<MutationSubmission> {
        self.inner.delete_exit(room_key, exit_id)
    }

    /// Applies an ordered compound edit to the optimistic cache and enqueues
    /// it as one CAS mutation/undo unit.
    ///
    /// # Errors
    /// Returns the shared local validation error without changing the cache
    /// or queue when the proposed final Connection graph is invalid.
    pub fn mutate_area(
        &self,
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: impl Into<String>,
    ) -> CloudResult<MutationSubmission> {
        self.inner.mutate_area(
            area_id,
            operations,
            description.into(),
            PairedExitPolicy::Reject,
        )
    }

    /// Validates and durably stages every envelope before publishing any of
    /// them to the optimistic cache or pending worker.
    pub fn mutate_batches(
        &self,
        batches: Vec<AreaMutationBatch>,
    ) -> CloudResult<Vec<MutationSubmission>> {
        self.inner.mutate_batches(batches)
    }

    /// Atomically merge two rooms in one area. The kept room's metadata wins;
    /// inbound and outgoing traversal is deduplicated and rewired, paired
    /// exits are explicitly split when their topology must change, and the
    /// removed room is deleted last in the same durable envelope.
    pub fn merge_rooms(
        &self,
        area_id: AreaId,
        keep_room_number: RoomNumber,
        remove_room_number: RoomNumber,
    ) -> CloudResult<MutationSubmission> {
        self.inner
            .merge_rooms(area_id, keep_room_number, remove_room_number)
    }

    /// Creates an exit with a client-minted id: the cache updates
    /// optimistically and the envelope queues for the backend, so the
    /// returned future is already resolved (the future-shaped signature is
    /// retained for call-site stability).
    pub fn create_exit(
        &self,
        room_key: RoomKey,
        args: ExitArgs,
    ) -> impl Future<Output = CloudResult<ExitId>> {
        std::future::ready(self.inner.create_exit(room_key, args))
    }

    /// Creates an exit and returns both its client-minted id and the durable
    /// mutation submission that owns the optimistic change.
    pub fn create_exit_tracked(
        &self,
        room_key: RoomKey,
        args: ExitArgs,
    ) -> CloudResult<(ExitId, MutationSubmission)> {
        self.inner.create_exit_tracked(room_key, args)
    }

    /// Creates a label with a client-minted id; immediately resolved like
    /// [`Self::create_exit`].
    pub fn create_label(
        &self,
        area_id: AreaId,
        args: LabelArgs,
    ) -> impl Future<Output = CloudResult<LabelId>> {
        std::future::ready(self.inner.create_label(area_id, args))
    }

    /// Creates a label and returns its id together with the durable mutation
    /// submission.
    pub fn create_label_tracked(
        &self,
        area_id: AreaId,
        args: LabelArgs,
    ) -> CloudResult<(LabelId, MutationSubmission)> {
        self.inner.create_label_tracked(area_id, args)
    }

    pub fn update_label(
        &self,
        area_id: AreaId,
        label_id: LabelId,
        updates: LabelUpdates,
    ) -> CloudResult<MutationSubmission> {
        self.inner.update_label(area_id, label_id, updates)
    }

    pub fn delete_label(
        &self,
        area_id: AreaId,
        label_id: LabelId,
    ) -> CloudResult<MutationSubmission> {
        self.inner.delete_label(area_id, label_id)
    }

    /// Creates a shape with a client-minted id; immediately resolved like
    /// [`Self::create_exit`].
    pub fn create_shape(
        &self,
        area_id: AreaId,
        args: ShapeArgs,
    ) -> impl Future<Output = CloudResult<ShapeId>> {
        std::future::ready(self.inner.create_shape(area_id, args))
    }

    /// Creates a shape and returns its id together with the durable mutation
    /// submission.
    pub fn create_shape_tracked(
        &self,
        area_id: AreaId,
        args: ShapeArgs,
    ) -> CloudResult<(ShapeId, MutationSubmission)> {
        self.inner.create_shape_tracked(area_id, args)
    }

    pub fn update_shape(
        &self,
        area_id: AreaId,
        shape_id: ShapeId,
        updates: ShapeUpdates,
    ) -> CloudResult<MutationSubmission> {
        self.inner.update_shape(area_id, shape_id, updates)
    }

    pub fn delete_shape(
        &self,
        area_id: AreaId,
        shape_id: ShapeId,
    ) -> CloudResult<MutationSubmission> {
        self.inner.delete_shape(area_id, shape_id)
    }

    pub fn wait_for_sync_completion(
        &self,
        timeout_secs: u64,
    ) -> impl Future<Output = Result<bool, ()>> {
        self.inner.wait_for_sync_completion(timeout_secs)
    }

    /// Whether the backend currently holds any credential; credential-less
    /// mappers serve cached data only and skip cloud loads.
    #[must_use]
    pub fn has_credential(&self) -> bool {
        self.inner.backend.has_credential()
    }

    /// Atlas ids served by a local (never-synced, on-disk) tier; empty for a
    /// pure-cloud mapper. Lets the UI gate cloud-only affordances (e.g. Share
    /// folder) off local folders.
    #[must_use]
    pub fn local_atlas_ids(&self) -> HashSet<AtlasId> {
        self.inner.backend.local_atlas_ids()
    }

    /// Area ids served by a local tier; empty for a pure-cloud mapper. Lets the
    /// UI keep cross-tier targets out of the move-to-folder picker.
    #[must_use]
    pub fn local_area_ids(&self) -> HashSet<AreaId> {
        self.inner.backend.local_area_ids()
    }

    #[must_use]
    pub fn get_sync_stats(&self) -> &SyncStats {
        self.inner.sync_stats()
    }

    // === PENDING-QUEUE SURFACE ===

    /// Subscribe to pending-queue lifecycle events: acknowledgements,
    /// conflicts, permanent failures, per-area save-status changes, and
    /// upgrade pauses.
    #[must_use]
    pub fn subscribe_mapper_events(&self) -> tokio::sync::broadcast::Receiver<MapperEvent> {
        self.inner.pending.subscribe()
    }

    /// Journal files that could not be recovered (corrupt, unknown schema,
    /// or namespace mismatch). Their originals are moved to quarantine.
    #[must_use]
    pub fn mutation_recovery_errors(&self) -> Vec<String> {
        self.inner.pending.recovery_errors()
    }

    /// Drains journal-recovery errors after a UI surface has notified the
    /// user. Detailed paths remain available in the application log.
    pub fn take_mutation_recovery_errors(&self) -> Vec<String> {
        self.inner.pending.take_recovery_errors()
    }

    /// The area-specific save status derived from its pending queue.
    #[must_use]
    pub fn area_save_status(&self, area_id: AreaId) -> AreaSaveStatus {
        self.inner.pending.save_status(area_id)
    }

    /// The operation currently paused for conflict review in this area.
    #[must_use]
    pub fn conflicted_operation_id(&self, area_id: AreaId) -> Option<OperationId> {
        self.inner.pending.conflicted_operation_id(area_id)
    }

    /// The operation currently paused after a permanent delivery failure.
    #[must_use]
    pub fn failed_operation_id(&self, area_id: AreaId) -> Option<OperationId> {
        self.inner.pending.failed_operation_id(area_id)
    }

    /// Whether an operation is still present in this area's pending queue.
    #[must_use]
    pub fn is_operation_pending(&self, area_id: AreaId, operation_id: OperationId) -> bool {
        self.inner.pending.contains_operation(area_id, operation_id)
    }

    /// Resolves only when the backend acknowledges this exact operation;
    /// permanent failure, conflict review, or cancellation returns an error.
    pub async fn wait_for_mutation(&self, operation_id: OperationId) -> CloudResult<()> {
        self.inner
            .pending
            .wait_for_completion(operation_id)
            .await
            .map_err(CloudError::PendingOperations)
    }

    /// Resolves a conflict-paused area. Keep mine keeps every pending
    /// operation (a deliberate overwrite of the remote edit): the displayed
    /// area is rebuilt as the backend's projection plus all pending
    /// operations, then the queue resumes sending against the fresh
    /// revision. Keep theirs discards exactly the conflicted operation and
    /// rebuilds the displayed area from the backend's projection plus the
    /// operations still pending.
    pub async fn resolve_conflict(&self, area_id: AreaId, keep_mine: bool) -> CloudResult<()> {
        self.inner.resolve_conflict(area_id, keep_mine).await
    }

    /// Resolves a permanently-failed area. Retry re-arms the parked
    /// operation; discard drops it and rebuilds the displayed area from the
    /// backend's projection plus the operations still pending.
    pub async fn resolve_failed(&self, area_id: AreaId, retry: bool) -> CloudResult<()> {
        self.inner.resolve_failed(area_id, retry).await
    }

    /// Cancels a queued-but-unsent operation — the local undo of
    /// unacknowledged work. Returns whether the operation was found and
    /// removable: the head of a non-idle queue is not (in flight it is on
    /// the wire; parked it belongs to [`Self::resolve_conflict`] /
    /// [`Self::resolve_failed`]). On success the displayed area is rebuilt
    /// without the canceled operation's optimistic effect.
    pub async fn cancel_pending(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
    ) -> CloudResult<bool> {
        self.inner.cancel_pending(area_id, operation_id).await
    }
}

impl Inner {
    pub(crate) fn activate_pending_viewer(
        &self,
        viewer: Option<Uuid>,
        auth_generation: u64,
    ) -> bool {
        let _mutation_guard = self.mutation_gate.lock();
        if self.backend.auth_generation() != auth_generation {
            return false;
        }
        let activation = self.pending.activate_viewer(viewer, auth_generation);
        let mut accounted = self.accounted_operations.lock();
        let newly_accounted_operations: HashSet<_> = activation
            .added_operations
            .iter()
            .filter(|operation_id| accounted.insert(**operation_id))
            .copied()
            .collect();
        let newly_accounted = newly_accounted_operations.len() as u64;
        let newly_expired = activation
            .expired_operations
            .iter()
            .filter(|(_, operation_id)| newly_accounted_operations.contains(operation_id))
            .count() as u64;
        drop(accounted);
        if newly_accounted > 0 {
            self.sync_stats
                .operations_sent
                .fetch_add(newly_accounted, Ordering::Relaxed);
        }
        if newly_expired > 0 {
            self.sync_stats
                .operations_failed
                .fetch_add(newly_expired, Ordering::Relaxed);
        }
        let mut pending = self.pending_by_area.lock();
        for (area_id, count) in activation.removed {
            if let Some(current) = pending.get_mut(&area_id) {
                *current = current.saturating_sub(count);
                if *current == 0 {
                    pending.remove(&area_id);
                }
            }
        }
        for (area_id, count) in activation.added {
            *pending.entry(area_id).or_insert(0) += count;
        }
        true
    }

    /// Get sync statistics for diagnostics
    #[must_use]
    pub fn sync_stats(&self) -> &SyncStats {
        &self.sync_stats
    }

    /// Wait for all sync operations to complete
    ///
    /// # Arguments
    /// * `timeout_secs` - Maximum time to wait in seconds (0 = no timeout)
    ///
    /// # Returns
    /// * `Ok(true)` if all operations completed successfully
    /// * `Ok(false)` if timeout was reached with pending operations
    /// * `Err(())` if there were failed operations
    ///
    /// # Errors
    /// Returns `Err(())` once the queue drains if any sync operation failed
    /// (the unit error simply signals "completed with failures").
    pub async fn wait_for_sync_completion(&self, timeout_secs: u64) -> Result<bool, ()> {
        let start_time = std::time::Instant::now();

        loop {
            let failed = self.sync_stats.operations_failed();
            let active_writes = !self.pending_by_area.lock().is_empty();

            // Parked CAS envelopes remain active writes because they still
            // await a user decision.
            if !active_writes && self.pending.total_pending() == 0 {
                return if failed > 0 { Err(()) } else { Ok(true) };
            }

            // Check for timeout
            if timeout_secs > 0 && start_time.elapsed().as_secs() >= timeout_secs {
                return Ok(false);
            }

            // Short sleep to avoid busy waiting
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    /// Load all areas from backend into cache
    /// # Errors
    /// Returns error if backend operations fail
    pub async fn load_all_areas(&self) -> CloudResult<LoadMapsSummary> {
        let result = self.load_all_areas_inner().await;
        // Open the initial-load gate either way: presence-checked imports wait
        // on it, and distinguish success from failure by the flag's value.
        self.initial_load.send_replace(Some(result.is_ok()));
        result
    }

    async fn load_all_areas_inner(&self) -> CloudResult<LoadMapsSummary> {
        let auth_generation = self.backend.auth_generation();
        let list_start = Instant::now();
        let areas = self.backend.list_areas().await?;
        let list_duration = list_start.elapsed();

        let mut fetched_areas = HashMap::with_capacity(areas.len());
        let mut stats = Vec::with_capacity(areas.len());

        for area in areas {
            let load_start = Instant::now();
            match self.backend.get_area(&area.id).await {
                Ok(details) => {
                    let load_duration = load_start.elapsed();
                    let source = self.backend.last_area_source(&area.id);
                    // Classify from the list row (which carries the viewer-scoped access block
                    // and the owner handle); either signal flagging non-ownership means shared.
                    let shared =
                        area.owner_nickname.is_some() || area.access.is_some_and(|a| !a.is_owner);

                    stats.push(AreaLoadStat {
                        area_id: area.id,
                        name: details.area.name.clone(),
                        revision: details.area.rev,
                        load_duration,
                        source,
                        shared,
                    });

                    fetched_areas.insert(area.id, details);
                }
                Err(err) => {
                    warn!("Failed to load area {}: {err}", area.id);
                }
            }
        }

        if self.backend.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        // A composite list may intentionally omit the cloud tier when it is
        // offline. Resolve every still-unfetched delete intent with a point
        // read instead of treating list absence as proof of deletion.
        for area_id in self.pending.recovery_area_ids() {
            if fetched_areas.contains_key(&area_id) || !self.pending.has_delete_intent(area_id) {
                continue;
            }
            let cloud_intent = self.pending.delete_intent_is_cloud(area_id);
            let fetched = if cloud_intent {
                self.backend
                    .get_area_at_generation(&area_id, auth_generation)
                    .await
            } else {
                self.backend.get_area(&area_id).await
            };
            match fetched {
                Ok(details) => {
                    let shared = details.area.owner_nickname.is_some()
                        || details.area.access.is_some_and(|access| !access.is_owner);
                    stats.push(AreaLoadStat {
                        area_id,
                        name: details.area.name.clone(),
                        revision: details.area.rev,
                        load_duration: Duration::ZERO,
                        source: self.backend.last_area_source(&area_id),
                        shared,
                    });
                    fetched_areas.insert(area_id, details);
                }
                Err(
                    CloudError::NotFoundOrNoAccess
                    | CloudError::PermissionDenied(_)
                    | CloudError::AreaNotFound(_),
                ) => {
                    let discarded = self.pending.commit_recovered_delete(area_id)?;
                    self.account_deleted_pending(area_id, &discarded);
                }
                Err(CloudError::CredentialChanged) => {
                    return Err(CloudError::CredentialChanged);
                }
                Err(error) => {
                    warn!("Could not yet reconcile interrupted delete for area {area_id}: {error}");
                }
            }
        }
        // Adopt revisions and fold queued work under the same gate used by
        // mutation compilation. Entry scripts racing initial load can neither
        // lose an optimistic edit nor have its create/update meaning silently
        // changed by the fetched document.
        let _mutation_guard = self.mutation_gate.lock();
        if self.backend.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        let mut new_cache = HashMap::with_capacity(fetched_areas.len());
        for (area_id, details) in fetched_areas {
            if self.pending.has_delete_intent(area_id) {
                self.pending.abort_recovered_delete(area_id)?;
            }
            self.pending.note_confirmed_rev(
                area_id,
                details.area.rev,
                details.area.access.map(|access| access.fingerprint()),
            );
            let (details, failed) = self.fold_pending(area_id, &details, ReplayMode::StopAtFailure);
            if let Some(operation_id) = failed {
                self.pending.pause_conflict(area_id, operation_id);
            }
            self.pending.recovery_base_loaded(area_id);
            new_cache.insert(area_id, Arc::new(AreaCache::new_with_area(details)));
        }
        // Carry every exclusion axis across the wholesale rebuild.
        let new_cache = Arc::new(self.atlas_cache.load().rebuild_with_areas(new_cache));
        self.atlas_cache.store(new_cache);
        drop(_mutation_guard);

        // The wholesale store can race the sync engine (e.g. re-inserting an
        // area the engine removed between our list and store). Nudge the
        // engine: its next tick prunes anything the fresh row set no longer
        // covers, so any membership drift heals immediately.
        self.sync_notify.notify_one();

        Ok(LoadMapsSummary {
            list_duration,
            areas: stats,
        })
    }

    // === READ OPERATIONS (Instant, Lock-Free) ===

    #[must_use]
    pub fn get_current_atlas(&self) -> Arc<AtlasCache> {
        self.atlas_cache.load().clone()
    }

    /// Create a new area (waits for backend to assign ID)
    ///
    /// # Errors
    /// Returns an error if the backend rejects the request — auth/permission
    /// failures, a transport/HTTP error, or a server-side failure while
    /// creating the area.
    pub async fn create_area(&self, name: String) -> CloudResult<AreaId> {
        let atlas_id = Option::<&AtlasId>::cloned(self.atlas_id.load().as_ref().as_ref());
        self.create_area_in(name, atlas_id).await
    }

    /// Create a new area filed into an explicit atlas (or loose).
    ///
    /// # Errors
    /// Returns an error if the backend rejects the request — auth/permission
    /// failures, a transport/HTTP error, or a server-side failure while
    /// creating the area.
    pub async fn create_area_in(
        &self,
        name: String,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<AreaId> {
        let request = CreateAreaRequest {
            name,
            atlas_id,
            ephemeral: false,
        };
        self.create_area_from_request(request).await
    }

    /// Create an area in an explicit storage tier and optional atlas.
    pub async fn create_area_at(
        &self,
        name: String,
        destination: MapDestination,
    ) -> CloudResult<AreaId> {
        if destination.storage == MapStorage::Session && destination.atlas_id.is_some() {
            return Err(CloudError::InvalidInput(
                "session maps cannot be filed into atlases".to_string(),
            ));
        }
        let request = CreateAreaRequest {
            name,
            atlas_id: destination.atlas_id,
            ephemeral: destination.storage == MapStorage::Session,
        };
        self.create_area_from_request_at(request, destination.storage)
            .await
    }

    /// Create an area in the ephemeral tier (see [`Mapper::create_area_ephemeral`]).
    ///
    /// # Errors
    /// Propagates the backend's create error.
    pub async fn create_area_ephemeral(&self, name: String) -> CloudResult<AreaId> {
        self.create_area_at(name, MapDestination::loose(MapStorage::Session))
            .await
    }

    async fn create_area_from_request_at(
        &self,
        request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<AreaId> {
        let backend_area = self.backend.create_area_at(request, storage).await?;
        self.finish_created_area(backend_area)
    }

    async fn create_area_from_request(&self, request: CreateAreaRequest) -> CloudResult<AreaId> {
        // Create area on backend first to get the real ID
        let backend_area = self.backend.create_area(request).await?;
        self.finish_created_area(backend_area)
    }

    fn finish_created_area(&self, backend_area: Area) -> CloudResult<AreaId> {
        let area_id = backend_area.id;

        // The created row is backend truth for the new area's revision.
        self.pending.note_confirmed_rev(
            area_id,
            backend_area.rev,
            backend_area.access.map(|access| access.fingerprint()),
        );

        self.atlas_cache.rcu(|cache| {
            Arc::new(cache.add_area(
                area_id,
                Arc::new(AreaCache::new_with_area(AreaWithDetails {
                    area: backend_area.clone(),
                    format_version: crate::AREA_FORMAT_VERSION,
                    content_hash: None,
                    properties: vec![],
                    rooms: vec![],
                    labels: vec![],
                    shapes: vec![],
                    connections: vec![],
                    linked_areas: vec![],
                })),
            ))
        });

        Ok(area_id)
    }

    /// Import a set of full areas into the LOCAL tier in one shot — the JSON-import fast path.
    ///
    /// Each area is given a fresh `AreaId` (and fresh exit/label/shape ids), so an import never
    /// collides with an existing area or another import. Cross-area exit targets that point *within
    /// the imported set* are remapped to the new ids; any pointing outside it are dropped (there is
    /// nothing to link to). All viewer/cloud metadata is reset to a locally-owned area. Persistence
    /// is one `store_area` per area and the atlas cache is rebuilt once for the whole set, so
    /// importing N rooms is O(N) rather than the O(N^2) of a per-room/per-exit replay.
    ///
    /// # Errors
    /// Propagates the local backend's persistence error.
    pub async fn import_areas(&self, mut areas: Vec<AreaWithDetails>) -> CloudResult<Vec<AreaId>> {
        if areas.is_empty() {
            return Ok(Vec::new());
        }

        // §8.4: the whole import is validated before any write — one invalid
        // document rejects the batch (a v1 document migrated on the way in
        // passes by construction).
        for details in &areas {
            validate_import_document(details)?;
        }

        // Mint every new id first, so cross-area exits can be remapped in the pass below.
        let mut id_map: HashMap<AreaId, AreaId> = HashMap::with_capacity(areas.len());
        for details in &areas {
            id_map.insert(details.area.id, AreaId(Uuid::new_v4()));
        }

        for details in &mut areas {
            details.area.id = id_map[&details.area.id];
            details.area.rev = 1;
            details.area.user_id = None;
            details.area.atlas_id = None;
            details.area.access = Some(AreaAccess::OWNER);
            details.area.owner_nickname = None;
            details.area.copied_from_area_id = None;
            details.area.copied_from_rev = None;
            details.area.copied_at = None;
            details.area.family_token = None;
            details.content_hash = None;
            details.linked_areas.clear();

            for label in &mut details.labels {
                label.id = LabelId(Uuid::new_v4());
                label.is_secret = false;
            }
            for shape in &mut details.shapes {
                shape.id = ShapeId(Uuid::new_v4());
                shape.is_secret = false;
            }
            // Fresh Connection identities, keeping the exits' membership
            // references consistent (validated above, so the lookups below
            // cannot miss).
            let connection_map: HashMap<crate::ConnectionId, crate::ConnectionId> = details
                .connections
                .iter()
                .map(|connection| (connection.id, crate::ConnectionId::new()))
                .collect();
            for connection in &mut details.connections {
                connection.id = connection_map[&connection.id];
            }
            for room in &mut details.rooms {
                room.is_secret = false;
                for exit in &mut room.exits {
                    exit.id = ExitId(Uuid::new_v4());
                    exit.connection_id = connection_map[&exit.connection_id];
                    exit.is_secret = false;
                    exit.to_unknown = false;
                    exit.to_area_token = None;
                    exit.to_area_id = match exit.to_area_id {
                        Some(old) if id_map.contains_key(&old) => Some(id_map[&old]),
                        Some(_) => {
                            // Target is outside the imported set: drop the dangling cross-area link.
                            exit.to_room_number = None;
                            exit.to_direction = None;
                            None
                        }
                        None => None,
                    };
                }
            }
            // Dropped cross-area links (and cleared `to_unknown` markers)
            // can leave an External Connection with no member that still
            // leaves the area: it becomes Dangling, exactly as a live edit
            // would convert it.
            let leaves_area: HashSet<crate::ConnectionId> = details
                .rooms
                .iter()
                .flat_map(|room| room.exits.iter())
                .filter(|exit| exit.to_area_id.is_some_and(|to| to != details.area.id))
                .map(|exit| exit.connection_id)
                .collect();
            for connection in &mut details.connections {
                if connection.kind == crate::ConnectionKind::External
                    && !leaves_area.contains(&connection.id)
                {
                    connection.kind = crate::ConnectionKind::Dangling;
                }
            }
        }

        for details in &areas {
            self.backend.import_local_area(details.clone()).await?;
            // The stored document is backend truth for the imported area.
            self.pending.note_confirmed_rev(
                details.area.id,
                details.area.rev,
                details.area.access.map(|access| access.fingerprint()),
            );
        }

        // Rebuild the atlas once for the whole set (one index build per area, no per-op churn).
        self.atlas_cache.rcu(|cache| {
            let mut next = cache.add_area(
                areas[0].area.id,
                Arc::new(AreaCache::new_with_area(areas[0].clone())),
            );
            for details in areas.iter().skip(1) {
                next = next.add_area(
                    details.area.id,
                    Arc::new(AreaCache::new_with_area(details.clone())),
                );
            }
            Arc::new(next)
        });

        Ok(areas.iter().map(|details| details.area.id).collect())
    }

    /// Waits for the first [`Inner::load_all_areas`] to complete. Presence-checked imports gate on
    /// this so they never compare against a not-yet-populated atlas — package entry scripts run
    /// before the session loads its maps, and an empty atlas would make every name look absent.
    ///
    /// # Errors
    /// Errors when the initial load failed; a caller should skip its import rather than seed
    /// blind into an unknown atlas.
    async fn wait_for_initial_load(&self) -> CloudResult<()> {
        let mut gate = self.initial_load.subscribe();
        let outcome = *gate.wait_for(Option::is_some).await.map_err(|_| {
            crate::CloudError::InternalError(
                "mapper dropped before its initial area load".to_string(),
            )
        })?;
        if outcome == Some(true) {
            Ok(())
        } else {
            Err(crate::CloudError::InternalError(
                "initial area load failed; refusing a presence-checked import".to_string(),
            ))
        }
    }

    /// Presence-checked import — the offer-once seeding primitive. Imports (via
    /// [`Inner::import_areas`]) only the areas whose name no resident area already bears, and
    /// reports the rest as skipped.
    ///
    /// Three properties make this safe to call unconditionally, from any load order:
    /// - It waits for the initial area load, so an entry script's seed compares against the real
    ///   atlas rather than the empty pre-load cache.
    /// - The presence check reads the **unfiltered** resident set: shared, manually-disabled, and
    ///   per-server scope-excluded areas all count as present, so a map parked for another server
    ///   entry (or hidden from identification) is never re-imported as a duplicate.
    /// - Concurrent presence-checked imports serialize on one gate, so two packages seeding the
    ///   same name cannot both miss it and both import.
    ///
    /// # Errors
    /// Errors when the initial area load failed or the local backend's persistence fails.
    pub async fn import_areas_if_absent(
        &self,
        areas: Vec<AreaWithDetails>,
    ) -> CloudResult<AreasImportedIfAbsent> {
        self.wait_for_initial_load().await?;
        let _gate = self.import_gate.lock().await;
        let existing: HashSet<String> = self
            .get_current_atlas()
            .areas()
            .map(|area| area.get_name().to_string())
            .collect();
        let (missing, present): (Vec<_>, Vec<_>) = areas
            .into_iter()
            .partition(|details| !existing.contains(&details.area.name));
        let skipped = present
            .into_iter()
            .map(|details| details.area.name)
            .collect();
        let added = self.import_areas(missing).await?;
        Ok(AreasImportedIfAbsent { added, skipped })
    }

    /// Serialize an area to its full [`AreaWithDetails`] — the JSON-export path. The bytes are the
    /// viewer-scoped, secret-redacted projection the backend already holds, so this can only ever
    /// emit what the viewer can see; the `can_copy` gate is enforced by the caller.
    ///
    /// §8.4: the export is v2 with `connections` stably id-sorted (the
    /// server already serves them sorted; local documents are sorted here)
    /// so repeated exports diff meaningfully. Route points keep their stored
    /// order — it is the path.
    ///
    /// # Errors
    /// Propagates the backend's read error.
    pub async fn export_area(&self, area_id: AreaId) -> CloudResult<AreaWithDetails> {
        let cloud_area = !self.backend.local_area_ids().contains(&area_id)
            && !self.backend.ephemeral_area_ids().contains(&area_id);
        let mut details = if cloud_area {
            let auth_generation = self.backend.auth_generation();
            self.backend
                .get_area_at_generation(&area_id, auth_generation)
                .await?
        } else {
            self.backend.get_area(&area_id).await?
        };
        details.connections.sort_by_key(|connection| connection.id);
        Ok(details)
    }

    /// The viewer's effective access for an area (owner-level for local/legacy areas), or `None`
    /// if the area isn't in the current atlas — used to gate export on `can_copy`.
    #[must_use]
    pub fn area_effective_access(&self, area_id: AreaId) -> Option<AreaAccess> {
        self.atlas_cache
            .load()
            .get_area(&area_id)
            .map(|area| area.effective_access())
    }

    fn begin_area_move(&self, area_ids: &[AreaId]) -> CloudResult<Vec<AreaMoveFence>> {
        let _mutation_guard = self.mutation_gate.lock();
        let metadata = self.metadata_writes_by_area.lock();
        if let Some(area_id) = area_ids
            .iter()
            .find(|area_id| metadata.contains_key(area_id))
        {
            return Err(CloudError::PendingOperations(format!(
                "map {area_id} is still being renamed or filed; retry the move when it finishes"
            )));
        }
        drop(metadata);

        // A queue paused for review holds edits the backend has not accepted.
        // Moving such an area would snapshot the optimistic view and delete
        // the source, silently resolving the pause as "keep mine" against
        // whatever the other side holds — the user must resolve it first.
        if let Some(area_id) = area_ids.iter().find(|area_id| {
            matches!(
                self.pending.save_status(**area_id),
                AreaSaveStatus::ConflictNeedsReview | AreaSaveStatus::CouldNotSave { .. }
            )
        }) {
            return Err(CloudError::PendingOperations(format!(
                "map {area_id} has edits awaiting conflict or failure review; resolve them before moving the map"
            )));
        }

        let mut fences = Vec::with_capacity(area_ids.len());
        for &area_id in area_ids {
            fences.push(AreaMoveFence {
                area_id,
                delete_fence: Some(AreaDeleteFence::begin(area_id, self.pending.clone())?),
            });
        }
        Ok(fences)
    }

    async fn delete_area_and_wait(&self, area_id: AreaId) -> CloudResult<()> {
        let auth_generation = self.metadata_auth_generation(area_id);
        let mut fences = self.begin_area_move(&[area_id])?;
        let delete_fence = fences
            .pop()
            .expect("one requested delete produces one fence")
            .into_delete_fence();
        self.delete_area_with_fence_at_generation(area_id, delete_fence, auth_generation, None)
            .await
    }

    async fn delete_area_with_fence(
        &self,
        area_id: AreaId,
        delete_fence: AreaDeleteFence,
        expected_rev: Option<i64>,
    ) -> CloudResult<()> {
        let auth_generation = self.metadata_auth_generation(area_id);
        self.delete_area_with_fence_at_generation(
            area_id,
            delete_fence,
            auth_generation,
            expected_rev,
        )
        .await
    }

    async fn delete_area_with_fence_at_generation(
        &self,
        area_id: AreaId,
        mut delete_fence: AreaDeleteFence,
        auth_generation: Option<u64>,
        expected_rev: Option<i64>,
    ) -> CloudResult<()> {
        self.pending.wait_until_delete_quiescent(area_id).await;
        if let Some(expected_rev) = expected_rev {
            // Compare-then-delete: the backend's DELETE carries no revision
            // precondition, so the strongest available guard is re-reading the
            // authoritative revision immediately before deleting and refusing
            // on drift. Returning before `prepare()` drops the still-armed
            // fence, which aborts the delete intent and reopens the source. A
            // TOCTOU window remains between this read and the DELETE below;
            // closing it needs a server-side expected-rev delete precondition.
            let current = if let Some(auth_generation) = auth_generation {
                self.backend
                    .get_area_at_generation(&area_id, auth_generation)
                    .await
            } else {
                self.backend.get_area(&area_id).await
            };
            match current {
                Ok(details) if details.area.rev != expected_rev => {
                    return Err(CloudError::RevisionConflict {
                        id: area_id.0,
                        expected_rev,
                        current_rev: details.area.rev,
                    });
                }
                Ok(_) => {}
                // Already gone (or access revoked): the DELETE below reports
                // the authoritative outcome through the existing path.
                Err(CloudError::NotFoundOrNoAccess) => {}
                // The revision could not be verified; refuse rather than
                // delete blind. Both sides keep a complete copy.
                Err(error) => return Err(error),
            }
        }
        delete_fence.prepare()?;
        delete_fence.request_started();
        let tracking = AcknowledgedWrite::new(
            area_id,
            self.pending_by_area.clone(),
            self.sync_stats.clone(),
        );
        let result = if let Some(auth_generation) = auth_generation {
            self.backend
                .delete_area_at_generation(&area_id, auth_generation)
                .await
        } else {
            self.backend.delete_area(&area_id).await
        };
        if let Err(error) = result {
            tracking.settle(false);
            // A transport error or cancelled response is not proof that the
            // server did not commit the DELETE. Keep the durable intent frozen
            // and let the sync engine resolve it with a point GET.
            delete_fence.reconcile();
            self.sync_notify.notify_one();
            return Err(error);
        }
        let discarded = match delete_fence.commit() {
            Ok(discarded) => discarded,
            Err(error) => {
                self.sync_notify.notify_one();
                return Err(error);
            }
        };
        self.account_deleted_pending(area_id, &discarded);
        tracking.settle(true);
        if auth_generation.is_some_and(|generation| self.backend.auth_generation() != generation) {
            return Ok(());
        }
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |_area| Arc::new(cache.delete_area(area_id)),
            )
        });
        Ok(())
    }

    async fn rename_area_and_wait(&self, area_id: AreaId, name: &str) -> CloudResult<()> {
        let auth_generation = self.metadata_auth_generation(area_id);
        let updates = AreaUpdates {
            name: Some(name.to_string()),
            atlas_id: None,
        };
        let tracking = {
            let _mutation_guard = self.mutation_gate.lock();
            if self.pending.is_delete_fenced(area_id) {
                return Err(CloudError::PendingOperations(
                    "this map is being moved or deleted".to_string(),
                ));
            }
            AcknowledgedWrite::new_metadata(
                area_id,
                self.pending_by_area.clone(),
                self.metadata_writes_by_area.clone(),
                self.sync_stats.clone(),
            )
        };
        let result = if let Some(auth_generation) = auth_generation {
            self.backend
                .update_area_at_generation(&area_id, updates, auth_generation)
                .await
        } else {
            self.backend.update_area(&area_id, updates).await
        };
        if let Err(error) = result {
            tracking.settle(false);
            return Err(error);
        }
        if auth_generation.is_some_and(|generation| self.backend.auth_generation() != generation) {
            tracking.settle(true);
            return Ok(());
        }
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| Arc::new(cache.insert_area(area_id, Arc::new(area.rename(name)))),
            )
        });
        tracking.settle(true);
        Ok(())
    }

    async fn move_area_to_atlas_and_wait(
        &self,
        area_id: AreaId,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<()> {
        let auth_generation = self.metadata_auth_generation(area_id);
        let tracking = {
            let _mutation_guard = self.mutation_gate.lock();
            if self.pending.is_delete_fenced(area_id) {
                return Err(CloudError::PendingOperations(
                    "this map is being moved or deleted".to_string(),
                ));
            }
            AcknowledgedWrite::new_metadata(
                area_id,
                self.pending_by_area.clone(),
                self.metadata_writes_by_area.clone(),
                self.sync_stats.clone(),
            )
        };
        let result = if let Some(auth_generation) = auth_generation {
            self.backend
                .move_area_to_atlas_at_generation(&area_id, atlas_id, auth_generation)
                .await
        } else {
            self.backend.move_area_to_atlas(&area_id, atlas_id).await
        };
        if let Err(error) = result {
            tracking.settle(false);
            return Err(error);
        }
        if auth_generation.is_some_and(|generation| self.backend.auth_generation() != generation) {
            tracking.settle(true);
            return Ok(());
        }
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| Arc::new(cache.insert_area(area_id, Arc::new(area.with_atlas(atlas_id)))),
            )
        });
        tracking.settle(true);
        Ok(())
    }

    fn metadata_auth_generation(&self, area_id: AreaId) -> Option<u64> {
        (!self.backend.local_area_ids().contains(&area_id)
            && !self.backend.ephemeral_area_ids().contains(&area_id))
        .then(|| self.backend.auth_generation())
    }

    pub fn set_area_property(
        &self,
        area_id: AreaId,
        name: String,
        value: String,
    ) -> CloudResult<MutationSubmission> {
        let description = format!("Set area property {name}");
        self.mutate_area(
            area_id,
            vec![AreaMutation::UpsertAreaProperty {
                name,
                value,
                is_secret: None,
            }],
            description,
            PairedExitPolicy::Reject,
        )
    }

    pub fn delete_area_property(
        &self,
        area_id: AreaId,
        name: String,
    ) -> CloudResult<MutationSubmission> {
        let description = format!("Delete area property {name}");
        self.mutate_area(
            area_id,
            vec![AreaMutation::DeleteAreaProperty { name }],
            description,
            PairedExitPolicy::Reject,
        )
    }

    /// Refuses room *creation* into an ephemeral area once the tier holds
    /// [`EPHEMERAL_ROOM_CAP`] rooms (updates to existing rooms always pass).
    /// The check runs before the optimistic cache write — the cache is where
    /// the memory lives, so a backend-side refusal would come too late.
    fn over_ephemeral_cap(&self, area_id: AreaId, new_rooms: &[RoomNumber]) -> bool {
        let ephemeral_ids = self.backend.ephemeral_area_ids();
        if !ephemeral_ids.contains(&area_id) {
            return false;
        }
        let cache = self.atlas_cache.load();
        let creating = cache.get_area(&area_id).map_or(new_rooms.len(), |area| {
            new_rooms
                .iter()
                .filter(|number| area.get_room(number).is_none())
                .count()
        });
        if creating == 0 {
            return false;
        }
        let total: usize = ephemeral_ids
            .iter()
            .filter_map(|id| cache.get_area(id))
            .map(|area| area.room_count())
            .sum();
        let over = total + creating > EPHEMERAL_ROOM_CAP;
        if over && !self.ephemeral_cap_warned.swap(true, Ordering::Relaxed) {
            warn!(
                "ephemeral map tier is at its {EPHEMERAL_ROOM_CAP}-room cap; \
                 further auto-mapped rooms are dropped (save and clear the session map to continue)"
            );
        }
        over
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn upsert_room(
        &self,
        room_key: RoomKey,
        updates: RoomUpdates,
    ) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        if self.over_ephemeral_cap(area_id, std::slice::from_ref(&room_number)) {
            return Err(CloudError::PendingOperations(format!(
                "ephemeral map tier is limited to {EPHEMERAL_ROOM_CAP} rooms"
            )));
        }
        let description = format!("Update room {room_number}");
        self.mutate_area(
            area_id,
            vec![AreaMutation::UpsertRoom {
                room_number,
                body: updates,
            }],
            description,
            PairedExitPolicy::Reject,
        )
    }

    pub fn upsert_rooms(
        &self,
        area_id: AreaId,
        updates: Vec<(RoomNumber, RoomUpdates)>,
    ) -> CloudResult<Vec<MutationSubmission>> {
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        let numbers: Vec<RoomNumber> = updates.iter().map(|(number, _)| *number).collect();
        if self.over_ephemeral_cap(area_id, &numbers) {
            return Err(CloudError::PendingOperations(format!(
                "ephemeral map tier is limited to {EPHEMERAL_ROOM_CAP} rooms"
            )));
        }

        // The batch rides one envelope, so it lands (and any conflict
        // review treats it) atomically — up to the server-enforced
        // per-envelope operation cap. An oversized batch must split before
        // enqueue (the server rejects it outright); the chunks land in
        // order on the area's queue, so atomicity becomes per-chunk.
        let description = if updates.len() == 1 {
            format!("Update room {}", updates[0].0)
        } else {
            format!("Update {} rooms", updates.len())
        };
        let mut ops: Vec<AreaMutation> = updates
            .into_iter()
            .map(|(room_number, body)| AreaMutation::UpsertRoom { room_number, body })
            .collect();
        let mut batches = Vec::new();
        while ops.len() > MAX_MUTATION_OPERATIONS {
            let rest = ops.split_off(MAX_MUTATION_OPERATIONS);
            batches.push(AreaMutationBatch::strict(area_id, ops, description.clone()));
            ops = rest;
        }
        batches.push(AreaMutationBatch::strict(area_id, ops, description));
        self.mutate_batches(batches)
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn delete_room(&self, room_key: RoomKey) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let description = format!("Delete room {room_number}");
        let result = self.mutate_area(
            area_id,
            vec![AreaMutation::DeleteRoom { room_number }],
            description.clone(),
            PairedExitPolicy::Reject,
        )?;

        // The server deletion also clears inbound links in other aggregates.
        // Mirror that cascade only after the source deletion's durable commit
        // point, so a reset can never leave an unjournaled optimistic edit.
        let target = RoomKey::new(area_id, room_number);
        self.atlas_cache.rcu(|cache| {
            let updated: Vec<_> = cache
                .areas()
                .filter(|area| *area.get_id() != area_id)
                .filter_map(|area| {
                    area.null_inbound_exits(&target)
                        .map(|updated| (*area.get_id(), Arc::new(updated)))
                })
                .collect();
            if updated.is_empty() {
                cache.clone()
            } else {
                Arc::new(cache.with_areas_updated(updated))
            }
        });
        Ok(result)
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn set_room_property(
        &self,
        room_key: RoomKey,
        name: String,
        value: String,
    ) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let description = format!("Set property {name} on room {room_number}");
        self.mutate_area(
            area_id,
            vec![AreaMutation::UpsertRoomProperty {
                room_number,
                name,
                value,
                is_secret: None,
            }],
            description,
            PairedExitPolicy::Reject,
        )
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn delete_room_property(
        &self,
        room_key: RoomKey,
        name: String,
    ) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let description = format!("Delete property {name} on room {room_number}");
        self.mutate_area(
            area_id,
            vec![AreaMutation::DeleteRoomProperty { room_number, name }],
            description,
            PairedExitPolicy::Reject,
        )
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn add_room_tag(&self, room_key: RoomKey, tag: String) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let tag = normalize_tag(&tag);
        if tag.is_empty() {
            return Ok(MutationSubmission::NoChange);
        }
        let description = format!("Add tag {tag} to room {room_number}");
        self.mutate_area(
            area_id,
            vec![AreaMutation::AddRoomTag { room_number, tag }],
            description,
            PairedExitPolicy::Reject,
        )
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn remove_room_tag(
        &self,
        room_key: RoomKey,
        tag: String,
    ) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let tag = normalize_tag(&tag);
        if tag.is_empty() {
            return Ok(MutationSubmission::NoChange);
        }
        let description = format!("Remove tag {tag} from room {room_number}");
        self.mutate_area(
            area_id,
            vec![AreaMutation::RemoveRoomTag { room_number, tag }],
            description,
            PairedExitPolicy::Reject,
        )
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    fn update_exit(
        &self,
        room_key: RoomKey,
        exit_id: ExitId,
        updates: ExitUpdates,
        paired_policy: PairedExitPolicy,
    ) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let cache = self.atlas_cache.load();
        let area = cache
            .get_area(&area_id)
            .ok_or(CloudError::AreaNotFound(area_id))?;
        let room = area
            .get_room(&room_number)
            .ok_or_else(|| CloudError::RoomNotFound(RoomKey::new(area_id, room_number)))?;
        if !room.get_exits().iter().any(|exit| exit.id == exit_id) {
            return Err(CloudError::ExitNotFound(exit_id));
        }
        drop(cache);

        self.mutate_area(
            area_id,
            vec![AreaMutation::UpdateExit {
                exit_id,
                body: updates,
            }],
            "Update exit".to_string(),
            paired_policy,
        )
    }

    fn merge_rooms(
        &self,
        area_id: AreaId,
        keep_room_number: RoomNumber,
        remove_room_number: RoomNumber,
    ) -> CloudResult<MutationSubmission> {
        let _mutation_guard = self.mutation_gate.lock();
        let cache = self.atlas_cache.load();
        let area = cache
            .get_area(&area_id)
            .ok_or(CloudError::AreaNotFound(area_id))?;
        if !area.effective_access().is_cleared_for_secrets() {
            return Err(CloudError::StructuralConflict(
                "merge_requires_full_projection".to_string(),
            ));
        }
        let remove_room = area
            .get_room(&remove_room_number)
            .ok_or_else(|| CloudError::RoomNotFound(RoomKey::new(area_id, remove_room_number)))?;
        let outbound_foreign = remove_room
            .get_exits()
            .iter()
            .any(|exit| exit.to_area_id.is_some_and(|target| target != area_id));
        let inbound_foreign = cache
            .areas()
            .filter(|candidate| *candidate.get_id() != area_id)
            .any(|candidate| {
                candidate.get_rooms().iter().any(|room| {
                    room.get_exits().iter().any(|exit| {
                        exit.to_area_id == Some(area_id)
                            && exit.to_room_number == Some(remove_room_number)
                    })
                })
            });
        if outbound_foreign || inbound_foreign {
            return Err(CloudError::StructuralConflict(
                "merge_cross_area_links".to_string(),
            ));
        }
        let operations =
            merge_room_operations(&area.to_details(), keep_room_number, remove_room_number)?;
        drop(cache);
        self.mutate_area_locked(
            area_id,
            operations,
            format!("Merge room {remove_room_number} into room {keep_room_number}"),
            PairedExitPolicy::Split,
        )
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn delete_exit(
        &self,
        room_key: RoomKey,
        exit_id: ExitId,
    ) -> CloudResult<MutationSubmission> {
        let RoomKey {
            area_id,
            room_number: _,
        } = room_key;
        self.mutate_area(
            area_id,
            vec![AreaMutation::DeleteExit { exit_id }],
            "Delete exit".to_string(),
            PairedExitPolicy::Reject,
        )
    }
    // === CREATE OPERATIONS (Client-Minted Ids) ===

    /// Create an exit. The id is client-minted before enqueue, the cache
    /// updates optimistically, and the envelope queues for the backend —
    /// there is no round-trip to wait for.
    ///
    /// # Errors
    /// Infallible today; the result type is retained so call sites keep
    /// handling a backend verdict where one used to surface.
    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn create_exit(&self, room_key: RoomKey, args: ExitArgs) -> CloudResult<ExitId> {
        self.create_exit_tracked(room_key, args)
            .map(|(exit_id, _submission)| exit_id)
    }

    fn create_exit_tracked(
        &self,
        room_key: RoomKey,
        mut args: ExitArgs,
    ) -> CloudResult<(ExitId, MutationSubmission)> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let exit_id = args.id.unwrap_or_else(|| ExitId(Uuid::new_v4()));
        args.id = Some(exit_id);
        let description = format!(
            "Create exit {} from room {room_number}",
            args.from_direction
        );
        let submission = self.mutate_area(
            area_id,
            vec![AreaMutation::CreateExit {
                room_number,
                body: args,
            }],
            description,
            PairedExitPolicy::Reject,
        )?;

        Ok((exit_id, submission))
    }

    /// Create a label with a client-minted id; see [`Self::create_exit`].
    ///
    /// # Errors
    /// Infallible today; the result type is retained so call sites keep
    /// handling a backend verdict where one used to surface.
    pub fn create_label(&self, area_id: AreaId, args: LabelArgs) -> CloudResult<LabelId> {
        self.create_label_tracked(area_id, args)
            .map(|(label_id, _submission)| label_id)
    }

    fn create_label_tracked(
        &self,
        area_id: AreaId,
        mut args: LabelArgs,
    ) -> CloudResult<(LabelId, MutationSubmission)> {
        let label_id = args.id.unwrap_or_else(|| LabelId(Uuid::new_v4()));
        args.id = Some(label_id);
        let submission = self.mutate_area(
            area_id,
            vec![AreaMutation::CreateLabel { body: args }],
            "Create label".to_string(),
            PairedExitPolicy::Reject,
        )?;

        Ok((label_id, submission))
    }

    /// Create a shape with a client-minted id; see [`Self::create_exit`].
    ///
    /// # Errors
    /// Infallible today; the result type is retained so call sites keep
    /// handling a backend verdict where one used to surface.
    pub fn create_shape(&self, area_id: AreaId, args: ShapeArgs) -> CloudResult<ShapeId> {
        self.create_shape_tracked(area_id, args)
            .map(|(shape_id, _submission)| shape_id)
    }

    fn create_shape_tracked(
        &self,
        area_id: AreaId,
        mut args: ShapeArgs,
    ) -> CloudResult<(ShapeId, MutationSubmission)> {
        let shape_id = args.id.unwrap_or_else(|| ShapeId(Uuid::new_v4()));
        args.id = Some(shape_id);
        let submission = self.mutate_area(
            area_id,
            vec![AreaMutation::CreateShape { body: args }],
            "Create shape".to_string(),
            PairedExitPolicy::Reject,
        )?;

        Ok((shape_id, submission))
    }

    pub fn update_label(
        &self,
        area_id: AreaId,
        label_id: LabelId,
        updates: LabelUpdates,
    ) -> CloudResult<MutationSubmission> {
        self.mutate_area(
            area_id,
            vec![AreaMutation::UpdateLabel {
                label_id,
                body: updates,
            }],
            "Update label".to_string(),
            PairedExitPolicy::Reject,
        )
    }

    pub fn delete_label(
        &self,
        area_id: AreaId,
        label_id: LabelId,
    ) -> CloudResult<MutationSubmission> {
        self.mutate_area(
            area_id,
            vec![AreaMutation::DeleteLabel { label_id }],
            "Delete label".to_string(),
            PairedExitPolicy::Reject,
        )
    }

    pub fn update_shape(
        &self,
        area_id: AreaId,
        shape_id: ShapeId,
        updates: ShapeUpdates,
    ) -> CloudResult<MutationSubmission> {
        self.mutate_area(
            area_id,
            vec![AreaMutation::UpdateShape {
                shape_id,
                body: updates,
            }],
            "Update shape".to_string(),
            PairedExitPolicy::Reject,
        )
    }

    pub fn delete_shape(
        &self,
        area_id: AreaId,
        shape_id: ShapeId,
    ) -> CloudResult<MutationSubmission> {
        self.mutate_area(
            area_id,
            vec![AreaMutation::DeleteShape { shape_id }],
            "Delete shape".to_string(),
            PairedExitPolicy::Reject,
        )
    }

    pub fn get_selected_atlas_id(&self) -> Option<Uuid> {
        None
    }

    // === INTERNAL SYNC HELPERS ===

    /// Drops one in-flight write marker for `area_id`, removing the entry
    /// once the count reaches zero.
    fn decrement_pending(pending_by_area: &Mutex<HashMap<AreaId, u64>>, area_id: AreaId) {
        let mut pending = pending_by_area.lock();
        if let Some(count) = pending.get_mut(&area_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pending.remove(&area_id);
            }
        }
    }

    pub(crate) fn account_deleted_pending(&self, area_id: AreaId, discarded: &[PendingEnvelope]) {
        let mut accounted = self.accounted_operations.lock();
        let discarded_count = discarded
            .iter()
            .filter(|envelope| accounted.remove(&envelope.operation_id))
            .count() as u64;
        drop(accounted);
        if discarded_count == 0 {
            return;
        }
        self.sync_stats
            .operations_failed
            .fetch_add(discarded_count, Ordering::Relaxed);
        for _ in 0..discarded_count {
            Self::decrement_pending(&self.pending_by_area, area_id);
        }
    }

    // === CAS PENDING QUEUE ===

    /// The compound counterpart to the ergonomic single-operation helpers.
    /// The shared document applier gives the live cache exactly the same
    /// all-or-nothing semantics as local/ephemeral backends and conflict
    /// replay; only after it succeeds is the envelope made visible to the
    /// pending worker.
    fn mutate_area(
        &self,
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: String,
        paired_policy: PairedExitPolicy,
    ) -> CloudResult<MutationSubmission> {
        let _mutation_guard = self.mutation_gate.lock();
        self.mutate_area_locked(area_id, operations, description, paired_policy)
    }

    fn mutate_batches(
        &self,
        batches: Vec<AreaMutationBatch>,
    ) -> CloudResult<Vec<MutationSubmission>> {
        let _mutation_guard = self.mutation_gate.lock();
        self.mutate_batches_locked(batches)
    }

    fn mutate_area_locked(
        &self,
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: String,
        paired_policy: PairedExitPolicy,
    ) -> CloudResult<MutationSubmission> {
        let mut submissions = self.mutate_batches_locked(vec![AreaMutationBatch {
            area_id,
            operations,
            description,
            paired_policy,
        }])?;
        Ok(submissions.pop().unwrap_or(MutationSubmission::NoChange))
    }

    fn mutate_batches_locked(
        &self,
        batches: Vec<AreaMutationBatch>,
    ) -> CloudResult<Vec<MutationSubmission>> {
        let cache = self.atlas_cache.load_full();
        let mut working = HashMap::<AreaId, AreaWithDetails>::new();
        let mut staged = Vec::<(AreaId, PendingEnvelope)>::new();
        let mut submissions = Vec::with_capacity(batches.len());
        let mut deleted_rooms = Vec::<RoomKey>::new();

        for batch in batches {
            let AreaMutationBatch {
                area_id,
                operations,
                description,
                paired_policy,
            } = batch;
            if operations.is_empty() {
                return Err(CloudError::InvalidInput(
                    "a mutation must contain at least one operation".to_string(),
                ));
            }
            if operations.len() > MAX_MUTATION_OPERATIONS {
                return Err(CloudError::InvalidInput(format!(
                    "a mutation may contain at most {MAX_MUTATION_OPERATIONS} operations"
                )));
            }

            let mut details = if let Some(details) = working.get(&area_id) {
                details.clone()
            } else {
                cache
                    .get_area(&area_id)
                    .ok_or(CloudError::AreaNotFound(area_id))?
                    .to_details()
            };
            let operations = compile_area_mutations(&details, operations, paired_policy)?;
            if operations.len() > MAX_MUTATION_OPERATIONS {
                return Err(CloudError::InvalidInput(format!(
                    "the compiled mutation may contain at most {MAX_MUTATION_OPERATIONS} operations"
                )));
            }
            if operations.is_empty() {
                submissions.push(MutationSubmission::NoChange);
                continue;
            }
            deleted_rooms.extend(operations.iter().filter_map(|operation| {
                if let AreaMutation::DeleteRoom { room_number } = operation {
                    Some(RoomKey::new(area_id, *room_number))
                } else {
                    None
                }
            }));

            let existing_rooms: HashSet<_> =
                details.rooms.iter().map(|room| room.room_number).collect();
            let structural_preconditions = operations
                .iter()
                .filter_map(|operation| match operation {
                    AreaMutation::UpsertRoom { room_number, .. } => {
                        Some(if existing_rooms.contains(room_number) {
                            StructuralPrecondition::RoomPresent(*room_number)
                        } else {
                            StructuralPrecondition::RoomAbsent(*room_number)
                        })
                    }
                    _ => None,
                })
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            let operation_id = Uuid::new_v4();
            let validation_envelope = MutationEnvelope {
                operation_id,
                preconditions: vec![Precondition {
                    resource: ResourceKind::Area,
                    id: area_id.0,
                    expected_rev: details.area.rev,
                    access_fingerprint: details.area.access.map(|access| access.fingerprint()),
                }],
                payload: operations.clone(),
            };
            area_edits::apply_envelope(&mut details, area_id, &validation_envelope)?;

            let local_area = self.backend.local_area_ids().contains(&area_id);
            let ephemeral_area = self.backend.ephemeral_area_ids().contains(&area_id);
            let non_cloud = !self.backend.supports_sync() || local_area || ephemeral_area;
            let local_durable = local_area || (!self.backend.supports_sync() && !ephemeral_area);
            let (viewer_id, auth_generation) = if non_cloud {
                (None, self.backend.auth_generation())
            } else {
                let (viewer_id, auth_generation) =
                    self.pending.active_viewer().ok_or_else(|| {
                        CloudError::PendingOperations(
                            "cloud map identity is not ready for this edit".to_string(),
                        )
                    })?;
                (Some(viewer_id), auth_generation)
            };

            staged.push((
                area_id,
                PendingEnvelope {
                    operation_id,
                    ops: operations,
                    description,
                    structural_preconditions,
                    attempts: 0,
                    viewer_id,
                    local_durable,
                    auth_generation,
                    sequence: 0,
                    queued_at: chrono::Utc::now(),
                    journal_path: None,
                    receipt_expired: false,
                    published: false,
                    journal_batch_id: None,
                    delete_intent: false,
                },
            ));
            working.insert(area_id, details);
            submissions.push(MutationSubmission::Queued(operation_id));
        }

        let queued: Vec<_> = staged
            .iter()
            .map(|(area_id, envelope)| (*area_id, envelope.operation_id))
            .collect();
        let publication = self.pending.enqueue_many_staged(staged)?;
        if !working.is_empty() {
            let updates = working
                .into_iter()
                .map(|(area_id, details)| (area_id, Arc::new(AreaCache::new_with_area(details))))
                .collect::<Vec<_>>();
            self.atlas_cache.rcu(|latest| {
                let mut next = latest.with_areas_updated(updates.clone());
                for target in &deleted_rooms {
                    let inbound_updates: Vec<_> = next
                        .areas()
                        .filter_map(|area| {
                            area.null_inbound_exits(target)
                                .map(|updated| (*area.get_id(), Arc::new(updated)))
                        })
                        .collect();
                    if !inbound_updates.is_empty() {
                        next = next.with_areas_updated(inbound_updates);
                    }
                }
                Arc::new(next)
            });
        }
        let mut accounted = self.accounted_operations.lock();
        let mut pending_by_area = self.pending_by_area.lock();
        for (area_id, operation_id) in &queued {
            self.sync_stats
                .operations_sent
                .fetch_add(1, Ordering::Relaxed);
            accounted.insert(*operation_id);
            *pending_by_area.entry(*area_id).or_insert(0) += 1;
        }
        drop(accounted);
        drop(pending_by_area);
        self.pending.publish_staged(publication);
        Ok(submissions)
    }

    /// Spawns the pending-queue worker: one task draining ready envelopes
    /// across all areas, sleeping until the store wakes it or the earliest
    /// backoff deadline passes. Holds only a weak reference to the mapper
    /// internals and exits once they are dropped.
    fn spawn_mutation_worker(inner: &Arc<Self>) {
        let weak = Arc::downgrade(inner);
        let pending = inner.pending.clone();
        tokio::spawn(async move {
            loop {
                let deadline = {
                    let Some(inner) = weak.upgrade() else { break };
                    let (retired, retirement_deadline) =
                        inner.pending.retry_ready_retirement(Instant::now());
                    if let Some((area_id, operation_id)) = retired {
                        if inner.accounted_operations.lock().remove(&operation_id) {
                            inner
                                .sync_stats
                                .operations_succeeded
                                .fetch_add(1, Ordering::Relaxed);
                            Self::decrement_pending(&inner.pending_by_area, area_id);
                        }
                        continue;
                    }
                    let (ready, deadline) = inner.pending.take_ready(Instant::now());
                    if let Some((area_id, envelope, rev, fingerprint)) = ready {
                        inner
                            .dispatch_envelope(area_id, envelope, rev, fingerprint)
                            .await;
                        continue;
                    }
                    match (deadline, retirement_deadline) {
                        (Some(left), Some(right)) => Some(left.min(right)),
                        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
                        (None, None) => None,
                    }
                };
                // Idle: a store wake (enqueue, acknowledge, resolve) or the
                // earliest backoff deadline resumes the loop.
                if let Some(deadline) = deadline {
                    let _ = tokio::time::timeout_at(
                        tokio::time::Instant::from_std(deadline),
                        pending.notify.notified(),
                    )
                    .await;
                } else {
                    pending.notify.notified().await;
                }
            }
        });
    }

    /// Sends one envelope to the backend and routes the verdict back into
    /// the pending store and the sync stats.
    async fn dispatch_envelope(
        &self,
        area_id: AreaId,
        envelope: PendingEnvelope,
        confirmed_rev: Option<i64>,
        fingerprint: Option<String>,
    ) {
        // The precondition rides the last backend-confirmed revision; until
        // any backend truth lands, the cached revision stands in. The server
        // requires the access fingerprint on every area precondition, so an
        // unrecorded fingerprint falls back to the cached area's access
        // block; local/ephemeral areas have no access block and their
        // backends ignore the field.
        let (expected_rev, fingerprint) = {
            let cache = self.atlas_cache.load();
            let area = cache.get_area(&area_id);
            (
                confirmed_rev.unwrap_or_else(|| area.as_ref().map_or(0, |a| a.get_rev())),
                fingerprint.or_else(|| area.map(|a| a.effective_access().fingerprint())),
            )
        };
        let operation_id = envelope.operation_id;
        let viewer_id = envelope.viewer_id;
        let auth_generation = envelope.auth_generation;
        let wire = MutationEnvelope {
            operation_id,
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: area_id.0,
                expected_rev,
                access_fingerprint: fingerprint,
            }],
            payload: envelope.ops,
        };
        let result = if viewer_id.is_some() {
            self.backend
                .execute_mutation_at_generation(&area_id, &wire, auth_generation)
                .await
        } else {
            self.backend.execute_mutation(&area_id, &wire).await
        };
        // Identity activation can remove this in-flight queue while the
        // captured-credential request is still on the wire. A late verdict
        // must never settle or park the newly active viewer's queue.
        if !self.pending.is_in_flight_at_generation(
            area_id,
            operation_id,
            viewer_id,
            auth_generation,
        ) {
            return;
        }
        match result {
            Ok(result) => {
                let own_rev = result
                    .versions
                    .iter()
                    .find(|version| {
                        version.resource == ResourceKind::Area && version.id == area_id.0
                    })
                    .map(|version| version.rev);
                // Settle the receipt under the same gate used by sync
                // refetch adoption. A GET that began before this ACK can
                // therefore observe the new confirmed revision and reject
                // its stale body instead of replacing the optimistic cache.
                let (acknowledged, foreign_versions) = {
                    let _mutation_guard = self.mutation_gate.lock();
                    if !self.pending.is_in_flight_at_generation(
                        area_id,
                        operation_id,
                        viewer_id,
                        auth_generation,
                    ) {
                        return;
                    }
                    let acknowledged = self.pending.acknowledge(area_id, operation_id, own_rev);
                    let mut foreign_versions = false;
                    for version in &result.versions {
                        if version.resource == ResourceKind::Area && version.id != area_id.0 {
                            self.pending
                                .note_confirmed_rev(AreaId(version.id), version.rev, None);
                            foreign_versions = true;
                        }
                    }
                    (acknowledged, foreign_versions)
                };
                if acknowledged {
                    self.sync_stats
                        .operations_succeeded
                        .fetch_add(1, Ordering::Relaxed);
                    Self::decrement_pending(&self.pending_by_area, area_id);
                }

                // A compound mutation can move aggregates beyond its own
                // area (cross-area cascades). Record their confirmed
                // revisions and nudge the sync engine to refetch the
                // affected projections.
                if foreign_versions {
                    self.sync_notify.notify_one();
                }
            }
            Err(CloudError::RevisionConflict { .. } | CloudError::ProjectionChanged { .. }) => {
                self.reconcile_conflict(area_id, operation_id, viewer_id, auth_generation)
                    .await;
            }
            Err(CloudError::UpgradeRequired) => {
                // Pause every cloud queue without discarding anything, and
                // surface the terminal state through the sync engine's
                // status so the UI opens the upgrade path.
                self.pending.pause_for_upgrade();
                sync_engine::set_status(
                    self,
                    SyncState::UpgradeRequired,
                    Some(CloudError::UpgradeRequired.to_string()),
                );
            }
            Err(CloudError::CredentialChanged) => {
                // This request was never sent: the backend could not produce
                // the exact credential snapshot captured at activation. Keep
                // the durable record and wait for the identity tick to move
                // it into the newly authenticated viewer namespace.
                self.pending
                    .credential_changed(area_id, operation_id, auth_generation);
                self.sync_notify.notify_one();
            }
            Err(err) if err.is_transport_error() => {
                self.transport_failure_with_accounting(area_id, operation_id);
            }
            Err(err) => {
                // Validation/authorization verdicts never spin: park the
                // envelope for Retry / Discard and close its accounting.
                if self
                    .pending
                    .permanent_failure(area_id, operation_id, err.to_string())
                {
                    self.sync_stats
                        .operations_failed
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Reports a transport failure to the store and, when the attempt budget
    /// just expired and parked the envelope, records the terminal failure in
    /// the stats. The count keys off the store's returned verdict — issued
    /// exactly once per park, under the transition's own lock — so a
    /// concurrent resolution can never skew the accounting.
    fn transport_failure_with_accounting(&self, area_id: AreaId, operation_id: OperationId) {
        if self
            .pending
            .transport_failure(area_id, operation_id, Instant::now())
            == TransportVerdict::Parked
        {
            self.sync_stats
                .operations_failed
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The conflict path (§2.5): refetch the area's confirmed projection,
    /// replay the pending queue over it, and either resend the head against
    /// the fresh revision (everything still applies — a sane operation's
    /// reapply is a deliberate overwrite by design) or pause the queue
    /// targeting the envelope that failed the sanity check.
    async fn reconcile_conflict(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
        viewer_id: Option<Uuid>,
        auth_generation: u64,
    ) {
        if viewer_id.is_some() && self.backend.auth_generation() != auth_generation {
            self.pending
                .credential_changed(area_id, operation_id, auth_generation);
            self.sync_notify.notify_one();
            return;
        }
        self.backend.purge_area(&area_id).await;
        let fetched = if viewer_id.is_some() {
            self.backend
                .get_area_at_generation(&area_id, auth_generation)
                .await
        } else {
            self.backend.get_area(&area_id).await
        };
        match fetched {
            Ok(fresh) => {
                let _mutation_guard = self.mutation_gate.lock();
                if viewer_id.is_some() && self.backend.auth_generation() != auth_generation {
                    self.pending
                        .credential_changed(area_id, operation_id, auth_generation);
                    self.sync_notify.notify_one();
                    return;
                }
                if !self.pending.is_in_flight_at_generation(
                    area_id,
                    operation_id,
                    viewer_id,
                    auth_generation,
                ) {
                    return;
                }
                match self.replay_pending_over_locked(area_id, &fresh, ReplayMode::StopAtFailure) {
                    None => self.pending.ready_resend(area_id),
                    Some(failed) => self.pending.pause_conflict(area_id, failed),
                }
            }
            Err(CloudError::CredentialChanged) => {
                self.pending
                    .credential_changed(area_id, operation_id, auth_generation);
                self.sync_notify.notify_one();
            }
            Err(err) => {
                // The refetch itself failed; back off and retry like any
                // transport failure (the envelope stays queued).
                warn!("Conflict refetch of area {area_id} failed: {err}");
                self.transport_failure_with_accounting(area_id, operation_id);
            }
        }
    }

    /// Folds every pending envelope for `area_id` over `fresh`, in order,
    /// per `mode`. Envelopes are atomic: each applies to a scratch copy so
    /// a partly applicable envelope leaves no trace in the fold. Returns
    /// the folded document and the first failing envelope's operation id.
    fn fold_pending(
        &self,
        area_id: AreaId,
        fresh: &AreaWithDetails,
        mode: ReplayMode,
    ) -> (AreaWithDetails, Option<OperationId>) {
        let mut working = fresh.clone();
        let mut first_failed = None;
        for envelope in self.pending.pending_for(area_id) {
            let mut scratch = working.clone();
            let preconditions_hold =
                mode == ReplayMode::KeepMine || envelope.structural_preconditions_hold(&working);
            if preconditions_hold
                && envelope
                    .ops
                    .iter()
                    .all(|op| area_edits::apply_mutation(&mut scratch, op).is_ok())
            {
                working = scratch;
            } else {
                if first_failed.is_none() {
                    first_failed = Some(envelope.operation_id);
                }
                if mode == ReplayMode::StopAtFailure {
                    break;
                }
            }
        }
        (working, first_failed)
    }

    /// Rebuilds the displayed area as `fresh confirmed projection + pending
    /// envelopes` (folded per `mode`), records the fetched revision and
    /// fingerprint as backend truth, and swaps the rebuilt cache in.
    ///
    /// The fold races concurrent enqueues: an envelope whose optimistic
    /// effect landed on the pre-swap cache would vanish from a swap folded
    /// without it, so the store's enqueue epoch is compared across each
    /// snapshot→swap window and the fold re-runs on a change. The retry is
    /// bounded — a sustained write storm keeps its later envelopes queued
    /// either way, and the sync engine heals any display residue.
    ///
    /// Returns the operation id of the first envelope that failed to
    /// apply, from the last fold performed (`None` = everything applied).
    /// The caller must already hold the mutation gate.
    fn replay_pending_over_locked(
        &self,
        area_id: AreaId,
        fresh: &AreaWithDetails,
        mode: ReplayMode,
    ) -> Option<OperationId> {
        self.pending.adopt_confirmed_rev(
            area_id,
            fresh.area.rev,
            fresh.area.access.map(|access| access.fingerprint()),
        );
        let (working, failed) = self.fold_pending(area_id, fresh, mode);
        self.swap_area_details(working);
        failed
    }

    /// Swaps a full area document into the atlas cache the way a sync
    /// refetch lands one, bumping the sync revision so pollers notice. The
    /// rcu preserves every other area and every exclusion axis.
    fn swap_area_details(&self, details: AreaWithDetails) {
        let area_id = details.area.id;
        let area_cache = Arc::new(AreaCache::new_with_area(details));
        self.atlas_cache
            .rcu(|cache| Arc::new(cache.insert_area(area_id, area_cache.clone())));
        self.sync_revision.fetch_add(1, Ordering::AcqRel);
    }

    /// Purges any cached copy of `area_id` and refetches the backend's
    /// confirmed projection for a display rebuild. A failed refetch is
    /// logged and yields `None` — the caller's queue state is already
    /// correct, and the sync engine heals the display later.
    async fn refetch_confirmed(
        &self,
        area_id: AreaId,
        identity: Option<(Option<Uuid>, u64)>,
    ) -> Option<AreaWithDetails> {
        if !self.read_identity_is_active(identity) {
            return None;
        }
        self.backend.purge_area(&area_id).await;
        let fetched = match identity {
            Some((Some(_), auth_generation)) => {
                self.backend
                    .get_area_at_generation(&area_id, auth_generation)
                    .await
            }
            _ => self.backend.get_area(&area_id).await,
        };
        match fetched {
            Ok(fresh) => Some(fresh),
            Err(err) => {
                warn!("Refetch of area {area_id} for a display rebuild failed: {err}");
                None
            }
        }
    }

    fn read_identity_is_active(&self, identity: Option<(Option<Uuid>, u64)>) -> bool {
        match identity {
            Some((Some(viewer_id), auth_generation)) => {
                self.backend.auth_generation() == auth_generation
                    && self.pending.active_viewer() == Some((viewer_id, auth_generation))
            }
            _ => true,
        }
    }

    /// Rebuild after an envelope leaves the queue outside the acknowledge
    /// path (discard, cancel): the departed operation's optimistic effect
    /// must leave the display, so the area is rebuilt from backend truth
    /// plus whatever remains pending. A remaining operation that depended
    /// on the removed one pauses at its own sanity check, targeted by id.
    async fn rebuild_after_removal(&self, area_id: AreaId, identity: Option<(Option<Uuid>, u64)>) {
        if let Some(fresh) = self.refetch_confirmed(area_id, identity).await {
            let _mutation_guard = self.mutation_gate.lock();
            if !self.read_identity_is_active(identity) {
                return;
            }
            if let Some(failed) =
                self.replay_pending_over_locked(area_id, &fresh, ReplayMode::StopAtFailure)
            {
                self.pending.pause_conflict(area_id, failed);
            }
        }
    }

    /// The Keep-mine display rebuild: refetch the confirmed projection and
    /// fold *every* pending envelope over it best-effort, so the operations
    /// the user chose to keep are visible again. An envelope that cannot
    /// apply locally stays pending (undisplayed) and still goes to the
    /// server, whose verdict is authoritative — it may well accept what the
    /// local sanity check could not model.
    async fn rebuild_keeping_pending(
        &self,
        area_id: AreaId,
        identity: Option<(Option<Uuid>, u64)>,
    ) {
        if let Some(fresh) = self.refetch_confirmed(area_id, identity).await {
            let _mutation_guard = self.mutation_gate.lock();
            if !self.read_identity_is_active(identity) {
                return;
            }
            self.replay_pending_over_locked(area_id, &fresh, ReplayMode::KeepMine);
        }
    }

    /// Resolves a conflict-paused area; see [`Mapper::resolve_conflict`].
    pub async fn resolve_conflict(&self, area_id: AreaId, keep_mine: bool) -> CloudResult<()> {
        let identity = self.pending.area_identity(area_id);
        let resolution = self.pending.resolve_conflict(area_id, keep_mine)?;
        if !resolution.resolved {
            return Ok(());
        }
        if resolution.discarded.is_some() {
            // The discarded envelope was sent-tracked but can never
            // succeed; close its accounting so the counters settle.
            self.sync_stats
                .operations_failed
                .fetch_add(1, Ordering::Relaxed);
            Self::decrement_pending(&self.pending_by_area, area_id);
            self.rebuild_after_removal(area_id, identity).await;
        } else if keep_mine {
            // The store held the queue paused for exactly this window:
            // rebuild the display with the kept operations first, then
            // release the resend so it cannot race the fold.
            self.rebuild_keeping_pending(area_id, identity).await;
            self.pending.ready_resend(area_id);
        }
        // Keep theirs with nothing to discard (the conflicted envelope was
        // independently canceled): the cancel already settled accounting
        // and rebuilt the display.
        Ok(())
    }

    /// Resolves a permanently-failed area; see [`Mapper::resolve_failed`].
    pub async fn resolve_failed(&self, area_id: AreaId, retry: bool) -> CloudResult<()> {
        // Parked envelopes were terminally counted at park time: a retry
        // reopens that accounting (the envelope will close it again when it
        // next acknowledges or parks), a discard closes the per-area
        // counter the park left in place. Both key off the store's returned
        // resolution — decided under the transition's own lock — never off
        // a status re-read a concurrent park could race.
        let identity = self.pending.area_identity(area_id);
        let resolution = self.pending.resolve_failure(area_id, retry)?;
        if !resolution.unparked {
            return Ok(());
        }
        if retry {
            self.sync_stats
                .operations_failed
                .fetch_sub(1, Ordering::Relaxed);
            // A restored queue may be waiting for an unavailable base rather
            // than for the mutation worker. Re-run reconciliation as well as
            // waking the pending queue.
            self.sync_notify.notify_one();
        } else if resolution.discarded.is_some() {
            Self::decrement_pending(&self.pending_by_area, area_id);
            self.rebuild_after_removal(area_id, identity).await;
        }
        Ok(())
    }

    /// Cancels a queued-but-unsent envelope; see [`Mapper::cancel_pending`].
    pub async fn cancel_pending(
        &self,
        area_id: AreaId,
        operation_id: OperationId,
    ) -> CloudResult<bool> {
        let Some(removed) = self.pending.cancel(area_id, operation_id)? else {
            return Ok(false);
        };
        // The canceled envelope never produced a backend verdict: unwind its
        // enqueue-time bookkeeping rather than recording an outcome.
        self.sync_stats
            .operations_sent
            .fetch_sub(1, Ordering::Relaxed);
        Self::decrement_pending(&self.pending_by_area, area_id);
        self.rebuild_after_removal(area_id, Some((removed.viewer_id, removed.auth_generation)))
            .await;
        Ok(true)
    }

    // === INDEX MANAGEMENT ===
}

impl Mapper {
    /// Optimistically mirrors a `POST /areas/{id}/secret-marks` change onto
    /// the cached atlas: flips `is_secret` on the referenced entities and
    /// bumps the area rev by one (like other local edits, so open editors
    /// notice and resync their inspectors).
    ///
    /// No sync operation is enqueued — the server already owns the change;
    /// its bumped rev arrives through the sync engine (callers typically
    /// follow a successful POST with [`Self::sync_now`]).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_local_secret_marks(
        &self,
        area_id: AreaId,
        secret: bool,
        rooms: &[RoomNumber],
        exits: &[ExitId],
        labels: &[LabelId],
        shapes: &[ShapeId],
        room_properties: &[(RoomNumber, String)],
        area_properties: &[String],
    ) {
        self.inner.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.apply_secret_marks(
                            secret,
                            rooms,
                            exits,
                            labels,
                            shapes,
                            room_properties,
                            area_properties,
                        )),
                    ))
                },
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Area, AreaAccess, AreaWithDetails, CloudError, CreateAreaRequest, Exit, RoomWithDetails,
    };
    use async_trait::async_trait;
    use chrono::Utc;

    /// Backend serving a set of areas from memory; content mutations apply
    /// through the shared CAS applier so the pending queue drains against
    /// real revision semantics, and each received envelope is logged for
    /// receipt/batching assertions.
    struct FixedBackend {
        areas: Mutex<HashMap<AreaId, AreaWithDetails>>,
        omit_from_list: Mutex<HashSet<AreaId>>,
        /// One `(operation id, operation count)` entry per received
        /// envelope, in arrival order.
        mutations: Mutex<Vec<(Uuid, usize)>>,
        /// When set, every received envelope fails with this error instead
        /// of applying (scripted server verdicts / outages).
        fail_with: Mutex<Option<CloudError>>,
    }

    impl FixedBackend {
        fn new(areas: Vec<AreaWithDetails>) -> Self {
            Self {
                areas: Mutex::new(areas.into_iter().map(|a| (a.area.id, a)).collect()),
                omit_from_list: Mutex::new(HashSet::new()),
                mutations: Mutex::new(Vec::new()),
                fail_with: Mutex::new(None),
            }
        }

        fn fail_mutations_with(&self, error: Option<CloudError>) {
            *self.fail_with.lock() = error;
        }

        fn omit_from_list(&self, area_id: AreaId) {
            self.omit_from_list.lock().insert(area_id);
        }
    }

    #[async_trait]
    impl MapperBackend for FixedBackend {
        async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
            Err(CloudError::NetworkError("read-only".to_string()))
        }

        async fn import_local_area(&self, _details: AreaWithDetails) -> CloudResult<()> {
            // Cache-side effects are all the import tests observe.
            Ok(())
        }

        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            let omitted = self.omit_from_list.lock();
            Ok(self
                .areas
                .lock()
                .values()
                .filter(|area| !omitted.contains(&area.area.id))
                .map(|area| area.area.clone())
                .collect())
        }

        async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            self.areas
                .lock()
                .get(area_id)
                .cloned()
                .ok_or(CloudError::NotFoundOrNoAccess)
        }

        async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
            Ok(())
        }

        async fn execute_mutation(
            &self,
            area_id: &AreaId,
            envelope: &crate::mutation::MutationEnvelope,
        ) -> CloudResult<crate::mutation::MutationResult> {
            self.mutations
                .lock()
                .push((envelope.operation_id, envelope.payload.len()));
            if let Some(err) = self.fail_with.lock().clone() {
                return Err(err);
            }
            let mut areas = self.areas.lock();
            let details = areas
                .get_mut(area_id)
                .ok_or(CloudError::NotFoundOrNoAccess)?;
            // All-or-nothing like the server: apply to a working copy and
            // commit only a fully-successful envelope.
            let mut working = details.clone();
            let result = area_edits::apply_envelope(&mut working, *area_id, envelope)?;
            *details = working;
            Ok(result)
        }
    }

    fn sample_area(area_id: AreaId, room_title: &str) -> AreaWithDetails {
        AreaWithDetails {
            area: Area {
                id: area_id,
                user_id: None,
                atlas_id: None,
                name: format!("area {area_id}"),
                created_at: Utc::now(),
                rev: 1,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                atlas_name: None,
            },
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: vec![],
            rooms: vec![RoomWithDetails {
                room_number: RoomNumber(1),
                title: room_title.to_string(),
                description: String::new(),
                level: 0,
                x: 0.0,
                y: 0.0,
                color: String::new(),
                properties: vec![],
                exits: vec![],
                tags: Default::default(),
                is_secret: false,
                external_id: None,
            }],
            labels: vec![],
            shapes: vec![],
            connections: vec![],
            linked_areas: vec![],
        }
    }

    fn temp_cache_dir() -> PathBuf {
        std::env::temp_dir().join(format!("smudgy-mapper-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn initial_load_point_probes_delete_intent_missing_from_best_effort_list() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(area_id, "still here")]));
        let cache_dir = temp_cache_dir();
        let mapper = Mapper::new(backend.clone(), &cache_dir);
        let envelope = PendingEnvelope {
            operation_id: Uuid::new_v4(),
            ops: vec![AreaMutation::UpsertRoom {
                room_number: RoomNumber(1),
                body: RoomUpdates {
                    title: Some("durable edit".to_string()),
                    ..RoomUpdates::default()
                },
            }],
            description: "durable edit before ambiguous delete".to_string(),
            structural_preconditions: Vec::new(),
            attempts: 0,
            viewer_id: None,
            local_durable: true,
            auth_generation: 0,
            sequence: 0,
            queued_at: Utc::now(),
            journal_path: None,
            receipt_expired: false,
            published: false,
            journal_batch_id: None,
            delete_intent: false,
        };
        let _publication = mapper
            .inner
            .pending
            .enqueue_staged(area_id, envelope)
            .expect("stage durable WAL");
        mapper
            .inner
            .pending
            .begin_delete(area_id)
            .expect("delete fence");
        mapper
            .inner
            .pending
            .prepare_delete(area_id)
            .expect("delete intent");
        mapper.inner.pending.mark_delete_ambiguous(area_id);
        backend.omit_from_list(area_id);

        mapper
            .load_all_areas()
            .await
            .expect("point read reconciles list omission");
        assert!(
            mapper.get_current_atlas().get_area(&area_id).is_some(),
            "best-effort list omission is not proof that deletion committed"
        );
        assert!(
            !mapper.inner.pending.has_delete_intent(area_id),
            "confirmed presence durably aborts the delete intent"
        );
        assert_eq!(mapper.inner.pending.pending_for(area_id).len(), 1);
        let _ = fs::remove_dir_all(cache_dir);
    }

    #[tokio::test]
    async fn import_areas_if_absent_skips_resident_names_even_when_scope_excluded() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![sample_area(a_id, "Plaza")]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let resident_name = format!("area {a_id}");

        // Scope-exclude the resident area: invisible to identification and to
        // a filtered area listing, but it must still count as present.
        mapper.set_scope_exclusions(HashSet::new(), std::iter::once(a_id).collect());

        let mut duplicate = sample_area(AreaId(Uuid::new_v4()), "Copy");
        duplicate.area.name.clone_from(&resident_name);
        let mut fresh = sample_area(AreaId(Uuid::new_v4()), "Hall");
        fresh.area.name = "Newtown".to_string();

        let outcome = mapper
            .import_areas_if_absent(vec![duplicate.clone(), fresh.clone()])
            .await
            .expect("import");
        assert_eq!(outcome.skipped, vec![resident_name.clone()]);
        assert_eq!(outcome.added.len(), 1, "only the new name imports");

        // A repeat offer is fully absorbed: the fresh import is resident now.
        let again = mapper
            .import_areas_if_absent(vec![duplicate, fresh])
            .await
            .expect("import again");
        assert!(again.added.is_empty());
        assert_eq!(again.skipped.len(), 2);
    }

    #[tokio::test]
    async fn import_areas_if_absent_waits_for_the_initial_load() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![sample_area(a_id, "Plaza")]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());

        // Fire the presence-checked import BEFORE any areas load — the entry-
        // script ordering. It must block on the initial-load gate and then
        // compare against the loaded atlas, not the empty pre-load cache.
        let mut duplicate = sample_area(AreaId(Uuid::new_v4()), "Copy");
        duplicate.area.name = format!("area {a_id}");
        let early = tokio::spawn({
            let mapper = mapper.clone();
            async move { mapper.import_areas_if_absent(vec![duplicate]).await }
        });
        tokio::task::yield_now().await;
        assert!(!early.is_finished(), "must wait for the initial load");

        mapper.load_all_areas().await.expect("load");
        let outcome = early.await.expect("join").expect("import");
        assert!(
            outcome.added.is_empty(),
            "the resident name was only visible because the import waited"
        );
        assert_eq!(outcome.skipped.len(), 1);
    }

    /// A v2 document with one reciprocal exit pair sharing one Connection,
    /// plus an External Connection for a cross-area exit.
    fn sample_v2_document(area_id: AreaId, foreign: AreaId) -> AreaWithDetails {
        use crate::{
            Connection, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionKind,
            ConnectionRouting, CornerStyle, PortMode, RoomSide, SegmentShape,
        };
        let pair = ConnectionId::new();
        let external = ConnectionId::new();
        let endpoint = |room: i32, side: RoomSide| ConnectionEndpoint {
            room_number: RoomNumber(room),
            side,
            port_offset: 0.5,
            port_mode: PortMode::AutoPinned,
        };
        let connection = |id: ConnectionId,
                          a: ConnectionEndpoint,
                          b: Option<ConnectionEndpoint>,
                          kind: ConnectionKind| Connection {
            id,
            endpoint_a: a,
            endpoint_b: b,
            kind,
            routing: ConnectionRouting::Simple,
            segment_shape: SegmentShape::Direct,
            corner: CornerStyle::Sharp,
            route_points: Vec::new(),
            dash: ConnectionDash::Solid,
            color: crate::DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: crate::DEFAULT_CONNECTION_THICKNESS,
        };
        let exit = |n: u128,
                    from: crate::ExitDirection,
                    to: Option<(AreaId, i32)>,
                    connection_id: ConnectionId| Exit {
            id: ExitId(Uuid::from_u128(n)),
            from_direction: from,
            to_area_id: to.map(|(area, _)| area),
            to_room_number: to.map(|(_, room)| RoomNumber(room)),
            to_direction: None,
            path: String::new(),
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: String::new(),
            connection_id,
            to_unknown: false,
            to_area_token: None,
            is_secret: false,
        };
        let mut details = sample_area(area_id, "Origin");
        details.rooms[0].exits = vec![
            exit(1, crate::ExitDirection::East, Some((area_id, 2)), pair),
            exit(3, crate::ExitDirection::North, Some((foreign, 1)), external),
        ];
        details.rooms.push(RoomWithDetails {
            room_number: RoomNumber(2),
            title: "Far".to_string(),
            description: String::new(),
            level: 0,
            x: 2.0,
            y: 0.0,
            color: String::new(),
            properties: vec![],
            exits: vec![exit(
                2,
                crate::ExitDirection::West,
                Some((area_id, 1)),
                pair,
            )],
            tags: std::collections::BTreeSet::default(),
            is_secret: false,
            external_id: None,
        });
        details.connections = vec![
            connection(
                pair,
                endpoint(1, RoomSide::East),
                Some(endpoint(2, RoomSide::West)),
                ConnectionKind::Internal,
            ),
            connection(
                external,
                endpoint(1, RoomSide::North),
                None,
                ConnectionKind::External,
            ),
        ];
        details
    }

    #[test]
    fn exit_compiler_strips_redundant_full_snapshot_on_pair() {
        let area_id = AreaId(Uuid::new_v4());
        let details = sample_v2_document(area_id, AreaId(Uuid::new_v4()));

        let compiled = compile_area_mutations(
            &details,
            vec![AreaMutation::UpdateExit {
                exit_id: ExitId(Uuid::from_u128(1)),
                body: ExitUpdates {
                    from_direction: Some(crate::ExitDirection::East),
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    path: Some(String::new()),
                    is_hidden: Some(false),
                    is_closed: Some(false),
                    is_locked: Some(false),
                    weight: Some(1.0),
                    command: Some(String::new()),
                    is_secret: Some(false),
                    clear_to: Some(false),
                    ..ExitUpdates::default()
                },
            }],
            PairedExitPolicy::Reject,
        )
        .expect("equal fields are not structural edits");

        assert!(compiled.is_empty(), "a no-op must not mint a mutation");
    }

    #[test]
    fn exit_compiler_persists_fresh_connection_identity() {
        let area_id = AreaId(Uuid::new_v4());
        let details = sample_area(area_id, "Origin");

        let compiled = compile_area_mutations(
            &details,
            vec![AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                body: ExitArgs {
                    from_direction: crate::ExitDirection::Special,
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            }],
            PairedExitPolicy::Reject,
        )
        .expect("fresh exit compiles");

        let [AreaMutation::CreateExit { body, .. }] = compiled.as_slice() else {
            panic!("one explicit exit create expected: {compiled:?}");
        };
        let exit_id = body.id.expect("exit identity is durable");
        let connection_id = body
            .new_connection_id
            .expect("fresh connection identity is durable");
        assert!(
            body.connection_id.is_none(),
            "a fresh identity is not an existing-membership request"
        );

        let mut applied = details;
        let envelope = MutationEnvelope {
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: area_id.0,
                expected_rev: applied.area.rev,
                access_fingerprint: applied.area.access.map(|access| access.fingerprint()),
            }],
            payload: compiled,
        };
        area_edits::apply_envelope(&mut applied, area_id, &envelope)
            .expect("compiled create validates");
        assert_eq!(applied.connections[0].id, connection_id);
        assert_eq!(applied.rooms[0].exits[0].id, exit_id);
        assert_eq!(applied.rooms[0].exits[0].connection_id, connection_id);
    }

    #[test]
    fn exit_compiler_persists_existing_auto_pair_identity() {
        let area_id = AreaId(Uuid::new_v4());
        let mut details = sample_v2_document(area_id, AreaId(Uuid::new_v4()));
        let pair_id = details.rooms[0].exits[0].connection_id;
        details.rooms[1].exits.clear();

        let compiled = compile_area_mutations(
            &details,
            vec![AreaMutation::CreateExit {
                room_number: RoomNumber(2),
                body: ExitArgs {
                    from_direction: crate::ExitDirection::West,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(1)),
                    to_direction: Some(crate::ExitDirection::East),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            }],
            PairedExitPolicy::Reject,
        )
        .expect("reciprocal exit compiles");

        let [AreaMutation::CreateExit { body, .. }] = compiled.as_slice() else {
            panic!("one explicit exit create expected: {compiled:?}");
        };
        assert_eq!(body.connection_id, Some(pair_id));
        assert!(
            body.new_connection_id.is_none(),
            "auto-pairing addresses the existing connection"
        );

        let mut applied = details;
        let envelope = MutationEnvelope {
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: area_id.0,
                expected_rev: applied.area.rev,
                access_fingerprint: applied.area.access.map(|access| access.fingerprint()),
            }],
            payload: compiled,
        };
        area_edits::apply_envelope(&mut applied, area_id, &envelope)
            .expect("compiled auto-pair validates");
        let member_count = applied
            .rooms
            .iter()
            .flat_map(|room| room.exits.iter())
            .filter(|exit| exit.connection_id == pair_id)
            .count();
        assert_eq!(member_count, 2);
    }

    #[test]
    fn exit_compiler_requires_explicit_split_for_real_pair_topology_change() {
        let area_id = AreaId(Uuid::new_v4());
        let details = sample_v2_document(area_id, AreaId(Uuid::new_v4()));
        let update = AreaMutation::UpdateExit {
            exit_id: ExitId(Uuid::from_u128(1)),
            body: ExitUpdates {
                to_direction: Some(crate::ExitDirection::North),
                ..ExitUpdates::default()
            },
        };

        let rejected =
            compile_area_mutations(&details, vec![update.clone()], PairedExitPolicy::Reject)
                .expect_err("generic update must not silently split a pair");
        assert!(matches!(
            rejected,
            CloudError::StructuralConflict(ref reason) if reason == "unlink_before_edit"
        ));

        let compiled = compile_area_mutations(&details, vec![update], PairedExitPolicy::Split)
            .expect("the explicit structural gesture may split");
        assert!(matches!(compiled[0], AreaMutation::Unlink { .. }));
        assert!(matches!(compiled[1], AreaMutation::UpdateExit { .. }));

        let mut applied = details.clone();
        let envelope = MutationEnvelope {
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: area_id.0,
                expected_rev: applied.area.rev,
                access_fingerprint: applied.area.access.map(|access| access.fingerprint()),
            }],
            payload: compiled,
        };
        area_edits::apply_envelope(&mut applied, area_id, &envelope)
            .expect("unlink and edit validate atomically");
        assert_eq!(applied.connections.len(), 3, "the pair was split once");
    }

    #[test]
    fn merge_rooms_rewires_traversal_and_deletes_the_source_atomically() {
        let area_id = AreaId(Uuid::new_v4());
        let foreign = AreaId(Uuid::new_v4());
        let details = sample_v2_document(area_id, foreign);

        let raw = merge_room_operations(&details, RoomNumber(1), RoomNumber(2))
            .expect("same-area merge plan");
        let compiled = compile_area_mutations(&details, raw, PairedExitPolicy::Split)
            .expect("paired topology is split explicitly");
        assert!(compiled.len() <= MAX_MUTATION_OPERATIONS);
        assert!(matches!(
            compiled.last(),
            Some(AreaMutation::DeleteRoom {
                room_number: RoomNumber(2)
            })
        ));

        let mut applied = details;
        let envelope = MutationEnvelope {
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: area_id.0,
                expected_rev: applied.area.rev,
                access_fingerprint: applied.area.access.map(|access| access.fingerprint()),
            }],
            payload: compiled,
        };
        area_edits::apply_envelope(&mut applied, area_id, &envelope)
            .expect("the whole merge validates");
        assert!(
            applied
                .rooms
                .iter()
                .all(|room| room.room_number != RoomNumber(2))
        );
        assert!(
            applied
                .rooms
                .iter()
                .flat_map(|room| &room.exits)
                .all(|exit| {
                    exit.to_area_id != Some(area_id) || exit.to_room_number != Some(RoomNumber(2))
                })
        );
        let kept = applied
            .rooms
            .iter()
            .find(|room| room.room_number == RoomNumber(1))
            .expect("kept room");
        assert!(
            kept.exits
                .iter()
                .any(|exit| exit.to_area_id == Some(foreign)
                    && exit.to_room_number == Some(RoomNumber(1))),
            "unrelated traversal already on the kept room survives"
        );
    }

    /// §8.4 import: fresh Connection identities with exits' membership kept
    /// consistent, and a dropped outside-the-set cross-area link converts
    /// its External Connection to Dangling.
    #[tokio::test]
    async fn import_remaps_connection_ids_and_repairs_dropped_links() {
        let source_id = AreaId(Uuid::new_v4());
        let foreign = AreaId(Uuid::new_v4());
        let document = sample_v2_document(source_id, foreign);
        let old_pair = document.connections[0].id;

        let backend = FixedBackend::new(vec![]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let ids = mapper.import_areas(vec![document]).await.expect("import");
        assert_eq!(ids.len(), 1);

        let cached = mapper
            .get_current_atlas()
            .get_area(&ids[0])
            .expect("imported area cached");
        let room1 = cached.get_room(&RoomNumber(1)).expect("room 1");
        let room2 = cached.get_room(&RoomNumber(2)).expect("room 2");
        let pair_ids: Vec<_> = room1
            .get_exits()
            .iter()
            .chain(room2.get_exits().iter())
            .filter(|exit| exit.to_area_id == Some(ids[0]))
            .map(|exit| exit.connection_id)
            .collect();
        assert_eq!(pair_ids.len(), 2, "both pair members survive");
        assert_eq!(pair_ids[0], pair_ids[1], "membership stays consistent");
        assert_ne!(pair_ids[0], old_pair, "connection ids are re-minted");

        // The cross-area link pointed outside the imported set: dropped, and
        // its External Connection became Dangling.
        let dropped = room1
            .get_exits()
            .iter()
            .find(|exit| exit.to_area_id.is_none())
            .expect("dropped link became dangling")
            .connection_id;
        let dangling = cached
            .get_room_connections()
            .iter()
            .find(|rc| rc.connection_id == dropped)
            .expect("the dangling Connection renders");
        assert_eq!(dangling.kind, crate::ConnectionKind::Dangling);
    }

    /// §8.4 import: invariant violations reject the whole import before any
    /// write.
    #[tokio::test]
    async fn import_rejects_invariant_violations_wholesale() {
        let backend = FixedBackend::new(vec![]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // (a) an exit referencing a connection the document does not carry;
        let broken_id = AreaId(Uuid::new_v4());
        let mut broken = sample_v2_document(broken_id, AreaId(Uuid::new_v4()));
        broken.rooms[0].exits[0].connection_id = crate::ConnectionId::new();
        // (b) a healthy sibling that must NOT import alongside it.
        let healthy = sample_v2_document(AreaId(Uuid::new_v4()), AreaId(Uuid::new_v4()));

        let err = mapper
            .import_areas(vec![healthy, broken])
            .await
            .expect_err("a broken document rejects the whole import");
        assert!(
            err.to_string().contains("references connection"),
            "the rejection names the violation: {err}"
        );
        assert!(
            mapper.get_current_atlas().areas().next().is_none(),
            "nothing imported"
        );

        // Member-count violation: an orphan Connection row.
        let orphan_id = AreaId(Uuid::new_v4());
        let mut orphan = sample_v2_document(orphan_id, AreaId(Uuid::new_v4()));
        orphan.connections.push(crate::Connection {
            id: crate::ConnectionId::new(),
            ..orphan.connections[0].clone()
        });
        let err = mapper
            .import_areas(vec![orphan])
            .await
            .expect_err("an orphan Connection rejects the import");
        assert!(err.to_string().contains("member exits"), "{err}");
    }

    /// §8.4 import surface: a v1 document deserializes through
    /// [`AreaImportDocument`] (migrating on the way in), and a newer format
    /// is rejected at the boundary.
    #[tokio::test]
    async fn import_document_dispatches_on_format_version() {
        let area_id = AreaId(Uuid::new_v4());
        let v1 = serde_json::json!({
            "id": area_id.0,
            "user_id": null,
            "atlas_id": null,
            "name": "Legacy Import",
            "created_at": "2025-01-01T00:00:00Z",
            "rev": 2,
            "properties": [],
            "rooms": [{
                "room_number": 1, "title": "Hall", "description": "",
                "level": 0, "x": 0.0, "y": 0.0, "color": "", "properties": [],
                "exits": [{
                    "id": Uuid::from_u128(0xAB), "from_direction": "North",
                    "to_area_id": null, "to_room_number": null, "to_direction": null,
                    "path": "", "is_hidden": false, "is_closed": false,
                    "is_locked": false, "weight": 1.0, "command": "",
                    "style": "Stub", "color": "#224466"
                }]
            }],
            "labels": [],
            "shapes": []
        });
        let document: AreaImportDocument =
            serde_json::from_value(v1).expect("a v1 document is accepted via migration");
        let migrated = document.into_inner();
        assert_eq!(migrated.format_version, crate::AREA_FORMAT_VERSION);
        assert_eq!(migrated.connections.len(), 1);
        assert_eq!(
            migrated.connections[0].routing,
            crate::ConnectionRouting::Stub
        );
        assert_eq!(migrated.connections[0].color, "#224466");

        let backend = FixedBackend::new(vec![]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let ids = mapper.import_areas(vec![migrated]).await.expect("import");
        assert_eq!(ids.len(), 1, "the migrated document imports cleanly");

        let v3 = serde_json::json!({ "format_version": 3, "id": Uuid::new_v4(), "name": "Future" });
        let err = serde_json::from_value::<AreaImportDocument>(v3)
            .expect_err("a newer format is rejected without partial import");
        assert!(err.to_string().contains("newer than this client"), "{err}");
    }

    /// §8.4 export: connections ride out stably id-sorted.
    #[tokio::test]
    async fn export_sorts_connections_by_id() {
        let area_id = AreaId(Uuid::new_v4());
        let mut details = sample_v2_document(area_id, AreaId(Uuid::new_v4()));
        details.connections.sort_by_key(|connection| connection.id);
        details.connections.reverse(); // serve them deliberately unsorted
        let backend = FixedBackend::new(vec![details]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        let exported = mapper.export_area(area_id).await.expect("export");
        let ids: Vec<_> = exported.connections.iter().map(|c| c.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "export orders connections stably by id");
    }

    #[tokio::test]
    async fn disabled_set_survives_unrelated_mutation() {
        let a_id = AreaId(Uuid::new_v4());
        let b_id = AreaId(Uuid::new_v4());
        let backend =
            FixedBackend::new(vec![sample_area(a_id, "Plaza"), sample_area(b_id, "Plaza")]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        mapper.set_area_enabled(b_id, false);
        assert!(!mapper.is_area_enabled(&b_id));

        // An unrelated mutation rebuilds the cache; the disabled set must
        // ride through instead of silently re-enabling the area.
        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("Annex".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");

        let atlas = mapper.get_current_atlas();
        assert!(!atlas.is_area_enabled(&b_id));
        assert!(atlas.is_area_enabled(&a_id));
        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Plaza")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![a_id]);
        assert!(
            atlas.get_room(&RoomKey::new(a_id, RoomNumber(2))).is_some(),
            "the unrelated mutation itself must land"
        );
    }

    #[tokio::test]
    async fn disabling_unknown_area_is_harmless_and_survives_full_reload() {
        let a_id = AreaId(Uuid::new_v4());
        let phantom = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![sample_area(a_id, "Plaza")]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());

        // Disable an area the cache has never seen, before anything loads.
        mapper.set_disabled_areas(std::iter::once(phantom).collect());
        assert!(mapper.disabled_areas().contains(&phantom));
        assert!(!mapper.is_area_enabled(&phantom));

        mapper.load_all_areas().await.expect("load");

        assert!(
            mapper.disabled_areas().contains(&phantom),
            "wholesale reload must preserve the disabled set"
        );
        assert!(mapper.is_area_enabled(&a_id));

        // Toggling back removes it.
        mapper.set_area_enabled(phantom, true);
        assert!(mapper.disabled_areas().is_empty());
    }

    fn exit_to(id: u128, to_area: AreaId, to_room: i32) -> Exit {
        Exit {
            id: ExitId(Uuid::from_u128(id)),
            from_direction: crate::ExitDirection::North,
            to_area_id: Some(to_area),
            to_room_number: Some(RoomNumber(to_room)),
            to_direction: Some(crate::ExitDirection::South),
            path: String::new(),
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: String::new(),
            connection_id: crate::ConnectionId::new(),
            to_unknown: false,
            to_area_token: None,
            is_secret: false,
        }
    }

    fn room_with_exits(number: i32, exits: Vec<Exit>) -> RoomWithDetails {
        RoomWithDetails {
            room_number: RoomNumber(number),
            title: format!("room {number}"),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            properties: vec![],
            exits,
            tags: Default::default(),
            is_secret: false,
            external_id: None,
        }
    }

    fn area_with_rooms(area_id: AreaId, mut rooms: Vec<RoomWithDetails>) -> AreaWithDetails {
        let exits: Vec<_> = rooms
            .iter_mut()
            .flat_map(|room| {
                let room_number = room.room_number;
                std::mem::take(&mut room.exits)
                    .into_iter()
                    .map(move |exit| (room_number, exit))
            })
            .collect();
        let mut details = AreaWithDetails {
            area: Area {
                id: area_id,
                user_id: None,
                atlas_id: None,
                name: format!("area {area_id}"),
                created_at: Utc::now(),
                rev: 1,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                atlas_name: None,
            },
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: vec![],
            rooms,
            labels: vec![],
            shapes: vec![],
            connections: vec![],
            linked_areas: vec![],
        };
        for (room_number, exit) in exits {
            area_edits::apply_mutation(
                &mut details,
                &AreaMutation::CreateExit {
                    room_number,
                    body: ExitArgs {
                        id: Some(exit.id),
                        connection_id: None,
                        new_connection_id: None,
                        from_direction: exit.from_direction,
                        to_area_id: exit.to_area_id,
                        to_room_number: exit.to_room_number,
                        to_direction: exit.to_direction,
                        path: Some(exit.path),
                        is_hidden: exit.is_hidden,
                        is_closed: exit.is_closed,
                        is_locked: exit.is_locked,
                        weight: exit.weight,
                        command: Some(exit.command),
                        is_secret: Some(exit.is_secret),
                    },
                },
            )
            .expect("valid exit fixture");
        }
        details
    }

    #[tokio::test]
    async fn merge_rooms_rejects_visible_cross_area_links_before_enqueue() {
        let area_id = AreaId(Uuid::new_v4());
        let foreign_id = AreaId(Uuid::new_v4());
        let area = area_with_rooms(
            area_id,
            vec![
                room_with_exits(1, vec![]),
                room_with_exits(2, vec![exit_to(10, foreign_id, 9)]),
            ],
        );
        let foreign = area_with_rooms(
            foreign_id,
            vec![room_with_exits(9, vec![exit_to(11, area_id, 2)])],
        );
        let backend = FixedBackend::new(vec![area, foreign]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        let error = mapper
            .merge_rooms(area_id, RoomNumber(1), RoomNumber(2))
            .expect_err("cross-area traversal makes a same-area merge unsafe");
        assert!(matches!(
            error,
            CloudError::StructuralConflict(ref reason) if reason == "merge_cross_area_links"
        ));
        assert_eq!(mapper.inner.pending.total_pending(), 0);
    }

    #[tokio::test]
    async fn delete_room_clears_inbound_exits_across_areas() {
        let a_id = AreaId(Uuid::new_v4());
        let b_id = AreaId(Uuid::new_v4());

        // A:1 --(north)--> A:2 (same area) and A:1 --> B:5 (cross area, an
        // unrelated link). B:5 --> A:2 (cross-area inbound to the victim).
        let a = area_with_rooms(
            a_id,
            vec![
                room_with_exits(1, vec![exit_to(1, a_id, 2), exit_to(2, b_id, 5)]),
                room_with_exits(2, vec![]),
            ],
        );
        let b = area_with_rooms(b_id, vec![room_with_exits(5, vec![exit_to(3, a_id, 2)])]);

        let backend = FixedBackend::new(vec![a, b]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Delete A:2 — every exit pointing at it must lose its destination.
        mapper
            .delete_room(RoomKey::new(a_id, RoomNumber(2)))
            .expect("enqueue room delete");

        let atlas = mapper.get_current_atlas();
        assert!(
            atlas.get_room(&RoomKey::new(a_id, RoomNumber(2))).is_none(),
            "room removed"
        );

        let find = |room: &Arc<crate::mapper::room_cache::RoomCache>, id: u128| {
            room.get_exits()
                .iter()
                .find(|e| e.id == ExitId(Uuid::from_u128(id)))
                .expect("exit present")
                .clone()
        };

        // Same-area inbound (A:1 -> A:2) is cleared, destination and direction.
        let a1 = atlas
            .get_room(&RoomKey::new(a_id, RoomNumber(1)))
            .expect("A:1");
        let cleared = find(&a1, 1);
        assert_eq!(cleared.to_area_id, None);
        assert_eq!(cleared.to_room_number, None);
        assert_eq!(cleared.to_direction, None);

        // The unrelated exit (A:1 -> B:5) is untouched.
        let untouched = find(&a1, 2);
        assert_eq!(untouched.to_area_id, Some(b_id));
        assert_eq!(untouched.to_room_number, Some(RoomNumber(5)));

        // Cross-area inbound (B:5 -> A:2) is cleared too.
        let b5 = atlas
            .get_room(&RoomKey::new(b_id, RoomNumber(5)))
            .expect("B:5");
        let cross = find(&b5, 3);
        assert_eq!(cross.to_area_id, None);
        assert_eq!(cross.to_room_number, None);
    }

    #[tokio::test]
    async fn scope_exclusion_hides_area_without_touching_the_manual_axis() {
        let a_id = AreaId(Uuid::new_v4());
        let b_id = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![
            sample_area(a_id, "Midgaard"),
            sample_area(b_id, "Midgaard"),
        ]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Scope-exclude B (by area id, since these sample areas are atlas-less).
        mapper.set_scope_exclusions(HashSet::new(), std::iter::once(b_id).collect());

        let atlas = mapper.get_current_atlas();
        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Midgaard")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(
            by_title,
            vec![a_id],
            "the scope-excluded stock zone drops out"
        );

        // The manual axis is untouched: B is still "enabled", disabled set empty.
        assert!(mapper.is_area_enabled(&b_id));
        assert!(mapper.disabled_areas().is_empty());
        assert!(
            !mapper.is_area_included(&b_id),
            "but it no longer participates"
        );
        assert!(mapper.is_area_included(&a_id));

        // Scope exclusion survives an unrelated mutation (cache rebuild).
        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(9)),
                RoomUpdates {
                    title: Some("Annex".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");
        assert!(!mapper.get_current_atlas().is_area_included(&b_id));
    }

    #[tokio::test]
    async fn ephemeral_area_survives_scope_exclusion_of_everything_else() {
        use crate::backends::EphemeralBackend;

        let mapper = Mapper::new(Arc::new(EphemeralBackend::new()), temp_cache_dir());
        let area_id = mapper
            .create_area_at(
                "Session".to_string(),
                MapDestination::loose(MapStorage::Session),
            )
            .await
            .expect("create ephemeral");
        mapper
            .upsert_room(
                RoomKey::new(area_id, RoomNumber(1)),
                RoomUpdates {
                    title: Some("Wilderness".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue ephemeral room");

        // Only cloud atlas/area ids ever enter the scope store, so excluding a
        // pile of arbitrary cloud ids can never touch a session-tier area.
        mapper.set_scope_exclusions(
            std::iter::once(AtlasId(Uuid::new_v4())).collect(),
            std::iter::once(AreaId(Uuid::new_v4())).collect(),
        );

        assert!(
            mapper.is_area_included(&area_id),
            "ephemeral areas are never scope-excluded"
        );
        let by_title: Vec<AreaId> = mapper
            .get_current_atlas()
            .get_rooms_by_title("Wilderness")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![area_id]);
    }

    /// Polls until `condition` holds — the mutation worker drains queues on
    /// its own task, so tests wait for the store to settle.
    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..1000u32 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(condition(), "condition not met within timeout");
    }

    #[tokio::test]
    async fn content_edit_enqueues_and_worker_drains_to_acknowledgement() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        assert_eq!(mapper.inner.pending.confirmed_rev(a_id).0, Some(1));

        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("Annex".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        // The backend applied one envelope and the confirmed revision moved.
        assert_eq!(backend.mutations.lock().len(), 1);
        assert_eq!(mapper.inner.pending.confirmed_rev(a_id).0, Some(2));
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        // The optimistic room stays displayed and the backend stored it.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(2)))
                .is_some()
        );
        assert!(
            backend.areas.lock()[&a_id]
                .rooms
                .iter()
                .any(|room| room.room_number == RoomNumber(2))
        );
    }

    #[tokio::test]
    async fn revision_conflict_refetches_replays_and_resends_the_same_receipt() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Someone else's edit moves the backend past our confirmed revision.
        backend.areas.lock().get_mut(&a_id).expect("area").area.rev = 2;

        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("Annex".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        // The first send conflicted; the refetch replayed the pending edit
        // cleanly and the identical receipt went out again.
        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 2, "conflicted send plus the resend");
        assert_eq!(
            mutations[0].0, mutations[1].0,
            "the resend carries the same operation id"
        );
        assert_eq!(mapper.inner.pending.confirmed_rev(a_id).0, Some(3));
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
    }

    #[tokio::test]
    async fn new_room_number_taken_during_conflict_pauses_instead_of_overwriting() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // The stale client chose room 2 while it was vacant. Before its
        // envelope reached the server, another editor created room 2.
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.push(RoomWithDetails {
                room_number: RoomNumber(2),
                title: "Remote room".to_string(),
                description: String::new(),
                level: 0,
                x: 8.0,
                y: 3.0,
                color: String::new(),
                properties: vec![],
                exits: vec![],
                tags: Default::default(),
                is_secret: false,
                external_id: None,
            });
        }

        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("My room".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        let displayed = mapper
            .get_current_atlas()
            .get_room(&RoomKey::new(a_id, RoomNumber(2)))
            .expect("fresh remote room is displayed");
        assert_eq!(displayed.get_title(), "Remote room");
        assert_eq!(backend.mutations.lock().len(), 1, "no automatic resend");

        mapper
            .resolve_conflict(a_id, false)
            .await
            .expect("resolve conflict");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert_eq!(
            backend.areas.lock()[&a_id].rooms[1].title,
            "Remote room",
            "Keep theirs preserves the remotely-created room"
        );
    }

    #[tokio::test]
    async fn keep_mine_explicitly_overrides_a_taken_new_room_number() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.push(RoomWithDetails {
                room_number: RoomNumber(2),
                title: "Remote room".to_string(),
                description: String::new(),
                level: 0,
                x: 8.0,
                y: 3.0,
                color: String::new(),
                properties: vec![],
                exits: vec![],
                tags: Default::default(),
                is_secret: false,
                external_id: None,
            });
        }
        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("My room".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        mapper
            .resolve_conflict(a_id, true)
            .await
            .expect("resolve conflict");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert_eq!(backend.areas.lock()[&a_id].rooms[1].title, "My room");
        assert_eq!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(2)))
                .expect("kept room displayed")
                .get_title(),
            "My room"
        );
    }

    #[tokio::test]
    async fn room_edit_does_not_recreate_a_room_deleted_during_conflict() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.clear();
        }

        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(1)),
                RoomUpdates {
                    title: Some("My edit".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;
        assert!(backend.areas.lock()[&a_id].rooms.is_empty());

        mapper
            .resolve_conflict(a_id, false)
            .await
            .expect("resolve conflict");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(1)))
                .is_none()
        );
    }

    #[tokio::test]
    async fn conflicting_delete_pauses_for_review_and_keep_theirs_discards() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Someone else already deleted room 1 (and the area moved on), so
        // our pending delete fails the structural sanity check on refetch.
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.clear();
        }

        mapper
            .delete_room(RoomKey::new(a_id, RoomNumber(1)))
            .expect("enqueue room delete");

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        // Keep theirs: the delete is discarded and the queue drains.
        mapper
            .resolve_conflict(a_id, false)
            .await
            .expect("resolve conflict");

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert_eq!(mapper.inner.pending.total_pending(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(
            stats.operations_failed(),
            1,
            "a discarded operation closes its accounting as failed"
        );
        // The display converged on the backend's truth.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(1)))
                .is_none()
        );
        // Only the conflicted send ever reached the backend.
        assert_eq!(backend.mutations.lock().len(), 1);
    }

    #[tokio::test]
    async fn upsert_rooms_batches_into_one_envelope() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        let rooms: Vec<(RoomNumber, RoomUpdates)> = (2..=4)
            .map(|number| {
                (
                    RoomNumber(number),
                    RoomUpdates {
                        title: Some(format!("Room {number}")),
                        ..RoomUpdates::default()
                    },
                )
            })
            .collect();
        mapper
            .upsert_rooms(a_id, rooms)
            .expect("enqueue room batch");

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 1, "one envelope for the whole batch");
        assert_eq!(mutations[0].1, 3, "every room rides that envelope");
        assert_eq!(mapper.get_sync_stats().operations_sent(), 1);
        assert_eq!(backend.areas.lock()[&a_id].rooms.len(), 4);
    }

    #[tokio::test]
    async fn mutation_batches_publish_nothing_when_a_later_batch_is_invalid() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend, temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        let result = mapper.mutate_batches(vec![
            AreaMutationBatch::strict(
                a_id,
                vec![AreaMutation::UpsertRoom {
                    room_number: RoomNumber(1),
                    body: RoomUpdates {
                        title: Some("Must not publish".to_string()),
                        ..RoomUpdates::default()
                    },
                }],
                "valid prefix",
            ),
            AreaMutationBatch::strict(
                a_id,
                vec![AreaMutation::DeleteExit {
                    exit_id: ExitId::new(),
                }],
                "invalid suffix",
            ),
        ]);

        assert!(result.is_err());
        assert_eq!(mapper.inner.pending.total_pending(), 0);
        assert_eq!(mapper.get_sync_stats().operations_sent(), 0);
        assert_eq!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(1)))
                .expect("room")
                .get_title(),
            "Plaza"
        );
    }

    #[tokio::test]
    async fn oversized_room_batch_splits_into_capped_envelopes() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // One over the wire cap: the batch must split, not be rejected.
        let count = MAX_MUTATION_OPERATIONS + 1;
        let rooms: Vec<(RoomNumber, RoomUpdates)> = (0..count)
            .map(|index| {
                (
                    RoomNumber(i32::try_from(index).expect("small") + 2),
                    RoomUpdates {
                        title: Some(format!("Room {index}")),
                        ..RoomUpdates::default()
                    },
                )
            })
            .collect();
        mapper
            .upsert_rooms(a_id, rooms)
            .expect("enqueue room batches");

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 2, "a capped chunk plus the remainder");
        assert_eq!(mutations[0].1, MAX_MUTATION_OPERATIONS);
        assert_eq!(mutations[1].1, 1);
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 2);
        assert_eq!(stats.operations_succeeded(), 2);
        assert_eq!(stats.operations_failed(), 0);
        // Every room landed exactly once (plus the pre-existing room 1).
        assert_eq!(backend.areas.lock()[&a_id].rooms.len(), count + 1);
    }

    /// The finding-1 scenario end-to-end: the envelope that fails the
    /// post-refetch sanity check is a *later* one, and the conflict flow
    /// must target it — never the sane head.
    #[tokio::test]
    async fn later_envelope_conflict_targets_that_envelope_and_keep_theirs_spares_the_rest() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let mut events = mapper.subscribe_mapper_events();

        // Someone else's edit moves the backend past our confirmed revision
        // and removes room 1 before our next fetch.
        let mut areas = backend.areas.lock();
        let remote = areas.get_mut(&a_id).expect("area");
        remote.area.rev = 2;
        remote.rooms.clear();
        drop(areas);

        // Head: a sane upsert. Follower: a delete that was valid against our
        // last confirmed view, but fails the sanity check on refetch.
        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("Annex".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue head room");
        mapper
            .delete_room(RoomKey::new(a_id, RoomNumber(1)))
            .expect("enqueue follower delete");
        let queued = mapper.inner.pending.pending_for(a_id);
        assert_eq!(queued.len(), 2);
        let head_id = queued[0].operation_id;
        let failing_id = queued[1].operation_id;

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        // The conflict event names the failing follower, not the head.
        let conflicted = loop {
            if let MapperEvent::MutationConflict {
                operation_id,
                description,
                ..
            } = events.try_recv().expect("conflict event emitted")
            {
                break (operation_id, description);
            }
        };
        assert_eq!(conflicted.0, failing_id);
        assert_eq!(conflicted.1, "Delete room 1");
        // The sane head's optimistic effect stays displayed at the pause.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(2)))
                .is_some(),
            "the clean prefix of the queue stays displayed"
        );

        // Keep theirs: exactly the failing envelope is discarded; the sane
        // head survives, resends under its original receipt, and lands.
        mapper
            .resolve_conflict(a_id, false)
            .await
            .expect("resolve conflict");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        assert_eq!(mapper.inner.pending.total_pending(), 0);
        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 2, "conflicted send plus the head's resend");
        assert_eq!(
            mutations[0].0, mutations[1].0,
            "the resend carries the head's original operation id"
        );
        assert_eq!(mutations[0].0, head_id);
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 2);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 1, "only the discarded follower");
        assert_eq!(stats.pending_operations(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        // The head's edit reached the backend; the discarded delete did not.
        assert!(
            backend.areas.lock()[&a_id]
                .rooms
                .iter()
                .any(|room| room.room_number == RoomNumber(2))
        );
    }

    /// The finding-4 scenario: Keep mine must rebuild the display with the
    /// kept operations, not leave them invisible until some later sync.
    #[tokio::test]
    async fn keep_mine_restores_kept_edits_to_the_display() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Someone else's edit moves the backend past our confirmed revision
        // and removes room 1 before our next fetch.
        let mut areas = backend.areas.lock();
        let remote = areas.get_mut(&a_id).expect("area");
        remote.area.rev = 2;
        remote.rooms.clear();
        drop(areas);

        // Head: a locally valid delete that fails the sanity check against the
        // newer remote view. Follower: a sane room the user expects to see.
        mapper
            .delete_room(RoomKey::new(a_id, RoomNumber(1)))
            .expect("enqueue head delete");
        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(3)),
                RoomUpdates {
                    title: Some("Later".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue follower room");

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;
        // The pause fold stopped at the failing head, hiding the follower.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(3)))
                .is_none(),
            "precondition: the pause hid the follower's optimistic effect"
        );

        // Keep mine: by the time the resolution returns, the kept edits are
        // displayed again (fresh + all pending, best-effort).
        mapper
            .resolve_conflict(a_id, true)
            .await
            .expect("resolve conflict");
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(3)))
                .is_some(),
            "keep mine restores the kept edits to the display"
        );

        // The kept head goes to the server as-is, whose verdict (room 9
        // does not exist) parks it permanently; discarding it lets the
        // sane follower drain.
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::CouldNotSave { .. }
            )
        })
        .await;
        mapper
            .resolve_failed(a_id, false)
            .await
            .expect("discard failure");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 2);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 1);
        assert_eq!(stats.pending_operations(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        assert!(
            backend.areas.lock()[&a_id]
                .rooms
                .iter()
                .any(|room| room.room_number == RoomNumber(3)),
            "the follower landed on the backend"
        );
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(3)))
                .is_some()
        );
    }

    /// Findings 2 + 8: a permanently-parked head refuses cancellation (its
    /// park already counted terminally), and the verdict-driven retry
    /// resolution reopens the accounting so every counter settles.
    #[tokio::test]
    async fn parked_head_refuses_cancel_and_retry_settles_the_accounting() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // The server rejects the envelope with a permanent verdict.
        backend.fail_mutations_with(Some(CloudError::PermissionDenied(
            "read-only share".to_string(),
        )));
        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("Annex".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::CouldNotSave { .. }
            )
        })
        .await;

        let head_id = mapper.inner.pending.pending_for(a_id)[0].operation_id;
        assert!(
            !mapper
                .cancel_pending(a_id, head_id)
                .await
                .expect("cancel check"),
            "a parked head resolves through resolve_failed, never cancel"
        );
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(stats.operations_failed(), 1);
        assert_eq!(stats.pending_operations(), 0, "no drift from the refusal");
        assert_eq!(mapper.inner.pending.total_pending(), 1);

        // Retry after the outage clears: the park's terminal count reopens
        // and the envelope drains normally.
        backend.fail_mutations_with(None);
        mapper
            .resolve_failed(a_id, true)
            .await
            .expect("retry failure");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 0);
        assert_eq!(stats.pending_operations(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        assert_eq!(
            mapper.wait_for_sync_completion(5).await,
            Ok(true),
            "the settled counters unblock the quit-time flush"
        );
    }

    /// A queue parked for conflict/failure review must refuse a move:
    /// snapshotting the optimistic view and deleting the source would
    /// silently resolve the review as "keep mine" against whatever the
    /// backend holds.
    #[tokio::test]
    async fn area_move_refuses_a_queue_parked_for_review() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        backend.fail_mutations_with(Some(CloudError::PermissionDenied(
            "read-only share".to_string(),
        )));
        mapper
            .upsert_room(
                RoomKey::new(a_id, RoomNumber(2)),
                RoomUpdates {
                    title: Some("Annex".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .expect("enqueue room");
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::CouldNotSave { .. }
            )
        })
        .await;

        assert!(
            matches!(
                mapper.begin_area_move(&[a_id]),
                Err(CloudError::PendingOperations(_))
            ),
            "a parked queue refuses the move fence"
        );

        // Resolving the failure reopens the move.
        backend.fail_mutations_with(None);
        mapper
            .resolve_failed(a_id, true)
            .await
            .expect("retry failure");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        let fences = mapper
            .begin_area_move(&[a_id])
            .expect("a drained queue moves freely");
        drop(fences);
    }

    /// Numbers drafted by an open scripted mutator are reserved against the
    /// live allocator: an ambient create landing mid-callback takes the next
    /// number past the drafts instead of silently merging with one, and an
    /// aborted mutator returns its numbers.
    #[tokio::test]
    async fn ambient_creates_skip_room_numbers_reserved_by_open_mutators() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // The sample area holds room 1; two mutator drafts reserve 2 and 3.
        let token = Uuid::new_v4();
        assert_eq!(
            mapper.reserve_room_number(&a_id, token).expect("reserve"),
            RoomNumber(2)
        );
        assert_eq!(
            mapper.reserve_room_number(&a_id, token).expect("reserve"),
            RoomNumber(3)
        );

        // The ambient allocator skips the drafted range.
        let ambient = mapper.next_room_number(&a_id).expect("area loaded");
        assert_eq!(ambient, RoomNumber(4), "ambient create lands past the drafts");
        mapper
            .upsert_room(RoomKey::new(a_id, ambient), RoomUpdates::default())
            .expect("enqueue ambient room");
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        // Later drafts continue past the ambient room; a second open mutator
        // holds its own reservations.
        assert_eq!(
            mapper.reserve_room_number(&a_id, token).expect("reserve"),
            RoomNumber(5)
        );
        let other = Uuid::new_v4();
        assert_eq!(
            mapper.reserve_room_number(&a_id, other).expect("reserve"),
            RoomNumber(6)
        );

        // Releasing one mutator keeps the other's range guarded; releasing
        // the last returns allocation to the cache maximum.
        mapper.release_room_reservations(&a_id, token);
        assert_eq!(mapper.next_room_number(&a_id), Some(RoomNumber(7)));
        mapper.release_room_reservations(&a_id, other);
        assert_eq!(
            mapper.next_room_number(&a_id),
            Some(RoomNumber(5)),
            "an aborted mutator's numbers become available again"
        );
    }
}
