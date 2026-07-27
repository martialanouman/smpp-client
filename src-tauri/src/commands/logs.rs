//! The log-screen commands of spec §15.2 (deliverable L-008-05).
//!
//! Four commands, all four moves of guide §8.3 and nothing else: deserialise,
//! validate, call the service, serialise. Every rule they appear to enforce —
//! the page ceiling, the content policy, which columns a search covers — lives
//! in `logging-export` or in `persistence`, where it can be tested without a
//! Tauri runtime.
//!
//! # Why the bulk goes through a command and not through an event
//!
//! CA-008-08. Two hundred thousand rows cannot cross the bridge as
//! notifications, and a screen that tried would freeze the WebView. So the
//! table fills itself with [`logs_query`], one page at a time, and
//! `message:update` carries only the aggregated increments of a committed
//! batch. Inspecting the IPC traffic during a campaign is what the criterion
//! asks for, and it shows exactly that: a handful of small events, and one page
//! request per scroll.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use logging_export::LoggingExportError;
use persistence::ports::PduLogRepository as _;
use persistence::{Cursor, Message, MessageFilter, MessageState, StoredOrphan};
use smpp_core::status_codes;
use smpp_core::time::Timestamp;
use smpp_core::types::{CampaignId, SessionId};

use crate::error::ErrorDto;
use crate::state::AppState;

/// Rows a page holds when the interface does not say.
///
/// A virtualised viewport shows a few dozen; asking for a hundred means a
/// scroll of three screens costs one round trip instead of three.
const DEFAULT_PAGE: u32 = 100;

/// A statement, not a test: a default above the journal's ceiling would be
/// silently clamped, so a caller reading this constant would be told one thing
/// and served another. Checked at compile time, so a change to either constant
/// fails the build rather than a test run.
const _: () = {
    assert!(DEFAULT_PAGE > 0);
    assert!(DEFAULT_PAGE <= logging_export::MAX_PAGE);
};

/// What the log screen filters on (spec §13.3).
///
/// Every field is optional and every one is a conjunction: an all-`None` filter
/// selects the whole journal. The strings are **raw** on purpose — validation
/// belongs to the backend, which treats the WebView as untrusted (CLAUDE.md
/// §3), so a malformed identifier is rejected here rather than assumed away.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogFilterInput {
    /// Restrict to one session, by identifier.
    pub(crate) session_id: Option<String>,
    /// Restrict to one campaign, by identifier.
    pub(crate) campaign_id: Option<String>,
    /// Restrict to one state — `QUEUED`, `SENT`, `ACCEPTED`, `DELIVERED`,
    /// `FAILED`, `EXPIRED`.
    pub(crate) state: Option<String>,
    /// Restrict to messages created at or after this RFC 3339 instant.
    pub(crate) created_from: Option<String>,
    /// Restrict to messages created at or before this RFC 3339 instant.
    pub(crate) created_to: Option<String>,
    /// Restrict to recipients starting with this prefix, `+` optional.
    pub(crate) dest_prefix: Option<String>,
    /// Restrict to one delivery-receipt error code.
    pub(crate) dlr_err: Option<String>,
    /// Restrict to rows containing this text in their recipient, body or SMSC
    /// identifier.
    pub(crate) search: Option<String>,
}

impl LogFilterInput {
    /// Projects the input onto the storage filter, rejecting what will not
    /// parse.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `SESSION_INVALID_ID` for a malformed identifier and
    /// `LOGS_INVALID_FILTER` for a state or an instant the model does not know.
    fn parse(&self) -> Result<MessageFilter, ErrorDto> {
        let mut filter = MessageFilter::all();

        if let Some(raw) = self.session_id.as_deref() {
            filter.session_id =
                Some(SessionId::parse(raw).map_err(|_| ErrorDto::session_invalid_id())?);
        }

        if let Some(raw) = self.campaign_id.as_deref() {
            filter.campaign_id =
                Some(CampaignId::parse(raw).map_err(|_| ErrorDto::session_invalid_id())?);
        }

        if let Some(raw) = self.state.as_deref() {
            filter.state = Some(
                MessageState::parse(raw).ok_or_else(|| ErrorDto::logs_invalid_filter("state"))?,
            );
        }

        filter.created_from = parse_instant(self.created_from.as_deref(), "createdFrom")?;
        filter.created_to = parse_instant(self.created_to.as_deref(), "createdTo")?;

        if let Some(prefix) = self.dest_prefix.as_deref().filter(|raw| !raw.is_empty()) {
            filter = filter.with_dest_prefix(prefix);
        }

        if let Some(code) = self.dlr_err.as_deref().filter(|raw| !raw.is_empty()) {
            filter = filter.with_dlr_err(code);
        }

        if let Some(needle) = self.search.as_deref().filter(|raw| !raw.is_empty()) {
            filter = filter.matching(needle);
        }

        Ok(filter)
    }
}

