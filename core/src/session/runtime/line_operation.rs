use std::sync::Arc;

use crate::session::styled_line::{Color, LinkAction, LinkSpan, Style, StyledLine};

/// One run of a styled splice: its text, the colors it SET (an unset channel
/// inherits the style at the splice point when the operation applies — which is
/// only knowable then, at the line, not at the op boundary), and an optional link.
#[derive(Debug, Clone)]
pub struct SpliceRun {
    pub text: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub link: Option<LinkAction>,
}

/// A pure text/style **transform** applied to one line. Suppression and
/// routing (gag/redirect/copy) are deliberately not transforms — they live in
/// the per-line `LineRouting` state — so transforms always apply to every
/// sink a line is delivered to, even when the line is gagged from main.
#[derive(Debug, Clone)]
pub enum LineOperation {
    Insert {
        str: Arc<String>,
        begin: usize,
        end: usize,
        style: Style,
    },
    Replace {
        str: Arc<String>,
        begin: usize,
        end: usize,
    },
    Highlight {
        begin: usize,
        end: usize,
        style: Style,
    },
    Remove {
        begin: usize,
        end: usize,
    },
    /// Replace the byte range `[begin, end)` with styled (possibly linked) runs —
    /// the write path for `insert`/`replaceAt`/`replace` given a `StyledText`
    /// fragment. Unset run colors inherit the style at the splice point.
    Splice {
        runs: Arc<Vec<SpliceRun>>,
        begin: usize,
        end: usize,
    },
}

/// The style at a splice point. Prefer the span that actually contains the
/// position so a splice on a style boundary inherits the following span, not
/// the one immediately before it. At the end of a line (or in a malformed gap),
/// fall back to the closest preceding span; an unstyled line uses terminal
/// defaults.
fn splice_style_at(line: &StyledLine, position: usize) -> Style {
    line.spans
        .iter()
        .find(|span| span.begin_pos <= position && position < span.end_pos)
        .or_else(|| {
            line.spans
                .iter()
                .rev()
                .find(|span| span.begin_pos <= position)
        })
        .map_or(
            Style {
                fg: Color::DefaultForeground { bold: false },
                bg: Color::DefaultBackground,
            },
            |span| span.style,
        )
}

