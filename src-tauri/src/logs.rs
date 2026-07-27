//! What the log commands and the receipt pipeline are allowed to reach.
//!
//! Three handles and no rules of its own (CLAUDE.md §3): the business journal
//! of `logging-export`, the PDU recorder, and the receipt pipeline of
//! `messaging`. Everything that decides anything — is this a receipt, which
//! message is it about, when is a batch committed, how much of a body is shown
//! — lives in one of those two crates.
//!
//! The one piece of behaviour here is the **receipt loop**: a task per session
//! that reads the queue `smpp-session` publishes, classifies each PDU by its
//! `esm_class`, and feeds the receipts to the pipeline. It has to be somewhere,
//! and it cannot be in `messaging` — that crate must not know Tauri exists, and
//! it is Tauri events the pipeline's observer emits.

use std::sync::Arc;

use logging_export::journal::Journal;
use logging_export::pdu_log::{PduRecorder, StoredPduLog};
use messaging::correlation::{
    Correlator, IdMatching, IncomingReceipt, ReceiptNote, ReceiptObserver, ReceiptPipeline,
};
use messaging::dlr::{as_deliver_sm, classify, Incoming};
use persistence::{
    Database, SqliteMessageRepository, SqliteOrphanRepository, SqlitePduLogRepository,
};
use smpp_core::time::SystemClock;
use tauri::{AppHandle, Runtime};

use crate::events::{EventEmitter, MessageUpdate, MessageUpdateEntry};

/// The journal this application reads: messages on one side, orphans on the
/// other.
pub(crate) type AppJournal = Journal<SqliteMessageRepository, SqliteOrphanRepository>;

/// The PDU recorder this application writes with.
pub(crate) type AppPduRecorder = PduRecorder<StoredPduLog<SqlitePduLogRepository>, SystemClock>;

/// Receipts held between the reader and the pipeline.
///
/// **Bounded** (CLAUDE.md §4). The queue upstream of it — the session's own
/// delivery queue — is bounded too, so a pipeline that fell behind would make
/// this one fill, then that one, and the reader would drop receipts with a
/// warning rather than growing a buffer nobody is watching. The alternative is
/// an unbounded queue that turns a slow disk into an out-of-memory kill.
const RECEIPT_QUEUE_CAPACITY: usize = 1_024;

/// The log half of the application state.
pub(crate) struct LogServices {
    journal: Arc<AppJournal>,
    recorder: Arc<AppPduRecorder>,
    pdu_log: SqlitePduLogRepository,
    messages: SqliteMessageRepository,
    orphans: SqliteOrphanRepository,
    events: Arc<EventEmitter>,
}

impl core::fmt::Debug for LogServices {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LogServices")
            .finish_non_exhaustive()
    }
}

impl LogServices {
    /// Binds the services to an open database.
    pub(crate) fn new(database: Database, events: Arc<EventEmitter>) -> Self {
        let messages = SqliteMessageRepository::new(database.clone());
        let orphans = SqliteOrphanRepository::new(database.clone());
        let pdu_log = SqlitePduLogRepository::new(database);

        Self {
            journal: Arc::new(Journal::new(messages.clone(), orphans.clone())),
            recorder: Arc::new(PduRecorder::new(
                StoredPduLog::new(pdu_log.clone()),
                SystemClock,
            )),
            pdu_log,
            messages,
            orphans,
            events,
        }
    }

    /// The business journal.
    pub(crate) fn journal(&self) -> &AppJournal {
        &self.journal
    }

    /// The PDU recorder, off until somebody turns it on (CA-008-09).
    pub(crate) fn recorder(&self) -> &AppPduRecorder {
        &self.recorder
    }

    /// The PDU log, for the detail panel that reads it back.
    pub(crate) const fn pdu_log(&self) -> &SqlitePduLogRepository {
        &self.pdu_log
    }

    /// Writes whatever the recorder still holds.
    ///
    /// Called when the application exits: the last PDUs before a shutdown are
    /// the ones somebody turned the recorder on for.
    pub(crate) async fn flush(&self) {
        if let Err(error) = self.recorder.flush().await {
            tracing::warn!(error = %error, "the PDU log could not be flushed on shutdown");
        }
    }

