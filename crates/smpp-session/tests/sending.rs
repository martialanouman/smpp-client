//! The send path, end to end — milestone 006.
//!
//! Every test here drives the **real** orchestrator against the **real**
//! session actors of milestone 005, over `tokio::io::duplex`. Nothing is
//! stubbed between `Sender::send` and the octets on the socket: the PDU a test
//! asserts on is the one the codec produced, framed, and the double decoded
//! with its own framing.
//!
//! The two doubles are the journal (in-memory, instrumented) and the message
//! centre (scripted). Both are in `support/`.
//!
//! # Why these live in `smpp-session` and not in `messaging`
//!
//! They exercise `messaging`'s orchestrator, so this looks like the wrong
//! crate. The right one would need `messaging` to dev-depend on
//! `smpp-session`, which depends back on `messaging` for the `SmscSession`
//! port — a cycle Cargo tolerates, because a dev-dependency cannot affect the
//! library it tests, but a cycle all the same. CLAUDE.md §3 says "no cycle"
//! without distinguishing kinds, and a reader checking the graph would find
//! one.
//!
//! From here both crates are reachable with **no new edge at all**. The cost
//! is that the end-to-end tests sit one crate away from the code they drive;
//! the orchestrator's unit tests stay next to it.

// `tests/` is compiled without `cfg(test)`, so the relaxations of
// `clippy.toml` do not reach it.
//
//   · `unwrap`/`expect`: a panic here IS the failure report.
//   · `disallowed_methods`: `#[tokio::test]` expands to `Runtime::block_on`,
//     which `clippy.toml` reserves for "the binary entry point". A test
//     harness is one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use messaging::addressing::{Destination, SourceAddress};
use messaging::encoding::{Encoding, EncodingChoice, Gsm7BitCharset};
use messaging::message::MessageState;
use messaging::ports::{MessageRepository as _, MessageStoreError, SubmitError};
use messaging::segmentation::{ConcatenationReferenceCounter, SegmentationMode};
use messaging::sender::{SegmentOutcome, SendRequest, Sender};
use messaging::submit::{CustomTlv, SubmitOptions};
use messaging::MessagingError;
use smpp_core::tlvs::TlvTag;
use smpp_core::types::SessionId;
use smpp_core::values::{CommandStatus, DataCoding, Ton};
use smpp_session::testing::{
    a_password, drain, submissions, wait_until_bound, Script, Seen, Smsc, SubmitReply,
};
use smpp_session::{profile::SessionProfile, SessionHandle};

use support::journal::{FrozenClock, HangingSession, Journal, JournalEvent};

/// The instant every test that asserts on a timestamp uses.
const NOW: &str = "2026-07-26T12:00:00Z";

/// A profile pointing at the double, with a charset the test chooses.
///
/// `smpp_session::testing::a_profile` takes no charset, and the charset is
/// exactly what one of these tests is about.
fn a_profile(charset: Gsm7BitCharset) -> SessionProfile {
    SessionProfile::builder(SessionId::new(), "double", "in-memory", 2775)
        .system_id("esme01")
        .gsm7_charset(charset)
        .build()
        .expect("the fixture is valid")
}

/// Binds a session against `smsc` and hands back its handle.
async fn bound(profile: SessionProfile, smsc: Smsc) -> (SessionHandle, smpp_session::Session) {
    let session = smpp_session::spawn(profile, a_password(), smsc);
    let handle = session.handle.clone();

    wait_until_bound(&handle).await;

    (handle, session)
}

fn a_request(text: &str) -> SendRequest {
    SendRequest::new(
        text,
        SubmitOptions::to(Destination::parse("+2250102030405").expect("valid"))
            .with_source(SourceAddress::parse("ShinobiSMS").expect("valid")),
    )
}

fn a_sender(journal: Journal) -> Sender<Journal, FrozenClock> {
    Sender::new(journal, FrozenClock::at(NOW))
        // A fixed reference so the concatenation assertions are exact
        // (CLAUDE.md §7: no uncontrolled randomness in a test).
        .with_reference_counter(ConcatenationReferenceCounter::starting_at(0x1234))
}

