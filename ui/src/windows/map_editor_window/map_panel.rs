//! The map's panel: what the inspector shows for the open map with nothing
//! selected on the canvas. Its head names the map, where it is kept and how
//! many rooms it has; below, sections that open and close: the map's data
//! fields, its Secrets, its tags with their rooms in each place, every room
//! (its Secrets' and Private's included) by title, place, number or whole
//! tag, and who has access to it.

use iced::alignment::Vertical;
use iced::widget::{Column, button, column, row, text, text_input};
use iced::{Length, Padding};
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{MapStorage, RoomNumber, SourceId};
use smudgy_map_widget::map_editor::EntityId;
use smudgy_map_widget::sources;

use crate::theme::Element as ThemedElement;
use crate::theme::builtins;

use super::panels::{self, PanelMessage, Section, muted};
use super::secrets::{self, dot, place_name};
use super::tags::{self, RoomRef, TagIndex};
use super::{MapEditorWindow, Message, inspector, modals};

/// The most rooms the Rooms section lays out; a filter narrows the rest.
const SHOWN: usize = 300;

/// One row of the Rooms section.
#[derive(Debug, Clone, PartialEq)]
struct RoomRow {
    source: SourceId,
    number: RoomNumber,
    title: String,
    place: String,
    /// The tag the filter matched, when it matched nothing else.
    tag: Option<String>,
}

impl RoomRow {
    fn entity(&self) -> EntityId {
        if self.source.is_map() {
            EntityId::Room(self.number)
        } else {
            EntityId::SourceRoom(self.source, self.number)
        }
    }
}

/// Every room of `area`: the map's, then each Secret's and Private's own
/// rooms, ordered by title (untitled last), then number.
fn rows(area: &AreaCache) -> Vec<RoomRow> {
    let mut rows: Vec<RoomRow> = area
        .get_rooms()
        .iter()
        .map(|room| RoomRow {
            source: SourceId::Map,
            number: room.get_room_number(),
            title: room.get_title().to_string(),
            place: String::new(),
            tag: None,
        })
        .collect();
    for layer in area.source_layers() {
        let source = layer.source();
        let place = place_name(area, source);
        for room in layer.content().get_rooms() {
            rows.push(RoomRow {
                source,
                number: room.get_room_number(),
                title: room.get_title().to_string(),
                place: place.clone(),
                tag: None,
            });
        }
    }
    sort_rows(&mut rows);
    rows
}

/// How many rows [`rows`] lists, without laying them out: the map's rooms
/// and every readable place's own.
pub(super) fn room_count(area: &AreaCache) -> usize {
    area.room_count()
        + area
            .source_layers()
            .iter()
            .map(|layer| layer.content().get_rooms().iter().count())
            .sum::<usize>()
}

/// Title order, untitled last, then number.
fn sort_rows(rows: &mut [RoomRow]) {
    rows.sort_by_cached_key(|row| (row.title.is_empty(), row.title.to_lowercase(), row.number.0));
}

/// Whether `row` matches the filter (lowercased, trimmed): its title, place
/// or number.
fn matches(row: &RoomRow, filter: &str) -> bool {
    filter.is_empty()
        || row.title.to_lowercase().contains(filter)
        || row.place.to_lowercase().contains(filter)
        || row.number.0.to_string() == filter.trim_start_matches('#')
}

/// The rows matching `filter` that the section lays out, and how many more
/// match beyond them. Besides title, place and number, a filter naming a
/// whole tag matches the rooms carrying it in any readable place (`tagged`,
/// sorted); a row matched by its tag alone says which.
fn page(rows: Vec<RoomRow>, filter: &str, tagged: &[RoomRef]) -> (Vec<RoomRow>, usize) {
    let tag = tags::filter_tag(filter);
    let filter = filter.trim().to_lowercase();
    let mut found: Vec<RoomRow> = rows
        .into_iter()
        .filter_map(|mut row| {
            if matches(&row, &filter) {
                return Some(row);
            }
            tagged.binary_search(&(row.source, row.number)).ok()?;
            row.tag.clone_from(&tag);
            Some(row)
        })
        .collect();
    let more = found.len().saturating_sub(SHOWN);
    found.truncate(SHOWN);
    (found, more)
}

