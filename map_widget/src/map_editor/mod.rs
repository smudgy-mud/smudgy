//! An editable map canvas: pan/zoom like [`crate::MapView`], plus level
//! ghosting, entity selection, and (via [`Event::RequestMutation`]) edit
//! gestures. The widget owns *view* state only — every mutation is
//! delegated to the host window, which owns the undo stack and the
//! [`Mapper`] write path.

mod program;

use std::cell::Cell;
use std::collections::HashSet;
use std::sync::Arc;

use iced::{
    Length, Point, Rectangle, Size, Vector,
    widget::{Canvas, container},
};
use smudgy_cloud::{
    AreaId, ConnectionId, ConnectionUpdates, ExitDirection, LabelId, MapPoint, Mapper, RoomNumber,
    ShapeId, SourceId,
    connection_geometry::ConnectionGeometry,
    mapper::{RoomKey, area_cache::SourceLayer},
};

use crate::{Update, render, viewport, viewport::Viewport};

pub type Renderer = iced::Renderer;
pub type Theme = smudgy_theme::Theme;
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, Renderer>;

/// Anything selectable on the editor canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityId {
    Connection(ConnectionId),
    Room(RoomNumber),
    /// One of a Secret's (or Private additions') own rooms.
    SourceRoom(SourceId, RoomNumber),
    Label(LabelId),
    Shape(ShapeId),
}

/// A room is identified by its source and number throughout the editor.
pub use smudgy_cloud::RoomAddress as PlacedRoom;

impl From<PlacedRoom> for EntityId {
    fn from(room: PlacedRoom) -> Self {
        if room.source.is_map() {
            Self::Room(room.number)
        } else {
            Self::SourceRoom(room.source, room.number)
        }
    }
}

/// The editable point within a selected Connection. Kept separate from the
/// entity selection so Escape and Delete can leave the link selected while
/// exiting point editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedConnectionHandle {
    PortA,
    PortB,
    Waypoint(usize),
}

/// Semantic canvas activity exposed to editor chrome. It deliberately avoids
/// canvas implementation details so future tools can reuse the status legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorActivity {
    #[default]
    Idle,
    DraggingConnectionWaypoint,
    DraggingConnectionPort,
}

/// One status-bar hint: an optional key or chord and the action it performs
/// in the current editor context. `action`, and `key` when it names a
/// gesture rather than a key (it starts with `legend-`), are translation
/// keys the UI translates; chords like "Alt" or "Ctrl+click" are literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegendItem {
    pub key: &'static str,
    pub action: &'static str,
}

/// The subject the editor-legend resolver describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LegendContext {
    #[default]
    None,
    Connection {
        routing: smudgy_cloud::ConnectionRouting,
        waypoint_selected: bool,
        /// A port handle is selected: arrow keys slide it along its wall.
        port_selected: bool,
    },
}

/// Resolves status-footer hints from semantic editor state. Presentation is a
/// separate concern owned by the UI crate.
#[must_use]
pub fn resolve_legend(
    activity: EditorActivity,
    editable: bool,
    context: LegendContext,
) -> Vec<LegendItem> {
    match activity {
        EditorActivity::DraggingConnectionWaypoint => {
            return vec![
                LegendItem {
                    key: "Alt",
                    action: "legend-move-freely",
                },
                LegendItem {
                    key: "Esc",
                    action: "legend-cancel",
                },
            ];
        }
        EditorActivity::DraggingConnectionPort => {
            return vec![
                LegendItem {
                    key: "legend-key-drag",
                    action: "legend-snap-port",
                },
                LegendItem {
                    key: "Alt",
                    action: "legend-move-freely",
                },
                LegendItem {
                    key: "Esc",
                    action: "legend-cancel",
                },
            ];
        }
        EditorActivity::Idle => {}
    }

    let LegendContext::Connection {
        routing,
        waypoint_selected,
        port_selected,
    } = context
    else {
        return Vec::new();
    };
    if !editable {
        return vec![LegendItem {
            key: "",
            action: "legend-read-only",
        }];
    }
    if waypoint_selected {
        return vec![
            LegendItem {
                key: "legend-key-drag",
                action: "legend-move-point",
            },
            LegendItem {
                key: "legend-key-delete",
                action: "legend-remove-point",
            },
            LegendItem {
                key: "Esc",
                action: "legend-stop-editing",
            },
        ];
    }
    if port_selected {
        return vec![
            LegendItem {
                key: "legend-key-drag",
                action: "legend-move-port",
            },
            LegendItem {
                key: "←→↑↓",
                action: "legend-slide-port",
            },
            LegendItem {
                key: "Esc",
                action: "legend-stop-editing",
            },
        ];
    }

    match routing {
        smudgy_cloud::ConnectionRouting::Simple | smudgy_cloud::ConnectionRouting::Manual => {
            vec![LegendItem {
                key: "Ctrl+click",
                action: "legend-add-point",
            }]
        }
        // Dragging the line body is deliberately inert; only Ctrl+click and
        // handle drags convert an Automatic route.
        smudgy_cloud::ConnectionRouting::Automatic => vec![LegendItem {
            key: "Ctrl+click",
            action: "legend-add-point",
        }],
        smudgy_cloud::ConnectionRouting::Stub => Vec::new(),
    }
}

