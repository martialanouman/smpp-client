# Jalon 005 — Session SMPP unique : acteurs, bind, keep-alive et reconnexion

> **Statut :** À faire · **Dépend de :** step-002, step-003 · **Réf. spec :** §7.9, §8.2, §8.3 · **Exigences :** EF-CNX-01, EF-CNX-02, EF-CNX-04, EF-CNX-05, EF-CNX-06

## 1. Objectif

Établir et maintenir **une** session SMPP vers un SMSC : connexion TCP, bind (TX/RX/TRX), keep-alive par `enquire_link`, détection de perte de lien, reconnexion avec back-off exponentiel et jitter, et arrêt propre par `unbind`.

Ce jalon introduit la concurrence réelle du projet. Le modèle d'acteurs — une tâche propriétaire du socket, les autres communiquant par canaux — n'est pas un choix esthétique : il élimine par construction les data races sur le flux et rend la session testable sans réseau via un `DuplexStream`.

## 2. Périmètre

### Dans le périmètre

- Crate `smpp-session` : profil de session (spec §8.2) persisté via `persistence`, et acteurs Tokio :
  - **connection** : propriétaire unique du `Framed<TcpStream, CommandCodec>`, effectue le bind ;
  - **writer** : consomme la file d'émission et écrit les PDU ;
  - **reader** : lit les PDU entrants, résout les `oneshot` en attente, route les PDU non sollicités ;
  - **keep-alive** : émet `enquire_link` à l'intervalle configuré ;
  - **supervisor** : surveille la santé, déclenche la reconnexion, publie l'état.
- Corrélation requête/réponse par `sequence_number` via `oneshot`, avec `response_timeout` et libération garantie de l'entrée en attente (y compris en cas de timeout ou d'annulation).
- Machine à états `CLOSED → CONNECTING → BINDING → BOUND → UNBOUND / ERROR|RECONNECT` (spec §7.9), diffusée par `watch`.
- Reconnexion : back-off exponentiel borné (1 s → 60 s) **avec jitter** ; les erreurs classées `Fatal` en step-003 (`ESME_RINVPASWD`, `ESME_RINVSYSID`) **n'entraînent pas** de boucle de reconnexion mais une remontée à l'UI.
- Arrêt propre par `CancellationToken` : `unbind`, drainage, fermeture du socket.
- Commandes IPC `session_create`, `session_update`, `session_delete`, `session_bind`, `session_unbind`, `session_list`, `session_status` ; écran Sessions fonctionnel (création de profil, bind/unbind, état en direct).
- Choix explicite de l'`interface_version` (0x34 / 0x50) annoncé au bind.

### Hors périmètre

