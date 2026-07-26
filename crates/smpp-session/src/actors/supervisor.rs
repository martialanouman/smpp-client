//! The supervisor: owns the lifecycle, and every task the session spawns.
//!
//! # Shape of the concurrency
//!
//! One task per session (this one) plus **one** spawned task per connection
//! (the reader). The supervisor is itself the writer: it holds the sending half
//! of the socket and drains the outgoing queue.
//!
//! Collapsing the writer, the keep-alive timer and the correlation-table reaper
//! into one `select!` loop is deliberate. Each of the three only ever produces
//! an outgoing PDU or touches the pending table, so as separate tasks they
//! would need a channel back into the writer to do their job — three tasks and
//! three channels to replace three arms. Fewer tasks is also fewer ways to
//! leave one behind, and CA-005-08 is a statement about exactly that.
//!
//! The reader is spawned because it is the one thing that genuinely blocks: it
//! sits on `stream.next()`, and a session that could not write while waiting
//! for a PDU would never send an `enquire_link`.
//!
//! Nothing here holds a lock across an `.await`. The only lock in the crate is
//! the `tokio::sync::Mutex` inside the correlation table, and every critical
//! section under it is a map operation.
//!
//! # Shutdown
//!
//! One [`CancellationToken`] per session and a child token per connection. On
//! cancellation the supervisor sends `unbind`, waits for `unbind_resp` under a
//! bounded timeout, cancels the child token, joins the reader, and returns.
//! Every request still in flight is failed rather than left waiting for a
//! response that can never come.

use std::sync::Arc;

use core::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use rand::rngs::StdRng;
use rand::SeedableRng as _;
use smpp_core::codec::{Command, Pdu};
use smpp_core::debug as pdu_debug;
use smpp_core::values::{CommandId, CommandStatus};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{sleep_until, Instant};
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;

use crate::actors::framing::SessionCodec;
use crate::actors::reader::{self, ReaderOutcome};
use crate::actors::transport::Transport;
use crate::actors::{connection, SessionSnapshot, MAX_MISSED_ENQUIRE_LINKS};
use crate::error::SessionError;
use crate::pending::{Pending, ResponseResult, ResponseWaiter};
use crate::profile::{Password, SessionProfile};
use crate::reconnect::ReconnectDecision;
use crate::state::SessionState;

/// How long the shutdown waits for `unbind_resp` before closing anyway.
///
/// A message centre that does not answer must not hold the application open:
/// CA-005-08 asks for a bounded wait, not a polite one.
const UNBIND_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the correlation table is swept when nothing else wakes the loop.
///
/// The sweep normally runs at the earliest deadline in the table; this is the
/// idle tick, so an empty table does not spin.
const IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// Everything the supervisor needs to run a session.
pub(crate) struct SupervisorContext<T: Transport> {
    /// The profile being bound.
    pub(crate) profile: SessionProfile,
    /// The credential, held in memory for the life of the session.
    pub(crate) password: Password,
    /// How the socket is obtained.
    pub(crate) transport: T,
    /// The correlation table, shared with the handle and the reader.
    pub(crate) pending: Arc<Pending>,
    /// The receiving half of the outgoing queue. The supervisor is the writer.
    pub(crate) outgoing: mpsc::Receiver<Command>,
    /// A sender onto the same queue, for the reader's responses.
    pub(crate) responses: mpsc::Sender<Command>,
    /// Where `deliver_sm` goes, when anyone is listening.
    pub(crate) deliveries: Option<mpsc::Sender<Command>>,
    /// Where the state is published.
    pub(crate) state: watch::Sender<SessionSnapshot>,
    /// The session-wide shutdown signal.
    pub(crate) token: CancellationToken,
}

