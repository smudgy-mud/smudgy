//! Clan Secrets: creating them, sharing them within their clan, who reads
//! them and why, their recorded owners, and offers of their ownership
//! (`/secrets/{s}/grants`, `/secrets/{s}/access`, `/secrets/{s}/owners`,
//! `/secrets/{s}/transfer`, `/me/secret-offers`).
//!
//! A Clan Secret belongs to one clan and is either Member-owned (its recorded
//! owners hold ownership authority) or Clan-owned (the clan's owners do). A
//! reader learns its badge ([`SecretSummary::ownership`]) and their own
//! actions on it, nothing more; who else reads it needs `manage_access`.
//!
//! Every refusal that could reveal a Secret the caller cannot read, or an
//! action they do not hold, is the uniform
//! [`NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess), exactly as
//! for a Secret that does not exist.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::clans::{ClanGroup, ClanMember};
use crate::cloud_api::{Auth, CloudApiClient, SecretSummary};
use crate::{AreaId, AtlasId, CloudError, CloudResult, SourceId};

/// A Secret's `ownership` badge, as the wire names it.
pub mod ownership {
    /// An owner Secret: the map owner's, on a user's own map.
    pub const OWNER: &str = "owner";
    /// A Member-owned Clan Secret: its recorded owners hold authority.
    pub const MEMBERS: &str = "members";
    /// A Clan-owned Clan Secret: the clan's owners hold authority.
    pub const CLAN: &str = "clan";
}

/// The Secret actions beyond the content actions: only ownership authority
/// holds these.
pub mod authority_action {
    /// Renaming the Secret or changing its color.
    pub const RENAME: &str = "rename";
    /// Deleting the Secret.
    pub const DELETE: &str = "delete";
    /// Removing owners and offering ownership.
    pub const MANAGE_OWNERSHIP: &str = "manage_ownership";
}

/// The clan map actions that create Clan Secrets on a map.
pub mod creation_action {
    /// Creating Member-owned Secrets, with the creator as first owner.
    pub const MEMBER_OWNED: &str = "secret.create_member_owned";
    /// Creating Clan-owned Secrets, with the creator as a Contributor.
    pub const CLAN_OWNED: &str = "secret.create_clan_owned";
}

/// Who owns a new Secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSecretOwner {
    /// An owner Secret, which only the owner of a user's map creates.
    Me,
    /// A Member-owned Secret of `clan_id`, with the creator as its first
    /// recorded owner. Needs `secret.create_member_owned` on the map.
    Members { clan_id: Uuid },
    /// A Clan-owned Secret of `clan_id`, whose creator starts as a
    /// Contributor. Needs `secret.create_clan_owned` on the map.
    Clan { clan_id: Uuid },
}

/// A Secret to create: `POST /areas/{a}/secrets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSecret {
    pub name: String,
    /// `#rrggbb`, or `None` for the palette's color.
    pub color: Option<String>,
    pub owner: NewSecretOwner,
}

impl NewSecret {
    /// An owner Secret named `name`.
    #[must_use]
    pub fn owner(name: &str, color: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            color: color.map(ToString::to_string),
            owner: NewSecretOwner::Me,
        }
    }

    /// The request body. An owner Secret sends no `ownership`, which the
    /// server reads as an owner Secret; a Clan Secret always names its clan.
    #[must_use]
    pub fn body(&self) -> Value {
        let mut body = Map::new();
        body.insert("name".to_string(), self.name.clone().into());
        if let Some(color) = &self.color {
            body.insert("color".to_string(), color.clone().into());
        }
        match self.owner {
            NewSecretOwner::Me => {}
            NewSecretOwner::Members { clan_id } => {
                body.insert("ownership".to_string(), ownership::MEMBERS.into());
                body.insert("clan_id".to_string(), clan_id.to_string().into());
            }
            NewSecretOwner::Clan { clan_id } => {
                body.insert("ownership".to_string(), ownership::CLAN.into());
                body.insert("clan_id".to_string(), clan_id.to_string().into());
            }
        }
        Value::Object(body)
    }
}

