use std::{
    cell::{Cell, Ref, RefCell},
    collections::HashSet,
    rc::Rc,
    sync::Arc,
};

use crate::terminal_buffer::{
    BufferLinkState, LinkClickEvent, LinkKey, LinkProtocolState, LinkRenderStyle, RenderedOffsets,
    SpanMetadata, TerminalBuffer, authored_color, make_span,
};
use iced::{
    Background, Border, Event, Pixels, Point, Rectangle, Size,
    advanced::{
        self, Layout, Widget, clipboard,
        graphics::core::keyboard,
        layout, mouse,
        renderer::{self, Quad},
        text::{self, Paragraph},
        widget::{Tree, tree},
    },
    alignment,
    time::{Duration, Instant},
    touch,
    widget::text::LineHeight,
    window,
};
use smudgy_core::session::styled_line::{
    LinkAction, LinkDecoration, LinkMenu, LinkMenuItem, LinkSpan, LinkStyleState, LinkTooltip,
    LinkTooltipCallback, LinkTooltipText, StyledLine,
};

mod spans;

use crate::terminal_buffer::selection::{BufferPosition, LineSelection, Selection, word_span_at};
use spans::Spans;

type Link = SpanMetadata;

/// 100 '0's shaped once per prefs generation to measure the monospace cell
/// advance for the column-based line-length clamp.
const ADVANCE_PROBE: &str = "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
const TERMINAL_SELECTION_BACKGROUND: iced::Color = iced::Color::from_rgb8(55, 23, 130);

