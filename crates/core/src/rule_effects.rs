//! Effet de chaque règle du bloc DRIFTy d'un CLAUDE.md : le gaspillage
//! qu'elle vise, avant / après sa mise en place. Une règle sans effet peut
//! être retirée : le CLAUDE.md ne garde que ce qui sert.

use crate::fixes;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;

const DAY: i64 = 86_400_000;
/// Fenêtre « avant » : les 30 jours précédant la règle.
const BEFORE_WINDOW: i64 = 30 * DAY;
/// En dessous, on ne conclut pas.
const MIN_SESSIONS_AFTER: i64 = 5;

/// La cause enregistrée par « Appliquer » qui a introduit une règle.
fn cause_of(key: &str) -> &'static str {
    match key {
        "repeat" => "repeats",
        "big-cmd" | "big-read" => "big-results",
        "poll" => "tool:poll:",
        _ => "failed",
    }
}

struct Rate {
    value: f64,
    sessions: i64,
}

/// Taux mesuré pour une règle sur les sessions du projet commencées dans [from, to).
fn rate(conn: &Connection, repo_id: i64, key: &str, from: i64, to: i64) -> Result<Rate> {
    let sessions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sessions s JOIN cwd_map c ON c.cwd = s.cwd
         WHERE c.repo_id = ?1 AND s.first_ts >= ?2 AND s.first_ts < ?3 AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id)",
        params![repo_id, from, to],
        |r| r.get(0),
    )?;
    let value = match key {
        // Part du contexte gaspillée (d'après l'analyse de chaque session).
        "repeat" | "big-cmd" | "big-read" => {
            let col = if key == "repeat" { "w.repeat_tokens" } else { "w.big_tokens" };
            let (waste, ctx): (i64, i64) = conn.query_row(
                &format!(
                    "SELECT COALESCE(SUM({col}), 0), COALESCE(SUM(w.context_tokens), 0)
                     FROM session_waste w JOIN sessions s ON s.id = w.session_id JOIN cwd_map c ON c.cwd = s.cwd
                     WHERE c.repo_id = ?1 AND s.first_ts >= ?2 AND s.first_ts < ?3"
                ),
                params![repo_id, from, to],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if ctx > 0 { waste as f64 / ctx as f64 } else { 0.0 }
        }
        // Pour 1 000 appels d'outil : boucles d'attente, ou échecs évitables de ce type.
        _ => {
            let mut st = conn.prepare(
                "SELECT t.name, t.target, COALESCE(t.is_error, 0), t.error_preview
                 FROM tool_calls t JOIN sessions s ON s.id = t.session_id JOIN cwd_map c ON c.cwd = s.cwd
                 WHERE c.repo_id = ?1 AND s.first_ts >= ?2 AND s.first_ts < ?3",
            )?;
            let (mut calls, mut hits) = (0i64, 0i64);
            let rows = st.query_map(params![repo_id, from, to], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)? != 0, r.get::<_, Option<String>>(3)?))
            })?;
            for (name, target, is_error, preview) in rows.flatten() {
                calls += 1;
                let hit = if key == "poll" {
                    name == "Bash" && target.as_deref().map(crate::digest::command_head).is_some_and(|h| {
                        matches!(h.split_whitespace().next(), Some("until" | "while" | "sleep"))
                    })
                } else {
                    is_error && preview.as_deref().and_then(fixes::avoidable).is_some_and(|f| f.key == key)
                };
                hits += hit as i64;
            }
            if calls > 0 { hits as f64 * 1000.0 / calls as f64 } else { 0.0 }
        }
    };
    Ok(Rate { value, sessions })
}

fn claude_md(conn: &Connection, repo_id: i64) -> Result<std::path::PathBuf> {
    let root: String = conn.query_row("SELECT root FROM repos WHERE id = ?1", params![repo_id], |r| r.get(0))?;
    Ok(Path::new(&root).join("CLAUDE.md"))
}

/// Les règles du bloc DRIFTy d'un projet, avec leur effet mesuré.
pub fn list(conn: &Connection, repo_id: i64) -> Result<Value> {
    let path = claude_md(conn, repo_id)?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let mut out = Vec::new();
    for (key, rule) in fixes::block_rules(&text) {
        let cause = cause_of(&key);
        // Mise en place : la première fois que sa cause a été appliquée dans ce projet.
        let applied: Option<i64> = conn
            .query_row(
                "SELECT MIN(ts) FROM fixes WHERE repo_id = ?1 AND (cause = ?2 OR (?2 = 'tool:poll:' AND cause LIKE 'tool:poll:%'))",
                params![repo_id, cause],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let Some(at) = applied else {
            out.push(json!({"key": key, "rule": rule, "verdict": "inconnu"}));
            continue;
        };
        let before = rate(conn, repo_id, &key, at - BEFORE_WINDOW, at)?;
        let after = rate(conn, repo_id, &key, at, now + DAY)?;
        let verdict = if after.sessions < MIN_SESSIONS_AFTER {
            "trop tôt"
        } else if before.value == 0.0 {
            "rien à mesurer"
        } else if after.value <= before.value * 0.5 {
            "efficace"
        } else if after.value >= before.value * 0.9 {
            "sans effet"
        } else {
            "partiel"
        };
        out.push(json!({
            "key": key, "rule": rule, "applied_at": at, "verdict": verdict,
            "unit": if matches!(key.as_str(), "repeat" | "big-cmd" | "big-read") { "part du contexte" } else { "pour 1 000 appels" },
            "before": before.value, "before_sessions": before.sessions,
            "after": after.value, "after_sessions": after.sessions, "min_sessions": MIN_SESSIONS_AFTER,
        }));
    }
    Ok(json!({"path": path, "rules": out}))
}

/// Retire une règle du bloc DRIFTy du projet.
pub fn remove(conn: &Connection, repo_id: i64, key: &str) -> Result<()> {
    let path = claude_md(conn, repo_id)?;
    let text = std::fs::read_to_string(&path)?;
    std::fs::write(&path, fixes::without_rule(&text, key)?)?;
    Ok(())
}
