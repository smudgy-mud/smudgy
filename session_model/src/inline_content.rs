//! Optional presentation attached to terminal text. Native resources are opaque;
//! model transforms do not depend on a renderer or a script runtime.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use web_time::Instant;

use crate::line_operation::{LineOperation, SpliceRun};
use crate::{Style, StyledLine};

pub const MAX_INLINE_OBJECTS: usize = 64;

pub const MAX_INLINE_DECORATIONS: usize = 64;
pub const MAX_INLINE_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_EFFECT_OUTSET: u16 = 2048;

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// Finite, renderer-independent parameters. Duration zero means continuous playback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEffect {
    pub shader: Arc<crate::text_shader::ShaderEffect>,
    pub duration_ms: u32,
    pub outset: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineDecoration {
    pub id: u64,
    pub range: Range<usize>,
    pub effect: TextEffect,
    pub started: Instant,
    /// Engine retirement disables existing visual effects without removing their text.
    pub owner: InlineOwner,
}

#[derive(Debug, Clone)]
pub struct InlineOwner(pub Arc<std::sync::atomic::AtomicBool>);

impl Default for InlineOwner {
    fn default() -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(true)))
    }
}

impl PartialEq for InlineOwner {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for InlineOwner {}

impl InlineOwner {
    pub fn active(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn retire(&self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

impl InlineDecoration {
    pub fn new(range: Range<usize>, effect: TextEffect, owner: InlineOwner) -> Self {
        Self {
            id: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            range,
            effect,
            started: Instant::now(),
            owner,
        }
    }

    pub fn instantiate(&self, offset: usize) -> Self {
        Self::new(
            self.range.start + offset..self.range.end + offset,
            self.effect.clone(),
            self.owner.clone(),
        )
    }

    pub fn animated(&self, now: Instant) -> bool {
        self.owner.active()
            && (self.effect.duration_ms == 0
                || now.saturating_duration_since(self.started)
                    < Duration::from_millis(u64::from(self.effect.duration_ms)))
            && (self.effect.shader.animated
                || self.effect.duration_ms > 0
                || now.saturating_duration_since(self.started)
                    < Duration::from_millis(u64::from(self.effect.shader.fade_in_ms)))
    }

    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.started);
        (self.owner.active()
            && (self.effect.duration_ms == 0
                || elapsed < Duration::from_millis(u64::from(self.effect.duration_ms))
                || self.effect.shader.hold))
            .then_some(elapsed)
    }
}

/// Where the text beside an inline object taller than its row sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Baseline {
    /// The row's text keeps its baseline centred in the row, and the object
    /// is centred on it: a script widget sits in the middle of its line.
    #[default]
    Centered,
    /// The text sits on the object's last row, where a terminal leaves the
    /// cursor after an image; the object is anchored at the row's top.
    LastRow,
}

/// Which pass draws an inline object relative to the row's text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Drawn after the text, over it: every script widget, and a terminal
    /// placement with a zero or positive z-index.
    #[default]
    Over,
    /// Drawn before the text, under it: a terminal placement with a negative
    /// z-index, which the protocol says text is rendered on top of. Such an
    /// object never consumes cells (it is an anchor with an empty range), so
    /// the text it sits under is the text that follows it on the row.
    Under,
}

/// A native attachment. The model owns only opaque, thread-safe lifetime data;
/// the host that created it supplies its renderer implementation.
///
/// An *anchor* is an object with an empty range (`range.start == range.end`):
/// it consumes no cells and is drawn from the cell at its position, over the
/// rows that follow, with `paint_outset` saying how far its box can reach.
#[derive(Clone)]
pub struct InlineObject {
    pub id: u64,
    pub range: Range<usize>,
    pub started: Instant,
    pub paint_outset: u16,
    /// How text shares the row with the object when the object is taller.
    pub baseline: Baseline,
    /// Whether the object is drawn under or over the row's text.
    pub layer: Layer,
    /// The stacking order among objects drawn on one line: ascending `z`,
    /// ties broken by the host in creation order (a terminal placement's
    /// image id, per the graphics protocol). Zero for everything but a
    /// terminal placement that asked for one.
    pub z: i32,
    pub resource: Arc<dyn std::any::Any + Send + Sync>,
    pub owner: InlineOwner,
}
impl std::fmt::Debug for InlineObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineObject")
            .field("id", &self.id)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}
