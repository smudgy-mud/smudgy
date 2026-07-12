use std::sync::Arc;

use super::connection::vt_processor;

pub use vt_processor::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: vt_processor::Color,
    pub bg: vt_processor::Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtSpan {
    pub style: Style,
    pub begin_pos: usize,
    pub end_pos: usize,
}

/// What a click on a linked range of a line does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAction {
    /// Send this command on the clicked pane's session, as if typed (alias
    /// processing and command splitting apply). Serialized into the line, so
    /// it works for as long as the line is on screen.
    Send(Arc<str>),
    /// Run a script callback in the engine that created the fragment. The
    /// line carries only this address — the function itself stays in its
    /// isolate's registry, so a click after that engine is gone is a no-op.
    Callback {
        /// The session whose engine holds the callback (fragments can be
        /// echoed into another session's pane).
        session: super::SessionId,
        /// The creating isolate instantiation, in widget-routing-token form
        /// (`IsolateId::to_widget_token`).
        isolate_token: Arc<str>,
        /// The slot in that instantiation's link-callback registry (`u64` so the
        /// monotonic ids can never wrap into aliasing another callback).
        id: u64,
    },
}

/// One clickable byte range of a line. Kept in a list parallel to the style
/// spans (not on [`VtSpan`]) so the hot ingest path and the span-surgery code
/// stay link-free; link ranges may cross style-span boundaries and vice versa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub begin_pos: usize,
    pub end_pos: usize,
    pub action: LinkAction,
}

#[derive(Debug, Clone, Eq)]
pub struct StyledLine {
    pub text: String,
    pub spans: Vec<VtSpan>,
    /// Clickable ranges, sorted and non-overlapping (usually empty — an empty
    /// vec does not allocate). Unlike `spans`, these need not cover the text.
    pub links: Vec<LinkSpan>,
    raw: Option<String>,
}

impl PartialEq for StyledLine {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl StyledLine {
    #[must_use]
    pub fn new(text: &str, span_info: Vec<VtSpan>) -> Self {
        Self {
            text: String::from(text),
            spans: span_info,
            links: Vec::new(),
            raw: None,
        }
    }

    #[must_use]
    pub fn new_with_raw(text: &str, span_info: Vec<VtSpan>, raw: &[u8]) -> Self {
        Self {
            text: String::from(text),
            spans: span_info,
            links: Vec::new(),
            raw: Some(String::from_utf8_lossy(raw).into_owned()),
        }
    }

    #[must_use]
    pub fn append(&self, other_line: &StyledLine) -> Self {
        Self {
            text: format!("{}{}", self.text, other_line.text),
            spans: self
                .spans
                .clone()
                .into_iter()
                .chain(other_line.spans.iter().map(|span| VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos + self.text.len(),
                    end_pos: span.end_pos + self.text.len(),
                }))
                .collect(),
            links: self
                .links
                .iter()
                .cloned()
                .chain(other_line.links.iter().map(|link| LinkSpan {
                    begin_pos: link.begin_pos + self.text.len(),
                    end_pos: link.end_pos + self.text.len(),
                    action: link.action.clone(),
                }))
                .collect(),
            raw: match self.raw {
                Some(ref raw) => {
                    let mut combined = raw.clone();
                    match other_line.raw {
                        Some(ref other_raw) => {
                            combined.push_str(other_raw);
                            Some(combined)
                        }
                        None => Some(combined),
                    }
                }
                None => other_line.raw.clone(),
            },
        }
    }

    /// Re-map the link spans across a splice that replaces `text[begin..end]` with
    /// `insert_len` new (link-free) bytes: the piece of a link before the replaced
    /// region survives in place, the piece after it shifts by the length delta, and
    /// bytes overlapping the region drop their link — the same interval rules as the
    /// style-span remap inside [`Self::insert`] (kept in lockstep; only the
    /// no-re-cover rule differs), so `begin`/`end` must arrive with the same clamping
    /// `insert`/`remove` apply.
    fn remap_links(&self, begin: usize, end: usize, insert_len: usize) -> Vec<LinkSpan> {
        if self.links.is_empty() {
            return Vec::new();
        }
        let shift = insert_len as i64 - (end - begin) as i64;
        let mut links = Vec::with_capacity(self.links.len());
        for link in &self.links {
            if link.begin_pos < begin {
                let clipped_end = link.end_pos.min(begin);
                if clipped_end > link.begin_pos {
                    links.push(LinkSpan {
                        begin_pos: link.begin_pos,
                        end_pos: clipped_end,
                        action: link.action.clone(),
                    });
                }
            }
            if link.end_pos > end {
                let after_begin = link.begin_pos.max(end);
                let begin_pos = ((after_begin as i64) + shift).max(0) as usize;
                let end_pos = ((link.end_pos as i64) + shift).max(0) as usize;
                if end_pos > begin_pos {
                    links.push(LinkSpan {
                        begin_pos,
                        end_pos,
                        action: link.action.clone(),
                    });
                }
            }
        }
        links
    }

