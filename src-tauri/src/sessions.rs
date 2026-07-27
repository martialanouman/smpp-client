//! What the session commands are allowed to reach.
//!
//! Three handles and no logic (CLAUDE.md §3): the profile repository, the
//! session registry of spec §8.3, and the emitter that pushes `sessions:state`.
//! Everything that decides anything lives in `smpp-session` or `persistence`.
//!
//! The one piece of behaviour here is the **forwarder**: a task per session
//! that watches the session's `watch` channel and turns each change into an
//! event. It has to be somewhere, and it cannot be in `smpp-session` — that
//! crate must not know Tauri exists.

use std::sync::Arc;

use core::time::Duration;

use persistence::{Database, SqliteSessionProfileRepository};
use smpp_session::profile::{Password, SessionProfile};
use smpp_session::{SessionHandle, SessionRegistry, TcpTransport};
use tauri::{AppHandle, Runtime};
use tokio::sync::watch;

use crate::commands::session::statuses;
use crate::error::ErrorDto;
use crate::events::{EventEmitter, MetricsTick, METRICS_TICK_INTERVAL, SESSIONS_STATE_INTERVAL};

/// The session half of the application state.
pub(crate) struct SessionServices {
    profiles: SqliteSessionProfileRepository,
    registry: Arc<SessionRegistry<TcpTransport>>,
    events: Arc<EventEmitter>,
}

impl core::fmt::Debug for SessionServices {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionServices")
            .finish_non_exhaustive()
    }
}

impl SessionServices {
    /// Binds the services to an open database.
    pub(crate) fn new(database: Database, events: Arc<EventEmitter>) -> Self {
        Self {
            profiles: SqliteSessionProfileRepository::new(database),
            registry: Arc::new(SessionRegistry::new(TcpTransport)),
            events,
        }
    }

    /// The profile repository.
    pub(crate) const fn profiles(&self) -> &SqliteSessionProfileRepository {
        &self.profiles
    }

    /// The session registry.
    pub(crate) const fn registry(&self) -> &Arc<SessionRegistry<TcpTransport>> {
        &self.registry
    }

