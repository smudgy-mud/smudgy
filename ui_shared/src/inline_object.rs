//! Native widgets occupying atomic object-replacement glyphs in terminal paragraphs.
//! The bundled, ink-free metric font has a one-em advance. Its size reserves the
//! measured widget width exactly; independent line-height reserves its height.
//! Source text never contains these glyphs: copy/search/log use the authored fallback.
//!
//! An object with an empty range, an *anchor* (a terminal graphics placement that leaves
//! the cursor where it was), reserves nothing: a zero-advance marker glyph carries its id
//! to where the paragraph puts the next character, and the widget hangs from the top of
//! that row over whatever follows, the row no taller for it. Objects draw in three passes
//! around the pane's rows: [`Host::draw_under`] twice before the pane's text (the
//! placements that go under the rows' background colours, then those that go under the
//! text alone) and [`Host::draw`] after it, each pass a line's objects in ascending z-index.
use crate::terminal_buffer::{RenderedOffsets, RenderedSpans, SpanMetadata};
use iced::{
    Element, Event, Point, Rectangle, Size, Vector,
    advanced::{
        self, Layout, layout, mouse, renderer,
        text::Paragraph,
        widget::{Tree, tree},
    },
};
use smudgy_protocol::graphics::{PLACEMENT_DEPTH_LIMIT, Placement};
use smudgy_session_model::{
    StyledLine,
    inline_content::{Baseline, InlineObject, Layer},
};
use std::{any::Any, collections::BTreeMap, ops::Range, rc::Rc, sync::Arc};

/// The text grid of the row an object is viewed in, in logical pixels. An
/// object sized in cells (a terminal graphics placement covers `columns`
/// cells of its line and `rows` text rows) measures itself with these; a
/// script widget sizes itself and ignores them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ObjectMetrics {
    /// The advance of one monospace cell at the pane's font and size.
    pub cell_width: f32,
    /// The height of one text row.
    pub line_height: f32,
}

#[derive(Clone)]
pub struct Resolver(Rc<dyn Any>);
type Factory<M, T, R> =
    Rc<dyn Fn(&InlineObject, &ObjectMetrics) -> Option<Element<'static, M, T, R>>>;
pub fn resolver<M: 'static, T: 'static, R: 'static>(
    f: impl Fn(&InlineObject, &ObjectMetrics) -> Option<Element<'static, M, T, R>> + 'static,
) -> Resolver {
    Resolver(Rc::new(Rc::new(f) as Factory<M, T, R>))
}
impl Resolver {
    fn factory<M: 'static, T: 'static, R: 'static>(&self) -> Option<&Factory<M, T, R>> {
        self.0.downcast_ref::<Factory<M, T, R>>()
    }
    /// Builds the element for `object` viewed in a row of `metrics`, or
    /// `None` when the resolver was made for another renderer or the object
    /// has nothing to show.
    pub fn resolve<M: 'static, T: 'static, R: 'static>(
        &self,
        object: &InlineObject,
        metrics: &ObjectMetrics,
    ) -> Option<Element<'static, M, T, R>> {
        self.factory::<M, T, R>()
            .and_then(|factory| factory(object, metrics))
    }
}
const FONT: iced::Font = iced::Font::with_name("Smudgy Inline Object");
/// A zero-advance glyph whose ascent is twice its descent. cosmic-text
/// centres a line's glyph box — its tallest ascent over its deepest descent
/// — inside the line height, so a box of ascent `2T` over descent `T` puts
/// the baseline `T/2` below the centre. Shaped right after a tall object's
/// glyph, at the size [`riser_size`] computes, it moves the row's text
/// baseline onto the object's last text row, where a terminal leaves the
/// cursor after an image.
const RISER: iced::Font = iced::Font::with_name("Smudgy Inline Riser");
fn load_font() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut system = iced::advanced::graphics::text::font_system()
            .write()
            .unwrap();
        system.load_font(std::borrow::Cow::Borrowed(include_bytes!(
            "inline_object/metrics.ttf"
        )));
        system.load_font(std::borrow::Cow::Borrowed(include_bytes!(
            "inline_object/riser.ttf"
        )));
    });
}

