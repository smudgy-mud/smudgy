//! Dialog content that scrolls without pushing its action buttons out of view.

use iced::widget::{container, scrollable};
use iced::{Length, Padding};

use super::grow_column::GrowColumn;
use crate::theme::Element;

/// Keep the footer outside the scroll viewport. The enclosing dialog must
/// supply bounded height (normally the window height and a maximum).
pub fn scrolling_body<'a, Message: Clone + 'a>(
    body: impl Into<Element<'a, Message>>,
    footer: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    GrowColumn::bounded(
        vec![
            scrollable(container(body).padding(Padding::ZERO.right(12)))
                .height(Length::Shrink)
                .into(),
            footer.into(),
        ],
        0,
    )
    .spacing(12.0)
    .into()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use iced::advanced::{
        Layout, Shell,
        layout::Limits,
        renderer::{Headless, Style},
        widget::{Operation, Tree},
    };
    use iced::{Event, Rectangle, Size, mouse};

    #[derive(Default)]
    struct Geometry {
        text: Vec<(String, Rectangle)>,
        scrolls: Vec<Rectangle>,
        in_scroll: bool,
        entering_scroll: bool,
    }
    impl Operation for Geometry {
        fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
            let previous = self.in_scroll;
            self.in_scroll |= std::mem::take(&mut self.entering_scroll);
            visit(self);
            self.in_scroll = previous;
        }
        fn text(&mut self, _: Option<&iced::widget::Id>, bounds: Rectangle, text: &str) {
            self.text.push((text.to_string(), bounds));
        }
        fn scrollable(
            &mut self,
            _: Option<&iced::widget::Id>,
            bounds: Rectangle,
            _: Rectangle,
            _: iced::Vector,
            _: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            if !self.in_scroll {
                self.scrolls.push(bounds);
            }
            self.entering_scroll = true;
        }
    }

    /// Check actual action geometry and dispatch clicks before and after
    /// scrolling. The outer dialog fitting the window is not sufficient:
    /// descendants can still overflow it or intercept the footer's clicks.
    pub(crate) async fn check_actions<M: Clone>(
        mut element: Element<'_, M>,
        size: (u32, u32),
        actions: &[String],
        screenshot: &str,
    ) -> Vec<M> {
        use crate::assets::fonts;
        iced_graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(fonts::GEIST_VF_BYTES.into());
        let mut renderer =
            <iced::Renderer as Headless>::new(fonts::GEIST_VF, 16.0.into(), Some("tiny-skia"))
                .await
                .unwrap();
        let viewport = Rectangle::with_size(Size::new(size.0 as f32, size.1 as f32));
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, viewport.size()),
        );
        assert!(node.size().width <= viewport.width && node.size().height <= viewport.height);
        let layout = Layout::new(&node);
        let mut geometry = Geometry::default();
        element
            .as_widget_mut()
            .operate(&mut tree, layout, &renderer, &mut geometry);
        let mut messages = Vec::new();
        for scrolled in [false, true] {
            if scrolled && let Some(bounds) = geometry.scrolls.first() {
                element.as_widget_mut().update(
                    &mut tree,
                    &Event::Mouse(mouse::Event::WheelScrolled {
                        delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1000.0 },
                    }),
                    layout,
                    mouse::Cursor::Available(bounds.center()),
                    &renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
            for label in actions {
                let bounds = geometry
                    .text
                    .iter()
                    .rev()
                    .find(|(text, _)| text == label)
                    .unwrap_or_else(|| panic!("missing action {label}: {screenshot}"))
                    .1;
                assert!(
                    bounds.width > 0.0
                        && bounds.height > 0.0
                        && bounds.x >= 0.0
                        && bounds.y >= 0.0
                        && bounds.x + bounds.width <= viewport.width
                        && bounds.y + bounds.height <= viewport.height,
                    "{screenshot} {size:?}: {label} outside viewport: {bounds:?}"
                );
                assert!(
                    geometry
                        .scrolls
                        .iter()
                        .all(|scroll| !scroll.intersects(&bounds)),
                    "{screenshot} {size:?}: {label} overlaps a scroll viewport"
                );
                for event in [
                    mouse::Event::ButtonPressed(mouse::Button::Left),
                    mouse::Event::ButtonReleased(mouse::Button::Left),
                ] {
                    element.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(event),
                        layout,
                        mouse::Cursor::Available(bounds.center()),
                        &renderer,
                        &mut iced::advanced::clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &viewport,
                    );
                }
            }
            if !scrolled && let Some(directory) = std::env::var_os("SMUDGY_MODAL_SCREENSHOTS") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let theme = crate::theme::smudgy();
                element.as_widget_mut().update(
                    &mut tree,
                    &Event::Window(iced::window::Event::RedrawRequested(
                        std::time::Instant::now(),
                    )),
                    layout,
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
                element.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &Style {
                        text_color: theme.styles.text.normal,
                    },
                    layout,
                    mouse::Cursor::Unavailable,
                    &viewport,
                );
                image::save_buffer(
                    directory.join(format!("{screenshot}-{}-{}.png", size.0, size.1)),
                    &renderer.screenshot(
                        Size::new(size.0, size.1),
                        1.0,
                        theme.styles.general.background,
                    ),
                    size.0,
                    size.1,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
        messages
    }

    #[tokio::test]
    async fn long_forms_keep_actions_outside_the_scroll_viewport() {
        use iced::widget::{button, column, row, text};
        for size in [(360, 300), (520, 480), (900, 720)] {
            let body = column((0..100).map(|n| {
                text(format!(
                    "Permission {n} with an explanation that wraps across several lines."
                ))
                .into()
            }))
            .spacing(12);
            let footer = row![
                button(text("Cancel")).on_press(0),
                button(text("Save")).on_press(1)
            ]
            .spacing(10);
            let messages = check_actions(
                scrolling_body(body, footer),
                size,
                &["Cancel".into(), "Save".into()],
                "form",
            )
            .await;
            assert_eq!(messages, [0, 1, 0, 1]);
        }
    }
}
