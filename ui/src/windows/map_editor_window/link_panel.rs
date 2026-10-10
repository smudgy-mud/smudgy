//! The link editor in the inspector. With a room selected, its Exits: a
//! compass of the directions its exits leave in, one row per link touching
//! it, and "+ Add exit…"; a new exit leads to the room picked, or nowhere
//! yet ("No destination yet"). With a link selected, the link: its two ends in
//! travel order, each with its room and "Change ▾", the direction its exit
//! leaves in, its weight and command; two-way or one-way and swapping ends;
//! its doors; its appearance; and "Remove link". Rooms are named by title
//! and place ([`super::links`]); edits are commands
//! ([`super::link_commands`]) through the window's funnel.

use std::collections::HashSet;
use std::fmt;

use iced::alignment::Vertical;
use iced::widget::{Column, Space, button, checkbox, container, pick_list, row, text, text_input};
use iced::{Color, Length, Padding, Task};
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{AreaId, ConnectionId, ExitDirection, SourceId};
use smudgy_map_widget::map_editor::{EntityId, PlacedRoom};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::commands::{ExitRef, FieldId};
use super::link_commands::{self, End, Sides};
use super::links::{
    self, Destination, DoorSide, DoorState, LinkPlaces, LinkRow, LinkView, RoomId, RoomName, Way,
};
use super::{Event, MapEditorWindow, Message};

/// How many rooms each picker group lists before "type to narrow".
const PICKER_LIMIT: usize = 40;

/// Where a destination picker's pick goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A new link from the selected room, leaving in this direction.
    NewExit(ExitDirection),
    /// Moves this end of the selected link.
    Change(End),
}

/// An open destination picker.
#[derive(Debug, Clone)]
pub struct Picker {
    pub purpose: Purpose,
    pub query: String,
    /// The other map whose rooms it lists, once picked under "Other map…".
    pub other_map: Option<AreaId>,
    /// Listing the other maps to pick one.
    pub choosing_map: bool,
}

impl Picker {
    fn new(purpose: Purpose) -> Self {
        Self {
            purpose,
            query: String::new(),
            other_map: None,
            choosing_map: false,
        }
    }
}

/// A door block: one for both sides, or one side's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DoorSlot {
    Both,
    Side(End),
}

impl DoorSlot {
    fn sides(self) -> Sides {
        match self {
            Self::Both => Sides::Both,
            Self::Side(end) => Sides::One(end),
        }
    }

    /// The end whose values the block shows.
    fn shown(self) -> End {
        match self {
            Self::Both => End::From,
            Self::Side(end) => end,
        }
    }
}

/// A text field behind a checkbox ("☐ command", "☐ named", "☐ opens
/// with"): checked while it holds text, or while it is open and empty. An
/// open field left empty stays checked, writing no name or command, until
/// Enter is pressed in it or its box is unchecked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Command(End),
    Name(DoorSlot),
    OpensWith(DoorSlot),
}

impl Field {
    fn id(self, window: iced::window::Id) -> iced::widget::Id {
        iced::widget::Id::from(format!("link-field-{window:?}-{self:?}"))
    }
}

/// The link editor's state: the compass highlight, an open picker, door
/// sides shown apart, checked fields still empty, and what is typed.
#[derive(Debug, Clone, Default)]
pub struct LinkPanel {
    /// The selection this state belongs to.
    key: Option<(Option<AreaId>, Option<EntityId>)>,
    pub highlight: Option<ExitDirection>,
    pub picker: Option<Picker>,
    /// "Same on both sides" unchecked while the two sides still agree.
    pub doors_apart: bool,
    /// Checked fields whose input is open, empty or not.
    pub open_fields: HashSet<Field>,
    /// What is typed in each end's weight.
    pub weight: [String; 2],
    /// What is typed in each text field behind a checkbox.
    pub texts: std::collections::HashMap<Field, String>,
    /// The thickness slider while it is dragged.
    pub thickness: Option<f32>,
}

fn index(end: End) -> usize {
    match end {
        End::From => 0,
        End::To => 1,
    }
}

impl LinkPanel {
    /// Keeps the state while the selection holds, else starts over; the
    /// typed text is read again from `view` either way.
    pub fn resync(&mut self, key: (Option<AreaId>, Option<EntityId>), view: Option<&LinkView>) {
        if self.key.as_ref() != Some(&key) {
            *self = Self::default();
            self.key = Some(key);
        }
        self.weight = [String::new(), String::new()];
        self.texts.clear();
        self.thickness = None;
        let Some(view) = view else {
            return;
        };
        for end in [End::From, End::To] {
            let Some(exit) = exit_at(view, end) else {
                continue;
            };
            self.weight[index(end)] = exit.weight.to_string();
            if let Some(command) = &exit.command {
                self.texts.insert(Field::Command(end), command.clone());
            }
        }
        for slot in [
            DoorSlot::Both,
            DoorSlot::Side(End::From),
            DoorSlot::Side(End::To),
        ] {
            let Some(side) = door_at(view, slot.shown()) else {
                continue;
            };
            if let Some(name) = side.name {
                self.texts.insert(Field::Name(slot), name);
            }
            if let Some(opens) = side.opens_with {
                self.texts.insert(Field::OpensWith(slot), opens);
            }
        }
    }

    fn text(&self, field: Field) -> &str {
        self.texts.get(&field).map_or("", String::as_str)
    }

    /// Whether a field's checkbox is checked: it holds text, or it is open.
    fn checked(&self, field: Field) -> bool {
        !self.text(field).is_empty() || self.open_fields.contains(&field)
    }
}

/// The exit leaving `end` of `view`, wherever it is kept (the way back
/// another map keeps is read from it there by the caller).
fn exit_at(view: &LinkView, end: End) -> Option<&smudgy_cloud::mapper::exit_cache::ExitCache> {
    match end {
        End::From => view.from.exit.as_ref(),
        End::To => view.to.exit.as_ref().or(view.return_exit.as_ref()),
    }
}

/// The door side `end` of `view` keeps.
fn door_at(view: &LinkView, end: End) -> Option<DoorSide> {
    exit_at(view, end).map(DoorSide::of)
}

/// The notice for a door whose name or opening command is longer than the
/// server takes (in characters); `None` for a door it takes.
fn door_too_long(side: &DoorSide) -> Option<String> {
    let over = |value: &Option<String>, limit: usize| {
        value
            .as_ref()
            .is_some_and(|value| value.chars().count() > limit)
    };
    if over(&side.name, smudgy_cloud::DOOR_NAME_LIMIT) {
        return Some(crate::i18n::t!(
            "link-door-name-too-long",
            "limit" => smudgy_cloud::DOOR_NAME_LIMIT
        ));
    }
    if over(&side.opens_with, smudgy_cloud::DOOR_COMMAND_LIMIT) {
        return Some(crate::i18n::t!(
            "link-opens-with-too-long",
            "limit" => smudgy_cloud::DOOR_COMMAND_LIMIT
        ));
    }
    None
}

#[derive(Debug, Clone)]
pub enum LinkMessage {
    /// A compass direction: highlight its rows, or start a link there.
    Compass(ExitDirection),
    /// "+ Add exit…".
    AddExit,
    /// The new exit's direction.
    NewDirection(ExitDirection),
    /// A row: select its link.
    RowOpened(ConnectionId),
    /// A row's trash button: remove its link.
    RowRemoved(ConnectionId),
    Query(String),
    /// Enter in the picker: its first room.
    QuerySubmitted,
    Picked(RoomId),
    /// "No destination yet": the new exit leads nowhere for now.
    NoDestination,
    /// "Other map…": list the other maps.
    OtherMaps,
    MapPicked(AreaId),
    /// Back from another map to this one.
    ThisMap,
    PickerClosed,
    /// "Change ▾" on an end.
    Change(End),
    /// The link's "In" picker.
    MoveTo(SourceId),
    Leaves(End, ExitDirection),
    /// A one-way link's far end: the direction it arrives from.
    ArrivesFrom(ExitDirection),
    Weight(End, String),
    Checked(Field, bool),
    Typed(Field, String),
    /// `true` two-way, `false` one-way.
    TwoWay(bool),
    Swap,
    SameDoors(bool),
    Door(DoorSlot, DoorState),
    Hidden(DoorSlot, bool),
    ColorReset,
    Thickness(f32),
    ThicknessReleased,
    Remove,
    /// Enter in a checked field: left empty, it unchecks.
    Submitted(Field),
}

/// A direction in a pick list, by its translated name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectionChoice(ExitDirection);

impl fmt::Display for DirectionChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(direction_name(self.0))
    }
}

fn direction_choices() -> Vec<DirectionChoice> {
    ExitDirection::ALL
        .iter()
        .copied()
        .map(DirectionChoice)
        .collect()
}

/// A direction picker; plain text where the viewer can't change it.
fn direction_picker<'a>(
    direction: Option<ExitDirection>,
    writable: bool,
    on_pick: impl Fn(ExitDirection) -> Message + 'a,
) -> ThemedElement<'a, Message> {
    if writable {
        pick_list(
            direction_choices(),
            direction.map(DirectionChoice),
            move |choice: DirectionChoice| on_pick(choice.0),
        )
        .text_size(12)
        .into()
    } else {
        text(direction.map_or("", direction_name)).size(12).into()
    }
}

/// A direction's translated name.
fn direction_name(direction: ExitDirection) -> &'static str {
    match direction {
        ExitDirection::North => crate::i18n::ts!("direction-north"),
        ExitDirection::East => crate::i18n::ts!("direction-east"),
        ExitDirection::South => crate::i18n::ts!("direction-south"),
        ExitDirection::West => crate::i18n::ts!("direction-west"),
        ExitDirection::Up => crate::i18n::ts!("direction-up"),
        ExitDirection::Down => crate::i18n::ts!("direction-down"),
        ExitDirection::Northeast => crate::i18n::ts!("direction-northeast"),
        ExitDirection::Northwest => crate::i18n::ts!("direction-northwest"),
        ExitDirection::Southeast => crate::i18n::ts!("direction-southeast"),
        ExitDirection::Southwest => crate::i18n::ts!("direction-southwest"),
        ExitDirection::In => crate::i18n::ts!("direction-in"),
        ExitDirection::Out => crate::i18n::ts!("direction-out"),
        ExitDirection::Special => crate::i18n::ts!("direction-special"),
        ExitDirection::Other => crate::i18n::ts!("direction-other"),
    }
}

/// A direction's short label on the compass and in rows.
fn compass_label(direction: ExitDirection) -> &'static str {
    match direction {
        ExitDirection::North => crate::i18n::ts!("compass-north"),
        ExitDirection::East => crate::i18n::ts!("compass-east"),
        ExitDirection::South => crate::i18n::ts!("compass-south"),
        ExitDirection::West => crate::i18n::ts!("compass-west"),
        ExitDirection::Up => crate::i18n::ts!("compass-up"),
        ExitDirection::Down => crate::i18n::ts!("compass-down"),
        ExitDirection::Northeast => crate::i18n::ts!("compass-northeast"),
        ExitDirection::Northwest => crate::i18n::ts!("compass-northwest"),
        ExitDirection::Southeast => crate::i18n::ts!("compass-southeast"),
        ExitDirection::Southwest => crate::i18n::ts!("compass-southwest"),
        ExitDirection::In => crate::i18n::ts!("compass-in"),
        ExitDirection::Out => crate::i18n::ts!("compass-out"),
        ExitDirection::Special => crate::i18n::ts!("compass-special"),
        ExitDirection::Other => crate::i18n::ts!("compass-other"),
    }
}

