//! Import de contacts, validation E.164 et gestion des listes.
//!
//! Lit des fichiers CSV et XLSX (`csv`, `calamine`), normalise les numéros au
//! format E.164 via `phonenumber`, déduplique, et matérialise les listes de
//! diffusion ainsi que la **liste d'exclusion** appliquée avant tout envoi
//! (garde-fou d'usage, CLAUDE.md §8).
//!
//! *Parse, don't validate* : un numéro qui franchit la frontière de cette
//! crate est un `Msisdn`, pas une `String` — l'invalidité est impossible à
//! représenter en aval.
//!
//! Implémentation au jalon 009.

mod error;

pub use error::ContactsError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!contacts::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::ContactsError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            ContactsError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
