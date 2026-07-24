//! The inspector pane: editable forms for the current selection.
//!
//! Field buffers live in [`State`] so the user can type freely (including
//! transiently invalid numbers); every valid change commits immediately
//! through the window's command stack with per-field coalescing, so a
//! typing burst is one undo step. Buffers resync from the cache when the
//! selection changes, after undo/redo, and when another writer bumps the
//! area revision — but not on the echo of the user's own commits.

use std::fmt;

use iced::widget::{
    Column, button, checkbox, column, container, pick_list, row, rule, scrollable, space, text,
    text_editor, text_input, tooltip,
};
use iced::{Length, Task, alignment::Vertical};
use smudgy_cloud::cloud_api::{RoomPropertyRef, SecretMarksRequest, SecretMarksResult};
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::mapper::exit_cache::ExitCache;
use smudgy_cloud::mapper::room_cache::PropertyEntry;
use smudgy_cloud::mapper::{AtlasCache, RoomKey};
use smudgy_cloud::{
    AreaId, CloudError, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionRouting,
    ConnectionUpdates, CornerStyle, DEFAULT_CONNECTION_COLOR, DEFAULT_CONNECTION_THICKNESS,
    ExitDirection, ExitId, HorizontalAlignment, LabelId, LabelUpdates, Mapper, RoomNumber,
    RoomSide, RoomUpdates, SegmentShape, ShapeId, ShapeType, ShapeUpdates, VerticalAlignment,
};
use smudgy_map_widget::map_editor::{EntityId, MapEditor};
use smudgy_map_widget::render::parse_color;

use crate::assets::{bootstrap_icons, fonts};
use crate::components::color_picker::{self, ColorPicker};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;
use crate::widgets::wrap_row::wrap_row;

use super::commands::FieldId;
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
    if connection.segment_shape == SegmentShape::Orthogonal
        && matches!(
            connection.routing,
            ConnectionRouting::Manual | ConnectionRouting::Automatic
        )
    {
        let render = area.get_room_connections().iter().find(|item| {
            item.connection_id == connection_id && item.geometry.stub_tip_b.is_some()
        })?;
        let room = area.get_room(&endpoint.room_number)?;
        let new_tip = smudgy_cloud::connection_geometry::stub_tip(
            smudgy_cloud::connection_geometry::port_position(
                smudgy_cloud::MapPoint::new(room.get_x(), room.get_y()),
                endpoint.side,
                endpoint.port_offset,
            ),
            endpoint.side,
        );
        let mut points = connection.route_points.clone();
        if endpoint_b {
            let old_tip = render.geometry.stub_tip_b?;
            if let Some(last) = points.last_mut() {
                if (last.y - old_tip.y).abs() <= (last.x - old_tip.x).abs() {
                    last.y = new_tip.y;
                } else {
                    last.x = new_tip.x;
                }
            } else if (render.geometry.stub_tip_a.x - new_tip.x).abs() > f32::EPSILON
                && (render.geometry.stub_tip_a.y - new_tip.y).abs() > f32::EPSILON
            {
                points.push(smudgy_cloud::MapPoint::new(
                    render.geometry.stub_tip_a.x,
                    new_tip.y,
                ));
            }
        } else if let Some(first) = points.first_mut() {
            if (first.y - render.geometry.stub_tip_a.y).abs()
                <= (first.x - render.geometry.stub_tip_a.x).abs()
            {
                first.y = new_tip.y;
            } else {
                first.x = new_tip.x;
            }
        } else if let Some(other_tip) = render.geometry.stub_tip_b
            && (new_tip.x - other_tip.x).abs() > f32::EPSILON
            && (new_tip.y - other_tip.y).abs() > f32::EPSILON
        {
            points.push(smudgy_cloud::MapPoint::new(new_tip.x, other_tip.y));
        }
        route_points = Some(points);
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
        if other.room_number == endpoint.room_number {
            let outward = other.side.outward();
            return if axis_x { outward.x } else { outward.y };
        }
        if let (Some(room), Some(partner)) = (
            area.get_room(&endpoint.room_number),
            area.get_room(&other.room_number),
        ) {
            return if axis_x {
                partner.get_x() - room.get_x()
            } else {
                partner.get_y() - room.get_y()
            };
        }
    }
    area.get_room(&endpoint.room_number)
        .and_then(|room| {
            room.get_exits()
                .iter()
                .filter(|exit| exit.connection_id == connection.id)
                .min_by_key(|exit| exit.from_direction.to_string())
        })
        .map_or(0.0, |exit| direction_component(exit.from_direction, axis_x))
}

/// Computes the deterministic preview/commit payload for an explicit wall
/// redistribution. Manual endpoints remain fixed. AutoPinned endpoints use
/// their rank in the full bearing-ordered group, preserving stable UUID/role
/// tie-breaks and the public/effective-secret layout split.
pub(super) fn redistribute_port_updates(
    area: &AreaCache,
    room_number: RoomNumber,
    side: RoomSide,
    secret: bool,
) -> Vec<(ConnectionId, ConnectionUpdates)> {
    let mut group = Vec::new();
    for connection in area.get_connections() {
        let connection_secret = area
            .get_room_connections()
            .iter()
            .find(|rendered| rendered.connection_id == connection.id)
            .is_some_and(|rendered| rendered.is_secret);
        if connection_secret != secret {
            continue;
        }
        for (endpoint_b, endpoint) in [
            (false, Some(connection.endpoint_a)),
            (true, connection.endpoint_b),
        ] {
            let Some(endpoint) = endpoint else { continue };
            if endpoint.room_number == room_number && endpoint.side == side {
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
    #[allow(clippy::cast_precision_loss)]
    let denominator = (group.len() + 1) as f32;
    group
        .into_iter()
        .enumerate()
        .filter(|(_, item)| item.endpoint.port_mode == smudgy_cloud::PortMode::AutoPinned)
        .filter_map(|(slot, mut item)| {
            #[allow(clippy::cast_precision_loss)]
            let offset = (slot + 1) as f32 / denominator;
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
    PropertyDeleted(usize),
    NewPropertyNameChanged(String),
    NewPropertyValueChanged(String),
    AddProperty,
    RoomTagInputChanged(String),
    RoomTagAdded(String),
    RoomTagRemoved(String),
    BulkColorChanged(String),
    BulkLevelChanged(String),
    ApplyBulkColor,
    ApplyBulkLevel,
    AreaPropertyValueChanged(usize, String),
    AreaPropertyDeleted(usize),
    NewAreaPropertyNameChanged(String),
    NewAreaPropertyValueChanged(String),
    AddAreaProperty,
    ExitFromDirectionChanged(usize, ExitDirection),
    ExitToAreaChanged(usize, AreaChoice),
    ExitToRoomChanged(usize, String),
    ExitToDirectionChanged(usize, ExitDirection),
    ExitPathChanged(usize, String),
    ExitCommandChanged(usize, String),
    ExitWeightChanged(usize, String),
    ExitHiddenToggled(usize, bool),
    ExitClosedToggled(usize, bool),
    ExitLockedToggled(usize, bool),
    ExitDeleted(usize),
    AddExit,
    ConnectionRoutingChanged(ConnectionRouting),
    ConnectionSegmentShapeChanged(SegmentShape),
    ConnectionCornerChanged(CornerStyle),
    ConnectionDashChanged(ConnectionDash),
    ConnectionColorChanged(String),
    ConnectionThicknessChanged(String),
    ConnectionEndpointSideChanged(bool, RoomSide),
    ConnectionEndpointOffsetChanged(bool, String),
    ConnectionEndpointReset(bool),
    ConnectionRedistributePorts(bool),
    ConnectionClearRoute,
    ConnectionReroute,
    ConnectionReset,
    ConnectionDelete,
    ConnectionUnlink(usize),
    ConnectionPair(ConnectionId),
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
    RoomSecretToggled(bool),
    ExitSecretToggled(usize, bool),
    LabelSecretToggled(bool),
    ShapeSecretToggled(bool),
    RoomPropertySecretToggled(usize, bool),
    AreaPropertySecretToggled(usize, bool),
    BulkSecretMark(bool),
    SecretMarksCompleted {
        area_id: AreaId,
        request: SecretMarksRequest,
        bulk: bool,
        result: Result<SecretMarksResult, CloudError>,
    },
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
}

/// An area option in the exit-destination picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaChoice {
    pub id: AreaId,
    pub name: String,
}

impl fmt::Display for AreaChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone)]
struct ExitRow {
    id: ExitId,
    from_room: RoomNumber,
    from_direction: ExitDirection,
    to_area: Option<AreaId>,
    to_room: String,
    to_direction: Option<ExitDirection>,
    path: String,
    command: String,
    weight: String,
    is_hidden: bool,
    is_closed: bool,
    is_locked: bool,
    is_secret: bool,
    /// The destination exists but was redacted ("Unknown map"): the
    /// destination controls render disabled instead of pretending the exit
    /// is dangling.
    to_unknown: bool,
}

#[derive(Debug, Clone, Default)]
struct PropertyRow {
    name: String,
    value: String,
    is_secret: bool,
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
    is_secret: bool,
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
            is_secret: false,
        }
    }
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
    /// The selected room's tags (normalized UPPERCASE, sorted).
    tags: Vec<String>,
    /// Distinct tags across the selected room's area, for the "add existing"
    /// suggestions. Computed once per resync, not per render.
    known_tags: Vec<String>,
    /// The add-tag input buffer.
    new_tag: String,
    exits: Vec<ExitRow>,
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
    /// Secrecy of the single selected room/label/shape.
    is_secret: bool,
    /// Error from the last secret-marks call (preserved across resyncs;
    /// cleared when the next secrecy action starts).
    secret_error: Option<String>,
    /// Per-type changed counts from the last bulk secret-marks call.
    secret_notice: Option<String>,
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
        }
    }
}

