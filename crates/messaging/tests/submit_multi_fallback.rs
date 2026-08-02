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
use messaging::ports::{MessageRepository as _, SmscSession as _, SubmitError};
use messaging::sender::Sender;
use messaging::submit::SubmitOptions;
use messaging::submit_multi::{
    Batch, BatchRecipient, BatchSender, FallbackReason, MultiSupport, RecipientOutcome, Via,
};
use messaging::testing::{journal_row, FakeSmsc, FixedClock, MemoryJournal, MultiReply, Refused};
use proptest::prelude::*;
use proptest::strategy::ValueTree as _;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use smpp_core::types::{CampaignId, ClientMessageId};
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

/// What a recipient's row already holds when the batch starts.
///
/// `Queued` is the family the old generator could not reach: it only ever
/// forced `Accepted` rows, so "a row a failed run left behind, which the batch
/// must send rather than skip" was excluded by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Existing {
    /// Written and never sent — what a failed insert leaves behind.
    Queued,
    /// Already taken by the message centre (CA-010-05).
    Accepted,
    /// In flight when the last run stopped: ADR 0014's arbitration.
    InFlight,
}

/// One case: a batch, a message centre, and the settings around them.
#[derive(Debug, Clone)]
struct Scenario {
    size: usize,
    answer: Answer,
    enabled: bool,
    last_attempt: bool,
    long_text: bool,
    /// Rows already in the journal before the batch runs.
    taken: Vec<(usize, Existing)>,
    /// Recipients listed **twice** in the batch, under two write-ahead keys.
    ///
    /// What a caller outside the campaign path can build, since
    /// [`BatchRecipient`] carries its key rather than deriving it — and the
    /// family where a refusal names two recipients and can be attributed to
    /// neither. The old generator built strictly distinct numbers, so it was
    /// excluded by construction.
    repeated: usize,
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
    // The two that `prevented_emission()` covers are weighted up: they are the
    // J-4 family — nothing left the socket, so nobody is a duplicate risk — and
    // at one weight each they came out of some seeds ZERO times in 192 cases.
    prop_oneof![
        2 => Just(SubmitError::ResponseTimeout),
        1 => Just(SubmitError::Closed),
        1 => Just(SubmitError::Transport {
            reason: String::from("the socket failed"),
        }),
        3 => Just(SubmitError::NotBound {
            state: String::from("RECONNECT"),
        }),
        3 => Just(SubmitError::OperationNotAllowed),
    ]
}

fn any_answer(size: usize) -> impl Strategy<Value = Answer> {
    // A STRICT SUBSET when there is one to take.
    //
    // `1..=size` refused everybody about as often as it refused somebody, so
    // "partially accepted" — the family a per-recipient verdict exists for —
    // came out of the sampler at 2 cases in 192 on some seeds and 13 on others.
    // Capping the refusals below `size` makes the family a *consequence of the
    // strategy* rather than of the seed.
    let partial = proptest::collection::vec((0..size, any_status()), 1..size.max(2))
        .prop_map(Answer::RefuseSome);

    prop_oneof![
        2 => Just(Answer::AcceptAll),
        9 => partial,
        2 => Just(Answer::RefuseAlien),
        8 => (0..size).prop_map(Answer::RefuseUnrecognisably),
        2 => any_status().prop_map(Answer::RefusePdu),
        10 => Just(Answer::Unsupported),
        1 => Just(Answer::Unreadable),
        8 => any_submit_error().prop_map(Answer::Silence),
    ]
}

fn any_existing() -> impl Strategy<Value = Existing> {
    prop_oneof![
        Just(Existing::Queued),
        Just(Existing::Accepted),
        Just(Existing::InFlight),
    ]
}