/// **CA-006-01** — a short message goes out, its identifier comes back, and
/// the row walks `QUEUED → SENT → ACCEPTED`.
#[tokio::test(start_paused = true)]
async fn ca_006_01_a_short_message_is_accepted_and_carries_its_smsc_identifier() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![SubmitReply::AcceptAs(String::from("MSG-42"))],
        SubmitReply::Accept,
    );
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request("Bonjour");

    let report = sender.send(&handle, &request).await.expect("the send runs");

    assert_eq!(report.state, MessageState::Accepted);
    assert_eq!(report.smsc_message_id.as_deref(), Some("MSG-42"));
    assert_eq!(report.segments, 1);
    assert_eq!(report.command_status, Some(CommandStatus::EsmeRok));
    assert!(!report.retryable);

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("the message was persisted");

    assert_eq!(row.state, MessageState::Accepted);
    assert_eq!(row.smsc_message_id.as_deref(), Some("MSG-42"));
    assert_eq!(row.segments, 1);
    assert_eq!(row.attempts, 1);
    assert_eq!(row.created_at, FrozenClock::at(NOW).instant());
    assert_eq!(row.sent_at, Some(FrozenClock::at(NOW).instant()));
    assert_eq!(row.resp_at, Some(FrozenClock::at(NOW).instant()));
    assert_eq!(row.session_id, Some(handle.session_id()));

    // The full path, in order: written `QUEUED`, then moved `SENT` and
    // `ACCEPTED` in one transaction.
    assert_eq!(
        journal.events().await,
        vec![
            JournalEvent::Inserted(MessageState::Queued),
            JournalEvent::Transitioned(vec![MessageState::Sent, MessageState::Accepted]),
        ]
    );

    assert_eq!(submissions(&drain(&mut seen)).len(), 1);
}

/// **CA-006-02** — the write-ahead order, across the two components.
///
/// The journal samples the message centre's submission counter from inside
/// its own `insert_message`. Zero there means no `submit_sm` had reached the
/// socket at the moment the row was written.
///
/// This is the assertion that fails if the two lines are swapped, and it is
/// the only shape that does: the end state is `ACCEPTED` either way.
#[tokio::test(start_paused = true)]
async fn ca_006_02_the_message_is_journalled_before_any_submit_sm_reaches_the_socket() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let observed = smsc.clone();
    let journal = Journal::new().witnessing(move || observed.submissions());
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;

    sender
        .send(&handle, &a_request(&"a".repeat(400)))
        .await
        .expect("the send runs");

    assert_eq!(
        journal.submissions_at_insert().await,
        Some(0),
        "a submit_sm had already crossed the socket when the row was written"
    );
    // And the send really did submit afterwards, so the zero above is an
    // ordering fact rather than a message that never went out.
    assert_eq!(smsc.submissions(), 3);
}

/// **CA-006-03** — a brutal stop between the persistence and the emission.
///
/// The send is aborted while suspended on its first `submit`. What is left
/// behind is a `QUEUED` row — recoverable by milestone 010 — and nothing on
/// the wire, so nothing is duplicated either.
#[tokio::test(start_paused = true)]
async fn ca_006_03_a_stop_between_the_write_and_the_send_leaves_the_message_queued() {
    let journal = Journal::new();
    let sender = std::sync::Arc::new(a_sender(journal.clone()));
    let (session, mut reached) = HangingSession::new();
    let request = a_request("Bonjour");
    let identifier = request.client_message_id;

    let running = tokio::spawn({
        let sender = std::sync::Arc::clone(&sender);

        async move { sender.send(&session, &request).await.map(|_| ()) }
    });

    // The send has reached its first submission and is suspended there.
    reached.recv().await.expect("the send reached the socket");
    running.abort();
    let outcome = running.await;
    assert!(outcome.is_err(), "the task must have been cancelled");

    let row = journal.row(identifier).await.expect("the row survives");

    assert_eq!(row.state, MessageState::Queued);
    assert_eq!(row.attempts, 0, "no attempt was recorded: none completed");
    assert!(row.sent_at.is_none());
    assert_eq!(journal.len().await, 1, "exactly one row, never two");
    assert_eq!(
        journal.events().await,
        vec![JournalEvent::Inserted(MessageState::Queued)],
        "no transition was applied"
    );
}

