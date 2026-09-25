//! The unified pane-drag layer: daemon-side window tracking, the drag record,
//! and the target geometry that classifies every release of a tab drag —
//! same-window and cross-window alike (`docs/panes.md` §10).
//!
//! Smudgy owns the whole drag path: a tab's press surface reports press,
//! deadband crossing, and release; the daemon tracks the cursor, classifies
//! the hovered target from live window geometry, and applies the terminal
//! operation as a pre-validated transaction. `iced::widget::PaneGrid` keeps
//! layout and divider resizing only — its native drag path is not wired.
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
//!
//! Target precedence, applied by [`classify_target`] from live geometry:
//! whole-grid edge bands, then a group's header/tab-strip band (merge at the
//! insertion slot; a reorder within the source group), then the body center
//! (swap with the target group's rendered tab), then body-edge thirds (split
//! a new singleton beside the whole target group). The classification the
//! overlay renders is byte-for-byte the one the drop applies — one geometry
//! function feeds both.

use std::collections::HashMap;

use iced::{Event as IcedEvent, Point, Rectangle, Size, keyboard, mouse, window};
use smallvec::SmallVec;

use crate::pane_groups::{GroupId, TabId};
use smudgy_session_model::pane::PaneRef;

/// The click-vs-drag deadband: a press that never strays further than this
/// from its true press point is a click (select), never a drag.
pub const DRAG_DEADBAND: f32 = 10.0;

/// Divisor for the whole-grid edge drop bands: the band depth is
/// `min(width, height) / GRID_EDGE_RATIO`, measured inward from the grid
/// bounds — pane_grid's native `THICKNESS_RATIO`, giving top-level placements
/// the same precedence and reach as pane_grid's own edge bands.
pub const GRID_EDGE_RATIO: f32 = 25.0;

/// Upper bound on the grid-edge band depth. The native proportional depth
/// grows with the window and would swallow the top row's ~30px header bands
/// almost whole on a large grid; header/tab bounds have first priority
/// among pane-local drop surfaces, so the band is bounded to keep every
/// header's merge surface usable while whole-grid placements stay reachable
/// along the rim.
pub const GRID_EDGE_MAX: f32 = 16.0;

/// Header-band height assumed for a group whose strip has not reported a
/// measured height yet (first frame after a temporary drag band mounts).
pub const DEFAULT_HEADER_BAND: f32 = 26.0;

/// A tab drag in flight, recorded when the press crosses the deadband and
/// consumed by exactly one terminal (release, Escape, or an abort when a
/// participant dies mid-drag). Identity is the stable [`TabId`] plus the
/// pane bound to it at pick time; the source group is retained for
/// validation and preview only — every terminal re-resolves all participants
/// against the live model before mutating anything.
#[derive(Debug)]
pub struct TabDrag {
    pub source_window: window::Id,
    /// The dragged tab — the drag's stable identity.
    pub tab: TabId,
    /// The pane bound to the tab at pick time (validation snapshot: a tab
    /// re-bound mid-drag is stale identity and cancels).
    pub slot: PaneRef,
    /// The tab's group at pick time. Validation/preview only; never mutation
    /// authority (interim whole-group moves re-mint group ids).
    pub source_group: GroupId,
    /// The true press point (source-window logical), from the press widget.
    pub press: Point,
    /// The window and classified target the cursor currently resolves to —
    /// what the hovered window's overlay renders. Advisory only: the drop
    /// re-classifies against live geometry at release.
    pub hover: Option<DragHover>,
}

/// The hovered window of an in-flight drag, with its classification.
/// `target == None` means the cursor is over the window but no drop surface
/// (chrome, toolbar, own-group center): a release there is a no-op re-dock.
#[derive(Debug)]
pub struct DragHover {
    pub window: window::Id,
    pub target: Option<ClassifiedTarget>,
}