/// The active editing tool. Creation tools are momentary: the host window
/// reverts to [`Tool::Select`] after a placement unless Shift is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Link,
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

    /// Selected rooms of the map's other sources.
    pub fn source_rooms(&self) -> impl Iterator<Item = (SourceId, RoomNumber)> + '_ {
        self.items.iter().filter_map(|entity| match entity {
            EntityId::SourceRoom(source, number) => Some((*source, *number)),
            _ => None,
        })
    }

    pub fn connections(&self) -> impl Iterator<Item = ConnectionId> + '_ {
        self.items.iter().filter_map(|entity| match entity {
            EntityId::Connection(id) => Some(*id),
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
        from: PlacedRoom,
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
    /// Commit a port/waypoint or inspector visual edit as one Connection
    /// mutation. Canvas drags preview locally and publish only on release.
    UpdateConnection {
        connection_id: ConnectionId,
        updates: ConnectionUpdates,
        description: &'static str,
    },
    /// Delete one selected stored route vertex without deleting the link.
    DeleteWaypoint {
        connection_id: ConnectionId,
        index: usize,
    },
}

/// Where an exit drag was dropped.
#[derive(Debug, Clone, Copy)]
pub enum ExitTarget {
    Room(PlacedRoom),
    /// Empty canvas; a connected room is created here (snapped already,
    /// unless the user held Alt).
    Empty(Point),
    /// Empty canvas while Shift is held; creates no destination room.
    Dangling(Point),
}

#[derive(Debug, Clone)]
pub enum Message {
    Translated(Vector),
    /// A right click at `at` (canvas-relative) over map point `map`;
    /// `translation` is the view before the press panned it.
    ContextMenuRequested {
        at: Point,
        map: Point,
        translation: Vector,
    },
    /// Add a route point to a link where the context menu was opened.
    InsertWaypointAt {
        connection_id: ConnectionId,
        at: Point,
    },
    Scaled(f32, Option<Vector>),
    ClickSelect {
        entity: EntityId,
        additive: bool,
    },
    /// A room on an adjacent level was clicked: a map room or one of a
    /// place's own rooms.
    GhostRoomSelected {
        room: EntityId,
        level: i32,
    },
    /// Rubber-band finished: select everything intersecting `rect`
    /// (map space).
    RubberBandSelect {
        rect: Rectangle,
        additive: bool,
    },
    /// The topmost hover targets under an idle cursor: the hovered room (any
    /// tool) and, in Select mode, the hovered Connection — published
    /// together so one mouse move can never leave one of them stale.
    SetHovered {
        room: Option<RoomKey>,
        connection: Option<ConnectionId>,
    },
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
        from: PlacedRoom,
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
    ConnectionHandleSelected {
        connection_id: ConnectionId,
        handle: SelectedConnectionHandle,
    },
    ConnectionUpdated {
        connection_id: ConnectionId,
        updates: ConnectionUpdates,
        description: &'static str,
    },
    WaypointInserted {
        connection_id: ConnectionId,
        index: usize,
        points: Vec<MapPoint>,
        selected_offset: usize,
    },
    ActivityChanged(EditorActivity),
    /// A room clicked while the editor waits for one to be picked (see
    /// [`MapEditor::set_picking`]).
    RoomPicked(PlacedRoom),
}

/// Where a Link-tool drag dropped: on a room, on empty canvas where a room
/// is made, or (Shift) where the link leads nowhere. `Nothing` when it ends
/// on the room it started from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LinkDrop {
    Nothing,
    Room(PlacedRoom, Point),
    Empty(Point),
    Dangling(Point),
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
    /// Open the context menu at `at` (canvas-relative) for the selection,
    /// which the click has already settled; `map` is the map point under
    /// it.
    ContextMenu {
        at: Point,
        map: Point,
    },
    HoveredRoomChanged(Option<RoomKey>),
    RequestMutation(MutationRequest),
    /// A room was picked on the canvas while picking (see
    /// [`MapEditor::set_picking`]); the selection is left as it was.
    RoomPicked(PlacedRoom),
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
    /// The Connection under an idle Select-tool cursor, drawn with a muted
    /// accent glow so the invisible hit band and click cycling are
    /// discoverable.
    hovered_connection: Option<ConnectionId>,
    /// The room the user came *from* when the selection transitioned room →
    /// one of its connections. Presentation-only: the inspector shows the
    /// connection from this room's perspective (it is the "From" end),
    /// whatever the stored endpoint order says.
    connection_anchor: Option<PlacedRoom>,
    selected_connection_handle: Option<(ConnectionId, SelectedConnectionHandle)>,
    /// Accepted solver output awaiting user confirmation. This is view-only:
    /// the cache and stored Connection remain untouched until the host emits
    /// one CAS command from the preview dialog.
    automatic_route_preview: Option<(ConnectionId, Arc<ConnectionGeometry>)>,
    activity: EditorActivity,
    editable: bool,
    /// Where new content goes ("Add to"): where rooms overlap, its rooms
    /// win the click.
    add_to: SourceId,
    /// A click on a room picks it ([`Event::RoomPicked`]) instead of
    /// selecting it: the inspector is choosing a link's room.
    picking: bool,
}

