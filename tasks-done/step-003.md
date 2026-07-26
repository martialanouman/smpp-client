# Jalon 003 — Cœur protocolaire `smpp-core`

> **Statut :** Terminé (2026-07-26) — 9 critères sur 9 vérifiés · **Dépend de :** step-000 · **Réf. spec :** §7.1, §7.2, §7.4, §7.6 · **Réf. guide :** §4.2, §5.6

## 1. Objectif

Encoder et décoder l'ensemble des PDU SMPP v3.4 et v5.0 utilisés par l'application, avec des types Rust exhaustifs pour TON, NPI, DCS, `command_id` et `command_status`, et une table complète des codes de statut assortis de leur libellé en clair.

`smpp-core` est la crate la plus basse et la seule sans dépendance interne. Elle ne connaît ni socket, ni base de données, ni session : elle transforme des octets en types sûrs et inversement. Cette pureté la rend entièrement testable et permet les tests de propriété qui garantissent l'absence de perte d'information à l'encodage.

## 2. Périmètre

### Dans le périmètre

- Intégration de `rusmpp` : réexport contrôlé des types de PDU, ou types de façade si l'API interne doit être stabilisée (**décision à consigner en ADR**).
- Enums typés et convertibles depuis/vers les octets : `Ton`, `Npi`, `DataCoding`, `EsmClass`, `RegisteredDelivery`, `PriorityFlag`, `InterfaceVersion`, `CommandId`, `CommandStatus`.
- Table exhaustive des `command_status` v3.4 et v5.0 avec, pour chacun : valeur hexadécimale, nom symbolique (`ESME_RTHROTTLED`…), libellé clair FR/EN, et **classification** `Fatal` / `Récupérable` / `Throttling` exploitée par les jalons 005, 007, 010 et 012.
- Newtypes du domaine : `Msisdn` (construction validante), `SessionId`, `ClientMessageId`, `SequenceNumber`.
- Erreur typée `SmppError` (décodage, PDU inattendu, champ invalide).
- Utilitaire de dump hexadécimal d'un PDU, pour le mode debug.

### Hors périmètre

- Toute I/O : pas de `TcpStream`, pas de `Framed` monté ici → **step-005**.
- L'encodage du texte et la segmentation → **step-004**.
- La machine à états de session → **step-005**.
- Les TLV et PDU spécifiques v5.0 (`broadcast_sm`, portabilité, identification de nœud) → **step-012** ; ce jalon les rend seulement représentables sans les exploiter.

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-003-01 | Façade de codec PDU (encode/décode) | `crates/smpp-core/src/codec.rs` |
| L-003-02 | Enums typés du protocole | `crates/smpp-core/src/values/` |
| L-003-03 | Table des `command_status` + classification + libellés | `crates/smpp-core/src/status_codes.rs` |
| L-003-04 | Newtypes du domaine | `crates/smpp-core/src/types.rs` |
| L-003-05 | `SmppError` | `crates/smpp-core/src/error.rs` |
| L-003-06 | Dump hexadécimal de PDU | `crates/smpp-core/src/debug.rs` |
| L-003-07 | ADR sur le niveau d'API rusmpp retenu | `docs/adr/0001-choix-de-la-pile-smpp.md` |

## 4. Critères d'acceptation

- [x] **CA-003-01** — Tous les PDU du tableau spec §7.2 sont encodables et décodables ; un test paramétré parcourt la liste et échoue si un `command_id` n'est pas couvert.
- [x] **CA-003-02** — Round-trip `décoder(encoder(pdu)) == pdu` vérifié par `proptest` sur au moins `submit_sm`, `submit_sm_resp`, `deliver_sm`, `bind_transceiver`, `enquire_link`, avec TLV présents et absents, corps vide et corps maximal.
- [x] **CA-003-03** — Un PDU malformé (longueur incohérente, chaîne non terminée, TLV tronqué) produit une `SmppError` typée, **jamais** un panic : test de fuzzing léger sur des octets aléatoires, aucune panique sur 10 000 entrées.
- [x] **CA-003-04** — `Msisdn::parse` refuse les entrées invalides et retourne un `Result` ; il est impossible de construire un `Msisdn` sans passer par la validation (constructeur privé, vérifié par un test de compilation ou une revue du champ).
- [x] **CA-003-05** — La table `command_status` couvre l'intégralité des codes v3.4 et v5.0 ; chaque code expose nom symbolique, libellé clair et classification.
- [x] **CA-003-06** — La classification est correcte sur les cas critiques : `ESME_RINVPASWD` et `ESME_RINVSYSID` sont `Fatal` (pas de boucle de reconnexion), `ESME_RTHROTTLED` et `ESME_RMSGQFUL` sont `Throttling`, `ESME_RINVDSTADR` est non rejouable.
- [x] **CA-003-07** — `crates/smpp-core/Cargo.toml` ne déclare **aucune** dépendance interne au workspace.
- [x] **CA-003-08** — Couverture ≥ 80 % sur la crate (`cargo llvm-cov`).
- [x] **CA-003-09** — Aucun `unwrap`/`expect`/`panic!` hors `#[cfg(test)]` : `rg "unwrap\(\)|expect\(|panic!" crates/smpp-core/src` ne retourne que des occurrences en test ou accompagnées d'un commentaire `// INVARIANT:`.

## 5. Tests attendus

- **Propriété (`proptest`) :** round-trip d'encodage, invariance de `command_length`, stabilité des conversions enum ⇄ octet sur toute la plage `u8`.
- **Unitaires :** vecteurs de test issus des spécifications (PDU de référence en hexadécimal → structure attendue), cas limites de longueur, TLV inconnus préservés plutôt que rejetés.
- **Robustesse :** décodage d'octets aléatoires et tronqués sans panique.
- **Non-régression :** chaque PDU mal décodé découvert ultérieurement ajoute son vecteur hexadécimal aux fixtures.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /rusmpp/rusmpp "CommandCodec encode decode Pdu builders TLV and CommandStatus"
  ```
- **Décision structurante (ADR 0001).** `rusmpp` expose deux niveaux : le codec bas niveau (`Framed<S, CommandCodec>` sur `Command`/`Pdu`) et le client haut niveau `rusmppc` (`ConnectionBuilder` gérant keep-alive, timeouts, bind et flux d'`Event`). Le second fait gagner beaucoup de travail en step-005, mais réduit le contrôle sur le fenêtrage, la corrélation par `sequence_number` et la stratégie de reconnexion — qui sont précisément le cœur des jalons 005 et 007. Trancher **explicitement**, en pesant le coût d'un contournement ultérieur, et documenter les conséquences.
- Éviter de réexporter naïvement toute l'API de `rusmpp` : chaque symbole public est un engagement. Exposer une surface minimale et volontaire.
- Les libellés en clair alimentent l'UI (ENF-UTI-02) : les stocker comme données (table statique), pas comme `match` dispersés, pour rester traduisibles.
