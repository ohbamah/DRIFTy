//! Renommer et archiver une conversation.
//!
//! Renommer écrit le titre comme le fait `/rename` de Claude Code : une ligne
//! `custom-title` ajoutée au transcript (le nom suit donc la conversation partout, même
//! dans `claude --resume`, et survit à une ré-analyse), puis la base est mise à jour tout
//! de suite. Archiver est propre à DRIFTy : la conversation sort de la barre latérale et
//! reste consultable dans les réglages de son projet.

use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::io::Write;

pub fn rename(conn: &Connection, sid: &str, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 || title.contains(['\n', '\r']) {
        bail!("The conversation cannot be renamed because its name must be 1 to 120 characters on one line.");
    }
    let file: Option<String> = conn
        .query_row("SELECT file_path FROM sessions WHERE id = ?1", params![sid], |r| r.get(0))
        .map_err(|_| anyhow::anyhow!("The conversation `{sid}` cannot be renamed because it is unknown."))?;
    let file = file.ok_or_else(|| anyhow::anyhow!("The conversation `{sid}` cannot be renamed because its transcript is unknown."))?;
    let line = format!("{}\n", json!({"type": "custom-title", "customTitle": title, "sessionId": sid}));
    // Une seule écriture en ajout : atomique face à un Claude Code qui écrit en même temps.
    let mut f = std::fs::OpenOptions::new().append(true).open(&file)
        .map_err(|e| anyhow::anyhow!("The conversation cannot be renamed because `{file}` cannot be written: {e}"))?;
    f.write_all(line.as_bytes())?;
    conn.execute("UPDATE sessions SET title = ?2, custom_title = ?2 WHERE id = ?1", params![sid, title])?;
    Ok(())
}

pub fn archive(conn: &Connection, sid: &str, at: i64) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO archived(session_id, at) VALUES (?1, ?2)", params![sid, at])?;
    Ok(())
}

pub fn unarchive(conn: &Connection, sid: &str) -> Result<()> {
    conn.execute("DELETE FROM archived WHERE session_id = ?1", params![sid])?;
    Ok(())
}

/// Conversations archivées d'un projet, les plus récemment archivées d'abord.
pub fn archived(conn: &Connection, repo_id: i64) -> Result<Value> {
    let mut st = conn.prepare(
        "SELECT s.id, s.title, s.last_ts, c.worktree, a.at,
                (SELECT t.preview FROM turns t WHERE t.session_id = s.id AND t.kind = 'prompt' AND t.agent_id = '' ORDER BY t.ts LIMIT 1)
         FROM archived a JOIN sessions s ON s.id = a.session_id JOIN cwd_map c ON c.cwd = s.cwd
         WHERE c.repo_id = ?1 GROUP BY s.id ORDER BY a.at DESC",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![repo_id], |r| {
            Ok(json!({"id": r.get::<_, String>(0)?, "title": r.get::<_, Option<String>>(1)?, "last_ts": r.get::<_, Option<i64>>(2)?,
                      "worktree": r.get::<_, String>(3)?, "archived_at": r.get::<_, i64>(4)?, "first_prompt": r.get::<_, Option<String>>(5)?}))
        })?
        .collect::<Result<_, _>>()?;
    Ok(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    #[test]
    fn rename_appends_custom_title_and_archive_round_trips() {
        let dir = std::env::temp_dir().join(format!("drifty-convs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = crate::db::open(&dir.join("t.db")).unwrap();
        let f = dir.join("s1.jsonl");
        std::fs::write(&f, "{\"type\":\"user\"}\n").unwrap();
        c.execute("INSERT INTO sessions(id, file_path, title) VALUES ('s1', ?1, 'Ancien')", rusqlite::params![f.to_str().unwrap()]).unwrap();
        super::rename(&c, "s1", "Nouveau nom").unwrap();
        let t: String = c.query_row("SELECT title FROM sessions WHERE id = 's1'", [], |r| r.get(0)).unwrap();
        assert_eq!(t, "Nouveau nom");
        assert!(std::fs::read_to_string(&f).unwrap().ends_with("{\"type\":\"custom-title\",\"customTitle\":\"Nouveau nom\",\"sessionId\":\"s1\"}\n"));
        assert!(super::rename(&c, "s1", "  ").is_err());
        super::archive(&c, "s1", 5).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM archived", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        super::unarchive(&c, "s1").unwrap();
        drop(c);
        std::fs::remove_dir_all(&dir).ok();
    }
}
