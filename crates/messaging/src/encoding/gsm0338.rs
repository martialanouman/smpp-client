//! The GSM 03.38 alphabet: tables, detection, septet packing.
//!
//! Deliverable L-004-01. Three things live here, and they are easy to confuse:
//!
//! 1. the **alphabet** — which characters GSM 7-bit can represent at all, and
//!    at what cost (spec §7.5 step 1);
//! 2. the **septet sequence** — one septet per character, two for a character
//!    of the extension table, which is what the segment budget counts;
//! 3. the **packing** — eight septets squeezed into seven octets, which is
//!    what goes on the wire.
//!
//! Nothing here knows about segments. The cost of a character is exported so
//! the planner can count without encoding.
//!
//! # The escape mechanism
//!
//! Nine characters (`^ { } [ ] ~ \ |` and `€`, plus form feed) are not in the
//! 128-entry base table. They are written as the escape septet `0x1B` followed
//! by a code from the extension table, so they cost **two** septets. Splitting
//! that pair across two segments delivers a stray escape to one handset and an
//! orphan code to the other; [`Self::septet_cost`](septet_cost) exists so the
//! planner can treat the pair as indivisible.

use crate::encoding::{error::EncodingError, Encoding};

/// The escape septet that introduces an extension-table character.
///
/// Also occupies position `0x1B` of the base table, where it is a placeholder
/// rather than a character: a literal `U+001B` in a text is *not* representable
/// in GSM 7-bit.
pub(crate) const ESCAPE: u8 = 0x1B;

/// Low seven bits — a septet.
const SEPTET_MASK: u16 = 0x7F;

/// Padding written into the seven spare bits of a final octet.
///
/// TS 23.038 §6.1.2.3.1: those bits would otherwise decode as an eighth
/// septet, and a zero septet is `@`. A carriage return is the value the
/// standard prescribes.
const CARRIAGE_RETURN: u8 = 0x0D;

/// Bits in a septet.
const SEPTET_BITS: usize = 7;

/// Bits in an octet.
const OCTET_BITS: usize = 8;

/// The 128 characters of the GSM 03.38 base table, indexed by septet value.
///
/// Position `0x1B` holds `U+001B` as a placeholder for the escape septet; it
/// is deliberately *not* an encodable character (see [`ESCAPE`]).
pub(crate) const BASE_TABLE: [char; 128] = [
    '@', '£', '$', '¥', 'è', 'é', 'ù', 'ì', // 0x00
    'ò', 'Ç', '\n', 'Ø', 'ø', '\r', 'Å', 'å', // 0x08
    'Δ', '_', 'Φ', 'Γ', 'Λ', 'Ω', 'Π', 'Ψ', // 0x10
    'Σ', 'Θ', 'Ξ', '\u{001B}', 'Æ', 'æ', 'ß', 'É', // 0x18
    ' ', '!', '"', '#', '¤', '%', '&', '\'', // 0x20
    '(', ')', '*', '+', ',', '-', '.', '/', // 0x28
    '0', '1', '2', '3', '4', '5', '6', '7', // 0x30
    '8', '9', ':', ';', '<', '=', '>', '?', // 0x38
    '¡', 'A', 'B', 'C', 'D', 'E', 'F', 'G', // 0x40
    'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', // 0x48
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', // 0x50
    'X', 'Y', 'Z', 'Ä', 'Ö', 'Ñ', 'Ü', '§', // 0x58
    '¿', 'a', 'b', 'c', 'd', 'e', 'f', 'g', // 0x60
    'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', // 0x68
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', // 0x70
    'x', 'y', 'z', 'ä', 'ö', 'ñ', 'ü', 'à', // 0x78
];

/// The GSM 03.38 extension table, as `(code after the escape, character)`.
///
/// Sorted by code so the reverse lookup can stay a linear scan over ten
/// entries without anyone wondering whether the order matters.
pub(crate) const EXTENSION_TABLE: [(u8, char); 10] = [
    (0x0A, '\u{000C}'), // FORM FEED
    (0x14, '^'),
    (0x28, '{'),
    (0x29, '}'),
    (0x2F, '\\'),
    (0x3C, '['),
    (0x3D, '~'),
    (0x3E, ']'),
    (0x40, '|'),
    (0x65, '€'),
];

