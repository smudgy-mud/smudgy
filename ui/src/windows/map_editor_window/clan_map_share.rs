//! A clan map's Share dialog, and putting the viewer's own map in a clan.
//!
//! One dialog for every clan map. A Clan-owned map lists who reaches it and
//! why: the grants on its folder and on the whole clan, read only with a way
//! to change them for the folder, then the map's own grants, which those who
//! manage its access change one by one (a preset, plus the actions no preset
//! holds). Its "Outside" section shares it view only with friends outside the
//! clan, for holders of `area.share_external`. A Member-owned map has only
//! its own grants, written by its owners, and an Owners section: offering
//! ownership, giving it up, and offering the map to the clan. Clan owners
//! may offer a Clan-owned map to members ("Make member-owned…"). Anyone who
//! may not manage access sees what they can do.
//!
//! "Put in a clan…" on the viewer's own map offers it to one of their clans,
//! staying theirs (Member-owned) or given to the clan (Clan-owned), and names
//! the friend shares that end.

use std::collections::{BTreeSet, HashMap, HashSet};

use iced::Task;
use iced::alignment::Vertical;
use iced::widget::{
    Column, button, checkbox, column, container, pick_list, row, scrollable, space, text,
};
use iced::{Length, Padding};
use smudgy_cloud::clan_access::GrantBody;
use smudgy_cloud::clan_maps::{
    AreaOwnershipOffer, MAX_OFFER_RECIPIENTS, MapOwner, MapOwnership, OutsideShare,
};
use smudgy_cloud::clans::{
    ClanGrant, ClanGrantFilter, ClanGroup, ClanMember, GrantRecipient, GrantScope, action,
};
use smudgy_cloud::cloud_api::{FriendView, GrantTreeNode};
use smudgy_cloud::{AreaId, AtlasId, CloudError, MapStorage, SourceId, Uuid};

use crate::components::cloud_errors::display_error;
use crate::presets::{self, Kind, Preset};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::{MapEditorWindow, Message, modals};

fn muted(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

fn note<'a>(content: String) -> ThemedElement<'a, Message> {
    text(content).size(12).style(muted).into()
}

fn label<'a>(content: String) -> ThemedElement<'a, Message> {
    text(content).size(11).style(muted).into()
}

fn danger<'a>(content: String) -> ThemedElement<'a, Message> {
    text(content).size(12).style(builtins::text::danger).into()
}

// ===========================================================================
// Rows of who has access
// ===========================================================================

/// Where a row's access comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowSource {
    /// A grant naming this map alone: the map's own.
    Map,
    /// A grant naming this map beside others.
    SeveralMaps,
    /// A grant on the folder the map is filed in.
    Folder,
    /// A grant on the whole clan.
    Clan,
}

/// One grant reaching the map, as the dialog lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessRow {
    pub grant: ClanGrant,
    pub source: RowSource,
    /// The viewer may change or remove it here.
    pub editable: bool,
    /// What the grant gives on a map: its map and Secret actions.
    pub actions: BTreeSet<String>,
}

/// Whether `action` is something a grant gives on one map: its map actions
/// (not creating maps, which is a folder's) and its Clan-owned Secret
/// actions.
fn map_action(action: &str) -> bool {
    (action.starts_with("area.")
        && action != action::CREATE_AREA
        && action != action::CREATE_MEMBER_OWNED_AREA)
        || action.starts_with("secret.")
}

/// The rows of who has access to `map`, from the grants the viewer may see:
/// on a Member-owned map, only the grants naming it alone; on a Clan-owned
/// one, also those naming it beside other maps, those on its `folder`, and
/// those on the whole clan. The map's own grants come first and are
/// editable when the viewer `manages` its access; inherited rows never are.
#[must_use]
pub fn access_rows(
    grants: &[ClanGrant],
    map: AreaId,
    folder: Option<AtlasId>,
    member_owned: bool,
    manages: bool,
) -> Vec<AccessRow> {
    let mut rows: Vec<AccessRow> = grants
        .iter()
        .filter_map(|grant| {
            let source = match &grant.scope {
                GrantScope::Areas { ids } if ids.as_slice() == [map] => RowSource::Map,
                _ if member_owned => return None,
                GrantScope::Areas { ids } if ids.contains(&map) => RowSource::SeveralMaps,
                GrantScope::Atlases { ids } if folder.is_some_and(|f| ids.contains(&f)) => {
                    RowSource::Folder
                }
                GrantScope::Clan => RowSource::Clan,
                _ => return None,
            };
            let actions: BTreeSet<String> = grant
                .actions
                .iter()
                .filter(|action| map_action(action))
                .cloned()
                .collect();
            (!actions.is_empty()).then(|| AccessRow {
                grant: grant.clone(),
                source,
                editable: manages && source == RowSource::Map,
                actions,
            })
        })
        .collect();
    rows.sort_by_key(|row| match row.source {
        RowSource::Map => 0,
        RowSource::SeveralMaps => 1,
        RowSource::Folder => 2,
        RowSource::Clan => 3,
    });
    rows
}

/// The groups whose access to a Clan-owned map comes from its folder or the
/// clan: who stops reaching it when it becomes Member-owned.
#[must_use]
pub fn losing_folder_access(rows: &[AccessRow]) -> Vec<GrantRecipient> {
    let mut losing: Vec<GrantRecipient> = Vec::new();
    for row in rows {
        let inherited = matches!(
            row.source,
            RowSource::Folder | RowSource::Clan | RowSource::SeveralMaps
        );
        let keeps = rows.iter().any(|other| {
            other.source == RowSource::Map && other.grant.recipient == row.grant.recipient
        });
        if inherited && !keeps && !losing.contains(&row.grant.recipient) {
            losing.push(row.grant.recipient);
        }
    }
    losing
}

/// Who chooses a group's members, as its row says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chooser {
    /// A built-in group: everyone in the clan, or its owners.
    Automatic,
    /// Only the clan's owners (and the group's creator).
    OwnersOnly,
    /// The clan's owners and the group's leads.
    OwnersAndLeads,
}

/// Who chooses `group`'s members. A viewer who is not a clan owner may not
/// see who leads a group, so their dialog assumes leads may.
#[must_use]
pub fn chooser(group: &ClanGroup, all_grants: Option<&[ClanGrant]>) -> Chooser {
    if group.is_builtin() {
        return Chooser::Automatic;
    }
    let Some(grants) = all_grants else {
        return Chooser::OwnersAndLeads;
    };
    let led = grants.iter().any(|grant| {
        grant.actions.contains(action::ASSIGN_GROUP)
            && match &grant.scope {
                GrantScope::Groups { ids } => ids.contains(&group.id),
                GrantScope::Clan => true,
                _ => false,
            }
    });
    if led {
        Chooser::OwnersAndLeads
    } else {
        Chooser::OwnersOnly
    }
}

// ===========================================================================
// State
// ===========================================================================

/// A preset in a pick list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetChoice(pub Preset);

impl std::fmt::Display for PresetChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.label())
    }
}

/// A recipient in a pick list: a group or a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientChoice {
    pub recipient: GrantRecipient,
    pub label: String,
}

impl std::fmt::Display for RecipientChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

pub use super::clan_maps::FolderChoice;

/// A grant being written: a new one ("+ Group / member") or a change to one
/// of the map's own.
#[derive(Debug, Clone)]
pub struct GrantDraft {
    /// The grant changed; `None` for a new one.
    pub grant_id: Option<Uuid>,
    pub recipient: Option<RecipientChoice>,
    /// `None` keeps a Custom grant's core actions as they are.
    pub preset: Option<Preset>,
    /// The grant's core actions when no preset is picked.
    pub core: BTreeSet<String>,
    /// The actions no preset holds, checked.
    pub separate: BTreeSet<String>,
    /// What the grant carries that no Map preset or checkbox holds
    /// (renaming and refiling the map, its Clan-owned Secret actions,
    /// seeing and handing out access, actions this client does not know),
    /// kept as they are whatever preset is picked.
    pub kept: BTreeSet<String>,
    /// What a grant that hands out access may hand out, kept as it is.
    pub may_grant: Option<BTreeSet<String>>,
    pub error: Option<String>,
}

impl GrantDraft {
    /// The actions the grant carries once saved.
    #[must_use]
    pub fn actions(&self) -> BTreeSet<String> {
        let mut actions: BTreeSet<String> = match self.preset {
            Some(preset) => preset.action_set(),
            None => self.core.clone(),
        };
        actions.extend(self.separate.iter().cloned());
        actions.extend(self.kept.iter().cloned());
        actions
    }

    /// What the grant hands out once saved: what it may hand out now, while
    /// it keeps handing out access.
    #[must_use]
    pub fn may_grant(&self) -> Option<Vec<String>> {
        self.kept
            .contains(action::MANAGE_GRANTS)
            .then(|| self.may_grant.iter().flatten().cloned().collect())
    }
}

/// An ownership step waiting for a confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerStep {
    /// Offering ownership to the picked members; `replace` makes them its
    /// only owners.
    Offer {
        picked: BTreeSet<Uuid>,
        replace: bool,
    },
    /// Giving up the viewer's own ownership.
    GiveUp,
    /// Offering the map to the clan, accepted at once into `folder` when
    /// the viewer may accept it.
    ToClan { folder: Option<FolderChoice> },
    /// A clan owner offering a Clan-owned map to members.
    ToMembers { picked: BTreeSet<Uuid> },
}

/// What the dialog shows and changes for one clan map.
#[derive(Debug, Clone)]
pub struct ClanMapShareDialog {
    session: ShareSession,
    pending: HashMap<RequestKind, Uuid>,
    pub area_id: AreaId,
    pub area_name: String,
    pub clan_id: Uuid,
    pub clan_name: String,
    pub folder: Option<(AtlasId, String)>,
    pub ownership: MapOwnership,
    pub frozen: bool,
    pub owned_by_me: bool,
    pub viewer_id: Option<Uuid>,
    pub clan_owner: bool,
    /// The viewer's actions on the map.
    pub held: BTreeSet<String>,
    /// Clan-owned folders the viewer may accept a map into.
    pub accept_folders: Vec<FolderChoice>,
    /// The clan's Secrets on the map whose access the viewer manages.
    pub secrets: Vec<(SourceId, String)>,
    pub groups: Option<Result<Vec<ClanGroup>, String>>,
    /// `None` when the viewer may not read the member list.
    pub members: Option<Result<Vec<ClanMember>, String>>,
    pub grants: Option<Result<Vec<ClanGrant>, String>>,
    /// Every grant the viewer sees, for who chooses a group's members; only
    /// for a clan owner, who sees them all.
    pub all_grants: Option<Vec<ClanGrant>>,
    pub owners: Option<Result<Vec<MapOwner>, String>>,
    pub offers: Option<Result<Vec<AreaOwnershipOffer>, String>>,
    pub outside: Option<Result<Vec<OutsideShare>, String>>,
    pub friends: Option<Result<Vec<FriendView>, String>>,
    pub draft: Option<GrantDraft>,
    pub removing: Option<Uuid>,
    pub outside_pick: Option<RecipientChoice>,
    pub step: Option<OwnerStep>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

impl ClanMapShareDialog {
    fn busy(&self) -> bool {
        self.pending.keys().any(|kind| kind.is_write())
    }

    fn accept(&mut self, request: ShareRequest, response: &ClanMapShareResponse) -> bool {
        let kind = response.kind();
        if request.dialog != self.session.id || self.pending.get(&kind) != Some(&request.id) {
            return false;
        }
        self.pending.remove(&kind);
        true
    }

    fn member_owned(&self) -> bool {
        self.ownership == MapOwnership::Members
    }

    /// Whether the viewer manages the map's access: on a Member-owned map,
    /// its active owners while it is not frozen; on a Clan-owned map, the
    /// clan's owners and holders of `grant.manage` on it.
    #[must_use]
    pub fn manages(&self) -> bool {
        if self.member_owned() {
            self.owned_by_me && !self.frozen
        } else {
            self.clan_owner || self.held.contains(action::MANAGE_GRANTS)
        }
    }

    /// Whether the viewer shares the map outside the clan: only a Clan-owned
    /// map, with `area.share_external` (which clan owners hold).
    #[must_use]
    pub fn shares_outside(&self) -> bool {
        !self.member_owned()
            && (self.clan_owner || self.held.contains(action::SHARE_AREA_EXTERNALLY))
    }

    fn rows(&self) -> Vec<AccessRow> {
        let mut rows = match &self.grants {
            Some(Ok(grants)) => access_rows(
                grants,
                self.area_id,
                self.folder.as_ref().map(|(id, _)| *id),
                self.member_owned(),
                self.manages(),
            ),
            _ => Vec::new(),
        };
        for row in &mut rows {
            row.editable = row.editable && self.may_change(&row.grant);
        }
        rows
    }

    /// Whether `recipient` is the viewer or one of the viewer's groups.
    fn is_mine(&self, recipient: GrantRecipient) -> bool {
        match recipient {
            GrantRecipient::User { user_id } => Some(user_id) == self.viewer_id,
            GrantRecipient::Group { group_id } => {
                self.group(group_id).is_some_and(|group| group.is_member)
            }
        }
    }

    /// Whether a grant's scope covers the map.
    fn covers_map(&self, scope: &GrantScope) -> bool {
        match scope {
            GrantScope::Clan => true,
            GrantScope::Atlases { ids } => self
                .folder
                .as_ref()
                .is_some_and(|(folder, _)| ids.contains(folder)),
            GrantScope::Areas { ids } => ids.contains(&self.area_id),
            GrantScope::Groups { .. } | GrantScope::Packages { .. } => false,
        }
    }

    /// What the viewer may hand out on the map: `None` for no bound beyond
    /// the map's (a clan owner, or the owners of a Member-owned map);
    /// otherwise, for each of the viewer's delegations covering the map,
    /// what it may hand out.
    #[must_use]
    pub fn ceilings(&self) -> Option<Vec<BTreeSet<String>>> {
        if self.clan_owner || self.member_owned() {
            return None;
        }
        let Some(Ok(grants)) = &self.grants else {
            return Some(Vec::new());
        };
        Some(
            grants
                .iter()
                .filter(|grant| {
                    grant.parent_id.is_none()
                        && grant.actions.contains(action::MANAGE_GRANTS)
                        && self.is_mine(grant.recipient)
                        && self.covers_map(&grant.scope)
                })
                .map(|grant| grant.may_grant.clone().unwrap_or_default())
                .collect(),
        )
    }

    /// Whether the viewer may give a grant on the map `actions`: a delegate
    /// only actions a delegation hands out, within one of their
    /// delegations covering the map.
    #[must_use]
    pub fn may_hand_out(&self, actions: &BTreeSet<String>) -> bool {
        self.ceilings().is_none_or(|ceilings| {
            actions.iter().all(|action| presets::delegable(action))
                && ceilings.iter().any(|ceiling| actions.is_subset(ceiling))
        })
    }

