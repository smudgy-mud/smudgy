use iced::{
    Color,
    widget::progress_bar::{self, Catalog, Style, StyleFn},
};

use crate::Theme;

impl Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>) -> progress_bar::Style {
        class(self)
    }
}

#[must_use]
pub fn default(theme: &Theme) -> progress_bar::Style {
    Style {
        background: theme.styles.general.background.into(),
        bar: Color::WHITE.into(),
        border: iced::Border::default(),
    }
}
