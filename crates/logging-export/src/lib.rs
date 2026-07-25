//! Journal métier et exports CSV, XLSX et JSON.
//!
//! Distincte de la journalisation technique (`tracing`) : cette crate produit
//! le journal *fonctionnel* consultable par l'utilisateur — envois, accusés
//! de réception, transitions de session — et ses exports.
//!
//! Contrainte de confidentialité (CLAUDE.md §8) : le contenu des messages est
//! masqué ou tronqué par défaut dans tout journal destiné au partage, et le
//! dump hexadécimal des PDU reste réservé à un mode debug explicite.
//!
//! Implémentation au jalon 014.

mod error;

pub use error::LoggingExportError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!logging_export::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::LoggingExportError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            LoggingExportError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