/// Why a member reads a Clan Secret (`GET /secrets/{s}/access`). Each
/// reason but ownership carries the actions it gives, within the member's
/// own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccessReason {
    /// A recorded owner of a Member-owned Secret.
    Owner,
    /// A clan owner, on a Clan-owned Secret.
    ClanOwner,
    /// The Secret's grant to them.
    Grant {
        grant_id: Uuid,
        #[serde(default)]
        actions: Vec<String>,
    },
    /// The Secret's grant to a group they are in.
    Group {
        grant_id: Uuid,
        group_id: Uuid,
        #[serde(default)]
        actions: Vec<String>,
    },
    /// The clan grants covering the map, on a Clan-owned Secret.
    ClanGrants {
        #[serde(default)]
        actions: Vec<String>,
    },
    /// A reason this client does not know yet.
    #[serde(other)]
    Other,
}

impl AccessReason {
    /// Whether the reason is ownership authority.
    #[must_use]
    pub const fn is_ownership(&self) -> bool {
        matches!(self, Self::Owner | Self::ClanOwner)
    }
}

/// One member who reads a Clan Secret, as the caller may see them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretReader {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    /// Their actions on the Secret, in the server's order.
    pub actions: Vec<String>,
    pub reasons: Vec<AccessReason>,
}

impl SecretReader {
    /// Whether they hold `action` on the Secret.
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.iter().any(|held| held == action)
    }
}

/// Who reads a Clan Secret now, and why. Holders of `manage_access` see
/// every member they may (group rosters and clan grants are clan
/// administration); any other reader sees only themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretAccess {
    pub secret_id: Uuid,
    pub area_id: AreaId,
    pub clan_id: Uuid,
    /// [`ownership::MEMBERS`] or [`ownership::CLAN`].
    pub ownership: String,
    /// Clan owners first, then by joining.
    pub members: Vec<SecretReader>,
}

/// A recorded owner of a Member-owned Secret (`GET /secrets/{s}/owners`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretOwner {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    /// False for an owner who is not an active member: their ownership is
    /// dormant and gives nothing.
    pub active: bool,
    pub added_at: DateTime<Utc>,
}

/// One user an ownership offer names, and whether they have accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferRecipient {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    pub accepted: bool,
}

/// A pending offer of a Clan Secret's ownership. An offer to several users
/// is joint: each accepts on their own, and the last acceptance applies it
/// for all of them at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipOffer {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub secret_name: String,
    #[serde(default)]
    pub secret_color: Option<String>,
    pub area_id: AreaId,
    pub clan_id: Uuid,
    pub recipients: Vec<OfferRecipient>,
    /// What the Secret becomes: [`ownership::MEMBERS`] (the recipients
    /// become owners) or [`ownership::CLAN`].
    pub ownership: String,
    /// Between members: whether the recipients become its only owners.
    #[serde(default)]
    pub replace: bool,
    pub initiator_id: Uuid,
    #[serde(default)]
    pub initiator_nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl OwnershipOffer {
    /// The Secret the offer is for, as a source of its map.
    #[must_use]
    pub const fn source(&self) -> SourceId {
        SourceId::Secret(self.secret_id)
    }

    /// Whether the offer names several users.
    #[must_use]
    pub fn is_joint(&self) -> bool {
        self.recipients.len() > 1
    }

    /// Whether `user` is named and has accepted.
    #[must_use]
    pub fn accepted_by(&self, user: Uuid) -> bool {
        self.recipients
            .iter()
            .any(|recipient| recipient.user_id == user && recipient.accepted)
    }
}

