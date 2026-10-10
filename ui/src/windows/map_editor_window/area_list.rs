//! The area list pane, a three-level tree. The top level is where maps come
//! from: the viewer's own maps on this device ("My local maps") and in the
//! cloud ("My shared maps"), each of their clans, one "Shared by" group per
//! sharer, and session maps. Folders sit one step in under their group, and
//! maps one step further in under their folder. The active area is
//! highlighted. A map shows in every folder holding it; "Shared by" leaves
//! out maps in a clan folder.
//!
//! Shared rows are grouped by the *sharer's* identity (the friend who handed
//! the map to the viewer), resolved from the received-grants list, falling
//! back to the area's owner. Grouping is keyed on the sharer's user id (a
//! [`Uuid`]) so two handle-less sharers never merge into one group; the
//! display label uses the resolved handle, with "a friend" only as the final
//! display fallback. A re-shared map (sharer differs from owner) gets a
//! subtle "owned by …" badge.

use std::collections::{HashMap, HashSet};

use iced::widget::{
    Column, button, column, container, row, scrollable, space, text, text_input, tooltip,
};
use iced::{Length, Padding, alignment::Vertical};
use smudgy_cloud::cloud_api::ShareGrantRow;
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{AreaId, AtlasId, AtlasListItem, MapStorage, Uuid};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::widgets::dropdown::Dropdown;

use smudgy_core::models::map_scopes::ScopeState;

use super::clan_maps::{self, ClanGroup};
use super::default_atlases;
use super::multi_select::{ListItem, MultiMessage};
use super::{FolderKey, MapEditorWindow, Message, ScopeTarget};

fn icon_button(
    codepoint: &'static str,
    message: Message,
) -> iced::widget::Button<'static, Message, crate::Theme> {
    button(text(codepoint).font(fonts::BOOTSTRAP_ICONS).size(12.0))
        .style(builtins::button::toolbar)
        .on_press(message)
}

/// How deep a row sits in the tree: a group heads its folders, a folder its
/// maps. Maps listed straight under a group (session maps, a sharer's maps in
/// no folder) sit at folder depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Group,
    Folder,
    Map,
}

/// The left inset every row's content starts at: a list row's own padding.
const ROW_INSET: f32 = button::DEFAULT_PADDING.left;

/// How far each level of the tree steps in.
const INDENT_STEP: f32 = 14.0;

/// Around a folder's chevron, pressable or not, so every folder's name
/// starts at the same place.
const CHEVRON_PADDING: Padding = Padding {
    top: 0.0,
    right: 2.0,
    bottom: 0.0,
    left: 2.0,
};

impl Depth {
    /// Where the row's content starts.
    fn inset(self) -> f32 {
        let steps = match self {
            Self::Group => 0.0,
            Self::Folder => 1.0,
            Self::Map => 2.0,
        };
        ROW_INSET + INDENT_STEP * steps
    }

    /// A list row's padding at this depth. The indent is inside the row, so
    /// a chosen row's highlight still spans the pane.
    fn row_padding(self) -> Padding {
        button::DEFAULT_PADDING.left(self.inset())
    }

    /// The padding around an inline rename input, whose text starts after
    /// the input's own padding.
    fn input_padding(self) -> Padding {
        Padding::ZERO.left(self.inset() - text_input::DEFAULT_PADDING.left)
    }
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

/// One folder in the tree: a named atlas, or the "Not in a folder" bucket
/// holding the viewer's own maps whose folder the inventory doesn't list
/// (yet). Empty atlas folders are kept (rendered as empty), so a freshly
/// created folder is visible before any map is filed into it. `owned`
/// distinguishes the viewer's own folders (full affordances) from a shared
/// atlas folder surfaced under a sharer group (only the per-user "Servers…"
/// override).
pub struct Folder {
    pub key: FolderKey,
    pub label: String,
    pub areas: Vec<AreaSummary>,
    /// The viewer owns this folder (drives owner-only header affordances).
    pub owned: bool,
}

/// The shared maps handed to the viewer by one sharer: the owner's named atlas
/// folders (only the granted areas inside — §4.1 un-redaction) plus a flat
/// pile of any genuinely atlas-less shared areas. Keyed on the sharer's user
/// id so handle-less sharers stay distinct, with a display label resolved
/// separately.
pub struct SharedGroup {
    pub label: String,
    pub folders: Vec<Folder>,
    pub loose: Vec<AreaSummary>,
}

/// Match map and atlas names, keeping a matching map's parent and all the
/// maps in a matching atlas. Synthetic buckets are not atlas names.
fn filter_folders(folders: &mut Vec<Folder>, query: &str) {
    if query.is_empty() {
        return;
    }
    folders.retain_mut(|folder| {
        if matches!(folder.key, FolderKey::Atlas(_)) && folder.label.to_lowercase().contains(query)
        {
            return true;
        }
        filter_areas(&mut folder.areas, query);
        !folder.areas.is_empty()
    });
}

fn filter_areas(areas: &mut Vec<AreaSummary>, query: &str) {
    if !query.is_empty() {
        areas.retain(|area| area.name.to_lowercase().contains(query));
    }
}

/// Some area to fall back to when no specific selection is wanted (initial
/// open, after a delete). The viewer's own areas first, then the rest
/// (shared and clans' maps), each name-sorted. Ephemeral (session) areas are
/// excluded — the editor doesn't manage them.
#[must_use]
pub fn first_area_id(
    atlas: &AtlasCache,
    ephemeral: &std::collections::HashSet<AreaId>,
) -> Option<AreaId> {
    let mut owned: Vec<(String, AreaId)> = Vec::new();
    let mut shared: Vec<(String, AreaId)> = Vec::new();
    for area in atlas.areas() {
        if ephemeral.contains(area.get_id()) {
            continue;
        }
        let entry = (area.get_name().to_lowercase(), *area.get_id());
        if is_own_map(&area) {
            owned.push(entry);
        } else {
            shared.push(entry);
        }
    }
    owned.sort_by(|a, b| a.0.cmp(&b.0));
    shared.sort_by(|a, b| a.0.cmp(&b.0));
    owned.into_iter().chain(shared).map(|(_, id)| id).next()
}

/// Whether `area` is one of the viewer's own maps: owned, and not a clan's,
/// which lists under its clan.
fn is_own_map(area: &AreaCache) -> bool {
    area.is_owned() && area.meta().clan_id.is_none()
}

/// One of the viewer's own maps, as the owned grouping reads it.
pub struct OwnedMap {
    pub summary: AreaSummary,
    pub atlas_id: Option<AtlasId>,
    /// The folder's name as the map carries it (cloud maps do).
    pub atlas_name: Option<String>,
    /// Stored on this device rather than in the cloud.
    pub local: bool,
}

/// The viewer's own folders, split by where they are stored: on this device
/// ("My local maps") or in the cloud, where they can be shared ("My shared
/// maps").
#[derive(Default)]
pub struct OwnedGroups {
    pub local: Vec<Folder>,
    pub shared: Vec<Folder>,
}

/// Groups the viewer's OWN areas into their folders, split into local and
/// shared. Shared rows are excluded — they group by sharer (see
/// [`shared_groups`]). Ephemeral (session) areas are excluded too: the
/// editor's tree only shows maps that outlive the session, so a session map
/// can't be toggled into the per-area preference lists or picked as a folder
/// member.
#[must_use]
pub fn owned_groups(
    atlas: &AtlasCache,
    atlases: &[AtlasListItem],
    local_atlases: &HashSet<AtlasId>,
    local_areas: &HashSet<AreaId>,
    family_members: &HashSet<AreaId>,
    ephemeral: &HashSet<AreaId>,
) -> OwnedGroups {
    let maps = atlas
        .areas()
        .filter(|area| is_own_map(area) && !ephemeral.contains(area.get_id()))
        .map(|area| {
            let access = area.effective_access();
            let area_id = *area.get_id();
            OwnedMap {
                summary: AreaSummary {
                    id: area_id,
                    name: area.get_name().to_string(),
                    owned: true,
                    can_edit: access.can_edit,
                    can_admin: access.can_admin,
                    enabled: atlas.is_area_enabled(&area_id),
                    reshare_owner: None,
                    sharer_label: None,
                    in_family: family_members.contains(&area_id),
                },
                atlas_id: area.meta().atlas_id,
                atlas_name: area.meta().atlas_name.clone(),
                local: local_areas.contains(&area_id),
            }
        })
        .collect();
    group_owned(maps, atlases, local_atlases)
}

/// [`owned_groups`] over plain rows: one folder per non-clan atlas in
/// `atlases` (kept even when empty), in the group its storage puts it in.
/// A map filed in a folder the inventory doesn't list yet (it hasn't loaded)
/// shows in that folder under the name the map carries. A map in no folder,
/// or one whose folder neither names, waits in a trailing "Not in a folder"
/// bucket of its own storage's group.
#[must_use]
pub fn group_owned(
    maps: Vec<OwnedMap>,
    atlases: &[AtlasListItem],
    local_atlases: &HashSet<AtlasId>,
) -> OwnedGroups {
    // Each folder: its id, name, and whether it is on this device. Clan
    // folders list under their clan, never under the viewer's own maps.
    let mut folders: Vec<(AtlasId, String, bool)> = atlases
        .iter()
        .filter(|item| item.clan_id.is_none())
        .map(|item| (item.id, item.name.clone(), local_atlases.contains(&item.id)))
        .collect();
    let known: HashSet<AtlasId> = folders.iter().map(|(id, _, _)| *id).collect();
    let mut by_atlas: HashMap<AtlasId, Vec<AreaSummary>> = HashMap::new();
    let mut unfiled_local: Vec<AreaSummary> = Vec::new();
    let mut unfiled_shared: Vec<AreaSummary> = Vec::new();

    for map in maps {
        match map.atlas_id {
            Some(atlas_id) if known.contains(&atlas_id) => {
                by_atlas.entry(atlas_id).or_default().push(map.summary);
            }
            Some(atlas_id) if map.atlas_name.is_some() => {
                if !by_atlas.contains_key(&atlas_id) {
                    folders.push((atlas_id, map.atlas_name.unwrap_or_default(), map.local));
                }
                by_atlas.entry(atlas_id).or_default().push(map.summary);
            }
            _ if map.local => unfiled_local.push(map.summary),
            _ => unfiled_shared.push(map.summary),
        }
    }

    folders.sort_by(|(a_id, a, _), (b_id, b, _)| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
            .then_with(|| a_id.0.cmp(&b_id.0))
    });