    /// Opens a session and starts forwarding its state to the interface.
    ///
    /// # Errors
    ///
    /// Whatever the registry refuses — `SESSION_BUSY`, most often.
    pub(crate) async fn bind<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        profile: SessionProfile,
        password: Password,
        logs: &crate::logs::LogServices,
    ) -> Result<SessionHandle, ErrorDto> {
        let session = self
            .registry
            .bind(profile, password)
            .await
            .map_err(|error| ErrorDto::from(&error))?;

        let handle = session.handle.clone();

        self.spawn_forwarder(app, &handle);
        self.spawn_metrics_ticker(app, &handle);
        // Milestone 008: the delivery queue is read rather than drained and
        // dropped. `LogServices` owns the pipeline because it owns the journal
        // the receipts are correlated against.
        logs.spawn_receipt_loop(app, session);

        Ok(handle)
    }

    /// Emits `sessions:state` with the current picture.
    ///
    /// Called right after a command: the interface has just asked for
    /// something and must see the answer.
    pub(crate) async fn publish<R: Runtime>(&self, app: &AppHandle<R>) {
        let payload = statuses(&self.registry).await;

        self.events.emit_sessions(app, &payload);
    }

    /// Closes every live session. Called when the application exits.
    pub(crate) async fn shutdown(&self) {
        self.registry.shutdown_all().await;
    }

    /// Watches one session's state and turns each change into an event.
    ///
    /// The task ends when the session's `watch` sender drops, which happens
    /// when the supervisor returns — so it has an owner and a defined end, and
    /// is not the orphan CLAUDE.md §4 forbids. It is not joined: the session
    /// it follows is already gone by the time it stops, and there is nothing
    /// left to wait for.
    ///
    /// # Pacing, not filtering
    ///
    /// This loop emits, then sleeps [`SESSIONS_STATE_INTERVAL`], then waits
    /// for the next change — and when it wakes it **re-reads the registry**
    /// rather than emitting a payload it prepared earlier. Transitions that
    /// happened during the sleep are therefore coalesced into the state that
    /// followed them, and the last state is always delivered.
    ///
    /// That is the property a throttle at the emitter could not have. It used
    /// to discard emissions arriving too close together, and since a healthy
    /// session stops changing once it is `BOUND`, the discarded one was
    /// frequently the last: `CONNECTING` at t=0 was shown, `BINDING` at t=1 ms
    /// and `BOUND` at t=6 ms were both dropped, and the screen said
    /// `CONNECTING` for as long as the session lived. `watch` keeps only the
    /// latest value, which is exactly the semantics this loop needs.
    fn spawn_forwarder<R: Runtime>(&self, app: &AppHandle<R>, handle: &SessionHandle) {
        let watch = handle.watch();
        let registry = Arc::clone(&self.registry);
        let events = Arc::clone(&self.events);
        let app = app.clone();

        // The Arcs are cloned per call rather than borrowed: a closure that
        // lends to the future it returns is not something stable Rust can
        // express, and an `Arc::clone` per state change is free next to
        // crossing the IPC bridge.
        let publish = move || {
            let registry = Arc::clone(&registry);
            let events = Arc::clone(&events);
            let app = app.clone();

            async move {
                let payload = statuses(&registry).await;

                events.emit_sessions(&app, &payload);
            }
        };

        tauri::async_runtime::spawn(forward(watch, SESSIONS_STATE_INTERVAL, publish));
    }

    /// Samples one session's metrics at a fixed cadence and emits them.
    ///
    /// # A ticker, not a listener
    ///
    /// The task never hears about a submission. It wakes every
    /// [`METRICS_TICK_INTERVAL`], reads the session's sliding averages — which
    /// `smpp_session` maintains at full rate — and emits one event. That is
    /// what makes CA-007-07 a property of the design rather than of a limiter:
    /// there is no path from a message to an emission, so no load can produce
    /// more than four events a second.
    ///
    /// It has an owner and a defined end, like the state forwarder: when the
    /// session's `watch` sender drops the session is gone, and the task
    /// returns rather than ticking against a handle nobody holds.
    fn spawn_metrics_ticker<R: Runtime>(&self, app: &AppHandle<R>, handle: &SessionHandle) {
        let events = Arc::clone(&self.events);
        let registry = Arc::clone(&self.registry);
        let session_id = handle.session_id();
        let rendered = session_id.to_string();
        let app = app.clone();

        // The handle is looked up in the registry on every tick rather than
        // captured once, and that is not a style choice. A `SessionHandle` is
        // `Clone` and the last one dropping is what stops the session — a
        // ticker holding its own copy would keep the session, its supervisor
        // and its socket alive for as long as it kept ticking, which is
        // forever. The lookup returning `None` *is* the end condition.
        let publish = move || {
            let events = Arc::clone(&events);
            let registry = Arc::clone(&registry);
            let rendered = rendered.clone();
            let app = app.clone();

            async move {
                let Some(handle) = registry.handle(session_id).await else {
                    return Ticking::Stop;
                };

                let payload = MetricsTick::of(&rendered, &handle.metrics().await);

                events.emit_metrics(&app, &payload);

                Ticking::Continue
            }
        };

        tauri::async_runtime::spawn(tick(METRICS_TICK_INTERVAL, publish));
    }
}

