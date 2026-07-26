# ADR 0006 — Calculer la version minimale de Rust depuis le graphe, et la vérifier

> **Statut :** Accepté — écarte la version plancher de la spec §6.3
> **Date :** 2026-07-26 · **Jalon :** step-003 · **Décideur :** Martial Anouman

## Contexte

La spec §6.3 et le guide §3.1 fixent **Rust ≥ 1.78, édition 2021**. Le
workspace a été créé au jalon 000 avec `rust-version = "1.78"`.

Le jalon 003 intègre `rusmpp` ([ADR 0001](0001-choix-de-la-pile-smpp.md)), qui
déclare `edition = "2024"` et `rust-version = "1.85.0"`. La question semblait
donc être : « faut-il relever 1.78 à 1.85 ? »

**C'était la mauvaise question**, et la première rédaction de cette ADR y a
répondu — en concluant 1.85. Deux erreurs successives ont été trouvées en
revue :

1. `rusmpp 0.4.0` dépend de `rusmpp-core 0.4.0`, qui exige **1.86**. Vérifier
   le paquet de façade ne suffit pas.
2. En recalculant depuis le graphe **complet**, le maximum est **1.88** —
   imposé par `darling`, `plist`, `serde_with` et `time`, tous tirés par
   Tauri.

Le troisième point est le plus instructif : ces quatre paquets étaient déjà
présents **au jalon 000**. La valeur `1.78` n'a donc jamais été vraie. Elle est
restée fausse pendant tout un jalon parce que **rien ne la vérifiait** : la CI
utilise `dtolnay/rust-toolchain@stable` partout, et une toolchain récente
compile évidemment un projet qui prétend n'exiger que 1.78.

## Décision

**Deux décisions, indissociables.**

### 1. La MSRV se calcule depuis le graphe complet

`rust-version = "1.88"` dans `[workspace.package]`, soit le maximum des
`rust-version` déclarées par **toutes** les dépendances, directes et
transitives :

```bash
cargo metadata --format-version 1 \
  | jq -r '.packages[] | select(.rust_version) | .rust_version' \
  | sort -V | tail -1
```

La commande est inscrite en commentaire dans `Cargo.toml`, à côté de la
valeur : c'est le seul endroit où quelqu'un qui la modifie la lira.

### 2. La MSRV est vérifiée en CI, sinon elle ne veut rien dire

Un job `msrv` installe une toolchain épinglée à la version déclarée et lance
`cargo check --workspace --all-targets`. Sans lui, la valeur est une intention
que personne ne teste — ce que le jalon 000 a démontré en la laissant fausse.

Ce job est délibérément **hors matrice** : la MSRV est une propriété du graphe
de dépendances, identique sur les trois systèmes.

## Options envisagées

### Option A — Rester en 1.78 et écrire le codec à la main

**Pour :** respecte la lettre de la spec.
**Contre :** ne résout rien. La chaîne Tauri exigeait déjà 1.88 sans rusmpp :
le plancher serait resté faux, et il faudrait en plus écrire le codec à la
main — plusieurs semaines, sur un protocole binaire où les erreurs sont
silencieuses, ce que l'ADR 0001 avait écarté comme l'option la plus coûteuse.

### Option B — Rester bas avec `rusmpp 0.1.x`

**Contre :** même objection sur Tauri, et les 0.1.x précèdent la couverture
SMPP v5.0 dont dépend entièrement le jalon 012.

### Option C — Déclarer le maximum réel du graphe et le vérifier

**Pour :** la valeur devient exacte et le reste, parce qu'une régression est
détectée au prochain run.
**Contre :** écarte une valeur écrite dans la spec, et interdit les toolchains
antérieures à juin 2025.

## Conséquences

- **Positives :** `rust-version` cesse d'être décoratif. Une dépendance future
  qui relève la barre est signalée par la CI plutôt que découverte par un
  contributeur dont la toolchain ne compile plus.
- **Négatives / dette assumée :** un écart de plus avec la spec, qui s'ajoute
  à ceux de l'[ADR 0005](0005-versions-de-la-chaine-frontend.md). La spec
  décrit une intention datée du démarrage ; elle n'est pas modifiée, et ce
  fichier est la seule trace qui explique pourquoi elle est écartée. Si les
  écarts continuent de s'accumuler, ce sera le signal qu'il faut réviser la
  spec plutôt que multiplier les ADR correctives.
- **Impacts opérationnels :** l'édition du workspace **reste 2021** — seule
  `rusmpp` est en édition 2024, et une crate 2021 peut en dépendre sans
  conséquence, l'édition étant une propriété par crate. Migrer notre code en
  édition 2024 serait une décision distincte, non prise ici.
  `rust-toolchain.toml` reste sur `stable`, qui satisfait la contrainte. Un
  poste figé sous 1.88 ne compilera plus : `rustup update` suffit.
- **Ce que cette ADR ne garantit pas :** que le code compile *effectivement*
  sous 1.88. Elle garantit que la valeur déclarée est cohérente avec ce que
  les dépendances déclarent, et que le job CI le vérifie à chaque run. Si une
  dépendance sous-déclare sa propre MSRV, le job échouera — et c'est
  exactement le signal attendu.
- **Point de réexamen :** aucun sur le principe. Une MSRV ne redescend pas.
  La valeur, elle, bougera au gré des dépendances ; la commande ci-dessus est
  la seule façon correcte de la recalculer.

## Références

- Spec §6.1 (rusmpp cité), §6.3 (plancher 1.78) · Guide §3.1, §20.2
- [ADR 0001](0001-choix-de-la-pile-smpp.md) · [ADR 0005](0005-versions-de-la-chaine-frontend.md)
- `Cargo.toml` (`[workspace.package] rust-version`), `.github/workflows/ci.yml` (job `msrv`)
