//! Requêtes de lecture pour l'interface (vue d'ensemble, listes).

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

fn since(days: i64) -> i64 {
    if days <= 0 {
        return 0;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    now - days * 86_400_000
}

/// `slot` vide = les deux comptes.
pub fn overview(conn: &Connection, days: i64, slot: &str) -> Result<Value> {
    let from = since(days);
    let totals = conn.query_row(
        "SELECT COUNT(DISTINCT m.session_id), COUNT(*), COALESCE(SUM(m.input), 0), COALESCE(SUM(m.cache_write), 0),
                COALESCE(SUM(m.cache_read), 0), COALESCE(SUM(m.output), 0), COALESCE(SUM(m.thinking), 0)
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.ts >= ?1 AND (?2 = '' OR s.slot = ?2)",
        params![from, slot],
        |r| {
            let (input, cw, cr): (i64, i64, i64) = (r.get(2)?, r.get(3)?, r.get(4)?);
            let ctx = input + cw + cr;
            Ok(json!({
                "sessions": r.get::<_, i64>(0)?, "api_calls": r.get::<_, i64>(1)?,
                "input": input, "cache_write": cw, "cache_read": cr,
                "output": r.get::<_, i64>(5)?, "thinking": r.get::<_, i64>(6)?,
                "cache_ratio": if ctx > 0 { cr as f64 / ctx as f64 } else { 0.0 },
            }))
        },
    )?;
    let cost: Option<f64> = conn.query_row(
        "SELECT SUM(cost_usd) FROM sessions WHERE last_ts >= ?1 AND (?2 = '' OR slot = ?2)",
        params![from, slot],
        |r| r.get(0),
    )?;
    let tools = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(t.is_error), 0) FROM tool_calls t JOIN sessions s ON s.id = t.session_id
         WHERE t.ts >= ?1 AND (?2 = '' OR s.slot = ?2)",
        params![from, slot],
        |r| Ok(json!({"calls": r.get::<_, i64>(0)?, "errors": r.get::<_, i64>(1)?})),
    )?;

    let mut st = conn.prepare(
        "SELECT strftime('%Y-%m-%d', m.ts / 1000, 'unixepoch', 'localtime') AS day,
                SUM(m.input), SUM(m.cache_write), SUM(m.cache_read), SUM(m.output), COUNT(DISTINCT m.session_id)
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.ts >= ?1 AND (?2 = '' OR s.slot = ?2)
         GROUP BY day ORDER BY day",
    )?;
    let daily: Vec<Value> = st
        .query_map(params![from, slot], |r| {
            Ok(json!({"day": r.get::<_, String>(0)?, "input": r.get::<_, i64>(1)?, "cache_write": r.get::<_, i64>(2)?,
                      "cache_read": r.get::<_, i64>(3)?, "output": r.get::<_, i64>(4)?, "sessions": r.get::<_, i64>(5)?}))
        })?
        .collect::<Result<_, _>>()?;

    let mut st = conn.prepare(
        "WITH m AS (
            SELECT c.repo_id AS p, COUNT(DISTINCT m.session_id) AS n, SUM(m.cache_read) AS cr,
                   SUM(m.input + m.cache_write) AS fresh, SUM(m.output) AS out
            FROM messages m JOIN sessions s ON s.id = m.session_id JOIN cwd_map c ON c.cwd = s.cwd
            WHERE m.ts >= ?1 AND (?2 = '' OR s.slot = ?2) GROUP BY c.repo_id),
         k AS (SELECT c.repo_id AS p, SUM(s.cost_usd) AS cost FROM sessions s JOIN cwd_map c ON c.cwd = s.cwd
               WHERE s.last_ts >= ?1 AND (?2 = '' OR s.slot = ?2) GROUP BY c.repo_id)
         SELECT r.id, r.name, r.root, m.n, m.cr, m.fresh, m.out, k.cost, r.icon, r.color, r.key
         FROM m JOIN repos r ON r.id = m.p LEFT JOIN k ON k.p = m.p
         ORDER BY COALESCE(k.cost, 0) DESC, m.cr DESC LIMIT 20",
    )?;
    let projects: Vec<Value> = st
        .query_map(params![from, slot], |r| {
            Ok(json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "root": r.get::<_, String>(2)?,
                      "sessions": r.get::<_, i64>(3)?, "cache_read": r.get::<_, i64>(4)?, "fresh": r.get::<_, i64>(5)?,
                      "output": r.get::<_, i64>(6)?, "cost_usd": r.get::<_, Option<f64>>(7)?,
                      "icon": r.get::<_, Option<String>>(8)?, "color": r.get::<_, Option<String>>(9)?, "key": r.get::<_, String>(10)?}))
        })?
        .collect::<Result<_, _>>()?;

    let mut st = conn.prepare(
        "SELECT m.model, COUNT(*), SUM(m.input + m.cache_write + m.cache_read), SUM(m.output)
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.ts >= ?1 AND (?2 = '' OR s.slot = ?2) AND m.model <> '<synthetic>'
         GROUP BY m.model ORDER BY 3 DESC",
    )?;
    let models: Vec<Value> = st
        .query_map(params![from, slot], |r| {
            Ok(json!({"model": r.get::<_, String>(0)?, "api_calls": r.get::<_, i64>(1)?, "context": r.get::<_, i64>(2)?,
                      "output": r.get::<_, i64>(3)?}))
        })?
        .collect::<Result<_, _>>()?;

    Ok(json!({"days": days, "slot": slot, "totals": totals, "cost_usd": cost, "tools": tools,
              "daily": daily, "projects": projects, "models": models}))
}