/// A tab press below the deadband, daemon-recorded. The press surface owns
/// the press hit-test and reports the true press point; the daemon carries
/// the gesture from there, promoting it to a [`TabDrag`] when tracked
/// motion crosses the deadband. Widget state is a fast path only: a subtree
/// rebuilt mid-gesture (an async repaint re-pairing the tree) loses the
/// widget's phase, never the gesture.
#[derive(Debug, Clone, Copy)]
pub struct PendingPress {
    pub window: window::Id,
    pub tab: TabId,
    pub slot: PaneRef,
    pub group: GroupId,
    /// The true press point (window-local logical).
    pub press: Point,
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
    /// Keyboard modifier state (window-agnostic — the keyboard is global).
    /// Tracked so widgets can be handed modifier state at view time instead
    /// of each keeping a shadow copy from `ModifiersChanged`.
    Modifiers(keyboard::Modifiers),
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
        IcedEvent::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
            Some(TrackEvent::Modifiers(*modifiers))
        }
        _ => None,
    }
}

/// A drag-terminal event surfaced by the daemon's drag-gated subscription.
#[derive(Debug, Clone, Copy)]
pub enum DragTerminal {
    /// The left button was released somewhere while a drag was live — the
    /// authoritative terminal (the tab widget's own release event is a fast
    /// path only; pane subtrees and captured events cannot swallow this one).
    Released,
    /// Escape mid-drag: cancel. Below the deadband no drag record exists,
    /// so a press in progress is unaffected (the drag matrix's
    /// `escape-pressed` scenario).
    Escape,
    /// A window lost focus while a gesture was armed. When it is the
    /// gesture's source window, this is the daemon-level proxy for a true OS
    /// capture loss (Win+L, UAC prompt, `WM_CANCELMODE`): the raw release
    /// will never reach this process, so waiting for [`Self::Released`]
    /// would leave the gesture armed forever. [`unfocus_stand_down`] decides
    /// what — if anything — stands down.
    Unfocused,
}

/// `event::listen_with` filter for drag terminals, subscribed only while a
/// gesture (press or drag) is armed. Capture status is deliberately ignored:
/// a widget swallowing the release or the Escape must not be able to strand
/// a drag in flight.
pub fn drag_terminal_event(event: &IcedEvent) -> Option<DragTerminal> {
    match event {
        IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(DragTerminal::Released)
        }
        IcedEvent::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(DragTerminal::Escape),
        IcedEvent::Window(window::Event::Unfocused) => Some(DragTerminal::Unfocused),
        _ => None,
    }
}

/// What a source-window focus loss stands down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandDown {
    /// A press below the deadband: clear it (and with it the full-rate
    /// tracking it keeps subscribed). Without the stand-down, cursor motion
    /// over the source window after focus returns would promote the orphaned
    /// press into a live drag with no button down, and the next left release
    /// anywhere would commit a real drop.
    Press,
    /// A live drag: cancel it with zero mutation. The widget-side proxy
    /// (`CaptureLost`) covers only a press surface whose subtree survived;
    /// this daemon-level terminal covers one that was wiped mid-drag.
    Drag,
}

