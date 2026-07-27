# ADR 0012 — Inverser le port des contacts, et laisser la pagination derrière

> **Statut :** Accepté
> **Date :** 2026-07-27 · **Jalon :** step-009 · **Décideur :** Martial Anouman
> **Complète :** [ADR 0007](0007-emplacement-des-traits-de-port.md) (dont elle honore la seconde échéance) et [ADR 0010](0010-inversion-des-ports-du-chemin-d-envoi.md) (dont elle applique la méthode)

## Contexte

L'ADR 0007 a placé cinq traits de port dans `persistence`, à côté de leur
implémentation, au lieu de les définir dans la crate qui les consomme comme
l'exige le guide §8.1. Son argument était borné dans le temps : payer le coût
de l'inversion — l'arête remontante — alors que la crate consommatrice était une
coquille vide, c'était « imiter la forme du principe » sans en tirer le
bénéfice.

Sa rectification du 2026-07-26 a fixé deux échéances : **step-006** pour
`MessageRepository`, honorée par l'ADR 0010, et **step-009** pour
`ContactRepository`. Cette seconde échéance est portée par **CA-009-13**, qui
précise ce qu'on en attend : « le déplacement retire l'arête provisoire ; rien
d'autre ne change ».

Le jalon 009 donne enfin un consommateur à ce port : l'importeur. Ce qu'il doit
prouver — qu'une annulation ne laisse aucun lot à moitié écrit (CA-009-10), que
le rapport est exact (CA-009-08), que chaque rejet porte sa ligne et son motif
(CA-009-05) — se teste ligne par ligne, sur des dizaines de milliers de lignes,
et devrait sans cela passer par un fichier SQLite réel à chaque assertion.

### Le graphe, avant

```
                 ┌──────────► messaging ──► smpp-core
                 │             ▲    ▲
        smpp-session ──────────┘    │
                 │                  │
                 └──► persistence ──┘

        contacts ──► (rien)          logging-export ──► persistence
```

`contacts` ne dépendait de rien et personne ne dépendait de `contacts`.

## Le cycle que le jalon 006 avait dû casser, et qui ne se reforme pas ici

L'ADR 0010 a buté sur une boucle : déplacer `MessageRepository` dans
`messaging` fermait `messaging → smpp-session → persistence → messaging`, et il
a fallu inverser **deux** ports pour la rompre. La même vérification s'imposait
ici avant d'écrire une ligne.

Elle est négative. L'arête créée est `persistence → contacts`, et `contacts` ne
dépend que de `smpp-core`, qui ne dépend d'aucune crate interne. Aucun chemin ne
revient donc vers `persistence`. Vérifié sur les deux *kinds* — `normal` et
`dev` —, parce que CLAUDE.md §3 dit « aucun cycle » sans les distinguer :

```bash
cargo metadata --format-version 1 --no-deps   # les arêtes déclarées
```

C'est aussi la raison pour laquelle `contacts` n'a **pas** de dev-dépendance
vers `persistence` : elle formerait `contacts (dev) → persistence → contacts`,
un cycle que Cargo tolère et que CLAUDE.md §3 ne distingue pas. Les tests
d'intégration de l'import tournent donc contre un double en mémoire, et les
tests SQLx du même port vivent dans `crates/persistence/tests/`, exactement
comme les tests de bout en bout de l'envoi vivent dans `smpp-session`.

## Ce qui a bougé, exactement

| Ce qui bouge | De | Vers | Pourquoi |
|---|---|---|---|
| `ContactRepository` | `persistence::ports` | `contacts::ports` | l'échéance de l'ADR 0007, CA-009-13 |
| `Contact`, `ContactList` | `persistence::records` | `contacts::model` | le vocabulaire d'un port suit le port |
| `ContactId`, `ListId` | `persistence::records::ids` | `contacts::model` | idem ; `ProfileId` naît là |
| `line_type: Option<String>` | — | `Option<LineType>` | CLAUDE.md §4 ; CA-009-06 compare un type, pas une chaîne |
| `page_contacts` | `ContactRepository` | `persistence::ports::ContactDirectory` | son consommateur est l'écran, pas l'import |

`persistence` ré-exporte tout ce qui a bougé, donc `persistence::Contact`
résout toujours et aucun site d'appel hors des deux crates n'a changé.

### Le graphe, après

```
                 ┌──────────► messaging ──► smpp-core
                 │             ▲    ▲            ▲
        smpp-session ──────────┘    │            │
                 │                  │            │
                 └──► persistence ──┴──► contacts┘
```

Acyclique. `contacts` ne dépend que de `smpp-core`.

## La décision qui n'allait pas de soi : la pagination reste en bas

`ContactRepository` portait un `page_contacts(cursor, limit)`. Le déplacer avec
le reste demandait que `contacts` connaisse `Cursor` et `Page`, qui vivent dans
`persistence`. Trois options.

### Option A — Descendre `Cursor` et `Page` dans `smpp-core`

C'est ce que le jalon 006 a fait de `Timestamp` (ADR 0010) : deux crates de
couches différentes en avaient besoin, la seule crate sous les deux était
`smpp-core`.

**Pour :** le port part entier ; une seule définition de la pagination.
**Contre :** `Timestamp` est un format de données que le protocole manipule
réellement. Un curseur de pagination est un `rowid` SQLite. Le mettre dans la
crate du codec PDU — dont `records::ids` disait encore « la crate qui doit
rester libre de tout ce que le format de fil ignore » — c'est déplacer une dette
plutôt que la solder.

