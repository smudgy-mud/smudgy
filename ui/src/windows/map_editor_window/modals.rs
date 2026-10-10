//! Window-local modals: create-area naming and filing, the move/save-to-folder
//! picker, delete-area confirmation (the only destructive action without
//! undo), and the share dialog (share the map or one of its Secrets, and
//! manage who has access).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

pub(super) use crate::widgets::dialog::scrolling_body;
use iced::alignment::Vertical;

use iced::widget::{
    Column, button, checkbox, column, container, pick_list, radio, row, scrollable, space, text,
    text_input,
};
use iced::{Length, Padding, Task};
use smudgy_cloud::cloud_api::{
    CreateShareRequest, FriendView, GrantTreeNode, SecretGrant, ShareDirection, ShareGrant,
    ShareGrantRow, SharePatch, ShareScope, TransferRecipient, secret_action,
};
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{
    AreaAccess, AreaId, AtlasId, CloudApiClient, CloudError, ConnectionId, ExitDirection,
    MapDestination, MapStorage, Mapper, RoomNumber, SourceId, Uuid,
};
use smudgy_map_widget::sources;

use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::folder_picker::{FolderPicker, NewFolderForm, NewFolderMessage};
use super::{MapEditorWindow, Message, commands};

/// A Link-tool gesture, kept while its new room's number is unconfirmed so
/// a collision can recreate the link under another number.
#[derive(Debug, Clone)]
pub struct LinkDraft {
    pub area_id: AreaId,
    pub from: RoomNumber,
    pub target: commands::NewExitTarget,
    pub from_direction: ExitDirection,
    pub to_direction: ExitDirection,
    pub one_way: bool,
    pub pair_candidate: Option<ConnectionId>,
    pub pair_with_candidate: bool,
}

#[derive(Debug, Clone)]
pub enum Modal {
    CreateArea {
        name: String,
        error: Option<String>,
        /// Where the new map is stored and the folder it is filed in. Fixed
        /// to the folder when opened from a folder's "new map" affordance.
        folder: FolderPicker,
        /// Its folder or the map is being made.
        busy: bool,
        /// In a clan's folder where the viewer may make Member-owned maps:
        /// whose the new map is.
        ownership: Option<NewMapOwnership>,
    },
    ReviewMove {
        id: Uuid,
        from: smudgy_cloud::SourceId,
        to: smudgy_cloud::SourceId,
        destination: String,
        content: smudgy_cloud::MovedContent,
        source_names: std::collections::BTreeMap<smudgy_cloud::SourceId, String>,
        reviewed: Option<Box<smudgy_cloud::access_review::ReviewedMove>>,
    },
    ReviewFiling(super::filing::Dialog),
    ConfirmDeleteArea {
        area_id: AreaId,
        name: String,
        room_count: usize,
    },
    /// Boundary links are omitted by default, but copy/cut must make that
    /// loss visible and offer the specified dangling-link representation.
    ConfirmCopySelection {
        boundary_count: usize,
        include_boundary_links: bool,
        cut_after_copy: bool,
    },
    /// Name a new folder (atlas) and pick its tier.
    CreateAtlas {
        name: String,
        error: Option<String>,
        /// Chosen durable storage tier.
        storage: MapStorage,
        /// Whether the cloud tier is selectable (i.e. signed in). When false
        /// the folder is forced local.
        cloud_available: bool,
    },
    /// Confirm copying a complete atlas to the other durable tier and only
    /// deleting the source after the destination is acknowledged.
    MoveAtlasStorage {
        atlas_id: AtlasId,
        name: String,
        area_count: i64,
        source: MapStorage,
        destination: MapStorage,
    },
    ReviewLocalMove {
        request: super::local_move::Request,
        reviews: Option<Vec<smudgy_cloud::relocation::LocalMoveReview>>,
        error: Option<String>,
        names: Vec<String>,
    },
    /// Deleting a folder. Its maps move first, into the folder picked here.
    ConfirmDeleteAtlas {
        atlas_id: AtlasId,
        name: String,
        /// The maps in it.
        maps: Vec<AreaId>,
        /// Where they go: another of the viewer's folders in the same
        /// storage, or a new one. `None` for an empty folder.
        folder: Option<FolderPicker>,
        busy: bool,
        error: Option<String>,
    },
    /// "Move to folder" picker for an owned area, and "Save to folder" for a
    /// session map. Every destination is a folder.
    MoveArea {
        area_id: AreaId,
        area_name: String,
        current: MapDestination,
        /// Available explicit destinations and labels, name-sorted.
        targets: Vec<(MapDestination, String)>,
        /// A new folder of the viewer's own can be made here (not when a
        /// clan's map moves between the clan's folders).
        make_folder: bool,
        /// The form naming a new folder to move the map into, while open.
        new_folder: Option<NewFolderForm>,
    },
    /// The "Share folder…" dialog: atlas-scope grants in one step.
    ShareAtlas(ShareAtlasDialog),
    /// The share dialog: share the active map, or one of its Secrets, with
    /// friends, and manage who has access as far as the viewer may.
    Share(Box<ShareDialog>),
    /// "Copy to my maps": clone a shared area (and optionally its whole
    /// atlas, when the atlas is visible) into the viewer's own maps.
    CopyArea(CopyAreaDialog),
    /// Offer to transfer ownership of an area or atlas to a friend.
    TransferOffer(TransferDialog),
    /// A clan dialog: a new clan folder, deleting one, Incoming maps, or
    /// sharing a clan folder with groups.
    Clan(Box<super::clan_maps::ClanModal>),
    /// A clan map's Share dialog.
    ClanMapShare(Box<super::clan_map_share::ClanMapShareDialog>),
    /// "Put in a clan…" for the viewer's own map.
    PutInClan(Box<super::clan_map_share::PutInClanDialog>),
    /// The per-atlas (or atlas-less area) "Show this atlas on:" checklist —
    /// the cloud-map scope override surface. Toggles apply live; `checked` is
    /// the display buffer kept in step with the daemon-owned store.
    ServersChecklist {
        target: super::ScopeTarget,
        name: String,
        servers: Vec<String>,
        checked: std::collections::BTreeSet<String>,
    },
    /// An action on several maps and folders chosen in the map list.
    Multi(Box<super::multi_select::MultiDialog>),
}

/// Whose a new map in a clan's folder is: Clan-owned, the default, or
/// Member-owned where the viewer may make those there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewMapOwnership {
    /// The viewer may make Clan-owned maps there too.
    pub clan_allowed: bool,
    pub picked: smudgy_cloud::clan_maps::MapOwnership,
}

impl NewMapOwnership {
    /// The choice a clan folder offers, from the viewer's actions on it;
    /// `None` where Member-owned maps can't be made, so the map is
    /// Clan-owned without asking.
    #[must_use]
    pub fn for_folder(actions: &BTreeSet<String>) -> Option<Self> {
        use smudgy_cloud::clan_maps::MapOwnership;
        use smudgy_cloud::clans::action;
        if !actions.contains(action::CREATE_MEMBER_OWNED_AREA) {
            return None;
        }
        let clan_allowed = actions.contains(action::CREATE_AREA);
        Some(Self {
            clan_allowed,
            picked: if clan_allowed {
                MapOwnership::Clan
            } else {
                MapOwnership::Members
            },
        })
    }
}

/// State of the copy-to-my-maps modal.
#[derive(Debug, Clone)]
pub struct CopyAreaDialog {
    pub source: AreaId,
    pub source_name: String,
    /// Editable name for the clone, prefilled "<source name> (copy)".
    pub name: String,
    /// The source's atlas when visible to the viewer (rare: shared rows
    /// usually have a redacted `atlas_id`); enables "Copy whole atlas…".
    pub atlas_id: Option<AtlasId>,
    /// A copy request is in flight.
    pub busy: bool,
    pub error: Option<String>,
    /// Human-readable report from a whole-atlas copy.
    pub atlas_report: Option<String>,
    /// This is an owner self-copy ("Duplicate"): the dialog reads "Duplicate
    /// map", offers no atlas option, and the resulting clone starts inactive.
    pub duplicate: bool,
    /// The cloud folder the copy is filed in.
    pub folder: FolderPicker,
    /// Which of the Secrets the viewer reads on the map come along.
    pub secrets: SecretsAlong,
}

/// The Secrets a copy of a map carries, among those the viewer reads on it:
/// only the ones they hold `copy` on come along. Secrets they do not read
/// never count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecretsAlong {
    /// Secrets the viewer reads on the map.
    pub read: usize,
    /// Of those, the ones they may copy.
    pub copied: usize,
}

impl SecretsAlong {
    /// Counts the Secrets among a map's readable sources.
    pub fn of(area: &AreaCache) -> Self {
        let mut along = Self::default();
        for bundle in area
            .meta()
            .sources
            .iter()
            .filter(|bundle| bundle.source.is_secret())
        {
            along.read += 1;
            if bundle.actions.contains(secret_action::COPY) {
                along.copied += 1;
            }
        }
        along
    }

    /// The copy dialog's line about Secrets; none for a map the viewer
    /// reads no Secret on.
    fn line(self) -> Option<String> {
        match (self.read, self.copied) {
            (0, _) => None,
            (_, 0) => Some(crate::i18n::t!("mapper-copy-secrets-none")),
            (_, copied) => Some(crate::i18n::t!("mapper-copy-secrets-along", "count" => copied)),
        }
    }
}

// ===========================================================================
// Share dialog state
// ===========================================================================

/// Which capability flag a toggle message refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantFlag {
    Edit,
    Reshare,
    Copy,
    /// Full-deputy. Owner-minted only; implies all lower caps.
    Admin,
}

/// Inline flag editing for one existing grant row.
#[derive(Debug, Clone)]
pub struct GrantEdit {
    pub id: Uuid,
    /// The flags as the server last reported them, for change detection.
    pub original: ShareGrant,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    /// Full-deputy flag on this grant.
    pub can_admin: bool,
    /// Whether `can_admin` may be changed here (the true owner, owner-minted
    /// root only).
    pub allow_admin: bool,
    pub saving: bool,
    pub error: Option<String>,
}

/// One box of a Secret share: what a friend may do beyond seeing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretFlag {
    /// `add`.
    Add,
    /// `edit` and `remove`.
    Edit,
    /// `manage_access`. Only the map's owner gives it.
    Share,
    /// `copy`: the Secret comes along when they copy the map. It is in no
    /// preset, and only someone who holds it gives it.
    Copy,
}

impl SecretFlag {
    const ALL: [Self; 4] = [Self::Add, Self::Edit, Self::Share, Self::Copy];

    /// The wire actions the box stands for.
    fn actions(self) -> &'static [&'static str] {
        match self {
            Self::Add => &[secret_action::ADD],
            Self::Edit => &[secret_action::EDIT, secret_action::REMOVE],
            Self::Share => &[secret_action::MANAGE_ACCESS],
            Self::Copy => &[secret_action::COPY],
        }
    }
}

/// The Secret boxes of a new share, or of a grant being edited. None
/// checked is view only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one per box the dialog shows
pub struct SecretFlags {
    pub add: bool,
    pub edit: bool,
    pub share: bool,
    pub copy: bool,
}

impl SecretFlags {
    fn get(self, flag: SecretFlag) -> bool {
        match flag {
            SecretFlag::Add => self.add,
            SecretFlag::Edit => self.edit,
            SecretFlag::Share => self.share,
            SecretFlag::Copy => self.copy,
        }
    }

    fn set(&mut self, flag: SecretFlag, value: bool) {
        match flag {
            SecretFlag::Add => self.add = value,
            SecretFlag::Edit => self.edit = value,
            SecretFlag::Share => self.share = value,
            SecretFlag::Copy => self.copy = value,
        }
    }

    /// The boxes a grant's actions check. Can edit stands for either of
    /// `edit` and `remove`.
    fn of(actions: &BTreeSet<String>) -> Self {
        let mut flags = Self::default();
        for flag in SecretFlag::ALL {
            flags.set(
                flag,
                flag.actions()
                    .iter()
                    .any(|action| actions.contains(*action)),
            );
        }
        flags
    }

    /// The actions a new grant with these boxes carries.
    fn actions(self) -> Vec<&'static str> {
        SecretFlag::ALL
            .into_iter()
            .filter(|flag| self.get(*flag))
            .flat_map(|flag| flag.actions().iter().copied())
            .collect()
    }

    /// The actions an edited grant carries: a box left as it was keeps
    /// exactly what the grant had, so saving never widens a box it did not
    /// touch.
    fn edited_actions(self, original: &BTreeSet<String>) -> Vec<&'static str> {
        let before = Self::of(original);
        let mut actions = Vec::new();
        for flag in SecretFlag::ALL {
            if self.get(flag) == before.get(flag) {
                actions.extend(
                    flag.actions()
                        .iter()
                        .filter(|action| original.contains(**action)),
                );
            } else if self.get(flag) {
                actions.extend(flag.actions());
            }
        }
        actions
    }

    /// These boxes with every one the sharer may not give turned off.
    fn clamped(mut self, sharer: &SecretSharer) -> Self {
        for flag in SecretFlag::ALL {
            if !sharer.may_give(flag) {
                self.set(flag, false);
            }
        }
        self
    }
}

/// The viewer as a sharer of one Secret. The map's owner holds every
/// action; anyone else shares a Secret they hold `manage_access` on, within
/// their own actions and never `manage_access` itself.
#[derive(Debug, Clone, Copy)]
struct SecretSharer<'a> {
    is_owner: bool,
    held: &'a BTreeSet<String>,
}

impl SecretSharer<'_> {
    /// Whether the viewer may check `flag` for a friend.
    fn may_give(&self, flag: SecretFlag) -> bool {
        match flag {
            SecretFlag::Share => self.is_owner,
            _ => {
                self.is_owner
                    || flag
                        .actions()
                        .iter()
                        .all(|action| self.held.contains(*action))
            }
        }
    }

    /// Whether the viewer may change or revoke `grant`: the owner any, a
    /// holder of `manage_access` any that does not carry it, whoever issued
    /// it.
    fn manages(&self, grant: &SecretGrant) -> bool {
        self.is_owner
            || (self.held.contains(secret_action::MANAGE_ACCESS)
                && !grant.can(secret_action::MANAGE_ACCESS))
    }

    /// Whether `flag` can change on `edit`: boxes the viewer may give, and
    /// boxes already on, which they may only clear. Can share is the owner's,
    /// on grants the owner issued.
    fn may_toggle(&self, edit: &SecretGrantEdit, flag: SecretFlag) -> bool {
        match flag {
            SecretFlag::Share => {
                self.is_owner && edit.original.grantor_id == edit.original.owner_id
            }
            _ => self.may_give(flag) || SecretFlags::of(&edit.original.actions).get(flag),
        }
    }
}

/// Inline box editing for one existing Secret grant row.
#[derive(Debug, Clone)]
pub struct SecretGrantEdit {
    pub original: SecretGrant,
    pub flags: SecretFlags,
    pub saving: bool,
    pub error: Option<String>,
}

/// A place the Share dialog can share: the map, or one of its Secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct SharePlace {
    pub source: SourceId,
    pub name: String,
    /// The place's ring color; the map has none.
    pub color: Option<iced::Color>,
    /// The viewer's own actions on a Secret; empty for the map.
    pub held: BTreeSet<String>,
    /// A Clan Secret's clan: it is shared with that clan's groups and
    /// members, under the clan's rules.
    pub clan_id: Option<Uuid>,
}

impl std::fmt::Display for SharePlace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// How sharing with one friend went.
#[derive(Debug, Clone)]
pub enum ShareOutcome {
    Shared,
    /// Shared a Secret, and the friend can now see the map too.
    SharedWithMap,
    Failed(CloudError),
}

#[derive(Debug, Clone)]
pub struct ShareDialog {
    pub area_id: AreaId,
    pub area_name: String,
    /// The viewer owns the map (admin, every Secret).
    pub is_owner: bool,
    /// The viewer may share the map itself.
    pub shares_map: bool,
    /// The map's owner, who is never a recipient.
    pub owner_id: Option<Uuid>,
    /// Owner attribution for grants not made by the viewer (re-share case).
    pub owner_nickname: Option<String>,
    pub viewer_id: Option<Uuid>,
    /// What the picker offers, in order: the map when the viewer may share
    /// it, then each Secret they may share.
    pub places: Vec<SharePlace>,
    /// The picked place.
    pub target: SourceId,
    /// `None` while loading.
    pub friends: Option<Result<Vec<FriendView>, String>>,
    pub filter: String,
    /// Checked friends; they stay checked when the picker changes.
    pub selected: HashSet<Uuid>,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    /// Grant full-deputy (`can_admin`) to the selected recipients. Owner-only.
    pub can_admin: bool,
    /// The boxes of a new Secret share.
    pub secret_flags: SecretFlags,
    /// The grant tree reaching this map; `None` while loading, and never
    /// fetched for a viewer who may not share the map.
    pub tree: Option<Result<Vec<GrantTreeNode>, String>>,
    /// Each Secret's grants as last loaded; absent while loading.
    pub secret_grants: HashMap<SourceId, Result<Vec<SecretGrant>, String>>,
    pub editing: Option<GrantEdit>,
    pub secret_editing: Option<SecretGrantEdit>,
    /// Grant pending two-step revoke confirmation.
    pub revoking: Option<Uuid>,
    pub revoke_busy: bool,
    pub submitting: bool,
    /// Per-recipient outcomes of the last Share press.
    pub results: Vec<(String, ShareOutcome)>,
    /// Errors from manage-tree operations (revoke, refresh).
    pub manage_error: Option<String>,
    /// §4.2 "disclose servers": `(host, checked)` snapshot of the shared
    /// thing's associated server entries' hosts, computed at dialog open and
    /// never refreshed mid-dialog. Empty means nothing to disclose (no section
    /// rendered). Checked hosts flow into every `CreateShareRequest.host_hints`.
    pub host_hints: Vec<(String, bool)>,
    /// The viewer may put this map in one of their clans: a personal cloud
    /// map they own, while they are in a clan.
    pub put_in_clan: bool,
    /// Each picked Clan Secret's groups, members and who reads it.
    pub clan_data: HashMap<SourceId, Result<super::clan_secret_share::ClanShareData, String>>,
}

#[derive(Debug, Clone)]
pub enum ShareMessage {
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    TreeLoaded(Result<Vec<GrantTreeNode>, CloudError>),
    SecretGrantsLoaded {
        secret: SourceId,
        result: Result<Vec<SecretGrant>, CloudError>,
    },
    /// A Clan Secret's grants with its clan's recipients.
    ClanLoaded {
        secret: SourceId,
        result:
            Result<Box<(Vec<SecretGrant>, super::clan_secret_share::ClanShareData)>, CloudError>,
    },
    PlacePicked(SharePlace),
    FilterChanged(String),
    RecipientToggled(Uuid, bool),
    FlagToggled(GrantFlag, bool),
    SecretFlagToggled(SecretFlag, bool),
    /// Check/uncheck one disclosed host (§4.2 grantor consent).
    HostHintToggled(String, bool),
    Submit,
    Submitted {
        target: SourceId,
        results: Vec<(String, ShareOutcome)>,
    },
    EditGrant(Uuid),
    EditFlagToggled(GrantFlag, bool),
    EditSecretFlagToggled(SecretFlag, bool),
    EditCancelled,
    EditSaved,
    EditResult {
        id: Uuid,
        result: Result<ShareGrant, CloudError>,
    },
    SecretEditResult {
        id: Uuid,
        result: Result<SecretGrant, CloudError>,
    },
    RevokeRequested(Uuid),
    RevokeCancelled,
    RevokeConfirmed,
    RevokeResult {
        target: SourceId,
        result: Result<(), CloudError>,
    },
    /// "Put in a clan…": opens its own dialog.
    PutInClan,
}

fn share(message: ShareMessage) -> Message {
    Message::Share(message)
}

// ===========================================================================
// Share-folder (atlas-scope) dialog
// ===========================================================================

