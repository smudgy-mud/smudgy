use iced::Background;
use iced::widget::text::Span;
use selection::Selection;
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

use crate::prefs::TerminalPalette;
use smudgy_core::session::runtime::line_operation::LineOperation;
use smudgy_core::session::styled_line::{Color, LinkAction, Style, StyledLine};
use std::collections::{HashSet, VecDeque};
use std::num::NonZeroUsize;

type Link = ();

pub mod selection;

/// A click on a link span, as delivered to the pane's `on_link` handler.
#[derive(Debug, Clone)]
pub struct LinkClickEvent {
    pub action: LinkAction,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// The chip fill behind a link: a nearly-transparent wash of the text's own
/// foreground. The alpha matches the Markdown widget's link chip, so a link whose
/// foreground is the Markdown link color renders identically to a Markdown link.
const LINK_WASH_ALPHA: f32 = 0.14;

/// One renderable segment: underlined over the foreground wash when linked (unless
/// the span sets an explicit background, which wins — the underline stays).
/// "Explicit" is judged by the resolved color model: `bg: "default"` normalizes to
/// `DefaultBackground` at the op boundary and so still washes, while a background
/// literally painted the theme's background color counts as explicit and doesn't.
#[inline]
fn make_span(
    text: &str,
    style: Style,
    linked: bool,
    palette: &TerminalPalette,
) -> Span<'static, Link> {
    let fg = palette.resolve(style.fg);
    let mut span = Span::<'static, Link>::new(Cow::Owned(text.to_string())).color(fg);
    // Only a meaningful background sets the span highlight: the widget draws a
    // quad per highlighted span region, so the (overwhelmingly common) default
    // background must stay decoration-free rather than painting a quad of the
    // pane's own color under every span.
    if linked && style.bg == Color::DefaultBackground {
        span = span.background(Background::Color(iced::Color {
            a: LINK_WASH_ALPHA,
            ..fg
        }));
    } else if style.bg != Color::DefaultBackground {
        span = span.background(Background::Color(palette.resolve(style.bg)));
    }
    if linked { span.underline(true) } else { span }
}

/// Bakes a styled line's semantic colors into renderable spans using the
/// given palette. Style spans are split at link boundaries so linked ranges get
/// the link affordance without disturbing the line's own colors.
#[inline]
fn to_spans(
    styled_line: &Arc<StyledLine>,
    palette: &TerminalPalette,
) -> Rc<Vec<Span<'static, Link>>> {
    let mut spans = Vec::with_capacity(styled_line.spans.len());
    for span_info in &styled_line.spans {
        let (begin, end) = (span_info.begin_pos, span_info.end_pos);
        if styled_line.links.is_empty() || begin == end {
            spans.push(make_span(
                &styled_line.text[begin..end],
                span_info.style,
                false,
                palette,
            ));
            continue;
        }
        // Links are sorted and non-overlapping; walk the ones intersecting this span,
        // alternating plain and linked segments.
        let mut cursor = begin;
        for link in &styled_line.links {
            if link.end_pos <= cursor {
                continue;
            }
            if link.begin_pos >= end {
                break;
            }
            let linked_begin = link.begin_pos.max(cursor);
            if linked_begin > cursor {
                spans.push(make_span(
                    &styled_line.text[cursor..linked_begin],
                    span_info.style,
                    false,
                    palette,
                ));
            }
            let linked_end = link.end_pos.min(end);
            spans.push(make_span(
                &styled_line.text[linked_begin..linked_end],
                span_info.style,
                true,
                palette,
            ));
            cursor = linked_end;
        }
        if cursor < end {
            spans.push(make_span(
                &styled_line.text[cursor..end],
                span_info.style,
                false,
                palette,
            ));
        }
    }
    Rc::new(spans)
}

/// Clamp a byte offset to `text`'s length and snap it down to the nearest char
/// boundary, yielding an offset that is always safe to slice `text` at.
#[inline]
fn clamp_to_char_boundary(text: &str, mut col: usize) -> usize {
    if col >= text.len() {
        return text.len();
    }
    while col > 0 && !text.is_char_boundary(col) {
        col -= 1;
    }
    col
}

