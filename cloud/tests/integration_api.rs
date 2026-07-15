//! `CloudApiClient` end-to-end over real HTTP against the contract-shaped mock
//! in `tests/support/` — identity, social, share-validator, secrets/preview,
//! and clone flows.
#![allow(clippy::too_many_lines, clippy::similar_names)]

mod support;

use smudgy_cloud::cloud_api::{
    CopyAreaRequest, CreateShareRequest, PreviewAudience, RoomPropertyRef, SecretEntityKind,
    SecretMarksRequest, ShareDirection, ShareScope,
};
use smudgy_cloud::{
    AreaId, AtlasId, CloudApiClient, CloudMapper, CreateAreaRequest, Credential, CredentialSource,
    ExitId, LabelId, CloudError, MapperBackend, RoomNumber, ShapeId,
};
use support::{GrantFlags, GrantScope, MockHandle, MockServer, TestUser};
use uuid::Uuid;

/// A `CloudApiClient` bearing a fixed API key.
fn api_client(base_url: &str, api_key: &str) -> CloudApiClient {
    CloudApiClient::new(
        base_url,
        CredentialSource::new(Some(Credential::ApiKey(api_key.to_string()))),
    )
}

/// The nickname handle the mock allocated for `user`.
fn nickname_of(server: &MockHandle, user: &TestUser) -> String {
    let st = server.state.lock();
    st.user(user.id)
        .and_then(|u| u.nickname.clone())
        .expect("verified users have nicknames")
}

/// A view-only `POST /shares` body for an area.
fn view_only_share(grantee_id: Uuid, area_id: AreaId) -> CreateShareRequest {
    CreateShareRequest {
        grantee_id,
        scope: ShareScope::Area { area_id },
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        include_secrets: false,
        can_admin: false,
        host_hints: None,
    }
}

// ---------------------------------------------------------------------------
// 1. Full account lifecycle
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_account_lifecycle() {
    const EMAIL: &str = "lifecycle@example.com";

    let server = MockServer::spawn().await;
    let credentials = CredentialSource::empty();
    let client = CloudApiClient::new(server.base_url.clone(), credentials.clone());

    // Signup is enumeration-flat; the emailed code is fished out of state.
    client.signup(EMAIL, "lifer").await.expect("signup accepted");
    let code = server.verify_code_for(EMAIL).expect("code minted");

    // Wrong code and unknown email are the same uniform 404.
    let wrong = if code == "000000" { "999999" } else { "000000" };
    let err = client
        .verify_email(EMAIL, wrong)
        .await
        .expect_err("wrong code");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));
    let err = client
        .verify_email("nobody@example.com", &code)
        .await
        .expect_err("unknown email");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));

    let session = client
        .verify_email(EMAIL, &code)
        .await
        .expect("verify-email");
    assert!(session.session_token.starts_with("smudgy_sess_"));
    assert!(session.user.is_verified());
    assert!(!session.needs_nickname, "handle allocated at verification");
    let first_session_token = session.session_token.clone();

    // The code is single-use.
    let err = client
        .verify_email(EMAIL, &code)
        .await
        .expect_err("consumed code");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));

    // Hot-swap the shared credential source onto the fresh session.
    credentials.set(Some(Credential::Session(first_session_token.clone())));
    let me = client.me().await.expect("/me over the session");
    assert_eq!(me.email, EMAIL);
    assert!(me.is_verified());
    assert!(me.nickname.clone().is_some());

    // API-key minting is session-only.
    let created = client
        .create_api_key()
        .await
        .expect("create_api_key with a session credential");
    assert!(created.api_key.starts_with("smudgy_"));
    let api_key_client = api_client(&server.base_url, &created.api_key);
    let err = api_key_client
        .create_api_key()
        .await
        .expect_err("an API-key credential cannot mint keys");
    assert!(matches!(err, CloudError::Unauthorized(_)));

    let keys = client.api_keys().await.expect("list api keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].id, created.id);
    assert_eq!(
        keys[0].key_suffix.as_deref(),
        Some(created.key_suffix.as_str()),
        "listing shows the suffix, never key material"
    );

    // A returning login: email -> fresh code -> verify mints a second
    // session without touching the handle. An unknown email gets the same flat
    // 202 (no oracle) — under create-on-absent it silently provisions a
    // nickname-less account behind that identical response — and each request
    // supersedes the previous code. (The dedicated unified-flow assertions live
    // in `login_creates_account_on_first_sight`.)
    client
        .login("nobody@example.com")
        .await
        .expect("login is enumeration-flat");
    client.login(EMAIL).await.expect("login accepted");
    let stale = server.verify_code_for(EMAIL).expect("first login code");
    client.login(EMAIL).await.expect("login again (resend)");
    let fresh = server.verify_code_for(EMAIL).expect("superseding code");
    if stale != fresh {
        let err = client
            .verify_email(EMAIL, &stale)
            .await
            .expect_err("superseded code is dead");
        assert!(matches!(err, CloudError::NotFoundOrNoAccess));
    }
    let second = client
        .verify_email(EMAIL, &fresh)
        .await
        .expect("returning login");
    assert_ne!(second.session_token, first_session_token);
    assert!(!second.needs_nickname, "returning user keeps their handle");
    assert_eq!(second.user.nickname.clone(), session.user.nickname.clone());
    let sessions = client.sessions().await.expect("list sessions");
    assert!(sessions.len() >= 2, "both sessions listed");

    // Fish the second session's id out of mock state and delete it while
    // staying on the first session.
    let second_id = {
        let st = server.state.lock();
        st.sessions
            .get(&second.session_token)
            .expect("second session stored")
            .id
    };
    client
        .delete_session(second_id)
        .await
        .expect("delete the other session");
    let sessions = client.sessions().await.expect("list sessions again");
    assert!(sessions.iter().all(|s| s.id != second_id));
}

