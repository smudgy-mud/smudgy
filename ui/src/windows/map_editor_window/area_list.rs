//! The area list pane: the viewer's own areas first, then one labeled group
//! per sharer who shared maps with them, with the active area highlighted.
//!
//! Shared rows are grouped by the *sharer's* identity (the friend who handed
//! the map to the viewer), resolved from the received-grants list, falling
//! back to the area's owner. Grouping is keyed on the sharer's user id (a
//! [`Uuid`]) so two handle-less sharers never merge into one group; the
//! display label uses the resolved handle, with "a friend" only as the final
//! display fallback. A re-shared map (sharer differs from owner) gets a
//! subtle "owned by …" badge.

use std::collections::HashMap;

use iced::widget::{
    Column, button, column, container, row, scrollable, space, text, text_input, tooltip,
};
use iced::{Length, alignment::Vertical};
use smudgy_cloud::cloud_api::ShareGrantRow;
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::{AreaId, AtlasId, AtlasListItem, Uuid};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;

use super::{FolderKey, MapEditorWindow, Message};

fn icon_button(
    codepoint: &'static str,
    message: Message,
) -> iced::widget::Button<'static, Message, crate::Theme> {
    button(
        text(codepoint)
            .font(fonts::BOOTSTRAP_ICONS)
            .size(12.0),
    )
    .style(builtins::button::toolbar)
    .on_press(message)
}

/// The friend who shared a map with the viewer, as resolved from the
/// received-grants list. `nickname` is the grantor's nickname
/// read straight off the received grant row (`grantor_nickname`) — *not* a
/// `GET /friends` join — and is `None` only when the server hasn't allocated
/// that user a nickname yet (so grouping must fall back to `user_id`, never the
/// nickname). `owner_nickname` is the original owner's nickname from the same row,
/// used only as a re-share fallback when the area's own `owner_nickname` (from
/// `GET /areas`) is absent.
#[derive(Debug, Clone)]
pub struct Sharer {
    pub user_id: Uuid,
    pub nickname: Option<String>,
    pub owner_nickname: Option<String>,
}

/// Maps shared scopes to their sharer, resolved from the received grants
/// alone. Built once at window construction and refreshed when the mapper's
/// sync revision changes.
///
/// `grantor_nickname` rides on each received row (there is no `GET /friends`
/// join), so a just-revoked friendship can't leave a grant row's sharer
/// unresolvable mid-refresh.
#[derive(Debug, Clone, Default)]
pub struct SharerIndex {
    by_area: HashMap<AreaId, Sharer>,
    by_atlas: HashMap<AtlasId, Sharer>,
}

impl SharerIndex {
    /// Builds the index from the received grants. When several grants cover
    /// the same scope the earliest `created_at` wins, so the result is
    /// deterministic regardless of server ordering.
    #[must_use]
    pub fn build(grants: &[ShareGrantRow]) -> Self {
        // Resolve the earliest-created grant per scope first (avoids naming
        // the chrono timestamp type, which is dev-only here), then map to
        // sharers.
        let mut by_area_grant: HashMap<AreaId, &ShareGrantRow> = HashMap::new();
        let mut by_atlas_grant: HashMap<AtlasId, &ShareGrantRow> = HashMap::new();
        for row in grants {
            match (row.grant.area_id, row.grant.atlas_id) {
                (Some(area_id), _) => {
                    by_area_grant
                        .entry(area_id)
                        .and_modify(|winner| {
                            if row.grant.created_at < winner.grant.created_at {
                                *winner = row;
                            }
                        })
                        .or_insert(row);
                }
                (None, Some(atlas_id)) => {
                    by_atlas_grant
                        .entry(atlas_id)
                        .and_modify(|winner| {
                            if row.grant.created_at < winner.grant.created_at {
                                *winner = row;
                            }
                        })
                        .or_insert(row);
                }
                (None, None) => {}
            }
        }

        let to_sharer = |row: &ShareGrantRow| Sharer {
            user_id: row.grant.grantor_id,
            // The grantor's nickname rides on the received row directly.
            nickname: row.grant.grantor_nickname.clone(),
            owner_nickname: row.grant.owner_nickname.clone(),
        };

        Self {
            by_area: by_area_grant
                .into_iter()
                .map(|(k, row)| (k, to_sharer(row)))
                .collect(),
            by_atlas: by_atlas_grant
                .into_iter()
                .map(|(k, row)| (k, to_sharer(row)))
                .collect(),
        }
    }

