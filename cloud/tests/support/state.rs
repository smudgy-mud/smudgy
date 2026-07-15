//! In-memory state for the mock server: users, credentials, areas, grants,
//! friendships, blocks. Mirrors the real schema closely enough to honor the
//! wire contract (dual revs, secrecy flags, grant trees).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const SESSION_PREFIX: &str = "smudgy_sess_";
pub const API_KEY_PREFIX: &str = "smudgy_";

/// Same fixed dev fallback the real server uses when `REDACTION_KEY` is unset.
pub const REDACTION_KEY: &[u8] = b"smudgy-dev-redaction-key-do-not-use-in-prod";

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
    pub is_secret: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RoomPropRecord {
    pub value: String,
    pub is_secret: bool,
}

#[derive(Debug, Clone)]
pub struct RoomRecord {
    pub room_number: i32,
    pub title: String,
    pub description: String,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub color: String,
    pub is_secret: bool,
    pub created_at: DateTime<Utc>,
    pub properties: BTreeMap<String, RoomPropRecord>,
    /// Case-insensitive room tags, normalized to UPPERCASE. Non-secret.
    pub tags: BTreeSet<String>,
    /// Server-global room identity (GMCP/MSDP room id). Nullable, non-secret,
    /// not unique-enforced.
    pub external_id: Option<String>,
}

impl RoomRecord {
    pub fn placeholder(room_number: i32) -> Self {
        Self {
            room_number,
            title: String::new(),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            is_secret: false,
            created_at: Utc::now(),
            properties: BTreeMap::new(),
            tags: BTreeSet::new(),
            external_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExitRecord {
    pub id: Uuid,
    pub from_room_number: i32,
    pub from_direction: String,
    pub to_area_id: Option<Uuid>,
    pub to_room_number: Option<i32>,
    pub to_direction: Option<String>,
    pub path: String,
    pub is_hidden: bool,
    pub is_closed: bool,
    pub is_locked: bool,
    pub weight: f32,
    pub command: String,
    pub style: String,
    pub color: String,
    pub is_secret: bool,
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
    pub is_secret: bool,
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
    pub is_secret: bool,
}

#[derive(Debug, Clone)]
pub struct AreaRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub atlas_id: Option<Uuid>,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Insertion order tiebreaker for `ORDER BY created_at` listings.
    pub created_seq: u64,
    pub rev: i64,
    pub public_rev: i64,
    pub copied_from_area_id: Option<Uuid>,
    pub copied_from_rev: Option<i64>,
    pub copied_at: Option<DateTime<Utc>>,
    pub properties: BTreeMap<String, AreaPropRecord>,
    pub rooms: BTreeMap<i32, RoomRecord>,
    pub exits: Vec<ExitRecord>,
    pub labels: Vec<LabelRecord>,
    pub shapes: Vec<ShapeRecord>,
}

impl AreaRecord {
    pub fn new(id: Uuid, user_id: Uuid, atlas_id: Option<Uuid>, name: String, seq: u64) -> Self {
        Self {
            id,
            user_id,
            atlas_id,
            name,
            created_at: Utc::now(),
            created_seq: seq,
            rev: 1,
            public_rev: 1,
            copied_from_area_id: None,
            copied_from_rev: None,
            copied_at: None,
            properties: BTreeMap::new(),
            rooms: BTreeMap::new(),
            exits: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AtlasRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
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
    pub include_secrets: bool,
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
        self.area_id == Some(area.id)
            || (self.atlas_id.is_some() && self.atlas_id == area.atlas_id)
    }
}

/// `effective_area_caps(viewer, area)`: every cap is `is_owner OR
/// bool_or(covering grants)`, `can_view` is `is_owner OR any covering grant`.
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

    /// `see_secrets` — note `include_secrets` already ORs in ownership.
    pub fn see_secrets(&self) -> bool {
        self.is_owner || self.include_secrets
    }

    /// `cleared` — may set/clear `is_secret`.
    pub fn cleared(&self) -> bool {
        self.can_edit && self.see_secrets()
    }
}

/// A `pending_transfers` row. `expires_at` is always null.
#[derive(Debug, Clone)]
pub struct PendingTransferRecord {
    pub id: Uuid,
    pub subject_kind: String,
    pub area_id: Option<Uuid>,
    pub atlas_id: Option<Uuid>,
    pub from_user_id: Uuid,
    pub to_user_id: Uuid,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub responded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default)]
pub struct MockState {
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
            caps.include_secrets |= g.include_secrets || g.can_admin;
            caps.can_admin |= g.can_admin;
        }
        Some(caps)
    }

    /// `increment_area_revision(area, bump_public)` — the per-row rev trigger.
    /// `suppress` models the txn-local `smudgy.suppress_public_rev` GUC.
    pub fn bump(&mut self, area_id: Option<Uuid>, bump_public: bool, suppress: bool) {
        let Some(area_id) = area_id else { return };
        if let Some(area) = self.areas.get_mut(&area_id) {
            area.rev += 1;
            if bump_public && !suppress {
                area.public_rev += 1;
            }
        }
    }

    /// All transitive descendants of a grant (children via `parent_grant_id`).
    pub fn grant_descendants(&self, root: Uuid) -> Vec<Uuid> {
        let mut out = Vec::new();
        let mut frontier = vec![root];
        while let Some(cur) = frontier.pop() {
            for g in self.grants.iter().filter(|g| g.parent_grant_id == Some(cur)) {
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

/// First 16 hex of `SHA-256("v2|o|e|r|c|a|s")`, bools rendered '1'/'0' (the v2
/// layout includes `can_admin`; must match the client `AreaAccess::fingerprint` exactly).
pub fn access_fingerprint(caps: &Caps) -> String {
    fn bit(b: bool) -> &'static str {
        if b { "1" } else { "0" }
    }
    let input = format!(
        "v2|{}|{}|{}|{}|{}|{}",
        bit(caps.is_owner),
        bit(caps.can_edit),
        bit(caps.can_reshare),
        bit(caps.can_copy),
        bit(caps.can_admin),
        bit(caps.include_secrets),
    );
    hex::encode(Sha256::digest(input.as_bytes()))[..16].to_string()
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

/// First 32 hex of `SHA-256(viewer_bytes || canonical_projection_bytes)`.
pub fn content_hash(viewer: Uuid, canonical: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(viewer.as_bytes());
    hasher.update(canonical);
    hex::encode(hasher.finalize())[..32].to_string()
}