impl PartialEq for InlineObject {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.range == other.range && self.owner == other.owner
    }
}
impl Eq for InlineObject {}
impl InlineObject {
    pub fn new(
        range: Range<usize>,
        resource: Arc<dyn std::any::Any + Send + Sync>,
        owner: InlineOwner,
    ) -> Self {
        Self {
            id: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            range,
            resource,
            owner,
            started: Instant::now(),
            paint_outset: 0,
            baseline: Baseline::default(),
            layer: Layer::default(),
            z: 0,
        }
    }
    pub fn instantiate(&self, offset: usize) -> Self {
        Self {
            paint_outset: self.paint_outset,
            baseline: self.baseline,
            layer: self.layer,
            z: self.z,
            ..Self::new(
                self.range.start + offset..self.range.end + offset,
                self.resource.clone(),
                self.owner.clone(),
            )
        }
    }
}

pub fn remap_objects(
    objects: &Option<Arc<Vec<InlineObject>>>,
    begin: usize,
    end: usize,
    inserted: usize,
) -> Option<Arc<Vec<InlineObject>>> {
    let objects = objects.as_ref()?;
    let result: Vec<_> = objects
        .iter()
        .filter_map(|object| {
            let mut object = object.clone();
            if object.range.end <= begin {
                Some(object)
            } else if object.range.start >= end {
                object.range = object.range.start - end + begin + inserted
                    ..object.range.end - end + begin + inserted;
                Some(object)
            } else {
                None
            }
        })
        .collect();
    (!result.is_empty()).then(|| Arc::new(result))
}

/// Font choices stay separate from VT styles and allocate only for authored spans.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct InlineFontOptions {
    pub font_size: Option<u16>,
    pub font_weight: Option<u16>,
    pub font_style: Option<InlineFontStyle>,
    pub font_face: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InlineFontStyle {
    Normal,
    Italic,
    Oblique,
}