    /// The sharer for a shared area: a per-area grant wins over an atlas-scope
    /// grant covering the area's atlas.
    #[must_use]
    pub fn sharer_for(&self, area_id: AreaId, atlas_id: Option<AtlasId>) -> Option<&Sharer> {
        self.by_area
            .get(&area_id)
            .or_else(|| atlas_id.and_then(|atlas_id| self.by_atlas.get(&atlas_id)))
    }
}

pub struct AreaSummary {
    pub id: AreaId,
    pub name: String,
    /// Cached on the [`smudgy_cloud::mapper::area_cache::AreaCache`] at
    /// construction — no per-redraw room scan.
    pub has_secrets: bool,
    /// The viewer owns this area (rename/delete are owner-only).
    pub owned: bool,
    /// The viewer may edit this area (drives the subtle "edit" badge on
    /// shared rows).
    pub can_edit: bool,
    /// The viewer holds effective `can_admin` on this area (drives the
    /// "admin" badge + the owner-or-admin action gating).
    pub can_admin: bool,
    /// Active maps are used to find your location as you play; inactive ones
    /// are skipped (drives the dimmed name + "Inactive" tag + the switch).
    pub enabled: bool,
    /// On a re-shared row, the owner's display handle (when it differs from
    /// the sharer); drives the "owned by …" badge.
    pub reshare_owner: Option<String>,
    /// The sharer's display label, for the re-share tooltip.
    pub sharer_label: Option<String>,
    /// This area is one of ≥2 copies in a copy-family (linked by `copied_from`
    /// provenance or a shared `family_token`); drives the "copy" family badge.
    pub in_family: bool,
}

