//! CA-010-08 as a property: **a batch never loses a recipient, and never
//! claims one the message centre did not take.**
//!
//! # The two halves, stated exactly
//!
//! `submit_multi` is partially successful by construction — one identifier, a
//! list of refusals — and it is answered in a dozen shapes: taken whole, taken
//! in part, refused for one reason or another, refused as an *operation*,
//! answered unreadably, not answered at all. Every one of those has to leave the
//! caller with a verdict per recipient, and the interesting failures are the two
//! opposite ones:
//!
//! 1. **Nothing is lost.** Every recipient of the batch has an entry in the
//!    report, and every recipient the batch wrote a row for leaves that row out
//!    of `QUEUED` — persisted and then forgotten is precisely the shape of a
//!    recipient who never receives anything and whom nothing counts.
//! 2. **Nothing is over-claimed.** A recipient reported `Accepted` was accepted
//!    by the message centre — checked against the centre's own record, not
//!    against what the code under test says about itself.
//!
//! The second is the one worth the file. The dangerous bug here is silent: a
//! message centre quoting refused addresses in a form this client does not
//! recognise makes every refusal unmatched, every recipient look absent from the
//! refusal list, and a whole batch of rejected messages journalled `ACCEPTED`.
//! No error, no log line, no failed message — just a delivery rate that is wrong
//! weeks later. `RefuseAlien` is the family that reaches it.
//!
//! # The generator is counted, not assumed
//!
//! `the_generator_reaches_the_families_the_property_can_break_in` samples the
//! strategy and asserts a floor on each family. Three sub-milestones of this
//! project have shipped a property test whose generator excluded, by
//! construction, the family the invariant fell in; a strategy that produced only
//! `AcceptAll` would pass every assertion here and prove nothing.

// `tests/` is compiled without `cfg(test)`, so the relaxations of `clippy.toml`
// do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: proptest is synchronous and the batch path is not,
//     so the property drives a runtime with `block_on`. A test harness is the
//     "binary entry point" the lint reserves it for.
//   · `panic`: the assertion helpers `expect`/`assert!` cover most of it, but
//     one check needs a message built from the scenario.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::collections::HashSet;

use messaging::addressing::Destination;
use messaging::campaign::resume::message_key;
use messaging::message::MessageState;
use messaging::ports::{MessageRepository as _, SubmitError};
use messaging::sender::Sender;
use messaging::submit::SubmitOptions;
use messaging::submit_multi::{
    Batch, BatchRecipient, BatchSender, FallbackReason, MultiSupport, RecipientOutcome, Via,
};
use messaging::testing::{journal_row, FakeSmsc, FixedClock, MemoryJournal, MultiReply, Refused};
use proptest::prelude::*;
use proptest::strategy::ValueTree as _;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use smpp_core::types::CampaignId;
use smpp_core::values::CommandStatus;

/// Recipients in the largest batch the generator builds.
///
/// Small on purpose: the families this property can break in are about the
/// *shape* of the answer, not about its size, and 254 recipients per case would
/// buy nothing but minutes. The protocol ceiling is covered by a unit test.
const MAX_BATCH: usize = 5;

fn campaign() -> CampaignId {
    CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000010").expect("a valid UUID")
}

fn number(index: usize) -> String {
    format!("+225{:010}", 7_000_000_000_u64 + index as u64)
}

/// A number no batch this file builds ever carries.
fn alien_number() -> String {
    String::from("2259999999999")
}

/// What the message centre does with the `submit_multi`.
#[derive(Debug, Clone)]
enum Answer {
    /// Every recipient taken.
    AcceptAll,
    /// These indices of the batch refused, each with its own status.
    RefuseSome(Vec<(usize, CommandStatus)>),
    /// A refusal naming somebody the batch does not carry.
    RefuseAlien,
    /// A real recipient refused, quoted back under an address this client
    /// cannot recognise.
    ///
    /// **The family the silent over-claim lives in**, and the only one where
    /// the message centre's truth and the PDU disagree: the recipient gets
    /// nothing, and a client that dropped the unmatched entry would report them
    /// accepted.
    RefuseUnrecognisably(usize),
    /// The whole PDU refused.
    RefusePdu(CommandStatus),
    /// The operation is unknown — the fallback trigger.
    Unsupported,
    /// `ESME_ROK` over a body that is not a `submit_multi_resp`.
    Unreadable,
    /// No answer at all.
    Silence(SubmitError),
}

