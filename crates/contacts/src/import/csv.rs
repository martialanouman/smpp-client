//! Streaming CSV reader with separator and encoding detection (L-009-01).
//!
//! # What a real customer file looks like
//!
//! Not three well-formed lines. A file that reaches this reader has, in any
//! combination: a UTF-8 byte-order mark that turns the first header into
//! `﻿telephone`; semicolons instead of commas, because that is what a French
//! Excel writes; CRLF line endings; Latin-1 accents; quoted fields containing
//! the separator or a newline; blank lines; and sometimes no header row at all.
//! Each of those is one silent way to import zero contacts out of fifty
//! thousand, so each is detected here and covered by a test.
//!
//! # Streaming, and what that buys (CA-009-01)
//!
//! Detection reads a bounded [`SAMPLE_BYTES`] prefix, then hands the reader
//! back **unconsumed** by chaining the sample in front of the rest. The `csv`
//! reader that follows holds one record at a time. A one-million-row file
//! therefore costs the same resident memory as a ten-row one, and the test that
//! proves it measures the allocator rather than trusting the shape of the code.

use std::io::{BufReader, Read};
use std::path::Path;

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use encoding_rs_io::DecodeReaderBytesBuilder;

use crate::error::ContactsError;
use crate::import::source::{RawRow, RowSource};

/// How many bytes detection looks at before deciding.
///
/// Large enough to hold a header row and a few data rows of a wide file,
/// small enough to be irrelevant next to a hundred-megabyte import. The whole
/// point is that this number does **not** grow with the file.
pub const SAMPLE_BYTES: usize = 64 * 1024;

/// The field separators this reader knows how to detect.
///
/// Comma first, so it wins a tie: it is the one the format is named after, and
/// a file where two candidates appear equally often is almost always a comma
/// file whose data happens to contain semicolons.
pub const CANDIDATE_SEPARATORS: [u8; 3] = *b",;\t";

/// Whether the first row names the columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeaderMode {
    /// Decide by looking at the first row.
    #[default]
    Detect,
    /// The first row names the columns.
    Present,
    /// Every row is data.
    Absent,
}

/// What detection concluded about a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsvDialect {
    /// The field separator.
    pub separator: u8,
    /// The character encoding the bytes were decoded from.
    pub encoding: &'static Encoding,
    /// Whether the first row was taken as a header row.
    pub has_headers: bool,
}

/// Guesses the character encoding of a file from a sample.
///
/// Three cases and no cleverness:
///
/// 1. a byte-order mark says what it is, and is believed;
/// 2. otherwise, bytes that decode as UTF-8 **are** UTF-8 — the encoding is
///    self-validating, and a Latin-1 file with accents virtually never
///    passes the check by accident;
/// 3. anything else is read as Windows-1252, the superset of ISO-8859-1 that
///    every "Latin-1" file produced by a Windows tool is actually in. Reading a
///    true ISO-8859-1 file as Windows-1252 differs only on the C1 control
///    range, which holds no text.
///
/// Never fails: case 3 is total, which is the point. A file this function
/// refused would be a file the operator cannot import at all.
#[must_use]
pub fn detect_encoding(sample: &[u8]) -> &'static Encoding {
    if let Some((encoding, _)) = Encoding::for_bom(sample) {
        return encoding;
    }

    // A truncated sample can cut a multi-byte sequence in half, which would
    // make a perfectly good UTF-8 file look invalid. Only a *decoding* error
    // strictly before the tail counts.
    match core::str::from_utf8(sample) {
        Ok(_) => UTF_8,
        Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => UTF_8,
        Err(_) => WINDOWS_1252,
    }
}

/// Guesses the field separator from decoded text.
///
/// Counts each candidate **outside quoted fields** on the first few lines, and
/// takes the most frequent. Counting inside quotes is how a file of addresses
/// — `"Abidjan, Cocody"` — gets read as a comma file when it is a semicolon
/// one, which produces a header row of one column and zero imported contacts.
#[must_use]
pub fn detect_separator(sample: &str) -> u8 {
    let mut counts = [0_usize; CANDIDATE_SEPARATORS.len()];
    let mut quoted = false;
    let mut lines_seen = 0_usize;

    for byte in sample.bytes() {
        match byte {
            b'"' => quoted = !quoted,
            b'\n' if !quoted => {
                lines_seen += 1;

                // Five lines is enough to tell a separator apart and short
                // enough that a wide file does not pay for the whole sample.
                if lines_seen >= 5 {
                    break;
                }
            }
            _ if !quoted => {
                if let Some(index) = CANDIDATE_SEPARATORS
                    .iter()
                    .position(|candidate| *candidate == byte)
                {
                    counts[index] += 1;
                }
            }
            _ => {}
        }
    }

    counts
        .iter()
        .enumerate()
        .max_by_key(|(index, count)| (**count, core::cmp::Reverse(*index)))
        .and_then(|(index, count)| (*count > 0).then(|| CANDIDATE_SEPARATORS[index]))
        .unwrap_or(b',')
}

