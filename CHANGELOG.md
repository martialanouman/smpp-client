# Changelog

Toutes les modifications notables de ce projet sont consignées ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) et le
versionnage suit [SemVer](https://semver.org/lang/fr/) : `feat` → mineur,
`fix` → correctif, changement cassant du contrat IPC ou du format de données →
majeur.

## [Non publié]

### Ajouté — jalon 003, cœur protocolaire

- **Codec PDU SMPP v3.4 et v5.0** : `encode`/`decode` d'un PDU complet, les
  33 opérations de la spec §7.2 couvertes et vérifiées par un test qui échoue
  si le codec en connaît une que la liste ignore.
- **Frontière du PDU décidée depuis `command_length`**, et non depuis ce que
  le codec laisse dans le tampon : un TLV vendeur appendu à un `bind` est
  reconnu comme interne au PDU, et le framing du jalon 005 ne peut plus se
  désynchroniser en silence.
- **Table des `command_status`** exhaustive v3.4/v5.0 — valeur, nom
  symbolique, libellés FR et EN, classification Fatal / Récupérable /
  Throttling. Un statut inconnu retombe sur `Fatal`, le choix conservateur.
- **Newtypes du domaine** (`Msisdn`, `SessionId`, `ClientMessageId`,
  `SequenceNumber`) en *parse, don't validate* : champ privé, validation dans
  le constructeur, invalidité irreprésentable en aval.
- **`debug::redacted`** — la voie sanctionnée pour journaliser une commande.
  Le `derive(Debug)` des types réexportés laisse fuir le mot de passe SMSC ;
  un test l'assert dans les deux sens.
- **Job CI `msrv`** : la version minimale de Rust est calculée depuis le
  graphe complet et vérifiée par une toolchain épinglée. Elle était déclarée
  à 1.78 depuis le jalon 000, valeur jamais vraie et que rien ne vérifiait.

### Décisions

- [ADR 0001](docs/adr/0001-choix-de-la-pile-smpp.md) passe en **Accepté** :
  niveau d'API rusmpp tranché au **niveau bas** (`CommandCodec`), `rusmppc`
  écarté. Le critère décisif est la propriété de la corrélation des
  `sequence_number`, dont les jalons 005, 007 et 012 ont besoin.
- [ADR 0006](docs/adr/0006-version-minimale-de-rust.md) : MSRV portée à
  **1.88**, calculée depuis le graphe et vérifiée en CI.

### Ajouté

- **Workspace Cargo** avec les neuf crates métier squelettes (`smpp-core`,
  `smpp-session`, `rate-control`, `messaging`, `contacts`, `numbers-gen`,
  `persistence`, `logging-export`, `security`) et `src-tauri`, membre du même
  workspace. Les frontières de dépendance du guide §4.2 sont inscrites dans
  les manifestes : un import remontant est rejeté par cargo comme cycle.
- **Règles de code appliquées par la machine** via `[workspace.lints]` et
  `clippy.toml` : `unwrap`, `expect`, `panic`, `todo`, `println!`,
  `std::thread::sleep`, `std::sync::Mutex`, les casts tronquants et les items
  publics sans documentation deviennent des erreurs de compilation.
  `unsafe_code` est en `forbid`, inlevable localement. `unwrap` et `expect`
  restent permis sous `#[cfg(test)]`.
- **Coquille Tauri 2** avec CSP stricte, capacités réduites à `core:default`,
  et démarrage sans `.expect()` — l'erreur remonte par `anyhow` jusqu'à `main`.
- **Frontend** Vite, React 19, TypeScript strict (`strict`,
  `noUncheckedIndexedAccess`), Tailwind v4, vitest, i18n amorcé en FR/EN.
  ESLint interdit `any`, `console` et l'import de `@tauri-apps/*` hors de
  `ui/src/ipc/`.
- **Pipeline CI** multi-OS (`ubuntu-22.04`, `macos-latest`,
  `windows-latest`) : formatage, lint Rust et TypeScript, types IPC générés,
  tests, doctests, chaîne d'approvisionnement, migrations, et paquets natifs
  sur `main` uniquement.
- **Pipeline release** déclenché par tag `v*`, avec un job de garde qui refuse
  d'écraser une release publiée et vérifie la concordance entre le tag et la
  version applicative.
- **Vérification des messages de commit** en local (hook husky) et en CI,
  Conventional Commits en français.
- **Contrôle de la chaîne d'approvisionnement** : `cargo audit` et
  `cargo deny` (licences, bannissements, provenance). `openssl` est banni —
  TLS exclusivement via rustls.
- **Scripts à cliquet** `check-ipc-types.sh` et `check-migrations.sh` : ils
  passent tant que le générateur et les migrations n'existent pas, mais
  échouent dès qu'un artefact apparaît sans son producteur.
- **Recettes `just`** : `fmt`, `fmt-check`, `lint`, `test`, `audit`, `check`,
  `dev`, `build`, `migrate`, `ipc-check`, `migrate-check`.
- **Documentation** : README, CONTRIBUTING, modèle de PR, CODEOWNERS, et les
  ADR 0001 à 0005.

### Notes

Ce jalon ne produit **aucune fonctionnalité SMPP**. L'application démarre sur
une page d'attente ; les crates métier sont des squelettes documentés.

Deux critères du jalon 000 restent à vérifier — CA-000-09 (démonstration
d'échec de la CI) et CA-000-10 (tag de test produisant une *draft release*) —
faute de portée distante accordée. Les protocoles sont décrits dans
[CONTRIBUTING.md](CONTRIBUTING.md#6-vérification-des-pipelines).

[Non publié]: https://github.com/martialanouman/smpp-client/commits/main