fn sort_by_name(areas: &mut [AreaSummary]) {
    areas.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// One shared group: keyed on the sharer's user id so handle-less sharers
/// stay distinct, with a display label resolved separately.
struct SharedGroup {
    label: String,
    areas: Vec<AreaSummary>,
}

/// One folder in the "My maps" tree: a named atlas, or the catch-all "Loose"
/// bucket for owned areas filed under no atlas. Empty atlas folders are kept
/// (rendered as empty), so a freshly created folder is visible before any
/// map is filed into it.
pub struct Folder {
    pub key: FolderKey,
    pub label: String,
    pub areas: Vec<AreaSummary>,
}

/// Some area to fall back to when no specific selection is wanted (initial
/// open, after a delete). Owned areas first, then shared, each name-sorted.
#[must_use]
pub fn first_area_id(atlas: &AtlasCache) -> Option<AreaId> {
    let mut owned: Vec<(String, AreaId)> = Vec::new();
    let mut shared: Vec<(String, AreaId)> = Vec::new();
    for area in atlas.areas() {
        let entry = (area.get_name().to_lowercase(), *area.get_id());
        if area.effective_access().is_owner {
            owned.push(entry);
        } else {
            shared.push(entry);
        }
    }
    owned.sort_by(|a, b| a.0.cmp(&b.0));
    shared.sort_by(|a, b| a.0.cmp(&b.0));
    owned.into_iter().chain(shared).map(|(_, id)| id).next()
}

/// Groups the viewer's OWN areas into folders: one per atlas in `atlases`
/// (kept even when empty), plus a trailing "Loose" folder when any owned area
/// is filed under no atlas. Areas whose `atlas_id` is absent from the
/// inventory (a just-deleted folder, or before the inventory has loaded) fall
/// back to Loose until the inventory catches up. Shared rows are excluded —
/// they group by sharer (see [`shared_groups`]).
#[must_use]
pub fn owned_folders(
    atlas: &AtlasCache,
    atlases: &[AtlasListItem],
    family_members: &std::collections::HashSet<AreaId>,
) -> Vec<Folder> {
    let known: std::collections::HashSet<AtlasId> = atlases.iter().map(|item| item.id).collect();
    let mut by_atlas: HashMap<AtlasId, Vec<AreaSummary>> = HashMap::new();
    let mut loose: Vec<AreaSummary> = Vec::new();

    for area in atlas.areas() {
        let access = area.effective_access();
        if !access.is_owner {
            continue;
        }
        let area_id = *area.get_id();
        let atlas_id = area.meta().atlas_id;
        let summary = AreaSummary {
            id: area_id,
            name: area.get_name().to_string(),
            has_secrets: area.has_secrets(),
            owned: true,
            can_edit: access.can_edit,
            can_admin: access.can_admin,
            enabled: atlas.is_area_enabled(&area_id),
            reshare_owner: None,
            sharer_label: None,
            in_family: family_members.contains(&area_id),
        };
        match atlas_id {
            Some(id) if known.contains(&id) => by_atlas.entry(id).or_default().push(summary),
            _ => loose.push(summary),
        }
    }

    let mut inventory: Vec<&AtlasListItem> = atlases.iter().collect();
    inventory.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut folders = Vec::with_capacity(inventory.len() + 1);
    for item in inventory {
        let mut areas = by_atlas.remove(&item.id).unwrap_or_default();
        sort_by_name(&mut areas);
        folders.push(Folder {
            key: FolderKey::Atlas(item.id),
            label: item.name.clone(),
            areas,
        });
    }
    if !loose.is_empty() {
        sort_by_name(&mut loose);
        folders.push(Folder {
            key: FolderKey::Loose,
            label: "Loose maps".to_string(),
            areas: loose,
        });
    }
    folders
}

/// Groups areas shared *to* the viewer by sharer: keyed on
/// the sharer's user id (so handle-less sharers stay distinct), labeled
/// "Shared by {handle}" with "a friend" as the final fallback, name-sorted
/// within each group and across groups.
#[must_use]
pub fn shared_groups(
    atlas: &AtlasCache,
    sharers: Option<&SharerIndex>,
    family_members: &std::collections::HashSet<AreaId>,
) -> Vec<(String, Vec<AreaSummary>)> {
    // Keyed on the sharer's user id (uuid), never the display handle.
    let mut shared: HashMap<Uuid, SharedGroup> = HashMap::new();

    for area in atlas.areas() {
        let access = area.effective_access();
        let meta = area.meta();
        let area_id = *area.get_id();

        if access.is_owner {
            continue;
        }

        // Resolve the sharer; fall back to the area's owner when the index
        // hasn't loaded or doesn't cover this scope.
        let resolved = sharers.and_then(|index| index.sharer_for(area_id, meta.atlas_id));
        let (group_key, sharer_nickname): (Uuid, Option<String>) = match resolved {
            Some(sharer) => (sharer.user_id, sharer.nickname.clone()),
            None => (meta.owner_id.unwrap_or_default(), meta.owner_nickname.clone()),
        };
        let sharer_label = sharer_nickname
            .clone()
            .unwrap_or_else(|| "a friend".to_string());

        // Re-share badge: the displayed sharer differs from the map's owner.
        // Owner handle comes from GET /areas (meta); fall back to the grant's
        // owner_nickname when that's absent, then to "a friend".
        let owner_id = meta.owner_id;
        let reshare_owner = match (resolved, owner_id) {
            (Some(sharer), Some(owner_id)) if sharer.user_id != owner_id => Some(
                meta.owner_nickname
                    .clone()
                    .or_else(|| sharer.owner_nickname.clone())
                    .unwrap_or_else(|| "a friend".to_string()),
            ),
            _ => None,
        };

        let summary = AreaSummary {
            id: area_id,
            name: area.get_name().to_string(),
            has_secrets: area.has_secrets(),
            owned: false,
            can_edit: access.can_edit,
            can_admin: access.can_admin,
            enabled: atlas.is_area_enabled(&area_id),
            reshare_owner,
            sharer_label: Some(sharer_label.clone()),
            in_family: family_members.contains(&area_id),
        };

        shared
            .entry(group_key)
            .or_insert_with(|| SharedGroup {
                label: sharer_label,
                areas: Vec::new(),
            })
            .areas
            .push(summary);
    }

    let mut shared: Vec<SharedGroup> = shared.into_values().collect();
    shared.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.label.cmp(&b.label))
    });

    shared
        .into_iter()
        .map(|mut group| {
            sort_by_name(&mut group.areas);
            (format!("Shared by {}", group.label), group.areas)
        })
        .collect()
}

/// A subtle, dimmed badge text used for inline row annotations.
fn badge<'a>(content: String) -> iced::widget::Text<'a, crate::Theme> {
    text(content)
        .size(10)
        .style(|theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.45)),
        })
}

