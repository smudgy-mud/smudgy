use std::collections::BTreeSet;

use smudgy_cloud::clans::{ClanGrant, ClanGroup, ClanInvitation, ClanMember, ClanSummary};
use smudgy_palette::ColorSequence;

use super::editor::EditorContext;
use super::*;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn summary(is_owner: bool, actions: &[&str]) -> ClanSummary {
    ClanSummary {
        id: id(1),
        name: "Lantern Company".to_string(),
        description: None,
        created_at: "2026-10-01T00:00:00Z".parse().unwrap(),
        member_count: 3,
        is_owner,
        group_ids: Vec::new(),
        actions: actions.iter().map(ToString::to_string).collect(),
    }
}

fn member(n: u128, is_owner: bool) -> ClanMember {
    ClanMember {
        user_id: id(n),
        nickname: Some(format!("user{n}")),
        joined_at: "2026-10-01T00:00:00Z".parse().unwrap(),
        is_owner,
        group_ids: Vec::new(),
    }
}

fn group(n: u128, color: Option<&str>, builtin: Option<&str>, actions: &[&str]) -> ClanGroup {
    ClanGroup {
        id: id(n),
        name: format!("group{n}"),
        color: color.map(ToString::to_string),
        builtin: builtin.map(ToString::to_string),
        is_member: false,
        created_by_me: false,
        actions: actions
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>(),
    }
}

fn invitation(inviter: u128) -> ClanInvitation {
    ClanInvitation {
        id: id(90),
        clan_id: id(1),
        user_id: id(91),
        nickname: Some("tomas".to_string()),
        inviter_id: id(inviter),
        inviter_nickname: None,
        group_ids: Vec::new(),
        created_at: "2026-10-01T00:00:00Z".parse().unwrap(),
    }
}

fn grant(recipient: GrantRecipient, scope: GrantScope, actions: &[&str]) -> ClanGrant {
    serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(),
        "clan_id": id(1),
        "recipient": recipient,
        "actions": actions,
        "scope": scope,
        "issuer_id": id(2),
        "delegated": [],
        "created_at": "2026-10-07T00:00:00Z",
        "updated_at": "2026-10-07T00:00:00Z"
    }))
    .unwrap()
}

fn panel_with(page: ClanPage) -> ClanPanel {
    let mut panel = ClanPanel::new(crate::cloud_account::test_handles_signed_in("mira"));
    panel.open = Some(page);
    panel
}

#[tokio::test]
async fn assignment_dialog_actions_stay_visible_and_clickable() {
    use crate::widgets::dialog::tests::check_actions;
    for scope in [
        ScopeKind::Clan,
        ScopeKind::Folders,
        ScopeKind::Maps,
        ScopeKind::Packages,
        ScopeKind::Groups,
    ] {
        let mut page = ClanPage::new(summary(true, &[]));
        page.groups = vec![group(3, None, None, &[])];
        let mut editor = editor::GrantEditor::for_resource(scope, id(10));
        editor.recipient = Some(GrantRecipient::Group { group_id: id(3) });
        editor.actions.insert(action::MANAGE_GRANTS);
        editor.error =
            Some("An example validation error with enough detail to wrap across lines. ".repeat(4));
        page.modal = Some(Modal::Grant(editor));
        let panel = panel_with(page);
        for size in [(400, 360), (640, 500), (900, 800)] {
            let element = iced::widget::center(panel.modal_view().unwrap())
                .padding(24)
                .into();
            let messages = check_actions(
                element,
                size,
                &[
                    crate::i18n::t!("action-cancel"),
                    crate::i18n::t!("action-save"),
                ],
                &format!("assign-{scope:?}"),
            )
            .await;
            assert_eq!(
                messages
                    .iter()
                    .filter(|m| matches!(m, Message::SaveGrant))
                    .count(),
                2
            );
            assert_eq!(
                messages
                    .iter()
                    .filter(|m| matches!(m, Message::CloseModal))
                    .count(),
                2
            );
        }
    }
}