    let mut groups = OwnedGroups::default();
    for (id, label, local) in folders {
        let mut areas = by_atlas.remove(&id).unwrap_or_default();
        sort_by_name(&mut areas);
        let folder = Folder {
            key: FolderKey::Atlas(id),
            label,
            areas,
            owned: true,
        };
        if local {
            groups.local.push(folder);
        } else {
            groups.shared.push(folder);
        }
    }
    for (storage, mut areas, group) in [
        (MapStorage::Local, unfiled_local, &mut groups.local),
        (MapStorage::Cloud, unfiled_shared, &mut groups.shared),
    ] {
        if areas.is_empty() {
            continue;
        }
        sort_by_name(&mut areas);
        group.push(Folder {
            key: FolderKey::Unfiled(storage),
            label: crate::i18n::t!("area-list-not-in-folder"),
            areas,
            owned: true,
        });
    }
    groups
}

/// The viewer's ephemeral (session) areas, name-sorted. These live only for
/// the session and are never persisted, so they never appear in the atlas
/// folder tree — but the editor lists them under their own group so a
/// protocol-driven auto-map can be inspected (and promoted) while it builds.
#[must_use]
pub fn session_maps(
    atlas: &AtlasCache,
    ephemeral: &std::collections::HashSet<AreaId>,
) -> Vec<AreaSummary> {
    let mut maps: Vec<AreaSummary> = atlas
        .areas()
        .filter(|area| ephemeral.contains(area.get_id()))
        .map(|area| {
            let area_id = *area.get_id();
            AreaSummary {
                id: area_id,
                name: area.get_name().to_string(),
                owned: true,
                can_edit: true,
                can_admin: true,
                enabled: atlas.is_area_enabled(&area_id),
                reshare_owner: None,
                sharer_label: None,
                in_family: false,
            }
        })
        .collect();
    sort_by_name(&mut maps);
    maps
}

