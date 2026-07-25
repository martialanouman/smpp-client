# CLAUDE.md — ShinobiSMPP

Règles de travail pour tout agent ou contributeur intervenant sur ce dépôt.

## 1. Projet

**ShinobiSMPP** — client **ESME** SMPP de qualité production, application desktop multiplateforme (Windows 10/11, macOS 12+ Intel & Apple Silicon, Linux x64). Il se connecte à un ou plusieurs SMSC via des sessions SMPP **v3.4** et **v5.0**, envoie des messages unitaires ou en masse, importe des contacts, génère des numéros valides par pays, journalise et exporte.

**Hors périmètre :** rôle serveur/SMSC (sauf simulateur de test), facturation, routage inter-opérateurs, canaux non-SMPP.

### Sources de vérité

| Document | Rôle |
|----------|------|
| [`docs/spec_smpp_client.md`](docs/spec_smpp_client.md) | **Le quoi** — exigences `EF-*`/`ENF-*`, architecture, modèle de données, contrat IPC, algorithmes |
| [`docs/guide_ingenierie_smpp.md`](docs/guide_ingenierie_smpp.md) | **Le comment** — conventions, erreurs, concurrence, tests, CI, sécurité, release |
| [`tasks-todo/step-XXX.md`](tasks-todo/) | **Le quand** — jalons d'implémentation, périmètre et critères d'acceptation |

En cas de contradiction entre ce fichier et la spec ou le guide, **la spec et le guide priment** ; signale l'écart plutôt que de trancher seul.

## 2. Pile technique

- **Backend :** Rust ≥ 1.78 (édition 2021), **Tokio** multi-thread, **rusmpp** / **rusmppc** (codec PDU et client SMPP v5, rétrocompatible v3.4), **SQLx** + SQLite en mode **WAL**, `governor` (débit), `phonenumber`, `calamine` / `rust_xlsxwriter` / `csv`, `tracing` + `tracing-appender`, `keyring` / `aes-gcm` / `argon2`, `tokio-rustls`.
- **Application :** **Tauri 2.x** (`src-tauri` = couche IPC mince uniquement).
- **Frontend :** React 18 + **TypeScript strict** (`strict`, `noUncheckedIndexedAccess`), Vite, Tailwind + shadcn/ui, Zustand, TanStack Table/Virtual, i18next.
- **Outils :** Node 20 LTS, **pnpm**, `just`, `cargo nextest`, `cargo-audit`, `cargo-deny`, `sqlx-cli`.

Toute nouvelle dépendance doit être justifiée (maintenance, licence permissive, taille) ; une dépendance lourde ou peu maintenue passe par une ADR.

## 3. Architecture et frontières

```
ui (TS)  ──invoke/events──►  src-tauri  ──►  messaging · contacts · numbers-gen · logging-export
                                              │              │
                                              ▼              ▼
                                        smpp-session    persistence · security
                                              │
                                              ▼
                                         smpp-core (rusmpp)
```

Crates : `smpp-core`, `smpp-session`, `rate-control`, `messaging`, `contacts`, `numbers-gen`, `persistence`, `logging-export`, `security`.

**Règles impératives :**

- Les dépendances vont **du haut vers le bas**, jamais l'inverse. `smpp-core` ne dépend d'aucune crate interne. Aucun cycle.
- **Aucune crate métier ne dépend de `tauri`.** Seul `src-tauri` connaît Tauri.
- `src-tauri` ne contient **aucune logique métier** : validation d'entrée, appel de service, sérialisation de sortie, émission d'événements. Ce qui déborde descend dans une crate.
- **Aucune logique protocolaire côté frontend.** Le frontend affiche l'état et déclenche des commandes ; il est traité comme **non fiable** — toute donnée franchissant l'IPC est validée côté Rust.
- Inversion de dépendance par traits (ports) définis dans la couche haute, implémentés dans la couche basse (ex. `MessageRepository` défini dans `messaging`, implémenté dans `persistence`).

## 4. Règles de code non négociables

### Rust

- **Interdit :** `unwrap()`, `expect()`, `panic!` en production (autorisés en tests ; exception uniquement sur invariant démontré et commenté `// INVARIANT: …`).
- **Interdit :** `unsafe` sauf nécessité absolue isolée, commentée, testée et revue par un mainteneur.
- **Erreurs :** un type `thiserror` explicite et exhaustif **par crate** (`SmppError`, `PersistenceError`…). Jamais de `Box<dyn Error>` dans une API publique de crate. `anyhow` uniquement en bordure (`src-tauri`, binaires, tests). Contextualiser en remontant, ne jamais avaler un `Result`. À la frontière IPC : DTO d'erreur stable `{ code, message, details }`, sans fuite de chemin ni de secret.
- **Typage fort :** newtypes (`Msisdn`, `SessionId`) et enums exhaustifs plutôt que `String`/`u8` nus. TON, NPI, DCS, `command_status` sont des enums typés. *Parse, don't validate*.
- **Async :** aucun blocage sur le runtime (`std::thread::sleep`, I/O bloquante, CPU intensif → `spawn_blocking`). Pas de `block_on` imbriqué, pas de busy-wait, pas de tâche orpheline non supervisée. Jamais de `std::sync::Mutex` tenu à travers un `.await` (utiliser `tokio::sync::Mutex`).
- **Concurrence :** passage de messages plutôt qu'état partagé verrouillé. Une seule tâche possède le socket. Files `mpsc` **bornées** (back-pressure). Toute tâche longue écoute un `CancellationToken` et s'arrête proprement (unbind, flush).
- **Journalisation :** `tracing` exclusivement, jamais `println!`/`eprintln!`. Champs structurés, spans par `session_id` et `campaign_id`.
- **Persistance write-ahead :** un message est persisté **avant** émission ; ses transitions d'état sont traçables et idempotentes (`client_message_id` UUID).
- **Visibilité minimale :** privé par défaut, `pub(crate)` pour l'interne, `pub` seulement pour l'API de crate. Chaque item public porte un `///` (contrat, invariants, erreurs).

