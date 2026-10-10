use std::{
    cell::{Cell, Ref, RefCell},
    collections::VecDeque,
    rc::{self, Rc},
};

use crate::terminal_buffer::{LinkClickEvent, TerminalBuffer, selection::Selection};
use iced::{
    Element, Event, Point, Rectangle, Size,
    advanced::{
        Clipboard, Layout, Shell, Widget,
        layout::{self, Node},
        mouse, text,
        widget::{Tree, tree},
    },
    time::Instant,
    window,
};
use smudgy_session_model::styled_line::LinkTooltipCallback;

mod scroll_bar;
pub(crate) mod terminal_pane;

use terminal_pane::{TerminalPane, terminal_pane};

const SPLIT_LIVE_TAIL_MAX_HEIGHT: f32 = 200.0;

/// How scrollback is presented after the user scrolls away from the latest
/// line. The main terminal keeps its live tail, while script-created terminal
/// panes use the whole pane for the historical view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrolledLayout {
    /// Preserve the existing live-output region beneath the historical view.
    SplitWithLiveTail,
    /// Give the historical view the full terminal region.
    FullPane,
}

/// A keyboard/search request issued by the command editor for its associated
/// terminal viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollRequest {
    PageUp,
    PageDown,
    Pages(i32),
    Lines(i32),
    Home,
    End,
    RevealLine(usize),
}

/// Shared bridge between one terminal's command editor and the custom
/// terminal widget state. Requests are queued because repeated page keys can
/// arrive before the next layout pass.
#[derive(Debug, Clone, Default)]
pub struct ScrollHandle {
    requests: Rc<RefCell<VecDeque<ScrollRequest>>>,
}

impl ScrollHandle {
    pub fn request(&self, request: ScrollRequest) {
        self.requests.borrow_mut().push_back(request);
    }

    pub fn take_requests(&self) -> VecDeque<ScrollRequest> {
        std::mem::take(&mut *self.requests.borrow_mut())
    }
}

/// The selection and viewport control shared by one terminal widget and its
/// associated command editor. `search_selection` distinguishes the temporary
/// search result without carrying every match into the renderer; it also
/// tracks who owns the shared selection — a mouse press in the terminal
/// clears it, handing the selection (and its normal styling) back to the
/// user until the next search navigation re-claims it.
#[derive(Debug, Clone, Default)]
pub struct TerminalViewHandle {
    pub selection: Rc<RefCell<Selection>>,
    pub search_selection: Rc<Cell<bool>>,
    pub scroll: ScrollHandle,
}

impl ScrolledLayout {
    const fn live_tail_max_height(self) -> f32 {
        match self {
            Self::SplitWithLiveTail => SPLIT_LIVE_TAIL_MAX_HEIGHT,
            Self::FullPane => 0.0,
        }
    }

    const fn keeps_live_tail(self) -> bool {
        matches!(self, Self::SplitWithLiveTail)
    }
}

struct SplitTerminalPane<'a, Message> {
    pub view: TerminalViewHandle,
    inline_resolver: Option<crate::inline_object::Resolver>,
    pub buffer: Ref<'a, TerminalBuffer>,
    pub on_link: Option<Rc<dyn Fn(LinkClickEvent) -> Message>>,
    pub on_link_tooltip: Option<Rc<dyn Fn(LinkTooltipCallback)>>,
    /// Called with `(cols, rows)` when the pane's character grid changes — the full
    /// terminal region in cells, quantized so it only fires on actual grid changes.
    /// A plain callback is sufficient because this report does not mutate UI
    /// state; it sends the session's NAWS runtime action directly and is wired
    /// only for the main terminal.
    pub on_grid_change: Option<Rc<dyn Fn(u16, u16)>>,
    /// Per-pane font override (`docs/panes.md`); `None` follows the global
    /// preference. This widget applies it to scrollback; the pane composition
    /// applies the same value to any input line.
    pub font_size: Option<f32>,
    pub scrolled_layout: ScrolledLayout,
}

impl<'a, Message> SplitTerminalPane<'a, Message> {
    pub fn new(
        buffer: Ref<'a, TerminalBuffer>,
        view: TerminalViewHandle,
        scrolled_layout: ScrolledLayout,
    ) -> Self {
        Self {
            view,
            inline_resolver: None,
            buffer,
            on_link: None,
            on_link_tooltip: None,
            on_grid_change: None,
            font_size: None,
            scrolled_layout,
        }
    }

