pub mod backends;
pub mod cloud_api;
pub mod color;
pub mod error;
pub mod mapper;
pub mod package_api;
pub mod store_bindings;
pub mod store_node;

use derive_more::{Add, Display, From, Into};
// Re-export core types
pub use backends::{
    CachedCloudMapper, CloudMapper, CompositeBackend, Credential, CredentialSource, LocalBackend,
    MapperBackend,
};
pub use cloud_api::CloudApiClient;
pub use color::parse_css_color;
pub use package_api::{
    highest_satisfying_version, CommentView, ModuleMetaView, PackageApiClient, PackageDetail,
    PackageGrantView, PackageSearchResult, PackageView, PublishDependency, PublishModule,
    PublishedVersionView, ResolvedDependency, ResolvedModuleWire, ResolvedPackageWire,
    SearchCategory, ShareClosureItem, StaleDependencyView, VersionListItem,
};
pub use error::{CloudError, CloudResult};
pub use mapper::{AreaLoadSource, AreaLoadStat, LoadMapsSummary, Mapper};
pub use store_bindings::{StoreBindingCell, StoreBindings};
pub use store_node::{ArrayNode, Node, ObjectNode, Usage};

// Re-export data structures that match the backend API
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub use uuid::Uuid;

/// Whether the running script isolate may create/alter on-screen widgets — the `widgets`
/// smudgy op-capability (`smudgy/script/PACKAGE-ISOLATES-OP-CAPABILITIES.md`).
///
/// Lives **here** for a crate-DAG reason, not a domain one: the widget ops are in the leaf
/// `smudgy_widgets` crate (built by the UI's extension factory), so `smudgy_widgets` cannot name `core`'s
/// `SmudgyGrants`, and `core` must not depend on `smudgy_widgets` (it would pull `iced` into the
/// UI-free core). `smudgy_cloud` is the one crate both `core` and `smudgy_widgets` already depend on,
/// so it is the shared home for this tiny gate flag: `core`'s ops extension places it in the
/// isolate's `OpState` (`true` for the main/trusted/granted isolate, `false` for a sandbox that
/// didn't request `widgets`), and the `smudgy_widgets` widget ops read it to throw when denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetsEnabled(pub bool);

/// The current isolate's identity, encoded as a flat string, parked here for the same
/// crate-DAG reason as [`WidgetsEnabled`]: the leaf `smudgy_widgets` crate cannot name `core`'s
/// `IsolateId`, but a widget callback (a v8 handle bound to the isolate that created it) must be
/// routed back to that isolate to run. `core` seeds this into each isolate's `OpState` (from
/// `IsolateId::to_widget_token`), the `smudgy_widgets` button op stamps it onto the callback
/// message, and `core` decodes it (`IsolateId::from_widget_token`) to dispatch the call into the
/// owning isolate instead of always `main`.
///
/// Token shape: `<instance>\u{1f}<role>`. The leading `instance` field names the exact isolate
/// *instantiation* and CHANGES whenever an engine rebuild recreates the role's isolate — it is
/// what lets `core` refuse a callback whose v8 handle belongs to a disposed predecessor. A
/// consumer deriving a key that must stay stable across rebuilds (e.g. a UI-side text-editor
/// buffer) strips the first `\u{1f}`-delimited field and keys on the role part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetIsolate(pub String);

use crate::mapper::exit_cache::ExitCache;

/// The version this client advertises to the server in the
/// `X-Smudgy-Client-Version` header. The smudgy crates are version-locked, so
/// this crate's own package version is the app version; the cloud API compares
/// it to its `MIN_CLIENT_VERSION` floor and replies 426 to anything older.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Header carrying [`CLIENT_VERSION`] on every cloud request.
pub(crate) const CLIENT_VERSION_HEADER: &str = "x-smudgy-client-version";

/// Build a `reqwest::Client` that stamps [`CLIENT_VERSION_HEADER`] on every
/// request, so the server's "client out of date" gate can see this build's
/// version. Shared by both cloud HTTP clients (`CloudApiClient`, `CloudMapper`).
#[must_use]
pub(crate) fn versioned_http_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        CLIENT_VERSION_HEADER,
        reqwest::header::HeaderValue::from_static(CLIENT_VERSION),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("a reqwest client with a static version header is always valid")
}

