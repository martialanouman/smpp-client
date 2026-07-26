# ADR 0001 — Adopter rusmpp comme pile SMPP, au niveau codec

<!-- Le titre disait « rusmpp/rusmppc » tant que l'ADR était en statut
     « Proposé » et que le niveau d'API restait ouvert. `rusmppc` ayant été
     écarté au jalon 003, le laisser aurait fait mentir l'index des ADR. Le nom
     de fichier, lui, ne change pas : les liens existants restent valides. -->


> **Statut :** Accepté — niveau d'API tranché au jalon 003 (voir « Arbitrage
> du niveau d'API » ci-dessous)
> **Date :** 2026-07-25, complétée le 2026-07-26 · **Jalons :** step-000 puis
> step-003 · **Décideur :** Martial Anouman

## Contexte

ShinobiSMPP doit parler SMPP **v3.4 et v5.0** en tant qu'ESME : encoder et
décoder les PDU, tenir la machine à états d'une session, gérer le fenêtrage,
les TLV, la segmentation et les accusés de réception (spec §3, §6).

Écrire ce codec à la main représenterait plusieurs semaines pour un protocole
binaire ancien, riche en cas limites et en divergences d'implémentation entre
SMSC. L'écosystème Rust offre peu de candidats matures.

Deux niveaux de décision se présentent, et ils sont indépendants :

1. **Quelle bibliothèque ?**
2. **À quel niveau d'API s'y brancher ?** — `rusmpp` expose un codec bas
   niveau (`Framed<S, CommandCodec>`), `rusmppc` un client haut niveau
   (`ConnectionBuilder`) qui prend en charge la session, les timeouts et
   `enquire_link`.

Le jalon 000 §8 autorise explicitement à ouvrir cette ADR maintenant tout en
différant le second arbitrage.

## Options envisagées

### Option A — `rusmpp` + `rusmppc`

Codec PDU complet couvrant v3.4 et v5.0, écrit en Rust, avec un client
asynchrone Tokio par-dessus.

**Pour :** couverture v5.0 réelle (`broadcast_sm`, TLV de congestion) ; typage
fort des champs protocolaires, qui rejoint le « parse, don't validate » de
CLAUDE.md §4 ; le niveau bas reste accessible si le niveau haut ne suffit pas.
**Contre :** projet à faible surface communautaire ; l'API peut évoluer ; peu
de retours d'expérience en production publiés.

### Option B — Implémentation maison du codec

**Pour :** contrôle total, aucune dépendance sur un tiers peu diffusé.
**Contre :** plusieurs semaines de travail avant le premier `submit_sm`, sur
un terrain où les erreurs sont silencieuses — un PDU malformé est accepté par
certains SMSC et rejeté par d'autres. Ce coût retarderait tous les jalons.

### Option C — Lier une bibliothèque C existante (libsmpp34)

**Pour :** implémentation éprouvée.
**Contre :** impose du `unsafe` et une FFI, que CLAUDE.md §4 interdit
(`unsafe_code = "forbid"`) ; complique le packaging multiplateforme ; ne
couvre pas v5.0.

## Décision

**Option A.** `rusmpp` pour le codec.

> Cette section a d'abord dit « et `rusmppc` comme point de départ pour la
> session ». L'arbitrage rendu au jalon 003 — plus bas — a écarté `rusmppc`.
> La phrase est corrigée plutôt que laissée telle quelle : une ADR dont la
> section « Décision » contredit sa propre conclusion envoie au lecteur
> pressé exactement la mauvaise réponse.

Le critère décisif n'est pas la maturité — aucune option n'est mature ici —
mais le **coût de l'erreur**. Si `rusmpp` déçoit, on garde le typage des PDU
et l'on remplace la couche session ; si l'on écrit tout à la main et que le
codec se révèle faux, on découvre le problème en production contre un SMSC
réel. La dépendance est isolée derrière `smpp-core`, dont c'est la seule
raison d'être : aucune autre crate n'importera `rusmpp` directement.

**Le niveau d'API restait ouvert** au jalon 000 : le trancher alors, sans
avoir écrit une ligne de session, aurait été décider sans information.
L'arbitrage a été rendu au jalon 003 — voir la section suivante, ajoutée à
ce moment-là.

## Arbitrage du niveau d'API — jalon 003

