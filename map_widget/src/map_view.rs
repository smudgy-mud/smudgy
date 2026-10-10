use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iced::{
    Length, Point, Rectangle, Size, Vector, keyboard, mouse,
    widget::{Canvas, canvas, container},
};
use smudgy_cloud::{
    AreaId, ExitDirection, Mapper, RoomAddress, RoomNumber,
    mapper::{
        RoomKey,
        room_connection::{RoomConnection, RoomConnectionEnd},
    },
};

use iced_anim::{Animated, spring::Motion, transition::Easing};

use crate::{
    CrossAreaLabelVisibility, MapExitRef, MapViewPresentation, ResolvedPresentation, Update,
    presentation::DEFAULT_DOOR_COLOR, render, sources, viewport::Viewport,
};
use iced::event::Event as IcedEvent;
use std::time::{Duration, Instant};
pub type Renderer = iced::Renderer;
pub type Theme = smudgy_theme::Theme;
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, Renderer>;

pub struct MapView {
    mapper: Mapper,
    active_area_id: AreaId,
    player_location: Option<RoomKey>,
    level: i32,
    scaling: f32,
    translation: iced_anim::Animated<Vector>,
    last_viewport_size: Cell<Option<Size>>,
    area_opacity: Animated<f32>,
    fade_phase: FadePhase,
    pending_area_change: Option<PendingAreaChange>,
    presentation: MapViewPresentation,
    /// The draw-ready form of `presentation` for the active area: colors
    /// parsed, apply entries folded into lookup tables. Rebuilt whenever the
    /// presentation or the active area changes, never per frame.
    resolved: ResolvedPresentation,

    hovered_room: Option<RoomKey>,
}

#[derive(Debug, Clone)]
struct PendingAreaChange {
    area_id: AreaId,
    player_location: Option<RoomKey>,
    level: i32,
    translation: Vector,
}

#[derive(Debug, Clone)]
pub enum Message {
    SetPlayerLocation(AreaId, Option<i32>),
    Translated(Vector),
    Scaled(f32, Option<Vector>),
    SetHoveredRoom(Option<RoomKey>),
    /// A room clicked: pressed and released with the left button over it.
    RoomClicked(RoomKey),
    /// Advance both in-flight animations (pan spring + area fade) to `now`.
    /// Published by the canvas program on any event while animating; the
    /// publish schedules a redraw, whose `RedrawRequested` produces the next
    /// tick, so the loop self-sustains until both values settle.
    AnimationTick(Instant),
}

#[derive(Debug, Clone)]
pub enum Event {
    HoveredRoomChanged(Option<RoomKey>),
    /// A room clicked, keyed as the view picks it (a place's own room by
    /// its own area). The host decides who hears of it.
    RoomClicked(RoomKey),
}

const FADE_EPSILON: f32 = 0.02;
const FADE_DURATION_TOTAL_MS: u64 = 200;
const FADE_HALF_DURATION_MS: u64 = FADE_DURATION_TOTAL_MS / 2;

/// How many levels above and below the current one the widget ghosts.
const GHOST_LEVEL_SPREAD: i32 = 2;
/// Per-level diagonal nudge for ghosted levels (1/5 of a room), so the
/// stack of levels reads as depth instead of overlapping the current floor.
const GHOST_LEVEL_OFFSET: f32 = render::MAP_ROOM_SIZE / 5.0;
/// Opacity of a ghost one level away; farther levels divide this by their
/// distance, and the whole thing is scaled by the area-fade opacity.
const GHOST_BASE_OPACITY: f32 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FadePhase {
    Idle,
    FadingOut,
    FadingIn,
}

impl MapView {
    const MIN_SCALING: f32 = 2.0;
    const MAX_SCALING: f32 = 200.0;
    /// Must stay comfortably above the worst-case overhang of a level
    /// treatment glyph past its Connection's stroke bounds (~0.8 map
    /// units); see the editor's constant of the same name.
    const SPATIAL_QUERY_PADDING: f32 = 1.0;

    pub fn new(mapper: Mapper, area_id: AreaId) -> Self {
        Self {
            mapper,
            active_area_id: area_id,
            player_location: None,
            level: 0,
            scaling: 40.0,
            hovered_room: None,
            // Momentum-based spring transition for translation. The velocity
            // guard (`translation_velocity_exceeds_threshold` -> `settle()`)
            // catches the divergent oscillation the spring can hit when frames
            // run slower than the animation tick clamp (33ms) under the
            // software renderer.
            translation: Animated::new(Vector::new(0.0, 0.0), Motion::default().quick()),
            last_viewport_size: Cell::new(None),
            area_opacity: Animated::new(
                1.0_f32,
                Easing::EASE_IN_OUT
                    .with_duration(Duration::from_millis(FADE_HALF_DURATION_MS))
                    .reversible(true),
            ),
            fade_phase: FadePhase::Idle,
            pending_area_change: None,
            presentation: MapViewPresentation::default(),
            resolved: ResolvedPresentation::default(),
        }
    }

    /// Replace the view-local appearance and per-item style associations.
    /// Changing room spacing preserves the map's current visual center;
    /// nothing here touches zoom or pan otherwise.
    pub fn set_presentation(&mut self, presentation: MapViewPresentation) {
        let presentation = presentation.normalized();
        if self.presentation == presentation {
            return;
        }
        let old_spacing = self.presentation.room_spacing;
        let new_spacing = presentation.room_spacing;
        if (old_spacing - new_spacing).abs() > f32::EPSILON {
            let ratio = new_spacing / old_spacing;
            let value = *self.translation.value() * ratio;
            let target = *self.translation.target() * ratio;
            self.translation.settle_at(value);
            self.translation.set_target(target);
            if let Some(pending) = &mut self.pending_area_change {
                pending.translation = pending.translation * ratio;
            }
        }
        self.resolved = presentation.resolve(self.active_area_id);
        self.presentation = presentation;
    }

    fn rooms_at_point(&self, point: &Point, bounds: &Size) -> Box<[RoomKey]> {
        self.rooms_at(self.viewport().project(*point, *bounds))
    }

