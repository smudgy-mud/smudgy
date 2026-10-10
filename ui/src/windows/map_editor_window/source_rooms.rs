//! Edits to the own rooms of a map's other sources (a Secret, or the
//! caller's Private additions). Each command sends its operations to that
//! source, so the room is edited where it lives whatever mode the editor is
//! in; modes only decide where new content goes.

use std::collections::BTreeMap;
use std::sync::Arc;

use iced::Vector;
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::mapper::area_cache::{AreaCache, SourceLayer};
use smudgy_cloud::mapper::room_cache::RoomCache;
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{
    AreaId, LabelArgs, LabelId, RoomNumber, RoomUpdates, ShapeArgs, ShapeId, SourceId, Uuid,
};
use smudgy_map_widget::map_editor::Selection;
use smudgy_map_widget::sources;

use super::commands::{CoalesceKey, Command, EntityRef, FieldId, Mutation};

/// A source's data on a room, identifying the data's owner and the room's owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRoomRef {
    pub area_id: AreaId,
    pub source: SourceId,
    pub room_number: RoomNumber,
    /// The source that owns the anchored room, independently of the data.
    pub room_source: SourceId,
}

impl SourceRoomRef {
    /// One of `source`'s own rooms.
    #[must_use]
    pub fn own(area_id: AreaId, source: SourceId, room_number: RoomNumber) -> Self {
        Self {
            area_id,
            source,
            room_number,
            room_source: source,
        }
    }

    /// What `source` keeps for the map's room `room_number`.
    #[cfg(test)]
    #[must_use]
    pub fn data_on(area_id: AreaId, source: SourceId, room_number: RoomNumber) -> Self {
        Self::data_on_source(area_id, source, SourceId::Map, room_number)
    }

    /// What `source` keeps for a readable room in `room_source`.
    #[must_use]
    pub fn data_on_source(
        area_id: AreaId,
        source: SourceId,
        room_source: SourceId,
        room_number: RoomNumber,
    ) -> Self {
        Self {
            area_id,
            source,
            room_source,
            room_number,
        }
    }

    /// The room's source on the wire: absent for a map room.
    fn wire_source(self) -> Option<SourceId> {
        (!self.room_source.is_map()).then_some(self.room_source)
    }

    fn entity(self) -> EntityRef {
        if self.source != self.room_source {
            EntityRef::RoomData(
                self.area_id,
                self.source,
                self.room_source,
                self.room_number,
            )
        } else if self.source.is_map() {
            EntityRef::Room(smudgy_cloud::mapper::RoomKey::new(
                self.area_id,
                self.room_number,
            ))
        } else {
            EntityRef::SourceRoom(self.area_id, self.source, self.room_number)
        }
    }
}

fn layer(area: &AreaCache, source: SourceId) -> Option<&SourceLayer> {
    area.source_layers()
        .iter()
        .find(|layer| layer.source() == source)
}

/// The source's own room or its content attached to another source's room.
/// An unreadable anchor has no attachment view in the cache.
fn room(area: &AreaCache, target: SourceRoomRef) -> Option<&Arc<RoomCache>> {
    if target.source == target.room_source {
        return if target.source.is_map() {
            area.get_room(&target.room_number)
        } else {
            sources::source_room(area, target.source, target.room_number)
        };
    }
    let layer = area.document_layer(target.source)?;
    layer.attachment(smudgy_cloud::RoomAddress::new(
        target.room_source,
        target.room_number,
    ))
}

fn batch(
    target_area: AreaId,
    source: SourceId,
    operations: Vec<AreaMutation>,
    description: String,
) -> Mutation {
    Mutation::SourceBatch {
        area_id: target_area,
        source,
        operations,
        description,
        split_paired_exit: false,
    }
}

fn upsert(target: SourceRoomRef, body: RoomUpdates) -> AreaMutation {
    AreaMutation::UpsertRoom {
        room_number: target.room_number,
        room_source: Some(target.source),
        body,
    }
}

/// The number a new room in `place` of map `area_id` takes. Each place
/// numbers its own rooms: the map's next number comes from the map's rooms
/// alone, a Secret's or the Private additions' from their own, and Private
/// additions not yet written start at 1. Allocation is the mapper's:
/// reservation-aware, one above the place's highest room.
pub fn new_room_number(
    mapper: &smudgy_cloud::Mapper,
    area_id: AreaId,
    place: SourceId,
) -> smudgy_cloud::CloudResult<RoomNumber> {
    if place.is_map() {
        return mapper.try_next_room_number(&area_id);
    }
    let atlas = mapper.get_current_atlas();
    let area = atlas
        .get_area(&area_id)
        .ok_or(smudgy_cloud::CloudError::AreaNotFound(area_id))?;
    match layer(&area, place) {
        Some(layer) => mapper.try_next_room_number(&layer.area_id()),
        None if place == SourceId::Private => Ok(RoomNumber(1)),
        None => Err(smudgy_cloud::CloudError::AreaNotFound(area_id)),
    }
}

/// A new, empty room in `source` at `at` on `level`.
pub fn create_room(
    area_id: AreaId,
    source: SourceId,
    room_number: RoomNumber,
    at: iced::Point,
    level: i32,
) -> Command {
    let description = format!("Create room {room_number}");
    let body = RoomUpdates {
        title: Some(String::new()),
        description: Some(String::new()),
        level: Some(level),
        x: Some(at.x),
        y: Some(at.y),
        color: Some(String::new()),
        external_id: None,
    };
    Command::new(
        vec![batch(
            area_id,
            source,
            vec![AreaMutation::CreateRoom {
                room_number,
                room_source: Some(source),
                body,
            }],
            description.clone(),
        )],
        vec![batch(
            area_id,
            source,
            vec![AreaMutation::DeleteRoom {
                room_number,
                room_source: Some(source),
            }],
            description,
        )],
    )
}

/// A new label in `source`, under an id chosen here so the command can
/// select it and undo can name it.
pub fn create_label(area_id: AreaId, source: SourceId, mut body: LabelArgs) -> (Command, LabelId) {
    let label_id = LabelId(Uuid::new_v4());
    body.id = Some(label_id);
    let description = "Create label".to_string();
    let command = Command::new(
        vec![batch(
            area_id,
            source,
            vec![AreaMutation::CreateLabel { body }],
            description.clone(),
        )],
        vec![batch(
            area_id,
            source,
            vec![AreaMutation::DeleteLabel { label_id }],
            description,
        )],
    );
    (command, label_id)
}

/// A new shape in `source`, under an id chosen here.
pub fn create_shape(area_id: AreaId, source: SourceId, mut body: ShapeArgs) -> (Command, ShapeId) {
    let shape_id = ShapeId(Uuid::new_v4());
    body.id = Some(shape_id);
    let description = "Create shape".to_string();
    let command = Command::new(
        vec![batch(
            area_id,
            source,
            vec![AreaMutation::CreateShape { body }],
            description.clone(),
        )],
        vec![batch(
            area_id,
            source,
            vec![AreaMutation::DeleteShape { shape_id }],
            description,
        )],
    );
    (command, shape_id)
}

