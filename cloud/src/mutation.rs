//! The client half of the mapper mutation envelope — the mirrored (not
//! shared; see the repo AGENTS.md) wire contract of `smudgy-api`'s
//! `mapping::contract` and `mapping::ops`.
//!
//! Every mapper content write compiles down to a [`MutationEnvelope`] of
//! ordered [`AreaMutation`]s that writes one source of one map, conditioned
//! on that source's revision (map wire format 3). The cloud backend submits envelopes to
//! `POST /areas/{id}/mutations`; local and ephemeral backends apply the same
//! operations under their own compare-and-set, so behavior cannot drift
//! between tiers.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Connection, ConnectionArgs, ConnectionId, ConnectionUpdates, Exit, ExitArgs, ExitId,
    ExitUpdates, Label, LabelArgs, LabelId, LabelUpdates, RoomNumber, RoomUpdates, RoomWithDetails,
    Shape, ShapeArgs, ShapeId, ShapeUpdates, SourceId,
};

/// Most operations one envelope may carry (server-enforced; mirrored so
/// callers can split oversized batches before submission). One authority:
/// the contract constant beside the Connection limits.
pub use crate::connection::MAX_MUTATION_OPERATIONS;

/// A client-generated operation identity: minted before enqueue, carried on
/// the wire, echoed in results, and the key of the server's idempotency
/// receipt — one retry with the same id can never double-apply.
pub type OperationId = Uuid;

/// The outer envelope of one mapper mutation. It writes one source of the
/// addressed map; the map itself unless `source` says otherwise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationEnvelope {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "SourceId::is_map")]
    pub source: SourceId,
    #[serde(default)]
    pub preconditions: Vec<Precondition>,
    pub payload: Vec<AreaMutation>,
}

/// `POST /areas/{id}/moves`: moves rooms, links, labels and shapes between
/// two sources of one map, conditioned on both sources' revisions. A room
/// takes the number it asks for (its own unless its entry names another)
/// unless `to` already uses it or an earlier room took it; then it takes
/// `to`'s next free number, in the order `rooms` lists them. Every exit and
/// connection touching a moved room travels with it, under the new numbers.
/// A connection in `connections` moves alone with its exits, keeping its
/// id, geometry and theirs, retaining qualified references to readable
/// same-map anchor rooms in other sources. At least one of the four lists
/// names something. The response is a [`MoveResult`] ([`parse_move_result`])
/// whose versions name `from`, then `to`, then any other source the move
/// changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveRequest {
    pub operation_id: OperationId,
    pub from: SourceId,
    pub to: SourceId,
    pub preconditions: Vec<Precondition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rooms: Vec<MovedRoom>,
    /// Links moved on their own; omitted when empty, so a move naming none
    /// hashes as one written before moves took them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ConnectionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<LabelId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shapes: Vec<ShapeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_review: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAddress>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub property_resolutions: Vec<PropertyResolution>,
}

/// A source property, or source-owned data attached to a qualified room.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropertyAddress {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_number: Option<RoomNumber>,
    #[serde(default, skip_serializing_if = "SourceId::is_map")]
    pub room_source: SourceId,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PropertyChoice {
    Source,
    Destination,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropertyResolution {
    pub property: PropertyAddress,
    pub keep: PropertyChoice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropertyConflict {
    pub property: PropertyAddress,
    pub source_value: String,
    pub destination_value: String,
    pub can_replace: bool,
}

/// One room a move carries: its number in the source it leaves, and the
/// number it asks to take in the other when that differs. Undoing a move
/// that renumbered a room asks for the room's old number this way. On the
/// wire it is the plain number, or `{"room_number": n, "as": m}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovedRoom {
    pub room_number: RoomNumber,
    /// The number asked for in the destination; `None` asks for
    /// `room_number`.
    pub asks: Option<RoomNumber>,
}

impl MovedRoom {
    /// The room under its own number.
    #[must_use]
    pub fn plain(room_number: RoomNumber) -> Self {
        Self {
            room_number,
            asks: None,
        }
    }

    /// The number the room asks to take in the destination.
    #[must_use]
    pub fn asked(&self) -> RoomNumber {
        self.asks.unwrap_or(self.room_number)
    }
}

impl Serialize for MovedRoom {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Asking {
            room_number: RoomNumber,
            #[serde(rename = "as")]
            asks: RoomNumber,
        }
        match self.asks.filter(|asks| *asks != self.room_number) {
            None => self.room_number.serialize(serializer),
            Some(asks) => Asking {
                room_number: self.room_number,
                asks,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for MovedRoom {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Plain(RoomNumber),
            Asking {
                room_number: RoomNumber,
                #[serde(rename = "as", default)]
                asks: Option<RoomNumber>,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Plain(room_number) => Self::plain(room_number),
            Wire::Asking { room_number, asks } => Self {
                room_number,
                asks: asks.filter(|asks| *asks != room_number),
            },
        })
    }
}

/// A move's response: the versions of the sources it changed, and every
/// moved room whose number in the destination differs from its number in
/// the source it left.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveResult {
    pub operation_id: OperationId,
    pub versions: Vec<VersionInfo>,
    /// Each moved room whose number changed, whether it asked for its new
    /// number or was renumbered, in the order the move listed the rooms.
    pub renumbered: Vec<RoomRenumbering>,
}

/// One moved room's number in the place it left and in the place it
/// reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomRenumbering {
    pub from: RoomNumber,
    pub to: RoomNumber,
}

