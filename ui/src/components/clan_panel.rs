//! The clans panel: the caller's clans, the invitations and Secret ownership
//! offers they received, and one clan's page with its Members, Groups,
//! Access and Settings tabs.
//!
//! Embedded as the "Clans" tab of the settings window, like the Friends
//! panel; the host renders its verified-email gate instead of this panel's
//! `view()` until the email is verified (or [`ClanPanel::needs_email_verification`]
//! reports a server-side `EmailNotVerified`). Dialogs render through
//! [`ClanPanel::modal_view`], which the host lays over its whole page.
//!
//! - **Members**: the member directory with each member's groups, Edit
//!   groups (and Remove member), Invite with proposed groups, and the pending
//!   invitations. A clan that narrowed who reads its directory shows the
//!   caller's own row alone.
//! - **Groups**: the built-in and custom groups; a group's Permissions (its
//!   grants, each a scope, preset chips and a note) and Members.
//! - **Access**: the clan's Clan-owned maps and folders, its packages and the
//!   Clan Secrets the caller reads, each with the groups that reach it.
//! - **Settings**: the clan's name and description, leaving, and deleting.
//!
//! Controls gate on what the server says the caller may do: the clan's
//! `actions`, each group's `actions`, each indexed resource's `actions`, and
//! `is_owner` for the owner-only steps (making members owners, handing out
//! access, deleting the clan). A badge never decides what the caller can do.

mod editor;
mod permission_rows;
mod permissions;
mod views;

use std::collections::BTreeSet;

use iced::Task;
use smudgy_cloud::clan_access::{ClanProfilePatch, IndexedResource, ResourceKind};
use smudgy_cloud::clan_secrets::{ClanSecretGrant, OwnershipOffer};
use smudgy_cloud::clans::{
    ClanGrant, ClanGrantFilter, ClanGroup, ClanGroupPatch, ClanInvitation, ClanMember, ClanSummary,
    ClansOverview, GrantRecipient, GrantScope, ReceivedInvitation, action,
};
use smudgy_cloud::cloud_api::UserRef;
use smudgy_cloud::{AreaId, CloudError, SourceId, Uuid};
use smudgy_palette::{ColorSequence, Rgb};

use crate::cloud_account::CloudHandles;
use crate::components::clan_map_offers;
use crate::components::cloud_errors::display_error;
use crate::presets::ScopeKind;

pub use editor::{
    EditorMessage, GrantEditor, GrantWrite, SecretEditorMessage, SecretGrantEditor, SecretWrite,
};

/// Members shown per page of the directory.
pub const MEMBERS_PER_PAGE: usize = 20;

/// A clan page's tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Members,
    Groups,
    Access,
    Settings,
}

/// A group's tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupTab {
    #[default]
    Permissions,
    Members,
}

/// The Access tab's tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessTab {
    #[default]
    Maps,
    Packages,
    Secrets,
}

/// A clan in the header's switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClanChoice {
    pub id: Uuid,
    pub name: String,
}

impl std::fmt::Display for ClanChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// What the panel hands to another window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// Show a map in the map editor.
    Map(AreaId),
    /// Open a map's Share dialog in the map editor.
    MapAccess(AreaId),
    /// Show a package in Automations, by name.
    Package(String),
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    /// The clan list and invitations, tagged with the credential generation
    /// the request went out under: a reply for an earlier account is
    /// dropped.
    Loaded(u64, Result<ClansOverview, CloudError>),

    NewClanNameChanged(String),
    CreateClan,
    ClanCreated(Result<ClanSummary, CloudError>),

    AcceptInvitation(Uuid),
    DeclineInvitation(Uuid),
    InvitationAnswered(Result<(), CloudError>),

    /// The Secret ownership offers naming the caller, tagged as
    /// [`Message::Loaded`] is.
    OffersLoaded(u64, Result<Vec<OwnershipOffer>, CloudError>),
    /// The map ownership offers naming the caller, tagged as
    /// [`Message::Loaded`] is.
    MapOffers(u64, clan_map_offers::Message),
    /// Accept or decline a Secret ownership offer: `(secret, offer)`.
    AcceptOffer(Uuid, Uuid),
    DeclineOffer(Uuid, Uuid),

    OpenClan(Uuid),
    SwitchClan(ClanChoice),
    /// "All clans": back to the list.
    CloseClan,
    PageLoaded(Uuid, Box<Result<PageData, CloudError>>),
    /// A change to the open clan finished; the page reloads.
    Changed(Uuid, Result<(), CloudError>),
    /// A dialog's change finished: the dialog closes once it is saved, and
    /// stays with the error otherwise.
    DialogSaved(Uuid, Result<(), CloudError>),
    TabSelected(Tab),
    MembersPage(usize),

    // ===== members =====
    OpenInvite,
    InviteNicknameChanged(String),
    InviteGroupToggled(Uuid, bool),
    SendInvite,
    InviteLookupFinished(Uuid, Result<UserRef, CloudError>),
    InviteFinished(Uuid, String, Result<ClanInvitation, CloudError>),
    RevokeInvitation(Uuid),
    EditMemberGroups(Uuid),
    MemberGroupToggled(Uuid, bool),
    MemberOwnerToggled(bool),
    SaveMemberGroups,
    RemoveMemberPressed,
    RemoveMemberConfirmed(Uuid),

    // ===== groups =====
    SelectGroup(Uuid),
    GroupTabSelected(GroupTab),
    RosterLoaded(Uuid, Uuid, Result<Vec<UserRef>, CloudError>),
    OpenNewGroup,
    OpenEditGroup(Uuid),
    GroupNameChanged(String),
    GroupColorPicked(Option<String>),
    SaveGroup,
    GroupCreated(Uuid, Result<ClanGroup, CloudError>),
    DeleteGroupPressed,
    DeleteGroupConfirmed(Uuid),
    OpenAddGroupMember,
    AddMemberFilterChanged(String),
    AddGroupMember(Uuid),
    RemoveGroupMember(Uuid),

    // ===== permissions =====
    AddPermission,
    GroupPermissions(permissions::Message),
    SaveGroupPermissions,
    EditGrant(Uuid),
    RemoveGrantPressed(Uuid),
    RemoveGrantConfirmed(Uuid),
    AssignGroup(ScopeKind, Uuid),
    Editor(EditorMessage),
    SaveGrant,

    // ===== access =====
    AccessTabSelected(AccessTab),
    SelectResource(Uuid),
    SecretGrantsLoaded(Uuid, Result<Vec<ClanSecretGrant>, CloudError>),
    AssignSecretGroup(Uuid),
    EditSecretGrant(Uuid, Uuid),
    RemoveSecretGrant(Uuid, Uuid),
    SecretGrantChanged(Uuid, Result<(), CloudError>),
    SecretEditor(SecretEditorMessage),
    SaveSecretGrant,
    OpenMap(AreaId),
    OpenMapAccess(AreaId),
    OpenPackage(String),
    /// Revoke an outside share of a Clan-owned map.
    RevokeOutsideShare(Uuid),

    // ===== settings =====
    ProfileNameChanged(String),
    ProfileDescriptionChanged(String),
    SaveProfile,
    LeaveClanPressed,
    /// Leaving copies the Member-owned maps the caller owns there to My
    /// maps first, unless unchecked.
    LeaveCopyToggled(bool),
    LeaveClanConfirmed,
    DeleteClanPressed,
    DeleteClanConfirmed,
    /// The caller left or dissolved the clan: back to the list.
    ClanGone(Uuid, Result<(), CloudError>),

    CloseModal,
    ConfirmCancelled,
}