// ---------------------------------------------------------------------------
// 1a. The unified email-only entry: login creates the account on first sight
// ---------------------------------------------------------------------------

/// `login` on a brand-new email provisions a nickname-less account (no signup,
/// no nickname up front); verifying it signs in but signals `needs_nickname` so
/// the client prompts for a handle post-sign-in, which `set_nickname` then
/// claims. This is the cross-wire contract for the login flow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_creates_account_on_first_sight() {
    const EMAIL: &str = "first-sight@example.com";

    let server = MockServer::spawn().await;
    let credentials = CredentialSource::empty();
    let client = CloudApiClient::new(server.base_url.clone(), credentials.clone());

    // No signup: the email-only entry creates the account and mails a code.
    client.login(EMAIL).await.expect("login provisions on first sight");
    let code = server.verify_code_for(EMAIL).expect("code minted for the new account");

    let session = client.verify_email(EMAIL, &code).await.expect("verify-email");
    assert!(session.user.is_verified(), "the email is verified");
    assert!(
        session.needs_nickname,
        "a handle-less account must be prompted for a nickname post-sign-in",
    );
    assert!(session.user.nickname.is_none(), "no handle allocated yet");

    // Post-sign-in, the user claims a handle over the fresh session.
    credentials.set(Some(Credential::Session(session.session_token.clone())));
    let profile = client.set_nickname("newcomer").await.expect("claim handle");
    assert_eq!(profile.nickname.as_deref(), Some("newcomer"));

    // A returning login for the same email now keeps that handle (no re-prompt).
    client.login(EMAIL).await.expect("returning login");
    let code = server.verify_code_for(EMAIL).expect("returning code");
    let again = client.verify_email(EMAIL, &code).await.expect("verify again");
    assert!(!again.needs_nickname, "a returning user keeps their handle");
    assert_eq!(again.user.nickname.as_deref(), Some("newcomer"));
}

// ---------------------------------------------------------------------------
// 1b. Session refresh (POST /auth/refresh) — the launch + ~24h keep-alive
// ---------------------------------------------------------------------------