/// What an ownership offer proposes: `POST /secrets/{s}/transfer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferRequest {
    /// One to 16 distinct users; exactly one to make the Secret Clan-owned.
    pub user_ids: Vec<Uuid>,
    /// [`ownership::MEMBERS`] or [`ownership::CLAN`].
    pub ownership: &'static str,
    /// Between members: the recipients become the Secret's only owners.
    pub replace: bool,
}

impl OfferRequest {
    /// The recipients become owners: beside the current ones, or with
    /// `replace` in their place. On a Clan-owned Secret this makes it
    /// Member-owned, with the recipients its only owners.
    #[must_use]
    pub fn to_members(user_ids: Vec<Uuid>, replace: bool) -> Self {
        Self {
            user_ids,
            ownership: ownership::MEMBERS,
            replace,
        }
    }

    /// The Secret becomes Clan-owned; `clan_owner` accepts for the clan.
    #[must_use]
    pub fn to_clan(clan_owner: Uuid) -> Self {
        Self {
            user_ids: vec![clan_owner],
            ownership: ownership::CLAN,
            replace: false,
        }
    }

    fn body(&self) -> Value {
        let mut body = json!({
            "user_ids": self.user_ids,
            "ownership": self.ownership,
        });
        if self.replace {
            body["replace"] = Value::Bool(true);
        }
        body
    }
}

/// The grant presets: inline bundles of a Clan Secret grant's actions.
/// There are no Secret-local roles; any set of the grantable actions is a
/// valid grant. `copy` is in none of them: a grant carries it only when it
/// names it.
pub mod secret_preset {
    /// Reading only.
    pub const READER: &[&str] = &["read"];
    /// Reading, adding and changing content.
    pub const CONTRIBUTOR: &[&str] = &["read", "add", "edit"];
    /// Reading, adding, changing and deleting content.
    pub const EDITOR: &[&str] = &["read", "add", "edit", "remove"];
    /// Reading, and sharing within the holder's own actions.
    pub const ACCESS_MANAGER: &[&str] = &["read", "manage_access"];
}

/// Who a Clan Secret grant names: an active member of its clan, or one of
/// the clan's groups (built-ins included). A group grant reaches whoever is
/// in the group when access is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SecretRecipient {
    User { user_id: Uuid },
    Group { group_id: Uuid },
}

impl SecretRecipient {
    /// The member this grant names, if it names one.
    #[must_use]
    pub const fn user(&self) -> Option<Uuid> {
        match self {
            Self::User { user_id } => Some(*user_id),
            Self::Group { .. } => None,
        }
    }

    /// The group this grant names, if it names one.
    #[must_use]
    pub const fn group(&self) -> Option<Uuid> {
        match self {
            Self::Group { group_id } => Some(*group_id),
            Self::User { .. } => None,
        }
    }
}

/// One grant on a Clan Secret (`/secrets/{s}/grants`). There is one grant
/// per recipient; it gives nothing while its recipient is not an active
/// member who reads the map, and stays when its grantor loses their own
/// access or leaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanSecretGrant {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub area_id: AreaId,
    pub clan_id: Uuid,
    pub recipient: SecretRecipient,
    /// Always `read`, plus any of `add`, `edit`, `remove`, `manage_access`
    /// and `copy`.
    pub actions: BTreeSet<String>,
    pub grantor_id: Uuid,
    /// A member recipient's nickname, when they have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor_nickname: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ClanSecretGrant {
    /// Whether the grant carries `action`.
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.contains(action)
    }
}

/// Whom a member may name as a Clan Secret's recipients: its clan's groups,
/// which any member reads, and its members, when the caller may read the
/// member directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClanRecipients {
    pub groups: Vec<ClanGroup>,
    /// `None` when the caller does not hold `clan.read_members`: groups are
    /// then the only recipients they can name.
    pub members: Option<Vec<ClanMember>>,
}

