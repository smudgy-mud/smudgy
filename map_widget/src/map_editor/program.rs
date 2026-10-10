//! The canvas program for [`MapEditor`]: interaction state machine, hit
//! testing dispatch, and rendering (ghost levels, current level, selection
//! outlines, gesture previews).
//!
//! `canvas::Program::update` takes `&self`, so everything mutable during a
//! gesture lives in [`EditorProgramState`]; committed state changes are
//! published as [`Message`]s and applied in [`MapEditor::update`].

use std::sync::Arc;

use iced::keyboard::{self, key::Named};
use iced::widget::canvas::{self, stroke};
use iced::{Color, Point, Rectangle, Size, Vector, mouse};

use iced::event::Event as IcedEvent;

use smudgy_cloud::{
    CORNER_INSET, ConnectionEndpoint, ConnectionId, ConnectionRouting, ConnectionUpdates, MapPoint,
    PortMode, RoomAddress, RoomSide, SegmentShape,
    connection_geometry::{
        EndpointGeometry, GeometryInput, Handle as ConnectionHandle, distance_to_segment,
        port_position, reroute_for_port_move, reroute_for_waypoint_move, resolve, stub_tip,
    },
    mapper::{RoomKey, area_cache::SourceLayer, room_connection::RoomConnection},
};

use crate::{render, sources, viewport};

use super::{
    EntityId, ExitTarget, LinkDrop, MapEditor, Message, PlacedRoom, RectKind, Renderer,
    SelectedConnectionHandle, Theme, Tool, direction_between,
};

/// Screen-space distance (pixels) a press must travel before it becomes a
/// drag rather than a click.
const DRAG_THRESHOLD: f32 = 4.0;

/// Chebyshev distance from a room's center (map units) beyond which a
/// press starts an exit drag instead of a move/select.
const EXIT_BAND_INNER: f32 = render::MAP_ROOM_SIZE * 0.35;

/// Screen-space size (pixels) of a resize handle's hit zone and visual.
const HANDLE_SCREEN_SIZE: f32 = 8.0;

/// Smallest label/shape dimension (map units) a resize or creation drag
/// can produce.
const MIN_RECT_DIMENSION: f32 = 0.5;

/// Stored Connection waypoints use a finer grid than rooms and other map
/// entities.
const CONNECTION_POINT_GRID: f32 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
}

impl HandleKind {
    fn moves_left(self) -> bool {
        matches!(self, Self::West | Self::NorthWest | Self::SouthWest)
    }

    fn moves_right(self) -> bool {
        matches!(self, Self::East | Self::NorthEast | Self::SouthEast)
    }

    fn moves_top(self) -> bool {
        matches!(self, Self::North | Self::NorthWest | Self::NorthEast)
    }

    fn moves_bottom(self) -> bool {
        matches!(self, Self::South | Self::SouthWest | Self::SouthEast)
    }
}

fn handle_positions(rect: Rectangle) -> [(HandleKind, Point); 8] {
    let center_x = rect.x + rect.width / 2.0;
    let center_y = rect.y + rect.height / 2.0;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;

    [
        (HandleKind::NorthWest, Point::new(rect.x, rect.y)),
        (HandleKind::North, Point::new(center_x, rect.y)),
        (HandleKind::NorthEast, Point::new(right, rect.y)),
        (HandleKind::East, Point::new(right, center_y)),
        (HandleKind::SouthEast, Point::new(right, bottom)),
        (HandleKind::South, Point::new(center_x, bottom)),
        (HandleKind::SouthWest, Point::new(rect.x, bottom)),
        (HandleKind::West, Point::new(rect.x, center_y)),
    ]
}

/// The rect that results from dragging `handle` to `target`.
fn resize_rect(original: Rectangle, handle: HandleKind, target: Point) -> Rectangle {
    let mut left = original.x;
    let mut right = original.x + original.width;
    let mut top = original.y;
    let mut bottom = original.y + original.height;

    if handle.moves_left() {
        left = target.x.min(right - MIN_RECT_DIMENSION);
    }
    if handle.moves_right() {
        right = target.x.max(left + MIN_RECT_DIMENSION);
    }
    if handle.moves_top() {
        top = target.y.min(bottom - MIN_RECT_DIMENSION);
    }
    if handle.moves_bottom() {
        bottom = target.y.max(top + MIN_RECT_DIMENSION);
    }

    Rectangle {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }
}

#[derive(Debug, Clone, Default)]
pub enum Interaction {
    #[default]
    Idle,
    Panning {
        translation: Vector,
        start: Point,
        /// The cursor left the drag threshold: a pan, not a right click.
        moved: bool,
    },
    /// A left press on an entity that hasn't crossed the drag threshold.
    PendingSelect {
        entity: EntityId,
        start_screen: Point,
        start_map: Point,
        was_selected: bool,
        additive: bool,
    },
    /// Dragging the current selection; rendered as a preview offset until
    /// release commits a single move mutation.
    DraggingSelection {
        start_map: Point,
        current_map: Point,
    },
    RubberBand {
        start_map: Point,
        current_map: Point,
        additive: bool,
    },
    /// Dragging a new exit out of a room's border band.
    DraggingExit {
        from: PlacedRoom,
        from_center: Point,
        current_map: Point,
    },
    /// Dragging out the bounds of a new label or shape.
    DrawingRect {
        kind: RectKind,
        start_map: Point,
        current_map: Point,
    },
    /// Dragging a resize handle of the selected label/shape.
    DraggingHandle {
        entity: EntityId,
        handle: HandleKind,
        original: Rectangle,
        current_map: Point,
    },
    /// A left press on a Connection port/vertex handle that hasn't crossed
    /// the drag threshold: a bare click selects the handle and must emit no
    /// mutation (and no Automatic→Manual conversion).
    PendingConnectionHandle {
        connection_id: ConnectionId,
        handle: ConnectionHandle,
        start_screen: Point,
    },
    /// A Connection port or stored route vertex. The cache is untouched
    /// during the drag; release emits one coalesced semantic update.
    DraggingConnectionHandle {
        connection_id: ConnectionId,
        handle: ConnectionHandle,
        current_map: Point,
    },
}

#[derive(Default)]
pub struct EditorProgramState {
    interaction: Interaction,
    modifiers: keyboard::Modifiers,
    last_click_point: Option<Point>,
    last_click_hits: Vec<EntityId>,
    last_click_index: usize,
}

impl MapEditor {
    /// Chooses the next overlapping entity for a repeated click. Moving more
    /// than the six-pixel hit tolerance, or changing the candidate set,
    /// starts again at the normal selection precedence.
    fn cycled_entity_at(&self, state: &mut EditorProgramState, point: Point) -> Option<EntityId> {
        let hits = self.entities_at(point);
        if hits.is_empty() {
            state.last_click_point = None;
            state.last_click_hits.clear();
            state.last_click_index = 0;
            return None;
        }

        let repeated = state
            .last_click_point
            .is_some_and(|old| chebyshev(old, point) <= 6.0 / self.scaling)
            && state.last_click_hits == hits;
        let index = if repeated {
            (state.last_click_index + 1) % hits.len()
        } else {
            0
        };
        let entity = hits[index];
        state.last_click_point = Some(point);
        state.last_click_hits = hits;
        state.last_click_index = index;
        Some(entity)
    }

    /// The map-space offset of an in-flight selection drag, snapped to the
    /// grid unless Alt is held.
    fn drag_offset(start: Point, current: Point, modifiers: keyboard::Modifiers) -> Vector {
        let offset = current - start;
        if modifiers.alt() {
            offset
        } else {
            viewport::snap_offset(offset)
        }
    }

    /// A map-space point, snapped unless Alt is held.
    fn maybe_snap(point: Point, modifiers: keyboard::Modifiers) -> Point {
        if modifiers.alt() {
            point
        } else {
            viewport::snap(point)
        }
    }

    fn maybe_snap_connection(point: Point, modifiers: keyboard::Modifiers) -> Point {
        if modifiers.alt() {
            point
        } else {
            viewport::snap_to_step(point, CONNECTION_POINT_GRID)
        }
    }

