//! Building a `submit_sm` (deliverable L-006-04).
//!
//! Spec §7.3 lists sixteen mandatory fields. CA-006-06 requires **every one**
//! of them to be settable from the interface and to actually reach the PDU, so
//! [`SubmitOptions`] carries all sixteen and nothing is quietly defaulted out
//! of reach.
//!
//! # What is decided here and what is decided elsewhere
//!
//! Four fields are **not** in [`SubmitOptions`], because they are a property
//! of the segment rather than of the message:
//!
//! | Field | Comes from |
//! |-------|-----------|
//! | `data_coding` | the encoding [`crate::segmentation`] settled on |
//! | `esm_class` | the same, UDHI bit included when the body carries a header |
//! | `short_message` | the segment body |
//! | `message_payload` TLV | the segment body, in `MessagePayload` mode |
//!
//! Letting the interface override those would let it contradict the encoder,
//! and the contradiction would be invisible until a handset showed mojibake.
//!
//! # Defaults
//!
//! [`SubmitOptions::to`] is the safe configuration of spec §23.3:
//! `registered_delivery = 1` (a receipt on final outcome, which is what
//! milestone 008 correlates), `International`/`E.164` on the destination,
//! everything else the protocol's own default. An operator who changes nothing
//! sends a message that works.

use core::str::FromStr as _;

use smpp_core::octets::{AnyOctetString, COctetString, EmptyOrFullCOctetString, OctetString};
use smpp_core::pdus::SubmitSm;
use smpp_core::tlvs::{MessageSubmissionRequestTlvValue, TlvTag};
use smpp_core::values::{
    DataCoding, EsmClass, IntermediateNotification, MCDeliveryReceipt, Npi, PriorityFlag,
    RegisteredDelivery, ReplaceIfPresentFlag, ServiceType, SmeOriginatedAcknowledgement, Ton,
};

use crate::addressing::{empty_address, AddressError, Destination, SourceAddress};
use crate::segmentation::{Segment, SegmentBody};

/// Longest `service_type`, `schedule_delivery_time` and `validity_period`
/// accepted by their protocol fields.
///
/// `service_type` is a `COctetString<1, 6>` — five characters plus the NUL.
/// The two time fields are `EmptyOrFullCOctetString<17>`: either empty or
/// exactly sixteen characters, never in between (spec §7.1.1).
const MAX_SERVICE_TYPE: usize = 5;

/// Exact length of an absolute or relative SMPP time, `YYMMDDhhmmsstnnp`.
pub const SMPP_TIME_LENGTH: usize = 16;

/// Highest `priority_flag` spec §7.3 defines.
///
/// The field is an octet, so a wider value is representable; `0` to `3` is
/// what GSM defines and what a message centre accepts. The bound lives here
/// rather than in the interface because CLAUDE.md §3 treats the WebView as
/// untrusted — a hand-crafted `invoke` carrying `200` must be refused by the
/// same code the form goes through.
pub const MAX_PRIORITY_FLAG: u8 = 3;

