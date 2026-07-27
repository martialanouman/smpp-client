//! What the CSV and XLSX readers have in common.
//!
//! One row of cells and a line number, and a reader that hands them out one at
//! a time. Everything above this — mapping, validation, deduplication, the
//! report — is written once against [`RowSource`] and works on both formats,
//! which is what stops the two paths from drifting into two behaviours.

use crate::error::ContactsError;

/// One row of a file, as text, before anything is made of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// 1-based line (CSV) or row (XLSX) number, as the operator's editor shows
    /// it.
    ///
    /// A rejection that says "line 4 500" has to point at line 4 500 of the
    /// file the operator is about to open, not at the 4 500th row handed out —
    /// so a quoted CSV field spanning three lines advances this by three, and
    /// an XLSX sheet whose used range starts on row 5 numbers its first row 5.
    ///
    /// **One exception, in CSV only:** the `csv` crate skips blank lines before
    /// this code sees them and does not count them, so blank lines between data
    /// rows make this short by their number. `import::csv::read_record` says
    /// why that cannot be fixed from outside the crate.
    pub line: u64,
    /// The cells, trimmed, in file order.
    pub values: Vec<String>,
    /// Positions of cells that held something no text could be read out of.
    ///
    /// Always empty for CSV, where every cell is text by construction. A
    /// spreadsheet can hold an error cell or a fractional number in a column
    /// meant for phone numbers, and CA-009-03 asks for those to be **rejected
    /// with a clear reason** rather than read as empty — "empty" would send the
    /// operator looking at a cell that is not.
    pub unreadable: Vec<usize>,
}

impl RawRow {
    /// Whether every cell of the row is empty.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.values.iter().all(|value| value.trim().is_empty())
    }
}

/// A file handed out one row at a time.
///
/// Deliberately **not** an `Iterator`: reading a row can fail, and an
/// `Iterator<Item = Result<…>>` makes "stop at the first error" and "skip the
/// error" look equally plausible at every call site. Here there is one way.
pub trait RowSource {
    /// The header row, when the file has one.
    fn headers(&self) -> Option<&[String]>;

    /// The next data row, or `None` at the end of the file.
    ///
    /// Blank rows are skipped by the implementation; a returned row always
    /// holds at least one non-empty cell.
    ///
    /// # Errors
    ///
    /// [`ContactsError::Read`] when the underlying file cannot be read or
    /// parsed. The error carries the line it happened on and never a cell
    /// value.
    fn next_row(&mut self) -> Result<Option<RawRow>, ContactsError>;
}
