//! Cross-window pane drag support: daemon-side window tracking and the
//! geometry that turns a pane_grid `DragEvent::Canceled` into a no-op
//! re-dock, a cross-window transplant, or a tear-out (§2.9 of
//! `docs/panes.md`).
//!
//! pane_grid handles in-window drags natively; everything here exists because
//! a drag released outside the source grid surfaces only as `Canceled` in the
//! *source* window (the OS captures the mouse for the initiating window, so
//! no other window sees anything). The daemon reconstructs the release point
//! in screen space from tracked window origins and the last captured cursor
//! position, then hit-tests the other smudgy windows itself.
//!
//! Coordinate model: iced delivers window origins (`window::Event::Moved`,
//! `window::position`) and cursor positions in *logical* coordinates scaled
//! by each window's own scale factor, so logical points from two windows on
//! monitors with different DPI are not comparable. All cross-window math here
//! round-trips through physical pixels via the per-window tracked scale
//! factor: `physical = logical * scale` exactly inverts iced's conversion.
//!
//! Tracking granularity: every event the daemon subscription maps to a
//! message makes iced rebuild and repaint *every* window, so the
//! high-frequency projections (`Moved` fires per mouse increment while the
//! user drags a window; `CursorMoved` fires on any mouse motion) are only
//! listened to while a pane drag is in flight ([`track_event`]). Idle
//! windows track just the rare geometry facts ([`track_event_idle`]);
//! origins that went stale while idle are re-queried via `window::position`
//! when a drag is picked.

use std::collections::HashMap;

use iced::widget::pane_grid;
use iced::{Event as IcedEvent, Point, Rectangle, Size, mouse, window};

use crate::windows::smudgy_window::PaneRef;

/// pane_grid's drag deadband (`DRAG_DEADBAND_DISTANCE`, private in iced): a
/// release within this distance of the pick point was a plain title-bar
/// click, published as `Canceled`. Mirrored so the daemon applies the same
/// click-vs-drag rule to its own disambiguation.
pub const DRAG_DEADBAND: f32 = 10.0;

/// A cross-window drag in flight: recorded at `DragEvent::Picked`, consumed
/// (or discarded) at the terminal `Dropped`/`Canceled` — or by an abort
/// (pane/session/window death mid-drag). A drag can also end with *no*
/// terminal event (cursor unavailable at release); the stale record is
/// harmless because pane_grid can only publish another terminal event after
/// a fresh `Picked`, which overwrites this.
#[derive(Debug, Clone, Copy)]
pub struct ActiveDrag {
    pub source_window: window::Id,
    /// The source grid's internal pane id — only meaningful in that grid.
    pub grid_pane: pane_grid::Pane,
    pub slot: PaneRef,
    /// The deadband reference: the first source-window cursor position
    /// tracked during the drag (cursor tracking only runs mid-drag, so the
    /// pick itself is never observed — the pointer has moved at most a few
    /// pixels against a 10-pixel deadband by the first sample). `None` when
    /// the drag saw no motion at all; disambiguation then treats the
    /// release as a plain click (the safe no-op).
    pub pick_cursor: Option<Point>,
}

/// Window-geometry facts observed from the event stream, one per live window
/// (all windows, not just smudgy windows — membership is filtered at use).
#[derive(Debug, Clone, Copy)]
pub struct TrackedWindow {
    /// Outer position in the window's own logical coordinates. `None` until
    /// observed — permanently `None` on platforms without global positions
    /// (Wayland), which degrades cross-window drops to tear-out.
    pub origin: Option<Point>,
    /// Inner (content) size, logical. The window is borderless with custom
    /// chrome, so inner and outer coincide.
    pub size: Size,
    /// The window's scale factor (`physical = logical * scale`).
    pub scale: f32,
    /// Last cursor position seen by this window, window-local logical —
    /// only tracked while a pane drag is in flight. While the OS captures
    /// the drag for this window, this keeps updating outside the window
    /// bounds (verified on Windows through winit 0.30).
    pub cursor: Option<Point>,
}

impl Default for TrackedWindow {
    fn default() -> Self {
        Self {
            origin: None,
            size: Size::ZERO,
            scale: 1.0,
            cursor: None,
        }
    }
}

/// A tracking-relevant event, filtered out of the raw daemon event stream by
/// [`track_event`].
#[derive(Debug, Clone, Copy)]
pub enum TrackEvent {
    /// The `window::position` seed task's answer for a fresh window (issued
    /// in case the window's `Opened` fired before the daemon subscription was
    /// polled). `None` keeps the origin unknown rather than clearing it.
    Origin(Option<Point>),
    /// `window::Event::Opened`: position and size in one event.
    OriginAndSize(Option<Point>, Size),
    Moved(Point),
    Resized(Size),
    Rescaled(f32),
    Focused,
    CursorMoved(Point),
}

