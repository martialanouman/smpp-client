//! Type d'erreur de la crate.

/// Erreurs produites par cette crate.
///
/// Conformément au guide §6.1, chaque crate expose **un** type d'erreur
/// `thiserror` exhaustif. Aucune API publique ne renvoie de
/// `Box<dyn Error>` : l'appelant doit pouvoir discriminer les cas.
///
/// `#[non_exhaustive]` permet d'ajouter des variantes aux jalons suivants
/// sans casser les `match` des crates appelantes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecurityError {
    /// Point d'extension du jalon 000 : la crate n'expose encore aucune
    /// logique. Remplacée par des variantes réelles dès le premier jalon
    /// qui implémente cette couche.
    #[error("fonctionnalité non implémentée")]
    NonImplemente,
}
