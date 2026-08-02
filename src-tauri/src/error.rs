//! The stable IPC error DTO.
//!
//! Every command returns `Result<T, ErrorDto>`. The shape
//! `{ code, message, details }` is a **durable design point** (milestone 001
//! §6): every later milestone adds codes to it, none reshapes it.
//!
//! The three fields answer three different questions, and conflating them is
//! what makes an error contract rot:
//!
//! - `code` — machine-readable and **stable**. The frontend branches on it and
//!   uses it as an i18n key. Renaming one is a breaking change to the IPC
//!   contract (CLAUDE.md §6: major bump).
//! - `message` — a short English sentence for the logs and for a developer.
//!   Never shown raw to the user, who sees the translation of `code`.
//! - `details` — optional, structured, machine-readable. Bounds, accepted
//!   values, the offending field. Never free-form prose.
//!
//! # What must never cross
//!
//! CA-001-06: no absolute path, no secret, no internal trace. The guarantee is
//! structural rather than a matter of care — a DTO is only ever built from a
//! **fixed** `ConfigError` message and from `details` assembled here from
//! constants. The `#[source]` chain, which is where the paths live, is logged
//! and dropped.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::config::ConfigError;

/// Stable, machine-readable error identifier.
///
/// Serialised in `SCREAMING_SNAKE_CASE` — `CONFIG_INVALID_LANGUAGE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
// The shared `Config` prefix is the point, not an accident: a code is
// `<DOMAIN>_<REASON>`, and later milestones add `Smpp*`, `Session*`,
// `Contacts*` beside it. Dropping the prefix here would produce `Malformed`
// and `Unwritable`, which say nothing on their own once the enum holds forty
// variants — and would break the `CONFIG_*` naming the frontend keys off.
#[allow(clippy::enum_variant_names)]
pub(crate) enum ErrorCode {
    /// The submitted language is not supported.
    ConfigInvalidLanguage,
    /// The submitted theme is not supported.
    ConfigInvalidTheme,
    /// The submitted log level is not supported.
    ConfigInvalidLogLevel,
    /// The submitted retention falls outside the accepted range.
    ConfigInvalidRetention,
    /// The preferences file could not be read.
    ConfigUnreadable,
    /// The preferences file could not be written.
    ConfigUnwritable,
    /// The preferences file is not valid JSON.
    ConfigMalformed,

    /// A session profile field failed validation.
    SessionInvalidProfile,
    /// The `session_id` is not a well-formed identifier.
    SessionInvalidId,
    /// No profile carries that identifier.
    SessionNotFound,
    /// Another session is already live (milestone 011 lifts this).
    SessionBusy,
    /// The message centre refused the bind.
    SessionBindRejected,
    /// The socket failed.
    SessionTransport,
    /// The session is gone, or its tasks ended abnormally.
    SessionClosed,
    /// The database could not be read or written.
    SessionStorage,

    /// The recipient is not a number this client can put on the wire.
    MessageInvalidDestination,
    /// The sender address was refused.
    MessageInvalidSource,
    /// A field of spec §7.3 does not fit its protocol slot.
    MessageInvalidField,
    /// A custom TLV is not readable hexadecimal, or is too long.
    MessageInvalidTlv,
    /// The text cannot be written under the chosen encoding, or needs more
    /// than 255 segments.
    MessageEncoding,
    /// No live session carries the identifier the interface sent.
    MessageSessionNotBound,
    /// The message journal refused a read or a write.
    ///
    /// On the write-ahead insert this means **nothing was sent**: the
    /// orchestrator does not submit a message it could not persist. A journal
    /// failure *after* the send is not reported here at all — it comes back on
    /// the successful result as `journalled: false`, because the two say
    /// opposite things.
    MessageStorage,
    /// A message already exists under that `client_message_id`.
    ///
    /// Its own code rather than [`Self::MessageStorage`]: a replay is the
    /// guard that makes a resumed send idempotent (spec §10.5), and it is not
    /// a fault the way a full disk is. The interface tells the operator the
    /// message is already there, not that the database broke.
    MessageDuplicate,

    /// A log-screen filter field could not be read.
    ///
    /// Its own code rather than a message-domain one: the operator typed a date
    /// or picked a state, and what the interface has to do is point at the
    /// offending box — which `details` names.
    LogsInvalidFilter,
    /// The business journal or the PDU log could not be read.
    LogsUnavailable,

