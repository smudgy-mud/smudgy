//! An atlas's panel: what the inspector shows while a folder is chosen alone
//! in the map list. Its head names the atlas, where it is kept and how many
//! maps it holds, with Rename where the viewer may rename it; below,
//! sections that open and close: its maps, who has access to it, and the
//! servers it shows on; then Delete folder, which asks where its maps go.
//! A clan's folder offers what its actions allow, through the clan's own
//! share and delete dialogs.

use std::collections::BTreeSet;

use iced::alignment::Vertical;
use iced::widget::{Column, button, checkbox, column, row, space, text, text_input};
use iced::{Length, Padding};
use smudgy_cloud::clans::action;
use smudgy_cloud::{AreaId, AtlasId, AtlasListItem};

use crate::theme::Element as ThemedElement;
use crate::theme::builtins;

use super::clan_maps::{self, ClanMessage};
use super::default_atlases;
use super::panels::{self, PanelMessage, Section, muted};
use super::{MapEditorWindow, Message, modals};

/// A map as the Maps section lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasMap {
    pub id: AreaId,
    pub name: String,
    /// The folder the map is filed in.
    pub atlas_id: Option<AtlasId>,
    /// Used to find the player's location (an inactive map dims).
    pub enabled: bool,
}

/// The maps of `atlas_id` among `maps`, by name: those filed in it, and on
/// a clan's folder the maps people put in it (`put_in`).
#[must_use]
pub fn atlas_maps(
    maps: Vec<AtlasMap>,
    atlas_id: AtlasId,
    put_in: impl Fn(AreaId) -> bool,
) -> Vec<AtlasMap> {
    let mut maps: Vec<AtlasMap> = maps
        .into_iter()
        .filter(|map| map.atlas_id == Some(atlas_id) || put_in(map.id))
        .collect();
    maps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    maps
}

/// How many of the listed server entries show the atlas.
#[must_use]
pub fn shown_on(servers: &[String], checked: &BTreeSet<String>) -> usize {
    servers
        .iter()
        .filter(|server| checked.contains(*server))
        .count()
}

/// What the panel offers on an atlas, by whose it is.
#[derive(Clone, Copy)]
enum Kind<'a> {
    /// One of the viewer's own folders (theirs, or one they administer).
    Own,
    /// A clan's folder, with the viewer's actions on it.
    Clan(&'a AtlasListItem),
    /// A folder someone shared with the viewer.
    Shared,
}

/// The maps the window knows in `atlas_id`, session maps left out.
fn maps_in(window: &MapEditorWindow, atlas_id: AtlasId) -> Vec<AtlasMap> {
    let atlas = window.mapper.get_current_atlas();
    let ephemeral = window.mapper.session_area_ids();
    let maps = atlas
        .areas()
        .filter(|area| !ephemeral.contains(area.get_id()))
        .map(|area| AtlasMap {
            id: *area.get_id(),
            name: area.get_name().to_string(),
            atlas_id: area.meta().atlas_id,
            enabled: atlas.is_area_enabled(area.get_id()),
        })
        .collect();
    atlas_maps(maps, atlas_id, |_| false)
}

/// The atlas's name: from the folder list, or for a folder shared with the
/// viewer, as its maps carry it. `None` for an atlas the window doesn't know.
fn atlas_name(window: &MapEditorWindow, atlas_id: AtlasId) -> Option<String> {
    if let Some(folder) = window.atlases.iter().find(|folder| folder.id == atlas_id) {
        return Some(folder.name.clone());
    }
    let atlas = window.mapper.get_current_atlas();
    let map = atlas
        .areas()
        .find(|area| area.meta().atlas_id == Some(atlas_id))?;
    Some(
        map.meta()
            .atlas_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| crate::i18n::t!("area-list-shared-folder")),
    )
}

