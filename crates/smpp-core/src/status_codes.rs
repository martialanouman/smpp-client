//! The `command_status` table.
//!
//! Spec §7.6 requires the application to display `command_status` codes **in
//! plain language**, and milestone 003 §6 requires them to be held as *data*
//! rather than scattered `match` arms: the labels feed the UI (ENF-UTI-02) and
//! must stay translatable.
//!
//! Each entry also carries a [`StatusClass`]. That classification is not
//! decorative — it is read by:
//!
//! * milestone 005, to stop the reconnection loop on a fatal bind error rather
//!   than hammering an SMSC that will keep refusing the credentials;
//! * milestone 007, to cut the rate when the SMSC signals congestion;
//! * milestones 010 and 012, to decide whether a failed message may be
//!   replayed.
//!
//! The class answers one question, always relative to the operation that was
//! answered: **may this request be sent again as is?** For a bind, "no" means
//! do not loop. For a `submit_sm`, "no" means do not replay the message.

use crate::values::CommandStatus;

/// How the engine must react to a `command_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StatusClass {
    /// `ESME_ROK`: the request succeeded.
    Success,
    /// The request failed and repeating it identically cannot succeed:
    /// wrong credentials, malformed field, unroutable destination, denied
    /// operation. No reconnection loop, no replay.
    Fatal,
    /// The request failed for a reason the SMSC presents as transient: system
    /// error, generic submission failure, temporary application error. A replay
    /// with back-off is legitimate.
    Recoverable,
    /// The SMSC is refusing the *rate*, not the request: the message may be
    /// replayed, but only after slowing down (spec §9.4).
    Throttling,
}

impl StatusClass {
    /// Whether the request may be sent again.
    ///
    /// [`StatusClass::Success`] answers `false`: there is nothing to replay.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Recoverable | Self::Throttling)
    }

    /// Whether the rate limiter must be reduced before any replay.
    #[must_use]
    pub const fn requires_slowdown(self) -> bool {
        matches!(self, Self::Throttling)
    }

    /// Whether the failure is definitive for the operation that carried it.
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(self, Self::Fatal)
    }
}

/// One row of the `command_status` table.
///
/// The French and English labels are user-facing data, which is why they live
/// here rather than in the i18n catalogue: they are indexed by a protocol value
/// and must stay next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct StatusCode {
    /// The value carried by the `command_status` header field.
    pub value: u32,
    /// The symbolic name of the specification, e.g. `ESME_RTHROTTLED`.
    pub symbol: &'static str,
    /// French label, the application's default language.
    pub label_fr: &'static str,
    /// English label.
    pub label_en: &'static str,
    /// How the engine must react.
    pub class: StatusClass,
}

/// Applies to every status the table does not list: the SMPP extension range
/// (0x00000113-0x000003FF) and the vendor range (0x00000400-0x000004FF).
///
/// [`StatusClass::Fatal`] is the conservative choice. Reading an unknown code
/// as replayable would turn a rejection nobody understands into a loop against
/// the SMSC; reading it as fatal costs, at worst, one message that a human can
/// requeue.
const UNKNOWN_STATUS_CLASS: StatusClass = StatusClass::Fatal;

/// First value of the vendor-specific range (spec: MC vendor specific errors).
const VENDOR_RANGE_START: u32 = 0x0000_0400;
/// Last value of the vendor-specific range.
const VENDOR_RANGE_END: u32 = 0x0000_04FF;