#[tokio::test]
async fn secret_assignment_keeps_save_visible_with_wrapped_errors() {
    let mut page = ClanPage::new(summary(true, &[]));
    page.groups = vec![group(3, None, None, &[])];
    let mut editor = SecretGrantEditor::new(id(10));
    editor.recipient = Some(smudgy_cloud::clan_secrets::SecretRecipient::Group { group_id: id(3) });
    editor.error = Some("A validation error with a detailed explanation. ".repeat(20));
    page.modal = Some(Modal::SecretGrant(editor));
    let panel = panel_with(page);
    for size in [(400, 360), (640, 500)] {
        let messages = crate::widgets::dialog::tests::check_actions(
            iced::widget::center(panel.modal_view().unwrap())
                .padding(24)
                .into(),
            size,
            &[crate::i18n::t!("action-save")],
            "assign-secret",
        )
        .await;
        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .all(|m| matches!(m, Message::SaveSecretGrant))
        );
    }
}

#[test]
fn a_new_group_takes_the_first_color_its_clan_has_not_used() {
    let sequence = ColorSequence::seeded(&["Lantern Company"]);
    let first = sequence.color(0).to_string();
    let second = sequence.color(1).to_string();
    let third = sequence.color(2).to_string();

    assert_eq!(default_group_color("Lantern Company", &[]), first);
    // Built-ins carry no color and take none.
    let builtins = [
        group(2, None, Some("owners"), &[]),
        group(3, None, Some("members"), &[]),
    ];
    assert_eq!(default_group_color("Lantern Company", &builtins), first);

    let one = [group(4, Some(&first), None, &[])];
    assert_eq!(default_group_color("Lantern Company", &one), second);
    // Deleting a group frees its color for the next one.
    let gap = [group(5, Some(&second), None, &[])];
    assert_eq!(default_group_color("Lantern Company", &gap), first);
    // The server stores colors in lowercase; any case counts as used.
    let upper = [
        group(6, Some(&first.to_uppercase()), None, &[]),
        group(7, Some(&second), None, &[]),
    ];
    assert_eq!(default_group_color("Lantern Company", &upper), third);
    // Another clan's sequence starts elsewhere.
    assert_ne!(default_group_color("Roads and Towns", &[]), first);
    assert_eq!(group_palette("Lantern Company")[0], first);
}

#[test]
fn only_the_sole_owner_is_the_last_owner() {
    let owner = summary(true, &[]);
    let alone = [member(1, true), member(2, false)];
    assert!(is_last_owner(&owner, Some(&alone), id(1)));
    let shared = [member(1, true), member(2, true)];
    assert!(!is_last_owner(&owner, Some(&shared), id(1)));
    // Unknown until the directory loads; the server's 409 covers that.
    assert!(!is_last_owner(&owner, None, id(1)));
    assert!(!is_last_owner(&summary(false, &[]), Some(&alone), id(2)));
}

#[test]
fn removal_follows_the_actions_not_the_owner_badge() {
    let plain = member(2, false);
    let owner = member(3, true);
    let me = id(1);

    // Owners hold every action.
    let all = summary(true, &[action::REMOVE_MEMBER]);
    assert!(can_remove(&all, &plain, me));
    assert!(can_remove(&all, &owner, me));
    assert!(!can_remove(&all, &member(1, true), me), "never oneself");

    // A member granted clan.remove_member removes members, not owners.
    let delegated = summary(false, &[action::REMOVE_MEMBER]);
    assert!(can_remove(&delegated, &plain, me));
    assert!(!can_remove(&delegated, &owner, me));

    // An owner badge without the action removes nobody.
    let bare = summary(true, &[]);
    assert!(!can_remove(&bare, &plain, me));
}

#[test]
fn invitations_revoke_by_action() {
    let me = id(1);
    let mine = invitation(1);
    let theirs = invitation(2);
    let inviter = summary(false, &[action::INVITE]);
    assert!(can_revoke_invitation(&inviter, &mine, me));
    assert!(!can_revoke_invitation(&inviter, &theirs, me));
    let revoker = summary(false, &[action::REVOKE_INVITATION]);
    assert!(can_revoke_invitation(&revoker, &theirs, me));
    assert!(!can_revoke_invitation(&summary(false, &[]), &mine, me));
}