impl InlineFontOptions {
    pub fn inherit(&self, parent: &Self) -> Self {
        Self {
            font_size: self.font_size.or(parent.font_size),
            font_weight: self.font_weight.or(parent.font_weight),
            font_style: self.font_style.or(parent.font_style),
            font_face: self.font_face.clone().or_else(|| parent.font_face.clone()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineFont {
    pub range: Range<usize>,
    pub options: InlineFontOptions,
}
impl InlineFont {
    pub fn shifted(&self, offset: usize) -> Self {
        Self {
            range: self.range.start + offset..self.range.end + offset,
            options: self.options.clone(),
        }
    }
}
/// Retain the font of untouched text on either side of a splice.
pub fn remap_fonts(
    fonts: &Option<Arc<Vec<InlineFont>>>,
    begin: usize,
    end: usize,
    inserted: usize,
) -> Option<Arc<Vec<InlineFont>>> {
    let mut result = Vec::new();
    for font in fonts.as_ref()?.iter() {
        if font.range.start < begin {
            let stop = font.range.end.min(begin);
            result.push(InlineFont {
                range: font.range.start..stop,
                options: font.options.clone(),
            });
        }
        if font.range.end > end {
            let start = font.range.start.max(end);
            result.push(InlineFont {
                range: start - end + begin + inserted..font.range.end - end + begin + inserted,
                options: font.options.clone(),
            });
        }
    }
    (!result.is_empty()).then(|| Arc::new(result))
}

/// Immutable authored content, reusable between ordinary widgets and terminal splices.
#[derive(Debug, Clone)]
pub struct InlineContent {
    pub runs: Arc<Vec<SpliceRun>>,
    pub decorations: Arc<Vec<InlineDecoration>>,
    pub objects: Arc<Vec<InlineObject>>,
    pub fonts: Arc<Vec<InlineFont>>,
}

impl InlineContent {
    /// Snapshot a text range, retaining every style, link and font boundary.
    /// Existing effects are deliberately omitted so a new effect can wrap the
    /// original text. Embedded widgets cannot be captured as text.
    ///
    /// # Errors
    /// Rejects invalid byte ranges, embedded widgets and oversized fragments.
    pub fn from_line_range(line: &StyledLine, range: Range<usize>) -> Result<Self, &'static str> {
        let text = line
            .text
            .get(range.clone())
            .ok_or("invalid highlight byte range")?;
        if text.len() > MAX_INLINE_TEXT_BYTES {
            return Err("highlight fragment exceeds its size limit");
        }
        if line
            .objects
            .iter()
            .flat_map(|objects| objects.iter())
            .any(|object| object.range.start < range.end && object.range.end > range.start)
        {
            return Err("highlight callbacks cannot wrap embedded widgets");
        }
        let mut boundaries = vec![range.start, range.end];
        for (begin, end) in line
            .spans
            .iter()
            .map(|span| (span.begin_pos, span.end_pos))
            .chain(line.links.iter().map(|link| (link.begin_pos, link.end_pos)))
        {
            boundaries.extend(
                [begin, end]
                    .into_iter()
                    .filter(|pos| range.start < *pos && *pos < range.end),
            );
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        if boundaries.len() > 4097 {
            return Err("highlight fragment exceeds its run limit");
        }
        let runs = boundaries
            .windows(2)
            .map(|pair| {
                let (begin, end) = (pair[0], pair[1]);
                let span = line
                    .spans
                    .get(line.spans.partition_point(|span| span.end_pos <= begin));
                let style = span
                    .filter(|span| span.begin_pos <= begin)
                    .map_or(Style::DEFAULT, |span| span.style);
                let link = line
                    .links
                    .get(line.links.partition_point(|link| link.end_pos <= begin))
                    .filter(|link| link.begin_pos <= begin);
                SpliceRun {
                    text: line.text[begin..end].to_owned(),
                    fg: Some(style.fg),
                    bg: Some(style.bg),
                    attributes: style.attributes.into(),
                    link: link.map(|link| crate::StyledLink {
                        action: link.action.clone(),
                        tooltip: link.tooltip.clone(),
                        style: link.style.clone(),
                    }),
                }
            })
            .collect();
        let fonts: Vec<_> = line
            .fonts
            .iter()
            .flat_map(|fonts| fonts.iter())
            .filter_map(|font| {
                let begin = font.range.start.max(range.start);
                let end = font.range.end.min(range.end);
                (begin < end).then(|| InlineFont {
                    range: begin - range.start..end - range.start,
                    options: font.options.clone(),
                })
            })
            .collect();
        if fonts.len() > 4096 {
            return Err("highlight fragment exceeds its font limit");
        }
        Ok(Self {
            runs: Arc::new(runs),
            fonts: Arc::new(fonts),
            decorations: Arc::default(),
            objects: Arc::default(),
        })
    }

    pub fn text_len(&self) -> usize {
        self.runs.iter().map(|run| run.text.len()).sum()
    }

    pub fn line(&self, style: Style) -> StyledLine {
        let base = Arc::new(StyledLine::from_linked_runs(&[], style));
        let operation = LineOperation::Splice {
            runs: self.runs.clone(),
            begin: 0,
            end: 0,
        };
        let mut line = (*operation.apply(&base)).clone();
        if !self.decorations.is_empty() {
            line.decorations = Some(Arc::new(
                self.decorations
                    .iter()
                    .map(|effect| effect.instantiate(0))
                    .collect(),
            ));
        }
        if !self.objects.is_empty() {
            line.objects = Some(Arc::new(
                self.objects
                    .iter()
                    .map(|object| object.instantiate(0))
                    .collect(),
            ));
        }
        if !self.fonts.is_empty() {
            line.fonts = Some(self.fonts.clone());
        }
        line
    }
}

/// Synchronous, single-use exchange between independently injected core and widget ops.
/// Values stay native; a JS object cannot forge a resolved callback or widget handle.
#[derive(Debug, Default)]
pub struct InlineContentExchange {
    generation: u32,
    pending: Option<(u32, InlineExchangeValue)>,
}

#[derive(Debug)]
enum InlineExchangeValue {
    Content(Arc<InlineContent>),
    Effect(InlineDecoration),
}

impl InlineContentExchange {
    pub fn put(&mut self, content: Arc<InlineContent>) -> Result<u32, &'static str> {
        self.publish(InlineExchangeValue::Content(content))
    }

    /// A validated effect prototype, with no text or Span allocation.
    pub fn put_effect(&mut self, effect: InlineDecoration) -> Result<u32, &'static str> {
        self.publish(InlineExchangeValue::Effect(effect))
    }

    fn publish(&mut self, value: InlineExchangeValue) -> Result<u32, &'static str> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("inline content exchange exhausted")?;
        self.pending = Some((self.generation, value));
        Ok(self.generation)
    }

    pub fn take(&mut self, generation: u32) -> Result<Arc<InlineContent>, &'static str> {
        match self.consume(generation)? {
            InlineExchangeValue::Content(content) => Ok(content),
            InlineExchangeValue::Effect(_) => {
                Err("expected inline content, received a shader effect")
            }
        }
    }

