//! What the send tests need that is not the send path.
//!
//! Two doubles and a clock:
//!
//! * [`Journal`] — an in-memory [`MessageRepository`] that also **records the
//!   order** of its calls. That recording is the whole of CA-006-02: "the
//!   message is in the journal before the `submit_sm` reaches the socket" is
//!   not a property of a final state, it is a property of a sequence, and only
//!   an instrumented double can see it.
//! * the message centre of milestone 005, which `super` re-exports from
//!   `smpp_session::testing` rather than copying.
//! * [`FrozenClock`] — CLAUDE.md §7 wants the clock injected, so a test can
//!   assert on `created_at` exactly instead of "roughly now".

// `tests/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: `#[tokio::test]` expands to `Runtime::block_on`,
//     which `clippy.toml` reserves for "the binary entry point". A test
//     harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;

use messaging::message::{Message, MessageState, MessageStateUpdate, SmscMessageIdUpdate};
use messaging::ports::{MessageRepository, MessageStoreError};
use smpp_core::time::{Clock, Timestamp};
use smpp_core::types::ClientMessageId;
use tokio::sync::Mutex;

/// One thing the journal was asked to do, in the order it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalEvent {
    /// A message was written, in this state.
    Inserted(MessageState),
    /// A batch of transitions was applied, to these states in order.
    Transitioned(Vec<MessageState>),
}

/// An in-memory message journal that remembers what it was asked, and when.
#[derive(Clone, Default)]
pub(crate) struct Journal {
    inner: Arc<Mutex<JournalState>>,
    /// When set, the write-ahead insert fails with this.
    insert_failure: Option<MessageStoreError>,
    /// When set, the final transitions fail with this.
    ///
    /// Separate from [`Self::insert_failure`] on purpose: the two failures
    /// mean opposite things — one leaves nothing sent, the other leaves
    /// everything sent and unrecorded — and a single flag could only ever
    /// exercise the first, which is how the second went uncovered.
    transition_failure: Option<MessageStoreError>,
    /// Read at insert time, so the test can ask "and how many `submit_sm` had
    /// crossed the socket by then?".
    ///
    /// This is what turns CA-006-02 into an assertion. The criterion is about
    /// an **order** across two components, and no final state can show one: a
    /// row that ends up `ACCEPTED` says nothing about whether it was written
    /// before or after the PDU went out. Sampling the other side's counter
    /// from inside the insert does.
    witness: Option<Arc<dyn Fn() -> u32 + Send + Sync>>,
}

impl core::fmt::Debug for Journal {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Journal").finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct JournalState {
    rows: Vec<Message>,
    events: Vec<JournalEvent>,
    submissions_at_insert: Option<u32>,
}

impl Journal {
    /// An empty journal that accepts everything.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A journal that refuses the write-ahead insert — the "full disk before
    /// anything was sent" case.
    pub(crate) fn refusing_inserts(failure: MessageStoreError) -> Self {
        Self {
            insert_failure: Some(failure),
            ..Self::default()
        }
    }

    /// A journal that accepts the insert and refuses the final transitions.
    ///
    /// The database locking up, or the disk filling, in the moment between the
    /// last `submit_sm_resp` and the commit — the message is out, the record
    /// is not.
    pub(crate) fn refusing_transitions(failure: MessageStoreError) -> Self {
        Self {
            transition_failure: Some(failure),
            ..Self::default()
        }
    }

    /// The same journal, sampling `witness` at insert time.
    pub(crate) fn witnessing(mut self, witness: impl Fn() -> u32 + Send + Sync + 'static) -> Self {
        self.witness = Some(Arc::new(witness));
        self
    }

    /// What the witness reported when the write-ahead insert happened.
    ///
    /// `None` if nothing was ever inserted.
    pub(crate) async fn submissions_at_insert(&self) -> Option<u32> {
        self.inner.lock().await.submissions_at_insert
    }

