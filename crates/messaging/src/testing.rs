//! Doubles for the campaign tests (milestone 010).
//!
//! # Why they live in `src/` rather than in `tests/`
//!
//! The same argument `smpp_session::testing` makes: the unit tests of this
//! crate, its integration tests and its property tests all need the same
//! journal and the same message centre, and a second copy of a double is a
//! double that drifts. Gated behind `test-support` so nothing here reaches the
//! application binary, and behind `cfg(test)` as well so the crate's own unit
//! tests reach it without the feature.
//!
//! # What each one is for
//!
//! | Double | Stands in for | The failure it makes reachable |
//! |---|---|---|
//! | [`MemoryJournal`] | `persistence` | a conflicting write-ahead key, a journal that cannot be read |
//! | [`FakeSmsc`] | a bound session | a rejection, a timeout, a message centre that stops answering |
//! | [`StaticRecipients`] | a contact list | a source that fails half-way through |
//! | [`GeneratedRecipients`] | a contact list of 500 000 | volume, without 500 000 rows in memory |
//! | [`FixedClock`] | the system clock | an assertion on `created_at` that is exact |
//!
//! [`GeneratedRecipients`] is the one that could not be replaced by a `Vec`:
//! CA-010-01 is a statement about memory, and a double that materialised half a
//! million recipients would be the very growth the criterion forbids.
//!
//! # `unwrap`, `expect` and `panic!` are relaxed below
//!
//! `clippy.toml` reopens them under `cfg(test)`, which this module is not when
//! the feature is on — it is library code, so the workspace `deny` would
//! otherwise apply. In a test double a panic **is** the failure report, and the
//! alternative is an error path no test would ever read.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use smpp_core::codec::{Command, Pdu};
use smpp_core::time::{Clock, Timestamp};
use smpp_core::types::{ClientMessageId, Msisdn, SessionId};
use smpp_core::values::{CommandStatus, Gsm7BitCharset, Gsm7BitPacking};
use tokio::sync::{Mutex, Semaphore};

use crate::message::{Message, MessageState, MessageStateUpdate, SmscMessageIdUpdate};
use crate::ports::{
    MessageRepository, MessageStoreError, Recipient, RecipientSource, RecipientSourceError,
    SmscSession, SubmitError,
};

/// A clock that never moves, so an assertion on an instant is exact.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(Timestamp);

impl FixedClock {
    /// A clock frozen at `raw`, which must be RFC 3339.
    ///
    /// # Panics
    ///
    /// If `raw` is not RFC 3339. A test fixture that does not parse is a test
    /// that cannot run, and reporting it as a panic is the shortest path to the
    /// line that is wrong.
    #[must_use]
    pub fn at(raw: &str) -> Self {
        Self(Timestamp::parse(raw).expect("the fixture instant is RFC 3339"))
    }

