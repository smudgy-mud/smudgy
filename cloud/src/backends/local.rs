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
//! area consistently), and a direct `get_area` of an unscanned id migrates
//! on demand. Each migration first writes an untouched timestamped backup,
//! then atomically writes the migrated document into `areas-v2/`; only the
//! completed rename marks the migration done, and any failure leaves the v1
//! file intact and unopened (never a partial or empty replacement). Once a
//! v2 copy exists the v1 file is never re-read, so later edits made by an
//! old binary to the stale v1 namespace are deliberately not merged.
//! Documents newer than [`crate::AREA_FORMAT_VERSION`] are a hard read-only
//! error naming the file.
//!
//! An area's atlas membership lives in its own `atlas_id` field, so moving an
//! area between folders is a single-file rewrite and folder deletion just
//! clears the member areas' `atlas_id`.
//!
//! A lightweight in-memory index of area/atlas metadata is loaded once,
//! lazily, off the construction hot path (the first async call triggers the
//! disk scan via [`tokio::task::spawn_blocking`]); a large local store must
//! not stall startup. Mutations keep the index in lock-step with disk.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, OnceCell},
    task,
};
use uuid::Uuid;

use super::{MapperBackend, area_edits, local_migration};
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, MapStorage,
    mutation::{MutationEnvelope, MutationResult},
};

const LOCAL_OPERATION_RECEIPT_LIMIT: usize = 1024;

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

/// On-disk authoritative map store. Cheaply shareable behind an `Arc`.
pub struct LocalBackend {
    root: PathBuf,
    /// Lightweight metadata index (area/atlas headers), so `list_areas` and
    /// sync-row synthesis don't re-read every file. Mirrors disk exactly.
    areas: RwLock<HashMap<AreaId, Area>>,
    atlases: RwLock<HashMap<AtlasId, Atlas>>,
    /// Drives the one-time lazy scan that fills the index.
    loaded: OnceCell<()>,
    /// Serializes read-modify-write of area files. Local writes are
    /// user-driven and infrequent, but a direct create (run on the UI task)
    /// can overlap a queued mutation on the same area; without this, the
    /// whole-file rewrite would drop one of the two writes.
    write_lock: Mutex<()>,
}

