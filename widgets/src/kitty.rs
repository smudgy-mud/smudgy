//! Kitty graphics placements in the terminal: the widget an inline object resolves to when
//! its resource is a [`Placement`].
//!
//! A placement's box is fixed when the server places it: `columns` cells of its line by
//! `rows` text rows, measured here in the pane's own grid ([`ObjectMetrics`]), so the row
//! is exactly as tall as the riser expects. What fills the box is read every frame: nothing
//! while the image decodes, after its decode failed, once the quota evicted its pixels, or
//! once a delete removed the placement (its box stays: nothing reflows); otherwise the
//! source rectangle at its natural pixel size or contained in the box, anchored top-left
//! and shifted by the placement's pixel offsets, clipped to the box.
//!
//! A placement and its relative placements (`P`/`Q`, at `H` columns and `V` rows from the
//! parent's top-left cell, each with relative placements of its own, down to
//! [`PLACEMENT_DEPTH_LIMIT`] levels) are drawn by its widget as one family, in the
//! protocol's stacking order: ascending z-index, then image id, then creation order, so a
//! child below its parent's z-index or id paints before the parent. Each is drawn in the
//! same cell metrics with its own image, source rectangle, fit and offsets, culled against
//! the viewport but not clipped to the parent. A relative placement has no inline object
//! of its own: the pane culls the family by the parent's box grown to reach every child's,
//! so a child in view still draws when its parent's own box has left the viewport, but a
//! parent whose row is no longer laid out (it left the pane's overscan band, a few rows
//! beyond the viewport) takes its children with it, wherever they are. Among the line's
//! objects the pane places the family where its parent goes, by the parent's z-index and
//! id alone: a child with a negative z-index under a parent drawn over the text draws over
//! the text too (and under its parent, when its z-index or id is the lower), and a
//! sibling placement the pane draws after the parent covers the child whatever the
//! child's id.
//!
//! Pixels become an iced handle through the process-global image store
//! ([`ImageStore::ensure_rgba`]), so every placement of one image shares one handle and one
//! texture upload per frame: the root frame keyed `kitty:{uid}`, any other frame
//! `kitty:{uid}/{frame id}` (a fresh id whenever a frame's pixels change, so such a key
//! never shows stale pixels; the root's key is the image's, and an entry left under it by
//! earlier root pixels is dropped before the new ones are ensured). The widget memoises
//! the cell it drew in its tree state and ensures again when the frame changes or the
//! store evicts the cell. Each entry lives as long as the pixels it was minted from: the
//! image record's UI slot holds a [hold](ImageStore::hold) on every entry minted from its
//! frames ([`ImageSlot`]), so they leave the store when the session evicts or frees the
//! image, when the root frame's pixels are replaced, or when the record drops (an edited
//! or deleted frame's entry goes when the next one is ensured), and no handle waits for
//! the store's own cap to turn over — which, for a held entry, never comes: the session's
//! quota bounds them. A hold's release takes only the very cell it was taken on, so when
//! a root replacement drops the slot and a new one is attached, the fresh root entry and
//! any frame entries ensured again under their stable keys are safe from the old holds'
//! queued releases: those name the cells that are gone, not the new ones under the keys.
//!
//! An animated image plays on a [`Timeline`] the widgets keep per image, shared by all its
//! placements so they show the same frame (as kitty's do) and kept across remounts: which
//! frame shows and since when, stepped by the frames' gaps as kitty steps them. Gapless
//! frames are skipped, a loading animation holds its last frame until more arrive, a
//! running one wraps until its loops are played. The client's controls ([`Animation`])
//! move it: a new generation restarts it from the current frame, a new control serial
//! resets the loop count and, leaving `Stopped`, restarts the clock from the frame
//! showing; stopped, it holds the frame it was showing. The redraw event steps the
//! timelines of the animations the family shows in the viewport, once per redraw, and
//! asks for the next redraw at their earliest frame boundary ([`Shell::request_redraw_at`],
//! which iced merges, so any number of placements cost one redraw per boundary); a
//! stopped, still or finished animation asks for nothing. The draw that follows shows the
//! frames as of that step, at the event's instant, so a boundary between the two cannot
//! cost a redraw. Every mount gets the event: iced delivers it to each widget before each
//! draw, and the pane hands an anchor the window's events (it is inert to the mouse
//! alone).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use iced::advanced::image::{FilterMethod, Handle, Image};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{self, Clipboard, Layout, Shell, Widget, layout, mouse, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, window};
use smudgy_protocol::graphics::{
    Animation, AnimationState, Fit, Frame, FrameId, ImageRecord, ImageUid, PLACEMENT_DEPTH_LIMIT,
    Placement, PlacementGeometry,
};
use smudgy_ui_shared::inline_object::ObjectMetrics;

use crate::image_store::{EntryHold, EntryState, ImageEntryCell, ImageStore};

thread_local! {
    static ACTIVE_STORE: RefCell<Option<ImageStore>> = const { RefCell::new(None) };
}

/// Run `f` with `store` as the image store terminal graphics placements built inside it
/// draw through. The UI wraps its inline-object resolver in this, as it does with the
/// map and text-editor store contexts; a placement built outside any context draws
/// nothing.
pub fn with_image_store_context<R>(store: &ImageStore, f: impl FnOnce() -> R) -> R {
    ACTIVE_STORE.with(|slot| {
        let previous = slot.replace(Some(store.clone()));
        let result = f();
        slot.replace(previous);
        result
    })
}

fn active_store() -> Option<ImageStore> {
    ACTIVE_STORE.with(|slot| slot.borrow().clone())
}

/// The element for an inline object whose resource is a terminal graphics placement, or
/// `None` when it is something else.
pub(crate) fn placement_element<Message, Theme, Renderer>(
    object: &smudgy_session_model::inline_content::InlineObject,
    metrics: ObjectMetrics,
) -> Option<Element<'static, Message, Theme, Renderer>>
where
    Renderer: advanced::image::Renderer<Handle = Handle> + 'static,
{
    let placement = object.resource.clone().downcast::<Placement>().ok()?;
    Some(KittyPlacement::new(placement, metrics, active_store()).into())
}

/// One kitty graphics placement, drawn into the box of cells it covers.
pub(crate) struct KittyPlacement {
    placement: Arc<Placement>,
    metrics: ObjectMetrics,
    store: Option<ImageStore>,
}

impl KittyPlacement {
    /// A placement measured in `metrics`, drawing through `store` (with none, it keeps its
    /// box and draws nothing).
    #[must_use]
    pub(crate) fn new(
        placement: Arc<Placement>,
        metrics: ObjectMetrics,
        store: Option<ImageStore>,
    ) -> Self {
        Self {
            placement,
            metrics,
            store,
        }
    }

    /// `columns` cells by `rows` text rows.
    fn box_size(&self) -> Size {
        self.box_of(&self.placement.geometry)
    }

    /// A box of `geometry`'s cells in the pane's grid.
    fn box_of(&self, geometry: &PlacementGeometry) -> Size {
        Size::new(
            f32::from(geometry.columns) * self.metrics.cell_width,
            f32::from(geometry.rows) * self.metrics.line_height,
        )
    }

    /// The store cell holding the frame to draw at `now`, or `None` when there is nothing
    /// to draw. Steady state is a memo hit plus an uncontended read or two of the image's
    /// state (for an animation, its frames and timeline too); the store is only consulted
    /// when the frame changes, on the first draw after the pixels land and after an
    /// eviction.
    fn entry(&self, state: &State, now: Instant) -> Option<Arc<ImageEntryCell>> {
        let mut memo = state.memo.borrow_mut();
        *memo = if self.placement.is_deleted() {
            None
        } else {
            self.store.as_ref().and_then(|store| {
                let image = &self.placement.image;
                let (number, frame) = frame_to_draw(image, now)?;
                let cell = ensure_cell(store, image, number, &frame, memo.as_ref())?;
                Some(Memo {
                    uid: image.uid,
                    frame: frame.id,
                    cell,
                })
            })
        };
        memo.as_ref().map(|memo| memo.cell.clone())
    }

    /// The store cell for a relative placement's image: one this draw ensured already, or
    /// through the memo the last draw left in `previous`, which this one rebuilds in `kept`.
    fn relative_entry(
        &self,
        image: &Arc<ImageRecord>,
        previous: &[Memo],
        kept: &mut Vec<Memo>,
        now: Instant,
    ) -> Option<Arc<ImageEntryCell>> {
        let store = self.store.as_ref()?;
        let (number, frame) = frame_to_draw(image, now)?;
        // Placements of one image show one frame: the timeline is the image's.
        if let Some(memo) = kept.iter().find(|memo| memo.uid == image.uid) {
            return Some(memo.cell.clone());
        }
        let memo = previous.iter().find(|memo| memo.uid == image.uid);
        let cell = ensure_cell(store, image, number, &frame, memo)?;
        kept.push(Memo {
            uid: image.uid,
            frame: frame.id,
            cell: cell.clone(),
        });
        Some(cell)
    }

    /// The placement (in `bounds`) with its live relative placements and their boxes, in
    /// the protocol's stacking order: ascending z-index, then image id, then creation
    /// order (the sort is stable, so a parent comes before its children and each before
    /// the next). Empty when it has none: it draws alone then, without the list. A
    /// deleted placement's relative placements went with it. The widget reads it with
    /// its box at the origin ([`Self::relative_family`]), so the boxes are offsets from
    /// its own and hold wherever the layout puts it.
    fn family(&self, bounds: Rectangle) -> Vec<(Arc<Placement>, Rectangle)> {
        if self.placement.is_deleted() {
            return Vec::new();
        }
        let children = self.placement.child_placements();
        if children.is_empty() {
            return Vec::new();
        }
        let mut family = vec![(self.placement.clone(), bounds)];
        self.for_each_relative(&children, bounds.position(), 1, &mut |child, bounds| {
            family.push((child.clone(), bounds));
        });
        family.sort_by_key(|(placement, _)| (placement.geometry.z, placement.image.id));
        family
    }

    /// [`Self::family`] with every box relative to the placement's own, whose size is
    /// `size`: what the redraw event keeps for the draw after it, which puts the family
    /// where the layout it is handed says — iced may lay the pane out again between the
    /// two (a drag's autoscroll, a hovered link), and the family must move with the text.
    fn relative_family(&self, size: Size) -> Vec<(Arc<Placement>, Rectangle)> {
        self.family(Rectangle::with_size(size))
    }

    /// Step to `now` the timelines of the animations `family` (each box relative to the
    /// placement's, which is `bounds`; or, when it is empty, the placement alone) shows in
    /// `viewport`: when the earliest next frame among them is due, or `None` when nothing
    /// playing shows there.
    fn step(
        &self,
        family: &[(Arc<Placement>, Rectangle)],
        bounds: Rectangle,
        viewport: &Rectangle,
        now: Instant,
    ) -> Option<Instant> {
        if self.placement.is_deleted() || self.store.is_none() {
            return None;
        }
        let mut next = None;
        if family.is_empty() {
            if bounds.intersects(viewport) {
                advance(&self.placement.image, now, &mut next);
            }
        } else {
            let offset = offset_of(bounds);
            for (placement, relative) in family {
                if (*relative + offset).intersects(viewport) {
                    advance(&placement.image, now, &mut next);
                }
            }
        }
        next
    }

    /// Calls `visit` with each live relative placement below `children`'s parent and its
    /// box, in drawing order: a child, then its own, depth first. `cell` is the parent's
    /// top-left cell; `depth` the children's distance from the placement on the line.
    #[allow(
        clippy::cast_precision_loss,
        reason = "relative offsets are a few thousand cells at most, exact in f32"
    )]
    fn for_each_relative(
        &self,
        children: &[Arc<Placement>],
        cell: Point,
        depth: usize,
        visit: &mut impl FnMut(&Arc<Placement>, Rectangle),
    ) {
        if depth > PLACEMENT_DEPTH_LIMIT {
            return;
        }
        for child in children {
            // A deleted placement took its own relative placements with it.
            if child.is_deleted() {
                continue;
            }
            let offset = child.relative.unwrap_or_default();
            let origin = Point::new(
                cell.x + offset.columns as f32 * self.metrics.cell_width,
                cell.y + offset.rows as f32 * self.metrics.line_height,
            );
            visit(child, Rectangle::new(origin, self.box_of(&child.geometry)));
            self.for_each_relative(&child.child_placements(), origin, depth + 1, visit);
        }
    }
}

/// The frame of `image` to draw at `now` and its number (1-based), the image's timeline
/// stepped when it is animated. `None` while it has no pixels. The frames are read in
/// place; only the one to draw is copied out (a reference-count bump of its pixels).
fn frame_to_draw(image: &Arc<ImageRecord>, now: Instant) -> Option<(usize, Frame)> {
    image.with_frames(|frames| {
        let position = if frames.len() < 2 {
            1
        } else {
            advance_timeline(image, frames, now)?.0
        };
        frames
            .get(position - 1)
            .map(|frame| (position, frame.clone()))
    })
}

