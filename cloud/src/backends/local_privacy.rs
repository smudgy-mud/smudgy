//! Preserve the privacy marks of pre-format-3 local maps and JSON imports.
//! Private content belongs to this device's user, and to the uploader if the
//! map is later copied to the cloud. It never becomes ordinary shared content.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Deserialize;
use uuid::Uuid;

use crate::{AreaWithDetails, ConnectionId, RoomData, RoomNumber, SourceBundle, SourceId};

pub(crate) fn qualify_legacy_exits(
    details: &mut AreaWithDetails,
    aliases: &HashSet<(crate::AreaId, RoomNumber)>,
) {
    for exit in details
        .rooms
        .iter_mut()
        .flat_map(|room| &mut room.exits)
        .chain(
            details
                .room_data
                .iter_mut()
                .flat_map(|data| &mut data.exits),
        )
        .chain(details.sources.iter_mut().flat_map(|source| {
            source
                .rooms
                .iter_mut()
                .flat_map(|room| &mut room.exits)
                .chain(source.room_data.iter_mut().flat_map(|data| &mut data.exits))
        }))
    {
        if exit.to_source.is_none_or(|source| source.is_map())
            && exit
                .to_area_id
                .zip(exit.to_room_number)
                .is_some_and(|target| aliases.contains(&target))
        {
            exit.to_source = Some(SourceId::Private);
        }
    }
}

#[derive(Default, Deserialize)]
pub(crate) struct LegacyPrivacy {
    #[serde(default)]
    properties: Vec<NamedFlag>,
    #[serde(default)]
    rooms: Vec<RoomFlags>,
    #[serde(default)]
    labels: Vec<IdFlag>,
    #[serde(default)]
    shapes: Vec<IdFlag>,
}

#[derive(Deserialize)]
struct NamedFlag {
    name: String,
    #[serde(default)]
    is_secret: bool,
}

#[derive(Deserialize)]
struct IdFlag {
    id: Uuid,
    #[serde(default)]
    is_secret: bool,
}

#[derive(Deserialize)]
struct RoomFlags {
    room_number: RoomNumber,
    #[serde(default)]
    is_secret: bool,
    #[serde(default)]
    properties: Vec<NamedFlag>,
    #[serde(default)]
    exits: Vec<IdFlag>,
}

fn take_matching<T>(items: &mut Vec<T>, mut take: impl FnMut(&T) -> bool) -> Vec<T> {
    let (taken, kept) = std::mem::take(items)
        .into_iter()
        .partition(|item| take(item));
    *items = kept;
    taken
}

impl LegacyPrivacy {
    pub(crate) fn read(value: &serde_json::Value) -> serde_json::Result<Self> {
        Self::deserialize(value)
    }

    pub(crate) fn private_rooms(&self) -> BTreeSet<RoomNumber> {
        self.rooms
            .iter()
            .filter(|room| room.is_secret)
            .map(|room| room.room_number)
            .collect()
    }

    fn private_links(
        &self,
        details: &AreaWithDetails,
        private_rooms: &BTreeSet<RoomNumber>,
    ) -> HashSet<ConnectionId> {
        let exits: HashSet<_> = self
            .rooms
            .iter()
            .flat_map(|room| &room.exits)
            .filter(|exit| exit.is_secret)
            .map(|exit| exit.id)
            .collect();
        let area_id = details.area.id;
        // Door migration moves these properties into exits before this
        // partition. Their classification must follow the command too.
        let private_doors: HashSet<_> = self
            .rooms
            .iter()
            .flat_map(|room| {
                room.properties
                    .iter()
                    .filter(|property| property.is_secret)
                    .filter_map(|property| {
                        super::local_migration::open_command_direction(&property.name)
                            .map(|direction| (room.room_number, direction))
                    })
            })
            .collect();
        // A Connection and its member exits belong to one source. If either
        // half or either anchor was private, keep the whole link Private.
        details
            .rooms
            .iter()
            .flat_map(|room| {
                let private = private_rooms.contains(&room.room_number);
                let exits = &exits;
                let private_doors = &private_doors;
                room.exits
                    .iter()
                    .filter(move |exit| {
                        private
                            || exits.contains(&exit.id.0)
                            || private_doors.contains(&(room.room_number, exit.from_direction))
                            || (exit.to_area_id == Some(area_id)
                                && exit
                                    .to_room_number
                                    .is_some_and(|number| private_rooms.contains(&number)))
                    })
                    .map(|exit| exit.connection_id)
            })
            .collect()
    }

