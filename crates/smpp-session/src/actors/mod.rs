//! The actors of one session, and the handle the rest of the application holds.
//!
//! ```text
//!            ┌──────────────────── SessionHandle (Clone) ───────────────────┐
//!            │  request()            watch::Receiver          shutdown()    │
//!            └──────┬──────────────────────▲──────────────────────┬─────────┘
//!    bounded mpsc   │                      │ watch                │ CancellationToken
//!                   ▼                      │                      ▼
//!            ┌───────────────────── supervisor task ────────────────────────┐
//!            │  connect · bind · WRITE · keep-alive · reap · reconnect      │
//!            └──────┬───────────────────────────────────────────────────────┘
//!                   │ owns the sending half           spawns, owns, joins
//!                   ▼                                          ▼
//!              SplitSink                              ┌── reader task ──┐
//!                                                     │  READ · resolve │
//!                                                     └─────────────────┘
//! ```
//!
//! Two tasks, four channels, no shared mutable state except the correlation
//! table — which is a `tokio::sync::Mutex` whose critical sections are map
//! operations and never span an `.await`.
//!
//! Every queue is **bounded**. That is what stops a campaign from exhausting
//! memory when the message centre slows down (CLAUDE.md §4): a full outgoing
//! queue makes the submitter wait, which is the signal, rather than growing a
//! buffer nobody is watching.

mod connection;
mod framing;
mod reader;
mod supervisor;
pub mod transport;

use std::sync::Arc;

use smpp_core::codec::{Command, Pdu};
use smpp_core::types::SessionId;
use smpp_core::values::CommandStatus;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::SessionError;
use crate::pending::Pending;
use crate::profile::{Password, SessionProfile};
use crate::state::{BindMode, SessionState};
use transport::Transport;

/// Unanswered `enquire_link`s a session tolerates before declaring the link
/// dead (CA-005-04).
///
/// Two, not one. A single lost PDU on a congested link is ordinary and does not
/// mean the session is gone; two consecutive misses, each one
/// `response_timeout` long, is no longer a coincidence. Higher would delay the
/// reconnection by a whole extra period for no additional certainty.
pub const MAX_MISSED_ENQUIRE_LINKS: u32 = 2;

/// PDUs the outgoing queue holds before a submitter has to wait.
///
/// Deliberately small. The queue is a hand-off, not a buffer: the place where
/// pending work belongs is the message journal, which is durable, and not a
/// `Vec` that a crash loses. Milestone 007 sizes the real in-flight window
/// from `window_size`.
const OUTGOING_QUEUE_CAPACITY: usize = 64;

/// `deliver_sm` PDUs held before the oldest is dropped.
const DELIVERY_QUEUE_CAPACITY: usize = 256;

/// What a session publishes about itself.
///
/// Read through [`SessionHandle::snapshot`] or watched through
/// [`SessionHandle::watch`]. Everything here is safe to show a user: the error
/// is a rendered [`SessionError`], and no variant of that type carries a
/// credential (there is a test).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// Where the session stands (spec §7.9).
    pub state: SessionState,
    /// The last failure, rendered. `None` while things are going well.
    pub last_error: Option<String>,
    /// Why the session stopped for good, when it did.
    ///
    /// A stable code — `FATAL_STATUS`, `RECONNECT_DISABLED` — because the
    /// interface translates it (CLAUDE.md §4: no hard-coded user text).
    pub give_up: Option<&'static str>,
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self {
            state: SessionState::Closed,
            last_error: None,
            give_up: None,
        }
    }
}

/// A handle onto a running session. Cheap to clone.
#[derive(Clone)]
pub struct SessionHandle {
    inner: Arc<HandleInner>,
}

/// The shared half of a handle.
struct HandleInner {
    session_id: SessionId,
    bind_mode: BindMode,
    response_timeout: core::time::Duration,
    pending: Arc<Pending>,
    outgoing: mpsc::Sender<Command>,
    state: watch::Receiver<SessionSnapshot>,
    token: CancellationToken,
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl core::fmt::Debug for SessionHandle {
    /// Names the session and its state. Never the profile, which would drag
    /// the `system_id` into every log line that formats a handle.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionHandle")
            .field("session_id", &self.inner.session_id)
            .field("state", &self.snapshot().state)
            .finish()
    }
}

/// A running session: its handle, and the queue of unsolicited PDUs.
pub struct Session {
    /// The handle the application keeps.
    pub handle: SessionHandle,
    /// `deliver_sm` PDUs the message centre pushed.
    ///
    /// Milestone 008 reads delivery receipts from here. A caller that does not
    /// drain it is not a bug — the queue is bounded and overflow is dropped
    /// with a warning rather than blocking the session.
    pub deliveries: mpsc::Receiver<Command>,
}

