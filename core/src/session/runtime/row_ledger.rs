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
    collections::VecDeque,
    rc::Rc,
    sync::Arc,
};

use super::pane::MAIN_PANE_KEY;
use crate::session::{
    BufferUpdate,
    styled_line::{LineFragments, StyledLine},
};

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
}

impl RowLedger {
    pub(crate) fn new(count: Rc<Cell<usize>>, ring: RecentLines) -> Self {
        Self {
            count,
            ring,
            open: LineFragments::None,
            row_open: false,
        }
    }

    /// A row appended before the ledger existed (the spawn-time "Loading
    /// session..." notice) that is still open.
    pub(crate) fn seed_open_row(&mut self, line: Arc<StyledLine>) {
        self.open.push(line);
        self.row_open = true;
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
            }
            BufferUpdate::FinishOpenLineReplacement(None)
            | BufferUpdate::PromptBoundary
            | BufferUpdate::AppendTo(..)
            | BufferUpdate::Clear(_) => {}
        }
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