impl std::fmt::Debug for LocalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalBackend")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl LocalBackend {
    /// Creates a backend rooted at `root`. **Does no disk IO** — neither the
    /// directory scan nor `mkdir` happens here, so a large store can't stall
    /// the construction hot path; both are deferred to the first async call.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            areas: RwLock::new(HashMap::new()),
            atlases: RwLock::new(HashMap::new()),
            loaded: OnceCell::new(),
            write_lock: Mutex::new(()),
        }
    }

    /// The authoritative v2 area namespace.
    fn areas_dir(&self) -> PathBuf {
        self.root.join("areas-v2")
    }

    /// The v1 namespace: only ever read (as a migration source); v2 code
    /// never writes here, so an old binary's files stay recoverable.
    fn legacy_areas_dir(&self) -> PathBuf {
        self.root.join("areas")
    }

    /// Untouched pre-migration v1 bytes, one timestamped file per migration.
    fn backup_dir(&self) -> PathBuf {
        self.root.join("areas-v1-backup")
    }

    fn atlases_dir(&self) -> PathBuf {
        self.root.join("atlases")
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

    /// Scans both directories once and fills the in-memory index. Idempotent;
    /// concurrent callers share one scan. Used by the create/mutate paths,
    /// which read the authoritative file fresh anyway and only need the index
    /// to be non-empty.
    async fn ensure_loaded(&self) {
        self.loaded.get_or_init(|| self.reload()).await;
    }

    /// Rebuilds the in-memory index from disk. Called by `list_*` (and atlas
    /// rename/delete) so the listing reflects external changes — notably
    /// another session's `LocalBackend` writing to the same shared local
    /// directory; the index alone would otherwise be a stale one-shot snapshot.
    async fn reload(&self) {
        let areas_dir = self.areas_dir();
        let legacy_dir = self.legacy_areas_dir();
        let backup_dir = self.backup_dir();
        let atlases_dir = self.atlases_dir();
        match task::spawn_blocking(move || {
            (
                scan_areas(&areas_dir, &legacy_dir, &backup_dir),
                scan_atlases(&atlases_dir),
            )
        })
        .await
        {
            Ok((areas, atlases)) => {
                *self.areas.write() = areas;
                *self.atlases.write() = atlases;
                // A reload also satisfies the one-shot `ensure_loaded` guard.
                let _ = self.loaded.set(());
            }
            Err(err) => log::warn!("local map store scan failed: {err}"),
        }
    }

    /// Reads one area's full record from disk: the v2 namespace first, and
    /// only when it has no copy, the v1 namespace via on-demand migration.
    async fn load_area_document(&self, id: AreaId) -> CloudResult<LocalAreaDocument> {
        let v2_path = self.area_path(id);
        let legacy_path = self.legacy_area_path(id);
        let backup_dir = self.backup_dir();
        task::spawn_blocking(move || -> CloudResult<LocalAreaDocument> {
            match fs::read(&v2_path) {
                Ok(bytes) => parse_v2_document(&bytes, &v2_path),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    let legacy_bytes = fs::read(&legacy_path).map_err(|err| match err.kind() {
                        io::ErrorKind::NotFound => CloudError::NotFoundOrNoAccess,
                        _ => CloudError::from(err),
                    })?;
                    migrate_legacy_file(&legacy_bytes, &legacy_path, &v2_path, &backup_dir)
                        .map(LocalAreaDocument::new)
                }
                Err(err) => Err(CloudError::from(err)),
            }
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))?
    }

    async fn load_area(&self, id: AreaId) -> CloudResult<AreaWithDetails> {
        self.load_area_document(id)
            .await
            .map(|document| document.details)
    }

    /// Writes one area's full record to disk (creating the directory if
    /// needed) and refreshes its index entry. The write is atomic
    /// (temp file + rename) so a concurrent reader never sees a torn file.
    async fn store_area(&self, area: AreaWithDetails) -> CloudResult<()> {
        self.store_area_document(LocalAreaDocument::new(area)).await
    }

    async fn store_area_document(&self, document: LocalAreaDocument) -> CloudResult<()> {
        let dir = self.areas_dir();
        let path = self.area_path(document.details.area.id);
        let to_write = document.clone();
        task::spawn_blocking(move || -> CloudResult<()> {
            fs::create_dir_all(&dir)?;
            write_atomic(&path, &serde_json::to_vec_pretty(&to_write)?)?;
            Ok(())
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))??;
        self.areas
            .write()
            .insert(document.details.area.id, document.details.area);
        Ok(())
    }

    /// Read-modify-write one area: loads it, applies `f` (which may mutate it
    /// and returns a value), bumps `rev`, persists, and updates the index.
    /// The `write_lock` makes the load→store sequence atomic against other
    /// local writers, so an overlapping mutation can't drop this one.
    async fn mutate_area<R, F>(&self, area_id: AreaId, f: F) -> CloudResult<R>
    where
        F: FnOnce(&mut AreaWithDetails) -> CloudResult<R> + Send,
        R: Send,
    {
        self.ensure_loaded().await;
        let _guard = self.write_lock.lock().await;
        let mut document = self.load_area_document(area_id).await?;
        let result = f(&mut document.details)?;
        document.details.area.rev += 1;
        self.store_area_document(document).await?;
        Ok(result)
    }

    async fn store_atlas(&self, atlas: Atlas) -> CloudResult<()> {
        let dir = self.atlases_dir();
        let path = self.atlas_path(atlas.id);
        let to_write = atlas.clone();
        task::spawn_blocking(move || -> CloudResult<()> {
            fs::create_dir_all(&dir)?;
            write_atomic(&path, &serde_json::to_vec_pretty(&to_write)?)?;
            Ok(())
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))??;
        self.atlases.write().insert(atlas.id, atlas);
        Ok(())
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
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
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
    let version = document_version(bytes)?;
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
    Ok(serde_json::from_slice(bytes)?)
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
    v2_path: &Path,
    backup_dir: &Path,
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

        local_migration::migrate_v1(legacy)
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
fn scan_areas(dir: &Path, legacy_dir: &Path, backup_dir: &Path) -> HashMap<AreaId, Area> {
    let mut out: HashMap<AreaId, Area> = HashMap::new();
    let mut migrated: HashSet<AreaId> = HashSet::new();
    let scanned = read_json_dir(dir, |bytes, path| match parse_v2_document(bytes, path) {
        Ok(document) => Some((document.details.area.id, document.details.area)),
        Err(err) => {
            log::warn!("skipping local map file: {err}");
            None
        }
    });
    for (id, area) in scanned {
        migrated.insert(id);
        out.insert(id, area);
    }

    let Ok(entries) = fs::read_dir(legacy_dir) else {
        return out; // no v1 namespace => nothing to migrate
    };
    for entry in entries.flatten() {
        let path = entry.path();
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
            Err(err) => {
                log::warn!("failed to read local map file {}: {err}", path.display());
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
        match migrate_legacy_file(&bytes, &path, &v2_path, backup_dir) {
            Ok(details) => {
                migrated.insert(id);
                out.insert(details.area.id, details.area);
            }
            Err(err) => log::warn!("local map migration failed: {err}"),
        }
    }
    out
}

/// Serde probe for a document's area id.
#[derive(Deserialize)]
struct AreaIdProbe {
    id: Uuid,
}

/// Reads every `*.json` under `dir` as an [`Atlas`] manifest, keyed by id.
fn scan_atlases(dir: &Path) -> HashMap<AtlasId, Atlas> {
    read_json_dir(dir, |bytes, path| {
        let parsed = serde_json::from_slice::<Atlas>(bytes).ok();
        if parsed.is_none() {
            log::warn!("skipping unreadable local map file {}", path.display());
        }
        parsed.map(|atlas| (atlas.id, atlas))
    })
}

fn read_json_dir<K, V>(dir: &Path, parse: impl Fn(&[u8], &Path) -> Option<(K, V)>) -> HashMap<K, V>
where
    K: std::hash::Hash + Eq,
{
    let mut out = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out; // missing dir => empty store
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_json = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        match fs::read(&path) {
            Ok(bytes) => {
                if let Some((k, v)) = parse(&bytes, &path) {
                    out.insert(k, v);
                }
            }
            Err(err) => log::warn!("failed to read local map file {}: {err}", path.display()),
        }
    }
    out
}

#[async_trait]
impl MapperBackend for LocalBackend {
    // Local areas are always the viewer's own and never need a credential.
    fn has_credential(&self) -> bool {
        true
    }

    // ===== AREA OPERATIONS =====

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        self.ensure_loaded().await;
        let area = Area {
            id: AreaId(Uuid::new_v4()),
            user_id: None,
            atlas_id: request.atlas_id,
            // Local areas keep no atlas name; the folder tree is local.
            atlas_name: None,
            name: request.name,
            created_at: Utc::now(),
            rev: 1,
            access: Some(AreaAccess::OWNER),
            owner_nickname: None,
            copied_from_area_id: None,
            copied_from_rev: None,
            copied_at: None,
            // Local areas are never synced, so they never carry a server-
            // issued per-viewer family token.
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
        self.store_area(details).await?;
        Ok(area)
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
        self.ensure_loaded().await;
        self.store_area(details).await
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        self.reload().await;
        Ok(self.areas.read().values().cloned().collect())
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.ensure_loaded().await;
        self.load_area(*area_id).await
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.mutate_area(*area_id, move |area| {
            if let Some(name) = updates.name {
                area.area.name = name;
            }
            // `Option<Option<_>>`: present sets (or clears), absent leaves it.
            if let Some(atlas_id) = updates.atlas_id {
                area.area.atlas_id = atlas_id;
            }
            Ok(())
        })
        .await
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        self.ensure_loaded().await;
        // Serialize deletion with versioned mutation persistence. Without
        // this lock, a worker can load the area, deletion can unlink it, and
        // the worker can then store its stale working copy back to disk.
        let _guard = self.write_lock.lock().await;
        let path = self.area_path(*area_id);
        // The v1-namespace file goes too: deleting only the v2 copy would
        // resurrect the area through the straggler migration on the next
        // scan. Timestamped backups stay — they are recovery, not state.
        let legacy_path = self.legacy_area_path(*area_id);
        task::spawn_blocking(move || {
            for target in [&path, &legacy_path] {
                match fs::remove_file(target) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(CloudError::from(err)),
                }
            }
            Ok(())
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))??;
        self.areas.write().remove(area_id);
        Ok(())
    }

    // ===== VERSIONED MUTATIONS =====

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        // The compare-and-set write path: the shared applier owns the
        // precondition check and the single revision bump, so this bypasses
        // `mutate_area` (whose unconditional bump would double-count) while
        // keeping the same lock + load + store shape. Any applier error
        // discards the working copy before it reaches disk, so a failed
        // envelope changes nothing.
        self.ensure_loaded().await;
        let _guard = self.write_lock.lock().await;
        let mut document = self
            .load_area_document(*area_id)
            .await
            .map_err(|err| match err {
                CloudError::NotFoundOrNoAccess => CloudError::AreaNotFound(*area_id),
                other => other,
            })?;
        if let Some(result) = document.receipt(envelope.operation_id) {
            return Ok(result);
        }
        let result = area_edits::apply_envelope(&mut document.details, *area_id, envelope)?;
        document.remember(result.clone());
        self.store_area_document(document).await?;
        Ok(result)
    }

    // ===== ATLAS (FOLDER) OPERATIONS =====

    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        self.reload().await;
        let areas = self.areas.read();
        let atlases = self.atlases.read();
        let mut items: Vec<AtlasListItem> = atlases
            .values()
            .map(|atlas| AtlasListItem {
                id: atlas.id,
                name: atlas.name.clone(),
                created_at: atlas.created_at,
                area_count: i64::try_from(
                    areas
                        .values()
                        .filter(|area| area.atlas_id == Some(atlas.id))
                        .count(),
                )
                .unwrap_or(i64::MAX),
                rev: atlas.rev,
                // Local atlases are owned by the user (no sharing on the local tier).
                is_owner: true,
                can_admin: true,
                owner_nickname: None,
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(items)
    }

    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        self.ensure_loaded().await;
        let atlas = Atlas {
            id: AtlasId(Uuid::new_v4()),
            user_id: None,
            name: name.to_string(),
            created_at: Utc::now(),
            rev: 1,
        };
        self.store_atlas(atlas.clone()).await?;
        Ok(atlas)
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
        self.reload().await;
        let mut atlas = self
            .atlases
            .read()
            .get(atlas_id)
            .cloned()
            .ok_or(CloudError::NotFoundOrNoAccess)?;
        atlas.name = name.to_string();
        atlas.rev += 1;
        self.store_atlas(atlas.clone()).await?;
        Ok(atlas)
    }

    async fn delete_atlas(&self, atlas_id: &AtlasId) -> CloudResult<()> {
        self.reload().await;
        // Gentle delete: member areas survive and become loose.
        let members: Vec<AreaId> = self
            .areas
            .read()
            .values()
            .filter(|area| area.atlas_id == Some(*atlas_id))
            .map(|area| area.id)
            .collect();
        // Detach every member first, all-or-nothing: if any fails we leave the
        // atlas in place (and propagate) rather than removing it while areas
        // still point at it on disk.
        for area_id in members {
            self.mutate_area(area_id, |area| {
                area.area.atlas_id = None;
                Ok(())
            })
            .await?;
        }

        let path = self.atlas_path(*atlas_id);
        task::spawn_blocking(move || match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(CloudError::from(err)),
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))??;
        self.atlases.write().remove(atlas_id);
        Ok(())
    }

    fn local_atlas_ids(&self) -> HashSet<AtlasId> {
        self.atlases.read().keys().copied().collect()
    }

    fn local_area_ids(&self) -> HashSet<AreaId> {
        self.areas.read().keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ExitArgs, ExitDirection, ExitId, LabelArgs, LabelId, RoomNumber, RoomUpdates, ShapeArgs,
        ShapeId,
        mutation::{AreaMutation, OpResult, Precondition, ResourceKind},
    };
    use std::sync::Arc;

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

        let write_guard = backend.write_lock.lock().await;
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
}
