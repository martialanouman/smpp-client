//! The send commands of spec §15.2 (deliverable L-006-05).
//!
//! Two commands and one event, all four moves of guide §8.3 and nothing else:
//! deserialise, hand the input to `messaging`'s validating constructors, call
//! the service, serialise its report, emit `message:update`.
//!
//! # Where the validation actually happens
//!
//! Not here. Every field of a send crosses through a type of `messaging` whose
//! constructor is the validation — `Destination::parse`, `SourceAddress::parse`,
//! `CustomTlv::new`, `SubmitOptions` — so this file has no rule of its own to
//! get out of step with the crate. What it does own is the **projection** of
//! each rejection onto a stable `ErrorCode`, which is a boundary concern.
//!
//! The WebView is untrusted (CLAUDE.md §3): a hand-crafted `invoke` carrying a
//! forty-character sender ID or a recipient full of letters takes exactly the
//! same path as the form.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, State};

use messaging::addressing::{AddressError, Destination, SourceAddress};
use messaging::encoding::{Encoding, EncodingChoice};
use messaging::segmentation::{SegmentationMode, SegmentationOptions};
use messaging::sender::{SegmentOutcome, SendReport, SendRequest};
use messaging::submit::{CustomTlv, SubmitOptions};
use messaging::MessagingError;
use smpp_core::status_codes;
use smpp_core::types::SessionId;
use smpp_core::values::{
    IntermediateNotification, MCDeliveryReceipt, Npi, PriorityFlag, RegisteredDelivery,
    ReplaceIfPresentFlag, SmeOriginatedAcknowledgement, Ton,
};

use crate::error::ErrorDto;
use crate::state::AppState;

/// Type of number, as spec §7.4 tabulates it.
///
/// A closed enum rather than the bare octet CLAUDE.md §4 forbids. The seven
/// values are the whole standard table; a message centre that wanted an eighth
/// would need a milestone, not a free-form field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TonDto {
    /// `0` — unknown.
    Unknown,
    /// `1` — international (E.164). The default, and the safe one.
    #[default]
    International,
    /// `2` — national.
    National,
    /// `3` — network specific, typically a short code.
    NetworkSpecific,
    /// `4` — subscriber number.
    SubscriberNumber,
    /// `5` — alphanumeric. Forced on a sender ID.
    Alphanumeric,
    /// `6` — abbreviated.
    Abbreviated,
}

impl From<TonDto> for Ton {
    fn from(value: TonDto) -> Self {
        match value {
            TonDto::Unknown => Self::Unknown,
            TonDto::International => Self::International,
            TonDto::National => Self::National,
            TonDto::NetworkSpecific => Self::NetworkSpecific,
            TonDto::SubscriberNumber => Self::SubscriberNumber,
            TonDto::Alphanumeric => Self::Alphanumeric,
            TonDto::Abbreviated => Self::Abbreviated,
        }
    }
}

/// Numbering plan indicator, as spec §7.4 tabulates it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum NpiDto {
    /// `0` — unknown.
    Unknown,
    /// `1` — ISDN / E.164. The default, and the safe one.
    #[default]
    Isdn,
    /// `3` — data (X.121).
    Data,
    /// `4` — telex (F.69).
    Telex,
    /// `6` — land mobile (E.212).
    LandMobile,
    /// `8` — national.
    National,
    /// `9` — private.
    Private,
}

impl From<NpiDto> for Npi {
    fn from(value: NpiDto) -> Self {
        match value {
            NpiDto::Unknown => Self::Unknown,
            NpiDto::Isdn => Self::Isdn,
            NpiDto::Data => Self::Data,
            NpiDto::Telex => Self::Telex,
            NpiDto::LandMobile => Self::LandMobile,
            NpiDto::National => Self::National,
            NpiDto::Private => Self::Private,
        }
    }
}