/// State of the "Share folder…" dialog. Deliberately simpler than the
/// area [`ShareDialog`]: this is just recipients + capabilities, plus a "who has access" list of
/// the existing atlas-scope grants.
#[derive(Debug, Clone)]
pub struct ShareAtlasDialog {
    pub atlas_id: AtlasId,
    pub atlas_name: String,
    /// `None` while loading.
    pub friends: Option<Result<Vec<FriendView>, String>>,
    pub filter: String,
    pub selected: HashSet<Uuid>,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    /// Grant full-deputy (`can_admin`) on the whole folder. Owner-only.
    pub can_admin: bool,
    pub submitting: bool,
    /// Per-recipient outcomes of the last Share press.
    pub results: Vec<(String, Result<(), CloudError>)>,
    /// All shares succeeded; the dialog closes itself after a beat.
    pub close_pending: bool,
    /// Existing atlas-scope grants for this folder (from
    /// `GET /shares?direction=given`, filtered by `atlas_id`). `None` while
    /// loading.
    pub grants: Option<Result<Vec<ShareGrantRow>, String>>,
    /// Grant pending two-step revoke confirmation.
    pub revoking: Option<Uuid>,
    pub revoke_busy: bool,
    pub manage_error: Option<String>,
    /// §4.2 "disclose servers": `(host, checked)` snapshot of the atlas's
    /// associated server entries' hosts, computed at open. See
    /// [`ShareDialog::host_hints`].
    pub host_hints: Vec<(String, bool)>,
}

#[derive(Debug, Clone)]
pub enum ShareAtlasMessage {
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    GrantsLoaded(Result<Vec<ShareGrantRow>, CloudError>),
    FilterChanged(String),
    RecipientToggled(Uuid, bool),
    FlagToggled(GrantFlag, bool),
    /// Check/uncheck one disclosed host (§4.2 grantor consent).
    HostHintToggled(String, bool),
    Submit,
    Submitted(Vec<(String, Result<(), CloudError>)>),
    CloseTick,
    RevokeRequested(Uuid),
    RevokeCancelled,
    RevokeConfirmed,
    RevokeResult(Result<(), CloudError>),
}

fn share_atlas(message: ShareAtlasMessage) -> Message {
    Message::ShareAtlas(message)
}

// ===========================================================================
// Ownership-transfer offer dialog
// ===========================================================================

#[derive(Debug, Clone)]
pub enum TransferSubject {
    Area(AreaId, String),
    Atlas(AtlasId, String),
}

impl TransferSubject {
    fn name(&self) -> &str {
        match self {
            Self::Area(_, name) | Self::Atlas(_, name) => name,
        }
    }
}

/// Offer to transfer ownership of a map or folder to a friend, who completes
/// it from their Friends panel, or directly into an authorized clan destination.
/// Friend offers never expire.
#[derive(Debug, Clone)]
pub struct TransferDialog {
    pub subject: TransferSubject,
    /// `None` while loading.
    pub friends: Option<Result<Vec<FriendView>, String>>,
    /// Destinations where the viewer can complete this transfer.
    pub destinations: super::clan_maps::TransferDestinations,
    pub operation: Uuid,
    pub filter: String,
    pub selected: Option<TransferRecipient>,
    /// Whose each map becomes in a clan: the clan's ("give to the clan"), or
    /// still the viewer's, as a Member-owned map ("stays mine").
    pub ownership: smudgy_cloud::clan_maps::MapOwnership,
    pub submitting: bool,
    pub error: Option<String>,
    /// The offer was sent; the dialog closes itself after a beat.
    pub sent: bool,
}

#[derive(Debug, Clone)]
pub enum TransferMessage {
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    FilterChanged(String),
    RecipientSelected(TransferRecipient),
    FolderPicked(super::clan_maps::FolderChoice),
    /// Whose the maps become in the picked clan.
    OwnershipPicked(smudgy_cloud::clan_maps::MapOwnership),
    Submit,
    Submitted(Result<(), CloudError>),
    CloseTick,
}

fn transfer(message: TransferMessage) -> Message {
    Message::Transfer(message)
}

/// Open the transfer-offer modal and load the friends list. The caller gates on
/// raw ownership — transfer is `is_owner`-only (a `can_admin` deputy can't) —
/// and never offers a clan's map or folder.
pub(super) fn open_transfer_dialog(
    window: &mut MapEditorWindow,
    subject: TransferSubject,
) -> Update<Message, super::Event> {
    let destinations = super::clan_maps::TransferDestinations::new(
        window,
        matches!(subject, TransferSubject::Area(..)),
        matches!(subject, TransferSubject::Atlas(..)),
    );
    window.modal = Some(Modal::TransferOffer(TransferDialog {
        subject,
        friends: None,
        destinations,
        operation: Uuid::new_v4(),
        filter: String::new(),
        selected: None,
        ownership: smudgy_cloud::clan_maps::MapOwnership::Clan,
        submitting: false,
        error: None,
        sent: false,
    }));
    let client = window.cloud.client.clone();
    Update::with_task(Task::perform(
        async move { client.friends().await },
        |result| transfer(TransferMessage::FriendsLoaded(result)),
    ))
}

pub(super) fn update_transfer(
    window: &mut MapEditorWindow,
    message: TransferMessage,
) -> Update<Message, super::Event> {
    let Some(Modal::TransferOffer(dialog)) = &mut window.modal else {
        return Update::none();
    };
    if dialog.submitting
        && !matches!(
            message,
            TransferMessage::Submitted(_)
                | TransferMessage::CloseTick
                | TransferMessage::FriendsLoaded(_)
        )
    {
        return Update::none();
    }
    match message {
        TransferMessage::FriendsLoaded(result) => {
            dialog.friends = Some(result.map_err(|error| display_error(&error)));
            Update::none()
        }
        TransferMessage::FilterChanged(value) => {
            dialog.filter = value;
            Update::none()
        }
        TransferMessage::RecipientSelected(recipient) => {
            dialog.selected = Some(recipient);
            dialog.destinations.select(recipient);
            dialog.operation = Uuid::new_v4();
            dialog.error = None;
            Update::none()
        }
        TransferMessage::FolderPicked(folder) => {
            dialog.destinations.folder = Some(folder);
            dialog.operation = Uuid::new_v4();
            Update::none()
        }
        TransferMessage::OwnershipPicked(ownership) => {
            dialog.operation = Uuid::new_v4();
            dialog.ownership = ownership;
            if let Some(TransferRecipient::Clan(clan, _)) = dialog.selected {
                dialog.selected = Some(TransferRecipient::Clan(clan, ownership));
            }
            Update::none()
        }
        TransferMessage::Submit => {
            let Some(to) = dialog.selected else {
                return Update::none();
            };
            if dialog.submitting
                || !dialog
                    .destinations
                    .ready(to, matches!(dialog.subject, TransferSubject::Area(..)))
            {
                return Update::none();
            }
            dialog.submitting = true;
            dialog.error = None;
            let client = window.cloud.client.clone();
            let subject = dialog.subject.clone();
            let operation = dialog.operation;
            let folder = dialog.destinations.folder.as_ref().map(|folder| folder.id);
            Update::with_task(Task::perform(
                async move {
                    match (subject, to) {
                        (
                            TransferSubject::Area(area, _),
                            TransferRecipient::Clan(clan, ownership),
                        ) => client
                            .transfer_area_to_clan(
                                area,
                                clan,
                                ownership,
                                folder.expect("authorized folder"),
                                operation,
                            )
                            .await
                            .map(|_| ()),
                        (
                            TransferSubject::Atlas(atlas, _),
                            TransferRecipient::Clan(clan, ownership),
                        ) => client
                            .transfer_atlas_to_clan(atlas, clan, ownership, operation)
                            .await
                            .map(|_| ()),
                        (TransferSubject::Area(area, _), to) => {
                            client.offer_area_transfer(area, to).await.map(|_| ())
                        }
                        (TransferSubject::Atlas(atlas, _), to) => {
                            client.offer_atlas_transfer(atlas, to).await.map(|_| ())
                        }
                    }
                },
                |result| transfer(TransferMessage::Submitted(result)),
            ))
        }
        TransferMessage::Submitted(result) => match result {
            Ok(()) => {
                dialog.sent = true;
                window.mapper.sync_now();
                Update::with_task(Task::perform(
                    async { tokio::time::sleep(Duration::from_millis(1600)).await },
                    |()| transfer(TransferMessage::CloseTick),
                ))
            }
            Err(error) => {
                dialog.submitting = false;
                dialog.error = Some(transfer_error_message(&error, dialog.selected));
                Update::none()
            }
        },
        TransferMessage::CloseTick => {
            if matches!(&window.modal, Some(Modal::TransferOffer(d)) if d.sent) {
                window.modal = None;
            }
            Update::none()
        }
    }
}

fn transfer_error_message(error: &CloudError, to: Option<TransferRecipient>) -> String {
    match (error, to) {
        (CloudError::NotFoundOrNoAccess, Some(TransferRecipient::Clan(..))) => {
            crate::i18n::t!("mapper-transfer-owner-clan-only")
        }
        (CloudError::NotFoundOrNoAccess, _) => {
            crate::i18n::t!("mapper-transfer-owner-friend-only")
        }
        (other, _) => display_error(other),
    }
}

impl TransferDialog {
    /// The clan the offer goes to, when one is picked.
    fn chosen_clan(&self) -> Option<&str> {
        let Some(TransferRecipient::Clan(id, _)) = self.selected else {
            return None;
        };
        self.destinations
            .clans
            .iter()
            .find(|(clan, _)| *clan == id)
            .map(|(_, name)| name.as_str())
    }
}

/// "Give it to the clan" (Clan-owned) or "It stays mine" (Member-owned):
/// whose a map becomes in a clan, with a line saying what that means.
pub(super) fn ownership_choice<'a>(
    picked: smudgy_cloud::clan_maps::MapOwnership,
    is_map: bool,
    on_pick: impl Fn(smudgy_cloud::clan_maps::MapOwnership) -> Message + Copy + 'a,
) -> ThemedElement<'a, Message> {
    use smudgy_cloud::clan_maps::MapOwnership;
    let option = |ownership: MapOwnership, label: String, help: String| {
        column![
            radio(label, ownership, Some(picked), on_pick)
                .size(14)
                .text_size(13),
            container(text(help).size(11).style(muted)).padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 22.0,
                right: 0.0,
            }),
        ]
        .spacing(2)
    };
    let (clan_help, mine_help) = if is_map {
        (
            crate::i18n::t!("clan-share-give-to-clan-help"),
            crate::i18n::t!("clan-share-stays-mine-help"),
        )
    } else {
        (
            crate::i18n::t!("clan-share-give-folder-to-clan-help"),
            crate::i18n::t!("clan-share-folder-stays-mine-help"),
        )
    };
    column![
        option(
            MapOwnership::Clan,
            crate::i18n::t!("clan-share-give-to-clan"),
            clan_help,
        ),
        option(
            MapOwnership::Members,
            crate::i18n::t!("clan-share-stays-mine"),
            mine_help,
        ),
    ]
    .spacing(6)
    .into()
}

/// Clan-owned or Member-owned, for a new map in a clan's folder.
fn new_map_ownership<'a>(
    picked: smudgy_cloud::clan_maps::MapOwnership,
) -> ThemedElement<'a, Message> {
    use smudgy_cloud::clan_maps::MapOwnership;
    column![
        radio(
            crate::i18n::t!("clan-share-new-map-clan-owned"),
            MapOwnership::Clan,
            Some(picked),
            Message::CreateAreaOwnership,
        )
        .size(14)
        .text_size(13),
        radio(
            crate::i18n::t!("clan-share-new-map-member-owned-choice"),
            MapOwnership::Members,
            Some(picked),
            Message::CreateAreaOwnership,
        )
        .size(14)
        .text_size(13),
        container(
            text(crate::i18n::t!("clan-share-new-map-member-owned-help"))
                .size(11)
                .style(muted)
        )
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 22.0,
            right: 0.0,
        }),
    ]
    .spacing(4)
    .into()
}

/// A recipient's button: primary while picked.
fn recipient_button(
    label: String,
    recipient: TransferRecipient,
    selected: Option<TransferRecipient>,
) -> ThemedElement<'static, Message> {
    let style = if selected == Some(recipient) {
        builtins::button::primary
    } else {
        builtins::button::secondary
    };
    button(text(label).size(13))
        .style(style)
        .width(Length::Fill)
        .on_press(transfer(TransferMessage::RecipientSelected(recipient)))
        .into()
}

pub(super) fn clan_transfer_folder(
    destinations: &super::clan_maps::TransferDestinations,
    clan: Uuid,
    on_pick: impl Fn(super::clan_maps::FolderChoice) -> Message + 'static,
) -> ThemedElement<'static, Message> {
    row![
        text(crate::i18n::t!("clan-share-into-folder")).size(12),
        pick_list(
            destinations.folders.get(&clan).cloned().unwrap_or_default(),
            destinations.folder.clone(),
            on_pick
        )
        .placeholder(crate::i18n::t!("clan-maps-folder-placeholder"))
        .text_size(13),
    ]
    .spacing(8)
    .align_y(Vertical::Center)
    .into()
}

