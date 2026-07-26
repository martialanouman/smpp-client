//! Turning a text into the octets of a segment body.
//!
//! Spec §7.5, milestone 004. Three encodings, one automatic choice, one manual
//! override, and a budget per segment expressed in the unit each encoding
//! actually counts in.
//!
//! # Modules
//!
//! | Module | Contents |
//! |--------|----------|
//! | `gsm0338` | the GSM 03.38 tables, the extension escape, septet packing |
//! | `latin1` | ISO-8859-1, one octet per character |
//! | `ucs2` | UTF-16BE, one or two code units per character |
//! | [`preview`] | the live counter the message editor reads |
//! | [`error`] | [`EncodingError`] |
//!
//! # Units, and why they are named
//!
//! The single most common bug in this area is comparing a count in one unit
//! against a limit in another. GSM 7-bit counts **septets** *after* the
//! extension escapes have been expanded and *independently of packing*; UCS2
//! counts **UTF-16 code units**, of which an emoji costs two; Latin-1 counts
//! **octets**. [`SegmentBudget`] carries its [`BudgetUnit`] so a limit can
//! never be read in the wrong one.
//!
//! The figures of spec §7.5 — 160 and 153 — are therefore **septets**, not
//! characters: a text of 153 characters containing one `€` is 154 septets and
//! does not fit a concatenated segment.

pub mod error;
pub(crate) mod gsm0338;
pub(crate) mod latin1;
pub mod preview;
pub(crate) mod ucs2;

pub use error::EncodingError;
pub use preview::MessagePreview;

use smpp_core::values::DataCoding;

/// A text encoding, and the `data_coding` octet that announces it.
///
/// Spec §7.5 also lists IA5 (`0x01`). It is not offered: it is a strict subset
/// of both GSM 7-bit and Latin-1 with no capacity advantage, and the fiche
/// limits the manual override to these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// GSM 03.38 7-bit. The default, and the densest.
    ///
    /// How the septets are laid out in the octets of `short_message` is a
    /// separate question — see [`Gsm7BitPacking`].
    Gsm7Bit,
    /// ISO-8859-1, one octet per character.
    Latin1,
    /// UTF-16 big endian. Covers the whole of Unicode.
    Ucs2,
}

/// The two GSM 7-bit layout characteristics, re-exported from
/// [`smpp_core::values`].
///
/// [`Gsm7BitPacking`] was defined here at milestone 004. It moved down to
/// `smpp-core` at milestone 005, when [`Gsm7BitCharset`] joined it: both are
/// decided by the **message centre**, so both are fields of the session
/// profile — and the profile lives in `smpp-session`, a layer *below* this
/// one. A value the profile carries and this crate applies has to sit under
/// both. The re-export keeps the milestone-004 paths working.
pub use smpp_core::values::{Gsm7BitCharset, Gsm7BitPacking};

impl Encoding {
    /// The `data_coding` value of spec §7.5 for this encoding.
    ///
    /// GSM 7-bit maps to `0x00`, which `rusmpp` names `McSpecific`: `0x00`
    /// means "MC specific default alphabet", and on the GSM networks this
    /// client targets that default *is* GSM 03.38.
    #[must_use]
    pub const fn data_coding(self) -> DataCoding {
        match self {
            Self::Gsm7Bit => DataCoding::McSpecific,
            Self::Latin1 => DataCoding::Latin1,
            Self::Ucs2 => DataCoding::Ucs2,
        }
    }

    /// How much fits in one segment, and in what unit.
    #[must_use]
    pub const fn budget(self) -> SegmentBudget {
        match self {
            Self::Gsm7Bit => SegmentBudget {
                unit: BudgetUnit::Septets,
                single: 160,
                concatenated: 153,
            },
            Self::Latin1 => SegmentBudget {
                unit: BudgetUnit::Octets,
                single: 140,
                concatenated: 134,
            },
            Self::Ucs2 => SegmentBudget {
                unit: BudgetUnit::Utf16CodeUnits,
                single: 70,
                concatenated: 67,
            },
        }
    }

    /// Units `character` costs, or `None` when this encoding cannot write it.
    #[must_use]
    pub fn unit_cost(self, character: char) -> Option<usize> {
        match self {
            Self::Gsm7Bit => gsm0338::septet_cost(character),
            Self::Latin1 => latin1::octet_cost(character),
            Self::Ucs2 => Some(ucs2::code_unit_cost(character)),
        }
    }

