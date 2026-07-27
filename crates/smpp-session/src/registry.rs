//! The session manager of spec §8.3.
//!
//! A registry of live sessions, and the only thing that starts or stops one.
//! The IPC layer holds it and does nothing else with sessions — which is what
//! keeps `src-tauri` a boundary rather than a business layer (CLAUDE.md §3).
//!
//! # One session at a time, for now
//!
//! Spec §8.3 describes a manager holding N sessions, and the registry is
//! already shaped that way — a map keyed by [`SessionId`]. Milestone 005
//! nevertheless refuses a second **live** session, and the refusal is
//! deliberate rather than a limitation of the data structure: step-005 §2 puts
//! multiple sessions and multi-bind in milestone 011, and quietly allowing two
//! would mean shipping the routing, the aggregated window and the shared rate
//! limiter untested. A typed refusal now is one line to delete then; a silent
//! half-implementation is not.

use std::collections::HashMap;

use smpp_core::types::SessionId;
use tokio::sync::Mutex;

use crate::actors::{spawn_observed, Session, SessionHandle, SessionSnapshot};
use crate::error::SessionError;
use crate::profile::{Password, SessionProfile};
use crate::transport::Transport;

/// Live sessions, and the transport they are opened over.
pub struct SessionRegistry<T: Transport> {
    transport: T,
    /// `tokio::sync::Mutex`: `bind` awaits inside the critical section — it
    /// shuts a previous session down — and a `std` guard held across an
    /// `.await` is the deadlock CLAUDE.md §4 bans.
    live: Mutex<HashMap<SessionId, SessionHandle>>,
}

impl<T: Transport> core::fmt::Debug for SessionRegistry<T> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SessionRegistry")
            .finish_non_exhaustive()
    }
}

impl<T: Transport + Clone> SessionRegistry<T> {
    /// An empty registry over `transport`.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Opens a session for `profile`.
    ///
    /// Returns as soon as the tasks are running; the caller watches the
    /// session's state to learn when the bind completes. Binding a profile
    /// that is already live replaces it: the previous session is unbound
    /// cleanly first, which is what "rebind" means in the interface.
    ///
    /// # Errors
    ///
    /// [`SessionError::TooManySessions`] when another profile is already
    /// live — see the note on this module.
    pub async fn bind(
        &self,
        profile: SessionProfile,
        password: Password,
    ) -> Result<Session, SessionError> {
        self.bind_observed(profile, password, None).await
    }

    /// The same, with somebody watching every PDU that crosses the socket.
    ///
    /// The observer must be **synchronous and non-blocking** — see
    /// [`crate::PduObserver`]. It is passed at bind time rather than set on the
    /// registry because a session that starts unwatched can never be watched
    /// afterwards without a second channel into a running actor, and the
    /// application knows at bind time whether a recorder exists.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::bind`] refuses.
    pub async fn bind_observed(
        &self,
        profile: SessionProfile,
        password: Password,
        observer: Option<std::sync::Arc<dyn crate::PduObserver>>,
    ) -> Result<Session, SessionError> {
        let session_id = profile.session_id();
        let mut live = self.live.lock().await;

        if let Some(previous) = live.remove(&session_id) {
            // Rebinding the same profile: close the old session first, so the
            // message centre does not see two binds for one `system_id`.
            previous.shutdown().await?;
        }

        if !live.is_empty() {
            return Err(SessionError::TooManySessions { live: live.len() });
        }

        let session = spawn_observed(profile, password, self.transport.clone(), observer);

        live.insert(session_id, session.handle.clone());

        Ok(session)
    }

