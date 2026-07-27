//! Delivery receipts, end to end — milestone 008.
//!
//! Same shape as `sending.rs`, and here for the same reason: these tests drive
//! `messaging`'s correlation against `smpp-session`'s real actors, and putting
//! them in `messaging` would need a dev-dependency back on `smpp-session` —
//! the cycle that file's header explains. From here both crates are reachable
//! with no new edge.
//!
//! Nothing is stubbed between the receipt the message centre writes and the row
//! the correlation moves: the double frames its own `deliver_sm`, the session's
//! reader decodes it, the reader answers `deliver_sm_resp` on its own, and the
//! test reads the queue the session publishes.

// `tests/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: `#[tokio::test]` expands to `Runtime::block_on`,
//     which `clippy.toml` reserves for "the binary entry point". A test
//     harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use core::time::Duration;

use messaging::addressing::Destination;
use messaging::correlation::{
    BatchPolicy, Correlator, IncomingReceipt, OrphanReason, OrphanReceipt, OrphanReceiptStore,
    ReceiptPipeline,
};
use messaging::dlr::{classify, Incoming};
use messaging::message::MessageState;
use messaging::ports::MessageStoreError;
use messaging::sender::{SendRequest, Sender};
use messaging::submit::SubmitOptions;
use smpp_core::types::ClientMessageId;
use smpp_session::testing::{
    acknowledged, drain, wait_until_bound, Script, Seen, Smsc, SubmitReply,
};
use smpp_session::Session;

use support::journal::{FrozenClock, Journal};

/// Long enough for every queued PDU to have crossed the in-memory socket.
///
/// Under `start_paused` this is virtual time: the runtime advances the clock as
/// soon as every task is idle, so it costs nothing and is not a race.
const SETTLE: Duration = Duration::from_secs(1);

/// An orphan journal that remembers what it was handed.
#[derive(Clone, Default)]
struct Orphanage {
    written: std::sync::Arc<tokio::sync::Mutex<Vec<OrphanReceipt>>>,
}

impl Orphanage {
    async fn written(&self) -> Vec<OrphanReceipt> {
        self.written.lock().await.clone()
    }
}

impl OrphanReceiptStore for Orphanage {
    async fn insert_orphans(&self, orphans: &[OrphanReceipt]) -> Result<u64, MessageStoreError> {
        self.written.lock().await.extend_from_slice(orphans);

        Ok(orphans.len() as u64)
    }
}

fn a_send(text: &str) -> SendRequest {
    SendRequest::new(
        text,
        SubmitOptions::to(Destination::parse("+2250102030405").unwrap()),
    )
}

/// Sends `count` messages and returns their identifiers, in order.
///
/// The message centre assigns `SMSC-1`, `SMSC-2`… so a test can name the
/// identifier a receipt should quote without reading it back.
async fn send_messages(session: &Session, journal: &Journal, count: usize) -> Vec<ClientMessageId> {
    let sender = Sender::new(journal.clone(), FrozenClock::at("2026-07-26T12:00:00Z"));
    let mut sent = Vec::with_capacity(count);

    for index in 0..count {
        let request = a_send(&format!("message {index}"));
        let report = sender.send(&session.handle, &request).await.unwrap();

        assert_eq!(report.state, MessageState::Accepted);
        sent.push(report.client_message_id);
    }

    sent
}

/// Drains the session's delivery queue into the receipts a pipeline consumes.
///
/// This is the shape `src-tauri` uses: read the queue, classify by `esm_class`,
/// keep the receipts and drop the mobile-originated messages (step-008 §2 puts
/// their business handling out of scope).
fn collect_receipts(session: &mut Session) -> Vec<IncomingReceipt> {
    let mut receipts = Vec::new();

    while let Ok(command) = session.deliveries.try_recv() {
        match messaging::dlr::as_deliver_sm(command.pdu()).map(classify) {
            Some(Incoming::Receipt(receipt)) => receipts.push(IncomingReceipt {
                session_id: Some(session.handle.session_id()),
                receipt,
            }),
            Some(Incoming::MobileOriginated) | None => {}
        }
    }

    receipts
}

async fn apply(journal: &Journal, orphans: &Orphanage, mut receipts: Vec<IncomingReceipt>) {
    let pipeline = ReceiptPipeline::new(
        Correlator::new(journal.clone(), FrozenClock::at("2026-07-26T13:00:00Z")),
        orphans.clone(),
    )
    .with_policy(BatchPolicy {
        max_receipts: 200,
        max_delay: Duration::from_millis(250),
    });

    pipeline.commit(&mut receipts, &()).await;
}