    /// Whether the viewer may change or remove `grant`, one of the map's
    /// own: a delegate only one that hands nothing out and lies within one
    /// of their delegations.
    fn may_change(&self, grant: &ClanGrant) -> bool {
        self.ceilings().is_none()
            || (grant.may_grant.is_none()
                && !grant.actions.contains(action::MANAGE_GRANTS)
                && self.may_hand_out(&grant.actions))
    }

    /// The presets a grant here may take beside what it `kept`.
    #[must_use]
    pub fn preset_choices(&self, kept: &BTreeSet<String>) -> Vec<Preset> {
        Kind::Map
            .presets()
            .iter()
            .copied()
            .filter(|preset| {
                let mut actions = preset.action_set();
                actions.extend(kept.iter().cloned());
                self.may_hand_out(&actions)
            })
            .collect()
    }

    fn group(&self, id: Uuid) -> Option<&ClanGroup> {
        match &self.groups {
            Some(Ok(groups)) => groups.iter().find(|group| group.id == id),
            _ => None,
        }
    }

    fn member_name(&self, user: Uuid) -> String {
        if Some(user) == self.viewer_id {
            return crate::i18n::t!("clan-share-you");
        }
        let from_members = match &self.members {
            Some(Ok(members)) => members
                .iter()
                .find(|member| member.user_id == user)
                .and_then(|member| member.nickname.clone()),
            _ => None,
        };
        let from_owners = || match &self.owners {
            Some(Ok(owners)) => owners
                .iter()
                .find(|owner| owner.user_id == user)
                .and_then(|owner| owner.nickname.clone()),
            _ => None,
        };
        from_members
            .or_else(from_owners)
            .unwrap_or_else(|| crate::i18n::t!("clan-maps-a-member"))
    }

    fn group_label(group: &ClanGroup) -> String {
        match group.builtin.as_deref() {
            Some("members") => crate::i18n::t!("clan-maps-everyone"),
            Some("owners") => crate::i18n::t!("clan-share-clan-owners"),
            _ => group.name.clone(),
        }
    }

    fn recipient_label(&self, recipient: GrantRecipient) -> String {
        match recipient {
            GrantRecipient::Group { group_id } => self
                .group(group_id)
                .map_or_else(|| crate::i18n::t!("clan-maps-a-group"), Self::group_label),
            GrantRecipient::User { user_id } => self.member_name(user_id),
        }
    }

    /// Who "+ Group / member" may pick: the clan's groups (Everyone first,
    /// then the custom groups) and, when the member list is readable, its
    /// members; none that already holds the map's own grant.
    #[must_use]
    pub fn recipient_choices(&self) -> Vec<RecipientChoice> {
        let own: Vec<GrantRecipient> = self
            .rows()
            .into_iter()
            .filter(|row| row.source == RowSource::Map)
            .map(|row| row.grant.recipient)
            .collect();
        let mut choices = Vec::new();
        if let Some(Ok(groups)) = &self.groups {
            let mut ordered: Vec<&ClanGroup> = groups
                .iter()
                .filter(|group| group.builtin.as_deref() != Some("owners"))
                .collect();
            ordered.sort_by_key(|group| (!group.is_builtin(), group.name.to_lowercase()));
            for group in ordered {
                choices.push(RecipientChoice {
                    recipient: GrantRecipient::Group { group_id: group.id },
                    label: Self::group_label(group),
                });
            }
        }
        if let Some(Ok(members)) = &self.members {
            for member in members {
                if Some(member.user_id) == self.viewer_id {
                    continue;
                }
                choices.push(RecipientChoice {
                    recipient: GrantRecipient::User {
                        user_id: member.user_id,
                    },
                    label: member
                        .nickname
                        .clone()
                        .unwrap_or_else(|| crate::i18n::t!("clan-maps-a-member")),
                });
            }
        }
        choices.retain(|choice| !own.contains(&choice.recipient));
        choices
    }

    /// The actions no preset holds that a grant here may carry: on a
    /// Member-owned map, those its own grants take; for a delegate, those
    /// a delegation of theirs hands out.
    #[must_use]
    pub fn separate_actions(&self) -> Vec<&'static str> {
        Kind::Map
            .separate()
            .iter()
            .copied()
            .filter(|action| !self.member_owned() || presets::takes_on_member_map(action))
            .filter(|action| self.may_hand_out(&BTreeSet::from([(*action).to_string()])))
            .collect()
    }

    /// Whether `member` reads the map, as far as the grants and groups the
    /// viewer sees tell: on a Clan-owned map, the clan's owners and the
    /// recipients of a grant reaching it with `area.read`; on a
    /// Member-owned map, its owners and the recipients of its own grants.
    fn reads(&self, member: &ClanMember) -> bool {
        if self.member_owned() {
            if let Some(Ok(owners)) = &self.owners
                && owners.iter().any(|owner| owner.user_id == member.user_id)
            {
                return true;
            }
        } else if member.is_owner {
            return true;
        }
        self.rows().iter().any(|row| {
            row.actions.contains(action::READ_AREA)
                && match row.grant.recipient {
                    GrantRecipient::User { user_id } => {
                        user_id == member.user_id && row.source == RowSource::Map
                    }
                    GrantRecipient::Group { group_id } => {
                        match self
                            .group(group_id)
                            .and_then(|group| group.builtin.as_deref())
                        {
                            Some("members") => true,
                            Some("owners") => member.is_owner,
                            _ => member.group_ids.contains(&group_id),
                        }
                    }
                }
        })
    }

    /// The draft that changes `grant_id`, one of the map's own grants the
    /// viewer may change: what the dialog shows of it (the Map preset's
    /// actions and the checkboxes), and everything else it carries (renaming
    /// and refiling, Clan-owned Secret actions, seeing and handing out
    /// access, actions this client does not know, and what the grant may
    /// hand out) kept as it is.
    #[must_use]
    pub fn edit_draft(&self, grant_id: Uuid) -> Option<GrantDraft> {
        let row = self
            .rows()
            .into_iter()
            .find(|row| row.grant.id == grant_id && row.editable)?;
        let separate_ok = self.separate_actions();
        let separate: BTreeSet<String> = row
            .actions
            .iter()
            .filter(|action| separate_ok.contains(&action.as_str()))
            .cloned()
            .collect();
        let core: BTreeSet<String> = row
            .actions
            .iter()
            .filter(|action| Kind::Map.core().contains(&action.as_str()))
            .cloned()
            .collect();
        let kept: BTreeSet<String> = row
            .grant
            .actions
            .iter()
            .filter(|action| !core.contains(*action) && !separate.contains(*action))
            .cloned()
            .collect();
        Some(GrantDraft {
            grant_id: Some(grant_id),
            recipient: Some(RecipientChoice {
                recipient: row.grant.recipient,
                label: self.recipient_label(row.grant.recipient),
            }),
            preset: presets::core_preset(Kind::Map, core.iter().map(String::as_str))
                .filter(|preset| preset.action_set() == core),
            core,
            separate,
            kept,
            may_grant: row.grant.may_grant.clone(),
            error: None,
        })
    }

