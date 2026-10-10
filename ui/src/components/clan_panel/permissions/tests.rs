use super::*;
use serde_json::json;
use smudgy_cloud::clan_access::IndexedResource;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn fixture() -> (ClanPage, ClanGroup) {
    let mut page = ClanPage::new(
        serde_json::from_value(json!({
            "id": id(1), "name": "Lantern Company", "description": null,
            "created_at": "2026-10-07T00:00:00Z", "member_count": 3,
            "is_owner": true, "group_ids": [id(2)], "actions": []
        }))
        .unwrap(),
    );
    let group: ClanGroup = serde_json::from_value(json!({
        "id": id(3), "name": "Cartographers", "color": null,
        "builtin": null, "is_member": false, "actions": []
    }))
    .unwrap();
    let resource = |kind: &str, n, name: &str| -> IndexedResource {
        serde_json::from_value(json!({"kind": kind, "id": id(n), "name": name,
            "owner": {"kind": "clan"}, "ownership": "clan", "actions": ["grant.manage"]}))
        .unwrap()
    };
    page.groups = vec![group.clone()];
    page.folders = vec![resource("atlas", 10, "Roads and towns")];
    let mut map = resource("area", 11, "Eastern roads");
    map.atlas_id = Some(smudgy_cloud::AtlasId(id(10)));
    page.maps = vec![map, resource("area", 12, "Unfiled map")];
    page.packages = vec![resource("package", 13, "travel-tools")];
    (page, group)
}

fn grant(scope: GrantScope, actions: &[&str], ceiling: Option<&[&str]>) -> ClanGrant {
    serde_json::from_value(json!({"id": Uuid::new_v4(), "clan_id": id(1),
        "recipient": {"group_id": id(3)}, "scope": scope, "actions": actions,
        "may_grant": ceiling, "issuer_id": id(2), "parent_id": null,
        "created_at": "2026-10-07T00:00:00Z", "updated_at": "2026-10-07T00:00:00Z"
    }))
    .unwrap()
}

fn select(editor: &mut Permissions, page: &ClanPage, kind: ScopeKind, n: u128) -> Target {
    let target = Target {
        kind,
        id: Some(id(n)),
    };
    editor.update(
        Message::Tab(if kind == ScopeKind::Packages {
            Tab::Packages
        } else {
            Tab::Maps
        }),
        page,
    );
    editor.update(Message::Select(target), page);
    target
}

#[test]
fn opening_duplicate_delegations_never_merges_their_ceilings() {
    let (mut page, group) = fixture();
    let scope = GrantScope::Packages { ids: vec![id(13)] };
    page.grants = vec![
        grant(
            scope.clone(),
            &[action::MANAGE_GRANTS],
            Some(&["package.read"]),
        ),
        grant(scope, &[action::MANAGE_GRANTS], Some(&["package.publish"])),
    ];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    select(&mut editor, &page, ScopeKind::Packages, 13);
    assert!(editor.writes(&page).is_empty());
    editor.update(Message::Toggle("package.edit_draft", true), &page);
    assert!(
        matches!(editor.writes(&page).as_slice(), [Write::Create(GrantScope::Packages {..}, body)] if body.actions == ["package.edit_draft"] && body.may_grant.is_none())
    );
}

#[test]
fn narrowing_one_delegation_does_not_give_it_another_delegations_actions() {
    let (mut page, group) = fixture();
    let scope = GrantScope::Packages { ids: vec![id(13)] };
    let first = grant(
        scope.clone(),
        &[action::MANAGE_GRANTS],
        Some(&["package.read"]),
    );
    let second = grant(scope, &[action::MANAGE_GRANTS], Some(&["package.publish"]));
    page.grants = vec![first, second.clone()];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    select(&mut editor, &page, ScopeKind::Packages, 13);
    editor.update(Message::Ceiling("package.publish", false), &page);
    assert!(
        matches!(editor.writes(&page).as_slice(), [Write::Patch(key, body)]
        if *key == second.id && body.may_grant == Some(Vec::new()))
    );
}

#[test]
fn edits_stay_on_the_selected_resource_and_preserve_unknown_actions() {
    let (mut page, group) = fixture();
    let scope = GrantScope::Areas {
        ids: vec![smudgy_cloud::AreaId(id(11))],
    };
    let original = grant(scope, &["area.read", "area.future"], None);
    page.grants = vec![original.clone()];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    editor.update(Message::Toggle("area.read", true), &page);
    assert!(
        editor.writes(&page).is_empty(),
        "no global resource permission"
    );
    select(&mut editor, &page, ScopeKind::Maps, 11);
    editor.update(Message::Toggle("area.read", false), &page);
    select(&mut editor, &page, ScopeKind::Maps, 12);
    editor.update(Message::Toggle("area.read", true), &page);
    let writes = editor.writes(&page);
    assert!(
        matches!(&writes[0], Write::Patch(key, body) if *key == original.id && body.actions == ["area.future"])
    );
    assert!(
        matches!(&writes[1], Write::Create(GrantScope::Areas {ids}, body) if ids == &[smudgy_cloud::AreaId(id(12))] && body.actions == ["area.read"])
    );
}