/// Why a `submit_sm` could not be built.
///
/// Every variant names a field. None carries its value: a message body is
/// user content and an address is personal data (CLAUDE.md §8).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SubmitBuildError {
    /// An address was refused.
    #[error(transparent)]
    Address(#[from] AddressError),

    /// `service_type` is longer than the five characters the field carries.
    #[error("service_type holds at most {MAX_SERVICE_TYPE} characters")]
    ServiceTypeTooLong,

    /// A time field is neither empty nor exactly sixteen characters.
    #[error("`{field}` must be empty or exactly {SMPP_TIME_LENGTH} characters (YYMMDDhhmmsstnnp)")]
    MalformedTime {
        /// Which of the two time fields.
        field: &'static str,
    },

    /// The segment body does not fit `short_message`.
    ///
    /// Unreachable through [`crate::segmentation::segment`], which never
    /// produces a body above 255 octets. Checked rather than asserted because
    /// an `expect` here would be a `panic!` in production code.
    #[error("the segment body does not fit short_message")]
    BodyTooLong,

    /// A custom TLV value is longer than a TLV can carry.
    #[error("a custom TLV value holds at most {maximum} octets")]
    TlvValueTooLong {
        /// The ceiling, so the interface can show it.
        maximum: usize,
    },

    /// `priority_flag` is outside the range spec §7.3 defines.
    #[error("priority_flag holds a value between 0 and {maximum}")]
    PriorityOutOfRange {
        /// The ceiling, so the interface can show it.
        maximum: u8,
    },

    /// A `submit_multi` was asked for with no recipient at all.
    ///
    /// `number_of_dests = 0` is not a PDU any message centre accepts, and a
    /// batch nobody is in is a caller mistake rather than an empty success.
    #[error("a submit_multi carries at least one recipient")]
    NoDestinations,

    /// A `submit_multi` was asked for with more recipients than one PDU holds.
    ///
    /// Refused rather than truncated. `number_of_dests` is a single octet and
    /// `rusmpp` fills it with `dest_address.len() as u8`, so a 256-recipient
    /// vector would announce **zero** destinations and the extra recipients
    /// would vanish without a trace — exactly the "losing a recipient"
    /// CA-010-08 forbids. Split the batch with
    /// [`slice::chunks`](slice::chunks) instead.
    #[error("a submit_multi carries at most {maximum} recipients")]
    TooManyDestinations {
        /// The ceiling, so the caller can split on it.
        maximum: usize,
    },
}

/// A custom optional parameter the operator typed (fiche §2).
///
/// Tag and raw value, nothing interpreted: the whole point of a custom TLV is
/// that this application does not know what it means. The tag is a `u16`
/// rather than a [`TlvTag`] because an operator's vendor tag is precisely one
/// the enum does not name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomTlv {
    tag: u16,
    value: Vec<u8>,
}

impl CustomTlv {
    /// Largest value a TLV can carry: its length field is 16 bits.
    pub const MAX_VALUE_OCTETS: usize = u16::MAX as usize;

    /// Builds a custom TLV.
    ///
    /// # Errors
    ///
    /// [`SubmitBuildError::TlvValueTooLong`] past 65 535 octets.
    pub fn new(tag: u16, value: Vec<u8>) -> Result<Self, SubmitBuildError> {
        if value.len() > Self::MAX_VALUE_OCTETS {
            return Err(SubmitBuildError::TlvValueTooLong {
                maximum: Self::MAX_VALUE_OCTETS,
            });
        }

        Ok(Self { tag, value })
    }

    /// The tag, as it goes on the wire.
    #[must_use]
    pub const fn tag(&self) -> u16 {
        self.tag
    }

    /// The value octets.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }

    /// The `rusmpp` TLV this becomes.
    fn to_tlv(&self) -> MessageSubmissionRequestTlvValue {
        MessageSubmissionRequestTlvValue::Other {
            tag: TlvTag::from(self.tag),
            value: AnyOctetString::from_vec(self.value.clone()),
        }
    }
}

/// Every field of spec §7.3 the operator controls, plus the custom TLVs.
///
/// Not `Copy` and not cheap to clone — it owns three strings and a `Vec` — so
/// the send path takes it by reference and builds one PDU per segment from the
/// same borrow (guide §18: no reflex `clone()` on the hot path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitOptions {
    /// `service_type`; empty for the message centre's default.
    pub service_type: String,
    /// The sender, or `None` to let the message centre choose one.
    pub source: Option<SourceAddress>,
    /// The recipient.
    pub destination: Destination,
    /// `protocol_id`; network-specific, `0` unless the operator says
    /// otherwise.
    pub protocol_id: u8,
    /// `priority_flag`.
    pub priority_flag: PriorityFlag,
    /// `schedule_delivery_time`; empty for immediate delivery.
    pub schedule_delivery_time: String,
    /// `validity_period`; empty for the message centre's default.
    pub validity_period: String,
    /// `registered_delivery`.
    pub registered_delivery: RegisteredDelivery,
    /// `replace_if_present_flag`.
    pub replace_if_present_flag: ReplaceIfPresentFlag,
    /// `sm_default_msg_id`; `0` unless a canned message is being sent.
    pub sm_default_msg_id: u8,
    /// Optional parameters the operator typed.
    pub tlvs: Vec<CustomTlv>,
}

