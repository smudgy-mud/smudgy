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

use super::{AreaMergeCommit, AreaMergePlan, MapperBackend, apply_area_merge, area_edits};
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, CloudError, CloudResult,
    CreateAreaRequest, MapStorage,
    mutation::{MutationEnvelope, MutationResult},
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
        let area = areas
            .get_mut(&area_id)
            .ok_or(CloudError::NotFoundOrNoAccess)?;
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
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: Vec::new(),
            rooms: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
        };
        self.areas.write().insert(area.id, details);
        Ok(area)
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<Area> {
        if storage != MapStorage::Session {
            return Err(CloudError::InvalidInput(format!(
                "the session backend cannot create a {storage} map"
            )));
        }
        self.create_area(request).await
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

    // ===== VERSIONED MUTATIONS =====

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        // The compare-and-set write path. The shared applier owns the
        // precondition check and the single revision bump; it runs against a
        // working clone that replaces the stored area only on full success,
        // so a failed envelope changes nothing (`mutate` edits in place and
        // bumps unconditionally, which would break both properties).
        let mut areas = self.areas.write();
        let stored = areas
            .get_mut(area_id)
            .ok_or(CloudError::AreaNotFound(*area_id))?;
        let mut working = stored.clone();
        let result = area_edits::apply_envelope(&mut working, *area_id, envelope)?;
        *stored = working;
        Ok(result)
    }

    // ===== MULTI-AREA TRANSACTIONS =====

    async fn merge_areas(&self, plan: &AreaMergePlan) -> CloudResult<AreaMergeCommit> {
        // One store lock across load, apply and swap. The applier runs over
        // working clones of every named document; they replace the stored
        // ones, and the whole sources leave the store, only once it has
        // succeeded in full, so a refused merge leaves every area exactly
        // as it was.
        let mut areas = self.areas.write();
        let stored = |id: AreaId| -> CloudResult<AreaWithDetails> {
            areas.get(&id).cloned().ok_or(CloudError::AreaNotFound(id))
        };
        let mut into = stored(plan.into)?;
        let mut sources = plan
            .sources
            .iter()
            .map(|source| stored(source.id))
            .collect::<CloudResult<Vec<_>>>()?;
        let mut inbound = plan
            .inbound
            .iter()
            .map(|id| stored(*id))
            .collect::<CloudResult<Vec<_>>>()?;

        let outcome = apply_area_merge(plan, &mut into, &mut sources, &mut inbound)?;

        // A partial source is a written document like a third party; a
        // whole source is removed below and never a post-image.
        let mut post_images: HashMap<AreaId, AreaWithDetails> =
            HashMap::with_capacity(1 + sources.len() + inbound.len());
        post_images.insert(into.area.id, into);
        for document in sources.into_iter().chain(inbound) {
            post_images.insert(document.area.id, document);
        }
        let documents: Vec<AreaWithDetails> = outcome
            .versions
            .iter()
            .filter(|version| !version.deleted)
            .filter_map(|version| post_images.remove(&AreaId(version.id)))
            .collect();
        for document in &documents {
            areas.insert(document.area.id, document.clone());
        }
        for id in plan.deleted_areas() {
            areas.remove(&id);
        }
        Ok(AreaMergeCommit { outcome, documents })
    }

    fn ephemeral_area_ids(&self) -> std::collections::HashSet<AreaId> {
        self.areas.read().keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConnectionArgs, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionKind,
        ConnectionRouting, CornerStyle, ExitArgs, ExitDirection, ExitId, LabelArgs, LabelId,
        MapPoint, PortMode, RoomNumber, RoomSide, RoomUpdates, SegmentShape, ShapeArgs, ShapeId,
        backends::{AreaMergeSource, RoomRemap, Translate},
        mapper::RoomKey,
        mutation::{AreaMutation, OpResult, Precondition, ResourceKind},
    };

    fn request(name: &str) -> CreateAreaRequest {
        CreateAreaRequest {
            name: name.to_string(),
            atlas_id: None,
            ephemeral: true,
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
    async fn explicit_creation_rejects_a_durable_tier() {
        let backend = EphemeralBackend::new();
        assert!(matches!(
            backend
                .create_area_at(request("Wrong tier"), MapStorage::Local)
                .await,
            Err(CloudError::InvalidInput(_))
        ));
        assert!(backend.list_areas().await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn create_room_exit_roundtrip_stays_in_memory() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Session"))
            .await
            .expect("create");
        assert!(area.effective_access().is_owner);

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
                                title: Some("Gate".to_string()),
                                ..RoomUpdates::default()
                            },
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(1),
                            body: ExitArgs {
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
        assert_eq!(details.rooms[0].title, "Gate");
        assert_eq!(details.rooms[0].exits.len(), 1);
    }

    #[tokio::test]
    async fn delete_room_nulls_inbound_exits() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Session"))
            .await
            .expect("create");
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
            .expect("delete");

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(
            details.rooms[0].exits[0].to_area_id, None,
            "inbound exit cleared"
        );
    }

    #[tokio::test]
    async fn execute_mutation_stale_revision_conflicts_and_stores_nothing() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Session"))
            .await
            .expect("create");

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
    }

    #[tokio::test]
    async fn execute_mutation_applies_all_ops_with_one_rev_bump() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Session"))
            .await
            .expect("create");

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
                                title: Some("Gate".to_string()),
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
        assert!(matches!(&result.data[0], OpResult::Room { room } if room.title == "Gate"));
        assert!(matches!(&result.data[1], OpResult::Exit { exit } if exit.id == exit_id));
        assert!(matches!(&result.data[2], OpResult::RoomTag { tag, .. } if tag == "INN"));

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.area.rev, 2);
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].title, "Gate");
        assert_eq!(details.rooms[0].exits.len(), 1);
        assert!(details.rooms[0].tags.contains("INN"));
    }

    #[tokio::test]
    async fn execute_mutation_failed_op_leaves_the_area_byte_identical() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Session"))
            .await
            .expect("create");
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
    }

    #[tokio::test]
    async fn execute_mutation_create_ops_honor_client_ids() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Session"))
            .await
            .expect("create");

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
    }

    fn endpoint(room_number: i32, side: RoomSide) -> ConnectionEndpoint {
        ConnectionEndpoint {
            room_number: RoomNumber(room_number),
            side,
            port_offset: 0.5,
            port_mode: PortMode::AutoPinned,
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn connection_lifecycle_create_unlink_pair_delete_is_atomic() {
        let backend = EphemeralBackend::new();
        let area = backend.create_area(request("Links")).await.expect("create");
        let connection_id = ConnectionId::new();
        let split_id = ConnectionId::new();
        let east = ExitId(Uuid::new_v4());
        let west = ExitId(Uuid::new_v4());

        let created = backend
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
                            body: RoomUpdates {
                                x: Some(2.0),
                                ..RoomUpdates::default()
                            },
                        },
                        AreaMutation::CreateConnection {
                            body: ConnectionArgs {
                                id: connection_id,
                                endpoint_a: endpoint(1, RoomSide::East),
                                endpoint_b: Some(endpoint(2, RoomSide::West)),
                                routing: ConnectionRouting::Simple,
                                segment_shape: SegmentShape::Direct,
                                corner: CornerStyle::Sharp,
                                route_points: vec![],
                                dash: ConnectionDash::Solid,
                                color: "#a4a4a4".to_string(),
                                thickness: 1.0,
                            },
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(1),
                            body: ExitArgs {
                                id: Some(east),
                                connection_id: Some(connection_id),
                                from_direction: ExitDirection::East,
                                to_area_id: Some(area.id),
                                to_room_number: Some(RoomNumber(2)),
                                to_direction: Some(ExitDirection::West),
                                weight: 1.0,
                                ..ExitArgs::default()
                            },
                        },
                        AreaMutation::CreateExit {
                            room_number: RoomNumber(2),
                            body: ExitArgs {
                                id: Some(west),
                                connection_id: Some(connection_id),
                                from_direction: ExitDirection::West,
                                to_area_id: Some(area.id),
                                to_room_number: Some(RoomNumber(1)),
                                to_direction: Some(ExitDirection::East),
                                weight: 1.0,
                                ..ExitArgs::default()
                            },
                        },
                    ],
                ),
            )
            .await
            .expect("atomic link create");
        assert_eq!(created.versions[0].rev, 2, "five rows are one revision");
        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.connections.len(), 1);
        assert_eq!(
            details
                .rooms
                .iter()
                .map(|room| room.exits.len())
                .sum::<usize>(),
            2
        );
        assert_eq!(details.connections[0].color, "#A4A4A4");

        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    2,
                    vec![AreaMutation::Unlink {
                        exit_id: east,
                        new_connection_id: split_id,
                    }],
                ),
            )
            .await
            .expect("unlink");
        let split = backend.get_area(&area.id).await.expect("get");
        assert_eq!(split.connections.len(), 2);
        assert!(split.connections.iter().all(|connection| {
            split
                .rooms
                .iter()
                .flat_map(|room| &room.exits)
                .filter(|exit| exit.connection_id == connection.id)
                .count()
                == 1
        }));

        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    3,
                    vec![AreaMutation::Pair {
                        keep_connection_id: connection_id,
                        merge_connection_id: split_id,
                    }],
                ),
            )
            .await
            .expect("pair");
        let paired = backend.get_area(&area.id).await.expect("get");
        assert_eq!(paired.connections.len(), 1);
        assert!(
            paired
                .rooms
                .iter()
                .flat_map(|room| &room.exits)
                .all(|exit| { exit.connection_id == connection_id })
        );

        backend
            .execute_mutation(
                &area.id,
                &envelope(area.id, 4, vec![AreaMutation::DeleteLink { connection_id }]),
            )
            .await
            .expect("delete link");
        let deleted = backend.get_area(&area.id).await.expect("get");
        assert!(deleted.connections.is_empty());
        assert!(deleted.rooms.iter().all(|room| room.exits.is_empty()));
    }

    #[tokio::test]
    async fn reciprocal_cross_level_exits_share_one_connection() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Cross-level link"))
            .await
            .expect("create");
        let connection_id = ConnectionId::new();
        let mut operations = routed_link_ops(area.id, connection_id);
        if let AreaMutation::UpsertRoom { body, .. } = &mut operations[1] {
            body.level = Some(1);
        }
        if let AreaMutation::CreateConnection { body } = &mut operations[2] {
            body.routing = ConnectionRouting::Simple;
            body.segment_shape = SegmentShape::Direct;
            body.route_points.clear();
        }

        backend
            .execute_mutation(&area.id, &envelope(area.id, 1, operations))
            .await
            .expect("cross-level pair");
        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(details.connections.len(), 1);
        assert_eq!(details.connections[0].kind, ConnectionKind::CrossLevel);
        assert_eq!(
            details
                .rooms
                .iter()
                .flat_map(|room| &room.exits)
                .filter(|exit| exit.connection_id == connection_id)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn malformed_one_member_connections_are_rejected_atomically() {
        let malformed_cases = [
            (
                "missing traversal",
                Some(endpoint(2, RoomSide::West)),
                None,
                None,
            ),
            (
                "same-area destination without endpoint B",
                None,
                Some(RoomNumber(2)),
                Some(ExitDirection::West),
            ),
        ];

        for (name, endpoint_b, to_room_number, to_direction) in malformed_cases {
            let backend = EphemeralBackend::new();
            let area = backend.create_area(request(name)).await.expect("create");
            let connection_id = ConnectionId::new();
            let result = backend
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
                            AreaMutation::CreateConnection {
                                body: ConnectionArgs {
                                    id: connection_id,
                                    endpoint_a: endpoint(1, RoomSide::East),
                                    endpoint_b,
                                    routing: ConnectionRouting::Simple,
                                    segment_shape: SegmentShape::Direct,
                                    corner: CornerStyle::Sharp,
                                    route_points: vec![],
                                    dash: ConnectionDash::Solid,
                                    color: "#A4A4A4".to_string(),
                                    thickness: 1.0,
                                },
                            },
                            AreaMutation::CreateExit {
                                room_number: RoomNumber(1),
                                body: ExitArgs {
                                    id: Some(ExitId::new()),
                                    connection_id: Some(connection_id),
                                    from_direction: ExitDirection::East,
                                    to_area_id: to_room_number.map(|_| area.id),
                                    to_room_number,
                                    to_direction,
                                    weight: 1.0,
                                    ..ExitArgs::default()
                                },
                            },
                        ],
                    ),
                )
                .await;

            assert!(
                matches!(result, Err(CloudError::InvalidConnection(reason)) if reason == "invalid_endpoint"),
                "{name} should fail topology validation"
            );
            let stored = backend
                .get_area(&area.id)
                .await
                .expect("get after rejection");
            assert_eq!(stored.area.rev, 1, "{name} must not bump the revision");
            assert!(stored.rooms.is_empty(), "{name} must roll back every room");
            assert!(
                stored.connections.is_empty(),
                "{name} must roll back the connection"
            );
        }
    }

    fn routed_link_ops(area_id: AreaId, connection_id: ConnectionId) -> Vec<AreaMutation> {
        let east = ExitId::new();
        let west = ExitId::new();
        vec![
            AreaMutation::UpsertRoom {
                room_number: RoomNumber(1),
                body: RoomUpdates::default(),
            },
            AreaMutation::UpsertRoom {
                room_number: RoomNumber(2),
                body: RoomUpdates {
                    x: Some(2.0),
                    ..RoomUpdates::default()
                },
            },
            AreaMutation::CreateConnection {
                body: ConnectionArgs {
                    id: connection_id,
                    endpoint_a: endpoint(1, RoomSide::East),
                    endpoint_b: Some(endpoint(2, RoomSide::West)),
                    routing: ConnectionRouting::Manual,
                    segment_shape: SegmentShape::Orthogonal,
                    corner: CornerStyle::Sharp,
                    route_points: vec![MapPoint::new(0.4, 1.0), MapPoint::new(1.6, 1.0)],
                    dash: ConnectionDash::Solid,
                    color: "#A4A4A4".to_string(),
                    thickness: 1.0,
                },
            },
            AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                body: ExitArgs {
                    id: Some(east),
                    connection_id: Some(connection_id),
                    from_direction: ExitDirection::East,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(ExitDirection::West),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            },
            AreaMutation::CreateExit {
                room_number: RoomNumber(2),
                body: ExitArgs {
                    id: Some(west),
                    connection_id: Some(connection_id),
                    from_direction: ExitDirection::West,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(1)),
                    to_direction: Some(ExitDirection::East),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            },
        ]
    }

    #[tokio::test]
    async fn moving_both_endpoints_translates_manual_route_points() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Route move"))
            .await
            .expect("create");
        let connection_id = ConnectionId::new();
        backend
            .execute_mutation(
                &area.id,
                &envelope(area.id, 1, routed_link_ops(area.id, connection_id)),
            )
            .await
            .expect("seed routed link");

        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    2,
                    vec![
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(1),
                            body: RoomUpdates {
                                x: Some(3.0),
                                y: Some(2.0),
                                ..RoomUpdates::default()
                            },
                        },
                        AreaMutation::UpsertRoom {
                            room_number: RoomNumber(2),
                            body: RoomUpdates {
                                x: Some(5.0),
                                y: Some(2.0),
                                ..RoomUpdates::default()
                            },
                        },
                    ],
                ),
            )
            .await
            .expect("move both rooms");

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(
            details.connections[0].route_points,
            vec![MapPoint::new(3.4, 3.0), MapPoint::new(4.6, 3.0)]
        );
    }

    #[tokio::test]
    async fn moving_one_endpoint_repairs_the_adjacent_orthogonal_leg() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Route repair"))
            .await
            .expect("create");
        let connection_id = ConnectionId::new();
        backend
            .execute_mutation(
                &area.id,
                &envelope(area.id, 1, routed_link_ops(area.id, connection_id)),
            )
            .await
            .expect("seed routed link");

        backend
            .execute_mutation(
                &area.id,
                &envelope(
                    area.id,
                    2,
                    vec![AreaMutation::UpsertRoom {
                        room_number: RoomNumber(1),
                        body: RoomUpdates {
                            x: Some(1.0),
                            ..RoomUpdates::default()
                        },
                    }],
                ),
            )
            .await
            .expect("move one room");

        let details = backend.get_area(&area.id).await.expect("get");
        assert_eq!(
            details.connections[0].route_points[0],
            MapPoint::new(1.4, 1.0)
        );
    }

    #[tokio::test]
    async fn folders_are_rejected_and_deletion_is_final() {
        let backend = EphemeralBackend::new();
        let area = backend
            .create_area(request("Session"))
            .await
            .expect("create");

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

    // ===== area merges =====

    /// Seeds room 1 in `area_id`, with an exit north out of it to `exit_to`
    /// when given; the area ends at rev 2.
    async fn seed_room(
        backend: &EphemeralBackend,
        area_id: AreaId,
        exit_to: Option<(AreaId, i32)>,
    ) {
        let mut payload = vec![AreaMutation::UpsertRoom {
            room_number: RoomNumber(1),
            body: RoomUpdates::default(),
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

    /// A destination with room 1, a source with room 1, and a third party
    /// whose room 1 exits into the source's room; all at rev 2.
    async fn merge_areas_fixture(backend: &EphemeralBackend) -> (AreaId, AreaId, AreaId) {
        let mut ids = Vec::new();
        for name in ["Into", "Source", "Third"] {
            ids.push(backend.create_area(request(name)).await.expect("create").id);
        }
        let (into, source, third) = (ids[0], ids[1], ids[2]);
        seed_room(backend, into, None).await;
        seed_room(backend, source, None).await;
        seed_room(backend, third, Some((source, 1))).await;
        (into, source, third)
    }

    fn merge_plan(into: AreaId, source: AreaId, third: AreaId) -> AreaMergePlan {
        AreaMergePlan {
            into,
            sources: vec![AreaMergeSource {
                id: source,
                translate: Translate::default(),
                rooms: None,
            }],
            inbound: vec![third],
            expected: vec![(into, 2), (source, 2), (third, 2)],
            number_floor: RoomNumber(2),
        }
    }

    /// Every stored area, serialized, in id order.
    fn store_snapshot(backend: &EphemeralBackend) -> Vec<(AreaId, String)> {
        let mut snapshot: Vec<(AreaId, String)> = backend
            .areas
            .read()
            .values()
            .map(|details| {
                (
                    details.area.id,
                    serde_json::to_string(details).expect("serialize"),
                )
            })
            .collect();
        snapshot.sort_by_key(|(id, _)| id.0);
        snapshot
    }

    #[tokio::test]
    async fn merge_areas_moves_the_room_and_removes_the_source() {
        let backend = EphemeralBackend::new();
        let (into, source, third) = merge_areas_fixture(&backend).await;

        let commit = backend
            .merge_areas(&merge_plan(into, source, third))
            .await
            .expect("merge");

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
        assert_eq!(written, vec![into, third]);

        let destination = backend.get_area(&into).await.expect("destination");
        assert_eq!(destination.area.rev, 3);
        assert_eq!(destination.rooms.len(), 2);
        assert_eq!(
            serde_json::to_string(&destination).expect("serialize"),
            serde_json::to_string(&commit.documents[0]).expect("serialize"),
            "the store holds the returned post-image"
        );
        let third_party = backend.get_area(&third).await.expect("third party");
        assert_eq!(third_party.area.rev, 3);
        assert_eq!(third_party.rooms[0].exits[0].to_area_id, Some(into));
        assert_eq!(
            third_party.rooms[0].exits[0].to_room_number,
            Some(RoomNumber(2))
        );
        assert!(matches!(
            backend.get_area(&source).await,
            Err(CloudError::NotFoundOrNoAccess)
        ));
        assert!(!backend.ephemeral_area_ids().contains(&source));
    }

    #[tokio::test]
    async fn merge_areas_refused_leaves_every_area_unchanged() {
        let backend = EphemeralBackend::new();
        let (into, source, third) = merge_areas_fixture(&backend).await;
        let before = store_snapshot(&backend);

        let mut drifted = merge_plan(into, source, third);
        drifted.expected[1].1 = 3;
        let result = backend.merge_areas(&drifted).await;
        assert!(
            matches!(
                &result,
                Err(CloudError::RevisionConflict {
                    id,
                    expected_rev: 3,
                    current_rev: 2,
                }) if *id == source.0
            ),
            "a moved revision conflicts unchanged, got {result:?}"
        );
        assert_eq!(store_snapshot(&backend), before, "nothing was swapped in");

        let ghost = AreaId(Uuid::new_v4());
        let mut missing = merge_plan(into, source, third);
        missing.inbound[0] = ghost;
        missing.expected[2].0 = ghost;
        let result = backend.merge_areas(&missing).await;
        assert!(
            matches!(&result, Err(CloudError::AreaNotFound(id)) if *id == ghost),
            "a named area with no document is AreaNotFound, got {result:?}"
        );
        assert_eq!(store_snapshot(&backend), before);
    }
}
