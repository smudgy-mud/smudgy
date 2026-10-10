//! Who owns a clan's map, and how that changes: Clan-owned and Member-owned
//! maps, a Member-owned map's recorded owners, ownership offers between
//! members and to the clan, and the copies a member takes when they leave
//! (`/areas/{a}/owners`, `/areas/{a}/ownership-offers`, `/me/area-offers`,
//! `/clans/{c}/area-offers`, `/clans/{c}/copies`).
//!
//! A Clan-owned map belongs to the clan: its owners hold it, and clan-wide
//! and folder grants reach it. A Member-owned map belongs to its recorded
//! owners: only grants naming that one map reach it, which its active owners
//! write, and the clan's owners hold nothing on it. To anyone who does not
//! read a Member-owned map the clan answers as if it did not exist.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::cloud_api::{Auth, CloudApiClient, CreateShareRequest, ShareGrant, ShareScope};
use crate::{Area, AreaId, AtlasId, CloudResult};

/// The most users one ownership offer names, for a map or a Secret; the
/// server refuses a longer list with a 400.
pub const MAX_OFFER_RECIPIENTS: usize = 16;

/// Whose a clan's map is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MapOwnership {
    /// The clan's: its owners hold it, and clan-wide and folder grants
    /// reach it.
    Clan,
    /// Its recorded owners': only grants naming the map alone reach it.
    Members,
}

impl MapOwnership {
    /// The wire name.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Clan => "clan",
            Self::Members => "members",
        }
    }
}

/// The ownership fields of a clan map's list row and header. Empty on every
/// other map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClanOwnership {
    /// `clan` or `members` on a clan's map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<MapOwnership>,
    /// On a Member-owned map: whether the caller is one of its active
    /// recorded owners.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owned_by_me: bool,
    /// On a Member-owned map whose last recorded owner's account is gone:
    /// nobody can change its access.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frozen: bool,
}

impl ClanOwnership {
    /// Whether this is a Member-owned map.
    #[must_use]
    pub fn member_owned(&self) -> bool {
        self.ownership == Some(MapOwnership::Members)
    }
}

/// One recorded owner of a Member-owned map (`GET /areas/{a}/owners`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapOwner {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    /// False for an owner who is not an active member now: their ownership
    /// is dormant until they rejoin.
    pub active: bool,
    pub added_at: DateTime<Utc>,
}

/// One recipient of an ownership offer, and whether they have accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferRecipient {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub accepted: bool,
}

/// An ownership offer on a clan's map. An offer to several members is joint:
/// each accepts on their own, and the last acceptance applies it. An offer
/// to the clan (making a Member-owned map Clan-owned) names nobody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaOwnershipOffer {
    pub id: Uuid,
    pub area_id: AreaId,
    #[serde(default)]
    pub area_name: String,
    pub clan_id: Uuid,
    #[serde(default)]
    pub recipients: Vec<OfferRecipient>,
    /// What the map becomes on acceptance.
    pub ownership: MapOwnership,
    #[serde(default)]
    pub replace: bool,
    pub initiator_id: Uuid,
    #[serde(default)]
    pub initiator_nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl AreaOwnershipOffer {
    /// Whether the offer goes to the clan rather than to members.
    #[must_use]
    pub fn to_clan(&self) -> bool {
        self.ownership == MapOwnership::Clan && self.recipients.is_empty()
    }

    /// Whether `user` is one of its recipients and has accepted.
    #[must_use]
    pub fn accepted_by(&self, user: Uuid) -> bool {
        self.recipients
            .iter()
            .any(|recipient| recipient.user_id == user && recipient.accepted)
    }
}

/// The map as it stands after an accepted ownership offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipAccepted {
    pub area_id: AreaId,
    pub ownership: MapOwnership,
    #[serde(default)]
    pub atlas_id: Option<AtlasId>,
}

/// One copy `POST /clans/{c}/copies` made in the caller's own maps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClanMapCopy {
    pub area_id: AreaId,
    pub name: String,
    pub copied_from: AreaId,
}

/// An outside share of a Clan-owned map (`GET /areas/{a}/shares` on a
/// clan's map): view only, to a friend outside the clan, with who made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideShare {
    pub id: Uuid,
    pub grantor_id: Uuid,
    #[serde(default)]
    pub grantor_nickname: Option<String>,
    pub grantee_id: Uuid,
    #[serde(default)]
    pub grantee_nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct Copies {
    copies: Vec<ClanMapCopy>,
}

