#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferPosition {
    pub line: usize,
    pub column: usize,
}

pub type LineSelection = Option<(usize, usize)>;

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    #[default]
    None,
    Selecting {
        origin: BufferPosition,
        from: BufferPosition,
        to: BufferPosition,
    },
    Selected {
        from: BufferPosition,
        to: BufferPosition,
    },
}

/// Returns the byte range `[start, end)` of the word that contains the caret
/// at `column` in `text`. Words are runs of non-whitespace characters.
///
/// `column` is a caret position, not a character index. The hit test snaps a
/// click on the right half of a glyph to the boundary after that glyph. A
/// caret between a word and whitespace (or the end of the line) therefore
/// selects the word before it. A caret inside a run of whitespace returns
/// `None`, and the caller falls back to a single click.
///
/// `column` is first moved back to the nearest char boundary, the same way
/// `TerminalBuffer::selected_text` clamps selection columns.
pub fn word_span_at(text: &str, column: usize) -> Option<(usize, usize)> {
    let mut column = column.min(text.len());
    while column > 0 && !text.is_char_boundary(column) {
        column -= 1;
    }

    let after = text[column..].chars().next();
    if after.is_none_or(char::is_whitespace) {
        // The caret touches no word on its right. Use the word on its left,
        // if there is one.
        let before = text[..column].chars().next_back()?;
        if before.is_whitespace() {
            return None;
        }
        column -= before.len_utf8();
    }

    let start = text[..column]
        .char_indices()
        .rev()
        .find(|&(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let end = text[column..]
        .char_indices()
        .find(|&(_, c)| c.is_whitespace())
        .map(|(i, _)| column + i)
        .unwrap_or(text.len());

    Some((start, end))
}

impl Selection {
    /// Whether this selection should block a click-release from focusing an
    /// input. The release handler runs after the terminal's own (the terminal
    /// is the `mouse_area`'s content), so a drag has settled into `Selected`
    /// with a non-empty range by the time this is read; a plain click reads
    /// as `None` or an empty `Selected`. Only the selection-less click
    /// focuses.
    pub fn blocks_focus(&self) -> bool {
        match self {
            Selection::None => false,
            Selection::Selected { from, to } => from != to,
            Selection::Selecting { .. } => true,
        }
    }

    pub fn for_line(&self, line_number: usize) -> LineSelection {
        match self {
            Selection::None => None,
            Selection::Selecting {
                from,
                to,
                origin: _,
            }
            | Selection::Selected { from, to } => {
                // see if this line_number fals in the range of from.line_number..=to.line_number
                if from.line <= line_number && to.line >= line_number {
                    Some((
                        if from.line == line_number {
                            from.column
                        } else {
                            0
                        },
                        if to.line == line_number {
                            to.column
                        } else {
                            usize::MAX
                        },
                    ))
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod word_span_at_tests {
    use super::word_span_at;

    #[test]
    fn mid_word() {
        let text = "advanced knifeplay foo";
        assert_eq!(word_span_at(text, 3), Some((0, 8)));
        assert_eq!(&text[0..8], "advanced");
    }

    #[test]
    fn start_of_word() {
        let text = "foo bar";
        assert_eq!(word_span_at(text, 4), Some((4, 7)));
        assert_eq!(&text[4..7], "bar");
    }

    #[test]
    fn caret_after_last_char_selects_word_before_it() {
        // A click on the right half of a glyph puts the caret after it, so
        // the right half of "foo" reads as column 3 and the right half of
        // the final "r" as column 7.
        let text = "foo bar";
        assert_eq!(word_span_at(text, 3), Some((0, 3)));
        assert_eq!(word_span_at(text, 7), Some((4, 7)));
    }

    #[test]
    fn single_word_line() {
        let text = "backstab";
        assert_eq!(word_span_at(text, 0), Some((0, 8)));
        assert_eq!(word_span_at(text, 7), Some((0, 8)));
        assert_eq!(word_span_at(text, 8), Some((0, 8)));
    }

    #[test]
    fn inside_whitespace_returns_none() {
        let text = "foo   bar";
        assert_eq!(word_span_at(text, 0), Some((0, 3)));
        assert_eq!(word_span_at(text, 3), Some((0, 3))); // caret after "foo"
        assert_eq!(word_span_at(text, 4), None); // inside the run of spaces
        assert_eq!(word_span_at(text, 5), None);
        assert_eq!(word_span_at(text, 6), Some((6, 9)));
    }

    #[test]
    fn multibyte_words() {
        // "héllo" is 6 bytes; the caret after its "o" is column 6.
        let text = "h\u{e9}llo w\u{f6}rld";
        assert_eq!(word_span_at(text, 2), Some((0, 6))); // inside é
        assert_eq!(word_span_at(text, 6), Some((0, 6)));
        assert_eq!(word_span_at(text, 7), Some((7, 13)));
        assert_eq!(&text[7..13], "w\u{f6}rld");

        // Three 3-byte ideographs; the caret after the third is column 9.
        let text = "\u{65e5}\u{672c}\u{8a9e} \u{30c6}\u{30ad}";
        assert_eq!(word_span_at(text, 9), Some((0, 9)));
        assert_eq!(word_span_at(text, 10), Some((10, 16)));

        // A multi-codepoint emoji cluster (4 + 3 + 4 bytes) is one word.
        let text = "a\u{1F469}\u{200D}\u{1F469} b";
        assert_eq!(word_span_at(text, 5), Some((0, 12)));
        assert_eq!(word_span_at(text, 12), Some((0, 12)));
        assert_eq!(word_span_at(text, 13), Some((13, 14)));
    }

    #[test]
    fn past_end_of_line_clamps_to_end() {
        let text = "foo";
        assert_eq!(word_span_at(text, 100), Some((0, 3)));
        assert_eq!(word_span_at("foo ", 4), None);
    }

    #[test]
    fn empty_line_returns_none() {
        assert_eq!(word_span_at("", 0), None);
    }
}
