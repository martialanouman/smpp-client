//! Limitation de débit et adaptation à la congestion.
//!
//! Fait respecter le TPS négocié avec le SMSC et réagit aux signaux de
//! congestion (`ESME_RTHROTTLED`, TLV `congestion_state` en v5.0) en
//! ralentissant la cadence d'émission. S'appuiera sur `governor`.
//!
//! Aucune dépendance interne : la crate raisonne sur des instants et des
//! quotas, jamais sur des PDU. C'est ce qui la rend testable avec une horloge
//! injectée, condition du déterminisme exigé par CLAUDE.md §7.
//!
//! Implémentation au jalon 007.

mod error;

pub use error::RateControlError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!rate_control::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::RateControlError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            RateControlError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
