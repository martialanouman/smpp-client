# Jalon 015 — Sécurité : secrets, TLS, durcissement et usage responsable

> **Statut :** À faire · **Dépend de :** step-014 · **Réf. spec :** §17 · **Réf. guide :** §17 · **Exigences :** EF-CNX-07, EF-CFG-02

## 1. Objectif

Rendre l'application sûre en usage réel : identifiants SMSC chiffrés au repos avec clé au trousseau du système, transport TLS vérifié, WebView durcie, entrées IPC intégralement validées, et garde-fous d'usage responsable (liste d'exclusion, plafonds, journal d'audit).

Ce jalon lève des dettes délibérément contractées plus tôt : jusqu'ici les mots de passe n'étaient pas persistés (step-005) et la colonne `password_enc` recevait un blob non chiffré (step-002). Il doit donc **fermer** ces points, pas seulement ajouter des fonctionnalités.

## 2. Périmètre

### Dans le périmètre

- Crate `security` : chiffrement **AES-256-GCM** des secrets, clé stockée dans le trousseau OS via `keyring` (Keychain macOS, Credential Manager Windows, Secret Service/libsecret Linux).
- Option de **mot de passe maître** avec dérivation **Argon2id**, indépendante du trousseau OS.
- Migration des profils de session existants vers le stockage chiffré.
- **TLS** sur la connexion SMPP via `tokio-rustls` : vérification du certificat **activée par défaut**, option de CA personnalisée, avertissement UI explicite pour une session en clair.
- Durcissement Tauri : capacités/permissions minimales auditées (pas de shell ; FS restreint aux répertoires applicatifs et aux fichiers choisis via dialogues natifs), CSP stricte, pas de contenu distant, pas d'`eval`.
- Revue systématique de la validation d'entrée sur **toutes** les commandes IPC accumulées depuis step-001.
- Masquage/tronquage du contenu des messages dans les journaux et les exports (option, activée par défaut pour les journaux partagés).
- Garde-fous d'usage responsable : import et application d'une **liste d'exclusion (opt-out) avant tout envoi**, plafonds de débit et de volume avec confirmation au-delà d'un seuil, journal d'audit des campagnes (qui, quoi, quand, combien).
- Bundle de diagnostic exportable : logs techniques + configuration **anonymisée**.
- `cargo audit` / `cargo deny` déjà en CI (step-000) : revue des exceptions accumulées.

### Hors périmètre

- La signature des binaires et la notarisation → **step-016**.
- Un audit de sécurité externe : recommandé mais hors périmètre du jalon.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-015-01 | Chiffrement AES-256-GCM + intégration keyring | `crates/security/src/secrets.rs` |
| L-015-02 | Mot de passe maître (Argon2id) | `crates/security/src/master_password.rs` |
| L-015-03 | Migration des profils vers le stockage chiffré | `migrations/`, `crates/security/src/migration.rs` |
| L-015-04 | Support TLS et vérification de certificat | `crates/security/src/tls.rs`, `crates/smpp-session/` |
| L-015-05 | Capacités Tauri auditées + CSP | `src-tauri/capabilities/`, `src-tauri/tauri.conf.json` |
| L-015-06 | Liste d'exclusion appliquée avant envoi | `crates/messaging/src/optout.rs` |
| L-015-07 | Plafonds, confirmation de volume, journal d'audit | `crates/messaging/src/guardrails.rs` |
| L-015-08 | Masquage du contenu dans logs et exports | `crates/logging-export/src/redaction.rs` |
| L-015-09 | Bundle de diagnostic anonymisé | `src-tauri/src/commands/diagnostics.rs` |

## 4. Critères d'acceptation