/// Reads a pagination cursor.
///
/// # Why it crosses as a string
///
/// A cursor is SQLite's `rowid`, an `i64`, and `specta` refuses to export a
/// 64-bit integer to TypeScript — JSON has no `BigInt` and `JSON.stringify`
/// throws on one, so a silent precision loss is the alternative. The repository
/// header of `src-tauri/src/ipc.rs` already states the rule: a 64-bit integer
/// goes over the bridge as a string.
///
/// A cursor that does not parse is **rejected** rather than read as "start
/// again". Starting again looks forgiving and is worse: the pager would fetch
/// page one for ever and the table would never reach its end.
fn parse_cursor(raw: Option<&str>) -> Result<Cursor, ErrorDto> {
    match raw.filter(|value| !value.is_empty()) {
        None => Ok(Cursor::start()),
        Some(value) => value
            .parse::<i64>()
            .map(Cursor::from_raw)
            .map_err(|_| ErrorDto::logs_invalid_filter("cursor")),
    }
}

/// Renders a cursor for the bridge.
fn render_cursor(cursor: Option<Cursor>) -> Option<String> {
    cursor.map(|position| position.into_raw().to_string())
}

/// Reads an optional RFC 3339 instant, naming the field that refused it.
fn parse_instant(raw: Option<&str>, field: &'static str) -> Result<Option<Timestamp>, ErrorDto> {
    match raw.filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => Timestamp::parse(value)
            .map(Some)
            .map_err(|_| ErrorDto::logs_invalid_filter(field)),
    }
}

/// One row of the log table (spec §13.2).
///
/// Flat and stringly on purpose: this is what a virtualised table renders, and
/// every field is a cell. The typed values — `Ton`, `DataCoding`,
/// `CommandStatus` — are rendered here rather than in the WebView, which
/// CLAUDE.md §3 keeps free of protocol knowledge.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogRowDto {
    /// The write-ahead key, and the row's React key.
    pub(crate) client_message_id: String,
    /// Session it went out on.
    pub(crate) session_id: Option<String>,
    /// Campaign it belongs to.
    pub(crate) campaign_id: Option<String>,
    /// Identifier the message centre assigned.
    pub(crate) smsc_message_id: Option<String>,
    /// Sender address.
    pub(crate) source_addr: Option<String>,
    /// Recipient.
    pub(crate) dest_addr: Option<String>,
    /// Segments the message was split into.
    pub(crate) segments: u32,
    /// The body, **truncated** by default (CLAUDE.md §8).
    pub(crate) text: Option<String>,
    /// `QUEUED`, `SENT`, `ACCEPTED`, `DELIVERED`, `FAILED`, `EXPIRED`.
    pub(crate) state: String,
    /// `command_status` of the response, as its numeric value.
    pub(crate) command_status: Option<u32>,
    /// Its symbol — `ESME_ROK`, `ESME_RTHROTTLED` — for the operator.
    pub(crate) command_status_symbol: Option<String>,
    /// `stat` field of the delivery receipt.
    pub(crate) dlr_stat: Option<String>,
    /// `err` field of the delivery receipt.
    pub(crate) dlr_err: Option<String>,
    /// Sending attempts spent.
    pub(crate) attempts: u32,
    /// When the row was written.
    pub(crate) created_at: String,
    /// When `submit_sm` left.
    pub(crate) sent_at: Option<String>,
    /// When the response came back.
    pub(crate) resp_at: Option<String>,
    /// When the delivery receipt arrived.
    pub(crate) dlr_at: Option<String>,
}

