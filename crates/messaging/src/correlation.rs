//! Attaching a delivery receipt to the message it is about (L-008-02).
//!
//! # Correlation goes through the journal, never through memory
//!
//! A receipt arrives **after** the `submit_sm_resp`, sometimes hours later, and
//! step-008 §6 spells out the case that decides the design: after a restart of
//! the application. So the lookup is
//! [`MessageRepository::find_message_by_smsc_id`] against the index of
//! milestone 002, and there is deliberately no in-memory table of outstanding
//! identifiers — one would be a cache that is wrong exactly when it matters.
//!
//! # Two barriers milestone 006 left, which this module goes through and not
//! around
//!
//! Both exist to stop a receipt from crediting a message the recipient never
//! saw, and both are load-bearing here:
//!
//! * a **partially failed** multi-segment message keeps no `smsc_message_id`
//!   ([`crate::sender::Sender::aggregate`]), so a receipt for one of its
//!   accepted fragments finds no row and becomes an orphan. That is the
//!   intended outcome, not a miss;
//! * the state machine refuses `FAILED → DELIVERED`
//!   ([`MessageState::can_move_to`]), enforced inside the `UPDATE` statement.
//!   This module therefore emits the transition the receipt calls for and lets
//!   the journal refuse it; it never reads the current state and decides for
//!   itself, which would be a read-then-write race between two tasks.
//!
//! # What a segmented message does, and why
//!
//! A message split into three segments gets **three** identifiers from the
//! message centre and, if `registered_delivery` asked for receipts, **three**
//! receipts. The `messages` row carries one `smsc_message_id` — the first
//! segment's (milestone 006) — so:
//!
//! * the receipt for segment 1 correlates and drives the message's state;
//! * the receipts for segments 2..n correlate to nothing and are journalled as
//!   orphans, with the reason [`OrphanReason::UnknownIdentifier`].
//!
//! That is a deliberate limitation of this milestone rather than an oversight.
//! Correlating every segment needs a per-segment identifier table, which means
//! a schema change and a change to the send path — step-008 §2 scopes the
//! correlation to "by `smsc_message_id`, the index of step-002", i.e. the one
//! column. The consequence is honest and visible: nothing is lost, the extra
//! receipts are in the orphan journal with their `stat`, and an operator can
//! see that segments 2 and 3 were delivered even though the row's state came
//! from segment 1. The CHANGELOG records it as a known limitation.
//!
//! # Orphans are kept, never dropped
//!
//! CA-008-04. A message centre sends receipts for messages this client never
//! sent — a previous installation sharing the `system_id`, a message submitted
//! before the journal was created, a fragment of a partially failed message. A
//! receipt that correlates to nothing is written to its own journal with the
//! reason it did not correlate, and shown in the log screen. Dropping it would
//! make the one diagnostic an operator has for "my delivery rate is wrong"
//! disappear.

use core::future::Future;

use smpp_core::time::{Clock, Timestamp};
use smpp_core::types::{ClientMessageId, SessionId};

use crate::dlr::DeliveryReceipt;
use crate::message::{MessageState, MessageStateUpdate};
use crate::ports::{MessageRepository, MessageStoreError};

/// How hard to look for a message when the identifier does not match verbatim.
///
/// # Why this is a setting and not a fixed rule
///
/// step-008 §6: the first cause of uncorrelated receipts in production is a
/// message centre that answers `submit_sm` with an identifier in one base and
/// sends the receipt in another — hexadecimal in `submit_sm_resp`, decimal in
/// the body of the `deliver_sm`, or the same digits in a different case.
///
/// Trying the alternatives costs one indexed lookup each and fixes the
/// commonest failure, so it is the **default**. It is nevertheless not free of
/// risk: a message centre whose identifiers are opaque strings could, in
/// principle, mint both `1A` and `26`, and the relaxed search would credit the
/// wrong one. [`Self::Exact`] exists for that centre, and the choice belongs to
/// the session profile rather than to this module.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdMatching {
    /// The identifier as it arrived, and nothing else.
    Exact,
    /// Also try the case variants, the two bases, and the unpadded form.
    #[default]
    Relaxed,
}

impl IdMatching {
    /// The identifiers to look up, in decreasing order of confidence.
    ///
    /// The received form is always first, so a message centre that is
    /// consistent never pays for the alternatives: the first lookup hits and
    /// the rest are never built into a query. Duplicates are dropped **in
    /// order** — a purely numeric identifier would otherwise produce the same
    /// query three or four times.
    #[must_use]
    pub fn candidates(self, received: &str) -> Vec<String> {
        let received = received.trim();
        let mut candidates = vec![received.to_owned()];

        if self == Self::Exact {
            return candidates;
        }

        // Case. Hexadecimal is written both ways and neither is wrong.
        push_new(&mut candidates, received.to_ascii_uppercase());
        push_new(&mut candidates, received.to_ascii_lowercase());

        // Decimal seen as hexadecimal, and back. `u64` rather than `u128`: an
        // SMPP `message_id` is a 65-octet C-Octet String in principle, but the
        // centres that change base are the ones handing out a machine word,
        // and a value that does not fit one is not a number this conversion is
        // about.
        if let Ok(decimal) = received.parse::<u64>() {
            push_new(&mut candidates, format!("{decimal:x}"));
            push_new(&mut candidates, format!("{decimal:X}"));
        }

        if let Ok(hexadecimal) = u64::from_str_radix(received, 16) {
            push_new(&mut candidates, hexadecimal.to_string());
        }

        // Padding. `id:0000000042` against a stored `42`.
        let unpadded = received.trim_start_matches('0');

        if !unpadded.is_empty() {
            push_new(&mut candidates, unpadded.to_owned());
        }

        candidates
    }
}

/// Appends `value` unless the list already holds it, preserving order.
///
/// `Vec::dedup` would not do: it only removes **adjacent** duplicates, and the
/// candidates of a numeric identifier repeat all over the list. Sorting to make
/// them adjacent would destroy the order, which is the one property the caller
/// relies on — the received form has to be tried first.
fn push_new(candidates: &mut Vec<String>, value: String) {
    if !candidates.contains(&value) {
        candidates.push(value);
    }
}

/// Why a receipt correlated to no message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OrphanReason {
    /// The receipt carried no identifier at all — neither TLV nor `id:`.
    NoIdentifier,
    /// It carried one, and no message in the journal has it.
    UnknownIdentifier,
}

impl OrphanReason {
    /// The stored form, and the key the interface translates.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoIdentifier => "NO_IDENTIFIER",
            Self::UnknownIdentifier => "UNKNOWN_ID",
        }
    }

    /// Parses the stored form.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        [Self::NoIdentifier, Self::UnknownIdentifier]
            .into_iter()
            .find(|reason| reason.as_str() == raw)
    }
}

