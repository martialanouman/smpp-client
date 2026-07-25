//! Événements émis vers le frontend.
//!
//! Le sens inverse de [`crate::commands`] : le backend pousse ici les
//! changements d'état que l'interface ne peut pas déduire seule — transitions
//! de session, progression de campagne, métriques.
//!
//! Conventions (guide §9.3) :
//!
//! - nommage `domaine:action` — `sessions:state`, `message:update`,
//!   `metrics:tick` ;
//! - les événements à haute fréquence sont **throttlés côté Rust** (1 à 4 Hz
//!   pour `metrics:tick`). Émettre à la cadence réelle des PDU saturerait le
//!   pont IPC et rendrait la WebView inutilisable pendant une campagne.
//!
//! Vide au jalon 000.
