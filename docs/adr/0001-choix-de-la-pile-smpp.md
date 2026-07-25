# ADR 0001 — Adopter rusmpp/rusmppc comme pile SMPP

> **Statut :** Proposé — le niveau d'API reste à trancher au jalon 003
> **Date :** 2026-07-25 · **Jalon :** step-000 · **Décideur :** Martial Anouman

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

**Option A.** `rusmpp` pour le codec, `rusmppc` comme point de départ pour la
session.

Le critère décisif n'est pas la maturité — aucune option n'est mature ici —
mais le **coût de l'erreur**. Si `rusmpp` déçoit, on garde le typage des PDU
et l'on remplace la couche session ; si l'on écrit tout à la main et que le
codec se révèle faux, on découvre le problème en production contre un SMSC
réel. La dépendance est isolée derrière `smpp-core`, dont c'est la seule
raison d'être : aucune autre crate n'importera `rusmpp` directement.

**Le niveau d'API reste ouvert.** Le trancher aujourd'hui, sans avoir écrit
une ligne de session, serait une décision prise sans information. L'arbitrage
est renvoyé au **jalon 003**, sur la base d'un prototype de bind réel.

## Conséquences

- **Positives :** le codec et la machine à états ne sont pas à écrire ;
  `smpp-core` peut se concentrer sur les invariants métier.
- **Négatives / dette assumée :** dépendance à un projet peu diffusé. Si son
  API casse, la mise à jour est à notre charge. Le confinement à `smpp-core`
  borne ce risque, il ne le supprime pas.
- **Impacts opérationnels :** si la crate est consommée depuis GitHub plutôt
  que depuis crates.io, il faudra déclarer l'organisation dans `deny.toml`
  (`[sources] allow-git`) — et non désactiver la vérification de provenance.
- **Point de réexamen :** au jalon 003, à la lumière d'un bind réel contre un
  simulateur. Une ADR 0006 tranchera le niveau d'API et pourra, le cas
  échéant, superséder celle-ci.

## Références

- Spec §6.1 (pile technique), §21 (découpage en crates)
- Guide §4.2 (frontières de dépendance)
- `tasks-todo/step-003.md`
- <https://github.com/JadKHaddad/Rusmpp>
