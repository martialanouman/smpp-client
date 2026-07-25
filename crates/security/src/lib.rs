//! Secrets, OS keyring and TLS configuration.
//!
//! The only crate allowed to handle SMSC credentials in the clear, and only
//! in memory. It encrypts secrets before persistence (AES-256-GCM, key
//! derived with Argon2 and stored in the OS keyring via `keyring`) and builds
//! `tokio-rustls` configurations with certificate verification enabled by
//! default.
//!
//! Invariant upheld here (CLAUDE.md §8): **no secret ever leaves in the
//! clear** — not in the database, not in logs even at `trace` level, not in
//! exports. Types carrying a secret do not derive `Debug`.
//!
//! Implemented at milestone 015.

mod error;

pub use error::SecurityError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!security::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::SecurityError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            SecurityError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