/// Classifies a window's focus loss against the armed gesture: the source
/// window losing focus stands the gesture down (drag before press — a live
/// drag record supersedes any press bookkeeping); any other window's focus
/// loss is unrelated (cross-window focus churn happens while a gesture's
/// source window keeps the OS capture).
pub fn unfocus_stand_down(
    unfocused: window::Id,
    press_window: Option<window::Id>,
    drag_source: Option<window::Id>,
) -> Option<StandDown> {
    if drag_source == Some(unfocused) {
        Some(StandDown::Drag)
    } else if press_window == Some(unfocused) {
        Some(StandDown::Press)
    } else {
        None
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
    /// Last observed keyboard modifier state (keyboard-global, not
    /// per-window). Handed to widgets at view time.
    modifiers: keyboard::Modifiers,
}

impl WindowTracker {
    pub fn apply(&mut self, id: window::Id, event: TrackEvent) {
        if let TrackEvent::Modifiers(modifiers) = event {
            self.modifiers = modifiers;
            return;
        }
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
            TrackEvent::Modifiers(_) => unreachable!("handled before the entry lookup"),
        }
    }

    /// The last observed keyboard modifier state.
    pub fn modifiers(&self) -> keyboard::Modifiers {
        self.modifiers
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

/// A whole-grid edge drop band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridEdgeSide {
    /// New leading top-level cluster.
    Left,
    /// New trailing top-level cluster.
    Right,
    /// Wrap the whole layout, new group first.
    Top,
    /// Wrap the whole layout, new group second.
    Bottom,
}

/// The semantic operation a release at the classified point performs. Groups
/// are resolved at classification and re-validated at apply time; a group
/// that vanished between the two rejects the drop rather than misdirecting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragAction {
    /// Whole-grid edge placement.
    GridEdge(GridEdgeSide),
    /// Move the dragged tab into `group`'s strip at `slot` (counted with the
    /// strip as it stands, dragged tab included — [`merge_tab`]'s contract).
    /// With the source group as destination this is a reorder.
    ///
    /// [`merge_tab`]: crate::pane_groups::GroupLayout::merge_tab
    Merge { group: GroupId, slot: usize },
    /// Atomic slot swap with `group`'s currently rendered tab.
    Swap { group: GroupId },
    /// Split the dragged tab, as a new singleton, beside the whole `group`
    /// (against the source group itself, the ungroup gesture). `region` is
    /// never `Center`.
    Split { group: GroupId, region: DropRegion },
    /// The hovered window hosts no panes: adopt the tab as its first cluster.
    Vacant,
}

/// A classified drop target: the action plus the exact feedback geometry the
/// overlay renders (grid-local). The overlay and the drop consume the same
/// classification, so target feedback can never disagree with the committed
/// operation.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifiedTarget {
    pub action: DragAction,
    /// The region the drop will occupy or affect.
    pub highlight: Rectangle,
    /// The insertion caret within a header band (merge/reorder targets).
    pub caret: Option<Rectangle>,
}

/// One tab's horizontal span within its group's header band, grid-local,
/// strip order. Feeds insertion-slot classification. `slot` is the tab's
/// index in the group's full member vec: bands cover only the tabs the
/// strip lists, so consecutive bands may skip invisible members (tabs
/// eyeball-hidden under a collapsed toolbar, vacancy bookkeeping), and
/// the drop slot must be counted in member space — [`merge_tab`]'s
/// contract — never band space.
///
/// [`merge_tab`]: crate::pane_groups::GroupLayout::merge_tab
#[derive(Debug, Clone, Copy)]
pub struct TabBand {
    pub start: f32,
    pub end: f32,
    pub slot: usize,
}

/// One rendered pane slot's drop geometry, assembled from live grid regions
/// and the window's draw-time strip mirrors.
#[derive(Debug, Clone)]
pub struct PaneTargetGeom {
    pub group: GroupId,
    /// The pane's full region (header band included), grid-local.
    pub bounds: Rectangle,
    /// Measured header-band height ([`DEFAULT_HEADER_BAND`] until measured).
    pub band_height: f32,
    /// Member tab spans, strip order. Empty when not yet measured — slot
    /// classification then appends (the safe clamp).
    pub tabs: SmallVec<[TabBand; 8]>,
}

/// The insertion slot and caret x for a header-band hit at `x`. The slot
/// is member-space, read from the bands' carried indices: a hit before a
/// band inserts at that band's member, a hit past the last band appends
/// right behind it (`slot + 1` — trailing invisible members keep their
/// positions after the insert).
fn insertion_slot(tabs: &[TabBand], x: f32) -> (usize, Option<f32>) {
    if tabs.is_empty() {
        return (usize::MAX, None);
    }
    let index = tabs
        .iter()
        .take_while(|band| x > (band.start + band.end) / 2.0)
        .count();
    let (slot, caret) = if index == 0 {
        (tabs[0].slot, tabs[0].start)
    } else if index == tabs.len() {
        (tabs[index - 1].slot + 1, tabs[index - 1].end)
    } else {
        (
            tabs[index].slot,
            (tabs[index - 1].end + tabs[index].start) / 2.0,
        )
    };
    (slot, Some(caret))
}