/// One case: a batch, a message centre, and the settings around them.
#[derive(Debug, Clone)]
struct Scenario {
    size: usize,
    answer: Answer,
    enabled: bool,
    last_attempt: bool,
    long_text: bool,
    /// Indices whose row is already in the journal before the batch runs.
    taken: Vec<usize>,
}

fn any_status() -> impl Strategy<Value = CommandStatus> {
    prop_oneof![
        Just(CommandStatus::EsmeRthrottled),
        Just(CommandStatus::EsmeRmsgqful),
        Just(CommandStatus::EsmeRinvdstadr),
        Just(CommandStatus::EsmeRsubmitfail),
        Just(CommandStatus::EsmeRinvsrcadr),
    ]
}

fn any_submit_error() -> impl Strategy<Value = SubmitError> {
    prop_oneof![
        Just(SubmitError::ResponseTimeout),
        Just(SubmitError::Closed),
        Just(SubmitError::Transport {
            reason: String::from("the socket failed"),
        }),
        Just(SubmitError::NotBound {
            state: String::from("RECONNECT"),
        }),
    ]
}

fn any_answer(size: usize) -> impl Strategy<Value = Answer> {
    prop_oneof![
        2 => Just(Answer::AcceptAll),
        3 => proptest::collection::vec((0..size, any_status()), 1..=size)
            .prop_map(Answer::RefuseSome),
        2 => Just(Answer::RefuseAlien),
        3 => (0..size).prop_map(Answer::RefuseUnrecognisably),
        2 => any_status().prop_map(Answer::RefusePdu),
        5 => Just(Answer::Unsupported),
        1 => Just(Answer::Unreadable),
        2 => any_submit_error().prop_map(Answer::Silence),
    ]
}

fn any_scenario() -> impl Strategy<Value = Scenario> {
    (1..=MAX_BATCH).prop_flat_map(|size| {
        (
            Just(size),
            any_answer(size),
            // Batching off in one case out of five: the fallback that happens
            // before anything is written has to be covered too.
            proptest::bool::weighted(0.8),
            proptest::bool::weighted(0.5),
            proptest::bool::weighted(0.25),
            proptest::collection::vec(0..size, 0..=1),
        )
            .prop_map(
                |(size, answer, enabled, last_attempt, long_text, taken)| Scenario {
                    size,
                    answer,
                    enabled,
                    last_attempt,
                    long_text,
                    taken,
                },
            )
    })
}

/// What one case showed, for the census.
#[derive(Debug, Default)]
struct Seen {
    used_multi: bool,
    fell_back_after_emission: bool,
    fell_back_before_emission: bool,
    accepted: usize,
    rejected: usize,
    uncertain: usize,
    already_present: usize,
    /// The message centre refused somebody it did not name recognisably.
    hid_a_refusal: bool,
}

