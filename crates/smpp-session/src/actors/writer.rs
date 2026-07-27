//! The regulated send path (deliverable L-007-03).
//!
//! Spec §9.3 gives the writer as pseudocode:
//!
//! ```text
//! recv → token → window slot → sequence → oneshot → write → persist SENT
//! ```
//!
//! # Where that lives, and why it is not in the socket loop
//!
//! The first four steps are here, in [`SendGate`], and [`SendGate::admit`] is
//! called by [`crate::SessionHandle::request`] — *before* the PDU is put on
//! the outgoing queue, not after it is taken off.
//!
//! The order of the pseudocode is what forces it. `sequence` and `oneshot`
//! come **after** the token and the slot, and in this crate both of those are
//! `Pending::register`, which starts the response timeout ticking. Regulating
//! inside the supervisor's socket loop would mean registering first and pacing
//! second: a message waiting five seconds for a token at 1 TPS would have
//! burned half of its ten-second response budget before its PDU ever reached
//! the socket, and would then time out against a message centre that answered
//! perfectly promptly. That is not a hypothetical ordering nicety — it is the
//! difference between a slow session and a session that reports failures.
//!
//! Putting it here has a second consequence worth naming: the outgoing queue
//! can no longer hold more than `window_size` message PDUs, because nothing
//! reaches it without a slot. The queue stops being the thing that has to be
//! sized against a campaign, and becomes what its header always claimed it was
//! — a hand-off.
//!
//! # What is regulated, and what deliberately is not
//!
//! Message-bearing operations only: `submit_sm`, `submit_multi`, `data_sm`,
//! `broadcast_sm`. The keep-alive, the unbind and the responses the reader
//! queues are **not**.
//!
//! That exclusion is load-bearing. Put `enquire_link` under the window and a
//! full window stops the keep-alive; two missed periods later the supervisor
//! declares the link dead and reconnects a session whose only problem was that
//! it was busy (CA-005-04 would fire on a healthy link). Put `unbind` under
//! the rate limiter and a shutdown at 1 TPS waits a second before it can even
//! say goodbye. Neither PDU carries a message, so neither is what
//! `throughput_tps` is about.
//!
//! # The permit's lifetime is the request's lifetime
//!
//! [`SendGate::admit`] returns a [`WindowPermit`] that the caller holds until
//! the response arrives, times out, or the caller's future is dropped. The
//! slot comes back in `Drop`, so all three are the same path — see
//! `rate_control::window` for why that is the design rather than a
//! convenience.

use std::sync::Arc;

use core::time::Duration;

use rate_control::{RateLimiter, SendWindow, ThroughputConfig, WindowPermit};
use smpp_core::status_codes::{self, StatusClass};
use smpp_core::values::{CommandId, CommandStatus};

use crate::error::SessionError;
use crate::metrics::{MetricsSnapshot, ResponseOutcome, SessionMetrics};

/// The two constraints of spec §9.2, applied jointly, plus the meter.
///
/// One per session, shared by every task that submits.
#[derive(Debug)]
pub(crate) struct SendGate {
    limiter: RateLimiter,
    window: SendWindow,
    metrics: Arc<SessionMetrics>,
}

impl SendGate {
    /// A gate for a profile's `throughput_tps` and `window_size`.
    ///
    /// # Why this cannot fail
    ///
    /// `SessionProfile::build` already refuses a `window_size` outside
    /// `1..=1000` and a floor above the target, so nothing that reaches here
    /// is invalid. Were this fallible, `spawn` would have to be too — and a
    /// session that refused to start over a setting the profile had already
    /// accepted would report `CLOSED` with no way to say why.
    ///
    /// A value that somehow got through is therefore **clamped and logged**,
    /// not silently accepted: a window of one still sends, one PDU at a time,
    /// which is the safest thing a misconfigured session can do.
    pub(crate) fn new(
        throughput: ThroughputConfig,
        window_size: u32,
        metrics: Arc<SessionMetrics>,
    ) -> Self {
        let limiter = RateLimiter::new(throughput).unwrap_or_else(|error| {
            tracing::error!(
                error = %error,
                "the throughput settings did not survive validation; falling back to unlimited"
            );

            RateLimiter::unlimited()
        });

        let window = SendWindow::new(window_size).unwrap_or_else(|error| {
            tracing::error!(
                error = %error,
                "the window size did not survive validation; falling back to a window of one"
            );

            SendWindow::single()
        });

        Self {
            limiter,
            window,
            metrics,
        }
    }

