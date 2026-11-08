//! Kitty terminal graphics protocol: the image table and the command state
//! machine.
//!
//! [`GraphicsState`] runs every whole graphics command on the thread that
//! drives the VT processor: it reassembles chunked uploads, checks what can
//! be checked from the bytes alone (header, size, length, quota), keeps the
//! table of stored images, hands pixel decoding to the [`GraphicsHost`], and
//! queues the protocol's replies in command order. A placement it creates is
//! returned to the caller, which attaches it at the cursor as an inline
//! object; the records here are what the UI downcasts that object's resource
//! to, so their shape is a contract with the renderer.
//!
//! Nothing here is on the byte path: the processor calls in only for a
//! complete graphics APC, an oversized one, a finished decode, and — when a
//! virtual placement exists — the placeholder runs of a line being built.
//!
//! # Kinds of placement
//!
//! A placement made at the cursor (`a=p`, `a=T`) consumes the cells of its
//! box on the line, unless it is an *anchor* — `C=1`, or a negative z-index,
//! which the protocol draws under the text — when it consumes none and the
//! inline object has an empty range ([`Placement::anchored`]). A *relative*
//! placement (`P`, `Q`) is never attached to a line: it lives in its
//! parent's [`Placement::children`] and is drawn relative to it. A *virtual*
//! placement (`U=1`, [`VirtualPlacement`]) is a prototype that Unicode
//! placeholder cells instantiate: each run of placeholder cells on a line
//! becomes a placement of its own ([`Placement::placeholder`]), attached
//! over the run's text. A relative placement may hang from a virtual one
//! ([`VirtualPlacement::children`]): the run showing the box's top-left
//! cell draws it.
//!
//! # Animation
//!
//! An image's frames are full-size RGBA canvases ([`Frame`]), the root
//! first. The frame commands (`a=f`, `a=a`, `a=c`, `d=f`) run in wire order
//! per image: the pixel work of a frame load or compose goes to the host
//! like a transmit's, one per image at a time, and every frame command
//! behind it waits in the image's queue — so a frame's base is always a
//! frame that has landed, and a control command sees the frames sent before
//! it. Replies park at admission, keeping their place in the reply order;
//! what can be refused from the command alone (a missing image, a
//! rectangle outside the image, the frame cap, the quota) is refused at
//! once. The queue holds at most [`MAX_QUEUED_FRAME_OPS`] commands, and a
//! load waiting in it holds its payload against the quota, so a client
//! cannot stack unbounded work behind one decode. The renderer reads
//! [`ImageRecord::animation`] and the frames live; see
//! [`Animation::generation`] and [`Animation::control_serial`] for what
//! restarts its position and its clock.
//!
//! # The screen, for positional deletes
//!
//! A terminal's screen is a grid the cursor moves around; ours is a stream
//! of lines with a cursor only ever on the pending one. The positional
//! deletes (`d=c`, `p`, `q`, `x`, `y`) are read against this approximation:
//! the screen is the last `grid.rows` committed lines plus the pending line,
//! row `y` (from 1 at the top) is the line `grid.rows - y` lines before the
//! pending one, and a placement sits at the cell it was attached at
//! ([`Placement::line_seq`], [`Placement::column`]) and spans its box's
//! columns and rows from there, as if the rows below its line were the
//! lines after it. A relative placement sits at its parent's cell plus its
//! offset ([`Placement::cell`]). Placements in scrollback above the screen
//! are never matched, placeholder runs never are (they are not placements
//! to the protocol), and virtual placements have no position at all — nor
//! do the relative placements hanging from one.

use std::any::Any;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::ops::{Range, RangeInclusive};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};

use smudgy_session_model::inline_content::{MAX_EFFECT_OUTSET, MAX_INLINE_OBJECTS};

use super::placeholder::PlaceholderRun;
use super::{
    Action, Compose, ComposeMode, ComposeSource, Compression, ControlData, DecodeRequest,
    DecodeResult, DecodeTicket, DecodedPixels, DeleteTarget, ErrorCode, Fed, Format, GraphicsError,
    GraphicsHost, MAX_IMAGE_DIMENSION, Medium, Quiet, Reassembler, ReplyQueue, Response,
    decode_payload, inflate, inflate_prefix, parse_apc, png_dimensions,
};

/// The most decoded bytes one image may take, whatever the quota says.
pub const MAX_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

/// The default per-session storage quota, counted in decoded RGBA bytes of
/// every image stored or still decoding.
pub const DEFAULT_QUOTA_BYTES: u64 = 64 * 1024 * 1024;

/// How many images one session stores. The byte quota alone would admit
/// millions of one-pixel images, each with its own record, so the count is
/// bounded too — loosely: this is what the default quota holds at 4 KiB
/// (a 32x32 icon) apiece, so for anything icon-sized or larger the bytes
/// run out first and only a stream of smaller images ever reaches it. A
/// tighter cap evicted a server's icons out of scrollback while they used
/// a few kilobytes between them.
///
/// Decodes in flight are not counted separately: a pending image holds its
/// decoded size against the quota from the moment it is admitted, so a
/// burst of transmits is bounded by the bytes and this count, not by how
/// many answers the host still owes. (A cap on those refused the 65th
/// transmit of a read batch, since the host reports back only between
/// batches.)
pub const MAX_IMAGES: usize = 16 * 1024;

/// Cell sizes travel as fixed point with eight fractional bits: a size in
/// logical pixels times this, rounded to the nearest unit ([`cell_fixed`]).
/// The pane's advance is rarely whole (9.6 px at common sizes), and a cell
/// truncated to whole pixels made a 96 px image reserve eleven columns where
/// it spans ten, leaving a blank strip before the text after it. A `u16`
/// holds a cell under 256 px, which no font size here reaches; larger
/// saturates.
pub const CELL_FIXED_SCALE: u16 = 256;

/// The cell size placements are laid out with until the UI has measured its
/// font and reported one: a common 9x18 terminal cell, in fixed point.
pub const FALLBACK_CELL: (u16, u16) = (9 * CELL_FIXED_SCALE, 18 * CELL_FIXED_SCALE);

/// A cell size measured in logical pixels as the fixed-point value it
/// travels as ([`CELL_FIXED_SCALE`] units to the pixel), rounded to the
/// nearest unit: a nominal 10 px the shaper hands back as 9.9999995 stays a
/// whole 10. Zero for a size not yet measured, or anything else that is no
/// size; a cell too large for the unit saturates.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped to the u16 range before the cast"
)]
pub fn cell_fixed(pixels: f32) -> u16 {
    if pixels.is_finite() && pixels > 0.0 {
        (pixels * f32::from(CELL_FIXED_SCALE))
            .round()
            .clamp(0.0, f32::from(u16::MAX)) as u16
    } else {
        0
    }
}

/// The whole pixels of a fixed-point cell size: what the size reports tell
/// a client. Truncated, not rounded, so a text area reckoned as cells times
/// this never exceeds the real one.
#[must_use]
pub const fn cell_whole_pixels(fixed: u16) -> u16 {
    fixed / CELL_FIXED_SCALE
}

/// The most columns a placement covers. A placement is placeholder text in
/// its line, so an absurd `c=` or a tiny cell must not turn into a line of
/// tens of thousands of spaces.
pub const MAX_PLACEMENT_CELLS: u16 = 1024;

/// The most rows a placement grows its line to. Rows cost more than
/// columns — the line's riser is sized to them — and a 1x10000 image at
/// `c=1` would otherwise ask for a line thousands of pixels tall. Past
/// either cap the box is clamped and the image is fit inside it.
pub const MAX_PLACEMENT_ROWS: u16 = 256;

/// The most parent links a relative placement may have above it: the
/// specification asks for chains of at least eight, and kitty allows exactly
/// that. A ninth link is refused with `ETOODEEP`, and so is a replacement
/// that would carry the chain below the placement it replaces past the
/// limit: every placement, adopted or not, stays within it. A virtual
/// parent is one link.
pub const PLACEMENT_DEPTH_LIMIT: usize = 8;

/// The most a compressed query payload is inflated on this thread. A query
/// is answered from its bytes alone when that is cheap; one whose inflated
/// size is larger than this (or, for a PNG, turns out to be) goes to the
/// host like a transmit, with its reply parked meanwhile.
const MAX_INLINE_INFLATE: u64 = 1024 * 1024;

/// The first id handed out for `I=` transmits. Clients pick their own ids
/// from the low end; starting high keeps the two apart.
const FIRST_ASSIGNED_ID: u32 = 0x8000_0000;

/// Bytes a PNG header probe needs: signature, IHDR length and type, width
/// and height.
const PNG_HEADER_LEN: usize = 24;

/// Process-unique image identity. The UI keys its image handles on it
/// (`kitty:{uid}`), so a retransmit under the same client id gets a fresh
/// key and never shows a stale handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageUid(pub u64);

impl ImageUid {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for ImageUid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The most frames one image holds, the root among them: a 257th is
/// refused with `ENOSPC`. Every frame is a full canvas, so the quota bounds
/// the bytes; this bounds the records and handles.
pub const MAX_FRAMES: usize = 256;

/// The most frame commands one image queues behind the decode in flight.
/// A load or compose past this is refused with `ENOSPC`; a control or
/// frame delete is dropped. Each queued load holds its payload against the
/// quota meanwhile, so what waits is bounded in count and in bytes.
pub const MAX_QUEUED_FRAME_OPS: usize = 64;

/// The gap a new frame gets without `z`, in milliseconds. The root frame's
/// gap is zero until `a=a,r=1,z=` sets it, as the specification has it.
pub const DEFAULT_FRAME_GAP_MS: u32 = 40;

/// Process-unique identity of one frame's pixel buffer: fresh whenever a
/// frame's pixels are replaced (an `a=f` edit, an `a=c` onto it), so a
/// handle keyed on it (`kitty:{uid}/{frame id}`) never shows stale pixels,
/// and stable while frames before it are deleted and it is renumbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameId(pub u64);

impl FrameId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for FrameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One frame of an image: a composed canvas the size of the image, and how
/// long it shows. Cloning shares the pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: FrameId,
    /// Straight RGBA8 of exactly the image's size.
    pub pixels: DecodedPixels,
    /// Milliseconds to show this frame before the next, when running. Zero
    /// is a *gapless* frame: skipped over at once, never shown by the
    /// animation (it exists as base data for other frames) — though a
    /// client may make it current.
    pub gap_ms: u32,
}

/// What an animation is doing (`a=a,s=`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationState {
    /// `s=1`, and every image to begin with: the current frame shows.
    #[default]
    Stopped,
    /// `s=2`: frames advance by their gaps; at the last one the animation
    /// holds it and waits for more frames instead of looping.
    Loading,
    /// `s=3`: frames advance by their gaps and the last loops to the first,
    /// [`Animation::loops`] times.
    Running,
}

/// An image's playback state. The renderer owns the clock and the playing
/// position; this is what the client set. It is read under its own lock,
/// apart from the frames, so a renderer clamps [`Self::current`] to
/// `1..=frames.len()` of the frames it read: the two can be a command
/// apart, and `current` is 1 while there are no frames at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Animation {
    pub state: AnimationState,
    /// Complete passes to play before holding the last frame: the `v` key
    /// less one (`v=2` plays once, `v=3` twice). Zero plays forever — the
    /// default, and `v=1`. Read live: changing it does not restart
    /// anything.
    pub loops: u32,
    /// The frame the client last made current (`a=a,c=`), 1-based, or the
    /// one a frame delete left in its place; one to begin with, with no
    /// frames too. A stopped animation shows it; a running one starts from
    /// it when [`Self::generation`] changes. Never trusted against the
    /// frames: clamp it to `1..=frames.len()`.
    pub current: u32,
    /// Bumped when the renderer must restart its playing position from
    /// [`Self::current`]: `a=a` with a `c=` that changes the current frame,
    /// and a frame delete (which renumbers frames). Not bumped by `s=`
    /// (that is [`Self::control_serial`]), by frames arriving, being edited
    /// or composed, by gap changes or by `v=`: the renderer reads those
    /// live, so a running animation keeps its place as frames stream in
    /// (what `Loading` relies on).
    pub generation: u64,
    /// Bumped on every `a=a` with `s=`, as kitty resets its loop counter on
    /// each: the renderer resets its loop count and, when [`Self::state`]
    /// went from `Stopped` to `Loading` or `Running`, restarts its clock
    /// from the frame it is showing. The position itself does not move:
    /// `s=3` re-sent mid-play does not jump, `s=1` freezes on the frame
    /// shown and `s=3` resumes from it. An `s=` that leaves the state as it
    /// is (the state it already has, or an unknown value) bumps this too.
    pub control_serial: u64,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            state: AnimationState::Stopped,
            loops: 0,
            current: 1,
            generation: 0,
            control_serial: 0,
        }
    }
}

/// Where an image's pixels are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageData {
    /// The decode is in flight.
    Pending,
    /// Straight RGBA8 of exactly the record's size.
    Ready(DecodedPixels),
    /// The decode failed; nothing will ever draw.
    Failed(GraphicsError),
    /// Quota eviction or an uppercase delete dropped the pixels. Placements
    /// left in scrollback keep their box and draw nothing.
    Evicted,
}

/// What a renderer ties to a record: see [`ImageRecord::attach_ui_slot`].
pub type UiSlot = Box<dyn Any + Send + Sync>;

/// One stored image. Shared between the image table and every placement of
/// it, so a placement in scrollback keeps the record (but not necessarily
/// its pixels: eviction tombstones it in place).
pub struct ImageRecord {
    pub uid: ImageUid,
    /// The client's id, or the one the terminal assigned for `I=`; zero for
    /// an image displayed without either (never addressable).
    pub id: u32,
    /// The client's `I=` number; zero when none.
    pub number: u32,
    /// The pixel size, known from the final chunk (`s`/`v`, or the PNG
    /// header) before anything decodes.
    pub width: u32,
    pub height: u32,
    /// The root frame's pixels (frame 1).
    data: RwLock<ImageData>,
    /// Every frame, the root first: `frames[0]` mirrors `data` while that
    /// is `Ready` (the same shared buffer) and the list is empty otherwise.
    /// Frame `n`, 1-based as the protocol counts, is `frames[n - 1]`. Taken
    /// after the data lock, never before it.
    frames: RwLock<Vec<Frame>>,
    /// Playback state. Taken after the frames lock.
    animation: RwLock<Animation>,
    /// Whatever the renderer minted from the pixels, living exactly as long
    /// as they do: cleared with an eviction or when the root frame's pixels
    /// are replaced, dropped with the record. Taken under its own lock
    /// after the others, never the other way.
    ui_slot: Mutex<Option<UiSlot>>,
}

impl std::fmt::Debug for ImageRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageRecord")
            .field("uid", &self.uid)
            .field("id", &self.id)
            .field("number", &self.number)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("data", &self.data)
            .field("frames", &self.frame_count())
            .field("animation", &self.animation())
            .field("ui_slot", &self.has_ui_slot())
            .finish()
    }
}

impl ImageRecord {
    /// A record whose pixels are still decoding, with a fresh [`ImageUid`].
    /// The state machine creates these; hosts need it only for tests.
    #[must_use]
    pub fn new(id: u32, number: u32, width: u32, height: u32) -> Self {
        Self {
            uid: ImageUid::next(),
            id,
            number,
            width,
            height,
            data: RwLock::new(ImageData::Pending),
            frames: RwLock::new(Vec::new()),
            animation: RwLock::new(Animation::default()),
            ui_slot: Mutex::new(None),
        }
    }

    /// A snapshot of where the pixels are. Cloning `Ready` shares the pixel
    /// buffer: it is a reference-count bump, not a copy.
    #[must_use]
    pub fn data(&self) -> ImageData {
        self.data
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The pixels, when they are ready.
    #[must_use]
    pub fn pixels(&self) -> Option<DecodedPixels> {
        match &*self.data.read().unwrap_or_else(PoisonError::into_inner) {
            ImageData::Ready(pixels) => Some(pixels.clone()),
            _ => None,
        }
    }

    /// Whether the pixels were dropped for good.
    #[must_use]
    pub fn is_evicted(&self) -> bool {
        matches!(
            *self.data.read().unwrap_or_else(PoisonError::into_inner),
            ImageData::Evicted
        )
    }

    /// Move the record to `data`. The state machine drives these
    /// transitions; hosts call this only in tests. `Ready` pixels are the
    /// root frame (frame 1): they replace its pixels under a fresh
    /// [`FrameId`] when it has some, keeping its gap; anything else clears
    /// every frame. The UI slot goes with the pixels it was minted from —
    /// taken when `Ready` pixels are replaced or dropped, and on
    /// [`ImageData::Evicted`] regardless — so whatever the renderer tied to
    /// them drops here, after the locks are released.
    pub fn set_data(&self, data: ImageData) {
        let evicted = matches!(data, ImageData::Evicted);
        let pixels = match &data {
            ImageData::Ready(pixels) => Some(pixels.clone()),
            _ => None,
        };
        let had_pixels = {
            let mut held = self.data.write().unwrap_or_else(PoisonError::into_inner);
            let had_pixels = matches!(*held, ImageData::Ready(_));
            *held = data;
            let mut frames = self.frames.write().unwrap_or_else(PoisonError::into_inner);
            match pixels {
                Some(pixels) => match frames.first_mut() {
                    Some(root) => {
                        root.id = FrameId::next();
                        root.pixels = pixels;
                    }
                    None => frames.push(Frame {
                        id: FrameId::next(),
                        pixels,
                        gap_ms: 0,
                    }),
                },
                None => frames.clear(),
            }
            had_pixels
        };
        if had_pixels || evicted {
            self.drop_ui_slot();
        }
    }

    fn drop_ui_slot(&self) {
        drop(
            self.ui_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }

    /// A snapshot of the playback state.
    #[must_use]
    pub fn animation(&self) -> Animation {
        *self
            .animation
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Set the playback state as given, generation included. For tests of
    /// what draws it; the state machine changes it through the protocol's
    /// commands.
    pub fn set_animation(&self, animation: Animation) {
        *self
            .animation
            .write()
            .unwrap_or_else(PoisonError::into_inner) = animation;
    }

    /// How many frames the image has: none until the root lands, one for a
    /// still image.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Frame `n`, 1-based; frame 1 is the root, whose pixels
    /// [`Self::pixels`] are. Shares the pixels.
    #[must_use]
    pub fn frame(&self, n: u32) -> Option<Frame> {
        let index = usize::try_from(n.checked_sub(1)?).ok()?;
        self.frames
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)
            .cloned()
    }

    /// Every frame in order, the root first. Each shares its pixels with
    /// the record.
    #[must_use]
    pub fn frames(&self) -> Vec<Frame> {
        self.frames
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Run `f` on the frames in order, the root first, without copying
    /// them: for a reader after their ids and gaps (a renderer stepping an
    /// animation on every redraw) rather than their pixels. The frames
    /// lock is held while `f` runs, so `f` may read the playback state
    /// ([`Self::animation`]) but nothing that takes the data lock
    /// ([`Self::is_evicted`], [`Self::pixels`]).
    pub fn with_frames<R>(&self, f: impl FnOnce(&[Frame]) -> R) -> R {
        f(&self.frames.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Append a frame after the last, showing for `gap_ms`. Its id, or
    /// `None` for a record without root pixels: a frame is never the root
    /// ([`Self::set_data`] makes one). The state machine keeps the count
    /// within [`MAX_FRAMES`]; this does not.
    pub fn push_frame(&self, pixels: DecodedPixels, gap_ms: u32) -> Option<FrameId> {
        let mut frames = self.frames.write().unwrap_or_else(PoisonError::into_inner);
        if frames.is_empty() {
            return None;
        }
        let id = FrameId::next();
        frames.push(Frame { id, pixels, gap_ms });
        Some(id)
    }

    /// Replace frame `n`'s pixels under a fresh id, keeping its gap. The
    /// root's go through [`Self::set_data`], so its UI slot drops. `None`
    /// for a frame the image does not have.
    pub fn replace_frame(&self, n: u32, pixels: DecodedPixels) -> Option<FrameId> {
        if n == 1 {
            if self.frame_count() == 0 {
                return None;
            }
            self.set_data(ImageData::Ready(pixels));
            return self.frame(1).map(|root| root.id);
        }
        let index = usize::try_from(n.checked_sub(1)?).ok()?;
        let mut frames = self.frames.write().unwrap_or_else(PoisonError::into_inner);
        let frame = frames.get_mut(index)?;
        frame.id = FrameId::next();
        frame.pixels = pixels;
        Some(frame.id)
    }

    /// Set frame `n`'s gap. `false` for a frame the image does not have.
    pub fn set_frame_gap(&self, n: u32, gap_ms: u32) -> bool {
        let Some(index) = n.checked_sub(1).and_then(|n| usize::try_from(n).ok()) else {
            return false;
        };
        let mut frames = self.frames.write().unwrap_or_else(PoisonError::into_inner);
        match frames.get_mut(index) {
            Some(frame) => {
                frame.gap_ms = gap_ms;
                true
            }
            None => false,
        }
    }

    /// Remove frame `n` (1-based), renumbering those after it; when the
    /// root goes, the next frame becomes the root and the UI slot drops
    /// with the old pixels. The only frame is never removed. The current
    /// frame follows kitty: one past the new end moves to the new last
    /// frame, one after the removed frame keeps showing the same pixels
    /// under its new number, and the removed frame's own number now shows
    /// the frame after it. The generation is bumped. `false` when nothing
    /// was removed.
    pub fn remove_frame(&self, n: u32) -> bool {
        let (root_went, count) = {
            let mut data = self.data.write().unwrap_or_else(PoisonError::into_inner);
            let mut frames = self.frames.write().unwrap_or_else(PoisonError::into_inner);
            let count = frames.len();
            let index = match usize::try_from(n) {
                Ok(index) if (1..=count).contains(&index) && count >= 2 => index - 1,
                _ => return false,
            };
            frames.remove(index);
            if index == 0 {
                *data = ImageData::Ready(frames[0].pixels.clone());
            }
            (index == 0, count - 1)
        };
        if root_went {
            self.drop_ui_slot();
        }
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        let mut animation = self
            .animation
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if animation.current > count {
            animation.current = count;
        } else if animation.current > n {
            animation.current -= 1;
        }
        animation.generation += 1;
        true
    }

    /// `a=a` as kitty applies it: `r` with a non-zero `z` sets that frame's
    /// gap (negative: gapless); `c` within the frames and different from
    /// the current frame makes it current, bumping the generation; `s`
    /// sets the state (`1` stopped, `2` loading, `3` running; any other
    /// value leaves it) and bumps the control serial, which resets the
    /// renderer's loop count and, leaving `Stopped`, restarts its clock
    /// from the frame shown — never the position; `v` sets the loops to
    /// `v - 1`. Whether anything was asked for, so the caller can repaint.
    fn animate(&self, control: &ControlData) -> bool {
        let mut changed = false;
        let count = u32::try_from(self.frame_count()).unwrap_or(u32::MAX);
        if control.rows != 0 && control.z != 0 && control.rows <= count {
            self.set_frame_gap(control.rows, u32::try_from(control.z).unwrap_or(0));
            changed = true;
        }
        let mut animation = self
            .animation
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if control.columns != 0 && control.columns <= count && control.columns != animation.current
        {
            animation.current = control.columns;
            animation.generation += 1;
            changed = true;
        }
        if control.width != 0 {
            animation.state = match control.width {
                1 => AnimationState::Stopped,
                2 => AnimationState::Loading,
                3 => AnimationState::Running,
                _ => animation.state,
            };
            animation.control_serial += 1;
            changed = true;
        }
        if control.height != 0 {
            animation.loops = control.height - 1;
            changed = true;
        }
        changed
    }

    /// Tie `slot` to the pixels: it lives until they are evicted or the
    /// record drops, whichever is first. The renderer uses it to drop what
    /// it minted from the pixels (a texture handle in its store) when they
    /// go, instead of holding it until its own cache turns over. Handed
    /// back untouched when the record is already evicted, so a slot offered
    /// for pixels read a moment earlier can be undone at once, and when a
    /// slot is already attached: the first stays, since one per record is
    /// the point.
    ///
    /// # Errors
    /// The slot, when it was not attached.
    pub fn attach_ui_slot(&self, slot: UiSlot) -> Result<(), UiSlot> {
        let mut held = self.ui_slot.lock().unwrap_or_else(PoisonError::into_inner);
        // Under the slot lock, so an eviction either sees the slot and
        // takes it, or refused it here.
        if held.is_some() || self.is_evicted() {
            return Err(slot);
        }
        *held = Some(slot);
        Ok(())
    }

    /// Whether a UI slot is attached.
    #[must_use]
    pub fn has_ui_slot(&self) -> bool {
        self.ui_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// What one frame of the record counts against the quota: its decoded
    /// RGBA size. The root and every frame cost this much each.
    #[must_use]
    pub fn decoded_len(&self) -> u64 {
        rgba_len(self.width, self.height)
    }
}

/// How the image fills the cells a placement covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Neither `c` nor `r`: the source rectangle at its natural pixel size.
    Natural,
    /// `c` and/or `r`: the source rectangle scaled to fit this box of cells,
    /// aspect preserved (letterboxed or pillarboxed, never stretched) and
    /// anchored top-left. When only one was given the other was derived from
    /// the aspect ratio, so the image fills the given dimension.
    Box { columns: u16, rows: u16 },
}

/// The part of the image a placement shows, in image pixels, already
/// intersected with the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Where and how big a placement is, fixed when it is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacementGeometry {
    /// Cells covered on the line: the placeholder columns of the inline
    /// object.
    pub columns: u16,
    /// Text rows the line grows to.
    pub rows: u16,
    pub source: SourceRect,
    /// `X`: pixels into the first cell where drawing starts, smaller than
    /// the cell.
    pub x_offset: u16,
    /// `Y`, likewise.
    pub y_offset: u16,
    pub fit: Fit,
    /// `z`: the stacking order among the line's objects, and the pass that
    /// draws the placement — over the text when zero or positive, under it
    /// when negative, under the cells' background colours too below
    /// `i32::MIN / 2`, as the specification lays them out.
    pub z: i32,
}

/// Where a relative placement sits: cells to the right (`columns`) and
/// down (`rows`) of its parent's top-left cell, either negative (`H`, `V`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelativeOffset {
    pub columns: i32,
    pub rows: i32,
}

/// One placement: an image shown in a box of cells. The inline object's
/// resource is an `Arc<Placement>`; a relative placement has no object of
/// its own and is reached through its parent's [`Self::children`].
#[derive(Debug)]
pub struct Placement {
    pub image: Arc<ImageRecord>,
    /// The client's `p=`; zero when none.
    pub id: u32,
    /// The box: its columns and rows are the box's size whether or not the
    /// placement consumes those cells on its line.
    pub geometry: PlacementGeometry,
    /// Consumes no cells: the inline object has an empty range at the cell
    /// the placement was made at, [`Baseline::Centered`] and a
    /// `paint_outset` reaching the box ([`anchor_outset`]), and the box is
    /// drawn from that cell over whatever follows — `C=1`, which leaves the
    /// cursor where it was, and a negative z-index, drawn under the text
    /// that follows it. A relative placement is anchored too, but to its
    /// parent rather than a line.
    ///
    /// [`Baseline::Centered`]: smudgy_session_model::inline_content::Baseline::Centered
    pub anchored: bool,
    /// `Some` for a relative placement (`P`, `Q`): drawn at its parent's
    /// top-left cell plus this offset, with the same cell metrics, and never
    /// attached to a line.
    pub relative: Option<RelativeOffset>,
    /// The relative placements whose parent this is, in creation order: at
    /// most [`MAX_INLINE_OBJECTS`], like a line's objects (one more is
    /// refused with `ENOSPC`). A parent draws its children after itself
    /// (each child its own, up to [`PLACEMENT_DEPTH_LIMIT`] deep); a child
    /// leaves the list when it is deleted or replaced. Written only by the
    /// state machine; readers take the lock briefly per frame. A
    /// placeholder run never has children of its own, but may draw its
    /// virtual placement's: see [`Self::child_placements`].
    pub children: RwLock<Vec<Arc<Placement>>>,
    /// Made for a run of Unicode placeholder cells from a virtual placement:
    /// not a placement to the protocol (no id, never a parent, untouched by
    /// positional deletes), gone when the virtual placement is deleted.
    pub placeholder: bool,
    /// The processor's committed-line count when the placement was attached:
    /// the sequence number of its line. Zero for a relative placement, which
    /// is attached to nothing: its cell follows its parent's
    /// ([`Self::cell`]).
    pub line_seq: u64,
    /// The cell on that line the box starts at, from zero; zero for a
    /// relative placement, likewise.
    pub column: u16,
    /// The parent of a relative placement, for walking a chain upward and
    /// for leaving its children when deleted.
    parent: Mutex<ParentLink>,
    /// For a placeholder run showing its virtual placement's top-left cell:
    /// that virtual placement, whose children the run draws.
    draws_children_of: Option<Weak<VirtualPlacement>>,
    /// Creation order within the state.
    created: u64,
    deleted: AtomicBool,
}

/// What a relative placement hangs from. Weak: a parent holds its children
/// strongly, so the link back must not.
#[derive(Clone, Debug, Default)]
enum ParentLink {
    #[default]
    None,
    Real(Weak<Placement>),
    Virtual(Weak<VirtualPlacement>),
}

impl Placement {
    /// A live placement at the cursor. The state machine creates these;
    /// hosts need it only for tests.
    #[must_use]
    pub fn new(image: Arc<ImageRecord>, id: u32, geometry: PlacementGeometry) -> Self {
        Self {
            image,
            id,
            geometry,
            anchored: false,
            relative: None,
            children: RwLock::new(Vec::new()),
            placeholder: false,
            line_seq: 0,
            column: 0,
            parent: Mutex::new(ParentLink::None),
            draws_children_of: None,
            created: 0,
            deleted: AtomicBool::new(false),
        }
    }

    /// Whether a delete removed this placement. Its box stays in the line
    /// (nothing reflows); the widget draws nothing in it.
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        self.deleted.load(Ordering::Relaxed)
    }

    /// Remove the placement from view, and its relative placements with it:
    /// their lifetime is tied to it. It leaves its own parent's children.
    /// The state machine does this for deletes and replacements; hosts call
    /// it only in tests.
    pub fn mark_deleted(&self) {
        self.mark_deleted_into(&mut Vec::new());
    }

    /// [`Self::mark_deleted`], noting the image of every relative placement
    /// below this one in `freed`, for the caller to free what nothing shows
    /// any more. The walk keeps its own stack: a chain is at most
    /// [`PLACEMENT_DEPTH_LIMIT`] deep, but a wire-driven recursion is not a
    /// thing to rely on a limit for.
    fn mark_deleted_into(&self, freed: &mut Vec<ImageUid>) {
        self.deleted.store(true, Ordering::Relaxed);
        self.leave_parent();
        let mut pending = take_children(&self.children);
        while let Some(child) = pending.pop() {
            child.deleted.store(true, Ordering::Relaxed);
            freed.push(child.image.uid);
            pending.extend(take_children(&child.children));
        }
    }

    /// Take this placement out of its parent's children.
    fn leave_parent(&self) {
        match self.parent_link() {
            ParentLink::None => {}
            ParentLink::Real(parent) => {
                if let Some(parent) = parent.upgrade() {
                    remove_child(&parent.children, self);
                }
            }
            ParentLink::Virtual(parent) => {
                if let Some(parent) = parent.upgrade() {
                    remove_child(&parent.children, self);
                }
            }
        }
    }

    /// What to draw right now: the image's pixels, unless the placement was
    /// deleted or the image is not (or no longer) ready.
    #[must_use]
    pub fn pixels(&self) -> Option<DecodedPixels> {
        if self.is_deleted() {
            None
        } else {
            self.image.pixels()
        }
    }

    /// A snapshot of the relative placements to draw after this one, each
    /// at this placement's top-left cell plus its offset: [`Self::children`]
    /// — or, for a placeholder run showing its virtual placement's top-left
    /// cell, that virtual placement's children
    /// ([`VirtualPlacement::children`]).
    #[must_use]
    pub fn child_placements(&self) -> Vec<Arc<Placement>> {
        if let Some(virt) = self.draws_children_of.as_ref().and_then(Weak::upgrade) {
            return virt.child_placements();
        }
        self.own_children()
    }

    /// The cell the box starts at, `(line sequence number, column)`: where
    /// the placement was attached — or, for a relative placement, its
    /// parent's cell plus its offset, up the chain, clamped to the line's
    /// start. `None` for one hanging from a virtual placement, which has no
    /// cell, or whose parent is gone.
    #[must_use]
    pub fn cell(&self) -> Option<(u64, u16)> {
        let Some(offset) = self.relative else {
            return Some((self.line_seq, self.column));
        };
        let (mut rows, mut columns) = (i64::from(offset.rows), i64::from(offset.columns));
        let mut link = self.parent_link();
        for _ in 0..=PLACEMENT_DEPTH_LIMIT {
            let parent = match link {
                ParentLink::Real(parent) => parent.upgrade()?,
                ParentLink::None | ParentLink::Virtual(_) => return None,
            };
            match parent.relative {
                None => {
                    let line_seq = parent.line_seq.saturating_add_signed(rows);
                    let column = i64::from(parent.column) + columns;
                    let column =
                        u16::try_from(column.clamp(0, i64::from(u16::MAX))).unwrap_or(u16::MAX);
                    return Some((line_seq, column));
                }
                Some(offset) => {
                    rows += i64::from(offset.rows);
                    columns += i64::from(offset.columns);
                    link = parent.parent_link();
                }
            }
        }
        None
    }

    /// The box's rows and columns on the screen, from its [`Self::cell`]:
    /// the lines it reaches, reading its rows as the lines after its own
    /// (see the module doc), and the cells it covers on each.
    fn footprint(&self) -> Option<(RangeInclusive<u64>, Range<u32>)> {
        let (line, column) = self.cell()?;
        let rows = u64::from(self.geometry.rows.max(1));
        let start = u32::from(column);
        Some((
            line..=line.saturating_add(rows - 1),
            start..start + u32::from(self.geometry.columns),
        ))
    }

    fn is_live(&self) -> bool {
        !self.is_deleted()
    }

    /// The relative placements whose parent this placement itself is.
    fn own_children(&self) -> Vec<Arc<Placement>> {
        self.children
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The real parent of a relative placement, if it is still around.
    fn parent(&self) -> Option<Arc<Placement>> {
        match self.parent_link() {
            ParentLink::Real(parent) => parent.upgrade(),
            ParentLink::None | ParentLink::Virtual(_) => None,
        }
    }

    fn parent_link(&self) -> ParentLink {
        self.parent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_parent(&self, link: ParentLink) {
        *self.parent.lock().unwrap_or_else(PoisonError::into_inner) = link;
    }

    /// How many parent links are above this placement (a virtual parent is
    /// one), counting at most one past the limit.
    fn depth(&self) -> usize {
        let mut depth = 0;
        let mut link = self.parent_link();
        while !matches!(link, ParentLink::None) {
            depth += 1;
            if depth > PLACEMENT_DEPTH_LIMIT {
                break;
            }
            link = match link {
                ParentLink::Real(parent) => parent
                    .upgrade()
                    .map_or(ParentLink::None, |parent| parent.parent_link()),
                ParentLink::None | ParentLink::Virtual(_) => ParentLink::None,
            };
        }
        depth
    }

    /// How many links the longest chain below this placement has: zero
    /// with no children. Counts at most one past the limit, with its own
    /// stack, like [`Self::mark_deleted_into`].
    fn subtree_height(&self) -> usize {
        let mut height = 0;
        let mut pending: Vec<(Arc<Placement>, usize)> = self
            .own_children()
            .into_iter()
            .map(|child| (child, 1))
            .collect();
        while let Some((node, level)) = pending.pop() {
            height = height.max(level);
            if level > PLACEMENT_DEPTH_LIMIT {
                break;
            }
            pending.extend(
                node.own_children()
                    .into_iter()
                    .map(|child| (child, level + 1)),
            );
        }
        height
    }

    /// Whether `target` is this placement or one of its ancestors.
    fn is_or_descends_from(self: &Arc<Self>, target: &Arc<Placement>) -> bool {
        let mut at = Some(self.clone());
        let mut steps = 0;
        while let Some(placement) = at {
            if Arc::ptr_eq(&placement, target) {
                return true;
            }
            steps += 1;
            if steps > PLACEMENT_DEPTH_LIMIT + 1 {
                break;
            }
            at = placement.parent();
        }
        false
    }
}

fn take_children(children: &RwLock<Vec<Arc<Placement>>>) -> Vec<Arc<Placement>> {
    std::mem::take(&mut *children.write().unwrap_or_else(PoisonError::into_inner))
}

fn remove_child(children: &RwLock<Vec<Arc<Placement>>>, child: &Placement) {
    children
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|c| !std::ptr::eq(Arc::as_ptr(c), child));
}

/// A virtual placement (`a=p,U=1`): the prototype Unicode placeholder cells
/// instantiate. It is never drawn itself and has no position; each run of
/// placeholder cells naming it gets a placement of its own, cropped to the
/// run's part of the box ([`GraphicsState::placeholder_run`]).
#[derive(Debug)]
pub struct VirtualPlacement {
    pub image: Arc<ImageRecord>,
    /// The client's `p=`; zero when none, when any placeholder with no
    /// placement id may use it.
    pub id: u32,
    /// `c`: the box's columns; zero means as many as the image needs at its
    /// natural size.
    pub columns: u32,
    /// `r`, likewise.
    pub rows: u32,
    /// The relative placements whose parent this is (`P`, `Q` naming it),
    /// in creation order and capped like [`Placement::children`]. The
    /// specification positions them from the top-left of the placeholder
    /// cells that instantiate the placement; here the run placement showing
    /// the box's top-left cell (row 0, column 0) draws them, handing them
    /// over as its own [`Placement::child_placements`]. The approximation:
    /// they appear only on a line where that cell is printed, and not when
    /// letterboxing leaves it blank. Deleted with the virtual placement;
    /// moved to the successor when a new virtual placement with its id
    /// supersedes it.
    pub children: RwLock<Vec<Arc<Placement>>>,
    /// The run placements made from it, deleted with it.
    runs: Mutex<Vec<Weak<Placement>>>,
    created: u64,
    deleted: AtomicBool,
}

impl VirtualPlacement {
    /// Whether a delete removed it.
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        self.deleted.load(Ordering::Relaxed)
    }

    /// A snapshot of [`Self::children`].
    #[must_use]
    pub fn child_placements(&self) -> Vec<Arc<Placement>> {
        self.children
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Remove it, every run placement made from it and every relative
    /// placement hanging from it, noting the latter's images in `freed`.
    fn mark_deleted(&self, freed: &mut Vec<ImageUid>) {
        let runs = std::mem::take(&mut *self.runs.lock().unwrap_or_else(PoisonError::into_inner));
        for run in runs.iter().filter_map(Weak::upgrade) {
            run.mark_deleted();
        }
        self.retire_into(freed);
    }

    /// Stop it being the prototype of anything — its relative placements go
    /// with it — while the runs already in scrollback stay as they are:
    /// for a virtual placement a real one supersedes under its id, or whose
    /// image a retransmit replaced.
    fn retire(&self) {
        self.retire_into(&mut Vec::new());
    }

    fn retire_into(&self, freed: &mut Vec<ImageUid>) {
        self.deleted.store(true, Ordering::Relaxed);
        for child in take_children(&self.children) {
            freed.push(child.image.uid);
            child.mark_deleted_into(freed);
        }
    }

    fn add_run(&self, run: &Arc<Placement>) {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        if runs.len() >= MIN_PRUNE_AT && runs.len().is_power_of_two() {
            runs.retain(|weak| weak.upgrade().is_some_and(|run| run.is_live()));
        }
        runs.push(Arc::downgrade(run));
    }
}

/// The parent a relative placement names.
enum Parent {
    Real(Arc<Placement>),
    Virtual(Arc<VirtualPlacement>),
}

impl Parent {
    fn children(&self) -> &RwLock<Vec<Arc<Placement>>> {
        match self {
            Self::Real(parent) => &parent.children,
            Self::Virtual(parent) => &parent.children,
        }
    }

    fn link(&self) -> ParentLink {
        match self {
            Self::Real(parent) => ParentLink::Real(Arc::downgrade(parent)),
            Self::Virtual(parent) => ParentLink::Virtual(Arc::downgrade(parent)),
        }
    }

    /// Whether `child` hangs from this parent.
    fn is_parent_of(&self, child: &Placement) -> bool {
        match (self, child.parent_link()) {
            (Self::Real(parent), ParentLink::Real(link)) => {
                std::ptr::eq(Arc::as_ptr(parent), link.as_ptr())
            }
            (Self::Virtual(parent), ParentLink::Virtual(link)) => {
                std::ptr::eq(Arc::as_ptr(parent), link.as_ptr())
            }
            _ => false,
        }
    }

    /// How many parent links a child of this parent has above it.
    fn child_depth(&self) -> usize {
        match self {
            Self::Real(parent) => parent.depth() + 1,
            Self::Virtual(_) => 1,
        }
    }
}

/// How far an anchored placement's box reaches from its cell, in logical
/// pixels: the larger of its width and height, so the inline object's
/// `paint_outset` covers the box however it is laid out. Clamped to
/// [`MAX_EFFECT_OUTSET`]; the box of a placement wider than that is culled
/// as if it ended there. `cell` is in fixed point, as everywhere here.
#[must_use]
pub fn anchor_outset(geometry: &PlacementGeometry, cell: (u16, u16)) -> u16 {
    let (cell_width, cell_height) = cell_or_fallback(cell);
    let pixels = |cells: u16, cell: u16| {
        (u64::from(cells) * u64::from(cell)).div_ceil(u64::from(CELL_FIXED_SCALE))
    };
    let extent = pixels(geometry.columns, cell_width).max(pixels(geometry.rows, cell_height));
    u16::try_from(extent)
        .unwrap_or(u16::MAX)
        .min(MAX_EFFECT_OUTSET)
}

/// `cell`, or [`FALLBACK_CELL`] while either side is unknown: what every
/// placement is laid out with, and what the size reports say, so the two
/// agree before the pane has measured a font as well as after.
#[must_use]
pub fn cell_or_fallback(cell: (u16, u16)) -> (u16, u16) {
    if cell.0 == 0 || cell.1 == 0 {
        FALLBACK_CELL
    } else {
        cell
    }
}

/// What a command handler needs from the processor around it.
pub struct GraphicsEnv<'a> {
    pub host: &'a mut dyn GraphicsHost,
    pub replies: &'a mut ReplyQueue,
    /// The cell size as the UI last reported it, in fixed point
    /// ([`CELL_FIXED_SCALE`] units to the logical pixel); zero while
    /// unknown, when [`FALLBACK_CELL`] lays placements out instead.
    pub cell: (u16, u16),
    /// Whether this connection answers at all (`GraphicsMode::Full`).
    pub replies_enabled: bool,
    /// How many more inline objects the line at the cursor can hold. A
    /// placement that would be the one too many is refused before it is
    /// registered or answered for, so no `OK` goes out for a tile that is
    /// never shown.
    pub objects_free: usize,
    /// The sequence number of the line at the cursor: how many lines the
    /// processor has committed before it. Stamped on placements made now
    /// ([`Placement::line_seq`]) and the bottom of the screen for positional
    /// deletes.
    pub line_seq: u64,
    /// The cell on that line the command acts at, from zero: the cursor's
    /// ([`Placement::column`]), or a placeholder run's first cell.
    pub column: u16,
    /// The main pane's grid, `(columns, rows)`: the screen's height for
    /// positional deletes.
    pub grid: (u16, u16),
}

/// One image in the table.
#[derive(Debug)]
struct Entry {
    record: Arc<ImageRecord>,
    /// Placements created with a placement id, by that id, so a new one
    /// with the same id replaces the old at once. Weak, like `anonymous`:
    /// scrollback owns placements, and one whose line has gone counts as
    /// removed.
    named: HashMap<u32, Weak<Placement>>,
    /// Placements created without a placement id, run placements made for
    /// placeholder cells among them.
    anonymous: Vec<Weak<Placement>>,
    /// The live virtual placements: at most one without an id (a new one
    /// supersedes it) and one per id. Strong: nothing else holds them.
    virtuals: Vec<Arc<VirtualPlacement>>,
    /// Both collections are swept of gone and deleted placements when they
    /// reach this many, so a server placing one image over and over costs
    /// amortised constant time per placement, not a scan each.
    prune_at: usize,
    /// Creation order, for "the newest image with this number".
    created: u64,
    /// Last creation or placement, for least-recently-used eviction.
    used: u64,
    /// Decoded bytes the image holds against the quota: its root and every
    /// frame, those still decoding included.
    charged: u64,
    /// A decode of this image (its root, a frame, a compose) is in flight:
    /// the frame commands behind it wait in `queue`.
    busy: bool,
    /// Frame commands waiting for the decode in flight, in wire order.
    queue: VecDeque<FrameOp>,
    /// New frames admitted but not landed — queued or in flight — so the
    /// frame cap counts them.
    frames_coming: usize,
}

/// The smallest sweep threshold of an image's placement lists.
const MIN_PRUNE_AT: usize = 64;

impl Entry {
    fn new(record: Arc<ImageRecord>, tick: u64) -> Self {
        Self {
            record,
            named: HashMap::new(),
            anonymous: Vec::new(),
            virtuals: Vec::new(),
            prune_at: MIN_PRUNE_AT,
            created: tick,
            used: tick,
            charged: 0,
            busy: false,
            queue: VecDeque::new(),
            frames_coming: 0,
        }
    }

    /// Whether a frame command arriving now must wait its turn.
    fn waits(&self) -> bool {
        self.busy || !self.queue.is_empty()
    }

    /// The frames the image will have once everything admitted lands: at
    /// least the root (which is still decoding while there are none).
    fn frames_expected(&self) -> usize {
        self.record.frame_count().max(1) + self.frames_coming
    }

    /// The placements still shown somewhere: relative placements and
    /// placeholder runs among them, virtual placements not.
    fn live_placements(&self) -> impl Iterator<Item = Arc<Placement>> + '_ {
        self.named
            .values()
            .chain(&self.anonymous)
            .filter_map(Weak::upgrade)
            .filter(|placement| placement.is_live())
    }

    /// Whether anything keeps the image in use: a placement shown
    /// somewhere, or a virtual placement waiting for placeholders (kitty
    /// counts those as placements too).
    fn has_live_placements(&self) -> bool {
        self.live_placements().next().is_some() || !self.virtuals.is_empty()
    }

    /// Register a placement. The live placement with the same id, which it
    /// replaces, is handed back for the caller to retire (it is not deleted
    /// here: its relative placements move to the replacement first).
    fn add(&mut self, placement: &Arc<Placement>) -> Option<Arc<Placement>> {
        if self.named.len() + self.anonymous.len() >= self.prune_at {
            let live = |weak: &Weak<Placement>| weak.upgrade().is_some_and(|p| p.is_live());
            self.named.retain(|_, weak| live(weak));
            self.anonymous.retain(live);
            self.prune_at = ((self.named.len() + self.anonymous.len()) * 2).max(MIN_PRUNE_AT);
        }
        let weak = Arc::downgrade(placement);
        if placement.id == 0 {
            self.anonymous.push(weak);
            None
        } else {
            self.named
                .insert(placement.id, weak)
                .and_then(|old| old.upgrade())
                .filter(|old| old.is_live())
        }
    }

    /// Register a virtual placement, superseding the one with its id (or
    /// the anonymous one when it has none). The superseded one is handed
    /// back; the runs already made from it stay as they are, since a client
    /// that re-creates its virtual placement means to change what new
    /// placeholders show, not to blank what it printed before.
    fn add_virtual(&mut self, placement: Arc<VirtualPlacement>) -> Option<Arc<VirtualPlacement>> {
        let old = self
            .virtuals
            .iter()
            .position(|v| v.id == placement.id)
            .map(|at| self.virtuals.remove(at));
        self.virtuals.push(placement);
        old
    }

    /// Drop the virtual placement with id `id`, if any, handing it back.
    fn take_virtual(&mut self, id: u32) -> Option<Arc<VirtualPlacement>> {
        self.virtuals
            .iter()
            .position(|v| v.id == id)
            .map(|at| self.virtuals.remove(at))
    }

    /// The virtual placement a placeholder names: by id, or the newest for
    /// id zero.
    fn virtual_for(&self, id: u32) -> Option<&Arc<VirtualPlacement>> {
        if id == 0 {
            self.virtuals.iter().max_by_key(|v| v.created)
        } else {
            self.virtuals.iter().find(|v| v.id == id)
        }
    }

    /// Delete every placement (virtual ones included), or only the one with
    /// placement id `only` — a real or a virtual one, since the two share
    /// the id space. Relative placements go with their parents; the images
    /// of those below the deleted ones are noted in `touched` for freeing.
    /// How many virtual placements went is returned.
    fn delete_placements(&mut self, only: u32, touched: &mut Vec<ImageUid>) -> usize {
        if only == 0 {
            for placement in self.live_placements() {
                placement.mark_deleted_into(touched);
            }
            self.named.clear();
            self.anonymous.clear();
            let virtuals = std::mem::take(&mut self.virtuals);
            for virt in &virtuals {
                virt.mark_deleted(touched);
            }
            virtuals.len()
        } else {
            if let Some(placement) = self.named.remove(&only).and_then(|weak| weak.upgrade()) {
                placement.mark_deleted_into(touched);
            }
            self.take_virtual(only).map_or(0, |virt| {
                virt.mark_deleted(touched);
                1
            })
        }
    }

    /// Retire every virtual placement: the image is no longer the one its
    /// id names. How many went is returned.
    fn retire_virtuals(&mut self) -> usize {
        let virtuals = std::mem::take(&mut self.virtuals);
        for virt in &virtuals {
            virt.retire();
        }
        virtuals.len()
    }
}

/// Who a command's reply goes to, and how quietly.
#[derive(Clone, Copy, Debug)]
struct ReplyTo {
    id: u32,
    number: u32,
    placement: u32,
    /// The `r` an `a=f` or `a=a` gave, echoed on every reply as kitty's
    /// `finish_command_response` does — the refusals before a load runs,
    /// a failing continuation chunk and an oversized command included. A
    /// load that ran replaces it with the frame it settled on
    /// ([`ReplyQueue::resolve_with`]). Zero for the other actions, which
    /// never echo it.
    frame: u32,
    quiet: Quiet,
}

impl ReplyTo {
    /// A command without `i` or `I` is never answered, and neither is
    /// anything on a connection that does not answer.
    fn new(
        id: u32,
        number: u32,
        placement: u32,
        frame: u32,
        quiet: Quiet,
        replies_enabled: bool,
    ) -> Self {
        let quiet = if replies_enabled && (id != 0 || number != 0) {
            quiet
        } else {
            Quiet::SuppressAll
        };
        Self {
            id,
            number,
            placement,
            frame,
            quiet,
        }
    }

