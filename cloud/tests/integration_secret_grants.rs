//! Sharing one Secret at a time (`/secrets/{id}/grants`) over real HTTP
//! against the contract-shaped mock in `tests/support/`: who may grant what,
//! what a grant shows its grantee, and what takes it away.
#![allow(clippy::too_many_lines)]

mod support;

use std::collections::BTreeSet;

use smudgy_cloud::cloud_api::{
    CopyAreaRequest, CreateShareRequest, SecretGrant, ShareScope, secret_action,
};
use smudgy_cloud::{
    AreaId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource, MapperBackend,
    SourceBundle, SourceId,
};
use support::{GrantFlags, GrantScope, MockHandle, MockServer, TestUser};
use uuid::Uuid;

fn api_client(server: &MockHandle, user: &TestUser) -> CloudApiClient {
    CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::ApiKey(user.api_key.clone()))),
    )
}

fn mapper(server: &MockHandle, user: &TestUser) -> CloudMapper {
    CloudMapper::new(server.base_url.clone(), user.api_key.clone())
}

fn actions(grant: &SecretGrant) -> Vec<&str> {
    grant.actions.iter().map(String::as_str).collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(ToString::to_string).collect()
}

/// The Secret's bundle in `user`'s projection of the map, if they read it.
async fn bundle_for(
    server: &MockHandle,
    user: &TestUser,
    area: AreaId,
    secret: &SourceId,
) -> Option<SourceBundle> {
    mapper(server, user)
        .get_area(&area)
        .await
        .ok()?
        .sources
        .into_iter()
        .find(|bundle| &bundle.source == secret)
}

/// An owner, a friend reading the map, and one Secret on it.
struct Shared {
    server: MockHandle,
    owner: TestUser,
    friend: TestUser,
    area: AreaId,
    secret: SourceId,
    map_share: Uuid,
}

