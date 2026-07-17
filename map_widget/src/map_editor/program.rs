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

use smudgy_cloud::RoomNumber;

use crate::{render, viewport};

use super::{
    EntityId, ExitTarget, MapEditor, Message, RectKind, Renderer, Theme, Tool, direction_between,
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
}

#[derive(Default)]
pub struct EditorProgramState {
    interaction: Interaction,
    modifiers: keyboard::Modifiers,
}

impl MapEditor {
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
        let (entity, rect) = self.selected_rect()?;
        let radius = HANDLE_SCREEN_SIZE / self.scaling / 2.0;

        handle_positions(rect)
            .into_iter()
            .find_map(|(kind, position)| {
                (chebyshev(point, position) <= radius).then_some((entity, kind, rect))
            })
    }

    fn zoom(
        &self,
        step: f32,
        cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> canvas::Action<Message> {
        if step < 0.0 && self.scaling > Self::MIN_SCALING
            || step > 0.0 && self.scaling < Self::MAX_SCALING
        {
            let old_scaling = self.scaling;

            let scaling =
                (self.scaling * (1.0 + step / 10.0)).clamp(Self::MIN_SCALING, Self::MAX_SCALING);

            let translation = cursor.position_from(bounds.center()).map(|cursor_to_center| {
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
    fn finish_gesture(
        &self,
        state: &mut EditorProgramState,
    ) -> Option<canvas::Action<Message>> {
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
                    Some(
                        canvas::Action::publish(Message::MoveCommitted { offset }).and_capture(),
                    )
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
                        Some((ExitTarget::Empty(at), at))
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
                            one_way: state.modifiers.control(),
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
                            // Resize handles take priority over everything.
                            if let Some((entity, handle, rect)) = self.handle_at(map_position) {
                                state.interaction = Interaction::DraggingHandle {
                                    entity,
                                    handle,
                                    original: rect,
                                    current_map: map_position,
                                };
                                return Some(canvas::Action::request_redraw().and_capture());
                            }

                            // Presses on a room's border band start an exit
                            // drag rather than a move/select.
                            if let Some((from, from_center)) =
                                self.room_at_with_center(map_position)
                                && chebyshev(map_position, from_center) > EXIT_BAND_INNER
                            {
                                state.interaction = Interaction::DraggingExit {
                                    from,
                                    from_center,
                                    current_map: map_position,
                                };
                                return Some(canvas::Action::request_redraw().and_capture());
                            }

                            if let Some(entity) = self.entity_at(map_position) {
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
                        let translation = *translation
                            + (cursor_position - *start) * (1.0 / self.scaling);
                        Some(canvas::Action::publish(Message::Translated(translation))
                            .and_capture())
                    }
                    Interaction::PendingSelect {
                        start_screen,
                        start_map,
                        ..
                    } => {
                        if (cursor_position - *start_screen).x.abs() > DRAG_THRESHOLD
                            || (cursor_position - *start_screen).y.abs() > DRAG_THRESHOLD
                        {
                            state.interaction = Interaction::DraggingSelection {
                                start_map: *start_map,
                                current_map: map_position,
                            };
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
                    | Interaction::DraggingHandle { current_map, .. } => {
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
                } else if drag_offset.is_none()
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

                // Placement ghost for the room tool.
                if self.tool == Tool::AddRoom
                    && let Some(cursor_position) = cursor.position_in(bounds)
                {
                    let map_position =
                        self.viewport().project(cursor_position, bounds.size());
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
            _ => {
                if let Some(cursor_position) = cursor.position_in(bounds) {
                    let map_position = self.viewport().project(cursor_position, bounds.size());
                    if let Some((_, handle, _)) = self.handle_at(map_position) {
                        return resize_cursor(handle);
                    }
                    if let Some((_, center)) = self.room_at_with_center(map_position) {
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
        HandleKind::NorthEast | HandleKind::SouthWest => {
            mouse::Interaction::ResizingDiagonallyUp
        }
        HandleKind::NorthWest | HandleKind::SouthEast => {
            mouse::Interaction::ResizingDiagonallyDown
        }
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
