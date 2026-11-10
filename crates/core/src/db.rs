//! Schéma SQLite et écriture des lots ingérés. Seul point d'écriture de la base.

use crate::extract::FileBatch;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Monter ce numéro force une ré-ingestion complète (les données dérivées
/// des transcripts sont jetées ; comptes, projets et réglages restent).
const SCHEMA_VERSION: i64 = 4;
const DERIVED_TABLES: [&str; 8] = ["files", "sessions", "messages", "tool_calls", "turns", "cwd_map", "session_waste", "texts"];

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

-- Deux comptes, pas un de plus : un compte perso et un compte pro.
CREATE TABLE IF NOT EXISTS accounts (
    slot        TEXT PRIMARY KEY CHECK (slot IN ('perso', 'pro')),
    label       TEXT NOT NULL,
    config_dir  TEXT NOT NULL UNIQUE,
    enabled     INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS files (
    path    TEXT PRIMARY KEY,
    slot    TEXT NOT NULL,
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    offset  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id            TEXT PRIMARY KEY,
    slot          TEXT,
    project_dir   TEXT,
    cwd           TEXT,
    file_path     TEXT,
    branch        TEXT,
    version       TEXT,
    title         TEXT,
    custom_title  TEXT,
    account_uuid  TEXT,
    org_uuid      TEXT,
    first_ts      INTEGER,
    last_ts       INTEGER,
    cost_usd      REAL,
    api_ms        REAL,
    tool_ms       REAL,
    total_ms      REAL,
    lines_added   INTEGER,
    lines_removed INTEGER,
    model_usage   TEXT
);
CREATE INDEX IF NOT EXISTS sessions_last ON sessions(last_ts);

CREATE TABLE IF NOT EXISTS messages (
    id             TEXT PRIMARY KEY,
    session_id     TEXT NOT NULL,
    agent_id       TEXT NOT NULL,
    ts             INTEGER NOT NULL,
    model          TEXT NOT NULL,
    input          INTEGER NOT NULL,
    cache_write    INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    cache_read     INTEGER NOT NULL,
    output         INTEGER NOT NULL,
    thinking       INTEGER NOT NULL,
    stop_reason    TEXT
);
CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, ts);
CREATE INDEX IF NOT EXISTS messages_ts ON messages(ts);

CREATE TABLE IF NOT EXISTS tool_calls (
    id            TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL,
    agent_id      TEXT NOT NULL,
    message_id    TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    name          TEXT NOT NULL,
    target        TEXT,
    input_hash    INTEGER NOT NULL,
    input_bytes   INTEGER NOT NULL,
    result_ts     INTEGER,
    is_error      INTEGER,
    result_bytes  INTEGER,
    result_hash   INTEGER,
    error_preview TEXT
);
CREATE INDEX IF NOT EXISTS tool_calls_session ON tool_calls(session_id, ts);

CREATE TABLE IF NOT EXISTS turns (
    uuid       TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_id   TEXT NOT NULL,
    ts         INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    preview    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS turns_session ON turns(session_id, ts);

-- Un projet = un dépôt git (worktrees compris), ou un dossier hors git.
CREATE TABLE IF NOT EXISTS repos (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    key      TEXT NOT NULL UNIQUE,
    root     TEXT NOT NULL,
    name     TEXT NOT NULL,
    icon     TEXT,
    color    TEXT,
    slot     TEXT CHECK (slot IS NULL OR slot IN ('perso', 'pro')),
    hidden   INTEGER NOT NULL DEFAULT 0
);

-- Où tombe chaque dossier de travail : quel projet, quel worktree, quel sous-dossier.
CREATE TABLE IF NOT EXISTS cwd_map (
    cwd       TEXT PRIMARY KEY,
    repo_id   INTEGER NOT NULL,
    worktree  TEXT NOT NULL,
    rel       TEXT NOT NULL,
    present   INTEGER NOT NULL
);

-- Gaspillage mesurable par session (cache du digest, recalculé si la session bouge).
CREATE TABLE IF NOT EXISTS session_waste (
    session_id     TEXT PRIMARY KEY,
    last_ts        INTEGER NOT NULL,
    context_tokens INTEGER NOT NULL,
    failed_tokens  INTEGER NOT NULL,
    repeat_tokens  INTEGER NOT NULL,
    big_tokens     INTEGER NOT NULL,
    waste_ms       INTEGER NOT NULL
);

-- Ce que DRIFTy dépense lui-même (évaluations par agent, etc.).
CREATE TABLE IF NOT EXISTS drifty_spend (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            INTEGER NOT NULL,
    kind          TEXT NOT NULL,
    session_id    TEXT,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd      REAL
);

-- Règles appliquées depuis « Optimiser » (pour mesurer leur effet ensuite).
CREATE TABLE IF NOT EXISTS fixes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         INTEGER NOT NULL,
    repo_id    INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    cause      TEXT NOT NULL,
    rules      TEXT NOT NULL,
    path       TEXT NOT NULL
);

-- Évaluations par agent (rapport conservé ; leur coût va aussi dans drifty_spend).
CREATE TABLE IF NOT EXISTS evaluations (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT NOT NULL,
    ts            INTEGER NOT NULL,
    report        TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd      REAL
);

-- Messages programmés : envoyés par le démon à l'heure dite.
CREATE TABLE IF NOT EXISTS scheduled (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms       INTEGER NOT NULL,
    chat_id     TEXT,
    session_id  TEXT,
    text        TEXT NOT NULL,
    model       TEXT,
    status      TEXT NOT NULL CHECK (status IN ('pending', 'sent', 'failed', 'cancelled')),
    created     INTEGER NOT NULL,
    sent_at     INTEGER,
    result_chat TEXT,
    error       TEXT
);

-- Recherche plein texte : messages de l'utilisateur et réponses de l'agent (pas les sorties d'outil).
CREATE VIRTUAL TABLE IF NOT EXISTS texts USING fts5(session_id UNINDEXED, ts UNINDEXED, role UNINDEXED, body, tokenize = 'unicode61 remove_diacritics 2');

-- Prompts sauvegardés : globaux (repo_id NULL) ou propres à un projet.
CREATE TABLE IF NOT EXISTS prompts (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    title   TEXT NOT NULL,
    body    TEXT NOT NULL,
    repo_id INTEGER,
    created INTEGER NOT NULL,
    used    INTEGER NOT NULL DEFAULT 0
);

-- Conversations archivées (propre à DRIFTy, non dérivé : survit aux ré-analyses).
CREATE TABLE IF NOT EXISTS archived (
    session_id TEXT PRIMARY KEY,
    at         INTEGER NOT NULL
);

-- Worktrees archivés : dossier retiré (branche gardée) ou laissé en place.
CREATE TABLE IF NOT EXISTS archived_worktrees (
    repo_id  INTEGER NOT NULL,
    worktree TEXT NOT NULL,
    path     TEXT NOT NULL,
    branch   TEXT NOT NULL,
    removed  INTEGER NOT NULL,
    at       INTEGER NOT NULL,
    PRIMARY KEY (repo_id, worktree)
);

CREATE TABLE IF NOT EXISTS settings (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
"#;

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < SCHEMA_VERSION {
        for t in DERIVED_TABLES {
            conn.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
        }
    }
    // Ancien nom de la table des dépenses de l'app (Claudius).
    let legacy: bool = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'claudius_spend'", [], |r| r.get::<_, i64>(0))? > 0;
    if legacy {
        conn.execute_batch("ALTER TABLE claudius_spend RENAME TO drifty_spend;")?;
    }
    conn.execute_batch(SCHEMA)?;
    add_column(&conn, "repos", "chat_mode", "TEXT NOT NULL DEFAULT 'worktree' CHECK (chat_mode IN ('worktree', 'project'))")?;
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    conn.execute(
        "INSERT OR IGNORE INTO settings(k, v) VALUES ('installed_at', ?1)",
        params![std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64).to_string()],
    )?;
    Ok(conn)
}

/// Ajoute une colonne à une table existante si elle manque (base créée par une version antérieure).
fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let has = st.query_map([], |r| r.get::<_, String>(1))?.filter_map(|c| c.ok()).any(|c| c == column);
    if !has {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}

pub fn set_setting(conn: &Connection, k: &str, v: &str) -> Result<()> {
    conn.execute("INSERT INTO settings(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v", params![k, v])?;
    Ok(())
}

pub fn setting(conn: &Connection, k: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT v FROM settings WHERE k = ?1", params![k], |r| r.get(0)).optional()?)
}

// ---------------------------------------------------------------- comptes

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub slot: String,
    pub label: String,
    pub config_dir: String,
    pub enabled: bool,
}

