//! The reader actor: the only task that reads from the socket.
//!
//! It does three things and nothing else:
//!
//! 1. **resolves** a response against the correlation table;
//! 2. **answers** the requests a message centre may send unprompted —
//!    `enquire_link`, `unbind`, `deliver_sm` — by pushing a response onto the
//!    outgoing queue, never by writing to the socket itself;
//! 3. **reports** why it stopped, so the supervisor can tell a clean unbind
//!    from a dropped link.
//!
//! Every response it produces goes through the same bounded queue the rest of
//! the application uses. That is what keeps "one task owns the writing half"
//! true (CA-005-10) without a lock.

use std::sync::Arc;

use futures_util::StreamExt as _;
use smpp_core::codec::{Command, Pdu};
use smpp_core::debug as pdu_debug;
use smpp_core::values::{CommandId, CommandStatus};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::actors::framing::DecodedFrame;
use crate::pending::Pending;

/// Why the reader stopped.
#[derive(Debug)]
pub(crate) enum ReaderOutcome {
    /// The peer sent `unbind`; we answered and stopped. A clean close.
    PeerUnbound,
    /// The stream ended, or the socket failed.
    LinkLost(std::io::Error),
    /// The connection token was cancelled: we are shutting down.
    Cancelled,
    /// The outgoing queue is gone, so no response can be written any more.
    WriterGone,
}

/// Runs the reader until the stream ends or the token is cancelled.
pub(crate) async fn run<S>(
    mut stream: S,
    pending: Arc<Pending>,
    outgoing: mpsc::Sender<Command>,
    deliveries: Option<mpsc::Sender<Command>>,
    token: CancellationToken,
) -> ReaderOutcome
where
    S: futures_util::Stream<Item = Result<DecodedFrame, std::io::Error>> + Unpin + Send,
{
    loop {
        let frame = tokio::select! {
            () = token.cancelled() => return ReaderOutcome::Cancelled,
            frame = stream.next() => frame,
        };

        let outcome = match frame {
            None => {
                return ReaderOutcome::LinkLost(std::io::Error::from(
                    std::io::ErrorKind::UnexpectedEof,
                ))
            }
            Some(Err(error)) => return ReaderOutcome::LinkLost(error),
            Some(Ok(Err(error))) => {
                // CA-005-07 — a PDU that will not parse. The frame boundary
                // was taken from `command_length`, so the stream is still
                // aligned (see `framing`); the session stays up, the incident
                // is logged, and `generic_nack` tells the peer.
                //
                // `sequence_number` is 0: the header did not survive the
                // parse, so there is nothing honest to echo. Spec §7.1 allows
                // it, and inventing one would risk cancelling an unrelated
                // request on the peer's side.
                tracing::warn!(error = %error, "malformed PDU, answering generic_nack");

                send(&outgoing, nack(0), &token).await
            }
            Some(Ok(Ok(command))) => {
                handle(command, &pending, &outgoing, deliveries.as_ref(), &token).await
            }
        };

        match outcome {
            Step::Continue => {}
            Step::Stop(outcome) => return outcome,
        }
    }
}

/// What handling one PDU decided about the reader's own future.
enum Step {
    /// Keep reading.
    Continue,
    /// Stop, for this reason.
    Stop(ReaderOutcome),
}

/// Dispatches one well-formed PDU.
async fn handle(
    command: Command,
    pending: &Pending,
    outgoing: &mpsc::Sender<Command>,
    deliveries: Option<&mpsc::Sender<Command>>,
    token: &CancellationToken,
) -> Step {
    let sequence = command.sequence_number();

    if command.id().is_response() {
        if !pending.resolve(sequence, command).await {
            // Not an error, and not fatal: a response to a request that has
            // already timed out is ordinary on a slow link. Logged because a
            // stream of them means the response timeout is set too low.
            tracing::debug!(sequence, "response arrived with nothing waiting for it");
        }

        return Step::Continue;
    }

    match command.id() {
        CommandId::EnquireLink => {
            send(outgoing, response(sequence, Pdu::EnquireLinkResp), token).await
        }
        CommandId::Unbind => {
            tracing::info!(sequence, "the message centre is unbinding the session");

            match send(outgoing, response(sequence, Pdu::UnbindResp), token).await {
                Step::Continue => Step::Stop(ReaderOutcome::PeerUnbound),
                stop => stop,
            }
        }
        CommandId::DeliverSm => {
            let answer = send(
                outgoing,
                response(
                    sequence,
                    Pdu::DeliverSmResp(smpp_core::pdus::DeliverSmResp::default()),
                ),
                token,
            )
            .await;

            if let Some(deliveries) = deliveries {
                // `try_send`, not `send`. The consumer of this queue is
                // optional — milestone 008 is the one that reads receipts —
                // and blocking the reader on a queue nobody drains would stall
                // the whole session, `enquire_link` included. Dropping with a
                // warning is the lesser failure, and it is visible.
                if deliveries.try_send(command).is_err() {
                    tracing::warn!(sequence, "delivery queue full or closed, receipt dropped");
                }
            }

            answer
        }
        operation => {
            // Well-formed, correctly framed, and not something an ESME
            // answers. `generic_nack` with `ESME_RINVCMDID` is what spec §7.2
            // prescribes, and the session carries on (CA-005-07).
            tracing::warn!(
                ?operation,
                sequence,
                "unexpected operation, answering generic_nack"
            );

            send(outgoing, nack(sequence), token).await
        }
    }
}

/// A response command, built for the sequence number it answers.
fn response(sequence: u32, pdu: Pdu) -> Command {
    Command::new(CommandStatus::EsmeRok, sequence, pdu)
}

/// A `generic_nack` for an operation we could not accept.
fn nack(sequence: u32) -> Command {
    Command::new(CommandStatus::EsmeRinvcmdid, sequence, Pdu::GenericNack)
}

/// Pushes a command onto the outgoing queue, watching the shutdown signal.
///
/// The queue is **bounded**, so this awaits when the writer is behind — which
/// is the back-pressure CLAUDE.md §4 asks for, applied to our own responses as
/// much as to a campaign's.
///
/// # Why the token is in the `select!`
///
/// It was not, and the pair of tasks could deadlock permanently. Once the
/// supervisor leaves its loop nothing drains this queue, and the supervisor
/// holds a `Sender` on it too — so the channel never closes and the `await`
/// cannot even fail. The reader parked here for ever; the supervisor sent its
/// `unbind`, cancelled the token the reader was no longer in a position to
/// observe, and then waited on the join. Neither ever moved, and `shutdown()`
/// never returned.
///
/// Answering the token here is the fix; the bounded join in the supervisor is
/// the belt to this pair of braces.
async fn send(
    outgoing: &mpsc::Sender<Command>,
    command: Command,
    token: &CancellationToken,
) -> Step {
    tracing::trace!(pdu = %pdu_debug::redacted(&command), "queuing outgoing PDU");

    tokio::select! {
        () = token.cancelled() => Step::Stop(ReaderOutcome::Cancelled),
        result = outgoing.send(command) => {
            if result.is_err() {
                Step::Stop(ReaderOutcome::WriterGone)
            } else {
                Step::Continue
            }
        }
    }
}
