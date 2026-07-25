//! SQLite access: migrations, repositories and transactions.
//!
//! Implements the *ports* declared by upper layers — the `MessageRepository`
//! trait belongs to `messaging`, its SQLx implementation lives here
//! (dependency inversion, guide §4.2). The crate therefore depends on no
//! other internal crate despite its position in the graph.
//!
//! SQLite in WAL mode, accessed through SQLx — see ADR
//! [`0002-persistance-sqlite-sqlx`](../../../docs/adr/0002-persistance-sqlite-sqlx.md).
//! It upholds the write-ahead invariant of CLAUDE.md §4: a message is
//! persisted **before** being sent, and its state transitions are idempotent.
//!
//! Schema and migrations at milestone 002.

mod error;

pub use error::PersistenceError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!persistence::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::PersistenceError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            PersistenceError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