    fn of(control: &ControlData, replies_enabled: bool) -> Self {
        Self::new(
            control.image_id,
            control.image_number,
            control.placement_id,
            echoed_frame(control),
            control.quiet,
            replies_enabled,
        )
    }

    fn response(self, status: Result<(), GraphicsError>) -> Response {
        Response {
            image_id: (self.id != 0).then_some(self.id),
            image_number: (self.number != 0).then_some(self.number),
            placement_id: (self.placement != 0).then_some(self.placement),
            extra: frame_key(self.frame),
            status,
        }
    }

    fn answer(self, replies: &mut ReplyQueue, status: Result<(), GraphicsError>) {
        replies.push_response(&self.response(status), self.quiet);
    }
}

/// The `r` a reply to `control` echoes: the frame an `a=f` or `a=a` named,
/// zero (none) for every other action, as kitty echoes it.
fn echoed_frame(control: &ControlData) -> u32 {
    match control.action {
        Action::Frame | Action::Animate => control.rows,
        _ => 0,
    }
}

/// A decode the host owes an answer for.
#[derive(Debug)]
struct Parked {
    /// Where the pixels land.
    landing: Landing,
    /// What the reply says once the pixels have landed: `OK`, or the
    /// refusal of a placement an `a=T` could not make (the image is stored
    /// all the same). A decode that fails answers with its own error...
    landed: Result<(), GraphicsError>,
    /// Keys the reply echoes after the ids: `r=` for the frame a load made.
    extra: Vec<(u8, String)>,
    /// ...unless the data was local media the host read for the peer,
    /// when a failed decode answers `EBADF` whatever the host found (see
    /// [`read_verdict`]).
    local_media: bool,
}

/// Where a decode's pixels land.
#[derive(Debug)]
enum Landing {
    /// A query: nowhere, however the decode goes. The canvas it declared
    /// is held against the quota meanwhile ([`GraphicsState::query_bytes`])
    /// and released when the decode lands.
    Nowhere { charged: u64 },
    /// A transmit: the root of its record.
    Root(Arc<ImageRecord>),
    /// `a=f`: a new frame appended, or an edited frame replaced.
    Frame {
        record: Arc<ImageRecord>,
        target: FrameTarget,
        /// The frame's gap when the command gave one.
        gap: Option<u32>,
        /// Bytes charged for a new frame, released if it fails to land.
        charged: u64,
    },
    /// `a=c`: the destination frame, replaced.
    Compose {
        record: Arc<ImageRecord>,
        destination: u32,
    },
}

/// Which frame an `a=f` lands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameTarget {
    /// Appended after the last.
    New,
    /// Frame `n`, 1-based, replaced.
    Edit(u32),
}

/// A frame command waiting its turn behind a decode of its image
/// ([`Entry::queue`]).
#[derive(Debug)]
enum FrameOp {
    Load(FrameLoad),
    Compose(FrameCompose),
    /// `a=a`: never answered when it runs, so it carries no ticket.
    Control(ControlData),
    /// `d=f` / `d=F`, with `r` as given.
    DeleteFrame {
        frame: u32,
        free: bool,
    },
}

impl FrameOp {
    /// The reply parked for the command, which carries the `r` it gave.
    fn ticket(&self) -> Option<DecodeTicket> {
        match self {
            Self::Load(load) => Some(load.ticket),
            Self::Compose(compose) => Some(compose.ticket),
            Self::Control(_) | Self::DeleteFrame { .. } => None,
        }
    }
}

/// An `a=f` admitted and waiting to be decoded.
#[derive(Debug)]
struct FrameLoad {
    ticket: DecodeTicket,
    format: Format,
    compression: Compression,
    /// The data's rectangle: its size and its top-left on the image.
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    data: Vec<u8>,
    /// `r` as given: the frame to edit, or (absent, or past the end) a
    /// new frame.
    frame: u32,
    /// `c`: the frame a new frame starts from; zero for the background.
    base: u32,
    mode: ComposeMode,
    background: u32,
    /// `z`: `Some(0)` for a gapless frame, `None` when unspecified.
    gap: Option<u32>,
    /// Bytes charged for a new frame; zero for an edit. Counted in
    /// [`Entry::frames_coming`] exactly while non-zero.
    charged: u64,
    /// Bytes the payload holds against the quota while the load waits in
    /// the queue (`data.len()`; zero for a load that ran at once), released
    /// when it runs — the host holds the data then — or the image goes.
    held: u64,
    /// Whether the data is local media the host read for the peer: its
    /// decode then answers `EBADF` whatever goes wrong (see
    /// [`read_verdict`]).
    local_media: bool,
}

/// An `a=c` admitted and waiting for its frames.
#[derive(Clone, Copy, Debug)]
struct FrameCompose {
    ticket: DecodeTicket,
    source: u32,
    destination: u32,
    width: u32,
    height: u32,
    source_at: (u32, u32),
    destination_at: (u32, u32),
    mode: ComposeMode,
}

/// The `r=` a frame command's reply echoes, as kitty's does: the frame it
/// named, or made.
fn frame_key(frame: u32) -> Vec<(u8, String)> {
    if frame == 0 {
        Vec::new()
    } else {
        vec![(b'r', frame.to_string())]
    }
}

/// One connection's graphics: the image table, the upload in flight, the
/// decodes awaiting their pixels, and the quota.
#[derive(Debug)]
pub struct GraphicsState {
    /// Every image holding quota: the addressable ones and those a
    /// retransmit replaced while scrollback still shows them.
    entries: HashMap<ImageUid, Entry>,
    /// The addressable images by client (or assigned) id.
    by_id: HashMap<u32, ImageUid>,
    reassembler: Reassembler,
    used_bytes: u64,
    /// The canvases of the queries the host is decoding, which store
    /// nothing but hold memory until they land: bounded by the quota like
    /// the images, on their own count, so a flood of compressed queries
    /// cannot park unbounded data at the host, and a transmit meanwhile —
    /// the query's own, as a client queries then transmits — is never
    /// refused or made to evict for a query still in flight.
    query_bytes: u64,
    quota: u64,
    next_assigned_id: u32,
    /// Decodes in flight, by the ticket their reply is parked under.
    parked: HashMap<DecodeTicket, Parked>,
    /// Orders creation and use.
    tick: u64,
    /// Virtual placements across every entry: while zero, no line can have
    /// a placeholder run, and the processor looks for none.
    virtual_count: usize,
    /// A command since [`Self::take_dirty`] changed what is drawn without a
    /// decode to announce it: an animation control or a frame delete.
    dirty: bool,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self::with_quota(DEFAULT_QUOTA_BYTES)
    }
}

impl GraphicsState {
    /// A state with the default quota.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A state storing at most `quota` decoded bytes.
    #[must_use]
    pub fn with_quota(quota: u64) -> Self {
        Self {
            entries: HashMap::new(),
            by_id: HashMap::new(),
            reassembler: Reassembler::default(),
            used_bytes: 0,
            query_bytes: 0,
            quota,
            next_assigned_id: FIRST_ASSIGNED_ID,
            parked: HashMap::new(),
            tick: 0,
            virtual_count: 0,
            dirty: false,
        }
    }

    /// Whether any virtual placement exists, so that Unicode placeholder
    /// cells could mean anything. The processor reads this on the print
    /// path's rare branch and before building a line.
    #[must_use]
    pub fn has_virtual_placements(&self) -> bool {
        self.virtual_count > 0
    }

    /// Whether a command since the last call changed what is drawn with no
    /// decode to announce it — an animation control, a frame delete — so
    /// the processor asks for a repaint.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The addressable image with client (or assigned) id `id`.
    #[must_use]
    pub fn image(&self, id: u32) -> Option<Arc<ImageRecord>> {
        self.by_id
            .get(&id)
            .and_then(|uid| self.entries.get(uid))
            .map(|entry| entry.record.clone())
    }

    /// The newest addressable image with `I=` number `number`.
    #[cfg(test)]
    fn newest_by_number(&self, number: u32) -> Option<Arc<ImageRecord>> {
        self.newest_uid_by_number(number)
            .and_then(|uid| self.entries.get(&uid))
            .map(|entry| entry.record.clone())
    }

    /// Decoded bytes counted against the quota.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// The canvases of the queries the host is decoding, held against the
    /// quota on their own count until they land.
    #[must_use]
    pub fn query_bytes(&self) -> u64 {
        self.query_bytes
    }

    /// Images holding quota, addressable or not.
    #[must_use]
    pub fn image_count(&self) -> usize {
        self.entries.len()
    }

    /// Whether a chunked upload is waiting for more chunks.
    #[must_use]
    pub fn upload_in_flight(&self) -> bool {
        self.reassembler.in_flight()
    }

    /// Decodes the host has not reported back yet. Not a bound: the host
    /// owns the work it was handed, and a pending image already holds its
    /// bytes against the quota.
    #[must_use]
    pub fn decodes_in_flight(&self) -> usize {
        self.parked.len()
    }

    /// Drop any chunked upload still waiting for more chunks, as a switch
    /// away from displaying graphics does: the chunks still to come are
    /// dropped before they reach here, and a switch back must not join
    /// them to a stale start.
    pub fn abandon_upload(&mut self) {
        if self.reassembler.abort().is_some() {
            log::debug!("graphics upload abandoned: graphics switched off");
        }
    }

    /// Run one whole graphics command. Replies go to `env.replies` in
    /// command order (a transmit's waits, parked, for its decode); a
    /// placement to show at the cursor is returned for the caller to attach.
    pub fn command(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        control: ControlData,
        payload: &[u8],
    ) -> Option<Arc<Placement>> {
        // A query exists only for its answer, which names the image id it
        // came with; kitty refuses one without, silently, image number or
        // not — before anything else, an upload in flight included.
        if control.action == Action::Query && control.image_id == 0 {
            log::debug!("graphics query without an image id ignored");
            return None;
        }
        if control.feature_query.is_some() && !control.is_feature_query() {
            log::debug!(
                "ignoring the e key on a graphics {:?} command",
                control.action
            );
        }
        // A feature query transmits nothing and leaves an upload in flight
        // alone.
        if control.is_feature_query() {
            answer_feature_query(env, &control);
            return None;
        }
        // While an upload is in flight, kitty treats every direct pixel
        // command as its next chunk and ignores its other keys.
        if self.reassembler.continues(&control) {
            return self.feed(env, control, payload);
        }
        let reply = ReplyTo::of(&control, env.replies_enabled);
        if let Err(err) = control.validate() {
            reply.answer(env.replies, Err(err));
            return None;
        }
        match control.action {
            Action::Transmit | Action::TransmitAndDisplay | Action::Query | Action::Frame => {
                if control.medium == Medium::Direct {
                    self.feed(env, control, payload)
                } else {
                    // A local-media command starts a new image load, which
                    // abandons any upload still in flight.
                    self.reassembler.abort();
                    self.load_media(env, control, payload, reply)
                }
            }
            Action::Put => self.put(env, &control, reply),
            Action::Delete => {
                self.reassembler.abort();
                self.delete(&control, env);
                None
            }
            Action::Animate => {
                self.animate(env, &control, reply);
                None
            }
            Action::Compose => {
                self.compose(env, &control, reply);
                None
            }
        }
    }

    /// A graphics APC too long to dispatch (`prefix` is what fit): a protocol
    /// violation, since a conforming chunk always fits. Any upload in flight
    /// is abandoned, and the command the prefix names is refused with
    /// `EINVAL` when it can be answered.
    pub fn truncated_command(&mut self, env: &mut GraphicsEnv<'_>, prefix: &[u8]) {
        let control = match parse_apc(prefix) {
            Some(Ok((control, _))) => Some(control),
            _ => None,
        };
        let reply = control.as_ref().and_then(|control| {
            if self.reassembler.continues(control) {
                self.upload_reply(control, env.replies_enabled)
            } else if control.action == Action::Delete {
                None
            } else {
                Some(ReplyTo::of(control, env.replies_enabled))
            }
        });
        if self.reassembler.abort().is_some() {
            log::debug!("graphics upload abandoned: an oversized command arrived");
        }
        if let Some(reply) = reply {
            reply.answer(
                env.replies,
                Err(GraphicsError::invalid(
                    "graphics command exceeds the size limit",
                )),
            );
        }
    }

    /// A decode the host was asked for has finished: land the pixels in
    /// their record (unless the record was evicted meanwhile, when the
    /// pixels are dropped and the reply still says the data arrived) and
    /// settle the parked reply. A query's decode lands nowhere: it only
    /// answers. A decode that succeeded answers with what its command
    /// still owed — `OK`, or the refusal of a placement it could not make.
    /// A frame or compose lands in its frame; either way the image's next
    /// queued frame command runs, through `host` when it needs a decode.
    pub fn decode_complete(
        &mut self,
        host: &mut dyn GraphicsHost,
        replies: &mut ReplyQueue,
        ticket: DecodeTicket,
        result: DecodeResult,
    ) {
        let Some(Parked {
            landing,
            landed,
            extra,
            local_media,
        }) = self.parked.remove(&ticket)
        else {
            if !replies.resolve(ticket, result.map(drop)) {
                log::debug!("graphics decode {ticket:?} finished for a reply no longer parked");
            }
            return;
        };
        // Local media the host read for the peer answers `EBADF` whatever
        // went wrong with it (see `read_verdict`); a placement refusal is a
        // verdict on the line, not the file, so it is not mapped.
        let verdict = |status: Result<(), GraphicsError>| {
            if local_media {
                status.map_err(|_| GraphicsError::bad_file())
            } else {
                status
            }
        };
        let (status, uid) = match landing {
            Landing::Nowhere { charged } => {
                self.query_bytes = self.query_bytes.saturating_sub(charged);
                (verdict(result.map(drop)), None)
            }
            Landing::Root(record) => (
                verdict(self.land(&record, result, replies)).and(landed),
                Some(record.uid),
            ),
            Landing::Frame {
                record,
                target,
                gap,
                charged,
            } => (
                verdict(self.land_frame(&record, target, gap, charged, result)).and(landed),
                Some(record.uid),
            ),
            Landing::Compose {
                record,
                destination,
            } => (
                land_compose(&record, destination, result).and(landed),
                Some(record.uid),
            ),
        };
        if !replies.resolve_with(ticket, status, extra) {
            log::debug!("graphics decode {ticket:?} finished for a reply no longer parked");
        }
        if let Some(uid) = uid {
            if let Some(entry) = self.entries.get_mut(&uid) {
                entry.busy = false;
            }
            self.dispatch(uid, host, replies);
        }
    }

    fn land(
        &mut self,
        record: &Arc<ImageRecord>,
        result: DecodeResult,
        replies: &mut ReplyQueue,
    ) -> Result<(), GraphicsError> {
        let result = check_canvas(record, result);
        if record.is_evicted() {
            // The slot was reclaimed while decoding; the data did arrive.
            return result.map(drop);
        }
        match result {
            Ok(pixels) => {
                record.set_data(ImageData::Ready(pixels));
                Ok(())
            }
            Err(err) => {
                record.set_data(ImageData::Failed(err.clone()));
                // A failed load never existed as far as the client knows.
                self.remove_entry(record.uid, replies);
                Err(err)
            }
        }
    }

    /// An `a=f`'s canvas has landed: appended or put in place of the frame
    /// it edits — unless the image went meanwhile, when the data still
    /// arrived and nothing holds it — or, failed, its charge released. The
    /// image stays either way, as in kitty.
    fn land_frame(
        &mut self,
        record: &Arc<ImageRecord>,
        target: FrameTarget,
        gap: Option<u32>,
        charged: u64,
        result: DecodeResult,
    ) -> Result<(), GraphicsError> {
        let result = check_canvas(record, result);
        if charged > 0
            && let Some(entry) = self.entries.get_mut(&record.uid)
        {
            entry.frames_coming = entry.frames_coming.saturating_sub(1);
        }
        match result {
            Ok(pixels) => {
                let stored = !record.is_evicted()
                    && match target {
                        FrameTarget::New => record
                            .push_frame(pixels, gap.unwrap_or(DEFAULT_FRAME_GAP_MS))
                            .is_some(),
                        FrameTarget::Edit(frame) => {
                            let stored = record.replace_frame(frame, pixels).is_some();
                            if let Some(gap) = gap.filter(|_| stored) {
                                record.set_frame_gap(frame, gap);
                            }
                            stored
                        }
                    };
                if !stored {
                    // Pixels nothing holds take no quota: the image's own
                    // removal released its charge, and an image that lost
                    // its frames some other way gives this one's back.
                    self.release(record.uid, charged);
                    log::debug!(
                        "graphics frame of image {} dropped: the image went or lost its frames meanwhile",
                        record.id
                    );
                }
                Ok(())
            }
            Err(err) => {
                self.release(record.uid, charged);
                Err(err)
            }
        }
    }

    /// Feed one chunk (or a whole unchunked command) to the reassembler and
    /// run the command it completes.
    fn feed(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        control: ControlData,
        payload: &[u8],
    ) -> Option<Arc<Placement>> {
        let reply = if self.reassembler.continues(&control) {
            self.upload_reply(&control, env.replies_enabled)
        } else {
            Some(ReplyTo::of(&control, env.replies_enabled))
        };
        match self.reassembler.feed(control, payload, self.payload_cap()) {
            Ok(Fed::Pending) => None,
            Ok(Fed::Complete(control, data)) => self.complete(env, &control, data),
            Err(err) => {
                self.reassembler.abort();
                if let Some(reply) = reply {
                    reply.answer(env.replies, Err(err));
                }
                None
            }
        }
    }

    /// The reply of the upload `chunk` continues: the first chunk's keys
    /// with the latest non-zero quiet level.
    fn upload_reply(&self, chunk: &ControlData, replies_enabled: bool) -> Option<ReplyTo> {
        let first = self.reassembler.pending_control()?;
        let quiet = if chunk.quiet == Quiet::Verbose {
            first.quiet
        } else {
            chunk.quiet
        };
        Some(ReplyTo::new(
            first.image_id,
            first.image_number,
            first.placement_id,
            echoed_frame(first),
            quiet,
            replies_enabled,
        ))
    }

    /// The most a joined payload may hold: more than the quota cannot be an
    /// image the quota admits.
    fn payload_cap(&self) -> usize {
        usize::try_from(self.quota.min(MAX_IMAGE_BYTES)).unwrap_or(usize::MAX)
    }

    /// Run a pixel command whose data is whole.
    fn complete(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        control: &ControlData,
        data: Vec<u8>,
    ) -> Option<Arc<Placement>> {
        let reply = ReplyTo::of(control, env.replies_enabled);
        match control.action {
            Action::Query => {
                self.query(env, control, data, reply);
                None
            }
            Action::Transmit | Action::TransmitAndDisplay => {
                self.transmit(env, control, data, reply)
            }
            Action::Frame => {
                self.frame_load(env, control, data, reply);
                None
            }
            Action::Put | Action::Delete | Action::Animate | Action::Compose => {
                log::debug!(
                    "graphics {:?} command carried pixel data; ignored",
                    control.action
                );
                None
            }
        }
    }

    /// `t=f`, `t=t`, `t=s`: the host reads the named file or shared memory
    /// (or, for a peer that may not, refuses with `EBADF`), and the command
    /// runs on what it read. Never silently dropped.
    fn load_media(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        mut control: ControlData,
        payload: &[u8],
        reply: ReplyTo,
    ) -> Option<Arc<Placement>> {
        let data = decode_payload(payload).and_then(|name| {
            env.host.read_media(
                control.medium,
                &name,
                control.data_offset,
                control.data_size,
            )
        });
        match data {
            Ok(data) => {
                control.more = false;
                self.complete(env, &control, data)
            }
            Err(err) => {
                reply.answer(env.replies, Err(err));
                None
            }
        }
    }

    /// `a=q` on whole data: everything a transmit would check, storing
    /// nothing. Uncompressed data is checked here and answered at once, and
    /// so is compressed data while its inflated size is known to be small
    /// ([`MAX_INLINE_INFLATE`]); past that the host inflates and decodes it
    /// off this thread as it would a transmit, with the reply parked
    /// meanwhile, the canvas it declared held against the quota until it
    /// lands (`ENOSPC` when the queries already in flight leave no room
    /// for it) and nothing stored when it does. A query whose answer could
    /// never go out — a connection that does not answer, or `q=2` — is
    /// dropped without a look: the answer is all a query is for.
    fn query(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        control: &ControlData,
        data: Vec<u8>,
        reply: ReplyTo,
    ) {
        if reply.quiet == Quiet::SuppressAll {
            return;
        }
        let admitted = self.probe_admitted(control, &data);
        let (width, height) = match admitted {
            Ok(dims) => dims,
            Err(err) => {
                reply.answer(env.replies, Err(err));
                return;
            }
        };
        if control.compression != Compression::Zlib {
            reply.answer(env.replies, Ok(()));
            return;
        }
        if let Some(status) = check_inflated(control, &data, width, height) {
            reply.answer(env.replies, status);
            return;
        }
        // More than this thread inflates: the host checks it as it would a
        // transmit's data, and the answer waits for its verdict. What the
        // host will hold for it — the canvas, as for a transmit — is
        // charged meanwhile, so the queries in flight are bounded like the
        // images; whether it fits is not for a local file's peer to measure.
        let charged = rgba_len(width, height);
        if self.query_bytes.saturating_add(charged) > self.quota.min(MAX_IMAGE_BYTES) {
            let err = GraphicsError::new(
                ErrorCode::NoSpace,
                "queries still decoding hold the storage quota",
            );
            reply.answer(env.replies, Err(read_verdict(control, err)));
            return;
        }
        self.query_bytes += charged;
        let ticket = env.replies.park(reply.response(Ok(())), reply.quiet);
        self.parked.insert(
            ticket,
            Parked {
                landing: Landing::Nowhere { charged },
                landed: Ok(()),
                extra: Vec::new(),
                local_media: control.medium != Medium::Direct,
            },
        );
        env.host.decode(
            ticket,
            DecodeRequest {
                format: control.format,
                compression: control.compression,
                width,
                height,
                data,
                compose: None,
            },
        );
    }

    /// `a=t`/`a=T` on whole data: create the record, hand the decode to the
    /// host with the reply parked behind it, and for `a=T` place it now. An
    /// `a=T` whose placement is refused (the line is full) still stores the
    /// image — as kitty keeps an image whose put fails — and answers with
    /// the refusal once the decode lands, so the client knows the tile is
    /// not shown and can put it elsewhere.
    fn transmit(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        control: &ControlData,
        data: Vec<u8>,
        mut reply: ReplyTo,
    ) -> Option<Arc<Placement>> {
        let displays = control.action == Action::TransmitAndDisplay;
        if !displays && control.image_id == 0 && control.image_number == 0 {
            log::debug!(
                "graphics transmit without an id or number dropped: nothing can address it"
            );
            return None;
        }
        let admitted = self.probe_admitted(control, &data);
        let (width, height) = match admitted {
            Ok(dims) => dims,
            Err(err) => {
                reply.answer(env.replies, Err(err));
                return None;
            }
        };
        let id = match (control.image_id, control.image_number) {
            (0, 0) => 0,
            (0, _) => self.mint_id(),
            (id, _) => id,
        };
        reply.id = id;
        if id == 0 {
            reply.placement = 0;
        }
        let record = Arc::new(ImageRecord::new(id, control.image_number, width, height));
        self.insert(record.clone(), env.replies);
        let (placement, landed) = if displays {
            match self.place(&record, control, env) {
                Ok(placement) => (placement, Ok(())),
                Err(err) => {
                    log::debug!("graphics image {id} stored but not placed: {err}");
                    (None, Err(err))
                }
            }
        } else {
            (None, Ok(()))
        };
        let ticket = env.replies.park(reply.response(Ok(())), reply.quiet);
        self.parked.insert(
            ticket,
            Parked {
                landing: Landing::Root(record.clone()),
                landed,
                extra: Vec::new(),
                local_media: control.medium != Medium::Direct,
            },
        );
        // Frame commands for the image wait for its root.
        if let Some(entry) = self.entries.get_mut(&record.uid) {
            entry.busy = true;
        }
        env.host.decode(
            ticket,
            DecodeRequest {
                format: control.format,
                compression: control.compression,
                width,
                height,
                data,
                compose: None,
            },
        );
        placement
    }

    /// [`probe`] the data and [`admit`](Self::admit) its size. Local media
    /// are refused in the file's one word either way: a PNG header the
    /// quota cannot hold is a fact about the file (see [`read_verdict`]).
    fn probe_admitted(
        &self,
        control: &ControlData,
        data: &[u8],
    ) -> Result<(u32, u32), GraphicsError> {
        let (width, height) = probe(control, data)?;
        self.admit(width, height)
            .map_err(|err| read_verdict(control, err))?;
        Ok((width, height))
    }

    /// Refuse an image the quota could never hold.
    fn admit(&self, width: u32, height: u32) -> Result<(), GraphicsError> {
        if rgba_len(width, height) > self.quota.min(MAX_IMAGE_BYTES) {
            return Err(GraphicsError::new(
                ErrorCode::NoSpace,
                "image is larger than the storage quota",
            ));
        }
        Ok(())
    }

    /// A free terminal-assigned id.
    fn mint_id(&mut self) -> u32 {
        loop {
            let id = self.next_assigned_id;
            self.next_assigned_id = id.checked_add(1).unwrap_or(FIRST_ASSIGNED_ID);
            if !self.by_id.contains_key(&id) {
                return id;
            }
        }
    }

