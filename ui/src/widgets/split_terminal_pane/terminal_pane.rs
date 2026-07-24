use std::{
    cell::{Ref, RefCell},
    rc::Rc,
};

use crate::terminal_buffer::{LinkClickEvent, TerminalBuffer};
use iced::{
    Background, Event, Pixels, Rectangle,
    advanced::{
        self, Layout, Widget, clipboard,
        graphics::core::keyboard,
        layout, mouse,
        renderer::{self, Quad},
        text::{self, Paragraph},
        widget::{Tree, tree},
    },
    alignment, touch,
    widget::text::LineHeight,
};

mod spans;

use crate::terminal_buffer::selection::{BufferPosition, LineSelection, Selection};
use spans::Spans;

type Link = ();

/// 100 '0's shaped once per prefs generation to measure the monospace cell
/// advance for the column-based line-length clamp.
const ADVANCE_PROBE: &str = "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone)]
struct ParagraphCache<P: text::Paragraph> {
    spans: Spans<Link>,
    paragraph: P,
    max_valid_width: f32,
    selection: LineSelection,
    /// The prefs generation this paragraph was shaped with; a mismatch is a
    /// cache miss (font/size/palette changes rebuild paragraphs).
    generation: u64,
}

/// State specific to the TerminalPane widget instance.
#[derive(Debug, Clone)]
pub(super) struct State<P: text::Paragraph> {
    pub last_line_number: usize,
    cache: Vec<ParagraphCache<P>>,
    pub is_focused: bool,
    /// Measured `(prefs generation, monospace cell advance)`.
    pub advance: Option<(u64, f32)>,
    /// Keyboard modifiers as of the last change event, reported with link clicks.
    pub modifiers: keyboard::Modifiers,
    /// The buffer cell the press landed on, kept while the pointer stays on it. A
    /// release on the same cell is a click (fires links); any divergence — a drag,
    /// or content scrolling under a stationary cursor — clears it. Per-pane state,
    /// NOT derived from the shared `Selection`: a sibling pane processes the same
    /// release first and flips `Selecting` → `Selected`, so selection state alone
    /// cannot tell this pane a click just ended on it.
    pub pressed_cell: Option<BufferPosition>,
}

impl<P: text::Paragraph> Default for State<P> {
    fn default() -> Self {
        Self {
            last_line_number: 0,
            cache: Vec::new(),
            is_focused: false,
            advance: None,
            modifiers: keyboard::Modifiers::default(),
            pressed_cell: None,
        }
    }
}

impl<P: text::Paragraph> State<P> {
    pub(super) fn hit_test(&self, bounds: Rectangle, point: iced::Point) -> Option<BufferPosition> {
        let mut line_top = bounds.height;

        for (line, offset) in self.cache.iter().zip(0..) {
            let line_number = self.last_line_number - offset;
            let line_bottom = line_top;
            line_top -= line.paragraph.min_height();

            if point.y >= line_top && point.y < line_bottom {
                let point_in_paragraph = iced::Point::new(point.x, point.y - line_top);
                return match line.paragraph.hit_test(point_in_paragraph) {
                    Some(hit) => Some(BufferPosition {
                        line: line_number,
                        column: hit.cursor(),
                    }),
                    None => {
                        // The point is not in the paragraph, but it is to the left or right of it, let's snap to it
                        if point_in_paragraph.x < 0.0 {
                            Some(BufferPosition {
                                line: line_number,
                                column: 0,
                            })
                        } else {
                            // The point is to the right of the paragraph, but we need to figure out which line it is on
                            // Let's find the last span that is to the left of the point

                            (0..line.spans.spans().len())
                                .filter_map(|idx| {
                                    line.paragraph
                                        .span_bounds(idx)
                                        .iter()
                                        .filter(|span_bounds| {
                                            span_bounds.y <= point_in_paragraph.y
                                                && span_bounds.y + span_bounds.height
                                                    > point_in_paragraph.y
                                        })
                                        .reduce(|acc, item| if acc.x > item.x { acc } else { item })
                                        .map(|span_bounds| (*span_bounds, idx))
                                })
                                .reduce(|acc, item| if acc.0.x > item.0.x { acc } else { item })
                                .map(|(_, idx)| BufferPosition {
                                    line: line_number,
                                    column: line
                                        .spans
                                        .spans()
                                        .iter()
                                        .take(idx + 1)
                                        .fold(0, |acc, span| acc + span.text.len()),
                                })
                        }
                    }
                };
            }
        }
        None
    }
}

