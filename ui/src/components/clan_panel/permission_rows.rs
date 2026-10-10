//! Individual permissions, with their consequences beside the switch.
use crate::theme::{self, Element as ThemedElement};
use iced::widget::{column, container, row, rule, text, toggler};
use iced::{Alignment, Length};

pub fn help(action: &str) -> String {
    crate::i18n::translate(&format!(
        "permission-help-{}",
        action.replace(['.', '_'], "-")
    ))
}

pub fn permission<'a, Message: Clone + 'a>(
    action: &'static str,
    checked: bool,
    enabled: bool,
    change: impl Fn(bool) -> Message + 'a,
) -> ThemedElement<'a, Message> {
    let mut switch = toggler(checked).size(22);
    if enabled {
        switch = switch.on_toggle(change);
    }
    column![
        container(
            row![
                column![
                    text(crate::presets::action_label(action)).size(15),
                    text(help(action)).size(13).style(description),
                ]
                .spacing(6)
                .width(Length::Fill),
                switch,
            ]
            .spacing(24)
            .align_y(Alignment::Start)
        )
        .padding([14, 0]),
        rule::horizontal(1),
    ]
    .into()
}

/// Permission explanations are decision text, so retain readable contrast.
pub(super) fn description(theme: &theme::Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.72)),
    }
}
