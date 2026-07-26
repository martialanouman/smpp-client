//! Splitting a message into segments (deliverable L-004-03).
//!
//! Spec §7.5, second half. Past one segment the parts have to tell the handset
//! how to put them back together, and there are two ways of saying it:
//!
//! * a **concatenation UDH** inside `short_message`, with the UDHI bit set in
//!   `esm_class` — six octets taken out of the body;
//! * the **`sar_*` TLVs**, which say the same thing out of band and leave the
//!   body alone.
//!
//! Which one to use is a property of the message centre, not of the message:
//! some accept only one, some translate the TLVs into a UDH on the delivery
//! leg. So it is configured per session or per campaign and handed in — this
//! module guesses nothing. [`SegmentationMode::Udh`] is the default because it
//! is the form every handset understands, the TLVs being optional in SMPP
//! v3.4.
//!
//! A third mode, [`SegmentationMode::MessagePayload`], does not split at all:
//! the whole body goes into the `message_payload` TLV, up to 64 KiB. Only some
//! message centres support it.
//!
//! # What this module does not do
//!
//! It does not send. It produces the field values a `submit_sm` needs —
//! `short_message` or `message_payload`, `esm_class`, `data_coding`, and the
//! TLVs — and stops there. Building the PDUs, correlating the responses and
//! pacing the emission belong to milestone 006.

use smpp_core::{
    octets::AnyOctetString,
    udhs::concatenation::ConcatenatedShortMessage8Bit,
    values::{DataCoding, EsmClass, MessagePayload},
};

use crate::encoding::{
    gsm0338, latin1,
    preview::{octets_for, plan, SegmentFiller, MAX_SEGMENTS},
    ucs2, Encoding, EncodingChoice, EncodingError,
};

/// How the parts of a long message announce that they belong together.
///
/// A characteristic of the message centre, configured per session or per
/// campaign (fiche §6). Never inferred from the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SegmentationMode {
    /// Concatenation UDH inside `short_message`, UDHI set in `esm_class`.
    #[default]
    Udh,
    /// `sar_msg_ref_num` / `sar_total_segments` / `sar_segment_seqnum` TLVs,
    /// and a body with no header.
    Sar,
    /// No splitting: the whole body in the `message_payload` TLV.
    MessagePayload,
}

/// The number that ties the parts of one concatenated message together.
///
/// # Uniqueness, and what happens without it
///
/// Two messages in flight to the same handset with the same reference are
/// reassembled into one mixture — a silent, unreproducible corruption on the
/// receiving side. The reference therefore has to be unique *per recipient*
/// over the time the parts can be in flight.
///
/// The strategy this crate implements is a cyclic counter per session
/// ([`ConcatenationReferenceCounter`]). It does not depend on the recipient,
/// which is stronger than required and much simpler than a per-recipient map;
/// it reuses a value only after the counter has wrapped, which at any
/// realistic throughput is far longer than a message stays in flight.
///
/// The counter runs over 16 bits, which `sar_msg_ref_num` carries whole. The
/// 8-bit concatenation UDH keeps only the low octet ([`Self::as_u8`]), so in
/// [`SegmentationMode::Udh`] the cycle is 256 messages — the ceiling the UDH
/// format imposes, not one this crate adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConcatenationReference(u16);

impl ConcatenationReference {
    /// A reference with this exact value.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The full 16-bit value, for `sar_msg_ref_num`.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// The low octet, for the 8-bit concatenation UDH of spec §7.5.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0.to_le_bytes()[0]
    }
}

/// A cyclic reference counter, one per session.
///
/// Atomic so a session can hand out references from several tasks without a
/// lock, which matters because milestone 006 pipelines submissions.
#[derive(Debug, Default)]
pub struct ConcatenationReferenceCounter {
    next: core::sync::atomic::AtomicU16,
}

impl ConcatenationReferenceCounter {
    /// A counter starting at `start`.
    #[must_use]
    pub const fn starting_at(start: u16) -> Self {
        Self {
            next: core::sync::atomic::AtomicU16::new(start),
        }
    }

    /// The next reference, wrapping at 65 535.
    ///
    /// `Relaxed` is enough: the only requirement is that two calls return
    /// different values, which `fetch_add` guarantees on its own. No other
    /// memory is being published.
    pub fn next(&self) -> ConcatenationReference {
        ConcatenationReference(
            self.next
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        )
    }
}

/// The `sar_*` TLV triplet of spec §7.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SarParameters {
    /// `sar_msg_ref_num` — identical on every segment of the message.
    pub msg_ref_num: u16,
    /// `sar_total_segments` — the count, identical on every segment.
    pub total_segments: u8,
    /// `sar_segment_seqnum` — this segment's index, from 1.
    pub segment_seqnum: u8,
}