/// The riser's font size for an object `height` tall in a row whose text
/// line height is `row_line_height` and whose text ascent exceeds its
/// descent by `text_rise`; `None` for an object no taller than the text,
/// which needs no lifting, and for one whose text stays centred (a script
/// widget), since only a [`Baseline::LastRow`] object asks for it. With
/// the object's own glyph contributing no ascent or descent, the baseline
/// lands at `height/2 + T/2` with `T = height - row_line_height +
/// text_rise`: the last text row's own baseline.
fn riser_size(
    baseline: Baseline,
    height: f32,
    row_line_height: f32,
    text_rise: f32,
) -> Option<f32> {
    (baseline == Baseline::LastRow && height > row_line_height + 0.5)
        .then_some(2.0 * (height - row_line_height + text_rise))
}

/// The object glyph alone, or with its riser and the break after it:
/// U+FFFC, U+E000 and U+200B are three bytes each.
fn rendered_len(riser: Option<f32>) -> usize {
    if riser.is_some() { 9 } else { 3 }
}

/// The metric font's U+0001 marking where an anchor stands, and its length: one byte.
const MARKER: &str = "\u{1}";
const MARKER_LEN: usize = MARKER.len();

/// An anchor's marker: U+0001 in the metric font, mapped to a glyph that advances by
/// nothing, so the text around it stays exactly where it was. It is set at the text's
/// own size with no line height of its own: the font's zero ascent and descent leave the
/// row's glyph box alone (the riser's, at the text's size, would deepen it and move the
/// baseline), and the marker's box in the paragraph is then its visual line's, which is
/// how [`Host::position`] knows that line's height. A C0 control is bidi class BN, so it
/// takes the level of the text around it and never parts a right-to-left run (U+E000,
/// the riser's glyph, is a strong left-to-right letter: standing between two Hebrew
/// words it would swap them), and line-break class CM, so it attaches to the character
/// before it and opens no break: after a space it goes with the word that follows, where
/// a terminal's cursor would be, and inside a word it parts nothing. It is not
/// default-ignorable, so the shaper keeps its glyph where it would hide U+200B's.
///
/// Two limits are the paragraph's own. cosmic-text lays out the shaping runs of one word
/// in logical order, so an anchor *inside* a right-to-left word (its marker is a run of
/// its own) still misorders that word's letters. And a zero-width word always fits the
/// line it follows, so a marker whose cell no longer does (the text before it fills the
/// row) is left at that line's end; [`wrapped_marker`] moves the anchor to where the
/// text after it went.
fn anchor_marker(id: u64) -> iced::widget::text::Span<'static, SpanMetadata> {
    iced::widget::text::Span::new(MARKER)
        .font(FONT)
        .link(SpanMetadata {
            object: Some(id),
            ..Default::default()
        })
}

/// Where an object stacks among the objects of its line drawn in the same pass: by
/// z-index, then, for a terminal graphics placement, by its image's id, as the graphics
/// protocol orders two images of one z-index. Ties (script widgets are all z 0 with no
/// image) keep creation order, which the mount map's key already gives.
fn stacking(object: &InlineObject) -> (i32, u32) {
    let image = object
        .resource
        .downcast_ref::<Placement>()
        .map_or(0, |placement| placement.image.id);
    (object.z, image)
}

/// The two passes that draw objects under the pane's text, parted at the graphics
/// protocol's threshold: a placement with a z-index below `i32::MIN / 2` goes under the
/// cells' background colours as well, every other negative one between those colours and
/// the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Under {
    /// z < `i32::MIN / 2` (-1073741824): under the rows' backgrounds too.
    Backgrounds,
    /// `i32::MIN / 2` <= z < 0: over the rows' backgrounds, under their text.
    Text,
}

impl Under {
    /// The z-index below which a placement goes under the backgrounds as well.
    pub(crate) const BACKGROUNDS_BELOW: i32 = i32::MIN / 2;

    fn of(z: i32) -> Self {
        if z < Self::BACKGROUNDS_BELOW {
            Self::Backgrounds
        } else {
            Self::Text
        }
    }
}

/// One of the three drawing passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Under(Under),
    Over,
}

