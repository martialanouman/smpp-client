//! Events emitted towards the frontend.
//!
//! The reverse direction of [`crate::commands`]: the backend pushes here the
//! state changes the interface cannot derive on its own — session
//! transitions, campaign progress, metrics.
//!
//! Conventions (guide §9.3):
//!
//! - `domain:action` naming — `error:notify`, `sessions:state`,
//!   `metrics:tick`;
//! - high-frequency events are **throttled on the Rust side**. Emitting at the
//!   real PDU rate would saturate the IPC bridge and make the WebView unusable
//!   during a campaign.
//!
//! [`Throttle`] exists from milestone 001 even though only one event does,
//! because it is the attachment point for `metrics:tick` at milestone 007:
//! retro-fitting a rate limit once a dozen call sites emit freely is how a
//! bridge ends up saturated in the first place.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Runtime};
use tauri_specta::Event as _;
use tokio::sync::Mutex;

use crate::error::{ErrorCode, ErrorDto};

/// Minimum interval between two `error:notify` emissions.
///
/// A campaign that fails on every message would otherwise push thousands of
/// identical toasts through the bridge. Five per second is well above what a
/// human reads and well below what the WebView struggles with.
const ERROR_NOTIFY_INTERVAL: Duration = Duration::from_millis(200);

/// Payload of `error:notify` — an error the frontend did not ask for.
///
/// Same shape as [`ErrorDto`], and deliberately so: a failure surfaces
/// identically whether the interface provoked it or the backend ran into it on
/// its own, so the notification component has a single case to handle.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
// Without this the derive would name the event `error-notify`, from the struct
// in kebab-case. Guide §9.3 mandates `domain:action`, and the separator is not
// cosmetic: it is what lets a listener filter a whole domain later on.
#[tauri_specta(event_name = "error:notify")]
pub(crate) struct ErrorNotify {
    /// Stable identifier, translated by the frontend.
    pub(crate) code: ErrorCode,
    /// Short English sentence, for the logs.
    pub(crate) message: String,
}

impl From<&ErrorDto> for ErrorNotify {
    fn from(dto: &ErrorDto) -> Self {
        Self {
            code: dto.code,
            message: dto.message.clone(),
        }
    }
}

/// Shortest interval between two `sessions:state` emissions.
///
/// A session that flaps — connect, fail, back off, connect — publishes a state
/// change every few hundred milliseconds, and a reconnection storm across
/// several sessions (milestone 011) multiplies that. Ten per second is well
/// past what a banner needs and well under what the bridge minds.
///
/// # This is a pace, not a filter, and the distinction was a bug
///
/// It used to be a [`Throttle`]: an emission arriving inside the interval was
/// **discarded**, with nothing to replay it. Since the state of a healthy
/// session stops changing once it is `BOUND`, and the frontend does no
/// polling, the last suppressed emission was the last emission full stop.
///
/// Against a message centre on the same host the whole handshake takes
/// milliseconds — `CONNECTING` at t=0, `BINDING` at t=1 ms, `BOUND` at t=6 ms.
/// The first was admitted and the other two dropped, so the screen showed
/// `CONNECTING` for ever on a session that was bound. Exactly what CA-005-01
/// measures.
///
/// The pacing now lives in the forwarder ([`crate::sessions`]), which sleeps
/// this long and then **re-reads** the registry before emitting. A throttle at
/// the emitter cannot do that: by the time it decides, it has already been
/// handed a payload that is about to be stale.
pub(crate) const SESSIONS_STATE_INTERVAL: Duration = Duration::from_millis(100);

/// Payload of `sessions:state` — every live session and where it stands.
///
/// The whole list rather than one session's delta, which is what makes the
/// throttle above harmless: an interface that missed an emission is not out of
/// sync, it is one emission behind.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
#[tauri_specta(event_name = "sessions:state")]
pub(crate) struct SessionsState {
    /// One entry per live session (spec §15.3).
    pub(crate) sessions: Vec<crate::commands::session::SessionStatusDto>,
}

/// One message's new standing.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageUpdateEntry {
    /// The write-ahead key, so the interface knows which message moved.
    pub(crate) client_message_id: String,
    /// `QUEUED`, `SENT`, `ACCEPTED`, `FAILED` — the names of spec §14.3.
    pub(crate) state: String,
    /// The `stat` code of the delivery receipt that moved it, when one did.
    ///
    /// `None` on the send path: `QUEUED`, `SENT` and `ACCEPTED` come from this
    /// application's own progress, not from a receipt.
    pub(crate) dlr_stat: Option<String>,
}

