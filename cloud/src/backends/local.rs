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
//! <root>/areas/<area_id>.json     one AreaWithDetails per area (the bytes are owned here)
//! <root>/atlases/<atlas_id>.json  one Atlas manifest per folder
//! ```
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
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use tokio::{
    sync::{Mutex, OnceCell},
    task,
};
use uuid::Uuid;

use super::{MapperBackend, area_edits};
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CreateAreaRequest, Exit, ExitArgs, ExitId, ExitUpdates, Label, LabelArgs, LabelId,
    LabelUpdates, CloudError, CloudResult, Room, RoomUpdates, Shape, ShapeArgs, ShapeId,
    ShapeUpdates, mapper::RoomKey,
};

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

    fn areas_dir(&self) -> PathBuf {
        self.root.join("areas")
    }

    fn atlases_dir(&self) -> PathBuf {
        self.root.join("atlases")
    }

    fn area_path(&self, id: AreaId) -> PathBuf {
        self.areas_dir().join(format!("{id}.json"))
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
        let atlases_dir = self.atlases_dir();
        match task::spawn_blocking(move || (scan_areas(&areas_dir), scan_atlases(&atlases_dir)))
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

    /// Reads one area's full record from disk.
    async fn load_area(&self, id: AreaId) -> CloudResult<AreaWithDetails> {
        let path = self.area_path(id);
        task::spawn_blocking(move || -> CloudResult<AreaWithDetails> {
            let bytes = fs::read(&path).map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => CloudError::NotFoundOrNoAccess,
                _ => CloudError::from(err),
            })?;
            Ok(serde_json::from_slice(&bytes)?)
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))?
    }

    /// Writes one area's full record to disk (creating the directory if
    /// needed) and refreshes its index entry. The write is atomic
    /// (temp file + rename) so a concurrent reader never sees a torn file.
    async fn store_area(&self, area: AreaWithDetails) -> CloudResult<()> {
        let dir = self.areas_dir();
        let path = self.area_path(area.area.id);
        let to_write = area.clone();
        task::spawn_blocking(move || -> CloudResult<()> {
            fs::create_dir_all(&dir)?;
            write_atomic(&path, &serde_json::to_vec_pretty(&to_write)?)?;
            Ok(())
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))??;
        self.areas.write().insert(area.area.id, area.area);
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
        let mut area = self.load_area(area_id).await?;
        let result = f(&mut area)?;
        area.area.rev += 1;
        self.store_area(area).await?;
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

/// Writes `bytes` to `path` atomically: a sibling temp file then a rename, so
/// a reader sees either the old or the new file, never a half-written one. A
/// leftover `.tmp` from a crash is ignored by the `.json`-only scan.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Reads every `*.json` under `dir` as an [`AreaWithDetails`], keyed by id.
fn scan_areas(dir: &Path) -> HashMap<AreaId, Area> {
    read_json_dir(dir, |bytes| {
        serde_json::from_slice::<AreaWithDetails>(bytes)
            .ok()
            .map(|details| (details.area.id, details.area))
    })
}

/// Reads every `*.json` under `dir` as an [`Atlas`] manifest, keyed by id.
fn scan_atlases(dir: &Path) -> HashMap<AtlasId, Atlas> {
    read_json_dir(dir, |bytes| {
        serde_json::from_slice::<Atlas>(bytes)
            .ok()
            .map(|atlas| (atlas.id, atlas))
    })
}

fn read_json_dir<K, V>(dir: &Path, parse: impl Fn(&[u8]) -> Option<(K, V)>) -> HashMap<K, V>
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
                if let Some((k, v)) = parse(&bytes) {
                    out.insert(k, v);
                } else {
                    log::warn!("skipping unreadable local map file {}", path.display());
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
            content_hash: None,
            properties: Vec::new(),
            rooms: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            linked_areas: Vec::new(),
        };
        self.store_area(details).await?;
        Ok(area)
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
        let path = self.area_path(*area_id);
        task::spawn_blocking(move || match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(CloudError::from(err)),
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))??;
        self.areas.write().remove(area_id);
        Ok(())
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
        };
        self.store_atlas(atlas.clone()).await?;
        Ok(atlas)
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

    // ===== AREA PROPERTIES =====

    async fn set_area_property(&self, area_id: &AreaId, name: &str, value: &str) -> CloudResult<()> {
        let name = name.to_string();
        let value = value.to_string();
        self.mutate_area(*area_id, move |area| {
            area_edits::upsert_property(&mut area.properties, &name, &value);
            Ok(())
        })
        .await
    }

    async fn delete_area_property(&self, area_id: &AreaId, name: &str) -> CloudResult<()> {
        let name = name.to_string();
        self.mutate_area(*area_id, move |area| {
            area.properties.retain(|p| p.name != name);
            Ok(())
        })
        .await
    }

    // ===== ROOM OPERATIONS =====

    async fn update_room(&self, room_key: &RoomKey, updates: RoomUpdates) -> CloudResult<Room> {
        let area_id = room_key.area_id;
        let number = room_key.room_number;
        self.mutate_area(area_id, move |area| {
            Ok(area_edits::upsert_room(area, area_id, number, &updates))
        })
        .await
    }

    async fn delete_room(&self, room_key: &RoomKey) -> CloudResult<()> {
        let area_id = room_key.area_id;
        let number = room_key.room_number;
        self.mutate_area(area_id, move |area| {
            area_edits::delete_room(area, area_id, number);
            Ok(())
        })
        .await
    }

    // ===== ROOM PROPERTIES =====

    async fn set_room_property(
        &self,
        room_key: &RoomKey,
        name: &str,
        value: &str,
    ) -> CloudResult<()> {
        let number = room_key.room_number;
        let key = room_key.clone();
        let name = name.to_string();
        let value = value.to_string();
        self.mutate_area(room_key.area_id, move |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == number)
                .ok_or(CloudError::RoomNotFound(key))?;
            area_edits::upsert_property(&mut room.properties, &name, &value);
            Ok(())
        })
        .await
    }

    async fn delete_room_property(&self, room_key: &RoomKey, name: &str) -> CloudResult<()> {
        let number = room_key.room_number;
        let key = room_key.clone();
        let name = name.to_string();
        self.mutate_area(room_key.area_id, move |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == number)
                .ok_or(CloudError::RoomNotFound(key))?;
            room.properties.retain(|p| p.name != name);
            Ok(())
        })
        .await
    }

    // ===== ROOM TAGS =====

    async fn add_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        let number = room_key.room_number;
        let key = room_key.clone();
        let tag = crate::mapper::normalize_tag(tag);
        self.mutate_area(room_key.area_id, move |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == number)
                .ok_or(CloudError::RoomNotFound(key))?;
            room.tags.insert(tag);
            Ok(())
        })
        .await
    }

    async fn remove_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        let number = room_key.room_number;
        let key = room_key.clone();
        let tag = crate::mapper::normalize_tag(tag);
        self.mutate_area(room_key.area_id, move |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == number)
                .ok_or(CloudError::RoomNotFound(key))?;
            room.tags.remove(&tag);
            Ok(())
        })
        .await
    }

    // ===== EXIT OPERATIONS =====

    async fn create_room_exit(&self, room_key: &RoomKey, exit_data: ExitArgs) -> CloudResult<Exit> {
        let key = room_key.clone();
        self.mutate_area(room_key.area_id, move |area| {
            area_edits::create_room_exit(area, &key, exit_data)
        })
        .await
    }

    async fn update_exit(
        &self,
        area_id: &AreaId,
        exit_id: &ExitId,
        updates: ExitUpdates,
    ) -> CloudResult<()> {
        let exit_id = *exit_id;
        self.mutate_area(*area_id, move |area| {
            let exit = area
                .rooms
                .iter_mut()
                .flat_map(|room| room.exits.iter_mut())
                .find(|exit| exit.id == exit_id)
                .ok_or(CloudError::ExitNotFound(exit_id))?;
            area_edits::apply_exit_updates(exit, updates);
            Ok(())
        })
        .await
    }

    async fn delete_exit(&self, area_id: &AreaId, exit_id: &ExitId) -> CloudResult<()> {
        let exit_id = *exit_id;
        self.mutate_area(*area_id, move |area| {
            for room in &mut area.rooms {
                room.exits.retain(|exit| exit.id != exit_id);
            }
            Ok(())
        })
        .await
    }

    // ===== LABEL OPERATIONS =====

    async fn create_label(&self, area_id: &AreaId, label_data: LabelArgs) -> CloudResult<Label> {
        self.mutate_area(*area_id, move |area| {
            let label = Label {
                id: LabelId(Uuid::new_v4()),
                level: label_data.level,
                x: label_data.x,
                y: label_data.y,
                width: label_data.width,
                height: label_data.height,
                horizontal_alignment: label_data.horizontal_alignment,
                vertical_alignment: label_data.vertical_alignment,
                text: label_data.text,
                color: label_data.color,
                background_color: label_data.background_color.unwrap_or_default(),
                font_size: label_data.font_size,
                font_weight: label_data.font_weight,
                is_secret: label_data.is_secret.unwrap_or(false),
            };
            area.labels.push(label.clone());
            Ok(label)
        })
        .await
    }

    async fn update_label(
        &self,
        area_id: &AreaId,
        label_id: &LabelId,
        updates: LabelUpdates,
    ) -> CloudResult<()> {
        let label_id = *label_id;
        self.mutate_area(*area_id, move |area| {
            let label = area
                .labels
                .iter_mut()
                .find(|label| label.id == label_id)
                .ok_or(CloudError::LabelNotFound(label_id))?;
            *label = updates.apply(label);
            Ok(())
        })
        .await
    }

    async fn delete_label(&self, area_id: &AreaId, label_id: &LabelId) -> CloudResult<()> {
        let label_id = *label_id;
        self.mutate_area(*area_id, move |area| {
            area.labels.retain(|label| label.id != label_id);
            Ok(())
        })
        .await
    }

    // ===== SHAPE OPERATIONS =====

    async fn create_shape(&self, area_id: &AreaId, shape_data: ShapeArgs) -> CloudResult<Shape> {
        self.mutate_area(*area_id, move |area| {
            let shape = Shape {
                id: ShapeId(Uuid::new_v4()),
                level: shape_data.level,
                x: shape_data.x,
                y: shape_data.y,
                width: shape_data.width,
                height: shape_data.height,
                background_color: shape_data.background_color,
                stroke_color: shape_data.stroke_color,
                shape_type: shape_data.shape_type,
                border_radius: shape_data.border_radius,
                stroke_width: shape_data.stroke_width.unwrap_or(1.0),
                is_secret: shape_data.is_secret.unwrap_or(false),
            };
            area.shapes.push(shape.clone());
            Ok(shape)
        })
        .await
    }

    async fn update_shape(
        &self,
        area_id: &AreaId,
        shape_id: &ShapeId,
        updates: ShapeUpdates,
    ) -> CloudResult<()> {
        let shape_id = *shape_id;
        self.mutate_area(*area_id, move |area| {
            let shape = area
                .shapes
                .iter_mut()
                .find(|shape| shape.id == shape_id)
                .ok_or(CloudError::ShapeNotFound(shape_id))?;
            *shape = updates.apply(shape);
            Ok(())
        })
        .await
    }

    async fn delete_shape(&self, area_id: &AreaId, shape_id: &ShapeId) -> CloudResult<()> {
        let shape_id = *shape_id;
        self.mutate_area(*area_id, move |area| {
            area.shapes.retain(|shape| shape.id != shape_id);
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExitDirection, RoomNumber};

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
        assert_eq!(backend.get_area(&area.id).await.unwrap().area.atlas_id, None);

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

        let key = RoomKey::new(area.id, RoomNumber(1));
        backend
            .update_room(
                &key,
                RoomUpdates {
                    title: Some("Hall".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .await
            .expect("upsert room");
        let exit = backend
            .create_room_exit(
                &key,
                ExitArgs {
                    from_direction: ExitDirection::North,
                    ..ExitArgs::default()
                },
            )
            .await
            .expect("create exit");

        let details = backend.get_area(&area.id).await.expect("get");
        assert!(details.area.rev > 1, "mutations bump rev");
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].title, "Hall");
        assert_eq!(details.rooms[0].exits.len(), 1);
        assert_eq!(details.rooms[0].exits[0].id, exit.id);

        backend
            .delete_exit(&area.id, &exit.id)
            .await
            .expect("del exit");
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
    async fn delete_room_nulls_same_area_inbound_exits() {
        let root = temp_root();
        let backend = LocalBackend::new(&root);
        let area = backend
            .create_area(new_area_request("A", None))
            .await
            .expect("area");

        let k1 = RoomKey::new(area.id, RoomNumber(1));
        backend
            .update_room(&k1, RoomUpdates::default())
            .await
            .expect("r1");
        backend
            .update_room(&RoomKey::new(area.id, RoomNumber(2)), RoomUpdates::default())
            .await
            .expect("r2");
        backend
            .create_room_exit(
                &k1,
                ExitArgs {
                    from_direction: ExitDirection::North,
                    to_area_id: Some(area.id),
                    to_room_number: Some(RoomNumber(2)),
                    ..ExitArgs::default()
                },
            )
            .await
            .expect("exit");

        backend
            .delete_room(&RoomKey::new(area.id, RoomNumber(2)))
            .await
            .expect("delete r2");

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.rooms.len(), 1, "room 2 removed");
        let exit = &details.rooms[0].exits[0];
        assert_eq!(exit.to_area_id, None, "inbound exit cleared");
        assert_eq!(exit.to_room_number, None);

        fs::remove_dir_all(&root).ok();
    }
}
