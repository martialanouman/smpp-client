//! The unit send orchestrator (deliverable L-006-01).
//!
//! Spec §5.4, in the order the fiche states it and for the reason it gives:
//!
//! ```text
//! validate → encode → segment → build → PERSIST → submit → correlate → record
//! ```
//!
//! # Where the order is load-bearing
//!
//! **Everything that can be refused is refused before the insert.** A bad
//! recipient, a `service_type` too long for its field, a text a forced
//! encoding cannot write: each of those leaves no row behind (CA-006-07).
//! Anything persisted is a message this client believed it could send.
//!
//! **Nothing leaves the socket before the insert has committed.** That is the
//! write-ahead of CLAUDE.md §4 and ENF-FIA-01, and it is what makes a crash
//! recoverable: a process that dies between the two leaves a `QUEUED` row,
//! which milestone 010 resumes, and no PDU went out, so nothing is duplicated
//! (CA-006-03).
//!
//! The residual window is stated rather than glossed over, and it is wider
//! than "after the last segment": it opens the moment the **first**
//! `submit_sm` is accepted. A crash after segment 1 of a message of three
//! leaves the row `QUEUED`, and a resume re-sends all three — so the message
//! centre receives segment 1 twice. The same is true of the whole message
//! once every segment has been accepted and the transitions have not
//! committed.
//!
//! Closing that would mean writing `SENT` before the socket, which is the
//! other trade — no duplicate, but a message lost whenever the write succeeds
//! and the send does not. ENF-FIA-01 asks for "no message lost", so this is
//! the side the ordering falls on.
//!
//! One case is **not** in that window, because it is handled rather than
//! traded: a journal that refuses the *final* transitions after a successful
//! send. That is reported through [`SendReport::journalled`] rather than as an
//! error, so the caller learns the message went out — the opposite of what an
//! `Err` would tell it. See the comment at the call site.
//!
//! # One row per message, N PDUs
//!
//! A 400-character message is one `messages` row with `segments = 3` and three
//! `submit_sm` (CA-006-04). Per-segment identifiers are what milestone 008
//! needs to correlate receipts; until then the row keeps the identifier of the
//! **first** segment, which is the one an operator quotes to the message
//! centre, and [`SendReport::segments`] carries all of them.

use smpp_core::codec::Pdu;
use smpp_core::status_codes::{self, StatusClass};
use smpp_core::time::{Clock, Timestamp};
use smpp_core::types::{CampaignId, ClientMessageId, SessionId};
use smpp_core::values::CommandStatus;

use crate::encoding::EncodingChoice;
use crate::error::MessagingError;
use crate::message::{Message, MessageState, MessageStateUpdate};
use crate::ports::{MessageRepository, SmscSession, SubmitError};
use crate::retry::SendFailure;
use crate::segmentation::{
    segment, ConcatenationReferenceCounter, SegmentationMode, SegmentationOptions,
};
use crate::submit::{build_submit_sm, SubmitOptions};

/// What the interface asked to send.
///
/// The `client_message_id` is **supplied by the caller**, not minted here: it
/// is the write-ahead key, and a resumed send has to reuse the one already in
/// the journal or it would insert a second row for the same message
/// (spec §10.5). [`Self::new`] mints one for the ordinary first attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRequest {
    /// The write-ahead key.
    pub client_message_id: ClientMessageId,
    /// The message body, before encoding.
    pub text: String,
    /// The fields of spec §7.3.
    pub submit: SubmitOptions,
    /// Automatic encoding, or the one the operator forced.
    pub encoding: EncodingChoice,
    /// How the parts of a long message announce that they belong together.
    pub mode: SegmentationMode,
    /// Which sending attempt this is, counting from 1.
    ///
    /// Not defaulted, and not an increment: `attempts` is stored as
    /// `MAX(attempts, ?)` so that a replayed transition is a no-op, which only
    /// works if the caller says which attempt it is. Milestone 010 passes the
    /// number it read back from the journal.
    pub attempt: u32,

    /// Whether the caller will accept this attempt's verdict as final.
    ///
    /// `true` for a unit send, which has no replay policy: its failure is the
    /// message's failure and the row is written `FAILED`.
    ///
    /// # Why a campaign says `false`, and the bug that made it necessary
    ///
    /// `FAILED` is **terminal** in the machine of spec §14.3, deliberately: a
    /// delivery receipt arriving for a message already failed must not
    /// resurrect it. But spec §10.7 replays a throttled message, and a replay
    /// that succeeds has to record `ACCEPTED` — which the machine refuses over a
    /// `FAILED` row.
    ///
    /// The result was a campaign whose replay reached the message centre, was
    /// accepted, and left the journal saying `FAILED` for ever. The runner's
    /// counters and the database disagreed, which is exactly what CA-010-02 is
    /// checked against.
    ///
    /// So an attempt the caller may replay writes `SENT` instead — the state of
    /// a message whose `submit_sm` has left and whose verdict is not in — and
    /// the verdict is written by the attempt that is final. The condition is
    /// stated in [`Sender::final_transition`], and it is exactly the condition
    /// under which [`crate::retry::RetryPolicy::decide`] gives up, so the row is
    /// `FAILED` precisely when the campaign has stopped trying.
    pub last_attempt: bool,

    /// The campaign this message belongs to, when it belongs to one.
    ///
    /// Written onto the row, and it is not decoration: `messages.campaign_id` is
    /// what CA-010-02 counts against — "total = sent + failed + cancelled,
    /// checked against the contents of the database" is a query filtered by this
    /// column — and what the resume of spec §10.5 selects on.
    pub campaign_id: Option<CampaignId>,
}