    /// The rooms on the current level under `point` (in spaced map units),
    /// topmost first: a Secret's or Private's own rooms, which draw over the
    /// map's, the last layer's first, then the map's own. A place's room is
    /// keyed by its own area, as the editor selects it. Hover stays in the
    /// view (it reveals a room's labels); a click is reported as
    /// [`Event::RoomClicked`].
    fn rooms_at(&self, point: Point) -> Box<[RoomKey]> {
        let atlas = self.mapper.get_current_atlas();
        let spacing = self.resolved.room_spacing;
        let lookup = Point::new(point.x / spacing, point.y / spacing);
        let half_size = render::MAP_ROOM_SIZE / 2.0;
        let lookup_half = half_size / spacing;
        let min_x = lookup.x - lookup_half;
        let min_y = lookup.y - lookup_half;
        let max_x = lookup.x + lookup_half;
        let max_y = lookup.y + lookup_half;

        let under = |room: &smudgy_cloud::mapper::room_cache::RoomCache| {
            room.get_level() == self.level
                && room.get_x() * spacing - half_size < point.x
                && room.get_x() * spacing + half_size > point.x
                && room.get_y() * spacing - half_size < point.y
                && room.get_y() * spacing + half_size > point.y
        };

        atlas
            .get_area(&self.active_area_id)
            .map(|area| {
                let mut hits: Vec<RoomKey> = Vec::new();
                for layer in area.source_layers().iter().rev() {
                    layer
                        .content()
                        .with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                            let number = room.get_room_number();
                            if under(room) {
                                hits.push(RoomKey {
                                    area_id: layer.area_id(),
                                    room_number: number,
                                });
                            }
                        });
                }
                area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                    if under(room) {
                        hits.push(RoomKey {
                            area_id: self.active_area_id,
                            room_number: room.get_room_number(),
                        });
                    }
                });
                hits.into_boxed_slice()
            })
            .unwrap_or_default()
    }

    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        match message {
            Message::AnimationTick(now) => {
                let previous = *self.translation.value();
                self.translation.tick(now);
                if self.translation_velocity_exceeds_threshold(previous) {
                    if std::env::var_os("SMUDGY_MAP_DEBUG").is_some() {
                        eprintln!(
                            "map update: velocity guard tripped, settling at {:?} (was {previous:?})",
                            self.translation.target(),
                        );
                    }
                    self.translation.settle();
                }
                self.area_opacity.tick(now);
                self.handle_fade_progress();
                Update::none()
            }
            Message::SetPlayerLocation(area_id, room_number) => {
                // A room of one of the map's Secrets is shown on its map.
                let shown = self
                    .mapper
                    .get_current_atlas()
                    .map_of(&area_id)
                    .unwrap_or(area_id);
                let area_changed = shown != self.active_area_id;

                if area_changed {
                    let mut pending = PendingAreaChange {
                        area_id: shown,
                        player_location: None,
                        level: 0,
                        translation: *self.translation.value(),
                    };

                    if let Some(room_number) = room_number {
                        let room_key = RoomKey {
                            area_id,
                            room_number: RoomNumber(room_number),
                        };

                        if let Some(room) = self.mapper.get_current_atlas().get_room(&room_key) {
                            pending.player_location = Some(room_key);
                            pending.translation = Vector::new(
                                -room.get_x() * self.resolved.room_spacing,
                                -room.get_y() * self.resolved.room_spacing,
                            );
                            pending.level = room.get_level();
                        }
                    } else {
                        pending.player_location = None;
                    }

                    self.pending_area_change = Some(pending);
                    self.start_area_fade();
                    return Update::none();
                }

                // The latest location still names the active area, so any
                // pending transition away from it is stale. This matters when
                // updates arrive B -> A -> B within one fade: active_area_id
                // remains B until the fade-out completes, and without this
                // cancellation the pending A would win after the newer B.
                self.cancel_pending_area_change();
                self.level = 0;

                if let Some(room_number) = room_number {
                    let room_key = RoomKey {
                        area_id,
                        room_number: RoomNumber(room_number),
                    };

                    if let Some(room) = self.mapper.get_current_atlas().get_room(&room_key) {
                        self.player_location = Some(room_key);
                        let target = Vector::new(
                            -room.get_x() * self.resolved.room_spacing,
                            -room.get_y() * self.resolved.room_spacing,
                        );
                        let visible = self.is_point_visible(Point {
                            x: room.get_x() * self.resolved.room_spacing,
                            y: room.get_y() * self.resolved.room_spacing,
                        });
                        if std::env::var_os("SMUDGY_MAP_DEBUG").is_some() {
                            eprintln!(
                                "map update: player -> room {} target={target:?} visible={visible} (animate={visible})",
                                room_number,
                            );
                        }
                        if visible {
                            self.translation.set_target(target);
                        } else {
                            self.translation.settle_at(target);
                        }
                        self.level = room.get_level();
                    }
                } else {
                    self.player_location = None;
                }

                Update::none()
            }
            Message::Translated(translation) => {
                self.translation.settle_at(translation);
                Update::none()
            }
            Message::Scaled(scaling, translation) => {
                self.scaling = scaling;

                if let Some(translation) = translation {
                    self.translation.settle_at(translation);
                }

                Update::none()
            }
            Message::SetHoveredRoom(room_key) => {
                self.hovered_room = room_key.clone();
                Update::with_event(Event::HoveredRoomChanged(room_key))
            }
            Message::RoomClicked(room_key) => Update::with_event(Event::RoomClicked(room_key)),
        }
    }

    /// Whether either animated value (pan spring, area fade) is in flight —
    /// the gate for publishing [`Message::AnimationTick`].
    fn is_animating(&self) -> bool {
        self.translation.is_animating() || self.area_opacity.is_animating()
    }

    #[inline]
    fn viewport(&self) -> Viewport {
        Viewport {
            translation: *self.translation.value(),
            scaling: self.scaling,
        }
    }

    fn translation_velocity_exceeds_threshold(&self, previous: Vector) -> bool {
        let current = *self.translation.value();
        let delta = Vector {
            x: current.x - previous.x,
            y: current.y - previous.y,
        };
        let step = (delta.x * delta.x + delta.y * delta.y).sqrt();
        if !step.is_finite() {
            return true;
        }
        self.viewport_span_in_map_units()
            .map(|span| span > 0.0 && step > span * 10.0)
            .unwrap_or(false)
    }

    fn viewport_span_in_map_units(&self) -> Option<f32> {
        let size = self.last_viewport_size.get()?;
        if !(self.scaling.is_finite() && self.scaling > 0.0) {
            return None;
        }
        let width = size.width / self.scaling;
        let height = size.height / self.scaling;
        Some((width * width + height * height).sqrt())
    }

    fn is_point_visible(&self, point: Point) -> bool {
        let size = match self.last_viewport_size.get() {
            Some(size) => size,
            None => return false,
        };
        self.viewport().visible_region(size).contains(point)
    }

    fn start_area_fade(&mut self) {
        if self.pending_area_change.is_some() {
            self.fade_phase = FadePhase::FadingOut;
            self.area_opacity.set_target(0.0);
        }
    }

    fn cancel_pending_area_change(&mut self) {
        if self.pending_area_change.take().is_none() {
            return;
        }

        if (1.0 - *self.area_opacity.value()).abs() <= FADE_EPSILON {
            self.area_opacity.settle_at(1.0);
            self.fade_phase = FadePhase::Idle;
        } else {
            self.area_opacity.set_target(1.0);
            self.fade_phase = FadePhase::FadingIn;
        }
    }

    fn handle_fade_progress(&mut self) {
        match self.fade_phase {
            FadePhase::FadingOut => {
                if *self.area_opacity.value() <= FADE_EPSILON {
                    self.apply_pending_area_change();
                    self.fade_phase = FadePhase::FadingIn;
                    self.area_opacity.set_target(1.0);
                }
            }
            FadePhase::FadingIn => {
                if (1.0 - *self.area_opacity.value()).abs() <= FADE_EPSILON {
                    self.fade_phase = FadePhase::Idle;
                }
            }
            FadePhase::Idle => {}
        }
    }

    fn apply_pending_area_change(&mut self) {
        if let Some(pending) = self.pending_area_change.take() {
            self.active_area_id = pending.area_id;
            self.player_location = pending.player_location;
            self.level = pending.level;
            self.translation.settle_at(pending.translation);
            self.translation.set_target(pending.translation);
            // Apply entries can be area-scoped, so the lookup tables are
            // per-area facts and must follow the area.
            self.resolved = self.presentation.resolve(self.active_area_id);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub enum Interaction {
    #[default]
    None,
    Panning {
        translation: Vector,
        start: Point,
    },
}

/// Canvas-local state: the in-flight interaction, the room a left press
/// landed on (a release over the same room clicks it), plus the last known
/// keyboard modifiers (tracked so scroll gestures can branch on them).
#[derive(Default)]
pub struct ProgramState {
    interaction: Interaction,
    pressed_room: Option<RoomKey>,
    modifiers: keyboard::Modifiers,
}

impl MapView {
    /// Zoom by a wheel step (±1 ≈ one notch), anchored at the cursor when
    /// possible. Captures the event even at the zoom limits so scrolling
    /// over the map never leaks to widgets beneath it.
    fn zoom(&self, step: f32, cursor: mouse::Cursor, bounds: Rectangle) -> canvas::Action<Message> {
        if step < 0.0 && self.scaling > Self::MIN_SCALING
            || step > 0.0 && self.scaling < Self::MAX_SCALING
        {
            let old_scaling = self.scaling;

            let scaling =
                (self.scaling * (1.0 + step / 10.0)).clamp(Self::MIN_SCALING, Self::MAX_SCALING);

            let translation = if let Some(cursor_to_center) = cursor.position_from(bounds.center())
            {
                let factor = scaling - old_scaling;

                Some(
                    *self.translation.target()
                        - Vector::new(
                            cursor_to_center.x * factor / (old_scaling * old_scaling),
                            cursor_to_center.y * factor / (old_scaling * old_scaling),
                        ),
                )
            } else {
                None
            };

            canvas::Action::publish(Message::Scaled(scaling, translation)).and_capture()
        } else {
            canvas::Action::capture()
        }
    }
}

impl MapView {
    /// The left button at `at` (canvas coordinates): a press over a room is
    /// the map's and may click it, and released over the room it was
    /// `pressed` on, it clicks it. A press anywhere else falls through like
    /// the other buttons'.
    fn left_button(
        &self,
        state: &mut ProgramState,
        event: &mouse::Event,
        at: Point,
        size: Size,
        pressed: Option<RoomKey>,
    ) -> Option<canvas::Action<Message>> {
        let room_at = || self.rooms_at_point(&at, &size).first().cloned();
        if matches!(event, mouse::Event::ButtonPressed(_)) {
            state.pressed_room = room_at();
            return state
                .pressed_room
                .is_some()
                .then(|| canvas::Action::request_redraw().and_capture());
        }
        let pressed = pressed?;
        (room_at().as_ref() == Some(&pressed))
            .then(|| canvas::Action::publish(Message::RoomClicked(pressed)).and_capture())
    }

    fn handle_event(
        &self,
        state: &mut ProgramState,
        event: &IcedEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if let IcedEvent::Mouse(mouse::Event::ButtonReleased(_)) = event {
            state.interaction = Interaction::None;
        }
        // A left release ends a press wherever it lands, outside the canvas
        // too.
        let pressed_room = match event {
            IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.pressed_room.take()
            }
            _ => None,
        };

        // Track modifiers before the cursor gate so the state stays fresh
        // even while the cursor is outside the canvas.
        if let IcedEvent::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
        }

        // `cursor.position_in` is unavailable after the pointer leaves the
        // canvas, so clear the retained hover before that gate. Hover-driven
        // cross-area labels must not stick at the edge of the widget.
        if matches!(event, IcedEvent::Mouse(mouse::Event::CursorLeft))
            && self.hovered_room.is_some()
        {
            return Some(canvas::Action::publish(Message::SetHoveredRoom(None)));
        }

        let cursor_position = cursor.position_in(bounds)?;

        match event {
            IcedEvent::Mouse(mouse_event) => match mouse_event {
                mouse::Event::ButtonPressed(mouse::Button::Right) => {
                    state.interaction = Interaction::Panning {
                        translation: *self.translation.value(),
                        start: cursor_position,
                    };

                    Some(canvas::Action::request_redraw().and_capture())
                }
                mouse::Event::ButtonPressed(mouse::Button::Left)
                | mouse::Event::ButtonReleased(mouse::Button::Left) => self.left_button(
                    state,
                    mouse_event,
                    cursor_position,
                    bounds.size(),
                    pressed_room,
                ),
                // The map does nothing with other buttons; let the press
                // fall through to whatever is beneath the canvas (e.g.
                // the terminal scrollbar under an overlaid minimap).
                mouse::Event::CursorMoved { .. } => {
                    let message = match state.interaction {
                        Interaction::Panning { translation, start } => Some(Message::Translated(
                            translation + (cursor_position - start) * (1.0 / self.scaling),
                        )),
                        Interaction::None => {
                            let rooms = self.rooms_at_point(&cursor_position, &bounds.size());

                            let room_key = rooms.first().cloned();
                            if room_key != self.hovered_room {
                                Some(Message::SetHoveredRoom(room_key))
                            } else {
                                None
                            }
                        }
                    };

                    let action = message
                        .map(canvas::Action::publish)
                        .unwrap_or(canvas::Action::request_redraw());

                    Some(match state.interaction {
                        Interaction::None => action,
                        _ => action.and_capture(),
                    })
                }
                // Trackpads report pixel deltas; without a modifier held,
                // two-finger scroll pans the map (right-drag panning is not
                // expressible on a trackpad, where moving two fingers emits
                // scroll events rather than cursor movement). Command/Ctrl +
                // scroll and mouse-wheel line deltas zoom.
                mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x, y },
                } if !state.modifiers.command() && !state.modifiers.control() => {
                    let translation = *self.translation.target()
                        + Vector::new(x / self.scaling, y / self.scaling);

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

    fn draw_geometry(&self, renderer: &Renderer, bounds: Rectangle) -> Vec<canvas::Geometry> {
        // Geometry is rebuilt from scratch every frame (no canvas::Cache), so
        // build time is the frame-time cost of this widget. Only sample the
        // clock when debugging is on so the normal path pays nothing.
        let draw_start = std::env::var_os("SMUDGY_MAP_DEBUG")
            .is_some()
            .then(std::time::Instant::now);
        self.last_viewport_size.set(Some(bounds.size()));
        let atlas = self.mapper.get_current_atlas();
        let opacity = self.area_opacity.value().clamp(0.0, 1.0);
        let resolved = &self.resolved;
        let spacing = resolved.room_spacing;

        // The player's room, when the shown map holds it: one of its own,
        // or one of its Secrets', drawn over the map.
        let player_room = self.player_location.as_ref().and_then(|room_key| {
            let map = atlas.map_of(&room_key.area_id).unwrap_or(room_key.area_id);
            (map == self.active_area_id)
                .then(|| atlas.get_room(room_key))
                .flatten()
        });

        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);

        if let Some(area) = atlas.get_area(&self.active_area_id) {
            let layers = sources::colored_layers(&area);
            // A place's link into another map names it as a map link does:
            // under the default style's policy, its anchor room hovered. A
            // link the place keeps on a map room anchors on that map room.
            let layer_label = |layer: &smudgy_cloud::mapper::area_cache::SourceLayer,
                               connection: &RoomConnection| {
                let key = layer.room_key(connection.room.address());
                cross_area_label_visible(
                    resolved.base_conn.cross_area_label_visibility,
                    self.hovered_room.as_ref(),
                    key.area_id,
                    key.room_number,
                )
            };
            frame.with_save(|frame| {
                frame.translate(center);
                frame.scale(self.scaling);
                frame.translate(*self.translation.value());
                frame.scale(1.0_f32);

                let region = self.viewport().visible_region(bounds.size());
                let min_x = (region.x - Self::SPATIAL_QUERY_PADDING) / spacing;
                let min_y = (region.y - Self::SPATIAL_QUERY_PADDING) / spacing;
                let max_x = (region.x + region.width + Self::SPATIAL_QUERY_PADDING) / spacing;
                let max_y = (region.y + region.height + Self::SPATIAL_QUERY_PADDING) / spacing;

                // Ghosts of the levels above and below: just rooms and their
                // connections (labels and shapes stay on their own level),
                // drawn faintly and nudged diagonally so the stack reads as
                // depth. Farthest levels first so nearer ghosts (and the
                // current floor) layer on top.
                for distance in (1..=GHOST_LEVEL_SPREAD).rev() {
                    for delta in [-distance, distance] {
                        let ghost_level = self.level + delta;
                        #[allow(clippy::cast_precision_loss)]
                        let (offset, ghost_opacity) = {
                            let d = delta as f32;
                            (
                                Vector::new(d * GHOST_LEVEL_OFFSET, -d * GHOST_LEVEL_OFFSET),
                                opacity * GHOST_BASE_OPACITY / distance as f32,
                            )
                        };

                        frame.with_save(|frame| {
                            frame.translate(offset);
                            let mut cross_area_labels = Vec::new();
                            area.with_room_connections_in(
                                min_x,
                                min_y,
                                max_x,
                                max_y,
                                |connection| {
                                    if connection.from_level == ghost_level {
                                        let connection = connection.with_room_spacing(spacing);
                                        let paint = resolved.base_conn;
                                        let anchor = map_connection_anchor(&area, connection.room.address());
                                        let label_visible = cross_area_label_visible(
                                            paint.cross_area_label_visibility,
                                            self.hovered_room.as_ref(),
                                            anchor.area_id,
                                            anchor.room_number,
                                        );
                                        let connection_opacity =
                                            ghost_opacity * paint.opacity.unwrap_or(1.0);
                                        render::draw_connection_styled(
                                            frame,
                                            &atlas,
                                            &connection,
                                            connection_opacity,
                                            true,
                                            None,
                                            None,
                                            None,
                                            false,
                                            paint.cross_area_label_background,
                                        );
                                        if label_visible && is_cross_area_connection(&connection) {
                                            cross_area_labels.push((
                                                connection,
                                                paint.cross_area_label_background,
                                                connection_opacity,
                                            ));
                                        }
                                    }
                                },
                            );
                            sources::draw_connections_labelled(
                                frame,
                                &atlas,
                                &layers,
                                ghost_level,
                                (min_x, min_y, max_x, max_y),
                                spacing,
                                ghost_opacity,
                                &|_| false,
                                &layer_label,
                            );
                            area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                                if room.get_level() == ghost_level {
                                    render::draw_room_styled(
                                        frame,
                                        room,
                                        room.get_x() * spacing,
                                        room.get_y() * spacing,
                                        ghost_opacity
                                            * resolved.base_room.opacity.unwrap_or(1.0),
                                        // Ghost floors take the defaultStyle
                                        // base only; per-room apply entries
                                        // accent the current floor.
                                        &resolved.base_room,
                                    );
                                }
                            });
                            sources::draw_rooms(
                                frame,
                                &layers,
                                ghost_level,
                                (min_x, min_y, max_x, max_y),
                                spacing,
                                ghost_opacity,
                    &|_, _| false,
                            );
                            for (connection, background, connection_opacity) in &cross_area_labels {
                                render::draw_cross_area_connection_label(
                                    frame,
                                    &atlas,
                                    connection,
                                    *connection_opacity,
                                    *background,
                                );
                            }
                        });
                    }
                }

                for shape in area.get_shapes() {
                    if shape.level == self.level {
                        let mut shape = shape.clone();
                        shape.x *= spacing;
                        shape.y *= spacing;
                        render::draw_shape(frame, &shape, opacity);
                    }
                }

                for label in area.get_labels() {
                    if label.level == self.level {
                        let mut label = label.clone();
                        label.x *= spacing;
                        label.y *= spacing;
                        render::draw_label(frame, &label, opacity);
                    }
                }
                sources::draw_drawings(frame, &layers, self.level, spacing, opacity, &|_| false);

                // Connections draw in spatial-query order; a style accent
                // changes a connection's paint, not its z-order, so a
                // later-drawn unaccented neighbor can still cross over an
                // accented stroke. Accepted: rooms (and their accents) draw
                // on top of all connections, and route accents read fine in
                // practice; a second accent-only pass would fix full
                // stacking if it ever matters.
                let connections_drawn = Cell::new(0_usize);
                let mut cross_area_labels = Vec::new();
                area.with_room_connections_in(min_x, min_y, max_x, max_y, |connection| {
                    if connection.from_level == self.level {
                        let connection = connection.with_room_spacing(spacing);
                        let (anchor, far) = connection_exit_keys(&connection);
                        let paint = resolved.conn_paint(anchor, far);
                        let connection_opacity = opacity * paint.opacity.unwrap_or(1.0);
                        let anchor_room = map_connection_anchor(&area, connection.room.address());
                        let label_visible = cross_area_label_visible(
                            paint.cross_area_label_visibility,
                            self.hovered_room.as_ref(),
                            anchor_room.area_id,
                            anchor_room.room_number,
                        );
                        let door = resolved.show_doors.then(|| {
                            let state = resolved.door_override(anchor, far);
                            (
                                state.closed.unwrap_or(
                                    connection.door.is_some_and(smudgy_cloud::DoorState::is_shut),
                                ),
                                state.locked.unwrap_or(
                                    connection.door == Some(smudgy_cloud::DoorState::Locked),
                                ),
                                paint.door_color.unwrap_or(DEFAULT_DOOR_COLOR),
                            )
                        });
                        render::draw_connection_styled(
                            frame,
                            &atlas,
                            &connection,
                            connection_opacity,
                            false,
                            paint.color,
                            paint.width,
                            door,
                            false,
                            paint.cross_area_label_background,
                        );
                        if label_visible && is_cross_area_connection(&connection) {
                            cross_area_labels.push((
                                connection,
                                paint.cross_area_label_background,
                                connection_opacity,
                            ));
                        }
                        connections_drawn.set(connections_drawn.get() + 1);
                    }
                });

                sources::draw_connections_labelled(
                    frame,
                    &atlas,
                    &layers,
                    self.level,
                    (min_x, min_y, max_x, max_y),
                    spacing,
                    opacity,
                    &|_| false,
                    &layer_label,
                );

                let rooms_drawn = Cell::new(0_usize);
                area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                    if room.get_level() == self.level {
                        let paint = resolved.room_paint(room.get_room_number());
                        render::draw_room_styled(
                            frame,
                            room,
                            room.get_x() * spacing,
                            room.get_y() * spacing,
                            opacity * paint.opacity.unwrap_or(1.0),
                            &paint,
                        );
                        rooms_drawn.set(rooms_drawn.get() + 1);
                    }
                });

                sources::draw_rooms(
                    frame,
                    &layers,
                    self.level,
                    (min_x, min_y, max_x, max_y),
                    spacing,
                    opacity,
                    &|_, _| false,
                );

                // Destination labels are an overlay: their optional
                // backgrounds and text must remain legible even when the
                // outward label anchor overlaps a room glyph.
                for (connection, background, connection_opacity) in &cross_area_labels {
                    render::draw_cross_area_connection_label(
                        frame,
                        &atlas,
                        connection,
                        *connection_opacity,
                        *background,
                    );
                }

                if let Some(room) = &player_room
                        && room.get_level() == self.level {
                            render::draw_player_indicator_styled(
                                frame,
                                room.get_x() * spacing,
                                room.get_y() * spacing,
                                opacity,
                                resolved.player_color,
                            );
                        }

                // draw_us brackets everything drawn into the frame — spatial
                // queries, ghost passes, shapes/labels, connections, rooms,
                // and the player indicator; only the frame finalization
                // (`into_geometry`) falls outside it.
                if let Some(draw_start) = draw_start {
                    eprintln!(
                        "map draw: bounds={:?} scaling={} translation={:?} opacity={} level={} region=({:.1},{:.1} {:.1}x{:.1}) rooms={} connections={} draw_us={}",
                        bounds,
                        self.scaling,
                        self.translation.value(),
                        opacity,
                        self.level,
                        region.x,
                        region.y,
                        region.width,
                        region.height,
                        rooms_drawn.get(),
                        connections_drawn.get(),
                        draw_start.elapsed().as_micros(),
                    );
                }
            });
        }

        vec![frame.into_geometry()]
    }
}