    fn terminal_pane(&self) -> TerminalPane<'a, Message> {
        terminal_pane(Ref::clone(&self.buffer), self.view.selection.clone())
            .search_selection(self.view.search_selection.clone())
            .on_link(self.on_link.clone())
            .on_link_tooltip(self.on_link_tooltip.clone())
            .font_size(self.font_size)
            .inline_widgets(self.inline_resolver.clone())
    }

    /// The pane's effective line height (see
    /// [`terminal_pane::effective_metrics`]).
    fn line_height(&self) -> f32 {
        terminal_pane::effective_metrics(&crate::prefs::current(), self.font_size).1
    }

    fn scroll_bar_element<Theme, Renderer: iced::advanced::Renderer>(
        &self,
        visible_lines: f32,
        state: Option<rc::Weak<RefCell<State>>>,
    ) -> Element<'a, Message, Theme, Renderer> {
        let max_line = self.buffer.last_line_number() as f64;
        let min_line = (self.buffer.last_line_number() - self.buffer.len()) as f64;
        let local_state = state.clone();

        let last_line = state
            .map(|state| {
                state
                    .upgrade()
                    .map(|state| {
                        let state = state.borrow();

                        if state.is_split() {
                            state.scroll_bar_value
                        } else {
                            max_line
                        }
                    })
                    .unwrap_or(max_line)
            })
            .unwrap_or(max_line);

        scroll_bar::scroll_bar(
            min_line as f32,
            max_line as f32,
            visible_lines,
            last_line as f32,
        )
        .on_change(move |value| {
            local_state.as_ref().map(|state| {
                state.upgrade().map(|state| {
                    let mut state = state.borrow_mut();

                    let value = if max_line < f64::from(visible_lines) {
                        max_line
                    } else {
                        f64::from(value)
                    };
                    state.scroll_bar_value = value;
                    state.is_split = value < max_line;
                })
            });
        })
        .into()
    }

    /// Vertical distance from the cursor to the nearest pane edge while the
    /// cursor is outside the pane: negative above the top edge, positive
    /// below the bottom edge, `None` while inside.
    fn autoscroll_overshoot(bounds: Rectangle, position: Point) -> Option<f32> {
        if position.y < bounds.y {
            Some(position.y - bounds.y)
        } else if position.y > bounds.y + bounds.height {
            Some(position.y - (bounds.y + bounds.height))
        } else {
            None
        }
    }

    /// Drag auto-scroll: while a selection drag is active and the cursor is
    /// past the top or bottom edge, scroll toward the cursor on every redraw
    /// tick — driven by a self-sustaining `request_redraw` loop rather than
    /// mouse events, so scrolling continues while the mouse is held still.
    fn drag_autoscroll<P>(
        &self,
        tree: &Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) where
        P: text::Paragraph + 'static,
    {
        if !matches!(*self.view.selection.borrow(), Selection::Selecting { .. }) {
            return;
        }

        let state = tree.state.downcast_ref::<Rc<RefCell<State>>>().clone();

        let position = cursor.position();
        let overshoot =
            position.and_then(|position| Self::autoscroll_overshoot(layout.bounds(), position));

        let Some(overshoot) = overshoot else {
            let mut state = state.borrow_mut();
            state.autoscroll_tick = None;
            state.autoscroll_debt = 0.0;
            return;
        };

        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                let was_split;
                let scrolled;
                {
                    let mut state = state.borrow_mut();

                    let dt = state
                        .autoscroll_tick
                        .map_or(0.0, |last| now.duration_since(last).as_secs_f32())
                        .min(AUTOSCROLL_MAX_TICK_SECS);
                    state.autoscroll_tick = Some(*now);

                    let line_height = self.line_height();
                    let speed = (AUTOSCROLL_BASE_LINES_PER_SEC
                        + (overshoot.abs() / line_height) * AUTOSCROLL_GAIN_PER_LINE)
                        .min(AUTOSCROLL_MAX_LINES_PER_SEC);

                    state.autoscroll_debt += overshoot.signum() * speed * dt;
                    let lines = state.autoscroll_debt.trunc();
                    state.autoscroll_debt -= lines;

                    let max_line = self.buffer.last_line_number() as f64;
                    let min_line = (self.buffer.last_line_number() - self.buffer.len()) as f64;

                    // Same lazy init as the wheel handler: while pinned to the
                    // bottom the stored value isn't kept up to date.
                    if !state.is_split {
                        state.scroll_bar_value = max_line;
                    }
                    was_split = state.is_split;

                    let before = state.scroll_bar_value;
                    state.scroll_bar_value =
                        (state.scroll_bar_value + f64::from(lines)).clamp(min_line, max_line);
                    state.is_split = state.scroll_bar_value < max_line;
                    scrolled = state.scroll_bar_value != before;
                }

                let extended = self.extend_selection_to_edge::<P>(
                    tree,
                    layout,
                    position.unwrap(),
                    overshoot,
                    was_split,
                );

                if scrolled || extended {
                    shell.invalidate_layout();
                }
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                // The cursor crossed an edge mid-drag; start the tick loop.
                shell.request_redraw();
            }
            _ => {}
        }
    }

    /// While auto-scrolling the cursor sits outside the pane, so the pane's
    /// own hit testing never fires; extend the selection to the line at the
    /// edge the cursor is past. Returns whether the selection changed.
    fn extend_selection_to_edge<P>(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        position: Point,
        overshoot: f32,
        was_split: bool,
    ) -> bool
    where
        P: text::Paragraph + 'static,
    {
        let mut layouts = layout.children();
        let scrollback_layout = layouts.next().unwrap();
        let main_layout = layouts.next().unwrap();

        // A full-pane historical view owns both edges. With a live tail,
        // preserve the split behavior: the historical view owns the top edge
        // and the live pane owns the bottom edge.
        let use_scrollback =
            was_split && (overshoot < 0.0 || !self.scrolled_layout.keeps_live_tail());
        let (pane_index, pane_layout) = if use_scrollback {
            (0, scrollback_layout)
        } else {
            (1, main_layout)
        };

        let bounds = pane_layout.bounds();
        let edge_y = if overshoot < 0.0 {
            0.0
        } else {
            bounds.height - 0.5
        };
        let point = Point::new((position.x - bounds.x).clamp(0.0, bounds.width), edge_y);

        let pane_state = tree.children[pane_index]
            .state
            .downcast_ref::<terminal_pane::State<P>>();

        let Some(hit) = pane_state.hit_test(bounds, point) else {
            return false;
        };

        let mut selection = self.view.selection.borrow_mut();
        if let Selection::Selecting { origin, from, to } = &*selection {
            let (new_from, new_to) = if hit.line < origin.line
                || (hit.line == origin.line && hit.column < origin.column)
            {
                (hit, origin.clone())
            } else {
                (origin.clone(), hit)
            };

            if new_from != *from || new_to != *to {
                let origin = origin.clone();
                *selection = Selection::Selecting {
                    origin,
                    from: new_from,
                    to: new_to,
                };
                return true;
            }
        }

        false
    }
}

