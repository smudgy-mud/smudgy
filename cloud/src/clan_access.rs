//! A clan's access index and its administration: what the clan holds and who
//! reaches each of it (`GET /clans/{c}/resources`), delegating grants, and
//! the clan's profile.
//!
//! The index lists only what the caller holds an action on or may inspect
//! grants for: no row, name or count of anything else. A resource's `grants`
//! appear only for a caller who may inspect the grants covering it.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::clans::{ClanGrant, ClanSummary, GrantRecipient, GrantScope};
use crate::cloud_api::{Auth, CloudApiClient};
use crate::{AreaId, AtlasId, CloudResult};

/// Which of a clan's resources the index lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// Its maps.
    Areas,
    /// Its folders.
    Atlases,
    /// The Clan Secrets the caller reads.
    Secrets,
    /// The packages it owns that the caller holds a package action on.
    Packages,
}

impl ResourceKind {
    /// The `kind` query value.
    #[must_use]
    pub const fn as_query(self) -> &'static str {
        match self {
            Self::Areas => "areas",
            Self::Atlases => "atlases",
            Self::Secrets => "secrets",
            Self::Packages => "packages",
        }
    }
}

/// Who owns an indexed resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ResourceOwner {
    /// The clan.
    Clan,
    /// A user: a map filed in by an older clan link.
    User {
        id: Uuid,
        #[serde(default)]
        nickname: Option<String>,
    },
}

/// Where a grant listed on a resource reaches it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantThrough {
    /// A grant over the whole clan.
    Clan,
    /// A grant over the folder the map is filed in.
    Atlas,
    /// A grant naming the resource itself.
    Direct,
    /// A source this client does not know yet.
    #[serde(other)]
    Other,
}

/// One grant covering an indexed resource, with what it gives there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceGrant {
    pub grant_id: Uuid,
    pub recipient: GrantRecipient,
    /// The actions it gives on this resource.
    pub actions: BTreeSet<String>,
    pub through: GrantThrough,
    /// The folder, for a grant reaching a map through its folder.
    #[serde(default)]
    pub atlas_id: Option<AtlasId>,
}

/// One of a Clan-owned map's outside shares: view only, with a friend
/// outside the clan, lasting while its sharer holds `area.share_external`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideShare {
    /// The share's ID, which `DELETE /shares/{id}` revokes.
    pub id: Uuid,
    pub grantor_id: Uuid,
    #[serde(default)]
    pub grantor_nickname: Option<String>,
    pub grantee_id: Uuid,
    #[serde(default)]
    pub grantee_nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One row of the access index. The fields beyond `kind`, `id`, `name` and
/// `actions` depend on the kind: maps carry `atlas_id`, `owner` and
/// `ownership`; Secrets `color`, `area_id`, `atlas_id` and `ownership`;
/// packages `is_public` and `owner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedResource {
    /// `"area"`, `"atlas"`, `"secret"` or `"package"`.
    pub kind: String,
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    /// A map's filing, or a Secret's map's.
    #[serde(default)]
    pub atlas_id: Option<AtlasId>,
    #[serde(default)]
    pub owner: Option<ResourceOwner>,
    /// `"clan"` or `"members"` on maps and Secrets.
    #[serde(default)]
    pub ownership: Option<String>,
    /// On a Member-owned map: whether the caller is one of its owners.
    #[serde(default)]
    pub owned_by_me: bool,
    /// The caller's own actions on it.
    #[serde(default)]
    pub actions: BTreeSet<String>,
    /// The grants covering it, for a caller who may inspect them (a
    /// package's: grants scoped to that package).
    #[serde(default)]
    pub grants: Option<Vec<ResourceGrant>>,
    /// A Clan-owned map's live outside shares, oldest first, for clan owners
    /// and holders of `area.share_external` on it.
    #[serde(default)]
    pub outside_shares: Option<Vec<OutsideShare>>,
    /// A Secret's color.
    #[serde(default)]
    pub color: Option<String>,
    /// A Secret's map.
    #[serde(default)]
    pub area_id: Option<AreaId>,
    /// A package's availability.
    #[serde(default)]
    pub is_public: Option<bool>,
}

