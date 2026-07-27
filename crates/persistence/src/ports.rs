//! The repository traits, one per aggregate.
//!
//! # Where these traits live, and why here for now
//!
//! Guide §8.1 puts a port in the crate that **consumes** it. ADR 0007 decided
//! at milestone 002 to park them here instead, with an argument that only held
//! while the consuming crates were empty shells: declaring a port in a crate
//! that uses it for nothing pays the inversion's whole cost — the upward
//! `persistence` → consumer edge — and buys none of its benefit.
//!
//! That argument has now expired for one of them. Milestone 006 gave
//! `messaging` its send orchestrator, so `MessageRepository` moved there
//! (ADR 0010) and this crate implements it. What is left below are the ports
//! whose consumers are still to come:
//!
//! | Port | Consumer, and when it arrives |
//! |------|-------------------------------|
//! | [`SessionProfileRepository`] | `smpp-session`, no milestone yet |
//! | [`ContactRepository`] | `contacts`, milestone 009 (CA-009-13) |
//! | [`CampaignRepository`] | `messaging`, milestone 010 |
//! | [`MessageJournal`] | `logging-export`, milestone 013 |
//! | [`PduLogRepository`] | `logging-export`, milestone 013 |
//!
//! Each move is a declaration change: the trait's shape, and every
//! implementation and double written against it, are unaffected.
//!
//! # Shape of the methods
//!
//! Each method returns `impl Future<Output = Result<…>> + Send` rather than
//! being an `async fn`. Same thing to write against, one difference that
//! matters: the `Send` bound is part of the trait, so an implementation that
//! is not `Send` fails to compile here instead of failing at the
//! `tokio::spawn` three layers up.
//!
//! Large result sets never come back as a `Vec`: [`crate::Page`] for a screen,
//! `BoxStream` for a traversal (guide §11.3).

use std::future::Future;

use futures_core::stream::BoxStream;
use smpp_core::types::SessionId;

use crate::records::{
    Campaign, CampaignId, Contact, ContactId, ContactList, ListId, Message, MessageFilter,
    PduLogEntry, SessionProfile,
};
use crate::{Cursor, Page, PersistenceError};

/// Reads and writes connection profiles (spec §14.2, `session_profiles`).
pub trait SessionProfileRepository {
    /// Inserts the profile, or replaces it if its identifier already exists.
    ///
    /// Upsert rather than insert-then-update because the caller is an
    /// interface form: it always holds the whole profile, and the distinction
    /// between "new" and "edited" is one the interface has already made.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the write fails.
    fn upsert_session_profile(
        &self,
        profile: &SessionProfile,
    ) -> impl Future<Output = Result<(), PersistenceError>> + Send;

    /// Reads one profile.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails, or
    /// [`PersistenceError::MalformedRow`] if a stored value no longer fits its
    /// type.
    fn find_session_profile(
        &self,
        session_id: SessionId,
    ) -> impl Future<Output = Result<Option<SessionProfile>, PersistenceError>> + Send;

    /// Reads every profile, oldest first.
    ///
    /// The only unpaginated list in this module, and deliberately: a profile
    /// is created by hand in a form (spec §16.1), so the table holds units,
    /// not thousands. Paginating it would be ceremony.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn list_session_profiles(
        &self,
    ) -> impl Future<Output = Result<Vec<SessionProfile>, PersistenceError>> + Send;

    /// Deletes one profile, reporting whether it existed.
    ///
    /// Messages sent from it keep their `session_id` as `NULL` rather than
    /// disappearing: the schema says `ON DELETE SET NULL` because the message
    /// journal is an audit trail (spec §17.6).
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the delete fails.
    fn delete_session_profile(
        &self,
        session_id: SessionId,
    ) -> impl Future<Output = Result<bool, PersistenceError>> + Send;
}

/// Reads and writes contacts and contact lists (spec §14.2, §11).
pub trait ContactRepository {
    /// Inserts one contact.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Conflict`] if the identifier already exists,
    /// [`PersistenceError::Database`] otherwise.
    fn insert_contact(
        &self,
        contact: &Contact,
    ) -> impl Future<Output = Result<(), PersistenceError>> + Send;

    /// Inserts a batch of contacts in **one** transaction.
    ///
    /// An import is tens of thousands of rows (spec §11.2). One transaction
    /// per row would mean one `fsync` per row; one transaction for the batch
    /// is also all-or-nothing, so a failed import leaves no half-imported
    /// list behind.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Conflict`] if any identifier already exists — and
    /// then **no** contact of the batch is written.
    fn insert_contacts(
        &self,
        contacts: &[Contact],
    ) -> impl Future<Output = Result<u64, PersistenceError>> + Send;

    /// Reads one contact.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails, or
    /// [`PersistenceError::MalformedRow`] if a stored value no longer fits its
    /// type.
    fn find_contact(
        &self,
        contact_id: ContactId,
    ) -> impl Future<Output = Result<Option<Contact>, PersistenceError>> + Send;

