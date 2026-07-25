# Contribuer à ShinobiSMPP

Ce document complète [`CLAUDE.md`](CLAUDE.md) (les règles) et
[`docs/guide_ingenierie_smpp.md`](docs/guide_ingenierie_smpp.md) (le
raisonnement). En cas de contradiction, **la spec et le guide priment** ;
signalez l'écart plutôt que de trancher seul.

## 1. Mise en route

```bash
git clone git@github.com:martialanouman/smpp-client.git
cd smpp-client
pnpm install          # installe aussi les hooks git via husky
cp .env.example .env
pnpm tauri dev
```

Voir le [README](README.md#prérequis) pour les prérequis et les dépendances
système.

## 2. Langues

| Où | Langue |
|---|---|
| Code, identifiants, commentaires, doc-comments, messages d'erreur | **Anglais** |
| Messages de commit | **Français** (type conventionnel en anglais) |
| Documentation, ADR, CHANGELOG, jalons | **Français** |
| Textes de l'interface | i18n — **FR par défaut**, EN |

## 3. Avant de commiter

```bash
just check
```

Soit `fmt-check`, `lint`, `test` et `audit`. Ces quatre recettes doivent être
vertes en local ; la CI les rejoue sur les trois systèmes.

## 4. Messages de commit

**Conventional Commits, rédigés en français.** Le `type` reste en anglais :
ce n'est pas de la prose mais une grammaire lue par les machines — elle
détermine le bump SemVer et alimente le CHANGELOG. Traduire `feat` en `fonc`
casserait les deux.

```
feat(smpp-core): ajoute le support du PDU broadcast_sm (v5.0)
fix(rate-control): respecte le TPS cible lors d'un burst initial
docs(adr): consigne le choix du niveau d'API rusmpp
```

Le scope est un nom de crate ou de zone ; la liste exhaustive vit dans
[`commitlint.config.mjs`](commitlint.config.mjs). Un scope absent est accepté,
un scope inconnu est refusé — si votre changement en réclame un nouveau,
ajoutez-le à la liste dans le même commit.

La vérification est **doublée**, et les deux couches sont nécessaires :

- localement par `.husky/commit-msg`, qui rejette le commit à l'écriture ;
- en CI par le job `commitlint`, qui rattrape ce qu'un `--no-verify` a laissé
  passer.

Un commit = **une intention cohérente et complète**. On ne mélange pas un
refactor et une fonctionnalité, ni du formatage et de la logique.

## 5. Branches et fusion

Trunk-based léger : `main` toujours livrable, jamais de commit direct.
Branches courtes `feat/…`, `fix/…`, `chore/…`, `docs/…`, `refactor/…`,
`test/…`, puis PR avec CI verte.

**Fusionner en `rebase merge`, pas en `squash`.** Un squash écrase les commits
de la PR et ne conserve que son titre : le job `commitlint` perd alors son
objet sur l'historique de `main`, et les commits atomiques exigés par
CLAUDE.md §6 disparaissent.

> **Revue.** Le guide §14.3 exige au moins une approbation, deux pour
> `smpp-core`, `smpp-session` et `security`. GitHub interdisant d'approuver sa
> propre PR, cette exigence n'est pas activée tant que le dépôt a un seul
> mainteneur — elle le sera dès l'arrivée d'un second. La protection de `main`
> se limite donc aujourd'hui aux *status checks*.

**Ne jamais pousser de tag ni créer de release sans demande explicite.**

## 6. Vérification des pipelines

Ces deux protocoles ne sont pas des rituels ponctuels : **rejouez-les à chaque
modification de `ci.yml` ou de `release.yml`**. Un pipeline dont on n'a jamais
observé l'échec n'est pas un pipeline vérifié.

> **État au jalon 000 :** ces deux vérifications correspondent aux critères
> CA-000-09 et CA-000-10, **non exécutées** — la portée distante accordée se
> limitait au push de la branche et à une PR en brouillon. Elles restent donc
> en attente, pas abandonnées.

### 6.1 La CI échoue-t-elle vraiment, et sur le bon job ? (CA-000-09)

Sur une branche jetable partant de la tête de la branche de travail, pousser
**quatre commits successifs** — un par cas, jamais ensemble : c'est la
succession qui démontre l'isolation.

| # | Injection | Job attendu en échec |
|---|---|---|
| a | Un diff de formatage (`cargo fmt` défait sur un fichier) | `quality` → *Rust formatting* |
| b | Un warning clippy (par exemple `let x = y.clone();` sur un `Copy`) | `quality` → *Clippy* |
| c | Une erreur `tsc` (assigner un `number` à une `string`) | `quality` → *TypeScript types* |
| d | Un message de commit non conforme (`git commit --no-verify -m "oups"`) | `commitlint` |

Après **chaque** push, vérifier que le job attendu échoue **et lui seul** —
c'est `fail-fast: false` qui rend cette observation possible. Consigner les
quatre URL de run dans la PR, puis fermer la PR sans fusionner et supprimer la
branche.

Limite à annoncer : `pnpm tauri build` étant conditionné à `push: main`, cette
démonstration couvre les étapes 1 à 8, pas la 9.

### 6.2 La release produit-elle les artefacts attendus ? (CA-000-10)

```bash
git tag v0.0.1 <sha> && git push origin v0.0.1
```

Compter 30 à 60 minutes pour les quatre jobs. Vérifier que la *draft release*
contient au minimum : `.msi` **et** `.exe` (Windows), deux `.dmg` (macOS
aarch64 et x86_64), `.deb`, `.rpm` et `.AppImage` (Linux).

Vérifier aussi le job `guard` : relancer `release.yml` sur un tag dont la
release aurait été **publiée** doit échouer avant toute construction.

Nettoyage :

```bash
gh release delete v0.0.1 --yes
git push origin :refs/tags/v0.0.1
git tag -d v0.0.1
```

## 7. Secrets attendus

Renseignés au **jalon 016**. En leur absence, `release.yml` produit des
artefacts **non signés** et l'annonce par des `::warning::` explicites — il
n'échoue pas silencieusement.

| Secret | Usage |
|---|---|
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` | Certificat Developer ID |
| `APPLE_SIGNING_IDENTITY` | Identité de signature macOS |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | Notarisation |
| `KEYCHAIN_PASSWORD` | Trousseau temporaire du runner |
| `WINDOWS_CERTIFICATE` | Authenticode |
| `TAURI_SIGNING_PRIVATE_KEY`, `…_PASSWORD` | Manifeste de l'updater |

Aucune de ces valeurs n'apparaît dans le dépôt, ni dans `.env`, ni dans un
fichier de test. `.env` est gitignoré : s'il apparaît un jour dans
`git status`, c'est un bug.

## 8. Dette assumée et pièges connus

Ces points sont **connus et documentés**, pas oubliés. Les traiter le moment
venu, ne pas les redécouvrir.

| Sujet | Nature | Échéance |
|---|---|---|
| `justfile` et `ci.yml` désynchronisables | La CI reprend les commandes verbatim plutôt que d'appeler `just`, pour éviter d'installer un outil de plus sur trois OS. Aucun garde-fou automatique satisfaisant : toute modification de l'un impose de vérifier l'autre. | permanent |
| `'unsafe-inline'` sur `style-src` | Seule relaxation de la CSP, imposée par les styles inline de Vite. | jalon 016 |
| Options rustfmt nightly | `group_imports` et `imports_granularity` sont ignorées **sans erreur** sur stable : les écrire donnerait l'illusion d'une règle appliquée. Elles supposent un job `cargo +nightly fmt` dédié. | non planifié |
| Licences de `deny.toml` | La liste `allow` ne contient que les licences réellement rencontrées. Une nouvelle dépendance apportant une licence inédite **fera échouer la CI** : c'est voulu, ajoutez la ligne et justifiez-la. | permanent |
| 17 avertissements `cargo audit` | Tous `unmaintained`, dans la chaîne GTK 0.18 de Tauri. Sans alternative amont ; d'où `unmaintained = "workspace"` dans `deny.toml`. | suivi amont |
| `tauri-specta` en `2.0.0-rc` | Préversion — [ADR 0003](docs/adr/0003-generation-des-types-ipc.md). | jalon 001 |
| `ubuntu-22.04` épinglé | Requis pour `webkit2gtk-4.1`. Ne pas basculer sur `ubuntu-latest` sans revérifier la disponibilité du paquet. | permanent |
| Icônes provisoires | Monogramme de substitution, à remplacer par l'identité visuelle définitive. | jalon 016 |

## 9. Tests

Un test = **un comportement**, nommé explicitement. Tests **déterministes** :
horloge et RNG injectés, graine fixe, jamais de dépendance à l'heure réelle.
Tests **isolés** : chaque test d'intégration utilise sa propre base SQLite
temporaire.

Toute correction de bug ajoute un **test de non-régression** reproduisant le
bug — écrit d'abord, vu échouer, puis rendu vert.

## 10. Où travailler

Le travail est découpé en jalons dans [`tasks-todo/`](tasks-todo/). Ouvrez le
jalon courant, lisez son objectif, son périmètre et ses critères
d'acceptation, et **restez dans le périmètre** : ce qui est marqué hors
périmètre appartient à un jalon ultérieur.

Un jalon est terminé quand **tous** ses critères sont vérifiés, la CI est
verte, les tests couvrent le comportement, et le CHANGELOG et les ADR sont à
jour. Le fichier passe alors dans [`tasks-done/`](tasks-done/).
