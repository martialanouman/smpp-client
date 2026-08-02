//! The property fiche §5 asks for: **at most one emission per recipient**,
//! under any sequence of events.
//!
//! # What is generated
//!
//! A campaign, and then an arbitrary history over it:
//!
//! * how many recipients it has;
//! * what the message centre answers, submission by submission — acceptance, a
//!   fatal refusal, a throttling refusal, a response that never comes, a session
//!   that closes mid-flight;
//! * what the operator does while it runs — pause, resume, cancel — and *when*;
//! * **how the journal misbehaves**, run by run: it can lose the verdicts of a
//!   whole run (a `kill -9` between the emission and the commit), refuse every
//!   read, or refuse every write;
//! * **how many times the process restarts**, modelled as running the campaign
//!   again over the same journal.
//!
//! # The two exclusions this file used to have, and why they mattered
//!
//! A review found both, and the first is what let a real defect through.
//!
//! 1. **The journal could never fail.** Every run used a healthy double, so the
//!    family "the message centre took the message and the verdict was never
//!    written" — the one the whole resume arbitration exists for — was
//!    unreachable. The generator now picks a `JournalFault` per run.
//! 2. **There was one suspension point: the retry delay.** Without a replayable
//!    failure in the script, the whole campaign ran in a single poll, so the
//!    operator's commands were only ever served *after* it had finished.
//!    Cancellation could not land between the guard and the send, nor between
//!    the insert and the submission, nor between the submission and the verdict
//!    — which are precisely the windows CA-010-04 and CA-010-05 are about.
//!
//!    The doubles now yield on every journal operation and every submission, and
//!    the operator's script is driven in **yield ticks** rather than in
//!    milliseconds. Time was the wrong clock for it: under `start_paused` the
//!    runtime only advances the clock when every task is idle, so a sleeping
//!    script cannot interleave with a campaign that never sleeps.
//!
//! `the_generator_reaches_the_families_the_invariant_can_break_in` counts what
//! the generator actually produces, so neither exclusion can come back
//! unnoticed.
//!
//! # What is asserted, stated exactly
//!
//! The invariant is *not* "at most one `submit_sm` per recipient": spec §10.7
//! replays a refused or unanswered message on purpose. It is about the
//! submissions the message centre **accepted** — the messages the recipient
//! reads — and it is checked against the centre's own record, the one place
//! outside the code under test that knows.
//!
//! But "at most one accepted message per recipient, full stop" is **false**, and
//! saying otherwise would be claiming that ADR 0014 has no cost.
//! [`UnansweredPolicy::Reemit`] deliberately re-sends a message that may already
//! have been taken; when it had been, the recipient receives it twice. That is
//! the arbitration, not a defect.
//!
//! So the property is stated in the two halves that are actually true, and
//! together they are stronger than the version this file used to assert:
//!
//! 1. **Under [`UnansweredPolicy::Abandon`], no recipient is ever accepted
//!    twice.** The policy that promises not to duplicate must not duplicate —
//!    which, before the review, it did, because the crash left rows reading
//!    `QUEUED` and it had nothing to abandon.
//! 2. **Under [`UnansweredPolicy::Reemit`], every duplicate was counted.** The
//!    number of extra acceptances never exceeds `reemitted_unanswered`, the
//!    figure the campaign reports and the `warn!` it logs. A duplicate the
//!    operator was not told about is a failure of this property — and that is
//!    precisely what a figure reading zero while five duplicates went out was.

// `tests/` is compiled without `cfg(test)`, so the relaxations of `clippy.toml`
// do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: proptest is synchronous and the campaign is not, so
//     the property drives a runtime with `block_on`. A test harness is the
//     "binary entry point" the lint reserves it for.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use core::time::Duration;
use std::collections::HashSet;

use messaging::addressing::Destination;
use messaging::campaign::control::CampaignControl;
use messaging::campaign::resume::{message_key, UnansweredPolicy};
use messaging::campaign::runner::{CampaignPlan, CampaignRunner};
use messaging::campaign::CampaignStatus;
use messaging::message::MessageState;
use messaging::ports::{Recipient, SubmitError};
use messaging::retry::{RetryBackoff, RetryPolicy};
use messaging::sender::Sender;
use messaging::submit::SubmitOptions;
use messaging::template::Template;
use messaging::testing::{
    FakeSmsc, FixedClock, JournalFault, MemoryJournal, Reply, StaticRecipients,
};
use proptest::prelude::*;
use proptest::strategy::ValueTree as _;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use smpp_core::types::{CampaignId, Msisdn};
use smpp_core::values::CommandStatus;

