//! A clan's permissions end to end over real HTTP against the contract-shaped
//! mock (`tests/support/clans.rs`, `tests/support/clan_resources.rs`):
//! groups their creators join and govern, inline grants without roles,
//! delegation that never reaches clan administration, package scopes, the
//! access index, invitations that propose groups, and the clan's profile.
#![allow(clippy::too_many_lines)]

mod support;

use smudgy_cloud::clan_access::{ClanProfilePatch, GrantBody, GrantThrough, ResourceKind};
use smudgy_cloud::clan_secrets::{NewSecret, NewSecretOwner};
use smudgy_cloud::clans::{ClanGrantFilter, GrantRecipient, GrantScope, action};
use smudgy_cloud::{
    AtlasId, CloudApiClient, CloudError, CloudMapper, Credential, CredentialSource, MapperBackend,
};
use support::clans::{ClanGrantScope, ClanRecipient};
use support::{MockHandle, MockServer, TestUser};
use uuid::Uuid;

struct Member {
    user: TestUser,
    api: CloudApiClient,
}

fn member(server: &MockHandle, nickname: &str) -> Member {
    let user = server.create_user(&format!("{nickname}@example.com"), nickname, true);
    let api = CloudApiClient::new(
        server.base_url.clone(),
        CredentialSource::new(Some(Credential::Session(user.session_token.clone()))),
    );
    Member { user, api }
}

fn is_not_found<T: std::fmt::Debug>(result: &Result<T, CloudError>) -> bool {
    matches!(result, Err(CloudError::NotFoundOrNoAccess))
}

fn is_bad_request<T: std::fmt::Debug>(result: &Result<T, CloudError>) -> bool {
    matches!(result, Err(CloudError::InvalidInput(_)))
}

/// Mira's clan, with Nessa and Arun as plain members.
struct Clan {
    server: MockHandle,
    mira: Member,
    nessa: Member,
    arun: Member,
    id: Uuid,
    everyone: Uuid,
}

async fn clan() -> Clan {
    let server = MockServer::spawn().await;
    let mira = member(&server, "mira");
    let nessa = member(&server, "nessa");
    let arun = member(&server, "arun");
    let id = mira.api.create_clan("Lantern Company").await.unwrap().id;
    server.join_clan(id, &nessa.user);
    server.join_clan(id, &arun.user);
    let everyone = server.clan_builtin_group(id, "members");
    Clan {
        server,
        mira,
        nessa,
        arun,
        id,
        everyone,
    }
}