/// The command a MUD takes for a direction, shown as the placeholder of
/// an exit's own command: game input, so never translated.
fn direction_word(direction: ExitDirection) -> &'static str {
    match direction {
        ExitDirection::North => "north",
        ExitDirection::East => "east",
        ExitDirection::South => "south",
        ExitDirection::West => "west",
        ExitDirection::Up => "up",
        ExitDirection::Down => "down",
        ExitDirection::Northeast => "northeast",
        ExitDirection::Northwest => "northwest",
        ExitDirection::Southeast => "southeast",
        ExitDirection::Southwest => "southwest",
        ExitDirection::In => "in",
        ExitDirection::Out => "out",
        ExitDirection::Special | ExitDirection::Other => "",
    }
}

/// A room's name on one line, as every list of exits names it: its map
/// when it is another ("Catacombs ›"), a dot in its place's color, then
/// "Crypt #1 · Bone Altar" (a map room just "#1 · West Gate").
pub(super) fn named_line<'a>(name: &RoomName, size: u32) -> ThemedElement<'a, Message> {
    let mut line = row![].spacing(4).align_y(Vertical::Center);
    if let Some(map) = &name.map_name {
        line = line.push(
            text(crate::i18n::t!("mapper-other-map-prefix", "map" => map.clone())).size(size),
        );
    }
    if let Some((_, color)) = &name.place {
        line = line.push(super::secrets::dot(*color));
    }
    line.push(text(place_room_label(name)).size(size)).into()
}

/// A room's label within its place: "Crypt #1 · Bone Altar", or, for a map
/// room, "#1 · West Gate".
fn place_room_label(name: &RoomName) -> String {
    let number = name.id.number.to_string();
    match (&name.place, name.title.is_empty()) {
        (None, _) => room_label(name),
        (Some((place, _)), true) => crate::i18n::t!(
            "mapper-place-room-name",
            "place" => place.clone(),
            "number" => number
        ),
        (Some((place, _)), false) => crate::i18n::t!(
            "mapper-place-room-name-titled",
            "place" => place.clone(),
            "number" => number,
            "title" => name.title.clone()
        ),
    }
}

/// A room's label: "#1 · West Gate", or "#1" without a title.
fn room_label(name: &RoomName) -> String {
    if name.title.is_empty() {
        crate::i18n::t!("mapper-room-name", "number" => name.id.number.to_string())
    } else {
        crate::i18n::t!(
            "mapper-room-name-titled",
            "number" => name.id.number.to_string(),
            "title" => name.title.clone()
        )
    }
}

fn muted(theme: &crate::Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

fn section_label<'a>(label: String) -> ThemedElement<'a, Message> {
    text(label.to_uppercase()).size(11).style(muted).into()
}

/// A place's or map's pill: a Secret (or Private) in its color with a dot,
/// another map plain.
fn badge<'a>(label: String, color: Option<Color>) -> ThemedElement<'a, Message> {
    let mut content = row![].spacing(5).align_y(Vertical::Center);
    if color.is_some() {
        content = content.push(
            text("\u{25CF}")
                .size(10)
                .style(move |_theme: &crate::Theme| text::Style { color }),
        );
    }
    content = content.push(text(label).size(11));
    container(content)
        .padding(Padding {
            top: 1.0,
            bottom: 1.0,
            left: 7.0,
            right: 7.0,
        })
        .style(move |theme: &crate::Theme| {
            let tint = color.unwrap_or(theme.styles.general.border);
            container::Style {
                background: color.map(|color| Color { a: 0.15, ..color }.into()),
                border: iced::Border {
                    color: tint,
                    width: 1.0,
                    radius: 8.0.into(),
                },
                text_color: Some(theme.styles.text.normal),
                ..Default::default()
            }
        })
        .into()
}

/// The badges naming where a room is when it isn't on this map's own
/// ground: another map plain, a place in its color; and the place keeping
/// the link (`kept`) when that differs from the room's.
fn badges<'a>(
    name: &RoomName,
    kept: Option<(String, Option<Color>)>,
) -> Option<ThemedElement<'a, Message>> {
    let mut pills: Vec<ThemedElement<'a, Message>> = Vec::new();
    if let Some(map) = &name.map_name {
        pills.push(badge(map.clone(), None));
    }
    if let Some((place, color)) = &name.place {
        pills.push(badge(place.clone(), *color));
    }
    if let Some((place, color)) = kept
        && name
            .place
            .as_ref()
            .is_none_or(|(room_place, _)| *room_place != place)
    {
        pills.push(badge(place, color));
    }
    (!pills.is_empty()).then(|| {
        crate::widgets::wrap_row::wrap_row(pills)
            .spacing(6.0, 4.0)
            .into()
    })
}

/// "hidden", and the door: "closed door", "“gate”, locked".
fn door_summary(side: &DoorSide) -> Option<String> {
    let door = match side.state {
        DoorState::None => None,
        state => {
            let state = match state {
                DoorState::Open => "open",
                DoorState::Closed => "closed",
                _ => "locked",
            };
            Some(match &side.name {
                Some(name) => {
                    crate::i18n::t!("link-door-named", "name" => name.clone(), "state" => state)
                }
                None => crate::i18n::t!("link-door", "state" => state),
            })
        }
    };
    match (side.hidden, door) {
        (false, None) => None,
        (true, None) => Some(crate::i18n::t!("link-hidden")),
        (false, Some(door)) => Some(door),
        (true, Some(door)) => Some(format!(
            "{} \u{00B7} {door}",
            crate::i18n::t!("link-hidden")
        )),
    }
}

/// The place a link is kept in, when it isn't the map: its name and color.
fn kept_in(area: &AreaCache, place: SourceId) -> Option<(String, Option<Color>)> {
    (!place.is_map()).then(|| {
        (
            super::secrets::place_name(area, place),
            smudgy_map_widget::sources::source_color(area, place),
        )
    })
}

impl MapEditorWindow {
    /// The selected room, map or place, as the canvas places it.
    fn selected_link_room(&self) -> Option<PlacedRoom> {
        match self.editor.selection().single()? {
            EntityId::Room(number) => Some(PlacedRoom::map(number)),
            EntityId::SourceRoom(source, number) => Some(PlacedRoom { source, number }),
            _ => None,
        }
    }

    /// The selected link, its ends in travel order from the room the
    /// selection came from.
    pub(super) fn selected_link_view(&self) -> Option<LinkView> {
        let Some(EntityId::Connection(id)) = self.editor.selection().single() else {
            return None;
        };
        let atlas = self.mapper.get_current_atlas();
        let map = atlas.get_area(&self.editor.area_id()?)?;
        links::link_view(&atlas, &map, id, self.editor.connection_anchor())
    }

    /// Selects link `id`, from room `from` when it is one of its ends.
    fn select_link(&mut self, id: ConnectionId, from: Option<PlacedRoom>) {
        match from {
            Some(room) => self.editor.select_link_from(id, room.into()),
            None => self.editor.select(EntityId::Connection(id)),
        }
        self.selection_reset();
        self.inspector.resync(&self.mapper, &self.editor);
    }

    fn notice(&mut self, key: &'static str) {
        self.editor_notice = Some((std::time::Instant::now(), crate::i18n::translate(key)));
    }