/// Runs one session until it is cancelled or gives up.
pub(crate) async fn run<T: Transport>(mut context: SupervisorContext<T>) {
    let session_id = context.profile.session_id();
    let span = tracing::info_span!("session", session_id = %session_id);
    let _entered = span.enter();

    let policy = context.profile.reconnect();
    let mut rng = StdRng::from_os_rng();
    let mut attempt = 0_u32;

    loop {
        if context.token.is_cancelled() {
            publish(&context.state, SessionState::Unbound, None, None);
            return;
        }

        let failure = match attempt_connection(&mut context).await {
            Ok(Stop::Shutdown { .. }) => {
                publish(&context.state, SessionState::Unbound, None, None);
                return;
            }
            Ok(Stop::Retry(error)) | Err(error) => error,
        };

        // Every request that was in flight on the dead connection is lost.
        // Leaving them waiting would hang whatever awaits them.
        let abandoned = context.pending.fail_all().await;
        if abandoned > 0 {
            tracing::warn!(
                abandoned,
                "requests in flight were lost with the connection"
            );
        }

        attempt = attempt.saturating_add(1);

        match policy.decide(&failure, attempt, &mut rng) {
            ReconnectDecision::GiveUp(reason) => {
                tracing::error!(
                    error = %failure,
                    reason = reason.code(),
                    "session stopped and will not retry"
                );
                publish(
                    &context.state,
                    SessionState::Failed,
                    Some(failure.to_string()),
                    Some(reason.code()),
                );

                return;
            }
            ReconnectDecision::RetryAfter(delay) => {
                tracing::warn!(
                    error = %failure,
                    attempt,
                    delay_ms = delay.as_millis(),
                    "session lost, reconnecting after back-off"
                );
                publish(
                    &context.state,
                    SessionState::Reconnecting,
                    Some(failure.to_string()),
                    None,
                );

                tokio::select! {
                    () = context.token.cancelled() => {
                        publish(&context.state, SessionState::Unbound, None, None);
                        return;
                    }
                    () = tokio::time::sleep(delay) => {}
                }
            }
        }

        // A new attempt starts from CLOSED, the only state CONNECTING may
        // follow once the machine has left the nominal path.
        publish(&context.state, SessionState::Closed, None, None);
    }
}

/// What ended a connection attempt.
enum Stop {
    /// The session closed on purpose.
    Shutdown {
        /// Whether an `unbind` still has to be sent. `false` when the peer
        /// unbound us: it asked, we answered, there is nothing left to say.
        needs_unbind: bool,
    },
    /// The connection ended for a reason the reconnection policy has to judge.
    Retry(SessionError),
}

/// One attempt: connect, bind, serve.
async fn attempt_connection<T: Transport>(
    context: &mut SupervisorContext<T>,
) -> Result<Stop, SessionError> {
    publish(&context.state, SessionState::Connecting, None, None);

    let address = context.profile.socket_address();
    let stream = tokio::select! {
        () = context.token.cancelled() => {
            return Ok(Stop::Shutdown { needs_unbind: false });
        }
        result = context.transport.connect(&address) => {
            result.map_err(|source| SessionError::Transport { operation: "connecting", source })?
        }
    };

    let mut framed = connection::frame(stream);

    publish(&context.state, SessionState::Binding, None, None);

    tokio::select! {
        () = context.token.cancelled() => {
            return Ok(Stop::Shutdown { needs_unbind: false });
        }
        result = connection::bind(&mut framed, &context.profile, &context.password) => result?,
    }

    let mode = context.profile.bind_mode();
    tracing::info!(mode = ?mode, version = context.profile.version().label(), "session bound");
    publish(&context.state, SessionState::Bound(mode), None, None);

    Ok(serve(framed, context).await)
}

