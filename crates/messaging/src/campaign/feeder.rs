//! Reading the recipients and filling the send queue (deliverable L-010-02).
//!
//! ```text
//!   RecipientSource            bounded mpsc              the runner
//! ┌────────────────────┐      ┌────────────┐      ┌────────────────────┐
//! │ one recipient      │─────►│  256 max   │─────►│ guard, persist,    │
//! │ resolve variables  │      └────────────┘      │ submit, journal    │
//! │ build the key      │       back-pressure      └────────────────────┘
//! └────────────────────┘
//! ```
//!
//! # Back-pressure, end to end (spec §10.4)
//!
//! The queue is **bounded**, and the feeder offers items with `send().await`.
//! There is no intermediate buffer anywhere on this path — not a `Vec` of
//! rendered messages, not a batch, not a `collect()` — because a single
//! unbounded one would undo the whole arrangement (fiche §6). The chain is
//! therefore:
//!
//! > the message centre stops answering → the session's send window fills → the
//! > runner stops taking items → the queue fills → **the feeder stops reading
//! > the database.**
//!
//! What is bounded is what this process holds: one recipient in flight, at most
//! [`RECIPIENT_QUEUE_CAPACITY`] rendered messages in the queue, and the counters.
//! Nothing here is proportional to the number of recipients, which is CA-010-01.
//!
//! # Why this reader is asynchronous, and the deadlock that says why it matters
//!
//! Milestone 009's import reads a spreadsheet, which is blocking CPU work, so
//! its reader lives on `spawn_blocking` and offers rows with `blocking_send`.
//! That call does **not** watch a cancellation token — it returns only when the
//! receiver is closed — so cancelling an import whose queue was full deadlocked
//! the reader, and `contacts::import` has to `receiver.close()` before joining
//! it. The comment there says so at length.
//!
//! This feeder reads a **database stream**, which is asynchronous, so the push
//! is one arm of a `select!` whose other arm is the token: a feeder blocked on a
//! full queue is freed by a cancellation directly, with no closing dance and no
//! ordering requirement on the consumer. The test
//! `cancelling_frees_a_feeder_blocked_on_a_full_queue` is what holds the
//! difference, and it is the regression test for that bug in this shape.
//!
//! The consumer still must **drain before joining**, and that is a property of
//! the runner rather than of this file: a runner that awaited the feeder while
//! the queue was full would deadlock whatever the feeder does. See
//! [`super::runner`].
//!
//! # What crosses the queue
//!
//! A rendered message, or a rejection. Both, deliberately: a recipient the
//! template cannot be rendered for is an outcome the campaign has to count
//! (CA-010-02), and routing it through the same queue keeps the counting in one
//! place and in the source's order. It costs nothing — a rejection is not a
//! message and is never emitted.

use smpp_core::types::{CampaignId, ClientMessageId, Msisdn};
use smpp_core::values::{Npi, Ton};
use tokio::sync::mpsc;

use crate::addressing::{AddressError, Destination};
use crate::campaign::control::{ControlHandle, Resumption};
use crate::campaign::resume::message_key;
use crate::ports::{RecipientSource, RecipientSourceError};
use crate::template::{MissingVariablePolicy, RenderError, Template, Variables};

/// Rendered messages held between the reader and the emitter.
///
/// **Bounded** (CLAUDE.md §4). Large enough that the emitter is never starved
/// while the database serves the next rows, small enough that a message centre
/// that stops answering cannot let the feeder build a queue proportional to the
/// campaign — which is the failure CA-010-01 is about.
///
/// Smaller than the import's thousand rows, on purpose: an item here carries a
/// **rendered message**, up to a few hundred characters of text, where an import
/// row carries a number.
pub const RECIPIENT_QUEUE_CAPACITY: usize = 256;

/// Why a recipient produces no message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The recipient's number could not be turned into a destination address.
    #[error(transparent)]
    Address(#[from] AddressError),

    /// The template could not be rendered for this recipient (CA-010-06).
    #[error(transparent)]
    Render(#[from] RenderError),
}

/// A recipient no message was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRejection {
    /// Who was rejected. The number, never the message.
    pub destination: Msisdn,
    /// Why.
    pub reason: RejectionReason,
}

/// One message, ready to be persisted and sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    /// The write-ahead key of this recipient in this campaign
    /// ([`message_key`]).
    pub client_message_id: ClientMessageId,
    /// Where it goes.
    pub destination: Destination,
    /// The text, with every variable already resolved.
    pub text: String,
}