/// Where the body of a segment goes in the PDU.
///
/// The two are exclusive (spec §7.5): when the payload TLV is used,
/// `sm_length` is zero and `short_message` is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentBody {
    /// Goes into `short_message`; `sm_length` is its length.
    ///
    /// In [`SegmentationMode::Udh`] the concatenation header occupies the
    /// first [`Segment::header_octets`] octets.
    ShortMessage(Vec<u8>),
    /// Goes into the `message_payload` TLV; `sm_length` is zero.
    MessagePayload(MessagePayload),
}

/// One segment, ready for a `submit_sm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    sequence_number: u8,
    total_segments: u8,
    encoding: Encoding,
    esm_class: EsmClass,
    header_octets: usize,
    content_units: usize,
    body: SegmentBody,
    sar: Option<SarParameters>,
}

impl Segment {
    /// This segment's index, from 1.
    #[must_use]
    pub const fn sequence_number(&self) -> u8 {
        self.sequence_number
    }

    /// Segments the message was split into.
    #[must_use]
    pub const fn total_segments(&self) -> u8 {
        self.total_segments
    }

    /// The encoding of the body.
    #[must_use]
    pub const fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// The `data_coding` octet to put in the PDU.
    #[must_use]
    pub const fn data_coding(&self) -> DataCoding {
        self.encoding.data_coding()
    }

    /// The `esm_class` octet to put in the PDU, UDHI bit included when the
    /// body starts with a concatenation header.
    #[must_use]
    pub const fn esm_class(&self) -> EsmClass {
        self.esm_class
    }

    /// Octets of concatenation header at the front of the body. Zero unless
    /// [`SegmentationMode::Udh`] actually split the message.
    #[must_use]
    pub const fn header_octets(&self) -> usize {
        self.header_octets
    }

    /// Encoding units of user data — septets, UTF-16 code units or octets,
    /// depending on [`Self::encoding`].
    ///
    /// Not derivable from the body length for GSM 7-bit: packing loses the
    /// count, which is precisely why the padding convention exists.
    #[must_use]
    pub const fn content_units(&self) -> usize {
        self.content_units
    }

    /// The body, and which PDU field it belongs in.
    #[must_use]
    pub fn body(&self) -> &SegmentBody {
        &self.body
    }

    /// The `short_message` octets, when the body goes in that field.
    #[must_use]
    pub fn short_message(&self) -> Option<&[u8]> {
        match &self.body {
            SegmentBody::ShortMessage(octets) => Some(octets),
            SegmentBody::MessagePayload(_) => None,
        }
    }

    /// The `message_payload` TLV, when the body goes in that field.
    #[must_use]
    pub fn message_payload(&self) -> Option<&MessagePayload> {
        match &self.body {
            SegmentBody::MessagePayload(payload) => Some(payload),
            SegmentBody::ShortMessage(_) => None,
        }
    }

    /// The `sar_*` TLVs, present only in [`SegmentationMode::Sar`] on a
    /// message that actually split.
    #[must_use]
    pub const fn sar(&self) -> Option<SarParameters> {
        self.sar
    }
}

/// A text turned into segments, with the encoding that was settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentedMessage {
    encoding: Encoding,
    mode: SegmentationMode,
    reference: Option<ConcatenationReference>,
    segments: Vec<Segment>,
}

impl SegmentedMessage {
    /// The encoding used — detected, or the one that was forced.
    #[must_use]
    pub const fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// The `data_coding` octet every segment carries.
    #[must_use]
    pub const fn data_coding(&self) -> DataCoding {
        self.encoding.data_coding()
    }

    /// The mode the segments were built for.
    #[must_use]
    pub const fn mode(&self) -> SegmentationMode {
        self.mode
    }

    /// The concatenation reference, present only when the message split.
    #[must_use]
    pub const fn reference(&self) -> Option<ConcatenationReference> {
        self.reference
    }

    /// The segments, in order, from index 1.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Consumes the message and yields its segments.
    #[must_use]
    pub fn into_segments(self) -> Vec<Segment> {
        self.segments
    }
}

