use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, Utc};

use crate::{
    AreaAccess, AreaId, AreaWithDetails, AtlasId, ExitId, ExitStyle, Label, LabelId, CloudError, CloudResult,
    RoomNumber, RoomUpdates, Shape, ShapeId,
    mapper::{
        RoomKey,
        exit_cache::ExitCache,
        room_cache::{PropertyEntry, RoomCache},
        room_connection::{RoomConnection, RoomConnectionEnd},
    },
};
use rstar::{AABB, RTree, RTreeObject};

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
        let bounds = connection_bounds(connection);
        Self { bounds, index }
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
            .map(|(index, connection)| ConnectionSpatialEntry::new(index, connection))
            .collect();
        RTree::bulk_load(entries)
    }

    fn rebuild_room_state(
        &self,
        rooms_by_number: HashMap<RoomNumber, Arc<RoomCache>>,
        rooms: Vec<Arc<RoomCache>>,
        max_room_number: RoomNumber,
    ) -> Self {
        let room_connections = Self::build_room_connections(&self.id, &rooms_by_number);
        let rooms_index = Self::build_rooms_index(&rooms);
        let room_connections_index = Self::build_room_connections_index(&room_connections);

        Self {
            rev: self.rev + 1,
            rooms_by_number,
            rooms,
            max_room_number,
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

        let room_connections = Self::build_room_connections(&area.area.id, &rooms_by_number);
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

    #[allow(clippy::too_many_lines)]
    fn build_room_connections(
        area_id: &AreaId,
        rooms_by_number: &HashMap<RoomNumber, Arc<RoomCache>>,
    ) -> Vec<RoomConnection> {
        let mut skip_exit_ids = HashSet::new();

        let mut room_connections = Vec::new();

        for room in rooms_by_number.values() {
            for from_exit in room.get_exits() {
                if skip_exit_ids.contains(&from_exit.id) {
                    continue;
                }

                // let's see if we have a matching exit coming back
                let paired_room: Option<&Arc<RoomCache>> =
                    if from_exit.to_area_id.as_ref() == Some(area_id) {
                        from_exit
                            .to_room_number
                            .and_then(|ref n| rooms_by_number.get(n))
                    } else {
                        // if the exit is in a different area, let's say it isn't bidirectional for simplicy's sake
                        // (this field is meant primarily for the graphical mapper, which will only show one area at a time)
                        None
                    };

                let mut is_bidirectional = false;
                let mut paired_exit_secret = false;
                let mut paired_exit_style: Option<ExitStyle> = None;

                if let Some(paired_room) = paired_room {
                    for paired_exit in paired_room.get_exits() {
                        if paired_exit.to_area_id.as_ref() == Some(area_id)
                            && paired_exit.to_room_number == Some(room.get_room_number())
                            && paired_exit.to_direction == Some(from_exit.from_direction)
                            && Some(paired_exit.from_direction) == from_exit.to_direction
                        {
                            is_bidirectional = true;
                            paired_exit_secret = paired_exit.is_secret;
                            paired_exit_style = Some(paired_exit.style);
                            skip_exit_ids.insert(paired_exit.id);
                        }
                    }
                }

                let exit_secret = from_exit.is_secret || room.is_secret();
                let exit_style = from_exit.style;

                if let Some(paired_room) = paired_room {
                    let is_secret =
                        exit_secret || paired_exit_secret || paired_room.is_secret();
                    if paired_room.get_room_number() == room.get_room_number() {
                        // Self-loop: the exit returns to its own room. A Normal
                        // end would collapse to a bare stub (source == target),
                        // reading identically to a dangling exit, so emit a
                        // SelfLoop for the renderer to mark with a loop arc.
                        room_connections.push(RoomConnection {
                            from_level: room.get_level(),
                            from_x: room.get_x(),
                            from_y: room.get_y(),
                            from_direction: from_exit.from_direction,
                            room: room.clone(),
                            is_bidirectional,
                            is_secret,
                            style: exit_style,
                            to: RoomConnectionEnd::SelfLoop,
                        });
                    } else if paired_room.get_level() == room.get_level() {
                        room_connections.push(RoomConnection {
                            from_level: room.get_level(),
                            from_x: room.get_x(),
                            from_y: room.get_y(),
                            from_direction: from_exit.from_direction,
                            room: room.clone(),
                            is_bidirectional,
                            is_secret,
                            style: exit_style,
                            to: RoomConnectionEnd::Normal {
                                x: paired_room.get_x(),
                                y: paired_room.get_y(),
                                direction: from_exit.to_direction.unwrap_or_default(),
                                room: paired_room.clone(),
                            },
                        });
                    } else {
                        room_connections.push(RoomConnection {
                            from_level: room.get_level(),
                            from_x: room.get_x(),
                            from_y: room.get_y(),
                            from_direction: from_exit.from_direction,
                            room: room.clone(),
                            is_bidirectional,
                            is_secret,
                            style: exit_style,
                            to: RoomConnectionEnd::ToLevel {
                                level: paired_room.get_level(),
                                x: paired_room.get_x(),
                                y: paired_room.get_y(),
                                direction: from_exit.to_direction.unwrap_or_default(),
                                room: paired_room.clone(),
                            },
                        });
                        room_connections.push(RoomConnection {
                            from_level: paired_room.get_level(),
                            from_x: paired_room.get_x(),
                            from_y: paired_room.get_y(),
                            from_direction: from_exit.to_direction.unwrap_or_default(),
                            room: paired_room.clone(),
                            is_bidirectional,
                            is_secret,
                            style: paired_exit_style.unwrap_or(exit_style),
                            to: RoomConnectionEnd::ToLevel {
                                level: room.get_level(),
                                x: room.get_x(),
                                y: room.get_y(),
                                direction: from_exit.from_direction,
                                room: room.clone(),
                            },
                        });
                    }
                } else if from_exit.to_unknown {
                    // Redacted destination: the server nulled `to_*` and set
                    // `to_unknown`, so this must render as "Unknown map" —
                    // not as a dangling exit.
                    room_connections.push(RoomConnection {
                        from_level: room.get_level(),
                        from_x: room.get_x(),
                        from_y: room.get_y(),
                        from_direction: from_exit.from_direction,
                        room: room.clone(),
                        is_bidirectional,
                        is_secret: exit_secret,
                        style: exit_style,
                        to: RoomConnectionEnd::Unknown {
                            token: from_exit.to_area_token.clone().unwrap_or_default(),
                        },
                    });
                } else if let Some(area_id) = from_exit.to_area_id {
                    room_connections.push(RoomConnection {
                        from_level: room.get_level(),
                        from_x: room.get_x(),
                        from_y: room.get_y(),
                        from_direction: from_exit.from_direction,
                        room: room.clone(),
                        is_bidirectional,
                        is_secret: exit_secret,
                        style: exit_style,
                        to: RoomConnectionEnd::External { area_id },
                    });
                } else {
                    room_connections.push(RoomConnection {
                        from_level: room.get_level(),
                        from_x: room.get_x(),
                        from_y: room.get_y(),
                        from_direction: from_exit.from_direction,
                        room: room.clone(),
                        is_bidirectional,
                        is_secret: exit_secret,
                        style: exit_style,
                        to: RoomConnectionEnd::None,
                    });
                }
            }
        }

        room_connections
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

        self.rebuild_room_state(new_rooms_by_number, new_rooms, max_room_number)
    }

    fn upsert_room_cache(&self, room_number: RoomNumber, room: Arc<RoomCache>) -> Self {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        let mut new_rooms = self.rooms.clone();

        new_rooms_by_number.insert(room_number, room.clone());
        new_rooms.retain(|r| r.get_room_number() != room_number);
        new_rooms.push(room);
        let max_room_number = RoomNumber(room_number.0.max(self.max_room_number.0));

        self.rebuild_room_state(new_rooms_by_number, new_rooms, max_room_number)
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

        self.rebuild_room_state(new_rooms_by_number, new_rooms, max_room_number)
    }

    /// Resets every exit in this area that points to `target` to no
    /// destination, returning the rebuilt area, or `None` when nothing here
    /// linked to `target`. Mirrors the server's inbound-exit cascade on room
    /// deletion so the cache never shows exits dangling at a deleted room
    /// until the next sync (see [`RoomCache::null_exits_to`]).
    pub(super) fn null_inbound_exits(&self, target: &RoomKey) -> Option<Self> {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        let mut touched = HashSet::new();
        for (room_number, room) in &self.rooms_by_number {
            if let Some(updated) = room.null_exits_to(target) {
                new_rooms_by_number.insert(*room_number, Arc::new(updated));
                touched.insert(*room_number);
            }
        }

        if touched.is_empty() {
            return None;
        }

        let mut new_rooms = self.rooms.clone();
        new_rooms.retain(|r| !touched.contains(&r.get_room_number()));
        new_rooms.extend(
            touched
                .iter()
                .filter_map(|n| new_rooms_by_number.get(n).cloned()),
        );

        Some(self.rebuild_room_state(new_rooms_by_number, new_rooms, self.max_room_number))
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

    pub(super) fn upsert_exit(&self, room_number: RoomNumber, exit: ExitCache) -> CloudResult<Self> {
        let room = self.rooms_by_number.get(&room_number);

        if let Some(room) = room {
            let room = room.upsert_exit(exit);
            Ok(self.upsert_room_cache(room_number, Arc::new(room)))
        } else {
            Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }))
        }
    }

    pub(super) fn delete_exit(&self, room_number: RoomNumber, exit_id: ExitId) -> CloudResult<Self> {
        let room = self.rooms_by_number.get(&room_number);

        if let Some(room) = room {
            let room = room.delete_exit(exit_id);
            Ok(self.upsert_room_cache(room_number, Arc::new(room)))
        } else {
            Err(CloudError::RoomNotFound(RoomKey {
                area_id: self.id,
                room_number,
            }))
        }
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
        let rooms_touched =
            !(rooms.is_empty() && exits.is_empty() && room_properties.is_empty());

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
                            .filter(|exit| {
                                exit.is_secret != secret && exits.contains(&exit.id)
                            })
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

            self.rebuild_room_state(new_rooms_by_number, new_rooms, self.max_room_number)
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

fn connection_bounds(connection: &RoomConnection) -> AABB<[f32; 2]> {
    let (target_x, target_y) = match &connection.to {
        RoomConnectionEnd::Normal { x, y, .. } | RoomConnectionEnd::ToLevel { x, y, .. } => {
            (*x, *y)
        }
        RoomConnectionEnd::External { .. }
        | RoomConnectionEnd::Unknown { .. }
        | RoomConnectionEnd::SelfLoop
        | RoomConnectionEnd::None => (connection.from_x, connection.from_y),
    };

    let min_x = connection.from_x.min(target_x);
    let max_x = connection.from_x.max(target_x);
    let min_y = connection.from_y.min(target_y);
    let max_y = connection.from_y.max(target_y);

    AABB::from_corners([min_x, min_y], [max_x, max_y])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExitDirection;
    use uuid::Uuid;

    /// A visible exit leaving via `from_direction` and arriving at
    /// `(to_area, to_room)` from `to_direction`.
    fn linked_exit(
        id: u128,
        from_direction: ExitDirection,
        to_area: AreaId,
        to_room: RoomNumber,
        to_direction: ExitDirection,
    ) -> ExitCache {
        ExitCache {
            id: ExitId(Uuid::from_u128(id)),
            from_direction,
            to_area_id: Some(to_area),
            to_room_number: Some(to_room),
            to_direction: Some(to_direction),
            path: None,
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: None,
            style: ExitStyle::Normal,
            color: None,
            iced_color: iced::Color::from_rgb8(128, 128, 128),
            to_unknown: false,
            to_area_token: None,
            is_secret: false,
        }
    }

    fn rooms_by_number(list: Vec<RoomCache>) -> HashMap<RoomNumber, Arc<RoomCache>> {
        list.into_iter()
            .map(|room| (room.get_room_number(), Arc::new(room)))
            .collect()
    }

    #[test]
    fn self_referential_exit_builds_a_self_loop_end() {
        let area = AreaId(Uuid::from_u128(1));
        let n = RoomNumber(1);
        // North leaves and returns to the same room (arriving from the south).
        let room = RoomCache::new(n).upsert_exit(linked_exit(
            10,
            ExitDirection::North,
            area,
            n,
            ExitDirection::South,
        ));

        let conns = AreaCache::build_room_connections(&area, &rooms_by_number(vec![room]));

        assert_eq!(conns.len(), 1);
        assert!(matches!(conns[0].to, RoomConnectionEnd::SelfLoop));
        // The loop attaches to the wall the exit leaves by.
        assert_eq!(conns[0].from_direction, ExitDirection::North);
    }

    #[test]
    fn exit_to_a_different_room_stays_normal() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let room_a = RoomCache::new(a).upsert_exit(linked_exit(
            10,
            ExitDirection::East,
            area,
            b,
            ExitDirection::West,
        ));
        let room_b = RoomCache::new(b);

        let conns =
            AreaCache::build_room_connections(&area, &rooms_by_number(vec![room_a, room_b]));

        assert_eq!(conns.len(), 1);
        assert!(matches!(conns[0].to, RoomConnectionEnd::Normal { .. }));
    }
}
