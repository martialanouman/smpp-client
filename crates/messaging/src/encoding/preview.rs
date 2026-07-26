//! The live character and segment counter (deliverable L-004-04).
//!
//! The message editor of milestone 006 calls this on every keystroke, so it
//! allocates nothing: it walks the characters twice and returns five numbers.
//! No segment is built, no octet is packed.
//!
//! # Why the planner lives here rather than in the segmenter
//!
//! CA-004-09 asks that the preview and the real segmentation always agree. Two
//! implementations of the same greedy fill would agree until the day one of
//! them was fixed. So there is one: [`plan`] decides how many segments a text
//! needs and where they end, [`preview`] formats its answer for the interface,
//! and [`segment`](crate::segmentation::segment) replays the same
//! [`SegmentFiller`] to place the cuts. The property test only has to confirm
//! a structural fact.

use crate::{
    encoding::{gsm0338, Encoding, EncodingChoice, EncodingError},
    segmentation::SegmentationMode,
};

/// Highest addressable segment of a concatenated message.
///
/// A protocol ceiling, not a policy: the UDH part number and
/// `sar_total_segments` are both a single octet.
pub const MAX_SEGMENTS: usize = 255;

/// Largest body the `message_payload` TLV can carry, in octets.
///
/// The TLV length field is 16 bits — the "64 Ko" of spec §7.5.
pub const MAX_MESSAGE_PAYLOAD_OCTETS: usize = 65_535;

/// What the message editor needs to draw its counter.
///
/// The four numbers of the fiche, plus the character count the user actually
/// typed — which is *not* the unit count, and telling them apart is half the
/// point of this type. A text of 10 characters containing three `€` uses 13
/// septets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessagePreview {
    encoding: Encoding,
    characters: usize,
    units_used: usize,
    units_remaining_in_segment: usize,
    segments: usize,
}

impl MessagePreview {
    /// The encoding that will be used — detected, or the forced one.
    #[must_use]
    pub const fn encoding(self) -> Encoding {
        self.encoding
    }

    /// Characters the user typed, as Unicode scalar values.
    #[must_use]
    pub const fn characters(self) -> usize {
        self.characters
    }

    /// Encoding units the whole text occupies.
    ///
    /// Septets for GSM 7-bit (`€` counts two), UTF-16 code units for UCS2 (an
    /// emoji counts two), octets for Latin-1.
    #[must_use]
    pub const fn units_used(self) -> usize {
        self.units_used
    }

    /// Units still free in the segment currently being filled.
    ///
    /// Zero means the next character opens a new segment — and if the message
    /// is still a single segment, that next character also shrinks every
    /// segment to the concatenated budget, so the counter can jump.
    #[must_use]
    pub const fn units_remaining_in_segment(self) -> usize {
        self.units_remaining_in_segment
    }

    /// Segments the message will be sent as. Never zero: an empty text is one
    /// empty segment.
    #[must_use]
    pub const fn segments(self) -> usize {
        self.segments
    }
}

/// The counter for `text` under `choice` and `mode`.
///
/// # Errors
///
/// [`EncodingError::UnrepresentableCharacter`] when a forced encoding cannot
/// write the text, [`EncodingError::TooManySegments`] past
/// [`MAX_SEGMENTS`], [`EncodingError::PayloadTooLarge`] past
/// [`MAX_MESSAGE_PAYLOAD_OCTETS`] in
/// [`SegmentationMode::MessagePayload`].
pub fn preview(
    text: &str,
    choice: EncodingChoice,
    mode: SegmentationMode,
) -> Result<MessagePreview, EncodingError> {
    let plan = plan(text, choice, mode)?;

    Ok(MessagePreview {
        encoding: plan.encoding,
        characters: plan.characters,
        units_used: plan.total_units,
        units_remaining_in_segment: plan.budget - plan.units_in_last_segment,
        segments: plan.segments,
    })
}

/// The greedy fill of one message into segments of `budget` units.
///
/// A character is never split: when its cost does not fit in what is left, the
/// whole character moves to the next segment and the remaining room is simply
/// lost. That is the rule CA-004-05 asks for, and it is stated once, here.
pub(crate) struct SegmentFiller {
    budget: usize,
    used: usize,
    segments: usize,
}

impl SegmentFiller {
    /// A filler over segments of `budget` units, positioned on the first one.
    pub(crate) const fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            segments: 1,
        }
    }

    /// Places one character of `cost` units.
    ///
    /// Returns `true` when the character did not fit and opened a new segment
    /// — the caller's signal to cut.
    pub(crate) const fn accept(&mut self, cost: usize) -> bool {
        if self.used + cost > self.budget {
            self.segments += 1;
            self.used = cost;

            return true;
        }

        self.used += cost;

        false
    }

    /// Segments opened so far.
    pub(crate) const fn segments(&self) -> usize {
        self.segments
    }

    /// Units placed in the segment currently being filled.
    pub(crate) const fn used_in_current_segment(&self) -> usize {
        self.used
    }
}