/// Exit direction enum matching the backend
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Display)]
pub enum ExitDirection {
    North,
    East,
    South,
    West,
    Up,
    Down,
    Northeast,
    Northwest,
    Southeast,
    Southwest,
    In,
    Out,
    Special,
    #[default]
    Other,
}

impl ExitDirection {
    /// Every direction, in compass-then-special order (e.g. for pickers).
    pub const ALL: [Self; 14] = [
        Self::North,
        Self::Northeast,
        Self::East,
        Self::Southeast,
        Self::South,
        Self::Southwest,
        Self::West,
        Self::Northwest,
        Self::Up,
        Self::Down,
        Self::In,
        Self::Out,
        Self::Special,
        Self::Other,
    ];

    /// The direction a reciprocal exit comes back from.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::North => Self::South,
            Self::South => Self::North,
            Self::East => Self::West,
            Self::West => Self::East,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
            Self::Northeast => Self::Southwest,
            Self::Southwest => Self::Northeast,
            Self::Northwest => Self::Southeast,
            Self::Southeast => Self::Northwest,
            Self::In => Self::Out,
            Self::Out => Self::In,
            Self::Special => Self::Special,
            Self::Other => Self::Other,
        }
    }
}

/// Exit direction enum matching the backend
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Display)]
pub enum ExitStyle {
    #[default]
    Normal,
    Dashed,
    Dotted,
    Meandering,
    /// Minimal marker: a same-level exit draws only a bare directional stub
    /// (no connecting line), and a cross-level cardinal exit re-anchors its
    /// level triangle to the exit's compass side instead of a fixed corner.
    Stub,
}

impl ExitStyle {
    /// Every style, for pickers.
    pub const ALL: [Self; 5] =
        [Self::Normal, Self::Dashed, Self::Dotted, Self::Meandering, Self::Stub];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, Display)]
pub enum ShapeType {
    #[default]
    Rectangle,
    RoundedRectangle,
}

impl ShapeType {
    /// Every shape type, for pickers.
    pub const ALL: [Self; 2] = [Self::Rectangle, Self::RoundedRectangle];
}

/// Horizontal alignment enum for labels
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, Display)]
pub enum HorizontalAlignment {
    Left,
    #[default]
    Center,
    Right,
}

impl HorizontalAlignment {
    /// Every alignment, for pickers.
    pub const ALL: [Self; 3] = [Self::Left, Self::Center, Self::Right];
}

/// Vertical alignment enum for labels
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, Display)]
pub enum VerticalAlignment {
    Top,
    #[default]
    Center,
    Bottom,
}

impl VerticalAlignment {
    /// Every alignment, for pickers.
    pub const ALL: [Self; 3] = [Self::Top, Self::Center, Self::Bottom];
}

/// Share type enum for permissions
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShareType {
    Read,
    Write,
    Owner,
}

/// Viewer-scoped capabilities on an area, served by the cloud API on every
/// area row (`GET /areas`, `GET /areas/{id}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaAccess {
    pub is_owner: bool,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    /// Effective full-deputy (`can_admin`). Implies all lower caps including
    /// `can_reshare`; drives the "owner or admin" affordance gating in the UI.
    #[serde(default)]
    pub can_admin: bool,
    pub include_secrets: bool,
}

impl AreaAccess {
    /// Full capabilities, used when the server predates the access block —
    /// every area it serves is owned by the caller.
    pub const OWNER: Self = Self {
        is_owner: true,
        can_edit: true,
        can_reshare: true,
        can_copy: true,
        can_admin: true,
        include_secrets: true,
    };

    /// The server's `access_fingerprint` formula (v2):
    /// first 16 hex chars of `SHA-256("v2|o|e|r|c|a|s")` with bools as '1'/'0'.
    /// Must stay in lockstep with the server (`smudgy-api` `crypto::access_fingerprint`)
    /// — a version mismatch defeats the sync cache key.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let payload = format!(
            "v2|{}|{}|{}|{}|{}|{}",
            u8::from(self.is_owner),
            u8::from(self.can_edit),
            u8::from(self.can_reshare),
            u8::from(self.can_copy),
            u8::from(self.can_admin),
            u8::from(self.include_secrets)
        );
        let digest = Sha256::digest(payload.as_bytes());
        let mut out = String::with_capacity(16);
        for byte in &digest[..8] {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Whether the viewer may set or clear `is_secret` flags in this area.
    #[must_use]
    pub const fn is_cleared_for_secrets(&self) -> bool {
        self.can_edit && (self.is_owner || self.include_secrets)
    }
}