impl IndexedResource {
    /// Whether the caller holds `action` on it.
    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.contains(action)
    }

    /// Whether it is owned by the clan: a Clan-owned map or Secret, a
    /// folder, or a package. Member-owned maps and Secrets are not.
    #[must_use]
    pub fn clan_owned(&self) -> bool {
        self.ownership
            .as_deref()
            .is_none_or(|ownership| ownership == "clan")
            && !matches!(self.owner, Some(ResourceOwner::User { .. }))
    }
}

/// `PATCH /clans/{c}`: `None` keeps a field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClanProfilePatch {
    pub name: Option<String>,
    pub description: Option<String>,
}

impl ClanProfilePatch {
    fn to_body(&self) -> Value {
        let mut body = Map::new();
        if let Some(name) = &self.name {
            body.insert("name".to_string(), json!(name));
        }
        if let Some(description) = &self.description {
            body.insert("description".to_string(), json!(description));
        }
        Value::Object(body)
    }
}

/// A clan grant to write: what it gives, and, on a grant that hands out
/// access (`grant.manage`), what its holder may hand out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GrantBody {
    pub actions: Vec<String>,
    /// Required with `grant.manage` and refused without it.
    pub may_grant: Option<Vec<String>>,
}

impl GrantBody {
    /// A grant of `actions` that hands nothing out.
    #[must_use]
    pub fn of<I, S>(actions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            actions: actions.into_iter().map(Into::into).collect(),
            may_grant: None,
        }
    }

    fn insert_into(&self, body: &mut Map<String, Value>) {
        body.insert("actions".to_string(), json!(self.actions));
        if let Some(may_grant) = &self.may_grant {
            body.insert("may_grant".to_string(), json!(may_grant));
        }
    }
}

