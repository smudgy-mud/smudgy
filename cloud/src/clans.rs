//! Clans: the caller's clans and invitations, members and owners, and
//! groups (`/clans`, `/clan-invitations`, `/me/clan-invitations`).
//!
//! A clan is a principal like a user: it has members, owners, invitations
//! and groups. Every route needs a verified email. Every refusal that could
//! reveal something the caller may not know is the uniform 404
//! ([`NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)); the rest
//! are the clan 409s ([`LastOwner`](crate::CloudError::LastOwner),
//! [`ClanNotEmpty`](crate::CloudError::ClanNotEmpty),
//! [`AlreadyMember`](crate::CloudError::AlreadyMember),
//! [`NameInUse`](crate::CloudError::NameInUse)).
//!
//! The caller's powers come from the server: [`ClanSummary::actions`] lists
//! the actions they hold on the whole clan, and [`ClanGroup::actions`] those
//! on one group. Controls gate on these, never on whether a member row says
//! "owner". Only the owner-only steps (making members owners, dissolving the
//! clan) read [`ClanSummary::is_owner`].

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::cloud_api::{Auth, CloudApiClient, UserRef};
use crate::{AreaId, Atlas, AtlasId, CloudResult};

/// The clan actions a client may meet (the server's vocabulary; a clan's
/// [`ClanSummary::actions`] and a group's [`ClanGroup::actions`] hold these
/// strings).
pub mod action {
    /// Changing the clan's name and description.
    pub const EDIT_PROFILE: &str = "clan.edit_profile";
    /// Reading the member directory.
    pub const READ_MEMBERS: &str = "clan.read_members";
    /// Inviting users, and revoking one's own invitations.
    pub const INVITE: &str = "clan.invite";
    /// Seeing and revoking anyone's invitations.
    pub const REVOKE_INVITATION: &str = "clan.revoke_invitation";
    /// Removing members who are not owners.
    pub const REMOVE_MEMBER: &str = "clan.remove_member";
    /// Creating custom groups, which their creator joins and governs.
    pub const CREATE_GROUP: &str = "group.create";
    /// Renaming a custom group and changing a group's color.
    pub const RENAME_GROUP: &str = "group.rename";
    /// Deleting a custom group.
    pub const DELETE_GROUP: &str = "group.delete";
    /// Adding and removing a custom group's members; adding oneself only to
    /// a group one created.
    pub const ASSIGN_GROUP: &str = "group.assign";
    /// Reading a group's roster.
    pub const INSPECT_GROUP: &str = "group.inspect_assignments";
    /// Reading the grants within one's own scope.
    pub const INSPECT_GRANTS: &str = "grant.inspect";
    /// Writing grants within one's scope and ceiling.
    pub const MANAGE_GRANTS: &str = "grant.manage";

    /// Creating folders.
    pub const CREATE_ATLAS: &str = "atlas.create";
    /// Seeing a folder.
    pub const READ_ATLAS: &str = "atlas.read";
    /// Renaming a folder.
    pub const RENAME_ATLAS: &str = "atlas.rename";
    /// Deleting a folder that holds none of the clan's own maps. Clan owners
    /// only: no grant carries it.
    pub const DELETE_ATLAS: &str = "atlas.delete";
    /// Filing a map into a folder: moving the clan's maps there.
    pub const ACCEPT_FILING: &str = "atlas.accept_filing";
    /// Accepting a member's transfer: of a map into a folder, or, held on the
    /// clan, of a whole atlas into the clan; and accepting a Member-owned map
    /// as Clan-owned into a folder.
    pub const ACCEPT_TRANSFER: &str = "atlas.accept_transfer";
    /// Creating maps in a folder.
    pub const CREATE_AREA: &str = "area.create";