/// How far an object may paint beyond its box: its own outset or, for a terminal graphics
/// placement, as far as its relative placements reach when that is more. The placement's
/// widget draws those children (each `H` columns and `V` rows from its parent's top-left
/// cell, with children of its own) and the host culls the family by this reach, so a
/// child in view keeps drawing after its parent's own box has left the viewport. The
/// pane lays out only the rows of its overscan band (a few beyond the viewport, as deep
/// as the widest object outset); a child further from its row than that is culled with
/// the row.
fn paint_reach(object: &InlineObject, metrics: &ObjectMetrics, size: Size) -> f32 {
    let outset = f32::from(object.paint_outset);
    let Some(placement) = object.resource.downcast_ref::<Placement>() else {
        return outset;
    };
    let mut extent = Rectangle::with_size(size);
    relative_extent(
        &placement.child_placements(),
        Point::ORIGIN,
        1,
        metrics,
        &mut extent,
    );
    let overhang = [
        -extent.x,
        -extent.y,
        extent.x + extent.width - size.width,
        extent.y + extent.height - size.height,
    ]
    .into_iter()
    .fold(0.0_f32, f32::max);
    outset.max(overhang)
}

/// Grows `extent` to the boxes of `children` and theirs, down to [`PLACEMENT_DEPTH_LIMIT`]
/// levels below the placement on the line (`depth` is the children's), each at `cell`
/// plus its offset in `metrics`' grid; a deleted child and its own are skipped, as the
/// widget skips them.
#[allow(
    clippy::cast_precision_loss,
    reason = "relative offsets are a few thousand cells at most, exact in f32"
)]
fn relative_extent(
    children: &[Arc<Placement>],
    cell: Point,
    depth: usize,
    metrics: &ObjectMetrics,
    extent: &mut Rectangle,
) {
    if depth > PLACEMENT_DEPTH_LIMIT {
        return;
    }
    for child in children {
        if child.is_deleted() {
            continue;
        }
        let offset = child.relative.unwrap_or_default();
        let origin = Point::new(
            cell.x + offset.columns as f32 * metrics.cell_width,
            cell.y + offset.rows as f32 * metrics.line_height,
        );
        let size = Size::new(
            f32::from(child.geometry.columns) * metrics.cell_width,
            f32::from(child.geometry.rows) * metrics.line_height,
        );
        *extent = extent.union(&Rectangle::new(origin, size));
        relative_extent(
            &child.child_placements(),
            origin,
            depth + 1,
            metrics,
            extent,
        );
    }
}

/// Where an anchor's marker belongs when the paragraph left it at the end of a visual line
/// its cell does not fit on (the text before it fills the row): at the start of the next
/// visual line, where a terminal's cursor goes after wrapping and where the paragraph put
/// the text after the marker. `None` when the cell fits, or when nothing of the paragraph
/// follows on a later line: the marker stays where it is. `marker` is the box of the
/// marker's span, `index`. The next line is found by the first box of each later span:
/// the first on a lower line starts it, and the boxes of that line's other spans say how
/// tall it is (a text span's box is one text line, an object's glyph its whole box).
fn wrapped_marker<P: Paragraph>(
    paragraph: &P,
    index: usize,
    span_count: usize,
    marker: Rectangle,
    cell_width: f32,
) -> Option<Rectangle> {
    if marker.x + cell_width <= paragraph.bounds().width + 0.5 {
        return None;
    }
    let mut later = (index + 1..span_count)
        .filter_map(|span| paragraph.span_bounds(span).into_iter().next())
        .skip_while(|first| first.y <= marker.y + 0.5);
    let start = later.next()?;
    let height = later
        .take_while(|first| (first.y - start.y).abs() < 0.5)
        .fold(start.height, |height, first| height.max(first.height));
    Some(Rectangle::new(
        Point::new(0.0, start.y),
        Size::new(0.0, height),
    ))
}

/// An object may be as wide as its row and as tall as it likes.
fn object_limits(width: f32) -> layout::Limits {
    layout::Limits::new(Size::ZERO, Size::new(width.max(1.0), 4096.0))
}