    /// Pushes a link edit; a refusal (where it may not write) shows the
    /// funnel's notice.
    fn push_link_command(
        &mut self,
        command: Option<super::commands::Command>,
    ) -> Update<Message, Event> {
        let update = self.push_command(command);
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    /// The room a picker's list is nearest to and leaves out: the selected
    /// room for a new exit, the other end for an end being moved.
    fn picker_origin(&self, purpose: Purpose) -> Option<RoomId> {
        let map = self.editor.area_id()?;
        match purpose {
            Purpose::NewExit(_) => Some(RoomId::on(map, self.selected_link_room()?)),
            Purpose::Change(End::To) => self.selected_link_view()?.from.room.room(),
            Purpose::Change(End::From) => self
                .selected_link_view()?
                .to
                .room
                .room()
                .filter(|room| room.map == map),
        }
    }

    /// Whether the open picker may list other maps: for a new exit, and
    /// for a link's To end.
    fn picker_reaches_other_maps(purpose: Purpose) -> bool {
        !matches!(purpose, Purpose::Change(End::From))
    }

    /// The rooms the open picker lists, first to last.
    fn picker_rooms(&self) -> Vec<RoomName> {
        let Some(picker) = &self.inspector.links.picker else {
            return Vec::new();
        };
        let atlas = self.mapper.get_current_atlas();
        let Some(map_id) = self.editor.area_id() else {
            return Vec::new();
        };
        if let Some(other) = picker.other_map.and_then(|id| atlas.get_area(&id)) {
            return links::other_map_picks(
                &atlas,
                map_id,
                &other,
                true,
                &picker.query,
                PICKER_LIMIT,
            )
            .rooms;
        }
        let Some(map) = atlas.get_area(&map_id) else {
            return Vec::new();
        };
        let origin = self.picker_origin(picker.purpose);
        let near = origin.and_then(|origin| links::near(&atlas, origin));
        let places = links::reachable_places(&map, origin);
        let (this_map, place_rooms) = links::this_map_picks(
            &atlas,
            &map,
            near,
            origin,
            &places,
            &picker.query,
            PICKER_LIMIT,
        );
        this_map
            .rooms
            .into_iter()
            .chain(place_rooms.rooms)
            .collect()
    }

    /// Opens a picker for `purpose`, with the canvas picking rooms too.
    fn open_picker(&mut self, purpose: Purpose) -> Update<Message, Event> {
        self.inspector.links.picker = Some(Picker::new(purpose));
        self.inspector.links.highlight = None;
        Update::with_task(iced::widget::operation::focus(picker_input_id(
            self.window_id,
        )))
    }

    /// A room picked in the picker or on the canvas.
    fn link_room_picked(&mut self, room: RoomId) -> Update<Message, Event> {
        let Some(picker) = self.inspector.links.picker.take() else {
            return Update::none();
        };
        let Some(map) = self.editor.area_id() else {
            return Update::none();
        };
        let atlas = self.mapper.get_current_atlas();
        match picker.purpose {
            Purpose::NewExit(direction) => {
                let Some(from) = self.selected_link_room() else {
                    return Update::none();
                };
                if room == RoomId::on(map, from) {
                    return Update::none();
                }
                match link_commands::create(
                    map,
                    RoomId::on(map, from),
                    direction,
                    room,
                    self.add_to(),
                ) {
                    Ok((command, id)) => {
                        let update = self.push_command(Some(command));
                        if self
                            .mapper
                            .get_current_atlas()
                            .get_area(&map)
                            .is_some_and(|area| area.find_connection(id).is_some())
                        {
                            self.select_link(id, Some(from));
                        } else {
                            self.inspector.resync(&self.mapper, &self.editor);
                        }
                        update
                    }
                    Err(key) => {
                        self.notice(key);
                        Update::none()
                    }
                }
            }
            Purpose::Change(end) => {
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                let keep = match end {
                    End::From => view.to.room.room(),
                    End::To => view.from.room.room(),
                };
                if keep == Some(room) {
                    return Update::none();
                }
                match link_commands::retarget(&atlas, map, &view, end, room) {
                    Ok((command, id)) => {
                        let update = self.push_command(Some(command));
                        let from = match end {
                            End::From => room.placed_on(map),
                            End::To => view.from.room.room().and_then(|room| room.placed_on(map)),
                        };
                        if self
                            .mapper
                            .get_current_atlas()
                            .get_area(&map)
                            .is_some_and(|area| area.find_connection(id).is_some())
                        {
                            self.select_link(id, from);
                        } else {
                            self.inspector.resync(&self.mapper, &self.editor);
                        }
                        update
                    }
                    Err(key) => {
                        self.notice(key);
                        Update::none()
                    }
                }
            }
        }
    }

    /// "No destination yet" in a new exit's picker: the exit leaves the
    /// selected room in the picker's direction and leads nowhere for now.
    /// The room stays selected, its new row "→ (no destination)" among its
    /// links, where "Change ▾" gives it a destination later.
    fn dangling_exit(&mut self) -> Update<Message, Event> {
        let Some(Purpose::NewExit(direction)) = self
            .inspector
            .links
            .picker
            .as_ref()
            .map(|picker| picker.purpose)
        else {
            return Update::none();
        };
        let (Some(map), Some(from)) = (self.editor.area_id(), self.selected_link_room()) else {
            return Update::none();
        };
        self.inspector.links.picker = None;
        match link_commands::create_dangling(map, RoomId::on(map, from), direction, self.add_to()) {
            Ok((command, _)) => {
                let update = self.push_link_command(Some(command));
                self.inspector.links.highlight = Some(direction);
                update
            }
            Err(key) => {
                self.notice(key);
                Update::none()
            }
        }
    }

    /// A room picked on the canvas while a picker is open.
    pub(super) fn room_picked(&mut self, room: PlacedRoom) -> Update<Message, Event> {
        let Some(map) = self.editor.area_id() else {
            return Update::none();
        };
        if self.inspector.links.picker.is_none() {
            return Update::none();
        }
        self.link_room_picked(RoomId::on(map, room))
    }

    /// Escape closes an open picker; whether it did.
    pub(super) fn close_link_picker(&mut self) -> bool {
        self.inspector.links.picker.take().is_some()
    }

    /// The window-level follow-ups of the link editor after any message:
    /// the canvas picks rooms while a picker is open.
    pub(super) fn sync_link_picking(&mut self) {
        let picking = self.inspector.links.picker.is_some()
            && matches!(
                super::panels::shown(self, self.editor.area_id().is_some()),
                super::panels::Panel::Selection
            );
        if self.editor.picking() != picking {
            self.editor.set_picking(picking);
        }
    }

    /// The exit an end's direction edit changes, as the editor addresses it.
    fn end_ref(&self, view: &LinkView, end: End) -> Option<ExitRef> {
        let side = match end {
            End::From => &view.from,
            End::To => &view.to,
        };
        Some(ExitRef {
            area_id: self.editor.area_id()?,
            place: view.place,
            room: side.doc_room?,
            id: side.exit.as_ref()?.id,
        })
    }

    /// Edits the exits `sides` of the selected link.
    fn edit_link_exits(
        &mut self,
        sides: Sides,
        field: FieldId,
        change: impl Fn(&mut smudgy_cloud::ExitUpdates),
    ) -> Update<Message, Event> {
        let Some(view) = self.selected_link_view() else {
            return Update::none();
        };
        let Some(map) = self.editor.area_id() else {
            return Update::none();
        };
        let command = link_commands::edit_exits(
            &self.mapper.get_current_atlas(),
            map,
            &view,
            sides,
            field,
            change,
        );
        self.push_command(command)
    }

    /// Writes door side `side` to the exits of `slot`; consecutive writes
    /// of the same `field` make one undo step.
    fn write_door_field(
        &mut self,
        slot: DoorSlot,
        side: DoorSide,
        field: FieldId,
    ) -> Update<Message, Event> {
        if let Some(refusal) = door_too_long(&side) {
            self.editor_notice = Some((std::time::Instant::now(), refusal));
            return Update::none();
        }
        self.edit_link_exits(slot.sides(), field, move |updates| side.write(updates))
    }

    fn write_door(&mut self, slot: DoorSlot, side: DoorSide) -> Update<Message, Event> {
        self.write_door_field(slot, side, FieldId::Flags)
    }

    /// The door side `slot` shows now.
    fn door_side(&self, slot: DoorSlot) -> Option<DoorSide> {
        let view = self.selected_link_view()?;
        door_at(&view, slot.shown())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn update_links(&mut self, message: LinkMessage) -> Update<Message, Event> {
        match message {
            LinkMessage::Compass(direction) => {
                let used = self
                    .room_rows()
                    .iter()
                    .any(|row| row.way != Way::In && row.direction == Some(direction));
                if used {
                    let links = &mut self.inspector.links;
                    links.picker = None;
                    links.highlight = (links.highlight != Some(direction)).then_some(direction);
                    Update::none()
                } else {
                    self.open_picker(Purpose::NewExit(direction))
                }
            }
            LinkMessage::AddExit => {
                let used = links::used_directions(&self.room_rows());
                let direction = ExitDirection::ALL
                    .into_iter()
                    .find(|direction| !used.contains(direction))
                    .unwrap_or(ExitDirection::North);
                self.open_picker(Purpose::NewExit(direction))
            }
            LinkMessage::NewDirection(direction) => {
                if let Some(picker) = &mut self.inspector.links.picker
                    && matches!(picker.purpose, Purpose::NewExit(_))
                {
                    picker.purpose = Purpose::NewExit(direction);
                }
                Update::none()
            }
            LinkMessage::RowOpened(id) => {
                let from = self.selected_link_room();
                self.select_link(id, from);
                Update::none()
            }
            LinkMessage::RowRemoved(id) => {
                let Some(map_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(map) = atlas.get_area(&map_id) else {
                    return Update::none();
                };
                let Some(view) = links::link_view(&atlas, &map, id, None) else {
                    return Update::none();
                };
                self.push_link_command(link_commands::remove(&atlas, map_id, &view))
            }
            LinkMessage::Query(query) => {
                if let Some(picker) = &mut self.inspector.links.picker {
                    picker.query = query;
                }
                Update::none()
            }
            LinkMessage::QuerySubmitted => {
                let choosing = self
                    .inspector
                    .links
                    .picker
                    .as_ref()
                    .is_some_and(|picker| picker.choosing_map);
                if choosing {
                    let first = self.picker_maps().into_iter().next();
                    return match first {
                        Some((id, _)) => self.update_links(LinkMessage::MapPicked(id)),
                        None => Update::none(),
                    };
                }
                match self.picker_rooms().into_iter().next() {
                    Some(room) => self.link_room_picked(room.id),
                    None => Update::none(),
                }
            }
            LinkMessage::Picked(room) => self.link_room_picked(room),
            LinkMessage::NoDestination => self.dangling_exit(),
            LinkMessage::OtherMaps => {
                if let Some(picker) = &mut self.inspector.links.picker {
                    picker.choosing_map = true;
                    picker.other_map = None;
                    picker.query.clear();
                }
                Update::with_task(iced::widget::operation::focus(picker_input_id(
                    self.window_id,
                )))
            }
            LinkMessage::MapPicked(id) => {
                if let Some(picker) = &mut self.inspector.links.picker {
                    picker.choosing_map = false;
                    picker.other_map = Some(id);
                    picker.query.clear();
                }
                Update::with_task(iced::widget::operation::focus(picker_input_id(
                    self.window_id,
                )))
            }
            LinkMessage::ThisMap => {
                if let Some(picker) = &mut self.inspector.links.picker {
                    picker.choosing_map = false;
                    picker.other_map = None;
                    picker.query.clear();
                }
                Update::none()
            }
            LinkMessage::PickerClosed => {
                self.inspector.links.picker = None;
                Update::none()
            }
            LinkMessage::Change(end) => {
                let open = self
                    .inspector
                    .links
                    .picker
                    .as_ref()
                    .is_some_and(|picker| picker.purpose == Purpose::Change(end));
                if open {
                    self.inspector.links.picker = None;
                    Update::none()
                } else {
                    self.open_picker(Purpose::Change(end))
                }
            }
            LinkMessage::MoveTo(place) => {
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                self.move_link(view.connection, view.place, place)
            }
            LinkMessage::Leaves(end, direction) => {
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                let Some(map) = self.editor.area_id() else {
                    return Update::none();
                };
                if view.to.exit.is_some() {
                    let command = link_commands::set_leaves(
                        &self.mapper.get_current_atlas(),
                        map,
                        &view,
                        end,
                        direction,
                    );
                    return self.push_link_command(command);
                }
                // Into another map that keeps the way back, the other exit
                // arrives from the turned end's new direction, as within a
                // map; each map writes its own exit.
                let atlas = self.mapper.get_current_atlas();
                let arrival = link_commands::arrival_follows(&atlas, map, &view, end, direction);
                if end == End::To {
                    let command = link_commands::edit_exits(
                        &atlas,
                        map,
                        &view,
                        Sides::One(End::To),
                        FieldId::FromDirection,
                        move |updates| {
                            updates.from_direction = Some(direction);
                        },
                    );
                    let command = match arrival {
                        Some((redo, undo)) => command.map(|command| command.also(redo, undo)),
                        None => command,
                    };
                    return self.push_command(command);
                }
                match self.end_ref(&view, End::From) {
                    Some(exit_ref) => self.commit_exit_direction_with(
                        exit_ref,
                        FieldId::FromDirection,
                        direction,
                        false,
                        arrival,
                    ),
                    None => Update::none(),
                }
            }
            LinkMessage::ArrivesFrom(direction) => {
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                match self.end_ref(&view, End::From) {
                    Some(exit_ref) => {
                        self.commit_exit_direction(exit_ref, FieldId::Destination, direction, true)
                    }
                    None => Update::none(),
                }
            }
            LinkMessage::Weight(end, value) => {
                let parsed = value
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|weight| weight.is_finite() && *weight >= 0.0);
                self.inspector.links.weight[index(end)] = value;
                match parsed {
                    Some(weight) => {
                        self.edit_link_exits(Sides::One(end), FieldId::Weight, move |updates| {
                            updates.weight = Some(weight);
                        })
                    }
                    None => Update::none(),
                }
            }
            LinkMessage::Checked(field, checked) => {
                let links = &mut self.inspector.links;
                if checked {
                    links.open_fields.insert(field);
                    // A door's name starts as "door".
                    if let Field::Name(slot) = field
                        && links.text(field).is_empty()
                    {
                        let name = crate::i18n::t!("link-door-default-name");
                        links.texts.insert(field, name.clone());
                        let mut update = self.write_name(slot, Some(name));
                        update.task = Task::batch([
                            update.task,
                            iced::widget::operation::focus(field.id(self.window_id)),
                        ]);
                        return update;
                    }
                    return Update::with_task(iced::widget::operation::focus(
                        field.id(self.window_id),
                    ));
                }
                links.open_fields.remove(&field);
                let had = !links.text(field).is_empty();
                links.texts.remove(&field);
                if !had {
                    return Update::none();
                }
                self.write_link_field(field, None)
            }
            LinkMessage::Typed(field, value) => {
                self.inspector.links.open_fields.insert(field);
                self.inspector.links.texts.insert(field, value.clone());
                // Emptied, it writes no name or command and stays checked.
                let value = (!value.trim().is_empty()).then_some(value);
                self.write_link_field(field, value)
            }
            LinkMessage::Submitted(field) => {
                let links = &mut self.inspector.links;
                if links.text(field).trim().is_empty() {
                    links.open_fields.remove(&field);
                    links.texts.remove(&field);
                }
                Update::none()
            }
            LinkMessage::TwoWay(two_way) => {
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                let Some(map) = self.editor.area_id() else {
                    return Update::none();
                };
                if view.two_way() == two_way {
                    return Update::none();
                }
                let atlas = self.mapper.get_current_atlas();
                if two_way && let Some(far) = link_commands::way_back_shows_more(&view) {
                    self.editor_notice =
                        Some((std::time::Instant::now(), way_back_would_show(&atlas, far)));
                    return Update::none();
                }
                let command = if two_way {
                    link_commands::two_way(&atlas, map, &view)
                } else {
                    link_commands::one_way(&atlas, map, &view)
                };
                if command.is_none() {
                    self.notice("mapper-link-not-changed");
                }
                self.push_link_command(command)
            }
            LinkMessage::Swap => {
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                let Some(map) = self.editor.area_id() else {
                    return Update::none();
                };
                let command = link_commands::swap(&self.mapper.get_current_atlas(), map, &view);
                let update = self.push_command(command);
                // The far room is the From end now.
                let from = view.to.room.room().and_then(|room| room.placed_on(map));
                self.select_link(view.connection, from);
                update
            }
            LinkMessage::SameDoors(same) => {
                self.inspector.links.doors_apart = !same;
                if !same {
                    return Update::none();
                }
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                let (Some(from), to) = (door_at(&view, End::From), door_at(&view, End::To)) else {
                    return Update::none();
                };
                if to.as_ref() == Some(&from) {
                    return Update::none();
                }
                // The far side takes the near side's door.
                let update = self.write_door(DoorSlot::Side(End::To), from);
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            LinkMessage::Door(slot, state) => {
                let Some(side) = self.door_side(slot) else {
                    return Update::none();
                };
                let side = side.with_state(state);
                if state == DoorState::None {
                    let links = &mut self.inspector.links;
                    for field in [Field::Name(slot), Field::OpensWith(slot)] {
                        links.texts.remove(&field);
                        links.open_fields.remove(&field);
                    }
                }
                self.write_door(slot, side)
            }
            LinkMessage::Hidden(slot, hidden) => {
                let Some(mut side) = self.door_side(slot) else {
                    return Update::none();
                };
                side.hidden = hidden;
                self.write_door(slot, side)
            }
            LinkMessage::ColorReset => {
                self.update_inspector(super::inspector::Message::ConnectionColorChanged(
                    smudgy_cloud::DEFAULT_CONNECTION_COLOR.to_string(),
                ))
            }
            LinkMessage::Thickness(value) => {
                self.inspector.links.thickness = Some(value);
                Update::none()
            }
            LinkMessage::ThicknessReleased => match self.inspector.links.thickness.take() {
                Some(value) => self
                    .update_inspector(super::inspector::Message::ConnectionThicknessPicked(value)),
                None => Update::none(),
            },
            LinkMessage::Remove => {
                let Some(view) = self.selected_link_view() else {
                    return Update::none();
                };
                let Some(map) = self.editor.area_id() else {
                    return Update::none();
                };
                let command = link_commands::remove(&self.mapper.get_current_atlas(), map, &view);
                if command.is_none() {
                    self.notice("mapper-link-not-changed");
                    return Update::none();
                }
                // Back to the room the link was opened from.
                let anchor = self.editor.connection_anchor();
                let back = [&view.from, &view.to]
                    .into_iter()
                    .find(|side| anchor.is_some() && side.doc_room == anchor)
                    .and_then(|side| side.room.room())
                    .and_then(|room| room.placed_on(map));
                let update = self.push_command(command);
                match back {
                    Some(room) => self.editor.select(room.into()),
                    None => self.editor.clear_selection(),
                }
                self.selection_reset();
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
        }
    }

    /// Writes a checked field's text; `None` writes none: no name, nothing
    /// that opens the door, or no command (the wire's empty command).
    fn write_link_field(&mut self, field: Field, value: Option<String>) -> Update<Message, Event> {
        match field {
            Field::Command(end) => {
                let command = value.unwrap_or_default();
                self.edit_link_exits(Sides::One(end), FieldId::Command, move |updates| {
                    updates.command = Some(command.clone());
                })
            }
            Field::Name(slot) => self.write_name(slot, value),
            Field::OpensWith(slot) => {
                let Some(mut side) = self.door_side(slot) else {
                    return Update::none();
                };
                side.opens_with = value;
                self.write_door_field(slot, side, FieldId::DoorOpensWith)
            }
        }
    }

    fn write_name(&mut self, slot: DoorSlot, name: Option<String>) -> Update<Message, Event> {
        let Some(mut side) = self.door_side(slot) else {
            return Update::none();
        };
        side.name = name;
        self.write_door_field(slot, side, FieldId::DoorName)
    }

    /// The selected room's link rows.
    fn room_rows(&self) -> Vec<LinkRow> {
        let Some(room) = self.selected_link_room() else {
            return Vec::new();
        };
        let atlas = self.mapper.get_current_atlas();
        let Some(map) = self.editor.area_id().and_then(|id| atlas.get_area(&id)) else {
            return Vec::new();
        };
        links::room_links(&atlas, &map, room)
    }

    /// The other maps the open picker lists.
    fn picker_maps(&self) -> Vec<(AreaId, String)> {
        let (Some(picker), Some(map)) = (&self.inspector.links.picker, self.editor.area_id())
        else {
            return Vec::new();
        };
        let atlas = self.mapper.get_current_atlas();
        let unreachable = links::unreachable_maps(
            &atlas,
            map,
            &self.mapper.session_area_ids(),
            &self.mapper.local_area_ids(),
        );
        links::other_maps(&atlas, map, &unreachable, &picker.query)
    }
}

/// The picker's search input.
pub(super) fn picker_input_id(window: iced::window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("link-picker-{window:?}"))
}

/// The picker: its search, its rooms in groups, and "Other map…".
fn picker_view<'a>(window: &'a MapEditorWindow, picker: &'a Picker) -> ThemedElement<'a, Message> {
    let atlas = window.mapper.get_current_atlas();
    let map_id = window.editor.area_id();
    let mut body = Column::new().spacing(6);
    body = body.push(
        row![
            text(crate::i18n::t!("link-picker-help"))
                .size(12)
                .style(muted)
                .width(Length::Fill),
            button(text(crate::i18n::t!("action-cancel")).size(11))
                .style(builtins::button::subtle)
                .padding([2, 8])
                .on_press(Message::Links(LinkMessage::PickerClosed)),
        ]
        .align_y(Vertical::Center),
    );
    let placeholder = if picker.choosing_map {
        crate::i18n::ts!("link-picker-map-placeholder")
    } else {
        crate::i18n::ts!("link-picker-placeholder")
    };
    body = body.push(
        text_input(placeholder, &picker.query)
            .id(picker_input_id(window.window_id))
            .size(13)
            .padding([6, 8])
            .on_input(|query| Message::Links(LinkMessage::Query(query)))
            .on_submit(Message::Links(LinkMessage::QuerySubmitted)),
    );

    let item = |name: &RoomName, with_place: bool| -> ThemedElement<'a, Message> {
        let line: ThemedElement<'a, Message> = if with_place {
            named_line(name, 13)
        } else {
            text(room_label(name)).size(13).into()
        };
        button(line)
            .style(builtins::button::list_item)
            .width(Length::Fill)
            .padding([4, 8])
            .on_press(Message::Links(LinkMessage::Picked(name.id)))
            .into()
    };
    let group = |title: String,
                 rooms: &[RoomName],
                 more: usize,
                 with_place: bool|
     -> Option<ThemedElement<'a, Message>> {
        if rooms.is_empty() {
            return None;
        }
        let mut list = Column::new().spacing(1).push(section_label(title));
        for name in rooms {
            list = list.push(item(name, with_place));
        }
        if more > 0 {
            list = list.push(
                text(crate::i18n::t!("link-picker-more", "count" => more))
                    .size(11)
                    .style(muted),
            );
        }
        Some(list.into())
    };

