//! The `sequence_number` → response correlation table.
//!
//! ADR 0001 settles that this is ours to hold rather than the SMPP client
//! library's: bounded windowing (milestone 007), a per-PDU response timeout
//! and a per-PDU round-trip measurement all hang off request/response
//! pairing, and a high-level client that owns the pairing owns all three.
//!
//! # What can go wrong here, and what stops it
//!
//! **A late response attributed to the wrong request.** A `sequence_number`
//! is never reused while it is still in flight: [`Pending::register`] scans
//! for a free slot instead of taking the next value blindly. Handing out a
//! number that a straggler is about to answer would silently mark the wrong
//! message accepted.
//!
//! **A waiter that never leaves the table.** A response that never comes, a
//! cancelled request, a session that dies mid-flight — all three would leak an
//! entry, and a campaign makes millions of requests. Every entry carries a
//! deadline and [`Pending::expire`] removes it whether or not anyone is still
//! listening (CA-005-06). The waiter learns of it through its own channel: no
//! `Drop` implementation is involved, because a `Drop` cannot `await` the lock
//! it would need.

use std::collections::HashMap;

use core::time::Duration;

use smpp_core::codec::Command;
use smpp_core::types::SequenceNumber;
use smpp_core::values::CommandId;
use tokio::sync::{oneshot, Mutex};
use tokio::time::Instant;

use crate::error::SessionError;

/// What a caller waiting on a response receives.
pub(crate) type ResponseResult = Result<Command, SessionError>;

/// The receiving half handed back by [`Pending::register`].
pub(crate) type ResponseWaiter = oneshot::Receiver<ResponseResult>;

/// One request in flight.
struct Waiter {
    /// The request that was sent — named in the timeout error, and checked
    /// against the response so a mismatched pairing is caught rather than
    /// handed to the caller.
    operation: CommandId,
    /// How long the caller agreed to wait — reported in the timeout error, so
    /// a log line says how patient the session actually was.
    timeout: Duration,
    /// When this entry stops being worth keeping.
    deadline: Instant,
    /// Where the response, or the timeout, is delivered.
    sender: oneshot::Sender<ResponseResult>,
}

/// The mutable half, behind the lock.
struct Inner {
    /// Where the next allocation starts scanning from.
    cursor: SequenceNumber,
    /// Requests in flight, keyed by `sequence_number`.
    waiting: HashMap<u32, Waiter>,
}

/// The correlation table of one session.
///
/// `tokio::sync::Mutex`, not `std::sync::Mutex` (CLAUDE.md §4). Nothing here
/// `await`s while holding it — every critical section is a map operation — but
/// the type is what makes that impossible to get wrong later, and the
/// workspace `clippy.toml` refuses the `std` one outright.
pub(crate) struct Pending {
    inner: Mutex<Inner>,
}

