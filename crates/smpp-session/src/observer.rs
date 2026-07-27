//! Watching the PDUs that cross a session's socket.
//!
//! # Why this trait is synchronous, and why that is the whole design
//!
//! Its two call sites are the reader and the writer — the tasks that own the
//! socket. Anything that **awaits** there paces the session: a database write
//! of a hundred microseconds per PDU turns a debug switch into a throughput
//! ceiling, and a slow disk turns it into a stall that looks like a dead
//! message centre.
//!
//! So the contract is: [`PduObserver::saw`] returns immediately and does no
//! I/O. The implementation that exists (`src-tauri`) pushes onto a **bounded**
//! queue that a dedicated task drains into the recorder; a full queue drops
//! with a warning rather than blocking, because a lost debug entry must never
//! cost a message.
//!
//! # Why the trait lives here and not in `logging-export`
//!
//! `smpp-session` is **below** `logging-export` and must not depend on it
//! (CLAUDE.md §3). The consumer of this port is this crate — it is the one
//! doing the calling — so the port is declared here and implemented above,
//! which is the same inversion `MessageRepository` uses in the other direction.
//!
//! [`PduFlow`] is a local enum rather than `persistence::PduDirection` for the
//! same reason: a session has no business naming a storage type.

use smpp_core::codec::Command;
use smpp_core::types::SessionId;

/// Which way a PDU crossed the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PduFlow {
    /// Read from the message centre.
    Inbound,
    /// Written to the message centre.
    Outbound,
}

/// Sees every PDU crossing one session's socket.
///
/// **Synchronous and non-blocking by contract** — see the module note. An
/// implementation that blocks here does not merely slow itself down; it paces
/// the session.
pub trait PduObserver: Send + Sync + 'static {
    /// A PDU crossed the socket.
    ///
    /// Called for **every** PDU, including the bind — which carries the
    /// credential. Whether anything is recorded is the observer's decision, and
    /// `logging_export::PduRecorder` makes it once per PDU against a switch
    /// that is off by default (CA-008-09).
    fn saw(&self, session_id: SessionId, flow: PduFlow, command: &Command);
}

/// The observer that watches nothing, for a session with no debug facility.
impl PduObserver for () {
    fn saw(&self, _session_id: SessionId, _flow: PduFlow, _command: &Command) {}
}
