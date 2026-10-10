//! The inspector pane: editable forms for the current selection, and the
//! map panel's data fields. Which panel the pane shows is
//! [`super::panels::shown`]'s to say.
//!
//! Field buffers live in [`State`] so the user can type freely (including
//! transiently invalid numbers); every valid change commits immediately
//! through the window's command stack with per-field coalescing, so a
//! typing burst is one undo step. Buffers resync from the cache when the
//! selection changes, after undo/redo, and when another writer bumps the
//! area revision — but not on the echo of the user's own commits.

use std::collections::HashSet;
use std::fmt;

use iced::widget::{
    Column, button, column, container, pick_list, row, rule, scrollable, space, text, text_editor,
    text_input,
};
use iced::{Length, Padding, Task, alignment::Vertical};
use smudgy_cloud::mapper::RoomKey;
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{
    AreaId, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionRouting, ConnectionUpdates,
    CornerStyle, DEFAULT_CONNECTION_COLOR, DEFAULT_CONNECTION_THICKNESS, ExitDirection,
    HorizontalAlignment, LabelId, LabelUpdates, Mapper, RoomNumber, RoomSide, RoomUpdates,
    SegmentShape, ShapeId, ShapeType, ShapeUpdates, SourceId, VerticalAlignment,
};
use smudgy_map_widget::map_editor::{EntityId, MapEditor};
use smudgy_map_widget::render::parse_color;

use crate::assets::{bootstrap_icons, fonts};
use crate::components::color_picker::{self, ColorPicker};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;
use crate::widgets::wrap_row::wrap_row;

use super::commands::{ExitRef, FieldId};
use super::document::Document;
use super::panels::Panel;
use super::source_rooms::{self, SourceRoomRef};
use super::{MapEditorWindow, commands};

const FIELD_SPACING: f32 = 10.0;

/// Builds a port edit and, for active orthogonal routes, adjusts or inserts
/// the endpoint-adjacent stored elbow in the same mutation. Canvas dragging,
/// inspector entry, and keyboard nudging must all preserve the same stored
/// geometry invariant; the renderer never repairs a diagonal leg.
pub(super) fn endpoint_updates(
    area: &AreaCache,
    connection_id: ConnectionId,
    endpoint: ConnectionEndpoint,
    endpoint_b: bool,
) -> Option<ConnectionUpdates> {
    let connection = area.get_connection(connection_id)?;
    let mut route_points = None;
    // Without endpoint B there is no active tip-to-tip route to repair —
    // dormant stored points stay untouched and the endpoint simply moves.
    if connection.segment_shape == SegmentShape::Orthogonal
        && matches!(
            connection.routing,
            ConnectionRouting::Manual | ConnectionRouting::Automatic
        )
        && connection.endpoint_b.is_some()
    {
        let render = area.get_room_connections().iter().find(|item| {
            item.connection_id == connection_id && item.geometry.stub_tip_b.is_some()
        })?;
        let room = area.get_room_at(endpoint.address())?;
        let new_tip = smudgy_cloud::connection_geometry::stub_tip(
            smudgy_cloud::connection_geometry::port_position(
                smudgy_cloud::MapPoint::new(room.get_x(), room.get_y()),
                endpoint.side,
                endpoint.port_offset,
            ),
            endpoint.side,
        );
        let (old_tip, other_tip) = if endpoint_b {
            (
                render.geometry.stub_tip_b?,
                Some(render.geometry.stub_tip_a),
            )
        } else {
            (render.geometry.stub_tip_a, render.geometry.stub_tip_b)
        };
        route_points = Some(smudgy_cloud::connection_geometry::reroute_for_port_move(
            &connection.route_points,
            old_tip,
            other_tip,
            new_tip,
            endpoint_b,
        ));
    }

    Some(if endpoint_b {
        ConnectionUpdates {
            endpoint_b: Some(endpoint),
            route_points,
            ..ConnectionUpdates::default()
        }
    } else {
        ConnectionUpdates {
            endpoint_a: Some(endpoint),
            route_points,
            ..ConnectionUpdates::default()
        }
    })
}

/// The endpoint edit that keeps `connection`'s port at `room` agreeing with
/// a changed exit direction: re-anchored to the direction's home slot in
/// `AutoPinned` mode, with the orthogonal elbow repair folded in. `None`
/// when the connection has no endpoint at `room`, the connection is a
/// self-loop (whose arc ignores ports), or the endpoint already sits home.
pub(super) fn endpoint_reanchor(
    area: &AreaCache,
    connection: &smudgy_cloud::Connection,
    room: smudgy_cloud::RoomAddress,
    direction: ExitDirection,
) -> Option<ConnectionUpdates> {
    if connection.kind == smudgy_cloud::ConnectionKind::SelfLoop {
        return None;
    }
    let endpoint_b = if connection.endpoint_a.address() == room {
        false
    } else if connection
        .endpoint_b
        .is_some_and(|endpoint| endpoint.address() == room)
    {
        true
    } else {
        return None;
    };
    let mut endpoint = if endpoint_b {
        connection.endpoint_b?
    } else {
        connection.endpoint_a
    };
    // `Special`/`Other` have no compass semantics; give them the partner
    // bearing so the anchor lands toward the destination, exactly as
    // connection creation does.
    let bearing = (|| {
        let this = area.get_room_at(room)?;
        let other_number = if endpoint_b {
            connection.endpoint_a.address()
        } else {
            connection.endpoint_b?.address()
        };
        let other = area.get_room_at(other_number)?;
        Some(smudgy_cloud::MapPoint::new(
            other.get_x() - this.get_x(),
            other.get_y() - this.get_y(),
        ))
    })();
    let (side, offset) = smudgy_cloud::default_anchor_for_direction(direction, bearing);
    if endpoint.side == side
        && (endpoint.port_offset - offset).abs() < smudgy_cloud::connection_geometry::EPSILON
        && endpoint.port_mode == smudgy_cloud::PortMode::AutoPinned
    {
        return None;
    }
    endpoint.side = side;
    endpoint.port_offset = offset;
    endpoint.port_mode = smudgy_cloud::PortMode::AutoPinned;
    endpoint_updates(area, connection.id, endpoint, endpoint_b)
}

#[derive(Clone, Copy)]
struct WallEndpoint {
    connection_id: ConnectionId,
    endpoint_b: bool,
    endpoint: ConnectionEndpoint,
    bearing: f32,
}

fn wall_axis_is_x(side: RoomSide) -> bool {
    matches!(side, RoomSide::North | RoomSide::South)
}

fn direction_component(direction: ExitDirection, axis_x: bool) -> f32 {
    const DIAG: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let (x, y) = match direction {
        ExitDirection::North => (0.0, -1.0),
        ExitDirection::East => (1.0, 0.0),
        ExitDirection::South => (0.0, 1.0),
        ExitDirection::West => (-1.0, 0.0),
        ExitDirection::Northeast => (DIAG, -DIAG),
        ExitDirection::Southeast => (DIAG, DIAG),
        ExitDirection::Southwest => (-DIAG, DIAG),
        ExitDirection::Northwest => (-DIAG, -DIAG),
        _ => (0.0, 0.0),
    };
    if axis_x { x } else { y }
}

fn endpoint_bearing(
    area: &AreaCache,
    connection: &smudgy_cloud::Connection,
    endpoint_b: bool,
) -> f32 {
    let endpoint = if endpoint_b {
        let Some(endpoint) = connection.endpoint_b else {
            return 0.0;
        };
        endpoint
    } else {
        connection.endpoint_a
    };
    let other = if endpoint_b {
        Some(connection.endpoint_a)
    } else {
        connection.endpoint_b
    };
    let axis_x = wall_axis_is_x(endpoint.side);
    if let Some(other) = other {
        if other.address() == endpoint.address() {
            let outward = other.side.outward();
            return if axis_x { outward.x } else { outward.y };
        }
        if let (Some(room), Some(partner)) = (
            area.get_room_at(endpoint.address()),
            area.get_room_at(other.address()),
        ) {
            return if axis_x {
                partner.get_x() - room.get_x()
            } else {
                partner.get_y() - room.get_y()
            };
        }
    }
    area.get_room_at(endpoint.address())
        .and_then(|room| {
            room.get_exits()
                .iter()
                .filter(|exit| exit.connection_id == connection.id)
                .min_by_key(|exit| exit.from_direction.to_string())
        })
        .map_or(0.0, |exit| direction_component(exit.from_direction, axis_x))
}

/// Computes the deterministic preview/commit payload for an explicit wall
/// redistribution — the author-invoked fan-out for walls where several
/// pinned ports coincide, and deliberately the ONLY port redistribution
/// anywhere: creation, migration, and retargeting all pin ports at their
/// direction's semantic default. Manual endpoints remain fixed. AutoPinned
/// endpoints use their rank in the full bearing-ordered group, preserving
/// stable UUID/role tie-breaks.
pub(super) fn redistribute_port_updates(
    area: &AreaCache,
    room: smudgy_cloud::RoomAddress,
    side: RoomSide,
) -> Vec<(ConnectionId, ConnectionUpdates)> {
    let mut group = Vec::new();
    for connection in area.get_connections() {
        for (endpoint_b, endpoint) in [
            (false, Some(connection.endpoint_a)),
            (true, connection.endpoint_b),
        ] {
            let Some(endpoint) = endpoint else { continue };
            if endpoint.address() == room && endpoint.side == side {
                group.push(WallEndpoint {
                    connection_id: connection.id,
                    endpoint_b,
                    endpoint,
                    bearing: endpoint_bearing(area, connection, endpoint_b),
                });
            }
        }
    }
    group.sort_by(|a, b| {
        a.bearing
            .total_cmp(&b.bearing)
            .then(a.connection_id.cmp(&b.connection_id))
            .then(a.endpoint_b.cmp(&b.endpoint_b))
    });
    let group_len = group.len();
    #[allow(clippy::cast_precision_loss)]
    let denominator = group_len.saturating_sub(1) as f32;
    group
        .into_iter()
        .enumerate()
        .filter(|(_, item)| item.endpoint.port_mode == smudgy_cloud::PortMode::AutoPinned)
        .filter_map(|(slot, mut item)| {
            #[allow(clippy::cast_precision_loss)]
            let offset = if group_len == 1 {
                item.endpoint.port_offset
            } else {
                smudgy_cloud::CORNER_INSET
                    + slot as f32 * (1.0 - 2.0 * smudgy_cloud::CORNER_INSET) / denominator
            };
            if (item.endpoint.port_offset - offset).abs() <= f32::EPSILON {
                return None;
            }
            item.endpoint.port_offset = offset;
            endpoint_updates(area, item.connection_id, item.endpoint, item.endpoint_b)
                .map(|updates| (item.connection_id, updates))
        })
        .collect()
}

#[derive(Debug, Clone)]
pub enum Message {
    TitleChanged(String),
    DescriptionEdited(text_editor::Action),
    LevelChanged(String),
    XChanged(String),
    YChanged(String),
    ColorChanged(String),
    PropertyValueChanged(usize, String),
    /// A Secret's or Private's data on the selected map room.
    PlaceValueChanged(SourceId, usize, String),
    PlacePropertyDeleted(SourceId, usize),
    PlaceNewNameChanged(SourceId, String),
    PlaceNewValueChanged(SourceId, String),
    PlacePropertyAdded(SourceId),
    PlaceMenuToggled(bool),
    PlaceStarted(SourceId),
    PropertyDeleted(usize),
    NewPropertyNameChanged(String),
    NewPropertyValueChanged(String),
    AddProperty,
    /// The tag input of the selected rooms' Tags block.
    TagInputChanged(String),
    /// Enter in the tag input: add the typed tag where the input writes.
    TagSubmitted,
    /// A suggestion under the tag input: add it where the input writes.
    TagSuggestionPicked(String),
    /// A chip's ×: remove the tag from that place on the selected rooms.
    TagRemoved(SourceId, String),
    /// Tab: whether the tag input had focus, to complete it.
    TagCompleted(bool),
    BulkColorChanged(String),
    BulkLevelChanged(String),
    ApplyBulkColor,
    ApplyBulkLevel,
    AreaPropertyValueChanged(usize, String),
    AreaPropertyDeleted(usize),
    NewAreaPropertyNameChanged(String),
    NewAreaPropertyValueChanged(String),
    AddAreaProperty,
    ConnectionRoutingChanged(ConnectionRouting),
    ConnectionSegmentShapeChanged(SegmentShape),
    ConnectionCornerChanged(CornerStyle),
    ConnectionDashChanged(ConnectionDash),
    ConnectionColorChanged(String),
    /// The thickness slider was let go at this width.
    ConnectionThicknessPicked(f32),
    ConnectionEndpointSideChanged(bool, RoomSide),
    ConnectionEndpointOffsetChanged(bool, String),
    ConnectionEndpointReset(bool),
    ConnectionRedistributePorts(bool),
    ConnectionClearRoute,
    ConnectionReroute,
    ConnectionReset,
    LabelTextChanged(String),
    LabelColorChanged(String),
    LabelBackgroundChanged(String),
    LabelFontSizeChanged(String),
    LabelFontWeightChanged(String),
    LabelHorizontalAlignmentChanged(HorizontalAlignment),
    LabelVerticalAlignmentChanged(VerticalAlignment),
    LabelBoundsChanged(BoundsField, String),
    ShapeTypeChanged(ShapeType),
    ShapeBackgroundChanged(String),
    ShapeStrokeColorChanged(String),
    ShapeStrokeWidthChanged(String),
    ShapeBorderRadiusChanged(String),
    ShapeBoundsChanged(BoundsField, String),
    PickerToggled(ColorField),
    Picker(color_picker::Message),
}

/// One of the four bounds fields shared by labels and shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundsField {
    X,
    Y,
    Width,
    Height,
}

/// Which color field an open picker edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorField {
    Room,
    Bulk,
    LabelText,
    LabelBackground,
    ShapeFill,
    ShapeStroke,
    Connection,
}

#[derive(Debug, Clone, Default)]
struct PropertyRow {
    name: String,
    value: String,
}

#[derive(Debug, Clone, Default)]
struct LabelBuffers {
    text: String,
    color: String,
    background: String,
    font_size: String,
    font_weight: String,
    horizontal_alignment: HorizontalAlignment,
    vertical_alignment: VerticalAlignment,
    x: String,
    y: String,
    width: String,
    height: String,
}

#[derive(Debug, Clone, Default)]
struct ShapeBuffers {
    shape_type: ShapeType,
    background: String,
    stroke_color: String,
    stroke_width: String,
    border_radius: String,
    x: String,
    y: String,
    width: String,
    height: String,
}

#[derive(Debug, Clone)]
struct ConnectionBuffers {
    routing: ConnectionRouting,
    segment_shape: SegmentShape,
    corner: CornerStyle,
    dash: ConnectionDash,
    color: String,
    thickness: String,
    endpoint_a_side: RoomSide,
    endpoint_a_offset: String,
    endpoint_b_side: RoomSide,
    endpoint_b_offset: String,
    has_endpoint_b: bool,
}

impl Default for ConnectionBuffers {
    fn default() -> Self {
        Self {
            routing: ConnectionRouting::Simple,
            segment_shape: SegmentShape::Direct,
            corner: CornerStyle::Sharp,
            dash: ConnectionDash::Solid,
            color: DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: DEFAULT_CONNECTION_THICKNESS.to_string(),
            endpoint_a_side: RoomSide::East,
            endpoint_a_offset: "0.5".to_string(),
            endpoint_b_side: RoomSide::West,
            endpoint_b_offset: "0.5".to_string(),
            has_endpoint_b: false,
        }
    }
}

/// What one Secret or Private keeps for the selected map room, with its
/// add-row buffers.
#[derive(Debug, Clone)]
struct PlaceData {
    source: SourceId,
    name: String,
    has_data: bool,
    writable: bool,
    properties: Vec<PropertyRow>,
    new_name: String,
    new_value: String,
}

/// Every Secret's and Private's properties on map room `map_room`, in color
/// order (their tags show in the Tags block, their links in the room's
/// Exits); a place keeping data there (properties, tags or exits) shows its
/// group. A place that keeps nothing there is listed empty, ready to add to.
fn place_data(area: &AreaCache, room_source: SourceId, map_room: RoomNumber) -> Vec<PlaceData> {
    let mut sources: Vec<SourceId> = super::secrets::secrets(area)
        .iter()
        .map(|bundle| bundle.source)
        .collect();
    sources.push(SourceId::Private);
    sources.insert(0, SourceId::Map);
    sources
        .into_iter()
        .filter(|source| *source != room_source)
        .map(|source| {
            let attachment = area.document_layer(source).and_then(|layer| {
                layer.attachment(smudgy_cloud::RoomAddress::new(room_source, map_room))
            });
            let (properties, has_exits) = match attachment {
                Some(room) => (
                    sorted_properties(room.properties()),
                    !room.get_exits().is_empty(),
                ),
                None => (Vec::new(), false),
            };
            PlaceData {
                source,
                name: super::secrets::place_name(area, source),
                has_data: !properties.is_empty() || has_exits,
                writable: super::secrets::can_write(area, source),
                properties,
                new_name: String::new(),
                new_value: String::new(),
            }
        })
        .collect()
}

