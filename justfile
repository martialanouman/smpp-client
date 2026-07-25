# Tâches standardisées — guide §3.4, CLAUDE.md §5.
#
# ATTENTION — ces recettes et .github/workflows/ci.yml doivent rester
# alignées. La CI n'appelle délibérément PAS `just` : elle reprend les
# commandes verbatim, ce qui évite d'installer un outil de plus sur trois OS.
# La contrepartie est un risque de dérive, sans garde-fou automatique
# satisfaisant. Toute modification ici impose de vérifier ci.yml, et
# réciproquement.
#
# Chaque recette est une suite de commandes simples, sans construction shell :
# elles doivent s'exécuter à l'identique sous sh et sous PowerShell.

# Liste les recettes disponibles.
default:
    @just --list

# --- Formatage --------------------------------------------------------------

# Formate le code Rust et TypeScript.
fmt:
    cargo fmt --all
    pnpm -C ui format

# Vérifie le formatage sans rien modifier (étape 1 de la CI).
fmt-check:
    cargo fmt --all --check
    pnpm -C ui format:check

# --- Qualité ----------------------------------------------------------------

# Lint Rust et TypeScript, plus le typage (étapes 2 et 3 de la CI).
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    pnpm -C ui typecheck
    pnpm -C ui lint

# Vérifie que les types IPC générés sont à jour (étape 4 de la CI).
ipc-check:
    bash scripts/check-ipc-types.sh

# --- Tests ------------------------------------------------------------------

# Tests unitaires, doctests et tests frontend (étapes 5 et 6 de la CI).
test:
    cargo nextest run --workspace
    cargo test --doc --workspace
    pnpm -C ui test --run

# --- Chaîne d'approvisionnement ---------------------------------------------

# Vulnérabilités et licences (étape 7 de la CI).
audit:
    cargo audit
    cargo deny check advisories bans licenses sources

# --- Base de données --------------------------------------------------------

# Applique les migrations (inactif jusqu'au jalon 002).
migrate:
    sqlx migrate run

# Vérifie les migrations sur une base neuve (étape 8 de la CI).
migrate-check:
    bash scripts/check-migrations.sh

# --- Application ------------------------------------------------------------

# Lance l'application en rechargement à chaud.
dev:
    pnpm tauri dev

# Produit les paquets natifs (étape 9 de la CI).
build:
    pnpm tauri build

# --- Raccourci ---------------------------------------------------------------

# Tout ce qui doit être vert avant un commit (CLAUDE.md §5).
check: fmt-check lint test audit
