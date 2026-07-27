# ADR 0011 — Virtualisation de la table des journaux

> **Statut :** Acceptée · **Date :** 2026-07-27 · **Jalon :** 008

## Contexte

CA-008-07 demande que l'écran Journaux affiche 200 000 lignes « sans dégradation
perceptible : défilement fluide, filtre appliqué en moins d'une seconde
(virtualisation + pagination backend) ». La moitié backend est acquise —
pagination par curseur et filtres mesurés dans `crates/persistence`.

La moitié frontend impose de ne rendre qu'une fenêtre de lignes. CLAUDE.md §2
liste **TanStack Table/Virtual** dans la pile, et step-008 §2 nomme
explicitement TanStack Virtual.

`@tanstack/react-virtual` 3.14 a donc été installé et intégré en premier. Deux
faits ont suivi, tous deux reproductibles :

1. **Le compilateur React refuse de compiler le composant.** Le plugin
   `react-hooks/incompatible-library` signale que `useVirtualizer()` « returns
   functions which cannot be memoized without leading to stale UI » et
   **saute la mémoïsation du composant entier**. L'écran qui a le plus besoin
   d'être mémoïsé est précisément celui qui la perd.

2. **Le virtualiseur ne se réaffiche jamais après le montage dans
   l'environnement de test du projet** (Vitest + jsdom + React 19). Vérifié sur
   un composant sonde minimal : `getVirtualItems()` renvoie une liste vide et
   `scrollRect` reste `{width: 0, height: 0}` sur tous les rendus, y compris
   après `waitFor`. Stubber `ResizeObserver`, stubber `getBoundingClientRect` et
   fournir `initialRect` ne changent rien — aucun second rendu n'a lieu.

La conséquence du point 2 est la seule qui décide : **le critère ne pouvait pas
être testé**. Le tableau rendait zéro ligne sous test, donc aucune assertion sur
la virtualisation n'était possible, et rien n'aurait distingué « la fenêtre est
correcte » de « le composant est cassé » avant un lancement manuel.

## Options

1. **Garder TanStack Virtual et ne pas tester la virtualisation.** Le critère
   serait déclaré tenu sur la foi d'un lancement manuel. C'est exactement la
   situation que l'avertissement du jalon décrit : un test qui passe pour la
   mauvaise raison, ou pas de test du tout.

2. **Garder TanStack Virtual et tester dans un vrai navigateur** (Playwright,
   `tauri-driver`). Le socle E2E n'existe pas encore — le guide le place au
   jalon 017 — et l'introduire ici pour un seul écran est un jalon en soi.

3. **Écrire la fenêtre à la main.** Le comportement utile est une division et
   une tranche à hauteur de ligne fixe : `floor(scrollTop / hauteur)`,
   `ceil(viewport / hauteur)`, plus un overscan. Une trentaine de lignes, dont
   l'essentiel est une **fonction pure**.

## Décision

**Option 3.** `ui/src/views/Logs/rowWindow.ts` contient :

- `rowWindow(count, rowHeight, scrollTop, viewportHeight, overscan)` — pure,
  totale (toutes les entrées sont bornées), testée sur les nombres ;
- `useRowWindow(count, rowHeight)` — le branchement React : un `ref`, un
  écouteur `scroll`, un écouteur `resize`, et rien d'autre.

`@tanstack/react-virtual` est retiré des dépendances.

## Conséquences

**Ce qu'on gagne.**

- La propriété que CA-008-07 énonce — « ce qui est rendu ne grandit pas avec le
  nombre de lignes » — est un test unitaire sur des entiers, vrai pour 100,
  200 000 et 2 000 000 de lignes, indépendant de tout DOM.
- Les cas dégénérés réels sont couverts et bornés : viewport nul (premier
  rendu), offset négatif (défilement élastique macOS), liste vide (filtre sans
  résultat). Aucun ne peut produire `start > end`, qu'`Array.slice` transformerait
  silencieusement en table vide.
- Le compilateur React compile de nouveau l'écran.
- Une dépendance de moins (CLAUDE.md §2 : toute dépendance doit se justifier —
  celle-ci ne se justifiait plus).

**Ce qu'on perd, et c'est réel.**

- Les hauteurs de ligne **variables**. TanStack les mesure ; cette fenêtre ne le
  fait pas. Les cellules du journal sont mono-ligne par construction, donc la
  hauteur est fixe — mais un écran futur qui voudrait des lignes extensibles
  devra soit mesurer, soit revenir à une bibliothèque.
- Le défilement **horizontal** virtualisé, la restauration de position et le
  `scrollToIndex`. Aucun n'est demandé par ce jalon.
- Trente lignes à maintenir, qui étaient auparavant maintenues par quelqu'un
  d'autre.

**Écart assumé.** CLAUDE.md §2 et step-008 §2 nomment TanStack Virtual ; cette
ADR s'en écarte pour l'écran Journaux. L'écart est signalé plutôt que tranché en
silence (CLAUDE.md §1) : si le compilateur React et TanStack se réconcilient, ou
si un socle E2E arrive au jalon 017, revenir à la bibliothèque est un
remplacement d'un seul fichier — `rowWindow.ts` n'est utilisé que par
`LogsView.tsx`, et l'interface qu'il expose est celle d'un virtualiseur.

Cette ADR n'autorise pas à retirer TanStack Table, qui n'a pas été évalué.