/// Inspector field buffers, rebuilt by [`State::resync`].
#[derive(Debug, Clone, Default)]
pub struct State {
    title: String,
    description: text_editor::Content,
    level: String,
    x: String,
    y: String,
    color: String,
    properties: Vec<PropertyRow>,
    new_property_name: String,
    new_property_value: String,
    /// The selected rooms' tags, place by place.
    tags: super::tags::SelectionTags,
    /// The tag input.
    tag_input: String,
    /// The selection the tag input was typed for: a rebuild keeps the
    /// input while the selection holds.
    tag_selection: Option<SelectionKey>,
    /// The place the tag input was typed for. The input empties when the
    /// place it would write changes, so a tag never lands somewhere other
    /// than the place shown while it was typed.
    tag_place: Option<SourceId>,
    /// What each Secret and Private keeps for the selected map room.
    places: Vec<PlaceData>,
    /// The link editor's state for the selected room or link.
    pub links: super::link_panel::LinkPanel,
    label: LabelBuffers,
    shape: ShapeBuffers,
    connection: ConnectionBuffers,
    bulk_color: String,
    bulk_level: String,
    /// The selected rooms disagree on color/level, so the bulk fields show
    /// "(mixed)" instead of a misleading value.
    bulk_color_mixed: bool,
    bulk_level_mixed: bool,
    area_properties: Vec<PropertyRow>,
    new_area_property_name: String,
    new_area_property_value: String,
    /// The open color picker, if any, and the field it edits.
    picker: Option<(ColorField, ColorPicker)>,
}

impl State {
    /// The text buffer backing a color field.
    fn color_buffer(&self, field: ColorField) -> &str {
        match field {
            ColorField::Room => &self.color,
            ColorField::Bulk => &self.bulk_color,
            ColorField::LabelText => &self.label.color,
            ColorField::LabelBackground => &self.label.background,
            ColorField::ShapeFill => &self.shape.background,
            ColorField::ShapeStroke => &self.shape.stroke_color,
            ColorField::Connection => &self.connection.color,
        }
    }

    /// Empties the tag input.
    pub fn clear_tag_input(&mut self) {
        self.tag_input.clear();
        self.tag_place = None;
    }

    /// Reads the selected rooms' tags again, leaving every other buffer
    /// (and whatever is being typed in it) as it is.
    pub fn reread_tags(&mut self, mapper: &Mapper, editor: &MapEditor) {
        let atlas = mapper.get_current_atlas();
        let Some(area) = editor.area_id().and_then(|id| atlas.get_area(&id)) else {
            return;
        };
        let selection = editor.selection();
        self.tags =
            super::tags::SelectionTags::read(&area, selection.rooms(), selection.source_rooms());
    }
}

/// A selection as the tag input remembers it.
type SelectionKey = (Option<AreaId>, HashSet<EntityId>);

fn selection_key(editor: &MapEditor) -> SelectionKey {
    (editor.area_id(), editor.selection().iter().collect())
}

impl State {
    /// Rebuilds every buffer from the current cache snapshot. The tag input
    /// stays while the selection is the one it was typed for.
    pub fn resync(&mut self, mapper: &Mapper, editor: &MapEditor) {
        let key = selection_key(editor);
        let tag_input = std::mem::take(&mut self.tag_input);
        let tag_place = self.tag_place.take();
        let kept = self.tag_selection.take() == Some(key.clone());
        let mut links = std::mem::take(&mut self.links);
        *self = Self::default();
        if kept {
            self.tag_input = tag_input;
            self.tag_place = tag_place;
        }
        self.tag_selection = Some(key);

        let atlas = mapper.get_current_atlas();
        let Some(area) = editor.area_id().and_then(|id| atlas.get_area(&id)) else {
            links.resync((editor.area_id(), editor.selection().single()), None);
            self.links = links;
            return;
        };
        let link = match editor.selection().single() {
            Some(EntityId::Connection(id)) => {
                super::links::link_view(&atlas, &area, id, editor.connection_anchor())
            }
            _ => None,
        };
        links.resync(
            (editor.area_id(), editor.selection().single()),
            link.as_ref(),
        );
        self.links = links;

        match editor.selection().single() {
            Some(EntityId::Room(room_number)) => {
                if let Some(room) = area.get_room(&room_number) {
                    self.title = room.get_title().to_string();
                    self.description = text_editor::Content::with_text(room.get_description());
                    self.level = room.get_level().to_string();
                    self.x = room.get_x().to_string();
                    self.y = room.get_y().to_string();
                    self.color = room.get_color().to_string();
                    self.properties = sorted_properties(room.properties());
                    self.tags = super::tags::SelectionTags::read(&area, [room_number], []);
                    self.places = place_data(&area, SourceId::Map, room_number);
                }
            }
            Some(EntityId::SourceRoom(source, room_number)) => {
                if let Some(room) =
                    smudgy_map_widget::sources::source_room(&area, source, room_number)
                {
                    self.title = room.get_title().to_string();
                    self.description = text_editor::Content::with_text(room.get_description());
                    self.level = room.get_level().to_string();
                    self.x = room.get_x().to_string();
                    self.y = room.get_y().to_string();
                    self.color = room.get_color().to_string();
                    self.properties = sorted_properties(room.properties());
                    self.tags =
                        super::tags::SelectionTags::read(&area, [], [(source, room_number)]);
                    self.places = place_data(&area, source, room_number);
                }
            }
            Some(EntityId::Label(label_id)) => {
                if let Some((_, label)) = area.find_label(&label_id) {
                    self.label = LabelBuffers {
                        text: label.text.clone(),
                        color: label.color.clone(),
                        background: label.background_color.clone(),
                        font_size: label.font_size.to_string(),
                        font_weight: label.font_weight.to_string(),
                        horizontal_alignment: label.horizontal_alignment.clone(),
                        vertical_alignment: label.vertical_alignment.clone(),
                        x: label.x.to_string(),
                        y: label.y.to_string(),
                        width: label.width.to_string(),
                        height: label.height.to_string(),
                    };
                }
            }
            Some(EntityId::Shape(shape_id)) => {
                if let Some((_, shape)) = area.find_shape(&shape_id) {
                    self.shape = ShapeBuffers {
                        shape_type: shape.shape_type.clone(),
                        background: shape.background_color.clone().unwrap_or_default(),
                        stroke_color: shape.stroke_color.clone().unwrap_or_default(),
                        stroke_width: shape.stroke_width.to_string(),
                        border_radius: shape.border_radius.to_string(),
                        x: shape.x.to_string(),
                        y: shape.y.to_string(),
                        width: shape.width.to_string(),
                        height: shape.height.to_string(),
                    };
                }
            }
            Some(EntityId::Connection(connection_id)) => {
                // A Secret's link reads from its Secret's document.
                let document = Document::of_connection(&area, connection_id);
                if let Some(document) = document
                    && let Some(connection) = document.content().get_connection(connection_id)
                {
                    let endpoint_b = connection.endpoint_b;
                    self.connection = ConnectionBuffers {
                        routing: connection.routing,
                        segment_shape: connection.segment_shape,
                        corner: connection.corner,
                        dash: connection.dash,
                        color: connection.color.clone(),
                        thickness: connection.thickness.to_string(),
                        endpoint_a_side: connection.endpoint_a.side,
                        endpoint_a_offset: connection.endpoint_a.port_offset.to_string(),
                        endpoint_b_side: endpoint_b
                            .map_or(RoomSide::West, |endpoint| endpoint.side),
                        endpoint_b_offset: endpoint_b.map_or_else(
                            || "0.5".to_string(),
                            |endpoint| endpoint.port_offset.to_string(),
                        ),
                        has_endpoint_b: endpoint_b.is_some(),
                    };
                }
            }
            None => {
                if editor.selection().is_empty() {
                    self.area_properties = sorted_properties(area.properties());
                } else {
                    self.tags = super::tags::SelectionTags::read(
                        &area,
                        editor.selection().rooms(),
                        editor.selection().source_rooms(),
                    );
                    // Multi-selection: prefill the bulk fields with values
                    // the rooms agree on, whatever place holds them;
                    // disagreements show "(mixed)".
                    let selection = editor.selection();
                    let rooms: Vec<_> = selection
                        .rooms()
                        .filter_map(|number| area.get_room(&number))
                        .chain(selection.source_rooms().filter_map(|(source, number)| {
                            smudgy_map_widget::sources::source_room(&area, source, number)
                        }))
                        .collect();

                    if let Some(first) = rooms.first() {
                        if rooms
                            .iter()
                            .all(|room| room.get_color() == first.get_color())
                        {
                            self.bulk_color = first.get_color().to_string();
                        } else {
                            self.bulk_color_mixed = true;
                        }

                        if rooms
                            .iter()
                            .all(|room| room.get_level() == first.get_level())
                        {
                            self.bulk_level = first.get_level().to_string();
                        } else {
                            self.bulk_level_mixed = true;
                        }
                    }
                }
            }
        }
    }
}

