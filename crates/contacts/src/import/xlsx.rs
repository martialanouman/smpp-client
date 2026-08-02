//! XLSX reader (deliverable L-009-02).
//!
//! # The typed-cell trap, and what this module does about it
//!
//! Fiche §6 calls it the major pitfall of the milestone, and it is worth
//! spelling out because the failure is silent. Type `0612345678` into Excel and
//! it stores the **number** 612345678: the leading zero is gone from the file,
//! not merely from the display. Type `+2250700000000` and Excel stores the
//! number 2250700000000, and shows it as `2,25E+12`.
//!
//! What comes out of `calamine` is therefore a `Data::Float` or a `Data::Int`,
//! never the string the operator believes they typed. The three cases and the
//! behaviour chosen for each:
//!
//! | Cell | Rendered as | Why |
//! |------|-------------|-----|
//! | `Data::String` | itself, trimmed | nothing was lost |
//! | `Data::Int`, `Data::Float` with no fractional part | its plain integer form | scientific notation is a *display* format; the value is intact |
//! | `Data::Float` with a fractional part | **rejected**, with a reason | a phone number has no decimals; something else is in that column |
//!
//! The middle row is the one that matters. `2.25e12` and `2250700000000` are
//! the same `f64`, so writing it out in full recovers the number exactly —
//! every E.164 number fits in fifteen digits, far inside the 2⁵³ an `f64`
//! represents without loss. The **leading zero** is genuinely gone, and is not
//! invented back here: `612345678` is handed to the validator with the import's
//! default region, and the numbering plan of `FR` recognises it as
//! `+33612345678`. That is the correct repair, and it is the plan's job, not a
//! string manipulation's.
//!
//! # Memory
//!
//! `calamine` materialises a worksheet range: an XLSX is a zip of XML, and
//! there is no row-at-a-time reading of one. CA-009-01 asks for streaming on
//! **CSV**, which is the format a million-row export actually arrives in, and
//! this module states the difference rather than implying a guarantee it cannot
//! give. Parsing is also CPU-bound, so the caller runs it under
//! `spawn_blocking` (guide §7.1); [`XlsxRows::open`] is deliberately a blocking
//! function so that is impossible to forget.

use std::path::Path;

use calamine::{Data, Reader, Xlsx};

use crate::error::ContactsError;
use crate::import::source::{RawRow, RowSource};

/// Renders one cell as the text the mapping and the validator work on.
///
/// Returns `None` for a cell that holds something a number column cannot be
/// read out of — a fractional number, an error cell, a date. The caller turns
/// that into a rejection with a reason rather than into an empty string, which
/// would be reported as "empty" and send the operator looking at the wrong
/// thing.
#[must_use]
pub fn render_cell(cell: &Data) -> Option<String> {
    match cell {
        Data::Empty => Some(String::new()),
        Data::String(value) => Some(value.trim().to_owned()),
        Data::Int(value) => Some(value.to_string()),
        Data::Float(value) => render_float(*value),
        Data::Bool(value) => Some(value.to_string()),
        // A date in a phone-number column is a mis-typed cell, and in an
        // attribute column it is text the operator can read. Its ISO rendering
        // is the honest one either way.
        Data::DateTimeIso(value) | Data::DurationIso(value) => Some(value.clone()),
        Data::DateTime(_) | Data::Error(_) => None,
    }
}

/// Writes a spreadsheet number back as plain digits, or refuses.
///
/// `{value}` on an `f64` would print `2250700000000` for the integer case,
/// which is what is wanted — but it would also print `6.12345678e9` for large
/// magnitudes on some formatters and `612345678.5` for a fractional one. Both
/// branches are therefore explicit.
fn render_float(value: f64) -> Option<String> {
    if !value.is_finite() || value.fract() != 0.0 || value.is_sign_negative() {
        return None;
    }

    // Beyond 2^53 an `f64` no longer represents every integer, so the digits
    // coming out would not be the digits that went in. No E.164 number is
    // remotely that large; a cell that is has something else in it.
    if value >= 9_007_199_254_740_992.0 {
        return None;
    }

    // Formatted rather than cast. `value as i64` is exactly the truncating
    // conversion the workspace denies (`cast_possible_truncation`), and the
    // fixed-point format produces the same digits without one — `{:.0}` on an
    // integral `f64` writes it in full, never in scientific notation.
    Some(format!("{value:.0}"))
}

/// The sheets of a workbook, in file order.
///
/// # Errors
///
/// [`ContactsError::Read`] if the file is not a readable workbook.
pub fn sheet_names(path: &Path) -> Result<Vec<String>, ContactsError> {
    let workbook: Xlsx<_> = calamine::open_workbook(path).map_err(ContactsError::spreadsheet)?;

    Ok(workbook.sheet_names().to_vec())
}

/// One worksheet, handed out row by row.
pub struct XlsxRows {
    rows: std::vec::IntoIter<RawRow>,
    headers: Option<Vec<String>>,
    offset: u64,
}

impl core::fmt::Debug for XlsxRows {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("XlsxRows").finish_non_exhaustive()
    }
}