    /// The instant this clock reads.
    #[must_use]
    pub const fn instant(&self) -> Timestamp {
        self.0
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self::at("2026-07-26T12:00:00Z")
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// A clock that moves with Tokio's virtual time.
///
/// The one thing [`FixedClock`] cannot do: a campaign that waits for its
/// scheduled start reads the clock *after* sleeping, and a frozen clock would
/// tell it to wait again, for ever.
///
/// Under `#[tokio::test(start_paused = true)]` the runtime advances its clock to
/// the next deadline as soon as every task is idle, so this reads exactly the
/// instant the campaign believes it is sleeping until — deterministically, and
/// at no wall-clock cost. Outside a paused runtime it is an ordinary offset
/// clock.
#[derive(Debug, Clone, Copy)]
pub struct VirtualClock {
    base: Timestamp,
    origin: tokio::time::Instant,
}

impl VirtualClock {
    /// A clock reading `raw` at the moment it is built, and advancing from
    /// there.
    ///
    /// # Panics
    ///
    /// If `raw` is not RFC 3339.
    #[must_use]
    pub fn at(raw: &str) -> Self {
        Self {
            base: Timestamp::parse(raw).expect("the fixture instant is RFC 3339"),
            origin: tokio::time::Instant::now(),
        }
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> Timestamp {
        let elapsed =
            time::Duration::try_from(self.origin.elapsed()).unwrap_or(time::Duration::ZERO);

        Timestamp::from_offset_date_time(*self.base.as_offset_date_time() + elapsed)
    }
}

/// A message row with plausible fields, for a test that only cares about two.
#[must_use]
pub fn journal_row(client_message_id: ClientMessageId, state: MessageState) -> Message {
    Message {
        client_message_id,
        campaign_id: None,
        session_id: None,
        smsc_message_id: None,
        source_addr: None,
        source_ton: None,
        source_npi: None,
        dest_addr: None,
        dest_ton: None,
        dest_npi: None,
        data_coding: None,
        segments: 1,
        text: None,
        state,
        command_status: None,
        dlr_stat: None,
        dlr_err: None,
        attempts: 0,
        created_at: FixedClock::default().instant(),
        sent_at: None,
        resp_at: None,
        dlr_at: None,
    }
}

/// A way for the journal to misbehave, for the duration of one run.
///
/// Set at runtime rather than at construction, because the family that matters
/// is a **sequence**: one run whose verdicts are lost, then a resume against a
/// journal that works. A double configured once could not express it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JournalFault {
    /// Everything works.
    #[default]
    None,

    /// Verdicts are swallowed — see [`MemoryJournal::lose_verdicts`].
    LosesVerdicts,

    /// Every read fails.
    RefusesReads,

    /// Every write fails.
    RefusesWrites,
}

/// An in-memory message journal.
///
/// Reproduces the two behaviours the campaign runner depends on, and neither is
/// decoration:
///
/// * a **conflicting** `client_message_id` is refused, because that conflict is
///   the primary guard of the emission invariant — the runner reads it as "this
///   recipient already has a row";
/// * an **illegal** transition is a no-op that reports `false`, because the
///   machine of spec §14.3 refuses some of them and a double that overwrote
///   would let a bug pass here and fail against SQLite.
#[derive(Clone, Default)]
pub struct MemoryJournal {
    inner: Arc<Mutex<JournalState>>,
    reads_fail: bool,
    insert_failure: Option<MessageStoreError>,
    /// When set, every method yields before doing anything.
    ///
    /// A **suspension point** per journal operation, which is what lets a
    /// cancellation land between the guard and the send, or between the insert
    /// and the submission, rather than only in a retry delay. Without them the
    /// whole campaign runs in one poll and no command can interleave — which is
    /// how a property test can look thorough and exercise one path.
    yields: bool,
    /// When false, rows are counted and thrown away.
    ///
    /// For the volume test only, where retaining half a million rows would
    /// measure the double rather than the code under test. See the module
    /// header.
    retain: bool,
}

#[derive(Debug, Default)]
struct JournalState {
    rows: HashMap<ClientMessageId, Message>,
    inserted: u64,
    transitions: u64,
    reads: u64,
    fault: JournalFault,
    lost_verdicts: u64,
    witness: Option<FakeSmsc>,
    submissions_at_first_transition: Option<u64>,
}

impl core::fmt::Debug for MemoryJournal {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("MemoryJournal")
            .finish_non_exhaustive()
    }
}

impl MemoryJournal {
    /// An empty journal that accepts everything and remembers its rows.
    #[must_use]
    pub fn new() -> Self {
        Self {
            retain: true,
            ..Self::default()
        }
    }

    /// The same journal, yielding before every operation.
    #[must_use]
    pub const fn yielding(mut self) -> Self {
        self.yields = true;
        self
    }

    /// A journal that counts what it is given and keeps none of it.
    ///
    /// Every lookup answers "no such message", so every recipient is admitted
    /// fresh. For the volume test, whose subject is the memory of the *client*
    /// and not of the database.
    #[must_use]
    pub fn forgetful() -> Self {
        Self::default()
    }

    /// The same journal, refusing every read.
    #[must_use]
    pub fn refusing_reads(mut self) -> Self {
        self.reads_fail = true;
        self
    }

    /// The same journal, refusing every write-ahead insert.
    #[must_use]
    pub fn refusing_inserts(mut self, failure: MessageStoreError) -> Self {
        self.insert_failure = Some(failure);
        self
    }