/// `client.refresh()` slides the session's idle deadline forward (returning the
/// same row with a ~365-day expiry and a stamped `last_used_at`), is
/// session-only (an API key is refused), and fails fast without a credential.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_refresh_extends_and_is_session_only() {
    let server = MockServer::spawn().await;
    let user = server.create_user("refresh@example.com", "refresher", true);

    let session_id = {
        let st = server.state.lock();
        st.sessions
            .get(&user.session_token)
            .expect("session exists")
            .id
    };

    // Wind the deadline back so a successful refresh visibly moves it forward.
    {
        let mut st = server.state.lock();
        let s = st
            .sessions
            .get_mut(&user.session_token)
            .expect("session exists");
        s.expires_at = chrono::Utc::now() + chrono::Duration::days(10);
    }

    // A session-bearing client refreshes: same row id, deadline jumps to ~365d.
    let session_client = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    );
    let refreshed = session_client.refresh().await.expect("refresh");
    assert_eq!(
        refreshed.id, session_id,
        "refresh returns the same session row"
    );
    assert!(
        refreshed.expires_at > chrono::Utc::now() + chrono::Duration::days(360),
        "refresh slides the deadline ~365 days out, got {}",
        refreshed.expires_at
    );
    assert!(
        refreshed.last_used_at.is_some(),
        "refresh stamps last_used_at"
    );

    // Session-only: an API-key client is refused (server 401 -> Unauthorized).
    let key_client = api_client(&server.base_url, &user.api_key);
    let err = key_client
        .refresh()
        .await
        .expect_err("an API key must not refresh");
    assert!(matches!(err, CloudError::Unauthorized(_)), "got {err:?}");

    // No credential -> fast Unauthorized (nothing leaves the client).
    let anon = CloudApiClient::new(server.base_url.clone(), CredentialSource::empty());
    let err = anon.refresh().await.expect_err("anon must not refresh");
    assert!(matches!(err, CloudError::Unauthorized(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// 1c. Client-version gate (graceful "client out of date" handling)
// ---------------------------------------------------------------------------

/// With the mock's version floor raised above this build, every request — on
/// BOTH HTTP clients — is rejected as `UpgradeRequired`; dropping the floor
/// back to `0.0.0` lets the same client through again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outdated_client_is_rejected_with_upgrade_required() {
    let server = MockServer::spawn().await;
    let user = server.create_user("old@example.com", "old", true);
    let area = server.create_area(&user, "Area");

    // Floor far above this build's CLIENT_VERSION: nobody passes.
    server.set_min_client_version("999.0.0");

    // CloudApiClient path (identity / social / shares / …).
    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    );
    let err = api.me().await.expect_err("outdated client rejected");
    assert!(matches!(err, CloudError::UpgradeRequired), "got {err:?}");
    assert!(err.is_upgrade_required());

    // CloudMapper path (area / room content) — the other HTTP client.
    let mapper = CloudMapper::new(server.base_url.clone(), user.api_key.clone());
    let err = mapper
        .get_area(&area)
        .await
        .expect_err("outdated mapper rejected");
    assert!(matches!(err, CloudError::UpgradeRequired), "got {err:?}");

    // Floor 0.0.0 disables the gate: the same (current) client now succeeds.
    server.set_min_client_version("0.0.0");
    api.me()
        .await
        .expect("current client passes once the floor is 0.0.0");
}

// ---------------------------------------------------------------------------
// 1d. Soft "upgrade available" nudge (NEWEST_CLIENT_VERSION)
// ---------------------------------------------------------------------------

/// An in-range client (>= floor, < newest) is allowed through, and the server's
/// `x-smudgy-upgrade-available` header surfaces on the client as the newest
/// version; with nothing advertised, nothing surfaces.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_range_client_sees_soft_upgrade_hint() {
    let server = MockServer::spawn().await;
    let user = server.create_user("behind@example.com", "behind", true);

    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    );

    // Nothing advertised yet -> no hint, even after a successful call.
    api.me().await.expect("me ok");
    assert_eq!(api.upgrade_available(), None);

    // Advertise a newer version (floor stays off, so the call still passes).
    server.set_newest_client_version("999.0.0");
    api.me().await.expect("me ok");
    assert_eq!(api.upgrade_available().as_deref(), Some("999.0.0"));
}

