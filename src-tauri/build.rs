//! Script de construction Tauri.
//!
//! `tauri_build::build()` produit le contexte compilé à partir de
//! `tauri.conf.json` et des `capabilities/` : c'est lui qui transforme la
//! liste de permissions en vérifications intégrées au binaire, plutôt qu'en
//! contrôles évaluables à l'exécution.

fn main() {
    tauri_build::build();
}
