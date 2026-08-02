//! Resume, and the guard against a second emission (deliverable L-010-04).
//!
//! # THE INVARIANT OF THIS MILESTONE
//!
//! > **A campaign emits at most one message that the message centre accepts,
//! > per recipient — across pauses, retries, errors and cold restarts.**
//!
//! Everything in this module exists to hold it, and any later change to the
//! emission path has to preserve it. It rests on two mechanisms, and neither is
//! sufficient alone:
//!
//! 1. **One row per recipient, by construction.** The `client_message_id` of a
//!    campaign message is not drawn at random: it is [`message_key`], a pure
//!    function of the campaign and the recipient's number. Two attempts to feed
//!    the same person produce the *same* key, so the second write-ahead insert
//!    hits the primary key of `messages` and comes back
//!    [`MessageStoreError::Conflict`](crate::ports::MessageStoreError::Conflict).
//!    The uniqueness is enforced by the database, not by a check in a loop —
//!    which is why it survives a process that died mid-campaign.
//! 2. **A state check before emitting.** A row that exists is not a licence to
//!    stay silent, nor to send: [`UnansweredPolicy::admit`] reads its state and
//!    answers. A message already `ACCEPTED` is never emitted again (CA-010-05);
//!    a `QUEUED` one never left and is emitted; a `SENT` one is the arbitration
//!    below.
//!
//! What the invariant deliberately does **not** say is "at most one emission".
//! Spec §10.7 retries a message the centre refused or never answered, so a
//! recipient may well see two `submit_sm` — what they may not see is a second
//! *accepted* one. That is the property the runner's property test asserts, over
//! arbitrary sequences of pause, resume, failure, timeout and restart.
//!
//! # The arbitration: a `SENT` message whose response never came
//!
//! Fiche §6 calls this a product decision, and it is. At the moment of a
//! `kill -9`, a row in `SENT` means the `submit_sm` left this process and no
//! response was journalled. The message centre may have accepted it and lost the
//! answer, or never seen it. **SMPP has no way to ask**: there is no idempotency
//! key on `submit_sm`, and `query_sm` takes the `message_id` the answer we never
//! got would have carried.
//!
//! So there are exactly two policies, and this module names both:
//!
//! | | [`UnansweredPolicy::Reemit`] (default) | [`UnansweredPolicy::Abandon`] |
//! |---|---|---|
//! | Risk | one recipient receives the message twice | one recipient receives nothing |
//! | Visible to | the recipient, who reads it twice | nobody |
//! | Counted as | an emission, in `attempts` | a skip, in the campaign report |
//!
//! **The default is to replay**, for three reasons stated in the order they
//! weighed:
//!
//! * ENF-FIA-01 asks for no message to be lost, and spec §10.5 says in so many
//!   words that a resume "restarts from the messages in `QUEUED`/`SENT` not
//!   confirmed" — `SENT` is in the list;
//! * it is the trade this codebase has already made everywhere else. The
//!   write-ahead order of [`crate::sender`] persists before sending precisely so
//!   that a crash duplicates rather than loses, and a resume that abandoned
//!   `SENT` rows would contradict the module it resumes;
//! * a duplicate is **visible and bounded** — the recipient sees it, the row
//!   carries `attempts >= 2`, and the campaign report says how many rows were in
//!   that position. A silent under-delivery is neither: nothing in the journal
//!   distinguishes a message that was never received from one that was.
//!
//! The window is narrow — it is the interval between the `submit_sm` leaving and
//! its response being committed, for the messages in flight at that instant, so
//! at most the size of the send window — but it is not zero, and this file says
//! so rather than implying otherwise.
//!
//! An operator who would rather under-deliver than duplicate switches the policy
//! to [`UnansweredPolicy::Abandon`], and nothing else in the runner changes.

use smpp_core::types::{CampaignId, ClientMessageId, Msisdn};
use uuid::Uuid;

use crate::message::{Message, MessageState};
use crate::ports::{MessageRepository, MessageStoreError};