/// The room's current values of the fields `updates` sets, for undo.
fn prior_fields(room: &RoomCache, updates: &RoomUpdates) -> RoomUpdates {
    RoomUpdates {
        title: updates.title.as_ref().map(|_| room.get_title().to_string()),
        description: updates
            .description
            .as_ref()
            .map(|_| room.get_description().to_string()),
        level: updates.level.map(|_| room.get_level()),
        x: updates.x.map(|_| room.get_x()),
        y: updates.y.map(|_| room.get_y()),
        color: updates.color.as_ref().map(|_| room.get_color().to_string()),
        external_id: updates
            .external_id
            .as_ref()
            .map(|_| room.get_external_id().map(str::to_string)),
    }
}

/// The same field changes to each of the selection's source rooms, one
/// batch per source (bulk color and level).
pub fn edit_rooms(
    area: &AreaCache,
    selection: &Selection,
    updates: &RoomUpdates,
) -> (Vec<Mutation>, Vec<Mutation>) {
    update_rooms(area, selection, "Update", |room| {
        (updates.clone(), prior_fields(room, updates))
    })
}

/// A field edit, coalescing like the map room's.
pub fn edit_field(
    atlas: &Arc<AtlasCache>,
    target: SourceRoomRef,
    field: FieldId,
    updates: RoomUpdates,
) -> Option<Command> {
    let area = atlas.get_area(&target.area_id)?;
    let room = room(&area, target)?;
    let prior = prior_fields(room, &updates);
    let description = format!("Update room {}", target.room_number);
    Some(
        Command::new(
            vec![batch(
                target.area_id,
                target.source,
                vec![upsert(target, updates)],
                description.clone(),
            )],
            vec![batch(
                target.area_id,
                target.source,
                vec![upsert(target, prior)],
                description,
            )],
        )
        .coalescing(CoalesceKey::new(target.entity(), field)),
    )
}

pub fn set_property(
    atlas: &Arc<AtlasCache>,
    target: SourceRoomRef,
    name: String,
    value: String,
) -> Option<Command> {
    let area = atlas.get_area(&target.area_id)?;
    let existing = room(&area, target);
    if existing.is_none() && target.source == target.room_source {
        return None;
    }
    let set = |value: String| AreaMutation::UpsertRoomProperty {
        room_number: target.room_number,
        room_source: target.wire_source(),
        name: name.clone(),
        value,
    };
    let undo = match existing.and_then(|room| room.get_property(&name)) {
        Some(prior) if prior == value => return None,
        Some(prior) => set(prior.to_string()),
        None => AreaMutation::DeleteRoomProperty {
            room_number: target.room_number,
            room_source: target.wire_source(),
            name: name.clone(),
        },
    };
    let description = format!("Set property {name}");
    Some(
        Command::new(
            vec![batch(
                target.area_id,
                target.source,
                vec![set(value)],
                description.clone(),
            )],
            vec![batch(
                target.area_id,
                target.source,
                vec![undo],
                description,
            )],
        )
        .coalescing(CoalesceKey::with_detail(
            target.entity(),
            FieldId::Property,
            name.clone(),
        )),
    )
}

pub fn delete_property(
    atlas: &Arc<AtlasCache>,
    target: SourceRoomRef,
    name: String,
) -> Option<Command> {
    let area = atlas.get_area(&target.area_id)?;
    let prior = room(&area, target)?.get_property(&name)?.to_string();
    let description = format!("Delete property {name}");
    Some(Command::new(
        vec![batch(
            target.area_id,
            target.source,
            vec![AreaMutation::DeleteRoomProperty {
                room_number: target.room_number,
                room_source: target.wire_source(),
                name: name.clone(),
            }],
            description.clone(),
        )],
        vec![batch(
            target.area_id,
            target.source,
            vec![AreaMutation::UpsertRoomProperty {
                room_number: target.room_number,
                room_source: target.wire_source(),
                name,
                value: prior,
            }],
            description,
        )],
    ))
}

fn tag_change(target: SourceRoomRef, tag: String, add: bool) -> AreaMutation {
    if add {
        AreaMutation::AddRoomTag {
            room_number: target.room_number,
            room_source: target.wire_source(),
            tag,
        }
    } else {
        AreaMutation::RemoveRoomTag {
            room_number: target.room_number,
            room_source: target.wire_source(),
            tag,
        }
    }
}

/// A tag change and how many rooms it writes.
#[derive(Debug)]
pub struct TagChange {
    pub command: Command,
    pub changed: usize,
}

/// Adds (`add`) or removes `tag` in `place` alone, on the qualified readable
/// `rooms`. The tag owner need not own the room. One batch for the place and
/// one undo entry, which `place_name` names; rooms where it changes nothing
/// are left out, and `None` means it changes nothing anywhere.
#[must_use]
pub fn change_tags(
    area: &AreaCache,
    place: SourceId,
    rooms: &[(SourceId, RoomNumber)],
    tag: &str,
    add: bool,
    place_name: &str,
) -> Option<TagChange> {
    let tag = smudgy_cloud::mapper::normalize_tag(tag);
    if tag.is_empty() {
        return None;
    }
    let area_id = *area.get_id();
    let mut targets = Vec::new();
    for &(anchor, number) in rooms {
        if area.keeps_only_own_rooms(place) && anchor != place {
            continue;
        }
        if room(area, SourceRoomRef::own(area_id, anchor, number)).is_none() {
            continue;
        }
        let target = SourceRoomRef::data_on_source(area_id, place, anchor, number);
        let present = room(area, target).is_some_and(|data| data.has_tag(&tag));
        if present != add && !targets.contains(&target) {
            targets.push(target);
        }
    }
    if targets.is_empty() {
        return None;
    }
    let description = if add {
        format!("Add tag {tag} to {place_name}")
    } else {
        format!("Remove tag {tag} from {place_name}")
    };
    let operations = |add: bool| -> Vec<AreaMutation> {
        targets
            .iter()
            .map(|target| tag_change(*target, tag.clone(), add))
            .collect()
    };
    let batches = |operations: Vec<AreaMutation>| -> Vec<Mutation> {
        if !place.is_map() {
            return vec![batch(area_id, place, operations, description.clone())];
        }
        // The map's envelopes hold a bounded number of operations each.
        operations
            .chunks(smudgy_cloud::MAX_MUTATION_OPERATIONS)
            .map(|chunk| Mutation::AreaBatch {
                area_id,
                operations: chunk.to_vec(),
                description: description.clone(),
            })
            .collect()
    };
    Some(TagChange {
        command: Command::new(batches(operations(add)), batches(operations(!add))),
        changed: targets.len(),
    })
}

/// The selection's source rooms, grouped by source.
fn by_source(selection: &Selection) -> BTreeMap<SourceId, Vec<RoomNumber>> {
    let mut grouped: BTreeMap<SourceId, Vec<RoomNumber>> = BTreeMap::new();
    for (source, number) in selection.source_rooms() {
        grouped.entry(source).or_default().push(number);
    }
    grouped
}