    /// The members who could own the map with the viewer: the clan's
    /// members who read it, but its current owners and the viewer.
    #[must_use]
    pub fn ownership_candidates(&self) -> Vec<(Uuid, String)> {
        let owners: Vec<Uuid> = match &self.owners {
            Some(Ok(owners)) => owners.iter().map(|owner| owner.user_id).collect(),
            _ => Vec::new(),
        };
        match &self.members {
            Some(Ok(members)) => members
                .iter()
                .filter(|member| {
                    !owners.contains(&member.user_id)
                        && (Some(member.user_id) != self.viewer_id || !self.member_owned())
                        && self.reads(member)
                })
                .map(|member| (member.user_id, self.member_name(member.user_id)))
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ClanMapShareMessage {
    Response {
        request: ShareRequest,
        response: ClanMapShareResponse,
    },
    AddStarted,
    EditStarted(Uuid),
    DraftRecipient(RecipientChoice),
    DraftPreset(PresetChoice),
    DraftSeparate(&'static str, bool),
    DraftCancelled,
    DraftSaved,
    RemoveRequested(Uuid),
    RemoveCancelled,
    RemoveConfirmed,
    OutsidePicked(RecipientChoice),
    OutsideShare,
    OutsideRevoke(Uuid),
    ChangeForFolder,
    ShareSecret(SourceId),
    Step(Option<OwnerStep>),
    StepToggle(Uuid, bool),
    StepReplace(bool),
    StepFolder(FolderChoice),
    StepConfirmed,
    OfferWithdrawn(Uuid),
}

/// An opening and one request within it; reopening the same map is distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareRequest {
    dialog: Uuid,
    id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RequestKind {
    Groups,
    Members,
    Grants,
    AllGrants,
    Owners,
    Offers,
    Outside,
    Friends,
    Draft,
    Remove,
    OutsideWrite,
    Ownership,
    Withdraw,
}

impl RequestKind {
    fn is_write(self) -> bool {
        matches!(
            self,
            Self::Draft | Self::Remove | Self::OutsideWrite | Self::Ownership | Self::Withdraw
        )
    }
}

#[derive(Debug, Clone)]
pub enum ClanMapShareResponse {
    GroupsLoaded(Result<Vec<ClanGroup>, CloudError>),
    MembersLoaded(Result<Vec<ClanMember>, CloudError>),
    GrantsLoaded(Result<Vec<ClanGrant>, CloudError>),
    AllGrantsLoaded(Result<Vec<ClanGrant>, CloudError>),
    OwnersLoaded(Result<Vec<MapOwner>, CloudError>),
    OffersLoaded(Result<Vec<AreaOwnershipOffer>, CloudError>),
    OutsideLoaded(Result<Vec<OutsideShare>, CloudError>),
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    DraftResult(Result<(), CloudError>),
    RemoveResult(Result<(), CloudError>),
    OutsideResult(Result<(), CloudError>),
    StepResult(Result<Option<String>, CloudError>),
    WithdrawResult(Result<(), CloudError>),
}

impl ClanMapShareResponse {
    fn kind(&self) -> RequestKind {
        match self {
            Self::GroupsLoaded(_) => RequestKind::Groups,
            Self::MembersLoaded(_) => RequestKind::Members,
            Self::GrantsLoaded(_) => RequestKind::Grants,
            Self::AllGrantsLoaded(_) => RequestKind::AllGrants,
            Self::OwnersLoaded(_) => RequestKind::Owners,
            Self::OffersLoaded(_) => RequestKind::Offers,
            Self::OutsideLoaded(_) => RequestKind::Outside,
            Self::FriendsLoaded(_) => RequestKind::Friends,
            Self::DraftResult(_) => RequestKind::Draft,
            Self::RemoveResult(_) => RequestKind::Remove,
            Self::OutsideResult(_) => RequestKind::OutsideWrite,
            Self::StepResult(_) => RequestKind::Ownership,
            Self::WithdrawResult(_) => RequestKind::Withdraw,
        }
    }
}

/// Keeps multi-request ownership operations on one principal across awaits.
#[derive(Debug, Clone)]
struct ShareSession {
    id: Uuid,
    credential_generation: u64,
    auth_projection_revision: u64,
    client: smudgy_cloud::CloudApiClient,
}

impl ShareSession {
    fn new(window: &MapEditorWindow) -> Self {
        let (credential_generation, credentials) = window.cloud.client.credentials().freeze();
        Self {
            id: Uuid::new_v4(),
            credential_generation,
            auth_projection_revision: window.mapper.auth_projection_revision(),
            client: smudgy_cloud::CloudApiClient::new(window.cloud.client.base_url(), credentials),
        }
    }

    fn is_current(&self, window: &MapEditorWindow) -> bool {
        self.credential_generation == window.cloud.client.credentials().generation()
            && self.auth_projection_revision == window.mapper.auth_projection_revision()
    }
}

/// Discard account-specific names and permissions without waiting for a reply.
pub(super) fn dismiss_stale(window: &mut MapEditorWindow) {
    let session = match &window.modal {
        Some(modals::Modal::ClanMapShare(dialog)) => &dialog.session,
        Some(modals::Modal::PutInClan(dialog)) => &dialog.session,
        _ => return,
    };
    if !session.is_current(window) {
        window.modal = None;
    }
}

fn msg(message: ClanMapShareMessage) -> Message {
    Message::ClanMapShare(Box::new(message))
}

// ===========================================================================
// Opening and loading
// ===========================================================================

/// Whether the viewer may put `area_id` in one of their clans: a personal
/// cloud map they own, while they are in a clan.
pub(super) fn may_put_in_clan(window: &MapEditorWindow, area_id: AreaId) -> bool {
    window
        .atlases
        .iter()
        .any(|atlas| atlas.clan_id.is_some() && atlas.can(action::ACCEPT_TRANSFER))
        && window.mapper.area_storage(&area_id) == MapStorage::Cloud
        && window
            .mapper
            .get_current_atlas()
            .get_area(&area_id)
            .is_some_and(|area| area.is_owned() && area.meta().clan_id.is_none())
}

/// Whether `area_id` is a clan's map, whose Share… opens this dialog.
pub(super) fn is_clan_map(window: &MapEditorWindow, area_id: AreaId) -> bool {
    window
        .mapper
        .get_current_atlas()
        .get_area(&area_id)
        .is_some_and(|area| area.meta().clan_id.is_some())
}

/// Opens the Share dialog of the clan map `area_id`. This is also the entry
/// point a shortcut from elsewhere (Settings › Access) opens it through.
pub(super) fn open(window: &mut MapEditorWindow, area_id: AreaId) -> Update<Message, super::Event> {
    let atlas = window.mapper.get_current_atlas();
    let Some(area) = atlas.get_area(&area_id) else {
        return Update::none();
    };
    let meta = area.meta();
    let Some(clan_id) = meta.clan_id else {
        return Update::none();
    };
    let summary = window.clans.clans.iter().find(|clan| clan.id == clan_id);
    let clan_owner = summary.is_some_and(|clan| clan.is_owner);
    let reads_members = summary.is_some_and(|clan| clan.can(action::READ_MEMBERS) || clan.is_owner);
    let ownership = meta.clan_ownership.ownership.unwrap_or(MapOwnership::Clan);
    let folder = meta.atlas_id.map(|atlas_id| {
        let name = window
            .atlases
            .iter()
            .find(|folder| folder.id == atlas_id)
            .map(|folder| folder.name.clone())
            .or_else(|| meta.atlas_name.clone())
            .unwrap_or_else(|| crate::i18n::t!("mapper-this-folder"));
        (atlas_id, name)
    });
    let mut accept_folders: Vec<FolderChoice> = window
        .atlases
        .iter()
        .filter(|folder| {
            folder.clan_id == Some(clan_id) && (clan_owner || folder.can(action::ACCEPT_TRANSFER))
        })
        .map(|folder| FolderChoice {
            id: folder.id,
            name: folder.name.clone(),
        })
        .collect();
    accept_folders.sort_by_key(|folder| folder.name.to_lowercase());
    // A Secret is shared from the open map's dialog, so only the open map
    // lists its Secrets here.
    let open_map = window.editor.area_id() == Some(area_id);
    let secrets = super::secrets::secrets(&area)
        .into_iter()
        .filter(|bundle| {
            open_map
                && bundle.clan_id == Some(clan_id)
                && bundle
                    .actions
                    .contains(smudgy_cloud::cloud_api::secret_action::MANAGE_ACCESS)
        })
        .map(|bundle| (bundle.source, bundle.name.clone().unwrap_or_default()))
        .collect();
    let mut dialog = ClanMapShareDialog {
        session: ShareSession::new(window),
        pending: HashMap::new(),
        area_id,
        area_name: area.get_name().to_string(),
        clan_id,
        clan_name: window.clans.name(clan_id).unwrap_or_default(),
        folder,
        ownership,
        frozen: meta.clan_ownership.frozen,
        owned_by_me: meta.clan_ownership.owned_by_me,
        viewer_id: window
            .cloud
            .snapshot
            .get()
            .profile
            .as_ref()
            .map(|profile| profile.id),
        clan_owner,
        held: meta.actions.clone().unwrap_or_default(),
        accept_folders,
        secrets,
        groups: None,
        members: None,
        grants: None,
        all_grants: None,
        owners: None,
        offers: None,
        outside: None,
        friends: None,
        draft: None,
        removing: None,
        outside_pick: None,
        step: None,
        error: None,
        notice: None,
    };
    let member_owned = dialog.member_owned();
    let shares_outside = dialog.shares_outside();
    let authority = if member_owned {
        dialog.owned_by_me
    } else {
        clan_owner
    };
    let client = dialog.session.client.clone();
    let mut tasks = vec![
        perform(
            &mut dialog,
            RequestKind::Groups,
            {
                let client = client.clone();
                async move { client.clan_groups(clan_id).await }
            },
            ClanMapShareResponse::GroupsLoaded,
        ),
        fetch_grants(&mut dialog),
    ];
    if reads_members {
        let client = client.clone();
        tasks.push(perform(
            &mut dialog,
            RequestKind::Members,
            async move { client.clan_members(clan_id).await },
            ClanMapShareResponse::MembersLoaded,
        ));
    }
    if member_owned {
        let client = client.clone();
        tasks.push(perform(
            &mut dialog,
            RequestKind::Owners,
            async move { client.area_owners(area_id).await },
            ClanMapShareResponse::OwnersLoaded,
        ));
    }
    if authority {
        tasks.push(fetch_offers(&mut dialog));
    }
    if shares_outside {
        tasks.push(fetch_outside(&mut dialog));
        let client = client.clone();
        tasks.push(perform(
            &mut dialog,
            RequestKind::Friends,
            async move { client.friends().await },
            ClanMapShareResponse::FriendsLoaded,
        ));
    }
    window.modal = Some(modals::Modal::ClanMapShare(Box::new(dialog)));
    Update::with_task(Task::batch(tasks))
}

fn perform<T: Send + 'static>(
    dialog: &mut ClanMapShareDialog,
    kind: RequestKind,
    future: impl std::future::Future<Output = T> + Send + 'static,
    to: impl Fn(T) -> ClanMapShareResponse + Send + 'static,
) -> Task<Message> {
    let request = ShareRequest {
        dialog: dialog.session.id,
        id: Uuid::new_v4(),
    };
    dialog.pending.insert(kind, request.id);
    // Reads predating a write must not restore the old list afterward.
    match kind {
        RequestKind::Draft | RequestKind::Remove => {
            dialog.pending.remove(&RequestKind::Grants);
            dialog.pending.remove(&RequestKind::AllGrants);
        }
        RequestKind::OutsideWrite => {
            dialog.pending.remove(&RequestKind::Outside);
        }
        RequestKind::Ownership | RequestKind::Withdraw => {
            dialog.pending.remove(&RequestKind::Offers);
        }
        _ => {}
    }
    Task::perform(future, move |result| {
        msg(ClanMapShareMessage::Response {
            request,
            response: to(result),
        })
    })
}

fn fetch_grants(dialog: &mut ClanMapShareDialog) -> Task<Message> {
    dialog.grants = None;
    dialog.all_grants = None;
    let client = dialog.session.client.clone();
    let (clan_id, area_id) = (dialog.clan_id, dialog.area_id);
    let grants = perform(
        dialog,
        RequestKind::Grants,
        async move {
            client
                .clan_grants(
                    clan_id,
                    ClanGrantFilter {
                        area_id: Some(area_id),
                        ..ClanGrantFilter::default()
                    },
                )
                .await
        },
        ClanMapShareResponse::GrantsLoaded,
    );
    if !dialog.clan_owner {
        return grants;
    }
    let client = dialog.session.client.clone();
    let all = perform(
        dialog,
        RequestKind::AllGrants,
        async move {
            client
                .clan_grants(clan_id, ClanGrantFilter::default())
                .await
        },
        ClanMapShareResponse::AllGrantsLoaded,
    );
    Task::batch([grants, all])
}

fn fetch_offers(dialog: &mut ClanMapShareDialog) -> Task<Message> {
    dialog.offers = None;
    let client = dialog.session.client.clone();
    let area_id = dialog.area_id;
    perform(
        dialog,
        RequestKind::Offers,
        async move { client.area_ownership_offers(area_id).await },
        ClanMapShareResponse::OffersLoaded,
    )
}

fn fetch_outside(dialog: &mut ClanMapShareDialog) -> Task<Message> {
    dialog.outside = None;
    let client = dialog.session.client.clone();
    let area_id = dialog.area_id;
    perform(
        dialog,
        RequestKind::Outside,
        async move { client.outside_shares(area_id).await },
        ClanMapShareResponse::OutsideLoaded,
    )
}

fn dialog_mut(window: &mut MapEditorWindow) -> Option<&mut ClanMapShareDialog> {
    match &mut window.modal {
        Some(modals::Modal::ClanMapShare(dialog)) => Some(dialog),
        _ => None,
    }
}

fn loaded<T>(result: Result<T, CloudError>) -> Result<T, String> {
    result.map_err(|error| display_error(&error))
}

fn refused(error: CloudError, refusal: String) -> String {
    match error {
        CloudError::NotFoundOrNoAccess => refusal,
        CloudError::LastOwner => crate::i18n::t!("clan-share-needs-another-owner"),
        other => display_error(&other),
    }
}

// ===========================================================================
// Update
// ===========================================================================

#[allow(clippy::too_many_lines)]
pub(super) fn update(
    window: &mut MapEditorWindow,
    message: ClanMapShareMessage,
) -> Update<Message, super::Event> {
    dismiss_stale(window);
    if let ClanMapShareMessage::Response { request, response } = message {
        let Some(dialog) = dialog_mut(window) else {
            return Update::none();
        };
        if !dialog.accept(request, &response) {
            return Update::none();
        }
        return apply_response(window, response);
    }
    let Some(dialog) = dialog_mut(window) else {
        return Update::none();
    };
    if dialog.busy() {
        return Update::none();
    }
    let client = dialog.session.client.clone();
    // Steps that leave this dialog for another.
    match &message {
        ClanMapShareMessage::ChangeForFolder => {
            let Some(dialog) = dialog_mut(window) else {
                return Update::none();
            };
            let (clan_id, folder) = (dialog.clan_id, dialog.folder.as_ref().map(|(id, _)| *id));
            return super::clan_share::open(window, clan_id, folder);
        }
        ClanMapShareMessage::ShareSecret(source) => {
            let source = *source;
            return modals::open_share_dialog_on(window, source);
        }
        _ => {}
    }
    let Some(dialog) = dialog_mut(window) else {
        return Update::none();
    };
    let (clan_id, area_id) = (dialog.clan_id, dialog.area_id);
    match message {
        ClanMapShareMessage::AddStarted => {
            dialog.draft = Some(GrantDraft {
                grant_id: None,
                recipient: None,
                preset: dialog.preset_choices(&BTreeSet::new()).first().copied(),
                core: BTreeSet::new(),
                separate: BTreeSet::new(),
                kept: BTreeSet::new(),
                may_grant: None,
                error: None,
            });
            dialog.removing = None;
        }
        ClanMapShareMessage::EditStarted(grant_id) => {
            let Some(draft) = dialog.edit_draft(grant_id) else {
                return Update::none();
            };
            dialog.draft = Some(draft);
            dialog.removing = None;
        }
        ClanMapShareMessage::DraftRecipient(choice) => {
            if let Some(draft) = &mut dialog.draft {
                draft.recipient = Some(choice);
            }
        }
        ClanMapShareMessage::DraftPreset(PresetChoice(preset)) => {
            if let Some(draft) = &mut dialog.draft {
                draft.preset = Some(preset);
            }
        }
        ClanMapShareMessage::DraftSeparate(action, on) => {
            if let Some(draft) = &mut dialog.draft {
                if on {
                    draft.separate.insert(action.to_string());
                } else {
                    draft.separate.remove(action);
                }
            }
        }
        ClanMapShareMessage::DraftCancelled => dialog.draft = None,
        ClanMapShareMessage::DraftSaved => {
            let Some(draft) = &mut dialog.draft else {
                return Update::none();
            };
            let Some(recipient) = draft.recipient.clone() else {
                return Update::none();
            };
            draft.error = None;
            let actions: Vec<String> = draft.actions().into_iter().collect();
            let may_grant = draft.may_grant();
            // The answer to a change can name another grant than the one
            // edited (clans.md §1.2); the result refetches the grants.
            let grant_id = draft.grant_id;
            return Update::with_task(perform(
                dialog,
                RequestKind::Draft,
                async move {
                    match grant_id {
                        Some(grant_id) => client
                            .change_clan_grant(clan_id, grant_id, &GrantBody { actions, may_grant })
                            .await
                            .map(|_| ()),
                        None => client
                            .create_clan_grant(
                                clan_id,
                                recipient.recipient,
                                &GrantScope::Areas { ids: vec![area_id] },
                                &actions.iter().map(String::as_str).collect::<Vec<_>>(),
                            )
                            .await
                            .map(|_| ()),
                    }
                },
                ClanMapShareResponse::DraftResult,
            ));
        }

        ClanMapShareMessage::RemoveRequested(grant_id) => {
            dialog.removing = Some(grant_id);
            dialog.draft = None;
        }
        ClanMapShareMessage::RemoveCancelled => dialog.removing = None,
        ClanMapShareMessage::RemoveConfirmed => {
            let Some(grant_id) = dialog.removing else {
                return Update::none();
            };
            return Update::with_task(perform(
                dialog,
                RequestKind::Remove,
                async move { client.delete_clan_grant(clan_id, grant_id).await },
                ClanMapShareResponse::RemoveResult,
            ));
        }

        ClanMapShareMessage::OutsidePicked(choice) => dialog.outside_pick = Some(choice),
        ClanMapShareMessage::OutsideShare => {
            let Some(choice) = dialog.outside_pick.clone() else {
                return Update::none();
            };
            let GrantRecipient::User { user_id } = choice.recipient else {
                return Update::none();
            };
            dialog.error = None;
            return Update::with_task(perform(
                dialog,
                RequestKind::OutsideWrite,
                async move { client.share_outside(area_id, user_id).await.map(|_| ()) },
                ClanMapShareResponse::OutsideResult,
            ));
        }
        ClanMapShareMessage::OutsideRevoke(share_id) => {
            return Update::with_task(perform(
                dialog,
                RequestKind::OutsideWrite,
                async move { client.revoke_share(share_id).await },
                ClanMapShareResponse::OutsideResult,
            ));
        }

        ClanMapShareMessage::Step(step) => {
            dialog.step = step;
            dialog.error = None;
            dialog.notice = None;
        }
        ClanMapShareMessage::StepToggle(user, on) => {
            if let Some(OwnerStep::Offer { picked, .. } | OwnerStep::ToMembers { picked }) =
                &mut dialog.step
            {
                toggle_recipient(picked, user, on);
            }
        }
        ClanMapShareMessage::StepReplace(value) => {
            if let Some(OwnerStep::Offer { replace, .. }) = &mut dialog.step {
                *replace = value;
            }
        }
        ClanMapShareMessage::StepFolder(choice) => {
            if let Some(OwnerStep::ToClan { folder }) = &mut dialog.step {
                *folder = Some(choice);
            }
        }
        ClanMapShareMessage::StepConfirmed => {
            let Some(step) = dialog.step.clone() else {
                return Update::none();
            };
            dialog.error = None;
            let viewer = dialog.viewer_id;
            let accepts_self = viewer.is_some_and(|viewer| match &step {
                OwnerStep::ToMembers { picked } => picked.len() == 1 && picked.contains(&viewer),
                _ => false,
            });
            return Update::with_task(perform(
                dialog,
                RequestKind::Ownership,
                async move {
                    match step {
                        OwnerStep::Offer { picked, replace } => {
                            let users: Vec<Uuid> = picked.into_iter().collect();
                            client
                                .offer_area_ownership(
                                    area_id,
                                    &users,
                                    MapOwnership::Members,
                                    replace,
                                )
                                .await
                                .map(|_| Some(crate::i18n::t!("clan-share-offer-sent")))
                        }
                        OwnerStep::GiveUp => {
                            let viewer = viewer.unwrap_or_default();
                            client
                                .remove_area_owner(area_id, viewer)
                                .await
                                .map(|()| None)
                        }
                        OwnerStep::ToClan { folder } => {
                            let offer = client
                                .offer_area_ownership(area_id, &[], MapOwnership::Clan, false)
                                .await?;
                            match folder {
                                Some(folder) => client
                                    .accept_area_offer(area_id, offer.id, Some(folder.id))
                                    .await
                                    .map(|_| None),
                                None => Ok(Some(crate::i18n::t!("clan-share-offered-to-clan"))),
                            }
                        }
                        OwnerStep::ToMembers { picked } => {
                            let users: Vec<Uuid> = picked.into_iter().collect();
                            let offer = client
                                .offer_area_ownership(area_id, &users, MapOwnership::Members, false)
                                .await?;
                            if accepts_self {
                                client
                                    .accept_area_offer(area_id, offer.id, None)
                                    .await
                                    .map(|_| None)
                            } else {
                                Ok(Some(crate::i18n::t!("clan-share-offer-sent")))
                            }
                        }
                    }
                },
                ClanMapShareResponse::StepResult,
            ));
        }

        ClanMapShareMessage::OfferWithdrawn(offer_id) => {
            return Update::with_task(perform(
                dialog,
                RequestKind::Withdraw,
                async move { client.withdraw_area_offer(area_id, offer_id).await },
                ClanMapShareResponse::WithdrawResult,
            ));
        }
        ClanMapShareMessage::ChangeForFolder
        | ClanMapShareMessage::ShareSecret(_)
        | ClanMapShareMessage::Response { .. } => {}
    }
    Update::none()
}

#[allow(clippy::too_many_lines)]
fn apply_response(
    window: &mut MapEditorWindow,
    response: ClanMapShareResponse,
) -> Update<Message, super::Event> {
    let Some(dialog) = dialog_mut(window) else {
        return Update::none();
    };
    match response {
        ClanMapShareResponse::GroupsLoaded(result) => dialog.groups = Some(loaded(result)),
        ClanMapShareResponse::MembersLoaded(result) => dialog.members = Some(loaded(result)),
        ClanMapShareResponse::GrantsLoaded(result) => dialog.grants = Some(loaded(result)),
        ClanMapShareResponse::AllGrantsLoaded(result) => dialog.all_grants = result.ok(),
        ClanMapShareResponse::OwnersLoaded(result) => dialog.owners = Some(loaded(result)),
        ClanMapShareResponse::OffersLoaded(result) => dialog.offers = Some(loaded(result)),
        ClanMapShareResponse::OutsideLoaded(result) => dialog.outside = Some(loaded(result)),
        ClanMapShareResponse::FriendsLoaded(result) => dialog.friends = Some(loaded(result)),
        ClanMapShareResponse::DraftResult(result) => {
            match result {
                Ok(()) => dialog.draft = None,
                Err(error) => {
                    if let Some(draft) = &mut dialog.draft {
                        let name = draft
                            .recipient
                            .as_ref()
                            .map(|choice| choice.label.clone())
                            .unwrap_or_default();
                        draft.error = Some(match error {
                            CloudError::NotFoundOrNoAccess => crate::i18n::t!(
                                "mapper-share-failed",
                                "recipient" => name
                            ),
                            other => display_error(&other),
                        });
                    }
                }
            }
            let task = fetch_grants(dialog);
            window.mapper.sync_now();
            return Update::with_task(task);
        }
        ClanMapShareResponse::RemoveResult(result) => {
            dialog.removing = None;
            if let Err(error) = result {
                dialog.error = Some(refused(error, crate::i18n::t!("mapper-could-not-revoke")));
            }
            let task = fetch_grants(dialog);
            window.mapper.sync_now();
            return Update::with_task(task);
        }
        ClanMapShareResponse::OutsideResult(result) => {
            match result {
                Ok(()) => dialog.outside_pick = None,
                Err(error) => {
                    dialog.error = Some(refused(
                        error,
                        crate::i18n::t!("clan-share-outside-refused"),
                    ));
                }
            }
            return Update::with_task(fetch_outside(dialog));
        }
        ClanMapShareResponse::StepResult(result) => {
            match result {
                Ok(notice) => {
                    let changed_ownership = notice.is_none();
                    dialog.step = None;
                    dialog.notice = notice;
                    if changed_ownership {
                        window.mapper.sync_now();
                        // Its ownership changed: the dialog closes, and the
                        // map's next sync says what the viewer holds now.
                        window.modal = None;
                        return Update::with_task(super::clan_maps::fetch(window));
                    }
                    let task = fetch_offers(dialog);
                    window.mapper.sync_now();
                    return Update::with_task(task);
                }
                Err(error) => {
                    dialog.error = Some(refused(
                        error,
                        crate::i18n::t!("clan-share-ownership-refused"),
                    ));
                }
            }
        }
        ClanMapShareResponse::WithdrawResult(result) => {
            if let Err(error) = result {
                dialog.error = Some(refused(
                    error,
                    crate::i18n::t!("clan-share-ownership-refused"),
                ));
            }
            let task = fetch_offers(dialog);
            window.mapper.sync_now();
            return Update::with_task(task);
        }
    }
    Update::none()
}

// ===========================================================================
// View
// ===========================================================================

/// The dialog's title and body.
#[must_use]
pub fn view(dialog: &ClanMapShareDialog) -> (String, ThemedElement<'_, Message>) {
    let title = crate::i18n::t!("clan-share-title", "name" => &dialog.area_name);
    let mut body: Column<'_, Message, crate::Theme> = Column::new().spacing(12);
    body = body.push(header(dialog));
    if dialog.busy() {
        body = body.push(note(crate::i18n::t!("mapper-saving")));
    }
    if let Some(error) = &dialog.error {
        body = body.push(danger(error.clone()));
    }
    if let Some(notice) = dialog.notice.as_ref().filter(|notice| !notice.is_empty()) {
        body = body.push(text(notice.clone()).size(12).style(builtins::text::success));
    }
    if dialog.manages() || (dialog.member_owned() && dialog.owned_by_me) {
        body = body.push(access_section(dialog));
    } else {
        // Holders of grant.inspect see who reaches the map, read only.
        body = body.push(you_can(dialog));
        if dialog.held.contains(action::INSPECT_GRANTS) {
            body = body.push(access_section(dialog));
        }
    }
    if dialog.shares_outside() {
        body = body.push(iced::widget::rule::horizontal(1));
        body = body.push(outside_section(dialog));
    }
    if dialog.member_owned() && dialog.owned_by_me {
        body = body.push(iced::widget::rule::horizontal(1));
        body = body.push(owners_section(dialog));
    } else if !dialog.member_owned() && dialog.clan_owner {
        body = body.push(iced::widget::rule::horizontal(1));
        body = body.push(make_member_owned_section(dialog));
    }
    if !dialog.secrets.is_empty() {
        let mut secrets = column![label(crate::i18n::t!("clan-share-secrets"))].spacing(4);
        for (source, name) in &dialog.secrets {
            secrets = secrets.push(
                button(text(crate::i18n::t!("clan-share-share-secret", "name" => name)).size(12))
                    .style(builtins::button::link)
                    .on_press_maybe(
                        (!dialog.busy()).then_some(msg(ClanMapShareMessage::ShareSecret(*source))),
                    ),
            );
        }
        body = body.push(secrets);
    }
    let close = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-close")).size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
    ];
    (title, modals::scrolling_body(body, close))
}

/// iced pick lists have no disabled mode; show the submitted choice while saving.
fn share_picker<'a, T: Clone + ToString + PartialEq + 'a>(
    options: Vec<T>,
    selected: Option<T>,
    busy: bool,
    placeholder: String,
    width: u32,
    on_select: impl Fn(T) -> Message + 'a,
) -> ThemedElement<'a, Message> {
    if busy {
        return text(selected.as_ref().map_or(placeholder, ToString::to_string))
            .size(12)
            .width(width)
            .style(muted)
            .into();
    }
    pick_list(options, selected, on_select)
        .placeholder(placeholder)
        .text_size(12.0)
        .width(width)
        .into()
}

fn header(dialog: &ClanMapShareDialog) -> ThemedElement<'_, Message> {
    let line = if dialog.member_owned() {
        let names: Vec<String> = match &dialog.owners {
            Some(Ok(owners)) => owners
                .iter()
                .map(|owner| {
                    let name = dialog.member_name(owner.user_id);
                    if owner.active {
                        name
                    } else {
                        crate::i18n::t!("mapper-owner-inactive", "name" => name)
                    }
                })
                .collect(),
            _ => Vec::new(),
        };
        if names.is_empty() {
            crate::i18n::t!("clan-share-member-owned")
        } else {
            crate::i18n::t!(
                "clan-share-owned-by",
                "owners" => names.join(&crate::i18n::t!("mapper-multi-list-separator"))
            )
        }
    } else {
        match &dialog.folder {
            Some((_, folder)) => crate::i18n::t!(
                "clan-share-owned-by-clan-in",
                "clan" => &dialog.clan_name,
                "folder" => folder
            ),
            None => crate::i18n::t!("clan-share-owned-by-clan", "clan" => &dialog.clan_name),
        }
    };
    let mut col = column![text(line).size(13)].spacing(4);
    if dialog.member_owned() {
        col = col.push(note(crate::i18n::t!("clan-share-member-owned-reach")));
        if dialog.frozen {
            col = col.push(note(crate::i18n::t!("clan-share-frozen")));
        }
    }
    col.into()
}

