pub mod access_review;
pub mod automatic_routing;
pub mod backends;
pub mod clan_access;
pub mod clan_maps;
pub mod clan_secrets;
pub mod clans;
pub mod cloud_api;
pub mod color;
pub mod connection;
pub mod connection_geometry;
pub mod connection_lifecycle;
pub mod error;
pub mod format3;
pub mod image_source;
pub mod link_edits;
pub mod mapper;
pub mod mutation;
pub mod package_api;
pub mod relocation;
pub mod store_bindings;
pub mod store_node;

use derive_more::{Add, Display, From, Into};
// Re-export core types
pub use backends::{
    AreaMergeCommit, AreaMergeOutcome, AreaMergePlan, AreaMergeSource, CachedCloudMapper,
    CloudMapper, CompositeBackend, Credential, CredentialSource, LocalBackend, MapperBackend,
    RoomRemap, Translate, apply_area_merge,
};
pub use cloud_api::CloudApiClient;
pub use color::{canonicalize_css_color, parse_css_color};
pub use connection::{
    CORNER_INSET, Connection, ConnectionArgs, ConnectionDash, ConnectionEndpoint, ConnectionId,
    ConnectionKind, ConnectionRouting, ConnectionUpdates, CornerStyle, DEFAULT_CONNECTION_COLOR,
    DEFAULT_CONNECTION_THICKNESS, DIAGONAL_BEARING_RATIO, MAX_COLOR_LEN, MAX_COORDINATE,
    MAX_MUTATION_OPERATIONS, MAX_ROUTE_POINTS, MapPoint, PortMode, RoomSide, SegmentShape,
    THICKNESS_RANGE, default_anchor_for_bearing, default_anchor_for_direction,
};
pub use error::{CloudError, CloudResult};
pub use format3::{RoomData, SourceBundle};
pub use mapper::{
    AreaImportDocument, AreaLoadSource, AreaLoadStat, CreateAreaError, LoadMapsSummary, Mapper,
    MovedContent,
};
pub use package_api::{
    BundleBody, CheckUpdatesEntry, CheckUpdatesHave, CheckUpdatesResponse, CheckUpdatesResult,
    CommentView, DependencyKind, ModuleMetaView, PackageApiClient, PackageDetail, PackageGrantView,
    PackageOwnerKind, PackageSearchResult, PackageView, PublishDependency, PublishModule,
    PublishedVersionView, ResolvedDependency, ResolvedModuleWire, ResolvedPackageWire,
    SearchCategory, ShareClosureItem, StaleDependencyView, UpdateCheckClosureNode,
    UpdateCheckDependency, UpdateCheckInstalled, UpdateCheckLatest, VersionListItem,
    highest_satisfying_version,
};
pub use relocation::{
    AtlasRelocation, MapRelocation, PartialRelocation, RelocationError, RelocationMode,
};
pub use store_bindings::{StoreBindingCell, StoreBindings};
pub use store_node::{ArrayNode, Node, ObjectNode, Usage};

// Re-export data structures that match the backend API
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
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

/// Whether this isolate may compile WGSL and construct shader-backed text effects.
/// Bridged from `permissions.smudgy.widgets: ["shaders"]` for the same crate-DAG
/// reason as [`WidgetsEnabled`]. An absent flag denies access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetShadersEnabled(pub bool);

/// The current isolate's identity, encoded as a flat string, parked here for the same
/// crate-DAG reason as [`WidgetsEnabled`]: the leaf `smudgy_widgets` crate cannot name `core`'s
/// `IsolateId`, but a widget callback lease must be routed back to the isolate whose registry
/// owns its V8 function. `core` seeds this into each isolate's `OpState` (from
/// `IsolateId::to_widget_token`), the `smudgy_widgets` button op stamps it onto the callback
/// message, and `core` decodes it (`IsolateId::from_widget_token`) to dispatch the call into the
/// owning isolate instead of always `main`.
///
/// Token shape: `<instance>\u{1f}<role>`. The leading `instance` field names the exact isolate
/// *instantiation* and CHANGES whenever an engine rebuild recreates the role's isolate — it is
/// what lets `core` refuse a lease into a retired callback registry before touching V8. A
/// consumer deriving a key that must stay stable across rebuilds (e.g. a UI-side text-editor
/// buffer) strips the first `\u{1f}`-delimited field and keys on the role part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetIsolate(pub String);

use crate::mapper::exit_cache::ExitCache;

/// Where authoritative map content lives.
///
/// Storage and organization are deliberately separate concepts: an area may
/// be loose or filed into an atlas, but either way it belongs to exactly one
/// storage tier. Session maps disappear with the session, local maps are
/// authoritative files on this device, and cloud maps sync through the map
/// service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
#[serde(rename_all = "snake_case")]
pub enum MapStorage {
    Session,
    Local,
    Cloud,
}