impl SendRequest {
    /// A first attempt at a brand-new message.
    #[must_use]
    pub fn new(text: impl Into<String>, submit: SubmitOptions) -> Self {
        Self {
            client_message_id: ClientMessageId::new(),
            text: text.into(),
            submit,
            encoding: EncodingChoice::Automatic,
            mode: SegmentationMode::default(),
            attempt: 1,
            last_attempt: true,
            campaign_id: None,
        }
    }

    /// The same request, as an attempt the caller may replay.
    ///
    /// See [`Self::last_attempt`] for what it changes and why it exists.
    #[must_use]
    pub const fn with_more_attempts_allowed(mut self, allowed: bool) -> Self {
        self.last_attempt = !allowed;
        self
    }

    /// The same request, under the write-ahead key the caller chose.
    ///
    /// A campaign derives its keys rather than drawing them
    /// ([`crate::campaign::resume::message_key`]), which is what makes a resumed
    /// campaign find the row it already wrote instead of inserting a second one.
    #[must_use]
    pub const fn keyed(mut self, client_message_id: ClientMessageId) -> Self {
        self.client_message_id = client_message_id;
        self
    }

    /// The same request, as part of a campaign.
    #[must_use]
    pub const fn in_campaign(mut self, campaign_id: CampaignId) -> Self {
        self.campaign_id = Some(campaign_id);
        self
    }

    /// The same request under another encoding choice.
    #[must_use]
    pub fn with_encoding(mut self, encoding: EncodingChoice) -> Self {
        self.encoding = encoding;
        self
    }

    /// The same request under another concatenation mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: SegmentationMode) -> Self {
        self.mode = mode;
        self
    }

    /// The same request, marked as attempt number `attempt`.
    #[must_use]
    pub const fn as_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }
}

/// What became of one `submit_sm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentOutcome {
    /// The message centre answered, with this status and this identifier.
    ///
    /// `status` is `ESME_ROK` on the nominal path and something else on a
    /// rejection — an answered rejection is still an answer, which is why it
    /// is not [`Self::Unanswered`].
    Answered {
        /// The `command_status` the response carried.
        status: CommandStatus,
        /// The identifier the message centre assigned, when it did.
        smsc_message_id: Option<String>,
    },
    /// No response: a timeout, a closed session, a refused operation.
    Unanswered {
        /// Why.
        failure: SubmitError,
    },
    /// Never sent, because an earlier segment of the same message failed.
    ///
    /// Sending the tail of a message whose middle was rejected produces parts
    /// the handset can never reassemble, and spends quota on them.
    NotAttempted,
}

impl SegmentOutcome {
    /// Whether this segment reached the message centre and was accepted.
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Answered { status, .. } if *status == CommandStatus::EsmeRok)
    }

    /// The identifier the message centre assigned, when there is one.
    #[must_use]
    pub fn smsc_message_id(&self) -> Option<&str> {
        match self {
            Self::Answered {
                smsc_message_id, ..
            } => smsc_message_id.as_deref(),
            Self::Unanswered { .. } | Self::NotAttempted => None,
        }
    }
}

