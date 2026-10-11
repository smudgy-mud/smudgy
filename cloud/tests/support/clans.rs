//! Clans: `/clans*`, `/clan-invitations/*`, `/me/clan-invitations`.
//! Fidelity reference: the Cloudflare service's docs/clans.md §1–§5 and
//! `src/library/clans/{membership,invitations,groups,grants}.ts`, with the
//! authority of `src/library/authz/clan.ts`: owners hold every action; a
//! group's creator governs it while a member; anyone else holds what the
//! grants reaching them give. Grants carry inline actions only (no roles), and
//! a delegation never hands out clan administration, group membership or
//! further delegation. A recipient holds one grant per scope: writes add to it
//! and change sets edit it, and each action a delegate adds records the
//! delegation it came through, going when that delegation stops handing it
//! out. A new clan starts with its founding grants: All clan members read the
//! member directory. Resource permissions are scoped.
//!
//! Every route needs a verified email. Every refusal that could reveal
//! something is the uniform 404; the rest are the clan 409s.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post, put};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::http::{
    Handled, authenticate, bad_request, conflict, created, gate_verified, not_found, ok,
};
use super::mock_server::{MockHandle, TestUser};
use super::state::MockState;

pub type Shared = Arc<Mutex<MockState>>;

const CLAN_WIDE: [&str; 8] = [
    "clan.edit_profile",
    "clan.read_members",
    "clan.invite",
    "clan.revoke_invitation",
    "clan.remove_member",
    "group.create",
    "atlas.create",
    "package.create",
];
const GROUP: [&str; 4] = [
    "group.rename",
    "group.delete",
    "group.assign",
    "group.inspect_assignments",
];
const GRANT: [&str; 2] = ["grant.inspect", "grant.manage"];

/// Every action, in the order responses list them.
const CLAN_ACTIONS: [&str; 45] = [
    "clan.edit_profile",
    "clan.read_members",
    "clan.invite",
    "clan.revoke_invitation",
    "clan.remove_member",
    "group.create",
    "atlas.create",
    "package.create",
    "group.rename",
    "group.delete",
    "group.assign",
    "group.inspect_assignments",
    "grant.inspect",
    "grant.manage",
    "atlas.read",
    "atlas.rename",
    "atlas.delete",
    "atlas.accept_filing",
    "atlas.accept_transfer",
    "area.create",
    "area.create_member_owned",
    "area.read",
    "area.add",
    "area.edit",
    "area.remove_content",
    "area.rename",
    "area.refile",
    "area.delete",
    "area.copy",
    "area.share_external",
    "secret.create_member_owned",
    "secret.create_clan_owned",
    "secret.read",
    "secret.add",
    "secret.edit",
    "secret.remove_content",
    "secret.manage_access",
    "secret.copy",
    "package.read",
    "package.edit_draft",
    "package.edit_metadata",
    "package.manage_availability",
    "package.publish",
    "package.retire",
    "package.delete",
];

/// The actions on one package (`package.create` applies to the clan).
const PACKAGE: [&str; 7] = [
    "package.read",
    "package.edit_draft",
    "package.edit_metadata",
    "package.manage_availability",
    "package.publish",
    "package.retire",
    "package.delete",
];

/// Clan administration, group membership and delegation: never in a
/// delegation's `may_grant`, and never given by a delegated grant.
fn administration(action: &str) -> bool {
    action.starts_with("clan.") || action.starts_with("group.") || action.starts_with("grant.")
}

/// Map, atlas and Secret actions, which reach groups, and members only on
/// one map.
fn grants_map_access(action: &str) -> bool {
    action.starts_with("area.") || action.starts_with("atlas.") || action.starts_with("secret.")
}

/// Whether a scope names exactly one map, where map access may go to a
/// member.
fn names_one_map(scope: &ClanGrantScope) -> bool {
    matches!(scope, ClanGrantScope::Areas(ids) if ids.len() == 1)
}

pub fn ordered(actions: &BTreeSet<String>) -> Vec<&'static str> {
    CLAN_ACTIONS
        .iter()
        .copied()
        .filter(|action| actions.contains(*action))
        .collect()
}

const ATLAS: [&str; 7] = [
    "atlas.read",
    "atlas.rename",
    "atlas.delete",
    "atlas.accept_filing",
    "atlas.accept_transfer",
    "area.create",
    "area.create_member_owned",
];

/// Accepting a member's transfer applies to a folder, for a map filed
/// there, and to the clan, for an atlas.
pub const ACCEPT_TRANSFER: &str = "atlas.accept_transfer";
const AREA: [&str; 9] = [
    "area.read",
    "area.add",
    "area.edit",
    "area.remove_content",
    "area.rename",
    "area.refile",
    "area.delete",
    "area.copy",
    "area.share_external",
];
const SECRET: [&str; 8] = [
    "secret.create_member_owned",
    "secret.create_clan_owned",
    "secret.read",
    "secret.add",
    "secret.edit",
    "secret.remove_content",
    "secret.manage_access",
    "secret.copy",
];

/// Whether `action` applies to `resource` (the server's `appliesTo`).
fn applies(action: &str, resource: ClanResource) -> bool {
    match resource {
        ClanResource::Clan => {
            CLAN_WIDE.contains(&action) || GRANT.contains(&action) || action == ACCEPT_TRANSFER
        }
        ClanResource::Group(_) => GROUP.contains(&action),
        ClanResource::Atlas(_) => ATLAS.contains(&action) || GRANT.contains(&action),
        ClanResource::Area { .. } => {
            AREA.contains(&action) || SECRET.contains(&action) || GRANT.contains(&action)
        }
        ClanResource::Package(_) => PACKAGE.contains(&action) || GRANT.contains(&action),
    }
}

/// Whether a scope kind takes `action` inline.
fn takes_inline(kind: &str, action: &str) -> bool {
    if action == "atlas.delete" {
        // Ownership authority alone: no grant carries it.
        return false;
    }
    match kind {
        "clan" => CLAN_WIDE.contains(&action) || GROUP.contains(&action),
        "groups" => GROUP.contains(&action),
        "atlases" => {
            ATLAS.contains(&action)
                || AREA.contains(&action)
                || SECRET.contains(&action)
                || GRANT.contains(&action)
        }
        "areas" => {
            (AREA.contains(&action)
                && action != "area.create"
                && action != "area.create_member_owned")
                || SECRET.contains(&action)
                || GRANT.contains(&action)
        }
        "packages" => PACKAGE.contains(&action) || GRANT.contains(&action),
        _ => false,
    }
}

/// Whether a scope covers a resource.
pub fn covers(scope: &ClanGrantScope, resource: ClanResource) -> bool {
    match (scope, resource) {
        (ClanGrantScope::Clan, _) => true,
        (ClanGrantScope::Groups(ids), ClanResource::Group(id))
        | (ClanGrantScope::Atlases(ids), ClanResource::Atlas(id))
        | (ClanGrantScope::Areas(ids), ClanResource::Area { id, .. })
        | (ClanGrantScope::Packages(ids), ClanResource::Package(id)) => ids.contains(&id),
        (
            ClanGrantScope::Atlases(ids),
            ClanResource::Area {
                atlas: Some(atlas), ..
            },
        ) => ids.contains(&atlas),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberStatus {
    Active,
    Left,
    Removed,
}

#[derive(Debug, Clone)]
pub struct ClanMemberRecord {
    pub status: MemberStatus,
    pub joined_at: DateTime<Utc>,
    /// Joining order; ties on `joined_at` never reorder the directory.
    pub joined_seq: u64,
}

#[derive(Debug, Clone)]
pub struct ClanGroupRecord {
    pub id: Uuid,
    pub name: String,
    pub color: Option<String>,
    /// `"owners"`, `"members"`, or `None` for a custom group.
    pub builtin: Option<&'static str>,
    /// Who created a custom group and governs it while a member. Cleared
    /// when they leave or are removed: rejoining does not restore it.
    pub creator: Option<Uuid>,
}

/// A package the clan owns, as grant scopes and the access index see it.
#[derive(Debug, Clone)]
pub struct ClanPackageRecord {
    pub id: Uuid,
    pub name: String,
    pub is_public: bool,
}

/// A grant's recipient: one member, or one group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClanRecipient {
    User(Uuid),
    Group(Uuid),
}

/// What a grant covers: the whole clan, some groups, some folders and the
/// maps filed in them, some maps, or some packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClanGrantScope {
    Clan,
    Groups(BTreeSet<Uuid>),
    Atlases(BTreeSet<Uuid>),
    Areas(BTreeSet<Uuid>),
    Packages(BTreeSet<Uuid>),
}

impl ClanGrantScope {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Clan => "clan",
            Self::Groups(_) => "groups",
            Self::Atlases(_) => "atlases",
            Self::Areas(_) => "areas",
            Self::Packages(_) => "packages",
        }
    }

    /// The listed IDs; `None` on a clan scope.
    pub fn ids(&self) -> Option<&BTreeSet<Uuid>> {
        match self {
            Self::Clan => None,
            Self::Groups(ids) | Self::Atlases(ids) | Self::Areas(ids) | Self::Packages(ids) => {
                Some(ids)
            }
        }
    }

    pub fn ids_mut(&mut self) -> Option<&mut BTreeSet<Uuid>> {
        match self {
            Self::Clan => None,
            Self::Groups(ids) | Self::Atlases(ids) | Self::Areas(ids) | Self::Packages(ids) => {
                Some(ids)
            }
        }
    }

    pub fn view(&self) -> Value {
        match self.ids() {
            None => json!({ "kind": "clan" }),
            Some(ids) => json!({ "kind": self.kind(), "ids": ids }),
        }
    }
}

/// A resource clan actions apply to, as grant coverage sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClanResource {
    Clan,
    Group(Uuid),
    Atlas(Uuid),
    /// A map with the folder it is filed in (none for a Member-owned map
    /// left unfiled); `member_owned` for a map only grants naming it alone
    /// reach.
    Area {
        id: Uuid,
        atlas: Option<Uuid>,
        member_owned: bool,
    },
    /// A package the clan owns.
    Package(Uuid),
}

/// A recipient's one grant over one scope (docs/clans.md §5.3).
#[derive(Debug, Clone)]
pub struct ClanGrantRecord {
    pub id: Uuid,
    pub recipient: ClanRecipient,
    pub scope: ClanGrantScope,
    /// Every action it holds: its own, and those that came through a
    /// delegation.
    pub actions: BTreeSet<String>,
    /// For each delegation actions came through, those actions. An action of
    /// `actions` none of them lists is the grant's own.
    pub delegated: BTreeMap<Uuid, BTreeSet<String>>,
    /// What a grant carrying `grant.manage` may hand out.
    pub may_grant: BTreeSet<String>,
    pub issuer_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Creation order; ties on `created_at` never reorder the list, and it
    /// stands in for the server's ID tie-break among grants of one instant.
    pub seq: u64,
}

/// Where an action a write adds comes from: the grant's own (an owner of the
/// clan or of a Member-owned map added it), or through a delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Own,
    Through(Uuid),
}

/// `actions` as an owner adds them: each the grant's own.
pub fn own<I, S>(actions: I) -> Vec<(String, Source)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    actions
        .into_iter()
        .map(|action| (action.as_ref().to_string(), Source::Own))
        .collect()
}