### Option B — Une pagination propre à `contacts`

**Pour :** aucune arête nouvelle.
**Contre :** deux types `Page` et deux types `Cursor` dans le workspace, dont
l'un se convertirait dans l'autre à chaque appel. La duplication a un coût
certain pour un bénéfice nul : personne dans `contacts` ne lit une page.

### Option C — Laisser la pagination dans `persistence`, sous son propre trait

`ContactRepository` part avec l'écriture, la lecture unitaire, le streaming et
l'algèbre de listes. `page_contacts` reste, sous
`persistence::ports::ContactDirectory`, et `src-tauri` le consomme vers le bas.

**Pour :** c'est exactement la coupure que l'ADR 0010 a faite entre
`messaging::ports::MessageRepository` et `persistence::ports::MessageJournal`, et
pour la même raison — *un port appartient à la couche qui le consomme*. Le
consommateur de la pagination des contacts est l'écran Contacts, dans
`src-tauri` ; rien dans l'import ni dans l'algèbre de listes ne lit une page.
Le précédent est déjà appliqué au jalon 008 : `commands/logs.rs` importe
`persistence::ports::PduLogRepository` directement.
**Contre :** `ContactRepository` n'est plus « tout ce qui touche aux contacts »,
et il faut savoir que la pagination est ailleurs.

## Décision

**Option C.**

Le critère d'arbitrage est celui de l'ADR 0010, appliqué deux fois dans la même
opération : un port appartient à la couche qui le consomme. Il fait monter
`ContactRepository`, parce que l'importeur le consomme ; il fait rester
`page_contacts`, parce que l'importeur ne le consomme pas. Répondre « le port
doit partir entier » ferait de la localisation d'un trait une question
d'esthétique plutôt que de dépendance.

CA-009-13 est donc satisfait à la lettre — `ContactRepository` est défini côté
`contacts`, implémenté côté `persistence`, et l'arête provisoire a disparu — et
l'écart restant est nommé ici plutôt que dissimulé dans une signature.

## Le type d'erreur du port

Même règle que l'ADR 0010, mêmes conséquences. `PersistenceError` porte une
`sqlx::Error` et, sur une variante, un chemin de fichier ; le faire traverser
mettrait SQLx dans la signature d'une crate qui ne doit pas savoir qu'une base
existe, et un chemin dans une erreur remontée vers l'IPC (CA-001-06).

`ContactStoreError` nomme donc les trois issues sur lesquelles un appelant peut
agir — `Conflict`, `NotFound`, `Unavailable { reason }` — et l'implémenteur
projette. **Le coût est réel et assumé :** la chaîne de `source` est perdue à la
frontière, donc `persistence::repositories::contacts::store_error` journalise
l'échec complet *avant* de convertir, et un test vérifie que l'erreur du port ne
porte ni numéro, ni attribut, ni identifiant.

Une conséquence secondaire mérite d'être dite : le test
`a_conflict_names_the_row_by_identifier_only` observait cette propriété à
travers `insert_contact`. Plus aucun port public ne rend un
`PersistenceError::Conflict` — les autres écritures sont des upserts ou des
auto-incréments. La *classification* (une violation d'unicité SQLite devient un
`Conflict` et non un `Database`) reste prouvée, puisque la variante du port ne
s'atteint que par elle ; le *rendu* est désormais vérifié sur une erreur
construite, ce que le test dit en toutes lettres.

## Conséquences

- **Positives :** la dette de l'ADR 0007 sur `ContactRepository` est soldée.
  `contacts` se teste sans base : l'import complet, son annulation et
  l'exactitude de son rapport tournent contre un double en mémoire. `LineType`
  devient un enum, donc « mobiles uniquement » compare un type au lieu d'une
  chaîne.
- **Négatives / dette assumée :** trois ports restent du mauvais côté —
  `CampaignRepository` (jalon 010), `MessageJournal` et `PduLogRepository`
  (jalon 013) — plus `ContactDirectory`, qui n'est pas une dette mais un choix
  argumenté ci-dessus. Le tableau en tête de `persistence::ports` les porte avec
  leur échéance.
- **Négatives :** `persistence` dépend maintenant de deux crates au-dessus
  d'elle. Un changement de forme de l'un de ces deux traits la recompile.
- **Impacts opérationnels :** une migration s'ajoute
  (`20260727150000_import_profiles`), et `contacts` amène `phonenumber`,
  `calamine`, `csv`, `encoding_rs` et `serde` dans le graphe — donc de nouvelles
  licences dans `deny.toml`. Aucun impact sur la sécurité ni sur le packaging.
- **Point de réexamen :** jalon 010 pour `CampaignRepository`, jalon 013 pour les
  deux derniers. Chacun doit vérifier qu'il ne referme pas un cycle, sur les
  deux kinds, comme celui-ci a dû le faire.

## Références

- Guide §8.1 (sens des dépendances), §8.2 (injection de dépendances)
- CLAUDE.md §3 (frontières et inversion par traits), §4 (typage fort)
- ADR [0007](0007-emplacement-des-traits-de-port.md) — dont ceci est la seconde échéance
- ADR [0010](0010-inversion-des-ports-du-chemin-d-envoi.md) — dont ceci reprend le critère
- `tasks-todo/step-009.md` CA-009-13
