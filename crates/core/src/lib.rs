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

/// `$CLAUDIUS_DB`, sinon `~/.claudius/claudius.db`.
pub fn db_path() -> PathBuf {
    std::env::var_os("CLAUDIUS_DB").map(PathBuf::from).unwrap_or_else(|| home().join(".claudius").join("claudius.db"))
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
    let start = std::time::Instant::now();
    let ingest = ingest::run(conn)?;
    let new_folders = repos::sync(conn)?;
    let analysed_sessions = ledger::refresh(conn)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    db::set_setting(conn, "last_sync_at", &now.to_string())?;
    Ok(SyncReport { ingest, new_folders, analysed_sessions, millis: start.elapsed().as_millis() })
}
