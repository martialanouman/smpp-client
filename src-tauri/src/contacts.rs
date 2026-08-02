//! What the contact commands are allowed to reach.
//!
//! Two handles and one rule of its own (CLAUDE.md §3): the SQLx repository, the
//! importer of the `contacts` crate, and the token of the import currently
//! running. Everything that decides anything — which separator, which encoding,
//! whether a number is valid, what to do with a duplicate, when a batch is
//! committed — lives in `contacts`.
//!
//! # Why "the import currently running" is state and not a parameter
//!
//! CA-009-10 asks for a cancellable import, and the cancellation arrives as a
//! **second IPC call** while the first is still awaiting. The token has to
//! outlive the command that created it and be reachable from another one, which
//! is what this holds — and holding exactly one is also what makes a second
//! concurrent import a clean refusal rather than two writers racing on the same
//! list.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use contacts::import::{ImportOptions, ImportProgress, ImportReport, ImportSource, Importer};
use contacts::ports::ContactRepository as _;
use persistence::{Database, SqliteContactRepository};
use smpp_core::time::SystemClock;
use tauri::{AppHandle, Runtime};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::error::ErrorDto;
use crate::events::{EventEmitter, ImportProgressEvent};

/// Progress events held between the importer and the emitter.
///
/// **Bounded** (CLAUDE.md §4), and small on purpose: the importer offers events
/// with `try_send` and drops what does not fit, which is the right trade for a
/// progress bar. A large buffer here would only delay the events without
/// preventing the drop.
const PROGRESS_QUEUE_CAPACITY: usize = 16;

/// The importer this application runs.
pub(crate) type AppImporter = Importer<SqliteContactRepository, SystemClock>;

/// The contact half of the application state.
pub(crate) struct ContactServices {
    repository: SqliteContactRepository,
    importer: AppImporter,
    /// The token of the import in flight, when there is one.
    ///
    /// `tokio::sync::Mutex`, not the std one: the guard is taken inside an
    /// async command (CLAUDE.md §4).
    ///
    /// Behind an `Arc` so the slot can be compared by identity. Releasing it
    /// unconditionally would let an import that has just finished clear the
    /// slot of the one that started after it — see [`ContactServices::import`].
    running: Mutex<Option<Arc<CancellationToken>>>,
    /// The files the native picker has handed back.
    ///
    /// The gate that makes "the WebView cannot name a file of its own
    /// choosing" true rather than merely intended. See
    /// [`ContactServices::remember_picked`].
    picked: Mutex<HashSet<PathBuf>>,
    events: Arc<EventEmitter>,
}

impl core::fmt::Debug for ContactServices {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ContactServices")
            .finish_non_exhaustive()
    }
}

impl ContactServices {
    /// Binds the services to an open database.
    pub(crate) fn new(database: Database, events: Arc<EventEmitter>) -> Self {
        let repository = SqliteContactRepository::new(database);

        Self {
            importer: Importer::new(repository.clone(), SystemClock),
            repository,
            running: Mutex::new(None),
            picked: Mutex::new(HashSet::new()),
            events,
        }
    }

    /// The repository, for the queries the contacts screen makes.
    pub(crate) const fn repository(&self) -> &SqliteContactRepository {
        &self.repository
    }

    /// Records a file the operator chose in the native picker.
    ///
    /// # Why an allow-list and not a check on the path
    ///
    /// CLAUDE.md §8 restricts the filesystem to "files chosen through native
    /// dialogs". Without something remembering which those were, that
    /// restriction is a convention of the interface rather than a property of
    /// the application: `contacts_import` takes a path, and a WebView running
    /// injected script could hand it `~/.ssh/id_rsa` and read the file back out
    /// of the rejected rows, which carry the offending value verbatim.
    ///
    /// The set only grows, and deliberately: an operator who imports the same
    /// file twice, or corrects and re-imports it, must not have to pick it
    /// again. It holds paths for the life of the process, which is bounded by
    /// how many files one person picks in one session.
    pub(crate) async fn remember_picked(&self, path: PathBuf) {
        self.picked.lock().await.insert(path);
    }