// ---------------------------------------------------------------------------
// 2. Friends + blocks lifecycle
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn friends_and_blocks_lifecycle() {
    let server = MockServer::spawn().await;
    let alice = server.create_user("alice@example.com", "alice", true);
    let bob = server.create_user("bob@example.com", "bob", true);
    let client_a = api_client(&server.base_url, &alice.api_key);
    let client_b = api_client(&server.base_url, &bob.api_key);

    // Exact-handle lookup, plus the case-insensitive nickname variant.
    let bob_nickname = nickname_of(&server, &bob);
    let hit = client_a.lookup(&bob_nickname).await.expect("exact lookup");
    assert_eq!(hit.user_id, bob.id);
    let hit = client_a
        .lookup(&bob_nickname.to_uppercase())
        .await
        .expect("nickname matching is case-insensitive");
    assert_eq!(hit.user_id, bob.id);
    let err = client_a
        .lookup("nobody")
        .await
        .expect_err("a miss is the uniform 404");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));

    // Request -> incoming -> accept -> friends on both sides.
    client_a
        .send_friend_request(bob.id)
        .await
        .expect("send is a flat 202");
    let requests = client_b.friend_requests().await.expect("bob's requests");
    assert!(requests.incoming.iter().any(|r| r.user_id == alice.id));
    let requests = client_a.friend_requests().await.expect("alice's requests");
    assert!(requests.outgoing.iter().any(|r| r.user_id == bob.id));
    client_b
        .accept_friend_request(alice.id)
        .await
        .expect("accept");
    let friends_a = client_a.friends().await.expect("alice friends");
    assert!(friends_a.iter().any(|f| f.user_id == bob.id));
    let friends_b = client_b.friends().await.expect("bob friends");
    assert!(friends_b.iter().any(|f| f.user_id == alice.id));

    // Unfriending severs an existing share.
    let area = server.create_area(&alice, "Lent Area");
    client_a
        .create_share(view_only_share(bob.id, area))
        .await
        .expect("share to a friend");
    let rows = client_b.sync().await.expect("bob /sync");
    assert!(
        rows.iter().any(|r| r.area_id == area),
        "the grant shows up in the grantee's sync rows"
    );

    client_a.unfriend(bob.id).await.expect("unfriend");
    assert!(client_a.friends().await.expect("friends").is_empty());
    let rows = client_b.sync().await.expect("bob /sync after unfriend");
    assert!(
        !rows.iter().any(|r| r.area_id == area),
        "unfriending severs the share"
    );

    // Blocking is silent: the blocked sender still gets Ok, the target's
    // incoming list stays empty.
    client_b.block(alice.id).await.expect("bob blocks alice");
    let blocks = client_b.blocks().await.expect("bob's blocks");
    assert!(blocks.iter().any(|b| b.user_id == alice.id));
    client_a
        .send_friend_request(bob.id)
        .await
        .expect("blocked sender still receives Ok (enumeration resistance)");
    let requests = client_b.friend_requests().await.expect("bob requests");
    assert!(
        requests.incoming.is_empty(),
        "the shadow-pending never reaches the blocking target"
    );
}