/// Everything one clan's page shows, loaded together.
#[derive(Debug, Clone, Default)]
pub struct PageData {
    clan: Option<ClanSummary>,
    /// `None` without `clan.read_members`.
    members: Option<Vec<ClanMember>>,
    pending: Vec<ClanInvitation>,
    groups: Vec<ClanGroup>,
    /// The grants the caller may see.
    grants: Vec<ClanGrant>,
    maps: Vec<IndexedResource>,
    folders: Vec<IndexedResource>,
    packages: Vec<IndexedResource>,
    secrets: Vec<IndexedResource>,
}

/// The inline "really?" box open on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    RemoveGrant(Uuid),
    LeaveClan,
    DeleteClan,
}

/// The open dialog.
#[derive(Debug, Clone)]
pub enum Modal {
    Invite {
        nickname: String,
        groups: BTreeSet<Uuid>,
        error: Option<String>,
    },
    MemberGroups {
        user_id: Uuid,
        checked: BTreeSet<Uuid>,
        owner: bool,
        confirm_remove: bool,
        error: Option<String>,
    },
    Group {
        /// `None` for a new group.
        group_id: Option<Uuid>,
        name: String,
        color: Option<String>,
        confirm_delete: bool,
        error: Option<String>,
    },
    AddGroupMember {
        group_id: Uuid,
        filter: String,
        error: Option<String>,
    },
    Grant(GrantEditor),
    GroupPermissions(permissions::Permissions),
    SecretGrant(SecretGrantEditor),
}

impl Modal {
    fn set_error(&mut self, message: String) {
        match self {
            Self::Invite { error, .. }
            | Self::MemberGroups { error, .. }
            | Self::Group { error, .. }
            | Self::AddGroupMember { error, .. } => *error = Some(message),
            Self::Grant(editor) => editor.error = Some(message),
            Self::GroupPermissions(editor) => {
                editor.saving = false;
                editor.error = Some(message);
            }
            Self::SecretGrant(editor) => editor.error = Some(message),
        }
    }
}

pub struct ClanPage {
    clan: ClanSummary,
    /// The first load hasn't answered yet.
    loading: bool,
    tab: Tab,
    members: Option<Vec<ClanMember>>,
    member_page: usize,
    pending: Vec<ClanInvitation>,
    groups: Vec<ClanGroup>,
    grants: Vec<ClanGrant>,
    maps: Vec<IndexedResource>,
    folders: Vec<IndexedResource>,
    packages: Vec<IndexedResource>,
    secrets: Vec<IndexedResource>,

    selected_group: Option<Uuid>,
    group_tab: GroupTab,
    /// The selected group's roster: `None` while loading, `Err` when the
    /// caller may not read it.
    roster: Option<(Uuid, Result<Vec<UserRef>, String>)>,

    access_tab: AccessTab,
    selected_resource: Option<Uuid>,
    /// The selected Secret's grants.
    secret_grants: Option<(Uuid, Result<Vec<ClanSecretGrant>, String>)>,

    profile_name: String,
    profile_description: String,

    confirm: Option<Confirm>,
    modal: Option<Modal>,
}

impl ClanPage {
    fn new(clan: ClanSummary) -> Self {
        let profile_name = clan.name.clone();
        let profile_description = clan.description.clone().unwrap_or_default();
        Self {
            clan,
            loading: true,
            tab: Tab::default(),
            members: None,
            member_page: 0,
            pending: Vec::new(),
            groups: Vec::new(),
            grants: Vec::new(),
            maps: Vec::new(),
            folders: Vec::new(),
            packages: Vec::new(),
            secrets: Vec::new(),
            selected_group: None,
            group_tab: GroupTab::default(),
            roster: None,
            access_tab: AccessTab::default(),
            selected_resource: None,
            secret_grants: None,
            profile_name,
            profile_description,
            confirm: None,
            modal: None,
        }
    }

    fn apply(&mut self, data: PageData) {
        if let Some(clan) = data.clan {
            if self.loading || clan.name != self.clan.name {
                self.profile_name.clone_from(&clan.name);
            }
            if self.loading || clan.description != self.clan.description {
                self.profile_description = clan.description.clone().unwrap_or_default();
            }
            self.clan = clan;
        }
        self.members = data.members;
        self.pending = data.pending;
        self.groups = data.groups;
        self.grants = data.grants;
        self.maps = data.maps;
        self.folders = data.folders;
        self.packages = data.packages;
        self.secrets = data.secrets;
        self.modal = match self.modal.take() {
            Some(Modal::GroupPermissions(mut editor)) => {
                editor.refresh(self);
                Some(Modal::GroupPermissions(editor))
            }
            other => other,
        };
        self.loading = false;
        let pages = self.member_pages();
        if self.member_page >= pages {
            self.member_page = pages.saturating_sub(1);
        }
        if self
            .selected_group
            .is_some_and(|id| !self.groups.iter().any(|group| group.id == id))
        {
            self.selected_group = None;
            self.roster = None;
        }
        if self.selected_group.is_none() {
            self.selected_group = self.groups.first().map(|group| group.id);
        }
    }

    fn group(&self, id: Uuid) -> Option<&ClanGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    fn custom_groups(&self) -> impl Iterator<Item = &ClanGroup> {
        self.groups.iter().filter(|group| !group.is_builtin())
    }

    fn member(&self, user: Uuid) -> Option<&ClanMember> {
        self.members
            .as_ref()?
            .iter()
            .find(|member| member.user_id == user)
    }

    fn member_pages(&self) -> usize {
        self.members
            .as_ref()
            .map_or(1, |members| members.len().div_ceil(MEMBERS_PER_PAGE).max(1))
    }

    /// The grants to `group`, oldest first.
    fn grants_to(&self, group: Uuid) -> Vec<&ClanGrant> {
        self.grants
            .iter()
            .filter(|grant| grant.recipient == GrantRecipient::Group { group_id: group })
            .collect()
    }

    /// The grants covering a package: clan-wide ones that give a package
    /// action, and those naming it.
    fn grants_on_package(&self, package: Uuid) -> Vec<&ClanGrant> {
        self.grants
            .iter()
            .filter(|grant| match &grant.scope {
                GrantScope::Clan => grant
                    .actions
                    .iter()
                    .any(|action| action.starts_with("package.") && action != "package.create"),
                GrantScope::Packages { ids } => ids.contains(&package),
                _ => false,
            })
            .collect()
    }

    /// Whether the caller may hand out access anywhere: as a clan owner, or
    /// through a delegation on a folder, map or package, or as a map owner.
    fn can_grant(&self) -> bool {
        self.clan.is_owner
            || self.maps.iter().any(|map| map.owned_by_me)
            || self.clan.can(action::MANAGE_GRANTS)
            || self
                .maps
                .iter()
                .chain(&self.folders)
                .chain(&self.packages)
                .any(|row| row.can(action::MANAGE_GRANTS))
    }

    /// Whether `scope` names a Member-owned map.
    fn names_member_map(&self, scope: &GrantScope) -> bool {
        matches!(scope, GrantScope::Areas { ids } if ids.iter().any(|id| {
            self.maps
                .iter()
                .any(|row| row.id == id.0 && !row.clan_owned())
        }))
    }

