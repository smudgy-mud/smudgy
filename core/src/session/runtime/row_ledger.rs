//! The main pane's row ledger: core's account of what the terminal's main
//! scrollback physically holds — its committed-row count, whether its tail
//! row is open, and the fragments of each recent row — folded from the very
//! `BufferUpdate` stream the UI consumes.
//!
//! The UI's `TerminalBuffer` and the session log both derive their row model
//! by folding that stream. The ledger doing the same is what makes core's
//! line numbering and `buffer.line(n)` agree with the screen by construction,
//! rather than by bookkeeping at every emit site. A row's fragments are kept
//! as the `Arc`s the UI holds; joining is deferred to a script read and
//! memoised, so a prompt row completed by later server text costs the inbound
//! path no copy.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
};

use super::pane::MAIN_PANE_KEY;
use crate::session::{
    BufferUpdate,
    styled_line::{LineFragments, StyledLine},
    system_row::{Progress, SystemRow, SystemRowId},
};

/// How many system rows stay addressable for replacement. Far more than a
/// session emits in practice; the cap only bounds a pathological reload loop.
const TRACKED_SYSTEM_ROWS: usize = 256;

/// How many of the most recently committed main rows the session keeps a
/// readable copy of. This is a deliberate, documented bound — `buffer.line(n)`
/// reads (text + styles) and the write-through resolve within this window
/// only; a line number older than the window reads as `undefined` from
/// script. The stored copies are the *same* `Arc<StyledLine>`s already handed
/// to the UI (no data duplication, no silent unlimited scrollback). 1000
/// covers any realistic "edit a line I just saw" use without pinning the whole
/// UI scrollback (10k) on the session thread.
pub(crate) const RECENT_LINES: usize = 1000;

/// The recent-rows ring: each entry is a UI line number paired with the row's
/// fragments (the same `Arc`s the UI holds, joined lazily on read). Shared —
/// the same `Rc` — into every isolate's ops so `op_smudgy_buffer_get_text`/
/// `_styles` read it; written by [`RowLedger`] at commit time and by the
/// `buffer` write-through. Numbers ascend front to back (a cleared row leaves
/// a gap, never a reversal), so a lookup is a binary search.
pub(crate) type RecentLines = Rc<RefCell<VecDeque<(usize, LineFragments)>>>;

/// Core's fold over the main pane's update stream. See the module docs.
pub(crate) struct RowLedger {
    /// UI number of the last committed main row. Shared (weakly) into the
    /// ops for `getCurrentLineNumber`, which reports the open row as
    /// `count + 1`.
    count: Rc<Cell<usize>>,
    ring: RecentLines,
    /// Fragments of the open tail row, in display order.
    open: LineFragments,
    /// Whether the terminal's tail row is open: it has consumed a line number
    /// but not been committed. The inverse of the UI's `line_terminated`.
    row_open: bool,
    /// Where each system row this ledger folded landed, and how it last
    /// reported itself. A replacement edits that ring entry in place; a
    /// replacement for a row that is no longer addressable changes nothing at
    /// all — `ReplaceSystem` never consumes a line number, so the UI dropping
    /// one it no longer holds cannot put the two out of step.
    system_rows: HashMap<SystemRowId, (usize, Progress)>,
}

impl RowLedger {
    pub(crate) fn new(count: Rc<Cell<usize>>, ring: RecentLines) -> Self {
        Self {
            count,
            ring,
            open: LineFragments::None,
            row_open: false,
            system_rows: HashMap::new(),
        }
    }

    /// A system row appended before the ledger existed — the spawn-time
    /// session heading, and the load row the runtime thread emits around
    /// engine construction — already counted, and landing at `number`.
    /// Records it in the ring and makes it addressable for replacement.
    pub(crate) fn seed_system_row(&mut self, row: &Arc<SystemRow>, number: usize) {
        self.ring
            .borrow_mut()
            .push_back((number, LineFragments::One(row.line.clone())));
        self.track_system_row(row, number);
    }

    /// How the system row `id` last reported itself, or `None` when this
    /// ledger never folded it (or has since forgotten it).
    pub(crate) fn system_row_progress(&self, id: SystemRowId) -> Option<Progress> {
        self.system_rows.get(&id).map(|(_, progress)| *progress)
    }

    /// Whether the system row `id` is still the transcript's last row —
    /// nothing has been written under it. A row that still sits at the bottom
    /// can be updated in place and be seen; one that has scrolled away under
    /// output needs a fresh row instead.
    pub(crate) fn is_last_row(&self, id: SystemRowId) -> bool {
        !self.row_open
            && self
                .system_rows
                .get(&id)
                .is_some_and(|(number, _)| *number == self.count.get())
    }