/// Step `image`'s timeline to `now` against `frames`, as the record has them: the frame
/// showing, and when the next is due. `None` for an image without frames. Stepping again
/// to the same instant changes nothing, so a draw after a redraw event's step costs a
/// look at the frame showing.
fn advance_timeline(
    image: &Arc<ImageRecord>,
    frames: &[Frame],
    now: Instant,
) -> Option<(usize, Option<Instant>)> {
    if frames.is_empty() {
        return None;
    }
    let animation = image.animation();
    Some(with_shared(image, |shared| {
        let timeline = shared
            .timeline
            .get_or_insert_with(|| Timeline::new(&animation, frames.len(), now));
        let due = timeline.advance(&animation, frames, now);
        (timeline.position, due)
    }))
}

/// Step `image`'s timeline to `now` when it has frames to play, merging when the next is
/// due into `next`.
fn advance(image: &Arc<ImageRecord>, now: Instant, next: &mut Option<Instant>) {
    let due = image.with_frames(|frames| {
        if frames.len() < 2 {
            return None;
        }
        advance_timeline(image, frames, now)?.1
    });
    merge_due(next, due);
}

/// `next` brought forward to `due`, when that is earlier.
fn merge_due(next: &mut Option<Instant>, due: Option<Instant>) {
    if let Some(due) = due {
        *next = Some(next.map_or(due, |next| next.min(due)));
    }
}

/// The store key of frame `number` (1-based) of the image `uid`, whose pixels are `id`'s:
/// the root's is the image's own, any other's names its pixels. Not a URL: the store's
/// other keys are the sources scripts name, and a `<Image src>` spelled like one of these
/// would otherwise hold the key (the store returns an existing entry as it is).
fn frame_key(uid: ImageUid, number: usize, id: FrameId) -> String {
    if number == 1 {
        format!("kitty:{uid}")
    } else {
        format!("kitty:{uid}/{id}")
    }
}

/// The store cell holding `frame`, frame `number` of `image`: `memo` when it is a live cell
/// for that frame, otherwise one ensured from the frame's pixels now and tied to the
/// record ([`ImageSlot`]). `None` when the record was evicted since the frame was read.
fn ensure_cell(
    store: &ImageStore,
    image: &Arc<ImageRecord>,
    number: usize,
    frame: &Frame,
    memo: Option<&Memo>,
) -> Option<Arc<ImageEntryCell>> {
    if let Some(memo) = memo
        && memo.uid == image.uid
        && memo.frame == frame.id
        && !memo.cell.is_evicted()
    {
        return Some(memo.cell.clone());
    }
    // The slot first: one attached now drops what an earlier slot left under the root's
    // key before this ensures under it.
    let slot = image_slot(store, image);
    if slot.is_none() && image.is_evicted() {
        return None;
    }
    let key = frame_key(image.uid, number, frame.id);
    let pixels = &frame.pixels;
    let Some(slot) = slot else {
        // Without a slot of ours the entry lasts until the store's cap turns it over.
        return Some(store.ensure_rgba(&key, pixels.width, pixels.height, pixels.rgba.clone()));
    };
    // Held as it is inserted, so the sweep an insert over the cap runs counts the fetched
    // entries alone (`ImageStore::ensure_rgba_held`). Holding the slot until here keeps an
    // eviction that raced this draw honest: the record let go of it, and dropping it below
    // takes the new entry too.
    let (cell, taken) =
        store.ensure_rgba_held(&key, pixels.width, pixels.height, pixels.rgba.clone());
    slot.hold(
        store,
        (number != 1).then_some(frame.id),
        &key,
        &cell,
        taken,
        image,
    );
    Some(cell)
}

/// `image`'s slot of ours: the one it holds, or a new one attached now. `None` when the
/// record refuses one: it was evicted, or holds a slot that is not ours.
fn image_slot(store: &ImageStore, image: &Arc<ImageRecord>) -> Option<Arc<ImageSlot>> {
    with_shared(image, |shared| {
        if let Some(slot) = shared.slot.upgrade() {
            return Some(slot);
        }
        let slot = Arc::new(ImageSlot::default());
        image.attach_ui_slot(Box::new(slot.clone())).ok()?;
        // The slot before went with the root frame's old pixels, and its holds' releases
        // are queued on the store's runtime: an entry still under the root's key shows
        // those pixels (the key is the image's, not the pixels'), so drop it here, before
        // the caller ensures the new ones under it. A release takes only the cell its
        // hold was on, so the fresh root entry is safe from the old root hold's release.
        store.remove(&format!("kitty:{}", image.uid));
        shared.slot = Arc::downgrade(&slot);
        Some(slot)
    })
}

/// What the widgets keep per image on the UI thread, shared by every placement of it:
/// the slot its store entries are tied to and the timeline it plays on. Kept while the
/// record lives, across remounts and the slot's turnover, so an animation keeps its
/// place; forgotten once the record drops.
struct Shared {
    record: Weak<ImageRecord>,
    /// The record's UI slot, while it holds one of ours.
    slot: Weak<ImageSlot>,
    /// Where its animation is, from the first draw that found it with frames to play.
    timeline: Option<Timeline>,
}

/// Every image's [`Shared`] entry, and the size at which the next new entry first forgets
/// those of dropped records: twice what was left the last time, so the sweeps cost
/// constant time per entry however many images scrollback keeps alive.
#[derive(Default)]
struct Images {
    entries: HashMap<ImageUid, Shared>,
    sweep_at: usize,
}

thread_local! {
    static IMAGES: RefCell<Images> = RefCell::new(Images::default());
}

/// Run `f` on `image`'s [`Shared`] entry, made on first use.
fn with_shared<R>(image: &Arc<ImageRecord>, f: impl FnOnce(&mut Shared) -> R) -> R {
    IMAGES.with_borrow_mut(|images| {
        if images.entries.len() >= images.sweep_at && !images.entries.contains_key(&image.uid) {
            images
                .entries
                .retain(|_, shared| shared.record.strong_count() > 0);
            images.sweep_at = (images.entries.len() * 2).max(64);
        }
        let shared = images.entries.entry(image.uid).or_insert_with(|| Shared {
            record: Arc::downgrade(image),
            slot: Weak::new(),
            timeline: None,
        });
        f(shared)
    })
}

/// What the widgets put in an image record's UI slot: a [hold](ImageStore::hold) on each
/// store entry minted from the record's frames, so they all leave the store with the
/// pixels — when the session evicts or frees the image, when the root frame's pixels are
/// replaced (a slot attached afterwards starts empty, and takes over the entries of the
/// frames that did not change), or when the record drops. Every placement of the image
/// adds to the one slot, found through the image's [`Shared`] entry.
#[derive(Default)]
struct ImageSlot {
    holds: Mutex<Holds>,
}

/// An [`ImageSlot`]'s holds, and the frames they were last swept against.
#[derive(Default)]
struct Holds {
    /// Each hold with the frame its entry shows: `None` for the root's, keyed by the
    /// image alone, a frame's id for any other.
    held: Vec<(Option<FrameId>, EntryHold)>,
    /// The frames after the root as of the last sweep for frames that are gone: how many,
    /// and the newest id. Ids only grow (an edited or pushed frame gets a fresh one) and a
    /// delete shrinks the count, so while both stand every held frame is still there and
    /// the sweep is skipped.
    swept_against: (usize, u64),
}

impl ImageSlot {
    /// Hold `cell`, the entry under `key` of `frame` (`None`: the root), unless the hold
    /// there already is on that cell — with `taken`, the hold the store gave as it
    /// inserted the entry ([`ImageStore::ensure_rgba_held`]), or one taken now — and let
    /// go of the entries of frames `image` no longer has under those ids (an edited
    /// frame's old pixels, a deleted frame, one that became the root and draws under its
    /// key): nothing draws them any more.
    fn hold(
        &self,
        store: &ImageStore,
        frame: Option<FrameId>,
        key: &str,
        cell: &Arc<ImageEntryCell>,
        taken: Option<EntryHold>,
        image: &ImageRecord,
    ) {
        let released: Vec<_> = {
            let mut holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
            let mut released =
                image.with_frames(|frames| sweep(&mut holds, frames.get(1..).unwrap_or(&[])));
            let take = |taken: Option<EntryHold>| taken.unwrap_or_else(|| store.hold(key, cell));
            match holds.held.iter_mut().find(|(id, _)| *id == frame) {
                // Held already: a hold taken at the insert (an entry the slot held
                // before cannot have been inserted now) would be surplus.
                Some((_, held)) if held.is_on(cell) => released.extend(taken),
                // The entry was evicted and ensured again: the hold moves to the new cell.
                Some((_, held)) => released.push(std::mem::replace(held, take(taken))),
                None => holds.held.push((frame, take(taken))),
            }
            released
        };
        // Their releases are queued outside the lock.
        drop(released);
    }
}

/// How many frames follow the root and the newest id among them: ids only grow and a
/// delete shrinks the count, so an unchanged fingerprint means every held frame stands.
fn sweep_fingerprint(frames: &[Frame]) -> (usize, u64) {
    (
        frames.len(),
        frames.iter().map(|frame| frame.id.0).max().unwrap_or(0),
    )
}

/// Let go of `holds`' entries of frames not among `frames` (the frames after the root),
/// when those changed since the last sweep.
fn sweep(holds: &mut Holds, frames: &[Frame]) -> Vec<EntryHold> {
    let fingerprint = sweep_fingerprint(frames);
    if fingerprint == holds.swept_against {
        return Vec::new();
    }
    holds.swept_against = fingerprint;
    let live: HashSet<FrameId> = frames.iter().map(|frame| frame.id).collect();
    let (kept, released): (Vec<_>, Vec<_>) = std::mem::take(&mut holds.held)
        .into_iter()
        .partition(|(id, _)| id.is_none_or(|id| live.contains(&id)));
    holds.held = kept;
    released.into_iter().map(|(_, hold)| hold).collect()
}

/// How far past a frame boundary a timeline catches up by skipping frames. Later than
/// this (the window was hidden, nothing redrew) it shows the next frame from now instead
/// of fast-forwarding through frames nobody saw, as kitty always does.
const MAX_CATCH_UP: Duration = Duration::from_millis(500);

/// An image's playing position, which the widgets keep against what the client set
/// ([`Animation`]): the frame showing and since when, stepped by the frames' gaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timeline {
    /// The frame showing, 1-based.
    position: usize,
    /// When it started showing.
    shown_at: Instant,
    /// Complete passes played since the last control serial: a running animation with
    /// [`Animation::loops`] set holds its last frame once it has played them.
    passes: u32,
    /// Waiting on the frame showing: a loading animation at its last frame for more, or
    /// a running one with its loops played. The frame it steps to next shows from then,
    /// not from when its predecessor's gap ran out.
    held: bool,
    /// What the record said at the last step.
    generation: u64,
    control_serial: u64,
    state: AnimationState,
}

impl Timeline {
    /// A timeline starting at `animation`'s current frame, now.
    fn new(animation: &Animation, frame_count: usize, now: Instant) -> Self {
        Self {
            position: clamp_frame(animation.current, frame_count),
            shown_at: now,
            passes: 0,
            held: false,
            generation: animation.generation,
            control_serial: animation.control_serial,
            state: animation.state,
        }
    }

    /// Bring the timeline to `now` against `animation` and `frames` as the record has
    /// them: take in the client's controls since the last step, then step past every
    /// frame whose gap has run out (as kitty steps: the next frame with a gap, a wrap at
    /// the end counting a pass). When the next frame is due, or `None` when nothing will
    /// change by itself: stopped, a single frame, every frame gapless, or holding.
    ///
    /// A new control serial resets the pass count without moving the position, as
    /// kitty's `s=` zeroes its loop counter and nothing else. So an `s=3` re-sent once
    /// the loops are played and the last frame held steps on from that frame, and the
    /// step wraps and counts the first pass at once: `v=2` replays nothing and `v=3`
    /// one pass — exactly as kitty, whose `handle_animation_control_command` leaves
    /// `current_frame_index` on the last frame with `current_loop` zeroed, and whose
    /// `scan_active_animations` then counts the wrap before showing frame one. A
    /// client that wants the loops over sends `c=1` with it, which restarts the
    /// position (a new generation) so the first pass is played in full.
    fn advance(
        &mut self,
        animation: &Animation,
        frames: &[Frame],
        now: Instant,
    ) -> Option<Instant> {
        let count = frames.len();
        if animation.generation != self.generation {
            self.generation = animation.generation;
            self.position = clamp_frame(animation.current, count);
            self.shown_at = now;
            self.held = false;
        }
        if animation.control_serial != self.control_serial {
            self.control_serial = animation.control_serial;
            self.passes = 0;
            if self.state == AnimationState::Stopped && animation.state != AnimationState::Stopped {
                self.shown_at = now;
                self.held = false;
            }
        }
        self.state = animation.state;
        self.position = self.position.clamp(1, count.max(1));
        if animation.state == AnimationState::Stopped
            || count < 2
            || frames.iter().all(|frame| frame.gap_ms == 0)
        {
            return None;
        }
        let running = animation.state == AnimationState::Running;
        let played = |passes: u32| running && animation.loops != 0 && passes >= animation.loops;
        // A full pass at most: further behind than that, it resynchronises below.
        for step in 0..=count {
            let due = self.shown_at + gap(&frames[self.position - 1]);
            if now < due {
                return Some(due);
            }
            if played(self.passes) {
                self.held = true;
                return None;
            }
            let (next, wrapped) = next_shown(frames, self.position);
            if wrapped {
                if !running {
                    // Loading: wait at the last frame for more.
                    self.held = true;
                    return None;
                }
                self.passes = self.passes.saturating_add(1);
                if played(self.passes) {
                    self.held = true;
                    return None;
                }
            }
            let late = now.saturating_duration_since(due);
            self.shown_at = if self.held || late > MAX_CATCH_UP || step == count {
                now
            } else {
                due
            };
            self.held = false;
            self.position = next;
        }
        Some(self.shown_at + gap(&frames[self.position - 1]))
    }
}