/// Resolve one connection style's label policy against the currently hovered
/// anchor room, `anchor_room` of `anchor_area` (the map's, or a place's own
/// area). The default remains the legacy always-visible mode.
fn cross_area_label_visible(
    visibility: Option<CrossAreaLabelVisibility>,
    hovered_room: Option<&RoomKey>,
    anchor_area: AreaId,
    anchor_room: RoomNumber,
) -> bool {
    let anchor_hovered = hovered_room
        .is_some_and(|room| room.area_id == anchor_area && room.room_number == anchor_room);
    visibility.unwrap_or_default().is_visible(anchor_hovered)
}

/// Map-owned connections can remain attached to rooms now owned by a Secret.
/// Hover follows the anchor's qualified address, independent of the connection's owner.
fn map_connection_anchor(
    area: &smudgy_cloud::mapper::area_cache::AreaCache,
    address: RoomAddress,
) -> RoomKey {
    area.map_document_layer().map_or_else(
        || RoomKey::new(*area.get_id(), address.number),
        |layer| layer.room_key(address),
    )
}

fn is_cross_area_connection(connection: &RoomConnection) -> bool {
    matches!(
        &connection.to,
        RoomConnectionEnd::External { .. } | RoomConnectionEnd::Unknown { .. }
    )
}