/// Starts a session: connect, bind, and keep it up.
///
/// Returns as soon as the tasks are spawned; the caller watches
/// [`SessionHandle::watch`] to learn when the bind completes. Nothing is
/// detached — [`SessionHandle::shutdown`] joins every task it started.
///
/// The `password` is moved in and never leaves: it is not persisted, not
/// logged, and not reachable from the handle.
pub fn spawn<T: Transport>(profile: SessionProfile, password: Password, transport: T) -> Session {
    let (outgoing_tx, outgoing_rx) = mpsc::channel(OUTGOING_QUEUE_CAPACITY);
    let (delivery_tx, delivery_rx) = mpsc::channel(DELIVERY_QUEUE_CAPACITY);
    let (state_tx, state_rx) = watch::channel(SessionSnapshot::default());

    let pending = Arc::new(Pending::new());
    let token = CancellationToken::new();

    let inner = Arc::new(HandleInner {
        session_id: profile.session_id(),
        bind_mode: profile.bind_mode(),
        response_timeout: profile.response_timeout(),
        pending: Arc::clone(&pending),
        outgoing: outgoing_tx.clone(),
        state: state_rx,
        token: token.clone(),
        supervisor: Mutex::new(None),
    });

    let context = supervisor::SupervisorContext {
        profile,
        password,
        transport,
        pending,
        outgoing: outgoing_rx,
        responses: outgoing_tx,
        deliveries: Some(delivery_tx),
        state: state_tx,
        token,
    };

    let handle = tokio::spawn(supervisor::run(context));

    // The `JoinHandle` is parked on the handle rather than dropped: a dropped
    // handle detaches the task, and a detached task is precisely the orphan
    // CLAUDE.md §4 forbids. `shutdown` awaits it.
    //
    // `try_lock` cannot fail here — nothing else has seen `inner` yet — but an
    // `expect` would be a `panic!` in production code, so the fallback is to
    // cancel rather than to leak.
    match inner.supervisor.try_lock() {
        Ok(mut slot) => *slot = Some(handle),
        Err(_) => {
            handle.abort();
            inner.token.cancel();
        }
    }

    Session {
        handle: SessionHandle { inner },
        deliveries: delivery_rx,
    }
}

impl SessionHandle {
    /// The session this handle drives.
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.inner.session_id
    }

    /// The current state, without waiting.
    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        self.inner.state.borrow().clone()
    }

    /// A receiver that yields every state change (spec §7.9).
    ///
    /// `watch` rather than a broadcast queue: a subscriber that falls behind
    /// gets the *latest* state, which is the only one worth showing, instead of
    /// replaying a history the interface would render and immediately discard.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<SessionSnapshot> {
        self.inner.state.clone()
    }

    /// Sends a request and waits for its response.
    ///
    /// The `sequence_number` is allocated here, from the correlation table, so
    /// a value still in flight is never reused and a late response cannot be
    /// attributed to a later request.
    ///
    /// # Errors
    ///
    /// * [`SessionError::NotBound`] if the session is not bound right now;
    /// * [`SessionError::OperationNotAllowed`] if the operation is illegal on
    ///   this bind type — submitting on a receiver session, for instance
    ///   (CA-005-02). Refused **here**, before the PDU leaves, rather than by
    ///   an `ESME_RINVBNDSTS` from the message centre;
    /// * [`SessionError::ResponseTimeout`] if no response arrives in time;
    /// * [`SessionError::Cancelled`] if the session drops while the request is
    ///   in flight;
    /// * [`SessionError::Closed`] if the session is gone.
    pub async fn request(&self, pdu: Pdu) -> Result<Command, SessionError> {
        let state = self.snapshot().state;
        let Some(mode) = state.bind_mode() else {
            return Err(SessionError::NotBound {
                state: state.code(),
            });
        };

        let operation = pdu.command_id();

        if !mode.allows(operation) {
            return Err(SessionError::OperationNotAllowed { operation, mode });
        }

        let (sequence, waiter) = self
            .inner
            .pending
            .register(operation, self.inner.response_timeout)
            .await?;

        let command = Command::new(CommandStatus::EsmeRok, sequence.get(), pdu);

        if self.inner.outgoing.send(command).await.is_err() {
            return Err(SessionError::Closed);
        }

        waiter.await.unwrap_or(Err(SessionError::Cancelled))
    }

    /// How many requests are waiting for a response right now.
    ///
    /// The interface shows it next to the window (spec §18.1), and it is the
    /// number CA-005-06 is about: a session that has been idle for longer than
    /// `response_timeout` must report zero.
    pub async fn in_flight(&self) -> usize {
        self.inner.pending.len().await
    }

    /// The bind type the profile asked for.
    ///
    /// Note the difference from `snapshot().state.bind_mode()`: this is what
    /// was *configured*, available even while the session is down, whereas the
    /// state carries what is *in force*.
    #[must_use]
    pub fn configured_bind_mode(&self) -> BindMode {
        self.inner.bind_mode
    }

    /// Closes the session and waits for every task it started to finish.
    ///
    /// Sends `unbind`, waits for `unbind_resp` under a bounded timeout, then
    /// stops. Calling it twice is harmless: the second call finds no task left
    /// to join.
    ///
    /// # Errors
    ///
    /// [`SessionError::Cancelled`] if the supervisor task ended abnormally,
    /// which can only be a cancellation from outside.
    pub async fn shutdown(&self) -> Result<(), SessionError> {
        self.inner.token.cancel();

        let handle = self.inner.supervisor.lock().await.take();

        match handle {
            Some(handle) => handle.await.map_err(|_| SessionError::Cancelled),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_snapshot_is_closed_and_carries_no_failure() {
        let snapshot = SessionSnapshot::default();

        assert_eq!(snapshot.state, SessionState::Closed);
        assert!(snapshot.last_error.is_none());
        assert!(snapshot.give_up.is_none());
    }

    /// A statement, not a computation: `mpsc::channel(0)` panics, and an
    /// unbounded queue is the bug CLAUDE.md §4 names. Both are checked at
    /// compile time, so a change to either constant fails the build rather
    /// than a test run.
    const _: () = {
        assert!(OUTGOING_QUEUE_CAPACITY > 0);
        assert!(DELIVERY_QUEUE_CAPACITY > 0);
        assert!(MAX_MISSED_ENQUIRE_LINKS >= 1);
    };
}