impl MapEditor {
    const MIN_SCALING: f32 = 2.0;
    const MAX_SCALING: f32 = 200.0;
    /// Must stay comfortably above the worst-case overhang of a level
    /// treatment glyph past its Connection's stroke bounds (~0.8 map units:
    /// [`render::LEVEL_TREATMENT_REACH`] with the port dragged to the
    /// opposite wall), or edge-of-viewport glyphs get culled.
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
            hovered_connection: None,
            connection_anchor: None,
            selected_connection_handle: None,
            automatic_route_preview: None,
            activity: EditorActivity::Idle,
            editable: true,
            add_to: SourceId::Map,
            picking: false,
        };
        editor.set_area(area_id);
        editor
    }

    /// Tells the canvas where new content goes, so overlapping rooms
    /// resolve toward that place's.
    pub fn set_add_to(&mut self, add_to: SourceId) {
        self.add_to = add_to;
    }

    /// Turns room picking on or off. While on, a click on a room (this
    /// level's, or a ghost on the next ones) picks it and nothing else
    /// happens: the selection stays, and the Select tool is in use.
    pub fn set_picking(&mut self, picking: bool) {
        self.picking = picking;
        if picking {
            self.tool = Tool::Select;
            self.hovered_connection = None;
            self.activity = EditorActivity::Idle;
        }
    }

    /// Whether a click on a room picks it rather than selecting it.
    #[must_use]
    pub fn picking(&self) -> bool {
        self.picking
    }

    /// Selects link `connection_id` from one of its rooms, which becomes the
    /// link's "From" end in the inspector, as a click on the link right after
    /// the room makes it.
    pub fn select_link_from(&mut self, connection_id: ConnectionId, room: EntityId) {
        let anchor = self.endpoint_of(connection_id, room);
        self.select(EntityId::Connection(connection_id));
        self.connection_anchor = anchor;
    }

    /// How a room of `source` ranks where rooms overlap: "Add to"'s first,
    /// then the map's, then every other place's.
    fn room_rank(&self, source: SourceId) -> u8 {
        if source == self.add_to {
            0
        } else if source.is_map() {
            1
        } else {
            2
        }
    }

    /// Switches the displayed area, clearing selection and view state.
    pub fn set_area(&mut self, area_id: Option<AreaId>) {
        self.area_id = area_id;
        self.picking = false;
        self.selection.clear();
        self.hovered_room = None;
        self.hovered_connection = None;
        self.connection_anchor = None;
        self.selected_connection_handle = None;
        self.automatic_route_preview = None;
        self.activity = EditorActivity::Idle;
        self.level = 0;
        self.translation = self.center_of_area().map_or_else(
            || Vector::new(0.0, 0.0),
            |center| Vector::new(-center.x, -center.y),
        );
    }

    /// The middle of the canvas, canvas-relative and as a map point: where a
    /// keyboard-opened context menu goes. `None` before the first draw.
    #[must_use]
    pub fn canvas_center(&self) -> Option<(Point, Point)> {
        let size = self.last_viewport_size.get()?;
        let at = Point::new(size.width / 2.0, size.height / 2.0);
        Some((at, self.viewport().project(at, size)))
    }

    /// Shows `level` with the map-space point `at` in the middle of the
    /// canvas, keeping the selection.
    pub fn center_on(&mut self, at: Point, level: i32) {
        self.set_level_keeping_selection(level);
        self.translation = Vector::new(-at.x, -at.y);
    }

    /// The bounding-box center of the area's rooms, if it has any: the map's
    /// and those of every place the viewer reads.
    fn center_of_area(&self) -> Option<Point> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        let mut iter = area.get_rooms().iter().chain(
            area.source_layers()
                .iter()
                .flat_map(|layer| layer.content().get_rooms().iter()),
        );
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
        // Connection hover is a Select-tool affordance; hover state is only
        // republished on cursor movement, so clear it here rather than glow
        // under the wrong tool.
        if tool != Tool::Select {
            self.hovered_connection = None;
        }
        self.activity = EditorActivity::Idle;
    }

    /// Enables mutation affordances while preserving selection/inspection in
    /// view-only areas.
    pub fn set_editable(&mut self, editable: bool) {
        self.editable = editable;
        if !editable {
            self.tool = Tool::Select;
            self.selected_connection_handle = None;
        }
        self.activity = EditorActivity::Idle;
    }

    /// Installs or clears a view-only Automatic route preview.
    pub fn set_automatic_route_preview(
        &mut self,
        preview: Option<(ConnectionId, Arc<ConnectionGeometry>)>,
    ) {
        self.automatic_route_preview = preview;
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
            self.hovered_connection = None;
            self.connection_anchor = None;
            self.selected_connection_handle = None;
            self.automatic_route_preview = None;
            self.activity = EditorActivity::Idle;
        }
    }

    /// Changes the displayed level without clearing the selection (for
    /// when the selected entities themselves moved across levels).
    pub fn set_level_keeping_selection(&mut self, level: i32) {
        self.level = level;
        self.hovered_room = None;
        self.hovered_connection = None;
    }

    #[must_use]
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.connection_anchor = None;
        self.selected_connection_handle = None;
        self.activity = EditorActivity::Idle;
    }

    /// Replaces the selection with a single entity (e.g. one just created).
    pub fn select(&mut self, entity: EntityId) {
        self.selection.replace_with(entity);
        self.connection_anchor = None;
        self.selected_connection_handle = None;
        self.activity = EditorActivity::Idle;
    }

    /// Adds an entity to the selection (e.g. pasted entities arriving as
    /// their asynchronous creates resolve).
    pub fn add_to_selection(&mut self, entity: EntityId) {
        self.selection.extend([entity]);
        self.selected_connection_handle = None;
        self.activity = EditorActivity::Idle;
    }

    /// Removes an entity from the selection (e.g. after it was cut).
    pub fn remove_from_selection(&mut self, entity: EntityId) {
        self.selection.items.remove(&entity);
        if self
            .selected_connection_handle
            .is_some_and(|(connection_id, _)| entity == EntityId::Connection(connection_id))
        {
            self.selected_connection_handle = None;
        }
        self.activity = EditorActivity::Idle;
    }

    #[must_use]
    pub fn selected_waypoint(&self) -> Option<(ConnectionId, usize)> {
        self.selected_connection_handle
            .and_then(|(connection_id, handle)| match handle {
                SelectedConnectionHandle::Waypoint(index) => Some((connection_id, index)),
                SelectedConnectionHandle::PortA | SelectedConnectionHandle::PortB => None,
            })
    }

    #[must_use]
    pub fn selected_connection_handle(&self) -> Option<(ConnectionId, SelectedConnectionHandle)> {
        self.selected_connection_handle
    }

    pub fn clear_selected_waypoint(&mut self) {
        self.selected_connection_handle = None;
        self.activity = EditorActivity::Idle;
    }

    /// Exits port/waypoint editing while retaining the Connection selection.
    /// Returns whether a handle was active.
    pub fn clear_selected_connection_handle(&mut self) -> bool {
        self.activity = EditorActivity::Idle;
        self.selected_connection_handle.take().is_some()
    }

    /// Context-sensitive editor hints. Connection editing is the first
    /// consumer; callers receive structured key/action pairs so other tools
    /// can reuse the same footer without formatting behavior in UI code.
    #[must_use]
    pub fn legend_items(&self) -> Vec<LegendItem> {
        let Some(EntityId::Connection(connection_id)) = self.selection.single() else {
            return resolve_legend(self.activity, self.editable, LegendContext::None);
        };
        let waypoint_selected = matches!(
            self.selected_connection_handle,
            Some((selected, SelectedConnectionHandle::Waypoint(_))) if selected == connection_id
        );
        let port_selected = matches!(
            self.selected_connection_handle,
            Some((
                selected,
                SelectedConnectionHandle::PortA | SelectedConnectionHandle::PortB,
            )) if selected == connection_id
        );

        let atlas = self.mapper.get_current_atlas();
        let Some(routing) = self
            .area_id
            .as_ref()
            .and_then(|area_id| atlas.get_area(area_id))
            .and_then(|area| {
                area.find_connection(connection_id)
                    .map(|(_, connection)| connection.routing)
            })
        else {
            return resolve_legend(self.activity, self.editable, LegendContext::None);
        };
        resolve_legend(
            self.activity,
            self.editable,
            LegendContext::Connection {
                routing,
                waypoint_selected,
                port_selected,
            },
        )
    }

    /// The perspective anchor of the selected connection: the room the
    /// selection transitioned from, when it was one of the connection's
    /// endpoints, preserving the source that owns the anchor room.
    #[must_use]
    pub fn connection_anchor(&self) -> Option<PlacedRoom> {
        self.connection_anchor
    }

    /// The qualified address of `room`, when it is a connection endpoint.
    fn endpoint_of(&self, connection_id: ConnectionId, room: EntityId) -> Option<PlacedRoom> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        let (document, _) = area.connection_document(connection_id)?;
        let address = match room {
            EntityId::Room(number) => PlacedRoom::map(number),
            EntityId::SourceRoom(source, number) => PlacedRoom::new(source, number),
            _ => return None,
        };
        let connection = document.get_connection(connection_id)?;
        (connection.endpoint_a.address() == address
            || connection
                .endpoint_b
                .is_some_and(|endpoint| endpoint.address() == address))
        .then_some(address)
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
            Message::ContextMenuRequested {
                at,
                map,
                translation,
            } => {
                self.translation = translation;
                // The menu acts on the selection when the click lands on
                // part of it; otherwise on what is under the cursor, which
                // becomes the selection, or on nothing.
                let hits = self.entities_at(map);
                if !hits.iter().any(|entity| self.selection.contains(*entity)) {
                    self.selected_connection_handle = None;
                    match hits.first() {
                        Some(entity) => self.selection.replace_with(*entity),
                        None => self.selection.clear(),
                    }
                }
                Update::with_event(Event::ContextMenu { at, map })
            }
            Message::InsertWaypointAt { connection_id, at } => {
                let Some((index, points, selected_offset)) = self.waypoint_insertion(
                    connection_id,
                    at,
                    iced::keyboard::Modifiers::default(),
                ) else {
                    return Update::none();
                };
                self.update(Message::WaypointInserted {
                    connection_id,
                    index,
                    points,
                    selected_offset,
                })
            }
            Message::Scaled(scaling, translation) => {
                self.scaling = scaling;
                if let Some(translation) = translation {
                    self.translation = translation;
                }
                Update::none()
            }
            Message::ClickSelect { entity, additive } => {
                // A room → its-connection transition remembers the room as
                // the perspective anchor; re-clicking the same connection
                // (click cycling) keeps it. Everything else forgets it.
                self.connection_anchor = match entity {
                    EntityId::Connection(id) => match self.selection.single() {
                        Some(room @ (EntityId::Room(_) | EntityId::SourceRoom(..))) => {
                            self.endpoint_of(id, room)
                        }
                        Some(EntityId::Connection(previous)) if previous == id => {
                            self.connection_anchor
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if additive {
                    self.selection.toggle(entity);
                } else {
                    self.selection.replace_with(entity);
                }
                self.selected_connection_handle = None;
                self.activity = EditorActivity::Idle;
                Update::with_event(Event::SelectionChanged)
            }
            Message::GhostRoomSelected { room, level } => {
                self.set_level(level);
                self.selection.replace_with(room);
                Update::with_event(Event::SelectionChanged)
            }
            Message::RubberBandSelect { rect, additive } => {
                let hits = self.entities_in_rect(rect);
                self.connection_anchor = None;
                if additive {
                    self.selection.extend(hits);
                } else {
                    self.selection.clear();
                    self.selection.extend(hits);
                }
                Update::with_event(Event::SelectionChanged)
            }
            Message::SetHovered { room, connection } => {
                self.hovered_connection = connection;
                if self.hovered_room == room {
                    return Update::none();
                }
                self.hovered_room = room.clone();
                Update::with_event(Event::HoveredRoomChanged(room))
            }
            Message::MoveCommitted { offset } => {
                Update::with_event(Event::RequestMutation(MutationRequest::MoveSelection {
                    offset,
                }))
            }
            Message::PlaceRoom { at, keep_tool } => {
                if !keep_tool {
                    self.tool = Tool::Select;
                }
                // A room is never made on top of another: a click on an
                // occupied cell selects the room there.
                if let Some((room, _)) = self.room_occupying(at) {
                    self.selection.replace_with(EntityId::from(room));
                    self.connection_anchor = None;
                    self.selected_connection_handle = None;
                    return Update::with_event(Event::SelectionChanged);
                }
                Update::with_event(Event::RequestMutation(MutationRequest::PlaceRoom { at }))
            }
            Message::RoomPicked(room) => Update::with_event(Event::RoomPicked(room)),
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
            Message::ConnectionHandleSelected {
                connection_id,
                handle,
            } => {
                self.selection
                    .replace_with(EntityId::Connection(connection_id));
                self.selected_connection_handle = Some((connection_id, handle));
                self.automatic_route_preview = None;
                // A press is selection only; the canvas reports the drag
                // activity separately once the pointer crosses the drag
                // threshold, so the legend doesn't flip on a bare click.
                self.activity = EditorActivity::Idle;
                Update::with_event(Event::SelectionChanged)
            }
            Message::ConnectionUpdated {
                connection_id,
                updates,
                description,
            } => {
                self.activity = EditorActivity::Idle;
                Update::with_event(Event::RequestMutation(MutationRequest::UpdateConnection {
                    connection_id,
                    updates,
                    description,
                }))
            }
            Message::WaypointInserted {
                connection_id,
                index,
                points,
                selected_offset,
            } => {
                let Some(area) = self
                    .area_id
                    .and_then(|area_id| self.mapper.get_current_atlas().get_area(&area_id))
                else {
                    return Update::none();
                };
                let Some((_, connection)) = area.find_connection(connection_id) else {
                    return Update::none();
                };
                let mut route_points = connection.route_points.clone();
                let index = index.min(route_points.len());
                if points.is_empty()
                    || route_points.len().saturating_add(points.len())
                        > smudgy_cloud::MAX_ROUTE_POINTS
                {
                    return Update::none();
                }
                route_points.splice(index..index, points);
                self.selection
                    .replace_with(EntityId::Connection(connection_id));
                self.selected_connection_handle = Some((
                    connection_id,
                    SelectedConnectionHandle::Waypoint(
                        index + selected_offset.min(route_points.len() - index - 1),
                    ),
                ));
                self.activity = EditorActivity::Idle;
                Update::new(
                    iced::Task::none(),
                    Some(Event::RequestMutation(MutationRequest::UpdateConnection {
                        connection_id,
                        updates: ConnectionUpdates {
                            routing: Some(smudgy_cloud::ConnectionRouting::Manual),
                            route_points: Some(route_points),
                            ..ConnectionUpdates::default()
                        },
                        description: "Add connection waypoint",
                    })),
                )
            }
            Message::ActivityChanged(activity) => {
                self.activity = activity;
                Update::none()
            }
        }
    }

    #[must_use]
    pub fn view(&self) -> Element<'_, Message> {
        // Clip to widget bounds, as MapView does: the canvas draws entities
        // within the spatial-query padding of the visible region plus
        // grid/preview/ghost geometry, all of which can land outside it. wgpu
        // hides the spill (full-frame redraws paint neighbors over it);
        // tiny-skia's damage-tracked partial redraws leave it on screen —
        // over the editor's own side panes.
        container(Canvas::new(self).width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .into()
    }

    #[inline]
    fn viewport(&self) -> Viewport {
        Viewport {
            translation: self.translation,
            scaling: self.scaling,
        }
    }

    /// Every selectable entity at a map-space point, in selection priority
    /// order. The small center of a room deliberately precedes crossing
    /// strokes; outside that refuge, visible Connection geometry precedes the
    /// rest of the room fill. Returning the full list lets repeated clicks
    /// cycle crossings instead of making the nearest line permanently hide
    /// everything below it.
    #[must_use]
    fn entities_at(&self, point: Point) -> Vec<EntityId> {
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = self.area_id.as_ref().and_then(|id| atlas.get_area(id)) else {
            return Vec::new();
        };

        let half_size = render::MAP_ROOM_SIZE / 2.0;
        let inner_half = render::MAP_ROOM_SIZE * 0.2;
        let mut room_hits = Vec::new();
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
                    let inner = (room.get_x() - point.x).abs() <= inner_half
                        && (room.get_y() - point.y).abs() <= inner_half;
                    room_hits.push((EntityId::Room(room.get_room_number()), inner));
                }
            },
        );
        // Only owned rooms participate in hit testing.
        for layer in area.source_layers() {
            layer.content().with_rooms_in(
                point.x - half_size,
                point.y - half_size,
                point.x + half_size,
                point.y + half_size,
                |room| {
                    let number = room.get_room_number();
                    if room.get_level() == self.level
                        && (room.get_x() - point.x).abs() < half_size
                        && (room.get_y() - point.y).abs() < half_size
                    {
                        let inner = (room.get_x() - point.x).abs() <= inner_half
                            && (room.get_y() - point.y).abs() <= inner_half;
                        room_hits.push((EntityId::SourceRoom(layer.source(), number), inner));
                    }
                },
            );
        }
        // Where rooms overlap, "Add to"'s rooms win the click.
        room_hits.sort_by_key(|(entity, _)| match entity {
            EntityId::Room(number) => (self.room_rank(SourceId::Map), number.0),
            EntityId::SourceRoom(source, number) => (self.room_rank(*source), number.0),
            _ => (3, 0),
        });

        let mut hits = Vec::new();
        hits.extend(
            room_hits
                .iter()
                .filter(|(_, inner)| *inner)
                .map(|(entity, _)| *entity),
        );
        hits.extend(
            self.connection_hits(area.as_ref(), point)
                .into_iter()
                .map(EntityId::Connection),
        );
        hits.extend(
            room_hits
                .iter()
                .filter(|(_, inner)| !*inner)
                .map(|(entity, _)| *entity),
        );

        // Labels and shapes keep their ids in every source, so a source's
        // are hit exactly like the map's.
        let holders = || {
            std::iter::once(area.as_ref())
                .chain(area.source_layers().iter().map(SourceLayer::content))
        };
        for holder in holders() {
            hits.extend(
                holder
                    .get_labels()
                    .iter()
                    .rev()
                    .filter(|label| {
                        label.level == self.level
                            && rect_contains(label.x, label.y, label.width, label.height, point)
                    })
                    .map(|label| EntityId::Label(label.id)),
            );
        }
        for holder in holders() {
            hits.extend(
                holder
                    .get_shapes()
                    .iter()
                    .rev()
                    .filter(|shape| {
                        shape.level == self.level
                            && rect_contains(shape.x, shape.y, shape.width, shape.height, point)
                    })
                    .map(|shape| EntityId::Shape(shape.id)),
            );
        }
        hits
    }

    /// The first entity in the selection priority at a point.
    #[must_use]
    fn entity_at(&self, point: Point) -> Option<EntityId> {
        self.entities_at(point).into_iter().next()
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

        let mut connection_ids = HashSet::new();
        // Padded so a rubber band tight around a level treatment glyph
        // (which can sit outside the stroke bounds) still finds its half.
        let glyph_pad = render::LEVEL_TREATMENT_REACH + render::MAP_ROOM_SIZE;
        // A Secret's links are selected like the map's.
        for holder in std::iter::once(area.as_ref())
            .chain(area.source_layers().iter().map(SourceLayer::content))
        {
            holder.with_room_connections_in(
                rect.x - glyph_pad,
                rect.y - glyph_pad,
                rect.x + rect.width + glyph_pad,
                rect.y + rect.height + glyph_pad,
                |connection| {
                    if connection.from_level != self.level {
                        return;
                    }
                    let bounds_hit = connection.geometry.bounds.max_x >= rect.x
                        && connection.geometry.bounds.min_x <= rect.x + rect.width
                        && connection.geometry.bounds.max_y >= rect.y
                        && connection.geometry.bounds.min_y <= rect.y + rect.height;
                    // The drawn level glyph is selectable exactly as drawn;
                    // both treatment forms are axis-aligned, so a box test is
                    // exact.
                    let glyph_hit = !bounds_hit
                        && render::level_treatment(connection, false).is_some_and(|treatment| {
                            let (min, max) = treatment.bounding_box();
                            rects_intersect(rect, min.x, min.y, max.x - min.x, max.y - min.y)
                        });
                    if (bounds_hit || glyph_hit) && connection_ids.insert(connection.connection_id)
                    {
                        hits.push(EntityId::Connection(connection.connection_id));
                    }
                },
            );
        }

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
        for layer in area.source_layers() {
            layer.content().with_rooms_in(
                rect.x - half_size,
                rect.y - half_size,
                rect.x + rect.width + half_size,
                rect.y + rect.height + half_size,
                |room| {
                    let number = room.get_room_number();
                    if room.get_level() == self.level
                        && rects_intersect(
                            rect,
                            room.get_x() - half_size,
                            room.get_y() - half_size,
                            render::MAP_ROOM_SIZE,
                            render::MAP_ROOM_SIZE,
                        )
                    {
                        hits.push(EntityId::SourceRoom(layer.source(), number));
                    }
                },
            );
        }

        for holder in std::iter::once(area.as_ref())
            .chain(area.source_layers().iter().map(SourceLayer::content))
        {
            for label in holder.get_labels() {
                if label.level == self.level
                    && rects_intersect(rect, label.x, label.y, label.width, label.height)
                {
                    hits.push(EntityId::Label(label.id));
                }
            }
            for shape in holder.get_shapes() {
                if shape.level == self.level
                    && rects_intersect(rect, shape.x, shape.y, shape.width, shape.height)
                {
                    hits.push(EntityId::Shape(shape.id));
                }
            }
        }

        hits
    }

    /// Visible Connection strokes and level-change glyphs within a stable
    /// six-pixel target, nearest first and UUID-stable for crossing
    /// click-cycling: the map's links and every Secret's.
    fn connection_hits(
        &self,
        area: &smudgy_cloud::mapper::area_cache::AreaCache,
        point: Point,
    ) -> Vec<ConnectionId> {
        let mut hits = Vec::new();
        let mut seen = HashSet::new();
        for holder in
            std::iter::once(area).chain(area.source_layers().iter().map(SourceLayer::content))
        {
            self.collect_connection_hits(holder, point, &mut seen, &mut hits);
        }
        hits.sort_by(|(id_a, distance_a), (id_b, distance_b)| {
            distance_a
                .total_cmp(distance_b)
                .then_with(|| id_a.cmp(id_b))
        });
        hits.into_iter().map(|(id, _)| id).collect()
    }

    /// One document's [`Self::connection_hits`], with each hit's distance.
    fn collect_connection_hits(
        &self,
        area: &smudgy_cloud::mapper::area_cache::AreaCache,
        point: Point,
        seen: &mut HashSet<ConnectionId>,
        hits: &mut Vec<(ConnectionId, f32)>,
    ) {
        let tolerance = 6.0 / self.scaling;
        let map_point = MapPoint::new(point.x, point.y);
        // Level treatments (corner triangles, fading directional stubs) can
        // reach outside a cross-level Connection's stroke bounds; pad the
        // spatial query so their halves stay candidates. The extra room
        // width covers the worst case of a port dragged to the wall
        // opposite the exit direction, where the stroke envelope starts on
        // the far side of the room the glyph hangs off.
        let reach = tolerance + render::LEVEL_TREATMENT_REACH + render::MAP_ROOM_SIZE;
        area.with_room_connections_in(
            point.x - reach,
            point.y - reach,
            point.x + reach,
            point.y + reach,
            |connection| {
                if connection.from_level != self.level || !seen.insert(connection.connection_id) {
                    return;
                }
                let mut distance = if connection.geometry.hit_test(map_point, tolerance) {
                    connection.geometry.distance_to(map_point)
                } else {
                    f32::INFINITY
                };
                // The rendered level glyph is clickable exactly as drawn.
                if let Some(treatment) = render::level_treatment(connection, false) {
                    let glyph = treatment.distance_to(map_point);
                    if glyph <= tolerance {
                        distance = distance.min(glyph);
                    }
                }
                if distance.is_finite() {
                    hits.push((connection.connection_id, distance));
                }
            },
        );
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
                let (_, label) = area.find_label(&id)?;
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
                let (_, shape) = area.find_shape(&id)?;
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
            EntityId::Room(_) | EntityId::SourceRoom(..) | EntityId::Connection(_) => None,
        }
    }

    /// The room under a map-space point on the current level, with its
    /// center (for exit-drag geometry): a map room or one of a Secret's
    /// (or Private additions') own rooms. Where rooms overlap, "Add to"'s
    /// win, then the map's.
    #[must_use]
    fn link_end_at(&self, point: Point) -> Option<(PlacedRoom, Point)> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;

        let half_size = render::MAP_ROOM_SIZE / 2.0;
        let mut hit: Option<(u8, PlacedRoom, Point)> = None;
        let mut offer = |rank: u8, room: PlacedRoom, center: Point| {
            // The last room found within the best rank wins, as the map's
            // own topmost room always has.
            if hit.is_none_or(|(best, _, _)| rank <= best) {
                hit = Some((rank, room, center));
            }
        };
        let under = |room: &smudgy_cloud::mapper::room_cache::RoomCache| {
            room.get_level() == self.level
                && (room.get_x() - point.x).abs() < half_size
                && (room.get_y() - point.y).abs() < half_size
        };
        area.with_rooms_in(
            point.x - half_size,
            point.y - half_size,
            point.x + half_size,
            point.y + half_size,
            |room| {
                if under(room) {
                    offer(
                        self.room_rank(SourceId::Map),
                        PlacedRoom::map(room.get_room_number()),
                        Point::new(room.get_x(), room.get_y()),
                    );
                }
            },
        );
        for layer in area.source_layers() {
            layer.content().with_rooms_in(
                point.x - half_size,
                point.y - half_size,
                point.x + half_size,
                point.y + half_size,
                |room| {
                    let number = room.get_room_number();
                    if under(room) {
                        offer(
                            self.room_rank(layer.source()),
                            PlacedRoom {
                                source: layer.source(),
                                number,
                            },
                            Point::new(room.get_x(), room.get_y()),
                        );
                    }
                },
            );
        }
        hit.map(|(_, room, center)| (room, center))
    }

    /// The room a new room at `at` would sit on: one on this level, of any
    /// place, whose box overlaps a room's box there, the nearest first (where
    /// two are as near, as [`Self::link_end_at`] ranks places).
    #[must_use]
    pub(crate) fn room_occupying(&self, at: Point) -> Option<(PlacedRoom, Point)> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        let size = render::MAP_ROOM_SIZE;
        let mut candidates = Vec::new();
        area.with_rooms_in(at.x - size, at.y - size, at.x + size, at.y + size, |room| {
            if room.get_level() == self.level {
                candidates.push((
                    self.room_rank(SourceId::Map),
                    PlacedRoom::map(room.get_room_number()),
                    Point::new(room.get_x(), room.get_y()),
                ));
            }
        });
        for layer in area.source_layers() {
            layer.content().with_rooms_in(
                at.x - size,
                at.y - size,
                at.x + size,
                at.y + size,
                |room| {
                    let number = room.get_room_number();
                    if room.get_level() == self.level {
                        candidates.push((
                            self.room_rank(layer.source()),
                            PlacedRoom {
                                source: layer.source(),
                                number,
                            },
                            Point::new(room.get_x(), room.get_y()),
                        ));
                    }
                },
            );
        }
        nearest_occupant(candidates, at, size)
    }

    /// Where a Link-tool drag from `from` released at `pointer` lands. A
    /// room under the pointer is the far end. Off every room the drop point
    /// snaps to the grid (unless `alt`); with `shift` the link leads nowhere,
    /// else a room is made there, unless a room already occupies that cell,
    /// which is then the far end: no room is ever made on top of another.
    #[must_use]
    pub(crate) fn link_drop(
        &self,
        from: PlacedRoom,
        pointer: Point,
        alt: bool,
        shift: bool,
    ) -> LinkDrop {
        if let Some((room, center)) = self.link_end_at(pointer) {
            return if room == from {
                LinkDrop::Nothing
            } else {
                LinkDrop::Room(room, center)
            };
        }
        let at = if alt {
            pointer
        } else {
            viewport::snap(pointer)
        };
        if shift {
            return LinkDrop::Dangling(at);
        }
        match self.room_occupying(at) {
            Some((room, _)) if room == from => LinkDrop::Nothing,
            Some((room, center)) => LinkDrop::Room(room, center),
            None => LinkDrop::Empty(at),
        }
    }

    /// The room a click at `point` picks while picking: one on this level,
    /// else a ghost on the next levels.
    #[must_use]
    pub(crate) fn picked_room_at(&self, point: Point) -> Option<PlacedRoom> {
        if let Some((room, _)) = self.link_end_at(point) {
            return Some(room);
        }
        match self.ghost_room_at(point)?.0 {
            EntityId::Room(number) => Some(PlacedRoom::map(number)),
            EntityId::SourceRoom(source, number) => Some(PlacedRoom { source, number }),
            _ => None,
        }
    }

    /// Topmost adjacent-level room under a point, the map's or a place's own,
    /// following the exact lower-then-upper level order, and the map's rooms
    /// under each place's, that ghost rendering uses.
    #[must_use]
    fn ghost_room_at(&self, point: Point) -> Option<(EntityId, i32)> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        let half_size = render::MAP_ROOM_SIZE / 2.0;
        let under = |room: &smudgy_cloud::mapper::room_cache::RoomCache, level: i32| {
            room.get_level() == level
                && (room.get_x() - point.x).abs() < half_size
                && (room.get_y() - point.y).abs() < half_size
        };
        let mut hit = None;
        for ghost_level in [self.level - 1, self.level + 1] {
            area.with_rooms_in(
                point.x - half_size,
                point.y - half_size,
                point.x + half_size,
                point.y + half_size,
                |room| {
                    if under(room, ghost_level) {
                        hit = Some((EntityId::Room(room.get_room_number()), ghost_level));
                    }
                },
            );
            for layer in area.source_layers() {
                layer.content().with_rooms_in(
                    point.x - half_size,
                    point.y - half_size,
                    point.x + half_size,
                    point.y + half_size,
                    |room| {
                        let number = room.get_room_number();
                        if under(room, ghost_level) {
                            hit = Some((EntityId::SourceRoom(layer.source(), number), ghost_level));
                        }
                    },
                );
            }
        }
        hit
    }
}