// ---------------------------------------------------------------------------
// 3. Share validator: every denial is the uniform 404
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shares_validator_uniform_404() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let stranger = server.create_user("stranger@example.com", "stranger", true);
    let friend = server.create_user("friend@example.com", "friend", true);
    let third = server.create_user("third@example.com", "third", true);
    server.befriend(&owner, &friend);
    server.befriend(&friend, &third);

    let area = server.create_area(&owner, "Guarded Area");
    let owner_client = api_client(&server.base_url, &owner.api_key);

    // Share to a non-friend.
    let err = owner_client
        .create_share(view_only_share(stranger.id, area))
        .await
        .expect_err("non-friend grantee");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));

    // Share to self.
    let err = owner_client
        .create_share(view_only_share(owner.id, area))
        .await
        .expect_err("self-share");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));

    // Re-share attempt without can_reshare.
    server.grant(&owner, &friend, GrantScope::Area(area), GrantFlags::VIEW_ONLY);
    let friend_client = api_client(&server.base_url, &friend.api_key);
    let err = friend_client
        .create_share(view_only_share(third.id, area))
        .await
        .expect_err("view-only grantee cannot re-share");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));

    // include_secrets MAY ride an atlas-scope root grant (not just an area-scope grant).
    let atlas = server.create_atlas(&owner, "Bundle");
    let grant = owner_client
        .create_share(CreateShareRequest {
            grantee_id: friend.id,
            scope: ShareScope::Atlas {
                atlas_id: AtlasId(atlas),
            },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            include_secrets: true,
            can_admin: false,
            host_hints: None,
        })
        .await
        .expect("include_secrets may ride an atlas-scope root grant (M6)");
    assert!(grant.area_id.is_none(), "atlas-scope grant");
    assert!(grant.include_secrets, "the atlas grant carries include_secrets");
}

