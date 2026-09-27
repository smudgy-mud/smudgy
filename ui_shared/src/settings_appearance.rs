//! The terminal-appearance slice of Settings, shared by native and browser.
//! The reducer emits committed values; each host owns persistence and fan-out.

use iced::widget::{checkbox, column, pick_list, text, text_input};
use serde::{Deserialize, Serialize};
use smudgy_session_model::{DEFAULT_SCROLLBACK_LINES, MAX_SCROLLBACK_LINES, MIN_SCROLLBACK_LINES};
use smudgy_theme::{Element, Theme, builtins};

use crate::prefs::TerminalBoldMode;

pub const MIN_FONT_SIZE: f32 = 8.0;
pub const MAX_FONT_SIZE: f32 = 40.0;
pub const MIN_LINE_LENGTH: u16 = 20;
pub const MAX_LINE_LENGTH: u16 = 1_000;
pub use smudgy_session_model::MAX_LINK_TOOLTIP_DELAY_MS;
pub const DEFAULT_LINK_TOOLTIP_DELAY_MS: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    pub font_size: f32,
    pub bold_mode: TerminalBoldMode,
    pub disable_blink: bool,
    #[serde(default)]
    pub line_length: Option<u16>,
    #[serde(default = "default_link_tooltip_delay_ms")]
    pub link_tooltip_delay_ms: u64,
    #[serde(default)]
    pub theme_extended_colors: bool,
    #[serde(default)]
    pub hide_pane_headers: bool,
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
}

const fn default_scrollback_lines() -> usize {
    DEFAULT_SCROLLBACK_LINES
}

const fn default_link_tooltip_delay_ms() -> u64 {
    DEFAULT_LINK_TOOLTIP_DELAY_MS
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            bold_mode: TerminalBoldMode::default(),
            disable_blink: false,
            line_length: None,
            link_tooltip_delay_ms: DEFAULT_LINK_TOOLTIP_DELAY_MS,
            theme_extended_colors: false,
            hide_pane_headers: false,
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
        }
    }
}

impl Appearance {
    pub fn validate(self) -> Result<Self, &'static str> {
        if self.font_size.is_finite()
            && (MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&self.font_size)
            && self
                .line_length
                .is_none_or(|length| (MIN_LINE_LENGTH..=MAX_LINE_LENGTH).contains(&length))
            && self.link_tooltip_delay_ms <= MAX_LINK_TOOLTIP_DELAY_MS
            && (MIN_SCROLLBACK_LINES..=MAX_SCROLLBACK_LINES).contains(&self.scrollback_lines)
        {
            Ok(self)
        } else {
            Err(
                "terminal display settings contain an invalid font size, wrap width, tooltip delay, or scrollback limit",
            )
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    FontSizeChanged(String),
    FontSizeSubmitted,
    BoldModeSelected(TerminalBoldMode),
    DisableBlinkToggled(bool),
    LineLengthChanged(String),
    LineLengthSubmitted,
    LinkTooltipDelayChanged(String),
    LinkTooltipDelaySubmitted,
    ThemeExtendedColorsToggled(bool),
    HidePaneHeadersToggled(bool),
    ScrollbackLinesChanged(String),
    ScrollbackLinesSubmitted,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    FontSize(f32),
    BoldMode(TerminalBoldMode),
    DisableBlink(bool),
    LineLength(Option<u16>),
    LinkTooltipDelay(u64),
    ThemeExtendedColors(bool),
    HidePaneHeaders(bool),
    ScrollbackLines(usize),
}

pub struct State {
    value: Appearance,
    font_size_input: String,
    line_length_input: String,
    link_tooltip_delay_input: String,
    scrollback_lines_input: String,
}

impl State {
    #[must_use]
    pub fn new(value: Appearance) -> Self {
        Self {
            value,
            font_size_input: value.font_size.to_string(),
            line_length_input: value
                .line_length
                .map_or(String::new(), |length| length.to_string()),
            link_tooltip_delay_input: value.link_tooltip_delay_ms.to_string(),
            scrollback_lines_input: value.scrollback_lines.to_string(),
        }
    }

    #[must_use]
    pub fn value(&self) -> Appearance {
        self.value
    }

    pub fn replace(&mut self, value: Appearance) {
        *self = Self::new(value);
    }