/// A complete destination for creating, copying, or moving an area.
///
/// `atlas_id: None` means a loose area. Session maps cannot be filed into an
/// atlas because the session tier has no durable folder inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapDestination {
    pub storage: MapStorage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_id: Option<AtlasId>,
}

impl MapDestination {
    #[must_use]
    pub const fn loose(storage: MapStorage) -> Self {
        Self {
            storage,
            atlas_id: None,
        }
    }

    #[must_use]
    pub const fn in_atlas(storage: MapStorage, atlas_id: AtlasId) -> Self {
        Self {
            storage,
            atlas_id: Some(atlas_id),
        }
    }
}

/// The version this client advertises to the server in the
/// `X-Smudgy-Client-Version` header. The smudgy crates are version-locked, so
/// this crate's own package version is the app version; the cloud API compares
/// it to its `MIN_CLIENT_VERSION` floor and replies 426 to anything older.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Final release line that carries the mapper's ephemeral compatibility shims
/// (the pre-storage-model way to request and observe the session tier). They
/// are intentionally unavailable starting with 0.6.0. Storage-less creation
/// is not a shim: it is the supported default (durable, cloud when signed in,
/// local otherwise) and stays past 0.6.
pub const MAP_STORAGE_COMPATIBILITY_LAST_RELEASE: &str = "0.5.x";

/// First release in which the mapper's ephemeral compatibility shims must be
/// removed. A test below trips as soon as the crate reaches this version,
/// preventing an accidental extra compatibility cycle.
pub const MAP_STORAGE_COMPATIBILITY_REMOVAL_VERSION: &str = "0.6.0";

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
        .connect_timeout(std::time::Duration::from_secs(10))
        // Bound a stalled response, while allowing large package transfers
        // to keep running as long as bytes continue to arrive.
        .read_timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("a reqwest client with a static version header is always valid")
}

/// Exit direction enum matching the backend
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, Display)]
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

    /// The access the clan map `actions` give, as the server derives it on a
    /// clan's own map: editing with any of `area.add`, `area.edit` and
    /// `area.remove_content`, copying with `area.copy`, and nothing more,
    /// since nobody owns a clan's map. No actions give no access.
    #[must_use]
    pub fn of_clan_map(actions: Option<&BTreeSet<String>>) -> Self {
        use clans::action;
        let holds = |wanted: &str| actions.is_some_and(|actions| actions.contains(wanted));
        Self {
            is_owner: false,
            can_edit: [
                action::ADD_TO_AREA,
                action::EDIT_AREA,
                action::REMOVE_FROM_AREA,
            ]
            .into_iter()
            .any(holds),
            can_reshare: false,
            can_copy: holds(action::COPY_AREA),
            can_admin: false,
            include_secrets: false,
        }
    }

    /// The viewer's access to a map served with `access`, or without it. A
    /// map served without it (by a server that predates the access block,
    /// or kept only on this device) is the viewer's own, unless it is a
    /// clan's map: nobody owns that one, and the clan's map `actions` say
    /// what the viewer may do with it.
    #[must_use]
    pub fn effective(
        access: Option<Self>,
        clan_id: Option<Uuid>,
        actions: Option<&BTreeSet<String>>,
    ) -> Self {
        match (access, clan_id) {
            (Some(access), _) => access,
            (None, Some(_)) => Self::of_clan_map(actions),
            (None, None) => Self::OWNER,
        }
    }
}

/// A source of a map's content (wire format 3): the map itself (`"map"`),
/// the caller's Private additions (`"private"`), or a Secret's UUID.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceId {
    #[default]
    Map,
    Private,
    Secret(Uuid),
}

/// A room's address within a map. The number is local to its owning source.
/// `Private` is relative to the viewer of the containing map projection;
/// addresses must not be carried across account generations without that context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomAddress {
    pub source: SourceId,
    pub number: RoomNumber,
}

impl RoomAddress {
    #[must_use]
    pub const fn new(source: SourceId, number: RoomNumber) -> Self {
        Self { source, number }
    }

    #[must_use]
    pub const fn map(number: RoomNumber) -> Self {
        Self::new(SourceId::Map, number)
    }

    #[must_use]
    pub fn from_wire(source: Option<SourceId>, number: RoomNumber) -> Self {
        Self::new(source.unwrap_or_default(), number)
    }

    /// Format-3 endpoint order: Map first, then room number. Different
    /// non-Map sources can tie; this key is never a room identity.
    #[must_use]
    pub const fn connection_order_key(self) -> (bool, RoomNumber) {
        (!self.source.is_map(), self.number)
    }

    #[must_use]
    pub const fn wire_source(self) -> Option<SourceId> {
        if self.source.is_map() {
            None
        } else {
            Some(self.source)
        }
    }
}

