# ADR 0009 — L'alt-charset est une caractéristique de session, et son alphabet est l'intersection

> **Statut :** Accepté
> **Date :** 2026-07-26 · **Jalon :** step-005 · **Décideur :** Martial Anouman

## Contexte

L'[ADR 0008](0008-strategie-de-segmentation.md) a tranché le **packing** des
septets GSM — non packé par défaut — et a explicitement laissé une question
ouverte, dans sa section « À trancher au jalon 005 » :

> **Deux écoles coexistent sur la valeur des octets non packés**, et le SMSC
> décide :
> - **valeurs GSM 03.38** — `@` vaut `0x00`, `é` vaut `0x05`, `€` s'écrit
>   `0x1B 0x65` ;
> - **valeurs Latin-1 / ASCII** — `@` vaut `0x40`, `é` vaut `0xE9`, le SMSC
>   transcodant lui-même vers GSM 03.38. C'est ce que Kannel appelle
>   `alt-charset`.

Trois choses rendent l'arbitrage nécessaire maintenant plutôt que plus tard.

**1. Le piège est silencieux et il est asymétrique.** Se tromper ne produit
aucune erreur : le SMSC répond `ESME_ROK`, le DLR dit `DELIVRD`, et le
destinataire lit du charabia. Pire, **un texte purement ASCII traverse les deux
lectures à l'identique** — `A` vaut `0x41` des deux côtés. Une suite de tests
écrite sur `"hello"`, une recette, un client pilote anglophone : tout passe. La
divergence porte exactement sur `@ £ $ ¥ è é ù ì ò Ç Ø ø Å å Æ æ ß É ¤ ¡ Ä Ö Ñ
Ü § ¿ ä ö ñ ü à` et la table d'extension — c'est-à-dire sur les signes
monétaires et les lettres accentuées, soit tout ce dont un message français,
espagnol ou allemand est fait, et rien de ce dont un message anglais est fait.

**2. Ce n'est pas une propriété du message.** Un même texte doit s'écrire
autrement selon le SMSC en face. C'est donc un réglage de session, au même
titre que `Gsm7BitPacking`, et le jalon 005 est celui qui introduit les profils
de session.

**3. Le placement fait remonter un problème de couches.** `Gsm7BitPacking`
était défini dans `messaging`, qui est **au-dessus** de `smpp-session` dans le
graphe de dépendances. Un profil de session ne peut pas porter un type qui vit
au-dessus de lui.

## Options envisagées

### Sur l'interprétation elle-même

**Option A — n'implémenter que GSM 03.38.** C'est la lecture fidèle au
protocole : `data_coding = 0x00` signifie « alphabet par défaut du MC », et sur
les réseaux GSM ce défaut *est* GSM 03.38.

**Pour :** rien à configurer, rien à se tromper.
**Contre :** rend l'application inutilisable avec tout SMSC configuré à la
Kannel — et ils sont nombreux. L'utilisateur n'aurait aucun recours : le
symptôme est un accent faux chez le destinataire, sans trace côté client.

**Option B — deviner d'après le SMSC.** Détecter à partir du `system_id`, de la
bannière du bind, d'une sonde.

**Pour :** rien à configurer.
**Contre :** rien ne permet de deviner. Le SMSC n'annonce pas son alt-charset,
et une sonde supposerait de lire un message qu'on aurait envoyé — ce qu'un ESME
ne peut pas faire.

**Option C — l'exposer comme réglage de session.**

**Pour :** c'est la seule information dont la source est l'opérateur, et il l'a
(elle est dans le contrat d'interconnexion ou dans la configuration du SMSC).
**Contre :** un réglage de plus, et un réglage qu'on peut mettre de travers.
Atténué par le défaut, qui est la lecture majoritaire.

### Sur l'alphabet accessible sous alt-charset

Une fois `Latin1` choisi, la question suivante est : **quels caractères
restent écrivables ?** Deux ensembles sont en jeu.

- Les **dix caractères de la table d'extension** (`€ { } [ ] ~ \ | ^` et le
  saut de page). Ils existent en ISO-8859-1 comme un octet unique, mais le SMSC
  les développe en paire d'échappement de **deux septets** en sortie.
- Les **dix capitales grecques** de la table de base (`Δ Φ Γ Λ Ω Π Ψ Σ Θ Ξ`),
  qui n'ont aucun point de code ISO-8859-1. Le cas est tranché d'office : il
  n'y a pas d'octet à écrire.

**Option D — accepter la table d'extension, en comptant deux septets au
budget et un octet au corps.** Protocolairement défendable : le plan réserve
bien deux septets, le SMSC en produit deux.

**Pour :** aucune perte de caractères.
**Contre :** le budget d'un segment se compte en septets et le corps se
découpe en octets ; les faire diverger est précisément le mécanisme par lequel
un segment déborde en silence sur la branche de livraison. Le segmenteur du
jalon 004 est construit sur l'égalité « une unité de budget = une entrée du
tampon », et la casser demanderait une représentation parallèle sur le chemin
chaud. Le coût en complexité est réel, le gain porte sur huit caractères
rarement présents dans un SMS.

**Option E — refuser la table d'extension sous alt-charset.**

