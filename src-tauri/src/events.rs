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