impl CloudApiClient {
    /// `GET /clans/{c}/resources?kind=…`: the clan's resources of `kind` that
    /// the caller holds an action on or may inspect grants for, by name.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// when the caller is not a member; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan_resources(
        &self,
        clan_id: Uuid,
        kind: ResourceKind,
    ) -> CloudResult<Vec<IndexedResource>> {
        self.get_with_query(
            &format!("/clans/{clan_id}/resources"),
            &[("kind", kind.as_query().to_string())],
        )
        .await
    }

    /// `PATCH /clans/{c}`: changes the clan's name or description. Needs
    /// `clan.edit_profile`.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// without that action; [`CloudError::InvalidInput`](crate::CloudError::InvalidInput)
    /// for a blank or overlong name or description; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn update_clan_profile(
        &self,
        clan_id: Uuid,
        patch: &ClanProfilePatch,
    ) -> CloudResult<ClanSummary> {
        self.patch(&format!("/clans/{clan_id}"), &patch.to_body())
            .await
    }

    /// `POST /clans/{c}/grants`: a grant of `body` to `recipient` over
    /// `scope`. A grant carrying `grant.manage` names its `may_grant`, and
    /// only clan owners write one; anyone else writes within their own
    /// delegation.
    ///
    /// # Errors
    /// [`CloudError::InvalidInput`](crate::CloudError::InvalidInput) for
    /// actions that do not apply to the scope, map access for a member, or
    /// a `may_grant` naming clan administration; the uniform 404 when the
    /// caller may not write it; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn grant_in_clan(
        &self,
        clan_id: Uuid,
        recipient: GrantRecipient,
        scope: &GrantScope,
        body: &GrantBody,
    ) -> CloudResult<ClanGrant> {
        let mut request = Map::new();
        request.insert("recipient".to_string(), json!(recipient));
        request.insert("scope".to_string(), json!(scope));
        body.insert_into(&mut request);
        self.post(
            &format!("/clans/{clan_id}/grants"),
            Some(&Value::Object(request)),
            Auth::Required,
        )
        .await
    }

    /// `PATCH /clans/{c}/grants/{g}`: replaces what the grant gives with
    /// `body`. Recipient and scope stay.
    ///
    /// # Errors
    /// As [`Self::grant_in_clan`].
    pub async fn change_clan_grant(
        &self,
        clan_id: Uuid,
        grant_id: Uuid,
        body: &GrantBody,
    ) -> CloudResult<ClanGrant> {
        let mut request = Map::new();
        body.insert_into(&mut request);
        self.patch(
            &format!("/clans/{clan_id}/grants/{grant_id}"),
            &Value::Object(request),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_rows_parse_each_kind() {
        let rows: Vec<IndexedResource> = serde_json::from_value(json!([
            {
                "kind": "area",
                "id": "11111111-1111-4111-8111-111111111111",
                "name": "Roads",
                "atlas_id": "22222222-2222-4222-8222-222222222222",
                "owner": { "kind": "clan" },
                "ownership": "clan",
                "actions": ["area.read"],
                "outside_shares": [{
                    "id": "77777777-7777-4777-8777-777777777777",
                    "grantor_id": "88888888-8888-4888-8888-888888888888",
                    "grantor_nickname": "Mira",
                    "grantee_id": "99999999-9999-4999-8999-999999999999",
                    "grantee_nickname": null,
                    "created_at": "2026-10-07T00:00:00Z"
                }],
                "grants": [{
                    "grant_id": "33333333-3333-4333-8333-333333333333",
                    "recipient": { "group_id": "44444444-4444-4444-8444-444444444444" },
                    "actions": ["area.read"],
                    "through": "atlas",
                    "atlas_id": "22222222-2222-4222-8222-222222222222"
                }]
            },
            {
                "kind": "package",
                "id": "55555555-5555-4555-8555-555555555555",
                "name": "trail-tools",
                "is_public": false,
                "owner": { "kind": "clan" },
                "actions": ["package.read"]
            },
            {
                "kind": "secret",
                "id": "66666666-6666-4666-8666-666666666666",
                "name": "Survey route",
                "color": "#8a5cf6",
                "area_id": "11111111-1111-4111-8111-111111111111",
                "atlas_id": null,
                "ownership": "members",
                "actions": ["read"]
            }
        ]))
        .unwrap();
        let grants = rows[0].grants.as_ref().unwrap();
        assert_eq!(grants[0].through, GrantThrough::Atlas);
        assert!(rows[0].clan_owned());
        assert!(rows[0].can("area.read"));
        let shares = rows[0].outside_shares.as_ref().unwrap();
        assert_eq!(shares[0].grantor_nickname.as_deref(), Some("Mira"));
        assert_eq!(shares[0].grantee_nickname, None);
        assert_eq!(rows[1].is_public, Some(false));
        assert!(rows[1].outside_shares.is_none());
        assert!(rows[1].grants.is_none());
        assert!(rows[1].clan_owned());
        assert!(!rows[2].clan_owned());
    }

    #[test]
    fn grant_bodies_carry_may_grant_only_when_given() {
        let mut body = Map::new();
        GrantBody::of(["area.read"]).insert_into(&mut body);
        assert_eq!(Value::Object(body), json!({ "actions": ["area.read"] }));
        let mut body = Map::new();
        GrantBody {
            actions: vec!["grant.inspect".to_string(), "grant.manage".to_string()],
            may_grant: Some(vec!["area.read".to_string()]),
        }
        .insert_into(&mut body);
        assert_eq!(
            Value::Object(body),
            json!({
                "actions": ["grant.inspect", "grant.manage"],
                "may_grant": ["area.read"]
            })
        );
    }

    #[test]
    fn profile_patches_send_only_what_changes() {
        assert_eq!(ClanProfilePatch::default().to_body(), json!({}));
        let patch = ClanProfilePatch {
            name: None,
            description: Some(String::new()),
        };
        assert_eq!(patch.to_body(), json!({ "description": "" }));
    }

    #[test]
    fn package_scopes_take_the_wire_shape() {
        let id = Uuid::from_u128(5);
        assert_eq!(
            serde_json::to_value(GrantScope::Packages { ids: vec![id] }).unwrap(),
            json!({ "kind": "packages", "ids": [id] })
        );
    }
}