impl From<Message> for LogRowDto {
    fn from(message: Message) -> Self {
        Self {
            client_message_id: message.client_message_id.to_string(),
            session_id: message.session_id.map(|id| id.to_string()),
            campaign_id: message.campaign_id.map(|id| id.to_string()),
            smsc_message_id: message.smsc_message_id,
            source_addr: message.source_addr,
            dest_addr: message.dest_addr.map(|number| number.as_str().to_owned()),
            segments: message.segments,
            text: message.text,
            state: message.state.as_str().to_owned(),
            command_status: message.command_status.map(u32::from),
            command_status_symbol: message
                .command_status
                .and_then(|status| status_codes::describe(status).map(|entry| entry.symbol.into())),
            dlr_stat: message.dlr_stat,
            dlr_err: message.dlr_err,
            attempts: message.attempts,
            created_at: message.created_at.to_storage(),
            sent_at: message.sent_at.map(|at| at.to_storage()),
            resp_at: message.resp_at.map(|at| at.to_storage()),
            dlr_at: message.dlr_at.map(|at| at.to_storage()),
        }
    }
}

/// One page of the log table.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogPageDto {
    /// The rows, oldest first.
    pub(crate) rows: Vec<LogRowDto>,
    /// Cursor to pass back for the next page, or `null` at the end.
    ///
    /// A **string**, and opaque: the interface hands it back untouched. See
    /// [`parse_cursor`] for why it is not a number.
    pub(crate) next: Option<String>,
    /// How many rows the filter selects in total.
    ///
    /// What sizes the virtualised scrollbar. A `u32`, saturating: the bridge
    /// carries JSON and `JSON.stringify` throws on a `BigInt`.
    pub(crate) total: u32,
}

/// One orphaned delivery receipt (CA-008-04).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrphanRowDto {
    /// Row identifier, and the row's React key.
    ///
    /// A string for the same reason a cursor is one: it is a SQLite `rowid`.
    pub(crate) id: String,
    /// Session it arrived on.
    pub(crate) session_id: Option<String>,
    /// The identifier it quoted, when it quoted one.
    pub(crate) smsc_message_id: Option<String>,
    /// `UNKNOWN_ID` or `NO_IDENTIFIER` — the interface translates it.
    pub(crate) reason: String,
    /// `stat`, as the message centre wrote it.
    pub(crate) dlr_stat: Option<String>,
    /// `err`, as the message centre wrote it.
    pub(crate) dlr_err: Option<String>,
    /// The body, **truncated** by default (CLAUDE.md §8).
    pub(crate) raw: String,
    /// When this application received it.
    pub(crate) received_at: String,
}

impl From<StoredOrphan> for OrphanRowDto {
    fn from(orphan: StoredOrphan) -> Self {
        Self {
            id: orphan.id.to_string(),
            session_id: orphan.receipt.session_id.map(|id| id.to_string()),
            smsc_message_id: orphan.receipt.smsc_message_id,
            reason: orphan.receipt.reason.as_str().to_owned(),
            dlr_stat: orphan.receipt.dlr_stat,
            dlr_err: orphan.receipt.dlr_err,
            raw: orphan.receipt.raw,
            received_at: orphan.receipt.received_at.to_storage(),
        }
    }
}

/// One page of orphaned receipts.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrphanPageDto {
    /// The rows, oldest first.
    pub(crate) rows: Vec<OrphanRowDto>,
    /// Cursor to pass back for the next page, or `null` at the end.
    pub(crate) next: Option<String>,
    /// How many orphans there are in total.
    pub(crate) total: u32,
}

/// One recorded PDU (CA-008-09).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PduRowDto {
    /// Row identifier, and the row's React key.
    ///
    /// A string for the same reason a cursor is one: it is a SQLite `rowid`.
    pub(crate) id: String,
    /// Session it belonged to.
    pub(crate) session_id: Option<String>,
    /// `in` or `out`.
    pub(crate) direction: String,
    /// `command_id` of the header.
    pub(crate) command_id: Option<u32>,
    /// `command_status` of the header.
    pub(crate) command_status: Option<u32>,
    /// `sequence_number` of the header.
    pub(crate) sequence_number: Option<u32>,
    /// Hexadecimal dump — present only because debug mode was on.
    pub(crate) raw_hex: Option<String>,
    /// Decoded body and TLVs — same.
    pub(crate) decoded: Option<String>,
    /// When the PDU crossed the socket.
    pub(crate) ts: String,
}