/// Maps a raw event to its tracking projection, if any — the full,
/// drag-in-flight filter. Kept free of the daemon `Message` type so
/// `main.rs` can wrap it in a plain `event::listen_with` fn.
pub fn track_event(event: &IcedEvent) -> Option<TrackEvent> {
    match event {
        IcedEvent::Window(window::Event::Opened { position, size }) => {
            Some(TrackEvent::OriginAndSize(*position, *size))
        }
        IcedEvent::Window(window::Event::Moved(position)) => Some(TrackEvent::Moved(*position)),
        IcedEvent::Window(window::Event::Resized(size)) => Some(TrackEvent::Resized(*size)),
        IcedEvent::Window(window::Event::Rescaled(scale)) => Some(TrackEvent::Rescaled(*scale)),
        IcedEvent::Window(window::Event::Focused) => Some(TrackEvent::Focused),
        IcedEvent::Mouse(mouse::Event::CursorMoved { position }) => {
            Some(TrackEvent::CursorMoved(*position))
        }
        _ => None,
    }
}

/// The no-drag-in-flight filter: [`track_event`] minus the high-frequency
/// projections. `Moved` and `CursorMoved` fire per mouse increment and each
/// mapped event costs a rebuild-and-repaint of every window, so they are
/// dropped here and only tracked mid-drag; the pick re-queries the origins
/// this filter missed.
pub fn track_event_idle(event: &IcedEvent) -> Option<TrackEvent> {
    match event {
        IcedEvent::Window(window::Event::Moved(_))
        | IcedEvent::Mouse(mouse::Event::CursorMoved { .. }) => None,
        _ => track_event(event),
    }
}

/// Per-window geometry observed from the event stream plus a focus-MRU
/// order. The MRU is the tie-break for overlapping drop targets: no z-order
/// API exists, so "most recently focused wins" is the documented best effort.
#[derive(Debug, Default)]
pub struct WindowTracker {
    windows: HashMap<window::Id, TrackedWindow>,
    /// Most-recently-focused first.
    mru: Vec<window::Id>,
}

impl WindowTracker {
    pub fn apply(&mut self, id: window::Id, event: TrackEvent) {
        let entry = self.windows.entry(id).or_default();
        match event {
            TrackEvent::Origin(origin) => {
                if origin.is_some() {
                    entry.origin = origin;
                }
            }
            TrackEvent::OriginAndSize(origin, size) => {
                if origin.is_some() {
                    entry.origin = origin;
                }
                entry.size = size;
            }
            TrackEvent::Moved(origin) => entry.origin = Some(origin),
            TrackEvent::Resized(size) => entry.size = size,
            TrackEvent::Rescaled(scale) => entry.scale = scale,
            TrackEvent::CursorMoved(position) => entry.cursor = Some(position),
            TrackEvent::Focused => {
                self.mru.retain(|other| *other != id);
                self.mru.insert(0, id);
            }
        }
    }

    pub fn remove(&mut self, id: window::Id) {
        self.windows.remove(&id);
        self.mru.retain(|other| *other != id);
    }

    pub fn get(&self, id: window::Id) -> Option<&TrackedWindow> {
        self.windows.get(&id)
    }

    /// Every tracked window, most-recently-focused first; windows never yet
    /// focused trail in arbitrary order.
    pub fn mru_order(&self) -> Vec<window::Id> {
        let mut order = self.mru.clone();
        for id in self.windows.keys() {
            if !order.contains(id) {
                order.push(*id);
            }
        }
        order
    }
}

/// A window-local logical point lifted to physical screen space.
pub fn screen_point(origin: Point, local: Point, scale: f32) -> Point {
    Point::new((origin.x + local.x) * scale, (origin.y + local.y) * scale)
}

/// The window's rect in physical screen space, if its origin is known.
pub fn window_rect(track: &TrackedWindow) -> Option<Rectangle> {
    let origin = track.origin?;
    Some(Rectangle::new(
        Point::new(origin.x * track.scale, origin.y * track.scale),
        Size::new(
            track.size.width * track.scale,
            track.size.height * track.scale,
        ),
    ))
}

/// A physical screen point translated into the window's local logical
/// coordinates — `None` when the window's origin is unknown or the point
/// falls outside its rect.
pub fn window_local(track: &TrackedWindow, screen: Point) -> Option<Point> {
    let rect = window_rect(track)?;
    if !rect.contains(screen) {
        return None;
    }
    Some(Point::new(
        screen.x / track.scale - track.origin?.x,
        screen.y / track.scale - track.origin?.y,
    ))
}

/// Where within a hovered pane a cross-window drop lands. Mirrors
/// pane_grid's own `layout_region` thirds (x tested before y), with `Center`
/// meaning "split along the pane's longer axis" for cross-window drops
/// (the native center-swap has no cross-window analogue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropRegion {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

