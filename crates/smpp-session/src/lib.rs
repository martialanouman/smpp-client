//! SMPP sessions: bind, windowing, `enquire_link` and reconnection.
//!
//! Turns the stateless codec of [`smpp_core`] into live sessions: one task
//! per session owns the socket, and every other component talks to it through
//! **bounded** `mpsc` channels (CLAUDE.md §4) — that back-pressure is what
//! stops a campaign from exhausting memory when the SMSC slows down.
//!
//! Depends on [`smpp_core`] for PDUs and on [`rate_control`] for send
//! pacing. Every long-running task watches a `CancellationToken` and shuts
//! down cleanly: `unbind`, then queue drain.
//!
//! Implemented at milestone 005.

mod error;

pub use error::SessionError;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!smpp_session::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::SessionError;

    #[test]
    fn crate_error_renders_a_readable_message() {
        assert_eq!(
            SessionError::NotImplemented.to_string(),
            "not implemented yet"
        );
    }
}
