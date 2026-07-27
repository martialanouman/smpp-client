//! The business journal the user reads (deliverable L-008-03).
//!
//! # Not `tracing`
//!
//! `tracing` is the technical trace: spans, levels, a file an engineer opens
//! when something is wrong. This is the **functional** log — what was sent, to
//! whom, what the message centre answered, what its delivery receipt said — and
//! its reader is the operator. Two different things with two different
//! audiences, and the only reason to say so is that conflating them is how
//! message content ends up in a support archive.
//!
//! # What this module is, and what it is not
//!
//! It is a **query** layer: filters in, one page of rows out, plus the counts
//! the screen needs. Every row already exists — the send path writes it
//! (milestone 006) and the receipt pipeline updates it (milestone 008). Nothing
//! here writes.
//!
//! It is not the exporter. CSV, XLSX and JSON, the aggregate statistics and the
//! retention policy are milestone 014's, and step-008 §2 puts all three out of
//! scope.
//!
//! # Volumetry
//!
//! CA-008-07 wants 200 000 rows on screen with a filter applied in under a
//! second. Nothing here ever holds more than one page: the filtering and the
//! paging happen in SQLite, and the page size is the caller's. A method
//! returning `Vec<Message>` for the whole table would be the one way to break
//! that, so there is none.

use persistence::ports::MessageJournal;
use persistence::{
    Cursor, Message, MessageFilter, OrphanJournal, Page, PersistenceError, StoredOrphan,
};
use smpp_core::types::SessionId;

use crate::error::LoggingExportError;

/// How much of a message body the journal hands out.
///
/// # Why the default hides it
///
/// CLAUDE.md §8: message content is masked or truncated by default in any log
/// meant to be shared. The log screen is exactly that — it is what an operator
/// screenshots into a support ticket — and the body is the subscriber's
/// personal data.
///
/// step-008 §2 assigns the **user-facing setting** to milestone 015 and asks
/// this milestone only to carry the option in the model. So the option is here,
/// it defaults to [`Self::Truncated`], and nothing yet lets the interface ask
/// for [`Self::Full`]. Reversing that — full by default, masking added later —
/// would mean shipping a screen that leaks and fixing it afterwards.
///
/// # What this policy is deliberately NOT applied to
///
/// The **raw body of an orphaned receipt**. It was, and that was a bug that
/// contradicted three places at once: the note on
/// `messaging::correlation::OrphanReceipt::raw` ("an orphan whose body has been
/// redacted is an orphan nobody can diagnose"), the comment in the migration
/// that creates the table, and CA-008-04.
///
/// An orphan exists precisely because nothing could be made of it. The operator
/// opening it is asking "what did my provider actually send?", and
/// `<<< something no specifi…` does not answer that. The message content it may
/// carry sits behind `text:`, at the **end** of a body whose useful half —
/// `id:`, `stat:`, `err:` — comes first; truncating the whole thing throws away
/// the diagnosis to protect the part that a 24-character budget was never
/// reaching anyway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentVisibility {
    /// The first few characters, with an ellipsis when there is more.
    #[default]
    Truncated,
    /// The body, whole. Milestone 015 is what lets a user ask for this.
    Full,
}

/// Characters of a body [`ContentVisibility::Truncated`] keeps.
///
/// Enough to recognise which template a message came from, far short of a
/// one-time code or an address.
pub const TRUNCATED_BODY_CHARS: usize = 24;

impl ContentVisibility {
    /// Applies the policy to one body.
    ///
    /// Truncates on a **character** boundary, not a byte one: a body cut in
    /// the middle of an `é` is not a shorter string, it is a panic.
    #[must_use]
    pub fn apply(self, text: &str) -> String {
        if self == Self::Full || text.chars().count() <= TRUNCATED_BODY_CHARS {
            return text.to_owned();
        }

        let kept: String = text.chars().take(TRUNCATED_BODY_CHARS).collect();

        format!("{kept}…")
    }
}

/// One page of the business journal, and the cursor that continues it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalPage {
    /// The rows, oldest first.
    pub messages: Vec<Message>,
    /// Position to pass back for the next page, or `None` at the end.
    pub next: Option<Cursor>,
    /// How many rows the filter selects in total.
    ///
    /// Read once per query rather than derived from the page: a virtualised
    /// table needs the total to size its scrollbar, and it cannot get it by
    /// paging to the end.
    pub total: u64,
}

/// One page of orphaned receipts, and the cursor that continues it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanPage {
    /// The rows, oldest first.
    pub orphans: Vec<StoredOrphan>,
    /// Position to pass back for the next page, or `None` at the end.
    pub next: Option<Cursor>,
    /// How many orphans there are in total.
    pub total: u64,
}