/// Splits `text` into segments.
///
/// `reference` is only used when the message actually splits; a message that
/// fits in one segment carries no concatenation information at all, and in
/// particular no UDHI bit — a lone segment has nothing to be concatenated
/// with.
///
/// # Errors
///
/// [`EncodingError::UnrepresentableCharacter`] when a forced encoding cannot
/// write the text, [`EncodingError::TooManySegments`] past 255 segments,
/// [`EncodingError::PayloadTooLarge`] past 64 KiB in
/// [`SegmentationMode::MessagePayload`].
pub fn segment(
    text: &str,
    choice: EncodingChoice,
    mode: SegmentationMode,
    reference: ConcatenationReference,
) -> Result<SegmentedMessage, EncodingError> {
    let layout = plan(text, choice, mode)?;
    let encoding = layout.encoding;

    let total_segments =
        u8::try_from(layout.segments).map_err(|_| EncodingError::TooManySegments {
            segments: layout.segments,
            maximum: MAX_SEGMENTS,
        })?;

    // Same greedy fill as the planner, replayed to find where the cuts fall.
    // `SegmentFiller` is the single statement of the rule (CA-004-09).
    let cuts = cut_offsets(text, encoding, layout.budget, layout.segments);
    let concatenated = layout.segments > 1;

    let units = EncodedUnits::encode(text, encoding, layout.total_units)?;
    let mut segments = Vec::with_capacity(layout.segments);

    for index in 0..layout.segments {
        // INVARIANT: `layout.segments` fits in a `u8` (checked above) and
        // `index` is strictly below it, so `index + 1` is in 1..=255.
        let sequence_number = u8::try_from(index + 1).unwrap_or(u8::MAX);

        let start = cuts.get(index).copied().unwrap_or(0);
        let end = cuts.get(index + 1).copied().unwrap_or(layout.total_units);

        let header = (concatenated && mode == SegmentationMode::Udh).then(|| {
            // INVARIANT: `total_segments >= 2` and `sequence_number` is in
            // 1..=total_segments, which is exactly what `new` checks.
            ConcatenatedShortMessage8Bit::new_unchecked(
                reference.as_u8(),
                total_segments,
                sequence_number,
            )
            .udh_bytes()
        });

        let header_octets = header.map_or(0, |octets| octets.len());
        let content_units = end - start;

        let mut body = Vec::with_capacity(
            header_octets + octets_for_body(encoding, content_units, header_octets),
        );

        if let Some(header) = header {
            body.extend_from_slice(&header);
        }

        units.write(start..end, header_octets, &mut body);

        let esm_class = if header.is_some() {
            EsmClass::default().with_udhi_indicator()
        } else {
            EsmClass::default()
        };

        let sar = (concatenated && mode == SegmentationMode::Sar).then_some(SarParameters {
            msg_ref_num: reference.as_u16(),
            total_segments,
            segment_seqnum: sequence_number,
        });

        let body = if mode == SegmentationMode::MessagePayload {
            SegmentBody::MessagePayload(MessagePayload::new(AnyOctetString::from_vec(body)))
        } else {
            SegmentBody::ShortMessage(body)
        };

        segments.push(Segment {
            sequence_number,
            total_segments,
            encoding,
            esm_class,
            header_octets,
            content_units,
            body,
            sar,
        });
    }

    Ok(SegmentedMessage {
        encoding,
        mode,
        reference: concatenated.then_some(reference),
        segments,
    })
}

/// Puts the segments of one message back together.
///
/// The inverse of [`segment`], and the check CA-004-08 asks for. Also what
/// milestone 012 will need to reassemble a long incoming `deliver_sm`.
///
/// Segments may arrive in any order; they are read by sequence number.
///
/// # Errors
///
/// [`EncodingError::IncompleteConcatenation`] when the segments do not form
/// exactly one message, [`EncodingError::MalformedUserData`] when a body is
/// not valid for its declared encoding — including a segment that ends on a
/// dangling GSM escape or half a surrogate pair, which is what a badly placed
/// cut produces.
pub fn reassemble(segments: &[Segment]) -> Result<String, EncodingError> {
    let Some(first) = segments.first() else {
        return Err(EncodingError::IncompleteConcatenation {
            reason: "no segment at all",
        });
    };

    let total = usize::from(first.total_segments);

    if segments.len() != total {
        return Err(EncodingError::IncompleteConcatenation {
            reason: "segment count does not match the announced total",
        });
    }

    let mut ordered: Vec<&Segment> = segments.iter().collect();
    ordered.sort_by_key(|segment| segment.sequence_number);

    let mut text = String::new();

    for (index, segment) in ordered.iter().enumerate() {
        if segment.total_segments != first.total_segments
            || segment.encoding != first.encoding
            || usize::from(segment.sequence_number) != index + 1
        {
            return Err(EncodingError::IncompleteConcatenation {
                reason: "segments disagree on the total, the encoding or the order",
            });
        }

        text.push_str(&decode_segment(segment)?);
    }

    Ok(text)
}

