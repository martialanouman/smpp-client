# ADR 0010 — Inverser les deux ports du chemin d'envoi

> **Statut :** Accepté
> **Date :** 2026-07-26 · **Jalon :** step-006 · **Décideur :** Martial Anouman
> **Complète :** [ADR 0007](0007-emplacement-des-traits-de-port.md) (dont elle honore l'échéance)

## Contexte

L'ADR 0007 a placé les cinq traits de port dans `persistence`, à côté de leur
implémentation, au lieu de les définir dans la crate qui les consomme comme
l'exige le guide §8.1. Son argument était borné dans le temps : payer le coût
de l'inversion — l'arête remontante `persistence → messaging` — alors que
`messaging` était une coquille vide, c'était « imiter la forme du principe »
sans en tirer le bénéfice.

Elle s'est fixé une échéance explicite : « si à la fin du jalon 006 le
déplacement n'a toujours pas eu lieu, il faut soit le faire, soit superséder
cette ADR en assumant que l'inversion n'aura pas lieu ». La fiche du jalon 006
porte la même exigence en §4 et en §6.

Le jalon 006 donne enfin un consommateur à `MessageRepository` : l'orchestrateur
d'envoi, qui doit persister avant d'émettre (CLAUDE.md §4) et dont la
non-régression tient à un test d'ordonnancement entre le journal et la socket
(CA-006-02). Ce test exige un double du journal. Le déplacement n'est donc plus
théorique.

### Le graphe, avant

```
messaging ──► smpp-session ──► persistence ──► smpp-core
    │                              ▲
    └──────────────────────────────┘
```

## Le problème que le déplacement fait apparaître

Déplacer `MessageRepository` dans `messaging` crée l'arête
`persistence → messaging`. Combinée aux arêtes existantes, elle **ferme une
boucle** :

```
messaging → smpp-session → persistence → messaging
```

Cargo refuse un cycle entre dépendances normales. Le déplacement demandé par la
fiche est donc impossible **tel quel** : il fallait couper une arête.

## Options envisagées

### Option A — Couper `smpp-session → persistence`

`smpp-session` ne dépend de `persistence` que pour `SessionProfileRecord`, la
conversion `from_record`/`to_record`, et `SessionError::Persistence`.

**Pour :** conserve l'arête `messaging → smpp-session` du schéma de CLAUDE.md §3.
**Contre :** déplace la conversion profil↔ligne dans `persistence`, ce qui exige
soit que `persistence` renvoie un `SessionError` — une seconde crate qui parle
le type d'erreur d'une autre —, soit de déplacer aussi `BindType` et
`SessionProfileRepository`. C'est un remaniement du jalon 005 dont le seul motif
est de faire de la place, et il crée l'arête `persistence → smpp-session`, qui
est elle aussi remontante.

### Option B — Un adaptateur SQLx hébergé par `messaging`

`messaging` définit le trait *et* l'implémente pour
`persistence::SqliteMessageRepository` (règle d'orphelin : trait local, type
étranger, c'est légal).

**Pour :** aucun cycle, aucune arête nouvelle.
**Contre :** conserve `messaging → persistence`, donc l'inversion n'achète rien.
`messaging` continue de ne pas pouvoir être compilé sans SQLx, et le double de
test reste impossible sans la base. C'est la forme sans le fond — exactement ce
que l'ADR 0007 refusait de faire à l'envers.

### Option C — Inverser **les deux** ports du chemin d'envoi

`messaging` déclare `MessageRepository` **et** `SmscSession`. `persistence`
implémente le premier, `smpp-session` le second. `messaging` ne dépend plus que
de `smpp-core`.

**Pour :** le cycle disparaît parce que `messaging` cesse d'être au-dessus de
quoi que ce soit. Les deux arêtes remontantes créées sont exactement celles que
CLAUDE.md §3 décrit — « inversion de dépendance par traits (ports) définis dans
la couche haute, implémentés dans la couche basse ». L'orchestrateur se teste
avec un journal en mémoire *et* un centre de messages en mémoire.
**Contre :** l'arête `messaging → smpp-session` du schéma de CLAUDE.md §3
disparaît au profit de son inverse. Deux ports à maintenir au lieu d'un.

## Décision

**Option C.**

Le critère d'arbitrage : l'arête remontante n'est pas un coût que l'inversion
fait payer par accident, c'est **ce qu'est** l'inversion. CLAUDE.md §3 énonce
les deux règles côte à côte — « les dépendances vont du haut vers le bas » et
« inversion de dépendance par traits (ports) définis dans la couche haute » — et
la seconde est une exception nommée à la première. Une flèche du schéma qui
s'inverse quand on applique la règle que le même paragraphe prescrit n'est pas
une contradiction : c'est la règle qui s'applique.

Ce qui **ne** change **pas** : au moment de l'exécution, `messaging` pilote
toujours `smpp-session` et `persistence`. Seule la direction de la dépendance
*de compilation* s'inverse. C'est la définition du principe.

### Ce qui a bougé, exactement

| Ce qui bouge | De | Vers | Pourquoi |
|---|---|---|---|
| `MessageRepository` (écriture, lecture unitaire) | `persistence::ports` | `messaging::ports` | l'échéance de l'ADR 0007 |
| `SmscSession` | *n'existait pas* | `messaging::ports` | casser le cycle |
| `Message`, `MessageState`, `MessageStateUpdate`, `SmscMessageIdUpdate` | `persistence::records` | `messaging::message` | le vocabulaire d'un port suit le port |
| `Timestamp` | `persistence::time` | `smpp_core::time` | les deux crates l'utilisent maintenant |
| `CampaignId` | `persistence::records::ids` | `smpp_core::types` | un `Message` en porte un |
| pagination, comptage, streaming | `MessageRepository` | `persistence::ports::MessageJournal` | leur consommateur est le jalon 013 |

`persistence` ré-exporte tout ce qui a bougé, donc aucun site d'appel hors des
deux crates n'a changé, et le **type** d'un `Message` est resté le même.

### Le graphe, après

```
                 ┌──────────► messaging ──► smpp-core
                 │             ▲    ▲
        smpp-session ──────────┘    │
                 │                  │
                 └──► persistence ──┘
```

Acyclique. `messaging` ne dépend que de `smpp-core`.

## Le type d'erreur des ports

Un port ne peut pas renvoyer l'erreur de son implémenteur : `PersistenceError`
porte une `sqlx::Error` et, sur une variante, un chemin de fichier. Le faire
traverser mettrait SQLx dans la signature d'une crate qui ne doit pas savoir
qu'une base existe, et un chemin dans une erreur que `messaging` remonte vers
l'IPC (CA-001-06).

Les ports nomment donc les issues sur lesquelles un appelant peut **agir** —
`MessageStoreError` en a trois, `SubmitError` en a cinq — et l'implémenteur
projette. **Le coût est réel et assumé :** la chaîne de `source` est perdue à
la frontière. L'implémenteur journalise donc l'échec complet, avec sa chaîne,
*avant* de convertir ; `persistence::repositories::messages::store_error` le
fait, et un test vérifie que l'erreur du port ne porte ni identifiant, ni
numéro, ni corps de message.

## Conséquences

- **Positives :** `messaging` se teste sans base et sans socket ; les vingt et
  un tests de `crates/messaging/tests/sending.rs` tournent en dix
  millisecondes. Le double du centre de messages du jalon 005 est partagé au
  lieu d'être recopié. La dette de l'ADR 0007 sur `MessageRepository` est
  soldée.
- **Négatives / dette assumée :** trois ports restent du mauvais côté —
  `ContactRepository` (échéance : jalon 009, CA-009-13), `CampaignRepository`
  (jalon 010), `MessageJournal` et `PduLogRepository` (jalon 013). Le tableau
  en tête de `persistence::ports` les porte avec leur échéance, ce que l'ADR
  0007 ne faisait que pour deux d'entre eux.
- **Négatives :** `smpp-session` et `persistence` dépendent toutes deux de
  `messaging` pour un seul trait chacune. Un changement de forme de l'un de ces
  traits recompile les deux.
- **Impacts opérationnels :** la feature `test-support` de `smpp-session` crée
  un cycle de **dev**-dépendances `messaging (dev) → smpp-session → messaging`.
  Cargo l'autorise — une dev-dépendance ne peut pas affecter la bibliothèque
  qu'elle teste — et `cargo build --workspace` reste correct. La feature est
  hors par défaut, donc rien du double n'atteint le binaire.
- **Point de réexamen :** jalon 009 pour `ContactRepository`, jalon 010 pour
  `CampaignRepository`, jalon 013 pour les deux derniers. Chacun doit vérifier
  qu'il ne referme pas un cycle, comme celui-ci a dû le faire.

## Références

- Guide §8.1 (sens des dépendances), §8.2 (injection de dépendances)
- CLAUDE.md §3 (frontières et inversion par traits)
- ADR [0007](0007-emplacement-des-traits-de-port.md) — dont ceci est l'échéance
- `tasks-todo/step-006.md` §4 et §6