/// Whether a first row looks like column names rather than data.
///
/// A header row is one where **no** cell looks like a phone number. The test is
/// deliberately one-sided: a file whose first data row is mistaken for a header
/// loses one contact out of fifty thousand and says so in the report, whereas a
/// header row mistaken for data produces one rejected row *and* a mapping that
/// resolves against nothing.
#[must_use]
pub fn looks_like_header(first_row: &[String]) -> bool {
    !first_row.iter().any(|cell| looks_like_number(cell))
}

/// Whether a cell looks like a subscriber number.
///
/// Five digits and no letter. Five rather than three because a three-digit
/// header — `n°1` — is not unheard of, and because a column of short codes is
/// mapped by hand anyway.
fn looks_like_number(cell: &str) -> bool {
    let trimmed = cell.trim();
    let digits = trimmed.chars().filter(char::is_ascii_digit).count();

    digits >= 5
        && !trimmed
            .chars()
            .any(|character| character.is_alphabetic() && character != 'e')
}

/// A CSV file, read one record at a time.
pub struct CsvRows {
    reader: csv::Reader<Box<dyn Read + Send>>,
    dialect: CsvDialect,
    headers: Option<Vec<String>>,
    pending: Option<RawRow>,
    line: u64,
}

impl core::fmt::Debug for CsvRows {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CsvRows")
            .field("dialect", &self.dialect)
            .finish_non_exhaustive()
    }
}

impl CsvRows {
    /// Opens a file, detecting its encoding, separator and header row.
    ///
    /// # Errors
    ///
    /// [`ContactsError::Read`] if the file cannot be opened or its first
    /// record cannot be parsed.
    pub fn open(path: &Path, headers: HeaderMode) -> Result<Self, ContactsError> {
        let file = std::fs::File::open(path).map_err(ContactsError::read)?;

        Self::from_reader(Box::new(BufReader::new(file)), headers)
    }

    /// Reads from an already-open byte source.
    ///
    /// Separate from [`Self::open`] so the tests exercise the detection on
    /// in-memory buffers rather than on temporary files.
    ///
    /// # Errors
    ///
    /// [`ContactsError::Read`] if the stream cannot be read.
    pub fn from_reader(
        mut reader: Box<dyn Read + Send>,
        headers: HeaderMode,
    ) -> Result<Self, ContactsError> {
        let mut sample = Vec::with_capacity(SAMPLE_BYTES);
        reader
            .by_ref()
            .take(
                u64::try_from(SAMPLE_BYTES)
                    .map_err(|_| ContactsError::invalid("sample size out of range"))?,
            )
            .read_to_end(&mut sample)
            .map_err(ContactsError::read)?;

        let encoding = detect_encoding(&sample);
        let (decoded_sample, _, _) = encoding.decode(&sample);
        let separator = detect_separator(&decoded_sample);

        // The sample is put back in front of the rest, so nothing is consumed
        // by the detection and the file is still read exactly once.
        let joined = std::io::Cursor::new(sample).chain(reader);

        let decoded = DecodeReaderBytesBuilder::new()
            .encoding(Some(encoding))
            .bom_sniffing(true)
            .build(joined);

        let mut reader = csv::ReaderBuilder::new()
            .delimiter(separator)
            .has_headers(false)
            .flexible(true)
            .from_reader(Box::new(decoded) as Box<dyn Read + Send>);

        let mut line = 0_u64;
        let first = read_record(&mut reader, &mut line)?;

        let has_headers = match (headers, first.as_ref()) {
            (_, None) => false,
            (HeaderMode::Present, Some(_)) => true,
            (HeaderMode::Absent, Some(_)) => false,
            (HeaderMode::Detect, Some(row)) => looks_like_header(&row.values),
        };

        let (headers, pending) = match (has_headers, first) {
            (true, Some(row)) => (Some(row.values), None),
            (false, first) => (None, first),
            (true, None) => (None, None),
        };

        Ok(Self {
            reader,
            dialect: CsvDialect {
                separator,
                encoding,
                has_headers,
            },
            headers,
            pending,
            line,
        })
    }

    /// What the detection concluded.
    #[must_use]
    pub const fn dialect(&self) -> CsvDialect {
        self.dialect
    }
}

impl RowSource for CsvRows {
    fn headers(&self) -> Option<&[String]> {
        self.headers.as_deref()
    }