struct Mount<M, T, R> {
    element: Element<'static, M, T, R>,
    tree: Tree,
    node: layout::Node,
    used: bool,
    /// Whether the last layout found the object's glyph or marker in its paragraph and
    /// moved the node there. One whose text a concealment or another object swallowed
    /// has nowhere to be, and is neither drawn nor hit.
    positioned: bool,
    /// How far beyond its box it may paint: see [`paint_reach`].
    outset: f32,
    /// The text grid of its row: a cell's advance, and the height of one text line.
    metrics: ObjectMetrics,
    /// Where the widget sits in a row taller than it: centred, or at the
    /// top beside text on the object's last row.
    baseline: Baseline,
    /// An empty range: the widget hangs from its marker over the text after it, takes
    /// up no room in the row, and is inert to the mouse.
    anchor: bool,
    /// The pass that draws it: under the pane's text or over it.
    layer: Layer,
    /// Its place among its line's mounts in that pass: see [`stacking`].
    order: (i32, u32),
}
impl<M, T, R> Mount<M, T, R> {
    /// The pass that draws it.
    fn pass(&self) -> Pass {
        match self.layer {
            Layer::Over => Pass::Over,
            Layer::Under => Pass::Under(Under::of(self.order.0)),
        }
    }
}
pub(crate) struct Host<M, T, R> {
    mounts: BTreeMap<(usize, u64), Mount<M, T, R>>,
}
impl<M: 'static, T: 'static, R: advanced::Renderer + 'static> Host<M, T, R> {
    pub fn init(children: &mut Vec<Tree>) -> &mut Self {
        if children.is_empty() {
            children.push(Tree {
                tag: tree::Tag::of::<Self>(),
                state: tree::State::new(Self {
                    mounts: BTreeMap::new(),
                }),
                children: Vec::new(),
            });
        }
        let host = children[0].state.downcast_mut::<Self>();
        for mount in host.mounts.values_mut() {
            mount.used = false;
            mount.positioned = false;
        }
        host
    }
    pub fn get(tree: &Tree) -> Option<&Self> {
        tree.children
            .first()
            .map(|t| t.state.downcast_ref::<Self>())
    }
    pub fn get_mut(tree: &mut Tree) -> Option<&mut Self> {
        tree.children
            .first_mut()
            .map(|t| t.state.downcast_mut::<Self>())
    }

    /// View and measure the line's objects, replacing each one's text with
    /// its placeholder glyph. `metrics` is the row's text grid, handed to
    /// each factory; its line height and `text_rise` (the text's ascent
    /// minus its descent) size the riser that keeps text beside a
    /// [`Baseline::LastRow`] object taller than a row on that row's last
    /// text line; a centred object gets none, and its text stays centred.
    /// An anchor (an empty range) replaces nothing: its marker goes in where
    /// it stands, ahead of any glyph that starts there.
    #[allow(
        clippy::too_many_arguments,
        reason = "everything a line's layout needs, passed once per line as the pane has it"
    )]
    pub fn prepare(
        &mut self,
        line: &StyledLine,
        line_number: usize,
        mut rendered: RenderedSpans,
        resolver: Option<&Resolver>,
        renderer: &R,
        width: f32,
        metrics: &ObjectMetrics,
        text_rise: f32,
    ) -> RenderedSpans {
        let Some(objects) = &line.objects else {
            return rendered;
        };
        let Some(factory) = resolver.and_then(Resolver::factory::<M, T, R>) else {
            return rendered;
        };
        let mut replacements = Vec::new();
        // (rendered offset, source offset, object id) of each anchor.
        let mut anchors = Vec::new();
        for object in objects.iter().filter(|o| o.owner.active()) {
            let key = (line_number, object.id);
            // Keep pathological collections bounded; omitted controls retain their
            // readable text projection until fewer objects occupy the viewport.
            if !self.mounts.contains_key(&key) && self.mounts.len() >= 512 {
                if let Some(id) = self
                    .mounts
                    .iter()
                    .find_map(|(id, m)| (!m.used).then_some(*id))
                {
                    self.mounts.remove(&id);
                } else {
                    continue;
                }
            }
            let Some(element) = factory(object, metrics) else {
                continue;
            };
            let mount = self.mounts.entry(key).or_insert_with(|| Mount {
                tree: Tree::new(&element),
                element: iced::widget::Space::new().into(),
                node: layout::Node::new(Size::ZERO),
                used: true,
                positioned: false,
                outset: 0.0,
                metrics: *metrics,
                baseline: object.baseline,
                anchor: false,
                layer: object.layer,
                order: (0, 0),
            });
            mount.tree.diff(&element);
            mount.element = element;
            mount.used = true;
            mount.metrics = *metrics;
            mount.baseline = object.baseline;
            mount.anchor = object.range.is_empty();
            mount.layer = object.layer;
            mount.order = stacking(object);
            mount.node = mount.element.as_widget_mut().layout(
                &mut mount.tree,
                renderer,
                &object_limits(width),
            );
            let size = mount.node.size();
            mount.outset = paint_reach(object, metrics, size);
            let start = rendered.offsets.source_to_rendered(object.range.start);
            if mount.anchor {
                anchors.push((start, object.range.start, object.id));
                continue;
            }
            let end = rendered.offsets.source_to_rendered(object.range.end);
            if start < end {
                let riser =
                    riser_size(object.baseline, size.height, metrics.line_height, text_rise);
                replacements.push((start..end, object.id, size, riser));
            }
        }
        // An anchor inside another object's text has no character left to mark: the
        // object's glyph stands in for all of it.
        anchors.retain(|(at, ..)| {
            !replacements
                .iter()
                .any(|(range, ..)| range.start < *at && *at < range.end)
        });
        if replacements.is_empty() && anchors.is_empty() {
            return rendered;
        }
        load_font();
        replacements.sort_by_key(|(r, _, _, _)| r.start);
        anchors.sort_by_key(|(at, ..)| *at);
        // Anchors cut the spans too: their markers go between the pieces either side.
        let boundaries = replacements
            .iter()
            .flat_map(|(r, _, _, _)| [r.start, r.end])
            .chain(anchors.iter().map(|(at, ..)| *at));
        let mut spans =
            Vec::with_capacity(rendered.spans.len() + 2 * replacements.len() + anchors.len());
        let mut replacement = 0;
        let mut anchor = 0;
        crate::span_cuts::split_at(&rendered.spans, boundaries, |piece, part| {
            // Markers go before whatever starts where they stand, an object's glyph
            // included: the anchor hangs where that object's box begins.
            while let Some((at, _, id)) = anchors.get(anchor)
                && *at <= piece.start
            {
                spans.push(anchor_marker(*id));
                anchor += 1;
            }
            while replacement < replacements.len() && replacements[replacement].0.end <= piece.start
            {
                replacement += 1;
            }
            match replacements
                .get(replacement)
                .filter(|(r, _, _, _)| r.start <= piece.start)
            {
                // The object glyph stands in for the whole of its text; later
                // pieces inside the range are that text and are dropped.
                Some((range, id, size, riser)) if range.start == piece.start => {
                    spans.push(
                        iced::widget::text::Span::new("\u{fffc}")
                            .font(FONT)
                            .size(size.width.max(0.01))
                            .line_height(iced::widget::text::LineHeight::Absolute(iced::Pixels(
                                size.height.max(1.0),
                            )))
                            .link(SpanMetadata {
                                object: Some(*id),
                                ..Default::default()
                            }),
                    );
                    // After the glyph, never before: a zero-width word always
                    // fits the line it follows, so a wrap cannot part them.
                    // The zero-width space closes that word. To the line
                    // breaker U+E000 is a letter (class XX, treated as AL),
                    // and letters never part, so a riser alone would join the
                    // word after it and leave with that word when it wraps:
                    // the object's line would lose its riser and the next
                    // line, a plain one, would carry it instead. U+200B is
                    // class ZW, which forbids a break before it and allows
                    // one after; it is default-ignorable, so the shaper gives
                    // it no advance and no ink whatever the font.
                    if let Some(riser) = riser {
                        spans.push(
                            iced::widget::text::Span::new("\u{e000}\u{200b}")
                                .font(RISER)
                                .size(*riser),
                        );
                    }
                }
                Some(_) => {}
                None => spans.push(part),
            }
        });
        // Anchors after the line's last character, or on a line with none.
        spans.extend(
            anchors[anchor..]
                .iter()
                .map(|(_, _, id)| anchor_marker(*id)),
        );
        // Every edit to the rendered text, in order: an object's text replaced by its
        // glyph (and riser), and a marker inserted where an anchor stands, ahead of a
        // glyph starting at the same offset.
        let mut edits: Vec<(Range<usize>, usize)> = replacements
            .iter()
            .map(|(range, _, _, riser)| (range.clone(), rendered_len(*riser)))
            .chain(anchors.iter().map(|(at, ..)| (*at..*at, MARKER_LEN)))
            .collect();
        edits.sort_by_key(|(range, _)| (range.start, range.end));
        let map = |offset: usize| {
            let mut delta: isize = 0;
            for (range, len) in &edits {
                if offset < range.start {
                    break;
                }
                if offset < range.end {
                    return (range.start as isize + delta) as usize;
                }
                delta += *len as isize - range.len() as isize;
            }
            (offset as isize + delta) as usize
        };
        let mut source = match &rendered.offsets {
            RenderedOffsets::Identity => vec![0, line.text.len()],
            RenderedOffsets::Mapped { source, .. } => source.to_vec(),
        };
        source.extend([0, line.text.len()]);
        for object in objects.iter() {
            source.extend([object.range.start, object.range.end]);
        }
        source.sort_unstable();
        source.dedup();
        let mut points: Vec<(usize, usize)> = source
            .iter()
            .map(|s| (*s, map(rendered.offsets.source_to_rendered(*s))))
            .collect();
        // An anchor's offset maps past its markers; the text before them needs a point
        // at their start as well, or it would no longer map one to one.
        for (index, (at, source_at, _)) in anchors.iter().enumerate() {
            if index > 0 && anchors[index - 1].0 == *at {
                continue;
            }
            let markers = anchors[index..]
                .iter()
                .take_while(|(next, ..)| next == at)
                .count();
            points.push((*source_at, map(*at) - markers * MARKER_LEN));
        }
        points.sort_unstable();
        points.dedup();
        // Both sides must ascend for the lookups to bisect them. A source offset that a
        // concealment folded onto an anchor's could break that; it loses its point.
        let mut last = 0;
        points.retain(|&(_, at)| {
            let ascending = at >= last;
            last = last.max(at);
            ascending
        });
        let (source, offsets): (Vec<_>, Vec<_>) = points.into_iter().unzip();
        rendered.offsets = RenderedOffsets::Mapped {
            identity_prefix: 0,
            source: source.into(),
            rendered: offsets.into(),
        };
        rendered.spans = Rc::new(spans);
        rendered
    }

    /// Re-views a line's objects against their retained mounts: the per-layout
    /// work a cached paragraph still owes them, since bound props resolve in
    /// the factory. Returns whether every active object kept its mount at the
    /// same size, in which case the paragraph that placed their glyphs is still
    /// right and need not be shaped again. An anchor's size is not in the
    /// paragraph (its marker is the same whatever its box), so only its mount
    /// matters.
    pub fn refresh(
        &mut self,
        line: &StyledLine,
        line_number: usize,
        resolver: Option<&Resolver>,
        renderer: &R,
        width: f32,
        metrics: &ObjectMetrics,
    ) -> bool {
        let Some(objects) = &line.objects else {
            return true;
        };
        let Some(factory) = resolver.and_then(Resolver::factory::<M, T, R>) else {
            return true;
        };
        let mut stable = true;
        for object in objects.iter() {
            let key = (line_number, object.id);
            if !object.owner.active() {
                stable &= !self.mounts.contains_key(&key);
                continue;
            }
            let Some(element) = factory(object, metrics) else {
                stable &= self.mounts.remove(&key).is_none();
                continue;
            };
            let Some(mount) = self.mounts.get_mut(&key) else {
                stable = false;
                continue;
            };
            mount.tree.diff(&element);
            mount.element = element;
            mount.used = true;
            mount.metrics = *metrics;
            let node = mount.element.as_widget_mut().layout(
                &mut mount.tree,
                renderer,
                &object_limits(width),
            );
            stable &= mount.anchor || node.size() == mount.node.size();
            mount.node = node;
            // A placement's children come and go without touching its paragraph.
            mount.outset = paint_reach(object, metrics, mount.node.size());
        }
        stable
    }

    /// Moves each mount to its glyph or marker in the laid-out paragraph. An anchor
    /// hangs from the top of the text line its marker stands on: in a visual line taller
    /// than one text line (one holding a [`Baseline::LastRow`] object), the last one,
    /// where the riser put the text and a terminal leaves its cursor after an image. A
    /// marker left at the end of a full line goes where the text after it wrapped to
    /// ([`wrapped_marker`]).
    pub fn position<P: Paragraph>(
        &mut self,
        paragraph: &P,
        line_number: usize,
        spans: &[iced::widget::text::Span<'static, SpanMetadata>],
        origin: Point,
    ) {
        for (index, span) in spans.iter().enumerate() {
            if let Some(id) = span.link.and_then(|m| m.object)
                && let Some(mount) = self.mounts.get_mut(&(line_number, id))
                && let Some(&marked) = paragraph.span_bounds(index).first()
            {
                let bounds = if mount.anchor {
                    wrapped_marker(
                        paragraph,
                        index,
                        spans.len(),
                        marked,
                        mount.metrics.cell_width,
                    )
                    .unwrap_or(marked)
                } else {
                    marked
                };
                let height = mount.node.size().height;
                // A placement's box is exactly its rows, so anchoring it at the top
                // only differs from centring when rounding does.
                let down = match mount.baseline {
                    _ if mount.anchor => (bounds.height - mount.metrics.line_height).max(0.0),
                    Baseline::Centered => (bounds.height - height).max(0.0) / 2.0,
                    Baseline::LastRow => 0.0,
                };
                mount
                    .node
                    .move_to_mut(Point::new(origin.x + bounds.x, origin.y + bounds.y + down));
                mount.positioned = true;
            }
        }
    }
    pub fn finish(&mut self) {
        self.mounts.retain(|_, m| m.used);
    }

    /// Draws the objects over the pane's text (every one but a placement with a
    /// negative z-index), in a layer of their own, after the text.
    pub fn draw(
        &self,
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.draw_pass(Pass::Over, renderer, theme, style, layout, cursor, viewport);
    }

    /// Draws the objects of one of the two passes under the pane's text, in a layer
    /// of their own, and returns whether any was in view. Layers paint in the order
    /// they open, so the pane calls [`Under::Backgrounds`] before it fills its rows'
    /// background colours and [`Under::Text`] after them, before the text; and when
    /// either returns `true`, whatever the pane draws after it goes in a layer opened
    /// later, or it would paint underneath.
    #[allow(
        clippy::too_many_arguments,
        reason = "the widget draw signature the pane forwards, plus the pass"
    )]
    pub(crate) fn draw_under(
        &self,
        under: Under,
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) -> bool {
        self.draw_pass(
            Pass::Under(under),
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        )
    }

    /// Draws the positioned mounts of `pass` that reach the viewport (a mount is
    /// culled by its box grown by its outset), line by line, each line's in
    /// ascending [`stacking`] order. Returns whether any was drawn.
    #[allow(
        clippy::too_many_arguments,
        reason = "the widget draw signature the pane forwards, plus the pass"
    )]
    fn draw_pass(
        &self,
        pass: Pass,
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) -> bool {
        let Some(clip) = layout.bounds().intersection(viewport) else {
            return false;
        };
        let origin = Vector::new(layout.bounds().x, layout.bounds().y);
        let mut visible: Vec<_> = self
            .mounts
            .iter()
            .filter(|(_, mount)| mount.positioned && mount.pass() == pass)
            .filter_map(|(&(line, _), mount)| {
                let bounds = Layout::with_offset(origin, &mount.node).bounds();
                bounds
                    .expand(mount.outset)
                    .intersects(&clip)
                    .then_some((line, mount, bounds))
            })
            .collect();
        if visible.is_empty() {
            return false;
        }
        // Stable: the map's order, line then creation, breaks ties.
        visible.sort_by_key(|(line, mount, _)| (*line, mount.order));
        renderer.with_layer(clip, |renderer| {
            for (index, (_, mount, bounds)) in visible.iter().enumerate() {
                let child = Layout::with_offset(origin, &mount.node);
                let draw = |renderer: &mut R| {
                    mount.element.as_widget().draw(
                        &mount.tree,
                        renderer,
                        theme,
                        style,
                        child,
                        cursor,
                        &clip,
                    );
                };
                // A widget may crop through a layer of its own, and a layer paints
                // above everything drawn into the one around it: so a mount drawn
                // after one whose box overlaps its own takes a layer too, and the
                // order holds. A layer is not free (in iced_wgpu an image layer is
                // a draw call of its own), so neighbours that only reach each
                // other by their outsets share the pass's layer; what a placement
                // draws beyond its box (its relative placements) is not looked
                // into, so one of those overlapping a later mount can paint over
                // it.
                if visible[..index]
                    .iter()
                    .any(|(_, _, earlier)| earlier.intersects(bounds))
                {
                    renderer.with_layer(clip, draw);
                } else {
                    draw(renderer);
                }
            }
        });
        true
    }

    /// Each mount's line and object id with its box in the pane's coordinates
    /// as of the last layout, or `None` when that layout did not position it.
    #[cfg(test)]
    pub(crate) fn boxes(&self) -> Vec<((usize, u64), Option<Rectangle>)> {
        self.mounts
            .iter()
            .map(|(key, mount)| (*key, mount.positioned.then(|| mount.node.bounds())))
            .collect()
    }
    // Keep the same event context as iced::Widget::update.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &R,
        clipboard: &mut dyn advanced::Clipboard,
        shell: &mut advanced::Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let Some(clip) = layout.bounds().intersection(viewport) else {
            return;
        };
        let cursor = if cursor.is_over(clip) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        let window_event = matches!(event, Event::Window(_));
        for mount in self.mounts.values_mut().rev() {
            if !mount.positioned {
                continue;
            }
            // An anchor lies over text the reader selects and clicks
            // through: it is inert to the mouse (and the keyboard), but sees
            // the window's events, since an animated placement asks for its
            // next frame on each redraw request.
            if mount.anchor && !window_event {
                continue;
            }
            let child = Layout::with_offset(
                Vector::new(layout.bounds().x, layout.bounds().y),
                &mount.node,
            );
            if !child.bounds().expand(mount.outset).intersects(&clip) {
                continue;
            }
            let cursor = if mount.anchor {
                mouse::Cursor::Unavailable
            } else {
                cursor
            };
            mount.element.as_widget_mut().update(
                &mut mount.tree,
                event,
                child,
                cursor,
                renderer,
                clipboard,
                shell,
                &clip,
            );
            if shell.is_event_captured() {
                break;
            }
        }
    }
    pub fn interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &R,
    ) -> Option<mouse::Interaction> {
        if !cursor.is_over(layout.bounds().intersection(viewport).unwrap_or_default()) {
            return None;
        }
        self.mounts
            .values()
            .rev()
            .filter(|m| m.positioned && !m.anchor)
            .find_map(|m| {
                let child =
                    Layout::with_offset(Vector::new(layout.bounds().x, layout.bounds().y), &m.node);
                cursor.is_over(child.bounds()).then(|| {
                    m.element
                        .as_widget()
                        .mouse_interaction(&m.tree, child, cursor, viewport, renderer)
                })
            })
    }
    pub fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &R,
        operation: &mut dyn advanced::widget::Operation,
    ) {
        for mount in self.mounts.values_mut().filter(|m| m.positioned) {
            mount.element.as_widget_mut().operate(
                &mut mount.tree,
                Layout::with_offset(
                    Vector::new(layout.bounds().x, layout.bounds().y),
                    &mount.node,
                ),
                renderer,
                operation,
            );
        }
    }
    pub fn overlay<'a>(
        &'a mut self,
        layout: Layout<'_>,
        renderer: &R,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<advanced::overlay::Element<'a, M, T, R>> {
        let overlays: Vec<_> = self
            .mounts
            .values_mut()
            .filter(|mount| mount.positioned)
            .filter_map(|mount| {
                let child = Layout::with_offset(
                    Vector::new(layout.bounds().x, layout.bounds().y),
                    &mount.node,
                );
                if !child.bounds().intersects(viewport) {
                    return None;
                }
                mount.element.as_widget_mut().overlay(
                    &mut mount.tree,
                    child,
                    renderer,
                    viewport,
                    translation,
                )
            })
            .collect();
        (!overlays.is_empty()).then(|| advanced::overlay::Group::with_children(overlays).overlay())
    }
}