/// How a character is written in the GSM 03.38 alphabet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sequence {
    /// One septet, taken from the base table.
    Base(u8),
    /// The escape septet followed by a code from the extension table.
    Extended(u8),
}

impl Sequence {
    /// Septets this sequence occupies: one, or two behind an escape.
    pub(crate) const fn septets(self) -> usize {
        match self {
            Self::Base(_) => 1,
            Self::Extended(_) => 2,
        }
    }
}

/// The GSM 03.38 sequence for `character`, or `None` when it has none.
///
/// A `match` rather than a reverse scan of [`BASE_TABLE`]: the segmenter calls
/// this once per character of every message, and the compiler turns the
/// contiguous ranges into arithmetic. The test module proves the match and the
/// tables agree over the whole of Unicode, so there is one source of truth
/// despite the two representations.
pub(crate) fn sequence_of(character: char) -> Option<Sequence> {
    // The identity ranges: base-table position equals the ASCII code point.
    // `$` (0x02) and `@` (0x00) are the holes that keep 0x24 and 0x40 out —
    // those positions hold `¤` and `¡`.
    let ascii_identity = matches!(character, ' '..='#' | '%'..='?' | 'A'..='Z' | 'a'..='z');

    if ascii_identity {
        return u8::try_from(u32::from(character)).ok().map(Sequence::Base);
    }

    let base = match character {
        '@' => 0x00,
        '£' => 0x01,
        '$' => 0x02,
        '¥' => 0x03,
        'è' => 0x04,
        'é' => 0x05,
        'ù' => 0x06,
        'ì' => 0x07,
        'ò' => 0x08,
        'Ç' => 0x09,
        '\n' => 0x0A,
        'Ø' => 0x0B,
        'ø' => 0x0C,
        '\r' => 0x0D,
        'Å' => 0x0E,
        'å' => 0x0F,
        'Δ' => 0x10,
        '_' => 0x11,
        'Φ' => 0x12,
        'Γ' => 0x13,
        'Λ' => 0x14,
        'Ω' => 0x15,
        'Π' => 0x16,
        'Ψ' => 0x17,
        'Σ' => 0x18,
        'Θ' => 0x19,
        'Ξ' => 0x1A,
        'Æ' => 0x1C,
        'æ' => 0x1D,
        'ß' => 0x1E,
        'É' => 0x1F,
        '¤' => 0x24,
        '¡' => 0x40,
        'Ä' => 0x5B,
        'Ö' => 0x5C,
        'Ñ' => 0x5D,
        'Ü' => 0x5E,
        '§' => 0x5F,
        '¿' => 0x60,
        'ä' => 0x7B,
        'ö' => 0x7C,
        'ñ' => 0x7D,
        'ü' => 0x7E,
        'à' => 0x7F,
        _ => {
            let extended = match character {
                '\u{000C}' => 0x0A,
                '^' => 0x14,
                '{' => 0x28,
                '}' => 0x29,
                '\\' => 0x2F,
                '[' => 0x3C,
                '~' => 0x3D,
                ']' => 0x3E,
                '|' => 0x40,
                '€' => 0x65,
                _ => return None,
            };

            return Some(Sequence::Extended(extended));
        }
    };

    Some(Sequence::Base(base))
}

/// Septets `character` costs, or `None` when GSM 7-bit cannot write it.
///
/// The whole point of CA-004-02: `€` answers 2, and so does every other
/// extension-table character.
pub(crate) fn septet_cost(character: char) -> Option<usize> {
    sequence_of(character).map(Sequence::septets)
}

/// Whether every character of `text` belongs to the alphabet.
///
/// Step 1 of the encoding algorithm in spec §7.5. An empty text qualifies.
pub(crate) fn is_representable(text: &str) -> bool {
    text.chars()
        .all(|character| sequence_of(character).is_some())
}