/// Drag auto-scroll: while a selection drag is active and the cursor is
/// above or below the pane, the view scrolls toward the cursor at a speed
/// proportional to how far past the edge it is. Speeds are in lines per
/// second; the overshoot gain is per line-height of overshoot.
const AUTOSCROLL_BASE_LINES_PER_SEC: f32 = 2.0;
const AUTOSCROLL_GAIN_PER_LINE: f32 = 3.0;
const AUTOSCROLL_MAX_LINES_PER_SEC: f32 = 60.0;
/// Cap the time credited per tick so a stale timestamp from an earlier
/// drag can't scroll the view a long distance in one frame.
const AUTOSCROLL_MAX_TICK_SECS: f32 = 0.1;

#[derive(Default)]
struct State {
    visible_lines: f32,
    scroll_bar_value: f64,
    is_split: bool,
    /// Timestamp of the previous auto-scroll tick while a drag is past an edge.
    autoscroll_tick: Option<Instant>,
    /// Fractional lines accumulated but not yet scrolled.
    autoscroll_debt: f32,
    /// The `(cols, rows)` grid last reported through `on_grid_change`, so layout
    /// re-runs only fire the callback on an actual grid change.
    reported_grid: Option<(u16, u16)>,
}

/// The whole character cells that fit in `extent` pixels of `cell`-sized cells,
/// clamped to `1..=u16::MAX` (NAWS carries 16-bit dimensions, and a zero report
/// is a protocol hazard).
fn whole_cells(extent: f32, cell: f32) -> u16 {
    let cells = (extent / cell).floor();
    // `clamp` propagates NaN (and a NaN cast saturates to 0), so a degenerate
    // extent or cell must bail out before the cast, not rely on the clamp.
    if !cells.is_finite() {
        return 1;
    }
    // The clamp bounds the value to the u16 range before the cast.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        cells.clamp(1.0, 65_535.0) as u16
    }
}

impl State {
    fn is_split(&self) -> bool {
        self.is_split
    }
}