/// The far-endpoint summary of a connection half, reduced to what exit-ref
/// matching needs.
#[derive(Debug, Clone, Copy)]
enum FarEnd {
    Normal {
        room: RoomNumber,
        direction: ExitDirection,
    },
    /// Cross-level half; `direction` is this half's rendered exit direction.
    ToLevel {
        room: RoomNumber,
        direction: ExitDirection,
    },
    SelfLoop,
    /// Dangling, external-area, or redacted: no second selectable endpoint,
    /// so the in-area anchor endpoint alone selects the connection.
    Terminal,
}

/// The exit refs under which one drawn connection half is selectable: its
/// anchor endpoint's ref, plus (when one exists) the far endpoint's.
fn connection_exit_keys(connection: &RoomConnection) -> (MapExitRef, Option<MapExitRef>) {
    let far = match &connection.to {
        RoomConnectionEnd::Normal {
            room, direction, ..
        } => FarEnd::Normal {
            room: room.get_room_number(),
            direction: *direction,
        },
        RoomConnectionEnd::ToLevel {
            room, direction, ..
        } => FarEnd::ToLevel {
            room: room.get_room_number(),
            direction: *direction,
        },
        RoomConnectionEnd::SelfLoop => FarEnd::SelfLoop,
        RoomConnectionEnd::None
        | RoomConnectionEnd::External { .. }
        | RoomConnectionEnd::Unknown { .. } => FarEnd::Terminal,
    };
    exit_keys(
        connection.room.get_room_number(),
        far,
        connection.direction_a,
        connection.direction_b,
    )
}

