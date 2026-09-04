use std::sync::Arc;

#[cfg(test)]
use crate::session::styled_line::LinkAction;
use crate::session::styled_line::{
    Color, LinkSpan, Style, StyleUpdate, StyledLine, StyledLink, TextAttributesUpdate,
};

/// One run of a styled splice: its text, the style channels it SET (an unset channel
/// — including each individual text attribute — inherits the style at the splice
/// point when the operation applies, which is only knowable then, at the line, not
/// at the op boundary), and an optional link.
#[derive(Debug, Clone)]
pub struct SpliceRun {
    pub text: String,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub attributes: TextAttributesUpdate,
    pub link: Option<StyledLink>,
}

/// The link half of a highlight: leave the range's links alone, strip them, or
/// cover the range with one link (stripping what it overlaps first — a link
/// spanning the range keeps its outside pieces, like a splice).
#[derive(Debug, Clone)]
pub enum LinkUpdate {
    Keep,
    Clear,
    Set(StyledLink),
}

/// A pure text/style **transform** applied to one line. Suppression and
/// routing (gag/redirect/copy) are deliberately not transforms — they live in
/// the per-line `LineRouting` state — so transforms always apply to every
/// sink a line is delivered to, even when the line is gagged from main.
#[derive(Debug, Clone)]
pub enum LineOperation {
    /// Insert `str` over `[begin, end)`. The style channels the write left
    /// unset inherit the style at the insertion point, like a splice.
    Insert {
        str: Arc<String>,
        begin: usize,
        end: usize,
        style: StyleUpdate,
    },
    Replace {
        str: Arc<String>,
        begin: usize,
        end: usize,
    },
    /// Restyle `[begin, end)`: the update's set channels apply over each span
    /// in the range; everything left unset keeps what the span already had.
    /// `link` optionally replaces the range's link coverage the same way.
    Highlight {
        begin: usize,
        end: usize,
        style: StyleUpdate,
        link: LinkUpdate,
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
        .map_or(Style::DEFAULT, |span| span.style)
}

impl LineOperation {
    /// Whether applying this operation leaves all text before `boundary` byte-for-byte in
    /// place. Style-only edits keep source offsets stable; text edits are safe only when
    /// their splice begins at or after the boundary.
    #[must_use]
    pub(crate) fn preserves_text_before(&self, boundary: usize) -> bool {
        match self {
            Self::Highlight { .. } => true,
            Self::Insert { begin, .. }
            | Self::Replace { begin, .. }
            | Self::Remove { begin, .. }
            | Self::Splice { begin, .. } => *begin >= boundary,
        }
    }