impl ClanGrantRecord {
    /// Whether it delegates: it carries `grant.manage`, which only owners
    /// add, so it is always the grant's own.
    pub fn delegates(&self) -> bool {
        self.actions.contains("grant.manage")
    }

    /// Whether `action` is the grant's own: held, through no delegation.
    pub fn owns(&self, action: &str) -> bool {
        self.actions.contains(action)
            && !self
                .delegated
                .values()
                .any(|through| through.contains(action))
    }

    /// The delegations `action` came through.
    pub fn sources(&self, action: &str) -> Vec<Uuid> {
        self.delegated
            .iter()
            .filter(|(_, through)| through.contains(action))
            .map(|(delegation, _)| *delegation)
            .collect()
    }

    /// Adds `action` from `source`. An owner's addition is the grant's own,
    /// ending any delegation it came through; a delegate's leaves one of
    /// the grant's own as it is, and otherwise records the delegation beside
    /// any other it came through.
    fn add(&mut self, action: &str, source: Source) {
        match source {
            Source::Own => {
                self.actions.insert(action.to_string());
                for through in self.delegated.values_mut() {
                    through.remove(action);
                }
                self.delegated.retain(|_, through| !through.is_empty());
            }
            Source::Through(delegation) => {
                if self.owns(action) {
                    return;
                }
                self.actions.insert(action.to_string());
                self.delegated
                    .entry(delegation)
                    .or_default()
                    .insert(action.to_string());
            }
        }
    }

    /// Removes `action`, whoever added it.
    fn remove(&mut self, action: &str) {
        self.actions.remove(action);
        for through in self.delegated.values_mut() {
            through.remove(action);
        }
        self.delegated.retain(|_, through| !through.is_empty());
    }

    /// Drops what came through `delegation` that `keep` leaves out: an action
    /// that came through nothing else goes.
    fn retire(&mut self, delegation: Uuid, keep: &BTreeSet<String>) {
        let Some(through) = self.delegated.get_mut(&delegation) else {
            return;
        };
        let dropped: Vec<String> = through.difference(keep).cloned().collect();
        if dropped.is_empty() {
            return;
        }
        through.retain(|action| keep.contains(action));
        if through.is_empty() {
            self.delegated.remove(&delegation);
        }
        self.updated_at = Utc::now();
        for action in dropped {
            if !self
                .delegated
                .values()
                .any(|through| through.contains(&action))
            {
                self.actions.remove(&action);
            }
        }
    }

    /// Whether it holds what `other` holds: the same actions, from the same
    /// sources, and the same ceiling.
    fn holds_as(&self, other: &Self) -> bool {
        self.actions == other.actions
            && self.delegated == other.delegated
            && self.may_grant == other.may_grant
    }
}

#[derive(Debug, Clone)]
pub struct ClanInvitationRecord {
    pub id: Uuid,
    pub user_id: Uuid,
    pub inviter_id: Uuid,
    pub group_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub seq: u64,
}

#[derive(Debug, Clone)]
pub struct ClanRecord {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub dissolved: bool,
    /// A dissolution has been checked and has not committed: no map enters
    /// the clan (docs/clans.md §2).
    pub dissolving: bool,
    /// The clan's Library committed its dissolution, and the directory has
    /// yet to mark it dissolved (docs/clans.md §10).
    pub unrecorded: bool,
    pub members: BTreeMap<Uuid, ClanMemberRecord>,
    pub owners: BTreeSet<Uuid>,
    pub groups: Vec<ClanGroupRecord>,
    /// `(group, user, added order)`.
    pub group_members: Vec<(Uuid, Uuid, u64)>,
    pub grants: Vec<ClanGrantRecord>,
    pub invitations: Vec<ClanInvitationRecord>,
    /// How many maps the clan owns; dissolving needs none.
    pub owned_maps: usize,
    /// The packages the clan owns.
    pub packages: Vec<ClanPackageRecord>,
}

#[derive(Debug, Default)]
pub struct ClanStore {
    pub clans: BTreeMap<Uuid, ClanRecord>,
}

impl ClanRecord {
    fn active(&self) -> bool {
        !self.dissolved
    }

    fn is_member(&self, user: Uuid) -> bool {
        self.active()
            && self
                .members
                .get(&user)
                .is_some_and(|member| member.status == MemberStatus::Active)
    }

    fn is_owner(&self, user: Uuid) -> bool {
        self.is_member(user) && self.owners.contains(&user)
    }

    fn owner_count(&self) -> usize {
        self.owners.len()
    }

    fn active_members(&self) -> usize {
        self.members
            .values()
            .filter(|member| member.status == MemberStatus::Active)
            .count()
    }

    fn group(&self, id: Uuid) -> Option<&ClanGroupRecord> {
        self.groups.iter().find(|group| group.id == id)
    }

    fn builtin(&self, kind: &str) -> Uuid {
        self.groups
            .iter()
            .find(|group| group.builtin == Some(kind))
            .map(|group| group.id)
            .expect("built-in groups exist")
    }

    /// The groups `user` is in, built-ins included.
    fn groups_of(&self, user: Uuid) -> BTreeSet<Uuid> {
        let mut groups = BTreeSet::new();
        if !self.is_member(user) {
            return groups;
        }
        groups.insert(self.builtin("members"));
        if self.is_owner(user) {
            groups.insert(self.builtin("owners"));
        }
        for (group, member, _) in &self.group_members {
            if *member == user {
                groups.insert(*group);
            }
        }
        groups
    }

    /// The grants reaching `user`, directly or through a group.
    fn held(&self, user: Uuid) -> Vec<&ClanGrantRecord> {
        if !self.is_member(user) {
            return Vec::new();
        }
        let groups = self.groups_of(user);
        self.grants
            .iter()
            .filter(|grant| match grant.recipient {
                ClanRecipient::User(id) => id == user,
                ClanRecipient::Group(id) => groups.contains(&id),
            })
            .collect()
    }

    /// The actions `user` holds on the whole clan, from clan-scoped grants.
    fn clan_wide(&self, user: Uuid) -> BTreeSet<String> {
        if !self.is_member(user) {
            return BTreeSet::new();
        }
        if self.is_owner(user) {
            return CLAN_ACTIONS.iter().map(ToString::to_string).collect();
        }
        let mut actions = BTreeSet::new();
        for grant in self.held(user) {
            if grant.scope != ClanGrantScope::Clan {
                continue;
            }
            for action in &grant.actions {
                if takes_inline("clan", action)
                    && (matches!(grant.recipient, ClanRecipient::Group(_))
                        || !grants_map_access(action))
                {
                    actions.insert(action.clone());
                }
            }
        }
        if actions.contains("grant.manage") {
            actions.insert("grant.inspect".to_string());
        }
        actions
    }

    /// Whether `user` holds a clan-level action (one that applies to the
    /// clan itself).
    fn may(&self, user: Uuid, action: &str) -> bool {
        (CLAN_WIDE.contains(&action) || GRANT.contains(&action))
            && self.clan_wide(user).contains(action)
    }

    /// The actions `user` holds on one group.
    fn group_actions(&self, user: Uuid, group: Uuid) -> BTreeSet<String> {
        let mut actions = BTreeSet::new();
        if !self.is_member(user) {
            return actions;
        }
        if self.is_owner(user) || self.created(user, group) {
            return GROUP.iter().map(ToString::to_string).collect();
        }
        for grant in self.held(user) {
            actions.extend(self.contribution(grant, ClanResource::Group(group)));
        }
        actions
    }

    /// Whether `user`, an active member, created `group` and so governs it.
    fn created(&self, user: Uuid, group: Uuid) -> bool {
        self.is_member(user)
            && self
                .group(group)
                .is_some_and(|record| record.builtin.is_none() && record.creator == Some(user))
    }

    fn may_group(&self, user: Uuid, group: Uuid, action: &str) -> bool {
        self.group_actions(user, group).contains(action)
    }

    /// Group IDs in display order: built-ins first, then by name.
    fn group_order(&self, ids: &BTreeSet<Uuid>) -> Vec<Uuid> {
        let mut groups: Vec<&ClanGroupRecord> = self
            .groups
            .iter()
            .filter(|group| ids.contains(&group.id))
            .collect();
        groups.sort_by_key(|group| group_sort_key(group));
        groups.into_iter().map(|group| group.id).collect()
    }

    fn name_taken(&self, name: &str, except: Option<Uuid>) -> bool {
        self.groups.iter().any(|group| {
            Some(group.id) != except && group.name.to_lowercase() == name.to_lowercase()
        })
    }

    fn summary(&self, user: Uuid) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "description": self.description,
            "created_at": self.created_at,
            "member_count": self.active_members(),
            "is_owner": self.is_owner(user),
            "group_ids": self.group_order(&self.groups_of(user)),
            "actions": ordered(&self.clan_wide(user)),
        })
    }

    fn member_view(&self, st: &MockState, caller: Uuid, user: Uuid) -> Value {
        let member = &self.members[&user];
        let visible: BTreeSet<Uuid> = self
            .group_members
            .iter()
            .filter(|(group, member, _)| {
                *member == user && self.may_group(caller, *group, "group.inspect_assignments")
            })
            .map(|(group, _, _)| *group)
            .collect();
        json!({
            "user_id": user,
            "nickname": nickname(st, user),
            "joined_at": member.joined_at,
            "is_owner": self.owners.contains(&user),
            "group_ids": self.group_order(&visible),
        })
    }

    fn group_view(&self, caller: Uuid, group: &ClanGroupRecord) -> Value {
        json!({
            "id": group.id,
            "name": group.name,
            "color": group.color,
            "builtin": group.builtin,
            "is_member": self.groups_of(caller).contains(&group.id),
            "created_by_me": self.created(caller, group.id),
            "actions": ordered(&self.group_actions(caller, group.id)),
        })
    }

    fn invitation_view(&self, st: &MockState, invitation: &ClanInvitationRecord) -> Value {
        let groups: BTreeSet<Uuid> = invitation.group_ids.iter().copied().collect();
        json!({
            "id": invitation.id,
            "clan_id": self.id,
            "user_id": invitation.user_id,
            "nickname": nickname(st, invitation.user_id),
            "inviter_id": invitation.inviter_id,
            "inviter_nickname": nickname(st, invitation.inviter_id),
            "group_ids": self.group_order(&groups),
            "created_at": invitation.created_at,
        })
    }

    /// Ends a membership: ownership, custom group memberships, direct grants
    /// and the invitations the member issued all go.
    fn depart(&mut self, user: Uuid, status: MemberStatus) {
        if let Some(member) = self.members.get_mut(&user) {
            member.status = status;
        }
        self.owners.remove(&user);
        self.group_members.retain(|(_, member, _)| *member != user);
        for group in &mut self.groups {
            if group.creator == Some(user) {
                group.creator = None;
            }
        }
        let direct: Vec<Uuid> = self
            .grants
            .iter()
            .filter(|grant| grant.recipient == ClanRecipient::User(user))
            .map(|grant| grant.id)
            .collect();
        self.remove_grants(&direct);
        self.invitations
            .retain(|invitation| invitation.inviter_id != user);
    }
}

/// Whether `user` is the last owner of an active clan, which refuses the
/// account's deletion with 409 `last_owner` (docs/architecture.md §9.4).
pub fn last_owner_anywhere(st: &MockState, user: Uuid) -> bool {
    st.clans
        .clans
        .values()
        .any(|clan| clan.is_owner(user) && clan.owner_count() == 1)
}

