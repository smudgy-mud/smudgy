//! An editable map canvas: pan/zoom like [`crate::MapView`], plus level
//! ghosting, entity selection, and (via [`Event::RequestMutation`]) edit
//! gestures. The widget owns *view* state only — every mutation is
//! delegated to the host window, which owns the undo stack and the
//! [`Mapper`] write path.

mod program;

use std::cell::Cell;
use std::collections::HashSet;

use iced::{Length, Point, Rectangle, Size, Vector, widget::Canvas};
use smudgy_cloud::{AreaId, ExitDirection, LabelId, Mapper, RoomNumber, ShapeId, mapper::RoomKey};

use crate::{Update, render, viewport::Viewport};

pub type Renderer = iced::Renderer;
pub type Theme = smudgy_theme::Theme;
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, Renderer>;

/// Anything selectable on the editor canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityId {
    Room(RoomNumber),
    Label(LabelId),
    Shape(ShapeId),
}

/// The active editing tool. Creation tools are momentary: the host window
/// reverts to [`Tool::Select`] after a placement unless Shift is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    AddRoom,
    AddLabel,
    AddShape,
}

/// The current selection, scoped to the editor's active area and level.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    items: HashSet<EntityId>,
}

impl Selection {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn contains(&self, entity: EntityId) -> bool {
        self.items.contains(&entity)
    }

    pub fn iter(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.items.iter().copied()
    }

    pub fn rooms(&self) -> impl Iterator<Item = RoomNumber> + '_ {
        self.items.iter().filter_map(|entity| match entity {
            EntityId::Room(number) => Some(*number),
            _ => None,
        })
    }

    pub fn labels(&self) -> impl Iterator<Item = LabelId> + '_ {
        self.items.iter().filter_map(|entity| match entity {
            EntityId::Label(id) => Some(*id),
            _ => None,
        })
    }

    pub fn shapes(&self) -> impl Iterator<Item = ShapeId> + '_ {
        self.items.iter().filter_map(|entity| match entity {
            EntityId::Shape(id) => Some(*id),
            _ => None,
        })
    }

    /// The selected entity when exactly one is selected.
    #[must_use]
    pub fn single(&self) -> Option<EntityId> {
        if self.items.len() == 1 {
            self.items.iter().next().copied()
        } else {
            None
        }
    }

    fn clear(&mut self) {
        self.items.clear();
    }

    fn replace_with(&mut self, entity: EntityId) {
        self.items.clear();
        self.items.insert(entity);
    }

    fn toggle(&mut self, entity: EntityId) {
        if !self.items.remove(&entity) {
            self.items.insert(entity);
        }
    }

    fn extend(&mut self, entities: impl IntoIterator<Item = EntityId>) {
        self.items.extend(entities);
    }
}

impl FromIterator<EntityId> for Selection {
    fn from_iter<T: IntoIterator<Item = EntityId>>(iter: T) -> Self {
        Self {
            items: iter.into_iter().collect(),
        }
    }
}

/// An edit gesture completed on the canvas. The host window translates
/// these into undoable commands; the widget never writes to the mapper.
#[derive(Debug, Clone)]
pub enum MutationRequest {
    /// Move the current selection by a map-space offset (already snapped
    /// unless the user held Alt).
    MoveSelection { offset: Vector },
    /// Create a room at a map-space point (already snapped unless the user
    /// held Alt) on the current level.
    PlaceRoom { at: Point },
    /// Create an exit (two-way unless `one_way`) from a room to either an
    /// existing room or a new room at a map-space point.
    CreateExit {
        from: RoomNumber,
        from_direction: ExitDirection,
        to: ExitTarget,
        to_direction: ExitDirection,
        one_way: bool,
    },
    /// Create a label covering a dragged-out map-space rect on the current
    /// level.
    CreateLabel { rect: Rectangle },
    /// Create a shape covering a dragged-out map-space rect on the current
    /// level.
    CreateShape { rect: Rectangle },
    /// Set a label's or shape's bounds (from a resize-handle drag).
    ResizeEntity { entity: EntityId, rect: Rectangle },
}

/// Where an exit drag was dropped.
#[derive(Debug, Clone, Copy)]
pub enum ExitTarget {
    Room(RoomNumber),
    /// Empty canvas; a connected room is created here (snapped already,
    /// unless the user held Alt).
    Empty(Point),
}