/// The `data_coding` selector, as the operator sees it (spec §7.5).
///
/// The DCS is **derived** from this rather than typed directly: an operator who
/// picked `data_coding = 8` and a text of pure ASCII would send UCS2 for
/// nothing, and one who picked `0` for a text full of emoji would send
/// mojibake. Choosing the *alphabet* and letting the encoder settle the octet
/// is the only combination that cannot contradict itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum EncodingDto {
    /// GSM 7-bit when the text allows it, UCS2 otherwise. The default.
    #[default]
    Automatic,
    /// Force GSM 7-bit; a text it cannot write is refused.
    Gsm7Bit,
    /// Force ISO-8859-1.
    Latin1,
    /// Force UCS2.
    Ucs2,
}

impl From<EncodingDto> for EncodingChoice {
    fn from(value: EncodingDto) -> Self {
        match value {
            EncodingDto::Automatic => Self::Automatic,
            EncodingDto::Gsm7Bit => Self::Forced(Encoding::Gsm7Bit),
            EncodingDto::Latin1 => Self::Forced(Encoding::Latin1),
            EncodingDto::Ucs2 => Self::Forced(Encoding::Ucs2),
        }
    }
}

impl From<Encoding> for EncodingDto {
    fn from(value: Encoding) -> Self {
        match value {
            Encoding::Gsm7Bit => Self::Gsm7Bit,
            Encoding::Latin1 => Self::Latin1,
            Encoding::Ucs2 => Self::Ucs2,
        }
    }
}

/// How the parts of a long message announce that they belong together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SegmentationModeDto {
    /// Concatenation UDH inside `short_message`. The portable default.
    #[default]
    Udh,
    /// `sar_*` TLVs, and a body with no header.
    Sar,
    /// No splitting: the whole body in the `message_payload` TLV.
    MessagePayload,
}

impl From<SegmentationModeDto> for SegmentationMode {
    fn from(value: SegmentationModeDto) -> Self {
        match value {
            SegmentationModeDto::Udh => Self::Udh,
            SegmentationModeDto::Sar => Self::Sar,
            SegmentationModeDto::MessagePayload => Self::MessagePayload,
        }
    }
}

/// What `registered_delivery` asks the message centre for.
///
/// The three values an operator has a reason to choose. The rest of the octet
/// — SME acknowledgements, intermediate notifications — is refused by most
/// message centres with `ESME_RINVREGDLVFLG`, so offering it would be offering
/// a rejection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RegisteredDeliveryDto {
    /// `0` — no delivery receipt.
    None,
    /// `1` — a receipt on the final outcome, success or failure. The default
    /// of spec §23.3, and the one milestone 008 correlates.
    #[default]
    OnAnyOutcome,
    /// `2` — a receipt on failure only.
    OnFailure,
}

impl From<RegisteredDeliveryDto> for RegisteredDelivery {
    fn from(value: RegisteredDeliveryDto) -> Self {
        let receipt = match value {
            RegisteredDeliveryDto::None => MCDeliveryReceipt::NoMcDeliveryReceiptRequested,
            RegisteredDeliveryDto::OnAnyOutcome => {
                MCDeliveryReceipt::McDeliveryReceiptRequestedWhereFinalDeliveryOutcomeIsSuccessOrFailure
            }
            RegisteredDeliveryDto::OnFailure => {
                MCDeliveryReceipt::McDeliveryReceiptRequestedWhereFinalDeliveryOutcomeIsFailure
            }
        };

        Self::new(
            receipt,
            SmeOriginatedAcknowledgement::NoReceiptSmeAcknowledgementRequested,
            IntermediateNotification::NoIntermediaryNotificationRequested,
            0,
        )
    }
}

/// One custom optional parameter the operator typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TlvDto {
    /// The tag, as a 16-bit value.
    pub(crate) tag: u16,
    /// The value, hexadecimal, without separators — `DEADBEEF`.
    ///
    /// Hexadecimal rather than base64 because that is how an operator reads a
    /// TLV in their message centre's documentation, and rather than a byte
    /// array because JSON turns one into a list of numbers nobody can check by
    /// eye.
    pub(crate) value_hex: String,
}

