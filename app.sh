#!/usr/bin/env bash
# Construit l'application macOS Claudius.app (Tauri v2), en release.
#   ./app.sh            → app/target/release/bundle/macos/Claudius.app
#   ./app.sh --open     → construit puis lance l'app
#   ./app.sh --install  → construit puis copie l'app dans /Applications
set -euo pipefail

cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

command -v cargo >/dev/null || { echo "cargo introuvable : installe Rust (https://rustup.rs)." >&2; exit 1; }
if ! cargo tauri --version >/dev/null 2>&1; then
  echo "tauri-cli absent : cargo install tauri-cli --version '^2' --locked" >&2
  exit 1
fi

# 1. Le démon, embarqué dans le bundle (externalBin attend le suffixe de la cible).
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
cargo build --release -q -p claudius-daemon
cp target/release/claudiusd "app/binaries/claudiusd-$TRIPLE"

# 2. L'app.
(cd app && cargo tauri build --bundles app)

APP="app/target/release/bundle/macos/Claudius.app"
echo "Claudius.app : $APP"

case "${1:-}" in
  --open) open "$APP" ;;
  --install) rm -rf /Applications/Claudius.app && cp -R "$APP" /Applications/ && echo "Installée dans /Applications." ;;
esac