    #[must_use]
    pub fn apply(&self, line: &Arc<StyledLine>) -> Arc<StyledLine> {
        match self {
            LineOperation::Insert {
                str,
                begin,
                end,
                style,
            } => Arc::new(line.insert(
                str.as_str(),
                *begin,
                *end,
                style.apply_to(splice_style_at(line, *begin)),
            )),
            LineOperation::Replace { str, begin, end } => {
                Arc::new(line.insert(str.as_str(), *begin, *end, splice_style_at(line, *begin)))
            }
            LineOperation::Highlight {
                begin,
                end,
                style,
                link,
            } => {
                // An unset style makes highlight() a plain clone, so a pure
                // linkify costs one rebuild, not two.
                let mut restyled = line.highlight(*begin, *end, *style);
                match link {
                    LinkUpdate::Keep => {}
                    LinkUpdate::Clear => restyled.relink(*begin, *end, None),
                    LinkUpdate::Set(link) => restyled.relink(*begin, *end, Some(link.clone())),
                }
                Arc::new(restyled)
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
                        attributes: run.attributes.apply_to(base_style.attributes),
                    };
                    if style != base_style {
                        result = result.highlight(cursor, run_end, style.into());
                    }
                    if let Some(link) = &run.link
                        && run_end > cursor
                    {
                        let may_merge = result.links.len() > fresh_links_start;
                        match result.links.last_mut() {
                            // A link crossing style runs arrives as several runs
                            // sharing one action; merge them back into one span.
                            Some(prev)
                                if may_merge
                                    && prev.end_pos == cursor
                                    && prev.action == link.action
                                    && prev.tooltip == link.tooltip
                                    && prev.style == link.style =>
                            {
                                prev.end_pos = run_end;
                            }
                            _ => result.links.push(LinkSpan {
                                begin_pos: cursor,
                                end_pos: run_end,
                                action: link.action.clone(),
                                tooltip: link.tooltip.clone(),
                                style: link.style.clone(),
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
    use crate::session::styled_line::{TextAttributes, VtSpan};

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
            ..Style::DEFAULT
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

    fn styled_link(action: LinkAction) -> StyledLink {
        StyledLink {
            action,
            tooltip: None,
            style: None,
        }
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
                    attributes: TextAttributesUpdate::UNSET,
                    link: Some(styled_link(link.clone())),
                },
                SpliceRun {
                    text: "ORTH".to_string(),
                    fg: Some(bright(AnsiColor::Red)),
                    bg: None,
                    attributes: TextAttributesUpdate::UNSET,
                    link: Some(styled_link(link.clone())),
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
                bg: Color::DefaultBackground,
                ..Style::DEFAULT
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
                tooltip: None,
                style: None,
            }]
        );
    }

    #[test]
    fn splice_inherits_or_explicitly_resets_text_attributes() {
        let inherited = TextAttributes {
            bold: true,
            italic: true,
            ..TextAttributes::DEFAULT
        };
        let base = Style {
            attributes: inherited,
            ..base_style()
        };
        let line = Arc::new(StyledLine::new(
            "xy",
            vec![VtSpan {
                style: base,
                begin_pos: 0,
                end_pos: 2,
            }],
        ));
        let op = LineOperation::Splice {
            runs: Arc::new(vec![
                SpliceRun {
                    text: "I".to_string(),
                    fg: None,
                    bg: None,
                    attributes: TextAttributesUpdate::UNSET,
                    link: None,
                },
                SpliceRun {
                    text: "R".to_string(),
                    fg: None,
                    bg: None,
                    attributes: TextAttributes::DEFAULT.into(),
                    link: None,
                },
            ]),
            begin: 0,
            end: 2,
        };
        let result = op.apply(&line);
        assert_eq!(style_at(&result, 0).attributes, inherited);
        assert_eq!(style_at(&result, 1).attributes, TextAttributes::DEFAULT);
        assert_tiles(&result);
    }

    #[test]
    fn splice_partial_attributes_merge_over_the_splice_point() {
        use crate::session::styled_line::Underline;

        let inherited = TextAttributes {
            bold: true,
            italic: true,
            ..TextAttributes::DEFAULT
        };
        let line = Arc::new(StyledLine::new(
            "xy",
            vec![VtSpan {
                style: Style {
                    attributes: inherited,
                    ..base_style()
                },
                begin_pos: 0,
                end_pos: 2,
            }],
        ));
        // The run sets two attribute fields; the splice point's bold survives.
        let op = LineOperation::Splice {
            runs: Arc::new(vec![SpliceRun {
                text: "Z".to_string(),
                fg: None,
                bg: None,
                attributes: TextAttributesUpdate {
                    italic: Some(false),
                    underline: Some(Underline::Single),
                    ..TextAttributesUpdate::UNSET
                },
                link: None,
            }]),
            begin: 0,
            end: 2,
        };
        let result = op.apply(&line);
        assert_eq!(
            style_at(&result, 0).attributes,
            TextAttributes {
                bold: true,
                italic: false,
                underline: Underline::Single,
                ..TextAttributes::DEFAULT
            }
        );
        assert_tiles(&result);
    }

    #[test]
    fn splice_point_style_is_positional() {
        // Two spans; a splice inside the second inherits the SECOND span's style,
        // not the first's (Replace's old first-span default).
        let text = "redgreen";
        let red = Style {
            fg: bright(AnsiColor::Red),
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
        };
        let green = Style {
            fg: bright(AnsiColor::Green),
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
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
                attributes: TextAttributesUpdate::UNSET,
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
            ..Style::DEFAULT
        };
        let green = Style {
            fg: bright(AnsiColor::Green),
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
        };
        let red = Style {
            fg: bright(AnsiColor::Red),
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
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
            tooltip: None,
            style: None,
        });
        let line = Arc::new(inner);

        // Append a same-action link flush against the existing one: two distinct
        // links were created, so two spans must remain.
        let op = LineOperation::Splice {
            runs: Arc::new(vec![SpliceRun {
                text: "!".to_string(),
                fg: None,
                bg: None,
                attributes: TextAttributesUpdate::UNSET,
                link: Some(styled_link(action.clone())),
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
                    tooltip: None,
                    style: None,
                },
                LinkSpan {
                    begin_pos: 8,
                    end_pos: 9,
                    action,
                    tooltip: None,
                    style: None,
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
            tooltip: None,
            style: None,
        });
        let line = Arc::new(inner);

        // Replace the linked word with a plain run: the old link must not survive
        // over the new text.
        let op = LineOperation::Splice {
            runs: Arc::new(vec![SpliceRun {
                text: "south".to_string(),
                fg: None,
                bg: None,
                attributes: TextAttributesUpdate::UNSET,
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

    /// A one-span line with one link span over `[begin, end)`.
    fn linked_line(text: &str, begin: usize, end: usize, action: LinkAction) -> Arc<StyledLine> {
        let mut inner = StyledLine::new(
            text,
            vec![VtSpan {
                style: base_style(),
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );
        inner.links.push(LinkSpan {
            begin_pos: begin,
            end_pos: end,
            action,
            tooltip: None,
            style: None,
        });
        Arc::new(inner)
    }

    #[test]
    fn highlight_set_link_replaces_overlapped_links_and_keeps_styling() {
        let old = LinkAction::Send(std::sync::Arc::from("north"));
        let new = LinkAction::Send(std::sync::Arc::from("south"));
        let line = linked_line("go north now", 3, 8, old);
        // A pure linkify: no style channels set, so spans are untouched.
        let op = LineOperation::Highlight {
            begin: 3,
            end: 8,
            style: StyleUpdate::UNSET,
            link: LinkUpdate::Set(styled_link(new.clone())),
        };
        let result = op.apply(&line);
        assert_eq!(result.text, "go north now");
        assert_eq!(result.spans, line.spans, "a linkify must not restyle");
        assert_eq!(
            result.links,
            vec![LinkSpan {
                begin_pos: 3,
                end_pos: 8,
                action: new,
                tooltip: None,
                style: None,
            }]
        );
    }

    #[test]
    fn highlight_clear_link_trims_a_spanning_link_to_its_outside_pieces() {
        let action = LinkAction::Send(std::sync::Arc::from("go"));
        let line = linked_line("go north now", 0, 12, action.clone());
        let op = LineOperation::Highlight {
            begin: 3,
            end: 8,
            style: StyleUpdate::UNSET,
            link: LinkUpdate::Clear,
        };
        let result = op.apply(&line);
        assert_eq!(
            result.links,
            vec![
                LinkSpan {
                    begin_pos: 0,
                    end_pos: 3,
                    action: action.clone(),
                    tooltip: None,
                    style: None,
                },
                LinkSpan {
                    begin_pos: 8,
                    end_pos: 12,
                    action,
                    tooltip: None,
                    style: None,
                },
            ]
        );
    }

    #[test]
    fn highlight_keep_leaves_links_alone() {
        let action = LinkAction::Send(std::sync::Arc::from("north"));
        let line = linked_line("go north now", 3, 8, action);
        let op = LineOperation::Highlight {
            begin: 0,
            end: 12,
            style: StyleUpdate {
                fg: Some(bright(AnsiColor::Red)),
                ..StyleUpdate::UNSET
            },
            link: LinkUpdate::Keep,
        };
        let result = op.apply(&line);
        assert_eq!(result.links, line.links);
        assert_eq!(style_at(&result, 5).fg, bright(AnsiColor::Red));
    }
}