/// Reads one segment's body back into text.
fn decode_segment(segment: &Segment) -> Result<String, EncodingError> {
    let octets = match &segment.body {
        SegmentBody::ShortMessage(octets) => octets.as_slice(),
        SegmentBody::MessagePayload(payload) => payload.value.as_ref(),
    };

    let Some(user_data) = octets.get(segment.header_octets..) else {
        return Err(EncodingError::MalformedUserData {
            sequence_number: segment.sequence_number,
            encoding: segment.encoding,
            reason: "body is shorter than its own header",
        });
    };

    match segment.encoding {
        Encoding::Gsm7Bit => {
            let septets = gsm0338::unpack(
                user_data,
                gsm0338::fill_bits_after(segment.header_octets),
                segment.content_units,
                segment.sequence_number,
            )?;

            // CA-004-05, GSM half: an escape is the first septet of a pair.
            // Finding one at the very end means the pair was cut in two, and
            // the extension character is lost on both handsets.
            if septets.last() == Some(&gsm0338::ESCAPE) {
                return Err(EncodingError::MalformedUserData {
                    sequence_number: segment.sequence_number,
                    encoding: Encoding::Gsm7Bit,
                    reason: "body ends on an escape septet with nothing to escape",
                });
            }

            Ok(gsm0338::decode(&septets))
        }
        Encoding::Latin1 => Ok(latin1::decode(user_data)),
        Encoding::Ucs2 => {
            let code_units = ucs2::unpack(user_data, segment.sequence_number)?;

            ucs2::decode(&code_units, segment.sequence_number)
        }
    }
}

/// Unit offsets at which each segment starts, first one included.
///
/// Empty when the message is not split: a single segment starts at zero and
/// runs to the end, and building a vector to say so would be one allocation
/// on the most common path.
fn cut_offsets(text: &str, encoding: Encoding, budget: usize, segments: usize) -> Vec<usize> {
    if segments <= 1 {
        return Vec::new();
    }

    let mut offsets = Vec::with_capacity(segments);
    offsets.push(0);

    let mut filler = SegmentFiller::new(budget);
    let mut offset = 0_usize;

    for character in text.chars() {
        let cost = encoding.unit_cost(character).unwrap_or(0);

        if filler.accept(cost) {
            offsets.push(offset);
        }

        offset += cost;
    }

    offsets
}

/// Octets a body of `content_units` occupies behind `header_octets` of header.
fn octets_for_body(encoding: Encoding, content_units: usize, header_octets: usize) -> usize {
    match encoding {
        Encoding::Gsm7Bit => {
            gsm0338::packed_len(content_units, gsm0338::fill_bits_after(header_octets))
        }
        _ => octets_for(encoding, content_units),
    }
}

/// The whole text encoded once, in the unit its encoding counts in.
///
/// Encoding the text once and slicing it per segment is what keeps the hot
/// path free of redundant work (CA-004-10): the alternative, re-encoding each
/// slice, would walk the text as many times as there are segments.
enum EncodedUnits {
    /// One septet per entry, escapes already expanded.
    Septets(Vec<u8>),
    /// One UTF-16 code unit per entry.
    CodeUnits(Vec<u16>),
    /// One octet per entry.
    Octets(Vec<u8>),
}

impl EncodedUnits {
    /// Encodes `text` whole into a buffer of exactly `units` entries.
    ///
    /// The planner already counted the units, so the buffer is allocated once
    /// at its final size and never grows (CA-004-10).
    fn encode(text: &str, encoding: Encoding, units: usize) -> Result<Self, EncodingError> {
        Ok(match encoding {
            Encoding::Gsm7Bit => {
                let mut septets = Vec::with_capacity(units);
                gsm0338::encode_into(text, &mut septets)?;

                Self::Septets(septets)
            }
            Encoding::Latin1 => {
                let mut octets = Vec::with_capacity(units);
                latin1::encode_into(text, &mut octets)?;

                Self::Octets(octets)
            }
            Encoding::Ucs2 => {
                let mut code_units = Vec::with_capacity(units);
                ucs2::encode_into(text, &mut code_units);

                Self::CodeUnits(code_units)
            }
        })
    }

    /// Appends the units of `range` to `out`, in wire form.
    ///
    /// `header_octets` only matters for GSM 7-bit, where it decides how many
    /// fill bits come before the first septet.
    fn write(&self, range: core::ops::Range<usize>, header_octets: usize, out: &mut Vec<u8>) {
        match self {
            Self::Septets(septets) => {
                let slice = septets.get(range).unwrap_or_default();

                gsm0338::pack(slice, gsm0338::fill_bits_after(header_octets), out);
            }
            Self::CodeUnits(code_units) => {
                let slice = code_units.get(range).unwrap_or_default();

                ucs2::pack(slice, out);
            }
            Self::Octets(octets) => {
                let slice = octets.get(range).unwrap_or_default();

                out.extend_from_slice(slice);
            }
        }
    }
}
