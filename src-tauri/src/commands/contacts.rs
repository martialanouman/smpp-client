//! The contacts-screen commands of spec §15.2 (deliverable L-009-07).
//!
//! Thin, like every command module (guide §8.3): deserialise, validate, call
//! the service, serialise. The column mapping, the E.164 validation, the
//! deduplication and the list algebra all live in `contacts`, so nothing here
//! knows what a septet or an indicatif is.
//!
//! # Why the DTOs are mirrored rather than reused
//!
//! `contacts::import::ColumnMapping` is already `Serialize`, and reusing it
//! would save the fifty lines below. It would also mean putting `specta` in a
//! business crate to export the TypeScript, and the shape the operator's form
//! posts would then be pinned to the shape the importer happens to use — a
//! rename inside `contacts` would silently become a breaking IPC change. The
//! mirror is the seam that makes that change fail to compile instead.
//!
//! # Why the bulk goes through a command and not through an event
//!
//! Same reason as the log screen: two hundred thousand contacts cannot cross
//! the bridge as notifications. The table fills itself with [`contacts_page`],
//! one page per scroll, and `import:progress` carries only counters.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt as _;

use contacts::import::{
    AttributeColumn, ColumnMapping, ColumnRef, Deduplication, HeaderMode, ImportOptions,
    ImportProfile, ImportReport, ImportSource, RejectedRow,
};
use contacts::lists::ListSelection;
use contacts::model::{Contact, ContactList, LineType, ListId, ProfileId};
use contacts::ports::ContactRepository as _;
use contacts::validation::{Region, ValidationOptions};
use persistence::ports::ContactDirectory as _;
use persistence::Cursor;
use smpp_core::time::Timestamp;

use crate::error::ErrorDto;
use crate::state::AppState;

/// Rows a page holds when the interface does not say.
const DEFAULT_PAGE: u32 = 100;

/// The largest page the backend will assemble.
///
/// A virtualised table asks for what its viewport needs; a caller asking for a
/// million rows is a bug or an abuse, and either way the WebView would not
/// survive the answer.
const MAX_PAGE: u32 = 1_000;

/// A statement, not a test: a default above the ceiling would be silently
/// clamped, so a caller reading `DEFAULT_PAGE` would be told one thing and
/// served another.
const _: () = {
    assert!(DEFAULT_PAGE > 0);
    assert!(DEFAULT_PAGE <= MAX_PAGE);
};

// ---------------------------------------------------------------------------
// Input DTOs
// ---------------------------------------------------------------------------

/// Where an import reads from.
///
/// A discriminated union rather than a path plus a flag: the sheet name only
/// means something for a workbook, and an optional field valid under one
/// variant only is a field somebody will set under the other.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum ImportSourceInput {
    /// A delimited text file.
    Csv {
        /// The file the operator chose, from the native dialog.
        path: String,
    },
    /// One sheet of a workbook.
    Xlsx {
        /// The file the operator chose.
        path: String,
        /// The sheet, or the first one when absent.
        sheet: Option<String>,
    },
}

impl From<ImportSourceInput> for ImportSource {
    fn from(source: ImportSourceInput) -> Self {
        match source {
            ImportSourceInput::Csv { path } => Self::Csv {
                path: PathBuf::from(path),
            },
            ImportSourceInput::Xlsx { path, sheet } => Self::Xlsx {
                path: PathBuf::from(path),
                sheet,
            },
        }
    }
}

/// How one column of the file is designated.
///
/// By name for a file with a header row, by zero-based position otherwise.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "by", content = "value")]
pub(crate) enum ColumnRefInput {
    /// By header name, matched case-insensitively after trimming.
    Name(String),
    /// By zero-based position.
    ///
    /// A `u32` and not a `usize`: `usize` exports to TypeScript as a number
    /// whose width depends on the host, and a column index that differs
    /// between a 32-bit and a 64-bit build is a contract that is not one.
    Index(u32),
}

impl From<ColumnRefInput> for ColumnRef {
    fn from(column: ColumnRefInput) -> Self {
        match column {
            ColumnRefInput::Name(name) => Self::Name(name),
            ColumnRefInput::Index(index) => {
                Self::Index(usize::try_from(index).unwrap_or(usize::MAX))
            }
        }
    }
}

