//! Projets : chaque dossier de travail est rattaché à son dépôt git (les
//! worktrees d'un même dépôt, Conductor compris, tombent dans le même projet),
//! ou au dossier lui-même hors git.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Resolved {
    key: String,
    root: String,
    worktree: String,
    rel: String,
    present: bool,
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<String>> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

fn basename(p: &str) -> String {
    Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

fn resolve(cwd: &str) -> Resolved {
    let path = PathBuf::from(cwd);
    let present = path.is_dir();
    // Dossier disparu (worktree supprimé…) : on remonte au premier ancêtre existant.
    let probe = path.ancestors().find(|a| a.is_dir()).map(Path::to_path_buf);
    if let Some(dir) = probe.filter(|_| present) {
        if let Some(lines) = git(&dir, &["rev-parse", "--path-format=absolute", "--git-common-dir", "--show-toplevel"]) {
            if let [common, top, ..] = lines.as_slice() {
                let root = if common.ends_with("/.git") { common.trim_end_matches("/.git").to_string() } else { top.clone() };
                let worktree = if top == &root { String::new() } else { basename(top) };
                let rel = path.strip_prefix(top).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
                return Resolved { key: common.clone(), root, worktree, rel, present };
            }
        }
    }
    Resolved { key: format!("dir:{cwd}"), root: cwd.to_string(), worktree: String::new(), rel: String::new(), present }
}

/// Rattache les dossiers de travail encore inconnus. Rapide : un `git` par dossier nouveau.
pub fn sync(conn: &Connection) -> Result<usize> {
    let mut st = conn.prepare(
        "SELECT DISTINCT cwd FROM sessions WHERE cwd IS NOT NULL AND cwd NOT IN (SELECT cwd FROM cwd_map)",
    )?;
    let cwds: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut resolved: Vec<(&String, Resolved)> = cwds.iter().map(|c| (c, resolve(c))).collect();
    resolved.sort_by_key(|(_, r)| !r.present);
    for (cwd, r) in resolved {
        conn.execute(
            "INSERT INTO repos(key, root, name) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO NOTHING",
            params![r.key, r.root, basename(&r.root)],
        )?;
        let id: i64 = conn.query_row("SELECT id FROM repos WHERE key = ?1", params![r.key], |x| x.get(0))?;
        conn.execute(
            "INSERT OR REPLACE INTO cwd_map(cwd, repo_id, worktree, rel, present) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![cwd, id, r.worktree, r.rel, r.present as i64],
        )?;
    }
    adopt_missing_worktrees(conn)?;
    Ok(cwds.len())
}

/// Un dossier disparu ne peut plus interroger git. S'il a un VOISIN (même
/// dossier parent) qui est un worktree d'un dépôt, c'est un worktree supprimé
/// de ce même dépôt : on l'y rattache, sous son nom.
fn adopt_missing_worktrees(conn: &Connection) -> Result<()> {
    let mut st = conn.prepare(
        "SELECT c.cwd FROM cwd_map c JOIN repos r ON r.id = c.repo_id WHERE c.present = 0 AND r.key LIKE 'dir:%'",
    )?;
    let missing: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut siblings = conn.prepare(
        "SELECT DISTINCT c.repo_id FROM cwd_map c JOIN repos r ON r.id = c.repo_id
         WHERE c.worktree <> '' AND r.key NOT LIKE 'dir:%' AND c.cwd LIKE ?1 || '/%' AND c.cwd NOT LIKE ?1 || '/%/%'",
    )?;
    for cwd in missing {
        let Some(parent) = Path::new(&cwd).parent().map(|p| p.to_string_lossy().into_owned()) else { continue };
        let repos: Vec<i64> = siblings.query_map(params![parent], |r| r.get(0))?.collect::<Result<_, _>>()?;
        // Deux dépôts voisins possibles : on ne devine pas, le dossier reste à part.
        if let [repo_id] = repos.as_slice() {
            conn.execute(
                "UPDATE cwd_map SET repo_id = ?2, worktree = ?3, rel = '' WHERE cwd = ?1",
                params![cwd, repo_id, basename(&cwd)],
            )?;
        }
    }
    // Projets devenus vides et jamais personnalisés.
    conn.execute(
        "DELETE FROM repos WHERE id NOT IN (SELECT repo_id FROM cwd_map)
           AND icon IS NULL AND color IS NULL AND slot IS NULL AND hidden = 0",
        [],
    )?;
    Ok(())
}

#[derive(Deserialize)]
pub struct RepoPatch {
    pub name: Option<String>,
    /// Emoji, ou image en `data:image/...;base64,...` (≤ 300 Ko).
    pub icon: Option<String>,
    pub color: Option<String>,
    /// `perso`, `pro`, ou `""` pour retirer le choix.
    pub slot: Option<String>,
    pub hidden: Option<bool>,
    /// Où démarre un nouveau chat : `worktree` (isolé) ou `project` (dossier du projet).
    pub chat_mode: Option<String>,
}

pub fn patch(conn: &Connection, id: i64, p: &RepoPatch) -> Result<()> {
    let exists: Option<i64> = conn.query_row("SELECT id FROM repos WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
    if exists.is_none() {
        anyhow::bail!("The project `{id}` cannot be changed because it does not exist.");
    }
    if let Some(name) = &p.name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            anyhow::bail!("The project name cannot be used because it must be 1 to 60 characters long.");
        }
        conn.execute("UPDATE repos SET name = ?2 WHERE id = ?1", params![id, name])?;
    }
    if let Some(icon) = &p.icon {
        let ok = icon.is_empty()
            || (icon.starts_with("data:image/") && icon.len() <= 300_000)
            || (!icon.starts_with("data:") && icon.chars().count() <= 4);
        if !ok {
            anyhow::bail!("The project icon cannot be used because it must be an emoji or an image under 300 KB.");
        }
        conn.execute("UPDATE repos SET icon = NULLIF(?2, '') WHERE id = ?1", params![id, icon])?;
    }
    if let Some(color) = &p.color {
        let ok = color.is_empty() || (color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit()));
        if !ok {
            anyhow::bail!("The project color `{color}` cannot be used because it is not a #rrggbb value.");
        }
        conn.execute("UPDATE repos SET color = NULLIF(?2, '') WHERE id = ?1", params![id, color])?;
    }
    if let Some(slot) = &p.slot {
        if !slot.is_empty() {
            let configured: Option<String> =
                conn.query_row("SELECT slot FROM accounts WHERE slot = ?1 AND enabled = 1", params![slot], |r| r.get(0)).optional()?;
            if configured.is_none() {
                anyhow::bail!("The account `{slot}` cannot be assigned because it is not configured or not enabled.");
            }
        }
        conn.execute("UPDATE repos SET slot = NULLIF(?2, '') WHERE id = ?1", params![id, slot])?;
    }
    if let Some(m) = &p.chat_mode {
        if m != "worktree" && m != "project" {
            anyhow::bail!("The chat mode `{m}` cannot be used because only `worktree` and `project` exist.");
        }
        conn.execute("UPDATE repos SET chat_mode = ?2 WHERE id = ?1", params![id, m])?;
    }
    if let Some(h) = p.hidden {
        conn.execute("UPDATE repos SET hidden = ?2 WHERE id = ?1", params![id, h as i64])?;
    }
    Ok(())
}

