//! Secrets, trousseau du système et configuration TLS.
//!
//! Seule crate autorisée à manipuler des identifiants SMSC en clair, et
//! uniquement en mémoire. Elle chiffre les secrets avant persistance
//! (AES-256-GCM, clé dérivée par Argon2 et conservée au trousseau OS via
//! `keyring`) et construit les configurations `tokio-rustls` avec vérification
//! de certificat active par défaut.
//!
//! Invariant tenu par cette crate (CLAUDE.md §8) : **aucun secret ne sort
//! jamais en clair** — ni en base, ni en journal même au niveau `trace`, ni
//! en export. Les types qui portent un secret n'implémentent pas `Debug` de
//! façon dérivée.
//!
//! Implémentation au jalon 015.

mod error;

pub use error::SecurityError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!security::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::SecurityError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            SecurityError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