pub struct TerminalPane<'a> {
    terminal_buffer: Ref<'a, TerminalBuffer>,
    selection: Rc<RefCell<Selection>>,
    last_line_number: Option<usize>,
    /// Called with the action of a clicked link span. A plain callback rather than a
    /// shell message so the pane stays `Message`-agnostic (it is instantiated under
    /// several message types); the handler sends the resulting runtime action itself.
    on_link: Option<Rc<dyn Fn(LinkClickEvent)>>,
}

impl<'a> TerminalPane<'a> {
    pub fn new(buffer: Ref<'a, TerminalBuffer>, selection: Rc<RefCell<Selection>>) -> Self {
        log::debug!("TerminalPane::new() called");
        Self {
            terminal_buffer: buffer,
            selection,
            last_line_number: None,
            on_link: None,
        }
    }

    pub fn last_line_number(mut self, last_line_number: usize) -> Self {
        self.last_line_number = Some(last_line_number);
        self
    }

    pub fn on_link(mut self, on_link: Option<Rc<dyn Fn(LinkClickEvent)>>) -> Self {
        self.on_link = on_link;
        self
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer> for TerminalPane<'a>
where
    Renderer: text::Renderer<Font = iced::Font> + 'a,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'a,
{
    fn size(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Fill, iced::Length::Fill)
    }

    fn size_hint(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Fill, iced::Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph>::default())
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        let selection = self.selection.borrow();
        let prefs = crate::prefs::current();

        // The measured width of one monospace cell at the current font/size,
        // measured once per prefs generation. It clamps the wrap width when a
        // maximum line length is configured, and the parent split pane reads
        // it to derive the character grid NAWS reports.
        let advance = match state.advance {
            Some((generation, advance)) if generation == prefs.generation => advance,
            _ => {
                let probe = Renderer::Paragraph::with_text(iced::advanced::text::Text {
                    content: ADVANCE_PROBE,
                    bounds: iced::Size::new(f32::INFINITY, f32::INFINITY),
                    size: Pixels(prefs.font_size),
                    font: prefs.font,
                    line_height: LineHeight::Absolute(Pixels(prefs.line_height)),
                    align_x: text::Alignment::Left,
                    align_y: alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::None,
                });
                let advance = probe.min_width() / ADVANCE_PROBE.len() as f32;
                state.advance = Some((prefs.generation, advance));
                advance
            }
        };

        // When a maximum line length (in columns) is configured, clamp the
        // wrap width to `cols * advance`. Text stays left-aligned in the
        // full pane.
        let text_width = match prefs.line_length {
            Some(cols) => limits.max().width.min(f32::from(cols) * advance),
            None => limits.max().width,
        };
        let text_bounds = iced::Size::new(text_width, limits.max().height);

        let mut new_cache: Vec<ParagraphCache<Renderer::Paragraph>> =
            Vec::with_capacity(state.cache.len());

        let mut i = 0;

        let mut available_y = limits.max().height;

        state.last_line_number = self
            .last_line_number
            .unwrap_or(self.terminal_buffer.last_line_number());

        for (line_number, line) in self
            .terminal_buffer
            .iter_rev_with_line_number(self.last_line_number)
        {
            if available_y < 0.0 {
                break;
            }

            // look for a matching cached Paragraph in state.paragraphs[i] or state.paragraphs[i + 1],
            // advancing i by 1 if a match is found; entries shaped under an
            // older prefs generation are always misses
            if let Some(cache) = state.cache.get_mut(i)
                && cache.generation == prefs.generation
            {
                let line_selection = selection.for_line(line_number);

                if cache.selection != line_selection {
                    match line_selection {
                        None => {
                            cache.spans.select_none();
                        }
                        Some((0, usize::MAX)) => {
                            cache.spans.select_all();
                        }
                        Some((from, to)) => {
                            cache.spans.select_range(from, to);
                        }
                    }
                } else if Rc::ptr_eq(&cache.spans.spans(), line.spans()) {
                    i += 1;

                    if text_bounds.width > cache.max_valid_width
                        || text_bounds.width < cache.paragraph.min_bounds().width
                    {
                        cache.paragraph.resize(text_bounds);
                        cache.max_valid_width = text_bounds.width;
                    }

                    new_cache.push(cache.clone());

                    available_y -= cache.paragraph.min_height();
                    continue;
                }
            }

            let line_selection = selection.for_line(line_number);
            let spans = Spans::with_selection(line.spans().clone(), line_selection);

            let spans_vec = spans.spans();

            let text = iced::advanced::text::Text {
                content: Vec::as_ref(&spans_vec),
                bounds: text_bounds,
                size: Pixels(prefs.font_size),
                font: prefs.font,
                line_height: LineHeight::Absolute(Pixels(prefs.line_height)),
                align_x: text::Alignment::Left,
                align_y: alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::WordOrGlyph,
            };

            let paragraph = Renderer::Paragraph::with_spans(text);

            available_y -= paragraph.min_height();

            new_cache.push(ParagraphCache {
                spans,
                paragraph,
                max_valid_width: text_bounds.width,
                selection: line_selection,
                generation: prefs.generation,
            });
        }

        state.cache = new_cache;

        layout::atomic(limits, iced::Length::Fill, iced::Length::Fill)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style_defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        let prefs = crate::prefs::current();

        if let Some(clipped_viewport) = layout.bounds().intersection(viewport) {
            let mut y = layout.bounds().y + layout.bounds().height;
            for cache in state.cache.iter() {
                y -= cache.paragraph.min_height();

                // Span decorations: explicit background quads and link underlines —
                // the same geometry iced's rich_text widget draws (fill_paragraph
                // renders glyphs only). Undecorated spans (the overwhelmingly
                // common case) skip before any span_bounds work.
                for (span_idx, span) in cache.spans.spans().iter().enumerate() {
                    if span.highlight.is_none() && !span.underline {
                        continue;
                    }
                    let regions = cache.paragraph.span_bounds(span_idx);

                    if let Some(highlight) = span.highlight {
                        for region in &regions {
                            let rect = Rectangle {
                                x: layout.bounds().x + region.x,
                                y: region.y + y,
                                width: region.width,
                                height: region.height,
                            };
                            if let Some(bounds) = rect.intersection(&clipped_viewport) {
                                renderer.fill_quad(
                                    Quad {
                                        bounds,
                                        border: highlight.border,
                                        ..Default::default()
                                    },
                                    highlight.background,
                                );
                            }
                        }
                    }

                    if span.underline {
                        // Baseline placement per iced's rich_text: the underline
                        // sits at font size plus half the leading, nudged up by
                        // 8% of the font size.
                        let underline_y = prefs.font_size
                            + (prefs.line_height - prefs.font_size) / 2.0
                            - prefs.font_size * 0.08;
                        for region in &regions {
                            let rect = Rectangle {
                                x: layout.bounds().x + region.x,
                                y: region.y + y + underline_y,
                                width: region.width,
                                height: 1.0,
                            };
                            if let Some(bounds) = rect.intersection(&clipped_viewport) {
                                renderer.fill_quad(
                                    Quad {
                                        bounds,
                                        ..Default::default()
                                    },
                                    span.color.unwrap_or(iced::Color::WHITE),
                                );
                            }
                        }
                    }
                }

                for selected_span_idx in cache.spans.selected().iter() {
                    let span_bounds_list = cache.paragraph.span_bounds(*selected_span_idx);

                    for span_bounds in span_bounds_list.iter() {
                        let span_rect = Rectangle {
                            x: layout.bounds().x + span_bounds.x,
                            y: span_bounds.y + y,
                            width: span_bounds.width,
                            height: span_bounds.height,
                        };
                        if let Some(bounds) = span_rect.intersection(&clipped_viewport) {
                            renderer.fill_quad(
                                Quad {
                                    bounds,
                                    ..Default::default()
                                },
                                Background::Color(prefs.palette.selection),
                            );
                        }
                    }
                }

                renderer.fill_paragraph(
                    &cache.paragraph,
                    iced::Point::new(layout.bounds().x, y),
                    iced::Color::WHITE,
                    clipped_viewport,
                );
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            // Pointer over a link span; text cursor elsewhere. The `has_links` guard
            // keeps linkless sessions (the common case) from paying the per-frame
            // hit test at all.
            if self.on_link.is_some() && self.terminal_buffer.has_links() {
                let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
                if let Some(position) = cursor
                    .position_in(layout.bounds())
                    .and_then(|position| state.hit_test(layout.bounds(), position))
                    && self
                        .terminal_buffer
                        .link_at(position.line, position.column)
                        .is_some()
                {
                    return mouse::Interaction::Pointer;
                }
            }
            mouse::Interaction::Text
        } else {
            mouse::Interaction::Idle
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        clipboard: &mut dyn advanced::Clipboard,
        shell: &mut advanced::Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                let mut selection = self.selection.borrow_mut();

                if let Some(click_position) = cursor.position_in(layout.bounds()) {
                    if let Some(position) = state.hit_test(layout.bounds(), click_position) {
                        state.pressed_cell = Some(position.clone());
                        *selection = Selection::Selecting {
                            origin: position.clone(),
                            from: position.clone(),
                            to: position,
                        };
                        shell.invalidate_layout();
                    }
                    state.is_focused = true;
                    // We don't capture the event here because we want the click input to bubble up, so we can also use it to focus this session's input
                } else {
                    state.is_focused = false;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. }) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();

                // A click is a press and release resolving to the SAME buffer cell
                // (`pressed_cell` survives only while the pointer stays on it): a drag
                // ends elsewhere, and content scrolling under a stationary cursor
                // moves the release onto a different absolute line — neither fires.
                if let Some(pressed) = state.pressed_cell.take()
                    && let Some(on_link) = self.on_link.as_ref()
                    && self.terminal_buffer.has_links()
                    && let Some(position) = cursor
                        .position_in(layout.bounds())
                        .and_then(|position| state.hit_test(layout.bounds(), position))
                    && position == pressed
                    && let Some(action) =
                        self.terminal_buffer.link_at(position.line, position.column)
                {
                    on_link(LinkClickEvent {
                        action,
                        shift: state.modifiers.shift(),
                        ctrl: state.modifiers.control(),
                        alt: state.modifiers.alt(),
                    });
                    // The handler may have staged UI state (the link-trust
                    // confirm dialog slot) rather than publishing a message;
                    // invalidate so it renders this frame, like the
                    // selection updates above.
                    shell.invalidate_layout();
                }

                let mut selection = self.selection.borrow_mut();
                if let Selection::Selecting {
                    origin: _,
                    ref from,
                    ref to,
                } = *selection
                {
                    *selection = Selection::Selected {
                        from: from.clone(),
                        to: to.clone(),
                    };

                    shell.invalidate_layout();
                    // We don't capture the event here because we want the click input to bubble up, so we can also use it to focus this session's input
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position: _ }) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();

                // The pointer left the pressed cell (or the pane): whatever ends this
                // press, it is a drag, not a click.
                if state.pressed_cell.is_some() {
                    let hit = cursor
                        .position_from(layout.position())
                        .and_then(|position| state.hit_test(layout.bounds(), position));
                    if hit.as_ref() != state.pressed_cell.as_ref() {
                        state.pressed_cell = None;
                    }
                }

                let mut selection = self.selection.borrow_mut();

                if let Selection::Selecting {
                    ref origin,
                    from: _,
                    to: _,
                } = *selection
                    && let Some(cursor_position) = cursor.position_from(layout.position())
                    && let Some(position) = state.hit_test(layout.bounds(), cursor_position)
                {
                    let (from, to) = if position.line < origin.line
                        || (position.line == origin.line && position.column < origin.column)
                    {
                        (position, origin.clone())
                    } else {
                        (origin.clone(), position)
                    };

                    *selection = Selection::Selecting {
                        origin: origin.clone(),
                        from,
                        to,
                    };

                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                }
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                state.modifiers = *modifiers;
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                // Key events carry modifiers too; syncing here heals a widget whose
                // state was created after the last ModifiersChanged (a fresh pane, a
                // rebuilt tree) while a modifier was already held.
                state.modifiers = *modifiers;

                if state.is_focused {
                    match key.as_ref() {
                        keyboard::Key::Character("c") if modifiers.command() => {
                            let to_copy =
                                self.terminal_buffer.selected_text(&self.selection.borrow());

                            if !to_copy.is_empty() {
                                clipboard.write(clipboard::Kind::Standard, to_copy);
                            }

                            shell.capture_event();
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn terminal_pane<'a>(
    buffer: Ref<'a, TerminalBuffer>,
    selection: Rc<RefCell<Selection>>,
) -> TerminalPane<'a> {
    TerminalPane::new(buffer, selection)
}
