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

use persistence::{Database, SqliteSessionProfileRepository};
use smpp_session::profile::{Password, SessionProfile};
use smpp_session::{SessionHandle, SessionRegistry, TcpTransport};
use tauri::{AppHandle, Runtime};

use crate::commands::session::statuses;
use crate::error::ErrorDto;
use crate::events::EventEmitter;

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
    ) -> Result<SessionHandle, ErrorDto> {
        let session = self
            .registry
            .bind(profile, password)
            .await
            .map_err(|error| ErrorDto::from(&error))?;

        let handle = session.handle.clone();

        self.spawn_forwarder(app, &handle);
        self.drain_deliveries(session);

        Ok(handle)
    }

    /// Emits `sessions:state` with the current picture, bypassing the throttle.
    ///
    /// Called right after a command: the interface has just asked for
    /// something and must see the answer even when the channel is busy.
    pub(crate) async fn publish<R: Runtime>(&self, app: &AppHandle<R>) {
        let payload = statuses(&self.registry).await;

        self.events.emit_sessions(app, &payload, true).await;
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
    fn spawn_forwarder<R: Runtime>(&self, app: &AppHandle<R>, handle: &SessionHandle) {
        let mut watch = handle.watch();
        let registry = Arc::clone(&self.registry);
        let events = Arc::clone(&self.events);
        let app = app.clone();

        tauri::async_runtime::spawn(async move {
            while watch.changed().await.is_ok() {
                let payload = statuses(&registry).await;

                // Throttled, unlike `publish`: this fires on every transition,
                // and a flapping session produces one every few hundred
                // milliseconds.
                events.emit_sessions(&app, &payload, false).await;
            }

            // One last emission once the session is gone, forced past the
            // throttle: it is the transition the interface most needs and the
            // one a rate limit would most likely eat.
            let payload = statuses(&registry).await;
            events.emit_sessions(&app, &payload, true).await;
        });
    }

    /// Drains the delivery queue of a session.
    ///
    /// Milestone 008 is what reads delivery receipts; until then the queue
    /// still has to be drained, because a full one makes the reader log a
    /// warning per PDU. Draining and dropping is the honest placeholder, and
    /// it says so in the log.
    fn drain_deliveries(&self, mut session: smpp_session::Session) {
        tauri::async_runtime::spawn(async move {
            while let Some(command) = session.deliveries.recv().await {
                tracing::debug!(
                    pdu = %smpp_core::debug::redacted(&command),
                    "incoming PDU dropped: delivery receipts arrive at milestone 008"
                );
            }
        });
    }
}
