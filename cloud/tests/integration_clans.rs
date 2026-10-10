//! Clans end to end over real HTTP against the contract-shaped mock in
//! `tests/support/clans.rs`: clans and their founding grants, invitations,
//! members and owners, groups created and joined by their creators, and the
//! server's refusals.
#![allow(clippy::too_many_lines)]

mod support;

use std::collections::BTreeSet;

use smudgy_cloud::clans::{ClanGrantFilter, ClanGroupPatch, action};
use smudgy_cloud::{CloudApiClient, CloudError, Credential, CredentialSource};
use support::clans::{ClanGrantScope, ClanRecipient};
use support::{MockHandle, MockServer, TestUser};
use uuid::Uuid;

fn client(server: &MockHandle, user: &TestUser) -> CloudApiClient {
    CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    )
}

fn user(server: &MockHandle, nickname: &str) -> (TestUser, CloudApiClient) {
    let user = server.create_user(&format!("{nickname}@example.com"), nickname, true);
    let client = client(server, &user);
    (user, client)
}

/// `owner` invites `invitee` by nickname, and `invitee` accepts.
async fn invite_and_join(
    owner: &CloudApiClient,
    invitee: &CloudApiClient,
    clan: Uuid,
    nickname: &str,
) {
    let found = owner.lookup(nickname).await.expect("lookup");
    let invitation = owner
        .invite_to_clan(clan, found.user_id, &[])
        .await
        .expect("invite");
    invitee
        .accept_clan_invitation(invitation.id)
        .await
        .expect("accept");
}

fn is_not_found<T: std::fmt::Debug>(result: &Result<T, CloudError>) -> bool {
    matches!(result, Err(CloudError::NotFoundOrNoAccess))
}

/// What every member holds on a new clan: its founding grants to All clan
/// members, reading the directory.
fn founding() -> BTreeSet<String> {
    BTreeSet::from([action::READ_MEMBERS.to_string()])
}

