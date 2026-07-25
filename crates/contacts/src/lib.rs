//! Contact import, E.164 validation and list management.
//!
//! Reads CSV and XLSX files (`csv`, `calamine`), normalises numbers to E.164
//! through `phonenumber`, deduplicates, and materialises distribution lists
//! as well as the **exclusion list** applied before any send (usage
//! safeguard, CLAUDE.md §8).
//!
//! *Parse, don't validate*: a number crossing this crate's boundary is an
//! `Msisdn`, not a `String` — invalid state becomes unrepresentable
//! downstream.
//!
//! Implemented at milestone 009.

mod error;

pub use error::ContactsError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!contacts::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::ContactsError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            ContactsError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
