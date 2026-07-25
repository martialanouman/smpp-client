//! Cœur protocolaire SMPP : codec PDU et machine à états, v3.4 et v5.0.
//!
//! Couche la plus basse de l'architecture (guide §8.1). Elle ne dépend
//! **d'aucune autre crate interne** et ne connaît ni la persistance, ni le
//! réseau, ni Tauri : elle traduit des octets en PDU typés et inversement,
//! et arbitre les transitions d'états autorisées par la norme.
//!
//! Le squelette est vide au jalon 000 ; l'implémentation commence au
//! jalon 003, sur la base de l'ADR
//! [`0001-choix-de-la-pile-smpp`](../../../docs/adr/0001-choix-de-la-pile-smpp.md).

mod error;

pub use error::SmppCoreError;

/// Version de la crate, telle que déclarée dans son manifeste.
///
/// ```
/// assert!(!smpp_core::VERSION.is_empty());
/// ```
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::SmppCoreError;

    #[test]
    fn l_erreur_de_crate_expose_un_message_lisible() {
        assert_eq!(
            SmppCoreError::NonImplemente.to_string(),
            "fonctionnalité non implémentée"
        );
    }
}