fn sorted_properties<'a>(properties: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<PropertyRow> {
    let mut rows: Vec<PropertyRow> = properties
        .map(|(name, value)| PropertyRow {
            name: name.to_string(),
            value: value.to_string(),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

impl MapEditorWindow {
    fn selected_room_key(&self) -> Option<RoomKey> {
        match self.editor.selection().single() {
            Some(EntityId::Room(room_number)) => Some(RoomKey {
                area_id: self.editor.area_id()?,
                room_number,
            }),
            _ => None,
        }
    }

    /// `source`'s data on the selected, readable room, wherever that room lives.
    fn place_target(&self, source: SourceId) -> Option<SourceRoomRef> {
        let (room_source, number) = match self.editor.selection().single()? {
            EntityId::Room(number) => (SourceId::Map, number),
            EntityId::SourceRoom(anchor, number) => (anchor, number),
            _ => return None,
        };
        Some(SourceRoomRef::data_on_source(
            self.editor.area_id()?,
            source,
            room_source,
            number,
        ))
    }

    fn place_mut(&mut self, source: SourceId) -> Option<&mut PlaceData> {
        self.inspector
            .places
            .iter_mut()
            .find(|place| place.source == source)
    }

    fn selected_source_room(&self) -> Option<SourceRoomRef> {
        match self.editor.selection().single() {
            Some(EntityId::SourceRoom(source, room_number)) => Some(SourceRoomRef::own(
                self.editor.area_id()?,
                source,
                room_number,
            )),
            _ => None,
        }
    }

    /// The open map's tags, place by place, read once per revision.
    pub(super) fn tag_index(
        &self,
        area: &std::sync::Arc<AreaCache>,
    ) -> std::rc::Rc<super::tags::TagIndex> {
        self.tag_index.get(area, self.secrets_apply())
    }

    /// The place the selected rooms' tag input writes: on map rooms "Add
    /// to", on a place's own room that place; `None` without selected rooms.
    pub(super) fn tag_destination(&self) -> Option<SourceId> {
        let selection = self.editor.selection();
        match selection.single() {
            Some(EntityId::Room(_)) => Some(super::tags::destination(SourceId::Map, self.add_to())),
            Some(EntityId::SourceRoom(source, _)) => {
                Some(super::tags::destination(source, self.add_to()))
            }
            Some(_) => None,
            None => (selection.rooms().next().is_some()
                || selection.source_rooms().next().is_some())
            .then(|| self.add_to()),
        }
    }

    /// Where the tag input writes, when the viewer may add there and some
    /// selected room can carry that place's tags.
    pub(super) fn tag_input_place(&self) -> Option<SourceId> {
        let place = self.tag_destination()?;
        let area = self
            .mapper
            .get_current_atlas()
            .get_area(&self.editor.area_id()?)?;
        let applicable = self.tag_rooms(place).len();
        (super::secrets::can_add(&area, place) && applicable > 0).then_some(place)
    }

    /// The readable selected rooms, with their source retained independently
    /// of the place that owns the tag.
    fn tag_rooms(&self, place: SourceId) -> Vec<(SourceId, RoomNumber)> {
        let selection = self.editor.selection();
        let keeps_own = self
            .editor
            .area_id()
            .and_then(|id| self.mapper.get_current_atlas().get_area(&id))
            .is_some_and(|area| area.keeps_only_own_rooms(place));
        let mut rooms: Vec<_> = selection
            .rooms()
            .map(|number| (SourceId::Map, number))
            .chain(selection.source_rooms())
            .filter(|(source, _)| !keeps_own || *source == place)
            .collect();
        rooms.sort_unstable();
        rooms
    }

    /// Empties the tag input when the place it writes is no longer the
    /// place it was typed for: "Add to" changed, or the place went away or
    /// stopped taking additions, by any route.
    pub(super) fn drop_stale_tag_input(&mut self) {
        if !self.inspector.tag_input.is_empty()
            && self.tag_input_place() != self.inspector.tag_place
        {
            self.inspector.clear_tag_input();
        }
    }

    /// What the tag input suggests now.
    pub(super) fn tag_suggestions(&self) -> Vec<super::tags::Suggestion> {
        let Some(place) = self.tag_input_place() else {
            return Vec::new();
        };
        let Some(area) = self
            .editor
            .area_id()
            .and_then(|id| self.mapper.get_current_atlas().get_area(&id))
        else {
            return Vec::new();
        };
        let tags = &self.inspector.tags;
        self.tag_index(&area)
            .suggestions(place, &self.inspector.tag_input, |tag| {
                tags.all_have(place, tag, area.keeps_only_own_rooms(place))
            })
    }

    /// Adds `tag` where the tag input writes, on every selected room that
    /// can carry it there and lacks it, as one undo entry. The input clears
    /// and keeps focus; a tag every room already has changes nothing.
    fn add_tag(&mut self, tag: &str) -> Update<super::Message, super::Event> {
        let focus = iced::widget::operation::focus(super::tags::input_id(self.window_id));
        let Some(place) = self.tag_input_place() else {
            return Update::none();
        };
        let Some(area) = self
            .editor
            .area_id()
            .and_then(|id| self.mapper.get_current_atlas().get_area(&id))
        else {
            return Update::none();
        };
        let rooms = self.tag_rooms(place);
        let name = super::secrets::place_name(&area, place);
        let change = source_rooms::change_tags(&area, place, &rooms, tag, true, &name);
        self.inspector.tag_input.clear();
        let Some(change) = change else {
            return Update::with_task(focus);
        };
        let several = self.editor.selection().single().is_none();
        let applicable = rooms.len();
        let changed = change.changed;
        // A refusal (where it may not write, or applying it) lands in the
        // footer; the count would hide it.
        let notice_before = self.editor_notice.as_ref().map(|(shown, _)| *shown);
        let mut update = self.push_command(Some(change.command));
        let refused = self.editor_notice.as_ref().map(|(shown, _)| *shown) != notice_before;
        if several && !refused {
            let tag = smudgy_cloud::mapper::normalize_tag(tag);
            let already = applicable.saturating_sub(changed);
            let notice = if already == 0 {
                crate::i18n::t!("inspector-tag-added", "tag" => tag, "count" => applicable)
            } else {
                crate::i18n::t!(
                    "inspector-tag-added-some-had",
                    "tag" => tag,
                    "count" => applicable,
                    "already" => already
                )
            };
            self.editor_notice = Some((std::time::Instant::now(), notice));
        }
        self.inspector.resync(&self.mapper, &self.editor);
        update.task = Task::batch([update.task, focus]);
        update
    }

    /// Removes `tag` from `place` on every selected room carrying it there,
    /// as one undo entry, where the viewer may remove from `place`.
    fn remove_tag(&mut self, place: SourceId, tag: &str) -> Update<super::Message, super::Event> {
        let Some(area) = self
            .editor
            .area_id()
            .and_then(|id| self.mapper.get_current_atlas().get_area(&id))
        else {
            return Update::none();
        };
        if !super::secrets::can_remove(&area, place) {
            return Update::none();
        }
        let rooms = self.tag_rooms(place);
        let name = super::secrets::place_name(&area, place);
        let Some(change) = source_rooms::change_tags(&area, place, &rooms, tag, false, &name)
        else {
            return Update::none();
        };
        let update = self.push_command(Some(change.command));
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    fn commit_room_field(
        &mut self,
        field: FieldId,
        updates: RoomUpdates,
    ) -> Update<super::Message, super::Event> {
        if let Some(target) = self.selected_source_room() {
            let command =
                source_rooms::edit_field(&self.mapper.get_current_atlas(), target, field, updates);
            return self.push_command(command);
        }
        let Some(room_key) = self.selected_room_key() else {
            return Update::none();
        };
        if field == FieldId::Position {
            self.mark_moved_automatic_routes_stale();
        }
        let command =
            commands::edit_room_field(&self.mapper.get_current_atlas(), room_key, field, updates);
        self.push_command(command)
    }

    fn selected_label_id(&self) -> Option<(AreaId, LabelId)> {
        match self.editor.selection().single() {
            Some(EntityId::Label(label_id)) => Some((self.editor.area_id()?, label_id)),
            _ => None,
        }
    }

    fn selected_shape_id(&self) -> Option<(AreaId, ShapeId)> {
        match self.editor.selection().single() {
            Some(EntityId::Shape(shape_id)) => Some((self.editor.area_id()?, shape_id)),
            _ => None,
        }
    }

    fn selected_connection_id(&self) -> Option<(AreaId, ConnectionId)> {
        match self.editor.selection().single() {
            Some(EntityId::Connection(connection_id)) => {
                Some((self.editor.area_id()?, connection_id))
            }
            _ => None,
        }
    }

    fn commit_connection_field(
        &mut self,
        field: FieldId,
        updates: ConnectionUpdates,
        description: &'static str,
    ) -> Update<super::Message, super::Event> {
        let Some((area_id, connection_id)) = self.selected_connection_id() else {
            return Update::none();
        };
        if matches!(
            field,
            FieldId::Endpoint | FieldId::CornerStyle | FieldId::Thickness
        ) && self
            .mapper
            .get_current_atlas()
            .get_area(&area_id)
            .and_then(|area| {
                area.find_connection(connection_id)
                    .map(|(_, connection)| connection.routing == ConnectionRouting::Automatic)
            })
            .unwrap_or(false)
        {
            self.automatic_routes_maybe_stale.insert(connection_id);
        }
        let command = commands::edit_connection(
            &self.mapper.get_current_atlas(),
            area_id,
            connection_id,
            field,
            updates,
            description,
        );
        self.push_command(command)
    }

    fn commit_label_field(
        &mut self,
        field: FieldId,
        updates: LabelUpdates,
    ) -> Update<super::Message, super::Event> {
        let Some((area_id, label_id)) = self.selected_label_id() else {
            return Update::none();
        };
        let command = commands::edit_label_field(
            &self.mapper.get_current_atlas(),
            area_id,
            label_id,
            field,
            updates,
        );
        self.push_command(command)
    }

    fn commit_shape_field(
        &mut self,
        field: FieldId,
        updates: ShapeUpdates,
    ) -> Update<super::Message, super::Event> {
        let Some((area_id, shape_id)) = self.selected_shape_id() else {
            return Update::none();
        };
        let command = commands::edit_shape_field(
            &self.mapper.get_current_atlas(),
            area_id,
            shape_id,
            field,
            updates,
        );
        self.push_command(command)
    }

    /// Commits an exit direction change with the matching Connection
    /// endpoint re-anchor in one undo unit: the port follows the new
    /// direction to its home slot. `to_side` targets the destination
    /// endpoint instead (only when no member exit originates there — a
    /// reciprocal member's own direction governs its port).
    pub(super) fn commit_exit_direction(
        &mut self,
        exit_ref: ExitRef,
        field: FieldId,
        direction: ExitDirection,
        to_side: bool,
    ) -> Update<super::Message, super::Event> {
        self.commit_exit_direction_with(exit_ref, field, direction, to_side, None)
    }

    /// [`Self::commit_exit_direction`], with `also` (a write and its undo)
    /// in the same undo step: the other exit's arrival, kept elsewhere.
    pub(super) fn commit_exit_direction_with(
        &mut self,
        exit_ref: ExitRef,
        field: FieldId,
        direction: ExitDirection,
        to_side: bool,
        also: Option<(commands::Mutation, commands::Mutation)>,
    ) -> Update<super::Message, super::Event> {
        let atlas = self.mapper.get_current_atlas();
        let Some(map) = atlas.get_area(&exit_ref.area_id) else {
            return Update::none();
        };
        // The exit's link lives in the exit's own place.
        let Some(document) = Document::of(&map, exit_ref.place) else {
            return Update::none();
        };
        let area = document.content();
        let connection_edit = (|| {
            let room = area.get_room_at(exit_ref.room)?;
            let exit = room
                .get_exits()
                .iter()
                .find(|exit| exit.id == exit_ref.id)?;
            let connection = area.get_connection(exit.connection_id)?;
            let target_room = if to_side {
                let destination = exit.destination_address()?;
                let to_room = destination.room;
                if destination.map != exit_ref.area_id {
                    return None;
                }
                if area
                    .get_room_at(to_room)?
                    .get_exits()
                    .iter()
                    .any(|other| other.connection_id == connection.id)
                {
                    return None;
                }
                to_room
            } else {
                exit_ref.room
            };
            endpoint_reanchor(area, connection, target_room, direction)
                .map(|updates| (connection.id, updates))
        })();
        let change = |updates: &mut smudgy_cloud::ExitUpdates| {
            if to_side {
                updates.to_direction = Some(direction);
            } else {
                updates.from_direction = Some(direction);
            }
        };
        let command = match &connection_edit {
            // With an endpoint re-anchor, one atomic two-mutation batch —
            // deliberately not coalescing (see edit_exit_with_endpoint).
            Some((connection_id, updates)) => {
                // Re-anchoring an Automatic route's endpoint leaves its
                // stored route stale exactly like an inspector port edit.
                if area
                    .get_connection(*connection_id)
                    .is_some_and(|connection| connection.routing == ConnectionRouting::Automatic)
                {
                    self.automatic_routes_maybe_stale.insert(*connection_id);
                }
                // If the endpoint will now render as its level triangle,
                // its port handle vanishes with this change; drop a
                // selected one rather than let keyboard nudges edit an
                // invisible port. (All other cases keep their handles —
                // the enum is positionless, so the selection stays valid
                // at the new anchor.)
                let becomes_triangle =
                    area.get_connection(*connection_id)
                        .is_some_and(|connection| {
                            smudgy_cloud::connection_geometry::renders_as_level_triangle(
                                connection.kind,
                                connection.routing,
                                smudgy_cloud::connection_geometry::StubAxis::for_direction(
                                    direction,
                                ),
                            )
                        });
                if becomes_triangle {
                    self.editor.clear_selected_connection_handle();
                }
                commands::edit_exit_with_endpoint(
                    &atlas,
                    exit_ref,
                    change,
                    *connection_id,
                    updates.clone(),
                )
            }
            // No endpoint to move: the plain coalescing exit edit.
            None => commands::edit_exit_field(&atlas, exit_ref, field, change),
        };
        let command = match also {
            Some((redo, undo)) => command.map(|command| command.also(redo, undo)),
            None => command,
        };
        let update = self.push_command(command);
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    pub(super) fn update_inspector(
        &mut self,
        message: Message,
    ) -> Update<super::Message, super::Event> {
        match message {
            Message::TitleChanged(value) => {
                self.inspector.title = value.clone();
                self.commit_room_field(
                    FieldId::Title,
                    RoomUpdates {
                        title: Some(value),
                        ..Default::default()
                    },
                )
            }
            Message::DescriptionEdited(action) => {
                let is_edit = action.is_edit();
                self.inspector.description.perform(action);
                if is_edit {
                    self.commit_room_field(
                        FieldId::Description,
                        RoomUpdates {
                            description: Some(self.inspector.description.text()),
                            ..Default::default()
                        },
                    )
                } else {
                    // Cursor movement and selection don't touch the data.
                    Update::none()
                }
            }
            Message::LevelChanged(value) => {
                self.inspector.level = value.clone();
                match value.parse::<i32>() {
                    Ok(level) => self.commit_room_field(
                        FieldId::Level,
                        RoomUpdates {
                            level: Some(level),
                            ..Default::default()
                        },
                    ),
                    Err(_) => Update::none(),
                }
            }
            Message::XChanged(value) => {
                self.inspector.x = value.clone();
                match value.parse::<f32>() {
                    Ok(x) => self.commit_room_field(
                        FieldId::Position,
                        RoomUpdates {
                            x: Some(x),
                            ..Default::default()
                        },
                    ),
                    Err(_) => Update::none(),
                }
            }
            Message::YChanged(value) => {
                self.inspector.y = value.clone();
                match value.parse::<f32>() {
                    Ok(y) => self.commit_room_field(
                        FieldId::Position,
                        RoomUpdates {
                            y: Some(y),
                            ..Default::default()
                        },
                    ),
                    Err(_) => Update::none(),
                }
            }
            Message::ColorChanged(value) => {
                self.inspector.color = value.clone();
                if value.is_empty() || parse_color(&value).is_some() {
                    self.commit_room_field(
                        FieldId::Color,
                        RoomUpdates {
                            color: Some(value),
                            ..Default::default()
                        },
                    )
                } else {
                    Update::none()
                }
            }
            Message::PlaceValueChanged(source, index, value) => {
                let Some(target) = self.place_target(source) else {
                    return Update::none();
                };
                let Some(row) = self
                    .place_mut(source)
                    .and_then(|place| place.properties.get_mut(index))
                else {
                    return Update::none();
                };
                row.value = value.clone();
                let name = row.name.clone();
                let command = source_rooms::set_property(
                    &self.mapper.get_current_atlas(),
                    target,
                    name,
                    value,
                );
                self.push_command(command)
            }
            Message::PlacePropertyDeleted(source, index) => {
                let Some(target) = self.place_target(source) else {
                    return Update::none();
                };
                let Some(place) = self.place_mut(source) else {
                    return Update::none();
                };
                if index >= place.properties.len() {
                    return Update::none();
                }
                let row = place.properties.remove(index);
                let command = source_rooms::delete_property(
                    &self.mapper.get_current_atlas(),
                    target,
                    row.name,
                );
                self.push_command(command)
            }
            Message::PlaceNewNameChanged(source, value) => {
                if let Some(place) = self.place_mut(source) {
                    place.new_name = value;
                }
                Update::none()
            }
            Message::PlaceNewValueChanged(source, value) => {
                if let Some(place) = self.place_mut(source) {
                    place.new_value = value;
                }
                Update::none()
            }
            Message::PlacePropertyAdded(source) => {
                let Some(target) = self.place_target(source) else {
                    return Update::none();
                };
                let Some(place) = self.place_mut(source) else {
                    return Update::none();
                };
                let name = place.new_name.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                let value = std::mem::take(&mut place.new_value);
                place.new_name.clear();
                let command = source_rooms::set_property(
                    &self.mapper.get_current_atlas(),
                    target,
                    name,
                    value,
                );
                let update = self.push_command(command);
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            Message::PlaceMenuToggled(open) => {
                self.secrets.place_menu_open = open;
                Update::none()
            }
            Message::PlaceStarted(source) => {
                self.secrets.place_menu_open = false;
                self.secrets.place_started = Some(source);
                Update::none()
            }
            Message::PropertyValueChanged(index, value) => {
                let Some(row) = self.inspector.properties.get_mut(index) else {
                    return Update::none();
                };
                row.value = value.clone();
                let name = row.name.clone();
                if let Some(target) = self.selected_source_room() {
                    let command = source_rooms::set_property(
                        &self.mapper.get_current_atlas(),
                        target,
                        name,
                        value,
                    );
                    return self.push_command(command);
                }
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                let command = commands::set_room_property(
                    &self.mapper.get_current_atlas(),
                    room_key,
                    name,
                    value,
                );
                self.push_command(command)
            }
            Message::PropertyDeleted(index) => {
                if index >= self.inspector.properties.len() {
                    return Update::none();
                }
                let row = self.inspector.properties.remove(index);
                if let Some(target) = self.selected_source_room() {
                    let command = source_rooms::delete_property(
                        &self.mapper.get_current_atlas(),
                        target,
                        row.name,
                    );
                    return self.push_command(command);
                }
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                let command = commands::delete_room_property(
                    &self.mapper.get_current_atlas(),
                    room_key,
                    row.name,
                );
                self.push_command(command)
            }
            Message::NewPropertyNameChanged(value) => {
                self.inspector.new_property_name = value;
                Update::none()
            }
            Message::NewPropertyValueChanged(value) => {
                self.inspector.new_property_value = value;
                Update::none()
            }
            Message::AddProperty => {
                let name = self.inspector.new_property_name.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                let value = self.inspector.new_property_value.clone();
                let command = if let Some(target) = self.selected_source_room() {
                    source_rooms::set_property(
                        &self.mapper.get_current_atlas(),
                        target,
                        name.clone(),
                        value.clone(),
                    )
                } else {
                    let Some(room_key) = self.selected_room_key() else {
                        return Update::none();
                    };
                    commands::set_room_property(
                        &self.mapper.get_current_atlas(),
                        room_key,
                        name.clone(),
                        value.clone(),
                    )
                };
                let update = self.push_command(command);
                self.inspector.properties.push(PropertyRow { name, value });
                self.inspector
                    .properties
                    .sort_by(|a, b| a.name.cmp(&b.name));
                self.inspector.new_property_name.clear();
                self.inspector.new_property_value.clear();
                update
            }
            Message::TagInputChanged(value) => {
                if self.inspector.tag_input.is_empty() {
                    self.inspector.tag_place = self.tag_input_place();
                }
                self.inspector.tag_input = value;
                Update::none()
            }
            Message::TagSubmitted if self.tag_input_place() != self.inspector.tag_place => {
                // Typed for a place it would no longer reach.
                self.inspector.clear_tag_input();
                Update::none()
            }
            Message::TagSubmitted => match super::tags::typed(&self.inspector.tag_input) {
                super::tags::Typed::Tag(tag) => self.add_tag(&tag),
                // Too long: the message under the input says so.
                super::tags::Typed::Empty | super::tags::Typed::TooLong => Update::none(),
            },
            Message::TagSuggestionPicked(tag) => self.add_tag(&tag),
            Message::TagRemoved(place, tag) => self.remove_tag(place, &tag),
            Message::TagCompleted(focused) => {
                let first = self
                    .tag_suggestions()
                    .into_iter()
                    .next()
                    .map(|suggestion| suggestion.tag);
                match first {
                    Some(tag) if focused => {
                        self.inspector.tag_place = self.tag_input_place();
                        self.inspector.tag_input = tag;
                        Update::with_task(iced::widget::operation::move_cursor_to_end(
                            super::tags::input_id(self.window_id),
                        ))
                    }
                    // Not completing a tag: Tab moves on.
                    _ => Update::with_task(focus_step(self.window_id, false)),
                }
            }
            Message::BulkColorChanged(value) => {
                self.inspector.bulk_color = value;
                Update::none()
            }
            Message::BulkLevelChanged(value) => {
                self.inspector.bulk_level = value;
                Update::none()
            }
            Message::ApplyBulkColor => {
                let value = self.inspector.bulk_color.clone();
                if !value.is_empty() && parse_color(&value).is_none() {
                    return Update::none();
                }
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let command = commands::bulk_edit_rooms(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    self.editor.selection(),
                    &RoomUpdates {
                        color: Some(value),
                        ..Default::default()
                    },
                );
                // The rooms now agree on this color.
                self.inspector.bulk_color_mixed = false;
                self.push_command(command)
            }
            Message::ApplyBulkLevel => {
                let Ok(level) = self.inspector.bulk_level.parse::<i32>() else {
                    return Update::none();
                };
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let command = commands::bulk_edit_rooms(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    self.editor.selection(),
                    &RoomUpdates {
                        level: Some(level),
                        ..Default::default()
                    },
                );
                self.inspector.bulk_level_mixed = false;
                self.push_command(command)
            }
            Message::AreaPropertyValueChanged(index, value) => {
                let Some(row) = self.inspector.area_properties.get_mut(index) else {
                    return Update::none();
                };
                row.value = value.clone();
                let name = row.name.clone();
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let command = commands::set_area_property(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    name,
                    value,
                );
                self.push_command(command)
            }
            Message::AreaPropertyDeleted(index) => {
                if index >= self.inspector.area_properties.len() {
                    return Update::none();
                }
                let row = self.inspector.area_properties.remove(index);
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let command = commands::delete_area_property(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    row.name,
                );
                self.push_command(command)
            }
            Message::NewAreaPropertyNameChanged(value) => {
                self.inspector.new_area_property_name = value;
                Update::none()
            }
            Message::NewAreaPropertyValueChanged(value) => {
                self.inspector.new_area_property_value = value;
                Update::none()
            }
            Message::ConnectionRoutingChanged(routing) => {
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(map) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                // A Secret's link is read from its Secret's document.
                let Some((area, _)) = map.connection_document(connection_id) else {
                    return Update::none();
                };
                let Some(connection) = area.get_connection(connection_id) else {
                    return Update::none();
                };
                if !connection.kind.allows_routing(routing) {
                    return Update::none();
                }
                if routing == ConnectionRouting::Automatic {
                    return self.start_automatic_route(connection_id);
                }
                self.inspector.connection.routing = routing;
                self.commit_connection_field(
                    FieldId::Routing,
                    ConnectionUpdates {
                        routing: Some(routing),
                        ..ConnectionUpdates::default()
                    },
                    "Change connection routing",
                )
            }
            Message::ConnectionSegmentShapeChanged(segment_shape) => {
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(map) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                // A Secret's link is read from its Secret's document.
                let Some((area, _)) = map.connection_document(connection_id) else {
                    return Update::none();
                };
                let Some(connection) = area.get_connection(connection_id) else {
                    return Update::none();
                };
                let mut route_points = connection.route_points.clone();
                if segment_shape == SegmentShape::Orthogonal
                    && connection.routing == ConnectionRouting::Manual
                    && let Some(render) = area.get_room_connections().iter().find(|render| {
                        render.connection_id == connection_id
                            && render.geometry.stub_tip_b.is_some()
                    })
                {
                    let Some(normalized) = smudgy_cloud::connection_geometry::orthogonalize_route(
                        render.geometry.stub_tip_a,
                        &route_points,
                        render.geometry.stub_tip_b.expect("checked"),
                    ) else {
                        self.editor_notice = Some((
                            std::time::Instant::now(),
                            crate::i18n::t!("inspector-route-too-many-points"),
                        ));
                        return Update::none();
                    };
                    route_points = normalized;
                }
                self.inspector.connection.segment_shape = segment_shape;
                self.commit_connection_field(
                    FieldId::SegmentShape,
                    ConnectionUpdates {
                        segment_shape: Some(segment_shape),
                        route_points: Some(route_points),
                        ..ConnectionUpdates::default()
                    },
                    "Change connection segment shape",
                )
            }
            Message::ConnectionCornerChanged(corner) => {
                self.inspector.connection.corner = corner;
                self.commit_connection_field(
                    FieldId::CornerStyle,
                    ConnectionUpdates {
                        corner: Some(corner),
                        ..ConnectionUpdates::default()
                    },
                    "Change connection corners",
                )
            }
            Message::ConnectionDashChanged(dash) => {
                self.inspector.connection.dash = dash;
                self.commit_connection_field(
                    FieldId::DashStyle,
                    ConnectionUpdates {
                        dash: Some(dash),
                        ..ConnectionUpdates::default()
                    },
                    "Change connection dash",
                )
            }
            Message::ConnectionColorChanged(value) => {
                self.inspector.connection.color = value.clone();
                // An empty buffer stays uncommitted (the input shows it as
                // invalid) rather than committing a value the mirror would
                // silently rewrite to the default.
                if smudgy_cloud::canonicalize_css_color(&value).is_none() {
                    return Update::none();
                }
                self.commit_connection_field(
                    FieldId::Color,
                    ConnectionUpdates {
                        color: Some(value),
                        ..ConnectionUpdates::default()
                    },
                    "Change connection color",
                )
            }
            Message::ConnectionThicknessPicked(thickness) => {
                if !smudgy_cloud::THICKNESS_RANGE.contains(&thickness) {
                    return Update::none();
                }
                self.inspector.connection.thickness = thickness.to_string();
                self.commit_connection_field(
                    FieldId::Thickness,
                    ConnectionUpdates {
                        thickness: Some(thickness),
                        ..ConnectionUpdates::default()
                    },
                    "Change connection thickness",
                )
            }
            Message::ConnectionEndpointSideChanged(endpoint_b, side) => {
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(map) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                // A Secret's link is read from its Secret's document.
                let Some((area, _)) = map.connection_document(connection_id) else {
                    return Update::none();
                };
                let Some(connection) = area.get_connection(connection_id) else {
                    return Update::none();
                };
                let endpoint = if endpoint_b {
                    let Some(mut endpoint) = connection.endpoint_b else {
                        return Update::none();
                    };
                    endpoint.side = side;
                    endpoint.port_mode = smudgy_cloud::PortMode::Manual;
                    self.inspector.connection.endpoint_b_side = side;
                    endpoint
                } else {
                    let mut endpoint = connection.endpoint_a;
                    endpoint.side = side;
                    endpoint.port_mode = smudgy_cloud::PortMode::Manual;
                    self.inspector.connection.endpoint_a_side = side;
                    endpoint
                };
                let Some(updates) = endpoint_updates(area, connection_id, endpoint, endpoint_b)
                else {
                    return Update::none();
                };
                self.commit_connection_field(FieldId::Endpoint, updates, "Move connection port")
            }
            Message::ConnectionEndpointOffsetChanged(endpoint_b, value) => {
                if endpoint_b {
                    self.inspector.connection.endpoint_b_offset = value.clone();
                } else {
                    self.inspector.connection.endpoint_a_offset = value.clone();
                }
                let Ok(offset) = value.parse::<f32>() else {
                    return Update::none();
                };
                if !(0.0..=1.0).contains(&offset) {
                    return Update::none();
                }
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(map) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                // A Secret's link is read from its Secret's document.
                let Some((area, _)) = map.connection_document(connection_id) else {
                    return Update::none();
                };
                let Some(connection) = area.get_connection(connection_id) else {
                    return Update::none();
                };
                let endpoint = if endpoint_b {
                    let Some(mut endpoint) = connection.endpoint_b else {
                        return Update::none();
                    };
                    endpoint.port_offset = offset;
                    endpoint.port_mode = smudgy_cloud::PortMode::Manual;
                    endpoint
                } else {
                    let mut endpoint = connection.endpoint_a;
                    endpoint.port_offset = offset;
                    endpoint.port_mode = smudgy_cloud::PortMode::Manual;
                    endpoint
                };
                let Some(updates) = endpoint_updates(area, connection_id, endpoint, endpoint_b)
                else {
                    return Update::none();
                };
                self.commit_connection_field(FieldId::Endpoint, updates, "Move connection port")
            }
            Message::ConnectionEndpointReset(endpoint_b) => {
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(map) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                // A Secret's link is read from its Secret's document.
                let Some((area, _)) = map.connection_document(connection_id) else {
                    return Update::none();
                };
                let Some(connection) = area.get_connection(connection_id) else {
                    return Update::none();
                };
                let Some(mut endpoint) = (if endpoint_b {
                    connection.endpoint_b
                } else {
                    Some(connection.endpoint_a)
                }) else {
                    return Update::none();
                };
                let direction = area
                    .document_rooms()
                    .find_map(|room| {
                        room.get_exits()
                            .iter()
                            .find(|exit| {
                                exit.connection_id == connection_id
                                    && room.address() == endpoint.address()
                            })
                            .map(|exit| exit.from_direction)
                    })
                    .or_else(|| {
                        area.document_rooms().find_map(|room| {
                            room.get_exits()
                                .iter()
                                .find(|exit| {
                                    exit.connection_id == connection_id
                                        && exit.destination_address().is_some_and(|destination| {
                                            destination.map == area_id
                                                && destination.room == endpoint.address()
                                        })
                                })
                                .and_then(|exit| exit.to_direction)
                        })
                    });
                let direction = direction.unwrap_or(match endpoint.side {
                    RoomSide::North => ExitDirection::North,
                    RoomSide::East => ExitDirection::East,
                    RoomSide::South => ExitDirection::South,
                    RoomSide::West => ExitDirection::West,
                });
                let (side, offset) = smudgy_cloud::default_anchor_for_direction(direction, None);
                endpoint.side = side;
                endpoint.port_offset = offset;
                endpoint.port_mode = smudgy_cloud::PortMode::AutoPinned;
                let Some(updates) = endpoint_updates(area, connection_id, endpoint, endpoint_b)
                else {
                    return Update::none();
                };
                let update = self.commit_connection_field(
                    FieldId::Endpoint,
                    updates,
                    "Reset connection port to automatic",
                );
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            Message::ConnectionRedistributePorts(endpoint_b) => {
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(map) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                // A Secret's link is read from its Secret's document.
                let Some((area, _)) = map.connection_document(connection_id) else {
                    return Update::none();
                };
                let Some(connection) = area.get_connection(connection_id) else {
                    return Update::none();
                };
                let Some(endpoint) = (if endpoint_b {
                    connection.endpoint_b
                } else {
                    Some(connection.endpoint_a)
                }) else {
                    return Update::none();
                };
                let edits = redistribute_port_updates(area, endpoint.address(), endpoint.side);
                // The place's own write access gates the push.
                if edits.is_empty() {
                    return Update::none();
                }
                let shown = Document::of_connection(&map, connection_id)
                    .map_or(endpoint.room_number, |document| {
                        document.shown_room(endpoint.address()).0
                    });
                let command = super::commands::edit_connections(
                    &atlas,
                    area_id,
                    edits,
                    format!("Redistribute room {shown} {} ports", endpoint.side),
                );
                let update = self.push_command(command);
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            Message::ConnectionClearRoute => {
                let routing = self
                    .selected_connection_id()
                    .and_then(|(area_id, connection_id)| {
                        self.mapper
                            .get_current_atlas()
                            .get_area(&area_id)
                            .and_then(|area| {
                                area.find_connection(connection_id).map(|(_, connection)| {
                                    matches!(
                                        connection.routing,
                                        ConnectionRouting::Manual | ConnectionRouting::Automatic
                                    )
                                    .then_some(ConnectionRouting::Simple)
                                })
                            })
                    })
                    .flatten();
                self.commit_connection_field(
                    FieldId::RoutePoints,
                    ConnectionUpdates {
                        routing,
                        route_points: Some(Vec::new()),
                        ..ConnectionUpdates::default()
                    },
                    "Clear connection route",
                )
            }
            Message::ConnectionReroute => {
                let Some((_, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                self.start_automatic_route(connection_id)
            }
            Message::ConnectionReset => self.commit_connection_field(
                FieldId::Routing,
                ConnectionUpdates {
                    routing: Some(ConnectionRouting::Simple),
                    segment_shape: Some(SegmentShape::Direct),
                    corner: Some(CornerStyle::Sharp),
                    route_points: Some(Vec::new()),
                    dash: Some(ConnectionDash::Solid),
                    color: Some(DEFAULT_CONNECTION_COLOR.to_string()),
                    thickness: Some(DEFAULT_CONNECTION_THICKNESS),
                    ..ConnectionUpdates::default()
                },
                "Reset connection appearance",
            ),
            Message::LabelTextChanged(value) => {
                self.inspector.label.text = value.clone();
                self.commit_label_field(
                    FieldId::Text,
                    LabelUpdates {
                        text: Some(value),
                        ..Default::default()
                    },
                )
            }
            Message::LabelColorChanged(value) => {
                self.inspector.label.color = value.clone();
                if parse_color(&value).is_some() {
                    self.commit_label_field(
                        FieldId::Color,
                        LabelUpdates {
                            color: Some(value),
                            ..Default::default()
                        },
                    )
                } else {
                    Update::none()
                }
            }
            Message::LabelBackgroundChanged(value) => {
                self.inspector.label.background = value.clone();
                if value.is_empty() || parse_color(&value).is_some() {
                    self.commit_label_field(
                        FieldId::BackgroundColor,
                        LabelUpdates {
                            background_color: Some(value),
                            ..Default::default()
                        },
                    )
                } else {
                    Update::none()
                }
            }
            Message::LabelFontSizeChanged(value) => {
                self.inspector.label.font_size = value.clone();
                match value.parse::<i32>() {
                    Ok(font_size) if font_size > 0 => self.commit_label_field(
                        FieldId::FontSize,
                        LabelUpdates {
                            font_size: Some(font_size),
                            ..Default::default()
                        },
                    ),
                    _ => Update::none(),
                }
            }
            Message::LabelFontWeightChanged(value) => {
                self.inspector.label.font_weight = value.clone();
                match value.parse::<i32>() {
                    Ok(font_weight) if font_weight > 0 => self.commit_label_field(
                        FieldId::FontWeight,
                        LabelUpdates {
                            font_weight: Some(font_weight),
                            ..Default::default()
                        },
                    ),
                    _ => Update::none(),
                }
            }
            Message::LabelHorizontalAlignmentChanged(alignment) => {
                self.inspector.label.horizontal_alignment = alignment.clone();
                self.commit_label_field(
                    FieldId::HorizontalAlignment,
                    LabelUpdates {
                        horizontal_alignment: Some(alignment),
                        ..Default::default()
                    },
                )
            }
            Message::LabelVerticalAlignmentChanged(alignment) => {
                self.inspector.label.vertical_alignment = alignment.clone();
                self.commit_label_field(
                    FieldId::VerticalAlignment,
                    LabelUpdates {
                        vertical_alignment: Some(alignment),
                        ..Default::default()
                    },
                )
            }
            Message::LabelBoundsChanged(bounds_field, value) => {
                {
                    let label = &mut self.inspector.label;
                    match bounds_field {
                        BoundsField::X => label.x = value.clone(),
                        BoundsField::Y => label.y = value.clone(),
                        BoundsField::Width => label.width = value.clone(),
                        BoundsField::Height => label.height = value.clone(),
                    }
                }
                match value.parse::<f32>() {
                    Ok(parsed) => {
                        let mut updates = LabelUpdates::default();
                        match bounds_field {
                            BoundsField::X => updates.x = Some(parsed),
                            BoundsField::Y => updates.y = Some(parsed),
                            BoundsField::Width => updates.width = Some(parsed.max(0.1)),
                            BoundsField::Height => updates.height = Some(parsed.max(0.1)),
                        }
                        self.commit_label_field(FieldId::Bounds, updates)
                    }
                    Err(_) => Update::none(),
                }
            }
            Message::ShapeTypeChanged(shape_type) => {
                self.inspector.shape.shape_type = shape_type.clone();
                self.commit_shape_field(
                    FieldId::ShapeType,
                    ShapeUpdates {
                        shape_type: Some(shape_type),
                        ..Default::default()
                    },
                )
            }
            Message::ShapeBackgroundChanged(value) => {
                self.inspector.shape.background = value.clone();
                if value.is_empty() || parse_color(&value).is_some() {
                    self.commit_shape_field(
                        FieldId::BackgroundColor,
                        ShapeUpdates {
                            background_color: Some(value),
                            ..Default::default()
                        },
                    )
                } else {
                    Update::none()
                }
            }
            Message::ShapeStrokeColorChanged(value) => {
                self.inspector.shape.stroke_color = value.clone();
                if value.is_empty() || parse_color(&value).is_some() {
                    self.commit_shape_field(
                        FieldId::StrokeColor,
                        ShapeUpdates {
                            stroke_color: Some(value),
                            ..Default::default()
                        },
                    )
                } else {
                    Update::none()
                }
            }
            Message::ShapeStrokeWidthChanged(value) => {
                self.inspector.shape.stroke_width = value.clone();
                match value.parse::<f32>() {
                    Ok(width) if width >= 0.0 => self.commit_shape_field(
                        FieldId::StrokeWidth,
                        ShapeUpdates {
                            stroke_width: Some(width),
                            ..Default::default()
                        },
                    ),
                    _ => Update::none(),
                }
            }
            Message::ShapeBorderRadiusChanged(value) => {
                self.inspector.shape.border_radius = value.clone();
                match value.parse::<f32>() {
                    Ok(radius) if radius >= 0.0 => self.commit_shape_field(
                        FieldId::BorderRadius,
                        ShapeUpdates {
                            border_radius: Some(radius),
                            ..Default::default()
                        },
                    ),
                    _ => Update::none(),
                }
            }
            Message::ShapeBoundsChanged(bounds_field, value) => {
                {
                    let shape = &mut self.inspector.shape;
                    match bounds_field {
                        BoundsField::X => shape.x = value.clone(),
                        BoundsField::Y => shape.y = value.clone(),
                        BoundsField::Width => shape.width = value.clone(),
                        BoundsField::Height => shape.height = value.clone(),
                    }
                }
                match value.parse::<f32>() {
                    Ok(parsed) => {
                        let mut updates = ShapeUpdates::default();
                        match bounds_field {
                            BoundsField::X => updates.x = Some(parsed),
                            BoundsField::Y => updates.y = Some(parsed),
                            BoundsField::Width => updates.width = Some(parsed.max(0.1)),
                            BoundsField::Height => updates.height = Some(parsed.max(0.1)),
                        }
                        self.commit_shape_field(FieldId::Bounds, updates)
                    }
                    Err(_) => Update::none(),
                }
            }
            Message::PickerToggled(field) => {
                let already_open = self
                    .inspector
                    .picker
                    .as_ref()
                    .is_some_and(|(open, _)| *open == field);

                self.inspector.picker = if already_open {
                    None
                } else {
                    let initial = parse_color(self.inspector.color_buffer(field))
                        .unwrap_or(iced::Color::from_rgb8(128, 128, 128));
                    Some((field, ColorPicker::from_color(initial)))
                };
                Update::none()
            }
            Message::Picker(message) => {
                let Some((field, picker)) = &mut self.inspector.picker else {
                    return Update::none();
                };
                let field = *field;
                match picker.update(message) {
                    // Mid-drag: the picker canvases preview the color; the
                    // field only commits (and syncs) on release.
                    color_picker::Event::Preview => Update::none(),
                    color_picker::Event::Committed(color) => {
                        let hex = color_picker::to_hex(color);
                        match field {
                            ColorField::Room => self.update_inspector(Message::ColorChanged(hex)),
                            ColorField::Bulk => {
                                self.inspector.bulk_color = hex;
                                self.update_inspector(Message::ApplyBulkColor)
                            }
                            ColorField::LabelText => {
                                self.update_inspector(Message::LabelColorChanged(hex))
                            }
                            ColorField::LabelBackground => {
                                self.update_inspector(Message::LabelBackgroundChanged(hex))
                            }
                            ColorField::ShapeFill => {
                                self.update_inspector(Message::ShapeBackgroundChanged(hex))
                            }
                            ColorField::ShapeStroke => {
                                self.update_inspector(Message::ShapeStrokeColorChanged(hex))
                            }
                            ColorField::Connection => {
                                self.update_inspector(Message::ConnectionColorChanged(hex))
                            }
                        }
                    }
                }
            }
            Message::AddAreaProperty => {
                let name = self.inspector.new_area_property_name.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                let value = self.inspector.new_area_property_value.clone();
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let command = commands::set_area_property(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    name.clone(),
                    value.clone(),
                );
                let update = self.push_command(command);
                self.inspector
                    .area_properties
                    .push(PropertyRow { name, value });
                self.inspector
                    .area_properties
                    .sort_by(|a, b| a.name.cmp(&b.name));
                self.inspector.new_area_property_name.clear();
                self.inspector.new_area_property_value.clear();
                update
            }
        }
    }
}

// ===== view =====

fn heading<'a>(content: String) -> iced::widget::Text<'a, crate::Theme> {
    text(content).size(16)
}

/// A room's heading. A map room is "Room #3"; a room of another place names
/// the place, after a dot in its color: "● Bookcase #1".
fn room_heading(
    window: &MapEditorWindow,
    room_number: smudgy_cloud::RoomNumber,
    source: Option<smudgy_cloud::SourceId>,
) -> ThemedElement<'_, super::Message> {
    let atlas = window.mapper.get_current_atlas();
    let area = window.editor.area_id().and_then(|id| atlas.get_area(&id));
    let (Some(source), Some(area)) = (source.filter(|source| !source.is_map()), area) else {
        return heading(crate::i18n::t!(
            "inspector-room-heading",
            "number" => room_number.to_string()
        ))
        .into();
    };
    let color = smudgy_map_widget::sources::source_color(&area, source);
    row![
        text("\u{25CF}")
            .size(16)
            .style(move |_theme: &crate::Theme| text::Style { color }),
        heading(crate::i18n::t!(
            "inspector-place-room-heading",
            "place" => super::secrets::place_name(&area, source),
            "number" => room_number.to_string()
        )),
    ]
    .spacing(6)
    .align_y(Vertical::Center)
    .into()
}

pub(super) fn field_label<'a>(label: impl Into<String>) -> iced::widget::Text<'a, crate::Theme> {
    text(label.into())
        .size(11)
        .style(|theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.6)),
        })
}

fn labeled_input<'a>(
    label: impl Into<String>,
    placeholder: &'static str,
    value: &str,
    valid: bool,
    on_input: impl Fn(String) -> Message + 'a,
) -> ThemedElement<'a, super::Message> {
    let mut col = column![
        field_label(label),
        text_input(placeholder, value)
            .size(14)
            .on_input(move |value| super::Message::Inspector(on_input(value))),
    ]
    .spacing(2);

    if !valid {
        col = col.push(
            text(crate::i18n::t!("inspector-invalid-value"))
                .size(11)
                .style(builtins::text::danger),
        );
    }

    col.into()
}

/// A clickable swatch that toggles the color picker for `field`. Unset or
/// unparseable colors render as a slashed empty well rather than a solid
/// fallback gray, so "no color" doesn't masquerade as a real value.
fn swatch_button<'a>(
    window: &MapEditorWindow,
    color: &str,
    field: ColorField,
) -> ThemedElement<'a, super::Message> {
    // While this field's picker is open, preview its in-flight color.
    let parsed = window
        .inspector
        .picker
        .as_ref()
        .filter(|(open, _)| *open == field)
        .map_or_else(|| parse_color(color), |(_, picker)| Some(picker.color()));

    let well: ThemedElement<'a, super::Message> = match parsed {
        Some(color) => container(space::horizontal().width(0.0))
            .width(18.0)
            .height(18.0)
            .style(move |theme: &crate::Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(color)),
                border: iced::border::color(theme.styles.general.border).width(1.0),
                ..Default::default()
            })
            .into(),
        None => container(
            text(bootstrap_icons::SLASH_CIRCLE)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(14.0)
                .style(|theme: &crate::Theme| iced::widget::text::Style {
                    color: Some(theme.styles.text.normal.scale_alpha(0.4)),
                }),
        )
        .width(18.0)
        .height(18.0)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(Vertical::Center)
        .style(|theme: &crate::Theme| iced::widget::container::Style {
            border: iced::border::color(theme.styles.general.border).width(1.0),
            ..Default::default()
        })
        .into(),
    };

    button(well)
        .style(builtins::button::toolbar)
        .padding(2)
        .on_press(super::Message::Inspector(Message::PickerToggled(field)))
        .into()
}