/// The bound phase: read, write, keep alive, sweep.
async fn serve<T: Transport>(
    framed: Framed<T::Stream, SessionCodec>,
    context: &mut SupervisorContext<T>,
) -> Stop {
    let (mut sink, stream) = framed.split();

    // A token of its own, NOT a child of the session token, and the difference
    // is load-bearing. A child is cancelled the instant the parent is, so a
    // shutdown would race: the reader would often stop before the supervisor
    // reached the `unbind`, and the reader stopping is itself a reason to end
    // the connection — with `needs_unbind: false`. The session would then
    // close without ever saying goodbye, intermittently, which is the kind of
    // failure that looks like a flaky test.
    //
    // The supervisor cancels this one explicitly, after the `unbind` has been
    // sent and answered.
    let connection_token = CancellationToken::new();

    let mut reader_handle: JoinHandle<ReaderOutcome> = tokio::spawn(reader::run(
        stream,
        Arc::clone(&context.pending),
        context.responses.clone(),
        context.deliveries.clone(),
        connection_token.clone(),
    ));
    let mut reader_finished = false;

    let interval = context.profile.enquire_link_interval();
    let timeout = context.profile.response_timeout();
    let mut next_keepalive = keepalive_deadline(interval);
    let mut keepalive_waiter: Option<ResponseWaiter> = None;
    let mut missed_keepalives = 0_u32;

    let stop = loop {
        // Moved out of the loop variable for the duration of the `select!`:
        // an arm's future borrows the slot, and the handling that replaces it
        // runs after the macro has returned. See [`Event`].
        let mut waiting = keepalive_waiter.take();
        let sweep_at = context
            .pending
            .next_deadline()
            .await
            .unwrap_or_else(|| Instant::now() + IDLE_SWEEP_INTERVAL);

        let event = tokio::select! {
            () = context.token.cancelled() => Event::Cancelled,
            joined = &mut reader_handle, if !reader_finished => Event::Reader(joined),
            queued = context.outgoing.recv() => Event::Outgoing(queued),
            () = sleep_at(next_keepalive) => Event::KeepaliveTick,
            result = keepalive_outcome(&mut waiting) => Event::KeepaliveResult(result),
            () = sleep_until(sweep_at) => Event::Sweep,
        };

        keepalive_waiter = waiting;

        match event {
            Event::Cancelled => break Stop::Shutdown { needs_unbind: true },
            Event::Reader(joined) => {
                reader_finished = true;

                break match joined {
                    Ok(ReaderOutcome::PeerUnbound | ReaderOutcome::Cancelled) => Stop::Shutdown {
                        needs_unbind: false,
                    },
                    Ok(ReaderOutcome::LinkLost(source)) => Stop::Retry(SessionError::Transport {
                        operation: "reading",
                        source,
                    }),
                    Ok(ReaderOutcome::WriterGone) => Stop::Retry(SessionError::Closed),
                    Err(error) => Stop::Retry(SessionError::Transport {
                        operation: "reading",
                        source: join_failure(&error),
                    }),
                };
            }
            // The handle dropped its sender: nothing will ever be submitted
            // again, so the session closes rather than idling on a socket.
            Event::Outgoing(None) => break Stop::Shutdown { needs_unbind: true },
            Event::Outgoing(Some(command)) => {
                tracing::trace!(pdu = %pdu_debug::redacted(&command), "writing PDU");

                if let Err(source) = sink.send(command).await {
                    break Stop::Retry(SessionError::Transport {
                        operation: "writing",
                        source,
                    });
                }
            }
            Event::KeepaliveTick => {
                next_keepalive = keepalive_deadline(interval);

                match enquire_link(&context.pending, timeout).await {
                    Ok((command, waiter)) => {
                        keepalive_waiter = Some(waiter);

                        if let Err(source) = sink.send(command).await {
                            break Stop::Retry(SessionError::Transport {
                                operation: "writing the enquire_link",
                                source,
                            });
                        }
                    }
                    Err(error) => break Stop::Retry(error),
                }
            }
            Event::KeepaliveResult(Ok(Ok(_))) => {
                // The receiver is spent: polling it again panics. Clearing the
                // slot is what makes the arm inert until the next tick refills
                // it.
                keepalive_waiter = None;
                missed_keepalives = 0;
            }
            Event::KeepaliveResult(Ok(Err(_)) | Err(_)) => {
                keepalive_waiter = None;
                missed_keepalives = missed_keepalives.saturating_add(1);

                // CA-005-04 — a TCP socket can stay open long after the
                // session behind it is gone. The missing response is the only
                // signal there is, and past the threshold it is read as a dead
                // link rather than as a slow one.
                tracing::warn!(
                    missed = missed_keepalives,
                    limit = MAX_MISSED_ENQUIRE_LINKS,
                    "enquire_link went unanswered"
                );

                if missed_keepalives >= MAX_MISSED_ENQUIRE_LINKS {
                    break Stop::Retry(SessionError::Transport {
                        operation: "keeping the session alive",
                        source: std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "the message centre stopped answering enquire_link",
                        ),
                    });
                }
            }
            Event::Sweep => {
                let expired = context.pending.expire(Instant::now()).await;

                if expired > 0 {
                    tracing::debug!(expired, "requests timed out");
                }
            }
        }
    };

    if matches!(stop, Stop::Shutdown { needs_unbind: true }) {
        unbind(&mut sink, &context.pending, &context.profile).await;
    }

    // No task outlives this function (CA-005-08): the child token stops the
    // reader, and the join is awaited rather than detached. Awaiting a handle
    // that already completed would panic, hence the flag.
    connection_token.cancel();

    if !reader_finished {
        let _joined = (&mut reader_handle).await;
    }

    stop
}