    /// The resize handle under a map-space point, when a single
    /// label/shape is selected.
    fn handle_at(&self, point: Point) -> Option<(EntityId, HandleKind, Rectangle)> {
        if !self.editable {
            return None;
        }
        let (entity, rect) = self.selected_rect()?;
        let radius = HANDLE_SCREEN_SIZE / self.scaling / 2.0;

        handle_positions(rect)
            .into_iter()
            .find_map(|(kind, position)| {
                (chebyshev(point, position) <= radius).then_some((entity, kind, rect))
            })
    }

    fn connection_handle_at(&self, point: Point) -> Option<(ConnectionId, ConnectionHandle)> {
        if !self.editable {
            return None;
        }
        let EntityId::Connection(connection_id) = self.selection.single()? else {
            return None;
        };
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        // A Secret's link is edited in its own document.
        let (area, _) = area.connection_document(connection_id)?;
        let stored = area.get_connection(connection_id)?;
        let radius = HANDLE_SCREEN_SIZE / self.scaling;
        let point = MapPoint::new(point.x, point.y);
        let render = area.get_room_connections().iter().find(|connection| {
            connection.connection_id == connection_id && connection.from_level == self.level
        })?;
        render.geometry.handles.iter().copied().find_map(|handle| {
            let on_level = match handle {
                ConnectionHandle::PortA(_) => area
                    .get_room_at(stored.endpoint_a.address())
                    .is_some_and(|room| room.get_level() == self.level),
                ConnectionHandle::PortB(_) => stored.endpoint_b.is_some_and(|endpoint| {
                    area.get_room_at(endpoint.address())
                        .is_some_and(|room| room.get_level() == self.level)
                }),
                ConnectionHandle::Waypoint(_, _) => true,
            };
            (on_level && handle.position().distance(point) <= radius)
                .then_some((connection_id, handle))
        })
    }

