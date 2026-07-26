//! SMPP sessions: bind, keep-alive, correlation and reconnection.
//!
//! Turns the stateless codec of [`smpp_core`] into a live session. One task
//! owns the socket, and everything else talks to it through **bounded**
//! `mpsc` channels (CLAUDE.md §4) — that back-pressure is what stops a
//! campaign from exhausting memory when the SMSC slows down.
//!
//! Implemented at milestone 005.
//!
//! # Modules
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`profile`] | the session profile of spec §8.2, and its mapping to storage |
//! | [`state`] | the state machine of spec §7.9 and its legal edges |
//! | [`reconnect`] | back-off, jitter, and the fatal/recoverable decision |
//!
//! Not here: the send orchestration (milestone 006), windowing and rate
//! control (milestone 007), TLS (milestone 015).

mod error;
mod pending;

pub mod reconnect;
pub mod state;

pub use error::{ProfileRejection, SessionError};

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!smpp_session::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