    pub fn update(&mut self, message: Message) -> Option<Change> {
        match message {
            Message::FontSizeChanged(input) => {
                self.font_size_input = input;
                None
            }
            Message::FontSizeSubmitted => {
                let size = parse_font_size(&self.font_size_input)?;
                self.value.font_size = size;
                Some(Change::FontSize(size))
            }
            Message::BoldModeSelected(mode) => {
                self.value.bold_mode = mode;
                Some(Change::BoldMode(mode))
            }
            Message::DisableBlinkToggled(disable) => {
                self.value.disable_blink = disable;
                Some(Change::DisableBlink(disable))
            }
            Message::LineLengthChanged(input) => {
                self.line_length_input = input;
                None
            }
            Message::LineLengthSubmitted => {
                let length = parse_line_length(&self.line_length_input)?;
                self.value.line_length = length;
                Some(Change::LineLength(length))
            }
            Message::LinkTooltipDelayChanged(input) => {
                self.link_tooltip_delay_input = input;
                None
            }
            Message::LinkTooltipDelaySubmitted => {
                let delay = parse_link_tooltip_delay(&self.link_tooltip_delay_input)?;
                self.value.link_tooltip_delay_ms = delay;
                Some(Change::LinkTooltipDelay(delay))
            }
            Message::ThemeExtendedColorsToggled(enabled) => {
                self.value.theme_extended_colors = enabled;
                Some(Change::ThemeExtendedColors(enabled))
            }
            Message::HidePaneHeadersToggled(enabled) => {
                self.value.hide_pane_headers = enabled;
                Some(Change::HidePaneHeaders(enabled))
            }
            Message::ScrollbackLinesChanged(input) => {
                self.scrollback_lines_input = input;
                None
            }
            Message::ScrollbackLinesSubmitted => {
                let limit = parse_scrollback_lines(&self.scrollback_lines_input)?;
                self.value.scrollback_lines = limit;
                Some(Change::ScrollbackLines(limit))
            }
        }
    }
}

fn parse_font_size(input: &str) -> Option<f32> {
    let size = input.trim().parse::<f32>().ok()?;
    (size.is_finite() && (MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&size)).then_some(size)
}

fn parse_line_length(input: &str) -> Option<Option<u16>> {
    let input = input.trim();
    if input.is_empty() {
        return Some(None);
    }
    let length = input.parse::<u16>().ok()?;
    (MIN_LINE_LENGTH..=MAX_LINE_LENGTH)
        .contains(&length)
        .then_some(Some(length))
}

fn parse_link_tooltip_delay(input: &str) -> Option<u64> {
    let delay = input.trim().parse::<u64>().ok()?;
    (delay <= MAX_LINK_TOOLTIP_DELAY_MS).then_some(delay)
}

fn parse_scrollback_lines(input: &str) -> Option<usize> {
    let lines = input.trim().parse::<usize>().ok()?;
    (MIN_SCROLLBACK_LINES..=MAX_SCROLLBACK_LINES)
        .contains(&lines)
        .then_some(lines)
}

#[derive(Clone, PartialEq, Eq)]
struct BoldChoice {
    mode: TerminalBoldMode,
    label: String,
}

impl std::fmt::Display for BoldChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn dim(label: String) -> iced::widget::Text<'static, Theme> {
    text(label).size(11).style(builtins::text::muted)
}