// ---------------------------------------------------------------------------
// CA-008-06 — every deliver_sm is acknowledged
// ---------------------------------------------------------------------------

/// **CA-008-06.** A message centre that does not get its `deliver_sm_resp`
/// re-sends the receipt for ever, so an unacknowledged one is not a lost
/// notification: it is a loop.
///
/// Fifty receipts pushed back to back, and the assertion is on the **set** of
/// sequence numbers acknowledged, not on their count: a client answering the
/// same one fifty times would satisfy a count.
#[tokio::test(start_paused = true)]
async fn ca_008_06_every_delivery_receipt_is_acknowledged_under_load() {
    const RECEIPTS: usize = 50;

    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session = smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());

    wait_until_bound(&session.handle).await;

    let pushed: Vec<u32> = (0..RECEIPTS)
        .map(|index| smsc.deliver_receipt(&format!("id:SMSC-{index} stat:DELIVRD err:000")))
        .collect();

    tokio::time::sleep(SETTLE).await;

    let mut answered = acknowledged(&drain(&mut seen));
    answered.sort_unstable();
    answered.dedup();

    let mut expected = pushed.clone();
    expected.sort_unstable();

    assert_eq!(
        answered, expected,
        "every pushed receipt must be acknowledged exactly once, by its own \
         sequence number"
    );

    session.handle.shutdown().await.unwrap();
}

/// The acknowledgement does not depend on anyone draining the delivery queue.
///
/// The queue is bounded and its consumer is optional; a receipt dropped for
/// want of a consumer must still be acknowledged, or the centre re-sends it and
/// the queue stays full for ever. The overflow case is the one that matters,
/// so this pushes past the queue's capacity without reading a single item.
#[tokio::test(start_paused = true)]
async fn a_receipt_is_acknowledged_even_when_nobody_drains_the_queue() {
    // The delivery queue holds 256; 300 forces the overflow path.
    const RECEIPTS: usize = 300;

    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session = smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());

    wait_until_bound(&session.handle).await;

    let pushed: Vec<u32> = (0..RECEIPTS)
        .map(|index| smsc.deliver_receipt(&format!("id:SMSC-{index} stat:DELIVRD")))
        .collect();

    tokio::time::sleep(SETTLE).await;

    let mut answered = acknowledged(&drain(&mut seen));
    answered.sort_unstable();
    answered.dedup();

    assert_eq!(
        answered.len(),
        pushed.len(),
        "the acknowledgement must not depend on the queue having room"
    );

    session.handle.shutdown().await.unwrap();
}

/// An incoming message is acknowledged too, and is **not** a receipt however
/// its body reads (step-008 §2).
#[tokio::test(start_paused = true)]
async fn a_mobile_originated_message_is_acknowledged_and_correlates_to_nothing() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());

    wait_until_bound(&session.handle).await;

    let pushed = smsc.deliver_message("id:SMSC-1 stat:DELIVRD err:000");
    tokio::time::sleep(SETTLE).await;

    assert_eq!(acknowledged(&drain(&mut seen)), vec![pushed]);
    assert!(
        collect_receipts(&mut session).is_empty(),
        "esm_class says normal message, so the body must not make it a receipt"
    );

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Correlation against the real send path
// ---------------------------------------------------------------------------

/// **CA-008-01** end to end: send, receive the receipt, and the row is
/// `DELIVERED` with its receipt fields.
#[tokio::test(start_paused = true)]
async fn a_receipt_moves_the_message_it_names_to_delivered() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());
    let journal = Journal::new();
    let orphans = Orphanage::default();

    wait_until_bound(&session.handle).await;
    let sent = send_messages(&session, &journal, 1).await;

    smsc.deliver_receipt("id:SMSC-1 sub:001 dlvrd:001 stat:DELIVRD err:000 text:message 0");
    tokio::time::sleep(SETTLE).await;

    apply(&journal, &orphans, collect_receipts(&mut session)).await;

    let row = journal.row(sent[0]).await.unwrap();

    assert_eq!(row.state, MessageState::Delivered);
    assert_eq!(row.dlr_stat.as_deref(), Some("DELIVRD"));
    assert_eq!(row.dlr_err.as_deref(), Some("000"));
    assert!(row.dlr_at.is_some());
    assert!(orphans.written().await.is_empty());

    session.handle.shutdown().await.unwrap();
}