/// An account's departure from every clan, as its deletion runs it: each
/// membership ends as leaving does, and its pending invitations are
/// declined. Grants it issued in a clan stay with the clan.
pub fn depart_everywhere(st: &mut MockState, user: Uuid) {
    for clan in st.clans.clans.values_mut() {
        if clan.is_member(user) {
            clan.depart(user, MemberStatus::Left);
        }
        clan.invitations
            .retain(|invitation| invitation.user_id != user);
    }
    super::shares::sweep_outside_shares(st);
}

/// Which folder a map is filed in, as grant scopes see it.
pub type Placement<'a> = &'a dyn Fn(Uuid) -> Option<Uuid>;

impl ClanRecord {
    /// Whether `user` is an active member of this active clan.
    pub fn has_member(&self, user: Uuid) -> bool {
        self.is_member(user)
    }

    /// Whether `user` is one of its owners.
    pub fn has_owner(&self, user: Uuid) -> bool {
        self.is_owner(user)
    }

    /// The groups `user` is in, built-ins included.
    pub fn member_groups(&self, user: Uuid) -> BTreeSet<Uuid> {
        self.groups_of(user)
    }

    /// The actions `user` holds on one group.
    pub fn group_actions_of(&self, user: Uuid, group: Uuid) -> BTreeSet<String> {
        self.group_actions(user, group)
    }

    /// Whether the clan owns package `id`.
    pub fn has_package(&self, id: Uuid) -> bool {
        self.packages.iter().any(|package| package.id == id)
    }

    /// Whether the clan has group `id`.
    pub fn has_group(&self, id: Uuid) -> bool {
        self.group(id).is_some()
    }

    /// The active members in directory order: owners first, then by joining.
    pub fn directory(&self) -> Vec<Uuid> {
        let mut members: Vec<(bool, u64, Uuid)> = self
            .members
            .iter()
            .filter(|(_, member)| member.status == MemberStatus::Active)
            .map(|(user, member)| (!self.owners.contains(user), member.joined_seq, *user))
            .collect();
        members.sort();
        members.into_iter().map(|(_, _, user)| user).collect()
    }

    /// What one grant gives on a resource: its actions that apply there. An
    /// action that came through a delegation gives something only while one
    /// of its delegations still hands it out there, and never clan, group or
    /// grant administration. Map, folder and Secret actions reach only group
    /// recipients.
    pub fn contribution(
        &self,
        grant: &ClanGrantRecord,
        resource: ClanResource,
    ) -> BTreeSet<String> {
        let mut given = BTreeSet::new();
        if !covers(&grant.scope, resource) {
            return given;
        }
        for action in &grant.actions {
            if !takes_inline(grant.scope.kind(), action)
                || !applies(action, resource)
                || (matches!(grant.recipient, ClanRecipient::User(_))
                    && grants_map_access(action)
                    && !names_one_map(&grant.scope))
            {
                continue;
            }
            let live = grant.owns(action)
                || (!administration(action)
                    && grant
                        .sources(action)
                        .into_iter()
                        .any(|delegation| self.hands_out(delegation, action, resource)));
            if live {
                given.insert(action.clone());
            }
        }
        given
    }

    /// Whether `delegation` hands out `action` on `resource`: it carries
    /// `grant.manage`, its ceiling holds the action, and it covers the
    /// resource.
    fn hands_out(&self, delegation: Uuid, action: &str, resource: ClanResource) -> bool {
        self.grants
            .iter()
            .find(|grant| grant.id == delegation)
            .is_some_and(|grant| {
                grant.delegates()
                    && takes_inline(grant.scope.kind(), "grant.manage")
                    && grant.may_grant.contains(action)
                    && covers(&grant.scope, resource)
            })
    }

    /// Narrows the grants of a map made Member-owned (docs/clans.md §7.3):
    /// grants naming it beside other maps lose it, and join a grant their
    /// scope then matches; grants naming it alone keep their own actions in
    /// `allowed` when `area.read` is among them, and go otherwise; every
    /// action that came through a delegation goes.
    pub fn narrow_to_member_owned(&mut self, area: Uuid, allowed: &[&str]) {
        let mut doomed = Vec::new();
        let mut undelegated = Vec::new();
        for grant in &mut self.grants {
            let ClanGrantScope::Areas(ids) = &mut grant.scope else {
                continue;
            };
            if !ids.contains(&area) {
                continue;
            }
            if ids.len() > 1 {
                ids.remove(&area);
                continue;
            }
            if grant.delegates() {
                undelegated.push(grant.id);
            }
            let own: BTreeSet<String> = grant
                .actions
                .iter()
                .filter(|action| grant.owns(action) && allowed.contains(&action.as_str()))
                .cloned()
                .collect();
            grant.actions = own;
            grant.delegated.clear();
            grant.may_grant.clear();
            if !grant.actions.contains("area.read") {
                doomed.push(grant.id);
            }
        }
        for delegation in undelegated {
            self.retire_through(delegation, &BTreeSet::new());
        }
        self.delete(&doomed);
        self.join();
        self.check();
    }

    /// `user`'s actions on a resource. Owners hold every action that applies,
    /// but nothing on a Member-owned map, which only grants naming it alone
    /// reach.
    pub fn actions_on(&self, user: Uuid, resource: ClanResource) -> BTreeSet<String> {
        let mut actions = BTreeSet::new();
        if !self.is_member(user) {
            return actions;
        }
        if let ClanResource::Area {
            id,
            member_owned: true,
            ..
        } = resource
        {
            for grant in self.held(user) {
                if grant.scope == ClanGrantScope::Areas(BTreeSet::from([id])) {
                    actions.extend(self.contribution(grant, resource));
                }
            }
            return actions;
        }
        if self.is_owner(user) {
            actions.extend(
                CLAN_ACTIONS
                    .iter()
                    .filter(|action| applies(action, resource))
                    .map(ToString::to_string),
            );
        } else {
            for grant in self.held(user) {
                actions.extend(self.contribution(grant, resource));
            }
        }
        if actions.contains("grant.manage") {
            actions.insert("grant.inspect".to_string());
        }
        actions
    }

    /// Whether `user` holds `action` anywhere: on the clan, a group, a folder
    /// or a map.
    pub fn holds_anywhere(&self, user: Uuid, action: &str) -> bool {
        if !self.is_member(user) {
            return false;
        }
        if self.is_owner(user) {
            return true;
        }
        self.held(user).into_iter().any(|grant| {
            (!grants_map_access(action)
                || matches!(grant.recipient, ClanRecipient::Group(_))
                || names_one_map(&grant.scope))
                && grant.actions.contains(action)
        })
    }

    /// Whether `inner` lies within `outer`.
    fn within(inner: &ClanGrantScope, outer: &ClanGrantScope, placement: Placement<'_>) -> bool {
        match (outer, inner) {
            (ClanGrantScope::Clan, _) => true,
            (ClanGrantScope::Areas(outer), ClanGrantScope::Areas(inner))
            | (ClanGrantScope::Atlases(outer), ClanGrantScope::Atlases(inner))
            | (ClanGrantScope::Packages(outer), ClanGrantScope::Packages(inner)) => {
                inner.is_subset(outer)
            }
            (ClanGrantScope::Atlases(outer), ClanGrantScope::Areas(inner)) => inner
                .iter()
                .all(|area| placement(*area).is_some_and(|atlas| outer.contains(&atlas))),
            _ => false,
        }
    }

    /// Whether `user` may see `grant`: owners every grant, members their own
    /// and their groups', and holders of `grant.inspect` or `grant.manage`
    /// those within it. Grant actions never come through a delegation. A
    /// Member-owned map's grants are its owners' to see, which callers decide
    /// before asking.
    pub fn may_inspect(
        &self,
        user: Uuid,
        grant: &ClanGrantRecord,
        placement: Placement<'_>,
    ) -> bool {
        if !self.is_member(user) {
            return false;
        }
        if self.is_owner(user) {
            return true;
        }
        let groups = self.groups_of(user);
        let reaches = match grant.recipient {
            ClanRecipient::User(id) => id == user,
            ClanRecipient::Group(id) => groups.contains(&id),
        };
        reaches
            || self.held(user).into_iter().any(|held| {
                takes_inline(held.scope.kind(), "grant.inspect")
                    && (held.actions.contains("grant.inspect") || held.delegates())
                    && Self::within(&grant.scope, &held.scope, placement)
            })
    }

    /// The delegation an action `user` adds over `scope` comes through: the
    /// oldest delegation reaching them whose scope `scope` lies within and
    /// whose ceiling holds `action` (docs/clans.md §1.2). `None` when no
    /// delegation of theirs lets them add or remove it there.
    pub(super) fn delegation_through(
        &self,
        user: Uuid,
        scope: &ClanGrantScope,
        action: &str,
        placement: Placement<'_>,
    ) -> Option<Uuid> {
        self.held(user)
            .into_iter()
            .filter(|held| {
                held.delegates()
                    && takes_inline(held.scope.kind(), "grant.manage")
                    && held.may_grant.contains(action)
                    && Self::within(scope, &held.scope, placement)
            })
            .min_by_key(|held| (held.created_at, held.seq))
            .map(|held| held.id)
    }

    /// Grant `id`.
    pub fn grant(&self, id: Uuid) -> Option<&ClanGrantRecord> {
        self.grants.iter().find(|grant| grant.id == id)
    }

    /// The grant `recipient` holds over exactly `scope`.
    pub fn grant_over(
        &self,
        recipient: ClanRecipient,
        scope: &ClanGrantScope,
    ) -> Option<&ClanGrantRecord> {
        self.grants
            .iter()
            .find(|grant| grant.recipient == recipient && grant.scope == *scope)
    }

    /// Adds `added` and `ceiling` to `recipient`'s grant over `scope`,
    /// creating it, issued by `issuer`, when they hold none. Returns its ID
    /// and whether it is new.
    pub fn upsert(
        &mut self,
        recipient: ClanRecipient,
        scope: ClanGrantScope,
        added: &[(String, Source)],
        ceiling: &BTreeSet<String>,
        issuer: Uuid,
        seq: u64,
    ) -> (Uuid, bool) {
        let now = Utc::now();
        let existing = self
            .grants
            .iter()
            .position(|grant| grant.recipient == recipient && grant.scope == scope);
        let created = existing.is_none();
        let at = existing.unwrap_or_else(|| {
            self.grants.push(ClanGrantRecord {
                id: Uuid::new_v4(),
                recipient,
                scope,
                actions: BTreeSet::new(),
                delegated: BTreeMap::new(),
                may_grant: BTreeSet::new(),
                issuer_id: issuer,
                created_at: now,
                updated_at: now,
                seq,
            });
            self.grants.len() - 1
        });
        let grant = &mut self.grants[at];
        let before = grant.clone();
        for (action, source) in added {
            grant.add(action, *source);
        }
        grant.may_grant.extend(ceiling.iter().cloned());
        if !created && !grant.holds_as(&before) {
            grant.updated_at = now;
        }
        let id = grant.id;
        self.check();
        (id, created)
    }

    /// Deletes grants, with every action that came through them; a grant
    /// left with no action goes too.
    pub fn remove_grants(&mut self, ids: &[Uuid]) {
        self.delete(ids);
        self.check();
    }