    fn connection_handle_update(
        &self,
        connection_id: ConnectionId,
        handle: ConnectionHandle,
        current: Point,
        modifiers: keyboard::Modifiers,
    ) -> Option<ConnectionUpdates> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        let (area, _) = area.connection_document(connection_id)?;
        let connection = area.get_connection(connection_id)?;
        match handle {
            ConnectionHandle::Waypoint(index, _) => {
                if index >= connection.route_points.len() {
                    return None;
                }
                let target = Self::maybe_snap_connection(current, modifiers);
                let target = MapPoint::new(target.x, target.y);
                let points = if connection.segment_shape == SegmentShape::Orthogonal {
                    let render = area.get_room_connections().iter().find(|render| {
                        render.connection_id == connection_id && render.from_level == self.level
                    })?;
                    reroute_for_waypoint_move(
                        &connection.route_points,
                        index,
                        render.geometry.stub_tip_a,
                        render.geometry.stub_tip_b,
                        target,
                    )?
                } else {
                    let mut points = connection.route_points.clone();
                    points[index] = target;
                    points
                };
                Some(ConnectionUpdates {
                    routing: Some(ConnectionRouting::Manual),
                    route_points: Some(points),
                    ..ConnectionUpdates::default()
                })
            }
            ConnectionHandle::PortA(_) => {
                let room = area.get_room_at(connection.endpoint_a.address())?;
                let endpoint = endpoint_at_pointer(
                    connection.endpoint_a.address(),
                    Point::new(room.get_x(), room.get_y()),
                    current,
                    !modifiers.alt(),
                );
                let mut route_points = None;
                if connection.segment_shape == SegmentShape::Orthogonal
                    && matches!(
                        connection.routing,
                        ConnectionRouting::Manual | ConnectionRouting::Automatic
                    )
                {
                    let render = area.get_room_connections().iter().find(|render| {
                        render.connection_id == connection_id && render.from_level == self.level
                    })?;
                    let new_tip = stub_tip(
                        port_position(
                            MapPoint::new(room.get_x(), room.get_y()),
                            endpoint.side,
                            endpoint.port_offset,
                        ),
                        endpoint.side,
                    );
                    route_points = Some(reroute_for_port_move(
                        &connection.route_points,
                        render.geometry.stub_tip_a,
                        render.geometry.stub_tip_b,
                        new_tip,
                        false,
                    ));
                }
                Some(ConnectionUpdates {
                    endpoint_a: Some(endpoint),
                    route_points,
                    ..ConnectionUpdates::default()
                })
            }
            ConnectionHandle::PortB(_) => {
                let endpoint_b = connection.endpoint_b?;
                let room = area.get_room_at(endpoint_b.address())?;
                let endpoint = endpoint_at_pointer(
                    endpoint_b.address(),
                    Point::new(room.get_x(), room.get_y()),
                    current,
                    !modifiers.alt(),
                );
                let mut route_points = None;
                if connection.segment_shape == SegmentShape::Orthogonal
                    && matches!(
                        connection.routing,
                        ConnectionRouting::Manual | ConnectionRouting::Automatic
                    )
                {
                    let render = area.get_room_connections().iter().find(|render| {
                        render.connection_id == connection_id && render.from_level == self.level
                    })?;
                    let old_tip = render.geometry.stub_tip_b?;
                    let new_tip = stub_tip(
                        port_position(
                            MapPoint::new(room.get_x(), room.get_y()),
                            endpoint.side,
                            endpoint.port_offset,
                        ),
                        endpoint.side,
                    );
                    route_points = Some(reroute_for_port_move(
                        &connection.route_points,
                        old_tip,
                        Some(render.geometry.stub_tip_a),
                        new_tip,
                        true,
                    ));
                }
                Some(ConnectionUpdates {
                    endpoint_b: Some(endpoint),
                    route_points,
                    ..ConnectionUpdates::default()
                })
            }
        }
    }

    /// Resolves the complete view-only Connection produced by the current
    /// drag. The Mapper cache stays untouched; release still submits one CAS
    /// update through the same pure `connection_handle_update` helper.
    fn connection_drag_preview(&self, state: &EditorProgramState) -> Option<RoomConnection> {
        let Interaction::DraggingConnectionHandle {
            connection_id,
            handle,
            current_map,
        } = &state.interaction
        else {
            return None;
        };
        let updates =
            self.connection_handle_update(*connection_id, *handle, *current_map, state.modifiers)?;
        let atlas = self.mapper.get_current_atlas();
        let map = atlas.get_area(self.area_id.as_ref()?)?;
        let (area, layer) = map.connection_document(*connection_id)?;
        let stored = area.get_connection(*connection_id)?;
        let updated = updates.apply(stored);
        let room_a = area.get_room_at(updated.endpoint_a.address())?;
        let room_b = updated
            .endpoint_b
            .and_then(|endpoint| area.get_room_at(endpoint.address()));
        let mut preview = area
            .get_room_connections()
            .iter()
            .find(|connection| {
                connection.connection_id == *connection_id && connection.from_level == self.level
            })?
            .clone();
        let geometry = resolve(&GeometryInput {
            kind: updated.kind,
            routing: updated.routing,
            corner: updated.corner,
            endpoint_a: EndpointGeometry {
                room_center: MapPoint::new(room_a.get_x(), room_a.get_y()),
                side: updated.endpoint_a.side,
                port_offset: updated.endpoint_a.port_offset,
                stub: preview.stub_a,
            },
            endpoint_b: updated
                .endpoint_b
                .zip(room_b)
                .map(|(endpoint, room)| EndpointGeometry {
                    room_center: MapPoint::new(room.get_x(), room.get_y()),
                    side: endpoint.side,
                    port_offset: endpoint.port_offset,
                    stub: preview.stub_b,
                }),
            route_points: &updated.route_points,
            thickness: updated.thickness,
        });
        preview.geometry = Arc::new(geometry);
        preview.routing = updated.routing;
        preview.corner = updated.corner;
        preview.dash = updated.dash;
        preview.thickness = updated.thickness;
        // A Secret's link keeps its layer's color while it moves.
        if let Some(color) = layer.and_then(|layer| sources::layer_color(&map, layer.source())) {
            preview.color = color;
        }
        Some(preview)
    }

    pub(super) fn waypoint_insertion(
        &self,
        connection_id: ConnectionId,
        point: Point,
        modifiers: keyboard::Modifiers,
    ) -> Option<(usize, Vec<MapPoint>, usize)> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
        let (area, _) = area.connection_document(connection_id)?;
        let connection = area.get_connection(connection_id)?;
        if !matches!(
            connection.routing,
            ConnectionRouting::Simple | ConnectionRouting::Manual | ConnectionRouting::Automatic
        ) {
            return None;
        }
        let render = area.get_room_connections().iter().find(|render| {
            render.connection_id == connection_id && render.from_level == self.level
        })?;
        let tip_b = render.geometry.stub_tip_b?;
        let mut logical = Vec::with_capacity(connection.route_points.len() + 2);
        logical.push(render.geometry.stub_tip_a);
        logical.extend(connection.route_points.iter().copied());
        logical.push(tip_b);
        let point = Self::maybe_snap_connection(point, modifiers);
        let point = MapPoint::new(point.x, point.y);
        let (index, segment) = logical.windows(2).enumerate().min_by(|(_, a), (_, b)| {
            distance_to_segment(point, a[0], a[1])
                .total_cmp(&distance_to_segment(point, b[0], b[1]))
        })?;
        if connection.segment_shape != SegmentShape::Orthogonal {
            return (point != segment[0] && point != segment[1]).then_some((index, vec![point], 0));
        }

        // A movable orthogonal insertion must depart from and rejoin the
        // selected leg. Four explicit elbows are the minimum valid stored
        // detour for a segment whose endpoints remain fixed; a lone projected
        // point would be collinear and impossible to move perpendicular to
        // the leg. Keep a small margin from both existing vertices so no
        // zero-length endpoint leg reaches validation.
        if connection.route_points.len().saturating_add(4) > smudgy_cloud::MAX_ROUTE_POINTS {
            return None;
        }
        let a = segment[0];
        let b = segment[1];
        let horizontal = (a.y - b.y).abs() <= f32::EPSILON;
        let vertical = (a.x - b.x).abs() <= f32::EPSILON;
        if !horizontal && !vertical {
            return None;
        }
        let length = if horizontal {
            (b.x - a.x).abs()
        } else {
            (b.y - a.y).abs()
        };
        if length <= 1e-4 {
            return None;
        }
        let margin = (length * 0.1).min(0.05);
        let span = (length * 0.5).min(0.5);
        let raw_fraction = if horizontal {
            (point.x - a.x) / (b.x - a.x)
        } else {
            (point.y - a.y) / (b.y - a.y)
        };
        let start_distance = (raw_fraction.clamp(0.0, 1.0) * length - span / 2.0)
            .clamp(margin, length - margin - span);
        let end_distance = start_distance + span;
        let direction = if horizontal {
            (b.x - a.x).signum()
        } else {
            (b.y - a.y).signum()
        };
        let normal = if horizontal {
            let delta = point.y - a.y;
            a.y + if delta.abs() >= 0.1 {
                delta
            } else if delta.is_sign_negative() {
                -0.25
            } else {
                0.25
            }
        } else {
            let delta = point.x - a.x;
            a.x + if delta.abs() >= 0.1 {
                delta
            } else if delta.is_sign_negative() {
                -0.25
            } else {
                0.25
            }
        };
        let points = if horizontal {
            let start_x = a.x + direction * start_distance;
            let end_x = a.x + direction * end_distance;
            vec![
                MapPoint::new(start_x, a.y),
                MapPoint::new(start_x, normal),
                MapPoint::new(end_x, normal),
                MapPoint::new(end_x, a.y),
            ]
        } else {
            let start_y = a.y + direction * start_distance;
            let end_y = a.y + direction * end_distance;
            vec![
                MapPoint::new(a.x, start_y),
                MapPoint::new(normal, start_y),
                MapPoint::new(normal, end_y),
                MapPoint::new(a.x, end_y),
            ]
        };
        Some((index, points, 1))
    }

    fn zoom(&self, step: f32, cursor: mouse::Cursor, bounds: Rectangle) -> canvas::Action<Message> {
        if step < 0.0 && self.scaling > Self::MIN_SCALING
            || step > 0.0 && self.scaling < Self::MAX_SCALING
        {
            let old_scaling = self.scaling;

            let scaling =
                (self.scaling * (1.0 + step / 10.0)).clamp(Self::MIN_SCALING, Self::MAX_SCALING);

            let translation = cursor
                .position_from(bounds.center())
                .map(|cursor_to_center| {
                    let factor = scaling - old_scaling;

                    self.translation
                        - Vector::new(
                            cursor_to_center.x * factor / (old_scaling * old_scaling),
                            cursor_to_center.y * factor / (old_scaling * old_scaling),
                        )
                });

            canvas::Action::publish(Message::Scaled(scaling, translation)).and_capture()
        } else {
            canvas::Action::capture()
        }
    }

    /// Finishes the in-flight gesture on left-button release. Runs before
    /// the cursor-in-bounds gate so releases outside the canvas still
    /// commit (the gesture coordinates are tracked in map space).
    fn finish_gesture(&self, state: &mut EditorProgramState) -> Option<canvas::Action<Message>> {
        match std::mem::take(&mut state.interaction) {
            Interaction::PendingSelect {
                entity,
                was_selected,
                additive,
                ..
            } => {
                // Selection of a not-yet-selected entity already happened
                // on press; a plain click on a selected entity collapses
                // (or toggles, with Shift) on release.
                if was_selected {
                    Some(
                        canvas::Action::publish(Message::ClickSelect { entity, additive })
                            .and_capture(),
                    )
                } else {
                    Some(canvas::Action::request_redraw().and_capture())
                }
            }
            Interaction::DraggingSelection {
                start_map,
                current_map,
            } => {
                let offset = Self::drag_offset(start_map, current_map, state.modifiers);
                if offset == Vector::new(0.0, 0.0) {
                    Some(canvas::Action::request_redraw().and_capture())
                } else {
                    Some(canvas::Action::publish(Message::MoveCommitted { offset }).and_capture())
                }
            }
            Interaction::RubberBand {
                start_map,
                current_map,
                additive,
            } => {
                let rect = rect_from_corners(start_map, current_map);
                Some(
                    canvas::Action::publish(Message::RubberBandSelect { rect, additive })
                        .and_capture(),
                )
            }
            Interaction::DraggingExit {
                from,
                from_center,
                current_map,
            } => {
                let target = match self.link_drop(
                    from,
                    current_map,
                    state.modifiers.alt(),
                    state.modifiers.shift(),
                ) {
                    LinkDrop::Nothing => None,
                    LinkDrop::Room(room, center) => Some((ExitTarget::Room(room), center)),
                    LinkDrop::Empty(at) => Some((ExitTarget::Empty(at), at)),
                    LinkDrop::Dangling(at) => Some((ExitTarget::Dangling(at), at)),
                };

                Some(target.map_or_else(
                    || canvas::Action::request_redraw().and_capture(),
                    |(to, target_center)| {
                        let from_direction = direction_between(from_center, target_center);
                        canvas::Action::publish(Message::ExitDragCommitted {
                            from,
                            from_direction,
                            to,
                            to_direction: from_direction.opposite(),
                            one_way: state.modifiers.control()
                                || matches!(to, ExitTarget::Dangling(_)),
                        })
                        .and_capture()
                    },
                ))
            }
            Interaction::DrawingRect {
                kind,
                start_map,
                current_map,
            } => {
                let a = Self::maybe_snap(start_map, state.modifiers);
                let b = Self::maybe_snap(current_map, state.modifiers);
                let mut rect = rect_from_corners(a, b);
                rect.width = rect.width.max(MIN_RECT_DIMENSION);
                rect.height = rect.height.max(MIN_RECT_DIMENSION);

                Some(
                    canvas::Action::publish(Message::RectDrawn {
                        kind,
                        rect,
                        keep_tool: state.modifiers.shift(),
                    })
                    .and_capture(),
                )
            }
            Interaction::DraggingHandle {
                entity,
                handle,
                original,
                current_map,
            } => {
                let target = Self::maybe_snap(current_map, state.modifiers);
                let rect = resize_rect(original, handle, target);
                Some(
                    canvas::Action::publish(Message::ResizeCommitted { entity, rect })
                        .and_capture(),
                )
            }
            // A click that never crossed the drag threshold: the handle was
            // selected on press; no mutation, no undo entry, no
            // Automatic→Manual conversion.
            Interaction::PendingConnectionHandle { .. } => {
                Some(canvas::Action::request_redraw().and_capture())
            }
            Interaction::DraggingConnectionHandle {
                connection_id,
                handle,
                current_map,
            } => {
                let waypoint = matches!(handle, ConnectionHandle::Waypoint(_, _));
                Some(
                    self.connection_handle_update(
                        connection_id,
                        handle,
                        current_map,
                        state.modifiers,
                    )
                    .map_or_else(
                        || {
                            canvas::Action::publish(Message::ActivityChanged(
                                super::EditorActivity::Idle,
                            ))
                            .and_capture()
                        },
                        |updates| {
                            canvas::Action::publish(Message::ConnectionUpdated {
                                connection_id,
                                updates,
                                description: if waypoint {
                                    "Move connection waypoint"
                                } else {
                                    "Move connection port"
                                },
                            })
                            .and_capture()
                        },
                    ),
                )
            }
            Interaction::Panning { .. } | Interaction::Idle => None,
        }
    }
}