/// `actions` with the founding grants' added.
fn with_founding(actions: &[&str]) -> BTreeSet<String> {
    let mut all = founding();
    all.extend(actions.iter().map(ToString::to_string));
    all
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clans_need_a_verified_email() {
    let server = MockServer::spawn().await;
    let unverified = server.create_user("new@example.com", "newbie", false);
    let error = client(&server, &unverified)
        .clans()
        .await
        .expect_err("unverified");
    assert!(matches!(error, CloudError::EmailNotVerified), "{error:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clan_is_created_listed_renamed_and_dissolved() {
    let server = MockServer::spawn().await;
    let (_, mira) = user(&server, "mira");
    let (_, tomas) = user(&server, "tomas");

    assert!(mira.clans().await.unwrap().clans.is_empty());
    let clan = mira.create_clan("Lantern Company").await.expect("create");
    assert!(clan.is_owner);
    assert_eq!(clan.member_count, 1);
    assert_eq!(clan.group_ids.len(), 2, "the creator is in both built-ins");
    for held in [
        action::EDIT_PROFILE,
        action::READ_MEMBERS,
        action::INVITE,
        action::REMOVE_MEMBER,
        action::CREATE_GROUP,
    ] {
        assert!(clan.can(held), "an owner holds {held}");
    }

    // A blank or overlong name is a 400.
    for bad in [String::from("   "), "x".repeat(65)] {
        let error = mira.create_clan(&bad).await.expect_err("bad name");
        assert!(matches!(error, CloudError::InvalidInput(_)), "{error:?}");
    }

    // Listed by name; there is no directory, so others never see it.
    mira.create_clan("Archivists").await.expect("second clan");
    let listed = mira.clans().await.unwrap();
    let names: Vec<&str> = listed.clans.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Archivists", "Lantern Company"]);
    assert!(tomas.clans().await.unwrap().clans.is_empty());
    assert!(is_not_found(&tomas.clan(clan.id).await));
    assert!(is_not_found(&tomas.rename_clan(clan.id, "Mine").await));
    assert!(is_not_found(&tomas.delete_clan(clan.id).await));

    let renamed = mira
        .rename_clan(clan.id, "Lantern Co.")
        .await
        .expect("rename");
    assert_eq!(renamed.name, "Lantern Co.");
    assert_eq!(mira.clan(clan.id).await.unwrap().name, "Lantern Co.");

    // A clan that owns maps cannot be dissolved.
    server.set_clan_owned_maps(clan.id, 1);
    let error = mira.delete_clan(clan.id).await.expect_err("owns maps");
    assert!(matches!(error, CloudError::ClanNotEmpty), "{error:?}");
    server.set_clan_owned_maps(clan.id, 0);
    mira.delete_clan(clan.id).await.expect("dissolve");
    assert!(is_not_found(&mira.clan(clan.id).await));
    let names: Vec<String> = mira
        .clans()
        .await
        .unwrap()
        .clans
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["Archivists"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_repeating_a_dissolution_whose_answer_was_lost_gets_null() {
    let server = MockServer::spawn().await;
    let (_, mira) = user(&server, "mira");
    let (_, tomas) = user(&server, "tomas");
    let clan = mira.create_clan("Lantern Company").await.unwrap();
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;

    server.lose_next_dissolution_answers(1);
    assert!(
        mira.delete_clan(clan.id).await.is_err(),
        "the answer is lost"
    );
    // Until the directory marks it dissolved, an owner's repeat gets null
    // and anyone else the 404.
    assert!(is_not_found(&tomas.delete_clan(clan.id).await));
    mira.delete_clan(clan.id)
        .await
        .expect("an owner's repeat finishes it");
    assert!(is_not_found(&mira.delete_clan(clan.id).await));
    assert!(is_not_found(&mira.clan(clan.id).await));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_are_sent_answered_and_revoked() {
    let server = MockServer::spawn().await;
    let (mira_user, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let (arun_user, arun) = user(&server, "arun");
    let clan = mira.create_clan("Lantern Company").await.unwrap();

    // Invite by nickname: the invitee sees it in both listings.
    let found = mira.lookup("tomas").await.expect("lookup");
    assert_eq!(found.user_id, tomas_user.id);
    let invitation = mira
        .invite_to_clan(clan.id, found.user_id, &[])
        .await
        .expect("invite");
    assert_eq!(invitation.user_id, tomas_user.id);
    assert_eq!(invitation.nickname.as_deref(), Some("tomas"));
    assert_eq!(invitation.inviter_id, mira_user.id);
    let received = tomas.clans().await.unwrap().invitations;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].clan_name, "Lantern Company");
    assert_eq!(received[0].inviter_nickname.as_deref(), Some("mira"));
    assert_eq!(tomas.my_clan_invitations().await.unwrap(), received);

    // Inviting again replaces the pending one instead of adding another.
    let again = mira
        .invite_to_clan(clan.id, tomas_user.id, &[])
        .await
        .expect("invite again");
    assert_eq!(again.id, invitation.id);
    assert_eq!(mira.clan_invitations(clan.id).await.unwrap().len(), 1);

    // Only the invitee answers.
    assert!(is_not_found(
        &arun.accept_clan_invitation(invitation.id).await
    ));
    assert!(is_not_found(
        &arun.decline_clan_invitation(invitation.id).await
    ));

    // Declining drops it.
    tomas
        .decline_clan_invitation(invitation.id)
        .await
        .expect("decline");
    assert!(tomas.my_clan_invitations().await.unwrap().is_empty());
    assert!(mira.clan_invitations(clan.id).await.unwrap().is_empty());

    // Accepting joins the clan.
    let invitation = mira
        .invite_to_clan(clan.id, tomas_user.id, &[])
        .await
        .unwrap();
    let joined = tomas
        .accept_clan_invitation(invitation.id)
        .await
        .expect("accept");
    assert_eq!(joined.id, clan.id);
    assert!(!joined.is_owner);
    assert_eq!(joined.member_count, 2);
    assert_eq!(
        joined.actions,
        founding(),
        "a plain member holds what the founding grants give"
    );
    assert!(mira.clan_invitations(clan.id).await.unwrap().is_empty());
    assert!(is_not_found(
        &tomas.accept_clan_invitation(invitation.id).await
    ));

    // A member can't be invited again.
    let error = mira
        .invite_to_clan(clan.id, tomas_user.id, &[])
        .await
        .expect_err("already a member");
    assert!(matches!(error, CloudError::AlreadyMember), "{error:?}");

    // A plain member may not invite.
    assert!(is_not_found(
        &tomas.invite_to_clan(clan.id, arun_user.id, &[]).await
    ));
    assert!(is_not_found(&tomas.clan_invitations(clan.id).await));

    // The inviter revokes; nobody else without the action can.
    let invitation = mira
        .invite_to_clan(clan.id, arun_user.id, &[])
        .await
        .unwrap();
    assert!(is_not_found(
        &tomas.revoke_clan_invitation(invitation.id).await
    ));
    mira.revoke_clan_invitation(invitation.id)
        .await
        .expect("revoke");
    assert!(arun.my_clan_invitations().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_block_hides_the_invitee_behind_the_uniform_404() {
    let server = MockServer::spawn().await;
    let (mira_user, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let clan = mira.create_clan("Lantern Company").await.unwrap();

    server.block(&tomas_user, &mira_user);
    assert!(is_not_found(
        &mira.invite_to_clan(clan.id, tomas_user.id, &[]).await
    ));
    assert!(tomas.my_clan_invitations().await.unwrap().is_empty());
    // An unknown user is the same answer.
    assert!(is_not_found(
        &mira.invite_to_clan(clan.id, Uuid::new_v4(), &[]).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invitation_is_void_once_its_inviter_may_no_longer_invite() {
    let server = MockServer::spawn().await;
    let (_, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let (arun_user, arun) = user(&server, "arun");
    let clan = mira.create_clan("Lantern Company").await.unwrap();
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;
    let grant = server.clan_grant(
        clan.id,
        ClanRecipient::User(tomas_user.id),
        ClanGrantScope::Clan,
        &[action::INVITE],
    );
    assert!(tomas.clan(clan.id).await.unwrap().can(action::INVITE));

    let invitation = tomas
        .invite_to_clan(clan.id, arun_user.id, &[])
        .await
        .expect("a member holding clan.invite invites");
    assert_eq!(arun.my_clan_invitations().await.unwrap().len(), 1);

    // Taking the action away voids the invitation: unlisted, and refused.
    server
        .state
        .lock()
        .clans
        .clans
        .get_mut(&clan.id)
        .unwrap()
        .grants
        .retain(|g| g.id != grant);
    assert!(arun.my_clan_invitations().await.unwrap().is_empty());
    assert!(is_not_found(
        &arun.accept_clan_invitation(invitation.id).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owners_change_hands_and_the_last_owner_stays() {
    let server = MockServer::spawn().await;
    let (mira_user, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let clan = mira.create_clan("Lantern Company").await.unwrap();
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;

    // The last owner can neither give up ownership nor leave.
    let error = mira
        .set_clan_owner(clan.id, mira_user.id, false)
        .await
        .expect_err("last owner");
    assert!(matches!(error, CloudError::LastOwner), "{error:?}");
    let error = mira
        .remove_clan_member(clan.id, mira_user.id)
        .await
        .expect_err("last owner leaves");
    assert!(matches!(error, CloudError::LastOwner), "{error:?}");

    // Only owners appoint owners.
    assert!(is_not_found(
        &tomas.set_clan_owner(clan.id, tomas_user.id, true).await
    ));

    let member = mira
        .set_clan_owner(clan.id, tomas_user.id, true)
        .await
        .expect("appoint");
    assert!(member.is_owner);
    assert_eq!(member.nickname.as_deref(), Some("tomas"));
    // Idempotent.
    mira.set_clan_owner(clan.id, tomas_user.id, true)
        .await
        .expect("appoint again");
    let members = mira.clan_members(clan.id).await.unwrap();
    assert!(members.iter().all(|member| member.is_owner));
    assert!(tomas.clan(clan.id).await.unwrap().is_owner);

    // With another owner, mira may step down and then leave.
    mira.set_clan_owner(clan.id, mira_user.id, false)
        .await
        .expect("step down");
    assert!(!mira.clan(clan.id).await.unwrap().is_owner);
    mira.remove_clan_member(clan.id, mira_user.id)
        .await
        .expect("leave");
    assert!(is_not_found(&mira.clan(clan.id).await));
    assert_eq!(tomas.clan(clan.id).await.unwrap().member_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_are_listed_owners_first_and_removed_by_the_rules() {
    let server = MockServer::spawn().await;
    let (mira_user, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let (arun_user, arun) = user(&server, "arun");
    let clan = mira.create_clan("Lantern Company").await.unwrap();
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;
    invite_and_join(&mira, &arun, clan.id, "arun").await;
    mira.set_clan_owner(clan.id, arun_user.id, true)
        .await
        .unwrap();

    let order: Vec<Uuid> = mira
        .clan_members(clan.id)
        .await
        .unwrap()
        .into_iter()
        .map(|member| member.user_id)
        .collect();
    assert_eq!(order, [mira_user.id, arun_user.id, tomas_user.id]);

    // The directory needs clan.read_members, which All clan members hold
    // until the clan narrows its founding grant.
    assert_eq!(tomas.clan_members(clan.id).await.unwrap().len(), 3);
    let everyone = server.clan_builtin_group(clan.id, "members");
    let founding_grant = mira
        .clan_grants(
            clan.id,
            ClanGrantFilter {
                group_id: Some(everyone),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .find(|grant| grant.actions.contains(action::READ_MEMBERS))
        .expect("the founding grant");
    mira.delete_clan_grant(clan.id, founding_grant.id)
        .await
        .expect("an owner narrows it");
    assert!(is_not_found(&tomas.clan_members(clan.id).await));
    assert!(!tomas.clan(clan.id).await.unwrap().can(action::READ_MEMBERS));
    assert_eq!(
        mira.clan_members(clan.id).await.unwrap().len(),
        3,
        "owners always read it"
    );
    server.clan_grant(
        clan.id,
        ClanRecipient::User(tomas_user.id),
        ClanGrantScope::Clan,
        &[action::READ_MEMBERS, action::REMOVE_MEMBER],
    );
    assert_eq!(tomas.clan_members(clan.id).await.unwrap().len(), 3);

    // clan.remove_member removes members, but never an owner.
    assert!(is_not_found(
        &tomas.remove_clan_member(clan.id, arun_user.id).await
    ));
    let (zoe_user, zoe) = user(&server, "zoe");
    invite_and_join(&mira, &zoe, clan.id, "zoe").await;
    tomas
        .remove_clan_member(clan.id, zoe_user.id)
        .await
        .expect("remove a member");
    assert!(is_not_found(&zoe.clan(clan.id).await));

    // An owner removes an owner.
    mira.remove_clan_member(clan.id, arun_user.id)
        .await
        .expect("remove an owner");
    assert_eq!(mira.clan(clan.id).await.unwrap().member_count, 2);

    // A former member can be invited again.
    invite_and_join(&mira, &zoe, clan.id, "zoe").await;
    assert_eq!(mira.clan(clan.id).await.unwrap().member_count, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaving_drops_custom_groups_direct_grants_and_issued_invitations() {
    let server = MockServer::spawn().await;
    let (_, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let (arun_user, arun) = user(&server, "arun");
    let clan = mira.create_clan("Lantern Company").await.unwrap();
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;
    let scouts = mira
        .create_clan_group(clan.id, "Scouts", Some("#336699"))
        .await
        .unwrap();
    mira.add_clan_group_member(clan.id, scouts.id, tomas_user.id)
        .await
        .unwrap();
    let group_grant = server.clan_grant(
        clan.id,
        ClanRecipient::Group(scouts.id),
        ClanGrantScope::Clan,
        &[action::READ_MEMBERS],
    );
    server.clan_grant(
        clan.id,
        ClanRecipient::User(tomas_user.id),
        ClanGrantScope::Clan,
        &[action::INVITE],
    );
    tomas
        .invite_to_clan(clan.id, arun_user.id, &[])
        .await
        .expect("tomas invites");

    tomas
        .remove_clan_member(clan.id, tomas_user.id)
        .await
        .expect("leave");

    let everyone = server.clan_builtin_group(clan.id, "members");
    let grants: Vec<_> = server
        .clan_grants(clan.id)
        .into_iter()
        .filter(|(_, recipient)| *recipient != ClanRecipient::Group(everyone))
        .collect();
    assert_eq!(
        grants,
        [(group_grant, ClanRecipient::Group(scouts.id))],
        "the direct grant went; the group's stays"
    );
    let roster = mira.clan_group_members(clan.id, scouts.id).await.unwrap();
    assert_eq!(roster.len(), 1, "only its creator is left in it");
    assert!(arun.my_clan_invitations().await.unwrap().is_empty());
    assert!(mira.clan_invitations(clan.id).await.unwrap().is_empty());

    // Rejoining starts over: no groups, no grants.
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;
    let rejoined = tomas.clan(clan.id).await.unwrap();
    assert!(!rejoined.group_ids.contains(&scouts.id));
    assert_eq!(rejoined.actions, founding());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groups_are_created_renamed_filled_and_deleted() {
    let server = MockServer::spawn().await;
    let (_, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let clan = mira.create_clan("Lantern Company").await.unwrap();
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;

    let groups = mira.clan_groups(clan.id).await.unwrap();
    let builtins: Vec<Option<&str>> = groups.iter().map(|g| g.builtin.as_deref()).collect();
    assert_eq!(builtins, [Some("owners"), Some("members")]);

    let mappers = mira
        .create_clan_group(clan.id, "City mappers", Some("#A1B2C3"))
        .await
        .expect("create");
    assert_eq!(
        mappers.color.as_deref(),
        Some("#a1b2c3"),
        "stored lowercase"
    );
    assert!(!mappers.is_builtin());
    assert!(mappers.is_member, "its creator joins it");
    assert!(mappers.created_by_me, "and governs it");
    assert!(mappers.can(action::ASSIGN_GROUP));
    let scouts = mira
        .create_clan_group(clan.id, "scouts", None)
        .await
        .unwrap();
    assert_eq!(scouts.color, None);

    // Names are unique ignoring case, built-ins included.
    for taken in ["city MAPPERS", "owner", "All Clan Members"] {
        let error = mira
            .create_clan_group(clan.id, taken, None)
            .await
            .expect_err("name in use");
        assert!(matches!(error, CloudError::NameInUse), "{taken}: {error:?}");
    }
    let error = mira
        .create_clan_group(clan.id, "Bad", Some("teal"))
        .await
        .expect_err("color");
    assert!(matches!(error, CloudError::InvalidInput(_)), "{error:?}");

    // Built-ins first, then by name ignoring case.
    let names: Vec<String> = mira
        .clan_groups(clan.id)
        .await
        .unwrap()
        .into_iter()
        .map(|group| group.name)
        .collect();
    assert_eq!(
        names,
        ["Owner", "All clan members", "City mappers", "scouts"]
    );

    // Rename: another group's name is taken; a case change of its own isn't.
    let error = mira
        .update_clan_group(
            clan.id,
            scouts.id,
            &ClanGroupPatch {
                name: Some("City Mappers".to_string()),
                color: None,
            },
        )
        .await
        .expect_err("rename onto another");
    assert!(matches!(error, CloudError::NameInUse), "{error:?}");
    let renamed = mira
        .update_clan_group(
            clan.id,
            scouts.id,
            &ClanGroupPatch {
                name: Some("Scouts".to_string()),
                color: Some(Some("#112233".to_string())),
            },
        )
        .await
        .expect("rename");
    assert_eq!(renamed.name, "Scouts");
    assert_eq!(renamed.color.as_deref(), Some("#112233"));
    assert!(renamed.created_by_me, "a rename answers created_by_me too");
    let owners_group = server.clan_builtin_group(clan.id, "owners");
    let error = mira
        .update_clan_group(
            clan.id,
            owners_group,
            &ClanGroupPatch {
                name: Some("Bosses".to_string()),
                color: None,
            },
        )
        .await
        .expect_err("built-in name");
    assert!(matches!(error, CloudError::InvalidInput(_)), "{error:?}");

    // Membership: idempotent adds and removes; members list their groups.
    mira.add_clan_group_member(clan.id, mappers.id, tomas_user.id)
        .await
        .expect("add");
    mira.add_clan_group_member(clan.id, mappers.id, tomas_user.id)
        .await
        .expect("add again");
    let roster = mira.clan_group_members(clan.id, mappers.id).await.unwrap();
    let names: Vec<Option<&str>> = roster.iter().map(|row| row.nickname.as_deref()).collect();
    assert_eq!(
        names,
        [Some("mira"), Some("tomas")],
        "in the order they joined"
    );
    let tomas_row = mira
        .clan_members(clan.id)
        .await
        .unwrap()
        .into_iter()
        .find(|member| member.user_id == tomas_user.id)
        .unwrap();
    assert_eq!(tomas_row.group_ids, [mappers.id]);
    assert!(
        tomas
            .clan(clan.id)
            .await
            .unwrap()
            .group_ids
            .contains(&mappers.id)
    );
    let error = mira
        .add_clan_group_member(clan.id, owners_group, tomas_user.id)
        .await
        .expect_err("built-in roster");
    assert!(matches!(error, CloudError::InvalidInput(_)), "{error:?}");
    assert!(is_not_found(
        &mira
            .add_clan_group_member(clan.id, mappers.id, Uuid::new_v4())
            .await
    ));
    mira.remove_clan_group_member(clan.id, mappers.id, tomas_user.id)
        .await
        .expect("remove");
    mira.remove_clan_group_member(clan.id, mappers.id, tomas_user.id)
        .await
        .expect("remove again");
    assert_eq!(
        mira.clan_group_members(clan.id, mappers.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // Plain members see the groups but hold nothing on them.
    let seen = tomas.clan_groups(clan.id).await.unwrap();
    assert_eq!(seen.len(), 4);
    assert!(seen.iter().all(|group| group.actions.is_empty()));
    assert!(seen.iter().all(|group| !group.created_by_me));
    let listed = mira.clan_groups(clan.id).await.unwrap();
    let created: Vec<bool> = listed.iter().map(|group| group.created_by_me).collect();
    assert_eq!(
        created,
        [false, false, true, true],
        "built-ins are nobody's; the creator's custom groups say so"
    );
    assert!(is_not_found(
        &tomas.create_clan_group(clan.id, "Mine", None).await
    ));
    assert!(is_not_found(
        &tomas.delete_clan_group(clan.id, mappers.id).await
    ));

    // Deleting takes the group's grants with it; built-ins stay.
    server.clan_grant(
        clan.id,
        ClanRecipient::Group(mappers.id),
        ClanGrantScope::Clan,
        &[action::READ_MEMBERS],
    );
    let before = server.clan_grants(clan.id).len();
    mira.delete_clan_group(clan.id, mappers.id)
        .await
        .expect("delete");
    assert_eq!(server.clan_grants(clan.id).len(), before - 1);
    let error = mira
        .delete_clan_group(clan.id, owners_group)
        .await
        .expect_err("built-in");
    assert!(matches!(error, CloudError::InvalidInput(_)), "{error:?}");
    assert!(is_not_found(
        &mira.delete_clan_group(clan.id, mappers.id).await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actions_come_from_grants_and_gate_each_route() {
    let server = MockServer::spawn().await;
    let (_, mira) = user(&server, "mira");
    let (tomas_user, tomas) = user(&server, "tomas");
    let clan = mira.create_clan("Lantern Company").await.unwrap();
    invite_and_join(&mira, &tomas, clan.id, "tomas").await;
    let mappers = mira
        .create_clan_group(clan.id, "City mappers", None)
        .await
        .unwrap();
    let scouts = mira
        .create_clan_group(clan.id, "Scouts", None)
        .await
        .unwrap();

    // A grant over one group gives group actions on that group only, and
    // nothing on the clan.
    server.clan_grant(
        clan.id,
        ClanRecipient::User(tomas_user.id),
        ClanGrantScope::Groups(BTreeSet::from([mappers.id])),
        &[action::ASSIGN_GROUP, action::RENAME_GROUP],
    );
    let summary = tomas.clan(clan.id).await.unwrap();
    assert_eq!(summary.actions, founding());
    let groups = tomas.clan_groups(clan.id).await.unwrap();
    let held = |id: Uuid| {
        groups
            .iter()
            .find(|group| group.id == id)
            .unwrap()
            .actions
            .clone()
    };
    assert_eq!(
        held(mappers.id),
        BTreeSet::from([
            action::RENAME_GROUP.to_string(),
            action::ASSIGN_GROUP.to_string()
        ])
    );
    assert!(held(scouts.id).is_empty());
    // group.assign adds others, never oneself to a group someone else made.
    assert!(is_not_found(
        &tomas
            .add_clan_group_member(clan.id, mappers.id, tomas_user.id)
            .await
    ));
    let (arun_user, arun) = user(&server, "arun");
    invite_and_join(&mira, &arun, clan.id, "arun").await;
    tomas
        .add_clan_group_member(clan.id, mappers.id, arun_user.id)
        .await
        .expect("assign another member");
    tomas
        .remove_clan_group_member(clan.id, mappers.id, arun_user.id)
        .await
        .expect("and take them out");
    assert!(is_not_found(
        &tomas
            .add_clan_group_member(clan.id, scouts.id, tomas_user.id)
            .await
    ));
    assert!(is_not_found(
        &tomas.clan_group_members(clan.id, mappers.id).await
    ));

    mira.add_clan_group_member(clan.id, mappers.id, tomas_user.id)
        .await
        .expect("an owner adds them");
    // A stale global grant contributes only administration, even when a
    // previous server stored resource actions in it.
    server.clan_grant(
        clan.id,
        ClanRecipient::Group(mappers.id),
        ClanGrantScope::Clan,
        &[action::READ_MEMBERS, action::CREATE_GROUP, "area.read"],
    );
    let summary = tomas.clan(clan.id).await.unwrap();
    assert_eq!(
        summary.actions,
        with_founding(&[action::READ_MEMBERS, action::CREATE_GROUP])
    );
    assert!(is_not_found(&tomas.rename_clan(clan.id, "Ours").await));

    // Stale global map and Secret actions give no access to a member either.
    server.clan_grant(
        clan.id,
        ClanRecipient::User(arun_user.id),
        ClanGrantScope::Clan,
        &["area.read", "secret.read", action::INVITE],
    );
    let summary = arun.clan(clan.id).await.unwrap();
    assert_eq!(summary.actions, with_founding(&[action::INVITE]));
}