fn transfer_offer_view(dialog: &TransferDialog) -> ThemedElement<'_, Message> {
    if dialog.sent {
        let sent = match dialog.chosen_clan() {
            Some(clan) => crate::i18n::t!(
                "mapper-transfer-offer-sent-clan",
                "subject" => dialog.subject.name(),
                "clan" => clan
            ),
            None => crate::i18n::t!(
                "mapper-transfer-offer-sent",
                "subject" => dialog.subject.name()
            ),
        };
        return column![text(sent).size(13)].spacing(10).into();
    }

    let is_map = matches!(dialog.subject, TransferSubject::Area(..));
    let to_clan = dialog.chosen_clan().is_some();
    let give = if dialog.destinations.clans.is_empty() {
        crate::i18n::t!("mapper-transfer-give", "subject" => dialog.subject.name())
    } else {
        crate::i18n::t!("mapper-transfer-give-or-clan", "subject" => dialog.subject.name())
    };
    let warning = if to_clan {
        crate::i18n::t!("mapper-transfer-warning-clan")
    } else {
        crate::i18n::t!("mapper-transfer-warning")
    };
    let mut body = column![
        text(give).size(13),
        text(warning).size(12).style(builtins::text::danger),
    ]
    .spacing(8);
    if to_clan {
        body = body.push(ownership_choice(dialog.ownership, is_map, |ownership| {
            transfer(TransferMessage::OwnershipPicked(ownership))
        }));
    }

    if is_map {
        if let Some(TransferRecipient::Clan(clan, _)) = dialog.selected {
            body = body.push(clan_transfer_folder(&dialog.destinations, clan, |folder| {
                transfer(TransferMessage::FolderPicked(folder))
            }));
        } else {
            body = body.push(
                text(crate::i18n::t!("mapper-transfer-leaves-folder"))
                    .size(11)
                    .style(muted),
            );
        }
    }

    body = body.push(section_label(crate::i18n::t!("mapper-transfer-to")));
    let filter = dialog.filter.trim().to_lowercase();
    let shown = |label: &str| filter.is_empty() || label.to_lowercase().contains(&filter);
    let friend_count = dialog
        .friends
        .as_ref()
        .and_then(|friends| friends.as_ref().ok())
        .map_or(0, Vec::len);
    if friend_count + dialog.destinations.clans.len() > 0 {
        body = body.push(
            text_input(
                crate::i18n::ts!("mapper-filter-placeholder"),
                &dialog.filter,
            )
            .size(13)
            .on_input(|value| transfer(TransferMessage::FilterChanged(value))),
        );
    }
    match &dialog.friends {
        None => {
            body = body.push(
                text(crate::i18n::t!("mapper-loading-friends"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(friends)) if friends.is_empty() && dialog.destinations.clans.is_empty() => {
            body = body.push(
                text(crate::i18n::t!("mapper-no-friends-transfer"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(friends)) => {
            let mut list = column![].spacing(4);
            for friend in friends {
                let label = friend_label(friend);
                if shown(&label) {
                    list = list.push(recipient_button(
                        label,
                        TransferRecipient::User(friend.user_id),
                        dialog.selected,
                    ));
                }
            }
            body = body.push(list);
        }
    }
    if !dialog.destinations.clans.is_empty() {
        body = body.push(section_label(crate::i18n::t!("mapper-transfer-clans")));
        let mut list = column![].spacing(4);
        for (id, name) in &dialog.destinations.clans {
            if shown(name) {
                list = list.push(recipient_button(
                    name.clone(),
                    TransferRecipient::Clan(*id, dialog.ownership),
                    dialog.selected,
                ));
            }
        }
        body = body.push(list);
    }

    if let Some(error) = &dialog.error {
        body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
    }

    let can_submit = dialog
        .selected
        .is_some_and(|to| dialog.destinations.ready(to, is_map))
        && !dialog.submitting;
    let footer = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-cancel")).size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
        button(
            text(if dialog.submitting {
                crate::i18n::t!("mapper-sending")
            } else if to_clan {
                crate::i18n::t!("clan-share-put-action")
            } else {
                crate::i18n::t!("mapper-send-offer")
            })
            .size(13)
        )
        .style(builtins::button::primary)
        .on_press_maybe(can_submit.then_some(transfer(TransferMessage::Submit))),
    ]
    .spacing(10)
    .align_y(Vertical::Center);

    scrolling_body(body, footer)
}

/// Opens the share-folder dialog pre-scoped to `atlas_id` and kicks off the
/// friends + existing-grants fetches.
pub(super) fn open_share_atlas_dialog(
    window: &mut MapEditorWindow,
    atlas_id: AtlasId,
) -> Update<Message, super::Event> {
    let atlas_name = window
        .atlases
        .iter()
        .find(|atlas| atlas.id == atlas_id)
        .map(|atlas| atlas.name.clone())
        .unwrap_or_else(|| crate::i18n::t!("mapper-this-folder"));

    // §4.2: snapshot the hosts of this folder's associated entries, pre-checked.
    let host_hints = disclose_hosts(&window.map_scopes.atlas_entries(&atlas_id));

    window.modal = Some(Modal::ShareAtlas(ShareAtlasDialog {
        atlas_id,
        atlas_name,
        friends: None,
        filter: String::new(),
        selected: HashSet::new(),
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        submitting: false,
        results: Vec::new(),
        close_pending: false,
        grants: None,
        revoking: None,
        revoke_busy: false,
        manage_error: None,
        host_hints,
    }));

    let friends_client = window.cloud.client.clone();
    Update::with_task(Task::batch([
        Task::perform(async move { friends_client.friends().await }, |result| {
            share_atlas(ShareAtlasMessage::FriendsLoaded(result))
        }),
        fetch_atlas_grants(window),
    ]))
}

/// Fetches the caller's given grants and keeps only those scoped to the
/// dialog's atlas.
fn fetch_atlas_grants(window: &MapEditorWindow) -> Task<Message> {
    let client = window.cloud.client.clone();
    Task::perform(
        async move { client.shares(ShareDirection::Given).await },
        |result| share_atlas(ShareAtlasMessage::GrantsLoaded(result)),
    )
}

/// Routes a share-folder dialog message. No-op unless the share-folder dialog
/// is the open modal (stale async completions are dropped).
#[allow(clippy::too_many_lines)]
pub(super) fn update_share_atlas(
    window: &mut MapEditorWindow,
    message: ShareAtlasMessage,
) -> Update<Message, super::Event> {
    let Some(Modal::ShareAtlas(dialog)) = &mut window.modal else {
        return Update::none();
    };

    match message {
        ShareAtlasMessage::FriendsLoaded(result) => {
            dialog.friends = Some(result.map_err(|error| display_error(&error)));
            Update::none()
        }
        ShareAtlasMessage::GrantsLoaded(result) => {
            match result {
                Ok(rows) => {
                    let atlas_id = dialog.atlas_id;
                    let mine: Vec<ShareGrantRow> = rows
                        .into_iter()
                        .filter(|row| row.grant.atlas_id == Some(atlas_id))
                        .collect();
                    let ids: HashSet<Uuid> = mine.iter().map(|row| row.grant.id).collect();
                    if dialog.revoking.is_some_and(|id| !ids.contains(&id)) {
                        dialog.revoking = None;
                    }
                    dialog.grants = Some(Ok(mine));
                    dialog.manage_error = None;
                }
                Err(error) => {
                    let message = display_error(&error);
                    if dialog.grants.is_none() {
                        dialog.grants = Some(Err(message));
                    } else {
                        dialog.manage_error = Some(message);
                    }
                }
            }
            Update::none()
        }
        ShareAtlasMessage::FilterChanged(value) => {
            dialog.filter = value;
            Update::none()
        }
        ShareAtlasMessage::RecipientToggled(user_id, selected) => {
            if selected {
                dialog.selected.insert(user_id);
            } else {
                dialog.selected.remove(&user_id);
            }
            Update::none()
        }
        ShareAtlasMessage::FlagToggled(flag, value) => {
            match flag {
                GrantFlag::Edit => dialog.can_edit = value,
                GrantFlag::Reshare => dialog.can_reshare = value,
                GrantFlag::Copy => dialog.can_copy = value,
                // Owner-minted full-deputy over the whole folder.
                GrantFlag::Admin => dialog.can_admin = value,
            }
            Update::none()
        }
        ShareAtlasMessage::HostHintToggled(host, value) => {
            if let Some((_, checked)) = dialog.host_hints.iter_mut().find(|(h, _)| *h == host) {
                *checked = value;
            }
            Update::none()
        }
        ShareAtlasMessage::Submit => {
            if dialog.submitting || dialog.selected.is_empty() {
                return Update::none();
            }
            let Some(Ok(friends)) = &dialog.friends else {
                return Update::none();
            };
            let scope = ShareScope::Atlas {
                atlas_id: dialog.atlas_id,
            };
            // §4.2: the grantor's checked host disclosures ride on every grant.
            let host_hints = checked_host_hints(&dialog.host_hints);
            let requests: Vec<(String, CreateShareRequest)> = friends
                .iter()
                .filter(|friend| dialog.selected.contains(&friend.user_id))
                .map(|friend| {
                    (
                        friend_label(friend),
                        CreateShareRequest {
                            grantee_id: friend.user_id,
                            scope,
                            can_edit: dialog.can_edit,
                            can_reshare: dialog.can_reshare,
                            can_copy: dialog.can_copy,
                            can_admin: dialog.can_admin,
                            host_hints: host_hints.clone(),
                        },
                    )
                })
                .collect();
            if requests.is_empty() {
                return Update::none();
            }
            dialog.submitting = true;
            dialog.results = Vec::new();
            dialog.close_pending = false;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move {
                    let mut results = Vec::with_capacity(requests.len());
                    for (label, request) in requests {
                        let result = client.create_share(request).await.map(|_| ());
                        results.push((label, result));
                    }
                    results
                },
                |results| share_atlas(ShareAtlasMessage::Submitted(results)),
            ))
        }
        ShareAtlasMessage::Submitted(results) => {
            dialog.submitting = false;
            let all_ok = !results.is_empty() && results.iter().all(|(_, result)| result.is_ok());
            dialog.results = results;
            let mut tasks = Vec::new();
            if all_ok {
                dialog.close_pending = true;
                tasks.push(Task::perform(
                    async { tokio::time::sleep(Duration::from_millis(1400)).await },
                    |()| share_atlas(ShareAtlasMessage::CloseTick),
                ));
            }
            // Refresh the access list either way; partial successes changed it.
            tasks.push(fetch_atlas_grants(window));
            Update::with_task(Task::batch(tasks))
        }
        ShareAtlasMessage::CloseTick => {
            if dialog.close_pending {
                window.modal = None;
            }
            Update::none()
        }
        ShareAtlasMessage::RevokeRequested(id) => {
            dialog.revoking = Some(id);
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareAtlasMessage::RevokeCancelled => {
            dialog.revoking = None;
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareAtlasMessage::RevokeConfirmed => {
            let Some(id) = dialog.revoking else {
                return Update::none();
            };
            if dialog.revoke_busy {
                return Update::none();
            }
            dialog.revoke_busy = true;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move { client.revoke_share(id).await },
                |result| share_atlas(ShareAtlasMessage::RevokeResult(result)),
            ))
        }
        ShareAtlasMessage::RevokeResult(result) => {
            dialog.revoke_busy = false;
            dialog.revoking = None;
            match result {
                Ok(()) => fetch_atlas_grants_update(window),
                Err(error) => {
                    if let Some(Modal::ShareAtlas(dialog)) = &mut window.modal {
                        dialog.manage_error = Some(match error {
                            CloudError::NotFoundOrNoAccess => {
                                crate::i18n::t!("mapper-could-not-revoke")
                            }
                            other => display_error(&other),
                        });
                    }
                    Update::none()
                }
            }
        }
    }
}

/// Helper: refetch the atlas grants as an `Update` (used after a successful
/// revoke, where `window.modal` is reborrowed).
fn fetch_atlas_grants_update(window: &MapEditorWindow) -> Update<Message, super::Event> {
    Update::with_task(fetch_atlas_grants(window))
}

/// Whether a viewer with `access` may share the map itself.
pub(super) fn shares_map(access: &AreaAccess) -> bool {
    access.is_owner || access.can_reshare
}

/// Whether a viewer may share a Secret they hold `held` on: the map's
/// owner every owner Secret, anyone else those they manage access to. A
/// Clan Secret (`clan` set) is its clan's, whoever owns the map.
fn shares_secret(is_owner: bool, clan: Option<Uuid>, held: &BTreeSet<String>) -> bool {
    (is_owner && clan.is_none()) || held.contains(secret_action::MANAGE_ACCESS)
}

/// Whether the Share dialog has anything to offer on `area`: the map, or a
/// Secret the viewer may share.
pub(super) fn may_share(area: &AreaCache) -> bool {
    let access = area.effective_access();
    // A clan's map always has its Share dialog: who reaches it, and what the
    // viewer can do.
    area.meta().clan_id.is_some()
        || shares_map(&access)
        || area.meta().sources.iter().any(|bundle| {
            bundle.source.is_secret()
                && shares_secret(access.is_owner, bundle.clan_id, &bundle.actions)
        })
}

/// The picker's places, in order: `map` when the viewer may share it, then
/// each of `secrets` (in color order) they may share.
fn share_places(
    map: SharePlace,
    shares_map: bool,
    is_owner: bool,
    secrets: impl IntoIterator<Item = SharePlace>,
) -> Vec<SharePlace> {
    shares_map
        .then_some(map)
        .into_iter()
        .chain(
            secrets
                .into_iter()
                .filter(|place| shares_secret(is_owner, place.clan_id, &place.held)),
        )
        .collect()
}

/// The place the dialog opens on: the "Add to" place when it is a Secret
/// the viewer may share, else the map, else the first Secret.
fn opening_place(places: &[SharePlace], add_to: SourceId) -> Option<SourceId> {
    places
        .iter()
        .find(|place| place.source.is_secret() && place.source == add_to)
        .or_else(|| places.iter().find(|place| place.source.is_map()))
        .or_else(|| places.first())
        .map(|place| place.source)
}

/// Builds the dialog for the active map and kicks off the friends, grant
/// tree and Secret grant fetches. No-op when there is nothing the viewer
/// may share.
pub(super) fn open_share_dialog(window: &mut MapEditorWindow) -> Update<Message, super::Event> {
    let add_to = window.add_to();
    open_share_dialog_on(window, add_to)
}

/// Builds the dialog opening on `place` when the viewer may share it.
pub(super) fn open_share_dialog_on(
    window: &mut MapEditorWindow,
    place: SourceId,
) -> Update<Message, super::Event> {
    let Some(area_id) = window.editor.area_id() else {
        return Update::none();
    };
    let atlas = window.mapper.get_current_atlas();
    let Some(area) = atlas.get_area(&area_id) else {
        return Update::none();
    };
    let access = area.effective_access();
    let is_owner = access.is_owner;
    let map = SharePlace {
        source: SourceId::Map,
        name: area.get_name().to_string(),
        color: None,
        held: BTreeSet::new(),
        clan_id: None,
    };
    // A Clan Secret is shared within its clan, not with friends.
    let secrets = super::secrets::secrets(&area)
        .into_iter()
        .map(|bundle| SharePlace {
            source: bundle.source,
            name: bundle.name.clone().unwrap_or_default(),
            color: sources::source_color(&area, bundle.source),
            held: bundle.actions.clone(),
            clan_id: bundle.clan_id,
        })
        .collect::<Vec<_>>();
    let places = share_places(map, shares_map(&access), is_owner, secrets);
    let Some(target) = opening_place(&places, place) else {
        return Update::none();
    };
    let target_clan = places
        .iter()
        .find(|place| place.source == target)
        .and_then(|place| place.clan_id);

    // §4.2: snapshot the hosts of the shared thing's associated entries. For an
    // atlas-filed area that's its atlas's entries (atlas-level association is
    // the norm); for a genuinely atlas-less area, the area's own entries.
    let host_hints = disclose_hosts(&match area.meta().atlas_id {
        Some(atlas_id) => window.map_scopes.atlas_entries(&atlas_id),
        None => window.map_scopes.area_entries(&area_id),
    });
    window.modal = Some(Modal::Share(Box::new(ShareDialog {
        area_id,
        area_name: area.get_name().to_string(),
        is_owner,
        shares_map: shares_map(&access),
        owner_id: area.meta().owner_id,
        owner_nickname: area.meta().owner_nickname.clone(),
        viewer_id: window
            .cloud
            .snapshot
            .get()
            .profile
            .as_ref()
            .map(|profile| profile.id),
        places,
        target,
        friends: None,
        filter: String::new(),
        selected: HashSet::new(),
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        secret_flags: SecretFlags::default(),
        tree: None,
        secret_grants: HashMap::new(),
        editing: None,
        secret_editing: None,
        revoking: None,
        revoke_busy: false,
        submitting: false,
        results: Vec::new(),
        manage_error: None,
        host_hints,
        put_in_clan: super::clan_map_share::may_put_in_clan(window, area_id),
        clan_data: HashMap::new(),
    })));

    let friends_client = window.cloud.client.clone();
    let mut tasks = vec![Task::perform(
        async move { friends_client.friends().await },
        |result| share(ShareMessage::FriendsLoaded(result)),
    )];
    if shares_map(&access) {
        tasks.push(fetch_tree(window.cloud.client.clone(), area_id));
    }
    if target.is_secret() {
        tasks.push(fetch_secret_grants(
            window.mapper.clone(),
            window.cloud.client.clone(),
            area_id,
            target,
            target_clan,
        ));
    }
    Update::with_task(Task::batch(tasks))
}

fn fetch_tree(client: CloudApiClient, area_id: AreaId) -> Task<Message> {
    Task::perform(async move { client.area_shares(area_id).await }, |result| {
        share(ShareMessage::TreeLoaded(result))
    })
}

/// Loads a Secret's grants; a Clan Secret's (`clan` set) come with its
/// clan's recipients and who reads it.
fn fetch_secret_grants(
    mapper: Mapper,
    client: CloudApiClient,
    area_id: AreaId,
    secret: SourceId,
    clan: Option<Uuid>,
) -> Task<Message> {
    if let Some(clan_id) = clan {
        return super::clan_secret_share::fetch(client, clan_id, secret);
    }
    Task::perform(
        async move { mapper.secret_grants(area_id, &secret).await },
        move |result| share(ShareMessage::SecretGrantsLoaded { secret, result }),
    )
}

impl ShareDialog {
    /// The picked place, while it is still on offer.
    fn place(&self) -> Option<&SharePlace> {
        self.places.iter().find(|place| place.source == self.target)
    }

    /// The viewer as a sharer of the picked Secret.
    fn sharer(&self) -> Option<SecretSharer<'_>> {
        self.place()
            .filter(|place| place.source.is_secret())
            .map(|place| SecretSharer {
                // On a Clan Secret, ownership authority stands in the map
                // owner's place.
                is_owner: if place.clan_id.is_some() {
                    place
                        .held
                        .contains(smudgy_cloud::clan_secrets::authority_action::MANAGE_OWNERSHIP)
                } else {
                    self.is_owner
                },
                held: &place.held,
            })
    }

    /// The clan of `source` when it is a Clan Secret on offer.
    fn clan_of(&self, source: SourceId) -> Option<Uuid> {
        self.places
            .iter()
            .find(|place| place.source == source)
            .and_then(|place| place.clan_id)
    }

    /// The clan recipients a press of Share would share the picked Clan
    /// Secret with: checked, in its clan, and without a grant yet (a second
    /// one would replace it).
    fn clan_recipients(&self) -> Vec<(String, smudgy_cloud::clan_secrets::SecretRecipient)> {
        if self.clan_of(self.target).is_none() {
            return Vec::new();
        }
        let Some(Ok(data)) = self.clan_data.get(&self.target) else {
            return Vec::new();
        };
        data.groups()
            .into_iter()
            .chain(data.members(self.viewer_id))
            .map(|(id, _)| id)
            .filter(|id| self.selected.contains(id) && !self.has_access(*id))
            .filter_map(|id| data.recipient(id, self.viewer_id))
            .collect()
    }

    /// The picked Secret's grants, once loaded.
    fn target_grants(&self) -> Option<&[SecretGrant]> {
        match self.secret_grants.get(&self.target) {
            Some(Ok(grants)) => Some(grants),
            _ => None,
        }
    }

    /// Whether `friend` already holds the viewer's own grant on the picked
    /// place: sharing again would replace it, so changes go through Who has
    /// access instead.
    fn has_access(&self, friend: Uuid) -> bool {
        let Some(viewer) = self.viewer_id else {
            return false;
        };
        if self.target.is_map() {
            return matches!(&self.tree, Some(Ok(nodes)) if nodes.iter().any(|node| {
                node.grant.grantor_id == viewer
                    && node.grant.grantee_id == friend
                    && node.grant.area_id == Some(self.area_id)
            }));
        }
        // A Clan Secret has one grant per recipient, whoever made it.
        let clan = self.clan_of(self.target).is_some();
        self.target_grants().is_some_and(|grants| {
            grants
                .iter()
                .any(|grant| (clan || grant.grantor_id == viewer) && grant.grantee_id == friend)
        })
    }

    /// Whether the map's grant tree shows `friend` reading the map. Only
    /// meaningful once the tree has loaded.
    fn sees_map(&self, friend: Uuid) -> bool {
        matches!(&self.tree, Some(Ok(nodes)) if nodes.iter().any(|node| node.grant.grantee_id == friend))
    }

    /// The friends a press of Share would share with: checked, still friends,
    /// not the map's owner, and without the viewer's grant on the place.
    fn recipients(&self) -> Vec<&FriendView> {
        let Some(Ok(friends)) = &self.friends else {
            return Vec::new();
        };
        friends
            .iter()
            .filter(|friend| {
                self.selected.contains(&friend.user_id)
                    && Some(friend.user_id) != self.owner_id
                    && !self.has_access(friend.user_id)
            })
            .collect()
    }

    /// Moves the picker to `source`, keeping checked friends and dropping
    /// the boxes the viewer may not give there.
    fn pick(&mut self, source: SourceId) {
        if !self.places.iter().any(|place| place.source == source) {
            return;
        }
        self.target = source;
        self.editing = None;
        self.secret_editing = None;
        self.revoking = None;
        self.revoke_busy = false;
        self.manage_error = None;
        self.results.clear();
        if let Some(sharer) = self.sharer() {
            self.secret_flags = self.secret_flags.clamped(&sharer);
        }
    }

    /// A grantor's name as Who has access attributes it, when known.
    fn grantor_name(&self, grant: &SecretGrant) -> Option<String> {
        grant
            .grantor_nickname
            .clone()
            .or_else(|| {
                self.target_grants()?
                    .iter()
                    .find(|other| other.grantee_id == grant.grantor_id)?
                    .grantee_nickname
                    .clone()
            })
            .or_else(|| match &self.friends {
                Some(Ok(friends)) => friends
                    .iter()
                    .find(|friend| friend.user_id == grant.grantor_id)?
                    .nickname
                    .clone(),
                _ => None,
            })
            .or_else(|| {
                (Some(grant.grantor_id) == self.owner_id)
                    .then(|| self.owner_nickname.clone())
                    .flatten()
            })
    }
}

fn friend_label(friend: &FriendView) -> String {
    friend
        .nickname
        .clone()
        .unwrap_or_else(|| friend.user_id.to_string())
}

/// §4.2 consent snapshot: the hosts of the server entries a shared thing is
/// associated with, each **pre-checked and removable**. Resolves the entry
/// names to their configured hosts (lowercased, trimmed, port dropped — the
/// plan's §5 matcher treats a bare host as port-agnostic, which survives port
/// drift), deduped case-insensitively across entries. An unassigned thing (no
/// entries) yields an empty list — nothing to disclose.
fn disclose_hosts(entries: &BTreeSet<String>) -> Vec<(String, bool)> {
    if entries.is_empty() {
        return Vec::new();
    }
    let servers = smudgy_core::models::server::list_servers().unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut hosts = Vec::new();
    for name in entries {
        let Some(server) = servers.iter().find(|server| &server.name == name) else {
            continue;
        };
        let host = server.config.host.trim().to_ascii_lowercase();
        if host.is_empty() {
            continue;
        }
        if seen.insert(host.clone()) {
            hosts.push((host, true));
        }
    }
    hosts
}

/// The checked hosts as a `CreateShareRequest.host_hints` payload: `None` when
/// the list is empty or nothing is checked (skip-when-none keeps the wire
/// clean and old-server compatible).
fn checked_host_hints(host_hints: &[(String, bool)]) -> Option<Vec<String>> {
    let checked: Vec<String> = host_hints
        .iter()
        .filter(|(_, checked)| *checked)
        .map(|(host, _)| host.clone())
        .collect();
    (!checked.is_empty()).then_some(checked)
}

/// Routes a share-dialog message. Everything here is a no-op unless the
/// share dialog is the open modal (stale async completions are dropped).
#[allow(clippy::too_many_lines)]
pub(super) fn update_share(
    window: &mut MapEditorWindow,
    message: ShareMessage,
) -> Update<Message, super::Event> {
    if let ShareMessage::PutInClan = message {
        let Some(Modal::Share(dialog)) = &window.modal else {
            return Update::none();
        };
        let area_id = dialog.area_id;
        return super::clan_map_share::open_put_in_clan(window, area_id);
    }
    let Some(Modal::Share(dialog)) = &mut window.modal else {
        return Update::none();
    };

    match message {
        ShareMessage::PutInClan => Update::none(),
        ShareMessage::FriendsLoaded(result) => {
            dialog.friends = Some(result.map_err(|error| display_error(&error)));
            Update::none()
        }
        ShareMessage::TreeLoaded(result) => {
            match result {
                Ok(nodes) => {
                    // Drop UI state pointing at grants that no longer exist.
                    let ids: HashSet<Uuid> = nodes.iter().map(|node| node.grant.id).collect();
                    if dialog
                        .editing
                        .as_ref()
                        .is_some_and(|edit| !ids.contains(&edit.id))
                    {
                        dialog.editing = None;
                    }
                    if dialog.target.is_map() {
                        if dialog.revoking.is_some_and(|id| !ids.contains(&id)) {
                            dialog.revoking = None;
                        }
                        dialog.manage_error = None;
                    }
                    dialog.tree = Some(Ok(nodes));
                }
                Err(error) => {
                    let message = display_error(&error);
                    if dialog.tree.is_none() {
                        dialog.tree = Some(Err(message));
                    } else if dialog.target.is_map() {
                        dialog.manage_error = Some(message);
                    }
                }
            }
            Update::none()
        }
        ShareMessage::SecretGrantsLoaded { secret, result } => {
            match result {
                Ok(grants) => {
                    if secret == dialog.target {
                        let ids: HashSet<Uuid> = grants.iter().map(|grant| grant.id).collect();
                        if dialog
                            .secret_editing
                            .as_ref()
                            .is_some_and(|edit| !ids.contains(&edit.original.id))
                        {
                            dialog.secret_editing = None;
                        }
                        if dialog.revoking.is_some_and(|id| !ids.contains(&id)) {
                            dialog.revoking = None;
                        }
                        dialog.manage_error = None;
                    }
                    dialog.secret_grants.insert(secret, Ok(grants));
                }
                Err(error) => {
                    let message = display_error(&error);
                    match dialog.secret_grants.get(&secret) {
                        Some(Ok(_)) => {
                            if secret == dialog.target {
                                dialog.manage_error = Some(message);
                            }
                        }
                        _ => {
                            dialog.secret_grants.insert(secret, Err(message));
                        }
                    }
                }
            }
            Update::none()
        }
        ShareMessage::ClanLoaded { secret, result } => {
            let result = match result {
                Ok(loaded) => {
                    let (grants, data) = *loaded;
                    dialog.clan_data.insert(secret, Ok(data));
                    Ok(grants)
                }
                Err(error) => {
                    if !matches!(dialog.clan_data.get(&secret), Some(Ok(_))) {
                        dialog.clan_data.insert(secret, Err(display_error(&error)));
                    }
                    Err(error)
                }
            };
            update_share(window, ShareMessage::SecretGrantsLoaded { secret, result })
        }
        ShareMessage::PlacePicked(place) => {
            if place.source == dialog.target {
                return Update::none();
            }
            dialog.pick(place.source);
            if dialog.target == place.source && place.source.is_secret() {
                let area_id = dialog.area_id;
                return Update::with_task(fetch_secret_grants(
                    window.mapper.clone(),
                    window.cloud.client.clone(),
                    area_id,
                    place.source,
                    place.clan_id,
                ));
            }
            Update::none()
        }
        ShareMessage::FilterChanged(value) => {
            dialog.filter = value;
            Update::none()
        }
        ShareMessage::RecipientToggled(user_id, selected) => {
            if selected {
                dialog.selected.insert(user_id);
            } else {
                dialog.selected.remove(&user_id);
            }
            Update::none()
        }
        ShareMessage::FlagToggled(flag, value) => {
            match flag {
                GrantFlag::Edit => dialog.can_edit = value,
                GrantFlag::Reshare => dialog.can_reshare = value,
                GrantFlag::Copy => dialog.can_copy = value,
                // can_admin is owner-minted only.
                GrantFlag::Admin => {
                    if dialog.is_owner {
                        dialog.can_admin = value;
                    }
                }
            }
            Update::none()
        }
        ShareMessage::SecretFlagToggled(flag, value) => {
            if dialog.sharer().is_some_and(|sharer| sharer.may_give(flag)) {
                dialog.secret_flags.set(flag, value);
            }
            Update::none()
        }
        ShareMessage::HostHintToggled(host, value) => {
            if let Some((_, checked)) = dialog.host_hints.iter_mut().find(|(h, _)| *h == host) {
                *checked = value;
            }
            Update::none()
        }
        ShareMessage::Submit => {
            if dialog.submitting {
                return Update::none();
            }
            if dialog.clan_of(dialog.target).is_some() {
                let recipients = dialog.clan_recipients();
                let Some(actions) = dialog
                    .sharer()
                    .map(|sharer| dialog.secret_flags.clamped(&sharer).actions())
                else {
                    return Update::none();
                };
                if recipients.is_empty() {
                    return Update::none();
                }
                let target = dialog.target;
                let client = window.cloud.client.clone();
                dialog.results.clear();
                dialog.submitting = true;
                return Update::with_task(Task::perform(
                    async move {
                        let mut results = Vec::with_capacity(recipients.len());
                        for (label, recipient) in recipients {
                            let outcome = match client
                                .grant_clan_secret(&target, recipient, &actions)
                                .await
                            {
                                Ok(_) => ShareOutcome::Shared,
                                Err(error) => ShareOutcome::Failed(error),
                            };
                            results.push((label, outcome));
                        }
                        results
                    },
                    move |results| share(ShareMessage::Submitted { target, results }),
                ));
            }
            let recipients: Vec<(String, Uuid)> = dialog
                .recipients()
                .into_iter()
                .map(|friend| (friend_label(friend), friend.user_id))
                .collect();
            if recipients.is_empty() {
                return Update::none();
            }
            let target = dialog.target;
            let area_id = dialog.area_id;
            let client = window.cloud.client.clone();
            dialog.results.clear();
            let mut tasks = Vec::new();
            let task = if let Some(sharer) = dialog.sharer() {
                let actions = dialog.secret_flags.clamped(&sharer).actions();
                // A Secret shows only to people who see its map: when the
                // viewer may share the map, a friend who can't see it gets
                // map view too. Unknown while the tree is loading.
                let needs_map: HashSet<Uuid> =
                    if dialog.shares_map && matches!(dialog.tree, Some(Ok(_))) {
                        recipients
                            .iter()
                            .map(|(_, friend)| *friend)
                            .filter(|friend| !dialog.sees_map(*friend))
                            .collect()
                    } else {
                        HashSet::new()
                    };
                let mapper = window.mapper.clone();
                Task::perform(
                    async move {
                        let mut results = Vec::with_capacity(recipients.len());
                        for (label, friend) in recipients {
                            let outcome = match mapper
                                .grant_secret(area_id, &target, friend, &actions)
                                .await
                            {
                                Err(error) => ShareOutcome::Failed(error),
                                Ok(_) if !needs_map.contains(&friend) => ShareOutcome::Shared,
                                Ok(_) => {
                                    match client.create_share(map_view_share(friend, area_id)).await
                                    {
                                        Ok(_) => ShareOutcome::SharedWithMap,
                                        Err(error) => ShareOutcome::Failed(error),
                                    }
                                }
                            };
                            results.push((label, outcome));
                        }
                        results
                    },
                    move |results| share(ShareMessage::Submitted { target, results }),
                )
            } else {
                // can_admin is owner-minted only.
                let can_admin = dialog.can_admin && dialog.is_owner;
                let (can_edit, can_reshare, can_copy) =
                    (dialog.can_edit, dialog.can_reshare, dialog.can_copy);
                // §4.2: the grantor's checked host disclosures ride on every grant.
                let host_hints = checked_host_hints(&dialog.host_hints);
                Task::perform(
                    async move {
                        let mut results = Vec::with_capacity(recipients.len());
                        for (label, friend) in recipients {
                            let request = CreateShareRequest {
                                grantee_id: friend,
                                scope: ShareScope::Area { area_id },
                                can_edit,
                                can_reshare,
                                can_copy,
                                can_admin,
                                host_hints: host_hints.clone(),
                            };
                            let outcome = match client.create_share(request).await {
                                Ok(_) => ShareOutcome::Shared,
                                Err(error) => ShareOutcome::Failed(error),
                            };
                            results.push((label, outcome));
                        }
                        results
                    },
                    move |results| share(ShareMessage::Submitted { target, results }),
                )
            };
            dialog.submitting = true;
            tasks.push(task);
            Update::with_task(Task::batch(tasks))
        }
        ShareMessage::Submitted { target, results } => {
            dialog.submitting = false;
            let map_changed = target.is_map()
                || results
                    .iter()
                    .any(|(_, outcome)| matches!(outcome, ShareOutcome::SharedWithMap));
            // Clan offers report beside these, in either order.
            if target == dialog.target {
                dialog.results.extend(results);
            }
            // Refresh either way; partial successes changed who has access.
            let area_id = dialog.area_id;
            let mut tasks = Vec::new();
            if map_changed && dialog.shares_map {
                tasks.push(fetch_tree(window.cloud.client.clone(), area_id));
            }
            if target.is_secret() {
                tasks.push(fetch_secret_grants(
                    window.mapper.clone(),
                    window.cloud.client.clone(),
                    area_id,
                    target,
                    dialog.clan_of(target),
                ));
            }
            Update::with_task(Task::batch(tasks))
        }
        ShareMessage::EditGrant(id) => {
            dialog.revoking = None;
            if let Some(sharer) = dialog.sharer() {
                let grant = dialog
                    .target_grants()
                    .and_then(|grants| grants.iter().find(|grant| grant.id == id))
                    .filter(|grant| sharer.manages(grant))
                    .cloned();
                if let Some(grant) = grant {
                    dialog.secret_editing = Some(SecretGrantEdit {
                        flags: SecretFlags::of(&grant.actions),
                        original: grant,
                        saving: false,
                        error: None,
                    });
                }
                return Update::none();
            }
            let Some(Ok(nodes)) = &dialog.tree else {
                return Update::none();
            };
            let Some(node) = nodes.iter().find(|node| node.grant.id == id) else {
                return Update::none();
            };
            dialog.editing = Some(GrantEdit {
                id,
                original: node.grant.clone(),
                can_edit: node.grant.can_edit,
                can_reshare: node.grant.can_reshare,
                can_copy: node.grant.can_copy,
                can_admin: node.grant.can_admin,
                // can_admin: the true owner only, on an owner-minted root.
                allow_admin: dialog.is_owner
                    && node.grant.parent_grant_id.is_none()
                    && node.grant.grantor_id == node.grant.owner_id,
                saving: false,
                error: None,
            });
            Update::none()
        }
        ShareMessage::EditFlagToggled(flag, value) => {
            if let Some(edit) = &mut dialog.editing {
                match flag {
                    GrantFlag::Edit => edit.can_edit = value,
                    GrantFlag::Reshare => edit.can_reshare = value,
                    GrantFlag::Copy => edit.can_copy = value,
                    GrantFlag::Admin => {
                        if edit.allow_admin {
                            edit.can_admin = value;
                        }
                    }
                }
            }
            Update::none()
        }
        ShareMessage::EditSecretFlagToggled(flag, value) => {
            let allowed = dialog
                .sharer()
                .zip(dialog.secret_editing.as_ref())
                .is_some_and(|(sharer, edit)| sharer.may_toggle(edit, flag));
            if allowed && let Some(edit) = &mut dialog.secret_editing {
                edit.flags.set(flag, value);
            }
            Update::none()
        }
        ShareMessage::EditCancelled => {
            dialog.editing = None;
            dialog.secret_editing = None;
            Update::none()
        }
        ShareMessage::EditSaved => {
            let editing_clan = dialog
                .secret_editing
                .as_ref()
                .and_then(|edit| dialog.clan_of(edit.original.secret_id));
            if let Some(edit) = &mut dialog.secret_editing {
                if edit.saving {
                    return Update::none();
                }
                if edit.flags == SecretFlags::of(&edit.original.actions) {
                    dialog.secret_editing = None;
                    return Update::none();
                }
                let actions = edit.flags.edited_actions(&edit.original.actions);
                edit.saving = true;
                edit.error = None;
                let id = edit.original.id;
                let secret = edit.original.secret_id;
                if editing_clan.is_some() {
                    let client = window.cloud.client.clone();
                    let original = edit.original.clone();
                    return Update::with_task(Task::perform(
                        async move {
                            client
                                .update_clan_secret_grant(&secret, id, &actions)
                                .await
                                .map(|grant| SecretGrant {
                                    actions: grant.actions,
                                    updated_at: grant.updated_at,
                                    ..original
                                })
                        },
                        move |result| share(ShareMessage::SecretEditResult { id, result }),
                    ));
                }
                let area_id = dialog.area_id;
                let mapper = window.mapper.clone();
                return Update::with_task(Task::perform(
                    async move {
                        mapper
                            .update_secret_grant(area_id, &secret, id, &actions)
                            .await
                    },
                    move |result| share(ShareMessage::SecretEditResult { id, result }),
                ));
            }
            let Some(edit) = &mut dialog.editing else {
                return Update::none();
            };
            if edit.saving {
                return Update::none();
            }
            let patch = SharePatch {
                can_edit: (edit.can_edit != edit.original.can_edit).then_some(edit.can_edit),
                can_reshare: (edit.can_reshare != edit.original.can_reshare)
                    .then_some(edit.can_reshare),
                can_copy: (edit.can_copy != edit.original.can_copy).then_some(edit.can_copy),
                can_admin: (edit.can_admin != edit.original.can_admin).then_some(edit.can_admin),
            };
            if patch == SharePatch::default() {
                dialog.editing = None;
                return Update::none();
            }
            edit.saving = true;
            edit.error = None;
            let id = edit.id;
            let client = window.cloud.client.clone();
            Update::with_task(Task::perform(
                async move { client.update_share(id, patch).await },
                move |result| share(ShareMessage::EditResult { id, result }),
            ))
        }
        ShareMessage::EditResult { id, result } => {
            let area_id = dialog.area_id;
            match result {
                Ok(_) => {
                    if dialog.editing.as_ref().is_some_and(|edit| edit.id == id) {
                        dialog.editing = None;
                    }
                    // Lowering flags may have clamped or deleted descendant
                    // grants server-side — refetch the whole tree.
                    Update::with_task(fetch_tree(window.cloud.client.clone(), area_id))
                }
                Err(error) => {
                    if let Some(edit) = &mut dialog.editing
                        && edit.id == id
                    {
                        edit.saving = false;
                        edit.error = Some(match error {
                            CloudError::NotFoundOrNoAccess => {
                                crate::i18n::t!("mapper-could-not-update-grant")
                            }
                            other => display_error(&other),
                        });
                    }
                    Update::none()
                }
            }
        }
        ShareMessage::SecretEditResult { id, result } => match result {
            Ok(grant) => {
                if dialog
                    .secret_editing
                    .as_ref()
                    .is_some_and(|edit| edit.original.id == id)
                {
                    dialog.secret_editing = None;
                }
                let area_id = dialog.area_id;
                Update::with_task(fetch_secret_grants(
                    window.mapper.clone(),
                    window.cloud.client.clone(),
                    area_id,
                    grant.secret_id,
                    dialog.clan_of(grant.secret_id),
                ))
            }
            Err(error) => {
                if let Some(edit) = &mut dialog.secret_editing
                    && edit.original.id == id
                {
                    edit.saving = false;
                    edit.error = Some(match error {
                        CloudError::NotFoundOrNoAccess => {
                            crate::i18n::t!("mapper-could-not-update-grant")
                        }
                        other => display_error(&other),
                    });
                }
                Update::none()
            }
        },
        ShareMessage::RevokeRequested(id) => {
            dialog.editing = None;
            dialog.secret_editing = None;
            dialog.revoking = Some(id);
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareMessage::RevokeCancelled => {
            dialog.revoking = None;
            dialog.revoke_busy = false;
            Update::none()
        }
        ShareMessage::RevokeConfirmed => {
            let Some(id) = dialog.revoking else {
                return Update::none();
            };
            if dialog.revoke_busy {
                return Update::none();
            }
            dialog.revoke_busy = true;
            let target = dialog.target;
            let area_id = dialog.area_id;
            let task = if target.is_secret() {
                let mapper = window.mapper.clone();
                Task::perform(
                    async move { mapper.revoke_secret_grant(area_id, &target, id).await },
                    move |result| share(ShareMessage::RevokeResult { target, result }),
                )
            } else {
                let client = window.cloud.client.clone();
                Task::perform(
                    async move { client.revoke_share(id).await },
                    move |result| share(ShareMessage::RevokeResult { target, result }),
                )
            };
            Update::with_task(task)
        }
        ShareMessage::RevokeResult { target, result } => {
            dialog.revoke_busy = false;
            dialog.revoking = None;
            let area_id = dialog.area_id;
            match result {
                Ok(()) if target.is_secret() => Update::with_task(fetch_secret_grants(
                    window.mapper.clone(),
                    window.cloud.client.clone(),
                    area_id,
                    target,
                    dialog.clan_of(target),
                )),
                Ok(()) => Update::with_task(fetch_tree(window.cloud.client.clone(), area_id)),
                Err(error) => {
                    dialog.manage_error = Some(match error {
                        CloudError::NotFoundOrNoAccess => {
                            crate::i18n::t!("mapper-could-not-revoke")
                        }
                        other => display_error(&other),
                    });
                    Update::none()
                }
            }
        }
    }
}

/// The view-only map share that lets a friend see the Secret they were
/// given. It carries no server names: the dialog showed none for a Secret.
fn map_view_share(grantee_id: Uuid, area_id: AreaId) -> CreateShareRequest {
    CreateShareRequest {
        grantee_id,
        scope: ShareScope::Area { area_id },
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        host_hints: None,
    }
}

fn muted(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

impl Modal {
    #[allow(clippy::too_many_lines)]
    pub fn view(&self) -> ThemedElement<'_, Message> {
        let (title, body): (String, ThemedElement<'_, Message>) = match self {
            Modal::CreateArea {
                name,
                error,
                folder,
                busy,
                ownership,
            } => {
                let mut body = column![
                    text(crate::i18n::t!("mapper-name-new-area")).size(13),
                    text_input(crate::i18n::ts!("mapper-area-name-placeholder"), name)
                        .size(14)
                        .on_input(Message::CreateAreaNameChanged)
                        .on_submit(Message::CreateAreaConfirmed),
                    folder.view(Message::CreateAreaConfirmed),
                ]
                .spacing(10);
                match ownership {
                    Some(choice) if choice.clan_allowed => {
                        body = body.push(new_map_ownership(choice.picked));
                    }
                    Some(_) => {
                        body = body.push(
                            text(crate::i18n::t!("clan-share-new-map-member-owned"))
                                .size(12)
                                .style(muted),
                        );
                    }
                    None => {}
                }

                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }

                let ready = !name.trim().is_empty() && folder.ready() && !busy;
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(text(crate::i18n::t!("action-create")).size(13))
                        .style(builtins::button::primary)
                        .on_press_maybe(ready.then_some(Message::CreateAreaConfirmed)),
                ]
                .spacing(10)
                .align_y(Vertical::Center);

                (
                    crate::i18n::t!("mapper-new-area"),
                    scrolling_body(body, footer),
                )
            }
            Modal::ReviewMove {
                destination,
                content,
                reviewed,
                source_names,
                ..
            } => {
                let mut body = column![].spacing(14);
                if !content.rooms.is_empty()
                    || !content.connections.is_empty()
                    || !content.labels.is_empty()
                    || !content.shapes.is_empty()
                {
                    body = body.push(
                        text(crate::i18n::t!("move-review-selection",
                            "rooms" => content.rooms.len(), "links" => content.connections.len(),
                            "labels" => content.labels.len(), "shapes" => content.shapes.len()
                        ))
                        .size(13),
                    );
                    body =
                        body.push(text(crate::i18n::t!("move-review-preserve-positions")).size(13));
                }
                if !content.properties.is_empty() {
                    body = body.push(text(crate::i18n::t!("move-review-properties", "count" => content.properties.len())).size(13));
                }

                if let Some(reviewed) = reviewed {
                    if reviewed.review.destination_notice {
                        body = body
                            .push(text(crate::i18n::t!("move-review-destination-notice")).size(13));
                    }
                    let mut details =
                        column![super::access_review::content(&reviewed.review)].spacing(12);
                    if !reviewed.review.preserves_undo {
                        details = details.push(
                            text(crate::i18n::t!("move-review-property-no-undo"))
                                .size(13)
                                .style(builtins::text::danger),
                        );
                    }
                    for conflict in &reviewed.review.property_conflicts {
                        use smudgy_cloud::mutation::{PropertyChoice, PropertyResolution};
                        let property = &conflict.property;
                        let label = property.room_number.map_or_else(
                            || property.name.clone(),
                            |number| {
                                let source = source_names
                                    .get(&property.room_source)
                                    .cloned()
                                    .unwrap_or_else(|| property.room_source.to_string());
                                format!("{source} · #{number} · {}", property.name)
                            },
                        );
                        let chosen = content
                            .property_resolutions
                            .iter()
                            .find(|choice| choice.property == *property)
                            .map(|choice| choice.keep);
                        let choice = |keep, key| {
                            button(text(crate::i18n::translate(key)).size(12))
                                .style(if chosen == Some(keep) {
                                    builtins::button::primary
                                } else {
                                    builtins::button::secondary
                                })
                                .on_press_maybe(
                                    (keep == PropertyChoice::Destination || conflict.can_replace)
                                        .then(|| {
                                            Message::Move(
                                                super::moves::MoveMessage::ResolveProperty(
                                                    PropertyResolution {
                                                        property: property.clone(),
                                                        keep,
                                                    },
                                                ),
                                            )
                                        }),
                                )
                        };
                        details = details.push(column![
                            text(label).size(14),
                            text(crate::i18n::t!("move-review-property-source", "value" => conflict.source_value.clone())).size(13),
                            text(crate::i18n::t!("move-review-property-destination", "value" => conflict.destination_value.clone())).size(13),
                            row![choice(PropertyChoice::Source, "move-review-property-keep-source"), choice(PropertyChoice::Destination, "move-review-property-keep-destination")].spacing(8),
                        ].spacing(6));
                    }
                    if !reviewed.review.changes.is_empty()
                        || !reviewed.review.preserves_undo
                        || !reviewed.review.property_conflicts.is_empty()
                    {
                        body = body.push(details);
                    }
                } else {
                    body = body.push(text(crate::i18n::t!("move-review-loading")).size(13));
                }
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(text(crate::i18n::t!("mapper-move-action")).size(13))
                        .style(builtins::button::primary)
                        .on_press_maybe(
                            reviewed
                                .as_ref()
                                .is_some_and(|review| review.properties_resolved())
                                .then_some(Message::Move(
                                    super::moves::MoveMessage::ReviewConfirmed
                                ))
                        ),
                ]
                .spacing(10);
                (
                    crate::i18n::t!("mapper-move-title", "place" => destination.clone()),
                    scrolling_body(body, footer),
                )
            }
            Modal::ReviewFiling(dialog) => {
                let mut entries = column![].spacing(12);
                for name in &dialog.names {
                    entries = entries.push(text(name).size(14));
                }
                if let Some(reviews) = &dialog.reviews {
                    for (name, reviewed) in reviews {
                        if !reviewed.review.changes.is_empty() {
                            entries = entries
                                .push(text(name).size(15))
                                .push(super::access_review::content(&reviewed.review));
                        }
                    }
                }
                if let super::filing::Request::DeleteSelection { dialog } = &dialog.request {
                    entries = entries.push(text(dialog.deletion_notice()).size(13));
                    for name in dialog.deletion_names() {
                        entries = entries.push(text(name).size(12).style(builtins::text::danger));
                    }
                }
                let mut body = column![
                    text(crate::i18n::t!("move-review-filing-notice")).size(13),
                    entries,
                ]
                .spacing(14);
                if dialog.reviews.is_none() && dialog.error.is_none() {
                    body = body.push(text(crate::i18n::t!("move-review-loading")).size(13));
                }
                if let Some(error) = &dialog.error {
                    body = body.push(text(error).size(13).style(builtins::text::danger));
                }
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(
                        text(
                            if matches!(
                                dialog.request,
                                super::filing::Request::DeleteSelection { .. }
                            ) {
                                crate::i18n::t!("action-delete")
                            } else {
                                crate::i18n::t!("mapper-move-action")
                            }
                        )
                        .size(13)
                    )
                    .style(builtins::button::primary)
                    .on_press_maybe(
                        (dialog.reviews.is_some() && dialog.error.is_none())
                            .then_some(Message::FilingConfirmed)
                    ),
                ]
                .spacing(10);
                (
                    crate::i18n::t!("mapper-move-to-folder"),
                    scrolling_body(body, footer),
                )
            }
            Modal::ConfirmDeleteArea {
                name, room_count, ..
            } => {
                let body = column![
                    text(crate::i18n::t!(
                        "mapper-delete-area-question",
                        "name" => name,
                        "rooms" => room_count
                    ))
                    .size(13),
                    text(crate::i18n::t!("mapper-cannot-undo"))
                        .size(12)
                        .style(builtins::text::danger),
                ]
                .spacing(10);
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(text(crate::i18n::t!("action-delete")).size(13))
                        .style(builtins::button::primary)
                        .on_press(Message::DeleteAreaConfirmed),
                ]
                .spacing(10)
                .align_y(Vertical::Center);

                (
                    crate::i18n::t!("mapper-delete-area"),
                    scrolling_body(body, footer),
                )
            }
            Modal::ConfirmCopySelection {
                boundary_count,
                include_boundary_links,
                cut_after_copy,
            } => {
                let boundary = if *boundary_count == 1 {
                    crate::i18n::t!(
                        "mapper-copy-boundary-one",
                        "count" => boundary_count.to_string()
                    )
                } else {
                    crate::i18n::t!(
                        "mapper-copy-boundary-many",
                        "count" => boundary_count.to_string()
                    )
                };
                let action = if *cut_after_copy {
                    crate::i18n::t!("action-cut")
                } else {
                    crate::i18n::t!("action-copy")
                };
                let body = column![
                    text(boundary).size(13),
                    text(crate::i18n::t!("mapper-copy-boundary-help")).size(12),
                    checkbox(*include_boundary_links)
                        .label(crate::i18n::t!("mapper-copy-include-boundary"))
                        .size(14)
                        .text_size(12)
                        .on_toggle(Message::CopyIncludeBoundaryChanged),
                ]
                .spacing(10);
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(text(action.clone()).size(13))
                        .style(builtins::button::primary)
                        .on_press(Message::CopySelectionConfirmed),
                ]
                .spacing(10)
                .align_y(Vertical::Center);
                (
                    crate::i18n::t!(
                        "mapper-copy-selection-title",
                        "action" => action
                    ),
                    scrolling_body(body, footer),
                )
            }
            Modal::CreateAtlas {
                name,
                error,
                storage,
                cloud_available,
            } => {
                let mut body = column![
                    text(crate::i18n::t!("mapper-name-new-folder")).size(13),
                    text_input(crate::i18n::ts!("mapper-folder-name-placeholder"), name)
                        .size(14)
                        .on_input(Message::CreateAtlasNameChanged)
                        .on_submit(Message::CreateAtlasConfirmed),
                ]
                .spacing(10);

                // Tier choice: only offered when signed in (cloud needs an
                // account). Signed out, the folder is local — say so.
                if *cloud_available {
                    body = body.push(
                        column![
                            section_label(crate::i18n::t!("mapper-save-in")),
                            radio(
                                crate::i18n::t!("mapper-save-cloud"),
                                MapStorage::Cloud,
                                Some(*storage),
                                Message::CreateAtlasTierChanged,
                            )
                            .size(14)
                            .text_size(13),
                            radio(
                                crate::i18n::t!("mapper-save-local"),
                                MapStorage::Local,
                                Some(*storage),
                                Message::CreateAtlasTierChanged,
                            )
                            .size(14)
                            .text_size(13),
                        ]
                        .spacing(4),
                    );
                } else {
                    body = body.push(
                        text(crate::i18n::t!("mapper-save-local-signed-out"))
                            .size(11)
                            .style(muted),
                    );
                }

                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }

                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(text(crate::i18n::t!("action-create")).size(13))
                        .style(builtins::button::primary)
                        .on_press_maybe(
                            (!name.trim().is_empty()).then_some(Message::CreateAtlasConfirmed)
                        ),
                ]
                .spacing(10)
                .align_y(Vertical::Center);

                (
                    crate::i18n::t!("mapper-new-folder"),
                    scrolling_body(body, footer),
                )
            }
            Modal::ConfirmDeleteAtlas {
                name,
                maps,
                folder,
                busy,
                error,
                ..
            } => {
                let mut body = column![
                    text(crate::i18n::t!("mapper-delete-folder-question", "name" => name)).size(13)
                ]
                .spacing(10);
                let ready = match folder {
                    None => {
                        body = body.push(
                            text(crate::i18n::t!("mapper-folder-empty"))
                                .size(12)
                                .style(muted),
                        );
                        true
                    }
                    Some(picker) => {
                        body = body
                            .push(
                                text(
                                    crate::i18n::t!("mapper-folder-maps-go", "count" => maps.len()),
                                )
                                .size(12)
                                .style(muted),
                            )
                            .push(picker.view(Message::DeleteAtlasConfirmed));
                        picker.ready()
                    }
                };
                if let Some(error) = error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press_maybe((!*busy).then_some(Message::ModalDismissed)),
                    button(text(crate::i18n::t!("mapper-delete-folder")).size(13))
                        .style(builtins::button::primary)
                        .on_press_maybe((ready && !*busy).then_some(Message::DeleteAtlasConfirmed)),
                ]
                .spacing(10)
                .align_y(Vertical::Center);

                (
                    crate::i18n::t!("mapper-delete-folder"),
                    scrolling_body(body, footer),
                )
            }
            Modal::ReviewLocalMove {
                reviews,
                error,
                names,
                ..
            } => {
                let detail = if let Some(error) = error {
                    error.clone()
                } else if reviews.is_none() {
                    crate::i18n::t!("mapper-loading")
                } else {
                    crate::i18n::t!("mapper-local-move-shared-warning")
                };
                let mut confirm = button(text(crate::i18n::t!("mapper-move-action")).size(13))
                    .style(builtins::button::primary);
                if reviews.is_some() && error.is_none() {
                    confirm = confirm.on_press(Message::LocalMoveConfirmed);
                }
                let body = column![
                    container(scrollable(text(names.join("\n")).size(13)).width(Length::Fill))
                        .max_height(150),
                    text(detail).size(13),
                ]
                .spacing(16);
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    confirm
                ]
                .spacing(10)
                .align_y(Vertical::Center);
                (
                    crate::i18n::t!("mapper-local-move-title"),
                    scrolling_body(body, footer),
                )
            }
            Modal::MoveAtlasStorage {
                name,
                area_count,
                source,
                destination,
                ..
            } => {
                let storage_label = |storage| match storage {
                    MapStorage::Local => crate::i18n::t!("mapper-save-local"),
                    MapStorage::Cloud => crate::i18n::t!("mapper-save-cloud"),
                    MapStorage::Session => unreachable!("atlases are durable"),
                };
                let detail = match area_count {
                    0 => crate::i18n::t!("mapper-folder-empty"),
                    n => crate::i18n::t!("mapper-folder-maps-move-along", "count" => n),
                };
                let body = column![
                    text(name.clone()).size(13),
                    text(format!(
                        "{} → {}",
                        storage_label(*source),
                        storage_label(*destination)
                    ))
                    .size(12),
                    text(detail).size(12).style(muted),
                ]
                .spacing(10);
                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(text(crate::i18n::t!("area-list-move-action")).size(13))
                        .style(builtins::button::primary)
                        .on_press(Message::MoveAtlasStorageConfirmed),
                ]
                .spacing(10)
                .align_y(Vertical::Center);
                (
                    crate::i18n::t!("area-list-move-action"),
                    scrolling_body(body, footer),
                )
            }
            Modal::MoveArea {
                area_id,
                area_name,
                current,
                targets,
                make_folder,
                new_folder,
            } => {
                // A session map isn't moved but saved, into a folder.
                let saving = current.storage == MapStorage::Session;
                let intro = if saving {
                    crate::i18n::t!("mapper-save-area-in", "name" => area_name)
                } else {
                    crate::i18n::t!("mapper-move-area-to", "name" => area_name)
                };
                let mut list = column![text(intro).size(13)].spacing(6);

                for (destination, label) in targets {
                    list = list.push(move_target_button(
                        label.clone(),
                        *current == *destination,
                        Message::MoveAreaTo {
                            area: *area_id,
                            destination: *destination,
                        },
                    ));
                }

                match new_folder {
                    Some(form) => {
                        list = list.push(new_folder_form(
                            form,
                            targets.is_empty(),
                            saving,
                            Message::MoveAreaNewFolder,
                        ));
                    }
                    None if *make_folder => {
                        list = list.push(
                            button(text(crate::i18n::t!("mapper-folder-new-option")).size(13))
                                .style(builtins::button::link)
                                .on_press(Message::MoveAreaNewFolder(NewFolderMessage::Opened)),
                        );
                    }
                    None => {}
                }

                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                ]
                .align_y(Vertical::Center);

                (
                    if saving {
                        crate::i18n::t!("mapper-save-to-folder")
                    } else {
                        crate::i18n::t!("mapper-move-to-folder")
                    },
                    scrolling_body(list, footer),
                )
            }
            Modal::ShareAtlas(dialog) => (
                crate::i18n::t!("mapper-share-folder-title", "name" => &dialog.atlas_name),
                share_atlas_view(dialog),
            ),
            Modal::Share(dialog) => (crate::i18n::t!("mapper-share"), share_view(dialog)),
            Modal::CopyArea(dialog) => {
                let intro = if dialog.duplicate {
                    crate::i18n::t!(
                        "mapper-copy-duplicate-intro",
                        "name" => &dialog.source_name
                    )
                } else {
                    crate::i18n::t!(
                        "mapper-copy-shared-intro",
                        "name" => &dialog.source_name
                    )
                };
                let mut body = column![
                    text(intro).size(12),
                    text_input(
                        crate::i18n::ts!("mapper-copy-name-placeholder"),
                        &dialog.name
                    )
                    .size(14)
                    .on_input(Message::CopyAreaNameChanged)
                    .on_submit(Message::CopyAreaConfirmed),
                    dialog.folder.view(Message::CopyAreaConfirmed),
                ]
                .spacing(10);

                if let Some(line) = dialog.secrets.line() {
                    body = body.push(text(line).size(11).style(muted));
                }

                // A duplicate starts inactive; say so up front.
                if dialog.duplicate {
                    body = body.push(
                        text(crate::i18n::t!("mapper-copy-duplicate-inactive"))
                            .size(11)
                            .style(muted),
                    );
                }

                if let Some(error) = &dialog.error {
                    body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
                }
                if let Some(report) = &dialog.atlas_report {
                    body = body.push(text(report.clone()).size(12).style(builtins::text::success));
                }

                // Whole-atlas copy is offered only when the source's atlas
                // id survived projection (viewer holds an atlas-scope grant);
                // never on an owner duplicate.
                if !dialog.duplicate && dialog.atlas_id.is_some() {
                    body = body.push(
                        column![
                            iced::widget::rule::horizontal(1),
                            text(crate::i18n::t!("mapper-copy-atlas-offer"))
                                .size(11)
                                .style(muted),
                            button(text(crate::i18n::t!("mapper-copy-whole-atlas")).size(12))
                                .style(builtins::button::secondary)
                                .on_press_maybe(
                                    (!dialog.busy).then_some(Message::CopyAtlasRequested)
                                ),
                        ]
                        .spacing(6),
                    );
                }

                let footer = row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                    button(
                        text(if dialog.busy {
                            crate::i18n::t!("mapper-copying")
                        } else {
                            crate::i18n::t!("mapper-copy")
                        })
                        .size(13)
                    )
                    .style(builtins::button::primary)
                    .on_press_maybe(
                        (!dialog.busy && !dialog.name.trim().is_empty() && dialog.folder.ready())
                            .then_some(Message::CopyAreaConfirmed)
                    ),
                ]
                .spacing(10)
                .align_y(Vertical::Center);

                let title = if dialog.duplicate {
                    crate::i18n::t!("mapper-duplicate-map")
                } else {
                    crate::i18n::t!("mapper-copy-to-my-maps")
                };
                (title, scrolling_body(body, footer))
            }
            Modal::TransferOffer(dialog) => (
                crate::i18n::t!("mapper-transfer-title", "name" => dialog.subject.name()),
                transfer_offer_view(dialog),
            ),
            Modal::ServersChecklist {
                name,
                servers,
                checked,
                ..
            } => (
                crate::i18n::t!("mapper-show-on-servers"),
                servers_checklist_view(name, servers, checked),
            ),
            Modal::Multi(dialog) => (dialog.title(), dialog.view()),
            Modal::Clan(modal) => modal.view(),
            Modal::ClanMapShare(dialog) => super::clan_map_share::view(dialog),
            Modal::PutInClan(dialog) => super::clan_map_share::put_in_clan_view(dialog),
        };

        let width = match self {
            Modal::Share(_) | Modal::ShareAtlas(_) => 600.0,
            Modal::TransferOffer(_) => 460.0,
            Modal::Multi(dialog) => dialog.width(),
            Modal::Clan(modal) => modal.width(),
            Modal::ClanMapShare(_) => 600.0,
            Modal::PutInClan(_) => 460.0,
            _ => 380.0,
        };

        container(column![
            container(
                row![text(title).size(14)]
                    .padding([0, 10])
                    .align_y(Vertical::Center)
                    .height(Length::Fill)
            )
            .style(builtins::container::modal_title_bar)
            .height(34.0)
            .width(Length::Fill),
            container(body)
                .style(builtins::container::modal_body)
                .padding(14)
                .width(Length::Fill),
        ])
        .style(builtins::container::modal_container)
        .width(width)
        .max_height(680)
        .into()
    }
}

