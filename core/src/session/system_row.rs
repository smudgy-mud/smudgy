//! Rows the client speaks in its own voice.
//!
//! Server output, script echoes and the client's own notices all become
//! [`StyledLine`]s, and until now nothing downstream could tell them apart —
//! the client's voice was a colour role. A [`SystemRow`] is a main-buffer row
//! the client authored: a loading notice, a session rule, a collapsible group
//! of what loaded. It still carries exactly one [`StyledLine`], its **text
//! projection**, and that projection is what every text consumer sees —
//! copy, search, the session log, the recent-lines ring a script reads with
//! `buffer.line(n)`, and the row ledger that numbers lines. Only the terminal
//! renderer looks past the projection at the kind, severity, progress and
//! chips, and draws the row in the application font with a gutter bar.
//!
//! A row occupies exactly one logical line, whatever it draws: a group's
//! children are display-only detail under its one-line summary. That single
//! rule is what lets the projection stand in for the row everywhere else.
//!
//! Rows are identified so an in-progress row ("Loading maps…") can be
//! replaced in place when its work finishes; see
//! [`crate::session::BufferUpdate::ReplaceSystem`]. Ids are unique per
//! process, so a session buffer never sees two rows sharing one, and
//! [`SESSION_ROW_ID`] is reserved for the spawn-time session rule that the
//! runtime finishes once it is up.

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use super::{
    connection::vt_processor::Color,
    styled_line::{LinkAction, Style, StyledLine, StyledLink, sanitize_display_text},
};

/// Identity of one [`SystemRow`] within a session's main buffer.
pub type SystemRowId = u64;

/// The id of the session rule appended at spawn, before the runtime exists.
/// The runtime replaces it with the finished rule once it is running.
pub const SESSION_ROW_ID: SystemRowId = 1;

static NEXT_ID: AtomicU64 = AtomicU64::new(SESSION_ROW_ID + 1);

/// Mint a fresh row id. Process-unique, so it is safe in every session.
#[must_use]
pub fn next_system_row_id() -> SystemRowId {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// How loudly the row speaks: the renderer picks the gutter and text colour
/// from it (the `echo` and `warn` palette roles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
}

impl Severity {
    /// The colour role the row's projection is written in.
    #[must_use]
    pub const fn color(self) -> Color {
        match self {
            Self::Info => Color::Echo,
            Self::Warn => Color::Warn,
        }
    }
}

/// Whether the row describes work still under way. A pending row shimmers
/// and is expected to be replaced (same id) when the work completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Pending,
    Done,
}

