//! Error type for this crate.

use std::path::PathBuf;

/// Errors produced by this crate.
///
/// Per guide §6.1, every crate exposes **one** exhaustive `thiserror` type.
/// No public API returns a `Box<dyn Error>`: callers must be able to
/// discriminate between cases.
///
/// # What never appears in a message
///
/// CLAUDE.md §8 forbids a secret from reaching a log or an error, and
/// CA-002-09 makes it a criterion. Two rules hold that line here:
///
/// * no variant carries a column **value**. [`Self::Conflict`] and
///   [`Self::NotFound`] carry an identifier — a UUID, which identifies a row
///   without revealing anything about it — and never the row itself. In
///   particular `session_profiles.password_enc` is unreachable from this type.
/// * the SQLite messages wrapped by [`Self::Database`] name tables, columns
///   and constraints, never the bound parameters. That is a property of
///   SQLite's own error strings, and
///   `a_rejected_insert_never_echoes_the_offending_value` in
///   `tests/errors.rs` holds it.
///
/// A filesystem **path** is not a secret and [`Self::DataDirectory`] carries
/// one: without it "permission denied" is unactionable. Redaction happens at
/// the IPC boundary (guide §6.1), which is `src-tauri`'s job, not this
/// crate's.
///
/// `#[non_exhaustive]` lets later milestones add variants without breaking
/// `match` expressions in calling crates.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PersistenceError {
    /// The application data directory could not be created or reached.
    #[error("cannot prepare the data directory `{path}`")]
    DataDirectory {
        /// Directory the crate tried to create.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// The SQLite file could not be opened, or its connection options were
    /// rejected.
    #[error("cannot open the database")]
    Open {
        /// Underlying driver error.
        #[source]
        source: sqlx::Error,
    },

    /// A migration failed to apply, or a shipped migration no longer matches
    /// the one recorded in the database.
    ///
    /// The second case is the interesting one: `sqlx` compares the checksum of
    /// every applied migration against the file on disk, so an edited shipped
    /// migration surfaces here rather than as a silently divergent schema
    /// (guide §11.2).
    #[error("migrations could not be applied")]
    Migrate {
        /// Underlying migration error.
        #[source]
        source: sqlx::migrate::MigrateError,
    },

    /// A statement failed.
    #[error("database query failed")]
    Database {
        /// Underlying driver error.
        #[source]
        source: sqlx::Error,
    },

    /// A row expected to exist does not.
    #[error("no {entity} with identifier {id}")]
    NotFound {
        /// Aggregate name, as used in this crate's repository names.
        entity: &'static str,
        /// Identifier looked up, in its canonical text form.
        id: String,
    },

    /// A write collided with an existing row on a primary key or a unique
    /// index.
    #[error("a {entity} with identifier {id} already exists")]
    Conflict {
        /// Aggregate name, as used in this crate's repository names.
        entity: &'static str,
        /// Identifier that collided, in its canonical text form.
        id: String,
    },

    /// A stored value could not be turned back into its domain type.
    ///
    /// Reaching this means the file was modified outside the application, or
    /// a migration lost information: the schema's own `CHECK` constraints make
    /// it unreachable through this crate's API.
    #[error("column `{table}.{column}` holds a value this version cannot read")]
    MalformedRow {
        /// Table the row was read from.
        table: &'static str,
        /// Column whose value could not be decoded.
        column: &'static str,
        /// What was expected, without echoing the offending value.
        expected: &'static str,
    },
}

impl PersistenceError {
    /// Classifies a driver error, promoting a uniqueness violation to
    /// [`Self::Conflict`].
    ///
    /// SQLite reports both a primary-key collision and a unique-index
    /// collision as extended code 1555/2067 under primary code 19
    /// (`SQLITE_CONSTRAINT`). Callers of an `insert_*` method need to tell
    /// "already there" from "the disk is full", and matching on a string
    /// message in every repository would be four copies of the same fragile
    /// test.
    pub(crate) fn from_write(
        source: sqlx::Error,
        entity: &'static str,
        id: impl Into<String>,
    ) -> Self {
        if is_unique_violation(&source) {
            return Self::Conflict {
                entity,
                id: id.into(),
            };
        }
        Self::Database { source }
    }
}

/// Reports whether a driver error is a uniqueness violation.
fn is_unique_violation(source: &sqlx::Error) -> bool {
    match source {
        sqlx::Error::Database(database_error) => database_error.is_unique_violation(),
        _ => false,
    }
}

impl From<sqlx::Error> for PersistenceError {
    fn from(source: sqlx::Error) -> Self {
        Self::Database { source }
    }
}

impl From<sqlx::migrate::MigrateError> for PersistenceError {
    fn from(source: sqlx::migrate::MigrateError) -> Self {
        Self::Migrate { source }
    }
}
