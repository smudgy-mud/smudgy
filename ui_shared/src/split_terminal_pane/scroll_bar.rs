use iced::{
    Color, Element, Event, Rectangle,
    advanced::{Widget, layout, mouse, renderer::Quad, widget::tree},
    border, touch,
};

pub const SCROLLBAR_WIDTH: f32 = 12.0;
const MIN_SCROLLBAR_HEIGHT: f32 = 32.0;

pub struct ScrollBar {
    min: f32,
    max: f32,
    visible: f32,
    value: f32,
    on_change: Option<Box<dyn Fn(f32)>>,
}

impl ScrollBar {
    pub fn new(min: f32, max: f32, visible: f32, value: f32) -> Self {
        Self {
            min,
            max,
            visible,
            value,
            on_change: None,
        }
    }

    pub fn on_change(mut self, f: impl Fn(f32) + 'static) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }
}

#[derive(Default)]
enum DragState {
    #[default]
    Idle,
    Dragging {
        y_offset: f32,
    },
}

#[derive(Default)]
struct State {
    grabber_bounds: iced::Rectangle,
    drag_state: DragState,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for ScrollBar
where
    Renderer: iced::advanced::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Fixed(SCROLLBAR_WIDTH), iced::Length::Fill)
    }

    fn size_hint(&self) -> iced::Size<iced::Length> {
        iced::Size::new(iced::Length::Fixed(SCROLLBAR_WIDTH), iced::Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();

        let height = limits.max().height;
        let min_height = height.min(MIN_SCROLLBAR_HEIGHT);

        let total_range = self.max - self.min;
        let visible_range = self.visible - self.min;
        let position = ((self.value - self.min) / total_range.max(1.0)).clamp(0.0, 1.0);
        let visible_portion = (visible_range / total_range.max(1.0)).clamp(0.0, 1.0);
        let grabber_height = (height * visible_portion).max(min_height);

        state.grabber_bounds = Rectangle {
            x: 0.0,
            y: (height - grabber_height) * position,
            width: SCROLLBAR_WIDTH,
            height: grabber_height,
        };

        layout::atomic(limits, iced::Length::Fill, iced::Length::Fill)
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        _viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();

        let grabber_bounds = state.grabber_bounds + iced::Vector::new(bounds.x, bounds.y);

        let is_hovered = cursor.is_over(grabber_bounds)
            || matches!(state.drag_state, DragState::Dragging { .. });

        renderer.fill_quad(
            Quad {
                bounds: grabber_bounds,
                border: border::rounded(3),
                ..Default::default()
            },
            Color::from_rgba8(255, 255, 255, if is_hovered { 0.20 } else { 0.10 }),
        );
    }

    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &iced::Event,
        layout: layout::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                let bounds = layout.bounds();
                let state: &mut State = tree.state.downcast_mut::<State>();
                let grabber_bounds = state.grabber_bounds + iced::Vector::new(bounds.x, bounds.y);

                if cursor
                    .position_in(grabber_bounds)
                    .map(|click_position| {
                        let y_offset = click_position.y;

                        state.drag_state = DragState::Dragging { y_offset };
                    })
                    .is_some()
                {
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. }) => {
                let state = tree.state.downcast_mut::<State>();
                if let DragState::Dragging { .. } = state.drag_state {
                    state.drag_state = DragState::Idle;

                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let state = tree.state.downcast_mut::<State>();
                if let DragState::Dragging { y_offset } = state.drag_state {
                    if let Some(f) = self.on_change.as_ref() {
                        let bounds = layout.bounds();
                        let new_grabber_y = (position.y - y_offset - bounds.y)
                            .clamp(0.0, bounds.height - state.grabber_bounds.height);

                        let max = (bounds.height - state.grabber_bounds.height).max(1.0);
                        let position = new_grabber_y / max;
                        // map position to min..max
                        let new_value = position * (self.max - self.min) + self.min;

                        f(new_value);

                        shell.invalidate_layout();
                        shell.request_redraw();
                    }
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }
}

impl<'a, Message, Theme, Renderer> From<ScrollBar> for Element<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn from(scroll_bar: ScrollBar) -> Self {
        Element::new(scroll_bar)
    }
}

pub fn scroll_bar(min: f32, max: f32, visible: f32, value: f32) -> ScrollBar {
    ScrollBar::new(min, max, visible, value)
}