#[derive(Serialize)]
struct Group {
    worktree: String,
    rel: String,
    sessions: i64,
    last_ts: Option<i64>,
}

/// Worktrees créés par DRIFTy (`~/drifty/worktrees/…`, ou l'ancien `~/claudius/worktrees/…`)
/// encore présents dans `git worktree list --porcelain` : leurs noms.
fn app_worktrees(porcelain: &str, home: &Path) -> Vec<String> {
    let bases = [home.join("drifty").join("worktrees"), home.join("claudius").join("worktrees")];
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .filter(|p| bases.iter().any(|b| Path::new(p).starts_with(b)))
        .map(basename)
        .collect()
}

/// Les projets, avec leurs sous-groupes (worktree / sous-dossier) et leurs totaux.
pub fn list(conn: &Connection) -> Result<Value> {
    let mut st = conn.prepare(
        "SELECT r.id, r.key, r.root, r.name, r.icon, r.color, r.slot, r.hidden,
                COUNT(s.id), MAX(s.last_ts), SUM(s.cost_usd), r.chat_mode
         FROM repos r
         JOIN cwd_map c ON c.repo_id = r.id
         JOIN sessions s ON s.cwd = c.cwd
         WHERE EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id)
         GROUP BY r.id ORDER BY MAX(s.last_ts) DESC",
    )?;
    let repos: Vec<(i64, Value)> = st
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            Ok((
                id,
                json!({
                    "id": id, "key": r.get::<_, String>(1)?, "root": r.get::<_, String>(2)?, "name": r.get::<_, String>(3)?,
                    "icon": r.get::<_, Option<String>>(4)?, "color": r.get::<_, Option<String>>(5)?,
                    "slot": r.get::<_, Option<String>>(6)?, "hidden": r.get::<_, i64>(7)? != 0,
                    "sessions": r.get::<_, i64>(8)?, "last_ts": r.get::<_, Option<i64>>(9)?, "cost_usd": r.get::<_, Option<f64>>(10)?,
                    "chat_mode": r.get::<_, String>(11)?,
                }),
            ))
        })?
        .collect::<Result<_, _>>()?;

    let mut gst = conn.prepare(
        "SELECT c.worktree, c.rel, COUNT(s.id), MAX(s.last_ts)
         FROM cwd_map c JOIN sessions s ON s.cwd = c.cwd
         WHERE c.repo_id = ?1 AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id)
           AND s.id NOT IN (SELECT session_id FROM archived)
         GROUP BY c.worktree, c.rel ORDER BY c.worktree <> '', c.worktree, c.rel",
    )?;
    let mut arch = conn.prepare("SELECT 1 FROM archived_worktrees WHERE repo_id = ?1 AND worktree = ?2")?;
    let default = crate::db::default_slot(conn)?;
    let mut out = Vec::with_capacity(repos.len());
    for (id, mut v) in repos {
        v["slot_explicit"] = json!(!v["slot"].is_null());
        if v["slot"].is_null() {
            v["slot"] = json!(default);
        }
        let mut groups: Vec<Group> = gst
            .query_map(params![id], |r| Ok(Group { worktree: r.get(0)?, rel: r.get(1)?, sessions: r.get(2)?, last_ts: r.get(3)? }))?
            .collect::<Result<_, _>>()?;
        // Un worktree créé par l'app existe avant que ses conversations soient analysées (et
        // une discussion en direct ne survit pas au redémarrage) : il reste visible et choisissable.
        if !v["key"].as_str().unwrap_or("").starts_with("dir:") {
            let root = v["root"].as_str().unwrap_or("").to_string();
            let porcelain = git(Path::new(&root), &["worktree", "list", "--porcelain"]).unwrap_or_default().join("\n");
            for name in app_worktrees(&porcelain, &crate::home()) {
                let archived: bool = arch.query_row(params![id, name], |_| Ok(())).optional()?.is_some();
                if !archived && !groups.iter().any(|g| g.worktree == name) {
                    groups.push(Group { worktree: name, rel: String::new(), sessions: 0, last_ts: None });
                }
            }
        }
        v["groups"] = serde_json::to_value(groups)?;
        out.push(v);
    }
    Ok(Value::Array(out))
}