/// step-008 §5 — receipts arriving **out of order**. Each must land on its own
/// message; a correlation that paired them by arrival order would swap them.
#[tokio::test(start_paused = true)]
async fn receipts_arriving_out_of_order_each_reach_their_own_message() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());
    let journal = Journal::new();
    let orphans = Orphanage::default();

    wait_until_bound(&session.handle).await;
    let sent = send_messages(&session, &journal, 3).await;

    // Third, first, second — and the middle one failed.
    smsc.deliver_receipt("id:SMSC-3 stat:DELIVRD err:000");
    smsc.deliver_receipt("id:SMSC-1 stat:DELIVRD err:000");
    smsc.deliver_receipt("id:SMSC-2 stat:UNDELIV err:058");
    tokio::time::sleep(SETTLE).await;

    apply(&journal, &orphans, collect_receipts(&mut session)).await;

    assert_eq!(
        journal.row(sent[0]).await.unwrap().state,
        MessageState::Delivered
    );
    assert_eq!(
        journal.row(sent[1]).await.unwrap().state,
        MessageState::Failed,
        "the second message is the one that failed, whatever the arrival order"
    );
    assert_eq!(
        journal.row(sent[1]).await.unwrap().dlr_err.as_deref(),
        Some("058")
    );
    assert_eq!(
        journal.row(sent[2]).await.unwrap().state,
        MessageState::Delivered
    );

    session.handle.shutdown().await.unwrap();
}

/// step-008 §5 — **the same receipt twice counts once.** The message centre
/// re-sends when it thinks its receipt was lost, and a client that counted the
/// second one would report more deliveries than it had messages.
#[tokio::test(start_paused = true)]
async fn a_receipt_received_twice_leaves_the_message_where_the_first_put_it() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());
    let journal = Journal::new();
    let orphans = Orphanage::default();

    wait_until_bound(&session.handle).await;
    let sent = send_messages(&session, &journal, 1).await;

    smsc.deliver_receipt("id:SMSC-1 stat:DELIVRD err:000");
    smsc.deliver_receipt("id:SMSC-1 stat:DELIVRD err:000");
    tokio::time::sleep(SETTLE).await;

    let receipts = collect_receipts(&mut session);
    assert_eq!(receipts.len(), 2, "both copies really arrived");

    apply(&journal, &orphans, receipts).await;

    let row = journal.row(sent[0]).await.unwrap();

    assert_eq!(row.state, MessageState::Delivered);
    assert_eq!(
        row.attempts, 1,
        "a receipt must never spend a sending attempt"
    );
    assert!(
        orphans.written().await.is_empty(),
        "the second copy correlates just as well as the first"
    );

    session.handle.shutdown().await.unwrap();
}

/// **CA-008-04** — a receipt for an identifier this client never saw is kept
/// and marked, not dropped.
#[tokio::test(start_paused = true)]
async fn a_receipt_for_an_unknown_identifier_is_journalled_as_an_orphan() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());
    let journal = Journal::new();
    let orphans = Orphanage::default();

    wait_until_bound(&session.handle).await;
    send_messages(&session, &journal, 1).await;

    smsc.deliver_receipt("id:SOMEBODY-ELSE stat:DELIVRD err:000");
    tokio::time::sleep(SETTLE).await;

    apply(&journal, &orphans, collect_receipts(&mut session)).await;

    let written = orphans.written().await;

    assert_eq!(written.len(), 1);
    assert_eq!(written[0].reason, OrphanReason::UnknownIdentifier);
    assert_eq!(written[0].smsc_message_id.as_deref(), Some("SOMEBODY-ELSE"));
    assert_eq!(written[0].session_id, Some(session.handle.session_id()));

    session.handle.shutdown().await.unwrap();
}

/// **CA-008-03** — a body nothing can be read out of produces an orphan, never
/// a panic and never a silent loss.
#[tokio::test(start_paused = true)]
async fn an_unreadable_receipt_body_is_journalled_rather_than_dropped() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());
    let journal = Journal::new();
    let orphans = Orphanage::default();

    wait_until_bound(&session.handle).await;

    smsc.deliver_receipt("<<< something no specification describes >>>");
    tokio::time::sleep(SETTLE).await;

    apply(&journal, &orphans, collect_receipts(&mut session)).await;

    let written = orphans.written().await;

    assert_eq!(written.len(), 1);
    assert_eq!(written[0].reason, OrphanReason::NoIdentifier);
    assert_eq!(
        written[0].raw,
        "<<< something no specification describes >>>"
    );

    session.handle.shutdown().await.unwrap();
}