- Le fenêtrage et le limiteur de débit → **step-007** (le writer émet ici sans régulation, un PDU à la fois).
- L'envoi de `submit_sm` métier → **step-006** (ce jalon envoie `enquire_link` et, pour les tests, un `submit_sm` brut).
- Les sessions multiples et le multi-bind → **step-011** : ici, une seule session à la fois.
- TLS → **step-015** (connexion en clair ; l'avertissement UI est posé en step-015).
- Le stockage chiffré du mot de passe → **step-015** : en attendant, aucun mot de passe réel n'est persisté (saisie en mémoire pour la durée de la session).

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-005-01 | Profil de session + repository | `crates/smpp-session/src/profile.rs`, `crates/persistence/` |
| L-005-02 | Acteurs de session et canaux | `crates/smpp-session/src/actors/` |
| L-005-03 | Machine à états + diffusion `watch` | `crates/smpp-session/src/state.rs` |
| L-005-04 | Table de corrélation `sequence_number` → `oneshot` | `crates/smpp-session/src/pending.rs` |
| L-005-05 | Politique de reconnexion (back-off + jitter + classification) | `crates/smpp-session/src/reconnect.rs` |
| L-005-06 | Commandes IPC de session + événement `sessions:state` | `src-tauri/src/commands/session.rs` |
| L-005-07 | Écran Sessions | `ui/src/views/Sessions/` |
| L-005-08 | Double de test : serveur SMPP en mémoire (`DuplexStream`) | `crates/smpp-session/tests/support/` |

## 4. Critères d'acceptation

- [ ] **CA-005-01** — Un bind TRX réussi fait passer l'état à `BOUND` et l'affiche dans l'UI en moins d'une seconde après la réponse.
- [ ] **CA-005-02** — Les trois types de bind (TX, RX, TRX) fonctionnent et refusent les opérations incompatibles (émettre sur une session RX retourne une erreur typée, sans panic).
- [ ] **CA-005-03** — Un bind rejeté avec `ESME_RINVPASWD` remonte une erreur explicite à l'UI et **ne déclenche aucune tentative de reconnexion** (vérifié par test : zéro nouvelle connexion pendant 3× le back-off minimal).
- [ ] **CA-005-04** — `enquire_link` est émis à l'intervalle configuré (±10 %) ; l'absence de `enquire_link_resp` au-delà du seuil fait transiter la session vers `RECONNECT`.
- [ ] **CA-005-05** — Une coupure TCP brutale déclenche une reconnexion avec back-off exponentiel **et jitter** : test vérifiant que les intervalles croissent, restent bornés par `max_backoff_s`, et ne sont pas tous identiques.
- [ ] **CA-005-06** — Une réponse qui n'arrive jamais libère son entrée de la table de corrélation après `response_timeout` : la table revient à zéro entrée, aucune fuite mémoire sur 10 000 requêtes expirées.
- [ ] **CA-005-07** — Un PDU inattendu ou malformé ne tue pas la session : `generic_nack` est émis si nécessaire, l'incident est journalisé, la session reste `BOUND`.
- [ ] **CA-005-08** — La fermeture de l'application émet `unbind`, attend `unbind_resp` (borné par un timeout) et ferme proprement ; aucune tâche ne survit (test : toutes les `JoinHandle` sont terminées).
- [ ] **CA-005-09** — Aucun `.await` n'est effectué en tenant un `std::sync::Mutex` ; revue de code explicite + `rg "std::sync::Mutex" crates/smpp-session` justifié ligne à ligne.
- [ ] **CA-005-10** — Une seule tâche accède au socket : le `Framed` n'est ni clonable ni partagé (garanti par le type, vérifié en revue).
- [ ] **CA-005-11** — Aucun mot de passe n'apparaît dans les traces, y compris au niveau `trace` : test qui capture la sortie `tracing` pendant un bind et cherche le secret.
- [ ] **CA-005-12** — Deux approbations requises pour la fusion (cœur protocolaire, guide §16.2).

## 5. Tests attendus

- **Intégration sans réseau :** serveur SMPP factice sur `tokio::io::duplex` — bind OK, bind rejeté, `enquire_link` sans réponse, coupure en cours d'échange, PDU malformé, `unbind` initié par le serveur.
- **Unitaires :** transitions de la machine à états (toutes les arêtes du diagramme spec §7.9), calcul du back-off (croissance, bornage, présence de jitter), table de corrélation (insertion, résolution, expiration, annulation).
- **Déterminisme :** l'horloge est injectée ; aucun test ne dépend de `tokio::time::sleep` réel (utiliser le temps virtuel de Tokio).
- **Non-régression :** chaque incident de session observé en aval ajoute son scénario au serveur factice.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /rusmpp/rusmpp "rusmppc ConnectionBuilder bind_transceiver enquire_link interval response timeout and Event stream"
  npx ctx7@latest library "tokio-util" "CancellationToken graceful shutdown for spawned tasks"
  ```
- **L'ADR 0001 se concrétise ici.** Si le client haut niveau `rusmppc` est retenu, vérifier qu'il laisse le contrôle nécessaire au fenêtrage (step-007) et à la reconnexion pilotée par la classification des `command_status` ; sinon, assumer le codec bas niveau et écrire les acteurs à la main. Ne pas laisser ce choix implicite.
- Le `sequence_number` doit rester dans la plage 1..=0x7FFFFFFF et boucler sans jamais réutiliser une valeur encore en attente.
- Le back-off sans jitter provoque des reconnexions synchronisées quand plusieurs sessions tombent ensemble (step-011) : le jitter n'est pas optionnel.
- Toute `spawn` a un propriétaire qui gère sa fin et ses erreurs (guide §7.5) : pas de tâche orpheline. Le superviseur détient les `JoinHandle`.