/// A delivery receipt that matched nothing, kept for the log screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanReceipt {
    /// Session it arrived on.
    pub session_id: Option<SessionId>,
    /// The identifier it carried, when it carried one.
    pub smsc_message_id: Option<String>,
    /// Why it did not correlate.
    pub reason: OrphanReason,
    /// `stat`, as the message centre wrote it.
    pub dlr_stat: Option<String>,
    /// `err`, as the message centre wrote it.
    pub dlr_err: Option<String>,
    /// `submit date`, when it could be read.
    pub submit_date: Option<Timestamp>,
    /// `done date`, when it could be read.
    pub done_date: Option<Timestamp>,
    /// The whole body, as it arrived.
    ///
    /// Content (CLAUDE.md §8): masked where it is rendered, not here — an
    /// orphan whose body has been redacted is an orphan nobody can diagnose.
    pub raw: String,
    /// When this application received it.
    pub received_at: Timestamp,
}

/// What became of one receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Correlated {
    /// It belongs to this message, and this is the transition it calls for.
    ///
    /// The transition is **not** applied here: applying receipts one at a time
    /// is what CA-008-10 rules out. [`ReceiptPipeline`] collects them and
    /// commits a batch in one transaction.
    Matched {
        /// The message the receipt is about.
        client_message_id: ClientMessageId,
        /// The transition to apply.
        update: Box<MessageStateUpdate>,
    },
    /// It belongs to no message this client knows about, and is kept for the
    /// log screen rather than dropped (CA-008-04).
    Orphan(Box<OrphanReceipt>),
    /// It says nothing about where the message stands.
    ///
    /// Two cases, and both must leave the row alone:
    ///
    /// * an **intermediate notification** — spec §7.8 has the centre send it
    ///   while it is still trying, so acting on it would close a message that
    ///   is still in flight;
    /// * a receipt whose `stat:` was **absent or unreadable** — there is
    ///   nothing to act on, and guessing would be inventing a delivery.
    NoTransition {
        /// The message it concerns, when it correlated to one.
        client_message_id: Option<ClientMessageId>,
    },
}

/// Correlates receipts against the journal.
///
/// Generic over the repository and the clock, so the whole of this module is
/// testable with an in-memory journal and a frozen instant (CLAUDE.md §7).
#[derive(Debug)]
pub struct Correlator<R, C> {
    repository: R,
    clock: C,
    matching: IdMatching,
}