/// All the standard statuses of SMPP v3.4 and v5.0, sorted by value.
///
/// Ordering is load-bearing: [`describe_value`] binary-searches it, and a test
/// fails if the invariant is broken.
static STATUS_CODES: &[StatusCode] = &[
    row(
        0x0000_0000,
        "ESME_ROK",
        "Succès",
        "No error",
        StatusClass::Success,
    ),
    row(
        0x0000_0001,
        "ESME_RINVMSGLEN",
        "Longueur de message invalide",
        "Message length is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0002,
        "ESME_RINVCMDLEN",
        "Longueur de commande invalide",
        "Command length is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0003,
        "ESME_RINVCMDID",
        "Identifiant de commande invalide",
        "Invalid command ID",
        StatusClass::Fatal,
    ),
    // 0x04 and 0x05 describe a TRANSIENT DISAGREEMENT ABOUT SESSION STATE, not
    // a malformed request. Classifying them `Fatal` produces exactly the two
    // failures the classification exists to prevent:
    //
    //   * 0x04 — a `submit_sm` in flight crosses an `unbind` from the SMSC.
    //     `Fatal` makes `is_retryable()` false, so milestone 010 marks the
    //     message permanently failed instead of replaying it after a rebind.
    //
    //   * 0x05 — the TCP connection drops without `unbind`; we reconnect
    //     before the SMSC has reaped the stale session. `Fatal` makes
    //     milestone 005 abandon the reconnection loop, leaving the session
    //     dead until a human intervenes — while a retry thirty seconds later
    //     would have succeeded. This is the most ordinary SMPP failure there
    //     is.
    //
    // `Recoverable`, not `Throttling`: the retry must go through the normal
    // back-off, because 0x05 keeps being returned for as long as the stale
    // session lives.
    row(
        0x0000_0004,
        "ESME_RINVBNDSTS",
        "État de bind incorrect pour cette commande",
        "Incorrect bind status for given command",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0005,
        "ESME_RALYBND",
        "Session déjà liée",
        "ESME already in bound state",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0006,
        "ESME_RINVPRTFLG",
        "Indicateur de priorité invalide",
        "Invalid priority flag",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0007,
        "ESME_RINVREGDLVFLG",
        "Indicateur d'accusé de livraison invalide",
        "Invalid registered delivery flag",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0008,
        "ESME_RSYSERR",
        "Erreur système du SMSC",
        "System error",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_000A,
        "ESME_RINVSRCADR",
        "Adresse source invalide",
        "Invalid source address",
        StatusClass::Fatal,
    ),
    row(
        0x0000_000B,
        "ESME_RINVDSTADR",
        "Adresse destinataire invalide",
        "Invalid destination address",
        StatusClass::Fatal,
    ),
    row(
        0x0000_000C,
        "ESME_RINVMSGID",
        "Identifiant de message invalide",
        "Message ID is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_000D,
        "ESME_RBINDFAIL",
        "Échec du bind",
        "Bind failed",
        StatusClass::Fatal,
    ),
    row(
        0x0000_000E,
        "ESME_RINVPASWD",
        "Mot de passe invalide",
        "Invalid password",
        StatusClass::Fatal,
    ),
    row(
        0x0000_000F,
        "ESME_RINVSYSID",
        "Identifiant système invalide",
        "Invalid system ID",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0011,
        "ESME_RCANCELFAIL",
        "Échec de l'annulation",
        "Cancel SM failed",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0013,
        "ESME_RREPLACEFAIL",
        "Échec du remplacement",
        "Replace SM failed",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0014,
        "ESME_RMSGQFUL",
        "File de messages du SMSC pleine",
        "Message queue full",
        StatusClass::Throttling,
    ),
    row(
        0x0000_0015,
        "ESME_RINVSERTYP",
        "Type de service invalide",
        "Invalid service type",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0033,
        "ESME_RINVNUMDESTS",
        "Nombre de destinataires invalide",
        "Invalid number of destinations",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0034,
        "ESME_RINVDLNAME",
        "Nom de liste de diffusion invalide",
        "Invalid distribution list name",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0040,
        "ESME_RINVDESTFLAG",
        "Indicateur de destination invalide",
        "Destination flag is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0042,
        "ESME_RINVSUBREP",
        "Soumission avec remplacement non autorisée",
        "Submit with replace is not supported or allowed",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0043,
        "ESME_RINVESMCLASS",
        "Champ esm_class invalide",
        "Invalid esm_class field data",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0044,
        "ESME_RCNTSUBDL",
        "Soumission à une liste de diffusion impossible",
        "Cannot submit to distribution list",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0045,
        "ESME_RSUBMITFAIL",
        "Échec de la soumission",
        "Submit failed",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0048,
        "ESME_RINVSRCTON",
        "TON source invalide",
        "Invalid source address TON",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0049,
        "ESME_RINVSRCNPI",
        "NPI source invalide",
        "Invalid source address NPI",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0050,
        "ESME_RINVDSTTON",
        "TON destinataire invalide",
        "Invalid destination address TON",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0051,
        "ESME_RINVDSTNPI",
        "NPI destinataire invalide",
        "Invalid destination address NPI",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0053,
        "ESME_RINVSYSTYP",
        "Champ system_type invalide",
        "Invalid system_type field",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0054,
        "ESME_RINVREPFLAG",
        "Indicateur replace_if_present invalide",
        "Invalid replace_if_present flag",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0055,
        "ESME_RINVNUMMSGS",
        "Nombre de messages invalide",
        "Invalid number of messages",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0058,
        "ESME_RTHROTTLED",
        "Débit autorisé dépassé",
        "Throttling error, message limits exceeded",
        StatusClass::Throttling,
    ),
    row(
        0x0000_0061,
        "ESME_RINVSCHED",
        "Heure de livraison programmée invalide",
        "Invalid scheduled delivery time",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0062,
        "ESME_RINVEXPIRY",
        "Période de validité invalide",
        "Invalid message validity period",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0063,
        "ESME_RINVDFTMSGID",
        "Identifiant de message prédéfini invalide",
        "Predefined message ID is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0064,
        "ESME_RX_T_APPN",
        "Erreur applicative temporaire côté récepteur",
        "Receiver temporary application error",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0065,
        "ESME_RX_P_APPN",
        "Erreur applicative permanente côté récepteur",
        "Receiver permanent application error",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0066,
        "ESME_RX_R_APPN",
        "Message rejeté par le récepteur",
        "Receiver reject message error",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0067,
        "ESME_RQUERYFAIL",
        "Échec de l'interrogation d'état",
        "Query request failed",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_00C0,
        "ESME_RINVTLVSTREAM",
        "Erreur dans la partie optionnelle du PDU",
        "Error in the optional part of the PDU body",
        StatusClass::Fatal,
    ),
    row(
        0x0000_00C1,
        "ESME_RTLVNOTALLWD",
        "TLV non autorisé dans ce contexte",
        "TLV not allowed",
        StatusClass::Fatal,
    ),
    row(
        0x0000_00C2,
        "ESME_RINVTLVLEN",
        "Longueur de TLV invalide",
        "Invalid parameter length",
        StatusClass::Fatal,
    ),
    row(
        0x0000_00C3,
        "ESME_RMISSINGTLV",
        "TLV obligatoire absent",
        "Expected TLV missing",
        StatusClass::Fatal,
    ),
    row(
        0x0000_00C4,
        "ESME_RINVTLVVAL",
        "Valeur de TLV invalide",
        "Invalid TLV value",
        StatusClass::Fatal,
    ),
    row(
        0x0000_00FE,
        "ESME_RDELIVERYFAILURE",
        "Échec de livraison en mode transaction",
        "Transaction delivery failure",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_00FF,
        "ESME_RUNKNOWNERR",
        "Erreur inconnue du SMSC",
        "Unknown error",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0100,
        "ESME_RSERTYPUNAUTH",
        "Type de service non autorisé pour cet ESME",
        "ESME not authorised to use specified service_type",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0101,
        "ESME_RPROHIBITED",
        "Opération interdite à cet ESME",
        "ESME prohibited from using specified operation",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0102,
        "ESME_RSERTYPUNAVAIL",
        "Type de service indisponible",
        "Specified service_type is unavailable",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0103,
        "ESME_RSERTYPDENIED",
        "Type de service refusé pour ce contenu",
        "Specified service_type is denied",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0104,
        "ESME_RINVDCS",
        "Schéma d'encodage (DCS) invalide",
        "Invalid data coding scheme",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0105,
        "ESME_RINVSRCADDRSUBUNIT",
        "Sous-unité d'adresse source invalide",
        "Source address sub-unit is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0106,
        "ESME_RINVDSTADDRSUBUNIT",
        "Sous-unité d'adresse destinataire invalide",
        "Destination address sub-unit is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0107,
        "ESME_RINVBCASTFREQINT",
        "Intervalle de répétition de diffusion invalide",
        "Broadcast frequency interval is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0108,
        "ESME_RINVBCASTALIAS_NAME",
        "Alias de diffusion invalide",
        "Broadcast alias name is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0109,
        "ESME_RINVBCASTAREAFMT",
        "Format de zone de diffusion invalide",
        "Broadcast area format is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_010A,
        "ESME_RINVNUMBCAST_AREAS",
        "Nombre de zones de diffusion invalide",
        "Number of broadcast areas is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_010B,
        "ESME_RINVBCASTCNTTYPE",
        "Type de contenu de diffusion invalide",
        "Broadcast content type is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_010C,
        "ESME_RINVBCASTMSGCLASS",
        "Classe de message de diffusion invalide",
        "Broadcast message class is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_010D,
        "ESME_RBCASTFAIL",
        "Échec de l'opération de diffusion",
        "broadcast_sm operation failed",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_010E,
        "ESME_RBCASTQUERYFAIL",
        "Échec de l'interrogation de diffusion",
        "query_broadcast_sm operation failed",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_010F,
        "ESME_RBCASTCANCELFAIL",
        "Échec de l'annulation de diffusion",
        "cancel_broadcast_sm operation failed",
        StatusClass::Recoverable,
    ),
    row(
        0x0000_0110,
        "ESME_RINVBCAST_REP",
        "Nombre de répétitions de diffusion invalide",
        "Number of repeated broadcasts is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0111,
        "ESME_RINVBCASTSRVGRP",
        "Groupe de service de diffusion invalide",
        "Broadcast service group is invalid",
        StatusClass::Fatal,
    ),
    row(
        0x0000_0112,
        "ESME_RINVBCASTCHANIND",
        "Indicateur de canal de diffusion invalide",
        "Broadcast channel indicator is invalid",
        StatusClass::Fatal,
    ),
];