/// The rooms carrying the whole tag `filter` names, in any readable place.
fn tagged<'a>(index: &'a TagIndex, filter: &str) -> &'a [RoomRef] {
    tags::filter_tag(filter).map_or(&[], |tag| index.rooms_with(&tag))
}

impl MapEditorWindow {
    /// A row of the Rooms section: selects its room and centers it, and the
    /// inspector shows the room under a link back to the map.
    pub(super) fn open_room(&mut self, source: SourceId, number: RoomNumber) {
        let Some(area_id) = self.editor.area_id() else {
            return;
        };
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&area_id) else {
            return;
        };
        let room = if source.is_map() {
            area.get_room(&number).cloned()
        } else {
            sources::source_room(&area, source, number).cloned()
        };
        let Some(room) = room else {
            return;
        };
        let row = RoomRow {
            source,
            number,
            title: String::new(),
            place: String::new(),
            tag: None,
        };
        self.editor.select(row.entity());
        self.selection_reset();
        self.editor.center_on(
            iced::Point::new(room.get_x(), room.get_y()),
            room.get_level(),
        );
        self.inspector.resync(&self.mapper, &self.editor);
    }
}

/// The map's panel.
pub fn view<'a>(
    window: &'a MapEditorWindow,
    area: &std::sync::Arc<AreaCache>,
) -> Column<'a, Message, crate::Theme> {
    let storage = window.mapper.area_storage(area.get_id());
    let rooms = room_count(area);
    let mut summary = format!(
        "{} \u{00b7} {}",
        panels::storage_label(storage),
        crate::i18n::t!("mapper-room-count", "count" => rooms)
    );
    // A clan's map says whose it is.
    match area.meta().clan_ownership.ownership {
        Some(smudgy_cloud::clan_maps::MapOwnership::Members) => {
            summary.push_str(" \u{00b7} ");
            summary.push_str(&crate::i18n::t!("clan-share-new-map-member-owned-choice"));
        }
        Some(smudgy_cloud::clan_maps::MapOwnership::Clan) => {
            summary.push_str(" \u{00b7} ");
            summary.push_str(&crate::i18n::t!("clan-share-new-map-clan-owned"));
        }
        None => {}
    }
    let mut content = Column::new().spacing(8).padding(12).push(panels::header(
        crate::i18n::t!("mapper-panel-kind-map"),
        area.get_name().to_string(),
        summary,
    ));

    content = content.push(panels::section(
        window,
        Section::Data,
        crate::i18n::t!("mapper-panel-data-fields"),
        Some(area.properties().count()),
        || inspector::data_fields(window, area).into(),
    ));

    let secrets = secrets::secrets(area).len();
    if secrets::section_applies(
        window.secrets_apply()
            && window.mapper.area_storage(area.get_id()) == smudgy_cloud::MapStorage::Cloud,
        super::clan_secrets::can_create(area),
        secrets,
    ) {
        content = content.push(panels::section(
            window,
            Section::Secrets,
            crate::i18n::t!("mapper-secrets"),
            Some(secrets),
            || secrets::section(window, area).into(),
        ));
    }

    let index = window.tag_index(area);
    content = content.push(panels::section(
        window,
        Section::Tags,
        crate::i18n::t!("mapper-tags"),
        Some(index.len()),
        || tags_section(area, &index),
    ));

    content = content.push(panels::section(
        window,
        Section::Rooms,
        crate::i18n::t!("mapper-rooms"),
        Some(rooms),
        || rooms_section(window, area, &index),
    ));

    let shares = storage == MapStorage::Cloud
        && window.cloud.snapshot.get().signed_in
        && modals::may_share(area);
    if shares {
        let count = window
            .panel
            .map_access
            .as_ref()
            .filter(|access| access.area_id == *area.get_id())
            .and_then(panels::MapAccess::count);
        content = content.push(panels::section(
            window,
            Section::Shares,
            crate::i18n::t!("mapper-panel-shares"),
            count,
            || shares_section(window, area),
        ));
    }
    content
}