/// The compass direction pointing from one map-space point toward another
/// (map y grows southward). Cardinal capture is intentionally wide; a bearing
/// must be close to a 45-degree diagonal before it produces a diagonal exit.
#[must_use]
pub(crate) fn direction_between(from: Point, to: Point) -> ExitDirection {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let major = dx.abs().max(dy.abs());
    let minor = dx.abs().min(dy.abs());
    if major > 0.0 && minor / major >= smudgy_cloud::DIAGONAL_BEARING_RATIO {
        return match (dx >= 0.0, dy >= 0.0) {
            (true, true) => ExitDirection::Southeast,
            (true, false) => ExitDirection::Northeast,
            (false, true) => ExitDirection::Southwest,
            (false, false) => ExitDirection::Northwest,
        };
    }
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            ExitDirection::East
        } else {
            ExitDirection::West
        }
    } else if dy >= 0.0 {
        ExitDirection::South
    } else {
        ExitDirection::North
    }
}

#[cfg(test)]
mod direction_tests {
    use super::*;

    #[test]
    fn cardinal_capture_is_wider_than_diagonal_capture() {
        let origin = Point::ORIGIN;
        assert_eq!(
            direction_between(origin, Point::new(2.0, 1.0)),
            ExitDirection::East
        );
        assert_eq!(
            direction_between(origin, Point::new(1.0, 1.0)),
            ExitDirection::Southeast
        );
        assert_eq!(
            direction_between(origin, Point::new(-1.0, -1.0)),
            ExitDirection::Northwest
        );
    }
}