impl LineOperation {
    #[must_use]
    pub fn apply(&self, line: &Arc<StyledLine>) -> Arc<StyledLine> {
        match self {
            LineOperation::Insert {
                str,
                begin,
                end,
                style,
            } => Arc::new(line.insert(str.as_str(), *begin, *end, *style)),
            LineOperation::Replace { str, begin, end } => {
                Arc::new(line.insert(str.as_str(), *begin, *end, splice_style_at(line, *begin)))
            }
            LineOperation::Highlight { begin, end, style } => {
                Arc::new(line.highlight(*begin, *end, *style))
            }
            LineOperation::Remove { begin, end } => Arc::new(line.remove(*begin, *end)),
            LineOperation::Splice { runs, begin, end } => {
                // Mirror `insert`'s clamping so the per-run offsets below line up with
                // where the text actually lands.
                let begin = (*begin).min(line.text.len());
                let end = (*end).min(line.text.len().max(begin));

                // Unset run colors inherit the style at the splice point, just
                // like a plain `Replace`.
                let base_style = splice_style_at(line, begin);

                // One text splice (which also remaps the line's existing links), then
                // per-run recolors and link spans over the inserted range.
                let text: String = runs.iter().map(|run| run.text.as_str()).collect();
                let mut result = line.insert(&text, begin, end, base_style);
                // Only links pushed by THIS loop may merge with each other — a
                // surviving remapped link can end flush against the splice point with
                // an equal action, and extending it would conflate two distinct links.
                let fresh_links_start = result.links.len();
                let mut cursor = begin;
                for run in runs.iter() {
                    let run_end = cursor + run.text.len();
                    let style = Style {
                        fg: run.fg.unwrap_or(base_style.fg),
                        bg: run.bg.unwrap_or(base_style.bg),
                    };
                    if style != base_style {
                        result = result.highlight(cursor, run_end, style);
                    }
                    if let Some(action) = &run.link
                        && run_end > cursor
                    {
                        let may_merge = result.links.len() > fresh_links_start;
                        match result.links.last_mut() {
                            // A link crossing style runs arrives as several runs
                            // sharing one action; merge them back into one span.
                            Some(prev)
                                if may_merge
                                    && prev.end_pos == cursor
                                    && prev.action == *action =>
                            {
                                prev.end_pos = run_end;
                            }
                            _ => result.links.push(LinkSpan {
                                begin_pos: cursor,
                                end_pos: run_end,
                                action: action.clone(),
                            }),
                        }
                    }
                    cursor = run_end;
                }
                // The surviving remapped links may interleave with the fresh ones;
                // restore the sorted order the renderer and hit tests rely on.
                result.links.sort_by_key(|link| link.begin_pos);
                Arc::new(result)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::connection::vt_processor::AnsiColor;
    use crate::session::styled_line::VtSpan;

    fn bright(color: AnsiColor) -> Color {
        Color::Ansi { color, bold: true }
    }

    fn base_style() -> Style {
        Style {
            fg: Color::Rgb {
                r: 10,
                g: 20,
                b: 30,
            },
            bg: Color::DefaultBackground,
        }
    }

    fn single_span_line(text: &str) -> Arc<StyledLine> {
        Arc::new(StyledLine::new(
            text,
            vec![VtSpan {
                style: base_style(),
                begin_pos: 0,
                end_pos: text.len(),
            }],
        ))
    }

    /// The style rendered at byte `at` (the span containing it).
    fn style_at(line: &StyledLine, at: usize) -> Style {
        line.spans
            .iter()
            .find(|span| span.begin_pos <= at && at < span.end_pos)
            .unwrap_or_else(|| panic!("no span at {at} in {:?}", line.spans))
            .style
    }

    fn assert_tiles(line: &StyledLine) {
        let mut cursor = 0;
        for span in &line.spans {
            assert_eq!(span.begin_pos, cursor, "gap/overlap in {:?}", line.spans);
            cursor = span.end_pos;
        }
        assert_eq!(cursor, line.text.len(), "spans do not cover the text");
    }

    #[test]
    fn splice_inherits_unset_colors_and_carries_links() {
        let line = single_span_line("go north now");
        let link = LinkAction::Send(std::sync::Arc::from("north"));
        // Replace "north" with a two-run linked fragment: "N" inherits, "ORTH" is red.
        let op = LineOperation::Splice {
            runs: Arc::new(vec![
                SpliceRun {
                    text: "N".to_string(),
                    fg: None,
                    bg: None,
                    link: Some(link.clone()),
                },
                SpliceRun {
                    text: "ORTH".to_string(),
                    fg: Some(bright(AnsiColor::Red)),
                    bg: None,
                    link: Some(link.clone()),
                },
            ]),
            begin: 3,
            end: 8,
        };
        let result = op.apply(&line);

        assert_eq!(result.text, "go NORTH now");
        assert_tiles(&result);
        // "N" inherits the splice-point style; "ORTH" is red over the inherited bg.
        assert_eq!(style_at(&result, 3), base_style());
        assert_eq!(
            style_at(&result, 4),
            Style {
                fg: bright(AnsiColor::Red),
                bg: Color::DefaultBackground
            }
        );
        assert_eq!(style_at(&result, 9), base_style());
        // The two same-action runs merged into ONE link span covering "NORTH".
        assert_eq!(
            result.links,
            vec![LinkSpan {
                begin_pos: 3,
                end_pos: 8,
                action: link,
            }]
        );
    }

    #[test]
    fn splice_point_style_is_positional() {
        // Two spans; a splice inside the second inherits the SECOND span's style,
        // not the first's (Replace's old first-span default).
        let text = "redgreen";
        let red = Style {
            fg: bright(AnsiColor::Red),
            bg: Color::DefaultBackground,
        };
        let green = Style {
            fg: bright(AnsiColor::Green),
            bg: Color::DefaultBackground,
        };
        let line = Arc::new(StyledLine::new(
            text,
            vec![
                VtSpan {
                    style: red,
                    begin_pos: 0,
                    end_pos: 3,
                },
                VtSpan {
                    style: green,
                    begin_pos: 3,
                    end_pos: 8,
                },
            ],
        ));
        let op = LineOperation::Splice {
            runs: Arc::new(vec![SpliceRun {
                text: "X".to_string(),
                fg: None,
                bg: None,
                link: None,
            }]),
            begin: 5,
            end: 5,
        };
        let result = op.apply(&line);
        assert_eq!(result.text, "redgrXeen");
        assert_tiles(&result);
        assert_eq!(style_at(&result, 5), green);
        assert!(result.links.is_empty());
    }

    #[test]
    fn replace_inherits_match_style_at_span_boundary() {
        let default = Style {
            fg: Color::DefaultForeground { bold: false },
            bg: Color::DefaultBackground,
        };
        let green = Style {
            fg: bright(AnsiColor::Green),
            bg: Color::DefaultBackground,
        };
        let red = Style {
            fg: bright(AnsiColor::Red),
            bg: Color::DefaultBackground,
        };
        let line = Arc::new(StyledLine::new(
            "beforefooafter",
            vec![
                VtSpan {
                    style: default,
                    begin_pos: 0,
                    end_pos: 6,
                },
                VtSpan {
                    style: green,
                    begin_pos: 6,
                    end_pos: 9,
                },
                VtSpan {
                    style: red,
                    begin_pos: 9,
                    end_pos: 14,
                },
            ],
        ));

        let result = LineOperation::Replace {
            str: Arc::new("foo".to_string()),
            begin: 6,
            end: 9,
        }
        .apply(&line);

        assert_eq!(result.text, "beforefooafter");
        assert_tiles(&result);
        assert_eq!(style_at(&result, 5), default);
        assert_eq!(style_at(&result, 6), green);
        assert_eq!(style_at(&result, 8), green);
        assert_eq!(style_at(&result, 9), red);
    }

    #[test]
    fn splice_does_not_extend_a_preexisting_link() {
        let action = LinkAction::Send(std::sync::Arc::from("north"));
        let mut inner = StyledLine::new(
            "go north",
            vec![VtSpan {
                style: base_style(),
                begin_pos: 0,
                end_pos: 8,
            }],
        );
        inner.links.push(LinkSpan {
            begin_pos: 3,
            end_pos: 8,
            action: action.clone(),
        });
        let line = Arc::new(inner);

        // Append a same-action link flush against the existing one: two distinct
        // links were created, so two spans must remain.
        let op = LineOperation::Splice {
            runs: Arc::new(vec![SpliceRun {
                text: "!".to_string(),
                fg: None,
                bg: None,
                link: Some(action.clone()),
            }]),
            begin: 8,
            end: 8,
        };
        let result = op.apply(&line);
        assert_eq!(result.text, "go north!");
        assert_eq!(
            result.links,
            vec![
                LinkSpan {
                    begin_pos: 3,
                    end_pos: 8,
                    action: action.clone(),
                },
                LinkSpan {
                    begin_pos: 8,
                    end_pos: 9,
                    action,
                },
            ]
        );
    }

    #[test]
    fn splice_over_an_existing_link_replaces_it() {
        let mut inner = StyledLine::new(
            "go north now",
            vec![VtSpan {
                style: base_style(),
                begin_pos: 0,
                end_pos: 12,
            }],
        );
        inner.links.push(LinkSpan {
            begin_pos: 3,
            end_pos: 8,
            action: LinkAction::Send(std::sync::Arc::from("north")),
        });
        let line = Arc::new(inner);

        // Replace the linked word with a plain run: the old link must not survive
        // over the new text.
        let op = LineOperation::Splice {
            runs: Arc::new(vec![SpliceRun {
                text: "south".to_string(),
                fg: None,
                bg: None,
                link: None,
            }]),
            begin: 3,
            end: 8,
        };
        let result = op.apply(&line);
        assert_eq!(result.text, "go south now");
        assert_tiles(&result);
        assert!(
            result.links.is_empty(),
            "stale link survived: {:?}",
            result.links
        );
    }
}