    /// Reads one page of contacts, in insertion order.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn page_contacts(
        &self,
        cursor: Cursor,
        limit: u32,
    ) -> impl Future<Output = Result<Page<Contact>, PersistenceError>> + Send;

    /// Traverses the contacts of a list, or the whole table when `list` is
    /// `None`.
    ///
    /// Rows arrive one at a time (spec §10.4): a campaign of several million
    /// recipients feeds a bounded queue from this stream, and memory stays
    /// flat whatever the total.
    ///
    /// The **order is unspecified**, and deliberately so: it is whichever order
    /// the index serving the traversal already yields. Imposing one would mean
    /// sorting, and sorting means buffering the whole result set before the
    /// first row — exactly what this method exists to avoid. Every contact is
    /// yielded exactly once; that is the whole contract.
    fn stream_contacts(
        &self,
        list: Option<ListId>,
    ) -> BoxStream<'_, Result<Contact, PersistenceError>>;

    /// Creates a contact list.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Conflict`] if the identifier already exists.
    fn insert_contact_list(
        &self,
        list: &ContactList,
    ) -> impl Future<Output = Result<(), PersistenceError>> + Send;

    /// Reads one contact list.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn find_contact_list(
        &self,
        list_id: ListId,
    ) -> impl Future<Output = Result<Option<ContactList>, PersistenceError>> + Send;

    /// Adds contacts to a list, in **one** transaction, ignoring those already
    /// in it.
    ///
    /// Returns how many memberships were created. Re-adding a contact is a
    /// no-op rather than an error: an import that overlaps a previous one is
    /// ordinary, not a fault.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the write fails — including when the
    /// list or a contact does not exist, which the foreign keys reject.
    fn add_contacts_to_list(
        &self,
        list_id: ListId,
        contacts: &[ContactId],
    ) -> impl Future<Output = Result<u64, PersistenceError>> + Send;
}

/// Reads and writes campaigns (spec §14.2, §10).
pub trait CampaignRepository {
    /// Inserts the campaign, or replaces it if its identifier already exists.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the write fails.
    fn upsert_campaign(
        &self,
        campaign: &Campaign,
    ) -> impl Future<Output = Result<(), PersistenceError>> + Send;

    /// Reads one campaign.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails, or
    /// [`PersistenceError::MalformedRow`] if a stored value no longer fits its
    /// type.
    fn find_campaign(
        &self,
        campaign_id: CampaignId,
    ) -> impl Future<Output = Result<Option<Campaign>, PersistenceError>> + Send;

    /// Reads one page of campaigns, in insertion order.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn page_campaigns(
        &self,
        cursor: Cursor,
        limit: u32,
    ) -> impl Future<Output = Result<Page<Campaign>, PersistenceError>> + Send;

    /// Deletes one campaign, reporting whether it existed.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the delete fails.
    fn delete_campaign(
        &self,
        campaign_id: CampaignId,
    ) -> impl Future<Output = Result<bool, PersistenceError>> + Send;
}

/// Reads the message journal in bulk (spec §14.2, §13.5).
///
/// # What became of `MessageRepository`
///
/// Its write-and-lookup half moved to `messaging` at milestone 006 — the
/// deadline ADR 0007 set itself, recorded by ADR 0010. This crate implements
/// that port on [`crate::SqliteMessageRepository`].
///
/// The three methods below did **not** move with it, and deliberately: their
/// caller is the log screen and the exporter of milestone 013, not the send
/// orchestrator, and a port belongs to the layer that consumes it. They also
/// speak in [`Cursor`] and [`Page`], types whose whole reason to exist is the
/// storage they page over. So they stay here until `logging-export` has the
/// logic to own them — the same argument ADR 0007 made, applied to the half
/// that still has no consumer.
pub trait MessageJournal {
    /// Reads one page of messages, in insertion order.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn page_messages(
        &self,
        filter: &MessageFilter,
        cursor: Cursor,
        limit: u32,
    ) -> impl Future<Output = Result<Page<Message>, PersistenceError>> + Send;

    /// Counts the messages a filter selects.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn count_messages(
        &self,
        filter: &MessageFilter,
    ) -> impl Future<Output = Result<u64, PersistenceError>> + Send;

    /// Traverses every message a filter selects, one row at a time.
    ///
    /// CA-002-05: the memory this holds does not grow with the size of the
    /// result set. That is what makes an export of a million-message campaign
    /// (spec §13.5) possible on a laptop.
    fn stream_messages(
        &self,
        filter: &MessageFilter,
    ) -> BoxStream<'_, Result<Message, PersistenceError>>;
}

/// Writes and reads the PDU log (spec §14.2, debug only).
///
/// Not one of the four ports step-002 §2 names; added for the same reason the
/// other four exist. `pdu_log` is a table of the schema, so without a
/// repository the only way to write to it would be SQL somewhere else, which
/// CA-002-03 forbids.
pub trait PduLogRepository {
    /// Appends one entry, returning its auto-incremented identifier.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the write fails.
    fn insert_entry(
        &self,
        entry: &PduLogEntry,
    ) -> impl Future<Output = Result<i64, PersistenceError>> + Send;

    /// Appends a batch of entries in **one** transaction.
    ///
    /// The PDU log records both directions of every PDU on a session that may
    /// run at a thousand messages a second (spec §9.5). One transaction per
    /// entry would mean one `fsync` per PDU, and turning the debug switch on
    /// would become a throughput cliff rather than a diagnostic.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the write fails — and then **no**
    /// entry of the batch is written.
    fn insert_entries(
        &self,
        entries: &[PduLogEntry],
    ) -> impl Future<Output = Result<u64, PersistenceError>> + Send;

    /// Reads one page of entries, oldest first, optionally for one session.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn page_entries(
        &self,
        session_id: Option<SessionId>,
        cursor: Cursor,
        limit: u32,
    ) -> impl Future<Output = Result<Page<PduLogEntry>, PersistenceError>> + Send;
}
