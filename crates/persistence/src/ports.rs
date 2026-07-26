//! The repository traits, one per aggregate.
//!
//! # Where these traits live, and why here for now
//!
//! Guide §8.1 puts a port in the crate that **consumes** it: `MessageRepository`
//! belongs to `messaging`, which then depends on nothing below it and can be
//! tested against a double. step-002 §6 asks for the placement to be decided
//! explicitly and written down — ADR 0007 does that, and this is its summary.
//!
//! The consuming crates are empty shells at this milestone: `messaging` starts
//! at milestone 004, `contacts` at 006. Declaring the traits there today would
//! mean creating a `persistence` → `messaging` edge to reach them, i.e. paying
//! the inversion's whole cost — the upward-looking dependency — while nobody
//! is using it to invert anything. So the traits sit here, next to their only
//! implementation, and move up the day a consumer exists to own them. That
//! move is a declaration change: the trait's shape, and every implementation
//! and double written against it, are unaffected.
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
use smpp_core::types::{ClientMessageId, SessionId};

use crate::records::{
    Campaign, CampaignId, Contact, ContactId, ContactList, ListId, Message, MessageFilter,
    MessageStateUpdate, PduLogEntry, SessionProfile,
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

/// Reads and writes the message journal (spec §14.2, CLAUDE.md §4).
pub trait MessageRepository {
    /// Writes one message, before it is sent.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Conflict`] if the `client_message_id` already
    /// exists — which is the guard that makes a replayed send idempotent
    /// (spec §10.5).
    fn insert_message(
        &self,
        message: &Message,
    ) -> impl Future<Output = Result<(), PersistenceError>> + Send;

    /// Writes a batch of messages in **one** transaction.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Conflict`] if any identifier already exists — and
    /// then **no** message of the batch is written.
    fn insert_messages(
        &self,
        messages: &[Message],
    ) -> impl Future<Output = Result<u64, PersistenceError>> + Send;

    /// Reads one message by its client-side identifier.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails, or
    /// [`PersistenceError::MalformedRow`] if a stored value no longer fits its
    /// type.
    fn find_message(
        &self,
        client_message_id: ClientMessageId,
    ) -> impl Future<Output = Result<Option<Message>, PersistenceError>> + Send;

    /// Reads one message by the identifier the SMSC assigned.
    ///
    /// The lookup a delivery receipt needs (spec §7.8, milestone 008), which
    /// is why `idx_messages_smscid` exists from this milestone on.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::Database`] if the read fails.
    fn find_message_by_smsc_id(
        &self,
        smsc_message_id: &str,
    ) -> impl Future<Output = Result<Option<Message>, PersistenceError>> + Send;

    /// Applies one state transition.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::NotFound`] if the message does not exist.
    fn update_state(
        &self,
        update: &MessageStateUpdate,
    ) -> impl Future<Output = Result<(), PersistenceError>> + Send;

    /// Applies a batch of state transitions in **one** transaction.
    ///
    /// CA-002-06: N transitions produce one transaction, not N. On the hot
    /// path of a campaign this is the difference between one commit per
    /// response window and one per message.
    ///
    /// All-or-nothing, and that is observable: if any message of the batch is
    /// missing, the whole batch is rolled back and **none** of the transitions
    /// applies.
    ///
    /// # Errors
    ///
    /// [`PersistenceError::NotFound`] if any message does not exist.
    fn update_states(
        &self,
        updates: &[MessageStateUpdate],
    ) -> impl Future<Output = Result<u64, PersistenceError>> + Send;

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