    fn delete(&mut self, ids: &[Uuid]) {
        let mut doomed: BTreeSet<Uuid> = ids.iter().copied().collect();
        while !doomed.is_empty() {
            self.grants.retain(|grant| !doomed.contains(&grant.id));
            for grant in &mut self.grants {
                for delegation in &doomed {
                    grant.retire(*delegation, &BTreeSet::new());
                }
            }
            doomed = self
                .grants
                .iter()
                .filter(|grant| grant.actions.is_empty())
                .map(|grant| grant.id)
                .collect();
        }
    }

    /// Drops every action that came through `delegation` that `keep` leaves
    /// out, as removing `grant.manage` (keeping nothing) or narrowing its
    /// ceiling does; a grant left with no action goes.
    fn retire_through(&mut self, delegation: Uuid, keep: &BTreeSet<String>) {
        for grant in &mut self.grants {
            grant.retire(delegation, keep);
        }
        let emptied: Vec<Uuid> = self
            .grants
            .iter()
            .filter(|grant| grant.actions.is_empty())
            .map(|grant| grant.id)
            .collect();
        self.delete(&emptied);
    }

    /// Joins each grant whose scope became another's, for the same
    /// recipient, into the older one (docs/clans.md §5.3): it keeps its ID
    /// and gains the other's actions with where each came from (one either
    /// held as its own stays its own), its ceiling, and the actions that
    /// came through it.
    fn join(&mut self) {
        loop {
            let pair = self.grants.iter().find_map(|grant| {
                self.grants
                    .iter()
                    .find(|other| {
                        other.recipient == grant.recipient
                            && other.scope == grant.scope
                            && (other.created_at, other.seq) > (grant.created_at, grant.seq)
                    })
                    .map(|newer| (grant.id, newer.id))
            });
            let Some((kept, gone)) = pair else {
                return;
            };
            let at = self
                .grants
                .iter()
                .position(|grant| grant.id == gone)
                .expect("found above");
            let gone = self.grants.remove(at);
            let record = self
                .grants
                .iter_mut()
                .find(|grant| grant.id == kept)
                .expect("found above");
            for action in &gone.actions {
                if gone.owns(action) {
                    record.add(action, Source::Own);
                }
                for delegation in gone.sources(action) {
                    record.add(action, Source::Through(delegation));
                }
            }
            record.may_grant.extend(gone.may_grant.iter().cloned());
            record.updated_at = Utc::now();
            // What came through the joined grant came through the kept one.
            for grant in &mut self.grants {
                if let Some(through) = grant.delegated.remove(&gone.id) {
                    grant.delegated.entry(kept).or_default().extend(through);
                    grant.updated_at = Utc::now();
                }
            }
        }
    }

    /// The grant invariants the service's schema keeps: one grant per
    /// recipient and scope, none without an action, a ceiling exactly on a
    /// grant carrying `grant.manage`, and every delegated action within a
    /// live delegation's ceiling. Grant writes check them, so a write the
    /// mock lets through wrongly surfaces in the tests.
    pub fn check(&self) {
        for grant in &self.grants {
            assert!(
                !grant.actions.is_empty(),
                "a grant with no action: {grant:?}"
            );
            assert_eq!(
                self.grants
                    .iter()
                    .filter(|other| other.recipient == grant.recipient && other.scope == grant.scope)
                    .count(),
                1,
                "two grants to one recipient over one scope: {grant:?}"
            );
            assert_eq!(
                grant.delegates(),
                !grant.may_grant.is_empty(),
                "a ceiling goes with grant.manage, and only with it: {grant:?}"
            );
            for (delegation, through) in &grant.delegated {
                assert!(!through.is_empty() && through.is_subset(&grant.actions));
                let delegation = self
                    .grants
                    .iter()
                    .find(|other| other.id == *delegation)
                    .unwrap_or_else(|| {
                        panic!("a delegated action outlived its delegation: {grant:?}")
                    });
                assert!(
                    delegation.owns("grant.manage") && through.is_subset(&delegation.may_grant),
                    "a delegated action beyond its delegation: {grant:?}"
                );
            }
        }
    }

    /// Takes a deleted group, folder or map out of every grant scope; a grant
    /// left with no IDs goes, with every action that came through it, and
    /// one left naming what another grant to its recipient names joins it.
    pub fn drop_target(&mut self, target: Uuid) {
        let mut emptied = Vec::new();
        for grant in &mut self.grants {
            if let Some(ids) = grant.scope.ids_mut()
                && ids.remove(&target)
                && ids.is_empty()
            {
                emptied.push(grant.id);
            }
        }
        self.delete(&emptied);
        self.join();
        self.check();
    }

    fn grant_view(&self, grant: &ClanGrantRecord) -> Value {
        let recipient = match grant.recipient {
            ClanRecipient::User(id) => json!({ "user_id": id }),
            ClanRecipient::Group(id) => json!({ "group_id": id }),
        };
        let mut view = json!({
            "id": grant.id,
            "clan_id": self.id,
            "recipient": recipient,
            "actions": ordered(&grant.actions),
            "scope": grant.scope.view(),
            "delegated": delegated_view(grant, &grant.actions),
            "issuer_id": grant.issuer_id,
            "created_at": grant.created_at,
            "updated_at": grant.updated_at,
        });
        if grant.delegates() {
            view["may_grant"] = json!(ordered(&grant.may_grant));
        }
        view
    }
}

/// A grant's `delegated` list, limited to `given`: each delegation with the
/// actions of `given` that came through it.
pub fn delegated_view(grant: &ClanGrantRecord, given: &BTreeSet<String>) -> Value {
    json!(
        grant
            .delegated
            .iter()
            .filter_map(|(delegation, through)| {
                let shown: BTreeSet<String> = through.intersection(given).cloned().collect();
                (!shown.is_empty())
                    .then(|| json!({ "delegation_id": delegation, "actions": ordered(&shown) }))
            })
            .collect::<Vec<_>>()
    )
}

fn group_sort_key(group: &ClanGroupRecord) -> (u8, String, Uuid) {
    let rank = match group.builtin {
        Some("owners") => 0,
        Some("members") => 1,
        _ => 2,
    };
    (rank, group.name.to_lowercase(), group.id)
}

fn nickname(st: &MockState, user: Uuid) -> Option<String> {
    st.user(user).and_then(|record| record.nickname.clone())
}

// ---------------------------------------------------------------------------
// Request plumbing
// ---------------------------------------------------------------------------

fn gate(st: &MockState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let (caller, _) = authenticate(st, headers)?;
    gate_verified(st, caller)?;
    Ok(caller)
}

fn respond(handled: Handled) -> Response {
    handled.unwrap_or_else(|response| response)
}

/// A clan ID in a path: malformed is a 400, answered only to a caller with
/// valid credentials (`credentials_first` in mock_server.rs).
fn clan_param(raw: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|_| bad_request("Invalid clan ID"))
}

/// Any other ID in a path: malformed is the same 404 as missing, answered
/// only to a caller with valid credentials.
fn id_param(raw: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(raw).map_err(|_| not_found())
}

/// The active clan in the path, or the uniform 404.
fn active_clan(st: &MockState, id: Uuid) -> Result<&ClanRecord, Response> {
    st.clans
        .clans
        .get(&id)
        .filter(|clan| clan.active())
        .ok_or_else(not_found)
}

fn active_clan_mut(st: &mut MockState, id: Uuid) -> Result<&mut ClanRecord, Response> {
    st.clans
        .clans
        .get_mut(&id)
        .filter(|clan| clan.active())
        .ok_or_else(not_found)
}

fn json_object(body: &str) -> Result<Map<String, Value>, Response> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(fields)) => Ok(fields),
        _ => Err(bad_request("Invalid JSON body")),
    }
}

/// Absent and null both mean "not provided".
fn optional_string(fields: &Map<String, Value>, name: &str) -> Result<Option<String>, Response> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(bad_request(&format!("expected a string for `{name}`"))),
    }
}

fn required_string(fields: &Map<String, Value>, name: &str) -> Result<String, Response> {
    if !fields.contains_key(name) {
        return Err(bad_request(&format!("missing field `{name}`")));
    }
    optional_string(fields, name)?.ok_or_else(|| bad_request(&format!("missing field `{name}`")))
}

fn uuid_list(fields: &Map<String, Value>, name: &str) -> Result<Option<Vec<Uuid>>, Response> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => {
            let mut ids = Vec::new();
            for value in values {
                let id = value
                    .as_str()
                    .and_then(|raw| Uuid::parse_str(raw).ok())
                    .ok_or_else(|| bad_request(&format!("expected UUIDs in `{name}`")))?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            Ok(Some(ids))
        }
        Some(_) => Err(bad_request(&format!("expected an array for `{name}`"))),
    }
}

/// A clan or group name: a non-space character, at most 64 characters, no
/// control characters.
fn clan_name(value: String) -> Result<String, Response> {
    if value.trim().is_empty() {
        return Err(bad_request("Name must not be blank"));
    }
    if value.chars().count() > 64 {
        return Err(bad_request("Name must be at most 64 characters"));
    }
    if value.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f) {
        return Err(bad_request("Name must not contain control characters"));
    }
    Ok(value)
}

fn description(value: String) -> Result<String, Response> {
    if value.chars().count() > 1000 {
        return Err(bad_request("Description must be at most 1000 characters"));
    }
    if value.contains('\0') {
        return Err(bad_request("Description must not contain NUL characters"));
    }
    Ok(value)
}

/// `#rrggbb`, stored in lowercase. `None`: absent (keep); `Some(None)`:
/// null (clear).
fn color(fields: &Map<String, Value>) -> Result<Option<Option<String>>, Response> {
    if !fields.contains_key("color") {
        return Ok(None);
    }
    let Some(value) = optional_string(fields, "color")? else {
        return Ok(Some(None));
    };
    let valid = value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err(bad_request("color must be #rrggbb"));
    }
    Ok(Some(Some(value.to_lowercase())))
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/clans", get(list_clans).post(create_clan))
        .route(
            "/clans/:clan_id",
            get(get_clan).patch(patch_clan).delete(delete_clan),
        )
        .route("/clans/:clan_id/members", get(list_members))
        .route(
            "/clans/:clan_id/members/:user_id",
            axum::routing::patch(set_owner).delete(remove_member),
        )
        .route(
            "/clans/:clan_id/invitations",
            get(list_invitations).post(invite),
        )
        .route(
            "/clan-invitations/:invitation_id",
            axum::routing::delete(revoke_invitation),
        )
        .route("/me/clan-invitations", get(received_invitations))
        .route(
            "/clan-invitations/:invitation_id/accept",
            post(accept_invitation),
        )
        .route(
            "/clan-invitations/:invitation_id/decline",
            post(decline_invitation),
        )
        .route(
            "/clans/:clan_id/groups",
            get(list_groups).post(create_group),
        )
        .route(
            "/clans/:clan_id/groups/:group_id",
            axum::routing::patch(patch_group).delete(delete_group),
        )
        .route(
            "/clans/:clan_id/groups/:group_id/members",
            get(group_roster),
        )
        .route(
            "/clans/:clan_id/groups/:group_id/members/:user_id",
            put(assign_group_member).delete(unassign_group_member),
        )
        .route(
            "/clans/:clan_id/grants",
            get(list_grants).post(create_grant),
        )
        .route(
            "/clans/:clan_id/grants/:grant_id",
            axum::routing::patch(patch_grant).delete(delete_grant),
        )
}

// ===== clans ================================================================