impl TlvDto {
    /// Parses the tag and the hexadecimal value.
    fn parse(&self) -> Result<CustomTlv, ErrorDto> {
        let trimmed: String = self
            .value_hex
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();

        if !trimmed.len().is_multiple_of(2) {
            return Err(ErrorDto::message_invalid_tlv());
        }

        let mut octets = Vec::with_capacity(trimmed.len() / 2);

        for index in (0..trimmed.len()).step_by(2) {
            let pair = trimmed
                .get(index..index + 2)
                .ok_or_else(ErrorDto::message_invalid_tlv)?;

            octets.push(u8::from_str_radix(pair, 16).map_err(|_| ErrorDto::message_invalid_tlv())?);
        }

        CustomTlv::new(self.tag, octets).map_err(|error| ErrorDto::from(&error))
    }
}

/// Input of [`message_send`] — every field of spec §7.3 the operator controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageSendInput {
    /// Which live session to send on. Chosen explicitly; automatic routing is
    /// milestone 011.
    pub(crate) session_id: String,
    /// The recipient, in any form `Msisdn` accepts.
    pub(crate) destination: String,
    /// `dest_addr_ton`.
    pub(crate) dest_ton: TonDto,
    /// `dest_addr_npi`.
    pub(crate) dest_npi: NpiDto,
    /// The sender, or `null` to let the message centre choose one.
    pub(crate) source: Option<String>,
    /// `source_addr_ton`, or `null` to derive it from the address.
    pub(crate) source_ton: Option<TonDto>,
    /// `source_addr_npi`, or `null` to derive it from the address.
    pub(crate) source_npi: Option<NpiDto>,
    /// The message body.
    pub(crate) text: String,
    /// Which alphabet to write it in.
    pub(crate) encoding: EncodingDto,
    /// How a long message announces its parts.
    pub(crate) segmentation_mode: SegmentationModeDto,
    /// `service_type`; empty for the message centre's default.
    pub(crate) service_type: String,
    /// `protocol_id`.
    pub(crate) protocol_id: u8,
    /// `priority_flag`, `0` to `3`.
    pub(crate) priority_flag: u8,
    /// `schedule_delivery_time`; empty for immediate delivery.
    pub(crate) schedule_delivery_time: String,
    /// `validity_period`; empty for the message centre's default.
    pub(crate) validity_period: String,
    /// What to ask for in `registered_delivery`.
    pub(crate) registered_delivery: RegisteredDeliveryDto,
    /// `replace_if_present_flag`.
    pub(crate) replace_if_present: bool,
    /// `sm_default_msg_id`.
    pub(crate) sm_default_msg_id: u8,
    /// Custom optional parameters.
    pub(crate) tlvs: Vec<TlvDto>,
}

impl MessageSendInput {
    /// Rebuilds the domain request, validating every field.
    fn parse(&self) -> Result<SendRequest, ErrorDto> {
        let destination = Destination::parse_with(
            &self.destination,
            self.dest_ton.into(),
            self.dest_npi.into(),
        )
        .map_err(|error| ErrorDto::from(&error))?;

        let mut submit = SubmitOptions::to(destination);

        if let Some(raw) = self.source.as_deref().filter(|raw| !raw.trim().is_empty()) {
            // TON and NPI are derived from the address, then **each** is
            // replaced if the operator chose one. They are two independent
            // selectors in the form, so honouring an override only when both
            // are set would silently discard a choice that was made — which is
            // exactly what CA-006-06 forbids.
            let mut source = SourceAddress::parse(raw).map_err(|error| ErrorDto::from(&error))?;

            if let Some(ton) = self.source_ton {
                source = source.with_ton(ton.into());
            }

            if let Some(npi) = self.source_npi {
                source = source.with_npi(npi.into());
            }

            submit = submit.with_source(source);
        }

        submit.service_type = self.service_type.clone();
        submit.protocol_id = self.protocol_id;
        submit.priority_flag = PriorityFlag::from(self.priority_flag);
        submit.schedule_delivery_time = self.schedule_delivery_time.clone();
        submit.validity_period = self.validity_period.clone();
        submit.registered_delivery = self.registered_delivery.into();
        submit.replace_if_present_flag = if self.replace_if_present {
            ReplaceIfPresentFlag::Replace
        } else {
            ReplaceIfPresentFlag::DoNotReplace
        };
        submit.sm_default_msg_id = self.sm_default_msg_id;
        submit.tlvs = self
            .tlvs
            .iter()
            .map(TlvDto::parse)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(SendRequest::new(self.text.clone(), submit)
            .with_encoding(self.encoding.into())
            .with_mode(self.segmentation_mode.into()))
    }
}