impl SubmitOptions {
    /// The safe configuration of spec §23.3 for `destination`.
    ///
    /// There is no `Default` impl, deliberately: a `submit_sm` without a
    /// recipient is not a defaultable thing, and a `..Default::default()` in a
    /// struct literal would fabricate one.
    #[must_use]
    pub fn to(destination: Destination) -> Self {
        Self {
            service_type: String::new(),
            source: None,
            destination,
            protocol_id: 0,
            priority_flag: PriorityFlag::default(),
            schedule_delivery_time: String::new(),
            validity_period: String::new(),
            registered_delivery: default_registered_delivery(),
            replace_if_present_flag: ReplaceIfPresentFlag::default(),
            sm_default_msg_id: 0,
            tlvs: Vec::new(),
        }
    }

    /// The same options with a sender address.
    #[must_use]
    pub fn with_source(mut self, source: SourceAddress) -> Self {
        self.source = Some(source);
        self
    }

    /// The same options with custom optional parameters.
    #[must_use]
    pub fn with_tlvs(mut self, tlvs: Vec<CustomTlv>) -> Self {
        self.tlvs = tlvs;
        self
    }
}

/// `registered_delivery = 1` — spec §23.3.
///
/// A receipt on the final outcome, success **or** failure. That is the one
/// value milestone 008 can correlate: `0` means the SMSC never sends a
/// receipt, and `request_all()` (`0x1F`) also asks for intermediate
/// notifications and SME acknowledgements, which most message centres refuse
/// outright with `ESME_RINVREGDLVFLG`.
#[must_use]
pub fn default_registered_delivery() -> RegisteredDelivery {
    RegisteredDelivery::new(
        MCDeliveryReceipt::McDeliveryReceiptRequestedWhereFinalDeliveryOutcomeIsSuccessOrFailure,
        SmeOriginatedAcknowledgement::NoReceiptSmeAcknowledgementRequested,
        IntermediateNotification::NoIntermediaryNotificationRequested,
        0,
    )
}

/// Builds the `submit_sm` for one segment of one message.
///
/// The two halves are visible in the body: `options` supplies the sixteen
/// fields of spec §7.3, `segment` supplies `data_coding`, `esm_class` and the
/// body — plus the `sar_*` TLVs when the message centre expects those rather
/// than a UDH.
///
/// # Errors
///
/// One of [`SubmitBuildError`]; every variant names the offending field.
pub fn build_submit_sm(
    options: &SubmitOptions,
    segment: &Segment,
) -> Result<SubmitSm, SubmitBuildError> {
    let common = CommonFields::build(options, segment)?;

    Ok(SubmitSm::new(
        common.service_type,
        common.source_ton,
        common.source_npi,
        common.source_addr,
        options.destination.ton(),
        options.destination.npi(),
        options.destination.to_field()?,
        common.esm_class,
        options.protocol_id,
        options.priority_flag,
        common.schedule_delivery_time,
        common.validity_period,
        options.registered_delivery,
        options.replace_if_present_flag,
        common.data_coding,
        options.sm_default_msg_id,
        common.short_message,
        common.tlvs,
    ))
}