impl canvas::Program<Message, Theme> for MapEditor {
    type State = EditorProgramState;

    fn update(
        &self,
        state: &mut EditorProgramState,
        event: &IcedEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        // Track modifiers before the cursor gate so the state stays fresh
        // even while the cursor is outside the canvas.
        if let IcedEvent::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
            if matches!(
                state.interaction,
                Interaction::DraggingConnectionHandle { .. }
            ) {
                return Some(canvas::Action::request_redraw().and_capture());
            }
        }

        // Escape cancels an in-flight gesture; when idle it is left for the
        // host window (tool revert / selection clear).
        if let IcedEvent::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(Named::Escape),
            ..
        }) = event
        {
            if !matches!(state.interaction, Interaction::Idle) {
                let connection_drag = matches!(
                    state.interaction,
                    Interaction::DraggingConnectionHandle { .. }
                );
                state.interaction = Interaction::Idle;
                return Some(if connection_drag {
                    canvas::Action::publish(Message::ActivityChanged(super::EditorActivity::Idle))
                        .and_capture()
                } else {
                    canvas::Action::request_redraw().and_capture()
                });
            }
            return None;
        }

        // Hover state is only republished on in-canvas cursor movement, so
        // the cursor leaving the canvas must clear it explicitly or the
        // hover glow outlives the pointer.
        if matches!(event, IcedEvent::Mouse(mouse::Event::CursorLeft))
            && (self.hovered_room.is_some() || self.hovered_connection.is_some())
        {
            return Some(canvas::Action::publish(Message::SetHovered {
                room: None,
                connection: None,
            }));
        }

        // Releases finish gestures even when the cursor has left the canvas.
        if let IcedEvent::Mouse(mouse::Event::ButtonReleased(button)) = event {
            match button {
                mouse::Button::Left => {
                    if let Some(action) = self.finish_gesture(state) {
                        return Some(action);
                    }
                }
                mouse::Button::Right => {
                    // Only a pan ends here; a left drag in progress stays.
                    if let Interaction::Panning {
                        translation, moved, ..
                    } = state.interaction.clone()
                    {
                        state.interaction = Interaction::Idle;
                        // A right press released where it began is a right
                        // click: the context menu, with the view the press
                        // had.
                        if !moved && let Some(at) = cursor.position_in(bounds) {
                            let map = viewport::Viewport {
                                translation,
                                scaling: self.scaling,
                            }
                            .project(at, bounds.size());
                            return Some(
                                canvas::Action::publish(Message::ContextMenuRequested {
                                    at,
                                    map,
                                    translation,
                                })
                                .and_capture(),
                            );
                        }
                        return Some(canvas::Action::request_redraw().and_capture());
                    }
                }
                _ => {}
            }
        }

        let cursor_position = cursor.position_in(bounds)?;
        let map_position = self.viewport().project(cursor_position, bounds.size());

        match event {
            IcedEvent::Mouse(mouse_event) => match mouse_event {
                mouse::Event::ButtonPressed(button) => match button {
                    mouse::Button::Right => {
                        // A right press abandons an in-flight connection
                        // drag; tell the host so the drag legend clears.
                        let was_connection_drag = matches!(
                            state.interaction,
                            Interaction::DraggingConnectionHandle { .. }
                        );
                        state.interaction = Interaction::Panning {
                            translation: self.translation,
                            start: cursor_position,
                            moved: false,
                        };

                        Some(if was_connection_drag {
                            canvas::Action::publish(Message::ActivityChanged(
                                super::EditorActivity::Idle,
                            ))
                            .and_capture()
                        } else {
                            canvas::Action::request_redraw().and_capture()
                        })
                    }
                    mouse::Button::Left => match self.tool {
                        Tool::Select if self.picking => {
                            Some(self.picked_room_at(map_position).map_or_else(
                                canvas::Action::capture,
                                |room| {
                                    canvas::Action::publish(Message::RoomPicked(room)).and_capture()
                                },
                            ))
                        }
                        Tool::Select => {
                            // Connection handles take priority over all other
                            // hit targets, followed by resize handles.
                            if let Some((connection_id, handle)) =
                                self.connection_handle_at(map_position)
                            {
                                state.interaction = Interaction::PendingConnectionHandle {
                                    connection_id,
                                    handle,
                                    start_screen: cursor_position,
                                };
                                return Some(
                                    canvas::Action::publish(Message::ConnectionHandleSelected {
                                        connection_id,
                                        handle: match handle {
                                            ConnectionHandle::PortA(_) => {
                                                SelectedConnectionHandle::PortA
                                            }
                                            ConnectionHandle::PortB(_) => {
                                                SelectedConnectionHandle::PortB
                                            }
                                            ConnectionHandle::Waypoint(index, _) => {
                                                SelectedConnectionHandle::Waypoint(index)
                                            }
                                        },
                                    })
                                    .and_capture(),
                                );
                            }

                            if let Some((entity, handle, rect)) = self.handle_at(map_position) {
                                state.interaction = Interaction::DraggingHandle {
                                    entity,
                                    handle,
                                    original: rect,
                                    current_map: map_position,
                                };
                                return Some(canvas::Action::request_redraw().and_capture());
                            }

                            if let Some(entity) = self.cycled_entity_at(state, map_position) {
                                if self.editable
                                    && state.modifiers.control()
                                    && let EntityId::Connection(connection_id) = entity
                                    && let Some((index, points, selected_offset)) = self
                                        .waypoint_insertion(
                                            connection_id,
                                            map_position,
                                            state.modifiers,
                                        )
                                {
                                    return Some(
                                        canvas::Action::publish(Message::WaypointInserted {
                                            connection_id,
                                            index,
                                            points,
                                            selected_offset,
                                        })
                                        .and_capture(),
                                    );
                                }
                                let was_selected = self.selection.contains(entity);
                                let additive = state.modifiers.shift();

                                state.interaction = Interaction::PendingSelect {
                                    entity,
                                    start_screen: cursor_position,
                                    start_map: map_position,
                                    was_selected,
                                    additive,
                                };

                                if was_selected {
                                    Some(canvas::Action::request_redraw().and_capture())
                                } else {
                                    Some(
                                        canvas::Action::publish(Message::ClickSelect {
                                            entity,
                                            additive,
                                        })
                                        .and_capture(),
                                    )
                                }
                            } else if let Some((room, level)) = self.ghost_room_at(map_position) {
                                state.interaction = Interaction::Idle;
                                Some(
                                    canvas::Action::publish(Message::GhostRoomSelected {
                                        room,
                                        level,
                                    })
                                    .and_capture(),
                                )
                            } else {
                                state.interaction = Interaction::RubberBand {
                                    start_map: map_position,
                                    current_map: map_position,
                                    additive: state.modifiers.shift(),
                                };

                                Some(canvas::Action::request_redraw().and_capture())
                            }
                        }
                        Tool::Link => {
                            if let Some((from, from_center)) = self.link_end_at(map_position)
                                && chebyshev(map_position, from_center) > EXIT_BAND_INNER
                            {
                                state.interaction = Interaction::DraggingExit {
                                    from,
                                    from_center,
                                    current_map: map_position,
                                };
                                Some(canvas::Action::request_redraw().and_capture())
                            } else {
                                Some(canvas::Action::capture())
                            }
                        }
                        Tool::AddRoom => {
                            let at = if state.modifiers.alt() {
                                map_position
                            } else {
                                viewport::snap(map_position)
                            };
                            Some(
                                canvas::Action::publish(Message::PlaceRoom {
                                    at,
                                    keep_tool: state.modifiers.shift(),
                                })
                                .and_capture(),
                            )
                        }
                        Tool::AddLabel | Tool::AddShape => {
                            state.interaction = Interaction::DrawingRect {
                                kind: if self.tool == Tool::AddLabel {
                                    RectKind::Label
                                } else {
                                    RectKind::Shape
                                },
                                start_map: map_position,
                                current_map: map_position,
                            };
                            Some(canvas::Action::request_redraw().and_capture())
                        }
                    },
                    _ => None,
                },
                mouse::Event::CursorMoved { .. } => match &mut state.interaction {
                    Interaction::Panning {
                        translation,
                        start,
                        moved,
                    } => {
                        let travel = cursor_position - *start;
                        if travel.x.abs() > DRAG_THRESHOLD || travel.y.abs() > DRAG_THRESHOLD {
                            *moved = true;
                        }
                        let translation = *translation + travel * (1.0 / self.scaling);
                        Some(
                            canvas::Action::publish(Message::Translated(translation)).and_capture(),
                        )
                    }
                    Interaction::PendingSelect {
                        entity,
                        start_screen,
                        start_map,
                        ..
                    } => {
                        if (cursor_position - *start_screen).x.abs() > DRAG_THRESHOLD
                            || (cursor_position - *start_screen).y.abs() > DRAG_THRESHOLD
                        {
                            // A Connection has no independently movable
                            // position: only its ports and waypoints do. Keep
                            // a line press as selection instead of emitting an
                            // empty movement command.
                            if self.editable && !matches!(entity, EntityId::Connection(_)) {
                                state.interaction = Interaction::DraggingSelection {
                                    start_map: *start_map,
                                    current_map: map_position,
                                };
                            }
                        }
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    Interaction::PendingConnectionHandle {
                        connection_id,
                        handle,
                        start_screen,
                    } => {
                        if (cursor_position - *start_screen).x.abs() > DRAG_THRESHOLD
                            || (cursor_position - *start_screen).y.abs() > DRAG_THRESHOLD
                        {
                            let (connection_id, handle) = (*connection_id, *handle);
                            state.interaction = Interaction::DraggingConnectionHandle {
                                connection_id,
                                handle,
                                current_map: map_position,
                            };
                            // The drag legend appears when a drag actually
                            // starts — a bare click never was one.
                            return Some(
                                canvas::Action::publish(Message::ActivityChanged(match handle {
                                    ConnectionHandle::Waypoint(..) => {
                                        super::EditorActivity::DraggingConnectionWaypoint
                                    }
                                    ConnectionHandle::PortA(_) | ConnectionHandle::PortB(_) => {
                                        super::EditorActivity::DraggingConnectionPort
                                    }
                                }))
                                .and_capture(),
                            );
                        }
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    Interaction::DraggingSelection { current_map, .. } => {
                        *current_map = map_position;
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    Interaction::RubberBand { current_map, .. }
                    | Interaction::DraggingExit { current_map, .. }
                    | Interaction::DrawingRect { current_map, .. }
                    | Interaction::DraggingHandle { current_map, .. }
                    | Interaction::DraggingConnectionHandle { current_map, .. } => {
                        *current_map = map_position;
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    Interaction::Idle => {
                        let top = self.entity_at(map_position);
                        let room = match (top, self.area_id) {
                            (Some(EntityId::Room(room_number)), Some(area_id)) => Some(RoomKey {
                                area_id,
                                room_number,
                            }),
                            _ => None,
                        };
                        let connection = match top {
                            Some(EntityId::Connection(id)) if self.tool == Tool::Select => Some(id),
                            _ => None,
                        };
                        if room == self.hovered_room && connection == self.hovered_connection {
                            Some(canvas::Action::request_redraw())
                        } else {
                            Some(canvas::Action::publish(Message::SetHovered {
                                room,
                                connection,
                            }))
                        }
                    }
                },
                // Trackpads report pixel deltas; without a modifier held,
                // two-finger scroll pans the map. Command/Ctrl + scroll and
                // mouse-wheel line deltas zoom.
                mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x, y },
                } if !state.modifiers.command() && !state.modifiers.control() => {
                    let translation =
                        self.translation + Vector::new(x / self.scaling, y / self.scaling);

                    Some(canvas::Action::publish(Message::Translated(translation)).and_capture())
                }
                mouse::Event::WheelScrolled { delta } => match *delta {
                    mouse::ScrollDelta::Lines { y, .. } => Some(self.zoom(y, cursor, bounds)),
                    mouse::ScrollDelta::Pixels { y, .. } => {
                        Some(self.zoom((y / 30.0).clamp(-1.0, 1.0), cursor, bounds))
                    }
                },
                _ => None,
            },
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &EditorProgramState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        self.last_viewport_size.set(Some(bounds.size()));
        let atlas = self.mapper.get_current_atlas();

        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);

        let area = self.area_id.as_ref().and_then(|id| atlas.get_area(id));

        if let Some(area) = area {
            let layers = sources::colored_layers(&area);
            let drag_preview = self.connection_drag_preview(state);
            frame.with_save(|frame| {
                frame.translate(center);
                frame.scale(self.scaling);
                frame.translate(self.translation);
                frame.scale(1.0_f32);

                let region = self.viewport().visible_region(bounds.size());
                let min_x = region.x - Self::SPATIAL_QUERY_PADDING;
                let min_y = region.y - Self::SPATIAL_QUERY_PADDING;
                let max_x = region.x + region.width + Self::SPATIAL_QUERY_PADDING;
                let max_y = region.y + region.height + Self::SPATIAL_QUERY_PADDING;

                render::draw_grid(frame, &region, self.scaling);

                let drag_offset = match &state.interaction {
                    Interaction::DraggingSelection {
                        start_map,
                        current_map,
                    } => Some(Self::drag_offset(*start_map, *current_map, state.modifiers)),
                    _ => None,
                };

                // Ghosted adjacent levels, below then above.
                for ghost_level in [self.level - 1, self.level + 1] {
                    let opacity = Self::GHOST_OPACITY;

                    for shape in area.get_shapes() {
                        if shape.level == ghost_level {
                            render::draw_shape(frame, shape, opacity);
                        }
                    }
                    for label in area.get_labels() {
                        if label.level == ghost_level {
                            render::draw_label(frame, label, opacity);
                        }
                    }
                    sources::draw_drawings(frame, &layers, ghost_level, 1.0, opacity, &|_| false);
                    area.with_room_connections_in(min_x, min_y, max_x, max_y, |connection| {
                        if connection.from_level == ghost_level {
                            render::draw_connection(frame, &atlas, connection, opacity, true);
                        }
                    });
                    sources::draw_connections(
                        frame,
                        &atlas,
                        &layers,
                        ghost_level,
                        (min_x, min_y, max_x, max_y),
                        1.0,
                        opacity,
                        &|_| false,
                    );
                    area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                        if room.get_level() == ghost_level {
                            render::draw_room(frame, room, opacity);
                        }
                    });
                    sources::draw_rooms(
                        frame,
                        &layers,
                        ghost_level,
                        (min_x, min_y, max_x, max_y),
                        1.0,
                        opacity,
                        &|_, _| false,
                    );
                }

                // Current level.
                for shape in area.get_shapes() {
                    if shape.level == self.level
                        && !(drag_offset.is_some()
                            && self.selection.contains(EntityId::Shape(shape.id)))
                    {
                        render::draw_shape(frame, shape, 1.0);
                    }
                }
                for label in area.get_labels() {
                    if label.level == self.level
                        && !(drag_offset.is_some()
                            && self.selection.contains(EntityId::Label(label.id)))
                    {
                        render::draw_label(frame, label, 1.0);
                    }
                }
                sources::draw_drawings(frame, &layers, self.level, 1.0, 1.0, &|drawing| {
                    drag_offset.is_some()
                        && self.selection.contains(match drawing {
                            sources::Drawing::Label(id) => EntityId::Label(id),
                            sources::Drawing::Shape(id) => EntityId::Shape(id),
                        })
                });
                area.with_room_connections_in(min_x, min_y, max_x, max_y, |connection| {
                    if connection.from_level == self.level
                        && drag_preview
                            .as_ref()
                            .is_none_or(|preview| preview.connection_id != connection.connection_id)
                    {
                        render::draw_connection(frame, &atlas, connection, 1.0, false);
                    }
                });
                if let Some(preview) = &drag_preview {
                    render::draw_connection(frame, &atlas, preview, 1.0, false);
                }
                if let Some((connection_id, geometry)) = &self.automatic_route_preview
                    && let Some(connection) = area.get_room_connections().iter().find(|candidate| {
                        candidate.connection_id == *connection_id
                            && candidate.from_level == self.level
                    })
                {
                    let mut preview = connection.clone();
                    preview.geometry = geometry.clone();
                    preview.routing = ConnectionRouting::Automatic;
                    preview.color = theme.styles.general.accent;
                    preview.thickness = preview.thickness.max(2.0);
                    render::draw_connection(frame, &atlas, &preview, 0.9, false);
                }
                // A dragged Secret link draws as its preview above.
                sources::draw_connections(
                    frame,
                    &atlas,
                    &layers,
                    self.level,
                    (min_x, min_y, max_x, max_y),
                    1.0,
                    1.0,
                    &|id| {
                        drag_preview
                            .as_ref()
                            .is_some_and(|preview| preview.connection_id == id)
                    },
                );
                area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                    if room.get_level() == self.level
                        && !(drag_offset.is_some()
                            && self
                                .selection
                                .contains(EntityId::Room(room.get_room_number())))
                    {
                        render::draw_room(frame, room, 1.0);
                    }
                });

                sources::draw_rooms(
                    frame,
                    &layers,
                    self.level,
                    (min_x, min_y, max_x, max_y),
                    1.0,
                    1.0,
                    &|source, number| {
                        drag_offset.is_some()
                            && self
                                .selection
                                .contains(EntityId::SourceRoom(source, number))
                    },
                );

                // Player marker.
                if let Some(room_key) = self
                    .player_location
                    .as_ref()
                    .filter(|key| Some(key.area_id) == self.area_id)
                    && let Some(room) = area.get_room(&room_key.room_number)
                    && room.get_level() == self.level
                {
                    render::draw_player_indicator(frame, room.get_x(), room.get_y(), 1.0);
                }

                // Selection: dragged entities render offset; otherwise
                // outline them in place.
                let accent = theme.styles.general.accent;

                // Hovered Connection: a muted accent glow that makes the
                // otherwise invisible hit band discoverable. Selected
                // connections already get the full outline below.
                if let Some(hovered) = self.hovered_connection
                    && self.tool == Tool::Select
                    && matches!(state.interaction, Interaction::Idle)
                    && !self.selection.contains(EntityId::Connection(hovered))
                    && let Some(connection) = self.drawn_connection(&area, hovered)
                {
                    Self::stroke_resolved_connection_outline(
                        frame,
                        &connection,
                        render::apply_opacity(accent, 0.35),
                        false,
                    );
                }

                if let Some(offset) = drag_offset {
                    frame.with_save(|frame| {
                        frame.translate(offset);
                        self.draw_selected_entities(frame, area.as_ref(), accent);
                    });
                } else {
                    self.draw_selection_outlines(
                        frame,
                        area.as_ref(),
                        drag_preview.as_ref(),
                        accent,
                    );
                }

                // Rubber-band preview.
                if let Interaction::RubberBand {
                    start_map,
                    current_map,
                    ..
                } = &state.interaction
                {
                    let rect = rect_from_corners(*start_map, *current_map);
                    let path = canvas::Path::rectangle(
                        Point::new(rect.x, rect.y),
                        Size::new(rect.width, rect.height),
                    );
                    frame.fill(&path, render::apply_opacity(accent, 0.1));
                    frame.stroke(&path, render::solid_stroke(accent, 1.0));
                }

                // Exit-drag preview: a line from the source room toward the
                // cursor, highlighting the drop target (or the room that
                // would be created).
                if let Interaction::DraggingExit {
                    from,
                    from_center,
                    current_map,
                } = &state.interaction
                {
                    let target = self.link_drop(
                        *from,
                        *current_map,
                        state.modifiers.alt(),
                        state.modifiers.shift(),
                    );
                    let end = match target {
                        LinkDrop::Room(_, center) => center,
                        LinkDrop::Nothing => *current_map,
                        LinkDrop::Empty(at) | LinkDrop::Dangling(at) => at,
                    };

                    let path = canvas::Path::line(*from_center, end);
                    frame.stroke(&path, render::solid_stroke(accent, 2.0));
                    render::draw_arrow_head(
                        frame,
                        Vector::new(from_center.x, from_center.y),
                        Vector::new(end.x, end.y),
                        accent,
                        0.1,
                    );

                    match target {
                        LinkDrop::Room(_, center) => {
                            let half = render::MAP_ROOM_SIZE / 2.0 + 0.06;
                            let path = canvas::Path::rounded_rectangle(
                                Point::new(center.x - half, center.y - half),
                                Size::new(half * 2.0, half * 2.0),
                                render::MAP_ROOM_BORDER_RADIUS.into(),
                            );
                            frame.stroke(&path, render::solid_stroke(accent, 2.0));
                        }
                        LinkDrop::Nothing | LinkDrop::Dangling(_) => {}
                        LinkDrop::Empty(_) => {
                            let path = canvas::Path::rounded_rectangle(
                                Point::new(
                                    end.x - render::MAP_ROOM_SIZE / 2.0,
                                    end.y - render::MAP_ROOM_SIZE / 2.0,
                                ),
                                render::MAP_ROOM_SIZE_AS_SIZE,
                                render::MAP_ROOM_BORDER_RADIUS.into(),
                            );
                            frame.fill(&path, render::apply_opacity(accent, 0.3));
                            frame.stroke(&path, render::solid_stroke(accent, 1.0));
                        }
                    }
                }

                // Drag-rect creation preview.
                if let Interaction::DrawingRect {
                    start_map,
                    current_map,
                    ..
                } = &state.interaction
                {
                    let a = Self::maybe_snap(*start_map, state.modifiers);
                    let b = Self::maybe_snap(*current_map, state.modifiers);
                    let rect = rect_from_corners(a, b);
                    let path = canvas::Path::rectangle(
                        Point::new(rect.x, rect.y),
                        Size::new(rect.width, rect.height),
                    );
                    frame.fill(&path, render::apply_opacity(accent, 0.15));
                    frame.stroke(&path, render::solid_stroke(accent, 1.0));
                }

                // Resize preview and handles for the selected label/shape.
                if let Interaction::DraggingHandle {
                    handle,
                    original,
                    current_map,
                    ..
                } = &state.interaction
                {
                    let target = Self::maybe_snap(*current_map, state.modifiers);
                    let rect = resize_rect(*original, *handle, target);
                    let path = canvas::Path::rectangle(
                        Point::new(rect.x, rect.y),
                        Size::new(rect.width, rect.height),
                    );
                    frame.stroke(&path, render::solid_stroke(accent, 2.0));
                } else if self.editable
                    && drag_offset.is_none()
                    && let Some((_, rect)) = self.selected_rect()
                {
                    let half = HANDLE_SCREEN_SIZE / self.scaling / 2.0;
                    for (_, position) in handle_positions(rect) {
                        let path = canvas::Path::rectangle(
                            Point::new(position.x - half, position.y - half),
                            Size::new(half * 2.0, half * 2.0),
                        );
                        frame.fill(&path, accent);
                    }
                }

                // Selected Connection ports and logical route vertices use a
                // stable screen-space target. A drag's handles and full stroke
                // both come from the same temporary resolved geometry.
                if self.editable
                    && let Some(EntityId::Connection(connection_id)) = self.selection.single()
                    && let Some(connection_render) = drag_preview
                        .clone()
                        .filter(|preview| preview.connection_id == connection_id)
                        .or_else(|| self.drawn_connection(&area, connection_id))
                {
                    let radius = HANDLE_SCREEN_SIZE / self.scaling / 2.0;
                    for handle in &connection_render.geometry.handles {
                        let position = handle.position();
                        let path = canvas::Path::circle(Point::new(position.x, position.y), radius);
                        frame.fill(&path, accent);
                        frame.stroke(
                            &path,
                            // Stroke widths are pixel-space: a crisp 1-px ring.
                            render::solid_stroke(Color::WHITE, 1.0),
                        );
                    }
                }

                // Placement ghost for the room tool.
                if self.tool == Tool::AddRoom
                    && let Some(cursor_position) = cursor.position_in(bounds)
                {
                    let map_position = self.viewport().project(cursor_position, bounds.size());
                    let at = if state.modifiers.alt() {
                        map_position
                    } else {
                        viewport::snap(map_position)
                    };
                    // A click on an occupied cell selects its room: outline
                    // that room rather than promise a new one on top of it.
                    if let Some((_, center)) = self.room_occupying(at) {
                        let half = render::MAP_ROOM_SIZE / 2.0 + 0.06;
                        let path = canvas::Path::rounded_rectangle(
                            Point::new(center.x - half, center.y - half),
                            Size::new(half * 2.0, half * 2.0),
                            render::MAP_ROOM_BORDER_RADIUS.into(),
                        );
                        frame.stroke(&path, render::solid_stroke(accent, 2.0));
                    } else {
                        let path = canvas::Path::rounded_rectangle(
                            Point::new(
                                at.x - render::MAP_ROOM_SIZE / 2.0,
                                at.y - render::MAP_ROOM_SIZE / 2.0,
                            ),
                            render::MAP_ROOM_SIZE_AS_SIZE,
                            render::MAP_ROOM_BORDER_RADIUS.into(),
                        );
                        frame.fill(&path, render::apply_opacity(accent, 0.3));
                        frame.stroke(&path, render::solid_stroke(accent, 1.0));
                    }
                }
            });
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &EditorProgramState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match state.interaction {
            Interaction::Panning { .. } => mouse::Interaction::Grabbing,
            Interaction::DraggingSelection { .. } => mouse::Interaction::Move,
            Interaction::RubberBand { .. }
            | Interaction::DraggingExit { .. }
            | Interaction::DrawingRect { .. } => mouse::Interaction::Crosshair,
            Interaction::DraggingHandle { handle, .. } => resize_cursor(handle),
            Interaction::DraggingConnectionHandle { .. }
            | Interaction::PendingConnectionHandle { .. } => mouse::Interaction::Grabbing,
            _ => {
                if let Some(cursor_position) = cursor.position_in(bounds) {
                    let map_position = self.viewport().project(cursor_position, bounds.size());
                    if self.picking {
                        return if self.picked_room_at(map_position).is_some() {
                            mouse::Interaction::Crosshair
                        } else {
                            mouse::Interaction::default()
                        };
                    }
                    if self.connection_handle_at(map_position).is_some() {
                        return mouse::Interaction::Grab;
                    }
                    if let Some((_, handle, _)) = self.handle_at(map_position) {
                        return resize_cursor(handle);
                    }
                    if self.tool == Tool::Link
                        && let Some((_, center)) = self.link_end_at(map_position)
                    {
                        return if chebyshev(map_position, center) > EXIT_BAND_INNER {
                            mouse::Interaction::Crosshair
                        } else {
                            mouse::Interaction::Pointer
                        };
                    }
                    if self.entity_at(map_position).is_some() {
                        return mouse::Interaction::Pointer;
                    }
                    if self.tool == Tool::Select && self.ghost_room_at(map_position).is_some() {
                        return mouse::Interaction::Pointer;
                    }
                }
                mouse::Interaction::default()
            }
        }
    }
}