/// What became of one segment, as the interface shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SegmentOutcomeDto {
    /// This segment's index, from 1.
    pub(crate) sequence_number: u32,
    /// `answered`, `unanswered` or `notAttempted`.
    pub(crate) outcome: String,
    /// The raw `command_status`, when the message centre answered.
    pub(crate) command_status: Option<u32>,
    /// Its symbolic name, `ESME_RTHROTTLED`.
    pub(crate) status_symbol: Option<String>,
    /// The identifier the message centre assigned to this segment.
    pub(crate) smsc_message_id: Option<String>,
}

/// What one send produced (spec §15.2).
///
/// A **value**, not an error, even for a rejected message: ENF-UTI-02 requires
/// the operator to read the message centre's own status, and a thrown error
/// would replace it with one of ours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageSendResultDto {
    /// The write-ahead key, which is how the interface follows the message.
    pub(crate) client_message_id: String,
    /// The session it went out on.
    pub(crate) session_id: String,
    /// `QUEUED`, `SENT`, `ACCEPTED`, `FAILED` — the names of spec §14.3.
    pub(crate) state: String,
    /// Segments the message was split into.
    pub(crate) segments: u32,
    /// The identifier of the first segment.
    pub(crate) smsc_message_id: Option<String>,
    /// The raw `command_status`, when the message centre answered.
    pub(crate) command_status: Option<u32>,
    /// Its symbolic name — `ESME_RINVDSTADR`.
    pub(crate) status_symbol: Option<String>,
    /// Its label, in the application's default language.
    ///
    /// Sent from the backend rather than translated in the interface because
    /// the table is protocol data indexed by an octet (milestone 003), not a
    /// user-interface catalogue: a code the table does not list has no key to
    /// look up, and the interface must still show something.
    pub(crate) status_label: Option<String>,
    /// Whether the status is one the message centre reserves for its own
    /// vendor range, which only its documentation explains.
    pub(crate) status_is_vendor_specific: bool,
    /// Whether sending the same message again could succeed.
    pub(crate) retryable: bool,
    /// Whether the journal recorded the outcome.
    ///
    /// `false` means the message **was** submitted and answered, but its
    /// transitions could not be written: the row is still `QUEUED`. The
    /// interface has to say so, because "sent and unrecorded" is the one
    /// state where doing nothing is right and resending is wrong.
    pub(crate) journalled: bool,
    /// One entry per segment.
    pub(crate) outcomes: Vec<SegmentOutcomeDto>,
}

impl From<&SendReport> for MessageSendResultDto {
    fn from(report: &SendReport) -> Self {
        let described = report.command_status.and_then(status_codes::describe);

        Self {
            client_message_id: report.client_message_id.to_string(),
            session_id: report.session_id.to_string(),
            state: report.state.as_str().to_owned(),
            segments: report.segments,
            smsc_message_id: report.smsc_message_id.clone(),
            command_status: report.command_status.map(u32::from),
            status_symbol: described.map(|entry| entry.symbol.to_owned()),
            status_label: described.map(|entry| entry.label_fr.to_owned()),
            status_is_vendor_specific: report
                .command_status
                .map(u32::from)
                .is_some_and(status_codes::is_vendor_specific),
            retryable: report.retryable,
            journalled: report.journalled,
            outcomes: report
                .outcomes
                .iter()
                .enumerate()
                .map(|(index, outcome)| segment_dto(index, outcome))
                .collect(),
        }
    }
}