    fn private_room_properties(&self) -> HashMap<RoomNumber, HashSet<&str>> {
        self.rooms
            .iter()
            .map(|room| {
                (
                    room.room_number,
                    room.properties
                        .iter()
                        .filter(|p| p.is_secret)
                        .map(|p| p.name.as_str())
                        .collect(),
                )
            })
            .collect()
    }

    pub(crate) fn apply(&self, details: &mut AreaWithDetails) {
        let private_rooms = self.private_rooms();
        let links = self.private_links(details, &private_rooms);
        let properties: HashSet<_> = self
            .properties
            .iter()
            .filter(|p| p.is_secret)
            .map(|p| p.name.as_str())
            .collect();
        let labels: HashSet<_> = self
            .labels
            .iter()
            .filter(|p| p.is_secret)
            .map(|p| p.id)
            .collect();
        let shapes: HashSet<_> = self
            .shapes
            .iter()
            .filter(|p| p.is_secret)
            .map(|p| p.id)
            .collect();
        let room_properties = self.private_room_properties();
        if private_rooms.is_empty()
            && links.is_empty()
            && properties.is_empty()
            && labels.is_empty()
            && shapes.is_empty()
            && !self
                .rooms
                .iter()
                .any(|room| room.properties.iter().any(|p| p.is_secret))
        {
            return;
        }
        let mut private = SourceBundle {
            source: SourceId::Private,
            name: None,
            ownership: None,
            clan_id: None,
            color: None,
            rev: details.area.rev,
            actions: ["read", "add", "edit", "remove"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            properties: take_matching(&mut details.properties, |p| {
                properties.contains(p.name.as_str())
            }),
            rooms: Vec::new(),
            room_data: Vec::new(),
            labels: take_matching(&mut details.labels, |p| labels.contains(&p.id.0)),
            shapes: take_matching(&mut details.shapes, |p| shapes.contains(&p.id.0)),
            connections: take_matching(&mut details.connections, |c| links.contains(&c.id)),
        };
        for room in &mut details.rooms {
            for exit in &mut room.exits {
                if exit.to_area_id == Some(details.area.id)
                    && exit
                        .to_room_number
                        .is_some_and(|number| private_rooms.contains(&number))
                {
                    exit.to_source = Some(SourceId::Private);
                }
            }
            if private_rooms.contains(&room.room_number) {
                continue;
            }
            let properties = take_matching(&mut room.properties, |p| {
                room_properties
                    .get(&room.room_number)
                    .is_some_and(|marked| marked.contains(p.name.as_str()))
            });
            let exits = take_matching(&mut room.exits, |exit| links.contains(&exit.connection_id));
            if !properties.is_empty() || !exits.is_empty() {
                private.room_data.push(RoomData {
                    room_number: room.room_number,
                    room_source: None,
                    properties,
                    tags: BTreeSet::new(),
                    exits,
                });
            }
        }
        private.rooms = take_matching(&mut details.rooms, |room| {
            private_rooms.contains(&room.room_number)
        });
        for connection in &mut private.connections {
            for endpoint in
                std::iter::once(&mut connection.endpoint_a).chain(connection.endpoint_b.as_mut())
            {
                endpoint.source = private_rooms
                    .contains(&endpoint.room_number)
                    .then_some(SourceId::Private);
            }
        }
        // Legacy documents have no sources. The loader only applies this
        // conversion to v1/v2, before a current-format file can be written.
        details.sources.push(private);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{AreaId, backends::local_migration::migrate_doorless};
    use serde_json::json;

    pub(crate) fn legacy_private_map(id: AreaId) -> serde_json::Value {
        let link = Uuid::from_u128(100);
        let room = |number, target, secret| {
            json!({
                "room_number": number, "title": format!("Room {number}"), "description": "",
                "x": number, "y": 0, "level": 0, "color": "", "is_secret": secret,
                "properties": [{"name": "ordinary", "value": "keep"}, {"name": "private-note", "value": "hidden", "is_secret": true}],
                "tags": ["tag"], "exits": [{
                "id": Uuid::from_u128(u128::try_from(number).unwrap()), "from_direction": if number == 1 {"East"} else {"West"},
                    "to_area_id": id, "to_room_number": target, "to_direction": null,
                    "path": "", "is_hidden": false, "is_closed": true, "is_locked": false,
                    "weight": 1, "command": "", "connection_id": link
                }]
            })
        };
        json!({
            "id": id, "user_id": null, "atlas_id": null, "name": "Legacy private",
            "created_at": "2026-08-01T00:00:00Z", "rev": 5, "format_version": 2,
            "properties": [{"name": "public", "value": "keep"}, {"name": "private", "value": "hidden", "is_secret": true}],
            "rooms": [room(1, 2, false), room(2, 1, true)],
            "labels": [{
                "id": Uuid::from_u128(200), "level": 0, "x": 0, "y": 0, "width": 1, "height": 1,
                "horizontal_alignment": "Center", "vertical_alignment": "Center", "text": "Private label",
                "color": "", "background_color": "", "font_size": 12, "font_weight": 400, "is_secret": true
            }],
            "shapes": [{
                "id": Uuid::from_u128(300), "level": 0, "x": 0, "y": 0, "width": 1, "height": 1,
                "background_color": null, "stroke_color": null, "shape_type": "Rectangle",
                "border_radius": 0, "stroke_width": 1, "is_secret": true
            }],
            "connections": [{
                "id": link, "endpoint_a": {"room_number": 1, "side": "East", "port_offset": 0.5, "port_mode": "AutoPinned"},
                "endpoint_b": {"room_number": 2, "side": "West", "port_offset": 0.5, "port_mode": "AutoPinned"},
                "kind": "Internal", "routing": "Simple", "segment_shape": "Direct", "corner": "Sharp",
                "route_points": [], "dash": "Solid", "color": "#A4A4A4", "thickness": 1
            }]
        })
    }

    #[test]
    fn legacy_flags_become_private_content_with_qualified_links() {
        let details = migrate_doorless(legacy_private_map(AreaId(Uuid::new_v4()))).unwrap();
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].room_number, RoomNumber(1));
        assert_eq!(details.rooms[0].properties.len(), 1);
        assert!(details.rooms[0].exits.is_empty());
        assert_eq!(details.properties[0].name, "public");
        assert!(
            details.connections.is_empty()
                && details.labels.is_empty()
                && details.shapes.is_empty()
        );
        let private = &details.sources[0];
        assert_eq!(private.source, SourceId::Private);
        assert_eq!(private.rooms[0].room_number, RoomNumber(2));
        assert_eq!(private.rooms[0].properties.len(), 2);
        assert_eq!(private.properties[0].name, "private");
        assert_eq!(private.room_data[0].properties[0].name, "private-note");
        assert_eq!(
            private.room_data[0].exits[0].to_source,
            Some(SourceId::Private)
        );
        assert_eq!(
            private.room_data[0].exits[0].door.as_ref().unwrap().state,
            crate::DoorState::Closed
        );
        assert_eq!(private.rooms[0].exits[0].to_source, None);
        assert_eq!(private.connections[0].endpoint_a.source, None);
        assert_eq!(
            private.connections[0].endpoint_b.unwrap().source,
            Some(SourceId::Private)
        );
        assert_eq!(private.labels.len(), 1);
        assert_eq!(private.shapes.len(), 1);
        let mut view =
            super::super::source_document::SourceDocument::open(&details, SourceId::Private)
                .unwrap();
        super::super::area_edits::validate_connection_graph_in(&mut view.content, &view.context)
            .unwrap();
        assert!(
            !serde_json::to_string(&details)
                .unwrap()
                .contains("is_secret")
        );
    }

