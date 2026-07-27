//! Reading a contact file, validating it, and writing what came out.
//!
//! # The shape of an import
//!
//! ```text
//!  spawn_blocking                 bounded mpsc               the runtime
//! ┌───────────────────────┐      ┌───────────┐      ┌──────────────────────┐
//! │ read a row            │─────►│ 1024 max  │─────►│ deduplicate          │
//! │ map its columns       │      └───────────┘      │ batch                │
//! │ validate the number   │       back-pressure     │ write in ONE txn     │
//! └───────────────────────┘                         │ emit progress        │
//!                                                   └──────────────────────┘
//! ```
//!
//! The split is not decoration. Reading a file and parsing a spreadsheet are
//! **blocking, CPU-bound** work, which guide §7.1 and CLAUDE.md §4 forbid on a
//! runtime thread; writing is I/O the runtime must be free to interleave. The
//! channel between them is **bounded**, so a slow disk makes the reader wait
//! rather than making the process grow (CLAUDE.md §4).
//!
//! # Cancellation (CA-009-10)
//!
//! Both halves watch one [`CancellationToken`]. The reader stops offering rows;
//! the writer commits the batch it is holding — a batch is one transaction, so
//! it is whole or it is nothing — and returns a report marked
//! [`ImportReport::cancelled`], whose `imported` is exactly what is in the
//! database. There is no state in which rows were written and nothing says so.
//!
//! # Progress (CA-009-11)
//!
//! Two mechanisms, and each covers what the other cannot. Progress is computed
//! every [`PROGRESS_EVERY_ROWS`] rows, so a million-row import produces a
//! thousand events rather than a million; and it is offered with `try_send`, so
//! an interface that has fallen behind loses intermediate events instead of
//! back-pressuring the import. The final event is the one that must arrive, and
//! it is sent on the blocking path.

pub mod csv;
pub mod mapping;
pub mod report;
pub mod source;
pub mod xlsx;

use std::path::PathBuf;

use smpp_core::time::{Clock, Timestamp};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use csv::{CsvDialect, CsvRows, HeaderMode};
pub use mapping::{
    AttributeColumn, ColumnMapping, ColumnRef, ImportProfile, MappingError, ResolvedMapping,
};
pub use report::{
    Deduplication, Deduplicator, ImportReport, ImportTally, RejectedRow, Verdict, MAX_REJECTED_ROWS,
};
pub use source::{RawRow, RowSource};
pub use xlsx::XlsxRows;

use crate::error::ContactsError;
use crate::model::{Contact, ContactId, ListId};
use crate::ports::ContactRepository;
use crate::validation::{validate, RejectionReason, ValidationOptions};

/// Rows between the reader and the writer.
///
/// **Bounded** (CLAUDE.md §4). Large enough that the writer is never starved
/// between two batches, small enough that a stalled writer cannot let the
/// reader build a queue proportional to the file — which is the failure
/// CA-009-01 is about.
const ROW_QUEUE_CAPACITY: usize = 1_024;

/// Contacts written per transaction.
///
/// One transaction per row would mean one `fsync` per row (spec §11.2); one
/// transaction for the whole file would mean an import that cannot be
/// cancelled without losing everything. A thousand is the middle: a few dozen
/// transactions for a fifty-thousand-row file, and at most a thousand contacts
/// lost by a cancellation that lands mid-batch.
pub const BATCH_SIZE: usize = 1_000;

/// How often a progress event is computed.
pub const PROGRESS_EVERY_ROWS: u64 = 1_000;

/// Where the rows come from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportSource {
    /// A delimited text file, read in streaming.
    Csv {
        /// The file the operator chose.
        path: PathBuf,
    },
    /// One sheet of a workbook.
    Xlsx {
        /// The file the operator chose.
        path: PathBuf,
        /// The sheet, or the first one when absent.
        sheet: Option<String>,
    },
}

impl ImportSource {
    /// The value written to `contacts.source`.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Csv { .. } => "import_csv",
            Self::Xlsx { .. } => "import_xlsx",
        }
    }
}