/// **CA-006-04** — 400 GSM characters make three segments, three `submit_sm`,
/// and one coherent row.
#[tokio::test(start_paused = true)]
async fn ca_006_04_a_four_hundred_character_message_sends_three_correlated_segments() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request(&"a".repeat(400));

    let report = sender.send(&handle, &request).await.expect("the send runs");

    assert_eq!(report.segments, 3);
    assert_eq!(report.state, MessageState::Accepted);
    assert_eq!(report.outcomes.len(), 3);
    assert!(report.outcomes.iter().all(SegmentOutcome::is_accepted));

    let sent = submissions(&drain(&mut seen));
    assert_eq!(sent.len(), 3, "one submit_sm per segment");

    // Correlation: each request carried its own `sequence_number`, and they
    // are distinct. A sender that reused one could not tell the responses
    // apart.
    let sequences: Vec<u32> = sent.iter().map(|(sequence, _)| *sequence).collect();
    let mut unique = sequences.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        3,
        "sequence numbers were reused: {sequences:?}"
    );

    // The concatenation UDH ties them together: same reference, same total,
    // increasing part number.
    for (index, (_, pdu)) in sent.iter().enumerate() {
        let body = pdu.short_message().as_ref();

        assert_eq!(body[0], 0x05, "UDH length");
        assert_eq!(body[3], 0x34, "the low octet of reference 0x1234");
        assert_eq!(body[4], 3, "total segments");
        assert_eq!(usize::from(body[5]), index + 1, "part number");
        // The exact octet, not "not zero". `assert_ne!(…, 0)` is the shape
        // that let `EsmClass::default()` — which is `0x08`, an ANSI-41
        // delivery acknowledgement — pass for three milestones, because the
        // octet was never zero to begin with. `0x40` is the UDHI bit alone.
        assert_eq!(u8::from(pdu.esm_class), 0x40);
    }

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("one row for the logical message");

    assert_eq!(row.segments, 3);
    assert_eq!(row.state, MessageState::Accepted);
    assert_eq!(journal.len().await, 1, "three PDUs, one row");
}

/// **CA-006-05** — a rejected `submit_sm_resp` fails the message and keeps the
/// raw `command_status`, which the interface renders through the table of
/// milestone 003.
#[tokio::test(start_paused = true)]
async fn ca_006_05_a_rejected_response_fails_the_message_and_keeps_its_status() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![SubmitReply::Reject(CommandStatus::EsmeRinvdstadr)],
        SubmitReply::Accept,
    );
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request("Bonjour");

    // A rejection is a **report**, not an error: the operation ran.
    let report = sender.send(&handle, &request).await.expect("the send runs");

    assert_eq!(report.state, MessageState::Failed);
    assert_eq!(report.command_status, Some(CommandStatus::EsmeRinvdstadr));
    assert!(!report.retryable, "an invalid destination is fatal");

    // The plain-language label ENF-UTI-02 asks for comes from the status
    // table, not from a string invented at the boundary.
    let described =
        smpp_core::status_codes::describe(CommandStatus::EsmeRinvdstadr).expect("a standard code");
    assert_eq!(described.symbol, "ESME_RINVDSTADR");

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("persisted");

    assert_eq!(row.state, MessageState::Failed);
    assert_eq!(row.command_status, Some(CommandStatus::EsmeRinvdstadr));
    assert_eq!(
        row.smsc_message_id, None,
        "a rejection assigns no identifier, and an empty one must not be stored"
    );
}

/// `ESME_RTHROTTLED` is a rejection like any other **here**: the message
/// fails, and the report says a replay could work. What must not happen is a
/// loop — milestone 007 owns the pacing, and this milestone sends once.
#[tokio::test(start_paused = true)]
async fn a_throttled_response_is_reported_as_retryable_and_is_not_retried() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![SubmitReply::Reject(CommandStatus::EsmeRthrottled)],
        SubmitReply::Reject(CommandStatus::EsmeRthrottled),
    );
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;

    let report = sender
        .send(&handle, &a_request("Bonjour"))
        .await
        .expect("the send runs");

    assert_eq!(report.state, MessageState::Failed);
    assert!(report.retryable, "throttling is retryable after a slowdown");
    assert!(messaging::sender::requires_slowdown(
        CommandStatus::EsmeRthrottled
    ));
    assert_eq!(smsc.submissions(), 1, "one attempt, no loop");
}