/// What crosses the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fed {
    /// A message to send.
    Ready(FeedItem),
    /// A recipient that produced none.
    Rejected(FeedRejection),
}

/// What one pass over the recipients produced.
///
/// Counters and one optional failure — nothing proportional to the number of
/// recipients, which is what lets a 500 000-recipient campaign report on itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedSummary {
    /// Recipients read from the source.
    pub read: u64,
    /// Items pushed into the queue, rejections included.
    ///
    /// Lower than [`Self::read`] only on a cancellation, where the item in the
    /// feeder's hand is dropped rather than pushed.
    pub queued: u64,
    /// Recipients no message was built for.
    pub rejected: u64,
    /// Whether the feeding stopped because the campaign was cancelled.
    pub cancelled: bool,
    /// Why the source stopped, when it did.
    ///
    /// A campaign whose source failed has **not** covered its recipients, and
    /// the runner reports it as `FAILED` rather than `COMPLETED`.
    pub failure: Option<RecipientSourceError>,
}

/// Reads the recipients of one campaign and fills the send queue.
///
/// Borrows its template and its policy rather than owning them: a campaign has
/// one of each and they outlive every run, and cloning a template per recipient
/// is the sort of copy that turns into a memory profile at half a million.
#[derive(Debug)]
pub struct Feeder<'a> {
    campaign_id: CampaignId,
    template: &'a Template,
    on_missing: &'a MissingVariablePolicy,
    ton: Ton,
    npi: Npi,
}

/// The policy applied when the campaign names none.
const DEFAULT_MISSING: MissingVariablePolicy = MissingVariablePolicy::Reject;

impl<'a> Feeder<'a> {
    /// A feeder for one campaign, over one template.
    ///
    /// Rejects a recipient whose variables cannot be resolved, which is the
    /// default of [`MissingVariablePolicy`] and the safe half of spec §10.2.
    #[must_use]
    pub const fn new(campaign_id: CampaignId, template: &'a Template) -> Self {
        Self {
            campaign_id,
            template,
            on_missing: &DEFAULT_MISSING,
            ton: Ton::International,
            npi: Npi::Isdn,
        }
    }

    /// The same feeder, under another missing-variable policy.
    #[must_use]
    pub const fn on_missing_variable(mut self, policy: &'a MissingVariablePolicy) -> Self {
        self.on_missing = policy;
        self
    }

    /// The same feeder, announcing recipients under another TON and NPI.
    ///
    /// The safe pair of spec §23.3 by default; an operator sending to short
    /// codes says otherwise.
    #[must_use]
    pub const fn addressed_as(mut self, ton: Ton, npi: Npi) -> Self {
        self.ton = ton;
        self.npi = npi;
        self
    }

    /// Reads every recipient and offers it to `sink`, until the source runs out,
    /// the campaign is cancelled, or the queue is closed.
    ///
    /// Returns rather than fails: a source that stops half-way is reported in
    /// [`FeedSummary::failure`], because the runner has to finish the messages
    /// it has already queued before deciding what the campaign's status is.
    #[tracing::instrument(skip_all, fields(campaign_id = %self.campaign_id))]
    pub async fn run<S: RecipientSource + ?Sized>(
        &self,
        source: &S,
        sink: mpsc::Sender<Fed>,
        mut control: ControlHandle,
    ) -> FeedSummary {
        use futures_util::StreamExt as _;

        let mut summary = FeedSummary::default();
        let mut recipients = source.stream_recipients();

        loop {
            // Pause is observed HERE, before a row is read, so a paused campaign
            // holds no recipient it has not accounted for.
            if control.wait_until_running().await == Resumption::Cancelled {
                summary.cancelled = true;
                break;
            }

            let next = tokio::select! {
                biased;

                () = control.cancelled() => {
                    summary.cancelled = true;
                    break;
                }
                next = recipients.next() => next,
            };

            let Some(recipient) = next else {
                break;
            };

            summary.read += 1;

            let recipient = match recipient {
                Ok(recipient) => recipient,
                Err(failure) => {
                    tracing::error!(error = %failure, "the recipient source stopped");

                    // Not counted as read: nothing came back.
                    summary.read -= 1;
                    summary.failure = Some(failure);
                    break;
                }
            };

            let item = match self.prepare(&recipient) {
                Ok(item) => Fed::Ready(item),
                Err(rejection) => {
                    summary.rejected += 1;

                    tracing::warn!(
                        reason = %rejection.reason,
                        "a recipient produced no message"
                    );

                    Fed::Rejected(rejection)
                }
            };

            // THE BACK-PRESSURE POINT, and the one place this task can be
            // parked for long. `send` on a full bounded queue waits, which is
            // exactly what is wanted — and it is one arm of a `select!` so a
            // cancellation frees it, which is the milestone-009 deadlock this
            // shape avoids (see the module header).
            let offered = tokio::select! {
                biased;

                () = control.cancelled() => {
                    summary.cancelled = true;
                    break;
                }
                result = sink.send(item) => result,
            };

            if offered.is_err() {
                // Nobody is reading any more: the runner stopped. Not an error,
                // and nothing left to do.
                tracing::debug!("the send queue is closed; the feeding stops");
                break;
            }

            summary.queued += 1;
        }

        tracing::info!(
            read = summary.read,
            queued = summary.queued,
            rejected = summary.rejected,
            cancelled = summary.cancelled,
            "the recipients have been fed"
        );

        summary
    }

