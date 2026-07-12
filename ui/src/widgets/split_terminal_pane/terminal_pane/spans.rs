use std::{
    borrow::Cow,
    cmp::{max, min},
    rc::Rc,
};

use iced::widget::text::Span;
use crate::terminal_buffer::selection::LineSelection;

#[derive(Debug, Clone)]
pub struct Spans<Link: Clone> {
    spans: Rc<Vec<Span<'static, Link>>>,
    selected: Vec<usize>,
    spans_with_selection: Option<Rc<Vec<Span<'static, Link>>>>,
}

impl<Link: Clone> Spans<Link> {
    pub fn with_selection(spans: Rc<Vec<Span<'static, Link>>>, selection: LineSelection) -> Self {
        match selection {
            None => Self {
                spans,
                selected: Vec::new(),
                spans_with_selection: None,
            },
            Some((0, usize::MAX)) => {
                let mut spans = Self {
                    spans,
                    selected: Vec::new(),
                    spans_with_selection: None,
                };
                spans.select_all();
                spans
            }
            Some((from, to)) => {
                let mut spans = Self {
                    spans,
                    selected: Vec::new(),
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

    pub fn select_none(&mut self) {
        self.selected.clear();
        self.spans_with_selection = None;
    }

    pub fn select_all(&mut self) {
        self.selected = (0..self.spans.len()).collect();
        self.spans_with_selection = None;
    }

    pub fn select_range(&mut self, sel_start: usize, sel_end: usize) {
        let mut char_position = 0; // Track character position across spans

        self.selected.clear();

        self.spans_with_selection = Some(Rc::new(
            self.spans
                .iter()
                .flat_map(|span| {
                    // Convert span text to character indices for safe slicing
                    let span_chars: Vec<char> = span.text.chars().collect();
                    let span_char_len = span_chars.len();
                    let span_char_end = char_position + span_char_len;

                    let mut spans = Vec::with_capacity(3);

                    // Part before selection
                    if sel_start > char_position {
                        let unselected_end = min(sel_start, span_char_end) - char_position;
                        if unselected_end > 0 {
                            let text: String = span_chars[0..unselected_end].iter().collect();
                            spans.push((
                                false,
                                Span {
                                    text: Cow::Owned(text),
                                    link: span.link.clone(),
                                    ..*span
                                },
                            ));
                        }
                    }

                    // Selected part
                    if sel_start < span_char_end && sel_end > char_position {
                        let selected_start = max(sel_start, char_position) - char_position;
                        let selected_end = min(sel_end, span_char_end) - char_position;

                        if selected_end > selected_start {
                            let text: String =
                                span_chars[selected_start..selected_end].iter().collect();
                            spans.push((
                                true,
                                Span {
                                    text: Cow::Owned(text),
                                    link: span.link.clone(),
                                    ..*span
                                },
                            ));
                        }
                    }

                    // Part after selection
                    if sel_end < span_char_end {
                        let unselected_start = max(sel_end, char_position) - char_position;
                        if unselected_start < span_char_len {
                            let text: String = span_chars[unselected_start..].iter().collect();
                            spans.push((
                                false,
                                Span {
                                    text: Cow::Owned(text),
                                    link: span.link.clone(),
                                    ..*span
                                },
                            ));
                        }
                    }

                    char_position = span_char_end;
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