### Frontend

- Tout appel backend passe par les **wrappers typés** de `ui/src/ipc/` — jamais d'`invoke` brut dans un composant.
- Les DTO sont définis **une seule fois en Rust** et les types TS sont **générés** (tauri-specta). Ne jamais redéclarer un type à la main ; un diff de types générés non commité fait échouer la CI.
- Tables volumineuses (logs, contacts) **virtualisées** et paginées côté backend. Aucun traitement lourd dans la WebView.
- Pas d'`any`. Textes utilisateur via i18n (FR par défaut, EN) — aucune chaîne en dur.

## 5. Commandes

```bash
pnpm install          # dépendances frontend
pnpm tauri dev        # app en hot-reload
just fmt              # cargo fmt + prettier
just lint             # cargo clippy -D warnings + eslint + tsc
just test             # cargo nextest run + vitest
just audit            # cargo audit + cargo deny check
just migrate          # sqlx migrate run
just build            # tauri build
```

`just lint` et `just test` doivent être verts **avant** tout commit.

## 6. Workflow Git

- **Trunk-based léger :** `main` toujours livrable. Pas de commit direct sur `main` : branches courtes `feat/…`, `fix/…`, `chore/…`, `docs/…`, `refactor/…`, `test/…`, puis PR avec CI verte.
- **Commits atomiques :** un commit = une intention cohérente et complète. On ne mélange pas un refactor et une fonctionnalité, ni du formatage et de la logique.
- **Conventional Commits en français**, scope = nom de crate ou de zone :

```
feat(smpp-core): ajoute le support du PDU broadcast_sm (v5.0)
fix(rate-control): respecte le TPS cible lors d'un burst initial
refactor(smpp-session): extrait le superviseur de reconnexion
test(numbers-gen): vérifie la reproductibilité par graine
docs(tasks): ajoute le jalon 004 — encodage et segmentation
chore(ci): met en cache le registre cargo
```

- `feat` → bump mineur, `fix` → correctif, changement cassant du contrat IPC ou du format de données → majeur. Le CHANGELOG (Keep a Changelog) est alimenté à partir des commits.
- Ne jamais committer `Cargo.lock`/`pnpm-lock.yaml` modifiés « par accident » ; ils sont versionnés et leurs mises à jour sont revues.
- **Ne pas pousser ni créer de tag sans demande explicite.**

## 7. Tests

- Pyramide : unitaire (≥ 80 % sur le cœur protocolaire), intégration contre simulateur SMSC, propriété (`proptest` : round-trip codec, unicité des numéros), performance (`criterion`), frontend (`vitest`, E2E `tauri-driver`).
- Tests **déterministes** : horloge et RNG **injectés** (graine fixe), jamais de dépendance à l'heure réelle ou à l'aléatoire non contrôlé.
- Tests **isolés** : chaque test d'intégration utilise sa propre base SQLite temporaire.
- Un test = un comportement, nommé explicitement. Toute correction de bug ajoute un **test de non-régression** reproduisant le bug.

## 8. Sécurité

- **Aucun secret** en clair : ni en base (AES-256-GCM, clé au trousseau OS via `keyring`), ni en log (même en `trace`), ni en export, ni en dur dans le code ou les fixtures de test.
- TLS (`tokio-rustls`) avec vérification de certificat **activée par défaut** ; avertissement UI explicite pour une session en clair.
- Capacités/permissions Tauri **minimales** (pas de shell ; FS restreint aux répertoires app et aux fichiers choisis via dialogues natifs), **CSP stricte**, pas de contenu distant, pas d'`eval`.
- Contenu des messages masqué/tronqué par défaut dans les journaux partagés ; dump hexadécimal des PDU réservé au mode debug explicite.
- Garde-fous d'usage : liste d'exclusion appliquée avant tout envoi, plafonds de débit/volume avec confirmation, journal d'audit des campagnes.

## 9. Documentation externe et décisions

- **Toujours utiliser `ctx7`** avant d'écrire du code s'appuyant sur une librairie externe (rusmpp, Tauri, SQLx, governor, phonenumber, calamine…), même si l'API semble connue :

```bash
npx ctx7@latest library "<nom officiel>" "<question>"
npx ctx7@latest docs /org/projet "<question précise, un concept à la fois>"
```

- Toute décision structurante (niveau d'API rusmpp, moteur de base, stratégie de segmentation, format des secrets, générateur de types IPC) est consignée dans une **ADR** `docs/adr/NNNN-titre.md` : contexte, options, décision, conséquences. Une ADR est immuable ; on la supersède, on ne la modifie pas.
- `cargo doc` pour l'API des crates ; contrat IPC documenté et généré.

## 10. Où travailler

Le travail est découpé en jalons dans [`tasks-todo/`](tasks-todo/), de `step-000.md` (fondations, CI, release) à `step-017.md`.

1. Ouvre le jalon courant, lis son **objectif**, son **périmètre (in/out)** et ses **critères d'acceptation**.
2. Reste **dans le périmètre** : ce qui est marqué hors périmètre appartient à un jalon ultérieur — ne l'anticipe pas sans accord explicite.
3. Un jalon est terminé quand **tous** ses critères d'acceptation sont vérifiés, la CI est verte, les tests couvrent le comportement, et le CHANGELOG/les ADR sont à jour.