    if picker.choosing_map {
        let mut list = Column::new()
            .spacing(1)
            .push(section_label(crate::i18n::t!("link-picker-other-maps")));
        for (id, name) in window.picker_maps() {
            list = list.push(
                button(text(name).size(13))
                    .style(builtins::button::list_item)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .on_press(Message::Links(LinkMessage::MapPicked(id))),
            );
        }
        body = body.push(list);
        body = body.push(
            button(text(crate::i18n::t!("link-picker-this-map")).size(12))
                .style(builtins::button::link)
                .padding(0)
                .on_press(Message::Links(LinkMessage::ThisMap)),
        );
    } else if let Some(other) = picker.other_map.and_then(|id| atlas.get_area(&id)) {
        let found = map_id.map_or_else(links::Found::default, |here| {
            links::other_map_picks(&atlas, here, &other, true, &picker.query, PICKER_LIMIT)
        });
        if let Some(list) = group(other.get_name().to_string(), &found.rooms, found.more, true) {
            body = body.push(list);
        } else {
            body = body.push(
                text(crate::i18n::t!("link-picker-nothing"))
                    .size(12)
                    .style(muted),
            );
        }
        body = body.push(
            button(text(crate::i18n::t!("link-picker-this-map")).size(12))
                .style(builtins::button::link)
                .padding(0)
                .on_press(Message::Links(LinkMessage::ThisMap)),
        );
    } else if let Some(map) = map_id.and_then(|id| atlas.get_area(&id)) {
        // A new exit may lead nowhere yet: an unexplored direction.
        if matches!(picker.purpose, Purpose::NewExit(_)) {
            body = body.push(
                button(text(crate::i18n::t!("link-picker-no-destination")).size(13))
                    .style(builtins::button::list_item)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .on_press(Message::Links(LinkMessage::NoDestination)),
            );
        }
        let origin = window.picker_origin(picker.purpose);
        let near = origin.and_then(|origin| links::near(&atlas, origin));
        let places = links::reachable_places(&map, origin);
        let (this_map, place_rooms) = links::this_map_picks(
            &atlas,
            &map,
            near,
            origin,
            &places,
            &picker.query,
            PICKER_LIMIT,
        );
        let mut any = false;
        if let Some(list) = group(
            crate::i18n::t!("link-picker-this-map-group"),
            &this_map.rooms,
            this_map.more,
            false,
        ) {
            body = body.push(list);
            any = true;
        }
        if let Some(list) = group(
            crate::i18n::t!("link-picker-places-group"),
            &place_rooms.rooms,
            place_rooms.more,
            true,
        ) {
            body = body.push(list);
            any = true;
        }
        if !any {
            body = body.push(
                text(crate::i18n::t!("link-picker-nothing"))
                    .size(12)
                    .style(muted),
            );
        }
        if MapEditorWindow::picker_reaches_other_maps(picker.purpose) {
            body = body.push(
                button(text(crate::i18n::t!("link-picker-other-map")).size(12))
                    .style(builtins::button::link)
                    .padding(0)
                    .on_press(Message::Links(LinkMessage::OtherMaps)),
            );
        }
    }
    container(body)
        .padding(8)
        .width(Length::Fill)
        .style(builtins::container::card)
        .into()
}

