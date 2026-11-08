//! Unicode placeholders for the kitty graphics protocol: the placeholder
//! character, the row/column diacritic table, and the grouping of
//! placeholder cells into runs.
//!
//! A client that cannot (or would rather not) address the terminal directly
//! prints `U+10EEEE` cells carrying an image id in their foreground colour,
//! an optional placement id in their underline colour, and up to three
//! combining diacritics naming the row, the column and the id's most
//! significant byte. The terminal draws, over each horizontal run of such
//! cells, the matching slice of a *virtual placement* (`a=p,U=1`). The
//! processor stamps each placeholder as it is printed and hands the cells to
//! [`RunBuilder`] when the line is built; the state machine turns each run
//! into a placement ([`super::GraphicsState::placeholder_run`]).
//!
//! The diacritic table is the specification's `rowcolumn-diacritics.txt`,
//! verbatim and in order: the first entry means 0, the next 1, and so on.
//! Specification: <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders>.

use std::ops::Range;

/// The placeholder character, `U+10EEEE`.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// [`PLACEHOLDER`] encoded as UTF-8, for byte searches.
pub const PLACEHOLDER_UTF8: [u8; 4] = [0xF4, 0x8E, 0xBB, 0xAE];

/// The diacritics that name a row, a column or an id byte on a placeholder
/// cell, in the specification's order: entry `n` means the number `n`.
/// Ascending, so [`diacritic_number`] can search it.
pub const ROW_COLUMN_DIACRITICS: [char; 297] = [
    '\u{0305}',
    '\u{030d}',
    '\u{030e}',
    '\u{0310}',
    '\u{0312}',
    '\u{033d}',
    '\u{033e}',
    '\u{033f}',
    '\u{0346}',
    '\u{034a}',
    '\u{034b}',
    '\u{034c}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035b}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036a}',
    '\u{036b}',
    '\u{036c}',
    '\u{036d}',
    '\u{036e}',
    '\u{036f}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059c}',
    '\u{059d}',
    '\u{059e}',
    '\u{059f}',
    '\u{05a0}',
    '\u{05a1}',
    '\u{05a8}',
    '\u{05a9}',
    '\u{05ab}',
    '\u{05ac}',
    '\u{05af}',
    '\u{05c4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065a}',
    '\u{065b}',
    '\u{065d}',
    '\u{065e}',
    '\u{06d6}',
    '\u{06d7}',
    '\u{06d8}',
    '\u{06d9}',
    '\u{06da}',
    '\u{06db}',
    '\u{06dc}',
    '\u{06df}',
    '\u{06e0}',
    '\u{06e1}',
    '\u{06e2}',
    '\u{06e4}',
    '\u{06e7}',
    '\u{06e8}',
    '\u{06eb}',
    '\u{06ec}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073a}',
    '\u{073d}',
    '\u{073f}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074a}',
    '\u{07eb}',
    '\u{07ec}',
    '\u{07ed}',
    '\u{07ee}',
    '\u{07ef}',
    '\u{07f0}',
    '\u{07f1}',
    '\u{07f3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081b}',
    '\u{081c}',
    '\u{081d}',
    '\u{081e}',
    '\u{081f}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082a}',
    '\u{082b}',
    '\u{082c}',
    '\u{082d}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0f82}',
    '\u{0f83}',
    '\u{0f86}',
    '\u{0f87}',
    '\u{135d}',
    '\u{135e}',
    '\u{135f}',
    '\u{17dd}',
    '\u{193a}',
    '\u{1a17}',
    '\u{1a75}',
    '\u{1a76}',
    '\u{1a77}',
    '\u{1a78}',
    '\u{1a79}',
    '\u{1a7a}',
    '\u{1a7b}',
    '\u{1a7c}',
    '\u{1b6b}',
    '\u{1b6d}',
    '\u{1b6e}',
    '\u{1b6f}',
    '\u{1b70}',
    '\u{1b71}',
    '\u{1b72}',
    '\u{1b73}',
    '\u{1cd0}',
    '\u{1cd1}',
    '\u{1cd2}',
    '\u{1cda}',
    '\u{1cdb}',
    '\u{1ce0}',
    '\u{1dc0}',
    '\u{1dc1}',
    '\u{1dc3}',
    '\u{1dc4}',
    '\u{1dc5}',
    '\u{1dc6}',
    '\u{1dc7}',
    '\u{1dc8}',
    '\u{1dc9}',
    '\u{1dcb}',
    '\u{1dcc}',
    '\u{1dd1}',
    '\u{1dd2}',
    '\u{1dd3}',
    '\u{1dd4}',
    '\u{1dd5}',
    '\u{1dd6}',
    '\u{1dd7}',
    '\u{1dd8}',
    '\u{1dd9}',
    '\u{1dda}',
    '\u{1ddb}',
    '\u{1ddc}',
    '\u{1ddd}',
    '\u{1dde}',
    '\u{1ddf}',
    '\u{1de0}',
    '\u{1de1}',
    '\u{1de2}',
    '\u{1de3}',
    '\u{1de4}',
    '\u{1de5}',
    '\u{1de6}',
    '\u{1dfe}',
    '\u{20d0}',
    '\u{20d1}',
    '\u{20d4}',
    '\u{20d5}',
    '\u{20d6}',
    '\u{20d7}',
    '\u{20db}',
    '\u{20dc}',
    '\u{20e1}',
    '\u{20e7}',
    '\u{20e9}',
    '\u{20f0}',
    '\u{2cef}',
    '\u{2cf0}',
    '\u{2cf1}',
    '\u{2de0}',
    '\u{2de1}',
    '\u{2de2}',
    '\u{2de3}',
    '\u{2de4}',
    '\u{2de5}',
    '\u{2de6}',
    '\u{2de7}',
    '\u{2de8}',
    '\u{2de9}',
    '\u{2dea}',
    '\u{2deb}',
    '\u{2dec}',
    '\u{2ded}',
    '\u{2dee}',
    '\u{2def}',
    '\u{2df0}',
    '\u{2df1}',
    '\u{2df2}',
    '\u{2df3}',
    '\u{2df4}',
    '\u{2df5}',
    '\u{2df6}',
    '\u{2df7}',
    '\u{2df8}',
    '\u{2df9}',
    '\u{2dfa}',
    '\u{2dfb}',
    '\u{2dfc}',
    '\u{2dfd}',
    '\u{2dfe}',
    '\u{2dff}',
    '\u{a66f}',
    '\u{a67c}',
    '\u{a67d}',
    '\u{a6f0}',
    '\u{a6f1}',
    '\u{a8e0}',
    '\u{a8e1}',
    '\u{a8e2}',
    '\u{a8e3}',
    '\u{a8e4}',
    '\u{a8e5}',
    '\u{a8e6}',
    '\u{a8e7}',
    '\u{a8e8}',
    '\u{a8e9}',
    '\u{a8ea}',
    '\u{a8eb}',
    '\u{a8ec}',
    '\u{a8ed}',
    '\u{a8ee}',
    '\u{a8ef}',
    '\u{a8f0}',
    '\u{a8f1}',
    '\u{aab0}',
    '\u{aab2}',
    '\u{aab3}',
    '\u{aab7}',
    '\u{aab8}',
    '\u{aabe}',
    '\u{aabf}',
    '\u{aac1}',
    '\u{fe20}',
    '\u{fe21}',
    '\u{fe22}',
    '\u{fe23}',
    '\u{fe24}',
    '\u{fe25}',
    '\u{fe26}',
    '\u{10a0f}',
    '\u{10a38}',
    '\u{1d185}',
    '\u{1d186}',
    '\u{1d187}',
    '\u{1d188}',
    '\u{1d189}',
    '\u{1d1aa}',
    '\u{1d1ab}',
    '\u{1d1ac}',
    '\u{1d1ad}',
    '\u{1d242}',
    '\u{1d243}',
    '\u{1d244}',
];