#[test]
fn broader_grants_cannot_be_silently_removed_from_one_resource() {
    let (mut page, group) = fixture();
    page.grants = vec![
        grant(
            GrantScope::Atlases {
                ids: vec![smudgy_cloud::AtlasId(id(10))],
            },
            &["area.read"],
            None,
        ),
        grant(
            GrantScope::Areas {
                ids: vec![smudgy_cloud::AreaId(id(11)), smudgy_cloud::AreaId(id(12))],
            },
            &["area.edit"],
            None,
        ),
    ];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    select(&mut editor, &page, ScopeKind::Maps, 11);
    for action in ["area.read", "area.edit"] {
        editor.update(Message::Toggle(action, false), &page);
    }
    assert!(editor.writes(&page).is_empty());
}

#[test]
fn pending_folder_changes_update_inheritance_without_overwriting_direct_map_access() {
    let (mut page, group) = fixture();
    page.grants = vec![grant(
        GrantScope::Atlases {
            ids: vec![smudgy_cloud::AtlasId(id(10))],
        },
        &["area.read"],
        None,
    )];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    select(&mut editor, &page, ScopeKind::Folders, 10);
    editor.update(Message::Toggle("area.read", false), &page);
    editor.update(Message::Toggle("area.edit", true), &page);
    let map = select(&mut editor, &page, ScopeKind::Maps, 11);
    let inherited = editor.inherited(map, &page);
    assert!(
        inherited
            .iter()
            .any(|(_, actions, _)| actions.contains("area.edit"))
    );
    assert!(
        inherited
            .iter()
            .all(|(_, actions, _)| !actions.contains("area.read"))
    );
    editor.update(Message::Toggle("area.read", true), &page);
    assert!(editor.writes(&page).iter().any(|write| matches!(write, Write::Create(GrantScope::Areas {..}, body) if body.actions == ["area.read"])));
}

#[test]
fn owner_authority_excludes_member_owned_maps_and_delegates_stay_bounded() {
    let (mut page, mut group) = fixture();
    group.builtin = Some("owners".into());
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    let clan_map = select(&mut editor, &page, ScopeKind::Maps, 11);
    assert!(editor.implicit(clan_map, &page));
    editor.update(Message::Toggle("area.read", false), &page);
    assert!(editor.writes(&page).is_empty());
    page.maps[0].ownership = Some("members".into());
    assert!(!editor.implicit(clan_map, &page));
    assert!(!editor.allowed(clan_map, "area.read", &page));
    page.maps[0].owned_by_me = true;
    assert!(editor.allowed(clan_map, "area.read", &page));
    assert!(!editor.allowed(clan_map, "secret.read", &page));
    group.builtin = None;
    page.clan.is_owner = false;
    page.clan.group_ids = vec![group.id];
    page.grants = vec![grant(
        GrantScope::Packages { ids: vec![id(13)] },
        &[action::MANAGE_GRANTS],
        Some(&["package.read"]),
    )];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    let package = select(&mut editor, &page, ScopeKind::Packages, 13);
    assert!(editor.allowed(package, "package.read", &page));
    assert!(!editor.allowed(package, "package.publish", &page));
    editor.update(Message::Ceiling("package.read", false), &page);
    assert!(
        editor.writes(&page).is_empty(),
        "delegates cannot change a ceiling"
    );
}

#[test]
fn member_map_edits_keep_the_read_permission_the_server_requires() {
    let (mut page, group) = fixture();
    page.maps[0].ownership = Some("members".into());
    page.maps[0].owned_by_me = true;
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    let target = select(&mut editor, &page, ScopeKind::Maps, 11);
    editor.update(Message::Toggle("area.edit", true), &page);
    editor.update(Message::Toggle("area.read", false), &page);
    assert!(editor.drafts[&target].actions.contains("area.read"));
    assert!(
        matches!(editor.writes(&page).as_slice(), [Write::Create(_, body)]
        if body.actions == ["area.edit", "area.read"])
    );
    editor.update(Message::Toggle("area.edit", false), &page);
    editor.update(Message::Toggle("area.read", false), &page);
    assert!(editor.writes(&page).is_empty());
}

#[test]
fn separate_delegations_create_separately_bounded_grants() {
    let (mut page, group) = fixture();
    page.clan.is_owner = false;
    let scope = GrantScope::Packages { ids: vec![id(13)] };
    for permission in ["package.read", "package.publish"] {
        let mut held = grant(scope.clone(), &[action::MANAGE_GRANTS], Some(&[permission]));
        held.recipient = GrantRecipient::User { user_id: id(2) };
        page.grants.push(held);
    }
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    select(&mut editor, &page, ScopeKind::Packages, 13);
    editor.update(Message::Toggle("package.read", true), &page);
    editor.update(Message::Toggle("package.publish", true), &page);
    let writes = editor.writes(&page);
    assert_eq!(writes.len(), 2);
    let mut actions = BTreeSet::new();
    for write in writes {
        let Write::Create(_, body) = write else {
            panic!("expected a new grant")
        };
        assert_eq!(body.actions.len(), 1, "each request must fit one ceiling");
        assert!(body.may_grant.is_none());
        actions.extend(body.actions);
    }
    assert_eq!(
        actions,
        BTreeSet::from(["package.read".into(), "package.publish".into()])
    );
}