/// Emits on every change, paced by `interval`, always reading the state fresh.
///
/// Split out of [`SessionServices::spawn_forwarder`] so the property that
/// matters can be tested without a Tauri runtime: **the last state a session
/// reaches is always emitted**, whatever happened in the milliseconds before
/// it. `publish` is called *after* the wake-up and after the pause, never with
/// a payload prepared earlier — that ordering is the whole fix.
async fn forward<T, P, Fut>(mut watch: watch::Receiver<T>, interval: Duration, publish: P)
where
    P: Fn() -> Fut,
    Fut: core::future::Future<Output = ()>,
{
    loop {
        let ended = watch.changed().await.is_err();

        publish().await;

        if ended {
            return;
        }

        // Coalesce whatever happens next. `watch` reports a change immediately
        // after this if one occurred meanwhile, and the read inside `publish`
        // then picks up the newest state rather than the one that woke us.
        tokio::time::sleep(interval).await;
    }
}

/// Whether the ticker has anything left to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ticking {
    /// The session is still there; keep sampling it.
    Continue,
    /// The session is gone; the task ends.
    Stop,
}

/// Samples and emits at a fixed cadence, until `publish` says to stop.
///
/// Split out of [`SessionServices::spawn_metrics_ticker`] so CA-007-07 can be
/// stated without a Tauri runtime: **the emission rate is the interval, and
/// there is no argument, no channel and no code path by which anything else
/// could raise it.**
///
/// The sleep comes first, so a session that ends before the first interval
/// emits nothing at all.
async fn tick<P, Fut>(interval: Duration, publish: P)
where
    P: Fn() -> Fut,
    Fut: core::future::Future<Output = Ticking>,
{
    loop {
        tokio::time::sleep(interval).await;

        if publish().await == Ticking::Stop {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for a binary entry point. A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::*;

    const INTERVAL: Duration = Duration::from_millis(100);

    /// The state cell the recorder reads, standing in for the registry.
    type Cell = Arc<tokio::sync::Mutex<&'static str>>;

    /// What `forward` chose to emit, in order.
    type Log = Arc<tokio::sync::Mutex<Vec<&'static str>>>;

    /// A `publish` that reads the cell **at call time**, the way the real one
    /// reads the registry.
    fn recorder(
        state: &Cell,
        seen: &Log,
    ) -> impl Fn() -> core::pin::Pin<Box<dyn core::future::Future<Output = ()> + Send>> {
        let state = Arc::clone(state);
        let seen = Arc::clone(seen);

        move || {
            let state = Arc::clone(&state);
            let seen = Arc::clone(&seen);

            Box::pin(async move {
                let current = *state.lock().await;

                seen.lock().await.push(current);
            })
        }
    }

    /// **The regression.** A message centre on the same host completes the
    /// handshake in milliseconds. Under the old throttle `CONNECTING` was
    /// admitted and both `BINDING` and `BOUND` were discarded with nothing to
    /// replay them, so the screen showed `CONNECTING` for as long as the
    /// session lived.
    ///
    /// What must hold is not "every transition is delivered" — coalescing is
    /// wanted — but "the **last** one always is".
    #[tokio::test(start_paused = true)]
    async fn the_final_state_is_emitted_however_fast_the_transitions_were() {
        let state: Cell = Arc::new(tokio::sync::Mutex::new("CLOSED"));
        let seen: Log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (sender, receiver) = watch::channel(0_u32);

        let forwarder = tokio::spawn(forward(receiver, INTERVAL, recorder(&state, &seen)));

        for (at, next) in [(0, "CONNECTING"), (1, "BINDING"), (6, "BOUND")] {
            tokio::time::sleep(Duration::from_millis(at)).await;
            *state.lock().await = next;
            sender.send_modify(|version| *version += 1);
        }

        // Let the pacing run its course, then end the session.
        tokio::time::sleep(INTERVAL * 3).await;
        drop(sender);
        forwarder.await.expect("the forwarder ends with its sender");

        let seen = seen.lock().await.clone();

        assert_eq!(
            seen.last(),
            Some(&"BOUND"),
            "the state the session settled in must reach the interface: {seen:?}"
        );
        assert!(
            seen.contains(&"CONNECTING"),
            "the first transition is not delayed by the pacing: {seen:?}"
        );
    }

    /// The pacing still holds: a storm of changes does not produce one
    /// emission each.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_changes_is_coalesced_rather_than_replayed_one_by_one() {
        let state: Cell = Arc::new(tokio::sync::Mutex::new("CLOSED"));
        let seen: Log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (sender, receiver) = watch::channel(0_u32);

        let forwarder = tokio::spawn(forward(receiver, INTERVAL, recorder(&state, &seen)));

        for _ in 0..50_u32 {
            *state.lock().await = "RECONNECT";
            sender.send_modify(|version| *version += 1);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        *state.lock().await = "BOUND";
        sender.send_modify(|version| *version += 1);
        tokio::time::sleep(INTERVAL * 3).await;
        drop(sender);
        forwarder.await.expect("the forwarder ends with its sender");

        let seen = seen.lock().await.clone();

        assert!(
            seen.len() < 10,
            "fifty changes over fifty milliseconds must not be fifty emissions: {seen:?}"
        );
        assert_eq!(seen.last(), Some(&"BOUND"), "and the last one still wins");
    }

    /// **CA-007-07** — `metrics:tick` never exceeds 4 Hz, whatever the load.
    ///
    /// The load is simulated by a "session" recording a thousand submissions a
    /// second next to the ticker. It changes nothing, and that is the
    /// assertion: the ticker has no input but the clock, so throughput cannot
    /// reach it.
    #[tokio::test(start_paused = true)]
    async fn ca_007_07_the_metrics_tick_never_exceeds_four_hertz_under_load() {
        let emitted = Arc::new(tokio::sync::Mutex::new(0_u32));
        let submissions = Arc::new(tokio::sync::Mutex::new(0_u32));

        let publish = {
            let emitted = Arc::clone(&emitted);

            move || {
                let emitted = Arc::clone(&emitted);

                async move {
                    *emitted.lock().await += 1;

                    Ticking::Continue
                }
            }
        };

        let ticker = tokio::spawn(tick(METRICS_TICK_INTERVAL, publish));

        // Ten seconds of traffic at a thousand messages a second.
        let load = tokio::spawn({
            let submissions = Arc::clone(&submissions);

            async move {
                for _ in 0..10_000_u32 {
                    *submissions.lock().await += 1;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        });

        load.await.expect("the load ran");
        ticker.abort();

        let emitted = *emitted.lock().await;

        assert_eq!(*submissions.lock().await, 10_000, "the load really ran");
        assert!(
            emitted <= 41,
            "ten seconds at 4 Hz is at most forty ticks, not {emitted}"
        );
        assert!(
            emitted >= 39,
            "and it must actually tick: {emitted} emissions in ten seconds"
        );
    }

    /// The ticker ends when its session does, rather than sampling a handle
    /// nobody holds for the life of the process.
    #[tokio::test(start_paused = true)]
    async fn the_metrics_ticker_stops_when_its_session_is_gone() {
        let emitted = Arc::new(tokio::sync::Mutex::new(0_u32));
        let alive = Arc::new(tokio::sync::Mutex::new(true));

        let publish = {
            let emitted = Arc::clone(&emitted);
            let alive = Arc::clone(&alive);

            move || {
                let emitted = Arc::clone(&emitted);
                let alive = Arc::clone(&alive);

                async move {
                    if !*alive.lock().await {
                        return Ticking::Stop;
                    }

                    *emitted.lock().await += 1;

                    Ticking::Continue
                }
            }
        };

        let ticker = tokio::spawn(tick(METRICS_TICK_INTERVAL, publish));

        tokio::time::sleep(METRICS_TICK_INTERVAL * 4).await;
        *alive.lock().await = false;

        ticker.await.expect("the ticker ends on its own");

        let after = *emitted.lock().await;
        tokio::time::sleep(METRICS_TICK_INTERVAL * 10).await;

        assert_eq!(
            *emitted.lock().await,
            after,
            "a stopped ticker must not emit again"
        );
    }
}