/// §4.2: `host_hints` ride the `create_share` body, echo on the created grant,
/// and surface on the grantee's received rows. A hint-less re-grant clears the
/// snapshot (upsert-replace); an over-cap list is the uniform 400.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_host_hints_roundtrip() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Cities");
    let owner_client = api_client(&server.base_url, &owner.api_key);
    let grantee_client = api_client(&server.base_url, &grantee.api_key);

    let hints = || vec!["arctic.org".to_string(), "localhost:4000".to_string()];

    // With hints: the created grant echoes them and they reach the grantee's row.
    let mut req = view_only_share(grantee.id, area);
    req.host_hints = Some(hints());
    let created = owner_client
        .create_share(req)
        .await
        .expect("share with host hints");
    assert_eq!(created.host_hints.as_deref(), Some(&hints()[..]));

    let received = grantee_client
        .shares(ShareDirection::Received)
        .await
        .expect("grantee received rows");
    let row = received
        .iter()
        .find(|r| r.grant.area_id == Some(area))
        .expect("the shared row is received");
    assert_eq!(row.grant.host_hints.as_deref(), Some(&hints()[..]));

    // A hint-less re-grant is a fresh consent moment and CLEARS the snapshot.
    let plain = owner_client
        .create_share(view_only_share(grantee.id, area))
        .await
        .expect("hint-less re-grant");
    assert!(plain.host_hints.is_none());
    let received = grantee_client
        .shares(ShareDirection::Received)
        .await
        .expect("grantee received rows after re-grant");
    let row = received
        .iter()
        .find(|r| r.grant.area_id == Some(area))
        .expect("the shared row is still received");
    assert!(
        row.grant.host_hints.is_none(),
        "the hint-less re-grant cleared host_hints"
    );

    // Over-cap (33 hints) is rejected uniformly as a 400.
    let mut too_many = view_only_share(grantee.id, area);
    too_many.host_hints = Some((0..33).map(|i| format!("h{i}.example")).collect());
    let err = owner_client
        .create_share(too_many)
        .await
        .expect_err("33 host hints exceed the cap");
    assert!(
        matches!(&err, CloudError::NetworkError(m) if m.contains("400")),
        "over-cap host_hints surfaces as a 400: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Secret marks, the audit list, and previews
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_marks_and_audit() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    let area = server.create_area(&owner, "Audited Area");
    server.add_room(area, 1, "Lobby", false);
    server.add_room(area, 2, "Vault", false);
    let exit = server.add_exit(area, 1, "East", Some((area, 2)), false);
    let label = server.add_label(area, "Watch out", false);
    let shape = server.add_shape(area, false);
    server.set_area_property(area, "notes", "owner notes", false);
    server.set_room_property(area, 1, "loot", "diamonds", false);

    let grant_id = server.grant(&owner, &grantee, GrantScope::Area(area), GrantFlags::VIEW_ONLY);

    // Bulk-mark everything; bogus ids are silently ignored by the server.
    let owner_client = api_client(&server.base_url, &owner.api_key);
    let result = owner_client
        .secret_marks(
            area,
            &SecretMarksRequest {
                secret: true,
                rooms: vec![2, 99], // 99 does not exist
                exits: vec![ExitId(exit), ExitId(Uuid::new_v4())],
                labels: vec![LabelId(label)],
                shapes: vec![ShapeId(shape)],
                room_properties: vec![RoomPropertyRef {
                    room_number: 1,
                    name: "loot".to_string(),
                }],
                area_properties: vec!["notes".to_string()],
            },
        )
        .await
        .expect("owner is cleared for secret marks");
    assert_eq!(result.rooms, 1, "the bogus room number was ignored");
    assert_eq!(result.exits, 1, "the bogus exit id was ignored");
    assert_eq!(result.labels, 1);
    assert_eq!(result.shapes, 1);
    assert_eq!(result.room_properties, 1);
    assert_eq!(result.area_properties, 1);

    // The owner audit list carries one row per marked entity.
    let secrets = owner_client.area_secrets(area).await.expect("audit list");
    assert_eq!(secrets.len(), 6);
    assert!(
        secrets
            .iter()
            .any(|s| s.kind == SecretEntityKind::Room && s.room_number == Some(2))
    );
    assert!(
        secrets
            .iter()
            .any(|s| s.kind == SecretEntityKind::Exit && s.id == Some(exit))
    );
    assert!(
        secrets
            .iter()
            .any(|s| s.kind == SecretEntityKind::Label && s.id == Some(label))
    );
    assert!(
        secrets
            .iter()
            .any(|s| s.kind == SecretEntityKind::Shape && s.id == Some(shape))
    );
    assert!(secrets.iter().any(|s| {
        s.kind == SecretEntityKind::RoomProperty
            && s.room_number == Some(1)
            && s.name.as_deref() == Some("loot")
    }));
    assert!(
        secrets
            .iter()
            .any(|s| s.kind == SecretEntityKind::AreaProperty && s.name.as_deref() == Some("notes"))
    );

    // Worst case: the anonymous audience sees nothing at all (data: null).
    let worst = owner_client
        .preview(area, PreviewAudience::WorstCase)
        .await
        .expect("worst-case preview");
    assert!(worst.is_none(), "no grant reaches an anonymous viewer");

    // Share-grant preview hides the secrets...
    let preview = owner_client
        .preview(area, PreviewAudience::Share(grant_id))
        .await
        .expect("share preview")
        .expect("the grantee audience sees the area");
    let room_numbers: Vec<i32> = preview.rooms.iter().map(|r| r.room_number.0).collect();
    assert_eq!(room_numbers, vec![1], "secret room hidden in the preview");
    assert!(
        preview.rooms[0].exits.is_empty(),
        "the secret exit (into the secret room) is hidden"
    );
    assert!(preview.properties.is_empty(), "secret area property hidden");
    assert!(preview.rooms[0].properties.is_empty(), "secret room property hidden");
    assert!(preview.labels.is_empty(), "secret label hidden");
    assert!(preview.shapes.is_empty(), "secret shape hidden");

    // ...and matches the grantee's actual projection byte-for-byte.
    let grantee_mapper = CloudMapper::new(server.base_url.clone(), grantee.api_key.clone());
    let grantee_view = grantee_mapper.get_area(&area).await.expect("grantee view");
    assert!(preview.content_hash.is_some());
    assert_eq!(
        preview.content_hash, grantee_view.content_hash,
        "preview(Share) is the grantee's projection (same viewer salt)"
    );
    assert_eq!(grantee_view.rooms.len(), 1);
}