    /// Store a new record, replacing an older image with its id and evicting
    /// until the quota holds it.
    fn insert(&mut self, record: Arc<ImageRecord>, replies: &mut ReplyQueue) {
        if record.id != 0
            && let Some(old) = self.by_id.remove(&record.id)
        {
            // The old image stops being addressable, and its virtual
            // placements go with its id: placeholders printed from now on
            // name the new image, which has none until the client makes
            // one. Placements in scrollback (placeholder runs among them)
            // keep showing it, so it keeps its quota until eviction claims
            // it; with none, it goes now.
            let orphaned = self.entries.get_mut(&old).map(|entry| {
                self.virtual_count -= entry.retire_virtuals();
                !entry.has_live_placements()
            });
            if orphaned == Some(true) {
                self.evict(old, replies);
            }
        }
        let bytes = record.decoded_len();
        self.make_room(bytes, None, replies);
        self.tick += 1;
        if record.id != 0 {
            self.by_id.insert(record.id, record.uid);
        }
        self.used_bytes += bytes;
        let mut entry = Entry::new(record, self.tick);
        entry.charged = bytes;
        self.entries.insert(entry.record.uid, entry);
    }

    /// Evict until `bytes` more fit the quota — and, for a new image
    /// (`keep` is `None`), the image count; for a frame of the image `keep`,
    /// which is never evicted here, only the bytes. Images no placement
    /// shows go first, least recently used first within each group. One
    /// sort, however many go — and once a sweep is due it frees a sixteenth
    /// of the quota (and of the count) beyond what is needed, so a session
    /// sitting at the quota does not sort every entry on every transmit:
    /// the next sixteenth's worth of transmits sweep nothing. (`admit`
    /// keeps the full quota: an image the full quota holds is never refused
    /// for the margin.) Whether the bytes fit now: they always do for a new
    /// image `admit` passed, and may not for a frame of an image that holds
    /// most of the quota itself.
    fn make_room(&mut self, bytes: u64, keep: Option<ImageUid>, replies: &mut ReplyQueue) -> bool {
        let fits = |state: &Self, quota: u64, images: usize| {
            state.used_bytes + bytes <= quota && (keep.is_some() || state.entries.len() < images)
        };
        if fits(self, self.quota, MAX_IMAGES) {
            return true;
        }
        let (quota, images) = (self.quota - self.quota / 16, MAX_IMAGES - MAX_IMAGES / 16);
        let mut order: Vec<(bool, u64, ImageUid)> = self
            .entries
            .iter()
            .filter(|(uid, _)| Some(**uid) != keep)
            .map(|(uid, entry)| (entry.has_live_placements(), entry.used, *uid))
            .collect();
        order.sort_unstable();
        for (_, _, uid) in order {
            if fits(self, quota, images) {
                break;
            }
            self.evict(uid, replies);
        }
        fits(self, self.quota, MAX_IMAGES)
    }

    /// Hold `bytes` more against the quota for a frame of image `uid`,
    /// evicting other images as needed.
    ///
    /// # Errors
    /// `ENOSPC` when they do not fit even then.
    fn charge(
        &mut self,
        uid: ImageUid,
        bytes: u64,
        replies: &mut ReplyQueue,
    ) -> Result<(), GraphicsError> {
        if !self.make_room(bytes, Some(uid), replies) {
            return Err(GraphicsError::new(
                ErrorCode::NoSpace,
                "Cache size exceeded cannot add new frames",
            ));
        }
        let entry = self
            .entries
            .get_mut(&uid)
            .expect("the image charged for is never evicted to make room");
        entry.charged += bytes;
        self.used_bytes += bytes;
        Ok(())
    }

    /// Release `bytes` of image `uid`'s hold on the quota: a frame that
    /// went, or failed to land. Nothing for an image already gone, whose
    /// removal released everything.
    fn release(&mut self, uid: ImageUid, bytes: u64) {
        if let Some(entry) = self.entries.get_mut(&uid) {
            let bytes = bytes.min(entry.charged);
            entry.charged -= bytes;
            self.used_bytes = self.used_bytes.saturating_sub(bytes);
        }
    }

    /// Drop an image's pixels for good and release its quota.
    fn evict(&mut self, uid: ImageUid, replies: &mut ReplyQueue) {
        if let Some(entry) = self.remove_entry(uid, replies) {
            entry.record.set_data(ImageData::Evicted);
        }
    }

    /// Take an image out of the table and the quota, leaving its record's
    /// data as it is. The frame commands still queued for it are answered
    /// as those in flight for an evicted image are — the data arrived —
    /// unless the image's own load failed, when it never existed for the
    /// client and they are `ENOENT`.
    fn remove_entry(&mut self, uid: ImageUid, replies: &mut ReplyQueue) -> Option<Entry> {
        let mut entry = self.entries.remove(&uid)?;
        self.used_bytes = self.used_bytes.saturating_sub(entry.charged);
        self.virtual_count = self.virtual_count.saturating_sub(entry.virtuals.len());
        if self.by_id.get(&entry.record.id) == Some(&uid) {
            self.by_id.remove(&entry.record.id);
        }
        let status = match entry.record.data() {
            ImageData::Failed(_) => Err(GraphicsError::new(
                ErrorCode::NotFound,
                format!(
                    "Animation command refers to non-existent image with id: {} and number: {}",
                    entry.record.id, entry.record.number
                ),
            )),
            _ => Ok(()),
        };
        for op in entry.queue.drain(..) {
            if let Some(ticket) = op.ticket() {
                replies.resolve(ticket, status.clone());
            }
        }
        Some(entry)
    }

    /// The next creation/use tick.
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn newest_uid_by_number(&self, number: u32) -> Option<ImageUid> {
        if number == 0 {
            return None;
        }
        self.by_id
            .values()
            .filter_map(|uid| self.entries.get(uid))
            .filter(|entry| entry.record.number == number)
            .max_by_key(|entry| entry.created)
            .map(|entry| entry.record.uid)
    }

    /// Create a placement of `record`. `Ok(Some)` is a placement to attach
    /// at the cursor — over its box's cells, or as an anchor over none
    /// ([`Placement::anchored`]). `Ok(None)` places nothing without refusing
    /// anything: a virtual placement (`U=1`) waits for placeholders, a
    /// relative placement (`P`) joins its parent's children, and a source
    /// rectangle that misses the image covers no cells, as in kitty. The
    /// line at the cursor having no room for one more object is a refusal,
    /// made before anything is registered.
    ///
    /// # Errors
    /// `ENOSPC` when the line is full; `EINVAL`, `ENOPARENT`, `ECYCLE` and
    /// `ETOODEEP` as the specification has them for relative placements.
    fn place(
        &mut self,
        record: &Arc<ImageRecord>,
        control: &ControlData,
        env: &GraphicsEnv<'_>,
    ) -> Result<Option<Arc<Placement>>, GraphicsError> {
        // A placement id means nothing without an image id to scope it.
        let id = if record.id == 0 {
            0
        } else {
            control.placement_id
        };
        if control.unicode_placeholder {
            if control.parent_image != 0 {
                return Err(GraphicsError::invalid(
                    "Put command creating a virtual placement cannot refer to a parent",
                ));
            }
            self.place_virtual(record, id, control);
            return Ok(None);
        }
        let geometry = geometry(control, record.width, record.height, env.cell);
        if control.parent_image != 0 {
            self.place_relative(record, id, control, geometry)?;
            return Ok(None);
        }
        if geometry.columns == 0 || geometry.rows == 0 {
            log::debug!(
                "graphics placement of image {} shows nothing: its source rectangle is empty",
                record.id
            );
            return Ok(None);
        }
        if env.objects_free == 0 {
            return Err(GraphicsError::new(
                ErrorCode::NoSpace,
                "too many placements on this line",
            ));
        }
        adoption_fits(0, self.replaced_by(record, id).as_ref())?;
        // `C=1` leaves the cursor in place; a negative z-index is drawn
        // under the text, which it must therefore not displace.
        let anchored = control.cursor_movement == 1 || control.z < 0;
        let placement = Arc::new(Placement {
            anchored,
            line_seq: env.line_seq,
            column: env.column,
            created: self.next_tick(),
            ..Placement::new(record.clone(), id, geometry)
        });
        self.register(record.uid, &placement);
        Ok(Some(placement))
    }

    /// `a=p,U=1`: a virtual placement of `record`, superseding the one with
    /// the same id, whose relative placements move to it. A real placement
    /// with that id goes: the two share the id space, as kitty keeps one
    /// ref per client id.
    fn place_virtual(&mut self, record: &Arc<ImageRecord>, id: u32, control: &ControlData) {
        let placement = Arc::new(VirtualPlacement {
            image: record.clone(),
            id,
            columns: control.columns,
            rows: control.rows,
            children: RwLock::new(Vec::new()),
            runs: Mutex::new(Vec::new()),
            created: self.next_tick(),
            deleted: AtomicBool::new(false),
        });
        let tick = self.tick;
        let Some(entry) = self.entries.get_mut(&record.uid) else {
            return;
        };
        entry.used = tick;
        if id != 0
            && let Some(real) = entry.named.remove(&id).and_then(|weak| weak.upgrade())
        {
            real.mark_deleted();
        }
        match entry.add_virtual(placement.clone()) {
            Some(old) => adopt(
                &old.children,
                &placement.children,
                &ParentLink::Virtual(Arc::downgrade(&placement)),
            ),
            None => self.virtual_count += 1,
        }
    }

    /// `a=p,P=<image>,Q=<placement>`: a placement of `record` relative to
    /// its parent, registered under its own image and in the parent's
    /// children, never attached to a line.
    fn place_relative(
        &mut self,
        record: &Arc<ImageRecord>,
        id: u32,
        control: &ControlData,
        geometry: PlacementGeometry,
    ) -> Result<(), GraphicsError> {
        let parent = self.parent_placement(control)?;
        let replaced = self.replaced_by(record, id);
        let self_parent =
            || GraphicsError::invalid("Put command refers to itself as its own parent");
        match &parent {
            // The placement this one replaces keeps its place in any
            // chain, so a parent that descends from it would close a loop.
            Parent::Real(real) => {
                if let Some(old) = &replaced {
                    if Arc::ptr_eq(old, real) {
                        return Err(self_parent());
                    }
                    if real.is_or_descends_from(old) {
                        return Err(GraphicsError::new(
                            ErrorCode::Cycle,
                            "This parent reference creates a cycle",
                        ));
                    }
                }
            }
            // A real placement takes the id of a virtual one of its image.
            Parent::Virtual(virt) => {
                if id != 0 && virt.id == id && virt.image.uid == record.uid {
                    return Err(self_parent());
                }
            }
        }
        let depth = parent.child_depth();
        if depth > PLACEMENT_DEPTH_LIMIT {
            return Err(too_deep());
        }
        adoption_fits(depth, replaced.as_ref())?;
        // A parent's children are capped like a line's objects: a relative
        // placement is never attached to a line, so nothing else bounds
        // them. One replacing a sibling takes its place.
        let replacing_sibling = replaced
            .as_ref()
            .is_some_and(|old| parent.is_parent_of(old));
        let siblings = parent
            .children()
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
            .saturating_sub(usize::from(replacing_sibling));
        if siblings >= MAX_INLINE_OBJECTS {
            return Err(GraphicsError::new(
                ErrorCode::NoSpace,
                "too many placements relative to this one",
            ));
        }
        let offset = RelativeOffset {
            columns: control.offset_columns,
            rows: control.offset_rows,
        };
        let placement = Arc::new(Placement {
            anchored: true,
            relative: Some(offset),
            created: self.next_tick(),
            ..Placement::new(record.clone(), id, geometry)
        });
        placement.set_parent(parent.link());
        parent
            .children()
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(placement.clone());
        self.register(record.uid, &placement);
        Ok(())
    }

    /// The live placement with id `id` of `record`, which a new one with
    /// that id replaces.
    fn replaced_by(&self, record: &ImageRecord, id: u32) -> Option<Arc<Placement>> {
        (id != 0)
            .then(|| self.entries.get(&record.uid))
            .flatten()
            .and_then(|entry| entry.named.get(&id))
            .and_then(Weak::upgrade)
            .filter(|old| old.is_live())
    }

    /// The parent a relative placement names: `Q` on image `P`, real or
    /// virtual, or with no `Q` the oldest placement of `P` (kitty takes its
    /// first). Placeholder runs are no placements to the protocol and are
    /// passed over.
    ///
    /// # Errors
    /// `ENOPARENT` when the image or placement does not exist.
    fn parent_placement(&self, control: &ControlData) -> Result<Parent, GraphicsError> {
        let (image, placement) = (control.parent_image, control.parent_placement);
        let Some(entry) = self.by_id.get(&image).and_then(|uid| self.entries.get(uid)) else {
            return Err(GraphicsError::new(
                ErrorCode::NoParent,
                format!(
                    "Put command refers to a parent image with id: {image} that does not exist"
                ),
            ));
        };
        if placement != 0 {
            if let Some(parent) = entry
                .named
                .get(&placement)
                .and_then(Weak::upgrade)
                .filter(|p| p.is_live())
            {
                return Ok(Parent::Real(parent));
            }
            if let Some(virt) = entry.virtual_for(placement) {
                return Ok(Parent::Virtual(virt.clone()));
            }
            return Err(GraphicsError::new(
                ErrorCode::NoParent,
                format!(
                    "Put command refers to a parent image placement with id: {image} and placement id: {placement} that does not exist"
                ),
            ));
        }
        let real = entry
            .live_placements()
            .filter(|p| !p.placeholder)
            .min_by_key(|p| p.created);
        let virt = entry.virtuals.iter().min_by_key(|v| v.created);
        match (real, virt) {
            (Some(real), Some(virt)) if virt.created < real.created => {
                Ok(Parent::Virtual(virt.clone()))
            }
            (Some(real), _) => Ok(Parent::Real(real)),
            (None, Some(virt)) => Ok(Parent::Virtual(virt.clone())),
            (None, None) => Err(GraphicsError::new(
                ErrorCode::NoParent,
                format!(
                    "Put command refers to a parent image with id: {image} that has no placements"
                ),
            )),
        }
    }

    /// Register a placement with its image, retiring the live placement
    /// with the same id: that one's relative placements move to the new
    /// one, which stands where it stood in any chain (the caller has
    /// checked that the chain below fits, [`adoption_fits`]), and it leaves
    /// its own parent's children. A virtual placement with the id goes
    /// with its relative placements: the two kinds share the id space.
    fn register(&mut self, uid: ImageUid, placement: &Arc<Placement>) {
        let tick = self.tick;
        let Some(entry) = self.entries.get_mut(&uid) else {
            return;
        };
        entry.used = tick;
        if placement.id != 0
            && let Some(virt) = entry.take_virtual(placement.id)
        {
            self.virtual_count -= 1;
            virt.retire();
        }
        if let Some(old) = entry.add(placement) {
            adopt(
                &old.children,
                &placement.children,
                &ParentLink::Real(Arc::downgrade(placement)),
            );
            old.mark_deleted();
        }
    }

    /// A placement for a run of Unicode placeholder cells: the slice of the
    /// virtual placement's box the run's cells show, laid out with `env`'s
    /// cell, to attach over the run's text. `None` when the run names no
    /// image or virtual placement this state holds, its cells fall outside
    /// the image, or the line has no room: the cells stay text. The byte
    /// range handed back is the part of the run the placement covers, which
    /// drops cells left blank by the box's letterboxing.
    #[must_use]
    pub fn placeholder_run(
        &mut self,
        run: &PlaceholderRun,
        env: &GraphicsEnv<'_>,
    ) -> Option<(Arc<Placement>, Range<usize>)> {
        if run.is_empty() || env.objects_free == 0 {
            return None;
        }
        let uid = *self.by_id.get(&run.image_id)?;
        let entry = self.entries.get(&uid)?;
        let virt = entry.virtual_for(run.placement_id)?.clone();
        let record = entry.record.clone();
        let (geometry, skip, count) = run_geometry(
            (virt.columns, virt.rows),
            (record.width, record.height),
            env.cell,
            (run.row, run.column),
            run.len(),
        )?;
        let column = run.first_cell_column.saturating_add(skip);
        // The run that shows the box's top-left cell draws the virtual
        // placement's relative placements from it (`VirtualPlacement::children`).
        let draws_children = run.row == 0 && run.column == 0 && skip == 0;
        let placement = Arc::new(Placement {
            placeholder: true,
            line_seq: env.line_seq,
            column: u16::try_from(column).unwrap_or(u16::MAX),
            created: self.next_tick(),
            draws_children_of: draws_children.then(|| Arc::downgrade(&virt)),
            ..Placement::new(record, 0, geometry)
        });
        virt.add_run(&placement);
        self.register(uid, &placement);
        Some((placement, run.byte_range(skip, count)))
    }

    /// `a=p`: place a stored image at the cursor, answering at once. An
    /// image still decoding is placed all the same and shows when it lands.
    fn put(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        control: &ControlData,
        mut reply: ReplyTo,
    ) -> Option<Arc<Placement>> {
        let uid = match (control.image_id, control.image_number) {
            (0, 0) => {
                log::debug!("graphics put without an image id or number ignored");
                return None;
            }
            (0, number) => self.newest_uid_by_number(number),
            (id, _) => self.by_id.get(&id).copied(),
        };
        let Some(record) = uid
            .and_then(|uid| self.entries.get(&uid))
            .map(|entry| entry.record.clone())
        else {
            reply.answer(
                env.replies,
                Err(GraphicsError::new(
                    ErrorCode::NotFound,
                    format!(
                        "Put command refers to non-existent image with id: {} and number: {}",
                        control.image_id, control.image_number
                    ),
                )),
            );
            return None;
        };
        reply.id = record.id;
        match self.place(&record, control, env) {
            Ok(placement) => {
                reply.answer(env.replies, Ok(()));
                placement
            }
            Err(err) => {
                reply.answer(env.replies, Err(err));
                None
            }
        }
    }

    /// `a=d`: never answered. Lowercase targets remove placements;
    /// uppercase ones also free each image a removed placement showed and
    /// no placement shows any more — the images of relative placements
    /// deleted with their parents among them, never an image the delete
    /// did not reach (kitty frees only what it unplaced). The id targets
    /// (`i`, `n`, `r`) take virtual placements too; the positional ones and
    /// `a` never do, nor the runs made from them (see the module doc for
    /// the screen they are read against). `d=f` removes a frame instead
    /// ([`Self::delete_frames`]).
    fn delete(&mut self, control: &ControlData, env: &mut GraphicsEnv<'_>) {
        let (target, free) = match control.delete_target() {
            Ok(target) => target,
            Err(err) => {
                log::debug!("graphics delete ignored: {err}");
                return;
            }
        };
        // Images whose placements went, to free if nothing shows them now.
        let mut touched: Vec<ImageUid> = Vec::new();
        match target {
            DeleteTarget::ByImageId | DeleteTarget::ByImageNumber | DeleteTarget::IdRange => {
                let targets: Vec<ImageUid> = match target {
                    DeleteTarget::ByImageId => self
                        .by_id
                        .get(&control.image_id)
                        .copied()
                        .into_iter()
                        .collect(),
                    DeleteTarget::ByImageNumber => self
                        .newest_uid_by_number(control.image_number)
                        .into_iter()
                        .collect(),
                    _ => self
                        .by_id
                        .iter()
                        .filter(|(id, _)| (control.x..=control.y).contains(id))
                        .map(|(_, uid)| *uid)
                        .collect(),
                };
                let only = match target {
                    DeleteTarget::IdRange => 0,
                    _ => control.placement_id,
                };
                for uid in targets {
                    if let Some(entry) = self.entries.get_mut(&uid) {
                        let virtuals = entry.delete_placements(only, &mut touched);
                        self.virtual_count -= virtuals;
                        touched.push(uid);
                    }
                }
            }
            DeleteTarget::Frames => {
                self.delete_frames(control, env, free);
                return;
            }
            _ => {
                let screen = Screen::new(env);
                for (uid, entry) in &self.entries {
                    let mut hit = false;
                    for placement in entry.live_placements() {
                        if !placement.placeholder
                            && screen.matches(target, control, env, &placement)
                        {
                            placement.mark_deleted_into(&mut touched);
                            hit = true;
                        }
                    }
                    if hit {
                        touched.push(*uid);
                    }
                }
            }
        }
        if free {
            self.free_touched(touched, env.replies);
        }
    }

    /// Free every image in `touched` that no placement shows any more.
    fn free_touched(&mut self, mut touched: Vec<ImageUid>, replies: &mut ReplyQueue) {
        touched.sort_unstable();
        touched.dedup();
        for uid in touched {
            if self
                .entries
                .get(&uid)
                .is_some_and(|entry| !entry.has_live_placements())
            {
                self.evict(uid, replies);
            }
        }
    }
}

/// The animation commands. Each runs in wire order with the others on its
/// image: at once when nothing of the image's is decoding, else queued
/// behind what is (see the module doc).
impl GraphicsState {
    /// The addressable image a frame command names: by id, or the newest
    /// with its number. `None` for a command naming neither, which kitty
    /// drops without a reply. The image counts as used, for eviction order:
    /// one being animated is in use as a placed one is.
    fn image_uid_for(&mut self, control: &ControlData) -> Option<ImageUid> {
        let uid = match (control.image_id, control.image_number) {
            (0, 0) => None,
            (0, number) => self.newest_uid_by_number(number),
            (id, _) => self.by_id.get(&id).copied(),
        }?;
        let tick = self.next_tick();
        self.entries.get_mut(&uid)?.used = tick;
        Some(uid)
    }

    /// `a=f` on whole data: check what the command alone decides — the
    /// image exists, the data's rectangle (`x`, `y`, `s`, `v`, or the PNG's
    /// size) lies inside it, the queue has room, the frame cap and the
    /// quota admit a new frame and the payload while it waits — park the
    /// reply and queue the load; it runs when the image has nothing
    /// decoding ([`Self::run_load`]). `r` naming a frame edits it (then no
    /// frame bytes are charged); absent or past the end, a new frame is
    /// appended, as kitty does. The reply echoes `i`/`I` as the command
    /// gave them with the last non-zero quiet level, plus `r=` for the
    /// frame.
    fn frame_load(
        &mut self,
        env: &mut GraphicsEnv<'_>,
        control: &ControlData,
        data: Vec<u8>,
        mut reply: ReplyTo,
    ) {
        if control.image_id == 0 && control.image_number == 0 {
            log::debug!("graphics frame without an image id or number dropped");
            return;
        }
        let Some(uid) = self.image_uid_for(control) else {
            reply.answer(env.replies, Err(not_found(control)));
            return;
        };
        let entry = &self.entries[&uid];
        let record = entry.record.clone();
        reply.id = record.id;
        let (width, height) = match probe(control, &data) {
            Ok(size) => size,
            Err(err) => {
                reply.answer(env.replies, Err(err));
                return;
            }
        };
        if let Err(err) = inside(&record, (control.x, control.y), (width, height), "frame") {
            // The rectangle a PNG's own header gives is a fact about the
            // file: a local one is refused in the file's one word.
            reply.answer(env.replies, Err(read_verdict(control, err)));
            return;
        }
        // A load that waits behind the image's decode takes a bounded
        // place in the queue and holds its payload against the quota until
        // it runs; one that runs now hands the data straight to the host.
        let waits = entry.waits();
        if waits && entry.queue.len() >= MAX_QUEUED_FRAME_OPS {
            reply.answer(env.replies, Err(queue_full(&record)));
            return;
        }
        let held = if waits {
            u64::try_from(data.len()).unwrap_or(u64::MAX)
        } else {
            0
        };
        // Whether `r` names a frame is settled when the load runs; the
        // frames admitted before it say what to charge for now.
        let expected = entry.frames_expected();
        let adds =
            !usize::try_from(control.rows).is_ok_and(|frame| (1..=expected).contains(&frame));
        let mut charged = 0;
        if adds {
            if expected >= MAX_FRAMES {
                reply.answer(env.replies, Err(frame_limit()));
                return;
            }
            charged = record.decoded_len();
        }
        if charged + held > 0
            && let Err(err) = self.charge(uid, charged + held, env.replies)
        {
            // `held` is the file's length: whether it fits what is left of
            // the quota is not for the peer to measure.
            reply.answer(env.replies, Err(read_verdict(control, err)));
            return;
        }
        let ticket = env.replies.park(reply.response(Ok(())), reply.quiet);
        let entry = self
            .entries
            .get_mut(&uid)
            .expect("the image loaded into is never evicted to make room");
        entry.frames_coming += usize::from(adds);
        entry.queue.push_back(FrameOp::Load(FrameLoad {
            ticket,
            format: control.format,
            compression: control.compression,
            width,
            height,
            x: control.x,
            y: control.y,
            data,
            frame: control.rows,
            base: control.columns,
            mode: compose_mode(control.x_offset),
            background: control.y_offset,
            gap: frame_gap(control.z),
            charged,
            held,
            local_media: control.medium != Medium::Direct,
        }));
        self.dispatch(uid, env.host, env.replies);
    }

    /// `a=c`: check the image and the rectangles (`w`, `h` default to the
    /// image's size; the source at `X`, `Y`, the destination at `x`, `y`;
    /// `EINVAL` outside the image, or overlapping within one frame) and
    /// the queue's room, park the reply and queue the compose; the frames
    /// are looked up when it runs ([`Self::run_compose`]). The reply is
    /// `OK` once the destination frame is replaced.
    fn compose(&mut self, env: &mut GraphicsEnv<'_>, control: &ControlData, mut reply: ReplyTo) {
        if control.image_id == 0 && control.image_number == 0 {
            log::debug!("graphics compose without an image id or number dropped");
            return;
        }
        let Some(uid) = self.image_uid_for(control) else {
            reply.answer(env.replies, Err(not_found(control)));
            return;
        };
        let record = self.entries[&uid].record.clone();
        reply.id = record.id;
        let (source, destination) = (control.rows, control.columns);
        if source == 0 || destination == 0 {
            let (which, frame) = if source == 0 {
                ("source", source)
            } else {
                ("destination", destination)
            };
            reply.answer(env.replies, Err(missing_frame(&record, which, frame)));
            return;
        }
        let width = if control.w == 0 {
            record.width
        } else {
            control.w
        };
        let height = if control.h == 0 {
            record.height
        } else {
            control.h
        };
        let source_at = (control.x_offset, control.y_offset);
        let destination_at = (control.x, control.y);
        let checked = inside(&record, destination_at, (width, height), "destination")
            .and_then(|()| inside(&record, source_at, (width, height), "source"));
        if let Err(err) = checked {
            reply.answer(env.replies, Err(err));
            return;
        }
        if source == destination && overlaps(source_at, destination_at, (width, height)) {
            reply.answer(
                env.replies,
                Err(GraphicsError::invalid(
                    "The source and destination rectangles overlap and the src and destination frames are the same",
                )),
            );
            return;
        }
        let entry = self.entries.get_mut(&uid).expect("looked up a moment ago");
        if entry.waits() && entry.queue.len() >= MAX_QUEUED_FRAME_OPS {
            reply.answer(env.replies, Err(queue_full(&record)));
            return;
        }
        let ticket = env.replies.park(reply.response(Ok(())), reply.quiet);
        entry.queue.push_back(FrameOp::Compose(FrameCompose {
            ticket,
            source,
            destination,
            width,
            height,
            source_at,
            destination_at,
            mode: compose_mode(control.cursor_movement),
        }));
        self.dispatch(uid, env.host, env.replies);
    }

    /// `a=a`: answered only when it fails — `ENOENT` for a missing image —
    /// never with `OK`, as kitty answers it: a client waiting for one would
    /// hang in kitty, so none does, and an unexpected `OK` could be taken
    /// for the next command's. One that finds the image's queue full is
    /// dropped. What it sets, [`ImageRecord::animate`].
    fn animate(&mut self, env: &mut GraphicsEnv<'_>, control: &ControlData, reply: ReplyTo) {
        if control.image_id == 0 && control.image_number == 0 {
            log::debug!("graphics animation control without an image id or number dropped");
            return;
        }
        let Some(uid) = self.image_uid_for(control) else {
            reply.answer(env.replies, Err(not_found(control)));
            return;
        };
        let entry = self.entries.get_mut(&uid).expect("looked up a moment ago");
        if entry.waits() {
            if entry.queue.len() >= MAX_QUEUED_FRAME_OPS {
                log::debug!(
                    "graphics animation control of image {} dropped: {MAX_QUEUED_FRAME_OPS} frame commands wait already",
                    entry.record.id
                );
                return;
            }
            entry.queue.push_back(FrameOp::Control(control.clone()));
        } else {
            self.run_control(uid, control);
        }
    }

    /// `d=f` / `d=F` by `i` or `I`: remove frame `r` (clamped to the frames
    /// the image has; absent, the root), as kitty does. A still image
    /// keeps its one frame; `d=F` on one deletes the image, placements and
    /// data, where `d=f` does nothing. Never answered; an image that does
    /// not exist is ignored, and so is a delete that finds the image's
    /// queue full.
    fn delete_frames(&mut self, control: &ControlData, env: &mut GraphicsEnv<'_>, free: bool) {
        let Some(uid) = self.image_uid_for(control) else {
            log::debug!("graphics frame delete of an image that does not exist ignored");
            return;
        };
        let entry = self.entries.get_mut(&uid).expect("looked up a moment ago");
        if entry.waits() {
            if entry.queue.len() >= MAX_QUEUED_FRAME_OPS {
                log::debug!(
                    "graphics frame delete of image {} dropped: {MAX_QUEUED_FRAME_OPS} frame commands wait already",
                    entry.record.id
                );
                return;
            }
            entry.queue.push_back(FrameOp::DeleteFrame {
                frame: control.rows,
                free,
            });
        } else {
            self.run_delete_frame(uid, control.rows, free, env.replies);
        }
    }

    /// Run image `uid`'s queued frame commands until one needs a decode
    /// (then it is in flight and the rest wait for it) or none are left.
    fn dispatch(&mut self, uid: ImageUid, host: &mut dyn GraphicsHost, replies: &mut ReplyQueue) {
        loop {
            let Some(entry) = self.entries.get_mut(&uid) else {
                return;
            };
            if entry.busy {
                return;
            }
            let Some(op) = entry.queue.pop_front() else {
                return;
            };
            let decoding = match op {
                FrameOp::Load(load) => self.run_load(uid, load, host, replies),
                FrameOp::Compose(compose) => self.run_compose(uid, compose, host, replies),
                FrameOp::Control(control) => {
                    self.run_control(uid, &control);
                    false
                }
                FrameOp::DeleteFrame { frame, free } => {
                    self.run_delete_frame(uid, frame, free, replies);
                    false
                }
            };
            if decoding {
                return;
            }
        }
    }

    /// Run a queued load: give back what its payload held while it waited,
    /// settle which frame it is now that everything before it has landed
    /// (and the charge the admission guessed at), find its base, and hand
    /// the decode to the host. `true` when it is in flight; `false` when it
    /// was refused — a base frame the image does not have is `ENOENT` —
    /// and the next command may run.
    fn run_load(
        &mut self,
        uid: ImageUid,
        mut load: FrameLoad,
        host: &mut dyn GraphicsHost,
        replies: &mut ReplyQueue,
    ) -> bool {
        let Some(entry) = self.entries.get(&uid) else {
            replies.resolve(load.ticket, Ok(()));
            return false;
        };
        let record = entry.record.clone();
        // The payload leaves the queue for the host, which owns it from here.
        self.release(uid, load.held);
        let count = record.frame_count();
        let (target, number) = match usize::try_from(load.frame) {
            Ok(frame) if (1..=count).contains(&frame) => {
                (FrameTarget::Edit(load.frame), load.frame)
            }
            _ => (
                FrameTarget::New,
                u32::try_from(count + 1).unwrap_or(u32::MAX),
            ),
        };
        let extra = frame_key(number);
        match target {
            FrameTarget::New if load.charged == 0 => {
                if count >= MAX_FRAMES {
                    return self.refuse_load(uid, &load, replies, frame_limit(), extra);
                }
                let bytes = record.decoded_len();
                if let Err(err) = self.charge(uid, bytes, replies) {
                    return self.refuse_load(uid, &load, replies, err, extra);
                }
                load.charged = bytes;
                self.entries
                    .get_mut(&uid)
                    .expect("kept by the charge")
                    .frames_coming += 1;
            }
            FrameTarget::Edit(_) if load.charged > 0 => {
                self.release(uid, load.charged);
                load.charged = 0;
                if let Some(entry) = self.entries.get_mut(&uid) {
                    entry.frames_coming = entry.frames_coming.saturating_sub(1);
                }
            }
            FrameTarget::New | FrameTarget::Edit(_) => {}
        }
        let base = match target {
            FrameTarget::Edit(frame) => frame,
            FrameTarget::New => load.base,
        };
        let base = if base == 0 {
            None
        } else if let Some(frame) = record.frame(base) {
            Some(frame.pixels)
        } else {
            let err = GraphicsError::new(
                ErrorCode::NotFound,
                format!("No frame with number: {base} found"),
            );
            return self.refuse_load(uid, &load, replies, err, extra);
        };
        let request = DecodeRequest {
            format: load.format,
            compression: load.compression,
            width: load.width,
            height: load.height,
            data: load.data,
            compose: Some(Compose {
                base,
                source: None,
                background: load.background,
                x: load.x,
                y: load.y,
                mode: load.mode,
                canvas: (record.width, record.height),
            }),
        };
        self.parked.insert(
            load.ticket,
            Parked {
                landing: Landing::Frame {
                    record,
                    target,
                    gap: load.gap,
                    charged: load.charged,
                },
                landed: Ok(()),
                extra,
                local_media: load.local_media,
            },
        );
        self.entries.get_mut(&uid).expect("kept by the charge").busy = true;
        host.decode(load.ticket, request);
        true
    }

    /// Refuse a queued load as it runs: release what it was charged and
    /// answer `err`. Always `false`, for [`Self::run_load`] to return.
    fn refuse_load(
        &mut self,
        uid: ImageUid,
        load: &FrameLoad,
        replies: &mut ReplyQueue,
        err: GraphicsError,
        extra: Vec<(u8, String)>,
    ) -> bool {
        if load.charged > 0 {
            self.release(uid, load.charged);
            if let Some(entry) = self.entries.get_mut(&uid) {
                entry.frames_coming = entry.frames_coming.saturating_sub(1);
            }
        }
        replies.resolve_with(load.ticket, Err(err), extra);
        false
    }

    /// Run a queued compose: hand the host frame `r` and the rectangle to
    /// read from it, to compose onto frame `c`. The frames' buffers are
    /// shared with the host, nothing is cut or copied here. `true` when it
    /// is in flight; `false` when a frame is missing (`ENOENT`).
    fn run_compose(
        &mut self,
        uid: ImageUid,
        compose: FrameCompose,
        host: &mut dyn GraphicsHost,
        replies: &mut ReplyQueue,
    ) -> bool {
        let Some(entry) = self.entries.get(&uid) else {
            replies.resolve(compose.ticket, Ok(()));
            return false;
        };
        let record = entry.record.clone();
        let Some(source) = record.frame(compose.source) else {
            replies.resolve(
                compose.ticket,
                Err(missing_frame(&record, "source", compose.source)),
            );
            return false;
        };
        let Some(destination) = record.frame(compose.destination) else {
            replies.resolve(
                compose.ticket,
                Err(missing_frame(&record, "destination", compose.destination)),
            );
            return false;
        };
        let request = DecodeRequest {
            format: Format::Rgba,
            compression: Compression::None,
            width: compose.width,
            height: compose.height,
            data: Vec::new(),
            compose: Some(Compose {
                base: Some(destination.pixels),
                source: Some(ComposeSource {
                    pixels: source.pixels,
                    x: compose.source_at.0,
                    y: compose.source_at.1,
                    width: compose.width,
                    height: compose.height,
                }),
                background: 0,
                x: compose.destination_at.0,
                y: compose.destination_at.1,
                mode: compose.mode,
                canvas: (record.width, record.height),
            }),
        };
        self.parked.insert(
            compose.ticket,
            Parked {
                landing: Landing::Compose {
                    record,
                    destination: compose.destination,
                },
                landed: Ok(()),
                extra: Vec::new(),
                local_media: false,
            },
        );
        self.entries
            .get_mut(&uid)
            .expect("looked up a moment ago")
            .busy = true;
        host.decode(compose.ticket, request);
        true
    }

    fn run_control(&mut self, uid: ImageUid, control: &ControlData) {
        if let Some(entry) = self.entries.get(&uid)
            && entry.record.animate(control)
        {
            self.dirty = true;
        }
    }

    fn run_delete_frame(
        &mut self,
        uid: ImageUid,
        frame: u32,
        free: bool,
        replies: &mut ReplyQueue,
    ) {
        let Some(entry) = self.entries.get_mut(&uid) else {
            return;
        };
        let record = entry.record.clone();
        let count = record.frame_count();
        if count <= 1 {
            if free {
                // kitty removes the image outright: placements and data.
                let mut touched = Vec::new();
                let virtuals = entry.delete_placements(0, &mut touched);
                self.virtual_count -= virtuals;
                touched.push(uid);
                self.free_touched(touched, replies);
                self.dirty = true;
            }
            return;
        }
        let last = u32::try_from(count).unwrap_or(u32::MAX);
        if record.remove_frame(frame.clamp(1, last)) {
            self.release(uid, record.decoded_len());
            self.dirty = true;
        }
    }
}

/// An `a=c`'s canvas has landed: put in place of the destination frame,
/// unless the image went meanwhile.
fn land_compose(
    record: &Arc<ImageRecord>,
    destination: u32,
    result: DecodeResult,
) -> Result<(), GraphicsError> {
    let pixels = check_canvas(record, result)?;
    if !record.is_evicted() && record.replace_frame(destination, pixels).is_none() {
        log::debug!(
            "graphics compose onto frame {destination} of image {} dropped: the frame went meanwhile",
            record.id
        );
    }
    Ok(())
}

/// A decode's result checked against the record: exactly the image's size.
fn check_canvas(record: &ImageRecord, result: DecodeResult) -> DecodeResult {
    result.and_then(|pixels| {
        let len = u64::try_from(pixels.rgba.len()).unwrap_or(u64::MAX);
        if pixels.width == record.width
            && pixels.height == record.height
            && len == record.decoded_len()
        {
            Ok(pixels)
        } else {
            Err(GraphicsError::invalid(format!(
                "decoded image is {}x{} in {len} bytes, expected {}x{}",
                pixels.width, pixels.height, record.width, record.height
            )))
        }
    })
}