/// One contact attribute and the column it comes from.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttributeColumnInput {
    /// The variable name a message template will use.
    pub(crate) variable: String,
    /// Where its value is read.
    pub(crate) column: ColumnRefInput,
}

impl From<AttributeColumnInput> for AttributeColumn {
    fn from(attribute: AttributeColumnInput) -> Self {
        Self {
            variable: attribute.variable,
            column: attribute.column.into(),
        }
    }
}

/// Which column means what (CA-009-09).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ColumnMappingInput {
    /// The recipient number. The one column an import cannot do without.
    pub(crate) msisdn: ColumnRefInput,
    /// A per-row country, which wins over the default region.
    pub(crate) country: Option<ColumnRefInput>,
    /// Everything else worth keeping.
    pub(crate) attributes: Vec<AttributeColumnInput>,
}

impl From<ColumnMappingInput> for ColumnMapping {
    fn from(mapping: ColumnMappingInput) -> Self {
        Self {
            msisdn: mapping.msisdn.into(),
            country: mapping.country.map(Into::into),
            attributes: mapping.attributes.into_iter().map(Into::into).collect(),
        }
    }
}

/// Whether the first row names the columns.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum HeaderModeInput {
    /// Decide by looking at the first row.
    #[default]
    Detect,
    /// The first row names the columns.
    Present,
    /// Every row is data.
    Absent,
}

impl From<HeaderModeInput> for HeaderMode {
    fn from(mode: HeaderModeInput) -> Self {
        match mode {
            HeaderModeInput::Detect => Self::Detect,
            HeaderModeInput::Present => Self::Present,
            HeaderModeInput::Absent => Self::Absent,
        }
    }
}

/// What to do with a row whose number was already seen.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DeduplicationInput {
    /// Keep the first occurrence and drop the rest.
    #[default]
    FirstWins,
    /// Keep the first occurrence and fold later attributes into it.
    ///
    /// Holds every distinct contact in memory until the import ends; the
    /// interface says so where the operator picks it.
    MergeAttributes,
}

impl From<DeduplicationInput> for Deduplication {
    fn from(strategy: DeduplicationInput) -> Self {
        match strategy {
            DeduplicationInput::FirstWins => Self::FirstWins,
            DeduplicationInput::MergeAttributes => Self::MergeAttributes,
        }
    }
}

/// Everything the operator chose in the import assistant.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportOptionsInput {
    /// Which column means what.
    pub(crate) mapping: ColumnMappingInput,
    /// Whether the first row names the columns.
    #[serde(default)]
    pub(crate) headers: HeaderModeInput,
    /// The region national forms are resolved against (CA-009-04).
    ///
    /// An ISO 3166-1 alpha-2 code. Rejected here if it is not one, rather
    /// than quietly ignored — an operator who typed `CIV` and got every row
    /// refused for "unknown indicatif" would look at the wrong thing.
    pub(crate) default_region: Option<String>,
    /// Keep mobile lines only (CA-009-06).
    #[serde(default)]
    pub(crate) mobiles_only: bool,
    /// What to do with a repeated number.
    #[serde(default)]
    pub(crate) deduplication: DeduplicationInput,
    /// A list every imported contact joins.
    pub(crate) list_id: Option<String>,
}

impl ImportOptionsInput {
    /// Projects the input onto the importer's options, rejecting what will not
    /// parse.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CONTACTS_INVALID_INPUT` naming `defaultRegion` or
    /// `listId`.
    fn parse(self) -> Result<ImportOptions, ErrorDto> {
        let default_region = match self.default_region.as_deref().filter(|raw| !raw.is_empty()) {
            None => None,
            Some(raw) => Some(
                Region::parse(raw)
                    .ok_or_else(|| ErrorDto::contacts_invalid_input("defaultRegion"))?,
            ),
        };

        let list = match self.list_id.as_deref().filter(|raw| !raw.is_empty()) {
            None => None,
            Some(raw) => {
                Some(ListId::parse(raw).ok_or_else(|| ErrorDto::contacts_invalid_input("listId"))?)
            }
        };

        Ok(ImportOptions {
            mapping: self.mapping.into(),
            headers: self.headers.into(),
            validation: ValidationOptions {
                default_region,
                mobiles_only: self.mobiles_only,
            },
            deduplication: self.deduplication.into(),
            list,
        })
    }
}