/// Everything the operator chose in the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOptions {
    /// Which column means what.
    pub mapping: ColumnMapping,
    /// Whether the first row names the columns.
    pub headers: HeaderMode,
    /// Default region and the mobiles-only switch.
    pub validation: ValidationOptions,
    /// What to do with a repeated number.
    pub deduplication: Deduplication,
    /// A list every imported contact joins.
    pub list: Option<ListId>,
}

impl ImportOptions {
    /// The options an import runs with when only the mapping is known.
    #[must_use]
    pub fn new(mapping: ColumnMapping) -> Self {
        Self {
            mapping,
            headers: HeaderMode::Detect,
            validation: ValidationOptions {
                default_region: None,
                mobiles_only: false,
            },
            deduplication: Deduplication::FirstWins,
            list: None,
        }
    }

    /// The same options, resolving national forms against `region`.
    #[must_use]
    pub fn with_validation(mut self, validation: ValidationOptions) -> Self {
        self.validation = validation;
        self
    }

    /// The same options, applying `strategy` to repeated numbers.
    #[must_use]
    pub fn with_deduplication(mut self, strategy: Deduplication) -> Self {
        self.deduplication = strategy;
        self
    }

    /// The same options, enrolling every imported contact in `list`.
    #[must_use]
    pub fn into_list(mut self, list: ListId) -> Self {
        self.list = Some(list);
        self
    }
}

/// How far an import has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportProgress {
    /// Non-blank rows dealt with.
    pub processed: u64,
    /// Contacts accepted so far.
    pub imported: u64,
    /// Rows refused so far.
    pub rejected: u64,
    /// Rows repeating an earlier number.
    pub duplicates: u64,
    /// Whether this is the last event of the import.
    pub done: bool,
}

/// What the reader hands the writer.
#[derive(Debug)]
enum Outcome {
    /// A row that produced a contact.
    Accepted(Box<Contact>),
    /// A row that did not.
    Rejected {
        line: u64,
        reason: RejectionReason,
        value: String,
    },
    /// A row that held nothing.
    Blank,
}

/// Runs imports against a contact store.
#[derive(Debug, Clone)]
pub struct Importer<R, C> {
    repository: R,
    clock: C,
    batch_size: usize,
}

