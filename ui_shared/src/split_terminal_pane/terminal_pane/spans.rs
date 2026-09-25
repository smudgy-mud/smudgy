use std::{borrow::Cow, cmp::min, rc::Rc};

use crate::terminal_buffer::selection::LineSelection;
use iced::widget::text::Span;

#[derive(Debug, Clone)]
pub struct Spans<Link: Clone> {
    spans: Rc<Vec<Span<'static, Link>>>,
    selected: Vec<usize>,
    selection_color: Option<iced::Color>,
    spans_with_selection: Option<Rc<Vec<Span<'static, Link>>>>,
}

impl<Link: Clone> Spans<Link> {
    pub fn with_selection_color(
        spans: Rc<Vec<Span<'static, Link>>>,
        selection: LineSelection,
        selection_color: Option<iced::Color>,
    ) -> Self {
        match selection {
            None => Self {
                spans,
                selected: Vec::new(),
                selection_color,
                spans_with_selection: None,
            },
            Some((0, usize::MAX)) => {
                let mut spans = Self {
                    spans,
                    selected: Vec::new(),
                    selection_color,
                    spans_with_selection: None,
                };
                spans.select_all();
                spans
            }
            Some((from, to)) => {
                let mut spans = Self {
                    spans,
                    selected: Vec::new(),
                    selection_color,
                    spans_with_selection: None,
                };
                spans.select_range(from, to);
                spans
            }
        }
    }

    pub fn spans(&self) -> Rc<Vec<Span<'static, Link>>> {
        self.spans_with_selection
            .as_ref()
            .map(|spans| spans.clone())
            .unwrap_or_else(|| self.spans.clone())
    }

    #[cfg(test)]
    pub fn select_none(&mut self) {
        self.selected.clear();
        self.spans_with_selection = None;
    }

    pub fn select_all(&mut self) {
        self.selected = (0..self.spans.len()).collect();
        self.spans_with_selection = self.selection_color.map(|color| {
            Rc::new(
                self.spans
                    .iter()
                    .map(|span| Span {
                        text: Cow::Owned(span.text.to_string()),
                        color: Some(color),
                        link: span.link.clone(),
                        ..*span
                    })
                    .collect(),
            )
        });
    }

    pub fn select_range(&mut self, sel_start: usize, sel_end: usize) {
        let mut byte_position = 0;

        self.selected.clear();

        self.spans_with_selection = Some(Rc::new(
            self.spans
                .iter()
                .flat_map(|span| {
                    let span_text = span.text.as_ref();
                    let span_byte_end = byte_position + span_text.len();
                    let boundary = |mut offset: usize| {
                        offset = offset.min(span_text.len());
                        while offset > 0 && !span_text.is_char_boundary(offset) {
                            offset -= 1;
                        }
                        offset
                    };

                    let mut spans = Vec::with_capacity(3);

                    // Part before selection
                    if sel_start > byte_position {
                        let unselected_end =
                            boundary(min(sel_start, span_byte_end).saturating_sub(byte_position));
                        if unselected_end > 0 {
                            spans.push((
                                false,
                                Span {
                                    text: Cow::Owned(span_text[..unselected_end].to_string()),
                                    link: span.link.clone(),
                                    ..*span
                                },
                            ));
                        }
                    }

                    // Selected part
                    if sel_start < span_byte_end && sel_end > byte_position {
                        let selected_start = boundary(sel_start.saturating_sub(byte_position));
                        let selected_end =
                            boundary(min(sel_end, span_byte_end).saturating_sub(byte_position));

                        if selected_end > selected_start {
                            spans.push((
                                true,
                                Span {
                                    text: Cow::Owned(
                                        span_text[selected_start..selected_end].to_string(),
                                    ),
                                    color: self.selection_color.or(span.color),
                                    link: span.link.clone(),
                                    ..*span
                                },
                            ));
                        }
                    }

                    // Part after selection
                    if sel_end < span_byte_end {
                        let unselected_start = boundary(sel_end.saturating_sub(byte_position));
                        if unselected_start < span_text.len() {
                            spans.push((
                                false,
                                Span {
                                    text: Cow::Owned(span_text[unselected_start..].to_string()),
                                    link: span.link.clone(),
                                    ..*span
                                },
                            ));
                        }
                    }

                    byte_position = span_byte_end;
                    spans
                })
                .enumerate()
                .map(|(i, (selected, span))| {
                    if selected {
                        self.selected.push(i);
                    }
                    span
                })
                .collect(),
        ));
    }

    pub fn selected(&self) -> &[usize] {
        &self.selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_ranges_are_utf8_byte_offsets() {
        let mut spans = Spans::with_selection_color(
            Rc::new(vec![Span::<'static, ()>::new(Cow::Borrowed("A🗝️B"))]),
            Some((1, 8)),
            Some(iced::Color::WHITE),
        );
        let rendered = spans.spans();

        assert_eq!(rendered.len(), 3);
        assert_eq!(rendered[0].text, "A");
        assert_eq!(rendered[1].text, "🗝️");
        assert_eq!(rendered[2].text, "B");
        assert_eq!(rendered[1].color, Some(iced::Color::WHITE));
        assert_eq!(spans.selected(), &[1]);

        spans.select_none();
        assert!(spans.selected().is_empty());

        spans.select_all();
        assert!(
            spans
                .spans()
                .iter()
                .all(|span| span.color == Some(iced::Color::WHITE))
        );
    }
}
