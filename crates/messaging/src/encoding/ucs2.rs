//! UCS2, the `data_coding` 0x08 of spec §7.5 — UTF-16 big endian.
//!
//! The name is a historical fiction: the field is called UCS2, which is a
//! fixed-width 16-bit encoding, but every message centre and handset in
//! service treats it as UTF-16, surrogate pairs included. That is what this
//! module implements, and it is why an emoji costs **two** code units instead
//! of one.
//!
//! The consequence for segmentation is the second half of CA-004-05: a
//! surrogate pair is one character written as two code units, and cutting
//! between them delivers two replacement characters instead of one emoji. The
//! planner treats the pair as indivisible, exactly as it does a GSM escape
//! pair — same problem, different alphabet.

use crate::encoding::{error::EncodingError, Encoding};

/// UTF-16 code units `character` costs: one, or two behind a surrogate pair.
pub(crate) const fn code_unit_cost(character: char) -> usize {
    character.len_utf16()
}

/// Turns `text` into UTF-16 code units. Cannot fail: UCS2 covers Unicode.
pub(crate) fn encode(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Writes code units as big-endian octet pairs, appending to `out`.
pub(crate) fn pack(code_units: &[u16], out: &mut Vec<u8>) {
    for &code_unit in code_units {
        out.extend_from_slice(&code_unit.to_be_bytes());
    }
}

/// Reads big-endian octet pairs back into code units.
///
/// # Errors
///
/// [`EncodingError::MalformedUserData`] on an odd octet count — a body that
/// was cut in the middle of a code unit. `sequence_number` only labels the
/// error.
pub(crate) fn unpack(octets: &[u8], sequence_number: u8) -> Result<Vec<u16>, EncodingError> {
    if !octets.len().is_multiple_of(2) {
        return Err(EncodingError::MalformedUserData {
            sequence_number,
            encoding: Encoding::Ucs2,
            reason: "body holds an odd number of octets, cutting a code unit in half",
        });
    }

    Ok(octets
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect())
}

/// Reads code units back into text.
///
/// # Errors
///
/// [`EncodingError::MalformedUserData`] on an unpaired surrogate — which is
/// what a segment boundary drawn in the wrong place produces, and what
/// CA-004-05 forbids the segmenter from ever emitting.
pub(crate) fn decode(code_units: &[u16], sequence_number: u8) -> Result<String, EncodingError> {
    String::from_utf16(code_units).map_err(|_| EncodingError::MalformedUserData {
        sequence_number,
        encoding: Encoding::Ucs2,
        reason: "body holds an unpaired surrogate",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_basic_plane_character_costs_one_code_unit() {
        assert_eq!(code_unit_cost('a'), 1);
        assert_eq!(code_unit_cost('你'), 1);
        assert_eq!(code_unit_cost('€'), 1);
    }

    /// The UCS2 half of CA-004-05: this is the pair that must never be split.
    #[test]
    fn a_character_outside_the_basic_plane_costs_two_code_units() {
        assert_eq!(code_unit_cost('\u{1F600}'), 2);
        assert_eq!(encode("\u{1F600}").len(), 2);
    }

    #[test]
    fn text_round_trips_through_code_units_and_octets() {
        let text = "Bonjour 你好 \u{1F600} €";
        let code_units = encode(text);

        let mut octets = Vec::new();
        pack(&code_units, &mut octets);

        assert_eq!(octets.len(), code_units.len() * 2);
        assert_eq!(unpack(&octets, 1), Ok(code_units.clone()));
        assert_eq!(decode(&code_units, 1), Ok(text.to_owned()));
    }

    #[test]
    fn the_octets_are_big_endian() {
        let mut octets = Vec::new();
        pack(&encode("A"), &mut octets);

        assert_eq!(octets, vec![0x00, 0x41]);
    }

    #[test]
    fn an_odd_octet_count_is_a_malformed_body() {
        assert!(matches!(
            unpack(&[0x00, 0x41, 0x00], 2),
            Err(EncodingError::MalformedUserData {
                sequence_number: 2,
                ..
            })
        ));
    }

    #[test]
    fn a_split_surrogate_pair_is_a_malformed_body() {
        let code_units = encode("\u{1F600}");

        assert!(matches!(
            decode(&code_units[..1], 2),
            Err(EncodingError::MalformedUserData {
                sequence_number: 2,
                ..
            })
        ));
    }

    #[test]
    fn an_empty_text_encodes_to_nothing() {
        assert_eq!(encode(""), Vec::<u16>::new());
        assert_eq!(decode(&[], 1), Ok(String::new()));
    }
}