/// A fatal status must not be retried either — and the loop that would do it
/// does not exist, which this states rather than assumes.
#[tokio::test(start_paused = true)]
async fn a_fatal_status_is_sent_once_and_reported_as_not_retryable() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        Vec::new(),
        SubmitReply::Reject(CommandStatus::EsmeRinvsrcadr),
    );
    let sender = a_sender(Journal::new());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;

    let report = sender
        .send(&handle, &a_request("Bonjour"))
        .await
        .expect("the send runs");

    assert!(!report.retryable);
    assert_eq!(smsc.submissions(), 1);
}

/// **The partial-failure decision** (fiche §6). Two segments accepted, one
/// rejected: the message is `FAILED`, the remaining segment is not sent, and
/// the per-segment detail survives in the report.
#[tokio::test(start_paused = true)]
async fn a_partially_rejected_message_fails_and_stops_sending() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![
            SubmitReply::Accept,
            SubmitReply::Reject(CommandStatus::EsmeRsubmitfail),
        ],
        SubmitReply::Accept,
    );
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;
    let request = a_request(&"a".repeat(400));

    let report = sender.send(&handle, &request).await.expect("the send runs");

    assert_eq!(report.state, MessageState::Failed);
    assert_eq!(report.command_status, Some(CommandStatus::EsmeRsubmitfail));
    assert_eq!(report.outcomes.len(), 3);
    assert!(report.outcomes[0].is_accepted());
    assert!(!report.outcomes[1].is_accepted());
    assert_eq!(report.outcomes[2], SegmentOutcome::NotAttempted);

    assert_eq!(
        smsc.submissions(),
        2,
        "the third segment must not be sent once the second was refused"
    );

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("persisted");
    assert_eq!(row.state, MessageState::Failed);
    assert_eq!(
        row.segments, 3,
        "the row still says how many segments the message needed"
    );

    // THE DECISION: a partially failed message keeps no `smsc_message_id`,
    // even though segment 1 was accepted and was assigned one. Storing it
    // would let a receipt for that fragment find this row at milestone 008 and
    // move it FAILED -> DELIVERED.
    assert_eq!(report.smsc_message_id, None);
    assert_eq!(row.smsc_message_id, None);

    // And it is not lost: the accepted segment still carries its own.
    assert_eq!(report.outcomes[0].smsc_message_id(), Some("SMSC-1"));
}

/// **The bug the decision above prevents**, stated as the sequence that would
/// produce it. A receipt for the accepted fragment must not be able to reach
/// the failed message, and the state machine refuses the move even if it did.
#[tokio::test(start_paused = true)]
async fn a_receipt_can_neither_find_nor_revive_a_partially_failed_message() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![
            SubmitReply::AcceptAs(String::from("FRAGMENT-1")),
            SubmitReply::Reject(CommandStatus::EsmeRsubmitfail),
        ],
        SubmitReply::Accept,
    );
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request(&"a".repeat(400));

    sender.send(&handle, &request).await.expect("the send runs");

    // Milestone 008 looks a receipt up by the identifier the centre quotes.
    assert!(
        journal
            .find_message_by_smsc_id("FRAGMENT-1", None)
            .await
            .expect("the lookup runs")
            .is_none(),
        "the failed message must not be reachable by its fragment's identifier"
    );

    // And even applied directly, the receipt cannot revive it.
    journal
        .update_state(
            &messaging::message::MessageStateUpdate::new(
                request.client_message_id,
                MessageState::Delivered,
            )
            .with_delivery_receipt("DELIVRD", None),
        )
        .await
        .expect("an illegal transition is a no-op, not an error");

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("persisted");

    assert_eq!(row.state, MessageState::Failed, "FAILED is terminal");
    assert_eq!(row.dlr_stat, None, "and the receipt wrote nothing at all");
}