/// The wall offsets a snapped port drag can land on: the wall midpoint and
/// the two corner-inset slots every automatic anchor uses.
const PORT_SNAP_OFFSETS: [f32; 3] = [CORNER_INSET, 0.5, 1.0 - CORNER_INSET];

fn endpoint_at_pointer(
    room: RoomAddress,
    center: Point,
    pointer: Point,
    snap: bool,
) -> ConnectionEndpoint {
    let half = render::MAP_ROOM_SIZE / 2.0;
    let left = center.x - half;
    let right = center.x + half;
    let top = center.y - half;
    let bottom = center.y + half;
    let candidates = [
        (RoomSide::North, (pointer.y - top).abs()),
        (RoomSide::East, (pointer.x - right).abs()),
        (RoomSide::South, (pointer.y - bottom).abs()),
        (RoomSide::West, (pointer.x - left).abs()),
    ];
    let side = candidates
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(RoomSide::East, |(side, _)| side);
    let mut port_offset = match side {
        RoomSide::North | RoomSide::South => (pointer.x - left) / render::MAP_ROOM_SIZE,
        RoomSide::East | RoomSide::West => (pointer.y - top) / render::MAP_ROOM_SIZE,
    }
    .clamp(0.0, 1.0);
    if snap {
        port_offset = PORT_SNAP_OFFSETS
            .into_iter()
            .min_by(|a, b| (a - port_offset).abs().total_cmp(&(b - port_offset).abs()))
            .unwrap_or(port_offset);
    }
    ConnectionEndpoint {
        source: room.wire_source(),
        room_number: room.number,
        side,
        port_offset,
        port_mode: PortMode::Manual,
    }
}