/// The open picker panel when it belongs to `field`.
fn picker_for<'a>(
    window: &'a MapEditorWindow,
    field: ColorField,
) -> Option<ThemedElement<'a, super::Message>> {
    window
        .inspector
        .picker
        .as_ref()
        .filter(|(open, _)| *open == field)
        .map(|(_, picker)| {
            picker
                .view()
                .map(|message| super::Message::Inspector(Message::Picker(message)))
        })
}

fn trash_button<'a>(message: super::Message) -> ThemedElement<'a, super::Message> {
    button(
        text(bootstrap_icons::TRASH_3)
            .font(fonts::BOOTSTRAP_ICONS)
            .size(14.0),
    )
    .style(builtins::button::toolbar)
    .on_press(message)
    .into()
}

/// Message constructors for one property list (room or area), so the
/// shared editor below stays target-agnostic.
struct PropertyHooks {
    on_value_change: fn(usize, String) -> Message,
    on_delete: fn(usize) -> Message,
    on_new_name: fn(String) -> Message,
    on_new_value: fn(String) -> Message,
    on_add: Message,
}

/// The shared key/value property list editor (rooms and areas).
fn properties_section<'a>(
    rows: &'a [PropertyRow],
    new_name: &'a str,
    new_value: &'a str,
    hooks: &PropertyHooks,
    window: &'a MapEditorWindow,
    from: smudgy_cloud::SourceId,
    anchor: Option<(smudgy_cloud::SourceId, smudgy_cloud::RoomNumber)>,
) -> ThemedElement<'a, super::Message> {
    let mut section = Column::new().spacing(4);
    section = section.push(field_label(crate::i18n::t!("inspector-properties")));

    let on_value_change = hooks.on_value_change;
    let on_delete = hooks.on_delete;
    let on_new_name = hooks.on_new_name;
    let on_new_value = hooks.on_new_value;

    for (index, property_row) in rows.iter().enumerate() {
        let mut widgets = row![
            text(property_row.name.clone())
                .size(13)
                .width(Length::FillPortion(2)),
            text_input(
                crate::i18n::ts!("inspector-value-placeholder"),
                &property_row.value
            )
            .size(13)
            .on_input(move |value| { super::Message::Inspector(on_value_change(index, value)) })
            .width(Length::FillPortion(3)),
            trash_button(super::Message::Inspector(on_delete(index))),
        ]
        .spacing(4)
        .align_y(Vertical::Center);
        if let Some(move_to) = super::moves::property_move(
            window,
            from,
            smudgy_cloud::mutation::PropertyAddress {
                name: property_row.name.clone(),
                room_number: anchor.map(|(_, number)| number),
                room_source: anchor.map_or(smudgy_cloud::SourceId::Map, |(source, _)| source),
            },
        ) {
            widgets = widgets.push(move_to);
        }
        section = section.push(widgets);
    }

    section = section.push(
        row![
            text_input(crate::i18n::ts!("inspector-name-placeholder"), new_name)
                .size(13)
                .on_input(move |value| super::Message::Inspector(on_new_name(value)))
                .width(Length::FillPortion(2)),
            text_input(crate::i18n::ts!("inspector-value-placeholder"), new_value)
                .size(13)
                .on_input(move |value| super::Message::Inspector(on_new_value(value)))
                .on_submit(super::Message::Inspector(hooks.on_add.clone()))
                .width(Length::FillPortion(3)),
            button(text(crate::i18n::t!("action-add")).size(13))
                .style(builtins::button::secondary)
                .on_press(super::Message::Inspector(hooks.on_add.clone())),
        ]
        .spacing(4)
        .align_y(Vertical::Center),
    );

    section.into()
}

