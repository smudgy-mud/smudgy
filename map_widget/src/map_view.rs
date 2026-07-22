use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iced::{
    Length, Point, Rectangle, Size, Vector,
    keyboard, mouse,
    widget::{Canvas, canvas, container},
};
use smudgy_cloud::{AreaId, Mapper, RoomNumber, mapper::RoomKey};

use iced_anim::{Animated, spring::Motion, transition::Easing};

use crate::{Update, render, viewport::Viewport};
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
    /// Advance both in-flight animations (pan spring + area fade) to `now`.
    /// Published by the canvas program on any event while animating; the
    /// publish schedules a redraw, whose `RedrawRequested` produces the next
    /// tick, so the loop self-sustains until both values settle.
    AnimationTick(Instant),
}

#[derive(Debug, Clone)]
pub enum Event {
    HoveredRoomChanged(Option<RoomKey>),
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
        }
    }

    fn rooms_at_point(&self, point: &Point, bounds: &Size) -> Box<[RoomKey]> {
        let atlas = self.mapper.get_current_atlas();

        let point = self.viewport().project(*point, *bounds);
        let half_size = render::MAP_ROOM_SIZE / 2.0;
        let min_x = point.x - half_size;
        let min_y = point.y - half_size;
        let max_x = point.x + half_size;
        let max_y = point.y + half_size;

        atlas
            .get_area(&self.active_area_id)
            .map(|area| {
                let mut hits: Vec<RoomKey> = Vec::new();
                area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                    if room.get_level() == self.level
                        && room.get_x() - half_size < point.x
                        && room.get_x() + half_size > point.x
                        && room.get_y() - half_size < point.y
                        && room.get_y() + half_size > point.y
                    {
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
                let area_changed = area_id != self.active_area_id;

                if area_changed {
                    let mut pending = PendingAreaChange {
                        area_id,
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
                            pending.translation =
                                Vector::new(-room.get_x() , -room.get_y() );
                            pending.level = room.get_level();
                        }
                    } else {
                        pending.player_location = None;
                    }

                    self.pending_area_change = Some(pending);
                    self.start_area_fade();
                    return Update::none();
                }

                self.level = 0;

                if let Some(room_number) = room_number {
                    let room_key = RoomKey {
                        area_id,
                        room_number: RoomNumber(room_number),
                    };

                    if let Some(room) = self.mapper.get_current_atlas().get_room(&room_key) {
                        self.player_location = Some(room_key);
                        let target = Vector::new(-room.get_x() , -room.get_y() );
                        let visible = self.is_point_visible(Point {
                            x: room.get_x(),
                            y: room.get_y(),
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

/// Canvas-local state: the in-flight interaction plus the last known
/// keyboard modifiers (tracked so scroll gestures can branch on them).
#[derive(Default)]
pub struct ProgramState {
    interaction: Interaction,
    modifiers: keyboard::Modifiers,
}

impl MapView {
    /// Zoom by a wheel step (±1 ≈ one notch), anchored at the cursor when
    /// possible. Captures the event even at the zoom limits so scrolling
    /// over the map never leaks to widgets beneath it.
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

            let translation = if let Some(cursor_to_center) =
                cursor.position_from(bounds.center())
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

        // Track modifiers before the cursor gate so the state stays fresh
        // even while the cursor is outside the canvas.
        if let IcedEvent::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
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

        let player_room_number = self.player_location.as_ref().and_then(|room_key| {
            (room_key.area_id == self.active_area_id).then_some(room_key.room_number)
        });

        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);

        if let Some(area) = atlas.get_area(&self.active_area_id) {
            frame.with_save(|frame| {
                frame.translate(center);
                frame.scale(self.scaling);
                frame.translate(*self.translation.value());
                frame.scale(1.0);

                let region = self.viewport().visible_region(bounds.size());
                let min_x = region.x - Self::SPATIAL_QUERY_PADDING;
                let min_y = region.y - Self::SPATIAL_QUERY_PADDING;
                let max_x = region.x + region.width + Self::SPATIAL_QUERY_PADDING;
                let max_y = region.y + region.height + Self::SPATIAL_QUERY_PADDING;

                // Ghosts of the levels above and below: just rooms and their
                // connections (labels and shapes stay on their own level),
                // drawn faintly and nudged diagonally so the stack reads as
                // depth. Farthest levels first so nearer ghosts (and the
                // current floor) layer on top. The widget never indicates
                // secrets, so secret rooms ghost like any other — hence
                // `show_secrets: false`.
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
                            area.with_room_connections_in(
                                min_x,
                                min_y,
                                max_x,
                                max_y,
                                |connection| {
                                    if connection.from_level == ghost_level {
                                        render::draw_connection(
                                            frame,
                                            &atlas,
                                            connection,
                                            ghost_opacity,
                                            false,
                                            true,
                                        );
                                    }
                                },
                            );
                            area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                                if room.get_level() == ghost_level {
                                    render::draw_room(frame, room, ghost_opacity, false);
                                }
                            });
                        });
                    }
                }

                for shape in area.get_shapes() {
                    if shape.level == self.level {
                        render::draw_shape(frame, shape, opacity, false);
                    }
                }

                for label in area.get_labels() {
                    if label.level == self.level {
                        render::draw_label(frame, label, opacity, false);
                    }
                }

                let connections_drawn = Cell::new(0_usize);
                area.with_room_connections_in(min_x, min_y, max_x, max_y, |connection| {
                    if connection.from_level == self.level {
                        render::draw_connection(frame, &atlas, connection, opacity, false, false);
                        connections_drawn.set(connections_drawn.get() + 1);
                    }
                });

                let rooms_drawn = Cell::new(0_usize);
                area.with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                    if room.get_level() == self.level {
                        render::draw_room(frame, room, opacity, false);
                        rooms_drawn.set(rooms_drawn.get() + 1);
                    }
                });

                if let Some(player_room_number) = player_room_number
                    && let Some(room) = area.get_room(&player_room_number)
                        && room.get_level() == self.level {
                            render::draw_player_indicator(
                                frame,
                                room.get_x(),
                                room.get_y(),
                                opacity,
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
    use smudgy_cloud::{LocalBackend, Uuid};
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
}
