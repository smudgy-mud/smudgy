use iced::widget::{button, svg};
use smudgy_theme::{Element, Theme, builtins};

pub fn title_bar_icon_button<M: Clone + 'static>(
    handle: svg::Handle,
    message: M,
) -> Element<'static, M> {
    button(
        svg(handle)
            .width(11)
            .height(11)
            .style(|theme: &Theme, _| svg::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.5)),
            }),
    )
    .style(builtins::button::link)
    .padding(3)
    .on_press(message)
    .into()
}