/// How many segments a text needs, and how full the last one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentPlan {
    /// The settled encoding.
    pub(crate) encoding: Encoding,
    /// Segments the message needs.
    pub(crate) segments: usize,
    /// Capacity applied to every segment, in units of `encoding`.
    pub(crate) budget: usize,
    /// Units the whole text occupies.
    pub(crate) total_units: usize,
    /// Units placed in the last segment.
    pub(crate) units_in_last_segment: usize,
    /// Characters in the text.
    pub(crate) characters: usize,
}

/// Settles the encoding, then walks the text to place the segment boundaries.
///
/// Two passes, no allocation. The first totals the units — which is what
/// decides between the single-segment budget and the concatenated one — and
/// the second replays the greedy fill only when the message actually splits.
///
/// # Errors
///
/// See [`preview`].
pub(crate) fn plan(
    text: &str,
    choice: EncodingChoice,
    mode: SegmentationMode,
) -> Result<SegmentPlan, EncodingError> {
    let encoding = super::resolve(choice, text)?;
    let (single, concatenated) = capacities(encoding, mode);

    let mut total_units = 0_usize;
    let mut characters = 0_usize;

    for (index, character) in text.chars().enumerate() {
        let Some(cost) = encoding.unit_cost(character) else {
            return Err(EncodingError::UnrepresentableCharacter {
                character,
                index,
                encoding,
            });
        };

        total_units += cost;
        characters += 1;
    }

    // A single segment carries no concatenation header, so it gets the full
    // budget. Deciding this before the greedy fill is what makes 160 GSM
    // characters one segment and 161 two.
    if total_units <= single {
        return Ok(SegmentPlan {
            encoding,
            segments: 1,
            budget: single,
            total_units,
            units_in_last_segment: total_units,
            characters,
        });
    }

    if mode == SegmentationMode::MessagePayload {
        return Err(EncodingError::PayloadTooLarge {
            octets: octets_for(encoding, total_units),
            maximum: MAX_MESSAGE_PAYLOAD_OCTETS,
        });
    }

    let mut filler = SegmentFiller::new(concatenated);

    for character in text.chars() {
        let cost = encoding.unit_cost(character).unwrap_or(0);

        filler.accept(cost);
    }

    if filler.segments() > MAX_SEGMENTS {
        return Err(EncodingError::TooManySegments {
            segments: filler.segments(),
            maximum: MAX_SEGMENTS,
        });
    }

    Ok(SegmentPlan {
        encoding,
        segments: filler.segments(),
        budget: concatenated,
        total_units,
        units_in_last_segment: filler.used_in_current_segment(),
        characters,
    })
}

/// The single-segment and concatenated capacities that apply under `mode`.
fn capacities(encoding: Encoding, mode: SegmentationMode) -> (usize, usize) {
    if mode == SegmentationMode::MessagePayload {
        // One "segment" that happens to be very large. Expressing the TLV
        // ceiling in the encoding's own unit keeps the counter meaningful:
        // 32 767 characters of UCS2, not 65 535 octets of something.
        let capacity = message_payload_capacity(encoding);

        return (capacity, capacity);
    }

    let budget = encoding.budget();

    (budget.single(), budget.concatenated())
}

/// Units of `encoding` that fit in a full `message_payload` TLV.
const fn message_payload_capacity(encoding: Encoding) -> usize {
    match encoding {
        Encoding::Gsm7Bit => MAX_MESSAGE_PAYLOAD_OCTETS * 8 / 7,
        Encoding::Latin1 => MAX_MESSAGE_PAYLOAD_OCTETS,
        Encoding::Ucs2 => MAX_MESSAGE_PAYLOAD_OCTETS / 2,
    }
}

