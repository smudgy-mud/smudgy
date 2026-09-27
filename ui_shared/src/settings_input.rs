//! Shared input-preference controls. Hosts persist the committed snapshot and
//! apply it to their own session/input state without a virtual host object.

use iced::widget::{checkbox, column, pick_list, text, text_input};
use serde::{Deserialize, Serialize};
use smudgy_session_model::input_policy::CommandSyntax;
use smudgy_theme::{Element, Theme, builtins};

use crate::prefs::CommandInputBehavior;

pub const MAX_HISTORY: usize = 1_000_000;

#[derive(Debug, Clone)]
pub enum SyntaxMessage {
    SeparatorChanged(String),
    RawPrefixChanged(String),
}

pub struct SyntaxState {
    value: CommandSyntax,
}

impl SyntaxState {
    #[must_use]
    pub fn new(value: CommandSyntax) -> Self {
        Self { value }
    }

    #[must_use]
    pub fn value(&self) -> &CommandSyntax {
        &self.value
    }

    pub fn replace(&mut self, value: CommandSyntax) {
        self.value = value;
    }

    pub fn update(&mut self, message: SyntaxMessage) -> bool {
        match message {
            SyntaxMessage::SeparatorChanged(value) => {
                let value: String = value.chars().take(4).collect();
                if self.value.separator == value {
                    return false;
                }
                self.value.separator = value;
                true
            }
            SyntaxMessage::RawPrefixChanged(value) => {
                if self.value.raw_prefix == value {
                    return false;
                }
                self.value.raw_prefix = value;
                true
            }
        }
    }
}

pub fn syntax_view<'a>(
    state: &'a SyntaxState,
    label: &impl Fn(&'static str) -> String,
) -> Element<'a, SyntaxMessage> {
    column![
        column![
            dim(label("preferences-command-separator")),
            text_input(";", &state.value.separator)
                .size(14)
                .width(80)
                .on_input(SyntaxMessage::SeparatorChanged),
            dim(label("preferences-command-separator-help")),
        ]
        .spacing(2),
        column![
            dim(label("preferences-raw-prefix")),
            text_input("\\", &state.value.raw_prefix)
                .size(14)
                .width(80)
                .on_input(SyntaxMessage::RawPrefixChanged),
            dim(label("preferences-raw-prefix-help")),
        ]
        .spacing(2),
    ]
    .spacing(10)
    .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPreferences {
    pub command_input_behavior: CommandInputBehavior,
    pub mask_input_on_server_echo: bool,
    #[serde(default = "default_true")]
    pub reconnect_on_send_error: bool,
    pub history_case_sensitive_match: bool,
    pub max_history: usize,
}

impl Default for InputPreferences {
    fn default() -> Self {
        Self {
            command_input_behavior: CommandInputBehavior::default(),
            mask_input_on_server_echo: true,
            reconnect_on_send_error: true,
            history_case_sensitive_match: false,
            max_history: 1_000,
        }
    }
}

impl InputPreferences {
    pub fn validate(self) -> Result<Self, &'static str> {
        (self.max_history <= MAX_HISTORY)
            .then_some(self)
            .ok_or("input history size exceeds one million")
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    CommandInputBehaviorSelected(CommandInputBehavior),
    MaskInputOnServerEchoToggled(bool),
    ReconnectOnSendErrorToggled(bool),
    HistoryCaseSensitiveMatchToggled(bool),
    MaxHistoryChanged(String),
    MaxHistorySubmitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    CommandInputBehavior(CommandInputBehavior),
    MaskInputOnServerEcho(bool),
    ReconnectOnSendError(bool),
    HistoryCaseSensitiveMatch(bool),
    MaxHistory(usize),
}

pub struct State {
    value: InputPreferences,
    max_history_input: String,
}

impl State {
    #[must_use]
    pub fn new(value: InputPreferences) -> Self {
        Self {
            value,
            max_history_input: value.max_history.to_string(),
        }
    }

    #[must_use]
    pub fn value(&self) -> InputPreferences {
        self.value
    }

    pub fn replace(&mut self, value: InputPreferences) {
        *self = Self::new(value);
    }

    pub fn update(&mut self, message: Message) -> Option<Change> {
        match message {
            Message::CommandInputBehaviorSelected(behavior) => {
                self.value.command_input_behavior = behavior;
                Some(Change::CommandInputBehavior(behavior))
            }
            Message::MaskInputOnServerEchoToggled(enabled) => {
                self.value.mask_input_on_server_echo = enabled;
                Some(Change::MaskInputOnServerEcho(enabled))
            }
            Message::ReconnectOnSendErrorToggled(enabled) => {
                self.value.reconnect_on_send_error = enabled;
                Some(Change::ReconnectOnSendError(enabled))
            }
            Message::HistoryCaseSensitiveMatchToggled(enabled) => {
                self.value.history_case_sensitive_match = enabled;
                Some(Change::HistoryCaseSensitiveMatch(enabled))
            }
            Message::MaxHistoryChanged(input) => {
                self.max_history_input = input;
                None
            }
            Message::MaxHistorySubmitted => {
                let max = parse_max_history(&self.max_history_input)?;
                self.value.max_history = max;
                Some(Change::MaxHistory(max))
            }
        }
    }
}