impl<R, C> Importer<R, C>
where
    R: ContactRepository + Clone + 'static,
    C: Clock,
{
    /// Binds an importer to a store and a clock.
    ///
    /// The clock is injected rather than read from the system (CLAUDE.md §7):
    /// every contact of one import shares one `created_at`, drawn once, so a
    /// test asserts on the instant it chose and an import is not spread over a
    /// second boundary.
    pub const fn new(repository: R, clock: C) -> Self {
        Self {
            repository,
            clock,
            batch_size: BATCH_SIZE,
        }
    }

    /// The same importer, committing every `size` contacts.
    ///
    /// For tests that need to observe a partial import without writing a
    /// thousand rows first.
    #[must_use]
    pub const fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }

    /// Imports a file.
    ///
    /// # Errors
    ///
    /// [`ContactsError::Read`] if the file cannot be opened or parsed,
    /// [`ContactsError::Mapping`] if the mapping does not fit it, or
    /// [`ContactsError::Store`] if a batch could not be written.
    ///
    /// A cancellation is **not** an error: it produces a report with
    /// [`ImportReport::cancelled`] set.
    pub async fn run(
        &self,
        source: ImportSource,
        options: ImportOptions,
        progress: Option<mpsc::Sender<ImportProgress>>,
        cancel: CancellationToken,
    ) -> Result<ImportReport, ContactsError> {
        let label = source.label();
        let headers = options.headers;

        // Opening the file is blocking, and opening a workbook parses the
        // whole zip: neither belongs on a runtime thread (guide §7.1).
        let opened = tokio::task::spawn_blocking(move || open(source, headers))
            .await
            .map_err(|_| ContactsError::invalid("the reader task did not finish"))?;

        match opened? {
            Opened::Csv(rows) => self.run_rows(rows, options, label, progress, cancel).await,
            Opened::Xlsx(rows) => self.run_rows(rows, options, label, progress, cancel).await,
        }
    }

    /// Imports from an already-open row source.
    ///
    /// Public so a test can feed rows without a file, which is what makes the
    /// deduplication, cancellation and progress behaviours testable without a
    /// temporary directory.
    ///
    /// # Errors
    ///
    /// Same as [`Self::run`].
    pub async fn run_rows<S>(
        &self,
        mut source: S,
        options: ImportOptions,
        label: &'static str,
        progress: Option<mpsc::Sender<ImportProgress>>,
        cancel: CancellationToken,
    ) -> Result<ImportReport, ContactsError>
    where
        S: RowSource + Send + 'static,
    {
        // ONE instant for the whole import. See `new`.
        let created_at = self.clock.now();
        let (sender, mut receiver) = mpsc::channel(ROW_QUEUE_CAPACITY);
        let reader_cancel = cancel.clone();
        let reader_options = options.clone();

        let reader = tokio::task::spawn_blocking(move || {
            read_all(
                &mut source,
                &reader_options,
                label,
                created_at,
                &sender,
                &reader_cancel,
            )
        });

        let mut tally = ImportTally::default();
        let mut deduplicator = Deduplicator::new(options.deduplication);
        let mut batch: Vec<Contact> = Vec::with_capacity(self.batch_size);
        let mut cancelled = false;
        let mut last_reported = 0_u64;

        loop {
            let received = tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    cancelled = true;
                    None
                }
                message = receiver.recv() => message,
            };

            let Some(outcome) = received else {
                break;
            };

            match outcome {
                Outcome::Blank => tally.blank(),
                Outcome::Rejected {
                    line,
                    reason,
                    value,
                } => tally.reject(line, reason, &value),
                Outcome::Accepted(contact) => {
                    let line_type = contact.line_type;

                    match deduplicator.offer(*contact) {
                        (Verdict::Fresh, kept) => {
                            tally.accept(line_type);

                            if let Some(contact) = kept {
                                batch.push(contact);
                            }
                        }
                        (Verdict::Duplicate | Verdict::Merged, _) => tally.duplicate(),
                    }
                }
            }

            if batch.len() >= self.batch_size {
                self.commit(&mut batch, options.list).await?;
            }

            if tally.processed() >= last_reported.saturating_add(PROGRESS_EVERY_ROWS) {
                last_reported = tally.processed();
                notify(progress.as_ref(), &tally, false);
            }
        }

        // A merging import held everything until now; a first-wins one handed
        // each contact over as it went and has nothing left here.
        batch.extend(deduplicator.into_held());

        while !batch.is_empty() {
            let mut chunk: Vec<Contact> = batch.drain(..self.batch_size.min(batch.len())).collect();

            self.commit(&mut chunk, options.list).await?;
        }

        // The reader task is joined AFTER the queue is drained, never before:
        // waiting on it first would deadlock on a full channel.
        match reader.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(ContactsError::invalid("the reader task did not finish")),
        }

        notify(progress.as_ref(), &tally, true);

        Ok(tally.finish(cancelled || cancel.is_cancelled()))
    }

    /// Writes one batch, and enrols it in the list when there is one.
    ///
    /// Two calls rather than one: the port has no "insert and enrol"
    /// method, and adding one would put a policy of this crate into the
    /// store's contract. The cost is stated — a failure between the two leaves
    /// contacts written and not enrolled, which the next import of the same
    /// file repairs, since a membership that already exists is a no-op.
    async fn commit(
        &self,
        batch: &mut Vec<Contact>,
        list: Option<ListId>,
    ) -> Result<(), ContactsError> {
        if batch.is_empty() {
            return Ok(());
        }

        self.repository.insert_contacts(batch).await?;

        if let Some(list) = list {
            let members: Vec<ContactId> = batch.iter().map(|contact| contact.contact_id).collect();

            self.repository.add_contacts_to_list(list, &members).await?;
        }

        batch.clear();

        Ok(())
    }
}