/// The caller's pending invitations into active clans whose inviter may still
/// invite, oldest first.
fn received(st: &MockState, caller: Uuid) -> Vec<Value> {
    let mut rows: Vec<(DateTime<Utc>, u64, Value)> = Vec::new();
    for clan in st.clans.clans.values().filter(|clan| clan.active()) {
        for invitation in &clan.invitations {
            if invitation.user_id != caller || !clan.may(invitation.inviter_id, "clan.invite") {
                continue;
            }
            rows.push((
                invitation.created_at,
                invitation.seq,
                json!({
                    "id": invitation.id,
                    "clan_id": clan.id,
                    "clan_name": clan.name,
                    "inviter_id": invitation.inviter_id,
                    "inviter_nickname": nickname(st, invitation.inviter_id),
                    "created_at": invitation.created_at,
                }),
            ));
        }
    }
    rows.sort_by_key(|(created_at, seq, _)| (*created_at, *seq));
    rows.into_iter().map(|(_, _, row)| row).collect()
}

pub async fn list_clans(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let caller = gate(&st, &headers)?;
        let mut clans: Vec<&ClanRecord> = st
            .clans
            .clans
            .values()
            .filter(|clan| clan.is_member(caller))
            .collect();
        clans.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let clans: Vec<Value> = clans.iter().map(|clan| clan.summary(caller)).collect();
        Ok(ok(json!({
            "clans": clans,
            "invitations": received(&st, caller),
        })))
    })())
}

pub async fn create_clan(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let caller = gate(&st, &headers)?;
        let fields = json_object(&body)?;
        let name = clan_name(required_string(&fields, "name")?)?;
        let about = description(optional_string(&fields, "description")?.unwrap_or_default())?;
        let seq = st.next_seq();
        let id = Uuid::new_v4();
        let now = Utc::now();
        let mut members = BTreeMap::new();
        members.insert(
            caller,
            ClanMemberRecord {
                status: MemberStatus::Active,
                joined_at: now,
                joined_seq: seq,
            },
        );
        let everyone = Uuid::new_v4();
        let founding_seq = st.next_seq();
        let mut clan = ClanRecord {
            id,
            name,
            description: about,
            created_at: now,
            dissolved: false,
            dissolving: false,
            unrecorded: false,
            members,
            owners: BTreeSet::from([caller]),
            groups: vec![
                ClanGroupRecord {
                    id: Uuid::new_v4(),
                    name: "Owner".to_string(),
                    color: None,
                    builtin: Some("owners"),
                    creator: None,
                },
                ClanGroupRecord {
                    id: everyone,
                    name: "All clan members".to_string(),
                    color: None,
                    builtin: Some("members"),
                    creator: None,
                },
            ],
            group_members: Vec::new(),
            grants: Vec::new(),
            invitations: Vec::new(),
            owned_maps: 0,
            packages: Vec::new(),
        };
        // Its founding grant: All clan members read the member directory.
        clan.upsert(
            ClanRecipient::Group(everyone),
            ClanGrantScope::Clan,
            &own(["clan.read_members"]),
            &BTreeSet::new(),
            caller,
            founding_seq,
        );
        let summary = clan.summary(caller);
        st.clans.clans.insert(id, clan);
        Ok(created(summary))
    })())
}

pub async fn get_clan(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan(&st, clan_id)?;
        if !clan.is_member(caller) {
            return Err(not_found());
        }
        Ok(ok(clan.summary(caller)))
    })())
}

pub async fn patch_clan(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        // The body's shape is judged before the clan is looked up.
        let fields = json_object(&body)?;
        let name = optional_string(&fields, "name")?
            .map(clan_name)
            .transpose()?;
        let about = optional_string(&fields, "description")?
            .map(description)
            .transpose()?;
        let clan = active_clan_mut(&mut st, clan_id)?;
        if !clan.may(caller, "clan.edit_profile") {
            return Err(not_found());
        }
        if let Some(name) = name {
            clan.name = name;
        }
        if let Some(about) = about {
            clan.description = about;
        }
        Ok(ok(clan.summary(caller)))
    })())
}

pub async fn delete_clan(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        // An owner repeating a dissolution whose answer was lost, before the
        // directory marks the clan dissolved, gets `null`; anyone else, and
        // anyone once it is marked, the 404.
        if let Some(clan) = st.clans.clans.get_mut(&clan_id)
            && clan.dissolved
            && clan.unrecorded
        {
            if !clan.owners.contains(&caller) {
                return Err(not_found());
            }
            clan.unrecorded = false;
            return Ok(ok(Value::Null));
        }
        let maps = st
            .areas
            .values()
            .filter(|area| area.clan_id == Some(clan_id) && area.member_owned.is_none())
            .count();
        let clan = active_clan_mut(&mut st, clan_id)?;
        if !clan.is_owner(caller) {
            return Err(not_found());
        }
        if clan.owned_maps + maps > 0 {
            clan.dissolving = false;
            return Err(conflict("clan_not_empty"));
        }
        // From here until the dissolution commits, no map enters the clan.
        clan.dissolving = true;
        if st.interrupt_dissolutions > 0 {
            st.interrupt_dissolutions -= 1;
            return Err(super::http::err(500, "injected dissolution failure"));
        }
        let clan = active_clan_mut(&mut st, clan_id)?;
        let active: Vec<Uuid> = clan
            .members
            .iter()
            .filter(|(_, member)| member.status == MemberStatus::Active)
            .map(|(user, _)| *user)
            .collect();
        // Each Member-owned map's active owners get a copy, every map before
        // any goes, judged as the owners read before the clan is marked
        // dissolved; its folders go, and so do the offers to it.
        super::clan_maps::on_dissolution(&mut st, clan_id, &active);
        let clan = active_clan_mut(&mut st, clan_id)?;
        clan.dissolved = true;
        clan.dissolving = false;
        clan.invitations.clear();
        st.atlases.retain(|_, atlas| atlas.clan_id != Some(clan_id));
        super::transfers::cancel_offers_to_clan(&mut st, clan_id, None);
        if st.lose_dissolution_answers > 0 {
            st.lose_dissolution_answers -= 1;
            if let Some(clan) = st.clans.clans.get_mut(&clan_id) {
                clan.unrecorded = true;
            }
            return Err(super::http::err(500, "injected lost answer"));
        }
        Ok(ok(Value::Null))
    })())
}

// ===== members and owners ===================================================

pub async fn list_members(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan(&st, clan_id)?;
        if !clan.may(caller, "clan.read_members") {
            return Err(not_found());
        }
        let mut members: Vec<(&Uuid, &ClanMemberRecord)> = clan
            .members
            .iter()
            .filter(|(_, member)| member.status == MemberStatus::Active)
            .collect();
        members.sort_by_key(|(user, member)| {
            (!clan.owners.contains(*user), member.joined_seq, **user)
        });
        let rows: Vec<Value> = members
            .into_iter()
            .map(|(user, _)| clan.member_view(&st, caller, *user))
            .collect();
        Ok(ok(json!(rows)))
    })())
}

pub async fn set_owner(
    State(state): State<Shared>,
    Path((raw_clan, raw_user)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let user = id_param(&raw_user)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        // The body's shape is judged before the clan is looked up.
        let fields = json_object(&body)?;
        let Some(owner) = fields.get("is_owner").and_then(Value::as_bool) else {
            return Err(bad_request("expected a boolean for `is_owner`"));
        };
        let clan = active_clan_mut(&mut st, clan_id)?;
        if !clan.is_owner(caller) || !clan.is_member(user) {
            return Err(not_found());
        }
        let already = clan.owners.contains(&user);
        if owner && !already {
            clan.owners.insert(user);
        }
        if !owner && already {
            // While the clan dissolves, its owners stay its owners.
            if clan.dissolving {
                return Err(conflict("clan_dissolving"));
            }
            if clan.owner_count() == 1 {
                return Err(conflict("last_owner"));
            }
            clan.owners.remove(&user);
        }
        super::shares::sweep_outside_shares(&mut st);
        let clan = active_clan(&st, clan_id)?;
        Ok(ok(clan.member_view(&st, caller, user)))
    })())
}

/// Leaving, with the caller's own ID, or removing another member.
pub async fn remove_member(
    State(state): State<Shared>,
    Path((raw_clan, raw_user)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let user = id_param(&raw_user)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan_mut(&mut st, clan_id)?;
        if user == caller {
            if !clan.is_member(caller) {
                return Err(not_found());
            }
            if clan.is_owner(caller) && clan.dissolving {
                return Err(conflict("clan_dissolving"));
            }
            if clan.is_owner(caller) && clan.owner_count() == 1 {
                return Err(conflict("last_owner"));
            }
            clan.depart(caller, MemberStatus::Left);
            super::clan_secrets::forget_member(&mut st, clan_id, caller);
            super::clan_maps::on_departure(&mut st, clan_id, caller, None);
        } else {
            let allowed = clan.may(caller, "clan.remove_member")
                && clan.is_member(user)
                && (!clan.is_owner(user) || clan.is_owner(caller));
            if !allowed {
                return Err(not_found());
            }
            if clan.is_owner(user) && clan.dissolving {
                return Err(conflict("clan_dissolving"));
            }
            let joined = clan.members[&user].joined_at;
            clan.depart(user, MemberStatus::Removed);
            super::clan_secrets::forget_member(&mut st, clan_id, user);
            super::clan_maps::on_departure(&mut st, clan_id, user, Some(joined));
        }
        // Their live offers to the clan, and the outside shares they made,
        // end with their membership.
        super::transfers::cancel_offers_to_clan(&mut st, clan_id, Some(user));
        super::shares::forget_outside_shares_by(&mut st, clan_id, user);
        super::shares::sweep_outside_shares(&mut st);
        Ok(ok(Value::Null))
    })())
}

// ===== invitations ==========================================================

pub async fn list_invitations(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan(&st, clan_id)?;
        if !clan.is_member(caller) {
            return Err(not_found());
        }
        let all = clan.may(caller, "clan.revoke_invitation");
        if !all && !clan.may(caller, "clan.invite") {
            return Err(not_found());
        }
        let mut invitations: Vec<&ClanInvitationRecord> = clan
            .invitations
            .iter()
            .filter(|invitation| all || invitation.inviter_id == caller)
            .collect();
        invitations.sort_by_key(|invitation| (invitation.created_at, invitation.seq));
        let rows: Vec<Value> = invitations
            .into_iter()
            .map(|invitation| clan.invitation_view(&st, invitation))
            .collect();
        Ok(ok(json!(rows)))
    })())
}