/// One map in a clan's access index (`GET /clans/{c}/resources`), with the
/// caller's own clan actions on it there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanMapResource {
    pub id: AreaId,
    /// `None` for a map filed in by clan link, whose name its owner holds.
    #[serde(default)]
    pub name: Option<String>,
    /// The clan folder it is filed in.
    #[serde(default)]
    pub atlas_id: Option<AtlasId>,
    pub actions: BTreeSet<String>,
}

#[derive(Deserialize)]
struct ResourceRow {
    kind: String,
    #[serde(flatten)]
    map: Value,
}

/// The path of a Clan Secret's route `rest` (`access`, `owners`, ...).
/// Only Secrets have these; the map and Private are refused before
/// anything is sent.
fn secret_path(secret: &SourceId, rest: &str) -> CloudResult<String> {
    match secret {
        SourceId::Secret(id) => Ok(format!("/secrets/{id}/{rest}")),
        SourceId::Map | SourceId::Private => Err(CloudError::InvalidInput(
            "only Secrets have grants, owners and offers".to_string(),
        )),
    }
}

impl CloudApiClient {
    // Sharing a Clan Secret within its clan (clans.md §8.4). Ownership
    // authority grants, changes and revokes any grant; a holder of
    // `manage_access` grants only actions it holds, never `manage_access`,
    // and changes or revokes only grants that do not carry it. Recipients
    // are active members and the clan's groups. Every refusal past the
    // request's shape (a recipient outside the clan, an action beyond the
    // caller's own, a grant they may not change) is the uniform
    // [`CloudError::NotFoundOrNoAccess`]; a malformed body or unknown
    // action is [`CloudError::InvalidInput`].

    /// `GET /secrets/{s}/grants` on a Clan Secret: every grant for holders
    /// of `manage_access`, oldest first; another reader sees the grants to
    /// them and to their groups.
    ///
    /// # Errors
    /// See the section comment; [`CloudError::InvalidInput`] for the map or
    /// Private.
    pub async fn clan_secret_grants(&self, secret: &SourceId) -> CloudResult<Vec<ClanSecretGrant>> {
        self.get(&secret_path(secret, "grants")?).await
    }

    /// `clan_id`'s groups and, when the caller holds `clan.read_members`, its
    /// members: the recipients a Clan Secret of that clan can be shared
    /// with, as far as the caller can name them. A refused member directory
    /// leaves the members out rather than failing.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] for a clan the caller is not in;
    /// other failures of either read.
    pub async fn clan_recipients(&self, clan_id: Uuid) -> CloudResult<ClanRecipients> {
        let groups = self.clan_groups(clan_id).await?;
        let members = match self.clan_members(clan_id).await {
            Ok(members) => Some(members),
            Err(CloudError::NotFoundOrNoAccess) => None,
            Err(error) => return Err(error),
        };
        Ok(ClanRecipients { groups, members })
    }

    /// `POST /secrets/{s}/grants` on a Clan Secret: shares it with a member
    /// or a group. `read` is implied. A recipient that already has a grant
    /// has its actions replaced, under the rules for changing it.
    ///
    /// # Errors
    /// See the section comment.
    pub async fn grant_clan_secret(
        &self,
        secret: &SourceId,
        recipient: SecretRecipient,
        actions: &[&str],
    ) -> CloudResult<ClanSecretGrant> {
        let body = json!({ "recipient": recipient, "actions": actions });
        self.post(&secret_path(secret, "grants")?, Some(&body), Auth::Required)
            .await
    }

    /// `PATCH /secrets/{s}/grants/{g}` on a Clan Secret: replaces what the
    /// grant gives.
    ///
    /// # Errors
    /// See the section comment.
    pub async fn update_clan_secret_grant(
        &self,
        secret: &SourceId,
        grant_id: Uuid,
        actions: &[&str],
    ) -> CloudResult<ClanSecretGrant> {
        let body = json!({ "actions": actions });
        self.patch(&secret_path(secret, &format!("grants/{grant_id}"))?, &body)
            .await
    }