#[cfg(test)]
mod legend_tests {
    use smudgy_cloud::ConnectionRouting;

    use super::{EditorActivity, LegendContext, LegendItem, resolve_legend};

    #[test]
    fn dragging_waypoints_takes_precedence_over_selection_context() {
        assert_eq!(
            resolve_legend(
                EditorActivity::DraggingConnectionWaypoint,
                true,
                LegendContext::Connection {
                    routing: ConnectionRouting::Automatic,
                    waypoint_selected: false,
                    port_selected: false,
                },
            ),
            vec![
                LegendItem {
                    key: "Alt",
                    action: "legend-move-freely",
                },
                LegendItem {
                    key: "Esc",
                    action: "legend-cancel",
                },
            ]
        );
    }

    #[test]
    fn idle_connection_hints_follow_routing_and_handle_state() {
        assert_eq!(
            resolve_legend(
                EditorActivity::Idle,
                true,
                LegendContext::Connection {
                    routing: ConnectionRouting::Manual,
                    waypoint_selected: false,
                    port_selected: false,
                },
            ),
            vec![LegendItem {
                key: "Ctrl+click",
                action: "legend-add-point",
            }]
        );
        assert_eq!(
            resolve_legend(
                EditorActivity::Idle,
                true,
                LegendContext::Connection {
                    routing: ConnectionRouting::Manual,
                    waypoint_selected: true,
                    port_selected: false,
                },
            )
            .len(),
            3
        );
        assert!(resolve_legend(EditorActivity::Idle, true, LegendContext::None).is_empty());
        // A selected port advertises its wall-axis nudge; Stub routing has
        // nothing to edit and shows no hint.
        assert_eq!(
            resolve_legend(
                EditorActivity::Idle,
                true,
                LegendContext::Connection {
                    routing: ConnectionRouting::Simple,
                    waypoint_selected: false,
                    port_selected: true,
                },
            )
            .len(),
            3
        );
        assert_eq!(
            resolve_legend(
                EditorActivity::Idle,
                true,
                LegendContext::Connection {
                    routing: ConnectionRouting::Stub,
                    waypoint_selected: false,
                    port_selected: false,
                },
            )
            .len(),
            0
        );
    }
}

