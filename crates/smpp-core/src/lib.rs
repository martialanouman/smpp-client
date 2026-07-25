//! SMPP protocol core: PDU codec and state machine, v3.4 and v5.0.
//!
//! The lowest layer of the architecture (guide §8.1). It depends on **no
//! other internal crate** and knows nothing about persistence, networking or
//! Tauri: it turns bytes into typed PDUs and back, and arbitrates the state
//! transitions the specification allows.
//!
//! The skeleton is empty at milestone 000; implementation starts at
//! milestone 003, based on ADR
//! [`0001-choix-de-la-pile-smpp`](../../../docs/adr/0001-choix-de-la-pile-smpp.md).

mod error;

pub use error::SmppCoreError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!smpp_core::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::SmppCoreError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            SmppCoreError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