    pub fn take_effect(&mut self, generation: u32) -> Result<InlineDecoration, &'static str> {
        match self.consume(generation)? {
            InlineExchangeValue::Effect(effect) => Ok(effect),
            InlineExchangeValue::Content(_) => {
                Err("expected a shader effect, received inline content")
            }
        }
    }

    fn consume(&mut self, generation: u32) -> Result<InlineExchangeValue, &'static str> {
        let pending = self
            .pending
            .take()
            .ok_or("inline content has already been consumed")?;
        if pending.0 != generation {
            return Err("stale inline content exchange");
        }
        Ok(pending.1)
    }
}

/// Edits outside an effect shift/preserve it; overlapping edits retire it.
pub fn remap_decorations(
    decorations: &Option<Arc<Vec<InlineDecoration>>>,
    begin: usize,
    end: usize,
    inserted: usize,
) -> Option<Arc<Vec<InlineDecoration>>> {
    let decorations = decorations.as_ref()?;
    let remapped: Vec<_> = decorations
        .iter()
        .filter_map(|decoration| {
            let mut result = decoration.clone();
            if result.range.end <= begin {
                Some(result)
            } else if result.range.start >= end {
                result.range = result.range.start - end + begin + inserted
                    ..result.range.end - end + begin + inserted;
                Some(result)
            } else {
                None
            }
        })
        .collect();
    (!remapped.is_empty()).then(|| Arc::new(remapped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect() -> InlineDecoration {
        InlineDecoration::new(
            3..6,
            TextEffect {
                shader: Arc::new(crate::text_shader::ShaderEffect {
                    pane: false,
                    scale: Default::default(),
                    capture_scale: Default::default(),
                    fade_in_ms: 0,
                    fade_out_ms: 0,
                    replace: false,
                    hold: false,
                    animated: true,
                    shader: crate::text_shader::Shader::compile(
                        "identity.wgsl",
                        "fn effect(p: vec2f) -> vec4f { return sampleText(p); }",
                    )
                    .unwrap(),
                    uniforms: vec![0; 16].into(),
                }),
                duration_ms: 0,
                outset: 24,
            },
            InlineOwner::default(),
        )
    }

    #[test]
    fn edits_preserve_identity_outside_the_effect_and_retire_overlap() {
        let item = effect();
        let original = Some(Arc::new(vec![item.clone()]));
        let shifted = remap_decorations(&original, 0, 2, 4).unwrap();
        assert_eq!(shifted[0].range, 5..8);
        assert_eq!(shifted[0].id, item.id);
        assert!(remap_decorations(&original, 4, 4, 1).is_none());
        assert!(remap_decorations(&original, 3, 6, 3).is_none());
        assert_eq!(
            remap_decorations(&original, 6, 6, 3).unwrap()[0].range,
            3..6
        );
        assert_eq!(
            remap_decorations(&original, 3, 3, 1).unwrap()[0].range,
            4..7
        );
    }

    #[test]
    fn instances_have_independent_identity_and_retire_with_owner() {
        let item = effect();
        let mounted = item.instantiate(10);
        assert_ne!(mounted.id, item.id);
        assert_eq!(mounted.range, 13..16);
        assert!(mounted.elapsed(Instant::now()).is_some());
        item.owner.retire();
        assert!(mounted.elapsed(Instant::now()).is_none());
    }

    #[test]
    fn an_object_instance_keeps_its_baseline_policy() {
        let mut object = InlineObject::new(0..3, Arc::new(()), InlineOwner::default());
        assert_eq!(object.baseline, Baseline::Centered, "script widgets centre");
        assert_eq!((object.layer, object.z), (Layer::Over, 0), "and draw over");
        object.baseline = Baseline::LastRow;
        object.paint_outset = 4;
        let mounted = object.instantiate(2);
        assert_eq!(mounted.range, 2..5);
        assert_eq!(mounted.baseline, Baseline::LastRow);
        assert_eq!(mounted.paint_outset, 4);
    }

    #[test]
    fn an_object_instance_keeps_its_layer_and_z_through_projection_and_joins() {
        let mut anchor = InlineObject::new(3..3, Arc::new(()), InlineOwner::default());
        anchor.layer = Layer::Under;
        anchor.z = -7;
        anchor.paint_outset = 96;
        let mounted = anchor.instantiate(5);
        assert_eq!(mounted.range, 8..8, "an anchor stays empty");
        assert_eq!((mounted.layer, mounted.z), (Layer::Under, -7));
        // The line projection and a fragment join carry the fields too.
        let content = Arc::new(InlineContent {
            fonts: Arc::default(),
            runs: Arc::new(vec![SpliceRun {
                text: "abc".into(),
                fg: None,
                bg: None,
                attributes: Default::default(),
                link: None,
            }]),
            decorations: Arc::default(),
            objects: Arc::new(vec![anchor]),
        });
        let line = Arc::new(content.line(Style::DEFAULT));
        let projected = &line.objects.as_ref().unwrap()[0];
        assert_eq!((projected.layer, projected.z), (Layer::Under, -7));
        let mut fragments = crate::styled_line::LineFragments::default();
        fragments.push(line.clone());
        fragments.push(line);
        let joined = fragments.take_joined().unwrap();
        let objects = joined.objects.as_ref().unwrap();
        assert_eq!(objects.len(), 2);
        assert!(objects.iter().all(|o| o.layer == Layer::Under && o.z == -7));
        assert_eq!(objects[1].range, 6..6);
    }

    #[test]
    fn object_projection_survives_style_edits_and_releases_on_overlapping_text_edit() {
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let object = InlineObject::new(0..6, resource, InlineOwner::default());
        let content = Arc::new(InlineContent {
            fonts: Arc::default(),
            runs: Arc::new(vec![SpliceRun {
                text: "Button".into(),
                fg: None,
                bg: None,
                attributes: Default::default(),
                link: None,
            }]),
            decorations: Arc::new(Vec::new()),
            objects: Arc::new(vec![object]),
        });
        let first = Arc::new(content.line(Style::DEFAULT));
        let second = content.line(Style::DEFAULT);
        assert_ne!(
            first.objects.as_ref().unwrap()[0].id,
            second.objects.as_ref().unwrap()[0].id
        );
        let shifted = first.insert("!", 0, 0, Style::DEFAULT);
        assert_eq!(shifted.objects.as_ref().unwrap()[0].range, 1..7);
        assert_eq!(
            shifted.objects.as_ref().unwrap()[0].id,
            first.objects.as_ref().unwrap()[0].id
        );
        let removed = first.remove(2, 3);
        assert!(removed.objects.is_none());
        assert_eq!(removed.text, "Buton");
        drop((first, second, shifted, content));
        assert!(
            weak.upgrade().is_none(),
            "no registry retains removed widget resources"
        );
    }

    #[test]
    fn fonts_follow_splices_removals_and_concatenation_without_styling_inserted_text() {
        let mut line = StyledLine::new("abCDEfg", vec![]);
        line.fonts = Some(Arc::new(vec![InlineFont {
            range: 2..5,
            options: InlineFontOptions {
                font_size: Some(24),
                ..Default::default()
            },
        }]));
        let inserted = line.insert("!", 3, 3, Style::DEFAULT);
        let fonts = inserted.fonts.as_ref().unwrap();
        assert_eq!(
            fonts.iter().map(|f| f.range.clone()).collect::<Vec<_>>(),
            [2..3, 4..6]
        );
        assert!(fonts.iter().all(|f| f.options.font_size == Some(24)));
        let removed = line.remove(3, 4);
        assert_eq!(removed.text, "abCEfg");
        assert_eq!(
            removed
                .fonts
                .as_ref()
                .unwrap()
                .iter()
                .map(|f| f.range.clone())
                .collect::<Vec<_>>(),
            [2..3, 3..4]
        );
        assert!(line.remove(2, 5).fonts.is_none());
        let mut fragments = crate::styled_line::LineFragments::default();
        fragments.push(Arc::new(line.clone()));
        fragments.push(Arc::new(line));
        let joined = fragments.take_joined().unwrap();
        assert_eq!(
            joined
                .fonts
                .as_ref()
                .unwrap()
                .iter()
                .map(|f| f.range.clone())
                .collect::<Vec<_>>(),
            [2..5, 9..12]
        );
    }

    #[test]
    fn exchange_is_single_use_and_does_not_retain_stale_content() {
        let content = Arc::new(InlineContent {
            fonts: Arc::default(),
            runs: Arc::new(Vec::new()),
            decorations: Arc::new(Vec::new()),
            objects: Arc::new(Vec::new()),
        });
        let mut exchange = InlineContentExchange::default();
        let first = exchange.put(content.clone()).unwrap();
        let second = exchange.put(content).unwrap();
        assert!(exchange.take(first).is_err());
        assert!(exchange.take(second).is_err());
    }

    #[test]
    fn exchange_preserves_effect_identity_and_rejects_payload_type_confusion() {
        let mut exchange = InlineContentExchange::default();
        let original = effect();
        let token = exchange.put_effect(original.clone()).unwrap();
        assert_eq!(exchange.take_effect(token).unwrap(), original);
        assert!(exchange.take_effect(token).is_err());
        let token = exchange.put_effect(effect()).unwrap();
        assert!(exchange.take(token).unwrap_err().contains("shader effect"));
        assert!(exchange.take_effect(token).is_err());
        let content = Arc::new(InlineContent {
            runs: Arc::default(),
            decorations: Arc::default(),
            objects: Arc::default(),
            fonts: Arc::default(),
        });
        let token = exchange.put(content).unwrap();
        assert!(
            exchange
                .take_effect(token)
                .unwrap_err()
                .contains("inline content")
        );
        assert!(exchange.take(token).is_err());
    }
}