/// One field change to each of the selection's source rooms, one batch per
/// source: `change` gives a room's forward and undo updates.
fn update_rooms(
    area: &AreaCache,
    selection: &Selection,
    noun: &str,
    change: impl Fn(&RoomCache) -> (RoomUpdates, RoomUpdates),
) -> (Vec<Mutation>, Vec<Mutation>) {
    let (mut redo, mut undo) = (Vec::new(), Vec::new());
    for (source, numbers) in by_source(selection) {
        let (mut forward, mut back) = (Vec::new(), Vec::new());
        for number in numbers {
            let Some(room) = sources::source_room(area, source, number) else {
                continue;
            };
            let target = SourceRoomRef::own(*area.get_id(), source, number);
            let (to, from) = change(room);
            forward.push(upsert(target, to));
            back.push(upsert(target, from));
        }
        if !forward.is_empty() {
            let description = format!("{noun} {} rooms", forward.len());
            redo.push(batch(*area.get_id(), source, forward, description.clone()));
            undo.push(batch(*area.get_id(), source, back, description));
        }
    }
    (redo, undo)
}

/// The level shift of the selection's source rooms, one batch per source.
pub fn shift_rooms_level(
    area: &AreaCache,
    selection: &Selection,
    delta: i32,
) -> (Vec<Mutation>, Vec<Mutation>) {
    update_rooms(area, selection, "Shift", |room| {
        (
            RoomUpdates {
                level: Some(room.get_level() + delta),
                ..RoomUpdates::default()
            },
            RoomUpdates {
                level: Some(room.get_level()),
                ..RoomUpdates::default()
            },
        )
    })
}

/// The moves of the selection's source rooms, one batch per source.
pub fn move_rooms(
    area: &AreaCache,
    selection: &Selection,
    offset: Vector,
) -> (Vec<Mutation>, Vec<Mutation>) {
    update_rooms(area, selection, "Move", |room| {
        (
            RoomUpdates {
                x: Some(room.get_x() + offset.x),
                y: Some(room.get_y() + offset.y),
                ..RoomUpdates::default()
            },
            RoomUpdates {
                x: Some(room.get_x()),
                y: Some(room.get_y()),
                ..RoomUpdates::default()
            },
        )
    })
}

