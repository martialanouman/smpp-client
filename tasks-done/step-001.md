# Jalon 001 — Socle applicatif : shell Tauri, UI et contrat IPC typé

> **Statut :** Terminé (2026-07-26) — 11 critères sur 11 vérifiés · **Dépend de :** step-000 · **Réf. spec :** §15, §16, §18.2 · **Réf. guide :** §9, §10, §12

## 1. Objectif

Obtenir une application qui démarre sur les trois OS, présente les huit écrans (vides mais navigables), et dans laquelle un aller-retour IPC typé de bout en bout fonctionne : une commande Rust appelée depuis le frontend via un wrapper **généré**, et un événement backend reçu par l'UI.

C'est le jalon qui fixe le **contrat** entre Rust et TypeScript. Tous les jalons suivants ajoutent des commandes et des événements à cette mécanique ; si la génération de types, la validation d'entrée et le DTO d'erreur ne sont pas corrects ici, la dérive se paiera sur chaque commande ultérieure.

## 2. Périmètre

### Dans le périmètre

- Initialisation applicative : état global Tauri, répertoire de données par OS (API `path`), `tracing` + `tracing-appender` (fichier rotatif + console en dev), niveau de log configurable.
- Contrat IPC : DTO Rust `serde`, génération TypeScript par tauri-specta, wrappers typés dans `ui/src/ipc/`, DTO d'erreur stable `{ code, message, details }`.
- Deux commandes témoins : `config_get` et `config_set` (préférences applicatives : langue, thème, niveau de log, rétention — persistées en fichier de configuration, pas encore en base).
- Un événement témoin `error:notify` et le mécanisme d'abonnement côté UI.
- Coquille UI : layout, navigation vers les 8 écrans (Tableau de bord, Sessions, Envoi, Contacts, Générateur, Journaux, Statistiques, Réglages), store Zustand, thèmes clair/sombre, i18n FR/EN branché avec les libellés de navigation.
- Durcissement de base : CSP stricte, capacités Tauri minimales déclarées explicitement.

### Hors périmètre

- Toute commande métier (`session_*`, `message_*`, `campaign_*`, `contacts_*`, `logs_*`) → jalons dédiés.
- La persistance SQLite → **step-002** (la config de ce jalon vit dans un fichier).
- Le chiffrement des secrets → **step-015**.
- Le contenu réel des écrans : ce sont des placeholders identifiés, pas des maquettes finales.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-001-01 | Initialisation applicative, état global, chemins de données par OS | `src-tauri/src/main.rs`, `src-tauri/src/state.rs` |
| L-001-02 | Journalisation `tracing` + fichier rotatif | `src-tauri/src/telemetry.rs` |
| L-001-03 | Commandes `config_get` / `config_set` + DTO d'erreur | `src-tauri/src/commands/config.rs`, `src-tauri/src/error.rs` |
| L-001-04 | Émission d'événements typés | `src-tauri/src/events.rs` |
| L-001-05 | Types TS générés + wrappers `invoke` typés | `ui/src/ipc/` |
| L-001-06 | Layout, routing, 8 vues placeholder | `ui/src/views/`, `ui/src/components/` |
| L-001-07 | Store global, thèmes, i18n FR/EN | `ui/src/store/`, `ui/src/i18n/` |
| L-001-08 | Capacités Tauri minimales et CSP | `src-tauri/capabilities/`, `src-tauri/tauri.conf.json` |

## 4. Critères d'acceptation

- [x] **CA-001-01** — L'application démarre et affiche le Tableau de bord ; la navigation atteint les 8 écrans sans erreur console.
- [x] **CA-001-02** — `config_set` puis redémarrage de l'app : les préférences sont conservées, dans le répertoire de données standard de l'OS et nulle part ailleurs.
- [x] **CA-001-03** — Les types TS de `ui/src/ipc/` sont **générés** ; modifier un DTO Rust sans régénérer fait échouer l'étape 4 de la CI (`git diff --exit-code`).
- [x] **CA-001-04** — Aucun `invoke` brut hors de `ui/src/ipc/` : `rg "invoke\(" ui/src --glob '!src/ipc/**'` ne retourne rien (règle vérifiée aussi par une règle eslint).
- [x] **CA-001-05** — Une entrée invalide passée à `config_set` (langue inconnue, niveau de log inexistant) retourne un `ErrorDto` explicite ; l'application ne panique pas et le processus reste vivant.
- [x] **CA-001-06** — L'`ErrorDto` retourné ne contient ni chemin absolu du système de fichiers, ni secret, ni trace interne.
- [x] **CA-001-07** — Les traces sont écrites dans un fichier rotatif du répertoire de logs de l'app ; `rg "println!|eprintln!" src-tauri/src crates` ne retourne rien.
- [x] **CA-001-08** — Bascule FR ⇄ EN et clair ⇄ sombre effectives sans rechargement ; aucune chaîne visible en dur dans les composants (`rg` sur les littéraux de navigation ne trouve que des clés i18n).
- [x] **CA-001-09** — Les capacités Tauri déclarées n'incluent ni `shell`, ni accès FS hors répertoires applicatifs ; la CSP interdit le contenu distant et `eval`.
- [x] **CA-001-10** — L'événement `error:notify` émis côté Rust déclenche un toast dans l'UI (test manuel scripté + test unitaire du réducteur).
- [x] **CA-001-11** — `just lint` et `just test` verts ; CI verte sur les trois OS.

## 5. Tests attendus

- **Unitaires Rust :** validation d'entrée de `config_set` (cas valides et invalides), conversion erreur interne → `ErrorDto` (vérifier l'absence de fuite d'information), résolution du répertoire de données.
- **Unitaires frontend :** rendu du layout, store de préférences, changement de langue/thème, réducteur de notifications.
- **Intégration légère :** aller-retour `config_set` → `config_get` via les wrappers générés.
- **Non-régression :** un test garantit qu'un DTO ajouté sans régénération est détecté.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /tauri-apps/tauri-docs "Tauri 2 commands, state management, events and capabilities permissions"
  npx ctx7@latest library "tauri-specta" "generate TypeScript bindings for Tauri 2 commands and events"
  ```
- Nommage imposé (guide §9) : commandes en `snake_case` sur le motif `domaine_action` ; événements en `domaine:action`.
- Le DTO d'erreur est un **point de conception durable** : prévoir dès maintenant un `code` stable et machine-lisible (ex. `CONFIG_INVALID_LANGUAGE`, `SMPP_BIND_REJECTED`) distinct du `message` localisable, et un `details` optionnel structuré. Le rétro-ajuster plus tard casse le frontend.
- `src-tauri` reste mince : la logique de configuration (lecture/écriture/validation) descend dans une crate ou un module dédié, la commande ne fait que valider, appeler, sérialiser.
- Prévoir dès maintenant le **throttling des événements** (agrégation côté backend) même si aucun événement à haute fréquence n'existe encore : c'est le point d'accroche de `metrics:tick` en step-007.
