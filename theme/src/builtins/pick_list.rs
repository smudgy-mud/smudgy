use crate::Theme;
use iced::overlay::menu;
use iced::widget::pick_list;
use iced::{Background, Border, Shadow, border};

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, pick_list::Status) -> pick_list::Style + 'a>;

impl pick_list::Catalog for Theme {
    type Class<'a> = StyleFn<'a, Theme>;

    fn default<'a>() -> <Self as pick_list::Catalog>::Class<'a> {
        Box::new(default)
    }

    fn style(
        &self,
        class: &<Self as pick_list::Catalog>::Class<'_>,
        status: pick_list::Status,
    ) -> pick_list::Style {
        class(self, status)
    }
}

impl menu::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> <Self as menu::Catalog>::Class<'a> {}

    fn style(&self, (): &<Self as menu::Catalog>::Class<'_>) -> menu::Style {
        menu::Style {
            background: Background::Color(self.styles.general.container_background),
            border: border::color(self.styles.general.border).width(1.0),
            text_color: self.styles.text.normal,
            selected_text_color: self.styles.text.normal,
            selected_background: Background::Color(self.styles.general.accent),
            shadow: Shadow::default(),
        }
    }
}

#[must_use]
pub fn default(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let border_color = match status {
        pick_list::Status::Active => theme.styles.general.border,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => {
            theme.styles.general.accent
        }
    };

    pick_list::Style {
        text_color: theme.styles.text.normal,
        placeholder_color: theme.styles.text.normal.scale_alpha(0.4),
        handle_color: theme.styles.text.normal,
        background: Background::Color(theme.styles.general.container_background),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 2.0.into(),
        },
    }
}