// ===========================================================================
// Share dialog view
// ===========================================================================

pub(super) fn section_label(label: String) -> iced::widget::Text<'static, crate::Theme> {
    text(label).size(11).style(muted)
}

/// The "Show this atlas on:" checklist body — one checkbox per server entry,
/// pre-checked from the current association. Toggles apply live (each emits a
/// [`Message::ScopeServerToggled`]); an empty tick set means Unassigned (shown
/// everywhere).
fn servers_checklist_view<'a>(
    name: &'a str,
    servers: &'a [String],
    checked: &'a std::collections::BTreeSet<String>,
) -> ThemedElement<'a, Message> {
    let mut list =
        column![text(crate::i18n::t!("mapper-show-name-on", "name" => name)).size(13)].spacing(6);

    if servers.is_empty() {
        list = list.push(
            text(crate::i18n::t!("mapper-no-server-entries"))
                .size(12)
                .style(muted),
        );
    } else {
        for server in servers {
            let entry = server.clone();
            list = list.push(
                checkbox(checked.contains(server))
                    .label(server.clone())
                    .size(14)
                    .text_size(13)
                    .on_toggle(move |show| Message::ScopeServerToggled {
                        entry: entry.clone(),
                        show,
                    }),
            );
        }
        list = list.push(
            text(crate::i18n::t!("mapper-unchecked-all-servers"))
                .size(11)
                .style(muted),
        );
    }

    let footer = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-done")).size(13))
            .style(builtins::button::primary)
            .on_press(Message::ModalDismissed),
    ]
    .align_y(Vertical::Center);

    scrolling_body(list, footer)
}