impl MapEditor {
    /// Draws every selected entity (used translated for drag previews).
    fn draw_selected_entities(
        &self,
        frame: &mut canvas::Frame,
        area: &smudgy_cloud::mapper::area_cache::AreaCache,
        accent: Color,
    ) {
        for entity in self.selection.iter() {
            match entity {
                EntityId::Connection(id) => {
                    self.stroke_connection_outline(frame, area, id, accent);
                }
                EntityId::Room(number) => {
                    if let Some(room) = area.get_room(&number) {
                        render::draw_room(frame, room, 1.0);
                        self.stroke_room_outline(
                            frame,
                            room.get_x(),
                            room.get_y(),
                            ROOM_SELECTION_MARGIN,
                            accent,
                        );
                    }
                }
                EntityId::SourceRoom(source, number) => {
                    if let Some(room) = sources::source_room(area, source, number) {
                        render::draw_room(frame, room, 1.0);
                        if let Some(color) = sources::layer_color(area, source) {
                            sources::room_ring(frame, room.get_x(), room.get_y(), color, 1.0);
                        }
                        self.stroke_room_outline(
                            frame,
                            room.get_x(),
                            room.get_y(),
                            sources::ring_clearance(self.scaling),
                            accent,
                        );
                    }
                }
                EntityId::Label(id) => {
                    if let Some((layer, label)) = area.find_label(&id) {
                        render::draw_label(frame, label, 1.0);
                        let rect = Rectangle::new(
                            Point::new(label.x, label.y),
                            Size::new(label.width, label.height),
                        );
                        let source = SourceLayer::source_of(layer);
                        self.outline_drawing(frame, area, source, rect, 0.0, true, accent);
                    }
                }
                EntityId::Shape(id) => {
                    if let Some((layer, shape)) = area.find_shape(&id) {
                        render::draw_shape(frame, shape, 1.0);
                        let rect = Rectangle::new(
                            Point::new(shape.x, shape.y),
                            Size::new(shape.width, shape.height),
                        );
                        self.outline_drawing(
                            frame,
                            area,
                            SourceLayer::source_of(layer),
                            rect,
                            shape.border_radius,
                            true,
                            accent,
                        );
                    }
                }
            }
        }
    }