    /// Waits for a token and a window slot, when `operation` needs them.
    ///
    /// Returns `None` for an unregulated operation — the keep-alive, the
    /// unbind — which is not the same as a refusal: nothing is ever refused
    /// here. A full window or a spent quota produces **waiting**, which is
    /// what carries back-pressure to whoever is producing (CLAUDE.md §4).
    ///
    /// # Errors
    ///
    /// [`SessionError::Closed`] if the window was closed while waiting, which
    /// only happens on shutdown.
    pub(crate) async fn admit(
        &self,
        operation: CommandId,
    ) -> Result<Option<WindowPermit>, SessionError> {
        if !is_regulated(operation) {
            return Ok(None);
        }

        // Spec §9.3, in its order: the token, then the slot.
        self.limiter.acquire().await;

        // `WindowClosed` is the only failure `acquire` has, and it means the
        // session has stopped — which is exactly `Closed` in this crate's
        // vocabulary.
        let permit = self
            .window
            .acquire()
            .await
            .map_err(|_| SessionError::Closed)?;

        self.metrics.record_submitted().await;

        Ok(Some(permit))
    }

    /// Records what a regulated request produced, and reacts to it.
    ///
    /// The one place `ESME_RTHROTTLED` becomes a change to the wire rather
    /// than a line in a report: a throttling status penalises the limiter, and
    /// the next submission waits out the cooling-off period.
    pub(crate) async fn settle(
        &self,
        operation: CommandId,
        round_trip: Duration,
        outcome: &Result<smpp_core::codec::Command, SessionError>,
    ) {
        if !is_regulated(operation) {
            return;
        }

        let outcome = match outcome {
            Ok(response) => classify(response.status()),
            Err(_) => ResponseOutcome::Unanswered,
        };

        if outcome == ResponseOutcome::Throttled {
            self.limiter.penalise();
        }

        self.metrics.record_response(outcome, round_trip).await;
    }

    /// The window, for whoever needs to read its occupancy.
    pub(crate) const fn window(&self) -> &SendWindow {
        &self.window
    }

    /// Everything spec §18.1 lists, read at this instant.
    ///
    /// The two fields the meter cannot know — the configured target and
    /// whether a throttling penalty is in force — are filled in here, from the
    /// limiter itself rather than from a copy kept alongside it.
    pub(crate) async fn snapshot(&self) -> MetricsSnapshot {
        let factor = self.limiter.factor().await;
        let mut snapshot = self.metrics.snapshot_with(&self.window, factor).await;

        snapshot.target_tps = self.limiter.target_tps();
        snapshot.backing_off = self.limiter.is_penalised();

        snapshot
    }

    /// The target throughput in force. Zero means unlimited.
    pub(crate) const fn target_tps(&self) -> u32 {
        self.limiter.target_tps()
    }

    /// Wakes every sender waiting for a slot, with [`SessionError::Closed`].
    ///
    /// Called when the session stops. Requests in flight are already failed by
    /// `Pending::fail_all`, which releases their permits; this covers the ones
    /// that never got a slot in the first place, and would otherwise wait for
    /// a release that is not coming.
    pub(crate) fn close(&self) {
        self.window.close();
    }
}

/// Whether an operation consumes a token and a window slot.
///
/// See the module header: message-bearing PDUs only.
const fn is_regulated(operation: CommandId) -> bool {
    matches!(
        operation,
        CommandId::SubmitSm | CommandId::SubmitMulti | CommandId::DataSm | CommandId::BroadcastSm
    )
}

/// How a response status lands in the counters.
fn classify(status: CommandStatus) -> ResponseOutcome {
    if status == CommandStatus::EsmeRok {
        return ResponseOutcome::Accepted;
    }

    // The classification of milestone 003, read rather than re-derived:
    // `ESME_RTHROTTLED` and `ESME_RMSGQFUL` are both `Throttling`, and a
    // hand-written list here would be a second one to keep in step.
    if status_codes::classify(status) == StatusClass::Throttling {
        ResponseOutcome::Throttled
    } else {
        ResponseOutcome::Rejected
    }
}

