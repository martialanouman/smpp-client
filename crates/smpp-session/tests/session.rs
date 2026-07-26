//! Acceptance tests for milestone 005, one section per criterion.
//!
//! Every test runs against the in-memory message centre of `support`, on
//! Tokio's **virtual clock**: `start_paused = true` means a thirty-second
//! `enquire_link` period and a sixty-second back-off cost nothing, and — far
//! more important — that the assertions about *when* something happened are
//! exact rather than tolerant.

// See the note in `support/mod.rs`: `tests/` is compiled without `cfg(test)`,
// so the relaxations of `clippy.toml` do not reach it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod support;

use core::time::Duration;

use smpp_core::codec::Pdu;
use smpp_core::values::{CommandId, CommandStatus};
use smpp_session::state::{BindMode, SessionState};
use smpp_session::SessionError;
use support::{
    a_profile, assert_state, drain, start, tight_backoff, wait_for_code, wait_until_bound, Script,
    Seen, Smsc, PASSWORD_TEXT, QUIET_PERIOD,
};

// ---------------------------------------------------------------------------
// CA-005-01 — a successful bind reaches BOUND
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ca_005_01_a_successful_transceiver_bind_reaches_bound() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session = start(a_profile(), smsc);

    assert_eq!(
        wait_until_bound(&session.handle).await,
        BindMode::Transceiver
    );
    assert_state(&session.handle, SessionState::Bound(BindMode::Transceiver));
    assert!(session.handle.snapshot().last_error.is_none());

    let notes = drain(&mut seen);
    assert!(
        matches!(
            notes.first(),
            Some(Seen::Bind {
                operation: CommandId::BindTransceiver,
                ..
            })
        ),
        "the first PDU of a session is its bind: {notes:?}"
    );

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CA-005-02 — the three bind types, and the operations each refuses
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ca_005_02_each_bind_type_binds_with_its_own_operation() {
    for (mode, expected) in [
        (BindMode::Transmitter, CommandId::BindTransmitter),
        (BindMode::Receiver, CommandId::BindReceiver),
        (BindMode::Transceiver, CommandId::BindTransceiver),
    ] {
        let (smsc, mut seen) = Smsc::always(Script::Accept);
        let profile = a_profile();
        let profile = smpp_session::profile::SessionProfile::builder(
            profile.session_id(),
            profile.name(),
            profile.host(),
            profile.port(),
        )
        .system_id(profile.system_id())
        .bind_mode(mode)
        .build()
        .unwrap();

        let session = start(profile, smsc);

        assert_eq!(wait_until_bound(&session.handle).await, mode);
        assert!(
            matches!(
                drain(&mut seen).first(),
                Some(Seen::Bind { operation, .. }) if *operation == expected
            ),
            "{mode:?} must bind with {expected:?}"
        );

        session.handle.shutdown().await.unwrap();
    }
}