/// How the lists a query spans are combined (CA-009-12).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CombinationInput {
    /// Every contact of the store, whatever list it belongs to.
    #[default]
    Everything,
    /// A contact of at least one of the lists.
    Union,
    /// A contact of every one of the lists.
    Intersection,
}

/// Which contacts a query spans.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SelectionInput {
    /// How [`Self::lists`] are combined.
    #[serde(default)]
    pub(crate) combination: CombinationInput,
    /// The lists the query spans.
    #[serde(default)]
    pub(crate) lists: Vec<String>,
    /// Lists whose members are removed from the result, whatever else says.
    #[serde(default)]
    pub(crate) excluded: Vec<String>,
}

impl SelectionInput {
    /// Projects the input onto the store's selection.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CONTACTS_INVALID_INPUT` naming `lists` or
    /// `excluded` if an identifier is not one.
    fn parse(&self) -> Result<ListSelection, ErrorDto> {
        let lists = parse_lists(&self.lists, "lists")?;
        let excluded = parse_lists(&self.excluded, "excluded")?;

        let selection = match self.combination {
            CombinationInput::Everything => ListSelection::everything(),
            CombinationInput::Union => ListSelection::union(lists),
            CombinationInput::Intersection => ListSelection::intersection(lists),
        };

        Ok(selection.excluding(excluded))
    }
}

/// Reads a list of identifiers, naming the field that refused one.
fn parse_lists(raw: &[String], field: &'static str) -> Result<Vec<ListId>, ErrorDto> {
    raw.iter()
        .map(|value| ListId::parse(value).ok_or_else(|| ErrorDto::contacts_invalid_input(field)))
        .collect()
}

// ---------------------------------------------------------------------------
// Output DTOs
// ---------------------------------------------------------------------------

/// What an import produced (CA-009-08).
///
/// The counts are `u32`, saturating: the bridge carries JSON and
/// `JSON.stringify` throws on a `BigInt`. A file large enough to saturate one
/// would have exhausted the disk first.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReportDto {
    /// Non-blank rows examined. Equals `imported + rejected + duplicates`.
    pub(crate) total: u32,
    /// Contacts written.
    pub(crate) imported: u32,
    /// Rows refused, whatever the reason.
    pub(crate) rejected: u32,
    /// Rows whose number repeated an earlier one.
    pub(crate) duplicates: u32,
    /// Rows that held nothing, counted apart from [`Self::total`].
    pub(crate) blank: u32,
    /// Contacts the numbering plan reported as mobile.
    pub(crate) mobiles: u32,
    /// Contacts the plan reported as a landline.
    pub(crate) fixed_lines: u32,
    /// How many rows each rejection reason accounts for.
    ///
    /// Keyed by the stable reason code, which the interface translates.
    pub(crate) by_reason: Vec<ReasonCountDto>,
    /// The refused rows, for the correction export (CA-009-05).
    pub(crate) rejected_rows: Vec<RejectedRowDto>,
    /// Whether [`Self::rejected_rows`] was cut short.
    pub(crate) rejected_truncated: bool,
    /// Whether the operator stopped the import before the end of the file.
    pub(crate) cancelled: bool,
}

/// How many rows one rejection reason accounts for.
///
/// A list of pairs rather than a map: `serde_json` would serialise the map
/// fine, but the order would be the map's, and the interface renders these in
/// the order the backend counted them.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReasonCountDto {
    /// The stable reason code.
    pub(crate) reason: String,
    /// How many rows it refused.
    pub(crate) count: u32,
}

/// One refused row, with everything needed to fix it.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RejectedRowDto {
    /// Line in the operator's file, as their editor shows it.
    ///
    /// A `u32`: a file with more than four billion lines is not one this
    /// application is going to report on.
    pub(crate) line: u32,
    /// Why it was refused — the interface translates the code.
    pub(crate) reason: String,
    /// The cell as it was in the file.
    ///
    /// The one operator value that crosses back, and deliberately: it goes to
    /// the person who supplied it, so the exported list is correctable. It is
    /// never logged and never part of an error message.
    pub(crate) value: String,
}

