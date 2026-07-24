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

use deno_core::v8;
use iced::widget::canvas;
use iced::{Color, Point, Radians, Rectangle, Size, Vector, mouse, window};
use smudgy_cloud::{Node, StoreBindingCell, WidgetIsolate};

use crate::WidgetMessage;

// ---------------------------------------------------------------------------------------------
// Complexity budgets (plans/widgets-iced-primitives.md §11). Exceeding any of these rejects the
// scene generation atomically; see the module docs for why rejection is all-or-nothing.

pub(crate) const MAX_RECORDS: usize = 10_000;
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
    /// Per-record soft failures, logged once per generation by the consumer.
    pub warnings: Vec<String>,
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
}

impl BudgetLedger {
    fn check(&self) -> Result<(), SceneReject> {
        if self.records > MAX_RECORDS {
            return Err(SceneReject::Budget("record-count"));
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
/// cell, static scenes convert once at build.
pub(crate) fn parse_scene(root: &Node) -> Result<ParsedScene, SceneReject> {
    let Node::Array(items) = root else {
        return Err(SceneReject::NotAnArray);
    };
    let mut scene = ParsedScene::default();
    let mut ledger = BudgetLedger::default();
    let mut ids = HashSet::new();
    scene.records = parse_records(items.items(), 0, &mut ledger, &mut ids, &mut scene.warnings)?;
    scene.animated = count_animated(&scene.records);
    Ok(scene)
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
) -> Result<Vec<Record>, SceneReject> {
    if depth > MAX_DEPTH {
        return Err(SceneReject::Budget("nesting-depth"));
    }
    let mut records = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        ledger.records += 1;
        ledger.approx_bytes += RECORD_OVERHEAD_BYTES;
        ledger.check()?;
        match parse_record(item, depth, ledger, ids, warnings) {
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
        return Err(skip("fill/stroke color must be a CSS color string or { gradient }"));
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
        return Err(skip("gradient.stops must be an array of [offset, color] pairs"));
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
    let paint = node
        .get("color")
        .map_or_else(|| Ok(Paint::Solid(Color::BLACK)), |color| parse_paint(color, ledger))?;
    let width = f32_field(node, "width", 1.0);
    let dash = match node.get("dash") {
        None | Some(Node::Null) => Vec::new(),
        Some(Node::Array(items)) => {
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
            let points = parse_points(
                node.get("points").ok_or_else(|| skip("missing points"))?,
            )?;
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
                Some(Node::Array(items)) => {
                    parse_records(items.items(), depth + 1, ledger, ids, warnings)?
                }
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
        Shape::Group { .. } => &["translate_x", "translate_y", "rotate", "scale"],
        Shape::Polyline { .. } | Shape::Polygon { .. } | Shape::Path { .. } => &[],
    };
    allowed.contains(&field)
}

fn color_field_allowed(shape: &Shape, field: &str) -> bool {
    match field {
        "fill" | "stroke" => !matches!(shape, Shape::Group { .. } | Shape::Text(_)),
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
        let is_number = field == "opacity"
            || field == "stroke_width"
            || numeric_field_allowed(shape, field);
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
            Some(control) => Point::new(
                2.0 * current.x - control.x,
                2.0 * current.y - control.y,
            ),
            // No previous curve to reflect: the control coincides with the current point
            // (SVG 2 §9.5.2), degrading S/T to a plain curve start.
            None => current,
        }
    }

    /// Endpoint-parameterized arc → center parameterization → cubic segments of at most 90°
    /// each (the standard SVG implementation-notes conversion, F.6). Degenerate radii draw a
    /// straight line, per spec.
    #[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
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
            "stroke" => {
                match &mut record.stroke {
                    Some(stroke) => stroke.paint = Paint::Solid(color),
                    None => {
                        record.stroke = Some(StrokeSpec {
                            paint: Paint::Solid(color),
                            width: 1.0,
                            dash: Vec::new(),
                        });
                    }
                }
            }
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
        Shape::Circle { cx, cy, r } => {
            canvas::Path::circle(Point::new(*cx, *cy), *r)
        }
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
        Shape::Text(_) | Shape::Group { .. } => return None,
    };
    Some(path)
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
    pub callback: Arc<v8::Global<v8::Function>>,
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
    pub view_box: Option<Rectangle>,
    pub fit: ViewFit,
    pub on_pointer: Option<PointerHandler>,
}

/// Per-widget-instance UI-side state, living in iced's widget `Tree` (the one place that is
/// created, used, and dropped entirely on the UI thread — the render closure itself is built
/// on the script thread and must not own thread-affine state).
pub(crate) struct CanvasState {
    cache: canvas::Cache,
    clocks: HashMap<String, ClockEntry>,
    /// The `Arc::as_ptr` of the generation the clocks were last reconciled against.
    reconciled: usize,
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
                    match parse_scene(&loaded) {
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
                state.tick(*now);
                let parsed = self.current();
                let generation = Arc::as_ptr(&parsed) as usize;
                if state.reconciled != generation {
                    state.reconciled = generation;
                    state.cache.clear();
                    reconcile_clocks(&parsed.records, &mut state.clocks, state.now_s);
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
        parse_scene(&node(value))
    }

    fn accepted(value: serde_json::Value) -> ParsedScene {
        parse(value).expect("scene should be accepted")
    }

    // ---- scene grammar --------------------------------------------------------------------

    #[test]
    fn parses_every_shape_kind() {
        let scene = accepted(json!([
            { "kind": "rect", "x": 1, "y": 2, "width": 3, "height": 4, "rx": 1, "fill": "#ff0000" },
            { "kind": "circle", "cx": 5, "cy": 6, "r": 7, "stroke": { "color": "#00ff00", "width": 2 } },
            { "kind": "ellipse", "cx": 1, "cy": 1, "rx": 2, "ry": 3 },
            { "kind": "line", "x1": 0, "y1": 0, "x2": 10, "y2": 10, "stroke": {} },
            { "kind": "polyline", "points": [[0, 0], [5, 5], [10, 0]], "stroke": {} },
            { "kind": "polygon", "points": [[0, 0], [5, 5], [10, 0]], "fill": "red" },
            { "kind": "path", "d": "M 0 0 L 10 10 Z", "stroke": {} },
            { "kind": "text", "x": 4, "y": 8, "text": "HP", "size": 12, "color": "white" },
            { "kind": "group", "transform": { "translate": [5, 5] }, "children": [
                { "kind": "rect", "width": 1, "height": 1 },
            ] },
        ]));
        assert_eq!(scene.records.len(), 9);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert_eq!(scene.animated, 0);
        let Shape::Rect { x, rx, .. } = &scene.records[0].shape else {
            panic!("expected rect");
        };
        assert_eq!((*x, *rx), (1.0, 1.0));
        assert_eq!(scene.records[0].fill, Some(Paint::Solid(Color::from_rgb(1.0, 0.0, 0.0))));
        let Shape::Group { children, .. } = &scene.records[8].shape else {
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
        assert_eq!(parse(json!({ "kind": "rect" })), Err(SceneReject::NotAnArray));
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
        assert_eq!((*start, *end), (Point::new(4.0, 0.0), Point::new(144.0, 0.0)));
        assert_eq!(stops.len(), 2);

        let stops: Vec<_> = (0..=8).map(|i| json!([f64::from(i) / 8.0, "#ffffff"])).collect();
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
        assert_eq!(tween.from, Some(TweenValue::Number(6.0)), "from defaults to the static r");
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
        assert!((value - 2.5).abs() < 1e-4, "restarted second run, got {value}");
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
        assert_eq!(clocks["ring"].started_s, 10.0, "mid-flight rewrite preserves the clock");
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
        assert!(!any_animation_live(&clocks, 5.0), "no redraw requests for a dead scene");
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
        assert!(path_data::parse("10 10 L 0 0").is_err(), "must start with a command");
        assert!(path_data::parse("M 1 banana").is_err());
        assert!(path_data::parse("M 0 0 X 1 1").is_err(), "unknown command letter");
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

    // ---- pointer mapping ------------------------------------------------------------------

    #[test]
    fn view_box_maps_pointer_coordinates_into_scene_space() {
        let program = SceneProgram {
            scene: SceneSource::Static(Arc::new(ParsedScene::default())),
            view_box: Some(Rectangle::new(Point::new(10.0, 20.0), Size::new(100.0, 50.0))),
            fit: ViewFit::Fill,
            on_pointer: None,
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
            scene: SceneSource::Static(Arc::new(ParsedScene::default())),
            view_box: Some(Rectangle::new(Point::ORIGIN, Size::new(480.0, 480.0))),
            fit: ViewFit::Contain,
            on_pointer: None,
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
        assert!(program.to_scene(Point::new(0.0, 0.0), bounds).x < 0.0, "margin maps outside");
    }

    // ---- bound-scene memoization ----------------------------------------------------------

    #[test]
    fn bound_scene_reparses_only_on_snapshot_change_and_keeps_rejected_out() {
        let cell = Arc::new(StoreBindingCell::new(json!([
            { "kind": "rect", "width": 1, "height": 1 },
        ])));
        let program = SceneProgram {
            scene: SceneSource::Bound {
                cell: cell.clone(),
                memo: Arc::new(Mutex::new(SceneMemo::default())),
                fallback: Arc::new(ParsedScene::default()),
            },
            view_box: None,
            fit: ViewFit::Fill,
            on_pointer: None,
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
        assert!(Arc::ptr_eq(&first, &after_reject), "rejected write leaves the prior scene");

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
