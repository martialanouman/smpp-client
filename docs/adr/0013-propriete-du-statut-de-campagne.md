# ADR 0013 — Le statut de campagne appartient à `messaging`

> **Statut :** Accepté
> **Date :** 2026-08-02 · **Jalon :** step-010 · **Décideur :** Martial Anouman
> **Applique :** [ADR 0010](0010-inversion-des-ports-du-chemin-d-envoi.md) (déplacement de `MessageState`) et [ADR 0012](0012-inversion-du-port-des-contacts.md) (déplacement de `Contact`)

## Contexte

Le jalon 010 demande une **machine d'états de campagne** (L-010-01) :
`CREATED → VALIDATED → RUNNING → (PAUSED ⇄ RUNNING) → COMPLETED`, plus
`CANCELLED` et `FAILED`, avec les transitions valides exprimées dans le type et
le rejet explicite des invalides. Elle est livrée dans
`crates/messaging/src/campaign/mod.rs`.

Or l'ensemble des statuts existe déjà : `CampaignStatus`, écrit au jalon 002
comme un `stored_enum!` de `persistence::records::enums`, à côté du code SQLx
qui lit la colonne `campaigns.status`. C'est exactement la situation que
l'ADR 0007 avait assumée pour `MessageState` et `Contact`, et pour la même
raison — la crate consommatrice était une coquille vide.

## Le problème n'est pas esthétique

`messaging` est **au-dessus** de `persistence` et ne dépend que de `smpp-core`
(ADR 0010). Une machine d'états écrite dans `messaging` sur un type déclaré dans
`persistence` n'est pas une chose qui compile : l'arête n'existe pas, et la
créer inverserait le sens des dépendances de CLAUDE.md §3.

Trois options.

### Option A — Écrire la machine dans `persistence`, à côté du type

**Pour :** aucun déplacement.
**Contre :** la logique de cycle de vie d'une campagne — quand une pause est
légale, pourquoi une campagne non validée ne démarre pas — atterrit dans la
crate de stockage, que le guide §8.1 veut ignorante du métier. C'est la dette
que l'ADR 0010 a soldée pour les messages ; la recréer un jalon plus tard pour
les campagnes serait un recul argumenté par rien.

### Option B — Un second enum dans `messaging`, converti à la frontière

**Pour :** aucune arête nouvelle, aucun fichier de `persistence` touché.
**Contre :** deux ensembles de statuts qui ne s'accordent que tant que
quelqu'un les tient en phase. Le jour où le jalon 012 ajoute un statut, la
conversion doit être exhaustive **et** les deux `parse` doivent l'être : un
`From` non exhaustif ne compile pas, mais un `parse` qui ignore une chaîne
compile parfaitement et perd la campagne à la relecture de la ligne. C'est le
doublon incohérent, avec une couche de cérémonie pour le rendre présentable.

### Option C — `messaging` prend le type, `persistence` le ré-exporte

C'est le geste de l'ADR 0010 pour `MessageState` et de l'ADR 0012 pour
`Contact`, appliqué une troisième fois : **la crate qui possède le cycle de vie
possède le type qui le porte.**

**Pour :** un seul ensemble de statuts, une seule table de transitions, aucun
appelant hors des deux crates à modifier — `persistence::CampaignStatus`
continue de résoudre.
**Contre :** `persistence` recompile quand la forme de l'enum change, et la
colonne `campaigns.status` perd le `MalformedRow` que la macro `stored_enum!`
produisait gratuitement.

## Décision

**Option C.**

`CampaignStatus` quitte `persistence::records::enums` pour
`messaging::campaign`. Le `parse` rend une `Option` — comme `MessageState`, et
pour la même raison : les deux appelants veulent des erreurs différentes du même
échec, le stockage nomme la colonne, la frontière IPC nomme le champ rejeté. Le
contexte de colonne est restitué là où il est connu, par
`persistence::repositories::convert::read_campaign_status`, à côté de
`read_message_state` qui fait déjà exactement cela.

Aucun cycle n'apparaît : l'arête `persistence → messaging` existe depuis le
jalon 006, et le déplacement n'en crée aucune.

## Ce qui n'est **pas** décidé ici

L'ADR 0012 a fixé le jalon 010 comme point de réexamen de
`CampaignRepository`, qui vit encore dans `persistence::ports`. Cette ADR ne
solde pas cette dette : elle déplace un type, pas un port. L'inversion du port
appartient à la sous-partie du jalon qui livrera l'alimentation et la reprise
(L-010-02, L-010-04), c'est-à-dire au premier consommateur réel du dépôt de
campagnes. La déplacer maintenant, sans consommateur, serait précisément
l'erreur que l'ADR 0007 a nommée.

## Conséquences

- **Positives :** la machine d'états est écrite une fois, dans la couche qui
  décide des transitions ; `persistence` reste ignorante du cycle de vie et se
  contente de stocker le texte qu'on lui donne.
- **Négatives / dette assumée :** `CampaignRepository` reste du mauvais côté
  jusqu'à la suite du jalon 010. Le tableau en tête de `persistence::ports` le
  porte avec son échéance.
- **Impacts opérationnels :** aucun. Le format stocké est inchangé — mêmes sept
  chaînes, mêmes valeurs en base —, donc aucune migration et aucun contrat IPC
  cassé.
- **Point de réexamen :** aucun pour ce type. Le prochain jalon qui ajoute un
  statut de campagne l'ajoute ici, et seulement ici.

## Références

- Guide §8.1 (sens des dépendances)
- CLAUDE.md §3 (frontières), §4 (typage fort)
- ADR [0007](0007-emplacement-des-traits-de-port.md), [0010](0010-inversion-des-ports-du-chemin-d-envoi.md), [0012](0012-inversion-du-port-des-contacts.md)
- `tasks-todo/step-010.md` L-010-01 · spec §10.3, §14.2