/// One row of `GET /sync`: the projected rev and access fingerprint for an
/// area the caller can view. `rev` is opaque — compare for inequality only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRow {
    pub area_id: AreaId,
    pub rev: i64,
    pub access_fingerprint: String,
}

/// Entry in a projected area's `linked_areas` list. Hidden targets carry only
/// the per-viewer `to_area_token`; visible ones the real id and name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedAreaInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_area_id: Option<AreaId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_area_token: Option<String>,
    pub visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash, Display, Copy)]
#[serde(transparent)]
pub struct AreaId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash, Display, Copy)]
#[serde(transparent)]
pub struct ExitId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash, Display, Copy)]
#[serde(transparent)]
pub struct AtlasId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash, Display, Copy)]
#[serde(transparent)]
pub struct LabelId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash, Display, Copy)]
#[serde(transparent)]
pub struct ShapeId(pub Uuid);

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Hash,
    PartialOrd,
    Ord,
    Copy,
    Display,
    Add,
    From,
    Into,
    Default,
)]
#[serde(transparent)]
pub struct RoomNumber(pub i32);
/// Atlas model for grouping areas
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Atlas {
    pub id: AtlasId,
    pub user_id: Option<Uuid>,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// One row of `GET /atlases`: an owned atlas (folder) with its member count.
///
/// This is the **only** place an atlas's name is served — areas carry only
/// the `atlas_id` UUID — so the client must hold this inventory to label
/// folders. `area_count` lets the UI render empty folders distinctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasListItem {
    pub id: AtlasId,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub area_count: i64,
    /// The caller owns this atlas (vs. only administers it). Older servers
    /// (owned-only `GET /atlases`) omit it → defaults true.
    #[serde(default = "default_true")]
    pub is_owner: bool,
    /// The caller holds effective `can_admin` on this atlas (owner ⇒ true).
    #[serde(default)]
    pub can_admin: bool,
    /// The owner's nickname on administered (non-owned) folders;
    /// omitted on the caller's own atlases.
    #[serde(default)]
    pub owner_nickname: Option<String>,
}

const fn default_true() -> bool {
    true
}

/// Area model
///
/// `rev` is **opaque**: it is the projected revision for the viewer and can
/// move *down* when capabilities change — compare for inequality, never order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Area {
    pub id: AreaId,
    pub user_id: Option<Uuid>,
    pub atlas_id: Option<AtlasId>,
    /// The denormalized name of the area's atlas (§4.1 of the map-server-scoping
    /// plan). Surfaced to *every* viewer who can see the area — alongside the
    /// un-redacted `atlas_id` — so a share recipient can render the owner's
    /// folder structure. Recipients have no other name source: `GET /atlases`
    /// stays owned-or-administered. Refreshed whenever the list is refetched;
    /// atlas renames don't bump member area revs (accepted staleness). `Some`
    /// iff `atlas_id` is `Some` (an atlas-less area carries no `atlas_name`
    /// key). Knowing the atlas id/name confers no capability — all container
    /// ops stay grant-gated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_name: Option<String>,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub rev: i64,
    /// Viewer-scoped capabilities; absent on legacy servers (=> owned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AreaAccess>,
    /// The owner's nickname; present only on areas shared
    /// *to* the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_nickname: Option<String>,
    /// Clone provenance; served only to the area's owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_from_area_id: Option<AreaId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_from_rev: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copied_at: Option<DateTime<Utc>>,
    /// Per-viewer copy-family bucketing token (`f_` + 16 hex, 18 chars total).
    ///
    /// Served **only on the `GET /areas` list** (never on `GET /areas/{id}`
    /// or the copy response), and **omitted whenever the viewer can see only
    /// one member of the family** — so its absence means "no grouping to
    /// show," *never* "this isn't a fork." Provenance (`copied_from_area_id`)
    /// is owner-only, so family membership must **not** be inferred from its
    /// absence either.
    ///
    /// The token is a per-viewer HMAC: stable for this user across requests,
    /// but a *different* value for every other user and not comparable to
    /// anything outside this user's own `GET /areas` response. Therefore:
    /// bucket rows by **exact string equality** for the current list only —
    /// never persist it, never cross-reference it, never round-trip it back to
    /// the server. (Because it is list-only it deliberately does **not** live
    /// on the area cache, which is fed by `get_area`; see the in-memory family
    /// index on the map editor window.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_token: Option<String>,
}