impl From<RejectedRow> for RejectedRowDto {
    fn from(row: RejectedRow) -> Self {
        Self {
            line: narrow(row.line),
            reason: row.reason.code().to_owned(),
            value: row.value,
        }
    }
}

impl From<ImportReport> for ImportReportDto {
    fn from(report: ImportReport) -> Self {
        Self {
            total: narrow(report.total),
            imported: narrow(report.imported),
            rejected: narrow(report.rejected),
            duplicates: narrow(report.duplicates),
            blank: narrow(report.blank),
            mobiles: narrow(report.mobiles),
            fixed_lines: narrow(report.fixed_lines),
            by_reason: report
                .by_reason
                .into_iter()
                .map(|(reason, count)| ReasonCountDto {
                    reason: reason.to_owned(),
                    count: narrow(count),
                })
                .collect(),
            rejected_rows: report
                .rejected_rows
                .into_iter()
                .map(RejectedRowDto::from)
                .collect(),
            rejected_truncated: report.rejected_truncated,
            cancelled: report.cancelled,
        }
    }
}

/// One row of the contacts table.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContactRowDto {
    /// Primary key, and the row's React key.
    pub(crate) contact_id: String,
    /// The number, normalised to E.164 (CA-009-04).
    pub(crate) msisdn: String,
    /// The country the number resolved to.
    pub(crate) country: Option<String>,
    /// Whether the number passed validation.
    pub(crate) valid: bool,
    /// `MOBILE`, `FIXED_LINE`, … as the numbering plan reported it.
    pub(crate) line_type: Option<String>,
    /// The mapped attributes, as a JSON object.
    pub(crate) attributes: Option<String>,
    /// `import_csv`, `import_xlsx`, or whatever created the contact.
    pub(crate) source: Option<String>,
    /// When the contact was written.
    pub(crate) created_at: String,
}

impl From<Contact> for ContactRowDto {
    fn from(contact: Contact) -> Self {
        Self {
            contact_id: contact.contact_id.to_string(),
            msisdn: contact.msisdn.as_str().to_owned(),
            country: contact.country,
            valid: contact.valid,
            line_type: contact
                .line_type
                .map(|kind| LineType::code(kind).to_owned()),
            attributes: contact.attributes,
            source: contact.source,
            created_at: contact.created_at.to_storage(),
        }
    }
}

/// One page of the contacts table.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContactPageDto {
    /// The rows, in insertion order.
    pub(crate) rows: Vec<ContactRowDto>,
    /// Cursor to pass back for the next page, or `null` at the end.
    ///
    /// A **string**, and opaque: the interface hands it back untouched. A
    /// cursor is a SQLite `rowid`, an `i64`, and `JSON.stringify` throws on a
    /// `BigInt`.
    pub(crate) next: Option<String>,
    /// How many contacts the selection holds in total.
    ///
    /// What sizes the virtualised scrollbar. Counts the selection and **not**
    /// the search: the count is what the scrollbar is sized against before the
    /// operator has typed anything, and recomputing it per keystroke would put
    /// a full scan behind every character.
    pub(crate) total: u32,
}

/// One contact list.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContactListDto {
    /// Primary key.
    pub(crate) list_id: String,
    /// Name shown in the interface.
    pub(crate) name: String,
    /// When the list was created.
    pub(crate) created_at: String,
}

impl From<ContactList> for ContactListDto {
    fn from(list: ContactList) -> Self {
        Self {
            list_id: list.list_id.to_string(),
            name: list.name,
            created_at: list.created_at.to_storage(),
        }
    }
}

/// A saved column-mapping profile (CA-009-09).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportProfileDto {
    /// Primary key. Absent when the interface is saving a new profile.
    pub(crate) profile_id: Option<String>,
    /// The name the operator gave it.
    pub(crate) name: String,
    /// The mapping it replays.
    pub(crate) mapping: ColumnMappingInput,
    /// When it was saved. Ignored on the way in.
    pub(crate) created_at: Option<String>,
}

