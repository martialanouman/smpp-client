//! ISO-8859-1, the `data_coding` 0x03 of spec §7.5.
//!
//! One octet per character, and the octet is the Unicode code point: Latin-1
//! is the first 256 code points of Unicode, by construction. There is no
//! escape mechanism and therefore no character worth two units — which makes
//! this the encoding where the segment budget is finally just a byte count.
//!
//! Never chosen automatically (see [`detect`](super::detect)): it costs the
//! same 140 octets per segment as UCS2 while representing a great deal less.
//! It exists because some message centres and some legacy handsets want it,
//! and spec §7.5 step 3 lets the user say so.

use crate::encoding::{error::EncodingError, Encoding};

/// Highest code point Latin-1 can write.
const MAX_CODE_POINT: u32 = 0xFF;

/// Octets `character` costs, or `None` when it is outside Latin-1.
pub(crate) fn octet_cost(character: char) -> Option<usize> {
    (u32::from(character) <= MAX_CODE_POINT).then_some(1)
}

/// Whether every character of `text` fits in Latin-1.
pub(crate) fn is_representable(text: &str) -> bool {
    text.chars()
        .all(|character| u32::from(character) <= MAX_CODE_POINT)
}

/// Appends the Latin-1 octets of `text` to `octets`.
///
/// # Errors
///
/// [`EncodingError::UnrepresentableCharacter`] on the first character above
/// `U+00FF`, with its position in characters. `octets` is left in an
/// unspecified state.
pub(crate) fn encode_into(text: &str, octets: &mut Vec<u8>) -> Result<(), EncodingError> {
    for (index, character) in text.chars().enumerate() {
        let octet = u8::try_from(u32::from(character)).map_err(|_| {
            EncodingError::UnrepresentableCharacter {
                character,
                index,
                encoding: Encoding::Latin1,
            }
        })?;

        octets.push(octet);
    }

    Ok(())
}

/// Reads Latin-1 octets back. Cannot fail: every octet is a code point.
pub(crate) fn decode(octets: &[u8]) -> String {
    octets.iter().map(|&octet| char::from(octet)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`encode_into`] into a fresh vector, for readability in the assertions.
    fn encode(text: &str) -> Result<Vec<u8>, EncodingError> {
        let mut octets = Vec::new();

        encode_into(text, &mut octets)?;

        Ok(octets)
    }

    #[test]
    fn every_octet_round_trips() {
        for octet in u8::MIN..=u8::MAX {
            let text = decode(&[octet]);

            assert_eq!(text.chars().count(), 1);
            assert_eq!(encode(&text), Ok(vec![octet]));
            assert_eq!(octet_cost(char::from(octet)), Some(1));
        }
    }

    #[test]
    fn the_first_character_above_the_range_is_rejected_with_its_position() {
        assert_eq!(
            encode("aé\u{0100}"),
            Err(EncodingError::UnrepresentableCharacter {
                character: '\u{0100}',
                index: 2,
                encoding: Encoding::Latin1,
            })
        );
    }

    #[test]
    fn no_character_ever_costs_two_octets() {
        assert_eq!(octet_cost('€'), None);
        assert_eq!(octet_cost('ÿ'), Some(1));
        assert!(!is_representable("€"));
        assert!(is_representable("çàÿ"));
    }
}
