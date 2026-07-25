//! Encoding, segmentation and send orchestration.
//!
//! The business entry point for sending: picks the DCS, segments long
//! messages (UDH or `sar_*` TLVs), persists before sending, then hands PDUs
//! to [`smpp_session`]. Also orchestrates bulk campaigns — resume after
//! interruption, progress tracking, volume caps.
//!
//! Declares the *ports* that [`persistence`] implements
//! (`MessageRepository`): the trait belongs to this layer, its SQLx
//! implementation to the lower one. That is what makes the orchestrator
//! testable without a real database.
//!
//! Implemented at milestones 004, 006 and 010.

mod error;

pub use error::MessagingError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!messaging::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::MessagingError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            MessagingError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