/// Everything a `submit_sm` and a `submit_multi` carry **identically**.
///
/// Extracted so the two builders cannot drift: the only difference between the
/// PDUs of spec §7.3 and §7.4 is how the recipient is named — one address
/// against a list — and every other field, every validation and the TLV
/// ordering are the same. Two independent transcriptions of that would
/// eventually disagree on one octet, and the octet they disagreed on would be
/// whichever one nobody wrote a test for.
///
/// Fields are `pub(crate)` rather than accessors: this is a bag of already
/// built values with no invariant of its own, and it never leaves the crate.
pub(crate) struct CommonFields {
    pub(crate) service_type: ServiceType,
    pub(crate) source_ton: Ton,
    pub(crate) source_npi: Npi,
    pub(crate) source_addr: COctetString<1, 21>,
    pub(crate) esm_class: EsmClass,
    pub(crate) schedule_delivery_time: EmptyOrFullCOctetString<17>,
    pub(crate) validity_period: EmptyOrFullCOctetString<17>,
    pub(crate) data_coding: DataCoding,
    pub(crate) short_message: OctetString<0, 255>,
    pub(crate) tlvs: Vec<MessageSubmissionRequestTlvValue>,
}

impl CommonFields {
    /// Builds them, refusing every field the protocol cannot carry.
    ///
    /// # Errors
    ///
    /// One of [`SubmitBuildError`]; every variant names the offending field.
    pub(crate) fn build(
        options: &SubmitOptions,
        segment: &Segment,
    ) -> Result<Self, SubmitBuildError> {
        if u8::from(options.priority_flag) > MAX_PRIORITY_FLAG {
            return Err(SubmitBuildError::PriorityOutOfRange {
                maximum: MAX_PRIORITY_FLAG,
            });
        }

        let (source_addr, source_ton, source_npi) = match options.source.as_ref() {
            Some(source) => (source.to_field()?, source.ton(), source.npi()),
            // An empty `source_addr` is legal and means "message centre, use
            // your own". TON and NPI go to `Unknown` with it: announcing
            // `International` beside an empty address is a contradiction some
            // SMSCs reject.
            None => (empty_address()?, Ton::Unknown, Npi::Unknown),
        };

        let mut tlvs: Vec<MessageSubmissionRequestTlvValue> =
            Vec::with_capacity(options.tlvs.len() + 4);

        // The operator's TLVs go in FIRST, so a `sar_*` or `message_payload` the
        // segmenter owns cannot be shadowed by one typed by hand: `rusmpp`
        // encodes the vector in order, and a message centre reading the last
        // occurrence would then reassemble against a hand-typed reference.
        tlvs.extend(options.tlvs.iter().map(CustomTlv::to_tlv));

        if let Some(sar) = segment.sar() {
            tlvs.push(MessageSubmissionRequestTlvValue::SarMsgRefNum(
                sar.msg_ref_num,
            ));
            tlvs.push(MessageSubmissionRequestTlvValue::SarTotalSegments(
                sar.total_segments,
            ));
            tlvs.push(MessageSubmissionRequestTlvValue::SarSegmentSeqnum(
                sar.segment_seqnum,
            ));
        }

        let short_message = match segment.body() {
            SegmentBody::ShortMessage(octets) => {
                OctetString::from_slice(octets).map_err(|_| SubmitBuildError::BodyTooLong)?
            }
            SegmentBody::MessagePayload(payload) => {
                tlvs.push(MessageSubmissionRequestTlvValue::MessagePayload(
                    payload.clone(),
                ));

                // Spec §7.5: the two are exclusive — with the payload TLV
                // present, `sm_length` is zero and `short_message` is empty.
                OctetString::empty()
            }
        };

        Ok(Self {
            service_type: service_type(&options.service_type)?,
            source_ton,
            source_npi,
            source_addr,
            esm_class: segment.esm_class(),
            schedule_delivery_time: smpp_time(
                &options.schedule_delivery_time,
                "schedule_delivery_time",
            )?,
            validity_period: smpp_time(&options.validity_period, "validity_period")?,
            data_coding: segment.data_coding(),
            short_message,
            tlvs,
        })
    }
}

