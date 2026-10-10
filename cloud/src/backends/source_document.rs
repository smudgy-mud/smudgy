//! Editing one source without manufacturing rooms for its attachments.
//! Owned rooms and anchored content keep their wire representation. Only the
//! anchor's readable geometry is borrowed from other sources.

use std::collections::{BTreeSet, HashMap};

use crate::{
    AreaId, AreaWithDetails, CloudError, CloudResult, Exit, Property, RoomAddress, RoomData,
    RoomNumber, RoomWithDetails, SourceBundle, SourceId, connection_lifecycle::RoomSite,
    mutation::AreaMutation,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct EditContext {
    pub source: SourceId,
    pub anchors: HashMap<RoomAddress, RoomSite>,
    pub unreadable_connections: HashMap<crate::ConnectionId, crate::Connection>,
}

impl EditContext {
    pub fn owned(&self, address: RoomAddress) -> CloudResult<RoomNumber> {
        if address.source != self.source {
            return Err(CloudError::InvalidInput(
                "a source cannot change another source's room fields".into(),
            ));
        }
        Ok(address.number)
    }

    pub fn site(&self, document: &AreaWithDetails, address: RoomAddress) -> Option<RoomSite> {
        if address.source == self.source {
            document
                .rooms
                .iter()
                .find(|r| r.room_number == address.number)
                .map(site)
        } else {
            self.anchors.get(&address).copied()
        }
    }

    pub fn require_anchor(&self, address: RoomAddress) -> CloudResult<()> {
        if address.source == self.source || self.anchors.contains_key(&address) {
            Ok(())
        } else {
            Err(CloudError::InvalidInput(
                "referenced_room_unavailable".into(),
            ))
        }
    }

    pub fn validate_destination(
        &self,
        map: AreaId,
        area: Option<AreaId>,
        source: Option<SourceId>,
        number: Option<RoomNumber>,
    ) -> CloudResult<()> {
        if area == Some(map) {
            if let Some(number) = number {
                self.require_anchor(RoomAddress::from_wire(source, number))?;
            }
        } else if let Some(source) = source.filter(|source| !source.is_map())
            && (!matches!(source, SourceId::Secret(_)) || source == self.source || area.is_none())
        {
            return Err(CloudError::InvalidInput(
                "secret_link_into_other_map".into(),
            ));
        }
        Ok(())
    }
}

fn site(room: &RoomWithDetails) -> RoomSite {
    RoomSite {
        x: room.x,
        y: room.y,
        level: room.level,
    }
}

pub(crate) fn room_sites(details: &AreaWithDetails) -> HashMap<RoomAddress, RoomSite> {
    details
        .rooms
        .iter()
        .map(|room| (RoomAddress::map(room.room_number), site(room)))
        .chain(details.sources.iter().flat_map(|bundle| {
            bundle.rooms.iter().map(move |room| {
                (
                    RoomAddress::new(bundle.source, room.room_number),
                    site(room),
                )
            })
        }))
        .collect()
}

/// Mutable content belongs to the writing source even when its anchor does not.
pub(crate) struct RoomContentMut<'a> {
    pub properties: &'a mut Vec<Property>,
    pub tags: &'a mut BTreeSet<String>,
    pub exits: &'a mut Vec<Exit>,
}

pub(crate) fn room_content_mut<'a>(
    document: &'a mut AreaWithDetails,
    context: &EditContext,
    address: RoomAddress,
) -> CloudResult<RoomContentMut<'a>> {
    if address.source == context.source {
        let room = document
            .rooms
            .iter_mut()
            .find(|r| r.room_number == address.number)
            .ok_or_else(|| {
                CloudError::RoomNotFound(crate::mapper::RoomKey::new(
                    document.area.id,
                    address.number,
                ))
            })?;
        return Ok(RoomContentMut {
            properties: &mut room.properties,
            tags: &mut room.tags,
            exits: &mut room.exits,
        });
    }
    context.require_anchor(address)?;
    let index = document
        .room_data
        .iter()
        .position(|data| room_data_address(data) == address)
        .unwrap_or_else(|| {
            document.room_data.push(RoomData {
                room_number: address.number,
                room_source: address.wire_source(),
                properties: Vec::new(),
                tags: BTreeSet::new(),
                exits: Vec::new(),
            });
            document.room_data.len() - 1
        });
    let data = &mut document.room_data[index];
    Ok(RoomContentMut {
        properties: &mut data.properties,
        tags: &mut data.tags,
        exits: &mut data.exits,
    })
}

pub(crate) fn room_data_address(data: &RoomData) -> RoomAddress {
    RoomAddress::from_wire(data.room_source, data.room_number)
}