#[derive(Debug, Clone)]
pub enum Message {
    Translated(Vector),
    Scaled(f32, Option<Vector>),
    ClickSelect {
        entity: EntityId,
        additive: bool,
    },
    /// Rubber-band finished: select everything intersecting `rect`
    /// (map space).
    RubberBandSelect {
        rect: Rectangle,
        additive: bool,
    },
    SetHoveredRoom(Option<RoomKey>),
    MoveCommitted {
        offset: Vector,
    },
    /// A creation-tool click. `keep_tool` (Shift held) suppresses the
    /// momentary-tool revert to Select.
    PlaceRoom {
        at: Point,
        keep_tool: bool,
    },
    ExitDragCommitted {
        from: RoomNumber,
        from_direction: ExitDirection,
        to: ExitTarget,
        to_direction: ExitDirection,
        one_way: bool,
    },
    /// A label/shape tool drag finished. `keep_tool` (Shift held)
    /// suppresses the momentary-tool revert to Select.
    RectDrawn {
        kind: RectKind,
        rect: Rectangle,
        keep_tool: bool,
    },
    ResizeCommitted {
        entity: EntityId,
        rect: Rectangle,
    },
}

/// Which entity a drag-rect creation produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RectKind {
    Label,
    Shape,
}

#[derive(Debug, Clone)]
pub enum Event {
    SelectionChanged,
    HoveredRoomChanged(Option<RoomKey>),
    RequestMutation(MutationRequest),
}

pub struct MapEditor {
    mapper: Mapper,
    area_id: Option<AreaId>,
    level: i32,
    tool: Tool,
    selection: Selection,
    scaling: f32,
    translation: Vector,
    last_viewport_size: Cell<Option<Size>>,
    player_location: Option<RoomKey>,
    hovered_room: Option<RoomKey>,
}

impl MapEditor {
    const MIN_SCALING: f32 = 2.0;
    const MAX_SCALING: f32 = 200.0;
    const SPATIAL_QUERY_PADDING: f32 = 1.0;
    /// Opacity of the ghosted adjacent levels.
    const GHOST_OPACITY: f32 = 0.15;

    #[must_use]
    pub fn new(mapper: Mapper, area_id: Option<AreaId>) -> Self {
        let mut editor = Self {
            mapper,
            area_id: None,
            level: 0,
            tool: Tool::Select,
            selection: Selection::default(),
            scaling: 40.0,
            translation: Vector::new(0.0, 0.0),
            last_viewport_size: Cell::new(None),
            player_location: None,
            hovered_room: None,
        };
        editor.set_area(area_id);
        editor
    }

    /// Switches the displayed area, clearing selection and view state.
    pub fn set_area(&mut self, area_id: Option<AreaId>) {
        self.area_id = area_id;
        self.selection.clear();
        self.hovered_room = None;
        self.level = 0;
        self.translation = self.center_of_area().map_or_else(
            || Vector::new(0.0, 0.0),
            |center| Vector::new(-center.x, -center.y),
        );
    }

    /// The bounding-box center of the area's rooms, if it has any.
    fn center_of_area(&self) -> Option<Point> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        let rooms = area.get_rooms();

        let mut iter = rooms.iter();
        let first = iter.next()?;
        let (mut min_x, mut max_x) = (first.get_x(), first.get_x());
        let (mut min_y, mut max_y) = (first.get_y(), first.get_y());

        for room in iter {
            min_x = min_x.min(room.get_x());
            max_x = max_x.max(room.get_x());
            min_y = min_y.min(room.get_y());
            max_y = max_y.max(room.get_y());
        }

