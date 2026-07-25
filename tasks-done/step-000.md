# Jalon 000 — Fondations du dépôt, tooling, CI et release

> **Statut :** Terminé (2026-07-25) — 12 critères sur 14 vérifiés · **Dépend de :** — · **Réf. spec :** §6, §20.4, §21 · **Réf. guide :** §3, §4, §14, §15, §19, §20

## 1. Objectif

Disposer d'un dépôt complet et vide de logique métier, dans lequel `just lint && just test && just build` réussit sur les trois OS, et où un tag `vX.Y.Z` déclenche automatiquement la production des paquets natifs.

Ce jalon ne produit **aucune fonctionnalité SMPP**. Il met en place le squelette du workspace, les garde-fous automatisés (format, lint, tests, audit, licences) et les deux pipelines — intégration continue et release — sur lesquels tous les jalons suivants s'appuient. Toute règle du guide d'ingénierie vérifiable par une machine doit être outillée ici, pas plus tard : un garde-fou ajouté après coup se heurte à une dette déjà constituée.

## 2. Périmètre

### Dans le périmètre

- Workspace Cargo avec les **9 crates métier** créées en squelette (`lib.rs` avec `//!` d'en-tête, aucune logique), plus `src-tauri`.
- Dossier `ui/` : Vite + React 18 + TypeScript strict, Tailwind, structure `views/ components/ store/ ipc/ i18n/` (vide, l'app affiche une page placeholder).
- Configuration de qualité : `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.editorconfig`, `.env.example`, `eslint`/`prettier`/`tsconfig`.
- `justfile` avec les recettes `fmt`, `lint`, `test`, `audit`, `build`, `migrate`.
- Documentation de contribution : `README.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `CODEOWNERS`, modèle de PR, `docs/adr/0000-template.md` et les premières ADR.
- Vérification des messages de commit (Conventional Commits) en local et en CI.
- **Pipeline CI** `.github/workflows/ci.yml` : matrice Windows/macOS/Linux, 9 étapes bloquantes.
- **Pipeline release** `.github/workflows/release.yml` : déclenchement sur tag `v*`, `tauri-action`, artefacts en *draft release*.
- Table de traçabilité `EF-*` → jalon (§7 de ce fichier).

### Hors périmètre

- Toute dépendance à `rusmpp`, `sqlx`, `governor`, `phonenumber` autre qu'une déclaration non utilisée — l'intégration réelle appartient aux jalons 002, 003, 007, 009 et 013.
- Le shell applicatif réel, le routing, l'i18n peuplé et le contrat IPC → **step-001**.
- Les migrations SQL et le schéma → **step-002**.
- La signature de code, la notarisation et l'updater → **step-016** ; ce jalon prépare seulement la structure du workflow et documente les secrets attendus.
- Les tests d'intégration contre un simulateur SMSC → **step-017**.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-000-01 | Workspace Cargo + 9 crates squelettes | `Cargo.toml`, `crates/*/` |
| L-000-02 | Application Tauri 2.x minimale qui démarre | `src-tauri/`, `src-tauri/tauri.conf.json` |
| L-000-03 | Frontend Vite/React/TS strict avec arborescence par domaine | `ui/` |
| L-000-04 | Configuration qualité et éditeur | `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.editorconfig`, `ui/.eslintrc.cjs`, `ui/tsconfig.json` |
| L-000-05 | Recettes de tâches standardisées | `justfile` |
| L-000-06 | Documentation projet et contribution | `README.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `CODEOWNERS`, `.github/pull_request_template.md` |
| L-000-07 | Modèle et premières ADR | `docs/adr/0000-template.md`, `docs/adr/0001…0004-*.md` |
| L-000-08 | Pipeline d'intégration continue multi-OS | `.github/workflows/ci.yml` |
| L-000-09 | Pipeline de release et packaging | `.github/workflows/release.yml` |
| L-000-10 | Variables d'environnement documentées sans valeurs | `.env.example` |

## 4. Critères d'acceptation

- [x] **CA-000-01** — `cargo metadata --no-deps` liste exactement les 9 crates métier + `src-tauri` ; `cargo build --workspace` réussit.
- [x] **CA-000-02** — `cargo fmt --all --check` ne produit aucun diff.
- [x] **CA-000-03** — `cargo clippy --workspace --all-targets --all-features -- -D warnings` ne produit aucun warning.
- [x] **CA-000-04** — `pnpm -C ui tsc --noEmit` et `pnpm -C ui lint` réussissent ; `tsconfig.json` a bien `"strict": true` et `"noUncheckedIndexedAccess": true`.
- [x] **CA-000-05** — `cargo nextest run --workspace` et `pnpm -C ui test` réussissent (au moins un test trivial par crate prouve que le harnais fonctionne).
- [x] **CA-000-06** — `cargo deny check` (avances, bans, licenses, sources) et `cargo audit` réussissent ; `deny.toml` n'autorise que des licences permissives.
- [x] **CA-000-07** — `pnpm tauri dev` ouvre une fenêtre affichant la page placeholder sur la machine de développement.
- [x] **CA-000-08** — `just fmt`, `just lint`, `just test`, `just audit` s'exécutent et reflètent les critères ci-dessus.
- [ ] **CA-000-09** — Le workflow CI s'exécute sur `pull_request` et `push: main`, en matrice `ubuntu-22.04` / `macos-latest` / `windows-latest`, et **échoue** si l'une des 9 étapes du §5 échoue (vérifié par une PR de démonstration introduisant volontairement un warning clippy, puis corrigée).
- [ ] **CA-000-10** — Un tag `v0.0.1` sur une branche de test déclenche `release.yml`, produit une *draft release* contenant au minimum : `.msi` + `.exe` (Windows), `.dmg` (macOS aarch64 et x86_64), `.deb` + `.rpm` + `.AppImage` (Linux), et n'écrase jamais une release publiée.
- [x] **CA-000-11** — Un commit dont le message ne respecte pas Conventional Commits est rejeté localement (hook) et en CI (job `commitlint`).
- [x] **CA-000-12** — Aucun secret n'est présent dans le dépôt : `.env` est ignoré, `.env.example` liste les clés sans valeurs, et les secrets de signature sont référencés uniquement via `${{ secrets.* }}`.
- [x] **CA-000-13** — La CI met en cache le registre Cargo, la cible de build et le store pnpm ; un second run sans changement de dépendances est sensiblement plus rapide que le premier.
- [x] **CA-000-14** — Les ADR 0001 à 0004 existent et sont référencées depuis `README.md`.

> **Deux critères en attente, pas abandonnés.** CA-000-09 (PR de démonstration
> cassant volontairement la CI) et CA-000-10 (tag `v0.0.1` produisant une
> *draft release*) exigent des opérations distantes — pousser des commits
> volontairement fautifs, créer puis supprimer un tag. La portée accordée pour
> ce jalon se limitait au push de la branche et à une PR en brouillon.
>
> CA-000-13, en revanche, **est** vérifié : deux runs consécutifs sans
> changement de dépendances ont été comparés sur la PR #1 —
> `windows-latest` 407 s → 170 s, `ubuntu-22.04` 257 s → 119 s,
> `macos-latest` 236 s → 75 s, soit un facteur 2,2 à 3,1.
>
> Les deux protocoles sont rédigés intégralement, commandes comprises, dans
> [CONTRIBUTING.md §6](../CONTRIBUTING.md#6-vérification-des-pipelines). Ils y
> deviennent une procédure permanente à rejouer à chaque modification des
> workflows, plutôt qu'un rituel jetable : un pipeline dont on n'a jamais
> observé l'échec n'est pas un pipeline vérifié.
>
> Ce qui a été vérifié à leur place : les deux workflows passent `actionlint` ;
> le garde-fou de `release.yml` a été simulé localement sur la concordance
> tag/version ; le volet local de CA-000-11 est prouvé — le hook a réellement
> rejeté deux commits pendant la construction de ce jalon.

## 5. Détail des deux pipelines

### 5.1 CI — `.github/workflows/ci.yml`

Déclencheurs : `pull_request`, `push` sur `main`. Matrice OS : `ubuntu-22.04`, `macos-latest`, `windows-latest`. `fail-fast: false`.

Étapes bloquantes (guide §15.1) :

| # | Étape | Commande |
|---|-------|----------|
| 1 | Format | `cargo fmt --all --check` + `pnpm -C ui prettier --check .` |
| 2 | Lint Rust | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| 3 | Lint/types TS | `pnpm -C ui tsc --noEmit` + `pnpm -C ui eslint .` |
| 4 | Types IPC générés | régénération puis `git diff --exit-code ui/src/ipc/` |
| 5 | Tests | `cargo nextest run --workspace` + `pnpm -C ui test --run` |
| 6 | Doctests | `cargo test --doc --workspace` |
| 7 | Supply chain | `cargo audit` + `cargo deny check` |
| 8 | Migrations | application sur base neuve + vérification du schéma (actif dès **step-002**) |
| 9 | Build applicatif | `pnpm tauri build` — sur `main` et sur tag uniquement, pour ne pas allonger chaque PR |

Jobs séparés (une seule fois, pas par OS) : `commitlint` sur les commits de la PR, et `deny`/`audit` si l'on veut éviter la redondance de matrice.

Optimisations attendues : `swatinem/rust-cache@v2` (avec `workspaces: './src-tauri -> target'` en plus du workspace racine), cache du store pnpm via `actions/setup-node` (`cache: 'pnpm'`), dépendances système Linux (`libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `librsvg2-dev`, `patchelf`, `libssl-dev`, `libsecret-1-dev`).

### 5.2 Release — `.github/workflows/release.yml`

Déclencheurs : `push` sur tag `v*` et `workflow_dispatch`. Permission `contents: write`.

Matrice conforme à la documentation Tauri 2 :

| Plateforme | `args` | Sortie |
|-----------|--------|--------|
| `macos-latest` | `--target aarch64-apple-darwin` | `.dmg` / `.app` Apple Silicon |
| `macos-latest` | `--target x86_64-apple-darwin` | `.dmg` / `.app` Intel |
| `ubuntu-22.04` | — | `.deb`, `.rpm`, `.AppImage` |
| `windows-latest` | — | `.msi` (WiX), `.exe` (NSIS) |

Étapes : checkout → dépendances système Linux → `actions/setup-node` (Node 20, cache pnpm) → `dtolnay/rust-toolchain@stable` (avec les cibles Apple sur macOS) → `swatinem/rust-cache@v2` → `pnpm install` → `tauri-apps/tauri-action@v1` avec `tagName`, `releaseName`, `releaseDraft: true`, `prerelease: false`.

Emplacements réservés (remplis en **step-016**, documentés dès maintenant dans `CONTRIBUTING.md`) : `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`, `KEYCHAIN_PASSWORD`, `WINDOWS_CERTIFICATE`, `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Sans ces secrets, le workflow produit des artefacts **non signés** et le signale explicitement dans les logs plutôt que d'échouer silencieusement.

## 6. Tests attendus

- **Par crate :** un test trivial (`mod tests { #[test] fn crate_compile_et_expose_son_module() }`) pour valider le harnais `nextest` sur les 9 crates.
- **Frontend :** un test `vitest` sur le composant placeholder.
- **CI (test du test) :** une PR jetable introduisant successivement (a) un diff de formatage, (b) un warning clippy, (c) une erreur `tsc`, (d) un message de commit non conforme — chacun doit faire échouer le job attendu, et lui seul.
- **Release (test du test) :** un tag `v0.0.1` sur branche de test, draft release supprimée après vérification.

## 7. Traçabilité des exigences

Chaque exigence fonctionnelle de la spec §3 est couverte par au moins un jalon :

| Exigences | Jalon |
|-----------|-------|
| EF-CNX-01, EF-CNX-02, EF-CNX-05, EF-CNX-06 | step-005 |
| EF-CNX-03, EF-CNX-08 | step-011 |
| EF-CNX-04 | step-005 (choix de version) puis step-012 (v5.0 complet) |
| EF-CNX-07 | step-015 |
| EF-MSG-01, EF-MSG-05 | step-006 |
| EF-MSG-03, EF-MSG-04 | step-004 |
| EF-MSG-02, EF-MSG-08, EF-MSG-09 | step-010 |
| EF-MSG-06 | step-006 (TLV par message) puis step-012 (TLV v5.0) |
| EF-MSG-07 | step-006 (`submit_sm_resp`) puis step-008 (DLR) |
| EF-CTC-01 → EF-CTC-05, EF-CTC-07 | step-009 |
| EF-CTC-06 | step-013 |
| EF-DBT-01, EF-DBT-02, EF-DBT-04 | step-007 |
| EF-DBT-03 | step-012 |
| EF-LOG-01, EF-LOG-02 | step-008 |
| EF-LOG-03, EF-LOG-04, EF-LOG-05, EF-LOG-06 | step-014 |
| EF-CFG-01, EF-CFG-03 | step-001 (config) et step-005 (profils de session) |
| EF-CFG-02 | step-015 |
| EF-CFG-04 | step-001 |
| ENF-PERF-* | step-007, step-010, step-017 |
| ENF-FIA-* | step-010 |
| ENF-POR-*, packaging | step-016 |

## 8. Notes d'implémentation

- **Avant de coder, consulter la doc à jour :**
  ```bash
  npx ctx7@latest docs /tauri-apps/tauri-docs "Tauri 2 project structure, tauri.conf.json capabilities and CSP configuration"
  npx ctx7@latest docs /tauri-apps/tauri-docs "GitHub Actions workflow with tauri-action to build and release on Windows macOS Linux"
  ```
- **Décisions à consigner en ADR dès ce jalon :**
  - `0001-choix-de-la-pile-smpp.md` — rusmpp/rusmppc, et **à quel niveau d'API** on se branche (bas niveau `Framed<S, CommandCodec>` vs client haut niveau `rusmppc::ConnectionBuilder`). Ce choix conditionne `smpp-core` et `smpp-session` ; l'arbitrage définitif peut être différé à step-003 mais l'ADR est ouverte ici.
  - `0002-persistance-sqlite-sqlx.md` — SQLx (async, requêtes vérifiées à la compilation) plutôt que rusqlite.
  - `0003-generation-des-types-ipc.md` — **tauri-specta** pour générer `ui/src/ipc/`.
  - `0004-gestionnaire-de-paquets-frontend.md` — pnpm.
- **Points de vigilance :**
  - Le job « types IPC générés » (étape 4) n'a de sens qu'une fois le générateur branché en step-001 ; le prévoir dès maintenant mais l'autoriser à passer à vide tant qu'aucun DTO n'existe — sans jamais le rendre permissif après coup.
  - Ne pas activer `pnpm tauri build` sur chaque PR : c'est le poste de coût dominant de la CI. Réserver aux pushes sur `main` et aux tags.
  - `ubuntu-22.04` est requis pour `webkit2gtk-4.1` ; ne pas basculer sur `ubuntu-latest` sans revérifier la disponibilité du paquet.
  - Les crates squelettes doivent déjà porter leurs **frontières de dépendance** dans les `Cargo.toml` (`smpp-core` sans dépendance interne, aucune crate métier ne dépendant de `tauri`) : la violation devient ainsi une erreur de compilation dès le premier jalon suivant.