impl<R, C> Correlator<R, C>
where
    R: MessageRepository,
    C: Clock,
{
    /// A correlator over a journal and a clock, matching identifiers loosely.
    #[must_use]
    pub const fn new(repository: R, clock: C) -> Self {
        Self {
            repository,
            clock,
            matching: IdMatching::Relaxed,
        }
    }

    /// The same correlator under another identifier-matching policy.
    #[must_use]
    pub const fn with_matching(mut self, matching: IdMatching) -> Self {
        self.matching = matching;
        self
    }

    /// The journal this correlator reads.
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    /// Finds the message a receipt is about.
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::Unavailable`] if the journal cannot be read. A
    /// receipt that correlates to nothing is **not** an error — it comes back
    /// as [`Correlated::Orphan`].
    #[tracing::instrument(skip_all, fields(session_id = ?session_id))]
    pub async fn correlate(
        &self,
        session_id: Option<SessionId>,
        receipt: &DeliveryReceipt,
    ) -> Result<Correlated, MessageStoreError> {
        let received_at = self.clock.now();

        let Some(identifier) = receipt.smsc_message_id.as_deref() else {
            tracing::warn!(
                stat = ?receipt.dlr_stat_text(),
                "delivery receipt carries no identifier; journalled as an orphan"
            );

            return Ok(Correlated::Orphan(Box::new(orphan(
                session_id,
                receipt,
                OrphanReason::NoIdentifier,
                received_at,
            ))));
        };

        let mut matched = None;

        for candidate in self.matching.candidates(identifier) {
            if let Some(message) = self.repository.find_message_by_smsc_id(&candidate).await? {
                if candidate != identifier {
                    // Worth a line: it is the signal that this session's
                    // message centre changes the form of its identifiers, and
                    // the reason `IdMatching::Exact` would lose receipts here.
                    tracing::info!(
                        received = identifier,
                        stored = candidate,
                        "delivery receipt correlated under a normalised identifier"
                    );
                }

                matched = Some(message);
                break;
            }
        }

        let Some(message) = matched else {
            tracing::warn!(
                smsc_message_id = identifier,
                "delivery receipt matches no message; journalled as an orphan"
            );

            return Ok(Correlated::Orphan(Box::new(orphan(
                session_id,
                receipt,
                OrphanReason::UnknownIdentifier,
                received_at,
            ))));
        };

        let client_message_id = message.client_message_id;

        let Some(state) = receipt.message_state() else {
            return Ok(Correlated::NoTransition {
                client_message_id: Some(client_message_id),
            });
        };

        Ok(Correlated::Matched {
            client_message_id,
            update: Box::new(transition(client_message_id, state, receipt, received_at)),
        })
    }
}

/// The transition a receipt calls for.
///
/// `dlr_at` is the instant **this application** received the receipt, not the
/// `done date` the message centre wrote. The two are different clocks: the
/// centre's is unverifiable, frequently in local time, and occasionally absent.
/// `done date` is kept in the orphan journal and in the receipt itself for an
/// operator to read; `dlr_at` is the one column an ordering or a retention rule
/// may rely on, so it comes from the injected clock.
fn transition(
    client_message_id: ClientMessageId,
    state: MessageState,
    receipt: &DeliveryReceipt,
    received_at: Timestamp,
) -> MessageStateUpdate {
    let update = MessageStateUpdate::new(client_message_id, state).receipt_at(received_at);

    match receipt.status.as_ref() {
        Some(status) => update.with_delivery_receipt(status.as_str(), receipt.error_code.clone()),
        None => update,
    }
}

/// The orphan record for a receipt that matched nothing.
fn orphan(
    session_id: Option<SessionId>,
    receipt: &DeliveryReceipt,
    reason: OrphanReason,
    received_at: Timestamp,
) -> OrphanReceipt {
    OrphanReceipt {
        session_id,
        smsc_message_id: receipt.smsc_message_id.clone(),
        reason,
        dlr_stat: receipt.dlr_stat_text(),
        dlr_err: receipt.error_code.clone(),
        submit_date: receipt.submit_date,
        done_date: receipt.done_date,
        raw: receipt.raw.clone(),
        received_at,
    }
}

/// Where orphaned receipts are kept.
///
/// A port, declared by the crate that consumes it (CLAUDE.md §3) and
/// implemented by `persistence`. Narrow on purpose: the read-and-paginate half
/// belongs to the log screen and lives with the rest of the journal reading.
pub trait OrphanReceiptStore: Send + Sync {
    /// Appends a batch of orphans in **one** transaction.
    ///
    /// Batched for the same reason the transitions are (CA-008-10): a message
    /// centre replaying a backlog sends orphans as fast as it sends receipts.
    ///
    /// # Errors
    ///
    /// [`MessageStoreError::Unavailable`] if the write fails.
    fn insert_orphans(
        &self,
        orphans: &[OrphanReceipt],
    ) -> impl Future<Output = Result<u64, MessageStoreError>> + Send;
}

/// One receipt, and the session it arrived on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingReceipt {
    /// Which session read it off the socket.
    pub session_id: Option<SessionId>,
    /// What it said.
    pub receipt: DeliveryReceipt,
}

/// When a batch of receipts is committed.
///
/// # Why both bounds, and why neither is optional
///
/// CA-008-10 asks for state changes to be written **in batches**: at a thousand
/// messages a second, the number of transactions per second must stay far below
/// the number of messages. A size bound alone gives that — and stalls the last
/// receipt of a quiet minute until the batch fills, which is CA-008-01's
/// "visible in the interface in under a second" broken. A delay bound alone
/// gives the latency and lets a burst build an unbounded batch.
///
/// So: commit when either is reached, and the delay is measured from the
/// **first** receipt of the batch, not from the last. Measuring from the last
/// would let a steady stream push the commit back for ever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchPolicy {
    /// Commit once this many receipts are waiting.
    pub max_receipts: usize,
    /// Commit this long after the first receipt of the batch, whatever its
    /// size.
    pub max_delay: core::time::Duration,
}

impl Default for BatchPolicy {
    /// Two hundred receipts, or a quarter of a second.
    ///
    /// At 1 000 receipts a second the size bound fires first and produces five
    /// commits a second against a thousand messages — the two orders of
    /// magnitude CA-008-10 asks for. At one receipt a minute the delay bound
    /// fires and the interface sees it 250 ms later, well inside CA-008-01.
    ///
    /// 250 ms is also `METRICS_TICK_INTERVAL`, and that is not a coincidence:
    /// it is the interval below which a repaint buys nothing a human can see.
    fn default() -> Self {
        Self {
            max_receipts: 200,
            max_delay: core::time::Duration::from_millis(250),
        }
    }
}

/// One message's new standing, as the interface is told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptNote {
    /// Which message moved.
    pub client_message_id: ClientMessageId,
    /// Where it moved to.
    pub state: MessageState,
    /// The `stat` code the message centre sent.
    pub dlr_stat: Option<String>,
}

/// Watches batches of receipts land.
///
/// The unit is a **batch**, not a receipt, and that is the whole point:
/// CA-008-08 requires `message:update` to carry aggregated increments rather
/// than one event per message. A trait that announced one message at a time
/// would leave the aggregation to the boundary, where a future call site would
/// eventually forget it.
///
/// Deliberately not `async`: it is called between two `.await`s of the commit
/// path, and an implementation doing I/O here would pace the receipt pipeline
/// from the interface.
pub trait ReceiptObserver: Send + Sync {
    /// A batch of transitions has been committed.
    ///
    /// Never called with an empty slice.
    fn receipts_applied(&self, notes: &[ReceiptNote]);
}

/// The observer that watches nothing, for a caller with no interface.
impl ReceiptObserver for () {
    fn receipts_applied(&self, _notes: &[ReceiptNote]) {}
}

/// What one commit did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Transitions written.
    pub applied: u64,
    /// Orphans journalled.
    pub orphaned: u64,
    /// Receipts that correlated but called for no transition.
    pub noted: u64,
    /// Receipts the journal could not be read for.
    pub failed: u64,
}

/// Correlates and commits receipts in batches (CA-008-10).
///
/// Owns no task and spawns none: [`Self::run`] is a future the caller drives,
/// which is what lets a test run the whole pipeline under
/// `tokio::time::pause()` with no real clock in sight.
#[derive(Debug)]
pub struct ReceiptPipeline<R, C, S> {
    correlator: Correlator<R, C>,
    orphans: S,
    policy: BatchPolicy,
}

impl<R, C, S> ReceiptPipeline<R, C, S>
where
    R: MessageRepository,
    C: Clock,
    S: OrphanReceiptStore,
{
    /// A pipeline over a correlator and an orphan journal.
    #[must_use]
    pub fn new(correlator: Correlator<R, C>, orphans: S) -> Self {
        Self {
            correlator,
            orphans,
            policy: BatchPolicy::default(),
        }
    }

    /// The same pipeline under another batching policy.
    #[must_use]
    pub const fn with_policy(mut self, policy: BatchPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The correlator this pipeline drives.
    pub const fn correlator(&self) -> &Correlator<R, C> {
        &self.correlator
    }

    /// Drains `receipts` until the channel closes, committing in batches.
    ///
    /// Returns when every sender has been dropped **and** the last batch has
    /// been committed: a shutdown does not lose the receipts already read off
    /// the socket.
    pub async fn run<O>(
        &self,
        mut receipts: tokio::sync::mpsc::Receiver<IncomingReceipt>,
        observer: &O,
    ) where
        O: ReceiptObserver + ?Sized,
    {
        let mut pending: Vec<IncomingReceipt> = Vec::with_capacity(self.policy.max_receipts);

        loop {
            // Nothing in hand: park until something arrives. No deadline is
            // running, so an idle pipeline costs no timer.
            match receipts.recv().await {
                Some(first) => pending.push(first),
                None => return,
            }

            // The deadline starts HERE — at the first receipt of the batch,
            // not at the last. Restarting it on each arrival would let a steady
            // stream postpone the commit indefinitely.
            let deadline = tokio::time::sleep(self.policy.max_delay);
            tokio::pin!(deadline);

            let mut closed = false;

            while pending.len() < self.policy.max_receipts {
                tokio::select! {
                    () = &mut deadline => break,
                    received = receipts.recv() => match received {
                        Some(next) => pending.push(next),
                        None => {
                            closed = true;
                            break;
                        }
                    },
                }
            }

            self.commit(&mut pending, observer).await;

            if closed {
                return;
            }
        }
    }

    /// Correlates a batch and commits it, emptying `pending`.
    ///
    /// Public so a caller with its own scheduling — a test, an import — can use
    /// the batching without the channel.
    pub async fn commit<O>(&self, pending: &mut Vec<IncomingReceipt>, observer: &O) -> BatchOutcome
    where
        O: ReceiptObserver + ?Sized,
    {
        let mut outcome = BatchOutcome::default();
        let mut updates = Vec::with_capacity(pending.len());
        let mut notes = Vec::with_capacity(pending.len());
        let mut orphans = Vec::new();

        for incoming in pending.drain(..) {
            match self
                .correlator
                .correlate(incoming.session_id, &incoming.receipt)
                .await
            {
                Ok(Correlated::Matched {
                    client_message_id,
                    update,
                }) => {
                    notes.push(ReceiptNote {
                        client_message_id,
                        state: update.state,
                        dlr_stat: update.dlr_stat.clone(),
                    });
                    updates.push(*update);
                }
                Ok(Correlated::Orphan(orphan)) => orphans.push(*orphan),
                Ok(Correlated::NoTransition { .. }) => outcome.noted += 1,
                Err(error) => {
                    // The journal could not be READ. The receipt is lost — it
                    // is not coming again — so this is an error line rather
                    // than a warning, and it is counted so the caller can see
                    // it happened at all.
                    tracing::error!(
                        error = %error,
                        "the message journal could not be read for a delivery receipt"
                    );

                    outcome.failed += 1;
                }
            }
        }

        outcome.applied = self.write_transitions(&updates).await;
        outcome.orphaned = self.write_orphans(&orphans).await;

        if !notes.is_empty() {
            observer.receipts_applied(&notes);
        }

        outcome
    }

    /// Commits the transitions of one batch in **one** transaction.
    ///
    /// # The fallback, and why it is not a retry loop
    ///
    /// `update_states` is all-or-nothing: one missing message rolls the whole
    /// batch back. That is the right guarantee for the send path, where a batch
    /// is one message's two transitions — and the wrong failure mode here,
    /// where a batch is two hundred unrelated receipts and one deleted message
    /// would discard a hundred and ninety-nine deliveries.
    ///
    /// So a failed batch is replayed **once**, one transition at a time, and
    /// the ones that still fail are logged and dropped. The cost is a hundred
    /// transactions on a path that normally does one, on a failure that
    /// normally does not happen.
    async fn write_transitions(&self, updates: &[MessageStateUpdate]) -> u64 {
        if updates.is_empty() {
            return 0;
        }

        match self.correlator.repository().update_states(updates).await {
            Ok(applied) => applied,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    batch = updates.len(),
                    "a batch of delivery receipts was refused; replaying it one by one"
                );

                let mut applied = 0;

                for update in updates {
                    match self.correlator.repository().update_state(update).await {
                        Ok(()) => applied += 1,
                        Err(error) => tracing::error!(
                            error = %error,
                            client_message_id = %update.client_message_id,
                            "a delivery receipt could not be journalled"
                        ),
                    }
                }

                applied
            }
        }
    }

    /// Journals the orphans of one batch in **one** transaction.
    ///
    /// A failure here is logged and nothing else: an orphan is a diagnostic,
    /// and losing one must not cost the deliveries committed alongside it.
    async fn write_orphans(&self, orphans: &[OrphanReceipt]) -> u64 {
        if orphans.is_empty() {
            return 0;
        }

        match self.orphans.insert_orphans(orphans).await {
            Ok(written) => written,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    batch = orphans.len(),
                    "orphaned delivery receipts could not be journalled"
                );

                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]
    // `std::sync::Mutex` is banned because it must never be held across an
    // `.await`. None of the guards below is: every critical section is a `Vec`
    // push or a `HashMap` lookup, and the doubles are deliberately written so
    // no lock is alive when a future yields.
    //
    // The alternative is not `tokio::sync::Mutex`: `ReceiptObserver` is a
    // **synchronous** trait — that is its contract, so an implementation cannot
    // pace the receipt pipeline — and an async lock has nothing to offer a
    // callback that cannot await.
    #![allow(clippy::disallowed_types)]

    use std::collections::HashMap;
    use std::sync::Mutex;

    use smpp_core::time::{Clock, Timestamp};
    use smpp_core::types::{ClientMessageId, Msisdn};

    use super::{
        BatchPolicy, Correlated, Correlator, IdMatching, IncomingReceipt, OrphanReason,
        OrphanReceipt, OrphanReceiptStore, ReceiptNote, ReceiptObserver, ReceiptPipeline,
    };
    use crate::dlr::{parse_receipt_body, DeliveryStatus};
    use crate::message::{Message, MessageState, MessageStateUpdate};
    use crate::ports::{MessageRepository, MessageStoreError};

    /// A clock frozen at one instant (CLAUDE.md §7).
    struct FrozenClock(Timestamp);

    impl Clock for FrozenClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    fn frozen() -> FrozenClock {
        FrozenClock(Timestamp::parse("2026-07-26T12:00:00Z").expect("valid instant"))
    }

    /// An in-memory journal, counting its lookups.
    ///
    /// `std::sync::Mutex` is banned in production code and fine here: no guard
    /// crosses an `.await` — every critical section below is a map operation.
    #[derive(Default)]
    struct FakeJournal {
        by_smsc_id: Mutex<HashMap<String, Message>>,
        lookups: Mutex<Vec<String>>,
        /// One entry per **transaction**: the number CA-008-10 is stated in.
        transactions: Mutex<Vec<usize>>,
        /// Makes the next `update_states` fail, to exercise the fallback.
        refuse_batches: bool,
    }

    impl FakeJournal {
        fn holding(pairs: &[(&str, ClientMessageId)]) -> Self {
            let journal = Self::default();

            for (identifier, client_message_id) in pairs {
                let mut message = a_message();
                message.client_message_id = *client_message_id;
                message.smsc_message_id = Some((*identifier).to_owned());

                journal
                    .by_smsc_id
                    .lock()
                    .expect("uncontended")
                    .insert((*identifier).to_owned(), message);
            }

            journal
        }

        fn lookups(&self) -> Vec<String> {
            self.lookups.lock().expect("uncontended").clone()
        }

        /// How many transactions the journal was asked to commit.
        fn transactions(&self) -> usize {
            self.transactions.lock().expect("uncontended").len()
        }
    }

    /// An orphan journal that remembers what it was handed.
    #[derive(Default)]
    struct FakeOrphans {
        written: Mutex<Vec<OrphanReceipt>>,
        transactions: Mutex<usize>,
    }

    impl OrphanReceiptStore for FakeOrphans {
        async fn insert_orphans(
            &self,
            orphans: &[OrphanReceipt],
        ) -> Result<u64, MessageStoreError> {
            *self.transactions.lock().expect("uncontended") += 1;
            self.written
                .lock()
                .expect("uncontended")
                .extend_from_slice(orphans);

            Ok(orphans.len().try_into().unwrap_or(u64::MAX))
        }
    }

    /// Records the batches the observer was told about, and **when**.
    ///
    /// The instant matters: CA-008-01 is a latency, and measuring it at the
    /// point `run` returns would measure when the channel closed instead.
    #[derive(Default)]
    struct RecordingObserver {
        batches: Mutex<Vec<(tokio::time::Instant, Vec<ReceiptNote>)>>,
    }

    impl RecordingObserver {
        fn batches(&self) -> Vec<Vec<ReceiptNote>> {
            self.batches
                .lock()
                .expect("uncontended")
                .iter()
                .map(|(_, notes)| notes.clone())
                .collect()
        }

        /// When the first batch was announced.
        fn first_announced_at(&self) -> Option<tokio::time::Instant> {
            self.batches
                .lock()
                .expect("uncontended")
                .first()
                .map(|(at, _)| *at)
        }
    }

    impl ReceiptObserver for RecordingObserver {
        fn receipts_applied(&self, notes: &[ReceiptNote]) {
            assert!(!notes.is_empty(), "an empty batch must not be announced");

            self.batches
                .lock()
                .expect("uncontended")
                .push((tokio::time::Instant::now(), notes.to_vec()));
        }
    }

    impl MessageRepository for FakeJournal {
        async fn insert_message(&self, _message: &Message) -> Result<(), MessageStoreError> {
            Ok(())
        }

        async fn insert_messages(&self, _messages: &[Message]) -> Result<u64, MessageStoreError> {
            Ok(0)
        }

        async fn find_message(
            &self,
            _client_message_id: ClientMessageId,
        ) -> Result<Option<Message>, MessageStoreError> {
            Ok(None)
        }

        async fn find_message_by_smsc_id(
            &self,
            smsc_message_id: &str,
        ) -> Result<Option<Message>, MessageStoreError> {
            self.lookups
                .lock()
                .expect("uncontended")
                .push(smsc_message_id.to_owned());

            Ok(self
                .by_smsc_id
                .lock()
                .expect("uncontended")
                .get(smsc_message_id)
                .cloned())
        }

        async fn update_state(
            &self,
            _update: &MessageStateUpdate,
        ) -> Result<(), MessageStoreError> {
            self.transactions.lock().expect("uncontended").push(1);

            Ok(())
        }

        async fn update_states(
            &self,
            updates: &[MessageStateUpdate],
        ) -> Result<u64, MessageStoreError> {
            if self.refuse_batches {
                return Err(MessageStoreError::NotFound);
            }

            self.transactions
                .lock()
                .expect("uncontended")
                .push(updates.len());

            Ok(updates.len().try_into().unwrap_or(u64::MAX))
        }
    }

    fn a_message() -> Message {
        Message {
            client_message_id: ClientMessageId::new(),
            campaign_id: None,
            session_id: None,
            smsc_message_id: None,
            source_addr: None,
            source_ton: None,
            source_npi: None,
            dest_addr: Some(Msisdn::parse("+2250102030405").expect("valid number")),
            dest_ton: None,
            dest_npi: None,
            data_coding: None,
            segments: 1,
            text: None,
            state: MessageState::Accepted,
            command_status: None,
            dlr_stat: None,
            dlr_err: None,
            attempts: 1,
            created_at: Timestamp::parse("2026-07-26T10:00:00Z").expect("valid instant"),
            sent_at: None,
            resp_at: None,
            dlr_at: None,
        }
    }

    // --- Correlation --------------------------------------------------------

    /// CA-008-01 — a `DELIVRD` receipt produces the transition to `DELIVERED`,
    /// carrying `dlr_at`, `dlr_stat` and `dlr_err`.
    #[tokio::test]
    async fn ca_008_01_a_delivered_receipt_moves_the_message_to_delivered() {
        let client_message_id = ClientMessageId::new();
        let journal = FakeJournal::holding(&[("SMSC-1", client_message_id)]);
        let correlator = Correlator::new(journal, frozen());
        let receipt = parse_receipt_body("id:SMSC-1 stat:DELIVRD err:000");

        let outcome = correlator
            .correlate(None, &receipt)
            .await
            .expect("the journal answers");

        let Correlated::Matched { update, .. } = outcome else {
            panic!("expected a match, got {outcome:?}");
        };

        assert_eq!(update.client_message_id, client_message_id);
        assert_eq!(update.state, MessageState::Delivered);
        assert_eq!(update.dlr_stat.as_deref(), Some("DELIVRD"));
        assert_eq!(update.dlr_err.as_deref(), Some("000"));
        assert_eq!(update.dlr_at, Some(frozen().now()));
    }

    /// `dlr_at` is **our** clock, not the centre's `done date`. The two are
    /// different clocks and only one of them is verifiable.
    #[tokio::test]
    async fn the_receipt_instant_is_the_local_clock_not_the_done_date() {
        let journal = FakeJournal::holding(&[("SMSC-1", ClientMessageId::new())]);
        let correlator = Correlator::new(journal, frozen());
        let receipt = parse_receipt_body("id:SMSC-1 done date:1501011200 stat:DELIVRD");

        let Correlated::Matched { update, .. } =
            correlator.correlate(None, &receipt).await.expect("answers")
        else {
            panic!("expected a match");
        };

        assert_eq!(update.dlr_at, Some(frozen().now()));
        assert_ne!(
            update.dlr_at.map(|at| at.to_storage()),
            Some(String::from("2015-01-01T12:00:00Z"))
        );
    }

    /// The transition emitted is the one the receipt calls for, **whatever the
    /// message's current state**. This module must not read the state and
    /// decide for itself: the `UPDATE` statement carries the machine, and doing
    /// it here would be a read-then-write race between two tasks.
    #[tokio::test]
    async fn a_receipt_for_a_failed_message_still_emits_its_transition() {
        let client_message_id = ClientMessageId::new();
        let journal = FakeJournal::default();
        let mut failed = a_message();
        failed.client_message_id = client_message_id;
        failed.state = MessageState::Failed;
        failed.smsc_message_id = Some(String::from("SMSC-9"));
        journal
            .by_smsc_id
            .lock()
            .expect("uncontended")
            .insert(String::from("SMSC-9"), failed);

        let correlator = Correlator::new(journal, frozen());
        let receipt = parse_receipt_body("id:SMSC-9 stat:DELIVRD");

        let Correlated::Matched { update, .. } =
            correlator.correlate(None, &receipt).await.expect("answers")
        else {
            panic!("expected a match");
        };

        // Emitted as DELIVERED; the journal is what refuses it. Deciding here
        // would duplicate the machine in a second place.
        assert_eq!(update.state, MessageState::Delivered);
        assert!(!MessageState::Failed.can_move_to(MessageState::Delivered));
    }

    // --- Orphans (CA-008-04) ------------------------------------------------

    #[tokio::test]
    async fn ca_008_04_an_unknown_identifier_becomes_a_journalled_orphan() {
        let correlator = Correlator::new(FakeJournal::default(), frozen());
        let receipt = parse_receipt_body("id:NEVER-SENT stat:DELIVRD err:000 text:hi");

        let Correlated::Orphan(orphan) =
            correlator.correlate(None, &receipt).await.expect("answers")
        else {
            panic!("expected an orphan");
        };

        assert_eq!(orphan.reason, OrphanReason::UnknownIdentifier);
        assert_eq!(orphan.smsc_message_id.as_deref(), Some("NEVER-SENT"));
        assert_eq!(orphan.dlr_stat.as_deref(), Some("DELIVRD"));
        assert_eq!(orphan.received_at, frozen().now());
        assert!(orphan.raw.contains("NEVER-SENT"), "the body is kept whole");
    }

    /// CA-008-03 — an unreadable body carries no identifier, and must still be
    /// kept rather than silently dropped.
    #[tokio::test]
    async fn ca_008_03_a_receipt_with_no_identifier_becomes_an_orphan() {
        let correlator = Correlator::new(FakeJournal::default(), frozen());
        let receipt = parse_receipt_body("completely unreadable");

        let Correlated::Orphan(orphan) =
            correlator.correlate(None, &receipt).await.expect("answers")
        else {
            panic!("expected an orphan");
        };

        assert_eq!(orphan.reason, OrphanReason::NoIdentifier);
        assert_eq!(orphan.smsc_message_id, None);
        assert_eq!(orphan.raw, "completely unreadable");
    }

    /// The milestone-006 barrier, exercised end to end: a partially failed
    /// message carries no `smsc_message_id`, so the receipt for its accepted
    /// fragment finds nothing and is kept as an orphan rather than crediting a
    /// message the recipient never saw.
    #[tokio::test]
    async fn a_receipt_for_a_partially_failed_message_is_an_orphan_not_a_delivery() {
        let journal = FakeJournal::default();
        // What `Sender::aggregate` writes for a partial failure: FAILED, and
        // NO identifier — which is exactly why the lookup below finds nothing.
        let mut partial = a_message();
        partial.state = MessageState::Failed;
        partial.segments = 3;
        partial.smsc_message_id = None;
        journal
            .by_smsc_id
            .lock()
            .expect("uncontended")
            .insert(String::from("unused"), partial);

        let correlator = Correlator::new(journal, frozen());
        let receipt = parse_receipt_body("id:SEGMENT-1 stat:DELIVRD");

        assert!(matches!(
            correlator.correlate(None, &receipt).await.expect("answers"),
            Correlated::Orphan(_)
        ));
    }

    // --- Intermediate notifications -----------------------------------------

    #[tokio::test]
    async fn an_intermediate_notification_correlates_without_a_transition() {
        let client_message_id = ClientMessageId::new();
        let journal = FakeJournal::holding(&[("SMSC-1", client_message_id)]);
        let correlator = Correlator::new(journal, frozen());
        let mut receipt = parse_receipt_body("id:SMSC-1 stat:DELIVRD");
        receipt.intermediate = true;

        assert_eq!(
            correlator.correlate(None, &receipt).await.expect("answers"),
            Correlated::NoTransition {
                client_message_id: Some(client_message_id)
            }
        );
    }

    // --- Identifier matching (step-008 §6) ----------------------------------

    /// The received form is tried first and, when it hits, nothing else is.
    #[tokio::test]
    async fn a_consistent_message_centre_costs_exactly_one_lookup() {
        let journal = FakeJournal::holding(&[("SMSC-1", ClientMessageId::new())]);
        let correlator = Correlator::new(journal, frozen());
        let receipt = parse_receipt_body("id:SMSC-1 stat:DELIVRD");

        correlator.correlate(None, &receipt).await.expect("answers");

        assert_eq!(correlator.repository().lookups(), vec!["SMSC-1"]);
    }

    /// step-008 §6 — the identifier came back hexadecimal and the receipt
    /// quotes it in decimal. The commonest cause of uncorrelated receipts.
    #[tokio::test]
    async fn an_identifier_sent_in_another_base_still_correlates() {
        let client_message_id = ClientMessageId::new();
        // 0x2a == 42.
        let journal = FakeJournal::holding(&[("2a", client_message_id)]);
        let correlator = Correlator::new(journal, frozen());
        let receipt = parse_receipt_body("id:42 stat:DELIVRD");

        let Correlated::Matched {
            client_message_id: matched,
            ..
        } = correlator.correlate(None, &receipt).await.expect("answers")
        else {
            panic!("expected a match");
        };

        assert_eq!(matched, client_message_id);
    }

    #[tokio::test]
    async fn an_identifier_in_another_case_still_correlates() {
        let journal = FakeJournal::holding(&[("ABCDEF", ClientMessageId::new())]);
        let correlator = Correlator::new(journal, frozen());

        assert!(matches!(
            correlator
                .correlate(None, &parse_receipt_body("id:abcdef stat:DELIVRD"))
                .await
                .expect("answers"),
            Correlated::Matched { .. }
        ));
    }

    #[tokio::test]
    async fn a_zero_padded_identifier_still_correlates() {
        let journal = FakeJournal::holding(&[("42", ClientMessageId::new())]);
        let correlator = Correlator::new(journal, frozen());

        assert!(matches!(
            correlator
                .correlate(None, &parse_receipt_body("id:0000000042 stat:DELIVRD"))
                .await
                .expect("answers"),
            Correlated::Matched { .. }
        ));
    }

    /// The escape hatch: a centre with opaque identifiers must be able to
    /// refuse the normalisation, and then a differently spelled identifier is
    /// an orphan rather than a wrong match.
    #[tokio::test]
    async fn exact_matching_refuses_a_normalised_identifier() {
        let journal = FakeJournal::holding(&[("2a", ClientMessageId::new())]);
        let correlator = Correlator::new(journal, frozen()).with_matching(IdMatching::Exact);

        assert!(matches!(
            correlator
                .correlate(None, &parse_receipt_body("id:42 stat:DELIVRD"))
                .await
                .expect("answers"),
            Correlated::Orphan(_)
        ));
        assert_eq!(correlator.repository().lookups(), vec!["42"]);
    }

    #[test]
    fn the_received_form_is_always_the_first_candidate() {
        for received in ["SMSC-1", "42", "2a", "0042", ""] {
            let candidates = IdMatching::Relaxed.candidates(received);

            assert_eq!(candidates.first().map(String::as_str), Some(received));
        }
    }

    #[test]
    fn the_candidate_list_holds_no_duplicates() {
        let candidates = IdMatching::Relaxed.candidates("42");
        let mut sorted = candidates.clone();
        sorted.sort_unstable();
        sorted.dedup();

        assert_eq!(sorted.len(), candidates.len(), "{candidates:?}");
    }

    #[test]
    fn the_orphan_reasons_round_trip_through_their_stored_form() {
        for reason in [OrphanReason::NoIdentifier, OrphanReason::UnknownIdentifier] {
            assert_eq!(OrphanReason::parse(reason.as_str()), Some(reason));
        }

        assert_eq!(OrphanReason::parse("SOMETHING_ELSE"), None);
    }

    /// A receipt with no readable `stat` correlates to its message and reports
    /// nothing about the state: the message stays where it was.
    #[tokio::test]
    async fn a_receipt_with_no_status_reports_no_transition() {
        let journal = FakeJournal::holding(&[("SMSC-1", ClientMessageId::new())]);
        let correlator = Correlator::new(journal, frozen());
        let receipt = parse_receipt_body("id:SMSC-1");

        assert_eq!(receipt.status, None);
        assert!(matches!(
            correlator.correlate(None, &receipt).await.expect("answers"),
            Correlated::NoTransition { .. }
        ));
    }

    #[test]
    fn the_status_text_is_the_code_the_centre_wrote() {
        let receipt = parse_receipt_body("id:1 stat:buffered");

        assert_eq!(receipt.dlr_stat_text().as_deref(), Some("BUFFERED"));
        assert_eq!(
            receipt.status,
            Some(DeliveryStatus::Other(String::from("BUFFERED")))
        );
    }

    // --- The batching pipeline (CA-008-08, CA-008-10) -----------------------

    /// A journal holding `count` messages, and the receipts that name them.
    fn a_backlog(count: usize) -> (FakeJournal, Vec<IncomingReceipt>) {
        let pairs: Vec<(String, ClientMessageId)> = (0..count)
            .map(|index| (format!("SMSC-{index}"), ClientMessageId::new()))
            .collect();
        let journal = FakeJournal::holding(
            &pairs
                .iter()
                .map(|(identifier, id)| (identifier.as_str(), *id))
                .collect::<Vec<_>>(),
        );

        let receipts = pairs
            .iter()
            .map(|(identifier, _)| IncomingReceipt {
                session_id: None,
                receipt: parse_receipt_body(&format!("id:{identifier} stat:DELIVRD err:000")),
            })
            .collect();

        (journal, receipts)
    }

    /// **CA-008-10**, measured rather than asserted by construction.
    ///
    /// A thousand receipts arriving at a thousand a second must not produce a
    /// thousand transactions. The size bound is 200, so five commits is the
    /// arithmetic — and what the test actually pins is "far fewer than the
    /// number of messages", which is the criterion's own wording.
    ///
    /// The clock is paused: the delay bound would otherwise fire on the real
    /// clock and make the count depend on how fast the machine is.
    #[tokio::test(start_paused = true)]
    async fn ca_008_10_a_thousand_receipts_commit_in_a_handful_of_transactions() {
        const RECEIPTS: usize = 1_000;
        const BATCH: usize = 200;

        let (journal, receipts) = a_backlog(RECEIPTS);
        let pipeline =
            ReceiptPipeline::new(Correlator::new(journal, frozen()), FakeOrphans::default())
                .with_policy(BatchPolicy {
                    max_receipts: BATCH,
                    max_delay: core::time::Duration::from_millis(250),
                });
        let observer = RecordingObserver::default();

        let (sender, receiver) = tokio::sync::mpsc::channel(RECEIPTS);
        for receipt in receipts {
            sender.send(receipt).await.expect("room");
        }
        drop(sender);

        pipeline.run(receiver, &observer).await;

        let transactions = pipeline.correlator().repository().transactions();

        assert_eq!(
            transactions,
            RECEIPTS / BATCH,
            "a thousand receipts in batches of {BATCH} is {} commits",
            RECEIPTS / BATCH
        );
        assert!(
            transactions * 10 < RECEIPTS,
            "{transactions} transactions for {RECEIPTS} receipts is not 'far below'"
        );

        // And the instrument works: without it the assertion above would pass
        // on a journal that was never called at all.
        let applied: usize = observer.batches().iter().map(Vec::len).sum();
        assert_eq!(applied, RECEIPTS, "every receipt must have been applied");
    }

    /// **CA-008-08** — the interface is told about a *batch*, not about each
    /// message. One event per batch is what stops the bridge from carrying a
    /// thousand payloads a second.
    #[tokio::test(start_paused = true)]
    async fn ca_008_08_the_interface_is_notified_once_per_batch() {
        let (journal, receipts) = a_backlog(300);
        let pipeline =
            ReceiptPipeline::new(Correlator::new(journal, frozen()), FakeOrphans::default())
                .with_policy(BatchPolicy {
                    max_receipts: 100,
                    max_delay: core::time::Duration::from_millis(250),
                });
        let observer = RecordingObserver::default();

        let (sender, receiver) = tokio::sync::mpsc::channel(300);
        for receipt in receipts {
            sender.send(receipt).await.expect("room");
        }
        drop(sender);

        pipeline.run(receiver, &observer).await;

        let batches = observer.batches();

        assert_eq!(
            batches.len(),
            3,
            "three hundred receipts, batches of a hundred"
        );
        assert!(
            batches.iter().all(|batch| batch.len() == 100),
            "each announcement carries its whole batch: {:?}",
            batches.iter().map(Vec::len).collect::<Vec<_>>()
        );
        assert!(batches
            .iter()
            .flatten()
            .all(|note| note.state == MessageState::Delivered));
    }

    /// **CA-008-01** — a lone receipt on a quiet link must not wait for a batch
    /// to fill. The delay bound is what makes it visible in under a second.
    ///
    /// Under `start_paused` the elapsed time is the virtual clock's, so this
    /// measures the policy rather than the machine.
    #[tokio::test(start_paused = true)]
    async fn ca_008_01_a_lone_receipt_is_committed_within_the_delay_bound() {
        let (journal, mut receipts) = a_backlog(1);
        let pipeline =
            ReceiptPipeline::new(Correlator::new(journal, frozen()), FakeOrphans::default())
                .with_policy(BatchPolicy {
                    max_receipts: 200,
                    max_delay: core::time::Duration::from_millis(250),
                });
        let observer = RecordingObserver::default();

        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        sender
            .send(receipts.pop().expect("one receipt"))
            .await
            .expect("room");

        let started = tokio::time::Instant::now();
        let running = tokio::spawn(async move {
            // The channel stays open: this is the "quiet link" case, where the
            // batch is never going to fill.
            tokio::time::sleep(core::time::Duration::from_secs(2)).await;
            drop(sender);
        });

        pipeline.run(receiver, &observer).await;
        running.await.expect("the sender task ends");

        // Measured at the ANNOUNCEMENT, not at the return of `run`: the loop
        // only returns when the channel closes, two seconds from now, and
        // timing that would time the sender rather than the policy.
        let announced = observer
            .first_announced_at()
            .expect("the lone receipt was committed");
        let waited = announced.saturating_duration_since(started);

        assert_eq!(
            observer.batches().len(),
            1,
            "one batch, holding one receipt"
        );
        assert!(
            waited < core::time::Duration::from_secs(1),
            "a lone receipt waited {waited:?}; CA-008-01 allows under a second"
        );
        assert!(
            waited >= core::time::Duration::from_millis(250),
            "it must have waited for the delay bound, not been flushed by \
             something else: {waited:?}"
        );
    }

    /// The deadline is measured from the **first** receipt of the batch.
    ///
    /// Restarting it on every arrival looks equivalent and is not: under a
    /// steady trickle — one receipt every 50 ms against a 250 ms bound — the
    /// timer would be reset before it ever fired, and with a size bound the
    /// stream never reaches, nothing would be committed at all. The interface
    /// would show a session delivering messages and a journal that never moves.
    ///
    /// Mutation-checked: this test is the one that fails when the deadline is
    /// reset inside the `select!`.
    #[tokio::test(start_paused = true)]
    async fn a_steady_trickle_cannot_postpone_the_commit_for_ever() {
        let (journal, receipts) = a_backlog(20);
        let pipeline =
            ReceiptPipeline::new(Correlator::new(journal, frozen()), FakeOrphans::default())
                .with_policy(BatchPolicy {
                    // Never reached by the twenty receipts below: only the deadline
                    // can flush this batch.
                    max_receipts: 10_000,
                    max_delay: core::time::Duration::from_millis(250),
                });
        let observer = RecordingObserver::default();

        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        let started = tokio::time::Instant::now();

        let trickle = tokio::spawn(async move {
            for receipt in receipts {
                sender.send(receipt).await.expect("room");
                tokio::time::sleep(core::time::Duration::from_millis(50)).await;
            }
        });

        pipeline.run(receiver, &observer).await;
        trickle.await.expect("the trickle ends");

        let announced = observer
            .first_announced_at()
            .expect("a batch must have been committed");

        assert!(
            announced.saturating_duration_since(started) < core::time::Duration::from_millis(400),
            "the first batch waited {:?}; the deadline runs from the first \
             receipt, not from the last",
            announced.saturating_duration_since(started)
        );
        assert!(
            observer.batches().len() > 1,
            "a trickle spanning a second must produce several batches, not one"
        );
    }

    /// CA-008-04 — orphans travel with the batch and reach their own journal.
    #[tokio::test(start_paused = true)]
    async fn orphans_are_written_alongside_the_transitions_of_the_same_batch() {
        let client_message_id = ClientMessageId::new();
        let journal = FakeJournal::holding(&[("KNOWN", client_message_id)]);
        let orphans = FakeOrphans::default();
        let pipeline = ReceiptPipeline::new(Correlator::new(journal, frozen()), orphans);
        let observer = RecordingObserver::default();

        let mut pending = vec![
            IncomingReceipt {
                session_id: None,
                receipt: parse_receipt_body("id:KNOWN stat:DELIVRD"),
            },
            IncomingReceipt {
                session_id: None,
                receipt: parse_receipt_body("id:STRANGER stat:UNDELIV err:058"),
            },
            IncomingReceipt {
                session_id: None,
                receipt: parse_receipt_body("nothing readable at all"),
            },
        ];

        let outcome = pipeline.commit(&mut pending, &observer).await;

        assert_eq!(outcome.applied, 1);
        assert_eq!(outcome.orphaned, 2);
        assert!(pending.is_empty(), "the batch is consumed");

        let written = pipeline
            .orphans
            .written
            .lock()
            .expect("uncontended")
            .clone();
        let reasons: Vec<_> = written.iter().map(|orphan| orphan.reason).collect();

        assert_eq!(
            reasons,
            vec![OrphanReason::UnknownIdentifier, OrphanReason::NoIdentifier]
        );
        // One transaction for both orphans, not one each.
        assert_eq!(
            *pipeline.orphans.transactions.lock().expect("uncontended"),
            1
        );
    }

    /// A batch the journal refuses as a whole is replayed one transition at a
    /// time. Without the fallback, one deleted message would discard the other
    /// hundred and ninety-nine deliveries of its batch.
    #[tokio::test(start_paused = true)]
    async fn a_refused_batch_is_replayed_one_transition_at_a_time() {
        let (mut journal, receipts) = a_backlog(3);
        journal.refuse_batches = true;

        let pipeline =
            ReceiptPipeline::new(Correlator::new(journal, frozen()), FakeOrphans::default());
        let observer = RecordingObserver::default();
        let mut pending = receipts;

        let outcome = pipeline.commit(&mut pending, &observer).await;

        assert_eq!(
            outcome.applied, 3,
            "each transition landed on the second try"
        );
        assert_eq!(
            pipeline.correlator().repository().transactions(),
            3,
            "the fallback is one transaction per transition"
        );
    }

    /// An empty batch announces nothing and writes nothing: an observer must
    /// not be woken to repaint the same screen.
    #[tokio::test(start_paused = true)]
    async fn an_empty_batch_touches_nothing() {
        let pipeline = ReceiptPipeline::new(
            Correlator::new(FakeJournal::default(), frozen()),
            FakeOrphans::default(),
        );
        let observer = RecordingObserver::default();
        let mut pending = Vec::new();

        assert_eq!(
            pipeline.commit(&mut pending, &observer).await,
            super::BatchOutcome::default()
        );
        assert!(observer.batches().is_empty());
        assert_eq!(pipeline.correlator().repository().transactions(), 0);
    }

    /// A shutdown must not lose the receipts already read off the socket: the
    /// loop commits what it holds before returning.
    #[tokio::test(start_paused = true)]
    async fn closing_the_channel_commits_what_is_still_in_hand() {
        let (journal, receipts) = a_backlog(5);
        let pipeline =
            ReceiptPipeline::new(Correlator::new(journal, frozen()), FakeOrphans::default())
                .with_policy(BatchPolicy {
                    // Far above what will arrive: only the close can flush this.
                    max_receipts: 10_000,
                    max_delay: core::time::Duration::from_secs(3_600),
                });
        let observer = RecordingObserver::default();

        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        for receipt in receipts {
            sender.send(receipt).await.expect("room");
        }
        drop(sender);

        pipeline.run(receiver, &observer).await;

        assert_eq!(
            observer.batches().iter().map(Vec::len).sum::<usize>(),
            5,
            "the last batch is committed on shutdown"
        );
    }
}