    /// `DELETE /secrets/{s}/grants/{g}` on a Clan Secret: revokes a grant.
    ///
    /// # Errors
    /// See the section comment.
    pub async fn revoke_clan_secret_grant(
        &self,
        secret: &SourceId,
        grant_id: Uuid,
    ) -> CloudResult<()> {
        self.delete(&secret_path(secret, &format!("grants/{grant_id}"))?)
            .await
    }

    /// `GET /clans/{c}/resources?kind=areas`: the clan's maps the caller
    /// holds an action on, its own and those filed in by link, with the
    /// caller's actions there.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] when the caller is not a member;
    /// other failures via [`CloudError::from_status`].
    pub async fn clan_map_resources(&self, clan_id: Uuid) -> CloudResult<Vec<ClanMapResource>> {
        let rows: Vec<ResourceRow> = self
            .get_with_query(
                &format!("/clans/{clan_id}/resources"),
                &[("kind", "areas".to_string())],
            )
            .await?;
        rows.into_iter()
            .filter(|row| row.kind == "area")
            .map(|row| serde_json::from_value(row.map).map_err(CloudError::from))
            .collect()
    }

    /// `GET /secrets/{s}/access`: who reads a Clan Secret now, and why.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] for an owner Secret, and for a
    /// Secret the caller does not read; [`CloudError::InvalidInput`] for the
    /// map or Private; other failures via [`CloudError::from_status`].
    pub async fn secret_access(&self, secret: &SourceId) -> CloudResult<SecretAccess> {
        self.get(&secret_path(secret, "access")?).await
    }

    /// `GET /secrets/{s}/owners`: a Member-owned Secret's recorded owners,
    /// oldest first, dormant ones included. Needs `manage_access`.
    ///
    /// # Errors
    /// As [`Self::secret_access`].
    pub async fn secret_owners(&self, secret: &SourceId) -> CloudResult<Vec<SecretOwner>> {
        self.get(&secret_path(secret, "owners")?).await
    }

    /// `DELETE /secrets/{s}/owners/{u}`: removes a recorded owner, or with
    /// the caller's own ID gives up their ownership. Needs
    /// `manage_ownership`.
    ///
    /// # Errors
    /// [`CloudError::LastOwner`] for the last recorded owner; otherwise as
    /// [`Self::secret_access`].
    pub async fn remove_secret_owner(&self, secret: &SourceId, user_id: Uuid) -> CloudResult<()> {
        self.delete(&secret_path(secret, &format!("owners/{user_id}"))?)
            .await
    }

    /// `POST /secrets/{s}/transfer`: offers the Secret's ownership. Needs
    /// `manage_ownership`. A new offer replaces every pending offer naming
    /// any of its recipients.
    ///
    /// # Errors
    /// [`CloudError::InvalidInput`] for a malformed offer (no recipients,
    /// more than 16, several to make it Clan-owned); every other refusal,
    /// including a recipient who does not read the Secret, is
    /// [`CloudError::NotFoundOrNoAccess`].
    pub async fn offer_secret_ownership(
        &self,
        secret: &SourceId,
        offer: &OfferRequest,
    ) -> CloudResult<OwnershipOffer> {
        self.post(
            &secret_path(secret, "transfer")?,
            Some(&offer.body()),
            Auth::Required,
        )
        .await
    }

    /// `GET /secrets/{s}/transfer`: the Secret's pending ownership offers,
    /// oldest first: all of them with `manage_ownership`, otherwise those
    /// naming the caller.
    ///
    /// # Errors
    /// As [`Self::secret_access`].
    pub async fn secret_offers(&self, secret: &SourceId) -> CloudResult<Vec<OwnershipOffer>> {
        self.get(&secret_path(secret, "transfer")?).await
    }

