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
    preview::{concatenated_filler, octets_for, plan, Placement, MAX_SEGMENTS},
    ucs2, Encoding, EncodingChoice, EncodingError, Gsm7BitPacking,
};

/// Octets a concatenation UDH takes out of the body.
///
/// `05 00 03 ref total index` — spec §7.5, and
/// [`ConcatenatedShortMessage8Bit::UDH_LENGTH`].
pub(crate) const CONCATENATION_UDH_OCTETS: usize = ConcatenatedShortMessage8Bit::UDH_LENGTH;

/// The septet a packed body is padded with — position `0x0D` of the base
/// table, a carriage return.
const CARRIAGE_RETURN_SEPTET: u8 = 0x0D;

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

/// Everything the segmenter needs to know that is not the text itself.
///
/// All three fields are properties of the **session or campaign**, not of the
/// message: which concatenation form the message centre accepts, how it
/// expects GSM 7-bit laid out, and whether the operator overrode the encoding.
/// Grouping them keeps [`segment`] and
/// [`preview`](crate::encoding::preview::preview) taking the *same* input,
/// which is what makes their agreement checkable (CA-004-09).
///
/// The default is the safe configuration for an unknown message centre:
/// automatic encoding, concatenation UDH, unpacked GSM 7-bit.
///
/// ```
/// use messaging::{
///     encoding::{Encoding, EncodingChoice, Gsm7BitPacking},
///     segmentation::{SegmentationMode, SegmentationOptions},
/// };
///
/// let defaults = SegmentationOptions::default();
///
/// assert_eq!(defaults.encoding, EncodingChoice::Automatic);
/// assert_eq!(defaults.mode, SegmentationMode::Udh);
/// assert_eq!(defaults.gsm_packing, Gsm7BitPacking::Unpacked);
///
/// let legacy = SegmentationOptions::default()
///     .with_mode(SegmentationMode::Sar)
///     .with_gsm_packing(Gsm7BitPacking::Packed)
///     .with_encoding(EncodingChoice::Forced(Encoding::Ucs2));
///
/// assert_eq!(legacy.mode, SegmentationMode::Sar);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SegmentationOptions {
    /// Automatic detection, or the encoding the user forced.
    pub encoding: EncodingChoice,
    /// How the parts announce that they belong together.
    pub mode: SegmentationMode,
    /// How GSM 7-bit septets sit in the octets of `short_message`.
    pub gsm_packing: Gsm7BitPacking,
}

impl SegmentationOptions {
    /// The same options with another encoding choice.
    #[must_use]
    pub const fn with_encoding(mut self, encoding: EncodingChoice) -> Self {
        self.encoding = encoding;

        self
    }

    /// The same options with another concatenation mode.
    #[must_use]
    pub const fn with_mode(mut self, mode: SegmentationMode) -> Self {
        self.mode = mode;

        self
    }

    /// The same options with another GSM 7-bit layout.
    #[must_use]
    pub const fn with_gsm_packing(mut self, gsm_packing: Gsm7BitPacking) -> Self {
        self.gsm_packing = gsm_packing;

        self
    }
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
///
/// # Why it does not start at zero
///
/// A deterministic start is a real hazard across a restart: the parts of a
/// message sent just before the process stopped may still be in flight, and a
/// counter that begins at zero again hands their reference to the very next
/// message. The handset then merges two unrelated messages, once, at a moment
/// nobody can reproduce.
///
/// [`Default`] therefore seeds the counter randomly.
/// [`starting_at`](Self::starting_at) exists for tests, which guide §7 requires
/// to be deterministic.
#[derive(Debug)]
pub struct ConcatenationReferenceCounter {
    next: core::sync::atomic::AtomicU16,
}

impl Default for ConcatenationReferenceCounter {
    fn default() -> Self {
        Self::random()
    }
}

impl ConcatenationReferenceCounter {
    /// A counter seeded from the operating system's randomness.
    ///
    /// The seed comes from a version 4 UUID rather than a random-number crate:
    /// `uuid` is already in the dependency graph and draws from the platform
    /// entropy source, and 16 bits of a v4 UUID are 16 random bits. Adding a
    /// dependency for two octets would not pass CLAUDE.md §2.
    #[must_use]
    pub fn random() -> Self {
        let [high, low, ..] = uuid::Uuid::new_v4().into_bytes();

        Self::starting_at(u16::from_be_bytes([high, low]))
    }

    /// A counter starting at `start`, for tests and for resuming a session.
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
    gsm_packing: Gsm7BitPacking,
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

    /// How the septets of the body are laid out. GSM 7-bit only.
    #[must_use]
    pub const fn gsm_packing(&self) -> Gsm7BitPacking {
        self.gsm_packing
    }