/// The milestone-006 barrier, end to end and through the real socket: a
/// partially failed multi-segment message keeps no `smsc_message_id`, so the
/// receipt for its accepted fragment cannot credit it.
///
/// This is the whole reason `Sender::aggregate` drops the identifier, and the
/// only test that exercises both halves at once.
#[tokio::test(start_paused = true)]
async fn a_receipt_cannot_credit_a_partially_failed_message() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![
            SubmitReply::AcceptAs(String::from("SEG-1")),
            SubmitReply::Reject(smpp_core::values::CommandStatus::EsmeRsubmitfail),
        ],
        SubmitReply::Accept,
    );
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());
    let journal = Journal::new();
    let orphans = Orphanage::default();

    wait_until_bound(&session.handle).await;

    // Long enough to split, so the second segment's rejection is a partial
    // failure rather than a plain one.
    let sender = Sender::new(journal.clone(), FrozenClock::at("2026-07-26T12:00:00Z"));
    let report = sender
        .send(&session.handle, &a_send(&"a".repeat(400)))
        .await
        .unwrap();

    assert_eq!(report.state, MessageState::Failed);
    assert_eq!(
        journal
            .row(report.client_message_id)
            .await
            .unwrap()
            .smsc_message_id,
        None,
        "a partially failed message keeps no identifier — this is the barrier"
    );

    // The message centre delivers the fragment it accepted and says so.
    smsc.deliver_receipt("id:SEG-1 stat:DELIVRD err:000");
    tokio::time::sleep(SETTLE).await;

    apply(&journal, &orphans, collect_receipts(&mut session)).await;

    let row = journal.row(report.client_message_id).await.unwrap();

    assert_eq!(
        row.state,
        MessageState::Failed,
        "a fragment's receipt must not turn a failed message into a delivered one"
    );
    assert_eq!(
        orphans.written().await.len(),
        1,
        "and it is kept as an orphan"
    );

    session.handle.shutdown().await.unwrap();
}

/// A receipt arriving **after** the message failed for good must not resurrect
/// it. The refusal lives in the journal's transition rule, and this checks the
/// whole path honours it rather than the rule in isolation.
#[tokio::test(start_paused = true)]
async fn a_late_receipt_cannot_resurrect_a_rejected_message() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![SubmitReply::Reject(
            smpp_core::values::CommandStatus::EsmeRinvdstadr,
        )],
        SubmitReply::Accept,
    );
    let mut session =
        smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());
    let journal = Journal::new();
    let orphans = Orphanage::default();

    wait_until_bound(&session.handle).await;

    let sender = Sender::new(journal.clone(), FrozenClock::at("2026-07-26T12:00:00Z"));
    let report = sender
        .send(&session.handle, &a_send("rejected"))
        .await
        .unwrap();

    assert_eq!(report.state, MessageState::Failed);

    // A message centre that rejected the submission has no identifier to quote,
    // so this receipt cannot correlate at all — which is the first barrier. The
    // second is checked by pointing a receipt at a row that DOES carry one.
    journal
        .force_identifier(report.client_message_id, "LATE-1")
        .await;

    smsc.deliver_receipt("id:LATE-1 stat:DELIVRD err:000");
    tokio::time::sleep(SETTLE).await;

    apply(&journal, &orphans, collect_receipts(&mut session)).await;

    assert_eq!(
        journal.row(report.client_message_id).await.unwrap().state,
        MessageState::Failed,
        "FAILED is terminal; a delivery receipt must not walk it back"
    );

    session.handle.shutdown().await.unwrap();
}

/// The session must not stop, slow down or stop answering keep-alives because
/// receipts are arriving: the reader answers each one and carries on.
#[tokio::test(start_paused = true)]
async fn a_stream_of_receipts_leaves_the_session_bound() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session = smpp_session::testing::start(smpp_session::testing::a_profile(), smsc.clone());

    wait_until_bound(&session.handle).await;

    for index in 0..100 {
        smsc.deliver_receipt(&format!("id:SMSC-{index} stat:DELIVRD"));
    }

    tokio::time::sleep(SETTLE).await;

    smpp_session::testing::assert_state(
        &session.handle,
        smpp_session::state::SessionState::Bound(smpp_session::state::BindMode::Transceiver),
    );

    let notes = drain(&mut seen);
    assert!(
        !notes.iter().any(|note| matches!(note, Seen::GenericNack)),
        "a deliver_sm must never be answered with a generic_nack"
    );

    session.handle.shutdown().await.unwrap();
}
