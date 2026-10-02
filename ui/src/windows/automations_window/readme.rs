//! Shared reading surface for package documentation.

use iced::advanced::text::Wrapping;
use iced::widget::{container, markdown, responsive, rich_text};
use iced::{Background, Border, Color, Length};

use crate::assets::fonts;
use crate::theme::Theme;

use super::{Elem, Message};

/// Scrolling belongs to the caller so package pages keep a single scrollbar.
pub(super) fn view(content: &markdown::Content) -> Elem<'_> {
    let mut style = markdown::Style::from_palette(iced::theme::Palette::DARK);
    style.font = fonts::GEIST_VF;
    style.inline_code_font = fonts::GEIST_MONO_VF;
    style.code_block_font = fonts::GEIST_MONO_VF;
    style.link_color = Color::from_rgb8(190, 172, 255);

    let mut settings = markdown::Settings::with_text_size(16.0, style);
    settings.h1_size = 28.0.into();
    settings.h2_size = 23.0.into();
    settings.h3_size = 19.0.into();
    settings.h4_size = 17.0.into();
    settings.code_size = 14.0.into();
    settings.spacing = 16.0.into();

    container(
        responsive(move |size| {
            let padding = if size.width < 480.0 { 16.0 } else { 28.0 };
            container(
                markdown::view_with(content.items(), settings, &ReadmeViewer)
                    .map(Message::OpenReadmeLink),
            )
            .padding(padding)
            .width(Length::Fill)
            .style(surface_style)
            .into()
        })
        .height(Length::Shrink),
    )
    .width(Length::Fill)
    .max_width(792.0)
    .into()
}

fn surface_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(
            theme.styles.text.normal.scale_alpha(0.035),
        )),
        text_color: Some(theme.styles.text.normal.scale_alpha(0.9)),
        border: Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

struct ReadmeViewer;

impl<'a> markdown::Viewer<'a, markdown::Uri, Theme> for ReadmeViewer {
    fn on_link_click(uri: markdown::Uri) -> markdown::Uri {
        uri
    }

    fn paragraph(
        &self,
        settings: markdown::Settings,
        text: &markdown::Text,
    ) -> iced::Element<'a, markdown::Uri, Theme> {
        let spans = text
            .spans(settings.style)
            .iter()
            .cloned()
            .map(|mut span| {
                if span.link.is_some() {
                    span.underline = true;
                }
                span
            })
            .collect::<Vec<_>>();

        // Iced also uses these paragraphs to measure table cells, so keep their intrinsic width.
        rich_text(spans)
            .size(settings.text_size)
            .line_height(1.5)
            .wrapping(Wrapping::WordOrGlyph)
            .on_link_click(Self::on_link_click)
            .into()
    }

    fn heading(
        &self,
        settings: markdown::Settings,
        level: &'a markdown::HeadingLevel,
        text: &'a markdown::Text,
        index: usize,
    ) -> iced::Element<'a, markdown::Uri, Theme> {
        container(markdown::heading(
            settings,
            level,
            text,
            index,
            Self::on_link_click,
        ))
        .padding(iced::padding::top(if index > 0 { 8.0 } else { 0.0 }))
        .into()
    }

    fn table(
        &self,
        settings: markdown::Settings,
        columns: &'a [markdown::Column],
        rows: &'a [markdown::Row],
    ) -> iced::Element<'a, markdown::Uri, Theme> {
        responsive(move |size| {
            let column_count = f32::from(u16::try_from(columns.len()).unwrap_or(u16::MAX).max(1));
            let max_width = (size.width / column_count - 2.0 * settings.spacing.0).max(144.0);
            markdown::table(&TableCellViewer { max_width }, settings, columns, rows)
        })
        .height(Length::Shrink)
        .into()
    }
}

struct TableCellViewer {
    max_width: f32,
}

impl<'a> markdown::Viewer<'a, markdown::Uri, Theme> for TableCellViewer {
    fn on_link_click(uri: markdown::Uri) -> markdown::Uri {
        uri
    }

    fn paragraph(
        &self,
        settings: markdown::Settings,
        text: &markdown::Text,
    ) -> iced::Element<'a, markdown::Uri, Theme> {
        container(markdown::Viewer::paragraph(&ReadmeViewer, settings, text))
            .max_width(self.max_width)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Size;
    use iced::advanced::layout::{Limits, Node};
    use iced::advanced::renderer::Headless;
    use iced::advanced::widget::Tree;

    fn cell_grid(node: &Node, cells: usize) -> Option<&Node> {
        if node.children().len() == cells {
            Some(node)
        } else {
            node.children()
                .iter()
                .find_map(|child| cell_grid(child, cells))
        }
    }

    #[tokio::test]
    async fn markdown_tables_keep_readable_cells_in_narrow_and_wide_panes() {
        {
            let mut font_system = iced_graphics::text::font_system().write().unwrap();
            font_system.load_font(fonts::GEIST_VF_BYTES.into());
            font_system.load_font(fonts::GEIST_MONO_VF_BYTES.into());
        }
        let renderer =
            <iced::Renderer as Headless>::new(fonts::GEIST_VF, 16.0.into(), Some("tiny-skia"))
                .await
                .expect("software renderer must support headless layout");

        let tables = [
            (
                "| Command | Behavior |\n|---|---|\n| `map off` | Disabled |\n| `map follow` | Tracks the current position without changing rooms |",
                6,
            ),
            (
                "| Setting | Value | Effect |\n|---|---|---|\n| Mode | Follow | Tracks the current position |\n| Text size | Medium | Controls headings, notes, and button labels |",
                9,
            ),
        ];

        for (source, cells) in tables {
            let content = markdown::Content::parse(source);
            for width in [320.0, 792.0] {
                let mut element = view(&content);
                let mut tree = Tree::new(element.as_widget());
                let layout = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &Limits::new(Size::ZERO, Size::new(width, f32::INFINITY)),
                );
                let grid = cell_grid(&layout, cells).expect("table cell layout");
                let columns = f32::from(u16::try_from(cells / 3).unwrap());
                assert!(
                    grid.bounds().height < 240.0,
                    "{width}px: {:?}",
                    grid.bounds()
                );
                for cell in grid.children() {
                    let bounds = cell.bounds();
                    assert!(bounds.width <= (width / columns).max(144.0), "{bounds:?}");
                    assert!(bounds.x.is_finite() && bounds.y.is_finite(), "{bounds:?}");
                    assert!(
                        bounds.width.is_finite() && bounds.width >= 24.0,
                        "{bounds:?}"
                    );
                    assert!(
                        bounds.height.is_finite() && bounds.height < 100.0,
                        "{bounds:?}"
                    );
                }
            }
        }
    }
}
