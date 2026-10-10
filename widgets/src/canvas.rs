//! The script-drawn `Canvas` component: a display-list scene (JSON shape records, static or
//! store-bound) rendered through an iced [`canvas::Program`], with host-side tweening.
//!
//! Design (plans/widgets-iced-primitives.md §11, until `docs/widgets.md` absorbs the as-built
//! record):
//!
//! - **The scene is data.** A scene is an array of shape records — plain JSON, so a bound scene
//!   (`scene={hud.bind("scene")}`) makes the session store the drawing channel: a producer writes
//!   records at data cadence and the canvas repaints with no V8 involvement, not even during
//!   animation.
//! - **Parsing is pure.** [`parse_scene`] maps a [`Node`] to a [`ParsedScene`] with no renderer in
//!   sight, which is what makes the scene grammar, budgets, and tween math unit-testable in a
//!   crate that has no headless widget harness.
//! - **Budgets reject generations atomically.** A scene that exceeds a complexity budget (or
//!   duplicates an animation id) is rejected whole — the previously accepted scene stays on
//!   screen — because truncating a record tree can drop a `group` transform and silently change
//!   the meaning of every surviving record. Per-record *parse* errors (a bad color, an unknown
//!   kind) skip just that record: malformed data is recoverable, exceeded budgets are not.
//! - **Paint order is sacred.** Records draw in order into one frame. The geometry cache is used
//!   only while nothing animates; an animating scene redraws whole, in order, every tick — a
//!   static/animated layer split would hoist every animated record above every static one.
//! - **Animation is host-tweened.** `animate` specs are pure functions of elapsed time, keyed by
//!   record `id` so a bound-scene rewrite mid-flight preserves a running animation's clock
//!   (same id + same spec), restarts it on a spec change (an intentional retrigger), and never
//!   resurrects a completed `transient` (the retained clock keeps it past its end).
//! - **The redraw loop is self-driven.** `Program::update` sees `RedrawRequested` and returns
//!   `Action::request_redraw()` while animations run — no messages ride the application update
//!   loop at frame rate (the `MapView` publish loop exists because map animation advances
//!   app-side state; a scene canvas has none).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iced::widget::canvas;
use iced::{Color, Point, Radians, Rectangle, Size, Vector, mouse, window};
use smudgy_cloud::image_source::{
    ImageSourcePolicy, RegisteredImageCreator, ResolvedImageSource, resolve_src,
};
use smudgy_cloud::{Node, StoreBindingCell, WidgetIsolate};

use crate::WidgetMessage;
use crate::image_store::{EntryState, ImageEntryCell, ImageStore};

// ---------------------------------------------------------------------------------------------
// Complexity budgets (plans/widgets-iced-primitives.md §11). Exceeding any of these rejects the
// scene generation atomically; see the module docs for why rejection is all-or-nothing.

pub(crate) const MAX_RECORDS: usize = 10_000;
pub(crate) const MAX_IMAGE_RECORDS: usize = 256;
/// How many NEVER-SEEN image sources one bound-scene write may spawn fetches for. A bound
/// scene's producer (a game server, another package) mints a fresh snapshot per write and
/// could otherwise name 256 nonce URLs each time — an unbounded fetch/disk amplifier. A
/// legitimate scene's sources recur, hit the store map, and never charge this budget.
pub(crate) const MAX_NEW_BOUND_SOURCES: usize = 64;
pub(crate) const MAX_DEPTH: usize = 16;
pub(crate) const MAX_SEGMENTS_PER_PATH: usize = 10_000;
pub(crate) const MAX_SEGMENTS_TOTAL: usize = 100_000;
pub(crate) const MAX_TEXT_BYTES_PER_RECORD: usize = 4 * 1024;
pub(crate) const MAX_TEXT_BYTES_TOTAL: usize = 256 * 1024;
pub(crate) const MAX_GRADIENT_STOPS: usize = 8;
pub(crate) const MAX_ANIMATED_FIELDS: usize = 1_000;
/// Approximate serialized-size ceiling. Counted from the parsed content (string bytes plus a
/// flat per-record/per-segment charge) rather than re-serializing — budgets bound runaway
/// producers, they don't bill exact bytes (the same stance as the store's `Usage` accounting).
pub(crate) const MAX_SCENE_BYTES: usize = 2 * 1024 * 1024;
const RECORD_OVERHEAD_BYTES: usize = 64;
const SEGMENT_OVERHEAD_BYTES: usize = 16;

// ---------------------------------------------------------------------------------------------
// Scene model

