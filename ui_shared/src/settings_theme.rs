//! Shared built-in theme selection.
//!
//! Hosts own persistence and may layer host-specific controls such as native's
//! per-theme tweak editor around this small reducer and view.

use iced::widget::{column, pick_list, text};
use smudgy_theme::{Element, builtins};

#[derive(Debug, Clone)]
pub enum Message {
    Selected(String),
}

pub struct State {
    selected: String,
}

impl State {
    #[must_use]
    pub fn new(selected: impl Into<String>) -> Self {
        Self {
            selected: selected.into(),
        }
    }

    #[must_use]
    pub fn selected(&self) -> &str {
        &self.selected
    }

    pub fn replace(&mut self, selected: impl Into<String>) {
        self.selected = selected.into();
    }

    pub fn update(&mut self, message: Message) -> Option<String> {
        let Message::Selected(selected) = message;
        if selected == self.selected {
            return None;
        }
        self.selected = selected.clone();
        Some(selected)
    }

    #[must_use]
    pub fn options(&self) -> Vec<String> {
        let mut options: Vec<_> = crate::prefs::palettes()
            .iter()
            .map(|palette| palette.name.to_owned())
            .collect();
        if !self.selected.is_empty() && !options.iter().any(|option| option == &self.selected) {
            options.push(self.selected.clone());
        }
        options
    }
}

pub fn view<'a>(state: &'a State, label: &impl Fn(&'static str) -> String) -> Element<'a, Message> {
    column![
        text(label("preferences-theme"))
            .size(11)
            .style(builtins::text::muted),
        pick_list(
            state.options(),
            Some(state.selected.clone()),
            Message::Selected,
        )
        .text_size(13)
        .width(280),
    ]
    .spacing(2)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_persisted_theme_remains_selectable() {
        let state = State::new("Future Theme");
        assert!(state.options().iter().any(|name| name == "Future Theme"));
    }

    #[test]
    fn reducer_emits_only_real_changes() {
        let mut state = State::new("Smudgy");
        assert!(state.update(Message::Selected("Smudgy".into())).is_none());
        assert_eq!(
            state.update(Message::Selected("Dracula".into())),
            Some("Dracula".into())
        );
    }
}