/// Largest page this service will hand out.
///
/// The interface is untrusted (CLAUDE.md §3), so a `limit` crossing the IPC
/// boundary is an input to clamp rather than a number to obey: without this,
/// `limit = 4_000_000_000` is one query that materialises the whole table.
/// Five hundred is well past what a virtualised viewport shows at once.
pub const MAX_PAGE: u32 = 500;

/// Reads the business journal (spec §13.2).
///
/// Generic over its two stores so the whole of it is testable against doubles,
/// and so the SQLite types stay on the other side of the port.
#[derive(Debug, Clone)]
pub struct Journal<M, O> {
    messages: M,
    orphans: O,
    visibility: ContentVisibility,
}

impl<M, O> Journal<M, O>
where
    M: MessageJournal,
    O: OrphanJournal,
{
    /// A journal over the two stores, hiding message bodies.
    ///
    /// Two type parameters rather than one, because the two halves are two
    /// SQLite repositories: `messages` and `dlr_orphans` are different tables
    /// with different row types, and demanding a single type implementing both
    /// ports would only mean inventing a wrapper whose whole content is these
    /// two fields.
    #[must_use]
    pub const fn new(messages: M, orphans: O) -> Self {
        Self {
            messages,
            orphans,
            visibility: ContentVisibility::Truncated,
        }
    }

    /// The same journal under another content policy.
    ///
    /// Milestone 015 is what calls this with [`ContentVisibility::Full`], from
    /// a setting the user turned on.
    #[must_use]
    pub const fn showing(mut self, visibility: ContentVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Reads one page of messages, with its total.
    ///
    /// `limit` is clamped to `1..=MAX_PAGE`: see [`MAX_PAGE`].
    ///
    /// # Errors
    ///
    /// [`LoggingExportError::Unavailable`] if the store cannot be read.
    pub async fn page(
        &self,
        filter: &MessageFilter,
        cursor: Cursor,
        limit: u32,
    ) -> Result<JournalPage, LoggingExportError> {
        let limit = limit.clamp(1, MAX_PAGE);

        let page: Page<Message> = self
            .messages
            .page_messages(filter, cursor, limit)
            .await
            .map_err(unavailable)?;
        let total = self
            .messages
            .count_messages(filter)
            .await
            .map_err(unavailable)?;

        Ok(JournalPage {
            messages: page
                .items
                .into_iter()
                .map(|message| self.apply_visibility(message))
                .collect(),
            next: page.next,
            total,
        })
    }

    /// Reads one page of orphaned receipts (CA-008-04).
    ///
    /// # Errors
    ///
    /// [`LoggingExportError::Unavailable`] if the store cannot be read.
    pub async fn orphans(
        &self,
        session_id: Option<SessionId>,
        cursor: Cursor,
        limit: u32,
    ) -> Result<OrphanPage, LoggingExportError> {
        let limit = limit.clamp(1, MAX_PAGE);

        let page = self
            .orphans
            .page_orphans(session_id, cursor, limit)
            .await
            .map_err(unavailable)?;
        let total = self
            .orphans
            .count_orphans(session_id)
            .await
            .map_err(unavailable)?;

        Ok(OrphanPage {
            // NOT put through the content policy. See below.
            orphans: page.items,
            next: page.next,
            total,
        })
    }

    /// Applies the content policy to one row.
    fn apply_visibility(&self, mut message: Message) -> Message {
        message.text = message.text.map(|text| self.visibility.apply(&text));

        message
    }
}

/// Projects a storage failure onto this crate's error.
///
/// The `#[source]` chain — the driver error, and on one variant a filesystem
/// path — is logged here and does not travel: what this crate returns is
/// rendered towards the interface (CLAUDE.md §4).
fn unavailable(error: PersistenceError) -> LoggingExportError {
    tracing::error!(error = ?error, "the business journal could not be read");

    LoggingExportError::Unavailable {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{ContentVisibility, TRUNCATED_BODY_CHARS};

    #[test]
    fn a_short_body_is_handed_out_whole_under_either_policy() {
        for visibility in [ContentVisibility::Truncated, ContentVisibility::Full] {
            assert_eq!(visibility.apply("Bonjour"), "Bonjour");
        }
    }

    /// CLAUDE.md §8 — the default hides what a body holds beyond its opening.
    #[test]
    fn a_long_body_is_truncated_by_default() {
        let body = "Votre code de validation est 481902, valable dix minutes.";

        let shown = ContentVisibility::default().apply(body);

        assert!(!shown.contains("481902"), "{shown}");
        assert!(shown.ends_with('…'));
        assert_eq!(shown.chars().count(), TRUNCATED_BODY_CHARS + 1);
    }

    #[test]
    fn the_full_policy_hides_nothing() {
        let body = "Votre code de validation est 481902, valable dix minutes.";

        assert_eq!(ContentVisibility::Full.apply(body), body);
    }

    /// Truncating on a byte boundary would panic here rather than shorten:
    /// every character of this body is two octets.
    #[test]
    fn truncation_falls_on_a_character_boundary() {
        let body = "é".repeat(TRUNCATED_BODY_CHARS * 2);

        let shown = ContentVisibility::Truncated.apply(&body);

        assert_eq!(shown.chars().count(), TRUNCATED_BODY_CHARS + 1);
    }

    /// A body of exactly the budget keeps no ellipsis: an ellipsis says "there
    /// is more", and there is not.
    #[test]
    fn a_body_of_exactly_the_budget_is_not_marked_as_truncated() {
        let body = "a".repeat(TRUNCATED_BODY_CHARS);

        assert_eq!(ContentVisibility::Truncated.apply(&body), body);
    }

    /// **Non-regression: an orphan's raw body is handed over whole.**
    ///
    /// It was put through the content policy, so the one diagnostic an operator
    /// has for "my delivery rate is wrong" arrived as
    /// `<<< something no specifi…`. The useful half of a receipt body — `id:`,
    /// `stat:`, `err:` — is at the front and fits in nobody's 24 characters.
    ///
    /// The test drives [`Journal::orphans`] rather than asserting on a private
    /// helper: the helper was **deleted**, and a test on a helper cannot notice
    /// a call site being added back.
    #[tokio::test]
    async fn an_orphan_keeps_its_whole_body_under_every_policy() {
        const BODY: &str = "id:STRANGER sub:001 dlvrd:000 submit date:2607261200 \
                            done date:2607261205 stat:UNDELIV err:058 text:promotion du mois";

        for visibility in [ContentVisibility::Truncated, ContentVisibility::Full] {
            let store = OneOrphan(BODY);
            let journal = Journal::new(NoMessages, store).showing(visibility);

            let page = journal
                .orphans(None, Cursor::start(), 10)
                .await
                .expect("the double answers");

            assert_eq!(
                page.orphans[0].receipt.raw, BODY,
                "the raw body must survive {visibility:?} whole"
            );
            assert!(
                page.orphans[0].receipt.raw.contains("err:058"),
                "the diagnostic fields sit past any truncation budget"
            );
        }
    }

    // --- doubles for the orphan test ------------------------------------------

    use futures_core::stream::BoxStream;
    use messaging::correlation::{OrphanReason, OrphanReceipt};
    use persistence::{
        Message, MessageFilter, OrphanJournal, Page, PersistenceError, StoredOrphan, Timestamp,
    };
    use smpp_core::types::SessionId;

    use super::{Cursor, Journal};

    /// A message journal holding nothing: this test is about orphans.
    struct NoMessages;

    impl persistence::ports::MessageJournal for NoMessages {
        async fn page_messages(
            &self,
            _filter: &MessageFilter,
            _cursor: Cursor,
            _limit: u32,
        ) -> Result<Page<Message>, PersistenceError> {
            Ok(Page {
                items: Vec::new(),
                next: None,
            })
        }

        async fn count_messages(&self, _filter: &MessageFilter) -> Result<u64, PersistenceError> {
            Ok(0)
        }

        fn stream_messages(
            &self,
            _filter: &MessageFilter,
        ) -> BoxStream<'_, Result<Message, PersistenceError>> {
            Box::pin(futures_util::stream::empty())
        }
    }

    /// An orphan journal holding exactly one receipt, with this body.
    struct OneOrphan(&'static str);

    impl OrphanJournal for OneOrphan {
        async fn page_orphans(
            &self,
            _session_id: Option<SessionId>,
            _cursor: Cursor,
            _limit: u32,
        ) -> Result<Page<StoredOrphan>, PersistenceError> {
            Ok(Page {
                items: vec![StoredOrphan {
                    id: 1,
                    receipt: OrphanReceipt {
                        session_id: None,
                        smsc_message_id: Some(String::from("STRANGER")),
                        reason: OrphanReason::UnknownIdentifier,
                        dlr_stat: Some(String::from("UNDELIV")),
                        dlr_err: Some(String::from("058")),
                        submit_date: None,
                        done_date: None,
                        raw: self.0.to_owned(),
                        received_at: Timestamp::parse("2026-07-26T12:00:00Z")
                            .expect("valid instant"),
                    },
                }],
                next: None,
            })
        }

        async fn count_orphans(
            &self,
            _session_id: Option<SessionId>,
        ) -> Result<u64, PersistenceError> {
            Ok(1)
        }
    }
}
