//! A terminal-text widget with local selection, sharing the transcript renderer.
use crate::{
    split_terminal_pane::terminal_pane::TerminalPane,
    terminal_buffer::{LinkClickEvent, TerminalBuffer, selection::Selection},
};
use iced::{
    Element, Event, Length, Rectangle, Size,
    advanced::{
        self, Layout, Widget, layout, mouse, renderer,
        widget::{Tree, tree},
    },
};
use smudgy_session_model::{
    Style, inline_content::InlineContent, styled_line::LinkTooltipCallback,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

type Theme = smudgy_theme::Theme;
type Renderer = iced::Renderer;

pub struct TerminalSpan<Message> {
    content: Arc<InlineContent>,
    inline_resolver: Option<crate::inline_object::Resolver>,
    started: Option<iced::time::Instant>,
    on_link: Rc<dyn Fn(LinkClickEvent) -> Message>,
    on_tooltip: Rc<dyn Fn(LinkTooltipCallback) -> Message>,
}
struct State {
    content: Arc<InlineContent>,
    /// The mount clock the line was stamped with; a new clock is new state.
    started: Option<iced::time::Instant>,
    buffer: RefCell<TerminalBuffer>,
    selection: Rc<RefCell<Selection>>,
    search_selection: Rc<Cell<bool>>,
    tooltips: Rc<RefCell<Vec<LinkTooltipCallback>>>,
    collect_tooltip: Rc<dyn Fn(LinkTooltipCallback)>,
}
impl State {
    fn new(content: Arc<InlineContent>, started: Option<iced::time::Instant>) -> Self {
        let mut buffer =
            TerminalBuffer::new_with_max_lines(std::num::NonZeroUsize::new(1).unwrap());
        let mut line = content.line(Style::default());
        if let Some(started) = started {
            if let Some(effects) = &mut line.decorations {
                for effect in Arc::make_mut(effects) {
                    effect.started = started;
                }
            }
            if let Some(objects) = &mut line.objects {
                for object in Arc::make_mut(objects) {
                    object.started = started;
                }
            }
        }
        buffer.push_line(Arc::new(line));
        let tooltips = Rc::new(RefCell::new(Vec::new()));
        let requests = tooltips.clone();
        Self {
            tooltips,
            collect_tooltip: Rc::new(move |request| requests.borrow_mut().push(request)),
            search_selection: Rc::new(Cell::new(false)),
            content,
            started,
            buffer: RefCell::new(buffer),
            selection: Rc::default(),
        }
    }
    fn pane<Message>(
        &self,
        on_link: Rc<dyn Fn(LinkClickEvent) -> Message>,
    ) -> TerminalPane<'_, Message> {
        TerminalPane::with_selection(
            self.buffer.borrow(),
            self.selection.clone(),
            self.search_selection.clone(),
        )
        .shrink()
        .on_link(Some(on_link))
    }
}
impl<Message: 'static> TerminalSpan<Message> {
    pub fn started_at(mut self, started: Option<iced::time::Instant>) -> Self {
        self.started = started;
        self
    }
    pub fn inline_widgets(mut self, resolver: Option<crate::inline_object::Resolver>) -> Self {
        self.inline_resolver = resolver;
        self
    }
    pub fn new(
        content: Arc<InlineContent>,
        on_link: impl Fn(LinkClickEvent) -> Message + 'static,
        on_tooltip: impl Fn(LinkTooltipCallback) -> Message + 'static,
    ) -> Self {
        Self {
            content,
            inline_resolver: None,
            started: None,
            on_link: Rc::new(on_link),
            on_tooltip: Rc::new(on_tooltip),
        }
    }
}
impl<Message: 'static> Widget<Message, Theme, Renderer> for TerminalSpan<Message> {
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn advanced::widget::Operation,
    ) {
        if let Some(child) = tree.children.first_mut()
            && let Some(host) =
                crate::inline_object::Host::<Message, Theme, Renderer>::get_mut(child)
        {
            host.operate(layout, renderer, operation);
        }
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<advanced::overlay::Element<'b, Message, Theme, Renderer>> {
        crate::inline_object::Host::<Message, Theme, Renderer>::get_mut(tree.children.first_mut()?)?
            .overlay(layout, renderer, viewport, translation)
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.content.clone(), self.started))
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        if !Arc::ptr_eq(&state.content, &self.content) || state.started != self.started {
            *state = State::new(self.content.clone(), self.started);
            tree.children.clear();
        }
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_ref::<State>();
        let mut pane = state
            .pane(self.on_link.clone())
            .inline_widgets(self.inline_resolver.clone());
        if tree.children.is_empty() {
            tree.children
                .push(Tree::new(&pane as &dyn Widget<Message, Theme, Renderer>));
        }
        <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::layout(
            &mut pane,
            &mut tree.children[0],
            renderer,
            limits,
        )
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        if let Some(child) = tree.children.first() {
            state
                .pane(self.on_link.clone())
                .inline_widgets(self.inline_resolver.clone())
                .draw(child, renderer, theme, style, layout, cursor, viewport);
        }
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn advanced::Clipboard,
        shell: &mut advanced::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        if let Some(child) = tree.children.first_mut() {
            let mut pane = state
                .pane(self.on_link.clone())
                .inline_widgets(self.inline_resolver.clone())
                .on_link_tooltip(Some(state.collect_tooltip.clone()));
            <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::update(
                &mut pane, child, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
            for request in state.tooltips.take() {
                shell.publish((self.on_tooltip)(request));
            }
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        tree.children
            .first()
            .map_or(mouse::Interaction::default(), |child| {
                <TerminalPane<'_, Message> as Widget<Message, Theme, Renderer>>::mouse_interaction(
                    &state
                        .pane(self.on_link.clone())
                        .inline_widgets(self.inline_resolver.clone()),
                    child,
                    layout,
                    cursor,
                    viewport,
                    renderer,
                )
            })
    }
}
impl<Message: 'static> From<TerminalSpan<Message>> for Element<'static, Message, Theme, Renderer> {
    fn from(span: TerminalSpan<Message>) -> Self {
        Self::new(span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_buffer::selection::BufferPosition;
    use smudgy_session_model::{SpliceRun, TextAttributesUpdate};

    #[test]
    fn a_new_mount_clock_restamps_the_line_even_with_the_same_content() {
        let content = Arc::new(InlineContent {
            fonts: Arc::default(),
            runs: Arc::new(vec![SpliceRun {
                text: "clocked".into(),
                fg: None,
                bg: None,
                attributes: TextAttributesUpdate::UNSET,
                link: None,
            }]),
            decorations: Arc::new(Vec::new()),
            objects: Arc::new(vec![
                smudgy_session_model::inline_content::InlineObject::new(
                    0..7,
                    Arc::new(()),
                    smudgy_session_model::inline_content::InlineOwner::default(),
                ),
            ]),
        });
        let first = iced::time::Instant::now();
        let later = first + std::time::Duration::from_secs(5);
        let span = TerminalSpan::<()>::new(content.clone(), |_| (), |_| ()).started_at(Some(first));
        let mut tree = Tree::new(&span as &dyn Widget<(), Theme, Renderer>);
        let stamped = |tree: &Tree| {
            let state = tree.state.downcast_ref::<State>();
            let buffer = state.buffer.borrow();
            let line = buffer.iter_rev_with_line_number(None).next().unwrap().1;
            line.styled_line.objects.as_ref().unwrap()[0].started
        };
        assert_eq!(stamped(&tree), first);

        TerminalSpan::<()>::new(content.clone(), |_| (), |_| ())
            .started_at(Some(first))
            .diff(&mut tree);
        assert_eq!(stamped(&tree), first);

        TerminalSpan::<()>::new(content, |_| (), |_| ())
            .started_at(Some(later))
            .diff(&mut tree);
        assert_eq!(stamped(&tree), later);
    }

    #[test]
    fn ordinary_span_shrinks_wraps_and_keeps_local_selection_across_views() {
        let content = Arc::new(InlineContent {
            fonts: Arc::default(),
            runs: Arc::new(vec![SpliceRun {
                text: "selectable terminal text in a container".into(),
                fg: None,
                bg: None,
                attributes: TextAttributesUpdate::UNSET,
                link: None,
            }]),
            decorations: Arc::new(Vec::new()),
            objects: Arc::new(Vec::new()),
        });
        let renderer = Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::MONOSPACE,
            iced::Pixels(16.0),
        ));
        let mut span = TerminalSpan::new(content.clone(), |_| (), |_| ());
        let mut tree = Tree::new(&span as &dyn Widget<(), Theme, Renderer>);
        let wide = span.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(800.0, 600.0)),
        );
        assert!(wide.size().width > 0.0 && wide.size().width < 800.0);
        assert!(wide.size().height < 60.0);
        let state = tree.state.downcast_ref::<State>();
        *state.selection.borrow_mut() = Selection::Selected {
            from: BufferPosition { line: 1, column: 0 },
            to: BufferPosition {
                line: 1,
                column: 10,
            },
        };
        let mut next_view = TerminalSpan::new(content, |_| (), |_| ());
        next_view.diff(&mut tree);
        let narrow = next_view.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(120.0, 600.0)),
        );
        assert!(narrow.size().height > wide.size().height);
        let state = tree.state.downcast_ref::<State>();
        assert_eq!(
            state
                .buffer
                .borrow()
                .selected_text(&state.selection.borrow()),
            "selectable"
        );
    }
}