/// `label` is a host-owned locale lookup; the view never reads a process-wide
/// locale or a platform store. The closure is statically dispatched.
pub fn view<'a>(state: &'a State, label: &impl Fn(&'static str) -> String) -> Element<'a, Message> {
    let choices = [
        (TerminalBoldMode::Bold, "preferences-bold-mode-bold"),
        (TerminalBoldMode::Bright, "preferences-bold-mode-bright"),
        (
            TerminalBoldMode::BoldAndBright,
            "preferences-bold-mode-both",
        ),
    ]
    .into_iter()
    .map(|(mode, id)| BoldChoice {
        mode,
        label: label(id),
    })
    .collect::<Vec<_>>();
    let selected = choices
        .iter()
        .find(|choice| choice.mode == state.value.bold_mode)
        .cloned();
    let mut font_size = column![
        dim(label("preferences-font-size")),
        text_input("16", &state.font_size_input)
            .size(14)
            .width(120)
            .on_input(Message::FontSizeChanged)
            .on_submit(Message::FontSizeSubmitted),
    ]
    .spacing(2);
    if parse_font_size(&state.font_size_input).is_none() {
        font_size = font_size.push(
            text(label("validation-invalid-value"))
                .size(11)
                .style(builtins::text::danger),
        );
    }
    font_size = font_size.push(dim(label("preferences-press-enter")));

    let mut line_length = column![
        dim(label("preferences-line-length")),
        text_input(&label("preferences-wrap-window"), &state.line_length_input)
            .size(14)
            .width(120)
            .on_input(Message::LineLengthChanged)
            .on_submit(Message::LineLengthSubmitted),
    ]
    .spacing(2);
    if parse_line_length(&state.line_length_input).is_none() {
        line_length = line_length.push(
            text(label("validation-invalid-value"))
                .size(11)
                .style(builtins::text::danger),
        );
    }
    line_length = line_length.push(dim(label("preferences-line-length-help")));

    let mut link_tooltip_delay = column![
        dim(label("preferences-link-tooltip-delay")),
        text_input("0", &state.link_tooltip_delay_input)
            .size(14)
            .width(140)
            .on_input(Message::LinkTooltipDelayChanged)
            .on_submit(Message::LinkTooltipDelaySubmitted),
    ]
    .spacing(2);
    if parse_link_tooltip_delay(&state.link_tooltip_delay_input).is_none() {
        link_tooltip_delay = link_tooltip_delay.push(
            text(label("validation-invalid-value"))
                .size(11)
                .style(builtins::text::danger),
        );
    }
    link_tooltip_delay = link_tooltip_delay.push(dim(label("preferences-link-tooltip-delay-help")));

    column![
        column![
            dim(label("preferences-bold-is-bright")),
            pick_list(choices, selected, |choice| {
                Message::BoldModeSelected(choice.mode)
            })
            .text_size(13)
            .width(280),
            dim(label("preferences-bold-is-bright-help")),
        ]
        .spacing(2),
        column![
            checkbox(state.value.disable_blink)
                .label(label("preferences-disable-blink"))
                .on_toggle(Message::DisableBlinkToggled),
            dim(label("preferences-disable-blink-help")),
        ]
        .spacing(2),
        font_size,
        line_length,
        link_tooltip_delay,
    ]
    .spacing(10)
    .into()
}

/// A theme-specific control, kept separate so native can show it beside its
/// theme picker while the browser can reuse the same reducer and rendering.
pub fn theme_extended_colors_view(
    state: &State,
    label: &impl Fn(&'static str) -> String,
) -> Element<'static, Message> {
    column![
        checkbox(state.value.theme_extended_colors)
            .label(label("preferences-theme-extended-colors"))
            .on_toggle(Message::ThemeExtendedColorsToggled),
        dim(label("preferences-theme-extended-colors-help")),
    ]
    .spacing(2)
    .into()
}

/// The pane-header policy is independent of the font and theme widgets.
pub fn pane_headers_view(
    state: &State,
    label: &impl Fn(&'static str) -> String,
) -> Element<'static, Message> {
    column![
        checkbox(state.value.hide_pane_headers)
            .label(label("preferences-hide-pane-headers"))
            .on_toggle(Message::HidePaneHeadersToggled),
        dim(label("preferences-hide-pane-headers-help")),
    ]
    .spacing(2)
    .into()
}

/// Kept separate so native can place scrollback beside its theme controls.
pub fn scrollback_view<'a>(
    state: &'a State,
    label: &impl Fn(&'static str) -> String,
) -> Element<'a, Message> {
    let mut scrollback = column![
        dim(label("preferences-scrollback")),
        text_input("100000", &state.scrollback_lines_input)
            .size(14)
            .width(140)
            .on_input(Message::ScrollbackLinesChanged)
            .on_submit(Message::ScrollbackLinesSubmitted),
    ]
    .spacing(2);
    if parse_scrollback_lines(&state.scrollback_lines_input).is_none() {
        scrollback = scrollback.push(
            text(label("validation-invalid-value"))
                .size(11)
                .style(builtins::text::danger),
        );
    }
    scrollback
        .push(dim(label("preferences-scrollback-help")))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_committed_valid_font_sizes_emit_changes() {
        let mut state = State::new(Appearance::default());
        assert!(state.update(Message::FontSizeChanged("7".into())).is_none());
        assert!(state.update(Message::FontSizeSubmitted).is_none());
        assert_eq!(state.value().font_size, 16.0);
        assert!(
            state
                .update(Message::FontSizeChanged("21.5".into()))
                .is_none()
        );
        assert_eq!(
            state.update(Message::FontSizeSubmitted),
            Some(Change::FontSize(21.5))
        );
        assert_eq!(state.value().font_size, 21.5);
        assert!(
            Appearance {
                font_size: f32::NAN,
                ..Appearance::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn toggles_update_the_snapshot() {
        let mut state = State::new(Appearance::default());
        assert_eq!(
            state.update(Message::BoldModeSelected(TerminalBoldMode::Bright)),
            Some(Change::BoldMode(TerminalBoldMode::Bright))
        );
        assert_eq!(
            state.update(Message::DisableBlinkToggled(true)),
            Some(Change::DisableBlink(true))
        );
        assert_eq!(state.value().bold_mode, TerminalBoldMode::Bright);
        assert!(state.value().disable_blink);
        assert_eq!(
            state.update(Message::ThemeExtendedColorsToggled(true)),
            Some(Change::ThemeExtendedColors(true))
        );
        assert_eq!(
            state.update(Message::HidePaneHeadersToggled(true)),
            Some(Change::HidePaneHeaders(true))
        );
        assert!(state.value().theme_extended_colors);
        assert!(state.value().hide_pane_headers);
    }

    #[test]
    fn line_length_commits_only_a_valid_width_or_window_wrap() {
        let mut state = State::new(Appearance::default());
        assert!(
            state
                .update(Message::LineLengthChanged("19".into()))
                .is_none()
        );
        assert!(state.update(Message::LineLengthSubmitted).is_none());
        assert_eq!(state.value().line_length, None);
        assert!(
            state
                .update(Message::LineLengthChanged("80".into()))
                .is_none()
        );
        assert_eq!(
            state.update(Message::LineLengthSubmitted),
            Some(Change::LineLength(Some(80)))
        );
        assert!(
            state
                .update(Message::LineLengthChanged("".into()))
                .is_none()
        );
        assert_eq!(
            state.update(Message::LineLengthSubmitted),
            Some(Change::LineLength(None))
        );
        assert_eq!(state.value().line_length, None);
    }

    #[test]
    fn previous_browser_appearance_records_default_to_window_wrap_and_immediate_tooltips() {
        let previous = r#"{"font_size":18.0,"bold_mode":"bright","disable_blink":true}"#;
        let value: Appearance = serde_json::from_str(previous).unwrap();
        assert_eq!(value.line_length, None);
        assert_eq!(value.link_tooltip_delay_ms, 0);
        assert!(!value.theme_extended_colors);
        assert!(!value.hide_pane_headers);
        assert_eq!(value.scrollback_lines, DEFAULT_SCROLLBACK_LINES);
        assert_eq!(value.font_size, 18.0);
        assert_eq!(value.bold_mode, TerminalBoldMode::Bright);
        assert!(value.disable_blink);
    }

    #[test]
    fn tooltip_delay_commits_only_bounded_values() {
        let mut state = State::new(Appearance::default());
        assert!(
            state
                .update(Message::LinkTooltipDelayChanged("60001".into()))
                .is_none()
        );
        assert!(state.update(Message::LinkTooltipDelaySubmitted).is_none());
        assert_eq!(state.value().link_tooltip_delay_ms, 0);
        assert!(
            state
                .update(Message::LinkTooltipDelayChanged("60000".into()))
                .is_none()
        );
        assert_eq!(
            state.update(Message::LinkTooltipDelaySubmitted),
            Some(Change::LinkTooltipDelay(60_000))
        );
    }

    #[test]
    fn scrollback_limit_commits_only_after_a_valid_submission() {
        let mut state = State::new(Appearance::default());
        assert!(
            state
                .update(Message::ScrollbackLinesChanged("99".into()))
                .is_none()
        );
        assert!(state.update(Message::ScrollbackLinesSubmitted).is_none());
        assert_eq!(state.value().scrollback_lines, DEFAULT_SCROLLBACK_LINES);
        assert!(
            state
                .update(Message::ScrollbackLinesChanged("250".into()))
                .is_none()
        );
        assert_eq!(
            state.update(Message::ScrollbackLinesSubmitted),
            Some(Change::ScrollbackLines(250))
        );
        assert_eq!(state.value().scrollback_lines, 250);
    }
}
