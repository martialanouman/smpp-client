# Guide d'Ingénierie — Client SMPP Multiplateforme (*ShinobiSMPP*)

**Version du document :** 1.0
**Date :** 23 juillet 2026
**Portée :** Conventions, workflow et bonnes pratiques d'ingénierie pour l'implémentation du projet
**Documents liés :** *Spécifications Techniques Client SMPP Multiplateforme v1.0* (`specs/specifications-techniques-client-smpp.md`)
**Pile :** Rust (édition 2021+) · Tauri 2.x · Frontend web (React/TypeScript) · SQLite

---

## Table des matières

1. [Objet et public](#1-objet-et-public)
2. [Principes d'ingénierie](#2-principes-dingénierie)
3. [Environnement de développement](#3-environnement-de-développement)
4. [Organisation du dépôt et des crates](#4-organisation-du-dépôt-et-des-crates)
5. [Conventions de code Rust](#5-conventions-de-code-rust)
6. [Gestion des erreurs](#6-gestion-des-erreurs)
7. [Programmation asynchrone et concurrence](#7-programmation-asynchrone-et-concurrence)
8. [Frontières de modules et règles de dépendance](#8-frontières-de-modules-et-règles-de-dépendance)
9. [Contrat IPC Tauri (backend ↔ frontend)](#9-contrat-ipc-tauri-backend--frontend)
10. [Conventions frontend](#10-conventions-frontend)
11. [Accès aux données et migrations](#11-accès-aux-données-et-migrations)
12. [Journalisation, traces et observabilité](#12-journalisation-traces-et-observabilité)
13. [Stratégie et conventions de test](#13-stratégie-et-conventions-de-test)
14. [Workflow Git et gestion des branches](#14-workflow-git-et-gestion-des-branches)
15. [Intégration continue (CI)](#15-intégration-continue-ci)
16. [Revue de code](#16-revue-de-code)
17. [Pratiques de sécurité](#17-pratiques-de-sécurité)
18. [Lignes directrices de performance](#18-lignes-directrices-de-performance)
19. [Versionnage et processus de release](#19-versionnage-et-processus-de-release)
20. [Documentation et ADR](#20-documentation-et-adr)
21. [Onboarding d'un nouveau développeur](#21-onboarding-dun-nouveau-développeur)
22. [Annexes — checklists et modèles](#22-annexes--checklists-et-modèles)

---

## 1. Objet et public

Ce guide définit **comment** l'équipe construit *ShinobiSMPP* : conventions de code, découpage en modules, gestion des erreurs, patterns de concurrence, tests, workflow Git, CI, sécurité et release. Il complète la spécification technique (le **quoi**) par les règles opérationnelles (le **comment**).

Il s'adresse à tout contributeur au dépôt : ingénieurs Rust (backend/protocole), développeurs frontend, relecteurs et mainteneurs. Les règles marquées **[MUST]** sont impératives et vérifiées en revue/CI ; celles marquées **[SHOULD]** sont fortement recommandées ; **[MAY]** relève du choix contextuel.

---

## 2. Principes d'ingénierie

1. **Le cœur métier est indépendant de l'UI et de Tauri. [MUST]** La logique SMPP, l'orchestration d'envoi, la persistance et les services vivent dans des crates testables sans WebView. `src-tauri` n'est qu'une couche d'adaptation IPC.
2. **Le frontend est non fiable. [MUST]** Toute donnée franchissant l'IPC est validée et normalisée côté Rust. Aucune logique protocolaire ni secret ne réside côté frontend.
3. **Asynchrone, sans blocage. [MUST]** Aucune opération bloquante (I/O disque/réseau lourde, CPU intensif) ne s'exécute sur le thread UI ni ne bloque le runtime async ; on utilise Tokio et, si besoin, `spawn_blocking`.
4. **Communication par messages plutôt qu'état partagé verrouillé. [SHOULD]** Les sessions et l'orchestrateur sont des acteurs coordonnés par canaux ; on minimise `Mutex`/`RwLock` sur des chemins chauds.
5. **Persistance write-ahead. [MUST]** Un message est persisté avant émission ; son état évolue de façon traçable et idempotente.
6. **Échouer proprement et visiblement. [MUST]** Les erreurs sont typées, remontées avec contexte, journalisées, et jamais silencieusement avalées (`unwrap`/`expect` interdits hors tests et invariants prouvés).
7. **Petit, composable, testé. [SHOULD]** Fonctions courtes à responsabilité unique, modules cohésifs, couverture des chemins critiques.
8. **Simplicité d'abord.** On n'introduit une abstraction, une dépendance ou une couche que lorsqu'un besoin réel le justifie (voir §17.5 et §20 ADR).

---

## 3. Environnement de développement

### 3.1 Prérequis

| Outil | Version min. | Rôle |
|-------|-------------|------|
| Rust (toolchain via `rustup`) | 1.78 | Compilation backend/crates |
| Node.js | 20 LTS | Build du frontend |
| Gestionnaire JS | `pnpm` (recommandé) | Dépendances frontend |
| Tauri CLI | 2.x (`cargo install tauri-cli` ou `pnpm add -D @tauri-apps/cli`) | Dev/build de l'app |
| SQLite | 3.40+ | Base embarquée (souvent fournie par la lib) |

**Dépendances système par OS :**

- **Linux :** `webkit2gtk-4.1`, `libssl-dev`, `libsecret-1-dev` (keyring), `build-essential`, `pkg-config`.
- **macOS :** Xcode Command Line Tools.
- **Windows :** WebView2 Runtime, Build Tools C++ (MSVC).

### 3.2 Composants de la toolchain Rust [MUST]

```bash
rustup component add rustfmt clippy
cargo install cargo-audit cargo-deny cargo-nextest sqlx-cli
```

### 3.3 Démarrage rapide

```bash
git clone <repo> && cd shinobismpp
pnpm install                 # dépendances frontend
cp .env.example .env         # variables locales (chemins, niveau de log)
pnpm tauri dev               # lance backend Rust + frontend en hot-reload
```

### 3.4 Commandes usuelles (Makefile / justfile) [SHOULD]

Un `justfile` (ou `Makefile`) standardise les tâches :

```
just fmt         # cargo fmt + prettier
just lint        # cargo clippy -D warnings + eslint + tsc
just test        # cargo nextest run + vitest
just audit       # cargo audit + cargo deny check
just build       # tauri build (paquets natifs)
just migrate     # sqlx migrate run
```

### 3.5 Configuration de l'éditeur [SHOULD]

- **rust-analyzer** activé, `clippy` comme *check on save*.
- Format à la sauvegarde (`rustfmt`, `prettier`).
- Fichier `.editorconfig` versionné (fins de ligne LF, UTF-8, indentation).

---

## 4. Organisation du dépôt et des crates

### 4.1 Rappel de structure

Le dépôt est un **workspace Cargo** multi-crates + un dossier frontend (voir spec §21). Chaque crate a une responsabilité unique et une frontière de dépendance explicite.

```
crates/
  smpp-core/        # codec PDU + machine à états (rusmpp)   — AUCUNE dépendance interne
  smpp-session/     # sessions, fenêtrage, reconnexion       — dépend de smpp-core, rate-control
  rate-control/     # limiteur de débit + adaptation congestion
  messaging/        # encodage, segmentation, orchestrateur  — dépend de smpp-session, persistence
  contacts/         # import CSV/XLSX, validation E.164
  numbers-gen/      # génération de numéros par pays
  persistence/      # SQLite (SQLx), migrations, repositories
  logging-export/   # journal métier + exports
  security/         # secrets, keyring, TLS
src-tauri/          # commandes IPC + événements (couche fine)
ui/                 # frontend React/TS
```

### 4.2 Règles de dépendance entre crates [MUST]

- Les dépendances vont **du haut niveau vers le bas niveau**, jamais l'inverse. `smpp-core` ne dépend d'aucune autre crate interne.
- **Aucune crate métier ne dépend de `tauri`.** Seul `src-tauri` connaît Tauri.
- Les types partagés entre couches vivent dans une crate `domain`/`types` légère (ou sont réexportés), pour éviter les dépendances cycliques.
- Un cycle de dépendances est une erreur de conception : **interdit** (échec CI via `cargo` naturellement, et revue).

### 4.3 Conventions de nommage des crates et modules

- Crates : `kebab-case` (`smpp-core`), noms de modules Rust : `snake_case`.
- Un module public expose un **API surface** minimal via `pub use` dans `lib.rs` ; le reste est `pub(crate)` ou privé.

---

## 5. Conventions de code Rust

### 5.1 Formatage et lint [MUST]

- `cargo fmt` (rustfmt par défaut) — aucun diff de format toléré en CI.
- `cargo clippy --all-targets --all-features -D warnings` — **zéro warning**. Toute exception est annotée localement (`#[allow(...)]`) avec un commentaire justificatif.

### 5.2 Nommage

| Élément | Convention | Exemple |
|---------|-----------|---------|
| Types, traits, enums | `PascalCase` | `SessionManager`, `SubmitSm` |
| Fonctions, variables, modules | `snake_case` | `send_message`, `window_size` |
| Constantes, statics | `SCREAMING_SNAKE_CASE` | `DEFAULT_WINDOW_SIZE` |
| Variables de durée de vie | courtes, explicites | `'a`, `'conn` |

Les noms reflètent le **domaine SMPP** (utiliser `submit_sm`, `enquire_link`, `command_status` plutôt que des synonymes inventés).

### 5.3 Structure des fichiers et visibilité

- Un type majeur par fichier lorsque c'est pertinent ; regrouper par cohésion fonctionnelle sinon.
- Visibilité **la plus restreinte possible** : privé par défaut, `pub(crate)` pour l'usage interne, `pub` seulement pour l'API de la crate.
- Pas de `pub` « par confort ». Chaque symbole public est un engagement d'API.

### 5.4 Interdits et règles fortes [MUST]

- **Pas de `unwrap()` / `expect()` / `panic!`** dans le code de production, sauf invariant démontré et commenté (`// SAFETY/INVARIANT: …`). Autorisés dans les tests.
- **Pas de `unsafe`** sauf nécessité absolue, isolé, commenté et couvert par des tests (revue obligatoire par un mainteneur).
- **Pas de `.clone()` réflexe** sur les chemins chauds ; préférer emprunts, `Arc`/`Cow` quand justifié.
- **Pas de blocage** dans du code async (`std::thread::sleep`, I/O bloquante) — utiliser les équivalents Tokio ou `spawn_blocking`.
- Pas de `println!`/`eprintln!` pour la journalisation : utiliser `tracing` (voir §12).

### 5.5 Documentation du code [SHOULD]

- Chaque item public porte un commentaire `///` décrivant contrat, invariants et cas d'erreur.
- Les modules portent un `//!` d'en-tête expliquant leur rôle.
- Les exemples de doc (`/// ````) compilent en CI (`cargo test --doc`).

### 5.6 Typage fort et invariants

- Encoder les invariants dans le **système de types** : *newtypes* (`struct Msisdn(String)`, `struct SessionId(Uuid)`), enums exhaustifs pour les états, plutôt que des chaînes/entiers nus.
- Les valeurs SMPP contraintes (TON, NPI, DCS, `command_status`) sont des **enums** typés avec conversion explicite depuis/vers les octets du fil.
- Préférer *parse, don't validate* : construire un type valide une fois (ex. `Msisdn::parse` retourne `Result`), puis manipuler le type sûr partout ensuite.

---

## 6. Gestion des erreurs

### 6.1 Deux familles d'erreurs [MUST]

- **Erreurs de bibliothèque (crates métier)** : type d'erreur **explicite par crate** via `thiserror`, exhaustif et signifiant (`SmppError`, `PersistenceError`, `ImportError`…). Jamais de `Box<dyn Error>` opaque dans une API publique de crate.
- **Erreurs applicatives (`src-tauri`, binaires, tests)** : `anyhow::Result` accepté pour agréger et contextualiser en bordure d'application.

### 6.2 Exemple de type d'erreur

```rust
#[derive(Debug, thiserror::Error)]
pub enum SmppError {
    #[error("échec du bind: command_status={0:#010x}")]
    BindRejected(u32),
    #[error("timeout de réponse pour seq={0}")]
    ResponseTimeout(u32),
    #[error("PDU invalide: {0}")]
    Decode(#[from] rusmpp::DecodeError),
    #[error("connexion perdue")]
    ConnectionLost,
}
```

### 6.3 Règles

- **Contextualiser** en remontant (`.map_err`, `anyhow::Context::context("…")`) plutôt que perdre l'origine.
- Les erreurs SMPP portent le `command_status` brut **et** son libellé pour l'affichage utilisateur.
- **Aucune erreur avalée** : tout `Result` est traité, propagé ou explicitement journalisé avec justification.
- À la frontière IPC, les erreurs sont converties en un DTO d'erreur stable (`{ code, message, details }`) ; jamais de fuite de détails internes sensibles (chemins, secrets).
- Distinguer erreurs **récupérables** (rejeu, back-off) et **fatales** (mauvais identifiants → pas de boucle de reconnexion).

---

## 7. Programmation asynchrone et concurrence

### 7.1 Runtime [MUST]

- **Tokio** multi-thread comme unique runtime. Pas de mélange avec d'autres exécuteurs.
- Le CPU intensif (parsing XLSX volumineux, génération massive, crypto) passe par `tokio::task::spawn_blocking` ou un pool dédié, jamais directement sur le runtime async.

### 7.2 Modèle d'acteurs pour les sessions [MUST]

Chaque session = tâches Tokio coopérantes (connection, writer, reader, keep-alive, supervisor) communiquant par canaux :

- `mpsc` pour les files d'émission et flux d'événements.
- `oneshot` pour corréler une requête (`submit_sm`) à sa réponse (`submit_sm_resp`), indexée par `sequence_number`.
- `watch` pour diffuser l'état/métriques d'une session à l'UI.
- **Une seule tâche possède le socket.** Les autres passent par un canal — pas de socket partagé.

### 7.3 État partagé

- Préférer le passage de messages. Quand un état partagé est inévitable : `Arc<…>` immuable, ou `Arc<Mutex<…>>`/`RwLock` à **granularité fine** et à section critique courte (jamais de `.await` en tenant un `std::sync::Mutex`).
- Pour un verrou tenu à travers un `.await`, utiliser `tokio::sync::Mutex`.

### 7.4 Annulation et arrêt propre [MUST]

- Toute tâche longue écoute un **`CancellationToken`** (crate `tokio-util`) et s'arrête proprement (unbind SMPP, flush persistance) à la fermeture de l'app ou à l'annulation d'une campagne.
- Les timeouts (`tokio::time::timeout`) encadrent bind, réponses et keep-alive.
- Back-pressure : files **bornées** (`mpsc` à capacité fixe) pour éviter l'explosion mémoire.

### 7.5 Interdits [MUST]

- Pas de `block_on` imbriqué dans du code déjà async.
- Pas de boucle *busy-wait* ; utiliser timers/notifications.
- Pas de tâche « orpheline » non supervisée : toute `spawn` a un propriétaire qui gère sa fin et ses erreurs (via `JoinHandle` ou canal de résultat).

---

## 8. Frontières de modules et règles de dépendance

### 8.1 Couches et sens des dépendances [MUST]

```
ui (TS)  ──invoke/events──►  src-tauri  ──►  messaging / contacts / numbers-gen / logging-export
                                              │            │
                                              ▼            ▼
                                        smpp-session   persistence   security
                                              │
                                              ▼
                                         smpp-core (rusmpp)
```

- Une couche ne connaît que les couches strictement inférieures. **Aucun import remontant.**
- Les traits d'abstraction (ports) sont définis dans la couche haute et implémentés dans la couche basse quand on veut inverser une dépendance (ex. `MessageRepository` défini côté `messaging`, implémenté côté `persistence`).

### 8.2 Injection de dépendances [SHOULD]

- Les collaborateurs (repositories, horloge, RNG, client SMPP) sont passés par **trait objet ou générique**, pas construits en dur, pour permettre les doubles de test.
- Exemple : `numbers-gen` reçoit un `Rng` injectable → génération déterministe par graine en test.

### 8.3 Couche `src-tauri` mince [MUST]

`src-tauri` ne contient **aucune** logique métier : uniquement (a) désérialisation/validation des entrées IPC, (b) appel des services métier, (c) sérialisation des sorties, (d) émission d'événements. Toute logique qui « déborde » doit descendre dans une crate métier.

---

## 9. Contrat IPC Tauri (backend ↔ frontend)

### 9.1 Commandes

- Une commande = une fonction `#[tauri::command] async fn` fine, nommée en `snake_case` (`message_send`, `session_bind`), retournant `Result<Dto, ErrorDto>`.
- **Validation d'entrée systématique** avant tout traitement (numéros, tailles, énumérations). Entrées invalides → `ErrorDto` explicite, pas de panic.

### 9.2 Types partagés générés [MUST]

- Les DTO sont définis **une seule fois en Rust** (`serde`) et les types TypeScript sont **générés** (`ts-rs` ou `tauri-specta`). Interdit de redéclarer manuellement les types côté TS (source de dérive).
- Toute évolution de DTO régénère les types en CI ; un diff non commité échoue la CI.

### 9.3 Événements

- Nommage `domaine:action` (`sessions:state`, `message:update`, `metrics:tick`).
- Charges utiles **petites et fréquentes** agrégées côté backend (throttling des `metrics:tick`, ex. 1–4 Hz) pour ne pas saturer l'IPC ni l'UI.
- Les gros volumes (logs, contacts) ne transitent **jamais** en bloc par événement : pagination via commande + requête.

### 9.4 Versionnement du contrat

- Le contrat IPC est documenté (§spec 15) et traité comme une API : tout changement cassant est signalé en revue et noté au CHANGELOG.

---

## 10. Conventions frontend

### 10.1 Stack et structure

- **React 18 + TypeScript strict** (`"strict": true`, `noUncheckedIndexedAccess`), Vite, Tailwind + shadcn/ui, état via Zustand.
- Arborescence par domaine : `views/`, `components/`, `store/`, `ipc/` (wrappers `invoke` typés), `i18n/`.

### 10.2 Règles [MUST]

- **Aucune logique métier/protocolaire côté frontend.** Le frontend affiche l'état et déclenche des commandes.
- Tout appel backend passe par les **wrappers typés** de `ipc/` (jamais `invoke` brut disséminé dans les composants).
- Les tables volumineuses (logs, contacts) sont **virtualisées** (TanStack Virtual) et paginées côté backend.
- Pas d'`any` implicite ; `eslint` + `tsc` bloquants en CI.
- Textes utilisateur via **i18n** (FR par défaut, EN) — pas de chaîne en dur dans les composants.

### 10.3 Style et accessibilité [SHOULD]

- Composants fonctionnels + hooks ; état local minimal, état partagé dans le store.
- Accessibilité : rôles/labels ARIA, navigation clavier, contraste WCAG AA, thèmes clair/sombre.
- Formatage `prettier` ; conventions de nommage `PascalCase` (composants), `camelCase` (variables/fonctions).

---

## 11. Accès aux données et migrations

### 11.1 Repositories [MUST]

- L'accès SQLite est encapsulé dans des **repositories** (`persistence`) exposant des méthodes métier (`insert_message`, `update_state`, `stream_messages(filter)`), jamais du SQL disséminé dans les couches hautes.
- Requêtes **vérifiées à la compilation** via `sqlx::query!`/`query_as!` lorsque possible.

### 11.2 Migrations [MUST]

- Migrations **versionnées et immuables** dans `migrations/` (`sqlx migrate`). On n'édite jamais une migration livrée : on en ajoute une nouvelle.
- Chaque migration est réversible ou documentée comme non réversible.
- La base tourne en **WAL** ; les écritures chaudes (états de messages) sont **groupées en transactions/batch** pour la performance.

### 11.3 Streaming et volumétrie

- Les gros ensembles (contacts, messages, exports) sont **streamés** (curseurs/pagination), jamais chargés intégralement en mémoire.
- Index requis sur les colonnes de filtre chaudes (`state`, `campaign_id`, `smsc_message_id`).

---

## 12. Journalisation, traces et observabilité

### 12.1 Traces techniques [MUST]

- **`tracing`** partout ; niveaux `error/warn/info/debug/trace`. Pas de `println!`.
- **Spans** structurés par session (`session_id`) et campagne (`campaign_id`) ; corrélation par `sequence_number` sur les échanges PDU.
- Champs structurés (`tracing`'s key-value), pas de concaténation de chaînes.
- Sortie fichier **rotative** (`tracing-appender`) + console en dev.

### 12.2 Secrets et confidentialité [MUST]

- **Jamais** de secret (mot de passe SMSC, clé) dans les traces, même en `trace`.
- Contenu des messages masqué/tronqué par défaut dans les logs partagés ; dump hexadécimal des PDU réservé au **mode debug** explicite.

### 12.3 Métriques

- Métriques exposées à l'UI (TPS, fenêtre, RTT, `congestion_state`, compteurs d'états) agrégées côté backend et publiées via événements throttlés.
- Bundle de diagnostic exportable (logs techniques + config anonymisée) pour le support.

---

## 13. Stratégie et conventions de test

### 13.1 Pyramide de tests [MUST]

| Niveau | Portée | Outils | Cible |
|--------|--------|--------|-------|
| Unitaire | fonctions/pures, codec, encodage, génération, validation | `cargo nextest`, `#[cfg(test)]` | ≥ 80 % du cœur |
| Intégration | contre simulateur SMSC (rusmpps/SMPPSim) : bind, submit, DLR, throttling, reconnexion | tests `tests/` | scénarios clés |
| Propriété | round-trip codec, invariants d'encodage, unicité numéros | `proptest` | chemins critiques |
| Performance | débit, back-pressure, mémoire | `criterion`, bancs de charge | objectifs spec §4.1 |
| Frontend | composants, E2E | `vitest`, `tauri-driver`/Playwright | vues critiques |

### 13.2 Conventions [MUST]

- Tests **déterministes** : pas de dépendance à l'horloge réelle ni au hasard non contrôlé → injecter horloge et RNG (graine fixe).
- Tests **isolés** : chaque test d'intégration utilise une base SQLite temporaire dédiée (fichier temporaire ou `:memory:`).
- Nommage : `mod tests { #[test] fn nom_du_cas_teste() }` ; un test = un comportement.
- Les corrections de bug ajoutent un **test de non-régression** reproduisant le bug.

### 13.3 Doubles de test

- Le client SMPP et les repositories sont abstraits par traits → substituables par des *fakes* en test unitaire.
- Un **simulateur SMSC** (serveur `rusmpps` embarqué) sert les tests d'intégration, avec injection de fautes (coupures, PDU malformés, throttling).

### 13.4 Couverture [SHOULD]

- Mesure via `cargo llvm-cov`. La couverture du cœur protocolaire et des modules critiques ne doit pas régresser (seuil en CI).

---

## 14. Workflow Git et gestion des branches

### 14.1 Modèle de branches [MUST]

- **Trunk-based léger** : branche `main` toujours livrable (verte en CI).
- Branches de travail courtes : `feat/<sujet>`, `fix/<sujet>`, `chore/<sujet>`, `docs/<sujet>`, `refactor/<sujet>`.
- Pas de commit direct sur `main` : tout passe par **Pull/Merge Request** avec revue et CI verte.

### 14.2 Commits — Conventional Commits [MUST]

Format : `type(scope): description` en français concis.

```
feat(smpp-core): support du PDU broadcast_sm (v5.0)
fix(rate-control): respect du TPS lors d'un burst initial
refactor(session): extraction du superviseur de reconnexion
test(numbers-gen): reproductibilité par graine
docs(guide): ajout de la section revue de code
```

- `feat`/`fix` alimentent le CHANGELOG et le SemVer (voir §19).
- Un commit = une intention cohérente ; historique lisible (rebase/squash à la fusion).

### 14.3 Pull Requests

- Petites, focalisées, avec description (contexte, changement, tests, captures si UI), et **liées à une exigence** (`EF-…`) ou une issue.
- CI verte + au moins une **approbation** (deux pour le cœur protocolaire et la sécurité) avant fusion.
- Modèle de PR fourni (§22).

---

## 15. Intégration continue (CI)

### 15.1 Pipeline [MUST]

Sur chaque PR et sur `main`, matrice **Windows / macOS / Linux** :

1. `cargo fmt --check` + `prettier --check`
2. `cargo clippy --all-targets --all-features -D warnings`
3. `tsc --noEmit` + `eslint`
4. Vérification des types générés (ts-rs) — pas de diff
5. `cargo nextest run` (unitaires + intégration) + `vitest`
6. `cargo test --doc`
7. `cargo audit` + `cargo deny check` (vulnérabilités + licences)
8. Migrations : application sur base neuve + vérif schéma
9. Build : `tauri build` (au moins sur `main`/release)

Un échec à toute étape **bloque la fusion**.

### 15.2 Optimisations

- Cache des dépendances (`cargo`/`sccache`, `pnpm store`).
- Jobs parallèles par OS ; build packaging seulement sur tag de release et `main`.

---

## 16. Revue de code

### 16.1 Attendus du relecteur [MUST]

Le relecteur vérifie, au-delà du style (couvert par CI) :

- **Correction** : la logique fait ce qu'annonce la PR ; cas limites SMPP couverts (encodage, segmentation, `command_status`, timeouts).
- **Frontières** : pas de dépendance remontante, `src-tauri` reste mince, pas de logique côté frontend.
- **Erreurs** : pas de `unwrap`/`panic` injustifié, erreurs typées et contextualisées.
- **Concurrence** : pas de blocage en async, pas de verrou tenu sur `.await`, tâches supervisées et annulables.
- **Sécurité** : pas de secret journalisé/exporté en clair, entrées validées, TLS respecté.
- **Tests** : nouveaux comportements testés, non-régression pour les bugs.
- **Performance** : pas d'allocation/clone inutile sur chemin chaud, streaming pour la volumétrie.

### 16.2 Règles de fusion

- Le cœur protocolaire (`smpp-core`, `smpp-session`) et `security` requièrent **deux approbations** dont un mainteneur.
- Les commentaires de revue bloquants sont résolus avant fusion ; les non-bloquants sont marqués `nit:`.
- L'auteur ne fusionne pas sa propre PR sans approbation.

---

## 17. Pratiques de sécurité

### 17.1 Secrets [MUST]

- Mots de passe SMSC chiffrés **AES-256-GCM** ; clé dans le **trousseau OS** via `keyring`. Option mot de passe maître → **Argon2id**.
- Aucun secret en clair : ni en base, ni en log, ni en export, ni en dur dans le code/les tests. Les fixtures de test utilisent des valeurs factices.
- `.env` et fichiers de secrets **gitignorés** ; `.env.example` documente les clés sans valeurs.

### 17.2 Transport [MUST]

- Support **TLS** (`tokio-rustls`) avec vérification de certificat activée par défaut ; avertissement UI explicite pour une session en clair.

### 17.3 Durcissement Tauri [MUST]

- Capacités/permissions Tauri **minimales** (pas de shell, FS restreint aux répertoires app + fichiers choisis via dialogues natifs).
- **CSP stricte**, pas de contenu distant, pas d'`eval`. Frontend traité comme non fiable.

### 17.4 Chaîne d'approvisionnement [MUST]

- `cargo audit` + `cargo deny` en CI (vulnérabilités + licences autorisées).
- `Cargo.lock` et `pnpm-lock.yaml` **commités** ; mises à jour de dépendances revues.
- Ajout de dépendance justifié (maintenance, licence, taille) — voir §17.5.

### 17.5 Politique de dépendances [SHOULD]

Avant d'ajouter une crate : vérifier maintenance active, licence compatible (permissive), popularité/audits, et absence d'alternative déjà présente. Une nouvelle dépendance lourde ou peu maintenue passe par une **ADR** (§20).

### 17.6 Usage responsable [MUST]

- Liste d'exclusion (opt-out) appliquée avant tout envoi ; plafonds de débit/volume avec confirmation ; journal d'audit des campagnes. Avertissements légaux sur la génération de numéros et l'envoi de masse.

---

## 18. Lignes directrices de performance

- **Mesurer avant d'optimiser.** Benchmarks `criterion` et profils avant toute optimisation micro.
- **Chemin chaud (émission)** : éviter allocations et `clone` par message ; réutiliser buffers ; éviter la sérialisation superflue.
- **Back-pressure** partout : files bornées ; la lecture des sources suit le rythme du SMSC.
- **Batch** des écritures d'état en base (transactions groupées) plutôt qu'une écriture par message.
- **UI réactive** : agrégation/throttling des événements ; virtualisation des listes ; jamais de traitement lourd dans la WebView.
- Objectifs de référence : ≥ 1 000 TPS/session, latence d'enfilement < 1 ms p99, mémoire au repos < 150 Mo (spec §4.1).

---

## 19. Versionnage et processus de release

### 19.1 SemVer [MUST]

- Version applicative en **SemVer** (`MAJEUR.MINEUR.CORRECTIF`). `feat` → mineur, `fix` → correctif, changement cassant du contrat IPC/format de données → majeur.

### 19.2 CHANGELOG

- **Keep a Changelog** alimenté à partir des Conventional Commits (génération assistée). Chaque release liste ajouts, corrections, changements cassants et migrations requises.

### 19.3 Procédure de release [MUST]

1. Geler `main`, vérifier CI verte multi-OS.
2. Passer par la **checklist de déploiement** (skill `deploy-checklist`) : migrations testées, secrets/keyring OK, TLS, notes de version.
3. Bump de version + tag `vX.Y.Z`.
4. CI de release : `tauri build` par OS → **signature** (Authenticode Windows, Developer ID + **notarisation** macOS) → checksums Linux.
5. Publication des artefacts + manifeste **updater** signé.
6. Vérification post-release (installation propre sur chaque OS, smoke test bind + envoi).

### 19.4 Rollback

- Conserver les artefacts N-1 ; un incident post-release suit le workflow de réponse à incident (skill `incident-response`) avec critères de rollback définis à l'avance.

---

## 20. Documentation et ADR

### 20.1 Documentation vivante [SHOULD]

- `README` (démarrage), ce guide, la spec technique, et des **runbooks** (diagnostic d'une session, procédure de release) tenus à jour avec le code.
- Doc d'API des crates via `cargo doc` ; contrat IPC documenté et généré.

### 20.2 Architecture Decision Records [MUST pour décisions structurantes]

- Toute décision structurante (choix de crate SMPP, moteur de base, stratégie de segmentation, mécanisme de throughput, format de secrets) est consignée dans un **ADR** (`docs/adr/NNNN-titre.md`) : contexte, options, décision, conséquences.
- Modèle d'ADR fourni via le skill `architecture`. Une ADR est immuable ; on la **supersède** par une nouvelle si la décision change.

---

## 21. Onboarding d'un nouveau développeur

Parcours cible (≈ 1 jour) :

1. Lire la **spec technique** (survol) puis ce guide (intégral).
2. Installer l'environnement (§3), lancer `pnpm tauri dev`, obtenir l'app en local.
3. Démarrer le **simulateur SMSC** de test et réaliser un **envoi simple** de bout en bout.
4. Lire le code de `smpp-core` (codec) et `smpp-session` (acteurs) — le cœur mental du projet.
5. Prendre une issue étiquetée `good-first-issue`, ouvrir une PR en respectant §14/§16.
6. Points de contact : mainteneurs du cœur protocolaire, référent frontend, référent sécurité (dans le `CODEOWNERS`).

Un fichier **`CONTRIBUTING.md`** résume ce parcours et pointe vers ce guide.

---

## 22. Annexes — checklists et modèles

### 22.1 Checklist d'ouverture de PR

- [ ] Portée unique, description claire, liée à une exigence/issue.
- [ ] `just fmt && just lint && just test` verts en local.
- [ ] Types IPC régénérés si DTO modifiés.
- [ ] Tests ajoutés (comportement + non-régression).
- [ ] Pas de `unwrap`/`panic`/secret/log en clair introduits.
- [ ] Migrations ajoutées (jamais éditées) si schéma modifié.
- [ ] CHANGELOG/ADR mis à jour si pertinent.

### 22.2 Checklist de revue

- [ ] Correction fonctionnelle et cas limites SMPP.
- [ ] Frontières de couches respectées, `src-tauri` mince.
- [ ] Erreurs typées/contextualisées, pas d'avalage.
- [ ] Async sans blocage, tâches supervisées/annulables.
- [ ] Sécurité (secrets, TLS, validation d'entrée).
- [ ] Tests suffisants et déterministes.
- [ ] Impact performance sur chemin chaud considéré.

### 22.3 Modèle de PR

```markdown
## Contexte
Exigence/issue : EF-___ / #___

## Changement
Résumé de ce qui change et pourquoi.

## Tests
Comment c'est testé (unitaire/intégration/manuel). Captures si UI.

## Risques / points d'attention
Compatibilité, migration, sécurité, performance.

## Checklist
- [ ] Lint/tests verts  - [ ] Types IPC à jour  - [ ] Docs/CHANGELOG
```

### 22.4 Définition de « terminé » (Definition of Done)

Une tâche est terminée quand : le code est fusionné dans `main` vert, les tests couvrent le comportement, la doc/CHANGELOG sont à jour, aucune régression de lint/audit/couverture, et l'exigence associée est démontrable (démo ou test d'acceptation).

---

*Fin du document — Guide d'Ingénierie Client SMPP Multiplateforme v1.0.*
