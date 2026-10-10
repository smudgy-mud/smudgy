//! POST /areas/{id}/mutations — the compound mutation envelope endpoint.
//! Mirrors the real server's `mapping::{contract, executor, ops}`: receipt-
//! gated idempotency per `(actor, operation_id)`, the single-source
//! precondition rule with its revision CAS check, ordered all-or-nothing
//! application, and exactly one revision bump per touched aggregate (an
//! all-no-op envelope bumps nothing at all).
//!
//! One envelope writes one source: the map, or one of its Secrets for a
//! caller holding the actions each operation needs. A Secret's write
//! lands only in the Secret; a map write that deletes a map room takes
//! every Secret's hold on it along. Who cannot read a Secret learns
//! nothing of it from any answer: versions, echoes, receipts, conflicts
//! and refusals read exactly as they would on a map without it, and a
//! write naming another source's room is refused from the request alone.
//!
//! Exits carry doors (format-3 §4.3), and may lead into a room of another
//! map's Secret (§4.4): such an exit shows only to that Secret's readers,
//! moves no revision when it alone changes, and to a writer not shown it
//! is an exit that does not exist. Connection operations (`create_link`'s
//! `create_connection`, `update_connection`, `pair`, `unlink`,
//! `delete_link`) follow the server's `links.ts`, and every connection an
//! envelope touches is validated at its end, as the server validates them.

mod qualified;

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::areas::{
    DIRECTIONS, H_ALIGN, SHAPE_TYPES, V_ALIGN, check_enum, double_option, embedded_exit_json,
    embedded_label_json, embedded_shape_json, require_caps,
};
use super::connections::{
    self, NewExitLink, attach_for_new_exit, cleanup_after_exit_delete, maintain_after_retarget,
};
use super::http::{authenticate, bad_request, err, err_with_details, not_found, ok, parse_area_id};
use super::mock_server::Shared;
use super::state::{
    AreaPropRecord, AreaRecord, Caps, ConnectionRecord, DoorRecord, EndpointRecord, ExitRecord,
    LabelRecord, MockState, MutationReceipt, RoomPropRecord, RoomRecord, STAND_IN, ShapeRecord,
    map_room_of, stand_in,
};

/// Most operations one envelope may carry (mirrors `contract.rs`).
pub const MAX_MUTATION_OPERATIONS: usize = 256;

/// The wire envelope (`MutationEnvelope<Vec<AreaMutation>>`).
#[derive(Debug, Deserialize)]
struct WireEnvelope {
    operation_id: Uuid,
    /// The written source; absent is the map itself. Of a Secret, the mock
    /// serves its owner's writes to its labels and shapes.
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    preconditions: Vec<WirePrecondition>,
    payload: Vec<AreaOp>,
}

#[derive(Debug, Deserialize)]
struct WirePrecondition {
    resource: String,
    id: Uuid,
    #[serde(default)]
    source: Option<String>,
    expected_rev: i64,
}

fn names_map(source: Option<&str>) -> bool {
    source.is_none_or(|source| source == "map")
}

/// Format 3 replaces `is_secret` with sources; the server refuses the key
/// anywhere in a write body.
pub fn mentions_is_secret(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => {
            fields.contains_key("is_secret") || fields.values().any(mentions_is_secret)
        }
        serde_json::Value::Array(items) => items.iter().any(mentions_is_secret),
        _ => false,
    }
}

/// One operation of a compound mutation — the `op`-tagged wire alphabet of
/// the server's `ops::AreaMutation`. Serialization doubles as the canonical
/// form the receipt request-hash covers, so key order and whitespace in the
/// incoming JSON never affect deduplication.
///
/// `room_source`, `to_source` and endpoint `source` may name only the
/// envelope's own source ([`check_named_sources`]); a room named without
/// one is a map room.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum AreaOp {
    UpsertRoom {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
        body: UpsertRoomBody,
    },
    /// Must-not-exist creation: a number the written source already uses
    /// is refused with `room_number_exists`. Each source numbers its own
    /// rooms, so another's never count.
    CreateRoom {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
        body: UpsertRoomBody,
    },
    DeleteRoom {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
    },
    AssertMergeSafe {
        keep_room_number: i32,
        remove_room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
    },
    UpsertRoomProperty {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
        name: String,
        value: String,
    },
    DeleteRoomProperty {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
        name: String,
    },
    AddRoomTag {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
        tag: String,
    },
    RemoveRoomTag {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
        tag: String,
    },
    UpsertAreaProperty {
        name: String,
        value: String,
    },
    DeleteAreaProperty {
        name: String,
    },
    CreateExit {
        room_number: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<String>,
        body: CreateExitBody,
    },
    UpdateExit {
        exit_id: Uuid,
        body: UpdateExitBody,
    },
    DeleteExit {
        exit_id: Uuid,
    },
    CreateConnection {
        body: CreateConnectionBody,
    },
    UpdateConnection {
        connection_id: Uuid,
        body: UpdateConnectionBody,
    },
    Pair {
        keep_connection_id: Uuid,
        merge_connection_id: Uuid,
    },
    Unlink {
        exit_id: Uuid,
        new_connection_id: Uuid,
    },
    DeleteLink {
        connection_id: Uuid,
    },
    CreateLabel {
        body: CreateLabelBody,
    },
    UpdateLabel {
        label_id: Uuid,
        body: UpdateLabelBody,
    },
    DeleteLabel {
        label_id: Uuid,
    },
    CreateShape {
        body: CreateShapeBody,
    },
    UpdateShape {
        shape_id: Uuid,
        body: UpdateShapeBody,
    },
    DeleteShape {
        shape_id: Uuid,
    },
}

impl AreaOp {
    /// The source this operation's own room reference names, if any.
    fn room_source(&self) -> Option<&str> {
        match self {
            Self::UpsertRoom { room_source, .. }
            | Self::CreateRoom { room_source, .. }
            | Self::DeleteRoom { room_source, .. }
            | Self::AssertMergeSafe { room_source, .. }
            | Self::UpsertRoomProperty { room_source, .. }
            | Self::DeleteRoomProperty { room_source, .. }
            | Self::AddRoomTag { room_source, .. }
            | Self::RemoveRoomTag { room_source, .. }
            | Self::CreateExit { room_source, .. } => room_source.as_deref(),
            _ => None,
        }
    }