fn group(id: Uuid) -> GrantRecipient {
    GrantRecipient::Group { group_id: id }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_clan_lets_every_member_read_the_directory_only() {
    let c = clan().await;
    let grants = c
        .mira
        .api
        .clan_grants(
            c.id,
            ClanGrantFilter {
                group_id: Some(c.everyone),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let founding: Vec<Vec<&str>> = grants
        .iter()
        .map(|grant| grant.actions.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(founding, [["clan.read_members"]]);
    assert!(grants.iter().all(|grant| grant.scope == GrantScope::Clan));
    assert_eq!(c.nessa.api.clan_members(c.id).await.unwrap().len(), 3);

    // A member sees the grants to the groups they are in, not others'.
    let seen = c
        .nessa
        .api
        .clan_grants(c.id, ClanGrantFilter::default())
        .await
        .unwrap();
    assert_eq!(seen.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_groups_creator_joins_it_and_chooses_its_members() {
    let c = clan().await;
    c.server.clan_grant(
        c.id,
        ClanRecipient::User(c.nessa.user.id),
        ClanGrantScope::Clan,
        &[action::CREATE_GROUP],
    );
    let researchers = c
        .nessa
        .api
        .create_clan_group(c.id, "DV researchers", None)
        .await
        .expect("group.create");
    assert!(researchers.is_member, "the creator joins it");
    for held in [
        action::RENAME_GROUP,
        action::DELETE_GROUP,
        action::ASSIGN_GROUP,
        action::INSPECT_GROUP,
    ] {
        assert!(researchers.can(held), "the creator holds {held}");
    }
    c.nessa
        .api
        .add_clan_group_member(c.id, researchers.id, c.arun.user.id)
        .await
        .expect("the creator adds others");
    let roster = c
        .nessa
        .api
        .clan_group_members(c.id, researchers.id)
        .await
        .unwrap();
    assert_eq!(roster.len(), 2);
    // Leaving and rejoining their own group is the creator's.
    c.nessa
        .api
        .remove_clan_group_member(c.id, researchers.id, c.nessa.user.id)
        .await
        .expect("leave it");
    c.nessa
        .api
        .add_clan_group_member(c.id, researchers.id, c.nessa.user.id)
        .await
        .expect("rejoin it");

    // A group lead elsewhere assigns others, never themselves.
    let mapper_lead = c
        .mira
        .api
        .create_clan_group(c.id, "Map authors", None)
        .await
        .unwrap();
    c.server.clan_grant(
        c.id,
        ClanRecipient::User(c.arun.user.id),
        ClanGrantScope::Groups([mapper_lead.id].into()),
        &[action::INSPECT_GROUP, action::ASSIGN_GROUP],
    );
    c.arun
        .api
        .add_clan_group_member(c.id, mapper_lead.id, c.nessa.user.id)
        .await
        .expect("a group lead adds another member");
    assert!(is_not_found(
        &c.arun
            .api
            .add_clan_group_member(c.id, mapper_lead.id, c.arun.user.id)
            .await
    ));
    // Clan owners add anyone to any group, themselves included.
    c.mira
        .api
        .add_clan_group_member(c.id, researchers.id, c.mira.user.id)
        .await
        .expect("an owner joins any group");

    // Leaving the clan ends the creator's authority; rejoining does not
    // bring it back.
    c.nessa
        .api
        .remove_clan_member(c.id, c.nessa.user.id)
        .await
        .expect("leave the clan");
    c.server.join_clan(c.id, &c.nessa.user);
    let groups = c.nessa.api.clan_groups(c.id).await.unwrap();
    let again = groups.iter().find(|g| g.id == researchers.id).unwrap();
    assert!(!again.is_member);
    assert!(again.actions.is_empty(), "{:?}", again.actions);
    assert!(is_not_found(
        &c.nessa
            .api
            .add_clan_group_member(c.id, researchers.id, c.nessa.user.id)
            .await
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grants_carry_inline_actions_and_owners_alone_delegate() {
    let c = clan().await;
    let atlas = c.server.create_clan_atlas(c.id, "Protected");
    let area = c.server.create_clan_area(c.id, atlas, "DV");
    let scouts = c
        .mira
        .api
        .create_clan_group(c.id, "Scouts", None)
        .await
        .unwrap();
    let folder = GrantScope::Atlases {
        ids: vec![AtlasId(atlas)],
    };

    // atlas.delete is ownership authority: no grant carries it.
    let refused = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(scouts.id),
            &folder,
            &GrantBody::of(["atlas.delete"]),
        )
        .await;
    assert!(is_bad_request(&refused), "{refused:?}");

    // A delegation never hands out administration, on any scope.
    for (scope, may) in [
        (GrantScope::Clan, "clan.invite"),
        (GrantScope::Clan, "group.assign"),
        (folder.clone(), "grant.inspect"),
    ] {
        let body = GrantBody {
            actions: vec!["grant.inspect".into(), "grant.manage".into()],
            may_grant: Some(vec!["area.read".into(), may.into()]),
        };
        let refused = c
            .mira
            .api
            .grant_in_clan(c.id, group(scouts.id), &scope, &body)
            .await;
        assert!(is_bad_request(&refused), "{may}: {refused:?}");
    }

    // A Folder manager: Nessa hands out Reader within the folder.
    c.mira
        .api
        .add_clan_group_member(c.id, scouts.id, c.nessa.user.id)
        .await
        .unwrap();
    let delegation = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(scouts.id),
            &folder,
            &GrantBody {
                actions: vec!["grant.inspect".into(), "grant.manage".into()],
                may_grant: Some(vec!["area.read".into()]),
            },
        )
        .await
        .expect("an owner delegates");
    assert_eq!(delegation.may_grant, Some(["area.read".to_string()].into()));
    let issued = c
        .nessa
        .api
        .grant_in_clan(
            c.id,
            group(scouts.id),
            &folder,
            &GrantBody::of(["area.read"]),
        )
        .await
        .expect("within the delegation");
    assert_eq!(issued.parent_id, Some(delegation.id));
    let wider = c
        .nessa
        .api
        .grant_in_clan(
            c.id,
            group(scouts.id),
            &folder,
            &GrantBody::of(["area.read", "area.edit"]),
        )
        .await;
    assert!(is_not_found(&wider), "beyond may_grant: {wider:?}");
    // A delegate never delegates further, nor gives themselves administration.
    let further = c
        .nessa
        .api
        .grant_in_clan(
            c.id,
            GrantRecipient::User {
                user_id: c.arun.user.id,
            },
            &GrantScope::Areas { ids: vec![area] },
            &GrantBody {
                actions: vec!["grant.manage".into()],
                may_grant: Some(vec!["area.read".into()]),
            },
        )
        .await;
    assert!(is_not_found(&further), "{further:?}");
    let admin = c
        .nessa
        .api
        .grant_in_clan(
            c.id,
            GrantRecipient::User {
                user_id: c.nessa.user.id,
            },
            &GrantScope::Groups {
                ids: vec![scouts.id],
            },
            &GrantBody::of([action::ASSIGN_GROUP]),
        )
        .await;
    assert!(admin.is_err(), "{admin:?}");

    // The delegate changes what they issued, within bounds.
    c.nessa
        .api
        .change_clan_grant(c.id, issued.id, &GrantBody::of(["area.read"]))
        .await
        .expect("change within bounds");
    // Taking grant.manage away deletes what was issued under it.
    c.mira
        .api
        .change_clan_grant(c.id, delegation.id, &GrantBody::of(["grant.inspect"]))
        .await
        .expect("an owner narrows the delegation");
    let left = c
        .mira
        .api
        .clan_grants(
            c.id,
            ClanGrantFilter {
                area_id: Some(area),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(left.iter().all(|grant| grant.id != issued.id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delegate_widens_a_grant_they_did_not_issue_only_under_their_delegation() {
    let c = clan().await;
    let atlas = c.server.create_clan_atlas(c.id, "Protected");
    let scouts = c
        .mira
        .api
        .create_clan_group(c.id, "Scouts", None)
        .await
        .unwrap();
    let folder = GrantScope::Atlases {
        ids: vec![AtlasId(atlas)],
    };
    c.mira
        .api
        .add_clan_group_member(c.id, scouts.id, c.nessa.user.id)
        .await
        .unwrap();
    let delegation = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(scouts.id),
            &folder,
            &GrantBody {
                actions: vec!["grant.inspect".into(), "grant.manage".into()],
                may_grant: Some(vec!["area.read".into(), "area.edit".into()]),
            },
        )
        .await
        .expect("an owner delegates");
    let owners = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(scouts.id),
            &folder,
            &GrantBody::of(["area.read"]),
        )
        .await
        .expect("an owner's grant");
    let grants_to_scouts = |grants: Vec<smudgy_cloud::clans::ClanGrant>| {
        let mut found: Vec<(Uuid, Vec<String>, Option<Uuid>)> = grants
            .into_iter()
            .filter(|grant| grant.recipient == group(scouts.id) && grant.id != delegation.id)
            .map(|grant| {
                (
                    grant.id,
                    grant.actions.iter().cloned().collect(),
                    grant.parent_id,
                )
            })
            .collect();
        found.sort_by_key(|(_, _, parent)| parent.is_some());
        found
    };

    // Widening keeps the owner's grant as it was and issues the addition
    // under Nessa's delegation, so it ends with it.
    let answered = c
        .nessa
        .api
        .change_clan_grant(c.id, owners.id, &GrantBody::of(["area.read", "area.edit"]))
        .await
        .expect("widen within the delegation");
    assert_eq!(answered.id, owners.id, "the grant kept its actions");
    let all = c
        .mira
        .api
        .clan_grants(c.id, ClanGrantFilter::default())
        .await
        .unwrap();
    let found = grants_to_scouts(all);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0], (owners.id, vec!["area.read".to_string()], None));
    let issued = found[1].0;
    assert_eq!(
        found[1],
        (issued, vec!["area.edit".to_string()], Some(delegation.id))
    );

    // A change that keeps nothing of the owner's grant answers the issued
    // grant, which the addition joins.
    let answered = c
        .nessa
        .api
        .change_clan_grant(c.id, owners.id, &GrantBody::of(["area.edit"]))
        .await
        .expect("narrow it away");
    assert_eq!(answered.id, issued, "the answer names another grant");
    let found = grants_to_scouts(
        c.mira
            .api
            .clan_grants(c.id, ClanGrantFilter::default())
            .await
            .unwrap(),
    );
    assert_eq!(
        found,
        [(issued, vec!["area.edit".to_string()], Some(delegation.id))]
    );

    // Ending the delegation ends what it added.
    c.mira
        .api
        .delete_clan_grant(c.id, delegation.id)
        .await
        .unwrap();
    let found = grants_to_scouts(
        c.mira
            .api
            .clan_grants(c.id, ClanGrantFilter::default())
            .await
            .unwrap(),
    );
    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn package_scopes_reach_exactly_their_packages() {
    let c = clan().await;
    let trail = c.server.clan_package(c.id, "trail-tools");
    let ledger = c.server.clan_package(c.id, "ledger");
    let maintainers = c
        .mira
        .api
        .create_clan_group(c.id, "Package maintainers", None)
        .await
        .unwrap();
    c.mira
        .api
        .add_clan_group_member(c.id, maintainers.id, c.nessa.user.id)
        .await
        .unwrap();
    let grant = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(maintainers.id),
            &GrantScope::Packages { ids: vec![trail] },
            &GrantBody::of(["package.read", "package.edit_draft", "package.publish"]),
        )
        .await
        .expect("a packages scope");

    // Only the specifically granted package appears in the access index.
    let rows = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Packages)
        .await
        .unwrap();
    let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["trail-tools"]);
    let trail_row = rows.iter().find(|row| row.id == trail).unwrap();
    assert!(trail_row.can("package.edit_draft") && trail_row.can("package.publish"));
    assert!(rows.iter().all(|row| row.id != ledger));

    // The grant filter finds what reaches one package.
    let on_trail = c
        .mira
        .api
        .clan_grants(
            c.id,
            ClanGrantFilter {
                package_id: Some(trail),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(on_trail.iter().any(|row| row.id == grant.id));
    let on_ledger = c
        .mira
        .api
        .clan_grants(
            c.id,
            ClanGrantFilter {
                package_id: Some(ledger),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(on_ledger.iter().all(|row| row.id != grant.id));

    // Package actions go to members too; an unknown package is the 404;
    // package.create applies to the clan alone.
    c.mira
        .api
        .grant_in_clan(
            c.id,
            GrantRecipient::User {
                user_id: c.arun.user.id,
            },
            &GrantScope::Packages { ids: vec![ledger] },
            &GrantBody::of(["package.read", "package.edit_draft"]),
        )
        .await
        .expect("a member takes package actions");
    let unknown = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(maintainers.id),
            &GrantScope::Packages {
                ids: vec![Uuid::new_v4()],
            },
            &GrantBody::of(["package.read"]),
        )
        .await;
    assert!(is_not_found(&unknown));
    let create = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(maintainers.id),
            &GrantScope::Packages { ids: vec![trail] },
            &GrantBody::of(["package.create"]),
        )
        .await;
    assert!(is_bad_request(&create));

    // Membership alone grants no package resource permissions.
    let solo = member(&c.server, "solo");
    c.server.join_clan(c.id, &solo.user);
    assert!(
        solo.api
            .clan_resources(c.id, ResourceKind::Packages)
            .await
            .unwrap()
            .is_empty()
    );
    let names: Vec<String> = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Packages)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.name)
        .collect();
    assert_eq!(names, ["trail-tools"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_access_index_lists_what_the_caller_holds_and_who_reaches_it() {
    let c = clan().await;
    let protected = c.server.create_clan_atlas(c.id, "Protected");
    let legendary = c.server.create_clan_atlas(c.id, "Legendary");
    let dv = c.server.create_clan_area(c.id, protected, "DV");
    let vault = c.server.create_clan_area(c.id, legendary, "Vault");
    let readers = c
        .mira
        .api
        .create_clan_group(c.id, "Protected readers", None)
        .await
        .unwrap();
    c.mira
        .api
        .add_clan_group_member(c.id, readers.id, c.nessa.user.id)
        .await
        .unwrap();
    let folder_grant = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(readers.id),
            &GrantScope::Atlases {
                ids: vec![AtlasId(protected)],
            },
            &GrantBody::of(["atlas.read", "area.read", "secret.create_member_owned"]),
        )
        .await
        .unwrap();
    let map_grant = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(readers.id),
            &GrantScope::Areas { ids: vec![dv] },
            &GrantBody::of(["area.read", "area.add", "area.edit"]),
        )
        .await
        .unwrap();

    // The owner sees every map and folder, with who reaches each and how.
    let maps = c
        .mira
        .api
        .clan_resources(c.id, ResourceKind::Areas)
        .await
        .unwrap();
    let names: Vec<&str> = maps.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(names, ["DV", "Vault"]);
    let dv_row = &maps[0];
    assert_eq!(dv_row.atlas_id, Some(AtlasId(protected)));
    assert!(dv_row.clan_owned());
    let reaching = dv_row.grants.as_ref().expect("an owner inspects");
    let through: Vec<(Uuid, GrantThrough)> = reaching
        .iter()
        .map(|row| (row.grant_id, row.through))
        .collect();
    assert!(through.contains(&(folder_grant.id, GrantThrough::Atlas)));
    assert!(through.contains(&(map_grant.id, GrantThrough::Direct)));
    let folders = c
        .mira
        .api
        .clan_resources(c.id, ResourceKind::Atlases)
        .await
        .unwrap();
    assert_eq!(folders.len(), 2);
    let protected_row = folders.iter().find(|row| row.id == protected).unwrap();
    assert!(
        protected_row
            .grants
            .as_ref()
            .unwrap()
            .iter()
            .any(|row| row.grant_id == folder_grant.id && row.through == GrantThrough::Direct)
    );

    // A reader lists only what they hold, without the grants.
    let seen = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Areas)
        .await
        .unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].id, dv.0);
    assert!(seen[0].grants.is_none());
    assert!(seen[0].can("area.edit"));
    let seen_folders = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Atlases)
        .await
        .unwrap();
    assert_eq!(seen_folders.len(), 1);
    assert!(
        c.arun
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .unwrap()
            .is_empty(),
        "nothing about maps someone does not reach"
    );
    assert!(
        c.arun
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .is_ok()
    );
    let _ = vault;

    // Secrets: only the ones the caller reads.
    let maps_client = CloudMapper::new(c.server.base_url.clone(), c.nessa.user.api_key.clone());
    let quest = maps_client
        .create_secret_as(
            &dv,
            &NewSecret {
                name: "Phylactery quest".to_string(),
                color: Some("#8a5cf6".to_string()),
                owner: NewSecretOwner::Members { clan_id: c.id },
            },
            maps_client.auth_generation(),
        )
        .await
        .expect("Nessa creates a Member-owned Secret");
    let hers = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Secrets)
        .await
        .unwrap();
    assert_eq!(hers.len(), 1);
    assert_eq!(hers[0].name, "Phylactery quest");
    assert_eq!(hers[0].ownership.as_deref(), Some("members"));
    assert_eq!(hers[0].area_id, Some(dv));
    assert!(!hers[0].clan_owned());
    let _ = quest;
    assert!(
        c.mira
            .api
            .clan_resources(c.id, ResourceKind::Secrets)
            .await
            .unwrap()
            .is_empty(),
        "the clan's owner learns nothing of a Member-owned Secret"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_lists_only_to_a_holder_of_an_action_on_it() {
    let c = clan().await;
    let protected = c.server.create_clan_atlas(c.id, "Protected");
    let dv = c.server.create_clan_area(c.id, protected, "DV");
    let readers = c
        .mira
        .api
        .create_clan_group(c.id, "DV readers", None)
        .await
        .unwrap();
    c.mira
        .api
        .add_clan_group_member(c.id, readers.id, c.nessa.user.id)
        .await
        .unwrap();
    c.mira
        .api
        .grant_in_clan(
            c.id,
            group(readers.id),
            &GrantScope::Areas { ids: vec![dv] },
            &GrantBody::of(["area.read"]),
        )
        .await
        .unwrap();

    // Reading a map in it holds nothing on the folder itself.
    let maps = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Areas)
        .await
        .unwrap();
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0].id, dv.0);
    assert!(
        c.nessa
            .api
            .clan_resources(c.id, ResourceKind::Atlases)
            .await
            .unwrap()
            .is_empty(),
        "no row for a folder the caller holds no action on"
    );

    // A folder action lists it, with that action alone.
    c.mira
        .api
        .grant_in_clan(
            c.id,
            group(readers.id),
            &GrantScope::Atlases {
                ids: vec![AtlasId(protected)],
            },
            &GrantBody::of(["atlas.read", "area.read"]),
        )
        .await
        .unwrap();
    let folders = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Atlases)
        .await
        .unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].id, protected);
    assert!(folders[0].can("atlas.read"));
    assert!(!folders[0].can("area.read"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_access_index_lists_only_the_grants_the_caller_may_inspect() {
    let c = clan().await;
    let protected = c.server.create_clan_atlas(c.id, "Protected");
    let legendary = c.server.create_clan_atlas(c.id, "Legendary");
    let dv = c.server.create_clan_area(c.id, protected, "DV");
    let wardens = c
        .mira
        .api
        .create_clan_group(c.id, "Wardens", None)
        .await
        .unwrap();
    c.mira
        .api
        .add_clan_group_member(c.id, wardens.id, c.arun.user.id)
        .await
        .unwrap();
    // Nessa inspects the grants within Protected, through her group.
    let inspectors = c
        .mira
        .api
        .create_clan_group(c.id, "Inspectors", None)
        .await
        .unwrap();
    c.mira
        .api
        .add_clan_group_member(c.id, inspectors.id, c.nessa.user.id)
        .await
        .unwrap();
    let inspect = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(inspectors.id),
            &GrantScope::Atlases {
                ids: vec![AtlasId(protected)],
            },
            &GrantBody::of(["grant.inspect"]),
        )
        .await
        .unwrap();
    // A grant naming DV lies within her scope; a clan-wide one to a group
    // she is not in does not, though it reaches DV too.
    let within = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(wardens.id),
            &GrantScope::Areas { ids: vec![dv] },
            &GrantBody::of(["area.read", "area.edit"]),
        )
        .await
        .unwrap();
    let clan_wide = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            group(wardens.id),
            &GrantScope::Atlases {
                ids: vec![AtlasId(protected), AtlasId(legendary)],
            },
            &GrantBody::of(["area.read"]),
        )
        .await
        .unwrap();

    let listed = |rows: Vec<smudgy_cloud::clan_access::IndexedResource>| -> Option<Vec<Uuid>> {
        let row = rows.into_iter().find(|row| row.id == dv.0)?;
        row.grants
            .map(|grants| grants.into_iter().map(|grant| grant.grant_id).collect())
    };
    let owner_sees = listed(
        c.mira
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .unwrap(),
    )
    .expect("an owner inspects");
    for grant in [inspect.id, within.id, clan_wide.id] {
        assert!(owner_sees.contains(&grant), "an owner sees every grant");
    }
    let nessa_sees = listed(
        c.nessa
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .unwrap(),
    )
    .expect("grant.inspect on the folder inspects its maps");
    assert!(nessa_sees.contains(&inspect.id), "her group's grant");
    assert!(nessa_sees.contains(&within.id), "a grant within her scope");
    assert!(
        !nessa_sees.contains(&clan_wide.id),
        "a grant reaching past her scope stays hidden"
    );
    assert_eq!(
        listed(
            c.arun
                .api
                .clan_resources(c.id, ResourceKind::Areas)
                .await
                .unwrap()
        ),
        None,
        "a reader inspects nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_propose_groups_the_inviter_may_fill() {
    let c = clan().await;
    let theo = member(&c.server, "theo");
    let readers = c
        .mira
        .api
        .create_clan_group(c.id, "Protected readers", None)
        .await
        .unwrap();
    let keepers = c
        .mira
        .api
        .create_clan_group(c.id, "Secret keepers", None)
        .await
        .unwrap();
    // Nessa invites and leads Protected readers only.
    c.server.clan_grant(
        c.id,
        ClanRecipient::User(c.nessa.user.id),
        ClanGrantScope::Clan,
        &[action::INVITE],
    );
    c.server.clan_grant(
        c.id,
        ClanRecipient::User(c.nessa.user.id),
        ClanGrantScope::Groups([readers.id].into()),
        &[action::ASSIGN_GROUP],
    );
    let refused = c
        .nessa
        .api
        .invite_to_clan(c.id, theo.user.id, &[readers.id, keepers.id])
        .await;
    assert!(is_not_found(&refused), "{refused:?}");
    let invitation = c
        .nessa
        .api
        .invite_to_clan(c.id, theo.user.id, &[readers.id])
        .await
        .expect("a group she may fill");
    assert_eq!(invitation.group_ids, [readers.id]);
    let listed = c.mira.api.clan_invitations(c.id).await.unwrap();
    assert_eq!(listed[0].group_ids, [readers.id]);

    let joined = theo
        .api
        .accept_clan_invitation(invitation.id)
        .await
        .expect("accept");
    assert!(joined.group_ids.contains(&readers.id));
    assert!(!joined.group_ids.contains(&keepers.id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_profile_changes_with_edit_profile() {
    let c = clan().await;
    let refused = c
        .nessa
        .api
        .update_clan_profile(
            c.id,
            &ClanProfilePatch {
                name: None,
                description: Some("Ours".to_string()),
            },
        )
        .await;
    assert!(is_not_found(&refused));
    let changed = c
        .mira
        .api
        .update_clan_profile(
            c.id,
            &ClanProfilePatch {
                name: Some("Lantern Co.".to_string()),
                description: Some("Maps for the next run.".to_string()),
            },
        )
        .await
        .expect("an owner edits the profile");
    assert_eq!(changed.name, "Lantern Co.");
    assert_eq!(
        changed.description.as_deref(),
        Some("Maps for the next run.")
    );
    c.server.clan_grant(
        c.id,
        ClanRecipient::User(c.nessa.user.id),
        ClanGrantScope::Clan,
        &[action::EDIT_PROFILE],
    );
    let only_description = c
        .nessa
        .api
        .update_clan_profile(
            c.id,
            &ClanProfilePatch {
                name: None,
                description: Some(String::new()),
            },
        )
        .await
        .expect("clan.edit_profile");
    assert_eq!(only_description.name, "Lantern Co.");
    let blank = c
        .mira
        .api
        .update_clan_profile(
            c.id,
            &ClanProfilePatch {
                name: Some("  ".to_string()),
                description: None,
            },
        )
        .await;
    assert!(is_bad_request(&blank));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_body_naming_a_role_is_refused() {
    let c = clan().await;
    let response = reqwest::Client::new()
        .post(format!("{}/clans/{}/grants", c.server.base_url, c.id))
        .bearer_auth(&c.mira.user.session_token)
        .json(&serde_json::json!({
            "recipient": { "group_id": c.everyone },
            "scope": { "kind": "clan" },
            "role_id": Uuid::new_v4(),
            "actions": ["area.read"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_takes_map_access_only_on_a_grant_naming_one_map() {
    let c = clan().await;
    let roads = c.server.create_clan_atlas(c.id, "Roads and Towns");
    let solace = c.server.create_clan_area(c.id, roads, "Solace");
    let haven = c.server.create_clan_area(c.id, roads, "Haven");
    let nessa = GrantRecipient::User {
        user_id: c.nessa.user.id,
    };
    let editor = ["area.read", "area.add", "area.edit", "area.remove_content"];

    let grant = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            nessa,
            &GrantScope::Areas { ids: vec![solace] },
            &GrantBody::of(editor),
        )
        .await
        .expect("one map takes a member");
    let rows = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Areas)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, solace.0);
    assert!(rows[0].can("area.remove_content"));
    let owner_view = c
        .mira
        .api
        .clan_resources(c.id, ResourceKind::Areas)
        .await
        .unwrap();
    let solace_row = owner_view.iter().find(|row| row.id == solace.0).unwrap();
    assert!(
        solace_row
            .grants
            .as_ref()
            .unwrap()
            .iter()
            .any(|row| row.grant_id == grant.id && row.through == GrantThrough::Direct)
    );

    // Two maps, or a folder, take no member.
    let two = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            nessa,
            &GrantScope::Areas {
                ids: vec![solace, haven],
            },
            &GrantBody::of(["area.read"]),
        )
        .await;
    assert!(is_bad_request(&two), "{two:?}");
    let folder = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            nessa,
            &GrantScope::Atlases {
                ids: vec![AtlasId(roads)],
            },
            &GrantBody::of(["area.read"]),
        )
        .await;
    assert!(is_bad_request(&folder), "{folder:?}");
    let clan_wide = c
        .mira
        .api
        .grant_in_clan(
            c.id,
            nessa,
            &GrantScope::Clan,
            &GrantBody::of(["area.read"]),
        )
        .await;
    assert!(is_bad_request(&clan_wide), "{clan_wide:?}");

    // Changing it keeps to the same rule.
    c.mira
        .api
        .change_clan_grant(c.id, grant.id, &GrantBody::of(["area.read", "secret.read"]))
        .await
        .expect("map and Secret actions on the one map");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_index_lists_a_maps_outside_shares_and_a_packages_grants() {
    use smudgy_cloud::cloud_api::{CreateShareRequest, ShareScope};

    let c = clan().await;
    let roads = c.server.create_clan_atlas(c.id, "Roads and Towns");
    let solace = c.server.create_clan_area(c.id, roads, "Solace");
    // Nessa reads Solace and may share it outside the clan; Arun reads it.
    c.mira
        .api
        .grant_in_clan(
            c.id,
            GrantRecipient::User {
                user_id: c.nessa.user.id,
            },
            &GrantScope::Areas { ids: vec![solace] },
            &GrantBody::of(["area.read", "area.share_external"]),
        )
        .await
        .unwrap();
    c.mira
        .api
        .grant_in_clan(
            c.id,
            GrantRecipient::User {
                user_id: c.arun.user.id,
            },
            &GrantScope::Areas { ids: vec![solace] },
            &GrantBody::of(["area.read"]),
        )
        .await
        .unwrap();
    let ollie = member(&c.server, "ollie");
    c.server.befriend(&c.nessa.user, &ollie.user);
    let share = c
        .nessa
        .api
        .create_share(CreateShareRequest {
            grantee_id: ollie.user.id,
            scope: ShareScope::Area { area_id: solace },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            can_admin: false,
            host_hints: None,
        })
        .await
        .expect("an outside share");

    let row_of = |rows: Vec<smudgy_cloud::clan_access::IndexedResource>| {
        rows.into_iter().find(|row| row.id == solace.0).unwrap()
    };
    let owner_row = row_of(
        c.mira
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .unwrap(),
    );
    let shares = owner_row.outside_shares.expect("clan owners see them");
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].id, share.id);
    assert_eq!(shares[0].grantor_nickname.as_deref(), Some("nessa"));
    assert_eq!(shares[0].grantee_nickname.as_deref(), Some("ollie"));
    let sharer_row = row_of(
        c.nessa
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .unwrap(),
    );
    assert_eq!(
        sharer_row.outside_shares.map(|shares| shares.len()),
        Some(1)
    );
    let reader_row = row_of(
        c.arun
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .unwrap(),
    );
    assert!(reader_row.outside_shares.is_none(), "a reader sees none");
    // Holding area.share_external without reading the map shows none.
    let sid = member(&c.server, "sid");
    c.server.join_clan(c.id, &sid.user);
    c.server.clan_grant(
        c.id,
        ClanRecipient::User(sid.user.id),
        ClanGrantScope::Areas([solace.0].into()),
        &["area.share_external"],
    );
    let unread = sid
        .api
        .clan_resources(c.id, ResourceKind::Areas)
        .await
        .unwrap();
    assert!(
        unread
            .iter()
            .all(|row| row.id != solace.0 || row.outside_shares.is_none()),
        "outside shares need area.read too"
    );
    assert!(is_not_found(&sid.api.outside_shares(solace).await));

    // A clan owner revokes it.
    c.mira.api.revoke_share(share.id).await.expect("revoke");
    let owner_row = row_of(
        c.mira
            .api
            .clan_resources(c.id, ResourceKind::Areas)
            .await
            .unwrap(),
    );
    assert_eq!(owner_row.outside_shares.map(|shares| shares.len()), Some(0));

    // A package's grants show to those who inspect that package.
    let trail = c.server.clan_package(c.id, "trail-tools");
    c.mira
        .api
        .grant_in_clan(
            c.id,
            group(c.everyone),
            &GrantScope::Packages { ids: vec![trail] },
            &GrantBody::of(["package.read"]),
        )
        .await
        .unwrap();
    let packages = c
        .mira
        .api
        .clan_resources(c.id, ResourceKind::Packages)
        .await
        .unwrap();
    let grants = packages[0].grants.as_ref().expect("an owner inspects");
    assert!(grants.iter().any(|grant| {
        grant.recipient
            == GrantRecipient::Group {
                group_id: c.everyone,
            }
            && grant.through == GrantThrough::Direct
            && grant.actions.contains("package.read")
    }));
    let seen = c
        .nessa
        .api
        .clan_resources(c.id, ResourceKind::Packages)
        .await
        .unwrap();
    assert_eq!(seen[0].id, trail);
    assert!(seen[0].grants.is_none());
}
