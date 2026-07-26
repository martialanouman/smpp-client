//! How GSM 7-bit user data is laid out in the octets of `short_message`.
//!
//! Two independent choices live here, and both are **characteristics of the
//! session**, not of the message: the message centre decides, and it never
//! tells us. They sit in `smpp-core` rather than in `messaging` because the
//! session profile of `smpp-session` carries them and `messaging` applies
//! them — a value both layers need belongs under both.
//!
//! # Why these are choices at all, and why getting them wrong is silent
//!
//! Neither mistake produces an error anywhere. The message centre answers
//! `ESME_ROK`, the delivery receipt says `DELIVRD`, and the handset shows
//! gibberish. Nothing ever comes back. Worse, **plain ASCII survives both
//! choices unchanged**: a test suite written on `"hello"` stays green under
//! every combination, and only `@ £ $ €` and the accented letters break, in
//! production, for the customers whose language uses them.
//!
//! That is why they are configured rather than guessed, and why the tests
//! that cover them are written on those exact characters.

/// How GSM 7-bit septets are packed into the octets of `short_message`.
///
/// GSM 03.38 §6.1.2.1.1 describes the **over-the-air** format, where eight
/// septets are squeezed into seven octets. That format applies between the
/// message centre and the handset — *not* to the `short_message` field of a
/// `submit_sm`. On the SMPP link the near-universal convention is the opposite
/// one: one septet per octet, high bit clear, with the message centre packing
/// before the radio interface. Kannel, Jasmin, CloudHopper and the large
/// commercial aggregators all expect that. Packing on the SMPP link exists —
/// some ZTE and legacy operator equipment — but it is the documented
/// exception.
///
/// Settled by ADR 0008.
///
/// # Deliberately not `#[non_exhaustive]`
///
/// Every other enum of this crate carries it. This one must not: a third
/// layout convention appearing would have to be handled *everywhere octets
/// are written*, and `#[non_exhaustive]` would turn each of those sites into a
/// wildcard arm that quietly keeps doing the old thing. A compile error at
/// each one is the outcome we want.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Gsm7BitPacking {
    /// One septet per octet, high bit clear. The default and the common case.
    ///
    /// `sm_length` counts one octet per septet, so a full single segment is
    /// 160 octets — well within the 254 the field allows. The message centre
    /// packs before the radio interface.
    #[default]
    Unpacked,
    /// Eight septets in seven octets, as GSM 03.38 §6.1.2.1.1 packs them.
    ///
    /// For the message centres that require it. Two consequences worth knowing
    /// before turning it on:
    ///
    /// * `sm_length` is in octets, so the receiver has to *recompute* the
    ///   septet count from it. The segmenter therefore refuses to close a
    ///   non-final segment on a count that would not come back exactly.
    /// * on the **last** segment the recomputation can still yield one septet
    ///   too many, which the padding makes a carriage return. That case is
    ///   unavoidable — there is no later segment to push a character into —
    ///   and it is the one TS 23.038 §6.1.2.3.1 covers by prescribing `CR` as
    ///   the pad value precisely so it stays harmless.
    Packed,
}

impl Gsm7BitPacking {
    /// A stable machine-readable name, for storage and the IPC contract.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unpacked => "unpacked",
            Self::Packed => "packed",
        }
    }

    /// Parses the stored form back.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "unpacked" => Some(Self::Unpacked),
            "packed" => Some(Self::Packed),
            _ => None,
        }
    }
}