/// Submitting on a receiver session is refused **here**, with a typed error and
/// no panic, rather than by an `ESME_RINVBNDSTS` from the message centre.
#[tokio::test(start_paused = true)]
async fn ca_005_02_a_receiver_session_refuses_to_submit_without_panicking() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let profile = a_profile();
    let profile = smpp_session::profile::SessionProfile::builder(
        profile.session_id(),
        profile.name(),
        profile.host(),
        profile.port(),
    )
    .system_id(profile.system_id())
    .bind_mode(BindMode::Receiver)
    .build()
    .unwrap();

    let session = start(profile, smsc);
    wait_until_bound(&session.handle).await;

    let rejection = session
        .handle
        .request(Pdu::SubmitSm(smpp_core::pdus::SubmitSm::default()))
        .await
        .expect_err("a receiver session cannot submit");

    assert!(
        matches!(
            rejection,
            SessionError::OperationNotAllowed {
                operation: CommandId::SubmitSm,
                mode: BindMode::Receiver,
            }
        ),
        "expected a typed refusal, got {rejection:?}"
    );

    // And the session is untouched by the refusal.
    assert_state(&session.handle, SessionState::Bound(BindMode::Receiver));

    session.handle.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn ca_005_02_a_request_on_a_session_that_is_not_bound_is_refused() {
    let (smsc, _seen) = Smsc::always(Script::RefuseConnection);
    let profile = a_profile();
    let session = start(profile, smsc);

    let rejection = session
        .handle
        .request(Pdu::EnquireLink)
        .await
        .expect_err("nothing is bound");

    assert!(matches!(rejection, SessionError::NotBound { .. }));

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CA-005-03 — a fatal bind rejection opens no reconnection loop
// ---------------------------------------------------------------------------

/// The criterion, stated exactly as it is written: **zero** new connections
/// during three times the minimum back-off.
#[tokio::test(start_paused = true)]
async fn ca_005_03_an_invalid_password_stops_the_session_without_retrying() {
    let (smsc, _seen) = Smsc::always(Script::Reject(CommandStatus::EsmeRinvpaswd));
    let session = start(a_profile(), smsc.clone());

    let snapshot = wait_for_code(&session.handle, "ERROR").await;

    assert_eq!(snapshot.state, SessionState::Failed);
    assert_eq!(snapshot.give_up, Some("FATAL_STATUS"));
    assert!(
        snapshot
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("ESME_RINVPASWD")),
        "the interface must be told which status stopped the session: {snapshot:?}"
    );

    let attempts_at_failure = smsc.connections();
    assert_eq!(attempts_at_failure, 1);

    // Three times the minimum back-off, and then some.
    tokio::time::sleep(QUIET_PERIOD).await;

    assert_eq!(
        smsc.connections(),
        attempts_at_failure,
        "a fatal rejection must not be retried"
    );
    assert_state(&session.handle, SessionState::Failed);

    session.handle.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn ca_005_03_an_unknown_system_id_is_equally_fatal() {
    let (smsc, _seen) = Smsc::always(Script::Reject(CommandStatus::EsmeRinvsysid));
    let session = start(a_profile(), smsc.clone());

    wait_for_code(&session.handle, "ERROR").await;
    tokio::time::sleep(QUIET_PERIOD).await;

    assert_eq!(smsc.connections(), 1);

    session.handle.shutdown().await.unwrap();
}

/// The other half of the classification: a status milestone 003 calls
/// `Recoverable` **is** retried. Without this, "no reconnection loop" could be
/// satisfied by never reconnecting at all.
#[tokio::test(start_paused = true)]
async fn ca_005_03_a_recoverable_bind_rejection_is_retried() {
    let (smsc, _seen) = Smsc::scripted(
        vec![Script::Reject(CommandStatus::EsmeRalybnd)],
        Script::Accept,
    );
    let profile = with_backoff(a_profile(), tight_backoff());
    let session = start(profile, smsc.clone());

    wait_until_bound(&session.handle).await;

    assert_eq!(
        smsc.connections(),
        2,
        "the first bind was refused for a transient reason and had to be retried"
    );

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CA-005-04 — enquire_link, and the dead session it detects
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ca_005_04_enquire_link_is_emitted_at_the_configured_interval() {
    const INTERVAL: Duration = Duration::from_secs(10);

    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let profile = builder(a_profile())
        .enquire_link_s(10)
        .response_timeout_s(3)
        .build()
        .unwrap();
    let session = start(profile, smsc);

    wait_until_bound(&session.handle).await;

    tokio::time::sleep(INTERVAL * 5 + INTERVAL / 2).await;

    let ticks: Vec<_> = drain(&mut seen)
        .into_iter()
        .filter_map(|note| match note {
            Seen::EnquireLink(at) => Some(at),
            _ => None,
        })
        .collect();

    assert!(
        ticks.len() >= 5,
        "five periods must produce at least five enquire_link, saw {}",
        ticks.len()
    );

    // ±10 %, as the criterion states it.
    let tolerance = INTERVAL / 10;
    for pair in ticks.windows(2) {
        let elapsed = pair[1] - pair[0];

        assert!(
            elapsed >= INTERVAL - tolerance && elapsed <= INTERVAL + tolerance,
            "an interval of {elapsed:?} is outside 10 % of {INTERVAL:?}"
        );
    }

    session.handle.shutdown().await.unwrap();
}

/// **The failure this criterion exists for.** The socket never closes: the
/// message centre reads normally and simply stops answering. Nothing at the TCP
/// level is wrong, and only the missing `enquire_link_resp` reveals it.
#[tokio::test(start_paused = true)]
async fn ca_005_04_a_session_that_stops_answering_is_declared_dead() {
    let (smsc, _seen) = Smsc::scripted(vec![Script::AcceptThenGoSilent], Script::Accept);
    let profile = builder(a_profile())
        .enquire_link_s(5)
        .response_timeout_s(2)
        .reconnect(tight_backoff())
        .build()
        .unwrap();

    let session = start(profile, smsc.clone());

    wait_until_bound(&session.handle).await;
    assert_eq!(smsc.connections(), 1);

    // The session must leave BOUND on its own, with the socket still open.
    wait_for_code(&session.handle, "RECONNECT").await;

    // And then come back on a fresh connection.
    wait_until_bound(&session.handle).await;
    assert!(smsc.connections() >= 2);

    session.handle.shutdown().await.unwrap();
}

/// **Regression, and the exact configuration that used to hide the hole.**
///
/// The original test ran `enquire_link_s = 5` with `response_timeout_s = 2` —
/// the only ordering under which the defect is invisible. Reverse the two and
/// nothing was ever detected: the tick overwrote the outstanding waiter, its
/// correlation entry was swept because the receiver had gone, the missed
/// counter stayed at zero for ever, and a black-holed session with a healthy
/// socket sat `BOUND` until someone noticed by hand.
///
/// The pair is now refused at construction — `response_timeout_s` must be
/// strictly under `enquire_link_s`, so an `enquire_link` always reaches a
/// verdict before the next one is due.
#[test]
fn ca_005_04_a_response_timeout_that_outlives_the_keep_alive_period_is_refused() {
    let refused = builder(a_profile())
        .enquire_link_s(10)
        .response_timeout_s(30)
        .build()
        .expect_err("an enquire_link that cannot time out before the next one detects nothing");

    assert_eq!(
        refused.to_string(),
        "invalid value for `response_timeout_s`: value contradicts another setting"
    );

    // Equal is refused too: the verdict and the next tick would land together,
    // which is a race rather than a margin.
    assert!(builder(a_profile())
        .enquire_link_s(10)
        .response_timeout_s(10)
        .build()
        .is_err());

    // Strictly under is what a working keep-alive needs.
    assert!(builder(a_profile())
        .enquire_link_s(10)
        .response_timeout_s(9)
        .build()
        .is_ok());

    // And the rule does not apply when the keep-alive is off: there is no
    // period for the timeout to outlive.
    assert!(builder(a_profile())
        .enquire_link_s(0)
        .response_timeout_s(300)
        .build()
        .is_ok());
}

/// The same black-hole scenario at the magnitudes it was reported with — a
/// ten-second period against a distant message centre — rather than the
/// five-second one the original test used.
#[tokio::test(start_paused = true)]
async fn ca_005_04_a_long_period_still_detects_a_black_holed_session() {
    let (smsc, _seen) = Smsc::scripted(vec![Script::AcceptThenGoSilent], Script::Accept);
    let profile = builder(a_profile())
        .enquire_link_s(10)
        .response_timeout_s(3)
        .reconnect(tight_backoff())
        .build()
        .unwrap();

    let session = start(profile, smsc.clone());

    wait_until_bound(&session.handle).await;
    assert_eq!(smsc.connections(), 1);

    wait_for_code(&session.handle, "RECONNECT").await;
    wait_until_bound(&session.handle).await;

    assert!(smsc.connections() >= 2);

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CA-005-05 — back-off: growing, bounded, and not all identical
// ---------------------------------------------------------------------------

/// The reconnection is driven from a real socket drop, and the intervals are
/// measured on the virtual clock between successive connection attempts.
///
/// # Why the double refuses every attempt after the first
///
/// It used to accept and drop *every* connection, and the test still asserted
/// growth — which it only saw because the attempt counter was never reset. Now
/// that a successful bind starts the back-off over, a script of
/// bind-then-drop produces a flat one-second retry for ever, which is the
/// **correct** behaviour and no longer this criterion's subject.
///
/// The back-off grows across *consecutive failures*. So: one brutal drop to
/// prove a lost socket triggers the reconnection at all, then a peer that will
/// not answer, which is what makes the attempts consecutive failures.
#[tokio::test(start_paused = true)]
async fn ca_005_05_a_dropped_socket_is_retried_with_a_growing_bounded_back_off() {
    let (smsc, _seen) = Smsc::scripted(vec![Script::AcceptThenDrop], Script::RefuseConnection);
    let policy = smpp_session::reconnect::ReconnectPolicy::new(true, 1, 8, true).unwrap();
    let profile = builder(a_profile())
        .enquire_link_s(0)
        .reconnect(policy)
        .build()
        .unwrap();

    let session = start(profile, smsc.clone());

    let mut marks = Vec::new();
    let mut previous = 0;

    // Walk the clock forward and note when each new attempt appeared.
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_millis(100)).await;

        let attempts = smsc.connections();

        if attempts != previous {
            marks.push((attempts, tokio::time::Instant::now()));
            previous = attempts;
        }

        if attempts >= 7 {
            break;
        }
    }

    assert!(
        marks.len() >= 6,
        "expected several reconnections, saw {}",
        marks.len()
    );

    let gaps: Vec<Duration> = marks.windows(2).map(|pair| pair[1].1 - pair[0].1).collect();

    // Bounded: nothing beyond the ceiling, plus the slack of the sampling.
    for gap in &gaps {
        assert!(
            *gap <= policy.max_backoff() + Duration::from_millis(200),
            "a gap of {gap:?} exceeds the ceiling of {:?}",
            policy.max_backoff()
        );
    }

    // Growing: the last gap is longer than the first.
    let first = gaps.first().copied().unwrap();
    let last = gaps.last().copied().unwrap();
    assert!(
        last > first,
        "the back-off must grow: first {first:?}, last {last:?}"
    );

    // And not all identical, which is what the jitter is for.
    assert!(
        gaps.windows(2).any(|pair| pair[0] != pair[1]),
        "without jitter every gap would be the same: {gaps:?}"
    );

    session.handle.shutdown().await.unwrap();
}

/// **Regression: the attempt counter is reset by a successful bind.**
///
/// It used to only ever grow. Six failures while a VPN came up left it at six,
/// and the first blip after a whole day of healthy operation waited the
/// sixty-second ceiling instead of one second — then a minute again for every
/// blip after that. No existing test saw it, because none of them ever held a
/// bind and then lost it: they either fail from the start or succeed and stop.
///
/// The assertion is on the *delay*, measured on the virtual clock between the
/// drop and the next connection attempt. With the counter left at four it
/// would be the ceiling; reset, it is the first step.
#[tokio::test(start_paused = true)]
async fn ca_005_05_a_successful_bind_resets_the_back_off() {
    let policy = smpp_session::reconnect::ReconnectPolicy::new(true, 1, 60, false).unwrap();
    let profile = builder(a_profile())
        .enquire_link_s(5)
        .response_timeout_s(2)
        .reconnect(policy)
        .build()
        .unwrap();

    // Four refused connections, then one that binds and *stays* bound long
    // enough to be observed — a session that dropped instantly would already
    // have reconnected by the time the test looked at it.
    let (smsc, _seen) = Smsc::scripted(
        vec![
            Script::RefuseConnection,
            Script::RefuseConnection,
            Script::RefuseConnection,
            Script::RefuseConnection,
            Script::AcceptThenGoSilent,
        ],
        Script::Accept,
    );

    let session = start(profile, smsc.clone());

    // The fifth attempt binds; the counter has climbed to four by then.
    wait_until_bound(&session.handle).await;
    assert_eq!(smsc.connections(), 5);

    // The counterfactual, stated: left un-reset, the next failure would wait
    // this long.
    assert_eq!(policy.base_delay(5), Duration::from_secs(16));

    // The peer then goes silent and the keep-alive tears the session down.
    wait_for_code(&session.handle, "RECONNECT").await;
    let lost_at = tokio::time::Instant::now();

    while smsc.connections() < 6 {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let waited = tokio::time::Instant::now() - lost_at;

    assert!(
        waited < Duration::from_secs(2),
        "a successful bind must start the back-off over: waited {waited:?}, \
         which is the {:?} of an un-reset counter",
        policy.base_delay(5)
    );

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CA-005-06 — a response that never comes frees its entry
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ca_005_06_a_request_that_is_never_answered_leaves_no_entry_behind() {
    let (smsc, _seen) = Smsc::always(Script::AcceptThenGoSilent);
    let profile = builder(a_profile())
        // The keep-alive would tear the session down before the request could
        // time out; this test is about the correlation table, not about
        // liveness.
        .enquire_link_s(0)
        .response_timeout_s(3)
        .build()
        .unwrap();

    let session = start(profile, smsc);
    wait_until_bound(&session.handle).await;

    let outcome = session.handle.request(Pdu::EnquireLink).await;

    assert!(
        matches!(outcome, Err(SessionError::ResponseTimeout { .. })),
        "expected a timeout, got {outcome:?}"
    );
    assert_eq!(
        session.handle.in_flight().await,
        0,
        "the correlation table must come back to zero"
    );

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CA-005-07 — a malformed PDU does not kill the session
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ca_005_07_a_malformed_pdu_is_nacked_and_the_session_stays_bound() {
    let (smsc, mut seen) = Smsc::always(Script::AcceptThenSendGarbage);
    let profile = builder(a_profile())
        .enquire_link_s(5)
        .response_timeout_s(2)
        .build()
        .unwrap();
    let session = start(profile, smsc.clone());

    wait_until_bound(&session.handle).await;

    // Let the garbage arrive and the answer come back.
    tokio::time::sleep(Duration::from_secs(12)).await;

    let notes = drain(&mut seen);
    assert!(
        notes.contains(&Seen::GenericNack),
        "a malformed PDU must be answered with generic_nack: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|note| matches!(note, Seen::EnquireLink(_))),
        "the session must still be working afterwards: {notes:?}"
    );

    assert_state(&session.handle, SessionState::Bound(BindMode::Transceiver));
    assert_eq!(
        smsc.connections(),
        1,
        "a malformed PDU must not cost a reconnection"
    );

    session.handle.shutdown().await.unwrap();
}

/// An `unbind` from the message centre is answered and closes the session — it
/// is not a lost link, so it must not open a reconnection loop.
///
/// Deliberately **not** waiting for `BOUND` first: the double unbinds in the
/// same breath as it accepts, and on the virtual clock the whole exchange is
/// over before the test gets to look. Asserting on a state that has already
/// gone by is how a test starts depending on scheduling.
#[tokio::test(start_paused = true)]
async fn ca_005_07_an_unbind_from_the_message_centre_closes_the_session_cleanly() {
    let (smsc, mut seen) = Smsc::always(Script::AcceptThenUnbind);
    let session = start(a_profile(), smsc.clone());

    wait_for_code(&session.handle, "UNBOUND").await;

    let notes = drain(&mut seen);
    assert!(
        matches!(notes.first(), Some(Seen::Bind { .. })),
        "the exchange starts with a bind: {notes:?}"
    );

    tokio::time::sleep(QUIET_PERIOD).await;

    assert_eq!(
        smsc.connections(),
        1,
        "a clean unbind by the peer is not a lost link"
    );
    assert_state(&session.handle, SessionState::Unbound);

    session.handle.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// CA-005-08 — a clean shutdown, and no task left behind
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ca_005_08_closing_sends_unbind_waits_for_the_response_and_joins_every_task() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session = start(a_profile(), smsc);

    wait_until_bound(&session.handle).await;

    session.handle.shutdown().await.unwrap();

    assert_state(&session.handle, SessionState::Unbound);
    assert!(
        drain(&mut seen).contains(&Seen::Unbind),
        "a clean shutdown sends unbind"
    );

    // Idempotent: nothing is left to join the second time.
    session.handle.shutdown().await.unwrap();
}

/// A message centre that never answers the `unbind` must not keep the
/// application open: the wait is bounded.
#[tokio::test(start_paused = true)]
async fn ca_005_08_a_message_centre_that_ignores_the_unbind_does_not_hold_the_session_open() {
    let (smsc, _seen) = Smsc::always(Script::AcceptThenGoSilent);
    let profile = builder(a_profile()).enquire_link_s(0).build().unwrap();
    let session = start(profile, smsc);

    wait_until_bound(&session.handle).await;

    session.handle.shutdown().await.unwrap();

    assert_state(&session.handle, SessionState::Unbound);
}

/// Dropping the last handle stops the session.
///
/// Without this the supervisor and the reader ran on with nobody able to reach
/// them: the outgoing queue never closes — the supervisor holds a `Sender` on
/// it for the reader's responses — so neither task ever noticed. Two tasks, a
/// socket and a live bind on the message centre, leaked for the life of the
/// process.
///
/// The queue of unsolicited PDUs is the observable: its sender lives in the
/// supervisor, so it closes exactly when the supervisor returns.
#[tokio::test(start_paused = true)]
async fn ca_005_08_dropping_the_last_handle_stops_every_task() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let session = start(a_profile(), smsc);

    wait_until_bound(&session.handle).await;

    let mut deliveries = session.deliveries;
    drop(session.handle);

    let closed = tokio::time::timeout(QUIET_PERIOD, deliveries.recv())
        .await
        .expect("an abandoned session must stop rather than run on unreachable");

    assert!(closed.is_none(), "the queue closes with the supervisor");
}

/// A session that was never bound still shuts down cleanly, without waiting out
/// its back-off.
#[tokio::test(start_paused = true)]
async fn ca_005_08_a_session_stuck_reconnecting_shuts_down_at_once() {
    let (smsc, _seen) = Smsc::always(Script::RefuseConnection);
    let profile = builder(a_profile())
        .reconnect(tight_backoff())
        .build()
        .unwrap();
    let session = start(profile, smsc);

    wait_for_code(&session.handle, "RECONNECT").await;

    session.handle.shutdown().await.unwrap();

    assert_state(&session.handle, SessionState::Unbound);
}

// ---------------------------------------------------------------------------
// CA-005-11 — the password reaches no log, at any level
// ---------------------------------------------------------------------------

/// Captures everything `tracing` writes during a whole bind — at `TRACE`, which
/// is the level that dumps PDUs — and searches it for the credential.
///
/// The double asserts the other half: the password *did* travel, so a green
/// test cannot mean "nothing was sent".
#[tokio::test(start_paused = true)]
// `std::io::Write` is a synchronous trait, so the buffer behind it has to be a
// synchronous lock. `clippy.toml` bans `std::sync::Mutex` because it must
// never be held across an `.await`; this one is taken and released inside a
// non-async `write`, which is the case the ban is not about.
#[allow(clippy::disallowed_types)]
async fn ca_005_11_no_password_reaches_the_traces_even_at_trace_level() {
    use std::sync::{Arc, Mutex};

    /// A writer the test owns, so the subscriber's output can be read back.
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);

            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));

    // Installed *before* the session starts, so the bind — the one PDU that
    // carries the credential — is inside the capture.
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer({
                let writer = Capture(Arc::clone(&captured));
                move || writer.clone()
            })
            .finish(),
    );

    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session = start(a_profile(), smsc);

    wait_until_bound(&session.handle).await;
    session.handle.request(Pdu::EnquireLink).await.unwrap();
    session.handle.shutdown().await.unwrap();

    let logs = String::from_utf8_lossy(&captured.lock().unwrap().clone()).into_owned();

    assert!(
        !logs.contains(PASSWORD_TEXT),
        "the password reached the traces:\n{logs}"
    );
    assert!(
        logs.contains("session bound"),
        "the capture must actually hold the session's traces:\n{logs}"
    );

    // The other direction: the credential really was sent, so the assertion
    // above is about redaction and not about an empty exchange.
    let notes = drain(&mut seen);
    assert!(
        notes.iter().any(|note| matches!(
            note,
            Seen::Bind { password, .. } if password == PASSWORD_TEXT
        )),
        "the double must have received the password: {notes:?}"
    );
}

// --- helpers ----------------------------------------------------------------

/// A builder seeded from an existing profile.
fn builder(
    profile: smpp_session::profile::SessionProfile,
) -> smpp_session::profile::ProfileBuilder {
    smpp_session::profile::SessionProfile::builder(
        profile.session_id(),
        profile.name(),
        profile.host(),
        profile.port(),
    )
    .system_id(profile.system_id())
    .bind_mode(profile.bind_mode())
}

/// The same profile with another reconnection policy.
fn with_backoff(
    profile: smpp_session::profile::SessionProfile,
    policy: smpp_session::reconnect::ReconnectPolicy,
) -> smpp_session::profile::SessionProfile {
    builder(profile).reconnect(policy).build().unwrap()
}