#[cfg(test)]
mod secret_tests {
    use std::sync::Arc;

    use iced::{Point, Rectangle};
    use smudgy_cloud::{
        AreaId, Connection, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionKind,
        ConnectionRouting, CornerStyle, Exit, ExitDirection, ExitId, Mapper, PortMode, RoomNumber,
        RoomSide, RoomWithDetails, SegmentShape, SourceBundle, SourceId, Uuid,
    };

    use super::{EntityId, LinkDrop, MapEditor, PlacedRoom};

    /// Serves one map and refuses every write.
    struct OneMap(smudgy_cloud::AreaWithDetails);

    #[async_trait::async_trait]
    impl smudgy_cloud::MapperBackend for OneMap {
        async fn create_area(
            &self,
            _request: smudgy_cloud::CreateAreaRequest,
        ) -> smudgy_cloud::CloudResult<smudgy_cloud::Area> {
            Err(smudgy_cloud::CloudError::NotFoundOrNoAccess)
        }

        async fn list_areas(&self) -> smudgy_cloud::CloudResult<Vec<smudgy_cloud::Area>> {
            Ok(vec![self.0.area.clone()])
        }

        async fn get_area(
            &self,
            _area_id: &AreaId,
        ) -> smudgy_cloud::CloudResult<smudgy_cloud::AreaWithDetails> {
            Ok(self.0.clone())
        }

