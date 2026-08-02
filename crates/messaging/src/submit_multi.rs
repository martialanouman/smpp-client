//! `submit_multi` and the fallback onto `submit_sm` (deliverable L-010-06).
//!
//! Spec §10.6: recipients sharing the same content go out in one PDU — up to
//! [`MAX_DESTINATIONS`] of them — "when the SMSC supports it. Otherwise,
//! automatic fallback onto individual `submit_sm`".
//!
//! ```text
//!   Batch (N recipients, one body)
//!     │
//!     ├─ N rows QUEUED  ─┐
//!     ├─ N transitions SENT  ├─ committed BEFORE the socket
//!     │                  ─┘
//!     ▼
//!   submit_multi ──► submit_multi_resp ──► one verdict PER RECIPIENT
//!     │
//!     └─ ESME_RINVCMDID / generic_nack ──► N × submit_sm, same rows
//! ```
//!
//! # `submit_multi_resp` is neither a success nor a failure
//!
//! It carries **one** `message_id` and a list of the recipients the message
//! centre refused, each with its own `error_status_code` (spec §7.4.2). A batch
//! of 254 comes back as "251 taken, three refused, for three different
//! reasons", and every consumer downstream needs that split: the journal writes
//! one row per recipient, the campaign counters partition the recipients, and a
//! message reported accepted when it was refused is a message nobody will ever
//! chase.
//!
//! So nothing here produces a batch-level verdict. [`RecipientOutcome`] is
//! per recipient, [`BatchReport::recipients`] has exactly one entry per
//! recipient of the batch — which is CA-010-08's "without losing a recipient" —
//! and the refusals are attributed by [`match_refusals`], which refuses to
//! guess.
//!
//! # THE CONSEQUENCE THIS MILESTONE DOES NOT SETTLE — delivery receipts
//!
//! **A message sent in a batch cannot be correlated with its delivery
//! receipt.**
//!
//! Milestone 008 correlates a receipt by looking its identifier up in the
//! journal: [`MessageRepository::find_message_by_smsc_id`] against the
//! `messages.smsc_message_id` index. That rests on one identifier per message.
//! `submit_multi` returns **one identifier for N messages**, and SMPP offers no
//! per-destination identifier in the response.
//!
//! Writing the shared identifier on all N rows was considered and is **not**
//! done, because it is worse than not correlating:
//!
//! * the lookup returns one arbitrary row — whichever the index yields first —
//!   so the receipt for recipient 200 credits recipient 1;
//! * the first receipt moves that row `ACCEPTED → DELIVERED` and the remaining
//!   N−1 are refused by the state machine or land on the same row;
//! * a `stat:UNDELIV` for one recipient would fail a message another recipient
//!   received. Silently, deterministically, and only visible in the delivery
//!   figures weeks later.
//!
//! `crate::sender::Sender::aggregate` already made the same call for a
//! partially failed multi-segment message, for the same reason and in the same
//! direction.
//!
//! So the rows of a batch carry **no** `smsc_message_id`, the identifier is
//! reported on [`BatchReport::smsc_message_id`] for the operator and the logs,
//! and the receipts of a batched message land in the orphan journal of
//! milestone 008 with [`OrphanReason::UnknownIdentifier`](crate::correlation::OrphanReason).
//! Nothing is lost, and nothing is misattributed.
//!
//! **This is a product arbitration, and it is not one this module may settle
//! alone.** With `registered_delivery = 1` — the default of
//! [`SubmitOptions::to`] — a campaign that batches asks for a receipt per
//! recipient and can correlate none of them: it trades PDU count for delivery
//! reporting. The three ways out all cost something outside this file (a
//! per-message identifier table and a schema change, a `query_sm` sweep, or
//! simply not batching when receipts are wanted), and choosing between them is
//! the maintainer's. Until then the behaviour is the conservative one and the
//! CHANGELOG records the limitation.
//!
//! # The fallback rule
//!
//! Stated in full on [`MultiResponse`], with the reasoning for each row. In
//! one sentence: **only a refusal of the operation itself falls back** —
//! `ESME_RINVCMDID` or a `generic_nack`. A throttling status does not, because
//! answering "slow down" with 254 PDUs is the opposite of what was asked; and
//! an unanswered `submit_multi` does not, because it may have been taken for
//! everybody and an in-run fallback would deliver 254 second copies without the
//! rows ever passing through the resume where ADR 0014 arbitrates and where the
//! duplicate is **counted**.
//!
//! # What an uncertain batch leaves in the journal, exactly
//!
//! It depends on [`Batch::last_attempt`], and the difference is not cosmetic —
//! it decides whether ADR 0014's arbitration ever runs for these rows:
//!
//! | `last_attempt` | Row | A resume |
//! |---|---|---|
//! | `false` — the caller will try again | `SENT`, no `command_status` | arbitrates it (ADR 0014's third line) |
//! | `true` — this verdict is final | `FAILED` | never reads it; `FAILED` is terminal |
//!
//! The second row is the **default** of [`Batch::new`], and it is the same rule
//! [`crate::sender::Sender`] applies to a unit send: the attempt nobody will
//! replay is the one that writes the terminal state, or a message the campaign
//! has given up on would stay non-terminal for ever. The consequence to be
//! aware of is that a batch sent with `last_attempt` and never answered writes
//! up to 254 `FAILED` rows for messages the centre may have delivered — visible
//! in the journal as failures, never re-sent, and never arbitrated. A caller
//! that wants the arbitration must say it has attempts left, which is what
//! [`Batch::with_more_attempts_allowed`] is for.
//!
//! # Why the fallback cannot duplicate
//!
//! ADR 0014's table has four rows; the fallback fires on the second one only.
//! `ESME_RINVCMDID` and `generic_nack` are *answers*, and what they answer is
//! "I did not take this PDU" — for anybody, since the operation itself was
//! refused. Re-sending is then exactly the replay spec §10.7 prescribes for a
//! refused message, and [`Sender::resend`] is the same path a retry takes.
//!
//! # Write-ahead, per recipient
//!
//! CLAUDE.md §4 applies to messages, not to PDUs: a batch of 254 is 254 rows
//! and 254 `SENT` transitions, all committed before the single `submit_multi`
//! reaches the socket. One row per batch would leave 253 recipients invisible
//! to the resume of spec §10.5 — the exact defect sub-PR B found in
//! [`crate::sender`] and fixed, reintroduced at 254× the scale.
//!
//! # What this module is not wired into yet
//!
//! [`CampaignRunner`](crate::campaign::runner::CampaignRunner) still emits one
//! `submit_sm` at a time. Batching a campaign means grouping its recipients by
//! rendered body — a template with variables has no shared body at all — and
//! reshaping the runner's per-message retry loop into a per-batch one, which
//! is a change to the emission loop the fiche puts outside this sub-milestone.
//! Everything CA-010-08 describes is implemented and tested here; what is not
//! yet true is that a campaign reaches it.

use core::sync::atomic::{AtomicBool, Ordering};

use smpp_core::codec::{Command, Pdu};
use smpp_core::pdus::SubmitMulti;
use smpp_core::time::{Clock, Timestamp};
use smpp_core::types::{CampaignId, ClientMessageId, Msisdn, SessionId};
use smpp_core::values::{CommandId, CommandStatus, DestAddress, SmeAddress};

use crate::addressing::Destination;
use crate::campaign::resume::{Admission, EmissionGuard, UnansweredPolicy};
use crate::encoding::EncodingChoice;
use crate::error::MessagingError;
use crate::message::{MessageState, MessageStateUpdate};
use crate::ports::{MessageRepository, MessageStoreError, SmscSession, SubmitError};
use crate::retry::SendFailure;
use crate::segmentation::{
    segment, ConcatenationReference, Segment, SegmentationMode, SegmentationOptions,
    SegmentedMessage,
};
use crate::sender::{SendReport, SendRequest, Sender};
use crate::submit::{CommonFields, SubmitBuildError, SubmitOptions};

/// Most recipients one `submit_multi` carries.
///
/// Spec §10.6 and the milestone fiche both say "~254". The protocol field is
/// wider — `number_of_dests` is an octet, so 255 would encode — and the tilde
/// is the reason this constant is the conservative end of it: the extra
/// recipient buys 0.4 % of a PDU and costs a batch at every message centre that
/// reads the last slot as reserved.
///
/// The ceiling is **enforced**, never clamped. `rusmpp` fills `number_of_dests`
/// with `dest_address.len() as u8`, so a 256-recipient vector announces zero
/// destinations and the whole batch disappears silently — which is the one
/// outcome CA-010-08 rules out.
///
/// A caller with more recipients than this splits before calling:
///
/// ```
/// use messaging::submit_multi::MAX_DESTINATIONS;
///
/// let recipients: Vec<u32> = (0..600).collect();
/// let batches: Vec<&[u32]> = recipients.chunks(MAX_DESTINATIONS).collect();
///
/// assert_eq!(batches.len(), 3);
/// assert_eq!(batches[0].len(), MAX_DESTINATIONS);
/// assert_eq!(batches[2].len(), 600 - 2 * MAX_DESTINATIONS);
/// ```
pub const MAX_DESTINATIONS: usize = 254;

/// Builds the `submit_multi` of spec §7.4 for one batch of recipients.
///
/// Every field but the recipient list is built by exactly the code
/// [`crate::submit::build_submit_sm`] uses, so the two PDUs cannot drift.
///
/// # `options.destination` is ignored
///
/// [`SubmitOptions`] carries one recipient and this PDU carries a list, so the
/// field is a placeholder here — the same arrangement
/// [`CampaignPlan`](crate::campaign::runner::CampaignPlan) already makes for the
/// per-recipient send. A separate "options without a destination" type would be
/// [`SubmitOptions`] with one field removed and two structures to keep in step.
///
/// # One body for everybody
///
/// The signature takes **one** [`Segment`], which is what "recipients sharing
/// the same content" means: a template resolving `{{prenom}}` per recipient has
/// no shared body and cannot be batched at all. Nothing here can express a
/// per-recipient body, deliberately.
///
/// # Errors
///
/// [`SubmitBuildError::NoDestinations`] on an empty batch,
/// [`SubmitBuildError::TooManyDestinations`] past [`MAX_DESTINATIONS`], and
/// otherwise whatever [`crate::submit::build_submit_sm`] would have refused for
/// the same options and segment.
pub fn build_submit_multi(
    options: &SubmitOptions,
    destinations: &[Destination],
    segment: &Segment,
) -> Result<SubmitMulti, SubmitBuildError> {
    if destinations.is_empty() {
        return Err(SubmitBuildError::NoDestinations);
    }

    if destinations.len() > MAX_DESTINATIONS {
        return Err(SubmitBuildError::TooManyDestinations {
            maximum: MAX_DESTINATIONS,
        });
    }

    // Built BEFORE the common fields so an unroutable recipient is refused
    // before anything else is done with the batch — the same ordering
    // `crate::sender` relies on, where everything refusable is refused before a
    // row is written.
    let dest_address = destinations
        .iter()
        .map(|destination| {
            Ok(DestAddress::SmeAddress(SmeAddress::new(
                destination.ton(),
                destination.npi(),
                destination.to_field()?,
            )))
        })
        .collect::<Result<Vec<_>, SubmitBuildError>>()?;

    let common = CommonFields::build(options, segment)?;

    Ok(SubmitMulti::new(
        common.service_type,
        common.source_ton,
        common.source_npi,
        common.source_addr,
        dest_address,
        common.esm_class,
        options.protocol_id,
        options.priority_flag,
        common.schedule_delivery_time,
        common.validity_period,
        options.registered_delivery,
        options.replace_if_present_flag,
        common.data_coding,
        options.sm_default_msg_id,
        common.short_message,
        common.tlvs,
    ))
}

/// One recipient the message centre refused, out of a batch.
///
/// The `unsuccess_sme` entry of spec §7.4.2, minus the TON and NPI: this client
/// matches on the address, and a message centre answering with a different TON
/// for the same digits is quoting the same subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The address as the message centre quoted it back.
    pub destination: String,
    /// `error_status_code` — this recipient's own refusal, not the batch's.
    pub status: CommandStatus,
}

