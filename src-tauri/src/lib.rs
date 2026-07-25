//! Couche IPC de ShinobiSMPP — la seule à connaître Tauri.
//!
//! Cette crate est une **bordure**, pas une couche métier (guide §8.3). Son
//! rôle se limite à quatre gestes : désérialiser et valider une entrée IPC,
//! appeler un service métier, sérialiser la sortie, émettre un événement.
//! Tout ce qui déborde de ces quatre gestes descend dans une crate de
//! `crates/`.
//!
//! C'est aussi le seul endroit du dépôt où `anyhow` est autorisé : les crates
//! métier exposent des erreurs `thiserror` typées, que le point d'entrée
//! agrège (CLAUDE.md §4).
//!
//! Au jalon 000, l'application se contente de démarrer et d'afficher la page
//! d'attente. Le contrat IPC arrive au jalon 001.

use anyhow::Context as _;

mod commands;
mod events;

/// Construit puis lance l'application Tauri.
///
/// # Erreurs
///
/// Renvoie une erreur si le contexte généré à la compilation est invalide ou
/// si la WebView ne peut pas être initialisée — typiquement une dépendance
/// système absente (WebView2 sur Windows, `webkit2gtk` sur Linux).
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> anyhow::Result<()> {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![])
        .run(tauri::generate_context!())
        .context("échec du démarrage de l'application Tauri")
}
