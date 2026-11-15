# ADR 0018 — Injecter le transport de session, et effacer son type pour y arriver

> **Statut :** Accepté
> **Date :** 2026-11-15 · **Jalon :** step-010 (recette M3) · **Décideur :** Martial Anouman
> **Applique la méthode de :** [ADR 0010](0010-inversion-des-ports-du-chemin-d-envoi.md) et [ADR 0012](0012-inversion-du-port-des-contacts.md)

## Contexte

`smpp_session::Transport` est une abstraction depuis le jalon 005, et son
en-tête dit pourquoi : la logique de session ne nomme jamais `TcpStream`, toute
la suite d'intégration tourne sur `tokio::io::duplex`, et TLS (jalon 015) sera
une seconde implémentation du trait plutôt qu'une branche dans la session.
`SessionRegistry<T: Transport>` est génrique en conséquence.

L'inversion s'arrêtait une couche trop tôt. `SessionServices::new` écrivait :

```rust
registry: Arc::new(SessionRegistry::new(TcpTransport)),
```

La couche haute instanciait elle-même la dépendance concrète. Le trait existait,
le registre était génrique, et le **choix** était figé à la compilation pour
toute l'application.

Ce qui l'a mis au jour est la recette M3 (CA-010-12). Le point d'assemblage à
tester est `launch`, qui cherche sa session dans ce registre ; or un registre
paramétré par `TcpTransport` n'accepte rien d'autre. Le premier jet du test a
donc dressé un `TcpListener` sur la boucle locale devant le SMSC factice et
recopié les octets dans les deux sens — une vingtaine de lignes de tuyauterie,
un port ouvert par test, et une suite qui ne pouvait plus se passer de la pile
réseau de l'hôte. Le test payait une dépendance que le code de production
n'avait pas inversée.

La contrainte qui rend la question non triviale : on ne peut pas simplement
rendre `SessionServices` génrique. Il est détenu par `AppState`, que **chaque**
`#[tauri::command]` reçoit en `State<'_, AppState>`, et une commande Tauri ne
peut pas être génrique. Un paramètre de type sur `SessionServices` se
propagerait à `AppState` puis à la trentaine de commandes, où il ne compile pas.

## Options envisagées

### Option A — Garder `TcpTransport` en dur, et le pont dans le test

Description : le code de production ne change pas ; chaque test qui a besoin
d'une session passe par un socket local.

**Pour :** aucun changement de production ; la pile réelle, transport compris,
est exercée de bout en bout.
**Contre :** la testabilité est achetée à chaque appelant au lieu d'être
fournie une fois ; un port et deux tâches de recopie par test ; le temps
virtuel de Tokio devient inutilisable là où il aurait pu servir ; et le jalon
015 trouvera la même impasse, puisque choisir TLS depuis un réglage demande
exactement ce que le pont contourne.

### Option B — Rendre `SessionServices` génrique sur le transport

Description : `SessionServices<T: Transport>`, et `AppState<T>` avec lui.

**Pour :** zéro coût à l'exécution, le choix reste statique.
**Contre :** ne compile pas. `#[tauri::command]` impose une signature concrète
pour `State<'_, AppState>` ; le paramètre contaminerait toute la frontière IPC.

### Option C — Effacer le type du transport, et l'injecter au démarrage

Description : à côté de `Transport`, un `DynTransport` object-safe — le même
contrat, type de flux effacé — obtenu par une implémentation générale pour tout
`Transport`, de sorte qu'aucun transport ne l'implémente à la main. Le type
`SharedTransport = Arc<dyn DynTransport>` est lui-même un `Transport`, donc il
entre dans `SessionRegistry` comme n'importe quel autre, et `AppState::new`
reçoit celui que l'application a choisi.

**Pour :** le choix se fait une fois, au démarrage, là où il appartient ; le
registre et la session restent génriques et inchangés ; le test injecte le SMSC
factice et le pont disparaît ; le jalon 015 choisira TLS depuis le profil au
même endroit.
**Contre :** une allocation et un appel indirect **par connexion** — pas par
PDU ; deux formes du même trait à garder alignées ; un `Box<dyn ByteStream>` à
la place d'un `TcpStream` nommé.

## Décision

**Option C.** Le critère d'arbitrage n'est pas le coût à l'exécution — une
allocation par connexion, contre des milliers de PDU qui suivent, ne se mesure
pas — mais **où le choix de la dépendance est fait**. Le guide §8.1 veut que la
couche haute déclare l'abstraction et qu'on lui fournisse l'implémentation ;
`SessionServices` déclarait en dépendre et se la donnait lui-même, ce qui est
la moitié du principe.

L'option A est écartée pour ce qu'elle révélait : un pont dans un test n'est
pas un détail de test, c'est la facture d'une inversion manquante, et elle
serait réglée à nouveau à chaque appelant — le jalon 015 en premier. L'option B
est écartée parce qu'elle ne compile pas, et c'est précisément ce qui force
l'effacement de type : il n'est pas un raffinement, c'est la seule forme
d'injection que la frontière Tauri laisse passer.

Les deux formes ne peuvent pas diverger : `impl<T: Transport> DynTransport for T`
est la seule implémentation de `DynTransport`, et un transport qui n'implémente
pas `Transport` n'existe pas.

## Conséquences

- **Positives :**
  - le SMSC scripté de `smpp_session::testing` est injectable dans
    l'application entière, ce qui est ce qui rend
    `src-tauri/src/commands/campaign/assembly.rs` possible sans socket ;
  - le jalon 015 branche TLS en passant une autre implémentation à
    `AppState::new`, sans toucher ni la session ni le registre ;
  - un seul endroit dans tout le dépôt nomme encore `TcpTransport` en
    production : `crate::run`.
- **Négatives / dette assumée :**
  - une allocation et une indirection par connexion. Négligeable au regard de
    la poignée de connexions qu'une session ouvre dans sa vie, mais réelle ;
  - `BoxedStream` remplace un type de flux nommé dans la signature du
    registre, donc un message d'erreur du compilateur y est un cran moins
    parlant ;
  - `ByteStream` existe uniquement pour que `BoxedStream` puisse s'écrire —
    `dyn AsyncRead + AsyncWrite` n'est pas un type. C'est un trait sans
    contenu, et il faut savoir pourquoi il est là.
- **Impacts opérationnels :** aucune dépendance ajoutée ; les dev-dependencies
  `tokio/net` et `tokio/io-util` de `src-tauri`, que le pont exigeait, sont
  retirées. Le contrat IPC ne change pas.
- **Points de réexamen :** si un transport devait être choisi **par session**
  plutôt que par application — un profil en clair et un profil TLS côté à côte,
  ce que le jalon 015 pourrait vouloir — alors c'est le registre qui doit porter
  le choix, pas l'application, et cette ADR serait à superséder.

## Références

- `crates/smpp-session/src/actors/transport.rs` — `Transport`, `DynTransport`,
  `SharedTransport`
- `src-tauri/src/sessions.rs`, `src-tauri/src/state.rs`, `src-tauri/src/lib.rs`
- `tasks-todo/step-010.md` §4 (CA-010-12), `tasks-todo/step-015.md` (TLS)
- Guide §8.1 — inversion de dépendance par ports ; CLAUDE.md §3

---

> **Une ADR est immuable.** Si la décision change, on n'édite pas ce fichier :
> on en écrit une nouvelle qui **supersède** celle-ci, et on met à jour le
> statut ci-dessus avec un lien.