/// Payload of `message:update` — a **batch** of messages reached a new state.
///
/// Emitted three times on a nominal send, each carrying one entry: `QUEUED`,
/// `SENT`, `ACCEPTED`. That is what CA-006-01 asks the interface to show, and a
/// command that only returned its final report could not — the three states
/// would collapse into one repaint.
///
/// # Why a batch, since milestone 008
///
/// It carried a single message until delivery receipts arrived. A message
/// centre replaying a backlog produces thousands of transitions a second, and
/// one event each is what CA-008-08 forbids: the bulk travels through the
/// paginated `logs_query`, and this channel carries **aggregated increments**.
///
/// The aggregation is the batch the receipt pipeline already commits (one
/// transaction per 200 receipts or per 250 ms), so it costs nothing extra and
/// cannot drift from what was written: the event describes exactly one commit.
///
/// # Still unthrottled, and now for a stronger reason
///
/// A throttle here would drop the last transition of a message nobody would
/// then see finish — the bug `sessions:state` already had. The rate is now
/// bounded by the pipeline's own batching instead, which is a **pacing** and
/// not a filter: nothing is discarded, several receipts become one event.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
#[tauri_specta(event_name = "message:update")]
pub(crate) struct MessageUpdate {
    /// The messages that moved, in the order they were committed.
    ///
    /// Never empty: an event announcing nothing is a repaint of the same
    /// screen.
    pub(crate) updates: Vec<MessageUpdateEntry>,
}

impl MessageUpdate {
    /// The payload for a single transition, as the send path produces one.
    pub(crate) fn single(client_message_id: String, state: String) -> Self {
        Self {
            updates: vec![MessageUpdateEntry {
                client_message_id,
                state,
                dlr_stat: None,
            }],
        }
    }
}

/// Interval between two `metrics:tick` emissions.
///
/// 250 ms — the 4 Hz ceiling of spec §15.3 and CA-007-07. Four repaints a
/// second is at the top of what a gauge needs to look live, and it is a
/// **fixed cadence**, not a rate limit applied to a stream of per-message
/// events: nothing on the send path can make it emit more often, whatever the
/// throughput.
///
/// # Why the aggregation is in the backend and not here
///
/// The payload is a *reading*, not an accumulation of deltas. `smpp_session`
/// keeps a sliding window at full rate and this tick samples it, so a tick
/// that is late, or one the interface misses, costs a frame of animation and
/// nothing else — the next one carries the current truth.
///
/// The alternative, emitting per message and averaging in the WebView, makes
/// the accuracy of every figure depend on this constant: tighten it to protect
/// the bridge and the throughput reading silently degrades. That is the
/// dependency the fiche asks to avoid.
pub(crate) const METRICS_TICK_INTERVAL: Duration = Duration::from_millis(250);

/// Payload of `metrics:tick` — one session's live figures (spec §18.1).
///
/// Every counter crosses as a `u32` rather than a `u64`. The bridge carries
/// JSON, `JSON.stringify` throws on a `BigInt`, and the alternative — a
/// 64-bit integer as a string — would make the interface parse a number it
/// only ever displays. Four billion submissions on one session is past what
/// any campaign reaches; the conversion saturates rather than wrapping, so the
/// worst case is a counter that stops climbing instead of one that resets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
#[tauri_specta(event_name = "metrics:tick")]
pub(crate) struct MetricsTick {
    /// Which session these figures belong to.
    pub(crate) session_id: String,
    /// Submissions per second over the last second (spec §9.6).
    pub(crate) tps_1s: f64,
    /// Submissions per second over the last ten seconds.
    pub(crate) tps_10s: f64,
    /// Submissions per second since the session was first bound.
    pub(crate) tps_average: f64,
    /// The highest one-second rate ever reached on this session.
    pub(crate) tps_peak: f64,
    /// The configured target. Zero means unlimited — the gauge then has no
    /// scale and the interface shows a figure rather than a bar.
    pub(crate) target_tps: u32,
    /// Slots the send window has in total (spec §9.2).
    pub(crate) window_size: u32,
    /// Slots occupied right now.
    pub(crate) window_in_use: u32,
    /// The two above as a fraction, in `0.0..=1.0`.
    pub(crate) window_occupancy: f64,
    /// Mean round-trip time of the recent responses, in milliseconds.
    pub(crate) rtt_ms: f64,
    /// How many times the session has reconnected.
    pub(crate) reconnects: u32,
    /// How long the session has been bound, in seconds.
    pub(crate) uptime_s: u32,
    /// Submissions handed to the writer.
    pub(crate) submitted: u32,
    /// Submissions the message centre accepted.
    pub(crate) accepted: u32,
    /// Submissions it refused.
    pub(crate) rejected: u32,
    /// Submissions that never got an answer.
    pub(crate) timed_out: u32,
    /// Responses carrying a throttling status (spec §9.4).
    pub(crate) throttled: u32,
    /// Whether submissions are held back by a throttling penalty right now.
    pub(crate) backing_off: bool,
    /// The adaptive factor in force, in per mille. 1 000 until milestone 012.
    pub(crate) adaptive_permille: u16,
}

