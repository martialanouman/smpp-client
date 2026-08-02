//! The property fiche §5 asks for: **at most one emission per recipient**,
//! under any sequence of events.
//!
//! # What is generated
//!
//! A campaign, and then an arbitrary history over it:
//!
//! * how many recipients it has;
//! * what the message centre answers, submission by submission — acceptance,
//!   a fatal refusal, a throttling refusal, a response that never comes, a
//!   session that closes mid-flight;
//! * what the operator does while it runs — pause, resume, cancel — and when;
//! * **how many times the process restarts**, modelled as running the campaign
//!   again over the same journal, which is exactly what a `kill -9` followed by
//!   a resume produces.
//!
//! # What is asserted, and why it is stated over acceptances
//!
//! The invariant is *not* "at most one `submit_sm` per recipient": spec §10.7
//! replays a refused or unanswered message on purpose, so a recipient may
//! legitimately be submitted to several times. What may never happen twice is a
//! submission the message centre **accepted** — that is the message the
//! recipient reads.
//!
//! So the property is stated over the message centre's own record of what it
//! accepted, which is the one place outside the code under test that knows.
//! Three assertions follow from it, and each catches a different way of getting
//! it wrong:
//!
//! 1. no recipient appears twice among the accepted submissions — a second
//!    emission after an acceptance, which is CA-010-05 and CA-010-03;
//! 2. the journal holds at most one row per recipient, and its keys are as many
//!    as its rows — a second `client_message_id` for one recipient, which is
//!    CA-010-04;
//! 3. every recipient the counters account for was handed over by the feeder,
//!    and the reverse — CA-010-02, an equality between two counters incremented
//!    by two different loops.

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
use messaging::ports::{Recipient, SubmitError};
use messaging::retry::{RetryBackoff, RetryPolicy};
use messaging::sender::Sender;
use messaging::submit::SubmitOptions;
use messaging::template::Template;
use messaging::testing::{FakeSmsc, FixedClock, MemoryJournal, Reply, StaticRecipients};
use proptest::prelude::*;
use smpp_core::types::{CampaignId, Msisdn};
use smpp_core::values::CommandStatus;

/// What the operator does while the campaign runs.
#[derive(Debug, Clone, Copy)]
enum Command {
    Pause,
    Resume,
    Cancel,
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

fn any_command() -> impl Strategy<Value = (u64, Command)> {
    (
        1_u64..40,
        prop_oneof![
            2 => Just(Command::Pause),
            2 => Just(Command::Resume),
            1 => Just(Command::Cancel),
        ],
    )
}

/// Applies the generated commands on the virtual clock, then releases the
/// campaign so a run that was left paused still terminates.
///
/// Without the final `resume` a paused campaign would wait for a command that
/// the generated history never sends, and the property would hang rather than
/// fail — which is a worse test, not a stricter one.
async fn drive(control: &CampaignControl, commands: &[(u64, Command)]) {
    for (delay, command) in commands {
        tokio::time::sleep(Duration::from_millis(*delay)).await;

        match command {
            Command::Pause => control.pause(),
            Command::Resume => control.resume(),
            Command::Cancel => control.cancel(),
        }
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    control.resume();
}

proptest! {
    // Sixty-four histories rather than proptest's default, deliberately: each
    // one runs a whole campaign up to three times, and the suite has to stay
    // fast enough to be run before every commit (CLAUDE.md §5).
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn at_most_one_accepted_message_reaches_each_recipient(
        recipient_count in 1_usize..12,
        replies in prop::collection::vec(any_reply(), 0..24),
        commands in prop::collection::vec(any_command(), 0..4),
        runs in 1_usize..4,
        abandon_unanswered in any::<bool>(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();

        runtime.block_on(async move {
            let journal = MemoryJournal::new();
            // ONE message centre across every run: what it accepted in the run
            // before a crash is exactly what the run after must not accept
            // again.
            let smsc = FakeSmsc::scripted(replies).recording();

            let source = StaticRecipients::new(
                (0..recipient_count)
                    .map(|index| Recipient {
                        destination: number(index),
                        attributes: None,
                    })
                    .collect(),
            );

            let template = Template::parse("Bonjour").unwrap();
            let submit = SubmitOptions::to(Destination::parse("+2250700000000").unwrap());

            for run in 0..runs {
                let mut plan = CampaignPlan::new(campaign(), template.clone(), submit.clone())
                    .with_retry(
                        RetryPolicy::new(
                            2,
                            Duration::from_secs(1),
                            Duration::from_secs(1),
                            RetryBackoff::Fixed,
                        )
                        .unwrap(),
                    );

                if abandon_unanswered {
                    plan = plan.on_unanswered(UnansweredPolicy::Abandon);
                }

                // The first run starts the campaign; every one after it is the
                // process coming back up.
                if run > 0 {
                    plan = plan.resuming();
                }

                let runner =
                    CampaignRunner::new(Sender::new(journal.clone(), FixedClock::default()), plan);
                let control = CampaignControl::new();

                let (outcome, ()) = tokio::join!(
                    runner.run(&smsc, &source, &control),
                    drive(&control, &commands),
                );

                let outcome = outcome.expect("the journal never fails in this property");

                // CA-010-02, per run: the buckets partition what the feeder
                // handed over.
                prop_assert_eq!(outcome.tally.total(), outcome.queued);
                prop_assert!(matches!(
                    outcome.status,
                    CampaignStatus::Completed | CampaignStatus::Cancelled
                ));
            }

            // --- THE INVARIANT ---------------------------------------------
            let accepted = smsc.accepted_destinations().await;
            let distinct: HashSet<&String> = accepted.iter().collect();

            prop_assert_eq!(
                accepted.len(),
                distinct.len(),
                "a recipient received an accepted message twice: {:?}",
                accepted
            );

            // CA-010-04: one row per recipient, whatever the history.
            let rows = journal.rows().await;
            let keys: HashSet<_> = rows.iter().map(|row| row.client_message_id).collect();

            prop_assert!(rows.len() <= recipient_count);
            prop_assert_eq!(rows.len(), keys.len());

            for row in &rows {
                prop_assert!(
                    (0..recipient_count)
                        .any(|index| message_key(campaign(), &number(index))
                            == row.client_message_id),
                    "the journal holds a row for nobody in the campaign"
                );
            }

            Ok(())
        })?;
    }
}