/// The write-ahead key of one recipient of one campaign.
///
/// A UUID v5 — name-based, so **deterministic** — over the campaign identifier
/// as the namespace and the recipient's normalised number as the name.
///
/// # Why this is not a random identifier
///
/// The unit send path mints a v4 identifier per message, and that is right for
/// it: an operator sending the same text to the same number twice means to send
/// it twice. A campaign means the opposite. Deriving the key makes "this
/// recipient already has a message in this campaign" a question the *primary
/// key* answers, in the database, atomically — rather than a lookup the runner
/// performs and a crash can land in the middle of.
///
/// Two consequences, both intended:
///
/// * a resumed campaign re-derives exactly the identifiers it used before, which
///   is what makes CA-010-04 hold — the number of distinct
///   `client_message_id`s stays equal to the number of recipients however many
///   times the process restarts;
/// * a recipient appearing **twice** in one campaign's source is one recipient.
///   The second occurrence conflicts, and is reported as a skip rather than sent
///   a second copy.
///
/// The number is the *normalised* one ([`Msisdn`] parses to digits only), so the
/// same subscriber written `+225 07 …` and `+22507…` is one key.
#[must_use]
pub fn message_key(campaign_id: CampaignId, destination: &Msisdn) -> ClientMessageId {
    ClientMessageId::from_uuid(Uuid::new_v5(
        campaign_id.as_uuid(),
        destination.as_str().as_bytes(),
    ))
}

/// What a resume does with a message whose `submit_sm` left and whose response
/// never came.
///
/// See the module header: this is a product arbitration, and the two arms are
/// the two ways it can be settled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnansweredPolicy {
    /// Send it again, at the risk of a duplicate. **The default.**
    #[default]
    Reemit,

    /// Leave it alone, at the risk of a recipient who received nothing.
    Abandon,
}

/// Why a recipient is not emitted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SkipReason {
    /// The message centre already accepted this message (CA-010-05).
    AlreadyAccepted,
    /// The message already reached a terminal state.
    AlreadyTerminal,
    /// It was in flight when the process stopped, and the policy abandons it.
    Unanswered,
}

/// Whether a recipient may be emitted to, and as which attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Admission {
    /// No message exists: write it ahead, then send.
    Fresh,

    /// A message exists and has not been accepted: send it again **without**
    /// inserting.
    Resume {
        /// Attempts already recorded against it, so the next one is numbered
        /// after them.
        attempts_made: u32,

        /// Whether the row was left **in flight** rather than merely written.
        ///
        /// `true` for a `SENT` row, which is the arbitration of the module
        /// header: this emission may be the recipient's second copy. Carried on
        /// the decision rather than read again by the caller — it is a fact
        /// about the row the guard has just read, and asking the journal a
        /// second question to learn it would double the reads of a resume.
        was_unanswered: bool,
    },

    /// Do not send.
    Skip(SkipReason),
}

impl Admission {
    /// Whether anything goes on the wire for this recipient.
    #[must_use]
    pub const fn emits(self) -> bool {
        matches!(self, Self::Fresh | Self::Resume { .. })
    }
}

impl UnansweredPolicy {
    /// What to do about a recipient whose journal row is `existing`.
    ///
    /// A **pure** function of the row and the policy: no clock, no database, no
    /// ordering. That is what lets the property test of this milestone enumerate
    /// arbitrary event sequences and check the invariant on every one of them.
    #[must_use]
    pub fn admit(self, existing: Option<&Message>) -> Admission {
        let Some(message) = existing else {
            return Admission::Fresh;
        };

        // EXHAUSTIVE ON PURPOSE, with no `_` arm. A state added to spec §14.3
        // must be decided here rather than falling into whichever branch a
        // wildcard happened to point at — and the wrong fall-through is a second
        // emission to somebody who already received the message.
        match message.state {
            // Written, never sent: the crash landed between the insert and the
            // socket. Emitting cannot duplicate; not emitting loses it.
            MessageState::Queued => Admission::Resume {
                attempts_made: message.attempts,
                was_unanswered: false,
            },

            // The arbitration. See the module header.
            MessageState::Sent => match self {
                Self::Reemit => Admission::Resume {
                    attempts_made: message.attempts,
                    was_unanswered: true,
                },
                Self::Abandon => Admission::Skip(SkipReason::Unanswered),
            },

            // CA-010-05. Not negotiable by policy: the message centre said it
            // took it, so sending again is a duplicate with no upside at all.
            MessageState::Accepted => Admission::Skip(SkipReason::AlreadyAccepted),

            MessageState::Delivered | MessageState::Failed | MessageState::Expired => {
                Admission::Skip(SkipReason::AlreadyTerminal)
            }
        }
    }
}

/// The state check that stands between a recipient and a second message.
///
/// Thin by design: it reads the journal and applies [`UnansweredPolicy::admit`].
/// The decision is in the pure function so it can be tested exhaustively, and
/// the I/O is here so the runner has one call to make.
#[derive(Debug)]
pub struct EmissionGuard<'a, R> {
    repository: &'a R,
    policy: UnansweredPolicy,
}