    /// The file an import points at could not be read or made sense of.
    ///
    /// Covers a missing file, an unreadable sheet and a mapping that names a
    /// column the file does not have. One code, because the operator's next
    /// move is the same in all three: look at `details`, which names the file
    /// position or the column, and fix the file or the mapping.
    ContactsImportRejected,
    /// An import is already running.
    ///
    /// Its own code rather than a storage one: nothing is broken, and the
    /// interface has to say "wait or cancel", not "retry".
    ContactsImportBusy,
    /// The contact store could not be read or written.
    ContactsStorage,
    /// A contact or a list already exists under that identifier.
    ContactsDuplicate,
    /// The contact, list or import profile a call referred to does not exist.
    ContactsNotFound,
    /// The file an import names was not chosen in the native picker.
    ///
    /// Its own code because it is not a fault of the file: it is the
    /// application refusing to open something the operator did not point at.
    /// The interface tells them to pick the file again.
    ContactsFileNotPicked,
    /// A field of a contacts call could not be read.
    ///
    /// Distinct from [`Self::ContactsImportRejected`], which is about the
    /// operator's **file**. This one is about the call itself — an identifier
    /// that is not one, a cursor that does not parse — so it points at the
    /// interface, not at something the operator can fix by editing a
    /// spreadsheet.
    ContactsInvalidInput,
}

/// Key of the `details` entry naming the offending field.
const FIELD: &str = "field";

/// Key of the `details` entry listing the accepted values.
const ALLOWED: &str = "allowed";

/// The error handed to the WebView.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub(crate) struct ErrorDto {
    /// Stable identifier the frontend branches on and translates.
    pub(crate) code: ErrorCode,
    /// Short English sentence, for logs and developers.
    pub(crate) message: String,
    /// Optional structured context — never prose, never a path.
    pub(crate) details: Option<BTreeMap<String, String>>,
}

impl ErrorDto {
    /// Builds a DTO with no details.
    fn bare(code: ErrorCode, message: &impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
            details: None,
        }
    }

    /// Builds a DTO carrying structured details.
    fn detailed(
        code: ErrorCode,
        message: &impl ToString,
        details: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        Self {
            code,
            message: message.to_string(),
            details: Some(
                details
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value))
                    .collect(),
            ),
        }
    }
}

impl From<&ConfigError> for ErrorDto {
    /// Projects a typed configuration error onto the IPC contract.
    ///
    /// Takes a reference so the caller keeps the error and can log it **with**
    /// its `#[source]` chain — the part that carries the path, and the part
    /// that must not travel.
    fn from(error: &ConfigError) -> Self {
        match error {
            ConfigError::InvalidLanguage { allowed } => Self::detailed(
                ErrorCode::ConfigInvalidLanguage,
                error,
                [
                    (FIELD, "language".to_owned()),
                    (ALLOWED, allowed.join(", ")),
                ],
            ),
            ConfigError::InvalidTheme { allowed } => Self::detailed(
                ErrorCode::ConfigInvalidTheme,
                error,
                [(FIELD, "theme".to_owned()), (ALLOWED, allowed.join(", "))],
            ),
            ConfigError::InvalidLogLevel { allowed } => Self::detailed(
                ErrorCode::ConfigInvalidLogLevel,
                error,
                [
                    (FIELD, "logLevel".to_owned()),
                    (ALLOWED, allowed.join(", ")),
                ],
            ),
            ConfigError::InvalidRetention { min, max } => Self::detailed(
                ErrorCode::ConfigInvalidRetention,
                error,
                [
                    (FIELD, "retentionDays".to_owned()),
                    ("min", min.to_string()),
                    ("max", max.to_string()),
                ],
            ),
            ConfigError::Unreadable(_) => Self::bare(ErrorCode::ConfigUnreadable, error),
            ConfigError::Unwritable(_) => Self::bare(ErrorCode::ConfigUnwritable, error),
            ConfigError::Malformed(_) => Self::bare(ErrorCode::ConfigMalformed, error),
        }
    }
}

impl ErrorDto {
    /// The identifier the interface sent is not a UUID.
    pub(crate) fn session_invalid_id() -> Self {
        Self::detailed(
            ErrorCode::SessionInvalidId,
            &"session identifier is not a well-formed UUID",
            [(FIELD, "sessionId".to_owned())],
        )
    }

