//! Throughput limiting and congestion adaptation.
//!
//! Enforces the TPS negotiated with the SMSC and reacts to congestion signals
//! (`ESME_RTHROTTLED`, the v5.0 `congestion_state` TLV) by slowing the send
//! rate. Will build on `governor`.
//!
//! No internal dependencies: this crate reasons about instants and quotas,
//! never about PDUs. That is what makes it testable with an injected clock,
//! which the determinism requirement of CLAUDE.md §7 demands.
//!
//! Implemented at milestone 007.

mod error;

pub use error::RateControlError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!rate_control::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::RateControlError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            RateControlError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