/// Appends the septet sequence of `text` to `septets`, one octet per septet.
///
/// This is *not* the wire form: [`pack`] compresses it afterwards. Keeping the
/// two apart is what lets the segmenter slice on septet boundaries — which is
/// where the segment budget is expressed — before packing each slice with the
/// fill bits its own header dictates.
///
/// It appends rather than returning a fresh vector because the planner already
/// counted the septets, so the segmenter can size the buffer exactly once
/// (CA-004-10).
///
/// # Errors
///
/// [`EncodingError::UnrepresentableCharacter`] on the first character outside
/// the alphabet, with its position in characters. `septets` is left in an
/// unspecified state.
pub(crate) fn encode_into(text: &str, septets: &mut Vec<u8>) -> Result<(), EncodingError> {
    for (index, character) in text.chars().enumerate() {
        match sequence_of(character) {
            Some(Sequence::Base(code)) => septets.push(code),
            Some(Sequence::Extended(code)) => {
                septets.push(ESCAPE);
                septets.push(code);
            }
            None => {
                return Err(EncodingError::UnrepresentableCharacter {
                    character,
                    index,
                    encoding: Encoding::Gsm7Bit,
                })
            }
        }
    }

    Ok(())
}

/// Reads back a septet sequence produced by [`encode_into`].
///
/// Lenient where TS 23.038 asks for leniency: an escape followed by a code the
/// extension table does not list falls back to the base-table character, and a
/// trailing escape with nothing after it is dropped. Neither can come out of
/// [`encode_into`]; both can come off the wire.
pub(crate) fn decode(septets: &[u8]) -> String {
    let mut text = String::with_capacity(septets.len());
    let mut septets = septets.iter().copied();

    while let Some(septet) = septets.next() {
        let code = usize::from(septet & 0x7F);

        if septet != ESCAPE {
            text.extend(BASE_TABLE.get(code));
            continue;
        }

        let Some(extended) = septets.next() else {
            // Lone trailing escape: nothing to escape, drop it.
            break;
        };

        match extension_character(extended) {
            Some(character) => text.push(character),
            None if extended == ESCAPE => text.push(' '),
            None => text.extend(BASE_TABLE.get(usize::from(extended & 0x7F))),
        }
    }

    text
}

/// The extension-table character for `code`, if the table lists it.
fn extension_character(code: u8) -> Option<char> {
    EXTENSION_TABLE
        .iter()
        .find(|(candidate, _)| *candidate == code)
        .map(|(_, character)| *character)
}

/// Octets a septet sequence occupies once packed behind `fill_bits` of padding.
pub(crate) const fn packed_len(septets: usize, fill_bits: usize) -> usize {
    let bits = fill_bits + septets * SEPTET_BITS;

    bits.div_ceil(OCTET_BITS)
}

/// Fill bits needed so the septets start on a septet boundary after a header.
///
/// A UDH is measured in octets, the user data in septets, and the two do not
/// line up: six octets of concatenation header are 48 bits, which is 6 and 6/7
/// of a septet. The seventh septet position is therefore partly consumed, and
/// the user data starts one bit later — this returns that one bit. Get it
/// wrong and every character of every segment is shifted.
pub(crate) const fn fill_bits_after(header_octets: usize) -> usize {
    let header_bits = header_octets * OCTET_BITS;
    let boundary = header_bits.div_ceil(SEPTET_BITS) * SEPTET_BITS;

    boundary - header_bits
}

/// Septets a receiver recovers from `octets` octets behind `fill_bits`.
///
/// This is the whole of the packed format's fragility over SMPP, in one line.
/// `sm_length` counts **octets**; the septet count is not transmitted, so the
/// receiver divides. Over the radio interface the equivalent field, TP-UDL,
/// counts septets and the question does not arise.
pub(crate) const fn septets_in(octets: usize, fill_bits: usize) -> usize {
    (octets * OCTET_BITS).saturating_sub(fill_bits) / SEPTET_BITS
}

/// Whether a receiver counting octets recovers exactly `septets` septets.
///
/// False when the packing leaves seven spare bits, which the receiver reads as
/// one septet too many — a carriage return, since that is what [`pack`] puts
/// there. Harmless at the very end of a message, corruption in the middle of a
/// concatenated one, which is why the segmenter refuses to close a non-final
/// segment on such a count.
pub(crate) const fn septet_count_is_recoverable(septets: usize, fill_bits: usize) -> bool {
    septets_in(packed_len(septets, fill_bits), fill_bits) == septets
}