#[inline]
fn strip_possessive_suffix(word: &str) -> &str {
    if let Some(stripped) = word.strip_suffix("'s") {
        stripped
    } else if let Some(stripped) = word.strip_suffix("'S") {
        stripped
    } else if let Some(stripped) = word.strip_suffix("’s") {
        stripped
    } else if let Some(stripped) = word.strip_suffix("’S") {
        stripped
    } else {
        word
    }
}

impl AsRef<[Span<'static, ()>]> for BufferLine {
    fn as_ref(&self) -> &[Span<'static, ()>] {
        self.spans().as_slice()
    }
}

#[derive(Debug, Clone)]
pub struct BufferLine {
    pub styled_line: Arc<StyledLine>,
    /// Renderable spans, baked from `styled_line` on first access. Lazy so a
    /// line that scrolls through the buffer unseen (a burst larger than the
    /// window, scrollback eviction) never pays `to_spans` at all; only lines
    /// the pane actually lays out are baked. Cleared — not eagerly rebaked —
    /// on palette changes and line edits.
    spans: std::cell::OnceCell<Rc<Vec<Span<'static, ()>>>>,
}

impl PartialEq for BufferLine {
    fn eq(&self, other: &Self) -> bool {
        self.styled_line == other.styled_line
    }
}

impl From<Arc<StyledLine>> for BufferLine {
    fn from(styled_line: Arc<StyledLine>) -> Self {
        Self {
            spans: std::cell::OnceCell::new(),
            styled_line,
        }
    }
}

impl BufferLine {
    /// The line's renderable spans, baking them against the current palette on
    /// first access. The returned `Rc` is pointer-stable until the spans are
    /// invalidated (palette change, line edit) — the pane's paragraph cache
    /// keys on that identity.
    pub fn spans(&self) -> &Rc<Vec<Span<'static, ()>>> {
        self.spans
            .get_or_init(|| to_spans(&self.styled_line, &crate::prefs::current().palette))
    }

    /// Drop the baked spans so the next access re-bakes them (and downstream
    /// paragraph caches, keyed on the `Rc` identity, naturally miss).
    fn invalidate_spans(&mut self) {
        self.spans.take();
    }
}

#[derive(Debug)]
pub struct TerminalBuffer {
    lines: VecDeque<BufferLine>,
    max_lines: NonZeroUsize,
    line_terminated: bool,
    last_line_number: usize,
    /// The prefs generation the lines' spans were baked with; see
    /// [`Self::refresh_styles`].
    span_generation: u64,
    /// How many held lines carry link spans, maintained at every structural
    /// mutation — so the per-frame hover path can skip hit testing entirely on
    /// the (overwhelmingly common) linkless buffer via [`Self::has_links`].
    lines_with_links: usize,
}

impl Default for TerminalBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalBuffer {
    /// Creates a new, empty `TerminalBuffer` with a default line limit (e.g., 10,000 lines).
    /// The internal buffer is pre-allocated to this default limit.
    pub fn new() -> Self {
        const DEFAULT_MAX_LINES: usize = 10_000;
        let max_lines =
            NonZeroUsize::new(DEFAULT_MAX_LINES).expect("Default max lines is non-zero");
        Self::new_with_max_lines(max_lines)
    }

    /// Creates a new `TerminalBuffer` with a specified maximum number of lines.
    ///
    /// # Arguments
    ///
    /// * `max_lines`: The maximum number of lines the buffer can hold. Must be non-zero.
    ///   The internal `VecDeque` will be initialized with this capacity.
    pub fn new_with_max_lines(max_lines: NonZeroUsize) -> Self {
        Self {
            lines: VecDeque::with_capacity(max_lines.get()),
            max_lines,
            line_terminated: false,
            last_line_number: 0,
            span_generation: crate::prefs::current().generation,
            lines_with_links: 0,
        }
    }

    /// Whether any held line carries a link span. O(1); the per-frame hover
    /// path uses it to skip hit testing on linkless buffers.
    pub fn has_links(&self) -> bool {
        self.lines_with_links > 0
    }

    /// Account for `line` entering the buffer (call beside every push).
    fn note_added(&mut self, line: &BufferLine) {
        if !line.styled_line.links.is_empty() {
            self.lines_with_links += 1;
        }
    }

