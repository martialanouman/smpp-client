# ADR 0004 — Utiliser pnpm et placer le `package.json` à la racine

> **Statut :** Accepté
> **Date :** 2026-07-25 · **Jalon :** step-000 · **Décideur :** Martial Anouman

## Contexte

Le guide §3.1 recommande `pnpm`. Cette ADR l'entérine, mais surtout elle
tranche une question que ni la spec ni le guide n'abordent et qui structure
tout le dépôt : **où vit le `package.json` ?**

La spec §21 place le frontend dans `ui/`. Mais trois exigences tirent dans
l'autre sens :

1. CLAUDE.md §5 et le guide §3.3 lancent `pnpm tauri dev` **depuis la
   racine** : `@tauri-apps/cli` doit y être résoluble.
2. Husky v9 installe ses hooks via `core.hooksPath=.husky/_`, chemin relatif à
   la racine du dépôt git. Piloté depuis `ui/`, le montage est fragile.
3. CA-000-04 exige que `pnpm -C ui tsc --noEmit` et `pnpm -C ui lint`
   fonctionnent.

## Options envisagées

### Option A — Un seul `package.json`, dans `ui/`

**Pour :** un seul manifeste, arborescence minimale.
**Contre :** `pnpm tauri dev` depuis la racine devient impossible sans
contorsion ; husky doit être configuré à contre-emploi ; les outils de dépôt
(commitlint) se retrouvent dans le manifeste du frontend, où ils n'ont rien à
faire.

### Option B — Deux `package.json` indépendants

**Pour :** séparation nette.
**Contre :** deux `node_modules`, deux lockfiles, deux installations à tenir
synchronisées. Le coût est récurrent, pour aucun bénéfice.

### Option C — Workspace pnpm : `package.json` racine + `ui/` comme membre

**Pour :** satisfait les trois exigences simultanément ; un seul
`pnpm-lock.yaml` ; l'outillage de dépôt (Tauri CLI, husky, commitlint) vit à
la racine, les dépendances d'interface dans `ui/` — la séparation est
conceptuelle plutôt que physique, ce qui est la bonne granularité ici.
**Contre :** un fichier de plus (`pnpm-workspace.yaml`) et la notion de
workspace à connaître.

## Décision

**Option C — workspace pnpm avec `package.json` à la racine.**

Ce n'est pas un choix de préférence : c'est la seule disposition qui satisfait
les trois exigences à la fois. `pnpm` lui-même se justifie par le stockage par
liens durs (installation rapide, peu d'espace) et par sa résolution stricte,
qui interdit d'importer un paquet non déclaré — une dépendance fantôme est une
erreur à l'installation, pas une surprise en production.

## Conséquences

- **Positives :** `pnpm install` à la racine installe tout ; `pnpm tauri dev`
  fonctionne depuis la racine ; le hook commitlint se monte naturellement ; un
  seul lockfile à revoir.
- **Négatives / dette assumée :** deux manifestes à maintenir. La règle est
  simple et doit le rester : **outillage de dépôt à la racine, dépendances
  d'interface dans `ui/`**.
- **Impacts opérationnels :**
  - pnpm ≥ 10 bloque par défaut les scripts de post-installation. `esbuild` en
    a besoin, d'où la liste `onlyBuiltDependencies` de `pnpm-workspace.yaml` —
    à ne pas élargir sans vérifier le besoin réel, c'est de l'exécution de
    code à l'installation.
  - `engine-strict=true` dans `.npmrc` : une version de Node non conforme à
    `engines` échoue plutôt que de diverger silencieusement de la CI.
  - En CI, `pnpm install --frozen-lockfile` refuse tout lockfile désynchronisé.
- **Point de réexamen :** si un second paquet frontend apparaissait (par
  exemple une bibliothèque de composants partagée), la structure de workspace
  l'accueille déjà sans changement.

## Références

- Spec §21 (arborescence) · Guide §3.1, §3.3 · CLAUDE.md §5
- `pnpm-workspace.yaml`, `package.json`, `.npmrc`