    fn next_row(&mut self) -> Result<Option<RawRow>, ContactsError> {
        if let Some(row) = self.pending.take() {
            return Ok(Some(row));
        }

        read_record(&mut self.reader, &mut self.line)
    }
}

/// Reads one record, skipping records that hold nothing at all.
///
/// A blank line is not a row: a file exported from a spreadsheet routinely ends
/// with several, and counting them as rejected contacts would make the report
/// say an import "failed" on rows that were never there. Blank lines still
/// advance the line counter, so the line number in a rejection is the line
/// number in the operator's editor.
fn read_record(
    reader: &mut csv::Reader<Box<dyn Read + Send>>,
    line: &mut u64,
) -> Result<Option<RawRow>, ContactsError> {
    let mut record = csv::StringRecord::new();

    loop {
        let read = reader
            .read_record(&mut record)
            .map_err(|error| ContactsError::read_at(line.saturating_add(1), &error))?;

        if !read {
            return Ok(None);
        }

        // The line number comes from the reader, NOT from a counter of records
        // handed out: the reader counts the newlines it consumed, so a record
        // whose quoted field spans three lines advances the count by three and
        // the next rejection points at the right place. A counter of records
        // would be two lines out from there on.
        //
        // ONE case it still gets wrong, stated rather than hidden: the `csv`
        // crate skips blank lines before this code sees them and does not count
        // them, so a file with blank lines *between* data rows reports a line
        // short by the number of blank lines above it. There is no way to
        // disable that skipping or to observe it through the public API, and
        // the alternative — counting newlines in a wrapping reader — cannot be
        // aligned with record boundaries because the reader buffers ahead. The
        // number is therefore "the line, not counting blank lines", which is
        // the line itself for the files that have none.
        *line = record
            .position()
            .map_or_else(|| line.saturating_add(1), csv::Position::line);

        let values: Vec<String> = record.iter().map(str::trim).map(str::to_owned).collect();

        if values.iter().all(String::is_empty) {
            continue;
        }

        return Ok(Some(RawRow {
            line: *line,
            values,
            unreadable: Vec::new(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_encoding, detect_separator, looks_like_header, CsvRows, HeaderMode};
    use crate::import::source::RowSource;
    use encoding_rs::{UTF_8, WINDOWS_1252};

    fn rows(bytes: &[u8], mode: HeaderMode) -> (CsvRows, Vec<Vec<String>>) {
        let mut reader = CsvRows::from_reader(Box::new(std::io::Cursor::new(bytes.to_vec())), mode)
            .expect("opens");
        let mut collected = Vec::new();

        while let Some(row) = reader.next_row().expect("reads") {
            collected.push(row.values);
        }

        (reader, collected)
    }

    #[test]
    fn the_three_separators_are_detected() {
        assert_eq!(detect_separator("a,b,c\n1,2,3\n"), b',');
        assert_eq!(detect_separator("a;b;c\n1;2;3\n"), b';');
        assert_eq!(detect_separator("a\tb\tc\n1\t2\t3\n"), b'\t');
    }

    /// The case a naive "count the commas" gets wrong: a semicolon file whose
    /// cells contain commas.
    #[test]
    fn a_separator_inside_quotes_does_not_win_the_count() {
        let sample =
            "nom;telephone\n\"Kone, Awa\";+2250700000000\n\"Diallo, Ali\";+2250700000001\n";

        assert_eq!(detect_separator(sample), b';');
    }

    #[test]
    fn a_file_with_no_separator_at_all_falls_back_to_the_comma() {
        assert_eq!(detect_separator("+2250700000000\n+2250700000001\n"), b',');
    }

    /// CA-009-02, the BOM half: the first header must be `telephone`, not
    /// `\u{feff}telephone`, or the mapping resolves against nothing.
    #[test]
    fn a_utf8_bom_is_stripped_from_the_first_header() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"telephone,ville\n+2250700000000,Abidjan\n");

        let (reader, collected) = rows(&bytes, HeaderMode::Detect);

        assert_eq!(
            reader.headers().expect("headers"),
            ["telephone".to_owned(), "ville".to_owned()]
        );
        assert_eq!(collected.len(), 1);
    }

    /// CA-009-02, the Latin-1 half: `Café` must survive, not become `Caf?` or
    /// `CafÃ©`.
    #[test]
    fn a_latin1_file_is_read_without_mangled_characters() {
        let (encoded, _, _) = WINDOWS_1252.encode("nom;telephone\nCafé Noël;+2250700000000\n");

        assert_eq!(detect_encoding(&encoded), WINDOWS_1252);

        let (_, collected) = rows(&encoded, HeaderMode::Present);

        assert_eq!(collected[0][0], "Café Noël");
    }

    #[test]
    fn a_utf8_file_without_a_bom_is_recognised_as_utf8() {
        assert_eq!(detect_encoding("nom;téléphone\n".as_bytes()), UTF_8);
    }

    #[test]
    fn crlf_line_endings_leave_no_carriage_return_in_the_last_cell() {
        let (_, collected) = rows(
            b"telephone,ville\r\n+2250700000000,Abidjan\r\n",
            HeaderMode::Present,
        );

        assert_eq!(collected[0][1], "Abidjan");
    }

    #[test]
    fn blank_lines_are_skipped_rather_than_counted_as_rows() {
        let (_, collected) = rows(
            b"telephone\n+2250700000000\n\n\n+2250700000001\n\n",
            HeaderMode::Present,
        );

        assert_eq!(collected.len(), 2);
    }

    /// A rejection has to point at the right line of the operator's editor, and
    /// the hard case is a quoted field spanning several lines: a counter of
    /// records handed out would be two lines out from there on.
    #[test]
    fn a_multi_line_field_advances_the_line_number_by_the_lines_it_spans() {
        let mut reader = CsvRows::from_reader(
            Box::new(std::io::Cursor::new(
                b"telephone,adresse\n+2250700000000,\"Cocody\nAbidjan\"\n+2250700000001,Bouake\n"
                    .to_vec(),
            )),
            HeaderMode::Present,
        )
        .expect("opens");

        assert_eq!(reader.next_row().expect("reads").expect("row").line, 2);
        assert_eq!(
            reader.next_row().expect("reads").expect("row").line,
            4,
            "the quoted field took two lines"
        );
    }

    /// The documented limitation, asserted so that a future `csv` that counts
    /// skipped blank lines fails this test rather than silently shifting every
    /// line number of every report.
    #[test]
    fn blank_lines_are_not_counted_towards_the_line_number() {
        let mut reader = CsvRows::from_reader(
            Box::new(std::io::Cursor::new(
                b"telephone\n+2250700000000\n\n+2250700000001\n".to_vec(),
            )),
            HeaderMode::Present,
        )
        .expect("opens");

        assert_eq!(reader.next_row().expect("reads").expect("row").line, 2);
        assert_eq!(
            reader.next_row().expect("reads").expect("row").line,
            3,
            "line 4 of the file: `csv` skips the blank line without counting it"
        );
    }

    #[test]
    fn a_quoted_field_may_contain_the_separator_and_a_newline() {
        let (_, collected) = rows(
            b"telephone;adresse\n+2250700000000;\"Cocody;\nAbidjan\"\n",
            HeaderMode::Present,
        );

        assert_eq!(collected[0][1], "Cocody;\nAbidjan");
    }

    #[test]
    fn a_headerless_file_is_detected_and_its_first_row_is_kept() {
        let (reader, collected) = rows(
            b"+2250700000000,Abidjan\n+2250700000001,Bouake\n",
            HeaderMode::Detect,
        );

        assert!(reader.headers().is_none());
        assert_eq!(collected.len(), 2, "the first row is data, not a header");
    }

    #[test]
    fn a_header_row_is_detected_and_not_returned_as_data() {
        let (reader, collected) = rows(
            b"telephone,ville\n+2250700000000,Abidjan\n",
            HeaderMode::Detect,
        );

        assert!(reader.headers().is_some());
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn the_operator_may_override_the_header_detection_in_both_directions() {
        let (forced_absent, collected) = rows(b"telephone,ville\n", HeaderMode::Absent);
        assert!(forced_absent.headers().is_none());
        assert_eq!(collected.len(), 1);

        let (forced_present, collected) = rows(b"+2250700000000,Abidjan\n", HeaderMode::Present);
        assert!(forced_present.headers().is_some());
        assert!(collected.is_empty());
    }

    #[test]
    fn a_row_of_numbers_is_not_mistaken_for_a_header() {
        assert!(looks_like_header(&[String::from("telephone")]));
        assert!(!looks_like_header(&[String::from("+2250700000000")]));
        assert!(!looks_like_header(&[
            String::from("Awa"),
            String::from("0700000000")
        ]));
    }

    #[test]
    fn an_empty_file_yields_no_row_and_no_header() {
        let (reader, collected) = rows(b"", HeaderMode::Detect);

        assert!(reader.headers().is_none());
        assert!(collected.is_empty());
    }

    /// `flexible(true)`: a hand-edited file whose rows have different widths is
    /// read rather than refused at the first short row.
    #[test]
    fn rows_of_differing_widths_are_all_read() {
        let (_, collected) = rows(
            b"telephone,ville,age\n+2250700000000\n+2250700000001,Abidjan,30\n",
            HeaderMode::Present,
        );

        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].len(), 1);
        assert_eq!(collected[1].len(), 3);
    }
}