#[test]
fn the_member_filter_matches_nicknames_ignoring_case() {
    let members = [member(1, true), member(12, false), member(2, false)];
    let names: Vec<Uuid> = filtered(&members, " USER1").map(|m| m.user_id).collect();
    assert_eq!(names, [id(1), id(12)]);
    assert_eq!(filtered(&members, "").count(), 3);
}

#[test]
fn colors_parse_from_the_wire() {
    assert_eq!(
        parse_rgb("#a1B2c3"),
        Some(Rgb {
            r: 0xa1,
            g: 0xb2,
            b: 0xc3
        })
    );
    assert_eq!(parse_rgb("a1b2c3"), None);
    assert_eq!(parse_rgb("#a1b2"), None);
    assert_eq!(parse_rgb("#zzzzzz"), None);
}

#[test]
fn self_assignment_follows_the_creators_rule() {
    let me = id(1);
    let other = id(2);
    let lead = group(
        10,
        None,
        None,
        &[action::ASSIGN_GROUP, action::INSPECT_GROUP],
    );
    let builtin = group(11, None, Some("members"), &[action::ASSIGN_GROUP]);
    let none = group(12, None, None, &[]);

    // A group lead through a grant: others yes, themselves no.
    let mut page = ClanPage::new(summary(false, &[]));
    page.clan.group_ids = vec![id(20)];
    page.grants = vec![grant(
        GrantRecipient::Group { group_id: id(20) },
        GrantScope::Groups { ids: vec![lead.id] },
        &[action::ASSIGN_GROUP],
    )];
    assert_eq!(page.assignable(&lead, other, me), Assign::Yes);
    assert_eq!(page.assignable(&lead, me, me), Assign::NotSelf);
    assert_eq!(page.assignable(&builtin, other, me), Assign::No);
    assert_eq!(page.assignable(&none, other, me), Assign::No);

    // The server says who created a group; no grant is inferred from.
    page.grants.clear();
    assert_eq!(page.assignable(&lead, me, me), Assign::NotSelf);
    let mut created = lead.clone();
    created.created_by_me = true;
    assert_eq!(page.assignable(&created, me, me), Assign::Yes);
    page.grants = vec![grant(
        GrantRecipient::Group { group_id: id(20) },
        GrantScope::Groups { ids: vec![lead.id] },
        &[action::ASSIGN_GROUP],
    )];
    assert_eq!(page.assignable(&created, me, me), Assign::Yes);

    // Leaving a group one is in is always one's own.
    let mut joined = lead.clone();
    joined.is_member = true;
    page.grants = vec![grant(
        GrantRecipient::User { user_id: me },
        GrantScope::Clan,
        &[action::ASSIGN_GROUP],
    )];
    assert_eq!(page.assignable(&joined, me, me), Assign::Yes);

    // Clan owners add anyone, themselves included.
    let owner_page = ClanPage::new(summary(true, &[]));
    assert_eq!(owner_page.assignable(&lead, me, me), Assign::Yes);
}

#[test]
fn delegates_change_only_grants_that_hand_nothing_out() {
    let mut page = ClanPage::new(summary(false, &[action::MANAGE_GRANTS]));
    let plain = grant(
        GrantRecipient::Group { group_id: id(5) },
        GrantScope::Clan,
        &["area.read"],
    );
    let mut delegating = grant(
        GrantRecipient::Group { group_id: id(5) },
        GrantScope::Clan,
        &["grant.inspect", "grant.manage"],
    );
    delegating.may_grant = Some(["area.read".to_string()].into());
    assert!(page.can_grant());
    assert!(page.can_change(&plain));
    assert!(!page.can_change(&delegating));
    page.clan.is_owner = true;
    assert!(page.can_change(&delegating));
    let plain_member = ClanPage::new(summary(false, &[]));
    assert!(!plain_member.can_grant());
    assert!(!plain_member.can_change(&plain));
}

