#!/usr/bin/env bash
#
# Étape 4 de la CI — « types IPC générés » (jalon 000 §5.1).
#
# Le générateur tauri-specta n'arrive qu'au jalon 001. Le piège classique
# serait d'écrire ici une étape qui ne fait rien : elle passerait toujours, et
# personne ne se souviendrait de la « dé-permissiver » le jour venu.
#
# Ce script vérifie donc sa propre précondition et bascule tout seul :
#
#   1. Le générateur existe  → on l'exécute et on compare. Comportement
#                              nominal, définitif.
#   2. Il n'existe pas, mais des types générés sont présents
#                            → ÉCHEC DUR. C'est le cliquet : un fichier
#                              généré ne peut pas apparaître sans générateur.
#   3. Ni l'un ni l'autre    → succès, avec un message explicite.
#
# La bascule dépend de la PRÉSENCE DU FICHIER GÉNÉRATEUR, pas d'une variable
# d'environnement ni d'un drapeau à retirer : il n'y a rien à se rappeler.

set -euo pipefail

racine="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$racine"

generateur="src-tauri/src/bin/gen_ipc.rs"
sortie="ui/src/ipc"

if [ -f "$generateur" ]; then
  echo "→ Générateur détecté ($generateur) : régénération des types IPC."
  cargo run --quiet --package shinobismpp --bin gen_ipc

  if ! git diff --exit-code -- "$sortie"; then
    echo >&2
    echo "✗ Les types IPC générés diffèrent de ceux commités." >&2
    echo "  Lancez 'just ipc-check' puis commitez le résultat." >&2
    exit 1
  fi

  echo "✓ Types IPC à jour."
  exit 0
fi

# Le générateur n'est pas branché. Aucun fichier généré ne doit exister.
if [ -d "$sortie" ]; then
  orphelins="$(grep -rl -- '@generated' "$sortie" 2>/dev/null || true)"
  if [ -n "$orphelins" ]; then
    echo >&2
    echo "✗ Des types générés sont présents alors que le générateur est absent :" >&2
    echo "$orphelins" | sed 's/^/    /' >&2
    echo >&2
    echo "  Soit ces fichiers ont été écrits à la main — ce qu'interdit" >&2
    echo "  ui/src/ipc/README.md — soit le générateur a été supprimé." >&2
    exit 1
  fi
fi

echo "✓ Générateur de types IPC non encore branché (arrive au jalon 001)."
echo "  Aucun type généré présent : état cohérent."