/// Packs septets into octets, seven bits at a time, least significant first.
///
/// `fill_bits` zero bits are emitted first — see [`fill_bits_after`].
pub(crate) fn pack(septets: &[u8], fill_bits: usize, out: &mut Vec<u8>) {
    let mut accumulator: u16 = 0;
    let mut pending = fill_bits;

    for &septet in septets {
        accumulator |= (u16::from(septet) & SEPTET_MASK) << pending;
        pending += SEPTET_BITS;

        if pending >= OCTET_BITS {
            out.push(accumulator.to_le_bytes()[0]);
            accumulator >>= OCTET_BITS;
            pending -= OCTET_BITS;
        }
    }

    if pending > 0 {
        if OCTET_BITS - pending == SEPTET_BITS {
            // Exactly seven bits free: they would read as one more septet.
            accumulator |= u16::from(CARRIAGE_RETURN) << pending;
        }

        out.push(accumulator.to_le_bytes()[0]);
    }
}

/// Reverses [`pack`], recovering exactly `septet_count` septets.
///
/// The count cannot be derived from the octets alone — that ambiguity is the
/// reason [`pack`] pads with a carriage return — so the caller supplies it from
/// the segment metadata.
///
/// # Errors
///
/// [`EncodingError::MalformedUserData`] when the octets hold fewer septets than
/// asked for. `sequence_number` only labels the error.
pub(crate) fn unpack(
    octets: &[u8],
    fill_bits: usize,
    septet_count: usize,
    sequence_number: u8,
) -> Result<Vec<u8>, EncodingError> {
    let mut septets = Vec::with_capacity(septet_count);
    let mut accumulator: u16 = 0;
    let mut pending: usize = 0;
    let mut to_skip = fill_bits;

    for &octet in octets {
        accumulator |= u16::from(octet) << pending;
        pending += OCTET_BITS;

        if to_skip > 0 {
            let dropped = to_skip.min(pending);
            accumulator >>= dropped;
            pending -= dropped;
            to_skip -= dropped;
        }

        while pending >= SEPTET_BITS && septets.len() < septet_count {
            septets.push((accumulator & SEPTET_MASK).to_le_bytes()[0]);
            accumulator >>= SEPTET_BITS;
            pending -= SEPTET_BITS;
        }
    }

    if septets.len() != septet_count {
        return Err(EncodingError::MalformedUserData {
            sequence_number,
            encoding: Encoding::Gsm7Bit,
            reason: "packed body holds fewer septets than the segment declares",
        });
    }

    Ok(septets)
}

