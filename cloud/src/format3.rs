//! Map wire format 3: the cloud service's per-source projection of a map
//! (`smudgy-cloudflare/docs/format-3.md`). A map's content arrives as one
//! bundle per source the caller can read: the map itself first, then Secrets
//! and the caller's Private additions. The client keeps its single area
//! document: the map bundle becomes the document's content, and every other
//! bundle rides beside it, unmerged.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::{
    AREA_FORMAT_VERSION, Area, AreaId, AreaWithDetails, CloudError, Connection, Exit, Label,
    LinkedAreaInfo, Property, RoomNumber, RoomWithDetails, Shape, SourceId,
};

/// The format a cloud projection must declare; anything else fails closed.
pub const WIRE_FORMAT_VERSION: u32 = 3;

/// `GET /areas/{id}` in format 3.
#[derive(Debug, Clone, Deserialize)]
pub struct AreaProjection {
    pub format_version: u32,
    #[serde(flatten)]
    pub area: Area,
    #[serde(default)]
    pub linked_areas: Vec<LinkedAreaInfo>,
    pub sources: Vec<SourceBundle>,
}

/// One source's content, as the caller sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceBundle {
    pub source: SourceId,
    /// A Secret's name; absent for the map and Private sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A Secret's ownership badge: `owner`, `members` or `clan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<String>,
    /// The clan a Clan Secret (`members` or `clan` ownership) belongs to;
    /// absent for owner Secrets, the map and Private.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clan_id: Option<uuid::Uuid>,
    /// A Secret's chosen color, `#rrggbb`; absent when none was chosen
    /// (the client's palette picks one) and for the map and Private.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub rev: i64,
    /// The caller's own actions on this source (`read`, `add`, `edit`,
    /// `remove`; on a Secret also `manage_access`, `copy` and the
    /// ownership authority actions they hold).
    pub actions: BTreeSet<String>,
    #[serde(default)]
    pub properties: Vec<Property>,
    /// The source's own rooms, in its own numbering.
    #[serde(default)]
    pub rooms: Vec<RoomWithDetails>,
    /// Attachments to rooms owned by another readable source of this map.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub room_data: Vec<RoomData>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub connections: Vec<Connection>,
}

/// A source's attachments to a room. Room visibility and attachment-source
/// visibility are both required; neither grants access to the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomData {
    pub room_number: RoomNumber,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_source: Option<SourceId>,
    #[serde(default)]
    pub properties: Vec<Property>,
    #[serde(default)]
    pub tags: BTreeSet<String>,
    #[serde(default)]
    pub exits: Vec<Exit>,
}

impl SourceBundle {
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.contains(action)
    }

    /// The chosen color as RGB, when there is one and it parses.
    #[must_use]
    pub fn rgb(&self) -> Option<[u8; 3]> {
        self.color.as_deref().and_then(parse_hex_color)
    }
}