/// "● Name" (or "Map", with no dot) in muted ink: the place a run of chips,
/// a suggestion or the tag input's destination belongs to.
fn place_label<'a>(area: &AreaCache, place: SourceId) -> ThemedElement<'a, super::Message> {
    let mut label = row![].spacing(4).align_y(Vertical::Center);
    if !place.is_map() {
        label = label.push(super::secrets::dot(
            smudgy_map_widget::sources::source_color(area, place),
        ));
    }
    label
        .push(
            text(super::secrets::place_name(area, place))
                .size(12)
                .style(muted_text),
        )
        .into()
}

/// One tag chip: the tag, with its count over the selection when several
/// rooms are selected, and × where the viewer may remove from its place.
fn tag_chip<'a>(
    place: SourceId,
    tag: &str,
    count: Option<(usize, usize)>,
    removable: bool,
) -> ThemedElement<'a, super::Message> {
    let mut label = match count {
        Some((count, total)) => format!("{tag}  {count}/{total}"),
        None => tag.to_string(),
    };
    if removable {
        label.push_str("  \u{00d7}");
    }
    let chip = button(text(label).size(12));
    if removable {
        chip.style(builtins::button::secondary)
            .on_press(super::Message::Inspector(Message::TagRemoved(
                place,
                tag.to_string(),
            )))
            .into()
    } else {
        chip.style(|theme: &crate::Theme, _status| {
            builtins::button::secondary(theme, iced::widget::button::Status::Active)
        })
        .into()
    }
}

/// The selected rooms' Tags block: one chip per (tag, place) in runs by
/// place (the rooms' own place, then "Add to", then color order), each run
/// led by its place; and the input adding a tag where it writes, with what
/// it would skip, its suggestions, and a hint when the typed tag so far
/// lives only somewhere more private. `None` without selected rooms. On a
/// map without places the chips are one plain list.
fn tags_block(window: &MapEditorWindow) -> Option<ThemedElement<'_, super::Message>> {
    let state = &window.inspector;
    let selection_tags = &state.tags;
    if selection_tags.rooms == 0 {
        return None;
    }
    let atlas = window.mapper.get_current_atlas();
    let area = atlas.get_area(&window.editor.area_id()?)?;
    let places_apply = window.secrets_apply();
    let index = window.tag_index(&area);
    let several = window.editor.selection().single().is_none();
    let own = match window.editor.selection().single() {
        Some(EntityId::SourceRoom(source, _)) => source,
        _ => SourceId::Map,
    };
    let order = if places_apply {
        super::tags::run_order(own, window.add_to(), index.places())
    } else {
        vec![own]
    };

    let mut block = Column::new()
        .spacing(4)
        .push(field_label(crate::i18n::t!("inspector-tags")));

    // The runs sit in one column of their own, so the input after them keeps
    // its place (and its focus) however many runs there are.
    let mut runs = Column::new().spacing(6);
    for place in order {
        let Some(tags) = selection_tags.in_place(place) else {
            continue;
        };
        let removable = super::secrets::can_remove(&area, place);
        let mut run: Vec<ThemedElement<'_, super::Message>> = Vec::new();
        if places_apply {
            run.push(
                container(place_label(&area, place))
                    .padding(Padding::ZERO.top(3.0))
                    .into(),
            );
        }
        for (tag, count) in tags {
            let count = several.then_some((*count, selection_tags.rooms));
            run.push(tag_chip(place, tag, count, removable));
        }
        runs = runs.push(wrap_row(run).spacing(6.0, 6.0));
    }
    block = block.push(runs);

    // Where the input writes, while the viewer may add there.
    let Some(destination) = window
        .tag_destination()
        .filter(|place| super::secrets::can_add(&area, *place))
    else {
        return Some(block.into());
    };
    let plan = super::tags::bulk_plan(
        selection_tags.map_rooms,
        &selection_tags.own_rooms,
        destination,
        area.keeps_only_own_rooms(destination),
    );
    let destination_name = super::secrets::place_name(&area, destination);
    if plan.applicable > 0 {
        let mut input_row = row![
            text_input(
                crate::i18n::ts!("inspector-add-tag-placeholder"),
                &state.tag_input
            )
            .id(super::tags::input_id(window.window_id))
            .size(13)
            .on_input(|value| super::Message::Inspector(Message::TagInputChanged(value)))
            .on_submit(super::Message::Inspector(Message::TagSubmitted))
            .width(Length::Fill),
        ]
        .spacing(6)
        .align_y(Vertical::Center);
        if places_apply {
            input_row = input_row
                .push(
                    text(crate::i18n::t!("inspector-tag-to"))
                        .size(12)
                        .style(muted_text),
                )
                .push(place_label(&area, destination));
        }
        if several {
            input_row = input_row.push(
                text(crate::i18n::t!("inspector-tag-on-rooms", "count" => plan.applicable))
                    .size(12)
                    .style(muted_text),
            );
        }
        block = block.push(input_row);

        let typed = super::tags::typed(&state.tag_input);
        match &typed {
            super::tags::Typed::TooLong => {
                block = block.push(
                    text(crate::i18n::t!(
                        "inspector-tag-too-long",
                        "limit" => super::tags::TAG_LIMIT
                    ))
                    .size(11)
                    .style(builtins::text::danger),
                );
            }
            super::tags::Typed::Tag(tag) => {
                if let Some(holder) = index.more_private_only(destination, tag) {
                    let holder_name = super::secrets::place_name(&area, holder);
                    let hint = if destination.is_map() {
                        crate::i18n::t!(
                            "inspector-tag-hint-map",
                            "tag" => tag.clone(),
                            "place" => holder_name
                        )
                    } else {
                        crate::i18n::t!(
                            "inspector-tag-hint-place",
                            "tag" => tag.clone(),
                            "place" => holder_name,
                            "destination" => destination_name.clone()
                        )
                    };
                    block = block.push(
                        row![
                            super::secrets::dot(smudgy_map_widget::sources::source_color(
                                &area, holder
                            )),
                            text(hint).size(11),
                        ]
                        .spacing(6)
                        .align_y(Vertical::Center),
                    );
                }
            }
            super::tags::Typed::Empty => {}
        }
    }

    // Before Enter, say which selected rooms can't carry the place's tags
    // (all of them, when the input is not offered).
    if several && !plan.skipped.is_empty() {
        let mut skipped = Column::new().spacing(2).push(
            text(crate::i18n::t!(
                "inspector-tag-adds-to",
                "count" => plan.applicable,
                "total" => selection_tags.rooms
            ))
            .size(11)
            .style(muted_text),
        );
        for (place, count) in &plan.skipped {
            skipped = skipped.push(
                row![
                    super::secrets::dot(smudgy_map_widget::sources::source_color(&area, *place)),
                    text(crate::i18n::t!(
                        "inspector-tag-cant-carry",
                        "count" => *count,
                        "place" => super::secrets::place_name(&area, *place),
                        "destination" => destination_name.clone()
                    ))
                    .size(11)
                    .style(muted_text),
                ]
                .spacing(6)
                .align_y(Vertical::Center),
            );
        }
        block = block.push(skipped);
    }

    let suggestions = window.tag_suggestions();
    if !suggestions.is_empty() {
        let mut list = Column::new().spacing(1);
        for suggestion in suggestions {
            let mut line = row![text(suggestion.tag.clone()).size(12).width(Length::Fill)]
                .spacing(4)
                .align_y(Vertical::Center);
            if places_apply {
                if suggestion.used_in.is_map() {
                    line = line.push(
                        text(crate::i18n::t!("inspector-tag-used-on-map"))
                            .size(11)
                            .style(muted_text),
                    );
                } else {
                    line = line
                        .push(
                            text(crate::i18n::t!("inspector-tag-used-in"))
                                .size(11)
                                .style(muted_text),
                        )
                        .push(place_label(&area, suggestion.used_in));
                }
            }
            list = list.push(
                button(line)
                    .style(builtins::button::list_item)
                    .width(Length::Fill)
                    .padding([2, 6])
                    .on_press(super::Message::Inspector(Message::TagSuggestionPicked(
                        suggestion.tag,
                    ))),
            );
        }
        block = block.push(list);
    }

    Some(block.into())
}