/// The events the bound loop reacts to.
///
/// Every arm produces a value that borrows nothing, and all the handling
/// happens after the `select!` returns. That is not a style preference: an arm
/// body that touched `sink` or the keep-alive slot while another arm's future
/// still borrowed them would not compile, and working around it with a lock
/// would put a lock on the hot path.
enum Event {
    /// The session was asked to shut down.
    Cancelled,
    /// The reader task ended.
    Reader(Result<ReaderOutcome, JoinError>),
    /// A PDU to write, or the queue closing.
    Outgoing(Option<Command>),
    /// The keep-alive period elapsed.
    KeepaliveTick,
    /// The outstanding `enquire_link` was answered, timed out or cancelled.
    KeepaliveResult(Result<ResponseResult, oneshot::error::RecvError>),
    /// Time to sweep the correlation table.
    Sweep,
}

/// When the next `enquire_link` is due, or never when the keep-alive is off.
fn keepalive_deadline(interval: Duration) -> Option<Instant> {
    (!interval.is_zero()).then(|| Instant::now() + interval)
}

/// Sleeps until `deadline`, or for ever when there is none.
async fn sleep_at(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Awaits the outstanding `enquire_link` response, or for ever when there is
/// none in flight.
async fn keepalive_outcome(
    slot: &mut Option<ResponseWaiter>,
) -> Result<ResponseResult, oneshot::error::RecvError> {
    match slot.as_mut() {
        Some(waiter) => waiter.await,
        None => std::future::pending().await,
    }
}

/// Registers and builds an `enquire_link`.
async fn enquire_link(
    pending: &Pending,
    timeout: Duration,
) -> Result<(Command, ResponseWaiter), SessionError> {
    let (sequence, waiter) = pending.register(CommandId::EnquireLink, timeout).await?;

    Ok((
        Command::new(CommandStatus::EsmeRok, sequence.get(), Pdu::EnquireLink),
        waiter,
    ))
}

/// Sends `unbind` and waits, briefly, for its response.
///
/// Failures are logged and swallowed: we are closing, and a message centre that
/// will not say goodbye must not keep the application open.
async fn unbind<W>(sink: &mut W, pending: &Pending, profile: &SessionProfile)
where
    W: futures_util::Sink<Command, Error = std::io::Error> + Unpin,
{
    let Ok((sequence, waiter)) = pending.register(CommandId::Unbind, UNBIND_TIMEOUT).await else {
        tracing::warn!("could not register the unbind, closing anyway");
        return;
    };

    if let Err(error) = sink
        .send(Command::new(
            CommandStatus::EsmeRok,
            sequence.get(),
            Pdu::Unbind,
        ))
        .await
    {
        tracing::warn!(error = %error, "could not send the unbind, closing anyway");
        return;
    }

    match tokio::time::timeout(UNBIND_TIMEOUT, waiter).await {
        Ok(Ok(Ok(_))) => tracing::info!(session = %profile.name(), "session unbound cleanly"),
        Ok(_) => tracing::warn!("the unbind was refused or lost, closing anyway"),
        Err(_) => tracing::warn!(
            timeout_s = UNBIND_TIMEOUT.as_secs(),
            "no unbind_resp within the timeout, closing anyway"
        ),
    }
}

/// Turns a join failure into the `io::Error` the session error carries.
fn join_failure(error: &JoinError) -> std::io::Error {
    std::io::Error::other(format!("the reader task ended abnormally: {error}"))
}

/// Publishes a state, validating the edge against spec §7.9.
///
/// An illegal edge is logged and **applied**: refusing it would leave the
/// interface showing a state the session is no longer in, which is a worse
/// failure than the inconsistency the log records. The unit tests of
/// [`crate::state`] are where an illegal edge is meant to be caught.
fn publish(
    state: &watch::Sender<SessionSnapshot>,
    next: SessionState,
    error: Option<String>,
    give_up: Option<&'static str>,
) {
    state.send_modify(|snapshot| {
        if let Err(rejected) = snapshot.state.try_transition(next) {
            tracing::error!(error = %rejected, "the session took an edge spec §7.9 does not draw");
        }

        snapshot.state = next;
        snapshot.last_error = error;
        snapshot.give_up = give_up;
    });
}
