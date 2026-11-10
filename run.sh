#!/usr/bin/env bash
# Lance DRIFTy : compile si besoin, démarre le démon, ouvre l'interface.
#   ./run.sh              → http://drifty.localhost:4317
#   ./run.sh --no-open    → sans ouvrir le navigateur
#   DRIFTY_ADDR=127.0.0.1:5000 ./run.sh
set -euo pipefail

cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v cargo >/dev/null; then
  echo "cargo introuvable : installe Rust (https://rustup.rs)." >&2
  exit 1
fi

ADDR="${DRIFTY_ADDR:-127.0.0.1:4317}"
export DRIFTY_ADDR="$ADDR"
URL="http://$ADDR"
# Interface sous drifty.localhost : même site que les relais du navigateur intégré.
UI="http://drifty.localhost:${ADDR##*:}"

if curl -sf "$URL/api/status" >/dev/null 2>&1; then
  echo "DRIFTy tourne déjà sur $UI"
  [[ "${1:-}" != "--no-open" ]] && open "$UI"
  exit 0
fi

cargo build --release -q -p drifty-daemon

./target/release/driftyd &
PID=$!
trap 'kill "$PID" 2>/dev/null' EXIT INT TERM

for _ in $(seq 50); do
  curl -sf "$URL/api/status" >/dev/null 2>&1 && break
  kill -0 "$PID" 2>/dev/null || { echo "Le démon s'est arrêté au démarrage." >&2; exit 1; }
  sleep 0.2
done

[[ "${1:-}" != "--no-open" ]] && open "$UI"
echo "DRIFTy : $UI  (Ctrl-C pour arrêter)"
wait "$PID"