/// A response that never comes: the message fails, carries **no**
/// `command_status` — the message centre sent none — and is reported
/// retryable, because nothing says it was refused (spec §10.7).
#[tokio::test(start_paused = true)]
async fn an_unanswered_submission_fails_without_inventing_a_status() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(Vec::new(), SubmitReply::Silent);
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request("Bonjour");

    let report = sender.send(&handle, &request).await.expect("the send runs");

    assert_eq!(report.state, MessageState::Failed);
    assert_eq!(report.command_status, None);
    assert!(report.retryable);
    assert_eq!(
        report.outcomes.first(),
        Some(&SegmentOutcome::Unanswered {
            failure: SubmitError::ResponseTimeout
        })
    );

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("persisted");
    assert_eq!(row.command_status, None);
    assert_eq!(row.attempts, 1, "one attempt was made, answered or not");
}

/// **CA-006-06** — the fields the operator typed are the fields on the wire.
#[tokio::test(start_paused = true)]
async fn ca_006_06_every_typed_field_of_the_specification_crosses_the_socket() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let journal = Journal::new();
    let sender = a_sender(journal);

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;

    let mut submit = SubmitOptions::to(
        Destination::parse_with(
            "3615",
            Ton::NetworkSpecific,
            smpp_core::values::Npi::National,
        )
        .expect("valid"),
    )
    .with_source(SourceAddress::parse("+2250102030405").expect("valid"));
    submit.service_type = String::from("CMT");
    submit.protocol_id = 0x42;
    submit.priority_flag = smpp_core::values::PriorityFlag::from(2);
    submit.schedule_delivery_time = String::from("2601011200000000");
    submit.validity_period = String::from("2601021200000000");
    submit.replace_if_present_flag = smpp_core::values::ReplaceIfPresentFlag::Replace;
    submit.sm_default_msg_id = 9;

    sender
        .send(&handle, &SendRequest::new("Bonjour", submit))
        .await
        .expect("the send runs");

    let sent = submissions(&drain(&mut seen));
    let (_, pdu) = sent.first().expect("one submit_sm");

    assert_eq!(pdu.service_type.value().as_str(), "CMT");
    assert_eq!(pdu.source_addr.as_str(), "2250102030405");
    assert_eq!(pdu.source_addr_ton, Ton::International);
    assert_eq!(pdu.destination_addr.as_str(), "3615");
    assert_eq!(pdu.dest_addr_ton, Ton::NetworkSpecific);
    assert_eq!(pdu.dest_addr_npi, smpp_core::values::Npi::National);
    assert_eq!(pdu.protocol_id, 0x42);
    assert_eq!(u8::from(pdu.priority_flag), 2);
    assert_eq!(pdu.schedule_delivery_time.as_str(), "2601011200000000");
    assert_eq!(pdu.validity_period.as_str(), "2601021200000000");
    assert_eq!(u8::from(pdu.registered_delivery), 1);
    assert_eq!(
        pdu.replace_if_present_flag,
        smpp_core::values::ReplaceIfPresentFlag::Replace
    );
    assert_eq!(pdu.sm_default_msg_id, 9);
    assert_eq!(pdu.short_message().as_ref(), b"Bonjour");
}

/// **CA-006-07** — an invalid recipient is refused before anything is written
/// and before anything is sent.
#[tokio::test(start_paused = true)]
async fn ca_006_07_an_invalid_recipient_is_refused_before_persisting_or_sending() {
    // The rejection happens at parse time, which is the point: an unparseable
    // recipient cannot even be put in a `SubmitOptions`.
    let rejection = Destination::parse("+225ABC").expect_err("letters are not a number");
    assert!(!rejection.to_string().contains("225ABC"));

    // And a request whose `service_type` does not fit is refused the same way,
    // one layer later — after the segmentation, still before the insert.
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;

    let mut submit = SubmitOptions::to(Destination::parse("+2250102030405").expect("valid"));
    submit.service_type = String::from("FAR-TOO-LONG");

    let failure = sender
        .send(&handle, &SendRequest::new("Bonjour", submit))
        .await
        .expect_err("the field does not fit");

    assert!(matches!(failure, MessagingError::Submit(_)));
    assert_eq!(journal.len().await, 0, "nothing was persisted");
    assert!(journal.events().await.is_empty());
    assert_eq!(smsc.submissions(), 0, "nothing was sent");
}