impl Area {
    /// The viewer's capabilities, treating a missing access block (legacy
    /// server, locally-created area) as fully owned.
    #[must_use]
    pub fn effective_access(&self) -> AreaAccess {
        self.access.unwrap_or(AreaAccess::OWNER)
    }
}

/// Complete area with all associated data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaWithDetails {
    #[serde(flatten)]
    pub area: Area,
    /// Viewer-salted hash of the projected content; equal hashes mean the
    /// refetched projection is byte-identical (skip re-render).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub properties: Vec<Property>,
    pub rooms: Vec<RoomWithDetails>,
    pub labels: Vec<Label>,
    pub shapes: Vec<Shape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_areas: Vec<LinkedAreaInfo>,
}

/// Room within an area
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub area_id: AreaId,
    pub room_number: RoomNumber,
    pub title: String,
    pub description: String,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub color: String,
    pub created_at: DateTime<Utc>,
    /// Owner-side secrecy flag. The wire projection never carries it for
    /// rooms; it is populated locally (secret-audit overlay, optimistic marks).
    #[serde(default)]
    pub is_secret: bool,
    /// Optional server-global room identity (a GMCP/MSDP room id, opaque
    /// string — hash ids exist in the wild). Indexed by the atlas cache for
    /// O(1) id → room resolution. Not unique-enforced; duplicate bindings are
    /// resolved best-effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

/// Room with all associated data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomWithDetails {
    pub room_number: RoomNumber,
    pub title: String,
    pub description: String,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub color: String,
    pub properties: Vec<Property>,
    pub exits: Vec<Exit>,
    /// Case-insensitive room tags, normalized to UPPERCASE. A set: deduped and
    /// deterministically ordered for stable cache fingerprints. Non-secret.
    #[serde(default)]
    pub tags: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub is_secret: bool,
    /// See [`Room::external_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

/// Exit connecting rooms
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exit {
    pub id: ExitId,
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: String,
    pub is_hidden: bool,
    pub is_closed: bool,
    pub is_locked: bool,
    pub weight: f32,
    pub command: String,
    #[serde(default)]
    pub style: ExitStyle,
    pub color: String,
    /// True when the destination area exists but is not visible to the
    /// viewer; the real `to_*` fields are nulled and `to_area_token` set.
    #[serde(default)]
    pub to_unknown: bool,
    /// Per-viewer stable token identifying a hidden destination; converging
    /// exits into the same hidden area share one token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_area_token: Option<String>,
    #[serde(default)]
    pub is_secret: bool,
}

/// Text label on area
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: LabelId,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub horizontal_alignment: HorizontalAlignment,
    pub vertical_alignment: VerticalAlignment,
    pub text: String,
    pub color: String,
    pub background_color: String,
    pub font_size: i32,
    pub font_weight: i32,
    #[serde(default)]
    pub is_secret: bool,
}

/// Graphical shape on area
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shape {
    pub id: ShapeId,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub background_color: Option<String>,
    pub stroke_color: Option<String>,
    pub shape_type: ShapeType,
    pub border_radius: f32,
    pub stroke_width: f32,
    #[serde(default)]
    pub is_secret: bool,
}

/// Simple property key-value pair
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub is_secret: bool,
}

/// Room creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RoomUpdates {
    pub title: Option<String>,
    pub description: Option<String>,
    pub level: Option<i32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub color: Option<String>,
    /// Only send when the caller is cleared for secrets (`can_edit AND
    /// (owner OR include_secrets)`) — the server uniform-404s otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
    /// `Option<Option<_>>` like [`AreaUpdates::atlas_id`]: absent = unchanged,
    /// present+null = clear the binding, present+string = set it. Omitted
    /// from the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<Option<String>>,
}

/// Exit creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ExitArgs {
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: bool,
    pub is_closed: bool,
    pub is_locked: bool,
    pub weight: f32,
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ExitStyle>,
    /// Only send when cleared for secrets; see [`RoomUpdates::is_secret`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
}