impl Pending {
    /// An empty table, allocating from [`SequenceNumber::FIRST`].
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                cursor: SequenceNumber::FIRST,
                waiting: HashMap::new(),
            }),
        }
    }

    /// Reserves a `sequence_number` and returns the channel its response will
    /// arrive on.
    ///
    /// The number is free at the moment it is handed out: a value still
    /// awaiting a response is skipped, whatever the cursor says.
    ///
    /// # Errors
    ///
    /// [`SessionError::SequenceSpaceExhausted`] when every number of
    /// `1..=0x7FFFFFFF` is in flight. Unreachable with any real window, and an
    /// error rather than a wrap because the alternative is a misattributed
    /// response.
    pub(crate) async fn register(
        &self,
        operation: CommandId,
        timeout: Duration,
    ) -> Result<(SequenceNumber, ResponseWaiter), SessionError> {
        let deadline = Instant::now() + timeout;
        let mut inner = self.inner.lock().await;

        let sequence = Self::allocate(&mut inner)?;
        let (sender, receiver) = oneshot::channel();

        inner.waiting.insert(
            sequence.get(),
            Waiter {
                operation,
                timeout,
                deadline,
                sender,
            },
        );

        Ok((sequence, receiver))
    }

    /// Finds a free `sequence_number`, starting at the cursor.
    ///
    /// Only `waiting.len()` numbers are taken, so a run of `len() + 1`
    /// consecutive values is guaranteed to contain a free one — which is what
    /// bounds the scan.
    fn allocate(inner: &mut Inner) -> Result<SequenceNumber, SessionError> {
        let probes = inner.waiting.len().saturating_add(1);

        for _ in 0..probes {
            let candidate = inner.cursor;
            inner.cursor = candidate.next();

            if !inner.waiting.contains_key(&candidate.get()) {
                return Ok(candidate);
            }
        }

        Err(SessionError::SequenceSpaceExhausted {
            in_flight: inner.waiting.len(),
        })
    }

    /// Delivers a response to the request it answers.
    ///
    /// Returns `false` when nothing was waiting on that `sequence_number` —
    /// a response to a request that already timed out, or one the SMSC
    /// invented. The caller logs it and carries on: an unsolicited response is
    /// not a reason to drop a session (CA-005-07).
    pub(crate) async fn resolve(&self, sequence: u32, command: Command) -> bool {
        let Some(waiter) = self.inner.lock().await.waiting.remove(&sequence) else {
            return false;
        };

        let expected = waiter.operation.matching_response();
        let response = if command.id() == expected {
            Ok(command)
        } else {
            Err(SessionError::UnexpectedResponse {
                expected,
                actual: command.id(),
            })
        };

        // `send` fails when the caller has gone away, which is ordinary: the
        // entry is removed either way, and that is the point.
        let _ignored = waiter.sender.send(response);

        true
    }

    /// Removes every entry whose deadline has passed, telling its waiter.
    ///
    /// Returns how many were removed. This is the guarantee behind CA-005-06:
    /// an entry leaves the table on its deadline whether or not anyone is
    /// still listening, so a cancelled request cannot leak.
    pub(crate) async fn expire(&self, now: Instant) -> usize {
        let mut inner = self.inner.lock().await;

        let expired: Vec<u32> = inner
            .waiting
            .iter()
            .filter(|(_, waiter)| waiter.deadline <= now || waiter.sender.is_closed())
            .map(|(sequence, _)| *sequence)
            .collect();

        for sequence in &expired {
            let Some(waiter) = inner.waiting.remove(sequence) else {
                continue;
            };

            let error = SequenceNumber::new(*sequence).map_or_else(
                |_| SessionError::Cancelled,
                |sequence| SessionError::ResponseTimeout {
                    operation: waiter.operation,
                    sequence,
                    timeout: waiter.timeout,
                },
            );

            let _ignored = waiter.sender.send(Err(error));
        }

        Self::reclaim(&mut inner);

        expired.len()
    }

    /// Fails every entry at once, and empties the table.
    ///
    /// Called when the link drops or the session shuts down: every request in
    /// flight is lost, and a caller left waiting for a response that can never
    /// come is a hung campaign.
    pub(crate) async fn fail_all(&self) -> usize {
        let mut inner = self.inner.lock().await;
        let count = inner.waiting.len();

        for (_, waiter) in inner.waiting.drain() {
            let _ignored = waiter.sender.send(Err(SessionError::Cancelled));
        }

        Self::reclaim(&mut inner);

        count
    }

    /// Gives the map's memory back once it empties.
    ///
    /// A `HashMap` keeps the capacity its largest population needed. After a
    /// burst of ten thousand expired requests the entries are gone but the
    /// buckets are not, and "no memory leak" would then be true only of the
    /// entry count.
    fn reclaim(inner: &mut Inner) {
        if inner.waiting.is_empty() {
            inner.waiting.shrink_to_fit();
        }
    }

    /// How many requests are in flight.
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.waiting.len()
    }

    /// The earliest deadline in the table, if any.
    ///
    /// What the reaper sleeps until, so it wakes once per expiry rather than
    /// on a fixed tick.
    pub(crate) async fn next_deadline(&self) -> Option<Instant> {
        self.inner
            .lock()
            .await
            .waiting
            .values()
            .map(|waiter| waiter.deadline)
            .min()
    }
}