/// Reads an **unpacked** body: one septet per octet, high bit clear.
///
/// No count is needed and none can be wrong — the length in octets *is* the
/// length in septets. That is the whole reason the unpacked layout is the
/// default over SMPP.
///
/// # Errors
///
/// [`EncodingError::MalformedUserData`] on an octet with its high bit set: a
/// septet cannot exceed `0x7F`, so such a body was not written by an unpacked
/// encoder — most likely it is packed, and decoding it here would produce
/// silent gibberish.
pub(crate) fn read_unpacked(octets: &[u8], sequence_number: u8) -> Result<Vec<u8>, EncodingError> {
    if octets.iter().any(|octet| octet & 0x80 != 0) {
        return Err(EncodingError::MalformedUserData {
            sequence_number,
            encoding: Encoding::Gsm7Bit,
            reason: "unpacked body holds an octet above 0x7F, which is not a septet",
        });
    }

    Ok(octets.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`encode_into`] into a fresh vector, for readability in the assertions.
    fn encode(text: &str) -> Result<Vec<u8>, EncodingError> {
        let mut septets = Vec::new();

        encode_into(text, &mut septets)?;

        Ok(septets)
    }

    /// Fiche §5: every character of the base table, checked both ways.
    #[test]
    fn every_base_table_character_round_trips_through_its_septet() {
        for (code, &character) in BASE_TABLE.iter().enumerate() {
            let code = u8::try_from(code).expect("the table has 128 entries");

            if code == ESCAPE {
                // Not a character: the placeholder for the escape mechanism.
                continue;
            }

            assert_eq!(
                sequence_of(character),
                Some(Sequence::Base(code)),
                "base table entry {code:#04X} ({character:?})"
            );
            assert_eq!(decode(&[code]), character.to_string());
        }
    }

    /// Fiche §5: every character of the extension table, checked both ways.
    #[test]
    fn every_extension_table_character_round_trips_through_its_escape_pair() {
        for &(code, character) in &EXTENSION_TABLE {
            assert_eq!(
                sequence_of(character),
                Some(Sequence::Extended(code)),
                "extension table entry {code:#04X} ({character:?})"
            );
            assert_eq!(septet_cost(character), Some(2));
            assert_eq!(decode(&[ESCAPE, code]), character.to_string());
        }
    }

    /// The `match` and the tables are two representations of one alphabet.
    /// This is what keeps them from drifting apart.
    #[test]
    fn the_lookup_agrees_with_the_tables_over_the_whole_of_unicode() {
        for code_point in 0..=0x10_FFFF_u32 {
            let Some(character) = char::from_u32(code_point) else {
                continue;
            };

            let expected = if character == '\u{001B}' {
                None
            } else if let Some(position) = BASE_TABLE.iter().position(|&c| c == character) {
                Some(Sequence::Base(
                    u8::try_from(position).expect("the table has 128 entries"),
                ))
            } else {
                EXTENSION_TABLE
                    .iter()
                    .find(|(_, c)| *c == character)
                    .map(|&(code, _)| Sequence::Extended(code))
            };

            assert_eq!(
                sequence_of(character),
                expected,
                "code point {code_point:#06X} ({character:?})"
            );
        }
    }

    #[test]
    fn the_escape_code_point_is_not_a_character() {
        assert_eq!(sequence_of('\u{001B}'), None);
        assert!(!is_representable("\u{001B}"));
    }

    /// The euro sign is the canonical trap of spec §7.5.
    #[test]
    fn the_euro_sign_costs_two_septets() {
        assert_eq!(septet_cost('€'), Some(2));
        assert_eq!(encode("€"), Ok(vec![ESCAPE, 0x65]));
    }

    #[test]
    fn a_character_outside_the_alphabet_is_reported_with_its_position() {
        assert_eq!(
            encode("abcł"),
            Err(EncodingError::UnrepresentableCharacter {
                character: 'ł',
                index: 3,
                encoding: Encoding::Gsm7Bit,
            })
        );
    }

    /// The position is counted in characters, not in bytes: a UTF-8 offset
    /// would point into the middle of a word for the user interface.
    #[test]
    fn the_reported_position_counts_characters_not_bytes() {
        assert_eq!(
            encode("éé你"),
            Err(EncodingError::UnrepresentableCharacter {
                character: '你',
                index: 2,
                encoding: Encoding::Gsm7Bit,
            })
        );
    }

    /// The reference vector everybody uses for septet packing.
    #[test]
    fn packing_matches_the_reference_vector() {
        let septets = encode("hellohello").expect("plain ASCII");
        let mut packed = Vec::new();
        pack(&septets, 0, &mut packed);

        assert_eq!(
            packed,
            vec![0xE8, 0x32, 0x9B, 0xFD, 0x46, 0x97, 0xD9, 0xEC, 0x37]
        );
    }

    /// TS 23.038 §6.1.2.3.1: seven spare bits are filled with CR, never left
    /// at zero — zero would show up as a trailing `@` on the handset.
    #[test]
    fn seven_spare_bits_are_padded_with_a_carriage_return() {
        let septets = vec![b'A'; 7];
        let mut packed = Vec::new();
        pack(&septets, 0, &mut packed);

        assert_eq!(packed.len(), 7);
        // Last octet: one bit of the seventh 'A', then CR shifted up.
        assert_eq!(packed[6], (CARRIAGE_RETURN << 1) | 0b0000_0001);
    }

    #[test]
    fn packing_a_full_single_segment_fills_exactly_one_hundred_and_forty_octets() {
        let septets = vec![b'A'; 160];
        let mut packed = Vec::new();
        pack(&septets, 0, &mut packed);

        assert_eq!(packed.len(), 140);
        assert_eq!(packed_len(160, 0), 140);
    }

    #[test]
    fn packing_a_concatenated_segment_fills_exactly_the_remaining_octets() {
        let septets = vec![b'A'; 153];
        let mut packed = Vec::new();
        pack(&septets, 1, &mut packed);

        assert_eq!(packed.len(), 134);
        assert_eq!(packed_len(153, 1), 134);
    }

    /// Six octets of concatenation UDH leave one bit before the next septet
    /// boundary. Every character of every concatenated segment depends on it.
    #[test]
    fn a_six_octet_header_leaves_one_fill_bit() {
        assert_eq!(fill_bits_after(6), 1);
        assert_eq!(fill_bits_after(0), 0);
        assert_eq!(fill_bits_after(7), 0);
    }

    #[test]
    fn packing_round_trips_through_unpacking_at_every_alignment() {
        for length in 0..40_usize {
            for fill_bits in 0..7_usize {
                let septets: Vec<u8> = (0..length)
                    .map(|index| u8::try_from(index % 128).expect("under 128"))
                    .collect();

                let mut packed = Vec::new();
                pack(&septets, fill_bits, &mut packed);

                assert_eq!(packed.len(), packed_len(length, fill_bits));
                assert_eq!(
                    unpack(&packed, fill_bits, length, 1),
                    Ok(septets),
                    "length {length}, fill {fill_bits}"
                );
            }
        }
    }

    /// The bug this predicate exists to prevent, stated as a table.
    ///
    /// With a six-octet UDH the fill is one bit, and a segment of 152 septets
    /// occupies the same 134 octets as one of 153 — so a receiver counting
    /// octets reads 153 and finds a carriage return that nobody typed. 152 is
    /// exactly the count the extension-pair rule produces.
    #[test]
    fn a_receiver_counting_octets_recovers_the_septet_count_or_one_too_many() {
        // Behind a six-octet UDH.
        assert_eq!(packed_len(153, 1), 134);
        assert_eq!(packed_len(152, 1), 134);
        assert_eq!(septets_in(134, 1), 153);

        assert!(septet_count_is_recoverable(153, 1));
        assert!(!septet_count_is_recoverable(152, 1), "the reported bug");
        assert!(septet_count_is_recoverable(151, 1));
        assert!(septet_count_is_recoverable(150, 1));

        // Without a header the fill is zero, and the two counts the segmenter
        // can produce are both safe — which is why `sar_*` is untouched.
        assert!(septet_count_is_recoverable(153, 0));
        assert!(septet_count_is_recoverable(152, 0));
        // The unsafe counts exist there too, one residue class away.
        assert!(!septet_count_is_recoverable(7, 0));
        assert!(!septet_count_is_recoverable(8, 1));
    }

    /// Whatever the alignment, a receiver never recovers *fewer* septets than
    /// were written, and never more than one extra. The rewind rule therefore
    /// only ever has to deal with an off-by-one.
    #[test]
    fn the_recovered_count_is_never_short_and_never_more_than_one_long() {
        for septets in 0..500_usize {
            for fill_bits in 0..7_usize {
                let recovered = septets_in(packed_len(septets, fill_bits), fill_bits);

                assert!(
                    recovered == septets || recovered == septets + 1,
                    "{septets} septets at fill {fill_bits} came back as {recovered}"
                );
                assert_eq!(
                    septet_count_is_recoverable(septets, fill_bits),
                    recovered == septets
                );
            }
        }
    }

    #[test]
    fn an_unpacked_body_is_its_own_septet_sequence() {
        assert_eq!(
            read_unpacked(&[0x41, 0x42, 0x00], 1),
            Ok(vec![0x41, 0x42, 0x00])
        );
        assert_eq!(read_unpacked(&[], 1), Ok(Vec::new()));
    }

    /// An octet above 0x7F cannot be a septet. Refusing it is what turns
    /// "packed body decoded as unpacked" from silent gibberish into an error.
    #[test]
    fn an_unpacked_body_rejects_an_octet_above_a_septet() {
        assert!(matches!(
            read_unpacked(&[0x41, 0xE8], 4),
            Err(EncodingError::MalformedUserData {
                sequence_number: 4,
                ..
            })
        ));
    }

    #[test]
    fn unpacking_more_septets_than_the_octets_hold_is_an_error() {
        assert!(matches!(
            unpack(&[0x00], 0, 10, 3),
            Err(EncodingError::MalformedUserData {
                sequence_number: 3,
                ..
            })
        ));
    }

    #[test]
    fn an_unknown_escape_sequence_falls_back_to_the_base_table() {
        // 0x41 has no extension entry; the base table holds 'A' there.
        assert_eq!(decode(&[ESCAPE, 0x41]), "A");
        // A doubled escape has no meaning at all.
        assert_eq!(decode(&[ESCAPE, ESCAPE]), " ");
        // A trailing escape escapes nothing.
        assert_eq!(decode(&[b'A', ESCAPE]), "A");
    }

    #[test]
    fn an_empty_text_encodes_to_nothing() {
        assert_eq!(encode(""), Ok(Vec::new()));
        assert_eq!(decode(&[]), "");
        assert!(is_representable(""));
    }
}
