//! The PDU log, off unless somebody turns it on (deliverable L-008-04).
//!
//! # Why this is not simply "another log"
//!
//! A recorded PDU is the wire form of a `submit_sm` — the subscriber's number
//! and the message body — and of a `bind_transmitter`, which carries the SMSC
//! password. Spec §17.7 and CLAUDE.md §8 therefore confine the hexadecimal dump
//! to an **explicitly enabled debug mode**, and `smpp_core::debug` makes that
//! more than a convention: the full dump cannot be produced without naming
//! `DebugDumpAuthorisation::granted`, which is one greppable call site.
//!
//! This module is that call site. Everything else follows from it:
//!
//! * **off by default** (CA-008-09) — [`PduRecorder::new`] starts disabled, and
//!   there is no constructor that starts enabled;
//! * **checked on every PDU**, not once at startup, so turning it off in the
//!   interface stops the recording of the very next PDU rather than of the next
//!   session;
//! * **batched**, because it records both directions of every PDU on a session
//!   that may run at a thousand messages a second, and one transaction each
//!   would make the debug switch a throughput cliff.
//!
//! # What is recorded
//!
//! CA-008-09 asks the detail panel to show the header, the decoded body, the
//! TLVs and the raw hexadecimal. The header fields go in their own columns
//! (`command_id`, `command_status`, `sequence_number`), the raw octets in
//! `raw_hex`, and the decoded rendering — body and TLVs, as `rusmpp` prints
//! them — in `decoded`.
//!
//! When the recorder is **off**, none of the three is produced: the entry is
//! not written at all, so there is nothing to leak and nothing to purge.
//!
//! # NOT YET CALLED FROM THE SESSION — the gap of milestone 008
//!
//! [`PduRecorder::observe`] has **no production call site**. The recorder, its
//! port, its storage, its batching, its `logs_set_pdu_logging` command and its
//! detail panel are all in place and tested; what is missing is the hook inside
//! `smpp-session`'s reader and writer that would hand it each PDU. Turning the
//! switch on therefore records nothing today.
//!
//! It is stated here rather than left to be discovered because a switch that
//! silently does nothing is worse than an absent one. The remaining work has a
//! shape, and the shape is the reason it was not rushed: the observer must be
//! **synchronous and non-blocking** at the call site, pushing onto a bounded
//! queue that a dedicated task drains into this recorder. Awaiting a database
//! write inside the reader would pace the whole session from the debug switch,
//! which is precisely the failure this crate's batching exists to avoid.

use core::future::Future;

use persistence::{PduDirection, PduLogEntry};
use smpp_core::codec::Command;
use smpp_core::debug::{self, DebugDumpAuthorisation};
use smpp_core::time::Clock;
use smpp_core::types::SessionId;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::LoggingExportError;

/// Where recorded PDUs go.
///
/// A port, declared by the crate that consumes it (CLAUDE.md §3) and
/// implemented by `persistence`.
pub trait PduSink: Send + Sync {
    /// Appends a batch of entries.
    ///
    /// # Errors
    ///
    /// [`LoggingExportError::Unavailable`] if the write fails.
    fn record(
        &self,
        entries: &[PduLogEntry],
    ) -> impl Future<Output = Result<u64, LoggingExportError>> + Send;
}

/// The sink that writes to the `pdu_log` table.
///
/// # Why the adapter is here and not in `persistence`
///
/// [`PduSink`] is declared by the crate that **consumes** it (CLAUDE.md §3),
/// which is this one. `persistence` sits below and cannot implement a trait it
/// must not see; so the implementation is here, over the repository port that
/// crate does expose. Eight lines, and the dependency arrow stays pointing
/// down.
#[derive(Debug, Clone)]
pub struct StoredPduLog<R> {
    repository: R,
}

impl<R> StoredPduLog<R>
where
    R: persistence::ports::PduLogRepository + Send + Sync,
{
    /// A sink over a PDU log repository.
    #[must_use]
    pub const fn new(repository: R) -> Self {
        Self { repository }
    }
}

impl<R> PduSink for StoredPduLog<R>
where
    R: persistence::ports::PduLogRepository + Send + Sync,
{
    async fn record(&self, entries: &[PduLogEntry]) -> Result<u64, LoggingExportError> {
        self.repository
            .insert_entries(entries)
            .await
            .map_err(|error| {
                tracing::error!(error = ?error, "the PDU log refused a write");

                LoggingExportError::Unavailable {
                    reason: error.to_string(),
                }
            })
    }
}

/// Entries held before the recorder flushes them.
///
/// Small: this is a debug facility, and a large buffer would mean losing more
/// of it when the application stops. The recorder flushes whenever it reaches
/// this many, and the caller flushes the remainder.
pub const FLUSH_THRESHOLD: usize = 64;