fn share_view(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut content = Column::new().spacing(12);

    // The map alone needs no picker.
    if dialog.places.iter().any(|place| place.source.is_secret()) {
        content = content.push(place_picker(dialog));
    }

    let secret = dialog.target.is_secret();
    if secret && !dialog.shares_map {
        content = content.push(
            text(crate::i18n::t!(
                "mapper-secret-map-hint",
                "map" => &dialog.area_name
            ))
            .size(12)
            .style(muted),
        );
    }

    content = content.push(if dialog.clan_of(dialog.target).is_some() {
        clan_recipients_section(dialog)
    } else {
        recipients_section(dialog)
    });
    content = content.push(if secret {
        secret_flags_section(dialog)
    } else {
        map_flags_section(dialog)
    });

    if !dialog.results.is_empty() {
        content = content.push(results_section(dialog));
    }

    content = content.push(iced::widget::rule::horizontal(1));
    content = content.push(if secret {
        secret_access_section(dialog)
    } else {
        manage_section(dialog)
    });
    if let Some(listing) = clan_access_listing(dialog) {
        content = content.push(listing);
    }

    let busy = dialog.submitting;
    let any_recipient = if dialog.clan_of(dialog.target).is_some() {
        !dialog.clan_recipients().is_empty()
    } else {
        !dialog.recipients().is_empty()
    };
    let share_enabled = !busy && any_recipient;
    let mut buttons = row![];
    if dialog.put_in_clan && dialog.target.is_map() {
        buttons = buttons.push(
            button(text(crate::i18n::t!("clan-share-put-in-clan")).size(13))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::PutInClan)),
        );
    }
    let buttons = buttons
        .push(
            row![
                space::horizontal(),
                button(text(crate::i18n::t!("action-close")).size(13))
                    .style(builtins::button::secondary)
                    .on_press(Message::ModalDismissed),
                button(
                    text(if busy {
                        crate::i18n::t!("mapper-sharing")
                    } else {
                        crate::i18n::t!("mapper-share")
                    })
                    .size(13)
                )
                .style(builtins::button::primary)
                .on_press_maybe(share_enabled.then_some(share(ShareMessage::Submit))),
            ]
            .spacing(10)
            .width(Length::Fill)
            .align_y(Vertical::Center),
        )
        .spacing(10)
        .align_y(Vertical::Center);

    scrolling_body(content, buttons)
}

/// The place picker: the "Add to" look, a dot and a pick list, no label.
fn place_picker(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let selected = dialog.place().cloned();
    let color = selected.as_ref().and_then(|place| place.color);
    let mut picker = iced::widget::Row::new()
        .spacing(8.0)
        .align_y(Vertical::Center);
    if color.is_some() {
        picker = picker.push(super::secrets::dot(color));
    }
    picker
        .push(
            pick_list(dialog.places.as_slice(), selected, |place| {
                share(ShareMessage::PlacePicked(place))
            })
            .text_size(13.0)
            .padding(Padding {
                top: 3.0,
                bottom: 3.0,
                left: 8.0,
                right: 6.0,
            }),
        )
        .into()
}