/// A solid color or an endpoint-based linear gradient. Geometry gradients are
/// `iced_graphics::gradient::Linear` — absolute `start`/`end` points and at most
/// [`MAX_GRADIENT_STOPS`] stops. (Not the angle-based `iced_core` gradient: that type is for
/// widget backgrounds; the canvas geometry pipeline takes endpoints, and the renderers apply
/// the frame's current transform to them, so endpoints authored in scene coordinates follow
/// `view_box` scaling and group transforms like any other geometry.)
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Paint {
    Solid(Color),
    Gradient {
        start: Point,
        end: Point,
        stops: Vec<(f32, Color)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StrokeSpec {
    pub paint: Paint,
    pub width: f32,
    pub dash: Vec<f32>,
}

/// One parsed SVG path-data command, coordinates already absolute. `H`/`V`/`S`/`T` and all
/// relative forms are resolved at parse; `A` arcs are flattened to cubic segments (see
/// [`path_data`]), so drawing only ever walks these five.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PathCommand {
    MoveTo(Point),
    LineTo(Point),
    Quad { control: Point, to: Point },
    Cubic { c1: Point, c2: Point, to: Point },
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextSpec {
    pub x: f32,
    pub y: f32,
    pub content: String,
    pub size: f32,
    pub color: Color,
    pub align_x: TextAlignX,
    pub align_y: TextAlignY,
    pub monospace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextAlignX {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextAlignY {
    Top,
    Center,
    Bottom,
}

/// How an `image` record's pixels map onto its `x/y/width/height` box. `Fill` (the
/// default) stretches exactly to the box, like every other shape fills its geometry;
/// the rest follow the widget `content_fit` semantics inside the box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ImageFit {
    #[default]
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

/// A scene image's live slot: the resolution inputs retained from parse time plus a
/// swappable store entry cell. `update()`'s per-redraw refresh walk calls [`refresh`]:
/// it stamps recency (a canvas's cached geometry means `draw` does NOT read the cell per
/// frame — without the walk a displayed image would go LRU-cold and become `evict_cold`'s
/// preferred victim while still on screen) and re-`ensure`s an evicted/flushed cell from
/// the retained `(key, source, policy)` so the image comes back after a cache clear.
///
/// [`refresh`]: ImageCell::refresh
pub(crate) struct ImageCell {
    key: Arc<str>,
    source: ResolvedImageSource,
    policy: Arc<ImageSourcePolicy>,
    cell: arc_swap::ArcSwap<ImageEntryCell>,
}

impl ImageCell {
    pub(crate) fn new(
        key: Arc<str>,
        source: ResolvedImageSource,
        policy: Arc<ImageSourcePolicy>,
        cell: Arc<ImageEntryCell>,
    ) -> Self {
        Self {
            key,
            source,
            policy,
            cell: arc_swap::ArcSwap::new(cell),
        }
    }

    /// The current entry state (one lock-free cell load; stamps recency via `state()`).
    fn state(&self) -> Arc<EntryState> {
        self.cell.load().state()
    }

    /// Keep the slot honest across redraws: touch the LRU stamp, and when the store
    /// evicted/flushed the cell, re-`ensure` from the retained inputs and swap the fresh
    /// cell in. Steady state is two lock-free loads + a relaxed store; the re-ensure path
    /// (writer lock + possible fetch spawn) only runs after an eviction — rare, and never
    /// on the draw path (`update()` calls this, where state is mutable by design).
    fn refresh(&self, store: &ImageStore) {
        let cell = self.cell.load();
        cell.touch();
        if cell.is_evicted() {
            self.cell
                .store(store.ensure_keyed(&self.key, &self.source, &self.policy));
        }
    }
}

impl std::fmt::Debug for ImageCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageCell({})", self.key)
    }
}

impl PartialEq for ImageCell {
    fn eq(&self, other: &Self) -> bool {
        // Same store key = same content identity (the swappable cell is derived state).
        self.key == other.key
    }
}

/// An `image` record: a policy-resolved raster drawn into a scene-unit box. The slot was
/// resolved + ensured at parse time (never during draw); `slot` is `None` when no image
/// context was available (a scene parsed outside a creator, e.g. tests) — the record then
/// draws nothing.
#[derive(Clone, Debug)]
pub(crate) struct ImageSpec {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub fit: ImageFit,
    pub nearest: bool,
    pub rotate_deg: f32,
    pub slot: Option<Arc<ImageCell>>,
}

impl PartialEq for ImageSpec {
    fn eq(&self, other: &Self) -> bool {
        let slots_match = match (&self.slot, &other.slot) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        slots_match
            && self.x == other.x
            && self.y == other.y
            && self.width == other.width
            && self.height == other.height
            && self.fit == other.fit
            && self.nearest == other.nearest
            && self.rotate_deg == other.rotate_deg
    }
}

/// Frozen `group` transform semantics: components apply translate → rotate → scale about the
/// group's local origin, regardless of key order in the record.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Transform {
    pub translate: Vector,
    pub rotate_deg: f32,
    pub scale: Vector,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translate: Vector::new(0.0, 0.0),
            rotate_deg: 0.0,
            scale: Vector::new(1.0, 1.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Shape {
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        rx: f32,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Polyline {
        points: Vec<Point>,
    },
    Polygon {
        points: Vec<Point>,
    },
    Path {
        commands: Vec<PathCommand>,
    },
    Text(TextSpec),
    Image(ImageSpec),
    Group {
        transform: Transform,
        children: Vec<Record>,
    },
}

impl Shape {
    fn kind(&self) -> &'static str {
        match self {
            Self::Rect { .. } => "rect",
            Self::Circle { .. } => "circle",
            Self::Ellipse { .. } => "ellipse",
            Self::Line { .. } => "line",
            Self::Polyline { .. } => "polyline",
            Self::Polygon { .. } => "polygon",
            Self::Path { .. } => "path",
            Self::Text(_) => "text",
            Self::Image(_) => "image",
            Self::Group { .. } => "group",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TweenValue {
    Number(f32),
    Color(Color),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ease {
    Linear,
    In,
    Out,
    InOut,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Repeat {
    Count(u32),
    Infinite,
}

/// One per-field tween spec. `from: None` means "the record's static value for the field".
/// Frozen semantics: each repetition restarts `from → to` (no ping-pong), and `delay` applies
/// once, before the first repetition only.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tween {
    pub field: String,
    pub from: Option<TweenValue>,
    pub to: TweenValue,
    pub duration_ms: f32,
    pub delay_ms: f32,
    pub ease: Ease,
    pub repeat: Repeat,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Record {
    pub shape: Shape,
    pub fill: Option<Paint>,
    pub stroke: Option<StrokeSpec>,
    pub opacity: f32,
    pub id: Option<String>,
    pub animate: Vec<Tween>,
    pub transient: bool,
}

/// One accepted scene generation. `animated` counts records carrying `animate` anywhere in the
/// tree — zero means the geometry cache may serve every frame.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ParsedScene {
    pub records: Vec<Record>,
    pub animated: usize,
    /// Every resolved image slot in the scene (flattened through groups), collected once
    /// at parse so `update()`'s per-redraw refresh walk is a linear pass, not a tree walk.
    /// Empty for image-free scenes, which then skip the store-generation comparison —
    /// unrelated image loads never clear their cached geometry. Cell-less image records
    /// (no context) are excluded: they can never repaint.
    pub image_cells: Vec<Arc<ImageCell>>,
    /// Per-record soft failures, logged once per generation by the consumer.
    pub warnings: Vec<String>,
}

/// What an `image` record needs to resolve its `src` at parse time (plan D7): the canvas
/// widget's registered creator, the global store, and whether the scene arrived through a
/// store binding (bound values are descend-only — D2's provenance rule; the producer of a
/// bound scene is not the widget's author). `None` at a parse site means image records
/// skip with a warning (headless tests, missing store).
#[derive(Clone)]
pub(crate) struct SceneImageCtx {
    pub creator: Arc<RegisteredImageCreator>,
    pub store: ImageStore,
    pub bound: bool,
}

/// How an `image` record's `src` resolves during a scene parse.
///
/// - `Memoized`: script-thread parses (static scenes, binding fallbacks — both written by
///   the widget's author). Resolution goes through the per-isolate `ImageRegistry` memo,
///   so a per-frame `createWidget` re-parse of the same scene never re-runs URL parsing
///   or `data:`-payload hashing (the exact cost the memo exists to kill).
/// - `Live`: UI-thread parses of bound snapshot values (no `OpState` there). Direct
///   resolution with the ctx's provenance; never-seen sources charge the ledger's
///   [`MAX_NEW_BOUND_SOURCES`] budget when `ctx.bound`.
pub(crate) enum SceneImages<'a> {
    Memoized(&'a mut dyn FnMut(&str) -> ImageResolution),
    Live(&'a SceneImageCtx),
}

/// A memoized resolver's answer for one `src`.
pub(crate) enum ImageResolution {
    Resolved(Arc<ImageCell>),
    Rejected(String),
}

/// Why a whole generation was refused (the previous scene stays on screen).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SceneReject {
    NotAnArray,
    Budget(&'static str),
    DuplicateId(String),
}

impl std::fmt::Display for SceneReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnArray => write!(f, "scene is not an array of shape records"),
            Self::Budget(which) => write!(f, "scene exceeds the {which} budget"),
            Self::DuplicateId(id) => write!(f, "duplicate animation id {id:?}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Parsing

/// Running totals for the whole-scene budgets, shared across the record-tree walk.
#[derive(Default)]
struct BudgetLedger {
    records: usize,
    segments: usize,
    text_bytes: usize,
    animated_fields: usize,
    approx_bytes: usize,
    image_records: usize,
    /// Never-seen (store-map-miss) image sources this parse has spawned fetches for —
    /// charged only for bound scenes (see [`MAX_NEW_BOUND_SOURCES`]).
    new_bound_sources: usize,
}

impl BudgetLedger {
    fn check(&self) -> Result<(), SceneReject> {
        if self.records > MAX_RECORDS {
            return Err(SceneReject::Budget("record-count"));
        }
        if self.image_records > MAX_IMAGE_RECORDS {
            return Err(SceneReject::Budget("image-records"));
        }
        if self.segments > MAX_SEGMENTS_TOTAL {
            return Err(SceneReject::Budget("path-segment"));
        }
        if self.text_bytes > MAX_TEXT_BYTES_TOTAL {
            return Err(SceneReject::Budget("text-bytes"));
        }
        if self.animated_fields > MAX_ANIMATED_FIELDS {
            return Err(SceneReject::Budget("animated-fields"));
        }
        if self.approx_bytes > MAX_SCENE_BYTES {
            return Err(SceneReject::Budget("scene-bytes"));
        }
        Ok(())
    }
}

/// Parse a scene value into an accepted generation, or reject it whole. The input is the
/// store's [`Node`] shape for both sources: bound scenes load it straight from the binding
/// cell, static scenes convert once at build. `images` supplies the creator/store context
/// `image` records resolve against (their `src` is validated and `ensure`d HERE, at parse —
/// the draw path only reads entry cells).
pub(crate) fn parse_scene(
    root: &Node,
    mut images: Option<&mut SceneImages>,
) -> Result<ParsedScene, SceneReject> {
    let Node::Array(items) = root else {
        return Err(SceneReject::NotAnArray);
    };
    let mut scene = ParsedScene::default();
    let mut ledger = BudgetLedger::default();
    let mut ids = HashSet::new();
    scene.records = parse_records(
        items.items(),
        0,
        &mut ledger,
        &mut ids,
        &mut scene.warnings,
        images.as_deref_mut(),
    )?;
    scene.animated = count_animated(&scene.records);
    collect_image_cells(&scene.records, &mut scene.image_cells);
    Ok(scene)
}

/// Flatten every resolved image slot in the record tree into `out` (parse-time, once).
fn collect_image_cells(records: &[Record], out: &mut Vec<Arc<ImageCell>>) {
    for record in records {
        match &record.shape {
            Shape::Image(image) => {
                if let Some(slot) = &image.slot {
                    out.push(slot.clone());
                }
            }
            Shape::Group { children, .. } => collect_image_cells(children, out),
            _ => {}
        }
    }
}

fn count_animated(records: &[Record]) -> usize {
    records
        .iter()
        .map(|record| {
            let own = usize::from(!record.animate.is_empty());
            let nested = match &record.shape {
                Shape::Group { children, .. } => count_animated(children),
                _ => 0,
            };
            own + nested
        })
        .sum()
}

fn parse_records(
    items: &[Node],
    depth: usize,
    ledger: &mut BudgetLedger,
    ids: &mut HashSet<String>,
    warnings: &mut Vec<String>,
    mut images: Option<&mut SceneImages>,
) -> Result<Vec<Record>, SceneReject> {
    if depth > MAX_DEPTH {
        return Err(SceneReject::Budget("nesting-depth"));
    }
    let mut records = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        ledger.records += 1;
        ledger.approx_bytes += RECORD_OVERHEAD_BYTES;
        ledger.check()?;
        match parse_record(item, depth, ledger, ids, warnings, images.as_deref_mut()) {
            Ok(record) => records.push(record),
            Err(RecordError::Reject(reject)) => return Err(reject),
            Err(RecordError::Skip(reason)) => {
                warnings.push(format!("record {index}: {reason}"));
            }
        }
    }
    Ok(records)
}

/// A per-record failure: either recoverable (skip the record, keep the scene) or a
/// generation-rejecting budget/identity violation discovered mid-record.
enum RecordError {
    Skip(String),
    Reject(SceneReject),
}

impl From<SceneReject> for RecordError {
    fn from(reject: SceneReject) -> Self {
        Self::Reject(reject)
    }
}

fn skip(reason: impl Into<String>) -> RecordError {
    RecordError::Skip(reason.into())
}

fn f32_field(obj: &Node, key: &str, default: f32) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    obj.get(key)
        .and_then(Node::as_f64)
        .map_or(default, |value| value as f32)
}

fn bool_field(obj: &Node, key: &str) -> bool {
    matches!(obj.get(key), Some(Node::Bool(true)))
}

/// One `[x, y]` pair (a `points` entry, a `transform.translate`, a two-component scale).
fn parse_pair(node: &Node) -> Result<Vector, RecordError> {
    let Node::Array(xy) = node else {
        return Err(skip("expected an [x, y] pair"));
    };
    #[allow(clippy::cast_possible_truncation)]
    match xy.items() {
        [x, y] => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => Ok(Vector::new(x as f32, y as f32)),
            _ => Err(skip("pair entries must be numbers")),
        },
        _ => Err(skip("expected an [x, y] pair")),
    }
}

fn parse_points(node: &Node) -> Result<Vec<Point>, RecordError> {
    let Node::Array(items) = node else {
        return Err(skip("points must be an array of [x, y] pairs"));
    };
    items
        .items()
        .iter()
        .map(|pair| parse_pair(pair).map(|v| Point::new(v.x, v.y)))
        .collect()
}

fn parse_paint(node: &Node, ledger: &mut BudgetLedger) -> Result<Paint, RecordError> {
    if let Some(text) = node.as_str() {
        ledger.approx_bytes += text.len();
        return smudgy_cloud::parse_css_color(text)
            .map(Paint::Solid)
            .ok_or_else(|| skip(format!("unparseable color {text:?}")));
    }
    let Some(gradient) = node.get("gradient") else {
        return Err(skip(
            "fill/stroke color must be a CSS color string or { gradient }",
        ));
    };
    let start = parse_pair(
        gradient
            .get("from")
            .ok_or_else(|| skip("gradient needs from: [x, y]"))?,
    )?;
    let end = parse_pair(
        gradient
            .get("to")
            .ok_or_else(|| skip("gradient needs to: [x, y]"))?,
    )?;
    let Some(Node::Array(stop_items)) = gradient.get("stops") else {
        return Err(skip(
            "gradient.stops must be an array of [offset, color] pairs",
        ));
    };
    if stop_items.items().len() > MAX_GRADIENT_STOPS {
        return Err(SceneReject::Budget("gradient-stops").into());
    }
    let mut stops = Vec::with_capacity(stop_items.items().len());
    for stop in stop_items.items() {
        let Node::Array(pair) = stop else {
            return Err(skip("gradient stops must be [offset, color] pairs"));
        };
        let [offset, color] = pair.items() else {
            return Err(skip("gradient stops must be [offset, color] pairs"));
        };
        #[allow(clippy::cast_possible_truncation)]
        let offset = offset
            .as_f64()
            .map(|offset| offset as f32)
            .filter(|offset| (0.0..=1.0).contains(offset))
            .ok_or_else(|| skip("gradient stop offsets must be numbers in 0..=1"))?;
        let color = color
            .as_str()
            .and_then(smudgy_cloud::parse_css_color)
            .ok_or_else(|| skip("gradient stop colors must be CSS color strings"))?;
        stops.push((offset, color));
    }
    Ok(Paint::Gradient {
        start: Point::new(start.x, start.y),
        end: Point::new(end.x, end.y),
        stops,
    })
}

fn parse_stroke(node: &Node, ledger: &mut BudgetLedger) -> Result<StrokeSpec, RecordError> {
    let paint = node.get("color").map_or_else(
        || Ok(Paint::Solid(Color::BLACK)),
        |color| parse_paint(color, ledger),
    )?;
    let width = f32_field(node, "width", 1.0);
    let dash = match node.get("dash") {
        None | Some(Node::Null) => Vec::new(),
        Some(Node::Array(items)) =>
        {
            #[allow(clippy::cast_possible_truncation)]
            items
                .items()
                .iter()
                .map(|item| {
                    item.as_f64()
                        .map(|len| len as f32)
                        .ok_or_else(|| skip("stroke.dash entries must be numbers"))
                })
                .collect::<Result<_, _>>()?
        }
        Some(_) => return Err(skip("stroke.dash must be an array of numbers")),
    };
    Ok(StrokeSpec { paint, width, dash })
}

// One flat match over the record kinds; splitting it would scatter the grammar.
#[allow(clippy::too_many_lines)]
fn parse_record(
    node: &Node,
    depth: usize,
    ledger: &mut BudgetLedger,
    ids: &mut HashSet<String>,
    warnings: &mut Vec<String>,
    mut images: Option<&mut SceneImages>,
) -> Result<Record, RecordError> {
    let Some(kind) = node.get("kind").and_then(Node::as_str) else {
        return Err(skip("record has no \"kind\""));
    };

    let shape = match kind {
        "rect" => Shape::Rect {
            x: f32_field(node, "x", 0.0),
            y: f32_field(node, "y", 0.0),
            width: f32_field(node, "width", 0.0),
            height: f32_field(node, "height", 0.0),
            rx: f32_field(node, "rx", 0.0),
        },
        "circle" => Shape::Circle {
            cx: f32_field(node, "cx", 0.0),
            cy: f32_field(node, "cy", 0.0),
            r: f32_field(node, "r", 0.0),
        },
        "ellipse" => Shape::Ellipse {
            cx: f32_field(node, "cx", 0.0),
            cy: f32_field(node, "cy", 0.0),
            rx: f32_field(node, "rx", 0.0),
            ry: f32_field(node, "ry", 0.0),
        },
        "line" => Shape::Line {
            x1: f32_field(node, "x1", 0.0),
            y1: f32_field(node, "y1", 0.0),
            x2: f32_field(node, "x2", 0.0),
            y2: f32_field(node, "y2", 0.0),
        },
        "polyline" | "polygon" => {
            let points = parse_points(node.get("points").ok_or_else(|| skip("missing points"))?)?;
            ledger.segments += points.len();
            ledger.approx_bytes += points.len() * SEGMENT_OVERHEAD_BYTES;
            ledger.check()?;
            if kind == "polyline" {
                Shape::Polyline { points }
            } else {
                Shape::Polygon { points }
            }
        }
        "path" => {
            let d = node
                .get("d")
                .and_then(Node::as_str)
                .ok_or_else(|| skip("path record has no \"d\" string"))?;
            ledger.approx_bytes += d.len();
            let commands =
                path_data::parse(d).map_err(|err| skip(format!("bad path data: {err}")))?;
            if commands.len() > MAX_SEGMENTS_PER_PATH {
                return Err(SceneReject::Budget("path-segment").into());
            }
            ledger.segments += commands.len();
            ledger.approx_bytes += commands.len() * SEGMENT_OVERHEAD_BYTES;
            ledger.check()?;
            Shape::Path { commands }
        }
        "text" => {
            let content = node
                .get("text")
                .and_then(Node::as_str)
                .ok_or_else(|| skip("text record has no \"text\""))?
                .to_string();
            if content.len() > MAX_TEXT_BYTES_PER_RECORD {
                return Err(SceneReject::Budget("text-bytes").into());
            }
            ledger.text_bytes += content.len();
            ledger.approx_bytes += content.len();
            ledger.check()?;
            let color = match node.get("color").and_then(Node::as_str) {
                None => Color::WHITE,
                Some(text) => smudgy_cloud::parse_css_color(text)
                    .ok_or_else(|| skip(format!("unparseable color {text:?}")))?,
            };
            let align_x = match node.get("align_x").and_then(Node::as_str) {
                None | Some("left" | "start") => TextAlignX::Left,
                Some("center") => TextAlignX::Center,
                Some("right" | "end") => TextAlignX::Right,
                Some(other) => return Err(skip(format!("unknown align_x {other:?}"))),
            };
            let align_y = match node.get("align_y").and_then(Node::as_str) {
                None | Some("top" | "start") => TextAlignY::Top,
                Some("center") => TextAlignY::Center,
                Some("bottom" | "end") => TextAlignY::Bottom,
                Some(other) => return Err(skip(format!("unknown align_y {other:?}"))),
            };
            Shape::Text(TextSpec {
                x: f32_field(node, "x", 0.0),
                y: f32_field(node, "y", 0.0),
                content,
                size: f32_field(node, "size", 16.0),
                color,
                align_x,
                align_y,
                monospace: node.get("font").and_then(Node::as_str) == Some("monospace"),
            })
        }
        "image" => {
            let src = node
                .get("src")
                .and_then(Node::as_str)
                .ok_or_else(|| skip("image record has no \"src\" string"))?;
            ledger.image_records += 1;
            ledger.approx_bytes += src.len();
            ledger.check()?;
            let fit = match node.get("fit").and_then(Node::as_str) {
                // A shape stretches to its box, like rect — unlike the widget's `contain`.
                None | Some("fill") => ImageFit::Fill,
                Some("contain") => ImageFit::Contain,
                Some("cover") => ImageFit::Cover,
                Some("none") => ImageFit::None,
                Some("scale-down") => ImageFit::ScaleDown,
                Some(other) => return Err(skip(format!("unknown image fit {other:?}"))),
            };
            let nearest = match node.get("filter").and_then(Node::as_str) {
                None | Some("linear") => false,
                Some("nearest") => true,
                Some(other) => return Err(skip(format!("unknown image filter {other:?}"))),
            };
            // Resolve + ensure at parse (plan D7): the policy check runs against the canvas
            // widget's registered creator. The draw path only ever reads the entry cell.
            let slot = match images.as_deref_mut() {
                None => {
                    warnings.push(
                        "image record has no image context here; it will draw nothing".to_string(),
                    );
                    None
                }
                Some(SceneImages::Memoized(resolve)) => match resolve(src) {
                    ImageResolution::Resolved(slot) => Some(slot),
                    ImageResolution::Rejected(reason) => {
                        return Err(skip(format!("image src rejected: {reason}")));
                    }
                },
                Some(SceneImages::Live(ctx)) => match resolve_src(src, &ctx.creator, ctx.bound) {
                    Ok(source) => {
                        let key = source.store_key(&ctx.creator.policy);
                        // Known sources ride the existing entry; never-seen ones spawn a
                        // fetch and (for bound scenes) charge the new-source budget.
                        let cell = if let Some(cell) = ctx.store.peek(&key) {
                            cell.touch();
                            cell
                        } else if ctx.bound && ledger.new_bound_sources >= MAX_NEW_BOUND_SOURCES {
                            return Err(skip(
                                "image budget: too many never-seen sources in one scene write",
                            ));
                        } else {
                            if ctx.bound {
                                ledger.new_bound_sources += 1;
                            }
                            ctx.store.ensure_keyed(&key, &source, &ctx.creator.policy)
                        };
                        Some(Arc::new(ImageCell::new(
                            Arc::from(key),
                            source,
                            ctx.creator.policy.clone(),
                            cell,
                        )))
                    }
                    Err(reason) => {
                        return Err(skip(format!("image src rejected: {reason}")));
                    }
                },
            };
            Shape::Image(ImageSpec {
                x: f32_field(node, "x", 0.0),
                y: f32_field(node, "y", 0.0),
                width: f32_field(node, "width", 0.0),
                height: f32_field(node, "height", 0.0),
                fit,
                nearest,
                rotate_deg: f32_field(node, "rotate", 0.0),
                slot,
            })
        }
        "group" => {
            let transform = match node.get("transform") {
                None | Some(Node::Null) => Transform::default(),
                Some(spec) => {
                    let translate = match spec.get("translate") {
                        None | Some(Node::Null) => Vector::new(0.0, 0.0),
                        Some(pair) => parse_pair(pair)?,
                    };
                    let scale = match spec.get("scale") {
                        None | Some(Node::Null) => Vector::new(1.0, 1.0),
                        Some(Node::Number(_)) => {
                            let scale = f32_field(spec, "scale", 1.0);
                            Vector::new(scale, scale)
                        }
                        Some(pair) => parse_pair(pair)?,
                    };
                    Transform {
                        translate,
                        rotate_deg: f32_field(spec, "rotate", 0.0),
                        scale,
                    }
                }
            };
            let children = match node.get("children") {
                Some(Node::Array(items)) => parse_records(
                    items.items(),
                    depth + 1,
                    ledger,
                    ids,
                    warnings,
                    images.as_deref_mut(),
                )?,
                _ => return Err(skip("group record has no \"children\" array")),
            };
            Shape::Group {
                transform,
                children,
            }
        }
        other => return Err(skip(format!("unknown kind {other:?}"))),
    };

    let fill = match node.get("fill") {
        None | Some(Node::Null) => None,
        Some(paint) => Some(parse_paint(paint, ledger)?),
    };
    let stroke = match node.get("stroke") {
        None | Some(Node::Null) => None,
        Some(spec) => Some(parse_stroke(spec, ledger)?),
    };
    let opacity = f32_field(node, "opacity", 1.0).clamp(0.0, 1.0);

    let id = node.get("id").and_then(Node::as_str).map(str::to_string);
    let animate = match node.get("animate") {
        None | Some(Node::Null) => Vec::new(),
        Some(spec) => parse_animate(spec, &shape, node, ledger)?,
    };
    if !animate.is_empty()
        && let Some(id) = &id
        && !ids.insert(id.clone())
    {
        return Err(SceneReject::DuplicateId(id.clone()).into());
    }
    let transient = bool_field(node, "transient");
    if transient && animate.is_empty() {
        warnings.push(format!(
            "{kind} record marked transient without animate; it will never complete"
        ));
    }

    Ok(Record {
        shape,
        fill,
        stroke,
        opacity,
        id,
        animate,
        transient,
    })
}

/// The numeric fields a tween may target on each shape kind, plus the shared paint/opacity
/// fields (`fill`, `stroke`, `color`, `opacity`, `stroke_width`) validated separately.
fn numeric_field_allowed(shape: &Shape, field: &str) -> bool {
    let allowed: &[&str] = match shape {
        Shape::Rect { .. } => &["x", "y", "width", "height", "rx"],
        Shape::Circle { .. } => &["cx", "cy", "r"],
        Shape::Ellipse { .. } => &["cx", "cy", "rx", "ry"],
        Shape::Line { .. } => &["x1", "y1", "x2", "y2"],
        Shape::Text(_) => &["x", "y", "size"],
        Shape::Image(_) => &["x", "y", "width", "height", "rotate"],
        Shape::Group { .. } => &["translate_x", "translate_y", "rotate", "scale"],
        Shape::Polyline { .. } | Shape::Polygon { .. } | Shape::Path { .. } => &[],
    };
    allowed.contains(&field)
}

fn color_field_allowed(shape: &Shape, field: &str) -> bool {
    match field {
        // A texture has no fill/stroke paint (record opacity routes to Image::opacity).
        "fill" | "stroke" => !matches!(
            shape,
            Shape::Group { .. } | Shape::Text(_) | Shape::Image(_)
        ),
        "color" => matches!(shape, Shape::Text(_)),
        _ => false,
    }
}

fn parse_animate(
    spec: &Node,
    shape: &Shape,
    record: &Node,
    ledger: &mut BudgetLedger,
) -> Result<Vec<Tween>, RecordError> {
    let Node::Object(fields) = spec else {
        return Err(skip("animate must be an object of per-field tween specs"));
    };
    let mut tweens = Vec::with_capacity(fields.iter().count());
    for (field, tween) in fields.iter() {
        ledger.animated_fields += 1;
        ledger.check()?;
        let is_color = color_field_allowed(shape, field);
        let is_number =
            field == "opacity" || field == "stroke_width" || numeric_field_allowed(shape, field);
        if !is_color && !is_number {
            return Err(skip(format!(
                "field {field:?} is not animatable on a {} record",
                shape.kind()
            )));
        }
        let parse_value = |node: &Node| -> Result<TweenValue, RecordError> {
            if is_color {
                node.as_str()
                    .and_then(smudgy_cloud::parse_css_color)
                    .map(TweenValue::Color)
                    .ok_or_else(|| skip(format!("animate.{field} endpoints must be CSS colors")))
            } else {
                #[allow(clippy::cast_possible_truncation)]
                node.as_f64()
                    .map(|value| TweenValue::Number(value as f32))
                    .ok_or_else(|| skip(format!("animate.{field} endpoints must be numbers")))
            }
        };
        let to = parse_value(
            tween
                .get("to")
                .ok_or_else(|| skip(format!("animate.{field} has no \"to\"")))?,
        )?;
        let from = match tween.get("from") {
            None | Some(Node::Null) => None,
            Some(node) => Some(parse_value(node)?),
        };
        let duration_ms = f32_field(tween, "duration", 0.0).max(0.0);
        let delay_ms = f32_field(tween, "delay", 0.0).max(0.0);
        let ease = match tween.get("ease").and_then(Node::as_str) {
            None | Some("linear") => Ease::Linear,
            Some("in") => Ease::In,
            Some("out") => Ease::Out,
            Some("in-out") => Ease::InOut,
            Some(other) => return Err(skip(format!("unknown ease {other:?}"))),
        };
        let repeat = match tween.get("repeat") {
            None | Some(Node::Null) => Repeat::Count(1),
            Some(Node::String(text)) if &**text == "infinite" => Repeat::Infinite,
            Some(node) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let count = node
                    .as_f64()
                    .filter(|count| *count >= 1.0)
                    .map(|count| count as u32)
                    .ok_or_else(|| skip("animate repeat must be a count >= 1 or \"infinite\""))?;
                Repeat::Count(count)
            }
        };
        // `from` defaulting to the record's static value happens here at parse (not per frame):
        // the record node is at hand and the resolved spec is what clock retention compares.
        let from = match from {
            Some(value) => Some(value),
            None => base_tween_value(shape, record, field, is_color),
        };
        tweens.push(Tween {
            field: field.to_string(),
            from,
            to,
            duration_ms,
            delay_ms,
            ease,
            repeat,
        });
    }
    Ok(tweens)
}

/// The record's own static value for `field`, used when a tween omits `from`. `None` (an
/// unset paint on a fill tween, say) makes the tween start from its `to` value — a degenerate
/// but harmless spec.
fn base_tween_value(
    shape: &Shape,
    record: &Node,
    field: &str,
    is_color: bool,
) -> Option<TweenValue> {
    if is_color {
        return record
            .get(field)
            .and_then(Node::as_str)
            .and_then(smudgy_cloud::parse_css_color)
            .map(TweenValue::Color);
    }
    if field == "opacity" {
        return Some(TweenValue::Number(f32_field(record, "opacity", 1.0)));
    }
    if field == "stroke_width" {
        return Some(TweenValue::Number(
            record
                .get("stroke")
                .map_or(1.0, |stroke| f32_field(stroke, "width", 1.0)),
        ));
    }
    let default = match (shape, field) {
        (Shape::Group { transform, .. }, "translate_x") => transform.translate.x,
        (Shape::Group { transform, .. }, "translate_y") => transform.translate.y,
        (Shape::Group { transform, .. }, "rotate") => transform.rotate_deg,
        (Shape::Group { transform, .. }, "scale") => transform.scale.x,
        _ => f32_field(record, field, 0.0),
    };
    Some(TweenValue::Number(default))
}

// ---------------------------------------------------------------------------------------------
// SVG path data (the `d` attribute)

pub(crate) mod path_data {
    //! A small, self-contained SVG path-data parser (SVG 2 §9.3 grammar): all commands,
    //! relative forms, implicit repetition, comma/whitespace separation, and unspaced arc
    //! flags. Arcs are converted endpoint→center and flattened to cubic segments here, so the
    //! draw path only handles Move/Line/Quad/Cubic/Close.

    use super::{PathCommand, Point};

    pub(crate) fn parse(d: &str) -> Result<Vec<PathCommand>, String> {
        Parser {
            bytes: d.as_bytes(),
            pos: 0,
        }
        .run()
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Parser<'_> {
        // One flat match over the command letters; splitting it would scatter the grammar.
        #[allow(clippy::too_many_lines)]
        fn run(mut self) -> Result<Vec<PathCommand>, String> {
            let mut out = Vec::new();
            // Path state: current point, current subpath start (for Z), the previous cubic /
            // quadratic control point (for S/T reflection), and the previous command letter.
            let mut current = Point::ORIGIN;
            let mut subpath_start = Point::ORIGIN;
            let mut last_cubic_control: Option<Point> = None;
            let mut last_quad_control: Option<Point> = None;
            let mut command: Option<u8> = None;

            self.skip_separators();
            while self.pos < self.bytes.len() {
                let byte = self.bytes[self.pos];
                let next = if byte.is_ascii_alphabetic() {
                    self.pos += 1;
                    byte
                } else {
                    // A coordinate where a command could sit repeats the previous command —
                    // except after M/m, whose implicit repetition is L/l (SVG 2 §9.3.3).
                    match command {
                        Some(b'M') => b'L',
                        Some(b'm') => b'l',
                        Some(previous) => previous,
                        None => return Err("path data must start with a command".to_string()),
                    }
                };
                command = Some(next);
                let relative = next.is_ascii_lowercase();
                let base = if relative { current } else { Point::ORIGIN };

                match next.to_ascii_uppercase() {
                    b'M' => {
                        let to = self.point(base)?;
                        out.push(PathCommand::MoveTo(to));
                        current = to;
                        subpath_start = to;
                        (last_cubic_control, last_quad_control) = (None, None);
                    }
                    b'L' => {
                        let to = self.point(base)?;
                        out.push(PathCommand::LineTo(to));
                        current = to;
                        (last_cubic_control, last_quad_control) = (None, None);
                    }
                    b'H' => {
                        let x = self.number()?;
                        let to = Point::new(base.x + x, current.y);
                        out.push(PathCommand::LineTo(to));
                        current = to;
                        (last_cubic_control, last_quad_control) = (None, None);
                    }
                    b'V' => {
                        let y = self.number()?;
                        let to = Point::new(current.x, base.y + y);
                        out.push(PathCommand::LineTo(to));
                        current = to;
                        (last_cubic_control, last_quad_control) = (None, None);
                    }
                    b'C' => {
                        let c1 = self.point(base)?;
                        let c2 = self.point(base)?;
                        let to = self.point(base)?;
                        out.push(PathCommand::Cubic { c1, c2, to });
                        current = to;
                        (last_cubic_control, last_quad_control) = (Some(c2), None);
                    }
                    b'S' => {
                        let c1 = reflect(last_cubic_control, current);
                        let c2 = self.point(base)?;
                        let to = self.point(base)?;
                        out.push(PathCommand::Cubic { c1, c2, to });
                        current = to;
                        (last_cubic_control, last_quad_control) = (Some(c2), None);
                    }
                    b'Q' => {
                        let control = self.point(base)?;
                        let to = self.point(base)?;
                        out.push(PathCommand::Quad { control, to });
                        current = to;
                        (last_cubic_control, last_quad_control) = (None, Some(control));
                    }
                    b'T' => {
                        let control = reflect(last_quad_control, current);
                        let to = self.point(base)?;
                        out.push(PathCommand::Quad { control, to });
                        current = to;
                        (last_cubic_control, last_quad_control) = (None, Some(control));
                    }
                    b'A' => {
                        let rx = self.number()?;
                        let ry = self.number()?;
                        let rotation_deg = self.number()?;
                        let large_arc = self.flag()?;
                        let sweep = self.flag()?;
                        let to = self.point(base)?;
                        arc_to_cubics(
                            current,
                            to,
                            rx.abs(),
                            ry.abs(),
                            rotation_deg,
                            large_arc,
                            sweep,
                            &mut out,
                        );
                        current = to;
                        (last_cubic_control, last_quad_control) = (None, None);
                    }
                    b'Z' => {
                        out.push(PathCommand::Close);
                        current = subpath_start;
                        (last_cubic_control, last_quad_control) = (None, None);
                    }
                    other => {
                        return Err(format!("unknown path command {:?}", char::from(other)));
                    }
                }
                self.skip_separators();
            }
            Ok(out)
        }

        fn skip_separators(&mut self) {
            while self.pos < self.bytes.len()
                && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r' | b',')
            {
                self.pos += 1;
            }
        }

        fn point(&mut self, base: Point) -> Result<Point, String> {
            let x = self.number()?;
            let y = self.number()?;
            Ok(Point::new(base.x + x, base.y + y))
        }

        fn number(&mut self) -> Result<f32, String> {
            self.skip_separators();
            let start = self.pos;
            if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'+' | b'-') {
                self.pos += 1;
            }
            let mut seen_digits = false;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
                seen_digits = true;
            }
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' {
                self.pos += 1;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                    self.pos += 1;
                    seen_digits = true;
                }
            }
            if seen_digits
                && self.pos < self.bytes.len()
                && matches!(self.bytes[self.pos], b'e' | b'E')
            {
                let mark = self.pos;
                self.pos += 1;
                if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'+' | b'-') {
                    self.pos += 1;
                }
                let exp_start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
                if self.pos == exp_start {
                    self.pos = mark; // a bare `e` belongs to whatever follows, not this number
                }
            }
            if !seen_digits {
                return Err(format!("expected a number at byte {start}"));
            }
            std::str::from_utf8(&self.bytes[start..self.pos])
                .ok()
                .and_then(|text| text.parse::<f32>().ok())
                .filter(|value| value.is_finite())
                .ok_or_else(|| format!("unparseable number at byte {start}"))
        }

        /// Arc flags are single `0`/`1` characters and may be unspaced from what follows
        /// (`a1 1 0 011 1` is legal SVG), so they cannot go through [`Self::number`].
        fn flag(&mut self) -> Result<bool, String> {
            self.skip_separators();
            match self.bytes.get(self.pos) {
                Some(b'0') => {
                    self.pos += 1;
                    Ok(false)
                }
                Some(b'1') => {
                    self.pos += 1;
                    Ok(true)
                }
                _ => Err(format!("expected an arc flag at byte {}", self.pos)),
            }
        }
    }

    fn reflect(control: Option<Point>, current: Point) -> Point {
        match control {
            Some(control) => Point::new(2.0 * current.x - control.x, 2.0 * current.y - control.y),
            // No previous curve to reflect: the control coincides with the current point
            // (SVG 2 §9.5.2), degrading S/T to a plain curve start.
            None => current,
        }
    }

    /// Endpoint-parameterized arc → center parameterization → cubic segments of at most 90°
    /// each (the standard SVG implementation-notes conversion, F.6). Degenerate radii draw a
    /// straight line, per spec.
    #[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    // Exact float compares are the spec's own degeneracy tests (F.6.2), and the rx/ry pairs
    // are the spec's own names.
    #[allow(clippy::float_cmp, clippy::similar_names)]
    fn arc_to_cubics(
        from: Point,
        to: Point,
        mut rx: f32,
        mut ry: f32,
        rotation_deg: f32,
        large_arc: bool,
        sweep: bool,
        out: &mut Vec<PathCommand>,
    ) {
        if rx == 0.0 || ry == 0.0 || (from.x == to.x && from.y == to.y) {
            out.push(PathCommand::LineTo(to));
            return;
        }
        let phi = rotation_deg.to_radians();
        let (sin_phi, cos_phi) = phi.sin_cos();

        // F.6.5.1: midpoint-relative coordinates in the ellipse's rotated frame.
        let dx = f64::from(from.x - to.x) / 2.0;
        let dy = f64::from(from.y - to.y) / 2.0;
        let x1p = f64::from(cos_phi) * dx + f64::from(sin_phi) * dy;
        let y1p = -f64::from(sin_phi) * dx + f64::from(cos_phi) * dy;

        // F.6.6: scale radii up when the endpoints cannot be reached.
        let mut rx64 = f64::from(rx);
        let mut ry64 = f64::from(ry);
        let lambda = x1p * x1p / (rx64 * rx64) + y1p * y1p / (ry64 * ry64);
        if lambda > 1.0 {
            let scale = lambda.sqrt();
            rx64 *= scale;
            ry64 *= scale;
            rx = rx64 as f32;
            ry = ry64 as f32;
        }
        let _ = (rx, ry);

        // F.6.5.2: center in the rotated frame.
        let num = (rx64 * rx64 * ry64 * ry64 - rx64 * rx64 * y1p * y1p - ry64 * ry64 * x1p * x1p)
            .max(0.0);
        let den = rx64 * rx64 * y1p * y1p + ry64 * ry64 * x1p * x1p;
        let mut coefficient = if den == 0.0 { 0.0 } else { (num / den).sqrt() };
        if large_arc == sweep {
            coefficient = -coefficient;
        }
        let cxp = coefficient * rx64 * y1p / ry64;
        let cyp = -coefficient * ry64 * x1p / rx64;

        // F.6.5.3: center in user space.
        let mx = f64::from(from.x + to.x) / 2.0;
        let my = f64::from(from.y + to.y) / 2.0;
        let cx = f64::from(cos_phi) * cxp - f64::from(sin_phi) * cyp + mx;
        let cy = f64::from(sin_phi) * cxp + f64::from(cos_phi) * cyp + my;

        // F.6.5.4–6: start angle and sweep extent.
        let angle = |ux: f64, uy: f64, vx: f64, vy: f64| -> f64 {
            let dot = ux * vx + uy * vy;
            let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
            let mut a = (dot / len).clamp(-1.0, 1.0).acos();
            if ux * vy - uy * vx < 0.0 {
                a = -a;
            }
            a
        };
        let start = angle(1.0, 0.0, (x1p - cxp) / rx64, (y1p - cyp) / ry64);
        let mut delta = angle(
            (x1p - cxp) / rx64,
            (y1p - cyp) / ry64,
            (-x1p - cxp) / rx64,
            (-y1p - cyp) / ry64,
        ) % (2.0 * std::f64::consts::PI);
        if !sweep && delta > 0.0 {
            delta -= 2.0 * std::f64::consts::PI;
        } else if sweep && delta < 0.0 {
            delta += 2.0 * std::f64::consts::PI;
        }

        // Split into <= 90° segments, each approximated by one cubic.
        let segments = (delta.abs() / std::f64::consts::FRAC_PI_2).ceil().max(1.0) as usize;
        let step = delta / segments as f64;
        // The standard tangent-length factor for a cubic approximating an arc of `step`.
        let alpha = 4.0 / 3.0 * (step / 4.0).tan();
        let point_at = |theta: f64| -> (f64, f64, f64, f64) {
            let (sin_t, cos_t) = theta.sin_cos();
            let x = cx + f64::from(cos_phi) * rx64 * cos_t - f64::from(sin_phi) * ry64 * sin_t;
            let y = cy + f64::from(sin_phi) * rx64 * cos_t + f64::from(cos_phi) * ry64 * sin_t;
            // The derivative (unnormalized tangent) in user space.
            let dx = -f64::from(cos_phi) * rx64 * sin_t - f64::from(sin_phi) * ry64 * cos_t;
            let dy = -f64::from(sin_phi) * rx64 * sin_t + f64::from(cos_phi) * ry64 * cos_t;
            (x, y, dx, dy)
        };
        let mut theta = start;
        let (mut x0, mut y0, mut dx0, mut dy0) = point_at(theta);
        // The conversion's own start should coincide with `from`; drawing uses `from` itself.
        let _ = (x0, y0);
        (x0, y0) = (f64::from(from.x), f64::from(from.y));
        for segment in 0..segments {
            let theta_next = theta + step;
            let (x1, y1, dx1, dy1) = point_at(theta_next);
            // The final endpoint is pinned to `to` so accumulated error never leaves a gap.
            let (x1, y1) = if segment == segments - 1 {
                (f64::from(to.x), f64::from(to.y))
            } else {
                (x1, y1)
            };
            out.push(PathCommand::Cubic {
                c1: Point::new((x0 + alpha * dx0) as f32, (y0 + alpha * dy0) as f32),
                c2: Point::new((x1 - alpha * dx1) as f32, (y1 - alpha * dy1) as f32),
                to: Point::new(x1 as f32, y1 as f32),
            });
            theta = theta_next;
            (x0, y0, dx0, dy0) = (x1, y1, dx1, dy1);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Tween evaluation (pure — everything here is a function of elapsed seconds)

fn ease(ease: Ease, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match ease {
        Ease::Linear => t,
        Ease::In => t * t * t,
        Ease::Out => 1.0 - (1.0 - t).powi(3),
        Ease::InOut => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
    }
}

fn lerp(from: TweenValue, to: TweenValue, t: f32) -> TweenValue {
    match (from, to) {
        (TweenValue::Number(a), TweenValue::Number(b)) => TweenValue::Number(a + (b - a) * t),
        (TweenValue::Color(a), TweenValue::Color(b)) => TweenValue::Color(Color {
            // Linear component lerp (including alpha); Oklab is the recorded upgrade if
            // cross-hue fades ever look muddy.
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: a.a + (b.a - a.a) * t,
        }),
        // Mixed endpoint kinds cannot parse (both endpoints go through one parser), so
        // holding `to` is an unreachable-in-practice fallback, not a semantics choice.
        (_, to) => to,
    }
}

/// The tween's value at `elapsed_s` seconds since its clock started, plus whether it has
/// finished (an infinite repeat never finishes).
#[allow(clippy::similar_names)] // elapsed_s (seconds in) vs elapsed_ms (millis local) is the point
pub(crate) fn tween_at(tween: &Tween, elapsed_s: f64) -> (TweenValue, bool) {
    let from = tween.from.unwrap_or(tween.to);
    let elapsed_ms = elapsed_s * 1000.0 - f64::from(tween.delay_ms);
    if elapsed_ms <= 0.0 {
        return (from, false);
    }
    let duration = f64::from(tween.duration_ms);
    if duration <= 0.0 {
        return (tween.to, !matches!(tween.repeat, Repeat::Infinite));
    }
    let (local, finished) = match tween.repeat {
        Repeat::Infinite => (elapsed_ms % duration, false),
        Repeat::Count(count) => {
            let total = duration * f64::from(count);
            if elapsed_ms >= total {
                (duration, true)
            } else {
                (elapsed_ms % duration, false)
            }
        }
    };
    #[allow(clippy::cast_possible_truncation)]
    let t = ease(tween.ease, (local / duration) as f32);
    (lerp(from, tween.to, t), finished)
}

/// Whether every tween in `spec` has finished by `elapsed_s`.
pub(crate) fn all_finished(spec: &[Tween], elapsed_s: f64) -> bool {
    spec.iter().all(|tween| tween_at(tween, elapsed_s).1)
}

// ---------------------------------------------------------------------------------------------
// Animation clocks

/// One animated record's clock: when its animation started (in the state's monotonic-seconds
/// timeline) and the spec it started for. Clock retention across scene generations is what
/// gives records their animation identity — and what tombstones completed transients: a
/// re-delivered identical record keeps its old clock, stays past its end, and stays skipped.
#[derive(Clone, Debug)]
pub(crate) struct ClockEntry {
    pub started_s: f64,
    pub spec: Vec<Tween>,
}

/// Reconcile the clock map against a newly accepted generation: an animated record whose key
/// and spec survive keeps its clock; a new or spec-changed record starts fresh at `now_s` (a
/// spec change is an intentional retrigger); keys absent from the generation are dropped.
pub(crate) fn reconcile_clocks(
    records: &[Record],
    clocks: &mut HashMap<String, ClockEntry>,
    now_s: f64,
) {
    let mut live = HashSet::new();
    reconcile_walk(records, clocks, now_s, &mut live, &mut 0);
    clocks.retain(|key, _| live.contains(key));
}

/// The clock key for the animated record at pre-order `index`: its `id`, or a positional
/// fallback for id-less records (positional keys restart on reorder, which is the documented
/// reason `animate` + `id` go together).
fn clock_key(record: &Record, index: usize) -> String {
    record
        .id
        .clone()
        .unwrap_or_else(|| format!("~{index}:{}", record.shape.kind()))
}

fn reconcile_walk(
    records: &[Record],
    clocks: &mut HashMap<String, ClockEntry>,
    now_s: f64,
    live: &mut HashSet<String>,
    index: &mut usize,
) {
    for record in records {
        let this = *index;
        *index += 1;
        if !record.animate.is_empty() {
            let key = clock_key(record, this);
            match clocks.get(&key) {
                Some(entry) if entry.spec == record.animate => {}
                _ => {
                    clocks.insert(
                        key.clone(),
                        ClockEntry {
                            started_s: now_s,
                            spec: record.animate.clone(),
                        },
                    );
                }
            }
            live.insert(key);
        }
        if let Shape::Group { children, .. } = &record.shape {
            reconcile_walk(children, clocks, now_s, live, index);
        }
    }
}

/// Whether any clock still has unfinished tweens at `now_s` — the "keep requesting redraws"
/// predicate.
pub(crate) fn any_animation_live(clocks: &HashMap<String, ClockEntry>, now_s: f64) -> bool {
    clocks
        .values()
        .any(|entry| !all_finished(&entry.spec, now_s - entry.started_s))
}

// ---------------------------------------------------------------------------------------------
// Resolving animated records (per frame)

/// The record as it should draw at `now_s`: `None` when it is a completed transient (visual
/// disposal — the scene value is never mutated, the record is simply not drawn), otherwise
/// the record with its animated fields resolved.
fn resolve_record<'a>(
    record: &'a Record,
    index: usize,
    clocks: &HashMap<String, ClockEntry>,
    now_s: f64,
) -> Option<std::borrow::Cow<'a, Record>> {
    if record.animate.is_empty() {
        return Some(std::borrow::Cow::Borrowed(record));
    }
    let key = clock_key(record, index);
    let Some(entry) = clocks.get(&key) else {
        // No clock yet (first frame before reconcile): draw the unanimated base.
        return Some(std::borrow::Cow::Borrowed(record));
    };
    let elapsed = now_s - entry.started_s;
    if record.transient && all_finished(&entry.spec, elapsed) {
        return None;
    }
    let mut resolved = record.clone();
    for tween in &entry.spec {
        let (value, _) = tween_at(tween, elapsed);
        apply_field(&mut resolved, &tween.field, value);
    }
    Some(std::borrow::Cow::Owned(resolved))
}

