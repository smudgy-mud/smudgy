//! In-memory state for the mock server: users, credentials, areas, grants,
//! friendships, blocks. Mirrors the real schema closely enough to honor the
//! wire contract (source revisions, grant trees).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const SESSION_PREFIX: &str = "smudgy_sess_";
pub const API_KEY_PREFIX: &str = "smudgy_";

/// Same fixed dev fallback the real server uses when `REDACTION_KEY` is unset.
pub const REDACTION_KEY: &[u8] = b"smudgy-dev-redaction-key-do-not-use-in-prod";

/// The map wire format this mock serves (format 3: per-source bundles).
pub const WIRE_FORMAT_VERSION: u32 = 3;

#[derive(Debug, Clone)]
pub struct UserRecord {
    pub id: Uuid,
    pub email: String,
    pub nickname: Option<String>,
    pub requested_nickname: Option<String>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub nickname_updated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub key_suffix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// An emailed one-time verify/sign-in code. The RAW code is kept (the real
/// server stores a salted hash) so tests can fish it out of state —
/// `MockHandle::verify_code_for`.
#[derive(Debug, Clone)]
pub struct EmailCodeRecord {
    pub code: String,
    pub user_id: Uuid,
    pub consumed: bool,
}

#[derive(Debug, Clone)]
pub struct AreaPropRecord {
    pub value: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RoomPropRecord {
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct RoomRecord {
    pub identity: Uuid,
    /// A data/link stand-in for this stable room identity in another source.
    /// It never constitutes a room owned by this source.
    pub anchor: Option<Uuid>,
    pub room_number: i32,
    pub title: String,
    pub description: String,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub color: String,
    pub created_at: DateTime<Utc>,
    pub properties: BTreeMap<String, RoomPropRecord>,
    /// Case-insensitive room tags, normalized to UPPERCASE.
    pub tags: BTreeSet<String>,
    /// Server-global room identity (GMCP/MSDP room id). Nullable, not
    /// unique-enforced.
    pub external_id: Option<String>,
}

impl RoomRecord {
    pub fn placeholder(room_number: i32) -> Self {
        Self {
            identity: Uuid::new_v4(),
            anchor: None,
            room_number,
            title: String::new(),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            created_at: Utc::now(),
            properties: BTreeMap::new(),
            tags: BTreeSet::new(),
            external_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExitRecord {
    pub to_room_identity: Option<Uuid>,
    pub id: Uuid,
    pub from_room_number: i32,
    pub from_direction: String,
    pub to_area_id: Option<Uuid>,
    pub to_room_number: Option<i32>,
    pub to_direction: Option<String>,
    pub path: String,
    pub is_hidden: bool,
    /// The exit's door; `None` for none.
    pub door: Option<DoorRecord>,
    pub weight: f32,
    pub command: String,
    /// The v2 contract: every exit is a member of exactly one Connection
    /// (`map_exits.connection_id NOT NULL`); the per-exit style/color of v1
    /// live on the Connection now.
    pub connection_id: Uuid,
    /// For an exit into a room of another map's Secret (the server's
    /// `to_secret_id`): that Secret, on map `to_area_id`, whose own room
    /// `to_room_number` names. Shown only to the Secret's readers.
    pub to_secret: Option<Uuid>,
}

/// An exit's door (format-3 §2.1): its state, `open`, `closed` or `locked`,
/// its name and the command that opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorRecord {
    pub state: String,
    pub name: Option<String>,
    pub opens_with: Option<String>,
}

impl DoorRecord {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "name": self.name,
            "opens_with": self.opens_with,
        })
    }
}

/// An exit's `door` as the wire carries it: null for none.
pub fn door_json(door: Option<&DoorRecord>) -> serde_json::Value {
    door.map_or(serde_json::Value::Null, DoorRecord::json)
}

/// One stored Connection endpoint (`map_connections.endpoint_*`). Enum-ish
/// fields ride as their PascalCase wire strings, like every other mock
/// record.
#[derive(Debug, Clone)]
pub struct EndpointRecord {
    pub room_number: i32,
    pub side: String,
    pub port_offset: f32,
    pub port_mode: String,
}

/// A `map_connections` row: the shared visual geometry of one or two member
/// exits. `kind` is derived at projection time (from endpoint shape,
/// external membership, and room levels), exactly like the server.
#[derive(Debug, Clone)]
pub struct ConnectionRecord {
    pub id: Uuid,
    pub endpoint_a: EndpointRecord,
    pub endpoint_b: Option<EndpointRecord>,
    pub routing: String,
    pub segment_shape: String,
    pub corner: String,
    pub route_points: Vec<(f32, f32)>,
    pub dash: String,
    pub color: String,
    pub thickness: f32,
}

impl ConnectionRecord {
    /// A fresh default-appearance Connection (the creation path's shape:
    /// Simple/Direct/Sharp, no route, Solid, canonical gray, thickness 1).
    pub fn blank(id: Uuid, endpoint_a: EndpointRecord, endpoint_b: Option<EndpointRecord>) -> Self {
        Self {
            id,
            endpoint_a,
            endpoint_b,
            routing: "Simple".to_string(),
            segment_shape: "Direct".to_string(),
            corner: "Sharp".to_string(),
            route_points: Vec::new(),
            dash: "Solid".to_string(),
            color: "#A4A4A4".to_string(),
            thickness: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LabelRecord {
    pub id: Uuid,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub horizontal_alignment: String,
    pub vertical_alignment: String,
    pub text: String,
    pub color: String,
    pub background_color: String,
    pub font_size: i32,
    pub font_weight: i32,
}

#[derive(Debug, Clone)]
pub struct ShapeRecord {
    pub id: Uuid,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub background_color: Option<String>,
    pub stroke_color: Option<String>,
    pub shape_type: String,
    pub border_radius: f32,
    pub stroke_width: f32,
}

/// Where a Secret keeps what it holds for map room `n`: under `STAND_IN + n`
/// among its rooms. The Secret's own rooms keep their own numbers, which
/// must stay below `STAND_IN`, and map rooms a Secret refers to must lie in
/// `0..STAND_IN`; the server numbers each source on its own, and this
/// offset keeps the two numberings apart inside one record.
pub const STAND_IN: i32 = 1_000_000_000;

/// The key a Secret holds map room `map_room` under, when it is in range.
pub fn stand_in(map_room: i32) -> Option<i32> {
    (0..STAND_IN)
        .contains(&map_room)
        .then(|| map_room + STAND_IN)
}

/// The map room a Secret's room key stands in for; `None` for its own rooms.
pub fn map_room_of(key: i32) -> Option<i32> {
    (key >= STAND_IN).then(|| key - STAND_IN)
}

/// An owner Secret: a named source of one map.
///
/// Its rooms are its own rooms under their own numbers and, under
/// [`STAND_IN`] + n, the map rooms it keeps something for: their properties
/// and tags are the Secret's data for map room n, never the map's, and the
/// exits leaving them are the exits the Secret keeps on map room n (its
/// hidden doors). An exit or connection endpoint naming this map names a
/// room by the same key. A stand-in exists only while it holds something or
/// something refers to it.
#[derive(Debug, Clone)]
pub struct SecretRecord {
    pub id: Uuid,
    pub name: String,
    /// The chosen color, `#rrggbb`.
    pub color: Option<String>,
    pub rev: i64,
    /// The Secret's own properties, keyed by nothing.
    pub properties: BTreeMap<String, AreaPropRecord>,
    pub rooms: BTreeMap<i32, RoomRecord>,
    pub exits: Vec<ExitRecord>,
    pub connections: Vec<ConnectionRecord>,
    pub labels: Vec<LabelRecord>,
    pub shapes: Vec<ShapeRecord>,
    /// The Secret's grants, oldest first. They go with the Secret, and with
    /// its map.
    pub grants: Vec<SecretGrantRecord>,
    /// What makes it a Clan Secret; `None` for an owner Secret.
    pub clan: Option<super::clan_secrets::ClanSecretRecord>,
}

impl SecretRecord {
    pub fn new(id: Uuid, name: String) -> Self {
        Self {
            id,
            name,
            color: None,
            rev: 1,
            properties: BTreeMap::new(),
            rooms: BTreeMap::new(),
            exits: Vec::new(),
            connections: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            grants: Vec::new(),
            clan: None,
        }
    }
}

/// A `secret_grants` row: one grantor's grant of a Secret to one grantee,
/// keyed by (Secret, grantor, grantee). Every grant gives `read`; `actions`
/// holds the ones beyond it (`add`, `edit`, `remove`, `manage_access`).
#[derive(Debug, Clone)]
pub struct SecretGrantRecord {
    pub id: Uuid,
    pub grantor_id: Uuid,
    pub grantee_id: Uuid,
    pub actions: BTreeSet<&'static str>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AreaRecord {
    pub id: Uuid,
    /// The owning user; nil on a clan's map.
    pub user_id: Uuid,
    /// The owning clan, on a clan's map (`user_id` is then nil).
    pub clan_id: Option<Uuid>,
    pub atlas_id: Option<Uuid>,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Insertion order tiebreaker for `ORDER BY created_at` listings.
    pub created_seq: u64,
    pub rev: i64,
    pub copied_from_area_id: Option<Uuid>,
    pub copied_from_rev: Option<i64>,
    pub copied_at: Option<DateTime<Utc>>,
    pub properties: BTreeMap<String, AreaPropRecord>,
    pub rooms: BTreeMap<i32, RoomRecord>,
    pub exits: Vec<ExitRecord>,
    pub connections: Vec<ConnectionRecord>,
    pub labels: Vec<LabelRecord>,
    pub shapes: Vec<ShapeRecord>,
    /// The map's Secrets in creation order, the order the server serves
    /// their bundles in.
    pub secrets: Vec<SecretRecord>,
    /// Each author's Private source. It never appears in the Secret directory.
    pub private_sources: BTreeMap<Uuid, SecretRecord>,
    /// On a clan's Member-owned map: its recorded owners. `None` on a
    /// Clan-owned map and on every user's map.
    pub member_owned: Option<super::clan_maps::MemberOwnedRecord>,
}

impl AreaRecord {
    pub fn new(id: Uuid, user_id: Uuid, atlas_id: Option<Uuid>, name: String, seq: u64) -> Self {
        Self {
            id,
            user_id,
            clan_id: None,
            atlas_id,
            name,
            created_at: Utc::now(),
            created_seq: seq,
            rev: 1,
            copied_from_area_id: None,
            copied_from_rev: None,
            copied_at: None,
            properties: BTreeMap::new(),
            rooms: BTreeMap::new(),
            exits: Vec::new(),
            connections: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            secrets: Vec::new(),
            private_sources: BTreeMap::new(),
            member_owned: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AtlasRecord {
    pub id: Uuid,
    /// The owning user; nil on a clan's folder.
    pub user_id: Uuid,
    /// The owning clan, on a clan's folder (`user_id` is then nil).
    pub clan_id: Option<Uuid>,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Moves when the name does.
    pub rev: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendStatus {
    Pending,
    Accepted,
}

#[derive(Debug, Clone)]
pub struct FriendshipRecord {
    pub requester_id: Uuid,
    pub addressee_id: Uuid,
    pub status: FriendStatus,
    pub created_at: DateTime<Utc>,
    pub responded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct BlockRecord {
    pub blocker_id: Uuid,
    pub blocked_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct GrantRecord {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub grantor_id: Uuid,
    pub grantee_id: Uuid,
    pub area_id: Option<Uuid>,
    pub atlas_id: Option<Uuid>,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub can_admin: bool,
    /// Grantor-authored advisory host hints snapshotted at share creation
    /// (mirrors `share_grants.host_hints`). `None` = the share carried none.
    pub host_hints: Option<Vec<String>>,
    pub parent_grant_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GrantRecord {
    /// Whether this grant covers `area` (Area-scope match or Atlas-scope on the
    /// area's CURRENT atlas) — the `effective_area_caps` join predicate.
    pub fn covers_area(&self, area: &AreaRecord) -> bool {
        self.area_id == Some(area.id) || (self.atlas_id.is_some() && self.atlas_id == area.atlas_id)
    }
}

/// `effective_area_caps(viewer, area)`: every cap is `is_owner OR
/// bool_or(covering grants)`, `can_view` is `is_owner OR any covering grant`.
/// `include_secrets` is the owner's alone: Secrets are shared one at a time.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub is_owner: bool,
    pub can_view: bool,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub include_secrets: bool,
    pub can_admin: bool,
}

impl Caps {
    pub const NONE: Self = Self {
        is_owner: false,
        can_view: false,
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        include_secrets: false,
        can_admin: false,
    };
}

/// A `mutation_receipts` row: one accepted envelope per `(actor,
/// operation_id)`, holding the request hash it was accepted under and the
/// stored response body an identical retry replays verbatim.
#[derive(Debug, Clone)]
pub struct MutationReceipt {
    pub request_hash: String,
    /// The stored `MutationResult` JSON (the `data` member of the success
    /// envelope), exactly as first served.
    pub result: Value,
}

/// A `transfer_offers` row: an offer to a user, or to a clan.
#[derive(Debug, Clone)]
pub struct PendingTransferRecord {
    pub id: Uuid,
    pub subject_kind: String,
    pub area_id: Option<Uuid>,
    pub atlas_id: Option<Uuid>,
    pub from_user_id: Uuid,
    /// Exactly one of the two recipients.
    pub to_user_id: Option<Uuid>,
    pub to_clan_id: Option<Uuid>,
    /// On an offer to a clan: `"clan"` (the default) or `"members"`.
    pub ownership: Option<&'static str>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub responded_at: Option<DateTime<Utc>>,
    pub direct: bool,
    pub destination_atlas_id: Option<Uuid>,
}

#[derive(Debug, Default)]
pub struct MockState {
    pub http_requests: Vec<(String, String)>,
    pub users: Vec<UserRecord>,
    /// Raw token -> key record (the real server stores sha256 digests).
    pub api_keys: HashMap<String, ApiKeyRecord>,
    /// Raw token -> session record.
    pub sessions: HashMap<String, SessionRecord>,
    pub email_codes: Vec<EmailCodeRecord>,
    pub areas: BTreeMap<Uuid, AreaRecord>,
    pub atlases: HashMap<Uuid, AtlasRecord>,
    pub grants: Vec<GrantRecord>,
    pub friendships: Vec<FriendshipRecord>,
    pub blocks: Vec<BlockRecord>,
    pub pending_transfers: Vec<PendingTransferRecord>,
    /// Accounts marked as being deleted (`users.deleting_at`): their
    /// credentials authenticate only `DELETE /me`.
    pub deleting_accounts: BTreeSet<Uuid>,
    pub clans: super::clans::ClanStore,
    /// Pending ownership offers on clan maps.
    pub area_offers: Vec<super::clan_maps::AreaOfferRecord>,
    /// Idempotency receipts of the compound mutation endpoint, keyed
    /// `(actor, operation_id)` (mirrors the `mutation_receipts` table).
    pub mutation_receipts: HashMap<(Uuid, Uuid), MutationReceipt>,
    /// Every mutation envelope the compound endpoint accepted, in arrival
    /// order: `(operation_id, replayed_from_receipt)`. Rejected envelopes
    /// (conflicts, validation failures) are not recorded — like the real
    /// server, a rolled-back transaction leaves no trace.
    pub mutation_log: Vec<(Uuid, bool)>,
    /// Test hook: process the next N compound mutations normally (commit +
    /// receipt) but replace their responses with a 500 — a lost response on
    /// an applied mutation, for transport-retry/receipt-dedupe tests.
    pub drop_mutation_responses: u32,
    /// Test hook: refuse the next N compound mutations with a 400 before
    /// anything applies — a server verdict the client parks instead of
    /// retrying.
    pub refuse_mutations: u32,
    /// Test hook: fail the next N area deletes with a 500 before anything is
    /// deleted — an outage the client cannot tell from a lost response.
    pub fail_area_deletes: u32,
    /// Test hook: stop the next N account deletions right after the account
    /// is marked as being deleted, with a 500 — a deletion left partway,
    /// which a repeat of `DELETE /me` finishes.
    pub interrupt_account_deletions: u32,
    /// Test hook: stop the next N clan dissolutions right after the clan is
    /// marked dissolving, with a 500 — a dissolution left partway, which its
    /// owner repeating the request finishes.
    pub interrupt_dissolutions: u32,
    /// Test hook: lose the answer of the next N clan dissolutions after their
    /// Library commits, before the directory marks the clan dissolved.
    pub lose_dissolution_answers: u32,
    /// Test hook: maps whose writes answer 503 `write_freeze` to those who
    /// read them, as a transfer's sealed export does from its seal until
    /// its drop.
    pub sealed_maps: BTreeSet<Uuid>,
    /// Test hook: runs inside the next transfer acceptance, after its claim
    /// and before its flip, as a request racing it would; `true` stops the
    /// acceptance there, as a failure would.
    pub interrupt_acceptance: Option<fn(&mut MockState, &PendingTransferRecord) -> bool>,
    /// Test hook: fail the next N area reads (`GET /areas/{id}`) with a 500 —
    /// an outage between a sync row and the refetch it calls for.
    pub fail_area_reads: u32,
    /// Client-version gate floor, mirroring the server's `MIN_CLIENT_VERSION`.
    /// `None` (the default) leaves the gate disabled for every test.
    pub min_client_version: Option<String>,
    /// Newest known client version, mirroring `NEWEST_CLIENT_VERSION`. `None`
    /// (the default) disables the soft `x-smudgy-upgrade-available` hint.
    pub newest_client_version: Option<String>,
    seq: u64,
}

impl MockState {
    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    pub fn user(&self, id: Uuid) -> Option<&UserRecord> {
        self.users.iter().find(|u| u.id == id)
    }

    pub fn user_mut(&mut self, id: Uuid) -> Option<&mut UserRecord> {
        self.users.iter_mut().find(|u| u.id == id)
    }

    pub fn user_by_email(&self, email: &str) -> Option<&UserRecord> {
        self.users
            .iter()
            .find(|u| u.email.eq_ignore_ascii_case(email))
    }

    pub fn email_verified(&self, user_id: Uuid) -> bool {
        self.user(user_id)
            .is_some_and(|u| u.email_verified_at.is_some())
    }

    /// Block in EITHER direction between the pair.
    pub fn blocked_pair(&self, a: Uuid, b: Uuid) -> bool {
        self.blocks.iter().any(|x| {
            (x.blocker_id == a && x.blocked_id == b) || (x.blocker_id == b && x.blocked_id == a)
        })
    }

    /// Accepted friendship on either side of the pair.
    pub fn are_friends(&self, a: Uuid, b: Uuid) -> bool {
        self.friendships.iter().any(|f| {
            f.status == FriendStatus::Accepted
                && ((f.requester_id == a && f.addressee_id == b)
                    || (f.requester_id == b && f.addressee_id == a))
        })
    }

    /// `effective_area_caps(viewer, area_id)`; `None` when the area is absent.
    pub fn caps(&self, viewer: Uuid, area_id: Uuid) -> Option<Caps> {
        let area = self.areas.get(&area_id)?;
        // A clan's map: its members' actions, folded into the share flags;
        // an outside share reads it and nothing more.
        if area.clan_id.is_some() {
            let mut caps = Caps::NONE;
            if let Some(actions) = super::clan_maps::area_actions(self, viewer, area) {
                super::clan_maps::fold_actions(&mut caps, &actions);
            }
            if !caps.can_view && super::shares::outside_reader(self, viewer, area) {
                caps.can_view = true;
            }
            return Some(caps);
        }
        let is_owner = area.user_id == viewer;
        let mut caps = Caps {
            is_owner,
            can_view: is_owner,
            can_edit: is_owner,
            can_reshare: is_owner,
            can_copy: is_owner,
            include_secrets: is_owner,
            can_admin: is_owner,
        };
        for g in self
            .grants
            .iter()
            .filter(|g| g.grantee_id == viewer && g.covers_area(area))
        {
            caps.can_view = true;
            // An effective can_admin folds into ALL lower caps incl. can_reshare.
            caps.can_edit |= g.can_edit || g.can_admin;
            caps.can_reshare |= g.can_reshare || g.can_admin;
            caps.can_copy |= g.can_copy || g.can_admin;
            caps.can_admin |= g.can_admin;
        }
        Some(caps)
    }

    /// Moves the map source's revision of `area_id` by one; `None` (a
    /// dangling exit's target) and absent areas are no-ops.
    pub fn bump(&mut self, area_id: Option<Uuid>) {
        let Some(area_id) = area_id else { return };
        if let Some(area) = self.areas.get_mut(&area_id) {
            area.rev += 1;
        }
    }

    /// All transitive descendants of a grant (children via `parent_grant_id`).
    pub fn grant_descendants(&self, root: Uuid) -> Vec<Uuid> {
        let mut out = Vec::new();
        let mut frontier = vec![root];
        while let Some(cur) = frontier.pop() {
            for g in self
                .grants
                .iter()
                .filter(|g| g.parent_grant_id == Some(cur))
            {
                out.push(g.id);
                frontier.push(g.id);
            }
        }
        out
    }

    /// Delete the listed grants AND their subtrees (FK `ON DELETE CASCADE`).
    pub fn delete_grants_cascading(&mut self, ids: &[Uuid]) {
        let mut doomed: Vec<Uuid> = ids.to_vec();
        for id in ids {
            doomed.extend(self.grant_descendants(*id));
        }
        self.grants.retain(|g| !doomed.contains(&g.id));
    }

    pub fn grant(&self, id: Uuid) -> Option<&GrantRecord> {
        self.grants.iter().find(|g| g.id == id)
    }

    pub fn grant_mut(&mut self, id: Uuid) -> Option<&mut GrantRecord> {
        self.grants.iter_mut().find(|g| g.id == id)
    }

    /// Claim `requested` as this user's nickname (the handle). Nicknames are
    /// globally unique, case-insensitive; returns `false` if it is already taken
    /// by another user (the caller surfaces that as "needs another nickname").
    pub fn claim_nickname(&mut self, user_id: Uuid, requested: &str) -> bool {
        let taken = self.users.iter().any(|u| {
            u.id != user_id
                && u.nickname
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(requested))
        });
        if taken {
            return false;
        }
        let Some(user) = self.user_mut(user_id) else {
            return false;
        };
        user.nickname = Some(requested.to_string());
        user.nickname_updated_at = Some(Utc::now());
        true
    }
}

// ---------------------------------------------------------------------------
// Crypto primitives, mirroring the real server's `crypto.rs`.
// ---------------------------------------------------------------------------

/// `prefix + 64 hex chars` opaque token (two v4 uuids; no `rand` dep needed).
pub fn gen_token(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

/// A 6-digit numeric one-time code, mirroring the real server's `gen_code`
/// (uuid-derived; no `rand` dep needed).
pub fn gen_code() -> String {
    let bytes = *Uuid::new_v4().as_bytes();
    let n = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000;
    format!("{n:06}")
}

/// The format-3 `projection_token`: opaque, and equal exactly when what the
/// viewer's projection shows is equal — the map's revision, their access,
/// the atlas it is filed in (whose name the header carries), each Secret
/// they read with its revision and their actions on it, and every exit into
/// another map's Secret room they are shown, with its connection (such
/// exits change no revision).
pub fn projection_token(
    state: &MockState,
    viewer: Uuid,
    area: &AreaRecord,
    caps: &Caps,
    atlas_name: Option<&str>,
) -> String {
    let bundles = super::source_refs::bundles(state, viewer, area);
    let view = serde_json::json!([
        [
            caps.is_owner,
            caps.can_edit,
            caps.can_reshare,
            caps.can_copy,
            caps.can_admin,
            caps.include_secrets
        ],
        area.atlas_id,
        atlas_name,
        bundles,
        super::source_refs::linked(state, area.id, &bundles),
    ]);
    let mut hasher = Sha256::new();
    hasher.update(viewer.as_bytes());
    hasher.update(area.id.as_bytes());
    hasher.update(view.to_string().as_bytes());
    format!("p_{}", &hex::encode(hasher.finalize())[..32])
}

/// The projection's `linked_areas` as the token covers them: each map the
/// shown exits lead into that `viewer` reads, by ID and name, and each other
/// one by its `to_area_token`, which nothing done to that map moves.
fn linked_view(state: &MockState, viewer: Uuid, area: &AreaRecord) -> Vec<String> {
    let mut linked: Vec<String> = Vec::new();
    let mut note = |exit: &ExitRecord| {
        let (exit, reads) = state.resolved_exit(viewer, area.id, exit);
        let Some(target) = exit.to_area_id.filter(|target| *target != area.id) else {
            return;
        };
        let entry = if reads {
            let name = state.areas.get(&target).map(|map| map.name.clone());
            format!("{target}:{name:?}")
        } else {
            to_area_token(
                viewer,
                if exit.to_room_number.is_some() {
                    exit.id
                } else {
                    target
                },
            )
        };
        if !linked.contains(&entry) {
            linked.push(entry);
        }
    };
    for exit in &area.exits {
        note(exit);
    }
    for (secret, _) in state.readable_secrets(viewer, area) {
        for exit in &secret.exits {
            note(exit);
        }
    }
    linked.sort();
    linked
}

/// Every exit into another map's Secret room that `viewer` is shown on
/// `area`, the map's and its readable Secrets', with its connection, as
/// the token covers them.
fn shown_foreign_exits(state: &MockState, viewer: Uuid, area: &AreaRecord) -> Vec<String> {
    let mut shown = Vec::new();
    let mut collect = |exits: &[ExitRecord]| {
        for exit in exits
            .iter()
            .filter(|exit| exit.to_area_id.is_some_and(|target| target != area.id))
        {
            let (resolved, readable) = state.resolved_exit(viewer, area.id, exit);
            shown.push(if readable {
                format!(
                    "{}:{:?}:{:?}:{:?}",
                    exit.id, resolved.to_area_id, resolved.to_secret, resolved.to_room_number
                )
            } else {
                format!("{}:unknown", exit.id)
            });
        }
    };
    collect(&area.exits);
    for (secret, _) in state.readable_secrets(viewer, area) {
        collect(&secret.exits);
    }
    shown.sort();
    shown
}

/// `"u_" + first 16 hex of HMAC-SHA256(key, viewer || target)`.
pub fn to_area_token(viewer: Uuid, target: Uuid) -> String {
    use hmac::{Hmac, Mac};
    let mut mac =
        Hmac::<Sha256>::new_from_slice(REDACTION_KEY).expect("HMAC accepts any key length");
    mac.update(viewer.as_bytes());
    mac.update(target.as_bytes());
    let bytes = mac.finalize().into_bytes();
    format!("u_{}", &hex::encode(bytes)[..16])
}