> Cette section complète une ADR qui était en statut **Proposé**. Elle ne
> réécrit aucune décision arrêtée : elle rend celle qui restait en suspens,
> ce qui est le cycle de vie normal d'une ADR proposée. Toute remise en
> cause ultérieure passera par une ADR qui supersède celle-ci.

**Décision : niveau bas.** `rusmpp` avec la feature `tokio-codec`
(`CommandCodec` + `Framed`). `rusmppc` n'est **pas** retenu.

### Le critère qui a départagé

Ce n'est ni la simplicité ni la quantité de code à écrire — sur ces deux
points le client haut niveau gagnait. C'est **qui possède la corrélation des
`sequence_number`**.

Les jalons 005 et 007 exigent trois choses que le niveau haut ne laisse pas
atteindre :

- une **fenêtre d'émission bornée** (`window_size`), tenue par un sémaphore
  libéré à la réponse **et au timeout** ;
- un **`response_timeout` par PDU en vol**, avec un `oneshot` par
  `sequence_number` ;
- la mesure du **RTT par PDU**, dont dépendent les métriques du jalon 007 et
  l'adaptation AIMD du jalon 012.

Ces trois besoins portent sur l'appariement requête/réponse. Un client haut
niveau qui gère lui-même cet appariement nous en dépossède : on ne peut plus
ni borner la fenêtre, ni instrumenter le RTT, sans le contourner. Or c'est
exactement la matière des jalons 005, 007 et 012 — soit trois des quatre
jalons les plus lourds du projet.

Le niveau bas laisse `smpp-core` sans état : il traduit des octets en
`Command` typées et l'inverse. La machine à états, le fenêtrage et la
corrélation appartiennent alors à `smpp-session`, là où l'architecture du
guide §8.1 les place.

### Ce qui a réellement été vérifié

- `encode`/`decode` round-trip sur les PDU de bind, `submit_sm`,
  `submit_sm_resp` et `deliver_sm`, sous `proptest` (256 cas par PDU).
- Le codec ne panique sur **aucune** entrée : deux propriétés dédiées le
  vérifient sur des octets arbitraires. C'est la garantie qui compte face à
  un SMSC hostile ou simplement bogué.
- `command_length` concorde toujours avec la taille réellement encodée.

Ce qui n'a **pas** été vérifié à ce stade, et ne pouvait pas l'être : le
comportement contre un SMSC réel. Le jalon 017 (simulateur avec injection de
fautes) est le premier point où cette ADR sera confrontée au réseau.

### Conséquence sur la façade

`smpp-core` réexporte `rusmpp::pdus`, `rusmpp::tlvs` et `rusmpp::types` sous
le nom `octets`, plutôt que de recopier une centaine de types. La règle « la
dépendance est isolée derrière `smpp-core` » est donc tenue au sens des
*chemins d'import* — aucune autre crate n'écrit `rusmpp::` — mais pas au sens
d'une abstraction : les types traversent la façade tels quels.

C'est assumé. Une couche d'adaptation complète coûterait des milliers de
lignes de conversion sans rien apprendre sur le protocole, et chaque
conversion serait une occasion de bug silencieux. Le prix à payer est qu'un
changement d'API de rusmpp se propagera aux crates appelantes ; le
réexport centralisé permet au moins de le constater en un seul endroit.

## Conséquences

- **Positives :** le codec et la machine à états ne sont pas à écrire ;
  `smpp-core` peut se concentrer sur les invariants métier.
- **Négatives / dette assumée :** dépendance à un projet peu diffusé. Si son
  API casse, la mise à jour est à notre charge. Le confinement à `smpp-core`
  borne ce risque, il ne le supprime pas.
- **Impacts opérationnels :** si la crate est consommée depuis GitHub plutôt
  que depuis crates.io, il faudra déclarer l'organisation dans `deny.toml`
  (`[sources] allow-git`) — et non désactiver la vérification de provenance.
- **Point de réexamen :** le niveau d'API a été tranché au jalon 003, dans la
  section « Arbitrage du niveau d'API » ci-dessus. Le prochain point de
  confrontation est le **jalon 017** : le simulateur SMSC avec injection de
  fautes est le premier endroit où cette ADR rencontrera un vrai réseau.

## Références

- Spec §6.1 (pile technique), §21 (découpage en crates)
- Guide §4.2 (frontières de dépendance)
- `tasks-todo/step-003.md`
- <https://github.com/JadKHaddad/Rusmpp>
