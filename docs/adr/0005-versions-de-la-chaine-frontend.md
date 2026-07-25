# ADR 0005 — Fixer les versions de la chaîne frontend (Node 24, React 19, ESLint)

> **Statut :** Accepté — écarte trois versions citées par la spec §6.2 et §6.3
> **Date :** 2026-07-25 · **Jalon :** step-000 · **Décideur :** Martial Anouman

## Contexte

La spec a été rédigée avant l'installation de la chaîne d'outils. Trois de ses
mentions ne correspondent plus à ce que produit l'outillage courant :

| Sujet | Spec | Outillage courant (juillet 2026) |
|---|---|---|
| Node | « ≥ 20 » (§6.3) | poste de développement en 24.18 |
| React | « React 18 » (§6.2) | `create-vite` génère React 19 |
| Lint | `eslint` (guide §15.1) | `create-vite` génère `oxlint` |

Le guide §20.2 impose une ADR pour toute décision structurante ; diverger de
la spec en est une. Cette ADR consigne les trois arbitrages d'un coup, parce
qu'ils relèvent de la même question : suivre l'outillage ou suivre la lettre
du document ?

Le jalon 000 ne demandait que les ADR 0001 à 0004 ; celle-ci s'y ajoute.
CA-000-14 ne porte que sur 0001-0004 et reste satisfait.

## Décisions

### Node 24 en local comme en CI

La spec dit « ≥ 20 » : 24 la respecte. Le point réel est ailleurs — un
`pnpm-lock.yaml` produit sous Node 24 et vérifié sous Node 20 peut diverger,
et la CI échouerait alors pour une raison sans rapport avec le code.

Deux valeurs distinctes, qui ne disent pas la même chose :

- `engines.node: ">=20.19"` dans `package.json` — le **plancher supporté**,
  conforme à la spec ;
- `.nvmrc` à `24`, utilisé par la CI via `node-version-file` — la **version
  effectivement exécutée**, identique sur le poste et sur les runners.

### React 19

`create-vite` génère React 19. Forcer React 18 signifierait rétrograder
volontairement vers une version dont le cycle se referme, et écrire de la
dette dès la première ligne.

La spec §6.2 relativise elle-même ce choix : « le frontend est
interchangeable. La seule contrainte est de rester une SPA statique servie par
la WebView et de communiquer exclusivement via l'IPC Tauri. » Cette contrainte
est tenue. React 19 est donc retenu.

Conséquence à surveiller au jalon 001 : la compatibilité de shadcn/ui et de
TanStack Table/Virtual avec React 19 — acquise à ce jour, à revérifier au
moment de les introduire.

### ESLint, contre le scaffolding

C'est le seul point où l'on va **contre** l'outillage, et il mérite d'être
justifié.

`oxlint` est nettement plus rapide. Mais trois textes nomment `eslint` — guide
§15.1 étape 3, jalon 000 §5.1, CA-000-04 — et surtout ESLint apporte ici
quelque chose qu'aucun gain de vitesse ne compense : `no-restricted-imports`
avec une **exception par répertoire**. C'est ce qui interdit
`@tauri-apps/api` partout sauf dans `ui/src/ipc/`, et donc ce qui transforme
la règle « jamais d'`invoke` brut dans un composant » (CLAUDE.md §4) d'une
convention de revue en un garde-fou mécanique.

Sur un projet de cette taille, la durée du lint n'est pas le facteur limitant.
La solidité de la frontière IPC, si.

### TypeScript 6

`create-vite` retient TypeScript ~6.0 alors que 7.0 est publié : à ce jour,
`typescript-eslint` déclare `typescript >=4.8.4 <6.1.0`. Adopter TS 7
supprimerait le lint typé. On suit donc l'outillage, sans zèle.

## Conséquences

- **Positives :** aucun décalage de version entre le poste et la CI ; chaîne
  d'outils alignée sur ce que l'écosystème produit aujourd'hui ; frontière IPC
  réellement gardée.
- **Négatives / dette assumée :** trois écarts documentés avec la spec. Ce
  fichier est la seule trace qui les explique — la spec, elle, n'est pas
  modifiée (elle décrit une intention datée).
- **Points de réexamen :** ESLint 10 est tout récent et pnpm l'a d'ailleurs
  placé sous `minimumReleaseAgeExclude` ; TypeScript 7 sera adoptable dès que
  `typescript-eslint` le supportera ; passer à `oxlint` redeviendrait
  discutable le jour où il gérerait les exceptions d'import par répertoire.

## Références

- Spec §6.2, §6.3 · Guide §3.1, §10.1, §15.1 · CLAUDE.md §4
- `.nvmrc`, `package.json`, `ui/package.json`, `ui/eslint.config.js`