impl MoveResult {
    /// The number moved room `number` has in the destination.
    #[must_use]
    pub fn number_in_destination(&self, number: RoomNumber) -> RoomNumber {
        self.renumbered
            .iter()
            .find(|renumbering| renumbering.from == number)
            .map_or(number, |renumbering| renumbering.to)
    }
}

/// Reads the body of a move's response, the one place that knows how the
/// server reports renumbered rooms: a `renumbered` list of `{from, to}`
/// beside the versions, one per moved room whose number changed, in the
/// order the request listed the rooms; empty or absent when none did.
///
/// # Errors
/// The body is not a move result.
pub fn parse_move_result(body: serde_json::Value) -> Result<MoveResult, serde_json::Error> {
    #[derive(Deserialize)]
    struct Wire {
        operation_id: OperationId,
        versions: Vec<VersionInfo>,
        #[serde(default)]
        renumbered: Vec<RoomRenumbering>,
    }
    let wire: Wire = serde_json::from_value(body)?;
    Ok(MoveResult {
        operation_id: wire.operation_id,
        versions: wire.versions,
        renumbered: wire.renumbered,
    })
}

/// The revision a mutation is conditioned on: one source of one map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Precondition {
    pub resource: ResourceKind,
    pub id: Uuid,
    #[serde(default)]
    pub source: SourceId,
    pub expected_rev: i64,
}

impl Precondition {
    /// The precondition naming one source of a map at a revision.
    #[must_use]
    pub fn source(area: Uuid, source: SourceId, expected_rev: i64) -> Self {
        Self {
            resource: ResourceKind::Source,
            id: area,
            source,
            expected_rev,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Source,
    Atlas,
}

/// One source's post-mutation version in a success response; a deleted
/// source reports its last accepted rev as a tombstone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub resource: ResourceKind,
    pub id: Uuid,
    #[serde(default)]
    pub source: SourceId,
    pub rev: i64,
    pub deleted: bool,
}

impl VersionInfo {
    /// The new revision of one map's own source.
    #[must_use]
    pub fn map_source(area: Uuid, rev: i64) -> Self {
        Self {
            resource: ResourceKind::Source,
            id: area,
            source: SourceId::map(),
            rev,
            deleted: false,
        }
    }

    /// Whether this names the given map's own source.
    #[must_use]
    pub fn is_map_of(&self, area: Uuid) -> bool {
        self.resource == ResourceKind::Source && self.id == area && self.source.is_map()
    }
}

/// A successful envelope's response: the echoed operation id, resulting
/// versions of every aggregate the caller can observe, and per-operation
/// results in operation order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationResult {
    pub operation_id: OperationId,
    pub versions: Vec<VersionInfo>,
    pub data: Vec<OpResult>,
}