impl MetricsTick {
    /// Projects a session's snapshot onto the payload.
    pub(crate) fn of(session_id: &str, snapshot: &smpp_session::metrics::MetricsSnapshot) -> Self {
        Self {
            session_id: session_id.to_owned(),
            tps_1s: snapshot.tps_1s,
            tps_10s: snapshot.tps_10s,
            tps_average: snapshot.tps_average,
            tps_peak: snapshot.tps_peak,
            target_tps: snapshot.target_tps,
            window_size: snapshot.window_size,
            window_in_use: snapshot.window_in_use,
            window_occupancy: snapshot.window_occupancy,
            rtt_ms: snapshot.rtt_ms,
            reconnects: snapshot.reconnects,
            uptime_s: narrow(snapshot.uptime_s),
            submitted: narrow(snapshot.submitted),
            accepted: narrow(snapshot.accepted),
            rejected: narrow(snapshot.rejected),
            timed_out: narrow(snapshot.timed_out),
            throttled: narrow(snapshot.throttled),
            backing_off: snapshot.backing_off,
            adaptive_permille: snapshot.adaptive_permille,
        }
    }
}

/// A counter narrowed for the bridge, saturating rather than wrapping.
fn narrow(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Rate limiter for a single event channel.
///
/// The clock is **injected** ([`Throttle::admit`] takes the instant): the
/// tests are then deterministic, with no dependency on wall-clock time
/// (CLAUDE.md §7).
#[derive(Debug)]
pub(crate) struct Throttle {
    /// Minimum delay between two admitted emissions.
    min_interval: Duration,
    /// Instant of the last admitted emission.
    ///
    /// `tokio::sync::Mutex`, not `std::sync::Mutex`: the guard is taken inside
    /// an async command and `std::sync` held across an `.await` is exactly the
    /// deadlock CLAUDE.md §4 bans.
    last_admitted: Mutex<Option<Instant>>,
}

impl Throttle {
    /// Builds a throttle admitting at most one emission per `min_interval`.
    pub(crate) const fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_admitted: Mutex::const_new(None),
        }
    }

    /// Decides whether an emission happening at `now` is admitted.
    ///
    /// Admitting updates the reference instant; refusing leaves it untouched,
    /// so a burst cannot push the next admission ever further away.
    pub(crate) async fn admit(&self, now: Instant) -> bool {
        let mut last = self.last_admitted.lock().await;

        let admitted = match *last {
            Some(previous) => now.duration_since(previous) >= self.min_interval,
            None => true,
        };

        if admitted {
            *last = Some(now);
        }

        admitted
    }
}

/// Owns the throttles and is the single place events are emitted from.
#[derive(Debug)]
pub(crate) struct EventEmitter {
    /// Rate limit of the `error:notify` channel.
    ///
    /// A throttle is right *here*: a dropped toast is a toast the user did not
    /// need, and the next failure brings its own. It is wrong for
    /// `sessions:state`, where the dropped emission may be the last one there
    /// will ever be — see [`SESSIONS_STATE_INTERVAL`].
    error_notify: Throttle,
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self {
            error_notify: Throttle::new(ERROR_NOTIFY_INTERVAL),
        }
    }
}