    /// Every call the journal received, in order.
    pub(crate) async fn events(&self) -> Vec<JournalEvent> {
        self.inner.lock().await.events.clone()
    }

    /// The row under `client_message_id`, as it stands now.
    pub(crate) async fn row(&self, client_message_id: ClientMessageId) -> Option<Message> {
        self.inner
            .lock()
            .await
            .rows
            .iter()
            .find(|row| row.client_message_id == client_message_id)
            .cloned()
    }

    /// How many rows the journal holds.
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.rows.len()
    }

    /// Writes an `smsc_message_id` onto a row, bypassing the transition rules.
    ///
    /// Only a test setup uses this, and only to reach a state the send path
    /// cannot produce on its own: a **terminal** message that nevertheless
    /// carries an identifier a receipt can correlate to. The send path refuses
    /// to build one — `Sender::aggregate` drops the identifier of a failed
    /// message precisely so no receipt can find it — and that refusal is the
    /// *first* barrier. Reaching past it is the only way to exercise the
    /// *second*, the state machine's `FAILED → DELIVERED` refusal, which would
    /// otherwise be shadowed for ever by the first.
    pub(crate) async fn force_identifier(
        &self,
        client_message_id: ClientMessageId,
        smsc_message_id: &str,
    ) {
        if let Some(row) = self
            .inner
            .lock()
            .await
            .rows
            .iter_mut()
            .find(|row| row.client_message_id == client_message_id)
        {
            row.smsc_message_id = Some(smsc_message_id.to_owned());
        }
    }

    /// Applies one transition to the stored row, the way SQLite does.
    ///
    /// The merge semantics are copied from the schema deliberately: `None`
    /// leaves the column alone, `attempts` takes `MAX(attempts, ?)`, and an
    /// **illegal** transition is a no-op. A double that overwrote instead
    /// would let a bug pass here and fail against the real repository — which
    /// is exactly what happened while `can_move_to` had no caller.
    ///
    /// Returns whether anything was written.
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
        if let Some(stat) = update.dlr_stat.clone() {
            row.dlr_stat = Some(stat);
        }
        if let Some(err) = update.dlr_err.clone() {
            row.dlr_err = Some(err);
        }
        if let Some(instant) = update.sent_at {
            row.sent_at = Some(instant);
        }
        if let Some(instant) = update.resp_at {
            row.resp_at = Some(instant);
        }
        if let Some(instant) = update.dlr_at {
            row.dlr_at = Some(instant);
        }
        if let Some(attempt) = update.attempt {
            row.attempts = row.attempts.max(attempt);
        }

        true
    }
}

impl MessageRepository for Journal {
    async fn insert_message(&self, message: &Message) -> Result<(), MessageStoreError> {
        if let Some(failure) = self.insert_failure.clone() {
            return Err(failure);
        }

        let mut state = self.inner.lock().await;

        if state
            .rows
            .iter()
            .any(|row| row.client_message_id == message.client_message_id)
        {
            return Err(MessageStoreError::Conflict);
        }

        if let Some(witness) = self.witness.as_ref() {
            state.submissions_at_insert = Some(witness());
        }

        state.events.push(JournalEvent::Inserted(message.state));
        state.rows.push(message.clone());

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
        Ok(self.row(client_message_id).await)
    }

    async fn find_message_by_smsc_id(
        &self,
        smsc_message_id: &str,
        session_id: Option<smpp_core::types::SessionId>,
    ) -> Result<Option<Message>, MessageStoreError> {
        // The session is part of the key, exactly as it is in SQL. A double
        // that ignored it would let a receipt correlate across sessions here
        // and fail against the real repository — which is the failure mode the
        // predicate exists to prevent.
        Ok(self
            .inner
            .lock()
            .await
            .rows
            .iter()
            .find(|row| {
                row.smsc_message_id.as_deref() == Some(smsc_message_id)
                    && match (session_id, row.session_id) {
                        (None, _) => true,
                        (Some(wanted), Some(stored)) => wanted == stored,
                        (Some(_), None) => false,
                    }
            })
            .cloned())
    }