/// One compass cell: a used direction (its rows highlight), an unused one
/// (starts a link), or the room itself in the middle.
fn compass_cell<'a>(
    direction: Option<ExitDirection>,
    used: &HashSet<ExitDirection>,
    active: Option<ExitDirection>,
) -> ThemedElement<'a, Message> {
    let Some(direction) = direction else {
        return container(text("\u{00B7}").size(13).style(muted))
            .width(40)
            .height(28)
            .center_x(40)
            .center_y(28)
            .into();
    };
    let label = text(compass_label(direction)).size(12).center();
    let style = if active == Some(direction) {
        builtins::button::primary
    } else if used.contains(&direction) {
        builtins::button::secondary
    } else {
        builtins::button::subtle
    };
    button(container(label).center_x(Length::Fill))
        .style(style)
        .width(40)
        .height(28)
        .padding([5, 0])
        .on_press(Message::Links(LinkMessage::Compass(direction)))
        .into()
}

/// The selected room's Exits: the compass, an open picker for a new exit,
/// one row per link, and "+ Add exit…".
#[allow(clippy::too_many_lines)]
pub(super) fn room_exits(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let atlas = window.mapper.get_current_atlas();
    let mut section =
        Column::new()
            .spacing(8)
            .push(super::inspector::field_label(crate::i18n::t!(
                "inspector-exits"
            )));
    let Some(map) = window.editor.area_id().and_then(|id| atlas.get_area(&id)) else {
        return section.into();
    };
    let Some(room) = window.selected_link_room() else {
        return section.into();
    };
    let here = *map.get_id();
    let rows = links::room_links(&atlas, &map, room);
    let used = links::used_directions(&rows);
    let state = &window.inspector.links;
    let starting = state
        .picker
        .as_ref()
        .and_then(|picker| match picker.purpose {
            Purpose::NewExit(direction) => Some(direction),
            Purpose::Change(_) => None,
        });
    let active = starting.or(state.highlight);

    let grid = |cells: &[&[Option<ExitDirection>]]| {
        let mut column = Column::new().spacing(4);
        for line in cells {
            let mut cells_row = row![].spacing(4);
            for cell in *line {
                cells_row = cells_row.push(compass_cell(*cell, &used, active));
            }
            column = column.push(cells_row);
        }
        column
    };
    use ExitDirection::{
        Down, East, In, North, Northeast, Northwest, Out, South, Southeast, Southwest, Up, West,
    };
    section = section.push(
        row![
            grid(&[
                &[Some(Northwest), Some(North), Some(Northeast)],
                &[Some(West), None, Some(East)],
                &[Some(Southwest), Some(South), Some(Southeast)],
            ]),
            grid(&[&[Some(Up), Some(Down)], &[Some(In), Some(Out)]]),
        ]
        .spacing(16),
    );

    if let (Some(direction), Some(picker)) = (starting, &state.picker) {
        let from = links::name(&atlas, here, RoomId::on(here, room));
        let mut box_ = Column::new().spacing(6).push(
            text(crate::i18n::t!(
                "link-new-exit",
                "direction" => direction_name(direction),
                "room" => room_label(&from)
            ))
            .size(13),
        );
        box_ = box_.push(
            row![
                text(crate::i18n::t!("link-leaves"))
                    .size(12)
                    .style(muted)
                    .width(90),
                pick_list(
                    direction_choices(),
                    Some(DirectionChoice(direction)),
                    |choice| { Message::Links(LinkMessage::NewDirection(choice.0)) }
                )
                .text_size(12),
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        );
        box_ = box_.push(picker_view(window, picker));
        section = section.push(box_);
    }

    for row_data in &rows {
        let far = match row_data.far {
            Destination::Room(id) => Some(links::name(&atlas, here, id)),
            Destination::Unknown | Destination::Nowhere => None,
        };
        let label = match (&far, row_data.far) {
            (Some(name), _) => room_label(name),
            (None, Destination::Unknown) => crate::i18n::t!("inspector-unknown-map"),
            (None, _) => crate::i18n::t!("link-nowhere"),
        };
        let direction = |direction: Option<ExitDirection>| direction.map_or("", compass_label);
        let ways = match row_data.way {
            Way::Both => format!(
                "{} \u{21C4} {}",
                direction(row_data.direction),
                direction(row_data.back)
            ),
            Way::Out => format!("{} \u{2192}", direction(row_data.direction)),
            Way::In => format!("{} \u{2190}", direction(row_data.direction)),
        };
        let mut lines = Column::new().spacing(2);
        if let Some(pills) = far
            .as_ref()
            .and_then(|name| badges(name, kept_in(&map, row_data.place)))
        {
            lines = lines.push(container(pills).padding(Padding::ZERO.left(70.0)));
        } else if far.is_none()
            && let Some((place, color)) = kept_in(&map, row_data.place)
        {
            lines = lines.push(container(badge(place, color)).padding(Padding::ZERO.left(70.0)));
        }
        lines = lines.push(
            row![
                text(ways).size(13).font(fonts::GEIST_MONO_VF).width(64),
                text(label).size(13).width(Length::Fill),
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        );
        if let Some(summary) = door_summary(&row_data.door) {
            lines = lines.push(
                container(text(summary).size(11).style(muted)).padding(Padding::ZERO.left(70.0)),
            );
        }
        let highlighted = state.highlight.is_some()
            && row_data.way != Way::In
            && row_data.direction == state.highlight;
        let row_button = button(lines)
            .style(if highlighted {
                builtins::button::list_item_selected
            } else {
                builtins::button::list_item
            })
            .width(Length::Fill)
            .padding([5, 6])
            .on_press(Message::Links(LinkMessage::RowOpened(row_data.connection)));
        let writable = super::secrets::can_write(&map, row_data.place);
        let unknown = row_data.far == Destination::Unknown;
        let trash = button(
            text(bootstrap_icons::TRASH_3)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(14.0),
        )
        .style(builtins::button::toolbar)
        .on_press_maybe(
            (writable && !unknown)
                .then_some(Message::Links(LinkMessage::RowRemoved(row_data.connection))),
        );
        section = section.push(row![row_button, trash].spacing(4).align_y(Vertical::Center));
    }

    if starting.is_none() {
        section = section.push(
            button(text(crate::i18n::t!("link-add-exit")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::Links(LinkMessage::AddExit)),
        );
    }
    section.into()
}

/// A row of mutually exclusive buttons, the chosen one marked.
pub(super) fn segmented<'a, T: Copy + PartialEq + 'a>(
    options: &[(T, &'static str)],
    chosen: T,
    enabled: bool,
    on_pick: impl Fn(T) -> Message + 'a,
) -> ThemedElement<'a, Message> {
    let mut buttons = row![].spacing(2);
    for (value, label) in options {
        let style = if *value == chosen {
            builtins::button::primary
        } else {
            builtins::button::secondary
        };
        buttons = buttons.push(
            button(container(text(*label).size(12)).center_x(Length::Fill))
                .style(style)
                .width(Length::Fill)
                .padding([4, 6])
                .on_press_maybe(enabled.then(|| on_pick(*value))),
        );
    }
    buttons.into()
}

pub(super) fn labeled<'a>(
    label: String,
    control: impl Into<ThemedElement<'a, Message>>,
) -> ThemedElement<'a, Message> {
    row![text(label).size(12).style(muted).width(80), control.into()]
        .spacing(8)
        .align_y(Vertical::Center)
        .into()
}

/// A field behind a checkbox: the checkbox, and once checked the input.
fn checked_field<'a>(
    window: &'a MapEditorWindow,
    field: Field,
    label: String,
    placeholder: String,
    enabled: bool,
) -> ThemedElement<'a, Message> {
    let links = &window.inspector.links;
    let checked = links.checked(field);
    let mut line = row![
        checkbox(checked)
            .label(label)
            .size(14)
            .text_size(12)
            .on_toggle_maybe(
                enabled
                    .then_some(move |checked| Message::Links(LinkMessage::Checked(field, checked)))
            ),
    ]
    .spacing(8)
    .align_y(Vertical::Center);
    if checked {
        let mut input = text_input(&placeholder, links.text(field))
            .id(field.id(window.window_id))
            .size(12)
            .padding([4, 8])
            .width(Length::Fill);
        if enabled {
            input = input
                .on_input(move |value| Message::Links(LinkMessage::Typed(field, value)))
                .on_submit(Message::Links(LinkMessage::Submitted(field)));
        }
        line = line.push(input);
    }
    line.into()
}

/// One end's block: its badges, its room with "Change ▾", an open picker,
/// and its exit's direction, weight and command (or, for a one-way link's
/// far end, the direction it arrives from).
#[allow(clippy::too_many_lines)]
fn end_block<'a>(
    window: &'a MapEditorWindow,
    map: &AreaCache,
    view: &LinkView,
    end: End,
    writable: bool,
) -> ThemedElement<'a, Message> {
    let atlas = window.mapper.get_current_atlas();
    let here = *map.get_id();
    let side = match end {
        End::From => &view.from,
        End::To => &view.to,
    };
    let name = side.room.room().map(|id| links::name(&atlas, here, id));
    let mut block = Column::new().spacing(6);
    if let Some(pills) = name.as_ref().and_then(|name| badges(name, None)) {
        block = block.push(container(pills).padding(Padding::ZERO.left(44.0)));
    }
    let role = match end {
        End::From => crate::i18n::t!("inspector-endpoint-from"),
        End::To => crate::i18n::t!("inspector-endpoint-to"),
    };
    let label = match (&name, side.room) {
        (Some(name), _) => room_label(name),
        (None, Destination::Unknown) => crate::i18n::t!("inspector-unknown-map"),
        (None, _) => crate::i18n::t!("link-nowhere"),
    };
    let open = window
        .inspector
        .links
        .picker
        .as_ref()
        .filter(|picker| picker.purpose == Purpose::Change(end));
    let changeable = writable && side.room != Destination::Unknown;
    block = block.push(
        row![
            text(role.to_uppercase()).size(11).style(muted).width(38),
            text(label).size(13).width(Length::Fill),
            button(
                text(if open.is_some() {
                    crate::i18n::t!("link-change-close")
                } else {
                    crate::i18n::t!("link-change")
                })
                .size(12)
            )
            .style(builtins::button::secondary)
            .padding([3, 8])
            .on_press_maybe(changeable.then_some(Message::Links(LinkMessage::Change(end)))),
        ]
        .spacing(6)
        .align_y(Vertical::Center),
    );
    if let Some(picker) = open {
        block = block.push(picker_view(window, picker));
    }

    let exit = exit_at(view, end);
    let elsewhere = end == End::To && view.return_elsewhere.is_some();
    // An end turns where the viewer may change its own exit: the To end of
    // a link whose way back another map keeps is that way back. The From
    // end's turn changes the way back's arrival only where it is writable.
    let writable = writable
        && view
            .return_elsewhere
            .filter(|_| elsewhere)
            .is_none_or(|back| link_commands::way_back_writable(&atlas, back));
    let mut details = Column::new().spacing(6);
    if exit.is_some() || elsewhere {
        let direction = exit.map(|exit| exit.from_direction);
        let weight = &window.inspector.links.weight[index(end)];
        let mut leaves = row![
            text(crate::i18n::t!("link-leaves"))
                .size(12)
                .style(muted)
                .width(80),
            direction_picker(direction, writable, move |direction| {
                Message::Links(LinkMessage::Leaves(end, direction))
            }),
            Space::new().width(Length::Fill),
        ]
        .spacing(6)
        .align_y(Vertical::Center);
        if exit.is_some() {
            let mut input = text_input("1", weight).size(12).padding([4, 6]).width(56);
            if writable {
                input =
                    input.on_input(move |value| Message::Links(LinkMessage::Weight(end, value)));
            }
            leaves = leaves
                .push(text(crate::i18n::t!("link-weight")).size(12).style(muted))
                .push(input);
        }
        details = details.push(leaves);
        if exit.is_some() {
            details = details.push(checked_field(
                window,
                Field::Command(end),
                crate::i18n::t!("link-command"),
                direction.map_or("", direction_word).to_string(),
                writable,
            ));
        }
        if elsewhere {
            let keeper = name
                .as_ref()
                .and_then(|name| name.map_name.clone())
                .unwrap_or_default();
            details = details.push(
                text(crate::i18n::t!("link-return-elsewhere", "map" => keeper))
                    .size(11)
                    .style(muted),
            );
        }
    } else if end == End::To && side.room != Destination::Nowhere {
        let arrives = view.from.exit.as_ref().and_then(|exit| exit.to_direction);
        details = details.push(
            row![
                text(crate::i18n::t!("link-arrives-from"))
                    .size(12)
                    .style(muted)
                    .width(80),
                direction_picker(arrives, writable, |direction| {
                    Message::Links(LinkMessage::ArrivesFrom(direction))
                }),
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        );
    }
    block = block.push(container(details).padding(Padding::ZERO.left(44.0)));
    container(block)
        .padding(10)
        .width(Length::Fill)
        .style(builtins::container::card)
        .into()
}