    /// Closes a session and forgets it.
    ///
    /// Returns whether it was live. Closing an unknown session is not an
    /// error: the interface may ask twice, and the second answer is "already
    /// closed", not a failure.
    ///
    /// # Errors
    ///
    /// [`SessionError::Cancelled`] if the session's tasks ended abnormally.
    pub async fn unbind(&self, session_id: SessionId) -> Result<bool, SessionError> {
        let handle = self.live.lock().await.remove(&session_id);

        match handle {
            Some(handle) => {
                handle.shutdown().await?;

                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The state of one session, when it is live.
    pub async fn status(&self, session_id: SessionId) -> Option<SessionSnapshot> {
        self.live
            .lock()
            .await
            .get(&session_id)
            .map(SessionHandle::snapshot)
    }

    /// The handle of one session, when it is live.
    pub async fn handle(&self, session_id: SessionId) -> Option<SessionHandle> {
        self.live.lock().await.get(&session_id).cloned()
    }

    /// Every live session's state.
    pub async fn statuses(&self) -> Vec<(SessionId, SessionSnapshot)> {
        self.live
            .lock()
            .await
            .iter()
            .map(|(id, handle)| (*id, handle.snapshot()))
            .collect()
    }

    /// Closes every live session, waiting for each to finish.
    ///
    /// What the application calls on exit (CA-005-08): no task outlives it.
    pub async fn shutdown_all(&self) {
        let handles: Vec<SessionHandle> = self.live.lock().await.drain().map(|(_, h)| h).collect();

        for handle in handles {
            if let Err(error) = handle.shutdown().await {
                tracing::warn!(error = %error, "a session did not shut down cleanly");
            }
        }
    }
}

#[cfg(test)]
// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    /// A transport that never connects. Enough to exercise the registry, which
    /// is about bookkeeping and not about the wire.
    #[derive(Clone, Copy)]
    struct Unreachable;

    impl Transport for Unreachable {
        type Stream = tokio::io::DuplexStream;

        async fn connect(&self, _address: &str) -> std::io::Result<Self::Stream> {
            Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
        }
    }

    fn a_profile(name: &str) -> SessionProfile {
        SessionProfile::builder(SessionId::new(), name, "smsc.test", 2775)
            .system_id("esme01")
            .build()
            .expect("valid profile")
    }

    #[tokio::test(start_paused = true)]
    async fn a_bound_profile_is_listed_and_can_be_unbound() {
        let registry = SessionRegistry::new(Unreachable);
        let profile = a_profile("first");
        let session_id = profile.session_id();

        let session = registry
            .bind(profile, Password::empty())
            .await
            .expect("room");

        assert!(registry.status(session_id).await.is_some());
        assert_eq!(registry.statuses().await.len(), 1);
        assert!(registry.unbind(session_id).await.expect("clean"));
        assert!(registry.status(session_id).await.is_none());

        drop(session);
    }

    #[tokio::test(start_paused = true)]
    async fn unbinding_a_session_that_is_not_live_is_not_an_error() {
        let registry = SessionRegistry::new(Unreachable);

        assert!(!registry
            .unbind(SessionId::new())
            .await
            .expect("not an error"));
    }

    /// Milestone 011 lifts this; until then a second session is refused with a
    /// type rather than half-supported.
    #[tokio::test(start_paused = true)]
    async fn a_second_live_session_is_refused_rather_than_half_supported() {
        let registry = SessionRegistry::new(Unreachable);

        let first = registry
            .bind(a_profile("first"), Password::empty())
            .await
            .expect("the first is welcome");

        let rejection = registry
            .bind(a_profile("second"), Password::empty())
            .await
            .expect_err("the second is not");

        assert!(matches!(
            rejection,
            SessionError::TooManySessions { live: 1 }
        ));

        drop(first);
    }

    /// Rebinding the same profile closes the previous session first: the
    /// message centre must not see two binds for one `system_id`.
    #[tokio::test(start_paused = true)]
    async fn rebinding_the_same_profile_replaces_the_previous_session() {
        let registry = SessionRegistry::new(Unreachable);
        let profile = a_profile("same");
        let session_id = profile.session_id();

        let first = registry
            .bind(profile.clone(), Password::empty())
            .await
            .expect("room");
        let second = registry
            .bind(profile, Password::empty())
            .await
            .expect("a rebind is not a second session");

        assert_eq!(registry.statuses().await.len(), 1);
        assert_eq!(second.handle.session_id(), session_id);

        drop(first);
    }

    #[tokio::test(start_paused = true)]
    async fn shutting_everything_down_empties_the_registry() {
        let registry = SessionRegistry::new(Unreachable);
        let session = registry
            .bind(a_profile("only"), Password::empty())
            .await
            .expect("room");

        registry.shutdown_all().await;

        assert!(registry.statuses().await.is_empty());

        drop(session);
    }
}