impl From<RoomNumber> for RoomAddress {
    fn from(number: RoomNumber) -> Self {
        Self::map(number)
    }
}

/// An address across maps, within one account's projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapRoomAddress {
    pub map: AreaId,
    pub room: RoomAddress,
}

impl SourceId {
    pub const MAP: &'static str = "map";
    pub const PRIVATE: &'static str = "private";

    #[must_use]
    pub const fn map() -> Self {
        Self::Map
    }

    #[must_use]
    pub const fn is_map(&self) -> bool {
        matches!(self, Self::Map)
    }

    /// Whether this is one of the map's Secrets.
    #[must_use]
    pub const fn is_secret(&self) -> bool {
        matches!(self, Self::Secret(_))
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Map => f.write_str(Self::MAP),
            Self::Private => f.write_str(Self::PRIVATE),
            Self::Secret(id) => write!(f, "{id}"),
        }
    }
}

impl std::str::FromStr for SourceId {
    type Err = CloudError;

    fn from_str(wire: &str) -> Result<Self, Self::Err> {
        match wire {
            Self::MAP => Ok(Self::Map),
            Self::PRIVATE => Ok(Self::Private),
            _ => Uuid::parse_str(wire)
                .map(Self::Secret)
                .map_err(|_| CloudError::SerializationError(format!("unknown source {wire:?}"))),
        }
    }
}

impl Serialize for SourceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        wire.parse().map_err(serde::de::Error::custom)
    }
}

/// One row of `GET /sync`: a map the caller can read, the token over
/// everything their projection of it shows, and the revision of each source
/// they can read. A changed token means refetch, whether or not a revision
/// moved with it: a token also moves alone when the caller's access changes
/// and when an exit into another map's Secret room they are shown changes,
/// appears or goes. Revisions are opaque — compare for inequality only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRow {
    pub area_id: AreaId,
    pub projection_token: String,
    #[serde(default)]
    pub revisions: BTreeMap<SourceId, i64>,
}

impl SyncRow {
    /// The map source's revision, when the row carries it.
    #[must_use]
    pub fn map_rev(&self) -> Option<i64> {
        self.revisions.get(&SourceId::map()).copied()
    }

    /// Whether `self` differs from `previous` only by its token: the
    /// caller's access changed, or an exit into another map's Secret room
    /// they are shown changed, appeared or went (a Secret they no longer
    /// read takes such exits with it). Either way, something they could
    /// read may be gone.
    #[must_use]
    pub fn token_moved_alone_since(&self, previous: &Self) -> bool {
        self.projection_token != previous.projection_token && self.revisions == previous.revisions
    }

    /// A row for a map no `/sync` covers: the cloud's own token when it
    /// served one (with revisions unknown), otherwise one derived from the
    /// revision of a local or ephemeral map.
    #[must_use]
    pub fn synthesized(area: &Area) -> Self {
        Self {
            area_id: area.id,
            projection_token: area.view_token(),
            revisions: if area.projection_token.is_some() {
                BTreeMap::new()
            } else {
                BTreeMap::from([(SourceId::map(), area.rev)])
            },
        }
    }
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

impl ExitId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

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
    /// The clan that owns the atlas, on a clan's folders (`user_id` is then
    /// `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clan_id: Option<Uuid>,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// Aggregate revision for atlas CAS preconditions; recorded now so the
    /// atlas-route conversion can build on values clients already hold.
    #[serde(default)]
    pub rev: i64,
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
    /// Aggregate revision for atlas CAS preconditions; see [`Atlas::rev`].
    #[serde(default)]
    pub rev: i64,
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
    /// The clan that owns the folder, on a clan's folders. A clan folder is
    /// listed when the caller holds an action on it or reads a map in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clan_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clan_name: Option<String>,
    /// The caller's actions on a clan folder (`atlas.*`, `area.create`,
    /// `grant.*`; see [`clans::action`]). Empty on other folders.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub actions: BTreeSet<String>,
}

impl AtlasListItem {
    /// Whether the caller holds the clan `action` on this clan folder.
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.contains(action)
    }

    /// The caller's own folder: not a clan's, and not one they only
    /// administer.
    #[must_use]
    pub fn is_own(&self) -> bool {
        self.is_owner && self.clan_id.is_none()
    }
}

/// The oldest of the caller's own folders in `atlases` named exactly
/// `name`, the one used when several share it.
#[must_use]
pub fn oldest_own_atlas_named(atlases: &[AtlasListItem], name: &str) -> Option<AtlasId> {
    atlases
        .iter()
        .filter(|atlas| atlas.is_own() && atlas.name == name)
        .min_by_key(|atlas| (atlas.created_at, atlas.id.0))
        .map(|atlas| atlas.id)
}

const fn default_true() -> bool {
    true
}

