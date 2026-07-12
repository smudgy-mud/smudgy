use iced::widget::text;

use crate::Theme;

impl text::Catalog for Theme {
    type Class<'a> = Box<dyn Fn(&Self) -> text::Style + 'a>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|_theme: &Self| text::Style { color: None })
    }

    fn style(&self, class: &Self::Class<'_>) -> text::Style {
        class(self)
    }
}

#[must_use]
pub fn danger(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.error),
    }
}

/// Muted secondary ink (~40% opacity of the normal text color). Used for field
/// labels, helper/subtext lines, and other secondary copy per the connect/
/// onboarding visual language (deep-purple dark theme, single accent).
#[must_use]
pub fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.4)),
    }
}

#[must_use]
pub fn success(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.success),
    }
}