/// Pure form of [`connection_exit_keys`] over the fields it actually reads.
fn exit_keys(
    anchor_room: RoomNumber,
    far: FarEnd,
    direction_a: ExitDirection,
    direction_b: Option<ExitDirection>,
) -> (MapExitRef, Option<MapExitRef>) {
    let anchor_direction = match far {
        FarEnd::ToLevel { direction, .. } => direction,
        _ => direction_a,
    };
    let anchor = MapExitRef {
        room: anchor_room,
        direction: anchor_direction,
    };
    let other = match far {
        FarEnd::Normal { room, direction } => Some(MapExitRef { room, direction }),
        FarEnd::ToLevel { room, .. } => {
            // Cross-level Connections render once per endpoint level. Offer
            // the opposite endpoint's ref too so one (room, direction) entry
            // accents both involved Up/Down markers, regardless of which
            // half is currently drawn.
            let other_direction = if anchor_direction == direction_a {
                direction_b.unwrap_or(direction_a)
            } else {
                direction_a
            };
            Some(MapExitRef {
                room,
                direction: other_direction,
            })
        }
        FarEnd::SelfLoop => direction_b.map(|direction| MapExitRef {
            room: anchor_room,
            direction,
        }),
        FarEnd::Terminal => None,
    };
    (anchor, other)
}

/// A cheaply cloneable, owning handle to a [`MapView`] — the canvas program
/// the map widget renders through. The element it builds owns an `Rc` of the
/// view, so it is genuinely `'static`: iced can retain it across frames and
/// outlive the store entry that created it without any borrow dangling.
#[derive(Clone)]
pub struct SharedMapView {
    view: Rc<RefCell<MapView>>,
}

impl SharedMapView {
    #[must_use]
    pub fn new(view: Rc<RefCell<MapView>>) -> Self {
        Self { view }
    }

    /// The widget element: the map canvas, clipped to its bounds. The canvas
    /// draws rooms within `SPATIAL_QUERY_PADDING` of the visible region,
    /// which can land outside it. wgpu hides the spill (full-frame redraws
    /// paint neighbors over it); tiny-skia's damage-tracked partial redraws
    /// leave it on screen — hence the clipping container.
    #[must_use]
    pub fn element(&self) -> Element<'static, Message> {
        container(
            Canvas::new(self.clone())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .into()
    }
}

impl canvas::Program<Message, Theme> for SharedMapView {
    type State = ProgramState;

    fn update(
        &self,
        state: &mut ProgramState,
        event: &IcedEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let view = self.view.borrow();
        let action = view.handle_event(state, event, bounds, cursor);
        if !view.is_animating() {
            return action;
        }

        // While a spring/fade is in flight, every event must end in a
        // publish (this program subsumes iced_anim's `Animation` widget so
        // the element can own its state): a publish always schedules a
        // redraw, and that redraw's `RedrawRequested` arrives back here to
        // produce the next tick. An event's own published message takes
        // precedence over a tick — its redraw delivers the tick one event
        // later.
        let tick = Message::AnimationTick(Instant::now());
        let Some(action) = action else {
            return Some(canvas::Action::publish(tick));
        };
        let (message, _redraw_request, status) = action.into_inner();
        let action = canvas::Action::publish(message.unwrap_or(tick));
        Some(if status == iced::event::Status::Captured {
            action.and_capture()
        } else {
            action
        })
    }