/// A room's fields. A source's room (`source`) names its source in its
/// source's color.
fn single_room_view<'a>(
    window: &'a MapEditorWindow,
    room_number: smudgy_cloud::RoomNumber,
    source: Option<smudgy_cloud::SourceId>,
) -> Column<'a, super::Message, crate::Theme> {
    let state = &window.inspector;

    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);
    content = content.push(room_heading(window, room_number, source));
    if let Some(field) = super::moves::in_field(window) {
        content = content.push(field);
    }

    content = content.push(labeled_input(
        crate::i18n::t!("inspector-title"),
        crate::i18n::ts!("inspector-room-title-placeholder"),
        &state.title,
        true,
        Message::TitleChanged,
    ));
    content = content.push(
        column![
            field_label(crate::i18n::t!("inspector-description")),
            text_editor(&state.description)
                .placeholder(crate::i18n::ts!("inspector-room-description-placeholder"))
                .size(14)
                .on_action(|action| {
                    super::Message::Inspector(Message::DescriptionEdited(action))
                }),
        ]
        .spacing(2),
    );
    content = content.push(labeled_input(
        crate::i18n::t!("inspector-level"),
        "0",
        &state.level,
        state.level.parse::<i32>().is_ok(),
        Message::LevelChanged,
    ));
    content = content.push(
        row![
            container(labeled_input(
                "X".to_string(),
                "0",
                &state.x,
                state.x.parse::<f32>().is_ok(),
                Message::XChanged,
            ))
            .width(Length::FillPortion(1)),
            container(labeled_input(
                "Y".to_string(),
                "0",
                &state.y,
                state.y.parse::<f32>().is_ok(),
                Message::YChanged,
            ))
            .width(Length::FillPortion(1)),
        ]
        .spacing(8),
    );
    content = content.push(
        row![
            container(labeled_input(
                crate::i18n::t!("inspector-color"),
                crate::i18n::ts!("inspector-default-placeholder"),
                &state.color,
                state.color.is_empty() || parse_color(&state.color).is_some(),
                Message::ColorChanged,
            ))
            .width(Length::Fill),
            column![
                space::vertical().height(14.0),
                swatch_button(window, &state.color, ColorField::Room),
            ],
        ]
        .spacing(8)
        .align_y(Vertical::Bottom),
    );
    if let Some(picker) = picker_for(window, ColorField::Room) {
        content = content.push(picker);
    }

    if let Some(tags) = tags_block(window) {
        content = content.push(tags);
    }

    content = content.push(properties_section(
        &state.properties,
        &state.new_property_name,
        &state.new_property_value,
        &PropertyHooks {
            on_value_change: Message::PropertyValueChanged,
            on_delete: Message::PropertyDeleted,
            on_new_name: Message::NewPropertyNameChanged,
            on_new_value: Message::NewPropertyValueChanged,
            on_add: Message::AddProperty,
        },
        window,
        source.unwrap_or(smudgy_cloud::SourceId::Map),
        Some((source.unwrap_or(smudgy_cloud::SourceId::Map), room_number)),
    ));

    // A map room's data in each place, then every link touching the room,
    // wherever it is kept.
    if let Some(places) = places_section(window) {
        content = content.push(places);
    }
    content = content.push(super::link_panel::room_exits(window));

    content
}

/// The selected map room's data in each Secret and Private: one group per
/// place that keeps properties or exits there, the place "Add to" points at
/// first and always shown, and an "Add" menu for the rest.
fn places_section(window: &MapEditorWindow) -> Option<ThemedElement<'_, super::Message>> {
    if !window.secrets_apply() {
        return None;
    }
    let atlas = window.mapper.get_current_atlas();
    let area = atlas.get_area(&window.editor.area_id()?)?;
    let add_to = window.add_to();
    let started = window.secrets.place_started;
    let places = &window.inspector.places;
    let shown = |place: &PlaceData| {
        place.has_data || place.source == add_to || Some(place.source) == started
    };
    let mut order: Vec<&PlaceData> = places
        .iter()
        .filter(|place| place.source == add_to)
        .collect();
    order.extend(
        places
            .iter()
            .filter(|place| place.source != add_to && shown(place)),
    );

    let mut section = Column::new().spacing(8);
    section = section.push(field_label(crate::i18n::t!("mapper-additional-room-data")));
    for place in order {
        let color = smudgy_map_widget::sources::source_color(&area, place.source);
        section = section.push(place_group(window, place, color));
    }

    let addable: Vec<&PlaceData> = places
        .iter()
        .filter(|place| !shown(place) && super::secrets::can_add(&area, place.source))
        .collect();
    if !addable.is_empty() {
        let open = window.secrets.place_menu_open;
        let trigger = button(text(format!("{} \u{25BE}", crate::i18n::t!("action-add"))).size(12))
            .style(builtins::button::subtle)
            .padding([3, 8])
            .on_press(super::Message::Inspector(Message::PlaceMenuToggled(!open)));
        let menu = open.then(|| {
            let mut list = Column::new().spacing(2);
            for place in addable {
                let color = smudgy_map_widget::sources::source_color(&area, place.source);
                list = list.push(
                    button(
                        row![
                            super::secrets::dot(color),
                            text(place.name.clone()).size(12)
                        ]
                        .spacing(6)
                        .align_y(Vertical::Center),
                    )
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(builtins::button::link)
                    .on_press(super::Message::Inspector(
                        Message::PlaceStarted(place.source),
                    )),
                );
            }
            container(list)
                .width(200)
                .padding(6)
                .style(builtins::container::card)
                .into()
        });
        section = section.push(crate::widgets::dropdown::Dropdown::new(
            trigger,
            menu,
            super::Message::Inspector(Message::PlaceMenuToggled(false)),
        ));
    }
    Some(section.into())
}

/// One place's data on the selected map room: its properties, editable
/// where the viewer may write there. Its tags show in the Tags block and
/// its links in the room's Exits.
fn place_group<'a>(
    window: &'a MapEditorWindow,
    place: &'a PlaceData,
    color: Option<iced::Color>,
) -> ThemedElement<'a, super::Message> {
    let source = place.source;
    let mut heading = row![].spacing(6).align_y(Vertical::Center);
    if !source.is_map() {
        heading = heading.push(super::secrets::dot(color));
    }
    let mut group = Column::new()
        .spacing(4)
        .push(heading.push(text(place.name.clone()).size(13)));
    for (index, property) in place.properties.iter().enumerate() {
        let value: ThemedElement<'_, super::Message> = if place.writable {
            text_input(
                crate::i18n::ts!("inspector-value-placeholder"),
                &property.value,
            )
            .size(13)
            .on_input(move |value| {
                super::Message::Inspector(Message::PlaceValueChanged(source, index, value))
            })
            .into()
        } else {
            text(property.value.clone()).size(13).into()
        };
        let mut line = row![
            text(property.name.clone())
                .size(13)
                .width(Length::FillPortion(2)),
            container(value).width(Length::FillPortion(3)),
        ]
        .spacing(4)
        .align_y(Vertical::Center);
        if place.writable {
            line = line.push(trash_button(super::Message::Inspector(
                Message::PlacePropertyDeleted(source, index),
            )));
        }
        let anchor = match window.editor.selection().single() {
            Some(EntityId::Room(number)) => Some((smudgy_cloud::SourceId::Map, number)),
            Some(EntityId::SourceRoom(source, number)) => Some((source, number)),
            _ => None,
        };
        if let Some((room_source, room_number)) = anchor
            && let Some(move_to) = super::moves::property_move(
                window,
                source,
                smudgy_cloud::mutation::PropertyAddress {
                    name: property.name.clone(),
                    room_number: Some(room_number),
                    room_source,
                },
            )
        {
            line = line.push(move_to);
        }
        group = group.push(line);
    }
    if place.writable {
        group = group.push(
            row![
                text_input(
                    crate::i18n::ts!("inspector-name-placeholder"),
                    &place.new_name
                )
                .size(13)
                .on_input(move |value| {
                    super::Message::Inspector(Message::PlaceNewNameChanged(source, value))
                })
                .width(Length::FillPortion(2)),
                text_input(
                    crate::i18n::ts!("inspector-value-placeholder"),
                    &place.new_value
                )
                .size(13)
                .on_input(move |value| {
                    super::Message::Inspector(Message::PlaceNewValueChanged(source, value))
                })
                .on_submit(super::Message::Inspector(Message::PlacePropertyAdded(
                    source
                )))
                .width(Length::FillPortion(3)),
                button(text(crate::i18n::t!("action-add")).size(13))
                    .style(builtins::button::secondary)
                    .on_press(super::Message::Inspector(Message::PlacePropertyAdded(
                        source
                    ))),
            ]
            .spacing(4)
            .align_y(Vertical::Center),
        );
    }
    container(group)
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 10.0,
            right: 0.0,
        })
        .into()
}

/// Localized names for the language-independent cloud enums the connection
/// editor displays. The cloud crate deliberately keeps Debug-style `Display`
/// impls; translation happens here at the UI boundary.
fn side_name(side: RoomSide) -> &'static str {
    match side {
        RoomSide::North => crate::i18n::ts!("side-north"),
        RoomSide::East => crate::i18n::ts!("side-east"),
        RoomSide::South => crate::i18n::ts!("side-south"),
        RoomSide::West => crate::i18n::ts!("side-west"),
    }
}

/// Wraps [`RoomSide`] so the endpoint pick_list renders translated wall names
/// while messages keep carrying the plain enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SideChoice(RoomSide);

impl fmt::Display for SideChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(side_name(self.0))
    }
}

/// A link setting's translated name.
trait Named: Copy {
    fn name(self) -> &'static str;
}

impl Named for ConnectionRouting {
    fn name(self) -> &'static str {
        match self {
            ConnectionRouting::Stub => crate::i18n::ts!("routing-stub"),
            ConnectionRouting::Simple => crate::i18n::ts!("routing-simple"),
            ConnectionRouting::Manual => crate::i18n::ts!("routing-manual"),
            ConnectionRouting::Automatic => crate::i18n::ts!("routing-automatic"),
        }
    }
}

impl Named for SegmentShape {
    fn name(self) -> &'static str {
        match self {
            SegmentShape::Direct => crate::i18n::ts!("segments-direct"),
            SegmentShape::Orthogonal => crate::i18n::ts!("inspector-connection-orthogonal"),
        }
    }
}

impl Named for CornerStyle {
    fn name(self) -> &'static str {
        match self {
            CornerStyle::Sharp => crate::i18n::ts!("corners-sharp"),
            CornerStyle::Rounded => crate::i18n::ts!("corners-rounded"),
        }
    }
}

impl Named for ConnectionDash {
    fn name(self) -> &'static str {
        match self {
            ConnectionDash::Solid => crate::i18n::ts!("dash-solid"),
            ConnectionDash::Dashed => crate::i18n::ts!("dash-dashed"),
            ConnectionDash::Dotted => crate::i18n::ts!("dash-dotted"),
        }
    }
}

const SIDE_CHOICES: [SideChoice; 4] = [
    SideChoice(RoomSide::North),
    SideChoice(RoomSide::East),
    SideChoice(RoomSide::South),
    SideChoice(RoomSide::West),
];