    /// The caller's actions on the Clan Secret `secret`.
    fn secret_actions(&self, secret: Uuid) -> Vec<&str> {
        self.secrets
            .iter()
            .find(|row| row.id == secret)
            .map(|row| row.actions.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// Whether the caller may change or remove `grant`: on a Member-owned
    /// map, its active owners alone; otherwise clan owners any, and a
    /// delegate one that hands nothing out (the server checks its bounds).
    fn can_change(&self, grant: &ClanGrant) -> bool {
        if let GrantScope::Areas { ids } = &grant.scope
            && let Some(map) = ids
                .iter()
                .find_map(|id| self.maps.iter().find(|row| row.id == id.0))
            && !map.clan_owned()
        {
            return map.owned_by_me;
        }
        self.clan.is_owner
            || (self.can_grant()
                && grant.may_grant.is_none()
                && !grant.actions.contains(action::MANAGE_GRANTS))
    }

    /// Whether the caller may put `user` in `group` or take them out: clan
    /// owners in every custom group; others in those they hold
    /// `group.assign` on, adding themselves only to a group they created.
    fn assignable(&self, group: &ClanGroup, user: Uuid, me: Uuid) -> Assign {
        if group.is_builtin() || !group.can(action::ASSIGN_GROUP) {
            return Assign::No;
        }
        if user != me || self.clan.is_owner || group.is_member {
            return Assign::Yes;
        }
        if group.created_by_me {
            Assign::Yes
        } else {
            Assign::NotSelf
        }
    }
}

/// Whether the caller may put a member in a group, or take them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assign {
    Yes,
    No,
    /// Adding oneself to a group someone else made takes a clan owner.
    NotSelf,
}

pub struct ClanPanel {
    cloud: CloudHandles,
    /// `None` until the first load.
    clans: Option<Vec<ClanSummary>>,
    invitations: Vec<ReceivedInvitation>,
    /// The Secret ownership offers naming the caller.
    offers: Vec<OwnershipOffer>,
    /// The map ownership offers naming the caller.
    map_offers: clan_map_offers::MapOffers,
    /// Leaving copies the caller's Member-owned maps to My maps first.
    leave_copy: bool,
    new_clan_name: String,
    open: Option<ClanPage>,
    /// What the panel asked another window to show, for the host to take.
    handoff: Option<Handoff>,

    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    /// The server answered `EmailNotVerified`: the host should show the gate.
    email_unverified: bool,
}

impl ClanPanel {
    pub fn new(cloud: CloudHandles) -> Self {
        Self {
            cloud,
            clans: None,
            invitations: Vec::new(),
            offers: Vec::new(),
            map_offers: clan_map_offers::MapOffers::default(),
            leave_copy: true,
            new_clan_name: String::new(),
            open: None,
            handoff: None,
            busy: false,
            error: None,
            notice: None,
            email_unverified: false,
        }
    }

    /// Whether the list has loaded yet (the host refreshes on first open).
    pub fn is_loaded(&self) -> bool {
        self.clans.is_some()
    }

    /// The server rejected a call with `EmailNotVerified` since the last
    /// refresh: the host should render its verify-email gate.
    pub fn needs_email_verification(&self) -> bool {
        self.email_unverified
    }

    /// What the panel asked another window to show since the last call.
    pub fn take_handoff(&mut self) -> Option<Handoff> {
        self.handoff.take()
    }

    /// What waits on the caller: invitations, and Secret and map ownership
    /// offers they have not accepted yet.
    #[must_use]
    pub fn pending(&self) -> usize {
        let me = self.me();
        self.invitations.len()
            + self.map_offers.waiting(me)
            + self
                .offers
                .iter()
                .filter(|offer| !me.is_some_and(|me| offer.accepted_by(me)))
                .count()
    }

    /// Leaves the clan page and its dialogs. Late page replies are ignored
    /// because they no longer have an open page to update.
    pub fn close_clan(&mut self) {
        self.open = None;
        self.clear_feedback();
    }

    /// Reloads the clan list, invitations and Secret ownership offers, and
    /// the open clan's page.
    pub fn refresh(&mut self) -> Task<Message> {
        self.email_unverified = false;
        let account = self.account();
        let client = self.cloud.client.clone();
        let offers = Task::perform(
            async move { client.my_secret_offers().await },
            move |result| Message::OffersLoaded(account, result),
        );
        let map_offers = clan_map_offers::MapOffers::refresh(&self.cloud)
            .map(move |message| Message::MapOffers(account, message));
        let list = Task::batch([self.refresh_list(), offers, map_offers]);
        match &self.open {
            Some(page) => Task::batch([list, self.load_page(page.clan.id)]),
            None => list,
        }
    }

    fn load_page(&self, clan_id: Uuid) -> Task<Message> {
        let client = self.cloud.client.clone();
        Task::perform(
            async move {
                let clan = client.clan(clan_id).await?;
                let read_members = clan.can(action::READ_MEMBERS);
                let see_pending = clan.can(action::INVITE) || clan.can(action::REVOKE_INVITATION);
                // Each part needs actions that can change between calls;
                // losing one is not losing the clan.
                let (members, pending, groups, grants, maps, folders, packages, secrets) = tokio::join!(
                    async {
                        if !read_members {
                            return Ok(None);
                        }
                        match client.clan_members(clan_id).await {
                            Err(CloudError::NotFoundOrNoAccess) => Ok(None),
                            result => result.map(Some),
                        }
                    },
                    async {
                        if see_pending {
                            quiet(client.clan_invitations(clan_id).await)
                        } else {
                            Ok(Vec::new())
                        }
                    },
                    client.clan_groups(clan_id),
                    async {
                        quiet(
                            client
                                .clan_grants(clan_id, ClanGrantFilter::default())
                                .await,
                        )
                    },
                    async { quiet(client.clan_resources(clan_id, ResourceKind::Areas).await) },
                    async { quiet(client.clan_resources(clan_id, ResourceKind::Atlases).await) },
                    async { quiet(client.clan_resources(clan_id, ResourceKind::Packages).await) },
                    async { quiet(client.clan_resources(clan_id, ResourceKind::Secrets).await) },
                );
                Ok(PageData {
                    clan: Some(clan),
                    members: members?,
                    pending: pending?,
                    groups: groups?,
                    grants: grants?,
                    maps: maps?,
                    folders: folders?,
                    packages: packages?,
                    secrets: secrets?,
                })
            },
            move |result| Message::PageLoaded(clan_id, Box::new(result)),
        )
    }

    fn load_roster(&self, clan_id: Uuid, group: &ClanGroup) -> Task<Message> {
        if !group.can(action::INSPECT_GROUP) {
            return Task::none();
        }
        let client = self.cloud.client.clone();
        let group_id = group.id;
        Task::perform(
            async move { client.clan_group_members(clan_id, group_id).await },
            move |result| Message::RosterLoaded(clan_id, group_id, result),
        )
    }

    fn load_secret_grants(&self, secret: Uuid) -> Task<Message> {
        let client = self.cloud.client.clone();
        Task::perform(
            async move { client.clan_secret_grants(&SourceId::Secret(secret)).await },
            move |result| Message::SecretGrantsLoaded(secret, result),
        )
    }

    /// The signed-in user's ID.
    fn me(&self) -> Option<Uuid> {
        self.cloud
            .snapshot
            .get()
            .profile
            .as_ref()
            .map(|profile| profile.id)
    }

    /// The open page, when it is still `clan_id`'s.
    fn page_for(&mut self, clan_id: Uuid) -> Option<&mut ClanPage> {
        self.open.as_mut().filter(|page| page.clan.id == clan_id)
    }

    fn open_id(&self) -> Option<Uuid> {
        self.open.as_ref().map(|page| page.clan.id)
    }

    /// Routes an error to the feedback line, except `EmailNotVerified`, which
    /// flips the panel back to the host's gate.
    fn absorb_error(&mut self, err: &CloudError) {
        if matches!(err, CloudError::EmailNotVerified) {
            self.email_unverified = true;
        } else {
            self.error = Some(display_error(err));
        }
    }

    fn clear_feedback(&mut self) {
        self.error = None;
        self.notice = None;
    }

