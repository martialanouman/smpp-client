# ADR 0002 — Persister avec SQLite via SQLx

> **Statut :** Accepté
> **Date :** 2026-07-25 · **Jalon :** step-000 · **Décideur :** Martial Anouman

## Contexte

L'application doit persister messages, campagnes, contacts, sessions et
journaux, sur une base **embarquée** — pas de serveur à installer, l'outil est
une application de bureau (spec §6.3, §20.5).

SQLite s'impose de fait : embarqué, éprouvé, présent partout, mode WAL adapté
à un lecteur concurrent d'un écrivain. La question réelle est le **pilote**.

Contrainte structurante : CLAUDE.md §4 impose un invariant *write-ahead* — un
message est persisté **avant** émission, et ses transitions d'état sont
idempotentes. Sur une campagne à plusieurs milliers de messages, l'écriture se
trouve donc sur le chemin chaud, à côté d'un runtime Tokio où **rien ne doit
bloquer**.

## Options envisagées

### Option A — `rusqlite`

Liaison directe et synchrone à la bibliothèque C.

**Pour :** mature, largement utilisée, surcoût minimal.
**Contre :** API **bloquante**. Chaque requête sur le chemin chaud doit passer
par `spawn_blocking`, sous peine de bloquer un thread du runtime — exactement
ce que CLAUDE.md §4 interdit. Cette discipline repose alors entièrement sur la
revue : rien ne l'impose mécaniquement. Aucune vérification des requêtes à la
compilation.

### Option B — `sqlx`

Pilote asynchrone pur Rust, avec vérification des requêtes à la compilation
(`query!`).

**Pour :** asynchrone de bout en bout, donc pas de `spawn_blocking` à
discipliner ; les macros `query!` valident **au moment de la compilation** que
la requête est correcte et que les types de colonnes correspondent — une
erreur de schéma devient une erreur de build ; pool de connexions intégré ;
migrations versionnées par `sqlx migrate`.
**Contre :** dépendance plus lourde ; les macros vérifiées exigent une base
accessible à la compilation (`DATABASE_URL`) ou un cache `.sqlx/` commité, ce
qui ajoute une contrainte de CI.

### Option C — `sea-orm` ou `diesel`

**Pour :** abstraction plus riche, entités typées.
**Contre :** un ORM impose son modèle de requêtes là où le nôtre est simple et
majoritairement en écriture. Le surcoût conceptuel n'achète rien ici.

## Décision

**Option B — SQLx**, SQLite en mode **WAL**.

Le critère décisif est la **vérification à la compilation**. Le schéma va
évoluer sur dix-sept jalons ; avec `rusqlite`, une colonne renommée se
manifeste par une erreur à l'exécution, potentiellement au milieu d'une
campagne, sur le poste de l'utilisateur. Avec `sqlx::query!`, elle ne compile
pas.

L'asynchronisme natif vient en second, mais compte : il retire une classe
entière d'erreurs — l'appel bloquant oublié dans une tâche Tokio — plutôt que
d'en confier la détection à la revue.

## Conséquences

- **Positives :** requêtes vérifiées à la compilation ; pas de
  `spawn_blocking` à disséminer ; migrations versionnées et rejouables ;
  l'étape 8 de la CI devient réellement vérifiante dès le jalon 002.
- **Négatives / dette assumée :** la CI doit disposer soit d'une base
  temporaire, soit du cache `.sqlx/` commité. Le second est préférable — il
  rend la compilation reproductible hors ligne — mais impose de régénérer le
  cache à chaque changement de requête, avec le risque d'oubli que cela
  comporte. À traiter au jalon 002.
- **Impacts opérationnels :** `DATABASE_URL` documentée dans `.env.example`
  dès maintenant ; `sqlx-cli` requis en CI à partir du jalon 002 ;
  `scripts/check-migrations.sh` bascule automatiquement à ce moment-là.
- **Point de réexamen :** si le surcoût de SQLx devenait mesurable sur le
  chemin chaud d'une campagne à haut débit (jalon 010), comparer contre
  `rusqlite` + `spawn_blocking` **avec des mesures**, pas par principe.

## Références

- Spec §6.1, §6.3, §7 (modèle de données)
- Guide §5.4 (interdiction du blocage en async), §15.1 (étape 8)
- CLAUDE.md §4 (persistance write-ahead)
- `tasks-todo/step-002.md`