/// The friend list with its filter. Friends who already hold the viewer's
/// grant on the picked place are shown but can't be picked.
fn recipients_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let recipients = column![
        section_label(crate::i18n::t!("mapper-recipients")),
        text_input(
            crate::i18n::ts!("mapper-filter-handle-placeholder"),
            &dialog.filter,
        )
        .size(13)
        .on_input(|value| share(ShareMessage::FilterChanged(value))),
    ]
    .spacing(4);

    let mut friend_list = Column::new().spacing(2);
    match &dialog.friends {
        None => {
            friend_list = friend_list.push(
                text(crate::i18n::t!("mapper-loading-friends"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            friend_list =
                friend_list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(friends)) => {
            let friends: Vec<&FriendView> = friends
                .iter()
                .filter(|friend| Some(friend.user_id) != dialog.owner_id)
                .collect();
            let filter = dialog.filter.trim().to_lowercase();
            let mut any = false;
            for friend in &friends {
                let label = friend_label(friend);
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                any = true;
                let user_id = friend.user_id;
                let checked = dialog.selected.contains(&user_id);
                let item: ThemedElement<'_, Message> = if dialog.has_access(user_id) {
                    row![
                        checkbox(checked).label(label).size(14).text_size(13),
                        text(crate::i18n::t!("mapper-has-access"))
                            .size(11)
                            .style(muted),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center)
                    .into()
                } else {
                    checkbox(checked)
                        .label(label)
                        .size(14)
                        .text_size(13)
                        .on_toggle(move |checked| {
                            share(ShareMessage::RecipientToggled(user_id, checked))
                        })
                        .into()
                };
                friend_list = friend_list.push(item);
            }
            if friends.is_empty() {
                friend_list = friend_list.push(
                    text(crate::i18n::t!("mapper-no-friends-share"))
                        .size(12)
                        .style(muted),
                );
            } else if !any {
                friend_list = friend_list.push(
                    text(crate::i18n::t!("mapper-no-friends-filter"))
                        .size(12)
                        .style(muted),
                );
            }
        }
    }
    recipients
        .push(
            container(scrollable(friend_list))
                .max_height(140.0)
                .width(Length::Fill),
        )
        .into()
}

/// A Clan Secret's recipients: its clan's groups, then its members, or a
/// note that only groups are listed when the viewer may not read the member
/// directory. Those who already have a grant on it are shown but can't be
/// picked.
fn clan_recipients_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut list = Column::new().spacing(2);
    match dialog.clan_data.get(&dialog.target) {
        None => {
            list = list.push(
                text(crate::i18n::t!("mapper-loading"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            list = list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(data)) => {
            let filter = dialog.filter.trim().to_lowercase();
            for (id, label) in data
                .groups()
                .into_iter()
                .chain(data.members(dialog.viewer_id))
            {
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                let checked = dialog.selected.contains(&id);
                let item: ThemedElement<'_, Message> = if dialog.has_access(id) {
                    row![
                        checkbox(checked).label(label).size(14).text_size(13),
                        text(crate::i18n::t!("mapper-has-access"))
                            .size(11)
                            .style(muted),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center)
                    .into()
                } else {
                    checkbox(checked)
                        .label(label)
                        .size(14)
                        .text_size(13)
                        .on_toggle(move |checked| {
                            share(ShareMessage::RecipientToggled(id, checked))
                        })
                        .into()
                };
                list = list.push(item);
            }
            if data.members.is_none() {
                list = list.push(
                    text(crate::i18n::t!("mapper-clan-members-hidden"))
                        .size(12)
                        .style(muted),
                );
            }
        }
    }
    column![
        section_label(crate::i18n::t!("mapper-recipients")),
        text_input(
            crate::i18n::ts!("mapper-filter-handle-placeholder"),
            &dialog.filter,
        )
        .size(13)
        .on_input(|value| share(ShareMessage::FilterChanged(value))),
        container(scrollable(list))
            .max_height(140.0)
            .width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

/// Who reads the picked Clan Secret now and why, as the viewer may see it.
fn clan_access_listing(dialog: &ShareDialog) -> Option<ThemedElement<'_, Message>> {
    dialog.clan_of(dialog.target)?;
    let Some(Ok(data)) = dialog.clan_data.get(&dialog.target) else {
        return None;
    };
    let access = data.access.as_ref()?;
    let groups: HashMap<Uuid, String> = data.groups().into_iter().collect();
    let mut list = Column::new()
        .spacing(4)
        .push(text(crate::i18n::t!("mapper-access")).size(13));
    for reader in &access.members {
        let reasons: Vec<String> = reader
            .reasons
            .iter()
            .filter_map(|reason| super::clan_secrets::reason_label(reason, &groups))
            .collect();
        list = list.push(
            column![
                text(super::clan_secrets::reader_name(reader, dialog.viewer_id)).size(13),
                text(reasons.join(" \u{00B7} ")).size(11).style(muted),
            ]
            .spacing(1),
        );
    }
    Some(container(scrollable(list)).max_height(180.0).into())
}

/// What sharing the map gives, and which servers it names.
fn map_flags_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut content = Column::new().spacing(12);
    let mut caps = column![
        section_label(crate::i18n::t!("mapper-they-can")),
        checkbox(dialog.can_edit)
            .label(crate::i18n::t!("mapper-can-edit-area"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Edit, value))),
    ]
    .spacing(6);
    caps = caps.push(
        checkbox(dialog.can_reshare)
            .label(crate::i18n::t!("mapper-can-reshare"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Reshare, value))),
    );
    caps = caps.push(
        checkbox(dialog.can_copy)
            .label(crate::i18n::t!("mapper-can-copy-area"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Copy, value))),
    );

    // Full-deputy. Owner-minted only; on the server it implies all the caps
    // above (incl. re-share). Everything the owner can do EXCEPT transfer ownership
    // or appoint other admins.
    if dialog.is_owner {
        caps = caps.push(
            checkbox(dialog.can_admin)
                .label(crate::i18n::t!("mapper-make-admin-area"))
                .size(14)
                .text_size(13)
                .on_toggle(|value| share(ShareMessage::FlagToggled(GrantFlag::Admin, value))),
        );
    }
    content = content.push(caps);

    // ===== disclose servers (§4.2 consent moment) =========================
    if !dialog.host_hints.is_empty() {
        let mut section = column![
            section_label(crate::i18n::t!("mapper-disclose-servers")),
            text(crate::i18n::t!("mapper-disclose-servers-help"))
                .size(11)
                .style(muted),
        ]
        .spacing(4);
        for (host, checked) in &dialog.host_hints {
            let host = host.clone();
            let toggle_host = host.clone();
            section = section.push(
                checkbox(*checked)
                    .label(host)
                    .size(14)
                    .text_size(13)
                    .on_toggle(move |value| {
                        share(ShareMessage::HostHintToggled(toggle_host.clone(), value))
                    }),
            );
        }
        content = content.push(section);
    }

    content.into()
}

/// The labels of the Secret boxes, in order.
fn secret_flag_label(flag: SecretFlag) -> String {
    match flag {
        SecretFlag::Add => crate::i18n::t!("mapper-secret-can-add"),
        SecretFlag::Edit => crate::i18n::t!("mapper-secret-can-edit"),
        SecretFlag::Share => crate::i18n::t!("mapper-secret-can-share"),
        SecretFlag::Copy => crate::i18n::t!("mapper-secret-can-copy"),
    }
}

/// Whether a box shows at all: Can share for the owner, Can copy for
/// whoever may give it. The others show to every sharer, off and locked
/// when they may not give them.
fn shows_secret_flag(flag: SecretFlag, may_change: bool, sharer: &SecretSharer) -> bool {
    match flag {
        SecretFlag::Share => sharer.is_owner && may_change,
        SecretFlag::Copy => may_change,
        SecretFlag::Add | SecretFlag::Edit => true,
    }
}

/// What sharing a Secret gives. None checked is view only. Can share is
/// the owner's to give, and Can copy is offered only to a sharer who holds
/// it; a manager's other boxes they don't hold stay off.
fn secret_flags_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut caps = column![section_label(crate::i18n::t!("mapper-they-can"))].spacing(6);
    let Some(sharer) = dialog.sharer() else {
        return caps.into();
    };
    for flag in SecretFlag::ALL {
        if !shows_secret_flag(flag, sharer.may_give(flag), &sharer) {
            continue;
        }
        let mut item = checkbox(dialog.secret_flags.get(flag))
            .label(secret_flag_label(flag))
            .size(14)
            .text_size(13);
        if sharer.may_give(flag) {
            item = item.on_toggle(move |value| share(ShareMessage::SecretFlagToggled(flag, value)));
        }
        caps = caps.push(item);
    }
    caps.into()
}

/// One line per friend the last Share press reached.
fn results_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut results = Column::new().spacing(2);
    for (label, outcome) in &dialog.results {
        match outcome {
            ShareOutcome::Shared | ShareOutcome::SharedWithMap => {
                results = results.push(
                    text(crate::i18n::t!("mapper-shared-with", "recipient" => label))
                        .size(12)
                        .style(builtins::text::success),
                );
                if matches!(outcome, ShareOutcome::SharedWithMap) {
                    results = results.push(
                        text(crate::i18n::t!(
                            "mapper-now-sees-map",
                            "recipient" => label,
                            "map" => &dialog.area_name
                        ))
                        .size(12)
                        .style(builtins::text::success),
                    );
                }
            }
            ShareOutcome::Failed(CloudError::NotFoundOrNoAccess) => {
                results = results.push(
                    text(crate::i18n::t!("mapper-share-failed", "recipient" => label))
                        .size(12)
                        .style(builtins::text::danger),
                );
            }
            ShareOutcome::Failed(error) => {
                results = results.push(
                    text(crate::i18n::t!(
                        "mapper-share-error",
                        "recipient" => label,
                        "error" => display_error(error)
                    ))
                    .size(12)
                    .style(builtins::text::danger),
                );
            }
        }
    }
    results.into()
}

/// The badges of a Secret grant: "add · edit · copy", or "view".
fn secret_badges(actions: &BTreeSet<String>) -> String {
    let flags = SecretFlags::of(actions);
    let mut badges = Vec::new();
    if flags.add {
        badges.push(crate::i18n::t!("mapper-badge-add"));
    }
    if flags.edit {
        badges.push(crate::i18n::t!("mapper-badge-edit"));
    }
    if flags.share {
        badges.push(crate::i18n::t!("mapper-badge-share"));
    }
    if flags.copy {
        badges.push(crate::i18n::t!("mapper-badge-copy"));
    }
    if badges.is_empty() {
        crate::i18n::t!("mapper-badge-view")
    } else {
        badges.join(" \u{00b7} ")
    }
}

/// Who has access to the picked Secret: a flat list, oldest first.
fn secret_access_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text(crate::i18n::t!("mapper-who-has-access")).size(13)].spacing(6);
    if let Some(error) = &dialog.manage_error {
        section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
    }
    match dialog.secret_grants.get(&dialog.target) {
        None => {
            section = section.push(
                text(crate::i18n::t!("mapper-loading"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(grants)) if grants.is_empty() => {
            section = section.push(
                text(crate::i18n::t!("mapper-not-shared"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(grants)) => {
            let mut list = Column::new().spacing(2);
            for grant in grants {
                list = list.push(secret_grant_row(dialog, grant));
                if let Some(edit) = &dialog.secret_editing
                    && edit.original.id == grant.id
                {
                    list = list.push(secret_grant_edit_row(dialog, edit));
                }
                if dialog.revoking == Some(grant.id) {
                    list = list.push(secret_revoke_row(dialog));
                }
            }
            section = section.push(container(scrollable(list)).max_height(220.0));
        }
    }
    section.into()
}

/// "Tomas  add · edit  shared by you  [Edit flags] [Revoke]".
fn secret_grant_row<'a>(
    dialog: &'a ShareDialog,
    grant: &'a SecretGrant,
) -> ThemedElement<'a, Message> {
    let grantee = grant
        .grantee_nickname
        .clone()
        .unwrap_or_else(|| grant.grantee_id.to_string());
    let shared_by = if Some(grant.grantor_id) == dialog.viewer_id {
        Some(crate::i18n::t!("mapper-shared-by-you"))
    } else {
        dialog
            .grantor_name(grant)
            .map(|handle| crate::i18n::t!("mapper-shared-by", "handle" => handle))
    };
    let mut label = row![
        text(grantee).size(13),
        text(secret_badges(&grant.actions)).size(11).style(muted),
        space::horizontal(),
    ]
    .spacing(8)
    .align_y(Vertical::Center);
    if let Some(shared_by) = shared_by {
        label = label.push(text(shared_by).size(11).style(muted));
    }
    let mut item = row![container(label).padding([4, 8]).width(Length::Fill)]
        .spacing(4)
        .align_y(Vertical::Center);
    if dialog.sharer().is_some_and(|sharer| sharer.manages(grant)) {
        let id = grant.id;
        item = item.push(
            button(text(crate::i18n::t!("mapper-edit-flags")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::EditGrant(id))),
        );
        item = item.push(
            button(text(crate::i18n::t!("mapper-revoke")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::RevokeRequested(id))),
        );
    }
    item.into()
}

fn secret_grant_edit_row<'a>(
    dialog: &'a ShareDialog,
    edit: &'a SecretGrantEdit,
) -> ThemedElement<'a, Message> {
    let mut flags = iced::widget::Row::new()
        .spacing(8)
        .align_y(Vertical::Center);
    if let Some(sharer) = dialog.sharer() {
        for flag in SecretFlag::ALL {
            let may_toggle = sharer.may_toggle(edit, flag);
            if !shows_secret_flag(flag, may_toggle, &sharer) {
                continue;
            }
            let mut item = checkbox(edit.flags.get(flag))
                .label(secret_flag_label(flag))
                .size(14)
                .text_size(12);
            if may_toggle {
                item = item.on_toggle(move |value| {
                    share(ShareMessage::EditSecretFlagToggled(flag, value))
                });
            }
            flags = flags.push(item);
        }
    }
    let mut block = column![flags].spacing(6).padding([4, 0]);
    if let Some(error) = &edit.error {
        block = block.push(text(error.clone()).size(11).style(builtins::text::danger));
    }
    block = block.push(
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::EditCancelled)),
            button(
                text(if edit.saving {
                    crate::i18n::t!("mapper-saving")
                } else {
                    crate::i18n::t!("action-save")
                })
                .size(11)
            )
            .style(builtins::button::primary)
            .on_press_maybe((!edit.saving).then_some(share(ShareMessage::EditSaved))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );
    container(block).padding([0, 16]).width(Length::Fill).into()
}

fn secret_revoke_row(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let name = dialog
        .place()
        .map(|place| place.name.clone())
        .unwrap_or_default();
    let block = column![
        text(crate::i18n::t!("mapper-revoke-secret-warning", "name" => name))
            .size(11)
            .style(builtins::text::danger),
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::RevokeCancelled)),
            button(
                text(if dialog.revoke_busy {
                    crate::i18n::t!("mapper-revoking")
                } else {
                    crate::i18n::t!("mapper-revoke")
                })
                .size(11)
            )
            .style(builtins::button::primary)
            .on_press_maybe((!dialog.revoke_busy).then_some(share(ShareMessage::RevokeConfirmed))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    ]
    .spacing(6)
    .padding([4, 0]);
    container(block).padding([0, 16]).width(Length::Fill).into()
}

#[allow(clippy::too_many_lines)]
fn manage_section(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text(crate::i18n::t!("mapper-who-has-access")).size(13)].spacing(6);

    if let Some(error) = &dialog.manage_error {
        section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
    }

    match &dialog.tree {
        None => {
            section = section.push(
                text(crate::i18n::t!("mapper-loading"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(nodes)) if nodes.is_empty() => {
            section = section.push(
                text(crate::i18n::t!("mapper-not-shared"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(nodes)) => {
            let handles = tree_handles(nodes);
            let mut list = Column::new().spacing(2);
            for node in nodes {
                list = list.push(grant_row(dialog, node, &handles));
                if let Some(edit) = &dialog.editing
                    && edit.id == node.grant.id
                {
                    list = list.push(grant_edit_row(node, edit));
                }
                if dialog.revoking == Some(node.grant.id) {
                    list = list.push(revoke_confirm_row(dialog, node));
                }
            }
            section = section.push(container(scrollable(list)).max_height(220.0));
        }
    }

    section.into()
}

/// Grant id -> grantee handle, to attribute child grants to the re-sharer
/// who made them (a child's grantor is its parent's grantee).
pub(super) fn tree_handles(nodes: &[GrantTreeNode]) -> HashMap<Uuid, String> {
    nodes
        .iter()
        .filter_map(|node| {
            node.grantee_nickname
                .clone()
                .map(|handle| (node.grant.id, handle))
        })
        .collect()
}

/// A grant's capabilities as compact badges: "edit · copy", or "view".
pub(super) fn grant_badges(grant: &ShareGrant) -> String {
    let mut badges = Vec::new();
    if grant.can_edit {
        badges.push(crate::i18n::t!("mapper-badge-edit"));
    }
    if grant.can_reshare {
        badges.push(crate::i18n::t!("mapper-badge-reshare"));
    }
    if grant.can_copy {
        badges.push(crate::i18n::t!("mapper-badge-copy"));
    }
    if badges.is_empty() {
        crate::i18n::t!("mapper-badge-view")
    } else {
        badges.join(" \u{00b7} ")
    }
}

/// A map grant's badges, saying when it reaches the map through its
/// folder.
pub(super) fn tree_badges(grant: &ShareGrant) -> String {
    let mut badges = grant_badges(grant);
    if grant.atlas_id.is_some() {
        badges.push_str(&format!(" ({})", crate::i18n::t!("mapper-badge-atlas")));
    }
    badges
}

/// Who made a grant on a map, as Who has access says it to the viewer
/// (`viewer_id`), who owns the map or not.
pub(super) fn shared_by(
    grant: &ShareGrant,
    viewer_id: Option<Uuid>,
    is_owner: bool,
    owner_nickname: Option<&str>,
    handles: &HashMap<Uuid, String>,
) -> String {
    if viewer_id == Some(grant.grantor_id) {
        crate::i18n::t!("mapper-shared-by-you")
    } else if let Some(handle) = grant
        .parent_grant_id
        .and_then(|parent| handles.get(&parent))
    {
        crate::i18n::t!("mapper-shared-via", "handle" => handle)
    } else if is_owner {
        // Root grants are made by the owner; if that isn't recognizably the
        // viewer (no profile loaded), still attribute honestly.
        crate::i18n::t!("mapper-shared-by-you")
    } else {
        match owner_nickname {
            Some(handle) => crate::i18n::t!("mapper-shared-by", "handle" => handle),
            None => crate::i18n::t!("mapper-shared-by-owner"),
        }
    }
}

/// One row of the manage tree: indentation by depth, the grantee handle,
/// compact capability badges, attribution, and (when permitted) edit/revoke.
fn grant_row<'a>(
    dialog: &'a ShareDialog,
    node: &'a GrantTreeNode,
    handles: &HashMap<Uuid, String>,
) -> ThemedElement<'a, Message> {
    let grant = &node.grant;
    let id = grant.id;

    let grantee = node
        .grantee_nickname
        .clone()
        .unwrap_or_else(|| grant.grantee_id.to_string());
    let badge_text = tree_badges(grant);
    let shared_by = shared_by(
        grant,
        dialog.viewer_id,
        dialog.is_owner,
        dialog.owner_nickname.as_deref(),
        handles,
    );

    let indent = f32::from(u8::try_from(node.depth.clamp(0, 12)).unwrap_or(0)) * 16.0;

    let label = row![
        text(grantee).size(13),
        text(badge_text).size(11).style(muted),
        space::horizontal(),
        text(shared_by).size(11).style(muted),
    ]
    .spacing(8)
    .align_y(Vertical::Center);

    // Owner may edit every row; a re-sharer only the grants they made.
    let may_edit = dialog.is_owner || dialog.viewer_id == Some(grant.grantor_id);

    let mut item = row![
        space::horizontal().width(indent),
        container(label).padding([5, 10]).width(Length::Fill),
    ]
    .spacing(4)
    .align_y(Vertical::Center);

    if may_edit {
        item = item.push(
            button(text(crate::i18n::t!("mapper-edit-flags")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::EditGrant(id))),
        );
        item = item.push(
            button(text(crate::i18n::t!("mapper-revoke")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::RevokeRequested(id))),
        );
    }

    item.into()
}

fn grant_edit_row<'a>(node: &'a GrantTreeNode, edit: &'a GrantEdit) -> ThemedElement<'a, Message> {
    let mut flags = row![
        checkbox(edit.can_edit)
            .label(crate::i18n::t!("mapper-badge-edit"))
            .size(14)
            .text_size(12)
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Edit, value))),
        checkbox(edit.can_reshare)
            .label(crate::i18n::t!("mapper-badge-reshare"))
            .size(14)
            .text_size(12)
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Reshare, value))),
        checkbox(edit.can_copy)
            .label(crate::i18n::t!("mapper-badge-copy"))
            .size(14)
            .text_size(12)
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Copy, value))),
    ]
    .spacing(8)
    .align_y(Vertical::Center);

    let mut admin_box = checkbox(edit.can_admin)
        .label(crate::i18n::t!("mapper-flag-admin"))
        .size(14)
        .text_size(12);
    if edit.allow_admin {
        admin_box = admin_box
            .on_toggle(|value| share(ShareMessage::EditFlagToggled(GrantFlag::Admin, value)));
    }
    flags = flags.push(admin_box);

    let mut block = column![flags].spacing(6).padding([4, 0]);

    if node.grant.can_reshare && !edit.can_reshare {
        block = block.push(
            text(crate::i18n::t!("mapper-remove-reshare-warning"))
                .size(11)
                .style(muted),
        );
    }
    if let Some(error) = &edit.error {
        block = block.push(text(error.clone()).size(11).style(builtins::text::danger));
    }

    block = block.push(
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::EditCancelled)),
            button(
                text(if edit.saving {
                    crate::i18n::t!("mapper-saving")
                } else {
                    crate::i18n::t!("action-save")
                })
                .size(11)
            )
            .style(builtins::button::primary)
            .on_press_maybe((!edit.saving).then_some(share(ShareMessage::EditSaved))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );

    container(block).padding([0, 16]).width(Length::Fill).into()
}