/// What the octets of an **unpacked** GSM 7-bit body actually mean.
///
/// The packing question does not exhaust the matter. Two conventions coexist
/// for the *value* of each unpacked octet, and again the message centre
/// decides:
///
/// * [`Self::Gsm0338`] — the octets are GSM 03.38 alphabet positions. `@` is
///   `0x00`, `é` is `0x05`, `€` is the pair `0x1B 0x65`. The client writes the
///   alphabet the handset will display.
/// * [`Self::Latin1`] — the octets are ISO-8859-1 code points and the message
///   centre transcodes to GSM 03.38 itself. `@` is `0x40`, `é` is `0xE9`,
///   `£` is `0xA3`. This is what Kannel calls `alt-charset`, and what a
///   `smsbox` configured with `alt-charset = "ISO-8859-1"` expects.
///
/// # The trap
///
/// The two conventions **agree on every printable ASCII character**: `A` is
/// `0x41` in both, `1` is `0x31` in both. A message written in English travels
/// identically through either, so a test suite, an acceptance run and a pilot
/// customer can all pass without touching the difference. The divergence is
/// exactly the set `@ £ $ ¥ è é ù ì ò Ç Ø ø Å å Æ æ ß É ¤ ¡ Ä Ö Ñ Ü § ¿ ä ö ñ
/// ü à` and the extension table — that is, the currency signs and the accented
/// letters, which is to say everything a French, Spanish or German message
/// contains and an English one does not.
///
/// Settled by ADR 0009; ADR 0008 left it open and named it explicitly.
///
/// Not `#[non_exhaustive]`, for the reason given on [`Gsm7BitPacking`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Gsm7BitCharset {
    /// The octets are GSM 03.38 alphabet positions. The default.
    ///
    /// The protocol-faithful reading: `data_coding` `0x00` means "MC specific
    /// default alphabet", and on the GSM networks this client targets that
    /// default *is* GSM 03.38.
    #[default]
    Gsm0338,
    /// The octets are ISO-8859-1 code points; the message centre transcodes.
    ///
    /// Turn this on for a message centre configured the Kannel way. Note that
    /// it narrows what can be sent: `€` has no ISO-8859-1 code point, so a
    /// message containing one is refused rather than mangled.
    Latin1,
}

impl Gsm7BitCharset {
    /// A stable machine-readable name, for storage and the IPC contract.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Gsm0338 => "gsm0338",
            Self::Latin1 => "latin1",
        }
    }

    /// Parses the stored form back.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "gsm0338" => Some(Self::Gsm0338),
            "latin1" => Some(Self::Latin1),
            _ => None,
        }
    }

    /// Whether this reading is compatible with `packing`.
    ///
    /// It is not, in one combination: [`Self::Latin1`] octets use the full
    /// eight bits — `é` is `0xE9` — and packing throws the top bit of every
    /// one of them away. The result is not "slightly wrong", it is
    /// unrecoverable, so the profile refuses the pair rather than shipping it.
    #[must_use]
    pub const fn is_compatible_with(self, packing: Gsm7BitPacking) -> bool {
        !matches!((self, packing), (Self::Latin1, Gsm7BitPacking::Packed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_what_the_installed_base_expects() {
        assert_eq!(Gsm7BitPacking::default(), Gsm7BitPacking::Unpacked);
        assert_eq!(Gsm7BitCharset::default(), Gsm7BitCharset::Gsm0338);
    }

    #[test]
    fn the_stored_forms_round_trip() {
        for packing in [Gsm7BitPacking::Unpacked, Gsm7BitPacking::Packed] {
            assert_eq!(Gsm7BitPacking::parse(packing.code()), Some(packing));
        }

        for charset in [Gsm7BitCharset::Gsm0338, Gsm7BitCharset::Latin1] {
            assert_eq!(Gsm7BitCharset::parse(charset.code()), Some(charset));
        }

        assert_eq!(Gsm7BitPacking::parse("PACKED"), None);
        assert_eq!(Gsm7BitCharset::parse(""), None);
    }

    /// Eight-bit octets cannot survive being packed seven bits at a time.
    #[test]
    fn latin1_octets_and_septet_packing_are_mutually_exclusive() {
        assert!(!Gsm7BitCharset::Latin1.is_compatible_with(Gsm7BitPacking::Packed));
        assert!(Gsm7BitCharset::Latin1.is_compatible_with(Gsm7BitPacking::Unpacked));
        assert!(Gsm7BitCharset::Gsm0338.is_compatible_with(Gsm7BitPacking::Packed));
        assert!(Gsm7BitCharset::Gsm0338.is_compatible_with(Gsm7BitPacking::Unpacked));
    }
}