/// Builds the `service_type` field.
fn service_type(raw: &str) -> Result<ServiceType, SubmitBuildError> {
    if raw.is_empty() {
        return Ok(ServiceType::null());
    }

    if raw.chars().count() > MAX_SERVICE_TYPE {
        return Err(SubmitBuildError::ServiceTypeTooLong);
    }

    COctetString::from_str(raw)
        .map(ServiceType::new)
        .map_err(|_| SubmitBuildError::ServiceTypeTooLong)
}

/// Builds one of the two SMPP time fields.
///
/// `EmptyOrFullCOctetString<17>` is exactly what its name says: empty, or
/// sixteen characters plus the NUL, with nothing legal in between. A
/// fifteen-character time is a typo, and letting it through would produce a
/// PDU the encoder refuses much further down.
fn smpp_time(
    raw: &str,
    field: &'static str,
) -> Result<EmptyOrFullCOctetString<17>, SubmitBuildError> {
    if raw.is_empty() {
        return EmptyOrFullCOctetString::from_str("")
            .map_err(|_| SubmitBuildError::MalformedTime { field });
    }

    if raw.chars().count() != SMPP_TIME_LENGTH {
        return Err(SubmitBuildError::MalformedTime { field });
    }

    EmptyOrFullCOctetString::from_str(raw).map_err(|_| SubmitBuildError::MalformedTime { field })
}

#[cfg(test)]
mod tests {
    use super::{
        build_submit_sm, default_registered_delivery, CustomTlv, SubmitBuildError, SubmitOptions,
        MAX_PRIORITY_FLAG, SMPP_TIME_LENGTH,
    };
    use crate::addressing::{Destination, SourceAddress};
    use crate::segmentation::{
        segment, ConcatenationReference, SegmentationMode, SegmentationOptions,
    };
    use smpp_core::tlvs::TlvTag;
    use smpp_core::values::{DataCoding, Npi, PriorityFlag, ReplaceIfPresentFlag, Ton};

    fn one_segment(text: &str, options: &SegmentationOptions) -> crate::segmentation::Segment {
        segment(text, options, ConcatenationReference::new(7))
            .expect("the fixture text encodes")
            .into_segments()
            .into_iter()
            .next()
            .expect("at least one segment")
    }

    fn options() -> SubmitOptions {
        SubmitOptions::to(Destination::parse("+2250102030405").expect("valid"))
    }

    /// CA-006-06, field by field: what was typed is what travels.
    #[test]
    fn every_mandatory_field_of_the_specification_reaches_the_pdu() {
        let time = "2601011200000000";
        assert_eq!(time.len(), SMPP_TIME_LENGTH);

        let mut typed = options().with_source(SourceAddress::parse("ShinobiSMS").expect("valid"));
        typed.service_type = String::from("CMT");
        typed.protocol_id = 0x42;
        typed.priority_flag = PriorityFlag::from(MAX_PRIORITY_FLAG);
        typed.schedule_delivery_time = String::from(time);
        typed.validity_period = String::from(time);
        typed.replace_if_present_flag = ReplaceIfPresentFlag::Replace;
        typed.sm_default_msg_id = 9;

        let body = one_segment("Bonjour", &SegmentationOptions::default());
        let pdu = build_submit_sm(&typed, &body).expect("the fixture builds");

        assert_eq!(pdu.service_type.value().as_str(), "CMT");
        assert_eq!(pdu.source_addr_ton, Ton::Alphanumeric);
        assert_eq!(pdu.source_addr_npi, Npi::Unknown);
        assert_eq!(pdu.source_addr.as_str(), "ShinobiSMS");
        assert_eq!(pdu.dest_addr_ton, Ton::International);
        assert_eq!(pdu.dest_addr_npi, Npi::Isdn);
        assert_eq!(pdu.destination_addr.as_str(), "2250102030405");
        assert_eq!(pdu.protocol_id, 0x42);
        assert_eq!(u8::from(pdu.priority_flag), 3);
        assert_eq!(pdu.schedule_delivery_time.as_str(), time);
        assert_eq!(pdu.validity_period.as_str(), time);
        assert_eq!(u8::from(pdu.registered_delivery), 1);
        assert_eq!(pdu.replace_if_present_flag, ReplaceIfPresentFlag::Replace);
        assert_eq!(pdu.data_coding, DataCoding::McSpecific);
        assert_eq!(pdu.sm_default_msg_id, 9);
        assert_eq!(pdu.short_message().as_ref(), b"Bonjour");
        assert_eq!(pdu.sm_length(), 7);
    }