pub(crate) fn exits(
    document: &AreaWithDetails,
    source: SourceId,
) -> impl Iterator<Item = (RoomAddress, &Exit)> {
    document
        .rooms
        .iter()
        .flat_map(move |room| {
            room.exits
                .iter()
                .map(move |exit| (RoomAddress::new(source, room.room_number), exit))
        })
        .chain(document.room_data.iter().flat_map(|data| {
            data.exits
                .iter()
                .map(move |exit| (room_data_address(data), exit))
        }))
}

pub(crate) fn exits_mut(document: &mut AreaWithDetails) -> impl Iterator<Item = &mut Exit> {
    document
        .rooms
        .iter_mut()
        .flat_map(|room| &mut room.exits)
        .chain(
            document
                .room_data
                .iter_mut()
                .flat_map(|data| &mut data.exits),
        )
}

pub(crate) struct SourceDocument {
    pub content: AreaWithDetails,
    pub context: EditContext,
}

impl SourceDocument {
    pub fn open(details: &AreaWithDetails, source: SourceId) -> CloudResult<Self> {
        let bundle = if source.is_map() {
            SourceBundle {
                properties: details.properties.clone(),
                rooms: details.rooms.clone(),
                room_data: details.room_data.clone(),
                labels: details.labels.clone(),
                shapes: details.shapes.clone(),
                connections: details.connections.clone(),
                ..empty_bundle(source)
            }
        } else {
            match details
                .sources
                .iter()
                .find(|bundle| bundle.source == source)
            {
                Some(bundle) => bundle.clone(),
                None if source == SourceId::Private => empty_bundle(source),
                None => return Err(CloudError::InvalidInput("secret_unavailable".into())),
            }
        };
        let mut anchors = room_sites(details);
        anchors.retain(|address, _| address.source != source);
        let unreadable_connections = bundle
            .connections
            .iter()
            .filter(|connection| {
                std::iter::once(connection.endpoint_a)
                    .chain(connection.endpoint_b)
                    .any(|end| {
                        end.address().source != source && !anchors.contains_key(&end.address())
                    })
            })
            .map(|connection| (connection.id, connection.clone()))
            .collect();
        Ok(Self {
            context: EditContext {
                source,
                anchors,
                unreadable_connections,
            },
            content: AreaWithDetails {
                area: details.area.clone(),
                format_version: details.format_version,
                properties: bundle.properties,
                rooms: bundle.rooms,
                room_data: bundle.room_data,
                labels: bundle.labels,
                shapes: bundle.shapes,
                connections: bundle.connections,
                linked_areas: Vec::new(),
                sources: Vec::new(),
            },
        })
    }

    pub fn close(mut self, details: &mut AreaWithDetails) {
        self.content.room_data.retain(|data| {
            !(data.properties.is_empty() && data.tags.is_empty() && data.exits.is_empty())
        });
        self.content.room_data.sort_by_key(room_data_address);
        self.content.rooms.sort_by_key(|room| room.room_number);
        let source = self.context.source;
        if source.is_map() {
            let kept: std::collections::HashSet<_> =
                self.content.rooms.iter().map(|r| r.room_number).collect();
            let removed: Vec<_> = details
                .rooms
                .iter()
                .map(|r| r.room_number)
                .filter(|n| !kept.contains(n))
                .collect();
            for number in removed {
                forget_room(details, RoomAddress::map(number));
            }
            details.properties = self.content.properties;
            details.rooms = self.content.rooms;
            details.room_data = self.content.room_data;
            details.labels = self.content.labels;
            details.shapes = self.content.shapes;
            details.connections = self.content.connections;
        } else {
            let index = details
                .sources
                .iter()
                .position(|bundle| bundle.source == source)
                .unwrap_or_else(|| {
                    details.sources.push(empty_bundle(source));
                    details.sources.len() - 1
                });
            let bundle = &mut details.sources[index];
            bundle.properties = self.content.properties;
            bundle.rooms = self.content.rooms;
            bundle.room_data = self.content.room_data;
            bundle.labels = self.content.labels;
            bundle.shapes = self.content.shapes;
            bundle.connections = self.content.connections;
        }
    }
}