    /// Record where a system row landed, forgetting rows that have fallen out
    /// of the readable window once the map grows past its cap.
    fn track_system_row(&mut self, row: &Arc<SystemRow>, number: usize) {
        if self.system_rows.len() >= TRACKED_SYSTEM_ROWS {
            let oldest = self.ring.borrow().front().map_or(0, |(number, _)| *number);
            self.system_rows.retain(|_, (number, _)| *number >= oldest);
        }
        self.system_rows.insert(row.id, (number, row.progress));
    }

    pub(crate) fn row_open(&self) -> bool {
        self.row_open
    }

    pub(crate) fn ring(&self) -> &RecentLines {
        &self.ring
    }

    /// Rows were committed behind the ledger's back — engine-construction
    /// notices write straight to the UI, each ending in `EnsureNewLine`, and
    /// bump the count themselves. Whatever the tail row held is committed
    /// now, unrecorded here; the ledger only needs to know the row is closed.
    pub(crate) fn close_row(&mut self) {
        self.open.clear();
        self.row_open = false;
    }

    /// Fold one update the runtime is about to queue for the UI. Every
    /// main-pane update passes through here before it is queued, so the
    /// ledger is never behind the stream a script may read against.
    pub(crate) fn observe(&mut self, update: &BufferUpdate) {
        match update {
            BufferUpdate::Append(line) | BufferUpdate::FinishOpenLineReplacement(Some(line)) => {
                self.open.push(line.clone());
                self.row_open = true;
            }
            BufferUpdate::EnsureNewLine => {
                if self.row_open {
                    self.commit_row();
                }
            }
            BufferUpdate::AppendSystem(row) => self.append_system_row(row),
            // Never a line number: the row either edits where it already sits
            // or, like the UI's copy, goes nowhere.
            BufferUpdate::ReplaceSystem(row) => {
                if let Some((number, progress)) = self.system_rows.get_mut(&row.id) {
                    *progress = row.progress;
                    let number = *number;
                    edit_ring_row(&self.ring, number, |_| row.line.clone());
                }
            }
            BufferUpdate::RetractOpenLine | BufferUpdate::BeginOpenLineReplacement => {
                self.close_row();
            }
            BufferUpdate::Clear(key) if *key == MAIN_PANE_KEY => {
                // The open row vanishes with the clear. The UI consumed a
                // number when the row opened, so account for it as
                // committed-then-cleared — counted, with nothing to read
                // back — to keep the numbering in step.
                if self.row_open {
                    self.count.set(self.count.get() + 1);
                    self.close_row();
                }
                // Cleared rows are gone from the screen, so nothing can still
                // replace them.
                self.system_rows.clear();
            }
            BufferUpdate::FinishOpenLineReplacement(None)
            | BufferUpdate::PromptBoundary
            | BufferUpdate::AppendTo(..)
            | BufferUpdate::Clear(_) => {}
        }
    }

    /// A system row is one whole committed line. An open tail row is
    /// committed first — the UI closes it the same way — so the numbering
    /// stays in step even for a producer that skipped the commit.
    fn append_system_row(&mut self, row: &Arc<SystemRow>) {
        if self.row_open {
            self.commit_row();
        }
        self.open.push(row.line.clone());
        self.row_open = true;
        self.commit_row();
        self.track_system_row(row, self.count.get());
    }

    fn commit_row(&mut self) {
        let number = self.count.get() + 1;
        self.count.set(number);
        let fragments = std::mem::take(&mut self.open);
        self.row_open = false;
        let mut ring = self.ring.borrow_mut();
        if ring.len() >= RECENT_LINES {
            ring.pop_front();
        }
        ring.push_back((number, fragments));
    }
}

/// Row `line_number` of `ring`, joined and memoised, or `None` when it is
/// outside the window (or was cleared).
pub(crate) fn ring_row(ring: &RecentLines, line_number: usize) -> Option<Arc<StyledLine>> {
    let mut ring = ring.borrow_mut();
    let index = ring.partition_point(|(number, _)| *number < line_number);
    let entry = ring.get_mut(index)?;
    if entry.0 != line_number {
        return None;
    }
    entry.1.joined()
}