    #[must_use]
    pub fn insert(&self, str: &str, begin: usize, end: usize, style: Style) -> Self {
        // Clamp bounds to text length
        let begin = begin.min(self.text.len());
        let end = end.min(self.text.len().max(begin));

        // Create new text by inserting the string
        let mut new_text = String::new();
        new_text.push_str(&self.text[..begin]);
        new_text.push_str(str);
        new_text.push_str(&self.text[end..]);

        let insert_len = str.len();
        let removed_len = end - begin;
        let shift = insert_len as i32 - removed_len as i32;

        let mut new_spans = Vec::new();

        // Re-map each existing span across the splice that replaces `text[begin..end]`
        // (length `removed_len`) with `str` (length `insert_len`, shifting everything past
        // `end` by `shift`). A span contributes at most two pieces: the part strictly
        // before `begin`, kept in place, and the part strictly after `end`, shifted right
        // by `shift`. Bytes overlapping the replaced region are dropped — the inserted-text
        // span below covers them. The result is non-overlapping and gap-free, which the
        // renderer relies on: it tiles the on-screen line by slicing `text[begin..end]` per
        // span, so overlapping spans would duplicate text (and overrun byte offsets on copy).
        for span in &self.spans {
            // Portion of the span before the replaced region, unchanged.
            if span.begin_pos < begin {
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos,
                    end_pos: span.end_pos.min(begin),
                });
            }
            // Portion of the span after the replaced region, shifted by the length delta.
            // `after_begin` maps to `begin + insert_len` for a span that spans the region,
            // sitting flush against the inserted-text span below.
            if span.end_pos > end {
                let after_begin = span.begin_pos.max(end);
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: ((after_begin as i32) + shift).max(0) as usize,
                    end_pos: ((span.end_pos as i32) + shift).max(0) as usize,
                });
            }
        }

        // Add span for the inserted text if it's not empty
        if !str.is_empty() {
            new_spans.push(VtSpan {
                style,
                begin_pos: begin,
                end_pos: begin + insert_len,
            });
        }

        // Sort spans by begin position
        new_spans.sort_by_key(|span| span.begin_pos);

        Self {
            text: new_text,
            spans: new_spans,
            links: self.remap_links(begin, end, insert_len),
            raw: self.raw.clone(),
        }
    }

    #[must_use]
    pub fn highlight(&self, begin: usize, end: usize, style: Style) -> Self {
        // Clamp bounds to text length
        let begin = begin.min(self.text.len());
        let end = end.min(self.text.len().max(begin));

        // If range is empty, return unchanged
        if begin >= end {
            return self.clone();
        }

        let mut new_spans = Vec::new();

        // We want to keep spans that are completely outside the range of the new style,
        // and shrink any spans that have partial overlap with the new style.
        // Any spans that are completely inside the new style are replaced with a single span.
        for span in &self.spans {
            if span.end_pos <= begin {
                // Span is completely before highlight range
                new_spans.push(*span);
            } else if span.begin_pos >= end {
                // Span is completely after highlight range
                new_spans.push(*span);
            } else if span.begin_pos < begin && span.end_pos > begin && span.end_pos <= end {
                // Span starts before and ends within highlight range - keep the part before
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos,
                    end_pos: begin,
                });
            } else if span.begin_pos >= begin && span.begin_pos < end && span.end_pos > end {
                // Span starts within and ends after highlight range - keep the part after
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: end,
                    end_pos: span.end_pos,
                });
            } else if span.begin_pos < begin && span.end_pos > end {
                // Span completely encompasses highlight range - split into before and after
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: span.begin_pos,
                    end_pos: begin,
                });
                new_spans.push(VtSpan {
                    style: span.style,
                    begin_pos: end,
                    end_pos: span.end_pos,
                });
            }
            // Case where span is completely within highlight range:
            // do nothing (gets replaced by highlight span)
        }

        // Add the highlight span
        new_spans.push(VtSpan {
            style,
            begin_pos: begin,
            end_pos: end,
        });

        // Sort spans by begin position to maintain order
        new_spans.sort_by_key(|span| span.begin_pos);

        Self {
            text: self.text.clone(),
            spans: new_spans,
            // A recolor moves no bytes, so the link ranges are untouched.
            links: self.links.clone(),
            raw: self.raw.clone(),
        }
    }

    #[must_use]
    pub fn remove(&self, begin: usize, end: usize) -> Self {
        let text = self.text.as_str();
        let begin = begin.min(text.len());
        let end = end.min(text.len().max(begin));

        let shift = end - begin;

        let new_spans = self
            .spans
            .iter()
            .filter_map(|span| {
                if span.begin_pos >= begin && span.end_pos <= end {
                    // Span is completely within removal range - remove it
                    None
                } else if span.begin_pos >= end {
                    // Span is completely after removal range - shift it left
                    Some(VtSpan {
                        begin_pos: span.begin_pos - shift,
                        end_pos: span.end_pos - shift,
                        style: span.style,
                    })
                } else if span.end_pos <= begin {
                    // Span is completely before removal range - keep it unchanged
                    Some(*span)
                } else if span.begin_pos < begin && span.end_pos > end {
                    // Span encompasses removal range - shrink it
                    Some(VtSpan {
                        begin_pos: span.begin_pos,
                        end_pos: span.end_pos - shift,
                        style: span.style,
                    })
                } else if span.begin_pos < begin && span.end_pos > begin {
                    // Span starts before and ends within removal range - truncate to before part
                    Some(VtSpan {
                        begin_pos: span.begin_pos,
                        end_pos: begin,
                        style: span.style,
                    })
                } else if span.begin_pos < end && span.end_pos > end {
                    // Span starts within and ends after removal range - keep the after part, shifted
                    Some(VtSpan {
                        begin_pos: begin,
                        end_pos: span.end_pos - shift,
                        style: span.style,
                    })
                } else {
                    // Should not reach here, but keep the span as fallback
                    Some(*span)
                }
            })
            .collect();

        Self {
            text: text[..begin].to_string() + &text[end..],
            spans: new_spans,
            links: self.remap_links(begin, end, 0),
            raw: self.raw.clone(),
        }
    }

    /// Build a line from styled runs: each run contributes its text with its style and
    /// optional link, in order. Spans are laid down flush against each other (adjacent
    /// same-style runs merge; adjacent same-action link runs merge), so the result tiles
    /// the text non-overlapping and gap-free by construction — the invariant the
    /// renderer relies on. Empty runs contribute nothing; an empty run set yields the
    /// same single empty span an empty echo produces.
    #[must_use]
    pub fn from_styled_runs(
        runs: &[(&str, Style, Option<LinkAction>)],
        empty_style: Style,
    ) -> Self {
        let mut text = String::with_capacity(runs.iter().map(|(t, _, _)| t.len()).sum());
        let mut spans: Vec<VtSpan> = Vec::with_capacity(runs.len());
        let mut links: Vec<LinkSpan> = Vec::new();
        for (run_text, style, link) in runs {
            if run_text.is_empty() {
                continue;
            }
            let begin = text.len();
            text.push_str(run_text);
            match spans.last_mut() {
                Some(prev) if prev.style == *style && prev.end_pos == begin => {
                    prev.end_pos = text.len();
                }
                _ => spans.push(VtSpan {
                    style: *style,
                    begin_pos: begin,
                    end_pos: text.len(),
                }),
            }
            if let Some(action) = link {
                match links.last_mut() {
                    Some(prev) if prev.action == *action && prev.end_pos == begin => {
                        prev.end_pos = text.len();
                    }
                    _ => links.push(LinkSpan {
                        begin_pos: begin,
                        end_pos: text.len(),
                        action: action.clone(),
                    }),
                }
            }
        }
        if spans.is_empty() {
            spans.push(VtSpan {
                style: empty_style,
                begin_pos: 0,
                end_pos: 0,
            });
        }
        Self {
            text,
            spans,
            links,
            raw: None,
        }
    }

    #[must_use]
    pub fn from_echo_str(text: &str) -> Self {
        Self {
            spans: vec![VtSpan {
                begin_pos: 0,
                end_pos: text.len(),
                style: Style {
                    fg: { Color::Echo },
                    bg: { Color::DefaultBackground },
                },
            }],
            text: String::from(text),
            links: Vec::new(),
            raw: None,
        }
    }

    #[must_use]
    pub fn from_warn_str(text: &str) -> Self {
        Self {
            spans: vec![VtSpan {
                begin_pos: 0,
                end_pos: text.len(),
                style: Style {
                    fg: { Color::Warn },
                    bg: { Color::DefaultBackground },
                },
            }],
            text: String::from(text),
            links: Vec::new(),
            raw: None,
        }
    }

    #[must_use]
    pub fn from_output_str(text: &str) -> Self {
        Self {
            spans: vec![VtSpan {
                begin_pos: 0,
                end_pos: text.len(),
                style: Style {
                    fg: { Color::Output },
                    bg: { Color::DefaultBackground },
                },
            }],
            text: String::from(text),
            links: Vec::new(),
            raw: None,
        }
    }

    #[must_use]
    pub fn raw(&self) -> Option<&str> {
        self.raw.as_deref()
    }
}