/// A 1-based frame number within `1..=count` (1 with no frames).
fn clamp_frame(number: u32, count: usize) -> usize {
    usize::try_from(number)
        .unwrap_or(usize::MAX)
        .clamp(1, count.max(1))
}

/// How long `frame` shows when the animation runs.
fn gap(frame: &Frame) -> Duration {
    Duration::from_millis(u64::from(frame.gap_ms))
}

/// The frame after `position` that an animation shows, skipping gapless ones, and whether
/// reaching it wraps past the last frame. Back at `position` itself when it is the only
/// frame with a gap; callers rule out every frame being gapless.
fn next_shown(frames: &[Frame], position: usize) -> (usize, bool) {
    let mut index = position - 1;
    let mut wrapped = false;
    for _ in 0..frames.len() {
        index += 1;
        if index == frames.len() {
            index = 0;
            wrapped = true;
        }
        if frames[index].gap_ms != 0 {
            return (index + 1, wrapped);
        }
    }
    (position, wrapped)
}

/// Where a box laid out from the origin goes when the placement's box is `bounds`.
fn offset_of(bounds: Rectangle) -> Vector {
    Vector::new(bounds.x, bounds.y)
}

/// Per-mount state: the memoised store cell of the frame drawn, and the same for the
/// images of the relative placements the last draw drew; what the last redraw event
/// stepped, for the draw that follows it. Interior mutability because `draw` sees the
/// tree immutably; the UI thread is the only reader.
#[derive(Default)]
struct State {
    memo: RefCell<Option<Memo>>,
    relatives: RefCell<Vec<Memo>>,
    /// The family as of the last redraw event, in drawing order (empty: the placement
    /// alone), each box relative to the placement's own ([`KittyPlacement::relative_family`]),
    /// which `draw` shows where its layout puts them rather than reading the children
    /// again.
    family: RefCell<Vec<(Arc<Placement>, Rectangle)>>,
    /// The instant of the last redraw event, which stepped the timelines: the draw that
    /// follows shows the frames as of it.
    stepped_at: Cell<Option<Instant>>,
}

/// A store cell a draw drew: frame `frame` of the image `uid`.
struct Memo {
    uid: ImageUid,
    frame: FrameId,
    cell: Arc<ImageEntryCell>,
}

/// Where a placement's pixels land.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Target {
    /// The whole image, scaled and positioned so its source rectangle lands on `visible`.
    image: Rectangle,
    /// The source rectangle's footprint, cut to the box: everything else is clipped.
    visible: Rectangle,
}

/// Lay the source rectangle of an image `image_size` pixels large into `bounds`: at one
/// logical pixel per image pixel for [`Fit::Natural`] (the processor sized the box from
/// that; a box a few pixels wider than the image just leaves a margin), or scaled to fit
/// what the offsets leave of the box for [`Fit::Box`], aspect preserved. Either way it
/// is anchored top-left at the `X`/`Y` offsets. `None` when nothing would show.
/// A box is letterboxed as the specification says; kitty's own renderer stretches an
/// ordinary placement into its `c`/`r` box instead, and we follow the specification.
#[allow(
    clippy::cast_precision_loss,
    reason = "image sizes and source rectangles are at most 10000 pixels a side, exact in f32"
)]
fn target(
    geometry: &PlacementGeometry,
    image_size: (u32, u32),
    bounds: Rectangle,
) -> Option<Target> {
    let source = geometry.source;
    if source.width == 0 || source.height == 0 || image_size.0 == 0 || image_size.1 == 0 {
        return None;
    }
    let (x_offset, y_offset) = (f32::from(geometry.x_offset), f32::from(geometry.y_offset));
    let (source_width, source_height) = (source.width as f32, source.height as f32);
    let scale = match geometry.fit {
        Fit::Natural => 1.0,
        Fit::Box { .. } => ((bounds.width - x_offset) / source_width)
            .min((bounds.height - y_offset) / source_height),
    };
    // NaN comes from a box with no area.
    if scale.is_nan() || scale <= 0.0 {
        return None;
    }
    let origin = Point::new(bounds.x + x_offset, bounds.y + y_offset);
    let visible = Rectangle::new(
        origin,
        Size::new(source_width * scale, source_height * scale),
    )
    .intersection(&bounds)?;
    let image = Rectangle::new(
        Point::new(
            origin.x - source.x as f32 * scale,
            origin.y - source.y as f32 * scale,
        ),
        Size::new(image_size.0 as f32 * scale, image_size.1 as f32 * scale),
    );
    Some(Target { image, visible })
}

/// Draws the image in `cell` into `bounds` as `geometry` lays it out, clipped to its box and
/// the viewport. `layered` says whether an earlier image of this widget's draw took a
/// layer: then this one takes one too, since a layer paints above everything drawn into the
/// one around it and an image drawn there would land under the earlier one.
fn paint<Renderer>(
    renderer: &mut Renderer,
    cell: &ImageEntryCell,
    geometry: &PlacementGeometry,
    bounds: Rectangle,
    viewport: &Rectangle,
    layered: &mut bool,
) where
    Renderer: advanced::image::Renderer<Handle = Handle>,
{
    let EntryState::Ready {
        handle,
        width,
        height,
        ..
    } = &*cell.state()
    else {
        return;
    };
    let Some(target) = target(geometry, (*width, *height), bounds) else {
        return;
    };
    let Some(clip) = target.visible.intersection(viewport) else {
        return;
    };
    let image = Image::new(handle.clone())
        .filter_method(FilterMethod::Linear)
        .snap(true);
    if target.image.is_within(&clip) && !*layered {
        renderer.draw_image(image, target.image, clip);
    } else {
        // A cropped or overflowing image: not every renderer honours an image's own
        // clip bounds, but all of them clip to a layer.
        renderer.with_layer(clip, |renderer| {
            renderer.draw_image(image, target.image, clip);
        });
        *layered = true;
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for KittyPlacement
where
    Renderer: advanced::image::Renderer<Handle = Handle>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        let size = self.box_size();
        Size::new(Length::Fixed(size.width), Length::Fixed(size.height))
    }

    /// Exactly the box, whatever the limits: a placement wider than the row overflows it
    /// and is clipped at the pane's edge, as a terminal clips an image at the screen's,
    /// and its height is what the riser lifted the row's text by.
    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(self.box_size())
    }

    /// Reports the placement and its box to an operation walking the pane, as
    /// [`Operation::custom`] with the `Arc<Placement>` for its state, then each of its
    /// live relative placements with theirs: how a tool or test finds where an image
    /// landed.
    fn operate(
        &mut self,
        _tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.custom(None, layout.bounds(), &mut self.placement);
        if self.placement.is_deleted() {
            return;
        }
        let children = self.placement.child_placements();
        self.for_each_relative(
            &children,
            layout.bounds().position(),
            1,
            &mut |child, bounds| {
                operation.custom(None, bounds, &mut child.clone());
            },
        );
    }

    /// On each redraw event, reads the family, steps the timelines of the animations it
    /// shows in `viewport` to the event's instant and asks for the next redraw when the
    /// earliest next frame among them is due. Iced keeps the earliest request of all the
    /// widgets, so placements showing the same boundary cost one redraw. The draw that
    /// follows shows what this stepped.
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let family = self.relative_family(bounds.size());
        if let Some(at) = self.step(&family, bounds, viewport, *now) {
            shell.request_redraw_at(at);
        }
        *state.family.get_mut() = family;
        state.stepped_at.set(Some(*now));
    }

    /// The placement's image in its box and its relative placements in theirs, the family
    /// in ascending z-index, then image id, then creation order, each showing its
    /// animation's frame as of the last redraw event's step, and each box where the
    /// layout handed in puts it. The pane's culling let the family through by the parent's
    /// reach; each member is culled against `viewport` here.
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        // A deleted placement's relative placements went with it.
        if self.placement.is_deleted() {
            return;
        }
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        // A mount no redraw event has reached (iced sends one before each draw; a test may
        // not) reads the family and steps for itself, now.
        let now = state.stepped_at.get().unwrap_or_else(Instant::now);
        let stepped = state.family.borrow();
        let own;
        let family: &[(Arc<Placement>, Rectangle)] = if state.stepped_at.get().is_some() {
            &stepped
        } else {
            own = self.relative_family(bounds.size());
            &own
        };
        let mut layered = false;
        if family.is_empty() {
            if !state.relatives.borrow().is_empty() {
                state.relatives.take();
            }
            if bounds.intersects(viewport)
                && let Some(cell) = self.entry(state, now)
            {
                paint(
                    renderer,
                    &cell,
                    &self.placement.geometry,
                    bounds,
                    viewport,
                    &mut layered,
                );
            }
        } else {
            let offset = offset_of(bounds);
            let previous = state.relatives.take();
            let mut kept = Vec::with_capacity(previous.len());
            for (placement, relative) in family {
                let bounds = *relative + offset;
                let own = Arc::ptr_eq(placement, &self.placement);
                // A relative placement deleted since the step draws nothing.
                if !bounds.intersects(viewport) || (!own && placement.is_deleted()) {
                    continue;
                }
                let cell = if own {
                    self.entry(state, now)
                } else {
                    self.relative_entry(&placement.image, &previous, &mut kept, now)
                };
                if let Some(cell) = cell {
                    paint(
                        renderer,
                        &cell,
                        &placement.geometry,
                        bounds,
                        viewport,
                        &mut layered,
                    );
                }
            }
            *state.relatives.borrow_mut() = kept;
        }
    }
}