/// One operation of a compound area mutation. Serialization is the wire
/// contract (`op` tag, `snake_case`); field names and body shapes must stay
/// lock-step with the server's `AreaMutation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AreaMutation {
    UpsertRoom {
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
        body: RoomUpdates,
    },
    /// Must-not-exist room creation. The server refuses an occupied number
    /// with a 409 `structural_conflict` (`room_number_exists`) instead of
    /// silently merging two logical rooms; local and ephemeral backends
    /// mirror the refusal in `area_edits`.
    ///
    /// SERVER FLOOR: `create_room` requires a smudgy-web release that knows
    /// the variant. An older server refuses the WHOLE envelope as a 400
    /// (serde rejects the unknown `op` tag before anything applies) — a
    /// clean rejection, never corruption — so the accepted
    /// server-ships-before-client deployment order applies.
    CreateRoom {
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
        body: RoomUpdates,
    },
    DeleteRoom {
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
    },
    /// Server-side guard for the following same-area merge plan. It verifies
    /// full secret visibility and rejects foreign inbound/outbound links
    /// atomically with the rewrites and final room deletion.
    AssertMergeSafe {
        keep_room_number: RoomNumber,
        remove_room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
    },
    UpsertRoomProperty {
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
        name: String,
        value: String,
    },
    DeleteRoomProperty {
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
        name: String,
    },
    AddRoomTag {
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
        tag: String,
    },
    RemoveRoomTag {
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
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
        room_number: RoomNumber,
        /// One of the writing source's own rooms; absent names a map room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room_source: Option<SourceId>,
        body: ExitArgs,
    },
    UpdateExit {
        exit_id: ExitId,
        body: ExitUpdates,
    },
    DeleteExit {
        exit_id: ExitId,
    },
    CreateConnection {
        body: ConnectionArgs,
    },
    UpdateConnection {
        connection_id: ConnectionId,
        body: ConnectionUpdates,
    },
    /// Merge two reciprocal one-member Connections, retaining
    /// `keep_connection_id`'s visual route and identity.
    Pair {
        keep_connection_id: ConnectionId,
        merge_connection_id: ConnectionId,
    },
    /// Move one member of a pair to a cloned Connection with this pre-minted
    /// identity. The unselected member keeps the old Connection id.
    Unlink {
        exit_id: ExitId,
        new_connection_id: ConnectionId,
    },
    /// Delete every member exit, then the Connection, as one link intent.
    DeleteLink {
        connection_id: ConnectionId,
    },
    CreateLabel {
        body: LabelArgs,
    },
    UpdateLabel {
        label_id: LabelId,
        body: LabelUpdates,
    },
    DeleteLabel {
        label_id: LabelId,
    },
    CreateShape {
        body: ShapeArgs,
    },
    UpdateShape {
        shape_id: ShapeId,
        body: ShapeUpdates,
    },
    DeleteShape {
        shape_id: ShapeId,
    },
}

impl AreaMutation {
    /// The room this operation addresses, when it addresses one — the
    /// structural-sanity check keys on it after a conflict refetch.
    #[must_use]
    pub fn room_number(&self) -> Option<RoomNumber> {
        match self {
            AreaMutation::UpsertRoom { room_number, .. }
            | AreaMutation::CreateRoom { room_number, .. }
            | AreaMutation::DeleteRoom { room_number, .. }
            | AreaMutation::UpsertRoomProperty { room_number, .. }
            | AreaMutation::DeleteRoomProperty { room_number, .. }
            | AreaMutation::AddRoomTag { room_number, .. }
            | AreaMutation::RemoveRoomTag { room_number, .. }
            | AreaMutation::CreateExit { room_number, .. } => Some(*room_number),
            _ => None,
        }
    }

    /// The action on the written source this operation needs, as the
    /// server judges each operation: `add` to create, `remove` to delete,
    /// `edit` for everything else (upserts, updates, pairing and
    /// unlinking, and the merge-safety assertion).
    #[must_use]
    pub fn required_action(&self) -> &'static str {
        match self {
            AreaMutation::CreateRoom { .. }
            | AreaMutation::CreateExit { .. }
            | AreaMutation::CreateConnection { .. }
            | AreaMutation::CreateLabel { .. }
            | AreaMutation::CreateShape { .. }
            | AreaMutation::AddRoomTag { .. } => "add",
            AreaMutation::DeleteRoom { .. }
            | AreaMutation::DeleteRoomProperty { .. }
            | AreaMutation::RemoveRoomTag { .. }
            | AreaMutation::DeleteAreaProperty { .. }
            | AreaMutation::DeleteExit { .. }
            | AreaMutation::DeleteLink { .. }
            | AreaMutation::DeleteLabel { .. }
            | AreaMutation::DeleteShape { .. } => "remove",
            AreaMutation::UpsertRoom { .. }
            | AreaMutation::AssertMergeSafe { .. }
            | AreaMutation::UpsertRoomProperty { .. }
            | AreaMutation::UpsertAreaProperty { .. }
            | AreaMutation::UpdateExit { .. }
            | AreaMutation::UpdateConnection { .. }
            | AreaMutation::Pair { .. }
            | AreaMutation::Unlink { .. }
            | AreaMutation::UpdateLabel { .. }
            | AreaMutation::UpdateShape { .. } => "edit",
        }
    }
}