    /// Runs a change to the open clan, then reloads its page.
    fn change<F>(&mut self, call: F) -> Task<Message>
    where
        F: std::future::Future<Output = Result<(), CloudError>> + Send + 'static,
    {
        let Some(clan_id) = self.open_id() else {
            return Task::none();
        };
        self.clear_feedback();
        Task::perform(call, move |result| Message::Changed(clan_id, result))
    }

    /// Runs a dialog's change: the dialog closes once it is saved.
    fn save_dialog<F>(&mut self, call: F) -> Task<Message>
    where
        F: std::future::Future<Output = Result<(), CloudError>> + Send + 'static,
    {
        let Some(clan_id) = self.open_id() else {
            return Task::none();
        };
        self.clear_feedback();
        Task::perform(call, move |result| Message::DialogSaved(clan_id, result))
    }

    fn modal_mut(&mut self) -> Option<&mut Modal> {
        self.open.as_mut().and_then(|page| page.modal.as_mut())
    }

    #[allow(clippy::too_many_lines)] // one arm per message
    /// The credential generation in use. A list or offers reply carries the
    /// one its request went out under.
    pub(crate) fn account(&self) -> u64 {
        self.cloud.credentials.generation()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                self.clear_feedback();
                self.refresh()
            }
            Message::Loaded(account, _)
            | Message::OffersLoaded(account, _)
            | Message::MapOffers(account, _)
                if account != self.account() =>
            {
                Task::none()
            }
            Message::Loaded(_, result) => {
                match result {
                    Ok(overview) => {
                        self.clans = Some(overview.clans);
                        self.invitations = overview.invitations;
                    }
                    Err(err) => self.absorb_error(&err),
                }
                Task::none()
            }

