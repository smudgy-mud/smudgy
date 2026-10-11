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

/// A native attachment. The model owns only opaque, thread-safe lifetime data;
/// the host that created it supplies its renderer implementation.
#[derive(Clone)]
pub struct InlineObject {
    pub id: u64,
    pub range: Range<usize>,
    pub started: Instant,
    pub paint_outset: u16,
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
        }
    }
    pub fn instantiate(&self, offset: usize) -> Self {
        Self {
            paint_outset: self.paint_outset,
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
    pending: Option<(u32, Arc<InlineContent>)>,
}

impl InlineContentExchange {
    pub fn put(&mut self, content: Arc<InlineContent>) -> Result<u32, &'static str> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("inline content exchange exhausted")?;
        self.pending = Some((self.generation, content));
        Ok(self.generation)
    }

    pub fn take(&mut self, generation: u32) -> Result<Arc<InlineContent>, &'static str> {
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
}
