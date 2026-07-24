use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use log::warn;

use crate::{
    AREA_FORMAT_VERSION, Area, AreaAccess, AreaId, AreaWithDetails, AtlasId, CloudError,
    CloudResult, Connection, ConnectionKind, ExitDirection, ExitId, Label, LabelId, MapPoint,
    RoomNumber, RoomUpdates, Shape, ShapeId, connection_geometry,
    connection_lifecycle::{self, ExitTopology, RoomSite},
    mapper::{
        RoomKey,
        exit_cache::ExitCache,
        room_cache::{PropertyEntry, RoomCache},
        room_connection::{RoomConnection, RoomConnectionEnd},
    },
    parse_css_color,
};
use rstar::{AABB, RTree, RTreeObject};

/// The rendered fallback when a Connection's stored color fails to parse —
/// the same gray as [`crate::DEFAULT_CONNECTION_COLOR`].
const DEFAULT_CONNECTION_ICED_COLOR: iced::Color = iced::Color::from_rgb8(164, 164, 164);

/// Cloud metadata for an area beyond its geometry: the viewer's access block,
/// owner attribution, atlas membership, and clone provenance.
#[derive(Debug, Clone, Default)]
pub struct AreaMeta {
    pub access: Option<AreaAccess>,
    /// The area owner's user id (`Area.user_id`). Used to group shared rows
    /// by sharer/owner identity rather than by the display handle, which may
    /// be absent.
    pub owner_id: Option<uuid::Uuid>,
    pub owner_nickname: Option<String>,
    pub atlas_id: Option<AtlasId>,
    /// The atlas's display name, denormalized onto the area (§4.1 un-redaction
    /// delivers it to every viewer who can see the area). `None` when the source
    /// row carried no name — a caller that needs a label falls back to the area
    /// name or a generic phrase. Purely descriptive; confers no capability.
    pub atlas_name: Option<String>,
    pub copied_from_area_id: Option<AreaId>,
    pub copied_from_rev: Option<i64>,
    pub copied_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AreaCache {
    id: AreaId,
    name: String,
    rev: i64,
    meta: AreaMeta,
    rooms_by_number: HashMap<RoomNumber, Arc<RoomCache>>,
    rooms: Vec<Arc<RoomCache>>,
    /// The stored Connection rows, as projected (or locally maintained by
    /// the optimistic edit paths); [`Self::room_connections`] is resolved
    /// from these.
    connections: Vec<Connection>,
    room_connections: Vec<RoomConnection>,
    properties: HashMap<String, PropertyEntry>,
    labels: Vec<Label>,
    shapes: Vec<Shape>,
    max_room_number: RoomNumber,
    rooms_index: RTree<RoomSpatialEntry>,
    room_connections_index: RTree<ConnectionSpatialEntry>,
    /// Whether any entity in the area is secret-marked; computed at
    /// construction (and after [`Self::apply_secret_marks`]) so list UIs can
    /// show a cheap indicator without scanning rooms per redraw.
    has_secrets: bool,
}

#[derive(Debug, Clone)]
struct RoomSpatialEntry {
    bounds: AABB<[f32; 2]>,
    room: Arc<RoomCache>,
}

impl RoomSpatialEntry {
    fn new(room: Arc<RoomCache>) -> Self {
        let bounds = AABB::from_corners([room.get_x(), room.get_y()], [room.get_x(), room.get_y()]);
        Self { bounds, room }
    }
}

impl RTreeObject for RoomSpatialEntry {
    type Envelope = AABB<[f32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.bounds
    }
}

#[derive(Debug, Clone)]
struct ConnectionSpatialEntry {
    bounds: AABB<[f32; 2]>,
    index: usize,
}

impl ConnectionSpatialEntry {
    fn new(index: usize, connection: &RoomConnection) -> Self {
        let bounds = &connection.geometry.bounds;
        Self {
            bounds: AABB::from_corners([bounds.min_x, bounds.min_y], [bounds.max_x, bounds.max_y]),
            index,
        }
    }
}

impl RTreeObject for ConnectionSpatialEntry {
    type Envelope = AABB<[f32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.bounds
    }
}

impl AreaCache {
    fn build_rooms_index(rooms: &[Arc<RoomCache>]) -> RTree<RoomSpatialEntry> {
        let entries: Vec<_> = rooms.iter().cloned().map(RoomSpatialEntry::new).collect();
        RTree::bulk_load(entries)
    }

    fn build_room_connections_index(
        room_connections: &[RoomConnection],
    ) -> RTree<ConnectionSpatialEntry> {
        let entries: Vec<_> = room_connections
            .iter()
            .enumerate()
            // An empty bounds is the inverted-infinity sentinel; it must
            // never reach the spatial index.
            .filter(|(_, connection)| !connection.geometry.bounds.is_empty())
            .map(|(index, connection)| ConnectionSpatialEntry::new(index, connection))
            .collect();
        RTree::bulk_load(entries)
    }

    fn rebuild_room_state(
        &self,
        rooms_by_number: HashMap<RoomNumber, Arc<RoomCache>>,
        rooms: Vec<Arc<RoomCache>>,
        max_room_number: RoomNumber,
        connections: Vec<Connection>,
    ) -> Self {
        let room_connections =
            Self::build_room_connections(&self.id, &connections, &rooms_by_number);
        let rooms_index = Self::build_rooms_index(&rooms);
        let room_connections_index = Self::build_room_connections_index(&room_connections);

        Self {
            rev: self.rev + 1,
            rooms_by_number,
            rooms,
            max_room_number,
            connections,
            room_connections,
            rooms_index,
            room_connections_index,
            ..self.clone()
        }
    }

