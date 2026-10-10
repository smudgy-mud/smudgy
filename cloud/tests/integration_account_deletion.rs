//! Account deletion (`DELETE /me`) end to end over real HTTP against the
//! contract-shaped mock: the session-only credential, the 409 `last_owner`
//! refusal, what goes with the account, and a deletion left partway.

mod support;

use smudgy_cloud::{
    AreaId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource, MapperBackend,
};
use support::{GrantFlags, GrantScope, MockHandle, MockServer, TestUser};
use uuid::Uuid;

fn session_client(server: &MockHandle, user: &TestUser) -> CloudApiClient {
    CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    )
}

fn api_key_client(server: &MockHandle, user: &TestUser) -> CloudApiClient {
    CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::ApiKey(user.api_key.clone()))),
    )
}

fn user(server: &MockHandle, nickname: &str) -> (TestUser, CloudApiClient) {
    let user = server.create_user(&format!("{nickname}@example.com"), nickname, true);
    let client = session_client(server, &user);
    (user, client)
}

async fn visible_maps(client: &CloudApiClient) -> Vec<AreaId> {
    client
        .sync()
        .await
        .expect("sync")
        .into_iter()
        .map(|row| row.area_id)
        .collect()
}

fn is_unauthorized<T: std::fmt::Debug>(result: &Result<T, CloudError>) -> bool {
    matches!(result, Err(CloudError::Unauthorized(_)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_key_cannot_delete_the_account() {
    let server = MockServer::spawn().await;
    let (mira, session) = user(&server, "mira");

    let refused = api_key_client(&server, &mira).delete_account().await;
    assert!(is_unauthorized(&refused), "{refused:?}");
    assert!(session.me().await.is_ok(), "nothing changed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_removes_maps_grants_friendships_and_the_account() {
    let server = MockServer::spawn().await;
    let (mira, mira_client) = user(&server, "mira");
    let (tomas, tomas_client) = user(&server, "tomas");
    server.befriend(&mira, &tomas);

    let mira_map = server.create_area(&mira, "Lantern Docks");
    let folder = server.create_atlas(&mira, "Coast");
    let filed = server.create_area_in_atlas(&mira, "Harbor", folder);
    server.grant(
        &mira,
        &tomas,
        GrantScope::Area(mira_map),
        GrantFlags::edit(),
    );
    server.grant(
        &mira,
        &tomas,
        GrantScope::Atlas(folder),
        GrantFlags::VIEW_ONLY,
    );
    let tomas_map = server.create_area(&tomas, "Old Mill");
    server.grant(
        &tomas,
        &mira,
        GrantScope::Area(tomas_map),
        GrantFlags::VIEW_ONLY,
    );

    let shared = visible_maps(&tomas_client).await;
    assert!(shared.contains(&mira_map) && shared.contains(&filed));

    mira_client.delete_account().await.expect("delete");

    let after = visible_maps(&tomas_client).await;
    assert_eq!(after, vec![tomas_map], "only tomas's own map is left");
    assert!(tomas_client.friends().await.expect("friends").is_empty());
    assert!(
        tomas_client
            .area_shares(tomas_map)
            .await
            .expect("tomas's shares")
            .is_empty(),
        "the grant mira held is gone"
    );
    {
        let st = server.state.lock();
        assert!(!st.areas.contains_key(&mira_map.0));
        assert!(!st.areas.contains_key(&filed.0));
        assert!(!st.atlases.contains_key(&folder));
        assert!(st.grants.is_empty());
        assert!(st.users.iter().all(|u| u.id != mira.id));
    }

    // The credential is unknown now: every route answers 401, a repeat too.
    assert!(is_unauthorized(&mira_client.me().await));
    assert!(is_unauthorized(&mira_client.refresh().await));
    assert!(is_unauthorized(&mira_client.delete_account().await));

    // The nickname is free again.
    let (_, again) = user(&server, "mira");
    let profile = again.me().await.expect("a new account");
    assert_eq!(profile.nickname.as_deref(), Some("mira"));
    assert_ne!(profile.id, mira.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_last_owner_of_a_clan_is_refused_and_nothing_changes() {
    let server = MockServer::spawn().await;
    let (mira, mira_client) = user(&server, "mira");
    let (_, tomas_client) = user(&server, "tomas");
    let mira_map = server.create_area(&mira, "Lantern Docks");
    let clan = mira_client
        .create_clan("Lantern Company")
        .await
        .expect("clan");

    let refused = mira_client.delete_account().await;
    assert!(matches!(refused, Err(CloudError::LastOwner)), "{refused:?}");
    assert!(mira_client.me().await.is_ok(), "the account still works");
    assert!(mira_client.refresh().await.is_ok());
    assert!(server.state.lock().areas.contains_key(&mira_map.0));

    // With a second owner, the account leaves the clan and goes.
    let tomas = tomas_client.me().await.expect("tomas").id;
    let invitation = mira_client
        .invite_to_clan(clan.id, tomas, &[])
        .await
        .expect("invite");
    tomas_client
        .accept_clan_invitation(invitation.id)
        .await
        .expect("join");
    mira_client
        .set_clan_owner(clan.id, tomas, true)
        .await
        .expect("second owner");

    mira_client.delete_account().await.expect("delete");
    let members = tomas_client.clan_members(clan.id).await.expect("members");
    assert_eq!(
        members.iter().map(|m| m.user_id).collect::<Vec<Uuid>>(),
        vec![tomas],
        "the clan stays with its other owner"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deletion_left_partway_locks_the_credentials_and_a_repeat_finishes_it() {
    let server = MockServer::spawn().await;
    let (mira, mira_client) = user(&server, "mira");
    let mira_map = server.create_area(&mira, "Lantern Docks");
    server.interrupt_next_account_deletions(1);

    let interrupted = mira_client.delete_account().await;
    assert!(interrupted.is_err(), "{interrupted:?}");
    assert!(
        !matches!(interrupted, Err(CloudError::Unauthorized(_))),
        "an interruption is not a 401: {interrupted:?}"
    );

    // Marked: the session works only for DELETE /me now.
    assert!(is_unauthorized(&mira_client.me().await));
    assert!(is_unauthorized(&mira_client.refresh().await));
    assert!(is_unauthorized(&mira_client.sync().await));
    assert!(server.state.lock().areas.contains_key(&mira_map.0));

    mira_client
        .delete_account()
        .await
        .expect("the repeat finishes");
    assert!(!server.state.lock().areas.contains_key(&mira_map.0));
    assert!(is_unauthorized(&mira_client.delete_account().await));
}

/// Whichever client sharing a credential meets the server refusing it (map sync, the map
/// editor's writes, account calls; packages go through the same check) notes the refusal on
/// the shared source, so the app can tell that the account it is deleting is gone from a 401
/// anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_client_notes_a_refusal_of_the_credential_it_shares() {
    let server = MockServer::spawn().await;
    let (mira, _) = user(&server, "mira");
    let credentials = CredentialSource::new(Some(Credential::Session(mira.session_token.clone())));
    let mut refusals = credentials.refusals();
    let mapper = CloudMapper::with_credentials(server.base_url.clone(), credentials.clone());
    let account = CloudApiClient::new(server.base_url.clone(), credentials.clone());

    mapper.sync_state().await.expect("the session works");
    account.me().await.expect("the session works");
    assert!(!credentials.current_was_refused());

    session_client(&server, &mira)
        .delete_account()
        .await
        .expect("delete");

    for (client, refused) in [
        ("map sync", mapper.sync_state().await.err()),
        ("account", account.me().await.err()),
    ] {
        assert!(
            matches!(refused, Some(CloudError::Unauthorized(_))),
            "{client}: {refused:?}"
        );
    }
    assert!(credentials.current_was_refused());
    assert!(refusals.has_changed().expect("the source lives"));
    refusals.borrow_and_update();

    // A new credential has not been refused.
    credentials.set(Some(Credential::Session("smudgy_sess_other".to_string())));
    assert!(!credentials.current_was_refused());
    // A detached copy reports to the same source.
    let (_, frozen) = credentials.freeze();
    let frozen_mapper = CloudMapper::with_credentials(server.base_url.clone(), frozen);
    assert!(frozen_mapper.sync_state().await.is_err());
    assert!(credentials.current_was_refused());
    assert!(refusals.has_changed().expect("the source lives"));
}