/// One door block: its state, "hidden exit", and for a door its name and
/// what opens it.
fn door_block<'a>(
    window: &'a MapEditorWindow,
    slot: DoorSlot,
    title: String,
    side: &DoorSide,
    writable: bool,
) -> ThemedElement<'a, Message> {
    let states = [
        (DoorState::None, crate::i18n::ts!("link-door-none")),
        (DoorState::Open, crate::i18n::ts!("link-door-open")),
        (DoorState::Closed, crate::i18n::ts!("link-door-closed")),
        (DoorState::Locked, crate::i18n::ts!("link-door-locked")),
    ];
    let mut block = Column::new()
        .spacing(6)
        .push(text(title).size(12))
        .push(segmented(&states, side.state, writable, move |state| {
            Message::Links(LinkMessage::Door(slot, state))
        }))
        .push(
            checkbox(side.hidden)
                .label(crate::i18n::t!("link-hidden-exit"))
                .size(14)
                .text_size(12)
                .on_toggle_maybe(
                    writable
                        .then_some(move |hidden| Message::Links(LinkMessage::Hidden(slot, hidden))),
                ),
        );
    if side.state != DoorState::None {
        let name = window.inspector.links.text(Field::Name(slot));
        let opens = if name.is_empty() {
            crate::i18n::t!("link-opens-with-placeholder", "name" => crate::i18n::t!("link-door-default-name"))
        } else {
            crate::i18n::t!("link-opens-with-placeholder", "name" => name.to_string())
        };
        block = block
            .push(checked_field(
                window,
                Field::Name(slot),
                crate::i18n::t!("link-named"),
                String::new(),
                writable,
            ))
            .push(checked_field(
                window,
                Field::OpensWith(slot),
                crate::i18n::t!("link-opens-with"),
                opens,
                writable,
            ));
    }
    container(block)
        .padding(10)
        .width(Length::Fill)
        .style(builtins::container::card)
        .into()
}

/// Why a link into `far`'s map stays one-way: its way back would show to
/// everyone who reads the place that would keep it (that map, or the Secret
/// `far` is in).
fn way_back_would_show(atlas: &smudgy_cloud::mapper::AtlasCache, far: RoomId) -> String {
    let keeper = atlas
        .get_area(&far.map)
        .map(|area| match far.place {
            SourceId::Map => area.get_name().to_string(),
            place => super::secrets::place_name(&area, place),
        })
        .unwrap_or_default();
    crate::i18n::t!("link-two-way-would-show", "map" => keeper)
}

/// The link's "In ● place" line: plain where its place is decided, else a
/// picker of the places it may move to.
fn place_line<'a>(
    window: &'a MapEditorWindow,
    map: &AreaCache,
    view: &LinkView,
) -> Option<ThemedElement<'a, Message>> {
    if !window.secrets_apply() {
        return None;
    }
    let mut line = row![text(crate::i18n::t!("inspector-in")).size(12).style(muted)]
        .spacing(6)
        .align_y(Vertical::Center);
    if let Some(color) = smudgy_map_widget::sources::source_color(map, view.place) {
        line = line.push(super::secrets::dot(Some(color)));
    }
    let places = links::link_places(map, view.place, &view.rooms());
    match places {
        LinkPlaces::Choice(places) if window.link_moves_ready() => {
            let options: Vec<super::secrets::Place> = places
                .iter()
                .map(|place| super::secrets::Place {
                    source: *place,
                    name: super::secrets::place_name(map, *place),
                })
                .collect();
            let current = options.first().cloned();
            line = line.push(
                pick_list(options, current, |place: super::secrets::Place| {
                    Message::Links(LinkMessage::MoveTo(place.source))
                })
                .text_size(12),
            );
        }
        _ => {
            line = line.push(text(super::secrets::place_name(map, view.place)).size(12));
        }
    }
    Some(line.into())
}

impl MapEditorWindow {
    /// Whether the panel can move a link between places: not while another
    /// move runs.
    fn link_moves_ready(&self) -> bool {
        !self.moving
    }
}

/// A link's line in a room's Exits: its directions, then its far room.
fn row_line<'a>(
    atlas: &smudgy_cloud::mapper::AtlasCache,
    here: AreaId,
    row: &LinkRow,
) -> ThemedElement<'a, Message> {
    let direction = |direction: Option<ExitDirection>| direction.map_or("", compass_label);
    let ways = match row.way {
        Way::Both => format!(
            "{} \u{21C4} {}",
            direction(row.direction),
            direction(row.back)
        ),
        Way::Out => format!("{} \u{2192}", direction(row.direction)),
        Way::In => format!("{} \u{2190}", direction(row.direction)),
    };
    let far = match row.far {
        Destination::Room(id) => named_line(&links::name(atlas, here, id), 12),
        Destination::Unknown => text(crate::i18n::t!("inspector-unknown-map"))
            .size(12)
            .into(),
        Destination::Nowhere => text(crate::i18n::t!("link-nowhere")).size(12).into(),
    };
    row![
        text(ways).size(12).font(fonts::GEIST_MONO_VF).width(64),
        far,
    ]
    .spacing(6)
    .align_y(Vertical::Center)
    .into()
}

/// A room's links where the viewer can't change them: one line each,
/// naming its far room by title and place.
pub(super) fn read_only_links(
    window: &MapEditorWindow,
    room: PlacedRoom,
) -> Option<ThemedElement<'_, Message>> {
    let atlas = window.mapper.get_current_atlas();
    let map = atlas.get_area(&window.editor.area_id()?)?;
    let rows = links::room_links(&atlas, &map, room);
    if rows.is_empty() {
        return None;
    }
    let mut list = Column::new()
        .spacing(4)
        .push(super::inspector::field_label(crate::i18n::t!(
            "inspector-exits"
        )));
    for row_data in &rows {
        list = list.push(row_line(&atlas, *map.get_id(), row_data));
    }
    Some(list.into())
}

/// The selected link where the viewer can't change it: where it lives, its
/// two ends by title and place, and whether it runs both ways.
pub(super) fn read_only_link(window: &MapEditorWindow) -> Column<'_, Message, crate::Theme> {
    let atlas = window.mapper.get_current_atlas();
    let mut content = Column::new().spacing(6);
    let (Some(map), Some(view)) = (
        window.editor.area_id().and_then(|id| atlas.get_area(&id)),
        window.selected_link_view(),
    ) else {
        return content;
    };
    let here = *map.get_id();
    content = content.push(text(crate::i18n::t!("link-heading")).size(16));
    if window.secrets_apply() {
        let mut line = row![text(crate::i18n::t!("inspector-in")).size(12).style(muted)]
            .spacing(6)
            .align_y(Vertical::Center);
        if let Some(color) = smudgy_map_widget::sources::source_color(&map, view.place) {
            line = line.push(super::secrets::dot(Some(color)));
        }
        content =
            content.push(line.push(text(super::secrets::place_name(&map, view.place)).size(12)));
    }
    for (role, side) in [
        (crate::i18n::t!("inspector-endpoint-from"), &view.from),
        (crate::i18n::t!("inspector-endpoint-to"), &view.to),
    ] {
        let name = match side.room {
            Destination::Room(id) => named_line(&links::name(&atlas, here, id), 12),
            Destination::Unknown => text(crate::i18n::t!("inspector-unknown-map"))
                .size(12)
                .into(),
            Destination::Nowhere => text(crate::i18n::t!("link-nowhere")).size(12).into(),
        };
        content = content.push(
            row![
                text(role.to_uppercase()).size(11).style(muted).width(38),
                name,
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        );
    }
    content.push(
        text(if view.two_way() {
            crate::i18n::t!("link-two-way")
        } else {
            crate::i18n::t!("link-one-way")
        })
        .size(12),
    )
}