fn draw_text_decoration<Renderer: advanced::Renderer>(
    renderer: &mut Renderer,
    region: Rectangle,
    y: f32,
    decoration: LinkDecoration,
    color: iced::Color,
    viewport: Rectangle,
) {
    let mut fill = |x: f32, y: f32, width: f32| {
        let rect = Rectangle {
            x,
            y,
            width: width.max(0.5),
            height: 1.0,
        };
        if let Some(bounds) = rect.intersection(&viewport) {
            renderer.fill_quad(
                Quad {
                    bounds,
                    ..Default::default()
                },
                color,
            );
        }
    };
    match decoration {
        LinkDecoration::None => {}
        LinkDecoration::Solid => fill(region.x, y, region.width),
        LinkDecoration::Double => {
            fill(region.x, y - 2.0, region.width);
            fill(region.x, y, region.width);
        }
        LinkDecoration::Dotted => {
            let mut x = region.x;
            while x < region.x + region.width {
                fill(x, y, 1.0_f32.min(region.x + region.width - x));
                x += 3.0;
            }
        }
        LinkDecoration::Dashed => {
            let mut x = region.x;
            while x < region.x + region.width {
                fill(x, y, 4.0_f32.min(region.x + region.width - x));
                x += 6.0;
            }
        }
        LinkDecoration::Wavy => {
            let mut x = region.x;
            let mut up = false;
            while x < region.x + region.width {
                fill(
                    x,
                    y + if up { -1.0 } else { 0.0 },
                    2.0_f32.min(region.x + region.width - x),
                );
                up = !up;
                x += 2.0;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ParagraphCache<P: text::Paragraph> {
    line_number: usize,
    source: Arc<StyledLine>,
    spans: Spans<Link>,
    offsets: RenderedOffsets,
    paragraph: P,
    hidden_blink_paragraphs: Rc<RefCell<HiddenBlinkParagraphs<P>>>,
    blink_modes: u8,
    max_valid_width: f32,
    selection: LineSelection,
    search_selection: bool,
    /// The prefs generation this paragraph was shaped with; a mismatch is a
    /// cache miss (font/size/palette changes rebuild paragraphs).
    generation: u64,
    /// The effective font size this paragraph was shaped at — the per-pane
    /// override composes with the generation (an override change re-shapes
    /// without a prefs bump).
    font_size: f32,
    visual_generation: (u64, u64, u64),
}

#[derive(Debug, Clone, Default)]
struct HiddenBlinkParagraphs<P> {
    slow: Option<P>,
    fast: Option<P>,
    all: Option<P>,
}

const SLOW_BLINK: u8 = 1;
const FAST_BLINK: u8 = 2;
const SLOW_BLINK_HALF_PERIOD_MS: u128 = 500;
const FAST_BLINK_HALF_PERIOD_MS: u128 = 250;
const TOOLTIP_SPINNER_FRAME_MS: u64 = 100;
const TOOLTIP_SPINNER_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

fn link_navigation_index(
    link_count: usize,
    current: Option<usize>,
    backwards: bool,
) -> Option<usize> {
    if link_count == 0 {
        return None;
    }
    Some(if backwards {
        current.map_or(link_count - 1, |index| {
            index.checked_sub(1).unwrap_or(link_count - 1)
        })
    } else {
        current.map_or(0, |index| (index + 1) % link_count)
    })
}

fn append_selected_flag(command: &str, selected: bool) -> Arc<str> {
    let (command, query) = command
        .split_once('?')
        .map_or((command, None), |(command, query)| (command, Some(query)));
    let mut parameters: Vec<_> = query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|parameter| {
            !parameter.is_empty()
                && parameter.split_once('=').map_or(*parameter, |(key, _)| key) != "selected"
        })
        .collect();
    let selected = if selected {
        "selected=true"
    } else {
        "selected=false"
    };
    parameters.push(selected);
    Arc::from(format!("{command}?{}", parameters.join("&")))
}

fn with_selected_callback(action: LinkAction, selected: bool) -> LinkAction {
    match action {
        LinkAction::ServerSend(command) => {
            LinkAction::ServerSend(append_selected_flag(&command, selected))
        }
        LinkAction::Prompt(command) => LinkAction::Prompt(append_selected_flag(&command, selected)),
        other => other,
    }
}

fn link_action_disabled(action: &LinkAction) -> bool {
    action.is_disabled()
        || action
            .protocol()
            .and_then(|protocol| protocol.selection.as_ref())
            .is_some_and(|selection| selection.disabled)
}

fn concealed_whole_lines(
    terminal_buffer: &TerminalBuffer,
    buffer_link_state: &BufferLinkState,
) -> HashSet<usize> {
    terminal_buffer
        .iter_rev_with_line_number(None)
        .map(|(line_number, _)| line_number)
        .filter(|line_number| buffer_link_state.line_concealed(*line_number))
        .collect()
}

fn link_key_available(
    key: LinkKey,
    buffer_link_state: &BufferLinkState,
    hidden_lines: &HashSet<usize>,
) -> bool {
    buffer_link_state.contains(key)
        && !buffer_link_state.concealed(key)
        && buffer_link_state
            .line(key)
            .is_none_or(|line| !hidden_lines.contains(&line))
}

fn hidden_blink_spans(
    spans: &[iced::widget::text::Span<'static, Link>],
    hide_slow: bool,
    hide_fast: bool,
) -> Vec<iced::widget::text::Span<'static, Link>> {
    spans
        .iter()
        .cloned()
        .map(|mut span| {
            let hide = span.link.is_some_and(|metadata| match metadata.blink {
                smudgy_core::session::styled_line::Blink::None => false,
                smudgy_core::session::styled_line::Blink::Slow => hide_slow,
                smudgy_core::session::styled_line::Blink::Fast => hide_fast,
            });
            if hide {
                span.color = Some(iced::Color::TRANSPARENT);
            }
            span
        })
        .collect()
}

fn span_blink_modes(spans: &[iced::widget::text::Span<'static, Link>]) -> u8 {
    spans.iter().fold(0, |modes, span| {
        modes
            | span.link.map_or(0, |metadata| match metadata.blink {
                smudgy_core::session::styled_line::Blink::None => 0,
                smudgy_core::session::styled_line::Blink::Slow => SLOW_BLINK,
                smudgy_core::session::styled_line::Blink::Fast => FAST_BLINK,
            })
    })
}

/// The effective text metrics for a pane: its font override (line height by
/// the same ×1.25 rule the global preference derives with, `prefs.rs`) or
/// the preference values.
pub(super) fn effective_metrics(
    prefs: &crate::prefs::TerminalPrefs,
    font_override: Option<f32>,
) -> (f32, f32) {
    match font_override {
        Some(px) => (px, (px * 1.25).round()),
        None => (prefs.font_size, prefs.line_height),
    }
}

fn clipped_tooltip_text(text: &LinkTooltipText) -> LinkTooltipText {
    // Enough for a dense 60-column by 20-row item/stat block plus breathing
    // room, while still bounding paragraph shaping work from untrusted servers.
    const MAX_CHARS: usize = 4096;
    let mut chars = text.text.chars();
    let mut result: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_none() {
        return text.clone();
    }
    let cutoff = result.len();
    result.push('\u{2026}');
    let mut spans: Vec<_> = text
        .spans
        .iter()
        .filter_map(|span| {
            (span.begin_pos < cutoff).then_some(smudgy_core::session::styled_line::VtSpan {
                style: span.style,
                begin_pos: span.begin_pos,
                end_pos: span.end_pos.min(cutoff),
            })
        })
        .collect();
    if let Some(last) = spans.last_mut() {
        last.end_pos = result.len();
    }
    LinkTooltipText {
        text: Arc::from(result),
        spans: spans.into(),
    }
}

fn loading_tooltip_display(
    spinner_frame: usize,
    target: Option<Arc<str>>,
) -> (LinkTooltipText, Option<Arc<str>>) {
    let indicator = TOOLTIP_SPINNER_FRAMES[spinner_frame % TOOLTIP_SPINNER_FRAMES.len()];
    let text = format!(
        "{indicator} {}",
        crate::i18n::translate("link-tooltip-loading")
    );
    (LinkTooltipText::plain(Arc::from(text)), target)
}

#[derive(Debug, Clone)]
struct LinkTooltipParagraphCache<P: text::Paragraph> {
    text: LinkTooltipText,
    spans: Rc<Vec<iced::widget::text::Span<'static, Link>>>,
    paragraph: P,
    secondary_source: Option<Arc<str>>,
    secondary_text: Option<String>,
    secondary_paragraph: Option<P>,
    generation: u64,
    content_width: f32,
}

fn draw_link_tooltip<Renderer>(
    renderer: &mut Renderer,
    prefs: &crate::prefs::TerminalPrefs,
    viewport: Rectangle,
    anchor: Point,
    link: &HoveredLinkTooltip,
    spinner_frame: usize,
    paragraph_cache: &RefCell<Option<LinkTooltipParagraphCache<Renderer::Paragraph>>>,
) where
    Renderer: text::Renderer<Font = iced::Font>,
    Renderer::Paragraph: iced::advanced::text::Paragraph<Font = iced::Font>,
{
    let tooltip = &link.tooltip;
    let display = if tooltip.is_loading() {
        Some(loading_tooltip_display(
            spinner_frame,
            tooltip.target.clone(),
        ))
    } else {
        tooltip.display_styled()
    };
    let Some((primary, secondary)) = display else {
        return;
    };
    let target = link.action.tooltip_target();
    let primary = if secondary.is_none() && target.as_deref() == Some(primary.text.as_ref()) {
        LinkTooltipText::plain(target.clone().expect("matched target must exist"))
    } else {
        clipped_tooltip_text(&primary)
    };
    // The tooltip's secondary value is only a signal that custom copy needs a
    // disclosure line. Always render the action's authoritative, escaped
    // target instead of trusting a copy stored alongside authored content.
    let secondary_source = secondary.and(target);

    let padding = 8.0;
    let gap = if secondary_source.is_some() { 3.0 } else { 0.0 };
    let max_width = (viewport.width - 2.0).min(520.0);
    if max_width < 2.0 * padding + 1.0 {
        return;
    }
    let content_bounds = Size::new(max_width - 2.0 * padding, f32::INFINITY);
    let make_plain_paragraph = |content: &str, size: f32| {
        Renderer::Paragraph::with_text(iced::advanced::text::Text {
            content,
            bounds: content_bounds,
            size: Pixels(size),
            font: prefs.font,
            line_height: LineHeight::Absolute(Pixels((size * 1.25).round())),
            align_x: text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::WordOrGlyph,
        })
    };
    let make_primary_paragraph = || {
        let spans: Rc<Vec<iced::widget::text::Span<'static, Link>>> = Rc::new(
            primary
                .spans
                .iter()
                .map(|span| {
                    make_span(
                        &primary.text[span.begin_pos..span.end_pos],
                        span.style,
                        false,
                        None,
                        prefs,
                    )
                })
                .collect(),
        );
        let paragraph = Renderer::Paragraph::with_spans(iced::advanced::text::Text {
            content: spans.as_slice(),
            bounds: content_bounds,
            size: Pixels(13.0),
            font: prefs.font,
            line_height: LineHeight::Absolute(Pixels((13.0_f32 * 1.25).round())),
            align_x: text::Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::WordOrGlyph,
        });
        (spans, paragraph)
    };
    let make_secondary = || {
        let text = secondary_source.as_deref().map(str::to_owned);
        let paragraph = text.as_deref().map(|text| make_plain_paragraph(text, 11.0));
        (text, paragraph)
    };
    let mut paragraph_cache = paragraph_cache.borrow_mut();
    let reset = paragraph_cache.as_ref().is_none_or(|cache| {
        cache.generation != prefs.generation || cache.content_width != content_bounds.width
    });
    if reset {
        let (spans, paragraph) = make_primary_paragraph();
        let (secondary_text, secondary_paragraph) = make_secondary();
        *paragraph_cache = Some(LinkTooltipParagraphCache {
            text: primary.clone(),
            spans,
            paragraph,
            secondary_source: secondary_source.clone(),
            secondary_text,
            secondary_paragraph,
            generation: prefs.generation,
            content_width: content_bounds.width,
        });
    } else if let Some(cache) = paragraph_cache.as_mut() {
        // The loading spinner changes the primary text ten times a second. Keep
        // the disclosed target's sanitized copy and paragraph when only that
        // primary changes.
        if cache.text != primary {
            let (spans, paragraph) = make_primary_paragraph();
            cache.text = primary.clone();
            cache.spans = spans;
            cache.paragraph = paragraph;
        }
        if cache.secondary_source != secondary_source {
            let (secondary_text, secondary_paragraph) = make_secondary();
            cache.secondary_source = secondary_source.clone();
            cache.secondary_text = secondary_text;
            cache.secondary_paragraph = secondary_paragraph;
        }
    }
    let primary_cache = paragraph_cache
        .as_ref()
        .expect("tooltip paragraph cache was just populated");
    let primary_paragraph = &primary_cache.paragraph;
    let secondary_paragraph = primary_cache.secondary_paragraph.as_ref();
    let content_width = secondary_paragraph
        .as_ref()
        .map_or(primary_paragraph.min_width(), |secondary| {
            primary_paragraph.min_width().max(secondary.min_width())
        });
    let width = (content_width + 2.0 * padding).min(max_width);
    let height = primary_paragraph.min_height()
        + secondary_paragraph.as_ref().map_or(0.0, |p| p.min_height())
        + gap
        + 2.0 * padding;
    let right = viewport.x + viewport.width;
    let bottom = viewport.y + viewport.height;
    let x = (anchor.x + 12.0).min(right - width).max(viewport.x);
    let below = anchor.y + 18.0;
    let y = if below + height <= bottom {
        below
    } else {
        (anchor.y - height - 8.0).max(viewport.y)
    };
    let bounds = Rectangle::new(Point::new(x, y), Size::new(width, height));
    let foreground = prefs.palette.foreground;

    // iced batches quads and glyphs into primitive sublayers. Without a fresh
    // renderer layer, every terminal paragraph is composited after this card's
    // fill, which makes the scrollback show through it.
    renderer.start_layer(viewport);
    renderer.fill_quad(
        Quad {
            bounds,
            border: Border {
                color: iced::Color {
                    a: 0.28,
                    ..foreground
                },
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        },
        Background::Color(iced::Color {
            a: 0.97,
            ..prefs.palette.background
        }),
    );
    let primary_at = Point::new(x + padding, y + padding);
    let primary_height = primary_paragraph.min_height();
    for (index, span) in primary_cache.spans.iter().enumerate() {
        let Some(highlight) = span.highlight else {
            continue;
        };
        for region in primary_paragraph.span_bounds(index) {
            let bounds = Rectangle {
                x: primary_at.x + region.x,
                y: primary_at.y + region.y,
                width: region.width,
                height: region.height,
            };
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
    renderer.fill_paragraph(primary_paragraph, primary_at, foreground, viewport);
    if let (Some(_secondary), Some(secondary_paragraph)) =
        (&primary_cache.secondary_text, secondary_paragraph)
    {
        let secondary_at = Point::new(x + padding, y + padding + primary_height + gap);
        renderer.fill_paragraph(
            secondary_paragraph,
            secondary_at,
            iced::Color {
                a: foreground.a * 0.58,
                ..foreground
            },
            viewport,
        );
    }
    renderer.end_layer();
}

#[derive(Debug, Clone)]
struct LinkMenuPopup {
    menu: LinkMenu,
    anchor: Point,
    source: Option<(LinkKey, LinkAction)>,
}

fn menu_source_is_available(
    popup: &LinkMenuPopup,
    live: &BufferLinkState,
    hidden_lines: &HashSet<usize>,
) -> bool {
    popup.source.as_ref().is_none_or(|(key, action)| {
        link_key_available(*key, live, hidden_lines) && !link_action_disabled(action)
    })
}

#[derive(Debug)]
enum LinkMenuInvocation {
    None,
    RevealSpoiler,
    Open(Box<LinkMenuPopup>),
}

fn resolve_link_menu_invocation<P: text::Paragraph>(
    state: &mut State<P>,
    buffer_link_state: &mut BufferLinkState,
    key: LinkKey,
    link: &LinkSpan,
    anchor: Point,
) -> LinkMenuInvocation {
    if !link_key_available(key, buffer_link_state, &state.hidden_lines)
        || link_action_disabled(&link.action)
    {
        return LinkMenuInvocation::None;
    }
    let Some(menu) = link.action.menu().cloned() else {
        return LinkMenuInvocation::None;
    };
    if buffer_link_state.reveal_spoiler(key) {
        state.link_tooltip_hover = None;
        state.invalidate_link_styles();
        return LinkMenuInvocation::RevealSpoiler;
    }
    LinkMenuInvocation::Open(Box::new(LinkMenuPopup {
        menu,
        anchor,
        source: Some((key, link.action.clone())),
    }))
}

fn apply_menu_choice_protocol(
    protocol_state: &mut LinkProtocolState,
    buffer_link_state: &mut BufferLinkState,
    source: Option<&(LinkKey, LinkAction)>,
    now: Instant,
) -> Option<Instant> {
    let (key, action) = source?;
    let redraw_at = buffer_link_state.activate_visibility(*key, now);
    protocol_state.record_menu_choice(action);
    redraw_at
}

struct LinkMenuGeometry {
    bounds: Rectangle,
    title: Option<Rectangle>,
    rows: Vec<Rectangle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HoveredLinkTooltip {
    action: LinkAction,
    tooltip: LinkTooltip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkTooltipOrigin {
    Mouse,
    Keyboard(LinkKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkTooltipHover {
    Waiting {
        link: HoveredLinkTooltip,
        at: Instant,
        origin: LinkTooltipOrigin,
    },
    Open {
        link: HoveredLinkTooltip,
        origin: LinkTooltipOrigin,
    },
}

impl LinkTooltipHover {
    fn link(&self) -> &HoveredLinkTooltip {
        match self {
            Self::Waiting { link, .. } | Self::Open { link, .. } => link,
        }
    }

    fn origin(&self) -> LinkTooltipOrigin {
        match self {
            Self::Waiting { origin, .. } | Self::Open { origin, .. } => *origin,
        }
    }

    fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }

    fn is_keyboard(&self) -> bool {
        matches!(self.origin(), LinkTooltipOrigin::Keyboard(_))
    }
}

fn keyboard_tooltip_scrolled_out(
    hover: Option<&LinkTooltipHover>,
    live: &BufferLinkState,
    is_visible: impl Fn(usize) -> bool,
) -> bool {
    hover
        .and_then(|hover| match hover.origin() {
            LinkTooltipOrigin::Keyboard(key) => live.line(key),
            LinkTooltipOrigin::Mouse => None,
        })
        .is_some_and(|line| !is_visible(line))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TooltipRedraw {
    now: bool,
    at: Option<Instant>,
}

impl TooltipRedraw {
    fn merge(&mut self, other: Self) {
        self.now |= other.now;
        self.at = self.at.into_iter().chain(other.at).min();
    }
}

fn begin_mouse_link_tooltip(
    link: HoveredLinkTooltip,
    delay: Duration,
    now: Instant,
) -> (LinkTooltipHover, TooltipRedraw) {
    if delay == Duration::ZERO {
        (
            LinkTooltipHover::Open {
                link,
                origin: LinkTooltipOrigin::Mouse,
            },
            TooltipRedraw {
                now: true,
                at: None,
            },
        )
    } else {
        (
            LinkTooltipHover::Waiting {
                link,
                at: now,
                origin: LinkTooltipOrigin::Mouse,
            },
            TooltipRedraw {
                now: false,
                at: Some(now + delay),
            },
        )
    }
}

fn update_mouse_link_tooltip(
    previous: Option<LinkTooltipHover>,
    hovered: Option<HoveredLinkTooltip>,
    delay: Duration,
    now: Instant,
    cursor_moved: bool,
) -> (Option<LinkTooltipHover>, TooltipRedraw) {
    if previous.as_ref().is_some_and(LinkTooltipHover::is_keyboard) {
        return (previous, TooltipRedraw::default());
    }

    match (previous, hovered) {
        (None, None) => (None, TooltipRedraw::default()),
        (Some(previous), None) => (
            None,
            TooltipRedraw {
                now: previous.is_open(),
                at: None,
            },
        ),
        (None, Some(link)) => {
            let (hover, redraw) = begin_mouse_link_tooltip(link, delay, now);
            (Some(hover), redraw)
        }
        (Some(previous), Some(link)) if previous.link() != &link => {
            let mut redraw = TooltipRedraw {
                now: previous.is_open(),
                at: None,
            };
            let (hover, begin_redraw) = begin_mouse_link_tooltip(link, delay, now);
            redraw.merge(begin_redraw);
            (Some(hover), redraw)
        }
        (
            Some(LinkTooltipHover::Waiting {
                link, at, origin, ..
            }),
            Some(_),
        ) => {
            let elapsed = now.saturating_duration_since(at);
            if elapsed >= delay {
                (
                    Some(LinkTooltipHover::Open { link, origin }),
                    TooltipRedraw {
                        now: true,
                        at: None,
                    },
                )
            } else {
                (
                    Some(LinkTooltipHover::Waiting { link, at, origin }),
                    TooltipRedraw {
                        now: false,
                        at: Some(now + delay - elapsed),
                    },
                )
            }
        }
        (Some(LinkTooltipHover::Open { link, origin }), Some(_)) => (
            Some(LinkTooltipHover::Open { link, origin }),
            TooltipRedraw {
                now: cursor_moved,
                at: None,
            },
        ),
    }
}

fn request_tooltip_redraw<Message>(
    redraw: TooltipRedraw,
    shell: &mut advanced::Shell<'_, Message>,
) {
    if redraw.now {
        shell.request_redraw();
    }
    if let Some(at) = redraw.at {
        shell.request_redraw_at(at);
    }
}

/// Claim the tooltip's one-shot callback only when there is somewhere to send
/// it. Claiming first would leave the shared state permanently loading when a
/// runtime (and therefore its handler) is unavailable.
fn claim_tooltip_request(
    tooltip: &LinkTooltip,
    handler_available: bool,
) -> Option<LinkTooltipCallback> {
    handler_available.then(|| tooltip.request()).flatten()
}

fn byte_ranges_overlap(begin: usize, end: usize, range_begin: usize, range_end: usize) -> bool {
    end > range_begin && begin < range_end
}

fn link_menu_geometry<Renderer>(
    popup: &LinkMenuPopup,
    prefs: &crate::prefs::TerminalPrefs,
    viewport: Rectangle,
) -> Option<LinkMenuGeometry>
where
    Renderer: text::Renderer<Font = iced::Font>,
    Renderer::Paragraph: iced::advanced::text::Paragraph<Font = iced::Font>,
{
    const PADDING: f32 = 4.0;
    const HORIZONTAL_PADDING: f32 = 10.0;
    const ACTION_HEIGHT: f32 = 27.0;
    const TITLE_HEIGHT: f32 = 25.0;
    const SEPARATOR_HEIGHT: f32 = 9.0;
    let max_width = (viewport.width - 2.0).min(420.0);
    if max_width < 2.0 * (PADDING + HORIZONTAL_PADDING) + 1.0 {
        return None;
    }
    let measure = |content: &str, size: f32| {
        Renderer::Paragraph::with_text(iced::advanced::text::Text {
            content,
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: Pixels(size),
            font: prefs.font,
            line_height: LineHeight::Absolute(Pixels((size * 1.25).round())),
            align_x: text::Alignment::Left,
            align_y: alignment::Vertical::Center,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
        })
        .min_width()
    };
    let title_width = popup
        .menu
        .title
        .as_ref()
        .map_or(0.0, |title| measure(&title.text, 12.0));
    let item_width = popup
        .menu
        .items
        .iter()
        .filter_map(|item| match item {
            LinkMenuItem::Separator => None,
            LinkMenuItem::Action { label, .. } => Some(measure(label, 13.0)),
        })
        .fold(0.0, f32::max);
    let width = (title_width.max(item_width) + 2.0 * (PADDING + HORIZONTAL_PADDING))
        .max(120.0_f32.min(max_width))
        .min(max_width);
    let content_height = popup.menu.title.as_ref().map_or(0.0, |_| TITLE_HEIGHT)
        + popup
            .menu
            .items
            .iter()
            .map(|item| match item {
                LinkMenuItem::Separator => SEPARATOR_HEIGHT,
                LinkMenuItem::Action { .. } => ACTION_HEIGHT,
            })
            .sum::<f32>();
    let height = content_height + 2.0 * PADDING;
    let right = viewport.x + viewport.width;
    let bottom = viewport.y + viewport.height;
    let x = popup.anchor.x.min(right - width).max(viewport.x);
    let y = popup.anchor.y.min(bottom - height).max(viewport.y);
    let bounds = Rectangle::new(Point::new(x, y), Size::new(width, height));
    let mut top = y + PADDING;
    let title = popup.menu.title.as_ref().map(|_| {
        let bounds = Rectangle::new(
            Point::new(x + PADDING, top),
            Size::new(width - 2.0 * PADDING, TITLE_HEIGHT),
        );
        top += TITLE_HEIGHT;
        bounds
    });
    let rows = popup
        .menu
        .items
        .iter()
        .map(|item| {
            let row_height = match item {
                LinkMenuItem::Separator => SEPARATOR_HEIGHT,
                LinkMenuItem::Action { .. } => ACTION_HEIGHT,
            };
            let bounds = Rectangle::new(
                Point::new(x + PADDING, top),
                Size::new(width - 2.0 * PADDING, row_height),
            );
            top += row_height;
            bounds
        })
        .collect();
    Some(LinkMenuGeometry {
        bounds,
        title,
        rows,
    })
}

fn draw_link_menu<Renderer>(
    renderer: &mut Renderer,
    prefs: &crate::prefs::TerminalPrefs,
    viewport: Rectangle,
    cursor: mouse::Cursor,
    popup: &LinkMenuPopup,
) where
    Renderer: text::Renderer<Font = iced::Font>,
    Renderer::Paragraph: iced::advanced::text::Paragraph<Font = iced::Font>,
{
    let Some(geometry) = link_menu_geometry::<Renderer>(popup, prefs, viewport) else {
        return;
    };
    let foreground = prefs.palette.foreground;
    let make_text = |content: String, size: f32, bounds: Rectangle| iced::advanced::text::Text {
        content,
        bounds: bounds.size(),
        size: Pixels(size),
        font: prefs.font,
        line_height: LineHeight::Absolute(Pixels((size * 1.25).round())),
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Center,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    };

    renderer.start_layer(viewport);
    renderer.fill_quad(
        Quad {
            bounds: geometry.bounds,
            border: Border {
                color: iced::Color {
                    a: 0.3,
                    ..foreground
                },
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        },
        Background::Color(iced::Color {
            a: 0.98,
            ..prefs.palette.background
        }),
    );
    if let (Some(title), Some(title_bounds)) = (&popup.menu.title, geometry.title) {
        let text_bounds = Rectangle {
            x: title_bounds.x + 10.0,
            width: title_bounds.width - 20.0,
            ..title_bounds
        };
        let mut title_font = prefs.font;
        if title.style.as_ref().and_then(|style| style.bold) == Some(true) {
            title_font.weight = iced::font::Weight::Bold;
        }
        if title.style.as_ref().and_then(|style| style.italic) == Some(true) {
            title_font.style = iced::font::Style::Italic;
        }
        if let Some(background) = title
            .style
            .as_ref()
            .and_then(|style| style.background)
            .map(authored_color)
        {
            renderer.fill_quad(
                Quad {
                    bounds: text_bounds,
                    ..Default::default()
                },
                Background::Color(background),
            );
        }
        let title_foreground = title
            .style
            .as_ref()
            .and_then(|style| style.foreground)
            .map(authored_color)
            .unwrap_or_else(|| iced::Color::from_rgb8(0x5f, 0xbd, 0xaf));
        renderer.fill_text(
            iced::advanced::text::Text {
                font: title_font,
                ..make_text(title.text.to_string(), 12.0, text_bounds)
            },
            Point::new(text_bounds.x, text_bounds.center_y()),
            title_foreground,
            viewport,
        );
        if let Some(style) = &title.style {
            let decoration_color = style
                .decoration_color
                .map(authored_color)
                .unwrap_or(title_foreground);
            draw_text_decoration(
                renderer,
                text_bounds,
                text_bounds.y + text_bounds.height - 5.0,
                style.underline.unwrap_or_default(),
                decoration_color,
                viewport,
            );
            draw_text_decoration(
                renderer,
                text_bounds,
                text_bounds.y + 3.0,
                style.overline.unwrap_or_default(),
                decoration_color,
                viewport,
            );
            draw_text_decoration(
                renderer,
                text_bounds,
                text_bounds.center_y(),
                style.strikethrough.unwrap_or_default(),
                decoration_color,
                viewport,
            );
        }
    }
    for (item, row) in popup.menu.items.iter().zip(geometry.rows) {
        match item {
            LinkMenuItem::Separator => {
                renderer.fill_quad(
                    Quad {
                        bounds: Rectangle::new(
                            Point::new(row.x + 8.0, row.center_y() - 0.5),
                            Size::new(row.width - 16.0, 1.0),
                        ),
                        ..Default::default()
                    },
                    Background::Color(iced::Color {
                        a: foreground.a * 0.2,
                        ..foreground
                    }),
                );
            }
            LinkMenuItem::Action { label, .. } => {
                if cursor.is_over(row) {
                    renderer.fill_quad(
                        Quad {
                            bounds: row,
                            border: Border {
                                radius: 3.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        Background::Color(iced::Color {
                            a: foreground.a * 0.12,
                            ..foreground
                        }),
                    );
                }
                let text_bounds = Rectangle {
                    x: row.x + 10.0,
                    width: row.width - 20.0,
                    ..row
                };
                renderer.fill_text(
                    make_text(label.to_string(), 13.0, text_bounds),
                    Point::new(text_bounds.x, text_bounds.center_y()),
                    foreground,
                    viewport,
                );
            }
        }
    }
    renderer.end_layer();
}

/// State specific to the TerminalPane widget instance.
#[derive(Debug, Clone)]
pub(super) struct State<P: text::Paragraph> {
    pub last_line_number: usize,
    cache: Vec<ParagraphCache<P>>,
    pub is_focused: bool,
    /// Measured `(prefs generation, effective font size, monospace cell
    /// advance)` — the font size composes because a per-pane override changes
    /// the cell without a prefs bump.
    pub advance: Option<(u64, f32, f32)>,
    /// Keyboard modifiers as of the last change event, reported with link clicks.
    pub modifiers: keyboard::Modifiers,
    /// The buffer cell the press landed on, kept while the pointer stays on it. A
    /// release on the same cell is a click (fires links); any divergence — a drag,
    /// or content scrolling under a stationary cursor — clears it. Per-pane state,
    /// NOT derived from the shared `Selection`: a sibling pane processes the same
    /// release first and flips `Selecting` → `Selected`, so selection state alone
    /// cannot tell this pane a click just ended on it.
    pub pressed_cell: Option<BufferPosition>,
    /// The most recent left click. `mouse::Click::new` compares the next press
    /// against it: a press at the same spot within iced's click window is a
    /// double click (select word) or a triple click (select line). The
    /// titlebar's "double click to maximize" gesture uses the same primitive.
    previous_click: Option<mouse::Click>,
    /// The terminal-owned OSC/script link context menu, anchored where it was
    /// opened. Actions are cloned with the scrollback so the popup remains
    /// valid even if new output arrives before a choice is clicked.
    menu_popup: Option<LinkMenuPopup>,
    /// Delay state for the OSC/script tooltip under the pointer. The link and
    /// tooltip values form the hover identity, so moving within one linked
    /// range does not restart the timer.
    link_tooltip_hover: Option<LinkTooltipHover>,
    /// Persistent rich paragraph for the open tooltip. iced's renderer keeps
    /// paragraph primitives by weak reference until GPU preparation, so this
    /// must outlive the draw call instead of being a transient local.
    link_tooltip_paragraph: RefCell<Option<LinkTooltipParagraphCache<P>>>,
    /// Common timebase and current visibility phases for SGR 5/6 text.
    /// `update` resolves the phases every frame; `draw` only reads them.
    blink_epoch: Instant,
    slow_blink_visible: bool,
    fast_blink_visible: bool,
    hovered_link: Option<LinkKey>,
    active_link: Option<LinkKey>,
    focused_link: Option<LinkKey>,
    focus_visible: bool,
    /// Logical lines concealed with OSC 8 `wholeline`. They are omitted from
    /// layout and every interaction path, including links that merely share
    /// the line with the visibility-controlling link.
    hidden_lines: HashSet<usize>,
    visual_generation: u64,
    seen_link_navigation_reset_epoch: u64,
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
            previous_click: None,
            menu_popup: None,
            link_tooltip_hover: None,
            link_tooltip_paragraph: RefCell::new(None),
            blink_epoch: Instant::now(),
            slow_blink_visible: true,
            fast_blink_visible: true,
            hovered_link: None,
            active_link: None,
            focused_link: None,
            focus_visible: false,
            hidden_lines: HashSet::new(),
            visual_generation: 0,
            seen_link_navigation_reset_epoch: 0,
        }
    }
}

impl<P: text::Paragraph> State<P> {
    fn invalidate_link_styles(&mut self) {
        self.visual_generation = self.visual_generation.wrapping_add(1);
    }

    fn install_menu_popup(&mut self, popup: LinkMenuPopup) {
        self.menu_popup = Some(popup);
        self.pressed_cell = None;
        self.link_tooltip_hover = None;
    }

    fn clear_keyboard_tooltip(&mut self) -> bool {
        if self
            .link_tooltip_hover
            .as_ref()
            .is_some_and(LinkTooltipHover::is_keyboard)
        {
            self.link_tooltip_hover = None;
            true
        } else {
            false
        }
    }

    fn dismiss_link_overlay(&mut self) -> bool {
        let menu = self.menu_popup.take().is_some();
        let tooltip = self.clear_keyboard_tooltip();
        menu || tooltip
    }

    fn prune_widget_links(&mut self, live: &BufferLinkState) {
        if self
            .hovered_link
            .is_some_and(|key| !link_key_available(key, live, &self.hidden_lines))
        {
            self.hovered_link = None;
            if self
                .link_tooltip_hover
                .as_ref()
                .is_some_and(|hover| hover.origin() == LinkTooltipOrigin::Mouse)
            {
                self.link_tooltip_hover = None;
            }
        }
        if self
            .active_link
            .is_some_and(|key| !link_key_available(key, live, &self.hidden_lines))
        {
            self.active_link = None;
        }
        if self
            .focused_link
            .is_some_and(|key| !link_key_available(key, live, &self.hidden_lines))
        {
            self.focused_link = None;
            self.focus_visible = false;
        }
        if self
            .menu_popup
            .as_ref()
            .is_some_and(|popup| !menu_source_is_available(popup, live, &self.hidden_lines))
        {
            self.menu_popup = None;
        }
        if self
            .link_tooltip_hover
            .as_ref()
            .is_some_and(|hover| match hover.origin() {
                LinkTooltipOrigin::Keyboard(key) => {
                    !link_key_available(key, live, &self.hidden_lines)
                }
                LinkTooltipOrigin::Mouse => false,
            })
        {
            self.link_tooltip_hover = None;
        }
    }

    fn keyboard_focused_link(&self) -> Option<LinkKey> {
        self.focus_visible.then_some(self.focused_link).flatten()
    }

    fn process_link_navigation_reset(&mut self, epoch: u64) {
        if epoch == self.seen_link_navigation_reset_epoch {
            return;
        }
        self.seen_link_navigation_reset_epoch = epoch;

        let cleared_tooltip = self.clear_keyboard_tooltip();
        let changed =
            self.is_focused || self.focused_link.is_some() || self.focus_visible || cleared_tooltip;
        self.is_focused = false;
        self.focused_link = None;
        self.focus_visible = false;
        if changed {
            self.invalidate_link_styles();
        }
    }

    pub(super) fn hit_test(&self, bounds: Rectangle, point: iced::Point) -> Option<BufferPosition> {
        let mut line_top = bounds.height;

        for line in &self.cache {
            let line_number = line.line_number;
            let line_bottom = line_top;
            line_top -= line.paragraph.min_height();

            if point.y >= line_top && point.y < line_bottom {
                let point_in_paragraph = iced::Point::new(point.x, point.y - line_top);
                return match line.paragraph.hit_test(point_in_paragraph) {
                    Some(hit) => Some(BufferPosition {
                        line: line_number,
                        column: line.offsets.rendered_to_source(hit.cursor()),
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
                                .map(|(_, idx)| {
                                    let rendered_column = line
                                        .spans
                                        .spans()
                                        .iter()
                                        .take(idx + 1)
                                        .fold(0, |acc, span| acc + span.text.len());
                                    BufferPosition {
                                        line: line_number,
                                        column: line.offsets.rendered_to_source(rendered_column),
                                    }
                                })
                        }
                    }
                };
            }
        }
        None
    }

    /// The rendered text of absolute line `line_number` and its offset map,
    /// or `None` if the line is not in the paragraph cache (scrolled out of
    /// view, or hidden). The text is joined from the shaped spans, so byte
    /// offsets into it match the cursors that `Paragraph::hit_test` returns.
    pub(super) fn rendered_line(&self, line_number: usize) -> Option<(String, &RenderedOffsets)> {
        let line = self
            .cache
            .iter()
            .find(|line| line.line_number == line_number)?;
        let text = line
            .spans
            .spans()
            .iter()
            .map(|span| span.text.as_ref())
            .collect::<String>();
        Some((text, &line.offsets))
    }

    fn link_tooltip_anchor(
        &self,
        bounds: Rectangle,
        line: usize,
        begin: usize,
        end: usize,
    ) -> Option<Point> {
        let mut y = bounds.y + bounds.height;
        for cache in &self.cache {
            y -= cache.paragraph.min_height();
            if cache.line_number != line {
                continue;
            }

            let range_begin = cache.offsets.source_to_rendered(begin);
            let range_end = cache.offsets.source_to_rendered(end);
            let mut span_begin = 0;
            for (index, span) in cache.spans.spans().iter().enumerate() {
                let span_end = span_begin + span.text.len();
                if byte_ranges_overlap(span_begin, span_end, range_begin, range_end) {
                    for region in cache.paragraph.span_bounds(index) {
                        let absolute = Rectangle {
                            x: bounds.x + region.x,
                            y: y + region.y,
                            width: region.width,
                            height: region.height,
                        };
                        if let Some(visible) = absolute.intersection(&bounds) {
                            return Some(visible.center());
                        }
                    }
                }
                span_begin = span_end;
            }
            return None;
        }
        None
    }
}

#[derive(Debug)]
enum LinkActivationOutcome {
    None,
    Publish(LinkClickEvent),
    OpenMenu(LinkMenuPopup),
}

#[derive(Debug)]
struct LinkActivation {
    outcome: LinkActivationOutcome,
    redraw_at: Option<Instant>,
}

struct LinkActivationRequest<'a> {
    key: LinkKey,
    link: &'a LinkSpan,
    menu_anchor: Option<Point>,
    modifiers: keyboard::Modifiers,
    now: Instant,
    can_publish: bool,
}

fn resolve_link_activation<P: text::Paragraph>(
    state: &mut State<P>,
    protocol_state: &mut LinkProtocolState,
    buffer_link_state: &mut BufferLinkState,
    request: LinkActivationRequest<'_>,
) -> LinkActivation {
    state.clear_keyboard_tooltip();
    if !link_key_available(request.key, buffer_link_state, &state.hidden_lines)
        || link_action_disabled(&request.link.action)
    {
        return LinkActivation {
            outcome: LinkActivationOutcome::None,
            redraw_at: None,
        };
    }
    let reveal_only = buffer_link_state.reveal_spoiler(request.key);
    if reveal_only {
        state.link_tooltip_hover = None;
    }

    // Opening a left-click menu is only navigation. Visibility, selection, and
    // visited bookkeeping belong to the chosen row so protocol state changes
    // exactly once when an action is actually selected.
    if !reveal_only
        && request.link.action.opens_menu_on_left_click()
        && let Some(menu) = request.link.action.menu().cloned()
        && let Some(anchor) = request.menu_anchor
    {
        return LinkActivation {
            outcome: LinkActivationOutcome::OpenMenu(LinkMenuPopup {
                menu,
                anchor,
                source: Some((request.key, request.link.action.clone())),
            }),
            redraw_at: None,
        };
    }

    let redraw_at = buffer_link_state.activate_visibility(request.key, request.now);
    let selected = protocol_state.toggle_link_selection(&request.link.action);
    let outcome = if !reveal_only
        && request.can_publish
        && let Some(mut primary) = request.link.action.primary().cloned()
    {
        if let Some(selected) = selected
            && request.link.action.menu().is_none()
        {
            primary = with_selected_callback(primary, selected);
        }
        protocol_state.mark_visited(&request.link.action);
        LinkActivationOutcome::Publish(LinkClickEvent {
            action: primary,
            shift: request.modifiers.shift(),
            ctrl: request.modifiers.control(),
            alt: request.modifiers.alt(),
        })
    } else {
        LinkActivationOutcome::None
    };

    LinkActivation { outcome, redraw_at }
}

fn open_menu_popup<Message, P: text::Paragraph>(
    state: &mut State<P>,
    popup: LinkMenuPopup,
    shell: &mut advanced::Shell<'_, Message>,
) {
    state.install_menu_popup(popup);
    shell.invalidate_layout();
    shell.request_redraw();
}

pub struct TerminalPane<'a, Message> {
    terminal_buffer: Ref<'a, TerminalBuffer>,
    link_protocol_state: Rc<RefCell<LinkProtocolState>>,
    buffer_link_state: Rc<RefCell<BufferLinkState>>,
    selection: Rc<RefCell<Selection>>,
    search_selection: Rc<Cell<bool>>,
    last_line_number: Option<usize>,
    /// Maps a clicked link span into the hosting session's message. Publishing
    /// through the widget shell defers session mutation until the terminal's
    /// immutable scrollback borrow has been released.
    on_link: Option<Rc<dyn Fn(LinkClickEvent) -> Message>>,
    /// Routes the one lazy first-hover request for script-authored tooltip copy.
    on_link_tooltip: Option<Rc<dyn Fn(LinkTooltipCallback)>>,
    /// Per-pane terminal font override (`docs/panes.md`); `None` follows the
    /// global preference.
    font_size: Option<f32>,
}

impl<'a, Message> TerminalPane<'a, Message> {
    pub fn new(buffer: Ref<'a, TerminalBuffer>, selection: Rc<RefCell<Selection>>) -> Self {
        log::debug!("TerminalPane::new() called");
        let buffer_link_state = buffer.link_state();
        let link_protocol_state = buffer.link_protocol_state();
        Self {
            terminal_buffer: buffer,
            link_protocol_state,
            buffer_link_state,
            selection,
            search_selection: Rc::new(Cell::new(false)),
            last_line_number: None,
            on_link: None,
            on_link_tooltip: None,
            font_size: None,
        }
    }

    pub fn last_line_number(mut self, last_line_number: usize) -> Self {
        self.last_line_number = Some(last_line_number);
        self
    }

    pub fn search_selection(mut self, search_selection: Rc<Cell<bool>>) -> Self {
        self.search_selection = search_selection;
        self
    }

    pub fn on_link(mut self, on_link: Option<Rc<dyn Fn(LinkClickEvent) -> Message>>) -> Self {
        self.on_link = on_link;
        self
    }

    pub fn on_link_tooltip(
        mut self,
        on_link_tooltip: Option<Rc<dyn Fn(LinkTooltipCallback)>>,
    ) -> Self {
        self.on_link_tooltip = on_link_tooltip;
        self
    }

    pub fn font_size(mut self, font_size: Option<f32>) -> Self {
        self.font_size = font_size;
        self
    }

    fn available_link_at<'b, P: text::Paragraph>(
        &'b self,
        state: &State<P>,
        line: usize,
        column: usize,
    ) -> Option<(LinkKey, &'b LinkSpan)> {
        let link = self.terminal_buffer.link_span_at(line, column)?;
        let key = self.terminal_buffer.link_key(line, link)?;
        link_key_available(key, &self.buffer_link_state.borrow(), &state.hidden_lines)
            .then_some((key, link))
    }

    fn activate_link<P: text::Paragraph>(
        &self,
        state: &mut State<P>,
        key: LinkKey,
        link: &LinkSpan,
        menu_anchor: Option<Point>,
        modifiers: keyboard::Modifiers,
        now: Instant,
        shell: &mut advanced::Shell<'_, Message>,
    ) {
        let activation = {
            let mut protocol_state = self.link_protocol_state.borrow_mut();
            let mut buffer_link_state = self.buffer_link_state.borrow_mut();
            resolve_link_activation(
                state,
                &mut protocol_state,
                &mut buffer_link_state,
                LinkActivationRequest {
                    key,
                    link,
                    menu_anchor,
                    modifiers,
                    now,
                    can_publish: self.on_link.is_some(),
                },
            )
        };
        if let Some(deadline) = activation.redraw_at {
            shell.request_redraw_at(deadline);
        }

        match activation.outcome {
            LinkActivationOutcome::None => {
                state.invalidate_link_styles();
                shell.invalidate_layout();
                shell.request_redraw();
            }
            LinkActivationOutcome::Publish(event) => {
                if let Some(on_link) = self.on_link.as_ref() {
                    shell.publish(on_link(event));
                }
                state.invalidate_link_styles();
                shell.invalidate_layout();
                shell.request_redraw();
            }
            LinkActivationOutcome::OpenMenu(popup) => {
                state.invalidate_link_styles();
                open_menu_popup(state, popup, shell);
            }
        }
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer> for TerminalPane<'a, Message>
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
        state.hidden_lines = {
            let live_links = self.buffer_link_state.borrow();
            concealed_whole_lines(&self.terminal_buffer, &live_links)
        };
        {
            let mut live_links = self.buffer_link_state.borrow_mut();
            live_links.retire_deleted_line_registrations();
            state.prune_widget_links(&live_links);
        }
        state.process_link_navigation_reset(self.terminal_buffer.link_navigation_reset_epoch());
        let selection = self.selection.borrow();
        let search_selection = self.search_selection.get();
        let protocol_state = self.link_protocol_state.borrow();
        let buffer_link_state = self.buffer_link_state.borrow();
        let prefs = crate::prefs::current();
        let (font_size, line_height) = effective_metrics(&prefs, self.font_size);

        // The measured width of one monospace cell at the current font/size,
        // measured once per (prefs generation, effective font size). It clamps
        // the wrap width when a maximum line length is configured, and the
        // parent split pane reads it to derive the character grid NAWS reports.
        let advance = match state.advance {
            Some((generation, probe_size, advance))
                if generation == prefs.generation && probe_size == font_size =>
            {
                advance
            }
            _ => {
                let probe = Renderer::Paragraph::with_text(iced::advanced::text::Text {
                    content: ADVANCE_PROBE,
                    bounds: iced::Size::new(f32::INFINITY, f32::INFINITY),
                    size: Pixels(font_size),
                    font: prefs.font,
                    line_height: LineHeight::Absolute(Pixels(line_height)),
                    align_x: text::Alignment::Left,
                    align_y: alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::None,
                });
                let advance = probe.min_width() / ADVANCE_PROBE.len() as f32;
                state.advance = Some((prefs.generation, font_size, advance));
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

            // A whole-line visibility action has deletion semantics. Omitting
            // the paragraph collapses its layout slot, removes it from hit
            // testing, and prevents selections from being initiated on it.
            if state.hidden_lines.contains(&line_number) {
                continue;
            }

            let line_selection = selection.for_line(line_number);
            let line_search_selection = search_selection && line_selection.is_some();

            let dynamic_links = line.styled_line.links.iter().any(|link| {
                link.style.as_ref().is_some_and(|style| style.has_states())
                    || link.action.protocol().is_some()
            });
            let visual_generation = if dynamic_links {
                (
                    state.visual_generation,
                    protocol_state.visual_generation(),
                    buffer_link_state.visual_generation(),
                )
            } else {
                (0, 0, 0)
            };

            // look for a matching cached Paragraph in state.paragraphs[i] or state.paragraphs[i + 1],
            // advancing i by 1 if a match is found; entries shaped under an
            // older prefs generation — or a different effective font size —
            // are always misses
            if let Some(cache) = state.cache.get_mut(i)
                && cache.generation == prefs.generation
                && cache.font_size == font_size
                && cache.visual_generation == visual_generation
                && Arc::ptr_eq(&cache.source, &line.styled_line)
                && cache.selection == line_selection
                && cache.search_selection == line_search_selection
            {
                i += 1;

                if text_bounds.width > cache.max_valid_width
                    || text_bounds.width < cache.paragraph.min_bounds().width
                {
                    cache.paragraph.resize(text_bounds);
                    *cache.hidden_blink_paragraphs.borrow_mut() = HiddenBlinkParagraphs::default();
                    cache.max_valid_width = text_bounds.width;
                }

                new_cache.push(cache.clone());

                available_y -= cache.paragraph.min_height();
                continue;
            }

            let rendered = if dynamic_links {
                line.spans_with_link_state(&prefs, false, |link| {
                    let key = buffer_link_state
                        .key_at(line_number, link)
                        .expect("every rendered link is registered");
                    let protocol = link.action.protocol();
                    let selected = protocol
                        .and_then(|protocol| protocol.selection.as_ref())
                        .is_some_and(|selection| protocol_state.selected(selection));
                    let disabled = link.action.is_disabled()
                        || protocol
                            .and_then(|protocol| protocol.selection.as_ref())
                            .is_some_and(|selection| selection.disabled);
                    let style_state = LinkStyleState {
                        active: state.active_link == Some(key),
                        hover: state.hovered_link == Some(key),
                        focus_visible: state.focus_visible && state.focused_link == Some(key),
                        focus: state.focused_link == Some(key),
                        visited: protocol_state.visited(&link.action),
                        selected,
                        disabled,
                    };
                    LinkRenderStyle {
                        authored: link.style.is_some(),
                        style: link
                            .style
                            .as_ref()
                            .map_or_else(Default::default, |style| style.resolve(style_state)),
                        spoiler_concealed: protocol.is_some_and(|protocol| protocol.spoiler)
                            && !buffer_link_state.spoiler_revealed(key),
                        hidden: buffer_link_state.concealed(key),
                    }
                })
            } else {
                line.rendered_spans()
            };
            let rendered_selection = rendered.offsets.map_selection(line_selection);
            let spans = Spans::with_selection_color(
                rendered.spans,
                rendered_selection,
                line_search_selection.then_some(iced::Color::WHITE),
            );

            let spans_vec = spans.spans();
            let blink_modes = span_blink_modes(&spans_vec);
            let paragraph = Renderer::Paragraph::with_spans(iced::advanced::text::Text {
                content: spans_vec.as_slice(),
                bounds: text_bounds,
                size: Pixels(font_size),
                font: prefs.font,
                line_height: LineHeight::Absolute(Pixels(line_height)),
                align_x: text::Alignment::Left,
                align_y: alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::WordOrGlyph,
            });

            available_y -= paragraph.min_height();

            new_cache.push(ParagraphCache {
                line_number,
                source: line.styled_line.clone(),
                spans,
                offsets: rendered.offsets,
                paragraph,
                hidden_blink_paragraphs: Rc::new(RefCell::new(HiddenBlinkParagraphs::default())),
                blink_modes,
                max_valid_width: text_bounds.width,
                selection: line_selection,
                search_selection: line_search_selection,
                generation: prefs.generation,
                font_size,
                visual_generation,
            });
        }

        state.cache = new_cache;

        // A keyboard tooltip must not keep owning tooltip state after its link
        // scrolls outside this terminal half. Otherwise it draws nowhere yet
        // suppresses every mouse tooltip until some unrelated focus change.
        if keyboard_tooltip_scrolled_out(
            state.link_tooltip_hover.as_ref(),
            &buffer_link_state,
            |line| state.cache.iter().any(|cache| cache.line_number == line),
        ) {
            state.link_tooltip_hover = None;
        }

        layout::atomic(limits, iced::Length::Fill, iced::Length::Fill)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style_defaults: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
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
                    let metadata = span.link.unwrap_or_default();
                    if span.highlight.is_none()
                        && metadata.underline == LinkDecoration::None
                        && metadata.overline == LinkDecoration::None
                        && metadata.strikethrough == LinkDecoration::None
                    {
                        continue;
                    }
                    let regions = cache.paragraph.span_bounds(span_idx);
                    let blink_hidden = match metadata.blink {
                        smudgy_core::session::styled_line::Blink::None => false,
                        smudgy_core::session::styled_line::Blink::Slow => !state.slow_blink_visible,
                        smudgy_core::session::styled_line::Blink::Fast => !state.fast_blink_visible,
                    };

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

                    if !blink_hidden {
                        // Baseline placement per iced's rich_text: the underline
                        // sits at font size plus half the leading, nudged up by
                        // 8% of the font size.
                        let (font_size, line_height) = effective_metrics(&prefs, self.font_size);
                        let underline_y =
                            font_size + (line_height - font_size) / 2.0 - font_size * 0.08;
                        let strike_y = (line_height - font_size) / 2.0 + font_size * 0.55;
                        let overline_y = (line_height - font_size) / 2.0 + 1.0;
                        let color = metadata
                            .decoration_color
                            .or(span.color)
                            .unwrap_or(iced::Color::WHITE);
                        for region in &regions {
                            let region = Rectangle {
                                x: layout.bounds().x + region.x,
                                y: region.y + y,
                                width: region.width,
                                height: region.height,
                            };
                            draw_text_decoration(
                                renderer,
                                region,
                                region.y + underline_y,
                                metadata.underline,
                                color,
                                clipped_viewport,
                            );
                            draw_text_decoration(
                                renderer,
                                region,
                                region.y + overline_y,
                                metadata.overline,
                                color,
                                clipped_viewport,
                            );
                            draw_text_decoration(
                                renderer,
                                region,
                                region.y + strike_y,
                                metadata.strikethrough,
                                color,
                                clipped_viewport,
                            );
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
                                Background::Color(if cache.search_selection {
                                    TERMINAL_SELECTION_BACKGROUND
                                } else {
                                    prefs.palette.selection
                                }),
                            );
                        }
                    }
                }

                let hide_slow = cache.blink_modes & SLOW_BLINK != 0 && !state.slow_blink_visible;
                let hide_fast = cache.blink_modes & FAST_BLINK != 0 && !state.fast_blink_visible;
                let at = iced::Point::new(layout.bounds().x, y);
                if hide_slow || hide_fast {
                    let mut hidden = cache.hidden_blink_paragraphs.borrow_mut();
                    let paragraph = match (hide_slow, hide_fast) {
                        (true, true) => &mut hidden.all,
                        (true, false) => &mut hidden.slow,
                        (false, true) => &mut hidden.fast,
                        (false, false) => unreachable!("a hidden blink rate was required"),
                    }
                    .get_or_insert_with(|| {
                        let spans = hidden_blink_spans(
                            cache.spans.spans().as_slice(),
                            hide_slow,
                            hide_fast,
                        );
                        Renderer::Paragraph::with_spans(iced::advanced::text::Text {
                            content: spans.as_slice(),
                            bounds: cache.paragraph.bounds(),
                            size: cache.paragraph.size(),
                            font: cache.paragraph.font(),
                            line_height: cache.paragraph.line_height(),
                            align_x: cache.paragraph.align_x(),
                            align_y: cache.paragraph.align_y(),
                            shaping: cache.paragraph.shaping(),
                            wrapping: cache.paragraph.wrapping(),
                        })
                    });
                    renderer.fill_paragraph(paragraph, at, iced::Color::WHITE, clipped_viewport);
                } else {
                    renderer.fill_paragraph(
                        &cache.paragraph,
                        at,
                        iced::Color::WHITE,
                        clipped_viewport,
                    );
                }
            }

            if let Some(popup) = &state.menu_popup {
                draw_link_menu(renderer, &prefs, clipped_viewport, cursor, popup);
            } else if let Some(LinkTooltipHover::Open { link, origin }) =
                state.link_tooltip_hover.as_ref()
            {
                let anchor = match *origin {
                    LinkTooltipOrigin::Mouse => cursor.position().filter(|_| {
                        cursor
                            .position_in(layout.bounds())
                            .and_then(|position| state.hit_test(layout.bounds(), position))
                            .and_then(|position| {
                                self.terminal_buffer
                                    .link_span_at(position.line, position.column)
                            })
                            .is_some_and(|span| {
                                link.action == span.action
                                    && span.tooltip.as_ref() == Some(&link.tooltip)
                            })
                    }),
                    LinkTooltipOrigin::Keyboard(key) => {
                        self.buffer_link_state.borrow().position(key).and_then(
                            |(line, begin, end)| {
                                state.link_tooltip_anchor(layout.bounds(), line, begin, end)
                            },
                        )
                    }
                };
                if let Some(anchor) = anchor {
                    draw_link_tooltip(
                        renderer,
                        &prefs,
                        clipped_viewport,
                        anchor,
                        link,
                        usize::try_from(
                            state.blink_epoch.elapsed().as_millis()
                                / u128::from(TOOLTIP_SPINNER_FRAME_MS),
                        )
                        .unwrap_or(0),
                        &state.link_tooltip_paragraph,
                    );
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            // Pointer over a link span; text cursor elsewhere. The `has_links` guard
            // keeps linkless sessions (the common case) from paying the per-frame
            // hit test at all.
            if self.on_link.is_some() && self.terminal_buffer.has_links() {
                let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
                if let Some(popup) = &state.menu_popup
                    && let Some(clipped_viewport) = layout.bounds().intersection(viewport)
                    && let Some(geometry) = link_menu_geometry::<Renderer>(
                        popup,
                        &crate::prefs::current(),
                        clipped_viewport,
                    )
                    && cursor.is_over(geometry.bounds)
                {
                    return mouse::Interaction::Pointer;
                }
                if let Some(position) = cursor
                    .position_in(layout.bounds())
                    .and_then(|position| state.hit_test(layout.bounds(), position))
                    && self
                        .available_link_at(state, position.line, position.column)
                        .is_some_and(|(_, link)| {
                            link.action.is_interactive() && !link_action_disabled(&link.action)
                        })
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
        viewport: &Rectangle,
    ) {
        let cursor_moved = matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. }));
        let mut mouse_tooltip_request = None;
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
            let (next, visibility_changed) = self
                .buffer_link_state
                .borrow_mut()
                .update_visibility_timers(*now);
            if let Some(next) = next {
                shell.request_redraw_at(next);
            }
            if visibility_changed {
                shell.invalidate_layout();
            }
            if state
                .link_tooltip_hover
                .as_ref()
                .is_some_and(|hover| hover.is_open() && hover.link().tooltip.is_loading())
            {
                shell.request_redraw_at(*now + Duration::from_millis(TOOLTIP_SPINNER_FRAME_MS));
            }
            // Resolve both phases here, so `draw` never consults the
            // preference. A rate that is off screen or switched off resets to
            // visible: a stale `false` would leave a line painted transparent
            // with no timer left to restore it. Zeroing `blink_modes` for the
            // preference also stops the timer.
            let blink_modes = if crate::prefs::current().disable_blink {
                0
            } else {
                state
                    .cache
                    .iter()
                    .fold(0, |modes, cache| modes | cache.blink_modes)
            };
            let elapsed_ms = now.saturating_duration_since(state.blink_epoch).as_millis();
            let mut next_ms = u128::MAX;
            if blink_modes & SLOW_BLINK != 0 {
                state.slow_blink_visible =
                    (elapsed_ms / SLOW_BLINK_HALF_PERIOD_MS).is_multiple_of(2);
                next_ms =
                    next_ms.min(SLOW_BLINK_HALF_PERIOD_MS - elapsed_ms % SLOW_BLINK_HALF_PERIOD_MS);
            } else {
                state.slow_blink_visible = true;
            }
            if blink_modes & FAST_BLINK != 0 {
                state.fast_blink_visible =
                    (elapsed_ms / FAST_BLINK_HALF_PERIOD_MS).is_multiple_of(2);
                next_ms =
                    next_ms.min(FAST_BLINK_HALF_PERIOD_MS - elapsed_ms % FAST_BLINK_HALF_PERIOD_MS);
            } else {
                state.fast_blink_visible = true;
            }
            if blink_modes != 0 {
                shell.request_redraw_at(
                    *now + Duration::from_millis(u64::try_from(next_ms).unwrap_or(1)),
                );
            }
        }
        if matches!(
            event,
            Event::Mouse(_) | Event::Window(window::Event::RedrawRequested(_))
        ) {
            let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
            let mouse_link = (state.menu_popup.is_none() && self.terminal_buffer.has_links())
                .then(|| cursor.position_in(layout.bounds()))
                .flatten()
                .and_then(|position| state.hit_test(layout.bounds(), position))
                .and_then(|position| self.available_link_at(state, position.line, position.column));
            let hovered_key = mouse_link.map(|(key, _)| key);
            if state.hovered_link != hovered_key {
                state.hovered_link = hovered_key;
                state.invalidate_link_styles();
                shell.invalidate_layout();
                shell.request_redraw();
            }
            let hovered = mouse_link.and_then(|(key, link)| {
                if link
                    .action
                    .protocol()
                    .is_some_and(|protocol| protocol.spoiler)
                    && self.buffer_link_state.borrow().spoiler_revealed(key)
                {
                    return None;
                }
                Some(HoveredLinkTooltip {
                    action: link.action.clone(),
                    tooltip: link.tooltip.clone()?,
                })
            });
            if cursor_moved {
                mouse_tooltip_request = hovered.as_ref().and_then(|hovered| {
                    claim_tooltip_request(&hovered.tooltip, self.on_link_tooltip.is_some())
                });
            }
            let now = Instant::now();
            let delay = Duration::from_millis(crate::prefs::current().link_tooltip_delay_ms);
            let previous = state.link_tooltip_hover.take();
            let (next, redraw) =
                update_mouse_link_tooltip(previous, hovered, delay, now, cursor_moved);
            state.link_tooltip_hover = next;
            request_tooltip_redraw(redraw, shell);
        }

        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) {
            let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
            if let Some(popup) = state.menu_popup.clone()
                && let Some(clipped_viewport) = layout.bounds().intersection(viewport)
                && let Some(geometry) = link_menu_geometry::<Renderer>(
                    &popup,
                    &crate::prefs::current(),
                    clipped_viewport,
                )
            {
                let chosen = cursor.position().and_then(|point| {
                    popup.menu.items.iter().zip(&geometry.rows).find_map(
                        |(item, bounds)| match item {
                            LinkMenuItem::Action { action, .. } if bounds.contains(point) => {
                                Some(action.clone())
                            }
                            _ => None,
                        },
                    )
                });
                let inside = cursor.is_over(geometry.bounds);
                let source_is_available = menu_source_is_available(
                    &popup,
                    &self.buffer_link_state.borrow(),
                    &state.hidden_lines,
                );
                state.menu_popup = None;
                shell.invalidate_layout();
                shell.request_redraw();
                if let Some(action) = chosen.filter(|_| source_is_available) {
                    let redraw_at = {
                        let mut protocol_state = self.link_protocol_state.borrow_mut();
                        let mut buffer_link_state = self.buffer_link_state.borrow_mut();
                        apply_menu_choice_protocol(
                            &mut protocol_state,
                            &mut buffer_link_state,
                            popup.source.as_ref(),
                            Instant::now(),
                        )
                    };
                    if let Some(deadline) = redraw_at {
                        shell.request_redraw_at(deadline);
                    }
                    state.invalidate_link_styles();
                    if let Some(on_link) = self.on_link.as_ref() {
                        shell.publish(on_link(LinkClickEvent {
                            action,
                            shift: state.modifiers.shift(),
                            ctrl: state.modifiers.control(),
                            alt: state.modifiers.alt(),
                        }));
                    }
                    shell.capture_event();
                    return;
                }
                if inside {
                    shell.capture_event();
                    return;
                }
            }
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                let had_popup = state.menu_popup.is_some();
                let invocation = if self.on_link.is_some()
                    && let Some(anchor) = cursor.position()
                    && let Some(position) = cursor
                        .position_in(layout.bounds())
                        .and_then(|position| state.hit_test(layout.bounds(), position))
                    && let Some((key, link)) =
                        self.available_link_at(state, position.line, position.column)
                {
                    resolve_link_menu_invocation(
                        state,
                        &mut self.buffer_link_state.borrow_mut(),
                        key,
                        link,
                        Point::new(anchor.x + 2.0, anchor.y + 2.0),
                    )
                } else {
                    LinkMenuInvocation::None
                };
                match invocation {
                    LinkMenuInvocation::Open(popup) => {
                        open_menu_popup(state, *popup, shell);
                        shell.capture_event();
                    }
                    LinkMenuInvocation::RevealSpoiler => {
                        state.menu_popup = None;
                        shell.invalidate_layout();
                        shell.request_redraw();
                        shell.capture_event();
                    }
                    LinkMenuInvocation::None if had_popup => {
                        state.menu_popup = None;
                        shell.invalidate_layout();
                        shell.request_redraw();
                    }
                    LinkMenuInvocation::None => {}
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                if state.clear_keyboard_tooltip() {
                    shell.request_redraw();
                }

                // Null-primary script menus are ordinary left-click menus. Open
                // them on mouse-down so the next frame can paint the popup without
                // waiting for a full click/release cycle. Touch keeps the existing
                // tap-on-release path below so a scroll gesture cannot open one.
                if matches!(
                    event,
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                ) && let Some(anchor) = cursor.position()
                    && let Some(position) = cursor
                        .position_in(layout.bounds())
                        .and_then(|position| state.hit_test(layout.bounds(), position))
                    && let Some((key, link)) =
                        self.available_link_at(state, position.line, position.column)
                    && link.action.opens_menu_on_left_click()
                {
                    let invocation = resolve_link_menu_invocation(
                        state,
                        &mut self.buffer_link_state.borrow_mut(),
                        key,
                        link,
                        Point::new(anchor.x + 2.0, anchor.y + 2.0),
                    );
                    match invocation {
                        LinkMenuInvocation::Open(popup) => {
                            state.is_focused = true;
                            open_menu_popup(state, *popup, shell);
                            // Keep the press uncaptured, like ordinary terminal clicks,
                            // so the parent can still focus this session's command input.
                            return;
                        }
                        LinkMenuInvocation::RevealSpoiler => {
                            state.is_focused = true;
                            state.menu_popup = None;
                            shell.invalidate_layout();
                            shell.request_redraw();
                            return;
                        }
                        LinkMenuInvocation::None => {}
                    }
                }

                let mut selection = self.selection.borrow_mut();

                if let Some(click_position) = cursor.position_in(layout.bounds()) {
                    // Classify this press against the previous one. A press at
                    // the same spot within iced's click window is a double or
                    // triple click. Those select a word or a line instead of
                    // starting a drag.
                    let click = mouse::Click::new(
                        click_position,
                        mouse::Button::Left,
                        state.previous_click,
                    );
                    state.previous_click = Some(click);

                    if let Some(position) = state.hit_test(layout.bounds(), click_position) {
                        if let Some((key, link)) =
                            self.available_link_at(state, position.line, position.column)
                            && !link_action_disabled(&link.action)
                        {
                            state.focused_link = Some(key);
                            state.focus_visible = false;
                            if matches!(event, Event::Mouse(_)) {
                                state.active_link = Some(key);
                            }
                            state.invalidate_link_styles();
                        }

                        // A double click selects the word under the caret. A
                        // triple click selects the line. Both apply at once,
                        // with no drag. A double click inside whitespace falls
                        // back to the single-click path below.
                        //
                        // The word is found in the rendered text, which is
                        // what the user sees. A concealed link renders as
                        // spaces, so a double click on it selects nothing.
                        // The result is mapped back to source offsets, which
                        // is what `Selection` stores.
                        let word_or_line = match click.kind() {
                            mouse::click::Kind::Double => state
                                .rendered_line(position.line)
                                .and_then(|(text, offsets)| {
                                    let column = offsets.source_to_rendered(position.column);
                                    word_span_at(&text, column).map(|(start, end)| {
                                        (
                                            offsets.rendered_to_source(start),
                                            offsets.rendered_to_source(end),
                                        )
                                    })
                                })
                                .map(|(start, end)| {
                                    (
                                        BufferPosition {
                                            line: position.line,
                                            column: start,
                                        },
                                        BufferPosition {
                                            line: position.line,
                                            column: end,
                                        },
                                    )
                                }),
                            mouse::click::Kind::Triple => Some((
                                BufferPosition {
                                    line: position.line,
                                    column: 0,
                                },
                                BufferPosition {
                                    line: position.line,
                                    column: usize::MAX,
                                },
                            )),
                            mouse::click::Kind::Single => None,
                        };

                        if let Some((from, to)) = word_or_line {
                            // `pressed_cell` stays `None` on purpose. The
                            // release handler reads it to decide whether a
                            // plain click activated a link. The first press of
                            // a double click already took the single-click path
                            // and fired any link under the word on its release.
                            // The second press must not fire it again.
                            *selection = Selection::Selected { from, to };
                        } else {
                            state.pressed_cell = Some(position.clone());
                            *selection = Selection::Selecting {
                                origin: position.clone(),
                                from: position.clone(),
                                to: position,
                            };
                        }
                        // The press hands the shared selection to the user:
                        // search no longer owns it, so the search styling
                        // must not apply and dismissing search must not
                        // revert it (see `SessionInput::exit_search`).
                        self.search_selection.set(false);
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
                if state.active_link.take().is_some() {
                    state.invalidate_link_styles();
                    shell.invalidate_layout();
                }

                // A click is a press and release resolving to the SAME buffer cell
                // (`pressed_cell` survives only while the pointer stays on it): a drag
                // ends elsewhere, and content scrolling under a stationary cursor
                // moves the release onto a different absolute line — neither fires.
                if let Some(pressed) = state.pressed_cell.take()
                    && self.on_link.is_some()
                    && self.terminal_buffer.has_links()
                    && let Some(position) = cursor
                        .position_in(layout.bounds())
                        .and_then(|position| state.hit_test(layout.bounds(), position))
                    && position == pressed
                    && let Some((key, link)) =
                        self.available_link_at(state, position.line, position.column)
                {
                    let modifiers = state.modifiers;
                    self.activate_link(
                        state,
                        key,
                        link,
                        cursor
                            .position()
                            .map(|anchor| Point::new(anchor.x + 2.0, anchor.y + 2.0)),
                        modifiers,
                        Instant::now(),
                        shell,
                    );
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

                if state.menu_popup.is_some() {
                    shell.request_redraw();
                }

                if let Some(on_tooltip) = self.on_link_tooltip.as_ref()
                    && let Some(request) = mouse_tooltip_request
                {
                    on_tooltip(request);
                    shell.request_redraw();
                }

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

                if matches!(
                    key.as_ref(),
                    keyboard::Key::Named(keyboard::key::Named::Escape)
                ) && state.dismiss_link_overlay()
                {
                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                    return;
                }

                if state.is_focused {
                    match key.as_ref() {
                        keyboard::Key::Named(keyboard::key::Named::Tab)
                            if self.terminal_buffer.has_links() =>
                        {
                            let buffer_link_state = self.buffer_link_state.borrow();
                            let links: Vec<_> = self
                                .terminal_buffer
                                .link_keys()
                                .filter(|key| {
                                    link_key_available(
                                        *key,
                                        &buffer_link_state,
                                        &state.hidden_lines,
                                    ) && self
                                        .terminal_buffer
                                        .link_span(*key)
                                        .is_some_and(|link| !link_action_disabled(&link.action))
                                })
                                .collect();
                            if !links.is_empty() {
                                let current = state.focused_link.and_then(|focused| {
                                    links.iter().position(|key| *key == focused)
                                });
                                let index =
                                    link_navigation_index(links.len(), current, modifiers.shift())
                                        .expect("the link list was checked as non-empty");
                                let focused = links[index];
                                let link = self
                                    .terminal_buffer
                                    .link_span(focused)
                                    .expect("a key from the buffer resolves to its link");
                                state.focused_link = Some(focused);
                                state.focus_visible = true;
                                state.link_tooltip_hover =
                                    link.tooltip.clone().and_then(|tooltip| {
                                        let revealed = link
                                            .action
                                            .protocol()
                                            .is_some_and(|protocol| protocol.spoiler)
                                            && buffer_link_state.spoiler_revealed(focused);
                                        (!revealed).then_some(LinkTooltipHover::Open {
                                            link: HoveredLinkTooltip {
                                                action: link.action.clone(),
                                                tooltip,
                                            },
                                            origin: LinkTooltipOrigin::Keyboard(focused),
                                        })
                                    });
                                drop(buffer_link_state);
                                if state.link_tooltip_hover.is_some()
                                    && let Some(on_tooltip) = self.on_link_tooltip.as_ref()
                                    && let Some(request) = link
                                        .tooltip
                                        .as_ref()
                                        .and_then(|tooltip| claim_tooltip_request(tooltip, true))
                                {
                                    on_tooltip(request);
                                }
                                state.invalidate_link_styles();
                                shell.invalidate_layout();
                                shell.request_redraw();
                            }
                            shell.capture_event();
                        }
                        keyboard::Key::Named(
                            keyboard::key::Named::Enter | keyboard::key::Named::Space,
                        ) if let Some(focused) = state.keyboard_focused_link()
                            && let Some(link) = self.terminal_buffer.link_span(focused) =>
                        {
                            self.activate_link(
                                state,
                                focused,
                                link,
                                Some(layout.bounds().center()),
                                *modifiers,
                                Instant::now(),
                                shell,
                            );
                            shell.capture_event();
                        }
                        keyboard::Key::Named(keyboard::key::Named::ContextMenu)
                        | keyboard::Key::Named(keyboard::key::Named::F10)
                            if (matches!(
                                key.as_ref(),
                                keyboard::Key::Named(keyboard::key::Named::ContextMenu)
                            ) || modifiers.shift())
                                && let Some(focused) = state.keyboard_focused_link()
                                && let Some(link) = self.terminal_buffer.link_span(focused)
                                && !link_action_disabled(&link.action)
                                && link.action.menu().is_some() =>
                        {
                            let invocation = resolve_link_menu_invocation(
                                state,
                                &mut self.buffer_link_state.borrow_mut(),
                                focused,
                                link,
                                layout.bounds().center(),
                            );
                            match invocation {
                                LinkMenuInvocation::Open(popup) => {
                                    open_menu_popup(state, *popup, shell);
                                }
                                LinkMenuInvocation::RevealSpoiler => {
                                    state.menu_popup = None;
                                    shell.invalidate_layout();
                                    shell.request_redraw();
                                }
                                LinkMenuInvocation::None => {}
                            }
                            shell.capture_event();
                        }
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

pub fn terminal_pane<'a, Message>(
    buffer: Ref<'a, TerminalBuffer>,
    selection: Rc<RefCell<Selection>>,
) -> TerminalPane<'a, Message> {
    TerminalPane::new(buffer, selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_core::session::{
        SessionId,
        styled_line::{Blink, LinkTooltipState},
    };

    fn blinking_span(blink: Blink) -> iced::widget::text::Span<'static, Link> {
        iced::widget::text::Span::new("blink")
            .color(iced::Color::WHITE)
            .link(SpanMetadata {
                blink,
                ..SpanMetadata::default()
            })
    }

    #[test]
    fn blink_variants_hide_only_the_requested_rates() {
        let spans = [blinking_span(Blink::Slow), blinking_span(Blink::Fast)];
        assert_eq!(span_blink_modes(&spans), SLOW_BLINK | FAST_BLINK);

        let slow = hidden_blink_spans(&spans, true, false);
        assert_eq!(slow[0].color, Some(iced::Color::TRANSPARENT));
        assert_eq!(slow[1].color, Some(iced::Color::WHITE));

        let fast = hidden_blink_spans(&spans, false, true);
        assert_eq!(fast[0].color, Some(iced::Color::WHITE));
        assert_eq!(fast[1].color, Some(iced::Color::TRANSPARENT));

        let all = hidden_blink_spans(&spans, true, true);
        assert!(
            all.iter()
                .all(|span| span.color == Some(iced::Color::TRANSPARENT))
        );
    }

    #[test]
    fn blink_probe_skips_ordinary_spans() {
        let spans = [iced::widget::text::Span::new("ordinary")];
        assert_eq!(span_blink_modes(&spans), 0);
    }

    #[test]
    fn selected_callback_treats_hash_as_command_data() {
        let action = with_selected_callback(
            LinkAction::ServerSend(Arc::from("vote?choice=yes#receipt")),
            true,
        );
        assert_eq!(
            action,
            LinkAction::ServerSend(Arc::from("vote?choice=yes#receipt&selected=true"))
        );
    }

    #[test]
    fn selected_callback_replaces_duplicates_and_does_not_mutate_urls() {
        assert_eq!(
            with_selected_callback(
                LinkAction::Prompt(Arc::from(
                    "say #staff?selected=true&channel=staff&selected=stale",
                )),
                false,
            ),
            LinkAction::Prompt(Arc::from("say #staff?channel=staff&selected=false"))
        );

        let url = LinkAction::OpenUrl(Arc::from("https://example.test/path?selected=false"));
        assert_eq!(with_selected_callback(url.clone(), true), url);
    }

    #[test]
    fn loading_tooltip_animates_and_retains_the_disclosed_target() {
        let target: Arc<str> = Arc::from("send:inspect");
        let (first, first_target) = loading_tooltip_display(0, Some(target.clone()));
        let (second, second_target) = loading_tooltip_display(1, Some(target.clone()));

        assert!(first.text.starts_with("| "));
        assert!(second.text.starts_with("/ "));
        assert_ne!(first.text, second.text);
        assert_eq!(first_target.as_deref(), Some(target.as_ref()));
        assert_eq!(second_target.as_deref(), Some(target.as_ref()));
    }

    fn hovered_tooltip(label: &str) -> HoveredLinkTooltip {
        HoveredLinkTooltip {
            action: LinkAction::Send(Arc::from("look")),
            tooltip: LinkTooltip::text(Arc::from(label.to_owned()), None),
        }
    }

    #[test]
    fn keyboard_tooltip_survives_mouse_and_redraw_recomputation() {
        let now = Instant::now();
        let key = LinkKey::test(7);
        let keyboard = LinkTooltipHover::Open {
            link: hovered_tooltip("keyboard"),
            origin: LinkTooltipOrigin::Keyboard(key),
        };

        for hovered in [None, Some(hovered_tooltip("mouse"))] {
            let (next, redraw) = update_mouse_link_tooltip(
                Some(keyboard.clone()),
                hovered,
                Duration::from_millis(200),
                now,
                true,
            );
            assert_eq!(next, Some(keyboard.clone()));
            assert_eq!(redraw, TooltipRedraw::default());
        }
    }

    #[test]
    fn mouse_tooltip_waits_opens_redraws_and_clears_as_before() {
        let now = Instant::now();
        let delay = Duration::from_millis(200);
        let link = hovered_tooltip("mouse");

        let (waiting, redraw) =
            update_mouse_link_tooltip(None, Some(link.clone()), delay, now, false);
        assert!(matches!(
            waiting,
            Some(LinkTooltipHover::Waiting {
                origin: LinkTooltipOrigin::Mouse,
                at,
                ..
            }) if at == now
        ));
        assert_eq!(
            redraw,
            TooltipRedraw {
                now: false,
                at: Some(now + delay),
            }
        );

        let (open, redraw) =
            update_mouse_link_tooltip(waiting, Some(link), delay, now + delay, false);
        assert!(matches!(
            open,
            Some(LinkTooltipHover::Open {
                origin: LinkTooltipOrigin::Mouse,
                ..
            })
        ));
        assert_eq!(
            redraw,
            TooltipRedraw {
                now: true,
                at: None,
            }
        );

        let (moved, redraw) = update_mouse_link_tooltip(
            open,
            Some(hovered_tooltip("mouse")),
            delay,
            now + delay,
            true,
        );
        assert!(moved.is_some());
        assert!(redraw.now);

        let (cleared, redraw) = update_mouse_link_tooltip(moved, None, delay, now + delay, false);
        assert!(cleared.is_none());
        assert!(redraw.now);
    }

    #[test]
    fn keyboard_tooltip_clears_without_disturbing_mouse_tooltip() {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;

        let key = LinkKey::test(7);
        let mut state = State::<Paragraph> {
            link_tooltip_hover: Some(LinkTooltipHover::Open {
                link: hovered_tooltip("keyboard"),
                origin: LinkTooltipOrigin::Keyboard(key),
            }),
            ..State::default()
        };
        assert!(state.clear_keyboard_tooltip());
        assert!(state.link_tooltip_hover.is_none());

        let mouse = LinkTooltipHover::Open {
            link: hovered_tooltip("mouse"),
            origin: LinkTooltipOrigin::Mouse,
        };
        state.link_tooltip_hover = Some(mouse.clone());
        assert!(!state.clear_keyboard_tooltip());
        assert_eq!(state.link_tooltip_hover, Some(mouse));
    }

    #[test]
    fn escape_dismisses_menu_and_keyboard_tooltip_together() {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;

        let key = LinkKey::test(7);
        let mut state = State::<Paragraph> {
            menu_popup: Some(LinkMenuPopup {
                menu: one_row_menu(),
                anchor: Point::ORIGIN,
                source: None,
            }),
            link_tooltip_hover: Some(LinkTooltipHover::Open {
                link: hovered_tooltip("keyboard"),
                origin: LinkTooltipOrigin::Keyboard(key),
            }),
            ..State::default()
        };

        assert!(state.dismiss_link_overlay());
        assert!(state.menu_popup.is_none());
        assert!(state.link_tooltip_hover.is_none());
    }

    #[test]
    fn rendered_span_overlap_uses_half_open_byte_ranges() {
        assert!(byte_ranges_overlap(0, 4, 0, 4));
        assert!(byte_ranges_overlap(0, 4, 2, 6));
        assert!(byte_ranges_overlap(4, 8, 2, 6));
        assert!(!byte_ranges_overlap(0, 4, 4, 8));
        assert!(!byte_ranges_overlap(8, 12, 4, 8));
    }

    #[test]
    fn keyboard_callback_tooltip_enters_loading_spinner_state() {
        let shared = Arc::new(LinkTooltipState::default());
        let tooltip = LinkTooltip::callback(
            LinkTooltipCallback {
                session: SessionId::from(1_u32),
                isolate_token: Arc::from("main"),
                id: 7,
                token: Arc::new(smudgy_core::session::styled_line::LinkToken::default()),
                state: Arc::clone(&shared),
            },
            Some(Arc::from("send:look")),
        );
        assert!(tooltip.request().is_some());
        assert!(tooltip.is_loading());
        assert!(tooltip.request().is_none());

        let key = LinkKey::test(7);
        let hover = LinkTooltipHover::Open {
            link: HoveredLinkTooltip {
                action: LinkAction::Send(Arc::from("look")),
                tooltip,
            },
            origin: LinkTooltipOrigin::Keyboard(key),
        };
        assert!(hover.is_open());
        assert!(hover.is_keyboard());
        assert!(hover.link().tooltip.is_loading());
        let (display, target) = loading_tooltip_display(0, hover.link().tooltip.target.clone());
        assert!(display.text.starts_with("| "));
        assert_eq!(target.as_deref(), Some("send:look"));
    }

    #[test]
    fn callback_tooltip_is_not_claimed_without_a_runtime_handler() {
        let shared = Arc::new(LinkTooltipState::default());
        let tooltip = LinkTooltip::callback(
            LinkTooltipCallback {
                session: SessionId::from(1_u32),
                isolate_token: Arc::from("main"),
                id: 9,
                token: Arc::new(smudgy_core::session::styled_line::LinkToken::default()),
                state: shared,
            },
            Some(Arc::from("send:look")),
        );

        assert!(claim_tooltip_request(&tooltip, false).is_none());
        assert!(!tooltip.is_loading());
        assert!(
            claim_tooltip_request(&tooltip, true).is_some(),
            "the available handler must still be able to claim the first request"
        );
        assert!(tooltip.is_loading());
    }

    #[test]
    fn keyboard_activation_requires_visible_link_focus_and_editor_focus_clears_it() {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;

        let key = LinkKey::test(7);
        let mut state = State::<Paragraph> {
            is_focused: true,
            focused_link: Some(key),
            focus_visible: false,
            ..State::default()
        };

        assert_eq!(state.keyboard_focused_link(), None);
        state.focus_visible = true;
        assert_eq!(state.keyboard_focused_link(), Some(key));
        state.link_tooltip_hover = Some(LinkTooltipHover::Open {
            link: hovered_tooltip("keyboard"),
            origin: LinkTooltipOrigin::Keyboard(key),
        });

        let generation = state.visual_generation;
        state.process_link_navigation_reset(1);
        assert!(!state.is_focused);
        assert_eq!(state.focused_link, None);
        assert!(!state.focus_visible);
        assert!(state.link_tooltip_hover.is_none());
        assert_ne!(state.visual_generation, generation);
    }

    #[test]
    fn keyboard_link_navigation_skips_current_and_wraps_in_both_directions() {
        assert_eq!(link_navigation_index(0, None, false), None);
        assert_eq!(link_navigation_index(2, None, false), Some(0));
        assert_eq!(link_navigation_index(2, Some(0), false), Some(1));
        assert_eq!(link_navigation_index(2, Some(1), false), Some(0));

        assert_eq!(link_navigation_index(2, None, true), Some(1));
        assert_eq!(link_navigation_index(2, Some(1), true), Some(0));
        assert_eq!(link_navigation_index(2, Some(0), true), Some(1));
    }

    fn link_span(action: LinkAction) -> LinkSpan {
        LinkSpan {
            begin_pos: 2,
            end_pos: 8,
            action,
            tooltip: None,
            style: None,
        }
    }

    fn protocol_states(
        line_number: usize,
        link: &LinkSpan,
    ) -> (Rc<RefCell<LinkProtocolState>>, BufferLinkState, LinkKey) {
        let shared = Rc::new(RefCell::new(LinkProtocolState::default()));
        let mut buffer = BufferLinkState::new(shared.clone());
        let mut line = StyledLine::new("0123456789", Vec::new());
        line.links.push(link.clone());
        buffer.replace_line(line_number, Some(&line));
        let key = buffer
            .key_at(line_number, link)
            .expect("registered test link");
        (shared, buffer, key)
    }

    fn one_row_menu() -> LinkMenu {
        LinkMenu {
            title: None,
            items: Arc::from(vec![LinkMenuItem::Action {
                label: Arc::from("Choose"),
                action: LinkAction::Send(Arc::from("choose")),
            }]),
        }
    }

    #[test]
    fn activation_resolver_applies_selection_to_the_published_primary_once() {
        use smudgy_core::session::styled_line::{LinkProtocol, LinkSelection};

        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let selection = LinkSelection {
            group: Arc::from("difficulty"),
            value: Arc::from("hard"),
            toggle: true,
            selected: false,
            exclusive: true,
            disabled: false,
        };
        let link = link_span(LinkAction::Configured {
            primary: Some(Box::new(LinkAction::ServerSend(Arc::from("hard")))),
            disabled: false,
            primary_enabled: true,
            menu: None,
            menu_on_left_click: false,
            protocol: Some(LinkProtocol {
                selection: Some(selection.clone()),
                ..LinkProtocol::default()
            }),
        });
        let mut state = State::<Paragraph>::default();
        let (protocol_state, mut buffer_link_state, key) = protocol_states(7, &link);

        let activation = {
            let mut protocol_state = protocol_state.borrow_mut();
            resolve_link_activation(
                &mut state,
                &mut protocol_state,
                &mut buffer_link_state,
                LinkActivationRequest {
                    key,
                    link: &link,
                    menu_anchor: Some(Point::new(12.0, 24.0)),
                    modifiers: keyboard::Modifiers::default(),
                    now: Instant::now(),
                    can_publish: true,
                },
            )
        };

        let LinkActivationOutcome::Publish(event) = activation.outcome else {
            panic!("an enabled primary must publish");
        };
        assert_eq!(
            event.action,
            LinkAction::ServerSend(Arc::from("hard?selected=true"))
        );
        assert!(protocol_state.borrow().selected(&selection));
        assert!(protocol_state.borrow().visited(&link.action));
    }

    #[test]
    fn activation_resolver_blocks_disabled_selection_links() {
        use smudgy_core::session::styled_line::{LinkProtocol, LinkSelection};

        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let selection = LinkSelection {
            group: Arc::from("difficulty"),
            value: Arc::from("hard"),
            toggle: true,
            selected: false,
            exclusive: true,
            disabled: true,
        };
        let link = link_span(LinkAction::Configured {
            primary: Some(Box::new(LinkAction::ServerSend(Arc::from("hard")))),
            disabled: false,
            primary_enabled: true,
            menu: Some(one_row_menu()),
            menu_on_left_click: true,
            protocol: Some(LinkProtocol {
                selection: Some(selection.clone()),
                ..LinkProtocol::default()
            }),
        });
        let mut state = State::<Paragraph>::default();
        let (protocol_state, mut buffer_link_state, key) = protocol_states(7, &link);

        let activation = {
            let mut protocol_state = protocol_state.borrow_mut();
            resolve_link_activation(
                &mut state,
                &mut protocol_state,
                &mut buffer_link_state,
                LinkActivationRequest {
                    key,
                    link: &link,
                    menu_anchor: Some(Point::new(12.0, 24.0)),
                    modifiers: keyboard::Modifiers::default(),
                    now: Instant::now(),
                    can_publish: true,
                },
            )
        };

        assert!(matches!(activation.outcome, LinkActivationOutcome::None));
        assert!(!protocol_state.borrow().selected(&selection));
        assert!(!protocol_state.borrow().visited(&link.action));
    }

    #[test]
    fn activation_resolver_blocks_concealed_links_and_hidden_lines() {
        use smudgy_core::session::styled_line::{
            LinkProtocol, LinkVisibility, LinkVisibilityAction, LinkVisibilityExpire,
        };

        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let link = link_span(LinkAction::Configured {
            primary: Some(Box::new(LinkAction::ServerSend(Arc::from("secret")))),
            disabled: false,
            primary_enabled: true,
            menu: None,
            menu_on_left_click: false,
            protocol: Some(LinkProtocol {
                visibility: Some(LinkVisibility {
                    action: LinkVisibilityAction::Reveal,
                    delay_ms: None,
                    expire: LinkVisibilityExpire {
                        input: false,
                        prompt: false,
                        output: false,
                        output_delay_ms: 0,
                    },
                    whole_line: false,
                }),
                ..LinkProtocol::default()
            }),
        });
        let mut state = State::<Paragraph>::default();
        let (protocol_state, mut buffer_link_state, key) = protocol_states(7, &link);
        assert!(buffer_link_state.concealed(key));

        let activate = |state: &mut State<Paragraph>, buffer_link_state: &mut BufferLinkState| {
            let mut protocol_state = protocol_state.borrow_mut();
            resolve_link_activation(
                state,
                &mut protocol_state,
                buffer_link_state,
                LinkActivationRequest {
                    key,
                    link: &link,
                    menu_anchor: None,
                    modifiers: keyboard::Modifiers::default(),
                    now: Instant::now(),
                    can_publish: true,
                },
            )
        };
        assert!(matches!(
            activate(&mut state, &mut buffer_link_state).outcome,
            LinkActivationOutcome::None
        ));

        let ordinary = link_span(LinkAction::ServerSend(Arc::from("co-located")));
        let (ordinary_protocol, mut ordinary_buffer, ordinary_key) = protocol_states(9, &ordinary);
        state.hidden_lines.insert(9);
        let hidden_line_activation = {
            let mut ordinary_protocol = ordinary_protocol.borrow_mut();
            resolve_link_activation(
                &mut state,
                &mut ordinary_protocol,
                &mut ordinary_buffer,
                LinkActivationRequest {
                    key: ordinary_key,
                    link: &ordinary,
                    menu_anchor: None,
                    modifiers: keyboard::Modifiers::default(),
                    now: Instant::now(),
                    can_publish: true,
                },
            )
        };
        assert!(matches!(
            hidden_line_activation.outcome,
            LinkActivationOutcome::None
        ));
        assert!(!protocol_state.borrow().visited(&link.action));
        assert!(!ordinary_protocol.borrow().visited(&ordinary.action));
    }

    #[test]
    fn menu_open_defers_protocol_state_until_the_row_is_selected() {
        use smudgy_core::session::styled_line::{
            LinkProtocol, LinkSelection, LinkVisibility, LinkVisibilityAction, LinkVisibilityExpire,
        };

        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let selection = LinkSelection {
            group: Arc::from("answer"),
            value: Arc::from("yes"),
            toggle: true,
            selected: false,
            exclusive: true,
            disabled: false,
        };
        let link = link_span(LinkAction::Configured {
            primary: None,
            disabled: false,
            primary_enabled: true,
            menu: Some(one_row_menu()),
            menu_on_left_click: true,
            protocol: Some(LinkProtocol {
                selection: Some(selection.clone()),
                visibility: Some(LinkVisibility {
                    action: LinkVisibilityAction::Conceal,
                    delay_ms: Some(25),
                    expire: LinkVisibilityExpire {
                        input: false,
                        prompt: false,
                        output: false,
                        output_delay_ms: 0,
                    },
                    whole_line: false,
                }),
                ..LinkProtocol::default()
            }),
        });
        let anchor = Point::new(320.0, 180.0);
        let now = Instant::now();
        let mut state = State::<Paragraph>::default();
        let (protocol_state, mut buffer_link_state, key) = protocol_states(11, &link);

        let activation = {
            let mut protocol_state = protocol_state.borrow_mut();
            resolve_link_activation(
                &mut state,
                &mut protocol_state,
                &mut buffer_link_state,
                LinkActivationRequest {
                    key,
                    link: &link,
                    menu_anchor: Some(anchor),
                    modifiers: keyboard::Modifiers::default(),
                    now,
                    can_publish: true,
                },
            )
        };

        let LinkActivationOutcome::OpenMenu(popup) = activation.outcome else {
            panic!("Enter/Space must open a configured left-click menu");
        };
        assert_eq!(popup.anchor, anchor);
        assert_eq!(popup.source.as_ref().map(|(key, _)| *key), Some(key));
        assert!(!protocol_state.borrow().selected(&selection));
        assert!(!protocol_state.borrow().visited(&link.action));
        assert!(!buffer_link_state.concealed(key));
        assert_eq!(activation.redraw_at, None);

        let redraw_at = apply_menu_choice_protocol(
            &mut protocol_state.borrow_mut(),
            &mut buffer_link_state,
            popup.source.as_ref(),
            now,
        );
        let deadline = now + Duration::from_millis(25);
        assert_eq!(redraw_at, Some(deadline));
        assert!(protocol_state.borrow().selected(&selection));
        assert!(protocol_state.borrow().visited(&link.action));
        assert!(!buffer_link_state.concealed(key));

        assert_eq!(
            buffer_link_state.update_visibility_timers(deadline),
            (None, true)
        );
        assert!(buffer_link_state.concealed(key));
    }

    #[test]
    fn menu_invocation_reveals_a_spoiler_before_opening_its_popup() {
        use smudgy_core::session::styled_line::LinkProtocol;

        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let link = link_span(LinkAction::Configured {
            primary: None,
            disabled: false,
            primary_enabled: true,
            menu: Some(one_row_menu()),
            menu_on_left_click: true,
            protocol: Some(LinkProtocol {
                spoiler: true,
                ..LinkProtocol::default()
            }),
        });
        let anchor = Point::new(320.0, 180.0);
        let mut state = State::<Paragraph>::default();
        let (_, mut buffer_link_state, key) = protocol_states(11, &link);

        assert!(matches!(
            resolve_link_menu_invocation(&mut state, &mut buffer_link_state, key, &link, anchor,),
            LinkMenuInvocation::RevealSpoiler
        ));
        assert!(buffer_link_state.spoiler_revealed(key));

        let LinkMenuInvocation::Open(popup) =
            resolve_link_menu_invocation(&mut state, &mut buffer_link_state, key, &link, anchor)
        else {
            panic!("the second menu invocation should open the revealed spoiler");
        };
        assert_eq!(popup.anchor, anchor);
        assert_eq!(popup.source.as_ref().map(|(key, _)| *key), Some(key));
    }

    #[test]
    fn popup_installation_clears_pressed_cell_through_the_common_path() {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;

        let mut state = State::<Paragraph> {
            pressed_cell: Some(BufferPosition { line: 4, column: 9 }),
            ..State::default()
        };
        state.install_menu_popup(LinkMenuPopup {
            menu: one_row_menu(),
            anchor: Point::new(10.0, 20.0),
            source: None,
        });

        assert!(state.menu_popup.is_some());
        assert_eq!(state.pressed_cell, None);
        assert!(state.link_tooltip_hover.is_none());
    }

    #[test]
    fn evicted_source_prunes_its_open_menu_and_blocks_choice_bookkeeping() {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;

        let link = link_span(LinkAction::Send(Arc::from("look")));
        let (protocol_state, mut live, key) = protocol_states(7, &link);
        let popup = LinkMenuPopup {
            menu: one_row_menu(),
            anchor: Point::new(10.0, 20.0),
            source: Some((key, link.action.clone())),
        };
        let mut state = State::<Paragraph> {
            menu_popup: Some(popup.clone()),
            ..State::default()
        };
        assert!(menu_source_is_available(&popup, &live, &HashSet::new()));

        live.replace_line(7, None);
        assert!(!menu_source_is_available(&popup, &live, &HashSet::new()));
        state.prune_widget_links(&live);
        assert!(state.menu_popup.is_none());
        assert!(!protocol_state.borrow().visited(&link.action));
    }

    #[test]
    fn keyboard_tooltip_detects_when_its_link_scrolls_out() {
        let link = link_span(LinkAction::Send(Arc::from("look")));
        let (_, live, key) = protocol_states(7, &link);
        let hover = LinkTooltipHover::Open {
            link: hovered_tooltip("keyboard"),
            origin: LinkTooltipOrigin::Keyboard(key),
        };

        assert!(!keyboard_tooltip_scrolled_out(
            Some(&hover),
            &live,
            |line| line == 7
        ));
        assert!(keyboard_tooltip_scrolled_out(Some(&hover), &live, |_| {
            false
        }));
    }

    #[test]
    fn visited_and_selection_state_are_semantic_and_stable() {
        use smudgy_core::session::styled_line::{LinkProtocol, LinkSelection};

        let selection = LinkSelection {
            group: Arc::from("difficulty"),
            value: Arc::from("hard"),
            toggle: true,
            selected: false,
            exclusive: true,
            disabled: false,
        };
        let action = LinkAction::Configured {
            primary: Some(Box::new(LinkAction::ServerSend(Arc::from("hard")))),
            disabled: false,
            primary_enabled: true,
            menu: None,
            menu_on_left_click: false,
            protocol: Some(LinkProtocol {
                selection: Some(selection.clone()),
                ..LinkProtocol::default()
            }),
        };
        let link = link_span(action.clone());
        let (protocol_state, _, _) = protocol_states(3, &link);

        assert!(!protocol_state.borrow().visited(&action));
        protocol_state.borrow_mut().mark_visited(&action);
        let generation = protocol_state.borrow().visual_generation();
        protocol_state.borrow_mut().mark_visited(&action);
        assert!(protocol_state.borrow().visited(&action));
        assert_eq!(protocol_state.borrow().visual_generation(), generation);

        assert!(!protocol_state.borrow().selected(&selection));
        assert_eq!(
            protocol_state.borrow_mut().toggle_link_selection(&action),
            Some(true)
        );
        assert!(protocol_state.borrow().selected(&selection));
        assert_eq!(
            protocol_state.borrow_mut().toggle_link_selection(&action),
            Some(false)
        );
        assert!(!protocol_state.borrow().selected(&selection));
    }
}