            // ===== the list =====
            Message::NewClanNameChanged(name) => {
                self.new_clan_name = name;
                Task::none()
            }
            Message::CreateClan => {
                if self.busy {
                    return Task::none();
                }
                self.clear_feedback();
                let name = self.new_clan_name.trim().to_string();
                if name.is_empty() {
                    self.error = Some(crate::i18n::t!("clans-enter-name"));
                    return Task::none();
                }
                self.busy = true;
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.create_clan(&name).await },
                    Message::ClanCreated,
                )
            }
            Message::ClanCreated(result) => {
                self.busy = false;
                match result {
                    Ok(clan) => {
                        self.new_clan_name.clear();
                        let clan_id = clan.id;
                        self.open = Some(ClanPage::new(clan));
                        self.refresh_after(clan_id)
                    }
                    Err(err) => {
                        self.absorb_error(&err);
                        Task::none()
                    }
                }
            }
            Message::AcceptInvitation(id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.accept_clan_invitation(id).await.map(|_| ()) },
                    Message::InvitationAnswered,
                )
            }
            Message::DeclineInvitation(id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.decline_clan_invitation(id).await },
                    Message::InvitationAnswered,
                )
            }
            Message::MapOffers(account, message) => self
                .map_offers
                .update(&self.cloud, message)
                .map(move |message| Message::MapOffers(account, message)),
            Message::LeaveCopyToggled(value) => {
                self.leave_copy = value;
                Task::none()
            }
            Message::OffersLoaded(_, result) => {
                match result {
                    Ok(offers) => self.offers = offers,
                    Err(CloudError::NotFoundOrNoAccess) => self.offers.clear(),
                    Err(err) => self.absorb_error(&err),
                }
                Task::none()
            }
            Message::AcceptOffer(secret, offer) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move {
                        client
                            .accept_secret_offer(&SourceId::Secret(secret), offer)
                            .await
                            .map(|_| ())
                    },
                    Message::InvitationAnswered,
                )
            }
            Message::DeclineOffer(secret, offer) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move {
                        client
                            .decline_secret_offer(&SourceId::Secret(secret), offer)
                            .await
                    },
                    Message::InvitationAnswered,
                )
            }
            // A 404: the invitation or offer was withdrawn meanwhile, or the
            // offer no longer stands; just resync.
            Message::InvitationAnswered(result) => match result {
                Ok(()) | Err(CloudError::NotFoundOrNoAccess) => self.refresh(),
                Err(err) => {
                    self.absorb_error(&err);
                    Task::none()
                }
            },
            Message::OpenClan(clan_id) => self.open_clan(clan_id),
            Message::SwitchClan(choice) => {
                if self.open_id() == Some(choice.id) {
                    return Task::none();
                }
                self.open_clan(choice.id)
            }
            Message::CloseClan => {
                self.close_clan();
                self.refresh()
            }

            // ===== the clan page =====
            Message::PageLoaded(clan_id, result) => self.page_loaded(clan_id, *result),
            Message::Changed(clan_id, result) => {
                if self.page_for(clan_id).is_none() {
                    return Task::none();
                }
                if let Err(err) = result {
                    self.absorb_error(&err);
                }
                self.load_page(clan_id)
            }
            Message::DialogSaved(clan_id, result) => {
                let Some(page) = self.page_for(clan_id) else {
                    return Task::none();
                };
                match result {
                    Ok(()) => {
                        page.modal = None;
                    }
                    Err(CloudError::EmailNotVerified) => self.email_unverified = true,
                    Err(err) => match page.modal.as_mut() {
                        Some(modal) => modal.set_error(display_error(&err)),
                        None => self.error = Some(display_error(&err)),
                    },
                }
                self.load_page(clan_id)
            }
            Message::TabSelected(tab) => {
                let Some(page) = self.open.as_mut() else {
                    return Task::none();
                };
                page.tab = tab;
                page.confirm = None;
                let clan_id = page.clan.id;
                if tab == Tab::Groups
                    && let Some(group) = page.selected_group.and_then(|id| page.group(id).cloned())
                {
                    page.roster = None;
                    return self.load_roster(clan_id, &group);
                }
                Task::none()
            }
            Message::MembersPage(index) => {
                if let Some(page) = self.open.as_mut() {
                    page.member_page = index.min(page.member_pages().saturating_sub(1));
                }
                Task::none()
            }

            // ===== members =====
            Message::OpenInvite => {
                if let Some(page) = self.open.as_mut() {
                    page.modal = Some(Modal::Invite {
                        nickname: String::new(),
                        groups: BTreeSet::new(),
                        error: None,
                    });
                }
                Task::none()
            }
            Message::InviteNicknameChanged(value) => {
                if let Some(Modal::Invite { nickname, .. }) = self.modal_mut() {
                    *nickname = value;
                }
                Task::none()
            }
            Message::InviteGroupToggled(group, on) => {
                if let Some(Modal::Invite { groups, .. }) = self.modal_mut() {
                    if on {
                        groups.insert(group);
                    } else {
                        groups.remove(&group);
                    }
                }
                Task::none()
            }
            Message::SendInvite => self.send_invite(),
            Message::InviteLookupFinished(clan_id, result) => {
                self.invite_lookup_finished(clan_id, result)
            }
            Message::InviteFinished(clan_id, name, result) => {
                self.busy = false;
                let Some(page) = self.page_for(clan_id) else {
                    return Task::none();
                };
                match result {
                    Ok(_) => {
                        page.modal = None;
                        self.notice = Some(crate::i18n::t!("clans-invitation-sent"));
                        self.load_page(clan_id)
                    }
                    // A block, or a lost right to invite: the uniform 404.
                    Err(err) => {
                        let message = match err {
                            CloudError::NotFoundOrNoAccess => {
                                crate::i18n::t!("clans-invite-failed", "name" => name)
                            }
                            other => display_error(&other),
                        };
                        if let Some(modal) = page.modal.as_mut() {
                            modal.set_error(message);
                        }
                        Task::none()
                    }
                }
            }
            Message::RevokeInvitation(invitation_id) => {
                let client = self.cloud.client.clone();
                self.change(async move { client.revoke_clan_invitation(invitation_id).await })
            }
            Message::EditMemberGroups(user_id) => {
                if let Some(page) = self.open.as_mut() {
                    let (checked, owner) = page.member(user_id).map_or_else(
                        || {
                            (
                                page.clan.group_ids.iter().copied().collect(),
                                page.clan.is_owner,
                            )
                        },
                        |member| (member.group_ids.iter().copied().collect(), member.is_owner),
                    );
                    page.modal = Some(Modal::MemberGroups {
                        user_id,
                        checked,
                        owner,
                        confirm_remove: false,
                        error: None,
                    });
                }
                Task::none()
            }
            Message::MemberGroupToggled(group, on) => {
                if let Some(Modal::MemberGroups { checked, .. }) = self.modal_mut() {
                    if on {
                        checked.insert(group);
                    } else {
                        checked.remove(&group);
                    }
                }
                Task::none()
            }
            Message::MemberOwnerToggled(on) => {
                if let Some(Modal::MemberGroups { owner, .. }) = self.modal_mut() {
                    *owner = on;
                }
                Task::none()
            }
            Message::SaveMemberGroups => self.save_member_groups(),
            Message::RemoveMemberPressed => {
                if let Some(Modal::MemberGroups { confirm_remove, .. }) = self.modal_mut() {
                    *confirm_remove = true;
                }
                Task::none()
            }
            Message::RemoveMemberConfirmed(user_id) => {
                let Some(clan_id) = self.open_id() else {
                    return Task::none();
                };
                let client = self.cloud.client.clone();
                self.save_dialog(async move { client.remove_clan_member(clan_id, user_id).await })
            }

            // ===== groups =====
            Message::SelectGroup(group_id) => {
                let Some(page) = self.open.as_mut() else {
                    return Task::none();
                };
                page.selected_group = Some(group_id);
                page.roster = None;
                page.confirm = None;
                let clan_id = page.clan.id;
                match page.group(group_id).cloned() {
                    Some(group) => self.load_roster(clan_id, &group),
                    None => Task::none(),
                }
            }
            Message::GroupTabSelected(tab) => {
                if let Some(page) = self.open.as_mut() {
                    page.group_tab = tab;
                    page.confirm = None;
                }
                Task::none()
            }
            Message::RosterLoaded(clan_id, group_id, result) => {
                if let Some(page) = self.page_for(clan_id)
                    && page.selected_group == Some(group_id)
                {
                    page.roster = Some((group_id, result.map_err(|err| display_error(&err))));
                }
                Task::none()
            }
            Message::OpenNewGroup => {
                if let Some(page) = self.open.as_mut() {
                    let color = default_group_color(&page.clan.name, &page.groups);
                    page.modal = Some(Modal::Group {
                        group_id: None,
                        name: String::new(),
                        color: Some(color),
                        confirm_delete: false,
                        error: None,
                    });
                }
                Task::none()
            }
            Message::OpenEditGroup(group_id) => {
                if let Some(page) = self.open.as_mut()
                    && let Some(group) = page.group(group_id).cloned()
                {
                    page.modal = Some(Modal::Group {
                        group_id: Some(group_id),
                        name: group.name,
                        color: group.color,
                        confirm_delete: false,
                        error: None,
                    });
                }
                Task::none()
            }
            Message::GroupNameChanged(value) => {
                if let Some(Modal::Group { name, .. }) = self.modal_mut() {
                    *name = value;
                }
                Task::none()
            }
            Message::GroupColorPicked(value) => {
                if let Some(Modal::Group { color, .. }) = self.modal_mut() {
                    *color = value;
                }
                Task::none()
            }
            Message::SaveGroup => self.save_group(),
            Message::GroupCreated(clan_id, result) => {
                let Some(page) = self.page_for(clan_id) else {
                    return Task::none();
                };
                match result {
                    Ok(group) => {
                        page.modal = None;
                        page.selected_group = Some(group.id);
                        page.roster = None;
                        page.tab = Tab::Groups;
                        let roster = self.load_roster(clan_id, &group);
                        Task::batch([self.load_page(clan_id), roster])
                    }
                    Err(err) => {
                        if let Some(modal) = page.modal.as_mut() {
                            modal.set_error(display_error(&err));
                        }
                        Task::none()
                    }
                }
            }
            Message::DeleteGroupPressed => {
                if let Some(Modal::Group { confirm_delete, .. }) = self.modal_mut() {
                    *confirm_delete = true;
                }
                Task::none()
            }
            Message::DeleteGroupConfirmed(group_id) => {
                let Some(clan_id) = self.open_id() else {
                    return Task::none();
                };
                let client = self.cloud.client.clone();
                self.save_dialog(async move { client.delete_clan_group(clan_id, group_id).await })
            }
            Message::OpenAddGroupMember => {
                if let Some(page) = self.open.as_mut()
                    && let Some(group_id) = page.selected_group
                {
                    page.modal = Some(Modal::AddGroupMember {
                        group_id,
                        filter: String::new(),
                        error: None,
                    });
                }
                Task::none()
            }
            Message::AddMemberFilterChanged(value) => {
                if let Some(Modal::AddGroupMember { filter, .. }) = self.modal_mut() {
                    *filter = value;
                }
                Task::none()
            }
            Message::AddGroupMember(user_id) => {
                let Some(page) = self.open.as_ref() else {
                    return Task::none();
                };
                let Some(Modal::AddGroupMember { group_id, .. }) = page.modal else {
                    return Task::none();
                };
                let clan_id = page.clan.id;
                let client = self.cloud.client.clone();
                self.save_dialog(async move {
                    client
                        .add_clan_group_member(clan_id, group_id, user_id)
                        .await
                })
            }
            Message::RemoveGroupMember(user_id) => {
                let Some(page) = self.open.as_ref() else {
                    return Task::none();
                };
                let Some(group_id) = page.selected_group else {
                    return Task::none();
                };
                let clan_id = page.clan.id;
                let client = self.cloud.client.clone();
                self.change(async move {
                    client
                        .remove_clan_group_member(clan_id, group_id, user_id)
                        .await
                })
            }

            // ===== permissions =====
            Message::AddPermission => {
                let viewer = self.me();
                if let Some(page) = self.open.as_mut()
                    && let Some(group) = page.selected_group.and_then(|id| page.group(id))
                {
                    page.modal = Some(Modal::GroupPermissions(permissions::Permissions::new(
                        group, page, viewer,
                    )));
                }
                Task::none()
            }
            Message::GroupPermissions(message) => {
                if let Some(page) = self.open.as_mut() {
                    page.modal = match page.modal.take() {
                        Some(Modal::GroupPermissions(mut editor)) => {
                            editor.update(message, page);
                            Some(Modal::GroupPermissions(editor))
                        }
                        other => other,
                    };
                }
                Task::none()
            }
            Message::SaveGroupPermissions => {
                let Some(page) = self.open.as_mut() else {
                    return Task::none();
                };
                let Some(Modal::GroupPermissions(editor)) = page.modal.as_ref() else {
                    return Task::none();
                };
                if editor.saving {
                    return Task::none();
                }
                let writes = editor.writes();
                if writes.is_empty() {
                    return Task::none();
                }
                let group_id = editor.group;
                if let Some(Modal::GroupPermissions(editor)) = page.modal.as_mut() {
                    editor.saving = true;
                }
                let clan_id = page.clan.id;
                let client = self.cloud.client.clone();
                self.save_dialog(async move {
                    for write in writes {
                        match write {
                            permissions::Write::Create(scope, body) => {
                                client
                                    .grant_in_clan(
                                        clan_id,
                                        GrantRecipient::Group { group_id },
                                        &scope,
                                        &body,
                                    )
                                    .await?;
                            }
                            permissions::Write::Change(id, change) => {
                                client.change_clan_grant(clan_id, id, &change).await?;
                            }
                        }
                    }
                    Ok(())
                })
            }
            Message::EditGrant(grant_id) => {
                if let Some(page) = self.open.as_mut()
                    && let Some(grant) = page.grants.iter().find(|grant| grant.id == grant_id)
                {
                    let mut editor = GrantEditor::editing(grant);
                    editor.member_map = page.names_member_map(&grant.scope);
                    page.modal = Some(Modal::Grant(editor));
                }
                Task::none()
            }
            Message::RemoveGrantPressed(grant_id) => {
                self.set_confirm(Confirm::RemoveGrant(grant_id));
                Task::none()
            }
            Message::RemoveGrantConfirmed(grant_id) => {
                let Some(clan_id) = self.open_id() else {
                    return Task::none();
                };
                self.set_confirm_none();
                let client = self.cloud.client.clone();
                self.change(async move { client.delete_clan_grant(clan_id, grant_id).await })
            }
            Message::AssignGroup(scope, id) => {
                if let Some(page) = self.open.as_mut() {
                    let mut editor = GrantEditor::for_resource(scope, id);
                    editor.member_map = scope == ScopeKind::Maps
                        && page
                            .maps
                            .iter()
                            .any(|row| row.id == id && !row.clan_owned());
                    page.modal = Some(Modal::Grant(editor));
                }
                Task::none()
            }
            Message::Editor(message) => {
                let chooses = matches!(
                    message,
                    EditorMessage::RecipientPicked(_)
                        | EditorMessage::ScopePicked(_)
                        | EditorMessage::TargetToggled(..)
                );
                if let Some(page) = self.open.as_mut()
                    && let Some(Modal::Grant(editor)) = &mut page.modal
                {
                    editor.update(message);
                    // A recipient holds one grant over a scope: choosing
                    // one they hold opens it (clans.md §5.3).
                    if chooses && !(editor.recipient_fixed && editor.scope_fixed) {
                        let scope = editor.scope();
                        let existing = editor.recipient.and_then(|recipient| {
                            page.grants
                                .iter()
                                .find(|grant| grant.recipient == recipient && grant.scope == scope)
                        });
                        editor.load(existing);
                    }
                }
                Task::none()
            }
            Message::SaveGrant => self.save_grant(),

            // ===== access =====
            Message::AccessTabSelected(tab) => {
                if let Some(page) = self.open.as_mut() {
                    page.access_tab = tab;
                    page.selected_resource = None;
                    page.secret_grants = None;
                    page.confirm = None;
                }
                Task::none()
            }
            Message::SelectResource(id) => {
                let Some(page) = self.open.as_mut() else {
                    return Task::none();
                };
                page.selected_resource = Some(id);
                page.confirm = None;
                if page.access_tab == AccessTab::Secrets {
                    page.secret_grants = None;
                    return self.load_secret_grants(id);
                }
                Task::none()
            }
            Message::SecretGrantsLoaded(secret, result) => {
                if let Some(page) = self.open.as_mut()
                    && page.selected_resource == Some(secret)
                {
                    page.secret_grants = Some((secret, result.map_err(|err| display_error(&err))));
                }
                Task::none()
            }
            Message::AssignSecretGroup(secret) => {
                if let Some(page) = self.open.as_mut() {
                    let editor = SecretGrantEditor::new(secret).within(page.secret_actions(secret));
                    page.modal = Some(Modal::SecretGrant(editor));
                }
                Task::none()
            }
            Message::EditSecretGrant(secret, grant_id) => {
                if let Some(page) = self.open.as_mut()
                    && let Some((_, Ok(grants))) = &page.secret_grants
                    && let Some(grant) = grants.iter().find(|grant| grant.id == grant_id)
                {
                    let editor = SecretGrantEditor::editing(secret, grant)
                        .within(page.secret_actions(secret));
                    page.modal = Some(Modal::SecretGrant(editor));
                }
                Task::none()
            }
            Message::RemoveSecretGrant(secret, grant_id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move {
                        client
                            .revoke_clan_secret_grant(&SourceId::Secret(secret), grant_id)
                            .await
                    },
                    move |result| Message::SecretGrantChanged(secret, result),
                )
            }
            Message::SecretGrantChanged(secret, result) => {
                if let Err(err) = result {
                    match self.modal_mut() {
                        Some(modal) => modal.set_error(display_error(&err)),
                        None => self.absorb_error(&err),
                    }
                    return Task::none();
                }
                if let Some(page) = self.open.as_mut() {
                    page.modal = None;
                }
                self.load_secret_grants(secret)
            }
            Message::SecretEditor(message) => {
                let picked = matches!(message, SecretEditorMessage::RecipientPicked(_));
                if let Some(page) = self.open.as_mut() {
                    let mine: Vec<String> = match &page.modal {
                        Some(Modal::SecretGrant(editor)) => page
                            .secret_actions(editor.secret)
                            .into_iter()
                            .map(ToString::to_string)
                            .collect(),
                        _ => Vec::new(),
                    };
                    if let Some(Modal::SecretGrant(editor)) = &mut page.modal {
                        editor.update(message);
                        // One grant per recipient on a Secret: picking a
                        // group that holds one opens it (clans.md §8.4).
                        if picked {
                            let existing = match (&page.secret_grants, editor.recipient) {
                                (Some((secret, Ok(grants))), Some(recipient))
                                    if *secret == editor.secret =>
                                {
                                    grants.iter().find(|grant| grant.recipient == recipient)
                                }
                                _ => None,
                            };
                            editor.load(existing);
                            editor.bound_by(mine.iter().map(String::as_str));
                        }
                    }
                }
                Task::none()
            }
            Message::SaveSecretGrant => self.save_secret_grant(),
            Message::OpenMap(area) => {
                self.handoff = Some(Handoff::Map(area));
                Task::none()
            }
            Message::OpenMapAccess(area) => {
                self.handoff = Some(Handoff::MapAccess(area));
                Task::none()
            }
            Message::OpenPackage(name) => {
                self.handoff = Some(Handoff::Package(name));
                Task::none()
            }
            Message::RevokeOutsideShare(share) => {
                let client = self.cloud.client.clone();
                self.change(async move { client.revoke_share(share).await })
            }

            // ===== settings =====
            Message::ProfileNameChanged(value) => {
                if let Some(page) = self.open.as_mut() {
                    page.profile_name = value;
                }
                Task::none()
            }
            Message::ProfileDescriptionChanged(value) => {
                if let Some(page) = self.open.as_mut() {
                    page.profile_description = value;
                }
                Task::none()
            }
            Message::SaveProfile => self.save_profile(),
            Message::LeaveClanPressed => {
                self.set_confirm(Confirm::LeaveClan);
                Task::none()
            }
            Message::LeaveClanConfirmed => {
                let (Some(clan_id), Some(me)) = (self.open_id(), self.me()) else {
                    return Task::none();
                };
                self.set_confirm_none();
                self.clear_feedback();
                let client = self.cloud.client.clone();
                let copy = self.leave_copy && self.owns_member_maps();
                Task::perform(
                    async move {
                        if copy {
                            client.copy_my_clan_maps(clan_id).await?;
                        }
                        client.remove_clan_member(clan_id, me).await
                    },
                    move |result| Message::ClanGone(clan_id, result),
                )
            }
            Message::DeleteClanPressed => {
                self.set_confirm(Confirm::DeleteClan);
                Task::none()
            }
            Message::DeleteClanConfirmed => {
                let Some(clan_id) = self.open_id() else {
                    return Task::none();
                };
                self.set_confirm_none();
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.delete_clan(clan_id).await },
                    move |result| Message::ClanGone(clan_id, result),
                )
            }
            Message::ClanGone(clan_id, result) => match result {
                Ok(()) => {
                    if self.open_id() == Some(clan_id) {
                        self.open = None;
                    }
                    self.refresh()
                }
                Err(err) => {
                    self.absorb_error(&err);
                    self.refresh_page(clan_id)
                }
            },

            Message::CloseModal => {
                if let Some(page) = self.open.as_mut() {
                    if matches!(&page.modal, Some(Modal::GroupPermissions(editor)) if editor.saving)
                    {
                        return Task::none();
                    }
                    page.modal = None;
                }
                self.busy = false;
                Task::none()
            }
            Message::ConfirmCancelled => {
                self.set_confirm_none();
                match self.modal_mut() {
                    Some(Modal::MemberGroups { confirm_remove, .. }) => *confirm_remove = false,
                    Some(Modal::Group { confirm_delete, .. }) => *confirm_delete = false,
                    _ => {}
                }
                Task::none()
            }
        }
    }

    /// Whether the caller is an owner of a Member-owned map in the open clan,
    /// whose copy leaving offers.
    fn owns_member_maps(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|page| page.maps.iter().any(|row| row.owned_by_me))
    }

    fn open_clan(&mut self, clan_id: Uuid) -> Task<Message> {
        let Some(clan) = self
            .clans
            .as_ref()
            .and_then(|clans| clans.iter().find(|clan| clan.id == clan_id))
            .cloned()
        else {
            return Task::none();
        };
        self.clear_feedback();
        self.open = Some(ClanPage::new(clan));
        self.load_page(clan_id)
    }

    fn page_loaded(
        &mut self,
        clan_id: Uuid,
        result: Result<PageData, CloudError>,
    ) -> Task<Message> {
        if self.page_for(clan_id).is_none() {
            return Task::none();
        }
        match result {
            Ok(data) => {
                let Some(page) = self.page_for(clan_id) else {
                    return Task::none();
                };
                page.apply(data);
                let roster = page
                    .selected_group
                    .and_then(|id| page.group(id).cloned())
                    .filter(|_| page.tab == Tab::Groups);
                let secret = page
                    .selected_resource
                    .filter(|_| page.tab == Tab::Access && page.access_tab == AccessTab::Secrets)
                    .filter(|id| page.secrets.iter().any(|row| row.id == *id));
                Task::batch([
                    roster.map_or_else(Task::none, |group| self.load_roster(clan_id, &group)),
                    secret.map_or_else(Task::none, |id| self.load_secret_grants(id)),
                ])
            }
            // Gone, or no longer ours: back to the list.
            Err(CloudError::NotFoundOrNoAccess) => {
                self.open = None;
                self.refresh()
            }
            Err(err) => {
                if let Some(page) = self.page_for(clan_id) {
                    page.loading = false;
                }
                self.absorb_error(&err);
                Task::none()
            }
        }
    }

    fn send_invite(&mut self) -> Task<Message> {
        let Some(clan_id) = self.open_id() else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let Some(Modal::Invite {
            nickname, error, ..
        }) = self.modal_mut()
        else {
            return Task::none();
        };
        let nickname = nickname.trim().to_string();
        if nickname.is_empty() {
            *error = Some(crate::i18n::t!("social-enter-nickname"));
            return Task::none();
        }
        *error = None;
        self.busy = true;
        let client = self.cloud.client.clone();
        Task::perform(
            async move { client.lookup(&nickname).await },
            move |result| Message::InviteLookupFinished(clan_id, result),
        )
    }

    fn invite_lookup_finished(
        &mut self,
        clan_id: Uuid,
        result: Result<UserRef, CloudError>,
    ) -> Task<Message> {
        match result {
            Ok(user) => {
                let groups: Vec<Uuid> =
                    match self.page_for(clan_id).and_then(|page| page.modal.as_ref()) {
                        Some(Modal::Invite { groups, .. }) => groups.iter().copied().collect(),
                        _ => Vec::new(),
                    };
                let client = self.cloud.client.clone();
                let name = nickname_or_fallback(user.nickname.clone());
                Task::perform(
                    async move { client.invite_to_clan(clan_id, user.user_id, &groups).await },
                    move |result| Message::InviteFinished(clan_id, name.clone(), result),
                )
            }
            Err(err) => {
                self.busy = false;
                let message = match err {
                    CloudError::NotFoundOrNoAccess => crate::i18n::t!("social-no-nickname-match"),
                    other => display_error(&other),
                };
                if let Some(modal) = self.modal_mut() {
                    modal.set_error(message);
                }
                Task::none()
            }
        }
    }

    /// Edit groups' Save: each group whose box changed, and the owner box.
    fn save_member_groups(&mut self) -> Task<Message> {
        let Some(page) = self.open.as_ref() else {
            return Task::none();
        };
        let Some(Modal::MemberGroups {
            user_id,
            checked,
            owner,
            ..
        }) = &page.modal
        else {
            return Task::none();
        };
        let user_id = *user_id;
        let (before, was_owner) = page.member(user_id).map_or_else(
            || {
                (
                    page.clan.group_ids.iter().copied().collect(),
                    page.clan.is_owner,
                )
            },
            |member| {
                (
                    member.group_ids.iter().copied().collect::<BTreeSet<_>>(),
                    member.is_owner,
                )
            },
        );
        let custom: BTreeSet<Uuid> = page.custom_groups().map(|group| group.id).collect();
        let added: Vec<Uuid> = checked
            .difference(&before)
            .filter(|group| custom.contains(group))
            .copied()
            .collect();
        let removed: Vec<Uuid> = before
            .difference(checked)
            .filter(|group| custom.contains(group))
            .copied()
            .collect();
        let owner_change = (page.clan.is_owner && *owner != was_owner).then_some(*owner);
        let clan_id = page.clan.id;
        let client = self.cloud.client.clone();
        self.save_dialog(async move {
            for group in added {
                client
                    .add_clan_group_member(clan_id, group, user_id)
                    .await?;
            }
            for group in removed {
                client
                    .remove_clan_group_member(clan_id, group, user_id)
                    .await?;
            }
            if let Some(owner) = owner_change {
                client.set_clan_owner(clan_id, user_id, owner).await?;
            }
            Ok(())
        })
    }

    fn save_group(&mut self) -> Task<Message> {
        let Some(page) = self.open.as_mut() else {
            return Task::none();
        };
        let clan_id = page.clan.id;
        let current = match &page.modal {
            Some(Modal::Group {
                group_id: Some(id), ..
            }) => page.group(*id).cloned(),
            _ => None,
        };
        if current
            .as_ref()
            .is_some_and(|group| !group.can(action::RENAME_GROUP))
        {
            return Task::none();
        }
        let Some(Modal::Group {
            group_id,
            name,
            color,
            error,
            ..
        }) = page.modal.as_mut()
        else {
            return Task::none();
        };
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            *error = Some(crate::i18n::t!("clans-enter-name"));
            return Task::none();
        }
        *error = None;
        let client = self.cloud.client.clone();
        let color = color.clone();
        match *group_id {
            None => Task::perform(
                async move {
                    client
                        .create_clan_group(clan_id, &trimmed, color.as_deref())
                        .await
                },
                move |result| Message::GroupCreated(clan_id, result),
            ),
            Some(group_id) => {
                let patch = ClanGroupPatch {
                    name: current
                        .as_ref()
                        .filter(|group| !group.is_builtin() && group.name != trimmed)
                        .map(|_| trimmed),
                    color: current
                        .as_ref()
                        .filter(|group| group.color != color)
                        .map(|_| color),
                };
                self.save_dialog(async move {
                    client
                        .update_clan_group(clan_id, group_id, &patch)
                        .await
                        .map(|_| ())
                })
            }
        }
    }

    fn save_grant(&mut self) -> Task<Message> {
        let Some(page) = self.open.as_mut() else {
            return Task::none();
        };
        let clan_id = page.clan.id;
        let Some(Modal::Grant(editor)) = page.modal.as_mut() else {
            return Task::none();
        };
        let member = matches!(editor.recipient, Some(GrantRecipient::User { .. }));
        let write = match editor.writes(member) {
            Ok(Some(write)) => write,
            Ok(None) => {
                page.modal = None;
                return Task::none();
            }
            Err(reason) => {
                editor.error = Some(reason);
                return Task::none();
            }
        };
        let Some(recipient) = editor.recipient else {
            return Task::none();
        };
        let client = self.cloud.client.clone();
        // A change can leave the grant deleted, and joins move grants
        // between IDs (clans.md §5.3): the page reloads its grants after a
        // save instead of keeping the edited one.
        self.save_dialog(async move {
            match write {
                GrantWrite::Create(scope, body) => {
                    client
                        .grant_in_clan(clan_id, recipient, &scope, &body)
                        .await?;
                }
                GrantWrite::Change(id, change) => {
                    client.change_clan_grant(clan_id, id, &change).await?;
                }
            }
            Ok(())
        })
    }

    fn save_secret_grant(&mut self) -> Task<Message> {
        let Some(Modal::SecretGrant(editor)) = self.modal_mut() else {
            return Task::none();
        };
        let secret = editor.secret;
        let write = match editor.writes() {
            Ok(Some(write)) => write,
            Ok(None) => {
                if let Some(page) = self.open.as_mut() {
                    page.modal = None;
                }
                return Task::none();
            }
            Err(reason) => {
                editor.error = Some(reason);
                return Task::none();
            }
        };
        let client = self.cloud.client.clone();
        Task::perform(
            async move {
                let source = SourceId::Secret(secret);
                match write {
                    SecretWrite::Change(grant, change) => client
                        .update_clan_secret_grant(&source, grant, &change)
                        .await
                        .map(|_| ()),
                    SecretWrite::Create(recipient, actions) => client
                        .grant_clan_secret(&source, recipient, &actions)
                        .await
                        .map(|_| ()),
                }
            },
            move |result| Message::SecretGrantChanged(secret, result),
        )
    }

    fn save_profile(&mut self) -> Task<Message> {
        let Some(page) = self.open.as_ref() else {
            return Task::none();
        };
        let name = page.profile_name.trim().to_string();
        if name.is_empty() {
            self.error = Some(crate::i18n::t!("clans-enter-name"));
            return Task::none();
        }
        let description = page.profile_description.trim().to_string();
        let patch = ClanProfilePatch {
            name: (name != page.clan.name).then_some(name),
            description: (description != page.clan.description.clone().unwrap_or_default())
                .then_some(description),
        };
        if patch == ClanProfilePatch::default() {
            return Task::none();
        }
        let clan_id = page.clan.id;
        let client = self.cloud.client.clone();
        let task = self.change(async move {
            client
                .update_clan_profile(clan_id, &patch)
                .await
                .map(|_| ())
        });
        Task::batch([task, self.refresh_list()])
    }

    /// After creating `clan_id`: reload the list and its page.
    fn refresh_after(&mut self, clan_id: Uuid) -> Task<Message> {
        Task::batch([self.refresh_list(), self.load_page(clan_id)])
    }

    fn refresh_list(&self) -> Task<Message> {
        let account = self.account();
        let client = self.cloud.client.clone();
        Task::perform(async move { client.clans().await }, move |result| {
            Message::Loaded(account, result)
        })
    }

    fn refresh_page(&self, clan_id: Uuid) -> Task<Message> {
        if self.open_id() == Some(clan_id) {
            self.load_page(clan_id)
        } else {
            Task::none()
        }
    }

    fn set_confirm(&mut self, confirm: Confirm) {
        if let Some(page) = self.open.as_mut() {
            page.confirm = Some(confirm);
        }
    }

    fn set_confirm_none(&mut self) {
        if let Some(page) = self.open.as_mut() {
            page.confirm = None;
        }
    }
}

