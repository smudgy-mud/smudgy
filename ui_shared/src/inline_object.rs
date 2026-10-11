//! Native widgets occupying atomic object-replacement glyphs in terminal paragraphs.
//! The bundled, ink-free metric font has a one-em advance. Its size reserves the
//! measured widget width exactly; independent line-height reserves its height.
//! Source text never contains these glyphs: copy/search/log use the authored fallback.
use crate::terminal_buffer::{RenderedOffsets, RenderedSpans, SpanMetadata};
use iced::{
    Element, Event, Point, Rectangle, Size, Vector,
    advanced::{
        self, Layout, layout, mouse, renderer,
        text::Paragraph,
        widget::{Tree, tree},
    },
};
use smudgy_session_model::{StyledLine, inline_content::InlineObject};
use std::{any::Any, collections::BTreeMap, rc::Rc};

#[derive(Clone)]
pub struct Resolver(Rc<dyn Any>);
type Factory<M, T, R> = Rc<dyn Fn(&InlineObject) -> Option<Element<'static, M, T, R>>>;
pub fn resolver<M: 'static, T: 'static, R: 'static>(
    f: impl Fn(&InlineObject) -> Option<Element<'static, M, T, R>> + 'static,
) -> Resolver {
    Resolver(Rc::new(Rc::new(f) as Factory<M, T, R>))
}
impl Resolver {
    fn factory<M: 'static, T: 'static, R: 'static>(&self) -> Option<&Factory<M, T, R>> {
        self.0.downcast_ref::<Factory<M, T, R>>()
    }
    /// Builds the element for `object`, or `None` when the resolver was made
    /// for another renderer or the object has nothing to show.
    pub fn resolve<M: 'static, T: 'static, R: 'static>(
        &self,
        object: &InlineObject,
    ) -> Option<Element<'static, M, T, R>> {
        self.factory::<M, T, R>()
            .and_then(|factory| factory(object))
    }
}
const FONT: iced::Font = iced::Font::with_name("Smudgy Inline Object");
fn load_font() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        iced::advanced::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(std::borrow::Cow::Borrowed(include_bytes!(
                "inline_object/metrics.ttf"
            )))
    });
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
    outset: f32,
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

    pub fn prepare(
        &mut self,
        line: &StyledLine,
        line_number: usize,
        mut rendered: RenderedSpans,
        resolver: Option<&Resolver>,
        renderer: &R,
        width: f32,
    ) -> RenderedSpans {
        let Some(objects) = &line.objects else {
            return rendered;
        };
        let Some(factory) = resolver.and_then(Resolver::factory::<M, T, R>) else {
            return rendered;
        };
        let mut replacements = Vec::new();
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
            let Some(element) = factory(object) else {
                continue;
            };
            let mount = self.mounts.entry(key).or_insert_with(|| Mount {
                tree: Tree::new(&element),
                element: iced::widget::Space::new().into(),
                node: layout::Node::new(Size::ZERO),
                used: true,
                outset: f32::from(object.paint_outset),
            });
            mount.tree.diff(&element);
            mount.element = element;
            mount.used = true;
            mount.node = mount.element.as_widget_mut().layout(
                &mut mount.tree,
                renderer,
                &object_limits(width),
            );
            let size = mount.node.size();
            let start = rendered.offsets.source_to_rendered(object.range.start);
            let end = rendered.offsets.source_to_rendered(object.range.end);
            if start < end {
                replacements.push((start..end, object.id, size));
            }
        }
        if replacements.is_empty() {
            return rendered;
        }
        load_font();
        replacements.sort_by_key(|(r, _, _)| r.start);
        let boundaries = replacements.iter().flat_map(|(r, _, _)| [r.start, r.end]);
        let mut spans = Vec::with_capacity(rendered.spans.len() + replacements.len());
        let mut replacement = 0;
        crate::span_cuts::split_at(&rendered.spans, boundaries, |piece, part| {
            while replacement < replacements.len() && replacements[replacement].0.end <= piece.start
            {
                replacement += 1;
            }
            match replacements
                .get(replacement)
                .filter(|(r, _, _)| r.start <= piece.start)
            {
                // The object glyph stands in for the whole of its text; later
                // pieces inside the range are that text and are dropped.
                Some((range, id, size)) if range.start == piece.start => spans.push(
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
                ),
                Some(_) => {}
                None => spans.push(part),
            }
        });
        let map = |offset: usize| {
            let mut delta: isize = 0;
            for (range, _, _) in &replacements {
                if offset < range.start {
                    break;
                }
                if offset < range.end {
                    return (range.start as isize + delta) as usize;
                }
                delta += 3 - range.len() as isize;
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
        let offsets: Vec<_> = source
            .iter()
            .map(|s| map(rendered.offsets.source_to_rendered(*s)))
            .collect();
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
    /// right and need not be shaped again.
    pub fn refresh(
        &mut self,
        line: &StyledLine,
        line_number: usize,
        resolver: Option<&Resolver>,
        renderer: &R,
        width: f32,
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
            let Some(element) = factory(object) else {
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
            let node = mount.element.as_widget_mut().layout(
                &mut mount.tree,
                renderer,
                &object_limits(width),
            );
            stable &= node.size() == mount.node.size();
            mount.node = node;
        }
        stable
    }

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
                && let Some(bounds) = paragraph.span_bounds(index).first()
            {
                let height = mount.node.size().height;
                mount.node.move_to_mut(Point::new(
                    origin.x + bounds.x,
                    origin.y + bounds.y + (bounds.height - height).max(0.0) / 2.0,
                ));
            }
        }
    }
    pub fn finish(&mut self) {
        self.mounts.retain(|_, m| m.used);
    }
    pub fn draw(
        &self,
        renderer: &mut R,
        theme: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Some(clip) = layout.bounds().intersection(viewport) else {
            return;
        };
        renderer.with_layer(clip, |renderer| {
            for mount in self.mounts.values() {
                let child = Layout::with_offset(
                    Vector::new(layout.bounds().x, layout.bounds().y),
                    &mount.node,
                );
                if !child.bounds().expand(mount.outset).intersects(&clip) {
                    continue;
                }
                mount.element.as_widget().draw(
                    &mount.tree,
                    renderer,
                    theme,
                    style,
                    child,
                    cursor,
                    &clip,
                );
            }
        });
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
        for mount in self.mounts.values_mut().rev() {
            let child = Layout::with_offset(
                Vector::new(layout.bounds().x, layout.bounds().y),
                &mount.node,
            );
            if !child.bounds().expand(mount.outset).intersects(&clip) {
                continue;
            }
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
        self.mounts.values().rev().find_map(|m| {
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
        for mount in self.mounts.values_mut() {
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
