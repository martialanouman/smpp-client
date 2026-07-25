//! Point d'entrée du binaire ShinobiSMPP.
//!
//! Se contente de déléguer à [`shinobismpp_lib::run`] : toute la construction
//! de l'application vit dans la bibliothèque, qui est seule testable et seule
//! utilisable comme point d'entrée mobile.

// Empêche l'ouverture d'une console supplémentaire sous Windows en release.
// NE PAS RETIRER : sans cet attribut, l'application distribuée affiche une
// fenêtre de terminal noire derrière son interface.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // L'erreur est propagée plutôt qu'attrapée : `main` renvoyant un
    // `Result`, le runtime l'affiche et positionne un code de sortie non nul.
    // C'est ce qui permet de se passer du `.expect()` du modèle Tauri, que
    // nos lints interdisent (CLAUDE.md §4).
    shinobismpp_lib::run()
}