    /// Outlines every selected entity in place.
    fn draw_selection_outlines(
        &self,
        frame: &mut canvas::Frame,
        area: &smudgy_cloud::mapper::area_cache::AreaCache,
        connection_preview: Option<&RoomConnection>,
        accent: Color,
    ) {
        for entity in self.selection.iter() {
            match entity {
                EntityId::Connection(id) => {
                    if let Some(preview) = connection_preview.filter(|preview| {
                        preview.connection_id == id && preview.from_level == self.level
                    }) {
                        // Preview during a drag: also stroke the (normally
                        // hidden) wall stubs so a cross-level port drag has
                        // moving feedback beyond the handle dot.
                        Self::stroke_resolved_connection_outline(frame, preview, accent, true);
                    } else {
                        self.stroke_connection_outline(frame, area, id, accent);
                    }
                }
                EntityId::Room(number) => {
                    if let Some(room) = area.get_room(&number) {
                        self.stroke_room_outline(
                            frame,
                            room.get_x(),
                            room.get_y(),
                            ROOM_SELECTION_MARGIN,
                            accent,
                        );
                    }
                }
                EntityId::SourceRoom(source, number) => {
                    if let Some(room) = sources::source_room(area, source, number) {
                        self.stroke_room_outline(
                            frame,
                            room.get_x(),
                            room.get_y(),
                            sources::ring_clearance(self.scaling),
                            accent,
                        );
                    }
                }
                EntityId::Label(id) => {
                    if let Some((layer, label)) = area.find_label(&id) {
                        let rect = Rectangle::new(
                            Point::new(label.x, label.y),
                            Size::new(label.width, label.height),
                        );
                        let source = SourceLayer::source_of(layer);
                        self.outline_drawing(frame, area, source, rect, 0.0, false, accent);
                    }
                }
                EntityId::Shape(id) => {
                    if let Some((layer, shape)) = area.find_shape(&id) {
                        let rect = Rectangle::new(
                            Point::new(shape.x, shape.y),
                            Size::new(shape.width, shape.height),
                        );
                        self.outline_drawing(
                            frame,
                            area,
                            SourceLayer::source_of(layer),
                            rect,
                            shape.border_radius,
                            false,
                            accent,
                        );
                    }
                }
            }
        }
    }