        async fn update_area(
            &self,
            _area_id: &AreaId,
            _updates: smudgy_cloud::AreaUpdates,
        ) -> smudgy_cloud::CloudResult<()> {
            Ok(())
        }

        async fn delete_area(&self, _area_id: &AreaId) -> smudgy_cloud::CloudResult<()> {
            Ok(())
        }

        async fn execute_mutation(
            &self,
            _area_id: &AreaId,
            _envelope: &smudgy_cloud::mutation::MutationEnvelope,
        ) -> smudgy_cloud::CloudResult<smudgy_cloud::mutation::MutationResult> {
            Err(smudgy_cloud::CloudError::NotFoundOrNoAccess)
        }
    }

    fn room(number: i32, x: f32, y: f32, exits: Vec<Exit>) -> RoomWithDetails {
        RoomWithDetails {
            room_number: RoomNumber(number),
            title: String::new(),
            description: String::new(),
            level: 0,
            x,
            y,
            color: String::new(),
            properties: Vec::new(),
            exits,
            tags: Default::default(),
            external_id: None,
        }
    }

    fn exit(
        map: AreaId,
        secret: SourceId,
        direction: ExitDirection,
        to: i32,
        link: ConnectionId,
    ) -> Exit {
        Exit {
            id: ExitId::new(),
            from_direction: direction,
            to_area_id: Some(map),
            to_room_number: Some(RoomNumber(to)),
            to_direction: Some(direction.opposite()),
            path: String::new(),
            is_hidden: false,
            door: None,
            weight: 1.0,
            command: String::new(),
            connection_id: link,
            to_unknown: false,
            to_area_token: None,
            to_source: Some(secret),
        }
    }

    fn end(number: i32, secret: SourceId, side: RoomSide) -> ConnectionEndpoint {
        ConnectionEndpoint {
            room_number: RoomNumber(number),
            source: Some(secret),
            side,
            port_offset: 0.5,
            port_mode: PortMode::AutoPinned,
        }
    }