fn parse_max_history(input: &str) -> Option<usize> {
    let value = input.trim().parse::<usize>().ok()?;
    (value <= MAX_HISTORY).then_some(value)
}

#[derive(Clone, PartialEq, Eq)]
struct InputChoice {
    behavior: CommandInputBehavior,
    label: &'static str,
}

impl std::fmt::Display for InputChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label)
    }
}

fn dim(label: String) -> iced::widget::Text<'static, Theme> {
    text(label).size(11).style(builtins::text::muted)
}

/// The locale lookup is statically dispatched and owns no platform state.
pub fn view<'a>(state: &'a State, label: &impl Fn(&'static str) -> String) -> Element<'a, Message> {
    let choices = vec![
        InputChoice {
            behavior: CommandInputBehavior::SelectAllClearOnBlur,
            label: "Select all on send, clear when unfocused",
        },
        InputChoice {
            behavior: CommandInputBehavior::SelectAll,
            label: "Select all on send",
        },
        InputChoice {
            behavior: CommandInputBehavior::Clear,
            label: "Clear on send",
        },
    ];
    let selected = choices
        .iter()
        .find(|choice| choice.behavior == state.value.command_input_behavior)
        .cloned();

    let mut max_history = column![
        dim(label("preferences-max-history")),
        text_input("1000", &state.max_history_input)
            .size(14)
            .width(120)
            .on_input(Message::MaxHistoryChanged)
            .on_submit(Message::MaxHistorySubmitted),
    ]
    .spacing(2);
    if parse_max_history(&state.max_history_input).is_none() {
        max_history = max_history.push(
            text(label("validation-invalid-value"))
                .size(11)
                .style(builtins::text::danger),
        );
    }
    max_history = max_history.push(dim(label("preferences-max-history-help")));

    column![
        column![
            dim(label("preferences-command-input")),
            pick_list(choices, selected, |choice| {
                Message::CommandInputBehaviorSelected(choice.behavior)
            })
            .text_size(13)
            .width(320),
            dim(label("preferences-command-input-help")),
        ]
        .spacing(2),
        column![
            checkbox(state.value.mask_input_on_server_echo)
                .label(label("preferences-mask-password-input"))
                .on_toggle(Message::MaskInputOnServerEchoToggled),
            dim("When a MUD turns off echo for a password prompt, the input shows dots instead of your text (with an eye button to peek). Turn off to keep your typing visible.".to_owned()),
        ]
        .spacing(2),
        column![
            checkbox(state.value.reconnect_on_send_error)
                .label(label("preferences-reconnect-on-send-error"))
                .on_toggle(Message::ReconnectOnSendErrorToggled),
            dim(label("preferences-reconnect-on-send-error-help")),
        ]
        .spacing(2),
        column![
            checkbox(state.value.history_case_sensitive_match)
                .label(label("preferences-history-case-sensitive-match"))
                .on_toggle(Message::HistoryCaseSensitiveMatchToggled),
            dim(label("preferences-history-case-sensitive-match-help")),
        ]
        .spacing(2),
        max_history,
    ]
    .spacing(10)
    .into()
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_syntax_edits_commit_immediately_and_bound_the_separator() {
        let mut state = SyntaxState::new(CommandSyntax::default());
        assert!(state.update(SyntaxMessage::SeparatorChanged("::extra".into())));
        assert_eq!(state.value().separator, "::ex");
        assert!(!state.update(SyntaxMessage::SeparatorChanged("::ex".into())));
        assert!(state.update(SyntaxMessage::RawPrefixChanged("!".into())));
        assert_eq!(state.value().raw_prefix, "!");
    }

    #[test]
    fn history_limit_accepts_zero_and_commits_only_valid_values() {
        let mut state = State::new(InputPreferences::default());
        assert!(
            state
                .update(Message::MaxHistoryChanged("1000001".into()))
                .is_none()
        );
        assert!(state.update(Message::MaxHistorySubmitted).is_none());
        assert_eq!(state.value().max_history, 1_000);
        assert!(
            state
                .update(Message::MaxHistoryChanged("0".into()))
                .is_none()
        );
        assert_eq!(
            state.update(Message::MaxHistorySubmitted),
            Some(Change::MaxHistory(0))
        );
        assert_eq!(state.value().max_history, 0);
    }

    #[test]
    fn toggles_update_the_snapshot_without_touching_other_fields() {
        let mut state = State::new(InputPreferences::default());
        assert_eq!(
            state.update(Message::MaskInputOnServerEchoToggled(false)),
            Some(Change::MaskInputOnServerEcho(false))
        );
        assert_eq!(
            state.update(Message::ReconnectOnSendErrorToggled(false)),
            Some(Change::ReconnectOnSendError(false))
        );
        assert_eq!(
            state.update(Message::HistoryCaseSensitiveMatchToggled(true)),
            Some(Change::HistoryCaseSensitiveMatch(true))
        );
        assert_eq!(state.value().max_history, 1_000);
    }
}