/// The number a row/column diacritic stands for; `None` for any other
/// character, which kitty reads as "unknown" too.
#[must_use]
pub fn diacritic_number(c: char) -> Option<u16> {
    ROW_COLUMN_DIACRITICS
        .binary_search(&c)
        .ok()
        .and_then(|index| u16::try_from(index).ok())
}

/// Whether `c` joins the placeholder cell before it, as kitty's cells take
/// combining marks: a row/column diacritic, or another mark of the
/// combining blocks — which names nothing, but is the cell's all the same.
#[must_use]
pub fn is_combining_mark(c: char) -> bool {
    diacritic_number(c).is_some()
        || matches!(
            c,
            '\u{0300}'..='\u{036F}'
                | '\u{1AB0}'..='\u{1AFF}'
                | '\u{1DC0}'..='\u{1DFF}'
                | '\u{20D0}'..='\u{20FF}'
                | '\u{FE20}'..='\u{FE2F}'
        )
}

/// One placeholder cell as the processor read it from the pending line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceholderCell {
    /// The cell's bytes in the line: the placeholder and the diacritics
    /// that follow it.
    pub range: Range<usize>,
    /// The cell's index on the line, counted in characters from its start.
    pub column: usize,
    /// The low 24 bits of the image id, from the foreground colour.
    pub image_low: u32,
    /// The placement id, from the underline colour; zero for any.
    pub placement_id: u32,
    /// The first diacritic: the row inside the virtual placement's box.
    pub row: Option<u16>,
    /// The second diacritic: the column inside the box.
    pub col: Option<u16>,
    /// The third diacritic: the image id's most significant byte.
    pub msb: Option<u16>,
}