    /// Samples `witness` the first time a transition is written.
    ///
    /// How an **order** across two components is asserted on: "the attempt was
    /// journalled before any PDU left" is not a property of any final state — a
    /// row that ends `ACCEPTED` says nothing about when it was written — and
    /// reading the other side's counter from inside the write is the only thing
    /// that can see it.
    pub async fn witness_transitions(&self, witness: FakeSmsc) {
        self.inner.lock().await.witness = Some(witness);
    }

    /// How many `submit_sm` had reached the message centre when the first
    /// transition was written.
    ///
    /// `None` if no transition was ever written.
    pub async fn submissions_at_first_transition(&self) -> Option<u64> {
        self.inner.lock().await.submissions_at_first_transition
    }

    /// Makes the journal **swallow every verdict** it is handed.
    ///
    /// A verdict is a transition carrying `resp_at`: the send path writes one
    /// exactly once, after the message centre has answered or failed to. Losing
    /// it is not a database fault — it is what a `kill -9` looks like from the
    /// journal's side, the process dying between the `submit_sm` leaving and the
    /// outcome being committed.
    ///
    /// The transition the send path writes **before** the socket is untouched,
    /// which is the point: it is the one that has to survive for a resume to
    /// know an emission may have happened.
    pub async fn lose_verdicts(&self, losing: bool) {
        self.set_fault(if losing {
            JournalFault::LosesVerdicts
        } else {
            JournalFault::None
        })
        .await;
    }

    /// How this journal misbehaves from now on.
    pub async fn set_fault(&self, fault: JournalFault) {
        self.inner.lock().await.fault = fault;
    }

    /// Yields, when this journal was built to.
    async fn suspend(&self) {
        if self.yields {
            tokio::task::yield_now().await;
        }
    }

    /// How many verdicts were swallowed.
    pub async fn lost_verdicts(&self) -> u64 {
        self.inner.lock().await.lost_verdicts
    }

    /// Writes a row directly, bypassing the transition rules.
    ///
    /// How a test reaches the state a crash would have left behind: a `SENT`
    /// row whose response never came cannot be produced by the send path, which
    /// always writes an outcome.
    pub async fn force_row(&self, message: Message) {
        self.inner
            .lock()
            .await
            .rows
            .insert(message.client_message_id, message);
    }

    /// The row under `client_message_id`, as it stands now.
    pub async fn row(&self, client_message_id: ClientMessageId) -> Option<Message> {
        self.inner
            .lock()
            .await
            .rows
            .get(&client_message_id)
            .cloned()
    }

    /// Every row, in no particular order.
    pub async fn rows(&self) -> Vec<Message> {
        self.inner.lock().await.rows.values().cloned().collect()
    }

    /// How many write-ahead inserts were **accepted**.
    ///
    /// Distinct rows, in other words: a conflicting insert is refused and not
    /// counted. This is the figure CA-010-04 compares against the number of
    /// recipients.
    pub async fn inserted(&self) -> u64 {
        self.inner.lock().await.inserted
    }

    /// How many transitions were written.
    pub async fn transitions(&self) -> u64 {
        self.inner.lock().await.transitions
    }

    /// How many times a message was looked up by its key.
    ///
    /// What makes `StartMode` observable: a fresh campaign asks the journal
    /// nothing and lets the write-ahead insert answer, a resumed one asks first.
    /// Without this counter the two modes are indistinguishable — both are safe,
    /// so no outcome differs — and a test that cannot tell them apart is a test
    /// that would not notice the mode being ignored.
    pub async fn reads(&self) -> u64 {
        self.inner.lock().await.reads
    }