    /// Reading a map.
    pub const READ_AREA: &str = "area.read";
    /// Adding content to a map.
    pub const ADD_TO_AREA: &str = "area.add";
    /// Changing a map's content.
    pub const EDIT_AREA: &str = "area.edit";
    /// Removing content from a map.
    pub const REMOVE_FROM_AREA: &str = "area.remove_content";
    /// Renaming a map.
    pub const RENAME_AREA: &str = "area.rename";
    /// Moving a map to another of the clan's folders (which also needs
    /// [`ACCEPT_FILING`] there).
    pub const REFILE_AREA: &str = "area.refile";
    /// Deleting a map.
    pub const DELETE_AREA: &str = "area.delete";
    /// Copying a map.
    pub const COPY_AREA: &str = "area.copy";
    /// Sharing a Clan-owned map with friends outside the clan, view only.
    pub const SHARE_AREA_EXTERNALLY: &str = "area.share_external";
    /// Creating Member-owned maps in a folder.
    pub const CREATE_MEMBER_OWNED_AREA: &str = "area.create_member_owned";

    /// Creating Member-owned Secrets on a map.
    pub const CREATE_MEMBER_OWNED_SECRET: &str = "secret.create_member_owned";
    /// Creating Clan-owned Secrets on a map.
    pub const CREATE_CLAN_OWNED_SECRET: &str = "secret.create_clan_owned";
    /// Reading every Clan-owned Secret on a map, now and later.
    pub const READ_SECRETS: &str = "secret.read";
    /// Adding content to those Secrets.
    pub const ADD_TO_SECRETS: &str = "secret.add";
    /// Changing those Secrets' content.
    pub const EDIT_SECRETS: &str = "secret.edit";
    /// Removing content from those Secrets.
    pub const REMOVE_FROM_SECRETS: &str = "secret.remove_content";
    /// Managing who reads those Secrets.
    pub const MANAGE_SECRET_ACCESS: &str = "secret.manage_access";
    /// Taking those Secrets along when copying their map.
    pub const COPY_SECRETS: &str = "secret.copy";

    /// Creating packages the clan owns.
    pub const CREATE_PACKAGE: &str = "package.create";
    /// Seeing a package among the clan's packages.
    pub const READ_PACKAGE: &str = "package.read";
    /// Editing a clan package's unpublished source in the client.
    pub const EDIT_PACKAGE_DRAFT: &str = "package.edit_draft";
    /// Changing a clan package's description and host alignment.
    pub const EDIT_PACKAGE_METADATA: &str = "package.edit_metadata";
    /// Making a clan package public or private.
    pub const MANAGE_PACKAGE_AVAILABILITY: &str = "package.manage_availability";
    /// Publishing a clan package's versions.
    pub const PUBLISH_PACKAGE: &str = "package.publish";
    /// Yanking, un-yanking and retiring a clan package's versions.
    pub const RETIRE_PACKAGE_VERSION: &str = "package.retire";
    /// Deleting a clan package.
    pub const DELETE_PACKAGE: &str = "package.delete";
}

/// What a Clan-owned map's creator starts with: a grant naming the map alone,
/// of reading, adding, editing and removing its content (the Editor map
/// preset). The client's presets are `smudgy_ui`'s `presets` module.
pub const MAP_CREATOR_ACTIONS: &[&str] = &[
    action::READ_AREA,
    action::ADD_TO_AREA,
    action::EDIT_AREA,
    action::REMOVE_FROM_AREA,
];

/// Who a clan grant goes to: one member, or one group. Map and folder
/// access goes to groups only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GrantRecipient {
    User { user_id: Uuid },
    Group { group_id: Uuid },
}

impl GrantRecipient {
    /// The group, on a grant to a group.
    #[must_use]
    pub const fn group(self) -> Option<Uuid> {
        match self {
            Self::Group { group_id } => Some(group_id),
            Self::User { .. } => None,
        }
    }
}

/// What a clan grant covers. A `Clan` scope takes clan/group administration
/// and folder/package creation, never resource access. `Groups` names groups;
/// `Atlases` names folders and their current and future Clan-owned maps;
/// `Areas` names maps; `Packages` names packages. Member-owned maps require
/// a grant naming that map alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GrantScope {
    Clan,
    Groups { ids: Vec<Uuid> },
    Atlases { ids: Vec<AtlasId> },
    Areas { ids: Vec<AreaId> },
    Packages { ids: Vec<Uuid> },
}