#[test]
fn package_assignments_are_the_grants_that_reach_the_package() {
    let package = id(30);
    let mut page = ClanPage::new(summary(true, &[]));
    let founding = grant(
        GrantRecipient::Group { group_id: id(5) },
        GrantScope::Clan,
        &["package.read"],
    );
    let directory = grant(
        GrantRecipient::Group { group_id: id(5) },
        GrantScope::Clan,
        &["clan.read_members"],
    );
    let named = grant(
        GrantRecipient::Group { group_id: id(6) },
        GrantScope::Packages { ids: vec![package] },
        &["package.read", "package.edit_draft"],
    );
    let elsewhere = grant(
        GrantRecipient::Group { group_id: id(6) },
        GrantScope::Packages { ids: vec![id(31)] },
        &["package.read"],
    );
    page.grants = vec![founding.clone(), directory, named.clone(), elsewhere];
    let reaching: Vec<Uuid> = page
        .grants_on_package(package)
        .into_iter()
        .map(|grant| grant.id)
        .collect();
    assert_eq!(reaching, [founding.id, named.id]);
}

#[test]
fn a_taken_name_keeps_the_dialog_open_with_the_reason() {
    let mut page = ClanPage::new(summary(true, &[]));
    page.modal = Some(Modal::Group {
        group_id: None,
        name: "Scouts".to_string(),
        color: None,
        confirm_delete: false,
        error: None,
    });
    let mut panel = panel_with(page);
    let _ = panel.update(Message::GroupCreated(id(1), Err(CloudError::NameInUse)));
    let Some(Modal::Group { name, error, .. }) = &panel.open.as_ref().unwrap().modal else {
        panic!("the dialog stays open");
    };
    assert_eq!(name, "Scouts");
    assert_eq!(
        error.as_deref(),
        Some(crate::i18n::translate("cloud-error-name-in-use").as_str())
    );
    let _ = panel.update(Message::DialogSaved(id(1), Ok(())));
    assert!(panel.open.as_ref().unwrap().modal.is_none());
}

#[test]
fn a_new_group_opens_on_its_page() {
    let mut page = ClanPage::new(summary(true, &[]));
    page.modal = Some(Modal::Group {
        group_id: None,
        name: "Scouts".to_string(),
        color: None,
        confirm_delete: false,
        error: None,
    });
    let mut panel = panel_with(page);
    let mut created = group(40, None, None, &[action::ASSIGN_GROUP]);
    created.is_member = true;
    let _ = panel.update(Message::GroupCreated(id(1), Ok(created)));
    let page = panel.open.as_ref().unwrap();
    assert!(page.modal.is_none());
    assert_eq!(page.tab, Tab::Groups);
    assert_eq!(page.selected_group, Some(id(40)));
}

#[test]
fn an_unverified_email_hands_back_to_the_gate() {
    let mut panel = ClanPanel::new(crate::cloud_account::test_handles_signed_in("mira"));
    let _ = panel.update(Message::Loaded(
        panel.account(),
        Err(CloudError::EmailNotVerified),
    ));
    assert!(panel.needs_email_verification());
    assert!(panel.error.is_none());
}