async fn shared_map() -> Shared {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let friend = server.create_user("friend@example.com", "tomas", true);
    server.befriend(&owner, &friend);
    let area = server.create_area(&owner, "DV");
    server.add_room(area, 1, "Gate");
    let secret = SourceId::Secret(server.add_secret(area, "Bookcase"));
    let map_share = server.grant(
        &owner,
        &friend,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    Shared {
        server,
        owner,
        friend,
        area,
        secret,
        map_share,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_round_trips() {
    let Shared {
        server,
        owner,
        friend,
        area,
        secret,
        ..
    } = shared_map().await;
    let owner_client = api_client(&server, &owner);
    let friend_client = api_client(&server, &friend);

    assert!(
        owner_client
            .secret_grants(&secret)
            .await
            .expect("an unshared Secret lists")
            .is_empty()
    );

    let grant = owner_client
        .grant_secret(&secret, friend.id, &[secret_action::EDIT])
        .await
        .expect("the owner shares the Secret");
    assert_eq!(grant.secret_id, secret);
    assert_eq!(grant.area_id, area);
    assert_eq!(grant.owner_id, owner.id);
    assert_eq!(grant.grantor_id, owner.id);
    assert_eq!(grant.grantee_id, friend.id);
    assert_eq!(actions(&grant), ["edit", "read"], "read always rides along");

    let listed = owner_client.secret_grants(&secret).await.expect("listing");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, grant.id);
    assert_eq!(listed[0].grantee_nickname.as_deref(), Some("tomas"));

    // The grantee sees the grant naming them.
    let own = friend_client
        .secret_grants(&secret)
        .await
        .expect("a reader lists their own grants");
    assert_eq!(own.iter().map(|g| g.id).collect::<Vec<_>>(), [grant.id]);

    let changed = owner_client
        .update_secret_grant(&secret, grant.id, &[secret_action::ADD])
        .await
        .expect("the owner changes the grant");
    assert_eq!(changed.id, grant.id);
    assert_eq!(actions(&changed), ["add", "read"]);

    owner_client
        .revoke_secret_grant(&secret, grant.id)
        .await
        .expect("the owner revokes the grant");
    assert!(
        owner_client
            .secret_grants(&secret)
            .await
            .expect("listing")
            .is_empty()
    );
    assert!(bundle_for(&server, &friend, area, &secret).await.is_none());
    assert!(matches!(
        friend_client.secret_grants(&secret).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));

    // Only Secrets have grants; the client refuses before sending.
    assert!(matches!(
        owner_client.secret_grants(&SourceId::Map).await,
        Err(CloudError::InvalidInput(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_projection_shows_the_secret_with_the_granted_actions() {
    let Shared {
        server,
        owner,
        friend,
        area,
        secret,
        ..
    } = shared_map().await;
    assert!(
        bundle_for(&server, &friend, area, &secret).await.is_none(),
        "no grant, no Secret"
    );
    let token_before = mapper(&server, &friend)
        .get_area(&area)
        .await
        .expect("the map")
        .area
        .projection_token;

    api_client(&server, &owner)
        .grant_secret(
            &secret,
            friend.id,
            &[secret_action::EDIT, secret_action::REMOVE],
        )
        .await
        .expect("grant");
    let bundle = bundle_for(&server, &friend, area, &secret)
        .await
        .expect("the Secret reaches the grantee");
    assert_eq!(bundle.name.as_deref(), Some("Bookcase"));
    assert_eq!(bundle.actions, set(&["read", "edit", "remove"]));

    let listed = api_client(&server, &friend)
        .area_secrets(area)
        .await
        .expect("the grantee lists the map's Secrets");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].actions, set(&["read", "edit", "remove"]));

    let token_after = mapper(&server, &friend)
        .get_area(&area)
        .await
        .expect("the map")
        .area
        .projection_token;
    assert_ne!(
        token_before, token_after,
        "the reader's projection token moves when a Secret appears"
    );

    // The owner holds every action an owner Secret takes: managing access,
    // copying, renaming and deleting included.
    let own = bundle_for(&server, &owner, area, &secret)
        .await
        .expect("the owner reads the Secret");
    assert_eq!(
        own.actions,
        set(&[
            "read",
            "add",
            "edit",
            "remove",
            "manage_access",
            "copy",
            "rename",
            "delete"
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repeat_grant_replaces_the_first() {
    let Shared {
        server,
        owner,
        friend,
        secret,
        ..
    } = shared_map().await;
    let client = api_client(&server, &owner);
    let first = client
        .grant_secret(&secret, friend.id, &[secret_action::ADD])
        .await
        .expect("first grant");
    let second = client
        .grant_secret(&secret, friend.id, &[secret_action::REMOVE])
        .await
        .expect("second grant");
    assert_eq!(second.id, first.id, "the same grant, replaced");
    assert_eq!(actions(&second), ["read", "remove"]);
    assert_eq!(client.secret_grants(&secret).await.expect("list").len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grants_are_refused_for_strangers_the_owner_and_the_grantor() {
    let Shared {
        server,
        owner,
        friend,
        area,
        secret,
        ..
    } = shared_map().await;
    let stranger = server.create_user("stranger@example.com", "stranger", true);
    let client = api_client(&server, &owner);

    for grantee in [stranger.id, owner.id] {
        assert!(matches!(
            client.grant_secret(&secret, grantee, &[]).await,
            Err(CloudError::NotFoundOrNoAccess)
        ));
    }
    let unknown = SourceId::Secret(Uuid::new_v4());
    assert!(matches!(
        client.grant_secret(&unknown, friend.id, &[]).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    assert!(matches!(
        client.grant_secret(&secret, friend.id, &["fly"]).await,
        Err(CloudError::InvalidInput(_))
    ));

    // A blocked friend is refused like a stranger.
    let blocked = server.create_user("blocked@example.com", "blocked", true);
    server.befriend(&owner, &blocked);
    server.block(&blocked, &owner);
    assert!(matches!(
        client.grant_secret(&secret, blocked.id, &[]).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));

    let unverified = server.create_user("new@example.com", "newcomer", false);
    assert!(matches!(
        api_client(&server, &unverified)
            .secret_grants(&secret)
            .await,
        Err(CloudError::EmailNotVerified)
    ));

    // A map admin has no say over the map's Secrets without a grant.
    let admin = server.create_user("admin@example.com", "admin", true);
    server.befriend(&owner, &admin);
    server.befriend(&admin, &friend);
    server.grant(&owner, &admin, GrantScope::Area(area), GrantFlags::admin());
    let admin_client = api_client(&server, &admin);
    assert!(matches!(
        admin_client.grant_secret(&secret, friend.id, &[]).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    assert!(matches!(
        admin_client.secret_grants(&secret).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    assert!(bundle_for(&server, &admin, area, &secret).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manager_grants_within_their_actions_and_never_manage_access() {
    let Shared {
        server,
        owner,
        friend: manager,
        area,
        secret,
        ..
    } = shared_map().await;
    let reader = server.create_user("reader@example.com", "reader", true);
    server.befriend(&owner, &reader);
    server.befriend(&manager, &reader);
    server.grant(
        &owner,
        &reader,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    let owner_client = api_client(&server, &owner);
    let manager_client = api_client(&server, &manager);

    let manager_grant = owner_client
        .grant_secret(
            &secret,
            manager.id,
            &[
                secret_action::ADD,
                secret_action::EDIT,
                secret_action::MANAGE_ACCESS,
            ],
        )
        .await
        .expect("the owner makes a manager");

    for refused in [[secret_action::REMOVE], [secret_action::MANAGE_ACCESS]] {
        assert!(
            matches!(
                manager_client
                    .grant_secret(&secret, reader.id, &refused)
                    .await,
                Err(CloudError::NotFoundOrNoAccess)
            ),
            "{refused:?} is beyond the manager"
        );
    }
    let issued = manager_client
        .grant_secret(&secret, reader.id, &[secret_action::EDIT])
        .await
        .expect("the manager shares within their actions");
    assert_eq!(issued.grantor_id, manager.id);

    // The grant carrying manage_access is the owner's to change or revoke.
    assert!(matches!(
        manager_client
            .update_secret_grant(&secret, manager_grant.id, &[secret_action::EDIT])
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    assert!(matches!(
        manager_client
            .revoke_secret_grant(&secret, manager_grant.id)
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));

    // Raising past their own actions is refused; lowering is fine.
    assert!(matches!(
        manager_client
            .update_secret_grant(
                &secret,
                issued.id,
                &[secret_action::EDIT, secret_action::REMOVE]
            )
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    let lowered = manager_client
        .update_secret_grant(&secret, issued.id, &[secret_action::ADD])
        .await
        .expect("the manager lowers their grant");
    assert_eq!(actions(&lowered), ["add", "read"]);

    // Only the owner grants manage_access, and only on grants the owner made.
    assert!(matches!(
        owner_client
            .update_secret_grant(
                &secret,
                issued.id,
                &[secret_action::ADD, secret_action::MANAGE_ACCESS]
            )
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    owner_client
        .update_secret_grant(
            &secret,
            issued.id,
            &[secret_action::ADD, secret_action::REMOVE],
        )
        .await
        .expect("the owner changes the manager's grant within the rules");

    // The manager sees every grant; the reader only theirs.
    let all = manager_client.secret_grants(&secret).await.expect("list");
    assert_eq!(
        all.iter().map(|g| g.id).collect::<Vec<_>>(),
        [manager_grant.id, issued.id],
        "oldest first"
    );
    let own = api_client(&server, &reader)
        .secret_grants(&secret)
        .await
        .expect("the reader lists");
    assert_eq!(own.iter().map(|g| g.id).collect::<Vec<_>>(), [issued.id]);

    // A manager may revoke any grant without manage_access, whoever issued it.
    let from_owner = owner_client
        .grant_secret(&secret, reader.id, &[secret_action::ADD])
        .await
        .expect("the owner's own grant to the reader");
    assert_ne!(from_owner.id, issued.id, "one grant per grantor");
    manager_client
        .revoke_secret_grant(&secret, from_owner.id)
        .await
        .expect("the manager revokes the owner's plain grant");

    // A grantee's actions are the union of their grants, and a manager's
    // grants stand when the manager loses manage_access.
    owner_client
        .revoke_secret_grant(&secret, manager_grant.id)
        .await
        .expect("the owner revokes the manager");
    assert!(bundle_for(&server, &manager, area, &secret).await.is_none());
    let bundle = bundle_for(&server, &reader, area, &secret)
        .await
        .expect("the manager's grant stands");
    assert_eq!(bundle.actions, set(&["read", "add", "remove"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_the_map_share_hides_the_secret_and_resharing_restores_it() {
    let Shared {
        server,
        owner,
        friend,
        area,
        secret,
        map_share,
    } = shared_map().await;
    let owner_client = api_client(&server, &owner);
    let grant = owner_client
        .grant_secret(&secret, friend.id, &[secret_action::ADD])
        .await
        .expect("grant");
    assert!(bundle_for(&server, &friend, area, &secret).await.is_some());

    owner_client
        .revoke_share(map_share)
        .await
        .expect("the map share goes");
    assert!(matches!(
        mapper(&server, &friend).get_area(&area).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    assert!(matches!(
        api_client(&server, &friend).secret_grants(&secret).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    // The grant stays in place, for the owner to see.
    let kept = owner_client.secret_grants(&secret).await.expect("listing");
    assert_eq!(kept.iter().map(|g| g.id).collect::<Vec<_>>(), [grant.id]);

    owner_client
        .create_share(CreateShareRequest {
            grantee_id: friend.id,
            scope: ShareScope::Area { area_id: area },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            can_admin: false,
            host_hints: None,
        })
        .await
        .expect("the map is shared again");
    let back = bundle_for(&server, &friend, area, &secret)
        .await
        .expect("the Secret comes back with the map");
    assert_eq!(back.actions, set(&["read", "add"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_can_be_issued_before_the_grantee_reads_the_map() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let friend = server.create_user("friend@example.com", "tomas", true);
    server.befriend(&owner, &friend);
    let area = server.create_area(&owner, "DV");
    let secret = SourceId::Secret(server.add_secret(area, "Bookcase"));
    api_client(&server, &owner)
        .grant_secret(&secret, friend.id, &[])
        .await
        .expect("granting needs no map access yet");
    assert!(bundle_for(&server, &friend, area, &secret).await.is_none());
    server.grant(
        &owner,
        &friend,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    let bundle = bundle_for(&server, &friend, area, &secret)
        .await
        .expect("the Secret shows once the map does");
    assert_eq!(bundle.actions, set(&["read"]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unfriending_and_blocking_remove_grants() {
    let Shared {
        server,
        owner,
        friend,
        area,
        secret,
        ..
    } = shared_map().await;
    let owner_client = api_client(&server, &owner);
    owner_client
        .grant_secret(&secret, friend.id, &[secret_action::EDIT])
        .await
        .expect("grant");

    api_client(&server, &friend)
        .unfriend(owner.id)
        .await
        .expect("unfriend");
    assert!(
        owner_client
            .secret_grants(&secret)
            .await
            .expect("listing")
            .is_empty(),
        "unfriending deletes the grant"
    );
    assert!(bundle_for(&server, &friend, area, &secret).await.is_none());

    // Blocking deletes grants on the blocker's Secrets whoever issued them.
    let manager = server.create_user("manager@example.com", "manager", true);
    let reader = server.create_user("reader@example.com", "reader", true);
    for (a, b) in [(&owner, &manager), (&owner, &reader), (&manager, &reader)] {
        server.befriend(a, b);
    }
    for user in [&manager, &reader] {
        server.grant(&owner, user, GrantScope::Area(area), GrantFlags::VIEW_ONLY);
    }
    owner_client
        .grant_secret(
            &secret,
            manager.id,
            &[secret_action::ADD, secret_action::MANAGE_ACCESS],
        )
        .await
        .expect("manager");
    api_client(&server, &manager)
        .grant_secret(&secret, reader.id, &[secret_action::ADD])
        .await
        .expect("the manager shares on");
    owner_client.block(reader.id).await.expect("block");
    let left = owner_client.secret_grants(&secret).await.expect("listing");
    assert_eq!(
        left.iter().map(|g| g.grantee_id).collect::<Vec<_>>(),
        [manager.id],
        "the blocked reader's grant from the manager is gone too"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_secret_deletes_its_grants() {
    let Shared {
        server,
        owner,
        friend,
        area,
        secret,
        ..
    } = shared_map().await;
    let owner_client = api_client(&server, &owner);
    owner_client
        .grant_secret(&secret, friend.id, &[secret_action::EDIT])
        .await
        .expect("grant");
    let backend = mapper(&server, &owner);
    backend
        .delete_secret(&area, &secret, backend.auth_generation())
        .await
        .expect("the owner deletes the Secret");
    assert!(matches!(
        owner_client.secret_grants(&secret).await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
    assert!(bundle_for(&server, &friend, area, &secret).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grants_go_through_the_cloud_backend_too() {
    let Shared {
        server,
        owner,
        friend,
        area,
        secret,
        ..
    } = shared_map().await;
    let backend = mapper(&server, &owner);
    let generation = backend.auth_generation();
    let grant = backend
        .grant_secret(&area, &secret, friend.id, &[secret_action::ADD], generation)
        .await
        .expect("grant through the backend");
    assert_eq!(actions(&grant), ["add", "read"]);
    let listed = backend
        .secret_grants(&area, &secret, generation)
        .await
        .expect("list through the backend");
    assert_eq!(listed.len(), 1);
    let changed = backend
        .update_secret_grant(&area, &secret, grant.id, &[secret_action::EDIT], generation)
        .await
        .expect("change through the backend");
    assert_eq!(actions(&changed), ["edit", "read"]);
    backend
        .revoke_secret_grant(&area, &secret, grant.id, generation)
        .await
        .expect("revoke through the backend");
    assert!(
        backend
            .secret_grants(&area, &secret, generation)
            .await
            .expect("list")
            .is_empty()
    );
}

/// `copy` is the owner's, and anyone else holds it only through a grant
/// that names it; a manager gives it only while holding it. A copy of the
/// map carries the Secret for whoever holds `copy`, leaves it out for
/// everyone else, and still needs the map's own copy permission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_comes_only_from_a_grant_that_names_it() {
    let Shared {
        server,
        owner,
        friend: manager,
        area,
        secret,
        ..
    } = shared_map().await;
    let reader = server.create_user("reader@example.com", "reader", true);
    server.befriend(&owner, &reader);
    server.befriend(&manager, &reader);
    server.grant(
        &owner,
        &reader,
        GrantScope::Area(area),
        GrantFlags::VIEW_ONLY,
    );
    server.grant(
        &owner,
        &manager,
        GrantScope::Area(area),
        GrantFlags {
            can_copy: true,
            ..GrantFlags::VIEW_ONLY
        },
    );
    let owner_client = api_client(&server, &owner);
    let manager_client = api_client(&server, &manager);
    let everything_but_copy = [
        secret_action::ADD,
        secret_action::EDIT,
        secret_action::REMOVE,
        secret_action::MANAGE_ACCESS,
    ];

    // Every other action leaves `copy` out, and the manager may not give
    // what they do not hold.
    let manager_grant = owner_client
        .grant_secret(&secret, manager.id, &everything_but_copy)
        .await
        .expect("the owner makes a manager");
    assert!(!manager_grant.can(secret_action::COPY));
    assert!(
        matches!(
            manager_client
                .grant_secret(&secret, reader.id, &[secret_action::COPY])
                .await,
            Err(CloudError::NotFoundOrNoAccess)
        ),
        "copy is beyond the manager"
    );
    let copy = manager_client
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .expect("the manager copies the map");
    assert!(
        manager_client
            .area_secrets(copy.id)
            .await
            .expect("the copy's Secrets")
            .is_empty(),
        "a Secret read without copy stays behind"
    );

    // Named, it is held, given on, and taken along.
    let mut with_copy = everything_but_copy.to_vec();
    with_copy.push(secret_action::COPY);
    owner_client
        .update_secret_grant(&secret, manager_grant.id, &with_copy)
        .await
        .expect("the owner gives copy");
    let bundle = bundle_for(&server, &manager, area, &secret)
        .await
        .expect("the manager reads the Secret");
    assert!(bundle.actions.contains(secret_action::COPY));
    let issued = manager_client
        .grant_secret(&secret, reader.id, &[secret_action::COPY])
        .await
        .expect("the manager gives copy, which they hold");
    assert_eq!(actions(&issued), ["copy", "read"]);
    let copy = manager_client
        .copy_area(area, &CopyAreaRequest::default())
        .await
        .expect("the manager copies the map");
    let carried = manager_client
        .area_secrets(copy.id)
        .await
        .expect("the copy's Secrets");
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].name, "Bookcase");
    assert_eq!(carried[0].ownership, "owner");
    assert_ne!(carried[0].source, secret);
    assert!(carried[0].actions.contains(secret_action::COPY));

    // The reader holds `copy` on the Secret, not on the map: no copy.
    assert!(matches!(
        api_client(&server, &reader)
            .copy_area(area, &CopyAreaRequest::default())
            .await,
        Err(CloudError::NotFoundOrNoAccess)
    ));
}
