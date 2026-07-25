//! Country-aware phone number generation.
//!
//! Produces structurally valid MSISDNs for a given country using the
//! numbering ranges of `phonenumber`, with uniqueness guaranteed across a
//! batch and reproducibility from a seed.
//!
//! The random number generator is **injected** (CLAUDE.md §7): given the same
//! seed, two runs produce the same sequence. That is what makes the
//! uniqueness properties checkable with `proptest`.
//!
//! Implemented at milestone 013.

mod error;

pub use error::NumbersGenError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!numbers_gen::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::NumbersGenError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            NumbersGenError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