pub async fn invite(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        // The body's shape is judged before the clan is looked up.
        let fields = json_object(&body)?;
        let user = required_string(&fields, "user_id")
            .and_then(|raw| Uuid::parse_str(&raw).map_err(|_| bad_request("Invalid user_id")))?;
        let groups = uuid_list(&fields, "group_ids")?.unwrap_or_default();
        // The invitee exists and no block stands between the two users.
        if st.user(user).is_none() || st.blocked_pair(caller, user) {
            return Err(not_found());
        }
        let seq = st.next_seq();
        let clan = active_clan(&st, clan_id)?;
        if !clan.may(caller, "clan.invite") {
            return Err(not_found());
        }
        if clan.is_member(user) {
            return Err(conflict("already_member"));
        }
        for group in &groups {
            let Some(record) = clan.group(*group) else {
                return Err(not_found());
            };
            if !clan.may_group(caller, *group, "group.assign") {
                return Err(not_found());
            }
            if record.builtin.is_some() {
                return Err(bad_request(
                    "Built-in groups follow membership and cannot be proposed",
                ));
            }
        }
        let clan = active_clan_mut(&mut st, clan_id)?;
        let id = if let Some(existing) = clan
            .invitations
            .iter_mut()
            .find(|invitation| invitation.user_id == user)
        {
            existing.inviter_id = caller;
            existing.group_ids.clone_from(&groups);
            existing.id
        } else {
            let id = Uuid::new_v4();
            clan.invitations.push(ClanInvitationRecord {
                id,
                user_id: user,
                inviter_id: caller,
                group_ids: groups,
                created_at: Utc::now(),
                seq,
            });
            id
        };
        let clan = active_clan(&st, clan_id)?;
        let invitation = clan
            .invitations
            .iter()
            .find(|invitation| invitation.id == id)
            .expect("just written");
        Ok(created(clan.invitation_view(&st, invitation)))
    })())
}

/// The active clan holding invitation `id`.
fn clan_of_invitation(st: &MockState, id: Uuid) -> Option<Uuid> {
    st.clans
        .clans
        .values()
        .filter(|clan| clan.active())
        .find(|clan| {
            clan.invitations
                .iter()
                .any(|invitation| invitation.id == id)
        })
        .map(|clan| clan.id)
}

pub async fn revoke_invitation(
    State(state): State<Shared>,
    Path(raw_invitation): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let id = id_param(&raw_invitation)?;
        let caller = gate(&st, &headers)?;
        let clan_id = clan_of_invitation(&st, id).ok_or_else(not_found)?;
        let clan = active_clan_mut(&mut st, clan_id)?;
        let invitation = clan
            .invitations
            .iter()
            .find(|invitation| invitation.id == id)
            .ok_or_else(not_found)?;
        let allowed = clan.is_member(caller)
            && (clan.may(caller, "clan.revoke_invitation")
                || (invitation.inviter_id == caller && clan.may(caller, "clan.invite")));
        if !allowed {
            return Err(not_found());
        }
        clan.invitations.retain(|invitation| invitation.id != id);
        Ok(ok(Value::Null))
    })())
}

pub async fn received_invitations(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let caller = gate(&st, &headers)?;
        Ok(ok(json!(received(&st, caller))))
    })())
}

pub async fn accept_invitation(
    State(state): State<Shared>,
    Path(raw_invitation): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let id = id_param(&raw_invitation)?;
        let caller = gate(&st, &headers)?;
        let clan_id = st
            .clans
            .clans
            .values()
            .find(|clan| {
                clan.invitations
                    .iter()
                    .any(|invitation| invitation.id == id && invitation.user_id == caller)
            })
            .map(|clan| clan.id)
            .ok_or_else(not_found)?;
        let seq = st.next_seq();
        let clan = st.clans.clans.get_mut(&clan_id).expect("found above");
        if !clan.active() {
            clan.invitations.retain(|invitation| invitation.id != id);
            return Err(not_found());
        }
        let invitation = clan
            .invitations
            .iter()
            .find(|invitation| invitation.id == id)
            .cloned()
            .expect("found above");
        // Acceptance rechecks the inviter: one who may no longer invite
        // voids the invitation.
        if !clan.may(invitation.inviter_id, "clan.invite") {
            clan.invitations.retain(|invitation| invitation.id != id);
            return Err(not_found());
        }
        let groups: Vec<Uuid> = invitation
            .group_ids
            .iter()
            .copied()
            .filter(|group| clan.may_group(invitation.inviter_id, *group, "group.assign"))
            .collect();
        clan.invitations.retain(|invitation| invitation.id != id);
        if !clan.is_member(caller) {
            clan.members.insert(
                caller,
                ClanMemberRecord {
                    status: MemberStatus::Active,
                    joined_at: Utc::now(),
                    joined_seq: seq,
                },
            );
            for group in groups {
                if !clan
                    .group_members
                    .iter()
                    .any(|(g, member, _)| *g == group && *member == caller)
                {
                    clan.group_members.push((group, caller, seq));
                }
            }
        }
        Ok(ok(clan.summary(caller)))
    })())
}

pub async fn decline_invitation(
    State(state): State<Shared>,
    Path(raw_invitation): Path<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let id = id_param(&raw_invitation)?;
        let caller = gate(&st, &headers)?;
        let clan = st
            .clans
            .clans
            .values_mut()
            .find(|clan| {
                clan.invitations
                    .iter()
                    .any(|invitation| invitation.id == id && invitation.user_id == caller)
            })
            .ok_or_else(not_found)?;
        let active = clan.active();
        clan.invitations.retain(|invitation| invitation.id != id);
        if !active {
            return Err(not_found());
        }
        Ok(ok(Value::Null))
    })())
}

// ===== groups ===============================================================

pub async fn list_groups(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan(&st, clan_id)?;
        if !clan.is_member(caller) {
            return Err(not_found());
        }
        let mut groups: Vec<&ClanGroupRecord> = clan.groups.iter().collect();
        groups.sort_by_key(|group| group_sort_key(group));
        let rows: Vec<Value> = groups
            .into_iter()
            .map(|group| clan.group_view(caller, group))
            .collect();
        Ok(ok(json!(rows)))
    })())
}

pub async fn create_group(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        // The body's shape is judged before the clan is looked up.
        let fields = json_object(&body)?;
        let name = clan_name(required_string(&fields, "name")?)?;
        let color = color(&fields)?.flatten();
        let seq = st.next_seq();
        let clan = active_clan_mut(&mut st, clan_id)?;
        if !clan.may(caller, "group.create") {
            return Err(not_found());
        }
        if clan.name_taken(&name, None) {
            return Err(conflict("name_in_use"));
        }
        // The creator is its first member, and governs it while a member.
        let group = ClanGroupRecord {
            id: Uuid::new_v4(),
            name,
            color,
            builtin: None,
            creator: Some(caller),
        };
        let id = group.id;
        clan.groups.push(group);
        clan.group_members.push((id, caller, seq));
        let group = clan.group(id).expect("just created").clone();
        Ok(created(clan.group_view(caller, &group)))
    })())
}

/// The group in the path that the caller holds `action` on.
fn governed<'a>(
    clan: &'a ClanRecord,
    caller: Uuid,
    group: Uuid,
    action: &str,
) -> Result<&'a ClanGroupRecord, Response> {
    let record = clan.group(group).ok_or_else(not_found)?;
    if !clan.may_group(caller, group, action) {
        return Err(not_found());
    }
    Ok(record)
}

pub async fn patch_group(
    State(state): State<Shared>,
    Path((raw_clan, raw_group)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let group_id = id_param(&raw_group)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        // The body's shape is judged before the clan is looked up.
        let fields = json_object(&body)?;
        let name = optional_string(&fields, "name")?
            .map(clan_name)
            .transpose()?;
        let color = color(&fields)?;
        let clan = active_clan_mut(&mut st, clan_id)?;
        let group = governed(clan, caller, group_id, "group.rename")?;
        let current = group.name.clone();
        if group.builtin.is_some() && name.as_ref().is_some_and(|name| *name != current) {
            return Err(bad_request("A built-in group's name cannot change"));
        }
        let name = name.unwrap_or(current.clone());
        if name != current && clan.name_taken(&name, Some(group_id)) {
            return Err(conflict("name_in_use"));
        }
        let group = clan
            .groups
            .iter_mut()
            .find(|group| group.id == group_id)
            .expect("governed");
        group.name = name;
        if let Some(color) = color {
            group.color = color;
        }
        let group = group.clone();
        Ok(ok(clan.group_view(caller, &group)))
    })())
}

pub async fn delete_group(
    State(state): State<Shared>,
    Path((raw_clan, raw_group)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let group_id = id_param(&raw_group)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan_mut(&mut st, clan_id)?;
        let group = governed(clan, caller, group_id, "group.delete")?;
        if group.builtin.is_some() {
            return Err(bad_request("Built-in groups cannot be deleted"));
        }
        clan.groups.retain(|group| group.id != group_id);
        clan.group_members
            .retain(|(group, _, _)| *group != group_id);
        // Its grants go with it, and it leaves every grant scope naming it;
        // a scope left with no IDs goes too.
        let held: Vec<Uuid> = clan
            .grants
            .iter()
            .filter(|grant| grant.recipient == ClanRecipient::Group(group_id))
            .map(|grant| grant.id)
            .collect();
        clan.remove_grants(&held);
        clan.drop_target(group_id);
        for invitation in &mut clan.invitations {
            invitation.group_ids.retain(|group| *group != group_id);
        }
        super::clan_secrets::forget_group(&mut st, clan_id, group_id);
        super::shares::sweep_outside_shares(&mut st);
        Ok(ok(Value::Null))
    })())
}

pub async fn group_roster(
    State(state): State<Shared>,
    Path((raw_clan, raw_group)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let group_id = id_param(&raw_group)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan(&st, clan_id)?;
        let group = governed(clan, caller, group_id, "group.inspect_assignments")?;
        let mut active: Vec<(&Uuid, &ClanMemberRecord)> = clan
            .members
            .iter()
            .filter(|(_, member)| member.status == MemberStatus::Active)
            .collect();
        active.sort_by_key(|(user, member)| (member.joined_seq, **user));
        let ids: Vec<Uuid> = match group.builtin {
            Some("owners") => active
                .into_iter()
                .filter(|(user, _)| clan.owners.contains(*user))
                .map(|(user, _)| *user)
                .collect(),
            Some(_) => active.into_iter().map(|(user, _)| *user).collect(),
            None => {
                let mut rows: Vec<(u64, Uuid)> = clan
                    .group_members
                    .iter()
                    .filter(|(group, user, _)| *group == group_id && clan.is_member(*user))
                    .map(|(_, user, added)| (*added, *user))
                    .collect();
                rows.sort_unstable();
                rows.into_iter().map(|(_, user)| user).collect()
            }
        };
        let rows: Vec<Value> = ids
            .into_iter()
            .map(|user| json!({ "user_id": user, "nickname": nickname(&st, user) }))
            .collect();
        Ok(ok(json!(rows)))
    })())
}

pub async fn assign_group_member(
    State(state): State<Shared>,
    Path((raw_clan, raw_group, raw_user)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let group_id = id_param(&raw_group)?;
        let user = id_param(&raw_user)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let seq = st.next_seq();
        let clan = active_clan_mut(&mut st, clan_id)?;
        let group = governed(clan, caller, group_id, "group.assign")?;
        if !clan.is_member(user) {
            return Err(not_found());
        }
        if group.builtin.is_some() {
            return Err(bad_request("A built-in group's members follow the clan"));
        }
        // Adding oneself takes a clan owner or the group's creator.
        if user == caller && !clan.is_owner(caller) && !clan.created(caller, group_id) {
            return Err(not_found());
        }
        if !clan
            .group_members
            .iter()
            .any(|(group, member, _)| *group == group_id && *member == user)
        {
            clan.group_members.push((group_id, user, seq));
        }
        Ok(ok(Value::Null))
    })())
}