impl From<ImportProfile> for ImportProfileDto {
    fn from(profile: ImportProfile) -> Self {
        Self {
            profile_id: Some(profile.profile_id.to_string()),
            name: profile.name,
            mapping: ColumnMappingInput::from(profile.mapping),
            created_at: Some(profile.created_at.to_storage()),
        }
    }
}

impl From<ColumnMapping> for ColumnMappingInput {
    fn from(mapping: ColumnMapping) -> Self {
        Self {
            msisdn: ColumnRefInput::from(mapping.msisdn),
            country: mapping.country.map(ColumnRefInput::from),
            attributes: mapping
                .attributes
                .into_iter()
                .map(|attribute| AttributeColumnInput {
                    variable: attribute.variable,
                    column: ColumnRefInput::from(attribute.column),
                })
                .collect(),
        }
    }
}

impl From<ColumnRef> for ColumnRefInput {
    fn from(column: ColumnRef) -> Self {
        match column {
            ColumnRef::Name(name) => Self::Name(name),
            ColumnRef::Index(index) => Self::Index(narrow(index)),
        }
    }
}

/// Saturating narrowing for a count that crosses the bridge.
///
/// The workspace forbids truncating casts, and rightly: a silent wrap here
/// would report an import of four billion rows as an import of three.
fn narrow<T>(value: T) -> u32
where
    u32: TryFrom<T>,
{
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Reads a pagination cursor.
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
            .map_err(|_| ErrorDto::contacts_invalid_input("cursor")),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Opens the native picker and returns the file the operator chose.
///
/// # Why the picker lives in the backend
///
/// It could run in the WebView — the plugin has a JavaScript side — and that
/// is what the first cut did. But then `contacts_import` has to take whatever
/// path it is handed, and CLAUDE.md §3 says the WebView is untrusted: injected
/// script could pass `~/.ssh/id_rsa` and read the file back out of the
/// rejected rows, which carry the offending value verbatim. Opening the picker
/// here is what lets the backend **remember** which files the operator pointed
/// at, so `contacts_import` can refuse everything else. The window is granted
/// no `dialog:` permission at all as a result.
///
/// Returns `None` when the operator dismissed the picker, which is an outcome
/// and not a failure.
///
/// # Errors
///
/// [`ErrorDto`] with `CONTACTS_INVALID_INPUT` if the picked entry is not a
/// path this platform can open — a content URI on a mobile target.
#[tauri::command]
#[specta::specta]
pub(crate) async fn contacts_pick_file(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, ErrorDto> {
    let (sender, receiver) = tokio::sync::oneshot::channel();

    app.dialog()
        .file()
        .add_filter("contacts", &["csv", "txt", "xlsx"])
        .pick_file(move |chosen| {
            // The receiver is gone only if the command was cancelled; there is
            // nothing to report to and nothing to clean up.
            drop(sender.send(chosen));
        });

    let Ok(Some(chosen)) = receiver.await else {
        // A dropped sender means the dialog went away without answering, which
        // the operator cannot tell apart from dismissing it.
        return Ok(None);
    };

    let path = chosen
        .into_path()
        .map_err(|_| ErrorDto::contacts_invalid_input("path"))?;

    let rendered = path.to_string_lossy().into_owned();

    state.contacts().remember_picked(path).await;

    Ok(Some(rendered))
}

/// Reads a file and writes the contacts it holds.
///
/// Progress arrives on `import:progress` (CA-009-11); this returns the final
/// report, which is authoritative even when the import was cancelled.
///
/// # Errors
///
/// [`ErrorDto`] with `CONTACTS_FILE_NOT_PICKED` if the file did not come from
/// [`contacts_pick_file`], `CONTACTS_IMPORT_BUSY` if one is already running,
/// `CONTACTS_INVALID_INPUT` if an option will not parse,
/// `CONTACTS_IMPORT_REJECTED` if the file cannot be read or mapped, or a
/// storage code if the contacts cannot be written.
#[tauri::command]
#[specta::specta]
pub(crate) async fn contacts_import(
    app: AppHandle,
    state: State<'_, AppState>,
    source: ImportSourceInput,
    options: ImportOptionsInput,
) -> Result<ImportReportDto, ErrorDto> {
    let options = options.parse()?;

    state
        .contacts()
        .import(&app, source.into(), options)
        .await
        .map(ImportReportDto::from)
}

/// Asks the running import to stop (CA-009-10).
///
/// Returns whether there was one. Not an error when there is none: an operator
/// who clicks cancel on an import that has just finished has got what they
/// wanted.
///
/// # Errors
///
/// Infallible today; the `Result` is what the bridge requires of an `async`
/// command.
#[tauri::command]
#[specta::specta]
pub(crate) async fn contacts_cancel_import(state: State<'_, AppState>) -> Result<bool, ErrorDto> {
    Ok(state.contacts().cancel().await)
}

/// Reads one page of the contacts table.
///
/// # Errors
///
/// [`ErrorDto`] with `CONTACTS_INVALID_INPUT` for a malformed identifier or
/// cursor, `CONTACTS_STORAGE` if the store will not answer.
#[tauri::command]
#[specta::specta]
pub(crate) async fn contacts_page(
    state: State<'_, AppState>,
    selection: Option<SelectionInput>,
    search: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<ContactPageDto, ErrorDto> {
    let selection = selection.unwrap_or_default().parse()?;
    let cursor = parse_cursor(cursor.as_deref())?;
    let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let needle = search.as_deref().filter(|raw| !raw.is_empty());

    let repository = state.contacts().repository();

    let page = repository
        .page_contacts(&selection, needle, cursor, limit)
        .await
        .map_err(|error| ErrorDto::contacts_storage(&error))?;

    let total = repository
        .count_contacts(&selection)
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    Ok(ContactPageDto {
        rows: page.items.into_iter().map(ContactRowDto::from).collect(),
        next: page.next.map(|position| position.into_raw().to_string()),
        total: narrow(total),
    })
}

/// Every contact list, oldest first (CA-009-12).
///
/// Unpaginated, and deliberately: a list is created by hand or by one import,
/// so the table holds units to hundreds. The contacts *inside* them are what
/// needs paging, and that is [`contacts_page`]'s job.
///
/// # Errors
///
/// [`ErrorDto`] with `CONTACTS_STORAGE` if the store will not answer.
#[tauri::command]
#[specta::specta]
pub(crate) async fn contacts_lists(
    state: State<'_, AppState>,
) -> Result<Vec<ContactListDto>, ErrorDto> {
    state
        .contacts()
        .repository()
        .list_contact_lists()
        .await
        .map(|lists| lists.into_iter().map(ContactListDto::from).collect())
        .map_err(|error| ErrorDto::from(&error))
}

/// Every saved mapping profile, oldest first (CA-009-09).
///
/// # Errors
///
/// [`ErrorDto`] with `CONTACTS_STORAGE` if the store will not answer.
#[tauri::command]
#[specta::specta]
pub(crate) async fn contacts_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<ImportProfileDto>, ErrorDto> {
    state
        .contacts()
        .profiles()
        .await
        .map(|profiles| profiles.into_iter().map(ImportProfileDto::from).collect())
}

/// Saves a mapping profile, replacing one of the same identifier.
///
/// Returns the identifier, which is the one the interface sent or a fresh one
/// when it sent none — that is how a form that saved a new profile learns what
/// to send next time.
///
/// # Errors
///
/// [`ErrorDto`] with `CONTACTS_INVALID_INPUT` for a malformed identifier,
/// `CONTACTS_STORAGE` if the write fails.
#[tauri::command]
#[specta::specta]
pub(crate) async fn contacts_save_profile(
    state: State<'_, AppState>,
    profile: ImportProfileDto,
) -> Result<String, ErrorDto> {
    let profile_id = match profile.profile_id.as_deref().filter(|raw| !raw.is_empty()) {
        None => ProfileId::new(),
        Some(raw) => {
            ProfileId::parse(raw).ok_or_else(|| ErrorDto::contacts_invalid_input("profileId"))?
        }
    };

    let stored = ImportProfile {
        profile_id,
        name: profile.name,
        mapping: profile.mapping.into(),
        created_at: Timestamp::now(),
    };

    state.contacts().save_profile(&stored).await?;

    Ok(profile_id.to_string())
}
