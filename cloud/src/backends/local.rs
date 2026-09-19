//! On-disk, authoritative local map storage.
//!
//! [`LocalBackend`] is a [`MapperBackend`] whose areas live purely on the
//! local filesystem — never synced, available even when signed out. It is the
//! "local tier" that sits alongside the cloud backend inside a session's
//! mapper (see [`super::composite`]).
//!
//! Storage layout, under a dedicated root (e.g. `~/Documents/smudgy/local/`):
//!
//! ```text
//! <root>/areas-v2/<area_id>.json          one v2 AreaWithDetails per area (authoritative)
//! <root>/areas/<area_id>.json             the v1 namespace: read-only migration source
//! <root>/areas-v1-backup/<id>.<ts>.json   untouched v1 bytes, written before each migration
//! <root>/atlases/<atlas_id>.json          one Atlas manifest per folder
//! ```
//!
//! v2 (Connection-contract) documents live in the **`areas-v2/`** namespace,
//! which old binaries do not know how to overwrite (§8.3). A v1 file in the
//! old `areas/` namespace is migrated the first time it is seen — the scan
//! migrates stragglers **eagerly** (so a freshly-opened store lists every
//! area consistently). Migration runs only during initialization or explicit
//! refresh. Each migration first writes an untouched timestamped backup,
//! then atomically writes the migrated document into `areas-v2/`; only the
//! completed rename marks the migration done, and any failure leaves the v1
//! file intact and unopened (never a partial or empty replacement). Once a
//! v2 copy exists the v1 file is never re-read, so later edits made by an
//! old binary to the stale v1 namespace are deliberately not merged.
//! Documents newer than [`crate::AREA_FORMAT_VERSION`] are a hard read-only
//! error naming the file.
//!
//! Single-document saves use atomic replacement. Changes spanning documents
//! share one redo journal; its durable installation is the commit decision.
//! Recovery finishes decided transactions before accepting another write.
//! Legacy single-document and atlas-delete journals remain readable.
//!
//! Sessions mounting the same canonical directory share one store, initialized
//! lazily off the construction hot path. Reads pin immutable generations and
//! never wait for the writer or touch disk. A successful transaction publishes
//! all its changes in one swap, then signals subscribers. Unchanged documents
//! are shared across generations. External file edits require explicit refresh.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, OnceCell},
    task,
};
use uuid::Uuid;

use super::{
    AreaMergeCommit, AreaMergePlan, MapperBackend, apply_area_merge, area_edits, local_migration,
};
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, MapStorage,
    mutation::{MutationEnvelope, MutationResult},
};

const LOCAL_OPERATION_RECEIPT_LIMIT: usize = 1024;
const ATLAS_DELETE_TRANSACTION_PREFIX: &str = "atlas-delete-";
const MULTI_WRITE_TRANSACTION_PREFIX: &str = "multi-write-";

/// The sequence the last multi-write journal took. Journals are named
/// `multi-write-{sequence}-{uuid}.json` with the sequence zero-padded, so
/// their file names sort in commit order and recovery replays them oldest
/// first. The counter is process-wide and monotonic: every allocation is a
/// `fetch_add`, and every scan of a transactions directory raises it past
/// the highest sequence found there, so a journal written after a restart
/// always sorts after one that lingered through it. A wall clock would not
/// give that guarantee (it can step backwards, and its resolution can tie).
static MULTI_WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Coordinate store ownership by canonical filesystem identity. Weak registry
/// entries do not keep unused stores alive.
/// This coordinates sessions in this process, not separate app processes.
static STORES: LazyLock<parking_lot::Mutex<HashMap<PathBuf, Weak<LocalStore>>>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

fn next_multi_write_sequence() -> u64 {
    MULTI_WRITE_SEQUENCE.fetch_add(1, Ordering::AcqRel) + 1
}

/// The sequence a multi-write journal's file name carries; `None` for a
/// name without one, which sorts before every sequenced journal.
fn multi_write_sequence_of(path: &Path) -> Option<u64> {
    path.file_name()?
        .to_str()?
        .strip_prefix(MULTI_WRITE_TRANSACTION_PREFIX)?
        .split('-')
        .next()?
        .parse()
        .ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalMutationReceipt {
    operation_id: Uuid,
    result: MutationResult,
}

/// Local-only persistence wrapper. Flattening preserves the portable v2 area
/// JSON shape while committing idempotency receipts in the same atomic file
/// as the mutation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalAreaDocument {
    /// Metadata-only copies retain this identity so sessions can reuse room indexes.
    #[serde(skip)]
    content_identity: Arc<()>,
    #[serde(flatten)]
    details: AreaWithDetails,
    #[serde(
        default,
        rename = "_smudgy_applied_operations",
        skip_serializing_if = "Vec::is_empty"
    )]
    applied_operations: Vec<LocalMutationReceipt>,
}

impl LocalAreaDocument {
    fn new(details: AreaWithDetails) -> Self {
        Self {
            content_identity: Arc::new(()),
            details,
            applied_operations: Vec::new(),
        }
    }

    fn receipt(&self, operation_id: Uuid) -> Option<MutationResult> {
        self.applied_operations
            .iter()
            .find(|receipt| receipt.operation_id == operation_id)
            .map(|receipt| receipt.result.clone())
    }

    fn remember(&mut self, result: MutationResult) {
        self.applied_operations.push(LocalMutationReceipt {
            operation_id: result.operation_id,
            result,
        });
        let overflow = self
            .applied_operations
            .len()
            .saturating_sub(LOCAL_OPERATION_RECEIPT_LIMIT);
        if overflow > 0 {
            self.applied_operations.drain(..overflow);
        }
    }
}

/// Crash-recovery journal for a gentle atlas delete. `committed = false`
/// restores membership while preserving any newer document content; `true`
/// finishes the detach/delete if the process stopped after committing but
/// before removing the journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalAtlasDeleteTransaction {
    atlas: Atlas,
    members: Vec<LocalAreaDocument>,
    committed: bool,
}

/// A committed set of document writes and deletes that must land together.
/// The journal is complete before any document is touched, so recovery only
/// ever rolls forward; re-applying it is idempotent. Unlike the atlas-delete
/// journal there is no prepared phase and hence no `committed` flag: the
/// journal's existence is the commit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LocalMultiWriteTransaction {
    /// Full post-images, receipts included.
    writes: Vec<LocalAreaDocument>,
    deletes: Vec<AreaId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    atlases: Vec<Atlas>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deleted_atlases: Vec<AtlasId>,
    /// Explicit imports replace existing documents, even at a lower revision.
    /// Ordinary writes and recovery preserve newer on-disk documents.
    #[serde(default)]
    exact: bool,
}

/// A published local generation. Documents are committed, readable state;
/// journaled operations remain separate until recovery publishes their result.
/// Cloning shares persistent maps and unchanged documents.
#[derive(Debug, Clone, Default)]
pub struct LocalSnapshot {
    pub generation: u64,
    documents: imbl::HashMap<AreaId, Arc<LocalAreaDocument>>,
    atlases: imbl::HashMap<AtlasId, Atlas>,
    errors: imbl::HashMap<AreaId, CloudError>,
    pub(crate) recovery_error: Option<CloudError>,
    blocked_atlases: imbl::HashSet<AtlasId>,
    journaled_operations: imbl::HashSet<(AreaId, Uuid)>,
}

impl LocalSnapshot {
    pub(crate) fn applied_after(&self, area: AreaId, operation: Uuid, revision: i64) -> bool {
        self.documents
            .get(&area)
            .and_then(|document| document.receipt(operation))
            .is_some_and(|result| {
                result
                    .versions
                    .iter()
                    .any(|version| version.id == area.0 && version.rev > revision)
            })
    }

    pub(crate) fn shares_content(&self, other: &Self, id: AreaId) -> bool {
        self.documents
            .get(&id)
            .zip(other.documents.get(&id))
            .is_some_and(|(a, b)| Arc::ptr_eq(&a.content_identity, &b.content_identity))
    }

    pub(crate) fn journaled_operations(&self) -> impl Iterator<Item = &(AreaId, Uuid)> {
        self.journaled_operations.iter()
    }

    fn remember_journaled(&mut self, transaction: &LocalMultiWriteTransaction) {
        for document in &transaction.writes {
            self.journaled_operations.extend(
                document
                    .applied_operations
                    .iter()
                    .map(|receipt| (document.details.area.id, receipt.operation_id)),
            );
        }
    }

    pub(crate) fn has_applied(&self, area: AreaId, operation: Uuid) -> bool {
        self.documents.get(&area).is_some_and(|document| {
            document
                .applied_operations
                .iter()
                .any(|receipt| receipt.operation_id == operation)
        })
    }

    pub(crate) fn shares_area(&self, other: &Self, id: AreaId) -> bool {
        self.documents
            .get(&id)
            .zip(other.documents.get(&id))
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
    }
    #[must_use]
    pub fn contains_area(&self, id: AreaId) -> bool {
        self.documents.contains_key(&id) || self.errors.contains_key(&id)
    }

    pub fn areas(&self) -> impl Iterator<Item = &AreaWithDetails> {
        self.documents.values().map(|document| &document.details)
    }

    pub fn atlases(&self) -> impl Iterator<Item = &Atlas> {
        self.atlases.values()
    }

    pub(crate) fn atlas_ids(&self) -> impl Iterator<Item = AtlasId> + '_ {
        self.atlases
            .keys()
            .chain(self.blocked_atlases.iter())
            .copied()
    }

    /// Borrow an area from this pinned generation.
    ///
    /// # Errors
    /// Reports an absent area or its startup/refresh read error.
    pub fn area(&self, id: AreaId) -> CloudResult<&AreaWithDetails> {
        self.documents
            .get(&id)
            .map(|document| &document.details)
            .ok_or_else(|| {
                self.errors
                    .get(&id)
                    .cloned()
                    .unwrap_or(CloudError::AreaNotFound(id))
            })
    }

    fn document(&self, id: AreaId) -> CloudResult<LocalAreaDocument> {
        self.area(id)?;
        Ok(self.documents[&id].as_ref().clone())
    }

    fn apply(&mut self, transaction: LocalMultiWriteTransaction) {
        for document in transaction.writes {
            let id = document.details.area.id;
            self.errors.remove(&id);
            self.documents.insert(id, Arc::new(document));
        }
        for id in transaction.deletes {
            self.documents.remove(&id);
            self.errors.remove(&id);
        }
        for atlas in transaction.atlases {
            self.atlases.insert(atlas.id, atlas);
        }
        for id in transaction.deleted_atlases {
            self.atlases.remove(&id);
        }
    }
}

struct LocalStore {
    root: PathBuf,
    writer: Arc<Mutex<()>>,
    initialized: AtomicBool,
    needs_reload: AtomicBool,
    snapshot: ArcSwap<LocalSnapshot>,
    changed: tokio::sync::watch::Sender<u64>,
    #[cfg(test)]
    write_faults: std::sync::atomic::AtomicU32,
    #[cfg(test)]
    retirement_faults: std::sync::atomic::AtomicU32,
    #[cfg(test)]
    refresh_attempts: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    legacy_single_journals: AtomicBool,
}