    /// Turns one recipient into the message it will receive.
    ///
    /// Pure, and that is what keeps the loop above readable: everything that can
    /// reject a recipient happens here, before anything is queued and long
    /// before anything is persisted.
    fn prepare(&self, recipient: &crate::ports::Recipient) -> Result<FeedItem, FeedRejection> {
        let reject = |reason: RejectionReason| FeedRejection {
            destination: recipient.destination.clone(),
            reason,
        };

        let destination =
            Destination::parse_with(recipient.destination.as_str(), self.ton, self.npi)
                .map_err(|error| reject(RejectionReason::Address(error)))?;

        let variables = Variables::from_attributes(recipient.attributes.as_deref())
            .map_err(|error| reject(RejectionReason::Render(error)))?;

        let text = self
            .template
            .render(&variables, self.on_missing)
            .map_err(|error| reject(RejectionReason::Render(error)))?;

        Ok(FeedItem {
            client_message_id: message_key(self.campaign_id, &recipient.destination),
            destination,
            text,
        })
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{Fed, Feeder, RejectionReason, RECIPIENT_QUEUE_CAPACITY};
    use crate::campaign::control::CampaignControl;
    use crate::campaign::resume::message_key;
    use crate::ports::Recipient;
    use crate::template::{MissingVariablePolicy, Template};
    use crate::testing::{GeneratedRecipients, StaticRecipients};
    use core::time::Duration;
    use smpp_core::types::{CampaignId, Msisdn};
    use tokio::sync::mpsc;

    const SETTLE: Duration = Duration::from_millis(50);

    fn campaign() -> CampaignId {
        CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000001").expect("a valid UUID")
    }

    fn template(source: &str) -> Template {
        Template::parse(source).expect("the fixture template parses")
    }

    fn with_attributes(number: &str, attributes: &str) -> Recipient {
        Recipient {
            destination: Msisdn::parse(number).expect("the fixture is a valid number"),
            attributes: Some(attributes.to_owned()),
        }
    }

    /// Drains the queue while the feeder fills it, which is what the runner
    /// does. Never `await`s the feeder first: a consumer that joined before
    /// draining would deadlock the moment the queue filled up.
    async fn feed_all(
        feeder: &Feeder<'_>,
        source: &impl crate::ports::RecipientSource,
        control: &CampaignControl,
    ) -> (Vec<Fed>, super::FeedSummary) {
        let (sender, mut receiver) = mpsc::channel(RECIPIENT_QUEUE_CAPACITY);
        let mut collected = Vec::new();

        let (summary, ()) = tokio::join!(feeder.run(source, sender, control.handle()), async {
            while let Some(item) = receiver.recv().await {
                collected.push(item);
            }
        });

        (collected, summary)
    }

    #[tokio::test]
    async fn every_recipient_reaches_the_queue_with_its_own_message() {
        let template = template("Bonjour {{prenom}}");
        let feeder = Feeder::new(campaign(), &template);
        let source = StaticRecipients::new(vec![
            with_attributes("+2250700000001", r#"{"prenom":"Awa"}"#),
            with_attributes("+2250700000002", r#"{"prenom":"Koffi"}"#),
        ]);

        let (items, summary) = feed_all(&feeder, &source, &CampaignControl::new()).await;

        assert_eq!(summary.read, 2);
        assert_eq!(summary.queued, 2);
        assert_eq!(summary.rejected, 0);
        assert!(!summary.cancelled);

        let texts: Vec<String> = items
            .iter()
            .map(|item| match item {
                Fed::Ready(ready) => ready.text.clone(),
                Fed::Rejected(_) => String::from("<rejected>"),
            })
            .collect();

        assert_eq!(texts, vec!["Bonjour Awa", "Bonjour Koffi"]);
    }

    /// The key is the recipient's, not the queue position's: it is what a
    /// resumed campaign re-derives to find the row it already wrote.
    #[tokio::test]
    async fn each_item_carries_the_write_ahead_key_of_its_recipient() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source = StaticRecipients::numbers(&["+2250700000001"]);

        let (items, _) = feed_all(&feeder, &source, &CampaignControl::new()).await;

        let Some(Fed::Ready(item)) = items.first() else {
            panic!("the recipient is ready to send");
        };

        assert_eq!(
            item.client_message_id,
            message_key(
                campaign(),
                &Msisdn::parse("+2250700000001").expect("a valid number")
            )
        );
    }

    /// CA-010-06: a recipient whose variable cannot be resolved is rejected by
    /// name, and **no** text holding `{{…}}` is queued.
    #[tokio::test]
    async fn a_recipient_missing_a_variable_is_rejected_and_not_queued() {
        let template = template("Bonjour {{prenom}}");
        let feeder = Feeder::new(campaign(), &template);
        let source = StaticRecipients::new(vec![
            with_attributes("+2250700000001", r#"{"prenom":"Awa"}"#),
            with_attributes("+2250700000002", "{}"),
        ]);

        let (items, summary) = feed_all(&feeder, &source, &CampaignControl::new()).await;

        assert_eq!(summary.read, 2);
        assert_eq!(summary.queued, 2, "a rejection is reported, not dropped");
        assert_eq!(summary.rejected, 1);

        let rejected: Vec<&super::FeedRejection> = items
            .iter()
            .filter_map(|item| match item {
                Fed::Rejected(rejection) => Some(rejection),
                Fed::Ready(_) => None,
            })
            .collect();

        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].destination.as_str(), "2250700000002");
        assert!(matches!(rejected[0].reason, RejectionReason::Render(_)));

        for item in &items {
            if let Fed::Ready(ready) = item {
                assert!(!ready.text.contains("{{"), "{}", ready.text);
            }
        }
    }

    /// The other half of the policy: a default value keeps the recipient.
    #[tokio::test]
    async fn a_missing_variable_may_be_substituted_instead_of_rejected() {
        let template = template("Bonjour {{prenom}}");
        let policy = MissingVariablePolicy::Substitute(String::from("cher client"));
        let feeder = Feeder::new(campaign(), &template).on_missing_variable(&policy);
        let source = StaticRecipients::new(vec![with_attributes("+2250700000001", "{}")]);

        let (items, summary) = feed_all(&feeder, &source, &CampaignControl::new()).await;

        assert_eq!(summary.rejected, 0);

        let Some(Fed::Ready(item)) = items.first() else {
            panic!("the recipient is ready to send");
        };

        assert_eq!(item.text, "Bonjour cher client");
    }

    /// **Back-pressure, end to end.** The queue is bounded, so a consumer that
    /// reads nothing stops the feeder after exactly one queue's worth — it does
    /// not accumulate anywhere else, which is the failure fiche §6 names.
    #[tokio::test(start_paused = true)]
    async fn a_full_queue_stops_the_reader_rather_than_growing_a_buffer() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source = GeneratedRecipients::of(10_000);
        let control = CampaignControl::new();

        let (sender, mut receiver) = mpsc::channel(RECIPIENT_QUEUE_CAPACITY);

        // Nothing is read for the whole of this window.
        let feeding = tokio::time::timeout(SETTLE, feeder.run(&source, sender, control.handle()));

        assert!(
            feeding.await.is_err(),
            "a feeder facing a queue nobody drains must block, not finish"
        );

        // One queue's worth, and one more in the hand of the blocked `send`.
        let mut queued = 0;
        while receiver.try_recv().is_ok() {
            queued += 1;
        }

        assert!(
            queued <= RECIPIENT_QUEUE_CAPACITY + 1,
            "{queued} recipients were read ahead of a consumer that read none"
        );
    }