pub(crate) fn empty_bundle(source: SourceId) -> SourceBundle {
    SourceBundle {
        source,
        name: None,
        ownership: None,
        clan_id: None,
        color: None,
        rev: 0,
        actions: ["read", "add", "edit", "remove"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        properties: Vec::new(),
        rooms: Vec::new(),
        room_data: Vec::new(),
        labels: Vec::new(),
        shapes: Vec::new(),
        connections: Vec::new(),
    }
}

pub(crate) fn forget_room(details: &mut AreaWithDetails, room: RoomAddress) {
    let sources: Vec<_> = details
        .sources
        .iter()
        .map(|bundle| bundle.source)
        .filter(|source| *source != room.source)
        .collect();
    for source in sources {
        let Ok(mut document) = SourceDocument::open(details, source) else {
            continue;
        };
        super::area_edits::delete_room_in(&mut document.content, room, &document.context);
        document.close(details);
    }
}

pub(crate) fn apply_source_ops(
    details: &mut AreaWithDetails,
    source: SourceId,
    ops: &[AreaMutation],
) -> CloudResult<()> {
    let mut document = SourceDocument::open(details, source)?;
    for op in ops {
        super::area_edits::apply_mutation_in(&mut document.content, op, &document.context)?;
    }
    document.close(details);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RoomUpdates, format3::AreaProjection};
    use serde_json::json;

    fn fixture() -> AreaWithDetails {
        let projection: AreaProjection = serde_json::from_value(json!({
            "format_version": 3,
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "name": "Qualified rooms", "rev": 1,
            "created_at": "2026-10-08T00:00:00Z",
            "sources": [{ "source": "map", "rev": 1, "actions": ["read", "edit"],
                "rooms": [{ "room_number": 1, "title": "Map room", "description": "",
                    "color": "", "level": 0, "x": 3.0, "y": 4.0,
                    "properties": [], "tags": [], "exits": [] }]
            }, { "source": "private", "rev": 1, "actions": ["read", "add", "edit", "remove"],
                "room_data": [{ "room_number": 1, "properties": [{ "name": "note", "value": "attached" }] }]
            }]
        })).unwrap();
        AreaWithDetails::try_from(projection).unwrap()
    }

    #[test]
    fn every_legal_room_number_remains_available_beside_attachments() {
        for create in [false, true] {
            let mut details = fixture();
            for number in [i32::MIN, i32::MAX, 0, 1] {
                let body = RoomUpdates {
                    title: Some(format!("Private {number}")),
                    ..RoomUpdates::default()
                };
                let op = if create {
                    AreaMutation::CreateRoom {
                        room_number: RoomNumber(number),
                        room_source: Some(SourceId::Private),
                        body,
                    }
                } else {
                    AreaMutation::UpsertRoom {
                        room_number: RoomNumber(number),
                        room_source: Some(SourceId::Private),
                        body,
                    }
                };
                apply_source_ops(&mut details, SourceId::Private, &[op]).unwrap();
            }
            assert_eq!(details.sources[0].rooms.len(), 4);
            assert_eq!(
                details.sources[0].room_data[0].properties[0].value,
                "attached"
            );
            assert_eq!(details.rooms[0].title, "Map room");
        }
    }

    #[test]
    fn attachment_edits_and_geometry_edits_cannot_confuse_equal_numbered_rooms() {
        let mut details = fixture();
        apply_source_ops(
            &mut details,
            SourceId::Private,
            &[
                AreaMutation::CreateRoom {
                    room_number: RoomNumber(1),
                    room_source: Some(SourceId::Private),
                    body: RoomUpdates {
                        x: Some(9.0),
                        ..RoomUpdates::default()
                    },
                },
                AreaMutation::UpsertRoomProperty {
                    room_number: RoomNumber(1),
                    room_source: None,
                    name: "note".into(),
                    value: "map anchor".into(),
                },
                AreaMutation::UpsertRoomProperty {
                    room_number: RoomNumber(1),
                    room_source: Some(SourceId::Private),
                    name: "note".into(),
                    value: "own room".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(details.sources[0].rooms[0].properties[0].value, "own room");
        assert_eq!(
            details.sources[0].room_data[0].properties[0].value,
            "map anchor"
        );
        let before = serde_json::to_value(&details).unwrap();
        let denied = apply_source_ops(
            &mut details,
            SourceId::Private,
            &[AreaMutation::UpsertRoom {
                room_number: RoomNumber(1),
                room_source: None,
                body: RoomUpdates {
                    x: Some(99.0),
                    ..RoomUpdates::default()
                },
            }],
        );
        assert!(denied.is_err());
        assert_eq!(serde_json::to_value(details).unwrap(), before);
    }

    #[test]
    fn opening_a_source_keeps_unreadable_attachments_without_inventing_geometry() {
        let mut details = fixture();
        details.rooms.clear();
        let before = serde_json::to_value(&details.sources[0]).unwrap();
        let document = SourceDocument::open(&details, SourceId::Private).unwrap();
        assert!(document.content.rooms.is_empty());
        assert!(
            document
                .context
                .site(&document.content, RoomAddress::map(RoomNumber(1)))
                .is_none()
        );
        document.close(&mut details);
        assert_eq!(serde_json::to_value(&details.sources[0]).unwrap(), before);
    }
}

pub(crate) fn mentions_map_room(
    bundle: &SourceBundle,
    area_id: AreaId,
    map_room: RoomNumber,
) -> bool {
    let into = |exit: &Exit| {
        exit.to_area_id == Some(area_id)
            && exit.to_source.unwrap_or_default().is_map()
            && exit.to_room_number == Some(map_room)
    };
    bundle.room_data.iter().any(|data| {
        (data.room_source.unwrap_or(SourceId::Map).is_map() && data.room_number == map_room)
            || data.exits.iter().any(into)
    }) || bundle.rooms.iter().any(|room| room.exits.iter().any(into))
        || bundle.connections.iter().any(|connection| {
            std::iter::once(connection.endpoint_a)
                .chain(connection.endpoint_b)
                .any(|endpoint| {
                    endpoint.source.unwrap_or_default().is_map() && endpoint.room_number == map_room
                })
        })
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::{
        ConnectionEndpoint, ConnectionUpdates, ExitArgs, ExitDirection, ExitUpdates, RoomUpdates,
        format3::AreaProjection,
    };
    use serde_json::json;
    use uuid::Uuid;

    const AREA: &str = "123e4567-e89b-12d3-a456-426614174000";
    const SECRET: &str = "6f1c2a9e-0b7d-4e1a-9c3f-2d8e5b4a7c10";

    fn secret() -> SourceId {
        SECRET.parse().expect("a Secret id")
    }

    fn area() -> AreaId {
        AreaId(Uuid::parse_str(AREA).expect("an area id"))
    }

    fn room(number: i32, x: f32) -> serde_json::Value {
        json!({
            "room_number": number, "title": format!("room {number}"), "description": "",
            "color": "", "level": 0, "x": x, "y": 0.0, "properties": [], "exits": [], "tags": []
        })
    }

    /// A map with rooms 1 and 2, and `secret_bundle` beside it.
    fn document(secret_bundle: serde_json::Value) -> AreaWithDetails {
        let projection: AreaProjection = serde_json::from_value(json!({
            "format_version": 3,
            "id": AREA,
            "user_id": "123e4567-e89b-12d3-a456-426614174001",
            "atlas_id": null,
            "name": "Library",
            "created_at": "2026-10-04T00:00:00Z",
            "access": {
                "is_owner": true, "can_edit": true, "can_reshare": true,
                "can_copy": true, "can_admin": true, "include_secrets": true
            },
            "projection_token": "p_abc",
            "linked_areas": [],
            "sources": [
                {
                    "source": "map", "rev": 3, "actions": ["read", "add", "edit", "remove"],
                    "properties": [], "rooms": [room(1, 0.0), room(2, 1.0)],
                    "labels": [], "shapes": [], "connections": []
                },
                secret_bundle,
            ],
        }))
        .expect("projection parses");
        AreaWithDetails::try_from(projection).expect("converts")
    }

    fn empty_secret() -> serde_json::Value {
        json!({
            "source": SECRET, "name": "Hidden", "ownership": "owner", "rev": 1,
            "actions": ["read", "add", "edit", "remove"], "properties": [], "rooms": [],
            "room_data": [], "labels": [], "shapes": [], "connections": []
        })
    }

    fn bundle(details: &AreaWithDetails) -> &SourceBundle {
        details
            .sources
            .iter()
            .find(|bundle| bundle.source == secret())
            .expect("the Secret's bundle")
    }

    fn own(number: i32) -> (RoomNumber, Option<SourceId>) {
        (RoomNumber(number), Some(secret()))
    }

    fn exit(
        direction: ExitDirection,
        to: (RoomNumber, Option<SourceId>),
        back: ExitDirection,
    ) -> ExitArgs {
        ExitArgs {
            from_direction: direction,
            to_area_id: Some(area()),
            to_room_number: Some(to.0),
            to_source: to.1,
            to_direction: Some(back),
            weight: 1.0,
            ..ExitArgs::default()
        }
    }

    #[test]
    fn a_secret_keeps_its_own_rooms_apart_from_its_data_for_map_rooms() {
        let mut details = document(empty_secret());
        apply_source_ops(
            &mut details,
            secret(),
            &[
                AreaMutation::CreateRoom {
                    room_number: RoomNumber(1),
                    room_source: Some(secret()),
                    body: RoomUpdates {
                        title: Some("Hidden study".into()),
                        ..RoomUpdates::default()
                    },
                },
                AreaMutation::UpsertRoomProperty {
                    room_number: RoomNumber(1),
                    room_source: None,
                    name: "notes".into(),
                    value: "the Secret's, on the map's room".into(),
                },
                AreaMutation::UpsertRoomProperty {
                    room_number: RoomNumber(1),
                    room_source: Some(secret()),
                    name: "notes".into(),
                    value: "its own room's".into(),
                },
            ],
        )
        .expect("applies");

        assert!(
            details.rooms[0].properties.is_empty(),
            "the map is untouched"
        );
        let hidden = bundle(&details);
        assert_eq!(hidden.rooms.len(), 1);
        assert_eq!(hidden.rooms[0].title, "Hidden study");
        assert_eq!(hidden.rooms[0].properties[0].value, "its own room's");
        assert_eq!(hidden.room_data.len(), 1);
        assert_eq!(hidden.room_data[0].room_number, RoomNumber(1));
        assert_eq!(
            hidden.room_data[0].properties[0].value,
            "the Secret's, on the map's room"
        );
    }

    #[test]
    fn a_door_between_a_map_room_and_a_secret_room_pairs_into_one_connection() {
        let mut details = document(empty_secret());
        apply_source_ops(
            &mut details,
            secret(),
            &[
                AreaMutation::CreateRoom {
                    room_number: RoomNumber(2),
                    room_source: Some(secret()),
                    body: RoomUpdates {
                        x: Some(4.0),
                        ..RoomUpdates::default()
                    },
                },
                AreaMutation::CreateExit {
                    room_number: RoomNumber(2),
                    room_source: None,
                    body: exit(ExitDirection::East, own(2), ExitDirection::West),
                },
                AreaMutation::CreateExit {
                    room_number: RoomNumber(2),
                    room_source: Some(secret()),
                    body: exit(
                        ExitDirection::West,
                        (RoomNumber(2), None),
                        ExitDirection::East,
                    ),
                },
            ],
        )
        .expect("applies");

        let hidden = bundle(&details);
        let into = &hidden.room_data[0].exits[0];
        assert_eq!(hidden.room_data[0].room_number, RoomNumber(2));
        assert_eq!(
            (into.to_room_number, into.to_source),
            (Some(RoomNumber(2)), Some(secret()))
        );
        let back = &hidden.rooms[0].exits[0];
        assert_eq!(
            (back.to_room_number, back.to_source),
            (Some(RoomNumber(2)), None)
        );
        assert_eq!(hidden.connections.len(), 1, "the two exits pair");
        let connection = &hidden.connections[0];
        assert_eq!(into.connection_id, connection.id);
        assert_eq!(back.connection_id, connection.id);
        let mut ends: Vec<_> = std::iter::once(connection.endpoint_a)
            .chain(connection.endpoint_b)
            .map(|endpoint| (endpoint.room_number, endpoint.source))
            .collect();
        ends.sort();
        assert_eq!(
            ends,
            vec![(RoomNumber(2), None), (RoomNumber(2), Some(secret()))]
        );
        assert!(
            details.rooms[1].exits.is_empty(),
            "the map's own room 2 has no exit of its own"
        );
    }

    #[test]
    fn a_source_cannot_change_a_map_rooms_own_fields() {
        let mut details = document(empty_secret());
        let refused = apply_source_ops(
            &mut details,
            secret(),
            &[AreaMutation::UpsertRoom {
                room_number: RoomNumber(1),
                room_source: None,
                body: RoomUpdates::default(),
            }],
        );
        assert!(
            matches!(refused, Err(CloudError::InvalidInput(_))),
            "{refused:?}"
        );
    }

    #[test]
    fn qualified_operations_apply_without_number_translation() {
        let details = document(empty_secret());
        let ops = vec![
            AreaMutation::CreateRoom {
                room_number: RoomNumber(7),
                room_source: Some(secret()),
                body: RoomUpdates::default(),
            },
            AreaMutation::AddRoomTag {
                room_number: RoomNumber(1),
                room_source: None,
                tag: "DOOR".into(),
            },
            AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                room_source: None,
                body: exit(ExitDirection::North, own(7), ExitDirection::South),
            },
            AreaMutation::CreateExit {
                room_number: RoomNumber(7),
                room_source: Some(secret()),
                body: exit(
                    ExitDirection::South,
                    (RoomNumber(1), None),
                    ExitDirection::North,
                ),
            },
        ];
        let mut changed = details.clone();
        apply_source_ops(&mut changed, secret(), &ops).unwrap();
        let owned = &bundle(&changed).rooms[0];
        assert_eq!(owned.room_number, RoomNumber(7));
        assert_eq!(owned.exits[0].to_room_number, Some(RoomNumber(1)));
        assert_eq!(
            bundle(&changed).room_data[0].exits[0].to_source,
            Some(secret())
        );
    }

    /// A Secret with its own room 2 behind a door on map room 1: a note and
    /// a tag on map room 1, an exit from it into room 2 and one back, paired
    /// into one connection.
    fn door() -> serde_json::Value {
        json!({
            "source": SECRET, "name": "Hidden", "ownership": "owner", "rev": 4,
            "actions": ["read", "add", "edit", "remove"],
            "properties": [{ "name": "zone", "value": "hidden" }],
            "rooms": [{
                "room_number": 2, "title": "Vault", "description": "", "color": "",
                "level": 0, "x": 2.0, "y": 0.0, "properties": [], "tags": [],
                "exits": [{
                    "id": "00000000-0000-4000-8000-000000000002", "from_direction": "West",
                    "to_area_id": AREA, "to_room_number": 1, "to_direction": "East",
                    "to_unknown": false, "path": "", "command": "", "weight": 1.0,
                    "connection_id": "00000000-0000-4000-8000-0000000000c1",
                    "is_hidden": false, "door": null
                }]
            }],
            "room_data": [{
                "room_number": 1,
                "properties": [{ "name": "notes", "value": "behind the shelf" }],
                "tags": ["DOOR"],
                "exits": [{
                    "id": "00000000-0000-4000-8000-000000000001", "from_direction": "East",
                    "to_area_id": AREA, "to_room_number": 2, "to_source": SECRET,
                    "to_direction": "West", "to_unknown": false, "path": "", "command": "",
                    "weight": 1.0, "connection_id": "00000000-0000-4000-8000-0000000000c1",
                    "is_hidden": false, "door": null
                }]
            }],
            "labels": [], "shapes": [],
            "connections": [{
                "id": "00000000-0000-4000-8000-0000000000c1",
                "endpoint_a": {
                    "room_number": 1, "side": "East", "port_offset": 0.5, "port_mode": "AutoPinned"
                },
                "endpoint_b": {
                    "room_number": 2, "source": SECRET, "side": "West",
                    "port_offset": 0.5, "port_mode": "AutoPinned"
                },
                "kind": "Internal", "routing": "Simple", "segment_shape": "Direct",
                "corner": "Sharp", "route_points": [], "dash": "Solid",
                "color": "#A4A4A4", "thickness": 1.0
            }]
        })
    }

    #[test]
    fn map_owned_data_on_a_secret_room_survives_the_document_round_trip() {
        let mut details = document(door());
        let mut retained = bundle(&details).room_data[0].clone();
        retained.room_source = Some(secret());
        retained.room_number = RoomNumber(2);
        retained.exits.clear();
        details.room_data.push(retained);
        details.connections = bundle(&details).connections.clone();
        let before = details.clone();
        let opened = SourceDocument::open(&details, SourceId::Map).unwrap();
        assert_eq!(opened.content.rooms, details.rooms);
        assert_eq!(opened.content.room_data, details.room_data);
        opened.close(&mut details);
        assert_eq!(
            serde_json::to_value(&details).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
    }

    #[test]
    fn editing_map_owned_data_on_a_secret_room_does_not_edit_same_numbered_rooms() {
        let mut details = document(door());
        let ordinary = details.rooms.clone();
        let sources = details.sources.clone();
        apply_source_ops(
            &mut details,
            SourceId::Map,
            &[
                AreaMutation::UpsertRoomProperty {
                    room_number: RoomNumber(2),
                    room_source: Some(secret()),
                    name: "notes".into(),
                    value: "Kept on the map".into(),
                },
                AreaMutation::AddRoomTag {
                    room_number: RoomNumber(2),
                    room_source: Some(secret()),
                    tag: "MAP NOTE".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(details.rooms, ordinary);
        assert_eq!(details.sources, sources);
        assert_eq!(details.room_data.len(), 1);
        let data = &details.room_data[0];
        assert_eq!(
            (data.room_source, data.room_number),
            (Some(secret()), RoomNumber(2))
        );
        assert_eq!(data.properties[0].value, "Kept on the map");
        assert!(data.tags.contains("MAP NOTE"));

        let before = details.clone();
        let failed = apply_source_ops(
            &mut details,
            SourceId::Map,
            &[AreaMutation::UpsertRoom {
                room_number: RoomNumber(2),
                room_source: Some(secret()),
                body: RoomUpdates {
                    title: Some("Cannot change the Secret's room".into()),
                    ..RoomUpdates::default()
                },
            }],
        );
        assert!(failed.is_err());
        assert_eq!(
            serde_json::to_value(&details).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
    }

    #[test]
    fn a_served_bundle_survives_opening_and_closing() {
        let mut details = document(door());
        let before = serde_json::to_value(bundle(&details)).unwrap();
        let opened = SourceDocument::open(&details, secret()).expect("opens");
        assert_eq!(opened.content.rooms.len(), 1);
        assert_eq!(opened.content.room_data.len(), 1);
        opened.close(&mut details);
        assert_eq!(serde_json::to_value(bundle(&details)).unwrap(), before);
    }

    #[test]
    fn deleting_a_map_room_drops_a_secrets_data_and_door_on_it() {
        let mut details = document(door());
        super::super::area_edits::apply_mutation(
            &mut details,
            &AreaMutation::DeleteRoom {
                room_number: RoomNumber(1),
                room_source: None,
            },
        )
        .expect("the map room deletes");

        let hidden = bundle(&details);
        assert!(
            hidden.room_data.is_empty(),
            "the note, the tag and the hidden exit go with the room"
        );
        let back = &hidden.rooms[0].exits[0];
        assert_eq!(
            (back.to_area_id, back.to_room_number, back.to_source),
            (None, None, None),
            "the way back dangles"
        );
        assert_eq!(hidden.connections.len(), 1, "its last member keeps it");
        let connection = &hidden.connections[0];
        assert_eq!(
            (
                connection.endpoint_a.room_number,
                connection.endpoint_a.source
            ),
            (RoomNumber(2), Some(secret()))
        );
        assert!(connection.endpoint_b.is_none());

        let opened = SourceDocument::open(&details, secret()).expect("opens");
        assert!(opened.content.room_data.is_empty());
        assert_eq!(opened.content.rooms.len(), 1, "only the Secret's own room");
    }

    #[test]
    fn deleting_an_unmentioned_map_room_leaves_a_secret_alone() {
        let mut details = document(door());
        let before = serde_json::to_value(bundle(&details)).unwrap();
        super::super::area_edits::apply_mutation(
            &mut details,
            &AreaMutation::DeleteRoom {
                room_number: RoomNumber(2),
                room_source: None,
            },
        )
        .expect("the map room deletes");
        assert_eq!(serde_json::to_value(bundle(&details)).unwrap(), before);
    }

    /// A pending write to a Secret the projection no longer carries never
    /// brings it back; Private additions still open before their first
    /// write.
    #[test]
    fn a_secret_the_projection_lost_takes_no_write() {
        let mut details = document(empty_secret());
        details.sources.clear();
        let note = |source: Option<SourceId>| AreaMutation::UpsertRoomProperty {
            room_number: RoomNumber(1),
            room_source: source,
            name: "notes".to_string(),
            value: "still here?".to_string(),
        };
        let refused = apply_source_ops(&mut details, secret(), &[note(None)]);
        assert!(
            matches!(&refused, Err(CloudError::InvalidInput(code)) if code == "secret_unavailable"),
            "{refused:?}"
        );
        assert!(details.sources.is_empty(), "no bundle was made up");
        apply_source_ops(&mut details, SourceId::Private, &[note(None)])
            .expect("Private additions start with their first write");
        assert_eq!(details.sources.len(), 1);
    }

    /// A source's write names only its own rooms and the map's: a link to
    /// another source's room is refused, whichever way it is named.
    #[test]
    fn a_secret_cannot_reference_an_unavailable_source_room() {
        let other = SourceId::Secret(Uuid::new_v4());
        let endpoint = |source: Option<SourceId>| ConnectionEndpoint {
            room_number: RoomNumber(1),
            source,
            side: crate::RoomSide::East,
            port_offset: 0.5,
            port_mode: crate::PortMode::AutoPinned,
        };
        let refusals = [
            AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                room_source: None,
                body: exit(
                    ExitDirection::East,
                    (RoomNumber(2), Some(other)),
                    ExitDirection::West,
                ),
            },
            AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                room_source: Some(other),
                body: exit(
                    ExitDirection::East,
                    (RoomNumber(2), None),
                    ExitDirection::West,
                ),
            },
            AreaMutation::UpdateExit {
                exit_id: crate::ExitId(Uuid::new_v4()),
                body: ExitUpdates {
                    to_source: Some(Some(other)),
                    ..ExitUpdates::default()
                },
            },
            AreaMutation::UpdateConnection {
                connection_id: crate::ConnectionId(Uuid::new_v4()),
                body: ConnectionUpdates {
                    endpoint_b: Some(endpoint(Some(other))),
                    ..ConnectionUpdates::default()
                },
            },
        ];
        for op in refusals {
            let mut details = document(empty_secret());
            let refused = apply_source_ops(&mut details, secret(), std::slice::from_ref(&op));
            assert!(refused.is_err(), "{op:?}: {refused:?}");
        }

        // An own room is a room of this map; another map has none of them.
        let mut details = document(empty_secret());
        let elsewhere = ExitArgs {
            to_area_id: Some(AreaId(Uuid::new_v4())),
            ..exit(ExitDirection::East, own(5), ExitDirection::West)
        };
        let refused = apply_source_ops(
            &mut details,
            secret(),
            &[AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                room_source: None,
                body: elsewhere,
            }],
        );
        assert!(
            matches!(&refused, Err(CloudError::InvalidInput(code)) if code == "secret_link_into_other_map"),
            "{refused:?}"
        );
    }

    #[test]
    fn attachments_to_equal_numbered_rooms_keep_their_source_and_edit_only_the_attachment() {
        let mut details = document(empty_secret());
        let other = SourceId::Secret(Uuid::new_v4());
        let mut second = empty_bundle(other);
        second
            .rooms
            .push(serde_json::from_value(room(1, 9.0)).unwrap());
        details.sources.push(second.clone());
        let operations = vec![
            AreaMutation::UpsertRoomProperty {
                room_number: RoomNumber(1),
                room_source: None,
                name: "notes".into(),
                value: "map attachment".into(),
            },
            AreaMutation::UpsertRoomProperty {
                room_number: RoomNumber(1),
                room_source: Some(other),
                name: "notes".into(),
                value: "Secret attachment".into(),
            },
        ];
        apply_source_ops(&mut details, secret(), &operations).unwrap();
        let attached = &bundle(&details).room_data;
        assert_eq!(attached.len(), 2);
        assert_eq!(attached[0].room_source, None);
        assert_eq!(attached[1].room_source, Some(other));
        assert_eq!(
            details.sources[1], second,
            "the other source's room is unchanged"
        );
        let invalid = AreaMutation::UpsertRoom {
            room_number: RoomNumber(1),
            room_source: Some(other),
            body: RoomUpdates {
                title: Some("forbidden".into()),
                ..RoomUpdates::default()
            },
        };
        assert!(apply_source_ops(&mut details, secret(), &[invalid]).is_err());
        let opened = SourceDocument::open(&details, secret()).unwrap();
        assert_eq!(
            opened.context.anchors[&RoomAddress::new(other, RoomNumber(1))].x,
            9.0
        );
        assert!(opened.content.rooms.is_empty());
    }
    #[test]
    fn equal_numbered_endpoints_are_not_a_self_loop_and_keep_their_sources() {
        let mut hidden = door();
        hidden["rooms"][0]["room_number"] = json!(1);
        hidden["room_data"][0]["exits"][0]["to_room_number"] = json!(1);
        hidden["connections"][0]["endpoint_b"]["room_number"] = json!(1);
        let details = document(hidden);
        let mut opened = SourceDocument::open(&details, secret()).unwrap();
        super::super::area_edits::validate_connection_graph_in(
            &mut opened.content,
            &opened.context,
        )
        .unwrap();
        assert_eq!(
            opened.content.connections[0].kind,
            crate::ConnectionKind::Internal
        );
        let operations = [AreaMutation::UpsertRoom {
            room_number: RoomNumber(1),
            room_source: Some(secret()),
            body: RoomUpdates {
                x: Some(12.0),
                ..RoomUpdates::default()
            },
        }];
        let before = super::super::area_edits::capture_room_moves_in(
            &opened.content,
            &operations,
            &opened.context,
        );
        super::super::area_edits::apply_mutation_in(
            &mut opened.content,
            &operations[0],
            &opened.context,
        )
        .unwrap();
        super::super::area_edits::maintain_routes_after_room_moves_in(
            &before,
            &mut opened.content,
            &opened.context,
        );
        super::super::area_edits::validate_connection_graph_in(
            &mut opened.content,
            &opened.context,
        )
        .unwrap();
        assert_eq!(
            opened.content.connections[0].endpoint_a.address(),
            RoomAddress::map(RoomNumber(1))
        );
        assert_eq!(
            opened.content.connections[0].endpoint_b.unwrap().address(),
            RoomAddress::new(secret(), RoomNumber(1))
        );
        assert_eq!(
            opened.context.anchors[&RoomAddress::map(RoomNumber(1))].x,
            0.0
        );
        assert_eq!(opened.content.rooms[0].x, 12.0);
    }

    #[test]
    fn an_unreadable_anchor_does_not_block_unrelated_edits_or_invent_geometry() {
        let mut details = document(door());
        details
            .rooms
            .retain(|room| room.room_number != RoomNumber(1));
        let stored = bundle(&details).clone();
        let mut opened = SourceDocument::open(&details, secret()).unwrap();
        super::super::area_edits::apply_mutation_in(
            &mut opened.content,
            &AreaMutation::UpsertAreaProperty {
                name: "notes".into(),
                value: "an unrelated edit".into(),
            },
            &opened.context,
        )
        .unwrap();
        super::super::area_edits::validate_connection_graph_in(
            &mut opened.content,
            &opened.context,
        )
        .unwrap();
        opened.close(&mut details);
        assert_eq!(bundle(&details).room_data, stored.room_data);
        assert_eq!(bundle(&details).connections, stored.connections);
        assert_eq!(bundle(&details).rooms, stored.rooms);
    }
}
