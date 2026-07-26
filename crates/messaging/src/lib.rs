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
//!
//! # Milestone 004 — what is here now
//!
//! Text in, segments out, no I/O at all. Two modules:
//!
//! * [`encoding`] settles the encoding (spec §7.5: GSM 7-bit when the text
//!   allows, UCS2 otherwise, or whatever the user forced), writes the octets,
//!   and answers the live counter the editor needs;
//! * [`segmentation`] cuts a long message into parts and gives each one the
//!   concatenation information its mode calls for.
//!
//! ```
//! use messaging::{
//!     encoding::{Encoding, EncodingChoice},
//!     segmentation::{segment, ConcatenationReference, SegmentationMode},
//! };
//!
//! let text = "a".repeat(161);
//! let split = segment(
//!     &text,
//!     EncodingChoice::Automatic,
//!     SegmentationMode::Udh,
//!     ConcatenationReference::new(42),
//! )?;
//!
//! assert_eq!(split.encoding(), Encoding::Gsm7Bit);
//! assert_eq!(split.segments().len(), 2);
//! assert_eq!(split.segments()[0].content_units(), 153);
//! assert_eq!(split.segments()[1].content_units(), 8);
//! # Ok::<(), messaging::encoding::EncodingError>(())
//! ```

pub mod encoding;
mod error;
pub mod segmentation;

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
