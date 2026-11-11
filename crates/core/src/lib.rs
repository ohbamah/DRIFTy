//! DRIFTy — noyau : ingestion des transcripts Claude Code, base SQLite,
//! requêtes d'analyse et digest d'évaluation (étage 1, sans token).

pub mod db;
pub mod diffstat;
pub mod digest;
pub mod extract;
pub mod fixes;
pub mod ingest;
pub mod ledger;
pub mod query;
pub mod raw;
pub mod repos;
pub mod rule_effects;
pub mod search;
pub mod convs;
pub mod transcript;
pub mod usage;

use std::path::PathBuf;

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// `$DRIFTY_DB`, sinon `~/.drifty/drifty.db`.
pub fn db_path() -> PathBuf {
    std::env::var_os("DRIFTY_DB").map(PathBuf::from).unwrap_or_else(|| home().join(".drifty").join("drifty.db"))
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncReport {
    pub ingest: ingest::Report,
    pub new_folders: usize,
    pub analysed_sessions: usize,
    pub millis: u128,
}

/// La synchronisation complète, en un seul point : transcripts → projets → bilan.
pub fn sync(conn: &mut rusqlite::Connection) -> anyhow::Result<SyncReport> {
    sync_in(conn, None)
}

/// La même synchronisation, limitée aux transcripts des dossiers `folders` (chemins) si donnés :
/// « Analyser » un seul worktree sans relire tous les comptes.
pub fn sync_in(conn: &mut rusqlite::Connection, folders: Option<&[String]>) -> anyhow::Result<SyncReport> {
    let start = std::time::Instant::now();
    let only: Option<Vec<String>> = folders.map(|f| f.iter().map(|p| ingest::project_dir_name(p)).collect());
    let ingest = ingest::run_in(conn, only.as_deref())?;
    let new_folders = repos::sync(conn)?;
    let analysed_sessions = ledger::refresh(conn)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    if folders.is_none() {
        db::set_setting(conn, "last_sync_at", &now.to_string())?;
    }
    Ok(SyncReport { ingest, new_folders, analysed_sessions, millis: start.elapsed().as_millis() })
}

/// Ancien nom (Claudius) : `~/.claudius` passe dans `~/.drifty` (base `drifty.db`).
/// À appeler quand l'ancien démon ne peut plus tourner (port déjà pris par le nouveau).
/// `~/.drifty` peut déjà exister (dossier de travail créé avant) : on FUSIONNE, et une
/// base `drifty.db` sans aucune conversation cède la place à l'ancienne (gardée de côté).
pub fn migrate_legacy_dir() -> std::io::Result<()> {
    if std::env::var_os("DRIFTY_DB").is_some() {
        return Ok(());
    }
    let (old, new) = (home().join(".claudius"), home().join(".drifty"));
    if !old.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&new)?;
    let old_db = old.join("claudius.db");
    let new_db = new.join("drifty.db");
    if old_db.exists() {
        if new_db.exists() && !db_is_empty(&new_db) {
            eprintln!("DRIFTy : {} contient déjà des conversations ; {} n'est pas repris.", new_db.display(), old_db.display());
        } else {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            for ext in ["", "-wal", "-shm"] {
                let fresh = new.join(format!("drifty.db{ext}"));
                if fresh.exists() {
                    std::fs::rename(&fresh, new.join(format!("drifty.db.vide-{stamp}{ext}")))?;
                }
                let legacy = old.join(format!("claudius.db{ext}"));
                if legacy.exists() {
                    std::fs::rename(legacy, fresh)?;
                }
            }
        }
    }
    merge_dir(&old, &new)?;
    let _ = std::fs::remove_dir(&old); // seulement s'il est vide
    Ok(())
}

/// Une base sans aucune conversation (créée au démarrage, avant la migration).
fn db_is_empty(path: &std::path::Path) -> bool {
    rusqlite::Connection::open(path)
        .and_then(|c| c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0)))
        .map_or(true, |n| n == 0)
}

/// Déplace dans `to` ce qui n'y est pas encore (récursif) ; ce qui existe des deux côtés reste.
fn merge_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    for e in std::fs::read_dir(from)?.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if !dst.exists() {
            std::fs::rename(&src, &dst)?;
        } else if src.is_dir() && dst.is_dir() {
            merge_dir(&src, &dst)?;
            let _ = std::fs::remove_dir(&src);
        }
    }
    Ok(())
}
