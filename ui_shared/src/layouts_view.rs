//! Portable presentation of the named-layout dialog.

use iced::Length;
use iced::widget::{Id, button, column, container, row, scrollable, text, text_input};
use smudgy_theme::{Element, builtins};

use crate::layouts_modal::{Message, OmittedRow, SaveOutcome, Stage, State};

/// A host supplies text for its selected locale without baking a locale
/// singleton or a catalog dependency into every shared UI consumer.
pub trait Labels {
    fn text(&self, id: &'static str) -> String;
    fn format(&self, id: &'static str, args: &[(&'static str, &str)]) -> String;
}

pub fn name_input_id() -> Id {
    Id::new("layouts-name-input")
}

pub fn view<'a, L: Labels>(state: &'a State, labels: &L) -> Element<'a, Message> {
    let body: Element<'_, Message> = match &state.stage {
        Stage::Browse => browse(state, labels),
        Stage::SaveAs { name, error } => name_form(
            labels,
            &labels.text("layouts-save-as"),
            name,
            error.as_deref(),
            Message::SaveAsConfirmed,
        ),
        Stage::ConfirmOverwrite { name, .. } => confirm(
            labels,
            labels.format("layouts-confirm-overwrite", &[("name", name)]),
            Message::OverwriteConfirmed,
            &labels.text("layouts-overwrite"),
        ),
        Stage::Rename { to, error, .. } => name_form(
            labels,
            &labels.text("layouts-rename"),
            to,
            error.as_deref(),
            Message::RenameConfirmed,
        ),
        Stage::ConfirmDelete { name } => confirm(
            labels,
            labels.format("layouts-confirm-delete", &[("name", name)]),
            Message::DeleteConfirmed,
            &labels.text("action-delete"),
        ),
        Stage::ConfirmReset => confirm(
            labels,
            labels.text("layouts-confirm-reset"),
            Message::ResetConfirmed,
            &labels.text("layouts-reset"),
        ),
        Stage::KeepOrClose { rows, .. } => keep_or_close(rows, labels),
    };
    let content: Element<'_, Message> = if let Some(error) = &state.storage_error {
        column![
            text(error.clone()).size(12).style(builtins::text::danger),
            body
        ]
        .spacing(10)
        .into()
    } else {
        body
    };
    container(content).padding(16).width(Length::Fill).into()
}