    fn draw(
        &self,
        _state: &ProgramState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        self.view.borrow().draw_geometry(renderer, bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::{LocalBackend, RoomUpdates, Uuid};
    use std::rc::Weak;
    use std::sync::Arc;

    /// The element must own its `MapView`: every store-side handle can drop
    /// while iced still retains the element, and the view stays alive until
    /// the element itself goes — the ownership guarantee that replaced the
    /// former `'static` lifetime transmute.
    #[tokio::test]
    async fn element_owns_the_map_view() {
        let cache_dir = std::env::temp_dir()
            .join("smudgy-map-widget-test")
            .join(format!("cache-{}", std::process::id()));
        let mapper = Mapper::new(
            Arc::new(LocalBackend::new(cache_dir.join("local"))),
            cache_dir,
        );

        let view = Rc::new(RefCell::new(MapView::new(mapper, AreaId(Uuid::nil()))));
        let weak: Weak<RefCell<MapView>> = Rc::downgrade(&view);

        let shared = SharedMapView::new(Rc::clone(&view));
        let element = shared.element();
        drop(shared);
        drop(view);

        assert!(
            weak.upgrade().is_some(),
            "the element must keep the view alive"
        );
        drop(element);
        assert!(
            weak.upgrade().is_none(),
            "dropping the element must free the view"
        );
    }

    async fn map_view_with_active_room(tag: &str) -> (MapView, AreaId, AreaId, RoomKey) {
        let cache_dir = std::env::temp_dir()
            .join("smudgy-map-widget-test")
            .join(format!("{tag}-{}", Uuid::new_v4()));
        let mapper = Mapper::new(
            Arc::new(LocalBackend::new(cache_dir.join("local"))),
            cache_dir,
        );
        mapper.load_all_areas().await.expect("load empty map");
        let active = mapper
            .create_area("Active".to_string())
            .await
            .expect("create active area");
        let room = RoomKey::new(active, RoomNumber(7));
        let submission = mapper
            .upsert_room(
                room.clone(),
                RoomUpdates {
                    level: Some(4),
                    x: Some(3.0),
                    y: Some(-2.0),
                    ..RoomUpdates::default()
                },
            )
            .expect("create active room");
        if let Some(operation_id) = submission.operation_id() {
            mapper
                .wait_for_mutation(operation_id)
                .await
                .expect("persist active room");
        }
        let other = AreaId(Uuid::new_v4());
        (MapView::new(mapper, active), active, other, room)
    }

    #[tokio::test]
    async fn newest_active_area_cancels_pending_foreign_area_before_fade() {
        let (mut view, active, other, room) = map_view_with_active_room("cancel-pending").await;

        let _ = view.update(Message::SetPlayerLocation(other, None));
        assert_eq!(view.fade_phase, FadePhase::FadingOut);
        assert_eq!(
            view.pending_area_change
                .as_ref()
                .map(|pending| pending.area_id),
            Some(other)
        );

        let _ = view.update(Message::SetPlayerLocation(active, Some(room.room_number.0)));

        assert_eq!(view.active_area_id, active);
        assert_eq!(view.player_location, Some(room));
        assert!(view.pending_area_change.is_none());
        assert_eq!(view.fade_phase, FadePhase::Idle);
        assert_eq!(*view.area_opacity.value(), 1.0);
        assert_eq!(*view.area_opacity.target(), 1.0);
        assert_eq!(view.level, 4);
        assert_eq!(
            *view.translation.value(),
            Vector::new(
                -3.0 * view.resolved.room_spacing,
                2.0 * view.resolved.room_spacing
            )
        );
    }

    #[tokio::test]
    async fn newest_active_area_reverses_a_partial_fade_out() {
        let (mut view, active, other, room) = map_view_with_active_room("reverse-fade").await;

        let _ = view.update(Message::SetPlayerLocation(other, None));
        view.area_opacity.settle_at(0.4);
        let _ = view.update(Message::SetPlayerLocation(active, Some(room.room_number.0)));

        assert_eq!(view.active_area_id, active);
        assert_eq!(view.player_location, Some(room));
        assert!(view.pending_area_change.is_none());
        assert_eq!(view.fade_phase, FadePhase::FadingIn);
        assert_eq!(*view.area_opacity.value(), 0.4);
        assert_eq!(*view.area_opacity.target(), 1.0);

        view.area_opacity.settle_at(1.0);
        view.handle_fade_progress();
        assert_eq!(view.fade_phase, FadePhase::Idle);
        assert_eq!(view.active_area_id, active);
    }

    #[tokio::test]
    async fn corrective_area_transition_survives_the_stale_areas_fade_in() {
        let (mut view, active, other, room) = map_view_with_active_room("fade-in-correction").await;

        let _ = view.update(Message::SetPlayerLocation(other, None));
        view.area_opacity.settle_at(0.0);
        view.handle_fade_progress();
        assert_eq!(view.active_area_id, other);
        assert_eq!(view.fade_phase, FadePhase::FadingIn);

        view.area_opacity.settle_at(0.4);
        let _ = view.update(Message::SetPlayerLocation(active, Some(room.room_number.0)));
        assert_eq!(view.fade_phase, FadePhase::FadingOut);
        assert_eq!(
            view.pending_area_change
                .as_ref()
                .map(|pending| pending.area_id),
            Some(active)
        );

        view.area_opacity.settle_at(0.0);
        view.handle_fade_progress();
        assert_eq!(view.active_area_id, active);
        assert_eq!(view.player_location, Some(room));
        assert!(view.pending_area_change.is_none());
        assert_eq!(view.fade_phase, FadePhase::FadingIn);
    }

    fn exit(room: i32, direction: ExitDirection) -> MapExitRef {
        MapExitRef {
            room: RoomNumber(room),
            direction,
        }
    }

    /// One `(room, direction)` entry must select **both halves** of a
    /// cross-level Connection: each half renders on its own level, and the
    /// entry may name either endpoint.
    #[test]
    fn one_exit_ref_selects_both_halves_of_a_cross_level_connection() {
        // Room 1 (below) connects Up to room 2 (above); the widget draws
        // one half anchored on each room.
        let lower_half = exit_keys(
            RoomNumber(1),
            FarEnd::ToLevel {
                room: RoomNumber(2),
                direction: ExitDirection::Up,
            },
            ExitDirection::Up,
            Some(ExitDirection::Down),
        );
        let upper_half = exit_keys(
            RoomNumber(2),
            FarEnd::ToLevel {
                room: RoomNumber(1),
                direction: ExitDirection::Down,
            },
            ExitDirection::Up,
            Some(ExitDirection::Down),
        );

        let selects = |keys: &(MapExitRef, Option<MapExitRef>), entry: MapExitRef| {
            keys.0 == entry || keys.1 == Some(entry)
        };
        // An entry naming the lower endpoint reaches both halves...
        assert!(selects(&lower_half, exit(1, ExitDirection::Up)));
        assert!(selects(&upper_half, exit(1, ExitDirection::Up)));
        // ...and so does one naming the upper endpoint.
        assert!(selects(&lower_half, exit(2, ExitDirection::Down)));
        assert!(selects(&upper_half, exit(2, ExitDirection::Down)));
    }

    /// Outbound cross-area, redacted, and dangling connections have no
    /// second selectable endpoint: the in-area anchor endpoint alone
    /// selects them.
    #[test]
    fn terminal_connections_select_via_the_anchor_endpoint_alone() {
        let keys = exit_keys(RoomNumber(5), FarEnd::Terminal, ExitDirection::East, None);
        assert_eq!(keys.0, exit(5, ExitDirection::East));
        assert_eq!(keys.1, None);
    }

    #[test]
    fn cross_area_label_visibility_matches_only_the_anchor_room() {
        let active = AreaId(Uuid::from_u64_pair(0, 1));
        let other = AreaId(Uuid::from_u64_pair(0, 2));
        let hovered = RoomKey {
            area_id: active,
            room_number: RoomNumber(5),
        };
        let elsewhere = RoomKey {
            area_id: other,
            room_number: RoomNumber(5),
        };

        assert!(cross_area_label_visible(
            Some(CrossAreaLabelVisibility::Always),
            None,
            active,
            RoomNumber(5),
        ));
        assert!(cross_area_label_visible(
            Some(CrossAreaLabelVisibility::Hover),
            Some(&hovered),
            active,
            RoomNumber(5),
        ));
        assert!(!cross_area_label_visible(
            Some(CrossAreaLabelVisibility::Hover),
            Some(&hovered),
            active,
            RoomNumber(6),
        ));
        assert!(!cross_area_label_visible(
            Some(CrossAreaLabelVisibility::Hover),
            Some(&elsewhere),
            active,
            RoomNumber(5),
        ));
        assert!(!cross_area_label_visible(
            Some(CrossAreaLabelVisibility::Never),
            Some(&hovered),
            active,
            RoomNumber(5),
        ));
        assert!(
            cross_area_label_visible(None, None, active, RoomNumber(5)),
            "an omitted style preserves the legacy always-visible behavior"
        );
    }

    /// A normal in-area connection is selectable from either endpoint's
    /// exit ref; a self-loop from either of its two directions.
    #[test]
    fn normal_and_self_loop_connections_offer_both_refs() {
        let normal = exit_keys(
            RoomNumber(1),
            FarEnd::Normal {
                room: RoomNumber(2),
                direction: ExitDirection::South,
            },
            ExitDirection::North,
            Some(ExitDirection::South),
        );
        assert_eq!(normal.0, exit(1, ExitDirection::North));
        assert_eq!(normal.1, Some(exit(2, ExitDirection::South)));

        let self_loop = exit_keys(
            RoomNumber(3),
            FarEnd::SelfLoop,
            ExitDirection::In,
            Some(ExitDirection::Out),
        );
        assert_eq!(self_loop.0, exit(3, ExitDirection::In));
        assert_eq!(self_loop.1, Some(exit(3, ExitDirection::Out)));
    }

    /// Apply/doors updates travel through `set_presentation`; they must not
    /// disturb the user's zoom or pan (only a room-spacing change rescales
    /// the translation, preserving the visual center).
    #[tokio::test]
    async fn presentation_updates_keep_zoom_and_pan() {
        let cache_dir = std::env::temp_dir()
            .join("smudgy-map-widget-test")
            .join(format!("pan-{}", std::process::id()));
        let mapper = Mapper::new(
            Arc::new(LocalBackend::new(cache_dir.join("local"))),
            cache_dir,
        );
        let mut view = MapView::new(mapper, AreaId(Uuid::nil()));
        let _ = view.update(Message::Translated(Vector::new(-3.0, 7.5)));
        let _ = view.update(Message::Scaled(80.0, None));

        let mut presentation = MapViewPresentation::default();
        presentation.styles.insert(
            "route".to_string(),
            crate::MapStyle {
                connection_color: Some("#00ff00".to_string()),
                ..crate::MapStyle::default()
            },
        );
        presentation.apply = vec![crate::MapStyleApplication {
            style: "route".to_string(),
            rooms: vec![RoomNumber(1)],
            exits: vec![exit(1, ExitDirection::North)],
            area: None,
        }];
        view.set_presentation(presentation);

        assert_eq!(*view.translation.value(), Vector::new(-3.0, 7.5));
        assert_eq!(view.scaling, 80.0);
        assert_eq!(
            view.resolved.conns[&exit(1, ExitDirection::North)].color,
            smudgy_cloud::parse_css_color("#00ff00")
        );
    }

    /// A cloud map with room 1 at the origin and one Secret whose own room
    /// 2 sits at (5, 7) on level 1, served as the cloud serves it.
    struct SecretCloud {
        details: smudgy_cloud::AreaWithDetails,
    }

    #[async_trait::async_trait]
    impl smudgy_cloud::MapperBackend for SecretCloud {
        async fn create_area(
            &self,
            _request: smudgy_cloud::CreateAreaRequest,
        ) -> smudgy_cloud::CloudResult<smudgy_cloud::Area> {
            Err(smudgy_cloud::CloudError::NotFoundOrNoAccess)
        }

        async fn list_areas(&self) -> smudgy_cloud::CloudResult<Vec<smudgy_cloud::Area>> {
            Ok(vec![self.details.area.clone()])
        }

        async fn get_area(
            &self,
            _area_id: &AreaId,
        ) -> smudgy_cloud::CloudResult<smudgy_cloud::AreaWithDetails> {
            Ok(self.details.clone())
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

    fn placed(number: i32, x: f32, y: f32, level: i32) -> smudgy_cloud::RoomWithDetails {
        smudgy_cloud::RoomWithDetails {
            room_number: RoomNumber(number),
            title: String::new(),
            description: String::new(),
            level,
            x,
            y,
            color: String::new(),
            properties: Vec::new(),
            exits: Vec::new(),
            tags: Default::default(),
            external_id: None,
        }
    }

    async fn map_view_over_a_secret() -> (MapView, AreaId, AreaId) {
        map_view_over_a_secret_at(5.0, 7.0, 1).await
    }

    /// Map room 1 at the origin on level 0; the Secret's own room 2 where
    /// asked.
    async fn map_view_over_a_secret_at(x: f32, y: f32, level: i32) -> (MapView, AreaId, AreaId) {
        map_view_with_secret_and_attachment(x, y, level, false).await
    }

    async fn map_view_with_secret_and_attachment(
        x: f32,
        y: f32,
        level: i32,
        retained: bool,
    ) -> (MapView, AreaId, AreaId) {
        let map = AreaId(Uuid::new_v4());
        let secret = Uuid::new_v4();
        let mut details = smudgy_cloud::AreaWithDetails {
            room_data: Vec::new(),
            area: smudgy_cloud::Area {
                id: map,
                user_id: None,
                atlas_id: None,
                atlas_name: None,
                name: "Library".to_string(),
                created_at: Default::default(),
                rev: 1,
                projection_token: Some("p_view".to_string()),
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
            rooms: vec![placed(1, 0.0, 0.0, 0)],
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
            sources: vec![smudgy_cloud::SourceBundle {
                source: smudgy_cloud::SourceId::Secret(secret),
                name: Some("Bookcase".to_string()),
                ownership: Some("owner".to_string()),
                clan_id: None,
                color: None,
                rev: 1,
                actions: ["read".to_string()].into_iter().collect(),
                properties: Vec::new(),
                rooms: vec![placed(2, x, y, level)],
                room_data: Vec::new(),
                labels: Vec::new(),
                shapes: Vec::new(),
                connections: Vec::new(),
            }],
        };
        if retained {
            details.rooms.push(placed(2, 1.0, 0.0, 0));
            details.room_data.push(smudgy_cloud::RoomData {
                room_number: RoomNumber(2),
                room_source: Some(smudgy_cloud::SourceId::Secret(secret)),
                properties: vec![smudgy_cloud::Property {
                    name: "notes".into(),
                    value: "Map-owned".into(),
                }],
                tags: Default::default(),
                exits: Vec::new(),
            });
        }
        let cache_dir = std::env::temp_dir()
            .join("smudgy-map-widget-test")
            .join(format!("secret-{}", Uuid::new_v4()));
        let mapper = Mapper::new(Arc::new(SecretCloud { details }), cache_dir);
        mapper.load_all_areas().await.expect("load the map");
        (MapView::new(mapper, map), map, AreaId(secret))
    }

    /// A left press and release over the same room clicks it (the press is
    /// the map's); a release elsewhere clicks nothing, and a press where no
    /// room is falls through to whatever lies beneath the map.
    #[tokio::test]
    async fn map_owned_attachment_hover_uses_the_secret_room_not_its_map_namesake() {
        let (view, map, secret) = map_view_with_secret_and_attachment(5.0, 7.0, 0, true).await;
        let area = view.mapper.get_current_atlas().get_area(&map).unwrap();
        let address = RoomAddress::new(smudgy_cloud::SourceId::Secret(secret.0), RoomNumber(2));
        assert!(
            area.map_document_layer()
                .unwrap()
                .attachment(address)
                .is_some()
        );
        let anchor = map_connection_anchor(&area, address);
        assert_eq!(anchor, RoomKey::new(secret, RoomNumber(2)));
        let hover = Some(CrossAreaLabelVisibility::Hover);
        assert!(cross_area_label_visible(
            hover,
            Some(&anchor),
            anchor.area_id,
            anchor.room_number
        ));
        assert!(!cross_area_label_visible(
            hover,
            Some(&RoomKey::new(map, RoomNumber(2))),
            anchor.area_id,
            anchor.room_number
        ));
    }

    #[tokio::test]
    async fn a_room_is_clicked_by_a_press_and_release_over_it() {
        let (mut view, map, _) = map_view_over_a_secret().await;
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(200.0, 200.0));
        view.last_viewport_size.set(Some(bounds.size()));
        let at = |x: f32, y: f32| {
            mouse::Cursor::Available(view.viewport().unproject(Point::new(x, y), bounds.size()))
        };
        let (on_room, off_room) = (at(0.0, 0.0), at(2.0, 2.0));
        let press = IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let outcome = |action: Option<canvas::Action<Message>>| {
            action.map(|action| {
                let (message, _, status) = action.into_inner();
                (message, status)
            })
        };
        let mut state = ProgramState::default();

        let pressed = outcome(view.handle_event(&mut state, &press, bounds, on_room));
        assert!(matches!(
            pressed,
            Some((None, iced::event::Status::Captured))
        ));
        let clicked = outcome(view.handle_event(&mut state, &release, bounds, on_room));
        let Some((Some(Message::RoomClicked(room)), iced::event::Status::Captured)) = clicked
        else {
            panic!("a click: {clicked:?}");
        };
        assert_eq!(room, RoomKey::new(map, RoomNumber(1)));
        let update = view.update(Message::RoomClicked(room.clone()));
        assert!(matches!(update.event, Some(Event::RoomClicked(clicked)) if clicked == room));

        let _ = view.handle_event(&mut state, &press, bounds, on_room);
        let moved_off = outcome(view.handle_event(&mut state, &release, bounds, off_room));
        assert!(moved_off.is_none(), "released elsewhere: {moved_off:?}");
        assert!(
            view.handle_event(&mut state, &press, bounds, off_room)
                .is_none(),
            "no room under the press: it falls through"
        );
        assert!(
            view.handle_event(&mut state, &release, bounds, off_room)
                .is_none()
        );
    }

    /// The pointer finds a Secret's own room as the editor does: on its
    /// level, keyed by the Secret's area, and over the map room it covers.
    #[tokio::test]
    async fn a_secrets_own_room_is_picked_over_the_map_room_it_covers() {
        let (mut view, map, secret) = map_view_over_a_secret().await;
        let spacing = view.resolved.room_spacing;
        assert_eq!(
            &*view.rooms_at(Point::ORIGIN),
            [RoomKey::new(map, RoomNumber(1))]
        );
        assert!(
            view.rooms_at(Point::new(5.0 * spacing, 7.0 * spacing))
                .is_empty(),
            "the Secret's room is on another level"
        );
        view.level = 1;
        assert_eq!(
            &*view.rooms_at(Point::new(5.0 * spacing, 7.0 * spacing)),
            [RoomKey::new(secret, RoomNumber(2))]
        );
        assert!(view.rooms_at(Point::ORIGIN).is_empty());

        let (view, map, secret) = map_view_over_a_secret_at(0.0, 0.0, 0).await;
        assert_eq!(
            &*view.rooms_at(Point::ORIGIN),
            [
                RoomKey::new(secret, RoomNumber(2)),
                RoomKey::new(map, RoomNumber(1))
            ],
            "the Secret's room draws over the map's and is picked first"
        );
        // Hovering it reveals its own links' labels, not the map room's.
        let hovered = RoomKey::new(secret, RoomNumber(2));
        let hover = Some(CrossAreaLabelVisibility::Hover);
        assert!(cross_area_label_visible(
            hover,
            Some(&hovered),
            secret,
            RoomNumber(2)
        ));
        assert!(!cross_area_label_visible(
            hover,
            Some(&hovered),
            map,
            RoomNumber(2)
        ));
    }

    #[tokio::test]
    async fn standing_in_a_secret_room_keeps_its_map_and_marks_the_room() {
        let (mut view, map, secret) = map_view_over_a_secret().await;

        let _ = view.update(Message::SetPlayerLocation(secret, Some(2)));

        assert_eq!(view.active_area_id, map, "the map stays shown");
        assert!(view.pending_area_change.is_none(), "no fade to another map");
        assert_eq!(
            view.player_location,
            Some(RoomKey::new(secret, RoomNumber(2)))
        );
        assert_eq!(view.level, 1);
        assert_eq!(
            *view.translation.value(),
            Vector::new(
                -5.0 * view.resolved.room_spacing,
                -7.0 * view.resolved.room_spacing
            )
        );

        // From another map, the switch goes to the Secret's map.
        let other = AreaId(Uuid::new_v4());
        let _ = view.update(Message::SetPlayerLocation(other, None));
        view.area_opacity.settle_at(0.0);
        view.handle_fade_progress();
        let _ = view.update(Message::SetPlayerLocation(secret, Some(2)));
        assert_eq!(
            view.pending_area_change
                .as_ref()
                .map(|pending| (pending.area_id, pending.player_location.clone())),
            Some((map, Some(RoomKey::new(secret, RoomNumber(2)))))
        );
    }
}