    /// Starts reading one session's delivery queue.
    ///
    /// Replaces the placeholder of milestone 005, which drained the queue and
    /// dropped what it held with a `debug!` line saying milestone 008 would
    /// read it. This is milestone 008.
    ///
    /// # Two tasks, and why not one
    ///
    /// The **reader** takes PDUs off the session's queue, classifies them and
    /// forwards the receipts. The **pipeline** correlates and commits in
    /// batches. They are split because the reader must never block: the queue
    /// it drains is what the session's reader actor pushes into, and a full one
    /// makes that actor drop receipts. A single task doing a database round
    /// trip between two reads is exactly that stall.
    ///
    /// Both end when the session does — the queue closes when the supervisor
    /// returns, the reader returns, its sender drops, the pipeline commits what
    /// it holds and returns. Neither is the orphan CLAUDE.md §4 forbids.
    pub(crate) fn spawn_receipt_loop<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        mut session: smpp_session::Session,
        matching: IdMatching,
    ) {
        let session_id = session.handle.session_id();
        let (receipts, inbox) = tokio::sync::mpsc::channel(RECEIPT_QUEUE_CAPACITY);

        // The policy comes from the SESSION PROFILE, not from a default chosen
        // here. `IdMatching::Bases` is lossy — it can map two distinct
        // identifiers onto each other — so it exists for the one message centre
        // whose operator knows it changes base, and an escape hatch nothing can
        // reach is not an escape hatch.
        let pipeline = ReceiptPipeline::new(
            Correlator::new(self.messages.clone(), SystemClock).with_matching(matching),
            self.orphans.clone(),
        );
        let observer = ReceiptForwarder {
            app: app.clone(),
            events: Arc::clone(&self.events),
        };

        tauri::async_runtime::spawn(async move {
            pipeline.run(inbox, &observer).await;
        });

        tauri::async_runtime::spawn(async move {
            while let Some(command) = session.deliveries.recv().await {
                let Some(pdu) = as_deliver_sm(command.pdu()) else {
                    // The queue carries `deliver_sm` and nothing else, so this
                    // is unreachable rather than a case. Logged instead of
                    // ignored: a PDU arriving here means the session started
                    // publishing something new.
                    tracing::warn!(
                        pdu = %smpp_core::debug::redacted(&command),
                        "the delivery queue carried something that is not a deliver_sm"
                    );
                    continue;
                };

                match classify(pdu) {
                    Incoming::Receipt(receipt) => {
                        if receipts
                            .send(IncomingReceipt {
                                session_id: Some(session_id),
                                receipt,
                            })
                            .await
                            .is_err()
                        {
                            // The pipeline is gone; nothing left to read for.
                            return;
                        }
                    }
                    Incoming::MobileOriginated => {
                        // Received, acknowledged by the session's reader, and
                        // journalled here. step-008 §2 puts business handling
                        // of mobile-originated traffic out of scope.
                        tracing::info!(
                            %session_id,
                            "incoming message received; no business handling until a later milestone"
                        );
                    }
                }
            }
        });
    }
}

/// Turns a committed batch of receipts into one `message:update`.
///
/// One event per **batch**, never per message: CA-008-08 requires the bulk to
/// travel through the paginated `logs_query` and the event to carry only
/// aggregated increments. The trait makes that structural — it is handed a
/// slice, so there is no shape in which a call site could emit per message.
struct ReceiptForwarder<R: Runtime> {
    app: AppHandle<R>,
    events: Arc<EventEmitter>,
}

impl<R: Runtime> ReceiptObserver for ReceiptForwarder<R> {
    fn receipts_applied(&self, notes: &[ReceiptNote]) {
        self.events.emit_message(
            &self.app,
            &MessageUpdate {
                updates: notes
                    .iter()
                    .map(|note| MessageUpdateEntry {
                        client_message_id: note.client_message_id.to_string(),
                        state: note.state.as_str().to_owned(),
                        dlr_stat: note.dlr_stat.clone(),
                    })
                    .collect(),
            },
        );
    }
}