/// Groups areas shared *to* the viewer by sharer, keyed on the sharer's user
/// id (so handle-less sharers stay distinct), labeled "Shared by {handle}"
/// with "a friend" as the final fallback. Within each sharer group the areas
/// are further grouped by their atlas into named folders (§4.1 un-redaction now
/// delivers `atlas_id` + `atlas_name` to any viewer who can see the area), with
/// genuinely atlas-less areas left in a flat pile. Name-sorted within each
/// folder/pile and across groups. Maps in the viewer's clan folders
/// are left out: they list under their clan.
#[must_use]
pub fn shared_groups(
    atlas: &AtlasCache,
    sharers: Option<&SharerIndex>,
    family_members: &std::collections::HashSet<AreaId>,
    in_clan_folders: &std::collections::HashSet<AreaId>,
) -> Vec<SharedGroup> {
    // The accumulating shape of one sharer group before folders are ordered:
    // atlas-id -> (name, areas), plus the atlas-less pile.
    struct Accum {
        label: String,
        by_atlas: HashMap<AtlasId, (String, Vec<AreaSummary>)>,
        loose: Vec<AreaSummary>,
    }

    // Keyed on the sharer's user id (uuid), never the display handle.
    let mut shared: HashMap<Uuid, Accum> = HashMap::new();

    for area in atlas.areas() {
        let access = area.effective_access();
        let meta = area.meta();
        let area_id = *area.get_id();

        // A map in one of the viewer's clan folders lists under its clan.
        if access.is_owner || in_clan_folders.contains(&area_id) {
            continue;
        }

        // Resolve the sharer; fall back to the area's owner when the index
        // hasn't loaded or doesn't cover this scope.
        let resolved = sharers.and_then(|index| index.sharer_for(area_id, meta.atlas_id));
        let (group_key, sharer_nickname): (Uuid, Option<String>) = match resolved {
            Some(sharer) => (sharer.user_id, sharer.nickname.clone()),
            None => (
                meta.owner_id.unwrap_or_default(),
                meta.owner_nickname.clone(),
            ),
        };
        let sharer_label = sharer_nickname
            .clone()
            .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));

        // Re-share badge: the displayed sharer differs from the map's owner.
        // Owner handle comes from GET /areas (meta); fall back to the grant's
        // owner_nickname when that's absent, then to "a friend".
        let owner_id = meta.owner_id;
        let reshare_owner = match (resolved, owner_id) {
            (Some(sharer), Some(owner_id)) if sharer.user_id != owner_id => Some(
                meta.owner_nickname
                    .clone()
                    .or_else(|| sharer.owner_nickname.clone())
                    .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend")),
            ),
            _ => None,
        };

        let summary = AreaSummary {
            id: area_id,
            name: area.get_name().to_string(),
            owned: false,
            can_edit: access.can_edit,
            can_admin: access.can_admin,
            enabled: atlas.is_area_enabled(&area_id),
            reshare_owner,
            sharer_label: Some(sharer_label.clone()),
            in_family: family_members.contains(&area_id),
        };

        let accum = shared.entry(group_key).or_insert_with(|| Accum {
            label: sharer_label,
            by_atlas: HashMap::new(),
            loose: Vec::new(),
        });
        match meta.atlas_id {
            Some(atlas_id) => {
                let name = meta
                    .atlas_name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| crate::i18n::t!("area-list-shared-folder"));
                let folder = accum
                    .by_atlas
                    .entry(atlas_id)
                    .or_insert_with(|| (name, Vec::new()));
                folder.1.push(summary);
            }
            None => accum.loose.push(summary),
        }
    }

    let mut groups: Vec<SharedGroup> = shared
        .into_values()
        .map(|accum| {
            let mut folders: Vec<Folder> = accum
                .by_atlas
                .into_iter()
                .map(|(atlas_id, (label, mut areas))| {
                    sort_by_name(&mut areas);
                    Folder {
                        key: FolderKey::Atlas(atlas_id),
                        label,
                        areas,
                        owned: false,
                    }
                })
                .collect();
            folders.sort_by(|a, b| {
                a.label
                    .to_lowercase()
                    .cmp(&b.label.to_lowercase())
                    .then_with(|| a.label.cmp(&b.label))
            });
            let mut loose = accum.loose;
            sort_by_name(&mut loose);
            SharedGroup {
                label: crate::i18n::t!("area-list-shared-by", "person" => accum.label),
                folders,
                loose,
            }
        })
        .collect();
    groups.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.label.cmp(&b.label))
    });
    groups
}

/// A subtle, dimmed badge text used for inline row annotations.
fn badge<'a>(content: String) -> iced::widget::Text<'a, crate::Theme> {
    text(content)
        .size(10)
        .style(|theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.45)),
        })
}

/// A top-level group's header row, its content lined up with the rows below
/// and its ⋯ menu with theirs.
fn group_header<'a>(content: impl Into<ThemedElement<'a, Message>>) -> ThemedElement<'a, Message> {
    container(content)
        .padding(Padding {
            top: 0.0,
            right: ROW_INSET,
            bottom: 0.0,
            left: Depth::Group.inset(),
        })
        .into()
}

/// A dimmed group header ("My local maps", "Shared by …"), spaced like a
/// clan's.
fn group_label<'a>(label: String) -> ThemedElement<'a, Message> {
    group_header(
        container(
            text(label)
                .size(12)
                .style(|theme: &crate::Theme| iced::widget::text::Style {
                    color: Some(theme.styles.text.normal.scale_alpha(0.6)),
                }),
        )
        .padding(Padding::ZERO.top(4)),
    )
}

/// One of the viewer's own groups: its header, then its folders. Nothing
/// when it holds no folder.
fn push_owned<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    label: String,
    folders: Vec<Folder>,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    if folders.is_empty() {
        return list;
    }
    list = list.push(group_label(label));
    for folder in folders {
        list = push_folder(list, window, folder, selected);
    }
    list
}

/// The viewer's local, then shared, group.
fn push_owned_groups<'a>(
    list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    owned: OwnedGroups,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    let list = push_owned(
        list,
        window,
        crate::i18n::t!("area-list-my-local-maps"),
        owned.local,
        selected,
    );
    push_owned(
        list,
        window,
        crate::i18n::t!("area-list-my-shared-maps"),
        owned.shared,
        selected,
    )
}