    #[test]
    fn private_exit_between_ordinary_rooms_keeps_its_paired_link_private() {
        let mut value = legacy_private_map(AreaId(Uuid::new_v4()));
        value["rooms"][1]["is_secret"] = json!(false);
        value["rooms"][0]["exits"][0]["is_secret"] = json!(true);
        let details = migrate_doorless(value).unwrap();
        assert_eq!(details.rooms.len(), 2);
        assert!(details.rooms.iter().all(|room| room.exits.is_empty()));
        assert!(details.sources[0].rooms.is_empty());
        assert_eq!(
            details.sources[0]
                .room_data
                .iter()
                .map(|data| data.exits.len())
                .sum::<usize>(),
            2
        );
        assert_eq!(details.sources[0].connections.len(), 1);
    }

    #[test]
    fn private_open_commands_do_not_become_ordinary_door_instructions() {
        let mut value = legacy_private_map(AreaId(Uuid::new_v4()));
        value["rooms"][1]["is_secret"] = json!(false);
        value["rooms"][0]["properties"] = json!([
            {"name": "open_e_command", "value": "pull concealed lever", "is_secret": true}
        ]);
        let details = migrate_doorless(value).unwrap();
        assert!(details.rooms.iter().all(|room| room.exits.is_empty()));
        assert_eq!(
            details.sources[0].room_data[0].exits[0]
                .door
                .as_ref()
                .unwrap()
                .opens_with
                .as_deref(),
            Some("pull concealed lever")
        );
    }