/// The Tags section: each tag the map's readable places carry, sorted, with
/// its room count in each place (the map's plain, the others' after their
/// dot). A row filters the Rooms section by its tag.
fn tags_section<'a>(area: &AreaCache, index: &TagIndex) -> ThemedElement<'a, Message> {
    if index.is_empty() {
        return panels::note(crate::i18n::t!("mapper-tags-none"));
    }
    let mut list = Column::new().spacing(2);
    for (tag, counts) in index.tags() {
        let mut line = row![text(tag.to_string()).size(13).width(Length::Fill)]
            .spacing(8)
            .align_y(Vertical::Center);
        for (place, count) in counts {
            let mut count_label = row![].spacing(3).align_y(Vertical::Center);
            if !place.is_map() {
                count_label = count_label.push(dot(sources::source_color(area, *place)));
            }
            line = line.push(count_label.push(text(count.to_string()).size(12).style(muted)));
        }
        list = list.push(
            button(line)
                .style(builtins::button::list_item)
                .width(Length::Fill)
                .padding([4, 8])
                .on_press(Message::Panel(PanelMessage::TagPicked(tag.to_string()))),
        );
    }
    list.into()
}

/// The Rooms section: a filter over every room, the first [`SHOWN`] that
/// match, and how many more there are.
fn rooms_section<'a>(
    window: &'a MapEditorWindow,
    area: &AreaCache,
    index: &TagIndex,
) -> ThemedElement<'a, Message> {
    let mut content = Column::new().spacing(8).push(
        text_input(
            crate::i18n::ts!("mapper-rooms-filter"),
            &window.panel.rooms_filter,
        )
        .size(13)
        .padding([5, 8])
        .on_input(|value| Message::Panel(PanelMessage::RoomsFilterChanged(value))),
    );
    let (found, more) = page(
        rows(area),
        &window.panel.rooms_filter,
        tagged(index, &window.panel.rooms_filter),
    );
    if found.is_empty() {
        return content
            .push(panels::note(crate::i18n::t!("mapper-rooms-none")))
            .into();
    }

    let mut list = Column::new().spacing(2);
    for room in found {
        let title = if room.title.is_empty() {
            crate::i18n::t!("mapper-room-untitled")
        } else {
            room.title.clone()
        };
        let mut line = row![text(title).size(13).width(Length::Fill)]
            .spacing(8)
            .align_y(Vertical::Center);
        if let Some(tag) = &room.tag {
            line = line.push(text(tag.clone()).size(11).style(muted));
        }
        if !room.source.is_map() {
            line = line.push(
                row![
                    dot(sources::source_color(area, room.source)),
                    text(room.place.clone()).size(12).style(muted),
                ]
                .spacing(4)
                .align_y(Vertical::Center),
            );
        }
        line = line.push(text(format!("#{}", room.number.0)).size(12).style(muted));
        list = list.push(
            button(line)
                .style(builtins::button::list_item)
                .width(Length::Fill)
                .padding([4, 8])
                .on_press(Message::Panel(PanelMessage::RoomOpened(
                    room.source,
                    room.number,
                ))),
        );
    }
    content = content.push(list);
    if more > 0 {
        content = content.push(panels::note(
            crate::i18n::t!("mapper-rooms-more", "count" => more),
        ));
    }
    content.into()
}

/// The Shares section: who has access to the map, as the Share dialog lists
/// it, and Share… to open that dialog. A viewer who shares only the map's
/// Secrets sees the button alone.
fn shares_section<'a>(window: &'a MapEditorWindow, area: &AreaCache) -> ThemedElement<'a, Message> {
    let mut content = Column::new().spacing(8);
    let access = window
        .panel
        .map_access
        .as_ref()
        .filter(|access| access.area_id == *area.get_id());
    if let Some(access) = access {
        content = content.push(map_access_rows(window, area, access));
    }
    let open = if area.meta().clan_id.is_some() {
        Message::MapAccessRequested(*area.get_id())
    } else {
        Message::ShareDialogRequested
    };
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
                .on_press(open),
        )
        .into()
}