    /// Whether every character of `text` can be written in this encoding.
    #[must_use]
    pub fn can_represent(self, text: &str) -> bool {
        match self {
            Self::Gsm7Bit => gsm0338::is_representable(text),
            Self::Latin1 => latin1::is_representable(text),
            Self::Ucs2 => true,
        }
    }
}

impl core::fmt::Display for Encoding {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let label = match self {
            Self::Gsm7Bit => "GSM 7-bit",
            Self::Latin1 => "Latin-1",
            Self::Ucs2 => "UCS2",
        };

        formatter.write_str(label)
    }
}

/// What a [`SegmentBudget`] counts.
///
/// Exists so a number can never be read in the wrong unit: a GSM budget of 160
/// is 160 *septets*, and a septet is not a character — `€` is one character
/// and two septets. Counting characters against it lets a message overflow by
/// as much as it contains extended characters, and the overflow only shows up
/// on a handset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetUnit {
    /// GSM 03.38 septets, escapes expanded, whatever the packing.
    Septets,
    /// UTF-16 code units — two for a character outside the basic plane.
    Utf16CodeUnits,
    /// Plain octets.
    Octets,
}

/// How much user data one segment carries, in the unit of its encoding.
///
/// The two figures come straight from the table in spec §7.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SegmentBudget {
    unit: BudgetUnit,
    single: usize,
    concatenated: usize,
}

impl SegmentBudget {
    /// The unit both figures are counted in.
    #[must_use]
    pub const fn unit(self) -> BudgetUnit {
        self.unit
    }

    /// Capacity of a message that fits in one segment, header-free.
    #[must_use]
    pub const fn single(self) -> usize {
        self.single
    }

    /// Capacity of one segment of a concatenated message.
    ///
    /// The shortfall against [`Self::single`] is the room the six-octet
    /// concatenation UDH takes out of the body.
    ///
    /// It applies to the `sar_*` mode too, where the segment body carries no
    /// header at all and could in principle hold the full [`Self::single`].
    /// That extra capacity is not claimed, deliberately: message centres
    /// routinely translate `sar_*` into a UDH on the delivery leg, and a
    /// segment sized for 160 septets would then no longer fit. Spec §7.5
    /// states the figure as a property of the encoding, and CA-004-01 and
    /// CA-004-03 name no mode.
    #[must_use]
    pub const fn concatenated(self) -> usize {
        self.concatenated
    }

    /// The capacity that applies for a message split into `segments` parts.
    #[must_use]
    pub const fn for_segment_count(self, segments: usize) -> usize {
        if segments <= 1 {
            self.single
        } else {
            self.concatenated
        }
    }
}

/// Whether the encoding is chosen for the user or by the user.
///
/// Spec §7.5 step 3. The distinction matters beyond ergonomics: under
/// [`Self::Automatic`] an unrepresentable character silently widens the
/// encoding, whereas under [`Self::Forced`] it is an error — never a corrupted
/// character on the handset (CA-004-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EncodingChoice {
    /// GSM 7-bit when the text allows, UCS2 otherwise.
    #[default]
    Automatic,
    /// This encoding, or an error.
    Forced(Encoding),
}

/// The encoding [`EncodingChoice::Automatic`] picks for `text`.
///
/// Spec §7.5: GSM 7-bit when every character is in the GSM 03.38 alphabet,
/// extension table included; UCS2 otherwise. Latin-1 is never chosen on its
/// own — it represents strictly less than UCS2 for the same 140 octets.
#[must_use]
pub fn detect(text: &str) -> Encoding {
    if gsm0338::is_representable(text) {
        Encoding::Gsm7Bit
    } else {
        Encoding::Ucs2
    }
}