impl State {
    /// Rebuilds every buffer from the current cache snapshot. Secrecy
    /// error/notice lines survive the rebuild (resyncs fire on every cache
    /// change, which would otherwise hide them before they're read).
    pub fn resync(&mut self, mapper: &Mapper, editor: &MapEditor) {
        let secret_error = self.secret_error.take();
        let secret_notice = self.secret_notice.take();
        *self = Self::default();
        self.secret_error = secret_error;
        self.secret_notice = secret_notice;

        let atlas = mapper.get_current_atlas();
        let Some(area) = editor.area_id().and_then(|id| atlas.get_area(&id)) else {
            return;
        };

        match editor.selection().single() {
            Some(EntityId::Room(room_number)) => {
                if let Some(room) = area.get_room(&room_number) {
                    self.title = room.get_title().to_string();
                    self.description = text_editor::Content::with_text(room.get_description());
                    self.level = room.get_level().to_string();
                    self.x = room.get_x().to_string();
                    self.y = room.get_y().to_string();
                    self.color = room.get_color().to_string();
                    self.is_secret = room.is_secret();
                    self.properties = sorted_properties(room.properties_with_secrecy());
                    self.tags = room.tags().map(String::from).collect();
                    // Distinct tags across this area, for "add existing" suggestions.
                    let mut known: std::collections::BTreeSet<String> =
                        std::collections::BTreeSet::new();
                    for r in area.get_rooms() {
                        known.extend(r.tags().map(String::from));
                    }
                    self.known_tags = known.into_iter().collect();

                    self.exits = room
                        .get_exits()
                        .iter()
                        .map(|exit| ExitRow {
                            id: exit.id,
                            from_room: room_number,
                            from_direction: exit.from_direction,
                            to_area: exit.to_area_id,
                            to_room: exit
                                .to_room_number
                                .map(|n| n.to_string())
                                .unwrap_or_default(),
                            to_direction: exit.to_direction,
                            path: exit.path.clone().unwrap_or_default(),
                            command: exit.command.clone().unwrap_or_default(),
                            weight: exit.weight.to_string(),
                            is_hidden: exit.is_hidden,
                            is_closed: exit.is_closed,
                            is_locked: exit.is_locked,
                            is_secret: exit.is_secret,
                            to_unknown: exit.to_unknown,
                        })
                        .collect();
                    // Cache order moves updated exits to the back; sort by
                    // id so rows stay put while being edited.
                    self.exits.sort_by_key(|row| row.id.0);
                }
            }
            Some(EntityId::Label(label_id)) => {
                if let Some(label) = area.get_label(&label_id) {
                    self.is_secret = label.is_secret;
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
                if let Some(shape) = area.get_shape(&shape_id) {
                    self.is_secret = shape.is_secret;
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
                if let Some(connection) = area.get_connection(connection_id) {
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
                        is_secret: area
                            .get_room_connections()
                            .iter()
                            .find(|render| render.connection_id == connection_id)
                            .is_some_and(|render| render.is_secret),
                    };
                    for room in area.get_rooms() {
                        for exit in room.get_exits() {
                            if exit.connection_id == connection_id {
                                self.exits.push(ExitRow {
                                    id: exit.id,
                                    from_room: room.get_room_number(),
                                    from_direction: exit.from_direction,
                                    to_area: exit.to_area_id,
                                    to_room: exit
                                        .to_room_number
                                        .map(|n| n.to_string())
                                        .unwrap_or_default(),
                                    to_direction: exit.to_direction,
                                    path: exit.path.clone().unwrap_or_default(),
                                    command: exit.command.clone().unwrap_or_default(),
                                    weight: exit.weight.to_string(),
                                    is_hidden: exit.is_hidden,
                                    is_closed: exit.is_closed,
                                    is_locked: exit.is_locked,
                                    is_secret: exit.is_secret,
                                    to_unknown: exit.to_unknown,
                                });
                            }
                        }
                    }
                    self.exits.sort_by_key(|row| row.id.0);
                }
            }
            None => {
                if editor.selection().is_empty() {
                    self.area_properties = sorted_properties(area.properties_with_secrecy());
                } else {
                    // Multi-selection: prefill the bulk fields with values
                    // the rooms agree on; disagreements show "(mixed)".
                    let rooms: Vec<_> = editor
                        .selection()
                        .rooms()
                        .filter_map(|number| area.get_room(&number))
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

fn sorted_properties<'a>(
    properties: impl Iterator<Item = (&'a str, &'a PropertyEntry)>,
) -> Vec<PropertyRow> {
    let mut rows: Vec<PropertyRow> = properties
        .map(|(name, entry)| PropertyRow {
            name: name.to_string(),
            value: entry.value.clone(),
            is_secret: entry.is_secret,
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

    fn commit_room_field(
        &mut self,
        field: FieldId,
        updates: RoomUpdates,
    ) -> Update<super::Message, super::Event> {
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
                area.get_connection(connection_id)
                    .map(|connection| connection.routing == ConnectionRouting::Automatic)
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

    fn commit_exit_field(
        &mut self,
        index: usize,
        field: FieldId,
        change: impl FnOnce(&mut smudgy_cloud::ExitUpdates),
    ) -> Update<super::Message, super::Event> {
        let Some(exit_id) = self.inspector.exits.get(index).map(|row| row.id) else {
            return Update::none();
        };
        let room_key = self.selected_room_key().or_else(|| {
            Some(RoomKey::new(
                self.editor.area_id()?,
                self.inspector.exits.get(index)?.from_room,
            ))
        });
        let Some(room_key) = room_key else {
            return Update::none();
        };
        let command = commands::edit_exit_field(
            &self.mapper.get_current_atlas(),
            room_key,
            exit_id,
            field,
            change,
        );
        self.push_command(command)
    }

    /// Sends one secret-marks POST and optimistically mirrors the flags into
    /// the local cache (reverted by [`Message::SecretMarksCompleted`] on
    /// failure). Secrecy edits deliberately bypass the undo stack, like area
    /// rename: they flip a server-side sharing flag, not map geometry.
    ///
    /// The request is filtered to entities whose cached flag actually
    /// differs from the target before anything is applied or sent, so the
    /// optimistic application and a failure's revert are exact inverses —
    /// re-marking an already-secret entity must not be "reverted" to public.
    fn send_secret_marks(
        &mut self,
        area_id: AreaId,
        mut request: SecretMarksRequest,
        bulk: bool,
    ) -> Update<super::Message, super::Event> {
        if !self.secrets_cleared() {
            return Update::none();
        }

        self.inspector.secret_error = None;
        self.inspector.secret_notice = None;

        retain_changing_marks(&self.mapper, area_id, &mut request);
        if secret_marks_request_is_empty(&request) {
            // Everything already matches the target: nothing to apply,
            // send, or revert.
            if bulk {
                self.inspector.secret_notice = Some("Nothing changed".to_string());
            }
            return Update::none();
        }

        apply_marks_locally(&self.mapper, area_id, &request, request.secret);
        self.refresh_seen_rev();
        self.inspector.resync(&self.mapper, &self.editor);

        let client = self.cloud.client.clone();
        let echo = request.clone();
        Update::with_task(Task::perform(
            async move { client.secret_marks(area_id, &request).await },
            move |result| {
                super::Message::Inspector(Message::SecretMarksCompleted {
                    area_id,
                    request: echo.clone(),
                    bulk,
                    result,
                })
            },
        ))
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
            Message::PropertyValueChanged(index, value) => {
                let Some(row) = self.inspector.properties.get_mut(index) else {
                    return Update::none();
                };
                row.value = value.clone();
                let name = row.name.clone();
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
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                let command = commands::set_room_property(
                    &self.mapper.get_current_atlas(),
                    room_key,
                    name.clone(),
                    value.clone(),
                );
                let update = self.push_command(command);
                self.inspector.properties.push(PropertyRow {
                    name,
                    value,
                    is_secret: false,
                });
                self.inspector
                    .properties
                    .sort_by(|a, b| a.name.cmp(&b.name));
                self.inspector.new_property_name.clear();
                self.inspector.new_property_value.clear();
                update
            }
            Message::RoomTagInputChanged(value) => {
                self.inspector.new_tag = value;
                Update::none()
            }
            Message::RoomTagAdded(tag) => {
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                // `add_room_tag` normalizes + dedupes; it returns None (no command)
                // when the tag is empty or already present.
                let Some(command) =
                    commands::add_room_tag(&self.mapper.get_current_atlas(), room_key, tag.clone())
                else {
                    // Clear the input even on a no-op add of the typed buffer.
                    if smudgy_cloud::mapper::normalize_tag(&tag)
                        == smudgy_cloud::mapper::normalize_tag(&self.inspector.new_tag)
                    {
                        self.inspector.new_tag.clear();
                    }
                    return Update::none();
                };
                let update = self.push_command(Some(command));
                let normalized = smudgy_cloud::mapper::normalize_tag(&tag);
                if !self.inspector.tags.iter().any(|t| *t == normalized) {
                    self.inspector.tags.push(normalized.clone());
                    self.inspector.tags.sort();
                }
                if smudgy_cloud::mapper::normalize_tag(&self.inspector.new_tag) == normalized {
                    self.inspector.new_tag.clear();
                }
                update
            }
            Message::RoomTagRemoved(tag) => {
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                let Some(command) = commands::remove_room_tag(
                    &self.mapper.get_current_atlas(),
                    room_key,
                    tag.clone(),
                ) else {
                    return Update::none();
                };
                let update = self.push_command(Some(command));
                let normalized = smudgy_cloud::mapper::normalize_tag(&tag);
                self.inspector.tags.retain(|t| *t != normalized);
                update
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
            Message::ExitFromDirectionChanged(index, direction) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.from_direction = direction;
                }
                self.commit_exit_field(index, FieldId::FromDirection, |updates| {
                    updates.from_direction = Some(direction);
                })
            }
            Message::ExitToAreaChanged(index, choice) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.to_area = Some(choice.id);
                }
                self.commit_exit_field(index, FieldId::Destination, move |updates| {
                    updates.to_area_id = Some(choice.id);
                })
            }
            Message::ExitToRoomChanged(index, value) => {
                let parsed = if value.trim().is_empty() {
                    Some(None)
                } else {
                    value
                        .trim()
                        .parse::<i32>()
                        .ok()
                        .map(|n| Some(RoomNumber(n)))
                };
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.to_room = value;
                    if parsed == Some(None) {
                        row.to_area = None;
                        row.to_direction = None;
                    }
                }
                match parsed {
                    Some(to_room_number) => {
                        self.commit_exit_field(index, FieldId::Destination, move |updates| {
                            updates.to_room_number = to_room_number;
                            if to_room_number.is_none() {
                                // The wire contract can only null a
                                // destination as a whole (`clear_to`); an
                                // update can never blank just the room. An
                                // emptied room field therefore unlinks the
                                // exit entirely (edit_exit_field turns this
                                // into clear_to on the way out).
                                updates.to_area_id = None;
                                updates.to_direction = None;
                            }
                        })
                    }
                    None => Update::none(),
                }
            }
            Message::ExitToDirectionChanged(index, direction) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.to_direction = Some(direction);
                }
                self.commit_exit_field(index, FieldId::Destination, move |updates| {
                    updates.to_direction = Some(direction);
                })
            }
            Message::ExitPathChanged(index, value) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.path = value.clone();
                }
                self.commit_exit_field(index, FieldId::Path, move |updates| {
                    updates.path = if value.is_empty() { None } else { Some(value) };
                })
            }
            Message::ExitCommandChanged(index, value) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.command = value.clone();
                }
                self.commit_exit_field(index, FieldId::Command, move |updates| {
                    updates.command = if value.is_empty() { None } else { Some(value) };
                })
            }
            Message::ExitWeightChanged(index, value) => {
                let parsed = value.parse::<f32>().ok();
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.weight = value;
                }
                match parsed {
                    Some(weight) => {
                        self.commit_exit_field(index, FieldId::Weight, move |updates| {
                            updates.weight = Some(weight);
                        })
                    }
                    None => Update::none(),
                }
            }
            Message::ExitHiddenToggled(index, hidden) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.is_hidden = hidden;
                }
                self.commit_exit_field(index, FieldId::Flags, move |updates| {
                    updates.is_hidden = Some(hidden);
                })
            }
            Message::ExitClosedToggled(index, closed) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.is_closed = closed;
                }
                self.commit_exit_field(index, FieldId::Flags, move |updates| {
                    updates.is_closed = Some(closed);
                })
            }
            Message::ExitLockedToggled(index, locked) => {
                if let Some(row) = self.inspector.exits.get_mut(index) {
                    row.is_locked = locked;
                }
                self.commit_exit_field(index, FieldId::Flags, move |updates| {
                    updates.is_locked = Some(locked);
                })
            }
            Message::ExitDeleted(index) => {
                if index >= self.inspector.exits.len() {
                    return Update::none();
                }
                // Exits to a redacted destination are not deletable one at a
                // time: undo could only recreate them dangling, destroying
                // the owner's cross-area link. The view hides the button;
                // commands::delete_exit refuses too.
                if self.inspector.exits[index].to_unknown {
                    return Update::none();
                }
                let row = self.inspector.exits.remove(index);
                let room_key = self
                    .selected_room_key()
                    .or_else(|| Some(RoomKey::new(self.editor.area_id()?, row.from_room)));
                let Some(room_key) = room_key else {
                    return Update::none();
                };
                let command =
                    commands::delete_exit(&self.mapper.get_current_atlas(), room_key, row.id);
                self.push_command(command)
            }
            Message::AddExit => {
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                // The row appears via the resync that runs when the async
                // create completes.
                self.push_command(Some(commands::add_default_exit(
                    room_key.area_id,
                    room_key.room_number,
                )))
            }
            Message::ConnectionRoutingChanged(routing) => {
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(area) = atlas.get_area(&area_id) else {
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
                let Some(area) = atlas.get_area(&area_id) else {
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
                            "That route has too many points to normalize as Orthogonal".to_string(),
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
                if !value.is_empty() && smudgy_cloud::canonicalize_css_color(&value).is_none() {
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
            Message::ConnectionThicknessChanged(value) => {
                self.inspector.connection.thickness = value.clone();
                let Ok(thickness) = value.parse::<f32>() else {
                    return Update::none();
                };
                if !smudgy_cloud::THICKNESS_RANGE.contains(&thickness) {
                    return Update::none();
                }
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
                let Some(area) = atlas.get_area(&area_id) else {
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
                let Some(updates) = endpoint_updates(&area, connection_id, endpoint, endpoint_b)
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
                let Some(area) = atlas.get_area(&area_id) else {
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
                let Some(updates) = endpoint_updates(&area, connection_id, endpoint, endpoint_b)
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
                let Some(area) = atlas.get_area(&area_id) else {
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
                    .get_rooms()
                    .iter()
                    .find_map(|room| {
                        room.get_exits()
                            .iter()
                            .find(|exit| {
                                exit.connection_id == connection_id
                                    && room.get_room_number() == endpoint.room_number
                            })
                            .map(|exit| exit.from_direction)
                    })
                    .or_else(|| {
                        area.get_rooms().iter().find_map(|room| {
                            room.get_exits()
                                .iter()
                                .find(|exit| {
                                    exit.connection_id == connection_id
                                        && exit.to_area_id == Some(area_id)
                                        && exit.to_room_number == Some(endpoint.room_number)
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
                let Some(updates) = endpoint_updates(&area, connection_id, endpoint, endpoint_b)
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
                let Some(area) = atlas.get_area(&area_id) else {
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
                let secret = area
                    .get_room_connections()
                    .iter()
                    .find(|rendered| rendered.connection_id == connection_id)
                    .is_some_and(|rendered| rendered.is_secret);
                let edits =
                    redistribute_port_updates(&area, endpoint.room_number, endpoint.side, secret);
                if edits.is_empty() {
                    return Update::none();
                }
                let preview = edits
                    .iter()
                    .filter_map(|(id, update)| {
                        update
                            .endpoint_a
                            .or(update.endpoint_b)
                            .map(|endpoint| (*id, endpoint.port_offset))
                    })
                    .collect();
                self.modal = Some(super::modals::Modal::ConfirmRedistributePorts {
                    area_id,
                    room_number: endpoint.room_number,
                    side: endpoint.side,
                    secret,
                    preview,
                });
                Update::none()
            }
            Message::ConnectionClearRoute => {
                let routing = self
                    .selected_connection_id()
                    .and_then(|(area_id, connection_id)| {
                        self.mapper
                            .get_current_atlas()
                            .get_area(&area_id)
                            .and_then(|area| {
                                area.get_connection(connection_id).map(|connection| {
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
            Message::ConnectionDelete => self.delete_selection(),
            Message::ConnectionUnlink(index) => {
                let Some((area_id, connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                let Some(exit_id) = self.inspector.exits.get(index).map(|row| row.id) else {
                    return Update::none();
                };
                self.push_command(Some(commands::unlink_exit(area_id, exit_id, connection_id)))
            }
            Message::ConnectionPair(merge_connection_id) => {
                let Some((area_id, keep_connection_id)) = self.selected_connection_id() else {
                    return Update::none();
                };
                self.push_command(commands::pair_connections(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    keep_connection_id,
                    merge_connection_id,
                ))
            }
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
                self.inspector.area_properties.push(PropertyRow {
                    name,
                    value,
                    is_secret: false,
                });
                self.inspector
                    .area_properties
                    .sort_by(|a, b| a.name.cmp(&b.name));
                self.inspector.new_area_property_name.clear();
                self.inspector.new_area_property_value.clear();
                update
            }
            Message::RoomSecretToggled(secret) => {
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                self.inspector.is_secret = secret;
                let mut request = empty_secret_marks_request(secret);
                request.rooms.push(room_key.room_number.0);
                self.send_secret_marks(room_key.area_id, request, false)
            }
            Message::ExitSecretToggled(index, secret) => {
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                let Some(row) = self.inspector.exits.get_mut(index) else {
                    return Update::none();
                };
                row.is_secret = secret;
                let exit_id = row.id;
                let mut request = empty_secret_marks_request(secret);
                request.exits.push(exit_id);
                self.send_secret_marks(room_key.area_id, request, false)
            }
            Message::LabelSecretToggled(secret) => {
                let Some((area_id, label_id)) = self.selected_label_id() else {
                    return Update::none();
                };
                self.inspector.is_secret = secret;
                let mut request = empty_secret_marks_request(secret);
                request.labels.push(label_id);
                self.send_secret_marks(area_id, request, false)
            }
            Message::ShapeSecretToggled(secret) => {
                let Some((area_id, shape_id)) = self.selected_shape_id() else {
                    return Update::none();
                };
                self.inspector.is_secret = secret;
                let mut request = empty_secret_marks_request(secret);
                request.shapes.push(shape_id);
                self.send_secret_marks(area_id, request, false)
            }
            Message::RoomPropertySecretToggled(index, secret) => {
                let Some(room_key) = self.selected_room_key() else {
                    return Update::none();
                };
                let Some(row) = self.inspector.properties.get_mut(index) else {
                    return Update::none();
                };
                row.is_secret = secret;
                let name = row.name.clone();
                let mut request = empty_secret_marks_request(secret);
                request.room_properties.push(RoomPropertyRef {
                    room_number: room_key.room_number.0,
                    name,
                });
                self.send_secret_marks(room_key.area_id, request, false)
            }
            Message::AreaPropertySecretToggled(index, secret) => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let Some(row) = self.inspector.area_properties.get_mut(index) else {
                    return Update::none();
                };
                row.is_secret = secret;
                let name = row.name.clone();
                let mut request = empty_secret_marks_request(secret);
                request.area_properties.push(name);
                self.send_secret_marks(area_id, request, false)
            }
            Message::BulkSecretMark(secret) => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                // Only entities directly selected; exits of selected rooms
                // are deliberately not implied.
                let mut request = empty_secret_marks_request(secret);
                request.rooms = self.editor.selection().rooms().map(|n| n.0).collect();
                request.labels = self.editor.selection().labels().collect();
                request.shapes = self.editor.selection().shapes().collect();
                if request.rooms.is_empty()
                    && request.labels.is_empty()
                    && request.shapes.is_empty()
                {
                    return Update::none();
                }
                self.send_secret_marks(area_id, request, true)
            }
            Message::SecretMarksCompleted {
                area_id,
                request,
                bulk,
                result,
            } => {
                match result {
                    Ok(counts) => {
                        // The server bumped the area rev; pull it promptly.
                        self.mapper.sync_now();
                        if bulk {
                            self.inspector.secret_notice =
                                Some(format_marks_notice(counts, request.secret));
                        }
                    }
                    Err(error) => {
                        // Roll the optimistic flags back.
                        apply_marks_locally(&self.mapper, area_id, &request, !request.secret);
                        self.refresh_seen_rev();
                        self.inspector.resync(&self.mapper, &self.editor);
                        self.inspector.secret_error = Some(match error {
                            // The server never distinguishes "missing" from
                            // "not allowed"; neither do we.
                            CloudError::NotFoundOrNoAccess => {
                                "You can't change secrets here.".to_string()
                            }
                            other => other.to_string(),
                        });
                    }
                }
                Update::none()
            }
        }
    }
}

/// An empty (no-op) secret-marks request body.
pub(super) fn empty_secret_marks_request(secret: bool) -> SecretMarksRequest {
    SecretMarksRequest {
        secret,
        rooms: Vec::new(),
        exits: Vec::new(),
        labels: Vec::new(),
        shapes: Vec::new(),
        room_properties: Vec::new(),
        area_properties: Vec::new(),
    }
}

/// Whether a secret-marks request targets no entities at all.
fn secret_marks_request_is_empty(request: &SecretMarksRequest) -> bool {
    request.rooms.is_empty()
        && request.exits.is_empty()
        && request.labels.is_empty()
        && request.shapes.is_empty()
        && request.room_properties.is_empty()
        && request.area_properties.is_empty()
}

/// Drops entities whose cached flag already equals the request's target, so
/// the request lists exactly the entities the operation will change.
/// Entities missing from the cache are kept: their current state can't be
/// proven, and both the optimistic apply and a revert no-op on unknown ids.
fn retain_changing_marks(mapper: &Mapper, area_id: AreaId, request: &mut SecretMarksRequest) {
    let atlas = mapper.get_current_atlas();
    let Some(area) = atlas.get_area(&area_id) else {
        return;
    };
    let target = request.secret;

    request.rooms.retain(|number| {
        area.get_room(&RoomNumber(*number))
            .is_none_or(|room| room.is_secret() != target)
    });
    request.exits.retain(|exit_id| {
        area.get_rooms()
            .iter()
            .flat_map(|room| room.get_exits())
            .find(|exit| exit.id == *exit_id)
            .is_none_or(|exit| exit.is_secret != target)
    });
    request.labels.retain(|label_id| {
        area.get_label(label_id)
            .is_none_or(|label| label.is_secret != target)
    });
    request.shapes.retain(|shape_id| {
        area.get_shape(shape_id)
            .is_none_or(|shape| shape.is_secret != target)
    });
    request.room_properties.retain(|property| {
        area.get_room(&RoomNumber(property.room_number))
            .is_none_or(|room| {
                room.properties_with_secrecy()
                    .find(|(name, _)| *name == property.name)
                    .is_none_or(|(_, entry)| entry.is_secret != target)
            })
    });
    request.area_properties.retain(|name| {
        area.properties_with_secrecy()
            .find(|(n, _)| *n == name.as_str())
            .is_none_or(|(_, entry)| entry.is_secret != target)
    });
}

/// Mirrors a secret-marks request into the local cache with `secret` as the
/// flag value (pass the opposite of the request's own value to revert an
/// optimistic application).
pub(super) fn apply_marks_locally(
    mapper: &Mapper,
    area_id: AreaId,
    request: &SecretMarksRequest,
    secret: bool,
) {
    let rooms: Vec<RoomNumber> = request.rooms.iter().copied().map(RoomNumber).collect();
    let room_properties: Vec<(RoomNumber, String)> = request
        .room_properties
        .iter()
        .map(|property| (RoomNumber(property.room_number), property.name.clone()))
        .collect();
    mapper.apply_local_secret_marks(
        area_id,
        secret,
        &rooms,
        &request.exits,
        &request.labels,
        &request.shapes,
        &room_properties,
        &request.area_properties,
    );
}

/// "3 rooms, 1 label marked secret" — per-type counts of rows the server
/// actually changed.
fn format_marks_notice(counts: SecretMarksResult, secret: bool) -> String {
    fn push_part(parts: &mut Vec<String>, count: u64, singular: &str, plural: &str) {
        if count > 0 {
            let noun = if count == 1 { singular } else { plural };
            parts.push(format!("{count} {noun}"));
        }
    }

    let mut parts = Vec::new();
    push_part(&mut parts, counts.rooms, "room", "rooms");
    push_part(&mut parts, counts.exits, "exit", "exits");
    push_part(&mut parts, counts.labels, "label", "labels");
    push_part(&mut parts, counts.shapes, "shape", "shapes");
    push_part(
        &mut parts,
        counts.room_properties,
        "room property",
        "room properties",
    );
    push_part(
        &mut parts,
        counts.area_properties,
        "area property",
        "area properties",
    );

    if parts.is_empty() {
        return "Nothing changed".to_string();
    }
    format!(
        "{} {}",
        parts.join(", "),
        if secret { "marked secret" } else { "unmarked" }
    )
}

// ===== view =====

fn heading<'a>(content: String) -> iced::widget::Text<'a, crate::Theme> {
    text(content).size(16)
}

/// A heading with a subtle lock glyph appended when the entity is secret.
fn secret_aware_heading<'a>(content: String, is_secret: bool) -> ThemedElement<'a, super::Message> {
    let mut heading_row = row![heading(content)].spacing(6).align_y(Vertical::Center);
    if is_secret {
        heading_row = heading_row.push(
            text(super::ICON_LOCK_FILL)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(13.0)
                .style(|theme: &crate::Theme| iced::widget::text::Style {
                    color: Some(theme.styles.text.normal.scale_alpha(0.6)),
                }),
        );
    }
    heading_row.into()
}

/// The pending secrecy error or bulk-marks notice, when present.
fn secrecy_status<'a>(state: &State) -> Option<ThemedElement<'a, super::Message>> {
    if let Some(error) = &state.secret_error {
        Some(
            text(error.clone())
                .size(12)
                .style(builtins::text::danger)
                .into(),
        )
    } else {
        state.secret_notice.as_ref().map(|notice| {
            text(notice.clone())
                .size(12)
                .style(|theme: &crate::Theme| iced::widget::text::Style {
                    color: Some(theme.styles.text.normal.scale_alpha(0.7)),
                })
                .into()
        })
    }
}

/// A small lock icon button toggling one property row's secrecy.
fn lock_toggle<'a>(is_secret: bool, on_press: super::Message) -> ThemedElement<'a, super::Message> {
    tooltip(
        button(
            text(if is_secret {
                super::ICON_LOCK_FILL
            } else {
                super::ICON_UNLOCK
            })
            .font(fonts::BOOTSTRAP_ICONS)
            .size(14.0)
            .style(move |theme: &crate::Theme| iced::widget::text::Style {
                color: Some(if is_secret {
                    theme.styles.text.normal
                } else {
                    theme.styles.text.normal.scale_alpha(0.35)
                }),
            }),
        )
        .style(builtins::button::toolbar)
        .on_press(on_press),
        if is_secret {
            "Unmark secret"
        } else {
            "Mark secret"
        },
        tooltip::Position::Bottom,
    )
    .into()
}

fn field_label<'a>(label: &'static str) -> iced::widget::Text<'a, crate::Theme> {
    text(label)
        .size(11)
        .style(|theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.6)),
        })
}

fn labeled_input<'a>(
    label: &'static str,
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
        col = col.push(text("invalid value").size(11).style(builtins::text::danger));
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
    /// Per-row secrecy lock toggle; `None` hides all secrecy UI (the viewer
    /// isn't cleared for secrets).
    on_secret_toggle: Option<fn(usize, bool) -> Message>,
}

/// The shared key/value property list editor (rooms and areas).
fn properties_section<'a>(
    rows: &'a [PropertyRow],
    new_name: &'a str,
    new_value: &'a str,
    hooks: &PropertyHooks,
) -> ThemedElement<'a, super::Message> {
    let mut section = Column::new().spacing(4);
    section = section.push(field_label("Properties"));

    let on_value_change = hooks.on_value_change;
    let on_delete = hooks.on_delete;
    let on_new_name = hooks.on_new_name;
    let on_new_value = hooks.on_new_value;

    for (index, property_row) in rows.iter().enumerate() {
        let mut widgets = row![
            text(property_row.name.clone())
                .size(13)
                .width(Length::FillPortion(2)),
            text_input("value", &property_row.value)
                .size(13)
                .on_input(move |value| { super::Message::Inspector(on_value_change(index, value)) })
                .width(Length::FillPortion(3)),
        ]
        .spacing(4)
        .align_y(Vertical::Center);

        if let Some(on_secret) = hooks.on_secret_toggle {
            widgets = widgets.push(lock_toggle(
                property_row.is_secret,
                super::Message::Inspector(on_secret(index, !property_row.is_secret)),
            ));
        }

        widgets = widgets.push(trash_button(super::Message::Inspector(on_delete(index))));
        section = section.push(widgets);
    }

    section = section.push(
        row![
            text_input("name", new_name)
                .size(13)
                .on_input(move |value| super::Message::Inspector(on_new_name(value)))
                .width(Length::FillPortion(2)),
            text_input("value", new_value)
                .size(13)
                .on_input(move |value| super::Message::Inspector(on_new_value(value)))
                .on_submit(super::Message::Inspector(hooks.on_add.clone()))
                .width(Length::FillPortion(3)),
            button(text("Add").size(13))
                .style(builtins::button::secondary)
                .on_press(super::Message::Inspector(hooks.on_add.clone())),
        ]
        .spacing(4)
        .align_y(Vertical::Center),
    );

    section.into()
}

/// The room-tags editor: current tags as removable chips, an input to add a new
/// tag (normalized to UPPERCASE on commit), and one-click chips for tags already
/// in use elsewhere in the area. A validated set, not free-text editing.
fn tags_section<'a>(
    tags: &'a [String],
    known_tags: &'a [String],
    new_tag: &'a str,
) -> ThemedElement<'a, super::Message> {
    let mut section = Column::new().spacing(4);
    section = section.push(field_label("Tags"));

    if !tags.is_empty() {
        let chips: Vec<ThemedElement<'a, super::Message>> = tags
            .iter()
            .map(|tag| {
                let tag = tag.clone();
                button(text(format!("{tag}  \u{00d7}")).size(12))
                    .style(builtins::button::secondary)
                    .on_press(super::Message::Inspector(Message::RoomTagRemoved(tag)))
                    .into()
            })
            .collect();
        section = section.push(wrap_row(chips).spacing(6.0, 6.0));
    }

    section = section.push(
        row![
            text_input("add a tag", new_tag)
                .size(13)
                .on_input(|value| super::Message::Inspector(Message::RoomTagInputChanged(value)))
                .on_submit(super::Message::Inspector(Message::RoomTagAdded(
                    new_tag.to_string()
                )))
                .width(Length::Fill),
            button(text("Add").size(13))
                .style(builtins::button::secondary)
                .on_press(super::Message::Inspector(Message::RoomTagAdded(
                    new_tag.to_string()
                ))),
        ]
        .spacing(4)
        .align_y(Vertical::Center),
    );

    // Suggestions: tags used elsewhere in the area but not on this room.
    let suggestions: Vec<ThemedElement<'a, super::Message>> = known_tags
        .iter()
        .filter(|t| !tags.iter().any(|cur| cur == *t))
        .map(|tag| {
            let tag = tag.clone();
            button(text(format!("+ {tag}")).size(11))
                .style(builtins::button::secondary)
                .on_press(super::Message::Inspector(Message::RoomTagAdded(tag)))
                .into()
        })
        .collect();
    if !suggestions.is_empty() {
        section = section
            .push(text("In this area:").size(11))
            .push(wrap_row(suggestions).spacing(6.0, 4.0));
    }

    section.into()
}

fn single_room_view<'a>(
    window: &'a MapEditorWindow,
    room_number: smudgy_cloud::RoomNumber,
) -> Column<'a, super::Message, crate::Theme> {
    let state = &window.inspector;
    let cleared = window.secrets_cleared();

    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);
    content = content.push(secret_aware_heading(
        format!("Room #{room_number}"),
        state.is_secret,
    ));

    content = content.push(labeled_input(
        "Title",
        "room title",
        &state.title,
        true,
        Message::TitleChanged,
    ));
    content = content.push(
        column![
            field_label("Description"),
            text_editor(&state.description)
                .placeholder("room description")
                .size(14)
                .on_action(|action| {
                    super::Message::Inspector(Message::DescriptionEdited(action))
                }),
        ]
        .spacing(2),
    );
    content = content.push(labeled_input(
        "Level",
        "0",
        &state.level,
        state.level.parse::<i32>().is_ok(),
        Message::LevelChanged,
    ));
    content = content.push(
        row![
            container(labeled_input(
                "X",
                "0",
                &state.x,
                state.x.parse::<f32>().is_ok(),
                Message::XChanged,
            ))
            .width(Length::FillPortion(1)),
            container(labeled_input(
                "Y",
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
                "Color",
                "(default)",
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

    if cleared {
        content = content.push(
            checkbox(state.is_secret)
                .label("Secret room")
                .size(14)
                .text_size(13)
                .on_toggle(|secret| super::Message::Inspector(Message::RoomSecretToggled(secret))),
        );
        if let Some(status) = secrecy_status(state) {
            content = content.push(status);
        }
    }

    content = content.push(tags_section(&state.tags, &state.known_tags, &state.new_tag));

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
            on_secret_toggle: cleared
                .then_some(Message::RoomPropertySecretToggled as fn(usize, bool) -> Message),
        },
    ));

    content = content.push(exits_section(window));

    content
}

fn exits_section(window: &MapEditorWindow) -> ThemedElement<'_, super::Message> {
    let state = &window.inspector;
    let cleared = window.secrets_cleared();
    let atlas = window.mapper.get_current_atlas();

    // Session (ephemeral) areas are excluded as destinations: an exit from a
    // persistent map into an area that vanishes with the session would dangle.
    let ephemeral = window.mapper.ephemeral_area_ids();
    let mut area_choices: Vec<AreaChoice> = atlas
        .areas()
        .filter(|area| !ephemeral.contains(area.get_id()))
        .map(|area| AreaChoice {
            id: *area.get_id(),
            name: area.get_name().to_string(),
        })
        .collect();
    area_choices.sort_by_key(|choice| choice.name.to_lowercase());

    let mut section = Column::new().spacing(8);
    let connection_selected = matches!(
        window.editor.selection().single(),
        Some(EntityId::Connection(_))
    );
    section = section.push(field_label(if connection_selected {
        "Traversal"
    } else {
        "Exits"
    }));

    for (index, exit) in state.exits.iter().enumerate() {
        if index > 0 {
            section = section.push(rule::horizontal(1));
        }
        if connection_selected {
            section = section.push(
                text(format!("From room {}", exit.from_room))
                    .size(12)
                    .style(muted_text),
            );
        }

        let selected_area = exit
            .to_area
            .and_then(|id| area_choices.iter().find(|choice| choice.id == id).cloned());

        if exit.to_unknown {
            // The destination exists but was redacted by the server: show an
            // honest, disabled "Unknown map" destination (placeholder only —
            // there is no name or id to show) instead of a dangling exit.
            // No trash button either: the destination is unknowable
            // client-side, so a delete could never be undone faithfully
            // (the recreate would dangle, destroying the owner's link).
            section = section.push(
                row![
                    pick_list(
                        &ExitDirection::ALL[..],
                        Some(exit.from_direction),
                        move |d| {
                            super::Message::Inspector(Message::ExitFromDirectionChanged(index, d))
                        }
                    )
                    .text_size(12)
                    .width(Length::Fill),
                    text("\u{2192}").size(13),
                    // No `.on_input`: renders as a disabled field whose
                    // placeholder reads "Unknown map".
                    text_input("Unknown map", "").size(12).width(Length::Fill),
                ]
                .spacing(4)
                .align_y(Vertical::Center),
            );
            section = section.push(
                text("Leads to a map that wasn't shared with you.")
                    .size(11)
                    .style(muted_text),
            );
        } else {
            section = section.push(
                row![
                    pick_list(
                        &ExitDirection::ALL[..],
                        Some(exit.from_direction),
                        move |d| {
                            super::Message::Inspector(Message::ExitFromDirectionChanged(index, d))
                        }
                    )
                    .text_size(12)
                    .width(Length::Fill),
                    text("\u{2192}").size(13),
                    pick_list(area_choices.clone(), selected_area, move |choice| {
                        super::Message::Inspector(Message::ExitToAreaChanged(index, choice))
                    })
                    .placeholder("area")
                    .text_size(12)
                    .width(Length::Fill),
                    trash_button(super::Message::Inspector(Message::ExitDeleted(index))),
                ]
                .spacing(4)
                .align_y(Vertical::Center),
            );

            section = section.push(
                row![
                    text_input("room #", &exit.to_room)
                        .size(12)
                        .on_input(move |value| {
                            super::Message::Inspector(Message::ExitToRoomChanged(index, value))
                        })
                        .width(Length::FillPortion(1)),
                    pick_list(&ExitDirection::ALL[..], exit.to_direction, move |d| {
                        super::Message::Inspector(Message::ExitToDirectionChanged(index, d))
                    })
                    .placeholder("return dir")
                    .text_size(12)
                    .width(Length::FillPortion(2)),
                ]
                .spacing(4)
                .align_y(Vertical::Center),
            );
        }

        let mut flags = row![
            checkbox(exit.is_hidden)
                .label("hidden")
                .size(14)
                .text_size(12)
                .on_toggle(move |checked| {
                    super::Message::Inspector(Message::ExitHiddenToggled(index, checked))
                }),
            checkbox(exit.is_closed)
                .label("closed")
                .size(14)
                .text_size(12)
                .on_toggle(move |checked| {
                    super::Message::Inspector(Message::ExitClosedToggled(index, checked))
                }),
            checkbox(exit.is_locked)
                .label("locked")
                .size(14)
                .text_size(12)
                .on_toggle(move |checked| {
                    super::Message::Inspector(Message::ExitLockedToggled(index, checked))
                }),
        ]
        .spacing(8)
        .align_y(Vertical::Center);

        if cleared {
            flags = flags.push(
                checkbox(exit.is_secret)
                    .label("secret")
                    .size(14)
                    .text_size(12)
                    .on_toggle(move |checked| {
                        super::Message::Inspector(Message::ExitSecretToggled(index, checked))
                    }),
            );
        }

        section = section.push(flags);

        // Connection appearance moves to the Connection inspector.
        section = section.push(
            row![
                text_input("weight", &exit.weight)
                    .size(12)
                    .on_input(move |value| {
                        super::Message::Inspector(Message::ExitWeightChanged(index, value))
                    })
                    .width(Length::FillPortion(1)),
            ]
            .spacing(4)
            .align_y(Vertical::Center),
        );

        section = section.push(
            row![
                text_input("command", &exit.command)
                    .size(12)
                    .on_input(move |value| {
                        super::Message::Inspector(Message::ExitCommandChanged(index, value))
                    })
                    .width(Length::FillPortion(1)),
                text_input("path", &exit.path)
                    .size(12)
                    .on_input(move |value| {
                        super::Message::Inspector(Message::ExitPathChanged(index, value))
                    })
                    .width(Length::FillPortion(1)),
            ]
            .spacing(4)
            .align_y(Vertical::Center),
        );
        if connection_selected && state.exits.len() == 2 {
            section = section.push(
                button(text("Unlink this direction").size(12))
                    .style(builtins::button::secondary)
                    .on_press(super::Message::Inspector(Message::ConnectionUnlink(index))),
            );
        }
    }

    if !connection_selected {
        section = section.push(
            button(text("Add exit").size(13))
                .style(builtins::button::secondary)
                .on_press(super::Message::Inspector(Message::AddExit)),
        );
    }

    section.into()
}

fn connection_view(
    window: &MapEditorWindow,
    connection_id: ConnectionId,
) -> Column<'_, super::Message, crate::Theme> {
    let atlas = window.mapper.get_current_atlas();
    let state = &window.inspector;
    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);
    let Some(area_id) = window.editor.area_id() else {
        return content.push(text("No area selected"));
    };
    let Some(area) = atlas.get_area(&area_id) else {
        return content.push(text("No area selected"));
    };
    let Some(connection) = area.get_connection(connection_id) else {
        return content.push(text("Connection no longer exists"));
    };
    let endpoints = connection.endpoint_b.map_or_else(
        || format!("Room {} outward", connection.endpoint_a.room_number),
        |endpoint| {
            format!(
                "Room {} {} to room {} {}",
                connection.endpoint_a.room_number,
                connection.endpoint_a.side,
                endpoint.room_number,
                endpoint.side
            )
        },
    );
    content = content.push(secret_aware_heading(
        "Connection".to_string(),
        state.connection.is_secret,
    ));

    // Link
    content = content.push(field_label("Link"));
    content = content.push(text(format!("{} · {endpoints}", connection.kind)).size(12));
    content = content.push(
        row![
            pick_list(
                &RoomSide::ALL[..],
                Some(state.connection.endpoint_a_side),
                |side| super::Message::Inspector(Message::ConnectionEndpointSideChanged(
                    false, side
                )),
            )
            .text_size(12)
            .width(Length::FillPortion(2)),
            text_input("port 0–1", &state.connection.endpoint_a_offset)
                .on_input(|value| super::Message::Inspector(
                    Message::ConnectionEndpointOffsetChanged(false, value),
                ))
                .size(12)
                .width(Length::FillPortion(1)),
            button(text("Auto").size(11))
                .style(builtins::button::secondary)
                .on_press(super::Message::Inspector(Message::ConnectionEndpointReset(
                    false,
                ))),
            button(text("Redistribute").size(11))
                .style(builtins::button::secondary)
                .on_press(super::Message::Inspector(
                    Message::ConnectionRedistributePorts(false,)
                )),
        ]
        .spacing(6),
    );
    if state.connection.has_endpoint_b {
        content = content.push(
            row![
                pick_list(
                    &RoomSide::ALL[..],
                    Some(state.connection.endpoint_b_side),
                    |side| super::Message::Inspector(Message::ConnectionEndpointSideChanged(
                        true, side
                    ),),
                )
                .text_size(12)
                .width(Length::FillPortion(2)),
                text_input("port 0–1", &state.connection.endpoint_b_offset)
                    .on_input(|value| super::Message::Inspector(
                        Message::ConnectionEndpointOffsetChanged(true, value),
                    ))
                    .size(12)
                    .width(Length::FillPortion(1)),
                button(text("Auto").size(11))
                    .style(builtins::button::secondary)
                    .on_press(super::Message::Inspector(Message::ConnectionEndpointReset(
                        true,
                    ))),
                button(text("Redistribute").size(11))
                    .style(builtins::button::secondary)
                    .on_press(super::Message::Inspector(
                        Message::ConnectionRedistributePorts(true,)
                    )),
            ]
            .spacing(6),
        );
    }

    if state.exits.len() == 1 {
        let selected = &state.exits[0];
        for candidate in area.get_connections() {
            if candidate.id == connection_id {
                continue;
            }
            let mut candidate_members = area
                .get_rooms()
                .iter()
                .flat_map(|room| {
                    room.get_exits()
                        .iter()
                        .map(move |exit| (room.get_room_number(), exit))
                })
                .filter(|(_, exit)| exit.connection_id == candidate.id);
            let Some((candidate_from, candidate_exit)) = candidate_members.next() else {
                continue;
            };
            if candidate_members.next().is_some() {
                continue;
            }
            let selected_to = selected.to_room.parse::<i32>().ok().map(RoomNumber);
            let reciprocal = selected.to_area == Some(area_id)
                && candidate_exit.to_area_id == Some(area_id)
                && selected_to == Some(candidate_from)
                && candidate_exit.to_room_number == Some(selected.from_room)
                && selected
                    .to_direction
                    .is_none_or(|direction| direction == candidate_exit.from_direction)
                && candidate_exit
                    .to_direction
                    .is_none_or(|direction| direction == selected.from_direction);
            if reciprocal {
                content = content.push(
                    button(text("Pair with reciprocal connection").size(12))
                        .style(builtins::button::secondary)
                        .on_press(super::Message::Inspector(Message::ConnectionPair(
                            candidate.id,
                        ))),
                );
            }
        }
    }

    // Route
    content = content.push(rule::horizontal(1));
    content = content.push(field_label("Route"));
    let routing_choices = if connection.kind == smudgy_cloud::ConnectionKind::Internal {
        &ConnectionRouting::ALL[..]
    } else {
        &ConnectionRouting::ALL[..2]
    };
    content = content.push(
        pick_list(routing_choices, Some(state.connection.routing), |routing| {
            super::Message::Inspector(Message::ConnectionRoutingChanged(routing))
        })
        .text_size(12),
    );
    if matches!(
        state.connection.routing,
        ConnectionRouting::Manual | ConnectionRouting::Automatic
    ) {
        let shape: ThemedElement<'_, super::Message> = if state.connection.routing
            == ConnectionRouting::Manual
        {
            pick_list(
                &SegmentShape::ALL[..],
                Some(state.connection.segment_shape),
                |shape| super::Message::Inspector(Message::ConnectionSegmentShapeChanged(shape)),
            )
            .text_size(12)
            .width(Length::Fill)
            .into()
        } else {
            container(text("Orthogonal").size(12))
                .width(Length::Fill)
                .into()
        };
        content = content.push(
            row![
                shape,
                pick_list(
                    &CornerStyle::ALL[..],
                    Some(state.connection.corner),
                    |corner| super::Message::Inspector(Message::ConnectionCornerChanged(corner)),
                )
                .text_size(12)
                .width(Length::Fill),
            ]
            .spacing(6),
        );
    }
    if connection.kind == smudgy_cloud::ConnectionKind::Internal {
        content = content.push(
            button(text("Re-route…").size(12))
                .style(builtins::button::secondary)
                .on_press_maybe(
                    window
                        .can_edit_active_area()
                        .then_some(super::Message::Inspector(Message::ConnectionReroute)),
                ),
        );
    }
    if connection.routing == ConnectionRouting::Automatic {
        if window.automatic_route_is_stale(connection_id) {
            content = content.push(
                text("Route may be stale after map changes; use Re-route.")
                    .size(12)
                    .style(builtins::text::danger),
            );
        }
        match window.automatic_route_validation(connection_id) {
            Some(smudgy_cloud::automatic_routing::RouteValidation::Collision) => {
                content = content.push(
                    text("Route intersects a public room; use Re-route or edit it manually.")
                        .size(12)
                        .style(builtins::text::danger),
                );
            }
            Some(smudgy_cloud::automatic_routing::RouteValidation::Invalid) => {
                content = content.push(
                    text("Stored automatic route is invalid; use Re-route.")
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
        content = content.push(text("A stored route is inactive in this mode.").size(12));
    }
    content = content.push(
        button(text("Clear stored route").size(12))
            .style(builtins::button::secondary)
            .on_press_maybe(
                (window.can_edit_active_area()
                    && (!connection.route_points.is_empty()
                        || matches!(
                            connection.routing,
                            ConnectionRouting::Manual | ConnectionRouting::Automatic
                        )))
                .then_some(super::Message::Inspector(Message::ConnectionClearRoute)),
            ),
    );

    // Appearance
    content = content.push(rule::horizontal(1));
    content = content.push(field_label("Appearance"));
    content = content.push(
        pick_list(
            &ConnectionDash::ALL[..],
            Some(state.connection.dash),
            |dash| super::Message::Inspector(Message::ConnectionDashChanged(dash)),
        )
        .text_size(12),
    );
    content = content.push(
        row![
            text_input("CSS color", &state.connection.color)
                .on_input(
                    |value| super::Message::Inspector(Message::ConnectionColorChanged(value),)
                )
                .size(12)
                .width(Length::FillPortion(2)),
            text_input("width", &state.connection.thickness)
                .on_input(
                    |value| super::Message::Inspector(Message::ConnectionThicknessChanged(value),)
                )
                .size(12)
                .width(Length::FillPortion(1)),
        ]
        .spacing(6),
    );
    content = content.push(
        button(text("Reset route and appearance").size(12))
            .style(builtins::button::secondary)
            .on_press(super::Message::Inspector(Message::ConnectionReset)),
    );
    content = content.push(exits_section(window));
    content = content.push(rule::horizontal(1));
    content = content.push(
        button(
            text(if state.exits.len() == 2 {
                "Delete link and both directions"
            } else {
                "Delete link"
            })
            .size(12),
        )
        .style(builtins::button::secondary)
        .on_press(super::Message::Inspector(Message::ConnectionDelete)),
    );
    content
}

/// The shared x/y/width/height grid for labels and shapes.
fn bounds_fields<'a>(
    x: &'a str,
    y: &'a str,
    width: &'a str,
    height: &'a str,
    on_change: fn(BoundsField, String) -> Message,
) -> ThemedElement<'a, super::Message> {
    let bound_input = move |label: &'static str, value: &'a str, field: BoundsField| {
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
            bound_input("X", x, BoundsField::X),
            bound_input("Y", y, BoundsField::Y),
        ]
        .spacing(8),
        row![
            bound_input("Width", width, BoundsField::Width),
            bound_input("Height", height, BoundsField::Height),
        ]
        .spacing(8),
    ]
    .spacing(FIELD_SPACING)
    .into()
}

fn color_input<'a>(
    window: &'a MapEditorWindow,
    field: ColorField,
    label: &'static str,
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
    content = content.push(secret_aware_heading(
        "Label".to_string(),
        window.inspector.is_secret,
    ));

    if window.secrets_cleared() {
        content = content.push(
            checkbox(window.inspector.is_secret)
                .label("Secret")
                .size(14)
                .text_size(13)
                .on_toggle(|secret| super::Message::Inspector(Message::LabelSecretToggled(secret))),
        );
        if let Some(status) = secrecy_status(&window.inspector) {
            content = content.push(status);
        }
    }

    content = content.push(labeled_input(
        "Text",
        "label text",
        &state.text,
        true,
        Message::LabelTextChanged,
    ));
    content = content.push(color_input(
        window,
        ColorField::LabelText,
        "Color",
        "(default)",
        &state.color,
        false,
        Message::LabelColorChanged,
    ));
    content = content.push(color_input(
        window,
        ColorField::LabelBackground,
        "Background",
        "(none)",
        &state.background,
        true,
        Message::LabelBackgroundChanged,
    ));
    content = content.push(
        row![
            container(labeled_input(
                "Font size",
                "16",
                &state.font_size,
                state.font_size.parse::<i32>().is_ok_and(|v| v > 0),
                Message::LabelFontSizeChanged,
            ))
            .width(Length::FillPortion(1)),
            container(labeled_input(
                "Font weight",
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
            field_label("Alignment"),
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
    content = content.push(secret_aware_heading(
        "Shape".to_string(),
        window.inspector.is_secret,
    ));

    if window.secrets_cleared() {
        content = content.push(
            checkbox(window.inspector.is_secret)
                .label("Secret")
                .size(14)
                .text_size(13)
                .on_toggle(|secret| super::Message::Inspector(Message::ShapeSecretToggled(secret))),
        );
        if let Some(status) = secrecy_status(&window.inspector) {
            content = content.push(status);
        }
    }

    content = content.push(
        column![
            field_label("Shape"),
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
        "Fill",
        "(none)",
        &state.background,
        true,
        Message::ShapeBackgroundChanged,
    ));
    content = content.push(color_input(
        window,
        ColorField::ShapeStroke,
        "Stroke",
        "(none)",
        &state.stroke_color,
        true,
        Message::ShapeStrokeColorChanged,
    ));
    content = content.push(
        row![
            container(labeled_input(
                "Stroke width",
                "1",
                &state.stroke_width,
                state.stroke_width.parse::<f32>().is_ok_and(|v| v >= 0.0),
                Message::ShapeStrokeWidthChanged,
            ))
            .width(Length::FillPortion(1)),
            container(labeled_input(
                "Corner radius",
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

    let rooms = selection.rooms().count();
    let connections = selection.connections().count();
    let labels = selection.labels().count();
    let shapes = selection.shapes().count();

    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);
    content = content.push(heading(format!("{} selected", selection.len())));
    content = content.push(
        text(format!(
            "{connections} connections, {rooms} rooms, {labels} labels, {shapes} shapes"
        ))
        .size(13),
    );

    if rooms > 0 {
        // An empty buffer means the rooms either disagree ("(mixed)") or
        // all have no color ("(default)") — never a fake value.
        let color_placeholder = if state.bulk_color_mixed {
            "(mixed)"
        } else {
            "(default)"
        };
        content = content.push(
            column![
                field_label("Set color (Enter to apply)"),
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
            "(mixed)"
        } else {
            "0"
        };
        content = content.push(
            column![
                field_label("Set level (Enter to apply)"),
                text_input(level_placeholder, &state.bulk_level)
                    .size(14)
                    .on_input(|value| {
                        super::Message::Inspector(Message::BulkLevelChanged(value))
                    })
                    .on_submit(super::Message::Inspector(Message::ApplyBulkLevel)),
            ]
            .spacing(2),
        );
    }

    if window.secrets_cleared() {
        content = content.push(
            column![
                field_label("Secrecy"),
                row![
                    button(text("Mark secret").size(13))
                        .style(builtins::button::secondary)
                        .on_press(super::Message::Inspector(Message::BulkSecretMark(true))),
                    button(text("Unmark secret").size(13))
                        .style(builtins::button::secondary)
                        .on_press(super::Message::Inspector(Message::BulkSecretMark(false))),
                ]
                .spacing(8),
            ]
            .spacing(2),
        );
        if let Some(status) = secrecy_status(state) {
            content = content.push(status);
        }
    }

    content
}

/// An "Active / Inactive" status with a switch (the same action the area
/// list's switch fires), surfacing the control beyond the area list for
/// discoverability. Active maps are used to find your location as you play.
fn identification_toggle<'a>(
    window: &MapEditorWindow,
    area_id: AreaId,
) -> ThemedElement<'a, super::Message> {
    let enabled = window.mapper.is_area_enabled(&area_id);
    let (icon, tip) = if enabled {
        (bootstrap_icons::TOGGLE_ON, "Active — click to deactivate")
    } else {
        (bootstrap_icons::TOGGLE_OFF, "Inactive — click to activate")
    };
    let status_style: fn(&crate::Theme) -> iced::widget::text::Style = if enabled {
        builtins::text::success
    } else {
        muted_text
    };
    let status_line = row![
        text("This map:").size(12).style(muted_text),
        text(if enabled { "Active" } else { "Inactive" })
            .size(12)
            .style(status_style),
        space::horizontal(),
        tooltip(
            button(text(icon).font(fonts::BOOTSTRAP_ICONS).size(16.0),)
                .style(builtins::button::toolbar)
                .on_press(super::Message::ToggleAreaEnabled(area_id)),
            tip,
            tooltip::Position::Bottom,
        ),
    ]
    .spacing(6)
    .align_y(Vertical::Center);

    column![
        status_line,
        text("Active maps are used to find your location as you play.")
            .size(11)
            .style(muted_text),
    ]
    .spacing(2)
    .into()
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
        .push(field_label("Copies of this map"));

    for member in &family {
        let Some(member_area) = atlas.get_area(member) else {
            continue;
        };
        let enabled = window.mapper.is_area_enabled(member);
        let is_current = *member == area_id;

        let name = member_area.get_name().to_string();
        let label = if is_current {
            format!("{name} (this map)")
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
            text("Active").size(11).style(builtins::text::success)
        } else {
            text("Inactive").size(11).style(muted_text)
        });
        row_widgets = row_widgets.push(space::horizontal());

        // "Use only this copy" activates this member and deactivates its
        // siblings in one click. Offered on every member that isn't already
        // the sole active copy (where it would be a no-op).
        if !(enabled && enabled_count == 1) {
            row_widgets = row_widgets.push(
                button(text("Use only this copy").size(11))
                    .style(builtins::button::secondary)
                    .on_press(super::Message::SetActiveCopy(*member)),
            );
        }

        section = section.push(row_widgets);
    }

    section = section.push(
        text(
            "You may have multiple copies active at a time. When visiting rooms with copies in multiple active maps, the mapper may place you unpredictably.",
        )
        .size(11)
        .style(muted_text),
    );

    Some(section.into())
}

fn area_view(window: &MapEditorWindow) -> Column<'_, super::Message, crate::Theme> {
    let atlas = window.mapper.get_current_atlas();
    let state = &window.inspector;

    let mut content = Column::new().spacing(FIELD_SPACING).padding(12);

    let Some(area) = window.editor.area_id().and_then(|id| atlas.get_area(&id)) else {
        return content.push(text("No area selected"));
    };

    content = content.push(heading(area.get_name().to_string()));
    content = content.push(
        text(format!(
            "{} rooms \u{b7} level {}",
            area.room_count(),
            window.editor.level()
        ))
        .size(13),
    );

    // Clone provenance (owner-only data; the server omits it otherwise).
    if area.is_owned()
        && let Some(source_id) = area.meta().copied_from_area_id
    {
        let source = atlas.get_area(&source_id);
        let source_name = source.as_ref().map_or_else(
            || "a shared map".to_string(),
            |source| format!("\u{201c}{}\u{201d}", source.get_name()),
        );
        let mut line = format!("Copied from {source_name}");
        if let Some(rev) = area.meta().copied_from_rev {
            line.push_str(&format!(" at rev {rev}"));
        }
        if let Some(copied_at) = area.meta().copied_at {
            line.push_str(&format!(" on {}", copied_at.format("%Y-%m-%d")));
        }
        // Rev is opaque — inequality means "changed", never a count.
        if let (Some(source), Some(rev)) = (source.as_ref(), area.meta().copied_from_rev)
            && source.get_rev() != rev
        {
            line.push_str(" (source has changed since)");
        }
        content = content.push(text(line).size(12).style(muted_text));
    }

    // Shared (but editable) areas get the enriched attribution here, since
    // read_only_view never runs for them.
    if !area.is_owned() {
        content = content.push(
            text(window.sharer_attribution(*area.get_id()))
                .size(12)
                .style(muted_text),
        );
    }

    // Room-identification status + discoverable toggle (for any area).
    content = content.push(identification_toggle(window, *area.get_id()));

    // Copy family: when ≥2 cache-resident clones share an ancestry, let the
    // user pick which one is active for room identification.
    if let Some(section) = copies_section(window, *area.get_id()) {
        content = content.push(section);
    }

    content = content.push(properties_section(
        &state.area_properties,
        &state.new_area_property_name,
        &state.new_area_property_value,
        &PropertyHooks {
            on_value_change: Message::AreaPropertyValueChanged,
            on_delete: Message::AreaPropertyDeleted,
            on_new_name: Message::NewAreaPropertyNameChanged,
            on_new_value: Message::NewAreaPropertyValueChanged,
            on_add: Message::AddAreaProperty,
            on_secret_toggle: window
                .secrets_cleared()
                .then_some(Message::AreaPropertySecretToggled as fn(usize, bool) -> Message),
        },
    ));

    if window.secrets_cleared()
        && let Some(status) = secrecy_status(state)
    {
        content = content.push(status);
    }

    if let Some(RoomKey { room_number, .. }) = window.hovered_room
        && let Some(room) = area.get_room(&room_number)
    {
        content = content.push(heading(format!("Room #{room_number}")));
        content = content.push(text(room.get_title().to_string()).size(13));
        content = content.push(text(room.get_description().to_string()).size(12));
    }

    content
}

pub fn view(window: &MapEditorWindow) -> ThemedElement<'_, super::Message> {
    let atlas = window.mapper.get_current_atlas();
    let area = window.editor.area_id().and_then(|id| atlas.get_area(&id));

    let selection = window.editor.selection();

    // View-only shared areas swap the editable forms for a read-only
    // summary. Mutations are also gated centrally in mod.rs (push_command /
    // handle_mutation_request), so this is presentation, not enforcement.
    let read_only = area
        .as_ref()
        .is_some_and(|area| !area.effective_access().can_edit);

    let content: Column<'_, super::Message, crate::Theme> = if area.is_none() {
        Column::new().padding(12).push(text("No area selected"))
    } else if read_only {
        read_only_view(window)
    } else if let Some(entity) = selection.single() {
        match entity {
            EntityId::Connection(connection_id) => connection_view(window, connection_id),
            EntityId::Room(room_number) => single_room_view(window, room_number),
            EntityId::Label(_) => label_view(window),
            EntityId::Shape(_) => shape_view(window),
        }
    } else if selection.is_empty() {
        area_view(window)
    } else {
        multi_selection_view(window)
    };

    container(scrollable(content).height(Length::Fill))
        .style(builtins::container::opaque)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// "{direction} → {target}" for the read-only exit listing, honoring the
/// projection: unknown destinations never get a name.
fn exit_summary(atlas: &AtlasCache, area: &AreaCache, exit: &ExitCache) -> String {
    let target = if exit.to_unknown {
        "Unknown map".to_string()
    } else if let Some(to_area) = exit.to_area_id.filter(|to| to != area.get_id()) {
        let name = atlas
            .get_area(&to_area)
            .map_or_else(|| "another area".to_string(), |a| a.get_name().to_string());
        match exit.to_room_number {
            Some(number) => format!("{name}, room {number}"),
            None => name,
        }
    } else {
        match exit.to_room_number {
            Some(number) => format!("room {number}"),
            None => "nowhere".to_string(),
        }
    };
    format!("{} \u{2192} {}", exit.from_direction, target)
}

fn muted_text(theme: &crate::Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

/// The whole inspector pane for a view-only shared area: an attribution
/// banner plus read-only summaries of the selection — no inputs at all.
#[allow(clippy::too_many_lines)]
fn read_only_view(window: &MapEditorWindow) -> Column<'_, super::Message, crate::Theme> {
    let atlas = window.mapper.get_current_atlas();
    let mut content = Column::new().padding(12).spacing(8);

    let Some(area) = window.editor.area_id().and_then(|id| atlas.get_area(&id)) else {
        return content.push(text("No area selected"));
    };

    // Attribution enriched from the sharer index: names the re-sharer and the
    // underlying owner when they differ.
    let attribution = window.sharer_attribution(*area.get_id());
    content = content.push(
        text(format!("{attribution} \u{2014} view only."))
            .size(12)
            .style(muted_text),
    );
    content = content.push(rule::horizontal(1));

    let selection = window.editor.selection();
    if let Some(entity) = selection.single() {
        match entity {
            EntityId::Connection(connection_id) => {
                if let Some(connection) = area.get_connection(connection_id) {
                    content = content.push(heading("Connection".to_string()));
                    content = content.push(
                        text(format!(
                            "{} · room {}{} · {}",
                            connection.kind,
                            connection.endpoint_a.room_number,
                            connection.endpoint_b.map_or_else(String::new, |endpoint| {
                                format!(" to room {}", endpoint.room_number)
                            }),
                            if area
                                .get_room_connections()
                                .iter()
                                .find(|render| render.connection_id == connection_id)
                                .is_some_and(|render| render.is_bidirectional)
                            {
                                "bidirectional"
                            } else {
                                "one-way"
                            }
                        ))
                        .size(12),
                    );
                }
            }
            EntityId::Room(room_number) => {
                if let Some(room) = area.get_room(&room_number) {
                    content = content.push(heading(format!("Room #{room_number}")));
                    if !room.get_title().is_empty() {
                        content = content.push(text(room.get_title().to_string()).size(13));
                    }
                    if !room.get_description().is_empty() {
                        content = content.push(text(room.get_description().to_string()).size(12));
                    }
                    content = content.push(
                        text(format!(
                            "Level {} \u{00b7} ({:.1}, {:.1})",
                            room.get_level(),
                            room.get_x(),
                            room.get_y()
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
                        content = content.push(field_label("Properties"));
                        for (name, value) in properties {
                            content = content.push(text(format!("{name}: {value}")).size(12));
                        }
                    }

                    let exits = room.get_exits();
                    if !exits.is_empty() {
                        content = content.push(field_label("Exits"));
                        for exit in exits {
                            content =
                                content.push(text(exit_summary(&atlas, &area, exit)).size(12));
                        }
                    }
                }
            }
            EntityId::Label(label_id) => {
                if let Some(label) = area.get_label(&label_id) {
                    content = content.push(heading("Label".to_string()));
                    content = content.push(text(label.text.clone()).size(13));
                }
            }
            EntityId::Shape(shape_id) => {
                if let Some(shape) = area.get_shape(&shape_id) {
                    content = content.push(heading("Shape".to_string()));
                    content = content.push(
                        text(format!(
                            "{:.0}\u{00d7}{:.0} at ({:.1}, {:.1})",
                            shape.width, shape.height, shape.x, shape.y
                        ))
                        .size(12)
                        .style(muted_text),
                    );
                }
            }
        }
    } else if selection.is_empty() {
        content = content.push(heading(area.get_name().to_string()));
        content = content.push(
            text(format!("{} rooms", area.room_count()))
                .size(12)
                .style(muted_text),
        );
        let mut properties: Vec<(String, String)> = area
            .properties()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        if !properties.is_empty() {
            properties.sort();
            content = content.push(field_label("Area properties"));
            for (name, value) in properties {
                content = content.push(text(format!("{name}: {value}")).size(12));
            }
        }
    } else {
        content = content.push(text(format!("{} entities selected", selection.len())).size(13));
    }

    if let Some(RoomKey { room_number, .. }) = window.hovered_room
        && let Some(room) = area.get_room(&room_number)
    {
        content = content.push(heading(format!("Room #{room_number}")));
        content = content.push(text(room.get_title().to_string()).size(13));
        content = content.push(text(room.get_description().to_string()).size(12));
    }

    content
}