/// Area model
///
/// `rev` is the map source's revision, shared by every viewer who can read
/// the map. Format-3 list rows omit it (0); a fetched projection fills it from
/// the map bundle. A database restore can rewind it, so comparisons are for
/// inequality.
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
    #[serde(default)]
    pub rev: i64,
    /// The token over everything the caller's projection of the map shows
    /// (format 3). Compared for equality only; absent on areas that never
    /// came from the cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_token: Option<String>,
    /// Viewer-scoped capabilities; absent on legacy servers and in a create
    /// reply. Read through [`Self::effective_access`].
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
    /// The clan that owns the map, on a clan's own maps (`user_id` is then
    /// `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clan_id: Option<Uuid>,
    /// The owning clan's name, on list rows of a clan's maps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clan_name: Option<String>,
    /// The caller's clan map actions (`area.*`, `secret.*`, `grant.*`; see
    /// [`clans::action`]) on a clan's map. `None` on a map reached only by
    /// ownership or shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<BTreeSet<String>>,
    /// Whose a clan's map is, and on a Member-owned map, whether the caller
    /// owns it and whether it is frozen.
    #[serde(flatten)]
    pub clan_ownership: clan_maps::ClanOwnership,
}

impl Area {
    /// Whether the caller holds the clan map `action` on this map (see
    /// [`Self::actions`]).
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions
            .as_ref()
            .is_some_and(|actions| actions.contains(action))
    }

    /// What identifies this copy of the area for caching and sync: the
    /// cloud's projection token, or for an area the cloud never served, one
    /// derived from its revision. Safe as part of a file name.
    #[must_use]
    pub fn view_token(&self) -> String {
        self.projection_token
            .clone()
            .unwrap_or_else(|| format!("local-{}", self.rev))
    }

    /// The viewer's capabilities; see [`AreaAccess::effective`].
    #[must_use]
    pub fn effective_access(&self) -> AreaAccess {
        AreaAccess::effective(self.access, self.clan_id, self.actions.as_ref())
    }
}

/// Complete area with all associated data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaWithDetails {
    #[serde(flatten)]
    pub area: Area,
    /// The area document format: v2 introduced Connections, v3 doors and
    /// separate content sources. Versioned file readers migrate older
    /// documents before this model consumes them.
    #[serde(default = "format_version_v1")]
    pub format_version: u32,
    pub properties: Vec<Property>,
    pub rooms: Vec<RoomWithDetails>,
    /// The map source's attachments to rooms in other readable sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub room_data: Vec<RoomData>,
    pub labels: Vec<Label>,
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_areas: Vec<LinkedAreaInfo>,
    /// The map's other sources the caller can read (Secrets, Private
    /// additions), as the cloud served them. Never merged into the content
    /// above.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceBundle>,
}

impl AreaWithDetails {
    /// Hide destinations in Secrets or Private sources from an export that
    /// cannot read those sources. The exit's own content and connection remain.
    pub fn redact_source_destinations(&mut self) {
        for exit in self
            .rooms
            .iter_mut()
            .flat_map(|room| &mut room.exits)
            .chain(self.room_data.iter_mut().flat_map(|data| &mut data.exits))
        {
            if exit.to_source.is_some_and(|source| !source.is_map()) {
                exit.to_area_id = None;
                exit.to_room_number = None;
                exit.to_direction = None;
                exit.to_source = None;
                exit.to_unknown = true;
                exit.to_area_token = None;
            }
        }
        self.room_data
            .retain(|data| data.room_source.is_none_or(|source| source.is_map()));
        self.connections.retain(|connection| {
            connection
                .endpoint_a
                .source
                .is_none_or(|source| source.is_map())
                && connection
                    .endpoint_b
                    .as_ref()
                    .is_none_or(|endpoint| endpoint.source.is_none_or(|source| source.is_map()))
        });
        let connections: HashSet<_> = self
            .connections
            .iter()
            .map(|connection| connection.id)
            .collect();
        for exits in self
            .rooms
            .iter_mut()
            .map(|room| &mut room.exits)
            .chain(self.room_data.iter_mut().map(|data| &mut data.exits))
        {
            exits.retain(|exit| connections.contains(&exit.connection_id));
        }
        self.retain_linked_areas_of_exits();
    }

    /// Keeps of the map's other sources what a copy of it carries
    /// (smudgy-cloudflare format-3.md §5.2): each Secret the caller holds
    /// `copy` on, which an owner Secret's owner always does, and the
    /// caller's own Private additions. A Secret left out takes everything it
    /// keeps along, its bundle being all of it; `linked_areas` is the
    /// caller's to rebuild ([`Self::retain_linked_areas_of_exits`]).
    pub fn keep_copyable_sources(&mut self) {
        let owner = self.area.effective_access().is_owner;
        self.sources.retain(|bundle| match bundle.source {
            SourceId::Map => false,
            SourceId::Private => true,
            SourceId::Secret(_) => {
                bundle.can(cloud_api::secret_action::COPY)
                    || (owner
                        && bundle
                            .ownership
                            .as_deref()
                            .is_none_or(|kind| kind == "owner"))
            }
        });
    }