    /// CA-010-09, and the trap milestone 009 fell into: a feeder blocked on a
    /// **full** queue has to be woken by the cancellation, not by a consumer
    /// that will never come back.
    ///
    /// The import of milestone 009 read its rows on `spawn_blocking` and offered
    /// them with `blocking_send`, which does not watch a token — so cancelling
    /// while the queue was full deadlocked until the receiver was closed. This
    /// feeder is asynchronous and its push is one arm of a `select!`, so the
    /// token is watched by construction; this test is what says so.
    #[tokio::test(start_paused = true)]
    async fn cancelling_frees_a_feeder_blocked_on_a_full_queue() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source = GeneratedRecipients::of(10_000);
        let control = CampaignControl::new();

        // The receiver is deliberately kept alive and never read: the only way
        // out is the token.
        let (sender, _receiver) = mpsc::channel(RECIPIENT_QUEUE_CAPACITY);

        let (summary, ()) = tokio::join!(feeder.run(&source, sender, control.handle()), async {
            tokio::time::sleep(SETTLE).await;
            control.cancel();
        });

        assert!(summary.cancelled);
        assert!(summary.read < 10_000);
    }

    /// Pause stops the **feeding** (spec §10.3). What is already queued stays
    /// queued; nothing new is read.
    #[tokio::test(start_paused = true)]
    async fn a_paused_campaign_stops_reading_and_resumes_where_it_left_off() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source = GeneratedRecipients::of(20);
        let control = CampaignControl::new();

        let (sender, mut receiver) = mpsc::channel(RECIPIENT_QUEUE_CAPACITY);

        control.pause();

        let (summary, (during_pause, total)) =
            tokio::join!(feeder.run(&source, sender, control.handle()), async {
                tokio::time::sleep(SETTLE).await;

                let mut collected = 0;
                while receiver.try_recv().is_ok() {
                    collected += 1;
                }

                let during_pause = collected;

                control.resume();

                while receiver.recv().await.is_some() {
                    collected += 1;
                }

                (during_pause, collected)
            });

        assert_eq!(during_pause, 0, "a paused feeder reads nothing");
        assert_eq!(summary.read, 20);
        assert_eq!(total, 20, "nothing is lost across a pause");
    }

    #[tokio::test(start_paused = true)]
    async fn a_campaign_paused_before_it_starts_reads_nothing() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source = GeneratedRecipients::of(20);
        let control = CampaignControl::new();

        control.pause();

        let (sender, _receiver) = mpsc::channel(RECIPIENT_QUEUE_CAPACITY);

        assert!(
            tokio::time::timeout(SETTLE, feeder.run(&source, sender, control.handle()))
                .await
                .is_err(),
            "a paused feeder must not read ahead"
        );
    }

    /// A source that fails half-way is not a campaign that quietly completes:
    /// the failure is carried back so the runner can mark the campaign `FAILED`
    /// rather than `COMPLETED` with a third of the recipients missing.
    #[tokio::test]
    async fn a_source_that_fails_stops_the_feeding_and_says_so() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source =
            StaticRecipients::numbers(&["+2250700000001", "+2250700000002", "+2250700000003"])
                .failing_after(1);

        let (items, summary) = feed_all(&feeder, &source, &CampaignControl::new()).await;

        assert_eq!(items.len(), 1);
        assert_eq!(summary.queued, 1);
        assert_eq!(
            summary.read, 1,
            "the row that did not come back is not counted as read"
        );
        assert!(summary.failure.is_some());
        assert!(!summary.cancelled);
    }

    /// The consumer going away is not an error: it is a runner that stopped, and
    /// the feeder has nowhere to push. It must return rather than spin.
    #[tokio::test]
    async fn a_closed_queue_ends_the_feeding() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source = GeneratedRecipients::of(10_000);
        let control = CampaignControl::new();

        let (sender, receiver) = mpsc::channel(RECIPIENT_QUEUE_CAPACITY);

        drop(receiver);

        let summary = feeder.run(&source, sender, control.handle()).await;

        assert!(summary.read < 10_000);
        assert!(!summary.cancelled);
    }

    #[tokio::test]
    async fn an_empty_source_feeds_nothing_and_completes() {
        let template = template("Bonjour");
        let feeder = Feeder::new(campaign(), &template);
        let source = StaticRecipients::default();

        let (items, summary) = feed_all(&feeder, &source, &CampaignControl::new()).await;

        assert!(items.is_empty());
        assert_eq!(summary.read, 0);
        assert!(!summary.cancelled);
        assert!(summary.failure.is_none());
    }
}
