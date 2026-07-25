//! Sessions SMPP : bind, fenêtrage, `enquire_link` et reconnexion.
//!
//! Transforme le codec sans état de [`smpp_core`] en sessions vivantes :
//! une tâche par session possède le socket, les autres composants lui parlent
//! par messages sur des files `mpsc` **bornées** (CLAUDE.md §4) — c'est le
//! back-pressure qui empêche une campagne de saturer la mémoire quand le SMSC
//! ralentit.
//!
//! Dépend de [`smpp_core`] pour les PDU et de [`rate_control`] pour la cadence
//! d'émission. Toute tâche longue écoute un `CancellationToken` et s'arrête
//! proprement : `unbind` puis vidage des files.
//!
//! Implémentation au jalon 005.

mod error;

pub use error::SessionError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!smpp_session::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::SessionError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            SessionError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