/// Actions a grant holds through one delegation: a delegate added them under
/// the delegating grant `delegation_id`, and they give something only while
/// it carries `grant.manage` and its `may_grant` holds them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedActions {
    pub delegation_id: Uuid,
    pub actions: BTreeSet<String>,
}

/// The actions in `actions` that came through none of `delegated`.
pub(crate) fn direct<'a>(
    actions: &'a BTreeSet<String>,
    delegated: &[DelegatedActions],
) -> BTreeSet<&'a str> {
    actions
        .iter()
        .map(String::as_str)
        .filter(|action| {
            !delegated
                .iter()
                .any(|through| through.actions.contains(*action))
        })
        .collect()
}

/// The delegations in `delegated` that `action` came through.
pub(crate) fn delegations(delegated: &[DelegatedActions], action: &str) -> Vec<Uuid> {
    delegated
        .iter()
        .filter(|through| through.actions.contains(action))
        .map(|through| through.delegation_id)
        .collect()
}

/// A clan grant (`/clans/{c}/grants`). A recipient holds at most one grant
/// over a scope; writing to them over that scope again changes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanGrant {
    pub id: Uuid,
    pub clan_id: Uuid,
    pub recipient: GrantRecipient,
    /// What the grant gives: its own inline actions and those that came
    /// through a delegation.
    pub actions: BTreeSet<String>,
    /// On a grant that delegates `grant.manage`: what its holder may hand
    /// out. Only clan owners issue such a grant.
    #[serde(default)]
    pub may_grant: Option<BTreeSet<String>>,
    pub scope: GrantScope,
    /// The actions that came through a delegation, by delegation. An action
    /// of `actions` listed in none of them is the grant's own.
    #[serde(default)]
    pub delegated: Vec<DelegatedActions>,
    /// Whoever created the grant.
    pub issuer_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ClanGrant {
    /// Whether the grant gives map access (any `area.*` action).
    #[must_use]
    pub fn gives_map_access(&self) -> bool {
        self.actions
            .iter()
            .any(|action| action.starts_with("area."))
    }

    /// The grant's own actions: those that came through no delegation. They
    /// stay until removed; only a clan owner, or a Member-owned map's owner,
    /// adds them, and a delegate never removes them from a grant that
    /// delegates.
    #[must_use]
    pub fn direct_actions(&self) -> BTreeSet<&str> {
        direct(&self.actions, &self.delegated)
    }

    /// The delegations `action` came through, as the server lists them:
    /// empty for one of the grant's own actions, or one it does not hold.
    /// The action goes when the last of them stops delegating it.
    #[must_use]
    pub fn delegations_of(&self, action: &str) -> Vec<Uuid> {
        delegations(&self.delegated, action)
    }
}

/// `GET /clans/{c}/grants` filters. An `atlas_id` keeps the grants that give
/// something on that folder or the maps filed in it; an `area_id` those that
/// give something on that map; a `package_id` those that give something on
/// that package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClanGrantFilter {
    pub user_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub atlas_id: Option<AtlasId>,
    pub area_id: Option<AreaId>,
    pub package_id: Option<Uuid>,
}

impl ClanGrantFilter {
    fn query(self) -> Vec<(&'static str, String)> {
        let mut query = Vec::new();
        if let Some(user) = self.user_id {
            query.push(("user_id", user.to_string()));
        }
        if let Some(group) = self.group_id {
            query.push(("group_id", group.to_string()));
        }
        if let Some(atlas) = self.atlas_id {
            query.push(("atlas_id", atlas.to_string()));
        }
        if let Some(area) = self.area_id {
            query.push(("area_id", area.to_string()));
        }
        if let Some(package) = self.package_id {
            query.push(("package_id", package.to_string()));
        }
        query
    }
}