pub fn view(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let atlas = window.mapper.get_current_atlas();
    let selected = window.editor.area_id();
    // Rows note themselves as they're drawn, for Shift+click ranges.
    window.multi.begin_order();

    let mut header = row![
        text(crate::i18n::t!("area-list-title")).size(14),
        space::horizontal(),
        tooltip(
            icon_button(bootstrap_icons::PLUS_LG, Message::NewAreaRequested),
            crate::i18n::ts!("area-list-new-map"),
            tooltip::Position::Bottom,
        ),
        tooltip(
            icon_button(bootstrap_icons::FOLDER_PLUS, Message::NewAtlasRequested),
            crate::i18n::ts!("area-list-new-folder"),
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(4)
    .align_y(Vertical::Center)
    .padding(8);
    if let Some(menu) = scope_menu(window) {
        header = header.push(menu);
    }

    // Copy-family membership (over copied_from edges + family_token) for the
    // "copy" badge; computed once for the whole list.
    let family_members = window.family_members();
    let ephemeral = window.mapper.session_area_ids();
    // Read from the mapper rather than the tick-refreshed snapshot, so a
    // folder made a moment ago lands in the right group straight away.
    let mut owned = owned_groups(
        &atlas,
        &window.atlases,
        &window.mapper.local_atlas_ids(),
        &window.mapper.local_area_ids(),
        &family_members,
        &ephemeral,
    );
    // Each clan's folders list between the viewer's own maps and "Shared
    // by"; a map in a clan folder lists there, and in every other folder
    // holding it.
    let mut clans = clan_maps::clan_groups(window, &atlas, &family_members, &ephemeral);
    let in_clans = clan_maps::in_clan_folders(&atlas);
    let mut shared = shared_groups(&atlas, window.sharers.as_ref(), &family_members, &in_clans);
    let query = window.map_list_filter.trim().to_lowercase();
    filter_folders(&mut owned.local, &query);
    filter_folders(&mut owned.shared, &query);
    for group in &mut clans {
        filter_folders(&mut group.folders, &query);
        filter_areas(&mut group.unfiled, &query);
    }
    for group in &mut shared {
        filter_folders(&mut group.folders, &query);
        filter_areas(&mut group.loose, &query);
    }
    shared.retain(|group| !group.folders.is_empty() || !group.loose.is_empty());

    let mut list = Column::new().spacing(2).padding(4);

    // Scope mode selects how the owned folders and shared groups are organized:
    //   This-server  — filter to the current entry (Unassigned collapses into
    //                  its own group; other entries' atlases are omitted).
    //   All-atlases  — with a server context, bucket everything by scope state
    //                  (This server / Unassigned / Other servers).
    //   No context   — flat: every folder and shared group, unfiltered.
    list = match (window.server_name.as_deref(), window.scope_all) {
        (Some(server), false) => {
            render_this_server(list, window, owned, clans, shared, selected, server)
        }
        (Some(server), true) => {
            render_all_buckets(list, window, owned, clans, shared, selected, server)
        }
        (None, _) => render_flat(list, window, owned, clans, shared, selected),
    };

    // Session (ephemeral) maps render as their own group, so a live auto-map
    // is inspectable while it builds. Excluded from the folder tree above
    // (they never persist); shown here for diagnosis + promotion.
    let mut session = session_maps(&atlas, &ephemeral);
    filter_areas(&mut session, &query);
    if !session.is_empty() {
        list = list.push(group_label(crate::i18n::t!("area-list-session-maps")));
        for area in session {
            list = list.push(area_row(window, area, selected, Depth::Folder));
        }
    }

    if !query.is_empty() && window.multi.order.borrow().is_empty() {
        list =
            list.push(container(text(crate::i18n::t!("area-list-no-matches")).size(12)).padding(8));
    }
    let chrome = column![
        header,
        filter_control(window),
        scrollable(list).height(Length::Fill)
    ];

    container(chrome)
        .style(builtins::container::opaque)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// One sharer's shared content narrowed to a single scope bucket: the sharer
/// label (repeated across buckets so attribution survives bucketing) plus the
/// atlas folders and atlas-less areas that fell into this bucket.
struct SharedFrag {
    label: String,
    folders: Vec<Folder>,
    loose: Vec<AreaSummary>,
}

impl SharedFrag {
    fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.loose.is_empty()
    }

    fn count(&self) -> usize {
        self.folders.len() + self.loose.len()
    }
}

/// The scope state of an owned/shared atlas folder against `server`. Local
/// atlases and the "Not in a folder" bucket aren't scope-keyed, so they
/// always read as `Here` (shown on every entry, including this one).
fn folder_scope(window: &MapEditorWindow, folder: &Folder, server: &str) -> ScopeState {
    match folder.key {
        FolderKey::Atlas(atlas_id) if !window.local_atlas_ids.contains(&atlas_id) => {
            window.map_scopes.atlas_scope(&atlas_id, server)
        }
        _ => ScopeState::Here,
    }
}

/// The scope-bucket index of a [`ScopeState`]: 0 = Here, 1 = Unassigned,
/// 2 = Elsewhere.
fn scope_index(state: ScopeState) -> usize {
    match state {
        ScopeState::Here => 0,
        ScopeState::Unassigned => 1,
        ScopeState::Elsewhere => 2,
    }
}

/// Split one sharer group's folders and atlas-less areas into the three scope
/// buckets (`[Here, Unassigned, Elsewhere]`) for `server`. The sharer label is
/// carried into every bucket so attribution persists wherever the content lands.
fn partition_shared_group(
    window: &MapEditorWindow,
    group: SharedGroup,
    server: &str,
) -> [SharedFrag; 3] {
    let mut frags = std::array::from_fn(|_| SharedFrag {
        label: group.label.clone(),
        folders: Vec::new(),
        loose: Vec::new(),
    });
    for folder in group.folders {
        let idx = scope_index(folder_scope(window, &folder, server));
        frags[idx].folders.push(folder);
    }
    for area in group.loose {
        // A genuinely atlas-less shared area is scoped by its own area record.
        let idx = scope_index(window.map_scopes.area_scope(&area.id, server));
        frags[idx].loose.push(area);
    }
    frags
}

/// Split one clan's folders into the three scope buckets for `server`. The
/// clan's name labels each bucket its folders land in.
fn partition_clan(window: &MapEditorWindow, group: ClanGroup, server: &str) -> [ClanGroup; 3] {
    let mut parts = std::array::from_fn(|_| ClanGroup {
        clan_id: group.clan_id,
        label: group.label.clone(),
        incoming: group.incoming,
        folders: Vec::new(),
        unfiled: Vec::new(),
    });
    for folder in group.folders {
        let idx = scope_index(folder_scope(window, &folder, server));
        parts[idx].folders.push(folder);
    }
    // A map in no folder is scoped by its own area record.
    for area in group.unfiled {
        let idx = scope_index(window.map_scopes.area_scope(&area.id, server));
        parts[idx].unfiled.push(area);
    }
    parts
}

/// Splits the viewer's own folders into the three scope buckets
/// (`[Here, Unassigned, Elsewhere]`) for `server`, keeping local and shared
/// apart.
fn partition_owned(window: &MapEditorWindow, owned: OwnedGroups, server: &str) -> [OwnedGroups; 3] {
    let mut parts: [OwnedGroups; 3] = std::array::from_fn(|_| OwnedGroups::default());
    for folder in owned.local {
        let idx = scope_index(folder_scope(window, &folder, server));
        parts[idx].local.push(folder);
    }
    for folder in owned.shared {
        let idx = scope_index(folder_scope(window, &folder, server));
        parts[idx].shared.push(folder);
    }
    parts
}

/// Renders one clan: its header (with its ⋯ menu where `with_menu`), then
/// its folders.
fn push_clan<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    group: ClanGroup,
    with_menu: bool,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    if !window.map_list_filter.trim().is_empty()
        && group.folders.is_empty()
        && group.unfiled.is_empty()
    {
        return list;
    }
    list = list.push(group_header(clan_maps::clan_header(
        window, &group, with_menu,
    )));
    for folder in group.folders {
        list = push_folder(list, window, folder, selected);
    }
    for area in group.unfiled {
        list = list.push(area_row(window, area, selected, Depth::Folder));
    }
    list
}

/// Renders one sharer's group: its label, then its atlas folders and the
/// shared maps in no folder.
fn push_shared<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    label: String,
    folders: Vec<Folder>,
    loose: Vec<AreaSummary>,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    list = list.push(group_label(label));
    for folder in folders {
        list = push_folder(list, window, folder, selected);
    }
    for area in loose {
        list = list.push(area_row(window, area, selected, Depth::Folder));
    }
    list
}

/// Renders a run of shared fragments, one sharer group each.
fn render_shared_frags<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    frags: Vec<SharedFrag>,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    for frag in frags {
        list = push_shared(list, window, frag.label, frag.folders, frag.loose, selected);
    }
    list
}

/// This-server scope: only content associated with (or unassigned relative to)
/// the current entry appears — atlases bound only to other entries are omitted,
/// and unassigned atlases/areas collapse into one "Unassigned" section.
fn render_this_server<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    owned: OwnedGroups,
    clans: Vec<ClanGroup>,
    shared: Vec<SharedGroup>,
    selected: Option<AreaId>,
    server: &str,
) -> Column<'a, Message, crate::Theme> {
    let [owned_here, owned_unassigned, _elsewhere] = partition_owned(window, owned, server);

    // Every clan heads its folders here, so its menu is always at hand.
    let mut clans_here: Vec<ClanGroup> = Vec::new();
    let mut clans_unassigned: Vec<ClanGroup> = Vec::new();
    for group in clans {
        let [here, unassigned, _elsewhere] = partition_clan(window, group, server);
        clans_here.push(here);
        if !unassigned.folders.is_empty() || !unassigned.unfiled.is_empty() {
            clans_unassigned.push(unassigned);
        }
    }

    let mut shared_here: Vec<SharedFrag> = Vec::new();
    let mut shared_unassigned: Vec<SharedFrag> = Vec::new();
    for group in shared {
        let [here, unassigned, _elsewhere] = partition_shared_group(window, group, server);
        if !here.is_empty() {
            shared_here.push(here);
        }
        if !unassigned.is_empty() {
            shared_unassigned.push(unassigned);
        }
    }

    list = push_owned_groups(list, window, owned_here, selected);
    for group in clans_here {
        list = push_clan(list, window, group, true, selected);
    }
    list = render_shared_frags(list, window, shared_here, selected);

    let unassigned_count = owned_unassigned.local.len()
        + owned_unassigned.shared.len()
        + clans_unassigned
            .iter()
            .map(|group| group.folders.len() + group.unfiled.len())
            .sum::<usize>()
        + shared_unassigned
            .iter()
            .map(SharedFrag::count)
            .sum::<usize>();
    if unassigned_count > 0 {
        let filtering = !window.map_list_filter.trim().is_empty();
        let collapsed = !filtering && window.collapsed_folders.contains(&FolderKey::Unassigned);
        list = list.push(unassigned_header(unassigned_count, collapsed, filtering));
        if !collapsed {
            list = push_owned_groups(list, window, owned_unassigned, selected);
            for group in clans_unassigned {
                list = push_clan(list, window, group, false, selected);
            }
            list = render_shared_frags(list, window, shared_unassigned, selected);
        }
    }
    list
}