    /// Applies one transition the way the schema does.
    fn apply(row: &mut Message, update: &MessageStateUpdate) -> bool {
        if !row.state.can_move_to(update.state) {
            return false;
        }

        row.state = update.state;

        if let SmscMessageIdUpdate::Set(identifier) = &update.smsc_message_id {
            row.smsc_message_id = Some(identifier.clone());
        }
        if let Some(status) = update.command_status {
            row.command_status = Some(status);
        }
        if let Some(instant) = update.sent_at {
            row.sent_at = Some(instant);
        }
        if let Some(instant) = update.resp_at {
            row.resp_at = Some(instant);
        }
        if let Some(attempt) = update.attempt {
            row.attempts = row.attempts.max(attempt);
        }

        true
    }
}

impl MessageRepository for MemoryJournal {
    async fn insert_message(&self, message: &Message) -> Result<(), MessageStoreError> {
        self.suspend().await;

        if let Some(failure) = self.insert_failure.clone() {
            return Err(failure);
        }

        let mut state = self.inner.lock().await;

        if state.fault == JournalFault::RefusesWrites {
            return Err(unavailable());
        }

        if self.retain {
            if state.rows.contains_key(&message.client_message_id) {
                return Err(MessageStoreError::Conflict);
            }

            state
                .rows
                .insert(message.client_message_id, message.clone());
        }

        state.inserted += 1;

        Ok(())
    }

    async fn insert_messages(&self, messages: &[Message]) -> Result<u64, MessageStoreError> {
        for message in messages {
            self.insert_message(message).await?;
        }

        Ok(messages.len() as u64)
    }

    async fn find_message(
        &self,
        client_message_id: ClientMessageId,
    ) -> Result<Option<Message>, MessageStoreError> {
        self.suspend().await;

        let mut state = self.inner.lock().await;

        if self.reads_fail || state.fault == JournalFault::RefusesReads {
            return Err(unavailable());
        }

        state.reads += 1;

        Ok(state.rows.get(&client_message_id).cloned())
    }

    async fn find_message_by_smsc_id(
        &self,
        smsc_message_id: &str,
        session_id: Option<SessionId>,
    ) -> Result<Option<Message>, MessageStoreError> {
        self.suspend().await;

        if self.reads_fail {
            return Err(unavailable());
        }

        Ok(self
            .inner
            .lock()
            .await
            .rows
            .values()
            .find(|row| {
                row.smsc_message_id.as_deref() == Some(smsc_message_id)
                    && session_id.is_none_or(|wanted| row.session_id == Some(wanted))
            })
            .cloned())
    }

    async fn update_state(&self, update: &MessageStateUpdate) -> Result<bool, MessageStoreError> {
        Ok(self.update_states(core::slice::from_ref(update)).await? == 1)
    }

    async fn update_states(
        &self,
        updates: &[MessageStateUpdate],
    ) -> Result<u64, MessageStoreError> {
        self.suspend().await;

        let mut state = self.inner.lock().await;
        let mut written = 0;

        if state.fault == JournalFault::RefusesWrites {
            return Err(unavailable());
        }

        if state.submissions_at_first_transition.is_none() {
            if let Some(witness) = state.witness.clone() {
                state.submissions_at_first_transition = Some(witness.submitted());
            }
        }

        // A crash after the emission and before the commit. The call is
        // reported as having happened — the process died, it did not get an
        // error back — and nothing is written.
        if state.fault == JournalFault::LosesVerdicts
            && updates.iter().any(|update| update.resp_at.is_some())
        {
            state.lost_verdicts += 1;

            return Ok(0);
        }

        for update in updates {
            if !self.retain {
                written += 1;
                continue;
            }

            let Some(row) = state.rows.get_mut(&update.client_message_id) else {
                return Err(MessageStoreError::NotFound);
            };

            if Self::apply(row, update) {
                written += 1;
            }
        }

        state.transitions += written;

        Ok(written)
    }
}

/// The failure a journal reports when it will not answer.
fn unavailable() -> MessageStoreError {
    MessageStoreError::Unavailable {
        reason: String::from("the journal is unavailable"),
    }
}

/// What a [`FakeSmsc`] answers one `submit_sm` with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `ESME_ROK`, with a generated `message_id`.
    Accepted,
    /// A response carrying a refusal.
    Rejected(CommandStatus),
    /// No response at all.
    Failed(SubmitError),
}

