# ADR 0007 — Segmenter au caractère, avec un budget unique par encodage

> **Statut :** Accepté
> **Date :** 2026-07-26 · **Jalon :** step-004 · **Décideur :** Martial Anouman

## Contexte

La spec §7.5 impose de découper un message qui dépasse un segment et de faire
porter à chaque partie de quoi la recomposer, par **UDH de concaténation** ou
par **TLV `sar_*`**, avec `message_payload` en troisième voie. Elle donne un
tableau de capacités : 160 caractères pour un segment GSM 7-bit isolé, 153 pour
un segment concaténé ; 70 et 67 pour UCS2 ; 140 et 134 octets pour Latin-1.

Trois questions restaient ouvertes, et chacune produit un bug qui ne se voit
qu'à la réception sur un téléphone réel.

**1. Où coupe-t-on ?** Le budget d'un segment se compte en *unités* — septets
pour GSM, unités de code UTF-16 pour UCS2 — mais un caractère ne vaut pas
toujours une unité. Les neuf caractères de la table d'extension GSM 03.38
(`^ { } [ ] ~ \ |` et `€`) s'écrivent `0x1B` suivi d'un code : **deux septets**.
Hors du plan de base, un caractère UCS2 s'écrit en paire de substitution :
**deux unités**. Couper entre les deux moitiés livre un échappement orphelin au
premier téléphone et un code orphelin au second.

**2. Quelle capacité en mode `sar_*` ?** En mode UDH les six octets d'en-tête
sortent du corps, d'où 153 septets. En mode `sar_*` l'information est
hors-bande et le corps est intact : il pourrait porter les 160 septets.

**3. Le mode est-il déduit ou fourni ?** UDH et `sar_*` ne sont pas
interchangeables selon les SMSC.

## Options envisagées

### Option A — Découper la séquence d'unités encodée par tranches de 153

Encoder tout le texte, puis trancher le tampon tous les 153 septets.

**Pour :** trivial, et exact tant que le texte est en ASCII.
**Contre :** coupe une paire d'échappement ou une paire de substitution dès
qu'il y en a une à cheval sur la frontière. Aucun test de haut niveau ne
l'attrape : le nombre de segments est correct, chaque segment est bien formé,
et seul le contenu affiché est faux.

### Option B — Découper caractère par caractère, avec un remplisseur glouton

Parcourir les caractères, connaître le coût de chacun dans l'encodage retenu,
et n'ouvrir un segment que sur une frontière de caractère. Un caractère de deux
unités face à une seule unité libre passe **entier** au segment suivant ;
l'unité restante est perdue.

**Pour :** correct par construction, et le seul endroit où la règle est écrite.
**Contre :** le nombre de segments ne se déduit plus d'une division ; il faut
simuler le remplissage — donc le simuler aussi pour le compteur en direct, sous
peine de voir l'interface et l'envoi diverger.

### Option C — Réclamer 160 septets par segment en mode `sar_*`

**Pour :** protocolairement exact, et gagne 4 % de capacité.
**Contre :** de nombreux SMSC retraduisent les TLV `sar_*` en UDH sur la
branche de livraison. Un segment dimensionné à 160 septets ne rentre alors plus
dans les 140 octets du `short_message` sortant, et le SMSC tronque ou rejette
— au moment de la livraison, pas de la soumission.

## Décision

**Option B**, avec le budget de la spec §7.5 appliqué **quel que soit le mode**.

1. **La coupure est décidée sur les caractères, jamais sur les unités.** Un
   seul type, `SegmentFiller`, énonce la règle gloutonne. Le planificateur
   (`plan`) et le segmenteur (`segment`) le partagent : leur accord n'est pas
   vérifié après coup, il est structurel. CA-004-09 se contente de le
   confirmer.

2. **Le budget concaténé est 153 / 67 / 134 dans les trois modes.** L'option C
   est écartée : la marge est faible, le risque est une troncature invisible
   côté livraison, et les critères CA-004-01 et CA-004-03 énoncent ces chiffres
   sans nommer de mode. En mode `sar_*` le corps porte donc jusqu'à 153 septets
   là où le protocole en autoriserait 160.

3. **Le mode est fourni par l'appelant**, configuré par session ou par
   campagne. `SegmentationMode::Udh` est le défaut : c'est la forme que tout
   téléphone comprend, les TLV `sar_*` étant optionnels en SMPP v3.4.

4. **La référence de concaténation est un compteur cyclique 16 bits par
   session**, atomique. Elle ne dépend pas du destinataire — plus fort que
   nécessaire, et bien plus simple qu'une table par destinataire. L'UDH 8 bits
   n'en garde que l'octet de poids faible : le cycle y est de 256 messages,
   plafond du format et non de cette implémentation.

5. **GSM 7-bit est packé**, sept octets pour huit septets, avec le bit de
   calage qu'impose un en-tête de six octets et le bourrage `CR` des sept bits
   libres (TS 23.038 §6.1.2.3.1). C'est ce qui rend cohérents les 160
   caractères et les 140 octets du tableau de la spec.

## Conséquences

- **Positives :** aucun caractère ne peut être coupé en deux, dans aucun
  encodage. Le compteur de l'éditeur et l'envoi réel ne peuvent pas diverger.
  Le segmenteur ne devine rien du SMSC.
- **Négatives / dette assumée :** le mode `sar_*` renonce à 7 septets par
  segment, soit environ 4 % de capacité. Un SMSC dont on saurait qu'il ne
  retraduit pas en UDH pourrait les récupérer ; ce serait une nouvelle ADR et
  un champ de configuration de plus.
- **Impacts opérationnels :** aucune dépendance ajoutée pour l'algorithme.
  L'UDH provient de `rusmpp` via `smpp-core`, qui valide déjà les invariants
  d'index. `criterion` entre en dépendance de développement pour le banc de
  référence (CA-004-10), sous licence MIT/Apache-2.0 déjà autorisée.
- **Points de réexamen :** si un SMSC rejette des segments `sar_*` de 153
  septets, ou si l'on veut exploiter les 160 ; si le besoin apparaît d'un UDH
  de concaténation 16 bits (IEI `0x08`), que ce jalon n'implémente pas.

## Références

- Spec §7.5 (tableau des capacités, algorithme de choix d'encodage,
  segmentation) · exigences EF-MSG-03 et EF-MSG-04
- `tasks-todo/step-004.md` — critères CA-004-01 à CA-004-10
- 3GPP TS 23.038 §6.1.2.3.1 (bourrage des bits libres), §6.2.1 (table de base
  et table d'extension GSM 03.38)
- [ADR 0001](0001-choix-de-la-pile-smpp.md) — les types de protocole viennent
  de `rusmpp`, réexportés par `smpp-core`