    /// Keeps each `linked_areas` entry that an exit the document still
    /// carries leads into, from the map or from a source it keeps: the
    /// server lists the maps every source the caller reads leads into, so a
    /// source that goes takes its entries along unless another exit leads
    /// there too. A hidden map's entry is matched by its token.
    pub fn retain_linked_areas_of_exits(&mut self) {
        let mut areas: HashSet<AreaId> = HashSet::new();
        let mut tokens: HashSet<&str> = HashSet::new();
        let exits = self.rooms.iter().flat_map(|room| room.exits.iter()).chain(
            self.sources.iter().flat_map(|bundle| {
                bundle
                    .rooms
                    .iter()
                    .flat_map(|room| room.exits.iter())
                    .chain(bundle.room_data.iter().flat_map(|data| data.exits.iter()))
            }),
        );
        for exit in exits {
            if let Some(to) = exit.to_area_id {
                areas.insert(to);
            }
            if let Some(token) = exit.to_area_token.as_deref() {
                tokens.insert(token);
            }
        }
        let linked = std::mem::take(&mut self.linked_areas);
        self.linked_areas = linked
            .into_iter()
            .filter(
                |entry| match (entry.to_area_id, entry.to_area_token.as_deref()) {
                    (Some(to), _) => areas.contains(&to),
                    (None, Some(token)) => tokens.contains(token),
                    (None, None) => false,
                },
            )
            .collect();
    }
}

/// Serde default for documents that predate `format_version`: they are v1.
/// Deserializes a present field, `null` included, as `Some`; an absent one
/// falls back to the field's default, `None`.
fn present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn format_version_v1() -> u32 {
    1
}

/// The current area document format: the Connection contract (2), with
/// doors in place of an exit's closed and locked flags (3).
pub const AREA_FORMAT_VERSION: u32 = 3;

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
    /// Optional server-global room identity (a GMCP/MSDP room id, opaque
    /// string — hash ids exist in the wild). Indexed by the atlas cache for
    /// O(1) id → room resolution. Not unique-enforced; duplicate bindings are
    /// resolved best-effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

/// Room with all associated data
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// See [`Room::external_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

/// Exit connecting rooms
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    pub id: ExitId,
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: String,
    pub is_hidden: bool,
    /// The exit's door; `None` for an exit without one.
    #[serde(default)]
    pub door: Option<Door>,
    pub weight: f32,
    pub command: String,
    pub connection_id: ConnectionId,
    /// True when the destination area exists but is not visible to the
    /// viewer; the real `to_*` fields are nulled and `to_area_token` set.
    #[serde(default)]
    pub to_unknown: bool,
    /// Per-viewer stable token identifying a hidden destination; converging
    /// exits into the same hidden area share one token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_area_token: Option<String>,
    /// The destination room's source, independently of the exit's owner.
    /// Together with `to_area_id` and `to_room_number`, identifies the room
    /// without depending on which source stores this exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_source: Option<SourceId>,
}

impl Exit {
    /// The Secret of another map this exit leads into, as (that map, the
    /// Secret's id), when it leads into one of its rooms.
    #[must_use]
    pub fn foreign_secret(&self) -> Option<(AreaId, Uuid)> {
        match (self.to_area_id, self.to_source) {
            (Some(map), Some(SourceId::Secret(secret))) => Some((map, secret)),
            _ => None,
        }
    }
}

/// Whether an exit's door is open, closed or locked. Locked implies
/// closed; with "no door", these are Mudlet's door states 0 to 3. Ordered
/// from open to locked, so the most shut of several is their maximum.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Display,
)]
#[serde(rename_all = "snake_case")]
pub enum DoorState {
    #[display("open")]
    Open,
    #[display("closed")]
    Closed,
    #[display("locked")]
    Locked,
}

impl DoorState {
    /// Whether the door is shut: closed or locked.
    #[must_use]
    pub const fn is_shut(self) -> bool {
        matches!(self, Self::Closed | Self::Locked)
    }
}

/// An exit's door: its state, its name, and the command that opens it,
/// which differs from the exit's `command` (the one that goes through it).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Door {
    pub state: DoorState,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub opens_with: Option<String>,
}

/// The longest door name, in Unicode code points (the house limit for
/// names).
pub const DOOR_NAME_LIMIT: usize = 64;
/// The longest command that opens a door, in Unicode code points.
pub const DOOR_COMMAND_LIMIT: usize = 255;

impl Door {
    /// A door in `state` with no name and no command.
    #[must_use]
    pub const fn new(state: DoorState) -> Self {
        Self {
            state,
            name: None,
            opens_with: None,
        }
    }