    /// Encoding units of user data — septets, UTF-16 code units or octets,
    /// depending on [`Self::encoding`].
    ///
    /// What the *encoder* wrote. Under [`Gsm7BitPacking::Packed`] a receiver
    /// that only has `sm_length` cannot always recover this exact number, and
    /// [`reassemble`] deliberately does not read it — it recomputes, the way a
    /// receiver has to. The field remains what the interface and the logs want
    /// to show.
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
    options: &SegmentationOptions,
    reference: ConcatenationReference,
) -> Result<SegmentedMessage, EncodingError> {
    let layout = plan(text, options)?;
    let encoding = layout.encoding;
    let mode = options.mode;
    let packing = options.gsm_packing;

    let total_segments =
        u8::try_from(layout.segments).map_err(|_| EncodingError::TooManySegments {
            segments: layout.segments,
            maximum: MAX_SEGMENTS,
        })?;

    // Same greedy fill as the planner, replayed to find where the cuts fall.
    // `SegmentFiller` is the single statement of the rule (CA-004-09).
    let cuts = cut_offsets(text, encoding, layout.budget, layout.segments, options)?;
    let concatenated = layout.segments > 1;

    let units = EncodedUnits::encode(text, encoding, layout.total_units)?;
    let mut segments = Vec::with_capacity(layout.segments);

    for index in 0..layout.segments {
        // INVARIANT: `layout.segments` fits in a `u8` (checked above) and
        // `index` is strictly below it, so `index + 1` is in 1..=255.
        let sequence_number = u8::try_from(index + 1).unwrap_or(u8::MAX);

        let start = cuts.get(index).copied().unwrap_or(0);
        let end = cuts.get(index + 1).copied().unwrap_or(layout.total_units);

        let header = match concatenated && mode == SegmentationMode::Udh {
            // The validating constructor, not the unchecked one: the numbering
            // above is what it checks, and a future change to it should fail
            // loudly rather than emit a malformed UDH.
            true => Some(
                ConcatenatedShortMessage8Bit::new(
                    reference.as_u8(),
                    total_segments,
                    sequence_number,
                )
                .map_err(|_| EncodingError::InvalidConcatenationHeader {
                    sequence_number,
                    total_segments,
                })?
                .udh_bytes(),
            ),
            false => None,
        };

        let header_octets = header.map_or(0, |octets| octets.len());
        let content_units = end - start;

        let mut body = Vec::with_capacity(
            header_octets + octets_for(encoding, content_units, header_octets, packing),
        );

        if let Some(header) = header {
            body.extend_from_slice(&header);
        }

        units.write(start..end, header_octets, packing, &mut body);

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
            gsm_packing: packing,
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

/// Drops the padding septet TS 23.038 §6.1.2.3.1 allows on a final segment.
///
/// When a packed body leaves exactly seven spare bits, the writer fills them
/// with a carriage return — zeroes would read as `@` — and a receiver dividing
/// octets by seven therefore recovers one septet too many.
///
/// The segmenter refuses to close a **non-final** segment on such a count, so
/// there the recovered number is exact and nothing is stripped. On the **last**
/// segment the count is whatever the text made it, there is no later segment to
/// push a character into, and this is the case the standard covers by
/// prescribing `CR` as the pad value.
///
/// # The residual ambiguity
///
/// A last segment whose genuine final character is a carriage return, at that
/// exact alignment, is indistinguishable from a padded one — and loses it.
/// TS 23.038 has the same ambiguity and tells the *sender* to write the
/// carriage return twice. It cannot arise under
/// [`Gsm7BitPacking::Unpacked`](crate::encoding::Gsm7BitPacking::Unpacked),
/// where the octet count is the septet count.
fn strip_padding_septet(septets: &mut Vec<u8>, segment: &Segment, fill_bits: usize) {
    let is_last = segment.sequence_number == segment.total_segments;
    let could_be_padding = septets
        .len()
        .checked_sub(1)
        .is_some_and(|written| !gsm0338::septet_count_is_recoverable(written, fill_bits));

    if is_last && could_be_padding && septets.last() == Some(&CARRIAGE_RETURN_SEPTET) {
        septets.pop();
    }
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
            let septets = match segment.gsm_packing {
                Gsm7BitPacking::Unpacked => {
                    gsm0338::read_unpacked(user_data, segment.sequence_number)?
                }
                Gsm7BitPacking::Packed => {
                    let fill_bits = gsm0338::fill_bits_after(segment.header_octets);

                    // Deliberately NOT `segment.content_units`. A receiver has
                    // `sm_length` in octets and nothing else, so it divides —
                    // and reading the count out of our own structure would
                    // make this function blind to exactly the class of bug
                    // that division introduces.
                    let available = gsm0338::septets_in(user_data.len(), fill_bits);
                    let mut septets =
                        gsm0338::unpack(user_data, fill_bits, available, segment.sequence_number)?;

                    strip_padding_septet(&mut septets, segment, fill_bits);

                    septets
                }
            };

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
fn cut_offsets(
    text: &str,
    encoding: Encoding,
    budget: usize,
    segments: usize,
    options: &SegmentationOptions,
) -> Result<Vec<usize>, EncodingError> {
    if segments <= 1 {
        return Ok(Vec::new());
    }

    let mut offsets = Vec::with_capacity(segments);
    offsets.push(0);

    let mut filler = concatenated_filler(budget, encoding, options);
    let mut offset = 0_usize;

    for (index, character) in text.chars().enumerate() {
        // The planner already proved every character representable; raising
        // the error rather than defaulting to zero keeps that a fact.
        let cost =
            encoding
                .unit_cost(character)
                .ok_or(EncodingError::UnrepresentableCharacter {
                    character,
                    index,
                    encoding,
                })?;

        if let Placement::Opened { rewound } = filler.accept(cost) {
            offsets.push(offset - rewound);
        }

        offset += cost;
    }

    Ok(offsets)
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
    /// `header_octets` and `packing` only matter for GSM 7-bit, where together
    /// they decide whether the septets are packed and behind how many fill
    /// bits.
    ///
    /// `range` comes from the planner and is always within the buffer the
    /// planner sized; the `debug_assert!` states that rather than leaving the
    /// empty-slice fallback to be discovered as a silently truncated body.
    fn write(
        &self,
        range: core::ops::Range<usize>,
        header_octets: usize,
        packing: Gsm7BitPacking,
        out: &mut Vec<u8>,
    ) {
        debug_assert!(
            range.end <= self.len(),
            "segment range {range:?} is out of bounds"
        );

        match self {
            Self::Septets(septets) => {
                let slice = septets.get(range).unwrap_or_default();

                match packing {
                    Gsm7BitPacking::Unpacked => out.extend_from_slice(slice),
                    Gsm7BitPacking::Packed => {
                        gsm0338::pack(slice, gsm0338::fill_bits_after(header_octets), out);
                    }
                }
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

    /// Units held, whatever the variant.
    fn len(&self) -> usize {
        match self {
            Self::Septets(septets) | Self::Octets(septets) => septets.len(),
            Self::CodeUnits(code_units) => code_units.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fiche §6 asks for the reference strategy to be decided and documented.
    /// This pins the decided one: a cyclic counter, per session.
    #[test]
    fn the_reference_counter_hands_out_distinct_values_and_wraps() {
        let counter = ConcatenationReferenceCounter::starting_at(0);

        assert_eq!(counter.next(), ConcatenationReference::new(0));
        assert_eq!(counter.next(), ConcatenationReference::new(1));
        assert_eq!(counter.next(), ConcatenationReference::new(2));

        let counter = ConcatenationReferenceCounter::starting_at(u16::MAX);

        assert_eq!(counter.next(), ConcatenationReference::new(u16::MAX));
        assert_eq!(counter.next(), ConcatenationReference::new(0));
    }

    /// The 8-bit UDH keeps the low octet, so two references 256 apart collide
    /// there while staying distinct in `sar_msg_ref_num`. Documented rather
    /// than fixed: it is the ceiling of the UDH format.
    #[test]
    fn the_udh_reference_is_the_low_octet_of_the_sixteen_bit_one() {
        let reference = ConcatenationReference::new(0xBE_EF);

        assert_eq!(reference.as_u16(), 0xBE_EF);
        assert_eq!(reference.as_u8(), 0xEF);
        assert_eq!(ConcatenationReference::new(0x00_EF).as_u8(), 0xEF);
    }

    #[test]
    fn the_default_mode_is_the_concatenation_udh() {
        assert_eq!(SegmentationMode::default(), SegmentationMode::Udh);
    }

    /// The default configuration is the safe one for an unknown message
    /// centre. Changing it is a protocol decision, not a preference.
    #[test]
    fn the_defaults_are_udh_and_unpacked_gsm() {
        let defaults = SegmentationOptions::default();

        assert_eq!(defaults.mode, SegmentationMode::Udh);
        assert_eq!(defaults.gsm_packing, Gsm7BitPacking::Unpacked);
        assert_eq!(defaults.encoding, EncodingChoice::Automatic);
    }

    /// The mode is a property of the message centre, not of the text, so the
    /// same text under two modes is cut in the same places — only the
    /// concatenation information differs.
    #[test]
    fn udh_and_sar_cut_the_text_in_the_same_places() {
        let text = "a".repeat(400);
        let reference = ConcatenationReference::new(7);

        let udh = segment(&text, &SegmentationOptions::default(), reference).expect("plain ASCII");
        let sar = segment(
            &text,
            &SegmentationOptions::default().with_mode(SegmentationMode::Sar),
            reference,
        )
        .expect("plain ASCII");

        let units = |message: &SegmentedMessage| {
            message
                .segments()
                .iter()
                .map(Segment::content_units)
                .collect::<Vec<_>>()
        };

        assert_eq!(units(&udh), units(&sar));
        assert_eq!(units(&udh), vec![153, 153, 94]);
    }

    /// Two references drawn at random from two counters are almost certainly
    /// different, which is the whole point of not starting at zero. One
    /// collision in 65 536 is expected; the test would have to be very
    /// unlucky twice to be flaky in a way that matters.
    #[test]
    fn the_default_counter_does_not_start_at_a_fixed_value() {
        let draws: std::collections::HashSet<u16> = (0..16)
            .map(|_| ConcatenationReferenceCounter::default().next().as_u16())
            .collect();

        assert!(
            draws.len() > 1,
            "sixteen counters all started on the same reference"
        );
    }

    // -----------------------------------------------------------------
    // Bodies the segmenter would never produce
    // -----------------------------------------------------------------

    impl Segment {
        /// A segment assembled from raw parts, for tests only.
        ///
        /// [`reassemble`] rejects bodies that cannot have come out of
        /// [`segment`] — a dangling GSM escape, half a UCS2 code unit. Nothing
        /// in the public API can build one, so without this the guards have no
        /// coverage at all and could be deleted without a test noticing.
        fn from_raw_parts(encoding: Encoding, octets: Vec<u8>, header_octets: usize) -> Self {
            let content_units = octets.len().saturating_sub(header_octets);

            Self {
                sequence_number: 1,
                total_segments: 1,
                encoding,
                gsm_packing: Gsm7BitPacking::Unpacked,
                esm_class: EsmClass::default(),
                header_octets,
                content_units,
                body: SegmentBody::ShortMessage(octets),
                sar: None,
            }
        }
    }

    /// CA-004-05, read from the receiving end: a body whose last septet is an
    /// escape lost the character it was escaping.
    #[test]
    fn a_body_ending_on_a_dangling_escape_is_refused() {
        let segment = Segment::from_raw_parts(Encoding::Gsm7Bit, vec![b'a', gsm0338::ESCAPE], 0);

        assert_eq!(
            reassemble(&[segment]),
            Err(EncodingError::MalformedUserData {
                sequence_number: 1,
                encoding: Encoding::Gsm7Bit,
                reason: "body ends on an escape septet with nothing to escape",
            })
        );
    }

    /// An unpacked body cannot hold an octet above 0x7F. Refusing it is what
    /// turns "a packed body read as unpacked" into an error instead of
    /// gibberish.
    #[test]
    fn an_unpacked_gsm_body_above_a_septet_is_refused() {
        let segment = Segment::from_raw_parts(Encoding::Gsm7Bit, vec![0xE8, 0x32], 0);

        assert!(matches!(
            reassemble(&[segment]),
            Err(EncodingError::MalformedUserData {
                encoding: Encoding::Gsm7Bit,
                ..
            })
        ));
    }

    /// CA-004-05, UCS2 half: an odd octet count cut a code unit in two.
    #[test]
    fn a_ucs2_body_of_odd_length_is_refused() {
        let segment = Segment::from_raw_parts(Encoding::Ucs2, vec![0x00, 0x41, 0x00], 0);

        assert!(matches!(
            reassemble(&[segment]),
            Err(EncodingError::MalformedUserData {
                encoding: Encoding::Ucs2,
                ..
            })
        ));
    }

    /// The other UCS2 half: both code units are whole, but the pair is not.
    #[test]
    fn a_ucs2_body_holding_half_a_surrogate_pair_is_refused() {
        // The high surrogate of U+1F600, with nothing after it.
        let segment = Segment::from_raw_parts(Encoding::Ucs2, vec![0xD8, 0x3D], 0);

        assert!(matches!(
            reassemble(&[segment]),
            Err(EncodingError::MalformedUserData {
                encoding: Encoding::Ucs2,
                ..
            })
        ));
    }

    #[test]
    fn a_body_shorter_than_its_own_header_is_refused() {
        let segment = Segment::from_raw_parts(Encoding::Gsm7Bit, vec![0x05, 0x00], 6);

        assert!(matches!(
            reassemble(&[segment]),
            Err(EncodingError::MalformedUserData { .. })
        ));
    }
}