impl<'a, R: MessageRepository> EmissionGuard<'a, R> {
    /// A guard over a journal, under a policy.
    #[must_use]
    pub const fn new(repository: &'a R, policy: UnansweredPolicy) -> Self {
        Self { repository, policy }
    }

    /// The policy this guard applies.
    #[must_use]
    pub const fn policy(&self) -> UnansweredPolicy {
        self.policy
    }

    /// Whether the message under `client_message_id` may be emitted.
    ///
    /// # Errors
    ///
    /// Whatever the journal returns. A read failure is propagated and **never**
    /// read as an admission: the alternative is bypassing the guard exactly when
    /// the database is in trouble, which is when a resumed campaign is most
    /// likely to be running.
    pub async fn admit(
        &self,
        client_message_id: ClientMessageId,
    ) -> Result<Admission, MessageStoreError> {
        let existing = self.repository.find_message(client_message_id).await?;

        Ok(self.policy.admit(existing.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{message_key, Admission, EmissionGuard, SkipReason, UnansweredPolicy};
    use crate::message::{Message, MessageState};
    use crate::testing::{journal_row, MemoryJournal};
    use smpp_core::types::{CampaignId, ClientMessageId, Msisdn};

    fn campaign() -> CampaignId {
        CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000001").expect("a valid UUID")
    }

    fn other_campaign() -> CampaignId {
        CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000002").expect("a valid UUID")
    }

    fn number(raw: &str) -> Msisdn {
        Msisdn::parse(raw).expect("the fixture is a valid number")
    }

    fn row(state: MessageState, attempts: u32) -> Message {
        let mut message = journal_row(ClientMessageId::new(), MessageState::Queued);

        message.state = state;
        message.attempts = attempts;
        message
    }

    // --- the write-ahead key ------------------------------------------------

    /// The whole of the resume design: the same campaign and the same recipient
    /// give the same key, in this process and in the one that restarts after a
    /// `kill -9`. Without it, a resumed campaign mints a second identifier for a
    /// person who already has a message and CA-010-04 fails by construction.
    #[test]
    fn the_write_ahead_key_is_the_same_in_every_process() {
        assert_eq!(
            message_key(campaign(), &number("+2250700000001")),
            message_key(campaign(), &number("+2250700000001"))
        );
    }

    #[test]
    fn two_recipients_of_one_campaign_have_different_keys() {
        assert_ne!(
            message_key(campaign(), &number("+2250700000001")),
            message_key(campaign(), &number("+2250700000002"))
        );
    }

    /// A person on two campaigns gets two messages, and they are two rows.
    #[test]
    fn one_recipient_on_two_campaigns_has_two_keys() {
        assert_ne!(
            message_key(campaign(), &number("+2250700000001")),
            message_key(other_campaign(), &number("+2250700000001"))
        );
    }

    /// The key is derived from the **normalised** number, so the same recipient
    /// written two ways is one recipient — which is what makes "at most one
    /// emission per recipient" a property of a person rather than of a spelling.
    #[test]
    fn the_same_number_written_two_ways_has_one_key() {
        assert_eq!(
            message_key(campaign(), &number("+225 07 00 00 00 01")),
            message_key(campaign(), &number("+2250700000001"))
        );
    }

    // --- the admission decision, as a pure function -------------------------

    #[test]
    fn a_recipient_with_no_row_is_admitted_fresh() {
        assert_eq!(
            UnansweredPolicy::default().admit(None),
            Admission::Fresh,
            "nothing has been persisted, so nothing has been emitted"
        );
    }

    /// CA-010-05, the criterion this whole module exists for.
    #[test]
    fn an_accepted_message_is_never_emitted_again() {
        for policy in [UnansweredPolicy::Reemit, UnansweredPolicy::Abandon] {
            assert_eq!(
                policy.admit(Some(&row(MessageState::Accepted, 1))),
                Admission::Skip(SkipReason::AlreadyAccepted),
                "under {policy:?}"
            );
        }
    }

    /// A message the message centre answered, or a receipt closed, is done.
    /// Re-emitting a `FAILED` one would replay a rejection the retry policy has
    /// already given up on (spec §10.7).
    #[test]
    fn a_terminal_message_is_never_emitted_again() {
        for state in [
            MessageState::Delivered,
            MessageState::Failed,
            MessageState::Expired,
        ] {
            for policy in [UnansweredPolicy::Reemit, UnansweredPolicy::Abandon] {
                assert_eq!(
                    policy.admit(Some(&row(state, 3))),
                    Admission::Skip(SkipReason::AlreadyTerminal),
                    "{state} under {policy:?}"
                );
            }
        }
    }

    /// A `QUEUED` row is a message that was written and never left: the crash
    /// landed between the insert and the socket. Nothing was emitted, so
    /// emitting now cannot duplicate anything — and not emitting would lose the
    /// message outright.
    #[test]
    fn a_queued_message_is_resumed_whatever_the_policy() {
        for policy in [UnansweredPolicy::Reemit, UnansweredPolicy::Abandon] {
            assert_eq!(
                policy.admit(Some(&row(MessageState::Queued, 0))),
                Admission::Resume {
                    attempts_made: 0,
                    was_unanswered: false,
                },
                "under {policy:?}"
            );
        }
    }

    /// The arbitration of fiche §6, third note. A `SENT` row is a message whose
    /// `submit_sm` left and whose response never arrived: the message centre may
    /// have taken it, or not, and no SMPP request can ask. The default replays
    /// it — losing a message is worse than sending one twice — and the other
    /// policy exists so the arbitration can be reversed without touching the
    /// runner.
    #[test]
    fn an_unanswered_message_follows_the_configured_policy() {
        let sent = row(MessageState::Sent, 1);

        assert_eq!(
            UnansweredPolicy::Reemit.admit(Some(&sent)),
            Admission::Resume {
                attempts_made: 1,
                was_unanswered: true,
            }
        );
        assert_eq!(
            UnansweredPolicy::Abandon.admit(Some(&sent)),
            Admission::Skip(SkipReason::Unanswered)
        );
    }

    #[test]
    fn replaying_is_the_default_arbitration() {
        assert_eq!(UnansweredPolicy::default(), UnansweredPolicy::Reemit);
    }

    /// The attempt number comes back with the decision because the journal
    /// stores `attempts` as `MAX(attempts, ?)`: a resumed message that reported
    /// attempt 1 again would leave the column stuck and the retry budget
    /// unreadable.
    #[test]
    fn a_resumed_message_carries_the_attempts_already_made() {
        assert_eq!(
            UnansweredPolicy::Reemit.admit(Some(&row(MessageState::Sent, 2))),
            Admission::Resume {
                attempts_made: 2,
                was_unanswered: true,
            }
        );
    }

    // --- the guard against a live journal -----------------------------------

    #[tokio::test]
    async fn the_guard_admits_a_recipient_the_journal_does_not_know() {
        let journal = MemoryJournal::new();
        let guard = EmissionGuard::new(&journal, UnansweredPolicy::default());

        assert_eq!(
            guard
                .admit(ClientMessageId::new())
                .await
                .expect("the journal answers"),
            Admission::Fresh
        );
    }

    #[tokio::test]
    async fn the_guard_refuses_a_recipient_whose_message_was_accepted() {
        let journal = MemoryJournal::new();
        let identifier = ClientMessageId::new();

        journal
            .force_row(journal_row(identifier, MessageState::Accepted))
            .await;

        let guard = EmissionGuard::new(&journal, UnansweredPolicy::default());

        assert_eq!(
            guard.admit(identifier).await.expect("the journal answers"),
            Admission::Skip(SkipReason::AlreadyAccepted)
        );
    }

    /// A journal that cannot be read is **not** an admission: emitting because
    /// the check failed is how the one guard CA-010-05 rests on gets bypassed
    /// exactly when the database is in trouble.
    #[tokio::test]
    async fn a_journal_failure_is_not_an_admission() {
        let journal = MemoryJournal::new().refusing_reads();
        let guard = EmissionGuard::new(&journal, UnansweredPolicy::default());

        assert!(guard.admit(ClientMessageId::new()).await.is_err());
    }

    #[test]
    fn an_admission_says_whether_anything_may_be_emitted() {
        assert!(Admission::Fresh.emits());
        assert!(Admission::Resume {
            attempts_made: 1,
            was_unanswered: false,
        }
        .emits());
        assert!(!Admission::Skip(SkipReason::AlreadyAccepted).emits());
        assert!(!Admission::Skip(SkipReason::AlreadyTerminal).emits());
        assert!(!Admission::Skip(SkipReason::Unanswered).emits());
    }
}
