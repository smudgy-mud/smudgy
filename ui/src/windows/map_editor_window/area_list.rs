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

use smudgy_core::models::map_scopes::ScopeState;

use super::{FolderKey, MapEditorWindow, Message, ScopeTarget};

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
    /// For a genuinely atlas-less **cloud** area, the scope target its row's
    /// "Servers…" affordance writes. `None` for atlas-filed areas (scoped by
    /// their atlas's folder header), local areas, and session maps.
    pub scope_target: Option<ScopeTarget>,
}

fn sort_by_name(areas: &mut [AreaSummary]) {
    areas.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// One folder in the "My maps" tree: a named atlas, or the catch-all "Loose"
/// bucket for owned areas filed under no atlas. Empty atlas folders are kept
/// (rendered as empty), so a freshly created folder is visible before any
/// map is filed into it. `owned` distinguishes the viewer's own folders (full
/// affordances) from a shared atlas folder surfaced under a sharer group (only
/// the per-user "Servers…" override).
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

/// Some area to fall back to when no specific selection is wanted (initial
/// open, after a delete). Owned areas first, then shared, each name-sorted.
/// Ephemeral (session) areas are excluded — the editor doesn't manage them.
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
/// they group by sharer (see [`shared_groups`]). Ephemeral (session) areas
/// are excluded too: the editor's tree only shows maps that outlive the
/// session, so a session map can't be toggled into the per-area preference
/// lists or picked as a folder member.
#[must_use]
pub fn owned_folders(
    atlas: &AtlasCache,
    atlases: &[AtlasListItem],
    family_members: &std::collections::HashSet<AreaId>,
    ephemeral: &std::collections::HashSet<AreaId>,
    local_areas: &std::collections::HashSet<AreaId>,
) -> Vec<Folder> {
    let known: std::collections::HashSet<AtlasId> = atlases.iter().map(|item| item.id).collect();
    let mut by_atlas: HashMap<AtlasId, Vec<AreaSummary>> = HashMap::new();
    let mut loose: Vec<AreaSummary> = Vec::new();

    for area in atlas.areas() {
        let access = area.effective_access();
        if !access.is_owner || ephemeral.contains(area.get_id()) {
            continue;
        }
        let area_id = *area.get_id();
        let atlas_id = area.meta().atlas_id;
        let filed = matches!(atlas_id, Some(id) if known.contains(&id));
        // A genuinely atlas-less *cloud* area carries its own scope target; a
        // filed area is scoped by its atlas, and local areas aren't scoped.
        let scope_target = (!filed && !local_areas.contains(&area_id))
            .then_some(ScopeTarget::Area(area_id));
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
            scope_target,
        };
        if filed {
            if let Some(id) = atlas_id {
                by_atlas.entry(id).or_default().push(summary);
            }
        } else {
            loose.push(summary);
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
            owned: true,
        });
    }
    if !loose.is_empty() {
        sort_by_name(&mut loose);
        folders.push(Folder {
            key: FolderKey::Loose,
            label: "Loose maps".to_string(),
            areas: loose,
            owned: true,
        });
    }
    folders
}