/// A dimmed group/section header ("My maps", "Shared by …").
fn group_label<'a>(label: String) -> ThemedElement<'a, Message> {
    text(label)
        .size(12)
        .style(|theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.6)),
        })
        .into()
}

/// A small text button for inline row actions ("Move…", "Share…").
fn text_button(
    label: &'static str,
    message: Message,
) -> iced::widget::Button<'static, Message, crate::Theme> {
    button(text(label).size(11))
        .style(builtins::button::toolbar)
        .on_press(message)
}

pub fn view(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let atlas = window.mapper.get_current_atlas();
    let selected = window.editor.area_id();

    let header = row![
        text("Areas").size(14),
        space::horizontal(),
        tooltip(
            icon_button(bootstrap_icons::PLUS_LG, Message::NewAreaRequested),
            "New map",
            tooltip::Position::Bottom,
        ),
        tooltip(
            icon_button(bootstrap_icons::FOLDER_PLUS, Message::NewAtlasRequested),
            "New folder",
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(4)
    .align_y(Vertical::Center)
    .padding(8);

    // Copy-family membership (over copied_from edges + family_token) for the
    // "copy" badge; computed once for the whole list.
    let family_members = window.family_members();
    let folders = owned_folders(&atlas, &window.atlases, &family_members);
    let shared = shared_groups(&atlas, window.sharers.as_ref(), &family_members);
    let has_folders = !window.atlases.is_empty();

    let mut list = Column::new().spacing(2).padding(4);

    if has_folders {
        // "My maps" is the folder tree; only label it when shared groups also
        // appear (otherwise the folders stand on their own).
        if !shared.is_empty() {
            list = list.push(group_label("My maps".to_string()));
        }
        for folder in folders {
            let collapsed = window.collapsed_folders.contains(&folder.key);
            list = list.push(folder_header(window, &folder, collapsed));
            if !collapsed {
                for area in folder.areas {
                    list = list.push(area_row(window, area, selected, true));
                }
            }
        }
    } else {
        // No atlases: keep today's flat owned list (clean single-user view).
        let owned: Vec<AreaSummary> = folders.into_iter().flat_map(|folder| folder.areas).collect();
        if !owned.is_empty() {
            if !shared.is_empty() {
                list = list.push(group_label("My maps".to_string()));
            }
            for area in owned {
                list = list.push(area_row(window, area, selected, false));
            }
        }
    }

    // Shared groups render as flat lists, one per sharer.
    for (label, areas) in shared {
        list = list.push(group_label(label));
        for area in areas {
            list = list.push(area_row(window, area, selected, false));
        }
    }

    container(column![header, scrollable(list).height(Length::Fill)])
        .style(builtins::container::opaque)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// One folder's disclosure header: a chevron, the folder name, a count badge,
/// and (for named atlases) new-map / rename / delete / share affordances.
/// Pressing the row toggles collapse; the nested affordance buttons capture
/// their own clicks. While the folder is being renamed it becomes a text
/// input instead.
fn folder_header<'a>(
    window: &'a MapEditorWindow,
    folder: &Folder,
    collapsed: bool,
) -> ThemedElement<'a, Message> {
    if let Some((renaming_id, name)) = &window.renaming_atlas
        && FolderKey::Atlas(*renaming_id) == folder.key
    {
        return text_input("folder name", name)
            .size(13)
            .on_input(Message::RenameAtlasChanged)
            .on_submit(Message::RenameAtlasCommitted)
            .into();
    }

    // Triangles render in the regular font, sidestepping the icon-font set.
    let disclosure = if collapsed { "\u{25B8}" } else { "\u{25BE}" };
    let count = folder.areas.len();

    let mut header = row![
        text(disclosure)
            .size(10)
            .style(|theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.6)),
            }),
        text(folder.label.clone()).size(13),
    ]
    .spacing(6)
    .align_y(Vertical::Center)
    .width(Length::Fill);

    header = header.push(badge(if count == 0 {
        "empty".to_string()
    } else {
        count.to_string()
    }));
    header = header.push(space::horizontal());

    // The Loose bucket isn't a real atlas — no rename/delete/share/new-map.
    if let FolderKey::Atlas(atlas_id) = folder.key {
        header = header.push(tooltip(
            icon_button(bootstrap_icons::PLUS_LG, Message::NewAreaInAtlas(atlas_id)),
            "New map in folder",
            tooltip::Position::Bottom,
        ));
        header = header.push(tooltip(
            icon_button(bootstrap_icons::PENCIL, Message::RenameAtlasStarted(atlas_id)),
            "Rename folder",
            tooltip::Position::Bottom,
        ));
        header = header.push(tooltip(
            icon_button(bootstrap_icons::TRASH_3, Message::DeleteAtlasRequested(atlas_id)),
            "Delete folder",
            tooltip::Position::Bottom,
        ));
        // Sharing is cloud-only — a local folder has no server identity, so the
        // affordance would always 404. New-map/rename/delete work on both tiers.
        if !window.local_atlas_ids.contains(&atlas_id) {
            header =
                header.push(text_button("Share\u{2026}", Message::ShareAtlasRequested(atlas_id)));
            // Hand the whole folder to a friend (owner-only).
            header = header.push(text_button(
                "Transfer\u{2026}",
                Message::TransferAtlasOwnershipRequested(atlas_id),
            ));
        }
    }

    button(header)
        .style(builtins::button::list_item)
        .on_press(Message::ToggleFolderCollapsed(folder.key))
        .width(Length::Fill)
        .into()
}