fn browse<'a>(state: &'a State, labels: &impl Labels) -> Element<'a, Message> {
    let mut listing = column![].spacing(4);
    if state.layouts.is_empty() {
        listing = listing.push(
            text(labels.format("layouts-empty", &[("server", state.server.as_str())]))
                .size(13)
                .style(builtins::text::muted),
        );
    }
    for name in &state.layouts {
        let entry = row![
            button(text(name.clone()).size(14))
                .style(builtins::button::list_item)
                .width(Length::Fill)
                .on_press(Message::ApplyPressed(name.clone())),
            button(text(labels.text("layouts-overwrite")).size(12))
                .style(builtins::button::subtle)
                .padding([4, 8])
                .on_press(Message::OverwritePressed(name.clone())),
            button(text(labels.text("layouts-rename")).size(12))
                .style(builtins::button::subtle)
                .padding([4, 8])
                .on_press(Message::RenamePressed(name.clone())),
            button(text(labels.text("action-delete")).size(12))
                .style(builtins::button::subtle)
                .padding([4, 8])
                .on_press(Message::DeletePressed(name.clone())),
        ]
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);
        listing = listing.push(entry);
    }

    let mut content = column![
        text(labels.text("layouts-apply-hint"))
            .size(12)
            .style(builtins::text::muted),
        scrollable(listing).height(Length::Fill),
    ]
    .spacing(10)
    .height(Length::Fill);

    if let Some(outcome) = &state.outcome {
        let line: Element<'_, Message> = match outcome {
            SaveOutcome::Saved { name, omitted: 0 } => {
                text(labels.format("layouts-saved", &[("name", name)]))
                    .size(12)
                    .style(builtins::text::muted)
                    .into()
            }
            SaveOutcome::Saved { name, omitted } => {
                let count = omitted.to_string();
                text(labels.format(
                    "layouts-saved-partial",
                    &[("name", name), ("count", &count)],
                ))
                .size(12)
                .style(builtins::text::danger)
                .into()
            }
            SaveOutcome::Failed { error } => {
                text(labels.format("layouts-save-failed", &[("error", error)]))
                    .size(12)
                    .style(builtins::text::danger)
                    .into()
            }
        };
        content = content.push(line);
    }

    content = content.push(
        row![
            button(text(labels.text("layouts-save-as")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::SaveAsPressed),
            button(text(labels.text("layouts-reset")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::ResetPressed),
        ]
        .spacing(8),
    );
    content.into()
}

/// The shared one-field form for save-as and rename: label, input (Enter
/// submits, Escape backs out through the modal's global handler), error
/// line, confirm/cancel.
fn name_form<'a>(
    labels: &impl Labels,
    label: &str,
    value: &str,
    error: Option<&'a str>,
    submit: Message,
) -> Element<'a, Message> {
    let error_line: Element<'_, Message> = match error {
        Some(error) => text(error.to_string())
            .size(12)
            .style(builtins::text::danger)
            .into(),
        None => iced::widget::space::horizontal().into(),
    };
    column![
        text(label.to_string()).size(14),
        text_input(&labels.text("layouts-name-placeholder"), value)
            .id(name_input_id())
            .on_input(Message::NameChanged)
            .on_submit(submit.clone()),
        error_line,
        row![
            button(text(labels.text("action-save")).size(13))
                .style(builtins::button::primary)
                .on_press(submit),
            button(text(labels.text("action-cancel")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::Back),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .into()
}

/// A confirmation stage: the question, then an explicit confirm beside a
/// cancel.
fn confirm<'a>(
    labels: &impl Labels,
    question: String,
    action: Message,
    action_label: &str,
) -> Element<'a, Message> {
    column![
        text(question).size(14),
        row![
            button(text(action_label.to_string()).size(13))
                .style(builtins::button::primary)
                .on_press(action),
            button(text(labels.text("action-cancel")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::Back),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .into()
}

fn keep_or_close<'a>(rows: &'a [OmittedRow], labels: &impl Labels) -> Element<'a, Message> {
    let mut listing = column![].spacing(6);
    for row_state in rows {
        let session = row_state.session;
        // The chosen side is marked twice over: the primary button style
        // (color) and a check-glyph label (shape) — the answer stays
        // legible without color vision.
        let keep_style = if row_state.close {
            builtins::button::secondary
        } else {
            builtins::button::primary
        };
        let close_style = if row_state.close {
            builtins::button::primary
        } else {
            builtins::button::secondary
        };
        let keep_label = if row_state.close {
            labels.text("layouts-keep")
        } else {
            labels.text("layouts-keep-chosen")
        };
        let close_label = if row_state.close {
            labels.text("layouts-close-chosen")
        } else {
            labels.text("layouts-close")
        };
        listing = listing.push(
            row![
                text(row_state.label.clone()).size(13).width(Length::Fill),
                button(text(keep_label).size(12))
                    .style(keep_style)
                    .padding([4, 8])
                    .on_press(Message::ToggleClose(session, false)),
                button(text(close_label).size(12))
                    .style(close_style)
                    .padding([4, 8])
                    .on_press(Message::ToggleClose(session, true)),
            ]
            .spacing(6)
            .align_y(iced::alignment::Vertical::Center),
        );
    }
    column![
        text(labels.text("layouts-keep-or-close-intro")).size(13),
        scrollable(listing).height(Length::Fill),
        row![
            button(text(labels.text("layouts-apply")).size(13))
                .style(builtins::button::primary)
                .on_press(Message::KeepOrCloseConfirmed),
            button(text(labels.text("action-cancel")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::Back),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .height(Length::Fill)
    .into()
}
