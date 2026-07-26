# ADR 0008 — Segmenter au caractère, en septets non packés par défaut

> **Statut :** Accepté
> **Date :** 2026-07-26 · **Jalon :** step-004 · **Décideur :** Martial Anouman

## Contexte

La spec §7.5 impose de découper un message qui dépasse un segment et de faire
porter à chaque partie de quoi la recomposer, par **UDH de concaténation** ou
par **TLV `sar_*`**, avec `message_payload` en troisième voie. Elle donne un
tableau de capacités : 160 pour un segment GSM 7-bit isolé, 153 pour un segment
concaténé ; 70 et 67 pour UCS2 ; 140 et 134 octets pour Latin-1.

> **Le mot juste est septets, pas caractères.** Le tableau de la spec dit
> « caractères » ; ce sont des **septets**. Un caractère de la table d'extension
> (`€`, `{`, `}`, `[`, `]`, `~`, `\`, `|`, `^`) en coûte deux. Un texte de 153
> caractères contenant un `€` fait 154 septets et ne tient pas dans un segment
> concaténé. Ce n'est pas un écart avec la spec, c'est sa lecture correcte, et
> tout ce document compte en septets.

Quatre questions restaient ouvertes, et chacune produit un bug qui ne se voit
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

**4. Les septets GSM sont-ils packés dans `short_message` ?** GSM 03.38
§6.1.2.1.1 décrit huit septets serrés dans sept octets. La question est de
savoir *où* ce format s'applique : sur l'interface radio, ou déjà sur le lien
SMPP entre l'ESME et le SMSC ?

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

### Option D — Packer les septets GSM dans `short_message`

**Pour :** c'est la lettre de GSM 03.38 §6.1.2.1.1, et 160 septets tiennent
alors dans 140 octets.
**Contre :** ce paragraphe décrit le format **over-the-air**, pas le contenu
du champ `short_message`. Sur le lien SMPP la convention dominante est
l'inverse — un septet par octet, bit de poids fort à zéro — et c'est le SMSC
qui packe avant l'interface radio. Kannel, Jasmin, CloudHopper, Sinch, Infobip
et les agrégateurs commerciaux attendent du non packé ; `rusmpp-extra`
n'implémente d'ailleurs que `Gsm7BitUnpacked` et en fait son défaut. Le packé
sur le lien SMPP existe (ZTE, matériel opérateur ancien) mais c'est l'exception
documentée.

Se tromper est **silencieux dans les deux sens** : le SMSC répond `ESME_ROK`,
le DLR dit `DELIVRD`, et le destinataire reçoit du charabia. Rien ne remonte
jamais.

Le packé traîne en outre une fragilité propre au lien SMPP : `sm_length` compte
des **octets**, le nombre de septets n'est pas transmis, et le récepteur doit
diviser. Sur l'interface radio la question ne se pose pas — TP-UDL compte des
septets.

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
   session**, atomique et **amorcé aléatoirement**. Elle ne dépend pas du
   destinataire — plus fort que nécessaire, et bien plus simple qu'une table
   par destinataire. L'UDH 8 bits n'en garde que l'octet de poids faible : le
   cycle y est de 256 messages, plafond du format et non de cette
   implémentation. L'amorce aléatoire couvre le redémarrage : un compteur qui
   repart de zéro donne à son premier message la référence de segments encore
   en vol, et le combiné les fusionne.

5. **GSM 7-bit est non packé par défaut** — un septet par octet, bit de poids
   fort à zéro — et le packé reste disponible par session pour les SMSC qui
   l'exigent. Les budgets ne changent pas : 160 et 153 sont des septets dans
   les deux dispositions ; seul le nombre d'octets diffère (160 contre 140).

6. **En mode packé, un segment non final ne peut pas se fermer sur un compte
   de septets que le récepteur ne retrouverait pas.** Derrière un UDH de six
   octets, 152 septets occupent les mêmes 134 octets que 153 : le récepteur
   divise, lit 153, et trouve le retour à la ligne de bourrage au milieu du
   message. Or 152 est exactement le compte que produit la règle de la paire
   d'échappement. Le remplisseur rend alors son dernier caractère au segment
   suivant (152 → 151, 133 octets, compte exact).

   Le segment **final** ne peut pas être réparé — il n'y a pas de segment
   suivant où pousser un caractère. C'est le cas que TS 23.038 §6.1.2.3.1
   couvre en prescrivant `CR` comme valeur de bourrage précisément pour qu'il
   reste inoffensif ; le réassembleur le retire.

7. **Le réassembleur recalcule le nombre de septets depuis la longueur en
   octets**, au lieu de le lire dans la structure du segment. Un récepteur n'a
   que `sm_length` ; un oracle qui lirait le compte que l'encodeur a mémorisé
   serait aveugle à toute la classe d'erreurs que la division introduit.

## Conséquences

- **Positives :** aucun caractère ne peut être coupé en deux, dans aucun
  encodage. Le compteur de l'éditeur et l'envoi réel ne peuvent pas diverger.
  Le segmenteur ne devine rien du SMSC. La disposition par défaut est celle que
  le parc attend, et la seule où le nombre de septets est exact par
  construction.
- **Négatives / dette assumée :** le mode `sar_*` renonce à 7 septets par
  segment, soit environ 4 % de capacité. Un SMSC dont on saurait qu'il ne
  retraduit pas en UDH pourrait les récupérer ; ce serait une nouvelle ADR et
  un champ de configuration de plus.
- **Négatives / limite connue du mode packé :** un segment final dont le
  dernier caractère est un retour à la ligne authentique, à l'alignement où le
  bourrage est possible, est indiscernable d'un segment bourré et le perd.
  TS 23.038 a la même ambiguïté et dit à l'**émetteur** d'écrire le retour à la
  ligne deux fois. Le cas n'existe pas en non packé, où la longueur en octets
  *est* la longueur en septets. Il est épinglé par un test nommé.
- **Impacts opérationnels :** aucune dépendance ajoutée pour l'algorithme.
  L'UDH provient de `rusmpp` via `smpp-core`, qui valide déjà les invariants
  d'index. `criterion` entre en dépendance de développement pour le banc de
  référence (CA-004-10), sous licence MIT/Apache-2.0 déjà autorisée.
- **Points de réexamen :** si un SMSC rejette des segments `sar_*` de 153
  septets, ou si l'on veut exploiter les 160 ; si le besoin apparaît d'un UDH
  de concaténation 16 bits (IEI `0x08`), que ce jalon n'implémente pas.

## À trancher au jalon 005 — l'interprétation des octets non packés

Le choix packé / non packé n'épuise pas la question. **Deux écoles coexistent
sur la valeur des octets non packés**, et le SMSC décide :

- **valeurs GSM 03.38** — `@` vaut `0x00`, `é` vaut `0x05`, `€` s'écrit
  `0x1B 0x65`. C'est ce que ce jalon implémente ;
- **valeurs Latin-1 / ASCII** — `@` vaut `0x40`, `é` vaut `0xE9`, le SMSC
  transcodant lui-même vers GSM 03.38. C'est ce que Kannel appelle
  `alt-charset`.

Le piège est vicieux : **un texte purement ASCII traverse les deux à
l'identique**. Les tests restent verts, la recette passe, et seuls `@ £ $ €`
et les lettres accentuées se corrompent en production.

Ce n'est **pas** dans le périmètre du jalon 004 et rien n'en est implémenté
ici. C'est une caractéristique de session, au même titre que le packing et le
mode de concaténation, et sa place est dans les **profils de session du jalon
005**, à côté de `Gsm7BitPacking`.

## Références

- Spec §7.5 (tableau des capacités, algorithme de choix d'encodage,
  segmentation) · exigences EF-MSG-03 et EF-MSG-04
- `tasks-todo/step-004.md` — critères CA-004-01 à CA-004-10
- 3GPP TS 23.038 §6.1.2.3.1 (bourrage des bits libres et retour à la ligne
  final), §6.1.2.1.1 (format packé over-the-air), §6.2.1 (table de base et
  table d'extension GSM 03.38)
- `rusmpp-extra` — `Gsm7BitUnpacked` comme encodeur par défaut de
  `SubmitSmMultipartExt`
- [ADR 0001](0001-choix-de-la-pile-smpp.md) — les types de protocole viennent
  de `rusmpp`, réexportés par `smpp-core`