/// **CA-006-08** — a custom TLV reaches the wire with its tag and its length.
#[tokio::test(start_paused = true)]
async fn ca_006_08_a_custom_tlv_reaches_the_wire_with_its_tag_and_length() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let sender = a_sender(Journal::new());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;

    let submit =
        SubmitOptions::to(Destination::parse("+2250102030405").expect("valid")).with_tlvs(vec![
            CustomTlv::new(0x1403, vec![0xDE, 0xAD, 0xBE, 0xEF]).expect("short enough"),
        ]);

    sender
        .send(&handle, &SendRequest::new("Bonjour", submit))
        .await
        .expect("the send runs");

    let sent = submissions(&drain(&mut seen));
    let (_, pdu) = sent.first().expect("one submit_sm");

    let tlv = pdu.tlvs().first().expect("the TLV survived the round trip");

    assert_eq!(tlv.tag(), TlvTag::from(0x1403));
    assert_eq!(tlv.value_length(), 4);
}

/// The wiring milestone 005 left open: the session's charset decides how the
/// body is written.
///
/// The text is chosen so the two readings **disagree**. `@ £ $ € è é` sits at
/// different positions in GSM 03.38 and in ISO-8859-1, and `€` is an escaped
/// character in one of them; an ASCII fixture would pass under either
/// convention and prove nothing.
#[tokio::test(start_paused = true)]
async fn the_session_charset_decides_how_a_gsm_body_is_written() {
    // `€` is deliberately absent: it exists in GSM 03.38 (escaped) and NOT in
    // ISO-8859-1, so including it would make the Latin-1 run fail to encode
    // rather than encode differently — which is a different property.
    const TEXT: &str = "@ £ $ è é";

    async fn body_under(charset: Gsm7BitCharset) -> Vec<u8> {
        let (smsc, mut seen) = Smsc::always(Script::Accept);
        let sender = a_sender(Journal::new());
        let (handle, _session) = bound(a_profile(charset), smsc).await;

        sender
            .send(
                &handle,
                &SendRequest::new(
                    TEXT,
                    SubmitOptions::to(Destination::parse("+2250102030405").expect("valid")),
                )
                .with_encoding(EncodingChoice::Forced(Encoding::Gsm7Bit)),
            )
            .await
            .expect("the send runs");

        let sent = submissions(&drain(&mut seen));
        let (_, pdu) = sent.first().expect("one submit_sm").clone();

        pdu.short_message().as_ref().to_vec()
    }

    let gsm = body_under(Gsm7BitCharset::Gsm0338).await;
    let latin1 = body_under(Gsm7BitCharset::Latin1).await;

    assert_ne!(
        gsm, latin1,
        "the two charsets produced identical octets: the session setting is not reaching the encoder"
    );

    // GSM 03.38: `@` is position 0x00, `£` is 0x01, `$` is 0x02.
    assert_eq!(gsm.first(), Some(&0x00));
    // ISO-8859-1: `@` is 0x40, `£` is 0xA3, `$` is 0x24.
    assert_eq!(latin1.first(), Some(&0x40));
    assert!(latin1.contains(&0xA3), "£ must be its Latin-1 code point");
}

/// The packing setting travels the same way, and it is visible in the octet
/// count: eight septets fit in seven packed octets.
#[tokio::test(start_paused = true)]
async fn the_session_packing_decides_the_length_of_a_gsm_body() {
    async fn length_under(packing: smpp_core::values::Gsm7BitPacking) -> u8 {
        let (smsc, mut seen) = Smsc::always(Script::Accept);
        let sender = a_sender(Journal::new());
        let profile = SessionProfile::builder(SessionId::new(), "double", "in-memory", 2775)
            .system_id("esme01")
            .gsm7_packing(packing)
            .build()
            .expect("valid");

        let (handle, _session) = bound(profile, smsc).await;

        sender
            .send(&handle, &a_request("abcdefgh"))
            .await
            .expect("the send runs");

        let sent = submissions(&drain(&mut seen));
        sent.first().expect("one submit_sm").1.sm_length()
    }

    assert_eq!(
        length_under(smpp_core::values::Gsm7BitPacking::Unpacked).await,
        8
    );
    assert_eq!(
        length_under(smpp_core::values::Gsm7BitPacking::Packed).await,
        7
    );
}