/// "You can: …" for a viewer who may not manage the map's access.
fn you_can(dialog: &ClanMapShareDialog) -> ThemedElement<'_, Message> {
    let chips: Vec<String> = presets::chips(
        dialog
            .held
            .iter()
            .filter(|action| map_action(action))
            .map(String::as_str),
    )
    .into_iter()
    .map(presets::Chip::label)
    .collect();
    let line = if chips.is_empty() {
        crate::i18n::t!("clan-share-you-can-read")
    } else {
        crate::i18n::t!(
            "clan-share-you-can",
            "actions" => chips.join(&crate::i18n::t!("mapper-multi-list-separator"))
        )
    };
    text(line).size(13).into()
}

fn source_label(dialog: &ClanMapShareDialog, row: &AccessRow) -> Option<String> {
    match row.source {
        RowSource::Map => None,
        RowSource::SeveralMaps => Some(crate::i18n::t!("clan-share-from-several-maps")),
        RowSource::Folder => Some(crate::i18n::t!(
            "clan-share-from-folder",
            "folder" => dialog
                .folder
                .as_ref()
                .map(|(_, name)| name.clone())
                .unwrap_or_default()
        )),
        RowSource::Clan => Some(crate::i18n::t!(
            "clan-share-from-clan",
            "clan" => &dialog.clan_name
        )),
    }
}

fn chooser_label(chooser: Chooser) -> String {
    match chooser {
        Chooser::Automatic => crate::i18n::t!("clan-share-chosen-automatically"),
        Chooser::OwnersOnly => crate::i18n::t!("clan-share-chosen-by-owners"),
        Chooser::OwnersAndLeads => crate::i18n::t!("clan-share-chosen-by-owners-and-leads"),
    }
}

#[allow(clippy::too_many_lines)]
fn access_section(dialog: &ClanMapShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text(crate::i18n::t!("mapper-who-has-access")).size(13)].spacing(6);
    let rows = match &dialog.grants {
        None => {
            return section.push(note(crate::i18n::t!("mapper-loading"))).into();
        }
        Some(Err(error)) => return section.push(danger(error.clone())).into(),
        Some(Ok(_)) => dialog.rows(),
    };
    if rows.is_empty() {
        section = section.push(note(crate::i18n::t!("clan-share-nobody-yet")));
    }
    let manages_folder = dialog.clan_owner || dialog.held.contains(action::MANAGE_GRANTS);
    for row_state in rows {
        let grant_id = row_state.grant.id;
        let label = dialog.recipient_label(row_state.grant.recipient);
        let what = presets::chips(row_state.actions.iter().map(String::as_str))
            .into_iter()
            .map(presets::Chip::label)
            .collect::<Vec<_>>()
            .join(" \u{00b7} ");
        let mut line = row![text(label).size(13), text(what).size(11).style(muted)]
            .spacing(8)
            .align_y(Vertical::Center);
        line = line.push(space::horizontal());
        if let Some(from) = source_label(dialog, &row_state) {
            line = line.push(text(from).size(11).style(muted));
            if row_state.source == RowSource::Folder && manages_folder {
                line = line.push(
                    button(text(crate::i18n::t!("clan-share-change-for-folder")).size(11))
                        .style(builtins::button::link)
                        .on_press_maybe(
                            (!dialog.busy()).then_some(msg(ClanMapShareMessage::ChangeForFolder)),
                        ),
                );
            }
        } else if row_state.editable {
            line = line
                .push(
                    button(text(crate::i18n::t!("mapper-edit-flags")).size(11))
                        .style(builtins::button::secondary)
                        .on_press_maybe(
                            (!dialog.busy())
                                .then_some(msg(ClanMapShareMessage::EditStarted(grant_id))),
                        ),
                )
                .push(
                    button(text(crate::i18n::t!("clan-share-remove")).size(11))
                        .style(builtins::button::secondary)
                        .on_press_maybe(
                            (!dialog.busy())
                                .then_some(msg(ClanMapShareMessage::RemoveRequested(grant_id))),
                        ),
                );
        }
        let mut item = column![line].spacing(2);
        if let GrantRecipient::Group { group_id } = row_state.grant.recipient
            && let Some(group) = dialog.group(group_id)
        {
            item = item.push(
                text(format!(
                    "{} \u{00b7} {}",
                    crate::i18n::t!("clan-share-includes-future-members"),
                    chooser_label(chooser(group, dialog.all_grants.as_deref()))
                ))
                .size(10)
                .style(muted),
            );
        }
        section = section.push(item);
        if dialog
            .draft
            .as_ref()
            .is_some_and(|draft| draft.grant_id == Some(grant_id))
        {
            section = section.push(draft_view(dialog));
        }
        if dialog.removing == Some(grant_id) {
            section = section.push(
                row![
                    text(crate::i18n::t!("clan-share-remove-question")).size(12),
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(11))
                        .style(builtins::button::secondary)
                        .on_press_maybe(
                            (!dialog.busy()).then_some(msg(ClanMapShareMessage::RemoveCancelled))
                        ),
                    button(text(crate::i18n::t!("clan-share-remove")).size(11))
                        .style(builtins::button::primary)
                        .on_press_maybe(
                            (!dialog.busy()).then_some(msg(ClanMapShareMessage::RemoveConfirmed))
                        ),
                ]
                .spacing(8)
                .align_y(Vertical::Center),
            );
        }
    }
    if dialog.manages() {
        match &dialog.draft {
            Some(draft) if draft.grant_id.is_none() => section = section.push(draft_view(dialog)),
            _ => {
                section = section.push(
                    button(text(crate::i18n::t!("clan-share-add-recipient")).size(12))
                        .style(builtins::button::secondary)
                        .on_press_maybe(
                            (!dialog.busy()).then_some(msg(ClanMapShareMessage::AddStarted)),
                        ),
                );
            }
        }
        if dialog.members.is_none() && !dialog.member_owned() {
            section = section.push(note(crate::i18n::t!("clan-share-groups-only")));
        }
    }
    section.into()
}