impl EventEmitter {
    /// Emits `error:notify`, unless the channel is saturated.
    ///
    /// A suppressed event is **logged**, never silently dropped: the toast is
    /// a convenience, the log is the record.
    pub(crate) async fn emit_error<R: Runtime>(&self, app: &AppHandle<R>, payload: &ErrorNotify) {
        if !self.error_notify.admit(Instant::now()).await {
            tracing::debug!(code = ?payload.code, "error:notify suppressed by the throttle");
            return;
        }

        if let Err(error) = payload.clone().emit(app) {
            tracing::warn!(error = %error, "failed to emit error:notify");
        }
    }

    /// Emits `message:update`.
    ///
    /// Unconditional: see [`MessageUpdate`] for why this channel has no
    /// throttle. An **empty** batch is dropped rather than emitted — it would
    /// be a repaint of the same screen, and the receipt pipeline never produces
    /// one, so this is a guard against a future call site rather than against
    /// the current ones.
    pub(crate) fn emit_message<R: Runtime>(&self, app: &AppHandle<R>, payload: &MessageUpdate) {
        if payload.updates.is_empty() {
            return;
        }

        if let Err(error) = payload.clone().emit(app) {
            tracing::warn!(error = %error, "failed to emit message:update");
        }
    }

    /// Emits `sessions:state`.
    ///
    /// Unconditional, deliberately. Nothing here decides whether the interface
    /// deserves this payload — the caller has just read the registry, so the
    /// payload is current, and dropping it would drop the only copy. The
    /// pacing is the forwarder's, because only the forwarder can wait and then
    /// look again.
    pub(crate) fn emit_sessions<R: Runtime>(&self, app: &AppHandle<R>, payload: &SessionsState) {
        if let Err(error) = payload.clone().emit(app) {
            tracing::warn!(error = %error, "failed to emit sessions:state");
        }
    }

    /// Emits `metrics:tick`.
    ///
    /// Unconditional, for the same reason `sessions:state` is: the caller has
    /// just read the session, so the payload is current, and the cadence is
    /// the forwarder's — see [`METRICS_TICK_INTERVAL`]. A throttle here would
    /// be a second rate limit on a channel that already ticks at a fixed rate,
    /// and its only possible effect would be to drop a reading nothing
    /// replays.
    pub(crate) fn emit_metrics<R: Runtime>(&self, app: &AppHandle<R>, payload: &MetricsTick) {
        if let Err(error) = payload.clone().emit(app) {
            tracing::warn!(error = %error, "failed to emit metrics:tick");
        }
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // disallows. The ban targets *production* code, where a nested `block_on`
    // deadlocks the runtime; a test harness is precisely the "binary entry
    // point" the reason string carves out.
    #![allow(clippy::disallowed_methods)]

    use super::*;

    #[tokio::test]
    async fn admits_the_very_first_emission() {
        let throttle = Throttle::new(Duration::from_millis(200));

        assert!(throttle.admit(Instant::now()).await);
    }

    #[tokio::test]
    async fn refuses_a_second_emission_inside_the_interval() {
        let throttle = Throttle::new(Duration::from_millis(200));
        let start = Instant::now();

        assert!(throttle.admit(start).await);
        assert!(!throttle.admit(start + Duration::from_millis(199)).await);
    }

    #[tokio::test]
    async fn admits_again_once_the_interval_has_elapsed() {
        let throttle = Throttle::new(Duration::from_millis(200));
        let start = Instant::now();

        assert!(throttle.admit(start).await);
        assert!(throttle.admit(start + Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn a_burst_does_not_push_the_next_admission_further_away() {
        let throttle = Throttle::new(Duration::from_millis(200));
        let start = Instant::now();

        assert!(throttle.admit(start).await);
        for offset in 1..200 {
            assert!(!throttle.admit(start + Duration::from_millis(offset)).await);
        }

        // The reference instant is still `start`, not the last refusal:
        // 200 ms after the admitted emission, a new one gets through.
        assert!(throttle.admit(start + Duration::from_millis(200)).await);
    }

    #[test]
    fn mirrors_the_error_dto_shape() {
        let dto = ErrorDto {
            code: ErrorCode::ConfigInvalidLanguage,
            message: "unsupported language".to_owned(),
            details: None,
        };

        let payload = ErrorNotify::from(&dto);

        assert_eq!(payload.code, dto.code);
        assert_eq!(payload.message, dto.message);
    }
}