/// Who has access to the map: every grant reaching it, re-shares stepped in
/// under their sharer.
fn map_access_rows<'a>(
    window: &MapEditorWindow,
    area: &AreaCache,
    access: &panels::MapAccess,
) -> ThemedElement<'a, Message> {
    let mut list = column![].spacing(6);
    match &access.grants {
        None => list = list.push(panels::note(crate::i18n::t!("mapper-loading"))),
        Some(Err(error)) => {
            list = list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(nodes)) if nodes.is_empty() => {
            list = list.push(panels::note(crate::i18n::t!("mapper-not-shared")));
        }
        Some(Ok(nodes)) => {
            let handles = modals::tree_handles(nodes);
            let viewer_id = window
                .cloud
                .snapshot
                .get()
                .profile
                .as_ref()
                .map(|profile| profile.id);
            let owner_nickname = area.meta().owner_nickname.clone();
            for node in nodes {
                let grant = &node.grant;
                let grantee = node
                    .grantee_nickname
                    .clone()
                    .unwrap_or_else(|| grant.grantee_id.to_string());
                let shared_by = modals::shared_by(
                    grant,
                    viewer_id,
                    area.is_owned(),
                    owner_nickname.as_deref(),
                    &handles,
                );
                list = list.push(panels::access_row(
                    grantee,
                    format!("{} \u{00b7} {shared_by}", modals::tree_badges(grant)),
                    node.depth,
                ));
            }
        }
    }
    list.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(title: &str, place: &str, number: i32) -> RoomRow {
        RoomRow {
            source: SourceId::Map,
            number: RoomNumber(number),
            title: title.to_string(),
            place: place.to_string(),
            tag: None,
        }
    }

    #[test]
    fn the_filter_matches_title_place_or_number() {
        let library = row("The Library", "Bookcase", 12);
        assert!(matches(&library, ""));
        assert!(matches(&library, "libr"));
        assert!(matches(&library, "book"));
        assert!(matches(&library, "12"));
        assert!(matches(&library, "#12"));
        assert!(!matches(&library, "1"));
        assert!(!matches(&library, "cellar"));
    }

    #[test]
    fn rooms_list_by_title_with_untitled_last() {
        let mut rows = vec![
            row("", "", 1),
            row("cellar", "", 9),
            row("Attic", "", 4),
            row("Cellar", "", 3),
        ];
        sort_rows(&mut rows);
        let order: Vec<i32> = rows.iter().map(|row| row.number.0).collect();
        assert_eq!(order, [4, 3, 9, 1]);
    }

    #[test]
    fn the_section_lays_out_the_first_rooms_and_counts_the_rest() {
        let rows: Vec<RoomRow> = (1..=SHOWN as i32 + 25)
            .map(|number| row(&format!("Room {number}"), "", number))
            .collect();
        let (shown, more) = page(rows.clone(), "", &[]);
        assert_eq!(shown.len(), SHOWN);
        assert_eq!(more, 25);

        // A filter narrows the list; the "more" line goes when all fit.
        let (shown, more) = page(rows, "  #42 ", &[]);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].number, RoomNumber(42));
        assert_eq!(more, 0);
    }

    #[test]
    fn a_whole_tag_filters_rooms_in_any_place_and_says_so() {
        let secret = SourceId::Secret(smudgy_cloud::Uuid::from_u128(7));
        let rows = vec![
            row("Vault door", "", 1),
            row("Hall", "", 2),
            RoomRow {
                source: secret,
                ..row("Strongroom", "Bookcase", 3)
            },
            row("Cellar", "", 4),
        ];
        // VAULT tags map room 2 (in some place) and the Secret's room 3.
        let mut tagged = vec![(SourceId::Map, RoomNumber(2)), (secret, RoomNumber(3))];
        tagged.sort_unstable();

        let (shown, _) = page(rows.clone(), "vault", &tagged);
        let found: Vec<(i32, Option<&str>)> = shown
            .iter()
            .map(|row| (row.number.0, row.tag.as_deref()))
            .collect();
        // Room 1 matches by title and needs no tag shown; 2 and 3 by tag.
        assert_eq!(found, [(1, None), (2, Some("VAULT")), (3, Some("VAULT"))]);
        // The tag filter is whole-tag: a partial name matches titles only.
        let (shown, _) = page(rows, "vaul", &[]);
        assert_eq!(shown.len(), 1);
    }

    #[test]
    fn a_room_row_selects_the_room_where_it_lives() {
        let secret = SourceId::Secret(smudgy_cloud::Uuid::from_u128(7));
        assert_eq!(row("Hall", "", 3).entity(), EntityId::Room(RoomNumber(3)));
        let in_secret = RoomRow {
            source: secret,
            ..row("Vault", "Bookcase", 5)
        };
        assert_eq!(
            in_secret.entity(),
            EntityId::SourceRoom(secret, RoomNumber(5))
        );
    }
}