/// The form writing a grant: recipient (for a new one), preset, and the
/// actions no preset holds.
fn draft_view(dialog: &ClanMapShareDialog) -> ThemedElement<'_, Message> {
    let Some(draft) = &dialog.draft else {
        return column![].into();
    };
    let presets: Vec<PresetChoice> = dialog
        .preset_choices(&draft.kept)
        .into_iter()
        .map(PresetChoice)
        .collect();
    let mut first = row![].spacing(8).align_y(Vertical::Center);
    if draft.grant_id.is_none() {
        first = first.push(share_picker(
            dialog.recipient_choices(),
            draft.recipient.clone(),
            dialog.busy(),
            crate::i18n::t!("clan-share-pick-recipient"),
            200,
            |choice| msg(ClanMapShareMessage::DraftRecipient(choice)),
        ));
    }
    first = first.push(share_picker(
        presets,
        draft.preset.map(PresetChoice),
        dialog.busy(),
        presets::custom_label(),
        150,
        |choice| msg(ClanMapShareMessage::DraftPreset(choice)),
    ));
    let mut separate = Column::new().spacing(2);
    for action in dialog.separate_actions() {
        separate = separate.push(
            checkbox(draft.separate.contains(action))
                .label(presets::action_label(action))
                .size(13)
                .text_size(12)
                .on_toggle_maybe(
                    (!dialog.busy())
                        .then_some(move |on| msg(ClanMapShareMessage::DraftSeparate(action, on))),
                ),
        );
    }
    let actions = draft.actions();
    let ready = draft.recipient.is_some()
        && !dialog.busy()
        && !actions.is_empty()
        && dialog.may_hand_out(&actions);
    let mut block = column![
        first,
        separate,
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::secondary)
                .on_press_maybe(
                    (!dialog.busy()).then_some(msg(ClanMapShareMessage::DraftCancelled))
                ),
            button(text(crate::i18n::t!("action-save")).size(11))
                .style(builtins::button::primary)
                .on_press_maybe(ready.then_some(msg(ClanMapShareMessage::DraftSaved))),
        ]
        .spacing(8),
    ]
    .spacing(6)
    .padding(Padding {
        top: 2.0,
        bottom: 4.0,
        left: 12.0,
        right: 0.0,
    });
    if let Some(error) = &draft.error {
        block = block.push(danger(error.clone()));
    }
    block.into()
}

/// "Outside <clan>": friends the map is shared with outside the clan, view
/// only, each with who made the share.
fn outside_section(dialog: &ClanMapShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![
        text(crate::i18n::t!("clan-share-outside", "clan" => &dialog.clan_name)).size(13),
        note(crate::i18n::t!("clan-share-outside-help")),
    ]
    .spacing(6);
    match &dialog.outside {
        None => section = section.push(note(crate::i18n::t!("mapper-loading"))),
        Some(Err(error)) => section = section.push(danger(error.clone())),
        Some(Ok(shares)) => {
            for share in shares {
                let friend = share
                    .grantee_nickname
                    .clone()
                    .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));
                let by = if Some(share.grantor_id) == dialog.viewer_id {
                    crate::i18n::t!("clan-share-shared-by-you")
                } else {
                    crate::i18n::t!(
                        "clan-share-shared-by",
                        "name" => share
                            .grantor_nickname
                            .clone()
                            .unwrap_or_else(|| crate::i18n::t!("clan-maps-a-member"))
                    )
                };
                let mut line = row![
                    text(friend).size(13),
                    text(by).size(11).style(muted),
                    space::horizontal()
                ]
                .spacing(8)
                .align_y(Vertical::Center);
                if dialog.clan_owner || Some(share.grantor_id) == dialog.viewer_id {
                    line = line.push(
                        button(text(crate::i18n::t!("mapper-revoke")).size(11))
                            .style(builtins::button::secondary)
                            .on_press_maybe(
                                (!dialog.busy())
                                    .then_some(msg(ClanMapShareMessage::OutsideRevoke(share.id))),
                            ),
                    );
                }
                section = section.push(line);
            }
        }
    }
    let shared: HashSet<Uuid> = match &dialog.outside {
        Some(Ok(shares)) => shares.iter().map(|share| share.grantee_id).collect(),
        _ => HashSet::new(),
    };
    let members: HashSet<Uuid> = match &dialog.members {
        Some(Ok(members)) => members.iter().map(|member| member.user_id).collect(),
        _ => HashSet::new(),
    };
    if let Some(Ok(friends)) = &dialog.friends {
        let choices: Vec<RecipientChoice> = friends
            .iter()
            .filter(|friend| {
                !shared.contains(&friend.user_id) && !members.contains(&friend.user_id)
            })
            .map(|friend| RecipientChoice {
                recipient: GrantRecipient::User {
                    user_id: friend.user_id,
                },
                label: friend
                    .nickname
                    .clone()
                    .unwrap_or_else(|| friend.user_id.to_string()),
            })
            .collect();
        if !choices.is_empty() {
            section = section.push(
                row![
                    share_picker(
                        choices,
                        dialog.outside_pick.clone(),
                        dialog.busy(),
                        crate::i18n::t!("clan-share-pick-friend"),
                        200,
                        |choice| { msg(ClanMapShareMessage::OutsidePicked(choice)) }
                    ),
                    button(text(crate::i18n::t!("clan-share-share-view-only")).size(12))
                        .style(builtins::button::primary)
                        .on_press_maybe(
                            (dialog.outside_pick.is_some() && !dialog.busy())
                                .then_some(msg(ClanMapShareMessage::OutsideShare))
                        ),
                ]
                .spacing(8)
                .align_y(Vertical::Center),
            );
        }
    }
    section.into()
}

/// Picks or drops one recipient of an ownership offer; an offer names at
/// most [`MAX_OFFER_RECIPIENTS`] users, so a pick past that is ignored.
pub(super) fn toggle_recipient(picked: &mut BTreeSet<Uuid>, user: Uuid, on: bool) {
    if !on {
        picked.remove(&user);
    } else if picked.len() < MAX_OFFER_RECIPIENTS {
        picked.insert(user);
    }
}

/// A checklist of members for an ownership offer.
fn member_checklist<'a>(
    dialog: &'a ClanMapShareDialog,
    picked: &'a BTreeSet<Uuid>,
) -> ThemedElement<'a, Message> {
    let mut list = Column::new().spacing(2);
    let candidates = dialog.ownership_candidates();
    if candidates.is_empty() {
        return note(crate::i18n::t!("clan-share-no-candidates"));
    }
    let full = picked.len() >= MAX_OFFER_RECIPIENTS;
    for (user, name) in candidates {
        let checked = picked.contains(&user);
        list = list.push(
            checkbox(checked)
                .label(name)
                .size(13)
                .text_size(12)
                .on_toggle_maybe(
                    ((checked || !full) && !dialog.busy())
                        .then_some(move |on| msg(ClanMapShareMessage::StepToggle(user, on))),
                ),
        );
    }
    let list = container(scrollable(list)).max_height(140.0);
    if !full {
        return list.into();
    }
    Column::new()
        .spacing(4)
        .push(list)
        .push(note(crate::i18n::t!(
            "mapper-offer-recipient-limit",
            "count" => MAX_OFFER_RECIPIENTS
        )))
        .into()
}

fn confirm_row<'a>(label: String, ready: bool, busy: bool) -> ThemedElement<'a, Message> {
    row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-cancel")).size(11))
            .style(builtins::button::secondary)
            .on_press_maybe((!busy).then_some(msg(ClanMapShareMessage::Step(None)))),
        button(text(label).size(11))
            .style(builtins::button::primary)
            .on_press_maybe((ready && !busy).then_some(msg(ClanMapShareMessage::StepConfirmed))),
    ]
    .spacing(8)
    .into()
}

fn pending_offers(dialog: &ClanMapShareDialog) -> Column<'_, Message, crate::Theme> {
    let mut list = Column::new().spacing(4);
    let Some(Ok(offers)) = &dialog.offers else {
        return list;
    };
    for offer in offers {
        let who = if offer.to_clan() {
            crate::i18n::t!("clan-share-offer-to-clan", "clan" => &dialog.clan_name)
        } else {
            let names: Vec<String> = offer
                .recipients
                .iter()
                .map(|recipient| {
                    let name = recipient
                        .nickname
                        .clone()
                        .unwrap_or_else(|| dialog.member_name(recipient.user_id));
                    if recipient.accepted {
                        crate::i18n::t!("clan-share-accepted", "name" => name)
                    } else {
                        crate::i18n::t!("clan-share-waiting-for", "name" => name)
                    }
                })
                .collect();
            names.join(&crate::i18n::t!("mapper-multi-list-separator"))
        };
        list = list.push(
            row![
                text(who).size(12),
                space::horizontal(),
                button(text(crate::i18n::t!("clan-share-cancel-offer")).size(11))
                    .style(builtins::button::secondary)
                    .on_press_maybe(
                        (!dialog.busy())
                            .then_some(msg(ClanMapShareMessage::OfferWithdrawn(offer.id)))
                    ),
            ]
            .spacing(8)
            .align_y(Vertical::Center),
        );
    }
    list
}