fn revoke_confirm_row<'a>(
    dialog: &'a ShareDialog,
    node: &'a GrantTreeNode,
) -> ThemedElement<'a, Message> {
    let mut block = column![
        text(crate::i18n::t!("mapper-revoke-warning"))
            .size(11)
            .style(builtins::text::danger),
    ]
    .spacing(6)
    .padding([4, 0]);

    if node.grant.atlas_id.is_some() {
        block = block.push(
            text(crate::i18n::t!("mapper-revoke-atlas-warning"))
                .size(11)
                .style(builtins::text::danger),
        );
    }

    block = block.push(
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::secondary)
                .on_press(share(ShareMessage::RevokeCancelled)),
            button(
                text(if dialog.revoke_busy {
                    crate::i18n::t!("mapper-revoking")
                } else {
                    crate::i18n::t!("mapper-revoke")
                })
                .size(11)
            )
            .style(builtins::button::primary)
            .on_press_maybe((!dialog.revoke_busy).then_some(share(ShareMessage::RevokeConfirmed))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );

    container(block).padding([0, 16]).width(Length::Fill).into()
}

// ===========================================================================
// Move-to-folder + share-folder views
// ===========================================================================

/// One selectable folder target in the move modal; the current folder shows a
/// check.
/// The Move dialog's new-folder form: name a folder, choose where it is kept
/// (when the cloud is available), and move the map into it. `first` says the
/// viewer has no folder yet; `saving` words it for a session map.
pub(super) fn new_folder_form(
    form: &NewFolderForm,
    first: bool,
    saving: bool,
    new_folder: fn(NewFolderMessage) -> Message,
) -> ThemedElement<'static, Message> {
    let mut body = column![].spacing(8);
    if first {
        body = body.push(
            text(crate::i18n::t!("mapper-folder-none"))
                .size(12)
                .style(muted),
        );
    }
    body = body.push(text(crate::i18n::t!("mapper-name-new-folder")).size(13));
    body = body.push(
        text_input(
            crate::i18n::ts!("mapper-folder-name-placeholder"),
            &form.name,
        )
        .size(14)
        .on_input(move |name| new_folder(NewFolderMessage::Name(name)))
        .on_submit(new_folder(NewFolderMessage::Confirmed)),
    );
    if form.cloud_available {
        body = body.push(
            column![
                section_label(crate::i18n::t!("mapper-save-in")),
                radio(
                    crate::i18n::t!("mapper-save-cloud"),
                    MapStorage::Cloud,
                    Some(form.storage),
                    move |storage| new_folder(NewFolderMessage::Storage(storage)),
                )
                .size(14)
                .text_size(13),
                radio(
                    crate::i18n::t!("mapper-save-local"),
                    MapStorage::Local,
                    Some(form.storage),
                    move |storage| new_folder(NewFolderMessage::Storage(storage)),
                )
                .size(14)
                .text_size(13),
            ]
            .spacing(4),
        );
    }
    if let Some(error) = &form.error {
        body = body.push(text(error.clone()).size(12).style(builtins::text::danger));
    }
    let ready = form.name().is_some() && !form.busy;
    body = body.push(
        row![
            space::horizontal(),
            button(
                text(if saving {
                    crate::i18n::t!("mapper-folder-create-and-save")
                } else {
                    crate::i18n::t!("mapper-folder-create-and-move")
                })
                .size(13)
            )
            .style(builtins::button::primary)
            .on_press_maybe(ready.then_some(new_folder(NewFolderMessage::Confirmed))),
        ]
        .align_y(Vertical::Center),
    );
    container(body).padding([6, 0]).into()
}

pub(super) fn move_target_button(
    label: String,
    selected: bool,
    message: Message,
) -> iced::widget::Button<'static, Message, crate::Theme> {
    let item = row![
        text(label).size(13),
        space::horizontal(),
        text(if selected { "\u{2713}" } else { "" })
            .size(13)
            .style(muted),
    ]
    .align_y(Vertical::Center);
    button(item)
        .style(if selected {
            builtins::button::list_item_selected
        } else {
            builtins::button::list_item
        })
        .width(Length::Fill)
        // The area's current folder is non-actionable (no redundant re-file).
        .on_press_maybe((!selected).then_some(message))
}

/// Recipients with their filter, as every folder Share dialog shows them.
pub(super) fn recipient_picker<'a>(
    filter: &'a str,
    placeholder: &'static str,
    on_filter: impl Fn(String) -> Message + 'a,
    list: Column<'a, Message, crate::Theme>,
) -> ThemedElement<'a, Message> {
    column![
        section_label(crate::i18n::t!("mapper-recipients")),
        text_input(placeholder, filter).size(13).on_input(on_filter),
        container(scrollable(list))
            .max_height(140.0)
            .width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

/// The folder Share dialog: help, recipients, what they can do, anything
/// else the dialog needs said, who has access, and Close / Share. A clan
/// folder's dialog shares the same body with groups as recipients.
pub(super) fn folder_share_layout<'a>(
    help: String,
    recipients: ThemedElement<'a, Message>,
    they_can: ThemedElement<'a, Message>,
    extra: Vec<ThemedElement<'a, Message>>,
    manage: ThemedElement<'a, Message>,
    submitting: bool,
    submit: Option<Message>,
) -> ThemedElement<'a, Message> {
    let mut content = column![text(help).size(12).style(muted), recipients, they_can].spacing(12);
    for section in extra {
        content = content.push(section);
    }
    content = content.push(iced::widget::rule::horizontal(1));
    content = content.push(manage);

    let buttons = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-close")).size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
        button(
            text(if submitting {
                crate::i18n::t!("mapper-sharing")
            } else {
                crate::i18n::t!("mapper-share")
            })
            .size(13)
        )
        .style(builtins::button::primary)
        .on_press_maybe(submit),
    ]
    .spacing(10)
    .align_y(Vertical::Center);

    scrolling_body(content, buttons)
}