/// A link's appearance in the link editor, expanded: its route (Simple,
/// Manual, Automatic, as its kind and place allow), segments, corners,
/// line, colour (with Reset to its place's) and thickness; how an automatic
/// route stands; and its ports, which the canvas also drags.
#[allow(clippy::too_many_lines)]
pub(super) fn link_appearance<'a>(
    window: &'a MapEditorWindow,
    map: &AreaCache,
    connection_id: ConnectionId,
    writable: bool,
) -> ThemedElement<'a, super::Message> {
    use super::link_panel::{LinkMessage, labeled, segmented};
    let state = &window.inspector;
    let mut content = Column::new().spacing(8);
    let Some(document) = Document::of_connection(map, connection_id) else {
        return content.into();
    };
    let area = document.content();
    let Some(connection) = area.get_connection(connection_id) else {
        return content.into();
    };
    let place = document.source();
    let inspector = |message: Message| super::Message::Inspector(message);

    // The automatic router plans around the map's rooms, so it routes the
    // map's links only.
    let mut routes: Vec<ConnectionRouting> = [
        ConnectionRouting::Simple,
        ConnectionRouting::Manual,
        ConnectionRouting::Automatic,
    ]
    .into_iter()
    .filter(|routing| {
        connection.kind.allows_routing(*routing)
            && (place.is_map() || *routing != ConnectionRouting::Automatic)
    })
    .collect();
    if state.connection.routing == ConnectionRouting::Stub {
        routes.insert(0, ConnectionRouting::Stub);
    }
    let routes: Vec<(ConnectionRouting, &'static str)> = routes
        .into_iter()
        .map(|routing| (routing, routing.name()))
        .collect();
    content = content.push(labeled(
        crate::i18n::t!("inspector-connection-route"),
        segmented(
            &routes,
            state.connection.routing,
            writable,
            move |routing| inspector(Message::ConnectionRoutingChanged(routing)),
        ),
    ));
    let manual = state.connection.routing == ConnectionRouting::Manual;
    let routed = matches!(
        state.connection.routing,
        ConnectionRouting::Manual | ConnectionRouting::Automatic
    );
    let shapes: Vec<(SegmentShape, &'static str)> = SegmentShape::ALL
        .iter()
        .map(|shape| (*shape, shape.name()))
        .collect();
    content = content.push(labeled(
        crate::i18n::t!("link-segments"),
        segmented(
            &shapes,
            state.connection.segment_shape,
            writable && manual,
            move |shape| inspector(Message::ConnectionSegmentShapeChanged(shape)),
        ),
    ));
    let corners: Vec<(CornerStyle, &'static str)> = CornerStyle::ALL
        .iter()
        .map(|corner| (*corner, corner.name()))
        .collect();
    content = content.push(labeled(
        crate::i18n::t!("link-corners"),
        segmented(
            &corners,
            state.connection.corner,
            writable && routed,
            move |corner| inspector(Message::ConnectionCornerChanged(corner)),
        ),
    ));
    let dashes: Vec<(ConnectionDash, &'static str)> = ConnectionDash::ALL
        .iter()
        .map(|dash| (*dash, dash.name()))
        .collect();
    content = content.push(labeled(
        crate::i18n::t!("link-line"),
        segmented(&dashes, state.connection.dash, writable, move |dash| {
            inspector(Message::ConnectionDashChanged(dash))
        }),
    ));

    let color_valid = parse_color(&state.connection.color).is_some();
    let mut color_input = text_input(
        crate::i18n::ts!("inspector-css-color-placeholder"),
        &state.connection.color,
    )
    .size(12)
    .padding([4, 6])
    .width(Length::Fill);
    if writable {
        color_input =
            color_input.on_input(move |value| inspector(Message::ConnectionColorChanged(value)));
    }
    content = content.push(labeled(
        crate::i18n::t!("inspector-color"),
        row![
            swatch_button(window, &state.connection.color, ColorField::Connection),
            color_input,
            button(text(crate::i18n::t!("link-color-reset")).size(12))
                .style(builtins::button::link)
                .padding([2, 4])
                .on_press_maybe(
                    (writable && state.connection.color != DEFAULT_CONNECTION_COLOR)
                        .then_some(super::Message::Links(LinkMessage::ColorReset)),
                ),
        ]
        .spacing(6)
        .align_y(Vertical::Center),
    ));
    if !color_valid {
        content = content.push(
            text(crate::i18n::t!("inspector-invalid-value"))
                .size(11)
                .style(builtins::text::danger),
        );
    }
    if let Some(picker) = picker_for(window, ColorField::Connection) {
        content = content.push(picker);
    }
    let thickness = state.links.thickness.unwrap_or_else(|| {
        state
            .connection
            .thickness
            .parse::<f32>()
            .unwrap_or(DEFAULT_CONNECTION_THICKNESS)
    });
    let mut thickness_slider = iced::widget::slider(
        smudgy_cloud::THICKNESS_RANGE,
        thickness.clamp(
            *smudgy_cloud::THICKNESS_RANGE.start(),
            *smudgy_cloud::THICKNESS_RANGE.end(),
        ),
        |value| super::Message::Links(LinkMessage::Thickness(value)),
    )
    .step(0.25_f32);
    if writable {
        thickness_slider =
            thickness_slider.on_release(super::Message::Links(LinkMessage::ThicknessReleased));
    }
    content = content.push(labeled(
        crate::i18n::t!("link-thickness"),
        row![
            thickness_slider,
            text(format!("{thickness:.2}"))
                .size(12)
                .font(fonts::GEIST_MONO_VF)
                .width(40),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    ));

    // How an automatic route stands, and route upkeep.
    if connection.routing == ConnectionRouting::Automatic {
        if window.automatic_route_is_stale(connection_id) {
            content = content.push(
                text(crate::i18n::t!("inspector-connection-route-stale"))
                    .size(12)
                    .style(builtins::text::danger),
            );
        }
        match window.automatic_route_validation(connection_id) {
            Some(smudgy_cloud::automatic_routing::RouteValidation::Collision) => {
                content = content.push(
                    text(crate::i18n::t!("inspector-connection-route-collision"))
                        .size(12)
                        .style(builtins::text::danger),
                );
            }
            Some(smudgy_cloud::automatic_routing::RouteValidation::Invalid) => {
                content = content.push(
                    text(crate::i18n::t!("inspector-connection-route-invalid"))
                        .size(12)
                        .style(builtins::text::danger),
                );
            }
            Some(smudgy_cloud::automatic_routing::RouteValidation::Valid) | None => {}
        }
    }
    if !connection.route_points.is_empty()
        && matches!(
            connection.routing,
            ConnectionRouting::Stub | ConnectionRouting::Simple
        )
    {
        content = content.push(
            text(crate::i18n::t!("inspector-connection-route-inactive"))
                .size(12)
                .style(muted_text),
        );
    }
    let mut upkeep = row![].spacing(6);
    if connection.kind == smudgy_cloud::ConnectionKind::Internal && place.is_map() {
        upkeep = upkeep.push(
            button(text(crate::i18n::t!("inspector-connection-reroute")).size(12))
                .style(builtins::button::secondary)
                .on_press_maybe(writable.then_some(inspector(Message::ConnectionReroute))),
        );
    }
    upkeep = upkeep.push(
        button(text(crate::i18n::t!("inspector-connection-clear-route")).size(12))
            .style(builtins::button::secondary)
            .on_press_maybe(
                (writable && (!connection.route_points.is_empty() || routed))
                    .then_some(inspector(Message::ConnectionClearRoute)),
            ),
    );
    content = content.push(upkeep);

    // Ports: where the link meets each room's wall.
    let render_item = area
        .get_room_connections()
        .iter()
        .find(|item| item.connection_id == connection_id);
    let level_endpoint = |endpoint_b: bool| {
        render_item.is_some_and(|item| {
            let stub = if endpoint_b { item.stub_b } else { item.stub_a };
            smudgy_cloud::connection_geometry::renders_as_level_triangle(
                item.kind,
                item.routing,
                stub,
            )
        })
    };
    let atlas = window.mapper.get_current_atlas();
    let here = *map.get_id();
    let port = |endpoint_b: bool| -> ThemedElement<'a, super::Message> {
        let endpoint = if endpoint_b {
            connection.endpoint_b.unwrap_or(connection.endpoint_a)
        } else {
            connection.endpoint_a
        };
        let room = super::links::name(
            &atlas,
            here,
            super::links::doc_room(&document, endpoint.address()),
        );
        let mut col = Column::new()
            .spacing(4)
            .push(super::link_panel::named_line(&room, 11));
        if level_endpoint(endpoint_b) {
            return col
                .push(
                    text(crate::i18n::t!("inspector-connection-level-anchored"))
                        .size(12)
                        .style(muted_text),
                )
                .into();
        }
        let (side, offset_buffer) = if endpoint_b {
            (
                state.connection.endpoint_b_side,
                state.connection.endpoint_b_offset.clone(),
            )
        } else {
            (
                state.connection.endpoint_a_side,
                state.connection.endpoint_a_offset.clone(),
            )
        };
        let offset_valid = offset_buffer
            .parse::<f32>()
            .is_ok_and(|offset| (0.0..=1.0).contains(&offset));
        col = col.push(
            row![
                pick_list(&SIDE_CHOICES[..], Some(SideChoice(side)), move |choice| {
                    inspector(Message::ConnectionEndpointSideChanged(endpoint_b, choice.0))
                })
                .text_size(12)
                .width(Length::FillPortion(2)),
                text_input(
                    crate::i18n::ts!("inspector-connection-port-placeholder"),
                    &offset_buffer
                )
                .on_input(
                    move |value| inspector(Message::ConnectionEndpointOffsetChanged(
                        endpoint_b, value
                    ))
                )
                .size(12)
                .width(Length::FillPortion(1)),
                button(text(crate::i18n::t!("inspector-connection-auto")).size(11))
                    .style(builtins::button::secondary)
                    .on_press(inspector(Message::ConnectionEndpointReset(endpoint_b))),
                button(text(crate::i18n::t!("inspector-connection-redistribute")).size(11))
                    .style(builtins::button::secondary)
                    .on_press_maybe(
                        (!redistribute_port_updates(area, endpoint.address(), endpoint.side)
                            .is_empty())
                        .then_some(inspector(Message::ConnectionRedistributePorts(endpoint_b)))
                    ),
            ]
            .spacing(6),
        );
        if !offset_valid {
            col = col.push(
                text(crate::i18n::t!("inspector-connection-port-invalid"))
                    .size(11)
                    .style(builtins::text::danger),
            );
        }
        col.into()
    };
    content = content.push(field_label(crate::i18n::t!("link-ports")));
    content = content.push(port(false));
    if state.connection.has_endpoint_b {
        content = content.push(port(true));
    }
    content = content.push(
        button(text(crate::i18n::t!("inspector-connection-reset")).size(12))
            .style(builtins::button::secondary)
            .on_press_maybe(writable.then_some(inspector(Message::ConnectionReset))),
    );
    content.into()
}

/// The shared x/y/width/height grid for labels and shapes.
fn bounds_fields<'a>(
    x: &'a str,
    y: &'a str,
    width: &'a str,
    height: &'a str,
    on_change: fn(BoundsField, String) -> Message,
) -> ThemedElement<'a, super::Message> {
    let bound_input = move |label: String, value: &'a str, field: BoundsField| {
        container(labeled_input(
            label,
            "0",
            value,
            value.parse::<f32>().is_ok(),
            move |v| on_change(field, v),
        ))
        .width(Length::FillPortion(1))
    };

    column![
        row![
            bound_input("X".to_string(), x, BoundsField::X),
            bound_input("Y".to_string(), y, BoundsField::Y),
        ]
        .spacing(8),
        row![
            bound_input(
                crate::i18n::t!("inspector-width"),
                width,
                BoundsField::Width
            ),
            bound_input(
                crate::i18n::t!("inspector-height"),
                height,
                BoundsField::Height
            ),
        ]
        .spacing(8),
    ]
    .spacing(FIELD_SPACING)
    .into()
}

fn color_input<'a>(
    window: &'a MapEditorWindow,
    field: ColorField,
    label: String,
    placeholder: &'static str,
    value: &'a str,
    allow_empty: bool,
    on_input: impl Fn(String) -> Message + 'a,
) -> ThemedElement<'a, super::Message> {
    let valid = (allow_empty && value.is_empty()) || parse_color(value).is_some();
    let mut col = column![
        row![
            container(labeled_input(label, placeholder, value, valid, on_input))
                .width(Length::Fill),
            column![
                space::vertical().height(14.0),
                swatch_button(window, value, field),
            ],
        ]
        .spacing(8)
        .align_y(Vertical::Bottom),
    ]
    .spacing(FIELD_SPACING);

    if let Some(picker) = picker_for(window, field) {
        col = col.push(picker);
    }

    col.into()
}

fn label_view(window: &MapEditorWindow) -> Column<'_, super::Message, crate::Theme> {
    let state = &window.inspector.label;

    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);
    content = content.push(heading(crate::i18n::t!("inspector-label")));
    if let Some(field) = super::moves::in_field(window) {
        content = content.push(field);
    }

    content = content.push(labeled_input(
        crate::i18n::t!("inspector-label-text"),
        crate::i18n::ts!("inspector-label-text-placeholder"),
        &state.text,
        true,
        Message::LabelTextChanged,
    ));
    content = content.push(color_input(
        window,
        ColorField::LabelText,
        crate::i18n::t!("inspector-color"),
        crate::i18n::ts!("inspector-default-placeholder"),
        &state.color,
        false,
        Message::LabelColorChanged,
    ));
    content = content.push(color_input(
        window,
        ColorField::LabelBackground,
        crate::i18n::t!("inspector-background"),
        crate::i18n::ts!("inspector-none-placeholder"),
        &state.background,
        true,
        Message::LabelBackgroundChanged,
    ));
    content = content.push(
        row![
            container(labeled_input(
                crate::i18n::t!("inspector-font-size"),
                "16",
                &state.font_size,
                state.font_size.parse::<i32>().is_ok_and(|v| v > 0),
                Message::LabelFontSizeChanged,
            ))
            .width(Length::FillPortion(1)),
            container(labeled_input(
                crate::i18n::t!("inspector-font-weight"),
                "400",
                &state.font_weight,
                state.font_weight.parse::<i32>().is_ok_and(|v| v > 0),
                Message::LabelFontWeightChanged,
            ))
            .width(Length::FillPortion(1)),
        ]
        .spacing(8),
    );
    content = content.push(
        column![
            field_label(crate::i18n::t!("inspector-alignment")),
            row![
                pick_list(
                    &HorizontalAlignment::ALL[..],
                    Some(state.horizontal_alignment.clone()),
                    |alignment| {
                        super::Message::Inspector(Message::LabelHorizontalAlignmentChanged(
                            alignment,
                        ))
                    }
                )
                .text_size(12)
                .width(Length::FillPortion(1)),
                pick_list(
                    &VerticalAlignment::ALL[..],
                    Some(state.vertical_alignment.clone()),
                    |alignment| {
                        super::Message::Inspector(Message::LabelVerticalAlignmentChanged(alignment))
                    }
                )
                .text_size(12)
                .width(Length::FillPortion(1)),
            ]
            .spacing(8),
        ]
        .spacing(2),
    );
    content = content.push(bounds_fields(
        &state.x,
        &state.y,
        &state.width,
        &state.height,
        Message::LabelBoundsChanged,
    ));

    content
}

fn shape_view(window: &MapEditorWindow) -> Column<'_, super::Message, crate::Theme> {
    let state = &window.inspector.shape;

    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);
    content = content.push(heading(crate::i18n::t!("inspector-shape")));
    if let Some(field) = super::moves::in_field(window) {
        content = content.push(field);
    }

    content = content.push(
        column![
            field_label(crate::i18n::t!("inspector-shape")),
            pick_list(
                &ShapeType::ALL[..],
                Some(state.shape_type.clone()),
                |shape_type| super::Message::Inspector(Message::ShapeTypeChanged(shape_type))
            )
            .text_size(12)
            .width(Length::Fill),
        ]
        .spacing(2),
    );
    content = content.push(color_input(
        window,
        ColorField::ShapeFill,
        crate::i18n::t!("inspector-fill"),
        crate::i18n::ts!("inspector-none-placeholder"),
        &state.background,
        true,
        Message::ShapeBackgroundChanged,
    ));
    content = content.push(color_input(
        window,
        ColorField::ShapeStroke,
        crate::i18n::t!("inspector-stroke"),
        crate::i18n::ts!("inspector-none-placeholder"),
        &state.stroke_color,
        true,
        Message::ShapeStrokeColorChanged,
    ));
    content = content.push(
        row![
            container(labeled_input(
                crate::i18n::t!("inspector-stroke-width"),
                "1",
                &state.stroke_width,
                state.stroke_width.parse::<f32>().is_ok_and(|v| v >= 0.0),
                Message::ShapeStrokeWidthChanged,
            ))
            .width(Length::FillPortion(1)),
            container(labeled_input(
                crate::i18n::t!("inspector-corner-radius"),
                "0",
                &state.border_radius,
                state.border_radius.parse::<f32>().is_ok_and(|v| v >= 0.0),
                Message::ShapeBorderRadiusChanged,
            ))
            .width(Length::FillPortion(1)),
        ]
        .spacing(8),
    );
    content = content.push(bounds_fields(
        &state.x,
        &state.y,
        &state.width,
        &state.height,
        Message::ShapeBoundsChanged,
    ));

    content
}

fn multi_selection_view(window: &MapEditorWindow) -> Column<'_, super::Message, crate::Theme> {
    let state = &window.inspector;
    let selection = window.editor.selection();

    let rooms = selection.rooms().count() + selection.source_rooms().count();
    let connections = selection.connections().count();
    let labels = selection.labels().count();
    let shapes = selection.shapes().count();

    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);
    content = content.push(heading(crate::i18n::t!(
        "inspector-selected",
        "count" => selection.len()
    )));
    if let Some(field) = super::moves::in_field(window) {
        content = content.push(field);
    }
    content = content.push(
        text(crate::i18n::t!(
            "inspector-selection-counts",
            "links" => connections,
            "rooms" => rooms,
            "labels" => labels,
            "shapes" => shapes
        ))
        .size(13),
    );

    if rooms > 0 {
        // An empty buffer means the rooms either disagree ("(mixed)") or
        // all have no color ("(default)") — never a fake value.
        let color_placeholder = if state.bulk_color_mixed {
            crate::i18n::ts!("inspector-mixed-placeholder")
        } else {
            crate::i18n::ts!("inspector-default-placeholder")
        };
        content = content.push(
            column![
                field_label(crate::i18n::t!("inspector-set-color")),
                row![
                    text_input(color_placeholder, &state.bulk_color)
                        .size(14)
                        .on_input(|value| {
                            super::Message::Inspector(Message::BulkColorChanged(value))
                        })
                        .on_submit(super::Message::Inspector(Message::ApplyBulkColor))
                        .width(Length::Fill),
                    swatch_button(window, &state.bulk_color, ColorField::Bulk),
                ]
                .spacing(8)
                .align_y(Vertical::Center),
            ]
            .spacing(2),
        );
        if let Some(picker) = picker_for(window, ColorField::Bulk) {
            content = content.push(picker);
        }

        let level_placeholder = if state.bulk_level_mixed {
            crate::i18n::ts!("inspector-mixed-placeholder")
        } else {
            "0"
        };
        content = content.push(
            column![
                field_label(crate::i18n::t!("inspector-set-level")),
                text_input(level_placeholder, &state.bulk_level)
                    .size(14)
                    .on_input(|value| {
                        super::Message::Inspector(Message::BulkLevelChanged(value))
                    })
                    .on_submit(super::Message::Inspector(Message::ApplyBulkLevel)),
            ]
            .spacing(2),
        );
        if let Some(tags) = tags_block(window) {
            content = content.push(tags);
        }
    }

    content
}