/// How the answer to a `submit_multi` reads.
///
/// # The fallback rule, stated once
///
/// CA-010-08 asks for an automatic fallback when the message centre **refuses
/// the operation**, and for nothing else. The distinction is not cosmetic: a
/// fallback re-emits the batch as 254 `submit_sm`, so triggering it on the
/// wrong failure either duplicates messages or answers a message centre asking
/// for less traffic with more of it.
///
/// | Answer | Read as | Falls back |
/// |---|---|---|
/// | `generic_nack`, any status | the operation is unknown | **yes** |
/// | `ESME_RINVCMDID` | the operation is unknown | **yes** |
/// | `ESME_ROK` + a `submit_multi_resp` | a verdict per recipient | no |
/// | `ESME_ROK` + anything else | nothing can be claimed | no |
/// | any other status | the whole PDU was refused | no |
/// | no answer at all (a [`SubmitError`](crate::ports::SubmitError)) | nothing can be claimed | no |
///
/// The last two rows are the ones worth defending.
///
/// **A refusal that is not `ESME_RINVCMDID`** — `ESME_RTHROTTLED`,
/// `ESME_RMSGQFUL`, `ESME_RINVSRCADR`, `ESME_RSUBMITFAIL` — says the message
/// centre understood the operation and declined this instance of it. Every one
/// of those reasons applies just as much to the `submit_sm` a fallback would
/// send, and on the throttling codes the fallback would be actively harmful.
/// The recipients get the status on their own rows and the replay policy of
/// spec §10.7 decides, exactly as it does for a `submit_sm`.
///
/// **No answer at all** is the case ADR 0014 arbitrates, and it is the reason
/// this list is not "everything that failed". A `submit_multi` that left and
/// whose response never came may have been taken for all 254 recipients; a
/// fallback would then deliver every one of them a second copy, inside a single
/// run, without the row ever passing through the resume where the arbitration
/// lives and where the duplicate would be **counted**. So there is no fallback:
/// the batch is left uncertain, and [`Batch::last_attempt`] decides what the
/// rows then say. See [`verdict_update`] — the short version is that a caller
/// with attempts left leaves them `SENT` without a `command_status`, ADR 0014's
/// third line, and a caller on its last attempt renders the verdict and writes
/// `FAILED`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MultiResponse {
    /// A readable `submit_multi_resp`. The verdict is **per recipient**.
    Answered {
        /// The one identifier the message centre assigned to the whole batch.
        ///
        /// See [`BatchReport::smsc_message_id`] for why it is reported and not
        /// journalled.
        smsc_message_id: Option<String>,
        /// The recipients it refused, each with its own status.
        refused: Vec<Refusal>,
    },

    /// The message centre does not know this operation.
    Unsupported {
        /// The status it answered with.
        status: CommandStatus,
    },

    /// The message centre answered, refusing the whole PDU.
    Refused {
        /// The status it answered with.
        status: CommandStatus,
    },

    /// The message centre answered something this client cannot read.
    ///
    /// It said `ESME_ROK` over a body that is not a `submit_multi_resp`, so it
    /// may well have accepted every recipient. Nothing is claimed either way.
    Unreadable,
}

impl MultiResponse {
    /// Whether this answer means "send them one at a time instead".
    ///
    /// See the type documentation for the whole rule and for what deliberately
    /// does **not** trigger it.
    #[must_use]
    pub const fn triggers_fallback(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }
}

/// Reads the answer to a `submit_multi`.
///
/// A pure function of the response, so the table in [`MultiResponse`] is
/// checked case by case rather than through a message centre.
#[must_use]
pub fn read_multi_response(response: &Command) -> MultiResponse {
    // Checked BEFORE the status: a `generic_nack` is what a message centre
    // sends when it could not even route the command, and its status is
    // whatever it chose to put there.
    if response.id() == CommandId::GenericNack {
        return MultiResponse::Unsupported {
            status: response.status(),
        };
    }

    let status = response.status();

    if status == CommandStatus::EsmeRinvcmdid {
        return MultiResponse::Unsupported { status };
    }

    if status != CommandStatus::EsmeRok {
        return MultiResponse::Refused { status };
    }

    let Some(Pdu::SubmitMultiResp(body)) = response.pdu() else {
        return MultiResponse::Unreadable;
    };

    let identifier = body.message_id.as_str();

    MultiResponse::Answered {
        // An empty `message_id` is not an identifier, and storing one would be
        // indistinguishable from an identifier a message centre really
        // assigned — the same reasoning as `crate::sender`'s.
        smsc_message_id: (!identifier.is_empty()).then(|| identifier.to_owned()),
        refused: body
            .unsuccess_sme()
            .iter()
            .map(|entry| Refusal {
                destination: entry.destination_addr.as_str().to_owned(),
                status: entry.error_status_code,
            })
            .collect(),
    }
}

/// Attributes the refusals to the recipients of the batch, in order.
///
/// `Some(verdicts)` has one entry per recipient: `None` for a recipient the
/// message centre did not refuse — it took the message — and `Some(status)` for
/// one it did.
///
/// # One rule: any ambiguity voids the batch
///
/// A refusal is attributed only when it names **exactly one** recipient, and
/// each recipient is refused **at most once**. Four situations return `None`,
/// and they are the same situation:
///
/// | The answer | Why nothing can be claimed |
/// |---|---|
/// | more refusals than recipients | at least one names nobody |
/// | a refusal naming nobody in the batch | the addresses are not comparable |
/// | a refusal naming **two** recipients | which of the two was refused? |
/// | **two** refusals naming one recipient | which status is the verdict? |
///
/// The third row is not hypothetical and it is not only about a subscriber
/// listed twice. [`Destination::parse_with`] builds two *legitimately distinct*
/// destinations out of the same digits under different TONs — a short code and
/// an international number — and `unsuccess_sme` gives this client nothing that
/// separates them. Attributing to the first would leave the second reported
/// `Accepted` for a message centre that took nobody.
///
/// # `None`, and why it is all-or-nothing
///
/// `None` means at least one refusal could not be attributed: it names an
/// address that is not in this batch, or there are more refusals than
/// recipients. The caller must then claim **nothing** about **any** recipient of
/// the batch.
///
/// Weakening that to "attribute the ones that match" is the silent-over-claim
/// this function exists to prevent. Suppose a message centre quotes addresses in
/// a form this client does not recognise: every entry fails to match, every
/// recipient looks absent from the refusals, and a whole batch of rejected
/// messages is journalled `ACCEPTED`. Nobody would see it — no error, no log,
/// no failed message — until the delivery figures were queried weeks later.
///
/// Voiding the batch instead costs precision and no correctness: no recipient
/// is claimed either way, and what the rows then say follows
/// [`Batch::last_attempt`] — the uncertain family of ADR 0014 while the caller
/// has attempts left, a rendered `FAILED` verdict on the last one. See
/// [`verdict_update`].
///
/// # What matching tolerates
///
/// The address as sent, or the same digits behind a `+`. That is the whole list.
/// A `+` is a presentation prefix — [`Destination`] strips it on the way in — so
/// the two spellings are one subscriber. Anything looser is the mistake
/// [`crate::correlation::IdMatching`] documents at length: a lenient match on a
/// dense numeric space silently attributes one message's verdict to another.
#[must_use]
pub fn match_refusals(
    destinations: &[Destination],
    refused: &[Refusal],
) -> Option<Vec<Option<CommandStatus>>> {
    if refused.len() > destinations.len() {
        return None;
    }

    let mut verdicts = vec![None; destinations.len()];

    for refusal in refused {
        let quoted = refusal.destination.trim();
        let quoted = quoted.strip_prefix('+').unwrap_or(quoted);

        let mut naming = destinations
            .iter()
            .enumerate()
            .filter(|(_, destination)| destination.number().as_str() == quoted);

        // EXACTLY ONE, and `next()` twice rather than `position()`. `position`
        // returns the FIRST match and says nothing about the second, which is
        // how a batch carrying one subscriber twice — or two destinations
        // differing only by their TON — had the refusal pinned on one of them
        // and the other reported accepted.
        let (matched, _) = naming.next()?;

        if naming.next().is_some() {
            return None;
        }

        // `get_mut` rather than indexing: the index comes from the same slice
        // `verdicts` was sized from, so this cannot fail — and an index that
        // panicked on a message centre's answer would be a remote peer crashing
        // the client.
        let verdict = verdicts.get_mut(matched)?;

        // Already refused by an earlier entry, with a status that may not be
        // this one. Overwriting kept whichever arrived last and called it the
        // verdict; this function does not guess.
        if verdict.is_some() {
            return None;
        }

        *verdict = Some(refusal.status);
    }

    Some(verdicts)
}

// ===========================================================================
// The batch send path
// ===========================================================================

/// One recipient of a batch, with the row it already owns or is about to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRecipient {
    /// The write-ahead key. Derived by
    /// [`message_key`](crate::campaign::resume::message_key) for a campaign.
    pub client_message_id: ClientMessageId,
    /// Where the message goes.
    pub destination: Destination,
}

/// One message, N recipients (spec §10.6).
///
/// Everything but the recipient list is shared, which is what "the same
/// content" means and what makes the batch legal at all.
#[derive(Debug, Clone)]
pub struct Batch {
    /// The body every recipient receives, already rendered.
    pub text: String,
    /// The fields of spec §7.3. `destination` is a placeholder — see
    /// [`build_submit_multi`].
    pub submit: SubmitOptions,
    /// Automatic encoding, or the one the operator forced.
    pub encoding: EncodingChoice,
    /// How the parts of a long message announce that they belong together.
    ///
    /// Only ever reached on the individual path: a batch that needs several
    /// segments has no `submit_multi` form.
    pub mode: SegmentationMode,
    /// Which sending attempt this is, counting from 1.
    pub attempt: u32,
    /// Whether the caller will accept this attempt's verdict as final.
    ///
    /// Exactly [`SendRequest::last_attempt`], and for the same reason: a
    /// failure the caller may replay is journalled `SENT`, so a later
    /// acceptance can still be recorded over it.
    pub last_attempt: bool,
    /// The campaign these messages belong to, when they belong to one.
    pub campaign_id: Option<CampaignId>,
    /// Who receives it.
    pub recipients: Vec<BatchRecipient>,
}

impl Batch {
    /// A first attempt at a batch, whose verdict the caller takes as final.
    #[must_use]
    pub fn new(
        text: impl Into<String>,
        submit: SubmitOptions,
        recipients: Vec<BatchRecipient>,
    ) -> Self {
        Self {
            text: text.into(),
            submit,
            encoding: EncodingChoice::Automatic,
            mode: SegmentationMode::default(),
            attempt: 1,
            last_attempt: true,
            campaign_id: None,
            recipients,
        }
    }

    /// The same batch, as part of a campaign.
    #[must_use]
    pub const fn in_campaign(mut self, campaign_id: CampaignId) -> Self {
        self.campaign_id = Some(campaign_id);
        self
    }

    /// The same batch under another encoding choice.
    #[must_use]
    pub fn with_encoding(mut self, encoding: EncodingChoice) -> Self {
        self.encoding = encoding;
        self
    }

    /// The same batch under another concatenation mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: SegmentationMode) -> Self {
        self.mode = mode;
        self
    }

    /// The same batch, marked as attempt number `attempt`.
    #[must_use]
    pub const fn as_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    /// The same batch, as an attempt the caller may replay.
    #[must_use]
    pub const fn with_more_attempts_allowed(mut self, allowed: bool) -> Self {
        self.last_attempt = !allowed;
        self
    }
}

/// What became of **one** recipient of a batch.
///
/// The reason this type exists rather than a single verdict on the batch:
/// `submit_multi_resp` is neither a success nor a failure. It carries one
/// identifier and a list of the recipients the message centre refused, each
/// with its own `error_status_code`, so a batch of 254 can come back with 251
/// accepted and three rejected for three different reasons. A batch-level
/// verdict would have to round that to "success" or "failure", and every figure
/// downstream — the journal, the campaign counters, the delivery rate — would
/// inherit the rounding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecipientOutcome {
    /// The message centre took it.
    Accepted,

    /// The message centre refused it, with this status.
    Rejected {
        /// This recipient's own `error_status_code`, or the batch's when the
        /// whole PDU was refused.
        status: CommandStatus,
    },

    /// No verdict could be established for this recipient.
    ///
    /// Three ways in, and they are the same fact: the `submit_multi` left and
    /// nothing came back that says what happened to this person. No answer at
    /// all, an answer this client cannot read, or an answer whose refusals
    /// cannot be attributed ([`match_refusals`]).
    ///
    /// The row is left in the uncertain family of ADR 0014 — `SENT` with no
    /// `command_status`, when the caller still has attempts left — which is
    /// where the duplicate arbitration lives and where the duplicate gets
    /// counted. On a **last** attempt the verdict is rendered instead, and the
    /// row is `FAILED`; see [`verdict_update`].
    Uncertain,

    /// The **session** refused the submission before writing to the socket.
    ///
    /// Distinct from [`Self::Uncertain`], and the distinction is the whole
    /// point: [`SubmitError::prevented_emission`] is a *guarantee* of the port,
    /// not a hint, so this recipient certainly received nothing and re-sending
    /// certainly cannot duplicate.
    ///
    /// Conflating the two used to multiply the duplicate-risk figure of ADR
    /// 0014 by the batch size — that figure is sized as "at most the send
    /// window", and one reconnecting session turned it into 254 at a stroke.
    /// [`BatchReport::at_risk_of_duplication`] excludes these.
    ///
    /// **The row still over-claims**, and this does not fix that: the `SENT`
    /// transition is committed before the socket by design, and ADR 0014 rules
    /// out the only narrowing available — writing a `command_status` no message
    /// centre sent. What changes is that the caller can now see the difference
    /// and stop counting these as possible duplicates.
    NotEmitted,

    /// The recipient already had a row, so nothing was sent.
    ///
    /// The write-ahead insert is the guard (ADR 0014, decision 1): a
    /// conflicting key means somebody already owns this message. The caller
    /// decides what to do with it — [`EmissionGuard`](crate::campaign::resume::EmissionGuard)
    /// is what answers that question, and it is deliberately not answered here.
    AlreadyPresent,
}