    pub(super) fn new_with_area(area: AreaWithDetails) -> Self {
        let max_room_number = area
            .rooms
            .iter()
            .map(|r| r.room_number)
            .max()
            .unwrap_or(RoomNumber(0));

        let rooms: Vec<Arc<RoomCache>> = area
            .rooms
            .iter()
            .map(|r| Arc::new(r.clone().into()))
            .collect();
        let rooms_by_number = rooms
            .iter()
            .map(|r| (r.get_room_number(), r.clone()))
            .collect();
        let properties = area
            .properties
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    PropertyEntry {
                        value: p.value.clone(),
                        is_secret: p.is_secret,
                    },
                )
            })
            .collect();

        let room_connections =
            Self::build_room_connections(&area.area.id, &area.connections, &rooms_by_number);
        let rooms_index = Self::build_rooms_index(&rooms);
        let room_connections_index = Self::build_room_connections_index(&room_connections);

        let mut cache = Self {
            id: area.area.id,
            name: area.area.name,
            rev: area.area.rev,
            meta: AreaMeta {
                access: area.area.access,
                owner_id: area.area.user_id,
                owner_nickname: area.area.owner_nickname,
                atlas_id: area.area.atlas_id,
                atlas_name: area.area.atlas_name,
                copied_from_area_id: area.area.copied_from_area_id,
                copied_from_rev: area.area.copied_from_rev,
                copied_at: area.area.copied_at,
                content_hash: area.content_hash,
            },
            rooms,
            rooms_by_number,
            max_room_number,
            properties,
            labels: area.labels,
            shapes: area.shapes,
            connections: area.connections,
            room_connections,
            rooms_index,
            room_connections_index,
            has_secrets: false,
        };
        cache.has_secrets = cache.compute_has_secrets();
        cache
    }

    /// Scans every entity for a secret mark; run once at construction and
    /// after secret-mark application, never per redraw.
    fn compute_has_secrets(&self) -> bool {
        self.rooms.iter().any(|room| {
            room.is_secret()
                || room.get_exits().iter().any(|exit| exit.is_secret)
                || room
                    .properties_with_secrecy()
                    .any(|(_, entry)| entry.is_secret)
        }) || self.labels.iter().any(|label| label.is_secret)
            || self.shapes.iter().any(|shape| shape.is_secret)
            || self.properties.values().any(|entry| entry.is_secret)
    }

    /// Whether any entity in this area is secret-marked (cached).
    #[must_use]
    pub fn has_secrets(&self) -> bool {
        self.has_secrets
    }

    /// This area's text labels.
    #[must_use]
    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    /// This area's graphical shapes.
    #[must_use]
    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// Resolves the stored `connections` array into render views: for each
    /// Connection, look up its endpoint rooms and member exits, resolve the
    /// geometry exactly once, and derive the special-kind end facts from
    /// visible topology. A Connection whose projection is corrupt (missing
    /// endpoint room, no member exit) is skipped with a warning — never a
    /// panic. Cross-level Connections emit two halves, one per endpoint
    /// level, sharing one geometry [`Arc`].
    #[allow(clippy::too_many_lines)]
    fn build_room_connections(
        area_id: &AreaId,
        connections: &[Connection],
        rooms_by_number: &HashMap<RoomNumber, Arc<RoomCache>>,
    ) -> Vec<RoomConnection> {
        // One walk of the rooms builds the connection_id → members map.
        let mut members_by_connection: HashMap<
            crate::ConnectionId,
            Vec<(&Arc<RoomCache>, &ExitCache)>,
        > = HashMap::new();
        for room in rooms_by_number.values() {
            for exit in room.get_exits() {
                members_by_connection
                    .entry(exit.connection_id)
                    .or_default()
                    .push((room, exit));
            }
        }

        let mut room_connections = Vec::with_capacity(connections.len());
        for connection in connections {
            let Some(room_a) = rooms_by_number.get(&connection.endpoint_a.room_number) else {
                warn!(
                    "area {area_id}: connection {} endpoint room {} missing from the projection; skipping",
                    connection.id, connection.endpoint_a.room_number.0
                );
                continue;
            };
            let room_b = if let Some(endpoint) = &connection.endpoint_b {
                let Some(room) = rooms_by_number.get(&endpoint.room_number) else {
                    warn!(
                        "area {area_id}: connection {} endpoint room {} missing from the projection; skipping",
                        connection.id, endpoint.room_number.0
                    );
                    continue;
                };
                Some(room)
            } else {
                None
            };
            let members = members_by_connection
                .get(&connection.id)
                .map_or(&[][..], Vec::as_slice);
            let Some(&(member_room, member_exit)) = members.first() else {
                warn!(
                    "area {area_id}: connection {} has no member exit; skipping",
                    connection.id
                );
                continue;
            };

            let geometry = Arc::new(connection_geometry::resolve(
                &connection_geometry::GeometryInput {
                    kind: connection.kind,
                    routing: connection.routing,
                    corner: connection.corner,
                    endpoint_a: connection_geometry::EndpointGeometry {
                        room_center: MapPoint::new(room_a.get_x(), room_a.get_y()),
                        side: connection.endpoint_a.side,
                        port_offset: connection.endpoint_a.port_offset,
                    },
                    endpoint_b: connection.endpoint_b.as_ref().zip(room_b).map(
                        |(endpoint, room)| connection_geometry::EndpointGeometry {
                            room_center: MapPoint::new(room.get_x(), room.get_y()),
                            side: endpoint.side,
                            port_offset: endpoint.port_offset,
                        },
                    ),
                    route_points: &connection.route_points,
                    thickness: connection.thickness,
                },
            ));

            let is_bidirectional = members.len() == 2;
            let arrow_toward_b = if is_bidirectional || connection.kind == ConnectionKind::SelfLoop
            {
                None
            } else {
                Some(member_room.get_room_number() == connection.endpoint_a.room_number)
            };
            let is_secret = members.iter().any(|(_, exit)| exit.is_secret)
                || room_a.is_secret()
                || room_b.is_some_and(|room| room.is_secret());
            let base = RoomConnection {
                connection_id: connection.id,
                from_level: room_a.get_level(),
                geometry: geometry.clone(),
                kind: connection.kind,
                routing: connection.routing,
                dash: connection.dash,
                corner: connection.corner,
                thickness: connection.thickness,
                color: parse_css_color(&connection.color).unwrap_or(DEFAULT_CONNECTION_ICED_COLOR),
                is_bidirectional,
                arrow_toward_b,
                is_secret,
                to: RoomConnectionEnd::None,
                room: (*room_a).clone(),
            };

            match connection.kind {
                ConnectionKind::SelfLoop => {
                    room_connections.push(RoomConnection {
                        to: RoomConnectionEnd::SelfLoop,
                        ..base
                    });
                }
                ConnectionKind::Internal => {
                    let (Some(room_b), Some(endpoint_b)) = (room_b, connection.endpoint_b) else {
                        warn!(
                            "area {area_id}: internal connection {} without endpoint B; skipping",
                            connection.id
                        );
                        continue;
                    };
                    room_connections.push(RoomConnection {
                        to: RoomConnectionEnd::Normal {
                            direction: Self::direction_at(
                                members,
                                endpoint_b.room_number,
                                Some(endpoint_b.side),
                            ),
                            x: room_b.get_x(),
                            y: room_b.get_y(),
                            room: (*room_b).clone(),
                        },
                        ..base
                    });
                }
                ConnectionKind::CrossLevel => {
                    let (Some(room_b), Some(endpoint_b)) = (room_b, connection.endpoint_b) else {
                        warn!(
                            "area {area_id}: cross-level connection {} without endpoint B; skipping",
                            connection.id
                        );
                        continue;
                    };
                    // Two halves — one per endpoint room's level — sharing
                    // the one resolved geometry.
                    room_connections.push(RoomConnection {
                        to: RoomConnectionEnd::ToLevel {
                            level: room_b.get_level(),
                            direction: Self::direction_at(
                                members,
                                connection.endpoint_a.room_number,
                                Some(connection.endpoint_a.side),
                            ),
                            x: room_b.get_x(),
                            y: room_b.get_y(),
                            room: (*room_b).clone(),
                        },
                        ..base.clone()
                    });
                    room_connections.push(RoomConnection {
                        from_level: room_b.get_level(),
                        room: (*room_b).clone(),
                        to: RoomConnectionEnd::ToLevel {
                            level: room_a.get_level(),
                            direction: Self::direction_at(
                                members,
                                endpoint_b.room_number,
                                Some(endpoint_b.side),
                            ),
                            x: room_a.get_x(),
                            y: room_a.get_y(),
                            room: (*room_a).clone(),
                        },
                        ..base
                    });
                }
                ConnectionKind::Dangling => {
                    let to = if member_exit.to_unknown {
                        // Redacted destination: must render as "Unknown map",
                        // never as a plain dangling exit.
                        RoomConnectionEnd::Unknown {
                            token: member_exit.to_area_token.clone().unwrap_or_default(),
                        }
                    } else {
                        RoomConnectionEnd::None
                    };
                    room_connections.push(RoomConnection { to, ..base });
                }
                ConnectionKind::External => {
                    let to = if member_exit.to_unknown {
                        RoomConnectionEnd::Unknown {
                            token: member_exit.to_area_token.clone().unwrap_or_default(),
                        }
                    } else if let Some(to_area_id) = member_exit.to_area_id {
                        RoomConnectionEnd::External {
                            area_id: to_area_id,
                        }
                    } else {
                        // The member lost its destination without the kind
                        // catching up (mid-edit projection); degrade to a
                        // bare stub rather than invent a destination.
                        RoomConnectionEnd::None
                    };
                    room_connections.push(RoomConnection { to, ..base });
                }
            }
        }

        room_connections
    }

    /// The compass direction a Connection anchors on at `room`: the member
    /// exit originating there knows it directly (`from_direction`); a member
    /// arriving there knows it as its `to_direction` (or the opposite of its
    /// origin direction); with neither, the endpoint's wall side stands in.
    fn direction_at(
        members: &[(&Arc<RoomCache>, &ExitCache)],
        room: RoomNumber,
        side: Option<crate::RoomSide>,
    ) -> ExitDirection {
        for (member_room, exit) in members {
            if member_room.get_room_number() == room {
                return exit.from_direction;
            }
        }
        for (_, exit) in members {
            if exit.to_room_number == Some(room) {
                return exit
                    .to_direction
                    .unwrap_or_else(|| exit.from_direction.opposite());
            }
        }
        match side {
            Some(crate::RoomSide::North) => ExitDirection::North,
            Some(crate::RoomSide::East) => ExitDirection::East,
            Some(crate::RoomSide::South) => ExitDirection::South,
            Some(crate::RoomSide::West) | None => ExitDirection::West,
        }
    }

    #[must_use]
    pub fn get_id(&self) -> &AreaId {
        &self.id
    }

    #[must_use]
    pub fn get_room(&self, room_number: &RoomNumber) -> Option<&Arc<RoomCache>> {
        self.rooms_by_number.get(room_number)
    }

    #[must_use]
    pub fn get_rooms(&self) -> &[Arc<RoomCache>] {
        &self.rooms
    }

    #[must_use]
    pub fn get_name(&self) -> &str {
        self.name.as_str()
    }

    #[must_use]
    pub(super) fn rename(&self, name: &str) -> Self {
        Self {
            name: name.to_string(),
            rev: self.rev + 1,
            ..self.clone()
        }
    }

    /// Returns a copy filed into `atlas_id` (`Some`) or pulled loose
    /// (`None`). Bumps `rev` like other local edits so an open editor on the
    /// area notices; the folder regrouping itself is read fresh from
    /// `meta().atlas_id` by the area list.
    #[must_use]
    pub(super) fn with_atlas(&self, atlas_id: Option<AtlasId>) -> Self {
        let mut meta = self.meta.clone();
        meta.atlas_id = atlas_id;
        Self {
            meta,
            rev: self.rev + 1,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn get_property(&self, name: &str) -> Option<&str> {
        self.properties.get(name).map(|p| p.value.as_str())
    }

    /// Iterates all properties in unspecified order; sort in the caller when
    /// stable ordering matters.
    pub fn properties(&self) -> impl Iterator<Item = (&str, &str)> {
        self.properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.value.as_str()))
    }

    /// Like [`Self::properties`] but including each property's secrecy flag.
    pub fn properties_with_secrecy(&self) -> impl Iterator<Item = (&str, &PropertyEntry)> {
        self.properties.iter().map(|(k, v)| (k.as_str(), v))
    }

    #[must_use]
    pub fn is_property_secret(&self, name: &str) -> bool {
        self.properties.get(name).is_some_and(|p| p.is_secret)
    }

    #[must_use]
    pub fn get_rev(&self) -> i64 {
        self.rev
    }

    /// Cloud metadata: access block, owner handle, provenance, atlas id.
    #[must_use]
    pub fn meta(&self) -> &AreaMeta {
        &self.meta
    }

    /// The viewer's capabilities on this area (missing access block => owned).
    #[must_use]
    pub fn effective_access(&self) -> AreaAccess {
        self.meta.access.unwrap_or(AreaAccess::OWNER)
    }

    /// Whether the viewer owns this area (shared areas return false).
    #[must_use]
    pub fn is_owned(&self) -> bool {
        self.effective_access().is_owner
    }

    #[must_use]
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// The next unused room number in this area.
    #[must_use]
    pub fn next_room_number(&self) -> RoomNumber {
        RoomNumber(self.max_room_number.0 + 1)
    }

    #[must_use]
    pub fn get_labels(&self) -> &[Label] {
        &self.labels
    }

    #[must_use]
    pub fn get_shapes(&self) -> &[Shape] {
        &self.shapes
    }

    #[must_use]
    pub fn get_label(&self, label_id: &LabelId) -> Option<&Label> {
        self.labels.iter().find(|l| &l.id == label_id)
    }

    #[must_use]
    pub fn get_shape(&self, shape_id: &ShapeId) -> Option<&Shape> {
        self.shapes.iter().find(|s| &s.id == shape_id)
    }

    pub(super) fn set_property(&self, name: String, value: String) -> Self {
        let mut new_properties = self.properties.clone();
        // Preserve secrecy on overwrite; new properties default to public.
        let is_secret = new_properties.get(&name).is_some_and(|p| p.is_secret);
        new_properties.insert(name, PropertyEntry { value, is_secret });

        Self {
            properties: new_properties,
            rev: self.rev + 1,
            ..self.clone()
        }
    }

    pub(super) fn delete_property(&self, name: &str) -> Self {
        let mut new_properties = self.properties.clone();
        new_properties.remove(name);

        Self {
            properties: new_properties,
            rev: self.rev + 1,
            ..self.clone()
        }
    }

    pub(super) fn upsert_room(&self, room_number: RoomNumber, updates: RoomUpdates) -> Self {
        let room = if let Some(room) = self.rooms_by_number.get(&room_number) {
            Arc::new(room.apply_updates(updates))
        } else {
            Arc::new(RoomCache::new(room_number).apply_updates(updates))
        };

        self.upsert_room_cache(room_number, room)
    }

    /// Applies a batch of room updates with a single index/connection rebuild,
    /// unlike repeated [`Self::upsert_room`] calls which rebuild per room.
    pub(super) fn upsert_rooms(&self, updates: &[(RoomNumber, RoomUpdates)]) -> Self {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        let mut max_room_number = self.max_room_number;
        let mut updated_numbers = HashSet::with_capacity(updates.len());

        for (room_number, room_updates) in updates {
            let room = if let Some(room) = new_rooms_by_number.get(room_number) {
                Arc::new(room.apply_updates(room_updates.clone()))
            } else {
                Arc::new(RoomCache::new(*room_number).apply_updates(room_updates.clone()))
            };
            new_rooms_by_number.insert(*room_number, room);
            updated_numbers.insert(*room_number);
            max_room_number = RoomNumber(room_number.0.max(max_room_number.0));
        }

        let mut new_rooms = self.rooms.clone();
        new_rooms.retain(|r| !updated_numbers.contains(&r.get_room_number()));
        new_rooms.extend(
            updated_numbers
                .iter()
                .filter_map(|n| new_rooms_by_number.get(n).cloned()),
        );

        self.rebuild_room_state(
            new_rooms_by_number,
            new_rooms,
            max_room_number,
            self.connections.clone(),
        )
    }

    fn upsert_room_cache(&self, room_number: RoomNumber, room: Arc<RoomCache>) -> Self {
        self.upsert_room_cache_with_connections(room_number, room, self.connections.clone())
    }

    /// [`Self::upsert_room_cache`] for the edit paths that also changed the
    /// stored Connection rows (exit lifecycle).
    fn upsert_room_cache_with_connections(
        &self,
        room_number: RoomNumber,
        room: Arc<RoomCache>,
        connections: Vec<Connection>,
    ) -> Self {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        let mut new_rooms = self.rooms.clone();

        new_rooms_by_number.insert(room_number, room.clone());
        new_rooms.retain(|r| r.get_room_number() != room_number);
        new_rooms.push(room);
        let max_room_number = RoomNumber(room_number.0.max(self.max_room_number.0));

        self.rebuild_room_state(new_rooms_by_number, new_rooms, max_room_number, connections)
    }

    pub(super) fn delete_room(&self, room_number: RoomNumber) -> Self {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        new_rooms_by_number.remove(&room_number);
        let mut new_rooms = self.rooms.clone();
        new_rooms.retain(|r| r.get_room_number() != room_number);

        let max_room_number = if self.max_room_number == room_number {
            new_rooms
                .iter()
                .map(|r| r.get_room_number())
                .max()
                .unwrap_or(RoomNumber(0))
        } else {
            self.max_room_number
        };

        // §3.3: Connections orphaned by the room's outgoing exits are
        // deleted; those kept alive by a surviving inbound member become
        // dangling. (The inbound destinations themselves are nulled by the
        // caller's follow-up `null_inbound_exits` cascade.)
        let mut connections = self.connections.clone();
        let survivors = Self::topologies_of(&self.id, &new_rooms_by_number, None);
        connection_lifecycle::repair_after_room_delete(room_number, &survivors, &mut connections);

        self.rebuild_room_state(new_rooms_by_number, new_rooms, max_room_number, connections)
    }

    /// Resets every exit in this area that points to `target` to no
    /// destination, returning the rebuilt area, or `None` when nothing here
    /// linked to `target`. Mirrors the server's inbound-exit cascade on room
    /// deletion so the cache never shows exits dangling at a deleted room
    /// until the next sync (see [`RoomCache::null_exits_to`]).
    pub(super) fn null_inbound_exits(&self, target: &RoomKey) -> Option<Self> {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        let mut touched = HashSet::new();
        // The pre-null copies of every affected exit, for Connection repair.
        let mut affected: Vec<(RoomNumber, ExitCache)> = Vec::new();
        for (room_number, room) in &self.rooms_by_number {
            if let Some(updated) = room.null_exits_to(target) {
                for exit in room.get_exits() {
                    if exit.to_area_id == Some(target.area_id)
                        && exit.to_room_number == Some(target.room_number)
                    {
                        affected.push((*room_number, exit.clone()));
                    }
                }
                new_rooms_by_number.insert(*room_number, Arc::new(updated));
                touched.insert(*room_number);
            }
        }

        if touched.is_empty() {
            return None;
        }

        // Each exit that lost its destination drags its Connection along:
        // the far side is gone, so the row becomes dangling (idempotent when
        // a same-area room deletion already repaired it).
        let mut connections = self.connections.clone();
        let site = |number: RoomNumber| {
            new_rooms_by_number.get(&number).map(|room| RoomSite {
                x: room.get_x(),
                y: room.get_y(),
                level: room.get_level(),
            })
        };
        for (room_number, before_exit) in &affected {
            let before = Self::exit_topology(&self.id, *room_number, before_exit);
            let after = ExitTopology {
                to_room_in_area: None,
                to_direction: None,
                leaves_area: false,
                ..before
            };
            let peers = Self::topologies_of(&self.id, &new_rooms_by_number, Some(before_exit.id));
            connection_lifecycle::reattach_after_update(
                &before,
                &after,
                &peers,
                &mut connections,
                site,
            );
        }

        let mut new_rooms = self.rooms.clone();
        new_rooms.retain(|r| !touched.contains(&r.get_room_number()));
        new_rooms.extend(
            touched
                .iter()
                .filter_map(|n| new_rooms_by_number.get(n).cloned()),
        );

        Some(self.rebuild_room_state(
            new_rooms_by_number,
            new_rooms,
            self.max_room_number,
            connections,
        ))
    }

    pub(super) fn set_room_property(
        &self,
        room_number: RoomNumber,
        name: String,
        value: String,
    ) -> CloudResult<Self> {
        let room = self.rooms_by_number.get(&room_number);

        if let Some(room) = room {
            let room = room.set_property(name, value);
            Ok(self.upsert_room_cache(room_number, Arc::new(room)))
        } else {
            Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }))
        }
    }

    pub(super) fn delete_room_property(
        &self,
        room_number: RoomNumber,
        name: &str,
    ) -> CloudResult<Self> {
        let room = self.rooms_by_number.get(&room_number);

        if let Some(room) = room {
            let room = room.delete_property(name);
            Ok(self.upsert_room_cache(room_number, Arc::new(room)))
        } else {
            Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }))
        }
    }

    pub(super) fn add_room_tag(&self, room_number: RoomNumber, tag: &str) -> CloudResult<Self> {
        let room = self.rooms_by_number.get(&room_number);

        if let Some(room) = room {
            let room = room.add_tag(tag);
            Ok(self.upsert_room_cache(room_number, Arc::new(room)))
        } else {
            Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }))
        }
    }

    pub(super) fn remove_room_tag(&self, room_number: RoomNumber, tag: &str) -> CloudResult<Self> {
        let room = self.rooms_by_number.get(&room_number);

        if let Some(room) = room {
            let room = room.remove_tag(tag);
            Ok(self.upsert_room_cache(room_number, Arc::new(room)))
        } else {
            Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }))
        }
    }

    /// Upserts an exit, maintaining its Connection membership: a new exit
    /// attaches (auto-pairing onto the unique reciprocal one-member
    /// candidate or minting a fresh Connection); an existing exit whose
    /// topology changed re-attaches. The incoming `connection_id` on a new
    /// exit is a placeholder and is overwritten here.
    pub(super) fn upsert_exit(
        &self,
        room_number: RoomNumber,
        mut exit: ExitCache,
    ) -> CloudResult<Self> {
        let Some(room) = self.rooms_by_number.get(&room_number) else {
            return Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }));
        };

        let mut connections = self.connections.clone();
        let site = |number: RoomNumber| {
            self.rooms_by_number.get(&number).map(|room| RoomSite {
                x: room.get_x(),
                y: room.get_y(),
                level: room.get_level(),
            })
        };
        match room.get_exits().iter().find(|e| e.id == exit.id) {
            None => {
                let peers = Self::topologies_of(&self.id, &self.rooms_by_number, None);
                let topology = Self::exit_topology(&self.id, room_number, &exit);
                exit.connection_id =
                    connection_lifecycle::attach_exit(&topology, &peers, &mut connections, site);
            }
            Some(existing) => {
                exit.connection_id = existing.connection_id;
                let before = Self::exit_topology(&self.id, room_number, existing);
                let after = Self::exit_topology(&self.id, room_number, &exit);
                if connection_lifecycle::topology_differs(&before, &after) {
                    let peers = Self::topologies_of(&self.id, &self.rooms_by_number, Some(exit.id));
                    exit.connection_id = connection_lifecycle::reattach_after_update(
                        &before,
                        &after,
                        &peers,
                        &mut connections,
                        site,
                    );
                }
            }
        }

        let room = room.upsert_exit(exit);
        Ok(self.upsert_room_cache_with_connections(room_number, Arc::new(room), connections))
    }

    pub(super) fn delete_exit(
        &self,
        room_number: RoomNumber,
        exit_id: ExitId,
    ) -> CloudResult<Self> {
        let Some(room) = self.rooms_by_number.get(&room_number) else {
            return Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }));
        };

        let removed_connection = room
            .get_exits()
            .iter()
            .find(|e| e.id == exit_id)
            .map(|e| e.connection_id);
        let room = room.delete_exit(exit_id);

        // Deleting the last member exit deletes the Connection; a surviving
        // member keeps it (now one-way).
        let mut connections = self.connections.clone();
        if let Some(connection_id) = removed_connection {
            let survivors = Self::topologies_of(&self.id, &self.rooms_by_number, Some(exit_id));
            connection_lifecycle::remove_orphan_connection(
                connection_id,
                &survivors,
                &mut connections,
            );
        }

        Ok(self.upsert_room_cache_with_connections(room_number, Arc::new(room), connections))
    }

    /// Projects one cached exit into its connection-relevant topology.
    fn exit_topology(area_id: &AreaId, from_room: RoomNumber, exit: &ExitCache) -> ExitTopology {
        let same_area = exit.to_area_id.as_ref() == Some(area_id);
        ExitTopology {
            id: exit.id,
            connection_id: exit.connection_id,
            from_room,
            from_direction: exit.from_direction,
            to_room_in_area: if same_area { exit.to_room_number } else { None },
            to_direction: exit.to_direction,
            leaves_area: exit.to_unknown || (!same_area && exit.to_area_id.is_some()),
        }
    }

    /// Every exit's topology, optionally excluding one (the exit being
    /// edited or deleted).
    fn topologies_of(
        area_id: &AreaId,
        rooms_by_number: &HashMap<RoomNumber, Arc<RoomCache>>,
        exclude: Option<ExitId>,
    ) -> Vec<ExitTopology> {
        rooms_by_number
            .iter()
            .flat_map(|(room_number, room)| {
                room.get_exits()
                    .iter()
                    .filter(|exit| Some(exit.id) != exclude)
                    .map(|exit| Self::exit_topology(area_id, *room_number, exit))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(super) fn upsert_label(&self, label_id: LabelId, label: Label) -> Self {
        let mut new_labels = self.labels.clone();
        new_labels.retain(|l| l.id != label_id);
        new_labels.push(label);

        Self {
            rev: self.rev + 1,
            labels: new_labels,
            ..self.clone()
        }
    }

    pub(super) fn delete_label(&self, label_id: LabelId) -> Self {
        let mut new_labels = self.labels.clone();
        new_labels.retain(|l| l.id != label_id);
        Self {
            rev: self.rev + 1,
            labels: new_labels,
            ..self.clone()
        }
    }

    pub(super) fn upsert_shape(&self, shape_id: ShapeId, shape: Shape) -> Self {
        let mut new_shapes = self.shapes.clone();
        new_shapes.retain(|s| s.id != shape_id);
        new_shapes.push(shape);
        Self {
            rev: self.rev + 1,
            shapes: new_shapes,
            ..self.clone()
        }
    }

    pub(super) fn delete_shape(&self, shape_id: ShapeId) -> Self {
        let mut new_shapes = self.shapes.clone();
        new_shapes.retain(|s| s.id != shape_id);
        Self {
            rev: self.rev + 1,
            shapes: new_shapes,
            ..self.clone()
        }
    }

    /// Mirrors a server-acknowledged `secret-marks` change onto the cached
    /// entities, bumping `rev` once (like other local edits, so open editors
    /// notice and resync). Room connections are rebuilt when room or exit
    /// secrecy changed, since [`RoomConnection::is_secret`] derives from
    /// both. Unknown ids are ignored, matching the server's behavior.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_secret_marks(
        &self,
        secret: bool,
        rooms: &[RoomNumber],
        exits: &[ExitId],
        labels: &[LabelId],
        shapes: &[ShapeId],
        room_properties: &[(RoomNumber, String)],
        area_properties: &[String],
    ) -> Self {
        let rooms_touched = !(rooms.is_empty() && exits.is_empty() && room_properties.is_empty());

        let mut next = if rooms_touched {
            let mut new_rooms_by_number = self.rooms_by_number.clone();

            for room_number in rooms {
                if let Some(room) = new_rooms_by_number.get(room_number) {
                    let updated = Arc::new(room.with_secrecy(secret));
                    new_rooms_by_number.insert(*room_number, updated);
                }
            }

            for (room_number, name) in room_properties {
                if let Some(room) = new_rooms_by_number.get(room_number) {
                    let updated = Arc::new(room.with_property_secrecy(name, secret));
                    new_rooms_by_number.insert(*room_number, updated);
                }
            }

            if !exits.is_empty() {
                let updated_rooms: Vec<(RoomNumber, Arc<RoomCache>)> = new_rooms_by_number
                    .iter()
                    .filter_map(|(number, room)| {
                        let flipped: Vec<ExitCache> = room
                            .get_exits()
                            .iter()
                            .filter(|exit| exit.is_secret != secret && exits.contains(&exit.id))
                            .map(|exit| ExitCache {
                                is_secret: secret,
                                ..exit.clone()
                            })
                            .collect();
                        if flipped.is_empty() {
                            return None;
                        }
                        let mut new_room = (**room).clone();
                        for exit in flipped {
                            new_room = new_room.upsert_exit(exit);
                        }
                        Some((*number, Arc::new(new_room)))
                    })
                    .collect();
                for (number, room) in updated_rooms {
                    new_rooms_by_number.insert(number, room);
                }
            }

            // Preserve the original room ordering with the updated instances.
            let new_rooms: Vec<Arc<RoomCache>> = self
                .rooms
                .iter()
                .map(|room| {
                    new_rooms_by_number
                        .get(&room.get_room_number())
                        .cloned()
                        .unwrap_or_else(|| room.clone())
                })
                .collect();

            self.rebuild_room_state(
                new_rooms_by_number,
                new_rooms,
                self.max_room_number,
                self.connections.clone(),
            )
        } else {
            Self {
                rev: self.rev + 1,
                ..self.clone()
            }
        };

        for label in &mut next.labels {
            if labels.contains(&label.id) {
                label.is_secret = secret;
            }
        }
        for shape in &mut next.shapes {
            if shapes.contains(&shape.id) {
                shape.is_secret = secret;
            }
        }
        for name in area_properties {
            if let Some(entry) = next.properties.get_mut(name) {
                entry.is_secret = secret;
            }
        }

        next.has_secrets = next.compute_has_secrets();
        next
    }

    #[must_use]
    pub fn get_max_room_number(&self) -> RoomNumber {
        self.max_room_number
    }

    #[must_use]
    pub fn get_room_connections(&self) -> &[RoomConnection] {
        &self.room_connections
    }

    /// Stored Connection rows (as opposed to their resolved render views).
    #[must_use]
    pub fn get_connections(&self) -> &[Connection] {
        &self.connections
    }

    #[must_use]
    pub fn get_connection(&self, id: crate::ConnectionId) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|connection| connection.id == id)
    }

    /// Rebuilds the full editable document used by the shared mutation
    /// applier. Cache-only metadata that the editor never mutates is retained;
    /// list-only family tokens and linked-area presentation rows are omitted.
    #[must_use]
    pub(super) fn to_details(&self) -> AreaWithDetails {
        let mut properties: Vec<_> = self
            .properties
            .iter()
            .map(|(name, entry)| crate::Property {
                name: name.clone(),
                value: entry.value.clone(),
                is_secret: entry.is_secret,
            })
            .collect();
        properties.sort_by(|a, b| a.name.cmp(&b.name));
        AreaWithDetails {
            area: Area {
                id: self.id,
                user_id: self.meta.owner_id,
                atlas_id: self.meta.atlas_id,
                atlas_name: self.meta.atlas_name.clone(),
                name: self.name.clone(),
                created_at: Utc::now(),
                rev: self.rev,
                access: self.meta.access,
                owner_nickname: self.meta.owner_nickname.clone(),
                copied_from_area_id: self.meta.copied_from_area_id,
                copied_from_rev: self.meta.copied_from_rev,
                copied_at: self.meta.copied_at,
                family_token: None,
            },
            format_version: AREA_FORMAT_VERSION,
            content_hash: self.meta.content_hash.clone(),
            properties,
            rooms: self.rooms.iter().map(|room| room.to_details()).collect(),
            labels: self.labels.clone(),
            shapes: self.shapes.clone(),
            connections: self.connections.clone(),
            linked_areas: Vec::new(),
        }
    }

    pub fn with_rooms_in<F>(&self, min_x: f32, min_y: f32, max_x: f32, max_y: f32, mut fun: F)
    where
        F: FnMut(&Arc<RoomCache>),
    {
        let envelope = bounds_to_envelope(min_x, min_y, max_x, max_y);
        for entry in self.rooms_index.locate_in_envelope_intersecting(&envelope) {
            fun(&entry.room);
        }
    }

    pub fn with_room_connections_in<F>(
        &self,
        min_x: f32,
        min_y: f32,
        max_x: f32,
        max_y: f32,
        mut fun: F,
    ) where
        F: FnMut(&RoomConnection),
    {
        let envelope = bounds_to_envelope(min_x, min_y, max_x, max_y);
        for entry in self
            .room_connections_index
            .locate_in_envelope_intersecting(&envelope)
        {
            if let Some(connection) = self.room_connections.get(entry.index) {
                fun(connection);
            }
        }
    }
}

