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
//! The residual window is stated rather than glossed over. A crash **after**
//! the last `submit_sm` left and before the transitions commit leaves the row
//! `QUEUED` for a message the message centre did accept, so a resume would
//! send it twice. Closing that would mean writing `SENT` before the socket,
//! which is the other trade — no duplicate, but a message lost whenever the
//! write succeeds and the send does not. ENF-FIA-01 asks for "no message
//! lost", so this is the side the ordering falls on.
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
use smpp_core::types::{ClientMessageId, SessionId};
use smpp_core::values::CommandStatus;

use crate::encoding::EncodingChoice;
use crate::error::MessagingError;
use crate::message::{Message, MessageState, MessageStateUpdate};
use crate::ports::{MessageRepository, SmscSession, SubmitError};
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
        }
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
    /// The identifier of the **first** segment, the one an operator quotes.
    pub smsc_message_id: Option<String>,
    /// The status the whole message is reported under.
    ///
    /// `ESME_ROK` when every segment was accepted; otherwise the status of the
    /// first segment that was not.
    pub command_status: Option<CommandStatus>,
    /// Whether sending the same message again could succeed.
    ///
    /// Read from the classification of milestone 003: `ESME_RTHROTTLED` and a
    /// system error say yes, an invalid destination says no. Nothing here acts
    /// on it — the retry policy is milestone 010's and the pacing milestone
    /// 007's — but the interface shows it, and a "retry" button that offers
    /// itself on a fatal status is a button that sends the same rejection
    /// twice.
    pub retryable: bool,
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
    #[tracing::instrument(
        skip_all,
        fields(
            client_message_id = %request.client_message_id,
            session_id = %session.session_id(),
            attempt = request.attempt,
        )
    )]
    pub async fn send_observed<S: SmscSession, O: SendObserver + ?Sized>(
        &self,
        session: &S,
        request: &SendRequest,
        observer: &O,
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
        let created_at = self.clock.now();
        let queued = self.queued_row(request, session.session_id(), &split, total, created_at);

        self.repository.insert_message(&queued).await?;

        tracing::debug!(segments = total, "message persisted before submission");

        observer.state_changed(request.client_message_id, MessageState::Queued);

        // --- 4. Submit, correlating each response with its own request ------
        let sent_at = self.clock.now();

        // Announced before the first PDU leaves, and recorded only after the
        // last response: the interface follows the message, the journal
        // records what actually happened. Conflating the two would mean
        // writing `SENT` before the socket, which is the trade the module
        // header rules out.
        observer.state_changed(request.client_message_id, MessageState::Sent);

        let outcomes = submit_all(session, pdus).await;
        let responded_at = self.clock.now();

        // --- 5. Record what happened ----------------------------------------
        let report = self.aggregate(request, session.session_id(), total, outcomes);

        let transitions = [
            MessageStateUpdate::new(request.client_message_id, MessageState::Sent)
                .sent_at(sent_at, request.attempt),
            self.final_transition(&report, responded_at),
        ];

        // ONE transaction: the two transitions are a single fact about a
        // single message, and a reader must never see `SENT` for a message
        // whose response has already been read.
        self.repository.update_states(&transitions).await?;

        observer.state_changed(request.client_message_id, report.state);

        tracing::info!(
            state = %report.state,
            segments = total,
            retryable = report.retryable,
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
            campaign_id: None,
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

        // A missing response is retryable in the same sense a `Recoverable`
        // status is: nothing says the message was refused, only that the
        // answer did not arrive (spec §10.7).
        let retryable = match command_status {
            Some(CommandStatus::EsmeRok) => false,
            Some(status) => status_codes::classify(status).is_retryable(),
            None => matches!(
                first_failure,
                Some(SegmentOutcome::Unanswered {
                    failure: SubmitError::ResponseTimeout | SubmitError::Closed,
                })
            ),
        };

        SendReport {
            client_message_id: request.client_message_id,
            session_id,
            state,
            segments,
            smsc_message_id: outcomes
                .first()
                .and_then(SegmentOutcome::smsc_message_id)
                .map(ToOwned::to_owned),
            command_status,
            retryable,
            outcomes,
        }
    }

    /// The transition that closes the send.
    fn final_transition(&self, report: &SendReport, responded_at: Timestamp) -> MessageStateUpdate {
        let mut update = MessageStateUpdate::new(report.client_message_id, report.state)
            .responded_at(responded_at);

        if let Some(status) = report.command_status {
            update = update.with_command_status(status);
        }

        if let Some(identifier) = report.smsc_message_id.as_deref() {
            update = update.with_smsc_message_id(identifier);
        }

        update
    }
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
) -> Vec<SegmentOutcome> {
    let total = pdus.len();
    let mut outcomes = Vec::with_capacity(total);

    for (index, pdu) in pdus.into_iter().enumerate() {
        let outcome = match session.submit(Pdu::SubmitSm(pdu)).await {
            Ok(response) => SegmentOutcome::Answered {
                status: response.status(),
                smsc_message_id: submitted_identifier(&response),
            },
            Err(failure) => SegmentOutcome::Unanswered { failure },
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

    outcomes
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