    async fn update_state(&self, update: &MessageStateUpdate) -> Result<bool, MessageStoreError> {
        self.update_states(core::slice::from_ref(update))
            .await
            .map(|applied| applied == 1)
    }

    async fn update_states(
        &self,
        updates: &[MessageStateUpdate],
    ) -> Result<u64, MessageStoreError> {
        if let Some(failure) = self.transition_failure.clone() {
            return Err(failure);
        }

        let mut state = self.inner.lock().await;

        // All-or-nothing, like the transaction it stands in for: a missing
        // message rolls the whole batch back.
        if updates.iter().any(|update| {
            !state
                .rows
                .iter()
                .any(|row| row.client_message_id == update.client_message_id)
        }) {
            return Err(MessageStoreError::NotFound);
        }

        let mut applied = Vec::with_capacity(updates.len());

        for update in updates {
            if let Some(row) = state
                .rows
                .iter_mut()
                .find(|row| row.client_message_id == update.client_message_id)
            {
                if Journal::apply(row, update) {
                    applied.push(update.state);
                }
            }
        }

        // The number of transitions actually WRITTEN, not the size of the
        // batch. The two differ whenever the machine refuses one, and a double
        // that reported the batch size would let the pipeline announce to the
        // interface a transition the real journal declines.
        let written = applied.len() as u64;

        state.events.push(JournalEvent::Transitioned(applied));

        Ok(written)
    }
}

/// A clock that never moves.
///
/// CLAUDE.md §7: injected, so `created_at` and `sent_at` are values a test can
/// assert on rather than a window it has to tolerate.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrozenClock(Timestamp);

impl FrozenClock {
    /// A clock stopped at `text`, an RFC 3339 instant.
    pub(crate) fn at(text: &str) -> Self {
        Self(Timestamp::parse(text).expect("the fixture instant is valid RFC 3339"))
    }

    /// The instant it reports.
    pub(crate) const fn instant(self) -> Timestamp {
        self.0
    }
}

impl Clock for FrozenClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// A session whose `submit` never returns, and says so first.
///
/// The instrument CA-006-03 needs. "A brutal stop between the persistence and
/// the emission" is a moment, not a state, and the only way to stop the world
/// exactly there is to suspend the send at its first `.await` past the insert
/// and abort the task that owns it. Aborting a Tokio task at a suspension
/// point is as close to killing the process as a test can get without a second
/// process.
pub(crate) struct HangingSession {
    session_id: smpp_core::types::SessionId,
    reached: tokio::sync::mpsc::UnboundedSender<()>,
}

impl HangingSession {
    /// A hanging session, and the receiver that fires when `submit` is reached.
    pub(crate) fn new() -> (Self, tokio::sync::mpsc::UnboundedReceiver<()>) {
        let (reached, receiver) = tokio::sync::mpsc::unbounded_channel();

        (
            Self {
                session_id: smpp_core::types::SessionId::new(),
                reached,
            },
            receiver,
        )
    }
}

impl messaging::ports::SmscSession for HangingSession {
    fn session_id(&self) -> smpp_core::types::SessionId {
        self.session_id
    }

    fn gsm7_packing(&self) -> smpp_core::values::Gsm7BitPacking {
        smpp_core::values::Gsm7BitPacking::Unpacked
    }

    fn gsm7_charset(&self) -> smpp_core::values::Gsm7BitCharset {
        smpp_core::values::Gsm7BitCharset::Gsm0338
    }

    async fn submit(
        &self,
        _pdu: smpp_core::codec::Pdu,
    ) -> Result<smpp_core::codec::Command, messaging::ports::SubmitError> {
        let _ignored = self.reached.send(());

        core::future::pending().await
    }
}