/// The selected link's editor.
#[allow(clippy::too_many_lines)]
pub(super) fn link_editor(window: &MapEditorWindow) -> Column<'_, Message, crate::Theme> {
    let atlas = window.mapper.get_current_atlas();
    let mut content = Column::new().spacing(10).padding(12);
    let Some(map) = window.editor.area_id().and_then(|id| atlas.get_area(&id)) else {
        return content.push(text(crate::i18n::t!("inspector-no-area-selected")));
    };
    let Some(view) = window.selected_link_view() else {
        return content.push(text(crate::i18n::t!("inspector-connection-missing")));
    };
    let writable = super::secrets::can_write(&map, view.place);
    content = content.push(text(crate::i18n::t!("link-heading")).size(16));
    if let Some(line) = place_line(window, &map, &view) {
        content = content.push(line);
    }

    content = content.push(end_block(window, &map, &view, End::From, writable));
    let two_way = view.two_way();
    let somewhere = view.to.room.room().is_some();
    let swappable = !two_way && view.to.doc_room.is_some() && view.from.exit.is_some();
    // A way back another map keeps goes only where the viewer may remove it.
    let way_back_removable = view.to.exit.is_some()
        || view.return_elsewhere.is_none_or(|back| {
            atlas
                .get_area(&back.map)
                .is_some_and(|area| super::secrets::can_remove(&area, back.place))
        });
    let kept_one_way = (!two_way)
        .then(|| link_commands::way_back_shows_more(&view))
        .flatten();
    let way_button = |label: String, on: bool, message: Option<LinkMessage>| {
        button(text(label).size(12))
            .style(if on {
                builtins::button::primary
            } else {
                builtins::button::secondary
            })
            .padding([4, 10])
            .on_press_maybe(message.map(Message::Links))
    };
    content = content.push(
        row![
            way_button(
                crate::i18n::t!("link-two-way"),
                two_way,
                (writable && !two_way && somewhere && kept_one_way.is_none())
                    .then_some(LinkMessage::TwoWay(true)),
            ),
            way_button(
                crate::i18n::t!("link-one-way"),
                !two_way,
                (writable && two_way && way_back_removable).then_some(LinkMessage::TwoWay(false)),
            ),
            Space::new().width(Length::Fill),
            button(text(crate::i18n::t!("link-swap")).size(12))
                .style(builtins::button::secondary)
                .padding([4, 10])
                .on_press_maybe(
                    (writable && swappable).then_some(Message::Links(LinkMessage::Swap))
                ),
        ]
        .spacing(6)
        .align_y(Vertical::Center),
    );
    if let Some(far) = kept_one_way {
        content = content.push(text(way_back_would_show(&atlas, far)).size(11).style(muted));
    }
    content = content.push(end_block(window, &map, &view, End::To, writable));

    // Doors.
    let state = &window.inspector.links;
    if let Some(from) = door_at(&view, End::From) {
        let to = door_at(&view, End::To);
        let one = links::doors_shown_as_one(&from, to.as_ref(), state.doors_apart);
        let mut doors = row![
            super::inspector::field_label(crate::i18n::t!("link-doors")),
            Space::new().width(Length::Fill)
        ]
        .align_y(Vertical::Center);
        if to.is_some() {
            doors = doors.push(
                checkbox(one)
                    .label(crate::i18n::t!("link-same-doors"))
                    .size(14)
                    .text_size(12)
                    .on_toggle_maybe(
                        writable.then_some(|same| Message::Links(LinkMessage::SameDoors(same))),
                    ),
            );
        }
        content = content.push(doors);
        let atlas_name = |end: End| {
            let side = match end {
                End::From => &view.from,
                End::To => &view.to,
            };
            side.room
                .room()
                .map(|id| links::name(&atlas, *map.get_id(), id))
                .map(|name| {
                    if name.title.is_empty() {
                        room_label(&name)
                    } else {
                        name.title
                    }
                })
                .unwrap_or_default()
        };
        match to {
            Some(to) if !one => {
                content = content.push(door_block(
                    window,
                    DoorSlot::Side(End::From),
                    crate::i18n::t!("link-door-side", "room" => atlas_name(End::From)),
                    &from,
                    writable,
                ));
                content = content.push(door_block(
                    window,
                    DoorSlot::Side(End::To),
                    crate::i18n::t!("link-door-side", "room" => atlas_name(End::To)),
                    &to,
                    writable,
                ));
            }
            Some(_) => {
                content = content.push(door_block(
                    window,
                    DoorSlot::Both,
                    crate::i18n::t!("link-door-both"),
                    &from,
                    writable,
                ));
            }
            None => {
                content = content.push(door_block(
                    window,
                    DoorSlot::Side(End::From),
                    crate::i18n::t!("link-door-one"),
                    &from,
                    writable,
                ));
            }
        }
    }

    // Appearance.
    content = content.push(super::inspector::field_label(crate::i18n::t!(
        "inspector-appearance"
    )));
    content = content.push(super::inspector::link_appearance(
        window,
        &map,
        view.connection,
        writable,
    ));
    content = content.push(
        button(text(crate::i18n::t!("link-remove")).size(12))
            .style(builtins::button::danger)
            .padding([5, 12])
            .on_press_maybe(writable.then_some(Message::Links(LinkMessage::Remove))),
    );
    content
}

#[cfg(test)]
mod tests {
    use super::super::links::fixture::*;