    /// Refuses a door the server refuses: a name or command that is empty
    /// (none is `None`) or longer than its limit.
    ///
    /// # Errors
    /// [`CloudError::InvalidInput`] naming the field, worded as the server
    /// words it.
    pub fn check(&self) -> CloudResult<()> {
        for (field, value, limit) in [
            ("name", &self.name, DOOR_NAME_LIMIT),
            ("opens_with", &self.opens_with, DOOR_COMMAND_LIMIT),
        ] {
            let Some(value) = value else { continue };
            if value.is_empty() {
                return Err(CloudError::InvalidInput(format!(
                    "`{field}` must not be empty; send null for none"
                )));
            }
            if value.chars().count() > limit {
                return Err(CloudError::InvalidInput(format!(
                    "`{field}` must be at most {limit} characters"
                )));
            }
        }
        Ok(())
    }
}

/// Text label on area
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

/// Graphical shape on area
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

/// Simple property key-value pair
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Property {
    pub name: String,
    pub value: String,
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
    /// `Option<Option<_>>` like [`AreaUpdates::atlas_id`]: absent = unchanged,
    /// present+null = clear the binding, present+string = set it. Omitted
    /// from the wire when absent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option::deserialize"
    )]
    pub external_id: Option<Option<String>>,
}

/// Exit creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ExitArgs {
    /// Client-minted identity (assigned before enqueue so optimistic
    /// references, batches, and retries stay unambiguous); the server mints
    /// one only when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ExitId>,
    /// Explicit membership for compound Connection creation/restore. When
    /// absent, the backend runs the conservative auto-pair/create rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<ConnectionId>,
    /// Stable identity for a fresh Connection. Mutually exclusive with
    /// `connection_id`; when present, auto-pairing is skipped and every
    /// backend creates the new one-member Connection with exactly this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_connection_id: Option<ConnectionId>,
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    /// The destination room's source when it is not an ordinary map room.
    /// The exit remains owned by the source receiving the write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_source: Option<SourceId>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: bool,
    /// The new exit's door; `None` for none.
    #[serde(default)]
    pub door: Option<Door>,
    pub weight: f32,
    pub command: Option<String>,
}

/// Exit creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ExitUpdates {
    pub from_direction: Option<ExitDirection>,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    /// Absent keeps the destination's source; `Some(None)` (wire `null`)
    /// names a map room, `Some(Some(s))` one of the writing source's own
    /// rooms or, with another map's `to_area_id` and a `to_room_number`, a
    /// room of a Secret on that map.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub to_source: Option<Option<SourceId>>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: Option<bool>,
    /// Absent keeps the door; `Some(None)` (wire `null`) removes it with
    /// its name and command; `Some(Some(door))` replaces it whole.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub door: Option<Option<Door>>,
    pub weight: Option<f32>,
    pub command: Option<String>,
    /// Explicitly null the destination (`to_*`) server-side; overrides any
    /// `to_*` fields sent in the same request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_to: Option<bool>,
}

impl ExitUpdates {
    /// `exit` with these updates applied. The updates are in wire form: an
    /// exit into a room of another map's Secret names that map, with the
    /// Secret as its `to_source`. `exit` and the result are in the cache's
    /// form, where such an exit names the Secret's own area and keeps the
    /// map in [`ExitCache::to_secret_map`].
    #[must_use]
    pub fn apply(self, exit: &ExitCache) -> ExitCache {
        let clear_to = self.clear_to == Some(true);
        // Mirror the server's COALESCE semantics: `None` means "unchanged";
        // the only way to null a destination is `clear_to`. (Diverging here
        // would ghost-unlink exits locally on partial updates, e.g. from the
        // script API.) An absent `to_source` keeps the destination's source.
        let (wire_area, to_room_number, to_direction, secret) = if clear_to {
            (None, None, None, None)
        } else {
            let wire_area = self.to_area_id.or(exit.wire_to_area_id());
            let secret = match self.to_source {
                None => exit.to_secret_map.and(exit.to_area_id).map(|area| area.0),
                Some(Some(SourceId::Secret(secret))) if wire_area.is_some() => Some(secret),
                Some(_) => None,
            };
            (
                wire_area,
                self.to_room_number.or(exit.to_room_number),
                self.to_direction.or(exit.to_direction),
                secret,
            )
        };
        let to_private_map = if clear_to {
            None
        } else {
            match self.to_source {
                Some(Some(SourceId::Private)) => wire_area,
                Some(_) => None,
                None => exit.to_private_map.map(|_| wire_area).flatten(),
            }
        };
        let (to_area_id, to_secret_map) = if let Some(map) = to_private_map {
            // A new Private destination stays unroutable until the cache
            // resolves it with the current viewer's source-area identity.
            (
                if exit.to_private_map == Some(map) {
                    exit.to_area_id
                } else {
                    None
                },
                None,
            )
        } else {
            match secret {
                Some(secret) => (Some(AreaId(secret)), wire_area),
                None => (wire_area, None),
            }
        };
        // A locally-set (or cleared) destination is known to the viewer.
        let destination_touched = clear_to
            || self.to_area_id.is_some()
            || self.to_room_number.is_some()
            || self.to_source.is_some();
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
            to_secret_map,
            to_private_map,
            path: self.path.or_else(|| exit.path.clone()),
            is_hidden: self.is_hidden.unwrap_or(exit.is_hidden),
            door: self.door.unwrap_or_else(|| exit.door.clone()),
            weight: self.weight.unwrap_or(exit.weight),
            command: self.command.or_else(|| exit.command.clone()),
            // Connection membership is repaired by the caller when the
            // destination changed (see the area cache's upsert path); the
            // updates themselves never carry it.
            connection_id: exit.connection_id,
            to_unknown,
            to_area_token,
        }
    }
}
/// Label creation/update data
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LabelArgs {
    /// Client-minted identity; see [`ExitArgs::id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<LabelId>,
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
}