/// The caret rectangle for a header insertion point.
fn caret_rect(x: f32, band: Rectangle) -> Rectangle {
    Rectangle {
        x: x - 1.0,
        y: band.y,
        width: 2.0,
        height: band.height,
    }
}

/// The half of `bounds` a body-edge split will hand to the new singleton.
fn split_half(bounds: Rectangle, region: DropRegion) -> Rectangle {
    match region {
        DropRegion::Left => Rectangle {
            width: bounds.width / 2.0,
            ..bounds
        },
        DropRegion::Right => Rectangle {
            x: bounds.x + bounds.width / 2.0,
            width: bounds.width / 2.0,
            ..bounds
        },
        DropRegion::Top => Rectangle {
            height: bounds.height / 2.0,
            ..bounds
        },
        DropRegion::Bottom => Rectangle {
            y: bounds.y + bounds.height / 2.0,
            height: bounds.height / 2.0,
            ..bounds
        },
        DropRegion::Center => bounds,
    }
}

/// Classify a grid-local point against live target geometry. `None` means no
/// drop surface is under the point — outside the grid entirely (window
/// chrome/toolbar), over the source group's own body center, or an own-edge
/// gesture with nothing to ungroup — and a release there is a no-op re-dock.
///
/// Precedence: whole-grid edge bands, then the hovered pane's header band,
/// then its body center/edge thirds. A point on inter-pane spacing resolves
/// to the nearest pane region — never a resize.
///
/// `source_group` is the dragged tab's group *when the classified window
/// hosts it* (self-target rules apply only there); `source_solo` marks a
/// source group whose only tab is the dragged one.
pub fn classify_target(
    area: Size,
    point: Point,
    panes: &[PaneTargetGeom],
    source_group: Option<GroupId>,
    source_solo: bool,
) -> Option<ClassifiedTarget> {
    let grid = Rectangle::with_size(area);
    if !grid.contains(point) {
        return None;
    }

    // Whole-grid edge bands, nearest edge on overlap (x before y, matching
    // the region thirds' corner rule). The proportional depth mirrors the
    // native path this classification supersedes, bounded so header bands
    // stay usable on large grids.
    let band = (area.width / GRID_EDGE_RATIO)
        .min(area.height / GRID_EDGE_RATIO)
        .min(GRID_EDGE_MAX);
    let side = if point.x < band {
        Some(GridEdgeSide::Left)
    } else if point.x > area.width - band {
        Some(GridEdgeSide::Right)
    } else if point.y < band {
        Some(GridEdgeSide::Top)
    } else if point.y > area.height - band {
        Some(GridEdgeSide::Bottom)
    } else {
        None
    };
    if let Some(side) = side {
        let highlight = match side {
            GridEdgeSide::Left => Rectangle {
                width: band,
                ..grid
            },
            GridEdgeSide::Right => Rectangle {
                x: area.width - band,
                width: band,
                ..grid
            },
            GridEdgeSide::Top => Rectangle {
                height: band,
                ..grid
            },
            GridEdgeSide::Bottom => Rectangle {
                y: area.height - band,
                height: band,
                ..grid
            },
        };
        return Some(ClassifiedTarget {
            action: DragAction::GridEdge(side),
            highlight,
            caret: None,
        });
    }

    // The hovered pane — containment first, nearest center for points on
    // inter-pane spacing/dividers.
    let pane = panes
        .iter()
        .find(|pane| pane.bounds.contains(point))
        .or_else(|| {
            panes.iter().min_by(|a, b| {
                point
                    .distance(a.bounds.center())
                    .total_cmp(&point.distance(b.bounds.center()))
            })
        })?;

    // Header band: merge at the insertion slot (a reorder in the source
    // group). The band always exists during a drag — hidden headers mount a
    // temporary one.
    let band_height = pane.band_height.min(pane.bounds.height);
    let header = Rectangle {
        height: band_height,
        ..pane.bounds
    };
    if point.y < header.y + header.height {
        let (slot, caret_x) = insertion_slot(&pane.tabs, point.x);
        return Some(ClassifiedTarget {
            action: DragAction::Merge {
                group: pane.group,
                slot,
            },
            highlight: header,
            caret: caret_x.map(|x| caret_rect(x, header)),
        });
    }

    // Body thirds below the band.
    let body = Rectangle {
        y: pane.bounds.y + band_height,
        height: pane.bounds.height - band_height,
        ..pane.bounds
    };
    let own = source_group == Some(pane.group);
    match region_for(body, point) {
        DropRegion::Center if own => None, // Already hosted here.
        DropRegion::Center => Some(ClassifiedTarget {
            action: DragAction::Swap { group: pane.group },
            highlight: body,
            caret: None,
        }),
        region => {
            if own && source_solo {
                // Own-edge gesture with a singleton source: nothing to ungroup.
                return None;
            }
            Some(ClassifiedTarget {
                action: DragAction::Split {
                    group: pane.group,
                    region,
                },
                highlight: split_half(pane.bounds, region),
                caret: None,
            })
        }
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
    fn unfocus_of_the_source_window_stands_the_gesture_down() {
        let (source, other) = (window::Id::unique(), window::Id::unique());

        // A press below the deadband stands down when its window unfocuses.
        assert_eq!(
            unfocus_stand_down(source, Some(source), None),
            Some(StandDown::Press)
        );
        // A live drag cancels when its source window unfocuses.
        assert_eq!(
            unfocus_stand_down(source, None, Some(source)),
            Some(StandDown::Drag)
        );
        // The drag record supersedes press bookkeeping if both are armed.
        assert_eq!(
            unfocus_stand_down(source, Some(source), Some(source)),
            Some(StandDown::Drag)
        );
        // Another window's focus loss never touches the gesture.
        assert_eq!(unfocus_stand_down(other, Some(source), None), None);
        assert_eq!(unfocus_stand_down(other, None, Some(source)), None);
        // No gesture armed: nothing to stand down.
        assert_eq!(unfocus_stand_down(source, None, None), None);
    }

    #[test]
    fn terminal_filter_maps_release_escape_and_unfocus() {
        let released = IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let unfocused = IcedEvent::Window(window::Event::Unfocused);
        let focused = IcedEvent::Window(window::Event::Focused);
        let right = IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right));

        assert!(matches!(
            drag_terminal_event(&released),
            Some(DragTerminal::Released)
        ));
        assert!(matches!(
            drag_terminal_event(&unfocused),
            Some(DragTerminal::Unfocused)
        ));
        // Focus gain and non-left releases are not terminals.
        assert!(drag_terminal_event(&focused).is_none());
        assert!(drag_terminal_event(&right).is_none());
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

    // ------------------------------------------------------------------
    // Target classification
    // ------------------------------------------------------------------

    /// Two 400x300 panes side by side in an 804x300 grid (4px spacing),
    /// 26px header bands, two 60px tabs in the left pane's strip.
    fn geoms() -> (Size, Vec<PaneTargetGeom>, GroupId, GroupId) {
        let left_group = GroupId::mint_for_tests();
        let right_group = GroupId::mint_for_tests();
        let area = Size::new(804.0, 300.0);
        let panes = vec![
            PaneTargetGeom {
                group: left_group,
                bounds: Rectangle::new(Point::ORIGIN, Size::new(400.0, 300.0)),
                band_height: 26.0,
                tabs: SmallVec::from_vec(vec![
                    TabBand {
                        start: 2.0,
                        end: 62.0,
                        slot: 0,
                    },
                    TabBand {
                        start: 64.0,
                        end: 124.0,
                        slot: 1,
                    },
                ]),
            },
            PaneTargetGeom {
                group: right_group,
                bounds: Rectangle::new(Point::new(404.0, 0.0), Size::new(400.0, 300.0)),
                band_height: 26.0,
                tabs: SmallVec::new(),
            },
        ];
        (area, panes, left_group, right_group)
    }

    fn classify(
        point: (f32, f32),
        source: Option<GroupId>,
        solo: bool,
    ) -> Option<ClassifiedTarget> {
        let (area, panes) = GEOMS.with(|g| (g.0, g.1.clone()));
        classify_target(area, Point::new(point.0, point.1), &panes, source, solo)
    }

    thread_local! {
        static GEOMS: (Size, Vec<PaneTargetGeom>, GroupId, GroupId) = geoms();
    }

    fn left_group() -> GroupId {
        GEOMS.with(|g| g.2)
    }

    fn right_group() -> GroupId {
        GEOMS.with(|g| g.3)
    }

    #[test]
    fn grid_edge_bands_take_precedence_over_pane_targets() {
        // 10px into the left edge sits over the left pane's header row, but
        // the grid band wins.
        let target = classify((10.0, 10.0), None, false).unwrap();
        assert_eq!(target.action, DragAction::GridEdge(GridEdgeSide::Left));
        let target = classify((800.0, 150.0), None, false).unwrap();
        assert_eq!(target.action, DragAction::GridEdge(GridEdgeSide::Right));
        // x wins over y where bands overlap in the corners.
        let target = classify((10.0, 5.0), None, false).unwrap();
        assert_eq!(target.action, DragAction::GridEdge(GridEdgeSide::Left));
        let target = classify((400.0, 5.0), None, false).unwrap();
        assert_eq!(target.action, DragAction::GridEdge(GridEdgeSide::Top));
        let target = classify((400.0, 295.0), None, false).unwrap();
        assert_eq!(target.action, DragAction::GridEdge(GridEdgeSide::Bottom));
    }

    #[test]
    fn grid_edge_bands_are_depth_capped_on_large_grids() {
        // A 2000x1000 grid: the native ratio would give a 40px band, deep
        // enough to swallow a top-row header. The cap keeps points below
        // GRID_EDGE_MAX out of the band, so the header band stays usable.
        let group = GroupId::mint_for_tests();
        let area = Size::new(2000.0, 1000.0);
        let panes = vec![PaneTargetGeom {
            group,
            bounds: Rectangle::with_size(area),
            band_height: 30.0,
            tabs: SmallVec::new(),
        }];
        let inside_band =
            classify_target(area, Point::new(1000.0, 10.0), &panes, None, false).unwrap();
        assert_eq!(inside_band.action, DragAction::GridEdge(GridEdgeSide::Top));
        let below_band =
            classify_target(area, Point::new(1000.0, 24.0), &panes, None, false).unwrap();
        assert!(
            matches!(below_band.action, DragAction::Merge { .. }),
            "expected the header band below the capped grid band, got {:?}",
            below_band.action
        );
    }

    #[test]
    fn header_band_merges_at_the_insertion_slot() {
        // Before the first tab's center: slot 0, caret at the strip start.
        let target = classify((20.0, 16.0), None, false).unwrap();
        assert_eq!(
            target.action,
            DragAction::Merge {
                group: left_group(),
                slot: 0
            }
        );
        assert!((target.caret.unwrap().x - 1.0).abs() < 0.01);
        // Between the two tabs: slot 1, caret in the gap.
        let target = classify((40.0, 16.0), None, false).unwrap();
        assert_eq!(
            target.action,
            DragAction::Merge {
                group: left_group(),
                slot: 1
            }
        );
        assert!((target.caret.unwrap().x - 62.0).abs() < 0.01);
        // Past the last tab: slot = len (append), caret at its end.
        let target = classify((300.0, 16.0), None, false).unwrap();
        assert_eq!(
            target.action,
            DragAction::Merge {
                group: left_group(),
                slot: 2
            }
        );
    }

    #[test]
    fn gapped_bands_classify_drops_in_member_space() {
        // A five-member strip listing only members 1 and 3 (the rest
        // invisible — eyeball-hidden under a collapsed toolbar): drops
        // land at member-vec slots, never band indices.
        let bands = [
            TabBand {
                start: 2.0,
                end: 62.0,
                slot: 1,
            },
            TabBand {
                start: 64.0,
                end: 124.0,
                slot: 3,
            },
        ];
        // Before the first listed tab: its own member slot.
        assert_eq!(insertion_slot(&bands, 10.0), (1, Some(2.0)));
        // Between the two listed tabs: the second's member slot, caret in
        // the visual gap.
        assert_eq!(insertion_slot(&bands, 63.0), (3, Some(63.0)));
        // Past the last listed tab: append right behind its member.
        assert_eq!(insertion_slot(&bands, 300.0), (4, Some(124.0)));
    }

    #[test]
    fn unmeasured_strip_appends_without_a_caret() {
        let target = classify((600.0, 16.0), None, false).unwrap();
        assert_eq!(
            target.action,
            DragAction::Merge {
                group: right_group(),
                slot: usize::MAX
            }
        );
        assert!(target.caret.is_none());
    }

    #[test]
    fn body_center_swaps_and_own_center_is_a_no_op() {
        let target = classify((200.0, 160.0), None, false).unwrap();
        assert_eq!(
            target.action,
            DragAction::Swap {
                group: left_group()
            }
        );
        // The dragged tab's own group's center offers nothing.
        assert!(classify((200.0, 160.0), Some(left_group()), false).is_none());
        // The other group's center still swaps for the same source.
        let target = classify((600.0, 160.0), Some(left_group()), false).unwrap();
        assert_eq!(
            target.action,
            DragAction::Swap {
                group: right_group()
            }
        );
    }

    #[test]
    fn body_edges_split_and_the_solo_own_edge_is_a_no_op() {
        let target = classify((200.0, 280.0), None, false).unwrap();
        assert_eq!(
            target.action,
            DragAction::Split {
                group: left_group(),
                region: DropRegion::Bottom
            }
        );
        // Ungrouping from a multi-tab source group is real...
        let target = classify((200.0, 280.0), Some(left_group()), false).unwrap();
        assert!(matches!(target.action, DragAction::Split { .. }));
        // ...but a singleton source dragged to its own edge has nothing to
        // ungroup.
        assert!(classify((200.0, 280.0), Some(left_group()), true).is_none());
    }

    #[test]
    fn spacing_between_panes_resolves_to_the_nearest_pane() {
        // x=402 sits on the divider spacing; the nearest center is the left
        // pane's, and the point classifies against its body arithmetic.
        let target = classify((401.0, 160.0), None, false).unwrap();
        match target.action {
            DragAction::Split { group, region } => {
                assert_eq!(group, left_group());
                assert_eq!(region, DropRegion::Right);
            }
            other => panic!("expected a split, got {other:?}"),
        }
    }

    #[test]
    fn outside_the_grid_is_no_target() {
        assert!(classify((-5.0, 100.0), None, false).is_none());
        assert!(classify((400.0, 400.0), None, false).is_none());
    }

    #[test]
    fn overlay_and_drop_share_the_same_clamped_geometry() {
        // The highlight for an edge split is exactly the half the singleton
        // will occupy.
        let target = classify((200.0, 280.0), None, false).unwrap();
        assert_eq!(
            target.highlight,
            Rectangle::new(Point::new(0.0, 150.0), Size::new(400.0, 150.0))
        );
        // A header merge highlights the band itself.
        let target = classify((40.0, 16.0), None, false).unwrap();
        assert_eq!(
            target.highlight,
            Rectangle::new(Point::ORIGIN, Size::new(400.0, 26.0))
        );
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
