//! The canvas program for [`MapEditor`]: interaction state machine, hit
//! testing dispatch, and rendering (ghost levels, current level, selection
//! outlines, gesture previews).
//!
//! `canvas::Program::update` takes `&self`, so everything mutable during a
//! gesture lives in [`EditorProgramState`]; committed state changes are
//! published as [`Message`]s and applied in [`MapEditor::update`].

use iced::keyboard::{self, key::Named};
use iced::widget::canvas::{self, stroke};
use iced::{Color, Point, Rectangle, Size, Vector, mouse};

use iced::event::Event as IcedEvent;

use smudgy_cloud::{
    ConnectionEndpoint, ConnectionId, ConnectionRouting, ConnectionUpdates, MapPoint, PortMode,
    RoomNumber, RoomSide, SegmentShape,
    connection_geometry::{
        Handle as ConnectionHandle, distance_to_segment, port_position, stub_tip,
    },
};

use crate::{render, viewport};

use super::{
    EntityId, ExitTarget, MapEditor, Message, RectKind, Renderer, SelectedConnectionHandle, Theme,
    Tool, direction_between,
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
        from: RoomNumber,
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
        let stored = area.get_connection(connection_id)?;
        let radius = HANDLE_SCREEN_SIZE / self.scaling;
        let point = MapPoint::new(point.x, point.y);
        let render = area.get_room_connections().iter().find(|connection| {
            connection.connection_id == connection_id && connection.from_level == self.level
        })?;
        render.geometry.handles.iter().copied().find_map(|handle| {
            let on_level = match handle {
                ConnectionHandle::PortA(_) => area
                    .get_room(&stored.endpoint_a.room_number)
                    .is_some_and(|room| room.get_level() == self.level),
                ConnectionHandle::PortB(_) => stored.endpoint_b.is_some_and(|endpoint| {
                    area.get_room(&endpoint.room_number)
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
        let connection = area.get_connection(connection_id)?;
        match handle {
            ConnectionHandle::Waypoint(index, _) => {
                let mut points = connection.route_points.clone();
                if index >= points.len() {
                    return None;
                }
                let target = Self::maybe_snap(current, modifiers);
                let mut target = MapPoint::new(target.x, target.y);
                if connection.segment_shape == SegmentShape::Orthogonal {
                    let render = area.get_room_connections().iter().find(|render| {
                        render.connection_id == connection_id && render.from_level == self.level
                    })?;
                    let previous = if index == 0 {
                        render.geometry.stub_tip_a
                    } else {
                        points[index - 1]
                    };
                    let next = if index + 1 == points.len() {
                        render.geometry.stub_tip_b?
                    } else {
                        points[index + 1]
                    };
                    let old = points[index];
                    let previous_horizontal =
                        (old.y - previous.y).abs() <= (old.x - previous.x).abs();
                    let next_horizontal = (old.y - next.y).abs() <= (old.x - next.x).abs();
                    if index == 0 {
                        if previous_horizontal {
                            target.y = previous.y;
                        } else {
                            target.x = previous.x;
                        }
                    } else if previous_horizontal {
                        points[index - 1].y = target.y;
                    } else {
                        points[index - 1].x = target.x;
                    }
                    if index + 1 == points.len() {
                        if next_horizontal {
                            target.y = next.y;
                        } else {
                            target.x = next.x;
                        }
                    } else if next_horizontal {
                        points[index + 1].y = target.y;
                    } else {
                        points[index + 1].x = target.x;
                    }
                }
                points[index] = target;
                Some(ConnectionUpdates {
                    routing: Some(ConnectionRouting::Manual),
                    route_points: Some(points),
                    ..ConnectionUpdates::default()
                })
            }
            ConnectionHandle::PortA(_) => {
                let room = area.get_room(&connection.endpoint_a.room_number)?;
                let endpoint = endpoint_at_pointer(
                    connection.endpoint_a.room_number,
                    Point::new(room.get_x(), room.get_y()),
                    current,
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
                    let mut points = connection.route_points.clone();
                    if let Some(first) = points.first_mut() {
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
                        points.push(MapPoint::new(new_tip.x, other_tip.y));
                    }
                    route_points = Some(points);
                }
                Some(ConnectionUpdates {
                    endpoint_a: Some(endpoint),
                    route_points,
                    ..ConnectionUpdates::default()
                })
            }
            ConnectionHandle::PortB(_) => {
                let endpoint_b = connection.endpoint_b?;
                let room = area.get_room(&endpoint_b.room_number)?;
                let endpoint = endpoint_at_pointer(
                    endpoint_b.room_number,
                    Point::new(room.get_x(), room.get_y()),
                    current,
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
                    let mut points = connection.route_points.clone();
                    if let Some(last) = points.last_mut() {
                        if (last.y - old_tip.y).abs() <= (last.x - old_tip.x).abs() {
                            last.y = new_tip.y;
                        } else {
                            last.x = new_tip.x;
                        }
                    } else if (render.geometry.stub_tip_a.x - new_tip.x).abs() > f32::EPSILON
                        && (render.geometry.stub_tip_a.y - new_tip.y).abs() > f32::EPSILON
                    {
                        points.push(MapPoint::new(render.geometry.stub_tip_a.x, new_tip.y));
                    }
                    route_points = Some(points);
                }
                Some(ConnectionUpdates {
                    endpoint_b: Some(endpoint),
                    route_points,
                    ..ConnectionUpdates::default()
                })
            }
        }
    }

    fn waypoint_insertion(
        &self,
        connection_id: ConnectionId,
        point: Point,
        modifiers: keyboard::Modifiers,
    ) -> Option<(usize, Vec<MapPoint>, usize)> {
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(self.area_id.as_ref()?)?;
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
        let point = Self::maybe_snap(point, modifiers);
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
                let target = match self.room_at_with_center(current_map) {
                    Some((number, _)) if number == from => None,
                    Some((number, center)) => Some((ExitTarget::Room(number), center)),
                    None => {
                        let at = if state.modifiers.alt() {
                            current_map
                        } else {
                            viewport::snap(current_map)
                        };
                        Some((
                            if state.modifiers.shift() {
                                ExitTarget::Dangling(at)
                            } else {
                                ExitTarget::Empty(at)
                            },
                            at,
                        ))
                    }
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
                        || canvas::Action::request_redraw().and_capture(),
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
        }

        // Escape cancels an in-flight gesture; when idle it is left for the
        // host window (tool revert / selection clear).
        if let IcedEvent::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(Named::Escape),
            ..
        }) = event
        {
            if !matches!(state.interaction, Interaction::Idle) {
                state.interaction = Interaction::Idle;
                return Some(canvas::Action::request_redraw().and_capture());
            }
            return None;
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
                    if matches!(state.interaction, Interaction::Panning { .. }) {
                        state.interaction = Interaction::Idle;
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
                        state.interaction = Interaction::Panning {
                            translation: self.translation,
                            start: cursor_position,
                        };

                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    mouse::Button::Left => match self.tool {
                        Tool::Select => {
                            // Connection handles take priority over all other
                            // hit targets, followed by resize handles.
                            if let Some((connection_id, handle)) =
                                self.connection_handle_at(map_position)
                            {
                                state.interaction = Interaction::DraggingConnectionHandle {
                                    connection_id,
                                    handle,
                                    current_map: map_position,
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
                            if let Some((from, from_center)) =
                                self.room_at_with_center(map_position)
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
                    Interaction::Panning { translation, start } => {
                        let translation =
                            *translation + (cursor_position - *start) * (1.0 / self.scaling);
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
                        let room_key = self.room_key_at(map_position);
                        if room_key == self.hovered_room {
                            Some(canvas::Action::request_redraw())
                        } else {
                            Some(canvas::Action::publish(Message::SetHoveredRoom(room_key)))
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
                            render::draw_shape(frame, shape, opacity, true);
                        }
                    }
                    for label in area.get_labels() {
                        if label.level == ghost_level {
                            render::draw_label(frame, label, opacity, true);
                        }
                    }
                    area.with_room_connections_in(min_x, min_y, max_x, max_y, |connection| {
                        if connection.from_level == ghost_level {
                            render::draw_connection(frame, &atlas, connection, opacity, true, true);
                        }
                    });
                    area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                        if room.get_level() == ghost_level {
                            render::draw_room(frame, room, opacity, true);
                        }
                    });
                }

                // Current level.
                for shape in area.get_shapes() {
                    if shape.level == self.level
                        && !(drag_offset.is_some()
                            && self.selection.contains(EntityId::Shape(shape.id)))
                    {
                        render::draw_shape(frame, shape, 1.0, true);
                    }
                }
                for label in area.get_labels() {
                    if label.level == self.level
                        && !(drag_offset.is_some()
                            && self.selection.contains(EntityId::Label(label.id)))
                    {
                        render::draw_label(frame, label, 1.0, true);
                    }
                }
                area.with_room_connections_in(min_x, min_y, max_x, max_y, |connection| {
                    if connection.from_level == self.level {
                        render::draw_connection(frame, &atlas, connection, 1.0, true, false);
                    }
                });
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
                    preview.is_secret = false;
                    preview.thickness = preview.thickness.max(2.0);
                    render::draw_connection(frame, &atlas, &preview, 0.9, false, false);
                }
                area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                    if room.get_level() == self.level
                        && !(drag_offset.is_some()
                            && self
                                .selection
                                .contains(EntityId::Room(room.get_room_number())))
                    {
                        render::draw_room(frame, room, 1.0, true);
                    }
                });

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
                if let Some(offset) = drag_offset {
                    frame.with_save(|frame| {
                        frame.translate(offset);
                        self.draw_selected_entities(frame, area.as_ref(), accent);
                    });
                } else {
                    self.draw_selection_outlines(frame, area.as_ref(), accent);
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
                    let target = self.room_at_with_center(*current_map);
                    let end = match &target {
                        Some((number, center)) if number != from => *center,
                        Some(_) => *current_map,
                        None => {
                            if state.modifiers.alt() {
                                *current_map
                            } else {
                                viewport::snap(*current_map)
                            }
                        }
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
                        Some((number, center)) if number != *from => {
                            let half = render::MAP_ROOM_SIZE / 2.0 + 0.06;
                            let path = canvas::Path::rounded_rectangle(
                                Point::new(center.x - half, center.y - half),
                                Size::new(half * 2.0, half * 2.0),
                                render::MAP_ROOM_BORDER_RADIUS.into(),
                            );
                            frame.stroke(&path, render::solid_stroke(accent, 2.0));
                        }
                        Some(_) => {}
                        None => {
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
                // stable screen-space target. During a drag the active handle
                // follows the pointer while the stored path remains an
                // uncommitted reference until release.
                if self.editable
                    && let Some(EntityId::Connection(connection_id)) = self.selection.single()
                    && let Some(render) = area.get_room_connections().iter().find(|connection| {
                        connection.connection_id == connection_id
                            && connection.from_level == self.level
                    })
                {
                    let dragging = match state.interaction {
                        Interaction::DraggingConnectionHandle {
                            connection_id: active,
                            handle,
                            current_map,
                        } if active == connection_id => Some((handle, current_map)),
                        _ => None,
                    };
                    let radius = HANDLE_SCREEN_SIZE / self.scaling / 2.0;
                    for handle in &render.geometry.handles {
                        let mut position = handle.position();
                        if dragging.is_some_and(|(active, _)| {
                            std::mem::discriminant(&active) == std::mem::discriminant(handle)
                                && match (active, *handle) {
                                    (
                                        ConnectionHandle::Waypoint(a, _),
                                        ConnectionHandle::Waypoint(b, _),
                                    ) => a == b,
                                    _ => true,
                                }
                        }) {
                            let (_, current) = dragging.expect("checked");
                            position = MapPoint::new(current.x, current.y);
                        }
                        let path = canvas::Path::circle(Point::new(position.x, position.y), radius);
                        frame.fill(&path, accent);
                        frame.stroke(
                            &path,
                            render::solid_stroke(Color::WHITE, 1.0 / self.scaling),
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
            Interaction::DraggingConnectionHandle { .. } => mouse::Interaction::Grabbing,
            _ => {
                if let Some(cursor_position) = cursor.position_in(bounds) {
                    let map_position = self.viewport().project(cursor_position, bounds.size());
                    if self.connection_handle_at(map_position).is_some() {
                        return mouse::Interaction::Grab;
                    }
                    if let Some((_, handle, _)) = self.handle_at(map_position) {
                        return resize_cursor(handle);
                    }
                    if self.tool == Tool::Link
                        && let Some((_, center)) = self.room_at_with_center(map_position)
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
                }
                mouse::Interaction::default()
            }
        }
    }
}

fn endpoint_at_pointer(
    room_number: RoomNumber,
    center: Point,
    pointer: Point,
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
    let port_offset = match side {
        RoomSide::North | RoomSide::South => (pointer.x - left) / render::MAP_ROOM_SIZE,
        RoomSide::East | RoomSide::West => (pointer.y - top) / render::MAP_ROOM_SIZE,
    }
    .clamp(0.0, 1.0);
    ConnectionEndpoint {
        room_number,
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
                        render::draw_room(frame, room, 1.0, true);
                        self.stroke_room_outline(frame, room.get_x(), room.get_y(), accent);
                    }
                }
                EntityId::Label(id) => {
                    if let Some(label) = area.get_label(&id) {
                        render::draw_label(frame, label, 1.0, true);
                        stroke_rect_outline(
                            frame,
                            label.x,
                            label.y,
                            label.width,
                            label.height,
                            accent,
                        );
                    }
                }
                EntityId::Shape(id) => {
                    if let Some(shape) = area.get_shape(&id) {
                        render::draw_shape(frame, shape, 1.0, true);
                        stroke_rect_outline(
                            frame,
                            shape.x,
                            shape.y,
                            shape.width,
                            shape.height,
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
        accent: Color,
    ) {
        for entity in self.selection.iter() {
            match entity {
                EntityId::Connection(id) => {
                    self.stroke_connection_outline(frame, area, id, accent);
                }
                EntityId::Room(number) => {
                    if let Some(room) = area.get_room(&number) {
                        self.stroke_room_outline(frame, room.get_x(), room.get_y(), accent);
                    }
                }
                EntityId::Label(id) => {
                    if let Some(label) = area.get_label(&id) {
                        stroke_rect_outline(
                            frame,
                            label.x,
                            label.y,
                            label.width,
                            label.height,
                            accent,
                        );
                    }
                }
                EntityId::Shape(id) => {
                    if let Some(shape) = area.get_shape(&id) {
                        stroke_rect_outline(
                            frame,
                            shape.x,
                            shape.y,
                            shape.width,
                            shape.height,
                            accent,
                        );
                    }
                }
            }
        }
    }

    fn stroke_room_outline(&self, frame: &mut canvas::Frame, x: f32, y: f32, accent: Color) {
        let margin = render::MAP_ROOM_SIZE * 0.12;
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
        let Some(connection) = area.get_room_connections().iter().find(|connection| {
            connection.connection_id == id && connection.from_level == self.level
        }) else {
            return;
        };
        let width = connection.thickness + 4.0 / self.scaling;
        frame.stroke(
            &render::path_from_primitives(&connection.geometry.primitives),
            render::solid_stroke(accent, width),
        );
    }
}

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