impl std::ops::Deref for StyledLine {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.text.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::connection::vt_processor::AnsiColor;

    fn create_test_style(fg_color: AnsiColor, bold: bool) -> Style {
        Style {
            fg: Color::Ansi {
                color: fg_color,
                bold,
            },
            bg: Color::DefaultBackground,
        }
    }

    fn create_test_line() -> StyledLine {
        StyledLine::new(
            "Hello World Test",
            vec![
                VtSpan {
                    style: create_test_style(AnsiColor::Red, false),
                    begin_pos: 0,
                    end_pos: 5, // "Hello"
                },
                VtSpan {
                    style: create_test_style(AnsiColor::Green, false),
                    begin_pos: 6,
                    end_pos: 11, // "World"
                },
                VtSpan {
                    style: create_test_style(AnsiColor::Blue, false),
                    begin_pos: 12,
                    end_pos: 16, // "Test"
                },
            ],
        )
    }

    #[test]
    fn test_insert_at_beginning() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("START ", 0, 0, new_style);

        assert_eq!(result.text, "START Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the new span is at the beginning
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 6);
        assert_eq!(result.spans[0].style, new_style);

        // Check that existing spans are shifted
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 11);
    }

    #[test]
    fn test_insert_at_end() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert(" END", 16, 16, new_style);

        assert_eq!(result.text, "Hello World Test END");
        assert_eq!(result.spans.len(), 4);

        // Check that existing spans are unchanged
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);

        // Check that the new span is at the end
        assert_eq!(result.spans[3].begin_pos, 16);
        assert_eq!(result.spans[3].end_pos, 20);
        assert_eq!(result.spans[3].style, new_style);
    }

    #[test]
    fn test_insert_in_middle() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert(" MIDDLE", 6, 6, new_style);

        assert_eq!(result.text, "Hello  MIDDLEWorld Test");
        assert_eq!(result.spans.len(), 4);

        // Check spans before insertion point
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);

        // Check inserted span
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 13);
        assert_eq!(result.spans[1].style, new_style);

        // Check spans after insertion point are shifted
        assert_eq!(result.spans[2].begin_pos, 13);
        assert_eq!(result.spans[2].end_pos, 18);
    }

    #[test]
    fn test_insert_with_replacement() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("REPLACEMENT", 6, 11, new_style); // Replace "World"

        assert_eq!(result.text, "Hello REPLACEMENT Test");
        assert_eq!(result.spans.len(), 3);

        // Check that the replaced span is gone and new span is there
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 17);
        assert_eq!(result.spans[1].style, new_style);
    }

    #[test]
    fn test_insert_empty_string() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("", 6, 6, new_style);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3); // No new span added for empty string

        // Check that spans are unchanged
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
    }

    #[test]
    fn test_insert_bounds_checking() {
        let line = create_test_line();
        let new_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.insert("OVERFLOW", 100, 100, new_style);

        assert_eq!(result.text, "Hello World TestOVERFLOW");
        assert_eq!(result.spans.len(), 4);

        // Check that the new span is at the actual end
        assert_eq!(result.spans[3].begin_pos, 16);
        assert_eq!(result.spans[3].end_pos, 24);
    }

    #[test]
    fn test_highlight_at_beginning() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(0, 3, highlight_style);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the highlight span is first
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 3);
        assert_eq!(result.spans[0].style, highlight_style);

        // Check that the original span is truncated
        assert_eq!(result.spans[1].begin_pos, 3);
        assert_eq!(result.spans[1].end_pos, 5);
    }

    #[test]
    fn test_highlight_at_end() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(14, 16, highlight_style);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the original span is truncated
        assert_eq!(result.spans[2].begin_pos, 12);
        assert_eq!(result.spans[2].end_pos, 14);

        // Check that the highlight span is last
        assert_eq!(result.spans[3].begin_pos, 14);
        assert_eq!(result.spans[3].end_pos, 16);
        assert_eq!(result.spans[3].style, highlight_style);
    }

    #[test]
    fn test_highlight_spanning_multiple_spans() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(3, 9, highlight_style); // Spans across "Hello" and "World"

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the first span is truncated
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 3);

        // Check that the highlight span is in the middle
        assert_eq!(result.spans[1].begin_pos, 3);
        assert_eq!(result.spans[1].end_pos, 9);
        assert_eq!(result.spans[1].style, highlight_style);

        // Check that the second span is truncated
        assert_eq!(result.spans[2].begin_pos, 9);
        assert_eq!(result.spans[2].end_pos, 11);
    }

    #[test]
    fn test_highlight_encompassing_span() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(4, 8, highlight_style); // Encompasses part of "Hello" and space

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 4);

        // Check that the original span is split
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 4);

        // Check that the highlight span is in the middle
        assert_eq!(result.spans[1].begin_pos, 4);
        assert_eq!(result.spans[1].end_pos, 8);
        assert_eq!(result.spans[1].style, highlight_style);

        // Check that the original span continues after
        assert_eq!(result.spans[2].begin_pos, 8);
        assert_eq!(result.spans[2].end_pos, 11);
    }

    #[test]
    fn test_highlight_empty_range() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(5, 5, highlight_style);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3); // No change in spans

        // Check that spans are unchanged
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
    }

    #[test]
    fn test_highlight_bounds_checking() {
        let line = create_test_line();
        let highlight_style = create_test_style(AnsiColor::Yellow, true);
        let result = line.highlight(10, 100, highlight_style);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3);

        // Check that the highlight goes to the end of the text
        assert_eq!(result.spans[2].begin_pos, 10);
        assert_eq!(result.spans[2].end_pos, 16);
        assert_eq!(result.spans[2].style, highlight_style);
    }

    #[test]
    fn test_remove_at_beginning() {
        let line = create_test_line();
        let result = line.remove(0, 6); // Remove "Hello "

        assert_eq!(result.text, "World Test");
        assert_eq!(result.spans.len(), 2);

        // Check that the first span is removed and others are shifted
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 10);
    }

    #[test]
    fn test_remove_at_end() {
        let line = create_test_line();
        let result = line.remove(12, 16); // Remove "Test"

        assert_eq!(result.text, "Hello World ");
        assert_eq!(result.spans.len(), 2);

        // Check that the last span is removed
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 11);
    }

    #[test]
    fn test_remove_in_middle() {
        let line = create_test_line();
        let result = line.remove(6, 12); // Remove "World "

        assert_eq!(result.text, "Hello Test");
        assert_eq!(result.spans.len(), 2);

        // Check that the middle span is removed and others are shifted
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 10);
    }

    #[test]
    fn test_remove_partial_span() {
        let line = create_test_line();
        let result = line.remove(2, 8); // Remove "llo Wo"

        assert_eq!(result.text, "Herld Test");
        assert_eq!(result.spans.len(), 3);

        // Check that the first span is truncated
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 2);

        // Check that the second span (from "World") is truncated and shifted
        assert_eq!(result.spans[1].begin_pos, 2);
        assert_eq!(result.spans[1].end_pos, 5);

        // Check that the third span (from "Test") is shifted
        assert_eq!(result.spans[2].begin_pos, 6);
        assert_eq!(result.spans[2].end_pos, 10);
    }

    #[test]
    fn test_remove_empty_range() {
        let line = create_test_line();
        let result = line.remove(5, 5);

        assert_eq!(result.text, "Hello World Test");
        assert_eq!(result.spans.len(), 3);

        // Check that nothing changes
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
    }

    #[test]
    fn test_remove_bounds_checking() {
        let line = create_test_line();
        let result = line.remove(10, 100);

        assert_eq!(result.text, "Hello Worl");
        assert_eq!(result.spans.len(), 2);

        // Check that removal goes to the end of the text
        assert_eq!(result.spans[0].begin_pos, 0);
        assert_eq!(result.spans[0].end_pos, 5);
        assert_eq!(result.spans[1].begin_pos, 6);
        assert_eq!(result.spans[1].end_pos, 10);
    }

    #[test]
    fn test_remove_entire_text() {
        let line = create_test_line();
        let result = line.remove(0, 100);

        assert_eq!(result.text, "");
        assert_eq!(result.spans.len(), 0);
    }

    /// The renderer tiles the on-screen line by concatenating `text[span]` for every
    /// span in order, so a fully-covered line's spans must reproduce its text exactly.
    /// Overlapping spans duplicate text (the on-screen corruption); gaps drop it.
    fn assert_spans_tile_text(line: &StyledLine) {
        let mut rendered = String::new();
        let mut cursor = 0;
        for span in &line.spans {
            assert!(
                span.begin_pos <= span.end_pos,
                "inverted span {:?} in {:?}",
                span,
                line.spans
            );
            assert!(
                span.begin_pos >= cursor,
                "overlapping/unsorted spans {:?}",
                line.spans
            );
            assert_eq!(
                span.begin_pos, cursor,
                "gap before span {span:?} in {:?}",
                line.spans
            );
            rendered.push_str(&line.text[span.begin_pos..span.end_pos]);
            cursor = span.end_pos;
        }
        assert_eq!(cursor, line.text.len(), "spans do not reach end of text");
        assert_eq!(rendered, line.text, "spans do not tile text: {:?}", line.spans);
    }

    #[test]
    fn from_styled_runs_tiles_and_merges() {
        let red = create_test_style(AnsiColor::Red, true);
        let green = create_test_style(AnsiColor::Green, true);
        let line = StyledLine::from_styled_runs(
            &[
                ("plain ", red, None),
                ("", green, None), // empty runs contribute nothing
                ("more", red, None),
                (" green", green, None),
            ],
            red,
        );
        assert_eq!(line.text, "plain more green");
        // The two adjacent red runs merge into one span.
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].begin_pos, 0);
        assert_eq!(line.spans[0].end_pos, 10);
        assert_eq!(line.spans[0].style, red);
        assert_eq!(line.spans[1].begin_pos, 10);
        assert_eq!(line.spans[1].end_pos, 16);
        assert_eq!(line.spans[1].style, green);
        assert_spans_tile_text(&line);
    }

    #[test]
    fn from_styled_runs_empty_matches_empty_echo() {
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::from_styled_runs(&[], style);
        let echo = StyledLine::from_echo_str("");
        assert_eq!(line.text, "");
        assert_eq!(line.spans.len(), echo.spans.len());
        assert_eq!(line.spans[0].begin_pos, 0);
        assert_eq!(line.spans[0].end_pos, 0);
        assert_eq!(line.spans[0].style, style);
    }

    #[test]
    fn from_styled_runs_non_ascii_offsets_are_bytes() {
        let red = create_test_style(AnsiColor::Red, true);
        let green = create_test_style(AnsiColor::Green, true);
        let line = StyledLine::from_styled_runs(
            &[("caf\u{e9}", red, None), ("\u{1F600}!", green, None)],
            red,
        );
        assert_eq!(line.text, "caf\u{e9}\u{1F600}!");
        assert_eq!(line.spans[0].end_pos, 5); // "café" is 5 bytes
        assert_eq!(line.spans[1].end_pos, 10); // + 4-byte emoji + '!'
        assert_spans_tile_text(&line);
    }

    fn send_link(cmd: &str) -> LinkAction {
        LinkAction::Send(Arc::from(cmd))
    }

    #[test]
    fn from_styled_runs_links_merge_across_style_boundaries() {
        let red = create_test_style(AnsiColor::Red, true);
        let green = create_test_style(AnsiColor::Green, true);
        // One link over two differently-styled runs: 2 style spans, 1 link span.
        let line = StyledLine::from_styled_runs(
            &[
                ("go ", red, None),
                ("nor", red, Some(send_link("north"))),
                ("th", green, Some(send_link("north"))),
            ],
            red,
        );
        assert_eq!(line.text, "go north");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(
            line.links,
            vec![LinkSpan {
                begin_pos: 3,
                end_pos: 8,
                action: send_link("north"),
            }]
        );
    }

    #[test]
    fn links_remap_across_insert_and_remove() {
        let style = create_test_style(AnsiColor::White, false);
        let mut line = StyledLine::from_styled_runs(
            &[
                ("a ", style, None),
                ("link", style, Some(send_link("go"))),
                (" z", style, None),
            ],
            style,
        );
        assert_eq!(line.links, vec![LinkSpan { begin_pos: 2, end_pos: 6, action: send_link("go") }]);

        // Insert before the link: it shifts right.
        line = line.insert("XX", 0, 0, style);
        assert_eq!(line.text, "XXa link z");
        assert_eq!(line.links[0].begin_pos, 4);
        assert_eq!(line.links[0].end_pos, 8);

        // Replace the middle of the link ("in"): head and tail survive linked, the
        // inserted bytes are link-free.
        let split = line.insert("-", 5, 7, style);
        assert_eq!(split.text, "XXa l-k z");
        assert_eq!(
            split.links,
            vec![
                LinkSpan { begin_pos: 4, end_pos: 5, action: send_link("go") },
                LinkSpan { begin_pos: 6, end_pos: 7, action: send_link("go") },
            ]
        );

        // Remove a range covering the whole link: it disappears.
        let gone = line.remove(3, 9);
        assert_eq!(gone.text, "XXaz");
        assert!(gone.links.is_empty());

        // A recolor leaves links untouched.
        let recolored = line.highlight(0, 10, create_test_style(AnsiColor::Red, true));
        assert_eq!(recolored.links, line.links);

        // Append shifts the appended line's links.
        let tail = StyledLine::from_styled_runs(&[("tail", style, Some(send_link("t")))], style);
        let joined = line.append(&tail);
        assert_eq!(joined.links.len(), 2);
        assert_eq!(joined.links[1].begin_pos, line.text.len());
        assert_eq!(joined.links[1].end_pos, line.text.len() + 4);
    }

    #[test]
    fn test_replace_midline_spans_tile_text() {
        // Regression: a single-span server line whose `line.replace` wraps a mid-line
        // term rendered `You hold <a roasted turkey le<a roasted turkey leg> roasted
        // turkey leg> high...` because the encompassing span split into overlapping
        // ranges. The text was always correct; the spans were not.
        let text = "You hold a roasted turkey leg high for everyone to see.";
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );

        let begin = text.find("a roasted turkey leg").unwrap();
        let end = begin + "a roasted turkey leg".len();
        let result = line.insert("<a roasted turkey leg>", begin, end, style);

        assert_eq!(
            result.text,
            "You hold <a roasted turkey leg> high for everyone to see."
        );
        assert_spans_tile_text(&result);
    }

    #[test]
    fn test_replace_whole_line_tiles() {
        let text = "a roasted turkey leg";
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );

        let result = line.insert("<a roasted turkey leg>", 0, text.len(), style);

        assert_eq!(result.text, "<a roasted turkey leg>");
        assert_spans_tile_text(&result);
    }

    #[test]
    fn test_replace_across_span_boundary_tiles() {
        // A replacement that starts inside one span and ends inside the next: the head of
        // the first span and the surviving tail of the second must both be kept (the tail
        // was previously dropped, leaving a gap that erased trailing text on screen).
        let red = create_test_style(AnsiColor::Red, false);
        let green = create_test_style(AnsiColor::Green, false);
        let yellow = create_test_style(AnsiColor::Yellow, true);
        let line = StyledLine::new(
            "HelloWorld",
            vec![
                VtSpan {
                    style: red,
                    begin_pos: 0,
                    end_pos: 5,
                },
                VtSpan {
                    style: green,
                    begin_pos: 5,
                    end_pos: 10,
                },
            ],
        );

        let result = line.insert("XX", 3, 7, yellow); // replace "loWo"

        assert_eq!(result.text, "HelXXrld");
        assert_spans_tile_text(&result);
    }

    #[test]
    fn test_replace_shorter_than_match_tiles() {
        // Replacement shorter than the match (negative shift) must still tile.
        let text = "You hold a roasted turkey leg high.";
        let style = create_test_style(AnsiColor::White, false);
        let line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );

        let begin = text.find("a roasted turkey leg").unwrap();
        let end = begin + "a roasted turkey leg".len();
        let result = line.insert("leg", begin, end, style);

        assert_eq!(result.text, "You hold leg high.");
        assert_spans_tile_text(&result);
    }
}
