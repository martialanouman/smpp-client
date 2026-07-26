//! Milestone 007 against a real session and the in-memory message centre.
//!
//! Every test drives `SessionHandle::request` — the one send path — rather
//! than the gate directly: the question this file answers is whether the
//! pacing and the windowing survive the hop through the outgoing queue, the
//! correlation table and the socket, and a test that called the gate would not
//! be asking it.
//!
//! **Virtual time throughout, except where it is the point.** Under
//! `start_paused = true` a ten-second campaign costs nothing and an assertion
//! about a sliding second is exact. The one exception is the throughput floor
//! of CA-007-05, which is a statement about *wall-clock* cost and would be
//! meaningless on a clock that skips ahead whenever every task is idle; it
//! says so where it is.

// A test reports its failures by panicking, and an integration test is
// compiled as its own crate rather than under `cfg(test)`, so the workspace
// `deny` would otherwise apply. `clippy.toml` reopens these under `cfg(test)`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use core::time::Duration;

use messaging::addressing::Destination;
use messaging::segmentation::{segment, ConcatenationReference, SegmentationOptions};
use messaging::submit::{build_submit_sm, SubmitOptions};
use smpp_core::codec::Pdu;
use smpp_core::values::CommandStatus;
use smpp_session::profile::SessionProfile;
use smpp_session::testing::{a_password, Script, Seen, Smsc, SubmitReply};
use smpp_session::{SessionError, SessionHandle};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// A profile pointing at the double, with the throughput settings under test.
///
/// `enquire_link_s` is turned off in every test here: a keep-alive PDU on the
/// wire is one more `Seen` note to filter out, and none of these tests is
/// about the keep-alive.
fn a_profile(throughput_tps: u32, window_size: u32, response_timeout_s: u32) -> SessionProfile {
    SessionProfile::builder(
        smpp_core::types::SessionId::new(),
        "gate",
        "in-memory",
        2775,
    )
    .system_id("esme01")
    .throughput_tps(throughput_tps)
    .window_size(window_size)
    .enquire_link_s(0)
    .response_timeout_s(response_timeout_s)
    .build()
    .expect("the fixture is valid")
}

/// The smallest legal `submit_sm`.
///
/// The body is irrelevant to every assertion in this file — what is counted is
/// PDUs, not characters — so it is one octet rather than a realistic message.
fn a_submit() -> Pdu {
    let options = SubmitOptions::to(Destination::parse("+2250102030405").expect("valid"));
    let split = segment(
        "x",
        &SegmentationOptions::default(),
        ConcatenationReference::new(1),
    )
    .expect("one character is one segment");

    Pdu::SubmitSm(build_submit_sm(&options, &split.segments()[0]).expect("a valid submit_sm"))
}

/// Fires `count` submissions concurrently and returns the set to join on.
///
/// Concurrent because that is how a campaign submits: a sequential loop is
/// bounded by the round-trip time and would exercise neither the window nor
/// the queue.
fn submit_many(
    handle: &SessionHandle,
    count: usize,
) -> JoinSet<Result<CommandStatus, SessionError>> {
    let mut tasks = JoinSet::new();

    for _ in 0..count {
        let handle = handle.clone();

        tasks.spawn(async move {
            handle
                .request(a_submit())
                .await
                .map(|response| response.status())
        });
    }

    tasks
}

/// Every instant at which the double saw a `submit_sm`, in order.
fn submit_instants(notes: &[Seen]) -> Vec<Instant> {
    notes
        .iter()
        .filter_map(|note| match note {
            Seen::Submit { at, .. } => Some(*at),
            _ => None,
        })
        .collect()
}