/// One page of recorded PDUs, and whether recording is on.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PduPageDto {
    /// The entries, oldest first.
    pub(crate) rows: Vec<PduRowDto>,
    /// Cursor to pass back for the next page, or `null` at the end.
    pub(crate) next: Option<String>,
    /// Whether the recorder is on right now.
    ///
    /// Travels with the page so an empty table can say **why** it is empty:
    /// "nothing recorded" and "recording is off" are different states, and a
    /// screen that showed the same emptiness for both would have an operator
    /// hunting a bug that is a switch.
    pub(crate) enabled: bool,
}

/// Reads one page of the business journal (spec §13.3, EF-LOG-01).
///
/// # Errors
///
/// [`ErrorDto`] if a filter field does not parse, or if the journal cannot be
/// read.
#[tauri::command]
#[specta::specta]
pub(crate) async fn logs_query(
    state: State<'_, AppState>,
    filter: LogFilterInput,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<LogPageDto, ErrorDto> {
    let parsed = filter.parse()?;

    let page = state
        .logs()
        .journal()
        .page(
            &parsed,
            parse_cursor(cursor.as_deref())?,
            limit.unwrap_or(DEFAULT_PAGE),
        )
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    Ok(LogPageDto {
        rows: page.messages.into_iter().map(LogRowDto::from).collect(),
        next: render_cursor(page.next),
        total: narrow(page.total),
    })
}

/// Reads one page of the orphaned receipts (CA-008-04).
///
/// # Errors
///
/// [`ErrorDto`] if the session identifier does not parse, or if the journal
/// cannot be read.
#[tauri::command]
#[specta::specta]
pub(crate) async fn logs_orphans(
    state: State<'_, AppState>,
    session_id: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<OrphanPageDto, ErrorDto> {
    let session = session_id
        .as_deref()
        .filter(|raw| !raw.is_empty())
        .map(|raw| SessionId::parse(raw).map_err(|_| ErrorDto::session_invalid_id()))
        .transpose()?;

    let page = state
        .logs()
        .journal()
        .orphans(
            session,
            parse_cursor(cursor.as_deref())?,
            limit.unwrap_or(DEFAULT_PAGE),
        )
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    Ok(OrphanPageDto {
        rows: page.orphans.into_iter().map(OrphanRowDto::from).collect(),
        next: render_cursor(page.next),
        total: narrow(page.total),
    })
}

/// Reads one page of the PDU log, and reports whether recording is on.
///
/// # Errors
///
/// [`ErrorDto`] if the session identifier does not parse, or if the log cannot
/// be read.
#[tauri::command]
#[specta::specta]
pub(crate) async fn logs_pdus(
    state: State<'_, AppState>,
    session_id: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<PduPageDto, ErrorDto> {
    let session = session_id
        .as_deref()
        .filter(|raw| !raw.is_empty())
        .map(|raw| SessionId::parse(raw).map_err(|_| ErrorDto::session_invalid_id()))
        .transpose()?;

    // Whatever is still buffered belongs on the screen: an operator who turns
    // recording on, sends one message and looks would otherwise see nothing
    // until sixty-three more PDUs had gone by.
    if let Err(error) = state.logs().recorder().flush().await {
        tracing::warn!(error = %error, "the PDU log could not be flushed before a read");
    }

    let page = state
        .logs()
        .pdu_log()
        .page_entries(
            session,
            parse_cursor(cursor.as_deref())?,
            limit
                .unwrap_or(DEFAULT_PAGE)
                .clamp(1, logging_export::MAX_PAGE),
        )
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    Ok(PduPageDto {
        rows: page
            .items
            .into_iter()
            .map(|stored| PduRowDto {
                id: stored.id.to_string(),
                session_id: stored.entry.session_id.map(|id| id.to_string()),
                direction: stored.entry.direction.as_str().to_owned(),
                command_id: stored.entry.command_id,
                command_status: stored.entry.command_status,
                sequence_number: stored.entry.sequence_number,
                raw_hex: stored.entry.raw_hex,
                decoded: stored.entry.decoded,
                ts: stored.entry.ts.to_storage(),
            })
            .collect(),
        next: render_cursor(page.next),
        enabled: state.logs().recorder().is_enabled(),
    })
}

/// Turns PDU recording on or off (CA-008-09).
///
/// Returns the state in force, so the interface reflects what happened rather
/// than what it asked for.
///
/// # Errors
///
/// [`ErrorDto`] if the buffered entries could not be written on the way out.
#[tauri::command]
#[specta::specta]
pub(crate) async fn logs_set_pdu_logging(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, ErrorDto> {
    state.logs().recorder().set_enabled(enabled);

    // Switching off flushes: what was recorded before the switch belongs in
    // the log, and leaving it in a buffer nobody will drain would lose exactly
    // the PDUs the operator turned the recorder on for.
    if !enabled {
        state
            .logs()
            .recorder()
            .flush()
            .await
            .map_err(|error| ErrorDto::from(&error))?;
    }

    Ok(state.logs().recorder().is_enabled())
}

/// A total narrowed for the bridge, saturating rather than wrapping.
///
/// Four billion rows is past what any journal reaches; the worst case is a
/// scrollbar that stops growing rather than one that resets.
fn narrow(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl From<&LoggingExportError> for ErrorDto {
    /// Projects a journal failure onto the IPC contract.
    ///
    /// One code, and deliberately: a locked file, a malformed row and a query
    /// that would not run are the same thing to the interface — the journal
    /// would not answer — and the distinction that matters is in the trace,
    /// with the source chain the DTO drops.
    fn from(error: &LoggingExportError) -> Self {
        Self::logs_unavailable(error)
    }
}

#[cfg(test)]
mod tests {
    use super::LogFilterInput;
    use crate::error::ErrorCode;
    use persistence::MessageState;

    #[test]
    fn an_empty_filter_restricts_nothing() {
        let filter = LogFilterInput::default().parse().expect("valid");

        assert_eq!(filter, persistence::MessageFilter::all());
    }

    #[test]
    fn every_field_reaches_the_storage_filter() {
        let input = LogFilterInput {
            session_id: None,
            campaign_id: None,
            state: Some(String::from("DELIVERED")),
            created_from: Some(String::from("2026-07-01T00:00:00Z")),
            created_to: Some(String::from("2026-07-31T00:00:00Z")),
            dest_prefix: Some(String::from("+225")),
            dlr_err: Some(String::from("058")),
            search: Some(String::from("promotion")),
        };

        let filter = input.parse().expect("valid");

        assert_eq!(filter.state, Some(MessageState::Delivered));
        assert!(filter.created_from.is_some());
        assert!(filter.created_to.is_some());
        // The `+` is stripped where the two forms meet, in `persistence`.
        assert_eq!(filter.dest_prefix.as_deref(), Some("225"));
        assert_eq!(filter.dlr_err.as_deref(), Some("058"));
        assert_eq!(filter.search.as_deref(), Some("promotion"));
    }

    /// An empty string is "the operator cleared the box", not "match the empty
    /// string" — which `LIKE '%%'` would make match everything and
    /// `dlr_err = ''` would make match nothing.
    #[test]
    fn an_empty_string_clears_a_criterion_rather_than_matching_one() {
        let input = LogFilterInput {
            dest_prefix: Some(String::new()),
            dlr_err: Some(String::new()),
            search: Some(String::new()),
            created_from: Some(String::new()),
            ..LogFilterInput::default()
        };

        let filter = input.parse().expect("valid");

        assert_eq!(filter, persistence::MessageFilter::all());
    }

    /// The WebView is untrusted (CLAUDE.md §3): a hand-crafted `invoke` takes
    /// the same path as the form and is refused with a code, not a panic.
    #[test]
    fn a_malformed_state_is_refused_with_a_stable_code() {
        let input = LogFilterInput {
            state: Some(String::from("PENDING")),
            ..LogFilterInput::default()
        };

        assert_eq!(
            input.parse().expect_err("refused").code,
            ErrorCode::LogsInvalidFilter
        );
    }

    #[test]
    fn a_malformed_instant_is_refused_with_a_stable_code() {
        let input = LogFilterInput {
            created_to: Some(String::from("31/07/2026")),
            ..LogFilterInput::default()
        };

        assert_eq!(
            input.parse().expect_err("refused").code,
            ErrorCode::LogsInvalidFilter
        );
    }

    #[test]
    fn a_malformed_session_identifier_is_refused_with_a_stable_code() {
        let input = LogFilterInput {
            session_id: Some(String::from("not-a-uuid")),
            ..LogFilterInput::default()
        };

        assert_eq!(
            input.parse().expect_err("refused").code,
            ErrorCode::SessionInvalidId
        );
    }
}