/// The `X` (`a=f`) or `C` (`a=c`) key: `1` overwrites, anything else blends.
fn compose_mode(key: u32) -> ComposeMode {
    if key == 1 {
        ComposeMode::Overwrite
    } else {
        ComposeMode::AlphaBlend
    }
}

/// The `z` key of a frame: zero is unspecified, negative is gapless.
fn frame_gap(z: i32) -> Option<u32> {
    match z {
        0 => None,
        z => Some(u32::try_from(z).unwrap_or(0)),
    }
}

/// `EINVAL` unless the `size` rectangle at `at` lies inside the image.
fn inside(
    record: &ImageRecord,
    (x, y): (u32, u32),
    (width, height): (u32, u32),
    what: &str,
) -> Result<(), GraphicsError> {
    if u64::from(x) + u64::from(width) > u64::from(record.width)
        || u64::from(y) + u64::from(height) > u64::from(record.height)
    {
        return Err(GraphicsError::invalid(format!(
            "The {what} rectangle is out of bounds"
        )));
    }
    Ok(())
}

/// Whether two `size` rectangles at `a` and `b` share a pixel.
fn overlaps(a: (u32, u32), b: (u32, u32), (width, height): (u32, u32)) -> bool {
    let along =
        |a: u32, b: u32, extent: u32| u64::from(a.max(b)) < u64::from(a.min(b)) + u64::from(extent);
    along(a.0, b.0, width) && along(a.1, b.1, height)
}

fn not_found(control: &ControlData) -> GraphicsError {
    GraphicsError::new(
        ErrorCode::NotFound,
        format!(
            "Animation command refers to non-existent image with id: {} and number: {}",
            control.image_id, control.image_number
        ),
    )
}

fn missing_frame(record: &ImageRecord, which: &str, frame: u32) -> GraphicsError {
    GraphicsError::new(
        ErrorCode::NotFound,
        format!(
            "No {which} frame number {frame} exists in image id: {}",
            record.id
        ),
    )
}

fn frame_limit() -> GraphicsError {
    GraphicsError::new(
        ErrorCode::NoSpace,
        format!("an image holds at most {MAX_FRAMES} frames"),
    )
}

fn queue_full(record: &ImageRecord) -> GraphicsError {
    GraphicsError::new(
        ErrorCode::NoSpace,
        format!(
            "image id: {} has {MAX_QUEUED_FRAME_OPS} frame commands waiting already",
            record.id
        ),
    )
}

/// The screen a positional delete is read against (see the module doc).
struct Screen {
    /// The sequence number of the top row's line.
    top: u64,
}

impl Screen {
    fn new(env: &GraphicsEnv<'_>) -> Self {
        let rows = u64::from(env.grid.1.max(1));
        Self {
            top: env.line_seq.saturating_sub(rows - 1),
        }
    }

    /// Whether a positional target, or `a`, takes `placement`. A placement
    /// without a cell (one hanging from a virtual placement) is taken only
    /// by `z`, which reads no position.
    fn matches(
        &self,
        target: DeleteTarget,
        control: &ControlData,
        env: &GraphicsEnv<'_>,
        placement: &Placement,
    ) -> bool {
        if target == DeleteTarget::ZIndex {
            return placement.geometry.z == control.z;
        }
        let Some((rows, columns)) = placement.footprint() else {
            return false;
        };
        match target {
            DeleteTarget::AllVisible => self.shows(&rows),
            DeleteTarget::AtCursor => {
                rows.contains(&env.line_seq) && columns.contains(&u32::from(env.column))
            }
            DeleteTarget::AtCell => {
                Self::column_hit(&columns, control.x) && self.row_hit(&rows, control.y)
            }
            DeleteTarget::AtCellWithZ => {
                placement.geometry.z == control.z
                    && Self::column_hit(&columns, control.x)
                    && self.row_hit(&rows, control.y)
            }
            DeleteTarget::Column => self.shows(&rows) && Self::column_hit(&columns, control.x),
            DeleteTarget::Row => self.row_hit(&rows, control.y),
            DeleteTarget::ZIndex
            | DeleteTarget::ByImageId
            | DeleteTarget::ByImageNumber
            | DeleteTarget::IdRange
            | DeleteTarget::Frames => false,
        }
    }

    /// Whether any of the placement's rows is on screen: what the
    /// specification means by "visible", and kitty's `start_row + rows > 0`.
    fn shows(&self, rows: &RangeInclusive<u64>) -> bool {
        *rows.end() >= self.top
    }

    /// Whether the placement's columns cover the 1-based column `x`.
    fn column_hit(columns: &Range<u32>, x: u32) -> bool {
        x >= 1 && columns.contains(&(x - 1))
    }

    /// Whether the placement's rows cover the 1-based screen row `y`.
    fn row_hit(&self, rows: &RangeInclusive<u64>, y: u32) -> bool {
        y >= 1 && rows.contains(&(self.top + u64::from(y - 1)))
    }
}

/// Move the children of a replaced placement (real or virtual) to its
/// replacement, which stands where it stood: `link` is the replacement.
fn adopt(from: &RwLock<Vec<Arc<Placement>>>, to: &RwLock<Vec<Arc<Placement>>>, link: &ParentLink) {
    let children = take_children(from);
    for child in &children {
        child.set_parent(link.clone());
    }
    to.write()
        .unwrap_or_else(PoisonError::into_inner)
        .extend(children);
}

/// Whether a placement at `depth` may adopt the chain below `replaced`
/// (none: nothing to adopt): the deepest adopted placement must stay within
/// [`PLACEMENT_DEPTH_LIMIT`], as a chain built link by link would.
///
/// # Errors
/// `ETOODEEP` when it would not.
fn adoption_fits(depth: usize, replaced: Option<&Arc<Placement>>) -> Result<(), GraphicsError> {
    let height = replaced.map_or(0, |old| old.subtree_height());
    if depth + height > PLACEMENT_DEPTH_LIMIT {
        return Err(too_deep());
    }
    Ok(())
}

fn too_deep() -> GraphicsError {
    GraphicsError::new(ErrorCode::TooDeep, "Too many levels of parent references")
}

/// Lay out one placeholder run's slice of a virtual placement's box. The
/// image is fit into the box (`columns` by `rows` cells; zero means the
/// cells its natural size spans) with its aspect kept and centred, as kitty
/// fits it; the run shows the box's row `row`, columns `column` onward for
/// `len` cells, and gets the image pixels under those cells as its source
/// rectangle, scaled back into the run's cells by `Fit::Box`. Letterboxing
/// can leave cells at the run's ends with no image under them: whole blank
/// cells are dropped from the run (`skip` from the left, and the count is
/// the cells kept), and a partly blank first cell gets the margin as its
/// pixel offset, so the slice lands exactly where the box puts it. `None`
/// when no cell of the run touches the image.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel arithmetic on sizes far below 2^24, rounded and clamped before the casts"
)]
fn run_geometry(
    (columns, rows): (u32, u32),
    (image_width, image_height): (u32, u32),
    cell: (u16, u16),
    (row, column): (u16, u16),
    len: u16,
) -> Option<(PlacementGeometry, usize, usize)> {
    if image_width == 0 || image_height == 0 || len == 0 {
        return None;
    }
    let (cell_width, cell_height) = cell_or_fallback(cell);
    let scale_unit = f64::from(CELL_FIXED_SCALE);
    let (cell_width, cell_height) = (
        f64::from(cell_width) / scale_unit,
        f64::from(cell_height) / scale_unit,
    );
    let (image_width, image_height) = (f64::from(image_width), f64::from(image_height));
    let box_columns = if columns == 0 {
        (image_width / cell_width).ceil()
    } else {
        f64::from(columns)
    };
    let box_rows = if rows == 0 {
        (image_height / cell_height).ceil()
    } else {
        f64::from(rows)
    };
    let (box_width, box_height) = (box_columns * cell_width, box_rows * cell_height);
    // Box pixels per image pixel, and where the fitted image starts.
    let scale = (box_width / image_width).min(box_height / image_height);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let (fit_x, fit_y) = (
        (box_width - image_width * scale) / 2.0,
        (box_height - image_height * scale) / 2.0,
    );
    // The run's rectangle in the box, then in the image.
    let (run_x, run_y) = (f64::from(column) * cell_width, f64::from(row) * cell_height);
    let (run_width, run_height) = (f64::from(len) * cell_width, cell_height);
    let (src_x, src_y) = ((run_x - fit_x) / scale, (run_y - fit_y) / scale);
    let (src_width, src_height) = (run_width / scale, run_height / scale);
    let (left, top) = (src_x.max(0.0), src_y.max(0.0));
    let (right, bottom) = (
        (src_x + src_width).min(image_width),
        (src_y + src_height).min(image_height),
    );
    if right <= left || bottom <= top {
        return None;
    }
    // Where the visible part lands inside the run, in box pixels.
    let (into_x, into_y) = ((left - src_x) * scale, (top - src_y) * scale);
    let skip = (into_x / cell_width).floor().clamp(0.0, f64::from(len)) as usize;
    let end = ((into_x + (right - left) * scale) / cell_width)
        .ceil()
        .clamp(0.0, f64::from(len)) as usize;
    if end <= skip {
        return None;
    }
    let count = end - skip;
    let offset = |pixels: f64, cell: f64| {
        u16::try_from(pixels.round().clamp(0.0, (cell - 1.0).max(0.0)) as u32).unwrap_or(u16::MAX)
    };
    let x_offset = offset(into_x - skip as f64 * cell_width, cell_width);
    let y_offset = offset(into_y, cell_height);
    let (x, y) = (left.round() as u32, top.round() as u32);
    let source = SourceRect {
        x,
        y,
        width: (right.round() as u32).saturating_sub(x).max(1),
        height: (bottom.round() as u32).saturating_sub(y).max(1),
    };
    let columns = u16::try_from(count).unwrap_or(u16::MAX);
    Some((
        PlacementGeometry {
            columns,
            rows: 1,
            source,
            x_offset,
            y_offset,
            fit: Fit::Box { columns, rows: 1 },
            z: 0,
        },
        skip,
        count,
    ))
}

/// `a=q` with `e=`: which keys of the asked-about action this terminal
/// supports with the values given. An action it does not support at all
/// gets a reply without `e`. The caller has refused a query without an
/// image id already.
fn answer_feature_query(env: &mut GraphicsEnv<'_>, control: &ControlData) {
    let mut reply = ReplyTo::of(control, env.replies_enabled);
    if let Err(err) = control.validate() {
        reply.answer(env.replies, Err(err));
        return;
    }
    // Every supported key is echoed after `e`, `p` among them.
    reply.placement = 0;
    let mut response = reply.response(Ok(()));
    if let Some(action) = control.feature_query {
        response.extra = supported_keys(action, control);
    }
    env.replies.push_response(&response, reply.quiet);
}

/// The keys of a feature query this terminal supports with the values given,
/// `e` first and the rest in byte order; empty for an unsupported action.
fn supported_keys(action: Action, control: &ControlData) -> Vec<(u8, String)> {
    fn number(keys: &mut Vec<(u8, String)>, key: u8, value: u32) {
        if value != 0 {
            keys.push((key, value.to_string()));
        }
    }
    let mut keys: Vec<(u8, String)> = Vec::new();
    let transmits = matches!(
        action,
        Action::Transmit | Action::TransmitAndDisplay | Action::Query | Action::Frame
    );
    let places = matches!(action, Action::TransmitAndDisplay | Action::Put);
    if transmits {
        number(&mut keys, b's', control.width);
        number(&mut keys, b'v', control.height);
    }
    if places {
        for (key, value) in [
            (b'P', control.parent_image),
            (b'Q', control.parent_placement),
            (b'X', control.x_offset),
            (b'Y', control.y_offset),
            (b'c', control.columns),
            (b'h', control.h),
            (b'p', control.placement_id),
            (b'r', control.rows),
            (b'w', control.w),
            (b'x', control.x),
            (b'y', control.y),
        ] {
            number(&mut keys, key, value);
        }
        for (key, value) in [
            (b'H', control.offset_columns),
            (b'V', control.offset_rows),
            (b'z', control.z),
        ] {
            if value != 0 {
                keys.push((key, value.to_string()));
            }
        }
        if control.cursor_movement == 1 {
            keys.push((b'C', "1".to_string()));
        }
        if control.unicode_placeholder {
            keys.push((b'U', "1".to_string()));
        }
    }
    match action {
        Action::Transmit | Action::TransmitAndDisplay | Action::Query | Action::Put => {}
        Action::Delete => {
            let Some((target, _)) = control.delete_target().ok() else {
                return Vec::new();
            };
            match target {
                DeleteTarget::ByImageId | DeleteTarget::ByImageNumber => {
                    number(&mut keys, b'p', control.placement_id);
                }
                DeleteTarget::IdRange | DeleteTarget::AtCell => {
                    number(&mut keys, b'x', control.x);
                    number(&mut keys, b'y', control.y);
                }
                DeleteTarget::AtCellWithZ => {
                    number(&mut keys, b'x', control.x);
                    number(&mut keys, b'y', control.y);
                    if control.z != 0 {
                        keys.push((b'z', control.z.to_string()));
                    }
                }
                DeleteTarget::Column => number(&mut keys, b'x', control.x),
                DeleteTarget::Row => number(&mut keys, b'y', control.y),
                DeleteTarget::ZIndex => {
                    if control.z != 0 {
                        keys.push((b'z', control.z.to_string()));
                    }
                }
                // `r` is not echoed for a frame delete, as kitty echoes
                // `d`, `p`, `x`, `y` and `z` alone for any delete.
                DeleteTarget::AllVisible | DeleteTarget::AtCursor | DeleteTarget::Frames => {}
            }
            if let Some(letter) = control.delete {
                keys.push((b'd', char::from(letter).to_string()));
            }
        }
        Action::Frame | Action::Animate | Action::Compose => {
            animation_keys(action, control, &mut keys);
        }
    }
    if transmits {
        match control.format {
            Format::Rgb => keys.push((b'f', "24".to_string())),
            Format::Png => keys.push((b'f', "100".to_string())),
            Format::Rgba | Format::Unsupported(_) => {}
        }
        if control.compression == Compression::Zlib {
            keys.push((b'o', "z".to_string()));
        }
    }
    keys.sort_by_key(|(key, _)| *key);
    keys.insert(0, (b'e', char::from(action.letter()).to_string()));
    keys
}

/// The keys of the animation actions a feature query echoes, as kitty
/// echoes them: the numbers given, `z` when non-zero, the mode flags when
/// `1`, and a playback state only when it is one of the three.
fn animation_keys(action: Action, control: &ControlData, keys: &mut Vec<(u8, String)>) {
    let numbers: &[(u8, u32)] = match action {
        Action::Frame => &[
            (b'Y', control.y_offset),
            (b'c', control.columns),
            (b'r', control.rows),
            (b'x', control.x),
            (b'y', control.y),
        ],
        Action::Animate => &[
            (b'c', control.columns),
            (b'r', control.rows),
            (b'v', control.height),
        ],
        Action::Compose => &[
            (b'X', control.x_offset),
            (b'Y', control.y_offset),
            (b'c', control.columns),
            (b'h', control.h),
            (b'r', control.rows),
            (b'w', control.w),
            (b'x', control.x),
            (b'y', control.y),
        ],
        _ => &[],
    };
    for &(key, value) in numbers {
        if value != 0 {
            keys.push((key, value.to_string()));
        }
    }
    let flag = match action {
        Action::Frame => Some((b'X', control.x_offset)),
        Action::Compose => Some((b'C', control.cursor_movement)),
        _ => None,
    };
    if let Some((key, 1)) = flag {
        keys.push((key, "1".to_string()));
    }
    if action != Action::Compose && control.z != 0 {
        keys.push((b'z', control.z.to_string()));
    }
    if action == Action::Animate && (1..=3).contains(&control.width) {
        keys.push((b's', control.width.to_string()));
    }
}

/// The pixel size a whole payload declares, checked against the limits
/// without decoding anything: packed data by its exact length (when
/// uncompressed; compressed data the host checks once inflated), PNG by its
/// signature and header. Local media answer `EBADF` whatever is wrong with
/// what the host read (see [`read_verdict`]).
fn probe(control: &ControlData, data: &[u8]) -> Result<(u32, u32), GraphicsError> {
    probe_data(control, data).map_err(|err| read_verdict(control, err))
}

/// What the peer hears of a check that failed on a command's data. Direct
/// data is the peer's own, and every check on it answers with kitty's
/// codes and words (`ENODATA:Insufficient image data: X < Y`, `EFBIG:Too
/// much data`, what a PNG header or a zlib stream lacked). Local media
/// (`t=f`, `t=t`, `t=s`) the host read for the peer, and a verdict on what
/// it read is a verdict on the file: every failure is the one `EBADF`, as
/// kitty answers every file-read problem, so a short file cannot be
/// measured through `ENODATA`, nor what any file holds told by another
/// check's words — a PNG header's size against the image or the quota, a
/// file's length against what is left of the quota, what the decoder made
/// of the bytes. Every check on local media after the host's read goes
/// through here. What the mapping cannot take away is the oracle kitty
/// has too: whether a name was readable at all shows in `OK` against
/// `EBADF`, so a peer allowed local media at all can tell which paths
/// exist and are readable by the user. That is why local media are opt-in
/// per server, for a loopback peer only.
fn read_verdict(control: &ControlData, err: GraphicsError) -> GraphicsError {
    if control.medium == Medium::Direct {
        err
    } else {
        GraphicsError::bad_file()
    }
}

/// [`probe`] before the medium has its say.
fn probe_data(control: &ControlData, data: &[u8]) -> Result<(u32, u32), GraphicsError> {
    let (width, height) = if control.format == Format::Png {
        let header = match control.compression {
            Compression::None => Cow::Borrowed(data),
            Compression::Zlib => Cow::Owned(inflate_prefix(data, PNG_HEADER_LEN)?),
        };
        png_dimensions(&header).ok_or_else(|| GraphicsError::invalid("not a PNG image"))?
    } else {
        let (width, height) = (control.width, control.height);
        if width == 0 || height == 0 {
            return Err(GraphicsError::invalid(
                "packed pixel data needs non-zero s and v",
            ));
        }
        let expected = control
            .format
            .expected_len(width, height)
            .ok_or_else(|| GraphicsError::invalid("Unknown image format"))?;
        if control.compression == Compression::None {
            check_len(data.len(), expected)?;
        }
        (width, height)
    };
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(GraphicsError::invalid(format!(
            "image dimensions {width}x{height} exceed {MAX_IMAGE_DIMENSION}"
        )));
    }
    Ok((width, height))
}

/// Check a compressed query payload here when its inflated size is known
/// to be small: the packed formats' exact length, or [`MAX_INLINE_INFLATE`]
/// for a PNG, whose size is only known once inflated. `None` when the data
/// is more than this thread inflates — the exact length is larger, or a
/// PNG grew past the cap — and the host must take it. Local media answer
/// `EBADF` whatever is wrong (see [`read_verdict`]).
fn check_inflated(
    control: &ControlData,
    data: &[u8],
    width: u32,
    height: u32,
) -> Option<Result<(), GraphicsError>> {
    let expected = control.format.expected_len(width, height);
    let cap = expected.unwrap_or(MAX_INLINE_INFLATE);
    if cap > MAX_INLINE_INFLATE {
        return None;
    }
    let status = match inflate(data, usize::try_from(cap).unwrap_or(usize::MAX)) {
        Ok(inflated) => {
            Some(expected.map_or(Ok(()), |expected| check_len(inflated.len(), expected)))
        }
        Err(err) if err.code == ErrorCode::NoSpace => {
            // Packed data inflating past its exact length is too long; a
            // PNG past the cap is merely too long for here.
            expected.map(|_| Err(too_much_data()))
        }
        Err(err) => Some(Err(err)),
    };
    status.map(|status| status.map_err(|err| read_verdict(control, err)))
}

/// Compare a packed payload's length with the one its size demands, with
/// kitty's codes and words for either way it can miss.
fn check_len(got: usize, expected: u64) -> Result<(), GraphicsError> {
    let got = u64::try_from(got).unwrap_or(u64::MAX);
    match got.cmp(&expected) {
        std::cmp::Ordering::Less => Err(GraphicsError::new(
            ErrorCode::NoData,
            format!("Insufficient image data: {got} < {expected}"),
        )),
        std::cmp::Ordering::Greater => Err(too_much_data()),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

fn too_much_data() -> GraphicsError {
    GraphicsError::new(ErrorCode::TooBig, "Too much data")
}

fn rgba_len(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * 4
}

/// The cells a fixed-point `extent` spans, at least one and at most `cap`.
fn cells(extent: u64, cell: u16, cap: u16) -> u16 {
    let count = extent.div_ceil(u64::from(cell.max(1)));
    u16::try_from(count).unwrap_or(u16::MAX).clamp(1, cap)
}

/// A client-given cell count, bounded like a computed one.
fn given_cells(count: u32, cap: u16) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX).clamp(1, cap)
}