#[cfg(test)]
// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use smpp_core::codec::Pdu;
    use smpp_core::values::CommandStatus;

    fn response(sequence: u32) -> Command {
        Command::new(CommandStatus::EsmeRok, sequence, Pdu::EnquireLinkResp)
    }

    const TIMEOUT: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn a_registered_request_is_resolved_by_its_own_response() {
        let pending = Pending::new();

        let (sequence, waiter) = pending
            .register(CommandId::EnquireLink, TIMEOUT)
            .await
            .expect("a fresh table always has room");

        assert_eq!(pending.len().await, 1);
        assert!(
            pending
                .resolve(sequence.get(), response(sequence.get()))
                .await
        );

        let command = waiter
            .await
            .expect("the sender is alive")
            .expect("the response matches the request");

        assert_eq!(command.id(), CommandId::EnquireLinkResp);
        assert_eq!(pending.len().await, 0);
    }

    #[tokio::test]
    async fn a_response_nobody_is_waiting_for_is_reported_rather_than_fatal() {
        let pending = Pending::new();

        assert!(
            !pending.resolve(4_242, response(4_242)).await,
            "an unsolicited response resolves nothing"
        );
        assert_eq!(pending.len().await, 0);
    }

    /// The bug this whole module exists to prevent: two requests in flight, the
    /// first answered late, and the answer landing on the second.
    #[tokio::test]
    async fn a_late_response_cannot_be_attributed_to_a_later_request() {
        let pending = Pending::new();

        let (first, first_waiter) = pending
            .register(CommandId::EnquireLink, TIMEOUT)
            .await
            .expect("room");
        let (second, second_waiter) = pending
            .register(CommandId::EnquireLink, TIMEOUT)
            .await
            .expect("room");

        assert_ne!(first, second);

        pending.resolve(first.get(), response(first.get())).await;

        assert!(
            first_waiter.await.is_ok(),
            "the first request must be the one resolved"
        );
        assert_eq!(
            pending.len().await,
            1,
            "the second request is still in flight"
        );
        drop(second_waiter);
    }

    /// A number still in flight is never handed out again, even when the
    /// cursor comes back round to it.
    #[tokio::test]
    async fn an_in_flight_sequence_number_is_skipped_on_allocation() {
        let pending = Pending::new();
        let mut held = Vec::new();

        for _ in 0..8 {
            held.push(
                pending
                    .register(CommandId::EnquireLink, TIMEOUT)
                    .await
                    .expect("room"),
            );
        }

        // Rewind the cursor to the very first value, as a wrap would.
        pending.inner.lock().await.cursor = SequenceNumber::FIRST;

        let (fresh, _waiter) = pending
            .register(CommandId::EnquireLink, TIMEOUT)
            .await
            .expect("room");

        assert!(
            held.iter().all(|(taken, _)| *taken != fresh),
            "{fresh} was still in flight"
        );
    }

    /// CA-005-06 — ten thousand requests that are never answered leave the
    /// table exactly as they found it, entries *and* buckets.
    #[tokio::test(start_paused = true)]
    async fn ten_thousand_expired_requests_leave_nothing_behind() {
        let pending = Pending::new();
        let mut waiters = Vec::with_capacity(10_000);

        for _ in 0..10_000 {
            let (_, waiter) = pending
                .register(CommandId::SubmitSm, TIMEOUT)
                .await
                .expect("room");
            waiters.push(waiter);
        }

        assert_eq!(pending.len().await, 10_000);

        tokio::time::advance(TIMEOUT + Duration::from_secs(1)).await;
        assert_eq!(pending.expire(Instant::now()).await, 10_000);
        assert_eq!(pending.len().await, 0);
        assert_eq!(
            pending.inner.lock().await.waiting.capacity(),
            0,
            "the entries are gone but the buckets are not"
        );

        for waiter in waiters {
            assert!(matches!(
                waiter.await.expect("the sweep answers every waiter"),
                Err(SessionError::ResponseTimeout { .. })
            ));
        }
    }

    /// Cancellation: the caller drops its receiver, and the entry must still
    /// go — nobody is left to notice it otherwise.
    #[tokio::test(start_paused = true)]
    async fn a_cancelled_request_releases_its_entry_without_waiting_for_the_deadline() {
        let pending = Pending::new();

        let (_, waiter) = pending
            .register(CommandId::SubmitSm, TIMEOUT)
            .await
            .expect("room");
        drop(waiter);

        assert_eq!(
            pending.expire(Instant::now()).await,
            1,
            "an abandoned entry is swept even before its deadline"
        );
        assert_eq!(pending.len().await, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn an_entry_survives_a_sweep_that_happens_before_its_deadline() {
        let pending = Pending::new();

        let (_, _waiter) = pending
            .register(CommandId::SubmitSm, TIMEOUT)
            .await
            .expect("room");

        tokio::time::advance(TIMEOUT / 2).await;

        assert_eq!(pending.expire(Instant::now()).await, 0);
        assert_eq!(pending.len().await, 1);
    }

    #[tokio::test]
    async fn a_lost_link_fails_every_request_in_flight_at_once() {
        let pending = Pending::new();
        let mut waiters = Vec::new();

        for _ in 0..4 {
            let (_, waiter) = pending
                .register(CommandId::SubmitSm, TIMEOUT)
                .await
                .expect("room");
            waiters.push(waiter);
        }

        assert_eq!(pending.fail_all().await, 4);
        assert_eq!(pending.len().await, 0);

        for waiter in waiters {
            assert!(matches!(
                waiter.await.expect("answered"),
                Err(SessionError::Cancelled)
            ));
        }
    }

    /// A response of the wrong kind is not handed to the caller as if it were
    /// the right one.
    #[tokio::test]
    async fn a_response_of_the_wrong_operation_is_reported_as_such() {
        let pending = Pending::new();

        let (sequence, waiter) = pending
            .register(CommandId::SubmitSm, TIMEOUT)
            .await
            .expect("room");

        assert!(
            pending
                .resolve(sequence.get(), response(sequence.get()))
                .await
        );

        assert!(matches!(
            waiter.await.expect("answered"),
            Err(SessionError::UnexpectedResponse {
                expected: CommandId::SubmitSmResp,
                actual: CommandId::EnquireLinkResp,
            })
        ));
    }

    /// The pairing rule of the specification: bit 31 set on the request's
    /// identifier. Taken from `rusmpp` rather than restated, so a new
    /// operation cannot arrive with the pairing left behind.
    #[test]
    fn every_request_maps_to_its_own_response_operation() {
        for (request, expected) in [
            (CommandId::SubmitSm, CommandId::SubmitSmResp),
            (CommandId::EnquireLink, CommandId::EnquireLinkResp),
            (CommandId::Unbind, CommandId::UnbindResp),
            (CommandId::BindTransceiver, CommandId::BindTransceiverResp),
            (CommandId::BindReceiver, CommandId::BindReceiverResp),
            (CommandId::BindTransmitter, CommandId::BindTransmitterResp),
        ] {
            assert_eq!(request.matching_response(), expected);
        }
    }

    /// The scan starts at the cursor and walks forward: with the first four
    /// numbers held, the fifth is what comes back.
    #[tokio::test]
    async fn allocation_walks_past_every_number_still_in_flight() {
        let pending = Pending::new();
        let mut held = Vec::new();

        for _ in 0..4 {
            held.push(
                pending
                    .register(CommandId::SubmitSm, TIMEOUT)
                    .await
                    .expect("room"),
            );
        }

        pending.inner.lock().await.cursor = SequenceNumber::FIRST;

        let (sequence, _waiter) = pending
            .register(CommandId::SubmitSm, TIMEOUT)
            .await
            .expect("a run of len() + 1 always holds a free value");

        assert_eq!(sequence.get(), 5);
    }
}