/// The viewer's ephemeral (session) areas, name-sorted. These live only for
/// the session and are never persisted, so they never appear in the atlas
/// folder tree — but the editor lists them under their own group so a
/// protocol-driven auto-map can be inspected (and promoted) while it builds.
#[must_use]
pub fn session_maps(atlas: &AtlasCache, ephemeral: &std::collections::HashSet<AreaId>) -> Vec<AreaSummary> {
    let mut maps: Vec<AreaSummary> = atlas
        .areas()
        .filter(|area| ephemeral.contains(area.get_id()))
        .map(|area| {
            let area_id = *area.get_id();
            AreaSummary {
                id: area_id,
                name: area.get_name().to_string(),
                has_secrets: false,
                owned: true,
                can_edit: true,
                can_admin: true,
                enabled: atlas.is_area_enabled(&area_id),
                reshare_owner: None,
                sharer_label: None,
                in_family: false,
                scope_target: None,
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
/// folder/pile and across groups.
#[must_use]
pub fn shared_groups(
    atlas: &AtlasCache,
    sharers: Option<&SharerIndex>,
    family_members: &std::collections::HashSet<AreaId>,
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

        // A genuinely atlas-less shared area carries its own area-level scope
        // target (the §5 override surface); an atlas-filed area is scoped by
        // its folder header instead.
        let scope_target = meta.atlas_id.is_none().then_some(ScopeTarget::Area(area_id));

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
            scope_target,
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
                    .unwrap_or_else(|| "Shared folder".to_string());
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
                label: format!("Shared by {}", accum.label),
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
    let ephemeral = window.mapper.ephemeral_area_ids();
    let local_areas = window.mapper.local_area_ids();
    let folders = owned_folders(
        &atlas,
        &window.atlases,
        &family_members,
        &ephemeral,
        &local_areas,
    );
    let shared = shared_groups(&atlas, window.sharers.as_ref(), &family_members);

    let mut list = Column::new().spacing(2).padding(4);

    // Scope mode selects how the owned folders and shared groups are organized:
    //   This-server  — filter to the current entry (Unassigned collapses into
    //                  its own group; other entries' atlases are omitted).
    //   All-atlases  — with a server context, bucket everything by scope state
    //                  (This server / Unassigned / Other servers).
    //   No context   — flat: every folder and shared group, unfiltered.
    list = match (window.server_name.as_deref(), window.scope_all) {
        (Some(server), false) => render_this_server(list, window, folders, shared, selected, server),
        (Some(server), true) => render_all_buckets(list, window, folders, shared, selected, server),
        (None, _) => render_flat(list, window, folders, shared, selected),
    };

    // Session (ephemeral) maps render as their own flat group, so a live
    // auto-map is inspectable while it builds. Excluded from the folder tree
    // above (they never persist); shown here for diagnosis + promotion.
    let session = session_maps(&atlas, &ephemeral);
    if !session.is_empty() {
        list = list.push(group_label("Session maps".to_string()));
        for area in session {
            list = list.push(area_row(window, area, selected, false));
        }
    }

    let mut chrome = column![header];
    if let Some(control) = scope_control(window) {
        chrome = chrome.push(control);
    }
    chrome = chrome.push(scrollable(list).height(Length::Fill));

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
/// atlases and the Loose bucket aren't scope-keyed, so they always read as
/// `Here` (shown on every entry, including this one).
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

/// Renders a run of shared fragments: each sharer label, then its atlas folders
/// and atlas-less area rows.
fn render_shared_frags<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    frags: Vec<SharedFrag>,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    for frag in frags {
        list = list.push(group_label(frag.label));
        for folder in frag.folders {
            list = push_folder(list, window, folder, selected);
        }
        for area in frag.loose {
            list = list.push(area_row(window, area, selected, false));
        }
    }
    list
}

/// This-server scope: only content associated with (or unassigned relative to)
/// the current entry appears — atlases bound only to other entries are omitted,
/// and unassigned atlases/areas collapse into one "Unassigned" group.
fn render_this_server<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    folders: Vec<Folder>,
    shared: Vec<SharedGroup>,
    selected: Option<AreaId>,
    server: &str,
) -> Column<'a, Message, crate::Theme> {
    let has_shared = !shared.is_empty();

    let mut owned_here = Vec::new();
    let mut owned_unassigned = Vec::new();
    for folder in folders {
        match folder_scope(window, &folder, server) {
            ScopeState::Here => owned_here.push(folder),
            ScopeState::Unassigned => owned_unassigned.push(folder),
            ScopeState::Elsewhere => {}
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

    // "My maps" labels the owned tree only when shared groups also appear.
    if has_shared && !owned_here.is_empty() {
        list = list.push(group_label("My maps".to_string()));
    }
    for folder in owned_here {
        list = push_folder(list, window, folder, selected);
    }
    list = render_shared_frags(list, window, shared_here, selected);

    let unassigned_count =
        owned_unassigned.len() + shared_unassigned.iter().map(SharedFrag::count).sum::<usize>();
    if unassigned_count > 0 {
        let collapsed = window.collapsed_folders.contains(&FolderKey::Unassigned);
        list = list.push(unassigned_header(unassigned_count, collapsed));
        if !collapsed {
            for folder in owned_unassigned {
                list = push_folder(list, window, folder, selected);
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
    folders: Vec<Folder>,
    shared: Vec<SharedGroup>,
    selected: Option<AreaId>,
    server: &str,
) -> Column<'a, Message, crate::Theme> {
    let mut owned: [Vec<Folder>; 3] = std::array::from_fn(|_| Vec::new());
    for folder in folders {
        let idx = scope_index(folder_scope(window, &folder, server));
        owned[idx].push(folder);
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
        format!("On {server}"),
        "Unassigned".to_string(),
        "Other servers".to_string(),
    ];
    for (idx, header) in headers.into_iter().enumerate() {
        let owned_bucket = std::mem::take(&mut owned[idx]);
        let shared_bucket = std::mem::take(&mut shared_buckets[idx]);
        if owned_bucket.is_empty() && shared_bucket.is_empty() {
            continue;
        }
        list = list.push(bucket_header(header));
        for folder in owned_bucket {
            list = push_folder(list, window, folder, selected);
        }
        list = render_shared_frags(list, window, shared_bucket, selected);
    }
    list
}

/// No server context: flat rendering — every owned folder, then every shared
/// group, unfiltered (the pre-scoping single-context behavior).
fn render_flat<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    folders: Vec<Folder>,
    shared: Vec<SharedGroup>,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    let has_shared = !shared.is_empty();
    if window.atlases.is_empty() {
        // No owned atlases: keep today's flat owned list (clean single-user
        // view) rather than a lone "Loose maps" header.
        let owned: Vec<AreaSummary> = folders.into_iter().flat_map(|folder| folder.areas).collect();
        if !owned.is_empty() {
            if has_shared {
                list = list.push(group_label("My maps".to_string()));
            }
            for area in owned {
                list = list.push(area_row(window, area, selected, false));
            }
        }
    } else {
        if has_shared {
            list = list.push(group_label("My maps".to_string()));
        }
        for folder in folders {
            list = push_folder(list, window, folder, selected);
        }
    }
    for group in shared {
        list = list.push(group_label(group.label));
        for folder in group.folders {
            list = push_folder(list, window, folder, selected);
        }
        for area in group.loose {
            list = list.push(area_row(window, area, selected, false));
        }
    }
    list
}

/// A top-level scope-bucket header for the All-atlases three-bucket view.
fn bucket_header<'a>(label: String) -> ThemedElement<'a, Message> {
    text(label).size(13).into()
}

/// The "This server / All atlases" scope control, shown only when the editor
/// has a server context (otherwise everything is shown and there is nothing to
/// switch).
fn scope_control(window: &MapEditorWindow) -> Option<ThemedElement<'_, Message>> {
    let server = window.server_name.as_deref()?;
    let tab = |label: String, active: bool, all: bool| {
        let style = if active {
            builtins::button::list_item_selected
        } else {
            builtins::button::toolbar
        };
        button(text(label).size(11))
            .style(style)
            .on_press(Message::ScopeAllToggled(all))
    };
    Some(
        row![
            tab(format!("This server ({server})"), !window.scope_all, false),
            tab("All atlases".to_string(), window.scope_all, true),
        ]
        .spacing(4)
        .align_y(Vertical::Center)
        .padding([0, 8])
        .into(),
    )
}

/// The collapsed-by-default "Unassigned" group header (This-server scope): the
/// atlases with no server-entry association yet.
fn unassigned_header<'a>(count: usize, collapsed: bool) -> ThemedElement<'a, Message> {
    let disclosure = if collapsed { "\u{25B8}" } else { "\u{25BE}" };
    let header = row![
        text(disclosure)
            .size(10)
            .style(|theme: &crate::Theme| iced::widget::text::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.6)),
            }),
        text("Unassigned").size(13),
        space::horizontal(),
        badge(count.to_string()),
    ]
    .spacing(6)
    .align_y(Vertical::Center)
    .width(Length::Fill);
    button(header)
        .style(builtins::button::list_item)
        .on_press(Message::ToggleFolderCollapsed(FolderKey::Unassigned))
        .width(Length::Fill)
        .into()
}

/// Appends a folder's disclosure header and (unless collapsed) its rows.
fn push_folder<'a>(
    mut list: Column<'a, Message, crate::Theme>,
    window: &'a MapEditorWindow,
    folder: Folder,
    selected: Option<AreaId>,
) -> Column<'a, Message, crate::Theme> {
    let collapsed = window.collapsed_folders.contains(&folder.key);
    list = list.push(folder_header(window, &folder, collapsed));
    if !collapsed {
        for area in folder.areas {
            list = list.push(area_row(window, area, selected, true));
        }
    }
    list
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
        // Owner-only structural affordances (new-map/rename/delete/share/
        // transfer). A shared atlas folder (surfaced under a sharer group) shows
        // none of these — the recipient doesn't own it — only the per-user
        // "Servers…" override below.
        if folder.owned {
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
            // Sharing is cloud-only — a local folder has no server identity, so
            // the affordance would always 404. New-map/rename/delete work on
            // both tiers.
            if !window.local_atlas_ids.contains(&atlas_id) {
                header = header
                    .push(text_button("Share\u{2026}", Message::ShareAtlasRequested(atlas_id)));
                // Hand the whole folder to a friend (owner-only).
                header = header.push(text_button(
                    "Transfer\u{2026}",
                    Message::TransferAtlasOwnershipRequested(atlas_id),
                ));
            }
        }
        // Choose which server entries this atlas is shown on (the §5 override
        // surface). Atlas-level scoping is the norm, and the associations are
        // per-user local — so it works on a shared atlas folder too. A local
        // atlas is entry-isolated and never scoped.
        if !window.local_atlas_ids.contains(&atlas_id) {
            header = header.push(text_button(
                "Servers\u{2026}",
                Message::ServersChecklistRequested(ScopeTarget::Atlas(atlas_id)),
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
    // A loose cloud area carries its own "show on servers" checklist (an
    // atlas-filed area is scoped by its folder header instead).
    if is_selected
        && let Some(target) = area.scope_target
    {
        item = item.push(text_button(
            "Servers\u{2026}",
            Message::ServersChecklistRequested(target),
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
}