/// Projects one segment outcome.
fn segment_dto(index: usize, outcome: &SegmentOutcome) -> SegmentOutcomeDto {
    let sequence_number = u32::try_from(index + 1).unwrap_or(u32::MAX);

    match outcome {
        SegmentOutcome::Answered {
            status,
            smsc_message_id,
        } => SegmentOutcomeDto {
            sequence_number,
            outcome: "answered".to_owned(),
            command_status: Some(u32::from(*status)),
            status_symbol: status_codes::describe(*status).map(|entry| entry.symbol.to_owned()),
            smsc_message_id: smsc_message_id.clone(),
        },
        SegmentOutcome::Unanswered { .. } => SegmentOutcomeDto {
            sequence_number,
            outcome: "unanswered".to_owned(),
            command_status: None,
            status_symbol: None,
            smsc_message_id: None,
        },
        SegmentOutcome::NotAttempted => SegmentOutcomeDto {
            sequence_number,
            outcome: "notAttempted".to_owned(),
            command_status: None,
            status_symbol: None,
            smsc_message_id: None,
        },
    }
}

/// Input of [`message_preview`] — what the editor is showing right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessagePreviewInput {
    /// The text as typed.
    pub(crate) text: String,
    /// Which alphabet the operator chose.
    pub(crate) encoding: EncodingDto,
    /// How a long message announces its parts.
    pub(crate) segmentation_mode: SegmentationModeDto,
    /// The session whose GSM conventions apply, or `null` for the defaults.
    pub(crate) session_id: Option<String>,
}

/// What the editor's counter shows (CA-006-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessagePreviewDto {
    /// The encoding that will be used — detected, or the forced one.
    pub(crate) encoding: EncodingDto,
    /// The `data_coding` octet that will go on the wire.
    pub(crate) data_coding: u8,
    /// Characters typed, as Unicode scalar values.
    pub(crate) characters: u32,
    /// Encoding units used — septets, UTF-16 code units or octets.
    pub(crate) units_used: u32,
    /// Units still free in the segment being filled.
    pub(crate) units_remaining: u32,
    /// Segments the message will be sent as.
    pub(crate) segments: u32,
}

/// Sends one message (EF-MSG-01, EF-MSG-05, EF-MSG-06).
///
/// Returns when the message centre has answered every segment, or refused one.
/// The interface follows the intermediate states through `message:update`.
///
/// # Errors
///
/// * `MESSAGE_INVALID_DESTINATION` / `MESSAGE_INVALID_SOURCE` — an address was
///   refused, **before** anything was persisted or sent;
/// * `MESSAGE_INVALID_FIELD` — a field of spec §7.3 does not fit its slot;
/// * `MESSAGE_INVALID_TLV` — a custom TLV is not readable hexadecimal;
/// * `MESSAGE_ENCODING` — the text cannot be written under the chosen
///   encoding, or needs more than 255 segments;
/// * `MESSAGE_SESSION_NOT_BOUND` — no live session carries that identifier;
/// * `MESSAGE_DUPLICATE` — a message already exists under that
///   `client_message_id`, which is the guard that makes a replay idempotent;
/// * `MESSAGE_STORAGE` — the journal refused the **write-ahead insert**, in
///   which case nothing was sent.
///
/// Two outcomes that are deliberately **not** errors:
///
/// * a message the centre **rejected** comes back as a result whose `state` is
///   `FAILED`, carrying the raw `command_status` (ENF-UTI-02);
/// * a journal failure *after* the send comes back as a successful result with
///   `journalled: false`. Reporting it as `MESSAGE_STORAGE` would tell the
///   operator nothing was sent, and the message would be sent twice.
#[tauri::command]
#[specta::specta]
pub(crate) async fn message_send(
    app: AppHandle,
    state: State<'_, AppState>,
    input: MessageSendInput,
) -> Result<MessageSendResultDto, ErrorDto> {
    let session_id =
        SessionId::parse(&input.session_id).map_err(|_| ErrorDto::session_invalid_id())?;
    let request = input.parse()?;

    let handle = state
        .sessions()
        .registry()
        .handle(session_id)
        .await
        .ok_or_else(ErrorDto::message_session_not_bound)?;

    let report = state
        .messages()
        .send(&app, &handle, &request)
        .await
        .map_err(|error| ErrorDto::from(&error))?;

    Ok(MessageSendResultDto::from(&report))
}