/// The largest number of submissions inside any **sliding** window of `width`.
///
/// Sliding, not bucketed: a burst that straddles a bucket boundary is invisible
/// to a bucketed count half the time, and the burst is what CA-007-01 is about.
fn busiest_window(instants: &[Instant], width: Duration) -> usize {
    instants
        .iter()
        .map(|start| {
            let end = *start + width;

            instants
                .iter()
                .filter(|at| **at >= *start && **at < end)
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Lets every runnable task make progress without advancing the clock.
///
/// `yield_now` once is not enough: a submission crosses two channels and three
/// tasks before it reaches the double, so a single yield observes a system
/// that is halfway there.
async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// **CA-007-01** — a thousand messages at 100 TPS take ten seconds, and no
/// sliding second on the message centre carries more than a hundred.
///
/// The two halves are one test on purpose: the elapsed time alone is satisfied
/// by a limiter that fires a hundred at once and then idles, which is exactly
/// the initial-burst bug. The sliding window alone is satisfied by a limiter
/// that is simply too slow.
#[tokio::test(start_paused = true)]
async fn ca_007_01_a_thousand_messages_at_a_hundred_tps_take_ten_seconds_and_never_burst() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let session = smpp_session::spawn(a_profile(100, 50, 30), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let started = Instant::now();
    let mut tasks = submit_many(&session.handle, 1_000);

    while let Some(joined) = tasks.join_next().await {
        assert_eq!(
            joined.expect("the task ran").expect("the centre answered"),
            CommandStatus::EsmeRok
        );
    }

    let elapsed = Instant::now().saturating_duration_since(started);
    let instants = submit_instants(&smpp_session::testing::drain(&mut seen));

    assert_eq!(instants.len(), 1_000, "every message must reach the centre");

    // 999 intervals of 10 ms: the first PDU leaves immediately. ±5 % of ten
    // seconds is 9.5 s to 10.5 s.
    assert!(
        elapsed >= Duration::from_millis(9_500) && elapsed <= Duration::from_millis(10_500),
        "1 000 messages at 100 TPS took {elapsed:?}"
    );

    assert!(
        busiest_window(&instants, Duration::from_secs(1)) <= 100,
        "a sliding second carried {} submissions, not 100",
        busiest_window(&instants, Duration::from_secs(1))
    );

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **CA-007-02** — with a window of ten and a message centre that reads but
/// never answers, exactly ten PDUs go out and the eleventh waits.
#[tokio::test(start_paused = true)]
async fn ca_007_02_a_full_window_stops_the_sender_at_exactly_window_size() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(Vec::new(), SubmitReply::Silent);
    let session = smpp_session::spawn(a_profile(0, 10, 60), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let mut tasks = submit_many(&session.handle, 40);

    settle().await;

    // Far longer than any hand-off needs, and far shorter than the sixty-second
    // response timeout that would free a slot.
    tokio::time::sleep(Duration::from_secs(5)).await;
    settle().await;

    let notes = smpp_session::testing::drain(&mut seen);

    assert_eq!(
        submit_instants(&notes).len(),
        10,
        "the window is ten, so exactly ten PDUs may be in flight"
    );
    assert_eq!(session.handle.window().in_use(), 10);
    assert_eq!(session.handle.in_flight().await, 10);

    tasks.abort_all();
    while tasks.join_next().await.is_some() {}

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **CA-007-03** — the slot a silent message centre never releases comes back
/// on the response timeout, and the sender resumes.
///
/// The window is one so the arithmetic is unambiguous: one PDU per timeout
/// period, and nothing in between.
#[tokio::test(start_paused = true)]
async fn ca_007_03_a_response_timeout_releases_the_slot_and_the_sender_resumes() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(Vec::new(), SubmitReply::Silent);
    let session = smpp_session::spawn(a_profile(0, 1, 5), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let mut tasks = submit_many(&session.handle, 3);

    settle().await;
    assert_eq!(
        submit_instants(&smpp_session::testing::drain(&mut seen)).len(),
        1,
        "a window of one admits one"
    );

    // Past the response timeout: the entry is swept, the waiter is failed, and
    // its permit is dropped.
    tokio::time::sleep(Duration::from_secs(6)).await;
    settle().await;

    assert!(
        !submit_instants(&smpp_session::testing::drain(&mut seen)).is_empty(),
        "the timeout must free the slot and let the next message out"
    );

    while let Some(joined) = tasks.join_next().await {
        assert!(
            matches!(
                joined.expect("the task ran"),
                Err(SessionError::ResponseTimeout { .. })
            ),
            "a silent centre times every submission out"
        );
    }

    // CA-007-10, on the timeout path: nothing is left holding a slot.
    assert_eq!(session.handle.window().in_use(), 0);

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **CA-007-04** — an unlimited target introduces no delay at all.
#[tokio::test(start_paused = true)]
async fn ca_007_04_an_unlimited_target_adds_no_delay() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let session = smpp_session::spawn(a_profile(0, 50, 30), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let started = Instant::now();
    let mut tasks = submit_many(&session.handle, 500);

    while let Some(joined) = tasks.join_next().await {
        joined.expect("the task ran").expect("the centre answered");
    }

    assert_eq!(
        Instant::now().saturating_duration_since(started),
        Duration::ZERO,
        "an unlimited session must not sleep"
    );
    assert_eq!(session.handle.target_tps(), 0);

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **CA-007-08** — the round-trip time reported is the latency the message
/// centre was given, and the window occupancy is the real one.
#[tokio::test(start_paused = true)]
async fn ca_007_08_the_reported_metrics_match_what_actually_happened() {
    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.with_latency(Duration::from_millis(40));
    let session = smpp_session::spawn(a_profile(0, 50, 30), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let mut tasks = submit_many(&session.handle, 200);

    while let Some(joined) = tasks.join_next().await {
        joined.expect("the task ran").expect("the centre answered");
    }

    let metrics = session.handle.metrics().await;

    assert_eq!(metrics.submitted, 200);
    assert_eq!(metrics.accepted, 200);
    assert_eq!(metrics.rejected, 0);
    assert!(
        (metrics.rtt_ms - 40.0).abs() <= 2.0,
        "the injected latency is 40 ms; the client reports {} ms",
        metrics.rtt_ms
    );

    // Everything answered, so nothing holds a slot.
    assert_eq!(metrics.window_in_use, 0);
    assert_eq!(metrics.window_size, 50);
    assert!((metrics.window_occupancy - 0.0).abs() < f64::EPSILON);

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **The point of this milestone that a counter cannot prove.** An
/// `ESME_RTHROTTLED` must slow the wire down: after the first refusal, the
/// message centre must not see another submission until the cooling-off period
/// has run.
#[tokio::test(start_paused = true)]
async fn a_throttled_submission_stops_the_next_one_reaching_the_message_centre() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(
        vec![SubmitReply::Reject(CommandStatus::EsmeRthrottled)],
        SubmitReply::Accept,
    );

    // Unlimited and a wide window, so the only thing that can delay a
    // submission is the throttling penalty.
    let session = smpp_session::spawn(a_profile(0, 50, 30), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let first = session
        .handle
        .request(a_submit())
        .await
        .expect("the centre answered");
    assert_eq!(first.status(), CommandStatus::EsmeRthrottled);

    let refused_at = Instant::now();
    let second = session
        .handle
        .request(a_submit())
        .await
        .expect("the centre answered");
    assert_eq!(second.status(), CommandStatus::EsmeRok);

    let waited = Instant::now().saturating_duration_since(refused_at);

    assert!(
        waited >= rate_control::DEFAULT_THROTTLE_COOLDOWN,
        "the sender resumed after {waited:?}, before the cooling-off period was over"
    );

    let instants = submit_instants(&smpp_session::testing::drain(&mut seen));
    assert_eq!(instants.len(), 2);
    assert!(
        instants[1].saturating_duration_since(instants[0])
            >= rate_control::DEFAULT_THROTTLE_COOLDOWN,
        "the pause has to be visible on the wire, not only in the client"
    );

    let metrics = session.handle.metrics().await;
    assert_eq!(metrics.throttled, 1);
    assert_eq!(metrics.accepted, 1);

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **CA-007-06** — feeding faster than the message centre consumes does not
/// grow anything: the window bounds what is in flight, the outgoing queue is
/// bounded by construction, and the correlation table never exceeds the window.
///
/// What this does **not** claim: it is not the five-minute resident-memory
/// measurement CA-007-06 describes. That needs a real clock and a real
/// process, and it belongs with the load runs of milestone 017. What is
/// checkable here — and what actually decides whether memory grows — is that
/// every structure on the path has a bound and stays inside it while a
/// producer far outruns the consumer.
#[tokio::test(start_paused = true)]
async fn ca_007_06_a_producer_outrunning_the_centre_grows_nothing() {
    let (smsc, mut seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(Vec::new(), SubmitReply::Silent);
    let session = smpp_session::spawn(a_profile(0, 20, 300), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    // Ten thousand submissions against a centre that answers none of them.
    let mut tasks = submit_many(&session.handle, 10_000);

    for _ in 0..20 {
        settle().await;
        tokio::time::sleep(Duration::from_secs(1)).await;

        assert!(
            session.handle.window().in_use() <= 20,
            "the window overflowed: {} in use",
            session.handle.window().in_use()
        );
        assert!(
            session.handle.in_flight().await <= 20,
            "the correlation table grew past the window: {}",
            session.handle.in_flight().await
        );
    }

    assert_eq!(
        submit_instants(&smpp_session::testing::drain(&mut seen)).len(),
        20,
        "ten thousand submitters, twenty PDUs on the wire"
    );

    tasks.abort_all();
    while tasks.join_next().await.is_some() {}

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **CA-007-10** — a long run mixing acceptances, rejections and timeouts
/// leaves the window at exactly zero.
///
/// Three thousand rather than the hundred thousand the criterion names: the
/// hundred-thousand run is in `rate-control`'s property tests, where a slot is
/// taken and released with no session in the way. This one is about the
/// *session* path — the correlation table, the sweep, the abandoned waiter —
/// and three thousand exercises every one of them many times over.
#[tokio::test(start_paused = true)]
async fn ca_007_10_a_long_run_of_mixed_outcomes_leaks_no_window_slot() {
    let mut replies = Vec::with_capacity(3_000);

    for index in 0..3_000_u32 {
        replies.push(match index % 3 {
            0 => SubmitReply::Accept,
            1 => SubmitReply::Reject(CommandStatus::EsmeRinvdstadr),
            _ => SubmitReply::Silent,
        });
    }

    let (smsc, _seen) = Smsc::always(Script::Accept);
    let smsc = smsc.answering_submits_with(replies, SubmitReply::Accept);
    let session = smpp_session::spawn(a_profile(0, 25, 2), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let mut tasks = submit_many(&session.handle, 3_000);
    let mut accepted = 0_u32;
    let mut rejected = 0_u32;
    let mut timed_out = 0_u32;

    while let Some(joined) = tasks.join_next().await {
        match joined.expect("the task ran") {
            Ok(CommandStatus::EsmeRok) => accepted += 1,
            Ok(_) => rejected += 1,
            Err(SessionError::ResponseTimeout { .. }) => timed_out += 1,
            Err(other) => panic!("unexpected failure: {other}"),
        }
    }

    assert_eq!(accepted + rejected + timed_out, 3_000);
    assert!(
        accepted > 0 && rejected > 0 && timed_out > 0,
        "all three paths must be exercised"
    );

    assert_eq!(
        session.handle.window().in_use(),
        0,
        "a window slot leaked over three thousand mixed outcomes"
    );
    assert_eq!(session.handle.in_flight().await, 0);

    let metrics = session.handle.metrics().await;
    assert_eq!(metrics.submitted, 3_000);
    assert_eq!(u32::try_from(metrics.accepted).unwrap(), accepted);
    assert_eq!(u32::try_from(metrics.timed_out).unwrap(), timed_out);

    session.handle.shutdown().await.expect("clean shutdown");
}

/// **CA-007-05** — the machinery sustains well past a thousand messages a
/// second.
///
/// **The only test in this file on the real clock**, and it has to be: virtual
/// time advances the moment every task is idle, so a throughput measured under
/// it would say "infinity" and mean nothing.
///
/// The limiter is set to unlimited on purpose. The criterion is about what the
/// windowing, the correlation table and the socket cost per message — whether
/// a *configured* 1 000 TPS is honoured is CA-007-01's question, at a rate the
/// virtual clock can answer exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ca_007_05_the_send_path_sustains_more_than_a_thousand_messages_a_second() {
    const MESSAGES: usize = 5_000;

    let (smsc, _seen) = Smsc::always(Script::Accept);
    let session = smpp_session::spawn(a_profile(0, 200, 30), a_password(), smsc);

    smpp_session::testing::wait_until_bound(&session.handle).await;

    let started = std::time::Instant::now();
    let handle = Arc::new(session.handle.clone());
    let mut tasks = JoinSet::new();

    // Two hundred submitters, each sending its share in sequence — the shape a
    // campaign has, rather than five thousand simultaneous tasks whose
    // scheduling would dominate the measurement.
    for _ in 0..200 {
        let handle = Arc::clone(&handle);

        tasks.spawn(async move {
            for _ in 0..(MESSAGES / 200) {
                handle
                    .request(a_submit())
                    .await
                    .expect("the centre answered");
            }
        });
    }

    while let Some(joined) = tasks.join_next().await {
        joined.expect("the task ran");
    }

    let elapsed = started.elapsed();
    let rate = MESSAGES as f64 / elapsed.as_secs_f64();

    assert!(
        rate >= 1_000.0,
        "{MESSAGES} messages in {elapsed:?} is {rate:.0} TPS, below the 1 000 TPS floor of ENF-PERF-01"
    );

    let metrics = session.handle.metrics().await;
    assert_eq!(metrics.submitted, MESSAGES as u64);
    assert_eq!(metrics.accepted, MESSAGES as u64);
    assert_eq!(metrics.window_in_use, 0, "no slot was lost at rate");

    session.handle.shutdown().await.expect("clean shutdown");
}