    /// Rooms are named the same way in every language: by number and title,
    /// a place's room by its place, another map's after the map.
    #[test]
    fn room_names_read_in_every_language() {
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            let titled = smudgy_i18n::t!(
                translator,
                "mapper-place-room-name-titled",
                "place" => "Bookcase",
                "number" => "1",
                "title" => "Hidden Library"
            );
            assert!(
                titled.contains("Bookcase")
                    && titled.contains("#1")
                    && titled.contains("Hidden Library")
                    && !titled.contains('⟦'),
                "{}: {titled}",
                catalog.tag
            );
            let other =
                smudgy_i18n::t!(translator, "mapper-other-map-prefix", "map" => "Catacombs");
            assert!(
                other.contains("Catacombs") && !other.contains('⟦'),
                "{other}"
            );
            let plain = smudgy_i18n::t!(translator, "mapper-room-name", "number" => "4");
            let titled = smudgy_i18n::t!(
                translator,
                "mapper-room-name-titled",
                "number" => "4",
                "title" => "Ossuary"
            );
            let placed = smudgy_i18n::t!(
                translator,
                "mapper-place-room-name",
                "place" => "Bookcase",
                "number" => "4"
            );
            for text in [plain, titled, placed] {
                assert!(
                    text.contains("#4") && !text.contains('⟦'),
                    "{}: {text}",
                    catalog.tag
                );
            }
        }
    }
    use super::*;
    use smudgy_cloud::RoomNumber;
    use smudgy_map_widget::map_editor;

    async fn window() -> MapEditorWindow {
        super::super::test_window(maps().await, area(KEEP))
    }

    fn select(window: &mut MapEditorWindow, entity: EntityId) {
        window.editor.select(entity);
        window.inspector.resync(&window.mapper, &window.editor);
    }

    fn send(window: &mut MapEditorWindow, message: LinkMessage) {
        let _ = window.update(Message::Links(message));
    }

    fn keep_rows(window: &MapEditorWindow, number: i32) -> Vec<LinkRow> {
        let atlas = window.mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");
        links::room_links(&atlas, &keep, PlacedRoom::map(RoomNumber(number)))
    }

    fn keep_room(number: i32) -> RoomId {
        RoomId {
            map: area(KEEP),
            place: SourceId::Map,
            number: RoomNumber(number),
        }
    }

    /// An unused compass direction starts a link: the picker opens, the
    /// canvas picks rooms, and the room picked ends a two-way link, which
    /// is then selected from the room it started at.
    #[tokio::test]
    async fn an_empty_direction_starts_a_link_picked_on_the_canvas() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(3)));
        send(&mut window, LinkMessage::Compass(ExitDirection::North));
        assert_eq!(
            window
                .inspector
                .links
                .picker
                .as_ref()
                .map(|picker| picker.purpose),
            Some(Purpose::NewExit(ExitDirection::North))
        );
        assert!(window.editor.picking(), "the canvas picks rooms meanwhile");
        let names: Vec<RoomId> = window.picker_rooms().iter().map(|room| room.id).collect();
        assert_eq!(names.first(), Some(&keep_room(1)), "nearest first");
        assert!(!names.contains(&keep_room(3)), "never the room itself");

        let _ = window.update(Message::Editor(map_editor::Message::RoomPicked(
            PlacedRoom::map(RoomNumber(5)),
        )));
        assert!(window.inspector.links.picker.is_none());
        assert!(!window.editor.picking());
        let row = keep_rows(&window, 3)
            .into_iter()
            .find(|row| row.far == Destination::Room(keep_room(5)))
            .expect("the new link");
        assert_eq!(
            (row.way, row.direction, row.back),
            (
                Way::Both,
                Some(ExitDirection::North),
                Some(ExitDirection::South)
            )
        );
        assert_eq!(
            window.editor.selection().single(),
            Some(EntityId::Connection(row.connection))
        );
        let view = window.selected_link_view().expect("the link");
        assert_eq!(
            view.from.room,
            Destination::Room(keep_room(3)),
            "from the room it started at"
        );
    }

    /// A used direction highlights its rows; Enter in the picker takes its
    /// first room; Escape closes it.
    #[tokio::test]
    async fn used_directions_highlight_and_the_picker_takes_enter_and_escape() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(1)));
        send(&mut window, LinkMessage::Compass(ExitDirection::North));
        assert_eq!(window.inspector.links.highlight, Some(ExitDirection::North));
        assert!(window.inspector.links.picker.is_none());
        send(&mut window, LinkMessage::Compass(ExitDirection::North));
        assert_eq!(window.inspector.links.highlight, None);

        send(&mut window, LinkMessage::AddExit);
        assert_eq!(
            window
                .inspector
                .links
                .picker
                .as_ref()
                .map(|picker| picker.purpose),
            Some(Purpose::NewExit(ExitDirection::Northeast)),
            "the first direction no exit leaves in"
        );
        let _ = window.update(Message::Hotkey(
            window.window_id,
            super::super::Hotkey::Escape,
        ));
        assert!(window.inspector.links.picker.is_none());
        assert_eq!(
            window.editor.selection().single(),
            Some(EntityId::Room(RoomNumber(1)))
        );

        send(&mut window, LinkMessage::AddExit);
        send(
            &mut window,
            LinkMessage::NewDirection(ExitDirection::Southwest),
        );
        send(&mut window, LinkMessage::Query("gard".to_string()));
        send(&mut window, LinkMessage::QuerySubmitted);
        assert!(
            keep_rows(&window, 3).iter().any(|row| row.way == Way::Both
                && row.far == Destination::Room(keep_room(1))
                && row.back == Some(ExitDirection::Southwest)),
            "a second link to the Garden, leaving southwest"
        );
    }

    /// "No destination yet" makes an exit leading nowhere: its row reads
    /// "→ (no destination)" and the room stays selected. "Change ▾" on its
    /// To end gives it a destination later, making it two-way like a new
    /// link; undo takes it back to leading nowhere, and the trash removes it.
    #[tokio::test]
    async fn an_exit_may_lead_nowhere_until_given_a_destination() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(3)));
        send(&mut window, LinkMessage::Compass(ExitDirection::North));
        send(&mut window, LinkMessage::NoDestination);
        assert!(window.inspector.links.picker.is_none());
        assert!(!window.editor.picking());
        assert_eq!(
            window.editor.selection().single(),
            Some(EntityId::Room(RoomNumber(3))),
            "the room stays selected"
        );
        let dangling = keep_rows(&window, 3)
            .into_iter()
            .find(|row| row.far == Destination::Nowhere)
            .expect("the new exit");
        assert_eq!(
            (dangling.way, dangling.direction, dangling.back),
            (Way::Out, Some(ExitDirection::North), None)
        );
        assert_eq!(window.inspector.links.highlight, Some(ExitDirection::North));
        let _ = room_exits(&window);

        send(&mut window, LinkMessage::RowOpened(dangling.connection));
        let view = window.selected_link_view().expect("the exit");
        assert_eq!(view.to.room, Destination::Nowhere);
        assert!(
            view.from
                .exit
                .as_ref()
                .is_some_and(|exit| exit.to_direction.is_none())
        );
        let _ = link_editor(&window);
        send(&mut window, LinkMessage::Change(End::To));
        send(&mut window, LinkMessage::Picked(keep_room(5)));
        let view = window.selected_link_view().expect("the link");
        assert_eq!(view.connection, dangling.connection, "the same link");
        assert_eq!(view.to.room, Destination::Room(keep_room(5)));
        assert!(view.two_way(), "two-way, as a new link in the map");
        assert_eq!(
            view.to.exit.as_ref().map(|exit| exit.from_direction),
            Some(ExitDirection::South)
        );
        assert_eq!(
            view.from.exit.as_ref().and_then(|exit| exit.to_direction),
            Some(ExitDirection::South)
        );

        let _ = window.update(Message::Undo);
        assert!(
            keep_rows(&window, 3)
                .iter()
                .any(|row| row.connection == dangling.connection
                    && row.far == Destination::Nowhere),
            "undo leads it nowhere again"
        );
        select(&mut window, EntityId::Room(RoomNumber(3)));
        send(&mut window, LinkMessage::RowRemoved(dangling.connection));
        assert!(
            keep_rows(&window, 3)
                .iter()
                .all(|row| row.connection != dangling.connection)
        );
    }

    /// A row's trash removes its link whole, as one undoable step.
    #[tokio::test]
    async fn a_rows_trash_removes_its_link_as_one_step() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(1)));
        send(&mut window, LinkMessage::RowRemoved(link(C_GATE)));
        assert!(keep_rows(&window, 2).is_empty());
        let _ = window.update(Message::Undo);
        assert_eq!(keep_rows(&window, 2).len(), 1);
    }

    /// A row opens its link from the room; "Change ▾" moves an end to the
    /// room picked; one-way and swap work from the panel.
    #[tokio::test]
    async fn the_link_editor_changes_ends_ways_and_swaps() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(2)));
        send(&mut window, LinkMessage::RowOpened(link(C_GATE)));
        let view = window.selected_link_view().expect("the gate");
        assert_eq!(
            view.from.room,
            Destination::Room(keep_room(2)),
            "from the room it was opened from"
        );

        send(&mut window, LinkMessage::Change(End::To));
        assert!(window.editor.picking());
        send(&mut window, LinkMessage::Picked(keep_room(4)));
        let view = window.selected_link_view().expect("the gate");
        assert_eq!(view.connection, link(C_GATE));
        assert_eq!(view.to.room, Destination::Room(keep_room(4)));
        assert_eq!(view.from.room, Destination::Room(keep_room(2)));

        send(&mut window, LinkMessage::TwoWay(false));
        let view = window.selected_link_view().expect("the gate");
        assert!(!view.two_way());
        send(&mut window, LinkMessage::Swap);
        let view = window.selected_link_view().expect("the gate");
        assert_eq!(view.from.room, Destination::Room(keep_room(4)), "swapped");
        send(&mut window, LinkMessage::TwoWay(true));
        assert!(window.selected_link_view().expect("the gate").two_way());

        send(&mut window, LinkMessage::Remove);
        assert!(
            window
                .mapper
                .get_current_atlas()
                .get_area(&area(KEEP))
                .is_some_and(|keep| keep.get_connection(link(C_GATE)).is_none())
        );
    }

    /// "☐ command": checking opens an empty input; Enter in it while empty
    /// unchecks it; typing writes the exit's command; unchecking clears it.
    #[tokio::test]
    async fn a_checked_field_opens_writes_and_closes() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(1)));
        send(&mut window, LinkMessage::RowOpened(link(C_GARDEN)));
        let field = Field::Command(End::From);
        send(&mut window, LinkMessage::Checked(field, true));
        assert!(window.inspector.links.checked(field));
        send(&mut window, LinkMessage::Submitted(field));
        assert!(
            !window.inspector.links.checked(field),
            "Enter in it empty unchecks it"
        );

        send(&mut window, LinkMessage::Checked(field, true));
        send(
            &mut window,
            LinkMessage::Typed(field, "go east".to_string()),
        );
        let command = |window: &MapEditorWindow| {
            window
                .selected_link_view()
                .and_then(|view| view.from.exit.and_then(|exit| exit.command))
        };
        assert_eq!(command(&window).as_deref(), Some("go east"));
        send(&mut window, LinkMessage::Checked(field, false));
        assert_eq!(command(&window), None);

        send(
            &mut window,
            LinkMessage::Weight(End::From, "3.5".to_string()),
        );
        assert!(
            window
                .selected_link_view()
                .and_then(|view| view.from.exit)
                .is_some_and(|exit| (exit.weight - 3.5).abs() < f32::EPSILON)
        );
    }

    /// An emptied "named" or "opens with" stays checked through other edits
    /// and the map's own refreshes, writing no name and nothing that opens
    /// the door (never an empty string), until Enter in it unchecks it.
    /// Enter in a field holding text keeps it.
    #[tokio::test]
    async fn an_emptied_field_stays_checked_until_enter() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(1)));
        send(&mut window, LinkMessage::RowOpened(link(C_GATE)));
        send(
            &mut window,
            LinkMessage::Door(DoorSlot::Both, DoorState::Closed),
        );
        let name = Field::Name(DoorSlot::Both);
        let opens = Field::OpensWith(DoorSlot::Both);
        let door = |window: &MapEditorWindow| {
            window
                .selected_link_view()
                .and_then(|view| view.from.exit)
                .and_then(|exit| exit.door)
                .expect("a door")
        };
        send(&mut window, LinkMessage::Checked(name, true));
        assert_eq!(door(&window).name.as_deref(), Some("door"));
        send(&mut window, LinkMessage::Typed(name, String::new()));
        send(&mut window, LinkMessage::Typed(opens, "pull".to_string()));
        send(&mut window, LinkMessage::Typed(opens, "  ".to_string()));
        assert_eq!(door(&window).name, None, "no name, not an empty one");
        assert_eq!(door(&window).opens_with, None, "nothing, not blanks");

        // Other edits and a refresh from the map leave both checked.
        send(&mut window, LinkMessage::Hidden(DoorSlot::Both, true));
        window.inspector.resync(&window.mapper, &window.editor);
        let links = &window.inspector.links;
        assert!(links.checked(name) && links.checked(opens));

        send(&mut window, LinkMessage::Submitted(name));
        assert!(!window.inspector.links.checked(name), "Enter unchecks it");
        assert!(window.inspector.links.checked(opens), "the other stays");
        send(&mut window, LinkMessage::Typed(opens, "pull".to_string()));
        send(&mut window, LinkMessage::Submitted(opens));
        assert!(window.inspector.links.checked(opens), "it holds text");
        assert_eq!(door(&window).opens_with.as_deref(), Some("pull"));
    }

    /// Every state of the panels lays out: rooms of each place, links of
    /// each kind, the picker on this map and another, doors apart.
    #[tokio::test]
    async fn the_panels_build_in_every_state() {
        let mut window = window().await;
        for entity in [
            EntityId::Room(RoomNumber(1)),
            EntityId::SourceRoom(secret(), RoomNumber(1)),
            EntityId::Connection(link(C_GATE)),
            EntityId::Connection(link(C_OSSUARY)),
            EntityId::Connection(link(C_LIBRARY)),
            EntityId::Connection(link(C_FERRY)),
        ] {
            select(&mut window, entity);
            let _ = super::super::inspector::view(&window);
        }
        select(&mut window, EntityId::Room(RoomNumber(1)));
        send(&mut window, LinkMessage::AddExit);
        let _ = super::super::inspector::view(&window);
        send(&mut window, LinkMessage::OtherMaps);
        let _ = super::super::inspector::view(&window);
        send(&mut window, LinkMessage::MapPicked(area(CATACOMBS)));
        let _ = super::super::inspector::view(&window);
        assert_eq!(
            window.picker_rooms().len(),
            2,
            "the Catacombs' room and its Crypt's"
        );
        select(&mut window, EntityId::Connection(link(C_GATE)));
        send(&mut window, LinkMessage::Change(End::To));
        send(&mut window, LinkMessage::SameDoors(false));
        let _ = super::super::inspector::view(&window);
    }

    /// Doors: one block writes both sides; apart, each side its own;
    /// "same on both sides" again gives the far side the near side's door.
    #[tokio::test]
    async fn doors_are_one_block_until_set_apart() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(1)));
        send(&mut window, LinkMessage::RowOpened(link(C_GATE)));
        let sides = |window: &MapEditorWindow| {
            let view = window.selected_link_view().expect("the gate");
            (
                door_at(&view, End::From).expect("near"),
                door_at(&view, End::To).expect("far"),
            )
        };
        send(
            &mut window,
            LinkMessage::Door(DoorSlot::Both, DoorState::Locked),
        );
        let (near, far) = sides(&window);
        assert_eq!(
            (near.state, far.state),
            (DoorState::Locked, DoorState::Locked)
        );

        send(&mut window, LinkMessage::SameDoors(false));
        send(
            &mut window,
            LinkMessage::Door(DoorSlot::Side(End::To), DoorState::Closed),
        );
        let (near, far) = sides(&window);
        assert_eq!(
            (near.state, far.state),
            (DoorState::Locked, DoorState::Closed)
        );
        assert!(
            !links::doors_shown_as_one(&near, Some(&far), false),
            "they differ"
        );

        send(
            &mut window,
            LinkMessage::Hidden(DoorSlot::Side(End::From), true),
        );
        send(&mut window, LinkMessage::SameDoors(true));
        let (near, far) = sides(&window);
        assert_eq!(near, far, "the far side takes the near side's door");
        assert!(far.hidden);

        // Named: checking prefills "door"; what opens it is typed.
        send(
            &mut window,
            LinkMessage::Checked(Field::Name(DoorSlot::Both), true),
        );
        send(
            &mut window,
            LinkMessage::Typed(Field::OpensWith(DoorSlot::Both), "pull lever".to_string()),
        );
        let (near, far) = sides(&window);
        assert_eq!(near.name.as_deref(), Some("door"));
        assert_eq!(far.opens_with.as_deref(), Some("pull lever"));
        assert_eq!(
            window
                .selected_link_view()
                .and_then(|view| view.from.exit)
                .and_then(|exit| exit.door),
            Some(smudgy_cloud::Door {
                state: smudgy_cloud::DoorState::Locked,
                name: Some("door".to_string()),
                opens_with: Some("pull lever".to_string()),
            })
        );
        send(
            &mut window,
            LinkMessage::Door(DoorSlot::Both, DoorState::Open),
        );
        assert_eq!(
            sides(&window).1.state,
            DoorState::Open,
            "an open door is kept"
        );
        // None clears the name and what opens it.
        send(
            &mut window,
            LinkMessage::Door(DoorSlot::Both, DoorState::None),
        );
        let view = window.selected_link_view().expect("the gate");
        assert!(view.from.exit.and_then(|exit| exit.door).is_none());
        assert!(view.to.exit.and_then(|exit| exit.door).is_none());
        assert!(!window.inspector.links.checked(Field::Name(DoorSlot::Both)));
    }

    /// A link moves to another place on its own, staying selected; into the
    /// map, it asks first.
    #[tokio::test]
    async fn a_link_moves_between_places() {
        let mut window = window().await;
        select(&mut window, EntityId::Room(RoomNumber(1)));
        send(&mut window, LinkMessage::RowOpened(link(C_GARDEN)));
        send(&mut window, LinkMessage::MoveTo(secret()));
        assert!(window.moving, "into a Secret it moves at once");
        let (record, _) = window.running_move.as_ref().expect("the move");
        assert_eq!(record.content.connections, vec![link(C_GARDEN)]);
        assert!(record.content.rooms.is_empty());
        assert_eq!(
            window.editor.selection().single(),
            Some(EntityId::Connection(link(C_GARDEN)))
        );

        let mut window = super::super::test_window(maps().await, area(KEEP));
        let (command, id) = link_commands::create(
            area(KEEP),
            keep_room(3),
            ExitDirection::North,
            keep_room(5),
            secret(),
        )
        .expect("a link kept in the Secret");
        let _ = window.push_command(Some(command));
        select(&mut window, EntityId::Connection(id));
        send(&mut window, LinkMessage::MoveTo(SourceId::Map));
        assert!(window.moving);
        assert!(
            matches!(
                &window.modal,
                Some(super::super::modals::Modal::ReviewMove { to, reviewed: None, .. })
                    if to.is_map()
            ),
            "into the map, everyone who reads it will see it: it asks"
        );
    }
}