/// What one send produced, as the interface shows it.
///
/// A **value**, not an error, even when the message failed: a rejected
/// `submit_sm_resp` is a normal protocol outcome that the operator has to read
/// (ENF-UTI-02), not a fault of this application. Only a failure that prevented
/// the send from being attempted at all comes back as a [`MessagingError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReport {
    /// The write-ahead key, so the interface can follow the message.
    pub client_message_id: ClientMessageId,
    /// The session it went out on.
    pub session_id: SessionId,
    /// Where the message ended up: `ACCEPTED` or `FAILED`.
    pub state: MessageState,
    /// Segments the message was split into.
    pub segments: u32,
    /// The identifier of the **first** segment, when the whole message was
    /// accepted.
    ///
    /// `None` on a failure, **including a partial one** where some segments
    /// were accepted and carry an identifier of their own. See
    /// [`Sender::aggregate`] for why; the per-segment identifiers are still in
    /// [`Self::outcomes`], which is where an operator reads them.
    pub smsc_message_id: Option<String>,
    /// The status the whole message is reported under.
    ///
    /// `ESME_ROK` when every segment was accepted; otherwise the status of the
    /// first segment that was not.
    pub command_status: Option<CommandStatus>,
    /// Whether sending the same message again could succeed.
    ///
    /// Answered by [`crate::retry::SendFailure::is_retryable`], which reads the
    /// classification of milestone 003 for a refusal and names the failures that
    /// carry no status. **One** reading of that question exists in this crate,
    /// and this is a projection of it.
    ///
    /// The interface shows it — a "retry" button that offers itself on a fatal
    /// status is a button that sends the same rejection twice — and, since
    /// milestone 010, [`Sender`] reads it too: it is half of the condition under
    /// which a failed attempt is journalled as `SENT` rather than `FAILED`. See
    /// [`SendRequest::last_attempt`].
    pub retryable: bool,
    /// Whether the journal recorded the outcome.
    ///
    /// `false` means the message **was** submitted and the message centre
    /// answered, but the transitions could not be written: the row is still
    /// `QUEUED`, so a resume would send it again.
    ///
    /// It is a field of the report rather than an error because the two say
    /// opposite things. An error means "nothing was sent"; this means
    /// "everything was sent and the record is missing". Reporting the second
    /// as the first is how an operator resends a message that already went
    /// out.
    pub journalled: bool,
    /// One entry per segment, in order.
    pub outcomes: Vec<SegmentOutcome>,
}

impl SendReport {
    /// Whether every segment was accepted.
    #[must_use]
    pub fn is_accepted(&self) -> bool {
        self.state == MessageState::Accepted
    }
}

/// Watches a message walk its lifecycle, while it walks it.
///
/// CA-006-01 asks for the interface to show `QUEUED → SENT → ACCEPTED`, and a
/// command that only returns its final report cannot: the three states would
/// collapse into one repaint. So the orchestrator announces each transition as
/// it applies it, and `src-tauri` turns each announcement into a
/// `message:update` event.
///
/// Deliberately **not** `async` and expected not to block: it is called on the
/// send path, between two `.await`s, and an implementation that did I/O here
/// would pace the sending from the interface.
pub trait SendObserver: Send + Sync {
    /// The message has reached `state`.
    fn state_changed(&self, client_message_id: ClientMessageId, state: MessageState);
}

/// The observer that watches nothing, for a caller with no interface.
impl SendObserver for () {
    fn state_changed(&self, _client_message_id: ClientMessageId, _state: MessageState) {}
}

/// The send orchestrator.
///
/// Generic over its two ports and over the clock, which is what lets a test
/// drive the whole path with an in-memory journal, a scripted message centre
/// and a frozen clock (CLAUDE.md §7).
#[derive(Debug)]
pub struct Sender<R, C> {
    repository: R,
    clock: C,
    references: ConcatenationReferenceCounter,
}