/// The selection's labels and shapes a source holds, grouped by source:
/// each source's deletes, and the creates that bring them back under
/// their own ids.
pub(super) fn drawings_by_source(
    area: &AreaCache,
    selection: &Selection,
) -> BTreeMap<SourceId, (Vec<AreaMutation>, Vec<AreaMutation>)> {
    let mut grouped: BTreeMap<SourceId, (Vec<AreaMutation>, Vec<AreaMutation>)> = BTreeMap::new();
    for label_id in selection.labels() {
        let Some((Some(layer), label)) = area.find_label(&label_id) else {
            continue;
        };
        let (deletes, restores) = grouped.entry(layer.source()).or_default();
        deletes.push(AreaMutation::DeleteLabel { label_id });
        restores.push(AreaMutation::CreateLabel {
            body: LabelArgs {
                id: Some(label_id),
                level: label.level,
                x: label.x,
                y: label.y,
                width: label.width,
                height: label.height,
                horizontal_alignment: label.horizontal_alignment.clone(),
                vertical_alignment: label.vertical_alignment.clone(),
                text: label.text.clone(),
                color: label.color.clone(),
                background_color: Some(label.background_color.clone()),
                font_size: label.font_size,
                font_weight: label.font_weight,
            },
        });
    }
    for shape_id in selection.shapes() {
        let Some((Some(layer), shape)) = area.find_shape(&shape_id) else {
            continue;
        };
        let (deletes, restores) = grouped.entry(layer.source()).or_default();
        deletes.push(AreaMutation::DeleteShape { shape_id });
        restores.push(AreaMutation::CreateShape {
            body: ShapeArgs {
                id: Some(shape_id),
                level: shape.level,
                x: shape.x,
                y: shape.y,
                width: shape.width,
                height: shape.height,
                background_color: Some(shape.background_color.clone().unwrap_or_default()),
                stroke_color: Some(shape.stroke_color.clone().unwrap_or_default()),
                shape_type: shape.shape_type.clone(),
                border_radius: shape.border_radius,
                stroke_width: Some(shape.stroke_width),
            },
        });
    }
    grouped
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;
    use smudgy_cloud::mutation::{MutationEnvelope, MutationResult};
    use smudgy_cloud::{
        Area, AreaUpdates, AreaWithDetails, CloudResult, CreateAreaRequest, ExitDirection, Mapper,
        MapperBackend, Uuid,
    };
    use smudgy_map_widget::map_editor::EntityId;

    const AREA: &str = "123e4567-e89b-12d3-a456-426614174000";
    const SECRET: &str = "6f1c2a9e-0b7d-4e1a-9c3f-2d8e5b4a7c10";
    const LABEL: &str = "00000000-0000-4000-8000-00000000aa01";

    /// Serves one map with one Secret, and accepts every write.
    struct OneMap(AreaWithDetails);

    #[async_trait]
    impl MapperBackend for OneMap {
        async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
            unreachable!("the test creates no maps")
        }
        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            Ok(vec![self.0.area.clone()])
        }
        async fn get_area(&self, _area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            Ok(self.0.clone())
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
            envelope: &MutationEnvelope,
        ) -> CloudResult<MutationResult> {
            // The tests read the optimistic cache; the backend acknowledges.
            Ok(MutationResult {
                operation_id: envelope.operation_id,
                versions: vec![smudgy_cloud::mutation::VersionInfo::map_source(
                    area_id.0, 100,
                )],
                data: Vec::new(),
            })
        }
    }

    const E1: &str = "00000000-0000-4000-8000-000000000001";
    const E2: &str = "00000000-0000-4000-8000-000000000002";
    const E3: &str = "00000000-0000-4000-8000-000000000003";
    const E4: &str = "00000000-0000-4000-8000-000000000004";
    const C1: &str = "00000000-0000-4000-8000-0000000000c1";
    const C2: &str = "00000000-0000-4000-8000-0000000000c2";

    fn exit_id(id: &str) -> smudgy_cloud::ExitId {
        smudgy_cloud::ExitId(Uuid::parse_str(id).expect("an exit id"))
    }

    fn connection_id(id: &str) -> smudgy_cloud::ConnectionId {
        smudgy_cloud::ConnectionId(Uuid::parse_str(id).expect("a link id"))
    }

    fn room(number: i32, x: f32, exits: serde_json::Value) -> serde_json::Value {
        json!({
            "room_number": number, "title": format!("room {number}"), "description": "",
            "color": "", "level": 0, "x": x, "y": 0.0, "properties": [], "exits": exits,
            "tags": []
        })
    }

    fn exit(id: &str, direction: &str, to: i32, own: bool, connection: &str) -> serde_json::Value {
        let mut exit = json!({
            "id": id, "from_direction": direction, "to_area_id": AREA, "to_room_number": to,
            "to_direction": null, "to_unknown": false, "path": "", "command": "", "weight": 1.0,
            "connection_id": connection, "is_hidden": false, "door": null
        });
        if own {
            exit["to_source"] = json!(SECRET);
        }
        exit
    }

    fn connection(id: &str, a: (i32, bool), b: (i32, bool)) -> serde_json::Value {
        let end = |(number, own): (i32, bool), side: &str| {
            let mut end = json!({
                "room_number": number, "side": side, "port_offset": 0.5, "port_mode": "AutoPinned"
            });
            if own {
                end["source"] = json!(SECRET);
            }
            end
        };
        json!({
            "id": id, "endpoint_a": end(a, "East"), "endpoint_b": end(b, "West"),
            "kind": "Internal", "routing": "Simple", "segment_shape": "Direct", "corner": "Sharp",
            "route_points": [], "dash": "Solid", "color": "#A4A4A4", "thickness": 1.0
        })
    }

    /// Map rooms 1 and 2. The Secret's own rooms 2 and 3 are linked both
    /// ways by a routed, dashed link; behind a dotted hidden door, its room
    /// 2 and the map's room 2 lead to each other; it keeps a note and a tag
    /// on the map's room 2, and a label.
    pub(in super::super) async fn loaded() -> (Mapper, AreaId, SourceId) {
        loaded_with(Vec::new()).await
    }

    /// [`loaded`], with `others` served beside the Secret.
    async fn loaded_with(others: Vec<serde_json::Value>) -> (Mapper, AreaId, SourceId) {
        let mut routed = connection(C1, (2, true), (3, true));
        routed["routing"] = json!("Manual");
        routed["route_points"] = json!([{ "x": 3.5, "y": 0.5 }]);
        routed["dash"] = json!("Dashed");
        let mut door = connection(C2, (2, false), (2, true));
        door["dash"] = json!("Dotted");
        let details: AreaWithDetails = serde_json::from_value(json!({
            "id": AREA,
            "user_id": null,
            "atlas_id": null,
            "name": "Library",
            "created_at": "2026-10-05T00:00:00Z",
            "format_version": smudgy_cloud::AREA_FORMAT_VERSION,
            "properties": [],
            "rooms": [room(1, 0.0, json!([])), room(2, 1.0, json!([]))],
            "labels": [],
            "shapes": [],
            "sources": [{
                "source": SECRET, "name": "Hidden", "ownership": "owner", "rev": 2,
                "actions": ["read", "add", "edit", "remove"],
                "rooms": [
                    room(2, 3.0, json!([
                        exit(E1, "East", 3, true, C1),
                        exit(E2, "West", 2, false, C2),
                    ])),
                    room(3, 4.0, json!([exit(E3, "West", 2, true, C1)])),
                ],
                "room_data": [{
                    "room_number": 2,
                    "properties": [{ "name": "notes", "value": "behind the shelf" }],
                    "tags": ["DOOR"],
                    "exits": [exit(E4, "East", 2, true, C2)]
                }],
                "connections": [routed, door],
                "labels": [{
                    "id": LABEL, "level": 0, "x": 3.0, "y": -1.0, "width": 1.0, "height": 0.3,
                    "horizontal_alignment": "Left", "vertical_alignment": "Top",
                    "text": "Pull the red book", "color": "#ffffff", "background_color": "",
                    "font_size": 12, "font_weight": 400
                }]
            }]
        }))
        .expect("a map with a Secret");
        let mut details = details;
        details.sources.extend(
            others
                .into_iter()
                .map(|bundle| serde_json::from_value(bundle).expect("another place")),
        );
        let area_id = details.area.id;
        let dir = std::env::temp_dir().join(format!("smudgy-source-rooms-{}", Uuid::new_v4()));
        let mapper = Mapper::new(Arc::new(OneMap(details)), dir);
        mapper.load_all_areas().await.expect("loads");
        (mapper, area_id, SECRET.parse().expect("a Secret id"))
    }

    /// The Secret's content as the map serves it.
    fn bundle(mapper: &Mapper, area_id: AreaId, secret: SourceId) -> smudgy_cloud::SourceBundle {
        mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("loaded")
            .meta()
            .sources
            .iter()
            .find(|bundle| bundle.source == secret)
            .expect("the Secret")
            .clone()
    }

    /// Every exit the bundle keeps, wherever it is kept, by id.
    fn bundle_exit(bundle: &smudgy_cloud::SourceBundle, id: &str) -> Option<smudgy_cloud::Exit> {
        bundle
            .rooms
            .iter()
            .flat_map(|room| room.exits.iter())
            .chain(bundle.room_data.iter().flat_map(|data| data.exits.iter()))
            .find(|exit| exit.id == exit_id(id))
            .cloned()
    }

    /// Applies `command`, checks it did, and returns the stack holding it.
    fn apply(mapper: &Mapper, command: Command) -> super::super::commands::CommandStack {
        let mut stack = super::super::commands::CommandStack::default();
        let _ = stack.push_and_apply(mapper, command);
        assert_eq!(stack.take_last_error(), None);
        assert!(stack.can_undo(), "the command applied");
        stack
    }

    fn undo(mapper: &Mapper, stack: &mut super::super::commands::CommandStack) {
        let _ = stack.undo(mapper);
        assert_eq!(stack.take_last_error(), None);
        assert!(stack.can_redo(), "the undo applied");
    }

    fn only_batch(mutations: &[Mutation]) -> (SourceId, &[AreaMutation]) {
        match mutations {
            [
                Mutation::SourceBatch {
                    source, operations, ..
                },
            ] => (*source, operations.as_slice()),
            other => panic!("expected one source batch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn moving_a_secret_room_writes_its_secret() {
        let (mapper, area_id, secret) = loaded().await;
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("loaded");
        let selection: Selection = [EntityId::SourceRoom(secret, RoomNumber(2))]
            .into_iter()
            .collect();

        let (redo, undo) = move_rooms(&area, &selection, Vector::new(1.0, 0.5));
        let (source, operations) = only_batch(&redo);
        assert_eq!(source, secret);
        assert!(
            matches!(
                operations,
                [AreaMutation::UpsertRoom {
                    room_number: RoomNumber(2),
                    room_source: Some(named),
                    body: RoomUpdates { x: Some(x), y: Some(y), .. },
                }] if *named == secret && (*x - 4.0).abs() < f32::EPSILON && (*y - 0.5).abs() < f32::EPSILON
            ),
            "{operations:?}"
        );
        let (_, back) = only_batch(&undo);
        assert!(matches!(
            back,
            [AreaMutation::UpsertRoom { body: RoomUpdates { x: Some(x), .. }, .. }]
                if (*x - 3.0).abs() < f32::EPSILON
        ));
    }

    #[tokio::test]
    async fn undoing_a_secret_room_delete_restores_its_links_by_id_with_route_and_dash() {
        let (mapper, area_id, secret) = loaded().await;
        let selection: Selection = [EntityId::SourceRoom(secret, RoomNumber(2))]
            .into_iter()
            .collect();
        let command = super::super::commands::delete_selection(
            &mapper.get_current_atlas(),
            area_id,
            &selection,
        )
        .expect("a delete");
        let (source, deletes) = only_batch(command.redo_mutations());
        assert_eq!(source, secret);
        assert!(matches!(
            deletes,
            [AreaMutation::DeleteRoom { room_number: RoomNumber(2), room_source: Some(named) }]
                if *named == secret
        ));

        let mut stack = apply(&mapper, command);
        let deleted = bundle(&mapper, area_id, secret);
        assert!(
            deleted
                .rooms
                .iter()
                .all(|room| room.room_number != RoomNumber(2))
        );
        assert_eq!(
            bundle_exit(&deleted, E3).map(|exit| exit.to_room_number),
            Some(None),
            "the way into the deleted room leads nowhere"
        );

        undo(&mapper, &mut stack);
        let restored = bundle(&mapper, area_id, secret);
        let room = restored
            .rooms
            .iter()
            .find(|room| room.room_number == RoomNumber(2))
            .expect("the room is back");
        assert_eq!(room.title, "room 2");
        let routed = restored
            .connections
            .iter()
            .find(|connection| connection.id == connection_id(C1))
            .expect("the link is back by its id");
        assert_eq!(
            routed.route_points,
            vec![smudgy_cloud::MapPoint::new(3.5, 0.5)],
            "with its route"
        );
        assert_eq!(
            routed.dash,
            smudgy_cloud::ConnectionDash::Dashed,
            "and its dash"
        );
        let inbound = bundle_exit(&restored, E3).expect("the way in");
        assert_eq!(
            (
                inbound.to_room_number,
                inbound.to_source,
                inbound.connection_id
            ),
            (Some(RoomNumber(2)), Some(secret), connection_id(C1)),
            "the way in leads to the room again, on its link"
        );
        for (id, link) in [(E1, C1), (E2, C2), (E4, C2)] {
            assert_eq!(
                bundle_exit(&restored, id).map(|exit| exit.connection_id),
                Some(connection_id(link)),
                "exit {id} returns under its id"
            );
        }
        let door = restored
            .connections
            .iter()
            .find(|connection| connection.id == connection_id(C2))
            .expect("the hidden door is back by its id");
        assert_eq!(door.dash, smudgy_cloud::ConnectionDash::Dotted);
    }

    #[tokio::test]
    async fn undoing_a_map_room_delete_restores_each_secrets_data_and_hidden_exits_on_it() {
        let (mapper, area_id, secret) = loaded().await;
        let selection: Selection = [EntityId::Room(RoomNumber(2))].into_iter().collect();
        let command = super::super::commands::delete_selection(
            &mapper.get_current_atlas(),
            area_id,
            &selection,
        )
        .expect("a delete");
        assert!(
            !command
                .redo_mutations()
                .iter()
                .any(|mutation| matches!(mutation, Mutation::SourceBatch { .. })),
            "the server takes the Secret's data with the room"
        );
        assert!(
            matches!(
                command.undo_mutations(),
                [Mutation::AreaBatch { .. }, Mutation::SourceBatch { source, .. }] if *source == secret
            ),
            "undo restores the map first, then the Secret: {:?}",
            command.undo_mutations()
        );

        let mut stack = apply(&mapper, command);
        let deleted = bundle(&mapper, area_id, secret);
        assert!(
            deleted.room_data.is_empty(),
            "the Secret's data went with the room"
        );
        assert_eq!(
            bundle_exit(&deleted, E2).map(|exit| exit.to_room_number),
            Some(None)
        );

        undo(&mapper, &mut stack);
        assert!(
            mapper
                .get_current_atlas()
                .get_area(&area_id)
                .and_then(|area| area.get_room(&RoomNumber(2)).cloned())
                .is_some(),
            "the map room is back"
        );
        let restored = bundle(&mapper, area_id, secret);
        let data = restored
            .room_data
            .iter()
            .find(|data| data.room_number == RoomNumber(2))
            .expect("the Secret's data on the room is back");
        assert_eq!(data.properties[0].value, "behind the shelf");
        assert!(data.tags.contains("DOOR"));
        let hidden = bundle_exit(&restored, E4).expect("the hidden exit is back");
        assert_eq!(
            (
                hidden.to_room_number,
                hidden.to_source,
                hidden.connection_id
            ),
            (Some(RoomNumber(2)), Some(secret), connection_id(C2))
        );
        let back = bundle_exit(&restored, E2).expect("the way back");
        assert_eq!(
            (back.to_room_number, back.to_source),
            (Some(RoomNumber(2)), None),
            "the Secret's way to the map room leads there again"
        );
        let door = restored
            .connections
            .iter()
            .find(|connection| connection.id == connection_id(C2))
            .expect("the door's link is back by its id");
        assert_eq!(door.dash, smudgy_cloud::ConnectionDash::Dotted);
    }

    #[tokio::test]
    async fn a_secret_label_deletes_in_its_secret_and_returns_under_its_id() {
        let (mapper, area_id, secret) = loaded().await;
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("loaded");
        let label = smudgy_cloud::LabelId(Uuid::parse_str(LABEL).expect("a label id"));
        assert_eq!(
            area.find_label(&label)
                .map(|(layer, _)| SourceLayer::source_of(layer)),
            Some(secret)
        );
        assert!(area.get_label(&label).is_none(), "the map does not hold it");
        let selection: Selection = [EntityId::Label(label)].into_iter().collect();

        let command = super::super::commands::delete_selection(&atlas, area_id, &selection)
            .expect("a delete");
        let (source, deletes) = only_batch(command.redo_mutations());
        assert_eq!(source, secret);
        assert!(matches!(deletes, [AreaMutation::DeleteLabel { label_id }] if *label_id == label));
        let (_, restores) = only_batch(command.undo_mutations());
        assert!(matches!(
            restores,
            [AreaMutation::CreateLabel { body }] if body.id == Some(label) && body.text == "Pull the red book"
        ));
    }

    #[tokio::test]
    async fn a_secret_link_is_edited_trimmed_and_deleted_in_its_secret() {
        use super::super::commands::{
            FieldId, delete_connection, delete_waypoint, edit_connection,
        };
        let (mapper, area_id, secret) = loaded().await;
        let link = connection_id(C1);

        let command = edit_connection(
            &mapper.get_current_atlas(),
            area_id,
            link,
            FieldId::DashStyle,
            smudgy_cloud::ConnectionUpdates {
                dash: Some(smudgy_cloud::ConnectionDash::Dotted),
                ..Default::default()
            },
            "Change connection dash",
        )
        .expect("a Secret's link is editable");
        assert_eq!(only_batch(command.redo_mutations()).0, secret);
        let mut stack = apply(&mapper, command);
        let dash = |mapper: &Mapper| {
            bundle(mapper, area_id, secret)
                .connections
                .iter()
                .find(|connection| connection.id == link)
                .map(|connection| connection.dash)
        };
        assert_eq!(dash(&mapper), Some(smudgy_cloud::ConnectionDash::Dotted));
        undo(&mapper, &mut stack);
        assert_eq!(dash(&mapper), Some(smudgy_cloud::ConnectionDash::Dashed));

        let command = delete_waypoint(&mapper.get_current_atlas(), area_id, link, 0)
            .expect("its route point can go");
        let _stack = apply(&mapper, command);
        assert!(
            bundle(&mapper, area_id, secret)
                .connections
                .iter()
                .any(|connection| connection.id == link && connection.route_points.is_empty())
        );

        let command =
            delete_connection(&mapper.get_current_atlas(), area_id, link).expect("a delete");
        let (_, restores) = only_batch(command.undo_mutations());
        assert!(
            matches!(restores.first(), Some(AreaMutation::CreateConnection { body }) if body.id == link)
        );
        let mut stack = apply(&mapper, command);
        assert!(bundle_exit(&bundle(&mapper, area_id, secret), E1).is_none());
        undo(&mapper, &mut stack);
        let restored = bundle(&mapper, area_id, secret);
        assert!(
            restored
                .connections
                .iter()
                .any(|connection| connection.id == link)
        );
        assert!(bundle_exit(&restored, E1).is_some() && bundle_exit(&restored, E3).is_some());
    }

    #[tokio::test]
    async fn a_secret_rooms_links_are_removed_and_edited_in_its_secret() {
        use super::super::commands::{ExitRef, FieldId, edit_exit_field};
        let (mapper, area_id, secret) = loaded().await;
        let exit = ExitRef {
            area_id,
            place: secret,
            room: smudgy_cloud::RoomAddress::new(secret, RoomNumber(3)),
            id: exit_id(E3),
        };

        // A removed link returns whole, in its Secret.
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("loaded");
        let view = super::super::links::link_view(&atlas, &area, connection_id(C1), None)
            .expect("the Secret's link");
        let command =
            super::super::link_commands::remove(&atlas, area_id, &view).expect("a removal");
        let (source, _) = only_batch(command.redo_mutations());
        assert_eq!(source, secret);
        let mut stack = apply(&mapper, command);
        assert!(bundle_exit(&bundle(&mapper, area_id, secret), E3).is_none());
        undo(&mapper, &mut stack);
        let restored = bundle(&mapper, area_id, secret);
        assert_eq!(
            bundle_exit(&restored, E3).map(|exit| exit.connection_id),
            Some(connection_id(C1)),
            "the exit returns on its link"
        );
        assert!(
            restored.connections.iter().any(|connection| {
                connection.id == connection_id(C1)
                    && connection.dash == smudgy_cloud::ConnectionDash::Dashed
                    && !connection.route_points.is_empty()
            }),
            "the link keeps its dash and route"
        );

        // The destination picker names both source and number. Changing
        // only a number must not infer another source from room existence.
        let command = edit_exit_field(
            &mapper.get_current_atlas(),
            exit,
            FieldId::Destination,
            |updates| {
                updates.to_room_number = Some(RoomNumber(1));
                updates.to_source = Some(None);
            },
        )
        .expect("an exit edit");
        let (source, operations) = only_batch(command.redo_mutations());
        assert_eq!(source, secret);
        assert!(
            matches!(
                operations,
                [AreaMutation::UpdateExit { body, .. }]
                    if body.to_room_number == Some(RoomNumber(1)) && body.to_source == Some(None)
            ),
            "{operations:?}"
        );
        let mut stack = apply(&mapper, command);
        let moved = bundle_exit(&bundle(&mapper, area_id, secret), E3).expect("the exit");
        assert_eq!(
            (moved.to_room_number, moved.to_source),
            (Some(RoomNumber(1)), None)
        );
        undo(&mapper, &mut stack);
        let back = bundle_exit(&bundle(&mapper, area_id, secret), E3).expect("the exit");
        assert_eq!(
            (back.to_room_number, back.to_source),
            (Some(RoomNumber(2)), Some(secret))
        );
    }

    #[tokio::test]
    async fn a_link_tool_link_goes_where_its_place_is() {
        use super::super::commands::{NewExitTarget, NewLink, NewLinkOptions, create_link};
        use smudgy_map_widget::map_editor::PlacedRoom;
        let (mapper, area_id, secret) = loaded().await;

        // From a map room to empty canvas while "Add to" is the Secret: the
        // new room and the link are the Secret's, never the map's.
        let link = NewLink {
            area_id,
            place: secret,
            from: PlacedRoom::map(RoomNumber(1)),
            from_direction: ExitDirection::South,
            to: NewExitTarget::NewRoom {
                room_number: RoomNumber(9),
                at: iced::Point::new(0.0, 3.0),
                level: 0,
            },
            to_direction: ExitDirection::North,
        };
        let (command, link_id) = create_link(&link, NewLinkOptions::default()).expect("a link");
        let (source, operations) = only_batch(command.redo_mutations());
        assert_eq!(source, secret);
        assert!(matches!(
            operations.first(),
            Some(AreaMutation::CreateRoom { room_number: RoomNumber(9), room_source: Some(named), .. })
                if *named == secret
        ));
        let _stack = apply(&mapper, command);
        let area = mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("loaded");
        assert!(
            area.get_room(&RoomNumber(9)).is_none(),
            "the map gains no room"
        );
        assert!(area.get_connection(link_id).is_none(), "nor a link");
        assert!(sources::source_room(&area, secret, RoomNumber(9)).is_some());
        let hidden = bundle(&mapper, area_id, secret);
        assert!(
            hidden
                .connections
                .iter()
                .any(|connection| connection.id == link_id)
        );
        assert!(
            hidden
                .room_data
                .iter()
                .any(|data| data.room_number == RoomNumber(1) && !data.exits.is_empty()),
            "the way out of the map room is the Secret's hidden exit"
        );

        // Between map rooms, it is the Secret's hidden passage.
        let passage = NewLink {
            to: NewExitTarget::Room(PlacedRoom::map(RoomNumber(2))),
            from_direction: ExitDirection::East,
            to_direction: ExitDirection::West,
            ..link
        };
        let (command, passage_id) =
            create_link(&passage, NewLinkOptions::default()).expect("a passage");
        let _stack = apply(&mapper, command);
        let area = mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("loaded");
        assert!(area.get_connection(passage_id).is_none());
        assert!(
            area.find_connection(passage_id)
                .is_some_and(|(layer, _)| { SourceLayer::source_of(layer) == secret })
        );

        // A room of another place is no end for this one.
        let stranger: SourceId = "00000000-0000-4000-8000-0000000000ff"
            .parse()
            .expect("an id");
        let across = NewLink {
            to: NewExitTarget::Room(PlacedRoom {
                source: stranger,
                number: RoomNumber(5),
            }),
            ..passage
        };
        assert!(create_link(&across, NewLinkOptions::default()).is_none());
    }

    #[tokio::test]
    async fn a_map_room_is_not_a_secret_room() {
        let (mapper, area_id, secret) = loaded().await;
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("loaded");
        let target = SourceRoomRef::own(area_id, secret, RoomNumber(1));
        assert!(
            set_property(&atlas, target, "notes".into(), "x".into()).is_none(),
            "the Secret has no room 1 of its own"
        );
        assert!(sources::source_room(&area, secret, RoomNumber(3)).is_some());
    }

    #[tokio::test]
    async fn a_new_room_in_a_secret_is_written_there_and_undone_there() {
        let (_mapper, area_id, secret) = loaded().await;

        let command = create_room(
            area_id,
            secret,
            RoomNumber(4),
            iced::Point::new(5.0, 1.0),
            0,
        );
        let (source, operations) = only_batch(command.redo_mutations());
        assert_eq!(source, secret);
        assert!(matches!(
            operations,
            [AreaMutation::CreateRoom { room_number: RoomNumber(4), room_source: Some(s), body }]
                if *s == secret && body.x == Some(5.0) && body.level == Some(0)
        ));
    }

    /// The map holds rooms 1 and 2 and the Secret its own rooms 2 and 3: each
    /// place's next room comes from its own rooms alone, Private additions
    /// not yet written start at 1, and a room placed in one place moves no
    /// other place's numbering.
    #[tokio::test]
    async fn each_place_numbers_the_rooms_placed_in_it() {
        let (mapper, area_id, secret) = loaded().await;
        let next = |place| new_room_number(&mapper, area_id, place).expect("a number");
        assert_eq!(next(SourceId::Map), RoomNumber(3));
        assert_eq!(next(secret), RoomNumber(4));
        assert_eq!(next(SourceId::Private), RoomNumber(1));

        apply(
            &mapper,
            create_room(area_id, secret, next(secret), iced::Point::new(5.0, 1.0), 0),
        );
        assert!(
            sources::source_room(
                &mapper.get_current_atlas().get_area(&area_id).unwrap(),
                secret,
                RoomNumber(4)
            )
            .is_some()
        );
        assert_eq!(next(secret), RoomNumber(5));
        assert_eq!(next(SourceId::Map), RoomNumber(3), "the map's own rooms");
    }

    #[tokio::test]
    async fn map_owned_note_on_a_secret_room_edits_and_undoes_in_its_own_source() {
        let (mapper, area_id, secret) = loaded().await;
        let before = mapper.get_current_atlas().get_area(&area_id).unwrap();
        let target = SourceRoomRef::data_on_source(area_id, SourceId::Map, secret, RoomNumber(2));
        let command = set_property(
            &mapper.get_current_atlas(),
            target,
            "notes".into(),
            "Map note".into(),
        )
        .unwrap();
        let mut stack = apply(&mapper, command);
        let after = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(after.room_count(), before.room_count());
        assert!(
            after
                .get_room(&RoomNumber(2))
                .unwrap()
                .get_property("notes")
                .is_none()
        );
        assert_eq!(after.meta().sources, before.meta().sources);
        assert_eq!(after.meta().room_data[0].room_source, Some(secret));
        assert_eq!(after.meta().room_data[0].properties[0].value, "Map note");
        undo(&mapper, &mut stack);
        let restored = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert!(restored.meta().room_data.is_empty());
        assert_eq!(restored.room_count(), before.room_count());
        assert!(
            restored
                .get_room(&RoomNumber(2))
                .unwrap()
                .get_property("notes")
                .is_none()
        );
        assert_eq!(restored.meta().sources, before.meta().sources);
    }

    #[tokio::test]
    async fn a_new_label_in_private_carries_the_id_undo_deletes() {
        let (_mapper, area_id, _secret) = loaded().await;

        let (command, label_id) = create_label(area_id, SourceId::Private, LabelArgs::default());
        let (source, operations) = only_batch(command.redo_mutations());
        assert_eq!(source, SourceId::Private);
        assert!(matches!(
            operations,
            [AreaMutation::CreateLabel { body }] if body.id == Some(label_id)
        ));
    }

    #[tokio::test]
    async fn adding_follows_each_places_own_actions() {
        use super::super::secrets::{can_add, can_write};
        let (mapper, area_id, secret) = loaded().await;
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("loaded");

        assert!(can_add(&area, secret), "the Secret grants add");
        assert!(
            can_add(&area, SourceId::Private),
            "Private needs only read access"
        );
        let stranger: SourceId = "00000000-0000-4000-8000-0000000000ff"
            .parse()
            .expect("an id");
        assert!(!can_add(&area, stranger), "an unknown Secret takes nothing");
        assert!(!can_write(&area, stranger));
    }

    #[tokio::test]
    async fn data_on_a_map_room_names_no_room_source() {
        let (mapper, area_id, secret) = loaded().await;
        let atlas = mapper.get_current_atlas();
        // The Secret keeps nothing for map room 1 yet.
        let target = SourceRoomRef::data_on(area_id, secret, RoomNumber(1));

        let command = set_property(&atlas, target, "notes".into(), "push the red book".into())
            .expect("a first property starts the room's data");
        let (source, operations) = only_batch(command.redo_mutations());
        assert_eq!(source, secret);
        assert!(matches!(
            operations,
            [AreaMutation::UpsertRoomProperty {
                room_number: RoomNumber(1),
                room_source: None,
                ..
            }]
        ));
        let (_, undo) = only_batch(command.undo_mutations());
        assert!(matches!(
            undo,
            [AreaMutation::DeleteRoomProperty {
                room_number: RoomNumber(1),
                room_source: None,
                ..
            }]
        ));

        let area = atlas.get_area(&area_id).expect("loaded");
        let tag = change_tags(
            &area,
            secret,
            &[(SourceId::Map, RoomNumber(1))],
            "secret door",
            true,
            "Hidden",
        )
        .expect("a new tag");
        assert!(matches!(
            only_batch(tag.command.redo_mutations()).1,
            [AreaMutation::AddRoomTag {
                room_source: None,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn a_tag_change_writes_one_place_and_skips_rooms_it_would_not_change() {
        let (mapper, area_id, secret) = loaded().await;
        let area = mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("loaded");

        // The Secret already tags map room 2 DOOR: adding DOOR to map rooms
        // 1 and 2 and its own room 3 writes rooms 1 and 3, in the Secret.
        let change = change_tags(
            &area,
            secret,
            &[
                (SourceId::Map, RoomNumber(1)),
                (SourceId::Map, RoomNumber(2)),
                (secret, RoomNumber(3)),
            ],
            "door",
            true,
            "Hidden",
        )
        .expect("two rooms lack it");
        assert_eq!(change.changed, 2);
        let (source, operations) = only_batch(change.command.redo_mutations());
        assert_eq!(source, secret);
        assert!(matches!(
            operations,
            [
                AreaMutation::AddRoomTag {
                    room_number: RoomNumber(1),
                    room_source: None,
                    ..
                },
                AreaMutation::AddRoomTag {
                    room_number: RoomNumber(3),
                    room_source: Some(_),
                    ..
                },
            ]
        ));
        let (_, undo_operations) = only_batch(change.command.undo_mutations());
        assert!(
            undo_operations
                .iter()
                .all(|operation| matches!(operation, AreaMutation::RemoveRoomTag { .. }))
        );

        // Removing it from the Secret leaves the map's tags alone, and the
        // map's own copy is removed from the map alone.
        let removal = change_tags(
            &area,
            secret,
            &[
                (SourceId::Map, RoomNumber(1)),
                (SourceId::Map, RoomNumber(2)),
            ],
            "DOOR",
            false,
            "Hidden",
        )
        .expect("room 2 has it");
        assert_eq!(removal.changed, 1);
        assert!(
            change_tags(
                &area,
                SourceId::Map,
                &[(SourceId::Map, RoomNumber(2))],
                "door",
                false,
                "Map"
            )
            .is_none(),
            "the map itself never had DOOR"
        );

        // The map's tags remain Map-owned even on a Secret room.
        let map = change_tags(
            &area,
            SourceId::Map,
            &[
                (SourceId::Map, RoomNumber(1)),
                (SourceId::Map, RoomNumber(2)),
                (secret, RoomNumber(3)),
            ],
            "peace",
            true,
            "Map",
        )
        .expect("both map rooms lack it");
        assert_eq!(map.changed, 3);
        assert!(matches!(
            map.command.redo_mutations(),
            [Mutation::AreaBatch { operations, .. }] if operations.len() == 3
        ));

        // Applied and undone, it lands where it was written.
        let mut stack = apply(&mapper, change.command);
        let tagged = |number: i32| {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&area_id).expect("loaded");
            let layer = layer(&area, secret).expect("the Secret's layer").clone();
            (
                area.get_room(&RoomNumber(number))
                    .is_some_and(|room| room.has_tag("DOOR")),
                layer
                    .map_room_data(RoomNumber(number))
                    .is_some_and(|data| data.has_tag("DOOR")),
            )
        };
        assert_eq!(tagged(1), (false, true), "in the Secret, not the map");
        undo(&mapper, &mut stack);
        assert_eq!(tagged(1), (false, false));
    }

    #[tokio::test]
    async fn retained_tags_keep_their_owner_anchor_and_undo_without_aliasing_room_numbers() {
        use super::super::inspector::Message;
        use super::super::tags::{SelectionTags, TagIndex};

        let (mapper, area_id, secret) = loaded_with(vec![json!({
            "source": WARDROBE, "name": "Wardrobe", "ownership": "owner", "rev": 1,
            "actions": ["read", "add", "edit", "remove"], "rooms": []
        })])
        .await;
        let wardrobe: SourceId = WARDROBE.parse().unwrap();
        let area = || mapper.get_current_atlas().get_area(&area_id).unwrap();
        for owner in [SourceId::Map, wardrobe, SourceId::Private] {
            let change = change_tags(
                &area(),
                owner,
                &[(secret, RoomNumber(2))],
                "retained",
                true,
                "Owner",
            )
            .expect("a readable room can carry another source's tag");
            let mut stack = apply(&mapper, change.command);
            let snapshot = area();
            assert!(
                !snapshot
                    .get_room(&RoomNumber(2))
                    .unwrap()
                    .has_tag("RETAINED")
            );
            assert!(
                !sources::source_room(&snapshot, secret, RoomNumber(2))
                    .unwrap()
                    .has_tag("RETAINED")
            );
            let selected = SelectionTags::read(&snapshot, [], [(secret, RoomNumber(2))]);
            assert_eq!(
                selected
                    .in_place(owner)
                    .and_then(|tags| tags.get("RETAINED")),
                Some(&1)
            );
            assert_eq!(
                TagIndex::build(&snapshot, true).rooms_with("RETAINED"),
                [(secret, RoomNumber(2))]
            );
            assert!(
                TagIndex::build(&snapshot, false)
                    .rooms_with("RETAINED")
                    .is_empty()
            );
            assert!(
                SelectionTags::read(&snapshot, [RoomNumber(2)], [])
                    .in_place(owner)
                    .is_none_or(|tags| !tags.contains_key("RETAINED"))
            );
            undo(&mapper, &mut stack);
            let after_undo = SelectionTags::read(&area(), [], [(secret, RoomNumber(2))]);
            assert!(
                after_undo
                    .in_place(owner)
                    .is_none_or(|tags| !tags.contains_key("RETAINED"))
            );
            let _ = stack.redo(&mapper);
            assert_eq!(stack.take_last_error(), None);
        }

        // Removing the Map chip in the actual inspector leaves both other
        // sources' tags on the Secret room, and Map room 2, unchanged.
        let mut window = super::super::test_window(mapper, area_id);
        window
            .editor
            .add_to_selection(EntityId::SourceRoom(secret, RoomNumber(2)));
        let _ = window.update_inspector(Message::TagRemoved(SourceId::Map, "RETAINED".into()));
        let snapshot = window
            .mapper
            .get_current_atlas()
            .get_area(&area_id)
            .unwrap();
        let selected = SelectionTags::read(&snapshot, [], [(secret, RoomNumber(2))]);
        assert!(
            selected
                .in_place(SourceId::Map)
                .is_none_or(|tags| !tags.contains_key("RETAINED"))
        );
        for owner in [wardrobe, SourceId::Private] {
            assert_eq!(
                selected
                    .in_place(owner)
                    .and_then(|tags| tags.get("RETAINED")),
                Some(&1)
            );
        }
    }

    #[tokio::test]
    async fn moving_one_room_reviews_only_the_selected_room() {
        let (mapper, area_id, secret) = loaded().await;
        let mut window = super::super::test_window(mapper, area_id);
        window
            .editor
            .add_to_selection(EntityId::SourceRoom(secret, RoomNumber(2)));
        let _ = window.update_move(super::super::moves::MoveMessage::Requested(SourceId::Map));
        let Some(super::super::modals::Modal::ReviewMove { content, .. }) = &window.modal else {
            panic!("server review")
        };
        assert_eq!(
            content.rooms,
            vec![RoomNumber(2)],
            "room 3 remains in its own source"
        );
    }

    const WARDROBE: &str = "0b7d6f1c-2a9e-4e1a-9c3f-2d8e5b4a7c11";

    /// Another Secret, with its own room 7, and Private additions with their
    /// own room 8, both keeping a way into map room 1.
    fn other_places() -> Vec<serde_json::Value> {
        let into_map = |id: &str| {
            json!({
                "id": id, "from_direction": "North", "to_area_id": AREA, "to_room_number": 1,
                "to_direction": null, "to_unknown": false, "path": "", "command": "",
                "weight": 1.0, "connection_id": id, "is_hidden": true, "door": null
            })
        };
        vec![
            json!({
                "source": WARDROBE, "name": "Wardrobe", "ownership": "owner", "rev": 1,
                "actions": ["read", "add", "edit", "remove"],
                "rooms": [room(7, 6.0, json!([into_map("00000000-0000-4000-8000-0000000000e7")]))],
            }),
            json!({
                "source": "private", "rev": 1, "actions": ["read", "add", "edit", "remove"],
                "rooms": [room(8, 7.0, json!([]))],
            }),
        ]
    }

    #[tokio::test]
    async fn moving_an_annotated_room_uses_the_authoritative_review() {
        let (mapper, area_id, _) = loaded_with(other_places()).await;
        let mut window = super::super::test_window(mapper, area_id);
        window
            .editor
            .add_to_selection(EntityId::Room(RoomNumber(2)));
        let _ = window.update_move(super::super::moves::MoveMessage::Requested(
            SourceId::Private,
        ));
        assert!(matches!(
            &window.modal,
            Some(super::super::modals::Modal::ReviewMove { .. })
        ));
        window.cancel_move_review();
        assert_eq!(
            window.editor.selection().single(),
            Some(EntityId::Room(RoomNumber(2)))
        );
    }
}