/// Which PDU carried one recipient's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Via {
    /// One `submit_multi`, shared with the rest of the batch.
    Multi,
    /// Its own `submit_sm`.
    Individual,
    /// Nothing was sent.
    Nothing,
}

/// Why a batch was not sent as one `submit_multi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FallbackReason {
    /// The operator turned batching off.
    Disabled,

    /// This session has already answered in a way that rules batching out.
    KnownUnsupported,

    /// The latch belongs to another session, so it says nothing about this one.
    ///
    /// A caller mistake rather than a message centre's answer, and it is
    /// answered by **not batching** rather than by an error: every recipient
    /// still gets their message, and reading another bind's latch would either
    /// disable batching on a centre that supports it or re-enable it on one that
    /// does not.
    ForeignLatch,

    /// One recipient. A `submit_multi` to one address is a `submit_sm` with
    /// more octets and one more way for a message centre to say no.
    SingleRecipient,

    /// The text does not fit one PDU.
    ///
    /// `submit_multi` carries a single `short_message`, so a concatenated
    /// message has no batched form: sending part 1 to 254 people and part 2 to
    /// 254 people would need two PDUs whose partial failures could not be
    /// reconciled per recipient.
    MultipleSegments,

    /// The message centre refused the **operation**, and the batch was
    /// re-sent one message at a time.
    ///
    /// The only reason reached **after** a PDU left. See [`MultiResponse`] for
    /// what qualifies and, more importantly, what does not.
    OperationRefused {
        /// The status it refused with.
        status: CommandStatus,
    },
}

/// What became of every recipient of one batch.
#[derive(Debug, Clone)]
pub struct BatchReport {
    /// The session it went out on.
    pub session_id: SessionId,

    /// The one identifier a `submit_multi_resp` assigned to the whole batch.
    ///
    /// **Reported and never journalled.** See the module header: N rows sharing
    /// one identifier would make every delivery receipt correlate to whichever
    /// of them the index returned first.
    pub smsc_message_id: Option<String>,

    /// Why the batch was not sent as one `submit_multi`, when it was not.
    pub fallback: Option<FallbackReason>,

    /// Whether the journal recorded the outcome.
    ///
    /// `false` means the messages **were** submitted and the verdicts could not
    /// be written. Same meaning, and same reason for being a field rather than
    /// an error, as [`SendReport::journalled`].
    pub journalled: bool,

    /// How many recipients a previous run had left **in flight**.
    ///
    /// The duplicate-risk figure of ADR 0014, for the recipients whose row was
    /// already `SENT` with no `command_status` when this batch picked them up.
    /// Under [`UnansweredPolicy::Reemit`] each of them may read the message
    /// twice, and the arbitration is only honest if the number is reported —
    /// which is the same contract `CampaignTally::reemitted_unanswered` carries.
    ///
    /// Distinct from [`Self::at_risk_of_duplication`], which is about what
    /// **this** batch leaves behind.
    pub reemitted_unanswered: usize,

    /// One entry per recipient of the batch, in the order they were given.
    ///
    /// CA-010-08 is "without losing a recipient", and this is where that is
    /// checked: the length equals the batch's, always, whatever the message
    /// centre answered.
    pub recipients: Vec<RecipientReport>,
}

impl BatchReport {
    /// How many recipients the message centre took.
    #[must_use]
    pub fn accepted(&self) -> usize {
        self.recipients
            .iter()
            .filter(|entry| entry.outcome == RecipientOutcome::Accepted)
            .count()
    }

    /// Whether one `submit_multi` carried the batch.
    #[must_use]
    pub fn used_submit_multi(&self) -> bool {
        self.recipients.iter().any(|entry| entry.via == Via::Multi)
    }

    /// How many recipients may receive this message twice if it is sent again.
    ///
    /// The batch's contribution to the figure ADR 0014 asks a campaign to
    /// report (`reemitted_unanswered`). Exactly the
    /// [`RecipientOutcome::Uncertain`] entries: a submission the session refused
    /// before the socket is **not** one of them, because the port guarantees
    /// nothing was written and re-sending cannot duplicate.
    ///
    /// The distinction is worth a method rather than a filter at each call site:
    /// counting `NotEmitted` here is how one reconnecting session turned a
    /// window-sized risk into a 254-sized one.
    #[must_use]
    pub fn at_risk_of_duplication(&self) -> usize {
        self.recipients
            .iter()
            .filter(|entry| entry.outcome == RecipientOutcome::Uncertain)
            .count()
    }
}

/// What became of one recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientReport {
    /// The write-ahead key, so the caller can follow the message.
    pub client_message_id: ClientMessageId,
    /// Where it was going.
    pub destination: Msisdn,
    /// What happened.
    pub outcome: RecipientOutcome,
    /// Which PDU carried it.
    pub via: Via,
}

/// Whether **one** session can usefully be batched.
///
/// A latch, held across the batches of a whole campaign. A message centre that
/// answered `ESME_RINVCMDID` once will answer it again: asking two thousand
/// more times costs two thousand round trips and two thousand `warn!` lines for
/// an answer that is already known.
///
/// # It carries its `SessionId`, and that is not decoration
///
/// What a latch learned is a fact about one message centre on one bind. This
/// type used to only *say* in prose that the caller builds one per session, with
/// nothing enforcing it and no caller honouring it — so a latch shared across
/// sessions would have disabled batching on a centre that supports it, or
/// re-enabled it on one that does not. [`Self::for_session`] is the only
/// constructor, and [`BatchSender`] refuses to read a latch belonging to another
/// session ([`FallbackReason::ForeignLatch`]).
///
/// # It only ever moves towards [`MultiSupportState::Unsupported`]
///
/// The safe direction: the fallback works everywhere, and re-enabling batching
/// on a session that refused it once would need evidence this client cannot
/// obtain.
#[derive(Debug)]
pub struct MultiSupport {
    session_id: SessionId,
    unsupported: AtomicBool,
    proven: AtomicBool,
}

/// What a [`MultiSupport`] knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MultiSupportState {
    /// Nothing has been tried yet.
    Unknown,

    /// A `submit_multi_resp` came back from this session **and its verdicts
    /// could be attributed**.
    Supported,

    /// Batching this session is not usable.
    ///
    /// Two ways in, and they cost the same:
    ///
    /// * the session refused the operation — `ESME_RINVCMDID`, `generic_nack`;
    /// * it answered a `submit_multi_resp` whose refusals could not be
    ///   attributed ([`match_refusals`]). A centre that quotes its addresses in
    ///   a form this client cannot compare will quote them that way again, so
    ///   every later batch would spend 254 rows on an answer nothing can be read
    ///   from.
    Unsupported,
}

impl MultiSupport {
    /// A latch for one session, which has learned nothing yet.
    #[must_use]
    pub const fn for_session(session_id: SessionId) -> Self {
        Self {
            session_id,
            unsupported: AtomicBool::new(false),
            proven: AtomicBool::new(false),
        }
    }

    /// Which session this latch speaks for.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// What this latch knows.
    #[must_use]
    pub fn state(&self) -> MultiSupportState {
        if self.unsupported.load(Ordering::SeqCst) {
            MultiSupportState::Unsupported
        } else if self.proven.load(Ordering::SeqCst) {
            MultiSupportState::Supported
        } else {
            MultiSupportState::Unknown
        }
    }

    /// Records that batching this session is not usable. Never undone.
    fn note_unsupported(&self) {
        self.unsupported.store(true, Ordering::SeqCst);
    }

    /// Records that this session answered usably.
    fn note_supported(&self) {
        self.proven.store(true, Ordering::SeqCst);
    }
}

/// Sends one batch, falling back onto individual `submit_sm` when it must
/// (L-010-06, CA-010-08).
///
/// Borrows the send orchestrator rather than replacing it: the individual path
/// **is** [`Sender`], with its write-ahead ordering, its segmentation and its
/// transitions. A second send path would drift, and the one that drifts is
/// always the one running on the day of the incident.
#[derive(Debug)]
pub struct BatchSender<'a, R, C> {
    sender: &'a Sender<R, C>,
    support: &'a MultiSupport,
    enabled: bool,
    unanswered: UnansweredPolicy,
}

impl<'a, R: MessageRepository, C: Clock> BatchSender<'a, R, C> {
    /// A batch sender over a send orchestrator and one session's latch.
    #[must_use]
    pub const fn new(sender: &'a Sender<R, C>, support: &'a MultiSupport) -> Self {
        Self {
            sender,
            support,
            enabled: true,
            unanswered: UnansweredPolicy::Reemit,
        }
    }

    /// The same batch sender, under another arbitration for a recipient a
    /// previous run left in flight.
    ///
    /// Exactly [`UnansweredPolicy`], applied by exactly
    /// [`EmissionGuard`] — the batch path does not get an arbitration of its
    /// own, because a second one would be a second answer to the question ADR
    /// 0014 settled.
    #[must_use]
    pub const fn on_unanswered(mut self, policy: UnansweredPolicy) -> Self {
        self.unanswered = policy;
        self
    }