/// Offers a progress event, dropping it if the interface has fallen behind.
///
/// `try_send`, never `send`: an import must not run at the speed of the
/// WebView. The `done` event is the one that matters, and it is the last thing
/// sent, so an interface that missed intermediate ones still ends up correct.
fn notify(progress: Option<&mpsc::Sender<ImportProgress>>, tally: &ImportTally, done: bool) {
    let Some(sender) = progress else {
        return;
    };

    let event = ImportProgress {
        processed: tally.processed(),
        imported: tally.imported(),
        rejected: tally.rejected(),
        duplicates: tally.duplicates(),
        done,
    };

    if sender.try_send(event).is_err() {
        tracing::trace!(
            processed = event.processed,
            "import progress dropped: the receiver is behind"
        );
    }
}

/// An opened file, in whichever of the two shapes it came.
enum Opened {
    Csv(CsvRows),
    Xlsx(XlsxRows),
}

/// Opens the file. **Blocking**; called from `spawn_blocking`.
fn open(source: ImportSource, headers: HeaderMode) -> Result<Opened, ContactsError> {
    match source {
        ImportSource::Csv { path } => CsvRows::open(&path, headers).map(Opened::Csv),
        ImportSource::Xlsx { path, sheet } => {
            XlsxRows::open(&path, sheet.as_deref(), headers).map(Opened::Xlsx)
        }
    }
}

/// Reads, maps and validates every row. **Blocking**.
///
/// Stops at the first read failure, and at cancellation. Sends on a bounded
/// channel with `blocking_send`, which is the back-pressure: a writer that
/// falls behind slows the reader down instead of letting a queue grow.
fn read_all<S>(
    source: &mut S,
    options: &ImportOptions,
    label: &'static str,
    created_at: Timestamp,
    sender: &mpsc::Sender<Outcome>,
    cancel: &CancellationToken,
) -> Result<(), ContactsError>
where
    S: RowSource,
{
    let mut resolved: Option<ResolvedMapping> = match source.headers() {
        Some(headers) => Some(options.mapping.resolve(Some(headers), headers.len())?),
        None => None,
    };

    while let Some(row) = source.next_row()? {
        if cancel.is_cancelled() {
            return Ok(());
        }

        if row.is_blank() {
            if sender.blocking_send(Outcome::Blank).is_err() {
                return Ok(());
            }

            continue;
        }

        // A headerless file has nothing to resolve names against until a row
        // says how wide it is.
        let mapping = match resolved.as_ref() {
            Some(mapping) => mapping,
            None => {
                resolved = Some(options.mapping.resolve(None, row.values.len())?);
                resolved
                    .as_ref()
                    .ok_or_else(|| ContactsError::invalid("the mapping could not be resolved"))?
            }
        };

        let raw = mapping.msisdn(&row.values);
        let country = mapping.country(&row.values);

        // CA-009-03: a spreadsheet cell nothing could be read out of is a
        // rejection with its own reason, never an empty cell.
        if mapping.holds_unreadable_number(&row) {
            if sender
                .blocking_send(Outcome::Rejected {
                    line: row.line,
                    reason: RejectionReason::UnreadableCell,
                    value: String::new(),
                })
                .is_err()
            {
                return Ok(());
            }

            continue;
        }

        let outcome = match validate(raw, country, options.validation) {
            Ok(number) => Outcome::Accepted(Box::new(Contact {
                contact_id: ContactId::new(),
                msisdn: number.msisdn().clone(),
                country: number
                    .region()
                    .as_ref()
                    .map(|region| region.code().to_owned()),
                valid: true,
                line_type: Some(number.line_type()),
                attributes: mapping.attributes(&row.values),
                source: Some(label.to_owned()),
                created_at,
            })),
            Err(reason) => Outcome::Rejected {
                line: row.line,
                reason,
                value: raw.to_owned(),
            },
        };

        if sender.blocking_send(outcome).is_err() {
            // The writer is gone — cancelled, or failed. Nothing more to do.
            return Ok(());
        }
    }

    Ok(())
}