fn offer(me: Uuid, accepted: bool) -> OwnershipOffer {
    OwnershipOffer {
        id: Uuid::new_v4(),
        secret_id: id(40),
        secret_name: "Quest".to_string(),
        secret_color: None,
        area_id: smudgy_cloud::AreaId(id(41)),
        clan_id: id(1),
        recipients: vec![
            smudgy_cloud::clan_secrets::OfferRecipient {
                user_id: me,
                nickname: Some("mira".to_string()),
                accepted,
            },
            smudgy_cloud::clan_secrets::OfferRecipient {
                user_id: id(42),
                nickname: None,
                accepted: false,
            },
        ],
        ownership: "members".to_string(),
        replace: false,
        initiator_id: id(43),
        initiator_nickname: None,
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn offers_wait_on_the_caller_until_they_accept() {
    let mut panel = ClanPanel::new(crate::cloud_account::test_handles_signed_in("mira"));
    let me = panel.me().expect("signed in");
    assert_eq!(panel.pending(), 0);
    let _ = panel.update(Message::OffersLoaded(
        panel.account(),
        Ok(vec![offer(me, false), offer(me, true)]),
    ));
    assert_eq!(
        panel.pending(),
        1,
        "an accepted joint offer waits on others"
    );
    // An unreachable offers list leaves none, without an error.
    let _ = panel.update(Message::OffersLoaded(
        panel.account(),
        Err(CloudError::NotFoundOrNoAccess),
    ));
    assert_eq!(panel.pending(), 0);
    assert!(panel.error.is_none());
}

/// A list or offers reply asked for under an earlier credential (another
/// account, or none) never reaches the account in use now.
#[test]
fn replies_for_an_earlier_account_are_dropped() {
    let handles = crate::cloud_account::test_handles_signed_in("mira");
    let credentials = handles.credentials.clone();
    let mut panel = ClanPanel::new(handles);
    let me = panel.me().expect("signed in");
    let earlier = panel.account();
    credentials.set(Some(smudgy_cloud::Credential::Session(
        "smudgy_sess_iris".to_string(),
    )));
    assert_ne!(panel.account(), earlier);

    let _ = panel.update(Message::Loaded(
        earlier,
        Ok(smudgy_cloud::clans::ClansOverview {
            clans: vec![summary(true, &[])],
            invitations: Vec::new(),
        }),
    ));
    assert!(!panel.is_loaded(), "the earlier account's clans never show");
    let _ = panel.update(Message::OffersLoaded(earlier, Ok(vec![offer(me, false)])));
    let map_offer = smudgy_cloud::clan_maps::AreaOwnershipOffer {
        id: Uuid::new_v4(),
        area_id: smudgy_cloud::AreaId(id(41)),
        area_name: "Docks".to_string(),
        clan_id: id(1),
        ownership: smudgy_cloud::clan_maps::MapOwnership::Members,
        replace: false,
        initiator_id: id(43),
        initiator_nickname: None,
        recipients: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    let _ = panel.update(Message::MapOffers(
        earlier,
        crate::components::clan_map_offers::Message::Loaded(Ok(vec![map_offer])),
    ));
    assert_eq!(panel.pending(), 0, "nor do its offers");

    let _ = panel.update(Message::Loaded(
        panel.account(),
        Ok(smudgy_cloud::clans::ClansOverview::default()),
    ));
    assert!(panel.is_loaded(), "the account in use loads");
}

#[test]
fn a_page_load_from_another_clan_is_ignored() {
    let mut panel = panel_with(ClanPage::new(summary(true, &[])));
    let other = ClanSummary {
        id: id(77),
        ..summary(false, &[])
    };
    let _ = panel.update(Message::PageLoaded(
        id(77),
        Box::new(Ok(PageData {
            clan: Some(other),
            ..PageData::default()
        })),
    ));
    let page = panel.open.as_ref().unwrap();
    assert_eq!(page.clan.id, id(1));
    assert!(page.loading, "still waiting for its own load");
}

#[test]
fn a_hidden_directory_leaves_the_page_without_members() {
    let mut panel = panel_with(ClanPage::new(summary(false, &[])));
    let mut groups = vec![
        group(2, None, Some("owners"), &[]),
        group(3, None, Some("members"), &[]),
        group(4, None, None, &[]),
    ];
    groups[2].is_member = true;
    let _ = panel.update(Message::PageLoaded(
        id(1),
        Box::new(Ok(PageData {
            clan: Some(summary(false, &[])),
            members: None,
            groups,
            ..PageData::default()
        })),
    ));
    let page = panel.open.as_ref().unwrap();
    assert!(!page.loading);
    assert!(page.members.is_none());
    assert_eq!(page.member_pages(), 1);
    assert_eq!(page.selected_group, Some(id(2)), "the first group opens");
}

#[test]
fn the_directory_pages_by_twenty() {
    let mut page = ClanPage::new(summary(true, &[]));
    page.members = Some((0..45).map(|n| member(100 + n, false)).collect());
    assert_eq!(page.member_pages(), 3);
    let mut panel = panel_with(page);
    let _ = panel.update(Message::MembersPage(7));
    assert_eq!(
        panel.open.as_ref().unwrap().member_page,
        2,
        "clamped to the last page"
    );
}

#[test]
fn editing_groups_saves_only_what_changed() {
    let mut page = ClanPage::new(summary(true, &[]));
    let mut tomas = member(2, false);
    tomas.group_ids = vec![id(10)];
    page.members = Some(vec![member(1, true), tomas]);
    page.groups = vec![
        group(10, None, None, &[action::ASSIGN_GROUP]),
        group(11, None, None, &[action::ASSIGN_GROUP]),
    ];
    let mut panel = panel_with(page);
    let _ = panel.update(Message::EditMemberGroups(id(2)));
    let Some(Modal::MemberGroups { checked, owner, .. }) = &panel.open.as_ref().unwrap().modal
    else {
        panic!("Edit groups opens");
    };
    assert_eq!(checked.iter().copied().collect::<Vec<_>>(), [id(10)]);
    assert!(!owner);
    let _ = panel.update(Message::MemberGroupToggled(id(11), true));
    let _ = panel.update(Message::MemberGroupToggled(id(10), false));
    let Some(Modal::MemberGroups { checked, .. }) = &panel.open.as_ref().unwrap().modal else {
        panic!("still open");
    };
    assert_eq!(checked.iter().copied().collect::<Vec<_>>(), [id(11)]);
    let _ = panel.update(Message::CloseModal);
    assert!(panel.open.as_ref().unwrap().modal.is_none());
}

#[test]
fn open_map_and_package_hand_off_once() {
    let mut panel = panel_with(ClanPage::new(summary(true, &[])));
    let area = smudgy_cloud::AreaId(id(50));
    let _ = panel.update(Message::OpenMap(area));
    assert_eq!(panel.take_handoff(), Some(Handoff::Map(area)));
    assert_eq!(panel.take_handoff(), None);
    let _ = panel.update(Message::OpenMapAccess(area));
    assert_eq!(panel.take_handoff(), Some(Handoff::MapAccess(area)));
    let _ = panel.update(Message::OpenPackage("trail-tools".to_string()));
    assert_eq!(
        panel.take_handoff(),
        Some(Handoff::Package("trail-tools".to_string()))
    );
}

#[test]
fn opening_group_permissions_without_changes_does_not_write() {
    let mut page = ClanPage::new(summary(true, &[]));
    page.selected_group = Some(id(5));
    page.groups.push(group(5, None, None, &[]));
    let mut panel = panel_with(page);
    let _ = panel.update(Message::AddPermission);
    let _ = panel.update(Message::SaveGroupPermissions);
    let Some(Modal::GroupPermissions(editor)) = &panel.open.as_ref().unwrap().modal else {
        panic!("Manage permissions opens the three-tab editor");
    };
    assert!(editor.writes().is_empty());
    assert!(!editor.saving);
}

fn indexed(kind: &str, n: u128, name: &str) -> IndexedResource {
    serde_json::from_value(serde_json::json!({
        "kind": kind,
        "id": id(n),
        "name": name,
        "owner": { "kind": "clan" },
        "ownership": "clan",
        "actions": ["area.read", "package.read", "read"],
        "grants": [{
            "grant_id": id(900),
            "recipient": { "group_id": id(4) },
            "actions": ["area.read"],
            "through": "direct"
        }]
    }))
    .unwrap()
}

/// Every tab and sub-tab lays out inside the settings page's vertical
/// scrollable: nothing in it may fill its height.
#[test]
fn no_tab_fills_the_page_height() {
    let mut page = ClanPage::new(summary(true, &[action::INVITE, action::CREATE_GROUP]));
    page.loading = false;
    page.members = Some((0..25).map(|n| member(100 + n, n == 0)).collect());
    page.groups = vec![
        group(2, None, Some("owners"), &[]),
        group(3, None, Some("members"), &[]),
        group(
            4,
            Some("#336699"),
            None,
            &[action::ASSIGN_GROUP, action::INSPECT_GROUP],
        ),
    ];
    page.pending = vec![invitation(100)];
    page.grants = vec![grant(
        GrantRecipient::Group { group_id: id(4) },
        GrantScope::Clan,
        &["area.read", "clan.invite"],
    )];
    page.folders = vec![indexed("atlas", 50, "Protected")];
    page.maps = vec![indexed("area", 51, "DV")];
    page.maps[0].outside_shares = Some(
        serde_json::from_value(serde_json::json!([{
            "id": id(70),
            "grantor_id": id(100),
            "grantor_nickname": "user100",
            "grantee_id": id(71),
            "grantee_nickname": "ollie",
            "created_at": "2026-10-07T00:00:00Z"
        }]))
        .unwrap(),
    );
    page.packages = vec![indexed("package", 52, "trail-tools")];
    page.secrets = vec![indexed("secret", 53, "Survey route")];
    page.selected_group = Some(id(4));
    page.roster = Some((id(4), Ok(Vec::new())));
    let mut panel = panel_with(page);
    let fills = |panel: &ClanPanel| {
        let element = panel.view();
        element.as_widget().size_hint().height.is_fill()
    };
    for tab in [Tab::Members, Tab::Groups, Tab::Access, Tab::Settings] {
        panel.open.as_mut().unwrap().tab = tab;
        assert!(!fills(&panel), "{tab:?}");
    }
    let page = panel.open.as_mut().unwrap();
    page.tab = Tab::Groups;
    page.group_tab = GroupTab::Members;
    assert!(!fills(&panel), "group members");
    for (access, selected) in [
        (AccessTab::Maps, id(51)),
        (AccessTab::Maps, id(50)),
        (AccessTab::Packages, id(52)),
        (AccessTab::Secrets, id(53)),
    ] {
        let page = panel.open.as_mut().unwrap();
        page.tab = Tab::Access;
        page.access_tab = access;
        page.selected_resource = Some(selected);
        assert!(!fills(&panel), "{access:?}");
    }
    // Each dialog renders.
    for message in [
        Message::OpenInvite,
        Message::EditMemberGroups(id(101)),
        Message::OpenNewGroup,
        Message::OpenEditGroup(id(4)),
        Message::OpenAddGroupMember,
        Message::AddPermission,
        Message::AssignGroup(ScopeKind::Packages, id(52)),
        Message::AssignSecretGroup(id(53)),
    ] {
        let _ = panel.update(message);
        assert!(panel.modal_view().is_some());
        let _ = panel.update(Message::CloseModal);
    }
    assert!(panel.modal_view().is_none());
}

#[test]
fn a_member_owned_maps_grants_are_its_owners_to_change() {
    let mut page = ClanPage::new(summary(true, &[]));
    let mut theirs = indexed("area", 60, "Nessa's DV");
    theirs.ownership = Some("members".to_string());
    let mut mine = indexed("area", 61, "My DV");
    mine.ownership = Some("members".to_string());
    mine.owned_by_me = true;
    page.maps = vec![theirs, mine, indexed("area", 62, "Roads")];
    let on = |map: u128| {
        grant(
            GrantRecipient::Group { group_id: id(4) },
            GrantScope::Areas {
                ids: vec![smudgy_cloud::AreaId(id(map))],
            },
            &["area.read"],
        )
    };
    assert!(
        !page.can_change(&on(60)),
        "a clan owner holds nothing on it"
    );
    assert!(page.can_change(&on(61)), "its owner writes its grants");
    assert!(page.can_change(&on(62)));
    // It is still named, though the editor offers only the clan's own maps.
    let context = EditorContext {
        groups: &page.groups,
        members: None,
        folders: &page.folders,
        maps: &page.maps,
        packages: &page.packages,
        grants: &page.grants,
        owner: true,
    };
    assert_eq!(context.scope_label(&on(60).scope), "Nessa's DV");
}

#[test]
fn a_saved_grant_change_keeps_no_grant_id_for_the_reload() {
    // A delegate's change may answer another grant, or delete the one
    // edited: the editor closes and the page's grants come from a reload.
    let mut page = ClanPage::new(summary(false, &[action::MANAGE_GRANTS]));
    let edited = grant(
        GrantRecipient::Group { group_id: id(5) },
        GrantScope::Clan,
        &["area.read"],
    );
    page.grants = vec![edited.clone()];
    let clan_id = page.clan.id;
    let mut panel = panel_with(page);
    let _ = panel.update(Message::EditGrant(edited.id));
    let Some(Modal::Grant(editor)) = &panel.open.as_ref().unwrap().modal else {
        panic!("Edit opens the editor");
    };
    assert_eq!(editor.editing, Some(edited.id));
    let _ = panel.update(Message::DialogSaved(clan_id, Ok(())));
    assert!(panel.open.as_ref().unwrap().modal.is_none());
}

#[test]
fn a_scope_counts_the_resources_out_of_sight_after_its_names() {
    let mut page = ClanPage::new(summary(true, &[]));
    page.maps = vec![indexed("area", 60, "Roads"), indexed("area", 61, "Grove")];
    let context = EditorContext {
        groups: &page.groups,
        members: None,
        folders: &page.folders,
        maps: &page.maps,
        packages: &page.packages,
        grants: &page.grants,
        owner: true,
    };
    let scope = |maps: &[u128]| GrantScope::Areas {
        ids: maps
            .iter()
            .map(|map| smudgy_cloud::AreaId(id(*map)))
            .collect(),
    };
    let separator = crate::i18n::t!("mapper-multi-list-separator");
    assert_eq!(
        context.scope_label(&scope(&[60, 61])),
        ["Roads", "Grove"].join(&separator)
    );
    assert_eq!(context.scope_label(&scope(&[60, 70, 71])), "Roads, 2 more");
    assert_eq!(context.scope_label(&scope(&[70])), "1 more");
}

/// A grant naming a Member-owned map is edited with only the map actions
/// its owners hold: nothing that is the clan's alone.
#[test]
fn a_member_owned_maps_grant_editor_offers_only_its_map_actions() {
    let mut page = ClanPage::new(summary(true, &[]));
    let mut mine = indexed("area", 61, "My DV");
    mine.ownership = Some("members".to_string());
    mine.owned_by_me = true;
    page.maps = vec![mine];
    let on_mine = grant(
        GrantRecipient::Group { group_id: id(4) },
        GrantScope::Areas {
            ids: vec![smudgy_cloud::AreaId(id(61))],
        },
        &["area.read"],
    );
    page.grants = vec![on_mine.clone()];
    let mut panel = panel_with(page);
    let _ = panel.update(Message::EditGrant(on_mine.id));
    let Some(Modal::Grant(editor)) = panel.open.as_ref().and_then(|page| page.modal.as_ref())
    else {
        panic!("the grant editor opens");
    };
    for action in [
        action::COPY_AREA,
        action::DELETE_AREA,
        action::CREATE_MEMBER_OWNED_SECRET,
    ] {
        assert!(editor.allowed(action));
    }
    assert!(!editor.allowed(action::READ_SECRETS));
    assert!(!editor.allowed(action::MANAGE_GRANTS));
}

/// Edit group changes a group's name and color only with group.rename: a
/// holder of group.delete alone deletes it and saves nothing.
#[test]
fn a_group_is_renamed_only_with_rename() {
    let mut page = ClanPage::new(summary(false, &[]));
    page.groups = vec![group(40, None, None, &[action::DELETE_GROUP])];
    let mut panel = panel_with(page);
    let _ = panel.update(Message::OpenEditGroup(id(40)));
    let _ = panel.update(Message::GroupNameChanged("Renamed".to_string()));
    let _ = panel.update(Message::SaveGroup);
    let page = panel.open.as_ref().unwrap();
    assert!(
        matches!(&page.modal, Some(Modal::Group { error: None, .. })),
        "nothing is saved and the dialog stays"
    );
}
