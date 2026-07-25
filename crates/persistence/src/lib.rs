//! Accès SQLite : migrations, repositories et transactions.
//!
//! Implémente les *ports* définis par les couches supérieures — le trait
//! `MessageRepository` appartient à `messaging`, son implémentation SQLx vit
//! ici (inversion de dépendance, guide §4.2). La crate ne dépend donc
//! d'aucune autre crate interne malgré sa position dans le graphe.
//!
//! Base SQLite en mode WAL, accès via SQLx — cf. ADR
//! [`0002-persistance-sqlite-sqlx`](../../../docs/adr/0002-persistance-sqlite-sqlx.md).
//! Elle porte l'invariant *write-ahead* de CLAUDE.md §4 : un message est
//! persisté **avant** émission, et ses transitions d'état sont idempotentes.
//!
//! Schéma et migrations au jalon 002.

mod error;

pub use error::PersistenceError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!persistence::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::PersistenceError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            PersistenceError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