impl LabelUpdates {
    /// Returns a copy of `label` with every `Some` field applied.
    #[must_use]
    pub fn apply(self, label: &Label) -> Label {
        Label {
            id: label.id,
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
    /// Client-minted identity; see [`ExitArgs::id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ShapeId>,
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
            level: self.level.unwrap_or(shape.level),
            x: self.x.unwrap_or(shape.x),
            y: self.y.unwrap_or(shape.y),
            width: self.width.unwrap_or(shape.width),
            height: self.height.unwrap_or(shape.height),
            background_color: self
                .background_color
                .or_else(|| shape.background_color.clone()),
            stroke_color: self.stroke_color.or_else(|| shape.stroke_color.clone()),
            shape_type: self.shape_type.unwrap_or_else(|| shape.shape_type.clone()),
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
    /// Creates the map in this clan's library; `atlas_id` must then be one of
    /// the clan's folders. The mapper fills it in when the folder is a clan's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clan_id: Option<Uuid>,
    /// Whose a new clan map is; the server's default is Clan-owned.
    /// Member-owned needs `area.create_member_owned` on the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<clan_maps::MapOwnership>,
    /// Route the new area to the session-lifetime ephemeral tier (in-memory,
    /// never persisted or synced). Client-side routing only — never on the
    /// wire, and single-tier backends ignore it.
    #[serde(skip)]
    pub ephemeral: bool,
    /// Area properties the new area starts with. Client-side only — never on
    /// the wire: the local and ephemeral backends write them into the document
    /// they create, and the mapper saves them to a cloud area with a follow-up
    /// area mutation.
    #[serde(skip)]
    pub properties: BTreeMap<String, String>,
}

impl CreateAreaRequest {
    /// The initial properties as an area document stores them.
    #[must_use]
    pub fn document_properties(&self) -> Vec<Property> {
        self.properties
            .iter()
            .map(|(name, value)| Property {
                name: name.clone(),
                value: value.clone(),
            })
            .collect()
    }
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option::deserialize"
    )]
    pub atlas_id: Option<Option<AtlasId>>,
}