**Pour :** un caractère vaut exactement un septet **et** un octet ; l'égalité
sur laquelle repose le segmenteur tient sans exception. Aucun risque de
troncature invisible.
**Contre :** `{ } [ ] ~ \ | ^` deviennent inécrivables sur une session
alt-charset. `€` l'était de toute façon — il n'a pas de point de code
ISO-8859-1.

## Décision

**Option C et option E.**

1. **`Gsm7BitCharset` est un champ du profil de session**, à côté de
   `Gsm7BitPacking`, persisté dans `session_profiles`, exposé par l'IPC et
   réglable dans l'écran Sessions. Défaut : `Gsm0338`, la lecture que le parc
   installé attend et celle que le jalon 004 implémentait déjà — aucun profil
   existant ne change de comportement.

2. **`Gsm7BitPacking` descend de `messaging` vers `smpp-core::values`**, et
   `Gsm7BitCharset` l'y rejoint. `smpp-core` est sous les deux couches qui en
   ont besoin : `smpp-session` les porte dans le profil, `messaging` les
   applique à l'encodage. `messaging::encoding` les réexporte, donc l'API du
   jalon 004 est inchangée.

3. **Aucun des deux n'est `#[non_exhaustive]`**, contrairement à tous les
   autres enums de `smpp-core`. Une troisième convention de disposition devrait
   être traitée *partout où des octets sont écrits* ; `#[non_exhaustive]`
   transformerait chacun de ces endroits en bras joker qui continue en silence
   à faire l'ancienne chose. On veut une erreur de compilation à chacun.

4. **Sous `Latin1`, l'alphabet est l'intersection GSM 03.38 ∩ ISO-8859-1.**
   Un caractère est écrivable s'il est dans la table de **base** GSM (donc
   transcodable par le SMSC) *et* a un point de code ≤ `0xFF`. La table
   d'extension et les capitales grecques sortent. Tout caractère restant coûte
   exactement un septet et un octet.

5. **Un texte que l'alt-charset ne peut pas écrire n'est pas déformé.** En
   choix automatique il élargit vers UCS2 — `« Total : 10€ »` part en UCS2 sur
   une session alt-charset et en GSM 7-bit ailleurs. En encodage forcé il
   produit `UnrepresentableCharacter`, exactement comme CA-004-04 l'exige des
   autres encodages.

6. **Le budget de segment ne bouge pas.** 160 et 153 restent des **septets**
   dans les deux lectures. L'alt-charset change ce que les octets veulent dire,
   pas combien il en tient.

7. **`Latin1` est incompatible avec `Packed`, et le profil le refuse.** Les
   octets Latin-1 utilisent les huit bits — `é` vaut `0xE9` — et le packing
   jette le bit de poids fort de chacun. Le résultat n'est pas « un peu faux »,
   il est irrécupérable. Le couple est rejeté à la construction du profil, avec
   `ProfileRejection::Contradictory`.

8. **Le segment retient la lecture sous laquelle il a été écrit.** Le
   réassemblage lit `Segment::gsm_charset` plutôt que de re-déduire : les deux
   lectures coïncidant sur l'ASCII, un segment décodé sous la mauvaise a l'air
   presque juste.

## Conséquences

- **Positives :** l'application fonctionne avec les deux familles de SMSC, sans
  rien deviner. La divergence est couverte par des tests écrits **sur les
  caractères qui divergent** — `@ £ $ € é` et les accents — et jamais sur de
  l'ASCII : un test ASCII passe sous les deux lectures et ne prouve rien. Le
  fait que l'ASCII soit identique est lui-même épinglé par un test nommé, pour
  que le piège reste visible dans la suite.
- **Négatives / dette assumée :** huit caractères de la table d'extension sont
  inaccessibles sur une session alt-charset. Les recours sont documentés :
  laisser la session en `Gsm0338`, ou forcer UCS2 pour ce message. L'option D
  reste ouverte si un besoin réel apparaît — ce serait une ADR qui supersède
  celle-ci et une représentation parallèle dans le segmenteur.
- **Négatives / limite connue :** le réglage reste déclaratif. Un opérateur qui
  le met de travers obtient exactement le symptôme que cette ADR décrit, et
  rien ne le lui dira. La seule atténuation possible serait un aller-retour
  réel avec un numéro de test, ce qui sort du périmètre.
- **Impacts opérationnels :** une migration ajoute `gsm7_packing` et
  `gsm7_charset` à `session_profiles`, avec les valeurs par défaut
  `'unpacked'` et `'gsm0338'` — les profils existants gardent le comportement
  du jalon 004.
- **Points de réexamen :** le **jalon 017** (simulateur SMSC avec injection de
  fautes) est le premier endroit où un aller-retour complet pourra être joué
  sous les deux lectures. Si un SMSC réel exige la table d'extension sous
  alt-charset, l'option D revient sur la table.

## Références

- [ADR 0008](0008-strategie-de-segmentation.md) — section « À trancher au
  jalon 005 », qui pose la question à laquelle celle-ci répond
- [ADR 0001](0001-choix-de-la-pile-smpp.md) — les types de protocole viennent
  de `rusmpp`, réexportés par `smpp-core`
- Spec §7.5 (tableau des capacités, algorithme de choix d'encodage), §8.2
  (profil de session)
- 3GPP TS 23.038 §6.2.1 (table de base et table d'extension GSM 03.38)
- Kannel, `smsbox.conf` — directive `alt-charset`
- `tasks-todo/step-005.md`