    /// Spec §23.3: the default is `registered_delivery = 1`, not `0` and not
    /// `request_all()`. Milestone 008 correlates receipts, and this octet is
    /// what asks for one.
    #[test]
    fn the_default_registered_delivery_is_one() {
        assert_eq!(u8::from(default_registered_delivery()), 1);

        let pdu = build_submit_sm(
            &options(),
            &one_segment("Bonjour", &SegmentationOptions::default()),
        )
        .expect("builds");

        assert_eq!(u8::from(pdu.registered_delivery), 1);
    }

    /// A message with no sender announces no numbering plan either: an empty
    /// address described as `International` is a contradiction.
    #[test]
    fn an_omitted_sender_leaves_the_field_and_its_type_empty() {
        let pdu = build_submit_sm(
            &options(),
            &one_segment("Bonjour", &SegmentationOptions::default()),
        )
        .expect("builds");

        assert_eq!(pdu.source_addr.as_str(), "");
        assert_eq!(pdu.source_addr_ton, Ton::Unknown);
        assert_eq!(pdu.source_addr_npi, Npi::Unknown);
    }

    /// CA-006-08: a custom TLV reaches the PDU with its tag and its length.
    #[test]
    fn a_custom_tlv_reaches_the_pdu_with_its_tag_and_length() {
        let typed = options().with_tlvs(vec![
            CustomTlv::new(0x1403, vec![0xDE, 0xAD, 0xBE, 0xEF]).expect("short enough")
        ]);

        let pdu = build_submit_sm(
            &typed,
            &one_segment("Bonjour", &SegmentationOptions::default()),
        )
        .expect("builds");

        let tlv = pdu.tlvs().first().expect("the TLV travelled");

        assert_eq!(tlv.tag(), TlvTag::from(0x1403));
        assert_eq!(tlv.value_length(), 4);
    }

    #[test]
    fn a_tlv_value_beyond_the_length_field_is_refused() {
        assert_eq!(
            CustomTlv::new(0x1403, vec![0; CustomTlv::MAX_VALUE_OCTETS + 1]).expect_err("too long"),
            SubmitBuildError::TlvValueTooLong {
                maximum: CustomTlv::MAX_VALUE_OCTETS
            }
        );
    }

    /// The `sar_*` triplet belongs to the segmenter, and it is emitted **after**
    /// the operator's TLVs so a hand-typed `sar_msg_ref_num` cannot win.
    #[test]
    fn the_segmenters_own_tlvs_are_written_after_the_operators() {
        let typed = options().with_tlvs(vec![
            CustomTlv::new(0x020C, vec![0x00, 0x01]).expect("short enough")
        ]);

        let split = segment(
            &"a".repeat(400),
            &SegmentationOptions::default().with_mode(SegmentationMode::Sar),
            ConcatenationReference::new(0x1234),
        )
        .expect("encodes");

        let first = split.segments().first().expect("three segments");
        let pdu = build_submit_sm(&typed, first).expect("builds");

        let tags: Vec<TlvTag> = pdu.tlvs().iter().map(smpp_core::tlvs::Tlv::tag).collect();

        assert_eq!(tags.first(), Some(&TlvTag::from(0x020C)));
        assert_eq!(tags.last(), Some(&TlvTag::SarSegmentSeqnum));
        assert!(tags.contains(&TlvTag::SarMsgRefNum));
    }