/// Deserializer preserving the two-level `Option` distinction — absent key
/// (`None`) versus explicit `null` (`Some(None)`) — which serde's default
/// otherwise folds together. Pairs with `skip_serializing_if` so the same
/// distinction survives serialization; mirrors the server's ingest.
mod double_option {
    // The nested Option IS the wire contract here (absent vs null vs value).
    #[allow(clippy::option_option)]
    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: serde::Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        serde::Deserialize::deserialize(deserializer).map(Some)
    }
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
    use super::{
        Area, AreaAccess, AreaUpdates, AtlasId, CreateAreaRequest,
        MAP_STORAGE_COMPATIBILITY_REMOVAL_VERSION, Uuid,
    };
    use serde_json::json;

    /// A map served without an access block is the viewer's own, unless it
    /// is a clan's: then its actions say what the viewer may do with it.
    #[test]
    fn a_map_served_without_access_is_owned_unless_a_clans() {
        let row = |extra: serde_json::Value| -> Area {
            let mut row = json!({
                "id": Uuid::from_u128(1), "user_id": null, "atlas_id": Uuid::from_u128(2),
                "name": "Solace", "created_at": "2026-10-06T00:00:00Z", "rev": 1
            });
            row.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(row).unwrap()
        };

        assert_eq!(row(json!({})).effective_access(), AreaAccess::OWNER);

        let created = row(json!({ "clan_id": Uuid::from_u128(3) }));
        assert_eq!(
            created.effective_access(),
            AreaAccess {
                is_owner: false,
                can_edit: false,
                can_reshare: false,
                can_copy: false,
                can_admin: false,
                include_secrets: false,
            }
        );

        let listed = row(json!({
            "clan_id": Uuid::from_u128(3),
            "actions": ["area.read", "area.edit", "area.copy", "area.delete"]
        }));
        let access = listed.effective_access();
        assert!(!access.is_owner && !access.can_admin && !access.can_reshare);
        assert!(access.can_edit && access.can_copy);

        let served = AreaAccess {
            can_edit: true,
            ..AreaAccess::of_clan_map(None)
        };
        let mut with_access = listed.clone();
        with_access.access = Some(served);
        assert_eq!(with_access.effective_access(), served);
    }

    /// The create body is the server's contract; routing and initial
    /// properties stay client-side.
    #[test]
    fn create_area_request_keeps_client_fields_off_the_wire() {
        let request = CreateAreaRequest {
            name: "The Deathlands".to_string(),
            atlas_id: None,
            clan_id: None,
            ownership: None,
            ephemeral: true,
            properties: [("nukefire.area".to_string(), "the deathlands".to_string())].into(),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({ "name": "The Deathlands", "atlas_id": null })
        );
    }

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

    /// Versioned imports preserve legacy secrecy as Private content rather
    /// than letting the current DTO silently ignore the obsolete flags.
    #[test]
    fn documents_written_with_secrecy_flags_still_read() {
        let document = json!({
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "user_id": null,
            "atlas_id": null,
            "name": "Old map",
            "created_at": "2026-07-01T00:00:00Z",
            "rev": 3,
            "format_version": 2,
            "properties": [{ "name": "zone", "value": "old", "is_secret": true }],
            "rooms": [{
                "room_number": 1, "title": "Vault", "description": "", "level": 0,
                "x": 0.0, "y": 0.0, "color": "", "is_secret": true,
                "properties": [{ "name": "notes", "value": "x", "is_secret": true }],
                "exits": [{
                    "id": "00000000-0000-4000-8000-000000000001", "from_direction": "North",
                    "to_area_id": null, "to_room_number": null, "to_direction": null,
                    "path": "", "is_hidden": false, "door": null,
                    "weight": 1.0, "command": "",
                    "connection_id": "00000000-0000-4000-8000-0000000000c1",
                    "is_secret": true
                }]
            }],
            "labels": [{
                "id": "00000000-0000-4000-8000-0000000000a1", "level": 0, "x": 0.0, "y": 0.0,
                "width": 1.0, "height": 1.0, "horizontal_alignment": "Center",
                "vertical_alignment": "Center", "text": "X", "color": "", "background_color": "",
                "font_size": 12, "font_weight": 400, "is_secret": true
            }],
            "shapes": [{
                "id": "00000000-0000-4000-8000-0000000000b1", "level": 0, "x": 0.0, "y": 0.0,
                "width": 1.0, "height": 1.0, "background_color": null, "stroke_color": null,
                "shape_type": "Rectangle", "border_radius": 0.0, "stroke_width": 1.0,
                "is_secret": true
            }]
        });
        let details = serde_json::from_value::<crate::mapper::AreaImportDocument>(document)
            .expect("an old document still reads")
            .into_inner();
        assert!(
            details.rooms.is_empty()
                && details.labels.is_empty()
                && details.shapes.is_empty()
                && details.properties.is_empty()
        );
        let private = &details.sources[0];
        assert_eq!(private.source, super::SourceId::Private);
        assert_eq!(private.rooms[0].title, "Vault");
        assert_eq!(private.rooms[0].exits.len(), 1);
        assert_eq!(private.labels.len(), 1);
        assert_eq!(private.shapes.len(), 1);
        assert_eq!(private.properties[0].value, "old");
        let written = serde_json::to_string(&details).expect("serializes");
        assert!(!written.contains("is_secret"), "{written}");
    }

    #[test]
    fn map_storage_compatibility_window_expires_before_0_6() {
        let running = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("Cargo package versions are valid semver");
        let removal = semver::Version::parse(MAP_STORAGE_COMPATIBILITY_REMOVAL_VERSION)
            .expect("the removal milestone is valid semver");
        assert!(
            (running.major, running.minor) < (removal.major, removal.minor),
            "remove the mapper's ephemeral compatibility shims before releasing {running}; \
             they are supported only through 0.5.x"
        );

        const RUST_DEPRECATION: &str = "supported through Smudgy 0.5.x and removed in 0.6.0";
        let mapper_source = include_str!("mapper.rs");
        assert_eq!(
            mapper_source.matches(RUST_DEPRECATION).count(),
            3,
            "the Rust compatibility catalog must stay explicitly deprecated: \
             create_area_ephemeral, is_ephemeral, and ephemeral_area_ids"
        );
    }
}