    /// Account for `line` leaving the buffer (call on every pop).
    fn note_removed(&mut self, line: &BufferLine) {
        if !line.styled_line.links.is_empty() {
            self.lines_with_links -= 1;
        }
    }

    /// Pop the oldest line, keeping the link accounting straight.
    fn evict_front(&mut self) {
        if let Some(line) = self.lines.pop_front() {
            self.note_removed(&line);
        }
    }

    /// Changes the scrollback limit, trimming the oldest lines if the buffer
    /// already exceeds it.
    pub fn set_max_lines(&mut self, max_lines: NonZeroUsize) {
        self.max_lines = max_lines;
        while self.lines.len() > max_lines.get() {
            self.evict_front();
        }
    }

    /// Invalidates every line's baked spans if the preferences changed since
    /// they were built (palette swaps, etc.), so visible lines re-bake against
    /// the new palette on their next layout — and never-shown scrollback pays
    /// nothing. Dropping the span `Rc`s naturally invalidates downstream
    /// paragraph caches. Cheap one-off per settings change; a no-op otherwise.
    pub fn refresh_styles(&mut self) {
        let prefs = crate::prefs::current();
        if prefs.generation == self.span_generation {
            return;
        }

        for line in &mut self.lines {
            line.invalidate_spans();
        }

        self.span_generation = prefs.generation;
    }

    pub fn commit_current_line(&mut self) {
        self.line_terminated = true;
    }

    pub fn extend_line(&mut self, line_in: Arc<StyledLine>) {
        if self.line_terminated {
            self.last_line_number += 1;
            self.line_terminated = false;

            while self.lines.len() > (self.max_lines.get() - 1) {
                self.evict_front();
            }

            let line: BufferLine = line_in.into();
            self.note_added(&line);
            self.lines.push_back(line);
        } else {
            match self.lines.pop_back() {
                Some(line) => {
                    self.note_removed(&line);
                    let joined: BufferLine = Arc::new(line.styled_line.append(&line_in)).into();
                    self.note_added(&joined);
                    self.lines.push_back(joined);
                }
                None => {
                    self.last_line_number += 1;
                    let line: BufferLine = line_in.into();
                    self.note_added(&line);
                    self.lines.push_back(line);
                }
            }
        }
    }

    /// Adds a line to the buffer.
    /// If the buffer is at its `max_lines` capacity, the oldest line is removed.
    // Buffer-manipulation helper; exercised by tests and kept as part of the
    // buffer's coherent line API (the live path uses `extend_line`).
    #[allow(dead_code)]
    pub fn push_line(&mut self, line: Arc<StyledLine>) {
        self.last_line_number += 1;

        let limit = self.max_lines.get();

        // Remove oldest lines if the buffer is at or would exceed the limit.
        // We want lines.len() to be at most limit - 1 before push_back,
        // so that after push_back, lines.len() is at most limit.
        while self.lines.len() >= limit {
            self.evict_front();
        }
        let line: BufferLine = line.into();
        self.note_added(&line);
        self.lines.push_back(line);
        self.line_terminated = true;
    }

    /// Returns a reverse iterator over the lines in the buffer.
    /// This allows iterating from the most recently added line to the oldest.
    // Part of the buffer's iteration API; kept alongside `iter_rev_with_offset`.
    #[allow(dead_code)]
    pub fn iter_rev(
        &self,
    ) -> impl DoubleEndedIterator<Item = &BufferLine> + ExactSizeIterator<Item = &BufferLine> {
        self.lines.iter().rev()
    }

    pub fn iter_rev_with_line_number(
        &self,
        last_line_number: Option<usize>,
    ) -> impl Iterator<Item = (usize, &BufferLine)> {
        let buffer_last_line_number = self.last_line_number;
        let to_skip = buffer_last_line_number - last_line_number.unwrap_or(buffer_last_line_number);
        self.lines
            .iter()
            .rev()
            .skip(to_skip)
            .zip(to_skip..)
            .map(move |(line, i)| (buffer_last_line_number - i, line))
    }