/// A `#rrggbb` color (any case) as RGB.
#[must_use]
pub fn parse_hex_color(color: &str) -> Option<[u8; 3]> {
    let hex = color.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// Keep references only to rooms present in this readable projection.
/// A reference into another map's Secret is separately filtered by the server.
fn keep_readable_references(
    bundle: &mut SourceBundle,
    map: AreaId,
    rooms: &BTreeSet<(SourceId, RoomNumber)>,
) {
    let readable = |source: Option<SourceId>, number: RoomNumber| {
        source.is_none_or(|source| source.is_map() || rooms.contains(&(source, number)))
    };
    let keep_exits = |exits: &mut Vec<Exit>| {
        exits.retain(|exit| {
            exit.to_area_id.is_some_and(|to| to != map)
                || exit
                    .to_room_number
                    .is_none_or(|number| readable(exit.to_source, number))
        });
    };
    for room in &mut bundle.rooms {
        keep_exits(&mut room.exits);
    }
    bundle
        .room_data
        .retain(|data| readable(data.room_source, data.room_number));
    for data in &mut bundle.room_data {
        keep_exits(&mut data.exits);
    }
    bundle.connections.retain(|connection| {
        readable(
            connection.endpoint_a.source,
            connection.endpoint_a.room_number,
        ) && connection
            .endpoint_b
            .is_none_or(|endpoint| readable(endpoint.source, endpoint.room_number))
    });
}
impl TryFrom<AreaProjection> for AreaWithDetails {
    type Error = CloudError;

    /// The map bundle becomes the document's content and its revision the
    /// document's revision; the remaining bundles are kept as they arrived,
    /// less anything referring to another source's rooms.
    fn try_from(projection: AreaProjection) -> Result<Self, Self::Error> {
        if projection.format_version != WIRE_FORMAT_VERSION {
            return Err(CloudError::SerializationError(format!(
                "unsupported map format {}; expected {WIRE_FORMAT_VERSION}",
                projection.format_version
            )));
        }
        let mut sources = projection.sources;
        let rooms = sources
            .iter()
            .flat_map(|bundle| {
                bundle
                    .rooms
                    .iter()
                    .map(|room| (bundle.source, room.room_number))
            })
            .collect();
        for bundle in &mut sources {
            keep_readable_references(bundle, projection.area.id, &rooms);
        }
        let position = sources
            .iter()
            .position(|bundle| bundle.source.is_map())
            .ok_or_else(|| {
                CloudError::SerializationError("projection has no map source".to_string())
            })?;
        let map = sources.remove(position);
        let mut area = projection.area;
        area.rev = map.rev;
        Ok(AreaWithDetails {
            area,
            format_version: AREA_FORMAT_VERSION,
            properties: map.properties,
            rooms: map.rooms,
            room_data: map.room_data,
            labels: map.labels,
            shapes: map.shapes,
            connections: map.connections,
            linked_areas: projection.linked_areas,
            sources,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Uuid;
    use serde_json::json;

    fn projection(format_version: u32, sources: &serde_json::Value) -> AreaProjection {
        serde_json::from_value(json!({
            "format_version": format_version,
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "user_id": "123e4567-e89b-12d3-a456-426614174001",
            "atlas_id": null,
            "name": "Midgaard",
            "created_at": "2026-10-04T00:00:00Z",
            "access": {
                "is_owner": true, "can_edit": true, "can_reshare": true,
                "can_copy": true, "can_admin": true, "include_secrets": true
            },
            "projection_token": "p_abc",
            "linked_areas": [],
            "sources": sources,
        }))
        .expect("projection parses")
    }

    fn room(number: i32, title: &str) -> serde_json::Value {
        json!({
            "room_number": number, "title": title, "description": "", "color": "",
            "level": 0, "x": 0.5, "y": 1.0, "properties": [], "exits": [], "tags": []
        })
    }

    #[test]
    fn the_map_bundle_becomes_the_document_and_other_sources_ride_beside_it() {
        let details = AreaWithDetails::try_from(projection(
            3,
            &json!([
                {
                    "source": "map", "rev": 41, "actions": ["read", "edit"],
                    "properties": [{ "name": "zone", "value": "z" }],
                    "rooms": [room(184, "Gate")], "labels": [], "shapes": [], "connections": []
                },
                {
                    "source": "private", "rev": 3, "actions": ["read", "add", "edit", "remove"],
                    "properties": [], "rooms": [room(1, "Mine")],
                    "room_data": [{
                        "room_number": 184,
                        "properties": [{ "name": "notes", "value": "mine" }],
                        "tags": ["QUEST"], "exits": []
                    }],
                    "labels": [], "shapes": [], "connections": []
                }
            ]),
        ))
        .expect("converts");

        assert_eq!(details.area.rev, 41);
        assert_eq!(details.area.projection_token.as_deref(), Some("p_abc"));
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].title, "Gate");
        assert_eq!(details.properties[0].name, "zone");
        assert_eq!(details.sources.len(), 1);
        let private = &details.sources[0];
        assert_eq!(private.source, SourceId::Private);
        assert!(private.can("edit"));
        assert_eq!(private.rooms[0].title, "Mine");
        assert_eq!(private.room_data[0].room_number, RoomNumber(184));
        assert_eq!(private.room_data[0].properties[0].value, "mine");
    }

    #[test]
    fn other_formats_and_a_missing_map_bundle_fail_closed() {
        let map = json!([{ "source": "map", "rev": 1, "actions": ["read"] }]);
        assert!(AreaWithDetails::try_from(projection(2, &map)).is_err());
        assert!(AreaWithDetails::try_from(projection(4, &map)).is_err());
        let only_private = json!([{ "source": "private", "rev": 1, "actions": ["read"] }]);
        assert!(AreaWithDetails::try_from(projection(3, &only_private)).is_err());
    }

    /// Whatever arrives, no exit or link naming another source's rooms
    /// reaches the area model: not in the map's content, not in a Secret's.
    #[test]
    fn references_to_unavailable_rooms_never_reach_the_area() {
        const AREA: &str = "123e4567-e89b-12d3-a456-426614174000";
        const SECRET: &str = "6f1c2a9e-0b7d-4e1a-9c3f-2d8e5b4a7c10";
        const OTHER: &str = "0b7d6f1c-2a9e-4e1a-9c3f-2d8e5b4a7c11";
        let exit = |id: u128, to_source: Option<&str>| {
            json!({
                "id": Uuid::from_u128(id), "from_direction": "East",
                "to_area_id": AREA, "to_room_number": 1, "to_source": to_source,
                "to_direction": null, "to_unknown": false, "path": "", "command": "",
                "weight": 1.0, "connection_id": Uuid::from_u128(0xc0 + id),
                "is_hidden": true, "door": null
            })
        };
        let link = |id: u128, source: Option<&str>| {
            json!({
                "id": Uuid::from_u128(0xc0 + id),
                "endpoint_a": { "room_number": 2, "side": "East", "port_offset": 0.5, "port_mode": "AutoPinned" },
                "endpoint_b": {
                    "room_number": 1, "source": source, "side": "West",
                    "port_offset": 0.5, "port_mode": "AutoPinned"
                },
                "kind": "Internal", "routing": "Simple", "segment_shape": "Direct",
                "corner": "Sharp", "route_points": [], "dash": "Solid",
                "color": "#A4A4A4", "thickness": 1.0
            })
        };
        let mut gate = room(2, "Gate");
        gate["exits"] = json!([exit(1, Some(SECRET)), exit(2, None)]);
        let details = AreaWithDetails::try_from(projection(
            3,
            &json!([
                {
                    "source": "map", "rev": 1, "actions": ["read"], "rooms": [gate, room(1, "Hall")],
                    "connections": [link(1, Some(SECRET)), link(2, None)]
                },
                {
                    "source": SECRET, "name": "Bookcase", "rev": 1, "actions": ["read"],
                    "room_data": [{
                        "room_number": 2,
                        "exits": [exit(3, Some(OTHER)), exit(4, Some(SECRET)), exit(5, None)]
                    }],
                    "connections": [link(3, Some(OTHER)), link(4, Some(SECRET)), link(5, None)]
                }
            ]),
        ))
        .expect("converts");

        let ids = |exits: &[Exit]| {
            exits
                .iter()
                .map(|exit| exit.id.0.as_u128())
                .collect::<Vec<_>>()
        };
        let gate = details
            .rooms
            .iter()
            .find(|room| room.room_number == RoomNumber(2))
            .expect("the Gate");
        assert_eq!(ids(&gate.exits), [2]);
        let links = |connections: &[Connection]| {
            connections
                .iter()
                .map(|connection| connection.id.0.as_u128() - 0xc0)
                .collect::<Vec<_>>()
        };
        assert_eq!(links(&details.connections), [2]);
        let secret = &details.sources[0];
        assert_eq!(ids(&secret.room_data[0].exits), [5]);
        assert_eq!(links(&secret.connections), [5]);
    }

    /// An exit into a room of another map's Secret is the one reference into
    /// another place, from the map and from a Secret alike; it arrives with
    /// its door.
    #[test]
    fn exits_into_another_maps_secret_rooms_stay() {
        const OTHER_MAP: &str = "0b7d6f1c-2a9e-4e1a-9c3f-2d8e5b4a7c12";
        const SECRET: &str = "6f1c2a9e-0b7d-4e1a-9c3f-2d8e5b4a7c10";
        const FOREIGN: &str = "0b7d6f1c-2a9e-4e1a-9c3f-2d8e5b4a7c11";
        let exit = |id: u128| {
            json!({
                "id": Uuid::from_u128(id), "from_direction": "East",
                "to_area_id": OTHER_MAP, "to_room_number": 4, "to_source": FOREIGN,
                "to_direction": null, "to_unknown": false, "path": "", "command": "",
                "weight": 1.0, "connection_id": Uuid::from_u128(0xc0 + id),
                "is_hidden": false,
                "door": { "state": "locked", "name": "gate", "opens_with": "unlock gate" }
            })
        };
        let mut gate = room(2, "Gate");
        gate["exits"] = json!([exit(1)]);
        let details = AreaWithDetails::try_from(projection(
            3,
            &json!([
                { "source": "map", "rev": 1, "actions": ["read"], "rooms": [gate] },
                {
                    "source": SECRET, "name": "Bookcase", "rev": 1, "actions": ["read"],
                    "room_data": [{ "room_number": 2, "exits": [exit(2)] }]
                }
            ]),
        ))
        .expect("converts");
        let kept = &details.rooms[0].exits[0];
        assert_eq!(
            kept.foreign_secret(),
            Some((
                AreaId(Uuid::parse_str(OTHER_MAP).unwrap()),
                Uuid::parse_str(FOREIGN).unwrap()
            ))
        );
        assert_eq!(
            kept.door,
            Some(crate::Door {
                state: crate::DoorState::Locked,
                name: Some("gate".to_string()),
                opens_with: Some("unlock gate".to_string()),
            })
        );
        assert_eq!(details.sources[0].room_data[0].exits.len(), 1);
    }

    #[test]
    fn a_secret_color_is_six_hex_digits() {
        assert_eq!(parse_hex_color("#3A7bd5"), Some([0x3a, 0x7b, 0xd5]));
        for bad in ["3a7bd5", "#3a7", "#3a7bd5ff", "#3a7bdz", "blue", ""] {
            assert_eq!(parse_hex_color(bad), None, "{bad}");
        }
    }
}