    /// No profile carries that identifier.
    pub(crate) fn session_not_found() -> Self {
        Self::bare(ErrorCode::SessionNotFound, &"no such session profile")
    }

    /// The recipient was refused.
    ///
    /// The rejection message is the one `messaging` produced, and no variant
    /// of `AddressError` echoes the value it refused — an MSISDN is personal
    /// data (CLAUDE.md §8), and there is a test on that side.
    pub(crate) fn message_invalid_destination(error: &impl ToString) -> Self {
        Self::detailed(
            ErrorCode::MessageInvalidDestination,
            error,
            [(FIELD, "destination".to_owned())],
        )
    }

    /// The sender address was refused.
    pub(crate) fn message_invalid_source(error: &impl ToString) -> Self {
        Self::detailed(
            ErrorCode::MessageInvalidSource,
            error,
            [(FIELD, "source".to_owned())],
        )
    }

    /// A field of spec §7.3 does not fit.
    pub(crate) fn message_invalid_field(error: &impl ToString) -> Self {
        Self::bare(ErrorCode::MessageInvalidField, error)
    }

    /// A custom TLV could not be read.
    pub(crate) fn message_invalid_tlv() -> Self {
        Self::detailed(
            ErrorCode::MessageInvalidTlv,
            &"a TLV value must be an even number of hexadecimal digits",
            [(FIELD, "tlvs".to_owned())],
        )
    }

    /// The text could not be encoded or split.
    pub(crate) fn message_encoding(message: &str) -> Self {
        Self::detailed(
            ErrorCode::MessageEncoding,
            &message,
            [(FIELD, "text".to_owned())],
        )
    }

    /// No live session carries that identifier.
    pub(crate) fn message_session_not_bound() -> Self {
        Self::bare(
            ErrorCode::MessageSessionNotBound,
            &"no live session carries that identifier",
        )
    }

    /// The journal refused the operation.
    ///
    /// One code for every storage failure, and no details: `MessageStoreError`
    /// already drops the source chain, and what is left — "database query
    /// failed" — says nothing the interface can act on beyond the code itself.
    pub(crate) fn message_storage() -> Self {
        Self::bare(
            ErrorCode::MessageStorage,
            &"the message journal refused the operation",
        )
    }

    /// A message already exists under that identifier.
    pub(crate) fn message_duplicate() -> Self {
        Self::bare(
            ErrorCode::MessageDuplicate,
            &"a message already exists under this client_message_id",
        )
    }

    /// A log-screen filter field could not be read.
    ///
    /// `details` names the field and **not** the value: the rule that no column
    /// value crosses the boundary (CA-001-06) is only worth anything if it has
    /// no exceptions, and a date the operator typed is still their data.
    pub(crate) fn logs_invalid_filter(field: &'static str) -> Self {
        Self::detailed(
            ErrorCode::LogsInvalidFilter,
            &"a log filter field could not be read",
            [(FIELD, field.to_owned())],
        )
    }

    /// The journal would not answer.
    pub(crate) fn logs_unavailable(error: &impl ToString) -> Self {
        Self::bare(ErrorCode::LogsUnavailable, error)
    }
}

impl From<&smpp_session::SessionError> for ErrorDto {
    /// Projects a session failure onto the IPC contract.
    ///
    /// # Why every arm is written out
    ///
    /// A catch-all would be shorter and would quietly give a new variant the
    /// wrong code — and the code is what the interface branches on and
    /// translates. Listing them makes a new variant a compile error here,
    /// which is where the decision belongs.
    ///
    /// No arm carries a value from the failure into `details`: a
    /// `SessionError` message names a status or a field, never a credential
    /// (there is a test in `smpp-session`), and keeping `details` to constants
    /// is what makes CA-001-06 structural rather than careful.
    fn from(error: &smpp_session::SessionError) -> Self {
        use smpp_session::SessionError as Failure;

        match error {
            Failure::InvalidProfile { field, .. } => Self::detailed(
                ErrorCode::SessionInvalidProfile,
                error,
                [(FIELD, (*field).to_owned())],
            ),
            Failure::TooManySessions { .. } => Self::bare(ErrorCode::SessionBusy, error),
            Failure::BindRejected { symbol, .. } => Self::detailed(
                ErrorCode::SessionBindRejected,
                error,
                [("status", (*symbol).to_owned())],
            ),
            Failure::Transport { .. } | Failure::Protocol(_) | Failure::ResponseTimeout { .. } => {
                Self::bare(ErrorCode::SessionTransport, error)
            }
            Failure::Persistence(inner) => Self::from(inner),
            Failure::Closed
            | Failure::Cancelled
            | Failure::NotBound { .. }
            | Failure::OperationNotAllowed { .. }
            | Failure::IllegalTransition { .. }
            | Failure::UnexpectedResponse { .. }
            | Failure::SequenceSpaceExhausted { .. } => Self::bare(ErrorCode::SessionClosed, error),
            // `SessionError` is `#[non_exhaustive]`, so a wildcard is
            // required. It reports the most conservative code rather than
            // guessing.
            _ => Self::bare(ErrorCode::SessionClosed, error),
        }
    }
}