/// Runs one case and checks the property on it.
async fn exercise(scenario: &Scenario) -> Seen {
    let journal = MemoryJournal::new();
    let recipients: Vec<BatchRecipient> = (0..scenario.size)
        .map(|index| {
            let destination = Destination::parse(&number(index)).expect("a valid fixture number");

            BatchRecipient {
                client_message_id: message_key(campaign(), destination.number()),
                destination,
            }
        })
        .collect();

    for index in &scenario.taken {
        if let Some(recipient) = recipients.get(*index) {
            journal
                .force_row(journal_row(
                    recipient.client_message_id,
                    MessageState::Accepted,
                ))
                .await;
        }
    }

    let reply = match &scenario.answer {
        Answer::AcceptAll => MultiReply::Accepted {
            refused: Vec::new(),
        },
        Answer::RefuseSome(entries) => MultiReply::Accepted {
            refused: entries
                .iter()
                .filter_map(|(index, status)| {
                    recipients.get(*index).map(|recipient| {
                        Refused::plain(recipient.destination.number().as_str(), *status)
                    })
                })
                .collect(),
        },
        Answer::RefuseAlien => MultiReply::Accepted {
            refused: vec![Refused::plain(
                alien_number(),
                CommandStatus::EsmeRinvdstadr,
            )],
        },
        Answer::RefuseUnrecognisably(index) => MultiReply::Accepted {
            refused: recipients
                .get(*index)
                .map(|recipient| {
                    let number = recipient.destination.number().as_str();

                    vec![Refused::quoted_as(
                        number,
                        format!("00{number}"),
                        CommandStatus::EsmeRinvdstadr,
                    )]
                })
                .unwrap_or_default(),
        },
        Answer::RefusePdu(status) => MultiReply::Refused(*status),
        Answer::Unsupported => MultiReply::Unsupported,
        Answer::Unreadable => MultiReply::Unreadable,
        Answer::Silence(failure) => MultiReply::Failed(failure.clone()),
    };

    let smsc = FakeSmsc::accepting().recording().answering_multi(reply);
    let sender = Sender::new(journal.clone(), FixedClock::default());
    let support = MultiSupport::new();

    let text = if scenario.long_text {
        "a".repeat(400)
    } else {
        String::from("Bonjour")
    };

    let batch = Batch::new(
        text,
        SubmitOptions::to(Destination::parse("+2250700000000").expect("valid")),
        recipients.clone(),
    )
    .in_campaign(campaign())
    .with_more_attempts_allowed(!scenario.last_attempt);

    let report = BatchSender::new(&sender, &support)
        .enabled(scenario.enabled)
        .submit_batch(&smsc, &batch)
        .await
        .expect("a batch over a working journal is sent");

    // --- 1. nothing is lost -------------------------------------------------

    assert_eq!(
        report.recipients.len(),
        scenario.size,
        "the report lost a recipient: {scenario:?}"
    );
    assert_eq!(
        report
            .recipients
            .iter()
            .map(|entry| entry.client_message_id)
            .collect::<Vec<_>>(),
        recipients
            .iter()
            .map(|recipient| recipient.client_message_id)
            .collect::<Vec<_>>(),
        "the report reordered or replaced a recipient: {scenario:?}"
    );

    for recipient in &recipients {
        let row = journal
            .find_message(recipient.client_message_id)
            .await
            .expect("the journal answers")
            .unwrap_or_else(|| panic!("a recipient has no row at all: {scenario:?}"));

        assert_ne!(
            row.state,
            MessageState::Queued,
            "a recipient was written and then forgotten: {scenario:?}"
        );
    }

    // --- 2. nothing is over-claimed -----------------------------------------

    let taken_by_the_centre: Vec<String> = smsc.accepted_destinations().await;
    let distinct: HashSet<&String> = taken_by_the_centre.iter().collect();

    // One MESSAGE per recipient, which is not one submission: a text of 400
    // characters is three `submit_sm`, all three accepted, and that is one
    // message. So the ceiling is the number of segments, and anything above it
    // is a second message to the same person.
    let segments = if scenario.long_text { 3 } else { 1 };

    for recipient in &distinct {
        let accepted = taken_by_the_centre
            .iter()
            .filter(|taken| taken == recipient)
            .count();

        assert!(
            accepted <= segments,
            "a recipient was accepted {accepted} times for a message of \
             {segments} segment(s): {scenario:?}"
        );
    }

    for entry in &report.recipients {
        if entry.outcome != RecipientOutcome::Accepted {
            continue;
        }

        assert!(
            distinct.contains(&entry.destination.as_str().to_owned()),
            "a recipient the message centre did not take was reported accepted: {scenario:?}"
        );

        let row = journal
            .find_message(entry.client_message_id)
            .await
            .expect("the journal answers")
            .expect("the row exists");

        assert_eq!(
            row.state,
            MessageState::Accepted,
            "a recipient reported accepted is not accepted in the journal: {scenario:?}"
        );
        // Only for the rows a `submit_multi` carried. A recipient sent by the
        // fallback got an ordinary `submit_sm_resp` with an identifier of its
        // own, and that one correlates — which is half of why the fallback is
        // not a degraded mode.
        if entry.via == Via::Multi {
            assert_eq!(
                row.smsc_message_id, None,
                "a batched row carries the identifier the whole batch shares: {scenario:?}"
            );
        } else {
            assert!(
                row.smsc_message_id.is_some(),
                "an individually sent row lost its own identifier: {scenario:?}"
            );
        }
    }

    Seen {
        used_multi: report.used_submit_multi(),
        fell_back_after_emission: matches!(
            report.fallback,
            Some(FallbackReason::OperationRefused { .. })
        ),
        fell_back_before_emission: matches!(
            report.fallback,
            Some(
                FallbackReason::Disabled
                    | FallbackReason::KnownUnsupported
                    | FallbackReason::SingleRecipient
                    | FallbackReason::MultipleSegments
            )
        ),
        accepted: report.accepted(),
        rejected: report
            .recipients
            .iter()
            .filter(|entry| matches!(entry.outcome, RecipientOutcome::Rejected { .. }))
            .count(),
        uncertain: report
            .recipients
            .iter()
            .filter(|entry| entry.outcome == RecipientOutcome::Uncertain)
            .count(),
        already_present: report
            .recipients
            .iter()
            .filter(|entry| {
                entry.outcome == RecipientOutcome::AlreadyPresent && entry.via == Via::Nothing
            })
            .count(),
        hid_a_refusal: matches!(scenario.answer, Answer::RefuseUnrecognisably(_))
            && report.used_submit_multi(),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
}

proptest! {
    #![proptest_config(Config { cases: 192, ..Config::default() })]

    #[test]
    fn a_batch_never_loses_a_recipient_and_never_claims_one_it_did_not_send(
        scenario in any_scenario()
    ) {
        runtime().block_on(async { exercise(&scenario).await });
    }
}

/// The census. Without it, a strategy that only ever produced `AcceptAll` would
/// pass every assertion above and prove nothing.
#[test]
fn the_generator_reaches_the_families_the_property_can_break_in() {
    const SAMPLE: u32 = 192;

    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: SAMPLE,
            ..Config::default()
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );

    let strategy = any_scenario();
    let runtime = runtime();

    let mut with_multi = 0_u32;
    let mut with_late_fallback = 0_u32;
    let mut with_early_fallback = 0_u32;
    let mut with_a_partial_batch = 0_u32;
    let mut with_everybody_uncertain = 0_u32;
    let mut with_a_row_already_taken = 0_u32;
    let mut with_an_acceptance = 0_u32;
    let mut with_a_hidden_refusal = 0_u32;

    for _ in 0..SAMPLE {
        let scenario = strategy.new_tree(&mut runner).unwrap().current();
        let seen = runtime.block_on(async { exercise(&scenario).await });

        with_multi += u32::from(seen.used_multi);
        with_late_fallback += u32::from(seen.fell_back_after_emission);
        with_early_fallback += u32::from(seen.fell_back_before_emission);
        with_a_partial_batch += u32::from(seen.accepted > 0 && seen.rejected > 0);
        with_everybody_uncertain +=
            u32::from(seen.uncertain > 0 && seen.accepted == 0 && seen.rejected == 0);
        with_a_row_already_taken += u32::from(seen.already_present > 0);
        with_an_acceptance += u32::from(seen.accepted > 0);
        with_a_hidden_refusal += u32::from(seen.hid_a_refusal);
    }

    let census = format!(
        "over {SAMPLE} scenarios: \
         {with_multi} went out as one submit_multi, \
         {with_late_fallback} fell back after the PDU had left, \
         {with_early_fallback} fell back before it, \
         {with_a_partial_batch} were partially accepted, \
         {with_everybody_uncertain} left every recipient uncertain, \
         {with_a_row_already_taken} met a row that already existed, \
         {with_an_acceptance} accepted at least one recipient, \
         {with_a_hidden_refusal} had a real refusal quoted unrecognisably"
    );

    // The batched path itself: without it every case is an ordinary unit send
    // and this file tests `Sender`.
    assert!(with_multi >= SAMPLE / 5, "{census}");
    // CA-010-08's own family — the message centre refuses the operation.
    assert!(with_late_fallback >= SAMPLE / 10, "{census}");
    // …and the fallbacks decided before anything is written.
    assert!(with_early_fallback >= SAMPLE / 10, "{census}");
    // The partial success that makes a per-recipient verdict necessary at all.
    assert!(with_a_partial_batch >= SAMPLE / 20, "{census}");
    // The family the over-claim lives in: an answer nothing can be read from.
    assert!(with_everybody_uncertain >= SAMPLE / 10, "{census}");
    // The write-ahead guard firing.
    assert!(with_a_row_already_taken >= SAMPLE / 20, "{census}");
    // And the nominal path, so a generator that only ever failed is caught too.
    assert!(with_an_acceptance >= SAMPLE / 4, "{census}");
    // THE family, and the one a generator would silently exclude: it needs the
    // batch to have gone out as a `submit_multi` AND the message centre to
    // disagree with its own PDU.
    assert!(with_a_hidden_refusal >= SAMPLE / 20, "{census}");
}