    /// A label's or shape's selection: its outline, outside the ring a
    /// source's drawing wears. `ring` draws that ring too (a dragged
    /// drawing is not drawn by its layer while it moves).
    #[allow(clippy::too_many_arguments)]
    fn outline_drawing(
        &self,
        frame: &mut canvas::Frame,
        area: &smudgy_cloud::mapper::area_cache::AreaCache,
        source: smudgy_cloud::SourceId,
        rect: Rectangle,
        radius: f32,
        ring: bool,
        accent: Color,
    ) {
        if source.is_map() {
            stroke_rect_outline(frame, rect.x, rect.y, rect.width, rect.height, accent);
            return;
        }
        if ring && let Some(color) = sources::layer_color(area, source) {
            sources::ring(
                frame,
                Point::new(rect.x, rect.y),
                rect.size(),
                radius,
                color,
                1.0,
            );
        }
        let margin = sources::ring_clearance(self.scaling);
        let path = canvas::Path::rounded_rectangle(
            Point::new(rect.x - margin, rect.y - margin),
            Size::new(rect.width + margin * 2.0, rect.height + margin * 2.0),
            (radius + margin).into(),
        );
        frame.stroke(&path, selection_stroke(accent));
    }

    fn stroke_room_outline(
        &self,
        frame: &mut canvas::Frame,
        x: f32,
        y: f32,
        margin: f32,
        accent: Color,
    ) {
        let size = render::MAP_ROOM_SIZE + margin * 2.0;
        let path = canvas::Path::rounded_rectangle(
            Point::new(
                x - render::MAP_ROOM_SIZE / 2.0 - margin,
                y - render::MAP_ROOM_SIZE / 2.0 - margin,
            ),
            Size::new(size, size),
            render::MAP_ROOM_BORDER_RADIUS.into(),
        );
        frame.stroke(&path, selection_stroke(accent));
    }

    fn stroke_connection_outline(
        &self,
        frame: &mut canvas::Frame,
        area: &smudgy_cloud::mapper::area_cache::AreaCache,
        id: ConnectionId,
        accent: Color,
    ) {
        let Some(connection) = self.drawn_connection(area, id) else {
            return;
        };
        Self::stroke_resolved_connection_outline(frame, &connection, accent, false);
    }

    /// Connection `id`'s half on the current level as the canvas draws it:
    /// the map's own, or a Secret's in its layer's color.
    pub(super) fn drawn_connection(
        &self,
        area: &smudgy_cloud::mapper::area_cache::AreaCache,
        id: ConnectionId,
    ) -> Option<RoomConnection> {
        let (document, layer) = area.connection_document(id)?;
        let mut connection = document
            .get_room_connections()
            .iter()
            .find(|connection| {
                connection.connection_id == id && connection.from_level == self.level
            })?
            .clone();
        if let Some(color) = layer.and_then(|layer| sources::layer_color(area, layer.source())) {
            connection.color = color;
        }
        Some(connection)
    }

    /// Accent-halos the *visible* form of one Connection half — the stroked
    /// primitives (plus any corner level markers) for planar halves, or the
    /// level-change treatment glyph for cross-level halves — then redraws
    /// the entity over its halo so color, dash (including the secret dash),
    /// and the ▲/▼ glyph shape stay legible while highlighted. Stroke widths
    /// are pixel-space, so the halo is visible at any zoom.
    ///
    /// `stroke_hidden_stubs` additionally halos the wall stubs behind a
    /// level treatment: they are not normally drawn, but a cross-level port
    /// drag needs moving feedback beyond the handle dot.
    fn stroke_resolved_connection_outline(
        frame: &mut canvas::Frame,
        connection: &RoomConnection,
        accent: Color,
        stroke_hidden_stubs: bool,
    ) {
        let halo = connection.thickness + 4.0;
        if let Some(treatment) = render::level_treatment(connection, false) {
            if stroke_hidden_stubs {
                frame.stroke(
                    &render::path_from_primitives(&connection.geometry.primitives),
                    render::solid_stroke(accent, halo),
                );
            }
            match treatment {
                render::LevelTreatment::Triangle { center, up } => {
                    render::draw_level_triangle_outline(
                        frame, center.x, center.y, up, accent, halo,
                    );
                    render::draw_level_triangle(frame, center.x, center.y, up, connection.color);
                }
                render::LevelTreatment::FadingStub { edge, tip } => {
                    let line = canvas::Path::line(edge, tip);
                    frame.stroke(&line, render::solid_stroke(accent, halo));
                    frame.stroke(
                        &line,
                        render::solid_stroke(connection.color, connection.thickness),
                    );
                }
            }
            return;
        }
        let path = render::path_from_primitives(&connection.geometry.primitives);
        frame.stroke(&path, render::solid_stroke(accent, halo));
        frame.stroke(
            &path,
            render::connection_stroke(connection.color, connection.thickness, connection.dash),
        );
        for &(center, up) in &connection.geometry.level_markers {
            render::draw_level_triangle_outline(frame, center.x, center.y, up, accent, halo);
            render::draw_level_triangle_outline(
                frame,
                center.x,
                center.y,
                up,
                connection.color,
                connection.thickness,
            );
        }
    }
}

/// How far outside a map room its selection outline sits, in map units.
const ROOM_SELECTION_MARGIN: f32 = render::MAP_ROOM_SIZE * 0.12;

fn stroke_rect_outline(
    frame: &mut canvas::Frame,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    accent: Color,
) {
    let path = canvas::Path::rectangle(Point::new(x, y), Size::new(width, height));
    frame.stroke(&path, selection_stroke(accent));
}

fn selection_stroke(accent: Color) -> canvas::Stroke<'static> {
    canvas::Stroke {
        style: stroke::Style::Solid(accent),
        width: 2.0,
        ..render::solid_stroke(accent, 2.0)
    }
}

fn chebyshev(a: Point, b: Point) -> f32 {
    (a.x - b.x).abs().max((a.y - b.y).abs())
}

fn resize_cursor(handle: HandleKind) -> mouse::Interaction {
    match handle {
        HandleKind::East | HandleKind::West => mouse::Interaction::ResizingHorizontally,
        HandleKind::North | HandleKind::South => mouse::Interaction::ResizingVertically,
        HandleKind::NorthEast | HandleKind::SouthWest => mouse::Interaction::ResizingDiagonallyUp,
        HandleKind::NorthWest | HandleKind::SouthEast => mouse::Interaction::ResizingDiagonallyDown,
    }
}

fn rect_from_corners(a: Point, b: Point) -> Rectangle {
    Rectangle {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        width: (a.x - b.x).abs(),
        height: (a.y - b.y).abs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_drags_snap_to_midpoint_and_corner_slots() {
        let center = Point::new(0.0, 0.0);
        let room = RoomAddress::map(smudgy_cloud::RoomNumber(1));
        // Near the east wall, slightly below the middle: the wall midpoint.
        let snapped = endpoint_at_pointer(room, center, Point::new(0.25, 0.03), true);
        assert_eq!(snapped.side, RoomSide::East);
        assert!((snapped.port_offset - 0.5).abs() < f32::EPSILON);
        // Toward the north end of the same wall: the corner-inset slot.
        let corner = endpoint_at_pointer(room, center, Point::new(0.25, -0.1), true);
        assert_eq!(corner.side, RoomSide::East);
        assert!((corner.port_offset - CORNER_INSET).abs() < f32::EPSILON);
        // Snapping disabled (Alt held): the exact pointer offset survives.
        let free = endpoint_at_pointer(room, center, Point::new(0.25, 0.03), false);
        assert!((free.port_offset - 0.56).abs() < 1e-4);
        assert_eq!(free.port_mode, PortMode::Manual);
    }
}