/// Replace row `line_number` of `ring` with `edit(row)`, when the row is in
/// the window. The write-through for `PerformLineOperation`: the same edit
/// the UI applies, so a later `buffer.line(n)` reflects it.
pub(crate) fn edit_ring_row(
    ring: &RecentLines,
    line_number: usize,
    edit: impl FnOnce(&Arc<StyledLine>) -> Arc<StyledLine>,
) {
    let mut ring = ring.borrow_mut();
    let index = ring.partition_point(|(number, _)| *number < line_number);
    if let Some(entry) = ring.get_mut(index)
        && entry.0 == line_number
        && let Some(row) = entry.1.joined()
    {
        entry.1 = LineFragments::One(edit(&row));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::runtime::pane::PaneKey;

    fn line(text: &str) -> Arc<StyledLine> {
        Arc::new(StyledLine::from_output_str(text))
    }

    fn ledger() -> (RowLedger, Rc<Cell<usize>>, RecentLines) {
        let count = Rc::new(Cell::new(0));
        let ring: RecentLines = Rc::new(RefCell::new(VecDeque::new()));
        (RowLedger::new(count.clone(), ring.clone()), count, ring)
    }

    fn text(ring: &RecentLines, n: usize) -> Option<String> {
        ring_row(ring, n).map(|row| row.text.clone())
    }

    #[test]
    fn system_rows_count_once_and_finish_in_place() {
        use crate::session::system_row::{Progress, Severity, SystemRow, SystemText};
        let (mut ledger, count, ring) = ledger();
        ledger.observe(&BufferUpdate::Append(line("partial")));
        let pending = SystemRow::notice(
            42,
            Progress::Pending,
            SystemText::new(Severity::Info).text("Loading maps…"),
        );
        ledger.observe(&BufferUpdate::AppendSystem(pending));
        // The open partial was committed first, then the row took a number.
        assert_eq!(count.get(), 2);
        assert_eq!(text(&ring, 1).as_deref(), Some("partial"));
        assert_eq!(text(&ring, 2).as_deref(), Some("Loading maps…"));
        assert_eq!(ledger.system_row_progress(42), Some(Progress::Pending));

        ledger.observe(&BufferUpdate::Append(line("server text")));
        ledger.observe(&BufferUpdate::EnsureNewLine);
        let done = SystemRow::notice(
            42,
            Progress::Done,
            SystemText::new(Severity::Info).text("Loaded 3 map areas"),
        );
        ledger.observe(&BufferUpdate::ReplaceSystem(done));
        // Finishing edits row 2 in place: no new number, and the ring reads
        // back the finished text.
        assert_eq!(count.get(), 3);
        assert_eq!(text(&ring, 2).as_deref(), Some("Loaded 3 map areas"));
        assert_eq!(ledger.system_row_progress(42), Some(Progress::Done));
    }

    #[test]
    fn a_replacement_for_an_unknown_row_changes_nothing() {
        use crate::session::system_row::{Progress, Severity, SystemRow, SystemText};
        let (mut ledger, count, ring) = ledger();
        ledger.observe(&BufferUpdate::Append(line("one")));
        ledger.observe(&BufferUpdate::EnsureNewLine);
        // A row this ledger never folded — or one a `clear()` retired — takes
        // no line number, because the UI drops the very same update.
        ledger.observe(&BufferUpdate::ReplaceSystem(SystemRow::notice(
            777,
            Progress::Done,
            SystemText::new(Severity::Info).text("late finish"),
        )));
        assert_eq!(count.get(), 1);
        assert_eq!(text(&ring, 1).as_deref(), Some("one"));
        assert_eq!(ledger.system_row_progress(777), None);
    }

    #[test]
    fn a_seeded_session_row_is_replaceable_after_it_finishes() {
        use crate::session::system_row::{
            Progress, SESSION_ROW_ID, Severity, SystemRow, SystemText,
        };
        let (mut ledger, count, ring) = ledger();
        count.set(1);
        ledger.seed_system_row(&SystemRow::loading_session(), 1);
        assert_eq!(
            ledger.system_row_progress(SESSION_ROW_ID),
            Some(Progress::Pending)
        );
        ledger.observe(&BufferUpdate::ReplaceSystem(SystemRow::heading_rule(
            SESSION_ROW_ID,
            Progress::Done,
            SystemText::new(Severity::Info).text("kapusta on arctic"),
        )));
        assert_eq!(count.get(), 1);
        assert_eq!(text(&ring, 1).as_deref(), Some("kapusta on arctic"));
        assert_eq!(
            ledger.system_row_progress(SESSION_ROW_ID),
            Some(Progress::Done)
        );

        // A finished row stays addressable: the connection rule cycles
        // offline → connecting → connected on one row.
        ledger.observe(&BufferUpdate::ReplaceSystem(SystemRow::heading_rule(
            SESSION_ROW_ID,
            Progress::Done,
            SystemText::new(Severity::Info).text("kapusta on arctic, connected"),
        )));
        assert_eq!(count.get(), 1);
        assert_eq!(
            text(&ring, 1).as_deref(),
            Some("kapusta on arctic, connected")
        );
    }

    #[test]
    fn whole_lines_count_and_record_in_order() {
        let (mut ledger, count, ring) = ledger();
        for word in ["one", "two"] {
            ledger.observe(&BufferUpdate::Append(line(word)));
            ledger.observe(&BufferUpdate::EnsureNewLine);
        }
        assert_eq!(count.get(), 2);
        assert!(!ledger.row_open());
        assert_eq!(text(&ring, 1).as_deref(), Some("one"));
        assert_eq!(text(&ring, 2).as_deref(), Some("two"));
        assert_eq!(text(&ring, 3), None);
        assert_eq!(text(&ring, 0), None);
    }

    #[test]
    fn a_prompt_row_glued_by_later_text_reads_back_as_one_physical_row() {
        let (mut ledger, count, ring) = ledger();
        ledger.observe(&BufferUpdate::Append(line("HP:10> ")));
        assert!(ledger.row_open());
        assert_eq!(count.get(), 0);
        ledger.observe(&BufferUpdate::PromptBoundary);
        ledger.observe(&BufferUpdate::Append(line("look")));
        ledger.observe(&BufferUpdate::EnsureNewLine);
        assert_eq!(count.get(), 1);
        assert_eq!(text(&ring, 1).as_deref(), Some("HP:10> look"));
    }

    #[test]
    fn ensure_new_line_on_a_closed_row_consumes_nothing() {
        let (mut ledger, count, ring) = ledger();
        ledger.observe(&BufferUpdate::EnsureNewLine);
        ledger.observe(&BufferUpdate::EnsureNewLine);
        assert_eq!(count.get(), 0);
        assert!(ring.borrow().is_empty());
    }

    #[test]
    fn retract_and_replacement_follow_the_ui() {
        let (mut ledger, count, ring) = ledger();
        ledger.observe(&BufferUpdate::Append(line("partial")));
        ledger.observe(&BufferUpdate::RetractOpenLine);
        assert!(!ledger.row_open());
        assert_eq!(count.get(), 0);

        ledger.observe(&BufferUpdate::Append(line("old")));
        ledger.observe(&BufferUpdate::BeginOpenLineReplacement);
        assert!(!ledger.row_open());
        ledger.observe(&BufferUpdate::FinishOpenLineReplacement(Some(line("new"))));
        assert!(ledger.row_open());
        ledger.observe(&BufferUpdate::EnsureNewLine);
        assert_eq!(text(&ring, 1).as_deref(), Some("new"));

        ledger.observe(&BufferUpdate::Append(line("gone")));
        ledger.observe(&BufferUpdate::BeginOpenLineReplacement);
        ledger.observe(&BufferUpdate::FinishOpenLineReplacement(None));
        assert!(!ledger.row_open());
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn clearing_main_counts_the_open_row_without_recording_it() {
        let (mut ledger, count, ring) = ledger();
        ledger.observe(&BufferUpdate::Append(line("kept")));
        ledger.observe(&BufferUpdate::EnsureNewLine);
        ledger.observe(&BufferUpdate::Append(line("open")));
        ledger.observe(&BufferUpdate::Clear(MAIN_PANE_KEY));
        assert_eq!(count.get(), 2);
        assert!(!ledger.row_open());
        assert_eq!(text(&ring, 1).as_deref(), Some("kept"));
        assert_eq!(text(&ring, 2), None);
        ledger.observe(&BufferUpdate::Append(line("after")));
        ledger.observe(&BufferUpdate::EnsureNewLine);
        assert_eq!(text(&ring, 3).as_deref(), Some("after"));
    }

    #[test]
    fn pane_updates_leave_main_untouched() {
        let (mut ledger, count, ring) = ledger();
        ledger.observe(&BufferUpdate::Append(line("open")));
        let pane = PaneKey::from_raw_for_tests(7);
        ledger.observe(&BufferUpdate::AppendTo(pane, line("pane")));
        ledger.observe(&BufferUpdate::Clear(pane));
        assert!(ledger.row_open());
        assert_eq!(count.get(), 0);
        assert!(ring.borrow().is_empty());
    }

    #[test]
    fn ring_is_bounded_and_the_write_through_edits_in_place() {
        let (mut ledger, _count, ring) = ledger();
        for n in 0..(RECENT_LINES + 5) {
            ledger.observe(&BufferUpdate::Append(line(&format!("row {n}"))));
            ledger.observe(&BufferUpdate::EnsureNewLine);
        }
        assert_eq!(ring.borrow().len(), RECENT_LINES);
        assert_eq!(text(&ring, 1), None);
        assert_eq!(text(&ring, 6).as_deref(), Some("row 5"));
        edit_ring_row(&ring, 6, |row| Arc::new(row.remove(0, 4)));
        assert_eq!(text(&ring, 6).as_deref(), Some("5"));
        edit_ring_row(&ring, 1, |_| unreachable!("outside the window"));
    }
}