impl XlsxRows {
    /// Opens one sheet of a workbook.
    ///
    /// **Blocking and CPU-bound.** Call it from `spawn_blocking`, never on a
    /// runtime thread (guide §7.1).
    ///
    /// `sheet` is `None` for the first sheet of the workbook, which is what a
    /// single-sheet file means and what the assistant preselects.
    ///
    /// # Errors
    ///
    /// [`ContactsError::Read`] if the workbook cannot be opened, or
    /// [`ContactsError::UnknownSheet`] if it holds no sheet by that name.
    pub fn open(
        path: &Path,
        sheet: Option<&str>,
        headers: super::csv::HeaderMode,
    ) -> Result<Self, ContactsError> {
        let mut workbook: Xlsx<_> =
            calamine::open_workbook(path).map_err(ContactsError::spreadsheet)?;

        let name = match sheet {
            Some(name) => {
                if !workbook.sheet_names().iter().any(|known| known == name) {
                    return Err(ContactsError::UnknownSheet {
                        sheet: name.to_owned(),
                    });
                }

                name.to_owned()
            }
            None => workbook
                .sheet_names()
                .first()
                .cloned()
                .ok_or(ContactsError::EmptyWorkbook)?,
        };

        let range = workbook
            .worksheet_range(&name)
            .map_err(ContactsError::spreadsheet)?;

        // `start()` is the first *used* cell: a sheet whose data begins on row
        // 5 must report line 5, not line 1, or every rejection points at the
        // wrong row of the operator's spreadsheet.
        let offset = u64::from(range.start().map_or(0, |(row, _)| row));

        let mut rows = Vec::new();

        for (index, row) in range.rows().enumerate() {
            let line = offset
                .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
                .saturating_add(1);

            let mut values = Vec::with_capacity(row.len());
            let mut unreadable = Vec::new();

            for (column, cell) in row.iter().enumerate() {
                match render_cell(cell) {
                    Some(value) => values.push(value),
                    None => {
                        unreadable.push(column);
                        values.push(String::new());
                    }
                }
            }

            // Blank rows are kept, for the reason `CsvRows` keeps them: the
            // writer counts them apart from the total, and a reader that
            // dropped them would hold `ImportReport::blank` at zero.
            rows.push(RawRow {
                line,
                values,
                unreadable,
            });
        }

        let mut rows = rows.into_iter();

        let has_headers = match headers {
            super::csv::HeaderMode::Present => true,
            super::csv::HeaderMode::Absent => false,
            super::csv::HeaderMode::Detect => rows
                .as_slice()
                .first()
                .is_some_and(|row| super::csv::looks_like_header(&row.values)),
        };

        let headers = has_headers
            .then(|| rows.next())
            .flatten()
            .map(|row| row.values);

        Ok(Self {
            rows,
            headers,
            offset,
        })
    }

    /// The 1-based row the used range starts at.
    #[must_use]
    pub const fn first_line(&self) -> u64 {
        self.offset.saturating_add(1)
    }
}

impl RowSource for XlsxRows {
    fn headers(&self) -> Option<&[String]> {
        self.headers.as_deref()
    }

    fn next_row(&mut self) -> Result<Option<RawRow>, ContactsError> {
        // Each row carries the number it had in the used range, assigned while
        // the sheet was read. Counting the rows handed out instead would drift
        // by every blank row dropped, and a rejection would point at the wrong
        // line of the operator's spreadsheet.
        Ok(self.rows.next())
    }
}

#[cfg(test)]
mod tests {
    use super::{render_cell, render_float};
    use calamine::Data;

    /// The trap of CA-009-03, in both of its forms: the lost leading zero and
    /// the scientific notation. Neither is a string problem — both are the same
    /// `f64` written back out in full.
    #[test]
    fn a_numeric_cell_is_written_back_as_plain_digits() {
        assert_eq!(
            render_cell(&Data::Float(612_345_678.0)),
            Some(String::from("612345678")),
            "Excel dropped the leading zero; the plan puts the country back"
        );
        assert_eq!(
            render_cell(&Data::Float(2_250_700_000_000.0)),
            Some(String::from("2250700000000")),
            "shown as 2,25E+12; the value is intact"
        );
        assert_eq!(
            render_cell(&Data::Int(2_250_700_000_000)),
            Some(String::from("2250700000000"))
        );
    }

    #[test]
    fn a_text_cell_keeps_its_leading_plus_and_zero() {
        assert_eq!(
            render_cell(&Data::String(String::from(" +2250700000000 "))),
            Some(String::from("+2250700000000"))
        );
        assert_eq!(
            render_cell(&Data::String(String::from("0612345678"))),
            Some(String::from("0612345678"))
        );
    }

    /// A cell that cannot be read as a number is refused rather than turned
    /// into an empty string: "empty" would send the operator looking at a
    /// blank cell that is not blank.
    #[test]
    fn a_cell_a_number_cannot_be_read_out_of_is_refused() {
        assert!(render_float(612_345_678.5).is_none());
        assert!(render_float(f64::NAN).is_none());
        assert!(render_float(f64::INFINITY).is_none());
        assert!(render_cell(&Data::Error(calamine::CellErrorType::Div0)).is_none());
    }

    /// Beyond 2^53 the digits coming out are not the digits that went in, so
    /// the cell is refused rather than silently rounded.
    #[test]
    fn a_number_too_large_to_be_exact_is_refused() {
        assert!(render_float(9_007_199_254_740_994.0).is_none());
        assert_eq!(
            render_float(9_007_199_254_740_991.0),
            Some(String::from("9007199254740991"))
        );
    }

    #[test]
    fn an_empty_cell_renders_as_the_empty_string() {
        assert_eq!(render_cell(&Data::Empty), Some(String::new()));
    }
}