/// The live counter of the message editor (CA-006-09).
///
/// Called on every keystroke, so it does no I/O and touches neither the
/// journal nor the socket: it is the same `preview` the segmenter uses to
/// decide where the cuts fall, which is what makes the counter and the
/// segments agree by construction rather than by coincidence.
///
/// # Errors
///
/// * `SESSION_INVALID_ID` — the session identifier is malformed;
/// * `MESSAGE_ENCODING` — a forced encoding cannot write the text.
#[tauri::command]
#[specta::specta]
pub(crate) async fn message_preview(
    state: State<'_, AppState>,
    input: MessagePreviewInput,
) -> Result<MessagePreviewDto, ErrorDto> {
    let mut options = SegmentationOptions::default()
        .with_encoding(input.encoding.into())
        .with_mode(input.segmentation_mode.into());

    // The GSM conventions belong to the message centre (ADR 0008, ADR 0009),
    // so the counter has to read them from the same session the message will
    // go out on — otherwise a `Latin1` session would be previewed under GSM
    // 03.38 positions and the two would disagree on `€`.
    if let Some(raw) = input.session_id.as_deref() {
        let session_id = SessionId::parse(raw).map_err(|_| ErrorDto::session_invalid_id())?;

        if let Some(handle) = state.sessions().registry().handle(session_id).await {
            options = options
                .with_gsm_packing(handle.gsm7_packing())
                .with_gsm_charset(handle.gsm7_charset());
        }
    }

    let preview = messaging::encoding::preview::preview(&input.text, &options)
        .map_err(|error| ErrorDto::message_encoding(&error.to_string()))?;

    Ok(MessagePreviewDto {
        encoding: preview.encoding().into(),
        data_coding: u8::from(preview.encoding().data_coding()),
        characters: saturating(preview.characters()),
        units_used: saturating(preview.units_used()),
        units_remaining: saturating(preview.units_remaining_in_segment()),
        segments: saturating(preview.segments()),
    })
}

/// A count as the contract carries it, saturating rather than wrapping.
fn saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Projects a messaging failure onto the IPC contract.
impl From<&MessagingError> for ErrorDto {
    fn from(error: &MessagingError) -> Self {
        match error {
            MessagingError::Address(inner) => Self::from(inner),
            MessagingError::Encoding(inner) => Self::message_encoding(&inner.to_string()),
            MessagingError::Submit(inner) => Self::from(inner),
            MessagingError::Store(messaging::ports::MessageStoreError::Conflict) => {
                Self::message_duplicate()
            }
            MessagingError::Store(_) => Self::message_storage(),
            // `MessagingError` is `#[non_exhaustive]`, so a wildcard is
            // required. It reports the most conservative code rather than
            // guessing.
            _ => Self::message_encoding(&error.to_string()),
        }
    }
}

impl From<&AddressError> for ErrorDto {
    fn from(error: &AddressError) -> Self {
        match error {
            AddressError::InvalidDestination | AddressError::MissingDestination => {
                Self::message_invalid_destination(error)
            }
            _ => Self::message_invalid_source(error),
        }
    }
}

