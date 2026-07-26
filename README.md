# ShinobiSMPP

Client **ESME SMPP** de qualité production — application de bureau
multiplateforme (Windows 10/11, macOS 12+ Intel et Apple Silicon, Linux x64).

Se connecte à un ou plusieurs SMSC via des sessions SMPP **v3.4** et **v5.0**,
envoie des messages unitaires ou en masse, importe des contacts, génère des
numéros valides par pays, journalise et exporte.

> **État : jalon 000 terminé.** Le dépôt contient le socle — workspace, CI,
> release, garde-fous — et **aucune fonctionnalité SMPP**. L'application
> démarre sur une page d'attente. Voir [`tasks-done/`](tasks-done/) et
> [`tasks-todo/`](tasks-todo/).

## Prérequis

| Outil | Version | Rôle |
|---|---|---|
| Rust (via `rustup`) | ≥ 1.93 | Backend et crates métier |
| Node.js | 24 (voir [`.nvmrc`](.nvmrc)) | Build du frontend |
| pnpm | ≥ 11 | Dépendances frontend |
| `just` | — | Recettes de tâches |
| SQLite | ≥ 3.40 | Base embarquée (à partir du jalon 002) |

Outils Rust complémentaires :

```bash
rustup component add rustfmt clippy
cargo install cargo-binstall --locked
cargo binstall -y cargo-nextest cargo-deny cargo-audit
```

Dépendances système : **Linux** `libwebkit2gtk-4.1-dev`, `libssl-dev`,
`libsecret-1-dev`, `build-essential`, `pkg-config` · **macOS** Xcode Command
Line Tools · **Windows** WebView2 Runtime et Build Tools C++ (MSVC).

## Démarrage

```bash
pnpm install
cp .env.example .env
pnpm tauri dev
```

## Commandes

| Commande | Effet |
|---|---|
| `just fmt` | Formate Rust et TypeScript |
| `just fmt-check` | Vérifie le formatage sans modifier |
| `just lint` | clippy `-D warnings`, `tsc --noEmit`, eslint |
| `just test` | `cargo nextest`, doctests, vitest |
| `just audit` | `cargo audit` et `cargo deny` |
| `just check` | Les quatre précédentes — à passer avant tout commit |
| `just dev` | Application en rechargement à chaud |
| `just build` | Paquets natifs |

`just lint` et `just test` doivent être verts **avant** tout commit.

## Architecture

```
ui (TS)  ──invoke/events──►  src-tauri  ──►  messaging · contacts · numbers-gen · logging-export
                                              │              │
                                              ▼              ▼
                                        smpp-session    persistence · security
                                              │
                                              ▼
                                         smpp-core (rusmpp)
```

Les dépendances vont **du haut vers le bas**, jamais l'inverse. Aucune crate
métier ne dépend de `tauri`. Ces frontières sont inscrites dans les
`Cargo.toml` : un import remontant est rejeté par cargo comme cycle.

| Crate | Rôle |
|---|---|
| [`smpp-core`](crates/smpp-core) | Codec PDU et machine à états v3.4/v5.0 |
| [`smpp-session`](crates/smpp-session) | Sessions, fenêtrage, reconnexion |
| [`rate-control`](crates/rate-control) | Débit et adaptation à la congestion |
| [`messaging`](crates/messaging) | Encodage, segmentation, orchestration |
| [`contacts`](crates/contacts) | Import CSV/XLSX, validation E.164 |
| [`numbers-gen`](crates/numbers-gen) | Génération de numéros par pays |
| [`persistence`](crates/persistence) | SQLite, migrations, repositories |
| [`logging-export`](crates/logging-export) | Journal métier et exports |
| [`security`](crates/security) | Secrets, trousseau OS, TLS |

**Langues :** le code, les commentaires et les messages d'erreur sont en
**anglais** ; la documentation, les ADR et les messages de commit sont en
**français** ; l'interface est traduite (FR par défaut, EN).

## Décisions d'architecture

Toute décision structurante est consignée dans une ADR immuable.

- [ADR 0001 — Adopter rusmpp comme pile SMPP, au niveau codec](docs/adr/0001-choix-de-la-pile-smpp.md)
- [ADR 0002 — Persister avec SQLite via SQLx](docs/adr/0002-persistance-sqlite-sqlx.md)
- [ADR 0003 — Générer les types IPC avec tauri-specta](docs/adr/0003-generation-des-types-ipc.md)
- [ADR 0004 — Utiliser pnpm et placer le `package.json` à la racine](docs/adr/0004-gestionnaire-de-paquets-frontend.md)
- [ADR 0005 — Fixer les versions de la chaîne frontend](docs/adr/0005-versions-de-la-chaine-frontend.md)
- [ADR 0006 — Relever la version minimale de Rust à 1.85](docs/adr/0006-version-minimale-de-rust.md)
- [Modèle d'ADR](docs/adr/0000-template.md)

## Documentation

| Document | Rôle |
|---|---|
| [`docs/spec_smpp_client.md`](docs/spec_smpp_client.md) | **Le quoi** — exigences, architecture, modèle de données |
| [`docs/guide_ingenierie_smpp.md`](docs/guide_ingenierie_smpp.md) | **Le comment** — conventions, tests, CI, sécurité |
| [`tasks-todo/`](tasks-todo/) | **Le quand** — jalons d'implémentation |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Workflow, commits, vérification des pipelines |
| [`CHANGELOG.md`](CHANGELOG.md) | Historique des versions |

## Licence

Propriétaire. Tous droits réservés.
