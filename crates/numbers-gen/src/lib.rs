//! Génération de numéros valides par pays.
//!
//! Produit des MSISDN structurellement valides pour un pays donné en
//! s'appuyant sur les plages de numérotation de `phonenumber`, avec unicité
//! garantie sur un lot et reproductibilité par graine.
//!
//! Le générateur aléatoire est **injecté** (CLAUDE.md §7) : à graine égale,
//! deux exécutions produisent la même suite. C'est ce qui rend les propriétés
//! d'unicité vérifiables par `proptest`.
//!
//! Implémentation au jalon 013.

mod error;

pub use error::NumbersGenError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!numbers_gen::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::NumbersGenError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            NumbersGenError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