    /// The `source` of each connection endpoint this operation names.
    fn endpoint_sources(&self) -> Vec<Option<&str>> {
        match self {
            Self::CreateConnection { body } => std::iter::once(&body.endpoint_a)
                .chain(body.endpoint_b.iter())
                .map(|endpoint| endpoint.source.as_deref())
                .collect(),
            Self::UpdateConnection { body, .. } => body
                .endpoint_a
                .iter()
                .chain(body.endpoint_b.iter())
                .map(|endpoint| endpoint.source.as_deref())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether this operation keys data to one of the map's rooms (no
    /// `room_source`) or leads an exit into one.
    fn touches_map_room(&self) -> bool {
        let into_map = |to_room: Option<i32>, to_source: Option<&str>| {
            to_room.is_some() && to_source.is_none_or(|source| source == "map")
        };
        match self {
            Self::UpsertRoom { room_source, .. }
            | Self::CreateRoom { room_source, .. }
            | Self::DeleteRoom { room_source, .. }
            | Self::AssertMergeSafe { room_source, .. }
            | Self::UpsertRoomProperty { room_source, .. }
            | Self::DeleteRoomProperty { room_source, .. }
            | Self::AddRoomTag { room_source, .. }
            | Self::RemoveRoomTag { room_source, .. } => {
                room_source.as_deref().is_none_or(|source| source == "map")
            }
            Self::CreateExit {
                room_source, body, ..
            } => {
                room_source.as_deref().is_none_or(|source| source == "map")
                    || into_map(body.to_room_number, body.to_source.as_deref())
            }
            Self::UpdateExit { body, .. } => into_map(
                body.to_room_number,
                body.to_source.as_ref().and_then(Option::as_deref),
            ),
            Self::CreateConnection { .. } | Self::UpdateConnection { .. } => self
                .endpoint_sources()
                .iter()
                .any(|source| source.is_none_or(|source| source == "map")),
            _ => false,
        }
    }

    /// The action on a Secret this operation needs, as the server judges
    /// each operation: `add` to create, `remove` to delete, `edit` for
    /// everything else (upserts and the merge-safety assertion included).
    fn secret_action(&self) -> &'static str {
        match self {
            Self::CreateRoom { .. }
            | Self::CreateExit { .. }
            | Self::CreateConnection { .. }
            | Self::CreateLabel { .. }
            | Self::CreateShape { .. }
            | Self::AddRoomTag { .. } => "add",
            Self::DeleteRoom { .. }
            | Self::DeleteRoomProperty { .. }
            | Self::RemoveRoomTag { .. }
            | Self::DeleteAreaProperty { .. }
            | Self::DeleteExit { .. }
            | Self::DeleteLink { .. }
            | Self::DeleteLabel { .. }
            | Self::DeleteShape { .. } => "remove",
            Self::UpsertRoom { .. }
            | Self::AssertMergeSafe { .. }
            | Self::UpsertRoomProperty { .. }
            | Self::UpsertAreaProperty { .. }
            | Self::UpdateExit { .. }
            | Self::UpdateConnection { .. }
            | Self::Pair { .. }
            | Self::Unlink { .. }
            | Self::UpdateLabel { .. }
            | Self::UpdateShape { .. } => "edit",
        }
    }
}

/// The source a wire name refers to, compared by id where it is one.
fn same_source(named: &str, own: &str) -> bool {
    match (Uuid::parse_str(named), Uuid::parse_str(own)) {
        (Ok(named), Ok(own)) => named == own,
        _ => named == own,
    }
}

/// The server's request-only refusal of a `to_source` naming another map's
/// Secret without its map and room.
const FOREIGN_NEEDS: &str = "invalid mutation envelope: a `to_source` naming another map's Secret needs `to_area_id` and `to_room_number`";

/// Refuses, from the request alone and in the server's order, every
/// `room_source`, connection endpoint `source` and exit `to_source` naming
/// anything but the envelope's own source (a map envelope may name none),
/// except an exit's `to_source` with another map's `to_area_id`, which names
/// a Secret on that map (format-3 §4.4) and must come with its room. The
/// refusal reads the same whether the source named exists, is readable, or
/// is neither: no write joins two of a map's sources' rooms or puts a
/// source's room in the map's content.
fn check_named_sources(
    area_id: Uuid,
    envelope_source: Option<&str>,
    ops: &[AreaOp],
) -> Result<(), Response> {
    let own = envelope_source.unwrap_or("map");
    let valid = |named: &str| {
        if named == "map" || named == "private" || Uuid::parse_str(named).is_ok() {
            Ok(())
        } else {
            Err(bad_request("invalid mutation envelope: unknown source"))
        }
    };
    valid(own)?;
    for op in ops {
        if let Some(named) = op.room_source() {
            valid(named)?;
        }
        if matches!(
            op,
            AreaOp::UpsertRoom { .. }
                | AreaOp::CreateRoom { .. }
                | AreaOp::DeleteRoom { .. }
                | AreaOp::AssertMergeSafe { .. }
        ) && !same_source(op.room_source().unwrap_or("map"), own)
        {
            return Err(bad_request(
                "invalid mutation envelope: a source cannot change a map room's own fields",
            ));
        }
        for named in op.endpoint_sources().into_iter().flatten() {
            valid(named)?;
        }
        let foreign = |named: &str, target: Option<Uuid>, room: Option<i32>| {
            valid(named)?;
            if target.is_some_and(|id| id != area_id) {
                if named == "map" || (named != "private" && same_source(named, own)) {
                    return Err(bad_request(
                        "invalid mutation envelope: `to_source` with another map's `to_area_id` names a Secret on that map",
                    ));
                }
                if room.is_none() {
                    return Err(bad_request(FOREIGN_NEEDS));
                }
            }
            Ok(())
        };
        match op {
            AreaOp::CreateExit { body, .. } => {
                if let Some(named) = body.to_source.as_deref() {
                    foreign(named, body.to_area_id, body.to_room_number)?;
                }
            }
            AreaOp::UpdateExit { body, .. } => {
                if let Some(Some(named)) = &body.to_source {
                    if body.to_area_id.is_none()
                        && !same_source(named, own)
                        && named != "map"
                        && named != "private"
                    {
                        return Err(bad_request(FOREIGN_NEEDS));
                    }
                    foreign(named, body.to_area_id, body.to_room_number)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Refuses, as the server's envelope parser does, an exit body that
/// carries `is_closed` or `is_locked` (which `door` replaces) or a `door`
/// that is not one (format-3 §4.3). Checked over the raw JSON, operation by
/// operation, before the envelope is read into its types.
fn check_exit_bodies(envelope: &Value) -> Result<(), Response> {
    let invalid = |reason: &str| Err(bad_request(&format!("invalid mutation envelope: {reason}")));
    let Some(payload) = envelope.get("payload").and_then(Value::as_array) else {
        return Ok(());
    };
    for op in payload {
        let is_exit = matches!(
            op.get("op").and_then(Value::as_str),
            Some("create_exit" | "update_exit")
        );
        let Some(body) = op
            .get("body")
            .and_then(Value::as_object)
            .filter(|_| is_exit)
        else {
            continue;
        };
        if body.contains_key("is_closed") || body.contains_key("is_locked") {
            return invalid("`is_closed` and `is_locked` are replaced by `door`");
        }
        let door = match body.get("door") {
            None | Some(Value::Null) => continue,
            Some(Value::Object(door)) => door,
            Some(_) => return invalid("expected `door` to be an object"),
        };
        match door.get("state") {
            None => return invalid("missing field `state`"),
            Some(Value::String(state)) if DOOR_STATES.contains(&state.as_str()) => {}
            Some(Value::String(state)) => {
                return invalid(&format!("unknown variant `{state}` for `state`"));
            }
            Some(_) => return invalid("expected a string for `state`"),
        }
        for (field, limit) in [("name", 64), ("opens_with", 255)] {
            match door.get(field) {
                Some(Value::String(value)) if value.is_empty() => {
                    return invalid(&format!("`{field}` must not be empty; send null for none"));
                }
                Some(Value::String(value)) if value.chars().count() > limit => {
                    return invalid(&format!("`{field}` must be at most {limit} characters"));
                }
                None | Some(Value::Null | Value::String(_)) => {}
                Some(_) => return invalid(&format!("expected a string for `{field}`")),
            }
        }
    }
    Ok(())
}

/// Names, tags and colors a source keeps are at most this many characters.
const KEPT_LIMIT: usize = 64;

/// Refuses, as the server's envelope parser does, what a source would keep
/// out of bounds, and a new exit whose destination fields disagree: a
/// property name, a tag (as trimmed and uppercased) or a color over
/// [`KEPT_LIMIT`] characters, an empty tag, and a `to_room_number` or
/// `to_source` without `to_area_id`. Checked over the raw JSON, before the
/// map is looked up.
fn check_kept_fields(envelope: &Value) -> Result<(), Response> {
    let invalid = |reason: &str| Err(bad_request(&format!("invalid mutation envelope: {reason}")));
    let too_long = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|text| text.chars().count() > KEPT_LIMIT)
    };
    let Some(payload) = envelope.get("payload").and_then(Value::as_array) else {
        return Ok(());
    };
    for op in payload {
        let body = op.get("body");
        let field = |name: &str| body.and_then(|body| body.get(name));
        let colors: &[&str] = match op.get("op").and_then(Value::as_str) {
            Some(
                "upsert_room_property"
                | "delete_room_property"
                | "upsert_area_property"
                | "delete_area_property",
            ) => {
                if too_long(op.get("name")) {
                    return invalid("A property name must be at most 64 characters");
                }
                &[]
            }
            Some("add_room_tag" | "remove_room_tag") => {
                if let Some(tag) = op.get("tag").and_then(Value::as_str) {
                    let normalized = tag.trim().to_uppercase();
                    if normalized.is_empty() {
                        return invalid("tag must not be empty");
                    }
                    if normalized.chars().count() > KEPT_LIMIT {
                        return invalid("A tag must be at most 64 characters");
                    }
                }
                &[]
            }
            Some("create_exit") => {
                let to_area = field("to_area_id").is_some_and(|value| !value.is_null());
                if !to_area && field("to_room_number").is_some_and(|value| !value.is_null()) {
                    return invalid("`to_room_number` needs `to_area_id`");
                }
                if !to_area && field("to_source").is_some_and(|value| !value.is_null()) {
                    return invalid("`to_source` names a room in this map");
                }
                &[]
            }
            Some("upsert_room" | "create_room") => &["color"],
            Some("create_label" | "update_label") => &["color", "background_color"],
            Some("create_shape" | "update_shape") => &["background_color", "stroke_color"],
            _ => &[],
        };
        if colors.iter().any(|name| too_long(field(name))) {
            return invalid("A color must be at most 64 characters");
        }
    }
    Ok(())
}

/// A door's states (format-3 §2.1); no door is `null`, never a state.
const DOOR_STATES: [&str; 3] = ["open", "closed", "locked"];

/// An exit's door as a write gives it; keys beyond these are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DoorBody {
    state: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    opens_with: Option<String>,
}

impl DoorBody {
    fn record(self) -> DoorRecord {
        DoorRecord {
            state: self.state,
            name: self.name,
            opens_with: self.opens_with,
        }
    }
}

/// A connection endpoint as a write names it: a map room, or with
/// `source` one of the written source's own rooms.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EndpointBody {
    room_number: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    side: String,
    port_offset: f32,
    port_mode: String,
}

impl EndpointBody {
    fn record(&self) -> EndpointRecord {
        EndpointRecord {
            room_number: self.room_number,
            side: self.side.clone(),
            port_offset: self.port_offset,
            port_mode: self.port_mode.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PointBody {
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateConnectionBody {
    id: Uuid,
    endpoint_a: EndpointBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint_b: Option<EndpointBody>,
    routing: String,
    segment_shape: String,
    corner: String,
    #[serde(default)]
    route_points: Vec<PointBody>,
    dash: String,
    color: String,
    thickness: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateConnectionBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint_a: Option<EndpointBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint_b: Option<EndpointBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    segment_shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    corner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route_points: Option<Vec<PointBody>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thickness: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpsertRoomBody {
    title: Option<String>,
    description: Option<String>,
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    color: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    external_id: Option<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateExitBody {
    /// Client-minted entity id (v2 contract); minted here when absent.
    #[serde(default)]
    id: Option<Uuid>,
    #[serde(default)]
    connection_id: Option<Uuid>,
    #[serde(default)]
    new_connection_id: Option<Uuid>,
    from_direction: String,
    to_area_id: Option<Uuid>,
    to_room_number: Option<i32>,
    /// The destination is one of the writing source's own rooms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to_source: Option<String>,
    to_direction: Option<String>,
    path: Option<String>,
    is_hidden: bool,
    /// Absent or null: no door.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    door: Option<DoorBody>,
    weight: f32,
    command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateExitBody {
    from_direction: Option<String>,
    to_area_id: Option<Uuid>,
    to_room_number: Option<i32>,
    /// Absent keeps the destination's source; null retargets to a map
    /// room; a name retargets to one of the writing source's own rooms.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    to_source: Option<Option<String>>,
    to_direction: Option<String>,
    path: Option<String>,
    is_hidden: Option<bool>,
    /// Absent keeps the door; null removes it; a door replaces it whole.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    door: Option<Option<DoorBody>>,
    weight: Option<f32>,
    command: Option<String>,
    clear_to: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateLabelBody {
    /// Client-minted entity id (v2 contract); minted here when absent.
    #[serde(default)]
    id: Option<Uuid>,
    level: Option<i32>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    horizontal_alignment: String,
    vertical_alignment: String,
    text: String,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateLabelBody {
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    horizontal_alignment: Option<String>,
    vertical_alignment: Option<String>,
    text: Option<String>,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateShapeBody {
    /// Client-minted entity id (v2 contract); minted here when absent.
    #[serde(default)]
    id: Option<Uuid>,
    level: Option<i32>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: String,
    border_radius: Option<f32>,
    stroke_width: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateShapeBody {
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: Option<String>,
    /// The update endpoint's field is `radius` (create/response use
    /// `border_radius`), mirroring the server's asymmetry.
    radius: Option<f32>,
    stroke_width: Option<f32>,
}

/// The receipt request hash (`contract::request_hash`): SHA-256 over the
/// scope area plus the canonical serialization of the ordered operations.
fn request_hash(area_id: Uuid, ops: &[AreaOp]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(area_id.as_bytes());
    hasher.update(serde_json::to_vec(ops).expect("mutation operations always serialize"));
    hex::encode(hasher.finalize())
}

/// Caller/scope facts resolved once per envelope (`ops::ApplyCtx`).
struct ApplyCtx {
    viewer: Uuid,
    area_id: Uuid,
    /// The envelope writes the map itself. Only a map write creates map
    /// rooms or reaches other maps; a Secret's write changes nothing
    /// outside the Secret.
    writes_map: bool,
    /// The Secret the envelope writes, when it writes one.
    secret: Option<Uuid>,
    /// The connections the envelope touched, validated at its end.
    dirty: std::cell::RefCell<BTreeSet<Uuid>>,
    /// Rooms of the written Secret the envelope deleted: other maps' exits
    /// into them go after the commit.
    released: std::cell::RefCell<BTreeSet<i32>>,
}

/// How revisions must move for one applied operation (`ops::OpOutcome`).
struct OpOutcome {
    result: Value,
    /// Whether any scope-area row changed at all. An idempotent no-op (a
    /// tag re-add) reports false so an all-no-op envelope moves no revision
    /// counter — the row triggers never fired when no row changed either.
    changed: bool,
    /// Other areas whose rows the operation touched.
    foreign_bumps: Vec<Uuid>,
    /// Secrets, as (map, Secret), whose content the operation changed in
    /// passing: a deleted map room takes their data on it with it.
    secret_bumps: Vec<(Uuid, Uuid)>,
}

impl OpOutcome {
    fn scoped(result: Value) -> Self {
        Self {
            result,
            changed: true,
            foreign_bumps: Vec::new(),
            secret_bumps: Vec::new(),
        }
    }
}

/// Room tags are stored trimmed-UPPERCASE (the case-insensitive tag
/// invariant); every entry path into the applier normalizes here, so a
/// compound-route tag can never bypass the invariant the per-entity route
/// enforces. Empty after trimming is a validation failure.
fn normalize_tag(tag: &str) -> Result<String, Response> {
    let normalized = tag.trim().to_uppercase();
    if normalized.is_empty() {
        return Err(bad_request("tag must not be empty"));
    }
    Ok(normalized)
}

type Working = BTreeMap<Uuid, AreaRecord>;

/// POST /areas/{id}/mutations.
pub async fn area_mutations(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut st = state.lock();
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // The envelope's shape is judged before the map is looked up: a
    // malformed one is the same 400 whatever map it names.
    if serde_json::from_str::<serde_json::Value>(&body)
        .is_ok_and(|value| mentions_is_secret(&value))
    {
        return bad_request(
            "invalid mutation envelope: `is_secret` is not part of format 3; write to a source instead",
        );
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body)
        && let Err(refusal) = check_exit_bodies(&value).and_then(|()| check_kept_fields(&value))
    {
        return refusal;
    }
    let envelope: WireEnvelope = match serde_json::from_str(&body) {
        Ok(envelope) => envelope,
        Err(e) => return bad_request(&format!("invalid mutation envelope: {e}")),
    };
    if let Err(refusal) =
        check_named_sources(area_id, envelope.source.as_deref(), &envelope.payload)
    {
        return refusal;
    }
    // Exactly one precondition, naming the written source of this map.
    let names_written = |p: &WirePrecondition| {
        p.resource == "source"
            && p.id == area_id
            && if names_map(envelope.source.as_deref()) {
                names_map(p.source.as_deref())
            } else {
                p.source == envelope.source
            }
    };
    if !matches!(envelope.preconditions.as_slice(), [p] if names_written(p)) {
        return bad_request("expected exactly one precondition naming the mutated source");
    }
    if envelope.payload.is_empty() {
        return bad_request("empty mutation");
    }
    if envelope.payload.len() > MAX_MUTATION_OPERATIONS {
        return bad_request(&format!(
            "too many operations (max {MAX_MUTATION_OPERATIONS})"
        ));
    }

    // Fast-path authorization, like the route handler: uniform 404 for an
    // absent area or one the caller cannot read. Writing the map needs
    // `can_edit`; writing a Secret needs its own actions (below).
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) if c.can_view => c,
        Ok(_) => return not_found(),
        Err(e) => return e,
    };
    // A sealed export refuses writes to those who read it, until its drop.
    if st.sealed_maps.contains(&area_id) {
        return super::http::err(503, "Service temporarily unavailable");
    }
    if envelope.source.as_deref() == Some("private") {
        return write_private(&mut st, viewer, area_id, envelope);
    }
    let secret = match envelope.source.as_deref() {
        source if names_map(source) => {
            if !caps.can_edit {
                return not_found();
            }
            None
        }
        source => {
            let area = &st.areas[&area_id];
            let index = source
                .and_then(|wire| Uuid::parse_str(wire).ok())
                .and_then(|id| area.secrets.iter().position(|secret| secret.id == id));
            let Some(index) = index else {
                return not_found();
            };
            // A Secret the caller cannot read, or cannot write as each
            // operation needs, is the uniform 404.
            let actions = st.secret_actions(viewer, area, &area.secrets[index]);
            let allowed = !actions.is_empty()
                && envelope
                    .payload
                    .iter()
                    .all(|op| actions.contains(&op.secret_action()));
            if !allowed {
                return not_found();
            }
            // A clan's Secret on a map filed by link takes no data on the
            // map's rooms and no exits into them.
            if super::clan_secrets::linked_clan(area, &area.secrets[index]).is_some()
                && envelope.payload.iter().any(AreaOp::touches_map_room)
            {
                return not_found();
            }
            Some(index)
        }
    };
    if st.refuse_mutations > 0 {
        st.refuse_mutations -= 1;
        return bad_request("injected refusal");
    }

    let mut request_hash = request_hash(area_id, &envelope.payload);
    if let (Some(_), Some(source)) = (secret, &envelope.source) {
        request_hash = format!("{request_hash}:{source}");
    }

    // Receipt gate: an identical resend replays the stored body verbatim
    // (nothing re-applies, no revision moves); the caller's access was
    // re-checked above. A different body under an already-used id is a
    // client bug.
    if let Some(receipt) = st.mutation_receipts.get(&(viewer, envelope.operation_id)) {
        if receipt.request_hash != request_hash {
            return err(409, "operation_id_reused");
        }
        let stored = receipt.result.clone();
        st.mutation_log.push((envelope.operation_id, true));
        return finish(&mut st, ok(stored));
    }
    if let Some(refusal) = super::clan_maps::refused_while_disposing(&st, area_id) {
        return refusal;
    }
    if let Some(index) = secret {
        return write_source(
            &mut st,
            viewer,
            area_id,
            super::source_refs::Source::Secret(index),
            envelope,
            request_hash,
        );
    }

    // The precondition names the map's own source (the envelope's shape),
    // at its revision.
    let current_rev = st
        .areas
        .get(&area_id)
        .expect("caps proved the area exists")
        .rev;
    let precondition = &envelope.preconditions[0];
    if precondition.expected_rev != current_rev {
        return err_with_details(
            409,
            "revision_conflict",
            json!({
                "resource": "source",
                "id": area_id,
                "source": "map",
                "expected_rev": precondition.expected_rev,
                "current_rev": current_rev,
                "operation_id": envelope.operation_id,
            }),
        );
    }

    // Apply in order against a working copy — any failure returns with
    // nothing applied (the transaction-rollback analogue).
    let ctx = ApplyCtx {
        viewer,
        area_id,
        writes_map: true,
        secret: None,
        dirty: std::cell::RefCell::default(),
        released: std::cell::RefCell::default(),
    };
    let mut working: Working = st.areas.clone();
    let mut results: Vec<Value> = Vec::with_capacity(envelope.payload.len());
    let mut scope_changed = false;
    let mut foreign: BTreeSet<Uuid> = BTreeSet::new();
    let mut secrets_changed: BTreeSet<(Uuid, Uuid)> = BTreeSet::new();
    for op in envelope.payload {
        let op = match qualified::input(&st, &mut working, &ctx, op) {
            Ok(op) => op,
            Err(response) => return response,
        };
        match apply_op(&st, &mut working, &ctx, op) {
            Ok(outcome) => {
                scope_changed |= outcome.changed;
                for foreign_area in outcome.foreign_bumps {
                    if foreign_area == area_id {
                        scope_changed = true;
                    } else {
                        foreign.insert(foreign_area);
                    }
                }
                secrets_changed.extend(outcome.secret_bumps);
                results.push(qualified::output(
                    &st,
                    &working[&area_id],
                    &ctx,
                    outcome.result,
                ));
            }
            Err(response) => return response,
        }
    }
    if let Err(response) = validate_connections(&working[&area_id], &ctx) {
        return response;
    }

    // Commit, then exactly one bump per touched aggregate (sorted order),
    // reporting each at the caller's own projection of it. Foreign areas the
    // caller cannot view still bump but stay out of the response. An
    // envelope whose every operation was a no-op (idempotent tag re-adds)
    // moves no counter at all — matching the row triggers, which never
    // fired when no row changed — and reports the standing revision instead.
    st.areas = working;
    let mut bumps = foreign;
    if scope_changed {
        bumps.insert(area_id);
    }
    let mut versions: Vec<Value> = Vec::new();
    if bumps.is_empty() {
        versions.push(json!({
            "resource": "source",
            "id": area_id,
            "source": "map",
            "rev": current_rev,
            "deleted": false,
        }));
    }
    // Versions name only the written map's Library (its owner's maps):
    // maps of other owners change after the commit, unreported.
    let written_library = st
        .areas
        .get(&area_id)
        .map(|area| (area.clan_id, area.user_id));
    for bump_area in &bumps {
        st.bump(Some(*bump_area));
        let Some(area) = st.areas.get(bump_area) else {
            continue;
        };
        if *bump_area == area_id
            || (Some((area.clan_id, area.user_id)) == written_library
                && st
                    .caps(viewer, *bump_area)
                    .is_some_and(|caps| caps.can_view))
        {
            versions.push(json!({
                "resource": "source",
                "id": bump_area,
                "source": "map",
                "rev": area.rev,
                "deleted": false,
            }));
        }
    }
    // Secrets the write changed in passing move too, reported only to
    // their readers: to anyone else the response is the one the same write
    // would get on a map without them.
    for (map_id, secret_id) in secrets_changed {
        let Some(map) = st.areas.get_mut(&map_id) else {
            continue;
        };
        let Some(secret) = map.secrets.iter_mut().find(|secret| secret.id == secret_id) else {
            continue;
        };
        secret.rev += 1;
        let rev = secret.rev;
        let map = &st.areas[&map_id];
        let secret = map
            .secrets
            .iter()
            .find(|secret| secret.id == secret_id)
            .expect("just moved");
        if !st.secret_actions(viewer, map, secret).is_empty() {
            versions.push(json!({
                "resource": "source",
                "id": map_id,
                "source": secret_id.to_string(),
                "rev": rev,
                "deleted": false,
            }));
        }
    }

    let result = json!({
        "operation_id": envelope.operation_id,
        "versions": versions,
        "data": results,
    });
    st.mutation_receipts.insert(
        (viewer, envelope.operation_id),
        MutationReceipt {
            request_hash,
            result: result.clone(),
        },
    );
    st.mutation_log.push((envelope.operation_id, false));
    finish(&mut st, ok(result))
}

/// A write to Secret `index` of the map: its precondition names the Secret
/// at its revision, the operations land in the Secret, and the Secret's
/// revision alone moves, once, when anything changed. The map's appliers
/// run over the Secret as a document ([`super::secrets::secret_doc`]):
/// its own rooms under their numbers, map rooms at their stand-ins.
/// Nothing outside the Secret changes: no map room, no placeholder in
/// another map, no other map's revision.
fn write_private(
    st: &mut MockState,
    viewer: Uuid,
    area_id: Uuid,
    envelope: WireEnvelope,
) -> Response {
    if let Some(refusal) = super::clan_maps::refused_while_disposing(st, area_id) {
        return refusal;
    }
    let area = &st.areas[&area_id];
    if !super::transfer_policy::holds(
        st,
        area,
        viewer,
        &super::transfer_policy::private(st, area, viewer, area.atlas_id),
        "read",
    ) {
        return not_found();
    }
    if st.refuse_mutations > 0 {
        st.refuse_mutations -= 1;
        return bad_request("injected refusal");
    }
    let hash = format!("{}:private", request_hash(area_id, &envelope.payload));
    if let Some(receipt) = st.mutation_receipts.get(&(viewer, envelope.operation_id)) {
        return if receipt.request_hash == hash {
            ok(receipt.result.clone())
        } else {
            err(409, "operation_id_reused")
        };
    }
    let area = st.areas.get_mut(&area_id).unwrap();
    let created = !area.private_sources.contains_key(&viewer);
    area.private_sources.entry(viewer).or_insert_with(|| {
        let mut source = super::state::SecretRecord::new(Uuid::new_v4(), "Private".into());
        source.rev = 0;
        source
    });
    let operation = envelope.operation_id;
    let response = write_source(
        st,
        viewer,
        area_id,
        super::source_refs::Source::Private(viewer),
        envelope,
        hash,
    );
    if created && !st.mutation_receipts.contains_key(&(viewer, operation)) {
        st.areas
            .get_mut(&area_id)
            .unwrap()
            .private_sources
            .remove(&viewer);
    }
    response
}

fn write_source(
    st: &mut MockState,
    viewer: Uuid,
    area_id: Uuid,
    source: super::source_refs::Source,
    envelope: WireEnvelope,
    request_hash: String,
) -> Response {
    let wire = envelope.source.clone().unwrap_or_default();
    let current_rev = super::source_refs::record(&st.areas[&area_id], source)
        .unwrap()
        .rev;
    // The envelope's shape names exactly this Secret.
    let precondition = &envelope.preconditions[0];
    if precondition.expected_rev != current_rev {
        return err_with_details(
            409,
            "revision_conflict",
            json!({
                "resource": "source",
                "id": area_id,
                "source": wire,
                "expected_rev": precondition.expected_rev,
                "current_rev": current_rev,
                "operation_id": envelope.operation_id,
            }),
        );
    }

    // Other maps ride along for destination checks only: without their
    // exits, so nothing the appliers do to "this map's room n" can reach an
    // exit that names the map's room n rather than the Secret's.
    let area = &st.areas[&area_id];
    let record = super::source_refs::record(area, source).unwrap();
    let mut doc = AreaRecord::new(area.id, area.user_id, area.atlas_id, area.name.clone(), 0);
    doc.properties = record.properties.clone();
    doc.rooms = record.rooms.clone();
    doc.exits.clone_from(&record.exits);
    doc.connections.clone_from(&record.connections);
    doc.labels.clone_from(&record.labels);
    doc.shapes.clone_from(&record.shapes);
    let mut working: Working = st
        .areas
        .iter()
        .filter(|(id, _)| **id != area_id)
        .map(|(id, other)| {
            let mut other = other.clone();
            other.exits.clear();
            other.secrets.clear();
            (*id, other)
        })
        .collect();
    working.insert(area_id, doc);
    let ctx = ApplyCtx {
        viewer,
        area_id,
        writes_map: false,
        secret: Some(record.id),
        dirty: std::cell::RefCell::default(),
        released: std::cell::RefCell::default(),
    };
    let mut results: Vec<Value> = Vec::with_capacity(envelope.payload.len());
    let mut changed = false;
    for op in envelope.payload {
        let op = match qualified::input(st, &mut working, &ctx, op) {
            Ok(op) => op,
            Err(response) => return response,
        };
        match apply_op(st, &mut working, &ctx, op) {
            Ok(outcome) => {
                changed |= outcome.changed;
                results.push(qualified::output(
                    st,
                    &working[&area_id],
                    &ctx,
                    outcome.result,
                ));
            }
            Err(response) => return response,
        }
    }
    if let Err(response) = validate_connections(&working[&area_id], &ctx) {
        return response;
    }
    let doc = working.remove(&area_id).expect("the Secret's document");
    let secret =
        super::source_refs::record_mut(st.areas.get_mut(&area_id).unwrap(), source).unwrap();
    super::secrets::store_doc(secret, doc);
    if changed {
        secret.rev += 1;
    }
    let rev = secret.rev;
    // Other maps' exits into the Secret's rooms the write deleted go with
    // them, moving no revision (format-3 §4.2).
    let released = ctx.released.take();
    if !released.is_empty() {
        let secret_id = ctx.secret.expect("a Secret's write");
        super::secrets::drop_exits_into_secret(&mut st.areas, area_id, secret_id, Some(&released));
    }
    let result = json!({
        "operation_id": envelope.operation_id,
        "versions": [{
            "resource": "source",
            "id": area_id,
            "source": wire,
            "rev": rev,
            "deleted": false,
        }],
        "data": results,
    });
    st.mutation_receipts.insert(
        (viewer, envelope.operation_id),
        MutationReceipt {
            request_hash,
            result: result.clone(),
        },
    );
    st.mutation_log.push((envelope.operation_id, false));
    finish(st, ok(result))
}

fn not_modeled(what: &str) -> Response {
    err(501, &format!("the mock does not model {what}"))
}

/// Applies the response-drop test hook: a queued drop swallows this (fully
/// committed) response and serves a 500 in its place.
fn finish(st: &mut MockState, response: Response) -> Response {
    if st.drop_mutation_responses > 0 {
        st.drop_mutation_responses -= 1;
        return err(500, "injected response loss");
    }
    response
}

fn apply_op(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    op: AreaOp,
) -> Result<OpOutcome, Response> {
    match op {
        AreaOp::AssertMergeSafe {
            keep_room_number,
            remove_room_number,
            ..
        } => {
            // An exit into another map's Secret room never counts: it goes
            // with the room, as a Secret's data does. Nor does an exit from a
            // Member-owned map the writer does not read.
            let has_foreign_link = working.iter().any(|(origin_area, area)| {
                let counts = *origin_area == ctx.area_id
                    || area.member_owned.is_none()
                    || st
                        .caps(ctx.viewer, *origin_area)
                        .is_some_and(|caps| caps.can_view);
                counts
                    && area.exits.iter().any(|exit| {
                        exit.to_secret.is_none()
                            && ((*origin_area == ctx.area_id
                                && exit.from_room_number == remove_room_number
                                && exit.to_area_id.is_some_and(|target| target != ctx.area_id))
                                || (*origin_area != ctx.area_id
                                    && exit.to_area_id == Some(ctx.area_id)
                                    && exit.to_room_number == Some(remove_room_number)))
                    })
            });
            if has_foreign_link {
                return Err(err_with_details(
                    409,
                    "structural_conflict",
                    json!({ "reason": "merge_cross_area_links" }),
                ));
            }
            Ok(OpOutcome {
                result: json!({
                    "entity": "merge_safety_checked",
                    "keep_room_number": keep_room_number,
                    "remove_room_number": remove_room_number,
                }),
                changed: false,
                foreign_bumps: Vec::new(),
                secret_bumps: Vec::new(),
            })
        }
        AreaOp::UpsertRoom {
            room_number, body, ..
        } => Ok(apply_upsert_room(st, working, ctx, room_number, &body)),
        AreaOp::CreateRoom {
            room_number, body, ..
        } => {
            if scope_area(working, ctx).rooms.contains_key(&room_number) {
                return Err(err_with_details(
                    409,
                    "structural_conflict",
                    json!({ "reason": "room_number_exists" }),
                ));
            }
            Ok(apply_upsert_room(st, working, ctx, room_number, &body))
        }
        AreaOp::DeleteRoom { room_number, .. } => apply_delete_room(working, ctx, room_number),
        AreaOp::UpsertRoomProperty {
            room_number,
            name,
            value,
            ..
        } => apply_upsert_room_property(working, ctx, room_number, name, &value),
        AreaOp::DeleteRoomProperty {
            room_number, name, ..
        } => apply_delete_room_property(working, ctx, room_number, name),
        AreaOp::AddRoomTag {
            room_number, tag, ..
        } => apply_room_tag(working, ctx, room_number, &tag, true),
        AreaOp::RemoveRoomTag {
            room_number, tag, ..
        } => apply_room_tag(working, ctx, room_number, &tag, false),
        AreaOp::UpsertAreaProperty { name, value } => {
            Ok(apply_upsert_area_property(working, ctx, name, &value))
        }
        AreaOp::DeleteAreaProperty { name } => apply_delete_area_property(working, ctx, name),
        AreaOp::CreateExit {
            room_number, body, ..
        } => apply_create_exit(st, working, ctx, room_number, body),
        AreaOp::UpdateExit { exit_id, body } => apply_update_exit(st, working, ctx, exit_id, body),
        AreaOp::DeleteExit { exit_id } => apply_delete_exit(st, working, ctx, exit_id),
        AreaOp::CreateConnection { body } => apply_create_connection(st, working, ctx, body),
        AreaOp::UpdateConnection {
            connection_id,
            body,
        } => apply_update_connection(st, working, ctx, connection_id, body),
        AreaOp::Pair {
            keep_connection_id,
            merge_connection_id,
        } => apply_pair(st, working, ctx, keep_connection_id, merge_connection_id),
        AreaOp::Unlink {
            exit_id,
            new_connection_id,
        } => apply_unlink(st, working, ctx, exit_id, new_connection_id),
        AreaOp::DeleteLink { connection_id } => apply_delete_link(st, working, ctx, connection_id),
        AreaOp::CreateLabel { body } => apply_create_label(st, working, ctx, body),
        AreaOp::UpdateLabel { label_id, body } => apply_update_label(working, ctx, label_id, &body),
        AreaOp::DeleteLabel { label_id } => apply_delete_label(working, ctx, label_id),
        AreaOp::CreateShape { body } => apply_create_shape(st, working, ctx, body),
        AreaOp::UpdateShape { shape_id, body } => apply_update_shape(working, ctx, shape_id, &body),
        AreaOp::DeleteShape { shape_id } => apply_delete_shape(working, ctx, shape_id),
    }
}

fn scope_area<'a>(working: &'a mut Working, ctx: &ApplyCtx) -> &'a mut AreaRecord {
    working
        .get_mut(&ctx.area_id)
        .expect("scope area presence was proven by authorization")
}

/// One room's echo (`ops.rs` room result): the full record, with exits into
/// maps the caller cannot read redacted.
fn room_echo(st: &MockState, working: &Working, ctx: &ApplyCtx, room_number: i32) -> Value {
    let area = &working[&ctx.area_id];
    let room = &area.rooms[&room_number];
    let properties: Vec<Value> = room
        .properties
        .iter()
        .map(|(name, p)| json!({"name": name, "value": p.value}))
        .collect();
    let exits = project_room_exits(st, working, ctx, room_number);
    json!({
        "area_id": ctx.area_id,
        "room_number": room.room_number,
        "title": room.title,
        "description": room.description,
        "color": room.color,
        "level": room.level,
        "x": room.x,
        "y": room.y,
        "created_at": room.created_at,
        "external_id": room.external_id,
        "properties": properties,
        "exits": exits,
    })
}

/// An exit echo as its writer may read it: a destination in a map they
/// cannot read is withheld (`to_area_id`, `to_room_number` and
/// `to_direction` null), as room echoes withhold it.
fn exit_echo(st: &MockState, ctx: &ApplyCtx, exit: &ExitRecord) -> Value {
    let (exit, readable) = st.resolved_exit(ctx.viewer, ctx.area_id, exit);
    let mut echo = embedded_exit_json(&exit);
    if !readable {
        echo["to_area_id"] = Value::Null;
        echo["to_room_number"] = Value::Null;
        echo["to_direction"] = Value::Null;
        echo.as_object_mut().unwrap().remove("to_source");
    } else if let Some(source) = exit.to_secret {
        echo["to_source"] = json!(st.destination_source(exit.to_area_id.unwrap(), source));
    }
    echo
}

/// The kinds of client-minted content id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentKind {
    Exit,
    Connection,
    Label,
    Shape,
}

fn holds(
    kind: ContentKind,
    id: Uuid,
    exits: &[ExitRecord],
    connections: &[super::state::ConnectionRecord],
    labels: &[LabelRecord],
    shapes: &[ShapeRecord],
) -> bool {
    match kind {
        ContentKind::Exit => exits.iter().any(|exit| exit.id == id),
        ContentKind::Connection => connections.iter().any(|connection| connection.id == id),
        ContentKind::Label => labels.iter().any(|label| label.id == id),
        ContentKind::Shape => shapes.iter().any(|shape| shape.id == id),
    }
}

/// Rejects duplicate UUIDs within the written Library, regardless of visibility.
/// A known-ID existence signal is accepted; no existing object is renamed.
fn claim_id(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    kind: ContentKind,
    id: Uuid,
) -> Result<(), Response> {
    let taken = || match kind {
        ContentKind::Exit => bad_request("an exit with that ID already exists"),
        ContentKind::Connection => err_with_details(
            422,
            "invalid_connection",
            json!({ "reason": "duplicate_connection" }),
        ),
        ContentKind::Label => bad_request("a label with that ID already exists"),
        ContentKind::Shape => bad_request("a shape with that ID already exists"),
    };
    let written = &working[&ctx.area_id];
    if holds(
        kind,
        id,
        &written.exits,
        &written.connections,
        &written.labels,
        &written.shapes,
    ) {
        return Err(taken());
    }
    for (area_id, original) in &st.areas {
        if !same_library(&st.areas, ctx.area_id, *area_id) {
            continue;
        }
        let area = if ctx.writes_map {
            &working[area_id]
        } else {
            original
        };
        if !(*area_id == ctx.area_id && ctx.writes_map)
            && holds(
                kind,
                id,
                &area.exits,
                &area.connections,
                &area.labels,
                &area.shapes,
            )
        {
            return Err(taken());
        }
        for source in area.secrets.iter().chain(area.private_sources.values()) {
            if *area_id == ctx.area_id && Some(source.id) == ctx.secret {
                continue;
            }
            if holds(
                kind,
                id,
                &source.exits,
                &source.connections,
                &source.labels,
                &source.shapes,
            ) {
                return Err(taken());
            }
        }
    }
    Ok(())
}

fn content_ids(area: &AreaRecord, kind: ContentKind) -> BTreeSet<Uuid> {
    let lists = std::iter::once((&area.exits, &area.connections, &area.labels, &area.shapes))
        .chain(
            area.secrets
                .iter()
                .chain(area.private_sources.values())
                .map(|source| {
                    (
                        &source.exits,
                        &source.connections,
                        &source.labels,
                        &source.shapes,
                    )
                }),
        );
    let mut ids = BTreeSet::new();
    for (exits, connections, labels, shapes) in lists {
        match kind {
            ContentKind::Exit => ids.extend(exits.iter().map(|item| item.id)),
            ContentKind::Connection => ids.extend(connections.iter().map(|item| item.id)),
            ContentKind::Label => ids.extend(labels.iter().map(|item| item.id)),
            ContentKind::Shape => ids.extend(shapes.iter().map(|item| item.id)),
        }
    }
    ids
}

/// The mock commits transfers under one lock; check before changing ownership.
pub(super) fn transfer_ids_available(
    st: &MockState,
    maps: &[Uuid],
    owner: Uuid,
    clan: Option<Uuid>,
) -> Result<(), Response> {
    for kind in [
        ContentKind::Exit,
        ContentKind::Connection,
        ContentKind::Label,
        ContentKind::Shape,
    ] {
        let incoming: BTreeSet<_> = maps
            .iter()
            .flat_map(|id| content_ids(&st.areas[id], kind))
            .collect();
        for area in st.areas.values().filter(|area| {
            !maps.contains(&area.id)
                && area.clan_id == clan
                && (clan.is_some() || area.user_id == owner)
        }) {
            if !incoming.is_disjoint(&content_ids(area, kind)) {
                return Err(bad_request("a content object with that ID already exists"));
            }
        }
    }
    Ok(())
}

/// One room's projected exits (`MapQueries::project_room_exits`): an exit
/// into a non-viewable area keeps its row but has its destination nulled.
fn project_room_exits(
    st: &MockState,
    working: &Working,
    ctx: &ApplyCtx,
    room_number: i32,
) -> Vec<Value> {
    let area = &working[&ctx.area_id];
    let mut out = Vec::new();
    for exit in area
        .exits
        .iter()
        .filter(|e| e.from_room_number == room_number && st.shows_exit(ctx.viewer, e))
    {
        let projected = exit_echo(st, ctx, exit);
        out.push(projected);
    }
    out
}

/// Whether two maps live in one Library: both one clan's own maps, or both
/// one user's.
fn same_library(working: &Working, a: Uuid, b: Uuid) -> bool {
    let holder = |id: Uuid| {
        working
            .get(&id)
            .map(|area| (area.clan_id, area.clan_id.is_none().then_some(area.user_id)))
    };
    holder(a).is_some() && holder(a) == holder(b)
}

/// The write-side fact round (format-3 §4): whether `viewer` reads a map
/// another Library holds, as that Library judges it, and whether it holds
/// `to_room` among its map rooms (or no room is named). That Library knows
/// ownership, shares and a clan's grants on its own maps, so a map read
/// only through a clan's link counts as unread. Anything else is the
/// uniform 404, exactly as for a map that does not exist.
fn read_elsewhere(
    st: &MockState,
    working: &Working,
    viewer: Uuid,
    to_area: Uuid,
    to_room: Option<i32>,
) -> Result<(), Response> {
    let Some(area) = working.get(&to_area) else {
        return Err(not_found());
    };
    let reads = if area.clan_id.is_some() {
        st.caps(viewer, to_area).is_some_and(|caps| caps.can_view)
    } else {
        area.user_id == viewer
            || st
                .grants
                .iter()
                .any(|grant| grant.grantee_id == viewer && grant.covers_area(area))
    };
    let holds = to_room.is_none_or(|room| area.rooms.contains_key(&room));
    if reads && holds {
        Ok(())
    } else {
        Err(not_found())
    }
}

/// A map revision an exit into `to` moves: only a map this Library holds.
/// Whether an exit change moves `to`'s revision: another map in the same
/// Library, unless the written map is Member-owned, whose exits move none
/// of the maps they lead into (format-3.md §4, clans.md §7.7).
fn bumps_target(working: &Working, ctx: &ApplyCtx, to: Uuid) -> bool {
    to != ctx.area_id
        && same_library(working, ctx.area_id, to)
        && working
            .get(&ctx.area_id)
            .is_none_or(|area| area.member_owned.is_none())
}

/// Whether the destination names a room that `to` lacks, so that the write
/// creates a placeholder there, which moves that map. A Member-owned map's
/// write never gets this far: it creates no placeholder.
fn makes_placeholder(
    working: &Working,
    ctx: &ApplyCtx,
    to: Option<Uuid>,
    room: Option<i32>,
) -> bool {
    match (to, room) {
        (Some(to), Some(room)) => {
            to != ctx.area_id
                && same_library(working, ctx.area_id, to)
                && working
                    .get(&to)
                    .is_some_and(|area| !area.rooms.contains_key(&room))
        }
        _ => false,
    }
}

/// The destination matrix (`resolve_exit_destination_tx`), against the
/// working copy and without revision side effects: a viewable target, then
/// link to an existing room or create a placeholder (which needs edit
/// rights on a foreign target). A map another Library holds is reached
/// only through the fact round: no placeholder there, nothing moves there.
fn resolve_destination(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    to_area_id: Uuid,
    to_room_number: Option<i32>,
) -> Result<(), Response> {
    let caps = st.caps(ctx.viewer, to_area_id).unwrap_or(Caps::NONE);
    let same_area = to_area_id == ctx.area_id;
    if !same_area && !same_library(working, ctx.area_id, to_area_id) {
        return read_elsewhere(st, working, ctx.viewer, to_area_id, to_room_number);
    }

    if !same_area && !caps.can_view {
        return Err(not_found());
    }
    let Some(to_room) = to_room_number else {
        return Ok(());
    };

    let exists = working
        .get(&to_area_id)
        .is_some_and(|a| a.rooms.contains_key(&to_room));
    if exists {
        return Ok(());
    }
    // A placeholder in another map needs a map write and the right to add
    // to that map; a Secret's write reaches nothing outside the Secret, and
    // a Member-owned map's exits create none (clans.md §7.7).
    let member_owned = working
        .get(&ctx.area_id)
        .is_some_and(|area| area.member_owned.is_some());
    if !same_area && (!caps.can_edit || !ctx.writes_map || member_owned) {
        return Err(not_found());
    }
    if let Some(area) = working.get_mut(&to_area_id) {
        area.rooms.insert(to_room, RoomRecord::placeholder(to_room));
    }
    Ok(())
}

fn apply_upsert_room(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    body: &UpsertRoomBody,
) -> OpOutcome {
    {
        let area = scope_area(working, ctx);
        match area.rooms.get_mut(&room_number) {
            Some(room) => {
                if let Some(title) = body.title.clone() {
                    room.title = title;
                }
                if let Some(description) = body.description.clone() {
                    room.description = description;
                }
                if let Some(level) = body.level {
                    room.level = level;
                }
                if let Some(x) = body.x {
                    room.x = x;
                }
                if let Some(y) = body.y {
                    room.y = y;
                }
                if let Some(color) = body.color.clone() {
                    room.color = color;
                }
                if let Some(binding) = body.external_id.clone() {
                    room.external_id = binding;
                }
            }
            None => {
                area.rooms.insert(
                    room_number,
                    RoomRecord {
                        identity: Uuid::new_v4(),
                        room_number,
                        title: body.title.clone().unwrap_or_default(),
                        description: body.description.clone().unwrap_or_default(),
                        level: body.level.unwrap_or(0),
                        x: body.x.unwrap_or(0.0),
                        y: body.y.unwrap_or(0.0),
                        color: body.color.clone().unwrap_or_default(),
                        external_id: body.external_id.clone().flatten(),
                        ..RoomRecord::placeholder(room_number)
                    },
                );
            }
        }
    }
    OpOutcome::scoped(json!({"entity": "room", "room": room_echo(st, working, ctx, room_number)}))
}

fn apply_delete_room(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
) -> Result<OpOutcome, Response> {
    if !working
        .get(&ctx.area_id)
        .is_some_and(|a| a.rooms.contains_key(&room_number))
    {
        return Err(not_found());
    }

    // Cross-area effects, gathered before the rows change: outbound exit
    // targets (their rows cascade away) and inbound exit origins (their
    // rows lose destinations).
    // Exits into other maps' Secret rooms are hidden content: they go with
    // the room and move nothing.
    // A Member-owned map's exits move none of the maps they lead into.
    let mut partners: BTreeSet<Uuid> = BTreeSet::new();
    let member_owned = working[&ctx.area_id].member_owned.is_some();
    for exit in &working[&ctx.area_id].exits {
        if !member_owned
            && exit.from_room_number == room_number
            && exit.to_secret.is_none()
            && let Some(to_area) = exit.to_area_id
            && to_area != ctx.area_id
        {
            partners.insert(to_area);
        }
    }
    for (host_id, host) in working.iter() {
        if *host_id == ctx.area_id {
            continue;
        }
        if host.exits.iter().any(|exit| {
            exit.to_secret.is_none()
                && exit.to_area_id == Some(ctx.area_id)
                && exit.to_room_number == Some(room_number)
        }) {
            partners.insert(*host_id);
        }
    }
    // A Secret's own room that goes takes other maps' exits into it along.
    if ctx.secret.is_some() && map_room_of(room_number).is_none() {
        ctx.released.borrow_mut().insert(room_number);
    }

    // §3.3 repair, in the server's order: outgoing exits removed, inbound
    // destinations (any area) nulled, this area's touched Connections
    // converted to dangling or removed as orphans — then the room itself.
    connections::repair_after_room_delete(working, ctx.area_id, room_number);
    scope_area(working, ctx).rooms.remove(&room_number);

    // A map room takes every source's hold on it along: the map's Secrets
    // lose their data and exits on it, and every Secret's exits into it,
    // this map's or another's, lose their destination. Each such Secret
    // moves its revision; only its readers ever hear of it.
    let mut secret_bumps = Vec::new();
    if ctx.writes_map {
        for secret in &mut scope_area(working, ctx).secrets {
            if super::secrets::forget_map_room(secret, ctx.area_id, room_number) {
                secret_bumps.push((ctx.area_id, secret.id));
            }
        }
        secret_bumps.extend(super::secrets::forget_inbound(
            working,
            ctx.area_id,
            room_number,
        ));
    }

    Ok(OpOutcome {
        result: json!({"entity": "room_deleted", "room_number": room_number}),
        changed: true,
        foreign_bumps: partners.into_iter().collect(),
        secret_bumps,
    })
}

fn apply_upsert_room_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    name: String,
    value: &str,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return Err(err(404, "Room not found"));
    };
    room.properties.insert(
        name.clone(),
        RoomPropRecord {
            value: value.to_string(),
        },
    );
    Ok(OpOutcome::scoped(
        json!({"entity": "room_property", "room_number": room_number, "name": name}),
    ))
}

fn apply_delete_room_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    name: String,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return Err(err(404, "Room not found"));
    };
    if room.properties.remove(&name).is_none() {
        return Err(err(404, "Room property not found"));
    }
    Ok(OpOutcome::scoped(
        json!({"entity": "room_property_deleted", "room_number": room_number, "name": name}),
    ))
}

/// Add or remove one room tag, normalized trim+UPPERCASE (`ops.rs`
/// `normalize_tag`). An actual insert or removal changes the map; an
/// idempotent re-add changes nothing and must not move any counter.
fn apply_room_tag(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    tag: &str,
    add: bool,
) -> Result<OpOutcome, Response> {
    let normalized = normalize_tag(tag)?;
    let area = scope_area(working, ctx);
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return Err(err(404, "Room not found"));
    };
    if add {
        let inserted = room.tags.insert(normalized.clone());
        Ok(OpOutcome {
            result: json!({"entity": "room_tag", "room_number": room_number, "tag": normalized}),
            changed: inserted,
            foreign_bumps: Vec::new(),
            secret_bumps: Vec::new(),
        })
    } else {
        // The deployed server 404s an absent tag and rolls the envelope
        // back (`delete_room_tag_in_tx` checks `rows_affected`).
        if !room.tags.remove(&normalized) {
            return Err(err(404, "Not found"));
        }
        Ok(OpOutcome::scoped(
            json!({"entity": "room_tag_removed", "room_number": room_number, "tag": normalized}),
        ))
    }
}

fn apply_upsert_area_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    name: String,
    value: &str,
) -> OpOutcome {
    scope_area(working, ctx).properties.insert(
        name.clone(),
        AreaPropRecord {
            value: value.to_string(),
            created_at: chrono::Utc::now(),
        },
    );
    OpOutcome::scoped(json!({"entity": "area_property", "name": name}))
}

fn apply_delete_area_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    name: String,
) -> Result<OpOutcome, Response> {
    if scope_area(working, ctx).properties.remove(&name).is_none() {
        return Err(err(404, "Property not found"));
    }
    Ok(OpOutcome::scoped(
        json!({"entity": "area_property_deleted", "name": name}),
    ))
}

fn apply_create_exit(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    body: CreateExitBody,
) -> Result<OpOutcome, Response> {
    check_enum(&body.from_direction, &DIRECTIONS, "direction")?;
    if let Some(d) = &body.to_direction {
        check_enum(d, &DIRECTIONS, "direction")?;
    }

    // A `to_source` with another map's `to_area_id` names a Secret there
    // (format-3 §4.4): admitted for a writer who reads it, holding the room.
    let foreign = body
        .to_source
        .as_deref()
        .filter(|_| body.to_area_id.is_some_and(|to| to != ctx.area_id))
        .and_then(|named| Uuid::parse_str(named).ok());
    let mut placeholder = false;
    if let Some(secret) = foreign {
        admit_foreign(
            st,
            ctx,
            body.to_area_id.expect("filtered above"),
            secret,
            body.to_room_number,
        )?;
    } else if let Some(to_area_id) = body.to_area_id {
        placeholder = makes_placeholder(working, ctx, Some(to_area_id), body.to_room_number);
        resolve_destination(st, working, ctx, to_area_id, body.to_room_number)?;
    }

    // From-room placeholder (always allowed — the caller can edit the host).
    scope_area(working, ctx)
        .rooms
        .entry(room_number)
        .or_insert_with(|| RoomRecord::placeholder(room_number));

    // The Connection carrying the new exit: auto-pair with the unique
    // reciprocal one-member candidate, or mint a one-member Connection with
    // §1.5 anchors and §4.3 port slots (mirrors `attach_for_new_exit`).
    if body.connection_id.is_some() && body.new_connection_id.is_some() {
        return Err(invalid_connection("ambiguous_connection"));
    }
    let connection_id = if let Some(connection_id) = body.connection_id {
        let shown = shown_connection(st, working, ctx, connection_id)?;
        if members(&working[&ctx.area_id], shown.id).len() >= 2 {
            return Err(invalid_connection("too_many_members"));
        }
        connection_id
    } else {
        if let Some(new_connection_id) = body.new_connection_id {
            claim_id(st, working, ctx, ContentKind::Connection, new_connection_id)?;
        }
        attach_for_new_exit(
            working,
            ctx.area_id,
            &NewExitLink {
                from_room: room_number,
                from_direction: body.from_direction.clone(),
                to_area_id: body.to_area_id,
                to_room_number: if foreign.is_some() {
                    None
                } else {
                    body.to_room_number
                },
                to_direction: body.to_direction.clone(),
                new_connection_id: body.new_connection_id,
            },
        )
    };
    ctx.dirty.borrow_mut().insert(connection_id);

    if let Some(id) = body.id {
        claim_id(st, working, ctx, ContentKind::Exit, id)?;
    }
    let exit = ExitRecord {
        to_room_identity: None,
        id: body.id.unwrap_or_else(Uuid::new_v4),
        from_room_number: room_number,
        from_direction: body.from_direction,
        to_area_id: body.to_area_id,
        to_room_number: body.to_room_number,
        to_direction: body.to_direction,
        path: body.path.unwrap_or_default(),
        is_hidden: body.is_hidden,
        door: body.door.map(DoorBody::record),
        weight: body.weight,
        command: body.command.unwrap_or_default(),
        connection_id,
        to_secret: foreign,
    };
    let result = json!({"entity": "exit", "exit": exit_echo(st, ctx, &exit)});
    let foreign_bumps = match exit.to_area_id {
        Some(to) if foreign.is_none() && (placeholder || bumps_target(working, ctx, to)) => {
            vec![to]
        }
        _ => Vec::new(),
    };
    scope_area(working, ctx).exits.push(exit);
    // An exit into another map's Secret room is hidden content: it moves no
    // revision, unless its room was created for it.
    Ok(OpOutcome {
        result,
        changed: true,
        foreign_bumps,
        secret_bumps: Vec::new(),
    })
}

/// The write-side check for an exit into room `room` of Secret `secret` on
/// map `map` (format-3 §4.4): the writer reads the Secret, it is on that
/// map and holds the room, and neither it nor the written Secret is a
/// clan's Secret on a map filed in the clan by link. Every refusal is the
/// uniform 404.
fn admit_foreign(
    st: &MockState,
    ctx: &ApplyCtx,
    map: Uuid,
    secret: Uuid,
    room: Option<i32>,
) -> Result<(), Response> {
    let Some(room) = room else {
        return Err(not_found());
    };
    let linked = |area: &AreaRecord, id: Uuid| {
        area.secrets
            .iter()
            .find(|s| s.id == id)
            .is_some_and(|candidate| super::clan_secrets::linked_clan(area, candidate).is_some())
    };
    let admitted = st.areas.get(&map).is_some_and(|area| {
        area.secrets
            .iter()
            .chain(area.private_sources.get(&ctx.viewer))
            .find(|s| s.id == secret)
            .is_some_and(|held| {
                st.reads_secret(ctx.viewer, map, secret)
                    && map_room_of(room).is_none()
                    && held.rooms.get(&room).is_some_and(|r| r.anchor.is_none())
                    && !linked(area, secret)
            })
    });
    let origin_linked = ctx
        .secret
        .is_some_and(|own| linked(&st.areas[&ctx.area_id], own));
    if admitted && !origin_linked {
        Ok(())
    } else {
        Err(not_found())
    }
}

/// Exit `exit_id` of the written source, when the writer is shown it; one
/// into a Secret room they are not shown does not exist, to them.
fn shown_exit(
    st: &MockState,
    working: &Working,
    ctx: &ApplyCtx,
    exit_id: Uuid,
) -> Result<ExitRecord, Response> {
    working[&ctx.area_id]
        .exits
        .iter()
        .find(|exit| exit.id == exit_id && st.shows_exit(ctx.viewer, exit))
        .cloned()
        .ok_or_else(not_found)
}

#[allow(clippy::too_many_lines)] // the server's updateExit, step by step
fn apply_update_exit(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    exit_id: Uuid,
    body: UpdateExitBody,
) -> Result<OpOutcome, Response> {
    if let Some(d) = &body.from_direction {
        check_enum(d, &DIRECTIONS, "direction")?;
    }
    if let Some(d) = &body.to_direction {
        check_enum(d, &DIRECTIONS, "direction")?;
    }

    let current = shown_exit(st, working, ctx, exit_id)?;

    let clear_to = body.clear_to.unwrap_or(false);
    let (new_to_area, new_to_room) = if clear_to {
        (None, None)
    } else {
        (
            body.to_area_id.or(current.to_area_id),
            body.to_room_number.or(current.to_room_number),
        )
    };
    // A `to_source` with another map's `to_area_id` names a Secret there;
    // an absent one keeps the destination's.
    let foreign_named = matches!(body.to_source, Some(Some(_)))
        && body.to_area_id.is_some_and(|to| to != ctx.area_id);
    let new_to_secret = if clear_to {
        None
    } else {
        match &body.to_source {
            None => current.to_secret,
            Some(Some(named)) if foreign_named => Uuid::parse_str(named).ok(),
            Some(_) => None,
        }
    };
    let new_from_direction = body
        .from_direction
        .clone()
        .unwrap_or_else(|| current.from_direction.clone());
    let new_to_direction = if clear_to {
        None
    } else {
        body.to_direction
            .clone()
            .or_else(|| current.to_direction.clone())
    };
    if new_to_room.is_some() && new_to_area.is_none() {
        return Err(bad_request(
            "invalid mutation envelope: `to_room_number` needs `to_area_id`",
        ));
    }
    if new_to_secret.is_some() && new_to_area == Some(ctx.area_id) {
        return Err(bad_request(
            "invalid mutation envelope: an exit into another map's Secret room changes map with `to_source`",
        ));
    }
    let destination_changed = (new_to_area, new_to_room, new_to_secret)
        != (
            current.to_area_id,
            current.to_room_number,
            current.to_secret,
        );
    let topology_changed = new_from_direction != current.from_direction
        || destination_changed
        || new_to_direction != current.to_direction;

    // §3.2: a member of a pair cannot be made non-reciprocal in place —
    // retargets and departure-direction changes on a two-member Connection
    // are refused with the unlink-then-edit prompt. Traversal-only fields
    // (path, command, weight, flags, door) stay editable. Field presence
    // alone is not a change: redundant full snapshots are valid.
    let member_count = members(&working[&ctx.area_id], current.connection_id).len();
    if member_count == 2 && topology_changed {
        return Err(err_with_details(
            409,
            "structural_conflict",
            json!({ "reason": "unlink_before_edit" }),
        ));
    }

    // A destination the body names is judged even when it is the exit's
    // current one: into a map the writer does not read, every room number
    // is the uniform 404, the current one included.
    let names_destination = !clear_to
        && (body.to_area_id.is_some() || body.to_room_number.is_some() || body.to_source.is_some());
    let placeholder = new_to_secret.is_none()
        && (destination_changed || names_destination)
        && makes_placeholder(working, ctx, new_to_area, new_to_room);
    if (destination_changed || names_destination)
        && let Some(to_area_id) = new_to_area
    {
        match new_to_secret {
            Some(secret) => admit_foreign(st, ctx, to_area_id, secret, new_to_room)?,
            None => resolve_destination(st, working, ctx, to_area_id, new_to_room)?,
        }
    }

    let updated = {
        let area = scope_area(working, ctx);
        let exit = area
            .exits
            .iter_mut()
            .find(|e| e.id == exit_id)
            .expect("existence checked above");
        exit.from_direction = new_from_direction;
        exit.to_area_id = new_to_area;
        exit.to_room_number = new_to_room;
        exit.to_direction = new_to_direction;
        exit.to_secret = new_to_secret;
        if destination_changed {
            exit.to_room_identity = None;
        }
        if let Some(p) = body.path {
            exit.path = p;
        }
        if let Some(v) = body.is_hidden {
            exit.is_hidden = v;
        }
        // A door is replaced whole; no door keeps no name or command.
        if let Some(door) = body.door {
            exit.door = door.map(DoorBody::record);
        }
        if let Some(v) = body.weight {
            exit.weight = v;
        }
        if let Some(c) = body.command {
            exit.command = c;
        }
        exit.clone()
    };

    // Any topology change on a (now guaranteed) one-member Connection
    // atomically maintains its endpoints per §3.2.
    if topology_changed {
        maintain_after_retarget(working, ctx.area_id, current.connection_id);
    }
    ctx.dirty.borrow_mut().insert(current.connection_id);

    // The old map target and, when it moved, the new one; an exit into
    // another map's Secret room moves nothing there.
    let was_foreign = current.to_secret.is_some();
    let mut foreign_bumps: Vec<Uuid> = Vec::new();
    if let Some(old_to) = current
        .to_area_id
        .filter(|to| !was_foreign && bumps_target(working, ctx, *to))
    {
        foreign_bumps.push(old_to);
    }
    if let Some(new_to) = updated.to_area_id.filter(|to| {
        updated.to_secret.is_none()
            && (placeholder || (bumps_target(working, ctx, *to) && Some(*to) != current.to_area_id))
    }) && !foreign_bumps.contains(&new_to)
    {
        foreign_bumps.push(new_to);
    }
    Ok(OpOutcome {
        result: json!({"entity": "exit", "exit": exit_echo(st, ctx, &updated)}),
        // Hidden content before and after moves nothing; leading there from
        // anywhere else, or back, is a change the source's readers see.
        changed: true,
        foreign_bumps,
        secret_bumps: Vec::new(),
    })
}

fn apply_delete_exit(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    exit_id: Uuid,
) -> Result<OpOutcome, Response> {
    let exit = shown_exit(st, working, ctx, exit_id)?;
    scope_area(working, ctx).exits.retain(|e| e.id != exit_id);
    // §3.3: the last member takes the Connection with it.
    cleanup_after_exit_delete(working, ctx.area_id, exit.connection_id);
    ctx.dirty.borrow_mut().insert(exit.connection_id);
    let foreign_bumps = match exit.to_area_id {
        Some(to) if bumps_target(working, ctx, to) => vec![to],
        _ => Vec::new(),
    };
    Ok(OpOutcome {
        result: json!({"entity": "exit_deleted", "exit_id": exit_id}),
        changed: true,
        foreign_bumps,
        secret_bumps: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Connection operations (the server's `links.ts`)
// ---------------------------------------------------------------------------

fn invalid_connection(reason: &str) -> Response {
    err_with_details(422, "invalid_connection", json!({ "reason": reason }))
}

/// The member exits of connection `id` in `area`.
fn members(area: &AreaRecord, id: Uuid) -> Vec<&ExitRecord> {
    area.exits
        .iter()
        .filter(|exit| exit.connection_id == id)
        .collect()
}

/// Whether connection `id`'s members are all exits into other maps' Secret
/// A connection of the written source as its writer may see it: one whose
/// members are all exits they are not shown does not exist, to them.
fn shown_connection(
    st: &MockState,
    working: &Working,
    ctx: &ApplyCtx,
    id: Uuid,
) -> Result<ConnectionRecord, Response> {
    let area = &working[&ctx.area_id];
    let connection = area
        .connections
        .iter()
        .find(|connection| connection.id == id)
        .cloned()
        .ok_or_else(|| invalid_connection("connection_not_found"))?;
    let members = members(area, id);
    if !members.is_empty() && members.iter().all(|exit| !st.shows_exit(ctx.viewer, exit)) {
        return Err(invalid_connection("connection_not_found"));
    }
    Ok(connection)
}

/// A room's place in the server's room order (`roomOrder`): map rooms
/// before a source's own rooms, then by number. In a Secret's document map
/// rooms are stand-ins.
fn room_order(ctx: &ApplyCtx, key: i32) -> (bool, i32) {
    match (ctx.secret, map_room_of(key)) {
        (Some(_), Some(map_room)) => (false, map_room),
        (Some(_), None) => (true, key),
        (None, _) => (false, key),
    }
}

const SIDES: [&str; 4] = ["North", "East", "South", "West"];
const PORT_MODES: [&str; 2] = ["AutoPinned", "Manual"];
const ROUTINGS: [&str; 4] = ["Stub", "Simple", "Manual", "Automatic"];
const SEGMENT_SHAPES: [&str; 2] = ["Direct", "Orthogonal"];
const CORNERS: [&str; 2] = ["Sharp", "Rounded"];
const DASHES: [&str; 3] = ["Solid", "Dashed", "Dotted"];

/// Puts a connection's endpoints in the server's order (`canonicalize`):
/// by room, then side and port on a self-loop; the route reverses with a
/// swap.
fn canonicalize(ctx: &ApplyCtx, connection: &mut ConnectionRecord) {
    let Some(b) = connection.endpoint_b.as_ref() else {
        return;
    };
    let a = &connection.endpoint_a;
    let side = |endpoint: &EndpointRecord| SIDES.iter().position(|s| *s == endpoint.side);
    let flip = match room_order(ctx, a.room_number).cmp(&room_order(ctx, b.room_number)) {
        std::cmp::Ordering::Equal => {
            side(a) > side(b) || (a.side == b.side && a.port_offset > b.port_offset)
        }
        order => order == std::cmp::Ordering::Greater,
    };
    if flip {
        let b = connection.endpoint_b.take().expect("checked above");
        connection.endpoint_b = Some(std::mem::replace(&mut connection.endpoint_a, b));
        connection.route_points.reverse();
    }
}

/// The checks the server makes of a connection's fields: enum values, its
/// endpoints' rooms in the written source, ports and thickness.
fn check_connection(
    working: &Working,
    ctx: &ApplyCtx,
    connection: &ConnectionRecord,
) -> Result<(), Response> {
    let area = &working[&ctx.area_id];
    for endpoint in std::iter::once(&connection.endpoint_a).chain(connection.endpoint_b.iter()) {
        check_enum(&endpoint.side, &SIDES, "side")?;
        check_enum(&endpoint.port_mode, &PORT_MODES, "port mode")?;
        if !area.rooms.contains_key(&endpoint.room_number)
            || !(0.0..=1.0).contains(&endpoint.port_offset)
        {
            return Err(invalid_connection("invalid_endpoint"));
        }
    }
    check_enum(&connection.routing, &ROUTINGS, "routing")?;
    check_enum(&connection.segment_shape, &SEGMENT_SHAPES, "segment shape")?;
    check_enum(&connection.corner, &CORNERS, "corner")?;
    check_enum(&connection.dash, &DASHES, "dash")?;
    if !(0.25..=8.0).contains(&connection.thickness) {
        return Err(invalid_connection("invalid_thickness"));
    }
    Ok(())
}

/// One connection's echo, as the projection shows it.
fn connection_echo(working: &Working, ctx: &ApplyCtx, id: Uuid) -> Value {
    let area = &working[&ctx.area_id];
    let connection = area
        .connections
        .iter()
        .find(|connection| connection.id == id)
        .expect("an echoed connection exists");
    super::projection::connection_json(area, connection)
}

fn apply_create_connection(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    body: CreateConnectionBody,
) -> Result<OpOutcome, Response> {
    let mut connection = ConnectionRecord {
        id: body.id,
        endpoint_a: body.endpoint_a.record(),
        endpoint_b: body.endpoint_b.as_ref().map(EndpointBody::record),
        routing: body.routing,
        segment_shape: body.segment_shape,
        corner: body.corner,
        route_points: body.route_points.iter().map(|p| (p.x, p.y)).collect(),
        dash: body.dash,
        color: body.color,
        thickness: body.thickness,
    };
    check_connection(working, ctx, &connection)?;
    canonicalize(ctx, &mut connection);
    claim_id(st, working, ctx, ContentKind::Connection, connection.id)?;
    let id = connection.id;
    scope_area(working, ctx).connections.push(connection);
    ctx.dirty.borrow_mut().insert(id);
    Ok(OpOutcome::scoped(
        json!({"entity": "connection", "connection": connection_echo(working, ctx, id)}),
    ))
}

fn apply_update_connection(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    connection_id: Uuid,
    body: UpdateConnectionBody,
) -> Result<OpOutcome, Response> {
    let current = shown_connection(st, working, ctx, connection_id)?;
    let mut updated = current.clone();
    if let Some(endpoint) = &body.endpoint_a {
        updated.endpoint_a = endpoint.record();
    }
    if let Some(endpoint) = &body.endpoint_b {
        updated.endpoint_b = Some(endpoint.record());
    }
    if updated.endpoint_a.room_number != current.endpoint_a.room_number
        || updated.endpoint_b.as_ref().map(|b| b.room_number)
            != current.endpoint_b.as_ref().map(|b| b.room_number)
    {
        return Err(invalid_connection("endpoint_room_immutable"));
    }
    if let Some(routing) = body.routing {
        updated.routing = routing;
    }
    if let Some(shape) = body.segment_shape {
        updated.segment_shape = shape;
    }
    if let Some(corner) = body.corner {
        updated.corner = corner;
    }
    if let Some(points) = body.route_points {
        updated.route_points = points.iter().map(|p| (p.x, p.y)).collect();
    }
    if let Some(dash) = body.dash {
        updated.dash = dash;
    }
    if let Some(color) = body.color {
        updated.color = color;
    }
    if let Some(thickness) = body.thickness {
        updated.thickness = thickness;
    }
    check_connection(working, ctx, &updated)?;
    canonicalize(ctx, &mut updated);
    let changed = true;
    if let Some(stored) = scope_area(working, ctx)
        .connections
        .iter_mut()
        .find(|connection| connection.id == connection_id)
    {
        *stored = updated;
    }
    ctx.dirty.borrow_mut().insert(connection_id);
    Ok(OpOutcome {
        result: json!({"entity": "connection", "connection": connection_echo(working, ctx, connection_id)}),
        changed,
        foreign_bumps: Vec::new(),
        secret_bumps: Vec::new(),
    })
}

/// The room an exit leads to in the written document, when it leads to one
/// there.
fn destination_in(area: &AreaRecord, exit: &ExitRecord) -> Option<i32> {
    (exit.to_area_id == Some(area.id) && exit.to_secret.is_none())
        .then_some(exit.to_room_number)
        .flatten()
}

/// Whether two exits are each other's return (the server's `reciprocal`).
fn reciprocal(area: &AreaRecord, a: &ExitRecord, b: &ExitRecord) -> bool {
    a.from_room_number != b.from_room_number
        && destination_in(area, a) == Some(b.from_room_number)
        && destination_in(area, b) == Some(a.from_room_number)
        && a.to_direction
            .as_ref()
            .is_none_or(|d| *d == b.from_direction)
        && b.to_direction
            .as_ref()
            .is_none_or(|d| *d == a.from_direction)
}

fn apply_pair(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    keep_id: Uuid,
    merge_id: Uuid,
) -> Result<OpOutcome, Response> {
    let keep = shown_connection(st, working, ctx, keep_id)?;
    let merge = shown_connection(st, working, ctx, merge_id)?;
    if keep.id == merge.id {
        return Err(invalid_connection("same_connection"));
    }
    let area = &working[&ctx.area_id];
    let (keep_members, merge_members) = (members(area, keep.id), members(area, merge.id));
    let ([kept], [merged]) = (keep_members.as_slice(), merge_members.as_slice()) else {
        return Err(invalid_connection("pair_requires_one_member"));
    };
    if !reciprocal(area, kept, merged) {
        return Err(invalid_connection("not_reciprocal"));
    }
    let merged = merged.id;
    let area = scope_area(working, ctx);
    if let Some(exit) = area.exits.iter_mut().find(|exit| exit.id == merged) {
        exit.connection_id = keep.id;
    }
    area.connections
        .retain(|connection| connection.id != merge.id);
    ctx.dirty.borrow_mut().remove(&merge.id);
    ctx.dirty.borrow_mut().insert(keep.id);
    Ok(OpOutcome::scoped(
        json!({"entity": "connection", "connection": connection_echo(working, ctx, keep.id)}),
    ))
}

/// The port an unlinked exit's new connection moves to, beside the old one.
fn nearby_port(value: f32) -> f32 {
    if value <= 0.9 {
        (value + 0.05).min(1.0 - smudgy_cloud::CORNER_INSET)
    } else {
        (value - 0.05).max(smudgy_cloud::CORNER_INSET)
    }
}

fn apply_unlink(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    exit_id: Uuid,
    new_connection_id: Uuid,
) -> Result<OpOutcome, Response> {
    let exit = shown_exit(st, working, ctx, exit_id)?;
    let old = shown_connection(st, working, ctx, exit.connection_id)?;
    if members(&working[&ctx.area_id], old.id).len() != 2 {
        return Err(invalid_connection("unlink_requires_pair"));
    }
    claim_id(st, working, ctx, ContentKind::Connection, new_connection_id)?;
    let mut clone = ConnectionRecord {
        id: new_connection_id,
        ..old.clone()
    };
    let from = exit.from_room_number;
    let nudged = if clone.endpoint_a.room_number == from {
        Some(&mut clone.endpoint_a)
    } else {
        clone
            .endpoint_b
            .as_mut()
            .filter(|endpoint| endpoint.room_number == from)
    };
    if let Some(endpoint) = nudged {
        endpoint.port_offset = nearby_port(endpoint.port_offset);
        endpoint.port_mode = "AutoPinned".to_string();
    }
    check_connection(working, ctx, &clone)?;
    canonicalize(ctx, &mut clone);
    let area = scope_area(working, ctx);
    area.connections.push(clone);
    if let Some(moved) = area.exits.iter_mut().find(|e| e.id == exit_id) {
        moved.connection_id = new_connection_id;
    }
    ctx.dirty.borrow_mut().insert(old.id);
    ctx.dirty.borrow_mut().insert(new_connection_id);
    Ok(OpOutcome::scoped(json!({
        "entity": "connections",
        "connections": [
            connection_echo(working, ctx, old.id),
            connection_echo(working, ctx, new_connection_id),
        ],
    })))
}

fn apply_delete_link(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    connection_id: Uuid,
) -> Result<OpOutcome, Response> {
    let loaded = shown_connection(st, working, ctx, connection_id)?;
    let area = &working[&ctx.area_id];
    let changed = true;
    let mut foreign_bumps: Vec<Uuid> = members(area, loaded.id)
        .iter()
        .filter(|exit| exit.to_secret.is_none())
        .filter_map(|exit| exit.to_area_id)
        .filter(|to| bumps_target(working, ctx, *to))
        .collect();
    foreign_bumps.dedup();
    let area = scope_area(working, ctx);
    area.exits.retain(|exit| exit.connection_id != loaded.id);
    area.connections
        .retain(|connection| connection.id != loaded.id);
    ctx.dirty.borrow_mut().remove(&loaded.id);
    Ok(OpOutcome {
        result: json!({"entity": "connection_deleted", "connection_id": loaded.id}),
        changed,
        foreign_bumps,
        secret_bumps: Vec::new(),
    })
}

/// Validates every connection the envelope touched, in id order, as the
/// server does (`validateMembership`): one or two members; a one-ended
/// connection's single member leaves its room for no room of this
/// document; a self-loop's single member leaves and enters its room; any
/// other's members run between its two rooms, two of them reciprocal.
fn validate_connections(area: &AreaRecord, ctx: &ApplyCtx) -> Result<(), Response> {
    for id in ctx.dirty.borrow().iter() {
        let Some(connection) = area.connections.iter().find(|c| c.id == *id) else {
            continue;
        };
        let members = members(area, *id);
        if members.is_empty() {
            return Err(invalid_connection("no_members"));
        }
        if members.len() > 2 {
            return Err(invalid_connection("too_many_members"));
        }
        let a = connection.endpoint_a.room_number;
        let first = members[0];
        let valid = match connection.endpoint_b.as_ref().map(|b| b.room_number) {
            Some(b) if b == a => {
                members.len() == 1
                    && first.from_room_number == a
                    && destination_in(area, first) == Some(a)
            }
            Some(b) => {
                members.iter().all(|member| {
                    let to = destination_in(area, member);
                    (member.from_room_number == a && to == Some(b))
                        || (member.from_room_number == b && to == Some(a))
                }) && (members.len() == 1 || reciprocal(area, members[0], members[1]))
            }
            None => {
                members.len() == 1
                    && first.from_room_number == a
                    && destination_in(area, first).is_none()
            }
        };
        if !valid {
            return Err(invalid_connection("invalid_membership"));
        }
    }
    Ok(())
}

fn apply_create_label(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    body: CreateLabelBody,
) -> Result<OpOutcome, Response> {
    if let Some(id) = body.id {
        claim_id(st, working, ctx, ContentKind::Label, id)?;
    }
    check_enum(&body.horizontal_alignment, &H_ALIGN, "horizontal alignment")?;
    check_enum(&body.vertical_alignment, &V_ALIGN, "vertical alignment")?;
    let label = LabelRecord {
        id: body.id.unwrap_or_else(Uuid::new_v4),
        level: body.level.unwrap_or(0),
        x: body.x,
        y: body.y,
        width: body.width,
        height: body.height,
        horizontal_alignment: body.horizontal_alignment,
        vertical_alignment: body.vertical_alignment,
        text: body.text,
        color: body.color.unwrap_or_else(|| "black".to_string()),
        background_color: body.background_color.unwrap_or_else(|| "white".to_string()),
        font_size: body.font_size.unwrap_or(12),
        font_weight: body.font_weight.unwrap_or(400),
    };
    let result = json!({"entity": "label", "label": embedded_label_json(&label)});
    scope_area(working, ctx).labels.push(label);
    Ok(OpOutcome::scoped(result))
}

fn apply_update_label(
    working: &mut Working,
    ctx: &ApplyCtx,
    label_id: Uuid,
    body: &UpdateLabelBody,
) -> Result<OpOutcome, Response> {
    if let Some(a) = &body.horizontal_alignment {
        check_enum(a, &H_ALIGN, "horizontal alignment")?;
    }
    if let Some(a) = &body.vertical_alignment {
        check_enum(a, &V_ALIGN, "vertical alignment")?;
    }
    let area = scope_area(working, ctx);
    let Some(label) = area.labels.iter_mut().find(|l| l.id == label_id) else {
        return Err(err(404, "Label not found"));
    };
    if let Some(v) = body.level {
        label.level = v;
    }
    if let Some(v) = body.x {
        label.x = v;
    }
    if let Some(v) = body.y {
        label.y = v;
    }
    if let Some(v) = body.width {
        label.width = v;
    }
    if let Some(v) = body.height {
        label.height = v;
    }
    if let Some(v) = body.horizontal_alignment.clone() {
        label.horizontal_alignment = v;
    }
    if let Some(v) = body.vertical_alignment.clone() {
        label.vertical_alignment = v;
    }
    if let Some(v) = body.text.clone() {
        label.text = v;
    }
    if let Some(v) = body.color.clone() {
        label.color = v;
    }
    if let Some(v) = body.background_color.clone() {
        label.background_color = v;
    }
    if let Some(v) = body.font_size {
        label.font_size = v;
    }
    if let Some(v) = body.font_weight {
        label.font_weight = v;
    }
    Ok(OpOutcome::scoped(
        json!({"entity": "label", "label": embedded_label_json(label)}),
    ))
}

fn apply_delete_label(
    working: &mut Working,
    ctx: &ApplyCtx,
    label_id: Uuid,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(idx) = area.labels.iter().position(|l| l.id == label_id) else {
        return Err(err(404, "Label not found"));
    };
    area.labels.remove(idx);
    Ok(OpOutcome::scoped(
        json!({"entity": "label_deleted", "label_id": label_id}),
    ))
}

fn apply_create_shape(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    body: CreateShapeBody,
) -> Result<OpOutcome, Response> {
    if let Some(id) = body.id {
        claim_id(st, working, ctx, ContentKind::Shape, id)?;
    }
    check_enum(&body.shape_type, &SHAPE_TYPES, "shape type")?;
    let shape = ShapeRecord {
        id: body.id.unwrap_or_else(Uuid::new_v4),
        level: body.level.unwrap_or(0),
        x: body.x,
        y: body.y,
        width: body.width,
        height: body.height,
        background_color: Some(body.background_color.unwrap_or_else(|| "grey".to_string())),
        stroke_color: Some(
            body.stroke_color
                .unwrap_or_else(|| "transparent".to_string()),
        ),
        shape_type: body.shape_type,
        border_radius: body.border_radius.unwrap_or(0.0),
        stroke_width: body.stroke_width.unwrap_or(1.0),
    };
    let result = json!({"entity": "shape", "shape": embedded_shape_json(&shape)});
    scope_area(working, ctx).shapes.push(shape);
    Ok(OpOutcome::scoped(result))
}

fn apply_update_shape(
    working: &mut Working,
    ctx: &ApplyCtx,
    shape_id: Uuid,
    body: &UpdateShapeBody,
) -> Result<OpOutcome, Response> {
    if let Some(t) = &body.shape_type {
        check_enum(t, &SHAPE_TYPES, "shape type")?;
    }
    let area = scope_area(working, ctx);
    let Some(shape) = area.shapes.iter_mut().find(|s| s.id == shape_id) else {
        return Err(err(404, "Shape not found"));
    };
    if let Some(v) = body.level {
        shape.level = v;
    }
    if let Some(v) = body.x {
        shape.x = v;
    }
    if let Some(v) = body.y {
        shape.y = v;
    }
    if let Some(v) = body.width {
        shape.width = v;
    }
    if let Some(v) = body.height {
        shape.height = v;
    }
    if let Some(v) = body.background_color.clone() {
        shape.background_color = Some(v);
    }
    if let Some(v) = body.stroke_color.clone() {
        shape.stroke_color = Some(v);
    }
    if let Some(v) = body.shape_type.clone() {
        shape.shape_type = v;
    }
    if let Some(v) = body.radius {
        shape.border_radius = v;
    }
    if let Some(v) = body.stroke_width {
        shape.stroke_width = v;
    }
    Ok(OpOutcome::scoped(
        json!({"entity": "shape", "shape": embedded_shape_json(shape)}),
    ))
}

fn apply_delete_shape(
    working: &mut Working,
    ctx: &ApplyCtx,
    shape_id: Uuid,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(idx) = area.shapes.iter().position(|s| s.id == shape_id) else {
        return Err(err(404, "Shape not found"));
    };
    area.shapes.remove(idx);
    Ok(OpOutcome::scoped(
        json!({"entity": "shape_deleted", "shape_id": shape_id}),
    ))
}