#[test]
fn a_delegate_cannot_toggle_part_of_an_unmanageable_grant() {
    let (mut page, group) = fixture();
    page.clan.is_owner = false;
    let scope = GrantScope::Packages { ids: vec![id(13)] };
    let mut held = grant(
        scope.clone(),
        &[action::MANAGE_GRANTS],
        Some(&["package.read"]),
    );
    held.recipient = GrantRecipient::User { user_id: id(2) };
    page.grants = vec![
        held,
        grant(scope, &["package.read", "package.publish"], None),
    ];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    let target = select(&mut editor, &page, ScopeKind::Packages, 13);
    assert!(!editor.allowed(target, "package.read", &page));
    editor.update(Message::Toggle("package.read", false), &page);
    assert!(editor.writes(&page).is_empty());
}

#[test]
fn refresh_preserves_only_our_changes_and_retry_does_not_duplicate_writes() {
    let (mut page, group) = fixture();
    let scope = GrantScope::Packages { ids: vec![id(13)] };
    let original = grant(scope.clone(), &["package.read"], None);
    page.grants = vec![original];
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    select(&mut editor, &page, ScopeKind::Packages, 13);
    editor.update(Message::Toggle("package.read", false), &page);
    editor.update(Message::Toggle("package.publish", true), &page);
    // The server applied the addition before another request failed, and
    // another owner independently granted metadata access.
    page.grants
        .push(grant(scope.clone(), &["package.publish"], None));
    page.grants
        .push(grant(scope, &["package.edit_metadata"], None));
    editor.refresh(&page);
    assert!(matches!(
        editor.writes(&page).as_slice(),
        [Write::Delete(_)]
    ));
}

#[tokio::test]
async fn the_three_tabs_fit_narrow_and_wide_dialogs() {
    use crate::assets::fonts;
    use iced::advanced::{
        Layout,
        layout::Limits,
        renderer::{Headless, Style},
        widget::Tree,
    };
    use iced::{Rectangle, Size, mouse};
    iced_graphics::text::font_system()
        .write()
        .unwrap()
        .load_font(fonts::GEIST_VF_BYTES.into());
    let (page, group) = fixture();
    let mut editor = Permissions::new(&group, &page, Some(id(2)));
    for tab in [Tab::Clan, Tab::Maps, Tab::Packages] {
        editor.update(Message::Tab(tab), &page);
        if tab == Tab::Maps {
            select(&mut editor, &page, ScopeKind::Folders, 10);
        }
        if tab == Tab::Packages {
            select(&mut editor, &page, ScopeKind::Packages, 13);
        }
        editor.update(
            Message::Toggle(
                match tab {
                    Tab::Clan => action::READ_MEMBERS,
                    Tab::Maps => action::READ_AREA,
                    Tab::Packages => action::READ_PACKAGE,
                },
                true,
            ),
            &page,
        );
        for (width, height) in [(640, 500), (940, 760)] {
            let mut renderer =
                <iced::Renderer as Headless>::new(fonts::GEIST_VF, 16.0.into(), Some("tiny-skia"))
                    .await
                    .unwrap();
            let mut element = view(&editor, &page);
            let messages = crate::widgets::dialog::tests::check_actions(
                view(&editor, &page),
                (width, height),
                &[
                    crate::i18n::t!("action-cancel"),
                    crate::i18n::t!("action-save"),
                ],
                &format!("group-permissions-{tab:?}"),
            )
            .await;
            assert_eq!(
                messages
                    .iter()
                    .filter(|m| matches!(m, PanelMessage::SaveGroupPermissions))
                    .count(),
                2
            );
            let mut tree = Tree::new(element.as_widget());
            let size = Size::new(width as f32, height as f32);
            let layout = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &Limits::new(Size::ZERO, size),
            );
            assert_eq!(layout.size(), size);
            let mut messages = Vec::new();
            element.as_widget_mut().update(
                &mut tree,
                &iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
            let theme = theme::smudgy();
            element.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &Style {
                    text_color: theme.styles.text.normal,
                },
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(size),
            );
            let pixels = renderer.screenshot(
                Size::new(width, height),
                1.0,
                theme.styles.general.background,
            );
            assert_eq!(pixels.len(), width as usize * height as usize * 4);
            if let Some(path) = std::env::var_os("SMUDGY_PERMISSION_SCREENSHOTS") {
                let path = std::path::PathBuf::from(path);
                std::fs::create_dir_all(&path).unwrap();
                image::save_buffer(
                    path.join(format!("permissions-{tab:?}-{width}.png")),
                    &pixels,
                    width,
                    height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}