/// The drop region of `point` within `bounds` — pane_grid's center-vs-edge
/// thirds. The caller guarantees containment (points outside classify by the
/// same arithmetic, which nearest-region fallbacks rely on).
pub fn region_for(bounds: Rectangle, point: Point) -> DropRegion {
    if point.x < bounds.x + bounds.width / 3.0 {
        DropRegion::Left
    } else if point.x > bounds.x + 2.0 * bounds.width / 3.0 {
        DropRegion::Right
    } else if point.y < bounds.y + bounds.height / 3.0 {
        DropRegion::Top
    } else if point.y > bounds.y + 2.0 * bounds.height / 3.0 {
        DropRegion::Bottom
    } else {
        DropRegion::Center
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(origin: Option<(f32, f32)>, size: (f32, f32), scale: f32) -> TrackedWindow {
        TrackedWindow {
            origin: origin.map(|(x, y)| Point::new(x, y)),
            size: Size::new(size.0, size.1),
            scale,
            cursor: None,
        }
    }

    #[test]
    fn screen_point_scales_origin_and_local_together() {
        // A 2x window at logical (100, 50): local (10, 10) is physical
        // (220, 120) — both terms scale, not just the local offset.
        let p = screen_point(Point::new(100.0, 50.0), Point::new(10.0, 10.0), 2.0);
        assert_eq!(p, Point::new(220.0, 120.0));
    }

    #[test]
    fn window_local_round_trips_across_scales() {
        // Source at 1x reports a screen point; target at 2x maps it back into
        // its own logical space.
        let source = tracked(Some((0.0, 0.0)), (800.0, 600.0), 1.0);
        let target = tracked(Some((500.0, 100.0)), (400.0, 300.0), 2.0);
        let screen = screen_point(
            source.origin.unwrap(),
            Point::new(1100.0, 300.0), // captured cursor, way right of source
            source.scale,
        );
        let local = window_local(&target, screen).unwrap();
        assert_eq!(local, Point::new(50.0, 50.0));
    }

    #[test]
    fn window_local_misses_outside_and_without_origin() {
        let target = tracked(Some((500.0, 100.0)), (400.0, 300.0), 1.0);
        assert!(window_local(&target, Point::new(100.0, 100.0)).is_none());
        let unknown = tracked(None, (400.0, 300.0), 1.0);
        assert!(window_local(&unknown, Point::new(600.0, 200.0)).is_none());
    }

    #[test]
    fn region_thirds_match_pane_grid_order() {
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(300.0, 300.0));
        assert_eq!(
            region_for(bounds, Point::new(50.0, 150.0)),
            DropRegion::Left
        );
        assert_eq!(
            region_for(bounds, Point::new(250.0, 150.0)),
            DropRegion::Right
        );
        assert_eq!(region_for(bounds, Point::new(150.0, 50.0)), DropRegion::Top);
        assert_eq!(
            region_for(bounds, Point::new(150.0, 250.0)),
            DropRegion::Bottom
        );
        assert_eq!(
            region_for(bounds, Point::new(150.0, 150.0)),
            DropRegion::Center
        );
        // x wins over y in the corners, exactly like pane_grid.
        assert_eq!(region_for(bounds, Point::new(50.0, 50.0)), DropRegion::Left);
    }

    #[test]
    fn idle_filter_drops_only_the_high_frequency_events() {
        let moved = IcedEvent::Window(window::Event::Moved(Point::new(1.0, 2.0)));
        let cursor = IcedEvent::Mouse(mouse::Event::CursorMoved {
            position: Point::new(3.0, 4.0),
        });
        let resized = IcedEvent::Window(window::Event::Resized(Size::new(5.0, 6.0)));
        let focused = IcedEvent::Window(window::Event::Focused);

        assert!(track_event(&moved).is_some());
        assert!(track_event(&cursor).is_some());
        assert!(track_event_idle(&moved).is_none());
        assert!(track_event_idle(&cursor).is_none());
        assert!(track_event_idle(&resized).is_some());
        assert!(track_event_idle(&focused).is_some());
    }

    #[test]
    fn mru_orders_focus_then_stragglers() {
        let (a, b, c) = (
            window::Id::unique(),
            window::Id::unique(),
            window::Id::unique(),
        );
        let mut tracker = WindowTracker::default();
        tracker.apply(a, TrackEvent::Resized(Size::new(1.0, 1.0)));
        tracker.apply(b, TrackEvent::Resized(Size::new(1.0, 1.0)));
        tracker.apply(c, TrackEvent::Resized(Size::new(1.0, 1.0)));
        tracker.apply(a, TrackEvent::Focused);
        tracker.apply(b, TrackEvent::Focused);
        let order = tracker.mru_order();
        assert_eq!(&order[..2], &[b, a]);
        assert_eq!(order[2], c);
        tracker.remove(b);
        assert_eq!(tracker.mru_order()[0], a);
        assert!(tracker.get(b).is_none());
    }
}
