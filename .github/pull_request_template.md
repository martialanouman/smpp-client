## Ce que fait cette PR

<!-- Une ou deux phrases. Le « pourquoi » compte davantage que le « quoi » :
     le diff dit déjà ce qui change. -->

**Jalon / exigence :** <!-- step-NNN, EF-XXX-NN, ou « hors jalon » -->

## Comment cela a été vérifié

<!-- Des preuves, pas des intentions. Commandes lancées, sortie observée,
     capture si le changement est visible. « Ça a l'air de marcher » n'est pas
     une vérification. -->

- [ ] `just check` vert en local (`fmt-check`, `lint`, `test`, `audit`)
- [ ] Comportement vérifié à l'exécution, pas seulement compilé
- [ ] Tests ajoutés ou mis à jour pour le comportement modifié
- [ ] Correction de bug : test de non-régression écrit **d'abord**, vu échouer

## Points d'attention

- [ ] Périmètre respecté — rien qui appartienne à un jalon ultérieur
- [ ] Aucun secret dans le code, les journaux, les tests ou les exports
- [ ] Frontières de dépendance tenues — aucune crate métier n'importe `tauri`,
      aucun import remontant
- [ ] Pas d'`unwrap`/`expect`/`panic!` hors tests, pas de blocage en async
- [ ] Contrat IPC modifié → types régénérés et commités
- [ ] Décision structurante → ADR ajoutée dans `docs/adr/`
- [ ] CHANGELOG mis à jour si le changement est visible par l'utilisateur

## Ce que je n'ai pas fait

<!-- Ce qui a été délibérément laissé de côté, et pourquoi. Une limite connue
     et énoncée vaut mieux qu'une limite découverte en production. -->
