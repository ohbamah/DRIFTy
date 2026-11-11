#!/usr/bin/env bash
# Construit l'application macOS DRIFTy.app (Tauri v2), en release.
#   ./app.sh            → app/target/release/bundle/macos/DRIFTy.app
#   ./app.sh --open     → construit puis lance l'app
#   ./app.sh --install  → construit puis copie l'app dans /Applications
#                         (refuse si un agent travaille : --install --force pour passer outre)
set -euo pipefail

cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

# Réinstaller relance l'app : un agent en plein tour serait coupé (il reprend ensuite, mais
# son tour est perdu). On refuse AVANT de construire, pour ne pas attendre pour rien.
if [[ "${1:-}" == --install && "${2:-}" != --force ]]; then
  BUSY="$(curl -s -m 3 -H 'Origin: http://drifty.localhost:4317' http://127.0.0.1:4317/api/chats 2>/dev/null \
    | python3 -c 'import json,sys
try: chats = json.load(sys.stdin)
except Exception: chats = []
for c in chats:
    if c.get("status") == "working": print("  -", c.get("name", "?").splitlines()[0][:70])' 2>/dev/null || true)"
  if [[ -n "$BUSY" ]]; then
    echo "Des agents travaillent encore :" >&2
    echo "$BUSY" >&2
    echo "Attends qu'ils aient fini, ou relance avec : ./app.sh --install --force" >&2
    exit 1
  fi
fi

command -v cargo >/dev/null || { echo "cargo introuvable : installe Rust (https://rustup.rs)." >&2; exit 1; }
if ! cargo tauri --version >/dev/null 2>&1; then
  echo "tauri-cli absent : cargo install tauri-cli --version '^2' --locked" >&2
  exit 1
fi

# 1. Le démon, embarqué dans le bundle (externalBin attend le suffixe de la cible).
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
cargo build --release -q -p drifty-daemon
cp target/release/driftyd "app/binaries/driftyd-$TRIPLE"

# 2. L'app.
(cd app && cargo tauri build --bundles app)

APP="app/target/release/bundle/macos/DRIFTy.app"
echo "DRIFTy.app : $APP"

case "${1:-}" in
  --open) open "$APP" ;;
  --install)
    # Remplacer l'app pendant qu'elle tourne peut la faire tomber : on la ferme proprement d'abord
    # (elle arrête son démon), puis on la relance si elle était ouverte.
    # Passage depuis une version qui ne tenait pas encore la liste des discussions (voir
    # crates/daemon/src/persist.rs) : on la capture une fois, pour qu'elles soient rétablies.
    SAVED="$HOME/.drifty/drifty.chats.json"
    if [[ ! -f "$SAVED" ]]; then
      curl -s -m 3 -H 'Origin: http://drifty.localhost:4317' http://127.0.0.1:4317/api/chats 2>/dev/null \
        | python3 -c '
import json, sqlite3, sys, os
try: chats = json.load(sys.stdin)
except Exception: sys.exit(0)
db = sqlite3.connect(os.path.expanduser("~/.drifty/drifty.db"))
dirs = dict(db.execute("SELECT slot, config_dir FROM accounts"))
out = [{"id": c["id"], "repo_id": c["repo_id"], "name": c["name"], "cwd": c["cwd"], "slot": c.get("slot"),
        "config_dir": dirs.get(c.get("slot")), "bypass": c["bypass"], "worktree": c.get("worktree"),
        "model": c.get("model"), "session_id": c["session_id"], "env": [], "tokens": c.get("tokens_total") or 0,
        "cost_usd": c.get("cost_usd") or 0, "interrupted": c.get("status") == "working"}
       for c in chats if c.get("session_id")]
if out:
    json.dump(out, open(sys.argv[1], "w"), indent=2)
    print(f"{len(out)} discussion(s) capturée(s) pour être rétablies.")
' "$SAVED" || true
    fi
    WAS_RUNNING=0
    if pgrep -x DRIFTy >/dev/null; then
      WAS_RUNNING=1
      osascript -e 'tell application "DRIFTy" to quit' >/dev/null 2>&1 || true
      for _ in $(seq 50); do pgrep -x DRIFTy >/dev/null || break; sleep 0.2; done
    fi
    rm -rf /Applications/DRIFTy.app && cp -R "$APP" /Applications/ && echo "Installée dans /Applications."
    [[ $WAS_RUNNING == 1 ]] && open /Applications/DRIFTy.app && echo "Relancée."
    ;;
esac
