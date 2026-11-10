# DRIFTy

Tableau de bord local de l'usage de Claude Code : tokens, cache, coût, et
**évaluation de chaque session** (où est passé le temps, ce qui est
optimisable, ce que ça rapporterait).

## Lancer

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo run --release -p claudius-daemon        # http://127.0.0.1:4317
```

- `CLAUDIUS_DB` : chemin de la base (défaut `~/.claudius/claudius.db`).
- `CLAUDIUS_ADDR` : adresse d'écoute (défaut `127.0.0.1:4317`).
- `claudiusd --sync-only` : synchronise puis sort (affiche le rapport JSON).
- `./run.sh` : compile si besoin, démarre et ouvre l'interface.

**Rien ne se synchronise tout seul** : ni au lancement, ni en tâche de fond.
Seul le bouton « Synchroniser » (ou `--sync-only`) relit les transcripts.

## Application macOS

```bash
./app.sh             # → app/target/release/bundle/macos/DRIFTy.app
./app.sh --open      # construit puis lance
./app.sh --install   # construit puis copie dans /Applications
```

Prérequis : `cargo install tauri-cli --version '^2' --locked`. Le projet Tauri
vit dans `app/`, hors du workspace (le `cargo test` racine ne le voit pas).

- Au lancement, l'app réutilise le démon qui répond sur `CLAUDIUS_ADDR`
  (défaut `127.0.0.1:4317`) ; sinon elle lance le `claudiusd` du bundle, avec
  le `PATH` du shell de connexion, et l'arrête en quittant (seulement
  celui-là). Journal : `~/Library/Logs/DRIFTy/claudiusd.log`.
- Notifications natives par le plugin Tauri ; liens externes dans le
  navigateur du système ; taille et position de la fenêtre mémorisées.
- Essai sans toucher au démon courant :
  `CLAUDIUS_ADDR=127.0.0.1:4398 CLAUDIUS_DB=/tmp/essai.db app/target/release/bundle/macos/DRIFTy.app/Contents/MacOS/DRIFTy`.

## Architecture

```
ui/ (HTML/CSS/JS sans build)  ⇄  HTTP/JSON  ⇄  claudiusd (Rust)  ⇄  SQLite
                                                      ↑
                       transcripts ~/.claude/projects/**/*.jsonl (perso, pro)
```

L'interface ne connaît que l'API HTTP/JSON : le démon peut être réécrit (en
Arch-C un jour) sans la toucher.

| Crate | Rôle |
|---|---|
| `crates/core/src/raw.rs` | Format brut d'une ligne de transcript |
| `crates/core/src/extract.rs` | Lignes → enregistrements (dédoublonnage des messages) |
| `crates/core/src/ingest.rs` | Ingestion incrémentale, parallèle (rayon) |
| `crates/core/src/db.rs` | Schéma SQLite, comptes, seul point d'écriture |
| `crates/core/src/query.rs` | Vue d'ensemble, listes de sessions |
| `crates/core/src/digest.rs` | Évaluation étage 1 : causes de lenteur, gains |
| `crates/daemon` | `claudiusd` : API + ingestion toutes les 60 s + UI embarquée |

### API

| Méthode | Chemin | |
|---|---|---|
| GET | `/api/overview?days=30&slot=` | totaux, séries par jour, projets, modèles |
| GET | `/api/sessions?slot=&project=&limit=` | liste des sessions |
| GET | `/api/sessions/{id}` | en-tête (cost-state officiel) |
| GET | `/api/sessions/{id}/digest` | causes, verdict optimisable, gains |
| GET | `/api/sessions/{id}/timeline` | chronologie (3 000 derniers événements) |
| GET/PUT | `/api/accounts` | les deux comptes (`perso`, `pro`) |
| POST | `/api/ingest` · GET `/api/status` | synchronisation |

## Comptes : deux, pas plus

Un emplacement `perso` et un emplacement `pro`, chacun pointant sur son
dossier de configuration Claude (`CLAUDE_CONFIG_DIR`). La limite est dans le
schéma (`CHECK (slot IN ('perso','pro'))`), pas seulement dans l'interface.
Au premier lancement, `~/.claude` devient le compte perso.

## Usage d'abonnement (5 h, semaine)

Source officielle : Claude Code envoie à son script de statusline un objet
`rate_limits` (`five_hour` / `seven_day` : `used_percentage`, `resets_at`).
Le bouton « Activer » d’une carte d’usage (le relais) remplace la commande de
statusline du compte par `claudiusd statusline --slot <compte> -- <commande
d'origine>` : le relais écrit le dernier relevé dans
`~/.claudius/usage/<compte>.json`, puis rejoue la statusline d'origine avec la
même entrée. `settings.json` est sauvegardé en `settings.json.claudius-backup` ;
`DELETE /api/accounts/{slot}/relay` remet la commande d'origine. Aucun appel à
un service, aucun jeton lu. Les jauges sont un instantané de la dernière
réponse de Claude dans ce compte.

## Projets

Chaque conversation est rattachée au dépôt git de son dossier de départ ;
les worktrees (Conductor compris) tombent dans le même projet, en sous-groupe.
Un worktree supprimé rejoint le dépôt de ses voisins s'il n'y en a qu'un.
Nom, logo (emoji ou image), couleur et compte se règlent par projet ;
« Nouvelle » ouvre Terminal à la racine avec le compte du projet, « Reprendre »
rouvre une conversation avec le compte qui détient son transcript.

## Mesures : ce qui est exact, ce qui est estimé

- **Tokens** : sommés par message de l'API, dédoublonnés par `message.id` (un
  message est écrit sur plusieurs lignes, une par bloc, avec le même `usage` ;
  la somme naïve compte 2 à 3 fois).
- **Coût** : uniquement le `cost-state` écrit par Claude Code (équivalent API).
  Aucun tarif n'est codé en dur.
- **Écart connu** : les transcripts couvrent 85 à 99 % des tokens du
  `cost-state` (mesuré le 2026-11-09) ; certains appels (compaction,
  nouvelles tentatives) ne laissent pas de ligne.
- **Temps** : modèle et outils viennent du `cost-state` quand il existe,
  sinon de la chronologie. Un trou de plus de 30 min est une pause.
- **Gains** : chiffrés seulement quand c'est mesurable sur la session
  (échecs, répétitions à l'identique, sorties volumineuses relues). Les autres
  causes disent « non chiffrable », jamais un chiffre inventé.
  Approximation déclarée : 1 token ≈ 4 octets.

## Feuille de route

- **V1** (ici) : ingestion, tableau de bord, digest étage 1 (zéro token).
- **V2** : bouton « Évaluer avec un agent » — un agent reçoit le digest (pas
  le transcript), rend causes et correctifs applicables (CLAUDE.md, mémoire,
  skills) ; suivi avant/après des conseils appliqués. Replay.
- **V3** : orchestration d'agents en parallèle (worktrees), routage
  perso/pro par agent, budgets, coquille Tauri.