/// Shorthand keeping the table above readable; `const` so the whole table is
/// built at compile time.
const fn row(
    value: u32,
    symbol: &'static str,
    label_fr: &'static str,
    label_en: &'static str,
    class: StatusClass,
) -> StatusCode {
    StatusCode {
        value,
        symbol,
        label_fr,
        label_en,
        class,
    }
}

/// The whole table, sorted by value.
///
/// Intended for the UI, which lists the codes and their meaning (spec §7.6).
#[must_use]
pub fn all() -> &'static [StatusCode] {
    STATUS_CODES
}

/// Looks up the description of a raw `command_status` value.
///
/// Returns `None` for the extension and vendor ranges, which no specification
/// documents — see [`is_vendor_specific`].
#[must_use]
pub fn describe_value(value: u32) -> Option<&'static StatusCode> {
    STATUS_CODES
        .binary_search_by_key(&value, |entry| entry.value)
        .ok()
        .and_then(|index| STATUS_CODES.get(index))
}

/// Looks up the description of a decoded [`CommandStatus`].
#[must_use]
pub fn describe(status: CommandStatus) -> Option<&'static StatusCode> {
    describe_value(u32::from(status))
}

/// How the engine must react to this status.
///
/// A status outside the table falls back to [`StatusClass::Fatal`]; that is the
/// only default, and it never applies to a standard code — a test enforces it.
#[must_use]
pub fn classify(status: CommandStatus) -> StatusClass {
    describe(status).map_or(UNKNOWN_STATUS_CLASS, |entry| entry.class)
}