impl<R, C> Sender<R, C>
where
    R: MessageRepository,
    C: Clock,
{
    /// Builds a sender over a journal and a clock.
    ///
    /// The concatenation reference counter starts at a random value: a
    /// deterministic start would hand a restarting process the reference of
    /// parts that may still be in flight, and the handset would merge two
    /// unrelated messages once, unreproducibly.
    #[must_use]
    pub fn new(repository: R, clock: C) -> Self {
        Self {
            repository,
            clock,
            references: ConcatenationReferenceCounter::random(),
        }
    }

    /// The same sender with a reference counter starting at a known value.
    ///
    /// For tests, and for a resumed session that knows where the previous one
    /// stopped.
    #[must_use]
    pub fn with_reference_counter(mut self, counter: ConcatenationReferenceCounter) -> Self {
        self.references = counter;
        self
    }

    /// The journal this sender writes to.
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    /// The clock this sender stamps its rows with.
    ///
    /// Exposed for the campaign runner, which has to answer "is the daily send
    /// window open right now" against the **same** clock the rows are stamped
    /// with. Two clocks would let a campaign wait on one and record on the
    /// other, and a test could only ever pin one of them.
    pub const fn clock(&self) -> &C {
        &self.clock
    }

    /// Sends one message and reports what became of it.
    ///
    /// # Errors
    ///
    /// * [`MessagingError::Encoding`] if the text cannot be written under the
    ///   chosen encoding, or splits into more than 255 segments;
    /// * [`MessagingError::Submit`] if a field of spec §7.3 does not fit;
    /// * [`MessagingError::Store`] if the journal refuses the write-ahead
    ///   insert — in which case **nothing was sent**.
    ///
    /// A message the centre rejected is **not** an error: it comes back as a
    /// [`SendReport`] in state [`MessageState::Failed`].
    pub async fn send<S: SmscSession>(
        &self,
        session: &S,
        request: &SendRequest,
    ) -> Result<SendReport, MessagingError> {
        self.send_observed(session, request, &()).await
    }

    /// The same send, announcing each transition to `observer` as it happens.
    ///
    /// # Errors
    ///
    /// Same as [`Self::send`].
    pub async fn send_observed<S: SmscSession, O: SendObserver + ?Sized>(
        &self,
        session: &S,
        request: &SendRequest,
        observer: &O,
    ) -> Result<SendReport, MessagingError> {
        self.dispatch(session, request, observer, WriteAhead::Insert)
            .await
    }

    /// Sends a message whose write-ahead row is **already** in the journal.
    ///
    /// The one difference with [`Self::send`] is the insert, which is skipped.
    /// Everything else — the encoding, the segmentation, the transitions, the
    /// report — is the same path, deliberately: two send paths would drift, and
    /// the one that drifts is always the one that runs on the day of the
    /// incident.
    ///
    /// # When this is the right call, and when it is not
    ///
    /// Only when something has **established** that the row exists and has not
    /// been accepted. There is exactly one such thing in this crate,
    /// [`crate::campaign::resume::EmissionGuard`], and calling this without it
    /// is how a message already accepted goes out a second time (CA-010-05).
    /// The two cases it serves:
    ///
    /// * a **retry** within one run (spec §10.7): the row was inserted by the
    ///   first attempt, and inserting again would conflict;
    /// * a **resume** after a restart: the row was written by the process that
    ///   died.
    ///
    /// # Errors
    ///
    /// Same as [`Self::send`], minus [`MessagingError::Store`] on the insert,
    /// which does not happen.
    pub async fn resend<S: SmscSession>(
        &self,
        session: &S,
        request: &SendRequest,
    ) -> Result<SendReport, MessagingError> {
        self.resend_observed(session, request, &()).await
    }

    /// The same resend, announcing each transition to `observer`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::resend`].
    pub async fn resend_observed<S: SmscSession, O: SendObserver + ?Sized>(
        &self,
        session: &S,
        request: &SendRequest,
        observer: &O,
    ) -> Result<SendReport, MessagingError> {
        self.dispatch(session, request, observer, WriteAhead::AlreadyWritten)
            .await
    }

    #[tracing::instrument(
        skip_all,
        fields(
            client_message_id = %request.client_message_id,
            session_id = %session.session_id(),
            attempt = request.attempt,
        )
    )]
    async fn dispatch<S: SmscSession, O: SendObserver + ?Sized>(
        &self,
        session: &S,
        request: &SendRequest,
        observer: &O,
        write_ahead: WriteAhead,
    ) -> Result<SendReport, MessagingError> {
        // --- 1. Encode and split, under the session's own conventions -------
        //
        // ADR 0008 and ADR 0009: the packing and the charset belong to the
        // message centre, not to the message. Milestone 005 put them on the
        // profile and milestone 004 made the segmenter take them; this line is
        // the wire between the two, and without it a `Latin1` session would
        // silently send GSM 03.38 positions.
        let options = SegmentationOptions::default()
            .with_encoding(request.encoding)
            .with_mode(request.mode)
            .with_gsm_packing(session.gsm7_packing())
            .with_gsm_charset(session.gsm7_charset());

        let split = segment(&request.text, &options, self.references.next())?;

        // --- 2. Build every PDU, still before touching the journal ----------
        //
        // A `service_type` that does not fit its field must not leave a
        // `QUEUED` row behind either (CA-006-07), and the only way to know is
        // to build.
        let pdus = split
            .segments()
            .iter()
            .map(|part| build_submit_sm(&request.submit, part))
            .collect::<Result<Vec<_>, _>>()?;

        let total = u32::try_from(pdus.len()).unwrap_or(u32::MAX);

        // --- 3. Write ahead -------------------------------------------------
        //
        // Skipped, and only skipped, when the caller has established that the
        // row is already there — a retry or a resume. See `Self::resend`.
        if write_ahead == WriteAhead::Insert {
            let created_at = self.clock.now();
            let queued = self.queued_row(request, session.session_id(), &split, total, created_at);

            self.repository.insert_message(&queued).await?;

            tracing::debug!(segments = total, "message persisted before submission");

            observer.state_changed(request.client_message_id, MessageState::Queued);
        }

        // --- 4. Submit, correlating each response with its own request ------
        let sent_at = self.clock.now();

        // Announced before the first PDU leaves, and recorded only after the
        // last response: the interface follows the message, the journal
        // records what actually happened. Conflating the two would mean
        // writing `SENT` before the socket, which is the trade the module
        // header rules out.
        observer.state_changed(request.client_message_id, MessageState::Sent);

        let Submission { outcomes, emitted } = submit_all(session, pdus).await;
        let responded_at = self.clock.now();

        // --- 5. Record what happened ----------------------------------------
        let mut report = self.aggregate(request, session.session_id(), total, outcomes);

        // `SENT` is a claim about the wire, so it is only written when
        // something actually reached it. A submission the session refused —
        // a receiver bind, a session that went down between the insert and
        // here — leaves the row `QUEUED` with no `sent_at` and no attempt
        // consumed, which is the truth and what spec §10.7 budgets against.
        let mut transitions = Vec::with_capacity(2);

        if emitted {
            transitions.push(
                MessageStateUpdate::new(request.client_message_id, MessageState::Sent)
                    .sent_at(sent_at, request.attempt),
            );
        }

        transitions.push(self.final_transition(request, &report, responded_at));

        // ONE transaction: the transitions are a single fact about a single
        // message, and a reader must never see `SENT` for a message whose
        // response has already been read.
        //
        // A FAILURE HERE IS NOT THE FAILURE OF THE INSERT, and must not be
        // reported as one. By this point the `submit_sm` have left and the
        // message centre has answered; the send happened. Propagating the
        // error would throw the report away — the `smsc_message_id` with it —
        // and hand the caller a code whose whole meaning is "nothing was
        // sent". The operator would resend, and the message would go out
        // twice.
        //
        // So the failure is logged with its full context and carried on the
        // report instead: the send is reported as what it was, plus the fact
        // that the journal does not know about it.
        if let Err(error) = self.repository.update_states(&transitions).await {
            tracing::error!(
                error = ?error,
                state = %report.state,
                segments = total,
                smsc_message_id = ?report.smsc_message_id,
                "the message was submitted but its transitions could not be journalled; \
                 the row stays QUEUED and a resume would send it again"
            );

            report.journalled = false;
        }

        observer.state_changed(request.client_message_id, report.state);

        tracing::info!(
            state = %report.state,
            segments = total,
            retryable = report.retryable,
            journalled = report.journalled,
            "message submitted"
        );

        Ok(report)
    }

    /// The write-ahead row.
    ///
    /// `attempts` is zero here on purpose: no `submit_sm` has left, so no
    /// attempt has been made. The number arrives with the `SENT` transition,
    /// which is the one that says a PDU went out.
    fn queued_row(
        &self,
        request: &SendRequest,
        session_id: SessionId,
        split: &crate::segmentation::SegmentedMessage,
        segments: u32,
        created_at: Timestamp,
    ) -> Message {
        let submit = &request.submit;

        Message {
            client_message_id: request.client_message_id,
            campaign_id: request.campaign_id,
            session_id: Some(session_id),
            smsc_message_id: None,
            source_addr: submit
                .source
                .as_ref()
                .map(|source| source.as_str().to_owned()),
            source_ton: submit
                .source
                .as_ref()
                .map(crate::addressing::SourceAddress::ton),
            source_npi: submit
                .source
                .as_ref()
                .map(crate::addressing::SourceAddress::npi),
            dest_addr: Some(submit.destination.number().clone()),
            dest_ton: Some(submit.destination.ton()),
            dest_npi: Some(submit.destination.npi()),
            data_coding: Some(split.data_coding()),
            segments,
            text: Some(request.text.clone()),
            state: MessageState::Queued,
            command_status: None,
            dlr_stat: None,
            dlr_err: None,
            attempts: 0,
            created_at,
            sent_at: None,
            resp_at: None,
            dlr_at: None,
        }
    }

    /// Folds the per-segment outcomes into one message-level verdict.
    ///
    /// # The partial-failure decision (fiche §6)
    ///
    /// Two segments accepted and one rejected makes the message **`FAILED`**,
    /// not `ACCEPTED` and not a fourth state.
    ///
    /// The reason is what the recipient sees: a concatenated message is
    /// reassembled from *all* its parts, and a handset holding two of three
    /// displays nothing until the third arrives or its reassembly timer
    /// expires. The message the operator wrote was not delivered, so counting
    /// it as accepted would inflate every figure milestone 014 draws.
    ///
    /// A `PARTIAL` state was considered and rejected: `messages.state` carries
    /// a `CHECK` constraint listing the six states of spec §14.3, so a seventh
    /// is a migration and a change to every screen that groups by state — for
    /// a distinction the operator can already read, segment by segment, in
    /// [`SendReport::outcomes`].
    ///
    /// # And its `smsc_message_id` is dropped
    ///
    /// A partially failed message keeps **no** `smsc_message_id`, even though
    /// its accepted segments were each assigned one.
    ///
    /// Storing the first segment's identifier on a `FAILED` row would arm a
    /// bug three milestones out. The message centre will try to deliver that
    /// fragment and will send a receipt for it; milestone 008 correlates a
    /// receipt by looking the identifier up
    /// ([`crate::ports::MessageRepository::find_message_by_smsc_id`]), would
    /// find this row, and would move it `FAILED → DELIVERED`. A message the
    /// recipient never saw would be counted as delivered.
    ///
    /// The identifiers are not lost: each is on its own
    /// [`SegmentOutcome`], which is what the interface shows and what an
    /// operator quotes to their provider.
    ///
    /// What this does **not** do is undo the accepted fragment. Nothing
    /// cancels it and no row records it, so re-sending the message produces a
    /// second copy of that segment at the message centre. Cancelling a
    /// submitted segment needs `cancel_sm`, which no milestone has scoped;
    /// the CHANGELOG says so rather than leaving it implied.
    fn aggregate(
        &self,
        request: &SendRequest,
        session_id: SessionId,
        segments: u32,
        outcomes: Vec<SegmentOutcome>,
    ) -> SendReport {
        let first_failure = outcomes.iter().find(|outcome| !outcome.is_accepted());

        let command_status = match first_failure {
            None => Some(CommandStatus::EsmeRok),
            Some(SegmentOutcome::Answered { status, .. }) => Some(*status),
            // No response carries no `command_status`, and inventing one would
            // put a status the message centre never sent into the journal.
            Some(SegmentOutcome::Unanswered { .. } | SegmentOutcome::NotAttempted) => None,
        };

        let state = if first_failure.is_none() {
            MessageState::Accepted
        } else {
            MessageState::Failed
        };

        // Asked of `SendFailure` rather than answered again here.
        //
        // It used to be a second reading — the status classification for a
        // refusal, and a hand-written `ResponseTimeout | Closed` for the rest —
        // and the two disagreed: `SubmitError::Transport` and `NotBound` are
        // replayed by the policy of spec §10.7 and were reported here as final.
        // That mattered once `final_transition` started asking this field which
        // state to write, since it made the row terminal for a message the
        // campaign was about to replay.
        let retryable = match first_failure {
            None => false,
            Some(SegmentOutcome::Answered { status, .. }) => {
                SendFailure::Rejected(*status).is_retryable()
            }
            Some(SegmentOutcome::Unanswered { failure }) => {
                SendFailure::NoResponse(failure.clone()).is_retryable()
            }
            // Unreachable: `NotAttempted` only ever follows a failure, so it is
            // never the *first* one.
            Some(SegmentOutcome::NotAttempted) => false,
        };

        // Only on a whole message. See the note above.
        let smsc_message_id = (state == MessageState::Accepted)
            .then(|| outcomes.first().and_then(SegmentOutcome::smsc_message_id))
            .flatten()
            .map(ToOwned::to_owned);

        SendReport {
            client_message_id: request.client_message_id,
            session_id,
            state,
            segments,
            smsc_message_id,
            command_status,
            retryable,
            journalled: true,
            outcomes,
        }
    }

    /// The transition that closes the send.
    ///
    /// # The state written is not always the state reported
    ///
    /// [`SendReport::state`] is about **this attempt**: it says `FAILED` the
    /// moment a segment was not accepted, which is what the caller has to read
    /// to decide whether to replay.
    ///
    /// The **row** is another matter. `FAILED` is terminal, so writing it ends
    /// the message: a later replay that the message centre accepts could not be
    /// recorded, and the journal would say `FAILED` for a message that went out.
    /// So a failure the caller may replay — `last_attempt == false` and a
    /// classification that says trying again may work — is written `SENT`, the
    /// state of a message whose `submit_sm` has left and whose verdict is not
    /// in, and the `command_status` of the refusal is recorded beside it.
    ///
    /// The two halves of the condition are exactly the two ways
    /// [`crate::retry::RetryPolicy::decide`] gives up, so the row reaches
    /// `FAILED` on the attempt the campaign stops at, and on no other.
    fn final_transition(
        &self,
        request: &SendRequest,
        report: &SendReport,
        responded_at: Timestamp,
    ) -> MessageStateUpdate {
        let state =
            if report.state == MessageState::Failed && !request.last_attempt && report.retryable {
                MessageState::Sent
            } else {
                report.state
            };

        let mut update =
            MessageStateUpdate::new(report.client_message_id, state).responded_at(responded_at);

        if let Some(status) = report.command_status {
            update = update.with_command_status(status);
        }

        if let Some(identifier) = report.smsc_message_id.as_deref() {
            update = update.with_smsc_message_id(identifier);
        }

        update
    }
}