/// `visible_lines` is the full terminal region's height in lines;
/// `split_visible_lines` is the height of the historical pane once the view is
/// split (the same value for `ScrolledLayout::FullPane`, smaller by the live
/// tail for `SplitWithLiveTail`). Paging steps by the window that is actually
/// on screen: the full region for the first page away from the pinned bottom,
/// the split pane afterwards — a full-height step from a split view would skip
/// the lines the live tail displaced.
#[allow(clippy::cast_precision_loss)]
fn apply_scroll_request(
    state: &mut State,
    request: ScrollRequest,
    min_line: f64,
    max_line: f64,
    visible_lines: f32,
    split_visible_lines: f32,
) {
    if max_line <= min_line {
        state.scroll_bar_value = max_line;
        state.is_split = false;
        return;
    }

    let page = f64::from(visible_lines.floor().max(1.0));
    let split_page = f64::from(split_visible_lines.floor().max(1.0)).min(page);
    let current = if state.is_split {
        state.scroll_bar_value.clamp(min_line, max_line)
    } else {
        max_line
    };
    // Every request other than `End` leaves the view split, so the oldest
    // reachable window is one *split* page above the buffer floor; anchoring
    // its bottom there puts the oldest retained line at the pane's top row.
    let oldest_full_page = (min_line + split_page).min(max_line);
    let value = match request {
        ScrollRequest::PageUp => {
            let step = if state.is_split { split_page } else { page };
            (current - step).max(oldest_full_page)
        }
        ScrollRequest::PageDown => (current + split_page).min(max_line),
        ScrollRequest::Pages(pages) if pages < 0 => {
            let count = f64::from(pages.unsigned_abs());
            let first_step = if state.is_split { split_page } else { page };
            (current - first_step - split_page * (count - 1.0)).max(oldest_full_page)
        }
        ScrollRequest::Pages(pages) if pages > 0 => {
            (current + split_page * f64::from(pages)).min(max_line)
        }
        ScrollRequest::Pages(_) => current,
        ScrollRequest::Lines(lines) => (current + f64::from(lines)).clamp(min_line + 1.0, max_line),
        ScrollRequest::Home => oldest_full_page,
        ScrollRequest::End => max_line,
        ScrollRequest::RevealLine(line) => {
            let line = (line as f64).clamp(min_line + 1.0, max_line);
            let visible_top = current - page + 1.0;
            if !state.is_split && line >= visible_top {
                current
            } else {
                line
            }
        }
    };

    state.scroll_bar_value = value;
    state.is_split = value < max_line;
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for SplitTerminalPane<'a, Message>
where
    Renderer:
        iced::advanced::Renderer + iced::advanced::text::Renderer<Font = iced::Font> + 'static,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'static,
    Message: 'static,
{
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        for (child, layout) in tree.children.iter_mut().take(2).zip(layout.children()) {
            if let Some(host) =
                crate::inline_object::Host::<Message, Theme, Renderer>::get_mut(child)
            {
                host.operate(layout, renderer, operation);
            }
        }
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, Theme, Renderer>> {
        let overlays: Vec<_> = tree
            .children
            .iter_mut()
            .take(2)
            .zip(layout.children())
            .filter_map(|(child, layout)| {
                crate::inline_object::Host::<Message, Theme, Renderer>::get_mut(child)?.overlay(
                    layout,
                    renderer,
                    viewport,
                    translation,
                )
            })
            .collect();
        (!overlays.is_empty())
            .then(|| iced::advanced::overlay::Group::with_children(overlays).overlay())
    }
    fn children(&self) -> Vec<tree::Tree> {
        vec![
            Tree::new(Element::<Message, Theme, Renderer>::new(
                self.terminal_pane(),
            )),
            Tree::new(Element::<Message, Theme, Renderer>::new(
                self.terminal_pane(),
            )),
            Tree::new::<Message, Theme, Renderer>(&self.scroll_bar_element(0.0, None)),
        ]
    }

    fn diff(&self, _tree: &mut Tree) {}

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Rc<RefCell<State>>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Rc::new(RefCell::new(State::default())))
    }

    fn size(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Fill, iced::Length::Fill)
    }

    fn size_hint(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Fill, iced::Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_ref::<Rc<RefCell<State>>>();

        let mut children = tree.children.iter_mut();
        let scrollback_pane_tree = children.next().unwrap();
        let main_pane_tree = children.next().unwrap();
        let scrollbar_tree = children.next().unwrap();

        let terminal_pane_limits = limits.shrink(Size::new(scroll_bar::SCROLLBAR_WIDTH, 0.0));
        let scrollbar_limits = limits.shrink(Size::new(terminal_pane_limits.max().width, 0.0));
        let line_height = self.line_height();
        let full_height = terminal_pane_limits.max().height;
        let visible_lines = full_height / line_height;
        let live_tail_height = self.scrolled_layout.live_tail_max_height().min(full_height);
        let split_visible_lines = (full_height - live_tail_height) / line_height;

        {
            let max_line = self.buffer.last_line_number() as f64;
            let min_line = (self.buffer.last_line_number() - self.buffer.len()) as f64;
            let mut state = state.borrow_mut();
            for request in self.view.scroll.take_requests() {
                let pixel_delta = self
                    .buffer
                    .has_inline_objects()
                    .then(|| {
                        object_scroll_delta(
                            request,
                            state.is_split,
                            full_height,
                            live_tail_height,
                            line_height,
                        )
                    })
                    .flatten();
                if let Some(delta) = pixel_delta {
                    if !state.is_split {
                        state.scroll_bar_value = max_line;
                    }
                    state.scroll_bar_value =
                        scroll_pixels(state.scroll_bar_value, delta, min_line, max_line, |line| {
                            [
                                scrollback_pane_tree
                                    .state
                                    .downcast_ref::<terminal_pane::State<Renderer::Paragraph>>(),
                                main_pane_tree
                                    .state
                                    .downcast_ref::<terminal_pane::State<Renderer::Paragraph>>(),
                            ]
                            .iter()
                            .find_map(|s| s.row_height_at(line))
                            .unwrap_or(line_height)
                        });
                    state.is_split = state.scroll_bar_value < max_line;
                    continue;
                }
                apply_scroll_request(
                    &mut state,
                    request,
                    min_line,
                    max_line,
                    visible_lines,
                    split_visible_lines,
                );
            }
            state.visible_lines = visible_lines;
        }

        let (main_pane_node, scrollback_pane_node) = if state.borrow().is_split() {
            let main_pane_limits = terminal_pane_limits
                .loose()
                .max_height(self.scrolled_layout.live_tail_max_height());

            let mut main_pane_node =
                <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::layout(
                    &mut self.terminal_pane(),
                    main_pane_tree,
                    renderer,
                    &main_pane_limits,
                );

            let scrollback_pane_limits =
                terminal_pane_limits.shrink(Size::new(0.0, main_pane_node.bounds().height));

            let scrollback_pane_node =
                <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::layout(
                    &mut self
                        .terminal_pane()
                        .last_line_position(state.borrow().scroll_bar_value),
                    scrollback_pane_tree,
                    renderer,
                    &scrollback_pane_limits,
                );

            main_pane_node =
                main_pane_node.move_to(Point::new(0.0, scrollback_pane_node.size().height));

            (main_pane_node, scrollback_pane_node)
        } else {
            scrollback_pane_tree
                .state
                .downcast_ref::<terminal_pane::State<Renderer::Paragraph>>()
                .retire_effect_inputs();
            let main_pane_node =
                <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::layout(
                    &mut self.terminal_pane(),
                    main_pane_tree,
                    renderer,
                    &terminal_pane_limits,
                );

            (main_pane_node, Node::new(Size::new(0.0, 0.0)))
        };

        let prefs = crate::prefs::current();
        let scrollbar_node = self
            .scroll_bar_element::<Theme, Renderer>(visible_lines, Some(Rc::downgrade(state)))
            .as_widget_mut()
            .layout(scrollbar_tree, renderer, &scrollbar_limits);

        let main_pane_width = main_pane_node.size().width;

        let mut state = state.borrow_mut();
        // Report the character grid of the FULL terminal region — not the
        // split-shrunk main pane; the scrollback split is a UI affordance, not
        // a terminal resize — whenever it actually changes. Cell-boundary
        // quantization means a pixel-level resize drag only fires on real grid
        // steps. The cell advance was measured by the child layout above.
        if let Some(on_grid_change) = &self.on_grid_change
            && let Some((_, _, advance)) = main_pane_tree
                .state
                .downcast_ref::<terminal_pane::State<Renderer::Paragraph>>()
                .advance
        {
            let full = terminal_pane_limits.max();
            let cols = whole_cells(full.width, advance).min(prefs.line_length.unwrap_or(u16::MAX));
            let rows = whole_cells(full.height, line_height);
            if state.reported_grid != Some((cols, rows)) {
                state.reported_grid = Some((cols, rows));
                on_grid_change(cols, rows);
            }
        }

        Node::with_children(
            limits.max(),
            vec![
                scrollback_pane_node,
                main_pane_node,
                scrollbar_node.move_to(Point::new(main_pane_width, 0.0)),
            ],
        )
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_ref::<Rc<RefCell<State>>>();

        let mut children = tree.children.iter();
        let scrollback_pane_tree = children.next().unwrap();
        let main_pane_tree = children.next().unwrap();
        let scroll_bar_tree = children.next().unwrap();

        let mut children = layout.children();
        let scrollback_pane_layout = children.next().unwrap();
        let main_pane_layout = children.next().unwrap();
        let scrollbar_layout = children.next().unwrap();

        if state.borrow().is_split() {
            <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::draw(
                &self.terminal_pane(),
                scrollback_pane_tree,
                renderer,
                theme,
                style,
                scrollback_pane_layout,
                cursor,
                viewport,
            );
        }

        <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::draw(
            &self.terminal_pane(),
            main_pane_tree,
            renderer,
            theme,
            style,
            main_pane_layout,
            cursor,
            viewport,
        );

        self.scroll_bar_element::<Theme, Renderer>(
            state.borrow().visible_lines,
            Some(Rc::downgrade(state)),
        )
        .as_widget()
        .draw(
            scroll_bar_tree,
            renderer,
            theme,
            style,
            scrollbar_layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<Rc<RefCell<State>>>();

        let scroll_bar = self.scroll_bar_element(0.0, Some(Rc::downgrade(state)));

        [
            &Element::<Message, Theme, Renderer>::new(self.terminal_pane()),
            &Element::<Message, Theme, Renderer>::new(self.terminal_pane()),
            &scroll_bar,
        ]
        .iter_mut()
        .zip(&tree.children)
        .zip(layout.children())
        .map(|((child, state), layout)| {
            child
                .as_widget()
                .mouse_interaction(state, layout, cursor, viewport, renderer)
        })
        .fold(mouse::Interaction::Idle, |left_i, right_i| {
            if left_i == mouse::Interaction::Idle {
                right_i
            } else {
                left_i
            }
        })
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<Rc<RefCell<State>>>();

        // A native scrollable inside a terminal line gets first refusal on wheel
        // input. If it cannot scroll, the terminal handles the event normally.
        // This is the hosts' only delivery: the fan-out below skips the terminal
        // halves for wheel events, whose own update would hand it to them again.
        let wheel = matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }));
        if wheel {
            for (child, child_layout) in tree.children.iter_mut().take(2).zip(layout.children()) {
                if let Some(host) =
                    crate::inline_object::Host::<Message, Theme, Renderer>::get_mut(child)
                {
                    host.update(
                        event,
                        child_layout,
                        cursor,
                        renderer,
                        clipboard,
                        shell,
                        viewport,
                    );
                    if shell.is_event_captured() {
                        return;
                    }
                }
            }
        }

        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event
            && cursor.position_in(layout.bounds()).is_some()
        {
            let mut state = state.borrow_mut();
            let max_line = self.buffer.last_line_number() as f64;
            let min_line = (self.buffer.last_line_number() - self.buffer.len()) as f64;

            // We don't update the scroll bar position when new lines come in, so if we're not split (it's fixed to the bottom),
            // update it lazily now before we do any arithmetic dependant on its value
            if !state.is_split {
                state.scroll_bar_value = max_line;
            }

            if self.buffer.has_inline_objects() {
                let pixels = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y * self.line_height(),
                    mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                state.scroll_bar_value =
                    scroll_pixels(state.scroll_bar_value, pixels, min_line, max_line, |line| {
                        tree.children
                            .iter()
                            .take(2)
                            .find_map(|child| {
                                child
                                    .state
                                    .downcast_ref::<terminal_pane::State<Renderer::Paragraph>>()
                                    .row_height_at(line)
                            })
                            .unwrap_or(self.line_height())
                    });
                state.is_split = state.scroll_bar_value < max_line;
                shell.invalidate_layout();
                shell.request_redraw();
                shell.capture_event();
                return;
            }

            match delta {
                mouse::ScrollDelta::Lines { y, .. } => {
                    state.scroll_bar_value -= f64::from(*y);
                    state.scroll_bar_value = state.scroll_bar_value.clamp(min_line, max_line);
                    state.is_split = state.scroll_bar_value < max_line;
                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                }
                mouse::ScrollDelta::Pixels { y, .. } => {
                    // Positive y scrolls up (toward older lines); cap the
                    // per-event step at one line in either direction.
                    state.scroll_bar_value -= f64::from((*y / 10.0).clamp(-1.0, 1.0));
                    state.scroll_bar_value = state.scroll_bar_value.clamp(min_line, max_line);
                    state.is_split = state.scroll_bar_value < max_line;
                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                }
            }
            return;
        }

        self.drag_autoscroll::<Renderer::Paragraph>(tree, event, layout, cursor, shell);

        let mut scroll_bar =
            self.scroll_bar_element(state.borrow().visible_lines, Some(Rc::downgrade(state)));

        [
            &mut Element::<Message, Theme, Renderer>::new(self.terminal_pane()),
            &mut Element::<Message, Theme, Renderer>::new(self.terminal_pane()),
            &mut scroll_bar,
        ]
        .iter_mut()
        .zip(&mut tree.children)
        .zip(layout.children())
        .skip(if wheel { 2 } else { 0 })
        .map(|((child, state), layout)| {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            )
        })
        .for_each(drop);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn split_terminal_pane<'a, Message, Theme, Renderer>(
    buffer: Ref<'a, TerminalBuffer>,
    view: TerminalViewHandle,
    on_link: Option<Rc<dyn Fn(LinkClickEvent) -> Message>>,
    on_link_tooltip: Option<Rc<dyn Fn(LinkTooltipCallback)>>,
    on_grid_change: Option<Rc<dyn Fn(u16, u16)>>,
    font_size: Option<f32>,
    scrolled_layout: ScrolledLayout,
    inline_resolver: Option<crate::inline_object::Resolver>,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer<Font = iced::Font> + 'static,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'static,
    Message: 'static,
{
    let mut pane = SplitTerminalPane::new(buffer, view, scrolled_layout);
    pane.inline_resolver = inline_resolver;
    pane.on_link = on_link;
    pane.on_link_tooltip = on_link_tooltip;
    pane.on_grid_change = on_grid_change;
    pane.font_size = font_size;
    Element::new(pane)
}