/// One of the caller's clans (`GET /clans`, `GET /clans/{c}`, and the
/// answer to creating, renaming, or joining one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanSummary {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub member_count: u32,
    /// Whether the caller is one of its owners.
    pub is_owner: bool,
    /// The groups the caller is in, built-ins included.
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
    /// The actions the caller holds on the whole clan; an owner holds all.
    #[serde(default)]
    pub actions: BTreeSet<String>,
}

impl ClanSummary {
    /// Whether the caller holds `action` (see [`action`]) on the whole clan.
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.contains(action)
    }
}

/// An invitation the caller has received (`GET /me/clan-invitations`, and
/// the `invitations` of `GET /clans`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedInvitation {
    pub id: Uuid,
    pub clan_id: Uuid,
    pub clan_name: String,
    pub inviter_id: Uuid,
    #[serde(default)]
    pub inviter_nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// `GET /clans`: the caller's clans by name, and their pending invitations,
/// oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClansOverview {
    pub clans: Vec<ClanSummary>,
    #[serde(default)]
    pub invitations: Vec<ReceivedInvitation>,
}

/// One row of the member directory (`GET /clans/{c}/members`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanMember {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    pub joined_at: DateTime<Utc>,
    pub is_owner: bool,
    /// The custom groups the member is in, among those whose roster the
    /// caller may read.
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
}

/// A pending invitation as the clan's administrators see it
/// (`GET /clans/{c}/invitations`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanInvitation {
    pub id: Uuid,
    pub clan_id: Uuid,
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    pub inviter_id: Uuid,
    #[serde(default)]
    pub inviter_nickname: Option<String>,
    /// Custom groups the invitee joins on accepting.
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// A clan's group (`GET /clans/{c}/groups`): the two built-ins (`owners`,
/// `members`) and the custom groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanGroup {
    pub id: Uuid,
    pub name: String,
    /// `#rrggbb`, or none.
    #[serde(default)]
    pub color: Option<String>,
    /// `"owners"`, `"members"`, or none for a custom group.
    #[serde(default)]
    pub builtin: Option<String>,
    /// Whether the caller is in it.
    #[serde(default)]
    pub is_member: bool,
    /// Whether the caller created it and still holds its creator's
    /// authority, so they may add themselves to it.
    #[serde(default)]
    pub created_by_me: bool,
    /// The caller's actions on this group (see [`action`]).
    #[serde(default)]
    pub actions: BTreeSet<String>,
}

impl ClanGroup {
    /// Whether this is one of the two built-in groups, whose members follow
    /// the clan and whose name is fixed.
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        self.builtin.is_some()
    }

    /// Whether the caller holds `action` (see [`action`]) on this group.
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.contains(action)
    }
}

/// `PATCH /clans/{c}/groups/{g}`: `None` keeps a field; `color:
/// Some(None)` clears the color.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClanGroupPatch {
    pub name: Option<String>,
    pub color: Option<Option<String>>,
}

impl ClanGroupPatch {
    fn to_body(&self) -> Value {
        let mut body = Map::new();
        if let Some(name) = &self.name {
            body.insert("name".to_string(), json!(name));
        }
        if let Some(color) = &self.color {
            body.insert("color".to_string(), json!(color));
        }
        Value::Object(body)
    }
}

impl CloudApiClient {
    // ===== clans ==========================================================