/// Whether the send path writes the journal row or expects to find one.
///
/// A two-arm enum rather than a `bool`, because `send(session, request, false)`
/// at a call site says nothing about which half of the write-ahead contract is
/// being suspended — and this is the contract of CLAUDE.md §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteAhead {
    /// Persist before submitting. The ordinary path.
    Insert,
    /// The row is already there: a retry, or a resume.
    AlreadyWritten,
}

/// What one pass over the segments produced.
struct Submission {
    /// One entry per segment, in order.
    outcomes: Vec<SegmentOutcome>,
    /// Whether at least one `submit_sm` actually reached the socket.
    ///
    /// A flag returned from here rather than re-derived from `outcomes` by the
    /// caller: this function is the only place that knows, per segment,
    /// whether the port refused before writing or after. Reconstructing it
    /// from the outcomes would mean restating [`SubmitError::prevented_emission`]
    /// somewhere it could drift.
    emitted: bool,
}

/// Submits every segment in order, stopping at the first that does not land.
///
/// Sequential, and that is the milestone's scope: windowing and rate control
/// are milestone 007's. What is **not** sequential by accident is the
/// correlation — [`SmscSession::submit`] allocates the `sequence_number` and
/// waits for the response carrying it, so a response is matched to its own
/// request rather than to whichever arrives next.
async fn submit_all<S: SmscSession>(
    session: &S,
    pdus: Vec<smpp_core::pdus::SubmitSm>,
) -> Submission {
    let total = pdus.len();
    let mut outcomes = Vec::with_capacity(total);
    let mut emitted = false;

    for (index, pdu) in pdus.into_iter().enumerate() {
        let outcome = match session.submit(Pdu::SubmitSm(pdu)).await {
            Ok(response) => {
                emitted = true;

                SegmentOutcome::Answered {
                    status: response.status(),
                    smsc_message_id: submitted_identifier(&response),
                }
            }
            Err(failure) => {
                emitted |= !failure.prevented_emission();

                SegmentOutcome::Unanswered { failure }
            }
        };

        let landed = outcome.is_accepted();
        outcomes.push(outcome);

        if !landed {
            tracing::warn!(
                segment = index + 1,
                total,
                "segment not accepted; the remaining segments are not sent"
            );

            outcomes.resize(total, SegmentOutcome::NotAttempted);
            break;
        }
    }

    Submission { outcomes, emitted }
}

