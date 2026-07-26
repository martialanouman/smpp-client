//! SQLite access: migrations, repositories and transactions.
//!
//! SQLite in WAL mode, accessed through SQLx — see ADR
//! [`0002-persistance-sqlite-sqlx`](../../../docs/adr/0002-persistance-sqlite-sqlx.md).
//! The crate upholds the write-ahead invariant of CLAUDE.md §4: a message is
//! persisted **before** being sent, and its state transitions are idempotent.
//!
//! # The one rule this crate exists to enforce
//!
//! No SQL anywhere else (CA-002-03, guide §11.1). Everything reachable from
//! outside speaks in aggregates — [`Message`], [`Contact`], [`Campaign`],
//! [`SessionProfile`] — and the connection pool is `pub(crate)`, so the rule
//! is not a convention a reviewer has to remember but something the type
//! system refuses to let you break.
//!
//! # Layout
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`db`] | opening the file, the `PRAGMA`s, the embedded migrations |
//! | [`ports`] | the four repository traits, for injection and test doubles |
//! | [`records`] | the aggregates the repositories read and write |
//! | [`repositories`] | the SQLx implementations |
//! | [`time`] | [`Timestamp`], the crate's only timestamp format |
//!
//! # Volumetry
//!
//! Guide §11.3 forbids loading a large set into memory. Two shapes are
//! offered and no third: [`ports::MessageRepository::stream_messages`] for a
//! traversal, and cursor pagination — never `OFFSET`, which re-walks the rows
//! it skips and degrades linearly with the page number — for a screen.

mod db;
mod error;
pub mod ports;
mod records;
mod repositories;
mod time;

pub use db::{Database, DatabaseConfig, SchemaObject};
pub use error::PersistenceError;
pub use records::{
    BindType, Campaign, CampaignId, CampaignStatus, Contact, ContactId, ContactList, ListId,
    Message, MessageFilter, MessageState, MessageStateUpdate, PduDirection, PduLogEntry,
    SessionProfile,
};
pub use repositories::{
    SqliteCampaignRepository, SqliteContactRepository, SqliteMessageRepository,
    SqlitePduLogRepository, SqliteSessionProfileRepository,
};
pub use time::Timestamp;

/// Opaque position in a paginated or streamed result set.
///
/// A cursor is SQLite's `rowid` for the last row handed out, which for these
/// tables is insertion order. Two consequences worth stating: pagination costs
/// the same on page one and page ten thousand — unlike `OFFSET`, which walks
/// and discards everything before the window — and a row inserted while the
/// caller is paging appears at the end rather than shifting the pages already
/// read.
///
/// The inner value is private: it is a position in *one* result set, not an
/// identifier, and nothing outside this crate should build one by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor(i64);

impl Cursor {
    /// The position before the first row.
    #[must_use]
    pub const fn start() -> Self {
        Self(0)
    }

    /// Rebuilds a cursor from its serialised form.
    ///
    /// The frontend hands a cursor back across the IPC boundary to ask for the
    /// next page; a value it never received is harmless, since the cursor only
    /// ever selects a window of rows the caller could already read.
    #[must_use]
    pub const fn from_raw(position: i64) -> Self {
        Self(position)
    }

    /// The serialised form.
    #[must_use]
    pub const fn into_raw(self) -> i64 {
        self.0
    }
}

/// One page of results, with the cursor that continues it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The rows of this page, in cursor order.
    pub items: Vec<T>,
    /// Where to resume, or `None` when the page is the last one.
    ///
    /// `None` means "this page was shorter than the limit asked for", which is
    /// the only cheap way to know there is nothing after it without a second
    /// round trip.
    pub next: Option<Cursor>,
}

impl<T> Page<T> {
    /// Reports whether the page holds no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The number of rows in the page.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!persistence::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::{Cursor, Page};

    #[test]
    fn a_page_reports_its_own_emptiness() {
        let page: Page<u8> = Page {
            items: Vec::new(),
            next: None,
        };

        assert!(page.is_empty());
        assert_eq!(page.len(), 0);
    }

    #[test]
    fn a_cursor_survives_serialisation() {
        let cursor = Cursor::from_raw(42);

        assert_eq!(Cursor::from_raw(cursor.into_raw()), cursor);
    }

    #[test]
    fn the_start_cursor_precedes_every_row() {
        assert_eq!(Cursor::start().into_raw(), 0);
    }
}