/// `sar_*` mode: the concatenation goes out of band and the body carries no
/// header, so the whole `short_message` is user data.
#[tokio::test(start_paused = true)]
async fn the_sar_mode_puts_the_concatenation_in_tlvs_rather_than_in_the_body() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let sender = a_sender(Journal::new());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;

    sender
        .send(
            &handle,
            &a_request(&"a".repeat(400)).with_mode(SegmentationMode::Sar),
        )
        .await
        .expect("the send runs");

    let sent = submissions(&drain(&mut seen));
    let (_, first) = sent.first().expect("three submit_sm");

    assert_eq!(u8::from(first.esm_class), 0, "no UDHI bit in sar mode");
    assert_eq!(first.short_message().as_ref()[0], b'a', "no header");

    let tags: Vec<TlvTag> = first.tlvs().iter().map(smpp_core::tlvs::Tlv::tag).collect();
    assert!(tags.contains(&TlvTag::SarMsgRefNum));
    assert!(tags.contains(&TlvTag::SarTotalSegments));
    assert!(tags.contains(&TlvTag::SarSegmentSeqnum));
}

/// A journal that refuses the write-ahead insert stops the send dead: the
/// order exists so that a message this client could not record is a message it
/// does not send.
#[tokio::test(start_paused = true)]
async fn a_journal_that_refuses_the_insert_sends_nothing() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let journal = Journal::refusing_inserts(MessageStoreError::Unavailable {
        reason: String::from("database query failed"),
    });
    let sender = a_sender(journal);

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;

    let failure = sender
        .send(&handle, &a_request("Bonjour"))
        .await
        .expect_err("the journal refused");

    assert!(matches!(failure, MessagingError::Store(_)));
    assert_eq!(smsc.submissions(), 0, "nothing was sent");
}

/// **The bug this test exists for.** A journal that fails *after* the message
/// went out must not be reported the way one that failed *before* it is.
///
/// The sequence: three segments accepted, an identifier assigned, then the
/// database locks up on the final transitions. Propagating that as an error
/// would throw the report away and hand the caller `MESSAGE_STORAGE`, whose
/// whole meaning is "nothing was sent" — the operator resends, and the message
/// goes out twice.
///
/// So the send is reported as what it was, with `journalled = false` saying
/// the record is missing. Note what the test asserts and what it does not: the
/// row **is** still `QUEUED`, and that is the honest residual state.
#[tokio::test(start_paused = true)]
async fn a_journal_failing_after_the_send_reports_the_send_rather_than_an_error() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![SubmitReply::AcceptAs(String::from("MSG-42"))],
        SubmitReply::Accept,
    );
    let journal = Journal::refusing_transitions(MessageStoreError::Unavailable {
        reason: String::from("database is locked"),
    });
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;
    let request = a_request("Bonjour");

    let report = sender
        .send(&handle, &request)
        .await
        .expect("a post-emission journal failure is not a send failure");

    // The send happened, and the report says so — including the identifier
    // that would otherwise have been thrown away with the error.
    assert_eq!(smsc.submissions(), 1);
    assert_eq!(report.state, MessageState::Accepted);
    assert_eq!(report.smsc_message_id.as_deref(), Some("MSG-42"));

    // And the one thing the caller could not otherwise know.
    assert!(
        !report.journalled,
        "the caller must be able to tell that the record is missing"
    );

    // The residual state, asserted rather than assumed: the row is still
    // `QUEUED`, so a resume WOULD re-send it. That is the known window the
    // module header describes, not something this test papers over.
    let row = journal
        .row(request.client_message_id)
        .await
        .expect("the write-ahead row is there");
    assert_eq!(row.state, MessageState::Queued);
}

/// The counterpart: a journal that fails *before* the send still reports an
/// error, because there nothing went out. The two paths must not converge.
#[tokio::test(start_paused = true)]
async fn a_journal_failing_before_the_send_still_reports_an_error() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let sender = a_sender(Journal::refusing_inserts(MessageStoreError::Unavailable {
        reason: String::from("database is locked"),
    }));

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc.clone()).await;

    let failure = sender
        .send(&handle, &a_request("Bonjour"))
        .await
        .expect_err("nothing was sent, so this IS a failure");

    assert!(matches!(failure, MessagingError::Store(_)));
    assert_eq!(smsc.submissions(), 0);
}