/// One display-only detail line under a group's summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemChild {
    pub severity: Severity,
    pub line: Arc<StyledLine>,
    /// Byte ranges of `line.text` drawn as chips.
    pub chips: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemRowKind {
    /// One line in the client's voice.
    Notice,
    /// A labelled horizontal rule: a boundary in the transcript (a session
    /// opening, a connection ending). A `prominent` rule is the session's own
    /// heading and is set larger than the rest of the transcript.
    Rule { prominent: bool },
    /// A one-line summary that can unfold into detail lines.
    Group {
        children: Vec<SystemChild>,
        /// Whether the group starts unfolded. The viewer can toggle it.
        expanded: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemRow {
    pub id: SystemRowId,
    pub kind: SystemRowKind,
    pub severity: Severity,
    pub progress: Progress,
    /// The row's one-line text projection: what copy, search, scripts and the
    /// log see. Its `links` are the row's links.
    pub line: Arc<StyledLine>,
    /// Byte ranges of `line.text` drawn as chips (pills).
    pub chips: Vec<Range<usize>>,
}

impl SystemRow {
    /// The pending session heading appended at spawn. Built identically by
    /// the spawn path (which sends it) and the runtime (which seeds its
    /// ledger with it), so both agree on what row [`SESSION_ROW_ID`] holds.
    #[must_use]
    pub fn loading_session() -> Arc<Self> {
        Self::heading_rule(
            SESSION_ROW_ID,
            Progress::Pending,
            SystemText::new(Severity::Info).text("Loading session\u{2026}"),
        )
    }

    #[must_use]
    pub fn notice(id: SystemRowId, progress: Progress, text: SystemText) -> Arc<Self> {
        Self::build(id, SystemRowKind::Notice, progress, text)
    }

    #[must_use]
    pub fn rule(id: SystemRowId, progress: Progress, text: SystemText) -> Arc<Self> {
        Self::build(id, SystemRowKind::Rule { prominent: false }, progress, text)
    }

    /// The session's own heading rule: the same shape, set larger.
    #[must_use]
    pub fn heading_rule(id: SystemRowId, progress: Progress, text: SystemText) -> Arc<Self> {
        Self::build(id, SystemRowKind::Rule { prominent: true }, progress, text)
    }

    /// A group starts unfolded when any child warns, so a warning is never
    /// hidden behind a collapsed summary.
    #[must_use]
    pub fn group(
        id: SystemRowId,
        progress: Progress,
        text: SystemText,
        children: Vec<SystemChild>,
    ) -> Arc<Self> {
        let expanded = children
            .iter()
            .any(|child| child.severity == Severity::Warn);
        Self::build(
            id,
            SystemRowKind::Group { children, expanded },
            progress,
            text,
        )
    }

    /// A finished one-line notice from plain text, freshly identified. A
    /// leading `[tag] ` (the `[package]`/`[interop]` convention of the
    /// runtime's diagnostics) becomes a chip reading `tag`.
    #[must_use]
    pub fn plain_notice(severity: Severity, message: &str) -> Arc<Self> {
        let mut text = SystemText::new(severity);
        if let Some(rest) = message.strip_prefix('[')
            && let Some((tag, body)) = rest.split_once("] ")
            && !tag.is_empty()
            && !tag.contains(['[', ']'])
        {
            text = text.chip(tag, None).text(" ").text(body);
        } else {
            text = text.text(message);
        }
        Self::notice(next_system_row_id(), Progress::Done, text)
    }

    fn build(
        id: SystemRowId,
        kind: SystemRowKind,
        progress: Progress,
        text: SystemText,
    ) -> Arc<Self> {
        let severity = text.severity;
        let (line, chips) = text.into_line_and_chips();
        Arc::new(Self {
            id,
            kind,
            severity,
            progress,
            line,
            chips,
        })
    }

    /// The display-only children of a group; empty for other kinds.
    #[must_use]
    pub fn children(&self) -> &[SystemChild] {
        match &self.kind {
            SystemRowKind::Group { children, .. } => children,
            _ => &[],
        }
    }

    /// Whether a group starts unfolded. `false` for other kinds.
    #[must_use]
    pub fn expanded_by_default(&self) -> bool {
        matches!(self.kind, SystemRowKind::Group { expanded: true, .. })
    }

    #[must_use]
    pub fn is_group(&self) -> bool {
        matches!(self.kind, SystemRowKind::Group { .. })
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.progress == Progress::Pending
    }

    /// Whether this row is the session's heading, set larger than the rest.
    #[must_use]
    pub fn is_prominent(&self) -> bool {
        matches!(self.kind, SystemRowKind::Rule { prominent: true })
    }
}

/// Builder for a row's (or child's) text: plain runs, emphasised runs, chips
/// and links, accumulated into one projection [`StyledLine`] plus chip
/// ranges. Every run is display-bound text that never met the VT parser, so
/// control characters are stripped as they enter.
#[derive(Debug, Clone)]
pub struct SystemText {
    severity: Severity,
    runs: Vec<(String, Style, Option<LinkAction>)>,
    chips: Vec<Range<usize>>,
    len: usize,
}

impl SystemText {
    #[must_use]
    pub fn new(severity: Severity) -> Self {
        Self {
            severity,
            runs: Vec::new(),
            chips: Vec::new(),
            len: 0,
        }
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    fn base_style(&self) -> Style {
        Style {
            fg: self.severity.color(),
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
        }
    }

    fn push(&mut self, text: &str, style: Style, link: Option<LinkAction>) -> Range<usize> {
        let text = sanitize_display_text(text);
        let begin = self.len;
        self.len += text.len();
        if !text.is_empty() {
            self.runs.push((text.into_owned(), style, link));
        }
        begin..self.len
    }

    /// Body text.
    #[must_use]
    pub fn text(mut self, text: &str) -> Self {
        let style = self.base_style();
        self.push(text, style, None);
        self
    }

    /// Emphasised text: the counts and names a reader scans for.
    #[must_use]
    pub fn strong(mut self, text: &str) -> Self {
        let mut style = self.base_style();
        style.attributes.bold = true;
        self.push(text, style, None);
        self
    }

    /// De-emphasised text: durations, versions, asides.
    #[must_use]
    pub fn muted(mut self, text: &str) -> Self {
        let mut style = self.base_style();
        style.attributes.faint = true;
        self.push(text, style, None);
        self
    }

    /// A chip: `label` with an optional muted `detail` directly after it (a
    /// version, say), the two drawn as one pill.
    #[must_use]
    pub fn chip(mut self, label: &str, detail: Option<&str>) -> Self {
        let style = self.base_style();
        let range = self.push(label, style, None);
        let mut end = range.end;
        if let Some(detail) = detail {
            let mut muted = style;
            muted.attributes.faint = true;
            end = self.push(detail, muted, None).end;
        }
        if range.start < end {
            self.chips.push(range.start..end);
        }
        self
    }

    /// A clickable run.
    #[must_use]
    pub fn link(mut self, text: &str, action: LinkAction) -> Self {
        let style = self.base_style();
        self.push(text, style, Some(action));
        self
    }

    /// The projection line and its chip ranges.
    #[must_use]
    pub fn into_line_and_chips(self) -> (Arc<StyledLine>, Vec<Range<usize>>) {
        let empty_style = self.base_style();
        let runs: Vec<(&str, Style, Option<StyledLink>)> = self
            .runs
            .iter()
            .map(|(text, style, action)| {
                (
                    text.as_str(),
                    *style,
                    action.clone().map(|action| StyledLink {
                        action,
                        tooltip: None,
                        style: None,
                    }),
                )
            })
            .collect();
        (
            Arc::new(StyledLine::from_linked_runs(&runs, empty_style)),
            self.chips,
        )
    }

    /// Finish as one display-only line of a group.
    #[must_use]
    pub fn into_child(self) -> SystemChild {
        let severity = self.severity;
        let (line, chips) = self.into_line_and_chips();
        SystemChild {
            severity,
            line,
            chips,
        }
    }
}

/// `"1 thing"` / `"2 things"`.
#[must_use]
pub fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// One clause of the load row: `**3 packages** (in 120ms)`, emphasised count
/// and muted timing. A zero count contributes nothing, so a session with no
/// packages simply never mentions them.
pub fn push_loaded_clause(
    text: SystemText,
    count: usize,
    singular: &str,
    plural_form: &str,
    elapsed: Option<std::time::Duration>,
    first: &mut bool,
) -> SystemText {
    if count == 0 {
        return text;
    }
    let text = if *first {
        *first = false;
        text.text("Loaded ")
    } else {
        text.text(", ")
    };
    let text = text.strong(&plural(count, singular, plural_form));
    match elapsed {
        Some(elapsed) => text.muted(&format!(" (in {}ms)", elapsed.as_millis())),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::super::styled_line::AppLink;
    use super::*;

    #[test]
    fn text_builder_tiles_runs_and_records_chips() {
        let text = SystemText::new(Severity::Info)
            .text("Loaded ")
            .strong("6")
            .text(" packages: ")
            .chip("arctic-mapper", Some("@0.1.1"))
            .text(" ")
            .link("Configure", LinkAction::App(AppLink::OpenSettings));
        let (line, chips) = text.into_line_and_chips();
        assert_eq!(
            line.text,
            "Loaded 6 packages: arctic-mapper@0.1.1 Configure"
        );
        assert_eq!(chips, vec![19..38]);
        assert_eq!(line.links.len(), 1);
        assert_eq!(
            &line.text[line.links[0].begin_pos..line.links[0].end_pos],
            "Configure"
        );
        // Spans tile the text gap-free.
        let mut cursor = 0;
        for span in &line.spans {
            assert_eq!(span.begin_pos, cursor);
            cursor = span.end_pos;
        }
        assert_eq!(cursor, line.text.len());
        assert!(line.spans[1].style.attributes.bold);
    }

    #[test]
    fn plain_notice_turns_a_bracket_tag_into_a_chip() {
        let row = SystemRow::plain_notice(Severity::Warn, "[package] foo not loaded");
        assert_eq!(row.line.text, "package foo not loaded");
        assert_eq!(row.chips, vec![0..7]);
        assert_eq!(row.severity, Severity::Warn);
        assert_eq!(row.line.spans[0].style.fg, Color::Warn);

        let plain = SystemRow::plain_notice(Severity::Info, "Reloading scripts...");
        assert_eq!(plain.line.text, "Reloading scripts...");
        assert!(plain.chips.is_empty());
    }

    #[test]
    fn control_characters_are_stripped_from_every_run() {
        let (line, _) = SystemText::new(Severity::Info)
            .text("a\u{1b}b")
            .strong("c\rd")
            .into_line_and_chips();
        assert_eq!(line.text, "abcd");
    }

    #[test]
    fn a_group_unfolds_by_default_only_when_a_child_warns() {
        let quiet = SystemRow::group(
            7,
            Progress::Done,
            SystemText::new(Severity::Info).text("Loaded 1 package"),
            vec![SystemText::new(Severity::Info).chip("a", None).into_child()],
        );
        assert!(!quiet.expanded_by_default());
        let loud = SystemRow::group(
            8,
            Progress::Done,
            SystemText::new(Severity::Info).text("Loaded 1 package"),
            vec![
                SystemText::new(Severity::Warn)
                    .text("b skipped")
                    .into_child(),
            ],
        );
        assert!(loud.expanded_by_default());
        assert_eq!(loud.children().len(), 1);
    }

    #[test]
    fn ids_are_unique_and_never_reuse_the_session_row() {
        let a = next_system_row_id();
        let b = next_system_row_id();
        assert_ne!(a, b);
        assert_ne!(a, SESSION_ROW_ID);
        assert_ne!(b, SESSION_ROW_ID);
    }
}