/// All-atlases scope with a server context: everything, bucketed by scope state
/// into This server / Unassigned / Other servers, composing the owned-folder and
/// shared structures inside each bucket.
fn render_all_buckets<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    owned: OwnedGroups,
    clans: Vec<ClanGroup>,
    shared: Vec<SharedGroup>,
    selected: Option<AreaId>,
    server: &str,
) -> Column<'a, Message, crate::Theme> {
    let mut owned = partition_owned(window, owned, server);
    // Every clan heads its folders on this server, with its menu; elsewhere
    // its name labels the folders that land there.
    let mut clan_buckets: [Vec<ClanGroup>; 3] = std::array::from_fn(|_| Vec::new());
    for group in clans {
        for (idx, part) in partition_clan(window, group, server)
            .into_iter()
            .enumerate()
        {
            if (idx == 0 && window.map_list_filter.trim().is_empty())
                || !part.folders.is_empty()
                || !part.unfiled.is_empty()
            {
                clan_buckets[idx].push(part);
            }
        }
    }
    let mut shared_buckets: [Vec<SharedFrag>; 3] = std::array::from_fn(|_| Vec::new());
    for group in shared {
        for (idx, frag) in partition_shared_group(window, group, server)
            .into_iter()
            .enumerate()
        {
            if !frag.is_empty() {
                shared_buckets[idx].push(frag);
            }
        }
    }

    let headers = [
        crate::i18n::t!("area-list-on-server", "server" => server),
        crate::i18n::t!("area-list-unassigned"),
        crate::i18n::t!("area-list-other-servers"),
    ];
    for (idx, header) in headers.into_iter().enumerate() {
        let owned_bucket = std::mem::take(&mut owned[idx]);
        let clan_bucket = std::mem::take(&mut clan_buckets[idx]);
        let shared_bucket = std::mem::take(&mut shared_buckets[idx]);
        if owned_bucket.local.is_empty()
            && owned_bucket.shared.is_empty()
            && clan_bucket.is_empty()
            && shared_bucket.is_empty()
        {
            continue;
        }
        list = list.push(bucket_header(header));
        list = push_owned_groups(list, window, owned_bucket, selected);
        for group in clan_bucket {
            list = push_clan(list, window, group, idx == 0, selected);
        }
        list = render_shared_frags(list, window, shared_bucket, selected);
    }
    list
}

/// No server context: flat rendering — the viewer's own groups, every clan,
/// then every sharer's group, unfiltered (the pre-scoping single-context
/// behavior).
fn render_flat<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    owned: OwnedGroups,
    clans: Vec<ClanGroup>,
    shared: Vec<SharedGroup>,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    list = push_owned_groups(list, window, owned, selected);
    for group in clans {
        list = push_clan(list, window, group, true, selected);
    }
    for group in shared {
        list = push_shared(
            list,
            window,
            group.label,
            group.folders,
            group.loose,
            selected,
        );
    }
    list
}

/// A scope-bucket section header for the All-atlases three-bucket view. It
/// divides the list rather than heading a level of the tree, so the groups
/// under it stay at the top level.
fn bucket_header<'a>(label: String) -> ThemedElement<'a, Message> {
    container(text(label).size(13))
        .padding(Padding::ZERO.left(Depth::Group.inset()))
        .into()
}

/// The "This server / All atlases" scope control, shown only when the editor
/// has a server context (otherwise everything is shown and there is nothing to
/// switch).
fn scope_menu(window: &MapEditorWindow) -> Option<ThemedElement<'_, Message>> {
    let server = window.server_name.as_deref()?;
    let choice = |label: String, active: bool, all: bool| {
        let style = if active {
            builtins::button::list_item_selected
        } else {
            builtins::button::toolbar
        };
        button(
            row![
                text(if active { "✓" } else { "" }).width(16),
                text(label).size(12)
            ]
            .spacing(6),
        )
        .style(style)
        .on_press(Message::ScopeAllToggled(all))
        .padding([6, 10])
        .width(Length::Fill)
    };
    let open = window.scope_menu_open;
    let trigger = tooltip(
        button(text("⋯").size(14))
            .style(if open {
                builtins::button::toolbar_active
            } else {
                builtins::button::toolbar
            })
            .padding([0, 6])
            .on_press(Message::ScopeMenuToggled(!open)),
        text(if window.scope_all {
            crate::i18n::t!("area-list-all-atlases")
        } else {
            crate::i18n::t!("area-list-this-server", "server" => server)
        }),
        tooltip::Position::Bottom,
    );
    let menu = open.then(|| {
        container(
            column![
                choice(
                    crate::i18n::t!("area-list-this-server", "server" => server),
                    !window.scope_all,
                    false
                ),
                choice(
                    crate::i18n::t!("area-list-all-atlases"),
                    window.scope_all,
                    true
                ),
            ]
            .spacing(2),
        )
        .width(240)
        .padding(6)
        .style(builtins::container::card)
        .into()
    });
    Some(Dropdown::new(trigger, menu, Message::ScopeMenuToggled(false)).into())
}

fn filter_control(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let mut filter = row![
        text_input(
            crate::i18n::ts!("area-list-filter-placeholder"),
            &window.map_list_filter
        )
        .on_input(Message::MapListFilterChanged)
        .size(12)
        .width(Length::Fill),
    ]
    .spacing(4)
    .align_y(Vertical::Center);
    if !window.map_list_filter.is_empty() {
        filter = filter.push(tooltip(
            button(text("×").size(14))
                .style(builtins::button::toolbar)
                .on_press(Message::MapListFilterChanged(String::new())),
            crate::i18n::ts!("action-clear"),
            tooltip::Position::Bottom,
        ));
    }
    container(filter).padding([0, 8]).into()
}

/// The collapsed-by-default "Unassigned" group header (This-server scope): the
/// atlases with no server-entry association yet.
fn unassigned_header<'a>(
    count: usize,
    collapsed: bool,
    filtering: bool,
) -> ThemedElement<'a, Message> {
    let disclosure = if collapsed { "\u{25B8}" } else { "\u{25BE}" };
    let header = row![
        text(disclosure)
            .size(10)
            .style(|theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.6)),
            }),
        text(crate::i18n::t!("area-list-unassigned")).size(13),
        space::horizontal(),
        badge(count.to_string()),
    ]
    .spacing(6)
    .align_y(Vertical::Center)
    .width(Length::Fill);
    button(header)
        .style(builtins::button::list_item)
        .on_press_maybe(
            (!filtering).then_some(Message::ToggleFolderCollapsed(FolderKey::Unassigned)),
        )
        .width(Length::Fill)
        .into()
}