/// What a [`FakeSmsc`] answers one `submit_multi` with.
///
/// Separate from [`Reply`] because the answers are not the same shape: a
/// `submit_multi_resp` is **partially** successful, carrying one identifier and
/// a list of the recipients it refused, each with its own status. Folding the
/// two into one enum would have made the batch double answer in the shape of a
/// `submit_sm`, which is the very confusion the fallback has to survive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MultiReply {
    /// `ESME_ROK` and a `submit_multi_resp` refusing these recipients.
    ///
    /// An empty list is the whole batch accepted.
    Accepted {
        /// Who this message centre refused, and how it says so.
        refused: Vec<Refused>,
    },

    /// A `submit_multi_resp` carrying a refusal of the whole PDU.
    Refused(CommandStatus),

    /// `generic_nack` — the message centre does not know the operation.
    Unsupported,

    /// `ESME_ROK` over a body that is not a `submit_multi_resp`.
    Unreadable,

    /// No response at all.
    Failed(SubmitError),
}

impl Default for MultiReply {
    fn default() -> Self {
        Self::Accepted {
            refused: Vec::new(),
        }
    }
}

/// One recipient a [`MultiReply::Accepted`] refuses.
///
/// # Why "who was refused" and "how it is written back" are two fields
///
/// They are the same string at every well-behaved message centre, and the whole
/// hazard of this milestone lives in the gap between them. A centre that refuses
/// `2250700000002` and renders it `00225700000002` in `unsuccess_sme` has
/// refused that recipient — the handset gets nothing — while the client cannot
/// attribute the refusal. Folding the two into one string would make that case
/// **unrepresentable**, and a test suite that cannot represent it is a test suite
/// that reports the batch accepted and says so with a green tick.
///
/// [`Self::destination`] is therefore the message centre's own truth, used to
/// decide what it accepted, and [`Self::quoted`] is only what travels in the
/// PDU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The destination that was really refused, as it appeared in the PDU.
    pub destination: String,
    /// How this message centre writes it in `unsuccess_sme`.
    pub quoted: String,
    /// Its own `error_status_code`.
    pub status: CommandStatus,
}

impl Refused {
    /// A refusal quoted back verbatim — what a well-behaved centre does.
    #[must_use]
    pub fn plain(destination: impl Into<String>, status: CommandStatus) -> Self {
        let destination = destination.into();

        Self {
            quoted: destination.clone(),
            destination,
            status,
        }
    }

    /// A refusal quoted back in another form than the one it was sent in.
    #[must_use]
    pub fn quoted_as(
        destination: impl Into<String>,
        quoted: impl Into<String>,
        status: CommandStatus,
    ) -> Self {
        Self {
            destination: destination.into(),
            quoted: quoted.into(),
            status,
        }
    }
}

/// A message centre that answers from a script.
///
/// Narrower than `smpp_session::testing::Smsc` on purpose: that one is a socket
/// and a codec, which is what the session tests need. Here the subject is the
/// campaign runner, so the double stops at the port — *this PDU, that answer* —
/// and a test that wants a timeout writes one word instead of a socket that
/// stops reading.
#[derive(Clone)]
pub struct FakeSmsc {
    inner: Arc<SmscState>,
}

struct SmscState {
    session_id: SessionId,
    script: Mutex<VecDeque<Reply>>,
    fallback: Reply,
    multi_script: Mutex<VecDeque<MultiReply>>,
    multi_fallback: MultiReply,
    submitted: AtomicU64,
    multi_submitted: AtomicU64,
    destinations: Mutex<Option<Vec<(String, bool)>>>,
    gate: Option<Semaphore>,
    yields: bool,
}

impl core::fmt::Debug for FakeSmsc {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("FakeSmsc").finish_non_exhaustive()
    }
}

impl FakeSmsc {
    /// A message centre that accepts everything.
    #[must_use]
    pub fn accepting() -> Self {
        Self {
            inner: Arc::new(SmscState {
                session_id: SessionId::new(),
                script: Mutex::new(VecDeque::new()),
                fallback: Reply::Accepted,
                multi_script: Mutex::new(VecDeque::new()),
                multi_fallback: MultiReply::default(),
                submitted: AtomicU64::new(0),
                multi_submitted: AtomicU64::new(0),
                destinations: Mutex::new(None),
                gate: None,
                yields: false,
            }),
        }
    }