/// Settles [`EncodingChoice`] against `text`.
///
/// # Errors
///
/// [`EncodingError::UnrepresentableCharacter`] when a forced encoding cannot
/// write some character. Never fails on [`EncodingChoice::Automatic`].
pub fn resolve(choice: EncodingChoice, text: &str) -> Result<Encoding, EncodingError> {
    let EncodingChoice::Forced(encoding) = choice else {
        return Ok(detect(text));
    };

    if let Some((index, character)) = text
        .chars()
        .enumerate()
        .find(|(_, character)| encoding.unit_cost(*character).is_none())
    {
        return Err(EncodingError::UnrepresentableCharacter {
            character,
            index,
            encoding,
        });
    }

    Ok(encoding)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §7.5 table, transcribed. If these drift the segment counts do too.
    #[test]
    fn the_budgets_match_the_specification_table() {
        let gsm = Encoding::Gsm7Bit.budget();
        assert_eq!(
            (gsm.unit(), gsm.single(), gsm.concatenated()),
            (BudgetUnit::Septets, 160, 153)
        );

        let latin1 = Encoding::Latin1.budget();
        assert_eq!(
            (latin1.unit(), latin1.single(), latin1.concatenated()),
            (BudgetUnit::Octets, 140, 134)
        );

        let ucs2 = Encoding::Ucs2.budget();
        assert_eq!(
            (ucs2.unit(), ucs2.single(), ucs2.concatenated()),
            (BudgetUnit::Utf16CodeUnits, 70, 67)
        );
    }

    #[test]
    fn the_data_coding_octets_match_the_specification_table() {
        assert_eq!(u8::from(Encoding::Gsm7Bit.data_coding()), 0x00);
        assert_eq!(u8::from(Encoding::Latin1.data_coding()), 0x03);
        assert_eq!(u8::from(Encoding::Ucs2.data_coding()), 0x08);
    }

    #[test]
    fn detection_keeps_gsm_for_the_whole_gsm_alphabet_extension_included() {
        assert_eq!(detect("Il faut regler 10 EUR"), Encoding::Gsm7Bit);
        // Extension-table characters do not force a widening, they only cost
        // two septets each.
        assert_eq!(detect("{prix} = 10€ [TTC]"), Encoding::Gsm7Bit);
        // Accents the base table happens to carry.
        assert_eq!(detect("Ça coûte cher"), Encoding::Ucs2);
        assert_eq!(detect("Éèùìòøåäöñüà"), Encoding::Gsm7Bit);
        assert_eq!(detect(""), Encoding::Gsm7Bit);
    }

    #[test]
    fn detection_switches_to_ucs2_on_the_first_foreign_character() {
        assert_eq!(detect("你好"), Encoding::Ucs2);
        assert_eq!(detect("Łódź"), Encoding::Ucs2);
        assert_eq!(detect("a\u{1F600}"), Encoding::Ucs2);
    }

    #[test]
    fn a_forced_encoding_that_cannot_write_the_text_is_an_error_not_a_fallback() {
        assert_eq!(
            resolve(EncodingChoice::Forced(Encoding::Gsm7Bit), "ab你"),
            Err(EncodingError::UnrepresentableCharacter {
                character: '你',
                index: 2,
                encoding: Encoding::Gsm7Bit,
            })
        );

        assert_eq!(
            resolve(EncodingChoice::Forced(Encoding::Latin1), "ab你"),
            Err(EncodingError::UnrepresentableCharacter {
                character: '你',
                index: 2,
                encoding: Encoding::Latin1,
            })
        );
    }

    #[test]
    fn a_forced_encoding_wins_over_detection() {
        // Plain ASCII would be detected as GSM 7-bit.
        assert_eq!(
            resolve(EncodingChoice::Forced(Encoding::Ucs2), "abc"),
            Ok(Encoding::Ucs2)
        );
        assert_eq!(
            resolve(EncodingChoice::Forced(Encoding::Latin1), "abc"),
            Ok(Encoding::Latin1)
        );
    }

    /// Latin-1 and GSM 7-bit do not cover the same characters, in either
    /// direction: `é` is in both, `€` is GSM-only, `ç` is Latin-1 only.
    #[test]
    fn the_two_narrow_encodings_do_not_nest() {
        assert!(Encoding::Gsm7Bit.can_represent("€"));
        assert!(!Encoding::Latin1.can_represent("€"));
        assert!(Encoding::Latin1.can_represent("ç"));
        assert!(!Encoding::Gsm7Bit.can_represent("ç"));
    }

    #[test]
    fn the_automatic_choice_never_fails() {
        assert_eq!(
            resolve(EncodingChoice::Automatic, "你好"),
            Ok(Encoding::Ucs2)
        );
        assert_eq!(
            resolve(EncodingChoice::default(), "abc"),
            Ok(Encoding::Gsm7Bit)
        );
    }
}