    /// Whether the picker handed this file back.
    async fn was_picked(&self, path: &Path) -> bool {
        self.picked.lock().await.contains(path)
    }

    /// Runs one import, forwarding its progress to `import:progress`.
    ///
    /// Refuses a second concurrent import: two importers writing into the same
    /// list would interleave their batches, and the report of each would
    /// describe half of what happened.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] if an import is already running, or if the file cannot be
    /// read, mapped or written.
    pub(crate) async fn import<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        source: ImportSource,
        options: ImportOptions,
    ) -> Result<ImportReport, ErrorDto> {
        // Before anything else: the file has to be one the operator chose.
        // Checked here rather than in the command so that no caller can reach
        // the importer past it.
        if !self.was_picked(source.path()).await {
            return Err(ErrorDto::contacts_file_not_picked());
        }

        let token = Arc::new(CancellationToken::new());

        {
            let mut running = self.running.lock().await;

            // `is_some`, and NOT "is some and not cancelled". A cancelled
            // import is still running: it commits the batch it holds and
            // drains its reader before it returns. Letting a second one start
            // in that window puts two importers on the same list, interleaving
            // their batches so that neither report describes what happened —
            // exactly what this guard exists to prevent.
            if running.is_some() {
                return Err(ErrorDto::contacts_import_busy());
            }

            *running = Some(Arc::clone(&token));
        }

        let (sender, receiver) = mpsc::channel(PROGRESS_QUEUE_CAPACITY);
        let forwarder =
            tauri::async_runtime::spawn(forward(app.clone(), Arc::clone(&self.events), receiver));

        let outcome = self
            .importer
            .run(source, options, Some(sender), (*token).clone())
            .await;

        // The sender is dropped by `run`, so the forwarder ends on its own.
        // Awaiting it is what guarantees the LAST event — the one carrying
        // `done` — has reached the bridge before the command returns, so the
        // interface cannot show a finished report beside a stale progress bar.
        if let Err(error) = forwarder.await {
            tracing::warn!(error = %error, "the import progress forwarder ended abnormally");
        }

        // Only if the slot still holds OUR token. It always will under the
        // guard above, and the check is what keeps that true if the guard ever
        // loosens: an unconditional `take` would clear another import's token,
        // leaving it impossible to cancel for the rest of its run.
        {
            let mut running = self.running.lock().await;

            if running
                .as_ref()
                .is_some_and(|held| Arc::ptr_eq(held, &token))
            {
                *running = None;
            }
        }

        outcome.map_err(|error| ErrorDto::from(&error))
    }

    /// Cancels the import in flight, reporting whether there was one.
    pub(crate) async fn cancel(&self) -> bool {
        match self.running.lock().await.as_ref() {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Saves a column-mapping profile (CA-009-09).
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] if the store refuses the write.
    pub(crate) async fn save_profile(
        &self,
        profile: &contacts::import::ImportProfile,
    ) -> Result<(), ErrorDto> {
        self.repository
            .upsert_import_profile(profile)
            .await
            .map_err(|error| ErrorDto::from(&error))
    }

    /// Every saved mapping profile.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] if the store cannot be read.
    pub(crate) async fn profiles(&self) -> Result<Vec<contacts::import::ImportProfile>, ErrorDto> {
        self.repository
            .list_import_profiles()
            .await
            .map_err(|error| ErrorDto::from(&error))
    }
}

/// Drains the progress channel onto the event bridge.
async fn forward<R: Runtime>(
    app: AppHandle<R>,
    events: Arc<EventEmitter>,
    mut receiver: mpsc::Receiver<ImportProgress>,
) {
    while let Some(progress) = receiver.recv().await {
        events.emit_import_progress(&app, &ImportProgressEvent::from(progress));
    }
}
