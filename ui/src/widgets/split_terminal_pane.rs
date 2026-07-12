use std::{
    cell::{Ref, RefCell},
    rc::{self, Rc},
    time::Instant,
};

use iced::{
    Element, Event, Point, Rectangle, Size,
    advanced::{
        Clipboard, Layout, Shell, Widget,
        layout::{self, Node},
        mouse, text,
        widget::{Tree, tree},
    }, window,
};
use crate::terminal_buffer::{LinkClickEvent, TerminalBuffer, selection::Selection};

mod scroll_bar;
mod terminal_pane;

use terminal_pane::{TerminalPane, terminal_pane};

struct SplitTerminalPane<'a> {
    pub selection: Rc<RefCell<Selection>>,
    pub buffer: Ref<'a, TerminalBuffer>,
    pub on_link: Option<Rc<dyn Fn(LinkClickEvent)>>,
}

impl<'a> SplitTerminalPane<'a> {
    pub fn new(buffer: Ref<'a, TerminalBuffer>, selection: Rc<RefCell<Selection>>) -> Self {
        Self {
            selection,
            buffer,
            on_link: None,
        }
    }

    fn terminal_pane(&self) -> TerminalPane<'a> {
        terminal_pane(Ref::clone(&self.buffer), self.selection.clone())
            .on_link(self.on_link.clone())
    }

    fn scroll_bar_element<Message, Theme, Renderer: iced::advanced::Renderer>(
        &self,
        visible_lines: f32,
        state: Option<rc::Weak<RefCell<State>>>,
    ) -> Element<'a, Message, Theme, Renderer> {
        let max_line = self.buffer.last_line_number() as f32;
        let min_line = (self.buffer.last_line_number() - self.buffer.len()) as f32;
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

        scroll_bar::scroll_bar(min_line, max_line, visible_lines, last_line)
            .on_change(move |value| {
                local_state.as_ref().map(|state| {
                    state.upgrade().map(|state| {
                        let mut state = state.borrow_mut();

                        let value = if max_line < visible_lines {
                            max_line
                        } else {
                            value
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
    fn drag_autoscroll<P, Message>(
        &self,
        tree: &Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) where
        P: text::Paragraph + 'static,
    {
        if !matches!(*self.selection.borrow(), Selection::Selecting { .. }) {
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

                    let line_height = crate::prefs::current().line_height;
                    let speed = (AUTOSCROLL_BASE_LINES_PER_SEC
                        + (overshoot.abs() / line_height) * AUTOSCROLL_GAIN_PER_LINE)
                        .min(AUTOSCROLL_MAX_LINES_PER_SEC);

                    state.autoscroll_debt += overshoot.signum() * speed * dt;
                    let lines = state.autoscroll_debt.trunc();
                    state.autoscroll_debt -= lines;

                    let max_line = self.buffer.last_line_number() as f32;
                    let min_line = (self.buffer.last_line_number() - self.buffer.len()) as f32;

                    // Same lazy init as the wheel handler: while pinned to the
                    // bottom the stored value isn't kept up to date.
                    if !state.is_split {
                        state.scroll_bar_value = max_line;
                    }
                    was_split = state.is_split;

                    let before = state.scroll_bar_value;
                    state.scroll_bar_value =
                        (state.scroll_bar_value + lines).clamp(min_line, max_line);
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

        // Above the pane, extend within whichever pane is at the top of the
        // widget; below, always within the bottom (live) pane.
        let (pane_index, pane_layout) = if overshoot < 0.0 && was_split {
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

        let mut selection = self.selection.borrow_mut();
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
    scroll_bar_value: f32,
    is_split: bool,
    /// Timestamp of the previous auto-scroll tick while a drag is past an edge.
    autoscroll_tick: Option<Instant>,
    /// Fractional lines accumulated but not yet scrolled.
    autoscroll_debt: f32,
}

impl State {
    fn is_split(&self) -> bool {
        self.is_split
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer> for SplitTerminalPane<'a>
where
    Renderer: iced::advanced::Renderer + iced::advanced::text::Renderer<Font = iced::Font> + 'a,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'a,
{
    fn children(&self) -> Vec<tree::Tree> {
        vec![
            Tree::new(Element::<(), Theme, Renderer>::new(self.terminal_pane())),
            Tree::new(Element::<(), Theme, Renderer>::new(self.terminal_pane())),
            Tree::new::<(), Theme, Renderer>(&self.scroll_bar_element(0.0, None)),
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

        let (main_pane_node, scrollback_pane_node) = if state.borrow().is_split() {
            let main_pane_limits = terminal_pane_limits.loose().max_height(200.0);

            let mut main_pane_node = <TerminalPane<'_> as Widget<Message, Theme, Renderer>>::layout(
                &mut self.terminal_pane(),
                main_pane_tree,
                renderer,
                &main_pane_limits,
            );

            let scrollback_pane_limits =
                terminal_pane_limits.shrink(Size::new(0.0, main_pane_node.bounds().height));

            let scrollback_pane_node =
                <TerminalPane<'_> as Widget<Message, Theme, Renderer>>::layout(
                    &mut self
                        .terminal_pane()
                        .last_line_number(state.borrow().scroll_bar_value as usize),
                    scrollback_pane_tree,
                    renderer,
                    &scrollback_pane_limits,
                );

            main_pane_node =
                main_pane_node.move_to(Point::new(0.0, scrollback_pane_node.size().height));

            (main_pane_node, scrollback_pane_node)
        } else {
            let main_pane_node = <TerminalPane<'_> as Widget<Message, Theme, Renderer>>::layout(
                &mut self.terminal_pane(),
                main_pane_tree,
                renderer,
                &terminal_pane_limits,
            );

            (main_pane_node, Node::new(Size::new(0.0, 0.0)))
        };

        // Use the same line height the panes lay text out with, so the
        // scrollbar's visible-lines math matches what is on screen.
        let visible_lines = terminal_pane_limits.max().height / crate::prefs::current().line_height;

        let scrollbar_node = self
            .scroll_bar_element::<Message, Theme, Renderer>(
                visible_lines,
                Some(Rc::downgrade(state)),
            )
            .as_widget_mut()
            .layout(scrollbar_tree, renderer, &scrollbar_limits);

        let main_pane_width = main_pane_node.size().width;

        let mut state = state.borrow_mut();
        state.visible_lines = visible_lines;

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
            <TerminalPane<'_> as Widget<Message, Theme, Renderer>>::draw(
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

        <TerminalPane<'_> as Widget<Message, Theme, Renderer>>::draw(
            &self.terminal_pane(),
            main_pane_tree,
            renderer,
            theme,
            style,
            main_pane_layout,
            cursor,
            viewport,
        );

        self.scroll_bar_element::<Message, Theme, Renderer>(
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

        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event
            && cursor.position_in(layout.bounds()).is_some() {
                let mut state = state.borrow_mut();
                let max_line = self.buffer.last_line_number() as f32;
                let min_line = (self.buffer.last_line_number() - self.buffer.len()) as f32;

                // We don't update the scroll bar position when new lines come in, so if we're not split (it's fixed to the bottom),
                // update it lazily now before we do any arithmetic dependant on its value
                if !state.is_split {
                    state.scroll_bar_value = max_line;
                }

                match delta {
                    mouse::ScrollDelta::Lines { y, .. } => {
                        state.scroll_bar_value -= y;
                        state.scroll_bar_value = state.scroll_bar_value.clamp(min_line, max_line);
                        state.is_split = state.scroll_bar_value < max_line;
                        shell.invalidate_layout();
                        shell.request_redraw();
                        shell.capture_event();
                    }
                    mouse::ScrollDelta::Pixels { y, .. } => {
                        // Positive y scrolls up (toward older lines); cap the
                        // per-event step at one line in either direction.
                        state.scroll_bar_value -= (*y / 10.0).clamp(-1.0, 1.0);
                        state.scroll_bar_value = state.scroll_bar_value.clamp(min_line, max_line);
                        state.is_split = state.scroll_bar_value < max_line;
                        shell.invalidate_layout();
                        shell.request_redraw();
                        shell.capture_event();
                    }
                }
                return;
            }

        self.drag_autoscroll::<Renderer::Paragraph, Message>(tree, event, layout, cursor, shell);

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
        .map(|((child, state), layout)| {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            )
        })
        .for_each(drop);
    }
}

pub fn split_terminal_pane<'a, Message, Theme, Renderer>(
    buffer: Ref<'a, TerminalBuffer>,
    selection: Rc<RefCell<Selection>>,
    on_link: Option<Rc<dyn Fn(LinkClickEvent)>>,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer<Font = iced::Font> + 'a,
    Renderer::Paragraph:
        iced::advanced::text::Paragraph<Font = iced::Font> + Clone + std::fmt::Debug + 'static,
    Theme: iced::widget::text::Catalog + 'a,
    Message: 'a,
{
    let mut pane = SplitTerminalPane::new(buffer, selection);
    pane.on_link = on_link;
    Element::new(pane)
}