    /// The same message centre, yielding before it answers.
    ///
    /// One more suspension point, in the middle of the send path: between the
    /// journalled attempt and the verdict. See [`MemoryJournal::yielding`].
    #[must_use]
    pub fn yielding(mut self) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.inner) {
            state.yields = true;
        }

        self
    }

    /// A message centre that answers `replies` in order, then accepts.
    #[must_use]
    pub fn scripted(replies: impl IntoIterator<Item = Reply>) -> Self {
        let smsc = Self::accepting();

        // The lock is uncontended here: nothing else holds this `Arc` yet.
        if let Ok(mut script) = smsc.inner.script.try_lock() {
            script.extend(replies);
        }

        smsc
    }

    /// The same message centre, answering every `submit_multi` with `reply`.
    #[must_use]
    pub fn answering_multi(mut self, reply: MultiReply) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.inner) {
            state.multi_fallback = reply;
        }

        self
    }

    /// The same message centre, answering `replies` to the first
    /// `submit_multi`s and [`Self::answering_multi`]'s reply after that.
    ///
    /// What the capability latch is observed with: "the first batch is refused
    /// and the second is never attempted" needs a double that *would* answer a
    /// second one differently.
    #[must_use]
    pub fn multi_scripted(self, replies: impl IntoIterator<Item = MultiReply>) -> Self {
        // The lock is uncontended here: nothing else holds this `Arc` yet.
        if let Ok(mut script) = self.inner.multi_script.try_lock() {
            script.extend(replies);
        }

        self
    }

    /// How many `submit_multi` reached this message centre.
    #[must_use]
    pub fn multi_submitted(&self) -> u64 {
        self.inner.multi_submitted.load(Ordering::SeqCst)
    }

    /// The same message centre, answering `reply` once the script runs out.
    #[must_use]
    pub fn then(mut self, reply: Reply) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.inner) {
            state.fallback = reply;
        }

        self
    }

    /// The same message centre, recording every recipient it was given.
    ///
    /// What the emission invariant is checked against: "at most one emission per
    /// recipient" is a property of this list, and of nothing the runner reports
    /// about itself.
    #[must_use]
    pub fn recording(mut self) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.inner) {
            state.destinations = Mutex::new(Some(Vec::new()));
        }

        self
    }

    /// The same message centre, answering only as many submissions as it is
    /// given permits.
    ///
    /// How back-pressure is observed: a message centre that has answered nothing
    /// leaves the send window, the queue and then the reader stuck behind it, in
    /// that order.
    #[must_use]
    pub fn gated(mut self, permits: usize) -> Self {
        if let Some(state) = Arc::get_mut(&mut self.inner) {
            state.gate = Some(Semaphore::new(permits));
        }

        self
    }

    /// Lets `count` more submissions through.
    pub fn release(&self, count: usize) {
        if let Some(gate) = self.inner.gate.as_ref() {
            gate.add_permits(count);
        }
    }

    /// How many `submit_sm` reached this message centre.
    #[must_use]
    pub fn submitted(&self) -> u64 {
        self.inner.submitted.load(Ordering::SeqCst)
    }

    /// Every recipient this message centre was given, in order, with repeats.
    ///
    /// Empty unless the double was built with [`Self::recording`].
    pub async fn destinations(&self) -> Vec<String> {
        self.recorded()
            .await
            .into_iter()
            .map(|(number, _)| number)
            .collect()
    }

    /// Every recipient whose message this centre **accepted**, in order.
    ///
    /// The list the emission invariant is stated over: spec §10.7 replays a
    /// refused message, so a recipient may legitimately appear twice in
    /// [`Self::destinations`]. What may never repeat is a recipient here.
    pub async fn accepted_destinations(&self) -> Vec<String> {
        self.recorded()
            .await
            .into_iter()
            .filter_map(|(number, accepted)| accepted.then_some(number))
            .collect()
    }

    /// Answers one `submit_multi`, recording every recipient it named.
    ///
    /// Each destination is recorded with **its own** verdict, not the batch's:
    /// a `submit_multi_resp` is partially successful, so a recording that
    /// credited the whole batch to the top-level status could not tell the
    /// invariant "at most one accepted message per recipient" from its
    /// opposite.
    async fn answer_multi(
        &self,
        body: &smpp_core::pdus::SubmitMulti,
        sequence: u64,
    ) -> Result<Command, SubmitError> {
        use smpp_core::values::DestAddress;

        self.inner.multi_submitted.fetch_add(1, Ordering::SeqCst);

        let reply = self
            .inner
            .multi_script
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| self.inner.multi_fallback.clone());

        if let Some(recorded) = self.inner.destinations.lock().await.as_mut() {
            for address in body.dest_address() {
                let DestAddress::SmeAddress(sme) = address else {
                    continue;
                };

                let number = sme.destination_addr.as_str().to_owned();
                let accepted = match &reply {
                    // Read off `destination`, never off `quoted`: this is what
                    // the message centre knows it did, and the client's ability
                    // to work it out from the PDU is the thing under test.
                    MultiReply::Accepted { refused } => !refused
                        .iter()
                        .any(|entry| entry.destination.trim_start_matches('+') == number),
                    MultiReply::Refused(_)
                    | MultiReply::Unsupported
                    | MultiReply::Unreadable
                    | MultiReply::Failed(_) => false,
                };

                recorded.push((number, accepted));
            }
        }

        let number = u32::try_from(sequence).unwrap_or(u32::MAX);

        match reply {
            MultiReply::Accepted { refused } => Ok(Command::new(
                CommandStatus::EsmeRok,
                number,
                multi_response(&format!("batch-{sequence}"), &refused),
            )),
            MultiReply::Refused(status) => {
                Ok(Command::new(status, number, multi_response("", &[])))
            }
            MultiReply::Unsupported => Ok(Command::new(
                CommandStatus::EsmeRinvcmdid,
                number,
                Pdu::GenericNack,
            )),
            MultiReply::Unreadable => Ok(Command::new(
                CommandStatus::EsmeRok,
                number,
                submit_response("not-a-batch"),
            )),
            MultiReply::Failed(failure) => Err(failure),
        }
    }

    /// Every recipient, with whether the submission was accepted.
    async fn recorded(&self) -> Vec<(String, bool)> {
        self.inner
            .destinations
            .lock()
            .await
            .clone()
            .unwrap_or_default()
    }
}