/// The atlas's panel.
pub fn view(window: &MapEditorWindow, atlas_id: AtlasId) -> Column<'_, Message, crate::Theme> {
    let mut content = Column::new().spacing(8).padding(12);
    // An atlas gone a moment ago leaves the selection on the next tick.
    let Some(name) = atlas_name(window, atlas_id) else {
        return content;
    };
    let kind = match clan_maps::clan_folder(window, atlas_id) {
        Some(folder) => Kind::Clan(folder),
        None if window.own_atlas(atlas_id) => Kind::Own,
        None => Kind::Shared,
    };
    let local = window.local_atlas(atlas_id);
    let maps = maps_in(window, atlas_id);
    let storage = if local {
        smudgy_cloud::MapStorage::Local
    } else {
        smudgy_cloud::MapStorage::Cloud
    };
    let summary = format!(
        "{} \u{00b7} {}",
        panels::storage_label(storage),
        crate::i18n::t!("mapper-multi-maps", "count" => maps.len())
    );
    content = content.push(panels::header(
        crate::i18n::t!("mapper-panel-kind-atlas"),
        name,
        summary,
    ));

    let may_rename = match kind {
        Kind::Own => true,
        Kind::Clan(folder) => folder.can(action::RENAME_ATLAS),
        Kind::Shared => false,
    };
    if let Some((_, draft)) = window
        .panel
        .atlas_rename
        .as_ref()
        .filter(|(renaming, _)| may_rename && *renaming == atlas_id)
    {
        content = content.push(rename_form(draft));
    } else if may_rename {
        content = content.push(
            button(text(crate::i18n::t!("mapper-menu-rename")).size(12))
                .style(builtins::button::subtle)
                .on_press(Message::Panel(PanelMessage::RenameStarted(atlas_id))),
        );
    }

    // Whether the editor's server's new maps go here, on a folder of the
    // viewer's own.
    if let Kind::Own = kind
        && let Some(line) = default_atlases::panel_line(window, atlas_id)
    {
        content = content.push(line);
    }

    content = content.push(panels::section(
        window,
        Section::Maps,
        crate::i18n::t!("mapper-panel-maps"),
        Some(maps.len()),
        || maps_section(window, maps),
    ));

    // Sharing is the owner's on a cloud folder of their own, a clan's
    // through its own dialog.
    match kind {
        Kind::Own if !local => {
            let count = window
                .panel
                .atlas_access
                .as_ref()
                .filter(|access| access.atlas_id == atlas_id)
                .and_then(panels::AtlasAccess::count);
            content = content.push(panels::section(
                window,
                Section::Shares,
                crate::i18n::t!("mapper-panel-shares"),
                count,
                || shares_section(window, atlas_id, Message::ShareAtlasRequested(atlas_id)),
            ));
        }
        Kind::Clan(folder) if folder.can(action::MANAGE_GRANTS) => {
            content = content.push(panels::section(
                window,
                Section::Shares,
                crate::i18n::t!("mapper-panel-shares"),
                None,
                || {
                    shares_section(
                        window,
                        atlas_id,
                        Message::Clan(ClanMessage::ShareFolderRequested(atlas_id)),
                    )
                },
            ));
        }
        Kind::Own | Kind::Clan(_) | Kind::Shared => {}
    }

    // Which servers show it: a per-viewer choice on any cloud folder.
    if !local {
        let checked = window.map_scopes.atlas_entries(&atlas_id);
        content = content.push(panels::section(
            window,
            Section::Servers,
            crate::i18n::t!("mapper-panel-servers"),
            Some(shown_on(&window.panel.servers, &checked)),
            || servers_section(window, atlas_id, &checked),
        ));
    }

    let delete = match kind {
        Kind::Own => Some(Message::DeleteAtlasRequested(atlas_id)),
        Kind::Clan(folder) if folder.can(action::DELETE_ATLAS) => {
            Some(Message::Clan(ClanMessage::DeleteFolderRequested(atlas_id)))
        }
        Kind::Clan(_) | Kind::Shared => None,
    };
    if let Some(delete) = delete {
        content = content.push(
            button(text(crate::i18n::t!("area-list-delete-folder")).size(12))
                .style(builtins::button::secondary)
                .padding(Padding {
                    top: 4.0,
                    bottom: 4.0,
                    left: 10.0,
                    right: 10.0,
                })
                .on_press(delete),
        );
    }
    content
}

/// The new name, with Cancel and Save.
fn rename_form(draft: &str) -> ThemedElement<'_, Message> {
    let ready = !draft.trim().is_empty();
    column![
        text_input(crate::i18n::ts!("mapper-folder-name-placeholder"), draft)
            .size(13)
            .padding([5, 8])
            .on_input(|name| Message::Panel(PanelMessage::RenameChanged(name)))
            .on_submit(Message::Panel(PanelMessage::RenameSubmitted)),
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(12))
                .style(builtins::button::secondary)
                .on_press(Message::Panel(PanelMessage::RenameCancelled)),
            button(text(crate::i18n::t!("action-save")).size(12))
                .style(builtins::button::primary)
                .on_press_maybe(ready.then_some(Message::Panel(PanelMessage::RenameSubmitted))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    ]
    .spacing(6)
    .into()
}

/// The Maps section: each map, opening it on the canvas.
fn maps_section(window: &MapEditorWindow, maps: Vec<AtlasMap>) -> ThemedElement<'_, Message> {
    if maps.is_empty() {
        return panels::note(crate::i18n::t!("mapper-panel-no-maps"));
    }
    let open = window.editor.area_id();
    let mut list = Column::new().spacing(2);
    for map in maps {
        let name = if map.enabled {
            text(map.name).size(13)
        } else {
            text(map.name)
                .size(13)
                .style(|theme: &crate::Theme| text::Style {
                    color: Some(theme.styles.text.normal.scale_alpha(0.4)),
                })
        };
        list = list.push(
            button(
                row![
                    name.width(Length::Fill),
                    text("\u{203A}").size(14).style(muted),
                ]
                .spacing(8)
                .align_y(Vertical::Center),
            )
            .style(if open == Some(map.id) {
                builtins::button::list_item_selected
            } else {
                builtins::button::list_item
            })
            .width(Length::Fill)
            .padding([4, 8])
            .on_press(Message::AreaSelected(map.id)),
        );
    }
    list.into()
}