    /// `GET /me/secret-offers`: the pending ownership offers naming the
    /// caller, across their clans, on Secrets they read, oldest first.
    ///
    /// # Errors
    /// Failures via [`CloudError::from_status`].
    pub async fn my_secret_offers(&self) -> CloudResult<Vec<OwnershipOffer>> {
        self.get("/me/secret-offers").await
    }

    /// `POST /secrets/{s}/transfer/{o}/accept`: accepts an offer naming the
    /// caller. The acceptance that completes it applies it; the answer is
    /// the Secret as it stands afterwards.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] when the offer is gone or void
    /// (the Secret's owners or grants changed since it was made, or someone
    /// it names no longer qualifies).
    pub async fn accept_secret_offer(
        &self,
        secret: &SourceId,
        offer_id: Uuid,
    ) -> CloudResult<SecretSummary> {
        self.post(
            &secret_path(secret, &format!("transfer/{offer_id}/accept"))?,
            None,
            Auth::Required,
        )
        .await
    }

    /// `POST /secrets/{s}/transfer/{o}/decline`: declines an offer naming
    /// the caller, which ends it for every recipient.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] when the offer is gone.
    pub async fn decline_secret_offer(&self, secret: &SourceId, offer_id: Uuid) -> CloudResult<()> {
        self.post_unit(
            &secret_path(secret, &format!("transfer/{offer_id}/decline"))?,
            None,
            Auth::Required,
        )
        .await
    }