pub async fn unassign_group_member(
    State(state): State<Shared>,
    Path((raw_clan, raw_group, raw_user)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let group_id = id_param(&raw_group)?;
        let user = id_param(&raw_user)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan_mut(&mut st, clan_id)?;
        let group = governed(clan, caller, group_id, "group.assign")?;
        if group.builtin.is_some() {
            return Err(bad_request("A built-in group's members follow the clan"));
        }
        clan.group_members
            .retain(|(group, member, _)| !(*group == group_id && *member == user));
        super::shares::sweep_outside_shares(&mut st);
        Ok(ok(Value::Null))
    })())
}

// ===== grants ===============================================================

const MAX_SCOPE_IDS: usize = 256;

/// An action list in a grant body: known actions, deduplicated.
fn action_set(
    fields: &Map<String, Value>,
    name: &str,
) -> Result<Option<BTreeSet<String>>, Response> {
    match fields.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => {
            let mut actions = BTreeSet::new();
            for value in values {
                let action = value
                    .as_str()
                    .ok_or_else(|| bad_request(&format!("expected strings in `{name}`")))?;
                if !CLAN_ACTIONS.contains(&action) {
                    return Err(bad_request(&format!("unknown action `{action}`")));
                }
                actions.insert(action.to_string());
            }
            Ok(Some(actions))
        }
        Some(_) => Err(bad_request(&format!("expected an array for `{name}`"))),
    }
}

fn recipient_of(fields: &Map<String, Value>) -> Result<ClanRecipient, Response> {
    let Some(Value::Object(recipient)) = fields.get("recipient") else {
        return Err(bad_request("missing field `recipient`"));
    };
    let id = |name: &str| {
        recipient
            .get(name)
            .and_then(Value::as_str)
            .and_then(|raw| Uuid::parse_str(raw).ok())
    };
    match (id("user_id"), id("group_id")) {
        (Some(user), None) => Ok(ClanRecipient::User(user)),
        (None, Some(group)) => Ok(ClanRecipient::Group(group)),
        _ => Err(bad_request(
            "`recipient` names exactly one of `user_id` and `group_id`",
        )),
    }
}

fn scope_of(fields: &Map<String, Value>) -> Result<ClanGrantScope, Response> {
    let Some(Value::Object(scope)) = fields.get("scope") else {
        return Err(bad_request("missing field `scope`"));
    };
    let kind = scope
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("missing field `kind`"))?;
    let ids: BTreeSet<Uuid> = uuid_list(scope, "ids")?
        .unwrap_or_default()
        .into_iter()
        .collect();
    if kind == "clan" {
        if !ids.is_empty() {
            return Err(bad_request("a clan scope names no IDs"));
        }
        return Ok(ClanGrantScope::Clan);
    }
    if ids.is_empty() {
        return Err(bad_request("a scope names at least one ID"));
    }
    if ids.len() > MAX_SCOPE_IDS {
        return Err(bad_request("a scope names at most 256 IDs"));
    }
    match kind {
        "groups" => Ok(ClanGrantScope::Groups(ids)),
        "atlases" => Ok(ClanGrantScope::Atlases(ids)),
        "areas" => Ok(ClanGrantScope::Areas(ids)),
        "packages" => Ok(ClanGrantScope::Packages(ids)),
        other => Err(bad_request(&format!("unknown scope kind `{other}`"))),
    }
}

fn no_roles(fields: &Map<String, Value>) -> Result<(), Response> {
    if fields.contains_key("role_id") {
        return Err(bad_request(
            "grants carry their own actions; there are no roles",
        ));
    }
    Ok(())
}

/// A ceiling holds map, folder, Secret and package actions only.
fn ceiling_refusal<'a>(named: impl IntoIterator<Item = &'a String>) -> Result<(), Response> {
    match named.into_iter().find(|action| administration(action)) {
        Some(action) => Err(bad_request(&format!(
            "`may_grant` holds map, folder, Secret and package actions only, not `{action}`"
        ))),
        None => Ok(()),
    }
}

/// A `POST` body's `actions` (inline, never a role here) and `may_grant`,
/// which goes with `grant.manage` and only with it.
fn bundle_of(
    fields: &Map<String, Value>,
) -> Result<(BTreeSet<String>, BTreeSet<String>), Response> {
    no_roles(fields)?;
    let actions =
        action_set(fields, "actions")?.ok_or_else(|| bad_request("missing field `actions`"))?;
    if actions.is_empty() {
        return Err(bad_request("a grant needs an action"));
    }
    let may_grant = action_set(fields, "may_grant")?;
    if actions.contains("grant.manage") != may_grant.is_some() {
        return Err(bad_request(
            "`may_grant` goes with `grant.manage`, and only with it",
        ));
    }
    let may_grant = may_grant.unwrap_or_default();
    ceiling_refusal(&may_grant)?;
    Ok((actions, may_grant))
}

/// A `PATCH` body: `{"actions": {"add", "remove"}, "may_grant": {"add",
/// "remove"}}`, every part optional, naming at least one action, none both
/// added and removed in one part.
#[derive(Debug, Default)]
struct Change {
    add: BTreeSet<String>,
    remove: BTreeSet<String>,
    ceiling_add: BTreeSet<String>,
    ceiling_remove: BTreeSet<String>,
}

impl Change {
    fn of(fields: &Map<String, Value>) -> Result<Self, Response> {
        no_roles(fields)?;
        let part = |name: &str| -> Result<(BTreeSet<String>, BTreeSet<String>), Response> {
            match fields.get(name) {
                None | Some(Value::Null) => Ok(Default::default()),
                Some(Value::Object(part)) => {
                    let add = action_set(part, "add")?.unwrap_or_default();
                    let remove = action_set(part, "remove")?.unwrap_or_default();
                    if let Some(action) = add.intersection(&remove).next() {
                        return Err(bad_request(&format!(
                            "`{action}` is both added and removed"
                        )));
                    }
                    Ok((add, remove))
                }
                Some(_) => Err(bad_request(&format!(
                    "expected `{{\"add\", \"remove\"}}` for `{name}`"
                ))),
            }
        };
        let (add, remove) = part("actions")?;
        let (ceiling_add, ceiling_remove) = part("may_grant")?;
        let change = Self {
            add,
            remove,
            ceiling_add,
            ceiling_remove,
        };
        if change.names_nothing() {
            return Err(bad_request("a change names at least one action"));
        }
        ceiling_refusal(change.ceiling_add.iter().chain(&change.ceiling_remove))?;
        Ok(change)
    }

    fn names_nothing(&self) -> bool {
        self.add.is_empty()
            && self.remove.is_empty()
            && self.ceiling_add.is_empty()
            && self.ceiling_remove.is_empty()
    }

    fn touches_ceiling(&self) -> bool {
        !self.ceiling_add.is_empty() || !self.ceiling_remove.is_empty()
    }
}

/// Inline actions and their ceiling must apply to the scope's kind.
fn inline_refusal(
    scope: &ClanGrantScope,
    actions: &BTreeSet<String>,
    may_grant: &BTreeSet<String>,
) -> Result<(), Response> {
    for action in actions.iter().chain(may_grant) {
        if !takes_inline(scope.kind(), action) {
            return Err(bad_request(&format!(
                "`{action}` does not apply to a {} scope",
                scope.kind()
            )));
        }
    }
    Ok(())
}

const MEMBER_MAP_ACCESS: &str =
    "Map and atlas actions go to groups, and to a single member only on one map";

pub async fn list_grants(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        // The query's shape is judged before the clan is looked up.
        let filter = |name: &str| -> Result<Option<Uuid>, Response> {
            params
                .get(name)
                .map(|raw| Uuid::parse_str(raw).map_err(|_| bad_request("Invalid ID")))
                .transpose()
        };
        let (user, group) = (filter("user_id")?, filter("group_id")?);
        let named = (
            filter("atlas_id")?,
            filter("area_id")?,
            filter("package_id")?,
        );
        let clan = active_clan(&st, clan_id)?;
        if !clan.is_member(caller) {
            return Err(not_found());
        }
        let resource = match named {
            (Some(atlas), _, _) => Some(ClanResource::Atlas(atlas)),
            (None, None, Some(package)) => Some(ClanResource::Package(package)),
            (None, Some(area), _) => match super::clan_maps::placement(&st, clan_id, area) {
                Some((atlas, member_owned)) => Some(ClanResource::Area {
                    id: area,
                    atlas,
                    member_owned,
                }),
                None => return Ok(ok(json!([]))),
            },
            (None, None, None) => None,
        };
        let placed = |area: Uuid| super::clan_maps::placement(&st, clan_id, area).and_then(|p| p.0);
        let mut grants: Vec<&ClanGrantRecord> = clan
            .grants
            .iter()
            .filter(|grant| {
                let visible =
                    match super::clan_maps::member_owned_target(&st, clan_id, &grant.scope) {
                        Some(area) => super::clan_maps::may_see_member_grant(
                            &st,
                            clan,
                            caller,
                            area,
                            grant.recipient,
                        ),
                        None => clan.may_inspect(caller, grant, &placed),
                    };
                visible
                    && user.is_none_or(|user| grant.recipient == ClanRecipient::User(user))
                    && group.is_none_or(|group| grant.recipient == ClanRecipient::Group(group))
                    && resource.is_none_or(|resource| match resource {
                        ClanResource::Atlas(_) => {
                            covers(&grant.scope, resource)
                                && grant.actions.iter().any(|action| {
                                    applies(action, resource)
                                        || applies(
                                            action,
                                            ClanResource::Area {
                                                id: Uuid::nil(),
                                                atlas: None,
                                                member_owned: false,
                                            },
                                        )
                                })
                        }
                        ClanResource::Area {
                            id,
                            member_owned: true,
                            ..
                        } => grant.scope == ClanGrantScope::Areas(BTreeSet::from([id])),
                        _ => !clan.contribution(grant, resource).is_empty(),
                    })
            })
            .collect();
        grants.sort_by_key(|grant| (grant.created_at, grant.seq));
        Ok(ok(json!(
            grants
                .into_iter()
                .map(|grant| clan.grant_view(grant))
                .collect::<Vec<_>>()
        )))
    })())
}