/// Lay a placement out: the source rectangle intersected with the image,
/// the offsets bounded by the cell, and the cells covered. With neither `c`
/// nor `r` the image keeps its natural size and covers the cells it spans
/// from its offset (as kitty counts them); with one, the other follows from
/// the aspect ratio of the box the given one makes — kitty scales the image
/// to the given cells plus their offset and counts the other side's pixels
/// in cells from the cell's edge, so `c=10,Y=19` is no taller for the `Y`;
/// with both, the box is as given and the image is fit inside it. A source
/// rectangle that misses the image covers no cells at all. Either side is
/// capped ([`MAX_PLACEMENT_CELLS`], [`MAX_PLACEMENT_ROWS`]); a box past a
/// cap is clamped to it. `cell` is in fixed point ([`CELL_FIXED_SCALE`]
/// units to the pixel), so the cells are counted from the pane's real
/// advance: a 96 px image in 9.6 px cells is ten columns, not the eleven a
/// cell truncated to 9 px would give.
///
/// A box (`c` and/or `r`) holds the image letterboxed, as the specification
/// says; kitty's own renderer stretches an ordinary placement into the box
/// instead, so a client tuned against kitty may expect a stretched image
/// here and see the aspect kept.
#[must_use]
pub fn geometry(
    control: &ControlData,
    image_width: u32,
    image_height: u32,
    cell: (u16, u16),
) -> PlacementGeometry {
    let (cell_width, cell_height) = if cell.0 == 0 || cell.1 == 0 {
        FALLBACK_CELL
    } else {
        cell
    };
    let x = control.x.min(image_width);
    let y = control.y.min(image_height);
    let span = |given: u32, available: u32| {
        if given == 0 {
            available
        } else {
            given.min(available)
        }
    };
    let source = SourceRect {
        x,
        y,
        width: span(control.w, image_width - x),
        height: span(control.h, image_height - y),
    };
    // An offset is whole pixels inside the first cell: at most the last
    // whole pixel the cell holds.
    let offset = |given: u32, cell: u16| {
        u16::try_from(given)
            .unwrap_or(u16::MAX)
            .min((cell - 1) / CELL_FIXED_SCALE)
    };
    let x_offset = offset(control.x_offset, cell_width);
    let y_offset = offset(control.y_offset, cell_height);
    let (src_w, src_h) = (u64::from(source.width), u64::from(source.height));
    // Extents below are in fixed point, like the cell.
    let scale = u64::from(CELL_FIXED_SCALE);
    let columns_for = |extent: u64| cells(extent, cell_width, MAX_PLACEMENT_CELLS);
    let rows_for = |extent: u64| cells(extent, cell_height, MAX_PLACEMENT_ROWS);
    let (columns, rows, fit) = if src_w == 0 || src_h == 0 {
        (0, 0, Fit::Natural)
    } else {
        match (control.columns, control.rows) {
            (0, 0) => (
                columns_for((src_w + u64::from(x_offset)) * scale),
                rows_for((src_h + u64::from(y_offset)) * scale),
                Fit::Natural,
            ),
            (columns, 0) => {
                let columns = given_cells(columns, MAX_PLACEMENT_CELLS);
                let width =
                    u64::from(columns) * u64::from(cell_width) + u64::from(x_offset) * scale;
                let rows = rows_for((width * src_h).div_ceil(src_w));
                (columns, rows, Fit::Box { columns, rows })
            }
            (0, rows) => {
                let rows = given_cells(rows, MAX_PLACEMENT_ROWS);
                let height = u64::from(rows) * u64::from(cell_height) + u64::from(y_offset) * scale;
                let columns = columns_for((height * src_w).div_ceil(src_h));
                (columns, rows, Fit::Box { columns, rows })
            }
            (columns, rows) => {
                let columns = given_cells(columns, MAX_PLACEMENT_CELLS);
                let rows = given_cells(rows, MAX_PLACEMENT_ROWS);
                (columns, rows, Fit::Box { columns, rows })
            }
        }
    };
    PlacementGeometry {
        columns,
        rows,
        source,
        x_offset,
        y_offset,
        fit,
        z: control.z,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::encode_payload;

    #[derive(Debug, Default)]
    struct StubHost {
        decodes: Vec<(DecodeTicket, DecodeRequest)>,
        media: Vec<(Medium, Vec<u8>, u32, u32)>,
        /// What every media read returns; `None` refuses with `EBADF`.
        media_bytes: Option<Vec<u8>>,
    }

    impl GraphicsHost for StubHost {
        fn decode(&mut self, ticket: DecodeTicket, request: DecodeRequest) {
            self.decodes.push((ticket, request));
        }

        fn read_media(
            &mut self,
            medium: Medium,
            name: &[u8],
            offset: u32,
            size: u32,
        ) -> Result<Vec<u8>, GraphicsError> {
            self.media.push((medium, name.to_vec(), offset, size));
            self.media_bytes.clone().ok_or_else(GraphicsError::bad_file)
        }
    }

    struct Fixture {
        state: GraphicsState,
        host: StubHost,
        replies: ReplyQueue,
        cell: (u16, u16),
        replies_enabled: bool,
        /// Room on the line at the cursor: unbounded unless a test says
        /// otherwise, since nothing here tracks a line.
        objects_free: usize,
        /// The pending line's sequence number and the cursor's column on
        /// it, as the processor would pass them.
        line_seq: u64,
        column: u16,
        grid: (u16, u16),
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_quota(DEFAULT_QUOTA_BYTES)
        }

        fn with_quota(quota: u64) -> Self {
            Self {
                state: GraphicsState::with_quota(quota),
                host: StubHost::default(),
                replies: ReplyQueue::default(),
                cell: px(10, 20),
                replies_enabled: true,
                objects_free: usize::MAX,
                line_seq: 0,
                column: 0,
                grid: (80, 24),
            }
        }

        /// Run one command, `<control>[;<payload>]`, as its APC would.
        fn send(&mut self, command: &str) -> Option<Arc<Placement>> {
            let apc = format!("G{command}");
            let (control, payload) = parse_apc(apc.as_bytes())
                .expect("a graphics APC")
                .unwrap_or_else(|err| panic!("{command}: {err}"));
            let mut env = GraphicsEnv {
                host: &mut self.host,
                replies: &mut self.replies,
                cell: self.cell,
                replies_enabled: self.replies_enabled,
                objects_free: self.objects_free,
                line_seq: self.line_seq,
                column: self.column,
                grid: self.grid,
            };
            self.state.command(&mut env, control, payload)
        }

        fn truncated(&mut self, prefix: &str) {
            let mut env = GraphicsEnv {
                host: &mut self.host,
                replies: &mut self.replies,
                cell: self.cell,
                replies_enabled: self.replies_enabled,
                objects_free: self.objects_free,
                line_seq: self.line_seq,
                column: self.column,
                grid: self.grid,
            };
            self.state.truncated_command(&mut env, prefix.as_bytes());
        }

        /// A placeholder run of `len` cells at line column `at` showing box
        /// row `row` from box column `column`, laid out as the processor
        /// would ask.
        fn run(
            &mut self,
            ids: (u32, u32),
            (row, column): (u16, u16),
            len: u16,
            at: usize,
        ) -> Option<(Arc<Placement>, std::ops::Range<usize>)> {
            let run = PlaceholderRun {
                image_id: ids.0,
                placement_id: ids.1,
                row,
                column,
                cells: (0..usize::from(len)).map(|i| i * 4..i * 4 + 4).collect(),
                first_cell_column: at,
            };
            let env = GraphicsEnv {
                host: &mut self.host,
                replies: &mut self.replies,
                cell: self.cell,
                replies_enabled: self.replies_enabled,
                objects_free: self.objects_free,
                line_seq: self.line_seq,
                column: self.column,
                grid: self.grid,
            };
            self.state.placeholder_run(&run, &env)
        }

        /// The replies ready now, one per frame, without `ESC _ G` and `ESC \`.
        fn replies(&mut self) -> Vec<String> {
            let mut out = Vec::new();
            self.replies.drain_into(&mut out);
            frames(&out)
        }

        /// Report the oldest decode still in flight with `result`.
        fn finish(&mut self, result: impl FnOnce(&DecodeRequest) -> DecodeResult) {
            assert!(!self.host.decodes.is_empty(), "no decode in flight");
            let (ticket, request) = self.host.decodes.remove(0);
            let result = result(&request);
            self.state
                .decode_complete(&mut self.host, &mut self.replies, ticket, result);
        }

        /// Report the oldest decode with pixels of the size it asked for:
        /// the canvas for a frame, the data's size otherwise.
        fn finish_ok(&mut self) {
            self.finish(|request| {
                let (width, height) = request
                    .compose
                    .as_ref()
                    .map_or((request.width, request.height), |compose| compose.canvas);
                Ok(pixels_for(width, height))
            });
        }

        /// Report the oldest decode with the reference composition of its
        /// data — which [`rgba`] sends as RGBA already — or, with `value`,
        /// of `value`-filled data, so the frame shows where it landed.
        fn finish_composed(&mut self, value: Option<u8>) {
            self.finish(|request| {
                let compose = request.compose.as_ref().expect("a frame decode");
                let len = usize::try_from(rgba_len(request.width, request.height)).unwrap();
                let data = value.map_or_else(|| request.data.clone(), |value| vec![value; len]);
                let rgba = compose.apply(&data, request.width, request.height)?;
                Ok(DecodedPixels {
                    width: compose.canvas.0,
                    height: compose.canvas.1,
                    rgba: rgba.into(),
                })
            });
        }

        /// The image with id `id`, which must exist.
        fn image(&self, id: u32) -> Arc<ImageRecord> {
            self.state.image(id).expect("the image exists")
        }
    }

    fn pixels_for(width: u32, height: u32) -> DecodedPixels {
        let len = usize::try_from(rgba_len(width, height)).unwrap();
        DecodedPixels {
            width,
            height,
            rgba: vec![0xff; len].into(),
        }
    }

    fn frames(bytes: &[u8]) -> Vec<String> {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .split("\x1b\\")
            .filter(|frame| !frame.is_empty())
            .map(|frame| {
                frame
                    .strip_prefix("\x1b_G")
                    .unwrap_or_else(|| panic!("not a graphics reply: {frame:?}"))
                    .to_string()
            })
            .collect()
    }

    /// A packed RGBA transmit of `width` by `height` with `keys` in front.
    fn rgba(keys: &str, width: u32, height: u32) -> String {
        let len = usize::try_from(rgba_len(width, height)).unwrap();
        format!(
            "{keys},f=32,s={width},v={height};{}",
            encode_payload(&vec![0; len])
        )
    }

    /// The first 33 bytes of a PNG: signature and IHDR, enough to size it.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        png
    }

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn control(keys: &str) -> ControlData {
        crate::graphics::parse_control(keys.as_bytes()).unwrap()
    }

    /// A whole-pixel cell, in fixed point.
    fn px(width: u16, height: u16) -> (u16, u16) {
        (width * CELL_FIXED_SCALE, height * CELL_FIXED_SCALE)
    }

    // --- transmit and display ---

    #[test]
    fn a_transmit_is_decoded_off_thread_and_answered_when_it_lands() {
        let mut fx = Fixture::new();
        assert!(fx.send(&rgba("a=t,i=1", 2, 3)).is_none());
        assert!(fx.replies().is_empty(), "the reply waits for the decode");
        let record = fx.state.image(1).expect("stored at once");
        assert_eq!((record.id, record.width, record.height), (1, 2, 3));
        assert_eq!(record.data(), ImageData::Pending);
        assert_eq!(fx.state.used_bytes(), 24);
        let request = &fx.host.decodes[0].1;
        assert_eq!(
            (
                request.format,
                request.compression,
                request.width,
                request.height
            ),
            (Format::Rgba, Compression::None, 2, 3)
        );
        assert_eq!(request.data.len(), 24);
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        assert_eq!(record.pixels(), Some(pixels_for(2, 3)));
        assert_eq!(fx.state.decodes_in_flight(), 0);
    }

    #[test]
    fn a_transmit_and_display_places_at_once_and_answers_after_the_decode() {
        let mut fx = Fixture::new();
        assert!(
            fx.send("a=T,i=2,p=3,f=24,s=20,v=40;").is_none(),
            "short data is refused before placing"
        );
        assert_eq!(
            fx.replies(),
            ["i=2,p=3;ENODATA:Insufficient image data: 0 < 2400"]
        );
        let data = encode_payload(&[0; 2400]);
        let placement = fx
            .send(&format!("a=T,i=2,p=3,f=24,s=20,v=40;{data}"))
            .expect("placed at the cursor");
        assert_eq!(placement.id, 3);
        assert!(Arc::ptr_eq(&placement.image, &fx.state.image(2).unwrap()));
        assert_eq!(
            placement.geometry,
            PlacementGeometry {
                columns: 2,
                rows: 2,
                source: SourceRect {
                    x: 0,
                    y: 0,
                    width: 20,
                    height: 40
                },
                x_offset: 0,
                y_offset: 0,
                fit: Fit::Natural,
                z: 0,
            }
        );
        assert_eq!(placement.pixels(), None, "nothing to draw while decoding");
        assert!(fx.replies().is_empty());
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=2,p=3;OK"]);
        assert_eq!(placement.pixels(), Some(pixels_for(20, 40)));
    }

    #[test]
    fn a_transmit_without_id_or_number_is_shown_but_never_answered() {
        let mut fx = Fixture::new();
        assert!(
            fx.send(&rgba("a=t", 1, 1)).is_none(),
            "a=t alone could never be addressed"
        );
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.image_count(), 0);
        let placement = fx.send(&rgba("a=T,p=4", 1, 1)).expect("a=T still shows it");
        assert_eq!(placement.id, 0, "a placement id needs an image id");
        assert_eq!(placement.image.id, 0);
        fx.finish_ok();
        assert!(fx.replies().is_empty());
        assert!(placement.pixels().is_some());
    }

    #[test]
    fn compressed_and_png_payloads_are_handed_over_undecoded() {
        let mut fx = Fixture::new();
        let packed = zlib(&[7; 12]);
        fx.send(&format!(
            "a=t,i=1,f=24,s=2,v=2,o=z;{}",
            encode_payload(&packed)
        ));
        let request = &fx.host.decodes[0].1;
        assert_eq!(request.compression, Compression::Zlib);
        assert_eq!(request.data, packed, "nothing inflated on this thread");

        let mut png = png_header(30, 10);
        png.extend_from_slice(&[0; 64]);
        fx.send(&format!("a=t,i=2,f=100;{}", encode_payload(&png)));
        let request = &fx.host.decodes[1].1;
        assert_eq!(
            (request.width, request.height),
            (30, 10),
            "sized by its header"
        );
        assert_eq!(request.data, png);

        let squeezed = zlib(&png);
        fx.send(&format!("a=t,i=3,f=100,o=z;{}", encode_payload(&squeezed)));
        let request = &fx.host.decodes[2].1;
        assert_eq!((request.width, request.height), (30, 10));
        assert_eq!(request.data, squeezed);
        assert_eq!(fx.state.used_bytes(), 16 + 1200 + 1200);
    }

    #[test]
    fn data_is_checked_before_any_decode() {
        let mut fx = Fixture::new();
        fx.send("a=t,i=1,f=24,s=2,v=1;AAAA");
        fx.send(&format!("a=t,i=2,f=24,s=1,v=1;{}", encode_payload(&[0; 4])));
        fx.send(&format!(
            "a=t,i=3,f=100;{}",
            encode_payload(b"GIF89a not a png")
        ));
        fx.send(&format!(
            "a=t,i=4,f=100;{}",
            encode_payload(&png_header(10_001, 1))
        ));
        fx.send("a=t,i=5,f=24,s=10001,v=1;AAAA");
        fx.send("a=t,i=6,f=24,s=1,v=1;!!!!");
        fx.send(&format!(
            "a=t,i=7,f=100,o=z;{}",
            encode_payload(b"not zlib")
        ));
        assert_eq!(
            fx.replies(),
            [
                "i=1;ENODATA:Insufficient image data: 3 < 6",
                "i=2;EFBIG:Too much data",
                "i=3;EINVAL:not a PNG image",
                "i=4;EINVAL:image dimensions 10001x1 exceed 10000",
                "i=5;EINVAL:image dimensions 10001x1 exceed 10000",
                "i=6;EINVAL:payload is not valid base64: Invalid symbol 33, offset 0.",
                "i=7;EINVAL:zlib data is corrupt: corrupt deflate stream",
            ]
        );
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.image_count(), 0);
    }

    #[test]
    fn an_image_the_quota_cannot_hold_is_refused_with_enospc() {
        let mut fx = Fixture::with_quota(64);
        fx.send(&rgba("a=T,i=1", 4, 4));
        assert_eq!(fx.host.decodes.len(), 1, "exactly the quota fits");
        fx.send(&format!(
            "a=T,i=2,f=24,s=5,v=4;{}",
            encode_payload(&[0; 60])
        ));
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=2;ENOSPC:image is larger than the storage quota"
            ]
        );
        assert!(fx.state.image(2).is_none());
        assert!(fx.state.image(1).is_some(), "nothing evicted for a refusal");
    }

    #[test]
    fn every_transmit_of_a_batch_is_placed_and_answered() {
        // The host reports back only between read batches, so a batch of
        // icons must not be bounded by how many decodes are still out.
        let mut fx = Fixture::new();
        let placements: Vec<Arc<Placement>> = (1..=100)
            .map(|id| {
                fx.send(&rgba(&format!("a=T,i={id}"), 1, 1))
                    .unwrap_or_else(|| panic!("transmit {id} placed"))
            })
            .collect();
        assert_eq!(fx.state.decodes_in_flight(), 100);
        assert_eq!(fx.state.image_count(), 100);
        assert_eq!(fx.state.used_bytes(), 400);
        assert!(fx.replies().is_empty());
        for _ in 0..100 {
            fx.finish_ok();
        }
        let expected: Vec<String> = (1..=100).map(|id| format!("i={id};OK")).collect();
        assert_eq!(fx.replies(), expected);
        assert!(placements.iter().all(|p| p.pixels().is_some()));
        assert_eq!(fx.state.decodes_in_flight(), 0);
    }

    // --- the line at the cursor ---

    #[test]
    fn a_put_on_a_full_line_is_refused_before_anything_is_registered() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        fx.replies();
        let uid = fx.state.image(1).unwrap().uid;
        let used_before = fx.state.entries[&uid].used;
        fx.objects_free = 0;
        assert!(fx.send("a=p,i=1,p=65").is_none());
        assert_eq!(
            fx.replies(),
            ["i=1,p=65;ENOSPC:too many placements on this line"]
        );
        let entry = &fx.state.entries[&uid];
        assert!(
            entry.named.is_empty() && entry.anonymous.is_empty(),
            "nothing registered for a tile never shown"
        );
        assert_eq!(entry.used, used_before, "and no use recorded");
        // On a line with room the same put goes through.
        fx.objects_free = 1;
        let placement = fx.send("a=p,i=1,p=65").expect("placed");
        assert_eq!(placement.id, 65);
        assert_eq!(fx.replies(), ["i=1,p=65;OK"]);
    }

    #[test]
    fn a_transmit_and_display_on_a_full_line_stores_the_image_and_refuses_the_placement() {
        let mut fx = Fixture::new();
        fx.objects_free = 0;
        assert!(fx.send(&rgba("a=T,i=2,p=7", 2, 2)).is_none(), "not placed");
        let record = fx.state.image(2).expect("stored all the same");
        assert_eq!(record.data(), ImageData::Pending);
        assert_eq!(fx.host.decodes.len(), 1, "and decoded");
        assert!(fx.replies().is_empty(), "the answer waits for the decode");
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            ["i=2,p=7;ENOSPC:too many placements on this line"],
            "the refusal names the placement, not the image"
        );
        assert!(record.pixels().is_some());
        // The client can put it on a line with room.
        fx.objects_free = 1;
        assert!(fx.send("a=p,i=2,p=7").is_some());
        assert_eq!(fx.replies(), ["i=2,p=7;OK"]);
        // A decode that fails answers with its own error: the image is gone.
        fx.objects_free = 0;
        fx.send(&rgba("a=T,i=3", 1, 1));
        fx.finish(|_| Err(GraphicsError::invalid("torn")));
        assert_eq!(fx.replies(), ["i=3;EINVAL:torn"]);
        assert!(fx.state.image(3).is_none());
    }

    #[test]
    fn an_empty_source_rectangle_places_nothing_and_answers_ok() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 50));
        fx.send(&rgba("a=t,i=2", 50, 1));
        fx.finish_ok();
        fx.finish_ok();
        fx.replies();
        let uid = fx.state.image(1).unwrap().uid;
        // `c=1` on a source one pixel to the right of a 1x50 image: the
        // `along.max(1)` guard once made this 25 rows tall.
        assert!(fx.send("a=p,i=1,c=1,x=1").is_none());
        assert!(fx.send("a=p,i=2,r=1,y=1").is_none());
        assert!(fx.send("a=p,i=1,x=1").is_none());
        assert_eq!(fx.replies(), ["i=1;OK", "i=2;OK", "i=1;OK"]);
        let entry = &fx.state.entries[&uid];
        assert!(entry.named.is_empty() && entry.anonymous.is_empty());
        // Even on a full line: nothing was going to be attached.
        fx.objects_free = 0;
        assert!(fx.send("a=p,i=1,x=1").is_none());
        assert_eq!(fx.replies(), ["i=1;OK"]);
    }

    // --- image numbers ---

    #[test]
    fn image_numbers_get_terminal_assigned_ids_and_commands_act_on_the_newest() {
        let mut fx = Fixture::new();
        // A client id in the assigned range is skipped, not shadowed.
        fx.send(&rgba("a=t,i=2147483649", 1, 1));
        fx.send(&rgba("a=t,I=13", 1, 1));
        fx.send(&rgba("a=t,I=13", 1, 1));
        for _ in 0..3 {
            fx.finish_ok();
        }
        assert_eq!(
            fx.replies(),
            [
                "i=2147483649;OK",
                "i=2147483648,I=13;OK",
                "i=2147483650,I=13;OK"
            ]
        );
        let newest = fx.state.newest_by_number(13).unwrap();
        assert_eq!(newest.id, 2_147_483_650);
        let placement = fx.send("a=p,I=13,p=1").expect("placed");
        assert!(Arc::ptr_eq(&placement.image, &newest));
        assert_eq!(fx.replies(), ["i=2147483650,I=13,p=1;OK"]);
        // Deleting by number frees the newest; the older one is newest now.
        fx.send("a=d,d=N,I=13");
        assert!(placement.is_deleted() && newest.is_evicted());
        assert_eq!(fx.state.newest_by_number(13).unwrap().id, 2_147_483_648);
        fx.send("a=p,I=14");
        assert_eq!(
            fx.replies(),
            ["I=14;ENOENT:Put command refers to non-existent image with id: 0 and number: 14"]
        );
    }

    #[test]
    fn a_transmit_and_display_by_number_echoes_the_minted_id_number_and_placement() {
        let mut fx = Fixture::new();
        let placement = fx.send(&rgba("a=T,I=7,p=3", 1, 1)).expect("placed");
        assert_eq!(
            (placement.id, placement.image.id, placement.image.number),
            (3, FIRST_ASSIGNED_ID, 7)
        );
        assert!(fx.replies().is_empty());
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=2147483648,I=7,p=3;OK"]);
    }

    #[test]
    fn an_id_and_a_number_together_are_refused() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=T,i=1,I=2", 1, 1));
        assert_eq!(
            fx.replies(),
            ["i=1,I=2;EINVAL:image id and image number cannot both be specified"]
        );
        assert!(fx.host.decodes.is_empty());
    }

    // --- put ---

    #[test]
    fn a_put_answers_at_once_in_order_and_shows_when_the_image_lands() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 3, 3));
        let placement = fx.send("a=p,i=1,p=5,c=2").expect("placed while decoding");
        assert!(
            fx.replies().is_empty(),
            "the put's OK queues behind the transmit's parked reply"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK", "i=1,p=5;OK"]);
        assert_eq!(placement.geometry.columns, 2);
        assert!(placement.pixels().is_some());
        // Same image, same placement id: the new one replaces the old.
        let replacement = fx.send("a=p,i=1,p=5").unwrap();
        assert!(placement.is_deleted() && !replacement.is_deleted());
        let other = fx.send("a=p,i=1,p=6").unwrap();
        assert!(!replacement.is_deleted() && !other.is_deleted());
    }

    #[test]
    fn a_put_of_a_missing_failed_or_evicted_image_is_enoent() {
        let mut fx = Fixture::new();
        fx.send("a=p,i=99");
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish(|_| Err(GraphicsError::invalid("broken")));
        fx.send("a=p,i=1");
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.finish_ok();
        fx.send("a=d,d=I,i=2");
        fx.send("a=p,i=2,q=1");
        fx.send("a=p,i=2,q=2");
        assert_eq!(
            fx.replies(),
            [
                "i=99;ENOENT:Put command refers to non-existent image with id: 99 and number: 0",
                "i=1;EINVAL:broken",
                "i=1;ENOENT:Put command refers to non-existent image with id: 1 and number: 0",
                "i=2;OK",
                "i=2;ENOENT:Put command refers to non-existent image with id: 2 and number: 0",
            ]
        );
    }

    // --- anchors ---

    #[test]
    fn cursor_keeping_and_negative_z_placements_are_anchors() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 25, 41));
        fx.finish_ok();
        fx.line_seq = 7;
        fx.column = 12;
        let kept = fx.send("a=p,i=1,C=1").expect("placed");
        assert!(kept.anchored, "C=1 leaves the cursor in place");
        assert_eq!((kept.geometry.columns, kept.geometry.rows), (3, 3));
        assert_eq!((kept.line_seq, kept.column), (7, 12));
        let under = fx.send("a=p,i=1,z=-1").expect("placed");
        assert!(
            under.anchored,
            "a negative z draws under the text that follows"
        );
        assert_eq!(under.geometry.z, -1);
        let over = fx.send("a=p,i=1,z=5").expect("placed");
        assert!(!over.anchored, "a positive z keeps consuming cells");
        let plain = fx.send("a=p,i=1,C=0").expect("placed");
        assert!(!plain.anchored && plain.relative.is_none() && !plain.placeholder);
        assert_eq!(
            fx.replies(),
            ["i=1;OK", "i=1;OK", "i=1;OK", "i=1;OK", "i=1;OK"]
        );
        // The outset covers the box: 3x3 cells of 10x20 is 30 by 60.
        assert_eq!(anchor_outset(&kept.geometry, px(10, 20)), 60);
        assert_eq!(
            anchor_outset(&kept.geometry, (0, 0)),
            54,
            "the fallback cell"
        );
        let huge = PlacementGeometry {
            columns: MAX_PLACEMENT_CELLS,
            rows: 1,
            ..kept.geometry
        };
        assert_eq!(anchor_outset(&huge, px(10, 20)), MAX_EFFECT_OUTSET);
        // An anchor on a full line is refused like any placement.
        fx.objects_free = 0;
        assert!(fx.send("a=p,i=1,C=1").is_none());
        assert_eq!(
            fx.replies(),
            ["i=1;ENOSPC:too many placements on this line"]
        );
    }

    // --- virtual placements and placeholder runs ---

    #[test]
    fn a_virtual_placement_is_stored_and_shown_only_through_placeholders() {
        let mut fx = Fixture::new();
        assert!(!fx.state.has_virtual_placements());
        fx.send(&rgba("a=t,i=1", 20, 20));
        fx.finish_ok();
        assert!(
            fx.send("a=p,U=1,i=1,c=2,r=2").is_none(),
            "nothing at the cursor"
        );
        assert!(fx.state.has_virtual_placements());
        assert_eq!(fx.replies(), ["i=1;OK", "i=1;OK"]);
        let uid = fx.state.image(1).unwrap().uid;
        let entry = &fx.state.entries[&uid];
        assert_eq!(entry.virtuals.len(), 1);
        assert_eq!((entry.virtuals[0].columns, entry.virtuals[0].rows), (2, 2));
        assert!(
            entry.has_live_placements(),
            "a virtual placement keeps the image in use"
        );
        // A virtual placement cannot be relative.
        fx.send("a=p,U=1,i=1,P=1");
        assert_eq!(
            fx.replies(),
            ["i=1;EINVAL:Put command creating a virtual placement cannot refer to a parent"]
        );
        // `a=T` can create one too.
        assert!(fx.send(&rgba("a=T,i=2,U=1,c=1,r=1", 1, 1)).is_none());
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=2;OK"]);
        assert_eq!(
            fx.state.entries[&fx.state.image(2).unwrap().uid]
                .virtuals
                .len(),
            1
        );
    }

    /// An image of `width` x `height` with a `c` x `r` virtual placement.
    fn virtual_image(fx: &mut Fixture, id: u32, (width, height): (u32, u32), keys: &str) {
        fx.send(&rgba(&format!("a=t,i={id}"), width, height));
        fx.finish_ok();
        fx.send(&format!("a=p,U=1,i={id},{keys}"));
        fx.replies();
    }

    #[test]
    fn a_placeholder_run_shows_its_cells_slice_of_the_box() {
        // A 40x40 image in a 4x2 box of 10x20 cells: the box is 40x40 px,
        // so the image fits it exactly, one pixel per box pixel.
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 42, (40, 40), "c=4,r=2");
        fx.line_seq = 3;
        let (run, range) = fx.run((42, 0), (1, 1), 2, 5).expect("a run placement");
        assert!(run.placeholder && !run.anchored && run.relative.is_none());
        assert_eq!(run.id, 0, "runs are never addressable");
        assert_eq!((run.line_seq, run.column), (3, 5));
        assert_eq!(range, 0..8, "both cells");
        assert_eq!(
            run.geometry,
            PlacementGeometry {
                columns: 2,
                rows: 1,
                source: SourceRect {
                    x: 10,
                    y: 20,
                    width: 20,
                    height: 20
                },
                x_offset: 0,
                y_offset: 0,
                fit: Fit::Box {
                    columns: 2,
                    rows: 1
                },
                z: 0,
            }
        );
        assert!(run.pixels().is_some());
        // The run is registered with its image: a delete by id takes it.
        let uid = fx.state.image(42).unwrap().uid;
        assert!(
            fx.state.entries[&uid]
                .live_placements()
                .any(|p| Arc::ptr_eq(&p, &run))
        );
        // Cells past the box show nothing and stay text.
        assert!(
            fx.run((42, 0), (2, 0), 1, 0).is_none(),
            "row 2 of a 2-row box"
        );
        assert!(
            fx.run((42, 0), (0, 4), 1, 0).is_none(),
            "column 4 of a 4-column box"
        );
        // A run straddling the edge keeps only the cells over the image.
        let (edge, range) = fx
            .run((42, 0), (0, 3), 3, 2)
            .expect("one cell over the image");
        assert_eq!((edge.geometry.columns, edge.column), (1, 2));
        assert_eq!(range, 0..4, "the first cell only");
        assert_eq!(edge.geometry.source.x, 30);
    }

    #[test]
    fn a_letterboxed_box_leaves_blank_cells_and_offsets_the_slice() {
        // A 20x40 image in a 4x2 box of 10x20 cells (40x40 px): fit to
        // height, scale 1, centred with a 10 px margin either side — the
        // first and last columns are blank.
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 1, (20, 40), "c=4,r=2");
        let (run, range) = fx.run((1, 0), (0, 0), 4, 0).expect("the middle cells");
        assert_eq!(range, 4..12, "the first and last cells are dropped");
        assert_eq!((run.column, run.geometry.columns), (1, 2));
        assert_eq!(
            run.geometry.source,
            SourceRect {
                x: 0,
                y: 0,
                width: 20,
                height: 20
            }
        );
        assert_eq!((run.geometry.x_offset, run.geometry.y_offset), (0, 0));
        assert!(
            fx.run((1, 0), (0, 0), 1, 0).is_none(),
            "the blank first cell alone"
        );
        // A 30x40 image: a 5 px margin lands inside the first cell.
        virtual_image(&mut fx, 2, (30, 40), "c=4,r=2");
        let (run, range) = fx.run((2, 0), (1, 0), 4, 0).expect("the whole row");
        assert_eq!(range, 0..16);
        assert_eq!((run.geometry.x_offset, run.geometry.y_offset), (5, 0));
        assert_eq!(
            run.geometry.source,
            SourceRect {
                x: 0,
                y: 20,
                width: 30,
                height: 20
            }
        );
        // Without `c` and `r` the box is the image's natural cells.
        virtual_image(&mut fx, 3, (25, 30), "x=0");
        let (natural, _) = fx.run((3, 0), (0, 0), 3, 0).expect("3 columns, 2 rows");
        assert_eq!(natural.geometry.source.width, 25);
        assert_eq!(natural.geometry.columns, 3);
    }

    #[test]
    fn placeholders_name_a_virtual_placement_by_id_or_take_the_newest() {
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 1, (10, 10), "c=1,r=1,p=5");
        fx.send("a=p,U=1,i=1,c=2,r=2,p=6");
        fx.send("a=p,U=1,i=1,c=3,r=3");
        fx.replies();
        assert!(fx.run((1, 5), (0, 0), 1, 0).is_some());
        assert!(fx.run((1, 6), (1, 1), 1, 0).is_some());
        assert!(
            fx.run((1, 5), (1, 1), 1, 0).is_none(),
            "placement 5 is one cell"
        );
        assert!(fx.run((1, 9), (0, 0), 1, 0).is_none(), "no placement 9");
        assert!(
            fx.run((1, 0), (2, 2), 1, 0).is_some(),
            "id 0: the newest, 3x3"
        );
        assert!(fx.run((2, 0), (0, 0), 1, 0).is_none(), "no image 2");
        // A new anonymous virtual placement supersedes the old one; a new
        // one with an id supersedes that id.
        fx.send("a=p,U=1,i=1,c=1,r=1");
        fx.send("a=p,U=1,i=1,c=1,r=1,p=6");
        fx.replies();
        let uid = fx.state.image(1).unwrap().uid;
        assert_eq!(fx.state.entries[&uid].virtuals.len(), 3);
        assert!(fx.run((1, 0), (2, 2), 1, 0).is_none());
        assert!(fx.run((1, 6), (1, 1), 1, 0).is_none());
        // A full line takes no run.
        fx.objects_free = 0;
        assert!(fx.run((1, 5), (0, 0), 1, 0).is_none());
    }

    #[test]
    fn deleting_a_virtual_placement_takes_its_runs_and_only_id_targets_reach_it() {
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 1, (20, 20), "c=2,r=2,p=5");
        fx.send("a=p,U=1,i=1,c=2,r=2,p=6");
        fx.replies();
        let (five, _) = fx.run((1, 5), (0, 0), 2, 0).unwrap();
        let (six, _) = fx.run((1, 6), (0, 0), 2, 0).unwrap();
        let (any, _) = fx.run((1, 0), (1, 0), 2, 0).unwrap();
        // Positional targets and `a` never touch virtual placements or runs
        // (`x=1,y=1` is the runs' own cell: line 0 is row 1 of a screen that
        // has not scrolled yet).
        for command in [
            "a=d",
            "a=d,d=c",
            "a=d,d=p,x=1,y=1",
            "a=d,d=p,x=1,y=24",
            "a=d,d=x,x=1",
            "a=d,d=z,z=0",
        ] {
            fx.send(command);
        }
        assert!(!five.is_deleted() && !six.is_deleted() && !any.is_deleted());
        assert!(fx.state.has_virtual_placements());
        // Deleting placement 5 takes the runs made from it, no others.
        fx.send("a=d,d=i,i=1,p=5");
        assert!(five.is_deleted());
        assert!(!six.is_deleted() && !any.is_deleted());
        assert!(
            fx.run((1, 5), (0, 0), 1, 0).is_none(),
            "placement 5 is gone"
        );
        assert!(fx.state.has_virtual_placements());
        // Deleting the image's placements takes every run and virtual placement.
        fx.send("a=d,d=i,i=1");
        assert!(six.is_deleted() && any.is_deleted());
        assert!(!fx.state.has_virtual_placements());
        assert!(fx.state.image(1).is_some(), "lowercase keeps the data");
        // Uppercase frees an image left with only a virtual placement.
        fx.send("a=p,U=1,i=1,c=1,r=1");
        fx.replies();
        fx.send("a=d,d=I,i=1");
        assert!(fx.state.image(1).is_none());
        assert!(!fx.state.has_virtual_placements());
        // By number and by range too.
        fx.send(&rgba("a=t,I=9", 1, 1));
        fx.finish_ok();
        fx.send("a=p,I=9,U=1,c=1,r=1");
        fx.replies();
        assert!(fx.state.has_virtual_placements());
        fx.send("a=d,d=n,I=9");
        assert!(!fx.state.has_virtual_placements());
        fx.send("a=p,I=9,U=1,c=1,r=1,p=3");
        fx.send("a=d,d=r,x=2147483648,y=2147483648");
        assert!(!fx.state.has_virtual_placements());
        // Eviction drops virtual placements with their image.
        let mut small = Fixture::with_quota(16);
        virtual_image(&mut small, 1, (2, 2), "c=1,r=1");
        small.send(&rgba("a=t,i=2", 2, 2));
        assert!(small.state.image(1).is_none() && !small.state.has_virtual_placements());
    }

    #[test]
    fn a_real_and_a_virtual_placement_share_the_id_space() {
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 1, (10, 10), "c=1,r=1,p=5");
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.finish_ok();
        fx.send("a=p,i=2,p=1,P=1,Q=5");
        fx.replies();
        let uid = fx.state.image(1).unwrap().uid;
        let hanging = fx.state.entries[&uid]
            .virtual_for(5)
            .unwrap()
            .child_placements()
            .pop()
            .unwrap();
        let real = fx.send("a=p,i=1,p=5").expect("placed");
        assert!(
            !fx.state.has_virtual_placements(),
            "the real placement 5 took over"
        );
        assert!(
            hanging.is_deleted(),
            "and what hung from the virtual one went with it"
        );
        assert_eq!(fx.replies(), ["i=1,p=5;OK"]);
        fx.send("a=p,U=1,i=1,c=1,r=1,p=5");
        assert!(real.is_deleted(), "and the virtual one took it back");
        assert!(fx.state.has_virtual_placements());
    }

    // --- relative placements ---

    #[test]
    fn a_relative_placement_lives_in_its_parent_and_never_at_the_cursor() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 10, 20));
        fx.send(&rgba("a=t,i=2", 10, 20));
        fx.finish_ok();
        fx.finish_ok();
        fx.line_seq = 10;
        fx.column = 4;
        let parent = fx.send("a=p,i=1,p=1").expect("placed");
        fx.column = 9;
        assert!(
            fx.send("a=p,i=2,p=7,P=1,Q=1,H=-3,V=4").is_none(),
            "not at the cursor"
        );
        assert_eq!(
            fx.replies(),
            ["i=1;OK", "i=2;OK", "i=1,p=1;OK", "i=2,p=7;OK"]
        );
        let children = parent.child_placements();
        assert_eq!(children.len(), 1);
        let child = &children[0];
        assert!(child.anchored && !child.placeholder);
        assert_eq!(
            child.relative,
            Some(RelativeOffset {
                columns: -3,
                rows: 4
            })
        );
        assert_eq!((child.id, child.image.id), (7, 2));
        assert_eq!(
            child.cell(),
            Some((14, 1)),
            "its parent's cell plus the offset"
        );
        assert_eq!(
            (child.line_seq, child.column),
            (0, 0),
            "attached to no line of its own"
        );
        assert_eq!((child.geometry.columns, child.geometry.rows), (1, 1));
        assert!(Arc::ptr_eq(&child.parent().unwrap(), &parent));
        // Registered with its own image too: `d=i` finds it, and it keeps
        // the image in use.
        let uid = fx.state.image(2).unwrap().uid;
        assert!(
            fx.state.entries[&uid]
                .live_placements()
                .any(|p| Arc::ptr_eq(&p, child))
        );
        // A negative offset past the line's start clamps.
        fx.send("a=p,i=2,p=8,P=1,Q=1,H=-100,V=-100");
        let late = parent.child_placements().pop().unwrap();
        assert_eq!(late.cell(), Some((0, 0)));
        assert_eq!(fx.replies(), ["i=2,p=8;OK"]);
        // Without `Q`, the parent's oldest placement is the parent.
        let newer = fx.send("a=p,i=1,p=2").unwrap();
        fx.send("a=p,i=2,p=9,P=1");
        assert_eq!(parent.child_placements().len(), 3);
        assert!(newer.child_placements().is_empty());
        assert_eq!(fx.replies(), ["i=1,p=2;OK", "i=2,p=9;OK"]);
    }

    #[test]
    fn a_relative_placement_s_parent_must_exist() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.finish_ok();
        fx.finish_ok();
        fx.replies();
        fx.send("a=p,i=1,p=1,P=5");
        fx.send("a=p,i=1,p=1,P=2");
        // Placements live as long as their lines: held here.
        let three = fx.send("a=p,i=2,p=3").expect("placed");
        fx.send("a=p,i=1,p=1,P=2,Q=9");
        fx.send("a=p,U=1,i=2,c=1,r=1,p=4");
        fx.send("a=p,i=1,p=1,P=2,Q=4");
        fx.send("a=p,i=1,p=1,P=2,Q=3");
        let one = fx.send("a=p,i=1,p=1").expect("placed at the cursor");
        fx.send("a=p,i=1,p=1,P=1,Q=1");
        fx.send("a=p,i=1,p=2,P=1,Q=2");
        assert_eq!(
            fx.replies(),
            [
                "i=1,p=1;ENOPARENT:Put command refers to a parent image with id: 5 that does not exist",
                "i=1,p=1;ENOPARENT:Put command refers to a parent image with id: 2 that has no placements",
                "i=2,p=3;OK",
                "i=1,p=1;ENOPARENT:Put command refers to a parent image placement with id: 2 and placement id: 9 that does not exist",
                "i=2,p=4;OK",
                "i=1,p=1;OK",
                "i=1,p=1;OK",
                "i=1,p=1;OK",
                "i=1,p=1;EINVAL:Put command refers to itself as its own parent",
                "i=1,p=2;ENOPARENT:Put command refers to a parent image placement with id: 1 and placement id: 2 that does not exist",
            ]
        );
        assert!(!one.is_deleted() && !three.is_deleted());
        // A placement whose line is gone is no parent.
        drop(one);
        fx.send("a=p,i=1,p=2,P=1,Q=1");
        assert_eq!(
            fx.replies(),
            [
                "i=1,p=2;ENOPARENT:Put command refers to a parent image placement with id: 1 and placement id: 1 that does not exist"
            ]
        );
        // With only a virtual placement left, `P` alone names it.
        fx.send("a=d,d=i,i=2,p=3");
        assert!(three.is_deleted());
        fx.send("a=p,i=1,p=5,P=2");
        assert_eq!(fx.replies(), ["i=1,p=5;OK"]);
        let uid = fx.state.image(2).unwrap().uid;
        let virt = fx.state.entries[&uid].virtual_for(4).unwrap().clone();
        let five = virt
            .child_placements()
            .pop()
            .expect("hangs from the virtual placement");
        assert_eq!((five.id, five.depth(), five.cell()), (5, 1, None));
        // A child of a deleted parent: ENOPARENT.
        let root = fx.send("a=p,i=2,p=3").unwrap();
        fx.send("a=d,d=i,i=2,p=3");
        assert!(root.is_deleted());
        fx.send("a=p,i=1,p=6,P=2,Q=3");
        assert_eq!(
            fx.replies(),
            [
                "i=2,p=3;OK",
                "i=1,p=6;ENOPARENT:Put command refers to a parent image placement with id: 2 and placement id: 3 that does not exist"
            ]
        );
        // `Q` without `P` is not a relative placement at all, as in kitty.
        assert!(fx.send("a=p,i=1,Q=3").is_some());
        assert_eq!(fx.replies(), ["i=1;OK"]);
    }

    #[test]
    fn chains_of_eight_are_allowed_and_a_ninth_link_is_too_deep() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        let root = fx.send("a=p,i=1,p=100").unwrap();
        fx.replies();
        for link in 1..=8 {
            fx.send(&format!("a=p,i=1,p={},P=1,Q={}", 100 + link, 99 + link));
            assert_eq!(
                fx.replies(),
                [format!("i=1,p={};OK", 100 + link)],
                "link {link}"
            );
        }
        fx.send("a=p,i=1,p=109,P=1,Q=108");
        assert_eq!(
            fx.replies(),
            ["i=1,p=109;ETOODEEP:Too many levels of parent references"]
        );
        // Deleting the root cascades down the whole chain.
        let uid = fx.state.image(1).unwrap().uid;
        let eighth = fx.state.entries[&uid].named[&108].upgrade().unwrap();
        assert_eq!(eighth.depth(), 8);
        fx.send("a=d,d=i,i=1,p=100");
        assert!(root.is_deleted() && eighth.is_deleted());
    }

    #[test]
    fn a_cycle_is_refused_and_a_replaced_parent_keeps_its_children() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.finish_ok();
        fx.finish_ok();
        let a = fx.send("a=p,i=1,p=1").unwrap();
        fx.send("a=p,i=2,p=2,P=1,Q=1");
        fx.send("a=p,i=1,p=3,P=2,Q=2");
        fx.replies();
        // A relative to C, which descends from A: a loop.
        fx.send("a=p,i=1,p=1,P=1,Q=3");
        assert_eq!(
            fx.replies(),
            ["i=1,p=1;ECYCLE:This parent reference creates a cycle"]
        );
        assert!(!a.is_deleted(), "the refused command changed nothing");
        assert_eq!(a.child_placements().len(), 1);
        // Re-placing A (a move) keeps B under it, under the new A — and B
        // and C sit at the new A's cell from then on.
        fx.line_seq = 5;
        fx.column = 3;
        let moved = fx.send("a=p,i=1,p=1,c=2").unwrap();
        assert!(a.is_deleted());
        let b = moved.child_placements().pop().expect("B moved with A");
        assert_eq!(b.id, 2);
        assert!(!b.is_deleted());
        assert!(Arc::ptr_eq(&b.parent().unwrap(), &moved));
        assert!(a.child_placements().is_empty());
        let c = b.child_placements().pop().expect("C stayed under B");
        assert_eq!((b.cell(), c.cell()), (Some((5, 3)), Some((5, 3))));
        // Re-placing B as a child of another parent takes it out of A's list.
        let d = fx.send("a=p,i=2,p=4").unwrap();
        fx.send("a=p,i=2,p=2,P=2,Q=4,H=1");
        assert!(moved.child_placements().is_empty());
        let b2 = d.child_placements().pop().unwrap();
        assert!(b.is_deleted() && !b2.is_deleted());
        assert_eq!(b2.child_placements().len(), 1, "C followed B");
        assert_eq!((b2.cell(), c.cell()), (Some((5, 4)), Some((5, 4))));
    }

    #[test]
    fn deleting_a_parent_cascades_and_uppercase_frees_the_children_s_images() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.send(&rgba("a=t,i=3", 1, 1));
        for _ in 0..3 {
            fx.finish_ok();
        }
        let parent = fx.send("a=p,i=1,p=1").unwrap();
        fx.send("a=p,i=2,p=1,P=1,Q=1");
        fx.send("a=p,i=3,p=1,P=2,Q=1");
        let other = fx.send("a=p,i=3,p=2").unwrap();
        fx.replies();
        let child = parent.child_placements().pop().unwrap();
        let grandchild = child.child_placements().pop().unwrap();
        fx.send("a=d,d=i,i=1,p=1");
        assert!(parent.is_deleted() && child.is_deleted() && grandchild.is_deleted());
        assert!(!other.is_deleted());
        assert!(fx.state.image(2).is_some(), "lowercase frees nothing");
        // Again, uppercase: image 2 had only the child; image 3 still shows.
        let parent = fx.send("a=p,i=1,p=1").unwrap();
        fx.send("a=p,i=2,p=1,P=1,Q=1");
        fx.send("a=p,i=3,p=3,P=2,Q=1");
        fx.replies();
        fx.send("a=d,d=I,i=1,p=1");
        assert!(parent.is_deleted());
        assert!(fx.state.image(1).is_none() && fx.state.image(2).is_none());
        assert!(
            fx.state.image(3).is_some(),
            "placement 2 of image 3 still shows it"
        );
        assert!(!other.is_deleted());
    }

    #[test]
    fn re_placing_a_chain_under_fresh_roots_is_refused_once_it_would_be_too_deep() {
        // Each round makes a fresh root and re-places the previous round's
        // root under it: the chain below the re-placed one is adopted a
        // level deeper every time, with no single link ever the ninth.
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        let mut roots = vec![fx.send("a=p,i=1,p=1").expect("placed")];
        fx.send("a=p,i=1,p=2,P=1,Q=1");
        fx.replies();
        let uid = fx.state.image(1).unwrap().uid;
        let leaf = fx.state.entries[&uid].named[&2].upgrade().unwrap();
        let mut top = 1;
        for round in 1..=8_u32 {
            let fresh = 100 + round;
            roots.push(fx.send(&format!("a=p,i=1,p={fresh}")).expect("placed"));
            fx.send(&format!("a=p,i=1,p={top},P=1,Q={fresh}"));
            // After round n the leaf is n + 1 links down: the eighth round
            // would make it the ninth.
            let verdict = if round < 8 {
                format!("i=1,p={top};OK")
            } else {
                format!("i=1,p={top};ETOODEEP:Too many levels of parent references")
            };
            assert_eq!(
                fx.replies(),
                [format!("i=1,p={fresh};OK"), verdict],
                "round {round}"
            );
            top = fresh;
        }
        assert_eq!(leaf.depth(), PLACEMENT_DEPTH_LIMIT);
        assert!(!leaf.is_deleted());
        assert!(
            roots.last().unwrap().child_placements().is_empty(),
            "the refused command changed nothing"
        );
        // Deleting the image's placements walks the whole chain.
        fx.send("a=d,d=i,i=1");
        assert!(leaf.is_deleted());
        assert!(roots.iter().all(|root| root.is_deleted()));
    }

    #[test]
    fn a_chain_far_past_the_limit_is_deleted_without_recursion() {
        // Nothing on the wire can build this any more; the walks must not
        // rely on that. On an eighth of a worker thread's stack a recursive
        // walk overflowed long before ten thousand links.
        let record = Arc::new(ImageRecord::new(1, 0, 1, 1));
        let geometry = geometry(&control("a=p"), 1, 1, px(10, 20));
        let root = Arc::new(Placement::new(record.clone(), 1, geometry));
        let mut nodes = vec![root.clone()];
        for _ in 0..10_000 {
            let parent = nodes.last().unwrap().clone();
            let child = Arc::new(Placement {
                relative: Some(RelativeOffset::default()),
                ..Placement::new(record.clone(), 0, geometry)
            });
            child.set_parent(ParentLink::Real(Arc::downgrade(&parent)));
            parent.children.write().unwrap().push(child.clone());
            nodes.push(child);
        }
        let worker = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                assert!(root.subtree_height() > PLACEMENT_DEPTH_LIMIT);
                assert_eq!(
                    nodes.last().unwrap().depth(),
                    PLACEMENT_DEPTH_LIMIT + 1,
                    "counted one past the limit"
                );
                assert_eq!(
                    nodes.last().unwrap().cell(),
                    None,
                    "too far to reach a cell"
                );
                let mut freed = Vec::new();
                root.mark_deleted_into(&mut freed);
                assert_eq!(freed.len(), 10_000);
                assert!(nodes.iter().all(|node| node.is_deleted()));
                assert!(
                    nodes.iter().all(|node| node.child_placements().is_empty()),
                    "the lists are released"
                );
                nodes
            })
            .unwrap();
        // The lists are empty, so dropping the nodes nests nothing.
        drop(worker.join().unwrap());
    }

    #[test]
    fn a_parent_holds_at_most_a_line_s_worth_of_children_and_sheds_deleted_ones() {
        let mut fx = Fixture::new();
        fx.grid = (80, 1);
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.finish_ok();
        fx.finish_ok();
        let parent = fx.send("a=p,i=1,p=1").unwrap();
        fx.replies();
        for _ in 0..MAX_INLINE_OBJECTS {
            fx.send("a=p,i=2,P=1,Q=1");
            assert_eq!(fx.replies(), ["i=2;OK"]);
        }
        for _ in 0..1000 {
            fx.send("a=p,i=2,P=1,Q=1");
            assert_eq!(
                fx.replies(),
                ["i=2;ENOSPC:too many placements relative to this one"]
            );
        }
        assert_eq!(parent.child_placements().len(), MAX_INLINE_OBJECTS);
        // Deleting the children leaves the list empty, not full of
        // tombstones.
        fx.send("a=d,d=i,i=2");
        assert!(parent.child_placements().is_empty());
        assert!(!parent.is_deleted());
        // Named children: one replaced takes its own slot; one deleted by
        // id or by position frees its slot.
        for id in 1..=MAX_INLINE_OBJECTS {
            fx.send(&format!("a=p,i=2,p={id},P=1,Q=1,H={id}"));
        }
        fx.replies();
        assert_eq!(parent.child_placements().len(), MAX_INLINE_OBJECTS);
        fx.send("a=p,i=2,p=5,P=1,Q=1,H=5,V=1");
        assert_eq!(fx.replies(), ["i=2,p=5;OK"]);
        assert_eq!(parent.child_placements().len(), MAX_INLINE_OBJECTS);
        fx.send("a=p,i=2,p=99,P=1,Q=1");
        assert_eq!(
            fx.replies(),
            ["i=2,p=99;ENOSPC:too many placements relative to this one"]
        );
        fx.send("a=d,d=i,i=2,p=5");
        assert_eq!(parent.child_placements().len(), MAX_INLINE_OBJECTS - 1);
        // Placement 7 sits at column 7 of the parent's line, row 1 of a
        // one-row screen.
        fx.send("a=d,d=p,x=8,y=1");
        assert_eq!(parent.child_placements().len(), MAX_INLINE_OBJECTS - 2);
        assert!(
            parent
                .child_placements()
                .iter()
                .all(|child| child.id != 5 && child.id != 7)
        );
        fx.send("a=p,i=2,p=99,P=1,Q=1");
        assert_eq!(fx.replies(), ["i=2,p=99;OK"]);
    }

    #[test]
    fn a_relative_placement_may_hang_from_a_virtual_placement() {
        // The specification's headline use: a one-pixel image placed by a
        // placeholder, which moves with the text, and a real image
        // positioned relative to it.
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 1, (1, 1), "c=1,r=1,p=5");
        fx.send(&rgba("a=t,i=2", 20, 40));
        fx.finish_ok();
        fx.replies();
        fx.send("a=p,i=2,p=7,P=1,Q=5,H=1,V=2");
        assert_eq!(fx.replies(), ["i=2,p=7;OK"]);
        let uid = fx.state.image(1).unwrap().uid;
        let virt = fx.state.entries[&uid].virtual_for(5).unwrap().clone();
        let child = virt
            .child_placements()
            .pop()
            .expect("hangs from the virtual placement");
        assert!(child.anchored && !child.placeholder);
        assert_eq!(
            child.relative,
            Some(RelativeOffset {
                columns: 1,
                rows: 2
            })
        );
        assert_eq!((child.id, child.image.id, child.depth()), (7, 2, 1));
        assert_eq!(child.cell(), None, "no cell of its own");
        let image_2 = fx.state.image(2).unwrap().uid;
        assert_eq!(
            fx.state.entries[&image_2].live_placements().count(),
            1,
            "registered with its own image"
        );
        // The run showing the box's top-left cell draws it — a child placed
        // after the placeholder was printed included: the list stays the
        // virtual placement's.
        fx.line_seq = 3;
        let (run, _) = fx.run((1, 5), (0, 0), 1, 0).unwrap();
        assert!(Arc::ptr_eq(&run.child_placements()[0], &child));
        assert!(run.children.read().unwrap().is_empty());
        fx.send("a=p,i=2,p=8,P=1,Q=5");
        // Without `Q`, the virtual placement is the oldest placement of
        // its image.
        fx.send("a=p,i=2,p=9,P=1");
        assert_eq!(fx.replies(), ["i=2,p=8;OK", "i=2,p=9;OK"]);
        assert_eq!(run.child_placements().len(), 3);
        // The virtual link is one of the eight.
        for link in 1..=7 {
            fx.send(&format!("a=p,i=2,p={},P=2,Q={}", 9 + link, 8 + link));
            assert_eq!(fx.replies(), [format!("i=2,p={};OK", 9 + link)]);
        }
        fx.send("a=p,i=2,p=17,P=2,Q=16");
        assert_eq!(
            fx.replies(),
            ["i=2,p=17;ETOODEEP:Too many levels of parent references"]
        );
        // A real placement taking the virtual one's id would be its own
        // parent.
        fx.send("a=p,i=1,p=5,P=1,Q=5");
        assert_eq!(
            fx.replies(),
            ["i=1,p=5;EINVAL:Put command refers to itself as its own parent"]
        );
        // Positional deletes read no cell for them; `z` needs none.
        fx.send("a=p,i=2,p=20,P=1,Q=5,z=3");
        fx.replies();
        for command in [
            "a=d",
            "a=d,d=c",
            "a=d,d=p,x=1,y=1",
            "a=d,d=p,x=2,y=3",
            "a=d,d=x,x=2",
            "a=d,d=y,y=1",
        ] {
            fx.send(command);
        }
        assert_eq!(virt.child_placements().len(), 4);
        assert!(!child.is_deleted() && !run.is_deleted());
        fx.send("a=d,d=z,z=3");
        assert_eq!(virt.child_placements().len(), 3);
        // Deleting the virtual placement cascades to the runs and to what
        // hangs from it; uppercase frees the children's image.
        fx.send("a=d,d=I,i=1,p=5");
        assert!(virt.is_deleted() && run.is_deleted() && child.is_deleted());
        assert!(run.child_placements().is_empty());
        assert!(fx.state.image(2).is_none(), "nothing else showed image 2");
        assert!(fx.state.image(1).is_none());
        assert!(fx.replies().is_empty());
    }

    #[test]
    fn only_the_run_showing_the_box_s_top_left_cell_draws_what_hangs_from_it() {
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 1, (20, 40), "c=2,r=2,p=1");
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.finish_ok();
        fx.send("a=p,i=2,p=1,P=1,Q=1");
        fx.replies();
        let (first_row, _) = fx.run((1, 1), (0, 0), 2, 0).unwrap();
        let (second_row, _) = fx.run((1, 1), (1, 0), 2, 0).unwrap();
        let (right_half, _) = fx.run((1, 1), (0, 1), 1, 0).unwrap();
        assert_eq!(first_row.child_placements().len(), 1);
        assert!(second_row.child_placements().is_empty());
        assert!(right_half.child_placements().is_empty());
        // Letterboxing that leaves the top-left cell blank: no run shows
        // it, so none draws them.
        virtual_image(&mut fx, 3, (10, 40), "c=4,r=2,p=1");
        fx.send("a=p,i=2,p=2,P=3,Q=1");
        fx.replies();
        let (blank_left, _) = fx.run((3, 1), (0, 0), 4, 0).unwrap();
        assert_eq!(blank_left.column, 1, "the blank first cell is dropped");
        assert!(blank_left.child_placements().is_empty());
        // A partly blank first cell is still the top-left cell.
        virtual_image(&mut fx, 4, (10, 40), "c=2,r=2,p=1");
        fx.send("a=p,i=2,p=3,P=4,Q=1");
        fx.replies();
        let (partly, _) = fx.run((4, 1), (0, 0), 2, 0).unwrap();
        assert_eq!((partly.column, partly.geometry.x_offset), (0, 5));
        assert_eq!(partly.child_placements().len(), 1);
        // A virtual placement re-created under its id takes over what hung
        // from the old one; the runs made from the old one show it no more.
        fx.send("a=p,U=1,i=1,c=2,r=2,p=1");
        fx.replies();
        assert!(first_row.child_placements().is_empty());
        assert!(!first_row.is_deleted());
        let (again, _) = fx.run((1, 1), (0, 0), 2, 0).unwrap();
        let moved = again.child_placements();
        assert_eq!(moved.len(), 1);
        assert_eq!((moved[0].id, moved[0].depth()), (1, 1));
        assert!(!moved[0].is_deleted());
    }

    // --- positional deletes ---

    /// Image 1, ready; placements of it at (line, column) with `keys`.
    fn placed_at(fx: &mut Fixture, cells: &[(u64, u16, &str)]) -> Vec<Arc<Placement>> {
        fx.send(&rgba("a=t,i=1", 20, 40));
        fx.finish_ok();
        let out = cells
            .iter()
            .map(|(line, column, keys)| {
                fx.line_seq = *line;
                fx.column = *column;
                fx.send(&format!("a=p,i=1,{keys}")).expect("placed")
            })
            .collect();
        fx.replies();
        out
    }

    fn deleted_flags(placements: &[Arc<Placement>]) -> Vec<bool> {
        placements.iter().map(|p| p.is_deleted()).collect()
    }

    #[test]
    fn deletes_at_the_cursor_and_at_a_cell_read_the_screen_from_the_pending_line() {
        let mut fx = Fixture::new();
        fx.grid = (80, 10);
        // Line 100 is the pending line (row 10); line 91 is row 1. Each
        // placement is 2 columns by 2 rows (20x40 px in 10x20 cells).
        let placed = placed_at(
            &mut fx,
            &[
                (100, 0, "p=1"),
                (100, 5, "p=2"),
                (99, 5, "p=3"),
                (91, 0, "p=4"),
                (89, 0, "p=5"),
            ],
        );
        fx.line_seq = 100;
        fx.column = 6;
        fx.send("a=d,d=c");
        assert_eq!(
            deleted_flags(&placed),
            [false, true, true, false, false],
            "the cursor cell (100, 6) is under placement 2 and the second row of 3"
        );
        fx.column = 2;
        fx.send("a=d,d=c");
        assert!(!deleted_flags(&placed)[0], "placement 1 ends at column 2");
        // `x=1,y=1` is the top-left cell: line 91, column 0.
        fx.send("a=d,d=p,x=1,y=1");
        assert!(deleted_flags(&placed)[3]);
        assert!(
            !deleted_flags(&placed)[4],
            "lines 89..=90 are scrollback: a placement at 90 would reach row 1"
        );
        fx.send("a=d,d=p,x=1,y=10");
        assert!(deleted_flags(&placed)[0]);
        // Zero coordinates match nothing, as they are outside the grid.
        let more = placed_at(&mut fx, &[(100, 0, "p=6")]);
        fx.send("a=d,d=p,x=0,y=10");
        fx.send("a=d,d=p,x=1,y=0");
        assert!(!more[0].is_deleted());
        assert!(fx.replies().is_empty(), "deletes never answer");
    }

    #[test]
    fn deletes_by_column_row_and_z_match_their_axes_on_screen() {
        let mut fx = Fixture::new();
        fx.grid = (80, 10);
        let placed = placed_at(
            &mut fx,
            &[
                (100, 0, "p=1,z=3"),
                (95, 3, "p=2,z=3"),
                (89, 0, "p=3,z=3"),
                (97, 0, "p=4,z=-2"),
            ],
        );
        fx.line_seq = 100;
        fx.send("a=d,d=x,x=4");
        assert_eq!(
            deleted_flags(&placed),
            [false, true, false, false],
            "columns 3..5 cover x=4"
        );
        fx.send("a=d,d=x,x=1");
        assert_eq!(
            deleted_flags(&placed),
            [true, true, false, true],
            "placement 3 is in scrollback"
        );
        let placed = placed_at(
            &mut fx,
            &[(100, 0, "p=1,z=3"), (95, 3, "p=2,z=3"), (94, 0, "p=3")],
        );
        fx.line_seq = 100;
        // Row 5 is line 95; placement 3 at line 94 spans lines 94..=95.
        fx.send("a=d,d=y,y=5");
        assert_eq!(deleted_flags(&placed), [false, true, true]);
        fx.send("a=d,d=q,x=1,y=10,z=1");
        assert!(!placed[0].is_deleted(), "z must match for q");
        fx.send("a=d,d=q,x=1,y=10,z=3");
        assert!(placed[0].is_deleted());
        let placed = placed_at(&mut fx, &[(100, 0, "z=3"), (1, 0, "z=3"), (100, 0, "z=4")]);
        fx.line_seq = 100;
        fx.send("a=d,d=z,z=3");
        assert_eq!(
            deleted_flags(&placed),
            [true, true, false],
            "z ignores the screen"
        );
        // Uppercase frees the image once nothing shows it.
        fx.send("a=d,d=Z,z=4");
        assert!(placed[2].is_deleted());
        assert!(fx.state.image(1).is_none());
        // The spec's examples parse and run.
        let placed = placed_at(&mut fx, &[(100, 2, "z=-1"), (100, 2, "p=9")]);
        fx.line_seq = 100;
        fx.send("a=d,d=Z,z=-1");
        assert_eq!(deleted_flags(&placed), [true, false]);
        assert!(fx.state.image(1).is_some(), "placement 9 still shows it");
        fx.grid = (80, 24);
        fx.send("a=d,d=p,x=3,y=24");
        assert!(placed[1].is_deleted());
    }

    #[test]
    fn positional_deletes_reach_relative_placements_through_their_parent_s_cell() {
        let mut fx = Fixture::new();
        fx.grid = (80, 10);
        let placed = placed_at(&mut fx, &[(100, 0, "p=1")]);
        fx.send("a=p,i=1,p=2,P=1,Q=1,H=10,V=-3");
        fx.replies();
        let child = placed[0].child_placements().pop().unwrap();
        assert_eq!(child.cell(), Some((97, 10)));
        fx.send("a=d,d=p,x=11,y=7");
        assert!(child.is_deleted() && !placed[0].is_deleted());
        fx.send("a=d,d=f,i=1");
        assert!(!placed[0].is_deleted(), "frame deletes are not supported");
    }

    // --- query ---

    #[test]
    fn a_query_is_checked_at_once_and_stores_nothing() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=31", 1, 1));
        fx.finish_ok();
        let stored = fx.state.image(31).unwrap();
        fx.send("i=31,s=1,v=1,a=q,t=d,f=24;AAAA");
        fx.send("a=q,i=32,s=2,v=1,f=24;AAAA");
        fx.send(&format!(
            "a=q,i=33,s=2,v=1,f=24,o=z;{}",
            encode_payload(&zlib(&[0; 6]))
        ));
        fx.send(&format!(
            "a=q,i=34,s=1,v=1,f=24,o=z;{}",
            encode_payload(&zlib(&[0; 6]))
        ));
        fx.send(&format!(
            "a=q,i=35,f=100;{}",
            encode_payload(&png_header(5, 5))
        ));
        fx.send(&format!(
            "a=q,i=36,f=100,o=z;{}",
            encode_payload(&zlib(&png_header(5, 5)))
        ));
        fx.send("a=q,i=37,f=100;AAAA");
        // A query names its image by `i`; one without is refused without a
        // word, as kitty refuses it ("Query graphics command without image
        // id"), an image number notwithstanding.
        fx.send("a=q,I=38,s=1,v=1,f=24;AAAA");
        fx.send("a=q,s=1,v=1,f=24;AAAA");
        assert_eq!(
            fx.replies(),
            [
                "i=31;OK",
                "i=31;OK",
                "i=32;ENODATA:Insufficient image data: 3 < 6",
                "i=33;OK",
                "i=34;EFBIG:Too much data",
                "i=35;OK",
                "i=36;OK",
                "i=37;EINVAL:not a PNG image",
            ]
        );
        assert!(
            fx.host.decodes.is_empty(),
            "a small query asks the host for nothing"
        );
        assert_eq!(fx.state.image_count(), 1);
        assert!(
            Arc::ptr_eq(&fx.state.image(31).unwrap(), &stored),
            "a query never replaces an image"
        );
    }

    #[test]
    fn a_compressed_query_too_large_to_inflate_here_is_checked_by_the_host() {
        let mut fx = Fixture::new();
        // 1024x1024 RGBA inflates to 4 MiB: past what this thread inflates.
        let packed = zlib(&[0; 16]);
        fx.send(&format!(
            "a=q,i=40,f=32,s=1024,v=1024,o=z;{}",
            encode_payload(&packed)
        ));
        assert!(fx.replies().is_empty(), "the answer waits for the host");
        let (_, request) = &fx.host.decodes[0];
        assert_eq!(
            (
                request.format,
                request.compression,
                request.width,
                request.height
            ),
            (Format::Rgba, Compression::Zlib, 1024, 1024)
        );
        assert_eq!(request.data, packed, "handed over as it came");
        assert_eq!(fx.state.decodes_in_flight(), 1);
        assert_eq!(fx.state.image_count(), 0, "a query stores nothing");
        assert_eq!(fx.state.used_bytes(), 0, "and holds no image quota");
        assert_eq!(
            fx.state.query_bytes(),
            4 * 1024 * 1024,
            "but its canvas, while the host decodes it"
        );
        // Everything behind it keeps its place.
        fx.send("a=p,i=41");
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=40;OK",
                "i=41;ENOENT:Put command refers to non-existent image with id: 41 and number: 0",
            ]
        );
        assert_eq!(fx.state.image_count(), 0);
        assert_eq!(fx.state.used_bytes(), 0);
        assert_eq!(fx.state.query_bytes(), 0, "released as it landed");
        assert_eq!(fx.state.decodes_in_flight(), 0);
        // The host's verdict is the answer.
        fx.send(&format!(
            "a=q,i=42,f=24,s=1000,v=1000,o=z;{}",
            encode_payload(&packed)
        ));
        fx.finish(|_| {
            Err(GraphicsError::new(
                ErrorCode::NoData,
                "Insufficient image data: 16 < 3000000",
            ))
        });
        assert_eq!(
            fx.replies(),
            ["i=42;ENODATA:Insufficient image data: 16 < 3000000"]
        );
        // A PNG's inflated size is unknown: it is inflated here up to the
        // cap, and past that the host takes it.
        let mut big = png_header(5, 5);
        big.resize(2 * 1024 * 1024, 0);
        fx.send(&format!(
            "a=q,i=43,f=100,o=z;{}",
            encode_payload(&zlib(&big))
        ));
        assert!(fx.replies().is_empty());
        assert_eq!(fx.host.decodes.len(), 1);
        assert_eq!(
            (fx.host.decodes[0].1.width, fx.host.decodes[0].1.height),
            (5, 5)
        );
        fx.finish(|_| Ok(pixels_for(5, 5)));
        assert_eq!(fx.replies(), ["i=43;OK"]);
        assert_eq!(fx.state.image_count(), 0);
    }

    #[test]
    fn parked_queries_are_bounded_by_the_quota_like_images() {
        // A 513x512 RGBA query inflates past what this thread inflates, so
        // it parks at the host and holds its canvas meanwhile: three of
        // them do not fit a 3 MiB quota.
        const CANVAS: u64 = 513 * 512 * 4;
        let mut fx = Fixture::with_quota(3 * 1024 * 1024);
        let compressed = zlib(&[0; 16]);
        let packed = encode_payload(&compressed);
        fx.send(&format!("a=q,i=40,f=32,s=513,v=512,o=z;{packed}"));
        fx.send(&format!("a=q,i=41,f=32,s=513,v=512,o=z;{packed}"));
        assert_eq!(fx.host.decodes.len(), 2, "both parked");
        assert_eq!(fx.state.query_bytes(), 2 * CANVAS);
        assert_eq!(fx.state.used_bytes(), 0, "no image quota");
        // A third does not fit beside them and is refused at once, with
        // nothing sent to the host; so is a 600x600 RGB one (a 1.44 MB
        // canvas the quota alone would admit); and a local file the quota
        // would refuse is refused in the file's one word.
        fx.send(&format!("a=q,i=42,f=32,s=513,v=512,o=z;{packed}"));
        fx.send(&format!("a=q,i=43,f=24,s=600,v=600,o=z;{packed}"));
        fx.host.media_bytes = Some(compressed);
        fx.send("a=q,i=44,f=32,s=513,v=512,o=z,t=f;L3RtcC9pbWcucmdi");
        assert_eq!(fx.host.decodes.len(), 2);
        assert_eq!(fx.state.query_bytes(), 2 * CANVAS);
        // A transmit meanwhile is neither refused nor made to evict: the
        // images have their own count.
        fx.send("a=T,i=45,f=32,s=1,v=1;AAAAAA==");
        assert_eq!(fx.state.image_count(), 1);
        assert_eq!(fx.state.used_bytes(), 4);
        assert_eq!(fx.host.decodes.len(), 3);
        // Landing releases a query's hold however the decode went, and the
        // next query parks again.
        fx.finish_ok();
        fx.finish(|_| Err(GraphicsError::invalid("no")));
        assert_eq!(fx.state.query_bytes(), 0);
        fx.send(&format!("a=q,i=46,f=32,s=513,v=512,o=z;{packed}"));
        assert_eq!(fx.host.decodes.len(), 2);
        assert_eq!(fx.state.query_bytes(), CANVAS);
        fx.finish_ok();
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=40;OK",
                "i=41;EINVAL:no",
                "i=42;ENOSPC:queries still decoding hold the storage quota",
                "i=43;ENOSPC:queries still decoding hold the storage quota",
                "i=44;EBADF:Failed to read image file",
                "i=45;OK",
                "i=46;OK",
            ]
        );
        assert_eq!(fx.state.query_bytes(), 0);
        assert_eq!(fx.state.used_bytes(), 4);
    }

    #[test]
    fn a_query_that_could_not_be_answered_does_no_work() {
        let big = encode_payload(&zlib(&[0; 16]));
        let mut fx = Fixture::new();
        fx.replies_enabled = false;
        fx.send(&format!("a=q,i=40,f=32,s=1024,v=1024,o=z;{big}"));
        fx.send("a=q,i=41,f=24,s=2,v=1;AAAA");
        assert!(fx.host.decodes.is_empty(), "nothing goes to the host");
        assert_eq!(fx.state.decodes_in_flight(), 0);
        assert!(fx.replies().is_empty());
        // `q=2` is the same promise of silence.
        let mut fx = Fixture::new();
        fx.send(&format!("a=q,i=40,q=2,f=32,s=1024,v=1024,o=z;{big}"));
        assert!(fx.host.decodes.is_empty());
        assert!(fx.replies().is_empty());
        // `q=1` still has an error to report.
        fx.send("a=q,i=41,q=1,f=24,s=2,v=1;AAAA");
        assert_eq!(
            fx.replies(),
            ["i=41;ENODATA:Insufficient image data: 3 < 6"]
        );
    }

    #[test]
    fn a_chunked_query_is_checked_whole_at_its_final_chunk() {
        let mut fx = Fixture::new();
        fx.send("a=q,i=50,f=24,s=2,v=1,m=1;AAAA");
        assert!(fx.state.upload_in_flight());
        assert!(fx.replies().is_empty());
        fx.send("m=0;AAAA");
        assert!(!fx.state.upload_in_flight());
        assert_eq!(fx.replies(), ["i=50;OK"]);
        fx.send("a=q,i=51,f=24,s=2,v=1,m=1;AAAA");
        fx.send("m=0;AAAAAAAA");
        assert_eq!(fx.replies(), ["i=51;EFBIG:Too much data"]);
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.image_count(), 0);
    }

    #[test]
    fn quiet_levels_gate_every_reply() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1,q=1", 1, 1));
        fx.send(&rgba("a=t,i=2,q=2", 1, 1));
        fx.send("a=t,i=3,q=1,f=24,s=1,v=1;AA");
        fx.send("a=t,i=4,q=2,f=24,s=1,v=1;AA");
        fx.send("a=q,i=5,q=1,f=24,s=1,v=1;AAAA");
        fx.send("a=p,i=1,q=1");
        fx.send("a=p,i=9,q=2");
        fx.finish_ok();
        fx.finish(|_| Err(GraphicsError::invalid("silenced")));
        assert_eq!(fx.replies(), ["i=3;ENODATA:Insufficient image data: 1 < 3"]);
    }

    #[test]
    fn a_connection_that_does_not_answer_still_stores_and_shows() {
        let mut fx = Fixture::new();
        fx.replies_enabled = false;
        let placement = fx.send(&rgba("a=T,i=1", 1, 1)).expect("shown");
        fx.send("a=p,i=99");
        fx.send("a=q,i=2,f=24,s=1,v=1;AAAA");
        fx.send("a=q,i=3,e=T");
        fx.finish_ok();
        assert!(fx.replies().is_empty());
        assert!(placement.pixels().is_some());
    }

    // --- retransmit and quota ---

    #[test]
    fn a_retransmit_under_the_same_id_replaces_the_image() {
        let mut fx = Fixture::new();
        let shown = fx.send(&rgba("a=T,i=1", 1, 1)).unwrap();
        fx.finish_ok();
        let old = shown.image.clone();
        fx.send(&rgba("a=t,i=1", 2, 1));
        let new = fx.state.image(1).unwrap();
        assert_ne!(old.uid, new.uid, "a fresh key for the UI");
        assert_eq!(new.width, 2);
        assert!(
            shown.pixels().is_some(),
            "scrollback keeps showing the old pixels"
        );
        assert_eq!(fx.state.used_bytes(), 4 + 8, "and they keep their quota");
        // Retransmitted again with nothing showing the previous one: it goes.
        fx.finish_ok();
        fx.send(&rgba("a=t,i=1", 3, 1));
        assert!(new.is_evicted());
        assert_eq!(fx.state.used_bytes(), 4 + 12);
        drop(shown);
        // An unaddressable image no placement shows is the first to go.
        let mut fx2 = Fixture::with_quota(8);
        let held = fx2.send(&rgba("a=T,i=1", 1, 1)).unwrap();
        fx2.send(&rgba("a=t,i=1", 1, 1));
        fx2.send(&rgba("a=t,i=2", 1, 1));
        assert!(
            !held.image.is_evicted() && fx2.state.image(1).is_none(),
            "the shown one stays; the unshown replacement went instead"
        );
        drop(held);
        fx2.send(&rgba("a=t,i=3", 1, 1));
        assert_eq!(fx2.state.image_count(), 2);
        assert!(fx2.state.image(2).is_some() && fx2.state.image(3).is_some());
    }

    #[test]
    fn a_retransmit_retires_the_old_image_s_virtual_placements() {
        let mut fx = Fixture::new();
        virtual_image(&mut fx, 1, (10, 20), "c=1,r=1,p=5");
        let old = fx.state.image(1).unwrap();
        assert!(fx.state.has_virtual_placements());
        fx.send(&rgba("a=t,i=1", 10, 20));
        assert!(
            !fx.state.has_virtual_placements(),
            "placeholders are disarmed: the new image has no virtual placement"
        );
        assert!(
            old.is_evicted(),
            "nothing showed the old image: it went at once"
        );
        assert_eq!(fx.state.image_count(), 1);
        assert!(fx.run((1, 5), (0, 0), 1, 0).is_none());
        fx.finish_ok();
        // A run in scrollback keeps the old image, shown there, like any
        // placement of a replaced image; a relative placement hanging from
        // the virtual placement goes with it.
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.finish_ok();
        fx.send("a=p,U=1,i=1,c=1,r=1,p=5");
        fx.send("a=p,i=2,p=9,P=1,Q=5");
        fx.replies();
        let (run, _) = fx.run((1, 5), (0, 0), 1, 0).unwrap();
        let hanging = run.child_placements().pop().unwrap();
        let shown = fx.state.image(1).unwrap();
        fx.send(&rgba("a=t,i=1", 10, 20));
        assert!(!run.is_deleted() && run.pixels().is_some());
        assert!(!shown.is_evicted());
        assert_eq!(fx.state.image_count(), 3);
        assert!(hanging.is_deleted());
        assert!(run.child_placements().is_empty());
        assert!(!fx.state.has_virtual_placements());
        assert!(fx.state.image(2).is_some(), "a retransmit frees nothing");
    }

    #[test]
    fn a_retransmit_over_a_shown_pending_image_whose_decode_then_fails() {
        let mut fx = Fixture::new();
        let shown = fx.send(&rgba("a=T,i=1", 2, 2)).unwrap();
        fx.send(&rgba("a=t,i=1", 3, 3));
        let new = fx.state.image(1).unwrap();
        assert_ne!(new.uid, shown.image.uid);
        assert_eq!(fx.state.used_bytes(), 16 + 36);
        assert_eq!(fx.state.image_count(), 2, "the shown one keeps its quota");
        // The old decode fails: its record goes, and only its bytes with it.
        fx.finish(|_| Err(GraphicsError::invalid("torn")));
        assert_eq!(
            shown.image.data(),
            ImageData::Failed(GraphicsError::invalid("torn"))
        );
        assert!(shown.pixels().is_none());
        assert_eq!(fx.state.used_bytes(), 36);
        assert_eq!(fx.state.image_count(), 1);
        assert!(
            Arc::ptr_eq(&fx.state.image(1).unwrap(), &new),
            "the id still names the new image"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;EINVAL:torn", "i=1;OK"]);
        assert!(new.pixels().is_some());
        assert_eq!(fx.state.decodes_in_flight(), 0);
    }

    #[test]
    fn eviction_prefers_unplaced_images_then_the_least_recently_used() {
        // Three 16-byte images fill the quota; a sweep frees down to a
        // sixteenth under it (45 bytes), so each sweep takes two of them.
        let mut fx = Fixture::with_quota(48);
        fx.send(&rgba("a=t,i=1", 2, 2));
        fx.send(&rgba("a=t,i=2", 2, 2));
        fx.send(&rgba("a=t,i=3", 2, 2));
        let placed = fx.send("a=p,i=2").unwrap();
        let first = fx.state.image(1).unwrap();
        fx.send(&rgba("a=t,i=4", 2, 2));
        assert!(first.is_evicted(), "the oldest unplaced image goes first");
        assert!(fx.state.image(3).is_none(), "and the next unplaced one");
        assert!(
            fx.state.image(2).is_some() && fx.state.image(4).is_some(),
            "placed images are kept while others can go"
        );
        fx.send(&rgba("a=t,i=5", 2, 2));
        assert_eq!(fx.state.used_bytes(), 48, "fits without a sweep");
        fx.send(&rgba("a=t,i=6", 2, 2));
        assert!(fx.state.image(4).is_none() && fx.state.image(5).is_none());
        assert!(fx.state.image(2).is_some());
        // Everything left is placed: the least recently used goes anyway.
        let six = fx.send("a=p,i=6").unwrap();
        fx.send(&rgba("a=t,i=7", 2, 2));
        assert!(!placed.image.is_evicted() && fx.state.used_bytes() == 48);
        fx.send(&rgba("a=t,i=8", 2, 2));
        assert!(placed.image.is_evicted(), "a tombstone in scrollback");
        assert!(placed.pixels().is_none() && !placed.is_deleted());
        assert!(!six.image.is_evicted());
        assert_eq!(fx.state.used_bytes(), 32);
        // The decode of an evicted image still answers: its data arrived.
        for _ in 0..8 {
            fx.finish_ok();
        }
        let replies = fx.replies();
        assert_eq!(replies.len(), 8 + 2);
        assert!(
            replies.iter().all(|reply| reply.ends_with(";OK")),
            "{replies:?}"
        );
        assert!(first.pixels().is_none(), "but its pixels are dropped");
    }

    #[test]
    fn a_sweep_frees_a_sixteenth_of_the_quota_so_the_next_transmits_sweep_nothing() {
        // 16-byte images in a 1024-byte quota: 64 fill it. The 65th sweeps
        // down to 960 - 16 bytes, taking five, and the next four fit with
        // no sweep at all; the 70th sweeps again.
        let mut fx = Fixture::with_quota(1024);
        for id in 1..=64 {
            fx.send(&rgba(&format!("a=t,i={id}"), 2, 2));
        }
        assert_eq!((fx.state.used_bytes(), fx.state.image_count()), (1024, 64));
        fx.send(&rgba("a=t,i=65", 2, 2));
        assert_eq!((fx.state.used_bytes(), fx.state.image_count()), (960, 60));
        assert!(fx.state.image(5).is_none() && fx.state.image(6).is_some());
        for id in 66..=69 {
            fx.send(&rgba(&format!("a=t,i={id}"), 2, 2));
            assert!(fx.state.image(6).is_some(), "no sweep for {id}");
        }
        assert_eq!((fx.state.used_bytes(), fx.state.image_count()), (1024, 64));
        fx.send(&rgba("a=t,i=70", 2, 2));
        assert_eq!((fx.state.used_bytes(), fx.state.image_count()), (960, 60));
        assert!(fx.state.image(10).is_none() && fx.state.image(11).is_some());
        // The full quota still admits an image the margin would not.
        let mut full = Fixture::with_quota(1024);
        assert!(full.send(&rgba("a=T,i=1", 16, 16)).is_some());
        assert_eq!(full.state.used_bytes(), 1024);
    }

    #[test]
    fn placements_gone_from_scrollback_are_swept_as_new_ones_arrive() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        let uid = fx.state.image(1).unwrap().uid;
        let kept = fx.send("a=p,i=1").unwrap();
        let named = fx.send("a=p,i=1,p=1").unwrap();
        for id in 2..1000 {
            drop(fx.send("a=p,i=1"));
            drop(fx.send(&format!("a=p,i=1,p={id}")));
        }
        let entry = &fx.state.entries[&uid];
        assert!(
            entry.anonymous.len() + entry.named.len() <= 2 * MIN_PRUNE_AT,
            "{} anonymous, {} named",
            entry.anonymous.len(),
            entry.named.len()
        );
        assert!(entry.live_placements().any(|p| Arc::ptr_eq(&p, &kept)));
        assert!(entry.live_placements().any(|p| Arc::ptr_eq(&p, &named)));
        assert!(!kept.is_deleted() && !named.is_deleted());
    }

    #[test]
    fn the_image_count_is_bounded_too() {
        let mut fx = Fixture::new();
        let total = u32::try_from(MAX_IMAGES).unwrap() + 1;
        for id in 1..=total {
            fx.send(&rgba(&format!("a=t,i={id}"), 1, 1));
            fx.finish_ok();
        }
        // The sweep at the cap frees a sixteenth of it, in one sort.
        let margin = MAX_IMAGES / 16;
        assert_eq!(fx.state.image_count(), MAX_IMAGES - margin);
        let survivor = u32::try_from(margin).unwrap() + 2;
        assert!(fx.state.image(survivor - 1).is_none() && fx.state.image(survivor).is_some());
        assert!(fx.state.image(total).is_some());
        // The next sixteenth's worth of transmits sweep nothing.
        for id in total + 1..=total + u32::try_from(margin).unwrap() {
            fx.send(&rgba(&format!("a=t,i={id}"), 1, 1));
            fx.finish_ok();
            assert!(fx.state.image(survivor).is_some(), "no sweep for {id}");
        }
        assert_eq!(fx.state.image_count(), MAX_IMAGES);
    }

    // --- decode completion ---

    #[test]
    fn a_failed_or_misshapen_decode_fails_the_record_and_the_reply() {
        let mut fx = Fixture::new();
        let shown = fx.send(&rgba("a=T,i=1", 2, 2)).unwrap();
        let other = fx.send(&rgba("a=T,i=2", 2, 2)).unwrap();
        fx.finish(|_| Err(GraphicsError::invalid("bad pixels")));
        fx.finish(|_| Ok(pixels_for(3, 2)));
        assert_eq!(
            fx.replies(),
            [
                "i=1;EINVAL:bad pixels",
                "i=2;EINVAL:decoded image is 3x2 in 24 bytes, expected 2x2"
            ]
        );
        assert_eq!(
            shown.image.data(),
            ImageData::Failed(GraphicsError::invalid("bad pixels"))
        );
        assert!(matches!(other.image.data(), ImageData::Failed(_)));
        assert_eq!(fx.state.image_count(), 0);
        assert_eq!(fx.state.used_bytes(), 0);
    }

    #[test]
    fn a_decode_landing_after_a_delete_drops_its_pixels_and_still_answers() {
        let mut fx = Fixture::new();
        let shown = fx.send(&rgba("a=T,i=1", 1, 1)).unwrap();
        fx.send("a=d,d=I,i=1");
        assert!(shown.is_deleted() && shown.image.is_evicted());
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        assert_eq!(shown.image.data(), ImageData::Evicted);
    }

    // --- deletes ---

    /// Images 1..=3, each Ready and placed twice (placement ids 7 and 8);
    /// image 3 also carries number 5. Returns the placements by image.
    fn three_placed_images(fx: &mut Fixture) -> Vec<[Arc<Placement>; 2]> {
        let mut out = Vec::new();
        for (id, keys) in [(1_u32, "i=1"), (2, "i=2"), (3, "I=5")] {
            fx.send(&rgba(&format!("a=t,{keys}"), 1, 1));
            fx.finish_ok();
            let image_id = if id == 3 { 2_147_483_648 } else { id };
            let seven = fx.send(&format!("a=p,i={image_id},p=7")).unwrap();
            let eight = fx.send(&format!("a=p,i={image_id},p=8")).unwrap();
            out.push([seven, eight]);
        }
        fx.replies();
        out
    }

    fn deleted(placements: &[[Arc<Placement>; 2]]) -> Vec<[bool; 2]> {
        placements
            .iter()
            .map(|[a, b]| [a.is_deleted(), b.is_deleted()])
            .collect()
    }

    fn evicted(placements: &[[Arc<Placement>; 2]]) -> Vec<bool> {
        placements
            .iter()
            .map(|[a, _]| a.image.is_evicted())
            .collect()
    }

    #[test]
    fn delete_all_removes_every_placement_and_uppercase_frees_what_it_unplaced() {
        let mut fx = Fixture::new();
        let placed = three_placed_images(&mut fx);
        fx.send(&rgba("a=t,i=9", 1, 1));
        fx.finish_ok();
        fx.send("a=d,d=A");
        assert_eq!(deleted(&placed), [[true, true]; 3]);
        assert_eq!(evicted(&placed), [true; 3]);
        assert!(
            fx.state.image(9).is_some(),
            "an image never placed is kept, as kitty keeps it"
        );
        assert_eq!(fx.state.image_count(), 1);
        assert_eq!(fx.replies(), ["i=9;OK"], "deletes never answer");
        // Lowercase keeps the data, and an uppercase delete after it
        // reaches no placement, so frees nothing either.
        let mut fx = Fixture::new();
        let placed = three_placed_images(&mut fx);
        fx.send("a=d");
        assert_eq!(deleted(&placed), [[true, true]; 3]);
        assert_eq!(evicted(&placed), [false; 3]);
        fx.send("a=d,d=A");
        assert_eq!(evicted(&placed), [false; 3]);
        assert_eq!(fx.state.image_count(), 3);
        assert!(fx.replies().is_empty());
    }

    #[test]
    fn uppercase_all_visible_keeps_an_image_that_was_never_placed() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=7", 1, 1));
        fx.finish_ok();
        fx.send("a=d,d=A");
        assert!(fx.state.image(7).is_some());
        assert_eq!(fx.state.used_bytes(), 4);
    }

    #[test]
    fn a_delete_of_the_visible_placements_spares_those_above_the_screen() {
        let mut fx = Fixture::new();
        fx.grid = (80, 10);
        // Two-row placements under a pending line 100: line 90's second
        // row is row 1 of the screen; line 89's rows are both above it.
        let placed = placed_at(
            &mut fx,
            &[
                (0, 0, "p=1"),
                (89, 0, "p=2"),
                (90, 0, "p=3"),
                (100, 0, "p=4"),
            ],
        );
        fx.line_seq = 100;
        fx.send("a=d");
        assert_eq!(deleted_flags(&placed), [false, false, true, true]);
        fx.send("a=d,d=A");
        assert_eq!(deleted_flags(&placed), [false, false, true, true]);
        assert!(
            fx.state.image(1).is_some(),
            "placements 1 and 2 still show it from scrollback"
        );
    }

    #[test]
    fn delete_by_id_narrows_to_a_placement_and_frees_only_unshown_data() {
        let mut fx = Fixture::new();
        let placed = three_placed_images(&mut fx);
        fx.send("a=d,d=i,i=1,p=7");
        assert_eq!(
            deleted(&placed),
            [[true, false], [false, false], [false, false]]
        );
        fx.send("a=d,d=I,i=1,p=7");
        assert_eq!(evicted(&placed), [false; 3], "placement 8 still shows it");
        fx.send("a=d,d=I,i=1");
        assert_eq!(deleted(&placed)[0], [true, true]);
        assert_eq!(evicted(&placed), [true, false, false]);
        fx.send("a=d,d=i,i=2");
        assert_eq!(deleted(&placed)[1], [true, true]);
        assert_eq!(
            evicted(&placed),
            [true, false, false],
            "lowercase keeps data"
        );
        assert!(fx.state.image(2).is_some());
        fx.send("a=d,d=I,i=2");
        assert!(
            fx.state.image(2).is_none(),
            "uppercase frees an unshown image"
        );
        assert!(fx.replies().is_empty());
    }

    #[test]
    fn delete_by_number_and_by_id_range() {
        let mut fx = Fixture::new();
        let placed = three_placed_images(&mut fx);
        fx.send("a=d,d=n,I=5,p=8");
        assert_eq!(deleted(&placed)[2], [false, true]);
        fx.send("a=d,d=N,I=5");
        assert_eq!(evicted(&placed), [false, false, true]);
        let mut fx = Fixture::new();
        let placed2 = three_placed_images(&mut fx);
        fx.send("a=d,d=r,x=2,y=3");
        assert_eq!(
            deleted(&placed2),
            [[false, false], [true, true], [false, false]]
        );
        fx.send("a=d,d=R,x=1,y=2");
        assert_eq!(evicted(&placed2), [true, true, false]);
        fx.send("a=d,d=R,x=5,y=1");
        assert_eq!(evicted(&placed2), [true, true, false], "an empty range");
    }

    #[test]
    fn positional_deletes_that_miss_and_unknown_targets_change_nothing() {
        let mut fx = Fixture::new();
        // Everything is placed at line 0, column 0 (2 columns, 2 rows).
        let placed = three_placed_images(&mut fx);
        fx.line_seq = 0;
        fx.column = 5;
        for command in [
            "a=d,d=c",
            "a=d,d=C",
            "a=d,d=p,x=3,y=24",
            "a=d,d=Q,x=1,y=24,z=1",
            "a=d,d=x,x=3",
            "a=d,d=Y,y=23",
            "a=d,d=Z,z=-1",
            "a=d,d=f,i=1",
            "a=d,d=F,i=9",
            "a=d,d=F",
            "a=d,d=k,i=1",
        ] {
            fx.send(command);
        }
        assert_eq!(deleted(&placed), [[false, false]; 3]);
        assert!(fx.replies().is_empty());
    }

    // --- chunked uploads ---

    #[test]
    fn a_chunked_upload_runs_at_its_final_chunk_with_its_last_quiet_level() {
        let mut fx = Fixture::new();
        assert!(fx.send("a=T,i=1,p=2,f=24,s=2,v=1,m=1;AAAA").is_none());
        assert!(fx.state.upload_in_flight());
        assert!(
            fx.state.image(1).is_none(),
            "nothing exists before the final chunk"
        );
        assert!(fx.send("m=1,q=1;AAAA").is_none());
        // kitty ignores a continuation's other keys.
        let placement = fx.send("a=t,i=9,m=0;").expect("placed at the final chunk");
        assert_eq!(placement.id, 2);
        assert_eq!(fx.host.decodes[0].1.data, [0; 6]);
        assert!(fx.state.image(9).is_none());
        fx.finish_ok();
        assert!(
            fx.replies().is_empty(),
            "q=1 from a later chunk silenced the OK"
        );
    }

    #[test]
    fn a_chunked_upload_without_dimensions_is_refused_at_its_first_chunk() {
        // kitty's timing: the first chunk carries every key, so it is
        // checked whole there, and its followers then have no upload to
        // join.
        let mut fx = Fixture::new();
        assert!(fx.send("a=T,i=1,f=24,m=1;AAAA").is_none());
        assert!(!fx.state.upload_in_flight());
        fx.send("m=1;AAAA");
        assert!(fx.send("m=0;AAAA").is_none());
        assert_eq!(
            fx.replies(),
            ["i=1;EINVAL:packed pixel data needs non-zero s and v"]
        );
        assert!(fx.host.decodes.is_empty());
        assert!(fx.state.image(1).is_none());
    }

    #[test]
    fn an_upload_out_of_space_answers_with_its_id_and_last_quiet_level() {
        // The chunk that overflows carries neither the id nor the quiet
        // level: both come from the upload it joined, whose control data
        // must still be at hand for the reply.
        let mut fx = Fixture::with_quota(16);
        fx.send("a=T,i=2,f=32,s=2,v=2,m=1;AAAA");
        fx.send("m=1,q=1;AAAA");
        fx.send("m=1;AAAAAAAAAAAAAAAAAAAA");
        assert!(!fx.state.upload_in_flight());
        assert_eq!(
            fx.replies(),
            ["i=2;ENOSPC:image data exceeds the storage quota"]
        );
        // `q=2` on any chunk silences the error too.
        fx.send("a=T,i=3,f=32,s=2,v=2,m=1;AAAA");
        fx.send("m=1,q=2;AAAA");
        fx.send("m=1;AAAAAAAAAAAAAAAAAAAA");
        assert!(!fx.state.upload_in_flight());
        assert!(fx.replies().is_empty());
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn the_e_key_is_ignored_outside_queries() {
        // A client that works in kitty sends `e` where kitty never reads
        // it; the command runs as if the key were absent.
        let mut fx = Fixture::new();
        let placement = fx
            .send("a=T,i=1,e=T,f=24,s=1,v=1;AAAA")
            .expect("a transmit-and-display carrying e= still places");
        assert_eq!(placement.image.id, 1);
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        // Nor does `e` keep a chunk from continuing an upload.
        fx.send("a=T,i=2,f=24,s=2,v=1,m=1;AAAA");
        assert!(fx.send("a=t,e=t,m=0;AAAA").is_some());
        assert!(!fx.state.upload_in_flight());
        assert_eq!(fx.host.decodes[0].1.data, [0; 6]);
    }

    #[test]
    fn a_failing_chunk_abandons_the_upload_and_answers_for_it() {
        let mut fx = Fixture::with_quota(16);
        fx.send("a=T,i=1,f=24,s=2,v=1,m=1;AAAA");
        fx.send("m=1;!!!!");
        assert!(!fx.state.upload_in_flight());
        fx.send("a=t,i=2,f=32,s=2,v=2,m=1;AAAAAAAAAAAAAAAAAAAA");
        fx.send("m=0,q=1;AAAA");
        assert_eq!(
            fx.replies(),
            [
                "i=1;EINVAL:payload is not valid base64: Invalid symbol 33, offset 0.",
                "i=2;ENOSPC:image data exceeds the storage quota",
            ]
        );
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn a_bad_first_chunk_is_refused_and_its_followers_go_nowhere() {
        let mut fx = Fixture::new();
        fx.send("a=T,i=1,f=8,m=1;AAAA");
        assert!(!fx.state.upload_in_flight());
        fx.send("m=1;AAAA");
        assert!(fx.send("m=0;AAAA").is_none());
        assert_eq!(fx.replies(), ["i=1;EINVAL:Unknown image format: 8"]);
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn what_an_upload_in_flight_lets_through() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=5", 1, 1));
        fx.finish_ok();
        fx.replies();
        fx.send("a=T,i=1,f=24,s=2,v=1,m=1;AAAA");
        // A feature query is answered without touching it.
        fx.send("a=q,i=31,e=T");
        assert!(fx.state.upload_in_flight());
        // So is a put, as in kitty.
        assert!(fx.send("a=p,i=5").is_some());
        assert!(fx.state.upload_in_flight());
        // A transmit or query continues it, its keys ignored.
        let placement = fx.send("a=q,i=6;AAAA").expect("the upload's a=T placed");
        assert!(Arc::ptr_eq(&placement.image, &fx.state.image(1).unwrap()));
        assert_eq!(fx.replies(), ["i=31,e=T;OK", "i=5;OK"]);
        // A delete abandons an upload; the next chunk then has no owner.
        fx.send("a=t,i=2,f=24,s=2,v=1,m=1;AAAA");
        fx.send("a=d,d=i,i=7");
        assert!(!fx.state.upload_in_flight());
        assert!(fx.send("m=0;AAAA").is_none());
        // Local media start over too.
        fx.send("a=t,i=3,f=24,s=2,v=1,m=1;AAAA");
        fx.send("a=t,i=4,f=100,t=f;L3RtcC94");
        assert!(!fx.state.upload_in_flight());
        assert_eq!(fx.host.decodes.len(), 1, "only image 1 was decoded");
    }

    #[test]
    fn a_delete_that_fails_validation_leaves_the_upload_in_flight() {
        let mut fx = Fixture::new();
        fx.send("a=T,i=1,f=24,s=2,v=1,m=1;AAAA");
        fx.send("a=d,d=i,i=1,I=2");
        assert!(
            fx.state.upload_in_flight(),
            "a delete refused before it runs abandons nothing"
        );
        assert_eq!(
            fx.replies(),
            ["i=1,I=2;EINVAL:image id and image number cannot both be specified"]
        );
        let placement = fx.send("m=0;AAAA").expect("the upload completes");
        assert!(Arc::ptr_eq(&placement.image, &fx.state.image(1).unwrap()));
        assert_eq!(fx.host.decodes[0].1.data, [0; 6]);
    }

    #[test]
    fn an_oversized_command_abandons_the_upload_and_is_refused() {
        let mut fx = Fixture::new();
        fx.send("a=T,i=1,f=24,s=2,v=1,m=1;AAAA");
        fx.truncated("Gm=1;AAAAAAAA");
        assert!(!fx.state.upload_in_flight());
        fx.truncated("Ga=T,i=2,q=1,f=24,s=1,v=1;AAAA");
        fx.truncated("Ga=d,d=i,i=3");
        fx.truncated("Ga=T,f=24;AAAA");
        fx.truncated("not graphics");
        fx.send("a=T,i=4,f=24,s=2,v=1,m=1,q=2;AAAA");
        fx.truncated("Gm=0");
        assert_eq!(
            fx.replies(),
            [
                "i=1;EINVAL:graphics command exceeds the size limit",
                "i=2;EINVAL:graphics command exceeds the size limit",
            ]
        );
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn an_oversized_non_pixel_command_abandons_the_upload_and_is_refused_itself() {
        // A put or a feature query continues no upload, so the refusal
        // names the oversized command, not the upload it abandoned.
        let mut fx = Fixture::new();
        fx.send("a=T,i=1,f=24,s=2,v=1,m=1;AAAA");
        fx.truncated("Ga=p,i=5,c=1");
        assert!(!fx.state.upload_in_flight());
        fx.send("a=T,i=2,f=24,s=2,v=1,m=1;AAAA");
        fx.truncated("Ga=q,i=6,e=T");
        assert!(!fx.state.upload_in_flight());
        assert_eq!(
            fx.replies(),
            [
                "i=5;EINVAL:graphics command exceeds the size limit",
                "i=6;EINVAL:graphics command exceeds the size limit",
            ]
        );
        assert!(
            fx.send("m=0;AAAA").is_none(),
            "the next chunk has no upload to join"
        );
        assert!(fx.replies().is_empty());
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.image_count(), 0);
    }

    // --- local media ---

    #[test]
    fn local_media_go_to_the_host_and_are_answered_at_once() {
        let mut fx = Fixture::new();
        fx.send("a=T,i=1,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.send("a=t,s=10,v=2,t=s,o=z;L2Zvbw==");
        fx.send("a=q,i=31,s=10,v=2,t=s,S=80,O=10;L2Zvbw==");
        fx.send("a=t,i=2,f=100,t=t;!!!!");
        assert_eq!(
            fx.host.media,
            [
                (Medium::File, b"/tmp/img.png".to_vec(), 0, 0),
                (Medium::SharedMemory, b"/foo".to_vec(), 0, 0),
                (Medium::SharedMemory, b"/foo".to_vec(), 10, 80),
            ]
        );
        assert_eq!(
            fx.replies(),
            [
                "i=1;EBADF:Failed to read image file",
                "i=31;EBADF:Failed to read image file",
                "i=2;EINVAL:payload is not valid base64: Invalid symbol 33, offset 0.",
            ]
        );
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn a_fault_in_local_media_is_always_a_bad_file() {
        // kitty answers every file-read problem with `EBADF`; `ENODATA`
        // would tell the peer the file's size, and `EFBIG` or a PNG
        // check's words what it holds. Direct data keeps kitty's verdicts.
        let mut fx = Fixture::new();
        fx.host.media_bytes = Some(vec![0; 3]);
        fx.send("a=t,i=1,f=24,s=2,v=1,t=f;L3RtcC9pbWcucmdi");
        fx.send("a=T,i=2,f=32,s=1,v=1,t=t;L3RtcC9pbWcucmdi");
        fx.send("a=q,i=3,f=100,t=s;L2Zvbw==");
        fx.send("a=t,i=4,f=24,s=2,v=1;AAAA");
        fx.send("a=t,i=5,f=24,s=1,v=1;AAAAAA");
        assert_eq!(
            fx.replies(),
            [
                "i=1;EBADF:Failed to read image file",
                "i=2;EBADF:Failed to read image file",
                "i=3;EBADF:Failed to read image file",
                "i=4;ENODATA:Insufficient image data: 3 < 6",
                "i=5;EFBIG:Too much data",
            ]
        );
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.image_count(), 0);
        // Compressed data checked here inflates to the wrong length...
        fx.host.media_bytes = Some(zlib(&[0; 3]));
        fx.send("a=q,i=6,f=24,s=2,v=1,o=z,t=f;L3RtcC9pbWcucmdi");
        assert_eq!(fx.replies(), ["i=6;EBADF:Failed to read image file"]);
        // ...and what the host finds in a file it decodes is the same
        // verdict, for a transmit and for a query.
        fx.host.media_bytes = Some(png_header(2, 2));
        fx.send("a=T,i=7,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.send("a=q,i=8,f=24,s=1000,v=1000,o=z,t=f;L3RtcC9pbWcucG5n");
        assert_eq!(fx.host.decodes.len(), 2);
        assert_eq!(fx.state.image_count(), 1);
        fx.finish(|_| Err(GraphicsError::invalid("not a PNG: bad IHDR")));
        fx.finish(|_| {
            Err(GraphicsError::new(
                ErrorCode::NoData,
                "Insufficient image data: 3 < 4000000",
            ))
        });
        assert_eq!(
            fx.replies(),
            [
                "i=7;EBADF:Failed to read image file",
                "i=8;EBADF:Failed to read image file",
            ]
        );
        assert_eq!(fx.state.image_count(), 0);
        // A placement an `a=T` could not make is refused in its own words:
        // that is a verdict on the line, not on the file.
        fx.host.media_bytes = Some(png_header(2, 2));
        fx.objects_free = 0;
        fx.send("a=T,i=9,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            ["i=9;ENOSPC:too many placements on this line"]
        );
    }

    #[test]
    fn a_fault_in_a_local_media_frame_is_always_a_bad_file() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 2, 2));
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        // A PNG whose own header is larger than the image: read from a
        // file, the rectangle check answers in the file's one word, where
        // direct data hears which rectangle.
        fx.host.media_bytes = Some(png_header(3, 3));
        fx.send("a=f,i=1,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.send(&format!(
            "a=f,i=1,f=100;{}",
            encode_payload(&png_header(3, 3))
        ));
        assert_eq!(
            fx.replies(),
            [
                "i=1;EBADF:Failed to read image file",
                "i=1;EINVAL:The frame rectangle is out of bounds",
            ]
        );
        assert!(fx.host.decodes.is_empty());
        // What the host finds decoding the file is the same verdict: a
        // zlib stream inflating to too few bytes...
        fx.host.media_bytes = Some(zlib(&[0; 3]));
        fx.send("a=f,i=1,f=24,s=2,v=2,o=z,t=f;L3RtcC9pbWcucmdi");
        assert_eq!(fx.host.decodes.len(), 1);
        fx.finish(|_| {
            Err(GraphicsError::invalid(
                "image data is 3 bytes; 2x2 needs 12",
            ))
        });
        assert_eq!(fx.replies(), ["i=1,r=2;EBADF:Failed to read image file"]);
        // ...or pixels of another size than the header announced, whether
        // the decoder says so or lands them for the canvas check to find.
        fx.host.media_bytes = Some(png_header(2, 2));
        fx.send("a=f,i=1,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.finish(|_| {
            Err(GraphicsError::invalid(
                "PNG decoded to 3x3, not the 2x2 its header announced",
            ))
        });
        fx.send("a=f,i=1,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.finish(|_| Ok(pixels_for(3, 3)));
        assert_eq!(
            fx.replies(),
            [
                "i=1,r=2;EBADF:Failed to read image file",
                "i=1,r=2;EBADF:Failed to read image file",
            ]
        );
        // Direct data keeps the decoder's words.
        fx.send(&format!(
            "a=f,i=1,f=100;{}",
            encode_payload(&png_header(2, 2))
        ));
        fx.finish(|_| {
            Err(GraphicsError::invalid(
                "PNG decoded to 3x3, not the 2x2 its header announced",
            ))
        });
        assert_eq!(
            fx.replies(),
            ["i=1,r=2;EINVAL:PNG decoded to 3x3, not the 2x2 its header announced"]
        );
        // Nothing landed, and every failed frame gave its charge back.
        assert_eq!(fx.image(1).frame_count(), 1);
        assert_eq!(fx.state.used_bytes(), 16);
    }

    #[test]
    fn local_media_the_quota_refuses_are_a_bad_file_too() {
        // A quota of 64 bytes holds a 4x4 image at most.
        let mut fx = Fixture::with_quota(64);
        fx.host.media_bytes = Some(png_header(5, 5));
        fx.send("a=t,i=1,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.send("a=q,i=2,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.send(&format!(
            "a=q,i=3,f=100;{}",
            encode_payload(&png_header(5, 5))
        ));
        assert_eq!(
            fx.replies(),
            [
                "i=1;EBADF:Failed to read image file",
                "i=2;EBADF:Failed to read image file",
                "i=3;ENOSPC:image is larger than the storage quota",
            ]
        );
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.image_count(), 0);
        // A frame waiting behind its image's decode holds its payload
        // against the quota: how long a file is, measured against what is
        // left, is not for the peer to learn either.
        fx.send(&rgba("a=t,i=4", 2, 2));
        fx.host.media_bytes = Some(png_header(2, 2));
        fx.send("a=f,i=4,f=100,t=f;L3RtcC9pbWcucG5n");
        fx.send(&format!(
            "a=f,i=4,f=100;{}",
            encode_payload(&png_header(2, 2))
        ));
        // Both refused at once; their replies wait behind the transmit's.
        assert_eq!(fx.state.used_bytes(), 16);
        assert!(fx.replies().is_empty());
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=4;OK",
                "i=4;EBADF:Failed to read image file",
                "i=4;ENOSPC:Cache size exceeded cannot add new frames",
            ]
        );
        assert_eq!(fx.image(4).frame_count(), 1);
    }

    // --- feature queries and unsupported actions ---

    #[test]
    fn feature_queries_report_the_supported_keys() {
        let mut fx = Fixture::new();
        fx.send("a=q,i=31,e=T,U=1,C=1");
        fx.send("a=q,i=31,e=f,r=1,X=1");
        fx.send("a=q,i=32,e=T,U=1");
        fx.send("a=q,i=33,e=f");
        fx.send("a=q,i=34,e=t,f=24,o=z,s=2,v=3,t=f,S=9");
        fx.send("a=q,i=35,e=p,c=3,r=2,x=1,y=0,w=5,h=6,X=2,Y=1,p=9,z=-1,P=1");
        fx.send("a=q,i=36,e=d,d=I,p=2");
        fx.send("a=q,i=37,e=d,d=R,x=1,y=9");
        fx.send("a=q,i=38,e=d,d=Z,z=1");
        fx.send("a=q,i=39,e=a");
        fx.send("a=q,e=T");
        fx.send("a=q,I=41,e=T");
        fx.send("a=q,i=40,I=1,e=T");
        fx.send("a=q,i=41,e=p,P=3,Q=4,H=-1,V=2,U=1");
        fx.send("a=q,i=42,e=d,d=q,x=3,y=4,z=-5");
        fx.send("a=q,i=43,e=d,d=c");
        fx.send("a=q,i=44,e=d,d=F,r=2");
        fx.send("a=q,i=45,e=f,f=100,o=z,s=10,v=20,x=1,y=2,c=3,r=4,z=-1,X=1,Y=255");
        fx.send("a=q,i=46,e=a,s=3,v=2,r=1,c=4,z=48");
        fx.send("a=q,i=47,e=a,s=4");
        fx.send("a=q,i=48,e=c,r=7,c=9,w=23,h=27,X=4,Y=8,x=1,y=3,C=1");
        assert_eq!(
            fx.replies(),
            [
                "i=31,e=T,C=1,U=1;OK",
                "i=31,e=f,X=1,r=1;OK",
                "i=32,e=T,U=1;OK",
                "i=33,e=f;OK",
                "i=34,e=t,f=24,o=z,s=2,v=3;OK",
                "i=35,e=p,P=1,X=2,Y=1,c=3,h=6,p=9,r=2,w=5,x=1,z=-1;OK",
                "i=36,e=d,d=I,p=2;OK",
                "i=37,e=d,d=R,x=1,y=9;OK",
                "i=38,e=d,d=Z,z=1;OK",
                "i=39,e=a;OK",
                "i=40,I=1;EINVAL:image id and image number cannot both be specified",
                "i=41,e=p,H=-1,P=3,Q=4,U=1,V=2;OK",
                "i=42,e=d,d=q,x=3,y=4,z=-5;OK",
                "i=43,e=d,d=c;OK",
                "i=44,e=d,d=F;OK",
                "i=45,e=f,X=1,Y=255,c=3,f=100,o=z,r=4,s=10,v=20,x=1,y=2,z=-1;OK",
                "i=46,e=a,c=4,r=1,s=3,v=2,z=48;OK",
                "i=47,e=a;OK",
                "i=48,e=c,C=1,X=4,Y=8,c=9,h=27,r=7,w=23,x=1,y=3;OK",
            ]
        );
    }

    // --- animation: frame loads ---

    /// Opaque `width` x `height` pixels of one grey `value`.
    fn grey(width: u32, height: u32, value: u8) -> DecodedPixels {
        let len = usize::try_from(rgba_len(width, height)).unwrap();
        let mut rgba = vec![value; len];
        for alpha in rgba.iter_mut().skip(3).step_by(4) {
            *alpha = 0xff;
        }
        DecodedPixels {
            width,
            height,
            rgba: rgba.into(),
        }
    }

    /// The red channel at (`x`, `y`) of a frame.
    fn red_at(frame: &Frame, x: u32, y: u32) -> u8 {
        let at = usize::try_from((y * frame.pixels.width + x) * 4).unwrap();
        frame.pixels.rgba[at]
    }

    #[test]
    fn a_frame_is_composed_by_the_host_and_appended_when_it_lands() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 2, 2));
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        let image = fx.image(1);
        assert_eq!(image.frame_count(), 1);
        let root = image.frame(1).unwrap();
        assert_eq!(root.gap_ms, 0, "the root frame has no gap");
        assert_eq!(Some(root.pixels.clone()), image.pixels());
        assert_eq!(image.animation(), Animation::default());

        fx.send(&rgba("a=f,i=1", 2, 2));
        assert!(fx.replies().is_empty(), "the reply waits for the decode");
        let (_, request) = &fx.host.decodes[0];
        assert_eq!((request.width, request.height), (2, 2));
        assert_eq!(
            request.compose,
            Some(Compose {
                base: None,
                source: None,
                background: 0,
                x: 0,
                y: 0,
                mode: ComposeMode::AlphaBlend,
                canvas: (2, 2),
            })
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=2;OK"], "the frame's number is echoed");
        assert_eq!(image.frame_count(), 2);
        let second = image.frame(2).unwrap();
        assert_eq!(second.gap_ms, DEFAULT_FRAME_GAP_MS);
        assert_ne!(second.id, root.id);
        assert_eq!(image.frames().len(), 2);
        assert_eq!(fx.state.used_bytes(), 32, "every frame counts");
        assert_eq!(
            image.animation().generation,
            0,
            "a frame arriving restarts nothing"
        );
        assert!(!fx.state.take_dirty(), "the decode's landing announces it");
    }

    #[test]
    fn a_partial_frame_lands_on_its_base_at_its_rectangle() {
        // The specification's example rectangle, overwriting the root with
        // opaque red behind it.
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=3", 120, 210));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=3,x=10,y=5,c=1,X=1,Y=4278190335", 100, 200));
        let (_, request) = &fx.host.decodes[0];
        let compose = request.compose.clone().unwrap();
        assert_eq!(compose.base, fx.image(3).pixels());
        assert_eq!((compose.x, compose.y), (10, 5));
        assert_eq!(compose.mode, ComposeMode::Overwrite);
        assert_eq!(compose.background_rgba(), [0xff, 0, 0, 0xff]);
        assert_eq!(compose.canvas, (120, 210));
        assert_eq!((request.width, request.height), (100, 200));
        fx.finish_composed(Some(0x80));
        assert_eq!(fx.replies(), ["i=3;OK", "i=3,r=2;OK"]);
        let frame = fx.image(3).frame(2).unwrap();
        assert_eq!((frame.pixels.width, frame.pixels.height), (120, 210));
        assert_eq!(
            [
                red_at(&frame, 0, 0),
                red_at(&frame, 10, 5),
                red_at(&frame, 109, 204),
                red_at(&frame, 110, 5),
                red_at(&frame, 10, 205),
            ],
            [0xff, 0x80, 0x80, 0xff, 0xff]
        );
        // Without a base, the background shows around the rectangle.
        fx.send(&rgba("a=f,i=3,x=10,y=5,Y=4278190335", 100, 200));
        let compose = fx.host.decodes[0].1.compose.clone().unwrap();
        assert_eq!(
            (compose.base, compose.mode),
            (None, ComposeMode::AlphaBlend)
        );
        fx.finish_composed(Some(0x80));
        let frame = fx.image(3).frame(3).unwrap();
        assert_eq!(&frame.pixels.rgba[..4], [0xff, 0, 0, 0xff]);
        assert_eq!(fx.replies(), ["i=3,r=3;OK"]);
    }

    #[test]
    fn a_frame_rectangle_outside_the_image_is_refused_at_once() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 4, 4));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1,x=1", 4, 4));
        fx.send(&rgba("a=f,i=1,y=3,r=1", 2, 2));
        fx.send(&rgba("a=f,i=1", 5, 1));
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1;EINVAL:The frame rectangle is out of bounds",
                "i=1,r=1;EINVAL:The frame rectangle is out of bounds",
                "i=1;EINVAL:The frame rectangle is out of bounds",
            ]
        );
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.used_bytes(), 64, "nothing was charged");
        assert_eq!(fx.image(1).frame_count(), 1);
    }

    #[test]
    fn a_frame_of_a_missing_image_is_enoent_and_one_without_an_id_is_dropped() {
        const MISSING: &str =
            "ENOENT:Animation command refers to non-existent image with id: 9 and number: 0";
        let mut fx = Fixture::new();
        fx.send(&rgba("a=f,i=9,r=2", 1, 1));
        fx.send(&rgba("a=f,I=9", 1, 1));
        fx.send(&rgba("a=f", 1, 1));
        fx.send("a=a,i=9,c=1,r=2");
        fx.send("a=a,c=1");
        fx.send("a=c,i=9,r=1,c=1");
        fx.send("a=c,r=1,c=1");
        fx.send("a=d,d=f,i=9");
        fx.send("a=d,d=F");
        assert_eq!(
            fx.replies(),
            [
                format!("i=9,r=2;{MISSING}"),
                "I=9;ENOENT:Animation command refers to non-existent image with id: 0 and number: 9"
                    .to_string(),
                format!("i=9,r=2;{MISSING}"),
                format!("i=9;{MISSING}"),
            ]
        );
        assert!(fx.host.decodes.is_empty());
        assert!(!fx.state.take_dirty());
    }

    #[test]
    fn frame_commands_wait_for_the_image_s_root_and_run_in_order() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1,z=100", 1, 1));
        fx.send("a=a,i=1,s=3,c=2");
        fx.send(&rgba("a=f,i=1,c=2", 1, 1));
        assert_eq!(fx.host.decodes.len(), 1, "frames wait for the root");
        let image = fx.image(1);
        assert_eq!(
            image.animation().state,
            AnimationState::Stopped,
            "the control waits too"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        assert_eq!(fx.host.decodes.len(), 1, "one frame decode at a time");
        assert_eq!(
            image.animation().state,
            AnimationState::Stopped,
            "the control is behind the first frame"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=2;OK"]);
        let animation = image.animation();
        assert_eq!(
            (
                animation.state,
                animation.current,
                animation.generation,
                animation.control_serial
            ),
            (AnimationState::Running, 2, 1, 1),
            "the control ran once frame 2 existed: c=2 restarts the position, s=3 the clock"
        );
        assert!(fx.state.take_dirty(), "a control run later still repaints");
        let (_, request) = &fx.host.decodes[0];
        assert_eq!(
            request.compose.as_ref().unwrap().base,
            Some(image.frame(2).unwrap().pixels),
            "the third frame's base is the second, which had landed"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=3;OK"]);
        assert_eq!(image.frame(2).unwrap().gap_ms, 100);
        assert_eq!(image.frame_count(), 3);
        assert_eq!(fx.state.decodes_in_flight(), 0);
    }

    #[test]
    fn a_failed_root_answers_the_frames_queued_behind_it_with_enoent() {
        const MISSING: &str =
            "ENOENT:Animation command refers to non-existent image with id: 1 and number: 0";
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1,r=1", 1, 1));
        fx.send("a=c,i=1,r=1,c=2");
        fx.send("a=a,i=1,s=3");
        fx.send("a=d,d=f,i=1");
        assert_eq!(
            fx.state.used_bytes(),
            8,
            "an edit of the root charges nothing beyond its payload while it waits"
        );
        fx.finish(|_| Err(GraphicsError::invalid("bad")));
        assert_eq!(
            fx.replies(),
            [
                "i=1;EINVAL:bad".to_string(),
                format!("i=1,r=1;{MISSING}"),
                format!("i=1;{MISSING}"),
            ]
        );
        assert!(fx.state.image(1).is_none());
        assert_eq!(fx.state.used_bytes(), 0);
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn decodes_of_other_images_run_alongside_a_frame_in_flight() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send(&rgba("a=t,i=2", 1, 1));
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send(&rgba("a=f,i=2", 1, 1));
        assert_eq!(
            fx.host.decodes.len(),
            2,
            "image 1's frame and image 2's root; the other frames wait"
        );
        // Image 2's root lands first: its reply waits behind the frame's.
        let (ticket, request) = fx.host.decodes.remove(1);
        let pixels = pixels_for(request.width, request.height);
        fx.state
            .decode_complete(&mut fx.host, &mut fx.replies, ticket, Ok(pixels));
        assert!(fx.replies().is_empty());
        assert_eq!(fx.host.decodes.len(), 2, "image 2's frame was dispatched");
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=2;OK", "i=2;OK"]);
        assert_eq!(fx.host.decodes.len(), 2, "image 1's second frame followed");
        fx.finish_ok();
        assert!(
            fx.replies().is_empty(),
            "image 2's frame waits behind image 1's"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=3;OK", "i=2,r=2;OK"]);
        assert_eq!(
            (fx.image(1).frame_count(), fx.image(2).frame_count()),
            (3, 2)
        );
    }

    #[test]
    fn editing_a_frame_composes_onto_it_and_keeps_its_number() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 2, 1));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1", 2, 1));
        fx.finish_ok();
        let image = fx.image(1);
        let before = image.frame(2).unwrap();
        fx.send(&rgba("a=f,i=1,r=2,x=1,z=-1,c=1,Y=7,X=1", 1, 1));
        let (_, request) = &fx.host.decodes[0];
        let compose = request.compose.clone().unwrap();
        assert_eq!(
            compose.base,
            Some(before.pixels.clone()),
            "the canvas is the frame itself, whatever c says"
        );
        assert_eq!((compose.x, compose.y), (1, 0));
        fx.finish_composed(Some(0x11));
        assert_eq!(fx.replies(), ["i=1;OK", "i=1,r=2;OK", "i=1,r=2;OK"]);
        let after = image.frame(2).unwrap();
        assert_ne!(
            after.id, before.id,
            "a fresh id: a handle keyed on it is never stale"
        );
        assert_eq!(
            &after.pixels.rgba[..],
            [0xff, 0xff, 0xff, 0xff, 0x11, 0x11, 0x11, 0x11]
        );
        assert_eq!(after.gap_ms, 0, "z=-1 made it gapless");
        assert_eq!(image.frame_count(), 2);
        assert_eq!(fx.state.used_bytes(), 16, "an edit adds no bytes");
        // `r` past the end makes a new frame, as in kitty.
        fx.send(&rgba("a=f,i=1,r=9", 2, 1));
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=3;OK"]);
        assert_eq!(image.frame_count(), 3);
        assert_eq!(fx.state.used_bytes(), 24);
    }

    #[test]
    fn editing_the_root_replaces_the_image_s_pixels_and_drops_the_ui_slot() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        let image = fx.image(1);
        let (slot, dropped) = token();
        assert!(image.attach_ui_slot(slot).is_ok());
        let root = image.frame(1).unwrap();
        fx.send(&rgba("a=f,i=1,r=1,X=1", 1, 1));
        assert!(!dropped.load(Ordering::SeqCst));
        fx.finish_composed(Some(0x22));
        assert_eq!(fx.replies(), ["i=1;OK", "i=1,r=1;OK"]);
        assert!(
            dropped.load(Ordering::SeqCst),
            "what the renderer minted from the old pixels goes"
        );
        let new_root = image.frame(1).unwrap();
        assert_ne!(new_root.id, root.id);
        assert_eq!(image.pixels(), Some(new_root.pixels.clone()));
        assert_eq!(&new_root.pixels.rgba[..], [0x22; 4]);
        assert_eq!(image.frame_count(), 1);
        assert_eq!(fx.state.used_bytes(), 4);
    }

    #[test]
    fn a_frame_with_a_base_the_image_does_not_have_is_enoent() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1,c=5", 1, 1));
        assert_eq!(
            fx.replies(),
            ["i=1;OK", "i=1,r=2;ENOENT:No frame with number: 5 found"]
        );
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.used_bytes(), 4, "the charge is released");
        assert_eq!(fx.image(1).frame_count(), 1);
        // The next frame is numbered as if the refused one never was.
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=2;OK"]);
    }

    #[test]
    fn a_failed_or_misshapen_frame_releases_its_charge_and_keeps_the_image() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.finish(|_| Err(GraphicsError::invalid("bad")));
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.finish(|_| Ok(pixels_for(2, 1)));
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1,r=2;EINVAL:bad",
                "i=1,r=2;EINVAL:decoded image is 2x1 in 8 bytes, expected 1x1",
            ]
        );
        let image = fx.image(1);
        assert_eq!((image.frame_count(), fx.state.used_bytes()), (1, 4));
        assert_eq!(image.data(), ImageData::Ready(pixels_for(1, 1)));
    }

    #[test]
    fn a_frame_landing_on_an_evicted_image_still_answers() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send("a=c,i=1,r=1,c=1,w=1,h=1,X=0,x=0");
        let image = fx.image(1);
        fx.send("a=d,d=I,i=1");
        assert!(image.is_evicted() && fx.state.image(1).is_none());
        assert_eq!(fx.state.used_bytes(), 0);
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1,r=2;OK",
                "i=1;EINVAL:The source and destination rectangles overlap and the src and destination frames are the same"
            ]
        );
        assert_eq!(image.frame_count(), 0, "an evicted image keeps no frames");
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn deleting_an_image_answers_the_frames_queued_for_it() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send("a=c,i=1,r=1,c=2");
        fx.send("a=d,d=I,i=1");
        assert!(fx.replies().is_empty(), "everything waits for the root");
        assert!(fx.state.image(1).is_none());
        assert_eq!(fx.state.used_bytes(), 0);
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            ["i=1;OK", "i=1;OK", "i=1;OK"],
            "the data arrived, as for a decode in flight"
        );
        assert!(
            fx.host.decodes.is_empty(),
            "nothing decodes for an image that is gone"
        );
    }

    #[test]
    fn frame_replies_echo_the_number_and_honour_quiet_levels() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,I=5", 1, 1));
        fx.finish_ok();
        let id = fx.state.newest_by_number(5).unwrap().id;
        fx.send(&rgba("a=f,I=5,q=1", 1, 1));
        fx.finish_ok();
        fx.send(&rgba("a=f,I=5,q=1,c=9", 1, 1));
        fx.send(&rgba("a=f,I=5,q=2,c=9", 1, 1));
        fx.send(&format!("a=f,I=5,s=1,v=1,m=1;{}", encode_payload(&[0; 2])));
        assert!(fx.state.upload_in_flight());
        fx.send(&format!("a=f,m=0,q=1;{}", encode_payload(&[0; 2])));
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                format!("i={id},I=5;OK"),
                format!("i={id},I=5,r=3;ENOENT:No frame with number: 9 found"),
            ]
        );
        assert_eq!(fx.image(id).frame_count(), 3);
        fx.replies_enabled = false;
        fx.send(&rgba("a=f,I=5", 1, 1));
        fx.finish_ok();
        assert!(fx.replies().is_empty());
        assert_eq!(fx.image(id).frame_count(), 4);
    }

    #[test]
    fn an_image_holds_at_most_max_frames() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        let image = fx.image(1);
        while image.frame_count() < MAX_FRAMES - 1 {
            image.push_frame(pixels_for(1, 1), 1).unwrap();
        }
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1,r=3", 1, 1));
        fx.finish_ok();
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1,r=256;OK",
                "i=1;ENOSPC:an image holds at most 256 frames",
                "i=1,r=3;OK",
            ],
            "the 256th frame lands; a 257th is refused at once, counting the one in flight; an edit is fine"
        );
        assert_eq!(image.frame_count(), MAX_FRAMES);
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn frames_count_against_the_quota_and_eviction_drops_the_whole_record() {
        // 48 bytes hold three 2x2 canvases.
        let mut fx = Fixture::with_quota(48);
        fx.send(&rgba("a=t,i=1", 2, 2));
        fx.send(&rgba("a=t,i=2", 2, 2));
        fx.finish_ok();
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1", 2, 2));
        fx.finish_ok();
        assert_eq!(fx.state.used_bytes(), 48);
        // One more frame: image 2, unplaced and idle, goes for it — never
        // image 1 itself.
        fx.send(&rgba("a=f,i=1", 2, 2));
        assert!(fx.state.image(2).is_none() && fx.state.image(1).is_some());
        fx.finish_ok();
        assert_eq!(fx.state.used_bytes(), 48);
        assert_eq!(fx.image(1).frame_count(), 3);
        // Image 1 alone fills the quota now: a fourth frame is ENOSPC.
        fx.send(&rgba("a=f,i=1", 2, 2));
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=2;OK",
                "i=1,r=2;OK",
                "i=1,r=3;OK",
                "i=1;ENOSPC:Cache size exceeded cannot add new frames",
            ]
        );
        assert!(fx.host.decodes.is_empty());
        // A deleted frame gives its bytes back; a failed one never keeps them.
        fx.send("a=d,d=f,i=1,r=3");
        assert_eq!(fx.state.used_bytes(), 32);
        fx.send(&rgba("a=f,i=1", 2, 2));
        fx.finish(|_| Err(GraphicsError::invalid("bad")));
        assert_eq!(fx.state.used_bytes(), 32);
        assert_eq!(fx.replies(), ["i=1,r=3;EINVAL:bad"]);
        // A new image evicts the animated one whole: frames and all, in the
        // usual order.
        let image = fx.image(1);
        fx.send(&rgba("a=t,i=3", 2, 2));
        assert_eq!(fx.state.used_bytes(), 48);
        fx.send(&rgba("a=t,i=4", 2, 2));
        assert!(image.is_evicted() && fx.state.image(1).is_none());
        assert_eq!((image.frame_count(), image.frames().len()), (0, 0));
        assert_eq!(fx.state.used_bytes(), 32);
    }

    // --- animation: control, compose, frame deletes ---

    #[test]
    fn gap_defaults_and_signs_follow_the_spec() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        for keys in ["a=f,i=1", "a=f,i=1,z=0", "a=f,i=1,z=48", "a=f,i=1,z=-1"] {
            fx.send(&rgba(keys, 1, 1));
            fx.finish_ok();
        }
        let image = fx.image(1);
        let gaps = |image: &ImageRecord| -> Vec<u32> {
            image.frames().iter().map(|frame| frame.gap_ms).collect()
        };
        assert_eq!(
            gaps(&image),
            [0, 40, 40, 48, 0],
            "root: none; unspecified and zero: 40 ms; negative: gapless"
        );
        // The control command sets a gap, the root's included; zero is
        // ignored, negative is gapless, a frame the image lacks too.
        fx.send("a=a,i=1,r=1,z=100");
        fx.send("a=a,i=1,r=2,z=0");
        fx.send("a=a,i=1,r=4,z=-3");
        fx.send("a=a,i=1,r=9,z=5");
        assert_eq!(gaps(&image), [100, 40, 40, 0, 0]);
        assert_eq!(
            fx.replies().len(),
            5,
            "the frames' OKs; controls say nothing"
        );
        assert_eq!(
            image.animation().generation,
            0,
            "gap changes do not restart the clock"
        );
        assert!(fx.state.take_dirty());
    }

    #[test]
    fn animation_control_examples_from_the_spec() {
        use AnimationState::{Loading, Running, Stopped};
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=3", 1, 1));
        fx.finish_ok();
        let image = fx.image(3);
        for _ in 0..6 {
            image.push_frame(pixels_for(1, 1), 40).unwrap();
        }
        assert_eq!(image.frame_count(), 7);
        assert_eq!(image.animation(), Animation::default());
        // (state, loops, current, generation, control serial).
        let snapshot = |image: &ImageRecord| {
            let a = image.animation();
            (a.state, a.loops, a.current, a.generation, a.control_serial)
        };
        fx.send("a=a,i=3,c=7");
        assert_eq!(
            snapshot(&image),
            (Stopped, 0, 7, 1, 0),
            "a new current frame restarts the position"
        );
        fx.send("a=a,i=3,c=7");
        assert_eq!(
            snapshot(&image),
            (Stopped, 0, 7, 1, 0),
            "the current frame again: nothing restarts"
        );
        fx.send("a=a,i=3,c=8");
        assert_eq!(snapshot(&image).2, 7, "a frame the image lacks is ignored");
        fx.send("a=a,i=3,s=2");
        assert_eq!(
            snapshot(&image),
            (Loading, 0, 7, 1, 1),
            "s= resets the loop count and, leaving Stopped, the clock; never the position"
        );
        fx.send("a=a,i=3,s=3,v=1");
        assert_eq!(snapshot(&image), (Running, 0, 7, 1, 2), "v=1 loops forever");
        fx.send("a=a,i=3,v=3");
        assert_eq!(
            snapshot(&image),
            (Running, 2, 7, 1, 2),
            "v=3 plays twice; v alone restarts nothing"
        );
        fx.send("a=a,i=3,v=0,s=3");
        assert_eq!(
            snapshot(&image),
            (Running, 2, 7, 1, 3),
            "v=0 is ignored; s=3 re-sent resets the loop count, and the position stays"
        );
        fx.send("a=a,i=3,s=1");
        assert_eq!(
            snapshot(&image),
            (Stopped, 2, 7, 1, 4),
            "stopping freezes on the frame being shown"
        );
        fx.send("a=a,i=3,s=7");
        assert_eq!(
            snapshot(&image),
            (Stopped, 2, 7, 1, 5),
            "an unknown state leaves the state and still resets the loop count, as in kitty"
        );
        assert_eq!(
            fx.replies(),
            ["i=3;OK"],
            "controls are never answered with OK"
        );
        assert!(fx.state.take_dirty(), "but they ask for a repaint");
        // Everything at once, the specification's loading recipe.
        fx.send("a=a,i=3,s=2,v=2,c=1,r=1,z=30");
        assert_eq!(snapshot(&image), (Loading, 1, 1, 2, 6));
        assert_eq!(image.frame(1).unwrap().gap_ms, 30);
    }

    #[test]
    fn a_compose_example_from_the_spec() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 30, 40));
        fx.finish_ok();
        let image = fx.image(1);
        for n in 2..=9u8 {
            image.push_frame(grey(30, 40, n), 40).unwrap();
        }
        let nine = image.frame(9).unwrap();
        fx.send("a=c,i=1,r=7,c=9,w=23,h=27,X=4,Y=8,x=1,y=3");
        assert_eq!(fx.replies(), ["i=1;OK"], "the compose waits for the host");
        let (_, request) = &fx.host.decodes[0];
        assert_eq!(
            (request.format, request.compression),
            (Format::Rgba, Compression::None)
        );
        assert_eq!((request.width, request.height), (23, 27));
        assert!(
            request.data.is_empty(),
            "a compose carries no data: the host reads the rectangle from frame 7"
        );
        let seven = image.frame(7).unwrap();
        assert_eq!(
            request.compose,
            Some(Compose {
                base: Some(nine.pixels.clone()),
                source: Some(ComposeSource {
                    pixels: seven.pixels.clone(),
                    x: 4,
                    y: 8,
                    width: 23,
                    height: 27,
                }),
                background: 0,
                x: 1,
                y: 3,
                mode: ComposeMode::AlphaBlend,
                canvas: (30, 40),
            })
        );
        let source = request.compose.as_ref().unwrap().source.as_ref().unwrap();
        assert_eq!(
            source.pixels.rgba.as_ptr(),
            seven.pixels.rgba.as_ptr(),
            "the source frame's buffer is shared, not copied"
        );
        fx.finish_composed(None);
        assert_eq!(fx.replies(), ["i=1;OK"]);
        let after = image.frame(9).unwrap();
        assert_ne!(after.id, nine.id);
        assert_eq!(
            [
                red_at(&after, 0, 0),
                red_at(&after, 1, 3),
                red_at(&after, 23, 29),
                red_at(&after, 24, 3),
                red_at(&after, 1, 30),
            ],
            [9, 7, 7, 9, 9]
        );
        assert_eq!(
            red_at(&image.frame(7).unwrap(), 0, 0),
            7,
            "the source is untouched"
        );
        assert_eq!(
            fx.state.used_bytes(),
            30 * 40 * 4,
            "a compose adds no bytes (frames pushed by hand hold no quota)"
        );
        assert_eq!(image.animation().generation, 0);
        // A compose onto the root replaces the image's pixels.
        fx.send("a=c,i=1,r=9,c=1,C=1");
        let compose = fx.host.decodes[0].1.compose.clone().unwrap();
        assert_eq!(
            (compose.mode, compose.base),
            (ComposeMode::Overwrite, image.pixels())
        );
        fx.finish_composed(None);
        assert_eq!(fx.replies(), ["i=1;OK"]);
        assert_eq!(red_at(&image.frame(1).unwrap(), 0, 0), 9);
        assert_eq!(image.pixels(), Some(image.frame(1).unwrap().pixels));
    }

    #[test]
    fn a_compose_is_refused_for_missing_frames_and_bad_rectangles() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 4, 4));
        fx.finish_ok();
        let image = fx.image(1);
        image.push_frame(pixels_for(4, 4), 40).unwrap();
        fx.send("a=c,i=1,r=1,c=3");
        fx.send("a=c,i=1,r=3,c=1");
        fx.send("a=c,i=1,c=1");
        fx.send("a=c,i=1,r=1,c=2,w=3,x=2");
        fx.send("a=c,i=1,r=1,c=2,h=3,Y=2");
        fx.send("a=c,i=1,r=2,c=2,w=2,h=2,X=1,Y=1,x=2,y=2");
        fx.send("a=c,i=1,r=2,c=2,w=2,h=2,x=2,y=2,C=1");
        fx.send("a=c,i=7,r=1,c=1");
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1;ENOENT:No destination frame number 3 exists in image id: 1",
                "i=1;ENOENT:No source frame number 3 exists in image id: 1",
                "i=1;ENOENT:No source frame number 0 exists in image id: 1",
                "i=1;EINVAL:The destination rectangle is out of bounds",
                "i=1;EINVAL:The source rectangle is out of bounds",
                "i=1;EINVAL:The source and destination rectangles overlap and the src and destination frames are the same",
            ]
        );
        assert_eq!(
            fx.host.decodes.len(),
            1,
            "within one frame, rectangles apart are fine"
        );
        let compose = fx.host.decodes[0].1.compose.clone().unwrap();
        assert_eq!(
            (compose.mode, compose.x, compose.y),
            (ComposeMode::Overwrite, 2, 2)
        );
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=7;ENOENT:Animation command refers to non-existent image with id: 7 and number: 0",
            ]
        );
    }

    #[test]
    fn deleting_frames_renumbers_them_and_follows_the_current_frame() {
        let mut fx = Fixture::new();
        let shown = fx.send(&rgba("a=T,i=1", 1, 1)).unwrap();
        fx.finish_ok();
        let image = fx.image(1);
        for n in 2..=4u8 {
            fx.send(&rgba("a=f,i=1,z=10", 1, 1));
            fx.finish_composed(Some(n));
        }
        let ids: Vec<FrameId> = image.frames().iter().map(|frame| frame.id).collect();
        assert_eq!((ids.len(), fx.state.used_bytes()), (4, 16));
        fx.send("a=a,i=1,c=3");
        assert!(fx.state.take_dirty());
        fx.send("a=d,d=f,i=1,r=2");
        let animation = image.animation();
        assert_eq!(
            (image.frame_count(), animation.current, animation.generation),
            (3, 2, 2),
            "the current frame keeps its pixels under its new number"
        );
        assert_eq!(image.frame(2).unwrap().id, ids[2]);
        assert_eq!(red_at(&image.frame(2).unwrap(), 0, 0), 3);
        assert_eq!(fx.state.used_bytes(), 12);
        assert!(fx.state.take_dirty());
        // Deleting the current frame: its number now shows the next one.
        fx.send("a=d,d=F,i=1,r=2");
        assert_eq!((image.frame_count(), image.animation().current), (2, 2));
        assert_eq!(image.frame(2).unwrap().id, ids[3]);
        assert!(
            !shown.is_deleted(),
            "d=F with frames left removes a frame only"
        );
        // `r` past the end names the last; a current frame past the end
        // moves to the new last.
        fx.send("a=d,d=f,i=1,r=99");
        assert_eq!((image.frame_count(), image.animation().current), (1, 1));
        fx.send("a=d,d=f,i=1");
        assert_eq!(image.frame_count(), 1, "the only frame stays");
        assert!(!image.is_evicted() && !shown.is_deleted());
        assert_eq!(fx.state.used_bytes(), 4);
        // The root goes: the next frame is the root now, and the UI slot
        // minted from the old one drops.
        let (slot, dropped) = token();
        assert!(image.attach_ui_slot(slot).is_ok());
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.finish_composed(Some(9));
        fx.send("a=d,d=f,i=1");
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(&image.pixels().unwrap().rgba[..], [9; 4]);
        assert_eq!((image.frame_count(), image.animation().current), (1, 1));
        assert_eq!(image.animation().generation, 5, "every removal restarts");
        assert_eq!(fx.state.used_bytes(), 4);
        // `d=F` on a still image deletes the image outright.
        fx.send("a=d,d=F,i=1");
        assert!(shown.is_deleted() && image.is_evicted() && fx.state.image(1).is_none());
        assert_eq!(fx.state.used_bytes(), 0);
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1,r=2;OK",
                "i=1,r=3;OK",
                "i=1,r=4;OK",
                "i=1,r=2;OK"
            ],
            "deletes are never answered"
        );
        assert!(fx.state.take_dirty());
    }

    #[test]
    fn a_frame_delete_waits_its_turn_behind_the_image_s_decodes() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send("a=d,d=f,i=1,r=1");
        fx.send(&rgba("a=f,i=1", 1, 1));
        let image = fx.image(1);
        fx.finish_ok();
        fx.finish_ok();
        assert_eq!(
            image.frame_count(),
            1,
            "the root went once frame 2 had landed, leaving it as the root"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK", "i=1,r=2;OK", "i=1,r=2;OK"]);
        assert_eq!(image.frame_count(), 2);
        assert_eq!(fx.state.used_bytes(), 8);
    }

    // --- animation: the queue's bounds ---

    #[test]
    fn the_frame_queue_is_bounded_and_its_payloads_are_charged() {
        const REFUSED: &str = "i=1,r=1;ENOSPC:image id: 1 has 64 frame commands waiting already";
        // With the root still decoding, a client stacks edits and controls
        // behind it without end: a bounded number wait, each holding its
        // payload against the quota; the loads past the bound are refused
        // at once and the controls past it dropped.
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        for _ in 0..500 {
            fx.send(&rgba("a=f,i=1,r=1", 1, 1));
            fx.send("a=a,i=1,s=3");
        }
        let waiting = MAX_QUEUED_FRAME_OPS / 2;
        assert!(
            fx.replies().is_empty(),
            "every reply, the refusals included, waits behind the parked root"
        );
        assert_eq!(
            fx.state.used_bytes(),
            4 + u64::try_from(waiting * 4).unwrap(),
            "the root, and every waiting load's four-byte payload"
        );
        assert_eq!(fx.host.decodes.len(), 1);
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        // The loads run one at a time, each giving its payload back as it
        // goes to the host; the controls between them run as they come.
        let mut landed = Vec::new();
        for n in 0..waiting {
            assert_eq!(
                fx.state.used_bytes(),
                4 + u64::try_from((waiting - 1 - n) * 4).unwrap(),
                "load {n}"
            );
            fx.finish_ok();
            landed.extend(fx.replies());
        }
        assert_eq!(landed.len(), 500, "one reply per load sent");
        assert!(landed[..waiting].iter().all(|reply| reply == "i=1,r=1;OK"));
        assert!(
            landed[waiting..].iter().all(|reply| reply == REFUSED),
            "{:?}",
            &landed[waiting..waiting + 2]
        );
        assert_eq!(
            fx.state.used_bytes(),
            4,
            "only the root is charged once everything ran"
        );
        assert!(fx.host.decodes.is_empty());
        let image = fx.image(1);
        assert_eq!(image.frame_count(), 1);
        assert_eq!(image.animation().state, AnimationState::Running);
        assert_eq!(
            image.animation().control_serial,
            u64::try_from(waiting).unwrap(),
            "the controls within the bound ran; the rest were dropped"
        );
    }

    #[test]
    fn composes_and_deletes_past_the_bound_are_refused_or_dropped() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        let image = fx.image(1);
        image.push_frame(pixels_for(1, 1), 40).unwrap();
        fx.send(&rgba("a=f,i=1", 1, 1));
        assert_eq!(fx.host.decodes.len(), 1, "in flight; the rest wait");
        for _ in 0..MAX_QUEUED_FRAME_OPS {
            fx.send("a=c,i=1,r=1,c=2");
        }
        fx.send("a=c,i=1,r=1,c=2");
        fx.send("a=d,d=f,i=1,r=2");
        fx.send("a=a,i=1,s=3");
        assert_eq!(
            fx.replies(),
            ["i=1;OK"],
            "the composes within the bound wait, parked, and the refusal behind them"
        );
        assert_eq!(fx.state.used_bytes(), 8, "a compose holds no payload");
        // Everything within the bound runs; the delete and the control
        // past it never happened.
        for _ in 0..=MAX_QUEUED_FRAME_OPS {
            fx.finish_composed(None);
        }
        assert!(fx.host.decodes.is_empty());
        let replies = fx.replies();
        assert_eq!(replies.len(), 2 + MAX_QUEUED_FRAME_OPS);
        assert_eq!(replies[0], "i=1,r=3;OK");
        assert!(
            replies[1..=MAX_QUEUED_FRAME_OPS]
                .iter()
                .all(|reply| reply == "i=1;OK")
        );
        assert_eq!(
            replies[1 + MAX_QUEUED_FRAME_OPS],
            "i=1;ENOSPC:image id: 1 has 64 frame commands waiting already"
        );
        assert_eq!(image.frame_count(), 3);
        assert_eq!(image.animation().state, AnimationState::Stopped);
    }

    #[test]
    fn queued_loads_hold_their_payloads_until_they_run_or_the_image_goes() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 2, 1));
        fx.send(&rgba("a=f,i=1", 2, 1));
        fx.send(&rgba("a=f,i=1,r=1", 2, 1));
        assert_eq!(
            fx.state.used_bytes(),
            8 + (8 + 8) + 8,
            "the root; a new frame and its payload; an edit's payload"
        );
        fx.finish_ok();
        assert_eq!(
            fx.state.used_bytes(),
            24,
            "the first load ran: its payload went to the host"
        );
        fx.finish_ok();
        assert_eq!(fx.state.used_bytes(), 16, "the edit ran");
        fx.finish_ok();
        assert_eq!(fx.state.used_bytes(), 16);
        assert_eq!(fx.replies(), ["i=1;OK", "i=1,r=2;OK", "i=1,r=1;OK"]);
        // A load that runs at once holds nothing extra.
        fx.send(&rgba("a=f,i=1", 2, 1));
        assert_eq!(fx.state.used_bytes(), 24);
        fx.finish_ok();
        assert_eq!(fx.state.used_bytes(), 24);
        // Deleting the image gives its queued payloads back with the rest;
        // the load in flight lands nowhere and still answers.
        fx.send(&rgba("a=f,i=1", 2, 1));
        fx.send(&rgba("a=f,i=1", 2, 1));
        assert_eq!(fx.state.used_bytes(), 24 + 8 + (8 + 8));
        fx.send("a=d,d=I,i=1");
        assert_eq!(fx.state.used_bytes(), 0);
        fx.finish_ok();
        assert_eq!(fx.state.used_bytes(), 0);
        assert_eq!(fx.replies(), ["i=1,r=3;OK", "i=1,r=4;OK", "i=1;OK"]);
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn a_queued_payload_counts_against_the_quota_at_admission() {
        // 16 bytes: a 1x1 root and three more frames, or fewer with
        // payloads waiting.
        let mut fx = Fixture::with_quota(16);
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1", 1, 1));
        assert_eq!(fx.state.used_bytes(), 12, "root, frame, payload");
        fx.send(&rgba("a=f,i=1", 1, 1));
        assert_eq!(
            fx.state.used_bytes(),
            12,
            "a second frame and its payload would make 20: refused"
        );
        fx.finish_ok();
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1,r=2;OK",
                "i=1;ENOSPC:Cache size exceeded cannot add new frames"
            ]
        );
        assert_eq!(fx.state.used_bytes(), 8);
        assert_eq!(fx.image(1).frame_count(), 2);
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn frame_refusals_echo_r_on_every_path() {
        let mut fx = Fixture::new();
        // A continuation chunk that fails: the first chunk's r.
        fx.send(&format!(
            "a=f,i=1,r=2,f=32,s=1,v=1,m=1;{}",
            encode_payload(&[0; 2])
        ));
        fx.send("m=0;!!!!");
        assert!(!fx.state.upload_in_flight());
        // A command that fails validation, load or control.
        fx.send(&rgba("a=f,i=1,I=2,r=3", 1, 1));
        fx.send("a=a,i=1,I=2,r=4,z=5");
        // An oversized command: whole, or continuing an upload.
        fx.truncated("Ga=f,i=1,r=5,f=32,s=1,v=1;AAAA");
        fx.send(&format!(
            "a=f,i=1,r=6,f=32,s=1,v=1,m=1;{}",
            encode_payload(&[0; 2])
        ));
        fx.truncated("Gm=0;AAAA");
        assert!(!fx.state.upload_in_flight());
        // Never for another action: a compose does not echo r.
        fx.send("a=c,i=1,I=2,r=7,c=1");
        assert_eq!(
            fx.replies(),
            [
                "i=1,r=2;EINVAL:payload is not valid base64: Invalid symbol 33, offset 0.",
                "i=1,I=2,r=3;EINVAL:image id and image number cannot both be specified",
                "i=1,I=2,r=4;EINVAL:image id and image number cannot both be specified",
                "i=1,r=5;EINVAL:graphics command exceeds the size limit",
                "i=1,r=6;EINVAL:graphics command exceeds the size limit",
                "i=1,I=2;EINVAL:image id and image number cannot both be specified",
            ]
        );
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.image_count(), 0);
    }

    #[test]
    fn a_landed_frame_nothing_holds_gives_its_charge_back() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1", 1, 1));
        assert_eq!(fx.state.used_bytes(), 8);
        // The record loses its frames some way the state machine never
        // takes itself (a host's test hook): the landed frame has no root
        // to follow, so it is dropped and its bytes released.
        let image = fx.image(1);
        image.set_data(ImageData::Pending);
        assert_eq!(image.frame_count(), 0);
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK", "i=1,r=2;OK"], "the data arrived");
        assert_eq!((image.frame_count(), fx.state.used_bytes()), (0, 4));
        assert!(fx.state.image(1).is_some(), "the image stays");
    }

    #[test]
    fn a_retransmit_leaves_the_old_record_s_queued_frames_to_run_on_it() {
        let mut fx = Fixture::new();
        let shown = fx.send(&rgba("a=T,i=1", 1, 1)).unwrap();
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send("a=a,i=1,s=3");
        fx.send("a=c,i=1,r=1,c=2");
        let old = shown.image.clone();
        assert_eq!(
            fx.state.used_bytes(),
            4 + 8,
            "the root; the frame and its payload"
        );
        fx.send(&rgba("a=t,i=1", 1, 1));
        let new = fx.image(1);
        assert!(!Arc::ptr_eq(&old, &new));
        assert!(
            !old.is_evicted(),
            "the placement keeps the old image, and its queue with it"
        );
        assert_eq!(fx.state.used_bytes(), 16);
        assert_eq!(fx.host.decodes.len(), 2, "both roots decode");
        // The old root lands: its frame commands run on it, in order,
        // answering under the id they were sent with; the new root's
        // answer waits its turn behind them.
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1;OK"]);
        assert_eq!(
            fx.host.decodes.len(),
            2,
            "the old image's frame joined the new root"
        );
        fx.finish_ok();
        assert!(
            fx.replies().is_empty(),
            "the new root landed; its reply waits"
        );
        assert_eq!(new.frame_count(), 1);
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=1,r=2;OK"]);
        assert_eq!(old.animation().state, AnimationState::Running);
        fx.finish_composed(None);
        assert_eq!(fx.replies(), ["i=1;OK", "i=1;OK"]);
        assert_eq!((old.frame_count(), new.frame_count()), (2, 1));
        assert_eq!(new.animation(), Animation::default());
        assert_eq!(fx.state.used_bytes(), 12);
        assert!(fx.host.decodes.is_empty());
        assert!(Arc::ptr_eq(&shown.image, &old));
    }

    #[test]
    fn a_compose_landing_misshapen_on_an_evicted_image_answers_and_holds_nothing() {
        let mut fx = Fixture::new();
        fx.send(&rgba("a=t,i=1", 2, 2));
        fx.finish_ok();
        let image = fx.image(1);
        image.push_frame(pixels_for(2, 2), 40).unwrap();
        fx.send("a=c,i=1,r=1,c=2");
        assert_eq!(fx.host.decodes.len(), 1);
        fx.send("a=d,d=I,i=1");
        assert!(image.is_evicted() && fx.state.image(1).is_none());
        assert_eq!(fx.state.used_bytes(), 0);
        fx.finish(|_| Ok(pixels_for(1, 1)));
        assert_eq!(
            fx.replies(),
            [
                "i=1;OK",
                "i=1;EINVAL:decoded image is 1x1 in 4 bytes, expected 2x2"
            ]
        );
        assert_eq!(
            (
                image.frame_count(),
                fx.state.used_bytes(),
                fx.state.decodes_in_flight()
            ),
            (0, 0, 0)
        );
        assert!(fx.host.decodes.is_empty());
    }

    #[test]
    fn a_frame_in_flight_for_an_image_the_quota_evicted_lands_nowhere_and_answers() {
        // 24 bytes: image 1 (1x1) with a frame in flight and another
        // waiting, then image 2 (2x2) needing 16.
        let mut fx = Fixture::with_quota(24);
        fx.send(&rgba("a=t,i=1", 1, 1));
        fx.finish_ok();
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send(&rgba("a=f,i=1", 1, 1));
        fx.send("a=a,i=1,s=3");
        let image = fx.image(1);
        assert_eq!(
            fx.state.used_bytes(),
            4 + 4 + (4 + 4),
            "root; frame in flight; frame waiting and its payload"
        );
        assert_eq!(fx.host.decodes.len(), 1);
        fx.send(&rgba("a=t,i=2", 2, 2));
        assert!(
            image.is_evicted() && fx.state.image(1).is_none(),
            "image 1, unplaced, went for image 2"
        );
        assert_eq!(fx.state.used_bytes(), 16, "only image 2 now");
        assert_eq!(
            fx.host.decodes.len(),
            2,
            "image 1's frame is still with the host"
        );
        fx.finish_ok();
        assert_eq!(
            fx.replies(),
            ["i=1;OK", "i=1,r=2;OK", "i=1;OK"],
            "the data arrived"
        );
        assert_eq!((image.frame_count(), fx.state.used_bytes()), (0, 16));
        assert_eq!(
            image.animation().state,
            AnimationState::Stopped,
            "the control never ran"
        );
        fx.finish_ok();
        assert_eq!(fx.replies(), ["i=2;OK"]);
        assert!(fx.host.decodes.is_empty());
        assert_eq!(fx.state.decodes_in_flight(), 0);
    }

    #[test]
    fn an_animation_with_no_frames_is_current_at_one_and_changes_nothing() {
        use AnimationState::Running;
        let record = ImageRecord::new(1, 0, 1, 1);
        assert_eq!((record.frame_count(), record.animation().current), (0, 1));
        assert!(
            record.push_frame(pixels_for(1, 1), 40).is_none(),
            "a frame is never the root"
        );
        assert!(!record.set_frame_gap(1, 5));
        assert!(!record.remove_frame(1));
        assert!(record.replace_frame(1, pixels_for(1, 1)).is_none());
        assert!(
            !record.animate(&control("a=a,c=1,r=1,z=5")),
            "no frame to make current or to time"
        );
        assert!(record.animate(&control("a=a,s=3,v=2")));
        let animation = record.animation();
        assert_eq!(
            (
                animation.state,
                animation.loops,
                animation.current,
                animation.generation,
                animation.control_serial
            ),
            (Running, 1, 1, 0, 1)
        );
        // An evicted image keeps no frames either; what the client set
        // stays, for the renderer to clamp against the frames it reads.
        record.set_data(ImageData::Ready(pixels_for(1, 1)));
        record.push_frame(pixels_for(1, 1), 40).unwrap();
        assert!(record.animate(&control("a=a,c=2")));
        assert_eq!(record.animation().current, 2);
        record.set_data(ImageData::Evicted);
        assert_eq!((record.frame_count(), record.animation().current), (0, 2));
    }

    // --- the UI slot ---

    /// Flags its drop.
    struct Token(Arc<AtomicBool>);

    impl Drop for Token {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn token() -> (UiSlot, Arc<AtomicBool>) {
        let dropped = Arc::new(AtomicBool::new(false));
        (Box::new(Token(dropped.clone())), dropped)
    }

    #[test]
    fn the_ui_slot_lives_as_long_as_the_pixels() {
        let record = ImageRecord::new(1, 0, 1, 1);
        let (first, first_dropped) = token();
        assert!(record.attach_ui_slot(first).is_ok());
        assert!(record.has_ui_slot());
        // A second slot is handed back; the first stays.
        let (second, second_dropped) = token();
        let refused = record
            .attach_ui_slot(second)
            .expect_err("one slot per record");
        assert!(refused.is::<Token>() && !second_dropped.load(Ordering::SeqCst));
        drop(refused);
        assert!(second_dropped.load(Ordering::SeqCst));
        assert!(record.has_ui_slot() && !first_dropped.load(Ordering::SeqCst));
        // Pixels landing keep it.
        record.set_data(ImageData::Ready(DecodedPixels {
            width: 1,
            height: 1,
            rgba: vec![0; 4].into(),
        }));
        assert!(record.has_ui_slot() && !first_dropped.load(Ordering::SeqCst));
        // An eviction drops it with the pixels.
        record.set_data(ImageData::Evicted);
        assert!(first_dropped.load(Ordering::SeqCst));
        assert!(!record.has_ui_slot());
        // And refuses any offered afterwards, untouched.
        let (third, third_dropped) = token();
        let back = record
            .attach_ui_slot(third)
            .expect_err("an evicted record takes no slot");
        assert!(!third_dropped.load(Ordering::SeqCst));
        drop(back);
        assert!(third_dropped.load(Ordering::SeqCst));
        assert!(!record.has_ui_slot());
    }

    #[test]
    fn the_ui_slot_drops_with_its_record() {
        let record = ImageRecord::new(1, 0, 1, 1);
        let (slot, dropped) = token();
        record.attach_ui_slot(slot).unwrap();
        assert!(format!("{record:?}").contains("ui_slot: true"));
        drop(record);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn a_quota_eviction_drops_the_ui_slot() {
        let mut fx = Fixture::with_quota(16);
        fx.send(&rgba("a=t,i=1", 2, 2));
        fx.finish_ok();
        let first = fx.state.image(1).unwrap();
        let (slot, dropped) = token();
        first.attach_ui_slot(slot).unwrap();
        fx.send(&rgba("a=t,i=2", 2, 2));
        assert!(first.is_evicted() && dropped.load(Ordering::SeqCst));
    }

    // --- geometry ---

    #[test]
    fn natural_size_covers_the_cells_it_spans_from_its_offset() {
        let at = |keys: &str| geometry(&control(keys), 25, 41, px(10, 20));
        let plain = at("");
        assert_eq!((plain.columns, plain.rows, plain.fit), (3, 3, Fit::Natural));
        let offset = at("X=5,Y=19");
        assert_eq!((offset.x_offset, offset.y_offset), (5, 19));
        assert_eq!((offset.columns, offset.rows), (3, 3));
        let pushed = at("X=6");
        assert_eq!(pushed.columns, 4, "25 px from 6 px in spans four cells");
        let clamped = at("X=99,Y=99");
        assert_eq!(
            (clamped.x_offset, clamped.y_offset),
            (9, 19),
            "offsets stay inside the first cell"
        );
    }

    #[test]
    fn the_source_rectangle_is_intersected_with_the_image() {
        let at = |keys: &str| geometry(&control(keys), 100, 50, px(10, 20)).source;
        let rect = |x, y, width, height| SourceRect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(at(""), rect(0, 0, 100, 50));
        assert_eq!(at("x=10,y=5"), rect(10, 5, 90, 45));
        assert_eq!(at("x=10,y=5,w=20,h=10"), rect(10, 5, 20, 10));
        assert_eq!(at("x=90,w=50,h=500"), rect(90, 0, 10, 50));
        assert_eq!(at("x=200,y=60"), rect(100, 50, 0, 0));
        // An empty source covers no cells, whatever box was asked for: the
        // placement shows nothing and the cursor moves by nothing.
        for keys in ["x=200,y=60", "x=200,y=60,c=3,r=2", "x=100,c=1", "y=50,r=1"] {
            let empty = geometry(&control(keys), 100, 50, px(10, 20));
            assert_eq!((empty.columns, empty.rows), (0, 0), "{keys}");
        }
        let tall = geometry(&control("c=1,x=1"), 1, 50, px(10, 20));
        assert_eq!(
            (tall.source.width, tall.columns, tall.rows),
            (0, 0, 0),
            "a zero-width source scales to no height"
        );
        let wide = geometry(&control("r=1,y=1"), 50, 1, px(10, 20));
        assert_eq!((wide.source.height, wide.columns, wide.rows), (0, 0, 0));
        let cropped = geometry(&control("x=10,w=31,h=20"), 100, 50, px(10, 20));
        assert_eq!((cropped.columns, cropped.rows), (4, 1));
    }

    #[test]
    fn columns_and_rows_scale_the_image_preserving_its_aspect() {
        let at = |keys: &str| geometry(&control(keys), 200, 100, px(10, 20));
        let both = at("c=20,r=5");
        assert_eq!(
            (both.columns, both.rows, both.fit),
            (
                20,
                5,
                Fit::Box {
                    columns: 20,
                    rows: 5
                }
            )
        );
        // 10 columns = 100 px wide, so 50 px tall: three 20 px rows.
        let columns = at("c=10");
        assert_eq!(
            (columns.columns, columns.rows, columns.fit),
            (
                10,
                3,
                Fit::Box {
                    columns: 10,
                    rows: 3
                }
            )
        );
        // 2 rows = 40 px tall, so 80 px wide: eight columns.
        let rows = at("r=2");
        assert_eq!((rows.columns, rows.rows), (8, 2));
        assert_eq!(at("z=-7").z, -7);
    }

    #[test]
    fn a_derived_dimension_counts_offsets_as_kitty_does() {
        // kitty scales the image to the given cells plus their own offset
        // (`width_px = cell_w * c + X`) and counts the other side in cells
        // from the cell's edge, without that side's offset.
        let at = |keys: &str| geometry(&control(keys), 200, 100, px(10, 20));
        let columns = at("c=10,Y=19");
        assert_eq!(
            (columns.columns, columns.rows, columns.y_offset),
            (10, 3, 19),
            "100 px wide is 50 px tall: three rows, Y notwithstanding"
        );
        let rows = at("r=2,Y=19");
        assert_eq!(
            (rows.columns, rows.rows),
            (12, 2),
            "40 + 19 px tall is 118 px wide: twelve columns"
        );
        let pushed = at("c=10,X=5");
        assert_eq!(
            (pushed.columns, pushed.rows),
            (10, 3),
            "105 px wide is 53 px tall"
        );
        let unmoved = at("r=2,X=9");
        assert_eq!(
            (unmoved.columns, unmoved.rows),
            (8, 2),
            "X does not widen a derived width"
        );
    }

    #[test]
    fn columns_and_rows_are_capped_separately() {
        let both = geometry(&control("c=100000,r=70000"), 200, 100, px(10, 20));
        assert_eq!(
            (both.columns, both.rows),
            (MAX_PLACEMENT_CELLS, MAX_PLACEMENT_ROWS)
        );
        // `c=1` on a 1x10000 image: 10 px wide is 100000 px tall, which the
        // row cap keeps from becoming a line thousands of pixels high.
        let ribbon = geometry(&control("c=1"), 1, 10_000, px(10, 20));
        assert_eq!((ribbon.columns, ribbon.rows), (1, MAX_PLACEMENT_ROWS));
        let strip = geometry(&control("r=1"), 10_000, 1, px(10, 20));
        assert_eq!((strip.columns, strip.rows), (MAX_PLACEMENT_CELLS, 1));
        let natural = geometry(&control(""), 10_000, 10_000, px(10, 20));
        assert_eq!((natural.columns, natural.rows), (1000, MAX_PLACEMENT_ROWS));
    }

    #[test]
    fn the_fallback_cell_lays_out_until_the_ui_reports_one() {
        for cell in [
            (0, 0),
            (0, 20 * CELL_FIXED_SCALE),
            (10 * CELL_FIXED_SCALE, 0),
        ] {
            let laid = geometry(&control(""), 18, 36, cell);
            assert_eq!((laid.columns, laid.rows), (2, 2), "{cell:?}");
        }
        assert_eq!(FALLBACK_CELL, px(9, 18));
        // A cell of one fixed-point unit: 1/256 px.
        let tiny = geometry(&control(""), 10_000, 10_000, (1, 1));
        assert_eq!(
            (tiny.columns, tiny.rows),
            (MAX_PLACEMENT_CELLS, MAX_PLACEMENT_ROWS)
        );
    }

    #[test]
    fn a_fractional_cell_counts_the_columns_the_image_really_spans() {
        // A 9.6 px advance: 96 px is ten columns exactly, not the eleven a
        // cell truncated to 9 px reserved, which left a blank strip before
        // the text after the image.
        let cell = (cell_fixed(9.6), cell_fixed(18.0));
        assert_eq!(cell, (2458, 4608));
        let ten = geometry(&control(""), 96, 36, cell);
        assert_eq!(
            (ten.columns, ten.rows),
            (10, 2),
            "rows are unchanged for a whole height"
        );
        assert_eq!(geometry(&control(""), 97, 36, cell).columns, 11);
        assert_eq!(geometry(&control(""), 96, 37, cell).rows, 3);
        // A derived side scales from the real cell too: ten columns are
        // 96 px, so a 2:1 image is 48 px tall, three 18 px rows.
        let derived = geometry(&control("c=10"), 200, 100, cell);
        assert_eq!((derived.columns, derived.rows), (10, 3));
        // The reports a client derives its own arithmetic from say 9, so a
        // text area reckoned from them never exceeds the real one.
        assert_eq!(cell_whole_pixels(cell.0), 9);
        assert_eq!(cell_whole_pixels(cell.1), 18);
        // Offsets stay inside the first cell's whole pixels.
        let offset = geometry(&control("X=99,Y=99"), 96, 36, cell);
        assert_eq!((offset.x_offset, offset.y_offset), (9, 17));
    }

    #[test]
    fn cell_sizes_round_to_the_nearest_fixed_point_unit() {
        assert_eq!(cell_fixed(9.6), 2458);
        assert_eq!(cell_fixed(10.0), 2560);
        assert_eq!(cell_fixed(9.999_999), 2560, "a nominal 10 px stays whole");
        assert_eq!(cell_fixed(0.0), 0);
        assert_eq!(cell_fixed(-1.0), 0);
        assert_eq!(cell_fixed(f32::NAN), 0);
        assert_eq!(cell_fixed(f32::INFINITY), 0);
        assert_eq!(cell_fixed(1e9), u16::MAX);
        assert_eq!(cell_whole_pixels(cell_fixed(19.2)), 19);
        assert!(f32::from(cell_whole_pixels(cell_fixed(9.6))) * 80.0 <= 9.6 * 80.0);
    }
}