fn bounds_to_envelope(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> AABB<[f32; 2]> {
    let (min_x, max_x) = if min_x <= max_x {
        (min_x, max_x)
    } else {
        (max_x, min_x)
    };
    let (min_y, max_y) = if min_y <= max_y {
        (min_y, max_y)
    } else {
        (max_y, min_y)
    };

    AABB::from_corners([min_x, min_y], [max_x, max_y])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionRouting, CornerStyle,
        ExitDirection, PortMode, RoomSide, RoomUpdates, SegmentShape,
    };
    use uuid::Uuid;

    /// A visible exit leaving via `from_direction` toward `(to_area,
    /// to_room)` (arriving from `to_direction`), as a member of `connection`.
    fn member_exit(
        id: u128,
        connection: ConnectionId,
        from_direction: ExitDirection,
        to_area: Option<AreaId>,
        to_room: Option<RoomNumber>,
        to_direction: Option<ExitDirection>,
    ) -> ExitCache {
        ExitCache {
            id: ExitId(Uuid::from_u128(id)),
            from_direction,
            to_area_id: to_area,
            to_room_number: to_room,
            to_direction,
            path: None,
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: None,
            connection_id: connection,
            to_unknown: false,
            to_area_token: None,
            is_secret: false,
        }
    }

    fn endpoint(room: RoomNumber, side: RoomSide) -> ConnectionEndpoint {
        ConnectionEndpoint {
            room_number: room,
            side,
            port_offset: 0.5,
            port_mode: PortMode::AutoPinned,
        }
    }

    fn stored_connection(
        id: ConnectionId,
        kind: ConnectionKind,
        endpoint_a: ConnectionEndpoint,
        endpoint_b: Option<ConnectionEndpoint>,
    ) -> Connection {
        Connection {
            id,
            endpoint_a,
            endpoint_b,
            kind,
            routing: ConnectionRouting::Simple,
            segment_shape: SegmentShape::Direct,
            corner: CornerStyle::Sharp,
            route_points: Vec::new(),
            dash: ConnectionDash::Solid,
            color: crate::DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: 1.0,
        }
    }

    fn placed_room(number: RoomNumber, x: f32, y: f32, level: i32) -> RoomCache {
        RoomCache::new(number).apply_updates(RoomUpdates {
            x: Some(x),
            y: Some(y),
            level: Some(level),
            ..RoomUpdates::default()
        })
    }

    fn rooms_by_number(list: Vec<RoomCache>) -> HashMap<RoomNumber, Arc<RoomCache>> {
        list.into_iter()
            .map(|room| (room.get_room_number(), Arc::new(room)))
            .collect()
    }

    #[test]
    fn self_loop_connection_resolves_a_self_loop_end() {
        let area = AreaId(Uuid::from_u128(1));
        let n = RoomNumber(1);
        let connection_id = ConnectionId::new();
        // North leaves and returns to the same room (arriving from the south).
        let room = placed_room(n, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::North,
            Some(area),
            Some(n),
            Some(ExitDirection::South),
        ));
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::SelfLoop,
            endpoint(n, RoomSide::North),
            Some(endpoint(n, RoomSide::South)),
        )];

        let conns =
            AreaCache::build_room_connections(&area, &connections, &rooms_by_number(vec![room]));

        assert_eq!(conns.len(), 1);
        assert!(matches!(conns[0].to, RoomConnectionEnd::SelfLoop));
        assert_eq!(conns[0].connection_id, connection_id);
        // Self-loops carry no arrow.
        assert!(conns[0].arrow_toward_b.is_none());
        assert!(!conns[0].geometry.circles.is_empty(), "loop arc resolved");
    }

    #[test]
    fn one_member_internal_connection_resolves_normal_with_an_arrow() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::East,
            Some(area),
            Some(b),
            Some(ExitDirection::West),
        ));
        let room_b = placed_room(b, 4.0, 0.0, 0);
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::Internal,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );

        assert_eq!(conns.len(), 1);
        let conn = &conns[0];
        assert!(matches!(
            conn.to,
            RoomConnectionEnd::Normal {
                direction: ExitDirection::West,
                ..
            }
        ));
        assert!(!conn.is_bidirectional);
        // The single member runs A→B: arrow at B.
        assert_eq!(conn.arrow_toward_b, Some(true));
        assert!(!conn.geometry.centerline.is_empty(), "stroke resolved");
        assert!(!conn.geometry.bounds.is_empty());
    }

    #[test]
    fn two_member_connection_is_bidirectional_without_an_arrow() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::East,
            Some(area),
            Some(b),
            Some(ExitDirection::West),
        ));
        let room_b = placed_room(b, 4.0, 0.0, 0).upsert_exit(member_exit(
            11,
            connection_id,
            ExitDirection::West,
            Some(area),
            Some(a),
            Some(ExitDirection::East),
        ));
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::Internal,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );

        assert_eq!(conns.len(), 1);
        assert!(conns[0].is_bidirectional);
        assert!(conns[0].arrow_toward_b.is_none());
    }

    #[test]
    fn cross_level_connection_emits_two_halves_sharing_geometry() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::Up,
            Some(area),
            Some(b),
            Some(ExitDirection::Down),
        ));
        let room_b = placed_room(b, 1.0, 1.0, 1);
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::CrossLevel,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );

        assert_eq!(conns.len(), 2);
        let by_level = |level: i32| {
            conns
                .iter()
                .find(|c| c.from_level == level)
                .expect("one half per level")
        };
        let (half_a, half_b) = (by_level(0), by_level(1));
        assert!(matches!(
            half_a.to,
            RoomConnectionEnd::ToLevel { level: 1, .. }
        ));
        assert!(matches!(
            half_b.to,
            RoomConnectionEnd::ToLevel { level: 0, .. }
        ));
        assert!(
            Arc::ptr_eq(&half_a.geometry, &half_b.geometry),
            "both halves share one resolved geometry"
        );
    }

    #[test]
    fn dangling_external_and_unknown_members_resolve_their_ends() {
        let area = AreaId(Uuid::from_u128(1));
        let other_area = AreaId(Uuid::from_u128(2));
        let n = RoomNumber(1);
        let dangling_id = ConnectionId::new();
        let external_id = ConnectionId::new();
        let unknown_id = ConnectionId::new();
        let unknown_exit = ExitCache {
            to_unknown: true,
            to_area_token: Some("tok".to_string()),
            ..member_exit(12, unknown_id, ExitDirection::South, None, None, None)
        };
        let room = placed_room(n, 0.0, 0.0, 0)
            .upsert_exit(member_exit(
                10,
                dangling_id,
                ExitDirection::North,
                None,
                None,
                None,
            ))
            .upsert_exit(member_exit(
                11,
                external_id,
                ExitDirection::East,
                Some(other_area),
                Some(RoomNumber(7)),
                None,
            ))
            .upsert_exit(unknown_exit);
        let connections = vec![
            stored_connection(
                dangling_id,
                ConnectionKind::Dangling,
                endpoint(n, RoomSide::North),
                None,
            ),
            stored_connection(
                external_id,
                ConnectionKind::External,
                endpoint(n, RoomSide::East),
                None,
            ),
            stored_connection(
                unknown_id,
                ConnectionKind::External,
                endpoint(n, RoomSide::South),
                None,
            ),
        ];

        let conns =
            AreaCache::build_room_connections(&area, &connections, &rooms_by_number(vec![room]));

        assert_eq!(conns.len(), 3);
        let end_of = |id: ConnectionId| &conns.iter().find(|c| c.connection_id == id).unwrap().to;
        assert!(matches!(end_of(dangling_id), RoomConnectionEnd::None));
        assert!(matches!(
            end_of(external_id),
            RoomConnectionEnd::External { area_id } if *area_id == other_area
        ));
        assert!(matches!(
            end_of(unknown_id),
            RoomConnectionEnd::Unknown { token } if token == "tok"
        ));
    }

    #[test]
    fn corrupt_connections_are_skipped_not_fatal() {
        let area = AreaId(Uuid::from_u128(1));
        let n = RoomNumber(1);
        let memberless = ConnectionId::new();
        let missing_room = ConnectionId::new();
        let orphan_member = ConnectionId::new();
        // An exit whose connection row is missing entirely: it simply does
        // not render.
        let room = placed_room(n, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            orphan_member,
            ExitDirection::North,
            None,
            None,
            None,
        ));
        let connections = vec![
            stored_connection(
                memberless,
                ConnectionKind::Dangling,
                endpoint(n, RoomSide::North),
                None,
            ),
            stored_connection(
                missing_room,
                ConnectionKind::Dangling,
                endpoint(RoomNumber(99), RoomSide::North),
                None,
            ),
        ];

        let conns =
            AreaCache::build_room_connections(&area, &connections, &rooms_by_number(vec![room]));
        assert!(conns.is_empty());
    }

    #[test]
    fn secrecy_folds_member_exits_and_endpoint_rooms() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::East,
            Some(area),
            Some(b),
            Some(ExitDirection::West),
        ));
        // The *destination* room is secret: the resolved view must be too.
        let room_b = placed_room(b, 4.0, 0.0, 0).with_secrecy(true);
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::Internal,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );
        assert!(conns[0].is_secret);
    }
}