    /// `GET /clans`: the caller's clans and their pending invitations.
    ///
    /// # Errors
    /// [`CloudError::EmailNotVerified`](crate::CloudError::EmailNotVerified)
    /// until the email is verified; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clans(&self) -> CloudResult<ClansOverview> {
        self.get("/clans").await
    }

    /// `GET /clans/{c}`: one of the caller's clans.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// when the caller is not a member; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan(&self, clan_id: Uuid) -> CloudResult<ClanSummary> {
        self.get(&format!("/clans/{clan_id}")).await
    }

    /// `POST /clans`: founds a clan with the caller as its first member and
    /// owner.
    ///
    /// # Errors
    /// [`CloudError::InvalidInput`](crate::CloudError::InvalidInput) for a
    /// blank, overlong, or control-character name; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn create_clan(&self, name: &str) -> CloudResult<ClanSummary> {
        let body = json!({ "name": name });
        self.post("/clans", Some(&body), Auth::Required).await
    }

    /// `PATCH /clans/{c}`: renames the clan. Needs `clan.edit_profile`.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// without that action; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn rename_clan(&self, clan_id: Uuid, name: &str) -> CloudResult<ClanSummary> {
        let body = json!({ "name": name });
        self.patch(&format!("/clans/{clan_id}"), &body).await
    }

    /// `DELETE /clans/{c}`: dissolves the clan. Owners only.
    ///
    /// # Errors
    /// [`CloudError::ClanNotEmpty`](crate::CloudError::ClanNotEmpty) while it
    /// owns maps; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn delete_clan(&self, clan_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/clans/{clan_id}")).await
    }

    // ===== members and owners =============================================

    /// `GET /clans/{c}/members`: owners first, then by joining time. Needs
    /// `clan.read_members`.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// without that action; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan_members(&self, clan_id: Uuid) -> CloudResult<Vec<ClanMember>> {
        self.get(&format!("/clans/{clan_id}/members")).await
    }

    /// `PATCH /clans/{c}/members/{u}`: makes a member an owner, or not,
    /// possibly the caller. Owners only; idempotent.
    ///
    /// # Errors
    /// [`CloudError::LastOwner`](crate::CloudError::LastOwner) for the last
    /// owner; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn set_clan_owner(
        &self,
        clan_id: Uuid,
        user_id: Uuid,
        is_owner: bool,
    ) -> CloudResult<ClanMember> {
        let body = json!({ "is_owner": is_owner });
        self.patch(&format!("/clans/{clan_id}/members/{user_id}"), &body)
            .await
    }

    /// `DELETE /clans/{c}/members/{u}`: removes a member. With the caller's
    /// own ID it leaves the clan instead.
    ///
    /// # Errors
    /// [`CloudError::LastOwner`](crate::CloudError::LastOwner) when the last
    /// owner leaves; [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// without `clan.remove_member` (and ownership, to remove an owner);
    /// other failures via [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn remove_clan_member(&self, clan_id: Uuid, user_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/clans/{clan_id}/members/{user_id}"))
            .await
    }

    // ===== invitations ====================================================

    /// `POST /clans/{c}/invitations`: invites a user (found through
    /// [`CloudApiClient::lookup`]), proposing custom groups. Inviting again
    /// replaces the pending invitation's inviter and groups. Needs
    /// `clan.invite`.
    ///
    /// # Errors
    /// [`CloudError::AlreadyMember`](crate::CloudError::AlreadyMember) for a
    /// member; the uniform 404 when a block stands between the two users or
    /// the caller may not invite; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn invite_to_clan(
        &self,
        clan_id: Uuid,
        user_id: Uuid,
        group_ids: &[Uuid],
    ) -> CloudResult<ClanInvitation> {
        let body = if group_ids.is_empty() {
            json!({ "user_id": user_id })
        } else {
            json!({ "user_id": user_id, "group_ids": group_ids })
        };
        self.post(
            &format!("/clans/{clan_id}/invitations"),
            Some(&body),
            Auth::Required,
        )
        .await
    }

    /// `GET /clans/{c}/invitations`: pending invitations, oldest first. All
    /// of them with `clan.revoke_invitation`, otherwise the caller's own with
    /// `clan.invite`.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// with neither action; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan_invitations(&self, clan_id: Uuid) -> CloudResult<Vec<ClanInvitation>> {
        self.get(&format!("/clans/{clan_id}/invitations")).await
    }

    /// `DELETE /clan-invitations/{i}`: revokes a pending invitation. Its
    /// inviter, or a holder of `clan.revoke_invitation`.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn revoke_clan_invitation(&self, invitation_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/clan-invitations/{invitation_id}"))
            .await
    }

    /// `GET /me/clan-invitations`: the invitations the caller has received,
    /// oldest first.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn my_clan_invitations(&self) -> CloudResult<Vec<ReceivedInvitation>> {
        self.get("/me/clan-invitations").await
    }

    /// `POST /clan-invitations/{i}/accept`: joins the clan.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// when the invitation is gone or void (its inviter may no longer
    /// invite); other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn accept_clan_invitation(&self, invitation_id: Uuid) -> CloudResult<ClanSummary> {
        self.post(
            &format!("/clan-invitations/{invitation_id}/accept"),
            None,
            Auth::Required,
        )
        .await
    }

    /// `POST /clan-invitations/{i}/decline`.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn decline_clan_invitation(&self, invitation_id: Uuid) -> CloudResult<()> {
        self.post_unit(
            &format!("/clan-invitations/{invitation_id}/decline"),
            None,
            Auth::Required,
        )
        .await
    }

    // ===== groups =========================================================

    /// `GET /clans/{c}/groups`: built-ins first, then by name. Any member.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan_groups(&self, clan_id: Uuid) -> CloudResult<Vec<ClanGroup>> {
        self.get(&format!("/clans/{clan_id}/groups")).await
    }

    /// `POST /clans/{c}/groups`: creates a custom group. Needs
    /// `group.create`.
    ///
    /// # Errors
    /// [`CloudError::NameInUse`](crate::CloudError::NameInUse) when the clan
    /// already has a group of that name, ignoring case; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn create_clan_group(
        &self,
        clan_id: Uuid,
        name: &str,
        color: Option<&str>,
    ) -> CloudResult<ClanGroup> {
        let body = match color {
            Some(color) => json!({ "name": name, "color": color }),
            None => json!({ "name": name }),
        };
        self.post(
            &format!("/clans/{clan_id}/groups"),
            Some(&body),
            Auth::Required,
        )
        .await
    }

    /// `PATCH /clans/{c}/groups/{g}`: renames a custom group or changes a
    /// group's color. Needs `group.rename`.
    ///
    /// # Errors
    /// [`CloudError::NameInUse`](crate::CloudError::NameInUse) for a name the
    /// clan already uses; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn update_clan_group(
        &self,
        clan_id: Uuid,
        group_id: Uuid,
        patch: &ClanGroupPatch,
    ) -> CloudResult<ClanGroup> {
        self.patch(
            &format!("/clans/{clan_id}/groups/{group_id}"),
            &patch.to_body(),
        )
        .await
    }

    /// `DELETE /clans/{c}/groups/{g}`: deletes a custom group, and what was
    /// shared with it. Needs `group.delete`.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn delete_clan_group(&self, clan_id: Uuid, group_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/clans/{clan_id}/groups/{group_id}"))
            .await
    }

    /// `GET /clans/{c}/groups/{g}/members`: the group's roster. Needs
    /// `group.inspect_assignments`.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan_group_members(
        &self,
        clan_id: Uuid,
        group_id: Uuid,
    ) -> CloudResult<Vec<UserRef>> {
        self.get(&format!("/clans/{clan_id}/groups/{group_id}/members"))
            .await
    }

    /// `PUT /clans/{c}/groups/{g}/members/{u}`: adds a member to a custom
    /// group. Needs `group.assign`; idempotent.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn add_clan_group_member(
        &self,
        clan_id: Uuid,
        group_id: Uuid,
        user_id: Uuid,
    ) -> CloudResult<()> {
        self.put_unit(&format!(
            "/clans/{clan_id}/groups/{group_id}/members/{user_id}"
        ))
        .await
    }

    /// `DELETE /clans/{c}/groups/{g}/members/{u}`: takes a member out of a
    /// custom group. Needs `group.assign`; idempotent.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn remove_clan_group_member(
        &self,
        clan_id: Uuid,
        group_id: Uuid,
        user_id: Uuid,
    ) -> CloudResult<()> {
        self.delete(&format!(
            "/clans/{clan_id}/groups/{group_id}/members/{user_id}"
        ))
        .await
    }

    // ===== clan folders and maps ==========================================

    /// `POST /atlases` with `clan_id`: creates a folder in the clan. Needs
    /// `atlas.create`.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// without that action; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn create_clan_atlas(&self, clan_id: Uuid, name: &str) -> CloudResult<Atlas> {
        let body = json!({ "name": name, "clan_id": clan_id });
        self.post("/atlases", Some(&body), Auth::Required).await
    }

    // ===== grants =========================================================

    /// `GET /clans/{c}/grants`: the grants the caller may see, oldest first.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan_grants(
        &self,
        clan_id: Uuid,
        filter: ClanGrantFilter,
    ) -> CloudResult<Vec<ClanGrant>> {
        self.get_with_query(&format!("/clans/{clan_id}/grants"), &filter.query())
            .await
    }

    /// `DELETE /clans/{c}/grants/{g}`, with every action that came through
    /// it.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn delete_clan_grant(&self, clan_id: Uuid, grant_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/clans/{clan_id}/grants/{grant_id}"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Area;

    #[test]
    fn clans_overview_parses_the_wire_shape() {
        let overview: ClansOverview = serde_json::from_value(json!({
            "clans": [{
                "id": "11111111-1111-4111-8111-111111111111",
                "name": "Lantern Company",
                "description": "",
                "created_at": "2026-10-01T12:00:00.123456Z",
                "member_count": 24,
                "is_owner": false,
                "group_ids": ["22222222-2222-4222-8222-222222222222"],
                "actions": ["clan.invite", "clan.read_members"]
            }],
            "invitations": [{
                "id": "33333333-3333-4333-8333-333333333333",
                "clan_id": "44444444-4444-4444-8444-444444444444",
                "clan_name": "Roads",
                "inviter_id": "55555555-5555-4555-8555-555555555555",
                "inviter_nickname": null,
                "created_at": "2026-10-02T00:00:00Z"
            }]
        }))
        .unwrap();
        let clan = &overview.clans[0];
        assert_eq!(clan.member_count, 24);
        assert!(clan.can(action::INVITE));
        assert!(!clan.can(action::EDIT_PROFILE));
        assert_eq!(overview.invitations[0].inviter_nickname, None);
    }

    #[test]
    fn groups_parse_builtins_and_colors() {
        let groups: Vec<ClanGroup> = serde_json::from_value(json!([
            {
                "id": "11111111-1111-4111-8111-111111111111",
                "name": "Owner",
                "color": null,
                "builtin": "owners",
                "is_member": true,
                "actions": []
            },
            {
                "id": "22222222-2222-4222-8222-222222222222",
                "name": "City mappers",
                "color": "#a1b2c3",
                "builtin": null,
                "is_member": false,
                "created_by_me": true,
                "actions": ["group.assign", "group.rename"]
            }
        ]))
        .unwrap();
        assert!(groups[0].is_builtin());
        assert!(!groups[1].is_builtin());
        assert_eq!(groups[1].color.as_deref(), Some("#a1b2c3"));
        assert!(groups[1].can(action::ASSIGN_GROUP));
        assert!(groups[1].created_by_me);
        assert!(!groups[0].created_by_me, "absent reads as false");
    }

    #[test]
    fn grant_scopes_and_recipients_take_the_wire_shape() {
        assert_eq!(
            serde_json::to_value(GrantScope::Clan).unwrap(),
            json!({ "kind": "clan" })
        );
        let atlas = AtlasId(Uuid::from_u128(7));
        assert_eq!(
            serde_json::to_value(GrantScope::Atlases { ids: vec![atlas] }).unwrap(),
            json!({ "kind": "atlases", "ids": [atlas] })
        );
        let group = Uuid::from_u128(9);
        assert_eq!(
            serde_json::to_value(GrantRecipient::Group { group_id: group }).unwrap(),
            json!({ "group_id": group })
        );
        let grant: ClanGrant = serde_json::from_value(json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "clan_id": "22222222-2222-4222-8222-222222222222",
            "recipient": { "group_id": group },
            "actions": ["area.read", "area.add", "area.edit", "area.remove_content"],
            "scope": { "kind": "clan" },
            "delegated": [],
            "issuer_id": "33333333-3333-4333-8333-333333333333",
            "created_at": "2026-10-01T12:00:00.123456Z",
            "updated_at": "2026-10-01T12:00:00.123456Z"
        }))
        .unwrap();
        assert_eq!(grant.recipient.group(), Some(group));
        assert_eq!(grant.scope, GrantScope::Clan);
        assert!(grant.actions.contains(action::REMOVE_FROM_AREA));
        assert!(grant.gives_map_access());
        assert_eq!(grant.direct_actions().len(), 4);
    }

    #[test]
    fn delegated_actions_are_told_apart_from_the_grants_own() {
        let (first, second) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let grant: ClanGrant = serde_json::from_value(json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "clan_id": "22222222-2222-4222-8222-222222222222",
            "recipient": { "group_id": "44444444-4444-4444-8444-444444444444" },
            "actions": ["area.read", "area.add", "area.edit"],
            "scope": { "kind": "atlases", "ids": ["55555555-5555-4555-8555-555555555555"] },
            "delegated": [
                { "delegation_id": first, "actions": ["area.add", "area.edit"] },
                { "delegation_id": second, "actions": ["area.edit"] }
            ],
            "issuer_id": "33333333-3333-4333-8333-333333333333",
            "created_at": "2026-10-01T12:00:00Z",
            "updated_at": "2026-10-01T12:00:00Z"
        }))
        .unwrap();
        assert_eq!(grant.direct_actions(), [action::READ_AREA].into());
        assert!(grant.delegations_of(action::READ_AREA).is_empty());
        assert_eq!(grant.delegations_of(action::ADD_TO_AREA), [first]);
        assert_eq!(grant.delegations_of(action::EDIT_AREA), [first, second]);
        assert!(grant.delegations_of(action::DELETE_AREA).is_empty());

        // A grant without the list holds only its own actions.
        let mut wire = serde_json::to_value(&grant).unwrap();
        wire.as_object_mut().unwrap().remove("delegated");
        let bare: ClanGrant = serde_json::from_value(wire).unwrap();
        assert_eq!(bare.direct_actions().len(), 3);
    }

    #[test]
    fn clan_map_rows_carry_their_clan_and_ownership() {
        let row: Area = serde_json::from_value(json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "user_id": null,
            "clan_id": "22222222-2222-4222-8222-222222222222",
            "clan_name": "Lantern Company",
            "atlas_id": "44444444-4444-4444-8444-444444444444",
            "name": "Solace",
            "created_at": "2026-10-01T12:00:00Z",
            "projection_token": "p_1",
            "access": {
                "is_owner": false, "can_edit": true, "can_reshare": false,
                "can_copy": false, "can_admin": false, "include_secrets": false
            },
            "actions": ["area.read", "area.edit"],
            "ownership": "clan"
        }))
        .unwrap();
        assert_eq!(row.user_id, None);
        assert!(row.clan_id.is_some());
        assert!(row.can(action::EDIT_AREA));
        assert!(!row.can(action::DELETE_AREA));
        assert_eq!(
            row.clan_ownership.ownership,
            Some(crate::clan_maps::MapOwnership::Clan)
        );
    }

    #[test]
    fn group_patch_sends_only_what_changes() {
        assert_eq!(ClanGroupPatch::default().to_body(), json!({}));
        let rename = ClanGroupPatch {
            name: Some("Scouts".to_string()),
            color: None,
        };
        assert_eq!(rename.to_body(), json!({ "name": "Scouts" }));
        let clear = ClanGroupPatch {
            name: None,
            color: Some(None),
        };
        assert_eq!(clear.to_body(), json!({ "color": null }));
    }
}
