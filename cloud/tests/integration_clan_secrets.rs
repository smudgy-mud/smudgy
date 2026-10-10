//! Clan Secrets end to end over real HTTP against the contract-shaped mock
//! in `tests/support/clan_secrets.rs`: creating them under the clan's
//! creation actions (a Clan-owned Secret's creator starts as a Contributor),
//! who has access and why, ownership offers (joint ones included), departure,
//! and frozen Secrets.
#![allow(clippy::too_many_lines)]

mod support;

use smudgy_cloud::clan_secrets::{
    AccessReason, NewSecret, NewSecretOwner, OfferRequest, SecretRecipient, ownership,
};
use smudgy_cloud::{
    AreaId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource, MapperBackend,
    SourceBundle, SourceId,
};
use support::clans::{ClanGrantScope, ClanRecipient};
use support::{MockHandle, MockServer, TestUser};
use uuid::Uuid;

struct Member {
    user: TestUser,
    api: CloudApiClient,
    maps: CloudMapper,
}

fn member(server: &MockHandle, nickname: &str) -> Member {
    let user = server.create_user(&format!("{nickname}@example.com"), nickname, true);
    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    );
    let maps = CloudMapper::new(server.base_url.clone(), user.api_key.clone());
    Member { user, api, maps }
}

fn is_not_found<T: std::fmt::Debug>(result: &Result<T, CloudError>) -> bool {
    matches!(result, Err(CloudError::NotFoundOrNoAccess))
}

fn names(actions: &[String]) -> Vec<&str> {
    actions.iter().map(String::as_str).collect()
}

async fn create(
    member: &Member,
    area: AreaId,
    name: &str,
    owner: NewSecretOwner,
) -> Result<SourceId, CloudError> {
    let secret = NewSecret {
        name: name.to_string(),
        color: None,
        owner,
    };
    member
        .maps
        .create_secret_as(&area, &secret, member.maps.auth_generation())
        .await
        .map(|summary| summary.source)
}

/// The Secret's bundle in the member's projection of the map, if they read
/// it.
async fn bundle(member: &Member, area: AreaId, secret: SourceId) -> Option<SourceBundle> {
    member
        .maps
        .get_area(&area)
        .await
        .ok()?
        .sources
        .into_iter()
        .find(|bundle| bundle.source == secret)
}

/// Shares a Clan Secret with All clan members, as the clan's owner.
async fn share_with_everyone(c: &Fixture, secret: &SourceId, actions: &[&str]) {
    c.owner
        .api
        .grant_clan_secret(
            secret,
            SecretRecipient::Group {
                group_id: c.everyone,
            },
            actions,
        )
        .await
        .expect("the owner shares it");
}

async fn held(member: &Member, area: AreaId, secret: SourceId) -> Vec<String> {
    bundle(member, area, secret)
        .await
        .map(|bundle| bundle.actions.into_iter().collect())
        .unwrap_or_default()
}

/// A clan with its owner, two more members and one map in one folder, which
/// every member reads.
struct Fixture {
    server: MockHandle,
    owner: Member,
    ann: Member,
    bo: Member,
    clan: Uuid,
    atlas: Uuid,
    area: AreaId,
    everyone: Uuid,
}

