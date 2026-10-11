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
    noted(action, checked, enabled, None, change)
}

/// A permission's switch, with `note` under its explanation: where the
/// permission came from, when that matters.
pub fn noted<'a, Message: Clone + 'a>(
    action: &'static str,
    checked: bool,
    enabled: bool,
    note: Option<String>,
    change: impl Fn(bool) -> Message + 'a,
) -> ThemedElement<'a, Message> {
    let mut switch = toggler(checked).size(22);
    if enabled {
        switch = switch.on_toggle(change);
    }
    let mut about = column![
        text(crate::presets::action_label(action)).size(15),
        text(help(action)).size(13).style(description),
    ]
    .spacing(6)
    .width(Length::Fill);
    if let Some(note) = note {
        about = about.push(text(note).size(12).style(theme::builtins::text::muted));
    }
    column![
        container(row![about, switch].spacing(24).align_y(Alignment::Start)).padding([14, 0]),
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