    /// `DELETE /secrets/{s}/transfer/{o}`: withdraws an offer. Needs
    /// `manage_ownership`.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] when the offer is gone or the
    /// caller may not withdraw it.
    pub async fn withdraw_secret_offer(
        &self,
        secret: &SourceId,
        offer_id: Uuid,
    ) -> CloudResult<()> {
        self.delete(&secret_path(secret, &format!("transfer/{offer_id}"))?)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_secret_names_its_owner() {
        let clan = Uuid::from_u128(1);
        assert_eq!(
            NewSecret::owner("Bookcase", None).body(),
            json!({ "name": "Bookcase" })
        );
        let members = NewSecret {
            name: "Quest".to_string(),
            color: Some("#8a5cf6".to_string()),
            owner: NewSecretOwner::Members { clan_id: clan },
        };
        assert_eq!(
            members.body(),
            json!({
                "name": "Quest",
                "color": "#8a5cf6",
                "ownership": "members",
                "clan_id": clan,
            })
        );
        let owned = NewSecret {
            name: "Survey".to_string(),
            color: None,
            owner: NewSecretOwner::Clan { clan_id: clan },
        };
        assert_eq!(
            owned.body(),
            json!({
                "name": "Survey",
                "ownership": "clan",
                "clan_id": clan,
            })
        );
    }

    #[test]
    fn clan_grants_name_a_member_or_a_group() {
        let grant: ClanSecretGrant = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "secret_id": "00000000-0000-0000-0000-000000000002",
            "area_id": "00000000-0000-0000-0000-000000000003",
            "clan_id": "00000000-0000-0000-0000-000000000004",
            "recipient": { "group_id": "00000000-0000-0000-0000-000000000005" },
            "actions": ["read", "add", "edit"],
            "grantor_id": "00000000-0000-0000-0000-000000000006",
            "grantor_nickname": "Nessa",
            "created_at": "2026-10-06T00:00:00Z",
            "updated_at": "2026-10-06T00:00:00Z",
        }))
        .unwrap();
        assert_eq!(grant.recipient.group(), Some(Uuid::from_u128(5)));
        assert_eq!(grant.recipient.user(), None);
        assert!(grant.can("edit") && !grant.can("remove"));
        assert_eq!(grant.grantor_nickname.as_deref(), Some("Nessa"));
        let member: SecretRecipient =
            serde_json::from_value(json!({ "user_id": "00000000-0000-0000-0000-000000000007" }))
                .unwrap();
        assert_eq!(member.user(), Some(Uuid::from_u128(7)));
        assert_eq!(
            serde_json::to_value(member).unwrap(),
            json!({ "user_id": "00000000-0000-0000-0000-000000000007" })
        );
    }

    #[test]
    fn access_parses_every_reason() {
        let access: SecretAccess = serde_json::from_value(json!({
            "secret_id": "00000000-0000-0000-0000-000000000001",
            "area_id": "00000000-0000-0000-0000-000000000002",
            "clan_id": "00000000-0000-0000-0000-000000000003",
            "ownership": "clan",
            "members": [
                {
                    "user_id": "00000000-0000-0000-0000-000000000004",
                    "nickname": "Nessa",
                    "actions": ["read", "add", "edit", "remove", "manage_access",
                                "rename", "delete", "manage_ownership"],
                    "reasons": [{ "kind": "clan_owner" }]
                },
                {
                    "user_id": "00000000-0000-0000-0000-000000000005",
                    "actions": ["read", "add"],
                    "reasons": [
                        { "kind": "group", "grant_id": "00000000-0000-0000-0000-000000000006",
                          "group_id": "00000000-0000-0000-0000-000000000007",
                          "actions": ["read", "add"] },
                        { "kind": "clan_grants", "actions": ["read"] },
                        { "kind": "someday" }
                    ]
                }
            ]
        }))
        .expect("parses");
        assert_eq!(access.members.len(), 2);
        assert!(access.members[0].reasons[0].is_ownership());
        assert_eq!(access.members[1].nickname, None);
        assert!(matches!(
            access.members[1].reasons[0],
            AccessReason::Group { .. }
        ));
        assert_eq!(access.members[1].reasons[2], AccessReason::Other);
        assert!(access.members[1].can("add"));
    }

    #[test]
    fn an_offer_parses_with_and_without_nicknames() {
        let offer: OwnershipOffer = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "secret_id": "00000000-0000-0000-0000-000000000002",
            "secret_name": "Phylactery quest",
            "secret_color": null,
            "area_id": "00000000-0000-0000-0000-000000000003",
            "clan_id": "00000000-0000-0000-0000-000000000004",
            "recipients": [
                { "user_id": "00000000-0000-0000-0000-000000000005", "nickname": "Ann", "accepted": true },
                { "user_id": "00000000-0000-0000-0000-000000000006", "accepted": false }
            ],
            "ownership": "members",
            "replace": false,
            "initiator_id": "00000000-0000-0000-0000-000000000007",
            "created_at": "2026-10-06T08:00:00.000000Z"
        }))
        .expect("parses");
        assert!(offer.is_joint());
        assert!(offer.accepted_by(Uuid::from_u128(5)));
        assert!(!offer.accepted_by(Uuid::from_u128(6)));
        assert_eq!(offer.initiator_nickname, None);
        assert_eq!(offer.source(), SourceId::Secret(Uuid::from_u128(2)));
    }

    #[test]
    fn an_offer_request_sends_replace_only_when_set() {
        let user = Uuid::from_u128(9);
        assert_eq!(
            OfferRequest::to_members(vec![user], false).body(),
            json!({ "user_ids": [user], "ownership": "members" })
        );
        assert_eq!(
            OfferRequest::to_members(vec![user], true).body(),
            json!({ "user_ids": [user], "ownership": "members", "replace": true })
        );
        assert_eq!(
            OfferRequest::to_clan(user).body(),
            json!({ "user_ids": [user], "ownership": "clan" })
        );
    }

    #[test]
    fn only_secrets_have_offers() {
        assert!(secret_path(&SourceId::Map, "access").is_err());
        assert!(secret_path(&SourceId::Private, "owners").is_err());
        assert_eq!(
            secret_path(&SourceId::Secret(Uuid::from_u128(1)), "access").unwrap(),
            "/secrets/00000000-0000-0000-0000-000000000001/access"
        );
    }
}