    #[test]
    fn v1_and_v2_json_imports_preserve_private_classifications() {
        for version in [1, 2] {
            let mut value = legacy_private_map(AreaId(Uuid::new_v4()));
            value["format_version"] = json!(version);
            if version == 1 {
                value.as_object_mut().unwrap().remove("connections");
                for room in value["rooms"].as_array_mut().unwrap() {
                    for exit in room["exits"].as_array_mut().unwrap() {
                        exit.as_object_mut().unwrap().remove("connection_id");
                        exit["style"] = json!("Normal");
                        exit["color"] = json!("#A4A4A4");
                    }
                }
            }
            let imported: crate::mapper::AreaImportDocument =
                serde_json::from_value(value).unwrap();
            let details = imported.into_inner();
            assert_eq!(details.sources[0].source, SourceId::Private);
            assert_eq!(details.sources[0].rooms[0].room_number, RoomNumber(2));
            crate::mapper::validate_import_document(&details).unwrap();
        }
    }

    #[test]
    fn batch_import_qualifies_legacy_cross_map_private_destinations() {
        use crate::mapper::AreaImportDocument;
        let origin = AreaId(Uuid::new_v4());
        let target = AreaId(Uuid::new_v4());
        let mut from = legacy_private_map(origin);
        from["rooms"][0]["exits"][0]["to_area_id"] = json!(target);
        let documents = [from, legacy_private_map(target)]
            .into_iter()
            .map(|value| serde_json::from_value::<AreaImportDocument>(value).unwrap())
            .collect();
        let migrated = AreaImportDocument::into_documents(documents);
        assert_eq!(
            migrated[0].sources[0].room_data[0].exits[0].to_source,
            Some(SourceId::Private)
        );
        // A current-format unqualified address deliberately names the map,
        // even when a legacy neighbor migrates a Private room of that number.
        let mut current = serde_json::to_value(&migrated[0]).unwrap();
        current["sources"][0]["room_data"][0]["exits"][0]["to_source"] = serde_json::Value::Null;
        let mixed = AreaImportDocument::into_documents(vec![
            serde_json::from_value(current).unwrap(),
            serde_json::from_value(legacy_private_map(target)).unwrap(),
        ]);
        assert_eq!(mixed[0].sources[0].room_data[0].exits[0].to_source, None);
    }
}