/// The Shares section: who has access to one of the viewer's own folders,
/// where that is listed, and Share… (`share`) to open the folder's share
/// dialog.
fn shares_section(
    window: &MapEditorWindow,
    atlas_id: AtlasId,
    share: Message,
) -> ThemedElement<'_, Message> {
    let mut content = Column::new().spacing(8);
    if let Some(access) = window
        .panel
        .atlas_access
        .as_ref()
        .filter(|access| access.atlas_id == atlas_id)
    {
        let mut list = column![].spacing(6);
        match &access.grants {
            None => list = list.push(panels::note(crate::i18n::t!("mapper-loading"))),
            Some(Err(error)) => {
                list = list.push(text(error.clone()).size(12).style(builtins::text::danger));
            }
            Some(Ok(grants)) if grants.is_empty() => {
                list = list.push(panels::note(crate::i18n::t!("mapper-not-shared")));
            }
            Some(Ok(grants)) => {
                for row in grants {
                    let grant = &row.grant;
                    let grantee = access
                        .handles
                        .get(&grant.grantee_id)
                        .cloned()
                        .unwrap_or_else(|| grant.grantee_id.to_string());
                    list = list.push(panels::access_row(grantee, modals::grant_badges(grant), 0));
                }
            }
        }
        content = content.push(list);
    }
    content
        .push(
            button(text(crate::i18n::t!("area-list-share-action")).size(12))
                .style(builtins::button::secondary)
                .padding(Padding {
                    top: 4.0,
                    bottom: 4.0,
                    left: 10.0,
                    right: 10.0,
                })
                .on_press(share),
        )
        .into()
}

/// The Servers section: the server entries, each showing the atlas or not,
/// as the folder's Servers dialog sets them.
fn servers_section<'a>(
    window: &'a MapEditorWindow,
    atlas_id: AtlasId,
    checked: &BTreeSet<String>,
) -> ThemedElement<'a, Message> {
    let servers = &window.panel.servers;
    if servers.is_empty() {
        return panels::note(crate::i18n::t!("mapper-no-server-entries"));
    }
    let mut list = Column::new().spacing(6);
    for server in servers {
        let entry = server.clone();
        list = list.push(
            checkbox(checked.contains(server))
                .label(server.clone())
                .size(14)
                .text_size(13)
                .on_toggle(move |show| {
                    Message::Panel(PanelMessage::ServerToggled {
                        atlas_id,
                        entry: entry.clone(),
                        show,
                    })
                }),
        );
    }
    list.push(panels::note(crate::i18n::t!(
        "mapper-unchecked-all-servers"
    )))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    fn atlas(n: u128) -> AtlasId {
        AtlasId(Uuid::from_u128(n))
    }

    fn map(n: u128, name: &str, atlas_id: Option<u128>) -> AtlasMap {
        AtlasMap {
            id: AreaId(Uuid::from_u128(n)),
            name: name.to_string(),
            atlas_id: atlas_id.map(atlas),
            enabled: true,
        }
    }

    fn names(maps: &[AtlasMap]) -> Vec<&str> {
        maps.iter().map(|map| map.name.as_str()).collect()
    }

    #[test]
    fn an_atlas_lists_its_maps_by_name() {
        let maps = vec![
            map(1, "midgaard", Some(10)),
            map(2, "Arena", Some(10)),
            map(3, "Elsewhere", Some(20)),
            map(4, "Loose", None),
        ];
        let listed = atlas_maps(maps, atlas(10), |_| false);
        assert_eq!(names(&listed), ["Arena", "midgaard"]);
    }

    #[test]
    fn a_clan_folder_lists_the_maps_put_in_it() {
        let maps = vec![map(1, "Clan hall", Some(10)), map(2, "Offered", Some(20))];
        let put_in = AreaId(Uuid::from_u128(2));
        let listed = atlas_maps(maps, atlas(10), |area_id| area_id == put_in);
        assert_eq!(names(&listed), ["Clan hall", "Offered"]);
    }

    #[test]
    fn the_servers_count_is_the_entries_showing_the_atlas() {
        let servers = vec!["Arctic".to_string(), "Nuke".to_string(), "Rom".to_string()];
        let checked: BTreeSet<String> = ["Nuke".to_string(), "Gone".to_string()].into();
        // An entry no longer on this device isn't counted.
        assert_eq!(shown_on(&servers, &checked), 1);
        assert_eq!(shown_on(&servers, &BTreeSet::new()), 0);
    }
}