    /// The same batch sender, with batching turned on or off.
    ///
    /// Off is not a degraded mode: every recipient still gets their message,
    /// one `submit_sm` each, and CA-010-08 says `submit_multi` is used "when it
    /// is **enabled** and supported".
    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sends one batch and reports what became of every recipient.
    ///
    /// # The order, and where it is load-bearing
    ///
    /// ```text
    /// segment → build → PERSIST every recipient → journal every attempt
    ///         → submit_multi → per-recipient verdicts | fall back
    /// ```
    ///
    /// The two halves of the write-ahead of CLAUDE.md §4 hold **per recipient**
    /// and not per PDU: a batch of 254 is 254 rows and 254 `SENT` transitions,
    /// all committed before the single `submit_multi` reaches the socket. A
    /// `kill -9` in between therefore leaves 254 rows saying "a PDU may have
    /// left", which is what [`crate::campaign::resume`] reads and what ADR 0014
    /// arbitrates. One row per batch would have made 253 recipients invisible to
    /// the resume.
    ///
    /// # Errors
    ///
    /// * [`MessagingError::Encoding`] if the text cannot be written under the
    ///   chosen encoding;
    /// * [`MessagingError::Submit`] if a field of spec §7.3 or §7.4 does not
    ///   fit, including an empty batch and one beyond [`MAX_DESTINATIONS`];
    /// * [`MessagingError::Store`] if the journal refuses a write-ahead insert
    ///   or the attempt transition — in which case **nothing was sent**.
    ///
    /// A message centre that refused messages is **not** an error: it comes back
    /// in the [`BatchReport`], recipient by recipient.
    #[tracing::instrument(
        skip_all,
        fields(
            session_id = %session.session_id(),
            recipients = batch.recipients.len(),
            attempt = batch.attempt,
        )
    )]
    pub async fn submit_batch<S: SmscSession>(
        &self,
        session: &S,
        batch: &Batch,
    ) -> Result<BatchReport, MessagingError> {
        if batch.recipients.is_empty() {
            return Err(SubmitBuildError::NoDestinations.into());
        }

        if batch.recipients.len() > MAX_DESTINATIONS {
            return Err(SubmitBuildError::TooManyDestinations {
                maximum: MAX_DESTINATIONS,
            }
            .into());
        }

        // Segmented once, under the session's own conventions (ADR 0008 and
        // 0009), and only to answer "does this fit one PDU". The concatenation
        // reference is irrelevant to that question and deliberately fixed: a
        // batch that needs one is sent by `Sender`, which draws its own.
        let options = SegmentationOptions::default()
            .with_encoding(batch.encoding)
            .with_mode(batch.mode)
            .with_gsm_packing(session.gsm7_packing())
            .with_gsm_charset(session.gsm7_charset());

        let split = segment(&batch.text, &options, ConcatenationReference::new(0))?;

        let mut slots: Vec<Slot> = batch.recipients.iter().map(|_| Slot::pending()).collect();
        let mut reemitted = 0_usize;

        if let Some(reason) =
            self.why_not_multi(batch, split.segments().len(), session.session_id())
        {
            tracing::debug!(reason = ?reason, "the batch is sent one message at a time");

            let journalled = self
                .send_each(session, batch, &mut slots, &mut reemitted)
                .await?;

            return Ok(report(
                session,
                batch,
                slots,
                Some(reason),
                None,
                journalled,
                reemitted,
            ));
        }

        // Built with EVERY recipient, before a single row is written: a field
        // that does not fit must leave no row behind (CA-006-07), and an
        // unroutable address in slot 200 must not cost 199 inserts first.
        let Some(body) = split.segments().first() else {
            return Err(SubmitBuildError::NoDestinations.into());
        };

        let destinations: Vec<Destination> = batch
            .recipients
            .iter()
            .map(|recipient| recipient.destination.clone())
            .collect();

        build_submit_multi(&batch.submit, &destinations, body)?;

        // --- write ahead, per recipient ------------------------------------
        let live = self
            .write_ahead(session, batch, &split, &mut slots, &mut reemitted)
            .await?;

        if live.is_empty() {
            tracing::debug!("every recipient of the batch already had a row");

            return Ok(report(session, batch, slots, None, None, true, reemitted));
        }

        let live_destinations: Vec<Destination> = live
            .iter()
            .filter_map(|index| batch.recipients.get(*index))
            .map(|recipient| recipient.destination.clone())
            .collect();

        // Rebuilt for the recipients that survived the inserts. It cannot fail
        // where the build above succeeded — this is a subset of the same
        // addresses and the same options — and the `?` is there because
        // asserting that would be a `panic!` in production code. The rows are
        // `QUEUED`, so a failure here loses nothing.
        let pdu = build_submit_multi(&batch.submit, &live_destinations, body)?;

        let answer = session.submit(Pdu::SubmitMulti(pdu)).await;
        let responded_at = self.sender.clock().now();

        match self.verdicts(&answer, &live_destinations) {
            BatchVerdicts::FallBack { status } => {
                self.support.note_unsupported();

                tracing::warn!(
                    status = ?status,
                    recipients = live.len(),
                    "the message centre refused submit_multi; the batch is re-sent \
                     one message at a time and this session will not be batched again"
                );

                let journalled = self.resend_each(session, batch, &live, &mut slots).await?;

                Ok(report(
                    session,
                    batch,
                    slots,
                    Some(FallbackReason::OperationRefused { status }),
                    None,
                    journalled,
                    reemitted,
                ))
            }
            BatchVerdicts::PerRecipient {
                outcomes,
                smsc_message_id,
                uncertain_is_retryable,
            } => {
                let journalled = self
                    .journal_verdicts(
                        batch,
                        &live,
                        &outcomes,
                        responded_at,
                        uncertain_is_retryable,
                    )
                    .await;

                for (position, index) in live.iter().enumerate() {
                    if let (Some(slot), Some(outcome)) =
                        (slots.get_mut(*index), outcomes.get(position))
                    {
                        // `Via` is what CARRIED the message, so a submission the
                        // session refused before the socket was carried by
                        // nothing — even though a `submit_multi` was built for
                        // it. Reporting `Multi` here would have the caller
                        // believe a PDU left.
                        slot.via = if *outcome == RecipientOutcome::NotEmitted {
                            Via::Nothing
                        } else {
                            Via::Multi
                        };
                        slot.outcome = outcome.clone();
                    }
                }

                tracing::info!(
                    accepted = outcomes
                        .iter()
                        .filter(|outcome| **outcome == RecipientOutcome::Accepted)
                        .count(),
                    recipients = live.len(),
                    journalled,
                    "the batch was submitted"
                );

                Ok(report(
                    session,
                    batch,
                    slots,
                    None,
                    smsc_message_id,
                    journalled,
                    reemitted,
                ))
            }
        }
    }

    /// Why this batch cannot go out as one `submit_multi`, if it cannot.
    fn why_not_multi(
        &self,
        batch: &Batch,
        segments: usize,
        session_id: SessionId,
    ) -> Option<FallbackReason> {
        if !self.enabled {
            return Some(FallbackReason::Disabled);
        }

        if self.support.session_id() != session_id {
            tracing::warn!(
                latch = %self.support.session_id(),
                "the submit_multi latch belongs to another session; this batch is \
                 sent one message at a time rather than on another bind's evidence"
            );

            // NO `debug_assert!(false)` here, unlike the runner's genuinely
            // unreachable arm. This one IS reachable — it is a caller mistake —
            // and it is *handled*: every recipient still gets their message.
            // Aborting a debug build would make the safe handling the only thing
            // no test could exercise.
            return Some(FallbackReason::ForeignLatch);
        }

        if self.support.state() == MultiSupportState::Unsupported {
            return Some(FallbackReason::KnownUnsupported);
        }

        if batch.recipients.len() < 2 {
            return Some(FallbackReason::SingleRecipient);
        }

        (segments != 1).then_some(FallbackReason::MultipleSegments)
    }

    /// Writes one `QUEUED` row and one `SENT` transition per recipient, before
    /// anything reaches the socket.
    ///
    /// Returns the indices of the recipients that are the batch's to send. A
    /// conflicting key is **not** an error: it is the guard of ADR 0014 firing,
    /// and that recipient leaves the batch with [`RecipientOutcome::AlreadyPresent`].
    async fn write_ahead<S: SmscSession>(
        &self,
        session: &S,
        batch: &Batch,
        split: &SegmentedMessage,
        slots: &mut [Slot],
        reemitted: &mut usize,
    ) -> Result<Vec<usize>, MessagingError> {
        let created_at = self.sender.clock().now();
        let mut live = Vec::with_capacity(batch.recipients.len());

        for (index, recipient) in batch.recipients.iter().enumerate() {
            let request = request_for(batch, recipient);
            let row = self
                .sender
                .queued_row(&request, session.session_id(), split, 1, created_at);

            match self.sender.repository().insert_message(&row).await {
                Ok(()) => live.push(index),
                // A CONFLICT IS NOT A REASON TO SKIP, it is a reason to ask.
                //
                // The insert is the first guard (ADR 0014, decision 1) and the
                // state check is the second; skipping on the first alone loses
                // the recipient of any row a previous run left behind. Those
                // rows exist: the inserts below are one per recipient rather
                // than one transaction, so a failure at the k-th leaves k − 1
                // `QUEUED` rows that nothing sent — and reading them as "already
                // has a message" meant they were never sent at all.
                Err(MessageStoreError::Conflict) => {
                    if self
                        .admit(recipient.client_message_id, index, slots, reemitted)
                        .await?
                    {
                        live.push(index);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }

        if live.is_empty() {
            return Ok(live);
        }

        // THE ATTEMPTS, COMMITTED BEFORE THE SOCKET AND IN ONE TRANSACTION.
        // The second half of the write-ahead of CLAUDE.md §4, and the reason a
        // resume can read these rows at all — see `crate::sender`, where the
        // same commit is made per message and where getting it wrong emptied the
        // population ADR 0014 arbitrates.
        //
        // A FAILURE HERE STOPS THE BATCH: nothing has been submitted, so
        // propagating loses nothing, and sending on would put a PDU on the wire
        // the journal has no attempt for.
        let sent_at = self.sender.clock().now();
        let transitions: Vec<MessageStateUpdate> = live
            .iter()
            .filter_map(|index| batch.recipients.get(*index))
            .map(|recipient| {
                MessageStateUpdate::new(recipient.client_message_id, MessageState::Sent)
                    .sent_at(sent_at, batch.attempt)
            })
            .collect();

        self.sender.repository().update_states(&transitions).await?;

        tracing::debug!(
            recipients = live.len(),
            "every recipient of the batch is persisted before submission"
        );

        Ok(live)
    }

    /// Asks the guard what to do about a recipient whose row already exists.
    ///
    /// `true` when the batch may send to them **without** inserting: the row is
    /// there and the message centre has not accepted it. `false` fills the slot
    /// with [`RecipientOutcome::AlreadyPresent`] and the recipient leaves the
    /// batch.
    ///
    /// The same [`EmissionGuard`] and the same [`UnansweredPolicy`] the campaign
    /// runner uses on the same conflict — deliberately, since a second reading
    /// of "may this recipient be emitted to" is a second answer to CA-010-05.
    async fn admit(
        &self,
        client_message_id: ClientMessageId,
        index: usize,
        slots: &mut [Slot],
        reemitted: &mut usize,
    ) -> Result<bool, MessagingError> {
        let guard = EmissionGuard::new(self.sender.repository(), self.unanswered);

        match guard.admit(client_message_id).await? {
            Admission::Resume { was_unanswered, .. } => {
                if was_unanswered {
                    // ADR 0014's duplicate-risk figure, kept for the batch path:
                    // this row may already have been taken by a previous run, so
                    // its recipient may read the message twice.
                    *reemitted += 1;
                }

                Ok(true)
            }
            Admission::Skip(reason) => {
                tracing::debug!(reason = ?reason, "the recipient already has a message");

                if let Some(slot) = slots.get_mut(index) {
                    slot.outcome = RecipientOutcome::AlreadyPresent;
                    slot.via = Via::Nothing;
                }

                Ok(false)
            }
            // The insert conflicted, so a row exists, and the read that followed
            // found none. With SQLite that is another run of the same campaign
            // deleting or rolling back in between — the case
            // `CampaignRunner::emit` documents at the same junction. Counted as
            // a skip, which is what it is: another writer owns this recipient.
            Admission::Fresh => {
                tracing::warn!(
                    "the write-ahead key conflicted and its row could not be read back; \
                     another run is writing to the same journal"
                );

                if let Some(slot) = slots.get_mut(index) {
                    slot.outcome = RecipientOutcome::AlreadyPresent;
                    slot.via = Via::Nothing;
                }

                Ok(false)
            }
        }
    }

    /// Reads the answer into one verdict per live recipient.
    fn verdicts(
        &self,
        answer: &Result<Command, SubmitError>,
        live_destinations: &[Destination],
    ) -> BatchVerdicts {
        let total = live_destinations.len();

        let response = match answer {
            Err(failure) => {
                let retryable = SendFailure::NoResponse(failure.clone()).is_retryable();

                // THE PORT'S GUARANTEE, READ RATHER THAN IGNORED.
                // `prevented_emission()` says the implementation refused before
                // writing to the socket, so this batch certainly reached nobody.
                // Treating it as a timeout put all `total` recipients in the
                // duplicate-risk family of ADR 0014 at once — a figure that
                // document sizes as "at most the send window".
                if failure.prevented_emission() {
                    tracing::warn!(
                        failure = %failure,
                        recipients = total,
                        "the session refused the submit_multi before the socket; \
                         nothing reached the message centre, so none of these \
                         recipients is a duplicate risk"
                    );

                    return BatchVerdicts::PerRecipient {
                        outcomes: vec![RecipientOutcome::NotEmitted; total],
                        smsc_message_id: None,
                        uncertain_is_retryable: retryable,
                    };
                }

                tracing::warn!(
                    failure = %failure,
                    recipients = total,
                    "the submit_multi produced no answer; every recipient of the batch \
                     is left uncertain and none is re-sent inside this run"
                );

                return BatchVerdicts::uncertain(total, retryable);
            }
            Ok(response) => response,
        };

        match read_multi_response(response) {
            MultiResponse::Unsupported { status } => BatchVerdicts::FallBack { status },
            MultiResponse::Refused { status } => BatchVerdicts::PerRecipient {
                outcomes: vec![RecipientOutcome::Rejected { status }; total],
                smsc_message_id: None,
                uncertain_is_retryable: true,
            },
            MultiResponse::Unreadable => {
                tracing::warn!(
                    recipients = total,
                    "the message centre answered ESME_ROK over a body that is not a \
                     submit_multi_resp; no verdict is claimed for any recipient"
                );

                BatchVerdicts::uncertain(total, true)
            }
            MultiResponse::Answered {
                smsc_message_id,
                refused,
            } => {
                // NOTED ONLY ONCE THE VERDICTS HOLD, and not on the `Answered`
                // shape alone. A centre whose refusals cannot be attributed will
                // answer the next batch the same way; leaving the latch on
                // `Supported` spent 254 rows per batch on an unreadable answer,
                // for ever.
                let Some(attributed) = match_refusals(live_destinations, &refused) else {
                    self.support.note_unsupported();

                    tracing::warn!(
                        refusals = refused.len(),
                        recipients = total,
                        "a submit_multi_resp could not be attributed to this batch's \
                         recipients; no verdict is claimed for any of them, and this \
                         session will not be batched again"
                    );

                    return BatchVerdicts::uncertain(total, true);
                };

                self.support.note_supported();

                BatchVerdicts::PerRecipient {
                    outcomes: attributed
                        .into_iter()
                        .map(|status| match status {
                            None => RecipientOutcome::Accepted,
                            Some(status) => RecipientOutcome::Rejected { status },
                        })
                        .collect(),
                    smsc_message_id,
                    uncertain_is_retryable: true,
                }
            }
        }
    }

    /// Writes the verdict of every live recipient, in one transaction.
    ///
    /// Returns whether the journal took them. A failure is reported rather than
    /// propagated, for the reason [`SendReport::journalled`] gives: by this
    /// point the PDU has left and the message centre has answered, and an error
    /// whose whole meaning is "nothing was sent" would have the operator send
    /// the batch again.
    async fn journal_verdicts(
        &self,
        batch: &Batch,
        live: &[usize],
        outcomes: &[RecipientOutcome],
        responded_at: Timestamp,
        uncertain_is_retryable: bool,
    ) -> bool {
        let transitions: Vec<MessageStateUpdate> = live
            .iter()
            .zip(outcomes)
            .filter_map(|(index, outcome)| {
                let recipient = batch.recipients.get(*index)?;

                Some(verdict_update(
                    recipient.client_message_id,
                    outcome,
                    responded_at,
                    batch.last_attempt,
                    uncertain_is_retryable,
                ))
            })
            .collect();

        if let Err(error) = self.sender.repository().update_states(&transitions).await {
            tracing::error!(
                error = ?error,
                recipients = transitions.len(),
                "the batch was submitted but its verdicts could not be journalled; \
                 the rows stay SENT, and a resume applies the arbitration of ADR 0014"
            );

            return false;
        }

        true
    }

    /// Sends the batch as one `submit_sm` per recipient, rows and all.
    ///
    /// The path taken when the batch never had a `submit_multi` form. Every
    /// recipient goes through [`Sender::send`], so the write-ahead insert is
    /// the guard exactly as it is for a unit send.
    async fn send_each<S: SmscSession>(
        &self,
        session: &S,
        batch: &Batch,
        slots: &mut [Slot],
        reemitted: &mut usize,
    ) -> Result<bool, MessagingError> {
        let mut journalled = true;

        for (index, recipient) in batch.recipients.iter().enumerate() {
            let request = request_for(batch, recipient);

            let report = match self.sender.send(session, &request).await {
                Ok(report) => Some(report),
                // Same guard, same reason as `Self::write_ahead`: a row that
                // exists is a question, not an answer. A `QUEUED` one left by a
                // failed run is re-sent through `resend`, which skips the insert
                // the conflict just refused.
                Err(MessagingError::Store(MessageStoreError::Conflict)) => {
                    if self
                        .admit(recipient.client_message_id, index, slots, reemitted)
                        .await?
                    {
                        Some(self.sender.resend(session, &request).await?)
                    } else {
                        None
                    }
                }
                Err(error) => return Err(error),
            };

            let Some(report) = report else {
                continue;
            };

            journalled &= report.journalled;

            if let Some(slot) = slots.get_mut(index) {
                slot.outcome = outcome_of(&report);
                slot.via = Via::Individual;
            }
        }

        Ok(journalled)
    }

    /// Re-sends a batch the message centre refused the operation for.
    ///
    /// [`Sender::resend`] rather than [`Sender::send`], because the rows were
    /// written by [`Self::write_ahead`] a moment ago. Its contract asks that
    /// something has **established** the row exists and was not accepted, and
    /// here that something is the refusal itself: `ESME_RINVCMDID` and
    /// `generic_nack` both mean the message centre did not take the PDU, for
    /// anybody. That is ADR 0014's second line — answered, refusing — where
    /// re-emitting cannot duplicate.
    async fn resend_each<S: SmscSession>(
        &self,
        session: &S,
        batch: &Batch,
        live: &[usize],
        slots: &mut [Slot],
    ) -> Result<bool, MessagingError> {
        let mut journalled = true;

        for index in live {
            let Some(recipient) = batch.recipients.get(*index) else {
                continue;
            };

            let request = request_for(batch, recipient);

            match self.sender.resend(session, &request).await {
                Ok(report) => {
                    journalled &= report.journalled;

                    if let Some(slot) = slots.get_mut(*index) {
                        slot.outcome = outcome_of(&report);
                        slot.via = Via::Individual;
                    }
                }
                Err(error) => return Err(error),
            }
        }

        Ok(journalled)
    }
}

/// What one pass over a `submit_multi` answer produced.
enum BatchVerdicts {
    /// One verdict per live recipient, in order.
    PerRecipient {
        outcomes: Vec<RecipientOutcome>,
        smsc_message_id: Option<String>,
        /// Whether an [`RecipientOutcome::Uncertain`] here may be tried again.
        uncertain_is_retryable: bool,
    },
    /// Send them one at a time instead.
    FallBack { status: CommandStatus },
}

impl BatchVerdicts {
    /// Every recipient uncertain, which claims nothing about any of them.
    fn uncertain(total: usize, retryable: bool) -> Self {
        Self::PerRecipient {
            outcomes: vec![RecipientOutcome::Uncertain; total],
            smsc_message_id: None,
            uncertain_is_retryable: retryable,
        }
    }
}

/// One recipient's place in the report, while it is being filled.
#[derive(Debug)]
struct Slot {
    outcome: RecipientOutcome,
    via: Via,
}

impl Slot {
    /// A recipient nothing has happened to yet.
    ///
    /// Starts [`RecipientOutcome::Uncertain`] rather than empty on purpose: a
    /// path that forgot to fill a slot would report "nothing is known about this
    /// recipient", which is true and safe, instead of an `Option` some caller
    /// would render as a success.
    const fn pending() -> Self {
        Self {
            outcome: RecipientOutcome::Uncertain,
            via: Via::Nothing,
        }
    }
}

/// The per-recipient request the individual path sends.
fn request_for(batch: &Batch, recipient: &BatchRecipient) -> SendRequest {
    let mut submit = batch.submit.clone();

    submit.destination = recipient.destination.clone();

    let request = SendRequest::new(batch.text.clone(), submit)
        .keyed(recipient.client_message_id)
        .with_encoding(batch.encoding)
        .with_mode(batch.mode)
        .as_attempt(batch.attempt)
        .with_more_attempts_allowed(!batch.last_attempt);

    match batch.campaign_id {
        Some(campaign_id) => request.in_campaign(campaign_id),
        None => request,
    }
}

/// One recipient's outcome, read off a unit send report.
fn outcome_of(report: &SendReport) -> RecipientOutcome {
    if report.is_accepted() {
        return RecipientOutcome::Accepted;
    }

    match report.command_status {
        Some(status) => RecipientOutcome::Rejected { status },
        // No status came back, so nothing is known — the same reading the batch
        // path gives an unanswered `submit_multi`.
        None => RecipientOutcome::Uncertain,
    }
}

/// The transition that closes one recipient's batch attempt.
///
/// The state written is not always the outcome reported, and for exactly the
/// reason [`Sender::final_transition`](crate::sender::Sender) gives: `FAILED` is
/// terminal, so a refusal the caller may still replay is written `SENT` with its
/// `command_status` beside it, and only the attempt nobody will replay writes
/// the terminal state.
fn verdict_update(
    client_message_id: ClientMessageId,
    outcome: &RecipientOutcome,
    responded_at: Timestamp,
    last_attempt: bool,
    uncertain_is_retryable: bool,
) -> MessageStateUpdate {
    let update = MessageStateUpdate::new(client_message_id, MessageState::Accepted);

    match outcome {
        RecipientOutcome::Accepted => update
            .responded_at(responded_at)
            .with_command_status(CommandStatus::EsmeRok),

        RecipientOutcome::Rejected { status } => {
            let retryable = SendFailure::Rejected(*status).is_retryable();
            let state = if !last_attempt && retryable {
                MessageState::Sent
            } else {
                MessageState::Failed
            };

            MessageStateUpdate::new(client_message_id, state)
                .responded_at(responded_at)
                .with_command_status(*status)
        }

        // NO `command_status`, deliberately: nothing answered for this
        // recipient, and writing a status no message centre sent is what would
        // move the row out of the uncertain family ADR 0014 arbitrates.
        //
        // `AlreadyPresent` never reaches here — such a recipient left the batch
        // at the insert and is not in the list this is called over — and it
        // shares the arm rather than getting an `unreachable!()`, which would be
        // a `panic!` in production code. Uncertain is the safe reading of it
        // anyway: nothing was sent, so nothing is claimed. The state machine
        // refuses the move over the terminal row it would land on.
        RecipientOutcome::Uncertain
        | RecipientOutcome::NotEmitted
        | RecipientOutcome::AlreadyPresent => {
            let state = if !last_attempt && uncertain_is_retryable {
                MessageState::Sent
            } else {
                MessageState::Failed
            };

            MessageStateUpdate::new(client_message_id, state).responded_at(responded_at)
        }
    }
}

/// Folds the slots into the report.
fn report<S: SmscSession>(
    session: &S,
    batch: &Batch,
    slots: Vec<Slot>,
    fallback: Option<FallbackReason>,
    smsc_message_id: Option<String>,
    journalled: bool,
    reemitted_unanswered: usize,
) -> BatchReport {
    BatchReport {
        session_id: session.session_id(),
        smsc_message_id,
        fallback,
        journalled,
        reemitted_unanswered,
        // Zipped, so the report can only ever be as long as the shorter of the
        // two — and both are built from `batch.recipients`, so it is exactly as
        // long as the batch. That is CA-010-08's "without losing a recipient".
        recipients: batch
            .recipients
            .iter()
            .zip(slots)
            .map(|(recipient, slot)| RecipientReport {
                client_message_id: recipient.client_message_id,
                destination: recipient.destination.number().clone(),
                outcome: slot.outcome,
                via: slot.via,
            })
            .collect(),
    }
}

/// The `destination_addr` of every SME the PDU names, in order.
///
/// A distribution-list entry has no address of its own and is skipped; this
/// crate never builds one, and a PDU that carries one did not come from here.
#[must_use]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "only the tests read the PDU back")
)]
fn destinations_of(pdu: &SubmitMulti) -> Vec<String> {
    pdu.dest_address()
        .iter()
        .filter_map(|address| match address {
            DestAddress::SmeAddress(sme) => Some(sme.destination_addr.as_str().to_owned()),
            DestAddress::DistributionListName(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{
        build_submit_multi, match_refusals, read_multi_response, Batch, BatchRecipient,
        BatchSender, FallbackReason, MultiResponse, MultiSupport, MultiSupportState,
        RecipientOutcome, Refusal, Via, MAX_DESTINATIONS,
    };
    use crate::addressing::Destination;
    use crate::segmentation::{segment, ConcatenationReference, SegmentationOptions};
    use crate::submit::{SubmitBuildError, SubmitOptions};
    use core::str::FromStr as _;
    use smpp_core::codec::{Command, Pdu};
    use smpp_core::octets::COctetString;
    use smpp_core::pdus::{SubmitMultiResp, SubmitSmResp};
    use smpp_core::values::{CommandStatus, Npi, Ton, UnsuccessSme};

    fn one_segment(text: &str) -> crate::segmentation::Segment {
        segment(
            text,
            &SegmentationOptions::default(),
            ConcatenationReference::new(7),
        )
        .expect("the fixture text encodes")
        .into_segments()
        .into_iter()
        .next()
        .expect("at least one segment")
    }

    fn destination(raw: &str) -> Destination {
        Destination::parse(raw).expect("the fixture is a valid number")
    }

    fn options() -> SubmitOptions {
        SubmitOptions::to(destination("+2250700000000"))
    }

    #[test]
    fn every_recipient_of_the_batch_reaches_the_pdu_in_order() {
        let recipients = [destination("+2250700000001"), destination("+2250700000002")];

        let pdu = build_submit_multi(&options(), &recipients, &one_segment("Bonjour"))
            .expect("the fixture builds");

        assert_eq!(pdu.number_of_dests(), 2);
        assert_eq!(
            super::destinations_of(&pdu),
            vec![String::from("2250700000001"), String::from("2250700000002")]
        );
    }

    #[test]
    fn a_batch_with_no_recipient_is_refused() {
        assert_eq!(
            build_submit_multi(&options(), &[], &one_segment("Bonjour"))
                .expect_err("no destinations"),
            SubmitBuildError::NoDestinations
        );
    }

    // --- reading the answer -------------------------------------------------

    fn multi_resp(identifier: &str, refused: Vec<UnsuccessSme>) -> Pdu {
        Pdu::SubmitMultiResp(SubmitMultiResp::new(
            COctetString::from_str(identifier).expect("the fixture fits"),
            refused,
            Vec::new(),
        ))
    }

    fn unsuccess(number: &str, status: CommandStatus) -> UnsuccessSme {
        UnsuccessSme::new(
            Ton::International,
            Npi::Isdn,
            COctetString::from_str(number).expect("the fixture fits"),
            status,
        )
    }

    #[test]
    fn a_readable_response_yields_the_identifier_and_the_refusals() {
        let response = Command::new(
            CommandStatus::EsmeRok,
            1,
            multi_resp(
                "batch-1",
                vec![unsuccess("2250700000002", CommandStatus::EsmeRinvdstadr)],
            ),
        );

        assert_eq!(
            read_multi_response(&response),
            MultiResponse::Answered {
                smsc_message_id: Some(String::from("batch-1")),
                refused: vec![Refusal {
                    destination: String::from("2250700000002"),
                    status: CommandStatus::EsmeRinvdstadr,
                }],
            }
        );
    }

    /// THE fallback trigger: the message centre does not know the operation.
    #[test]
    fn an_invalid_command_id_is_read_as_a_message_centre_without_submit_multi() {
        let response = Command::new(
            CommandStatus::EsmeRinvcmdid,
            1,
            multi_resp("unused", Vec::new()),
        );

        assert_eq!(
            read_multi_response(&response),
            MultiResponse::Unsupported {
                status: CommandStatus::EsmeRinvcmdid
            }
        );
        assert!(read_multi_response(&response).triggers_fallback());
    }

    /// The other shape of the same answer: a message centre that cannot route
    /// the operation at all nacks it rather than answering `submit_multi_resp`.
    ///
    /// The status is deliberately **not** `ESME_RINVCMDID`: a nack carrying it
    /// would be read as unsupported by the status rule alone, and the test would
    /// pass with the `generic_nack` rule deleted. Message centres do send a
    /// generic error here.
    #[test]
    fn a_generic_nack_is_read_as_a_message_centre_without_submit_multi() {
        let response = Command::new(CommandStatus::EsmeRunknownerr, 1, Pdu::GenericNack);

        assert_ne!(response.status(), CommandStatus::EsmeRinvcmdid);
        assert_eq!(
            read_multi_response(&response),
            MultiResponse::Unsupported {
                status: CommandStatus::EsmeRunknownerr
            }
        );
    }

    /// Answering "slow down" with 254 individual submissions is the opposite of
    /// what the message centre asked for.
    #[test]
    fn throttling_is_a_refusal_and_never_a_fallback() {
        for status in [CommandStatus::EsmeRthrottled, CommandStatus::EsmeRmsgqful] {
            let response = Command::new(status, 1, multi_resp("unused", Vec::new()));

            assert_eq!(
                read_multi_response(&response),
                MultiResponse::Refused { status }
            );
            assert!(!read_multi_response(&response).triggers_fallback());
        }
    }

    /// `ESME_ROK` over a body this client cannot read claims nothing: the
    /// message centre may well have taken every recipient.
    #[test]
    fn an_unreadable_body_claims_nothing_and_does_not_fall_back() {
        let response = Command::new(
            CommandStatus::EsmeRok,
            1,
            Pdu::SubmitSmResp(SubmitSmResp::new(COctetString::empty(), Vec::new())),
        );

        assert_eq!(read_multi_response(&response), MultiResponse::Unreadable);
        assert!(!read_multi_response(&response).triggers_fallback());
    }

    // --- matching the refusals back onto the batch --------------------------

    #[test]
    fn a_recipient_absent_from_the_refusals_was_accepted() {
        let batch = [destination("+2250700000001"), destination("+2250700000002")];
        let refused = [Refusal {
            destination: String::from("2250700000002"),
            status: CommandStatus::EsmeRinvdstadr,
        }];

        assert_eq!(
            match_refusals(&batch, &refused),
            Some(vec![None, Some(CommandStatus::EsmeRinvdstadr)])
        );
    }

    /// A message centre quoting the address back with its `+` is quoting the
    /// same subscriber. That difference is lossless, and it is the only one
    /// tolerated.
    #[test]
    fn a_refusal_quoting_the_number_with_its_plus_matches_the_same_recipient() {
        let batch = [destination("+2250700000001")];
        let refused = [Refusal {
            destination: String::from("+2250700000001"),
            status: CommandStatus::EsmeRinvdstadr,
        }];

        assert_eq!(
            match_refusals(&batch, &refused),
            Some(vec![Some(CommandStatus::EsmeRinvdstadr)])
        );
    }

    /// The dangerous case, and the reason this returns an `Option`.
    ///
    /// One entry naming somebody who is not in the batch means the refusals
    /// cannot be attributed. Reporting the others as accepted would mark a
    /// message the recipient never got as accepted, silently.
    #[test]
    fn a_refusal_naming_nobody_in_the_batch_voids_every_verdict() {
        let batch = [destination("+2250700000001"), destination("+2250700000002")];
        let refused = [Refusal {
            destination: String::from("2250700000009"),
            status: CommandStatus::EsmeRinvdstadr,
        }];

        assert_eq!(match_refusals(&batch, &refused), None);
    }

    /// J-1: two recipients the refusal cannot tell apart.
    ///
    /// `unsuccess_sme` names an address, and two entries of one batch may carry
    /// the same one — the same subscriber listed twice, or two destinations that
    /// differ only by a TON the answer does not repeat back usefully. Attributing
    /// the refusal to the first of them leaves the second looking accepted, which
    /// is the over-claim in its purest form: the message centre took nobody and
    /// one row says `ACCEPTED`.
    #[test]
    fn a_refusal_matching_two_recipients_voids_every_verdict() {
        let batch = [destination("+2250700000001"), destination("+2250700000001")];
        let refused = [Refusal {
            destination: String::from("2250700000001"),
            status: CommandStatus::EsmeRinvdstadr,
        }];

        assert_eq!(match_refusals(&batch, &refused), None);
    }

    /// The same digits under two TONs are two destinations — a short code and an
    /// international number — and `unsuccess_sme` does not let this client tell
    /// which one was refused.
    #[test]
    fn two_destinations_sharing_their_digits_void_every_verdict() {
        let batch = [
            Destination::parse_with("2250700000001", Ton::International, Npi::Isdn).expect("valid"),
            Destination::parse_with("2250700000001", Ton::NetworkSpecific, Npi::Unknown)
                .expect("valid"),
        ];
        let refused = [Refusal {
            destination: String::from("2250700000001"),
            status: CommandStatus::EsmeRinvdstadr,
        }];

        assert_eq!(match_refusals(&batch, &refused), None);
    }

    /// A batch with a repeated recipient and **no** refusal is not ambiguous:
    /// the message centre took everybody, and there is nothing to attribute.
    #[test]
    fn a_repeated_recipient_is_only_a_problem_when_a_refusal_names_it() {
        let batch = [destination("+2250700000001"), destination("+2250700000001")];

        assert_eq!(match_refusals(&batch, &[]), Some(vec![None, None]));
    }

    /// M-2: the same recipient refused twice, with two different reasons. Which
    /// one is the verdict? Nothing here can say, so nothing is claimed — the
    /// alternative silently kept whichever arrived last.
    #[test]
    fn two_refusals_naming_the_same_recipient_void_every_verdict() {
        let batch = [destination("+2250700000001"), destination("+2250700000002")];
        let refused = [
            Refusal {
                destination: String::from("2250700000001"),
                status: CommandStatus::EsmeRinvdstadr,
            },
            Refusal {
                destination: String::from("+2250700000001"),
                status: CommandStatus::EsmeRthrottled,
            },
        ];

        assert_eq!(match_refusals(&batch, &refused), None);
    }

    #[test]
    fn more_refusals_than_recipients_voids_every_verdict() {
        let batch = [destination("+2250700000001")];
        let refused = [
            Refusal {
                destination: String::from("2250700000001"),
                status: CommandStatus::EsmeRinvdstadr,
            },
            Refusal {
                destination: String::from("2250700000001"),
                status: CommandStatus::EsmeRsubmitfail,
            },
        ];

        assert_eq!(match_refusals(&batch, &refused), None);
    }

    #[test]
    fn a_batch_beyond_the_protocol_ceiling_is_refused() {
        let recipients: Vec<Destination> = (0..=MAX_DESTINATIONS)
            .map(|index| destination(&format!("+225{:010}", 7_000_000_000_u64 + index as u64)))
            .collect();

        assert_eq!(
            build_submit_multi(&options(), &recipients, &one_segment("Bonjour"))
                .expect_err("too many destinations"),
            SubmitBuildError::TooManyDestinations {
                maximum: MAX_DESTINATIONS
            }
        );
    }

    // --- the batch send path ------------------------------------------------

    use crate::campaign::resume::{message_key, UnansweredPolicy};
    use crate::message::MessageState;
    use crate::ports::{MessageRepository as _, MessageStoreError, SmscSession as _, SubmitError};
    use crate::sender::Sender;
    use crate::testing::{journal_row, FakeSmsc, FixedClock, MemoryJournal, MultiReply, Refused};
    use crate::MessagingError;
    use smpp_core::types::{CampaignId, ClientMessageId, SessionId};

    fn campaign() -> CampaignId {
        CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000001").expect("a valid UUID")
    }

    fn numbers(count: usize) -> Vec<String> {
        (1..=count)
            .map(|index| format!("+225{:010}", 7_000_000_000_u64 + index as u64))
            .collect()
    }

    fn batch_of(count: usize) -> Batch {
        let recipients = numbers(count)
            .iter()
            .map(|raw| {
                let destination = destination(raw);

                BatchRecipient {
                    client_message_id: message_key(campaign(), destination.number()),
                    destination,
                }
            })
            .collect();

        Batch::new("Bonjour", options(), recipients).in_campaign(campaign())
    }

    /// The write-ahead of CLAUDE.md §4, for a batch: 254 recipients are 254
    /// rows, and every one of them is committed before the PDU leaves.
    #[tokio::test]
    async fn every_recipient_is_journalled_before_the_batch_reaches_the_socket() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting();
        let batch = batch_of(3);

        journal.witness_transitions(smsc.clone()).await;

        let support = MultiSupport::for_session(smsc.session_id());

        BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(
            journal.submissions_at_first_transition().await,
            Some(0),
            "a PDU crossed the wire before the attempts were journalled"
        );
        assert_eq!(journal.inserted().await, 3, "one row per recipient");
        assert_eq!(smsc.multi_submitted(), 1, "one PDU for the three of them");
    }

    /// CA-010-08, the criterion itself.
    #[tokio::test]
    async fn a_message_centre_without_submit_multi_falls_back_without_losing_a_recipient() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting()
            .recording()
            .answering_multi(MultiReply::Unsupported);
        let batch = batch_of(3);

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert!(matches!(
            report.fallback,
            Some(FallbackReason::OperationRefused { .. })
        ));
        assert_eq!(report.recipients.len(), 3, "no recipient was dropped");
        assert!(report.recipients.iter().all(
            |entry| entry.outcome == RecipientOutcome::Accepted && entry.via == Via::Individual
        ));

        assert_eq!(smsc.multi_submitted(), 1);
        assert_eq!(
            smsc.accepted_destinations().await.len(),
            3,
            "each recipient got exactly one accepted submit_sm"
        );

        for row in journal.rows().await {
            assert_eq!(row.state, MessageState::Accepted);
        }
    }

    /// A message centre that has refused the operation once will refuse it
    /// again; asking 2 000 more times is 2 000 wasted round trips.
    #[tokio::test]
    async fn the_operation_is_not_attempted_again_once_it_has_been_refused() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        // The script answers the FIRST submit_multi with a refusal and would
        // accept any later one — so a second attempt would be visible.
        let smsc = FakeSmsc::accepting().multi_scripted([MultiReply::Unsupported]);

        let support = MultiSupport::for_session(smsc.session_id());

        let batcher = BatchSender::new(&sender, &support);

        batcher
            .submit_batch(&smsc, &batch_of(2))
            .await
            .expect("the first batch is sent");

        let second = Batch::new(
            "Bonjour",
            options(),
            numbers(4)
                .iter()
                .skip(2)
                .map(|raw| {
                    let destination = destination(raw);

                    BatchRecipient {
                        client_message_id: message_key(campaign(), destination.number()),
                        destination,
                    }
                })
                .collect(),
        )
        .in_campaign(campaign());

        let report = batcher
            .submit_batch(&smsc, &second)
            .await
            .expect("the second batch is sent");

        assert_eq!(smsc.multi_submitted(), 1, "the operation was tried twice");
        assert_eq!(report.fallback, Some(FallbackReason::KnownUnsupported));
        assert_eq!(report.recipients.len(), 2);
    }

    /// Answering "slow down" with 254 individual submissions is the opposite of
    /// what the message centre asked for.
    #[tokio::test]
    async fn a_throttled_batch_is_not_replayed_one_message_at_a_time() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting()
            .answering_multi(MultiReply::Refused(CommandStatus::EsmeRthrottled));
        let batch = batch_of(3).with_more_attempts_allowed(true);

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(report.fallback, None, "throttling is not a fallback");
        assert_eq!(smsc.submitted(), 1, "nothing was re-sent");
        assert!(report.recipients.iter().all(|entry| entry.outcome
            == RecipientOutcome::Rejected {
                status: CommandStatus::EsmeRthrottled
            }));

        for row in journal.rows().await {
            // The campaign may replay it, so the row is not terminal yet — the
            // same rule `Sender::final_transition` applies.
            assert_eq!(row.state, MessageState::Sent);
            assert_eq!(row.command_status, Some(CommandStatus::EsmeRthrottled));
        }
    }

    /// The heart of the piece: `submit_multi_resp` is neither a success nor a
    /// failure, and the journal has to carry the difference recipient by
    /// recipient.
    #[tokio::test]
    async fn a_partially_successful_batch_gives_every_recipient_its_own_verdict() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let batch = batch_of(3);
        let refused = batch
            .recipients
            .get(1)
            .expect("three recipients")
            .destination
            .number()
            .as_str()
            .to_owned();

        let smsc = FakeSmsc::accepting().answering_multi(MultiReply::Accepted {
            refused: vec![Refused::plain(refused, CommandStatus::EsmeRinvdstadr)],
        });

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(
            report
                .recipients
                .iter()
                .map(|entry| entry.outcome.clone())
                .collect::<Vec<_>>(),
            vec![
                RecipientOutcome::Accepted,
                RecipientOutcome::Rejected {
                    status: CommandStatus::EsmeRinvdstadr
                },
                RecipientOutcome::Accepted,
            ]
        );

        for (index, recipient) in batch.recipients.iter().enumerate() {
            let row = journal
                .row(recipient.client_message_id)
                .await
                .expect("the row is there");

            if index == 1 {
                assert_eq!(row.state, MessageState::Failed);
                assert_eq!(row.command_status, Some(CommandStatus::EsmeRinvdstadr));
            } else {
                assert_eq!(row.state, MessageState::Accepted);
                assert_eq!(row.command_status, Some(CommandStatus::EsmeRok));
            }
        }
    }

    /// THE consequence of a batch: one identifier for N messages, so no row may
    /// carry it. See the module header.
    #[tokio::test]
    async fn no_row_carries_the_identifier_the_whole_batch_shares() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting();

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch_of(3))
            .await
            .expect("the batch is sent");

        let shared = report
            .smsc_message_id
            .clone()
            .expect("the message centre did assign one");

        assert_eq!(report.accepted(), 3);

        for row in journal.rows().await {
            assert_eq!(
                row.smsc_message_id, None,
                "a shared identifier on a row would credit one recipient's \
                 receipt to another"
            );
        }

        // The consequence, stated where it bites: this is the lookup
        // `crate::correlation` performs for every delivery receipt, and it finds
        // nothing. The receipts of a batched message are orphans — see the
        // module header.
        assert_eq!(
            journal
                .find_message_by_smsc_id(&shared, None)
                .await
                .expect("the journal answers"),
            None
        );
    }

    /// A refusal naming somebody outside the batch means no verdict can be
    /// attributed. Nothing is claimed — the rows land in the uncertain family
    /// of ADR 0014 rather than being reported accepted.
    #[tokio::test]
    async fn a_refusal_that_cannot_be_attributed_leaves_every_recipient_uncertain() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting().answering_multi(MultiReply::Accepted {
            refused: vec![Refused::plain(
                "2259999999999",
                CommandStatus::EsmeRinvdstadr,
            )],
        });

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch_of(3).with_more_attempts_allowed(true))
            .await
            .expect("the batch is sent");

        assert!(report
            .recipients
            .iter()
            .all(|entry| entry.outcome == RecipientOutcome::Uncertain));

        for row in journal.rows().await {
            assert_eq!(row.state, MessageState::Sent);
            assert_eq!(
                row.command_status, None,
                "no status was established, so none is journalled"
            );
        }
    }

    /// The failure this whole design exists to prevent, and the only one that
    /// would be **silent**.
    ///
    /// The message centre really refused the second recipient — the handset gets
    /// nothing — but quotes the address back in a form this client does not
    /// recognise. If unmatched refusals were dropped instead of voiding the
    /// batch, all three recipients would look absent from the refusal list, all
    /// three rows would be journalled `ACCEPTED`, and nothing anywhere would say
    /// otherwise until somebody queried the delivery rate weeks later.
    #[tokio::test]
    async fn a_recipient_refused_under_an_unrecognisable_address_is_never_reported_accepted() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let batch = batch_of(3);
        let refused = batch
            .recipients
            .get(1)
            .expect("three recipients")
            .destination
            .number()
            .as_str()
            .to_owned();

        let smsc = FakeSmsc::accepting()
            .recording()
            .answering_multi(MultiReply::Accepted {
                refused: vec![Refused::quoted_as(
                    refused.clone(),
                    format!("00{refused}"),
                    CommandStatus::EsmeRinvdstadr,
                )],
            });

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch.clone().with_more_attempts_allowed(true))
            .await
            .expect("the batch is sent");

        assert_eq!(
            report.accepted(),
            0,
            "an unattributable refusal must not leave anybody reported accepted"
        );
        assert!(report
            .recipients
            .iter()
            .all(|entry| entry.outcome == RecipientOutcome::Uncertain));

        for row in journal.rows().await {
            assert_ne!(row.state, MessageState::Accepted);
        }
    }

    /// J-1, end to end and at the only level that matters: the message centre
    /// took **nobody**, so nothing may be reported accepted.
    ///
    /// Two recipients, one subscriber, two distinct write-ahead keys — which is
    /// what a caller outside the campaign path can build, since
    /// [`BatchRecipient`] carries its key rather than deriving it. Attributing
    /// the refusal to the first left the second reported `Accepted` and its row
    /// `ACCEPTED`, for a batch the centre refused whole.
    #[tokio::test]
    async fn a_subscriber_listed_twice_and_refused_twice_is_never_reported_accepted() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let twice = destination("+2250700000001");

        let batch = Batch::new(
            "Bonjour",
            options(),
            vec![
                BatchRecipient {
                    client_message_id: ClientMessageId::new(),
                    destination: twice.clone(),
                },
                BatchRecipient {
                    client_message_id: ClientMessageId::new(),
                    destination: twice.clone(),
                },
            ],
        );

        let smsc = FakeSmsc::accepting()
            .recording()
            .answering_multi(MultiReply::Accepted {
                refused: vec![
                    Refused::plain(twice.number().as_str(), CommandStatus::EsmeRinvdstadr),
                    Refused::plain(twice.number().as_str(), CommandStatus::EsmeRinvdstadr),
                ],
            });

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert!(
            smsc.accepted_destinations().await.is_empty(),
            "the message centre took nobody"
        );
        assert_eq!(report.accepted(), 0);

        for row in journal.rows().await {
            assert_ne!(row.state, MessageState::Accepted);
        }
    }

    /// M-1: an answer this client cannot attribute is a session it must stop
    /// batching.
    ///
    /// Not folding *this* batch is right — the `submit_multi` left and may have
    /// been taken. Batching the *next* one is not: the centre will quote its
    /// refusals the same way again, so every later batch would burn 254 rows on
    /// an answer nothing can be read from, for ever.
    #[tokio::test]
    async fn a_message_centre_whose_refusals_cannot_be_attributed_stops_being_batched() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let batch = batch_of(3);
        let refused = batch
            .recipients
            .get(1)
            .expect("three recipients")
            .destination
            .number()
            .as_str()
            .to_owned();

        let smsc = FakeSmsc::accepting().answering_multi(MultiReply::Accepted {
            refused: vec![Refused::quoted_as(
                refused.clone(),
                format!("00{refused}"),
                CommandStatus::EsmeRinvdstadr,
            )],
        });

        let support = MultiSupport::for_session(smsc.session_id());

        BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(
            support.state(),
            MultiSupportState::Unsupported,
            "an unusable answer must not leave the session marked batchable"
        );
        assert_eq!(smsc.multi_submitted(), 1);
    }

    /// The other half of M-1: an answer that **is** usable leaves the session
    /// batchable, so the latch does not disable batching at the first refused
    /// recipient.
    #[tokio::test]
    async fn a_message_centre_answering_usably_stays_batchable() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let batch = batch_of(3);
        let refused = batch
            .recipients
            .get(1)
            .expect("three recipients")
            .destination
            .number()
            .as_str()
            .to_owned();

        let smsc = FakeSmsc::accepting().answering_multi(MultiReply::Accepted {
            refused: vec![Refused::plain(refused, CommandStatus::EsmeRinvdstadr)],
        });

        let support = MultiSupport::for_session(smsc.session_id());

        BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(support.state(), MultiSupportState::Supported);
    }

    /// M-4: a latch belongs to one session, and the type says so.
    #[tokio::test]
    async fn a_latch_from_another_session_is_never_read_as_this_one() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let elsewhere = MultiSupport::for_session(SessionId::new());
        let smsc = FakeSmsc::accepting();

        assert_ne!(elsewhere.session_id(), smsc.session_id());

        let report = BatchSender::new(&sender, &elsewhere)
            .submit_batch(&smsc, &batch_of(3))
            .await
            .expect("the batch is sent");

        assert_eq!(report.fallback, Some(FallbackReason::ForeignLatch));
        assert_eq!(
            smsc.multi_submitted(),
            0,
            "what another session learned says nothing about this one"
        );
        assert_eq!(report.recipients.len(), 3);
    }

    /// J-3: what an uncertain batch leaves in the journal is **conditional**,
    /// and three doc comments used to state one half of it as if it were the
    /// whole.
    ///
    /// The default of [`Batch::new`] is `last_attempt`, so the ordinary caller
    /// gets `FAILED` — terminal, never re-read by `campaign::resume`, so ADR
    /// 0014's arbitration never runs for those rows. That is the same rule
    /// `Sender::final_transition` applies and it is defensible; asserting the
    /// opposite in the documentation was not.
    #[tokio::test]
    async fn an_uncertain_batch_writes_a_verdict_or_leaves_it_open_by_last_attempt() {
        async fn states(last_attempt: bool) -> Vec<(MessageState, Option<CommandStatus>)> {
            let journal = MemoryJournal::new();
            let sender = Sender::new(journal.clone(), FixedClock::default());
            let smsc = FakeSmsc::accepting().answering_multi(MultiReply::Unreadable);
            let support = MultiSupport::for_session(smsc.session_id());

            BatchSender::new(&sender, &support)
                .submit_batch(
                    &smsc,
                    &batch_of(2).with_more_attempts_allowed(!last_attempt),
                )
                .await
                .expect("the batch is sent");

            let mut states: Vec<(MessageState, Option<CommandStatus>)> = journal
                .rows()
                .await
                .into_iter()
                .map(|row| (row.state, row.command_status))
                .collect();

            states.sort_by_key(|(state, _)| *state);
            states
        }

        assert_eq!(
            states(false).await,
            vec![(MessageState::Sent, None); 2],
            "with attempts left the row stays in ADR 0014's uncertain family"
        );
        assert_eq!(
            states(true).await,
            vec![(MessageState::Failed, None); 2],
            "on a last attempt the verdict is rendered, and FAILED is terminal"
        );

        assert!(
            Batch::new("Bonjour", options(), Vec::new()).last_attempt,
            "the second case is the DEFAULT, which is what made the claim wrong"
        );
    }

    /// The duplicate the fallback must not create: a `submit_multi` that left
    /// and was never answered may have been taken for all 254 recipients.
    #[tokio::test]
    async fn an_unanswered_batch_is_not_replayed_one_message_at_a_time() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc =
            FakeSmsc::accepting().answering_multi(MultiReply::Failed(SubmitError::ResponseTimeout));

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch_of(3).with_more_attempts_allowed(true))
            .await
            .expect("the batch is sent");

        assert_eq!(report.fallback, None);
        assert_eq!(smsc.submitted(), 1, "nothing was re-sent inside this run");
        assert!(report
            .recipients
            .iter()
            .all(|entry| entry.outcome == RecipientOutcome::Uncertain));

        for row in journal.rows().await {
            assert_eq!(row.state, MessageState::Sent);
            assert_eq!(row.command_status, None);
        }
    }

    /// J-4: a submission the **session** refused before writing to the socket
    /// is not an unanswered one, and 254 recipients must not be reported as 254
    /// possible duplicates because of it.
    ///
    /// [`SubmitError::prevented_emission`] is part of the port's contract, not a
    /// hint: `NotBound` guarantees nothing was written. Reading it is what keeps
    /// the duplicate-risk figure of ADR 0014 — sized as "at most the send
    /// window" — from being multiplied by the batch size on a single
    /// reconnecting session.
    #[tokio::test]
    async fn a_batch_the_session_refused_before_the_socket_is_not_a_duplicate_risk() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc =
            FakeSmsc::accepting().answering_multi(MultiReply::Failed(SubmitError::NotBound {
                state: String::from("RECONNECT"),
            }));

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch_of(3).with_more_attempts_allowed(true))
            .await
            .expect("the batch is sent");

        assert!(report.recipients.iter().all(|entry| entry.outcome
            == RecipientOutcome::NotEmitted
            && entry.via == Via::Nothing));
        assert_eq!(
            report.at_risk_of_duplication(),
            0,
            "nothing left the socket, so nothing can arrive twice"
        );
        assert_eq!(report.fallback, None, "a submit_sm would be refused too");
    }

    /// The two failures are **not** the same fact, and the report says so: one
    /// may have been taken by the message centre, the other certainly was not.
    #[tokio::test]
    async fn a_timeout_and_a_pre_socket_refusal_are_not_reported_alike() {
        async fn outcomes(failure: SubmitError) -> (Vec<RecipientOutcome>, usize) {
            let journal = MemoryJournal::new();
            let sender = Sender::new(journal, FixedClock::default());
            let smsc = FakeSmsc::accepting().answering_multi(MultiReply::Failed(failure));
            let support = MultiSupport::for_session(smsc.session_id());

            let report = BatchSender::new(&sender, &support)
                .submit_batch(&smsc, &batch_of(2).with_more_attempts_allowed(true))
                .await
                .expect("the batch is sent");

            (
                report
                    .recipients
                    .iter()
                    .map(|entry| entry.outcome.clone())
                    .collect(),
                report.at_risk_of_duplication(),
            )
        }

        let (timed_out, timed_out_risk) = outcomes(SubmitError::ResponseTimeout).await;
        let (refused, refused_risk) = outcomes(SubmitError::OperationNotAllowed).await;

        assert_eq!(
            timed_out,
            vec![RecipientOutcome::Uncertain; 2],
            "a timeout leaves the batch's fate unknown"
        );
        assert_eq!(timed_out_risk, 2);

        assert_eq!(
            refused,
            vec![RecipientOutcome::NotEmitted; 2],
            "the port guarantees nothing was written to the socket"
        );
        assert_eq!(refused_risk, 0);
    }

    /// A bind that may not submit will not be able to a moment later either, so
    /// the verdict is final however many attempts the caller has left — the same
    /// classification [`crate::retry::SendFailure::is_retryable`] applies to a
    /// unit send.
    #[tokio::test]
    async fn a_bind_that_may_not_submit_is_a_final_verdict_for_every_recipient() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting()
            .answering_multi(MultiReply::Failed(SubmitError::OperationNotAllowed));

        let support = MultiSupport::for_session(smsc.session_id());

        BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch_of(3).with_more_attempts_allowed(true))
            .await
            .expect("the batch is sent");

        for row in journal.rows().await {
            assert_eq!(row.state, MessageState::Failed);
            assert_eq!(row.command_status, None, "no message centre answered");
        }
    }

    /// A `submit_multi` to one recipient is a `submit_sm` with more octets and
    /// a message centre that may not support it.
    #[tokio::test]
    async fn a_batch_of_one_is_sent_as_an_ordinary_submit_sm() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting();

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch_of(1))
            .await
            .expect("the batch is sent");

        assert_eq!(report.fallback, Some(FallbackReason::SingleRecipient));
        assert_eq!(smsc.multi_submitted(), 0);
        assert_eq!(smsc.submitted(), 1);
    }

    /// `submit_multi` carries one `short_message`, so a text that does not fit
    /// one PDU has no batched form at all.
    #[tokio::test]
    async fn a_text_that_needs_several_segments_is_sent_one_message_at_a_time() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting();

        let mut batch = batch_of(2);
        batch.text = "a".repeat(400);

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(report.fallback, Some(FallbackReason::MultipleSegments));
        assert_eq!(smsc.multi_submitted(), 0);
        assert_eq!(smsc.submitted(), 6, "two recipients, three segments each");
    }

    #[tokio::test]
    async fn a_disabled_batch_sender_never_puts_a_submit_multi_on_the_wire() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting();

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .enabled(false)
            .submit_batch(&smsc, &batch_of(3))
            .await
            .expect("the batch is sent");

        assert_eq!(report.fallback, Some(FallbackReason::Disabled));
        assert_eq!(smsc.multi_submitted(), 0);
        assert_eq!(smsc.submitted(), 3);
    }

    /// The write-ahead insert is the guard: a recipient whose row already
    /// exists is not batched, and not sent to.
    #[tokio::test]
    async fn a_recipient_whose_row_already_exists_is_reported_and_not_sent_to() {
        let journal = MemoryJournal::new();
        let batch = batch_of(3);
        let taken = batch
            .recipients
            .first()
            .expect("three recipients")
            .client_message_id;

        journal
            .force_row(journal_row(taken, MessageState::Accepted))
            .await;

        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting().recording();

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(
            report.recipients.len(),
            3,
            "every recipient is accounted for"
        );
        assert_eq!(
            report.recipients.first().map(|entry| entry.outcome.clone()),
            Some(RecipientOutcome::AlreadyPresent)
        );
        assert_eq!(
            smsc.destinations().await.len(),
            2,
            "the recipient that already had a row was not sent to"
        );
    }

    /// J-5: a row written by a run that then failed must not exclude its
    /// recipient for ever.
    ///
    /// The inserts are one per recipient and not one transaction, so a failure
    /// at the *k*-th leaves `k − 1` rows `QUEUED` with no `SENT` transition.
    /// Nothing was emitted, so that is safe on the spot — but the retry of the
    /// same batch re-derives the same keys, the inserts conflict, and reporting
    /// those recipients `AlreadyPresent` means **they are never sent to at
    /// all**. Persisted and then forgotten, which is exactly what the property
    /// test's header names as the danger.
    #[tokio::test]
    async fn a_recipient_left_queued_by_a_failed_run_is_sent_by_the_next_one() {
        let journal = MemoryJournal::new();
        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting().recording();

        let support = MultiSupport::for_session(smsc.session_id());
        let batch = batch_of(3);

        // The second insert fails: recipient 0 keeps a QUEUED row, and nothing
        // reaches the message centre.
        journal.fail_inserts_from(Some(2)).await;

        BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect_err("the journal refused the second insert");

        assert_eq!(smsc.submitted(), 0, "nothing was sent by the failed run");
        assert_eq!(
            journal
                .row(
                    batch
                        .recipients
                        .first()
                        .expect("three recipients")
                        .client_message_id
                )
                .await
                .expect("the first row was written")
                .state,
            MessageState::Queued,
            "the fixture must actually leave an orphan row behind"
        );

        // …and the journal recovers.
        journal.fail_inserts_from(None).await;

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the second run goes through");

        assert_eq!(
            report
                .recipients
                .iter()
                .filter(|entry| entry.outcome == RecipientOutcome::AlreadyPresent)
                .count(),
            0,
            "a QUEUED row is a message that never left, not a reason to skip"
        );
        assert_eq!(smsc.accepted_destinations().await.len(), 3);

        for row in journal.rows().await {
            assert_eq!(row.state, MessageState::Accepted);
        }
    }

    /// The same guard on the individual path: the fallback must not lose the
    /// recipient a failed run left `QUEUED` either.
    #[tokio::test]
    async fn a_queued_recipient_is_also_recovered_on_the_individual_path() {
        let journal = MemoryJournal::new();
        let batch = batch_of(2);
        let stranded = batch
            .recipients
            .first()
            .expect("two recipients")
            .client_message_id;

        journal
            .force_row(journal_row(stranded, MessageState::Queued))
            .await;

        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting().recording();

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .enabled(false)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(smsc.accepted_destinations().await.len(), 2);
        assert_eq!(
            journal.row(stranded).await.expect("the row is there").state,
            MessageState::Accepted
        );
        assert_eq!(report.recipients.len(), 2);
    }

    /// CA-010-05 is not weakened by the recovery above: the guard reads the
    /// state, and an accepted message is still never sent again.
    #[tokio::test]
    async fn an_accepted_row_is_still_never_sent_again() {
        let journal = MemoryJournal::new();
        let batch = batch_of(3);
        let taken = batch
            .recipients
            .first()
            .expect("three recipients")
            .client_message_id;

        journal
            .force_row(journal_row(taken, MessageState::Accepted))
            .await;

        let sender = Sender::new(journal.clone(), FixedClock::default());
        let smsc = FakeSmsc::accepting().recording();

        let support = MultiSupport::for_session(smsc.session_id());

        let report = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch)
            .await
            .expect("the batch is sent");

        assert_eq!(
            report.recipients.first().map(|entry| entry.outcome.clone()),
            Some(RecipientOutcome::AlreadyPresent)
        );
        assert_eq!(smsc.destinations().await.len(), 2);
    }

    /// The arbitration of ADR 0014 reaches the batch path, and it is counted.
    ///
    /// A row left `SENT` with no `command_status` by a previous run may already
    /// have been taken. Under the default policy it is sent again and the batch
    /// **reports** how many such recipients there were; under `Abandon` it is
    /// left alone.
    #[tokio::test]
    async fn a_recipient_left_in_flight_is_replayed_and_counted() {
        async fn run(policy: UnansweredPolicy) -> (usize, usize, u64) {
            let journal = MemoryJournal::new();
            let batch = batch_of(2);
            let in_flight = batch
                .recipients
                .first()
                .expect("two recipients")
                .client_message_id;

            journal
                .force_row(journal_row(in_flight, MessageState::Sent))
                .await;

            let sender = Sender::new(journal.clone(), FixedClock::default());
            let smsc = FakeSmsc::accepting().recording();

            let support = MultiSupport::for_session(smsc.session_id());

            let report = BatchSender::new(&sender, &support)
                .on_unanswered(policy)
                .submit_batch(&smsc, &batch)
                .await
                .expect("the batch is sent");

            (
                report.accepted(),
                report.reemitted_unanswered,
                smsc.submitted(),
            )
        }

        assert_eq!(
            run(UnansweredPolicy::Reemit).await,
            (2, 1, 1),
            "both are sent in one submit_multi, and the risk is reported"
        );
        assert_eq!(
            run(UnansweredPolicy::Abandon).await,
            (1, 0, 1),
            "the in-flight one is left alone; the other still goes out"
        );
    }

    /// A journal that cannot be written stops the batch, and **nothing** is
    /// sent: emitting without the write-ahead row is the one thing the ordering
    /// of CLAUDE.md §4 forbids.
    #[tokio::test]
    async fn a_batch_the_journal_refuses_sends_nothing() {
        let journal = MemoryJournal::new().refusing_inserts(MessageStoreError::Unavailable {
            reason: String::from("the journal is unavailable"),
        });
        let sender = Sender::new(journal, FixedClock::default());
        let smsc = FakeSmsc::accepting();

        let support = MultiSupport::for_session(smsc.session_id());

        let refusal = BatchSender::new(&sender, &support)
            .submit_batch(&smsc, &batch_of(3))
            .await
            .expect_err("the journal refused");

        assert!(matches!(refusal, MessagingError::Store(_)));
        assert_eq!(smsc.submitted(), 0);
    }
}