impl LocalStore {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            writer: Arc::new(Mutex::new(())),
            initialized: AtomicBool::new(false),
            needs_reload: AtomicBool::new(false),
            snapshot: ArcSwap::from_pointee(LocalSnapshot::default()),
            changed: tokio::sync::watch::channel(0).0,
            #[cfg(test)]
            write_faults: std::sync::atomic::AtomicU32::new(0),
            #[cfg(test)]
            retirement_faults: std::sync::atomic::AtomicU32::new(0),
            #[cfg(test)]
            refresh_attempts: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            legacy_single_journals: AtomicBool::new(false),
        }
    }

    fn publish(&self, mut snapshot: LocalSnapshot) {
        snapshot.generation = self.snapshot.load().generation + 1;
        let generation = snapshot.generation;
        self.snapshot.store(Arc::new(snapshot));
        self.changed.send_replace(generation);
    }

    /// Recovery must finish before a writer derives or validates its inputs.
    /// Refuse further writes if a journal cannot be read, applied or retired.
    fn recover(&self) -> CloudResult<bool> {
        let transactions = self.root.join("transactions");
        let entries = match fs::read_dir(&transactions) {
            Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let journals: Vec<_> = entries
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        path.extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
                            && (name.starts_with(MULTI_WRITE_TRANSACTION_PREFIX)
                                || name.starts_with(ATLAS_DELETE_TRANSACTION_PREFIX))
                    })
            })
            .collect();
        if journals.is_empty() {
            if self.needs_reload.load(Ordering::Acquire) {
                sync_parent(&transactions.join("retired-journal"))?;
            }
            return Ok(false);
        }
        self.needs_reload.store(true, Ordering::Release);
        let areas = self.root.join("areas-v2");
        recover_atlas_delete_transactions(&transactions, &areas, &self.root.join("atlases"));
        recover_multi_write_transactions(&transactions, &areas, &self.root.join("areas"));
        sync_parent(&transactions.join("retired-journal"))?;
        if let Some(path) = journals.iter().find(|path| path.exists()) {
            return Err(CloudError::InternalError(format!(
                "local transaction {} requires recovery before further writes",
                path.display()
            )));
        }
        Ok(true)
    }

    fn scan(&self) -> CloudResult<LocalSnapshot> {
        self.scan_with_migration(true)
    }

    fn scan_with_migration(&self, migrate_legacy: bool) -> CloudResult<LocalSnapshot> {
        let areas = self.root.join("areas-v2");
        let legacy = self.root.join("areas");
        let backup = self.root.join("areas-v1-backup");
        let documents = scan_areas(&areas, &legacy, migrate_legacy.then_some(backup.as_path()))?;
        let mut snapshot = LocalSnapshot {
            documents: documents
                .into_iter()
                .map(|(id, document)| (id, Arc::new(document)))
                .collect(),
            atlases: scan_atlases(&self.root.join("atlases"))?
                .into_iter()
                .collect(),
            ..Default::default()
        };
        // Retain read failures in the snapshot. Ordinary reads never retry IO
        // or migrate files, including reads of an unsupported document version.
        for directory in [&areas, &legacy] {
            if let Ok(entries) = fs::read_dir(directory) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_none_or(|extension| extension != "json") {
                        continue;
                    }
                    let Some(id) = path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .and_then(|stem| Uuid::parse_str(stem).ok())
                        .map(AreaId)
                    else {
                        continue;
                    };
                    if snapshot.documents.contains_key(&id) || snapshot.errors.contains_key(&id) {
                        continue;
                    }
                    let error = if directory == &areas {
                        fs::read(&path)
                            .map_err(CloudError::from)
                            .and_then(|bytes| parse_v2_document(&bytes, &path))
                            .err()
                    } else {
                        Some(CloudError::InvalidInput(format!(
                            "could not migrate local map {}",
                            path.display()
                        )))
                    };
                    if let Some(error) = error {
                        snapshot.errors.insert(id, error);
                    }
                }
            }
        }
        Ok(snapshot)
    }

    /// A cold start has no previous generation to retain. Keep surviving
    /// documents readable, but withhold every member of an unfinished known
    /// transaction rather than publish its partially installed post-images.
    /// An unreadable journal cannot identify its members; preserve the files
    /// and report the degraded store instead of denying unrelated reads.
    fn scan_blocked(&self, error: &CloudError) -> CloudResult<LocalSnapshot> {
        let mut snapshot = self.scan_with_migration(false)?;
        snapshot.recovery_error = Some(error.clone());
        if let Ok(entries) = fs::read_dir(self.root.join("transactions")) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if path
                    .extension()
                    .is_none_or(|extension| !extension.eq_ignore_ascii_case("json"))
                {
                    continue;
                }
                let (areas, atlases) = if name.starts_with(MULTI_WRITE_TRANSACTION_PREFIX) {
                    let Ok(transaction) = read_multi_write_journal(&path) else {
                        continue;
                    };
                    snapshot.remember_journaled(&transaction);
                    (
                        transaction
                            .writes
                            .iter()
                            .map(|d| d.details.area.id)
                            .chain(transaction.deletes)
                            .collect::<Vec<_>>(),
                        transaction
                            .atlases
                            .iter()
                            .map(|a| a.id)
                            .chain(transaction.deleted_atlases)
                            .collect::<Vec<_>>(),
                    )
                } else if name.starts_with(ATLAS_DELETE_TRANSACTION_PREFIX) {
                    let Ok(bytes) = fs::read(&path) else { continue };
                    let Ok(transaction) =
                        serde_json::from_slice::<LocalAtlasDeleteTransaction>(&bytes)
                    else {
                        continue;
                    };
                    (
                        transaction
                            .members
                            .iter()
                            .map(|d| d.details.area.id)
                            .collect(),
                        vec![transaction.atlas.id],
                    )
                } else {
                    continue;
                };
                for id in areas {
                    snapshot.documents.remove(&id);
                    snapshot.errors.insert(id, error.clone());
                }
                for id in atlases {
                    snapshot.atlases.remove(&id);
                    snapshot.blocked_atlases.insert(id);
                }
            }
        }
        Ok(snapshot)
    }

    /// Keep the last readable documents, but publish the durable operation
    /// identities so a restarted session cannot offer to discard these edits.
    fn commit_pending(
        &self,
        transaction: &LocalMultiWriteTransaction,
        message: String,
    ) -> CloudError {
        let mut snapshot = self.snapshot.load_full().as_ref().clone();
        snapshot.recovery_error = Some(CloudError::InternalError(message.clone()));
        snapshot.remember_journaled(transaction);
        self.publish(snapshot);
        CloudError::LocalCommitPending {
            generation: self.snapshot.load().generation,
            message,
        }
    }

    fn commit_single(&self, transaction: LocalMultiWriteTransaction) -> CloudResult<()> {
        let (path, bytes) = if let Some(document) = transaction.writes.first() {
            (
                self.root
                    .join("areas-v2")
                    .join(format!("{}.json", document.details.area.id)),
                serde_json::to_vec_pretty(document)?,
            )
        } else {
            let atlas = &transaction.atlases[0];
            (
                self.root.join("atlases").join(format!("{}.json", atlas.id)),
                serde_json::to_vec_pretty(atlas)?,
            )
        };
        fs::create_dir_all(path.parent().expect("document directory"))?;
        install_atomic(&path, &bytes)?;
        self.needs_reload.store(true, Ordering::Release);
        #[cfg(test)]
        let durability = if self
            .retirement_faults
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_ok()
        {
            Err(io::Error::other("injected directory sync failure"))
        } else {
            sync_parent(&path)
        };
        #[cfg(not(test))]
        let durability = sync_parent(&path);
        if let Err(error) = durability {
            return Err(self.commit_pending(
                &transaction,
                format!(
                    "local file {} was replaced but durability could not be confirmed: {error}",
                    path.display()
                ),
            ));
        }
        self.needs_reload.store(false, Ordering::Release);
        let mut snapshot = self.snapshot.load_full().as_ref().clone();
        snapshot.apply(transaction);
        self.publish(snapshot);
        Ok(())
    }

    fn check_disk_revisions(&self, transaction: &LocalMultiWriteTransaction) -> CloudResult<()> {
        if !transaction.exact {
            let snapshot = self.snapshot.load();
            for id in transaction
                .writes
                .iter()
                .map(|d| d.details.area.id)
                .chain(transaction.deletes.iter().copied())
            {
                let path = self.root.join("areas-v2").join(format!("{id}.json"));
                // Unreadable targets are handled by roll-forward below, which
                // retains the decided transaction for recovery. A readable
                // newer document, however, is a conflict before the decision.
                if let Ok(bytes) = fs::read(&path) {
                    let disk_rev = v2_document_revision(&bytes, &path)?;
                    let expected = snapshot
                        .documents
                        .get(&id)
                        .map_or(0, |d| d.details.area.rev);
                    if disk_rev > expected {
                        return Err(CloudError::InvalidInput(format!(
                            "local area {id} changed on disk; refresh local maps before saving"
                        )));
                    }
                } else if !path.exists() && snapshot.contains_area(id) {
                    return Err(CloudError::InvalidInput(format!(
                        "local area {id} was removed on disk; refresh local maps before saving"
                    )));
                }
            }
        }
        Ok(())
    }

    fn commit(&self, transaction: LocalMultiWriteTransaction) -> CloudResult<()> {
        if transaction.writes.is_empty()
            && transaction.deletes.is_empty()
            && transaction.atlases.is_empty()
            && transaction.deleted_atlases.is_empty()
        {
            return Ok(());
        }
        self.check_disk_revisions(&transaction)?;
        let single_write = transaction.writes.len() + transaction.atlases.len() == 1
            && transaction.deletes.is_empty()
            && transaction.deleted_atlases.is_empty();
        #[cfg(test)]
        let single_write = single_write && !self.legacy_single_journals.load(Ordering::Acquire);
        if single_write {
            return self.commit_single(transaction);
        }
        let directory = self.root.join("transactions");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            "{MULTI_WRITE_TRANSACTION_PREFIX}{:020}-{}.json",
            next_multi_write_sequence(),
            Uuid::new_v4()
        ));
        let bytes = serde_json::to_vec_pretty(&transaction)?;
        if let Err(error) = write_atomic(&path, &bytes) {
            if fs::metadata(&path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound) {
                return Err(error.into());
            }
            self.needs_reload.store(true, Ordering::Release);
            return Err(self.commit_pending(
                &transaction,
                format!(
                    "transaction {} was installed but durability could not be confirmed: {error}",
                    path.display()
                ),
            ));
        }
        // The durable decision is now made. The blocking owner retains both
        // store and writer guard even if its async caller is cancelled.
        self.needs_reload.store(true, Ordering::Release);
        let finish = || {
            #[cfg(test)]
            if self
                .write_faults
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(CloudError::InternalError("injected write fault".into()));
            }
            roll_multi_write_journal_forward(
                &path,
                &transaction,
                &HashSet::new(),
                &self.root.join("areas-v2"),
                &self.root.join("areas"),
            )?;
            #[cfg(test)]
            if self
                .retirement_faults
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(CloudError::InternalError(
                    "injected retirement flush fault".into(),
                ));
            }
            Ok::<_, CloudError>(())
        };
        if let Err(first) = finish()
            && let Err(second) = finish()
        {
            return Err(self.commit_pending(&transaction, format!(
                "transaction {} is journaled and completes on the next start or refresh: {first}; retry: {second}", path.display()
            )));
        }
        let mut snapshot = self.snapshot.load_full().as_ref().clone();
        snapshot.apply(transaction);
        self.publish(snapshot);
        self.needs_reload.store(false, Ordering::Release);
        Ok(())
    }
}

/// A lazy handle to the process-wide store for a canonical local directory.
/// Construction does no IO; after initialization reads pin immutable state.
pub struct LocalBackend {
    root: PathBuf,
    store: OnceCell<Arc<LocalStore>>,
}

impl std::fmt::Debug for LocalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalBackend")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl LocalBackend {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            store: OnceCell::new(),
        }
    }

    async fn resolve(&self) -> CloudResult<Arc<LocalStore>> {
        self.store
            .get_or_try_init(|| async {
                let root = self.root.clone();
                task::spawn_blocking(move || {
                    fs::create_dir_all(&root)?;
                    let canonical = fs::canonicalize(root)?;
                    let mut stores = STORES.lock();
                    stores.retain(|_, store| store.strong_count() > 0);
                    let slot = stores.entry(canonical.clone()).or_default();
                    if let Some(store) = slot.upgrade() {
                        return Ok(store);
                    }
                    let store = Arc::new(LocalStore::new(canonical));
                    *slot = Arc::downgrade(&store);
                    Ok::<_, CloudError>(store)
                })
                .await
                .map_err(|error| CloudError::InternalError(error.to_string()))?
            })
            .await
            .cloned()
    }

    async fn ensure_loaded(&self) -> CloudResult<Arc<LocalStore>> {
        let store = self.resolve().await?;
        if !store.initialized.load(Ordering::Acquire) {
            let guard = Arc::clone(&store.writer).lock_owned().await;
            let owned = Arc::clone(&store);
            task::spawn_blocking(move || {
                let _guard = guard;
                if !owned.initialized.load(Ordering::Acquire) {
                    let snapshot = match owned.recover() {
                        Ok(_) => {
                            let snapshot = owned.scan()?;
                            owned.needs_reload.store(false, Ordering::Release);
                            snapshot
                        }
                        Err(error) => {
                            log::warn!("Local maps are read-only until recovery succeeds: {error}");
                            owned.scan_blocked(&error)?
                        }
                    };
                    owned.publish(snapshot);
                    owned.initialized.store(true, Ordering::Release);
                }
                Ok::<_, CloudError>(())
            })
            .await
            .map_err(|error| CloudError::InternalError(error.to_string()))??;
        }
        Ok(store)
    }

    /// Pin one committed generation without taking the writer lock.
    ///
    /// # Errors
    /// Reports directory initialization failures. Unfinished transactions
    /// block writes and reads of their known members, not unrelated areas.
    pub async fn snapshot(&self) -> CloudResult<Arc<LocalSnapshot>> {
        Ok(self.ensure_loaded().await?.snapshot.load_full())
    }

    /// Explicitly adopt external filesystem changes. Normal reads do not
    /// scan, migrate or recover; same-process writes publish automatically.
    ///
    /// # Errors
    /// Reports directory access or unfinished journal recovery failures.
    pub async fn refresh(&self) -> CloudResult<()> {
        let store = self.resolve().await?;
        #[cfg(test)]
        store.refresh_attempts.fetch_add(1, Ordering::Relaxed);
        let guard = Arc::clone(&store.writer).lock_owned().await;
        task::spawn_blocking(move || {
            let _guard = guard;
            store.needs_reload.store(true, Ordering::Release);
            store.recover()?;
            store.publish(store.scan()?);
            store.needs_reload.store(false, Ordering::Release);
            store.initialized.store(true, Ordering::Release);
            Ok(())
        })
        .await
        .map_err(|error| CloudError::InternalError(error.to_string()))?
    }

    #[cfg(test)]
    pub(crate) fn refresh_attempts_for_test(&self) -> usize {
        self.store
            .get()
            .map_or(0, |store| store.refresh_attempts.load(Ordering::Relaxed))
    }

    async fn transact<R: Send + 'static>(
        &self,
        prepare: impl FnOnce(&LocalSnapshot) -> CloudResult<(LocalMultiWriteTransaction, R)>
        + Send
        + 'static,
    ) -> CloudResult<R> {
        let store = self.ensure_loaded().await?;
        let guard = Arc::clone(&store.writer).lock_owned().await;
        task::spawn_blocking(move || {
            let _guard = guard;
            if store.recover()? || store.needs_reload.load(Ordering::Acquire) {
                store.publish(store.scan()?);
                store.needs_reload.store(false, Ordering::Release);
            }
            let (transaction, result) = prepare(&store.snapshot.load_full())?;
            store.commit(transaction)?;
            Ok(result)
        })
        .await
        .map_err(|error| CloudError::InternalError(error.to_string()))?
    }
}

/// Writes `bytes` to `path` atomically and durably: a sibling temp file,
/// fsync of the temp, then a rename — so a reader sees either the old or the
/// new file, never a half-written one, and a crash after the rename cannot
/// lose the content. On Unix the parent directory is fsynced too so the
/// rename itself is durable; on Windows `std` cannot open a directory handle
/// (that needs `FILE_FLAG_BACKUP_SEMANTICS`), and NTFS journals the rename's
/// metadata, so `File::sync_all` on the temp before the rename is
/// sufficient. A leftover `.tmp` from a crash is ignored by the `.json`-only
/// scan.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    install_atomic(path, bytes)?;
    sync_parent(path)
}

fn install_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("local map path has no parent directory"))?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

fn remove_durable(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match sync_parent(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        },
        Err(error) => Err(error),
    }
}

/// Resolve any atlas-delete journal before rebuilding the in-memory index.
/// Prepared transactions roll back; committed transactions roll forward.
/// A journal is removed only after the selected recovery direction succeeds.
fn recover_atlas_delete_transactions(transactions: &Path, areas: &Path, atlases: &Path) {
    let entries = match fs::read_dir(transactions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            log::warn!(
                "could not scan local map transactions {}: {error}",
                transactions.display()
            );
            return;
        }
    };
    let mut journals = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_transaction = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with(ATLAS_DELETE_TRANSACTION_PREFIX) && name.ends_with(".json")
            });
        if !is_transaction {
            continue;
        }
        let transaction: LocalAtlasDeleteTransaction = match fs::read(&path)
            .map_err(CloudError::from)
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(CloudError::from))
        {
            Ok(transaction) => transaction,
            Err(error) => {
                log::warn!(
                    "could not read local atlas-delete transaction {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        journals.push((path, transaction));
    }
    // Multiple app processes can race the same atlas delete. Once any journal
    // for an atlas reached commit, every sibling journal must roll forward;
    // recovery order must never let an older prepared record resurrect it.
    let committed_atlases: HashSet<AtlasId> = journals
        .iter()
        .filter(|(_, transaction)| transaction.committed)
        .map(|(_, transaction)| transaction.atlas.id)
        .collect();
    for (path, transaction) in journals {
        let committed = committed_atlases.contains(&transaction.atlas.id);
        let recover = || -> CloudResult<()> {
            fs::create_dir_all(areas)?;
            fs::create_dir_all(atlases)?;
            for original in &transaction.members {
                let area_path = areas.join(format!("{}.json", original.details.area.id));
                let mut document = match fs::read(&area_path) {
                    Ok(bytes) => parse_v2_document(&bytes, &area_path)?,
                    // Atlas deletion never removes member areas. A missing
                    // document therefore reflects a newer, independent area
                    // delete and must not be resurrected from the journal.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let membership = document.details.area.atlas_id;
                let target = if committed && membership == Some(transaction.atlas.id) {
                    Some(None)
                } else if !committed && membership.is_none() {
                    Some(Some(transaction.atlas.id))
                } else {
                    None
                };
                // Only undo/finish the membership transition made by this
                // transaction. A later move to another atlas wins, as do all
                // newer room/content edits already present in `document`.
                if let Some(target) = target {
                    document.details.area.atlas_id = target;
                    document.details.area.rev += 1;
                    write_atomic(&area_path, &serde_json::to_vec_pretty(&document)?)?;
                }
            }
            let atlas_path = atlases.join(format!("{}.json", transaction.atlas.id));
            if committed {
                match fs::remove_file(&atlas_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            } else {
                write_atomic(&atlas_path, &serde_json::to_vec_pretty(&transaction.atlas)?)?;
            }
            fs::remove_file(&path)?;
            Ok(())
        };
        if let Err(error) = recover() {
            log::warn!(
                "could not recover local atlas-delete transaction {}: {error}",
                path.display()
            );
        }
    }
}

/// Rolls every multi-write journal forward before the index is rebuilt. A
/// journal exists only once its transaction is fully decided, so recovery
/// has a single direction: re-apply every post-image, remove every deleted
/// area from both namespaces, then retire the journal. Each step is
/// idempotent, so a crash during recovery is finished by the next one.
/// Returns the transactions that were fully rolled forward, oldest first.
///
/// Journals replay in sequence order, the order their transactions
/// committed in (see [`MULTI_WRITE_SEQUENCE`]). Transactions run one at a
/// time under `write_lock`, so each journal holds complete post-images
/// taken after every earlier transaction finished; replaying them in that
/// order lands the newest state last. Two guards cover journals that
/// lingered past their transaction: a post-image whose document on disk is
/// already newer is skipped, so a completed journal whose removal failed
/// cannot revert an edit made after it; and a post-image whose area a later
/// journal in the same pass deletes is skipped, so it cannot resurrect
/// that area even briefly. Deletes always apply: area ids are never reused,
/// so removing an already-removed area is a no-op. A journal that fails to
/// parse is logged and left in place.
fn recover_multi_write_transactions(
    transactions: &Path,
    areas: &Path,
    legacy_areas: &Path,
) -> Vec<LocalMultiWriteTransaction> {
    let mut journals = Vec::new();
    for path in multi_write_journals(transactions) {
        match read_multi_write_journal(&path) {
            Ok(transaction) => journals.push((path, transaction)),
            Err(error) => {
                log::warn!(
                    "could not read local multi-write transaction {}: {error}",
                    path.display()
                );
                return Vec::new();
            }
        }
    }
    let mut recovered = Vec::with_capacity(journals.len());
    for (index, (path, transaction)) in journals.iter().enumerate() {
        let deleted_later: HashSet<AreaId> = journals[index + 1..]
            .iter()
            .flat_map(|(_, later)| later.deletes.iter().copied())
            .collect();
        match roll_multi_write_journal_forward(
            path,
            transaction,
            &deleted_later,
            areas,
            legacy_areas,
        ) {
            Ok(()) => recovered.push(transaction.clone()),
            Err(error) => {
                log::warn!(
                    "could not recover local multi-write transaction {}: {error}",
                    path.display()
                );
                break;
            }
        }
    }
    recovered
}

/// Every multi-write journal under `transactions`, oldest first by
/// sequence. Also raises [`MULTI_WRITE_SEQUENCE`] past every sequence seen,
/// so a journal written later in this process sorts after all of them.
fn multi_write_journals(transactions: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(transactions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            log::warn!(
                "could not scan local map transactions {}: {error}",
                transactions.display()
            );
            return Vec::new();
        }
    };
    let mut journals: Vec<(Option<u64>, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let is_json = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
            is_json
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(MULTI_WRITE_TRANSACTION_PREFIX))
        })
        .map(|path| (multi_write_sequence_of(&path), path))
        .collect();
    journals.sort();
    for (sequence, _) in &journals {
        if let Some(sequence) = sequence {
            MULTI_WRITE_SEQUENCE.fetch_max(*sequence, Ordering::AcqRel);
        }
    }
    journals.into_iter().map(|(_, path)| path).collect()
}