/// A horizontal run of placeholder cells that show consecutive columns of
/// one row of one virtual placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceholderRun {
    /// The whole image id: the foreground's 24 bits under the third
    /// diacritic's byte.
    pub image_id: u32,
    /// The placement id the cells named; zero for any virtual placement of
    /// the image.
    pub placement_id: u32,
    /// The row of the box the run shows, from zero.
    pub row: u16,
    /// The box column the first cell shows, from zero.
    pub column: u16,
    /// Each cell's bytes in the line, left to right.
    pub cells: Vec<Range<usize>>,
    /// The line column (in characters) of the first cell.
    pub first_cell_column: usize,
}

impl PlaceholderRun {
    /// How many cells the run covers.
    #[must_use]
    pub fn len(&self) -> u16 {
        u16::try_from(self.cells.len()).unwrap_or(u16::MAX)
    }

    /// Whether the run has no cells (it never does once built).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The bytes of cells `skip..skip + count`, for attaching an object over
    /// part of the run.
    #[must_use]
    pub fn byte_range(&self, skip: usize, count: usize) -> Range<usize> {
        let first = &self.cells[skip];
        let last = &self.cells[skip + count - 1];
        first.start..last.end
    }
}

/// Groups placeholder cells into runs as kitty's line scan does: a cell
/// continues the run before it when it is the next cell on the line, names
/// the same image (low bits) and placement, and everything it does say
/// agrees with the cell to its left (same row, the next column, the same id
/// byte). What it leaves unsaid it inherits: the row, the column after the
/// previous cell's, the id byte. A cell starting a run reads a missing row
/// or column as zero and a missing id byte as zero.
#[derive(Debug, Default)]
pub struct RunBuilder {
    run: Option<Open>,
}

#[derive(Debug)]
struct Open {
    image_low: u32,
    placement_id: u32,
    row: u16,
    first_column: u16,
    msb: u16,
    /// The column the last cell shows.
    last_column: u16,
    cells: Vec<Range<usize>>,
    first_cell_column: usize,
    /// Where the last cell's bytes end: the next cell must start there.
    end: usize,
}

