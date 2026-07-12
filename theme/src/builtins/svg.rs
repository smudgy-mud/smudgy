use iced::widget::svg;

use crate::Theme;

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, svg::Status) -> svg::Style + 'a>;

impl svg::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>, status: svg::Status) -> svg::Style {
        class(self, status)
    }
}

fn default(theme: &Theme, _status: svg::Status) -> svg::Style {
    svg::Style {
        color: Some(theme.styles.text.normal),
    }
}