/// A part of the page the caller may lose the right to read between calls:
/// a refusal leaves it empty.
fn quiet<T>(result: Result<Vec<T>, CloudError>) -> Result<Vec<T>, CloudError> {
    match result {
        Err(CloudError::NotFoundOrNoAccess) => Ok(Vec::new()),
        other => other,
    }
}

// ===================== state logic =====================

/// The color a new group in `clan_name` takes: the first of the clan's color
/// sequence that none of its groups has.
pub(crate) fn default_group_color(clan_name: &str, groups: &[ClanGroup]) -> String {
    let used: Vec<Rgb> = groups
        .iter()
        .filter_map(|group| group.color.as_deref().and_then(parse_rgb))
        .collect();
    ColorSequence::seeded(&[clan_name])
        .first_unused(&used)
        .to_string()
}

/// The colors a group may take: the clan's color sequence.
pub(crate) fn group_palette(clan_name: &str) -> Vec<String> {
    let sequence = ColorSequence::seeded(&[clan_name]);
    (0..10)
        .map(|index| sequence.color(index).to_string())
        .collect()
}

/// `#rrggbb` as an [`Rgb`].
fn parse_rgb(color: &str) -> Option<Rgb> {
    let hex = color.strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some(Rgb {
        r: channel(0)?,
        g: channel(2)?,
        b: channel(4)?,
    })
}