/// A nominal send reports `journalled = true`, so the flag is a fact rather
/// than a constant nobody sets.
#[tokio::test(start_paused = true)]
async fn a_recorded_send_says_so() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let sender = a_sender(Journal::new());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;

    let report = sender
        .send(&handle, &a_request("Bonjour"))
        .await
        .expect("the send runs");

    assert!(report.journalled);
}

/// Replaying a send with the same `client_message_id` is refused by the
/// journal rather than duplicating the message (spec §10.5).
#[tokio::test(start_paused = true)]
async fn replaying_the_same_client_message_identifier_is_a_conflict() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request("Bonjour");

    sender
        .send(&handle, &request)
        .await
        .expect("the first send");

    let failure = sender
        .send(&handle, &request)
        .await
        .expect_err("the second insert collides");

    assert!(matches!(
        failure,
        MessagingError::Store(MessageStoreError::Conflict)
    ));
    assert_eq!(journal.len().await, 1);
}

/// The attempt number is the caller's, and it is stored as `MAX(attempts, ?)`:
/// a lower attempt replayed afterwards must not walk the counter backwards.
#[tokio::test(start_paused = true)]
async fn the_attempt_number_comes_from_the_caller_and_never_decreases() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request("Bonjour").as_attempt(3);

    sender.send(&handle, &request).await.expect("the send runs");

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("persisted");
    assert_eq!(row.attempts, 3);

    // Replaying the recorded transitions of an *earlier* attempt leaves the
    // counter where it was.
    sender
        .repository()
        .update_state(
            &messaging::message::MessageStateUpdate::new(
                request.client_message_id,
                MessageState::Sent,
            )
            .sent_at(FrozenClock::at(NOW).instant(), 1),
        )
        .await
        .expect("the transition applies");

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("persisted");
    assert_eq!(row.attempts, 3, "MAX(attempts, ?), not an assignment");
}

/// A `submit_sm` on a receiver session is refused before the PDU leaves —
/// CA-005-02, seen from the send path.
#[tokio::test(start_paused = true)]
async fn submitting_on_a_receiver_session_is_refused_without_reaching_the_socket() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let profile = SessionProfile::builder(SessionId::new(), "double", "in-memory", 2775)
        .system_id("esme01")
        .bind_mode(smpp_session::state::BindMode::Receiver)
        .build()
        .expect("valid");
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(profile, smsc.clone()).await;
    let request = a_request("Bonjour");

    let report = sender
        .send(&handle, &request)
        .await
        .expect("the send runs and reports");

    assert_eq!(report.state, MessageState::Failed);
    assert_eq!(
        report.outcomes.first(),
        Some(&SegmentOutcome::Unanswered {
            failure: SubmitError::OperationNotAllowed
        })
    );
    assert!(
        !report.retryable,
        "a wrong bind type is not a transient fault"
    );
    assert_eq!(smsc.submissions(), 0);
    // The row still exists, `FAILED`, which is what makes the mistake visible
    // in the journal rather than only in a toast.
    assert_eq!(journal.len().await, 1);
}

/// UCS-2 is chosen on its own for a text GSM 7-bit cannot write, and the
/// `data_coding` on the wire says so.
#[tokio::test(start_paused = true)]
async fn a_text_outside_the_gsm_alphabet_goes_out_as_ucs2() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let journal = Journal::new();
    let sender = a_sender(journal.clone());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;
    let request = a_request("Bonjour 😀");

    sender.send(&handle, &request).await.expect("the send runs");

    let sent = submissions(&drain(&mut seen));
    assert_eq!(
        sent.first().expect("one submit_sm").1.data_coding,
        DataCoding::Ucs2
    );

    let row = journal
        .row(request.client_message_id)
        .await
        .expect("persisted");
    assert_eq!(row.data_coding, Some(DataCoding::Ucs2));
}

/// A `Seen::Bind` still arrives first: the double is the one from milestone
/// 005, and this file did not quietly bypass the bind.
#[tokio::test(start_paused = true)]
async fn the_send_runs_on_a_genuinely_bound_session() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let sender = a_sender(Journal::new());

    let (handle, _session) = bound(a_profile(Gsm7BitCharset::Gsm0338), smsc).await;

    sender
        .send(&handle, &a_request("Bonjour"))
        .await
        .expect("the send runs");

    let notes = drain(&mut seen);
    assert!(matches!(notes.first(), Some(Seen::Bind { .. })));
}