impl ErrorDto {
    /// The refusal an import gets while another one is running.
    ///
    /// Not built from an error type: the condition is a state of the
    /// application, not a failure of anything, and there is no source chain to
    /// carry.
    pub(crate) fn contacts_import_busy() -> Self {
        Self {
            code: ErrorCode::ContactsImportBusy,
            message: String::from("an import is already running"),
            details: None,
        }
    }

    /// An import named a file the picker never handed back.
    ///
    /// Carries no path — naming the file the caller asked for would report
    /// back what it already knows, and CA-001-06 keeps paths off the bridge
    /// either way.
    pub(crate) fn contacts_file_not_picked() -> Self {
        Self {
            code: ErrorCode::ContactsFileNotPicked,
            message: String::from("the file was not chosen through the native picker"),
            details: None,
        }
    }

    /// The contact store would not answer a read.
    ///
    /// Takes a `ToString` rather than a `&ContactStoreError` because the
    /// contacts screen reads through `ContactDirectory`, a `persistence` port
    /// that returns `PersistenceError`, while the writes go through
    /// `contacts::ports`. One code either way — what the operator does about
    /// it is the same, and the `#[source]` chain stays on this side.
    pub(crate) fn contacts_storage(error: &impl ToString) -> Self {
        Self::bare(ErrorCode::ContactsStorage, error)
    }

    /// A field of a contacts call could not be read.
    ///
    /// `details` names the field and never carries its value, for the reason
    /// [`Self::logs_invalid_filter`] gives: the rule that no operator value
    /// crosses the boundary is only worth anything without exceptions.
    pub(crate) fn contacts_invalid_input(field: &'static str) -> Self {
        Self::detailed(
            ErrorCode::ContactsInvalidInput,
            &"a contacts field could not be read",
            [(FIELD, field.to_owned())],
        )
    }
}

impl From<&contacts::ContactsError> for ErrorDto {
    /// Projects a read or mapping failure onto the IPC contract.
    ///
    /// `details` carries the position — the line, the sheet, the column name —
    /// because an import that fails without saying **where** is unusable on a
    /// file of fifty thousand rows, which is the fiche's own argument.
    fn from(error: &contacts::ContactsError) -> Self {
        Self::bare(ErrorCode::ContactsImportRejected, error)
    }
}

impl From<&contacts::ports::ContactStoreError> for ErrorDto {
    /// Projects a store failure onto the IPC contract.
    ///
    /// The three outcomes the port names stay distinct here, because the
    /// operator's next move differs: a conflict means the record is already
    /// there, a missing row means the list was deleted under them, and
    /// unavailability means retry or check the disk.
    fn from(error: &contacts::ports::ContactStoreError) -> Self {
        use contacts::ports::ContactStoreError as Store;

        let code = match error {
            Store::Conflict => ErrorCode::ContactsDuplicate,
            Store::NotFound => ErrorCode::ContactsNotFound,
            _ => ErrorCode::ContactsStorage,
        };

        Self::bare(code, error)
    }
}