async fn fixture() -> Fixture {
    let server = MockServer::spawn().await;
    let owner = member(&server, "mira");
    let ann = member(&server, "ann");
    let bo = member(&server, "bo");
    let clan = owner.api.create_clan("Lantern Company").await.unwrap().id;
    server.join_clan(clan, &ann.user);
    server.join_clan(clan, &bo.user);
    let atlas = server.create_clan_atlas(clan, "Roads");
    let area = server.create_clan_area(clan, atlas, "Midgaard");
    let everyone = server.clan_builtin_group(clan, "members");
    server.clan_grant(
        clan,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Atlases([atlas].into()),
        &["area.read"],
    );
    Fixture {
        server,
        owner,
        ann,
        bo,
        clan,
        atlas,
        area,
        everyone,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn member_owned_secrets_need_the_creation_action_and_stay_unseen() {
    let c = fixture().await;
    let members = NewSecretOwner::Members { clan_id: c.clan };

    // Reading the map gives no creation action; the refusal is the 404.
    let refused = create(&c.ann, c.area, "Quest", members).await;
    assert!(is_not_found(&refused), "{refused:?}");

    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_member_owned"],
    );
    let secret = create(&c.ann, c.area, "Quest", members)
        .await
        .expect("the creation action creates");

    // Its creator owns it and holds every action; the badge says so.
    let mine = bundle(&c.ann, c.area, secret)
        .await
        .expect("the creator reads it");
    assert_eq!(mine.ownership.as_deref(), Some(ownership::MEMBERS));
    assert_eq!(mine.clan_id, Some(c.clan));
    assert!(mine.actions.contains("manage_ownership"));

    // Nobody else learns of it, the clan's owner included.
    assert!(bundle(&c.bo, c.area, secret).await.is_none());
    assert!(bundle(&c.owner, c.area, secret).await.is_none());
    let listed = c.owner.api.area_secrets(c.area).await.expect("lists");
    assert!(listed.is_empty(), "no names, colors or counts: {listed:?}");
    assert!(is_not_found(&c.owner.api.secret_access(&secret).await));

    // A clan map takes no owner Secret, and naming another clan is the 404.
    let owner_secret = create(&c.owner, c.area, "Mine", NewSecretOwner::Me).await;
    assert!(is_not_found(&owner_secret), "{owner_secret:?}");
    let elsewhere = create(
        &c.ann,
        c.area,
        "Quest",
        NewSecretOwner::Members {
            clan_id: Uuid::new_v4(),
        },
    )
    .await;
    assert!(is_not_found(&elsewhere));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clan_owned_secrets_creator_starts_as_a_contributor() {
    let c = fixture().await;
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_clan_owned"],
    );
    let secret = create(
        &c.ann,
        c.area,
        "Survey",
        NewSecretOwner::Clan { clan_id: c.clan },
    )
    .await
    .expect("the creation action creates");

    // The creator reads, adds and edits; nobody else reads it until the
    // clan's grants or its own reach them; the clan's owner holds authority.
    assert_eq!(
        names(&held(&c.ann, c.area, secret).await),
        ["add", "edit", "read"]
    );
    assert!(held(&c.bo, c.area, secret).await.is_empty());
    assert!(bundle(&c.bo, c.area, secret).await.is_none());
    let owner = held(&c.owner, c.area, secret).await;
    assert!(owner.contains(&"manage_ownership".to_string()), "{owner:?}");

    // A clan grant of `secret.read` on the folder reaches it.
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.read"],
    );
    assert_eq!(names(&held(&c.bo, c.area, secret).await), ["read"]);
    let badge = bundle(&c.bo, c.area, secret).await.unwrap();
    assert_eq!(badge.ownership.as_deref(), Some(ownership::CLAN));

    // A Contributor shares nothing: granting takes manage_access.
    let creator = c
        .ann
        .api
        .grant_clan_secret(
            &secret,
            SecretRecipient::User {
                user_id: c.ann.user.id,
            },
            &["read"],
        )
        .await;
    assert!(
        is_not_found(&creator),
        "a Contributor grants nothing: {creator:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn access_lists_who_reads_and_why_within_what_the_caller_may_inspect() {
    let c = fixture().await;
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_clan_owned"],
    );
    let secret = create(
        &c.ann,
        c.area,
        "Survey",
        NewSecretOwner::Clan { clan_id: c.clan },
    )
    .await
    .unwrap();
    share_with_everyone(&c, &secret, &["read"]).await;

    // The clan's owner manages access: every reader appears, owners first.
    let access = c.owner.api.secret_access(&secret).await.expect("access");
    assert_eq!(access.ownership, ownership::CLAN);
    assert_eq!(access.clan_id, c.clan);
    let order: Vec<Uuid> = access.members.iter().map(|m| m.user_id).collect();
    assert_eq!(order, [c.owner.user.id, c.ann.user.id, c.bo.user.id]);
    assert_eq!(access.members[0].reasons[0], AccessReason::ClanOwner);
    assert_eq!(access.members[1].nickname.as_deref(), Some("ann"));
    assert!(
        access.members[1]
            .reasons
            .iter()
            .any(|reason| matches!(reason, AccessReason::Grant { .. })),
        "the creator's own grant: {:?}",
        access.members[1].reasons
    );
    assert!(matches!(
        access.members[2].reasons.as_slice(),
        [AccessReason::Group { group_id, .. }] if *group_id == c.everyone
    ));

    // A reader without manage_access sees only themselves.
    let own = c.bo.api.secret_access(&secret).await.expect("own entry");
    assert_eq!(own.members.len(), 1);
    assert_eq!(own.members[0].user_id, c.bo.user.id);
    assert_eq!(names(&own.members[0].actions), ["read"]);

    // An owner Secret has no access list: the uniform 404.
    let outsider_map = c.server.create_area(&c.bo.user, "Own");
    let owner_secret = create(&c.bo, outsider_map, "Mine", NewSecretOwner::Me)
        .await
        .unwrap();
    assert!(is_not_found(&c.bo.api.secret_access(&owner_secret).await));
    assert!(matches!(
        c.bo.api.secret_access(&SourceId::Map).await,
        Err(CloudError::InvalidInput(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_joint_offer_applies_when_the_last_recipient_accepts() {
    let c = fixture().await;
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_member_owned", "secret.create_clan_owned"],
    );
    // A Clan-owned Secret every member reads.
    let secret = create(
        &c.owner,
        c.area,
        "Quest",
        NewSecretOwner::Clan { clan_id: c.clan },
    )
    .await
    .unwrap();
    share_with_everyone(&c, &secret, &["read"]).await;

    // Only manage_ownership offers.
    let refused =
        c.bo.api
            .offer_secret_ownership(
                &secret,
                &OfferRequest::to_members(vec![c.ann.user.id], false),
            )
            .await;
    assert!(is_not_found(&refused));
    // Malformed offers are 400s.
    let empty = c
        .owner
        .api
        .offer_secret_ownership(&secret, &OfferRequest::to_members(Vec::new(), false))
        .await;
    assert!(
        matches!(empty, Err(CloudError::InvalidInput(_))),
        "{empty:?}"
    );

    let offer = c
        .owner
        .api
        .offer_secret_ownership(
            &secret,
            &OfferRequest::to_members(vec![c.ann.user.id, c.bo.user.id], false),
        )
        .await
        .expect("a joint offer");
    assert!(offer.is_joint());
    assert_eq!(offer.secret_name, "Quest");
    assert_eq!(offer.initiator_nickname.as_deref(), Some("mira"));

    // Each recipient sees it, with the other recipient named.
    let ann_offers = c.ann.api.my_secret_offers().await.expect("Ann's offers");
    assert_eq!(ann_offers.len(), 1);
    assert_eq!(ann_offers[0].recipients.len(), 2);
    assert!(c.owner.api.my_secret_offers().await.unwrap().is_empty());
    assert_eq!(c.owner.api.secret_offers(&secret).await.unwrap().len(), 1);

    // The first acceptance changes nothing yet.
    let summary = c
        .ann
        .api
        .accept_secret_offer(&secret, offer.id)
        .await
        .expect("Ann accepts");
    assert_eq!(summary.ownership, ownership::CLAN);
    let pending = c.owner.api.secret_offers(&secret).await.unwrap();
    assert!(pending[0].accepted_by(c.ann.user.id));
    assert!(!pending[0].accepted_by(c.bo.user.id));

    // The last one applies it for both: Member-owned, both its owners.
    let summary =
        c.bo.api
            .accept_secret_offer(&secret, offer.id)
            .await
            .expect("Bo completes it");
    assert_eq!(summary.ownership, ownership::MEMBERS);
    assert!(summary.actions.contains("manage_ownership"));
    let owners = c.ann.api.secret_owners(&secret).await.expect("owners");
    // Both became owners at once: ties list by user ID.
    let owner_ids: Vec<Uuid> = owners.iter().map(|owner| owner.user_id).collect();
    let mut expected = vec![c.ann.user.id, c.bo.user.id];
    expected.sort();
    assert_eq!(owner_ids, expected);
    assert!(c.ann.api.secret_offers(&secret).await.unwrap().is_empty());

    // The clan's owner keeps only what the Secret's grants give, here the
    // Contributor grant they started with as its creator: authority went
    // with the clan's ownership.
    assert_eq!(
        names(&held(&c.owner, c.area, secret).await),
        ["add", "edit", "read"]
    );
    assert!(c.owner.api.secret_offers(&secret).await.unwrap().is_empty());

    // Removing the last owner is refused; another one is fine.
    c.ann
        .api
        .remove_secret_owner(&secret, c.bo.user.id)
        .await
        .expect("Ann removes Bo");
    assert!(matches!(
        c.ann.api.remove_secret_owner(&secret, c.ann.user.id).await,
        Err(CloudError::LastOwner)
    ));
}

/// The Ownership section's three changes on a Member-owned Secret: removing
/// another owner, giving up one's own ownership while another owner stays,
/// and making it Clan-owned through a clan owner who reads it, who may be
/// the owner themselves and accept at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owners_go_and_a_member_owned_secret_becomes_clan_owned() {
    let c = fixture().await;
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_member_owned"],
    );
    let members = NewSecretOwner::Members { clan_id: c.clan };
    let secret = create(&c.ann, c.area, "Cache", members).await.unwrap();
    let reader = |member: &Member| SecretRecipient::User {
        user_id: member.user.id,
    };

    // Bo reads it, then becomes a second owner.
    c.ann
        .api
        .grant_clan_secret(&secret, reader(&c.bo), &[])
        .await
        .expect("Bo reads it");
    let offer = c
        .ann
        .api
        .offer_secret_ownership(
            &secret,
            &OfferRequest::to_members(vec![c.bo.user.id], false),
        )
        .await
        .unwrap();
    c.bo.api
        .accept_secret_offer(&secret, offer.id)
        .await
        .expect("Bo accepts");
    assert_eq!(c.ann.api.secret_owners(&secret).await.unwrap().len(), 2);

    // Ann gives up her ownership; her own grants are all she keeps, so she
    // no longer reads it.
    c.ann
        .api
        .remove_secret_owner(&secret, c.ann.user.id)
        .await
        .expect("Ann gives up ownership");
    assert!(bundle(&c.ann, c.area, secret).await.is_none());
    let owners = c.bo.api.secret_owners(&secret).await.unwrap();
    assert_eq!(
        owners.iter().map(|owner| owner.user_id).collect::<Vec<_>>(),
        [c.bo.user.id]
    );
    // Bo is the last owner now: neither giving up nor a removal leaves it
    // ownerless.
    assert!(matches!(
        c.bo.api.remove_secret_owner(&secret, c.bo.user.id).await,
        Err(CloudError::LastOwner)
    ));
    // Someone who isn't an owner is nobody to remove.
    assert!(is_not_found(
        &c.bo.api.remove_secret_owner(&secret, c.ann.user.id).await
    ));

    // Only a clan owner who reads it may accept it for the clan.
    assert!(is_not_found(
        &c.bo
            .api
            .offer_secret_ownership(&secret, &OfferRequest::to_clan(c.bo.user.id))
            .await
    ));
    assert!(is_not_found(
        &c.bo
            .api
            .offer_secret_ownership(&secret, &OfferRequest::to_clan(c.owner.user.id))
            .await
    ));
    c.bo.api
        .grant_clan_secret(&secret, reader(&c.owner), &[])
        .await
        .expect("the clan's owner reads it");
    let offer =
        c.bo.api
            .offer_secret_ownership(&secret, &OfferRequest::to_clan(c.owner.user.id))
            .await
            .expect("an offer to the clan");
    let summary = c
        .owner
        .api
        .accept_secret_offer(&secret, offer.id)
        .await
        .expect("the clan's owner accepts it for the clan");
    assert_eq!(summary.ownership, ownership::CLAN);
    // Its recorded owners are cleared: Bo keeps what is shared with him.
    assert!(c.owner.api.secret_owners(&secret).await.unwrap().is_empty());
    assert!(
        !held(&c.bo, c.area, secret)
            .await
            .contains(&"manage_ownership".to_string())
    );
    assert!(
        held(&c.owner, c.area, secret)
            .await
            .contains(&"manage_ownership".to_string())
    );
    // An owner list has nobody to remove on a Clan-owned Secret.
    assert!(is_not_found(
        &c.owner.api.remove_secret_owner(&secret, c.bo.user.id).await
    ));

    // A clan owner who owns a Member-owned Secret makes it Clan-owned in
    // one step: an offer to themselves, accepted at once.
    let own = create(&c.owner, c.area, "Vault", members).await.unwrap();
    let offer = c
        .owner
        .api
        .offer_secret_ownership(&own, &OfferRequest::to_clan(c.owner.user.id))
        .await
        .expect("an offer to themselves");
    let summary = c
        .owner
        .api
        .accept_secret_offer(&own, offer.id)
        .await
        .expect("accepted at once");
    assert_eq!(summary.ownership, ownership::CLAN);
    assert!(summary.actions.contains("manage_ownership"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declining_an_offer_needs_the_read_its_listing_needs() {
    let c = fixture().await;
    let reading = c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_clan_owned", "secret.read"],
    );
    let secret = create(
        &c.owner,
        c.area,
        "Survey",
        NewSecretOwner::Clan { clan_id: c.clan },
    )
    .await
    .unwrap();
    let offer = c
        .owner
        .api
        .offer_secret_ownership(
            &secret,
            &OfferRequest::to_members(vec![c.ann.user.id], false),
        )
        .await
        .unwrap();
    assert_eq!(c.ann.api.my_secret_offers().await.unwrap().len(), 1);

    // Ann stops reading the Secret: its offer is not listed to her, and
    // declining it is the uniform 404, as for an offer that never was.
    c.owner
        .api
        .delete_clan_grant(c.clan, reading)
        .await
        .unwrap();
    assert!(c.ann.api.my_secret_offers().await.unwrap().is_empty());
    assert!(is_not_found(
        &c.ann.api.decline_secret_offer(&secret, offer.id).await
    ));
    assert!(is_not_found(
        &c.ann
            .api
            .decline_secret_offer(&secret, Uuid::new_v4())
            .await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_on_a_member_owned_map_is_never_made_clan_owned() {
    let c = fixture().await;
    let grove = c
        .server
        .create_member_owned_area(c.clan, c.atlas, "Grove", &[c.owner.user.id]);
    let members = NewSecretOwner::Members { clan_id: c.clan };
    let secret = create(&c.owner, grove, "Cache", members)
        .await
        .expect("the map's owner creates a Member-owned Secret");
    assert!(
        held(&c.owner, grove, secret)
            .await
            .contains(&"manage_ownership".to_string())
    );

    // A clan owner who owns it still cannot offer it to the clan.
    assert!(is_not_found(
        &c.owner
            .api
            .offer_secret_ownership(&secret, &OfferRequest::to_clan(c.owner.user.id))
            .await
    ));
    assert!(c.owner.api.secret_offers(&secret).await.unwrap().is_empty());
    let summary = c
        .owner
        .api
        .clan_resources(c.clan, smudgy_cloud::clan_access::ResourceKind::Secrets)
        .await
        .unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].ownership.as_deref(), Some("members"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declined_withdrawn_and_void_offers_end() {
    let c = fixture().await;
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_clan_owned", "secret.read"],
    );
    let secret = create(
        &c.owner,
        c.area,
        "Survey",
        NewSecretOwner::Clan { clan_id: c.clan },
    )
    .await
    .unwrap();
    let to = |users: Vec<Uuid>| OfferRequest::to_members(users, false);

    // Declining ends it for every recipient.
    let offer = c
        .owner
        .api
        .offer_secret_ownership(&secret, &to(vec![c.ann.user.id, c.bo.user.id]))
        .await
        .unwrap();
    c.bo.api
        .decline_secret_offer(&secret, offer.id)
        .await
        .expect("Bo declines");
    assert!(c.ann.api.my_secret_offers().await.unwrap().is_empty());
    assert!(is_not_found(
        &c.ann.api.accept_secret_offer(&secret, offer.id).await
    ));

    // Withdrawing needs manage_ownership.
    let offer = c
        .owner
        .api
        .offer_secret_ownership(&secret, &to(vec![c.ann.user.id]))
        .await
        .unwrap();
    assert!(is_not_found(
        &c.bo.api.withdraw_secret_offer(&secret, offer.id).await
    ));
    c.owner
        .api
        .withdraw_secret_offer(&secret, offer.id)
        .await
        .expect("the owner withdraws");
    assert!(c.ann.api.my_secret_offers().await.unwrap().is_empty());

    // A recipient's departure voids the offers naming them.
    let offer = c
        .owner
        .api
        .offer_secret_ownership(&secret, &to(vec![c.bo.user.id]))
        .await
        .unwrap();
    c.bo.api
        .remove_clan_member(c.clan, c.bo.user.id)
        .await
        .expect("Bo leaves");
    assert!(c.owner.api.secret_offers(&secret).await.unwrap().is_empty());
    assert!(is_not_found(
        &c.bo.api.accept_secret_offer(&secret, offer.id).await
    ));

    // An offer to someone who does not read the Secret is the 404, whoever
    // they are.
    let stranger = member(&c.server, "stranger");
    let refused = c
        .owner
        .api
        .offer_secret_ownership(&secret, &to(vec![stranger.user.id]))
        .await;
    assert!(is_not_found(&refused));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frozen_secret_is_read_only_and_goes_when_no_grant_reaches_it() {
    let c = fixture().await;
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(c.everyone),
        ClanGrantScope::Atlases([c.atlas].into()),
        &["secret.create_member_owned", "secret.create_clan_owned"],
    );
    // A Clan-owned Secret every member edits becomes Ann's alone.
    let secret = create(
        &c.owner,
        c.area,
        "Quest",
        NewSecretOwner::Clan { clan_id: c.clan },
    )
    .await
    .unwrap();
    share_with_everyone(&c, &secret, &["read", "add", "edit"]).await;
    let offer = c
        .owner
        .api
        .offer_secret_ownership(
            &secret,
            &OfferRequest::to_members(vec![c.ann.user.id], true),
        )
        .await;
    assert!(
        matches!(offer, Err(CloudError::InvalidInput(_))),
        "replace applies only between members: {offer:?}"
    );
    let offer = c
        .owner
        .api
        .offer_secret_ownership(
            &secret,
            &OfferRequest::to_members(vec![c.ann.user.id], false),
        )
        .await
        .unwrap();
    c.ann
        .api
        .accept_secret_offer(&secret, offer.id)
        .await
        .unwrap();
    assert_eq!(
        names(&held(&c.bo, c.area, secret).await),
        ["add", "edit", "read"]
    );
    // A Secret only Ann reads, with no grant at all.
    let diary = create(
        &c.ann,
        c.area,
        "Diary",
        NewSecretOwner::Members { clan_id: c.clan },
    )
    .await
    .unwrap();

    // Ann's account goes: the Secret freezes. Its readers keep reading it,
    // with the same badge, and nothing else.
    c.server.forget_account_in_clan_secrets(c.ann.user.id);
    assert_eq!(names(&held(&c.bo, c.area, secret).await), ["read"]);
    let frozen = bundle(&c.bo, c.area, secret).await.unwrap();
    assert_eq!(frozen.ownership.as_deref(), Some(ownership::MEMBERS));
    let own = c.bo.api.secret_access(&secret).await.unwrap();
    assert!(matches!(
        own.members[0].reasons.as_slice(),
        [AccessReason::Group { actions, .. }] if actions == &["read".to_string()]
    ));
    assert_eq!(
        c.server.clan_secret_state(match secret {
            SourceId::Secret(id) => id,
            _ => unreachable!(),
        }),
        Some(("members".to_string(), true, 0))
    );
    // A frozen Secret no grant reaches goes.
    let SourceId::Secret(diary) = diary else {
        unreachable!("a Secret")
    };
    assert_eq!(c.server.clan_secret_state(diary), None);
}
