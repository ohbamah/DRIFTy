//! Archiver un worktree (comme Conductor) : ses conversations sont archivées, ses
//! discussions en cours arrêtées, et son dossier retiré (`git worktree remove`), sa
//! branche gardée. Refusé s'il a des changements non commités, sauf à garder le dossier.
//! Restaurer remet le dossier sur sa branche et ses conversations dans la barre.

use anyhow::{bail, Result};
use axum::extract::{Path, State};
use axum::Json;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Command;

use crate::{now_ms, with_conn, ApiResult, App};

fn git(dir: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Dossier du worktree et dossier principal du projet.
fn locate(c: &Connection, repo_id: i64, worktree: &str) -> Result<(String, String)> {
    if worktree.is_empty() {
        bail!("The worktree cannot be archived because the primary folder of the project is not a worktree.");
    }
    let root: String = c.query_row("SELECT root FROM repos WHERE id = ?1", params![repo_id], |r| r.get(0))?;
    let path: String = c
        .query_row(
            "SELECT cwd FROM cwd_map WHERE repo_id = ?1 AND worktree = ?2 ORDER BY rel <> '', length(cwd) LIMIT 1",
            params![repo_id, worktree],
            |r| r.get(0),
        )
        .map_err(|_| anyhow::anyhow!("The worktree `{worktree}` cannot be archived because it is unknown."))?;
    Ok((path, root))
}

#[derive(Deserialize)]
pub struct ArchiveReq {
    worktree: String,
    /// Changements non commités : archiver quand même, en gardant le dossier.
    #[serde(default)]
    keep_folder: bool,
}

pub async fn archive(State(app): State<App>, Path(repo_id): Path<i64>, Json(r): Json<ArchiveReq>) -> ApiResult {
    let wt = r.worktree.clone();
    let v = with_conn(&app, move |c| {
        let (path, root) = locate(c, repo_id, &wt)?;
        Ok(json!([path, root]))
    })
    .await?
    .0;
    let (path, root) = (v[0].as_str().unwrap_or("").to_string(), v[1].as_str().unwrap_or("").to_string());
    let exists = std::path::Path::new(&path).is_dir();
    let (branch, dirty) = if exists {
        let p = path.clone();
        tokio::task::spawn_blocking(move || -> Result<(String, usize)> {
            Ok((git(&p, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default(), git(&p, &["status", "--porcelain"])?.lines().filter(|l| !l.is_empty()).count()))
        })
        .await??
    } else {
        (String::new(), 0)
    };
    if dirty > 0 && !r.keep_folder {
        return Err(anyhow::anyhow!(
            "The worktree `{}` cannot be archived because {dirty} file{} would be lost (uncommitted changes).",
            r.worktree,
            if dirty > 1 { "s" } else { "" }
        )
        .into());
    }
    // Ses discussions en cours s'arrêtent (et leurs terminaux avec elles).
    for chat in app.chats.list() {
        let cwd = chat["cwd"].as_str().unwrap_or("");
        if !path.is_empty() && (cwd == path || cwd.starts_with(&format!("{path}/"))) {
            let id = chat["id"].as_str().unwrap_or("").to_string();
            let _ = app.chats.remove(&id).await;
            app.terms.close_for_chat(&id);
        }
    }
    let removed = exists && !r.keep_folder;
    if removed {
        let (p, rt) = (path.clone(), root.clone());
        tokio::task::spawn_blocking(move || git(&rt, &["worktree", "remove", &p]))
            .await?
            .map_err(|e| anyhow::anyhow!("The worktree cannot be archived because git refused to remove its folder: {e}"))?;
    }
    let wt = r.worktree.clone();
    with_conn(&app, move |c| {
        let at = now_ms();
        c.execute(
            "INSERT OR IGNORE INTO archived(session_id, at)
             SELECT s.id, ?3 FROM sessions s JOIN cwd_map m ON m.cwd = s.cwd WHERE m.repo_id = ?1 AND m.worktree = ?2",
            params![repo_id, wt, at],
        )?;
        c.execute(
            "INSERT OR REPLACE INTO archived_worktrees(repo_id, worktree, path, branch, removed, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![repo_id, wt, path, branch, removed, at],
        )?;
        Ok(json!({"ok": true, "removed": removed, "branch": branch}))
    })
    .await
}

#[derive(Deserialize)]
pub struct RestoreReq {
    worktree: String,
}

pub async fn restore(State(app): State<App>, Path(repo_id): Path<i64>, Json(r): Json<RestoreReq>) -> ApiResult {
    let wt = r.worktree.clone();
    let v = with_conn(&app, move |c| {
        let row: (String, String, bool, String) = c
            .query_row(
                "SELECT a.path, a.branch, a.removed, r.root FROM archived_worktrees a JOIN repos r ON r.id = a.repo_id WHERE a.repo_id = ?1 AND a.worktree = ?2",
                params![repo_id, wt],
                |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)),
            )
            .map_err(|_| anyhow::anyhow!("The worktree `{wt}` cannot be restored because it is not archived."))?;
        Ok(json!([row.0, row.1, row.2, row.3]))
    })
    .await?
    .0;
    let (path, branch, removed, root) = (v[0].as_str().unwrap_or("").to_string(), v[1].as_str().unwrap_or("").to_string(), v[2] == true, v[3].as_str().unwrap_or("").to_string());
    if removed && !std::path::Path::new(&path).exists() {
        if branch.is_empty() || branch == "HEAD" {
            return Err(anyhow::anyhow!("The worktree cannot be restored because its branch is unknown.").into());
        }
        tokio::task::spawn_blocking(move || git(&root, &["worktree", "add", &path, &branch]))
            .await?
            .map_err(|e| anyhow::anyhow!("The worktree cannot be restored because git refused: {e}"))?;
    }
    let wt = r.worktree.clone();
    with_conn(&app, move |c| {
        c.execute(
            "DELETE FROM archived WHERE session_id IN (SELECT s.id FROM sessions s JOIN cwd_map m ON m.cwd = s.cwd WHERE m.repo_id = ?1 AND m.worktree = ?2)",
            params![repo_id, wt],
        )?;
        c.execute("DELETE FROM archived_worktrees WHERE repo_id = ?1 AND worktree = ?2", params![repo_id, wt])?;
        Ok(json!({"ok": true}))
    })
    .await
}

pub async fn list(State(app): State<App>, Path(repo_id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| {
        let mut st = c.prepare(
            "SELECT a.worktree, a.branch, a.removed, a.at,
                    (SELECT COUNT(*) FROM sessions s JOIN cwd_map m ON m.cwd = s.cwd WHERE m.repo_id = a.repo_id AND m.worktree = a.worktree)
             FROM archived_worktrees a WHERE a.repo_id = ?1 ORDER BY a.at DESC",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![repo_id], |r| {
                Ok(json!({"worktree": r.get::<_, String>(0)?, "branch": r.get::<_, String>(1)?, "removed": r.get::<_, bool>(2)?,
                          "archived_at": r.get::<_, i64>(3)?, "conversations": r.get::<_, i64>(4)?}))
            })?
            .collect::<Result<_, _>>()?;
        Ok(Value::Array(rows))
    })
    .await
}