#[allow(clippy::too_many_lines)]
fn share_atlas_view(dialog: &ShareAtlasDialog) -> ThemedElement<'_, Message> {
    let mut friend_list = Column::new().spacing(2);
    match &dialog.friends {
        None => {
            friend_list = friend_list.push(
                text(crate::i18n::t!("mapper-loading-friends"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            friend_list =
                friend_list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(friends)) if friends.is_empty() => {
            friend_list = friend_list.push(
                text(crate::i18n::t!("mapper-no-friends-share"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(friends)) => {
            let filter = dialog.filter.trim().to_lowercase();
            let mut any = false;
            for friend in friends {
                let label = friend_label(friend);
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                any = true;
                let user_id = friend.user_id;
                friend_list = friend_list.push(
                    checkbox(dialog.selected.contains(&user_id))
                        .label(label)
                        .size(14)
                        .text_size(13)
                        .on_toggle(move |checked| {
                            share_atlas(ShareAtlasMessage::RecipientToggled(user_id, checked))
                        }),
                );
            }
            if !any {
                friend_list = friend_list.push(
                    text(crate::i18n::t!("mapper-no-friends-filter"))
                        .size(12)
                        .style(muted),
                );
            }
        }
    }
    let recipients = recipient_picker(
        &dialog.filter,
        crate::i18n::ts!("mapper-filter-handle-placeholder"),
        |value| share_atlas(ShareAtlasMessage::FilterChanged(value)),
        friend_list,
    );
    let mut extra: Vec<ThemedElement<'_, Message>> = Vec::new();

    // ===== capabilities (atlas scope; admin allowed) ======================
    let they_can: ThemedElement<'_, Message> = column![
        section_label(crate::i18n::t!("mapper-they-can")),
        checkbox(dialog.can_edit)
            .label(crate::i18n::t!("mapper-can-edit-folder"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(GrantFlag::Edit, value))),
        checkbox(dialog.can_reshare)
            .label(crate::i18n::t!("mapper-can-reshare"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(
                GrantFlag::Reshare,
                value
            ))),
        checkbox(dialog.can_copy)
            .label(crate::i18n::t!("mapper-can-copy-folder"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(GrantFlag::Copy, value))),
        checkbox(dialog.can_admin)
            .label(crate::i18n::t!("mapper-make-admin-folder"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| share_atlas(ShareAtlasMessage::FlagToggled(
                GrantFlag::Admin,
                value
            ))),
    ]
    .spacing(6)
    .into();

    // ===== disclose servers (§4.2 consent moment) =========================
    if !dialog.host_hints.is_empty() {
        let mut section = column![
            section_label(crate::i18n::t!("mapper-disclose-servers")),
            text(crate::i18n::t!("mapper-disclose-servers-help"))
                .size(11)
                .style(muted),
        ]
        .spacing(4);
        for (host, checked) in &dialog.host_hints {
            let host = host.clone();
            let toggle_host = host.clone();
            section = section.push(
                checkbox(*checked)
                    .label(host)
                    .size(14)
                    .text_size(13)
                    .on_toggle(move |value| {
                        share_atlas(ShareAtlasMessage::HostHintToggled(
                            toggle_host.clone(),
                            value,
                        ))
                    }),
            );
        }
        extra.push(section.into());
    }

    // ===== per-recipient results ==========================================
    if !dialog.results.is_empty() {
        let mut results = Column::new().spacing(2);
        for (label, result) in &dialog.results {
            results = results.push(match result {
                Ok(()) => text(crate::i18n::t!("mapper-shared-with", "recipient" => label))
                    .size(12)
                    .style(builtins::text::success),
                Err(CloudError::NotFoundOrNoAccess) => text(format!(
                    "Couldn't share with {label} — are you still friends?"
                ))
                .size(12)
                .style(builtins::text::danger),
                Err(error) => text(format!("Couldn't share with {label} — {error}"))
                    .size(12)
                    .style(builtins::text::danger),
            });
        }
        extra.push(results.into());
    }

    let share_enabled = !dialog.submitting && !dialog.selected.is_empty();
    folder_share_layout(
        crate::i18n::t!("mapper-folder-share-help"),
        recipients,
        they_can,
        extra,
        atlas_manage_section(dialog),
        dialog.submitting,
        share_enabled.then_some(share_atlas(ShareAtlasMessage::Submit)),
    )
}

/// Each friend's handle, for naming the people a folder is shared with.
pub(super) fn friend_handles(friends: &[FriendView]) -> HashMap<Uuid, String> {
    friends
        .iter()
        .filter_map(|friend| {
            friend
                .nickname
                .clone()
                .map(|handle| (friend.user_id, handle))
        })
        .collect()
}

fn atlas_manage_section(dialog: &ShareAtlasDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text(crate::i18n::t!("mapper-who-has-access")).size(13)].spacing(6);

    if let Some(error) = &dialog.manage_error {
        section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
    }

    // Resolve grantee handles from the friends list when available.
    let handles = match &dialog.friends {
        Some(Ok(friends)) => friend_handles(friends),
        _ => HashMap::new(),
    };

    match &dialog.grants {
        None => {
            section = section.push(
                text(crate::i18n::t!("mapper-loading"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            section = section.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(rows)) if rows.is_empty() => {
            section = section.push(
                text(crate::i18n::t!("mapper-not-shared"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Ok(rows)) => {
            let mut list = Column::new().spacing(2);
            for row in rows {
                list = list.push(atlas_grant_row(row, &handles));
                if dialog.revoking == Some(row.grant.id) {
                    list = list.push(atlas_revoke_confirm_row(dialog));
                }
            }
            section = section.push(container(scrollable(list)).max_height(200.0));
        }
    }

    section.into()
}

fn atlas_grant_row<'a>(
    row: &'a ShareGrantRow,
    handles: &HashMap<Uuid, String>,
) -> ThemedElement<'a, Message> {
    let grant = &row.grant;
    let grantee = handles
        .get(&grant.grantee_id)
        .cloned()
        .unwrap_or_else(|| grant.grantee_id.to_string());
    let badge_text = grant_badges(grant);

    row![
        text(grantee).size(13),
        text(badge_text).size(11).style(muted),
        space::horizontal(),
        button(text(crate::i18n::t!("mapper-revoke")).size(11))
            .style(builtins::button::secondary)
            .on_press(share_atlas(ShareAtlasMessage::RevokeRequested(grant.id))),
    ]
    .spacing(8)
    .align_y(Vertical::Center)
    .into()
}

fn atlas_revoke_confirm_row(dialog: &ShareAtlasDialog) -> ThemedElement<'_, Message> {
    let block = column![
        text(crate::i18n::t!("mapper-revoke-folder-warning"))
            .size(11)
            .style(builtins::text::danger),
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::secondary)
                .on_press(share_atlas(ShareAtlasMessage::RevokeCancelled)),
            button(
                text(if dialog.revoke_busy {
                    crate::i18n::t!("mapper-revoking")
                } else {
                    crate::i18n::t!("mapper-revoke")
                })
                .size(11)
            )
            .style(builtins::button::primary)
            .on_press_maybe(
                (!dialog.revoke_busy).then_some(share_atlas(ShareAtlasMessage::RevokeConfirmed))
            ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    ]
    .spacing(6)
    .padding([4, 0]);

    container(block).padding([0, 16]).width(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn long_transfer_and_share_dialogs_keep_their_actions_reachable() {
        let friends: Vec<_> = (100..160)
            .map(|n| friend(n, &format!("Explorer {n} with a long nickname")))
            .collect();
        let transfer = Modal::TransferOffer(TransferDialog {
            subject: TransferSubject::Area(AreaId(id(99)), "The northern lands".to_string()),
            friends: Some(Ok(friends.clone())),
            destinations: Default::default(),
            operation: Uuid::new_v4(),
            filter: String::new(),
            selected: Some(TransferRecipient::User(id(100))),
            ownership: smudgy_cloud::clan_maps::MapOwnership::Clan,
            submitting: false,
            error: Some("A detailed error that wraps onto several lines. ".repeat(5)),
            sent: false,
        });
        let mut sharing = dialog(true, true, vec![map_place()]);
        sharing.friends = Some(Ok(friends));
        sharing.selected.insert(id(100));
        sharing.put_in_clan = true;
        sharing.results = (100..160)
            .map(|n| (format!("Explorer {n}"), ShareOutcome::Shared))
            .collect();
        let sharing = Modal::Share(Box::new(sharing));
        for (modal, label, name) in [
            (&transfer, crate::i18n::t!("mapper-send-offer"), "transfer"),
            (&sharing, crate::i18n::t!("mapper-share"), "share"),
        ] {
            for size in [(380, 360), (600, 500), (900, 800)] {
                let messages = crate::widgets::dialog::tests::check_actions(
                    modal.view(),
                    size,
                    std::slice::from_ref(&label),
                    name,
                )
                .await;
                assert_eq!(messages.len(), 2, "{name}: both clicks reach the action");
                assert!(messages.iter().all(|m| matches!(
                    m,
                    Message::Transfer(TransferMessage::Submit)
                        | Message::Share(ShareMessage::Submit)
                )));
            }
        }
    }

    async fn render_review(modal: &Modal, prefix: &str) {
        use crate::assets::fonts;
        use iced::advanced::{
            Layout,
            layout::Limits,
            renderer::{Headless, Style},
            widget::Tree,
        };
        use iced::{Rectangle, Size, mouse};
        iced_graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(fonts::GEIST_VF_BYTES.into());
        for (width, height) in [(520, 500), (800, 600)] {
            let mut renderer =
                <iced::Renderer as Headless>::new(fonts::GEIST_VF, 16.0.into(), Some("tiny-skia"))
                    .await
                    .unwrap();
            let mut element = modal.view();
            let mut tree = Tree::new(element.as_widget());
            let size = Size::new(width as f32, height as f32);
            let layout = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &Limits::new(Size::ZERO, size),
            );
            assert!(layout.size().width <= size.width && layout.size().height <= size.height);
            let mut messages = Vec::new();
            element.as_widget_mut().update(
                &mut tree,
                &iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
            let theme = smudgy_theme::smudgy();
            element.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &Style {
                    text_color: theme.styles.text.normal,
                },
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(size),
            );
            if let Some(path) = std::env::var_os("SMUDGY_MOVE_SCREENSHOTS") {
                let path = std::path::PathBuf::from(path);
                std::fs::create_dir_all(&path).unwrap();
                image::save_buffer(
                    path.join(format!("{prefix}-{width}.png")),
                    &renderer.screenshot(
                        Size::new(width, height),
                        1.0,
                        theme.styles.general.background,
                    ),
                    width,
                    height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn local_move_warning_keeps_long_map_lists_inside_the_dialog() {
        let modal = Modal::ReviewLocalMove {
            request: super::super::local_move::Request::Maps {
                ids: vec![],
                destination: MapDestination::loose(MapStorage::Local),
                multi: true,
            },
            reviews: Some(vec![]),
            error: None,
            names: (1..=40)
                .map(|n| format!("Map {n}: A long title for a shared adventure"))
                .collect(),
        };
        for size in [(380, 360), (520, 500)] {
            let messages = crate::widgets::dialog::tests::check_actions(
                modal.view(),
                size,
                &[crate::i18n::t!("mapper-move-action")],
                "local-move-actions",
            )
            .await;
            assert_eq!(
                messages
                    .iter()
                    .filter(|m| matches!(m, Message::LocalMoveConfirmed))
                    .count(),
                2
            );
        }
        render_review(&modal, "local-move").await;
    }

    #[tokio::test]
    async fn property_review_keeps_long_values_bounded_and_requires_each_choice() {
        use super::super::links::fixture::{KEEP, area, secret, serving_with_review, the_maps};
        use super::super::moves::{MoveKind, MoveMessage};
        use futures::StreamExt;
        use smudgy_cloud::access_review::AccessReview;
        use smudgy_cloud::mutation::{
            PropertyAddress, PropertyChoice, PropertyConflict, PropertyResolution,
        };
        let properties: Vec<_> = (1..=20)
            .map(|n| PropertyAddress {
                name: format!("Route note {n}"),
                room_number: Some(smudgy_cloud::RoomNumber(1)),
                room_source: SourceId::Map,
            })
            .collect();
        let review = AccessReview {
            token: "UI fixture".into(),
            requires_confirmation: true,
            destination_notice: true,
            changes: vec![],
            preserves_undo: false,
            property_conflicts: properties
                .iter()
                .map(|property| PropertyConflict {
                    property: property.clone(),
                    source_value:
                        "Incoming route with a long description that must wrap inside the review."
                            .repeat(3),
                    destination_value: "Existing route kept by the destination source.".repeat(3),
                    can_replace: true,
                })
                .collect(),
        };
        let mapper =
            serving_with_review(the_maps(&["read", "add", "edit", "remove"]), Some(review)).await;
        let content = smudgy_cloud::MovedContent {
            properties: properties.clone(),
            ..smudgy_cloud::MovedContent::default()
        };
        let mut window = super::super::test_window(mapper, area(KEEP));
        let update = window.start_move(
            SourceId::Map,
            secret(),
            content,
            MoveKind::Asked { undoable: true },
        );
        let mut stream = iced_runtime::task::into_stream(update.task).unwrap();
        let iced_runtime::Action::Output(message) = stream.next().await.unwrap() else {
            panic!("review result")
        };
        let _ = window.update(message);
        let blocked = window.update_move(MoveMessage::ReviewConfirmed);
        assert!(iced_runtime::task::into_stream(blocked.task).is_none());
        assert!(matches!(window.modal, Some(Modal::ReviewMove { .. })));
        let update = window.update_move(MoveMessage::ResolveProperty(PropertyResolution {
            property: properties[0].clone(),
            keep: PropertyChoice::Destination,
        }));
        let mut stream = iced_runtime::task::into_stream(update.task).unwrap();
        let iced_runtime::Action::Output(message) = stream.next().await.unwrap() else {
            panic!("fresh review result")
        };
        let _ = window.update(message);
        let modal = window.modal.as_ref().unwrap();
        assert!(
            matches!(modal, Modal::ReviewMove { reviewed: Some(review), .. } if !review.properties_resolved())
        );
        render_review(modal, "property-conflicts").await;
        window.cancel_move_review();
        assert!(window.modal.is_none());
        assert!(
            window
                .mapper
                .get_current_atlas()
                .get_area(&area(KEEP))
                .is_some()
        );
    }

    const OWNER: u128 = 1;
    const VIEWER: u128 = 2;
    const TOMAS: u128 = 3;
    const MIRA: u128 = 4;
    const ALL: &[&str] = &["read", "add", "edit", "remove", "manage_access", "copy"];

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn secret(n: u128) -> SourceId {
        SourceId::Secret(id(n))
    }

    fn actions(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn place(source: SourceId, name: &str, held: &[&str]) -> SharePlace {
        SharePlace {
            clan_id: None,
            source,
            name: name.to_string(),
            color: None,
            held: actions(held),
        }
    }

    fn map_place() -> SharePlace {
        place(SourceId::Map, "DV", &[])
    }

    fn sources(places: &[SharePlace]) -> Vec<SourceId> {
        places.iter().map(|place| place.source).collect()
    }

    fn friend(n: u128, nickname: &str) -> FriendView {
        FriendView {
            user_id: id(n),
            nickname: Some(nickname.to_string()),
            since: Utc::now(),
        }
    }

    fn secret_grant(
        n: u128,
        on: SourceId,
        grantor: u128,
        grantee: u128,
        held: &[&str],
    ) -> SecretGrant {
        SecretGrant {
            id: id(n),
            secret_id: on,
            area_id: AreaId(id(99)),
            owner_id: id(OWNER),
            grantor_id: id(grantor),
            grantee_id: id(grantee),
            grantee_nickname: None,
            grantor_nickname: None,
            actions: actions(held),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn map_node(grantor: u128, grantee: u128, area: Option<AreaId>) -> GrantTreeNode {
        GrantTreeNode {
            grant: ShareGrant {
                id: Uuid::new_v4(),
                owner_id: id(OWNER),
                grantor_id: id(grantor),
                grantee_id: id(grantee),
                area_id: area,
                atlas_id: area.is_none().then_some(AtlasId(id(98))),
                can_edit: false,
                can_reshare: false,
                can_copy: false,
                can_admin: false,
                parent_grant_id: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                grantor_nickname: None,
                owner_nickname: None,
                host_hints: None,
            },
            depth: 0,
            grantee_nickname: None,
        }
    }

    /// The dialog as `VIEWER` opens it on map 99, friends with Tomas, Mira
    /// and the map's owner.
    fn dialog(is_owner: bool, shares_map: bool, places: Vec<SharePlace>) -> ShareDialog {
        let target = opening_place(&places, SourceId::Map).expect("something to share");
        ShareDialog {
            area_id: AreaId(id(99)),
            area_name: "DV".to_string(),
            is_owner,
            shares_map,
            owner_id: Some(id(OWNER)),
            owner_nickname: Some("owner".to_string()),
            viewer_id: Some(id(VIEWER)),
            places,
            target,
            friends: Some(Ok(vec![
                friend(TOMAS, "tomas"),
                friend(MIRA, "mira"),
                friend(OWNER, "owner"),
            ])),
            filter: String::new(),
            selected: HashSet::new(),
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            can_admin: false,
            secret_flags: SecretFlags::default(),
            tree: None,
            secret_grants: HashMap::new(),
            editing: None,
            secret_editing: None,
            revoking: None,
            revoke_busy: false,
            submitting: false,
            results: Vec::new(),
            manage_error: None,
            host_hints: Vec::new(),
            put_in_clan: false,
            clan_data: HashMap::new(),
        }
    }

    #[test]
    fn the_picker_offers_what_each_viewer_may_share() {
        // The owner: the map, then every Secret.
        let owner = share_places(
            map_place(),
            true,
            true,
            [
                place(secret(10), "Bookcase", ALL),
                place(secret(11), "Cellar", ALL),
            ],
        );
        assert_eq!(sources(&owner), [SourceId::Map, secret(10), secret(11)]);

        // A map admin re-shares the map, but no Secret without managing it.
        let admin = share_places(
            map_place(),
            true,
            false,
            [place(secret(10), "Bookcase", &["read", "edit"])],
        );
        assert_eq!(sources(&admin), [SourceId::Map]);

        // A manager who can't share the map: only the Secrets they manage.
        let manager = share_places(
            map_place(),
            false,
            false,
            [
                place(secret(10), "Bookcase", &["read", "add", "manage_access"]),
                place(secret(11), "Cellar", &["read", "add", "edit", "remove"]),
            ],
        );
        assert_eq!(sources(&manager), [secret(10)]);

        // A Clan Secret is shared by those who manage access to it, never
        // by owning the map it sits on; the toolbar's Share follows the same
        // rule.
        let clan = |source, held: &[&str]| SharePlace {
            clan_id: Some(Uuid::from_u128(99)),
            ..place(source, "Quest", held)
        };
        let linked = share_places(
            map_place(),
            true,
            true,
            [
                clan(secret(12), &["read", "add", "edit", "remove"]),
                clan(secret(13), &["read", "manage_access"]),
            ],
        );
        assert_eq!(sources(&linked), [SourceId::Map, secret(13)]);
        let managed = clan(secret(13), &["read", "manage_access"]);
        assert!(shares_secret(false, managed.clan_id, &managed.held));
        let unmanaged = clan(secret(12), &["read", "add"]);
        assert!(!shares_secret(true, unmanaged.clan_id, &unmanaged.held));

        // A reader shares nothing.
        let reader = share_places(
            map_place(),
            false,
            false,
            [place(secret(11), "Cellar", &["read", "edit"])],
        );
        assert!(reader.is_empty());
        assert!(!shares_map(&AreaAccess {
            is_owner: false,
            can_edit: true,
            can_reshare: false,
            can_copy: true,
            can_admin: false,
            include_secrets: false,
        }));
        assert!(!shares_secret(false, None, &actions(&["read", "edit"])));
        assert!(shares_secret(true, None, &actions(&["read"])));
    }

    #[test]
    fn the_dialog_opens_on_the_add_to_secret_else_the_map() {
        let owner = vec![
            map_place(),
            place(secret(10), "Bookcase", ALL),
            place(secret(11), "Cellar", ALL),
        ];
        assert_eq!(opening_place(&owner, secret(11)), Some(secret(11)));
        assert_eq!(
            opening_place(&owner, SourceId::Private),
            Some(SourceId::Map)
        );
        assert_eq!(opening_place(&owner, SourceId::Map), Some(SourceId::Map));
        // A Secret the viewer adds to but may not share is not where it opens.
        let manager = vec![place(secret(10), "Bookcase", &["read", "manage_access"])];
        assert_eq!(opening_place(&manager, secret(11)), Some(secret(10)));
        assert_eq!(opening_place(&[], SourceId::Map), None);
    }

    #[test]
    fn secret_boxes_clamp_to_what_the_sharer_may_give() {
        let everything = SecretFlags {
            add: true,
            edit: true,
            share: true,
            copy: true,
        };
        let all = actions(ALL);
        let owner = SecretSharer {
            is_owner: true,
            held: &all,
        };
        assert_eq!(everything.clamped(&owner), everything);

        // A manager without `remove` can't give Can edit (edit and remove),
        // nor Can copy without `copy`, and never Can share.
        let held = actions(&["read", "add", "edit", "manage_access"]);
        let manager = SecretSharer {
            is_owner: false,
            held: &held,
        };
        assert_eq!(
            everything.clamped(&manager),
            SecretFlags {
                add: true,
                ..SecretFlags::default()
            }
        );
        let held = actions(&["read", "add", "manage_access", "copy"]);
        let copier = SecretSharer {
            is_owner: false,
            held: &held,
        };
        assert_eq!(
            everything.clamped(&copier),
            SecretFlags {
                add: true,
                copy: true,
                ..SecretFlags::default()
            }
        );
        assert_eq!(
            SecretFlags {
                add: true,
                edit: true,
                share: false,
                copy: false,
            }
            .actions(),
            ["add", "edit", "remove"]
        );
        assert_eq!(
            SecretFlags {
                add: true,
                copy: true,
                ..SecretFlags::default()
            }
            .actions(),
            ["add", "copy"],
            "Can copy is its own box, in the wire's order"
        );
        assert!(SecretFlags::default().actions().is_empty(), "view only");
    }

    /// The copy dialog says how many of the Secrets the viewer reads come
    /// along, and nothing about Secrets on a map where they read none.
    #[test]
    fn the_copy_dialog_counts_the_secrets_that_come_along() {
        let along = |read, copied| SecretsAlong { read, copied }.line();
        assert_eq!(along(0, 0), None);
        assert_eq!(
            along(2, 0),
            Some(crate::i18n::t!("mapper-copy-secrets-none"))
        );
        assert_eq!(
            along(3, 2),
            Some(crate::i18n::t!("mapper-copy-secrets-along", "count" => 2))
        );
        let one = along(1, 1).unwrap();
        assert!(one.contains('1') && one != along(3, 2).unwrap(), "{one}");
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            for count in [1, 2, 5, 22] {
                let text =
                    smudgy_i18n::t!(translator, "mapper-copy-secrets-along", "count" => count);
                assert!(
                    text.contains(&count.to_string()) && !text.contains('⟦'),
                    "{}: {text}",
                    catalog.tag
                );
            }
        }
    }

    /// Can copy shows only to a sharer who may give it: the owner, or a
    /// manager who holds `copy`. On an existing grant it also shows when the
    /// grant carries it, so a manager may clear it.
    #[test]
    fn can_copy_is_offered_only_to_a_sharer_who_holds_it() {
        let all = actions(ALL);
        let owner = SecretSharer {
            is_owner: true,
            held: &all,
        };
        let held = actions(&["read", "add", "edit", "remove", "manage_access"]);
        let manager = SecretSharer {
            is_owner: false,
            held: &held,
        };
        let copying = actions(&["read", "add", "manage_access", "copy"]);
        let copier = SecretSharer {
            is_owner: false,
            held: &copying,
        };
        let shows = |sharer: &SecretSharer| {
            shows_secret_flag(SecretFlag::Copy, sharer.may_give(SecretFlag::Copy), sharer)
        };
        assert!(shows(&owner));
        assert!(!shows(&manager));
        assert!(shows(&copier));
        // Add and Edit show to every sharer, locked when out of reach.
        assert!(shows_secret_flag(SecretFlag::Add, false, &manager));

        let edit = |grant: SecretGrant| SecretGrantEdit {
            flags: SecretFlags::of(&grant.actions),
            original: grant,
            saving: false,
            error: None,
        };
        let plain = edit(secret_grant(20, secret(10), OWNER, TOMAS, &["read", "add"]));
        let copies = edit(secret_grant(21, secret(10), OWNER, MIRA, &["read", "copy"]));
        assert!(!manager.may_toggle(&plain, SecretFlag::Copy));
        assert!(
            manager.may_toggle(&copies, SecretFlag::Copy),
            "a manager may clear copy, though they lack it"
        );
        assert!(copier.may_toggle(&plain, SecretFlag::Copy));
        assert!(owner.may_toggle(&plain, SecretFlag::Copy));
        assert!(copies.flags.copy && !copies.flags.add);
    }

    #[test]
    fn friends_holding_the_sharers_grant_have_access() {
        // A re-sharer of the map who manages access to Bookcase.
        let mut dialog = dialog(
            false,
            true,
            vec![
                map_place(),
                place(secret(10), "Bookcase", &["read", "add", "manage_access"]),
            ],
        );
        dialog.pick(secret(10));
        dialog.secret_grants.insert(
            secret(10),
            Ok(vec![
                secret_grant(20, secret(10), VIEWER, TOMAS, &["read", "add"]),
                // Another grantor's grant doesn't stop the viewer granting.
                secret_grant(21, secret(10), OWNER, MIRA, &["read"]),
            ]),
        );
        assert!(dialog.has_access(id(TOMAS)));
        assert!(!dialog.has_access(id(MIRA)));
        dialog.selected.extend([id(TOMAS), id(MIRA), id(OWNER)]);
        let recipients: Vec<Uuid> = dialog
            .recipients()
            .iter()
            .map(|friend| friend.user_id)
            .collect();
        assert_eq!(recipients, [id(MIRA)], "the owner is never a recipient");

        // On the map, only the viewer's own share of this map counts.
        dialog.pick(SourceId::Map);
        dialog.tree = Some(Ok(vec![
            map_node(VIEWER, MIRA, Some(AreaId(id(99)))),
            map_node(VIEWER, TOMAS, None),
        ]));
        assert!(dialog.has_access(id(MIRA)));
        assert!(
            !dialog.has_access(id(TOMAS)),
            "a folder share is another grant"
        );
        assert!(dialog.sees_map(id(TOMAS)) && !dialog.sees_map(id(OWNER)));
    }

    #[test]
    fn checked_friends_stay_checked_when_the_picker_changes() {
        let mut dialog = dialog(
            false,
            false,
            vec![
                place(secret(10), "Bookcase", &["read", "add", "manage_access"]),
                place(
                    secret(11),
                    "Cellar",
                    &["read", "add", "edit", "remove", "manage_access"],
                ),
            ],
        );
        assert_eq!(dialog.target, secret(10));
        dialog.pick(secret(11));
        dialog.selected.extend([id(TOMAS), id(MIRA)]);
        dialog.secret_flags = SecretFlags {
            add: true,
            edit: true,
            share: false,
            copy: false,
        };
        dialog
            .results
            .push(("tomas".to_string(), ShareOutcome::Shared));

        dialog.pick(secret(10));
        assert_eq!(dialog.target, secret(10));
        assert_eq!(dialog.selected, HashSet::from([id(TOMAS), id(MIRA)]));
        assert_eq!(
            dialog.secret_flags,
            SecretFlags {
                add: true,
                ..SecretFlags::default()
            },
            "Can edit is beyond what the manager holds on Bookcase"
        );
        assert!(dialog.results.is_empty());

        // A place that isn't offered is not picked.
        dialog.pick(SourceId::Map);
        assert_eq!(dialog.target, secret(10));
    }

    #[test]
    fn managers_edit_within_their_actions() {
        let held = actions(&["read", "add", "edit", "manage_access"]);
        let manager = SecretSharer {
            is_owner: false,
            held: &held,
        };
        let edit = |grant: SecretGrant| SecretGrantEdit {
            flags: SecretFlags::of(&grant.actions),
            original: grant,
            saving: false,
            error: None,
        };

        // Can edit is already on: the manager may clear it, though they
        // lack `remove`. Off, it stays off.
        let editor = edit(secret_grant(
            20,
            secret(10),
            OWNER,
            TOMAS,
            &["read", "edit", "remove"],
        ));
        assert!(manager.manages(&editor.original));
        assert!(manager.may_toggle(&editor, SecretFlag::Edit));
        assert!(manager.may_toggle(&editor, SecretFlag::Add));
        assert!(!manager.may_toggle(&editor, SecretFlag::Share));
        let viewer = edit(secret_grant(21, secret(10), OWNER, MIRA, &["read"]));
        assert!(!manager.may_toggle(&viewer, SecretFlag::Edit));

        // A grant carrying manage_access is the owner's.
        let co_manager = secret_grant(22, secret(10), OWNER, MIRA, &["read", "manage_access"]);
        assert!(!manager.manages(&co_manager));

        // The owner gives Can share only on grants the owner issued.
        let all = actions(ALL);
        let owner = SecretSharer {
            is_owner: true,
            held: &all,
        };
        assert!(owner.manages(&co_manager));
        assert!(owner.may_toggle(&viewer, SecretFlag::Share));
        let from_manager = edit(secret_grant(23, secret(10), VIEWER, TOMAS, &["read"]));
        assert!(!owner.may_toggle(&from_manager, SecretFlag::Share));
    }

    #[test]
    fn a_clan_folder_offers_member_owned_maps_only_with_the_action() {
        use smudgy_cloud::clan_maps::MapOwnership;
        assert_eq!(
            NewMapOwnership::for_folder(&actions(&["area.create"])),
            None
        );
        let both =
            NewMapOwnership::for_folder(&actions(&["area.create", "area.create_member_owned"]))
                .unwrap();
        assert!(both.clan_allowed);
        assert_eq!(both.picked, MapOwnership::Clan, "Clan-owned is the default");
        let only = NewMapOwnership::for_folder(&actions(&["area.create_member_owned"])).unwrap();
        assert!(!only.clan_allowed);
        assert_eq!(only.picked, MapOwnership::Members);
    }

    /// A clan's choice names its clan in the offer with the picked
    /// ownership, and picking the other ownership follows the pick.
    #[test]
    fn a_clan_pick_carries_whose_the_map_becomes() {
        use smudgy_cloud::clan_maps::MapOwnership;
        let dialog = TransferDialog {
            subject: TransferSubject::Area(AreaId(id(50)), "Solace".to_string()),
            friends: None,
            destinations: super::super::clan_maps::TransferDestinations {
                clans: vec![(id(60), "Lantern Company".to_string())],
                ..Default::default()
            },
            operation: Uuid::new_v4(),
            filter: String::new(),
            selected: Some(TransferRecipient::Clan(id(60), MapOwnership::Clan)),
            ownership: MapOwnership::Clan,
            submitting: false,
            error: None,
            sent: false,
        };
        assert_eq!(dialog.chosen_clan(), Some("Lantern Company"));
        let friend = TransferDialog {
            selected: Some(TransferRecipient::User(id(TOMAS))),
            ..dialog
        };
        assert_eq!(friend.chosen_clan(), None);
    }

    #[test]
    fn saving_keeps_untouched_boxes_exactly() {
        // A grant with `edit` but not `remove` keeps just `edit` when only
        // Can add changes.
        let original = actions(&["read", "edit"]);
        let mut flags = SecretFlags::of(&original);
        assert!(flags.edit);
        flags.add = true;
        assert_eq!(flags.edited_actions(&original), ["add", "edit"]);
        // Turning a box on gives its whole set; off takes it all away.
        flags.edit = false;
        flags.share = true;
        assert_eq!(flags.edited_actions(&original), ["add", "manage_access"]);
        let none = SecretFlags::default();
        assert!(none.edited_actions(&original).is_empty());

        // Can copy is its own box: untouched, it keeps the grant's `copy`
        // exactly; checked, it adds just `copy`.
        let original = actions(&["read", "edit", "copy"]);
        let mut flags = SecretFlags::of(&original);
        assert!(flags.copy && flags.edit && !flags.add);
        flags.add = true;
        assert_eq!(flags.edited_actions(&original), ["add", "edit", "copy"]);
        flags.copy = false;
        assert_eq!(flags.edited_actions(&original), ["add", "edit"]);
        let original = actions(&["read", "add"]);
        let mut flags = SecretFlags::of(&original);
        flags.copy = true;
        assert_eq!(flags.edited_actions(&original), ["add", "copy"]);
    }
}