/// One operation's echo, tagged for dispatch. Deletions echo the identity
/// they removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entity", rename_all = "snake_case")]
pub enum OpResult {
    MergeSafetyChecked {
        keep_room_number: RoomNumber,
        remove_room_number: RoomNumber,
    },
    Room {
        room: RoomWithDetails,
    },
    RoomDeleted {
        room_number: RoomNumber,
    },
    RoomProperty {
        room_number: RoomNumber,
        name: String,
    },
    RoomPropertyDeleted {
        room_number: RoomNumber,
        name: String,
    },
    RoomTag {
        room_number: RoomNumber,
        tag: String,
    },
    RoomTagRemoved {
        room_number: RoomNumber,
        tag: String,
    },
    AreaProperty {
        name: String,
    },
    AreaPropertyDeleted {
        name: String,
    },
    Exit {
        exit: Exit,
    },
    ExitDeleted {
        exit_id: ExitId,
    },
    Connection {
        connection: Connection,
    },
    Connections {
        connections: Vec<Connection>,
    },
    ConnectionDeleted {
        connection_id: ConnectionId,
    },
    Label {
        label: Label,
    },
    LabelDeleted {
        label_id: LabelId,
    },
    Shape {
        shape: Shape,
    },
    ShapeDeleted {
        shape_id: ShapeId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExitDirection;

    #[test]
    fn op_serialization_matches_the_server_contract() {
        let op = AreaMutation::CreateExit {
            room_source: None,
            room_number: RoomNumber(3),
            body: ExitArgs {
                from_direction: ExitDirection::North,
                ..ExitArgs::default()
            },
        };
        let json = serde_json::to_value(&op).expect("serializes");
        assert_eq!(json["op"], "create_exit");
        assert_eq!(json["room_number"], 3);
        assert_eq!(json["body"]["from_direction"], "North");

        let tag = AreaMutation::AddRoomTag {
            room_source: None,
            room_number: RoomNumber(1),
            tag: "inn".to_string(),
        };
        let json = serde_json::to_value(&tag).expect("serializes");
        assert_eq!(json["op"], "add_room_tag");
        assert_eq!(json["tag"], "inn");

        let create = AreaMutation::CreateRoom {
            room_source: None,
            room_number: RoomNumber(7),
            body: crate::RoomUpdates::default(),
        };
        let json = serde_json::to_value(&create).expect("serializes");
        assert_eq!(json["op"], "create_room");
        assert_eq!(json["room_number"], 7);
    }

    #[test]
    fn envelope_round_trips() {
        let envelope = MutationEnvelope {
            source: crate::SourceId::map(),
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition::source(
                Uuid::new_v4(),
                crate::SourceId::map(),
                41,
            )],
            payload: vec![AreaMutation::DeleteRoom {
                room_source: None,
                room_number: RoomNumber(9),
            }],
        };
        let json = serde_json::to_string(&envelope).expect("serializes");
        let back: MutationEnvelope = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.preconditions, envelope.preconditions);
        assert_eq!(back.operation_id, envelope.operation_id);
    }

    #[test]
    fn a_move_result_names_each_renumbered_room() {
        let operation_id = Uuid::new_v4();
        let renumbered = parse_move_result(serde_json::json!({
            "operation_id": operation_id,
            "versions": [],
            "data": [],
            "renumbered": [{ "from": 2, "to": 5 }],
        }))
        .expect("parses");
        assert_eq!(
            renumbered.renumbered,
            vec![RoomRenumbering {
                from: RoomNumber(2),
                to: RoomNumber(5)
            }]
        );
        assert_eq!(
            renumbered.number_in_destination(RoomNumber(2)),
            RoomNumber(5)
        );
        assert_eq!(
            renumbered.number_in_destination(RoomNumber(3)),
            RoomNumber(3)
        );

        let kept = parse_move_result(serde_json::json!({
            "operation_id": operation_id,
            "versions": [],
            "data": [],
        }))
        .expect("parses without renumbered rooms");
        assert!(kept.renumbered.is_empty());
    }

    /// A room asking for another number travels as `{room_number, as}`; one
    /// asking for its own number, or for none, as the plain number, which
    /// the server hashes the same.
    #[test]
    fn a_moved_room_asks_for_a_number_only_when_it_differs() {
        let asking = |room_number, asks| MovedRoom {
            room_number: RoomNumber(room_number),
            asks: Some(RoomNumber(asks)),
        };
        let rooms = vec![MovedRoom::plain(RoomNumber(2)), asking(4, 3), asking(5, 5)];
        assert_eq!(
            serde_json::to_value(&rooms).expect("serializes"),
            serde_json::json!([2, { "room_number": 4, "as": 3 }, 5])
        );
        let back: Vec<MovedRoom> = serde_json::from_value(serde_json::json!([
            2,
            { "room_number": 4, "as": 3 },
            { "room_number": 5, "as": 5 },
            { "room_number": 6 }
        ]))
        .expect("deserializes");
        assert_eq!(
            back,
            vec![
                MovedRoom::plain(RoomNumber(2)),
                asking(4, 3),
                MovedRoom::plain(RoomNumber(5)),
                MovedRoom::plain(RoomNumber(6)),
            ]
        );
        assert_eq!(back[1].asked(), RoomNumber(3));
        assert_eq!(back[0].asked(), RoomNumber(2));
    }
}