impl Open {
    fn finish(self) -> PlaceholderRun {
        PlaceholderRun {
            image_id: self.image_low | (u32::from(self.msb) & 0xff) << 24,
            placement_id: self.placement_id,
            row: self.row,
            column: self.first_column,
            cells: self.cells,
            first_cell_column: self.first_cell_column,
        }
    }
}

impl RunBuilder {
    /// Add the next placeholder cell on the line (cells arrive left to
    /// right). The run the cell ended, if any, is handed back.
    pub fn push(&mut self, cell: PlaceholderCell) -> Option<PlaceholderRun> {
        if let Some(open) = self.run.as_mut() {
            let next_column = open.last_column.checked_add(1);
            let continues = cell.range.start == open.end
                && cell.image_low == open.image_low
                && cell.placement_id == open.placement_id
                && cell.row.is_none_or(|row| row == open.row)
                && cell.col.is_none_or(|col| Some(col) == next_column)
                && cell.msb.is_none_or(|msb| msb == open.msb)
                && next_column.is_some();
            if continues {
                open.last_column = next_column.unwrap_or(u16::MAX);
                open.end = cell.range.end;
                open.cells.push(cell.range);
                return None;
            }
        }
        let finished = self.run.take().map(Open::finish);
        self.run = Some(Open {
            image_low: cell.image_low,
            placement_id: cell.placement_id,
            row: cell.row.unwrap_or(0),
            first_column: cell.col.unwrap_or(0),
            msb: cell.msb.unwrap_or(0),
            last_column: cell.col.unwrap_or(0),
            end: cell.range.end,
            cells: vec![cell.range],
            first_cell_column: cell.column,
        });
        finished
    }