/// Octets `units` of `encoding` occupy on the wire, headers excluded.
pub(crate) const fn octets_for(encoding: Encoding, units: usize) -> usize {
    match encoding {
        Encoding::Gsm7Bit => gsm0338::packed_len(units, 0),
        Encoding::Latin1 => units,
        Encoding::Ucs2 => units * 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn automatic(text: &str) -> MessagePreview {
        preview(text, EncodingChoice::Automatic, SegmentationMode::Udh)
            .expect("automatic detection never fails on a concatenable text")
    }

    #[test]
    fn an_empty_text_is_one_empty_segment() {
        let preview = automatic("");

        assert_eq!(preview.segments(), 1);
        assert_eq!(preview.characters(), 0);
        assert_eq!(preview.units_used(), 0);
        assert_eq!(preview.units_remaining_in_segment(), 160);
        assert_eq!(preview.encoding(), Encoding::Gsm7Bit);
    }

    /// The boundaries of CA-004-01, read through the counter.
    #[test]
    fn the_gsm_single_segment_boundary_is_at_one_hundred_and_sixty() {
        let at_159 = automatic(&"a".repeat(159));
        assert_eq!(
            (at_159.segments(), at_159.units_remaining_in_segment()),
            (1, 1)
        );

        let at_160 = automatic(&"a".repeat(160));
        assert_eq!(
            (at_160.segments(), at_160.units_remaining_in_segment()),
            (1, 0)
        );

        let at_161 = automatic(&"a".repeat(161));
        assert_eq!(at_161.segments(), 2);
        assert_eq!(at_161.units_used(), 161);
        // 153 in the first segment, 8 in the second.
        assert_eq!(at_161.units_remaining_in_segment(), 145);
    }

    /// The boundaries of CA-004-03.
    #[test]
    fn the_ucs2_single_segment_boundary_is_at_seventy() {
        let at_70 = automatic(&"你".repeat(70));
        assert_eq!(at_70.encoding(), Encoding::Ucs2);
        assert_eq!(
            (at_70.segments(), at_70.units_remaining_in_segment()),
            (1, 0)
        );

        let at_71 = automatic(&"你".repeat(71));
        assert_eq!(at_71.segments(), 2);
        // 67 in the first segment, 4 in the second.
        assert_eq!(at_71.units_remaining_in_segment(), 63);
    }

    /// CA-004-02: characters and units part ways.
    #[test]
    fn an_extension_character_counts_two_units_but_one_character() {
        let preview = automatic("€");

        assert_eq!(preview.characters(), 1);
        assert_eq!(preview.units_used(), 2);
        assert_eq!(preview.units_remaining_in_segment(), 158);
    }

    /// 80 euro signs are 160 septets: still one segment. 81 are 162, which is
    /// two — the segment count follows the septets, not the characters.
    #[test]
    fn the_segment_count_follows_the_septets_not_the_characters() {
        let at_80 = automatic(&"€".repeat(80));
        assert_eq!(
            (at_80.characters(), at_80.units_used(), at_80.segments()),
            (80, 160, 1)
        );

        let at_81 = automatic(&"€".repeat(81));
        assert_eq!(
            (at_81.characters(), at_81.units_used(), at_81.segments()),
            (81, 162, 2)
        );
    }

    #[test]
    fn a_surrogate_pair_counts_two_code_units() {
        let preview = automatic("\u{1F600}");

        assert_eq!(preview.encoding(), Encoding::Ucs2);
        assert_eq!(preview.characters(), 1);
        assert_eq!(preview.units_used(), 2);
    }

    #[test]
    fn a_forced_encoding_that_cannot_write_the_text_is_reported() {
        assert!(matches!(
            preview(
                "你",
                EncodingChoice::Forced(Encoding::Gsm7Bit),
                SegmentationMode::Udh
            ),
            Err(EncodingError::UnrepresentableCharacter { .. })
        ));
    }

    #[test]
    fn a_message_beyond_two_hundred_and_fifty_five_segments_is_refused() {
        let text = "a".repeat(153 * MAX_SEGMENTS + 1);

        assert_eq!(
            preview(&text, EncodingChoice::Automatic, SegmentationMode::Udh),
            Err(EncodingError::TooManySegments {
                segments: MAX_SEGMENTS + 1,
                maximum: MAX_SEGMENTS,
            })
        );
    }

    #[test]
    fn the_message_payload_mode_counts_one_very_large_segment() {
        let text = "a".repeat(10_000);
        let preview = preview(
            &text,
            EncodingChoice::Automatic,
            SegmentationMode::MessagePayload,
        )
        .expect("well within 64 KiB");

        assert_eq!(preview.segments(), 1);
        assert_eq!(preview.units_used(), 10_000);
    }

    #[test]
    fn a_message_payload_beyond_sixty_four_kibibytes_is_refused() {
        let text = "你".repeat(MAX_MESSAGE_PAYLOAD_OCTETS / 2 + 1);

        assert!(matches!(
            preview(
                &text,
                EncodingChoice::Automatic,
                SegmentationMode::MessagePayload
            ),
            Err(EncodingError::PayloadTooLarge { .. })
        ));
    }

    /// The greedy rule, isolated: a two-unit character facing one free unit
    /// moves whole to the next segment and leaves the unit unused.
    #[test]
    fn a_two_unit_character_never_straddles_a_boundary() {
        let mut filler = SegmentFiller::new(10);

        for _ in 0..9 {
            assert!(!filler.accept(1));
        }

        assert_eq!(filler.used_in_current_segment(), 9);
        assert!(filler.accept(2), "the pair must open a new segment");
        assert_eq!(filler.segments(), 2);
        assert_eq!(filler.used_in_current_segment(), 2);
    }
}