pub const SLOTS: [&str; 2] = ["perso", "pro"];

pub fn accounts(conn: &Connection) -> Result<Vec<Account>> {
    let mut st = conn.prepare("SELECT slot, label, config_dir, enabled FROM accounts ORDER BY slot = 'pro'")?;
    let rows = st.query_map([], |r| {
        Ok(Account { slot: r.get(0)?, label: r.get(1)?, config_dir: r.get(2)?, enabled: r.get::<_, i64>(3)? != 0 })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Enregistre un compte. Refuse tout emplacement autre que `perso` / `pro`
/// et deux emplacements sur le même dossier de configuration.
pub fn set_account(conn: &Connection, a: &Account) -> Result<()> {
    let mut a = a.clone();
    if let Some(rest) = a.config_dir.strip_prefix("~/") {
        a.config_dir = crate::home().join(rest).to_string_lossy().into_owned();
    }
    let a = &a;
    if !Path::new(&a.config_dir).is_dir() {
        anyhow::bail!("The config dir `{}` cannot be used because it does not exist.", a.config_dir);
    }
    if !SLOTS.contains(&a.slot.as_str()) {
        anyhow::bail!("The account slot `{}` cannot be used because only `perso` and `pro` exist.", a.slot);
    }
    let other: Option<String> = conn
        .query_row("SELECT slot FROM accounts WHERE config_dir = ?1 AND slot <> ?2", params![a.config_dir, a.slot], |r| r.get(0))
        .optional()?;
    if let Some(o) = other {
        anyhow::bail!("The config dir `{}` cannot be used for `{}` because the `{}` account already uses it.", a.config_dir, a.slot, o);
    }
    conn.execute(
        "INSERT INTO accounts(slot, label, config_dir, enabled) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(slot) DO UPDATE SET label = excluded.label, config_dir = excluded.config_dir, enabled = excluded.enabled",
        params![a.slot, a.label, a.config_dir, a.enabled as i64],
    )?;
    Ok(())
}

/// Le compte utilisé quand un projet n'en choisit pas : celui de `~/.claude`
/// (le dossier du `claude` du terminal), sinon `perso`, sinon le premier configuré.
pub fn default_slot(conn: &Connection) -> Result<Option<String>> {
    let accs = accounts(conn)?;
    let home_dir = crate::home().join(".claude");
    let pick = accs
        .iter()
        .find(|a| Path::new(&a.config_dir) == home_dir)
        .or_else(|| accs.iter().find(|a| a.slot == "perso"))
        .or_else(|| accs.first());
    Ok(pick.map(|a| a.slot.clone()))
}

/// Retire un compte. Les projets qui l'utilisaient n'ont plus de compte ;
/// les conversations déjà importées restent (elles portent leur compte d'origine).
pub fn delete_account(conn: &Connection, slot: &str) -> Result<()> {
    if !SLOTS.contains(&slot) {
        anyhow::bail!("The account slot `{slot}` cannot be removed because only `perso` and `pro` exist.");
    }
    conn.execute("UPDATE repos SET slot = NULL WHERE slot = ?1", params![slot])?;
    conn.execute("DELETE FROM accounts WHERE slot = ?1", params![slot])?;
    Ok(())
}

/// Au premier lancement : `~/.claude` devient le compte `perso`.
pub fn ensure_default_account(conn: &Connection, home: &Path) -> Result<()> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
    if n == 0 {
        set_account(
            conn,
            &Account {
                slot: "perso".into(),
                label: "Perso".into(),
                config_dir: home.join(".claude").to_string_lossy().into_owned(),
                enabled: true,
            },
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------- fichiers

pub struct FileState {
    pub size: u64,
    pub mtime: i64,
    pub offset: u64,
}

pub fn file_states(conn: &Connection) -> Result<std::collections::HashMap<String, FileState>> {
    let mut st = conn.prepare("SELECT path, size, mtime, offset FROM files")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, FileState { size: r.get::<_, i64>(1)? as u64, mtime: r.get(2)?, offset: r.get::<_, i64>(3)? as u64 }))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub struct Ingested {
    pub path: String,
    pub slot: String,
    pub project_dir: String,
    pub size: u64,
    pub mtime: i64,
    pub offset: u64,
    /// Transcript principal de la session (pas celui d'un sous-agent).
    pub is_main: bool,
    pub batch: FileBatch,
}

/// Écrit tous les lots dans UNE transaction. Les appels d'outil passent avant
/// leurs résultats, quel que soit le fichier d'origine.
pub fn write(conn: &mut Connection, files: &[Ingested]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut sess = tx.prepare(
            "INSERT INTO sessions(id, slot, project_dir, cwd, branch, version, title, account_uuid, org_uuid, first_ts, last_ts, file_path, custom_title)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?13, ?7), ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
               cwd = COALESCE(cwd, excluded.cwd),
               file_path = COALESCE(excluded.file_path, file_path),
               branch = COALESCE(excluded.branch, branch),
               version = COALESCE(excluded.version, version),
               title = COALESCE(excluded.custom_title, custom_title, excluded.title, title),
               custom_title = COALESCE(excluded.custom_title, custom_title),
               account_uuid = COALESCE(excluded.account_uuid, account_uuid),
               org_uuid = COALESCE(excluded.org_uuid, org_uuid),
               first_ts = MIN(COALESCE(first_ts, excluded.first_ts), COALESCE(excluded.first_ts, first_ts)),
               last_ts = MAX(COALESCE(last_ts, excluded.last_ts), COALESCE(excluded.last_ts, last_ts))",
        )?;
        let mut cost = tx.prepare(
            "UPDATE sessions SET cost_usd = ?2, api_ms = ?3, tool_ms = ?4, total_ms = ?5,
               lines_added = ?6, lines_removed = ?7, model_usage = ?8
             WHERE id = ?1 AND (cost_usd IS NULL OR cost_usd <= ?2)",
        )?;
        let mut msg = tx.prepare(
            "INSERT OR IGNORE INTO messages VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        let mut tool = tx.prepare(
            "INSERT OR IGNORE INTO tool_calls(id, session_id, agent_id, message_id, ts, name, target, input_hash, input_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut result = tx.prepare(
            "UPDATE tool_calls SET result_ts = ?2, is_error = ?3, result_bytes = ?4, result_hash = ?5, error_preview = ?6
             WHERE id = ?1",
        )?;
        let mut turn = tx.prepare("INSERT OR IGNORE INTO turns VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
        let mut text = tx.prepare("INSERT INTO texts(session_id, ts, role, body) VALUES (?1, ?2, ?3, ?4)")?;
        let mut file = tx.prepare(
            "INSERT INTO files(path, slot, size, mtime, offset) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET slot = excluded.slot, size = excluded.size, mtime = excluded.mtime, offset = excluded.offset",
        )?;

        for f in files {
            for (id, s) in &f.batch.sessions {
                sess.execute(params![
                    id, f.slot, f.project_dir, s.cwd, s.branch, s.version, s.title, s.account, s.org, s.first_ts, s.last_ts,
                    f.is_main.then_some(&f.path), s.custom_title
                ])?;
                if let Some(c) = &s.cost {
                    cost.execute(params![
                        id,
                        c.cost_usd,
                        c.api_ms,
                        c.tool_ms,
                        c.total_ms,
                        c.lines_added.map(|x| x as i64),
                        c.lines_removed.map(|x| x as i64),
                        c.model_usage
                    ])?;
                }
            }
            for m in &f.batch.messages {
                msg.execute(params![
                    m.id,
                    m.session_id,
                    m.agent_id,
                    m.ts,
                    m.model,
                    m.input as i64,
                    m.cache_write as i64,
                    m.cache_write_5m as i64,
                    m.cache_write_1h as i64,
                    m.cache_read as i64,
                    m.output as i64,
                    m.thinking as i64,
                    m.stop_reason
                ])?;
            }
            for t in &f.batch.tool_uses {
                tool.execute(params![t.id, t.session_id, t.agent_id, t.message_id, t.ts, t.name, t.target, t.input_hash, t.input_bytes])?;
            }
            for (sid, ts, role, body) in &f.batch.texts {
                text.execute(params![sid, ts, role, body])?;
            }
            for t in &f.batch.turns {
                turn.execute(params![t.uuid, t.session_id, t.agent_id, t.ts, t.kind, t.preview])?;
            }
        }
        for f in files {
            for r in &f.batch.tool_results {
                result.execute(params![r.tool_use_id, r.ts, r.is_error as i64, r.bytes, r.result_hash, r.error_preview])?;
            }
            file.execute(params![f.path, f.slot, f.size as i64, f.mtime, f.offset as i64])?;
        }
    }
    tx.commit()?;
    Ok(())
}