    /// Returns an iterator over the lines in the buffer, starting from an offset from the end and iterating in reverse.
    ///
    /// # Arguments
    ///
    /// * `offset`: The number of lines to skip from the end before starting reverse iteration.
    ///   An offset of 0 is equivalent to `iter_rev()`.
    // Part of the buffer's iteration API; kept for scrollback-offset rendering.
    #[allow(dead_code)]
    pub fn iter_rev_with_offset(
        &self,
        offset: usize,
    ) -> impl DoubleEndedIterator<Item = &BufferLine> + ExactSizeIterator<Item = &BufferLine> {
        self.lines.iter().rev().skip(offset)
    }

    /// Returns the current number of lines in the buffer.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Returns `true` if the buffer contains no lines.
    // Kept as the conventional companion to `len()`.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn last_line_number(&self) -> usize {
        self.last_line_number
    }

    pub fn selected_text(&self, selection: &Selection) -> String {
        match selection {
            Selection::None => String::new(),
            Selection::Selecting { from, to, .. } | Selection::Selected { from, to } => {
                let offset = self.last_line_number - self.lines.len();

                // Selection line numbers are absolute and outlive the buffer:
                // a `clear()` (clear_lines) or scrollback eviction can leave a
                // stale selection pointing at lines that are no longer held.
                // Clamp to the live range `(offset, last_line_number]` and bail
                // when nothing overlaps, so the subtraction below never
                // underflows and `self.lines[i]` never indexes out of bounds.
                if self.lines.is_empty() || to.line <= offset || from.line > self.last_line_number {
                    return String::new();
                }
                let first_line = from.line.max(offset + 1);
                let last_line = to.line.min(self.last_line_number);
                let start_line_index = first_line - offset - 1;
                let to_line_index = last_line - offset - 1;
                // Only honor the selection's own column bounds on the lines
                // that survived the clamp; a clamped-in edge starts/ends whole.
                let use_from_column = first_line == from.line;
                let use_to_column = last_line == to.line;

                (start_line_index..=to_line_index)
                    .map(|i| {
                        let line = &self.lines[i];
                        let text = line.styled_line.text.as_str();
                        let start_column = if i == start_line_index && use_from_column {
                            from.column
                        } else {
                            0
                        };
                        let end_column = if i == to_line_index && use_to_column {
                            to.column
                        } else {
                            text.len()
                        };

                        // Selection columns are byte offsets into the rendered line; clamp
                        // to the text and snap to char boundaries so copy can never slice
                        // past the end or mid-codepoint (either of which panics).
                        let start_column = clamp_to_char_boundary(text, start_column);
                        let end_column = clamp_to_char_boundary(text, end_column).max(start_column);

                        &text[start_column..end_column]
                    })
                    .collect::<Vec<&str>>()
                    .join("\n")
            }
        }
    }

    /// Finds the most recent word matching the given prefix.
    /// Tokens are broken apart using any non-alphanumeric delimiter (e.g., `:`, `/`,
    /// `]`, etc.) while preserving useful in-word punctuation like apostrophes and
    /// hyphens. If the user types a delimiter in the prefix, the full token (including
    /// the delimiter and the segment that follows) is matched. Trailing punctuation is
    /// stripped automatically so words like `guard:Awful,` stay searchable. Possessive
    /// endings (`'s`) are removed unless the prefix itself contains an apostrophe.
    ///
    /// # Arguments
    /// * `prefix` - The prefix to match against (case-insensitive)
    /// * `skip_words_in` - Optional set of words to ignore in the search (exact match)
    /// * `skip_words_folded` - Borrowed sets of lowercase-folded words to
    ///   ignore case-insensitively (candidates are folded before the check):
    ///   the completion blacklist and the offered-registered-suggestion
    ///   filter, passed as the caller already holds them — no per-call union
    ///   set is materialized
    /// * `n_recent_lines` - Number of recent lines to search through
    ///
    /// # Returns
    /// * `Option<String>` - The matching word if found, or None otherwise
    pub fn find_recent_word_by_prefix(
        &self,
        prefix: &str,
        skip_words_in: Option<&HashSet<String>>,
        skip_words_folded: &[&HashSet<String>],
        n_recent_lines: usize,
    ) -> Option<String> {
        let lowercase_prefix = prefix.to_lowercase();
        let is_internal_punctuation =
            |c: char| matches!(c, '\'' | '’' | '-' | '‐' | '‑' | '‒' | '–' | '—' | '_');
        let is_segment_delimiter = |c: char| !c.is_alphanumeric() && !is_internal_punctuation(c);
        let prefix_contains_delimiter = prefix.chars().any(is_segment_delimiter);
        let prefix_contains_apostrophe = prefix.chars().any(|c| matches!(c, '\'' | '’'));

        let consider_candidate = |candidate: &str| -> Option<String> {
            let candidate_for_match = if prefix_contains_apostrophe {
                candidate
            } else {
                strip_possessive_suffix(candidate)
            };

            if candidate_for_match.is_empty() {
                return None;
            }

            if let Some(history) = skip_words_in
                && history.contains(candidate_for_match)
            {
                return None;
            }

            let folded_candidate = candidate_for_match.to_lowercase();
            if skip_words_folded
                .iter()
                .any(|folded| folded.contains(&folded_candidate))
            {
                return None;
            }

            if folded_candidate.starts_with(&lowercase_prefix) {
                return Some(candidate_for_match.to_string());
            }

            None
        };

        self.lines
            .iter()
            .rev()
            .take(n_recent_lines)
            .find_map(|line| {
                // Split line by whitespace to get words
                for raw_word in line.styled_line.text.split_whitespace() {
                    // Clean the word by trimming non-alphanumeric chars from start/end
                    let word = raw_word.trim_matches(|c: char| !c.is_alphanumeric());

                    // Skip empty words
                    if word.is_empty() {
                        continue;
                    }

                    if prefix_contains_delimiter {
                        if let Some(result) = consider_candidate(word) {
                            return Some(result);
                        }
                        continue;
                    }

                    let mut segment_start: Option<usize> = None;

                    for (idx, ch) in word.char_indices() {
                        if is_segment_delimiter(ch) {
                            if let Some(start) = segment_start.take()
                                && start != idx
                                && let Some(result) = consider_candidate(&word[start..idx])
                            {
                                return Some(result);
                            }
                        } else if segment_start.is_none() {
                            segment_start = Some(idx);
                        }
                    }

                    if let Some(start) = segment_start
                        && let Some(result) = consider_candidate(&word[start..])
                    {
                        return Some(result);
                    }
                }
                None
            })
    }

    /// The link action under byte `column` of absolute line `line_number`, if any.
    /// Backs the pane's hover cursor and click dispatch.
    pub fn link_at(&self, line_number: usize, column: usize) -> Option<LinkAction> {
        let offset = self.last_line_number - self.lines.len();
        if line_number <= offset || line_number > self.last_line_number {
            return None;
        }
        let line = self.lines.get(line_number - offset - 1)?;
        line.styled_line
            .links
            .iter()
            .find(|link| link.begin_pos <= column && column < link.end_pos)
            .map(|link| link.action.clone())
    }

    pub fn perform_line_operation(&mut self, line_number: usize, operation: LineOperation) {
        let offset = self.last_line_number - self.lines.len();
        // A line older than the buffer holds (scrolled out, or dropped by
        // `clear_lines`) has no index here; without this guard the subtraction
        // below underflows.
        if line_number <= offset {
            return;
        }
        let line_number = line_number - offset - 1;
        if let Some(line) = self.lines.get_mut(line_number) {
            let had_links = !line.styled_line.links.is_empty();
            line.styled_line = operation.apply(&line.styled_line);
            line.invalidate_spans();
            // An edit can add or drop a line's links; keep the O(1) count true.
            let has_links = !line.styled_line.links.is_empty();
            if has_links && !had_links {
                self.lines_with_links += 1;
            } else if !has_links && had_links {
                self.lines_with_links -= 1;
            }
        }
    }

    /// Drop the unterminated tail line (core's `RetractOpenLine`): the line's
    /// text is being routed elsewhere. Rolls the line number back so the next
    /// line takes the retracted one's number — exactly the accounting core
    /// keeps (`emitted_line_count` never counted the open line). A no-op when
    /// no line is open.
    pub fn retract_open_line(&mut self) {
        if !self.line_terminated
            && let Some(line) = self.lines.pop_back()
        {
            self.note_removed(&line);
            self.last_line_number -= 1;
            self.line_terminated = true;
        }
    }

    /// Clear the scrollback (`pane.clear()`), keeping the line numbering —
    /// numbers keep increasing across a clear so core/UI parity is untouched.
    pub fn clear_lines(&mut self) {
        self.lines.clear();
        self.lines_with_links = 0;
        self.line_terminated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_core::session::styled_line::{StyledLine, VtSpan};
    use std::num::NonZeroUsize; // Assuming VtSpan is needed for StyledLine::new

    // Helper to create Arc<StyledLine> for tests
    fn sl(s: &str) -> Arc<StyledLine> {
        Arc::new(StyledLine::new(s, Vec::<VtSpan>::new()))
    }

    #[test]
    fn test_new_buffer_initial_state() {
        let buffer = TerminalBuffer::new();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.last_line_number, 0);
        assert_eq!(buffer.max_lines.get(), 10_000); // Default max lines
        assert!(!buffer.line_terminated); // Initial state before any line commit or push
    }

    #[test]
    fn test_new_with_max_lines_initial_state() {
        let max_lines = NonZeroUsize::new(50).unwrap();
        let buffer = TerminalBuffer::new_with_max_lines(max_lines);
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.last_line_number, 0);
        assert_eq!(buffer.max_lines, max_lines);
        assert!(!buffer.line_terminated);
    }

    #[test]
    fn test_push_line_increments_current_line_number() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(3).unwrap());
        assert_eq!(buffer.last_line_number, 0);

        buffer.push_line(sl("line 1"));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, 1);
        assert!(buffer.line_terminated);

        buffer.push_line(sl("line 2"));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 2);
        assert!(buffer.line_terminated);
    }

    #[test]
    fn test_extend_line_increments_current_line_number() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(3).unwrap());

        // Case 1: Extending when line_terminated is true
        buffer.commit_current_line(); // Make line_terminated true
        assert!(buffer.line_terminated);
        buffer.extend_line(sl("line 1 part 1"));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, 1); // Incremented
        assert!(!buffer.line_terminated); // Becomes false after extend

        // Case 2: Extending when line_terminated is false (continuing a line)
        // The current logic in extend_line when line_terminated is false and buffer not empty
        // pops and re-pushes the existing last line, ignoring the input.
        // So, current_line_number should not change.
        let previous_line_number = buffer.last_line_number;
        buffer.extend_line(sl("line 1 part 2 - ignored"));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, previous_line_number); // Not incremented
        assert!(!buffer.line_terminated);

        // Reset for next test part
        let mut buffer2 = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(3).unwrap());

        // Case 3: Extending when line_terminated is false but buffer is empty (first line)
        assert!(!buffer2.line_terminated);
        assert!(buffer2.is_empty());
        buffer2.extend_line(sl("first line segment"));
        assert_eq!(buffer2.len(), 1);
        assert_eq!(buffer2.last_line_number, 1); // Incremented
        assert!(!buffer2.line_terminated);
    }

    #[test]
    fn selected_text_survives_clear_and_scrollback_eviction() {
        use super::selection::{BufferPosition, Selection};
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("alpha"));
        buffer.push_line(sl("bravo"));
        let selection = Selection::Selected {
            from: BufferPosition { line: 1, column: 0 },
            to: BufferPosition { line: 2, column: 5 },
        };
        assert_eq!(buffer.selected_text(&selection), "alpha\nbravo");

        // A script `mainPane.clear()` empties the buffer but keeps line
        // numbering; the stale selection must clamp away, never panic
        // (it used to underflow in debug / index out of bounds in release).
        buffer.clear_lines();
        assert_eq!(buffer.selected_text(&selection), "");

        // Fresh content after the clear: the stale low line numbers stay
        // clamped out, so no wrong row is ever read.
        buffer.push_line(sl("charlie"));
        assert_eq!(buffer.selected_text(&selection), "");

        // A selection that straddles the live/evicted boundary keeps only the
        // surviving tail, starting whole (the clamped-in edge drops its column).
        let straddling = Selection::Selected {
            from: BufferPosition { line: 2, column: 2 },
            to: BufferPosition { line: 3, column: 7 },
        };
        assert_eq!(buffer.selected_text(&straddling), "charlie");
    }

    #[test]
    fn test_buffer_wrapping_and_current_line_number() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(2).unwrap());
        buffer.push_line(sl("1"));
        buffer.push_line(sl("2"));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 2);

        buffer.push_line(sl("3")); // Wraps, "1" is popped
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 3);
        assert_eq!(buffer.lines[0].styled_line.text, "2");
        assert_eq!(buffer.lines[1].styled_line.text, "3");

        buffer.push_line(sl("4")); // Wraps, "2" is popped
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 4);
        assert_eq!(buffer.lines[0].styled_line.text, "3");
        assert_eq!(buffer.lines[1].styled_line.text, "4");
    }

    #[test]
    fn test_iter_rev_with_line_number_empty() {
        let buffer = TerminalBuffer::new();
        assert_eq!(buffer.iter_rev_with_line_number(None).count(), 0);
    }

    #[test]
    fn test_iter_rev_with_line_number_no_wrap() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(5).unwrap());
        buffer.push_line(sl("L1")); // cln=1
        buffer.push_line(sl("L2")); // cln=2
        buffer.push_line(sl("L3")); // cln=3. Lines: [L1,L2,L3]

        // iter().rev(): L3, L2, L1
        // enumerate(): (0,L3), (1,L2), (2,L1)
        // map |(i,line)| (cln - i, line) where cln = 3
        // (3-0, L3) -> (3,L3)
        // (3-1, L2) -> (2,L2)
        // (3-2, L1) -> (1,L1)
        let mut iter = buffer.iter_rev_with_line_number(None);
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((3, "L3"))
        );
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((2, "L2"))
        );
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((1, "L1"))
        );
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_iter_rev_with_line_number_with_wrap() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(2).unwrap());
        buffer.push_line(sl("L1")); // cln=1
        buffer.push_line(sl("L2")); // cln=2. Buffer: [L1,L2]
        buffer.push_line(sl("L3")); // cln=3. Buffer: [L2,L3]

        // cln = 3. Lines in buffer (reversed): L3, L2
        // enumerate: (0, L3), (1, L2)
        // map |(i,line)| (cln - i, line)
        // (3-0, L3) -> (3, L3)
        // (3-1, L2) -> (2, L2)
        let mut iter = buffer.iter_rev_with_line_number(None);
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((3, "L3"))
        );
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((2, "L2"))
        );
        assert_eq!(iter.next(), None);
    }

    fn linked_line(text: &str, begin: usize, end: usize) -> Arc<StyledLine> {
        use smudgy_core::session::styled_line::{LinkSpan, VtSpan};
        let style = Style {
            fg: Color::Rgb {
                r: 200,
                g: 10,
                b: 10,
            },
            bg: Color::DefaultBackground,
        };
        let mut line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );
        line.links.push(LinkSpan {
            begin_pos: begin,
            end_pos: end,
            action: LinkAction::Send(Arc::from("north")),
        });
        Arc::new(line)
    }

    #[test]
    fn to_spans_splits_at_link_boundaries_with_chip() {
        let line = linked_line("go north now", 3, 8);
        let palette = &crate::prefs::current().palette;
        let spans = to_spans(&line, palette);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "go ");
        assert_eq!(spans[1].text, "north");
        assert_eq!(spans[2].text, " now");

        // Only the linked segment is underlined, over a wash of its own foreground;
        // the segments around it keep the plain background.
        assert!(!spans[0].underline && !spans[2].underline);
        assert!(spans[1].underline);
        let fg = palette.resolve(Color::Rgb {
            r: 200,
            g: 10,
            b: 10,
        });
        assert_eq!(
            spans[1].highlight.map(|h| h.background),
            Some(Background::Color(iced::Color {
                a: LINK_WASH_ALPHA,
                ..fg
            }))
        );
        assert_ne!(
            spans[0].highlight.map(|h| h.background),
            spans[1].highlight.map(|h| h.background)
        );
    }

    #[test]
    fn to_spans_keeps_explicit_background_under_a_link() {
        use smudgy_core::session::styled_line::{LinkSpan, VtSpan};
        let style = Style {
            fg: Color::Rgb {
                r: 200,
                g: 10,
                b: 10,
            },
            bg: Color::Rgb { r: 1, g: 2, b: 3 },
        };
        let mut line = StyledLine::new(
            "north",
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: 5,
            }],
        );
        line.links.push(LinkSpan {
            begin_pos: 0,
            end_pos: 5,
            action: LinkAction::Send(Arc::from("north")),
        });
        let palette = &crate::prefs::current().palette;
        let spans = to_spans(&Arc::new(line), palette);
        assert_eq!(spans.len(), 1);
        // The author's background wins over the wash; the underline stays.
        assert!(spans[0].underline);
        assert_eq!(
            spans[0].highlight.map(|h| h.background),
            Some(Background::Color(palette.resolve(Color::Rgb {
                r: 1,
                g: 2,
                b: 3
            })))
        );
    }

    #[test]
    fn link_at_resolves_by_absolute_line_and_column() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("plain"));
        buffer.push_line(linked_line("go north now", 3, 8));

        // Inside the link.
        assert_eq!(
            buffer.link_at(2, 5),
            Some(LinkAction::Send(Arc::from("north")))
        );
        // Boundary semantics: begin inclusive, end exclusive.
        assert_eq!(
            buffer.link_at(2, 3),
            Some(LinkAction::Send(Arc::from("north")))
        );
        assert_eq!(buffer.link_at(2, 8), None);
        // Off-link text, another line, and out-of-window numbers all miss.
        assert_eq!(buffer.link_at(2, 0), None);
        assert_eq!(buffer.link_at(1, 5), None);
        assert_eq!(buffer.link_at(0, 5), None);
        assert_eq!(buffer.link_at(99, 5), None);
    }

    #[test]
    fn find_recent_word_logic() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("hello world"));
        buffer.push_line(sl("test another one"));
        buffer.push_line(sl("prefix_found here"));
        buffer.push_line(sl("try prefix_again"));

        // Test basic prefix matching
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", None, &[], 4),
            Some("prefix_again".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", None, &[], 2),
            Some("prefix_again".to_string())
        ); // Only search last 2 lines
        assert_eq!(
            buffer.find_recent_word_by_prefix("anot", None, &[], 4),
            Some("another".to_string())
        );

        // Test case-insensitivity
        assert_eq!(
            buffer.find_recent_word_by_prefix("PREFIX", None, &[], 4),
            Some("prefix_again".to_string())
        );

        // Test not found
        assert_eq!(
            buffer.find_recent_word_by_prefix("nonexistent", None, &[], 4),
            None
        );

        // Test with skip_words
        let mut skip_set = HashSet::new();
        skip_set.insert("prefix_again".to_string());
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", Some(&skip_set), &[], 4),
            Some("prefix_found".to_string())
        );

        skip_set.insert("prefix_found".to_string());
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", Some(&skip_set), &[], 4),
            None
        ); // All "pref" words skipped

        // Test n_recent_lines
        assert_eq!(
            buffer.find_recent_word_by_prefix("hello", None, &[], 1),
            None
        ); // "hello" is not in the last line
        assert_eq!(
            buffer.find_recent_word_by_prefix("hello", None, &[], 4),
            Some("hello".to_string())
        ); // "hello" is in the last 4 lines
    }

    #[test]
    fn find_recent_word_handles_colon_segments() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl(
            "[SC:Order] [Rr'Kar:Awful] guard:Awful Mem:2 T:40 Exits:N(S)W>",
        ));
        buffer.push_line(sl("An alert militia guard misses Zurek with his slash."));

        assert_eq!(
            buffer.find_recent_word_by_prefix("sc", None, &[], 5),
            Some("SC".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("sc:", None, &[], 5),
            Some("SC:Order".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("rr", None, &[], 5),
            Some("Rr'Kar".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("gu", None, &[], 5),
            Some("guard".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("guard:", None, &[], 5),
            Some("guard:Awful".to_string())
        );

        buffer.push_line(sl("Half-orc's strike leaves a scratch-!"));
        assert_eq!(
            buffer.find_recent_word_by_prefix("half", None, &[], 5),
            Some("Half-orc".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("half-orc'", None, &[], 5),
            Some("Half-orc's".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("scr", None, &[], 5),
            Some("scratch".to_string())
        );
    }
}
