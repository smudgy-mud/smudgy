
use iced::widget::{container, scrollable, text_editor};
use iced::{Background, Border, Color, Shadow, border};

pub mod markdown;
mod secondary;
mod smudgy;

pub use secondary::secondary;
pub use smudgy::smudgy;

pub mod builtins {
    pub mod button;
    pub mod checkbox;
    pub mod container;
    pub mod pane_grid;
    pub mod pick_list;
    pub mod progress_bar;
    pub mod radio;
    pub mod rule;
    pub mod slider;
    pub mod svg;
    pub mod text;
    pub mod text_input;
}

pub type Element<'a, Message> = iced::Element<'a, Message, Theme>;
pub struct Theme {
    pub name: String,
    pub styles: Styles,
}

impl iced::theme::Base for Theme {
    fn default(_preference: iced::theme::Mode) -> Self {
        smudgy::smudgy()
    }

    fn mode(&self) -> iced::theme::Mode {
        iced::theme::Mode::Dark
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base(&self) -> iced::theme::Style {
        iced::theme::Style {
            background_color: self.styles.general.background,
            text_color: self.styles.text.normal,
        }
    }

    fn palette(&self) -> Option<iced::theme::Palette> {
        Some(iced::theme::Palette {
            background: self.styles.general.background,
            text: self.styles.text.normal,
            primary: self.styles.buttons.primary.text,
            success: self.styles.text.success,
            warning: self.styles.text.error,
            danger: self.styles.text.error,
        })
    }
}

#[derive(Debug)]
pub struct Styles {
    pub buttons: Buttons,
    pub general: General,
    pub text: Text,
    pub modal: Modal,
}

#[derive(Debug, Clone)]
pub struct Modal {
    pub title_bar_background: Background,
    pub title_bar_border: Border,
    pub body_background: Background,
    pub body_border: Border,
    pub shadow: Shadow,
}
#[derive(Debug, Clone)]
pub struct Buttons {
    pub primary: Button,
    pub secondary: Button,
}

#[derive(Debug, Clone)]
pub struct Button {
    pub background: Background,
    pub background_hover: Background,
    pub background_pressed: Background,
    pub border: Border,
    pub text: Color,
}

#[derive(Debug, Clone)]
pub struct General {
    pub background: Color,
    pub container_background: Color,
    pub accent: Color,
    pub border: Color,
    pub rule: Color,
    pub overlay_background: Color,
    /// The command-input strip. Deliberately contrasts with `background`
    /// (the terminal behind it) — themes choose the pairing.
    pub input_background: Color,
    pub input_text: Color,
    /// A translucent highlight composited over a surface for a short gradient
    /// glow at the top of a view. White at low alpha; kept subtle so it reads as
    /// depth, not a band.
    pub top_highlight: Color,
}

#[derive(Debug, Clone)]
pub struct Text {
    pub normal: Color,
    pub success: Color,
    pub error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        smudgy::smudgy()
    }
}

impl scrollable::Catalog for Theme {
    type Class<'a> = ();

    fn default<'a>() -> Self::Class<'a> {}

    fn style(&self, _class: &Self::Class<'_>, _status: scrollable::Status) -> scrollable::Style {
        scrollable::Style {
            container: container::Style {
                ..Default::default()
            },
            gap: None,
            horizontal_rail: scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: scrollable::Scroller {
                    background: Background::Color(self.styles.general.accent),
                    border: Border::default(),
                },
            },
            vertical_rail: scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: scrollable::Scroller {
                    background: Background::Color(self.styles.general.accent),
                    border: Border::default(),
                },
            },
            auto_scroll: scrollable::AutoScroll {
                background: Background::Color(self.styles.general.overlay_background),
                border: Border::default(),
                shadow: Shadow::default(),
                icon: self.styles.text.normal,
            },
        }
    }
}

pub enum TextEditorClass {
    Default,
}

impl text_editor::Catalog for Theme {
    type Class<'a> = TextEditorClass;

    fn default<'a>() -> Self::Class<'a> {
        TextEditorClass::Default
    }

    fn style(&self, _class: &Self::Class<'_>, _status: text_editor::Status) -> text_editor::Style {
        text_editor::Style {
            background: Background::Color(self.styles.general.container_background),
            border: border::color(self.styles.general.border).width(1.0),
            placeholder: self.styles.text.normal.scale_alpha(0.4),
            value: self.styles.text.normal,
            selection: self.styles.general.accent,
        }
    }
}

// `markdown::Catalog` requires `table::Catalog` as a supertrait even when the
// rendered Markdown contains no tables, so both are implemented here. The
// Markdown widget powers the settings "Licenses" pane.
impl iced::widget::table::Catalog for Theme {
    type Class<'a> = iced::widget::table::StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(|theme: &Theme| iced::widget::table::Style {
            separator_x: theme.styles.general.border.into(),
            separator_y: theme.styles.general.border.into(),
        })
    }

    fn style(&self, class: &Self::Class<'_>) -> iced::widget::table::Style {
        class(self)
    }
}

impl iced::widget::markdown::Catalog for Theme {
    fn code_block<'a>() -> <Self as container::Catalog>::Class<'a> {
        Box::new(|theme: &Theme| container::Style {
            background: Some(theme.styles.general.container_background.into()),
            border: border::color(theme.styles.general.border)
                .width(1.0)
                .rounded(4.0),
            ..Default::default()
        })
    }
}