- [ ] **CA-015-01** — Un mot de passe SMSC saisi est stocké **chiffré** ; l'inspection directe du fichier de base (`sqlite3` + `hexdump`) ne révèle aucune trace du secret en clair.
- [ ] **CA-015-02** — La clé de chiffrement réside dans le trousseau OS ; supprimer l'entrée du trousseau rend les profils illisibles et produit un message d'erreur explicite plutôt qu'un plantage.
- [ ] **CA-015-03** — Le mode mot de passe maître fonctionne indépendamment du trousseau ; un mot de passe erroné est rejeté sans révéler d'information sur le contenu chiffré.
- [ ] **CA-015-04** — **Aucun secret dans les traces**, y compris au niveau `trace` et dans le dump PDU : test automatisé qui exécute un bind complet, capture toute la sortie `tracing` et le journal PDU, et échoue si le mot de passe y apparaît.
- [ ] **CA-015-05** — Aucun secret dans les exports ni dans le bundle de diagnostic : test équivalent sur les fichiers produits.
- [ ] **CA-015-06** — Aucun secret en dur dans le code ni dans les fixtures : `rg` sur des motifs de secrets + revue ; les fixtures utilisent des valeurs manifestement factices.
- [ ] **CA-015-07** — Une session TLS se connecte à un SMSC TLS de test ; un certificat invalide, expiré ou dont le nom ne correspond pas est **rejeté par défaut**.
- [ ] **CA-015-08** — Une session sans TLS affiche un avertissement explicite et non ambigu dans l'UI.
- [ ] **CA-015-09** — Les capacités Tauri déclarées n'incluent aucune permission shell ni accès FS hors répertoires applicatifs ; la CSP interdit contenu distant et `eval` — audit du fichier de capacités documenté dans la PR.
- [ ] **CA-015-10** — **Toutes** les commandes IPC valident leurs entrées : revue exhaustive listée dans la PR ; un fuzzing léger des commandes avec des entrées aberrantes ne provoque aucun panic (le processus survit à 10 000 appels malformés).
- [ ] **CA-015-11** — La liste d'exclusion est appliquée **avant** toute émission : un numéro exclu n'est jamais envoyé, y compris en campagne massive et en cas de reprise après crash (test explicite sur le chemin de reprise).
- [ ] **CA-015-12** — Un envoi dépassant le plafond de volume configuré exige une confirmation explicite ; sans confirmation, rien n'est émis.
- [ ] **CA-015-13** — Le journal d'audit enregistre chaque campagne (initiateur, contenu, horodatage, volume) et n'est pas modifiable depuis l'UI.
- [ ] **CA-015-14** — Le masquage du contenu est actif par défaut dans les journaux partagés et les exports ; le désactiver est un geste explicite.
- [ ] **CA-015-15** — `cargo audit` et `cargo deny check` verts, sans exception non justifiée par un commentaire daté.
- [ ] **CA-015-16** — Deux approbations requises dont un mainteneur (guide §16.2).

## 5. Tests attendus

- **Unitaires :** chiffrement/déchiffrement (round-trip, clé erronée, données corrompues détectées par le tag GCM), dérivation Argon2id, rédaction/masquage, application de la liste d'exclusion, plafonds.
- **Intégration :** bind TLS contre un serveur de test (certificat valide, auto-signé, expiré, mauvais nom d'hôte) ; migration d'un profil non chiffré vers le stockage chiffré.
- **Sécurité (tests négatifs) :** recherche de secrets dans toutes les sorties (traces, base, exports, bundle de diagnostic) ; fuzzing des commandes IPC.
- **Non-régression :** un test garantit qu'une nouvelle commande IPC sans validation d'entrée est détectée en revue (checklist) ou par test.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest library "keyring" "store and retrieve a secret from the OS keychain in Rust cross-platform"
  npx ctx7@latest docs /tauri-apps/tauri-docs "Tauri 2 capabilities, permissions scope and Content Security Policy hardening"
  ```
- Le **nonce AES-GCM ne doit jamais être réutilisé** avec la même clé : générer un nonce aléatoire par chiffrement et le stocker à côté du texte chiffré. Une réutilisation compromet la confidentialité — c'est le piège classique de GCM.
- Sur Linux, `keyring` dépend du Secret Service (libsecret) : sur une machine sans trousseau disponible (serveur, session minimale), prévoir un repli explicite vers le mot de passe maître plutôt qu'un échec incompréhensible.
- Le test « aucun secret dans les traces » doit être **automatisé et permanent**, pas une vérification manuelle ponctuelle : c'est la seule protection contre une régression introduite par un futur `tracing::debug!` bien intentionné.
- L'application de la liste d'exclusion doit se situer sur le chemin d'émission lui-même, pas seulement à la constitution de la campagne : sinon un opt-out ajouté pendant une campagne en cours ne serait pas respecté.