pub fn sessions(conn: &Connection, slot: &str, project: &str, limit: i64) -> Result<Value> {
    let mut st = conn.prepare(
        "WITH m AS (SELECT session_id, COUNT(*) AS n, SUM(input + cache_write + output) AS fresh, SUM(cache_read) AS cr,
                           COUNT(DISTINCT NULLIF(agent_id, '')) AS agents
                    FROM messages GROUP BY session_id),
              t AS (SELECT session_id, COUNT(*) AS calls, SUM(COALESCE(is_error, 0)) AS errors FROM tool_calls GROUP BY session_id)
         SELECT s.id, s.slot, s.project_dir, s.cwd, s.title, s.branch, s.first_ts, s.last_ts, s.cost_usd,
                COALESCE(m.n, 0), COALESCE(m.fresh, 0), COALESCE(m.cr, 0), COALESCE(m.agents, 0),
                COALESCE(t.calls, 0), COALESCE(t.errors, 0)
         FROM sessions s LEFT JOIN m ON m.session_id = s.id LEFT JOIN t ON t.session_id = s.id
         WHERE (?1 = '' OR s.slot = ?1) AND (?2 = '' OR s.project_dir = ?2) AND COALESCE(m.n, 0) > 0
         ORDER BY s.last_ts DESC LIMIT ?3",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![slot, project, limit], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "slot": r.get::<_, Option<String>>(1)?, "project": r.get::<_, Option<String>>(2)?,
                "cwd": r.get::<_, Option<String>>(3)?, "title": r.get::<_, Option<String>>(4)?, "branch": r.get::<_, Option<String>>(5)?,
                "first_ts": r.get::<_, Option<i64>>(6)?, "last_ts": r.get::<_, Option<i64>>(7)?, "cost_usd": r.get::<_, Option<f64>>(8)?,
                "api_calls": r.get::<_, i64>(9)?, "fresh": r.get::<_, i64>(10)?, "cache_read": r.get::<_, i64>(11)?,
                "subagents": r.get::<_, i64>(12)?, "tool_calls": r.get::<_, i64>(13)?, "tool_errors": r.get::<_, i64>(14)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(Value::Array(rows))
}

pub fn session(conn: &Connection, sid: &str) -> Result<Value> {
    Ok(conn.query_row(
        "SELECT id, slot, project_dir, cwd, title, branch, version, first_ts, last_ts, cost_usd,
                api_ms, tool_ms, total_ms, lines_added, lines_removed, model_usage, account_uuid,
                (SELECT COALESCE(SUM(m.input + m.cache_write + m.cache_read + m.output), 0) FROM messages m WHERE m.session_id = sessions.id),
                (SELECT m.model FROM messages m WHERE m.session_id = sessions.id AND m.agent_id = '' AND m.model LIKE 'claude-%' ORDER BY m.ts DESC LIMIT 1),
                (SELECT c.repo_id FROM cwd_map c WHERE c.cwd = sessions.cwd),
                (SELECT c.worktree FROM cwd_map c WHERE c.cwd = sessions.cwd),
                (SELECT c.rel FROM cwd_map c WHERE c.cwd = sessions.cwd)
         FROM sessions WHERE id = ?1",
        params![sid],
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "slot": r.get::<_, Option<String>>(1)?, "project": r.get::<_, Option<String>>(2)?,
                "cwd": r.get::<_, Option<String>>(3)?, "title": r.get::<_, Option<String>>(4)?, "branch": r.get::<_, Option<String>>(5)?,
                "version": r.get::<_, Option<String>>(6)?, "first_ts": r.get::<_, Option<i64>>(7)?, "last_ts": r.get::<_, Option<i64>>(8)?,
                "cost_usd": r.get::<_, Option<f64>>(9)?, "api_ms": r.get::<_, Option<f64>>(10)?, "tool_ms": r.get::<_, Option<f64>>(11)?,
                "total_ms": r.get::<_, Option<f64>>(12)?, "lines_added": r.get::<_, Option<i64>>(13)?,
                "lines_removed": r.get::<_, Option<i64>>(14)?,
                "model_usage": r.get::<_, Option<String>>(15)?.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                "account_uuid": r.get::<_, Option<String>>(16)?,
                "tokens_total": r.get::<_, i64>(17)?,
                "last_model": r.get::<_, Option<String>>(18)?,
                "repo_id": r.get::<_, Option<i64>>(19)?,
                "worktree": r.get::<_, Option<String>>(20)?,
                "rel": r.get::<_, Option<String>>(21)?,
            }))
        },
    )?)
}
