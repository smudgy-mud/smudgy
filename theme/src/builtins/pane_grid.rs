use crate::Theme;
use iced::widget::pane_grid;
use iced::{Background, Border};

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> pane_grid::Style + 'a>;

impl pane_grid::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> <Self as pane_grid::Catalog>::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &<Self as pane_grid::Catalog>::Class<'_>) -> pane_grid::Style {
        class(self)
    }
}

#[must_use]
pub fn default(theme: &Theme) -> pane_grid::Style {
    pane_grid::Style {
        hovered_region: pane_grid::Highlight {
            background: Background::Color(
                theme.styles.general.accent.scale_alpha(0.3),
            ),
            border: Border {
                width: 2.0,
                color: theme.styles.general.accent,
                radius: 0.0.into(),
            },
        },
        hovered_split: pane_grid::Line {
            color: theme.styles.general.accent,
            width: 2.0,
        },
        picked_split: pane_grid::Line {
            color: theme.styles.general.accent,
            width: 2.0,
        },
    }
}