/// The "Copies of this map" section: one row per cache-resident family
/// member showing its active/inactive status, with a "Use only this copy"
/// helper on every member that isn't already the sole active one. `None`
/// when the family has fewer than two members.
fn copies_section<'a>(
    window: &MapEditorWindow,
    area_id: AreaId,
) -> Option<ThemedElement<'a, super::Message>> {
    let atlas = window.mapper.get_current_atlas();
    let family = window.copy_family(area_id);
    if family.len() < 2 {
        return None;
    }

    let enabled_count = family
        .iter()
        .filter(|id| window.mapper.is_area_enabled(id))
        .count();

    let mut section = Column::new()
        .spacing(4)
        .push(field_label(crate::i18n::t!("inspector-copies")));

    for member in &family {
        let Some(member_area) = atlas.get_area(member) else {
            continue;
        };
        let enabled = window.mapper.is_area_enabled(member);
        let is_current = *member == area_id;

        let name = member_area.get_name().to_string();
        let label = if is_current {
            crate::i18n::t!("inspector-this-map-suffix", "name" => name)
        } else {
            name
        };
        let name_text = if enabled {
            text(label).size(13)
        } else {
            text(label).size(13).style(muted_text)
        };

        let mut row_widgets = row![name_text].spacing(6).align_y(Vertical::Center);
        row_widgets = row_widgets.push(if enabled {
            text(crate::i18n::t!("inspector-active"))
                .size(11)
                .style(builtins::text::success)
        } else {
            text(crate::i18n::t!("inspector-inactive"))
                .size(11)
                .style(muted_text)
        });
        row_widgets = row_widgets.push(space::horizontal());

        // "Use only this copy" activates this member and deactivates its
        // siblings in one click. Offered on every member that isn't already
        // the sole active copy (where it would be a no-op).
        if !(enabled && enabled_count == 1) {
            row_widgets = row_widgets.push(
                button(text(crate::i18n::t!("inspector-use-only-copy")).size(11))
                    .style(builtins::button::secondary)
                    .on_press(super::Message::SetActiveCopy(*member)),
            );
        }

        section = section.push(row_widgets);
    }

    section = section.push(
        text(crate::i18n::t!("inspector-multiple-copies-warning"))
            .size(11)
            .style(muted_text),
    );

    Some(section.into())
}

/// The map panel's data fields: where the map was copied from and who
/// shared it, its copies, and its properties, which the viewer edits where
/// they may edit the map.
pub(super) fn data_fields<'a>(
    window: &'a MapEditorWindow,
    area: &AreaCache,
) -> Column<'a, super::Message, crate::Theme> {
    let atlas = window.mapper.get_current_atlas();
    let state = &window.inspector;
    let mut content = Column::new().spacing(FIELD_SPACING);

    // A view-only map: who shared it, and its properties to read.
    if !area.effective_access().can_edit {
        content = content.push(
            text(crate::i18n::t!(
                "inspector-view-only",
                "attribution" => window.sharer_attribution(*area.get_id())
            ))
            .size(12)
            .style(muted_text),
        );
        let mut properties: Vec<(String, String)> = area
            .properties()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        if !properties.is_empty() {
            properties.sort();
            let mut list = Column::new()
                .spacing(4)
                .push(field_label(crate::i18n::t!("inspector-area-properties")));
            for (name, value) in properties {
                list = list.push(text(format!("{name}: {value}")).size(12));
            }
            content = content.push(list);
        }
        return content;
    }

    // Clone provenance (owner-only data; the server omits it otherwise).
    if area.is_owned()
        && let Some(source_id) = area.meta().copied_from_area_id
    {
        let source = atlas.get_area(&source_id);
        let source_name = source.as_ref().map_or_else(
            || crate::i18n::t!("inspector-shared-map"),
            |source| format!("\u{201c}{}\u{201d}", source.get_name()),
        );
        let mut line = crate::i18n::t!("inspector-copied-from", "source" => source_name);
        if let Some(rev) = area.meta().copied_from_rev {
            line.push(' ');
            line.push_str(&crate::i18n::t!("inspector-copy-revision", "revision" => rev));
        }
        if let Some(copied_at) = area.meta().copied_at {
            line.push(' ');
            line.push_str(&crate::i18n::t!(
                "inspector-copy-date",
                "date" => copied_at.format("%Y-%m-%d").to_string()
            ));
        }
        // Rev is opaque — inequality means "changed", never a count.
        if let (Some(source), Some(rev)) = (source.as_ref(), area.meta().copied_from_rev)
            && source.get_rev() != rev
        {
            line.push(' ');
            line.push_str(&crate::i18n::t!("inspector-source-changed"));
        }
        content = content.push(text(line).size(12).style(muted_text));
    }

    // A shared map the viewer may edit names who shared it here.
    if !area.is_owned() {
        content = content.push(
            text(window.sharer_attribution(*area.get_id()))
                .size(12)
                .style(muted_text),
        );
    }

    // Copy family: when ≥2 cache-resident clones share an ancestry, let the
    // user pick which one is active for room identification.
    if let Some(section) = copies_section(window, *area.get_id()) {
        content = content.push(section);
    }

    content.push(properties_section(
        &state.area_properties,
        &state.new_area_property_name,
        &state.new_area_property_value,
        &PropertyHooks {
            on_value_change: Message::AreaPropertyValueChanged,
            on_delete: Message::AreaPropertyDeleted,
            on_new_name: Message::NewAreaPropertyNameChanged,
            on_new_value: Message::NewAreaPropertyValueChanged,
            on_add: Message::AddAreaProperty,
        },
        window,
        smudgy_cloud::SourceId::Map,
        None,
    ))
}

/// The inspector pane's id, scoping Tab's moves between its inputs.
fn pane_id(window: iced::window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("inspector-pane-{window:?}"))
}

/// Moves focus to the inspector's next input (or, `back`, its previous
/// one); from nowhere, to its first (or last).
pub(super) fn focus_step(window: iced::window::Id, back: bool) -> Task<super::Message> {
    use iced::advanced::widget::operation::{focusable, scope};
    let step: Box<dyn iced::advanced::widget::Operation<()>> = if back {
        Box::new(focusable::focus_previous::<()>())
    } else {
        Box::new(focusable::focus_next::<()>())
    };
    iced::advanced::widget::operate(scope(pane_id(window), step)).discard()
}

/// The inspector pane: an atlas's panel while one is chosen in the map list,
/// else the open map's panel, a Secret's or Private's page, or the canvas
/// selection's view under a link back to the map's panel.
pub fn view(window: &MapEditorWindow) -> ThemedElement<'_, super::Message> {
    let atlas = window.mapper.get_current_atlas();
    let area = window.editor.area_id().and_then(|id| atlas.get_area(&id));

    let content: Column<'_, super::Message, crate::Theme> =
        match (super::panels::shown(window, area.is_some()), area.as_ref()) {
            (Panel::Atlas(atlas_id), _) => super::atlas_panel::view(window, atlas_id),
            (Panel::Map, Some(area)) => super::map_panel::view(window, area),
            (Panel::Place(source), Some(area)) => Column::new()
                .push(super::panels::back_link(area.get_name()))
                .push(super::secrets::place_page(window, area, source)),
            (Panel::Selection, Some(area)) => Column::new()
                .push(super::panels::back_link(area.get_name()))
                .push(selection_view(window)),
            (Panel::NoMap | Panel::Map | Panel::Place(_) | Panel::Selection, _) => Column::new()
                .padding(12)
                .push(text(crate::i18n::t!("inspector-no-area-selected"))),
        };

    container(scrollable(content).height(Length::Fill))
        .id(pane_id(window.window_id))
        .style(builtins::container::opaque)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The canvas selection's view: read-only where the viewer can't change
/// what is selected where it lives. View-only maps swap the editable forms
/// for a read-only summary; mutations are also gated centrally in mod.rs
/// (push_command / handle_mutation_request), so this is presentation, not
/// enforcement.
fn selection_view(window: &MapEditorWindow) -> Column<'_, super::Message, crate::Theme> {
    if !window.selection_writable() {
        return read_only_view(window);
    }
    match window.editor.selection().single() {
        Some(EntityId::Connection(_)) => super::link_panel::link_editor(window),
        Some(EntityId::Room(room_number)) => single_room_view(window, room_number, None),
        Some(EntityId::SourceRoom(source, room_number)) => {
            single_room_view(window, room_number, Some(source))
        }
        Some(EntityId::Label(_)) => label_view(window),
        Some(EntityId::Shape(_)) => shape_view(window),
        None => multi_selection_view(window),
    }
}

fn muted_text(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

/// A room's title, description, position and properties, read-only, below
/// its heading in `content`. A room of any place reads the same way.
fn read_only_room<'a>(
    mut content: Column<'a, super::Message, crate::Theme>,
    room: &smudgy_cloud::mapper::room_cache::RoomCache,
    window: &'a MapEditorWindow,
    source: smudgy_cloud::SourceId,
    room_number: smudgy_cloud::RoomNumber,
) -> Column<'a, super::Message, crate::Theme> {
    if !room.get_title().is_empty() {
        content = content.push(text(room.get_title().to_string()).size(13));
    }
    if !room.get_description().is_empty() {
        content = content.push(text(room.get_description().to_string()).size(12));
    }
    content = content.push(
        text(crate::i18n::t!(
            "inspector-room-position",
            "level" => room.get_level(),
            "x" => format!("{:.1}", room.get_x()),
            "y" => format!("{:.1}", room.get_y())
        ))
        .size(12)
        .style(muted_text),
    );
    let mut properties: Vec<(String, String)> = room
        .properties()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    if !properties.is_empty() {
        properties.sort();
        content = content.push(field_label(crate::i18n::t!("inspector-properties")));
        for (name, value) in properties {
            let mut property = row![text(format!("{name}: {value}")).size(12)]
                .spacing(6)
                .align_y(Vertical::Center);
            if let Some(move_to) = super::moves::property_move(
                window,
                source,
                smudgy_cloud::mutation::PropertyAddress {
                    name,
                    room_number: Some(room_number),
                    room_source: source,
                },
            ) {
                property = property.push(move_to);
            }
            content = content.push(property);
        }
    }
    content
}

/// A selection the viewer can't change where it lives: an attribution
/// banner plus read-only summaries of the selection — no inputs at all.
#[allow(clippy::too_many_lines)]
fn read_only_view(window: &MapEditorWindow) -> Column<'_, super::Message, crate::Theme> {
    let atlas = window.mapper.get_current_atlas();
    let mut content = Column::new().padding(12).spacing(8);

    let Some(area) = window.editor.area_id().and_then(|id| atlas.get_area(&id)) else {
        return content.push(text(crate::i18n::t!("inspector-no-area-selected")));
    };

    // Attribution enriched from the sharer index: names the re-sharer and the
    // underlying owner when they differ.
    let attribution = window.sharer_attribution(*area.get_id());
    content = content.push(
        text(crate::i18n::t!(
            "inspector-view-only",
            "attribution" => attribution
        ))
        .size(12)
        .style(muted_text),
    );
    content = content.push(rule::horizontal(1));

    let selection = window.editor.selection();
    // The selection's own lines sit in one column, so the Tags block after
    // them keeps its place (and its input's focus) as they change.
    let mut details = Column::new().spacing(8);
    if let Some(entity) = selection.single() {
        match entity {
            EntityId::Connection(_) => {
                details = details.push(super::link_panel::read_only_link(window));
            }
            EntityId::SourceRoom(source, room_number) => {
                if let Some(room) =
                    smudgy_map_widget::sources::source_room(&area, source, room_number)
                {
                    details = details.push(room_heading(window, room_number, Some(source)));
                    details = read_only_room(details, room, window, source, room_number);
                    if let Some(links) = super::link_panel::read_only_links(
                        window,
                        smudgy_map_widget::map_editor::PlacedRoom {
                            source,
                            number: room_number,
                        },
                    ) {
                        details = details.push(links);
                    }
                }
            }
            EntityId::Room(room_number) => {
                if let Some(room) = area.get_room(&room_number) {
                    details = details.push(room_heading(window, room_number, None));
                    details = read_only_room(
                        details,
                        room,
                        window,
                        smudgy_cloud::SourceId::Map,
                        room_number,
                    );
                    if let Some(links) = super::link_panel::read_only_links(
                        window,
                        smudgy_map_widget::map_editor::PlacedRoom::map(room_number),
                    ) {
                        details = details.push(links);
                    }
                }
            }
            EntityId::Label(label_id) => {
                if let Some((_, label)) = area.find_label(&label_id) {
                    details = details.push(heading(crate::i18n::t!("inspector-label")));
                    details = details.push(text(label.text.clone()).size(13));
                }
            }
            EntityId::Shape(shape_id) => {
                if let Some((_, shape)) = area.find_shape(&shape_id) {
                    details = details.push(heading(crate::i18n::t!("inspector-shape")));
                    details = details.push(
                        text(crate::i18n::t!(
                            "inspector-shape-summary",
                            "width" => format!("{:.0}", shape.width),
                            "height" => format!("{:.0}", shape.height),
                            "x" => format!("{:.1}", shape.x),
                            "y" => format!("{:.1}", shape.y)
                        ))
                        .size(12)
                        .style(muted_text),
                    );
                }
            }
        }
    } else {
        details = details.push(
            text(crate::i18n::t!(
                "inspector-entities-selected",
                "count" => selection.len()
            ))
            .size(13),
        );
    }

    content = content.push(details);
    if let Some(tags) = tags_block(window) {
        content = content.push(tags);
    }

    // A map room the viewer can't edit can still carry their Private notes
    // and what Secrets they may write keep for it.
    if matches!(
        selection.single(),
        Some(EntityId::Room(_) | EntityId::SourceRoom(..))
    ) && let Some(places) = places_section(window)
    {
        content = content.push(places);
    }

    content
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn secret_room_inspector_shows_map_owned_notes_separately() {
        let (mapper, area_id, secret) = super::super::source_rooms::tests::loaded().await;
        let target = super::SourceRoomRef::data_on_source(
            area_id,
            smudgy_cloud::SourceId::Map,
            secret,
            smudgy_cloud::RoomNumber(2),
        );
        let command = super::source_rooms::set_property(
            &mapper.get_current_atlas(),
            target,
            "notes".into(),
            "Map-owned note".into(),
        )
        .unwrap();
        let mut stack = super::super::commands::CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        assert_eq!(stack.take_last_error(), None);
        let area = mapper.get_current_atlas().get_area(&area_id).unwrap();
        let places = super::place_data(&area, secret, smudgy_cloud::RoomNumber(2));
        let note = places.iter().find(|place| place.source.is_map()).unwrap();
        assert!(note.has_data);
        assert_eq!(note.properties[0].value, "Map-owned note");
        assert!(
            !places.iter().any(|place| place.source == secret),
            "own properties stay in the main section"
        );
        let ordinary = super::place_data(
            &area,
            smudgy_cloud::SourceId::Map,
            smudgy_cloud::RoomNumber(2),
        );
        assert!(
            !ordinary
                .iter()
                .flat_map(|place| &place.properties)
                .any(|property| property.value == "Map-owned note")
        );
    }
    use super::*;

    #[tokio::test]
    async fn a_rebuild_keeps_the_tag_input_while_the_selection_holds() {
        let (mapper, area_id, secret) = super::super::source_rooms::tests::loaded().await;
        let mut editor = MapEditor::new(mapper.clone(), Some(area_id));
        editor.select(EntityId::Room(RoomNumber(2)));
        let mut state = State::default();
        state.resync(&mapper, &editor);
        // The Secret's tag on the map room reads under the Secret.
        assert_eq!(state.tags.in_place(secret).map(|tags| tags.len()), Some(1));

        state.tag_input = "pea".to_string();
        state.resync(&mapper, &editor);
        assert_eq!(state.tag_input, "pea", "an outside change keeps the input");

        // A place changing alone re-reads the tags, nothing else.
        let area = mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("loaded");
        let change = source_rooms::change_tags(
            &area,
            secret,
            &[(SourceId::Map, RoomNumber(2))],
            "wine",
            true,
            "Hidden",
        )
        .expect("room 2 lacks it");
        let mut stack = commands::CommandStack::default();
        let _ = stack.push_and_apply(&mapper, change.command);
        state.new_property_name = "draft".to_string();
        state.reread_tags(&mapper, &editor);
        assert_eq!(state.tags.in_place(secret).map(|tags| tags.len()), Some(2));
        assert_eq!(state.tag_input, "pea");
        assert_eq!(state.new_property_name, "draft");

        editor.select(EntityId::Room(RoomNumber(1)));
        state.resync(&mapper, &editor);
        assert!(state.tag_input.is_empty(), "another room starts over");
    }

    /// A room of another place is headed by the place's name and the room's
    /// number in it, in every language.
    #[test]
    fn a_place_rooms_heading_names_the_place_and_number() {
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            let heading = smudgy_i18n::t!(
                translator,
                "inspector-place-room-heading",
                "place" => "Bookcase",
                "number" => "1"
            );
            assert!(
                heading.contains("Bookcase") && heading.contains("#1") && !heading.contains('⟦'),
                "{}: {heading}",
                catalog.tag
            );
            let room = smudgy_i18n::t!(
                translator,
                "mapper-room-name-titled",
                "number" => "1",
                "title" => "Hidden Study"
            );
            assert!(
                room.contains("#1") && room.contains("Hidden Study") && !room.contains('⟦'),
                "{}: {room}",
                catalog.tag
            );
        }
    }
}
