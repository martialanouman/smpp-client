# Jalon 002 — Persistance SQLite : schéma, migrations et repositories

> **Statut :** À faire · **Dépend de :** step-001 · **Réf. spec :** §14 · **Réf. guide :** §11

## 1. Objectif

Disposer d'une base SQLite embarquée, en mode WAL, dont le schéma est créé par des migrations versionnées et dont l'accès est exclusivement encapsulé dans des repositories typés et testables.

La persistance conditionne la garantie « aucune perte de message » (ENF-FIA-01) : tout message est écrit **avant** émission. Ce jalon pose le socle de données et surtout les **règles d'accès** — pas de SQL disséminé, pas de chargement intégral en mémoire, écritures chaudes groupées — qui déterminent la tenue en charge des jalons 007 à 014.

## 2. Périmètre

### Dans le périmètre

- Crate `persistence` : pool SQLx, activation WAL, ouverture/initialisation de la base dans le répertoire de données applicatif.
- Migrations versionnées dans `migrations/` couvrant les tables de la spec §14.2 : `session_profiles`, `contacts`, `contact_lists`, `contact_list_members`, `campaigns`, `messages`, `pdu_log` — avec les index `idx_contacts_msisdn`, `idx_messages_campaign`, `idx_messages_state`, `idx_messages_smscid`.
- Repositories par agrégat, exposant des méthodes **métier** (`insert_message`, `update_state`, `stream_messages(filter)`, `upsert_session_profile`…), jamais du SQL brut vers l'extérieur.
- Traits de port (`MessageRepository`, `ContactRepository`, `CampaignRepository`, `SessionProfileRepository`) permettant des doubles de test.
- Pagination et streaming par curseur pour tous les ensembles volumineux.
- Écritures groupées en transaction pour les mises à jour d'état de messages.
- Harnais de test : base temporaire par test, application des migrations, jeu de données minimal.
- Activation de l'étape 8 de la CI (migrations appliquées sur base neuve + vérification du schéma).

### Hors périmètre

- Le chiffrement du champ `password_enc` : la colonne existe et reçoit un blob opaque, mais la cryptographie appartient à **step-015**. Tant que step-015 n'est pas livré, aucun mot de passe réel n'est stocké.
- Les requêtes d'agrégation statistique → **step-014**.
- La politique de rétention/purge/VACUUM → **step-014**.
- Toute logique métier utilisant ces repositories → jalons consommateurs.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-002-01 | Pool, configuration WAL, ouverture de base | `crates/persistence/src/db.rs` |
| L-002-02 | Migrations SQL versionnées | `migrations/` |
| L-002-03 | Repositories typés | `crates/persistence/src/repositories/` |
| L-002-04 | Traits de port réutilisables par les couches hautes | `crates/persistence/src/ports.rs` (ou définis côté consommateur) |
| L-002-05 | Erreur typée `PersistenceError` | `crates/persistence/src/error.rs` |
| L-002-06 | Harnais de test avec base temporaire | `crates/persistence/tests/` |
| L-002-07 | Recette `just migrate` opérationnelle | `justfile` |

## 4. Critères d'acceptation

- [ ] **CA-002-01** — `just migrate` sur une base neuve crée l'intégralité du schéma ; `PRAGMA journal_mode` retourne `wal`.
- [ ] **CA-002-02** — Toutes les tables et tous les index de la spec §14.2 existent, vérifiés par un test qui interroge `sqlite_master`.
- [ ] **CA-002-03** — Aucun SQL hors de `crates/persistence` : `rg -i "SELECT |INSERT INTO|UPDATE .* SET" crates src-tauri --glob '!crates/persistence/**'` ne retourne rien.
- [ ] **CA-002-04** — Les requêtes sont vérifiées à la compilation (`sqlx::query!` / `query_as!`) partout où c'est possible ; les exceptions sont commentées et justifiées.
- [ ] **CA-002-05** — `stream_messages` sur 100 000 lignes ne charge pas l'ensemble en mémoire : test mesurant que la consommation reste bornée et que le premier élément arrive avant la fin du parcours.
- [ ] **CA-002-06** — Une mise à jour d'état par lot de N messages produit **une** transaction, pas N (vérifié par test).
- [ ] **CA-002-07** — Chaque test d'intégration utilise sa propre base temporaire ; l'exécution en parallèle via `cargo nextest run` est stable sur 10 exécutions consécutives.
- [ ] **CA-002-08** — Aucune migration livrée n'est modifiée : un test/contrôle CI compare l'empreinte des migrations existantes.
- [ ] **CA-002-09** — `PersistenceError` est exhaustive, sans `Box<dyn Error>` dans l'API publique ; aucune valeur de secret n'apparaît dans un message d'erreur.
- [ ] **CA-002-10** — L'étape 8 de la CI est active et bloquante.

## 5. Tests attendus

- **Unitaires/intégration :** CRUD par repository, contraintes de clés, transitions d'état de message, filtres et pagination, insertion en lot, comportement en cas de conflit d'unicité.
- **Migrations :** application sur base neuve ; application successive de toutes les migrations en ordre ; vérification du schéma résultant.
- **Concurrence :** écritures depuis plusieurs tâches Tokio pendant une lecture longue — WAL doit permettre la lecture sans blocage.
- **Non-régression :** test garantissant qu'une migration déjà livrée n'a pas été éditée.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest library "SQLx" "SQLite pool configuration, WAL mode, compile-time checked queries and migrations"
  ```
- Le mode WAL et les `PRAGMA` (`synchronous`, `busy_timeout`, `foreign_keys`) doivent être appliqués **à chaque connexion** du pool, pas seulement à la première : c'est une source classique de comportements incohérents en charge.
- Les horodatages sont stockés en TEXT ISO-8601 UTC (cohérent avec le schéma spec) ; centraliser la conversion dans un helper unique pour éviter les formats divergents.
- Les traits de port sont définis **du côté qui consomme** (guide §8.1) quand on veut inverser la dépendance ; ce jalon fournit les implémentations. Décider explicitement de l'emplacement et le documenter.
- Prévoir dès maintenant l'index nécessaire à la corrélation DLR (`idx_messages_smscid`) même si le DLR arrive en step-008 : l'ajouter plus tard sur une table volumineuse est coûteux.