/// Exit creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ExitUpdates {
    pub from_direction: Option<ExitDirection>,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: Option<bool>,
    pub is_closed: Option<bool>,
    pub is_locked: Option<bool>,
    pub weight: Option<f32>,
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ExitStyle>,
    pub color: Option<String>,
    /// Only send when cleared for secrets; see [`RoomUpdates::is_secret`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
    /// Explicitly null the destination (`to_*`) server-side; overrides any
    /// `to_*` fields sent in the same request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_to: Option<bool>,
}

impl ExitUpdates {
    #[must_use]
    pub fn apply(self, exit: &ExitCache) -> ExitCache {
        let (color, iced_color) = self
            .color
            .map(|c| {
                let iced_color = parse_css_color(&c).unwrap_or(exit.iced_color);
                (Some(c), iced_color)
            })
            .unwrap_or((exit.color.clone(), exit.iced_color));

        let clear_to = self.clear_to == Some(true);
        // Mirror the server's COALESCE semantics: `None` means "unchanged";
        // the only way to null a destination is `clear_to`. (Diverging here
        // would ghost-unlink exits locally on partial updates, e.g. from the
        // script API.)
        let (to_area_id, to_room_number, to_direction) = if clear_to {
            (None, None, None)
        } else {
            (
                self.to_area_id.or(exit.to_area_id),
                self.to_room_number.or(exit.to_room_number),
                self.to_direction.or(exit.to_direction),
            )
        };
        // A locally-set (or cleared) destination is known to the viewer.
        let destination_touched =
            clear_to || self.to_area_id.is_some() || self.to_room_number.is_some();
        let (to_unknown, to_area_token) = if destination_touched {
            (false, None)
        } else {
            (exit.to_unknown, exit.to_area_token.clone())
        };

        ExitCache {
            id: exit.id,
            from_direction: self.from_direction.unwrap_or(exit.from_direction),
            to_area_id,
            to_room_number,
            to_direction,
            path: self.path.or_else(|| exit.path.clone()),
            is_hidden: self.is_hidden.unwrap_or(exit.is_hidden),
            is_closed: self.is_closed.unwrap_or(exit.is_closed),
            is_locked: self.is_locked.unwrap_or(exit.is_locked),
            weight: self.weight.unwrap_or(exit.weight),
            command: self.command.or_else(|| exit.command.clone()),
            style: self.style.unwrap_or(exit.style),
            color,
            iced_color,
            to_unknown,
            to_area_token,
            is_secret: self.is_secret.unwrap_or(exit.is_secret),
        }
    }
}
/// Label creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LabelArgs {
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub horizontal_alignment: HorizontalAlignment,
    pub vertical_alignment: VerticalAlignment,
    pub text: String,
    pub color: String,
    pub background_color: Option<String>,
    pub font_size: i32,
    pub font_weight: i32,
    /// Only send when cleared for secrets; see [`RoomUpdates::is_secret`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
}

/// Label creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LabelUpdates {
    pub level: Option<i32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub horizontal_alignment: Option<HorizontalAlignment>,
    pub vertical_alignment: Option<VerticalAlignment>,
    pub text: Option<String>,
    pub color: Option<String>,
    pub background_color: Option<String>,
    pub font_size: Option<i32>,
    pub font_weight: Option<i32>,
    /// Only send when cleared for secrets; see [`RoomUpdates::is_secret`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
}

impl LabelUpdates {
    /// Returns a copy of `label` with every `Some` field applied.
    #[must_use]
    pub fn apply(self, label: &Label) -> Label {
        Label {
            id: label.id,
            is_secret: self.is_secret.unwrap_or(label.is_secret),
            level: self.level.unwrap_or(label.level),
            x: self.x.unwrap_or(label.x),
            y: self.y.unwrap_or(label.y),
            width: self.width.unwrap_or(label.width),
            height: self.height.unwrap_or(label.height),
            horizontal_alignment: self
                .horizontal_alignment
                .unwrap_or_else(|| label.horizontal_alignment.clone()),
            vertical_alignment: self
                .vertical_alignment
                .unwrap_or_else(|| label.vertical_alignment.clone()),
            text: self.text.unwrap_or_else(|| label.text.clone()),
            color: self.color.unwrap_or_else(|| label.color.clone()),
            background_color: self
                .background_color
                .unwrap_or_else(|| label.background_color.clone()),
            font_size: self.font_size.unwrap_or(label.font_size),
            font_weight: self.font_weight.unwrap_or(label.font_weight),
        }
    }
}