/// Appends a folder's disclosure header and (unless collapsed) its rows, a
/// step in from it.
fn push_folder<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    folder: Folder,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    let collapsed =
        window.map_list_filter.trim().is_empty() && window.collapsed_folders.contains(&folder.key);
    list = list.push(folder_header(window, &folder, collapsed));
    if !collapsed {
        for area in folder.areas {
            list = list.push(area_row(window, area, selected, Depth::Map));
        }
    }
    list
}

/// One folder's disclosure header: a chevron, the folder name, a count badge,
/// and (for named atlases) new-map / rename / delete / share affordances.
/// Pressing a named folder's row chooses it, and the inspector shows its
/// panel (Ctrl/Shift+click chooses several); its chevron opens and closes
/// it. The "Not in a folder" row opens and closes on any press. The nested
/// buttons capture their own clicks. While the folder is being renamed it
/// becomes a text input instead.
fn folder_header<'a>(
    window: &'a MapEditorWindow,
    folder: &Folder,
    collapsed: bool,
) -> ThemedElement<'a, Message> {
    let item = match folder.key {
        FolderKey::Atlas(atlas_id) => Some(ListItem::Folder(atlas_id)),
        FolderKey::Unfiled(_) | FolderKey::Unassigned => None,
    };
    if let Some(item) = item {
        window.multi.record(item);
    }
    if let Some((renaming_id, name)) = &window.renaming_atlas
        && FolderKey::Atlas(*renaming_id) == folder.key
    {
        return container(
            text_input(crate::i18n::ts!("mapper-folder-name-placeholder"), name)
                .size(13)
                .on_input(Message::RenameAtlasChanged)
                .on_submit(Message::RenameAtlasCommitted),
        )
        .padding(Depth::Folder.input_padding())
        .into();
    }

    // Triangles render in the regular font, sidestepping the icon-font set.
    let disclosure = if collapsed { "\u{25B8}" } else { "\u{25BE}" };
    let count = folder.areas.len();
    let chevron =
        text(disclosure)
            .size(10)
            .style(|theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.6)),
            });
    // A row that chooses its folder opens and closes by its chevron.
    let chevron: ThemedElement<'static, Message> = if item.is_some() {
        button(chevron)
            .style(builtins::button::link)
            .padding(CHEVRON_PADDING)
            .on_press_maybe(
                window
                    .map_list_filter
                    .trim()
                    .is_empty()
                    .then_some(Message::ToggleFolderCollapsed(folder.key)),
            )
            .into()
    } else {
        container(chevron).padding(CHEVRON_PADDING).into()
    };

    let mut header = row![chevron, text(folder.label.clone()).size(13)]
        .spacing(6)
        .align_y(Vertical::Center)
        .width(Length::Fill);

    // Where this server's scripts put new maps that name no folder.
    if let FolderKey::Atlas(atlas_id) = folder.key
        && window.default_atlases.contains(atlas_id)
        && let Some(server) = window.server_name.as_deref()
    {
        header = header.push(tooltip(
            badge(crate::i18n::t!("area-list-default-badge")),
            text(crate::i18n::t!("area-list-default-tip", "server" => server)),
            tooltip::Position::Bottom,
        ));
    }

    header = header.push(badge(if count == 0 {
        crate::i18n::t!("area-list-empty")
    } else {
        count.to_string()
    }));
    header = header.push(space::horizontal());

    // The "Not in a folder" bucket isn't a real atlas — no rename/delete/
    // share/new-map.
    if let FolderKey::Atlas(atlas_id) = folder.key {
        // Owner-only structural affordances (new-map/rename/delete/share/
        // transfer). A shared atlas folder (surfaced under a sharer group) shows
        // none of these — the recipient doesn't own it — only the per-user
        // "Servers…" override below.
        if folder.owned {
            header = header.push(tooltip(
                icon_button(bootstrap_icons::PLUS_LG, Message::NewAreaInAtlas(atlas_id)),
                crate::i18n::ts!("area-list-new-map-folder"),
                tooltip::Position::Bottom,
            ));
        }
        if let Some(menu) = folder_menu(window, atlas_id, folder.owned) {
            header = header.push(menu);
        }
    }

    let chosen = item.is_some_and(|item| {
        window
            .multi
            .selection
            .contains(item, window.editor.area_id())
    });
    button(header)
        .style(if chosen {
            builtins::button::list_item_selected
        } else {
            builtins::button::list_item
        })
        .padding(Depth::Folder.row_padding())
        .on_press_maybe(match item {
            Some(item) => Some(Message::Multi(MultiMessage::Pressed(item))),
            None => window
                .map_list_filter
                .trim()
                .is_empty()
                .then_some(Message::ToggleFolderCollapsed(folder.key)),
        })
        .width(Length::Fill)
        .into()
}

/// One map row at `depth`, shared by the folder tree and the by-sharer
/// groups. Rows only choose a map: its actions live in the toolbar.
/// Ctrl/Shift+click chooses several.
fn area_row<'a>(
    window: &'a MapEditorWindow,
    area: AreaSummary,
    selected: Option<AreaId>,
    depth: Depth,
) -> ThemedElement<'a, Message> {
    let list_item = ListItem::Map(area.id);
    window.multi.record(list_item);
    let is_selected = window.multi.selection.contains(list_item, selected);

    // A row being renamed swaps to a text input; Enter commits, Escape
    // (window-level) cancels.
    if let Some((renaming_id, name)) = &window.renaming_area
        && *renaming_id == area.id
    {
        return container(
            text_input(crate::i18n::ts!("mapper-area-name-placeholder"), name)
                .size(14)
                .on_input(Message::RenameAreaChanged)
                .on_submit(Message::RenameAreaCommitted),
        )
        .padding(depth.input_padding())
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
            badge(crate::i18n::t!("inspector-inactive")),
            crate::i18n::ts!("area-list-inactive-tip"),
            tooltip::Position::Bottom,
        ));
    }

    // Family badge: this map is one of several copies sharing an origin.
    if area.in_family {
        item = item.push(tooltip(
            badge(crate::i18n::t!("area-list-copy-badge")),
            crate::i18n::ts!("area-list-copy-family-tip"),
            tooltip::Position::Bottom,
        ));
    }

    // Re-share badge: the sharer isn't the map's owner.
    if let Some(owner) = &area.reshare_owner {
        let sharer = area
            .sharer_label
            .clone()
            .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));
        item = item.push(tooltip(
            badge(crate::i18n::t!("area-list-owned-by", "owner" => owner)),
            text(crate::i18n::t!(
                "area-list-reshared",
                "sharer" => sharer,
                "owner" => owner
            )),
            tooltip::Position::Bottom,
        ));
    }

    // Subtle capability badges on shared rows.
    if !area.owned && area.can_admin {
        item = item.push(badge(crate::i18n::t!("area-list-admin-badge")));
    } else if !area.owned && area.can_edit {
        item = item.push(badge(crate::i18n::t!("area-list-edit-badge")));
    }

    item = item.push(space::horizontal());

    button(item)
        .style(if is_selected {
            builtins::button::list_item_selected
        } else {
            builtins::button::list_item
        })
        .padding(depth.row_padding())
        .on_press(Message::Multi(MultiMessage::Pressed(list_item)))
        .width(Length::Fill)
        .into()
}