/// The `message_id` a `submit_sm_resp` carried, when it carried one.
///
/// A rejected response has an empty `message_id`, and an empty identifier
/// stored would be indistinguishable from one the message centre really
/// assigned — and would then match the wrong message when milestone 008 looks
/// a receipt up by it.
fn submitted_identifier(response: &smpp_core::codec::Command) -> Option<String> {
    match response.pdu() {
        Some(Pdu::SubmitSmResp(body)) => {
            let identifier = body.message_id().as_str();

            (!identifier.is_empty()).then(|| identifier.to_owned())
        }
        _ => None,
    }
}

/// Whether a status asks the sender to slow down before any replay.
///
/// Exposed because milestone 007 is what acts on it: this milestone reads the
/// classification into [`SendReport::retryable`] and does **not** loop, which
/// is the one thing a `Fatal` status forbids outright.
#[must_use]
pub fn requires_slowdown(status: CommandStatus) -> bool {
    status_codes::classify(status) == StatusClass::Throttling
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{SendRequest, Sender};
    use crate::addressing::Destination;
    use crate::message::{MessageState, MessageStateUpdate};
    use crate::ports::{MessageRepository as _, MessageStoreError};
    use crate::submit::SubmitOptions;
    use crate::testing::{journal_row, FakeSmsc, FixedClock, MemoryJournal, Reply};
    use crate::MessagingError;
    use smpp_core::types::{CampaignId, ClientMessageId};
    use smpp_core::values::CommandStatus;

    fn campaign() -> CampaignId {
        CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000001").expect("a valid UUID")
    }

    fn request() -> SendRequest {
        SendRequest::new(
            "Bonjour",
            SubmitOptions::to(
                Destination::parse("+2250700000001").expect("the fixture is a valid number"),
            ),
        )
    }

    /// CA-010-02 is checked against the database, and a campaign message with a
    /// null `campaign_id` is invisible to the query that checks it.
    #[tokio::test]
    async fn a_campaign_message_is_persisted_under_its_campaign() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let request = request().in_campaign(campaign());

        sender
            .send(&FakeSmsc::accepting(), &request)
            .await
            .expect("the send succeeds");

        let row = journal
            .row(request.client_message_id)
            .await
            .expect("the message was persisted");

        assert_eq!(row.campaign_id, Some(campaign()));
    }

    #[tokio::test]
    async fn a_unit_message_belongs_to_no_campaign() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let request = request();

        sender
            .send(&FakeSmsc::accepting(), &request)
            .await
            .expect("the send succeeds");

        let row = journal
            .row(request.client_message_id)
            .await
            .expect("the message was persisted");

        assert_eq!(row.campaign_id, None);
    }

    /// The write-ahead key is the caller's, so a resumed campaign re-derives the
    /// one it used before instead of minting a second identifier for the same
    /// recipient.
    #[tokio::test]
    async fn the_write_ahead_key_may_be_chosen_by_the_caller() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let chosen = ClientMessageId::new();

        sender
            .send(&FakeSmsc::accepting(), &request().keyed(chosen))
            .await
            .expect("the send succeeds");

        assert!(journal.row(chosen).await.is_some());
    }

    /// The primary guard of the emission invariant: a second write-ahead insert
    /// under the same key is refused, and **nothing is submitted**.
    #[tokio::test]
    async fn a_second_send_under_the_same_key_is_refused_before_anything_is_sent() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting();
        let request = request();

        sender.send(&smsc, &request).await.expect("the first send");

        let refusal = sender
            .send(&smsc, &request)
            .await
            .expect_err("the second send conflicts");

        assert!(matches!(
            refusal,
            MessagingError::Store(MessageStoreError::Conflict)
        ));
        assert_eq!(smsc.submitted(), 1, "nothing was sent for the second call");
        assert_eq!(journal.inserted().await, 1);
    }

    /// A resend writes no row: the retry of spec §10.7 and the resume of §10.5
    /// both reuse the one that is already there, which is what keeps the number
    /// of distinct `client_message_id`s equal to the number of recipients
    /// (CA-010-04).
    #[tokio::test]
    async fn a_resend_reuses_the_row_instead_of_writing_a_second_one() {
        let journal = MemoryJournal::new();
        let identifier = ClientMessageId::new();

        journal
            .force_row(journal_row(identifier, MessageState::Queued))
            .await;

        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting();

        let report = sender
            .resend(&smsc, &request().keyed(identifier).as_attempt(2))
            .await
            .expect("the resend succeeds");

        assert!(report.is_accepted());
        assert_eq!(smsc.submitted(), 1);
        assert_eq!(journal.inserted().await, 0, "no second row");

        let row = journal
            .row(identifier)
            .await
            .expect("the row is still there");

        assert_eq!(row.state, MessageState::Accepted);
        assert_eq!(row.attempts, 2);
    }

    /// The two paths differ by the insert and by nothing else.
    #[tokio::test]
    async fn a_resend_journals_its_outcome_exactly_as_a_send_does() {
        let journal = MemoryJournal::new();
        let identifier = ClientMessageId::new();

        journal
            .force_row(journal_row(identifier, MessageState::Queued))
            .await;

        let sender = Sender::new(journal.clone(), FixedClock::default());

        sender
            .resend(&FakeSmsc::accepting(), &request().keyed(identifier))
            .await
            .expect("the resend succeeds");

        let row = journal.row(identifier).await.expect("the row is there");

        assert!(row.sent_at.is_some());
        assert!(row.resp_at.is_some());
        assert!(row.smsc_message_id.is_some());
    }

    /// A resend of a message the journal does not hold is a caller mistake, and
    /// it must surface rather than write a row nobody asked for: the transitions
    /// come back `NotFound`, and the report says the send was not journalled.
    #[tokio::test]
    async fn a_resend_of_an_unknown_message_reports_that_it_was_not_journalled() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());

        let report = sender
            .resend(&FakeSmsc::accepting(), &request())
            .await
            .expect("the submission itself succeeds");

        assert!(!report.journalled);
        assert_eq!(journal.inserted().await, 0);
    }

    /// A campaign that will replay this attempt must not have it journalled
    /// `FAILED`: that state is terminal, and the replay's acceptance could then
    /// never be recorded. See [`SendRequest::last_attempt`].
    #[tokio::test]
    async fn a_replayable_failure_is_journalled_as_sent_rather_than_failed() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRthrottled)]);
        let request = request().with_more_attempts_allowed(true);

        let report = sender
            .send(&smsc, &request)
            .await
            .expect("the send happens");

        assert_eq!(report.state, MessageState::Failed, "the attempt failed");
        assert!(report.retryable);

        let row = journal
            .row(request.client_message_id)
            .await
            .expect("the row is there");

        assert_eq!(row.state, MessageState::Sent, "the verdict is not in yet");
        assert_eq!(row.command_status, Some(CommandStatus::EsmeRthrottled));
    }

    /// The other half of the condition: a failure nothing can replay is written
    /// down at once, however many attempts the caller has left.
    #[tokio::test]
    async fn a_fatal_failure_is_journalled_failed_even_with_attempts_left() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRinvdstadr)]);
        let request = request().with_more_attempts_allowed(true);

        sender
            .send(&smsc, &request)
            .await
            .expect("the send happens");

        assert_eq!(
            journal
                .row(request.client_message_id)
                .await
                .expect("the row is there")
                .state,
            MessageState::Failed
        );
    }

    /// A unit send has no replay policy, so its failure is the message's.
    #[tokio::test]
    async fn a_unit_send_journals_its_failure_at_once() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::scripted([Reply::Rejected(CommandStatus::EsmeRthrottled)]);
        let request = request();

        assert!(request.last_attempt, "a unit send is its own last attempt");

        sender
            .send(&smsc, &request)
            .await
            .expect("the send happens");

        assert_eq!(
            journal
                .row(request.client_message_id)
                .await
                .expect("the row is there")
                .state,
            MessageState::Failed
        );
    }

    /// The state machine is the last barrier under the guard of CA-010-05: even
    /// reached by something that bypassed the guard, an `ACCEPTED` row is not
    /// walked back to `SENT`.
    #[tokio::test]
    async fn an_accepted_row_is_never_walked_back_by_a_later_transition() {
        let journal = MemoryJournal::new();
        let identifier = ClientMessageId::new();

        journal
            .force_row(journal_row(identifier, MessageState::Accepted))
            .await;

        let written = journal
            .update_states(&[MessageStateUpdate::new(identifier, MessageState::Sent)])
            .await
            .expect("the journal answers");

        assert_eq!(written, 0);
        assert_eq!(
            journal
                .row(identifier)
                .await
                .expect("the row is there")
                .state,
            MessageState::Accepted
        );
    }
}