fn read_multi_write_journal(path: &Path) -> CloudResult<LocalMultiWriteTransaction> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Rolls one multi-write journal forward and retires it: every post-image
/// is written unless its area is in `deleted_later` or its document on disk
/// already carries a higher `rev`; every delete removes both namespaces;
/// then the journal file is removed. Any failure leaves the journal in
/// place for the next attempt.
fn roll_multi_write_journal_forward(
    path: &Path,
    transaction: &LocalMultiWriteTransaction,
    deleted_later: &HashSet<AreaId>,
    areas: &Path,
    legacy_areas: &Path,
) -> CloudResult<()> {
    fs::create_dir_all(areas)?;
    for document in &transaction.writes {
        let area_id = document.details.area.id;
        if deleted_later.contains(&area_id) {
            continue;
        }
        let area_path = areas.join(format!("{area_id}.json"));
        let disk_is_newer = !transaction.exact
            && match fs::read(&area_path) {
                Ok(bytes) => v2_document_revision(&bytes, &area_path)? > document.details.area.rev,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
        if !disk_is_newer {
            write_atomic(&area_path, &serde_json::to_vec_pretty(document)?)?;
        }
    }
    for area_id in &transaction.deletes {
        let file = format!("{area_id}.json");
        remove_area_files(&areas.join(&file), &legacy_areas.join(&file))?;
    }
    let atlases = areas
        .parent()
        .expect("area directory has a store root")
        .join("atlases");
    if !transaction.atlases.is_empty() {
        fs::create_dir_all(&atlases)?;
    }
    for atlas in &transaction.atlases {
        write_atomic(
            &atlases.join(format!("{}.json", atlas.id)),
            &serde_json::to_vec_pretty(atlas)?,
        )?;
    }
    for id in &transaction.deleted_atlases {
        let path = atlases.join(format!("{id}.json"));
        remove_durable(&path)?;
    }
    remove_durable(path)?;
    Ok(())
}

/// Removes an area from both namespaces, tolerating an absent file in
/// either. The v1 file must go with the v2 one: deleting only the v2 copy
/// would resurrect the area through the straggler migration on the next
/// scan. Timestamped backups stay; they are recovery, not state.
fn remove_area_files(path: &Path, legacy_path: &Path) -> CloudResult<()> {
    for target in [path, legacy_path] {
        remove_durable(target)?;
    }
    Ok(())
}

/// Journals one transaction record atomically, creating the transactions
/// directory on first use.
#[cfg(test)]
async fn store_transaction_file<T>(path: PathBuf, transaction: T) -> CloudResult<()>
where
    T: Serialize + Send + 'static,
{
    task::spawn_blocking(move || -> CloudResult<()> {
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        write_atomic(&path, &serde_json::to_vec_pretty(&transaction)?)?;
        Ok(())
    })
    .await
    .map_err(|error| CloudError::InternalError(error.to_string()))?
}

#[cfg(test)]
async fn remove_transaction_file(path: PathBuf) -> CloudResult<()> {
    task::spawn_blocking(move || match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    })
    .await
    .map_err(|error| CloudError::InternalError(error.to_string()))?
}

/// Serde probe for the version dispatch: documents that predate the field
/// are v1.
#[derive(Deserialize)]
struct FormatVersionProbe {
    #[serde(default = "probe_v1")]
    format_version: u32,
}

const fn probe_v1() -> u32 {
    1
}

fn document_version(bytes: &[u8]) -> CloudResult<u32> {
    Ok(serde_json::from_slice::<FormatVersionProbe>(bytes)?.format_version)
}

/// Parses bytes from the v2 namespace, refusing anything that is not the
/// current format: a *newer* document is a hard read-only error naming the
/// file (opening it read-write would corrupt data this build cannot
/// represent), and an older one does not belong in `areas-v2/` at all.
fn parse_v2_document(bytes: &[u8], path: &Path) -> CloudResult<LocalAreaDocument> {
    validate_v2_version(document_version(bytes)?, path)?;
    Ok(serde_json::from_slice(bytes)?)
}

/// Revision guards need no room graph or receipt allocations. Serde still
/// consumes the complete JSON and validates the header's format and types.
fn v2_document_revision(bytes: &[u8], path: &Path) -> CloudResult<i64> {
    #[derive(Deserialize)]
    struct Header {
        #[serde(default = "probe_v1")]
        format_version: u32,
        rev: i64,
    }
    let header: Header = serde_json::from_slice(bytes)?;
    validate_v2_version(header.format_version, path)?;
    Ok(header.rev)
}

fn validate_v2_version(version: u32, path: &Path) -> CloudResult<()> {
    if version > crate::AREA_FORMAT_VERSION {
        return Err(CloudError::InvalidInput(format!(
            "local area file {} is format v{version}, newer than this client (max v{}); \
             refusing to open it read-write",
            path.display(),
            crate::AREA_FORMAT_VERSION
        )));
    }
    if version < crate::AREA_FORMAT_VERSION {
        return Err(CloudError::InvalidInput(format!(
            "local area file {} is format v{version} inside the v2 namespace; \
             v1 documents belong in areas/ and migrate from there",
            path.display(),
        )));
    }
    Ok(())
}

/// Migrates one v1-namespace file into the v2 namespace (§8.3): version
/// dispatch, untouched timestamped backup, in-memory migration, and an
/// atomic durable write into `areas-v2/`. The migration is complete only
/// once the rename lands; any failure returns an error naming the file and
/// leaves the v1 source intact — a partial or empty replacement is never
/// opened. A file already in the current format (hand-moved) is adopted
/// verbatim without a backup (nothing is transformed).
fn migrate_legacy_file(
    bytes: &[u8],
    legacy_path: &Path,
    persistence: Option<(&Path, &Path)>,
) -> CloudResult<AreaWithDetails> {
    let version = document_version(bytes).map_err(|err| {
        CloudError::InvalidInput(format!(
            "local area file {} is not a readable area document: {err}",
            legacy_path.display()
        ))
    })?;
    if version > crate::AREA_FORMAT_VERSION {
        return Err(CloudError::InvalidInput(format!(
            "local area file {} is format v{version}, newer than this client (max v{}); \
             refusing to open it read-write",
            legacy_path.display(),
            crate::AREA_FORMAT_VERSION
        )));
    }

    let details = if version == crate::AREA_FORMAT_VERSION {
        serde_json::from_slice::<AreaWithDetails>(bytes).map_err(|err| {
            CloudError::InvalidInput(format!(
                "local area file {} claims v{version} but does not parse: {err}",
                legacy_path.display()
            ))
        })?
    } else {
        let legacy: local_migration::LegacyAreaV1 =
            serde_json::from_slice(bytes).map_err(|err| {
                CloudError::InvalidInput(format!(
                    "local area file {} does not parse as a v1 document: {err}",
                    legacy_path.display()
                ))
            })?;

        if let Some((_, backup_dir)) = persistence {
            // Backup BEFORE anything else can go wrong: the untouched v1 bytes,
            // timestamped so repeated attempts never clobber an older backup.
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let backup_path = backup_dir.join(format!("{}.{timestamp}.json", legacy.area.id));
            fs::create_dir_all(backup_dir)
                .and_then(|()| fs::write(&backup_path, bytes))
                .map_err(|err| {
                    CloudError::InternalError(format!(
                        "cannot back up {} before migration (leaving the v1 file untouched): {err}",
                        legacy_path.display()
                    ))
                })?;
        }

        local_migration::migrate_v1(legacy)
    };

    let Some((v2_path, _)) = persistence else {
        return Ok(details);
    };
    // The atomic durable write into areas-v2 is what completes the
    // migration; on failure the v1 source stays authoritative.
    if let Some(parent) = v2_path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_atomic(v2_path, &serde_json::to_vec_pretty(&details)?).map_err(|err| {
        CloudError::InternalError(format!(
            "migrating {} failed while writing {} (the v1 file is untouched): {err}",
            legacy_path.display(),
            v2_path.display()
        ))
    })?;
    log::info!(
        "migrated local area {} ({}) from {} to {}",
        details.area.name,
        details.area.id,
        legacy_path.display(),
        v2_path.display()
    );
    Ok(details)
}

/// Reads every `*.json` under the v2 namespace as an [`AreaWithDetails`],
/// keyed by id, then **eagerly migrates stragglers** from the v1 namespace
/// (documented choice — a freshly-opened store lists every area
/// consistently instead of surfacing v1 areas one `get_area` at a time).
/// Once an id has a v2 copy its v1 file is never re-read; a failed
/// migration is reported and skipped, leaving the v1 file intact.
fn scan_areas(
    dir: &Path,
    legacy_dir: &Path,
    backup_dir: Option<&Path>,
) -> CloudResult<HashMap<AreaId, LocalAreaDocument>> {
    let mut out: HashMap<AreaId, LocalAreaDocument> = HashMap::new();
    let mut migrated: HashSet<AreaId> = HashSet::new();
    let scanned = read_json_dir(dir, |bytes, path| match parse_v2_document(bytes, path) {
        Ok(document) => Some((document.details.area.id, document)),
        Err(err) => {
            log::warn!("skipping local map file: {err}");
            None
        }
    })?;
    for (id, area) in scanned {
        migrated.insert(id);
        out.insert(id, area);
    }

    let entries = match fs::read_dir(legacy_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        let is_json = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        // Cheap skip: a well-named `<uuid>.json` whose id already has a v2
        // copy is not even read.
        let stem_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| Uuid::parse_str(stem).ok())
            .map(AreaId);
        if stem_id.is_some_and(|id| migrated.contains(&id)) {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!(
                    "skipping unreadable local map file {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        // The embedded id is the authority (a mis-named file still checks
        // against the v2 set before migrating).
        let id = match serde_json::from_slice::<AreaIdProbe>(&bytes) {
            Ok(probe) => AreaId(probe.id),
            Err(err) => {
                log::warn!(
                    "skipping unreadable local map file {}: {err}",
                    path.display()
                );
                continue;
            }
        };
        if migrated.contains(&id) {
            continue;
        }
        let v2_path = dir.join(format!("{id}.json"));
        match migrate_legacy_file(
            &bytes,
            &path,
            backup_dir.map(|backup| (v2_path.as_path(), backup)),
        ) {
            Ok(details) => {
                migrated.insert(id);
                out.insert(details.area.id, LocalAreaDocument::new(details));
            }
            Err(err) => log::warn!("local map migration failed: {err}"),
        }
    }
    Ok(out)
}

/// Serde probe for a document's area id.
#[derive(Deserialize)]
struct AreaIdProbe {
    id: Uuid,
}

/// Reads every `*.json` under `dir` as an [`Atlas`] manifest, keyed by id.
fn scan_atlases(dir: &Path) -> CloudResult<HashMap<AtlasId, Atlas>> {
    read_json_dir(dir, |bytes, path| {
        let parsed = serde_json::from_slice::<Atlas>(bytes).ok();
        if parsed.is_none() {
            log::warn!("skipping unreadable local map file {}", path.display());
        }
        parsed.map(|atlas| (atlas.id, atlas))
    })
}

fn read_json_dir<K, V>(
    dir: &Path,
    parse: impl Fn(&[u8], &Path) -> Option<(K, V)>,
) -> CloudResult<HashMap<K, V>>
where
    K: std::hash::Hash + Eq,
{
    let mut out = HashMap::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        let is_json = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!("Could not read local map file {}: {error}", path.display());
                continue;
            }
        };
        if let Some((key, value)) = parse(&bytes, &path) {
            out.insert(key, value);
        }
    }
    Ok(out)
}

#[async_trait]
impl MapperBackend for LocalBackend {
    fn has_credential(&self) -> bool {
        true
    }

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        self.transact(move |_| {
            let area = Area {
                id: AreaId(Uuid::new_v4()),
                user_id: None,
                atlas_id: request.atlas_id,
                atlas_name: None,
                name: request.name,
                created_at: Utc::now(),
                rev: 1,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
            };
            let details = AreaWithDetails {
                area: area.clone(),
                format_version: crate::AREA_FORMAT_VERSION,
                content_hash: None,
                properties: Vec::new(),
                rooms: Vec::new(),
                labels: Vec::new(),
                shapes: Vec::new(),
                connections: Vec::new(),
                linked_areas: Vec::new(),
            };
            Ok((
                LocalMultiWriteTransaction {
                    writes: vec![LocalAreaDocument::new(details)],
                    exact: true,
                    ..Default::default()
                },
                area,
            ))
        })
        .await
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<Area> {
        if storage != MapStorage::Local {
            return Err(CloudError::InvalidInput(format!(
                "the local backend cannot create a {storage} map"
            )));
        }
        self.create_area(request).await
    }

