//! A preset picker shown beside the individual permissions it sets.
//!
//! Choosing a preset checks the facet's actions it holds and unchecks the
//! rest, which still change one at a time; a set that no preset names reads
//! as Custom (clans.md §5.2). Each editor draws its own switches or
//! checkboxes and makes a choice by toggling them ([`PresetPick::changes`]),
//! so a picker never bypasses the rules a single toggle follows.

use iced::widget::{pick_list, row, text};
use iced::{Alignment, Length};

use crate::presets::{self, Facet, FacetState, PresetPick};
use crate::theme::Element as ThemedElement;

/// What a facet's picker shows and offers.
#[derive(Debug, Clone)]
pub struct Picker {
    pub facet: Facet,
    pub state: FacetState,
    /// The choices the viewer may make here.
    pub choices: Vec<PresetPick>,
}

impl Picker {
    /// The picker for `facet` over the `checked` actions, offering the
    /// presets `offered` keeps and, when `offered` keeps it, none of them.
    #[must_use]
    pub fn new<'c>(
        facet: Facet,
        checked: impl IntoIterator<Item = &'c str>,
        presets: impl IntoIterator<Item = presets::Preset>,
        offered: impl Fn(PresetPick) -> bool,
    ) -> Self {
        let choices = std::iter::once(PresetPick::None(facet))
            .chain(presets.into_iter().map(PresetPick::Preset))
            .filter(|pick| offered(*pick))
            .collect();
        Self {
            facet,
            state: facet.state(checked),
            choices,
        }
    }

    /// The choice the picker shows as made: `None` for Custom.
    #[must_use]
    pub fn selected(&self) -> Option<PresetPick> {
        match self.state {
            FacetState::Empty => Some(PresetPick::None(self.facet)),
            FacetState::Preset(preset) => Some(PresetPick::Preset(preset)),
            FacetState::Custom => None,
        }
    }

    /// The picker labelled `label`; `on_pick` is `None` while it may not be
    /// used, and then it shows its value as text.
    pub fn view<'a, Message: Clone + 'a>(
        &self,
        label: String,
        on_pick: Option<impl Fn(PresetPick) -> Message + 'a>,
    ) -> ThemedElement<'a, Message> {
        let selected = self.selected();
        let shown = selected.map_or_else(presets::custom_label, |pick| pick.to_string());
        let control: ThemedElement<'a, Message> = match on_pick {
            Some(on_pick) if self.choices.len() > 1 => {
                pick_list(self.choices.clone(), selected, on_pick)
                    .placeholder(presets::custom_label())
                    .text_size(13)
                    .width(Length::Fixed(170.0))
                    .into()
            }
            _ => text(shown).size(13).into(),
        };
        row![text(label).size(13).width(Length::Fill), control]
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
    }
}