    /// Spec §7.5: with the payload TLV in use, `sm_length` is zero and
    /// `short_message` is empty.
    #[test]
    fn a_payload_body_leaves_short_message_empty() {
        let body = one_segment(
            "Bonjour",
            &SegmentationOptions::default().with_mode(SegmentationMode::MessagePayload),
        );

        let pdu = build_submit_sm(&options(), &body).expect("builds");

        assert_eq!(pdu.sm_length(), 0);
        assert!(pdu.short_message().as_ref().is_empty());
        assert_eq!(
            pdu.tlvs().first().map(smpp_core::tlvs::Tlv::tag),
            Some(TlvTag::MessagePayload)
        );
    }

    /// The WebView is untrusted: a `priority_flag` the form would have clamped
    /// is refused here too, and by the code every caller goes through.
    #[test]
    fn a_priority_outside_the_specification_range_is_refused() {
        let mut typed = options();
        typed.priority_flag = PriorityFlag::from(4);

        assert_eq!(
            build_submit_sm(
                &typed,
                &one_segment("Bonjour", &SegmentationOptions::default())
            )
            .expect_err("out of range"),
            SubmitBuildError::PriorityOutOfRange {
                maximum: MAX_PRIORITY_FLAG
            }
        );

        // And the whole legal range is accepted.
        for level in 0..=MAX_PRIORITY_FLAG {
            let mut typed = options();
            typed.priority_flag = PriorityFlag::from(level);

            assert!(
                build_submit_sm(
                    &typed,
                    &one_segment("Bonjour", &SegmentationOptions::default())
                )
                .is_ok(),
                "priority {level} was refused"
            );
        }
    }

    #[test]
    fn a_service_type_longer_than_the_field_is_refused() {
        let mut typed = options();
        typed.service_type = String::from("TOOLONG");

        assert_eq!(
            build_submit_sm(
                &typed,
                &one_segment("Bonjour", &SegmentationOptions::default())
            )
            .expect_err("too long"),
            SubmitBuildError::ServiceTypeTooLong
        );
    }

    /// `EmptyOrFullCOctetString<17>` is empty or exactly sixteen. A
    /// fifteen-character time is a typo and is refused where the operator can
    /// still see the field it belongs to.
    #[test]
    fn a_time_field_is_empty_or_exactly_sixteen_characters() {
        let mut typed = options();
        typed.validity_period = String::from("260101120000000");

        assert_eq!(
            build_submit_sm(
                &typed,
                &one_segment("Bonjour", &SegmentationOptions::default())
            )
            .expect_err("wrong length"),
            SubmitBuildError::MalformedTime {
                field: "validity_period"
            }
        );
    }

    #[test]
    fn an_empty_time_field_means_immediate_and_default() {
        let pdu = build_submit_sm(
            &options(),
            &one_segment("Bonjour", &SegmentationOptions::default()),
        )
        .expect("builds");

        assert_eq!(pdu.schedule_delivery_time.as_str(), "");
        assert_eq!(pdu.validity_period.as_str(), "");
    }

    /// The four fields the segmenter owns follow the segment, not the options
    /// — a concatenated part carries the UDHI bit whatever the operator typed.
    #[test]
    fn a_concatenated_segment_carries_the_udhi_bit_and_the_settled_encoding() {
        let split = segment(
            "é".repeat(200).as_str(),
            &SegmentationOptions::default(),
            ConcatenationReference::new(3),
        )
        .expect("encodes");

        let first = split.segments().first().expect("several segments");
        let pdu = build_submit_sm(&options(), first).expect("builds");

        assert_eq!(pdu.data_coding, first.data_coding());
        assert_eq!(pdu.esm_class, first.esm_class());
        // The exact octet, not "not zero". `assert_ne!(…, 0)` is the shape
        // that let `EsmClass::default()` — which is `0x08`, an ANSI-41
        // delivery acknowledgement — pass for three milestones, because the
        // octet was never zero to begin with. `0x40` is the UDHI bit alone.
        assert_eq!(u8::from(pdu.esm_class), 0x40);
    }
}
