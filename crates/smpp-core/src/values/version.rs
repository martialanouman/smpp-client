//! The SMPP interface version a session negotiates.

use rusmpp::{values::InterfaceVersion, CommandId};

/// The two SMPP versions ShinobiSMPP speaks.
///
/// Spec §7.7: the version is chosen **per session** and announced in the bind.
/// [`InterfaceVersion`] is the wire type and can hold any octet, including the
/// pre-3.4 versions and vendor values this application does not support;
/// `SmppVersion` is the domain type, and an unsupported version is
/// unrepresentable in it (*parse, don't validate*, guide §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum SmppVersion {
    /// SMPP v3.4, announced as `interface_version` `0x34`.
    V3_4,
    /// SMPP v5.0, announced as `interface_version` `0x50`.
    ///
    /// The default: v5.0 is backward compatible with v3.4, and an SMSC that
    /// only speaks v3.4 answers the bind with the version it supports.
    #[default]
    V5_0,
}

impl SmppVersion {
    /// The octet carried by the `interface_version` field of a bind PDU.
    #[must_use]
    pub const fn as_octet(self) -> u8 {
        match self {
            Self::V3_4 => 0x34,
            Self::V5_0 => 0x50,
        }
    }

    /// Whether an operation exists in this version of the specification.
    ///
    /// Spec §7.2 marks the three broadcast operations as v5.0 only. Sending one
    /// over a v3.4 bind earns an `ESME_RINVCMDID` at best; the UI hides them
    /// (§7.7), and this is the check behind that.
    ///
    /// Unknown command ids are reported as supported: only the SMSC can settle
    /// a vendor-specific operation, and refusing it here would be guessing.
    #[must_use]
    pub const fn supports(self, command_id: CommandId) -> bool {
        match self {
            Self::V5_0 => true,
            Self::V3_4 => !matches!(
                command_id,
                CommandId::BroadcastSm
                    | CommandId::BroadcastSmResp
                    | CommandId::QueryBroadcastSm
                    | CommandId::QueryBroadcastSmResp
                    | CommandId::CancelBroadcastSm
                    | CommandId::CancelBroadcastSmResp
            ),
        }
    }

    /// Human-readable label, as displayed in session profiles (spec §8.2).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::V3_4 => "v3.4",
            Self::V5_0 => "v5.0",
        }
    }
}

impl From<SmppVersion> for InterfaceVersion {
    fn from(value: SmppVersion) -> Self {
        match value {
            SmppVersion::V3_4 => Self::Smpp3_4,
            SmppVersion::V5_0 => Self::Smpp5_0,
        }
    }
}

impl From<SmppVersion> for u8 {
    fn from(value: SmppVersion) -> Self {
        value.as_octet()
    }
}

/// An `interface_version` octet this application does not speak.
///
/// Returned by `SmppVersion::try_from`; carries the offending octet so the
/// caller can report it (guide §6.3: never lose the origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unsupported SMPP interface version: {octet:#04X}")]
pub struct UnsupportedInterfaceVersion {
    /// The rejected `interface_version` octet.
    pub octet: u8,
}

impl TryFrom<InterfaceVersion> for SmppVersion {
    type Error = UnsupportedInterfaceVersion;

    fn try_from(value: InterfaceVersion) -> Result<Self, Self::Error> {
        match value {
            InterfaceVersion::Smpp3_4 => Ok(Self::V3_4),
            InterfaceVersion::Smpp5_0 => Ok(Self::V5_0),
            other => Err(UnsupportedInterfaceVersion {
                octet: u8::from(other),
            }),
        }
    }
}

impl TryFrom<u8> for SmppVersion {
    type Error = UnsupportedInterfaceVersion;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::try_from(InterfaceVersion::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsupported_version_reports_the_offending_octet() {
        let error = SmppVersion::try_from(0x33u8).expect_err("0x33 is SMPP 3.3");

        assert_eq!(error.octet, 0x33);
        assert_eq!(
            error.to_string(),
            "unsupported SMPP interface version: 0x33"
        );
    }

    #[test]
    fn every_octet_either_parses_or_is_rejected_without_panicking() {
        for octet in u8::MIN..=u8::MAX {
            match SmppVersion::try_from(octet) {
                Ok(version) => assert_eq!(version.as_octet(), octet),
                Err(error) => assert_eq!(error.octet, octet),
            }
        }
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(SmppVersion::V3_4.label(), "v3.4");
        assert_eq!(SmppVersion::V5_0.label(), "v5.0");
    }
}