#[cfg(test)]
// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use smpp_core::codec::{Command, Pdu};
    use tokio::time::Instant;

    fn a_gate(target_tps: u32, window_size: u32) -> SendGate {
        SendGate::new(
            ThroughputConfig::at(target_tps),
            window_size,
            Arc::new(SessionMetrics::new()),
        )
    }

    fn a_response(status: CommandStatus) -> Result<Command, SessionError> {
        Ok(Command::new(
            status,
            1,
            Pdu::SubmitSmResp(smpp_core::pdus::SubmitSmResp::default()),
        ))
    }

    /// **The exclusion the module header argues for.** A full window must not
    /// be able to stop the keep-alive: a session that reconnects because it
    /// was busy is a self-inflicted outage.
    #[tokio::test(start_paused = true)]
    async fn the_keep_alive_passes_a_window_that_is_completely_full() {
        let gate = a_gate(1, 1);
        let held = gate
            .admit(CommandId::SubmitSm)
            .await
            .expect("open")
            .expect("a submission takes a slot");

        assert_eq!(gate.window().in_use(), 1);

        // Both would block for ever if they were regulated: the window is full
        // and the quota is one per second.
        let started = Instant::now();

        assert!(gate
            .admit(CommandId::EnquireLink)
            .await
            .expect("open")
            .is_none());
        assert!(gate.admit(CommandId::Unbind).await.expect("open").is_none());

        assert_eq!(
            Instant::now().saturating_duration_since(started),
            Duration::ZERO,
            "an unregulated operation must not wait"
        );

        drop(held);
    }

    /// Every message-bearing operation is regulated; nothing else is.
    #[test]
    fn the_regulated_set_is_the_message_bearing_operations() {
        for operation in [
            CommandId::SubmitSm,
            CommandId::SubmitMulti,
            CommandId::DataSm,
            CommandId::BroadcastSm,
        ] {
            assert!(is_regulated(operation), "{operation:?} carries a message");
        }

        for operation in [
            CommandId::EnquireLink,
            CommandId::Unbind,
            CommandId::QuerySm,
            CommandId::CancelSm,
            CommandId::ReplaceSm,
            CommandId::DeliverSmResp,
        ] {
            assert!(
                !is_regulated(operation),
                "{operation:?} does not carry a message"
            );
        }
    }

    /// **Point 3 of this milestone, at the level of the gate.** An
    /// `ESME_RTHROTTLED` must make the *next* submission wait.
    #[tokio::test(start_paused = true)]
    async fn a_throttled_response_delays_the_next_submission() {
        // Unlimited, so any delay observed is the penalty and nothing else.
        let gate = a_gate(0, 10);

        let permit = gate.admit(CommandId::SubmitSm).await.expect("open");
        gate.settle(
            CommandId::SubmitSm,
            Duration::from_millis(5),
            &a_response(CommandStatus::EsmeRthrottled),
        )
        .await;
        drop(permit);

        assert!(gate.snapshot().await.backing_off);

        let started = Instant::now();
        let _next = gate.admit(CommandId::SubmitSm).await.expect("open");

        assert_eq!(
            Instant::now().saturating_duration_since(started),
            rate_control::DEFAULT_THROTTLE_COOLDOWN,
            "the next submission must wait out the cooling-off period"
        );

        let snapshot = gate.snapshot().await;
        assert_eq!(snapshot.throttled, 1);
        assert_eq!(snapshot.rejected, 1);
    }

    /// `ESME_RMSGQFUL` is the other throttling status of spec §9.4, and it
    /// must have the same effect — the classification of milestone 003 is what
    /// answers, not a list written here.
    #[tokio::test(start_paused = true)]
    async fn a_full_message_queue_slows_the_sender_just_as_a_throttle_does() {
        let gate = a_gate(0, 10);

        gate.settle(
            CommandId::SubmitSm,
            Duration::from_millis(5),
            &a_response(CommandStatus::EsmeRmsgqful),
        )
        .await;

        assert!(gate.snapshot().await.backing_off);
    }

    /// An ordinary rejection is not a reason to slow down: an invalid
    /// destination says nothing about the message centre's capacity.
    #[tokio::test(start_paused = true)]
    async fn an_ordinary_rejection_does_not_slow_the_sender_down() {
        let gate = a_gate(0, 10);

        gate.settle(
            CommandId::SubmitSm,
            Duration::from_millis(5),
            &a_response(CommandStatus::EsmeRinvdstadr),
        )
        .await;

        assert!(!gate.snapshot().await.backing_off);

        let started = Instant::now();
        let _next = gate.admit(CommandId::SubmitSm).await.expect("open");

        assert_eq!(
            Instant::now().saturating_duration_since(started),
            Duration::ZERO
        );

        let snapshot = gate.snapshot().await;
        assert_eq!(snapshot.rejected, 1);
        assert_eq!(snapshot.throttled, 0);
    }

    /// An unanswered request is counted, and does not penalise: a timeout is
    /// not the message centre asking us to slow down.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_request_is_counted_without_a_penalty() {
        let gate = a_gate(0, 10);

        gate.settle(
            CommandId::SubmitSm,
            Duration::from_secs(10),
            &Err(SessionError::Cancelled),
        )
        .await;

        assert!(!gate.snapshot().await.backing_off);
        assert_eq!(gate.snapshot().await.timed_out, 1);
    }

    /// An unregulated operation contributes nothing to the meter: counting the
    /// keep-alive as a submission would put a floor under every idle session's
    /// throughput.
    #[tokio::test(start_paused = true)]
    async fn the_keep_alive_is_not_counted_as_a_submission() {
        let gate = a_gate(0, 10);

        for _ in 0..100 {
            assert!(gate
                .admit(CommandId::EnquireLink)
                .await
                .expect("open")
                .is_none());
            gate.settle(
                CommandId::EnquireLink,
                Duration::from_millis(1),
                &a_response(CommandStatus::EsmeRok),
            )
            .await;
        }

        let snapshot = gate.snapshot().await;

        assert_eq!(snapshot.submitted, 0);
        assert_eq!(snapshot.accepted, 0);
    }

    /// Closing wakes a sender that would otherwise wait for a slot the session
    /// is never going to give back.
    #[tokio::test(start_paused = true)]
    async fn closing_the_gate_releases_a_sender_waiting_for_a_slot() {
        let gate = Arc::new(a_gate(0, 1));
        let _held = gate.admit(CommandId::SubmitSm).await.expect("open");

        let waiting = tokio::spawn({
            let gate = Arc::clone(&gate);

            async move {
                gate.admit(CommandId::SubmitSm)
                    .await
                    .map(|permit| permit.is_some())
            }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!waiting.is_finished());

        gate.close();

        assert!(matches!(
            waiting.await.expect("the task ran"),
            Err(SessionError::Closed)
        ));
    }

    /// A setting the profile should never have let through does not stop the
    /// session: it is clamped to something that still sends. See
    /// [`SendGate::new`].
    #[tokio::test(start_paused = true)]
    async fn a_setting_that_did_not_survive_validation_is_clamped_rather_than_fatal() {
        let gate = SendGate::new(
            ThroughputConfig::at(10).with_min_tps(1_000),
            0,
            Arc::new(SessionMetrics::new()),
        );

        assert_eq!(gate.window().size(), 1, "a window of one still sends");
        assert_eq!(gate.target_tps(), 0, "and it sends unpaced rather than not");

        let permit = gate.admit(CommandId::SubmitSm).await.expect("open");

        assert!(permit.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_reports_the_settings_it_was_built_with() {
        let gate = a_gate(250, 20);

        let snapshot = gate.snapshot().await;

        assert_eq!(gate.target_tps(), 250);
        assert_eq!(gate.window().size(), 20);
        assert_eq!(snapshot.target_tps, 250);
        assert_eq!(snapshot.window_size, 20);
        assert_eq!(snapshot.adaptive_permille, 1_000);
        assert!(!snapshot.backing_off);
    }
}