/// A Member-owned map's owners: Offer ownership…, Give up, Make
/// clan-owned….
fn owners_section(dialog: &ClanMapShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![text(crate::i18n::t!("mapper-owners")).size(13)].spacing(6);
    if dialog.frozen {
        return section.into();
    }
    section = section.push(pending_offers(dialog));
    match &dialog.step {
        None => {
            let only_owner = matches!(&dialog.owners, Some(Ok(owners)) if owners.len() <= 1);
            let mut actions = row![
                button(text(crate::i18n::t!("mapper-offer-ownership")).size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe((!dialog.busy()).then_some(msg(ClanMapShareMessage::Step(
                        Some(OwnerStep::Offer {
                            picked: BTreeSet::new(),
                            replace: false,
                        })
                    )))),
            ]
            .spacing(8);
            actions = actions.push(
                button(text(crate::i18n::t!("clan-share-give-up")).size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe(
                        (!only_owner && !dialog.busy())
                            .then_some(msg(ClanMapShareMessage::Step(Some(OwnerStep::GiveUp)))),
                    ),
            );
            actions = actions.push(
                button(text(crate::i18n::t!("clan-share-make-clan-owned")).size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe(
                        (!dialog.busy()).then_some(msg(ClanMapShareMessage::Step(Some(
                            OwnerStep::ToClan {
                                folder: (dialog.accept_folders.len() == 1)
                                    .then(|| dialog.accept_folders[0].clone()),
                            },
                        )))),
                    ),
            );
            section = section.push(actions);
            if only_owner {
                section = section.push(note(crate::i18n::t!("clan-share-only-owner")));
            }
        }
        Some(OwnerStep::Offer { picked, replace }) => {
            section = section
                .push(note(crate::i18n::t!("clan-share-offer-help")))
                .push(member_checklist(dialog, picked))
                .push(
                    checkbox(*replace)
                        .label(crate::i18n::t!("clan-share-offer-replace"))
                        .size(13)
                        .text_size(12)
                        .on_toggle_maybe(
                            (!dialog.busy())
                                .then_some(|on| msg(ClanMapShareMessage::StepReplace(on))),
                        ),
                )
                .push(confirm_row(
                    crate::i18n::t!("clan-share-send-offer"),
                    !picked.is_empty(),
                    dialog.busy(),
                ));
        }
        Some(OwnerStep::GiveUp) => {
            section = section
                .push(
                    text(
                        crate::i18n::t!("clan-share-give-up-confirm", "name" => &dialog.area_name),
                    )
                    .size(12),
                )
                .push(confirm_row(
                    crate::i18n::t!("clan-share-give-up"),
                    true,
                    dialog.busy(),
                ));
        }
        Some(OwnerStep::ToClan { folder }) => {
            section = section.push(
                text(crate::i18n::t!(
                    "clan-share-make-clan-owned-confirm",
                    "name" => &dialog.area_name,
                    "clan" => &dialog.clan_name
                ))
                .size(12),
            );
            if dialog.accept_folders.is_empty() {
                section = section.push(note(crate::i18n::t!("clan-share-someone-accepts")));
            } else {
                section = section.push(
                    row![
                        text(crate::i18n::t!("clan-share-into-folder")).size(12),
                        share_picker(
                            dialog.accept_folders.clone(),
                            folder.clone(),
                            dialog.busy(),
                            crate::i18n::t!("clan-maps-folder-placeholder"),
                            180,
                            |choice| { msg(ClanMapShareMessage::StepFolder(choice)) }
                        ),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center),
                );
            }
            section = section.push(confirm_row(
                crate::i18n::t!("clan-share-make-clan-owned-action"),
                dialog.accept_folders.is_empty() || folder.is_some(),
                dialog.busy(),
            ));
        }
        Some(OwnerStep::ToMembers { .. }) => {}
    }
    section.into()
}

/// A clan owner's "Make member-owned…": the members it goes to, and who
/// loses the access the folder and the clan gave.
fn make_member_owned_section(dialog: &ClanMapShareDialog) -> ThemedElement<'_, Message> {
    let mut section = column![].spacing(6);
    section = section.push(pending_offers(dialog));
    match &dialog.step {
        Some(OwnerStep::ToMembers { picked }) => {
            section = section
                .push(text(crate::i18n::t!("clan-share-make-member-owned-help")).size(12))
                .push(member_checklist(dialog, picked));
            let losing: Vec<String> = losing_folder_access(&dialog.rows())
                .into_iter()
                .map(|recipient| dialog.recipient_label(recipient))
                .collect();
            if !losing.is_empty() {
                section = section.push(
                    text(crate::i18n::t!(
                        "clan-share-lose-folder-access",
                        "names" => losing.join(&crate::i18n::t!("mapper-multi-list-separator"))
                    ))
                    .size(12)
                    .style(builtins::text::danger),
                );
            }
            section = section.push(confirm_row(
                crate::i18n::t!("clan-share-send-offer"),
                !picked.is_empty(),
                dialog.busy(),
            ));
        }
        _ => {
            section = section.push(
                button(text(crate::i18n::t!("clan-share-make-member-owned")).size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe((!dialog.busy()).then_some(msg(ClanMapShareMessage::Step(
                        Some(OwnerStep::ToMembers {
                            picked: BTreeSet::new(),
                        }),
                    )))),
            );
        }
    }
    section.into()
}

// ===========================================================================
// Put in a clan…
// ===========================================================================

/// "Put in a clan…": transfers an owned map into an authorized clan folder.
#[derive(Debug, Clone)]
pub struct PutInClanDialog {
    session: ShareSession,
    operation: Uuid,
    pub area_id: AreaId,
    pub area_name: String,
    /// The viewer's clans by name.
    pub clans: Vec<(Uuid, String)>,
    pub clan: Option<Uuid>,
    pub ownership: MapOwnership,
    /// Per clan, the folders the viewer may accept the map into at once.
    pub folders: HashMap<Uuid, Vec<FolderChoice>>,
    pub folder: Option<FolderChoice>,
    /// The friends the map is shared with, whose shares end; `None` while
    /// loading.
    pub shares: Option<Result<Vec<String>, String>>,
    pub busy: bool,
    pub error: Option<String>,
    pub done: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PutInClanMessage {
    SharesLoaded {
        dialog: Uuid,
        result: Result<Vec<GrantTreeNode>, CloudError>,
    },
    ClanPicked(Uuid),
    OwnershipPicked(MapOwnership),
    FolderPicked(FolderChoice),
    Submit,
    Submitted {
        dialog: Uuid,
        result: Result<(), CloudError>,
    },
}

fn put(message: PutInClanMessage) -> Message {
    Message::PutInClan(message)
}

/// The friends a map's shares name: each grantee of a grant reaching it.
#[must_use]
pub fn shared_with(nodes: &[GrantTreeNode]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for node in nodes {
        let name = node
            .grantee_nickname
            .clone()
            .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// "Put in a clan…": transfers an owned map into an authorized clan folder.
pub(super) fn open_put_in_clan(
    window: &mut MapEditorWindow,
    area_id: AreaId,
) -> Update<Message, super::Event> {
    if !may_put_in_clan(window, area_id) {
        return Update::none();
    }
    let area_name = window
        .mapper
        .get_current_atlas()
        .get_area(&area_id)
        .map(|area| area.get_name().to_string())
        .unwrap_or_default();
    let destinations = super::clan_maps::TransferDestinations::new(window, true, false);
    let clans = destinations.clans;
    let folders = destinations.folders;
    let clan = (clans.len() == 1).then(|| clans[0].0);
    let folder = clan
        .and_then(|clan| folders.get(&clan))
        .filter(|list| list.len() == 1)
        .map(|list| list[0].clone());
    let session = ShareSession::new(window);
    let dialog_id = session.id;
    let client = session.client.clone();
    window.modal = Some(modals::Modal::PutInClan(Box::new(PutInClanDialog {
        operation: Uuid::new_v4(),
        session,
        area_id,
        area_name,
        clans,
        clan,
        ownership: MapOwnership::Members,
        folders,
        folder,
        shares: None,
        busy: false,
        error: None,
        done: None,
    })));
    Update::with_task(Task::perform(
        async move { client.area_shares(area_id).await },
        move |result| {
            put(PutInClanMessage::SharesLoaded {
                dialog: dialog_id,
                result,
            })
        },
    ))
}

pub(super) fn update_put_in_clan(
    window: &mut MapEditorWindow,
    message: PutInClanMessage,
) -> Update<Message, super::Event> {
    dismiss_stale(window);
    let Some(modals::Modal::PutInClan(dialog)) = &mut window.modal else {
        return Update::none();
    };
    let response_id = match &message {
        PutInClanMessage::SharesLoaded { dialog, .. }
        | PutInClanMessage::Submitted { dialog, .. } => Some(*dialog),
        _ => None,
    };
    if response_id.is_some_and(|id| id != dialog.session.id)
        || (response_id.is_none() && (dialog.busy || dialog.done.is_some()))
    {
        return Update::none();
    }
    let client = dialog.session.client.clone();
    let dialog_id = dialog.session.id;
    match message {
        PutInClanMessage::SharesLoaded { result, .. } => {
            dialog.shares = Some(
                result
                    .map(|nodes| shared_with(&nodes))
                    .map_err(|error| display_error(&error)),
            );
        }
        PutInClanMessage::ClanPicked(clan) => {
            dialog.operation = Uuid::new_v4();
            dialog.clan = Some(clan);
            dialog.folder = dialog
                .folders
                .get(&clan)
                .filter(|list| list.len() == 1)
                .map(|list| list[0].clone());
        }
        PutInClanMessage::OwnershipPicked(ownership) => {
            dialog.ownership = ownership;
            dialog.operation = Uuid::new_v4();
        }
        PutInClanMessage::FolderPicked(folder) => {
            dialog.folder = Some(folder);
            dialog.operation = Uuid::new_v4();
        }
        PutInClanMessage::Submit => {
            let Some(clan) = dialog.clan else {
                return Update::none();
            };
            let Some(folder) = dialog.folder.as_ref().filter(|folder| {
                dialog
                    .folders
                    .get(&clan)
                    .is_some_and(|folders| folders.contains(folder))
            }) else {
                return Update::none();
            };
            if dialog.busy {
                return Update::none();
            }
            dialog.busy = true;
            dialog.error = None;
            let (area_id, ownership) = (dialog.area_id, dialog.ownership);
            let folder = folder.id;
            let operation = dialog.operation;
            return Update::with_task(Task::perform(
                async move {
                    client
                        .transfer_area_to_clan(area_id, clan, ownership, folder, operation)
                        .await
                        .map(|_| ())
                },
                move |result| {
                    put(PutInClanMessage::Submitted {
                        dialog: dialog_id,
                        result,
                    })
                },
            ));
        }
        PutInClanMessage::Submitted { result, .. } => {
            if !dialog.busy {
                return Update::none();
            }
            dialog.busy = false;
            match result {
                Ok(()) => {
                    let clan = dialog
                        .clan
                        .and_then(|clan| dialog.clans.iter().find(|(id, _)| *id == clan))
                        .map(|(_, name)| name.clone())
                        .unwrap_or_default();
                    dialog.done = Some(crate::i18n::t!("clan-share-put-done", "clan" => clan));
                    window.mapper.sync_now();
                    return Update::with_task(super::clan_maps::fetch(window));
                }
                Err(error) => {
                    dialog.error = Some(match error {
                        CloudError::NotFoundOrNoAccess => {
                            crate::i18n::t!("mapper-transfer-owner-clan-only")
                        }
                        other => display_error(&other),
                    });
                }
            }
        }
    }
    Update::none()
}

/// The Put in a clan dialog's title and body.
#[must_use]
pub fn put_in_clan_view(dialog: &PutInClanDialog) -> (String, ThemedElement<'_, Message>) {
    let title = crate::i18n::t!("clan-share-put-title", "name" => &dialog.area_name);
    if let Some(done) = &dialog.done {
        return (
            title,
            modals::scrolling_body(
                text(done.clone()).size(13),
                row![
                    space::horizontal(),
                    button(text(crate::i18n::t!("action-close")).size(13))
                        .style(builtins::button::secondary)
                        .on_press(Message::ModalDismissed),
                ],
            ),
        );
    }
    let mut body: Column<'_, Message, crate::Theme> = Column::new().spacing(10);
    body = body.push(label(crate::i18n::t!("mapper-transfer-clans")));
    let mut clans = Column::new().spacing(4);
    for (id, name) in &dialog.clans {
        let style = if dialog.clan == Some(*id) {
            builtins::button::primary
        } else {
            builtins::button::secondary
        };
        clans = clans.push(
            button(text(name.clone()).size(13))
                .style(style)
                .width(Length::Fill)
                .on_press_maybe((!dialog.busy).then_some(put(PutInClanMessage::ClanPicked(*id)))),
        );
    }
    body = body.push(clans);
    body = body.push(modals::ownership_choice(
        dialog.ownership,
        true,
        |ownership| put(PutInClanMessage::OwnershipPicked(ownership)),
    ));
    if let Some(clan) = dialog.clan {
        match dialog.folders.get(&clan).filter(|list| !list.is_empty()) {
            Some(list) => {
                body = body.push(
                    row![
                        text(crate::i18n::t!("clan-share-into-folder")).size(12),
                        share_picker(
                            list.clone(),
                            dialog.folder.clone(),
                            dialog.busy,
                            crate::i18n::t!("clan-maps-folder-placeholder"),
                            180,
                            |choice| put(PutInClanMessage::FolderPicked(choice))
                        ),
                    ]
                    .spacing(8)
                    .align_y(Vertical::Center),
                );
            }
            None => body = body.push(note(crate::i18n::t!("clan-maps-no-transfer-folder"))),
        }
    }
    match &dialog.shares {
        None => body = body.push(note(crate::i18n::t!("mapper-loading"))),
        Some(Err(error)) => body = body.push(danger(error.clone())),
        Some(Ok(names)) if names.is_empty() => {}
        Some(Ok(names)) => {
            body = body.push(
                text(crate::i18n::t!(
                    "clan-share-friend-shares-end",
                    "names" => names.join(&crate::i18n::t!("mapper-multi-list-separator"))
                ))
                .size(12)
                .style(builtins::text::danger),
            );
        }
    }
    body = body.push(note(crate::i18n::t!("clan-share-put-secrets")));
    if let Some(error) = &dialog.error {
        body = body.push(danger(error.clone()));
    }
    if dialog.busy {
        body = body.push(note(crate::i18n::t!("mapper-saving")));
    }
    let ready = dialog.clan.is_some() && dialog.folder.is_some() && !dialog.busy;
    let footer = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-cancel")).size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
        button(text(crate::i18n::t!("clan-share-put-action")).size(13))
            .style(builtins::button::primary)
            .on_press_maybe(ready.then_some(put(PutInClanMessage::Submit))),
    ]
    .spacing(10);
    (title, modals::scrolling_body(body, footer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clan_map_dialog_footers_survive_long_lists() {
        let mut sharing = dialog(MapOwnership::Clan);
        sharing.secrets = (100..150)
            .map(|n| (SourceId::Secret(id(n)), format!("Secret {n}")))
            .collect();
        let transfer = PutInClanDialog {
            session: sharing.session.clone(),
            operation: Uuid::new_v4(),
            area_id: MAP,
            area_name: "Grove".to_string(),
            clans: (100..150).map(|n| (id(n), format!("Clan {n}"))).collect(),
            clan: Some(id(100)),
            ownership: MapOwnership::Clan,
            folders: HashMap::new(),
            folder: Some(FolderChoice {
                id: ROADS,
                name: "Roads".to_string(),
            }),
            shares: Some(Ok(vec!["Explorer".repeat(100)])),
            busy: false,
            error: None,
            done: None,
        };
        for (modal, label, name) in [
            (
                modals::Modal::ClanMapShare(Box::new(sharing)),
                crate::i18n::t!("action-close"),
                "clan-map-share",
            ),
            (
                modals::Modal::PutInClan(Box::new(transfer)),
                crate::i18n::t!("clan-share-put-action"),
                "put-in-clan",
            ),
        ] {
            for size in [(380, 360), (600, 500)] {
                let messages = crate::widgets::dialog::tests::check_actions(
                    modal.view(),
                    size,
                    std::slice::from_ref(&label),
                    name,
                )
                .await;
                assert_eq!(messages.len(), 2, "{name}: both clicks reach the footer");
                assert!(messages.iter().all(|m| matches!(
                    m,
                    Message::ModalDismissed | Message::PutInClan(PutInClanMessage::Submit)
                )));
            }
        }
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn an_offer_picks_at_most_sixteen_recipients() {
        let mut picked = BTreeSet::new();
        for n in 0..20 {
            toggle_recipient(&mut picked, id(n), true);
        }
        assert_eq!(picked.len(), MAX_OFFER_RECIPIENTS);
        assert!(!picked.contains(&id(16)), "a pick past the cap is ignored");
        toggle_recipient(&mut picked, id(3), false);
        toggle_recipient(&mut picked, id(17), true);
        assert_eq!(picked.len(), MAX_OFFER_RECIPIENTS);
        assert!(picked.contains(&id(17)) && !picked.contains(&id(3)));
    }

    fn grant(n: u128, recipient: GrantRecipient, scope: GrantScope, actions: &[&str]) -> ClanGrant {
        ClanGrant {
            id: id(n),
            clan_id: id(1),
            recipient,
            actions: actions.iter().map(ToString::to_string).collect(),
            may_grant: None,
            scope,
            issuer_id: id(2),
            parent_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn group(n: u128) -> GrantRecipient {
        GrantRecipient::Group { group_id: id(n) }
    }

    const MAP: AreaId = AreaId(Uuid::from_u128(50));
    const OTHER: AreaId = AreaId(Uuid::from_u128(51));
    const ROADS: AtlasId = AtlasId(Uuid::from_u128(60));
    const TOWNS: AtlasId = AtlasId(Uuid::from_u128(61));

    fn grants() -> Vec<ClanGrant> {
        vec![
            grant(
                10,
                group(100),
                GrantScope::Clan,
                &["area.read", "clan.invite"],
            ),
            grant(
                11,
                group(101),
                GrantScope::Atlases { ids: vec![ROADS] },
                &["area.read", "area.add", "area.edit", "area.create"],
            ),
            grant(
                12,
                group(102),
                GrantScope::Atlases { ids: vec![TOWNS] },
                &["area.read"],
            ),
            grant(
                13,
                GrantRecipient::User { user_id: id(7) },
                GrantScope::Areas { ids: vec![MAP] },
                &["area.read", "area.copy"],
            ),
            grant(
                14,
                group(103),
                GrantScope::Areas {
                    ids: vec![MAP, OTHER],
                },
                &["area.read"],
            ),
            // Gives nothing on a map: left out.
            grant(15, group(104), GrantScope::Clan, &["clan.invite"]),
        ]
    }

    #[test]
    fn a_clan_owned_map_lists_its_own_grants_then_what_it_inherits() {
        let rows = access_rows(&grants(), MAP, Some(ROADS), false, true);
        let seen: Vec<(u128, RowSource, bool)> = rows
            .iter()
            .map(|row| (row.grant.id.as_u128(), row.source, row.editable))
            .collect();
        assert_eq!(
            seen,
            [
                (13, RowSource::Map, true),
                (14, RowSource::SeveralMaps, false),
                (11, RowSource::Folder, false),
                (10, RowSource::Clan, false),
            ]
        );
        // What a row gives is its map actions alone.
        assert_eq!(
            rows[2].actions,
            ["area.add", "area.edit", "area.read"]
                .iter()
                .map(ToString::to_string)
                .collect()
        );
        // Without managing access nothing is editable.
        assert!(
            access_rows(&grants(), MAP, Some(ROADS), false, false)
                .iter()
                .all(|row| !row.editable)
        );
    }

    #[test]
    fn a_member_owned_map_lists_only_its_own_grants() {
        let rows = access_rows(&grants(), MAP, Some(ROADS), true, true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].grant.id, id(13));
        assert!(rows[0].editable);
    }

    #[test]
    fn making_a_map_member_owned_names_who_loses_folder_access() {
        let mut all = grants();
        // Group 101 also holds the map's own grant: it keeps access.
        all.push(grant(
            16,
            group(101),
            GrantScope::Areas { ids: vec![MAP] },
            &["area.read"],
        ));
        let rows = access_rows(&all, MAP, Some(ROADS), false, true);
        assert_eq!(losing_folder_access(&rows), [group(103), group(100)]);
    }

    #[test]
    fn who_chooses_a_groups_members() {
        let custom = ClanGroup {
            id: id(101),
            name: "Scouts".to_string(),
            color: None,
            builtin: None,
            is_member: true,
            created_by_me: false,
            actions: BTreeSet::new(),
        };
        let everyone = ClanGroup {
            builtin: Some("members".to_string()),
            ..custom.clone()
        };
        assert_eq!(chooser(&everyone, None), Chooser::Automatic);
        assert_eq!(chooser(&custom, None), Chooser::OwnersAndLeads);
        assert_eq!(chooser(&custom, Some(&grants())), Chooser::OwnersOnly);
        let led = vec![grant(
            20,
            GrantRecipient::User { user_id: id(7) },
            GrantScope::Groups { ids: vec![id(101)] },
            &["group.assign"],
        )];
        assert_eq!(chooser(&custom, Some(&led)), Chooser::OwnersAndLeads);
    }

    #[test]
    fn a_draft_is_its_preset_plus_the_separate_actions() {
        let mut draft = GrantDraft {
            grant_id: None,
            recipient: None,
            preset: Some(Preset::MapContributor),
            core: BTreeSet::new(),
            separate: ["area.copy".to_string()].into(),
            kept: BTreeSet::new(),
            may_grant: None,
            error: None,
        };
        assert_eq!(
            draft.actions(),
            ["area.read", "area.add", "area.edit", "area.copy"]
                .iter()
                .map(ToString::to_string)
                .collect()
        );
        // A Custom grant keeps its core actions as they are.
        draft.preset = None;
        draft.core = ["area.read".to_string(), "area.remove_content".to_string()].into();
        assert!(draft.actions().contains("area.remove_content"));
        assert!(!draft.actions().contains("area.add"));
    }

    fn dialog(ownership: MapOwnership) -> ClanMapShareDialog {
        ClanMapShareDialog {
            session: ShareSession {
                id: Uuid::new_v4(),
                credential_generation: 0,
                auth_projection_revision: 0,
                client: crate::cloud_account::test_handles_signed_in("tester").client,
            },
            pending: HashMap::new(),
            area_id: MAP,
            area_name: "Grove".to_string(),
            clan_id: id(1),
            clan_name: "Lantern Company".to_string(),
            folder: Some((ROADS, "Roads".to_string())),
            ownership,
            frozen: false,
            owned_by_me: false,
            viewer_id: Some(id(9)),
            clan_owner: false,
            held: BTreeSet::new(),
            accept_folders: Vec::new(),
            secrets: Vec::new(),
            groups: Some(Ok(vec![
                ClanGroup {
                    id: id(100),
                    name: "All clan members".to_string(),
                    color: None,
                    builtin: Some("members".to_string()),
                    is_member: true,
                    created_by_me: false,
                    actions: BTreeSet::new(),
                },
                ClanGroup {
                    id: id(102),
                    name: "Owner".to_string(),
                    color: None,
                    builtin: Some("owners".to_string()),
                    is_member: false,
                    created_by_me: false,
                    actions: BTreeSet::new(),
                },
                ClanGroup {
                    id: id(101),
                    name: "Scouts".to_string(),
                    color: None,
                    builtin: None,
                    is_member: true,
                    created_by_me: false,
                    actions: BTreeSet::new(),
                },
            ])),
            members: None,
            grants: Some(Ok(grants())),
            all_grants: None,
            owners: None,
            offers: None,
            outside: None,
            friends: None,
            draft: None,
            removing: None,
            outside_pick: None,
            step: None,
            error: None,
            notice: None,
        }
    }

    async fn share_window() -> MapEditorWindow {
        use super::super::links::fixture;
        let mut window =
            super::super::test_window(fixture::maps().await, fixture::area(fixture::KEEP));
        show_share(&mut window, MAP);
        window
    }

    fn show_share(window: &mut MapEditorWindow, area: AreaId) {
        let mut state = dialog(MapOwnership::Clan);
        state.session = ShareSession::new(window);
        state.area_id = area;
        state.clan_owner = true;
        state.step = Some(OwnerStep::GiveUp);
        window.modal = Some(modals::Modal::ClanMapShare(Box::new(state)));
        let _ = update(window, ClanMapShareMessage::AddStarted);
    }

    fn request(window: &mut MapEditorWindow, kind: RequestKind) -> ShareRequest {
        let state = dialog_mut(window).unwrap();
        ShareRequest {
            dialog: state.session.id,
            id: state.pending[&kind],
        }
    }

    fn answer(request: ShareRequest, response: ClanMapShareResponse) -> ClanMapShareMessage {
        ClanMapShareMessage::Response { request, response }
    }

    // Complete an actual iced task, but let the test decide when its message arrives.
    async fn delayed_reply(
        window: &mut MapEditorWindow,
        response: ClanMapShareResponse,
    ) -> ClanMapShareMessage {
        use futures::StreamExt;
        let task = perform(
            dialog_mut(window).unwrap(),
            response.kind(),
            async move { response },
            std::convert::identity,
        );
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(Message::ClanMapShare(message))) =
            stream.next().await
        else {
            panic!("expected a scoped sharing response");
        };
        *message
    }

    #[tokio::test]
    async fn replies_never_populate_or_close_another_opening_even_of_the_same_map() {
        let mut window = share_window().await;
        for next_map in [MAP, OTHER] {
            for response in [
                ClanMapShareResponse::GroupsLoaded(Ok(vec![])),
                ClanMapShareResponse::MembersLoaded(Ok(vec![])),
                ClanMapShareResponse::GrantsLoaded(Ok(vec![])),
                ClanMapShareResponse::AllGrantsLoaded(Ok(vec![])),
                ClanMapShareResponse::OwnersLoaded(Ok(vec![])),
                ClanMapShareResponse::OffersLoaded(Ok(vec![])),
                ClanMapShareResponse::OutsideLoaded(Ok(vec![])),
                ClanMapShareResponse::FriendsLoaded(Ok(vec![])),
                ClanMapShareResponse::DraftResult(Ok(())),
                ClanMapShareResponse::RemoveResult(Ok(())),
                ClanMapShareResponse::OutsideResult(Ok(())),
                ClanMapShareResponse::StepResult(Ok(None)),
                ClanMapShareResponse::WithdrawResult(Ok(())),
                ClanMapShareResponse::DraftResult(Err(CloudError::NotFoundOrNoAccess)),
                ClanMapShareResponse::StepResult(Err(CloudError::NotFoundOrNoAccess)),
            ] {
                show_share(&mut window, MAP);
                let reply = delayed_reply(&mut window, response.clone()).await;
                show_share(&mut window, next_map);
                // The new opening can have a request of exactly the same kind pending.
                let _ = delayed_reply(&mut window, response).await;
                let before = format!("{:?}", window.modal);
                assert_eq!(update(&mut window, reply).task.units(), 0);
                assert_eq!(format!("{:?}", window.modal), before);
            }
        }
    }

    #[tokio::test]
    async fn dismissed_dialog_ignores_late_ownership_success() {
        let mut window = share_window().await;
        let reply = delayed_reply(&mut window, ClanMapShareResponse::StepResult(Ok(None))).await;
        window.modal = None;
        assert_eq!(update(&mut window, reply).task.units(), 0);
        assert!(window.modal.is_none());
    }

    #[tokio::test]
    async fn saving_locks_the_submitted_form_and_rejects_duplicate_writes() {
        let mut window = share_window().await;
        let recipient = RecipientChoice {
            recipient: group(101),
            label: "Scouts".into(),
        };
        let _ = update(&mut window, ClanMapShareMessage::DraftRecipient(recipient));
        assert_eq!(
            update(&mut window, ClanMapShareMessage::DraftSaved)
                .task
                .units(),
            1
        );
        let pending = request(&mut window, RequestKind::Draft);
        let before = format!("{:?}", window.modal);
        for message in [
            ClanMapShareMessage::DraftSaved,
            ClanMapShareMessage::DraftCancelled,
            ClanMapShareMessage::AddStarted,
            ClanMapShareMessage::EditStarted(id(13)),
            ClanMapShareMessage::DraftPreset(PresetChoice(Preset::MapReader)),
            ClanMapShareMessage::DraftSeparate("area.copy", true),
            ClanMapShareMessage::RemoveRequested(id(13)),
            ClanMapShareMessage::RemoveConfirmed,
            ClanMapShareMessage::OutsideShare,
            ClanMapShareMessage::Step(None),
            ClanMapShareMessage::StepConfirmed,
            ClanMapShareMessage::OfferWithdrawn(id(90)),
        ] {
            assert_eq!(update(&mut window, message).task.units(), 0);
            assert_eq!(format!("{:?}", window.modal), before);
        }
        let _ = view(dialog_mut(&mut window).unwrap());
        let failure = answer(
            pending,
            ClanMapShareResponse::DraftResult(Err(CloudError::NotFoundOrNoAccess)),
        );
        assert_eq!(update(&mut window, failure).task.units(), 2);
        let state = dialog_mut(&mut window).unwrap();
        assert!(!state.busy());
        assert!(state.draft.as_ref().unwrap().error.is_some());
        let _ = update(&mut window, ClanMapShareMessage::DraftCancelled);
        assert!(dialog_mut(&mut window).unwrap().draft.is_none());
    }

    #[tokio::test]
    async fn older_grant_lists_cannot_undo_a_completed_save_or_its_refresh() {
        let mut window = share_window().await;
        let _ = fetch_grants(dialog_mut(&mut window).unwrap());
        let old = request(&mut window, RequestKind::Grants);
        let old_all = request(&mut window, RequestKind::AllGrants);
        let _ = update(&mut window, ClanMapShareMessage::RemoveRequested(id(13)));
        let _ = update(&mut window, ClanMapShareMessage::RemoveConfirmed);
        let removal = request(&mut window, RequestKind::Remove);
        // The initial GET also cannot repopulate the list while the DELETE is pending.
        let _ = update(
            &mut window,
            answer(old, ClanMapShareResponse::GrantsLoaded(Ok(grants()))),
        );
        assert!(dialog_mut(&mut window).unwrap().grants.is_none());
        let _ = update(
            &mut window,
            answer(removal, ClanMapShareResponse::RemoveResult(Ok(()))),
        );
        let fresh = request(&mut window, RequestKind::Grants);
        let fresh_all = request(&mut window, RequestKind::AllGrants);
        let _ = update(
            &mut window,
            answer(fresh, ClanMapShareResponse::GrantsLoaded(Ok(vec![]))),
        );
        let _ = update(
            &mut window,
            answer(fresh_all, ClanMapShareResponse::AllGrantsLoaded(Ok(vec![]))),
        );
        let _ = update(
            &mut window,
            answer(old, ClanMapShareResponse::GrantsLoaded(Ok(grants()))),
        );
        let _ = update(
            &mut window,
            answer(old_all, ClanMapShareResponse::AllGrantsLoaded(Ok(grants()))),
        );
        let state = dialog_mut(&mut window).unwrap();
        assert!(state.grants.as_ref().unwrap().as_ref().unwrap().is_empty());
        assert!(state.all_grants.as_ref().unwrap().is_empty());
    }

    #[tokio::test]
    async fn consumed_save_reply_cannot_clear_a_later_draft() {
        let mut window = share_window().await;
        let reply = delayed_reply(&mut window, ClanMapShareResponse::DraftResult(Ok(()))).await;
        let _ = update(&mut window, reply.clone());
        assert!(dialog_mut(&mut window).unwrap().draft.is_none());
        let _ = update(&mut window, ClanMapShareMessage::AddStarted);
        assert_eq!(update(&mut window, reply).task.units(), 0);
        assert!(dialog_mut(&mut window).unwrap().draft.is_some());
    }

    #[tokio::test]
    async fn withdrawing_an_offer_does_not_clear_an_unrelated_ownership_draft() {
        let mut window = share_window().await;
        let _ = fetch_offers(dialog_mut(&mut window).unwrap());
        let old = request(&mut window, RequestKind::Offers);
        let _ = update(&mut window, ClanMapShareMessage::OfferWithdrawn(id(90)));
        let withdrawal = request(&mut window, RequestKind::Withdraw);
        assert_eq!(
            update(&mut window, ClanMapShareMessage::OfferWithdrawn(id(90)))
                .task
                .units(),
            0
        );
        let _ = update(
            &mut window,
            answer(withdrawal, ClanMapShareResponse::WithdrawResult(Ok(()))),
        );
        let fresh = request(&mut window, RequestKind::Offers);
        let _ = update(
            &mut window,
            answer(fresh, ClanMapShareResponse::OffersLoaded(Ok(vec![]))),
        );
        let _ = update(
            &mut window,
            answer(
                old,
                ClanMapShareResponse::OffersLoaded(Err(CloudError::NotFoundOrNoAccess)),
            ),
        );
        let state = dialog_mut(&mut window).unwrap();
        assert_eq!(state.step, Some(OwnerStep::GiveUp));
        assert!(state.offers.as_ref().unwrap().is_ok());
    }

    #[tokio::test]
    async fn outside_share_refresh_ignores_the_previous_list_and_unlocks_the_picker() {
        let mut window = share_window().await;
        let _ = fetch_outside(dialog_mut(&mut window).unwrap());
        let old = request(&mut window, RequestKind::Outside);
        let _ = update(
            &mut window,
            ClanMapShareMessage::OutsidePicked(RecipientChoice {
                recipient: GrantRecipient::User { user_id: id(30) },
                label: "A friend".into(),
            }),
        );
        let _ = update(&mut window, ClanMapShareMessage::OutsideShare);
        let sharing = request(&mut window, RequestKind::OutsideWrite);
        let _ = update(
            &mut window,
            answer(sharing, ClanMapShareResponse::OutsideResult(Ok(()))),
        );
        let fresh = request(&mut window, RequestKind::Outside);
        let _ = update(
            &mut window,
            answer(fresh, ClanMapShareResponse::OutsideLoaded(Ok(vec![]))),
        );
        let _ = update(
            &mut window,
            answer(
                old,
                ClanMapShareResponse::OutsideLoaded(Err(CloudError::NotFoundOrNoAccess)),
            ),
        );
        let state = dialog_mut(&mut window).unwrap();
        assert!(!state.busy());
        assert!(state.outside_pick.is_none());
        assert!(state.outside.as_ref().unwrap().is_ok());
    }

    #[tokio::test]
    async fn current_ownership_success_still_closes_its_own_dialog() {
        let mut window = share_window().await;
        let _ = update(&mut window, ClanMapShareMessage::StepConfirmed);
        let pending = request(&mut window, RequestKind::Ownership);
        let _ = update(
            &mut window,
            answer(pending, ClanMapShareResponse::StepResult(Ok(None))),
        );
        assert!(window.modal.is_none());
    }

    #[tokio::test]
    async fn account_change_discards_names_replies_and_actions_even_before_mapper_sync() {
        let mut window = share_window().await;
        let reply =
            delayed_reply(&mut window, ClanMapShareResponse::MembersLoaded(Ok(vec![]))).await;
        let old = window.cloud.client.credentials().get();
        let frozen = dialog_mut(&mut window).unwrap().session.client.clone();
        window
            .cloud
            .client
            .credentials()
            .set(Some(smudgy_cloud::Credential::Session(
                "another-account".into(),
            )));
        assert_eq!(frozen.credentials().get(), old);
        assert_eq!(update(&mut window, reply).task.units(), 0);
        assert!(window.modal.is_none());
        show_share(&mut window, MAP);
        // Even a switch back to the same credential invalidates the opening.
        window.cloud.client.credentials().set(old);
        assert_eq!(
            update(&mut window, ClanMapShareMessage::StepConfirmed)
                .task
                .units(),
            0
        );
        assert!(window.modal.is_none());
        show_share(&mut window, MAP);
        window.cloud.client.credentials().set(None);
        dismiss_stale(&mut window);
        assert!(window.modal.is_none());
    }

    fn show_put(window: &mut MapEditorWindow, area: AreaId) -> Uuid {
        let session = ShareSession::new(window);
        let dialog_id = session.id;
        window.modal = Some(modals::Modal::PutInClan(Box::new(PutInClanDialog {
            operation: Uuid::new_v4(),
            session,
            area_id: area,
            area_name: "A personal map".into(),
            clans: vec![(id(1), "First clan".into()), (id(2), "Second clan".into())],
            clan: Some(id(1)),
            ownership: MapOwnership::Members,
            folders: HashMap::from([(
                id(1),
                vec![FolderChoice {
                    id: AtlasId(id(3)),
                    name: "Destination".into(),
                }],
            )]),
            folder: Some(FolderChoice {
                id: AtlasId(id(3)),
                name: "Destination".into(),
            }),
            shares: None,
            busy: false,
            error: None,
            done: None,
        })));
        dialog_id
    }

    #[tokio::test]
    async fn put_in_clan_replies_belong_to_one_opening_and_one_account() {
        let mut window = share_window().await;
        for next_map in [MAP, OTHER] {
            let original = show_put(&mut window, MAP);
            let _ = update_put_in_clan(&mut window, PutInClanMessage::Submit);
            let active = show_put(&mut window, next_map);
            let _ = update_put_in_clan(&mut window, PutInClanMessage::Submit);
            let before = format!("{:?}", window.modal);
            for message in [
                PutInClanMessage::SharesLoaded {
                    dialog: original,
                    result: Ok(vec![]),
                },
                PutInClanMessage::Submitted {
                    dialog: original,
                    result: Ok(()),
                },
                PutInClanMessage::Submitted {
                    dialog: original,
                    result: Err(CloudError::NotFoundOrNoAccess),
                },
            ] {
                assert_eq!(update_put_in_clan(&mut window, message).task.units(), 0);
                assert_eq!(format!("{:?}", window.modal), before);
            }
            let _ = update_put_in_clan(
                &mut window,
                PutInClanMessage::Submitted {
                    dialog: active,
                    result: Ok(()),
                },
            );
            let Some(modals::Modal::PutInClan(state)) = &window.modal else {
                panic!("put dialog");
            };
            assert!(state.done.is_some());
            assert!(!state.busy);
        }
        let original = show_put(&mut window, MAP);
        let _ = update_put_in_clan(&mut window, PutInClanMessage::Submit);
        window.cloud.client.credentials().set(None);
        let _ = update_put_in_clan(
            &mut window,
            PutInClanMessage::Submitted {
                dialog: original,
                result: Ok(()),
            },
        );
        assert!(window.modal.is_none());
    }

    #[tokio::test]
    async fn put_in_clan_keeps_the_submitted_clan_and_ownership_until_completion() {
        let mut window = share_window().await;
        let original = show_put(&mut window, MAP);
        assert_eq!(
            update_put_in_clan(&mut window, PutInClanMessage::Submit)
                .task
                .units(),
            1
        );
        let before = format!("{:?}", window.modal);
        for message in [
            PutInClanMessage::ClanPicked(id(2)),
            PutInClanMessage::OwnershipPicked(MapOwnership::Clan),
            PutInClanMessage::FolderPicked(FolderChoice {
                id: ROADS,
                name: "Roads".into(),
            }),
            PutInClanMessage::Submit,
        ] {
            assert_eq!(update_put_in_clan(&mut window, message).task.units(), 0);
            assert_eq!(format!("{:?}", window.modal), before);
        }
        let Some(modals::Modal::PutInClan(state)) = &window.modal else {
            panic!("put dialog");
        };
        let _ = put_in_clan_view(state);
        let _ = update_put_in_clan(
            &mut window,
            PutInClanMessage::Submitted {
                dialog: original,
                result: Ok(()),
            },
        );
        let Some(modals::Modal::PutInClan(state)) = &window.modal else {
            panic!("put dialog");
        };
        assert_eq!(
            state.done,
            Some(crate::i18n::t!("clan-share-put-done", "clan" => "First clan"))
        );
    }

    /// Editing one of the map's own grants keeps what the dialog does not
    /// show: seeing and handing out access, what it may hand out, and
    /// actions this client does not know, through Save.
    #[test]
    fn editing_a_grant_keeps_what_the_dialog_does_not_show() {
        let mut clan_owned = dialog(MapOwnership::Clan);
        clan_owned.clan_owner = true;
        let mut administered = grant(
            30,
            group(101),
            GrantScope::Areas { ids: vec![MAP] },
            &[
                "area.read",
                "area.add",
                "area.edit",
                "grant.inspect",
                "grant.manage",
                "lore.read",
            ],
        );
        administered.may_grant = Some(["area.read".to_string()].into());
        clan_owned.grants = Some(Ok(vec![administered]));

        let mut draft = clan_owned.edit_draft(id(30)).expect("the map's own grant");
        assert_eq!(draft.preset, Some(Preset::MapContributor));
        draft.preset = Some(Preset::MapReader);
        assert_eq!(
            draft.actions(),
            ["area.read", "grant.inspect", "grant.manage", "lore.read"]
                .iter()
                .map(ToString::to_string)
                .collect()
        );
        assert_eq!(draft.may_grant(), Some(vec!["area.read".to_string()]));

        // A grant that hands nothing out sends no ceiling.
        clan_owned.grants = Some(Ok(vec![grant(
            31,
            group(101),
            GrantScope::Areas { ids: vec![MAP] },
            &["area.read", "grant.inspect"],
        )]));
        let draft = clan_owned.edit_draft(id(31)).expect("the map's own grant");
        assert!(draft.actions().contains("grant.inspect"));
        assert_eq!(draft.may_grant(), None);
    }

    /// Picking a preset replaces only the Map preset's actions: renaming,
    /// Clan-owned Secret actions and the rest the grant carries stay.
    #[test]
    fn a_preset_pick_keeps_what_no_preset_holds() {
        let mut clan_owned = dialog(MapOwnership::Clan);
        clan_owned.clan_owner = true;
        clan_owned.grants = Some(Ok(vec![grant(
            32,
            group(101),
            GrantScope::Areas { ids: vec![MAP] },
            &["area.read", "area.rename", "secret.read"],
        )]));
        let mut draft = clan_owned.edit_draft(id(32)).expect("the map's own grant");
        assert_eq!(draft.preset, Some(Preset::MapReader));
        draft.preset = Some(Preset::MapContributor);
        assert_eq!(
            draft.actions(),
            BTreeSet::from(
                [
                    "area.read",
                    "area.add",
                    "area.edit",
                    "area.rename",
                    "secret.read"
                ]
                .map(String::from)
            )
        );
    }

    /// A delegate is offered only what one of their delegations covering
    /// the map hands out, and changes only the map's own grants within it
    /// that hand nothing out; a clan owner is bound by none.
    #[test]
    fn a_delegate_is_offered_only_what_their_delegation_hands_out() {
        let mut clan_owned = dialog(MapOwnership::Clan);
        clan_owned.held.insert(action::MANAGE_GRANTS.to_string());
        let mut delegation = grant(
            40,
            group(101),
            GrantScope::Atlases { ids: vec![ROADS] },
            &["grant.inspect", "grant.manage"],
        );
        delegation.may_grant = Some(
            ["area.read", "area.add", "area.edit", "area.copy"]
                .iter()
                .map(ToString::to_string)
                .collect(),
        );
        // Not the viewer's: the owners' group, which they are not in.
        let mut theirs = grant(44, group(102), GrantScope::Clan, &["grant.manage"]);
        theirs.may_grant = Some(
            ["area.read", "area.remove_content", "area.delete"]
                .iter()
                .map(ToString::to_string)
                .collect(),
        );
        let mut delegating = grant(
            43,
            group(104),
            GrantScope::Areas { ids: vec![MAP] },
            &["area.read", "grant.manage"],
        );
        delegating.may_grant = Some(["area.read".to_string()].into());
        let map_only = GrantScope::Areas { ids: vec![MAP] };
        clan_owned.grants = Some(Ok(vec![
            delegation,
            theirs,
            grant(41, group(100), map_only.clone(), &["area.read", "area.add"]),
            grant(
                42,
                group(103),
                map_only,
                &["area.read", "area.remove_content"],
            ),
            delegating,
        ]));

        assert_eq!(
            clan_owned.preset_choices(&BTreeSet::new()),
            [Preset::MapReader, Preset::MapContributor]
        );
        assert_eq!(clan_owned.separate_actions(), [action::COPY_AREA]);
        let editable: Vec<Uuid> = clan_owned
            .rows()
            .into_iter()
            .filter(|row| row.editable)
            .map(|row| row.grant.id)
            .collect();
        assert_eq!(editable, [id(41)]);
        assert!(clan_owned.edit_draft(id(42)).is_none());

        clan_owned.clan_owner = true;
        assert_eq!(clan_owned.preset_choices(&BTreeSet::new()).len(), 3);
        assert_eq!(
            clan_owned.separate_actions().len(),
            Kind::Map.separate().len()
        );
        assert!(clan_owned.edit_draft(id(42)).is_some());
        assert!(clan_owned.edit_draft(id(43)).is_some());
    }

    fn member(n: u128, is_owner: bool, groups: &[u128]) -> ClanMember {
        ClanMember {
            user_id: id(n),
            nickname: Some(format!("user{n}")),
            joined_at: chrono::Utc::now(),
            is_owner,
            group_ids: groups.iter().map(|group| id(*group)).collect(),
        }
    }

    /// Ownership is offered only to members who read the map, as its
    /// grants tell: never to one the server would refuse.
    #[test]
    fn ownership_is_offered_only_to_members_who_read_the_map() {
        let members = vec![
            member(9, false, &[]),
            member(20, false, &[101]),
            member(21, false, &[]),
            member(22, true, &[]),
            member(23, false, &[]),
        ];
        let map_only = || GrantScope::Areas { ids: vec![MAP] };
        let candidates = |dialog: &ClanMapShareDialog| -> Vec<Uuid> {
            dialog
                .ownership_candidates()
                .into_iter()
                .map(|(user, _)| user)
                .collect()
        };

        let mut member_owned = dialog(MapOwnership::Members);
        member_owned.owned_by_me = true;
        member_owned.owners = Some(Ok(vec![MapOwner {
            user_id: id(9),
            nickname: None,
            active: true,
            added_at: chrono::Utc::now(),
        }]));
        member_owned.members = Some(Ok(members.clone()));
        member_owned.grants = Some(Ok(vec![
            grant(50, group(101), map_only(), &["area.read"]),
            grant(
                51,
                GrantRecipient::User { user_id: id(23) },
                map_only(),
                &["area.read", "area.copy"],
            ),
        ]));
        assert_eq!(
            candidates(&member_owned),
            [id(20), id(23)],
            "a clan owner reads no Member-owned map they are not given"
        );

        let mut clan_owned = dialog(MapOwnership::Clan);
        clan_owned.clan_owner = true;
        clan_owned.members = Some(Ok(members));
        clan_owned.grants = Some(Ok(vec![grant(
            52,
            group(101),
            GrantScope::Atlases { ids: vec![ROADS] },
            &["area.read"],
        )]));
        assert_eq!(
            candidates(&clan_owned),
            [id(20), id(22)],
            "its folder's readers and the clan's owners"
        );
    }

    #[test]
    fn who_manages_and_shares_a_clan_map_outside() {
        let mut clan_owned = dialog(MapOwnership::Clan);
        assert!(!clan_owned.manages() && !clan_owned.shares_outside());
        clan_owned.held.insert(action::MANAGE_GRANTS.to_string());
        assert!(clan_owned.manages());
        clan_owned
            .held
            .insert(action::SHARE_AREA_EXTERNALLY.to_string());
        assert!(clan_owned.shares_outside());

        // A Member-owned map: its active owners alone, never while frozen,
        // and never outside the clan, clan owners included.
        let mut member_owned = dialog(MapOwnership::Members);
        member_owned.clan_owner = true;
        member_owned
            .held
            .insert(action::SHARE_AREA_EXTERNALLY.to_string());
        assert!(!member_owned.manages());
        assert!(!member_owned.shares_outside());
        member_owned.owned_by_me = true;
        assert!(member_owned.manages());
        member_owned.frozen = true;
        assert!(!member_owned.manages());
    }

    #[test]
    fn recipients_leave_out_the_owner_group_and_who_holds_the_maps_own_access() {
        let mut clan_owned = dialog(MapOwnership::Clan);
        clan_owned.members = Some(Ok(vec![
            ClanMember {
                user_id: id(9),
                nickname: Some("me".to_string()),
                joined_at: chrono::Utc::now(),
                is_owner: false,
                group_ids: Vec::new(),
            },
            ClanMember {
                user_id: id(7),
                nickname: Some("tomas".to_string()),
                joined_at: chrono::Utc::now(),
                is_owner: false,
                group_ids: Vec::new(),
            },
            ClanMember {
                user_id: id(8),
                nickname: Some("kai".to_string()),
                joined_at: chrono::Utc::now(),
                is_owner: false,
                group_ids: Vec::new(),
            },
        ]));
        let labels: Vec<String> = clan_owned
            .recipient_choices()
            .into_iter()
            .map(|choice| choice.label)
            .collect();
        // Everyone first, then the custom groups, then members; not the
        // viewer, and not member 7, who holds the map's own grant.
        assert_eq!(
            labels,
            [
                crate::i18n::t!("clan-maps-everyone"),
                "Scouts".to_string(),
                "kai".to_string()
            ]
        );
    }

    #[test]
    fn a_member_owned_maps_separate_actions_are_its_own() {
        let mut clan_owned = dialog(MapOwnership::Clan);
        clan_owned.clan_owner = true;
        assert!(
            clan_owned
                .separate_actions()
                .contains(&action::SHARE_AREA_EXTERNALLY)
        );
        let member_owned = dialog(MapOwnership::Members);
        assert_eq!(
            member_owned.separate_actions(),
            [
                action::COPY_AREA,
                action::DELETE_AREA,
                action::CREATE_MEMBER_OWNED_SECRET
            ]
        );
    }

    #[test]
    fn put_in_a_clan_names_each_friend_once() {
        let node = |grantee: u128, nickname: Option<&str>| -> GrantTreeNode {
            serde_json::from_value(serde_json::json!({
                "id": id(grantee + 1000),
                "owner_id": id(1),
                "grantor_id": id(1),
                "grantee_id": id(grantee),
                "area_id": MAP,
                "can_edit": false,
                "can_reshare": false,
                "can_copy": false,
                "created_at": "2026-10-07T00:00:00Z",
                "updated_at": "2026-10-07T00:00:00Z",
                "depth": 0,
                "grantee_nickname": nickname,
            }))
            .unwrap()
        };
        let names = shared_with(&[
            node(3, Some("kai")),
            node(4, Some("bo")),
            node(3, Some("kai")),
        ]);
        assert_eq!(names, ["kai", "bo"]);
    }
}