impl SmscSession for FakeSmsc {
    fn session_id(&self) -> SessionId {
        self.inner.session_id
    }

    fn gsm7_packing(&self) -> Gsm7BitPacking {
        Gsm7BitPacking::default()
    }

    fn gsm7_charset(&self) -> Gsm7BitCharset {
        Gsm7BitCharset::default()
    }

    async fn submit(&self, pdu: Pdu) -> Result<Command, SubmitError> {
        if self.inner.yields {
            tokio::task::yield_now().await;
        }

        if let Some(gate) = self.inner.gate.as_ref() {
            // `forget` rather than dropping the permit: the point of the gate is
            // that a submission consumes one, so a released permit lets exactly
            // one more through.
            match gate.acquire().await {
                Ok(permit) => permit.forget(),
                Err(_) => return Err(SubmitError::Closed),
            }
        }

        let sequence = self.inner.submitted.fetch_add(1, Ordering::SeqCst) + 1;

        if let Pdu::SubmitMulti(body) = &pdu {
            return self.answer_multi(body, sequence).await;
        }

        let reply = self
            .inner
            .script
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| self.inner.fallback.clone());

        // Recorded WITH its verdict, and after the verdict is known: the
        // invariant is about accepted messages, and a recording that only knew
        // the recipient could not tell a legitimate replay from a duplicate.
        if let Pdu::SubmitSm(body) = &pdu {
            if let Some(recorded) = self.inner.destinations.lock().await.as_mut() {
                recorded.push((
                    body.destination_addr.as_str().to_owned(),
                    reply == Reply::Accepted,
                ));
            }
        }

        match reply {
            Reply::Accepted => Ok(Command::new(
                CommandStatus::EsmeRok,
                u32::try_from(sequence).unwrap_or(u32::MAX),
                submit_response(&format!("smsc-{sequence}")),
            )),
            Reply::Rejected(status) => Ok(Command::new(
                status,
                u32::try_from(sequence).unwrap_or(u32::MAX),
                submit_response(""),
            )),
            Reply::Failed(failure) => Err(failure),
        }
    }
}

