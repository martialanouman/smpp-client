//! Commandes IPC exposées au frontend.
//!
//! Chaque commande suit le même contrat (guide §9.1) :
//!
//! - nommage `domaine_action` en `snake_case` — `session_bind`, `message_send` ;
//! - signature `Result<Dto, ErrorDto>`, jamais de `panic!` ni de type opaque ;
//! - validation des entrées **ici**, jamais côté frontend : la WebView est
//!   traitée comme non fiable (CLAUDE.md §3) ;
//! - le DTO d'erreur `{ code, message, details }` est stable et ne laisse
//!   fuir ni chemin de fichier, ni secret.
//!
//! Vide au jalon 000 : le contrat IPC et la génération des types TypeScript
//! arrivent au jalon 001.
