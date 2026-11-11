#!/usr/bin/env bash
# Tous les tests : Rust (cœur, démon) puis règles de l'interface (Node, sans dépendance).
set -euo pipefail
cd "$(dirname "$0")"
cargo test -q
node --test ui/tests/*.test.mjs