/// Shape creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ShapeArgs {
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub background_color: Option<String>,
    pub stroke_color: Option<String>,
    pub shape_type: ShapeType,
    pub border_radius: f32,
    pub stroke_width: Option<f32>,
    /// Only send when cleared for secrets; see [`RoomUpdates::is_secret`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
}

/// Shape creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ShapeUpdates {
    pub level: Option<i32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub background_color: Option<String>,
    pub stroke_color: Option<String>,
    pub shape_type: Option<ShapeType>,
    /// The update endpoint names this field `radius` (create/response use
    /// `border_radius`); the alias keeps old serialized forms readable.
    #[serde(
        rename(serialize = "radius"),
        alias = "radius",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub border_radius: Option<f32>,
    pub stroke_width: Option<f32>,
    /// Only send when cleared for secrets; see [`RoomUpdates::is_secret`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
}

impl ShapeUpdates {
    /// Returns a copy of `shape` with every `Some` field applied.
    ///
    /// `background_color`/`stroke_color` are kept when the update is `None`;
    /// clearing them is not expressible through updates.
    #[must_use]
    pub fn apply(self, shape: &Shape) -> Shape {
        Shape {
            id: shape.id,
            is_secret: self.is_secret.unwrap_or(shape.is_secret),
            level: self.level.unwrap_or(shape.level),
            x: self.x.unwrap_or(shape.x),
            y: self.y.unwrap_or(shape.y),
            width: self.width.unwrap_or(shape.width),
            height: self.height.unwrap_or(shape.height),
            background_color: self
                .background_color
                .or_else(|| shape.background_color.clone()),
            stroke_color: self.stroke_color.or_else(|| shape.stroke_color.clone()),
            shape_type: self
                .shape_type
                .unwrap_or_else(|| shape.shape_type.clone()),
            border_radius: self.border_radius.unwrap_or(shape.border_radius),
            stroke_width: self.stroke_width.unwrap_or(shape.stroke_width),
        }
    }
}

/// Area creation data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAreaRequest {
    pub name: String,
    pub atlas_id: Option<AtlasId>,
    /// Route the new area to the session-lifetime ephemeral tier (in-memory,
    /// never persisted or synced). Client-side routing only — never on the
    /// wire, and single-tier backends ignore it.
    #[serde(skip)]
    pub ephemeral: bool,
}

/// Area update data.
///
/// Both fields are omitted from the wire when `None` so the server's
/// COALESCE semantics see "no change". This is load-bearing: `atlas_id` is
/// `Option<Option<_>>` (absent = unchanged, present+null = make loose,
/// present+uuid = set), so a name-only rename MUST omit `atlas_id` — otherwise
/// serde would emit `"atlas_id": null` and silently pull the area out of its
/// folder.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AreaUpdates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_id: Option<Option<AtlasId>>,
}

// This is meant to represent a doubly or singly connected exit pair of exits between two rooms
// for use by the map view
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomConnector {
    pub from_room: Room,
    pub from: Exit,
    pub to_room: Option<Room>,
    pub to: Option<Exit>,
}

#[cfg(test)]
mod tests {
    use super::{AreaUpdates, AtlasId, Uuid};
    use serde_json::json;

    /// Regression: a name-only rename must not carry `atlas_id` on the wire,
    /// and the move cases must (present+uuid = set, present+null = make loose).
    #[test]
    fn area_updates_only_serialize_the_fields_in_play() {
        let rename = AreaUpdates {
            name: Some("New".to_string()),
            atlas_id: None,
        };
        assert_eq!(
            serde_json::to_value(&rename).unwrap(),
            json!({ "name": "New" }),
            "name-only rename must omit atlas_id (else the server makes the area loose)"
        );

        let atlas_id = AtlasId(Uuid::from_u128(0x1234));
        let into_folder = AreaUpdates {
            name: None,
            atlas_id: Some(Some(atlas_id)),
        };
        assert_eq!(
            serde_json::to_value(&into_folder).unwrap(),
            json!({ "atlas_id": atlas_id.0 })
        );

        let pull_loose = AreaUpdates {
            name: None,
            atlas_id: Some(None),
        };
        assert_eq!(
            serde_json::to_value(&pull_loose).unwrap(),
            json!({ "atlas_id": null })
        );
    }
}