impl From<&persistence::PersistenceError> for ErrorDto {
    /// Projects a storage failure onto the IPC contract.
    ///
    /// One code for all of them, and deliberately: a `MalformedRow`, a
    /// conflict and a locked file are the same thing to the interface — the
    /// database would not cooperate — and the distinction that matters is in
    /// the log, with the source chain the DTO drops.
    fn from(error: &persistence::PersistenceError) -> Self {
        Self::bare(ErrorCode::SessionStorage, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ConfigSetInput, Language, LogLevel, RetentionDays, Theme};

    /// Every `ConfigError` variant, each fed a source that carries an absolute
    /// path and a password — the two things CA-001-06 forbids from travelling.
    fn every_error() -> Vec<ConfigError> {
        let poisoned = || {
            std::io::Error::other(
                "/Users/someone/Library/Application Support/com.shinobismpp.desktop/config.json \
                 password=hunter2",
            )
        };

        vec![
            ConfigError::InvalidLanguage {
                allowed: Language::ALLOWED,
            },
            ConfigError::InvalidTheme {
                allowed: Theme::ALLOWED,
            },
            ConfigError::InvalidLogLevel {
                allowed: LogLevel::ALLOWED,
            },
            ConfigError::InvalidRetention {
                min: RetentionDays::MIN,
                max: RetentionDays::MAX,
            },
            ConfigError::Unreadable(poisoned()),
            ConfigError::Unwritable(poisoned()),
            ConfigError::Malformed(
                serde_json::from_str::<AppConfig>("{ not json")
                    .expect_err("this input is not valid JSON"),
            ),
        ]
    }

    #[test]
    fn maps_each_error_to_its_own_stable_code() {
        let codes: Vec<_> = every_error()
            .iter()
            .map(|error| ErrorDto::from(error).code)
            .collect();

        assert_eq!(
            codes,
            vec![
                ErrorCode::ConfigInvalidLanguage,
                ErrorCode::ConfigInvalidTheme,
                ErrorCode::ConfigInvalidLogLevel,
                ErrorCode::ConfigInvalidRetention,
                ErrorCode::ConfigUnreadable,
                ErrorCode::ConfigUnwritable,
                ErrorCode::ConfigMalformed,
            ]
        );
    }

    #[test]
    fn leaks_neither_a_path_nor_a_secret_nor_an_internal_trace() {
        for error in every_error() {
            let dto = ErrorDto::from(&error);

            let mut surface = vec![dto.message.clone()];
            surface.extend(dto.details.into_iter().flatten().flat_map(|(k, v)| [k, v]));

            for text in surface {
                assert!(!text.contains('/'), "path leaked: {text}");
                assert!(!text.contains('\\'), "path leaked: {text}");
                assert!(!text.contains("password"), "secret leaked: {text}");
                assert!(
                    !text.contains("config.json"),
                    "internal name leaked: {text}"
                );
                assert!(!text.contains("src-tauri"), "internal path leaked: {text}");
                assert!(
                    !text.contains("shinobismpp::"),
                    "internal trace leaked: {text}"
                );
            }
        }
    }

    #[test]
    fn serialises_codes_in_screaming_snake_case() {
        let json = serde_json::to_string(&ErrorCode::ConfigInvalidLanguage)
            .expect("serialisation must succeed");

        assert_eq!(json, "\"CONFIG_INVALID_LANGUAGE\"");
    }

    #[test]
    fn exposes_the_accepted_values_of_a_rejected_field() {
        let error = AppConfig::parse(ConfigSetInput {
            language: "kl".to_owned(),
            theme: "dark".to_owned(),
            log_level: "info".to_owned(),
            retention_days: 30,
        })
        .expect_err("an unknown language must be rejected");

        let dto = ErrorDto::from(&error);
        let details = dto.details.expect("a validation error carries details");

        assert_eq!(details.get(FIELD).map(String::as_str), Some("language"));
        assert_eq!(details.get(ALLOWED).map(String::as_str), Some("fr, en"));
    }

    #[test]
    fn exposes_the_bounds_of_a_rejected_retention() {
        let error = RetentionDays::parse(0).expect_err("zero is out of bounds");
        let details = ErrorDto::from(&error)
            .details
            .expect("bounds are carried as details");

        assert_eq!(details.get("min").map(String::as_str), Some("1"));
        assert_eq!(details.get("max").map(String::as_str), Some("365"));
    }

    #[test]
    fn carries_no_details_for_a_filesystem_failure() {
        let error = ConfigError::Unreadable(std::io::Error::from(std::io::ErrorKind::NotFound));

        assert_eq!(ErrorDto::from(&error).details, None);
    }
}