fn any_scenario() -> impl Strategy<Value = Scenario> {
    // Two or more, so `submit_multi` is reachable at all: a batch of one takes
    // the individual path by design, and half the families below need the
    // batched one.
    (2..=MAX_BATCH).prop_flat_map(|size| {
        (
            Just(size),
            any_answer(size),
            // Batching off in one case out of seven: the fallback that happens
            // before anything is written has to be covered too.
            proptest::bool::weighted(6.0 / 7.0),
            proptest::bool::weighted(0.5),
            proptest::bool::weighted(0.15),
            proptest::collection::vec((0..size, any_existing()), 0..=1),
            // A repeated recipient in one case out of three.
            prop_oneof![2 => Just(0_usize), 1 => Just(1_usize)],
        )
            .prop_map(
                |(size, answer, enabled, last_attempt, long_text, taken, repeated)| Scenario {
                    size,
                    answer,
                    enabled,
                    last_attempt,
                    long_text,
                    taken,
                    repeated,
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
    /// The batch carried one subscriber twice, so a refusal naming them is
    /// attributable to neither entry (J-1).
    ambiguous_recipients: bool,
    /// A row a failed run left `QUEUED` was met, and had to be sent rather than
    /// skipped (J-5).
    met_a_stranded_row: bool,
    /// A row left in flight was sent again, and counted (ADR 0014).
    replayed_an_in_flight_row: bool,
    /// The session refused before the socket (J-4).
    not_emitted: usize,
}

/// Runs one case and checks the property on it.
async fn exercise(scenario: &Scenario) -> Seen {
    let journal = MemoryJournal::new();
    let mut recipients: Vec<BatchRecipient> = (0..scenario.size)
        .map(|index| {
            let destination = Destination::parse(&number(index)).expect("a valid fixture number");

            BatchRecipient {
                client_message_id: message_key(campaign(), destination.number()),
                destination,
            }
        })
        .collect();

    // The same subscriber a second time, under a **different** write-ahead key
    // — so both rows are written and both are sent to. That is what a caller
    // building its own keys can produce, and it is the family where a refusal
    // names two recipients and belongs to neither.
    for _ in 0..scenario.repeated {
        if let Some(first) = recipients.first().cloned() {
            recipients.push(BatchRecipient {
                client_message_id: ClientMessageId::new(),
                destination: first.destination,
            });
        }
    }

    for (index, existing) in &scenario.taken {
        if let Some(recipient) = recipients.get(*index) {
            let mut row = journal_row(
                recipient.client_message_id,
                match existing {
                    Existing::Queued => MessageState::Queued,
                    Existing::Accepted => MessageState::Accepted,
                    Existing::InFlight => MessageState::Sent,
                },
            );

            // ADR 0014's third line is `SENT` **without** a `command_status`;
            // `journal_row` already leaves it `None`, and this says so where a
            // later edit could change it.
            if *existing == Existing::InFlight {
                row.command_status = None;
            }

            journal.force_row(row).await;
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
    let support = MultiSupport::for_session(smsc.session_id());

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
        recipients.len(),
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

    // One accepted MESSAGE per **entry of the batch**, and neither of those two
    // words is loose.
    //
    //   · a message is not a submission: a text of 400 characters is three
    //     `submit_sm`, all three accepted, and that is one message;
    //   · an entry is not a subscriber: a caller building its own keys may list
    //     one person twice, which is two rows and two messages by construction.
    //     The campaign path cannot — its keys are derived — but this API can,
    //     and `Scenario::repeated` is that family. What must never happen is a
    //     subscriber accepted more times than the batch has entries for them.
    let segments = if scenario.long_text { 3 } else { 1 };

    for address in &distinct {
        let accepted = taken_by_the_centre
            .iter()
            .filter(|taken| taken == address)
            .count();

        let entries = recipients
            .iter()
            .filter(|recipient| recipient.destination.number().as_str() == address.as_str())
            .count();

        assert!(
            accepted <= segments * entries,
            "a recipient was accepted {accepted} times for {entries} batch \
             entr(y/ies) of {segments} segment(s): {scenario:?}"
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
        ambiguous_recipients: scenario.repeated > 0 && report.used_submit_multi(),
        met_a_stranded_row: scenario
            .taken
            .iter()
            .any(|(_, existing)| *existing == Existing::Queued),
        replayed_an_in_flight_row: report.reemitted_unanswered > 0,
        not_emitted: report
            .recipients
            .iter()
            .filter(|entry| entry.outcome == RecipientOutcome::NotEmitted)
            .count(),
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

/// The census, over **several seeds**.
///
/// # Why not one pinned seed
///
/// The first version of this test pinned a deterministic RNG and asserted a
/// floor on that one sample. Replayed over sixty seeds, one of those floors was
/// cleared **by chance in half the runs** — `partially accepted` bottomed out at
/// 2 against a floor of 9. A floor that holds on the seed it was written against
/// and nowhere else is not a guarantee: the first engineer to meet it red would
/// lower it, and the protection would disappear without anybody deciding to
/// remove it.
///
/// So each floor is asserted on the **minimum over `SEEDS` independent seeds**,
/// which is a statement about the *strategy* rather than about a sample. The
/// strategy was reweighted until every family clears its floor at its worst
/// seed; `print_the_census` shows the margins, and they are what to re-read
/// after any change to `any_scenario`.
///
/// It also closes the gap the review named: the `proptest!` above runs on
/// `Config::default()`, whose `rng_seed` is **random**, so certifying one pinned
/// distribution certified a distribution the property never uses.
#[test]
fn the_generator_reaches_the_families_the_property_can_break_in() {
    let (worst, census) = census();

    for (family, count) in FAMILIES.iter().zip(&worst) {
        assert!(
            *count >= family.floor,
            "family \"{}\" is reached by chance rather than by the strategy — {census}",
            family.name
        );
    }
}

/// Prints the census the floors above are read from.
///
/// ```text
/// cargo test -p messaging --test submit_multi_fallback -- --ignored --nocapture
/// ```
#[test]
#[ignore = "reporting only; run it after changing the strategy"]
#[expect(
    clippy::print_stdout,
    reason = "the whole purpose of this ignored test is to print the table the \
              floors above are read from"
)]
fn print_the_census() {
    println!("{}", census().1);
}

/// Cases drawn per seed.
const SAMPLE: u32 = 192;

/// Independent seeds every floor must hold on.
const SEEDS: u8 = 12;

/// One family the property can break in: what it counts, and its floor.
///
/// # Where the floors come from
///
/// Each is **half** the count that family reached at its *worst* of the
/// `SEEDS` seeds, measured with `print_the_census` and written down. Two
/// consequences, and both are the point:
///
/// * every floor has at least 2× headroom today, so a floor is never cleared by
///   luck — which is what the previous version of this test did, with one floor
///   passing in half of sixty replayed seeds;
/// * a change to `any_scenario` that halves a family's representation turns the
///   test red instead of quietly narrowing what the property covers.
///
/// They are absolute numbers rather than fractions of `SAMPLE` because that is
/// what they are: a measurement, not a proportion somebody chose.
struct Family {
    name: &'static str,
    /// Half the worst-seed count measured when this line was written.
    floor: u32,
    seen: fn(&Seen) -> bool,
}

const FAMILIES: &[Family] = &[
    // The batched path itself: without it every case is an ordinary unit send
    // and this file tests `Sender`.
    Family {
        name: "went out as one submit_multi",
        floor: 41,
        seen: |seen| seen.used_multi,
    },
    // CA-010-08's own family — the message centre refuses the operation.
    Family {
        name: "fell back after the PDU had left",
        floor: 9,
        seen: |seen| seen.fell_back_after_emission,
    },
    // …and the fallbacks decided before anything is written.
    Family {
        name: "fell back before it",
        floor: 19,
        seen: |seen| seen.fell_back_before_emission,
    },
    // The partial success that makes a per-recipient verdict necessary at all.
    Family {
        name: "were partially accepted",
        floor: 5,
        seen: |seen| seen.accepted > 0 && seen.rejected > 0,
    },
    // An answer nothing can be read from.
    Family {
        name: "left every recipient uncertain",
        floor: 26,
        seen: |seen| seen.uncertain > 0 && seen.accepted == 0 && seen.rejected == 0,
    },
    // The write-ahead guard firing.
    Family {
        name: "met a row that already existed",
        floor: 13,
        seen: |seen| seen.already_present > 0,
    },
    // The nominal path, so a generator that only ever failed is caught too.
    Family {
        name: "accepted at least one recipient",
        floor: 47,
        seen: |seen| seen.accepted > 0,
    },
    // The silent over-claim: a real refusal quoted unrecognisably.
    Family {
        name: "had a real refusal quoted unrecognisably",
        floor: 8,
        seen: |seen| seen.hid_a_refusal,
    },
    // J-1: a batch carrying one subscriber twice, so a refusal naming them
    // belongs to neither entry.
    Family {
        name: "carried one subscriber twice",
        floor: 11,
        seen: |seen| seen.ambiguous_recipients,
    },
    // J-5: a row a failed run left QUEUED, which must be sent and not skipped.
    Family {
        name: "met a row stranded QUEUED",
        floor: 12,
        seen: |seen| seen.met_a_stranded_row,
    },
    // ADR 0014's arbitration, reached and reported.
    Family {
        name: "replayed a row left in flight",
        floor: 14,
        seen: |seen| seen.replayed_an_in_flight_row,
    },
    // J-4: the session refused before the socket.
    Family {
        name: "were refused before the socket",
        floor: 6,
        seen: |seen| seen.not_emitted > 0,
    },
];

/// Samples the strategy on every seed and returns, per family, the **worst**
/// count across them — plus the table to read it from.
fn census() -> (Vec<u32>, String) {
    let strategy = any_scenario();
    let runtime = runtime();
    let mut worst = vec![u32::MAX; FAMILIES.len()];

    for seed in 0..SEEDS {
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: SAMPLE,
                ..Config::default()
            },
            TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
        );

        let mut counts = vec![0_u32; FAMILIES.len()];

        for _ in 0..SAMPLE {
            let scenario = strategy.new_tree(&mut runner).unwrap().current();
            let seen = runtime.block_on(async { exercise(&scenario).await });

            for (count, family) in counts.iter_mut().zip(FAMILIES) {
                *count += u32::from((family.seen)(&seen));
            }
        }

        for (slot, count) in worst.iter_mut().zip(&counts) {
            *slot = (*slot).min(*count);
        }
    }

    let table = FAMILIES
        .iter()
        .zip(&worst)
        .map(|(family, count)| {
            format!(
                "\n  {count:>4} / {SAMPLE}  (floor {:>3}, margin {:>4}%)  {}",
                family.floor,
                count * 100 / family.floor.max(1),
                family.name
            )
        })
        .collect::<String>();

    (
        worst,
        format!("worst count over {SEEDS} seeds of {SAMPLE} cases:{table}"),
    )
}