impl CloudApiClient {
    /// `POST /areas` with `clan_id` and `ownership`: creates a map in one of
    /// the clan's folders. Clan-owned needs `area.create` there;
    /// Member-owned needs `area.create_member_owned` and makes the caller its
    /// first recorded owner.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`](crate::CloudError::NotFoundOrNoAccess)
    /// without the action or for a folder that is not the clan's; other
    /// failures via [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn create_clan_area(
        &self,
        clan_id: Uuid,
        atlas_id: AtlasId,
        name: &str,
        ownership: MapOwnership,
    ) -> CloudResult<Area> {
        let body = json!({
            "name": name,
            "atlas_id": atlas_id,
            "clan_id": clan_id,
            "ownership": ownership.wire(),
        });
        self.post("/areas", Some(&body), Auth::Required).await
    }

    /// `GET /areas/{a}/owners`: a Member-owned map's recorded owners, oldest
    /// first. Any reader of the map; a Clan-owned map lists none.
    ///
    /// # Errors
    /// The uniform 404 for a map the caller does not read; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn area_owners(&self, area_id: AreaId) -> CloudResult<Vec<MapOwner>> {
        self.get(&format!("/areas/{area_id}/owners")).await
    }

    /// `DELETE /areas/{a}/owners/{u}`: removes a recorded owner, or with the
    /// caller's own ID gives up ownership. An active owner. The grants to
    /// that user stay as they are.
    ///
    /// # Errors
    /// [`CloudError::LastOwner`](crate::CloudError::LastOwner) for the last
    /// recorded owner; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn remove_area_owner(&self, area_id: AreaId, user_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}/owners/{user_id}"))
            .await
    }

    /// `POST /areas/{a}/ownership-offers`: offers the map's ownership to
    /// members (`user_ids`, one to 16, with `ownership` the map's current
    /// one for members to members, or `members` from the clan), or offers a
    /// Member-owned map to the clan (`user_ids` empty, `ownership` `clan`).
    /// `replace` makes the recipients its only owners.
    ///
    /// # Errors
    /// The uniform 404 when the caller may not make the offer or a recipient
    /// is not eligible; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn offer_area_ownership(
        &self,
        area_id: AreaId,
        user_ids: &[Uuid],
        ownership: MapOwnership,
        replace: bool,
    ) -> CloudResult<AreaOwnershipOffer> {
        let body = if user_ids.is_empty() {
            json!({ "user_ids": [], "ownership": ownership.wire() })
        } else {
            json!({ "user_ids": user_ids, "ownership": ownership.wire(), "replace": replace })
        };
        self.post(
            &format!("/areas/{area_id}/ownership-offers"),
            Some(&body),
            Auth::Required,
        )
        .await
    }

    /// `GET /areas/{a}/ownership-offers`: pending offers on the map, oldest
    /// first: all of them to those with ownership authority, else those
    /// naming the caller.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn area_ownership_offers(
        &self,
        area_id: AreaId,
    ) -> CloudResult<Vec<AreaOwnershipOffer>> {
        self.get(&format!("/areas/{area_id}/ownership-offers"))
            .await
    }

    /// `GET /me/area-offers`: the offers naming the caller, across their
    /// clans, oldest first.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn my_area_offers(&self) -> CloudResult<Vec<AreaOwnershipOffer>> {
        self.get("/me/area-offers").await
    }

    /// `GET /clans/{c}/area-offers`: offers to make a Member-owned map
    /// Clan-owned, oldest first. Clan owners, and holders of
    /// `atlas.accept_transfer` on the clan or any folder.
    ///
    /// # Errors
    /// The uniform 404 for anyone else; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn clan_area_offers(&self, clan_id: Uuid) -> CloudResult<Vec<AreaOwnershipOffer>> {
        self.get(&format!("/clans/{clan_id}/area-offers")).await
    }

    /// `POST /areas/{a}/ownership-offers/{o}/accept`. An offer to the clan
    /// needs `atlas_id`, the folder it is filed in.
    ///
    /// # Errors
    /// The uniform 404 when the offer is void or not the caller's to accept;
    /// other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn accept_area_offer(
        &self,
        area_id: AreaId,
        offer_id: Uuid,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<OwnershipAccepted> {
        let body = match atlas_id {
            Some(atlas_id) => json!({ "atlas_id": atlas_id }),
            None => json!({}),
        };
        self.post(
            &format!("/areas/{area_id}/ownership-offers/{offer_id}/accept"),
            Some(&body),
            Auth::Required,
        )
        .await
    }

    /// `POST /areas/{a}/ownership-offers/{o}/decline`: ends the offer for
    /// every recipient.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn decline_area_offer(&self, area_id: AreaId, offer_id: Uuid) -> CloudResult<()> {
        self.post_unit(
            &format!("/areas/{area_id}/ownership-offers/{offer_id}/decline"),
            None,
            Auth::Required,
        )
        .await
    }

    /// `DELETE /areas/{a}/ownership-offers/{o}`: withdraws an offer. Those
    /// with ownership authority over the map.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn withdraw_area_offer(&self, area_id: AreaId, offer_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}/ownership-offers/{offer_id}"))
            .await
    }

    /// `GET /areas/{a}/shares` on a Clan-owned map: its outside shares,
    /// oldest first. Clan owners and holders of `area.share_external` on
    /// it.
    ///
    /// # Errors
    /// The uniform 404 for anyone else; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn outside_shares(&self, area_id: AreaId) -> CloudResult<Vec<OutsideShare>> {
        self.get(&format!("/areas/{area_id}/shares")).await
    }

    /// `POST /shares` on a Clan-owned map: shares it with a friend outside
    /// the clan, view only. Needs `area.share_external`. Revoked through
    /// [`CloudApiClient::revoke_share`] by its sharer or a clan owner.
    ///
    /// # Errors
    /// The uniform 404 without the action, or for someone who is not a
    /// friend outside the clan; other failures via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn share_outside(&self, area_id: AreaId, friend: Uuid) -> CloudResult<ShareGrant> {
        self.create_share(CreateShareRequest {
            grantee_id: friend,
            scope: ShareScope::Area { area_id },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            can_admin: false,
            host_hints: None,
        })
        .await
    }

    /// `POST /clans/{c}/copies`: copies each Member-owned map the caller is
    /// an active recorded owner of into their own maps, before they leave.
    /// The list is empty when they own none.
    ///
    /// # Errors
    /// Non-2xx statuses via
    /// [`CloudError::from_status`](crate::CloudError::from_status).
    pub async fn copy_my_clan_maps(&self, clan_id: Uuid) -> CloudResult<Vec<ClanMapCopy>> {
        let copies: Copies = self
            .post(&format!("/clans/{clan_id}/copies"), None, Auth::Required)
            .await?;
        Ok(copies.copies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_parse_joint_and_clan_shapes() {
        let joint: AreaOwnershipOffer = serde_json::from_value(json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "area_id": "22222222-2222-4222-8222-222222222222",
            "area_name": "Roads",
            "clan_id": "33333333-3333-4333-8333-333333333333",
            "recipients": [
                { "user_id": "44444444-4444-4444-8444-444444444444", "accepted": true, "nickname": "Nessa" },
                { "user_id": "55555555-5555-4555-8555-555555555555", "accepted": false }
            ],
            "ownership": "members",
            "replace": false,
            "initiator_id": "66666666-6666-4666-8666-666666666666",
            "created_at": "2026-10-07T01:00:00Z"
        }))
        .unwrap();
        assert!(!joint.to_clan());
        assert!(joint.accepted_by(Uuid::from_u128(0x4444_4444_4444_4444_8444_4444_4444_4444)));
        let to_clan: AreaOwnershipOffer = serde_json::from_value(json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "area_id": "22222222-2222-4222-8222-222222222222",
            "area_name": "Roads",
            "clan_id": "33333333-3333-4333-8333-333333333333",
            "recipients": [],
            "ownership": "clan",
            "replace": false,
            "initiator_id": "66666666-6666-4666-8666-666666666666",
            "created_at": "2026-10-07T01:00:00Z"
        }))
        .unwrap();
        assert!(to_clan.to_clan());
    }

    #[test]
    fn a_clan_maps_row_carries_its_ownership() {
        let row: Area = serde_json::from_value(json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "user_id": null,
            "clan_id": "22222222-2222-4222-8222-222222222222",
            "clan_name": "Lantern Company",
            "atlas_id": null,
            "name": "Solace",
            "created_at": "2026-10-01T12:00:00Z",
            "ownership": "members",
            "owned_by_me": true,
            "actions": ["area.read", "area.edit"]
        }))
        .unwrap();
        assert!(row.clan_ownership.member_owned());
        assert!(row.clan_ownership.owned_by_me);
        assert!(!row.clan_ownership.frozen);
        let back = serde_json::to_value(&row).unwrap();
        assert_eq!(back["ownership"], "members");
        assert_eq!(back["owned_by_me"], true);
        assert!(back.get("frozen").is_none());
    }
}