/// A `submit_sm_resp` carrying `message_id`.
fn submit_response(message_id: &str) -> Pdu {
    use core::str::FromStr as _;

    let identifier = smpp_core::octets::COctetString::from_str(message_id)
        .unwrap_or_else(|_| smpp_core::octets::COctetString::empty());

    Pdu::SubmitSmResp(smpp_core::pdus::SubmitSmResp::new(identifier, Vec::new()))
}

/// A `submit_multi_resp` carrying `message_id` and the refused destinations.
fn multi_response(message_id: &str, refused: &[Refused]) -> Pdu {
    use core::str::FromStr as _;

    use smpp_core::values::{Npi, Ton, UnsuccessSme};

    let identifier = smpp_core::octets::COctetString::from_str(message_id)
        .unwrap_or_else(|_| smpp_core::octets::COctetString::empty());

    let unsuccess = refused
        .iter()
        .map(|entry| {
            UnsuccessSme::new(
                Ton::International,
                Npi::Isdn,
                smpp_core::octets::COctetString::from_str(&entry.quoted)
                    .unwrap_or_else(|_| smpp_core::octets::COctetString::empty()),
                entry.status,
            )
        })
        .collect();

    Pdu::SubmitMultiResp(smpp_core::pdus::SubmitMultiResp::new(
        identifier,
        unsuccess,
        Vec::new(),
    ))
}

/// A recipient source holding a fixed list.
#[derive(Debug, Clone, Default)]
pub struct StaticRecipients {
    recipients: Vec<Recipient>,
    fails_after: Option<usize>,
}

impl StaticRecipients {
    /// A source over these numbers, with no attributes.
    ///
    /// # Panics
    ///
    /// If one of them is not a valid number.
    #[must_use]
    pub fn numbers(numbers: &[&str]) -> Self {
        Self {
            recipients: numbers
                .iter()
                .map(|raw| Recipient {
                    destination: Msisdn::parse(raw).expect("the fixture is a valid number"),
                    attributes: None,
                })
                .collect(),
            fails_after: None,
        }
    }

    /// A source over these recipients.
    #[must_use]
    pub fn new(recipients: Vec<Recipient>) -> Self {
        Self {
            recipients,
            fails_after: None,
        }
    }

    /// The same source, failing after `count` recipients.
    #[must_use]
    pub const fn failing_after(mut self, count: usize) -> Self {
        self.fails_after = Some(count);
        self
    }

    /// How many recipients this source holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recipients.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recipients.is_empty()
    }
}

impl RecipientSource for StaticRecipients {
    fn stream_recipients(&self) -> BoxStream<'_, Result<Recipient, RecipientSourceError>> {
        let fails_after = self.fails_after;

        futures_util::stream::iter(self.recipients.clone().into_iter().enumerate().map(
            move |(index, recipient)| match fails_after {
                Some(limit) if index >= limit => Err(RecipientSourceError::Unavailable {
                    reason: String::from("the recipient source stopped"),
                }),
                _ => Ok(recipient),
            },
        ))
        .boxed()
    }
}

/// A recipient source that synthesises numbers as it goes.
///
/// Holds one recipient at a time whatever `count` is, which is what makes the
/// volume measurement of CA-010-01 a measurement of the code under test.
#[derive(Debug, Clone, Copy)]
pub struct GeneratedRecipients {
    count: u64,
}

impl GeneratedRecipients {
    /// A source of `count` distinct recipients.
    #[must_use]
    pub const fn of(count: u64) -> Self {
        Self { count }
    }

    /// The `index`-th number this source produces.
    ///
    /// # Panics
    ///
    /// If the generated text is not a valid number, which would be a defect in
    /// this helper rather than in a test.
    #[must_use]
    pub fn number(index: u64) -> Msisdn {
        Msisdn::parse(&format!("+225{:010}", 7_000_000_000_u64 + index))
            .expect("a generated number is valid")
    }
}

impl RecipientSource for GeneratedRecipients {
    fn stream_recipients(&self) -> BoxStream<'_, Result<Recipient, RecipientSourceError>> {
        let count = self.count;

        futures_util::stream::unfold(0_u64, move |index| async move {
            if index >= count {
                return None;
            }

            let recipient = Recipient {
                destination: Self::number(index),
                attributes: None,
            };

            Some((Ok(recipient), index + 1))
        })
        .boxed()
    }
}