/// Records PDUs, when it is switched on.
///
/// Cheap to share: the switch is an [`AtomicBool`], read once per PDU with a
/// `Relaxed` load, so a session that never enables it pays one atomic read per
/// PDU and nothing else.
#[derive(Debug)]
pub struct PduRecorder<S, C> {
    sink: S,
    clock: C,
    /// **Off**, until somebody says otherwise (CA-008-09).
    enabled: AtomicBool,
    /// Entries waiting to be written.
    pending: tokio::sync::Mutex<Vec<PduLogEntry>>,
}

impl<S, C> PduRecorder<S, C>
where
    S: PduSink,
    C: Clock,
{
    /// A recorder that is switched **off**.
    ///
    /// There is deliberately no constructor that starts enabled: CA-008-09 is
    /// "disabled by default", and a second constructor is how a call site three
    /// milestones from now ends up recording passwords because it looked like
    /// the convenient one.
    #[must_use]
    pub const fn new(sink: S, clock: C) -> Self {
        Self {
            sink,
            clock,
            enabled: AtomicBool::new(false),
            pending: tokio::sync::Mutex::const_new(Vec::new()),
        }
    }

    /// Turns recording on or off.
    ///
    /// Takes effect on the **next** PDU: the flag is read per PDU, not cached
    /// per session, so an operator who notices the log filling up can stop it
    /// without unbinding.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);

        tracing::info!(enabled, "PDU logging switched");
    }

    /// Whether recording is on right now.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Records one PDU, if recording is on.
    ///
    /// Does nothing at all when it is off — no allocation, no dump, no entry.
    /// That is the point: the cost and the exposure are both zero until an
    /// operator asks for them.
    ///
    /// # Errors
    ///
    /// [`LoggingExportError::Unavailable`] if a flush fails. The PDU itself is
    /// never at fault, and the session must not be affected by a failure here —
    /// the caller is expected to log and carry on.
    pub async fn observe(
        &self,
        session_id: Option<SessionId>,
        direction: PduDirection,
        command: &Command,
    ) -> Result<(), LoggingExportError> {
        if !self.is_enabled() {
            return Ok(());
        }

        let entry = self.entry(session_id, direction, command);
        let batch = {
            let mut pending = self.pending.lock().await;
            pending.push(entry);

            if pending.len() < FLUSH_THRESHOLD {
                return Ok(());
            }

            core::mem::take(&mut *pending)
        };

        self.sink.record(&batch).await.map(|_| ())
    }

    /// Writes whatever is still buffered.
    ///
    /// Called when a session ends and when the application stops: the last few
    /// PDUs before a crash are the ones somebody turned the recorder on for.
    ///
    /// # Errors
    ///
    /// [`LoggingExportError::Unavailable`] if the write fails.
    pub async fn flush(&self) -> Result<(), LoggingExportError> {
        let batch = core::mem::take(&mut *self.pending.lock().await);

        if batch.is_empty() {
            return Ok(());
        }

        self.sink.record(&batch).await.map(|_| ())
    }

    /// Builds the entry for one PDU.
    ///
    /// THE ONE PLACE a full PDU dump is produced in this application. It is
    /// reached only past the `is_enabled` check above, which is what
    /// `DebugDumpAuthorisation` exists to make visible.
    fn entry(
        &self,
        session_id: Option<SessionId>,
        direction: PduDirection,
        command: &Command,
    ) -> PduLogEntry {
        let encoded = smpp_core::codec::encode(command).ok();

        PduLogEntry {
            session_id,
            direction,
            command_id: Some(u32::from(command.id())),
            command_status: Some(u32::from(command.status())),
            sequence_number: Some(command.sequence_number()),
            raw_hex: encoded
                .as_deref()
                .map(|bytes| debug::full_dump(bytes, DebugDumpAuthorisation::granted())),
            // The decoded rendering: body and TLVs, as CA-008-09 asks. It is
            // the `Debug` of the PDU, which is precisely what `smpp_core::debug`
            // warns against printing casually — and precisely what an operator
            // needs once they have deliberately turned this on.
            decoded: command.pdu().map(|pdu| format!("{pdu:?}")),
            ts: self.clock.now(),
        }
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]
    // `std::sync::Mutex` is banned because it must never be held across an
    // `.await`. The guard below is taken and released inside a `Vec` push.
    #![allow(clippy::disallowed_types)]

    use std::sync::{Arc, Mutex};

    use smpp_core::codec::Pdu;
    use smpp_core::time::Timestamp;
    use smpp_core::values::CommandStatus;

    use super::{PduDirection, PduLogEntry, PduRecorder, PduSink, FLUSH_THRESHOLD};
    use crate::error::LoggingExportError;

    #[derive(Clone, Default)]
    struct Recording(Arc<Mutex<Vec<PduLogEntry>>>);

    impl Recording {
        fn entries(&self) -> Vec<PduLogEntry> {
            self.0.lock().expect("uncontended").clone()
        }

        /// How many entries were written, which is what "off" must keep at zero.
        fn len(&self) -> usize {
            self.entries().len()
        }
    }

    impl PduSink for Recording {
        async fn record(&self, entries: &[PduLogEntry]) -> Result<u64, LoggingExportError> {
            self.0
                .lock()
                .expect("uncontended")
                .extend_from_slice(entries);

            Ok(entries.len() as u64)
        }
    }

    struct FrozenClock;

    impl smpp_core::time::Clock for FrozenClock {
        fn now(&self) -> Timestamp {
            Timestamp::parse("2026-07-26T12:00:00Z").expect("valid instant")
        }
    }

    fn a_command(sequence: u32) -> Command {
        Command::new(CommandStatus::EsmeRok, sequence, Pdu::EnquireLink)
    }

    use smpp_core::codec::Command;

    /// **CA-008-09** — nothing is recorded until somebody asks.
    #[tokio::test]
    async fn a_fresh_recorder_is_off_and_writes_nothing() {
        let sink = Recording::default();
        let recorder = PduRecorder::new(sink.clone(), FrozenClock);

        assert!(!recorder.is_enabled());

        for sequence in 0..1_000 {
            recorder
                .observe(None, PduDirection::Outbound, &a_command(sequence))
                .await
                .expect("recording off cannot fail");
        }
        recorder.flush().await.expect("nothing to flush");

        assert_eq!(sink.len(), 0, "a disabled recorder must write nothing");
    }

    /// Once on, the entry carries what the detail panel shows: the three header
    /// fields, the raw hexadecimal and the decoded body.
    #[tokio::test]
    async fn an_enabled_recorder_captures_the_header_the_dump_and_the_decoding() {
        let sink = Recording::default();
        let recorder = PduRecorder::new(sink.clone(), FrozenClock);

        recorder.set_enabled(true);
        recorder
            .observe(None, PduDirection::Inbound, &a_command(42))
            .await
            .expect("recorded");
        recorder.flush().await.expect("flushed");

        let entries = sink.entries();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].sequence_number, Some(42));
        assert_eq!(entries[0].direction, PduDirection::Inbound);
        assert_eq!(entries[0].command_status, Some(0));
        // The dump is spaced octet by octet, so the sequence number appears as
        // four separate bytes. Asserting on the exact octets rather than on
        // "the dump is non-empty" is the difference between checking that the
        // right PDU was captured and checking that something was.
        assert!(
            entries[0]
                .raw_hex
                .as_deref()
                .is_some_and(|dump| dump.contains("00 00 00 2A")),
            "the raw dump must hold the sequence number: {:?}",
            entries[0].raw_hex
        );
        assert!(
            entries[0]
                .decoded
                .as_deref()
                .is_some_and(|decoded| decoded.contains("EnquireLink")),
            "the decoded rendering must name the PDU: {:?}",
            entries[0].decoded
        );
    }

    /// Turning it off stops the **next** PDU, not the next session.
    #[tokio::test]
    async fn switching_the_recorder_off_takes_effect_immediately() {
        let sink = Recording::default();
        let recorder = PduRecorder::new(sink.clone(), FrozenClock);

        recorder.set_enabled(true);
        recorder
            .observe(None, PduDirection::Outbound, &a_command(1))
            .await
            .expect("recorded");

        recorder.set_enabled(false);
        recorder
            .observe(None, PduDirection::Outbound, &a_command(2))
            .await
            .expect("not recorded");

        recorder.flush().await.expect("flushed");

        let sequences: Vec<Option<u32>> = sink
            .entries()
            .iter()
            .map(|entry| entry.sequence_number)
            .collect();

        assert_eq!(sequences, vec![Some(1)]);
    }

    /// A session at a thousand PDUs a second must not mean a thousand writes.
    #[tokio::test]
    async fn entries_are_written_in_batches_rather_than_one_at_a_time() {
        #[derive(Clone, Default)]
        struct Counting(Arc<Mutex<Vec<usize>>>);

        impl PduSink for Counting {
            async fn record(&self, entries: &[PduLogEntry]) -> Result<u64, LoggingExportError> {
                self.0.lock().expect("uncontended").push(entries.len());

                Ok(entries.len() as u64)
            }
        }

        let sink = Counting::default();
        let recorder = PduRecorder::new(sink.clone(), FrozenClock);

        recorder.set_enabled(true);

        let total = u32::try_from(FLUSH_THRESHOLD).expect("the threshold fits") * 4;

        for sequence in 0..total {
            recorder
                .observe(None, PduDirection::Outbound, &a_command(sequence))
                .await
                .expect("recorded");
        }

        let writes = sink.0.lock().expect("uncontended").clone();

        assert_eq!(writes, vec![FLUSH_THRESHOLD; 4]);
    }

    /// What is buffered when a session ends must reach the store: the last PDUs
    /// before a failure are the ones the recorder was turned on for.
    #[tokio::test]
    async fn a_flush_writes_what_never_reached_the_threshold() {
        let sink = Recording::default();
        let recorder = PduRecorder::new(sink.clone(), FrozenClock);

        recorder.set_enabled(true);
        recorder
            .observe(None, PduDirection::Outbound, &a_command(7))
            .await
            .expect("recorded");

        assert_eq!(sink.len(), 0, "one PDU is below the threshold");

        recorder.flush().await.expect("flushed");

        assert_eq!(sink.len(), 1);
    }
}