pub async fn create_grant(
    State(state): State<Shared>,
    Path(raw_clan): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let fields = json_object(&body)?;
        let recipient = recipient_of(&fields)?;
        let scope = scope_of(&fields)?;
        let (actions, may_grant) = bundle_of(&fields)?;
        let clan = active_clan(&st, clan_id)?;
        if !clan.is_member(caller) {
            return Err(not_found());
        }
        inline_refusal(&scope, &actions, &may_grant)?;
        if matches!(recipient, ClanRecipient::User(_))
            && !names_one_map(&scope)
            && (matches!(scope, ClanGrantScope::Atlases(_) | ClanGrantScope::Areas(_))
                || actions.iter().any(|action| grants_map_access(action)))
        {
            return Err(bad_request(MEMBER_MAP_ACCESS));
        }
        let placed = |area: Uuid| super::clan_maps::placement(&st, clan_id, area).and_then(|p| p.0);
        let member_owned = super::clan_maps::member_owned_target(&st, clan_id, &scope);
        let actions = match member_owned {
            Some(area) => super::clan_maps::member_grant_actions(
                &st, caller, area, &scope, &actions, &may_grant,
            )?,
            None => actions,
        };
        // Where each action comes from: an owner's, of the clan or of the
        // Member-owned map, are the grant's own; anyone else's each come
        // through the oldest of their delegations that hands it out there.
        let added: Vec<(String, Source)> = if clan.is_owner(caller) || member_owned.is_some() {
            own(&actions)
        } else if actions.contains("grant.manage") {
            return Err(not_found());
        } else {
            actions
                .iter()
                .map(|action| {
                    clan.delegation_through(caller, &scope, action, &placed)
                        .map(|delegation| (action.clone(), Source::Through(delegation)))
                        .ok_or_else(not_found)
                })
                .collect::<Result<_, _>>()?
        };
        let recipient_exists = match recipient {
            ClanRecipient::User(user) => clan.is_member(user),
            ClanRecipient::Group(group) => clan.group(group).is_some(),
        };
        let targets_exist = match &scope {
            ClanGrantScope::Clan => true,
            ClanGrantScope::Groups(ids) => ids.iter().all(|id| clan.group(*id).is_some()),
            ClanGrantScope::Atlases(ids) => ids.iter().all(|id| {
                st.atlases
                    .get(id)
                    .is_some_and(|atlas| atlas.clan_id == Some(clan_id))
            }),
            ClanGrantScope::Areas(ids) => ids
                .iter()
                .all(|id| super::clan_maps::placement(&st, clan_id, *id).is_some()),
            ClanGrantScope::Packages(ids) => ids.iter().all(|id| clan.has_package(*id)),
        };
        if !recipient_exists || !targets_exist {
            return Err(not_found());
        }
        // The recipient's grant over the scope, if any, takes the addition;
        // a grant carrying `grant.manage` keeps a ceiling.
        let held = clan.grant_over(recipient, &scope);
        if (actions.contains("grant.manage") || held.is_some_and(ClanGrantRecord::delegates))
            && may_grant.is_empty()
            && held.is_none_or(|grant| grant.may_grant.is_empty())
        {
            return Err(bad_request(
                "a grant carrying `grant.manage` hands out something",
            ));
        }
        let seq = st.next_seq();
        let clan = active_clan_mut(&mut st, clan_id)?;
        let (id, fresh) = clan.upsert(recipient, scope, &added, &may_grant, caller, seq);
        let view = clan.grant_view(clan.grant(id).expect("just written"));
        super::shares::sweep_outside_shares(&mut st);
        Ok(if fresh { created(view) } else { ok(view) })
    })())
}

/// How the caller writes a grant (docs/clans.md §1.2, §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Writer {
    /// A clan owner: any grant but a Member-owned map's, every action the
    /// grant's own.
    Owner,
    /// An active owner of the Member-owned map a grant names alone.
    MapOwner,
    /// Anyone else, action by action within their delegations.
    Delegate,
}

/// A grant the caller may see, and how they may write it. Whether a delegate
/// may write what they ask is judged action by action.
fn managed(
    st: &MockState,
    clan: &ClanRecord,
    caller: Uuid,
    grant_id: Uuid,
) -> Result<(ClanGrantRecord, Writer), Response> {
    let placed = |area: Uuid| super::clan_maps::placement(st, clan.id, area).and_then(|p| p.0);
    let grant = clan.grant(grant_id).ok_or_else(not_found)?;
    // A Member-owned map's grants are its active owners' to change.
    if let Some(area) = super::clan_maps::member_owned_target(st, clan.id, &grant.scope) {
        let record = st.areas.get(&area).ok_or_else(not_found)?;
        if !super::clan_maps::has_authority(st, caller, record) {
            return Err(not_found());
        }
        return Ok((grant.clone(), Writer::MapOwner));
    }
    if !clan.may_inspect(caller, grant, &placed) {
        return Err(not_found());
    }
    let writer = if clan.is_owner(caller) {
        Writer::Owner
    } else {
        Writer::Delegate
    };
    Ok((grant.clone(), writer))
}

pub async fn patch_grant(
    State(state): State<Shared>,
    Path((raw_clan, raw_grant)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let grant_id = id_param(&raw_grant)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let fields = json_object(&body)?;
        let change = Change::of(&fields)?;
        let clan = active_clan(&st, clan_id)?;
        let (grant, writer) = managed(&st, clan, caller, grant_id)?;
        inline_refusal(&grant.scope, &change.add, &change.ceiling_add)?;
        if matches!(grant.recipient, ClanRecipient::User(_))
            && !names_one_map(&grant.scope)
            && change.add.iter().any(|action| grants_map_access(action))
        {
            return Err(bad_request(MEMBER_MAP_ACCESS));
        }
        let placed = |area: Uuid| super::clan_maps::placement(&st, clan_id, area).and_then(|p| p.0);
        let mut changed = grant.clone();
        match writer {
            Writer::Owner => {
                for action in &change.add {
                    changed.add(action, Source::Own);
                }
            }
            Writer::MapOwner => {
                super::clan_maps::member_grant_change(
                    &change.add,
                    &change.remove,
                    change.touches_ceiling(),
                )?;
                for action in &change.add {
                    changed.add(action, Source::Own);
                }
            }
            Writer::Delegate => {
                // Never delegation itself; each action within one of their
                // delegations over the grant's scope; and on a grant that
                // delegates, never one of its own.
                if change.touches_ceiling()
                    || change.add.contains("grant.manage")
                    || change.remove.contains("grant.manage")
                {
                    return Err(not_found());
                }
                for action in &change.add {
                    let delegation = clan
                        .delegation_through(caller, &grant.scope, action, &placed)
                        .ok_or_else(not_found)?;
                    changed.add(action, Source::Through(delegation));
                }
                for action in &change.remove {
                    if clan
                        .delegation_through(caller, &grant.scope, action, &placed)
                        .is_none()
                        || (grant.delegates() && grant.owns(action))
                    {
                        return Err(not_found());
                    }
                }
            }
        }
        for action in &change.remove {
            changed.remove(action);
        }
        changed.may_grant.extend(change.ceiling_add.iter().cloned());
        changed
            .may_grant
            .retain(|action| !change.ceiling_remove.contains(action));
        if changed.delegates() == changed.may_grant.is_empty() {
            return Err(bad_request(
                "a grant carrying `grant.manage` hands out something, and only such a grant",
            ));
        }
        // A delegation that stops delegating takes every action that came
        // through it; a narrower ceiling, those it leaves out.
        let retired = grant.delegates()
            && (!changed.delegates() || !grant.may_grant.is_subset(&changed.may_grant));
        let kept = if changed.delegates() {
            changed.may_grant.clone()
        } else {
            BTreeSet::new()
        };
        if !changed.holds_as(&grant) {
            changed.updated_at = Utc::now();
        }
        let clan = active_clan_mut(&mut st, clan_id)?;
        *clan
            .grants
            .iter_mut()
            .find(|other| other.id == grant_id)
            .expect("found above") = changed;
        if retired {
            clan.retire_through(grant_id, &kept);
        }
        if clan
            .grant(grant_id)
            .is_some_and(|grant| grant.actions.is_empty())
        {
            clan.delete(&[grant_id]);
        }
        clan.check();
        let answer = clan
            .grant(grant_id)
            .map_or(Value::Null, |grant| clan.grant_view(grant));
        super::shares::sweep_outside_shares(&mut st);
        Ok(ok(answer))
    })())
}

pub async fn delete_grant(
    State(state): State<Shared>,
    Path((raw_clan, raw_grant)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock();
    respond((|| -> Handled {
        let grant_id = id_param(&raw_grant)?;
        let clan_id = clan_param(&raw_clan)?;
        let caller = gate(&st, &headers)?;
        let clan = active_clan(&st, clan_id)?;
        let (grant, writer) = managed(&st, clan, caller, grant_id)?;
        // A delegate deletes a grant that does not delegate, when each of
        // its actions is within one of their delegations over its scope.
        if writer == Writer::Delegate {
            let placed =
                |area: Uuid| super::clan_maps::placement(&st, clan_id, area).and_then(|p| p.0);
            if grant.delegates()
                || grant.actions.iter().any(|action| {
                    clan.delegation_through(caller, &grant.scope, action, &placed)
                        .is_none()
                })
            {
                return Err(not_found());
            }
        }
        active_clan_mut(&mut st, clan_id)?.remove_grants(&[grant_id]);
        super::shares::sweep_outside_shares(&mut st);
        Ok(ok(Value::Null))
    })())
}

// ---------------------------------------------------------------------------
// Test seeding
// ---------------------------------------------------------------------------

impl MockHandle {
    /// Seeds actions in a clan, the way an owner writes them: they join the
    /// recipient's grant over the scope, or make it. Returns its ID.
    pub fn clan_grant(
        &self,
        clan: Uuid,
        recipient: ClanRecipient,
        scope: ClanGrantScope,
        actions: &[&str],
    ) -> Uuid {
        self.clan_delegation(clan, recipient, scope, actions, &[])
    }

    /// The grants a clan holds (IDs and recipients), to observe what a
    /// departure or deletion took with it.
    pub fn clan_grants(&self, clan: Uuid) -> Vec<(Uuid, ClanRecipient)> {
        let st = self.state.lock();
        st.clans.clans[&clan]
            .grants
            .iter()
            .map(|grant| (grant.id, grant.recipient))
            .collect()
    }

    /// One of a clan's grants as the service holds it.
    pub fn clan_grant_record(&self, clan: Uuid, grant: Uuid) -> Option<ClanGrantRecord> {
        self.state.lock().clans.clans[&clan].grant(grant).cloned()
    }

    /// Seeds a delegating grant (`grant.manage`) with what it may hand out,
    /// as a clan owner writes one, and returns its ID.
    pub fn clan_delegation(
        &self,
        clan: Uuid,
        recipient: ClanRecipient,
        scope: ClanGrantScope,
        actions: &[&str],
        may_grant: &[&str],
    ) -> Uuid {
        let mut st = self.state.lock();
        let seq = st.next_seq();
        let clan = st.clans.clans.get_mut(&clan).expect("clan exists");
        let issuer = clan.owners.iter().next().copied().unwrap_or_default();
        let ceiling = may_grant.iter().map(ToString::to_string).collect();
        clan.upsert(recipient, scope, &own(actions), &ceiling, issuer, seq)
            .0
    }

    /// Seeds a package the clan owns, and returns its ID.
    pub fn clan_package(&self, clan: Uuid, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.state
            .lock()
            .clans
            .clans
            .get_mut(&clan)
            .expect("clan exists")
            .packages
            .push(ClanPackageRecord {
                id,
                name: name.to_string(),
                is_public: false,
            });
        id
    }

    /// The built-in group of `kind` (`"owners"` or `"members"`).
    pub fn clan_builtin_group(&self, clan: Uuid, kind: &str) -> Uuid {
        self.state.lock().clans.clans[&clan].builtin(kind)
    }

    /// Records that a clan owns `count` maps, which keeps it from being
    /// dissolved.
    pub fn set_clan_owned_maps(&self, clan: Uuid, count: usize) {
        self.state
            .lock()
            .clans
            .clans
            .get_mut(&clan)
            .expect("clan exists")
            .owned_maps = count;
    }

    /// Adds `user` straight to a clan as an active member (test setup that
    /// skips the invitation round trip).
    pub fn join_clan(&self, clan: Uuid, user: &TestUser) {
        let mut st = self.state.lock();
        let seq = st.next_seq();
        st.clans
            .clans
            .get_mut(&clan)
            .expect("clan exists")
            .members
            .insert(
                user.id,
                ClanMemberRecord {
                    status: MemberStatus::Active,
                    joined_at: Utc::now(),
                    joined_seq: seq,
                },
            );
    }
}
