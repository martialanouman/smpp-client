//! What the send commands are allowed to reach.
//!
//! Two handles and no logic (CLAUDE.md §3): the send orchestrator of
//! `messaging`, bound to the SQLite journal, and the emitter that pushes
//! `message:update`. Everything that decides anything — the validation, the
//! segmentation, the write-ahead order, the aggregation of a multi-segment
//! message — lives in `messaging`.
//!
//! The one piece of behaviour here is the **observer**: `messaging` announces
//! each transition as it applies it, and something has to turn each
//! announcement into a Tauri event. It cannot be `messaging` — that crate must
//! not know Tauri exists — so it is here, and it is eleven lines.

use std::sync::Arc;

use messaging::message::MessageState;
use messaging::sender::{SendObserver, SendReport, SendRequest, Sender};
use messaging::MessagingError;
use persistence::{Database, SqliteMessageRepository};
use smpp_core::time::SystemClock;
use smpp_core::types::ClientMessageId;
use smpp_session::SessionHandle;
use tauri::{AppHandle, Runtime};

use crate::events::{EventEmitter, MessageUpdate};

/// The message half of the application state.
pub(crate) struct MessageServices {
    sender: Sender<SqliteMessageRepository, SystemClock>,
    events: Arc<EventEmitter>,
}

impl core::fmt::Debug for MessageServices {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("MessageServices")
            .finish_non_exhaustive()
    }
}

impl MessageServices {
    /// Binds the services to an open database.
    ///
    /// The clock is the real one; `messaging`'s own tests inject a frozen one,
    /// which is what CLAUDE.md §7 asks for and what this constructor cannot
    /// provide.
    pub(crate) fn new(database: Database, events: Arc<EventEmitter>) -> Self {
        Self {
            sender: Sender::new(SqliteMessageRepository::new(database), SystemClock),
            events,
        }
    }

    /// Sends one message, pushing each transition to the interface.
    ///
    /// # Errors
    ///
    /// Whatever the orchestrator refuses. A message the message centre
    /// **rejected** is not one of them: it comes back as a report.
    pub(crate) async fn send<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        session: &SessionHandle,
        request: &SendRequest,
    ) -> Result<SendReport, MessagingError> {
        let forwarder = EventForwarder {
            app: app.clone(),
            events: Arc::clone(&self.events),
        };

        self.sender
            .send_observed(session, request, &forwarder)
            .await
    }
}

/// Turns each transition into a `message:update`.
///
/// Emitting is non-blocking and does no I/O of its own, which is the contract
/// [`SendObserver`] states: this runs between two `.await`s of the send path,
/// and anything slow here would pace the sending from the interface.
struct EventForwarder<R: Runtime> {
    app: AppHandle<R>,
    events: Arc<EventEmitter>,
}

impl<R: Runtime> SendObserver for EventForwarder<R> {
    fn state_changed(&self, client_message_id: ClientMessageId, state: MessageState) {
        self.events.emit_message(
            &self.app,
            &MessageUpdate {
                client_message_id: client_message_id.to_string(),
                state: state.as_str().to_owned(),
            },
        );
    }
}