    async fn import_local_area(&self, details: AreaWithDetails) -> CloudResult<()> {
        self.transact(move |_| {
            Ok((
                LocalMultiWriteTransaction {
                    writes: vec![LocalAreaDocument::new(details)],
                    exact: true,
                    ..Default::default()
                },
                (),
            ))
        })
        .await
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        Ok(self
            .snapshot()
            .await?
            .areas()
            .map(|details| details.area.clone())
            .collect())
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.snapshot()
            .await?
            .area(*area_id)
            .cloned()
            .map_err(|error| match error {
                CloudError::AreaNotFound(_) => CloudError::NotFoundOrNoAccess,
                other => other,
            })
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        let id = *area_id;
        self.transact(move |snapshot| {
            let mut document = snapshot.document(id)?;
            if let Some(name) = updates.name {
                document.details.area.name = name;
            }
            if let Some(atlas_id) = updates.atlas_id {
                document.details.area.atlas_id = atlas_id;
            }
            document.details.area.rev += 1;
            Ok((
                LocalMultiWriteTransaction {
                    writes: vec![document],
                    ..Default::default()
                },
                (),
            ))
        })
        .await
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        let id = *area_id;
        self.transact(move |_| {
            Ok((
                LocalMultiWriteTransaction {
                    deletes: vec![id],
                    ..Default::default()
                },
                (),
            ))
        })
        .await
    }

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        let id = *area_id;
        let envelope = envelope.clone();
        self.transact(move |snapshot| {
            let mut document = snapshot.document(id)?;
            if let Some(result) = document.receipt(envelope.operation_id) {
                return Ok((LocalMultiWriteTransaction::default(), result));
            }
            let result = area_edits::apply_envelope(&mut document.details, id, &envelope)?;
            document.content_identity = Arc::new(());
            document.remember(result.clone());
            Ok((
                LocalMultiWriteTransaction {
                    writes: vec![document],
                    ..Default::default()
                },
                result,
            ))
        })
        .await
    }

    async fn merge_areas(&self, plan: &AreaMergePlan) -> CloudResult<AreaMergeCommit> {
        let plan = plan.clone();
        self.transact(move |snapshot| {
            let LocalAreaDocument {
                details: mut into,
                applied_operations: into_receipts,
                ..
            } = snapshot.document(plan.into)?;
            let mut sources = Vec::with_capacity(plan.sources.len());
            let mut source_receipts = Vec::with_capacity(plan.sources.len());
            for source in &plan.sources {
                let document = snapshot.document(source.id)?;
                sources.push(document.details);
                source_receipts.push(document.applied_operations);
            }
            let mut inbound = Vec::with_capacity(plan.inbound.len());
            let mut inbound_receipts = Vec::with_capacity(plan.inbound.len());
            for id in &plan.inbound {
                let document = snapshot.document(*id)?;
                inbound.push(document.details);
                inbound_receipts.push(document.applied_operations);
            }

            let outcome = apply_area_merge(&plan, &mut into, &mut sources, &mut inbound)?;

            // Only documents the applier reports as changed are rewritten; a
            // third party none of whose exits named a source keeps its bytes,
            // and a whole source is deleted rather than written.
            let mut post_images: HashMap<AreaId, LocalAreaDocument> =
                HashMap::with_capacity(1 + sources.len() + inbound.len());
            post_images.insert(
                into.area.id,
                LocalAreaDocument {
                    content_identity: Arc::new(()),
                    details: into,
                    applied_operations: into_receipts,
                },
            );
            let kept_sources = sources
                .into_iter()
                .zip(source_receipts)
                .filter(|(details, _)| {
                    plan.sources
                        .iter()
                        .any(|source| source.id == details.area.id && source.is_partial())
                });
            for (details, applied_operations) in
                kept_sources.chain(inbound.into_iter().zip(inbound_receipts))
            {
                post_images.insert(
                    details.area.id,
                    LocalAreaDocument {
                        content_identity: Arc::new(()),
                        details,
                        applied_operations,
                    },
                );
            }
            let writes: Vec<LocalAreaDocument> = outcome
                .versions
                .iter()
                .filter(|version| !version.deleted)
                .filter_map(|version| post_images.remove(&AreaId(version.id)))
                .collect();
            let deletes = plan.deleted_areas();
            let documents: Vec<AreaWithDetails> = writes
                .iter()
                .map(|document| document.details.clone())
                .collect();
            Ok((
                LocalMultiWriteTransaction {
                    writes,
                    deletes,
                    ..Default::default()
                },
                AreaMergeCommit { outcome, documents },
            ))
        })
        .await
    }

    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        let snapshot = self.snapshot().await?;
        let mut items: Vec<_> = snapshot
            .atlases()
            .map(|atlas| AtlasListItem {
                id: atlas.id,
                name: atlas.name.clone(),
                created_at: atlas.created_at,
                rev: atlas.rev,
                area_count: i64::try_from(
                    snapshot
                        .areas()
                        .filter(|details| details.area.atlas_id == Some(atlas.id))
                        .count(),
                )
                .unwrap_or(i64::MAX),
                is_owner: true,
                can_admin: true,
                owner_nickname: None,
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(items)
    }

    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        let atlas = Atlas {
            id: AtlasId(Uuid::new_v4()),
            user_id: None,
            name: name.to_string(),
            created_at: Utc::now(),
            rev: 1,
        };
        self.transact(move |_| {
            Ok((
                LocalMultiWriteTransaction {
                    atlases: vec![atlas.clone()],
                    ..Default::default()
                },
                atlas,
            ))
        })
        .await
    }

    async fn create_atlas_at(&self, name: &str, storage: MapStorage) -> CloudResult<Atlas> {
        if storage != MapStorage::Local {
            return Err(CloudError::InvalidInput(format!(
                "the local backend cannot create a {storage} atlas"
            )));
        }
        self.create_atlas(name).await
    }

    async fn rename_atlas(&self, atlas_id: &AtlasId, name: &str) -> CloudResult<Atlas> {
        let id = *atlas_id;
        let name = name.to_string();
        self.transact(move |snapshot| {
            let mut atlas = snapshot
                .atlases
                .get(&id)
                .cloned()
                .ok_or(CloudError::NotFoundOrNoAccess)?;
            atlas.name = name;
            atlas.rev += 1;
            Ok((
                LocalMultiWriteTransaction {
                    atlases: vec![atlas.clone()],
                    ..Default::default()
                },
                atlas,
            ))
        })
        .await
    }

    async fn delete_atlas(&self, atlas_id: &AtlasId) -> CloudResult<()> {
        let id = *atlas_id;
        self.transact(move |snapshot| {
            if !snapshot.atlases.contains_key(&id) {
                return Err(CloudError::NotFoundOrNoAccess);
            }
            let writes = snapshot
                .documents
                .values()
                .filter(|document| document.details.area.atlas_id == Some(id))
                .map(|document| {
                    let mut detached = document.as_ref().clone();
                    detached.details.area.atlas_id = None;
                    detached.details.area.rev += 1;
                    detached
                })
                .collect();
            Ok((
                LocalMultiWriteTransaction {
                    writes,
                    deleted_atlases: vec![id],
                    ..Default::default()
                },
                (),
            ))
        })
        .await
    }

    fn local_atlas_ids(&self) -> HashSet<AtlasId> {
        self.local_snapshot()
            .map_or_else(HashSet::new, |snapshot| snapshot.atlas_ids().collect())
    }

    fn local_area_ids(&self) -> HashSet<AreaId> {
        self.local_snapshot().map_or_else(HashSet::new, |snapshot| {
            snapshot.documents.keys().copied().collect()
        })
    }

    fn local_snapshot(&self) -> Option<Arc<LocalSnapshot>> {
        self.store
            .get()
            .filter(|store| store.initialized.load(Ordering::Acquire))
            .map(|store| store.snapshot.load_full())
    }

    async fn subscribe_local(&self) -> CloudResult<Option<tokio::sync::watch::Receiver<u64>>> {
        Ok(Some(self.ensure_loaded().await?.changed.subscribe()))
    }

    async fn refresh_local(&self) -> CloudResult<()> {
        self.refresh().await
    }
}