/// A folder's ⋯ menu: rename, delete, move, share, transfer (owner only) and
/// the per-user server checklist; a clan folder's as its actions allow.
/// `None` when nothing applies.
fn folder_menu(
    window: &MapEditorWindow,
    atlas_id: AtlasId,
    owned: bool,
) -> Option<ThemedElement<'static, Message>> {
    let atlas_is_local = window.local_atlas_ids.contains(&atlas_id);
    // A clan folder's menu follows the folder's actions.
    let clan_folder = clan_maps::is_clan_folder(window, atlas_id);
    let mut entries: Vec<(String, Message)> = if clan_folder {
        clan_maps::folder_entries(window, atlas_id)
    } else {
        Vec::new()
    };
    if owned {
        entries.push((
            crate::i18n::t!("mapper-menu-rename"),
            Message::RenameAtlasStarted(atlas_id),
        ));
        // An atlas move always crosses the local/cloud boundary, so both
        // directions need the cloud side: signed out, a local source has
        // no destination and a cloud source's delete would fail after the
        // copy (a recoverable duplicate, but an error all the same).
        if window.cloud.snapshot.get().signed_in && !clan_folder {
            entries.push((
                crate::i18n::t!("area-list-move-action"),
                Message::MoveAtlasStorageRequested(atlas_id),
            ));
        }
        // Sharing is cloud-only: a local folder has no server identity. A
        // clan's folder is never transferred.
        if !atlas_is_local {
            entries.push((
                crate::i18n::t!("area-list-share-action"),
                Message::ShareAtlasRequested(atlas_id),
            ));
            if !clan_folder {
                entries.push((
                    crate::i18n::t!("mapper-transfer-action"),
                    Message::TransferAtlasOwnershipRequested(atlas_id),
                ));
            }
        }
        if !clan_folder {
            entries.extend(default_atlases::menu_entry(window, atlas_id));
        }
    }
    // Which server entries the folder shows on. The associations are
    // per-user, so this works on a shared folder too; a local folder is never
    // scoped.
    if !atlas_is_local && !clan_folder {
        entries.push((
            crate::i18n::t!("area-list-servers-action"),
            Message::ServersChecklistRequested(ScopeTarget::Atlas(atlas_id)),
        ));
    }
    if owned {
        entries.push((
            crate::i18n::t!("area-list-delete-folder"),
            Message::DeleteAtlasRequested(atlas_id),
        ));
    }
    if entries.is_empty() {
        return None;
    }

    let open = window.folder_menu == Some(atlas_id);
    let trigger = button(text("⋯").size(14.0))
        .style(if open {
            builtins::button::toolbar_active
        } else {
            builtins::button::toolbar
        })
        .padding([0, 6])
        .on_press(Message::FolderMenuToggled((!open).then_some(atlas_id)));
    let menu = open.then(|| {
        let mut list = column![].spacing(2);
        for (label, message) in entries {
            list = list.push(
                button(text(label).size(12))
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(builtins::button::link)
                    .on_press(Message::MenuPicked(Box::new(message))),
            );
        }
        container(list)
            .width(200)
            .padding(6)
            .style(builtins::container::card)
            .into()
    });
    Some(Dropdown::new(trigger, menu, Message::FolderMenuToggled(None)).into())
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
                can_admin: false,
                parent_grant_id: None,
                created_at: Utc::now() + Duration::seconds(created_offset_secs),
                updated_at: Utc::now(),
                grantor_nickname: None,
                owner_nickname: None,
                host_hints: None,
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

    fn owned_map(n: u128, name: &str, atlas: Option<u128>, local: bool) -> OwnedMap {
        OwnedMap {
            summary: AreaSummary {
                id: AreaId(uuid(n)),
                name: name.to_string(),
                owned: true,
                can_edit: true,
                can_admin: true,
                enabled: true,
                reshare_owner: None,
                sharer_label: None,
                in_family: false,
            },
            atlas_id: atlas.map(|atlas| AtlasId(uuid(atlas))),
            atlas_name: None,
            local,
        }
    }

    fn folder_item(n: u128, name: &str, clan: Option<u128>) -> AtlasListItem {
        AtlasListItem {
            id: AtlasId(uuid(n)),
            name: name.to_string(),
            created_at: Utc::now(),
            area_count: 0,
            rev: 1,
            is_owner: clan.is_none(),
            can_admin: clan.is_none(),
            owner_nickname: None,
            clan_id: clan.map(uuid),
            clan_name: None,
            actions: std::collections::BTreeSet::new(),
        }
    }

    /// Each group's folders as (key, label, map names).
    fn shape(folders: &[Folder]) -> Vec<(FolderKey, String, Vec<&str>)> {
        folders
            .iter()
            .map(|folder| {
                (
                    folder.key,
                    folder.label.clone(),
                    folder.areas.iter().map(|area| area.name.as_str()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn own_maps_split_into_local_and_shared_groups() {
        let atlases = [
            folder_item(1, "Roads", None),
            folder_item(2, "Cities", None),
            folder_item(3, "Empty", None),
            // A clan's folder lists under its clan, never here.
            folder_item(4, "Clan towns", Some(40)),
        ];
        let local_atlases: HashSet<AtlasId> = [AtlasId(uuid(1))].into();
        let maps = vec![
            owned_map(10, "Trail", Some(1), true),
            owned_map(11, "Midgaard", Some(2), false),
            owned_map(12, "Scratch", None, true),
            owned_map(13, "Sketch", None, false),
            // Its folder isn't in the inventory (yet).
            owned_map(14, "Arriving", Some(99), false),
        ];
        let groups = group_owned(maps, &atlases, &local_atlases);
        let not_in_folder = crate::i18n::t!("area-list-not-in-folder");
        assert_eq!(
            shape(&groups.local),
            [
                (
                    FolderKey::Atlas(AtlasId(uuid(1))),
                    "Roads".to_string(),
                    vec!["Trail"]
                ),
                (
                    FolderKey::Unfiled(MapStorage::Local),
                    not_in_folder.clone(),
                    vec!["Scratch"]
                ),
            ]
        );
        assert_eq!(
            shape(&groups.shared),
            [
                (
                    FolderKey::Atlas(AtlasId(uuid(2))),
                    "Cities".to_string(),
                    vec!["Midgaard"]
                ),
                // An empty folder still shows, so a new one is visible.
                (
                    FolderKey::Atlas(AtlasId(uuid(3))),
                    "Empty".to_string(),
                    vec![]
                ),
                (
                    FolderKey::Unfiled(MapStorage::Cloud),
                    not_in_folder,
                    vec!["Arriving", "Sketch"]
                ),
            ]
        );
    }

    #[test]
    fn a_map_shows_in_its_folder_before_the_folder_list_arrives() {
        let arriving = OwnedMap {
            atlas_name: Some("Harbor".to_string()),
            ..owned_map(14, "Docks", Some(99), false)
        };
        let groups = group_owned(
            vec![arriving, owned_map(15, "Sketch", None, false)],
            &[],
            &HashSet::new(),
        );
        assert_eq!(
            shape(&groups.shared),
            [
                (
                    FolderKey::Atlas(AtlasId(uuid(99))),
                    "Harbor".to_string(),
                    vec!["Docks"]
                ),
                (
                    FolderKey::Unfiled(MapStorage::Cloud),
                    crate::i18n::t!("area-list-not-in-folder"),
                    vec!["Sketch"]
                ),
            ]
        );
    }

    #[test]
    fn a_group_with_every_map_filed_has_no_unfiled_bucket() {
        let atlases = [folder_item(1, "Roads", None)];
        let local_atlases: HashSet<AtlasId> = [AtlasId(uuid(1))].into();
        let groups = group_owned(
            vec![owned_map(10, "Trail", Some(1), true)],
            &atlases,
            &local_atlases,
        );
        assert_eq!(groups.local.len(), 1);
        assert!(groups.shared.is_empty());
    }

    #[test]
    fn filtering_keeps_matching_maps_with_their_folder_and_matching_atlases_with_all_maps() {
        let mut groups = group_owned(
            vec![
                owned_map(10, "North Gate", Some(1), false),
                owned_map(11, "Market", Some(1), false),
                owned_map(12, "South Gate", Some(2), false),
                owned_map(13, "Trail", None, false),
            ],
            &[
                folder_item(1, "City", None),
                folder_item(2, "North Country", None),
                folder_item(3, "North Sea", None),
                folder_item(4, "Empty", None),
            ],
            &HashSet::new(),
        );
        filter_folders(&mut groups.shared, "north");
        assert_eq!(
            shape(&groups.shared),
            [
                (
                    FolderKey::Atlas(AtlasId(uuid(1))),
                    "City".to_string(),
                    vec!["North Gate"]
                ),
                (
                    FolderKey::Atlas(AtlasId(uuid(2))),
                    "North Country".to_string(),
                    vec!["South Gate"]
                ),
                (
                    FolderKey::Atlas(AtlasId(uuid(3))),
                    "North Sea".to_string(),
                    vec![]
                ),
            ]
        );
    }

    #[test]
    fn filtering_unfiled_maps_does_not_match_the_synthetic_bucket_name() {
        let mut groups = group_owned(
            vec![owned_map(10, "Trail", None, true)],
            &[],
            &HashSet::new(),
        );
        filter_folders(&mut groups.local, "");
        assert_eq!(groups.local.len(), 1);
        let bucket_name = groups.local[0].label.to_lowercase();
        filter_folders(&mut groups.local, &bucket_name);
        assert!(groups.local.is_empty());
    }

    /// Exercise the rendered row order used for Shift+click, including
    /// unassigned folders, clan maps, scope changes, and clearing the query.
    #[tokio::test]
    async fn filtering_reveals_collapsed_matches_without_changing_scope_or_collapse_state() {
        let mapper = super::super::links::fixture::serving(vec![
            created_map(10, "North Gate", 1, None),
            created_map(11, "Market", 1, None),
            created_map(12, "North Road", 2, Some(7)),
        ])
        .await;
        let mut window = super::super::test_window(mapper, AreaId(uuid(10)));
        // Opening the map associates its atlas with this server. Put it
        // back in Unassigned to exercise that section's disclosure too.
        window.map_scopes = smudgy_core::models::map_scopes::MapScopes::default();
        window.atlases = vec![
            folder_item(1, "City", None),
            folder_item(2, "Roads", Some(7)),
        ];
        window
            .map_scopes
            .set_atlas_entry(AtlasId(uuid(2)), "another server", true);
        window
            .collapsed_folders
            .insert(FolderKey::Atlas(AtlasId(uuid(1))));
        window.collapsed_folders.insert(FolderKey::Unassigned);
        let collapsed = window.collapsed_folders.clone();
        drop(view(&window));
        assert!(window.multi.order.borrow().is_empty());
        let _ = window.update(Message::MapListFilterChanged("  NORTH  ".to_string()));
        drop(view(&window));
        assert_eq!(
            *window.multi.order.borrow(),
            [
                ListItem::Folder(AtlasId(uuid(1))),
                ListItem::Map(AreaId(uuid(10)))
            ]
        );
        assert_eq!(window.collapsed_folders, collapsed);

        let _ = window.update(Message::ScopeAllToggled(true));
        drop(view(&window));
        assert_eq!(
            *window.multi.order.borrow(),
            [
                ListItem::Folder(AtlasId(uuid(1))),
                ListItem::Map(AreaId(uuid(10))),
                ListItem::Folder(AtlasId(uuid(2))),
                ListItem::Map(AreaId(uuid(12))),
            ]
        );
        let _ = window.update(Message::MapListFilterChanged("no such map".to_string()));
        drop(view(&window));
        assert!(window.multi.order.borrow().is_empty());

        let _ = window.update(Message::MapListFilterChanged(" ".to_string()));
        let _ = window.update(Message::ScopeAllToggled(false));
        drop(view(&window));
        assert!(window.multi.order.borrow().is_empty());
        assert_eq!(window.collapsed_folders, collapsed);
        assert_eq!(window.editor.area_id(), Some(AreaId(uuid(10))));
    }

    /// A map as a create reply describes it: no word of what the viewer may
    /// do with it.
    fn created_map(
        n: u128,
        name: &str,
        atlas: u128,
        clan: Option<u128>,
    ) -> smudgy_cloud::AreaWithDetails {
        serde_json::from_value(serde_json::json!({
            "id": uuid(n), "user_id": null, "atlas_id": uuid(atlas), "name": name,
            "created_at": "2026-10-06T00:00:00Z", "rev": 1, "clan_id": clan.map(uuid),
            "format_version": smudgy_cloud::AREA_FORMAT_VERSION,
            "properties": [], "rooms": [], "labels": [], "shapes": []
        }))
        .expect("a map")
    }

    /// A clan's map the viewer just made lists under its clan alone, even
    /// before the server says what the viewer may do with it.
    #[tokio::test]
    async fn a_new_clan_map_is_not_among_the_viewers_own() {
        let mapper = super::super::links::fixture::serving(vec![
            created_map(10, "Solace", 2, Some(7)),
            created_map(11, "Wayside", 1, None),
        ])
        .await;
        let atlas = mapper.get_current_atlas();
        let atlases = [
            folder_item(1, "Mine", None),
            folder_item(2, "Roads", Some(7)),
        ];
        let none = HashSet::new();
        let groups = owned_groups(&atlas, &atlases, &HashSet::new(), &none, &none, &none);
        assert!(groups.local.is_empty());
        assert_eq!(
            shape(&groups.shared),
            [(
                FolderKey::Atlas(AtlasId(uuid(1))),
                "Mine".to_string(),
                vec!["Wayside"]
            )]
        );
        assert_eq!(first_area_id(&atlas, &none), Some(AreaId(uuid(11))));
    }

    #[test]
    fn rows_step_in_one_level_per_depth() {
        assert!(Depth::Group.inset() < Depth::Folder.inset());
        assert!(Depth::Folder.inset() < Depth::Map.inset());
        assert_eq!(Depth::Group.inset(), ROW_INSET);
        // A rename input's text lines up with the row it replaces.
        assert_eq!(
            Depth::Map.input_padding().left + text_input::DEFAULT_PADDING.left,
            Depth::Map.inset()
        );
    }
}
