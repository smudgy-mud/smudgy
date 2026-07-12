use iced::{
    border::radius,
    widget::rule::{self, Catalog},
};

use crate::Theme;

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> rule::Style + 'a>;

impl Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>) -> rule::Style {
        class(self)
    }
}

#[must_use]
pub fn default(theme: &Theme) -> rule::Style {
    rule::Style {
        color: theme.styles.general.rule,
        radius: radius(0.0),
        fill_mode: rule::FillMode::Full,
        snap: false,
    }
}
