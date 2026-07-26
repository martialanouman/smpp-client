# Jalon 004 — Encodage du texte et segmentation des messages longs

> **Statut :** Terminé (2026-07-26) — 10 critères sur 10 vérifiés · **Dépend de :** step-003 · **Réf. spec :** §7.5 · **Exigences :** EF-MSG-03, EF-MSG-04

## 1. Objectif

Transformer un texte utilisateur quelconque en une séquence de segments SMPP prêts à l'émission, avec le DCS optimal choisi automatiquement, la segmentation correcte au-delà d'un segment, et un compteur de caractères/segments exploitable en direct par l'UI.

C'est la brique la plus piégeuse du projet : les seuils (160/153, 70/67), l'alphabet GSM 03.38 et son extension, l'échappement des caractères étendus qui comptent double, et le choix UDH ⇄ TLV `sar_*` produisent des bugs silencieux qui n'apparaissent qu'à la réception sur un vrai téléphone. Elle est traitée isolément, sans I/O, pour être exhaustivement testable.

## 2. Périmètre

### Dans le périmètre

- Détection de l'alphabet : si tous les caractères appartiennent à **GSM 03.38** (table de base + table d'extension) → GSM 7-bit (DCS 0x00) ; sinon → UCS2 (DCS 0x08).
- Forçage manuel de l'encodage (GSM 7-bit / Latin-1 / UCS2) au niveau message ou campagne.
- Packing GSM 7-bit (septets) et encodage UCS2 (UTF-16BE).
- Calcul du nombre de segments et de la longueur utile, en tenant compte du fait qu'un caractère de la table d'extension consomme **deux** septets — y compris à cheval sur une frontière de segment.
- Segmentation avec deux modes configurables :
  - **UDH** de concaténation (IEI 0x00, 6 octets : référence, total, index) + `esm_class` UDHI activé ;
  - TLV `sar_msg_ref_num` / `sar_total_segments` / `sar_segment_seqnum`.
- Utilisation de `message_payload` (jusqu'à 64 Ko) en alternative, quand le SMSC le supporte.
- API de prévisualisation pour l'UI : `(encodage détecté, caractères utilisés, caractères restants dans le segment courant, nombre de segments)`.

### Hors périmètre

- L'envoi effectif des segments et leur corrélation → **step-006**.
- Le rendu du compteur dans l'éditeur de message → **step-006** (l'API est fournie ici).
- La résolution des variables `{{prenom}}` → **step-010** (la segmentation s'applique au texte déjà résolu).

## 3. Livrables

| # | Livrable | Emplacement |
|---|----------|-------------|
| L-004-01 | Tables GSM 03.38 (base + extension) et détection d'alphabet | `crates/messaging/src/encoding/gsm0338.rs` |
| L-004-02 | Encodeurs GSM 7-bit / Latin-1 / UCS2 | `crates/messaging/src/encoding/` |
| L-004-03 | Segmenteur (UDH et `sar_*`) | `crates/messaging/src/segmentation.rs` |
| L-004-04 | API de prévisualisation (compteur) | `crates/messaging/src/encoding/preview.rs` |
| L-004-05 | Erreur typée `EncodingError` | `crates/messaging/src/encoding/error.rs` |

## 4. Critères d'acceptation

- [x] **CA-004-01** — Un texte de 160 caractères GSM produit **1** segment ; 161 caractères produisent **2** segments de 153 et 8.
- [x] **CA-004-02** — Un texte contenant « € » (table d'extension) voit ce caractère compter pour **2** septets, y compris dans le calcul du nombre de segments.
- [x] **CA-004-03** — Un texte contenant un caractère hors GSM (ex. « 你 », « ł ») bascule automatiquement en UCS2 : 70 caractères → 1 segment, 71 → 2 segments de 67 et 4.
- [x] **CA-004-04** — Le forçage manuel est respecté ; forcer GSM 7-bit sur un texte non représentable retourne une `EncodingError` explicite plutôt que de produire des caractères corrompus.
- [x] **CA-004-05** — Aucun segment ne coupe un caractère en deux : en UCS2, aucune paire de substitution (surrogate pair) n'est scindée ; en GSM, aucun caractère d'extension n'est séparé de son octet d'échappement.
- [x] **CA-004-06** — Mode UDH : chaque segment porte un UDH de 6 octets, `esm_class` a le bit UDHI positionné, la référence de concaténation est identique sur tous les segments et l'index va de 1 à N.
- [x] **CA-004-07** — Mode `sar_*` : les trois TLV sont présents et cohérents, `esm_class` **ne** porte **pas** le bit UDHI.
- [x] **CA-004-08** — La concaténation inverse des segments (fonction de test) restitue exactement le texte d'origine, pour les deux modes et les trois encodages.
- [x] **CA-004-09** — L'API de prévisualisation retourne les mêmes valeurs que la segmentation réelle : test de propriété comparant les deux sur des textes aléatoires.
- [x] **CA-004-10** — Aucune allocation superflue par segment sur le chemin chaud (revue + micro-benchmark `criterion` de référence, servant de base de comparaison en step-017).

## 5. Tests attendus

- **Unitaires :** chaque caractère de la table GSM de base et de la table d'extension ; textes aux frontières exactes 159/160/161, 69/70/71, 152/153/154, 66/67/68 ; texte vide ; texte d'un seul caractère d'extension.
- **Propriété (`proptest`) :** round-trip texte → segments → texte pour des chaînes Unicode arbitraires ; cohérence prévisualisation ⇄ segmentation ; le nombre de segments est monotone croissant avec la longueur du texte.
- **Non-régression :** tout caractère mal encodé signalé en aval devient un cas de test nommé.

## 6. Notes d'implémentation

- Doc à consulter avant de coder :
  ```bash
  npx ctx7@latest docs /rusmpp/rusmpp "short_message OctetString message_payload TLV sar_msg_ref_num and EsmClass UDHI"
  ```
- Ne pas confondre **caractères** et **octets** : GSM 7-bit compte en septets après packing, UCS2 en octets (2 par unité de code UTF-16), et Latin-1 en octets. Un seul type `SegmentBudget` explicite évite les confusions d'unités.
- Le choix UDH vs `sar_*` est une caractéristique du SMSC, pas du message : il est **configuré par session/campagne** et transmis au segmenteur, qui ne devine rien.
- La référence de concaténation (8 ou 16 bits) doit être unique par destinataire sur une fenêtre de temps raisonnable — décider de la stratégie (compteur cyclique par session) et la documenter ; une collision produit des messages mélangés côté terminal.
- `message_payload` et `short_message` sont **exclusifs** : quand `message_payload` est utilisé, `sm_length` vaut 0.
