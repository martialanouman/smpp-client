#!/usr/bin/env bash
#
# Étape 8 de la CI — « migrations » (jalon 000 §5.1, active dès le jalon 002).
#
# Même patron à cliquet que scripts/check-ipc-types.sh : un seul mécanisme
# mental pour les deux étapes différées du jalon 000.
#
#   1. Des migrations existent → on les applique sur une base neuve et on
#                                vérifie que le schéma se construit.
#   2. Aucune migration        → succès, avec un message explicite.
#
# La bascule dépend de la présence de fichiers dans migrations/, pas d'un
# drapeau : dès que le jalon 002 écrit sa première migration, l'étape devient
# réellement vérifiante sans que personne n'ait à modifier ce script.

set -euo pipefail

racine="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$racine"

migrations="migrations"

if [ ! -d "$migrations" ] || [ -z "$(find "$migrations" -name '*.sql' -print -quit 2>/dev/null)" ]; then
  echo "✓ Aucune migration SQL (le schéma arrive au jalon 002)."
  exit 0
fi

if ! command -v sqlx >/dev/null 2>&1; then
  echo >&2
  echo "✗ Des migrations existent mais sqlx-cli est absent." >&2
  echo "  Installez-le : cargo install sqlx-cli --no-default-features --features sqlite" >&2
  exit 1
fi

base="$(mktemp -d)/verification.db"
export DATABASE_URL="sqlite://${base}?mode=rwc"

echo "→ Application des migrations sur une base neuve : $base"
sqlx database create
sqlx migrate run

echo "✓ Migrations appliquées sans erreur sur une base vierge."