fn apply_field(record: &mut Record, field: &str, value: TweenValue) {
    match value {
        TweenValue::Color(color) => match field {
            "fill" => record.fill = Some(Paint::Solid(color)),
            "stroke" => match &mut record.stroke {
                Some(stroke) => stroke.paint = Paint::Solid(color),
                None => {
                    record.stroke = Some(StrokeSpec {
                        paint: Paint::Solid(color),
                        width: 1.0,
                        dash: Vec::new(),
                    });
                }
            },
            "color" => {
                if let Shape::Text(text) = &mut record.shape {
                    text.color = color;
                }
            }
            _ => {}
        },
        TweenValue::Number(number) => match field {
            "opacity" => record.opacity = number.clamp(0.0, 1.0),
            "stroke_width" => {
                if let Some(stroke) = &mut record.stroke {
                    stroke.width = number;
                }
            }
            _ => apply_numeric_shape_field(&mut record.shape, field, number),
        },
    }
}

fn apply_numeric_shape_field(shape: &mut Shape, field: &str, value: f32) {
    match shape {
        Shape::Rect {
            x,
            y,
            width,
            height,
            rx,
        } => match field {
            "x" => *x = value,
            "y" => *y = value,
            "width" => *width = value,
            "height" => *height = value,
            "rx" => *rx = value,
            _ => {}
        },
        Shape::Circle { cx, cy, r } => match field {
            "cx" => *cx = value,
            "cy" => *cy = value,
            "r" => *r = value,
            _ => {}
        },
        Shape::Ellipse { cx, cy, rx, ry } => match field {
            "cx" => *cx = value,
            "cy" => *cy = value,
            "rx" => *rx = value,
            "ry" => *ry = value,
            _ => {}
        },
        Shape::Line { x1, y1, x2, y2 } => match field {
            "x1" => *x1 = value,
            "y1" => *y1 = value,
            "x2" => *x2 = value,
            "y2" => *y2 = value,
            _ => {}
        },
        Shape::Text(text) => match field {
            "x" => text.x = value,
            "y" => text.y = value,
            "size" => text.size = value,
            _ => {}
        },
        Shape::Image(image) => match field {
            "x" => image.x = value,
            "y" => image.y = value,
            "width" => image.width = value,
            "height" => image.height = value,
            "rotate" => image.rotate_deg = value,
            _ => {}
        },
        Shape::Group { transform, .. } => match field {
            "translate_x" => transform.translate.x = value,
            "translate_y" => transform.translate.y = value,
            "rotate" => transform.rotate_deg = value,
            "scale" => transform.scale = Vector::new(value, value),
            _ => {}
        },
        Shape::Polyline { .. } | Shape::Polygon { .. } | Shape::Path { .. } => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing

fn paint_style(paint: &Paint, opacity: f32) -> canvas::Style {
    match paint {
        Paint::Solid(color) => canvas::Style::Solid(scaled(*color, opacity)),
        Paint::Gradient { start, end, stops } => {
            let mut linear = canvas::gradient::Linear::new(*start, *end);
            for (offset, color) in stops {
                linear = linear.add_stop(*offset, scaled(*color, opacity));
            }
            canvas::Style::Gradient(canvas::Gradient::Linear(linear))
        }
    }
}

fn scaled(color: Color, opacity: f32) -> Color {
    if opacity >= 1.0 {
        color
    } else {
        Color {
            a: color.a * opacity,
            ..color
        }
    }
}

fn shape_path(shape: &Shape) -> Option<canvas::Path> {
    let path = match shape {
        Shape::Rect {
            x,
            y,
            width,
            height,
            rx,
        } => canvas::Path::new(|builder| {
            if *rx > 0.0 {
                builder.rounded_rectangle(
                    Point::new(*x, *y),
                    Size::new(*width, *height),
                    (*rx).into(),
                );
            } else {
                builder.rectangle(Point::new(*x, *y), Size::new(*width, *height));
            }
        }),
        Shape::Circle { cx, cy, r } => canvas::Path::circle(Point::new(*cx, *cy), *r),
        Shape::Ellipse { cx, cy, rx, ry } => canvas::Path::new(|builder| {
            builder.ellipse(canvas::path::arc::Elliptical {
                center: Point::new(*cx, *cy),
                radii: Vector::new(*rx, *ry),
                rotation: Radians(0.0),
                start_angle: Radians(0.0),
                end_angle: Radians(2.0 * std::f32::consts::PI),
            });
        }),
        Shape::Line { x1, y1, x2, y2 } => {
            canvas::Path::line(Point::new(*x1, *y1), Point::new(*x2, *y2))
        }
        Shape::Polyline { points } | Shape::Polygon { points } => {
            if points.is_empty() {
                return None;
            }
            let close = matches!(shape, Shape::Polygon { .. });
            canvas::Path::new(|builder| {
                builder.move_to(points[0]);
                for point in &points[1..] {
                    builder.line_to(*point);
                }
                if close {
                    builder.close();
                }
            })
        }
        Shape::Path { commands } => canvas::Path::new(|builder| {
            for command in commands {
                match command {
                    PathCommand::MoveTo(to) => builder.move_to(*to),
                    PathCommand::LineTo(to) => builder.line_to(*to),
                    PathCommand::Quad { control, to } => {
                        builder.quadratic_curve_to(*control, *to);
                    }
                    PathCommand::Cubic { c1, c2, to } => {
                        builder.bezier_curve_to(*c1, *c2, *to);
                    }
                    PathCommand::Close => builder.close(),
                }
            }
        }),
        Shape::Text(_) | Shape::Image(_) | Shape::Group { .. } => return None,
    };
    Some(path)
}

/// The rectangle an image's pixels actually occupy inside its record box, per `fit`.
/// `Fill` is the box itself (a shape stretches to its geometry); the rest scale the pixel
/// size into the box with `iced::ContentFit` semantics and center the result. A `Cover`
/// overflow deliberately spills the box — the canvas widget's container clips at the
/// widget bounds, matching how oversized geometry already behaves.
fn fitted_image_bounds(image: &ImageSpec, pixel_w: f32, pixel_h: f32) -> Rectangle {
    let box_size = Size::new(image.width, image.height);
    let content_fit = match image.fit {
        ImageFit::Fill => return Rectangle::new(Point::new(image.x, image.y), box_size),
        ImageFit::Contain => iced::ContentFit::Contain,
        ImageFit::Cover => iced::ContentFit::Cover,
        ImageFit::None => iced::ContentFit::None,
        ImageFit::ScaleDown => iced::ContentFit::ScaleDown,
    };
    let fitted = content_fit.fit(Size::new(pixel_w, pixel_h), box_size);
    Rectangle::new(
        Point::new(
            image.x + (box_size.width - fitted.width) / 2.0,
            image.y + (box_size.height - fitted.height) / 2.0,
        ),
        fitted,
    )
}

fn draw_records(
    frame: &mut canvas::Frame,
    records: &[Record],
    clocks: &HashMap<String, ClockEntry>,
    now_s: f64,
    index: &mut usize,
) {
    for record in records {
        let this = *index;
        *index += 1;
        let Some(resolved) = resolve_record(record, this, clocks, now_s) else {
            // A completed transient still owns its pre-order index range.
            if let Shape::Group { children, .. } = &record.shape {
                *index += count_records(children);
            }
            continue;
        };
        draw_record(frame, &resolved, clocks, now_s, index);
    }
}

fn count_records(records: &[Record]) -> usize {
    records
        .iter()
        .map(|record| {
            1 + match &record.shape {
                Shape::Group { children, .. } => count_records(children),
                _ => 0,
            }
        })
        .sum()
}

// The exact compares pick the cheaper uniform-scale call for untouched defaults; both
// branches are correct for any value.
#[allow(clippy::float_cmp)]
fn draw_record(
    frame: &mut canvas::Frame,
    record: &Record,
    clocks: &HashMap<String, ClockEntry>,
    now_s: f64,
    index: &mut usize,
) {
    match &record.shape {
        Shape::Group {
            transform,
            children,
        } => {
            frame.with_save(|frame| {
                frame.translate(transform.translate);
                frame.rotate(Radians(transform.rotate_deg.to_radians()));
                if transform.scale.x == transform.scale.y {
                    if transform.scale.x != 1.0 {
                        frame.scale(transform.scale.x);
                    }
                } else {
                    frame.scale_nonuniform(transform.scale);
                }
                draw_records(frame, children, clocks, now_s, index);
            });
        }
        Shape::Image(image) => {
            let Some(slot) = &image.slot else { return };
            // One lock-free state read. Recency is stamped here AND by `update()`'s
            // refresh walk (this closure only re-runs when the geometry cache re-records,
            // so the walk is what keeps a displayed-but-cached image LRU-hot).
            // Loading/Failed draw nothing (the scene's other records stand).
            if let EntryState::Ready {
                handle,
                width: pixel_w,
                height: pixel_h,
                ..
            } = &*slot.state()
            {
                // Caveats (documented in the .d.ts, mirroring CanvasText): images paint
                // above ALL fill/stroke geometry and below text (fixed wgpu layer order),
                // and a rotated image inside a non-uniformly-scaled group won't shear (the
                // frame transform reduces to a rect + rotation).
                #[allow(clippy::cast_precision_loss)]
                let bounds = fitted_image_bounds(image, *pixel_w as f32, *pixel_h as f32);
                frame.draw_image(
                    bounds,
                    canvas::Image::new(handle.clone())
                        .filter_method(if image.nearest {
                            iced::widget::image::FilterMethod::Nearest
                        } else {
                            iced::widget::image::FilterMethod::Linear
                        })
                        .rotation(Radians(image.rotate_deg.to_radians()))
                        // The alpha-scaled-paint trick other shapes use doesn't apply to
                        // textures; record opacity routes into the image itself.
                        .opacity(record.opacity),
                );
            }
        }
        Shape::Text(text) => {
            frame.fill_text(canvas::Text {
                content: text.content.clone(),
                position: Point::new(text.x, text.y),
                color: scaled(text.color, record.opacity),
                size: text.size.into(),
                font: if text.monospace {
                    iced::Font::MONOSPACE
                } else {
                    iced::Font::default()
                },
                align_x: match text.align_x {
                    TextAlignX::Left => iced::advanced::text::Alignment::Left,
                    TextAlignX::Center => iced::advanced::text::Alignment::Center,
                    TextAlignX::Right => iced::advanced::text::Alignment::Right,
                },
                align_y: match text.align_y {
                    TextAlignY::Top => iced::alignment::Vertical::Top,
                    TextAlignY::Center => iced::alignment::Vertical::Center,
                    TextAlignY::Bottom => iced::alignment::Vertical::Bottom,
                },
                ..canvas::Text::default()
            });
        }
        shape => {
            let Some(path) = shape_path(shape) else {
                return;
            };
            if let Some(paint) = &record.fill {
                frame.fill(
                    &path,
                    canvas::Fill {
                        style: paint_style(paint, record.opacity),
                        ..canvas::Fill::default()
                    },
                );
            }
            if let Some(stroke) = &record.stroke {
                frame.stroke(
                    &path,
                    canvas::Stroke {
                        style: paint_style(&stroke.paint, record.opacity),
                        width: stroke.width,
                        line_dash: canvas::LineDash {
                            segments: &stroke.dash,
                            offset: 0,
                        },
                        ..canvas::Stroke::default()
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The canvas program

/// Where a program's scene comes from. `Bound` re-reads its cell and re-parses only when the
/// snapshot pointer changes (the memoized bound parse); the memo also pins the last *accepted*
/// generation so a rejected write leaves the prior scene on screen.
#[derive(Clone)]
pub(crate) enum SceneSource {
    Static(Arc<ParsedScene>),
    Bound {
        cell: Arc<StoreBindingCell>,
        memo: Arc<Mutex<SceneMemo>>,
        /// The scene drawn while the bound path is absent/null: the binding token's
        /// `fallback`, parsed once at build (empty when none was given).
        fallback: Arc<ParsedScene>,
        /// The image-resolution context live values re-parse with (bound provenance —
        /// descend-only srcs, no absolute paths — already baked in). `None` when the
        /// canvas has no registered creator or store: image records skip with a warning.
        image_ctx: Option<Arc<SceneImageCtx>>,
    },
}

pub(crate) struct SceneMemo {
    /// `Arc::as_ptr` of the last snapshot examined (accepted or rejected).
    last_seen: usize,
    parsed: Arc<ParsedScene>,
}

impl Default for SceneMemo {
    fn default() -> Self {
        Self {
            last_seen: 0,
            parsed: Arc::new(ParsedScene::default()),
        }
    }
}

/// The pointer-event callback: the creating isolate's `onPointer` function plus its routing
/// token, exactly the Button `onPress` shape.
#[derive(Clone)]
pub(crate) struct PointerHandler {
    pub callback: crate::WidgetCallback,
    pub isolate: WidgetIsolate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerButton {
    Left,
    Right,
    Middle,
}

impl PointerButton {
    fn from_iced(button: mouse::Button) -> Option<Self> {
        match button {
            mouse::Button::Left => Some(Self::Left),
            mouse::Button::Right => Some(Self::Right),
            mouse::Button::Middle => Some(Self::Middle),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }
}

/// How a `view_box` maps onto the widget bounds. `Fill` (the default) is the exact
/// rect-to-bounds mapping — non-uniform when aspects differ. `Contain` scales uniformly to
/// the limiting axis and centers, preserving the scene's aspect ratio with empty margins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ViewFit {
    #[default]
    Fill,
    Contain,
}

#[derive(Clone)]
pub(crate) struct SceneProgram {
    pub scene: SceneSource,
    pub started: Option<Instant>,
    pub view_box: Option<Rectangle>,
    pub fit: ViewFit,
    pub on_pointer: Option<PointerHandler>,
    /// The global image store, for the completion-generation check in `update()` (a bare
    /// relaxed atomic read per redraw, and only for scenes that contain image records).
    /// `None` in headless runtimes.
    pub image_store: Option<ImageStore>,
}

/// Per-widget-instance UI-side state, living in iced's widget `Tree` (the one place that is
/// created, used, and dropped entirely on the UI thread — the render closure itself is built
/// on the script thread and must not own thread-affine state).
pub(crate) struct CanvasState {
    cache: canvas::Cache,
    clocks: HashMap<String, ClockEntry>,
    /// The `Arc::as_ptr` of the generation the clocks were last reconciled against.
    reconciled: usize,
    /// The image store's completion generation this state last drew with. Only compared
    /// for scenes that actually contain image records — a completion anywhere in the
    /// process bumps the (global) counter, and image-free canvases must not clear their
    /// cached geometry for it.
    image_generation: u64,
    /// Monotonic-seconds timeline: `epoch` is the first instant this state ever saw, `now_s`
    /// the seconds since then as of the latest `RedrawRequested`.
    epoch: Option<Instant>,
    now_s: f64,
    pressed: Option<PointerButton>,
    /// Last cursor position in widget-local coordinates (tracked while pressed, so a release
    /// outside the bounds still reports where it happened).
    last_local: Point,
    moved_this_frame: bool,
    pending_move: Option<Point>,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            cache: canvas::Cache::new(),
            clocks: HashMap::new(),
            reconciled: 0,
            image_generation: 0,
            epoch: None,
            now_s: 0.0,
            pressed: None,
            last_local: Point::ORIGIN,
            moved_this_frame: false,
            pending_move: None,
        }
    }
}

impl CanvasState {
    fn tick(&mut self, now: Instant) {
        let epoch = *self.epoch.get_or_insert(now);
        self.now_s = now.duration_since(epoch).as_secs_f64();
    }
}

impl SceneProgram {
    /// The current accepted generation. For a bound scene this is where the memoized parse
    /// happens: a changed snapshot pointer re-parses; a rejected generation logs once and
    /// keeps the previous scene.
    fn current(&self) -> Arc<ParsedScene> {
        match &self.scene {
            SceneSource::Static(parsed) => parsed.clone(),
            SceneSource::Bound {
                cell,
                memo,
                fallback,
                image_ctx,
            } => {
                let loaded = cell.load();
                if loaded.is_null() {
                    // An absent path is the binding's fallback scene, not an author error.
                    return fallback.clone();
                }
                let ptr = Arc::as_ptr(&loaded) as usize;
                let mut memo = memo.lock().expect("scene memo poisoned");
                if memo.last_seen != ptr {
                    memo.last_seen = ptr;
                    let mut images = image_ctx.as_deref().map(SceneImages::Live);
                    match parse_scene(&loaded, images.as_mut()) {
                        Ok(parsed) => {
                            log_warnings(&parsed);
                            memo.parsed = Arc::new(parsed);
                        }
                        Err(reject) => {
                            log::warn!(
                                "smudgy canvas: scene generation rejected ({reject}); keeping the previous scene"
                            );
                        }
                    }
                }
                memo.parsed.clone()
            }
        }
    }

    /// The `view_box` mapping at `bounds`: the box, the per-axis scale, and the pixel-space
    /// letterbox offset (zero offset and independent scales for `Fill`; uniform scale,
    /// centered, for `Contain`).
    fn view_mapping(&self, bounds: Size) -> Option<(Rectangle, Vector, Vector)> {
        let view_box = self.view_box?;
        let sx = bounds.width / view_box.width.max(f32::EPSILON);
        let sy = bounds.height / view_box.height.max(f32::EPSILON);
        Some(match self.fit {
            ViewFit::Fill => (view_box, Vector::new(sx, sy), Vector::new(0.0, 0.0)),
            ViewFit::Contain => {
                let s = sx.min(sy);
                (
                    view_box,
                    Vector::new(s, s),
                    Vector::new(
                        (bounds.width - view_box.width * s) / 2.0,
                        (bounds.height - view_box.height * s) / 2.0,
                    ),
                )
            }
        })
    }

    /// Widget-local coordinates → scene coordinates (the `view_box` inverse mapping; a
    /// point in a `Contain` margin maps outside the box, which is well-defined for
    /// hit-testing).
    fn to_scene(&self, local: Point, bounds: Size) -> Point {
        match self.view_mapping(bounds) {
            None => local,
            Some((view_box, scale, offset)) => Point::new(
                view_box.x + (local.x - offset.x) / scale.x.max(f32::EPSILON),
                view_box.y + (local.y - offset.y) / scale.y.max(f32::EPSILON),
            ),
        }
    }

    fn pointer_message(
        &self,
        kind: &str,
        local: Point,
        bounds: Size,
        button: PointerButton,
    ) -> Option<WidgetMessage> {
        let handler = self.on_pointer.as_ref()?;
        let scene = self.to_scene(local, bounds);
        Some(WidgetMessage::InvokeCallback {
            callback: handler.callback.clone(),
            isolate: handler.isolate.clone(),
            args: vec![format!(
                r#"{{"kind":"{kind}","x":{x},"y":{y},"button":"{button}"}}"#,
                x = f64::from(scene.x),
                y = f64::from(scene.y),
                button = button.as_str(),
            )],
        })
    }
}

pub(crate) fn log_warnings(parsed: &ParsedScene) {
    for warning in &parsed.warnings {
        log::warn!("smudgy canvas: {warning}");
    }
}

impl canvas::Program<WidgetMessage, smudgy_theme::Theme> for SceneProgram {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut CanvasState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<WidgetMessage>> {
        match event {
            iced::Event::Window(window::Event::RedrawRequested(now)) => {
                let first = state.epoch.is_none();
                if first {
                    state.epoch = self.started;
                }
                state.tick(*now);
                let parsed = self.current();
                let generation = Arc::as_ptr(&parsed) as usize;
                if state.reconciled != generation {
                    state.reconciled = generation;
                    state.cache.clear();
                    reconcile_clocks(
                        &parsed.records,
                        &mut state.clocks,
                        if first && self.started.is_some() {
                            0.0
                        } else {
                            state.now_s
                        },
                    );
                }
                // An image load landing anywhere bumps the store's completion generation;
                // a scene that draws images must re-record its cached geometry to show it
                // (plan D7: the check lives HERE — `draw` sees an immutable state). The
                // store's poke already scheduled this redraw. Image-free scenes skip all
                // of this, so unrelated loads never clear their cache. The refresh walk
                // keeps displayed slots LRU-hot (cached geometry means `draw` doesn't
                // read them per frame) and revives evicted/flushed cells — ≤256 lock-free
                // loads + relaxed stores per redraw, re-ensure only after an eviction.
                if !parsed.image_cells.is_empty()
                    && let Some(store) = &self.image_store
                {
                    for slot in &parsed.image_cells {
                        slot.refresh(store);
                    }
                    let image_generation = store.completion_generation();
                    if state.image_generation != image_generation {
                        state.image_generation = image_generation;
                        state.cache.clear();
                    }
                }
                state.moved_this_frame = false;
                // A coalesced drag position publishes now (one per frame); its publish
                // schedules the next redraw, which also keeps any animation ticking.
                if let Some(local) = state.pending_move.take()
                    && let Some(button) = state.pressed
                    && let Some(message) =
                        self.pointer_message("move", local, bounds.size(), button)
                {
                    return Some(canvas::Action::publish(message));
                }
                if any_animation_live(&state.clocks, state.now_s) {
                    return Some(canvas::Action::request_redraw());
                }
                None
            }
            iced::Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                // One button owns the interaction until its release: a chorded second press
                // is ignored outright (it must neither steal the tracking slot nor publish
                // an extra `down`), keeping the stream strictly down(X) -> moves -> up(X).
                if state.pressed.is_some() {
                    return None;
                }
                let button = PointerButton::from_iced(*button)?;
                let local = cursor.position_in(bounds)?;
                self.on_pointer.as_ref()?;
                state.pressed = Some(button);
                state.last_local = local;
                let message = self.pointer_message("down", local, bounds.size(), button)?;
                Some(canvas::Action::publish(message).and_capture())
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let button = state.pressed?;
                // Track through the drag even outside the bounds (press captures).
                let position = cursor.position()?;
                let local = Point::new(position.x - bounds.x, position.y - bounds.y);
                state.last_local = local;
                if state.moved_this_frame {
                    state.pending_move = Some(local);
                    return None;
                }
                state.moved_this_frame = true;
                let message = self.pointer_message("move", local, bounds.size(), button)?;
                Some(canvas::Action::publish(message).and_capture())
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let released = PointerButton::from_iced(*button)?;
                if state.pressed != Some(released) {
                    return None;
                }
                state.pressed = None;
                state.pending_move = None;
                let local = cursor
                    .position_in(bounds)
                    .or_else(|| {
                        cursor
                            .position()
                            .map(|p| Point::new(p.x - bounds.x, p.y - bounds.y))
                    })
                    .unwrap_or(state.last_local);
                let message = self.pointer_message("up", local, bounds.size(), released)?;
                Some(canvas::Action::publish(message).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &CanvasState,
        renderer: &iced::Renderer,
        _theme: &smudgy_theme::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let parsed = self.current();
        let draw_all = |frame: &mut canvas::Frame| {
            if let Some((view_box, scale, offset)) = self.view_mapping(bounds.size()) {
                frame.translate(offset);
                frame.scale_nonuniform(scale);
                frame.translate(Vector::new(-view_box.x, -view_box.y));
            }
            draw_records(frame, &parsed.records, &state.clocks, state.now_s, &mut 0);
        };
        // The two span names separate the cached fast path (near-zero except on
        // invalidation) from the per-frame re-tessellation an animating scene pays.
        if parsed.animated == 0 {
            iced_debug::time_with("canvas draw (cached)", || {
                vec![state.cache.draw(renderer, bounds.size(), draw_all)]
            })
        } else {
            // Ordered, uncached: the whole scene draws fresh so animated records paint in
            // their scene position, never hoisted above later static ones.
            iced_debug::time_with("canvas draw (animated)", || {
                let mut frame = canvas::Frame::new(renderer, bounds.size());
                draw_all(&mut frame);
                vec![frame.into_geometry()]
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(value: serde_json::Value) -> Node {
        Node::from(value)
    }

    fn parse(value: serde_json::Value) -> Result<ParsedScene, SceneReject> {
        parse_scene(&node(value), None)
    }

    /// An image context over the shared test store: a trusted user creator whose modules
    /// root is `/m` (relative srcs resolve under it; the store's mock fetcher never
    /// touches the disk).
    fn image_ctx(bound: bool) -> SceneImageCtx {
        let (store, _) = crate::image_store::tests::test_store();
        let policy = crate::image_store::tests::test_policy();
        let creator =
            smudgy_cloud::image_source::register_creator(r#"{"kind":"user"}"#, None, policy)
                .expect("user creator registers on a trusted policy");
        SceneImageCtx {
            creator: Arc::new(creator),
            store,
            bound,
        }
    }

    fn parse_with_images(
        value: serde_json::Value,
        bound: bool,
    ) -> Result<ParsedScene, SceneReject> {
        let ctx = image_ctx(bound);
        let mut images = SceneImages::Live(&ctx);
        parse_scene(&node(value), Some(&mut images))
    }

    fn accepted(value: serde_json::Value) -> ParsedScene {
        parse(value).expect("scene should be accepted")
    }

    // ---- scene grammar --------------------------------------------------------------------

    #[test]
    fn parses_every_shape_kind() {
        let scene = parse_with_images(
            json!([
                { "kind": "rect", "x": 1, "y": 2, "width": 3, "height": 4, "rx": 1, "fill": "#ff0000" },
                { "kind": "circle", "cx": 5, "cy": 6, "r": 7, "stroke": { "color": "#00ff00", "width": 2 } },
                { "kind": "ellipse", "cx": 1, "cy": 1, "rx": 2, "ry": 3 },
                { "kind": "line", "x1": 0, "y1": 0, "x2": 10, "y2": 10, "stroke": {} },
                { "kind": "polyline", "points": [[0, 0], [5, 5], [10, 0]], "stroke": {} },
                { "kind": "polygon", "points": [[0, 0], [5, 5], [10, 0]], "fill": "red" },
                { "kind": "path", "d": "M 0 0 L 10 10 Z", "stroke": {} },
                { "kind": "text", "x": 4, "y": 8, "text": "HP", "size": 12, "color": "white" },
                { "kind": "image", "src": "map-bg.png", "x": 0, "y": 0, "width": 480, "height": 320,
                  "fit": "cover", "filter": "nearest", "rotate": 15 },
                { "kind": "group", "transform": { "translate": [5, 5] }, "children": [
                    { "kind": "rect", "width": 1, "height": 1 },
                ] },
            ]),
            false,
        )
        .expect("scene should be accepted");
        assert_eq!(scene.records.len(), 10);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert_eq!(scene.animated, 0);
        assert_eq!(scene.image_cells.len(), 1);
        let Shape::Image(image) = &scene.records[8].shape else {
            panic!("expected image");
        };
        assert_eq!(
            (image.fit, image.nearest, image.rotate_deg),
            (ImageFit::Cover, true, 15.0)
        );
        assert!(image.slot.is_some(), "src resolved and ensured at parse");
        let Shape::Rect { x, rx, .. } = &scene.records[0].shape else {
            panic!("expected rect");
        };
        assert_eq!((*x, *rx), (1.0, 1.0));
        assert_eq!(
            scene.records[0].fill,
            Some(Paint::Solid(Color::from_rgb(1.0, 0.0, 0.0)))
        );
        let Shape::Group { children, .. } = &scene.records[9].shape else {
            panic!("expected group");
        };
        assert_eq!(children.len(), 1);
    }

    #[test]
    fn unknown_kind_and_bad_color_skip_the_record_not_the_scene() {
        let scene = accepted(json!([
            { "kind": "blob", "x": 1 },
            { "kind": "rect", "width": 1, "height": 1, "fill": "not-a-color" },
            { "kind": "rect", "width": 2, "height": 2, "fill": "#0000ff" },
        ]));
        assert_eq!(scene.records.len(), 1, "only the valid record survives");
        assert_eq!(scene.warnings.len(), 2);
    }

    #[test]
    fn non_array_scene_is_rejected() {
        assert_eq!(
            parse(json!({ "kind": "rect" })),
            Err(SceneReject::NotAnArray)
        );
        assert_eq!(parse(json!(null)), Err(SceneReject::NotAnArray));
    }

    #[test]
    fn gradient_fill_parses_and_respects_the_stop_cap() {
        let scene = accepted(json!([
            { "kind": "rect", "width": 10, "height": 10,
              "fill": { "gradient": { "from": [4, 0], "to": [144, 0],
                                      "stops": [[0, "#7a1f1f"], [1, "#d64541"]] } } },
        ]));
        let Some(Paint::Gradient { start, end, stops }) = &scene.records[0].fill else {
            panic!("expected gradient fill");
        };
        assert_eq!(
            (*start, *end),
            (Point::new(4.0, 0.0), Point::new(144.0, 0.0))
        );
        assert_eq!(stops.len(), 2);

        let stops: Vec<_> = (0..=8)
            .map(|i| json!([f64::from(i) / 8.0, "#ffffff"]))
            .collect();
        assert_eq!(
            parse(json!([
                { "kind": "rect", "width": 1, "height": 1,
                  "fill": { "gradient": { "from": [0, 0], "to": [1, 0], "stops": stops } } },
            ])),
            Err(SceneReject::Budget("gradient-stops")),
            "a 9th stop rejects the generation (iced ignores it silently; we do not)"
        );
    }

    // ---- budgets: atomic rejection --------------------------------------------------------

    #[test]
    fn record_count_budget_rejects_atomically() {
        let records: Vec<_> = (0..=MAX_RECORDS)
            .map(|_| json!({ "kind": "rect", "width": 1, "height": 1 }))
            .collect();
        assert_eq!(
            parse(serde_json::Value::Array(records)),
            Err(SceneReject::Budget("record-count"))
        );
    }

    #[test]
    fn nesting_depth_budget_rejects() {
        let mut scene = json!({ "kind": "rect", "width": 1, "height": 1 });
        for _ in 0..=MAX_DEPTH {
            scene = json!({ "kind": "group", "children": [scene] });
        }
        assert_eq!(
            parse(json!([scene])),
            Err(SceneReject::Budget("nesting-depth"))
        );
    }

    #[test]
    fn text_budgets_reject() {
        let big = "x".repeat(MAX_TEXT_BYTES_PER_RECORD + 1);
        assert_eq!(
            parse(json!([{ "kind": "text", "text": big }])),
            Err(SceneReject::Budget("text-bytes"))
        );
    }

    #[test]
    fn duplicate_animation_ids_reject() {
        let ring = json!({
            "kind": "circle", "id": "ring", "cx": 0, "cy": 0, "r": 1,
            "animate": { "r": { "to": 10, "duration": 100 } },
        });
        assert_eq!(
            parse(json!([ring, ring])),
            Err(SceneReject::DuplicateId("ring".to_string()))
        );
    }

    #[test]
    fn animated_field_budget_rejects() {
        let records: Vec<_> = (0..=MAX_ANIMATED_FIELDS)
            .map(|i| {
                json!({
                    "kind": "rect", "id": format!("r{i}"), "width": 1, "height": 1,
                    "animate": { "x": { "to": 5, "duration": 100 } },
                })
            })
            .collect();
        assert_eq!(
            parse(serde_json::Value::Array(records)),
            Err(SceneReject::Budget("animated-fields"))
        );
    }

    // ---- animate specs --------------------------------------------------------------------

    #[test]
    fn animate_parses_with_base_value_from_default() {
        let scene = accepted(json!([
            { "kind": "circle", "id": "ring", "cx": 74, "cy": 24, "r": 6,
              "stroke": { "color": "#ff2222", "width": 1 },
              "animate": { "r": { "to": 400, "duration": 1500, "ease": "out" } },
              "transient": true },
        ]));
        assert_eq!(scene.animated, 1);
        let record = &scene.records[0];
        assert!(record.transient);
        let tween = &record.animate[0];
        assert_eq!(
            tween.from,
            Some(TweenValue::Number(6.0)),
            "from defaults to the static r"
        );
        assert_eq!(tween.to, TweenValue::Number(400.0));
        assert_eq!(tween.ease, Ease::Out);
        assert_eq!(tween.repeat, Repeat::Count(1));
    }

    #[test]
    fn animate_rejects_fields_foreign_to_the_kind() {
        let scene = accepted(json!([
            { "kind": "rect", "width": 1, "height": 1,
              "animate": { "r": { "to": 4, "duration": 100 } } },
        ]));
        assert!(scene.records.is_empty(), "the record is skipped");
        assert_eq!(scene.warnings.len(), 1);
    }

    #[test]
    fn color_tweens_parse_on_fill_stroke_and_text_color() {
        let scene = accepted(json!([
            { "kind": "rect", "width": 1, "height": 1, "fill": "#ffd54a00",
              "animate": { "fill": { "to": "#ffd54a33", "duration": 250 } } },
            { "kind": "text", "text": "Mora", "color": "#9a9a9a",
              "animate": { "color": { "to": "#ffffff", "duration": 250 } } },
        ]));
        assert_eq!(scene.animated, 2);
        let TweenValue::Color(from) = scene.records[0].animate[0].from.unwrap() else {
            panic!("expected a color from");
        };
        assert_eq!(from.a, 0.0, "from defaults to the record's own fill");
    }

    // ---- tween evaluation (injected clock: plain seconds) ---------------------------------

    fn number_tween(from: f32, to: f32, duration_ms: f32) -> Tween {
        Tween {
            field: "r".to_string(),
            from: Some(TweenValue::Number(from)),
            to: TweenValue::Number(to),
            duration_ms,
            delay_ms: 0.0,
            ease: Ease::Linear,
            repeat: Repeat::Count(1),
        }
    }

    #[test]
    fn tween_interpolates_and_finishes() {
        let tween = number_tween(0.0, 100.0, 1000.0);
        assert_eq!(tween_at(&tween, 0.0), (TweenValue::Number(0.0), false));
        assert_eq!(tween_at(&tween, 0.5), (TweenValue::Number(50.0), false));
        let (value, finished) = tween_at(&tween, 2.0);
        assert_eq!(value, TweenValue::Number(100.0));
        assert!(finished, "past the end holds `to` and reports finished");
    }

    #[test]
    fn delay_applies_once_before_the_first_repetition() {
        let tween = Tween {
            delay_ms: 500.0,
            repeat: Repeat::Count(2),
            ..number_tween(0.0, 10.0, 1000.0)
        };
        assert_eq!(tween_at(&tween, 0.25), (TweenValue::Number(0.0), false));
        assert_eq!(tween_at(&tween, 1.0), (TweenValue::Number(5.0), false));
        // Second repetition restarts from `from` (restart semantics, no ping-pong).
        let (TweenValue::Number(value), finished) = tween_at(&tween, 1.75) else {
            panic!("expected a number");
        };
        assert!(
            (value - 2.5).abs() < 1e-4,
            "restarted second run, got {value}"
        );
        assert!(!finished);
        assert!(tween_at(&tween, 2.6).1, "two repetitions + delay complete");
    }

    #[test]
    fn infinite_repeat_never_finishes() {
        let tween = Tween {
            repeat: Repeat::Infinite,
            ..number_tween(0.0, 10.0, 100.0)
        };
        assert!(!tween_at(&tween, 1e6).1);
        assert!(!all_finished(&[tween], 1e6));
    }

    #[test]
    fn easing_curves_hit_their_endpoints_and_shape() {
        for ease_kind in [Ease::Linear, Ease::In, Ease::Out, Ease::InOut] {
            assert_eq!(ease(ease_kind, 0.0), 0.0);
            assert_eq!(ease(ease_kind, 1.0), 1.0);
        }
        assert!(ease(Ease::In, 0.25) < 0.25, "ease-in starts slow");
        assert!(ease(Ease::Out, 0.25) > 0.25, "ease-out starts fast");
        assert!((ease(Ease::InOut, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn color_lerp_is_componentwise_including_alpha() {
        let tween = Tween {
            field: "fill".to_string(),
            from: Some(TweenValue::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.0))),
            to: TweenValue::Color(Color::from_rgba(1.0, 0.5, 0.0, 1.0)),
            duration_ms: 1000.0,
            delay_ms: 0.0,
            ease: Ease::Linear,
            repeat: Repeat::Count(1),
        };
        let (TweenValue::Color(mid), _) = tween_at(&tween, 0.5) else {
            panic!("expected a color");
        };
        assert!((mid.r - 0.5).abs() < 1e-6);
        assert!((mid.g - 0.25).abs() < 1e-6);
        assert!((mid.a - 0.5).abs() < 1e-6);
    }

    #[test]
    fn inline_canvas_does_not_restart_a_completed_transient_after_virtualization() {
        use canvas::Program;
        let now = Instant::now();
        let scene = accepted(
            serde_json::json!([{"kind":"circle","id":"pulse","r":3,"transient":true,"animate":{"r":{"to":1000,"duration":100}}}]),
        );
        let program = SceneProgram {
            started: Some(now.checked_sub(std::time::Duration::from_secs(3)).unwrap()),
            scene: SceneSource::Static(Arc::new(scene)),
            view_box: None,
            fit: ViewFit::Fill,
            on_pointer: None,
            image_store: None,
        };
        let mut state = CanvasState::default();
        assert!(
            program
                .update(
                    &mut state,
                    &iced::Event::Window(window::Event::RedrawRequested(now)),
                    Rectangle::with_size(Size::new(12.0, 12.0)),
                    mouse::Cursor::Unavailable
                )
                .is_none()
        );
        assert!(!any_animation_live(&state.clocks, state.now_s));
        assert!(
            resolve_record(&program.current().records[0], 0, &state.clocks, state.now_s).is_none()
        );
    }

    // ---- clocks: identity across generations ----------------------------------------------

    fn ring_scene(radius_to: f64) -> ParsedScene {
        accepted(json!([
            { "kind": "circle", "id": "ring", "cx": 0, "cy": 0, "r": 6,
              "animate": { "r": { "to": radius_to, "duration": 1000 } }, "transient": true },
        ]))
    }

    #[test]
    fn clock_survives_a_rewrite_with_the_same_spec() {
        let mut clocks = HashMap::new();
        reconcile_clocks(&ring_scene(400.0).records, &mut clocks, 10.0);
        assert_eq!(clocks["ring"].started_s, 10.0);
        // A later generation with the identical record keeps the running clock.
        reconcile_clocks(&ring_scene(400.0).records, &mut clocks, 10.5);
        assert_eq!(
            clocks["ring"].started_s, 10.0,
            "mid-flight rewrite preserves the clock"
        );
        // A changed spec is a retrigger.
        reconcile_clocks(&ring_scene(500.0).records, &mut clocks, 11.0);
        assert_eq!(clocks["ring"].started_s, 11.0);
    }

    #[test]
    fn removed_records_drop_their_clocks() {
        let mut clocks = HashMap::new();
        reconcile_clocks(&ring_scene(400.0).records, &mut clocks, 0.0);
        assert_eq!(clocks.len(), 1);
        reconcile_clocks(&accepted(json!([])).records, &mut clocks, 1.0);
        assert!(clocks.is_empty());
    }

    #[test]
    fn completed_transient_is_tombstoned_not_resurrected() {
        let scene = ring_scene(400.0);
        let mut clocks = HashMap::new();
        reconcile_clocks(&scene.records, &mut clocks, 0.0);
        assert!(resolve_record(&scene.records[0], 0, &clocks, 0.5).is_some());
        assert!(
            resolve_record(&scene.records[0], 0, &clocks, 2.0).is_none(),
            "completed transient stops drawing"
        );
        // Re-delivering the identical record later keeps the old clock: still complete.
        reconcile_clocks(&scene.records, &mut clocks, 5.0);
        assert!(
            resolve_record(&scene.records[0], 0, &clocks, 5.0).is_none(),
            "the retained clock tombstones the re-delivered record"
        );
        assert!(
            !any_animation_live(&clocks, 5.0),
            "no redraw requests for a dead scene"
        );
    }

    #[test]
    fn resolve_applies_animated_fields() {
        let scene = ring_scene(406.0);
        let mut clocks = HashMap::new();
        reconcile_clocks(&scene.records, &mut clocks, 0.0);
        let resolved = resolve_record(&scene.records[0], 0, &clocks, 0.5).unwrap();
        let Shape::Circle { r, .. } = resolved.shape else {
            panic!("expected circle");
        };
        assert!((r - 206.0).abs() < 1e-3, "halfway from 6 to 406, got {r}");
    }

    #[test]
    fn idless_animated_records_key_positionally() {
        let scene = accepted(json!([
            { "kind": "rect", "width": 1, "height": 1,
              "animate": { "x": { "to": 5, "duration": 100 } } },
        ]));
        let mut clocks = HashMap::new();
        reconcile_clocks(&scene.records, &mut clocks, 0.0);
        assert!(clocks.contains_key("~0:rect"));
    }

    // ---- path data ------------------------------------------------------------------------

    use super::path_data;

    #[test]
    fn path_data_parses_absolute_and_relative_forms() {
        let commands = path_data::parse("M 10 10 L 20 20 l 5 0 H 30 v -5 Z").unwrap();
        assert_eq!(
            commands,
            vec![
                PathCommand::MoveTo(Point::new(10.0, 10.0)),
                PathCommand::LineTo(Point::new(20.0, 20.0)),
                PathCommand::LineTo(Point::new(25.0, 20.0)),
                PathCommand::LineTo(Point::new(30.0, 20.0)),
                PathCommand::LineTo(Point::new(30.0, 15.0)),
                PathCommand::Close,
            ]
        );
    }

    #[test]
    fn path_data_implicit_repetition_after_move_is_lineto() {
        let commands = path_data::parse("M0 0 10 10 20 20").unwrap();
        assert_eq!(
            commands,
            vec![
                PathCommand::MoveTo(Point::ORIGIN),
                PathCommand::LineTo(Point::new(10.0, 10.0)),
                PathCommand::LineTo(Point::new(20.0, 20.0)),
            ]
        );
    }

    #[test]
    fn path_data_curves_and_reflection() {
        let commands = path_data::parse("M0 0 C 0 10 10 10 10 0 S 20 -10 20 0").unwrap();
        assert_eq!(commands.len(), 3);
        let PathCommand::Cubic { c1, .. } = &commands[2] else {
            panic!("S emits a cubic");
        };
        // Reflection of (10, 10) about (10, 0).
        assert_eq!(*c1, Point::new(10.0, -10.0));

        let commands = path_data::parse("M0 0 Q 5 10 10 0 T 20 0").unwrap();
        let PathCommand::Quad { control, .. } = &commands[2] else {
            panic!("T emits a quad");
        };
        assert_eq!(*control, Point::new(15.0, -10.0));
    }

    #[test]
    fn path_data_arcs_flatten_to_cubics_that_land_on_the_endpoint() {
        // Unspaced arc flags, the classic parser trap: `a1 1 0 011 1`.
        let commands = path_data::parse("M 0 0 a1 1 0 011 1").unwrap();
        assert!(commands.len() >= 2);
        let PathCommand::Cubic { to, .. } = commands.last().unwrap() else {
            panic!("arcs flatten to cubics");
        };
        assert!((to.x - 1.0).abs() < 1e-4 && (to.y - 1.0).abs() < 1e-4);

        // A half circle of radius 10: two <=90-degree cubic segments, endpoint exact.
        let commands = path_data::parse("M 0 0 A 10 10 0 0 1 20 0").unwrap();
        assert_eq!(commands.len(), 3, "move + two cubic segments");
        let PathCommand::Cubic { to, .. } = commands.last().unwrap() else {
            panic!("expected cubic");
        };
        assert_eq!(*to, Point::new(20.0, 0.0));
    }

    #[test]
    fn path_data_zero_radius_arc_degrades_to_a_line() {
        assert_eq!(
            path_data::parse("M 0 0 A 0 10 0 0 1 5 5").unwrap(),
            vec![
                PathCommand::MoveTo(Point::ORIGIN),
                PathCommand::LineTo(Point::new(5.0, 5.0)),
            ]
        );
    }

    #[test]
    fn path_data_rejects_garbage_loudly() {
        assert!(
            path_data::parse("10 10 L 0 0").is_err(),
            "must start with a command"
        );
        assert!(path_data::parse("M 1 banana").is_err());
        assert!(
            path_data::parse("M 0 0 X 1 1").is_err(),
            "unknown command letter"
        );
    }

    #[test]
    fn scientific_notation_and_compact_negatives_parse() {
        let commands = path_data::parse("M1e1 1E1L-5-5").unwrap();
        assert_eq!(
            commands,
            vec![
                PathCommand::MoveTo(Point::new(10.0, 10.0)),
                PathCommand::LineTo(Point::new(-5.0, -5.0)),
            ]
        );
    }

    // ---- image records --------------------------------------------------------------------

    #[test]
    fn bound_scenes_budget_never_seen_sources() {
        // One bound write may spawn at most MAX_NEW_BOUND_SOURCES fresh fetches; records
        // past the budget skip with a warning. Already-known sources ride free.
        let ctx = image_ctx(true);
        let over: Vec<_> = (0..=MAX_NEW_BOUND_SOURCES)
            .map(|i| {
                json!({ "kind": "image", "src": format!("https://x.example/{i}.png"),
                             "width": 1, "height": 1 })
            })
            .collect();
        let mut images = SceneImages::Live(&ctx);
        let parsed = parse_scene(
            &node(serde_json::Value::Array(over.clone())),
            Some(&mut images),
        )
        .unwrap();
        assert_eq!(parsed.records.len(), MAX_NEW_BOUND_SOURCES);
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("image budget"),
            "{:?}",
            parsed.warnings
        );
        // A re-write of the same scene: every source is now known — all records parse.
        let mut images = SceneImages::Live(&ctx);
        let parsed = parse_scene(&node(serde_json::Value::Array(over)), Some(&mut images)).unwrap();
        assert_eq!(parsed.records.len(), MAX_NEW_BOUND_SOURCES + 1);
        // Static scenes are author-written: never budgeted.
        let many: Vec<_> = (0..MAX_NEW_BOUND_SOURCES + 8)
            .map(
                |i| json!({ "kind": "image", "src": format!("s{i}.png"), "width": 1, "height": 1 }),
            )
            .collect();
        let parsed = parse_with_images(serde_json::Value::Array(many), false).unwrap();
        assert_eq!(parsed.records.len(), MAX_NEW_BOUND_SOURCES + 8);
    }

    #[test]
    fn refresh_walk_revives_evicted_cells() {
        let ctx = image_ctx(false);
        let mut images = SceneImages::Live(&ctx);
        let parsed = parse_scene(
            &node(json!([{ "kind": "image", "src": "logo.png", "width": 4, "height": 4 }])),
            Some(&mut images),
        )
        .unwrap();
        let slot = parsed.image_cells[0].clone();
        let before = slot.cell.load_full();
        // A flush evicts the cell; the refresh walk must swap in a fresh one.
        ctx.store.clear();
        assert!(before.is_evicted());
        slot.refresh(&ctx.store);
        let after = slot.cell.load_full();
        assert!(!Arc::ptr_eq(&before, &after), "evicted cell was replaced");
        assert!(!after.is_evicted());
        // A refresh with a live cell is a no-op swap-wise.
        slot.refresh(&ctx.store);
        assert!(Arc::ptr_eq(&after, &slot.cell.load_full()));
    }

    #[test]
    fn image_records_have_their_own_budget() {
        let over: Vec<_> = (0..=MAX_IMAGE_RECORDS)
            .map(
                |i| json!({ "kind": "image", "src": format!("a{i}.png"), "width": 1, "height": 1 }),
            )
            .collect();
        assert_eq!(
            parse_with_images(serde_json::Value::Array(over), false),
            Err(SceneReject::Budget("image-records"))
        );
    }

    #[test]
    fn bound_scene_image_srcs_are_descend_only() {
        // The same src: fine from a static scene (trusted user creator, unclamped), denied
        // from a bound one (the producer of a bound scene is not the widget's author).
        let scene = json!([
            { "kind": "image", "src": "../outside.png", "width": 1, "height": 1 },
        ]);
        let parsed = parse_with_images(scene.clone(), false).unwrap();
        assert_eq!(
            parsed.records.len(),
            1,
            "static: `..` allowed for a user module"
        );
        let parsed = parse_with_images(scene, true).unwrap();
        assert_eq!(parsed.records.len(), 0, "bound: descend-only");
        assert_eq!(parsed.warnings.len(), 1);
        assert!(
            parsed.warnings[0].contains("rejected"),
            "{:?}",
            parsed.warnings
        );

        // Absolute paths are always denied for bound values.
        let parsed = parse_with_images(
            json!([{ "kind": "image", "src": "/etc/passwd.png", "width": 1, "height": 1 }]),
            true,
        )
        .unwrap();
        assert_eq!(parsed.records.len(), 0);
    }

    #[test]
    fn image_records_skip_softly() {
        // A bad record skips (scene survives); the reasons land in warnings.
        let parsed = parse_with_images(
            json!([
                { "kind": "image", "width": 1, "height": 1 },                       // no src
                { "kind": "image", "src": "x.png", "fit": "tile" },                 // unknown fit
                { "kind": "image", "src": "x.png", "filter": "cubic" },             // unknown filter
                { "kind": "rect", "width": 1, "height": 1 },
            ]),
            false,
        )
        .unwrap();
        assert_eq!(parsed.records.len(), 1, "only the rect survives");
        assert_eq!(parsed.warnings.len(), 3);

        // No image context at all: the record skips with a warning, no panic.
        let parsed = parse(json!([
            { "kind": "image", "src": "x.png", "width": 1, "height": 1 },
        ]))
        .unwrap();
        assert_eq!(
            parsed.records.len(),
            1,
            "record kept (cell-less, draws nothing)"
        );
        assert_eq!(parsed.warnings.len(), 1);
        let Shape::Image(image) = &parsed.records[0].shape else {
            panic!("expected image");
        };
        assert!(image.slot.is_none());
    }

    #[test]
    fn image_animation_gates_on_geometry_fields_only() {
        // x/y/width/height/rotate + opacity animate; fit/filter/fill/stroke do not.
        let ok = parse_with_images(
            json!([{
                "kind": "image", "src": "x.png", "width": 10, "height": 10,
                "animate": {
                    "x": { "to": 5, "duration": 1 },
                    "rotate": { "to": 90, "duration": 1 },
                    "opacity": { "to": 0, "duration": 1 },
                },
            }]),
            false,
        )
        .unwrap();
        assert_eq!(ok.records.len(), 1);
        assert_eq!(ok.animated, 1);

        for field in ["fit", "filter", "fill", "stroke", "src"] {
            let parsed = parse_with_images(
                json!([{
                    "kind": "image", "src": "x.png", "width": 10, "height": 10,
                    "animate": { field: { "to": 1, "duration": 1 } },
                }]),
                false,
            )
            .unwrap();
            assert_eq!(
                parsed.records.len(),
                0,
                "animate.{field} must reject the record"
            );
        }
    }

    #[test]
    fn image_fit_math_matches_content_fit() {
        let spec = |fit| ImageSpec {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
            fit,
            nearest: false,
            rotate_deg: 0.0,
            slot: None,
        };
        // Fill: the box, exactly.
        let bounds = fitted_image_bounds(&spec(ImageFit::Fill), 40.0, 40.0);
        assert_eq!(
            bounds,
            Rectangle::new(Point::new(10.0, 20.0), Size::new(100.0, 50.0))
        );
        // Contain on a square 40x40 image in a 100x50 box: 50x50, centered horizontally.
        let bounds = fitted_image_bounds(&spec(ImageFit::Contain), 40.0, 40.0);
        assert_eq!(
            bounds,
            Rectangle::new(Point::new(35.0, 20.0), Size::new(50.0, 50.0))
        );
        // Cover: 100x100, vertically centered spill (clipped at the widget bounds).
        let bounds = fitted_image_bounds(&spec(ImageFit::Cover), 40.0, 40.0);
        assert_eq!(
            bounds,
            Rectangle::new(Point::new(10.0, -5.0), Size::new(100.0, 100.0))
        );
        // None: intrinsic pixels, centered.
        let bounds = fitted_image_bounds(&spec(ImageFit::None), 40.0, 40.0);
        assert_eq!(
            bounds,
            Rectangle::new(Point::new(40.0, 25.0), Size::new(40.0, 40.0))
        );
    }

    // ---- pointer mapping ------------------------------------------------------------------

    #[test]
    fn view_box_maps_pointer_coordinates_into_scene_space() {
        let program = SceneProgram {
            started: None,
            scene: SceneSource::Static(Arc::new(ParsedScene::default())),
            view_box: Some(Rectangle::new(
                Point::new(10.0, 20.0),
                Size::new(100.0, 50.0),
            )),
            fit: ViewFit::Fill,
            on_pointer: None,
            image_store: None,
        };
        let scene = program.to_scene(Point::new(110.0, 90.0), Size::new(220.0, 90.0));
        assert!((scene.x - 60.0).abs() < 1e-4, "x: got {}", scene.x);
        assert!((scene.y - 70.0).abs() < 1e-4, "y: got {}", scene.y);
        // Without a view_box, widget coordinates are scene coordinates.
        let identity = SceneProgram {
            view_box: None,
            ..program
        };
        assert_eq!(
            identity.to_scene(Point::new(7.0, 9.0), Size::new(220.0, 90.0)),
            Point::new(7.0, 9.0)
        );
    }

    #[test]
    fn contain_fit_scales_uniformly_and_centers() {
        let program = SceneProgram {
            started: None,
            scene: SceneSource::Static(Arc::new(ParsedScene::default())),
            view_box: Some(Rectangle::new(Point::ORIGIN, Size::new(480.0, 480.0))),
            fit: ViewFit::Contain,
            on_pointer: None,
            image_store: None,
        };
        // A wide widget: height limits, content is 400x400 centered with 200px margins.
        let bounds = Size::new(800.0, 400.0);
        let (_, scale, offset) = program.view_mapping(bounds).unwrap();
        assert!((scale.x - scale.y).abs() < 1e-6, "contain scale is uniform");
        assert!((scale.x - 400.0 / 480.0).abs() < 1e-6);
        assert!((offset.x - 200.0).abs() < 1e-4 && offset.y.abs() < 1e-4);
        // The content corners round-trip; a margin point maps outside the box.
        let top_left = program.to_scene(Point::new(200.0, 0.0), bounds);
        assert!(top_left.x.abs() < 1e-3 && top_left.y.abs() < 1e-3);
        let bottom_right = program.to_scene(Point::new(600.0, 400.0), bounds);
        assert!((bottom_right.x - 480.0).abs() < 1e-3 && (bottom_right.y - 480.0).abs() < 1e-3);
        assert!(
            program.to_scene(Point::new(0.0, 0.0), bounds).x < 0.0,
            "margin maps outside"
        );
    }

    // ---- bound-scene memoization ----------------------------------------------------------

    #[test]
    fn bound_scene_reparses_only_on_snapshot_change_and_keeps_rejected_out() {
        let cell = Arc::new(StoreBindingCell::new(json!([
            { "kind": "rect", "width": 1, "height": 1 },
        ])));
        let program = SceneProgram {
            started: None,
            scene: SceneSource::Bound {
                cell: cell.clone(),
                memo: Arc::new(Mutex::new(SceneMemo::default())),
                fallback: Arc::new(ParsedScene::default()),
                image_ctx: None,
            },
            view_box: None,
            fit: ViewFit::Fill,
            on_pointer: None,
            image_store: None,
        };
        let first = program.current();
        assert_eq!(first.records.len(), 1);
        assert!(
            Arc::ptr_eq(&first, &program.current()),
            "unchanged snapshot returns the memoized parse"
        );

        // A rejected generation keeps the previous scene on screen.
        cell.set(json!("not a scene"));
        let after_reject = program.current();
        assert!(
            Arc::ptr_eq(&first, &after_reject),
            "rejected write leaves the prior scene"
        );

        // An accepted one replaces it.
        cell.set(json!([
            { "kind": "rect", "width": 2, "height": 2 },
            { "kind": "rect", "width": 3, "height": 3 },
        ]));
        assert_eq!(program.current().records.len(), 2);

        // A null snapshot (absent path) serves the binding's fallback scene (empty here).
        cell.set(json!(null));
        assert!(program.current().records.is_empty());
    }
}

// The terminal supplies each attachment's mount clock during factory invocation.
// This also preserves transient completion when virtualization reconstructs a tree.
thread_local! { static INLINE_START: std::cell::Cell<Option<Instant>> = const {std::cell::Cell::new(None)}; }
pub(crate) fn with_inline_start<T>(started: Instant, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            INLINE_START.set(self.0);
        }
    }
    let _restore = Restore(INLINE_START.replace(Some(started)));
    f()
}
pub(crate) fn inline_start() -> Option<Instant> {
    INLINE_START.get()
}

/// The canvas frame covers its declared paint bounds, while layout and pointer
/// input keep the authored small box. Both GPU and software damage tracking see
/// the real geometry extent, including pixels outside that box.
pub(crate) struct OverflowCanvas {
    pub program: SceneProgram,
    pub width: iced::Length,
    pub height: iced::Length,
    pub overflow: u16,
}
impl OverflowCanvas {
    fn paint_bounds(&self, bounds: Rectangle, viewport: &Rectangle) -> Option<Rectangle> {
        if self.overflow == u16::MAX {
            bounds
                .expand(viewport.height)
                .intersects(viewport)
                .then_some(*viewport)
        } else {
            bounds
                .expand(f32::from(self.overflow))
                .intersection(viewport)
        }
    }
}
impl iced::advanced::Widget<WidgetMessage, smudgy_theme::Theme, iced::Renderer> for OverflowCanvas {
    fn size(&self) -> Size<iced::Length> {
        Size::new(self.width, self.height)
    }
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<CanvasState>()
    }
    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(CanvasState::default())
    }
    fn layout(
        &mut self,
        _: &mut iced::advanced::widget::Tree,
        _: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        iced::advanced::layout::atomic(limits, self.width, self.height)
    }
    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: mouse::Cursor,
        _: &iced::Renderer,
        _: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, WidgetMessage>,
        viewport: &Rectangle,
    ) {
        use canvas::Program;
        let bounds = layout.bounds();
        if self.paint_bounds(bounds, viewport).is_none() {
            return;
        }
        if let Some(action) = self.program.update(
            tree.state.downcast_mut::<CanvasState>(),
            event,
            bounds,
            cursor,
        ) {
            let (message, redraw, status) = action.into_inner();
            // Bounded native cadence; no JS callback runs for animation frames.
            // Preserve Canvas's next-frame request so native tweens follow the
            // presentation cadence, including high-refresh displays.
            shell.request_redraw_at(redraw);
            if let Some(message) = message {
                shell.publish(message);
            }
            if status == iced::event::Status::Captured {
                shell.capture_event();
            }
        }
    }
    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        _: &smudgy_theme::Theme,
        _: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::{Renderer as _, graphics::geometry::Renderer as _};
        let bounds = layout.bounds();
        let Some(paint) = self.paint_bounds(bounds, viewport) else {
            return;
        };
        if paint.width < 1.0 || paint.height < 1.0 {
            return;
        }
        let state = tree.state.downcast_ref::<CanvasState>();
        let parsed = self.program.current();
        let mut frame = canvas::Frame::new(renderer, paint.size());
        frame.translate(Vector::new(bounds.x - paint.x, bounds.y - paint.y));
        if let Some((view_box, scale, offset)) = self.program.view_mapping(bounds.size()) {
            frame.translate(offset);
            frame.scale_nonuniform(scale);
            frame.translate(Vector::new(-view_box.x, -view_box.y));
        }
        draw_records(
            &mut frame,
            &parsed.records,
            &state.clocks,
            state.now_s,
            &mut 0,
        );
        renderer.with_layer(paint, |renderer| {
            renderer.with_translation(Vector::new(paint.x, paint.y), |renderer| {
                renderer.draw_geometry(frame.into_geometry());
            });
        });
    }
}