/// Conversations d'un projet, filtrables par worktree et sous-dossier.
pub fn conversations(conn: &Connection, repo_id: i64, worktree: Option<&str>, rel: Option<&str>, sort: &str) -> Result<Value> {
    let order = match sort {
        "cost" => "COALESCE(s.cost_usd, 0) DESC",
        "calls" => "COALESCE(m.n, 0) DESC",
        "title" => "COALESCE(s.title, s.id) COLLATE NOCASE",
        _ => "s.last_ts DESC",
    };
    let sql = format!(
        "WITH m AS (SELECT session_id, COUNT(*) AS n, COUNT(DISTINCT NULLIF(agent_id, '')) AS agents FROM messages GROUP BY session_id),
              t AS (SELECT session_id, SUM(COALESCE(is_error, 0)) AS errors FROM tool_calls GROUP BY session_id),
              p AS (SELECT session_id, preview FROM turns WHERE kind = 'prompt' AND agent_id = ''
                    AND ts = (SELECT MIN(ts) FROM turns t2 WHERE t2.session_id = turns.session_id AND t2.kind = 'prompt' AND t2.agent_id = ''))
         SELECT s.id, s.title, s.slot, s.branch, s.first_ts, s.last_ts, s.cost_usd, c.worktree, c.rel,
                COALESCE(m.n, 0), COALESCE(m.agents, 0), COALESCE(t.errors, 0), p.preview, w.failed_tokens + w.repeat_tokens + w.big_tokens,
                (SELECT mm.model FROM messages mm WHERE mm.session_id = s.id AND mm.agent_id = '' AND mm.model LIKE 'claude-%' ORDER BY mm.ts DESC LIMIT 1)
         FROM sessions s
         JOIN cwd_map c ON c.cwd = s.cwd
         JOIN m ON m.session_id = s.id
         LEFT JOIN t ON t.session_id = s.id
         LEFT JOIN p ON p.session_id = s.id
         LEFT JOIN session_waste w ON w.session_id = s.id
         WHERE c.repo_id = ?1 AND (?2 IS NULL OR c.worktree = ?2) AND (?3 IS NULL OR c.rel = ?3)
           AND s.id NOT IN (SELECT session_id FROM archived)
         GROUP BY s.id ORDER BY {order} LIMIT 1000"
    );
    let mut st = conn.prepare(&sql)?;
    let rows: Vec<Value> = st
        .query_map(params![repo_id, worktree, rel], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "title": r.get::<_, Option<String>>(1)?, "slot": r.get::<_, Option<String>>(2)?,
                "branch": r.get::<_, Option<String>>(3)?, "first_ts": r.get::<_, Option<i64>>(4)?, "last_ts": r.get::<_, Option<i64>>(5)?,
                "cost_usd": r.get::<_, Option<f64>>(6)?, "worktree": r.get::<_, String>(7)?, "rel": r.get::<_, String>(8)?,
                "api_calls": r.get::<_, i64>(9)?, "subagents": r.get::<_, i64>(10)?, "tool_errors": r.get::<_, i64>(11)?,
                "first_prompt": r.get::<_, Option<String>>(12)?, "waste_tokens": r.get::<_, Option<i64>>(13)?,
                "model": r.get::<_, Option<String>>(14)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(Value::Array(rows))
}

/// Ce qu'il faut pour ouvrir un terminal sur ce projet / cette session.
pub struct LaunchTarget {
    pub dir: String,
    pub config_dir: Option<String>,
    pub resume: Option<String>,
}

pub fn account_dir(conn: &Connection, slot: Option<&str>) -> Result<Option<String>> {
    let Some(slot) = slot else { return Ok(None) };
    Ok(conn
        .query_row("SELECT config_dir FROM accounts WHERE slot = ?1 AND enabled = 1", params![slot], |r| r.get(0))
        .optional()?)
}

/// Nouvelle conversation : à la racine du projet, avec le compte du projet.
pub fn launch_new(conn: &Connection, repo_id: i64) -> Result<LaunchTarget> {
    let (root, slot): (String, Option<String>) =
        conn.query_row("SELECT root, slot FROM repos WHERE id = ?1", params![repo_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let Some(slot) = slot.or(crate::db::default_slot(conn)?) else {
        anyhow::bail!("The project cannot start a conversation because no account is configured.");
    };
    let config_dir = account_dir(conn, Some(&slot))?
        .ok_or_else(|| anyhow::anyhow!("The account `{slot}` cannot be used because it is not enabled."))?;
    if !Path::new(&root).is_dir() {
        anyhow::bail!("The project folder `{root}` cannot be opened because it no longer exists.");
    }
    Ok(LaunchTarget { dir: root, config_dir: Some(config_dir), resume: None })
}

/// Reprise : dans le dossier d'origine, avec le compte qui DÉTIENT le transcript
/// (une reprise sous un autre compte ne trouverait pas la session).
pub fn launch_resume(conn: &Connection, sid: &str) -> Result<LaunchTarget> {
    let (cwd, slot): (Option<String>, Option<String>) =
        conn.query_row("SELECT cwd, slot FROM sessions WHERE id = ?1", params![sid], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let Some(cwd) = cwd else { anyhow::bail!("The conversation `{sid}` cannot be resumed because its folder is unknown.") };
    if !Path::new(&cwd).is_dir() {
        anyhow::bail!("The conversation cannot be resumed because its folder `{cwd}` no longer exists.");
    }
    let config_dir = account_dir(conn, slot.as_deref())?;
    Ok(LaunchTarget { dir: cwd, config_dir, resume: Some(sid.to_string()) })
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_worktrees_are_listed() {
        let p = "worktree /r/archc\nHEAD aa\nbranch refs/heads/main\n\nworktree /h/drifty/worktrees/archc/Silencieux-2-decisions\nHEAD bb\n\nworktree /h/claudius/worktrees/archc/old\n\nworktree /h/conductor/workspaces/archc/x\n";
        assert_eq!(super::app_worktrees(p, std::path::Path::new("/h")), ["Silencieux-2-decisions", "old"]);
    }
}