    /// The run still open at the end of the line, if any.
    pub fn finish(&mut self) -> Option<PlaceholderRun> {
        self.run.take().map(Open::finish)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_diacritic_table_is_the_specification_s() {
        assert_eq!(ROW_COLUMN_DIACRITICS.len(), 297);
        assert_eq!(ROW_COLUMN_DIACRITICS[0], '\u{0305}');
        assert_eq!(ROW_COLUMN_DIACRITICS[1], '\u{030D}');
        assert_eq!(ROW_COLUMN_DIACRITICS[2], '\u{030E}');
        assert_eq!(ROW_COLUMN_DIACRITICS[296], '\u{1D244}');
        assert!(
            ROW_COLUMN_DIACRITICS
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "ascending, as the search needs"
        );
        assert_eq!(diacritic_number('\u{0305}'), Some(0));
        assert_eq!(diacritic_number('\u{030D}'), Some(1));
        assert_eq!(diacritic_number('\u{030E}'), Some(2));
        assert_eq!(diacritic_number('\u{1D244}'), Some(296));
        assert_eq!(
            diacritic_number('\u{0300}'),
            None,
            "a grave is not in the table"
        );
        assert_eq!(diacritic_number('a'), None);
        assert!(is_combining_mark('\u{0305}') && is_combining_mark('\u{1D244}'));
        assert!(
            is_combining_mark('\u{0301}'),
            "an acute names nothing but joins the cell"
        );
        assert!(!is_combining_mark('a') && !is_combining_mark(PLACEHOLDER));
        assert_eq!(PLACEHOLDER.len_utf8(), 4);
        let mut bytes = [0; 4];
        PLACEHOLDER.encode_utf8(&mut bytes);
        assert_eq!(bytes, PLACEHOLDER_UTF8);
    }

    fn cell(
        start: usize,
        len: usize,
        column: usize,
        ids: (u32, u32),
        row: Option<u16>,
        col: Option<u16>,
        msb: Option<u16>,
    ) -> PlaceholderCell {
        PlaceholderCell {
            range: start..start + len,
            column,
            image_low: ids.0,
            placement_id: ids.1,
            row,
            col,
            msb,
        }
    }

    fn runs(cells: Vec<PlaceholderCell>) -> Vec<PlaceholderRun> {
        let mut builder = RunBuilder::default();
        let mut out: Vec<_> = cells.into_iter().filter_map(|c| builder.push(c)).collect();
        out.extend(builder.finish());
        out
    }

    #[test]
    fn explicit_rows_and_columns_group_into_one_run_per_row() {
        // The specification's 2x2 example, one line: (0,0) (0,1).
        let out = runs(vec![
            cell(0, 6, 0, (42, 0), Some(0), Some(0), None),
            cell(6, 6, 1, (42, 0), Some(0), Some(1), None),
        ]);
        assert_eq!(out.len(), 1);
        let run = &out[0];
        assert_eq!(
            (run.image_id, run.placement_id, run.row, run.column),
            (42, 0, 0, 0)
        );
        assert_eq!(run.cells, [0..6, 6..12]);
        assert_eq!(run.len(), 2);
        assert_eq!(run.byte_range(0, 2), 0..12);
        assert_eq!(run.byte_range(1, 1), 6..12);
        // A column that is not the next one starts a new run.
        let out = runs(vec![
            cell(0, 6, 0, (42, 0), Some(0), Some(0), None),
            cell(6, 6, 1, (42, 0), Some(0), Some(2), None),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!((out[1].row, out[1].column), (0, 2));
    }

    #[test]
    fn missing_diacritics_inherit_from_the_cell_to_the_left() {
        // Row only on the first cell, nothing on the rest: 2 rows by 3
        // columns spelled with one diacritic per line.
        let out = runs(vec![
            cell(0, 6, 0, (42, 0), Some(1), None, None),
            cell(6, 4, 1, (42, 0), None, None, None),
            cell(10, 4, 2, (42, 0), None, None, None),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].row, out[0].column, out[0].len()), (1, 0, 3));
        // A row-only cell continues the run when the row agrees...
        let out = runs(vec![
            cell(0, 8, 0, (42, 0), Some(1), Some(3), None),
            cell(8, 6, 1, (42, 0), Some(1), None, None),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].column, out[0].len()), (3, 2));
        // ...and breaks it when it does not.
        let out = runs(vec![
            cell(0, 8, 0, (42, 0), Some(1), Some(3), None),
            cell(8, 6, 1, (42, 0), Some(2), None, None),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!((out[1].row, out[1].column), (2, 0));
    }

    #[test]
    fn the_third_diacritic_supplies_the_id_s_high_byte_and_is_inherited() {
        let out = runs(vec![
            cell(0, 10, 0, (42, 0), Some(0), Some(0), Some(2)),
            cell(10, 8, 1, (42, 0), Some(0), Some(1), None),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].image_id, 42 + (2 << 24));
        // A differing high byte breaks the run.
        let out = runs(vec![
            cell(0, 10, 0, (42, 0), Some(0), Some(0), Some(2)),
            cell(10, 10, 1, (42, 0), Some(0), Some(1), Some(3)),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].image_id, 42 + (3 << 24));
        // Past a byte it wraps, as a 32-bit shift does.
        let out = runs(vec![cell(0, 10, 0, (1, 0), None, None, Some(256))]);
        assert_eq!(out[0].image_id, 1);
    }

    #[test]
    fn colours_and_gaps_break_runs() {
        let out = runs(vec![
            cell(0, 4, 0, (42, 0), None, None, None),
            cell(4, 4, 1, (43, 0), None, None, None),
            cell(8, 4, 2, (43, 1), None, None, None),
            // A gap of text between cells.
            cell(13, 4, 4, (43, 1), None, None, None),
        ]);
        assert_eq!(out.len(), 4);
        assert_eq!(out[3].first_cell_column, 4);
        assert_eq!(
            (out[3].row, out[3].column),
            (0, 0),
            "a fresh run starts at the origin"
        );
    }
}
