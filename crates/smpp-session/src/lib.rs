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
//! Plus one file with no public items of its own: `messaging` implements
//! `messaging::ports::SmscSession` for [`SessionHandle`], which is how the
//! send orchestrator of milestone 006 reaches a session without depending on
//! this crate (ADR 0010).
//!
//! Not here: the send orchestration (milestone 006), windowing and rate
//! control (milestone 007), TLS (milestone 015).

mod error;
mod messaging;
mod pending;

pub mod actors;
pub mod profile;
pub mod reconnect;
pub mod registry;
pub mod state;

#[cfg(feature = "test-support")]
pub mod testing;

pub use actors::transport::{self as transport, TcpTransport, Transport};
pub use actors::{spawn, Session, SessionHandle, SessionSnapshot, MAX_MISSED_ENQUIRE_LINKS};
pub use error::{ProfileRejection, SessionError};
pub use registry::SessionRegistry;

/// Crate version, as declared in its manifest.
///
/// ```
/// assert!(!smpp_session::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