impl<'a, Message, Theme, Renderer> From<KittyPlacement> for Element<'a, Message, Theme, Renderer>
where
    Renderer: advanced::image::Renderer<Handle = Handle> + 'a,
{
    fn from(placement: KittyPlacement) -> Self {
        Element::new(placement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_store::tests::{test_store, wait_until};
    use iced::advanced::image::{Allocation, Error};
    use iced::window::RedrawRequest;
    use smudgy_protocol::graphics::{
        DecodedPixels, GraphicsError, ImageData, ImageRecord, RelativeOffset, SourceRect,
    };

    /// A renderer that records image draws and layers.
    #[derive(Default)]
    struct Recorder {
        layers: Vec<Rectangle>,
        images: Vec<Drawn>,
    }

    #[derive(Debug, PartialEq)]
    struct Drawn {
        bounds: Rectangle,
        clip: Rectangle,
        filter: FilterMethod,
        handle: iced::advanced::image::Id,
    }

    impl advanced::Renderer for Recorder {
        fn start_layer(&mut self, bounds: Rectangle) {
            self.layers.push(bounds);
        }
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _transformation: iced::Transformation) {}
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, _quad: renderer::Quad, _background: impl Into<iced::Background>) {}
        fn reset(&mut self, _new_bounds: Rectangle) {}
        fn allocate_image(
            &mut self,
            _handle: &Handle,
            callback: impl FnOnce(Result<Allocation, Error>) + Send + 'static,
        ) {
            callback(Err(Error::Unsupported));
        }
    }

    impl advanced::image::Renderer for Recorder {
        type Handle = Handle;
        fn load_image(&self, _handle: &Handle) -> Result<Allocation, Error> {
            Err(Error::Unsupported)
        }
        fn measure_image(&self, _handle: &Handle) -> Option<Size<u32>> {
            None
        }
        fn draw_image(&mut self, image: Image<Handle>, bounds: Rectangle, clip: Rectangle) {
            self.images.push(Drawn {
                bounds,
                clip,
                filter: image.filter_method,
                handle: image.handle.id(),
            });
        }
    }

    const VIEWPORT: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 2000.0,
        height: 2000.0,
    };

    const GRID: ObjectMetrics = ObjectMetrics {
        cell_width: 9.0,
        line_height: 18.0,
    };

    fn geometry(columns: u16, rows: u16, source: SourceRect, fit: Fit) -> PlacementGeometry {
        PlacementGeometry {
            columns,
            rows,
            source,
            x_offset: 0,
            y_offset: 0,
            fit,
            z: 0,
        }
    }

    fn whole(width: u32, height: u32) -> SourceRect {
        SourceRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// A pending `width` x `height` image placed with `geometry`.
    fn placed(width: u32, height: u32, geometry: PlacementGeometry) -> Arc<Placement> {
        let record = Arc::new(ImageRecord::new(1, 0, width, height));
        Arc::new(Placement::new(record, 0, geometry))
    }

    fn ready(placement: &Placement) {
        let (width, height) = (placement.image.width, placement.image.height);
        placement.image.set_data(ImageData::Ready(DecodedPixels {
            width,
            height,
            rgba: vec![0x80; (width * height * 4) as usize].into(),
        }));
    }

    /// One mounted placement, laid out at `origin`, drawable repeatedly like a pane does.
    struct Mounted {
        widget: KittyPlacement,
        tree: Tree,
        node: layout::Node,
    }

    impl Mounted {
        fn new(widget: KittyPlacement, origin: Point) -> Self {
            let mut tree = Tree::new(&widget as &dyn Widget<(), (), Recorder>);
            let mut widget = widget;
            let node = Widget::<(), (), Recorder>::layout(
                &mut widget,
                &mut tree,
                &Recorder::default(),
                &layout::Limits::new(Size::ZERO, Size::new(1.0, 1.0)),
            )
            .move_to(origin);
            Self { widget, tree, node }
        }

        fn draw(&self) -> Recorder {
            self.draw_in(VIEWPORT)
        }

        /// Lay the mount out again at `origin`, as a relayout of the pane between a
        /// redraw event and the draw after it does.
        fn move_to(&mut self, origin: Point) {
            self.node = self.node.clone().move_to(origin);
        }

        /// The redraw a redraw event at `now` makes the widget ask for; the draws that
        /// follow show the frames as of `now`.
        fn redraw_at(&mut self, now: Instant) -> RedrawRequest {
            self.redraw_in(now, VIEWPORT)
        }

        fn redraw_in(&mut self, now: Instant, viewport: Rectangle) -> RedrawRequest {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            self.update_in(&mut shell, now, viewport);
            shell.redraw_request()
        }

        /// Deliver a redraw event at `now` through `shell`, as the pane's host delivers
        /// it to every mount, anchor or not: the window event, no cursor, its clip.
        fn update_in(&mut self, shell: &mut Shell<'_, ()>, now: Instant, viewport: Rectangle) {
            Widget::<(), (), Recorder>::update(
                &mut self.widget,
                &mut self.tree,
                &Event::Window(window::Event::RedrawRequested(now)),
                Layout::new(&self.node),
                mouse::Cursor::Unavailable,
                &Recorder::default(),
                &mut iced::advanced::clipboard::Null,
                shell,
                &viewport,
            );
        }

        fn draw_in(&self, viewport: Rectangle) -> Recorder {
            let mut renderer = Recorder::default();
            Widget::<(), (), Recorder>::draw(
                &self.widget,
                &self.tree,
                &mut renderer,
                &(),
                &renderer::Style::default(),
                Layout::new(&self.node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            renderer
        }
    }

    /// A relative placement of `image`, `columns` cells right of and `rows` down from
    /// `parent`'s top-left cell, made `parent`'s newest child.
    fn relative_of(
        parent: &Placement,
        image: Arc<ImageRecord>,
        columns: i32,
        rows: i32,
        geometry: PlacementGeometry,
    ) -> Arc<Placement> {
        let mut child = Placement::new(image, 0, geometry);
        child.anchored = true;
        child.relative = Some(RelativeOffset { columns, rows });
        let child = Arc::new(child);
        parent.children.write().unwrap().push(child.clone());
        child
    }

    /// The same, of a fresh `width` x `height` image whose pixels have landed, its whole
    /// source laid out by `geometry`.
    fn relative(
        parent: &Placement,
        columns: i32,
        rows: i32,
        (width, height): (u32, u32),
        geometry: PlacementGeometry,
    ) -> Arc<Placement> {
        let image = Arc::new(ImageRecord::new(2, 0, width, height));
        let child = relative_of(parent, image, columns, rows, geometry);
        ready(&child);
        child
    }

    /// The image draws' boxes and clips, in order.
    fn boxes(drawn: &Recorder) -> Vec<(Rectangle, Rectangle)> {
        drawn
            .images
            .iter()
            .map(|image| (image.bounds, image.clip))
            .collect()
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), Size::new(width, height))
    }

    /// How many relative placements' images the last draw memoised.
    fn memo(mounted: &Mounted) -> usize {
        mounted
            .tree
            .state
            .downcast_ref::<State>()
            .relatives
            .borrow()
            .len()
    }

    #[test]
    fn layout_is_columns_by_rows_in_the_panes_grid_whatever_the_limits() {
        let placement = placed(30, 40, geometry(3, 2, whole(30, 40), Fit::Natural));
        let metrics = ObjectMetrics {
            cell_width: 8.4,
            line_height: 17.0,
        };
        let mounted = Mounted::new(KittyPlacement::new(placement, metrics, None), Point::ORIGIN);
        assert_eq!(mounted.node.size(), Size::new(3.0 * 8.4, 34.0));
        assert_eq!(
            Widget::<(), (), Recorder>::size(&mounted.widget),
            Size::new(Length::Fixed(3.0 * 8.4), Length::Fixed(34.0))
        );
    }

    fn zeroed(width: u32, height: u32) -> bytes::Bytes {
        vec![0; (width * height * 4) as usize].into()
    }

    #[test]
    fn an_evicted_image_takes_its_store_entry_with_it() {
        let (store, _) = test_store();
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&placement);
        let key = format!("kitty:{}", placement.image.uid);
        let mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(mounted.draw().images.len(), 1);
        assert!(
            placement.image.has_ui_slot(),
            "the first draw ties the entry"
        );
        assert_eq!(store.ready_bytes(), 32);
        // Another placement of the same image draws the shared entry without a
        // token of its own, and without disturbing the first.
        let other = Arc::new(Placement::new(
            placement.image.clone(),
            0,
            placement.geometry,
        ));
        let again = Mounted::new(
            KittyPlacement::new(other, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(again.draw().images.len(), 1);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(store.peek(&key).is_some() && placement.image.has_ui_slot());
        // The session evicts the image: the entry goes, on the store's runtime.
        placement.image.set_data(ImageData::Evicted);
        assert!(!placement.image.has_ui_slot());
        wait_until("the entry's removal", || store.peek(&key).is_none());
        wait_until("the bytes released", || store.ready_bytes() == 0);
        assert!(mounted.draw().images.is_empty() && again.draw().images.is_empty());
    }

    #[test]
    fn a_record_dropped_with_its_placements_takes_its_store_entry_with_it() {
        let (store, _) = test_store();
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&placement);
        let key = format!("kitty:{}", placement.image.uid);
        let mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(mounted.draw().images.len(), 1);
        drop(mounted);
        drop(placement);
        wait_until("the entry's removal", || store.peek(&key).is_none());
        wait_until("the bytes released", || store.ready_bytes() == 0);
    }

    #[test]
    fn an_image_evicted_while_its_frame_was_ensured_loses_the_entry_with_the_slot() {
        // Between taking the slot and tying the entry to it, the socket thread
        // evicted the image: the record let go of the slot, the draw's hold on it is
        // the last, and the entry goes when the draw lets go of it.
        let (store, _) = test_store();
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&placement);
        let image = &placement.image;
        let frame = image.frame(1).unwrap();
        let slot = image_slot(&store, image).expect("a slot for a live record");
        assert!(image.has_ui_slot());
        image.set_data(ImageData::Evicted);
        assert!(!image.has_ui_slot());
        let key = frame_key(image.uid, 1, frame.id);
        let cell = store.ensure_rgba(&key, 4, 2, frame.pixels.rgba.clone());
        slot.hold(&store, None, &key, &cell, None, image);
        drop(slot);
        wait_until("the entry's removal", || store.peek(&key).is_none());
        wait_until("the cell's eviction", || cell.is_evicted());
        wait_until("the bytes released", || store.ready_bytes() == 0);
        // Once evicted, the record takes no slot and nothing is ensured for it.
        assert!(image_slot(&store, image).is_none());
        assert!(ensure_cell(&store, image, 1, &frame, None).is_none());
        assert!(store.peek(&key).is_none());
    }

    #[test]
    fn an_operation_finds_the_placement_and_its_box() {
        struct Found(Vec<(ImageUid, Rectangle)>);
        impl Operation for Found {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(
                &mut self,
                _id: Option<&iced::widget::Id>,
                bounds: Rectangle,
                state: &mut dyn std::any::Any,
            ) {
                if let Some(placement) = state.downcast_ref::<Arc<Placement>>() {
                    self.0.push((placement.image.uid, bounds));
                }
            }
        }
        let placement = placed(30, 40, geometry(3, 2, whole(30, 40), Fit::Natural));
        let uid = placement.image.uid;
        let origin = Point::new(5.0, 7.0);
        let mut mounted = Mounted::new(KittyPlacement::new(placement, GRID, None), origin);
        let mut found = Found(Vec::new());
        Widget::<(), (), Recorder>::operate(
            &mut mounted.widget,
            &mut mounted.tree,
            Layout::new(&mounted.node),
            &Recorder::default(),
            &mut found,
        );
        assert_eq!(
            found.0,
            vec![(uid, Rectangle::new(origin, Size::new(27.0, 36.0)))]
        );
    }

    #[test]
    fn a_box_laid_out_from_the_real_advance_is_exactly_the_image_wide() {
        // The processor counts columns from the fixed-point cell the pane
        // reported, so a 96 px image in 9.6 px cells is ten columns, and the
        // box the widget takes is the image's own width: no blank strip
        // before the text after it.
        use smudgy_protocol::graphics::{ControlData, cell_fixed, geometry as lay_out};
        let metrics = ObjectMetrics {
            cell_width: 9.6,
            line_height: 18.0,
        };
        let cell = (
            cell_fixed(metrics.cell_width),
            cell_fixed(metrics.line_height),
        );
        let geometry = lay_out(&ControlData::default(), 96, 36, cell);
        assert_eq!((geometry.columns, geometry.rows), (10, 2));
        let placement = placed(96, 36, geometry);
        let mounted = Mounted::new(KittyPlacement::new(placement, metrics, None), Point::ORIGIN);
        let size = mounted.node.size();
        assert!(
            (size.width - 96.0).abs() < 1e-4 && (size.height - 36.0).abs() < 1e-4,
            "{size:?}"
        );
    }

    #[test]
    fn draws_nothing_until_the_pixels_land_then_shares_one_handle() {
        let (store, fetcher) = test_store();
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        let origin = Point::new(10.0, 20.0);
        let mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            origin,
        );
        assert!(mounted.draw().images.is_empty(), "pending draws nothing");
        assert_eq!(store.ready_bytes(), 0, "nothing ensured while pending");

        ready(&placement);
        let drawn = mounted.draw();
        let key = format!("kitty:{}", placement.image.uid);
        let cell = store.peek(&key).expect("ensured under the image's key");
        let EntryState::Ready { handle, .. } = &*cell.state() else {
            panic!("a Ready cell");
        };
        assert_eq!(
            drawn.images,
            vec![Drawn {
                bounds: Rectangle::new(origin, Size::new(4.0, 2.0)),
                clip: Rectangle::new(origin, Size::new(4.0, 2.0)),
                filter: FilterMethod::Linear,
                handle: handle.id(),
            }],
            "natural size, top-left of the box"
        );
        assert!(drawn.layers.is_empty(), "nothing to crop, no layer");

        // A second placement of the same image draws the same handle.
        let other = Arc::new(Placement::new(
            placement.image.clone(),
            0,
            placement.geometry,
        ));
        let again = Mounted::new(
            KittyPlacement::new(other, GRID, Some(store.clone())),
            origin,
        );
        assert_eq!(again.draw().images[0].handle, handle.id());
        assert_eq!(store.ready_bytes(), 32, "one entry for both");
        assert_eq!(fetcher.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn draws_nothing_after_a_failed_decode() {
        let (store, _) = test_store();
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        placement
            .image
            .set_data(ImageData::Failed(GraphicsError::invalid("bad")));
        let mounted = Mounted::new(
            KittyPlacement::new(placement, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert!(mounted.draw().images.is_empty());
        assert_eq!(store.ready_bytes(), 0);
    }

    #[test]
    fn draws_nothing_once_deleted_or_evicted_but_keeps_its_box() {
        let (store, _) = test_store();
        for evict in [false, true] {
            let placement = placed(4, 2, geometry(2, 1, whole(4, 2), Fit::Natural));
            ready(&placement);
            let mounted = Mounted::new(
                KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
                Point::ORIGIN,
            );
            assert_eq!(mounted.draw().images.len(), 1, "drawn while live");
            if evict {
                placement.image.set_data(ImageData::Evicted);
            } else {
                placement.mark_deleted();
            }
            assert!(mounted.draw().images.is_empty(), "evicted: {evict}");
            assert_eq!(mounted.node.size(), Size::new(18.0, 18.0), "no reflow");
        }
    }

    #[test]
    fn draws_nothing_without_an_image_store() {
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&placement);
        let mounted = Mounted::new(KittyPlacement::new(placement, GRID, None), Point::ORIGIN);
        assert!(mounted.draw().images.is_empty());
    }

    #[test]
    fn ensures_again_after_the_store_evicts_its_cell() {
        let (store, _) = test_store();
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&placement);
        let mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let first = mounted.draw().images[0].handle;
        let key = format!("kitty:{}", placement.image.uid);
        let evicted = store.peek(&key).unwrap();
        store.clear();
        assert!(evicted.is_evicted());
        let second = mounted.draw().images[0].handle;
        let fresh = store.peek(&key).expect("ensured again");
        assert!(!Arc::ptr_eq(&evicted, &fresh));
        assert_ne!(first, second, "a fresh handle from the same pixels");
        assert_eq!(store.ready_bytes(), 32);
    }

    #[test]
    fn offsets_shift_the_image_inside_its_first_cell() {
        let (store, _) = test_store();
        let mut geometry = geometry(1, 1, whole(4, 2), Fit::Natural);
        geometry.x_offset = 2;
        geometry.y_offset = 3;
        let placement = placed(4, 2, geometry);
        ready(&placement);
        let origin = Point::new(100.0, 50.0);
        let mounted = Mounted::new(KittyPlacement::new(placement, GRID, Some(store)), origin);
        let drawn = mounted.draw();
        let at = Rectangle::new(Point::new(102.0, 53.0), Size::new(4.0, 2.0));
        assert_eq!((drawn.images[0].bounds, drawn.images[0].clip), (at, at));
    }

    #[test]
    fn a_source_rectangle_crops_through_a_layer() {
        let (store, _) = test_store();
        // Columns 1..3 of a 4x2 image.
        let source = SourceRect {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let placement = placed(4, 2, geometry(1, 1, source, Fit::Natural));
        ready(&placement);
        let origin = Point::new(10.0, 10.0);
        let mounted = Mounted::new(KittyPlacement::new(placement, GRID, Some(store)), origin);
        let drawn = mounted.draw();
        let visible = Rectangle::new(origin, Size::new(2.0, 2.0));
        assert_eq!(
            drawn.images[0].bounds,
            Rectangle::new(Point::new(9.0, 10.0), Size::new(4.0, 2.0)),
            "the whole image, shifted so the source starts at the box"
        );
        assert_eq!(drawn.images[0].clip, visible);
        assert_eq!(drawn.layers, vec![visible], "clipped by a layer");
    }

    #[test]
    fn a_box_fit_contains_the_image_top_left() {
        let (store, _) = test_store();
        // A 4x2 image in 2x2 cells (18x36): width-bound, scaled 4.5x to 18x9.
        let placement = placed(
            4,
            2,
            geometry(
                2,
                2,
                whole(4, 2),
                Fit::Box {
                    columns: 2,
                    rows: 2,
                },
            ),
        );
        ready(&placement);
        let mounted = Mounted::new(
            KittyPlacement::new(placement, GRID, Some(store)),
            Point::ORIGIN,
        );
        let drawn = mounted.draw();
        let at = Rectangle::new(Point::ORIGIN, Size::new(18.0, 9.0));
        assert_eq!((drawn.images[0].bounds, drawn.images[0].clip), (at, at));
    }

    #[test]
    fn a_natural_image_larger_than_its_box_is_clipped_to_it() {
        // The processor laid the box out with another cell size (the fallback, before the
        // pane reported one): the image overflows a smaller box and is cut at its edge.
        let geometry = geometry(1, 1, whole(30, 40), Fit::Natural);
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(9.0, 18.0));
        let target = target(&geometry, (30, 40), bounds).unwrap();
        assert_eq!(
            target.image,
            Rectangle::new(Point::ORIGIN, Size::new(30.0, 40.0))
        );
        assert_eq!(target.visible, bounds);
    }

    #[test]
    fn an_empty_box_or_source_shows_nothing() {
        let empty_box = Rectangle::new(Point::ORIGIN, Size::new(0.0, 0.0));
        let fit = Fit::Box {
            columns: 0,
            rows: 0,
        };
        assert_eq!(
            target(&geometry(0, 0, whole(4, 2), fit), (4, 2), empty_box),
            None
        );
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(9.0, 18.0));
        assert_eq!(
            target(&geometry(1, 1, whole(0, 2), Fit::Natural), (4, 2), bounds),
            None
        );
    }

    #[test]
    fn placements_resolve_only_with_a_placement_resource() {
        use smudgy_session_model::inline_content::{InlineObject, InlineOwner};
        let (store, _) = test_store();
        let placement = placed(4, 2, geometry(2, 1, whole(4, 2), Fit::Natural));
        let object = InlineObject::new(0..2, placement, InlineOwner::default());
        let element = with_image_store_context(&store, || {
            placement_element::<(), (), Recorder>(&object, GRID)
        });
        assert!(element.is_some());
        let other = InlineObject::new(0..2, Arc::new(()), InlineOwner::default());
        assert!(placement_element::<(), (), Recorder>(&other, GRID).is_none());
        assert!(
            active_store().is_none(),
            "the context ends with its closure"
        );
    }

    #[test]
    fn relative_placements_draw_from_the_parents_cell_in_its_grid_with_their_own_geometry() {
        let (store, _) = test_store();
        let parent = placed(4, 2, geometry(2, 1, whole(4, 2), Fit::Natural));
        ready(&parent);
        // Three cells right and a row down, shifted 2 px and 1 px into its cell.
        let mut shifted = geometry(1, 1, whole(6, 6), Fit::Natural);
        shifted.x_offset = 2;
        shifted.y_offset = 1;
        let child = relative(&parent, 3, 1, (6, 6), shifted);
        // Relative to the child: two cells left and a row up (back on the parent's row),
        // its 8x4 image contained in a 2x1 box.
        let fit = Fit::Box {
            columns: 2,
            rows: 1,
        };
        relative(&child, -2, -1, (8, 4), geometry(2, 1, whole(8, 4), fit));
        let mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store)),
            Point::new(10.0, 20.0),
        );
        let drawn = mounted.draw();
        assert_eq!(
            boxes(&drawn),
            vec![
                (rect(10.0, 20.0, 4.0, 2.0), rect(10.0, 20.0, 4.0, 2.0)),
                // Cell (3, 1) of 9x18 cells from (10, 20) is (37, 38); the offsets add.
                (rect(39.0, 39.0, 6.0, 6.0), rect(39.0, 39.0, 6.0, 6.0)),
                // Cell (-2, -1) from the child's is (19, 20); 8x4 scaled 2.25x to 18x9.
                (rect(19.0, 20.0, 18.0, 9.0), rect(19.0, 20.0, 18.0, 9.0)),
            ],
            "the parent, then its child, then the child's own"
        );
        assert!(drawn.layers.is_empty(), "nothing cropped, no layer");
        let handles: std::collections::HashSet<_> =
            drawn.images.iter().map(|image| image.handle).collect();
        assert_eq!(handles.len(), 3, "each its own image");
    }

    #[test]
    fn a_layout_moved_between_the_redraw_event_and_the_draw_moves_the_whole_family() {
        let (store, _) = test_store();
        let parent = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&parent);
        relative(
            &parent,
            2,
            1,
            (4, 2),
            geometry(1, 1, whole(4, 2), Fit::Natural),
        );
        let mut mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store)),
            Point::new(10.0, 20.0),
        );
        mounted.redraw_at(Instant::now());
        assert_eq!(
            boxes(&mounted.draw()),
            vec![
                (rect(10.0, 20.0, 4.0, 2.0), rect(10.0, 20.0, 4.0, 2.0)),
                (rect(28.0, 38.0, 4.0, 2.0), rect(28.0, 38.0, 4.0, 2.0)),
            ]
        );
        // The pane laid out again before drawing (a drag's autoscroll moved the row up
        // a line): the family goes where the layout the draw is handed says, not where
        // the redraw event saw it.
        mounted.move_to(Point::new(10.0, 2.0));
        assert_eq!(
            boxes(&mounted.draw()),
            vec![
                (rect(10.0, 2.0, 4.0, 2.0), rect(10.0, 2.0, 4.0, 2.0)),
                (rect(28.0, 20.0, 4.0, 2.0), rect(28.0, 20.0, 4.0, 2.0)),
            ],
            "parent and child alike"
        );
        // Culling follows too: with the mount moved out of view, the event steps nothing
        // and the draw draws nothing.
        mounted.move_to(Point::new(3000.0, 3000.0));
        assert_eq!(mounted.redraw_at(Instant::now()), RedrawRequest::Wait);
        assert!(mounted.draw().images.is_empty());
    }

    #[test]
    fn a_placement_drawn_into_a_full_store_sweeps_no_fetched_entry_that_fit() {
        // Two fetched 49 MiB entries sit under the store's 128 MiB cap; a 49 MiB
        // placement drawn beside them is the session's to bound, and its entry is held
        // as it is inserted, so the sweep its insert would otherwise run takes neither.
        const SIDE: u32 = 3584;
        let bytes = u64::from(SIDE) * u64::from(SIDE) * 4;
        let (store, _) = test_store();
        let first = store.ensure_rgba("fetched:1", SIDE, SIDE, zeroed(SIDE, SIDE));
        std::thread::sleep(ms(3));
        let second = store.ensure_rgba("fetched:2", SIDE, SIDE, zeroed(SIDE, SIDE));
        std::thread::sleep(ms(3));
        let placement = placed(SIDE, SIDE, geometry(1, 1, whole(SIDE, SIDE), Fit::Natural));
        ready(&placement);
        let mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(mounted.draw().images.len(), 1);
        assert!(!first.is_evicted() && !second.is_evicted(), "nothing swept");
        assert_eq!(
            (store.ready_bytes(), store.held_bytes()),
            (3 * bytes, bytes)
        );
        // The hold the insert took is the slot's: the image's eviction releases it.
        placement.image.set_data(ImageData::Evicted);
        wait_until("the bytes released", || store.ready_bytes() == 2 * bytes);
        assert_eq!(store.held_bytes(), 0);
    }

    #[test]
    fn a_relative_placement_is_clipped_to_its_own_box_and_stays_above_a_cropped_one() {
        let (store, _) = test_store();
        // Columns 1..3 of the parent's 4x2 image: cropped through a layer.
        let source = SourceRect {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let parent = placed(4, 2, geometry(1, 1, source, Fit::Natural));
        ready(&parent);
        // A 4x2 child fits its cell, but is drawn after a layered image: a layer too, or
        // the parent's layer would paint over it. A 30x40 one overflows its 9x18 box and is
        // cut at the box, not the parent's.
        relative(
            &parent,
            0,
            0,
            (4, 2),
            geometry(1, 1, whole(4, 2), Fit::Natural),
        );
        relative(
            &parent,
            1,
            0,
            (30, 40),
            geometry(1, 1, whole(30, 40), Fit::Natural),
        );
        let mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store)),
            Point::ORIGIN,
        );
        let drawn = mounted.draw();
        assert_eq!(
            boxes(&drawn),
            vec![
                (rect(-1.0, 0.0, 4.0, 2.0), rect(0.0, 0.0, 2.0, 2.0)),
                (rect(0.0, 0.0, 4.0, 2.0), rect(0.0, 0.0, 4.0, 2.0)),
                (rect(9.0, 0.0, 30.0, 40.0), rect(9.0, 0.0, 9.0, 18.0)),
            ]
        );
        assert_eq!(
            drawn.layers,
            vec![
                rect(0.0, 0.0, 2.0, 2.0),
                rect(0.0, 0.0, 4.0, 2.0),
                rect(9.0, 0.0, 9.0, 18.0)
            ],
            "every image after the first layered one takes a layer"
        );
    }

    #[test]
    fn relative_placements_are_culled_against_the_viewport_not_their_parent() {
        let (store, _) = test_store();
        let viewport = rect(0.0, 0.0, 100.0, 100.0);
        // The parent's box is above the viewport.
        let parent = placed(9, 18, geometry(1, 1, whole(9, 18), Fit::Natural));
        ready(&parent);
        let cell = || geometry(1, 1, whole(9, 18), Fit::Natural);
        // Five rows down: in view.
        relative(&parent, 0, 5, (9, 18), cell());
        // Twenty columns right: out of view, but its own child, fifteen back, is in it.
        let away = relative(&parent, 20, 0, (9, 18), cell());
        relative(&away, -15, 4, (9, 18), cell());
        // Half in view at the bottom edge: cut at the viewport.
        relative(&parent, 0, 7, (9, 18), cell());
        let mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store)),
            Point::new(0.0, -36.0),
        );
        let drawn = mounted.draw_in(viewport);
        assert_eq!(
            boxes(&drawn),
            vec![
                (rect(0.0, 54.0, 9.0, 18.0), rect(0.0, 54.0, 9.0, 18.0)),
                (rect(45.0, 36.0, 9.0, 18.0), rect(45.0, 36.0, 9.0, 18.0)),
                (rect(0.0, 90.0, 9.0, 18.0), rect(0.0, 90.0, 9.0, 10.0)),
            ]
        );
        assert_eq!(drawn.layers, vec![rect(0.0, 90.0, 9.0, 10.0)]);
    }

    #[test]
    fn deleted_relative_placements_draw_nothing_and_neither_do_their_own() {
        let (store, _) = test_store();
        let parent = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&parent);
        let cell = || geometry(1, 1, whole(4, 2), Fit::Natural);
        let doomed = relative(&parent, 1, 0, (4, 2), cell());
        let grandchild = relative(&doomed, 1, 0, (4, 2), cell());
        relative(&parent, 3, 0, (4, 2), cell());
        let mounted = Mounted::new(
            KittyPlacement::new(parent.clone(), GRID, Some(store)),
            Point::ORIGIN,
        );
        assert_eq!(mounted.draw().images.len(), 4);
        doomed.mark_deleted();
        assert_eq!(
            boxes(&mounted.draw())
                .iter()
                .map(|(bounds, _)| bounds.x)
                .collect::<Vec<_>>(),
            vec![0.0, 27.0],
            "the parent and the sibling"
        );
        assert!(grandchild.is_deleted(), "deletes cascade");
        assert!(
            doomed.child_placements().is_empty(),
            "a deleted placement sheds its children"
        );
        // One added under a deleted placement afterwards is not flagged, and still has
        // nothing to hang from.
        relative(&doomed, 2, 0, (4, 2), cell());
        assert_eq!(mounted.draw().images.len(), 2);
        parent.mark_deleted();
        relative(&parent, 5, 0, (4, 2), cell());
        assert!(mounted.draw().images.is_empty());
    }

    #[test]
    fn relative_placements_draw_down_to_the_depth_limit() {
        let (store, _) = test_store();
        let root = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&root);
        let mut parent = root.clone();
        for _ in 0..PLACEMENT_DEPTH_LIMIT + 2 {
            parent = relative(
                &parent,
                1,
                0,
                (4, 2),
                geometry(1, 1, whole(4, 2), Fit::Natural),
            );
        }
        let mounted = Mounted::new(KittyPlacement::new(root, GRID, Some(store)), Point::ORIGIN);
        let drawn = mounted.draw();
        assert_eq!(drawn.images.len(), 1 + PLACEMENT_DEPTH_LIMIT);
        let deepest = f32::from(u16::try_from(PLACEMENT_DEPTH_LIMIT).unwrap()) * GRID.cell_width;
        assert!((drawn.images.last().unwrap().bounds.x - deepest).abs() < 1e-4);
    }

    #[test]
    fn relative_placements_share_one_entry_per_image_and_forget_what_they_stop_drawing() {
        let (store, _) = test_store();
        let parent = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&parent);
        let cell = || geometry(1, 1, whole(4, 2), Fit::Natural);
        let shared = relative(&parent, 1, 0, (4, 2), cell());
        let twin = relative_of(&parent, shared.image.clone(), 2, 0, cell());
        let other = relative(&parent, 3, 0, (4, 2), cell());
        let mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let drawn = mounted.draw();
        assert_eq!(drawn.images.len(), 4);
        assert_eq!(drawn.images[1].handle, drawn.images[2].handle, "one handle");
        assert_eq!(memo(&mounted), 2, "one memo per image");
        assert_eq!(store.ready_bytes(), 3 * 32, "three images, three entries");
        other.mark_deleted();
        mounted.draw();
        assert_eq!(memo(&mounted), 1);
        shared.mark_deleted();
        twin.mark_deleted();
        assert_eq!(mounted.draw().images.len(), 1);
        assert_eq!(memo(&mounted), 0, "nothing held for what no longer draws");
    }

    #[test]
    fn an_operation_finds_relative_placements_and_their_boxes() {
        struct Found(Vec<(ImageUid, Rectangle)>);
        impl Operation for Found {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn custom(
                &mut self,
                _id: Option<&iced::widget::Id>,
                bounds: Rectangle,
                state: &mut dyn std::any::Any,
            ) {
                if let Some(placement) = state.downcast_ref::<Arc<Placement>>() {
                    self.0.push((placement.image.uid, bounds));
                }
            }
        }
        let parent = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        let child = relative(
            &parent,
            1,
            2,
            (4, 2),
            geometry(2, 1, whole(4, 2), Fit::Natural),
        );
        relative(
            &parent,
            4,
            0,
            (4, 2),
            geometry(1, 1, whole(4, 2), Fit::Natural),
        )
        .mark_deleted();
        let origin = Point::new(5.0, 7.0);
        let expected = vec![
            (parent.image.uid, rect(5.0, 7.0, 9.0, 18.0)),
            (child.image.uid, rect(14.0, 43.0, 18.0, 18.0)),
        ];
        let mut mounted = Mounted::new(KittyPlacement::new(parent, GRID, None), origin);
        let mut found = Found(Vec::new());
        Widget::<(), (), Recorder>::operate(
            &mut mounted.widget,
            &mut mounted.tree,
            Layout::new(&mounted.node),
            &Recorder::default(),
            &mut found,
        );
        assert_eq!(found.0, expected, "the deleted one is not reported");
    }

    #[test]
    fn relative_placements_ensure_again_after_the_store_evicts_their_cells() {
        let (store, _) = test_store();
        let parent = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&parent);
        let child = relative(
            &parent,
            1,
            0,
            (4, 2),
            geometry(1, 1, whole(4, 2), Fit::Natural),
        );
        let mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let first = mounted.draw();
        assert_eq!(first.images.len(), 2);
        let key = format!("kitty:{}", child.image.uid);
        let evicted = store.peek(&key).unwrap();
        store.clear();
        assert!(evicted.is_evicted());
        let second = mounted.draw();
        assert_eq!(second.images.len(), 2, "both ensured again");
        assert_ne!(
            first.images[1].handle, second.images[1].handle,
            "a fresh handle from the child's pixels"
        );
        let fresh = store
            .peek(&key)
            .expect("ensured again under the child's key");
        assert!(!Arc::ptr_eq(&evicted, &fresh));
        assert_eq!(store.ready_bytes(), 2 * 32);
        assert_eq!(memo(&mounted), 1);
    }

    #[test]
    fn a_relative_placement_whose_image_is_evicted_while_memoised_stops_drawing() {
        let (store, _) = test_store();
        let parent = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&parent);
        let child = relative(
            &parent,
            1,
            0,
            (4, 2),
            geometry(1, 1, whole(4, 2), Fit::Natural),
        );
        let mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(mounted.draw().images.len(), 2);
        assert_eq!(memo(&mounted), 1);
        // The session evicts the child's image while its cell is memoised: the next
        // draw finds the record evicted, draws the parent alone and forgets the cell.
        child.image.set_data(ImageData::Evicted);
        assert_eq!(mounted.draw().images.len(), 1, "the parent alone");
        assert_eq!(memo(&mounted), 0, "nothing held for an evicted image");
        let key = format!("kitty:{}", child.image.uid);
        wait_until("the entry's removal", || store.peek(&key).is_none());
        wait_until("the parent's entry alone", || store.ready_bytes() == 32);
    }

    /// The family draws in the protocol's stacking order whatever the tree: ascending
    /// z-index, then image id, then creation order. A child below its parent's z-index
    /// or id paints before the parent.
    #[test]
    fn a_family_draws_in_ascending_z_then_image_id_then_creation_order() {
        let (store, _) = test_store();
        let cell = |z: i32| {
            let mut geometry = geometry(1, 1, whole(4, 2), Fit::Natural);
            geometry.z = z;
            geometry
        };
        let parent = Arc::new(Placement::new(
            Arc::new(ImageRecord::new(5, 0, 4, 2)),
            0,
            cell(0),
        ));
        ready(&parent);
        let child = |image: u32, column: i32, z: i32| {
            let child = relative_of(
                &parent,
                Arc::new(ImageRecord::new(image, 0, 4, 2)),
                column,
                0,
                cell(z),
            );
            ready(&child);
            child
        };
        // Created in this order, each a column further right than the last.
        child(9, 1, -1); // z below the parent's: first, whatever its id
        child(3, 2, 0); // equal z, a lower id: before the parent
        let third = child(7, 3, 0); // equal z, a higher id: after it
        child(1, 4, 1); // z above: last, whatever its id
        // A grandchild of equal z and id to its parent: creation order.
        let grandchild = relative_of(
            &third,
            Arc::new(ImageRecord::new(7, 0, 4, 2)),
            2,
            0,
            cell(0),
        );
        ready(&grandchild);
        let mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store)),
            Point::ORIGIN,
        );
        let columns: Vec<f32> = boxes(&mounted.draw())
            .iter()
            .map(|(bounds, _)| bounds.x / GRID.cell_width)
            .collect();
        assert_eq!(columns, vec![1.0, 2.0, 0.0, 3.0, 5.0, 4.0]);
    }

    // Animation: the timeline, then the widget drawing and scheduling it.

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// Frames with these gaps, the root first, for timeline tests: only the gaps count.
    fn frames(gaps: &[u32]) -> Vec<Frame> {
        gaps.iter()
            .zip(1..)
            .map(|(&gap_ms, id)| Frame {
                id: FrameId(id),
                pixels: DecodedPixels {
                    width: 1,
                    height: 1,
                    rgba: vec![0; 4].into(),
                },
                gap_ms,
            })
            .collect()
    }

    fn playing(state: AnimationState, loops: u32) -> Animation {
        Animation {
            state,
            loops,
            ..Animation::default()
        }
    }

    /// Step `timeline` to `millis` after `t0`: the frame showing and when the next is due,
    /// after `t0`.
    fn step(
        timeline: &mut Timeline,
        animation: &Animation,
        frames: &[Frame],
        t0: Instant,
        millis: u64,
    ) -> (usize, Option<Duration>) {
        let due = timeline.advance(animation, frames, t0 + ms(millis));
        (timeline.position, due.map(|due| due - t0))
    }

    #[test]
    fn a_running_timeline_steps_by_each_frames_gap_and_wraps() {
        let t0 = Instant::now();
        let frames = frames(&[100, 50, 200]);
        let animation = playing(AnimationState::Running, 0);
        let mut timeline = Timeline::new(&animation, 3, t0);
        let mut at = |millis| step(&mut timeline, &animation, &frames, t0, millis);
        assert_eq!(at(0), (1, Some(ms(100))));
        assert_eq!(at(99), (1, Some(ms(100))));
        assert_eq!(at(100), (2, Some(ms(150))));
        assert_eq!(at(149), (2, Some(ms(150))));
        assert_eq!(at(150), (3, Some(ms(350))));
        assert_eq!(at(350), (1, Some(ms(450))), "wraps to the root");
        // Drawn late, the frames keep their schedule: frame 1 ended at 450, so 460 is
        // frame 2's; at 720 frame 3 (500 to 700) is over too, and frame 1 shows.
        assert_eq!(at(460), (2, Some(ms(500))));
        assert_eq!(at(720), (1, Some(ms(800))));
        assert_eq!(timeline.passes, 2);
    }

    #[test]
    fn gapless_frames_are_skipped_and_a_root_without_a_gap_never_shows() {
        // The root's gap is zero until `a=a,r=1,z=` sets one: a running animation
        // starting there shows frame 2 at once, and skips frame 3 the same way.
        let t0 = Instant::now();
        let frames = frames(&[0, 40, 0, 40]);
        let animation = playing(AnimationState::Running, 0);
        let mut timeline = Timeline::new(&animation, 4, t0);
        let mut at = |millis| step(&mut timeline, &animation, &frames, t0, millis);
        assert_eq!(at(0), (2, Some(ms(40))));
        assert_eq!(at(40), (4, Some(ms(80))));
        assert_eq!(at(80), (2, Some(ms(120))), "past the gapless root");
    }

    #[test]
    fn loops_play_their_passes_then_hold_the_last_frame() {
        // `v=3`: two passes.
        let t0 = Instant::now();
        let frames = frames(&[10, 10, 10]);
        let mut animation = playing(AnimationState::Running, 2);
        let mut timeline = Timeline::new(&animation, 3, t0);
        for (millis, expected) in [
            (0, (1, Some(ms(10)))),
            (20, (3, Some(ms(30)))),
            (30, (1, Some(ms(40)))),
            (50, (3, Some(ms(60)))),
            (60, (3, None)),
            (10_000, (3, None)),
        ] {
            assert_eq!(
                step(&mut timeline, &animation, &frames, t0, millis),
                expected,
                "at {millis} ms"
            );
        }
        assert!(timeline.held);
        // The loops are read live: playing forever resumes from the frame held, the
        // next frame showing from now for its whole gap.
        animation.loops = 0;
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 20_000),
            (1, Some(ms(20_010)))
        );
    }

    #[test]
    fn a_loading_timeline_holds_its_last_frame_until_more_arrive() {
        let t0 = Instant::now();
        let mut frames = frames(&[20, 20]);
        let mut animation = playing(AnimationState::Loading, 0);
        let mut timeline = Timeline::new(&animation, 2, t0);
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 0),
            (1, Some(ms(20)))
        );
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 20),
            (2, Some(ms(40)))
        );
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 40),
            (2, None),
            "waits"
        );
        assert_eq!(step(&mut timeline, &animation, &frames, t0, 100), (2, None));
        // A frame lands: it shows from the draw that finds it, for its whole gap.
        frames.push(Frame {
            id: FrameId(3),
            ..frames[1].clone()
        });
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 130),
            (3, Some(ms(150)))
        );
        // A frame that lands before the last one's gap is out just follows it.
        frames.push(Frame {
            id: FrameId(4),
            ..frames[1].clone()
        });
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 150),
            (4, Some(ms(170)))
        );
        // Running (`s=3`): the end wraps.
        animation.state = AnimationState::Running;
        animation.control_serial += 1;
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 170),
            (1, Some(ms(190)))
        );
    }

    #[test]
    fn a_new_generation_restarts_the_timeline_from_the_current_frame() {
        let t0 = Instant::now();
        let mut frames = frames(&[10, 10, 10, 10]);
        let mut animation = playing(AnimationState::Running, 0);
        let mut timeline = Timeline::new(&animation, 4, t0);
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 25),
            (3, Some(ms(30)))
        );
        // `a=a,c=2`: frame 2, from now.
        animation.current = 2;
        animation.generation += 1;
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 27),
            (2, Some(ms(37)))
        );
        // Stopped, a new generation still moves it: a client-driven animation.
        animation.state = AnimationState::Stopped;
        animation.control_serial += 1;
        animation.current = 4;
        animation.generation += 1;
        assert_eq!(step(&mut timeline, &animation, &frames, t0, 30), (4, None));
        // `current` is clamped to the frames there are, and so is the position.
        animation.current = 9;
        animation.generation += 1;
        assert_eq!(step(&mut timeline, &animation, &frames, t0, 31), (4, None));
        frames.truncate(2);
        assert_eq!(step(&mut timeline, &animation, &frames, t0, 32), (2, None));
        animation.current = 0;
        animation.generation += 1;
        assert_eq!(step(&mut timeline, &animation, &frames, t0, 33), (1, None));
        // A timeline starts at the current frame, clamped.
        let started = Timeline::new(
            &Animation {
                current: 7,
                ..animation
            },
            2,
            t0,
        );
        assert_eq!(started.position, 2);
    }

    #[test]
    fn stopping_holds_the_frame_showing_and_running_again_restarts_its_clock() {
        let t0 = Instant::now();
        let frames = frames(&[10, 10, 10]);
        let mut animation = playing(AnimationState::Running, 0);
        let mut timeline = Timeline::new(&animation, 3, t0);
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 15),
            (2, Some(ms(20)))
        );
        // `s=1`: the frame showing stays, whatever `current` says.
        animation.state = AnimationState::Stopped;
        animation.control_serial += 1;
        assert_eq!(step(&mut timeline, &animation, &frames, t0, 15), (2, None));
        assert_eq!(step(&mut timeline, &animation, &frames, t0, 500), (2, None));
        // `s=3`: on from the frame showing, its gap counted from now.
        animation.state = AnimationState::Running;
        animation.control_serial += 1;
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 600),
            (2, Some(ms(610)))
        );
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 625),
            (1, Some(ms(630)))
        );
        assert_eq!(timeline.passes, 1);
        // `s=3` again mid-play keeps the clock and resets the loop count.
        animation.control_serial += 1;
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 627),
            (1, Some(ms(630)))
        );
        assert_eq!(timeline.passes, 0);
    }

    #[test]
    fn a_timeline_far_behind_resumes_from_now_instead_of_fast_forwarding() {
        let t0 = Instant::now();
        let frames = frames(&[40, 40, 40]);
        let animation = playing(AnimationState::Running, 0);
        let mut timeline = Timeline::new(&animation, 3, t0);
        // An hour without a draw: one frame on, showing from now.
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 3_600_000),
            (2, Some(ms(3_600_040)))
        );
        // Within the catch-up bound but more than a pass behind: one pass at most,
        // then from now.
        let frames = super::tests::frames(&[10, 10, 10]);
        let mut timeline = Timeline::new(&animation, 3, t0);
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 400),
            (2, Some(ms(410)))
        );
    }

    #[test]
    fn nothing_is_due_when_stopped_still_or_gapless() {
        let t0 = Instant::now();
        let stopped = playing(AnimationState::Stopped, 0);
        let running = playing(AnimationState::Running, 0);
        let loading = playing(AnimationState::Loading, 0);
        for (animation, gaps) in [
            (&stopped, &[10, 10][..]),
            (&running, &[10][..]),
            (&loading, &[10][..]),
            (&running, &[0, 0, 0][..]),
        ] {
            let frames = frames(gaps);
            let mut timeline = Timeline::new(animation, frames.len(), t0);
            assert_eq!(
                timeline.advance(animation, &frames, t0 + ms(25)),
                None,
                "{gaps:?}"
            );
            assert_eq!(timeline.position, 1);
        }
    }

    /// A 4x2 placement of an image whose frames have these gaps, the root first, each
    /// frame filled with its own byte, playing as `animation`.
    fn animated(gaps: &[u32], animation: Animation) -> Arc<Placement> {
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&placement);
        let image = &placement.image;
        image.set_frame_gap(1, gaps[0]);
        for (&gap, fill) in gaps.iter().zip(1u8..).skip(1) {
            image.push_frame(
                DecodedPixels {
                    width: 4,
                    height: 2,
                    rgba: vec![fill; 32].into(),
                },
                gap,
            );
        }
        image.set_animation(animation);
        placement
    }

    /// The handle id of the entry under `key`.
    fn handle_of(store: &ImageStore, key: &str) -> iced::advanced::image::Id {
        let cell = store.peek(key).unwrap_or_else(|| panic!("{key} ensured"));
        let EntryState::Ready { handle, .. } = &*cell.state() else {
            panic!("a Ready cell");
        };
        handle.id()
    }

    fn frame_keys(image: &ImageRecord) -> Vec<String> {
        image
            .frames()
            .iter()
            .zip(1..)
            .map(|(frame, number)| frame_key(image.uid, number, frame.id))
            .collect()
    }

    #[test]
    fn each_frame_draws_from_its_own_entry_and_they_all_go_with_the_image() {
        let (store, _) = test_store();
        let placement = animated(&[100, 100, 100], playing(AnimationState::Running, 0));
        let keys = frame_keys(&placement.image);
        assert_eq!(
            keys[0],
            format!("kitty:{}", placement.image.uid),
            "the root's"
        );
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        let mut handles = Vec::new();
        for (key, millis) in keys.iter().zip([0, 100, 200]) {
            mounted.redraw_at(t0 + ms(millis));
            let drawn = mounted.draw();
            assert_eq!(drawn.images.len(), 1);
            assert_eq!(drawn.images[0].handle, handle_of(&store, key), "{key}");
            handles.push(drawn.images[0].handle);
        }
        assert_ne!(handles[0], handles[1]);
        assert_ne!(handles[1], handles[2]);
        // Round again: the same entries, nothing minted twice.
        mounted.redraw_at(t0 + ms(300));
        assert_eq!(mounted.draw().images[0].handle, handles[0]);
        mounted.redraw_at(t0 + ms(400));
        assert_eq!(mounted.draw().images[0].handle, handles[1]);
        assert_eq!(store.ready_bytes(), 3 * 32);
        // One slot holds all three: they go with the image.
        placement.image.set_data(ImageData::Evicted);
        for key in &keys {
            wait_until("the frame's removal", || store.peek(key).is_none());
        }
        wait_until("the bytes released", || store.ready_bytes() == 0);
        assert!(mounted.draw().images.is_empty());
        assert_eq!(mounted.redraw_at(t0 + ms(500)), RedrawRequest::Wait);
    }

    #[test]
    fn a_playing_animation_asks_for_its_next_frame_and_a_stopped_one_for_nothing() {
        let (store, _) = test_store();
        let placement = animated(&[100, 50], playing(AnimationState::Running, 0));
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        assert_eq!(mounted.redraw_at(t0), RedrawRequest::At(t0 + ms(100)));
        assert_eq!(
            mounted.redraw_at(t0 + ms(120)),
            RedrawRequest::At(t0 + ms(150))
        );
        let second = mounted.draw().images[0].handle;
        // Stopped: nothing to ask for, and frame 2 stays.
        let mut animation = placement.image.animation();
        animation.state = AnimationState::Stopped;
        animation.control_serial += 1;
        placement.image.set_animation(animation);
        assert_eq!(mounted.redraw_at(t0 + ms(130)), RedrawRequest::Wait);
        assert_eq!(mounted.redraw_at(t0 + ms(900)), RedrawRequest::Wait);
        assert_eq!(mounted.draw().images[0].handle, second);
        // Running again, but out of view: nothing either.
        animation.state = AnimationState::Running;
        animation.control_serial += 1;
        placement.image.set_animation(animation);
        let away = Rectangle::new(Point::new(0.0, 500.0), Size::new(100.0, 100.0));
        assert_eq!(mounted.redraw_in(t0 + ms(1000), away), RedrawRequest::Wait);
        assert_eq!(
            mounted.redraw_at(t0 + ms(1000)),
            RedrawRequest::At(t0 + ms(1050))
        );
        // A still image, and an animation that has played its loops, ask for nothing.
        let still = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&still);
        let mut still = Mounted::new(
            KittyPlacement::new(still, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(still.redraw_at(t0), RedrawRequest::Wait);
        let once = animated(&[10, 10], playing(AnimationState::Running, 1));
        let mut once = Mounted::new(
            KittyPlacement::new(once, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(once.redraw_at(t0), RedrawRequest::At(t0 + ms(10)));
        assert_eq!(once.redraw_at(t0 + ms(10)), RedrawRequest::At(t0 + ms(20)));
        assert_eq!(once.redraw_at(t0 + ms(20)), RedrawRequest::Wait, "played");
        assert_eq!(once.redraw_at(t0 + ms(90)), RedrawRequest::Wait);
    }

    #[test]
    fn a_loading_animation_waits_for_frames_and_shows_each_as_it_lands() {
        let (store, _) = test_store();
        let placement = animated(&[30, 30], playing(AnimationState::Loading, 0));
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        assert_eq!(mounted.redraw_at(t0), RedrawRequest::At(t0 + ms(30)));
        assert_eq!(
            mounted.redraw_at(t0 + ms(30)),
            RedrawRequest::At(t0 + ms(60))
        );
        assert_eq!(mounted.redraw_at(t0 + ms(60)), RedrawRequest::Wait, "waits");
        // The frame lands; the session repaints for it.
        placement.image.push_frame(
            DecodedPixels {
                width: 4,
                height: 2,
                rgba: vec![9; 32].into(),
            },
            30,
        );
        assert_eq!(
            mounted.redraw_at(t0 + ms(200)),
            RedrawRequest::At(t0 + ms(230))
        );
        let keys = frame_keys(&placement.image);
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &keys[2]));
    }

    #[test]
    fn placements_of_one_image_show_one_frame_and_cost_one_redraw() {
        let (store, _) = test_store();
        let placement = animated(&[100, 100], playing(AnimationState::Running, 0));
        let twin = Arc::new(Placement::new(
            placement.image.clone(),
            0,
            placement.geometry,
        ));
        let mut first = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        assert_eq!(first.redraw_at(t0), RedrawRequest::At(t0 + ms(100)));
        // Mounted later, the twin joins the image's timeline rather than starting its own.
        let mut second = Mounted::new(
            KittyPlacement::new(twin, GRID, Some(store.clone())),
            Point::new(0.0, 40.0),
        );
        let other = animated(&[30, 30], playing(AnimationState::Running, 0));
        let mut third = Mounted::new(
            KittyPlacement::new(other, GRID, Some(store.clone())),
            Point::new(0.0, 80.0),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let now = t0 + ms(150);
        first.update_in(&mut shell, now, VIEWPORT);
        second.update_in(&mut shell, now, VIEWPORT);
        assert_eq!(
            shell.redraw_request(),
            RedrawRequest::At(t0 + ms(200)),
            "one"
        );
        // Another image's earlier boundary wins the one redraw.
        third.update_in(&mut shell, now, VIEWPORT);
        assert_eq!(shell.redraw_request(), RedrawRequest::At(now + ms(30)));
        let (a, b) = (first.draw(), second.draw());
        assert_eq!(a.images[0].handle, b.images[0].handle, "the same frame");
        assert_eq!(
            a.images[0].handle,
            handle_of(&store, &frame_keys(&placement.image)[1])
        );
    }

    #[test]
    fn an_edited_frame_lets_go_of_its_old_entry() {
        let (store, _) = test_store();
        let placement = animated(
            &[100, 100, 100],
            Animation {
                current: 2,
                ..Animation::default()
            },
        );
        let mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let old = frame_keys(&placement.image)[1].clone();
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &old));
        // An `a=f,r=2` edit: new pixels under a new id, drawn from a new entry; the old
        // one goes once the new one is tied to the record.
        placement.image.replace_frame(
            2,
            DecodedPixels {
                width: 4,
                height: 2,
                rgba: vec![7; 32].into(),
            },
        );
        let new = frame_keys(&placement.image)[1].clone();
        assert_ne!(new, old);
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &new));
        wait_until("the old entry's removal", || store.peek(&old).is_none());
        wait_until("the bytes released", || store.ready_bytes() == 32);
    }

    #[test]
    fn a_root_entry_an_earlier_slot_left_is_minted_again_not_drawn() {
        let (store, _) = test_store();
        let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&placement);
        let key = format!("kitty:{}", placement.image.uid);
        // An entry under the root's key from pixels that are gone, whose removal is
        // queued on the store's runtime and has not run.
        let stale = store.ensure_rgba(&key, 4, 2, zeroed(4, 2));
        let mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let drawn = mounted.draw().images[0].handle;
        assert!(stale.is_evicted(), "dropped before the draw ensured");
        assert_eq!(drawn, handle_of(&store, &key));
        // The root's pixels replaced in place (an `a=f,r=1` edit): the record let go of
        // its slot, whose hold's release is queued on the store's runtime. The next draw
        // shows the new pixels from a fresh entry, which that release leaves alone
        // whenever it runs: it was taken on the entry that is gone.
        placement.image.set_data(ImageData::Ready(DecodedPixels {
            width: 4,
            height: 2,
            rgba: vec![3; 32].into(),
        }));
        assert!(!placement.image.has_ui_slot());
        let again = mounted.draw().images[0].handle;
        assert_ne!(again, drawn);
        assert_eq!(again, handle_of(&store, &key));
        assert!(placement.image.has_ui_slot());
        let fresh = store.peek(&key).expect("the fresh entry");
        std::thread::sleep(ms(50));
        assert!(
            !fresh.is_evicted() && Arc::ptr_eq(&store.peek(&key).unwrap(), &fresh),
            "the old hold's release took nothing"
        );
        assert_eq!(mounted.draw().images[0].handle, again, "no second mint");
        assert_eq!((store.ready_bytes(), store.held_bytes()), (32, 32));
    }

    #[test]
    fn an_anchored_placement_steps_through_the_redraw_events_the_pane_forwards() {
        // The pane hands an anchor the window's events, as every other mount, with no
        // cursor and its clip (what `update_in` sends). An anchored animation asks for its
        // next frame on each and shows it on the next; nothing else is needed.
        let (store, _) = test_store();
        let mut placement = animated(&[40, 40], playing(AnimationState::Running, 0));
        Arc::get_mut(&mut placement).unwrap().anchored = true;
        let keys = frame_keys(&placement.image);
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        assert_eq!(mounted.redraw_at(t0), RedrawRequest::At(t0 + ms(40)));
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &keys[0]));
        assert_eq!(
            mounted.redraw_at(t0 + ms(40)),
            RedrawRequest::At(t0 + ms(80))
        );
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &keys[1]));
    }

    #[test]
    fn a_draw_shows_the_frame_the_redraw_event_stepped_to() {
        // The event's instant is the draw's clock, however much later the draw runs: the
        // frame asked for at the boundary is the one shown, never one past it. A clock set
        // well in the past keeps the wall clock out of it — had the draw stepped at
        // `Instant::now()` it would be a second on, past both 100 ms boundaries.
        let (store, _) = test_store();
        let placement = animated(&[100, 100], playing(AnimationState::Running, 0));
        let keys = frame_keys(&placement.image);
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("the monotonic clock is past the epoch");
        assert_eq!(mounted.redraw_at(t0), RedrawRequest::At(t0 + ms(100)));
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &keys[0]));
        assert_eq!(
            mounted.redraw_at(t0 + ms(99)),
            RedrawRequest::At(t0 + ms(100))
        );
        assert_eq!(
            mounted.draw().images[0].handle,
            handle_of(&store, &keys[0]),
            "a millisecond short of the boundary"
        );
        assert_eq!(
            mounted.redraw_at(t0 + ms(100)),
            RedrawRequest::At(t0 + ms(200))
        );
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &keys[1]));
    }

    #[test]
    fn a_relative_placements_animation_steps_with_the_family_and_is_culled_by_its_box() {
        let (store, _) = test_store();
        // A still parent; five rows down, an animated child.
        let parent = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
        ready(&parent);
        let playing = animated(&[40, 40], playing(AnimationState::Running, 0));
        let child = relative_of(&parent, playing.image.clone(), 0, 5, playing.geometry);
        let keys = frame_keys(&child.image);
        let mut mounted = Mounted::new(
            KittyPlacement::new(parent, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        assert_eq!(
            mounted.redraw_at(t0),
            RedrawRequest::At(t0 + ms(40)),
            "the child's boundary is the family's request"
        );
        let drawn = mounted.draw();
        assert_eq!(drawn.images.len(), 2);
        assert_eq!(drawn.images[1].handle, handle_of(&store, &keys[0]));
        assert_eq!(
            mounted.redraw_at(t0 + ms(40)),
            RedrawRequest::At(t0 + ms(80))
        );
        assert_eq!(
            mounted.draw().images[1].handle,
            handle_of(&store, &keys[1]),
            "the child stepped"
        );
        // A viewport holding the parent but not the child: nothing plays in it.
        let parent_only = rect(0.0, 0.0, 100.0, 50.0);
        assert_eq!(
            mounted.redraw_in(t0 + ms(80), parent_only),
            RedrawRequest::Wait
        );
        assert_eq!(mounted.draw_in(parent_only).images.len(), 1);
    }

    /// Play one full pass of `mounted`'s three-frame animation from `t0` (gaps of 50 ms),
    /// one redraw per boundary, and the handle each frame draws.
    fn play_three(mounted: &mut Mounted, t0: Instant) -> Vec<iced::advanced::image::Id> {
        [0, 50, 100]
            .into_iter()
            .map(|millis| {
                mounted.redraw_at(t0 + ms(millis));
                mounted.draw().images[0].handle
            })
            .collect()
    }

    #[test]
    fn a_root_replaced_mid_play_keeps_stable_frame_keys_and_mints_each_frame_once() {
        let (store, _) = test_store();
        let placement = animated(&[50, 50, 50], playing(AnimationState::Running, 0));
        let image = &placement.image;
        let keys = frame_keys(image);
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        let first = play_three(&mut mounted, t0);
        assert_eq!(store.ready_bytes(), 3 * 32);
        // An `a=f,r=1` edit while frame 3 shows: the record drops its slot, and the holds
        // on all three frames' entries queue their releases on the store runtime. The
        // frames after the root keep their ids, so their keys do not move.
        image.set_data(ImageData::Ready(DecodedPixels {
            width: 4,
            height: 2,
            rgba: vec![9; 32].into(),
        }));
        assert_eq!(
            frame_keys(image)[1..],
            keys[1..],
            "the frames keep their keys"
        );
        // One pass to re-attach the slot and re-ensure the frames: whether each re-ensures
        // its old entry or mints a fresh one (the frame's old entry may already be gone)
        // races the queued releases, but by the pass's end the new slot holds every entry,
        // and a hold keeps a cell in the store, so a release still falling due names a
        // cell no key maps to and removes nothing. From here the identities are stable.
        let _ = play_three(&mut mounted, t0 + ms(150));
        let settled = play_three(&mut mounted, t0 + ms(300));
        // The root shows its new pixels from a fresh entry, never the stale ones.
        assert_ne!(settled[0], first[0], "a fresh root entry");
        // Each frame is minted once and then stands: the next pass draws the very same
        // handles — nothing re-minted, and no stale removal evicted what the slot holds.
        assert_eq!(
            play_three(&mut mounted, t0 + ms(450)),
            settled,
            "minted once"
        );
        wait_until("the three frames settle under their keys", || {
            (store.ready_bytes(), store.held_bytes()) == (3 * 32, 3 * 32)
                && keys
                    .iter()
                    .all(|key| store.peek(key).is_some_and(|cell| !cell.is_evicted()))
        });

        // A root delete mid-play (`a=d,d=f,r=1`): frame 2 becomes the root under the
        // root's key, its own entry goes, and frame 3 stays. The generation bumped, so
        // the timeline restarts from the current frame (the root).
        assert!(image.remove_frame(1));
        assert_eq!(frame_keys(image), vec![keys[0].clone(), keys[2].clone()]);
        let t1 = t0 + ms(600);
        // Two passes to re-establish the two frames (the delete dropped the slot too).
        let after = (0..2)
            .map(|pass| {
                let base = t1 + ms(pass * 100);
                mounted.redraw_at(base);
                let root = mounted.draw().images[0].handle;
                mounted.redraw_at(base + ms(50));
                (root, mounted.draw().images[0].handle)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            after[0], after[1],
            "frame 2 as the root, then frame 3, minted once"
        );
        // The old frame-2 entry (its key now names nothing) goes; the two live frames stay.
        wait_until("the two frames settle and the orphan goes", || {
            store.peek(&keys[1]).is_none()
                && (store.ready_bytes(), store.held_bytes()) == (2 * 32, 2 * 32)
                && [&keys[0], &keys[2]]
                    .iter()
                    .all(|key| store.peek(key).is_some_and(|cell| !cell.is_evicted()))
        });
    }

    #[test]
    fn a_frame_delete_restarts_the_timeline_from_the_current_frame_and_frees_its_entry() {
        let (store, _) = test_store();
        let placement = animated(&[30, 30, 30], playing(AnimationState::Running, 0));
        let image = &placement.image;
        let keys = frame_keys(image);
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        for millis in [0, 30, 60] {
            mounted.redraw_at(t0 + ms(millis));
            mounted.draw();
        }
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &keys[2]));
        assert_eq!(store.ready_bytes(), 3 * 32);
        // `a=d,d=f,r=2` while frame 3 shows: the frames renumber and the generation
        // bumps, so the timeline restarts from the current frame (the root), now.
        let before = image.animation().generation;
        assert!(image.remove_frame(2));
        assert_eq!(image.animation().generation, before + 1);
        assert_eq!(
            mounted.redraw_at(t0 + ms(70)),
            RedrawRequest::At(t0 + ms(100))
        );
        assert_eq!(mounted.draw().images[0].handle, handle_of(&store, &keys[0]));
        // The deleted frame's entry went with that draw's hold.
        wait_until("the deleted frame's entry removal", || {
            store.peek(&keys[1]).is_none()
        });
        wait_until("its bytes released", || store.ready_bytes() == 2 * 32);
        assert_eq!(store.held_bytes(), 2 * 32);
        assert!(!store.peek(&keys[2]).unwrap().is_evicted());
    }

    #[test]
    fn catching_up_keeps_the_schedule_to_the_bound_and_resumes_from_now_past_it() {
        let t0 = Instant::now();
        let frames = frames(&[400, 400, 400]);
        let animation = playing(AnimationState::Running, 0);
        // Exactly the bound late for frame 1's end: frames keep their schedule, so frame
        // 2's 400 ms are over too and frame 3 shows until 1200.
        let mut timeline = Timeline::new(&animation, 3, t0);
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 900),
            (3, Some(ms(1200)))
        );
        // A millisecond more: the next frame shows from now.
        let mut timeline = Timeline::new(&animation, 3, t0);
        assert_eq!(
            step(&mut timeline, &animation, &frames, t0, 901),
            (2, Some(ms(1301)))
        );
    }

    #[test]
    fn a_remount_keeps_the_animations_place() {
        let (store, _) = test_store();
        let placement = animated(&[100, 100], playing(AnimationState::Running, 0));
        let keys = frame_keys(&placement.image);
        let t0 = Instant::now();
        let mut first = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(first.redraw_at(t0), RedrawRequest::At(t0 + ms(100)));
        first.redraw_at(t0 + ms(100));
        assert_eq!(first.draw().images[0].handle, handle_of(&store, &keys[1]));
        drop(first);
        // The line was laid out again: a new mount, the image's own timeline.
        let mut second = Mounted::new(
            KittyPlacement::new(placement, GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        assert_eq!(
            second.redraw_at(t0 + ms(120)),
            RedrawRequest::At(t0 + ms(200)),
            "frame 2's gap counts from when it started showing"
        );
        assert_eq!(second.draw().images[0].handle, handle_of(&store, &keys[1]));
    }

    #[test]
    fn a_deleted_placement_with_a_running_animation_asks_for_nothing() {
        let (store, _) = test_store();
        let placement = animated(&[40, 40], playing(AnimationState::Running, 0));
        let mut mounted = Mounted::new(
            KittyPlacement::new(placement.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        let t0 = Instant::now();
        assert_eq!(mounted.redraw_at(t0), RedrawRequest::At(t0 + ms(40)));
        placement.mark_deleted();
        assert_eq!(mounted.redraw_at(t0 + ms(10)), RedrawRequest::Wait);
        assert!(mounted.draw().images.is_empty());
        assert_eq!(mounted.redraw_at(t0 + ms(40)), RedrawRequest::Wait);
    }

    #[test]
    fn the_shared_entries_of_dropped_records_are_swept() {
        let (store, _) = test_store();
        let entries = || IMAGES.with_borrow(|images| images.entries.len());
        let before = entries();
        for _ in 0..200 {
            let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
            ready(&placement);
            let mounted = Mounted::new(
                KittyPlacement::new(placement, GRID, Some(store.clone())),
                Point::ORIGIN,
            );
            assert_eq!(mounted.draw().images.len(), 1);
        }
        assert!(entries() <= before.max(64), "{} entries", entries());
        // A live record keeps its entry through the sweeps.
        let kept = animated(&[10, 10], playing(AnimationState::Running, 0));
        let mounted = Mounted::new(
            KittyPlacement::new(kept.clone(), GRID, Some(store.clone())),
            Point::ORIGIN,
        );
        mounted.draw();
        for _ in 0..200 {
            let placement = placed(4, 2, geometry(1, 1, whole(4, 2), Fit::Natural));
            ready(&placement);
            Mounted::new(
                KittyPlacement::new(placement, GRID, Some(store.clone())),
                Point::ORIGIN,
            )
            .draw();
        }
        assert!(IMAGES.with_borrow(|images| {
            images
                .entries
                .get(&kept.image.uid)
                .is_some_and(|shared| shared.timeline.is_some())
        }));
    }
}