        Some(Point::new((min_x + max_x) / 2.0, (min_y + max_y) / 2.0))
    }

    #[must_use]
    pub fn area_id(&self) -> Option<AreaId> {
        self.area_id
    }

    #[must_use]
    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    #[must_use]
    pub fn level(&self) -> i32 {
        self.level
    }

    pub fn set_level(&mut self, level: i32) {
        if level != self.level {
            self.level = level;
            self.selection.clear();
            self.hovered_room = None;
        }
    }

    /// Changes the displayed level without clearing the selection (for
    /// when the selected entities themselves moved across levels).
    pub fn set_level_keeping_selection(&mut self, level: i32) {
        self.level = level;
        self.hovered_room = None;
    }

    #[must_use]
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Replaces the selection with a single entity (e.g. one just created).
    pub fn select(&mut self, entity: EntityId) {
        self.selection.replace_with(entity);
    }

    /// Adds an entity to the selection (e.g. pasted entities arriving as
    /// their asynchronous creates resolve).
    pub fn add_to_selection(&mut self, entity: EntityId) {
        self.selection.extend([entity]);
    }

    /// Removes an entity from the selection (e.g. after it was cut).
    pub fn remove_from_selection(&mut self, entity: EntityId) {
        self.selection.items.remove(&entity);
    }

    #[must_use]
    pub fn hovered_room(&self) -> Option<&RoomKey> {
        self.hovered_room.as_ref()
    }

    /// Updates the player marker, returning whether it actually moved. The
    /// editor canvas has no animation pumping redraws of its own, so the
    /// caller queues a repaint only when this returns `true`.
    pub fn set_player_location(&mut self, location: Option<RoomKey>) -> bool {
        if self.player_location == location {
            return false;
        }
        self.player_location = location;
        true
    }

    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        match message {
            Message::Translated(translation) => {
                self.translation = translation;
                Update::none()
            }
            Message::Scaled(scaling, translation) => {
                self.scaling = scaling;
                if let Some(translation) = translation {
                    self.translation = translation;
                }
                Update::none()
            }
            Message::ClickSelect { entity, additive } => {
                if additive {
                    self.selection.toggle(entity);
                } else {
                    self.selection.replace_with(entity);
                }
                Update::with_event(Event::SelectionChanged)
            }
            Message::RubberBandSelect { rect, additive } => {
                let hits = self.entities_in_rect(rect);
                if additive {
                    self.selection.extend(hits);
                } else {
                    self.selection.clear();
                    self.selection.extend(hits);
                }
                Update::with_event(Event::SelectionChanged)
            }
            Message::SetHoveredRoom(room_key) => {
                self.hovered_room = room_key.clone();
                Update::with_event(Event::HoveredRoomChanged(room_key))
            }
            Message::MoveCommitted { offset } => Update::with_event(Event::RequestMutation(
                MutationRequest::MoveSelection { offset },
            )),
            Message::PlaceRoom { at, keep_tool } => {
                if !keep_tool {
                    self.tool = Tool::Select;
                }
                Update::with_event(Event::RequestMutation(MutationRequest::PlaceRoom { at }))
            }
            Message::ExitDragCommitted {
                from,
                from_direction,
                to,
                to_direction,
                one_way,
            } => Update::with_event(Event::RequestMutation(MutationRequest::CreateExit {
                from,
                from_direction,
                to,
                to_direction,
                one_way,
            })),
            Message::RectDrawn {
                kind,
                rect,
                keep_tool,
            } => {
                if !keep_tool {
                    self.tool = Tool::Select;
                }
                Update::with_event(Event::RequestMutation(match kind {
                    RectKind::Label => MutationRequest::CreateLabel { rect },
                    RectKind::Shape => MutationRequest::CreateShape { rect },
                }))
            }
            Message::ResizeCommitted { entity, rect } => {
                Update::with_event(Event::RequestMutation(MutationRequest::ResizeEntity {
                    entity,
                    rect,
                }))
            }
        }
    }

    #[must_use]
    pub fn view(&self) -> Element<'_, Message> {
        Canvas::new(self).width(Length::Fill).height(Length::Fill).into()
    }

    #[inline]
    fn viewport(&self) -> Viewport {
        Viewport {
            translation: self.translation,
            scaling: self.scaling,
        }
    }

    /// The topmost entity at a map-space point on the current level.
    /// Priority: rooms above labels above shapes; among labels/shapes the
    /// last in storage order (drawn last, visually topmost) wins.
    #[must_use]
    fn entity_at(&self, point: Point) -> Option<EntityId> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;

        let half_size = render::MAP_ROOM_SIZE / 2.0;
        let mut room_hit = None;
        area.with_rooms_in(
            point.x - half_size,
            point.y - half_size,
            point.x + half_size,
            point.y + half_size,
            |room| {
                if room.get_level() == self.level
                    && (room.get_x() - point.x).abs() < half_size
                    && (room.get_y() - point.y).abs() < half_size
                {
                    room_hit = Some(EntityId::Room(room.get_room_number()));
                }
            },
        );
        if room_hit.is_some() {
            return room_hit;
        }

        if let Some(label) = area
            .get_labels()
            .iter()
            .rev()
            .find(|label| label.level == self.level && rect_contains(label.x, label.y, label.width, label.height, point))
        {
            return Some(EntityId::Label(label.id));
        }

        area.get_shapes()
            .iter()
            .rev()
            .find(|shape| {
                shape.level == self.level
                    && rect_contains(shape.x, shape.y, shape.width, shape.height, point)
            })
            .map(|shape| EntityId::Shape(shape.id))
    }

    /// All entities on the current level intersecting a map-space rect.
    #[must_use]
    fn entities_in_rect(&self, rect: Rectangle) -> Vec<EntityId> {
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = self.area_id.as_ref().and_then(|id| atlas.get_area(id)) else {
            return Vec::new();
        };

        let mut hits = Vec::new();
        let half_size = render::MAP_ROOM_SIZE / 2.0;

        area.with_rooms_in(
            rect.x - half_size,
            rect.y - half_size,
            rect.x + rect.width + half_size,
            rect.y + rect.height + half_size,
            |room| {
                if room.get_level() == self.level
                    && rects_intersect(
                        rect,
                        room.get_x() - half_size,
                        room.get_y() - half_size,
                        render::MAP_ROOM_SIZE,
                        render::MAP_ROOM_SIZE,
                    )
                {
                    hits.push(EntityId::Room(room.get_room_number()));
                }
            },
        );

        for label in area.get_labels() {
            if label.level == self.level
                && rects_intersect(rect, label.x, label.y, label.width, label.height)
            {
                hits.push(EntityId::Label(label.id));
            }
        }

        for shape in area.get_shapes() {
            if shape.level == self.level
                && rects_intersect(rect, shape.x, shape.y, shape.width, shape.height)
            {
                hits.push(EntityId::Shape(shape.id));
            }
        }

        hits
    }

    #[must_use]
    fn room_key_at(&self, point: Point) -> Option<RoomKey> {
        match self.entity_at(point) {
            Some(EntityId::Room(number)) => Some(RoomKey {
                area_id: self.area_id?,
                room_number: number,
            }),
            _ => None,
        }
    }

    /// The bounds of the single selected label/shape on the current level
    /// (the entities that get resize handles).
    #[must_use]
    fn selected_rect(&self) -> Option<(EntityId, Rectangle)> {
        let entity = self.selection.single()?;
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;

        match entity {
            EntityId::Label(id) => {
                let label = area.get_label(&id)?;
                (label.level == self.level).then_some((
                    entity,
                    Rectangle {
                        x: label.x,
                        y: label.y,
                        width: label.width,
                        height: label.height,
                    },
                ))
            }
            EntityId::Shape(id) => {
                let shape = area.get_shape(&id)?;
                (shape.level == self.level).then_some((
                    entity,
                    Rectangle {
                        x: shape.x,
                        y: shape.y,
                        width: shape.width,
                        height: shape.height,
                    },
                ))
            }
            EntityId::Room(_) => None,
        }
    }

    /// The room under a map-space point on the current level, with its
    /// center (for exit-drag geometry).
    #[must_use]
    fn room_at_with_center(&self, point: Point) -> Option<(RoomNumber, Point)> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;

        let half_size = render::MAP_ROOM_SIZE / 2.0;
        let mut hit = None;
        area.with_rooms_in(
            point.x - half_size,
            point.y - half_size,
            point.x + half_size,
            point.y + half_size,
            |room| {
                if room.get_level() == self.level
                    && (room.get_x() - point.x).abs() < half_size
                    && (room.get_y() - point.y).abs() < half_size
                {
                    hit = Some((
                        room.get_room_number(),
                        Point::new(room.get_x(), room.get_y()),
                    ));
                }
            },
        );
        hit
    }

}