/// What the operator does while the campaign runs.
#[derive(Debug, Clone, Copy)]
enum Command {
    Pause,
    Resume,
    Cancel,
}

/// One generated history.
#[derive(Debug, Clone)]
struct History {
    recipient_count: usize,
    replies: Vec<Reply>,
    /// `(ticks to wait, command)`, in order.
    commands: Vec<(u32, Command)>,
    /// One entry per run of the campaign: how the journal misbehaves during it.
    runs: Vec<JournalFault>,
    abandon_unanswered: bool,
}

/// What one exercised history produced, for the coverage census.
#[derive(Debug, Clone, Copy, Default)]
struct Observations {
    /// Submissions the message centre accepted, across every run.
    accepted: usize,
    /// Runs that ended `CANCELLED`.
    cancelled_runs: usize,
    /// Runs the journal made fail outright.
    failed_runs: usize,
    /// Verdicts the journal swallowed.
    lost_verdicts: u64,
    /// Rows a resume found in the **uncertain** family — `SENT`, no answer.
    uncertain_rows_seen: usize,
    /// Rows a resume found already accepted, which CA-010-05 forbids re-sending.
    accepted_rows_seen: usize,
    /// Messages the runner reported as possible duplicates.
    reemitted_unanswered: u64,
    /// Recipients that were accepted more than once.
    duplicates: usize,
}

fn campaign() -> CampaignId {
    CampaignId::parse("3f8d0a2e-0000-4000-8000-0000000000ff").unwrap()
}

fn number(index: usize) -> Msisdn {
    Msisdn::parse(&format!("+225{:010}", 7_000_000_000_u64 + index as u64)).unwrap()
}

/// Every answer a message centre can give, including the two that carry none.
fn any_reply() -> impl Strategy<Value = Reply> {
    prop_oneof![
        4 => Just(Reply::Accepted),
        1 => Just(Reply::Rejected(CommandStatus::EsmeRthrottled)),
        1 => Just(Reply::Rejected(CommandStatus::EsmeRinvdstadr)),
        1 => Just(Reply::Rejected(CommandStatus::EsmeRmsgqful)),
        1 => Just(Reply::Failed(SubmitError::ResponseTimeout)),
        1 => Just(Reply::Failed(SubmitError::Closed)),
    ]
}

fn any_command() -> impl Strategy<Value = (u32, Command)> {
    (
        0_u32..24,
        prop_oneof![
            2 => Just(Command::Pause),
            2 => Just(Command::Resume),
            1 => Just(Command::Cancel),
        ],
    )
}

/// How the journal behaves during one run.
///
/// Weighted towards working, because a journal broken for every run of every
/// history would exercise the send path barely at all.
fn any_fault() -> impl Strategy<Value = JournalFault> {
    prop_oneof![
        5 => Just(JournalFault::None),
        3 => Just(JournalFault::LosesVerdicts),
        1 => Just(JournalFault::RefusesReads),
        1 => Just(JournalFault::RefusesWrites),
    ]
}

fn any_history() -> impl Strategy<Value = History> {
    (
        1_usize..12,
        prop::collection::vec(any_reply(), 0..24),
        prop::collection::vec(any_command(), 0..4),
        prop::collection::vec(any_fault(), 1..4),
        any::<bool>(),
    )
        .prop_map(
            |(recipient_count, replies, commands, runs, abandon_unanswered)| History {
                recipient_count,
                replies,
                commands,
                runs,
                abandon_unanswered,
            },
        )
}

/// Waits `count` scheduler turns.
///
/// **Not** a sleep. Under `start_paused` the runtime only advances its clock
/// once every task is idle, so a script that slept could never interleave with a
/// campaign that does not — it would be served after the campaign had finished,
/// which is exactly how the previous version of this file ended up exercising
/// one path. A yield hands control back on every turn, so a command lands
/// wherever the doubles happen to have suspended the campaign.
async fn ticks(count: u32) {
    for _ in 0..count {
        tokio::task::yield_now().await;
    }
}