// ---------------------------------------------------------------------------
// 5. Clone flows: area copy with provenance, atlas copy with a copy split
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clone_flow() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let grantee = server.create_user("friend@example.com", "friend", true);
    server.befriend(&owner, &grantee);

    // Source area with cross-area links into a shared and an unshared area.
    let source = server.create_area(&owner, "Original");
    let shared_target = server.create_area(&owner, "Neighbour");
    let hidden_target = server.create_area(&owner, "Private");
    server.add_room(source, 1, "Hall", false);
    server.add_room(shared_target, 1, "Annex", false);
    server.add_room(hidden_target, 1, "Sanctum", false);
    server.add_exit(source, 1, "North", Some((shared_target, 1)), false);
    server.add_exit(source, 1, "South", Some((hidden_target, 1)), false);

    server.grant(
        &owner,
        &grantee,
        GrantScope::Area(source),
        GrantFlags {
            can_copy: true,
            ..GrantFlags::VIEW_ONLY
        },
    );
    server.grant(
        &owner,
        &grantee,
        GrantScope::Area(shared_target),
        GrantFlags::VIEW_ONLY,
    );

    let grantee_client = api_client(&server.base_url, &grantee.api_key);
    let grantee_mapper = CloudMapper::new(server.base_url.clone(), grantee.api_key.clone());
    let owner_mapper = CloudMapper::new(server.base_url.clone(), owner.api_key.clone());

    let source_before = owner_mapper.get_area(&source).await.expect("source before");

    let cloned = grantee_client
        .copy_area(source, &CopyAreaRequest::default())
        .await
        .expect("can_copy grantee clones the area");
    assert_eq!(cloned.user_id, Some(grantee.id), "the caller owns the clone");
    assert_eq!(cloned.name, "Original (copy)");
    assert_eq!(cloned.copied_from_area_id, Some(source), "provenance source");
    assert!(cloned.copied_from_rev.is_some(), "provenance rev");
    assert!(cloned.copied_at.is_some(), "provenance timestamp");

    // Access is OWNER in the /areas listing.
    let listed = grantee_mapper.list_areas().await.expect("grantee list");
    let row = listed.iter().find(|a| a.id == cloned.id).expect("clone listed");
    let access = row.access.expect("access block on the clone");
    assert!(access.is_owner && access.can_edit && access.include_secrets);

    // The source is untouched by the clone.
    let source_after = owner_mapper.get_area(&source).await.expect("source after");
    assert_eq!(source_after.area.rev, source_before.area.rev);
    assert_eq!(source_after.rooms.len(), source_before.rooms.len());

    // Cross-area links: still-shared target stays real; unshared one dangles.
    let clone_details = grantee_mapper.get_area(&cloned.id).await.expect("clone details");
    let room = clone_details
        .rooms
        .iter()
        .find(|r| r.room_number == RoomNumber(1))
        .expect("room 1 copied");
    assert_eq!(room.exits.len(), 2);
    let real = room
        .exits
        .iter()
        .find(|e| e.to_area_id == Some(shared_target))
        .expect("link into the still-shared area stays real");
    assert!(!real.to_unknown);
    assert_eq!(real.to_room_number, Some(RoomNumber(1)));
    let dangling = room
        .exits
        .iter()
        .find(|e| e.to_area_id.is_none())
        .expect("link into the unshared area dangles");
    assert!(
        !dangling.to_unknown,
        "dangling, not tokenized — the hidden id never entered the clone"
    );
    assert!(dangling.to_room_number.is_none());

    // ---- Atlas copy with a per-member can_copy split ----
    let atlas = server.create_atlas(&owner, "Bundle");
    let member_one = owner_mapper
        .create_area(CreateAreaRequest {
            name: "Member One".to_string(),
            atlas_id: Some(AtlasId(atlas)),
            ephemeral: false,
        })
        .await
        .expect("member one");
    let member_two = owner_mapper
        .create_area(CreateAreaRequest {
            name: "Member Two".to_string(),
            atlas_id: Some(AtlasId(atlas)),
            ephemeral: false,
        })
        .await
        .expect("member two");
    // Atlas-scope view covers both members; copy is added on member one only.
    server.grant(&owner, &grantee, GrantScope::Atlas(atlas), GrantFlags::VIEW_ONLY);
    server.grant(
        &owner,
        &grantee,
        GrantScope::Area(member_one.id),
        GrantFlags {
            can_copy: true,
            ..GrantFlags::VIEW_ONLY
        },
    );

    let report = grantee_client
        .copy_atlas(AtlasId(atlas), Some("Bundle Copy".to_string()))
        .await
        .expect("atlas copy");
    assert_eq!(report.name, "Bundle Copy");
    assert_eq!(report.copied.len(), 1, "only the copyable member lands");
    assert_ne!(
        report.copied[0], member_one.id,
        "copied ids are the new clones, not the sources"
    );
    assert_eq!(
        report.skipped,
        vec![member_two.id],
        "viewable-but-not-copyable member is reported as skipped"
    );

    // The copied member is owned by the grantee.
    let listed = grantee_mapper.list_areas().await.expect("grantee list again");
    let copied_row = listed
        .iter()
        .find(|a| a.id == report.copied[0])
        .expect("atlas-copied area listed");
    assert!(copied_row.access.expect("access").is_owner);
}