fn owner_count(members: &[ClanMember]) -> usize {
    members.iter().filter(|member| member.is_owner).count()
}

/// Whether the caller is the clan's only owner (known once the directory
/// has loaded).
pub(crate) fn is_last_owner(clan: &ClanSummary, members: Option<&[ClanMember]>, me: Uuid) -> bool {
    clan.is_owner
        && members.is_some_and(|members| {
            owner_count(members) == 1
                && members
                    .iter()
                    .any(|member| member.user_id == me && member.is_owner)
        })
}

/// Whether the caller may remove `member`: `clan.remove_member`, ownership
/// to remove an owner, and never themselves (that is leaving).
fn can_remove(clan: &ClanSummary, member: &ClanMember, me: Uuid) -> bool {
    member.user_id != me && clan.can(action::REMOVE_MEMBER) && (!member.is_owner || clan.is_owner)
}

/// Whether the caller may revoke a pending invitation: anyone's with
/// `clan.revoke_invitation`, their own with `clan.invite`.
fn can_revoke_invitation(clan: &ClanSummary, invitation: &ClanInvitation, me: Uuid) -> bool {
    clan.can(action::REVOKE_INVITATION) || (invitation.inviter_id == me && clan.can(action::INVITE))
}

/// The members whose nickname contains `filter`, ignoring case.
fn filtered<'a>(members: &'a [ClanMember], filter: &str) -> impl Iterator<Item = &'a ClanMember> {
    let needle = filter.trim().to_lowercase();
    members.iter().filter(move |member| {
        needle.is_empty()
            || member
                .nickname
                .as_deref()
                .is_some_and(|nickname| nickname.to_lowercase().contains(&needle))
    })
}

fn nickname_or_fallback(nickname: Option<String>) -> String {
    nickname.unwrap_or_else(|| crate::i18n::t!("social-no-nickname"))
}

#[cfg(test)]
mod tests;