/// One area row, shared by the folder tree and the by-sharer groups. Selected
/// owned rows gain the active/inactive switch plus move/rename/delete; the
/// move affordance only appears when there are folders to move into.
fn area_row<'a>(
    window: &'a MapEditorWindow,
    area: AreaSummary,
    selected: Option<AreaId>,
    has_folders: bool,
) -> ThemedElement<'a, Message> {
    let is_selected = Some(area.id) == selected;

    // A row being renamed swaps to a text input; Enter commits, Escape
    // (window-level) cancels.
    if let Some((renaming_id, name)) = &window.renaming_area
        && *renaming_id == area.id
    {
        return text_input("area name", name)
            .size(14)
            .on_input(Message::RenameAreaChanged)
            .on_submit(Message::RenameAreaCommitted)
            .into();
    }

    // Inactive maps grey their name hard so the active/inactive split reads at
    // a glance.
    let name_text = if area.enabled {
        text(area.name).size(14)
    } else {
        text(area.name)
            .size(14)
            .style(|theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.4)),
            })
    };

    let mut item = row![name_text]
        .spacing(4)
        .align_y(Vertical::Center)
        .width(Length::Fill);

    // Inactive maps carry an explicit tag (the dim alone is easy to miss); the
    // tooltip explains what "inactive" means.
    if !area.enabled {
        item = item.push(tooltip(
            badge("Inactive".to_string()),
            "Not used to find your location as you play",
            tooltip::Position::Bottom,
        ));
    }

    if area.has_secrets {
        item = item.push(
            text(super::ICON_LOCK_FILL)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(10.0)
                .style(|theme: &crate::Theme| iced::widget::text::Style {
                    color: Some(theme.styles.text.normal.scale_alpha(0.45)),
                }),
        );
    }

    // Family badge: this map is one of several copies sharing an origin.
    if area.in_family {
        item = item.push(tooltip(
            badge("copy".to_string()),
            "One of several copies of the same map \u{2014} open it to pick the active copy",
            tooltip::Position::Bottom,
        ));
    }

    // Re-share badge: the sharer isn't the map's owner.
    if let Some(owner) = &area.reshare_owner {
        let sharer = area
            .sharer_label
            .clone()
            .unwrap_or_else(|| "a friend".to_string());
        item = item.push(tooltip(
            badge(format!("owned by {owner}")),
            text(format!("Re-shared: {sharer} shared a map owned by {owner}")),
            tooltip::Position::Bottom,
        ));
    }

    // Subtle capability badges on shared rows.
    if !area.owned && area.can_admin {
        item = item.push(badge("admin".to_string()));
    } else if !area.owned && area.can_edit {
        item = item.push(badge("edit".to_string()));
    }

    item = item.push(space::horizontal());

    // The selected row gets an active/inactive switch: the icon shows the
    // current state (switch on = active), the tooltip the action.
    if is_selected {
        let (codepoint, tip) = if area.enabled {
            (bootstrap_icons::TOGGLE_ON, "Active — click to deactivate")
        } else {
            (bootstrap_icons::TOGGLE_OFF, "Inactive — click to activate")
        };
        item = item.push(tooltip(
            icon_button(codepoint, Message::ToggleAreaEnabled(area.id)),
            tip,
            tooltip::Position::Bottom,
        ));
    }

    // rename/delete are gated on "owner OR admin" (server: is_owner OR can_admin).
    // Move (same-owner) and Transfer (raw is_owner) stay owner-only.
    if is_selected && area.owned && has_folders {
        item = item.push(text_button("Move\u{2026}", Message::MoveAreaRequested(area.id)));
    }
    if is_selected && (area.owned || area.can_admin) {
        item = item.push(icon_button(
            bootstrap_icons::PENCIL,
            Message::RenameAreaStarted(area.id),
        ));
        item = item.push(icon_button(
            bootstrap_icons::TRASH_3,
            Message::DeleteAreaRequested(area.id),
        ));
    }
    if is_selected && area.owned {
        item = item.push(text_button(
            "Transfer\u{2026}",
            Message::TransferAreaOwnershipRequested(area.id),
        ));
    }

    button(item)
        .style(if is_selected {
            builtins::button::list_item_selected
        } else {
            builtins::button::list_item
        })
        .on_press(Message::AreaSelected(area.id))
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use smudgy_cloud::cloud_api::ShareGrant;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn grant_row(
        grantor: Uuid,
        owner: Uuid,
        area_id: AreaId,
        created_offset_secs: i64,
    ) -> ShareGrantRow {
        ShareGrantRow {
            grant: ShareGrant {
                id: uuid(900 + created_offset_secs as u128),
                owner_id: owner,
                grantor_id: grantor,
                grantee_id: uuid(1),
                area_id: Some(area_id),
                atlas_id: None,
                can_edit: false,
                can_reshare: false,
                can_copy: false,
                include_secrets: false,
                can_admin: false,
                parent_grant_id: None,
                created_at: Utc::now() + Duration::seconds(created_offset_secs),
                updated_at: Utc::now(),
                grantor_nickname: None,
                owner_nickname: None,
            },
            depth: 0,
        }
    }

    #[test]
    fn handle_resolves_from_grant_row() {
        let grantor = uuid(10);
        let area = AreaId(uuid(100));
        // The grantor handle rides on the received row — no friends join.
        let mut row = grant_row(grantor, grantor, area, 0);
        row.grant.grantor_nickname = Some("wbk".to_string());
        let index = SharerIndex::build(&[row]);
        let sharer = index.sharer_for(area, None).expect("sharer");
        assert_eq!(sharer.user_id, grantor);
        assert_eq!(sharer.nickname.as_deref(), Some("wbk"));
    }

    #[test]
    fn missing_grantor_handle_leaves_handle_none() {
        // Handle absence must fall back to user_id grouping, never merge.
        let grantor = uuid(10);
        let area = AreaId(uuid(100));
        let index = SharerIndex::build(&[grant_row(grantor, grantor, area, 0)]);
        let sharer = index.sharer_for(area, None).expect("sharer");
        assert_eq!(sharer.user_id, grantor);
        assert!(sharer.nickname.is_none());
    }

    #[test]
    fn earliest_grant_wins_for_same_scope() {
        let early_grantor = uuid(10);
        let late_grantor = uuid(11);
        let area = AreaId(uuid(100));
        // Feed the later grant first; the earlier created_at must still win.
        let index = SharerIndex::build(&[
            grant_row(late_grantor, late_grantor, area, 50),
            grant_row(early_grantor, early_grantor, area, 5),
        ]);
        let sharer = index.sharer_for(area, None).expect("sharer");
        assert_eq!(sharer.user_id, early_grantor);
    }

    #[test]
    fn per_area_grant_beats_atlas_scope() {
        let area_grantor = uuid(10);
        let atlas_grantor = uuid(11);
        let area = AreaId(uuid(100));
        let atlas = AtlasId(uuid(200));

        let mut area_grant = grant_row(area_grantor, area_grantor, area, 0);
        let mut atlas_grant = grant_row(atlas_grantor, atlas_grantor, area, 0);
        atlas_grant.grant.area_id = None;
        atlas_grant.grant.atlas_id = Some(atlas);
        area_grant.grant.atlas_id = None;

        let index = SharerIndex::build(&[area_grant, atlas_grant]);
        let sharer = index.sharer_for(area, Some(atlas)).expect("sharer");
        assert_eq!(sharer.user_id, area_grantor);
    }
}