/// Applies the generated commands, then releases the campaign.
///
/// The final `resume` is not decoration: a history whose last command is a pause
/// would otherwise wait for a command that never comes, and the property would
/// hang rather than fail — a worse test, not a stricter one.
async fn drive(control: &CampaignControl, commands: &[(u32, Command)]) {
    for (delay, command) in commands {
        ticks(*delay).await;

        match command {
            Command::Pause => control.pause(),
            Command::Resume => control.resume(),
            Command::Cancel => control.cancel(),
        }
    }

    ticks(8).await;
    control.resume();
}

/// Runs one history and holds it to the invariant.
async fn exercise(history: &History) -> Result<Observations, TestCaseError> {
    let mut seen = Observations::default();

    let journal = MemoryJournal::new().yielding();
    // ONE message centre across every run: what it accepted before a crash is
    // exactly what the run after must not accept again.
    let smsc = FakeSmsc::scripted(history.replies.clone())
        .recording()
        .yielding();

    let source = StaticRecipients::new(
        (0..history.recipient_count)
            .map(|index| Recipient {
                destination: number(index),
                attributes: None,
            })
            .collect(),
    );

    let template = Template::parse("Bonjour").unwrap();
    let submit = SubmitOptions::to(Destination::parse("+2250700000000").unwrap());

    for (index, fault) in history.runs.iter().enumerate() {
        // What this run is about to look at, counted before it starts.
        for row in journal.rows().await {
            match row.state {
                MessageState::Sent if row.command_status.is_none() => {
                    seen.uncertain_rows_seen += 1;
                }
                MessageState::Accepted => seen.accepted_rows_seen += 1,
                _ => {}
            }
        }

        journal.set_fault(*fault).await;

        let mut plan = CampaignPlan::new(campaign(), template.clone(), submit.clone()).with_retry(
            RetryPolicy::new(
                2,
                Duration::from_secs(1),
                Duration::from_secs(1),
                RetryBackoff::Fixed,
            )
            .unwrap(),
        );

        if history.abandon_unanswered {
            plan = plan.on_unanswered(UnansweredPolicy::Abandon);
        }

        // The first run starts the campaign; every one after it is the process
        // coming back up.
        if index > 0 {
            plan = plan.resuming();
        }

        let runner = CampaignRunner::new(Sender::new(journal.clone(), FixedClock::default()), plan);
        let control = CampaignControl::new();

        let (outcome, ()) = tokio::join!(
            runner.run(&smsc, &source, &control),
            drive(&control, &history.commands),
        );

        match outcome {
            Ok(outcome) => {
                // CA-010-02, per run: the buckets partition what the feeder
                // handed over.
                prop_assert_eq!(outcome.tally.total(), outcome.queued);
                prop_assert!(matches!(
                    outcome.status,
                    CampaignStatus::Completed | CampaignStatus::Cancelled
                ));

                if outcome.status == CampaignStatus::Cancelled {
                    seen.cancelled_runs += 1;
                }

                seen.reemitted_unanswered += outcome.tally.reemitted_unanswered;
            }
            // A journal that will not answer stops the campaign. That is the
            // documented behaviour — no write-ahead, no sending — and it is one
            // of the families this generator produces on purpose.
            Err(_) => seen.failed_runs += 1,
        }

        journal.set_fault(JournalFault::None).await;
    }

    seen.lost_verdicts = journal.lost_verdicts().await;

    // --- THE INVARIANT -----------------------------------------------------
    let accepted = smsc.accepted_destinations().await;
    let distinct: HashSet<&String> = accepted.iter().collect();

    seen.accepted = accepted.len();

    let duplicates = accepted.len() - distinct.len();

    if history.abandon_unanswered {
        prop_assert_eq!(
            duplicates,
            0,
            "the policy that exists to avoid duplicates produced {:?}",
            accepted
        );
    }

    seen.duplicates = duplicates;

    prop_assert!(
        duplicates as u64 <= seen.reemitted_unanswered,
        "{} recipient(s) were accepted twice and {} were reported as at risk: {:?}",
        duplicates,
        seen.reemitted_unanswered,
        accepted
    );

    // CA-010-04: one row per recipient, whatever the history.
    let rows = journal.rows().await;
    let keys: HashSet<_> = rows.iter().map(|row| row.client_message_id).collect();

    prop_assert!(rows.len() <= history.recipient_count);
    prop_assert_eq!(rows.len(), keys.len());

    for row in &rows {
        prop_assert!(
            (0..history.recipient_count)
                .any(|index| message_key(campaign(), &number(index)) == row.client_message_id),
            "the journal holds a row for nobody in the campaign"
        );
    }

    Ok(seen)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
}

