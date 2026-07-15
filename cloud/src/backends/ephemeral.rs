//! In-memory, session-lifetime map storage — the ephemeral tier.
//!
//! [`EphemeralBackend`] is a [`MapperBackend`] whose areas live purely in
//! memory: never written to disk, never synced, gone when the owning
//! session's mapper drops. It is where protocol-driven auto-mapping (GMCP /
//! MSDP) lands by default, so an unknown or hostile server can never touch
//! the user's real maps; keeping an ephemeral area is an explicit copy into
//! the local tier (export → import), not a mode switch.
//!
//! The composite backend owns one per session and routes by membership (see
//! [`super::composite`]). `supports_sync` stays `false`, and the sync
//! operations the mapper fires after its optimistic cache writes terminate
//! here in a `HashMap` update — no HTTP, no disk, no serialization.
//!
//! Ephemeral areas are always loose: this tier has no folder (atlas) notion,
//! so the atlas operations keep their unsupported defaults.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use uuid::Uuid;

use super::{MapperBackend, area_edits};
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, CloudError, CloudResult,
    CreateAreaRequest, Exit, ExitArgs, ExitId, ExitUpdates, Label, LabelArgs, LabelId,
    LabelUpdates, Room, RoomUpdates, Shape, ShapeArgs, ShapeId, ShapeUpdates,
    mapper::RoomKey,
};

/// In-memory authoritative map store for session-lifetime areas. Cheaply
/// shareable behind an `Arc`.
#[derive(Default)]
pub struct EphemeralBackend {
    areas: RwLock<HashMap<AreaId, AreaWithDetails>>,
}

impl std::fmt::Debug for EphemeralBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralBackend")
            .field("areas", &self.areas.read().len())
            .finish()
    }
}

impl EphemeralBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-modify-write one area under the store lock: applies `f` and bumps
    /// `rev` (promotion snapshots and sync-row synthesis both read it).
    fn mutate<R>(
        &self,
        area_id: AreaId,
        f: impl FnOnce(&mut AreaWithDetails) -> CloudResult<R>,
    ) -> CloudResult<R> {
        let mut areas = self.areas.write();
        let area = areas.get_mut(&area_id).ok_or(CloudError::NotFoundOrNoAccess)?;
        let result = f(area)?;
        area.area.rev += 1;
        Ok(result)
    }
}