#[cfg(test)]
impl LocalBackend {
    /// Exercise recovery of single-document journals created by older versions.
    pub(crate) async fn journal_single_writes_for_test(&self) {
        self.ensure_loaded()
            .await
            .unwrap()
            .legacy_single_journals
            .store(true, Ordering::Release);
    }
    async fn lock_store(&self) -> CloudResult<tokio::sync::OwnedMutexGuard<()>> {
        Ok(self
            .ensure_loaded()
            .await?
            .writer
            .clone()
            .lock_owned()
            .await)
    }
    fn areas_dir(&self) -> PathBuf {
        self.root.join("areas-v2")
    }
    fn legacy_areas_dir(&self) -> PathBuf {
        self.root.join("areas")
    }
    fn atlases_dir(&self) -> PathBuf {
        self.root.join("atlases")
    }
    fn transactions_dir(&self) -> PathBuf {
        self.root.join("transactions")
    }
    fn area_path(&self, id: AreaId) -> PathBuf {
        self.areas_dir().join(format!("{id}.json"))
    }
    fn legacy_area_path(&self, id: AreaId) -> PathBuf {
        self.legacy_areas_dir().join(format!("{id}.json"))
    }
    fn atlas_path(&self, id: AtlasId) -> PathBuf {
        self.atlases_dir().join(format!("{id}.json"))
    }
    fn atlas_delete_transaction_path(&self, id: Uuid) -> PathBuf {
        self.transactions_dir()
            .join(format!("{ATLAS_DELETE_TRANSACTION_PREFIX}{id}.json"))
    }
    fn multi_write_transaction_path(&self, sequence: u64, id: Uuid) -> PathBuf {
        self.transactions_dir().join(format!(
            "{MULTI_WRITE_TRANSACTION_PREFIX}{sequence:020}-{id}.json"
        ))
    }
    async fn reload(&self) -> CloudResult<()> {
        self.refresh().await
    }
    async fn load_area_document(&self, id: AreaId) -> CloudResult<LocalAreaDocument> {
        self.snapshot().await?.document(id)
    }
    async fn load_area(&self, id: AreaId) -> CloudResult<AreaWithDetails> {
        self.get_area(&id).await
    }
    // Fixture helpers: callers serialize these operations with lock_store.
    async fn store_area_document(&self, document: LocalAreaDocument) -> CloudResult<()> {
        self.ensure_loaded()
            .await?
            .commit(LocalMultiWriteTransaction {
                writes: vec![document],
                ..Default::default()
            })
    }
    async fn commit_documents(
        &self,
        writes: Vec<LocalAreaDocument>,
        deletes: Vec<AreaId>,
    ) -> CloudResult<()> {
        let store = self.ensure_loaded().await?;
        if store.recover()? {
            store.publish(store.scan()?);
        }
        store.commit(LocalMultiWriteTransaction {
            writes,
            deletes,
            ..Default::default()
        })
    }
    async fn apply_committed_documents(
        &self,
        transaction: &LocalMultiWriteTransaction,
    ) -> CloudResult<()> {
        self.ensure_loaded().await?.commit(transaction.clone())
    }
    fn headers(&self) -> HashMap<AreaId, Area> {
        self.local_snapshot()
            .unwrap()
            .areas()
            .map(|details| (details.area.id, details.area.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ExitArgs, ExitDirection, ExitId, LabelArgs, LabelId, RoomNumber, RoomUpdates, ShapeArgs,
        ShapeId,
        backends::{AreaMergeSource, RoomRemap, Translate},
        mapper::RoomKey,
        mutation::{AreaMutation, OpResult, Precondition, ResourceKind},
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn failed_retirement_without_a_journal_reloads_before_the_next_write() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        backend.journal_single_writes_for_test().await;
        let area = backend
            .create_area(new_area_request("Original", None))
            .await
            .unwrap();
        backend
            .ensure_loaded()
            .await
            .unwrap()
            .retirement_faults
            .store(2, Ordering::Release);
        let result = backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: Some("Committed".into()),
                    atlas_id: None,
                },
            )
            .await;
        assert!(matches!(result, Err(CloudError::LocalCommitPending { .. })));
        assert_eq!(
            backend.get_area(&area.id).await.unwrap().area.name,
            "Original"
        );
        assert_eq!(fs::read_dir(root.join("transactions")).unwrap().count(), 0);
        backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: None,
                    atlas_id: None,
                },
            )
            .await
            .unwrap();
        let latest = backend.get_area(&area.id).await.unwrap();
        assert_eq!(latest.area.name, "Committed");
        assert_eq!(latest.area.rev, 3);
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn ordinary_commit_preserves_a_newer_file_until_explicit_refresh() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Original", None))
            .await
            .unwrap();
        let mut newer = backend.get_area(&area.id).await.unwrap();
        newer.area.name = "External revision".into();
        newer.area.rev += 10;
        let bytes = serde_json::to_vec(&LocalAreaDocument::new(newer)).unwrap();
        fs::write(backend.area_path(area.id), &bytes).unwrap();
        let before = backend.snapshot().await.unwrap();
        let result = backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: Some("Stale overwrite".into()),
                    atlas_id: None,
                },
            )
            .await;
        assert!(matches!(result, Err(CloudError::InvalidInput(_))));
        assert_eq!(fs::read(backend.area_path(area.id)).unwrap(), bytes);
        assert!(Arc::ptr_eq(&before, &backend.snapshot().await.unwrap()));
        assert!(multi_write_journals(&backend).is_empty());
        backend.refresh().await.unwrap();
        assert_eq!(backend.get_area(&area.id).await.unwrap().area.rev, 11);
        backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: Some("After refresh".into()),
                    atlas_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(backend.get_area(&area.id).await.unwrap().area.rev, 12);
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn replacing_an_import_keeps_disk_and_snapshot_identical() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Original", None))
            .await
            .unwrap();
        let mut imported = backend.get_area(&area.id).await.unwrap();
        backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: Some("Revision two".into()),
                    atlas_id: None,
                },
            )
            .await
            .unwrap();
        imported.area.name = "Explicit replacement at revision one".into();
        backend.import_local_area(imported.clone()).await.unwrap();
        let before = serde_json::to_value(backend.get_area(&area.id).await.unwrap()).unwrap();
        backend.refresh().await.unwrap();
        assert_eq!(before, serde_json::to_value(imported).unwrap());
        assert_eq!(
            before,
            serde_json::to_value(backend.get_area(&area.id).await.unwrap()).unwrap()
        );
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn failed_directory_refresh_preserves_the_committed_generation() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Keep me", None))
            .await
            .unwrap();
        let before = backend.snapshot().await.unwrap();
        let saved = root.join("saved-areas");
        fs::rename(backend.areas_dir(), &saved).unwrap();
        fs::write(backend.areas_dir(), b"not a directory").unwrap();
        assert!(backend.refresh().await.is_err());
        assert!(Arc::ptr_eq(&before, &backend.snapshot().await.unwrap()));
        assert_eq!(
            backend.get_area(&area.id).await.unwrap().area.name,
            "Keep me"
        );
        fs::remove_file(backend.areas_dir()).unwrap();
        fs::rename(saved, backend.areas_dir()).unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn recovery_followed_by_scan_failure_must_reload_before_the_next_writer() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (writes, deletes) = merge_shaped(&documents);
        let removed = deletes[0];
        let destination = writes[0].details.area.id;
        let journal =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(
            journal.clone(),
            LocalMultiWriteTransaction {
                writes: writes.clone(),
                deletes,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        fs::write(backend.atlases_dir(), b"obstruct scanning after recovery").unwrap();
        assert!(backend.refresh().await.is_err());
        assert!(
            !journal.exists(),
            "disk recovery finished before the failed scan"
        );
        fs::remove_file(backend.atlases_dir()).unwrap();
        backend
            .update_area(
                &destination,
                AreaUpdates {
                    name: Some("Later rename".into()),
                    atlas_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            backend.get_area(&destination).await.unwrap().area.rev,
            writes[0].details.area.rev + 1
        );
        assert_eq!(
            backend
                .get_area(&writes[1].details.area.id)
                .await
                .unwrap()
                .area
                .name,
            writes[1].details.area.name
        );
        assert!(backend.get_area(&removed).await.is_err());
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn failed_legacy_recovery_retains_later_delete_journals() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (writes, deletes) = merge_shaped(&documents);
        let removed_id = writes[0].details.area.id;
        let early =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        let later =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(
            early.clone(),
            LocalMultiWriteTransaction {
                writes: writes.clone(),
                deletes,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        store_transaction_file(
            later.clone(),
            LocalMultiWriteTransaction {
                deletes: vec![removed_id],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let obstruction = backend.area_path(writes[1].details.area.id);
        fs::remove_file(&obstruction).unwrap();
        fs::create_dir(&obstruction).unwrap();
        let before = backend.snapshot().await.unwrap();
        assert!(backend.refresh().await.is_err());
        assert!(Arc::ptr_eq(&before, &backend.snapshot().await.unwrap()));
        assert!(early.exists() && later.exists());
        drop(backend);
        let backend = LocalBackend::new(&root);
        let blocked = backend.snapshot().await.unwrap();
        assert!(blocked.recovery_error.is_some());
        for id in writes
            .iter()
            .map(|d| d.details.area.id)
            .chain(documents.iter().map(|d| d.details.area.id))
        {
            assert!(blocked.contains_area(id));
            assert!(
                blocked.area(id).is_err(),
                "an unfinished merge must not expose partial results"
            );
        }
        fs::remove_dir(obstruction).unwrap();
        backend.refresh().await.unwrap();
        assert!(backend.get_area(&removed_id).await.is_err());
        assert!(!backend.area_path(removed_id).exists());
        assert!(!early.exists() && !later.exists());
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn generations_share_unchanged_documents_and_coalesce_notifications() {
        let root = temp_root();
        let first = LocalBackend::new(&root);
        let documents = three_areas(&first).await;
        let second = LocalBackend::new(root.join("."));
        let mut changed = second.subscribe_local().await.unwrap().unwrap();
        let before = second.snapshot().await.unwrap();
        let changed_id = documents[0].details.area.id;
        let unchanged_id = documents[1].details.area.id;
        for name in ["One", "Two", "Latest"] {
            first
                .update_area(
                    &changed_id,
                    AreaUpdates {
                        name: Some(name.into()),
                        atlas_id: None,
                    },
                )
                .await
                .unwrap();
        }
        changed.changed().await.unwrap();
        let after = second.snapshot().await.unwrap();
        assert_eq!(*changed.borrow_and_update(), after.generation);
        assert_eq!(after.generation, before.generation + 3);
        assert_eq!(after.area(changed_id).unwrap().area.name, "Latest");
        assert_eq!(before.area(changed_id).unwrap().area.rev, 1);
        assert!(Arc::ptr_eq(
            &before.documents[&unchanged_id],
            &after.documents[&unchanged_id]
        ));
        assert!(!Arc::ptr_eq(
            &before.documents[&changed_id],
            &after.documents[&changed_id]
        ));
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn cancelled_caller_does_not_release_the_transaction_owner() {
        let root = temp_root();
        let backend = Arc::new(LocalBackend::new(&root));
        let area = backend
            .create_area(new_area_request("Original", None))
            .await
            .unwrap();
        let old = backend.snapshot().await.unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let caller = {
            let backend = backend.clone();
            tokio::spawn(async move {
                backend
                    .transact(move |snapshot| {
                        entered.send(()).unwrap();
                        resume.recv().unwrap();
                        let mut document = snapshot.document(area.id)?;
                        document.details.area.name = "Committed after cancellation".into();
                        document.details.area.rev += 1;
                        Ok((
                            LocalMultiWriteTransaction {
                                writes: vec![document],
                                ..Default::default()
                            },
                            (),
                        ))
                    })
                    .await
            })
        };
        started.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let following = backend.update_area(
            &area.id,
            AreaUpdates {
                name: Some("Following writer".into()),
                atlas_id: None,
            },
        );
        tokio::pin!(following);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut following)
                .await
                .is_err()
        );
        assert_eq!(
            backend.get_area(&area.id).await.unwrap().area.name,
            "Original"
        );
        release.send(()).unwrap();
        following.await.unwrap();
        assert_eq!(backend.get_area(&area.id).await.unwrap().area.rev, 3);
        assert_eq!(old.area(area.id).unwrap().area.name, "Original");
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn single_save_recovers_an_installed_file_after_directory_sync_failure() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Original", None))
            .await
            .unwrap();
        backend
            .store
            .get()
            .unwrap()
            .retirement_faults
            .store(1, Ordering::Release);
        let result = backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: Some("Installed".into()),
                    atlas_id: None,
                },
            )
            .await;
        assert!(matches!(result, Err(CloudError::LocalCommitPending { .. })));
        assert!(!backend.transactions_dir().exists());
        assert_eq!(
            backend.get_area(&area.id).await.unwrap().area.name,
            "Original"
        );
        backend.refresh().await.unwrap();
        assert_eq!(
            backend.get_area(&area.id).await.unwrap().area.name,
            "Installed"
        );
        assert!(backend.snapshot().await.unwrap().recovery_error.is_none());
        fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn single_save_needs_no_journal_and_missing_files_are_conflicts() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Original", None))
            .await
            .unwrap();
        assert!(!backend.transactions_dir().exists());
        backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: Some("Saved".into()),
                    atlas_id: None,
                },
            )
            .await
            .unwrap();
        assert!(
            !backend.transactions_dir().exists(),
            "single saves never create a store journal"
        );
        let before = backend.snapshot().await.unwrap();
        fs::remove_file(backend.area_path(area.id)).unwrap();
        let result = backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: Some("Must not resurrect".into()),
                    atlas_id: None,
                },
            )
            .await;
        assert!(matches!(result, Err(CloudError::InvalidInput(_))));
        assert!(!backend.area_path(area.id).exists());
        assert!(Arc::ptr_eq(&before, &backend.snapshot().await.unwrap()));
        fs::remove_dir_all(root).ok();
    }

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("smudgy-local-test-{}", Uuid::new_v4()))
    }

    fn new_area_request(name: &str, atlas_id: Option<AtlasId>) -> CreateAreaRequest {
        CreateAreaRequest {
            name: name.to_string(),
            atlas_id,
            ephemeral: false,
        }
    }

    fn envelope(
        area_id: AreaId,
        expected_rev: i64,
        payload: Vec<AreaMutation>,
    ) -> MutationEnvelope {
        MutationEnvelope {
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: area_id.0,
                expected_rev,
                access_fingerprint: None,
            }],
            payload,
        }
    }

    #[tokio::test]
    async fn explicit_creation_rejects_a_foreign_tier() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        assert!(matches!(
            backend
                .create_area_at(new_area_request("Wrong tier", None), MapStorage::Cloud)
                .await,
            Err(CloudError::InvalidInput(_))
        ));
        assert!(backend.list_areas().await.expect("list").is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn create_list_get_roundtrip_and_persists_across_instances() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);

        let area = backend
            .create_area(new_area_request("Cellars", None))
            .await
            .expect("create");
        assert_eq!(area.name, "Cellars");
        assert!(area.effective_access().is_owner, "local areas are owned");

        let listed = backend.list_areas().await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, area.id);

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.area.name, "Cellars");
        assert!(details.rooms.is_empty());

        // A fresh backend on the same root lazily loads the persisted area —
        // the bytes are authoritative on disk, not just in memory.
        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        let listed = reopened.list_areas().await.expect("reopened list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, area.id);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn atlas_list_counts_members_and_gentle_delete_orphans_them() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);

        let atlas = backend.create_atlas("Old Roads").await.expect("atlas");
        let area = backend
            .create_area(new_area_request("A", Some(atlas.id)))
            .await
            .expect("area");

        let atlases = backend.list_atlases().await.expect("list atlases");
        assert_eq!(atlases.len(), 1);
        assert_eq!(atlases[0].name, "Old Roads");
        assert_eq!(atlases[0].area_count, 1, "members are counted");

        // Gentle delete: the atlas is gone but its area survives as loose.
        backend.delete_atlas(&atlas.id).await.expect("delete atlas");
        assert!(backend.list_atlases().await.expect("list").is_empty());
        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.area.atlas_id, None, "member became loose");

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn prepared_atlas_delete_journal_rolls_back_on_reopen() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let atlas = backend.create_atlas("Old Roads").await.expect("atlas");
        let area = backend
            .create_area(new_area_request("A", Some(atlas.id)))
            .await
            .expect("area");
        let original = backend
            .load_area_document(area.id)
            .await
            .expect("original document");
        let transaction = LocalAtlasDeleteTransaction {
            atlas: atlas.clone(),
            members: vec![original.clone()],
            committed: false,
        };
        let transaction_path = backend.atlas_delete_transaction_path(Uuid::new_v4());
        store_transaction_file(transaction_path.clone(), transaction)
            .await
            .expect("prepared journal");

        // Simulate a crash after the member was detached and the atlas file
        // removed, but before the transaction's commit marker was durable.
        let mut detached = original.clone();
        detached.details.area.atlas_id = None;
        detached.details.area.name = "Edited while recovery was pending".to_string();
        detached.details.area.rev += 1;
        backend
            .store_area_document(detached)
            .await
            .expect("partial detach");
        fs::remove_file(backend.atlas_path(atlas.id)).expect("partial atlas delete");
        drop(backend);

        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        let atlases = reopened.list_atlases().await.expect("recover and list");
        assert_eq!(atlases.len(), 1);
        assert_eq!(atlases[0].id, atlas.id);
        let recovered = reopened.get_area(&area.id).await.expect("recovered area");
        assert_eq!(recovered.area.atlas_id, Some(atlas.id));
        assert_eq!(recovered.area.name, "Edited while recovery was pending");
        assert_eq!(recovered.area.rev, original.details.area.rev + 2);
        assert!(!transaction_path.exists(), "recovery retires the journal");

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn committed_atlas_delete_journal_rolls_forward_on_reopen() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let atlas = backend.create_atlas("Old Roads").await.expect("atlas");
        let area = backend
            .create_area(new_area_request("A", Some(atlas.id)))
            .await
            .expect("area");
        let original = backend
            .load_area_document(area.id)
            .await
            .expect("original document");
        let transaction = LocalAtlasDeleteTransaction {
            atlas: atlas.clone(),
            members: vec![original.clone()],
            committed: true,
        };
        let transaction_path = backend.atlas_delete_transaction_path(Uuid::new_v4());
        store_transaction_file(transaction_path.clone(), transaction)
            .await
            .expect("committed journal");
        drop(backend);

        // Even if the process stopped immediately after the commit marker,
        // reopening deterministically finishes every member detach and delete.
        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        assert!(
            reopened
                .list_atlases()
                .await
                .expect("recover and list")
                .is_empty()
        );
        let recovered = reopened.get_area(&area.id).await.expect("surviving area");
        assert_eq!(recovered.area.atlas_id, None);
        assert_eq!(recovered.area.rev, original.details.area.rev + 1);
        assert!(!transaction_path.exists(), "recovery retires the journal");

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn atlas_delete_recovery_preserves_a_newer_move_to_another_atlas() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let source = backend
            .create_atlas("Old Roads")
            .await
            .expect("source atlas");
        let destination = backend
            .create_atlas("New Roads")
            .await
            .expect("destination atlas");
        let area = backend
            .create_area(new_area_request("A", Some(source.id)))
            .await
            .expect("area");
        let original = backend
            .load_area_document(area.id)
            .await
            .expect("original document");
        let transaction = LocalAtlasDeleteTransaction {
            atlas: source.clone(),
            members: vec![original.clone()],
            committed: false,
        };
        let transaction_path = backend.atlas_delete_transaction_path(Uuid::new_v4());
        store_transaction_file(transaction_path.clone(), transaction)
            .await
            .expect("prepared journal");

        // A different writer moves the member after the delete began. The
        // prepared rollback owns only the None -> source transition, not this
        // newer placement, and must not overwrite it with the journal copy.
        let mut moved = original;
        moved.details.area.atlas_id = Some(destination.id);
        moved.details.area.name = "Moved and edited".to_string();
        moved.details.area.rev += 1;
        backend
            .store_area_document(moved)
            .await
            .expect("newer move");
        fs::remove_file(backend.atlas_path(source.id)).expect("partial atlas delete");
        drop(backend);

        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        reopened.list_atlases().await.expect("recover and list");
        let recovered = reopened.get_area(&area.id).await.expect("surviving area");
        assert_eq!(recovered.area.atlas_id, Some(destination.id));
        assert_eq!(recovered.area.name, "Moved and edited");
        assert!(!transaction_path.exists(), "recovery retires the journal");

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn committed_sibling_forces_a_prepared_atlas_delete_forward() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let atlas = backend.create_atlas("Old Roads").await.expect("atlas");
        let area = backend
            .create_area(new_area_request("A", Some(atlas.id)))
            .await
            .expect("area");
        let original = backend
            .load_area_document(area.id)
            .await
            .expect("original document");
        let prepared_path = backend.atlas_delete_transaction_path(Uuid::new_v4());
        let committed_path = backend.atlas_delete_transaction_path(Uuid::new_v4());
        for (path, committed) in [(&prepared_path, false), (&committed_path, true)] {
            store_transaction_file(
                path.to_path_buf(),
                LocalAtlasDeleteTransaction {
                    atlas: atlas.clone(),
                    members: vec![original.clone()],
                    committed,
                },
            )
            .await
            .expect("journal");
        }
        drop(backend);

        // Recovery order is filesystem-dependent. The committed sibling is
        // collected first so an older prepared record can never resurrect the
        // atlas regardless of which journal is visited first.
        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        assert!(
            reopened
                .list_atlases()
                .await
                .expect("recover and list")
                .is_empty()
        );
        let recovered = reopened.get_area(&area.id).await.expect("surviving area");
        assert_eq!(recovered.area.atlas_id, None);
        assert!(!prepared_path.exists());
        assert!(!committed_path.exists());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn move_between_atlases_via_update_area() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let atlas = backend.create_atlas("Folder").await.expect("atlas");
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");

        backend
            .move_area_to_atlas(&area.id, Some(atlas.id))
            .await
            .expect("move in");
        assert_eq!(
            backend.get_area(&area.id).await.unwrap().area.atlas_id,
            Some(atlas.id)
        );

        backend
            .move_area_to_atlas(&area.id, None)
            .await
            .expect("move out");
        assert_eq!(
            backend.get_area(&area.id).await.unwrap().area.atlas_id,
            None
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn mutations_bump_rev_and_room_exit_persist() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");
        assert_eq!(area.rev, 1);

        let exit_id = ExitId(Uuid::new_v4());
        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    1,
                    vec![
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(1),
                            body: RoomUpdates {
                                title: Some("Hall".to_string()),
                                ..RoomUpdates::default()
                            },
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(1),
                            body: ExitArgs {
                                id: Some(exit_id),
                                from_direction: ExitDirection::North,
                                ..ExitArgs::default()
                            },
                        },
                    ],
                ),
            )
            .await
            .expect("seed envelope");

        let details = backend.get_area(&area.id).await.expect("get");
        assert!(details.area.rev > 1, "mutations bump rev");
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].title, "Hall");
        assert_eq!(details.rooms[0].exits.len(), 1);
        assert_eq!(details.rooms[0].exits[0].id, exit_id);

        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    details.area.rev,
                    vec![AreaMutation::DeleteExit { exit_id }],
                ),
            )
            .await
            .expect("delete exit envelope");
        assert!(
            backend.get_area(&area.id).await.unwrap().rooms[0]
                .exits
                .is_empty()
        );

        fs::remove_dir_all(&root).ok();
    }

    /// Two `LocalBackend`s over the same shared root (the per-session mount):
    /// after B has already loaded its index, a create by A is still observed by
    /// B's next `list_areas` (reload-on-list, not a frozen one-shot index).
    #[tokio::test]
    async fn list_reflects_another_instances_writes_on_the_same_root() {
        let root = temp_root();
        let a = LocalBackend::new(&root);
        let b = LocalBackend::new(&root);

        // B loads its (empty) index first.
        assert!(b.list_areas().await.expect("b list").is_empty());

        // A creates an area on the shared root.
        let area = a
            .create_area(new_area_request("Shared", None))
            .await
            .expect("a create");

        // B re-lists and sees it.
        let listed = b.list_areas().await.expect("b relist");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, area.id);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn execute_mutation_stale_revision_conflicts_and_stores_nothing() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");

        let result = backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    41,
                    vec![AreaMutation::UpsertRoom {
                        room_number: RoomNumber(1),
                        body: RoomUpdates::default(),
                    }],
                ),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(CloudError::RevisionConflict {
                    expected_rev: 41,
                    current_rev: 1,
                    ..
                })
            ),
            "a stale precondition must conflict with the live rev, got {result:?}"
        );

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.area.rev, 1, "a conflicted envelope moves nothing");
        assert!(details.rooms.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn execute_mutation_applies_all_ops_with_one_rev_bump() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");

        let exit_id = ExitId(Uuid::new_v4());
        let result = backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    1,
                    vec![
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(1),
                            body: RoomUpdates {
                                title: Some("Hall".to_string()),
                                ..RoomUpdates::default()
                            },
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(1),
                            body: ExitArgs {
                                id: Some(exit_id),
                                from_direction: ExitDirection::North,
                                ..ExitArgs::default()
                            },
                        },
                        AreaMutation::AddRoomTag {
                            room_number: RoomNumber(1),
                            tag: "INN".to_string(),
                        },
                    ],
                ),
            )
            .await
            .expect("envelope applies");

        assert_eq!(result.versions.len(), 1);
        assert_eq!(result.versions[0].rev, 2, "three ops bump rev exactly once");
        assert_eq!(result.data.len(), 3);
        assert!(matches!(&result.data[0], OpResult::Room { room } if room.title == "Hall"));
        assert!(matches!(&result.data[1], OpResult::Exit { exit } if exit.id == exit_id));
        assert!(matches!(&result.data[2], OpResult::RoomTag { tag, .. } if tag == "INN"));

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.area.rev, 2);
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].title, "Hall");
        assert_eq!(details.rooms[0].exits.len(), 1);
        assert!(details.rooms[0].tags.contains("INN"));

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn execute_mutation_failed_op_leaves_the_area_byte_identical() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");
        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    1,
                    vec![AreaMutation::UpsertRoom {
                        room_number: RoomNumber(1),
                        body: RoomUpdates::default(),
                    }],
                ),
            )
            .await
            .expect("seed room");

        let before = backend.get_area(&area.id).await.expect("get");
        let result = backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    before.area.rev,
                    vec![
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(2),
                            body: RoomUpdates::default(),
                        },
                        AreaMutation::DeleteRoom {
                            room_number: RoomNumber(999),
                        },
                    ],
                ),
            )
            .await;
        assert!(matches!(result, Err(CloudError::RoomNotFound(_))));

        let after = backend.get_area(&area.id).await.expect("get");
        assert_eq!(
            serde_json::to_string(&before).expect("serialize"),
            serde_json::to_string(&after).expect("serialize"),
            "a failed envelope must leave the area byte-identical"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn execute_mutation_create_ops_honor_client_ids() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");

        let exit_id = ExitId(Uuid::new_v4());
        let label_id = LabelId(Uuid::new_v4());
        let shape_id = ShapeId(Uuid::new_v4());
        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    1,
                    vec![
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(1),
                            body: RoomUpdates::default(),
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(1),
                            body: ExitArgs {
                                id: Some(exit_id),
                                from_direction: ExitDirection::North,
                                ..ExitArgs::default()
                            },
                        },
                        AreaMutation::CreateLabel {
                            body: LabelArgs {
                                id: Some(label_id),
                                ..LabelArgs::default()
                            },
                        },
                        AreaMutation::CreateShape {
                            body: ShapeArgs {
                                id: Some(shape_id),
                                ..ShapeArgs::default()
                            },
                        },
                    ],
                ),
            )
            .await
            .expect("envelope applies");

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.rooms[0].exits[0].id, exit_id);
        assert_eq!(details.labels[0].id, label_id);
        assert_eq!(details.shapes[0].id, shape_id);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn delete_room_nulls_same_area_inbound_exits() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");

        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    1,
                    vec![
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(1),
                            body: RoomUpdates::default(),
                        },
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(2),
                            body: RoomUpdates::default(),
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(1),
                            body: ExitArgs {
                                from_direction: ExitDirection::North,
                                to_area_id: Some(area.id),
                                to_room_number: Some(RoomNumber(2)),
                                ..ExitArgs::default()
                            },
                        },
                    ],
                ),
            )
            .await
            .expect("seed envelope");

        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    2,
                    vec![AreaMutation::DeleteRoom {
                        room_number: RoomNumber(2),
                    }],
                ),
            )
            .await
            .expect("delete r2");

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.rooms.len(), 1, "room 2 removed");
        let exit = &details.rooms[0].exits[0];
        assert_eq!(exit.to_area_id, None, "inbound exit cleared");
        assert_eq!(exit.to_room_number, None);

        fs::remove_dir_all(&root).ok();
    }

    /// Server parity: `CreateExit` materializes an absent from-room (and a
    /// same-area destination room) as blank placeholders instead of
    /// failing, exactly like the deployed server's applier — so an
    /// envelope accepted by the cloud tier is accepted here too.
    #[tokio::test]
    async fn create_exit_materializes_placeholder_rooms_like_the_server() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");

        // Neither room 42 nor its same-area destination 43 exists yet.
        let result = backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    1,
                    vec![AreaMutation::CreateExit {
                        room_number: RoomNumber(42),
                        body: ExitArgs {
                            from_direction: ExitDirection::North,
                            to_area_id: Some(area.id),
                            to_room_number: Some(RoomNumber(43)),
                            ..ExitArgs::default()
                        },
                    }],
                ),
            )
            .await
            .expect("the envelope applies with placeholder rooms");
        assert_eq!(result.versions[0].rev, 2, "one bump");

        let details = backend.get_area(&area.id).await.expect("get");
        let numbers: Vec<i32> = details.rooms.iter().map(|r| r.room_number.0).collect();
        assert!(numbers.contains(&42), "from-room placeholder created");
        assert!(
            numbers.contains(&43),
            "same-area destination placeholder created"
        );
        let from_room = details
            .rooms
            .iter()
            .find(|r| r.room_number == RoomNumber(42))
            .expect("from-room");
        assert!(from_room.title.is_empty(), "placeholders are blank");
        assert_eq!(from_room.exits.len(), 1);
        assert_eq!(from_room.exits[0].to_room_number, Some(RoomNumber(43)));

        // A cross-area destination stays a stored reference: a single-area
        // applier cannot create rooms in a foreign document.
        let foreign = AreaId(Uuid::new_v4());
        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    2,
                    vec![AreaMutation::CreateExit {
                        room_number: RoomNumber(42),
                        body: ExitArgs {
                            from_direction: ExitDirection::South,
                            to_area_id: Some(foreign),
                            to_room_number: Some(RoomNumber(7)),
                            ..ExitArgs::default()
                        },
                    }],
                ),
            )
            .await
            .expect("cross-area destination applies as a reference");
        let details = backend.get_area(&area.id).await.expect("get");
        assert!(
            !details.rooms.iter().any(|r| r.room_number == RoomNumber(7)),
            "no local room is minted for a foreign destination"
        );

        fs::remove_dir_all(&root).ok();
    }

    // ===== v1 → v2 file migration (§8.3) =====

    /// A v1 area document with one reciprocal exit pair, as an old binary
    /// wrote it (per-exit style/color, no `format_version`, no connections).
    fn v1_area_json(area_id: AreaId, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": area_id.0,
            "user_id": null,
            "atlas_id": null,
            "name": name,
            "created_at": "2025-01-01T00:00:00Z",
            "rev": 5,
            "properties": [],
            "rooms": [
                {
                    "room_number": 1, "title": "West End", "description": "",
                    "level": 0, "x": 0.0, "y": 0.0, "color": "",
                    "properties": [],
                    "exits": [{
                        "id": Uuid::from_u128(0xE1), "from_direction": "East",
                        "to_area_id": area_id.0, "to_room_number": 2,
                        "to_direction": "West", "path": "", "is_hidden": false,
                        "is_closed": false, "is_locked": false, "weight": 1.0,
                        "command": "", "style": "Dashed", "color": "#ff8800"
                    }]
                },
                {
                    "room_number": 2, "title": "East End", "description": "",
                    "level": 0, "x": 2.0, "y": 0.0, "color": "",
                    "properties": [],
                    "exits": [{
                        "id": Uuid::from_u128(0xE2), "from_direction": "West",
                        "to_area_id": area_id.0, "to_room_number": 1,
                        "to_direction": "East", "path": "", "is_hidden": false,
                        "is_closed": false, "is_locked": false, "weight": 1.0,
                        "command": "", "style": "Normal", "color": ""
                    }]
                }
            ],
            "labels": [],
            "shapes": []
        })
    }

    fn write_v1_file(root: &Path, area_id: AreaId, json: &serde_json::Value) -> Vec<u8> {
        let dir = root.join("areas");
        fs::create_dir_all(&dir).expect("create v1 dir");
        let bytes = serde_json::to_vec_pretty(json).expect("serialize v1 fixture");
        fs::write(dir.join(format!("{area_id}.json")), &bytes).expect("write v1 file");
        bytes
    }

    #[tokio::test]
    async fn v1_file_migrates_on_scan_with_backup_and_untouched_source() {
        let root = temp_root();
        let area_id = AreaId(Uuid::new_v4());
        let original = write_v1_file(&root, area_id, &v1_area_json(area_id, "Old Roads"));

        let backend = LocalBackend::new(&root);
        let listed = backend.list_areas().await.expect("list");
        assert_eq!(listed.len(), 1, "the scan migrates the straggler eagerly");
        assert_eq!(listed[0].id, area_id);

        let details = backend.get_area(&area_id).await.expect("get");
        assert_eq!(details.format_version, crate::AREA_FORMAT_VERSION);
        assert_eq!(details.connections.len(), 1, "the reciprocal pair paired");
        let exits: Vec<_> = details
            .rooms
            .iter()
            .flat_map(|room| room.exits.iter())
            .collect();
        assert_eq!(exits.len(), 2, "every exit keeps its identity");
        assert!(
            exits
                .iter()
                .all(|exit| exit.connection_id == details.connections[0].id),
            "both members reference the one Connection"
        );
        assert_eq!(details.connections[0].color, "#ff8800");

        // The v2 copy exists; the v1 source is byte-identical; the backup
        // holds the untouched v1 bytes.
        assert!(
            root.join("areas-v2")
                .join(format!("{area_id}.json"))
                .exists()
        );
        let source = fs::read(root.join("areas").join(format!("{area_id}.json")))
            .expect("v1 source still present");
        assert_eq!(source, original, "the v1 source is never rewritten");
        let backups: Vec<_> = fs::read_dir(root.join("areas-v1-backup"))
            .expect("backup dir")
            .flatten()
            .collect();
        assert_eq!(backups.len(), 1, "one timestamped backup");
        assert_eq!(
            fs::read(backups[0].path()).expect("backup readable"),
            original,
            "the backup is the untouched v1 bytes"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn v1_file_migrates_on_demand_and_is_never_reread_after() {
        let root = temp_root();
        let area_id = AreaId(Uuid::new_v4());
        write_v1_file(&root, area_id, &v1_area_json(area_id, "Lazy Lane"));

        // Direct get_area (no scan first): migrates on demand.
        let backend = LocalBackend::new(&root);
        let details = backend
            .get_area(&area_id)
            .await
            .expect("on-demand migration");
        assert_eq!(details.format_version, crate::AREA_FORMAT_VERSION);

        // An old binary editing the stale v1 namespace afterwards is
        // deliberately ignored: the v2 copy wins and the v1 file is never
        // re-read.
        let mut stale = v1_area_json(area_id, "Renamed By Old Binary");
        stale["rev"] = serde_json::json!(99);
        write_v1_file(&root, area_id, &stale);
        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        let details = reopened.get_area(&area_id).await.expect("get");
        assert_eq!(
            details.area.name, "Lazy Lane",
            "the stale v1 edit is not merged"
        );
        assert_eq!(details.area.rev, 5);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn unreadable_v1_file_is_reported_and_left_intact() {
        let root = temp_root();
        let dir = root.join("areas");
        fs::create_dir_all(&dir).expect("create v1 dir");
        let path = dir.join(format!("{}.json", Uuid::new_v4()));
        fs::write(&path, b"{ not json").expect("write garbage");

        let backend = LocalBackend::new(&root);
        assert!(
            backend.list_areas().await.expect("list").is_empty(),
            "a failed migration is skipped, not fatal"
        );
        assert!(path.exists(), "the unreadable v1 file is left intact");
        assert!(
            !root.join("areas-v1-backup").exists(),
            "no backup is written for a file that never parsed"
        );
        assert!(
            fs::read_dir(root.join("areas-v2")).is_ok_and(|entries| entries.count() == 0)
                || !root.join("areas-v2").exists(),
            "no partial replacement is ever written"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn newer_format_is_a_hard_readonly_error_naming_the_file() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Future", None))
            .await
            .expect("create");

        // Rewrite the stored v2 file as a claimed v3 document.
        let path = root.join("areas-v2").join(format!("{}.json", area.id));
        let mut doc: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse");
        doc["format_version"] = serde_json::json!(3);
        fs::write(&path, serde_json::to_vec_pretty(&doc).expect("serialize")).expect("write");
        backend.refresh().await.expect("adopt external edit");

        let err = backend
            .get_area(&area.id)
            .await
            .expect_err("must refuse v3");
        let message = err.to_string();
        assert!(
            message.contains("format v3") && message.contains(&format!("{}", area.id)),
            "the error names the file and version: {message}"
        );

        // The scan skips it too (reported, not fatal).
        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        assert!(reopened.list_areas().await.expect("list").is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn delete_area_removes_both_namespaces() {
        let root = temp_root();
        let area_id = AreaId(Uuid::new_v4());
        write_v1_file(&root, area_id, &v1_area_json(area_id, "Doomed"));

        let backend = LocalBackend::new(&root);
        backend.get_area(&area_id).await.expect("migrates");
        backend.delete_area(&area_id).await.expect("delete");
        assert!(
            !root
                .join("areas-v2")
                .join(format!("{area_id}.json"))
                .exists(),
            "v2 copy removed"
        );
        assert!(
            !root.join("areas").join(format!("{area_id}.json")).exists(),
            "v1 source removed too (else the next scan would resurrect it)"
        );
        assert!(
            backend.get_area(&area_id).await.is_err(),
            "the area is gone for good"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn create_room_refuses_an_occupied_number_and_applies_to_a_vacant_one() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("CreateOnly", None))
            .await
            .expect("area");
        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    1,
                    vec![AreaMutation::UpsertRoom {
                        room_number: RoomNumber(1),
                        body: RoomUpdates {
                            title: Some("Original".to_string()),
                            ..RoomUpdates::default()
                        },
                    }],
                ),
            )
            .await
            .expect("seed room 1");

        // Occupied number: the whole envelope refuses (a leading op that
        // would apply proves atomicity), nothing changes, no rev moves.
        let refused = backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    2,
                    vec![
                        AreaMutation::UpsertAreaProperty {
                            name: "climate".to_string(),
                            value: "arid".to_string(),
                            is_secret: None,
                        },
                        AreaMutation::CreateRoom {
                            room_number: RoomNumber(1),
                            body: RoomUpdates {
                                title: Some("Usurper".to_string()),
                                ..RoomUpdates::default()
                            },
                        },
                    ],
                ),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(CloudError::StructuralConflict(ref reason)) if reason == "room_number_exists"
            ),
            "an occupied number is a structural conflict, got {refused:?}"
        );
        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.area.rev, 2, "the refused envelope moved no rev");
        assert!(details.properties.is_empty(), "the leading op rolled back");
        assert_eq!(details.rooms[0].title, "Original", "no silent merge");

        // Vacant number: create_room applies like an upsert.
        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    2,
                    vec![AreaMutation::CreateRoom {
                        room_number: RoomNumber(2),
                        body: RoomUpdates {
                            title: Some("Fresh".to_string()),
                            ..RoomUpdates::default()
                        },
                    }],
                ),
            )
            .await
            .expect("vacant create applies");
        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.rooms.len(), 2);

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn execute_mutation_replays_persisted_receipt_exactly_once() {
        let root = temp_root();
        let area_id;
        let mutation;
        let first_result;
        {
            let backend = LocalBackend::new(&root);
            let area = backend
                .create_area(new_area_request("A", None))
                .await
                .expect("area");
            area_id = area.id;
            mutation = envelope(
                area.id,
                1,
                vec![AreaMutation::UpsertRoom {
                    room_number: RoomNumber(1),
                    body: RoomUpdates {
                        title: Some("Applied once".to_string()),
                        ..RoomUpdates::default()
                    },
                }],
            );
            first_result = backend
                .execute_mutation(&area.id, &mutation)
                .await
                .expect("first application");
            assert_eq!(backend.get_area(&area.id).await.unwrap().area.rev, 2);
        }

        // Simulate restart after the area-file commit but before pending-WAL
        // retirement. The stale expected revision would conflict if the
        // operation were applied again.
        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        let replayed = reopened
            .execute_mutation(&area_id, &mutation)
            .await
            .expect("receipt replay");
        assert_eq!(replayed.operation_id, first_result.operation_id);
        assert_eq!(replayed.versions, first_result.versions);
        assert_eq!(reopened.get_area(&area_id).await.unwrap().area.rev, 2);

        let bytes = fs::read(reopened.area_path(area_id)).expect("read local document");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("parse document");
        assert_eq!(
            json["_smudgy_applied_operations"].as_array().map(Vec::len),
            Some(1)
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn delete_area_uses_the_mutation_write_lock() {
        let root = temp_root();
        let backend = Arc::new(LocalBackend::new(&root));
        let area = backend
            .create_area(new_area_request("Serialized", None))
            .await
            .expect("create");

        let write_guard = backend.lock_store().await.expect("store lock");
        let delete = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.delete_area(&area.id).await })
        };
        tokio::task::yield_now().await;
        assert!(
            !delete.is_finished(),
            "delete must wait for an in-flight whole-file mutation"
        );

        drop(write_guard);
        delete.await.expect("delete task").expect("delete");
        assert!(backend.get_area(&area.id).await.is_err());

        fs::remove_dir_all(&root).ok();
    }

    // ===== multi-write transactions =====

    /// Three empty areas, returned as their on-disk documents.
    async fn three_areas(backend: &LocalBackend) -> Vec<LocalAreaDocument> {
        let mut documents = Vec::new();
        for name in ["A", "B", "C"] {
            let area = backend
                .create_area(new_area_request(name, None))
                .await
                .expect("create");
            documents.push(backend.load_area_document(area.id).await.expect("document"));
        }
        documents
    }

    /// A merge-shaped transaction over `three_areas`: the first two are
    /// rewritten with a new name and a bumped rev, the third is deleted.
    fn merge_shaped(documents: &[LocalAreaDocument]) -> (Vec<LocalAreaDocument>, Vec<AreaId>) {
        let writes = documents[..2]
            .iter()
            .map(|document| {
                let mut written = document.clone();
                written.details.area.name = format!("{} (merged)", written.details.area.name);
                written.details.area.rev += 1;
                written
            })
            .collect();
        (writes, vec![documents[2].details.area.id])
    }

    fn multi_write_journals(backend: &LocalBackend) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(backend.transactions_dir()) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(MULTI_WRITE_TRANSACTION_PREFIX))
            })
            .collect()
    }

    #[tokio::test]
    async fn commit_documents_writes_and_deletes_together() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (writes, deletes) = merge_shaped(&documents);
        let removed_id = deletes[0];

        {
            let _guard = backend.lock_store().await.expect("store lock");
            backend
                .commit_documents(writes.clone(), deletes)
                .await
                .expect("commit");
        }

        for written in &writes {
            let on_disk = backend
                .get_area(&written.details.area.id)
                .await
                .expect("written area");
            assert_eq!(on_disk.area.name, written.details.area.name);
            assert_eq!(on_disk.area.rev, written.details.area.rev);
        }
        assert!(matches!(
            backend.get_area(&removed_id).await,
            Err(CloudError::NotFoundOrNoAccess)
        ));
        assert!(!backend.area_path(removed_id).exists());

        let headers = backend.headers();
        assert_eq!(headers.len(), 2, "index mirrors disk");
        for written in &writes {
            assert_eq!(
                headers[&written.details.area.id].name,
                written.details.area.name
            );
        }
        assert!(!headers.contains_key(&removed_id));
        assert!(
            multi_write_journals(&backend).is_empty(),
            "a completed transaction retires its journal"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn multi_write_journal_rolls_forward_on_reopen() {
        let crashed_root = temp_root();
        let clean_root = temp_root();
        let crashed = LocalBackend::new(&crashed_root);
        let clean = LocalBackend::new(&clean_root);
        let documents = three_areas(&crashed).await;
        // The same three documents, ids included, in a second store that
        // will run the transaction without interruption.
        for document in &documents {
            clean
                .store_area_document(document.clone())
                .await
                .expect("seed clean store");
        }
        let (writes, deletes) = merge_shaped(&documents);
        let removed_id = deletes[0];
        {
            let _guard = clean.lock_store().await.expect("store lock");
            clean
                .commit_documents(writes.clone(), deletes.clone())
                .await
                .expect("uninterrupted commit");
        }

        // Crash after the journal and the first post-image landed: the
        // second write and the delete never ran.
        let journal =
            crashed.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(
            journal.clone(),
            LocalMultiWriteTransaction {
                writes: writes.clone(),
                deletes,
                ..Default::default()
            },
        )
        .await
        .expect("journal");
        crashed
            .store_area_document(writes[0].clone())
            .await
            .expect("first write");
        drop(crashed);

        let reopened = LocalBackend::new(&crashed_root);
        reopened.ensure_loaded().await.expect("load reopened store");
        let second = reopened
            .get_area(&writes[1].details.area.id)
            .await
            .expect("second write completed");
        assert_eq!(second.area.name, writes[1].details.area.name);
        assert_eq!(second.area.rev, writes[1].details.area.rev);
        assert!(matches!(
            reopened.get_area(&removed_id).await,
            Err(CloudError::NotFoundOrNoAccess)
        ));
        assert!(!journal.exists(), "recovery retires the journal");
        assert!(!reopened.headers().contains_key(&removed_id));

        for written in &writes {
            let id = written.details.area.id;
            assert_eq!(
                fs::read(reopened.area_path(id)).expect("recovered bytes"),
                fs::read(clean.area_path(id)).expect("clean bytes"),
                "recovery lands the same bytes as an uninterrupted run"
            );
        }
        assert!(!clean.area_path(removed_id).exists());
        assert!(!reopened.area_path(removed_id).exists());

        fs::remove_dir_all(&crashed_root).ok();
        fs::remove_dir_all(&clean_root).ok();
    }

    #[tokio::test]
    async fn multi_write_journal_replay_is_idempotent() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (writes, deletes) = merge_shaped(&documents);
        let removed_id = deletes[0];
        let transaction = LocalMultiWriteTransaction {
            writes: writes.clone(),
            deletes,
            ..Default::default()
        };
        let journal =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(journal.clone(), transaction.clone())
            .await
            .expect("journal");
        let (transactions, areas, legacy) = (
            backend.transactions_dir(),
            backend.areas_dir(),
            backend.legacy_areas_dir(),
        );

        recover_multi_write_transactions(&transactions, &areas, &legacy);
        let snapshot = |backend: &LocalBackend| -> Vec<Vec<u8>> {
            writes
                .iter()
                .map(|written| fs::read(backend.area_path(written.details.area.id)).expect("bytes"))
                .collect()
        };
        let first = snapshot(&backend);
        assert!(!journal.exists());
        assert!(!backend.area_path(removed_id).exists());

        // A second pass over an empty transactions directory changes nothing.
        recover_multi_write_transactions(&transactions, &areas, &legacy);
        assert_eq!(snapshot(&backend), first);

        // Re-applying the same journal (a crash mid-recovery) changes nothing
        // either: each post-image is already on disk, each delete already done.
        store_transaction_file(journal.clone(), transaction)
            .await
            .expect("journal again");
        recover_multi_write_transactions(&transactions, &areas, &legacy);
        assert_eq!(snapshot(&backend), first);
        assert!(!journal.exists());
        assert!(!backend.area_path(removed_id).exists());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn multi_write_recovery_skips_a_document_disk_has_moved_past() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (writes, deletes) = merge_shaped(&documents);
        let journal =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(
            journal.clone(),
            LocalMultiWriteTransaction {
                writes: writes.clone(),
                deletes,
                ..Default::default()
            },
        )
        .await
        .expect("journal");

        // A completed transaction whose journal removal failed, followed by
        // an ordinary edit: the edit's rev is higher than the post-image's.
        let mut later = writes[0].clone();
        later.details.area.name = "Edited after the transaction".to_string();
        later.details.area.rev += 1;
        backend
            .store_area_document(later.clone())
            .await
            .expect("later edit");

        recover_multi_write_transactions(
            &backend.transactions_dir(),
            &backend.areas_dir(),
            &backend.legacy_areas_dir(),
        );
        backend.refresh().await.expect("adopt disk recovery");
        let kept = backend
            .get_area(&later.details.area.id)
            .await
            .expect("edited area");
        assert_eq!(kept.area.name, "Edited after the transaction");
        assert_eq!(kept.area.rev, later.details.area.rev);
        let rolled = backend
            .get_area(&writes[1].details.area.id)
            .await
            .expect("other area");
        assert_eq!(rolled.area.name, writes[1].details.area.name);
        assert!(!journal.exists());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn multi_write_recovery_removes_both_namespaces() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Straggler", None))
            .await
            .expect("create");
        // A v1-namespace copy in the current format: if it survived the
        // delete, the straggler migration would adopt it verbatim on the
        // next scan and the area would be listed again (see the note in
        // `delete_area`).
        let v2_bytes = fs::read(backend.area_path(area.id)).expect("v2 bytes");
        fs::create_dir_all(backend.legacy_areas_dir()).expect("legacy dir");
        fs::write(backend.legacy_area_path(area.id), v2_bytes).expect("legacy copy");
        let journal =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(
            journal.clone(),
            LocalMultiWriteTransaction {
                writes: Vec::new(),
                deletes: vec![area.id],
                ..Default::default()
            },
        )
        .await
        .expect("journal");
        drop(backend);

        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        let listed = reopened.list_areas().await.expect("recover and list");
        assert!(listed.is_empty(), "the deleted area is not resurrected");
        assert!(!reopened.area_path(area.id).exists());
        assert!(!reopened.legacy_area_path(area.id).exists());
        assert!(!journal.exists());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn unparseable_multi_write_journal_is_left_in_place() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("Survivor", None))
            .await
            .expect("create");
        let journal =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        fs::create_dir_all(backend.transactions_dir()).expect("transactions dir");
        let mut legacy = backend.get_area(&area.id).await.unwrap();
        legacy.area.id = AreaId(Uuid::new_v4());
        legacy.area.name = "Legacy namespace".into();
        fs::create_dir_all(backend.legacy_areas_dir()).unwrap();
        let legacy_path = backend.legacy_area_path(legacy.area.id);
        fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let unreadable_id = AreaId(Uuid::new_v4());
        fs::create_dir(backend.legacy_area_path(unreadable_id)).unwrap();
        let garbage = b"{ not a transaction";
        fs::write(&journal, garbage).expect("garbage journal");
        drop(backend);

        let reopened = Arc::new(LocalBackend::new(&root));
        let listed = reopened
            .list_areas()
            .await
            .expect("surviving maps remain readable");
        assert_eq!(listed.len(), 2);
        assert!(reopened.get_area(&unreadable_id).await.is_err());
        assert!(listed.iter().any(|entry| entry.id == area.id));
        assert_eq!(
            reopened.get_area(&area.id).await.unwrap().area.name,
            "Survivor"
        );
        assert_eq!(
            reopened.get_area(&legacy.area.id).await.unwrap().area.name,
            "Legacy namespace"
        );
        assert!(
            !reopened.area_path(legacy.area.id).exists(),
            "degraded reads must not migrate files"
        );
        assert!(reopened.snapshot().await.unwrap().recovery_error.is_some());
        assert!(reopened.refresh().await.is_err());
        assert!(
            reopened
                .create_area(new_area_request("Blocked", None))
                .await
                .is_err()
        );
        assert_eq!(
            fs::read(&journal).expect("journal still present"),
            garbage,
            "an unreadable journal is left for inspection, untouched"
        );
        let cloud = Arc::new(crate::backends::EphemeralBackend::new());
        let remote = cloud
            .create_area(new_area_request("Cloud", None))
            .await
            .unwrap();
        let composite = crate::backends::CompositeBackend::new(reopened.clone(), cloud);
        assert_eq!(
            composite.get_area(&remote.id).await.unwrap().area.name,
            "Cloud"
        );
        assert_eq!(
            composite.get_area(&area.id).await.unwrap().area.name,
            "Survivor"
        );
        fs::remove_file(&journal).unwrap();
        reopened
            .create_area(new_area_request("Unblocked", None))
            .await
            .unwrap();
        assert!(reopened.snapshot().await.unwrap().recovery_error.is_none());

        fs::remove_dir_all(&root).ok();
    }

    /// A post-image write that fails after the journal landed is finished
    /// inline: the caller sees `Ok`, disk and the header index hold the
    /// outcome, and no journal lingers.
    #[tokio::test]
    async fn commit_documents_finishes_inline_when_a_write_fails_after_the_journal() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (writes, deletes) = merge_shaped(&documents);
        let removed_id = deletes[0];
        backend
            .store
            .get()
            .unwrap()
            .write_faults
            .store(1, std::sync::atomic::Ordering::Release);

        {
            let _guard = backend.lock_store().await.expect("store lock");
            backend
                .commit_documents(writes.clone(), deletes)
                .await
                .expect("the stalled transaction is finished inline");
        }

        for written in &writes {
            let on_disk = backend
                .get_area(&written.details.area.id)
                .await
                .expect("written area");
            assert_eq!(on_disk.area.name, written.details.area.name);
            assert_eq!(on_disk.area.rev, written.details.area.rev);
        }
        assert!(!backend.area_path(removed_id).exists());
        let headers = backend.headers();
        assert_eq!(headers.len(), 2, "the index mirrors the finished outcome");
        for written in &writes {
            assert_eq!(
                headers[&written.details.area.id].rev,
                written.details.area.rev
            );
        }
        assert!(!headers.contains_key(&removed_id));
        assert!(
            multi_write_journals(&backend).is_empty(),
            "the finished transaction retires its journal"
        );

        fs::remove_dir_all(&root).ok();
    }

    /// When the inline roll-forward fails too, the error says the
    /// transaction is journaled, the journal stays, and the next reload
    /// finishes it once the obstruction is gone.
    #[tokio::test]
    async fn commit_documents_reports_a_journaled_transaction_when_inline_recovery_fails() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (writes, deletes) = merge_shaped(&documents);
        let removed_id = deletes[0];
        // A directory where the second post-image must land: the atomic
        // rename onto it fails on every platform, inline and on retry.
        let obstructed = backend.area_path(writes[1].details.area.id);
        fs::remove_file(&obstructed).expect("clear the target");
        fs::create_dir_all(&obstructed).expect("obstruct the target");

        let error = {
            let _guard = backend.lock_store().await.expect("store lock");
            backend
                .commit_documents(writes.clone(), deletes)
                .await
                .expect_err("the obstruction defeats the inline roll-forward")
        };
        let message = error.to_string();
        assert!(
            message.contains("journaled") && message.contains("completes on the next start"),
            "{message}"
        );
        let journals = multi_write_journals(&backend);
        assert_eq!(journals.len(), 1, "the journal waits for the next start");

        fs::remove_dir(&obstructed).expect("lift the obstruction");
        backend.reload().await.expect("reload");
        for written in &writes {
            let on_disk = backend
                .get_area(&written.details.area.id)
                .await
                .expect("written area");
            assert_eq!(on_disk.area.name, written.details.area.name);
        }
        assert!(!backend.area_path(removed_id).exists());
        assert!(!journals[0].exists(), "the reload retires the journal");

        fs::remove_dir_all(&root).ok();
    }

    /// A journal left behind by an earlier transaction is rolled forward
    /// before the next transaction writes its own, so its outcome does not
    /// wait for a reload.
    #[tokio::test]
    async fn commit_documents_rolls_lingering_journals_forward_first() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (lingering_writes, lingering_deletes) = merge_shaped(&documents);
        let removed_id = lingering_deletes[0];
        // The earlier transaction's journal, none of whose steps ran.
        let lingering =
            backend.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(
            lingering.clone(),
            LocalMultiWriteTransaction {
                writes: lingering_writes.clone(),
                deletes: lingering_deletes,
                ..Default::default()
            },
        )
        .await
        .expect("lingering journal");

        // A fresh transaction touching only the second area, on top of the
        // lingering one's post-image.
        let mut fresh = lingering_writes[1].clone();
        fresh.details.area.name = "Fresh".to_string();
        fresh.details.area.rev += 1;
        {
            let _guard = backend.lock_store().await.expect("store lock");
            backend
                .commit_documents(vec![fresh.clone()], Vec::new())
                .await
                .expect("commit");
        }

        assert!(
            !lingering.exists(),
            "the lingering journal was retired first"
        );
        let first = backend
            .get_area(&lingering_writes[0].details.area.id)
            .await
            .expect("first area");
        assert_eq!(first.area.name, lingering_writes[0].details.area.name);
        let second = backend
            .get_area(&fresh.details.area.id)
            .await
            .expect("second area");
        assert_eq!(second.area.name, "Fresh");
        assert!(!backend.area_path(removed_id).exists());
        let headers = backend.headers();
        assert_eq!(
            headers[&first.area.id].name, first.area.name,
            "the index follows the rolled-forward journal"
        );
        assert!(!headers.contains_key(&removed_id));
        assert!(multi_write_journals(&backend).is_empty());

        fs::remove_dir_all(&root).ok();
    }

    /// Two lingering journals replay oldest first, and a write in the older
    /// one whose area the newer one deletes is not replayed: the area stays
    /// deleted, and a document both wrote ends at the newer revision.
    #[tokio::test]
    async fn multi_write_journals_replay_in_sequence_and_a_later_delete_wins() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let documents = three_areas(&backend).await;
        let (a, b, c) = (
            documents[0].details.area.id,
            documents[1].details.area.id,
            documents[2].details.area.id,
        );
        let renamed = |document: &LocalAreaDocument, name: &str, rev: i64| {
            let mut written = document.clone();
            written.details.area.name = name.to_string();
            written.details.area.rev = rev;
            written
        };
        // Sequence 7 writes A and B and deletes C; sequence 12 rewrites A
        // and deletes B. The uuids are chosen so that a name order ignoring
        // the sequence would replay them the other way round.
        let older = backend.multi_write_transaction_path(7, Uuid::from_u128(u128::MAX));
        let newer = backend.multi_write_transaction_path(12, Uuid::from_u128(1));
        store_transaction_file(
            older.clone(),
            LocalMultiWriteTransaction {
                writes: vec![
                    renamed(&documents[0], "A first", 2),
                    renamed(&documents[1], "B first", 2),
                ],
                deletes: vec![c],
                ..Default::default()
            },
        )
        .await
        .expect("older journal");
        store_transaction_file(
            newer.clone(),
            LocalMultiWriteTransaction {
                writes: vec![renamed(&documents[0], "A second", 3)],
                deletes: vec![b],
                ..Default::default()
            },
        )
        .await
        .expect("newer journal");
        drop(backend);

        let reopened = LocalBackend::new(&root);
        reopened.refresh().await.expect("explicit disk refresh");
        let listed = reopened.list_areas().await.expect("recover and list");
        assert_eq!(
            listed.iter().map(|area| area.id).collect::<Vec<_>>(),
            vec![a],
            "only the survivor is listed"
        );
        let survivor = reopened.get_area(&a).await.expect("survivor");
        assert_eq!(survivor.area.name, "A second");
        assert_eq!(survivor.area.rev, 3);
        assert!(!reopened.area_path(b).exists(), "the later delete wins");
        assert!(!reopened.area_path(c).exists());
        assert!(!older.exists() && !newer.exists(), "both journals retired");
        assert!(
            next_multi_write_sequence() > 12,
            "the sequence continues past every journal seen on disk"
        );

        fs::remove_dir_all(&root).ok();
    }

    // ===== area merges =====

    /// Seeds room 1 in `area_id` through one envelope, with an exit north
    /// out of it to `exit_to` when given. Leaves the area at rev 2 holding
    /// one receipt.
    async fn seed_room(backend: &LocalBackend, area_id: AreaId, exit_to: Option<(AreaId, i32)>) {
        let mut payload = vec![AreaMutation::UpsertRoom {
            room_number: RoomNumber(1),
            body: RoomUpdates {
                title: Some("Hall".to_string()),
                ..RoomUpdates::default()
            },
        }];
        if let Some((to_area, to_room)) = exit_to {
            payload.push(AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                body: ExitArgs {
                    from_direction: ExitDirection::North,
                    to_area_id: Some(to_area),
                    to_room_number: Some(RoomNumber(to_room)),
                    ..ExitArgs::default()
                },
            });
        }
        backend
            .execute_mutation(&area_id, &envelope(area_id, 1, payload))
            .await
            .expect("seed envelope");
    }

    /// A destination with room 1, a source with room 1 (so the merge must
    /// renumber it), and a third party with room 1 whose exit points into
    /// the source's room when `third_party_links_source`, else nowhere.
    /// Every area is at rev 2 with one receipt.
    async fn merge_areas_fixture(
        backend: &LocalBackend,
        third_party_links_source: bool,
    ) -> (AreaId, AreaId, AreaId) {
        let mut ids = Vec::new();
        for name in ["Into", "Source", "Third"] {
            let area = backend
                .create_area(new_area_request(name, None))
                .await
                .expect("create");
            ids.push(area.id);
        }
        let (into, source, third) = (ids[0], ids[1], ids[2]);
        seed_room(backend, into, None).await;
        seed_room(backend, source, None).await;
        seed_room(
            backend,
            third,
            third_party_links_source.then_some((source, 1)),
        )
        .await;
        (into, source, third)
    }

    /// A plan over the live documents: every expected rev as stored, the
    /// floor one above the destination's highest room number.
    async fn merge_plan(
        backend: &LocalBackend,
        into: AreaId,
        sources: &[AreaId],
        inbound: &[AreaId],
    ) -> AreaMergePlan {
        let mut expected = Vec::new();
        for id in std::iter::once(&into).chain(sources).chain(inbound) {
            let rev = backend.get_area(id).await.expect("touched area").area.rev;
            expected.push((*id, rev));
        }
        let floor = backend
            .get_area(&into)
            .await
            .expect("destination")
            .rooms
            .iter()
            .map(|room| room.room_number.0)
            .max()
            .unwrap_or(0)
            + 1;
        AreaMergePlan {
            into,
            sources: sources
                .iter()
                .map(|id| AreaMergeSource {
                    id: *id,
                    translate: Translate::default(),
                    rooms: None,
                })
                .collect(),
            inbound: inbound.to_vec(),
            expected,
            number_floor: RoomNumber(floor),
        }
    }

    fn area_bytes(backend: &LocalBackend, id: AreaId) -> Vec<u8> {
        fs::read(backend.area_path(id)).expect("area bytes")
    }

    fn receipt_count(backend: &LocalBackend, id: AreaId) -> usize {
        let json: serde_json::Value =
            serde_json::from_slice(&area_bytes(backend, id)).expect("parse document");
        json["_smudgy_applied_operations"]
            .as_array()
            .map_or(0, Vec::len)
    }

    fn header_revs(backend: &LocalBackend) -> Vec<(AreaId, i64)> {
        let mut revs: Vec<(AreaId, i64)> = backend
            .headers()
            .values()
            .map(|area| (area.id, area.rev))
            .collect();
        revs.sort_by_key(|(id, _)| id.0);
        revs
    }

    #[tokio::test]
    async fn merge_areas_writes_destination_and_changed_third_parties_and_deletes_sources() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let (into, source, third) = merge_areas_fixture(&backend, true).await;
        // A v1-namespace copy of the source: the delete must clear it too,
        // or the straggler migration would resurrect the source on the
        // next scan.
        fs::create_dir_all(backend.legacy_areas_dir()).expect("legacy dir");
        fs::write(
            backend.legacy_area_path(source),
            area_bytes(&backend, source),
        )
        .expect("legacy copy");
        let plan = merge_plan(&backend, into, &[source], &[third]).await;

        let commit = backend.merge_areas(&plan).await.expect("merge");

        assert_eq!(
            commit.outcome.rooms,
            vec![RoomRemap {
                from: RoomKey::new(source, RoomNumber(1)),
                to: RoomNumber(2),
            }],
            "the colliding source room lands above the destination's"
        );
        let written: Vec<AreaId> = commit
            .documents
            .iter()
            .map(|document| document.area.id)
            .collect();
        assert_eq!(
            written,
            vec![into, third],
            "post-images: the destination, then the changed third party"
        );

        let destination = backend.get_area(&into).await.expect("destination");
        assert_eq!(destination.area.rev, 3);
        let mut numbers: Vec<i32> = destination
            .rooms
            .iter()
            .map(|room| room.room_number.0)
            .collect();
        numbers.sort_unstable();
        assert_eq!(numbers, vec![1, 2]);
        assert_eq!(
            serde_json::to_string(&destination).expect("serialize"),
            serde_json::to_string(&commit.documents[0]).expect("serialize"),
            "the returned post-image is what disk holds"
        );

        let third_party = backend.get_area(&third).await.expect("third party");
        assert_eq!(third_party.area.rev, 3);
        let exit = &third_party.rooms[0].exits[0];
        assert_eq!(exit.to_area_id, Some(into), "the inbound exit followed");
        assert_eq!(exit.to_room_number, Some(RoomNumber(2)));
        assert_eq!(
            serde_json::to_string(&third_party).expect("serialize"),
            serde_json::to_string(&commit.documents[1]).expect("serialize")
        );

        assert!(matches!(
            backend.get_area(&source).await,
            Err(CloudError::NotFoundOrNoAccess)
        ));
        assert!(!backend.area_path(source).exists(), "v2 copy removed");
        assert!(
            !backend.legacy_area_path(source).exists(),
            "v1 copy removed too"
        );
        assert_eq!(
            receipt_count(&backend, into),
            1,
            "the destination keeps its receipts"
        );
        assert_eq!(receipt_count(&backend, third), 1, "so does the third party");
        let mut expected_headers = vec![(into, 3), (third, 3)];
        expected_headers.sort_by_key(|(id, _)| id.0);
        assert_eq!(
            header_revs(&backend),
            expected_headers,
            "index mirrors disk"
        );
        assert!(
            multi_write_journals(&backend).is_empty(),
            "a completed merge retires its journal"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn merge_areas_revision_drift_stores_nothing() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let (into, source, third) = merge_areas_fixture(&backend, true).await;
        let mut plan = merge_plan(&backend, into, &[source], &[third]).await;
        for (id, rev) in &mut plan.expected {
            if *id == source {
                *rev += 1;
            }
        }
        let before: Vec<Vec<u8>> = [into, source, third]
            .iter()
            .map(|id| area_bytes(&backend, *id))
            .collect();
        let headers_before = header_revs(&backend);

        let result = backend.merge_areas(&plan).await;
        assert!(
            matches!(
                &result,
                Err(CloudError::RevisionConflict {
                    id,
                    expected_rev: 3,
                    current_rev: 2,
                }) if *id == source.0
            ),
            "a moved revision must conflict unchanged, got {result:?}"
        );

        let after: Vec<Vec<u8>> = [into, source, third]
            .iter()
            .map(|id| area_bytes(&backend, *id))
            .collect();
        assert_eq!(
            after, before,
            "a refused merge leaves every file byte-identical"
        );
        assert_eq!(header_revs(&backend), headers_before);
        assert!(
            multi_write_journals(&backend).is_empty(),
            "nothing was journaled"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn merge_areas_missing_source_is_area_not_found_and_writes_nothing() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let (into, source, third) = merge_areas_fixture(&backend, true).await;
        let mut plan = merge_plan(&backend, into, &[source], &[third]).await;
        let ghost = AreaId(Uuid::new_v4());
        plan.sources[0].id = ghost;
        plan.expected[1].0 = ghost;
        let before: Vec<Vec<u8>> = [into, source, third]
            .iter()
            .map(|id| area_bytes(&backend, *id))
            .collect();

        let result = backend.merge_areas(&plan).await;
        assert!(
            matches!(&result, Err(CloudError::AreaNotFound(id)) if *id == ghost),
            "a source with no document is AreaNotFound, got {result:?}"
        );

        let after: Vec<Vec<u8>> = [into, source, third]
            .iter()
            .map(|id| area_bytes(&backend, *id))
            .collect();
        assert_eq!(after, before);
        assert!(multi_write_journals(&backend).is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn store_lock_is_shared_by_directory_aliases_and_independent_between_roots() {
        let root = temp_root();
        let first = LocalBackend::new(&root);
        let alias = LocalBackend::new(root.join("."));
        let separate_root = temp_root();
        let separate = LocalBackend::new(&separate_root);
        let guard = first.lock_store().await.expect("first lock");
        let waiting = alias.lock_store();
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
                .await
                .is_err(),
            "an alias must wait for the existing store owner"
        );
        let independent =
            tokio::time::timeout(std::time::Duration::from_secs(5), separate.lock_store())
                .await
                .expect("another root is independent")
                .expect("separate lock");
        drop(guard);
        let alias_guard = waiting.await.expect("alias lock");
        assert!(Arc::ptr_eq(
            first.store.get().unwrap(),
            alias.store.get().unwrap()
        ));
        drop((alias_guard, independent));
        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&separate_root).ok();
    }

    #[tokio::test]
    async fn merge_waits_for_another_sessions_edit_then_checks_its_revision() {
        let root = temp_root();
        let writer = LocalBackend::new(&root);
        let (into, source, third) = merge_areas_fixture(&writer, true).await;
        let merging = LocalBackend::new(&root);
        merging
            .ensure_loaded()
            .await
            .expect("second session loaded");
        let plan = merge_plan(&writer, into, &[source], &[third]).await;

        // Pause an ordinary writer after its read, before its persistence.
        let guard = writer.lock_store().await.expect("writer lock");
        let mut document = writer.load_area_document(source).await.expect("source");
        let merge = merging.merge_areas(&plan);
        tokio::pin!(merge);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut merge)
                .await
                .is_err(),
            "another session's merge must wait for this edit"
        );
        document.details.area.name = "Edited in the other session".to_string();
        document.details.area.rev += 1;
        writer
            .store_area_document(document)
            .await
            .expect("persist edit");
        drop(guard);

        assert!(
            matches!(merge.await, Err(CloudError::RevisionConflict { id, .. }) if id == source.0)
        );
        let preserved = merging.get_area(&source).await.expect("source survives");
        assert_eq!(preserved.area.name, "Edited in the other session");
        assert_eq!(
            merging
                .get_area(&into)
                .await
                .expect("destination")
                .rooms
                .len(),
            1
        );
        assert!(multi_write_journals(&writer).is_empty());
        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn another_session_reads_the_old_snapshot_during_an_active_merge() {
        let root = temp_root();
        let writer = LocalBackend::new(&root);
        let (into, source, third) = merge_areas_fixture(&writer, true).await;
        let reader = LocalBackend::new(&root);
        reader.ensure_loaded().await.expect("second session loaded");
        let plan = merge_plan(&writer, into, &[source], &[third]).await;
        let guard = writer.lock_store().await.expect("transaction lock");
        let mut destination = writer.load_area_document(into).await.expect("destination");
        let mut sources = vec![writer.load_area(source).await.expect("source")];
        let mut inbound = vec![writer.load_area(third).await.expect("third party")];
        apply_area_merge(&plan, &mut destination.details, &mut sources, &mut inbound)
            .expect("apply");
        let transaction = LocalMultiWriteTransaction {
            writes: vec![destination, LocalAreaDocument::new(inbound.remove(0))],
            deletes: vec![source],
            ..Default::default()
        };
        let path = writer.multi_write_transaction_path(next_multi_write_sequence(), Uuid::new_v4());
        store_transaction_file(path.clone(), transaction.clone())
            .await
            .expect("journal");
        // Pause at the real commit boundary: the journal is durable but its
        // post-images have not landed. Readers must not treat it as abandoned.
        let retained = reader.snapshot().await.expect("pin old generation");
        let listed =
            tokio::time::timeout(std::time::Duration::from_millis(50), reader.list_areas())
                .await
                .expect("read does not wait")
                .expect("list");
        assert!(listed.iter().any(|area| area.id == source));
        assert_eq!(
            reader
                .get_area(&into)
                .await
                .expect("old destination")
                .rooms
                .len(),
            1
        );
        assert!(
            path.exists(),
            "the second session must not retire the journal"
        );
        assert!(writer.area_path(source).exists());
        writer
            .apply_committed_documents(&transaction)
            .await
            .expect("finish commit");
        remove_transaction_file(path).await.expect("retire journal");
        drop(guard);

        assert_eq!(
            retained
                .area(into)
                .expect("retained old destination")
                .rooms
                .len(),
            1
        );
        assert!(retained.area(source).is_ok());
        assert_eq!(
            reader
                .get_area(&into)
                .await
                .expect("merged destination")
                .rooms
                .len(),
            2
        );
        let listed = reader.list_areas().await.expect("committed listing");
        assert!(!listed.iter().any(|area| area.id == source));
        assert_eq!(
            reader.get_area(&third).await.expect("third party").rooms[0].exits[0].to_area_id,
            Some(into)
        );
        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn merge_areas_holds_the_write_lock_for_the_whole_span() {
        let root = temp_root();
        let backend = Arc::new(LocalBackend::new(&root));
        let (into, source, third) = merge_areas_fixture(&backend, true).await;
        let plan = merge_plan(&backend, into, &[source], &[third]).await;

        let write_guard = backend.lock_store().await.expect("store lock");
        let merge = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.merge_areas(&plan).await })
        };
        tokio::task::yield_now().await;
        assert!(
            !merge.is_finished(),
            "the merge must wait for an in-flight whole-file mutation"
        );

        drop(write_guard);
        let commit = merge.await.expect("merge task").expect("merge");
        assert_eq!(commit.documents[0].area.id, into);
        assert!(backend.get_area(&source).await.is_err());

        fs::remove_dir_all(&root).ok();
    }

    /// A source that gives up only some rooms is written like a third
    /// party, receipts and all, and stays on disk and in the header index
    /// at its new revision; only whole sources are deleted.
    #[tokio::test]
    async fn merge_areas_partial_source_is_written_with_its_receipts_and_kept() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let (into, source, third) = merge_areas_fixture(&backend, true).await;
        // A second room in the source, reaching room 1, so the source keeps
        // something and one of its exits must follow the moved room.
        backend
            .execute_mutation(
                &source,
                &envelope(
                    source,
                    2,
                    vec![
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(2),
                            body: RoomUpdates {
                                title: Some("Annex".to_string()),
                                ..RoomUpdates::default()
                            },
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(2),
                            body: ExitArgs {
                                from_direction: ExitDirection::South,
                                to_area_id: Some(source),
                                to_room_number: Some(RoomNumber(1)),
                                ..ExitArgs::default()
                            },
                        },
                    ],
                ),
            )
            .await
            .expect("second envelope");
        assert_eq!(receipt_count(&backend, source), 2);
        let mut plan = merge_plan(&backend, into, &[source], &[third]).await;
        plan.sources[0].rooms = Some(vec![RoomNumber(1)]);

        let commit = backend.merge_areas(&plan).await.expect("partial merge");

        assert_eq!(
            commit.outcome.rooms,
            vec![RoomRemap {
                from: RoomKey::new(source, RoomNumber(1)),
                to: RoomNumber(2),
            }]
        );
        let written: Vec<AreaId> = commit
            .documents
            .iter()
            .map(|document| document.area.id)
            .collect();
        assert_eq!(
            written,
            vec![into, source, third],
            "post-images: the destination, the partial source, the changed third party"
        );
        assert!(
            commit
                .outcome
                .versions
                .iter()
                .any(|version| version.id == source.0 && !version.deleted && version.rev == 4),
            "the partial source reports a write, not a delete: {:?}",
            commit.outcome.versions
        );

        let kept = backend.get_area(&source).await.expect("the source stays");
        assert_eq!(kept.area.rev, 4);
        assert_eq!(kept.rooms.len(), 1);
        assert_eq!(kept.rooms[0].room_number, RoomNumber(2));
        let exit = &kept.rooms[0].exits[0];
        assert_eq!(
            (exit.to_area_id, exit.to_room_number),
            (Some(into), Some(RoomNumber(2))),
            "the remaining room's exit follows the moved room"
        );
        assert_eq!(
            serde_json::to_string(&kept).expect("serialize"),
            serde_json::to_string(&commit.documents[1]).expect("serialize"),
            "the returned post-image is what disk holds"
        );
        assert!(backend.area_path(source).exists(), "not removed from disk");
        assert_eq!(
            receipt_count(&backend, source),
            2,
            "the partial source keeps its receipts"
        );
        let third_party = backend.get_area(&third).await.expect("third party");
        let exit = &third_party.rooms[0].exits[0];
        assert_eq!(
            (exit.to_area_id, exit.to_room_number),
            (Some(into), Some(RoomNumber(2)))
        );
        let mut expected_headers = vec![(into, 3), (source, 4), (third, 3)];
        expected_headers.sort_by_key(|(id, _)| id.0);
        assert_eq!(
            header_revs(&backend),
            expected_headers,
            "index mirrors disk"
        );
        assert!(multi_write_journals(&backend).is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn merge_areas_unchanged_third_party_is_not_rewritten() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let (into, source, third) = merge_areas_fixture(&backend, false).await;
        let plan = merge_plan(&backend, into, &[source], &[third]).await;
        let third_before = area_bytes(&backend, third);

        let commit = backend.merge_areas(&plan).await.expect("merge");

        let written: Vec<AreaId> = commit
            .documents
            .iter()
            .map(|document| document.area.id)
            .collect();
        assert_eq!(written, vec![into], "only the destination is a post-image");
        assert!(
            commit
                .outcome
                .versions
                .iter()
                .all(|version| version.id != third.0),
            "an untouched third party reports no version"
        );
        assert_eq!(
            area_bytes(&backend, third),
            third_before,
            "its file is byte-identical"
        );
        assert_eq!(backend.get_area(&third).await.expect("third").area.rev, 2);
        assert!(backend.get_area(&source).await.is_err());

        fs::remove_dir_all(&root).ok();
    }
}