/// The compass octant pointing from one map-space point toward another
/// (map y grows southward).
#[must_use]
pub(crate) fn direction_between(from: Point, to: Point) -> ExitDirection {
    let angle = (to.y - from.y).atan2(to.x - from.x).to_degrees();

    match angle {
        a if (-22.5..22.5).contains(&a) => ExitDirection::East,
        a if (22.5..67.5).contains(&a) => ExitDirection::Southeast,
        a if (67.5..112.5).contains(&a) => ExitDirection::South,
        a if (112.5..157.5).contains(&a) => ExitDirection::Southwest,
        a if (-67.5..-22.5).contains(&a) => ExitDirection::Northeast,
        a if (-112.5..-67.5).contains(&a) => ExitDirection::North,
        a if (-157.5..-112.5).contains(&a) => ExitDirection::Northwest,
        _ => ExitDirection::West,
    }
}

fn rect_contains(x: f32, y: f32, width: f32, height: f32, point: Point) -> bool {
    point.x >= x && point.x <= x + width && point.y >= y && point.y <= y + height
}

fn rects_intersect(rect: Rectangle, x: f32, y: f32, width: f32, height: f32) -> bool {
    rect.x < x + width && x < rect.x + rect.width && rect.y < y + height && y < rect.y + rect.height
}