    /// Map rooms 1 at (0, 0) and 2 at (4, 0). A Secret's own rooms 3 at
    /// (0, 4) and 4 at (4, 4) are linked both ways; its room 5 sits on the
    /// map's room 2.
    async fn editor() -> (MapEditor, SourceId, ConnectionId) {
        let map = AreaId(Uuid::new_v4());
        let secret = SourceId::Secret(Uuid::new_v4());
        let link = ConnectionId::new();
        let details = smudgy_cloud::AreaWithDetails {
            room_data: Vec::new(),
            area: smudgy_cloud::Area {
                id: map,
                user_id: None,
                atlas_id: None,
                atlas_name: None,
                name: "Library".to_string(),
                created_at: Default::default(),
                rev: 1,
                projection_token: Some("p_editor".to_string()),
                access: None,
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: smudgy_cloud::clan_maps::ClanOwnership::default(),
            },
            format_version: smudgy_cloud::AREA_FORMAT_VERSION,
            properties: Vec::new(),
            rooms: vec![room(1, 0.0, 0.0, Vec::new()), room(2, 4.0, 0.0, Vec::new())],
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
            sources: vec![SourceBundle {
                source: secret,
                name: Some("Bookcase".to_string()),
                ownership: Some("owner".to_string()),
                clan_id: None,
                color: None,
                rev: 1,
                actions: ["read", "add", "edit", "remove"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                properties: Vec::new(),
                rooms: vec![
                    room(
                        3,
                        0.0,
                        4.0,
                        vec![exit(map, secret, ExitDirection::East, 4, link)],
                    ),
                    room(
                        4,
                        4.0,
                        4.0,
                        vec![exit(map, secret, ExitDirection::West, 3, link)],
                    ),
                    room(5, 4.0, 0.0, Vec::new()),
                ],
                room_data: Vec::new(),
                labels: Vec::new(),
                shapes: Vec::new(),
                connections: vec![Connection {
                    id: link,
                    endpoint_a: end(3, secret, RoomSide::East),
                    endpoint_b: Some(end(4, secret, RoomSide::West)),
                    kind: ConnectionKind::Internal,
                    routing: ConnectionRouting::Simple,
                    segment_shape: SegmentShape::Direct,
                    corner: CornerStyle::Sharp,
                    route_points: Vec::new(),
                    dash: ConnectionDash::Solid,
                    color: "#A4A4A4".to_string(),
                    thickness: 1.0,
                }],
            }],
        };
        let cache_dir = std::env::temp_dir()
            .join("smudgy-map-widget-test")
            .join(format!("editor-secret-{}", Uuid::new_v4()));
        let mapper = Mapper::new(Arc::new(OneMap(details)), cache_dir);
        mapper.load_all_areas().await.expect("load the map");
        (MapEditor::new(mapper, Some(map)), secret, link)
    }

    #[tokio::test]
    async fn a_secrets_link_is_hit_and_selected_like_the_maps() {
        let (editor, secret, link) = editor().await;
        assert!(
            editor
                .entities_at(Point::new(2.0, 4.0))
                .contains(&EntityId::Connection(link)),
            "a click on the Secret's link finds it"
        );
        assert!(
            editor
                .entities_in_rect(Rectangle::new(
                    Point::new(1.5, 3.5),
                    iced::Size::new(1.0, 1.0)
                ))
                .contains(&EntityId::Connection(link)),
            "a rubber band finds it"
        );
        let atlas = editor.mapper.get_current_atlas();
        let area = atlas
            .get_area(&editor.area_id().expect("a map"))
            .expect("loaded");
        let drawn = editor
            .drawn_connection(&area, link)
            .expect("its selection outline has a line to follow");
        assert_eq!(
            Some(drawn.color),
            crate::sources::layer_color(&area, secret),
            "in the Secret's color"
        );
    }

    #[tokio::test]
    async fn link_ends_reach_secret_rooms_and_overlaps_favor_add_to() {
        let (mut editor, secret, _link) = editor().await;
        assert_eq!(
            editor
                .link_end_at(Point::new(0.0, 4.0))
                .map(|(room, _)| room),
            Some(PlacedRoom {
                source: secret,
                number: RoomNumber(3)
            }),
            "a Secret's room is a link end"
        );
        // The map's room 2 and the Secret's room 5 overlap.
        assert_eq!(
            editor
                .link_end_at(Point::new(4.0, 0.0))
                .map(|(room, _)| room),
            Some(PlacedRoom::map(RoomNumber(2)))
        );
        assert_eq!(
            editor.entities_at(Point::new(4.0, 0.0)).first(),
            Some(&EntityId::Room(RoomNumber(2)))
        );
        editor.set_add_to(secret);
        assert_eq!(
            editor
                .link_end_at(Point::new(4.0, 0.0))
                .map(|(room, _)| room),
            Some(PlacedRoom {
                source: secret,
                number: RoomNumber(5)
            }),
            "\"Add to\"'s room wins the overlap"
        );
        assert_eq!(
            editor.entities_at(Point::new(4.0, 0.0)).first(),
            Some(&EntityId::SourceRoom(secret, RoomNumber(5)))
        );
    }

    #[tokio::test]
    async fn a_link_selected_from_a_secret_room_takes_it_as_its_from_end() {
        let (mut editor, secret, link) = editor().await;
        editor.select(EntityId::SourceRoom(secret, RoomNumber(4)));
        let _ = editor.update(super::Message::ClickSelect {
            entity: EntityId::Connection(link),
            additive: false,
        });
        assert_eq!(
            editor.connection_anchor(),
            Some(PlacedRoom::new(secret, RoomNumber(4)))
        );
    }

    /// A Link-tool drop just outside a room's box snaps onto that room's
    /// cell: it links to the room there instead of making a room on top of
    /// it, whatever place the room is in.
    #[tokio::test]
    async fn a_drop_on_an_occupied_cell_links_to_the_room_there() {
        let (mut editor, secret, _link) = editor().await;
        let from = PlacedRoom::map(RoomNumber(2));
        assert_eq!(
            editor.link_drop(from, Point::new(0.3, 0.3), false, false),
            LinkDrop::Room(PlacedRoom::map(RoomNumber(1)), Point::new(0.0, 0.0)),
            "the map's room 1 holds the cell"
        );
        assert_eq!(
            editor.link_drop(from, Point::new(0.3, 4.4), false, false),
            LinkDrop::Room(
                PlacedRoom {
                    source: secret,
                    number: RoomNumber(3)
                },
                Point::new(0.0, 4.0)
            ),
            "a Secret's room holds a cell too"
        );
        assert_eq!(
            editor.link_drop(from, Point::new(2.2, 1.8), false, false),
            LinkDrop::Empty(Point::new(2.0, 2.0)),
            "an empty cell takes a new room"
        );
        assert_eq!(
            editor.link_drop(from, Point::new(0.4, 0.0), true, false),
            LinkDrop::Room(PlacedRoom::map(RoomNumber(1)), Point::new(0.0, 0.0)),
            "unsnapped, a new room's box would still overlap room 1"
        );
        assert_eq!(
            editor.link_drop(from, Point::new(1.0, 0.0), true, false),
            LinkDrop::Empty(Point::new(1.0, 0.0))
        );
        assert_eq!(
            editor.link_drop(from, Point::new(0.3, 0.3), false, true),
            LinkDrop::Dangling(Point::new(0.0, 0.0)),
            "Shift makes no room, so the cell doesn't matter"
        );
        assert_eq!(
            editor.link_drop(from, Point::new(4.35, 0.3), false, false),
            LinkDrop::Nothing,
            "back onto the room it started from"
        );
        // The Secret's room 5 shares the map's room 2's cell; "Add to"
        // decides which one a drop from room 1 meets.
        let from = PlacedRoom::map(RoomNumber(1));
        assert_eq!(
            editor.link_drop(from, Point::new(4.3, 0.3), false, false),
            LinkDrop::Room(PlacedRoom::map(RoomNumber(2)), Point::new(4.0, 0.0))
        );
        editor.set_add_to(secret);
        assert_eq!(
            editor.link_drop(from, Point::new(4.3, 0.3), false, false),
            LinkDrop::Room(
                PlacedRoom {
                    source: secret,
                    number: RoomNumber(5)
                },
                Point::new(4.0, 0.0)
            )
        );
    }

    /// The room tool never stacks rooms either: a click on an occupied
    /// cell selects the room there.
    #[tokio::test]
    async fn placing_a_room_on_an_occupied_cell_selects_the_room_there() {
        let (mut editor, secret, _link) = editor().await;
        let update = editor.update(super::Message::PlaceRoom {
            at: Point::new(0.0, 4.0),
            keep_tool: false,
        });
        assert!(matches!(update.event, Some(super::Event::SelectionChanged)));
        assert!(
            editor
                .selection()
                .contains(EntityId::SourceRoom(secret, RoomNumber(3)))
        );
        let update = editor.update(super::Message::PlaceRoom {
            at: Point::new(2.0, 2.0),
            keep_tool: false,
        });
        assert!(matches!(
            update.event,
            Some(super::Event::RequestMutation(
                super::MutationRequest::PlaceRoom { .. }
            ))
        ));
    }

    /// While picking, a click finds a room on this level or a ghost on the
    /// next, and reports it without touching the selection.
    #[tokio::test]
    async fn picking_reports_the_room_and_keeps_the_selection() {
        let (mut editor, secret, _link) = editor().await;
        editor.select(EntityId::Room(RoomNumber(1)));
        editor.set_picking(true);
        assert_eq!(
            editor.picked_room_at(Point::new(0.0, 4.0)),
            Some(PlacedRoom {
                source: secret,
                number: RoomNumber(3)
            })
        );
        editor.set_level(1);
        editor.select(EntityId::Room(RoomNumber(1)));
        assert_eq!(
            editor.picked_room_at(Point::new(0.0, 0.0)),
            Some(PlacedRoom::map(RoomNumber(1))),
            "a ghost on the level below"
        );
        assert_eq!(editor.picked_room_at(Point::new(2.0, 2.0)), None);
        let update = editor.update(super::Message::RoomPicked(PlacedRoom::map(RoomNumber(2))));
        assert!(matches!(
            update.event,
            Some(super::Event::RoomPicked(room)) if room == PlacedRoom::map(RoomNumber(2))
        ));
        assert!(editor.selection().contains(EntityId::Room(RoomNumber(1))));
    }

    /// The map opens centered on every room the viewer reads: the map's on
    /// the top row and the Secret's below them.
    #[tokio::test]
    async fn a_map_opens_centered_on_its_secrets_rooms_too() {
        let (editor, _secret, _link) = editor().await;
        assert_eq!(editor.center_of_area(), Some(Point::new(2.0, 2.0)));
    }

    /// One level up, the map's rooms and the Secret's are ghosts below; a
    /// click on the Secret's ghost room selects it on its level.
    #[tokio::test]
    async fn a_secrets_ghost_room_is_clicked_like_the_maps() {
        let (mut editor, secret, _link) = editor().await;
        editor.set_level(1);
        assert_eq!(
            editor.ghost_room_at(Point::new(0.0, 0.0)),
            Some((EntityId::Room(RoomNumber(1)), 0))
        );
        let ghost = editor.ghost_room_at(Point::new(0.0, 4.0));
        assert_eq!(
            ghost,
            Some((EntityId::SourceRoom(secret, RoomNumber(3)), 0))
        );
        let (room, level) = ghost.expect("a ghost room");
        let _ = editor.update(super::Message::GhostRoomSelected { room, level });
        assert_eq!(editor.level(), 0);
        assert!(
            editor
                .selection()
                .contains(EntityId::SourceRoom(secret, RoomNumber(3)))
        );
    }
}

/// Of `candidates` (rank, room, center), the one whose box a room box at
/// `at` would overlap, nearest first, then by rank (lower wins).
fn nearest_occupant(
    candidates: Vec<(u8, PlacedRoom, Point)>,
    at: Point,
    size: f32,
) -> Option<(PlacedRoom, Point)> {
    candidates
        .into_iter()
        .filter(|(_, _, center)| (center.x - at.x).abs() < size && (center.y - at.y).abs() < size)
        .min_by(|a, b| {
            let distance = |center: Point| (center.x - at.x).hypot(center.y - at.y);
            distance(a.2).total_cmp(&distance(b.2)).then(a.0.cmp(&b.0))
        })
        .map(|(_, room, center)| (room, center))
}

fn rect_contains(x: f32, y: f32, width: f32, height: f32, point: Point) -> bool {
    point.x >= x && point.x <= x + width && point.y >= y && point.y <= y + height
}

fn rects_intersect(rect: Rectangle, x: f32, y: f32, width: f32, height: f32) -> bool {
    rect.x < x + width && x < rect.x + rect.width && rect.y < y + height && y < rect.y + rect.height
}