fn object_scroll_delta(
    request: ScrollRequest,
    split: bool,
    full: f32,
    tail: f32,
    line_height: f32,
) -> Option<f32> {
    let historical = (full - tail).max(line_height);
    let first = if split { historical } else { full };
    match request {
        ScrollRequest::PageUp => Some(first),
        ScrollRequest::PageDown => Some(-historical),
        ScrollRequest::Pages(pages) if pages < 0 => {
            Some(first + historical * (pages.unsigned_abs() as f32 - 1.0))
        }
        ScrollRequest::Pages(pages) => Some(-historical * pages as f32),
        ScrollRequest::Lines(lines) => Some(-(lines as f32) * line_height),
        _ => None,
    }
}

/// A logical-line scrollbar can address the interior of a tall row through its
/// fractional part. Input is measured in pixels, using cached visible row heights.
fn scroll_pixels(
    mut value: f64,
    pixels: f32,
    min: f64,
    max: f64,
    height: impl Fn(usize) -> f32,
) -> f64 {
    let mut pixels = f64::from(pixels);
    for _ in 0..4096 {
        if pixels.abs() < 0.01 {
            break;
        }
        if pixels > 0.0 {
            if value <= min {
                break;
            }
            let line = value.ceil();
            let h = f64::from(height(line as usize).max(1.0));
            let remaining = (value - (line - 1.0)) * h;
            let step = pixels.min(remaining);
            value -= step / h;
            pixels -= step;
        } else {
            if value >= max {
                break;
            }
            let line = value.floor() + 1.0;
            let h = f64::from(height(line as usize).max(1.0));
            let remaining = (line - value) * h;
            let step = (-pixels).min(remaining);
            value += step / h;
            pixels += step;
        }
    }
    value.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::{
        SPLIT_LIVE_TAIL_MAX_HEIGHT, ScrollRequest, ScrolledLayout, State, apply_scroll_request,
    };

    /// A widget that counts the wheel events it is handed.
    struct WheelCounter(std::rc::Rc<std::cell::Cell<u32>>);
    impl iced::advanced::Widget<(), smudgy_theme::Theme, iced_tiny_skia::Renderer> for WheelCounter {
        fn size(&self) -> iced::Size<iced::Length> {
            iced::Size::new(iced::Length::Fixed(40.0), iced::Length::Fixed(20.0))
        }
        fn layout(
            &mut self,
            _: &mut iced::advanced::widget::Tree,
            _: &iced_tiny_skia::Renderer,
            _: &iced::advanced::layout::Limits,
        ) -> iced::advanced::layout::Node {
            iced::advanced::layout::Node::new(iced::Size::new(40.0, 20.0))
        }
        fn draw(
            &self,
            _: &iced::advanced::widget::Tree,
            _: &mut iced_tiny_skia::Renderer,
            _: &smudgy_theme::Theme,
            _: &iced::advanced::renderer::Style,
            _: iced::advanced::Layout<'_>,
            _: iced::advanced::mouse::Cursor,
            _: &iced::Rectangle,
        ) {
        }
        fn update(
            &mut self,
            _: &mut iced::advanced::widget::Tree,
            event: &iced::Event,
            _: iced::advanced::Layout<'_>,
            _: iced::advanced::mouse::Cursor,
            _: &iced_tiny_skia::Renderer,
            _: &mut dyn iced::advanced::Clipboard,
            _: &mut iced::advanced::Shell<'_, ()>,
            _: &iced::Rectangle,
        ) {
            if matches!(
                event,
                iced::Event::Mouse(iced::advanced::mouse::Event::WheelScrolled { .. })
            ) {
                self.0.set(self.0.get() + 1);
            }
        }
    }

    #[test]
    fn wheel_input_reaches_an_inline_widget_once_wherever_the_cursor_is() {
        use crate::terminal_buffer::TerminalBuffer;
        use iced::advanced::{Layout, Shell, Widget, layout, mouse, widget::Tree};
        use iced::{Point, Rectangle, Size};
        use smudgy_session_model::{
            Style, StyledLine,
            inline_content::{InlineObject, InlineOwner},
        };
        use std::{cell::RefCell, rc::Rc, sync::Arc};
        type Painter = iced_tiny_skia::Renderer;

        iced_tiny_skia::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(crate::assets::GEIST_MONO_BYTES.into());
        let renderer = Painter::new(crate::assets::GEIST_MONO, iced::Pixels(16.0));
        let buffer = RefCell::new(TerminalBuffer::new());
        let mut line =
            StyledLine::from_styled_runs(&[("widget", Style::DEFAULT, None)], Style::DEFAULT);
        line.objects = Some(Arc::new(vec![InlineObject::new(
            0..6,
            Arc::new(()),
            InlineOwner::default(),
        )]));
        buffer.borrow_mut().push_line(Arc::new(line));
        let count = Rc::new(std::cell::Cell::new(0));
        let seen = count.clone();
        let mut pane = super::SplitTerminalPane::<()>::new(
            buffer.borrow(),
            super::TerminalViewHandle::default(),
            ScrolledLayout::SplitWithLiveTail,
        );
        pane.inline_resolver = Some(crate::inline_object::resolver::<
            (),
            smudgy_theme::Theme,
            Painter,
        >(move |_| {
            Some(iced::Element::new(WheelCounter(seen.clone())))
        }));
        type Pane<'a> = super::SplitTerminalPane<'a, ()>;
        let mut tree = Tree::new(&pane as &dyn Widget<(), smudgy_theme::Theme, Painter>);
        let size = Size::new(400.0, 200.0);
        let node = <Pane<'_> as Widget<(), smudgy_theme::Theme, Painter>>::layout(
            &mut pane,
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let viewport = Rectangle::new(Point::ORIGIN, size);
        let event = iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        let mut clipboard = iced::advanced::clipboard::Null;
        for cursor in [
            mouse::Cursor::Available(Point::new(10.0, 190.0)),
            mouse::Cursor::Available(Point::new(-50.0, -50.0)),
            mouse::Cursor::Unavailable,
        ] {
            count.set(0);
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            <Pane<'_> as Widget<(), smudgy_theme::Theme, Painter>>::update(
                &mut pane,
                &mut tree,
                &event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard,
                &mut shell,
                &viewport,
            );
            assert_eq!(count.get(), 1, "cursor {cursor:?}");
        }
    }

    #[test]
    fn paging_a_tall_object_does_not_skip_the_live_tail_height() {
        assert_eq!(
            super::object_scroll_delta(ScrollRequest::PageUp, false, 560.0, 200.0, 20.0),
            Some(560.0)
        );
        assert_eq!(
            super::object_scroll_delta(ScrollRequest::PageUp, true, 560.0, 200.0, 20.0),
            Some(360.0)
        );
        assert_eq!(
            super::object_scroll_delta(ScrollRequest::PageDown, true, 560.0, 200.0, 20.0),
            Some(-360.0)
        );
        assert_eq!(
            super::object_scroll_delta(ScrollRequest::Pages(-2), false, 560.0, 200.0, 20.0),
            Some(920.0)
        );
    }

    #[test]
    fn pixel_scrolling_reaches_the_interior_of_a_tall_row() {
        let height = |line| if line == 10 { 1000.0 } else { 20.0 };
        let late = super::scroll_pixels(1000000.0, 16.0, 0.0, 1000000.0, |_| 4096.0);
        assert!((late - (1000000.0 - 16.0 / 4096.0)).abs() < 1e-8);
        let value = super::scroll_pixels(10.0, 100.0, 0.0, 10.0, height);
        assert!((value - 9.9).abs() < 0.001);
        assert!((super::scroll_pixels(value, -100.0, 0.0, 10.0, height) - 10.0).abs() < 0.001);
        assert!((super::scroll_pixels(10.0, 1020.0, 0.0, 10.0, height) - 8.0).abs() < 0.001);
    }

    #[test]
    fn full_pane_scrollback_reserves_no_live_tail() {
        assert_eq!(ScrolledLayout::FullPane.live_tail_max_height(), 0.0);
        assert!(!ScrolledLayout::FullPane.keeps_live_tail());
    }

    #[test]
    fn split_scrollback_preserves_the_existing_live_tail() {
        assert_eq!(
            ScrolledLayout::SplitWithLiveTail.live_tail_max_height(),
            SPLIT_LIVE_TAIL_MAX_HEIGHT
        );
        assert!(ScrolledLayout::SplitWithLiveTail.keeps_live_tail());
    }

    #[test]
    fn keyboard_scroll_requests_use_full_pages_and_pin_end() {
        let mut state = State::default();

        apply_scroll_request(&mut state, ScrollRequest::PageUp, 0.0, 100.0, 20.0, 20.0);
        assert_eq!(state.scroll_bar_value, 80.0);
        assert!(state.is_split);

        apply_scroll_request(&mut state, ScrollRequest::PageUp, 0.0, 100.0, 20.0, 20.0);
        assert_eq!(state.scroll_bar_value, 60.0);

        apply_scroll_request(&mut state, ScrollRequest::Home, 0.0, 100.0, 20.0, 20.0);
        assert_eq!(state.scroll_bar_value, 20.0);

        apply_scroll_request(&mut state, ScrollRequest::End, 0.0, 100.0, 20.0, 20.0);
        assert_eq!(state.scroll_bar_value, 100.0);
        assert!(!state.is_split);
    }

    #[test]
    fn split_paging_steps_by_the_historical_pane_and_reaches_the_oldest_line() {
        let mut state = State::default();

        // Full region 20 lines, historical pane 12 once the live tail splits
        // off. The first page away from the pinned bottom steps by the full
        // region (the whole 20 lines were on screen), leaving the pane bottom
        // adjacent to the previously visible top.
        apply_scroll_request(&mut state, ScrollRequest::PageUp, 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 80.0);
        assert!(state.is_split);

        // Subsequent pages step by the historical pane's height: the pane
        // showed lines 69..=80, so the next window is 57..=68 — contiguous.
        apply_scroll_request(&mut state, ScrollRequest::PageUp, 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 68.0);

        apply_scroll_request(&mut state, ScrollRequest::PageDown, 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 80.0);

        // Home anchors one split page above the floor, so the oldest retained
        // line (1) lands on the pane's top row instead of staying unreachable.
        apply_scroll_request(&mut state, ScrollRequest::Home, 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 12.0);

        // PageUp from Home has nowhere older to go.
        apply_scroll_request(&mut state, ScrollRequest::PageUp, 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 12.0);

        apply_scroll_request(&mut state, ScrollRequest::End, 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 100.0);
        assert!(!state.is_split);
    }

    #[test]
    fn reveal_only_leaves_the_live_tail_when_the_result_is_offscreen() {
        let mut state = State::default();

        apply_scroll_request(
            &mut state,
            ScrollRequest::RevealLine(95),
            0.0,
            100.0,
            20.0,
            12.0,
        );
        assert_eq!(state.scroll_bar_value, 100.0);
        assert!(!state.is_split);

        apply_scroll_request(
            &mut state,
            ScrollRequest::RevealLine(70),
            0.0,
            100.0,
            20.0,
            12.0,
        );
        assert_eq!(state.scroll_bar_value, 70.0);
        assert!(state.is_split);
    }

    #[test]
    fn page_delta_matches_repeated_page_requests() {
        let mut state = State::default();

        apply_scroll_request(&mut state, ScrollRequest::Pages(-3), 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 56.0);
        assert!(state.is_split);

        apply_scroll_request(&mut state, ScrollRequest::Pages(2), 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 80.0);
        assert!(state.is_split);
    }

    #[test]
    fn line_delta_clamps_to_retained_lines() {
        let mut state = State::default();

        apply_scroll_request(&mut state, ScrollRequest::Lines(-5), 0.0, 100.0, 20.0, 12.0);
        assert_eq!(state.scroll_bar_value, 95.0);
        assert!(state.is_split);

        apply_scroll_request(
            &mut state,
            ScrollRequest::Lines(-200),
            0.0,
            100.0,
            20.0,
            12.0,
        );
        assert_eq!(state.scroll_bar_value, 1.0);

        apply_scroll_request(
            &mut state,
            ScrollRequest::Lines(200),
            0.0,
            100.0,
            20.0,
            12.0,
        );
        assert_eq!(state.scroll_bar_value, 100.0);
        assert!(!state.is_split);
    }
}