#[async_trait]
impl MapperBackend for EphemeralBackend {
    // ===== AREA OPERATIONS =====

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        let area = Area {
            id: AreaId(Uuid::new_v4()),
            user_id: None,
            // Always loose: the ephemeral tier has no folders.
            atlas_id: None,
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
            content_hash: None,
            properties: Vec::new(),
            rooms: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            linked_areas: Vec::new(),
        };
        self.areas.write().insert(area.id, details);
        Ok(area)
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        Ok(self
            .areas
            .read()
            .values()
            .map(|details| details.area.clone())
            .collect())
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.areas
            .read()
            .get(area_id)
            .cloned()
            .ok_or(CloudError::NotFoundOrNoAccess)
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.mutate(*area_id, move |area| {
            if let Some(name) = updates.name {
                area.area.name = name;
            }
            // Filing into a folder is a cross-tier move; the composite rejects
            // it before it gets here, so this is the defensive backstop.
            if matches!(updates.atlas_id, Some(Some(_))) {
                return Err(CloudError::InvalidInput(
                    "session maps can't be filed into folders".to_string(),
                ));
            }
            Ok(())
        })
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        self.areas.write().remove(area_id);
        Ok(())
    }

    // ===== AREA PROPERTIES =====

    async fn set_area_property(&self, area_id: &AreaId, name: &str, value: &str) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            area_edits::upsert_property(&mut area.properties, name, value);
            Ok(())
        })
    }

    async fn delete_area_property(&self, area_id: &AreaId, name: &str) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            area.properties.retain(|p| p.name != name);
            Ok(())
        })
    }

    // ===== ROOM OPERATIONS =====

    async fn update_room(&self, room_key: &RoomKey, updates: RoomUpdates) -> CloudResult<Room> {
        let area_id = room_key.area_id;
        let number = room_key.room_number;
        self.mutate(area_id, |area| {
            Ok(area_edits::upsert_room(area, area_id, number, &updates))
        })
    }

    async fn delete_room(&self, room_key: &RoomKey) -> CloudResult<()> {
        self.mutate(room_key.area_id, |area| {
            area_edits::delete_room(area, room_key.area_id, room_key.room_number);
            Ok(())
        })
    }

    // ===== ROOM PROPERTIES =====

    async fn set_room_property(
        &self,
        room_key: &RoomKey,
        name: &str,
        value: &str,
    ) -> CloudResult<()> {
        self.mutate(room_key.area_id, |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == room_key.room_number)
                .ok_or_else(|| CloudError::RoomNotFound(room_key.clone()))?;
            area_edits::upsert_property(&mut room.properties, name, value);
            Ok(())
        })
    }

    async fn delete_room_property(&self, room_key: &RoomKey, name: &str) -> CloudResult<()> {
        self.mutate(room_key.area_id, |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == room_key.room_number)
                .ok_or_else(|| CloudError::RoomNotFound(room_key.clone()))?;
            room.properties.retain(|p| p.name != name);
            Ok(())
        })
    }

    // ===== ROOM TAGS =====

    async fn add_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        let tag = crate::mapper::normalize_tag(tag);
        self.mutate(room_key.area_id, |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == room_key.room_number)
                .ok_or_else(|| CloudError::RoomNotFound(room_key.clone()))?;
            room.tags.insert(tag);
            Ok(())
        })
    }

    async fn remove_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        let tag = crate::mapper::normalize_tag(tag);
        self.mutate(room_key.area_id, |area| {
            let room = area
                .rooms
                .iter_mut()
                .find(|r| r.room_number == room_key.room_number)
                .ok_or_else(|| CloudError::RoomNotFound(room_key.clone()))?;
            room.tags.remove(&tag);
            Ok(())
        })
    }

    // ===== EXIT OPERATIONS =====

    async fn create_room_exit(&self, room_key: &RoomKey, exit_data: ExitArgs) -> CloudResult<Exit> {
        self.mutate(room_key.area_id, |area| {
            area_edits::create_room_exit(area, room_key, exit_data)
        })
    }

    async fn update_exit(
        &self,
        area_id: &AreaId,
        exit_id: &ExitId,
        updates: ExitUpdates,
    ) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            let exit = area
                .rooms
                .iter_mut()
                .flat_map(|room| room.exits.iter_mut())
                .find(|exit| exit.id == *exit_id)
                .ok_or(CloudError::ExitNotFound(*exit_id))?;
            area_edits::apply_exit_updates(exit, updates);
            Ok(())
        })
    }

    async fn delete_exit(&self, area_id: &AreaId, exit_id: &ExitId) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            for room in &mut area.rooms {
                room.exits.retain(|exit| exit.id != *exit_id);
            }
            Ok(())
        })
    }

    // ===== LABEL OPERATIONS =====

    async fn create_label(&self, area_id: &AreaId, label_data: LabelArgs) -> CloudResult<Label> {
        self.mutate(*area_id, |area| {
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
    }

    async fn update_label(
        &self,
        area_id: &AreaId,
        label_id: &LabelId,
        updates: LabelUpdates,
    ) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            let label = area
                .labels
                .iter_mut()
                .find(|label| label.id == *label_id)
                .ok_or(CloudError::LabelNotFound(*label_id))?;
            *label = updates.apply(label);
            Ok(())
        })
    }

    async fn delete_label(&self, area_id: &AreaId, label_id: &LabelId) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            area.labels.retain(|label| label.id != *label_id);
            Ok(())
        })
    }

    // ===== SHAPE OPERATIONS =====

    async fn create_shape(&self, area_id: &AreaId, shape_data: ShapeArgs) -> CloudResult<Shape> {
        self.mutate(*area_id, |area| {
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
    }

    async fn update_shape(
        &self,
        area_id: &AreaId,
        shape_id: &ShapeId,
        updates: ShapeUpdates,
    ) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            let shape = area
                .shapes
                .iter_mut()
                .find(|shape| shape.id == *shape_id)
                .ok_or(CloudError::ShapeNotFound(*shape_id))?;
            *shape = updates.apply(shape);
            Ok(())
        })
    }

    async fn delete_shape(&self, area_id: &AreaId, shape_id: &ShapeId) -> CloudResult<()> {
        self.mutate(*area_id, |area| {
            area.shapes.retain(|shape| shape.id != *shape_id);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExitDirection, RoomNumber};

    fn request(name: &str) -> CreateAreaRequest {
        CreateAreaRequest {
            name: name.to_string(),
            atlas_id: None,
            ephemeral: true,
        }
    }

    #[tokio::test]
    async fn create_room_exit_roundtrip_stays_in_memory() {
        let backend = EphemeralBackend::new();
        let area = backend.create_area(request("Session")).await.expect("create");
        assert!(area.effective_access().is_owner);

        let key = RoomKey::new(area.id, RoomNumber(1));
        backend
            .update_room(
                &key,
                RoomUpdates {
                    title: Some("Gate".to_string()),
                    ..RoomUpdates::default()
                },
            )
            .await
            .expect("upsert");
        backend
            .create_room_exit(
                &key,
                ExitArgs {
                    from_direction: ExitDirection::North,
                    ..ExitArgs::default()
                },
            )
            .await
            .expect("exit");

        let details = backend.get_area(&area.id).await.expect("get");
        assert!(details.area.rev > 1, "mutations bump rev");
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].title, "Gate");
        assert_eq!(details.rooms[0].exits.len(), 1);
    }

    #[tokio::test]
    async fn delete_room_nulls_inbound_exits() {
        let backend = EphemeralBackend::new();
        let area = backend.create_area(request("Session")).await.expect("create");
        let k1 = RoomKey::new(area.id, RoomNumber(1));
        backend.update_room(&k1, RoomUpdates::default()).await.expect("r1");
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
            .expect("delete");

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].exits[0].to_area_id, None, "inbound exit cleared");
    }

    #[tokio::test]
    async fn folders_are_rejected_and_deletion_is_final() {
        let backend = EphemeralBackend::new();
        let area = backend.create_area(request("Session")).await.expect("create");

        let filed = backend
            .update_area(
                &area.id,
                AreaUpdates {
                    name: None,
                    atlas_id: Some(Some(crate::AtlasId(uuid::Uuid::new_v4()))),
                },
            )
            .await;
        assert!(matches!(filed, Err(CloudError::InvalidInput(_))));

        backend.delete_area(&area.id).await.expect("delete");
        assert!(matches!(
            backend.get_area(&area.id).await,
            Err(CloudError::NotFoundOrNoAccess)
        ));
        assert!(backend.list_areas().await.expect("list").is_empty());
    }
}