proptest! {
    // Sixty-four histories rather than proptest's default, deliberately: each
    // one runs a whole campaign up to three times, and the suite has to stay
    // fast enough to be run before every commit (CLAUDE.md §5).
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn at_most_one_accepted_message_reaches_each_recipient(history in any_history()) {
        runtime().block_on(async { exercise(&history).await })?;
    }
}

/// The census the review asked for: **does the generator reach the families the
/// invariant can break in?**
///
/// A property that only ever draws histories where nothing can go wrong is a
/// property that passes. This test draws a fixed, seeded sample from the same
/// strategy, exercises every one of them, and asserts a floor on each family. It
/// is the guard against this file quietly narrowing again — and its failure
/// message prints the whole census, so a drift is legible rather than a bare
/// `assertion failed`.
///
/// The floors are deliberately far below what the strategy produces: they catch
/// a family becoming *unreachable*, not a shift in its frequency.
#[test]
fn the_generator_reaches_the_families_the_invariant_can_break_in() {
    const SAMPLE: u32 = 192;

    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: SAMPLE,
            ..Config::default()
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );

    let strategy = any_history();
    let runtime = runtime();

    let mut with_lost_verdicts = 0_u32;
    let mut with_uncertain_row_resumed = 0_u32;
    let mut with_accepted_row_resumed = 0_u32;
    let mut with_cancellation = 0_u32;
    let mut with_journal_failure = 0_u32;
    let mut with_two_or_more_acceptances = 0_u32;
    let mut with_reported_duplicate_risk = 0_u32;
    let mut with_a_real_duplicate = 0_u32;

    for _ in 0..SAMPLE {
        let history = strategy.new_tree(&mut runner).unwrap().current();
        let seen = runtime
            .block_on(async { exercise(&history).await })
            .expect("the invariant holds on every sampled history");

        with_lost_verdicts += u32::from(seen.lost_verdicts > 0);
        with_uncertain_row_resumed += u32::from(seen.uncertain_rows_seen > 0);
        with_accepted_row_resumed += u32::from(seen.accepted_rows_seen > 0);
        with_cancellation += u32::from(seen.cancelled_runs > 0);
        with_journal_failure += u32::from(seen.failed_runs > 0);
        with_two_or_more_acceptances += u32::from(seen.accepted >= 2);
        with_reported_duplicate_risk += u32::from(seen.reemitted_unanswered > 0);
        with_a_real_duplicate += u32::from(seen.duplicates > 0);
    }

    let census = format!(
        "over {SAMPLE} histories: \
         {with_lost_verdicts} lost a verdict, \
         {with_uncertain_row_resumed} resumed over an uncertain row, \
         {with_accepted_row_resumed} resumed over an accepted row, \
         {with_cancellation} were cancelled, \
         {with_journal_failure} were stopped by the journal, \
         {with_two_or_more_acceptances} accepted two messages or more, \
         {with_reported_duplicate_risk} reported a duplicate risk, \
         {with_a_real_duplicate} actually delivered one"
    );

    // The family the review found the defect in: the message centre took the
    // message and the verdict was never written.
    assert!(with_lost_verdicts >= SAMPLE / 10, "{census}");
    // …and a later run then had to decide what to do with the row it left.
    assert!(with_uncertain_row_resumed >= SAMPLE / 20, "{census}");
    // CA-010-05: a resume that finds an already-accepted message.
    assert!(with_accepted_row_resumed >= SAMPLE / 20, "{census}");
    // The operator's commands have to bite somewhere.
    assert!(with_cancellation >= SAMPLE / 20, "{census}");
    assert!(with_journal_failure >= SAMPLE / 20, "{census}");
    // And the invariant has to be non-vacuous: a history with fewer than two
    // acceptances cannot show a recipient accepted twice.
    assert!(with_two_or_more_acceptances >= SAMPLE / 2, "{census}");
    assert!(with_reported_duplicate_risk >= SAMPLE / 20, "{census}");
    // The sharpest one: histories where a recipient really was accepted twice.
    // If none is reached, the bound "every duplicate was counted" is vacuous and
    // the whole arbitration is untested.
    assert!(with_a_real_duplicate >= SAMPLE / 40, "{census}");
}