impl From<&messaging::submit::SubmitBuildError> for ErrorDto {
    fn from(error: &messaging::submit::SubmitBuildError) -> Self {
        use messaging::submit::SubmitBuildError as Failure;

        match error {
            Failure::Address(inner) => Self::from(inner),
            Failure::TlvValueTooLong { .. } => Self::message_invalid_tlv(),
            _ => Self::message_invalid_field(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use messaging::message::MessageState;
    use smpp_core::values::CommandStatus;

    fn an_input() -> MessageSendInput {
        MessageSendInput {
            session_id: SessionId::new().to_string(),
            destination: "+2250102030405".to_owned(),
            dest_ton: TonDto::International,
            dest_npi: NpiDto::Isdn,
            source: Some("ShinobiSMS".to_owned()),
            source_ton: None,
            source_npi: None,
            text: "Bonjour".to_owned(),
            encoding: EncodingDto::Automatic,
            segmentation_mode: SegmentationModeDto::Udh,
            service_type: String::new(),
            protocol_id: 0,
            priority_flag: 0,
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: RegisteredDeliveryDto::OnAnyOutcome,
            replace_if_present: false,
            sm_default_msg_id: 0,
            tlvs: Vec::new(),
        }
    }

    #[test]
    fn a_well_formed_input_parses_into_a_request() {
        let request = an_input().parse().expect("the fixture is valid");

        assert_eq!(request.text, "Bonjour");
        assert_eq!(request.attempt, 1, "a fresh send is attempt one");
        assert_eq!(
            request.submit.destination.number().as_str(),
            "2250102030405"
        );
    }

    /// CA-006-07: the rejection happens here, before the command can reach a
    /// repository or a socket.
    #[test]
    fn an_invalid_recipient_is_rejected_at_the_boundary() {
        let mut input = an_input();
        input.destination = "+225ABC".to_owned();

        let rejection = input.parse().expect_err("letters are not a number");

        assert_eq!(
            rejection.code,
            crate::error::ErrorCode::MessageInvalidDestination
        );
        assert!(!rejection.message.contains("225ABC"));
    }

    /// The WebView is untrusted: a sender ID the form would have capped is
    /// capped here too.
    #[test]
    fn an_oversized_sender_id_is_rejected_whatever_the_form_allowed() {
        let mut input = an_input();
        input.source = Some("A".repeat(12));

        let rejection = input.parse().expect_err("twelve characters");

        assert_eq!(
            rejection.code,
            crate::error::ErrorCode::MessageInvalidSource
        );
    }

    /// An empty source means "message centre, choose one" and is not a
    /// rejection — the DTO carries `null` for that, and an empty string from a
    /// hand-made `invoke` is treated the same way rather than refused.
    #[test]
    fn an_absent_sender_is_accepted_and_leaves_the_field_empty() {
        let mut input = an_input();
        input.source = None;
        assert!(input.parse().expect("valid").submit.source.is_none());

        let mut input = an_input();
        input.source = Some("   ".to_owned());
        assert!(input.parse().expect("valid").submit.source.is_none());
    }

    #[test]
    fn a_custom_tlv_is_read_from_its_hexadecimal_form() {
        let mut input = an_input();
        input.tlvs = vec![TlvDto {
            tag: 0x1403,
            value_hex: "DE AD be ef".to_owned(),
        }];

        let request = input.parse().expect("valid hexadecimal");
        let tlv = request.submit.tlvs.first().expect("one TLV");

        assert_eq!(tlv.tag(), 0x1403);
        assert_eq!(tlv.value(), &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn a_malformed_tlv_is_rejected_rather_than_truncated() {
        for value in ["DEA", "ZZ", "0xDEAD"] {
            let mut input = an_input();
            input.tlvs = vec![TlvDto {
                tag: 0x1403,
                value_hex: value.to_owned(),
            }];

            assert_eq!(
                input.parse().expect_err(value).code,
                crate::error::ErrorCode::MessageInvalidTlv,
                "{value:?}"
            );
        }
    }

    /// An empty TLV value is legal SMPP: some tags are indicators whose
    /// presence is the whole message.
    #[test]
    fn an_empty_tlv_value_is_accepted() {
        let mut input = an_input();
        input.tlvs = vec![TlvDto {
            tag: 0x130C,
            value_hex: String::new(),
        }];

        assert!(input.parse().is_ok());
    }

    /// Spec §23.3 and CA-006-06: the default the form sends produces
    /// `registered_delivery = 1`.
    #[test]
    fn the_default_registered_delivery_selection_is_the_octet_the_specification_prescribes() {
        assert_eq!(
            u8::from(RegisteredDelivery::from(
                RegisteredDeliveryDto::OnAnyOutcome
            )),
            1
        );
        assert_eq!(
            u8::from(RegisteredDelivery::from(RegisteredDeliveryDto::None)),
            0
        );
        assert_eq!(
            u8::from(RegisteredDelivery::from(RegisteredDeliveryDto::OnFailure)),
            2
        );
    }

    /// Spec §7.4: the drop-down values are the octets of the table.
    #[test]
    fn the_type_of_number_selector_carries_the_octets_of_the_specification() {
        assert_eq!(u8::from(Ton::from(TonDto::International)), 1);
        assert_eq!(u8::from(Ton::from(TonDto::Alphanumeric)), 5);
        assert_eq!(u8::from(Npi::from(NpiDto::Isdn)), 1);
        assert_eq!(u8::from(Npi::from(NpiDto::National)), 8);
    }

    /// Each selector is honoured on its own. The form offers two, so dropping
    /// one because the other was left alone would discard a choice the
    /// operator made without telling them (CA-006-06).
    #[test]
    fn each_sender_field_is_honoured_independently_of_the_other() {
        let mut input = an_input();
        input.source_ton = Some(TonDto::International);
        input.source_npi = None;

        let source = input
            .parse()
            .expect("valid")
            .submit
            .source
            .expect("a sender");

        assert_eq!(source.ton(), Ton::International, "the chosen TON is used");
        assert_eq!(source.npi(), Npi::Unknown, "and the derived NPI stands");

        let mut input = an_input();
        input.source_ton = None;
        input.source_npi = Some(NpiDto::Isdn);

        let source = input
            .parse()
            .expect("valid")
            .submit
            .source
            .expect("a sender");

        assert_eq!(source.ton(), Ton::Alphanumeric, "derived from the address");
        assert_eq!(source.npi(), Npi::Isdn, "the chosen NPI is used");
    }

    /// With neither chosen, both come from the address.
    #[test]
    fn an_unspecified_sender_type_is_derived_from_the_address() {
        let source = an_input()
            .parse()
            .expect("valid")
            .submit
            .source
            .expect("a sender");

        assert_eq!(source.ton(), Ton::Alphanumeric);
        assert_eq!(source.npi(), Npi::Unknown);
    }

    #[test]
    fn the_send_result_carries_the_raw_status_and_its_plain_language_label() {
        let report = SendReport {
            client_message_id: smpp_core::types::ClientMessageId::new(),
            session_id: SessionId::new(),
            state: MessageState::Failed,
            segments: 1,
            smsc_message_id: None,
            command_status: Some(CommandStatus::EsmeRthrottled),
            retryable: true,
            journalled: true,
            outcomes: vec![SegmentOutcome::Answered {
                status: CommandStatus::EsmeRthrottled,
                smsc_message_id: None,
            }],
        };

        let dto = MessageSendResultDto::from(&report);

        assert_eq!(dto.state, "FAILED");
        assert_eq!(dto.command_status, Some(0x0000_0058));
        assert_eq!(dto.status_symbol.as_deref(), Some("ESME_RTHROTTLED"));
        assert!(dto.status_label.is_some(), "ENF-UTI-02: a readable label");
        assert!(!dto.status_is_vendor_specific);
        assert!(dto.retryable);
        assert_eq!(dto.outcomes.len(), 1);
        assert_eq!(dto.outcomes[0].outcome, "answered");
    }

    #[test]
    fn a_segment_that_was_never_sent_says_so_and_carries_no_status() {
        let dto = segment_dto(2, &SegmentOutcome::NotAttempted);

        assert_eq!(dto.sequence_number, 3);
        assert_eq!(dto.outcome, "notAttempted");
        assert_eq!(dto.command_status, None);
    }
}
