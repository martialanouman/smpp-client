//! Encodage, segmentation et orchestration des envois.
//!
//! Point d'entrée métier de l'émission : choisit le DCS, segmente les
//! messages longs (UDH ou TLV `sar_*`), persiste avant émission puis confie
//! les PDU à [`smpp_session`]. Orchestre également les campagnes de masse —
//! reprise après interruption, suivi de progression, plafonds de volume.
//!
//! Définit les *ports* que [`persistence`] implémente (`MessageRepository`) :
//! le trait appartient à cette couche, son implémentation SQLx à la couche
//! basse. C'est ce qui rend l'orchestrateur testable sans base réelle.
//!
//! Implémentation aux jalons 004, 006 et 010.

mod error;

pub use error::MessagingError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!messaging::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::MessagingError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            MessagingError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