/// Whether a value falls in the range the specification reserves for
/// SMSC-vendor-specific errors (0x00000400-0x000004FF).
///
/// Worth surfacing in the UI: such a code is not a bug in the application, it
/// is a code only the operator's documentation explains.
#[must_use]
pub const fn is_vendor_specific(value: u32) -> bool {
    value >= VENDOR_RANGE_START && value <= VENDOR_RANGE_END
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::CommandStatus;

    /// Highest standard `command_status` (spec: 0x00000113 onwards is reserved
    /// for SMPP extensions, 0x00000400-0x000004FF for vendor errors).
    const SCAN_UPPER_BOUND: u32 = 0x0000_0200;

    /// CA-003-05 — exhaustiveness, direction 1: every status `rusmpp` names is
    /// described here. `rusmpp`'s enum is an independent transcription of the
    /// specification, so it is a real cross-check and not a tautology.
    #[test]
    fn every_named_status_of_the_specification_is_described() {
        let mut missing = Vec::new();
        let mut named = 0usize;

        for value in 0..=SCAN_UPPER_BOUND {
            let status = CommandStatus::from(value);

            if matches!(status, CommandStatus::Other(_)) {
                continue;
            }

            named += 1;

            if describe(status).is_none() {
                missing.push(format!("{value:#010X}"));
            }
        }

        assert!(
            missing.is_empty(),
            "statuses missing from the table: {missing:?}"
        );
        // Guards against a scan that would silently find nothing to check.
        assert_eq!(
            named,
            all().len(),
            "the table and the codec do not agree on how many statuses exist"
        );
    }

    /// CA-003-05 — exhaustiveness, direction 2: the table invents nothing.
    #[test]
    fn every_described_status_is_a_status_of_the_specification() {
        for entry in all() {
            assert!(
                !matches!(CommandStatus::from(entry.value), CommandStatus::Other(_)),
                "{} ({:#010X}) is unknown to the codec",
                entry.symbol,
                entry.value
            );
        }
    }

    #[test]
    fn the_table_has_no_duplicate_and_is_sorted_by_value() {
        let values: Vec<u32> = all().iter().map(|entry| entry.value).collect();
        let mut sorted = values.clone();
        sorted.sort_unstable();
        sorted.dedup();

        assert_eq!(
            values, sorted,
            "the table must be sorted and duplicate-free"
        );
    }

    /// CA-003-05 — every entry carries the four pieces of information the UI
    /// and the retry policy need. An empty label would reach the user as such.
    #[test]
    fn every_entry_carries_a_symbol_and_both_labels() {
        for entry in all() {
            assert!(
                entry.symbol.starts_with("ESME_"),
                "{:#010X} has a malformed symbol: {:?}",
                entry.value,
                entry.symbol
            );
            assert!(
                !entry.label_fr.is_empty(),
                "{} has no French label",
                entry.symbol
            );
            assert!(
                !entry.label_en.is_empty(),
                "{} has no English label",
                entry.symbol
            );
            assert_ne!(
                entry.label_fr, entry.label_en,
                "{} looks like an untranslated label",
                entry.symbol
            );
        }
    }

    /// CA-003-06 — the classification is not decorative: milestone 005 reads it
    /// to decide whether to loop, milestone 010 to decide whether to replay.
    #[test]
    fn critical_statuses_are_classified_as_the_engine_expects() {
        // Bad credentials: no reconnection loop (guide §6.3).
        assert_eq!(classify(CommandStatus::EsmeRinvpaswd), StatusClass::Fatal);
        assert_eq!(classify(CommandStatus::EsmeRinvsysid), StatusClass::Fatal);
        assert_eq!(classify(CommandStatus::EsmeRbindfail), StatusClass::Fatal);

        // Flow control: slow down and replay (spec §7.6, §9.4).
        assert_eq!(
            classify(CommandStatus::EsmeRthrottled),
            StatusClass::Throttling
        );
        assert_eq!(
            classify(CommandStatus::EsmeRmsgqful),
            StatusClass::Throttling
        );

        // Invalid destination: the message is lost for good, the contact is
        // flagged; replaying it would burn quota on a number that cannot work.
        assert_eq!(classify(CommandStatus::EsmeRinvdstadr), StatusClass::Fatal);
        assert!(!classify(CommandStatus::EsmeRinvdstadr).is_retryable());

        // Session-state disagreements are NOT fatal — regression guard.
        //
        // These two were classified `Fatal` and caught in review. Each one
        // produced a concrete production failure:
        //
        //   * RALYBND after a TCP drop without `unbind`: the reconnection loop
        //     gives up for good, on the most ordinary SMPP failure there is.
        //   * RINVBNDSTS on a `submit_sm` crossing an `unbind`: the message is
        //     marked permanently failed instead of being replayed.
        //
        // Both must stay retryable. Failing this test means one of those two
        // regressions is back.
        assert_eq!(
            classify(CommandStatus::EsmeRalybnd),
            StatusClass::Recoverable
        );
        assert!(classify(CommandStatus::EsmeRalybnd).is_retryable());
        assert_eq!(
            classify(CommandStatus::EsmeRinvbndsts),
            StatusClass::Recoverable
        );
        assert!(classify(CommandStatus::EsmeRinvbndsts).is_retryable());

        // Transient failures: replay according to policy.
        assert_eq!(
            classify(CommandStatus::EsmeRsubmitfail),
            StatusClass::Recoverable
        );
        assert_eq!(
            classify(CommandStatus::EsmeRsyserr),
            StatusClass::Recoverable
        );

        assert_eq!(classify(CommandStatus::EsmeRok), StatusClass::Success);
    }

    /// CA-003-06 — "no value wrongly classified by default". The default only
    /// applies to statuses outside the table, and it is the conservative one.
    #[test]
    fn only_statuses_outside_the_table_fall_back_to_the_default() {
        for value in 0..=SCAN_UPPER_BOUND {
            let status = CommandStatus::from(value);

            if matches!(status, CommandStatus::Other(_)) {
                assert_eq!(
                    classify(status),
                    StatusClass::Fatal,
                    "{value:#010X} is not standard: the default must not invite a replay"
                );
            } else {
                assert!(
                    describe(status).is_some(),
                    "{value:#010X} would be classified by default"
                );
            }
        }
    }

    #[test]
    fn a_vendor_specific_status_is_reported_as_such() {
        assert!(is_vendor_specific(0x0000_0400));
        assert!(is_vendor_specific(0x0000_04FF));
        assert!(!is_vendor_specific(0x0000_0058));
        assert!(describe(CommandStatus::from(0x0000_0400)).is_none());
        assert_eq!(
            classify(CommandStatus::from(0x0000_0400)),
            StatusClass::Fatal
        );
    }

    #[test]
    fn retryability_follows_the_class() {
        assert!(!StatusClass::Success.is_retryable());
        assert!(!StatusClass::Fatal.is_retryable());
        assert!(StatusClass::Recoverable.is_retryable());
        assert!(StatusClass::Throttling.is_retryable());
        assert!(StatusClass::Throttling.requires_slowdown());
        assert!(!StatusClass::Recoverable.requires_slowdown());
    }

    /// Spec §7.6 quotes these codes with their hexadecimal value; a typo here
    /// would put the wrong label in front of the user.
    #[test]
    fn the_values_quoted_by_the_specification_are_at_the_right_place() {
        assert_eq!(
            describe_value(0x0000_0000).map(|e| e.symbol),
            Some("ESME_ROK")
        );
        assert_eq!(
            describe_value(0x0000_000A).map(|e| e.symbol),
            Some("ESME_RINVSRCADR")
        );
        assert_eq!(
            describe_value(0x0000_000B).map(|e| e.symbol),
            Some("ESME_RINVDSTADR")
        );
        assert_eq!(
            describe_value(0x0000_000E).map(|e| e.symbol),
            Some("ESME_RINVPASWD")
        );
        assert_eq!(
            describe_value(0x0000_000F).map(|e| e.symbol),
            Some("ESME_RINVSYSID")
        );
        assert_eq!(
            describe_value(0x0000_0014).map(|e| e.symbol),
            Some("ESME_RMSGQFUL")
        );
        assert_eq!(
            describe_value(0x0000_0045).map(|e| e.symbol),
            Some("ESME_RSUBMITFAIL")
        );
        assert_eq!(
            describe_value(0x0000_0058).map(|e| e.symbol),
            Some("ESME_RTHROTTLED")
        );
        // Spec §7.6 lists ESME_RINVMSGLEN at 0x00000005. The SMPP v3.4 and
        // v5.0 specifications both put it at 0x00000001, and 0x00000005 is
        // ESME_RALYBND. The table follows the protocol; the discrepancy is
        // reported in the milestone report.
        assert_eq!(
            describe_value(0x0000_0001).map(|e| e.symbol),
            Some("ESME_RINVMSGLEN")
        );
        assert_eq!(
            describe_value(0x0000_0005).map(|e| e.symbol),
            Some("ESME_RALYBND")
        );
    }
}
