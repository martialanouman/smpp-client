# ADR 0006 — Relever la version minimale de Rust à 1.85

> **Statut :** Accepté — écarte la version plancher de la spec §6.3
> **Date :** 2026-07-26 · **Jalon :** step-003 · **Décideur :** Martial Anouman

## Contexte

La spec §6.3 et le guide §3.1 fixent tous deux **Rust ≥ 1.78, édition 2021**.
Le workspace a été créé au jalon 000 avec `rust-version = "1.78"`.

Le jalon 003 intègre `rusmpp`, retenu par l'[ADR 0001](0001-choix-de-la-pile-smpp.md).
Or `rusmpp 0.4.0` déclare :

```toml
edition = "2024"
rust-version = "1.85.0"
```

La contrainte n'est donc pas un confort mais un fait : le paquet ne compile
pas sous 1.78.

## Ce qui a été vérifié avant de trancher

La question posée n'était pas « faut-il moderniser ? » mais « la contrainte
est-elle contournable ? ». Réponse obtenue en interrogeant crates.io sur
**toutes** les versions publiées de rusmpp :

| Version | `rust-version` déclarée |
|---|---|
| 0.4.0 | 1.85.0 |
| 0.3.0-alpha.0 et .1 | 1.85.0 |
| 0.2.0 → 0.2.3 | 1.85.0 |
| 0.2.0-alpha.1 | non déclarée |
| 0.1.0 → 0.1.3 | non déclarée |

Aucune version depuis la 0.2.0 ne descend sous 1.85. Seules les 0.1.x ne
déclarent rien — ce sont des versions antérieures à la couverture v5.0, dont
le jalon 012 dépend entièrement.

Rester en 1.78 signifierait donc renoncer à rusmpp, c'est-à-dire revenir sur
l'ADR 0001 et écrire le codec à la main : plusieurs semaines sur un protocole
binaire où les erreurs sont silencieuses, ce que l'ADR 0001 avait justement
écarté comme l'option la plus coûteuse.

## Options envisagées

### Option A — Rester en 1.78 et écrire le codec à la main

**Pour :** respecte la lettre de la spec.
**Contre :** supersède de fait l'ADR 0001 et rouvre un arbitrage tranché sur
des critères qui, eux, n'ont pas changé. Le coût réel se paierait sur les
jalons 003, 004 et 012.

### Option B — Rester en 1.78 avec rusmpp 0.1.x

**Pour :** conserve la bibliothèque.
**Contre :** les 0.1.x précèdent la couverture SMPP v5.0. Le jalon 012 —
`broadcast_sm`, TLV `congestion_state`, opérations v5.0 — deviendrait
irréalisable sans forker la bibliothèque.

### Option C — Relever la MSRV à 1.85

**Pour :** lève la contrainte à sa racine, sans toucher à aucune décision
antérieure.
**Contre :** écarte une valeur écrite dans la spec, et interdit les toolchains
antérieures à février 2025.

## Décision

**Option C.** `rust-version = "1.85"` dans `[workspace.package]`.

L'édition du workspace **reste 2021**. Seule `rusmpp` est en édition 2024, ce
qui est sans conséquence : une crate en édition 2021 peut dépendre d'une crate
en édition 2024, les éditions étant une propriété par crate et non par graphe.
Migrer notre propre code en édition 2024 serait une décision distincte, non
prise ici.

Le raisonnement tient en une phrase : la spec fixait un plancher pour garantir
la portabilité, pas pour interdire une bibliothèque qu'elle recommande
elle-même par ailleurs (§6.1 cite nommément rusmpp). Les deux exigences sont
incompatibles en l'état ; celle qui cède est celle dont le coût est le plus
faible.

## Conséquences

- **Positives :** rusmpp compile ; les éditions récentes sont disponibles pour
  les dépendances à venir (`sqlx`, `tokio-rustls`) sans réexamen.
- **Négatives / dette assumée :** un écart de plus avec la spec, qui s'ajoute à
  ceux de l'[ADR 0005](0005-versions-de-la-chaine-frontend.md). La spec décrit
  une intention datée du démarrage du projet ; elle n'est pas modifiée, et ce
  fichier est la seule trace qui explique pourquoi elle est écartée. Si les
  écarts continuent de s'accumuler, ce sera le signal qu'il faut réviser la
  spec plutôt que multiplier les ADR correctives.
- **Impacts opérationnels :** `rust-toolchain.toml` reste sur `stable`, qui
  satisfait la contrainte ; la CI utilise `dtolnay/rust-toolchain@stable`,
  également satisfaisant. Aucune modification de pipeline n'est nécessaire.
  Un poste de développement figé sur une toolchain antérieure à 1.85 ne
  compilera plus le projet — `rustup update` suffit à le corriger.
- **Point de réexamen :** aucun. Une MSRV ne redescend pas ; elle ne sera
  relevée à nouveau que si une dépendance future l'impose, auquel cas la même
  vérification — la contrainte est-elle contournable ? — devra être refaite.

## Références

- Spec §6.1 (rusmpp cité), §6.3 (plancher 1.78) · Guide §3.1, §20.2
- [ADR 0001](0001-choix-de-la-pile-smpp.md) · [ADR 0005](0005-versions-de-la-chaine-frontend.md)
- `Cargo.toml` (`[workspace.package] rust-version`)
