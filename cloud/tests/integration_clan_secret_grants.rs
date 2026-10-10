//! Sharing a Clan Secret within its clan over real HTTP against the
//! contract-shaped mock (`tests/support/clan_secret_grants.rs`): grants to
//! members and groups, the sharer's limits, who may change or revoke a
//! grant, the uniform 404s, and the `copy` action that takes a Secret along
//! in a copy of its map.
#![allow(clippy::too_many_lines)]

mod support;

use smudgy_cloud::clan_secrets::{
    NewSecret, NewSecretOwner, OfferRequest, SecretRecipient, secret_preset,
};
use smudgy_cloud::clans::action;
use smudgy_cloud::cloud_api::{CopyAreaRequest, secret_action};
use smudgy_cloud::{
    AreaId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource, MapperBackend,
    SourceId,
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

fn to(user: &Member) -> SecretRecipient {
    SecretRecipient::User {
        user_id: user.user.id,
    }
}

/// The member's actions on the Secret, as their projection of the map
/// shows them (sorted); empty when they do not read it.
async fn held(member: &Member, area: AreaId, secret: SourceId) -> Vec<String> {
    member
        .maps
        .get_area(&area)
        .await
        .ok()
        .and_then(|area| {
            area.sources
                .into_iter()
                .find(|bundle| bundle.source == secret)
        })
        .map(|bundle| bundle.actions.into_iter().collect())
        .unwrap_or_default()
}

/// A clan of Mira (its owner), Ann, Bo and Cy, one map every member reads,
/// and a Member-owned Secret on it that Ann created and owns. Zed is no
/// member.
struct Fixture {
    server: MockHandle,
    mira: Member,
    ann: Member,
    bo: Member,
    cy: Member,
    zed: Member,
    clan: Uuid,
    area: AreaId,
    secret: SourceId,
}

async fn fixture() -> Fixture {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let ann = member(&server, "ann");
    let bo = member(&server, "bo");
    let cy = member(&server, "cy");
    let zed = member(&server, "zed");
    let clan = mira.api.create_clan("Lantern Company").await.unwrap().id;
    for joining in [&ann, &bo, &cy] {
        server.join_clan(clan, &joining.user);
    }
    let atlas = server.create_clan_atlas(clan, "Roads");
    let area = server.create_clan_area(clan, atlas, "Midgaard");
    let everyone = server.clan_builtin_group(clan, "members");
    server.clan_grant(
        clan,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Atlases([atlas].into()),
        &["area.read", "secret.create_member_owned"],
    );
    let secret = ann
        .maps
        .create_secret_as(
            &area,
            &NewSecret {
                name: "Quest".to_string(),
                color: None,
                owner: NewSecretOwner::Members { clan_id: clan },
            },
            ann.maps.auth_generation(),
        )
        .await
        .unwrap()
        .source;
    Fixture {
        server,
        mira,
        ann,
        bo,
        cy,
        zed,
        clan,
        area,
        secret,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owners_share_with_members_and_groups() {
    let c = fixture().await;

    // A member gets the preset's actions; read is implied.
    let grant = c
        .ann
        .api
        .grant_clan_secret(&c.secret, to(&c.bo), &secret_preset::CONTRIBUTOR[1..])
        .await
        .expect("an owner shares with a member");
    assert_eq!(grant.recipient.user(), Some(c.bo.user.id));
    assert_eq!(grant.nickname.as_deref(), Some("bo"));
    assert_eq!(grant.grantor_nickname.as_deref(), Some("ann"));
    assert_eq!(grant.clan_id, c.clan);
    assert_eq!(held(&c.bo, c.area, c.secret).await, ["add", "edit", "read"]);

    // A group grant reaches whoever is in it, the clan's owner included,
    // who holds nothing on a Member-owned Secret otherwise.
    let group = c
        .mira
        .api
        .create_clan_group(c.clan, "City mappers", None)
        .await
        .unwrap()
        .id;
    c.mira
        .api
        .add_clan_group_member(c.clan, group, c.mira.user.id)
        .await
        .unwrap();
    assert!(held(&c.mira, c.area, c.secret).await.is_empty());
    let to_group = c
        .ann
        .api
        .grant_clan_secret(&c.secret, SecretRecipient::Group { group_id: group }, &[])
        .await
        .expect("an owner shares with a group");
    assert_eq!(held(&c.mira, c.area, c.secret).await, ["read"]);

    // One grant per recipient: a second one replaces its actions and keeps
    // its ID.
    let again = c
        .ann
        .api
        .grant_clan_secret(&c.secret, to(&c.bo), &secret_preset::EDITOR[1..])
        .await
        .unwrap();
    assert_eq!(again.id, grant.id);
    assert_eq!(
        held(&c.bo, c.area, c.secret).await,
        ["add", "edit", "read", "remove"]
    );

    // The owner sees every grant; a reader sees those to them and their
    // groups.
    let all = c.ann.api.clan_secret_grants(&c.secret).await.unwrap();
    assert_eq!(
        all.iter().map(|grant| grant.id).collect::<Vec<_>>(),
        [grant.id, to_group.id]
    );
    let mine = c.mira.api.clan_secret_grants(&c.secret).await.unwrap();
    assert_eq!(
        mine.iter().map(|grant| grant.id).collect::<Vec<_>>(),
        [to_group.id]
    );

    // Changing and revoking.
    let changed = c
        .ann
        .api
        .update_clan_secret_grant(&c.secret, to_group.id, &["add"])
        .await
        .unwrap();
    assert!(changed.can("add") && changed.can("read"));
    c.ann
        .api
        .revoke_clan_secret_grant(&c.secret, to_group.id)
        .await
        .unwrap();
    assert!(held(&c.mira, c.area, c.secret).await.is_empty());
    assert!(is_not_found(
        &c.mira.api.clan_secret_grants(&c.secret).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manager_shares_within_their_own_actions() {
    let c = fixture().await;
    let manager = c
        .ann
        .api
        .grant_clan_secret(&c.secret, to(&c.bo), &["add", "manage_access"])
        .await
        .unwrap();

    // Bo gives what he holds, never more and never manage_access.
    assert!(is_not_found(
        &c.bo
            .api
            .grant_clan_secret(&c.secret, to(&c.cy), &["edit"])
            .await
    ));
    assert!(is_not_found(
        &c.bo
            .api
            .grant_clan_secret(&c.secret, to(&c.cy), &["manage_access"])
            .await
    ));
    let cy =
        c.bo.api
            .grant_clan_secret(&c.secret, to(&c.cy), &["add"])
            .await
            .expect("within his own actions");
    assert_eq!(cy.grantor_id, c.bo.user.id);

    // He keeps what a grant already has, even beyond his own, and may clear
    // it, but not add it back.
    c.ann
        .api
        .update_clan_secret_grant(&c.secret, cy.id, &["add", "remove"])
        .await
        .unwrap();
    c.bo.api
        .update_clan_secret_grant(&c.secret, cy.id, &["remove"])
        .await
        .expect("keeps remove, drops add");
    let cleared =
        c.bo.api
            .update_clan_secret_grant(&c.secret, cy.id, &[])
            .await
            .unwrap();
    assert!(!cleared.can("remove"));
    assert!(is_not_found(
        &c.bo
            .api
            .update_clan_secret_grant(&c.secret, cy.id, &["remove"])
            .await
    ));

    // Grants carrying manage_access are the owners' to change: his own
    // included.
    assert!(is_not_found(
        &c.bo
            .api
            .update_clan_secret_grant(&c.secret, manager.id, &["add"])
            .await
    ));
    assert!(is_not_found(
        &c.bo
            .api
            .revoke_clan_secret_grant(&c.secret, manager.id)
            .await
    ));
    c.bo.api
        .revoke_clan_secret_grant(&c.secret, cy.id)
        .await
        .expect("a grant without manage_access");

    // A reader without manage_access shares nothing.
    let reader = c
        .ann
        .api
        .grant_clan_secret(&c.secret, to(&c.cy), &[])
        .await
        .unwrap();
    assert!(is_not_found(
        &c.cy
            .api
            .grant_clan_secret(&c.secret, to(&c.mira), &[])
            .await
    ));
    assert!(is_not_found(
        &c.cy
            .api
            .revoke_clan_secret_grant(&c.secret, reader.id)
            .await
    ));
}

/// A member who may not read the member directory still names the clan's
/// groups, and shares their own Secret with one of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_without_the_directory_shares_with_a_group() {
    let c = fixture().await;
    // The clan narrows its founding grant: the directory is for owners.
    let everyone = c.server.clan_builtin_group(c.clan, "members");
    let founding = c
        .mira
        .api
        .clan_grants(
            c.clan,
            smudgy_cloud::clans::ClanGrantFilter {
                group_id: Some(everyone),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .find(|grant| grant.actions.contains("clan.read_members"))
        .expect("the founding grant");
    c.mira
        .api
        .delete_clan_grant(c.clan, founding.id)
        .await
        .expect("an owner narrows it");
    assert!(is_not_found(&c.ann.api.clan_members(c.clan).await));

    let recipients = c
        .ann
        .api
        .clan_recipients(c.clan)
        .await
        .expect("the directory's refusal leaves the groups");
    assert_eq!(recipients.members, None);
    let everyone = recipients
        .groups
        .iter()
        .find(|group| group.builtin.as_deref() == Some("members"))
        .expect("every member names Everyone")
        .id;
    assert!(held(&c.bo, c.area, c.secret).await.is_empty());
    let grant = c
        .ann
        .api
        .grant_clan_secret(
            &c.secret,
            SecretRecipient::Group { group_id: everyone },
            &[],
        )
        .await
        .expect("a member shares their Secret with a group");
    assert_eq!(grant.recipient.group(), Some(everyone));
    assert_eq!(held(&c.bo, c.area, c.secret).await, ["read"]);

    // The clan's owner reads the directory too.
    let owners_view = c.mira.api.clan_recipients(c.clan).await.unwrap();
    let members = owners_view.members.expect("the owner reads the directory");
    assert!(members.iter().any(|member| member.user_id == c.ann.user.id));
    let ids = |groups: &[smudgy_cloud::clans::ClanGroup]| -> Vec<Uuid> {
        groups.iter().map(|group| group.id).collect()
    };
    assert_eq!(ids(&owners_view.groups), ids(&recipients.groups));
    let _ = &c.server;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recipients_are_the_clans_members_and_groups() {
    let c = fixture().await;

    // Someone outside the clan, a group of another clan and an unknown one
    // are all the uniform 404.
    assert!(is_not_found(
        &c.ann
            .api
            .grant_clan_secret(&c.secret, to(&c.zed), &[])
            .await
    ));
    let other = c.zed.api.create_clan("Elsewhere").await.unwrap().id;
    let foreign = c
        .zed
        .api
        .create_clan_group(other, "Scouts", None)
        .await
        .unwrap()
        .id;
    for group in [foreign, Uuid::new_v4()] {
        assert!(is_not_found(
            &c.ann
                .api
                .grant_clan_secret(&c.secret, SecretRecipient::Group { group_id: group }, &[])
                .await
        ));
    }

    // A member who leaves loses their grants.
    c.ann
        .api
        .grant_clan_secret(&c.secret, to(&c.cy), &[])
        .await
        .unwrap();
    c.mira
        .api
        .remove_clan_member(c.clan, c.cy.user.id)
        .await
        .unwrap();
    assert!(
        c.ann
            .api
            .clan_secret_grants(&c.secret)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(is_not_found(
        &c.ann.api.grant_clan_secret(&c.secret, to(&c.cy), &[]).await
    ));

    // Someone who does not read the Secret learns nothing of it.
    assert!(is_not_found(&c.zed.api.clan_secret_grants(&c.secret).await));
    assert!(is_not_found(
        &c.bo.api.grant_clan_secret(&c.secret, to(&c.cy), &[]).await
    ));
    let _ = &c.server;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_bodies_are_refused_and_grant_writes_end_offers() {
    let c = fixture().await;

    // An unknown action is refused for its shape.
    let both = c
        .ann
        .api
        .update_clan_secret_grant(&c.secret, Uuid::new_v4(), &["fly"])
        .await;
    assert!(matches!(both, Err(CloudError::InvalidInput(_))), "{both:?}");

    // A pending ownership offer stands only while access stays as it was.
    c.ann
        .api
        .grant_clan_secret(&c.secret, to(&c.bo), &[])
        .await
        .unwrap();
    let offer = c
        .ann
        .api
        .offer_secret_ownership(
            &c.secret,
            &OfferRequest::to_members(vec![c.bo.user.id], false),
        )
        .await
        .unwrap();
    c.ann
        .api
        .grant_clan_secret(&c.secret, to(&c.cy), &[])
        .await
        .unwrap();
    assert!(is_not_found(
        &c.bo.api.accept_secret_offer(&c.secret, offer.id).await
    ));
}

/// Lets every member copy the fixture's map.
fn everyone_copies_the_map(c: &Fixture, more: &[&str]) {
    let everyone = c.server.clan_builtin_group(c.clan, "members");
    let mut actions = vec!["area.copy"];
    actions.extend_from_slice(more);
    c.server.clan_grant(
        c.clan,
        ClanRecipient::Group(everyone),
        ClanGrantScope::Areas([c.area.0].into()),
        &actions,
    );
}

/// The names of the Secrets on `member`'s copy of the fixture's map, each
/// an owner Secret of the copy.
async fn copied_secrets(member: &Member, area: AreaId) -> Vec<String> {
    let copy = member
        .api
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .expect("the member copies the map");
    member
        .api
        .area_secrets(copy.id)
        .await
        .expect("the copy's Secrets")
        .into_iter()
        .map(|secret| {
            assert_eq!(secret.ownership, "owner");
            assert_eq!(secret.clan_id, None);
            secret.name
        })
        .collect()
}

/// `copy` is in no preset: a grant carries it only when it names it, and
/// only someone who holds it gives it. A copy of the map carries the
/// Secret for whoever holds `copy` and leaves it out for everyone else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_comes_only_from_a_grant_that_names_it() {
    for preset in [
        secret_preset::READER,
        secret_preset::CONTRIBUTOR,
        secret_preset::EDITOR,
        secret_preset::ACCESS_MANAGER,
    ] {
        assert!(!preset.contains(&secret_action::COPY), "{preset:?}");
    }
    let c = fixture().await;
    everyone_copies_the_map(&c, &[]);

    // An Editor who manages access, without copy.
    let bo = c
        .ann
        .api
        .grant_clan_secret(
            &c.secret,
            to(&c.bo),
            &["add", "edit", "remove", "manage_access"],
        )
        .await
        .unwrap();
    assert!(!bo.can(secret_action::COPY));
    assert_eq!(
        held(&c.bo, c.area, c.secret).await,
        ["add", "edit", "manage_access", "read", "remove"]
    );
    assert!(is_not_found(
        &c.bo
            .api
            .grant_clan_secret(&c.secret, to(&c.cy), &["copy"])
            .await
    ));
    assert!(copied_secrets(&c.bo, c.area).await.is_empty());

    // Named, it is held, given on, and taken along.
    c.ann
        .api
        .update_clan_secret_grant(
            &c.secret,
            bo.id,
            &["add", "edit", "remove", "manage_access", "copy"],
        )
        .await
        .unwrap();
    let cy =
        c.bo.api
            .grant_clan_secret(&c.secret, to(&c.cy), &["copy"])
            .await
            .expect("within his own actions");
    assert_eq!(
        cy.actions.iter().map(String::as_str).collect::<Vec<_>>(),
        ["copy", "read"]
    );
    // Its recorded owner holds it through ownership.
    for copier in [&c.ann, &c.bo, &c.cy] {
        assert_eq!(copied_secrets(copier, c.area).await, ["Quest"]);
    }
    // The clan's owner, who does not read a Member-owned Secret without a
    // grant, copies the map without it.
    assert!(copied_secrets(&c.mira, c.area).await.is_empty());
}

/// On a Clan-owned Secret, a clan grant of `secret.copy` covering its map
/// gives `copy`, and the clan's owners hold it through ownership. It never
/// reaches a Member-owned Secret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clan_grant_of_secret_copy_takes_clan_owned_secrets_along() {
    let c = fixture().await;
    everyone_copies_the_map(&c, &["secret.read"]);
    let hoard = c
        .mira
        .maps
        .create_secret_as(
            &c.area,
            &NewSecret {
                name: "Hoard".to_string(),
                color: None,
                owner: NewSecretOwner::Clan { clan_id: c.clan },
            },
            c.mira.maps.auth_generation(),
        )
        .await
        .unwrap()
        .source;
    c.ann
        .api
        .grant_clan_secret(&c.secret, to(&c.bo), &[])
        .await
        .unwrap();

    // Bo reads both through `secret.read` and his grant, and copies neither.
    assert_eq!(held(&c.bo, c.area, hoard).await, ["read"]);
    assert!(copied_secrets(&c.bo, c.area).await.is_empty());

    everyone_copies_the_map(&c, &[action::COPY_SECRETS]);
    assert_eq!(held(&c.bo, c.area, hoard).await, ["copy", "read"]);
    assert_eq!(
        held(&c.bo, c.area, c.secret).await,
        ["read"],
        "the clan's grants never reach a Member-owned Secret"
    );
    assert_eq!(copied_secrets(&c.bo, c.area).await, ["Hoard"]);
    assert_eq!(copied_secrets(&c.mira, c.area).await, ["Hoard"]);
}

/// A frozen Secret gives only `read`: nobody copies it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nobody_copies_a_frozen_secret() {
    let c = fixture().await;
    everyone_copies_the_map(&c, &[]);
    c.ann
        .api
        .grant_clan_secret(&c.secret, to(&c.bo), &["copy"])
        .await
        .unwrap();
    assert_eq!(held(&c.bo, c.area, c.secret).await, ["copy", "read"]);

    c.server.forget_account_in_clan_secrets(c.ann.user.id);
    assert_eq!(held(&c.bo, c.area, c.secret).await, ["read"]);
    assert!(copied_secrets(&c.bo, c.area).await.is_empty());
}