// ---------------------------------------------------------------------------
// Ownership transfer (offer -> accept, auto-admin, gates)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfer_accept_rehomes_and_auto_admins() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let recip = server.create_user("recip@example.com", "recip", true);
    server.befriend(&owner, &recip);
    let area = server.create_area(&owner, "Heirloom");

    let owner_client = api_client(&server.base_url, &owner.api_key);
    let offer = owner_client
        .offer_area_transfer(area, recip.id)
        .await
        .expect("owner may offer");

    let recip_client = api_client(&server.base_url, &recip.api_key);
    recip_client
        .accept_transfer(offer.id, None, None)
        .await
        .expect("recipient accepts");

    let st = server.state.lock();
    assert_eq!(st.areas.get(&area.0).unwrap().user_id, recip.id, "re-homed");
    assert!(
        st.grants.iter().any(|g| g.owner_id == recip.id
            && g.grantor_id == recip.id
            && g.grantee_id == owner.id
            && g.area_id == Some(area.0)
            && g.can_admin),
        "former owner auto-retained as can_admin"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfer_offer_is_raw_owner_only() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let admin = server.create_user("admin@example.com", "admin", true);
    let recip = server.create_user("recip@example.com", "recip", true);
    server.befriend(&owner, &admin);
    server.befriend(&admin, &recip);
    let area = server.create_area(&owner, "Castle");
    // Make `admin` a full deputy on the area.
    server.grant(&owner, &admin, GrantScope::Area(area), GrantFlags::admin());

    let admin_client = api_client(&server.base_url, &admin.api_key);
    let err = admin_client
        .offer_area_transfer(area, recip.id)
        .await
        .expect_err("an effective admin cannot transfer ownership");
    assert!(matches!(err, CloudError::NotFoundOrNoAccess));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfer_decline_and_conflict() {
    let server = MockServer::spawn().await;
    let owner = server.create_user("owner@example.com", "owner", true);
    let recip = server.create_user("recip@example.com", "recip", true);
    server.befriend(&owner, &recip);
    let area = server.create_area(&owner, "Workshop");
    let owner_client = api_client(&server.base_url, &owner.api_key);
    let recip_client = api_client(&server.base_url, &recip.api_key);

    let offer = owner_client.offer_area_transfer(area, recip.id).await.unwrap();

    // A second live offer for the same subject conflicts.
    assert!(
        owner_client.offer_area_transfer(area, recip.id).await.is_err(),
        "one live offer per subject"
    );

    // Recipient declines -> the offer is no longer acceptable; ownership unchanged.
    recip_client.decline_transfer(offer.id).await.expect("decline ok");
    assert!(
        recip_client.accept_transfer(offer.id, None, None).await.is_err(),
        "a declined offer cannot be accepted"
    );
    assert_eq!(
        server.state.lock().areas.get(&area.0).unwrap().user_id,
        owner.id,
        "ownership unchanged after decline"
    );
}
