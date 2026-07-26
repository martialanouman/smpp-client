//! Turning a row window into a [`Page`].
//!
//! The same six lines used to sit in four repositories. They are here once,
//! behind a trait each row type implements, so the rule that decides "is there
//! a page after this one" exists in one place.

use crate::{Cursor, Page, PersistenceError};

/// A row that can be handed out as one element of a page.
pub(crate) trait PagedRow {
    /// The domain record this row maps to.
    type Record;

    /// The cursor position of this row.
    ///
    /// SQLite's `rowid` for every table here, aliased to `id` on `pdu_log`
    /// where the schema declares it explicitly.
    fn cursor(&self) -> i64;

    /// Maps the stored columns to the domain record.
    fn into_record(self) -> Result<Self::Record, PersistenceError>;
}

/// Assembles a page and the cursor that continues it.
///
/// A page shorter than the limit is the last one, which is how the caller
/// learns there is nothing after it without a second round trip. A page that
/// is exactly full may still be the last, and then the following call returns
/// an empty page — one wasted round trip at the end of a traversal, against a
/// `COUNT(*)` on every page otherwise.
pub(crate) fn into_page<R>(rows: Vec<R>, limit: u32) -> Result<Page<R::Record>, PersistenceError>
where
    R: PagedRow,
{
    let complete = u64::try_from(rows.len()).unwrap_or(u64::MAX) == u64::from(limit);
    let last = rows.last().map(PagedRow::cursor);

    let items = rows
        .into_iter()
        .map(PagedRow::into_record)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Page {
        items,
        next: if complete {
            last.map(Cursor::from_raw)
        } else {
            None
        },
    })
}
