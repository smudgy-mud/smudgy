//! The client half of the mapper mutation envelope — the mirrored (not
//! shared; see the repo AGENTS.md) wire contract of `smudgy-api`'s
//! `mapping::contract` and `mapping::ops`.
//!
//! Every mapper content write compiles down to a [`MutationEnvelope`] of
//! ordered [`AreaMutation`]s conditioned on the caller's projected revision
//! of the area. The cloud backend submits envelopes to
//! `POST /areas/{id}/mutations`; local and ephemeral backends apply the same
//! operations under their own compare-and-set, so behavior cannot drift
//! between tiers.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Connection, ConnectionArgs, ConnectionId, ConnectionUpdates, Exit, ExitArgs, ExitId,
    ExitUpdates, Label, LabelArgs, LabelId, LabelUpdates, RoomNumber, RoomUpdates, RoomWithDetails,
    Shape, ShapeArgs, ShapeId, ShapeUpdates,
};

/// Most operations one envelope may carry (server-enforced; mirrored so
/// callers can split oversized batches before submission). One authority:
/// the contract constant beside the Connection limits.
pub use crate::connection::MAX_MUTATION_OPERATIONS;

/// A client-generated operation identity: minted before enqueue, carried on
/// the wire, echoed in results, and the key of the server's idempotency
/// receipt — one retry with the same id can never double-apply.
pub type OperationId = Uuid;

/// The outer envelope of one mapper mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationEnvelope {
    pub operation_id: OperationId,
    #[serde(default)]
    pub preconditions: Vec<Precondition>,
    pub payload: Vec<AreaMutation>,
}

/// An aggregate this mutation is conditioned on, at the revision of the
/// caller's own projection (the server decides which counter that names).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Precondition {
    pub resource: ResourceKind,
    pub id: Uuid,
    pub expected_rev: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Area,
    Atlas,
}

/// One aggregate's post-mutation version in a success response; a deleted
/// aggregate reports its last accepted rev as a tombstone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub resource: ResourceKind,
    pub id: Uuid,
    pub rev: i64,
    pub deleted: bool,
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
        body: RoomUpdates,
    },
    DeleteRoom {
        room_number: RoomNumber,
    },
    UpsertRoomProperty {
        room_number: RoomNumber,
        name: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_secret: Option<bool>,
    },
    DeleteRoomProperty {
        room_number: RoomNumber,
        name: String,
    },
    AddRoomTag {
        room_number: RoomNumber,
        tag: String,
    },
    RemoveRoomTag {
        room_number: RoomNumber,
        tag: String,
    },
    UpsertAreaProperty {
        name: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_secret: Option<bool>,
    },
    DeleteAreaProperty {
        name: String,
    },
    CreateExit {
        room_number: RoomNumber,
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
            | AreaMutation::DeleteRoom { room_number }
            | AreaMutation::UpsertRoomProperty { room_number, .. }
            | AreaMutation::DeleteRoomProperty { room_number, .. }
            | AreaMutation::AddRoomTag { room_number, .. }
            | AreaMutation::RemoveRoomTag { room_number, .. }
            | AreaMutation::CreateExit { room_number, .. } => Some(*room_number),
            _ => None,
        }
    }
}

/// One operation's echo, tagged for dispatch. Deletions echo the identity
/// they removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entity", rename_all = "snake_case")]
pub enum OpResult {
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
            room_number: RoomNumber(1),
            tag: "inn".to_string(),
        };
        let json = serde_json::to_value(&tag).expect("serializes");
        assert_eq!(json["op"], "add_room_tag");
        assert_eq!(json["tag"], "inn");
    }

    #[test]
    fn envelope_round_trips() {
        let envelope = MutationEnvelope {
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: Uuid::new_v4(),
                expected_rev: 41,
                access_fingerprint: Some("abcd".to_string()),
            }],
            payload: vec![AreaMutation::DeleteRoom {
                room_number: RoomNumber(9),
            }],
        };
        let json = serde_json::to_string(&envelope).expect("serializes");
        let back: MutationEnvelope = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.preconditions, envelope.preconditions);
        assert_eq!(back.operation_id, envelope.operation_id);
    }
}
