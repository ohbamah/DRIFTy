//! Bilan : le gaspillage mesuré au fil du temps, et ce que Claudius dépense
//! lui-même. Le gaspillage d'une session vient du digest (un seul calcul).

use crate::digest;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const WEEK_MS: i64 = 7 * 86_400_000;

/// Recalcule le gaspillage des sessions nouvelles ou modifiées depuis le dernier passage.
/// Monter ce numéro quand la définition du gaspillage change : tout est recalculé.
const WASTE_VERSION: &str = "2";

pub fn refresh(conn: &Connection) -> Result<usize> {
    if crate::db::setting(conn, "waste_version")?.as_deref() != Some(WASTE_VERSION) {
        conn.execute("DELETE FROM session_waste", [])?;
        crate::db::set_setting(conn, "waste_version", WASTE_VERSION)?;
    }
    let mut st = conn.prepare(
        "SELECT s.id, s.last_ts FROM sessions s LEFT JOIN session_waste w ON w.session_id = s.id
         WHERE s.last_ts IS NOT NULL AND (w.last_ts IS NULL OR w.last_ts < s.last_ts)
           AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id)",
    )?;
    let todo: Vec<(String, i64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    for (sid, last_ts) in &todo {
        let d = digest::digest(conn, sid)?;
        let cause = |id: &str| -> (i64, i64) {
            d["causes"]
                .as_array()
                .and_then(|cs| cs.iter().find(|c| c["id"] == id))
                .map_or((0, 0), |c| (c["gain"]["tokens"].as_i64().unwrap_or(0), c["gain"]["ms"].as_i64().unwrap_or(0)))
        };
        let (failed, failed_ms) = cause("failed");
        let (repeat, repeat_ms) = cause("repeats");
        let (big, _) = cause("big-results");
        conn.execute(
            "INSERT OR REPLACE INTO session_waste VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![sid, last_ts, d["tokens"]["context_total"].as_i64().unwrap_or(0), failed, repeat, big, failed_ms + repeat_ms],
        )?;
    }
    Ok(todo.len())
}

/// Enregistre ce que Claudius a consommé lui-même (tokens qui n'existeraient pas sans l'app).
pub fn record_spend(conn: &Connection, kind: &str, session_id: Option<&str>, input: i64, output: i64, cost_usd: Option<f64>) -> Result<()> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    conn.execute(
        "INSERT INTO claudius_spend(ts, kind, session_id, input_tokens, output_tokens, cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, kind, session_id, input, output, cost_usd],
    )?;
    Ok(())
}

pub fn summary(conn: &Connection) -> Result<Value> {
    let installed: i64 = crate::db::setting(conn, "installed_at")?.and_then(|v| v.parse().ok()).unwrap_or(0);

    // Gaspillage par semaine (la semaine d'une session = celle de sa dernière activité).
    let mut st = conn.prepare(
        "SELECT (s.last_ts / ?1) AS wk, SUM(w.context_tokens), SUM(w.failed_tokens), SUM(w.repeat_tokens), SUM(w.big_tokens),
                SUM(w.waste_ms), COUNT(*)
         FROM session_waste w JOIN sessions s ON s.id = w.session_id
         GROUP BY wk ORDER BY wk",
    )?;
    let weeks: Vec<Value> = st
        .query_map(params![WEEK_MS], |r| {
            let wk: i64 = r.get(0)?;
            let ctx: i64 = r.get(1)?;
            let (f, rep, big): (i64, i64, i64) = (r.get(2)?, r.get(3)?, r.get(4)?);
            Ok(json!({
                "week_start": wk * WEEK_MS, "context": ctx, "failed": f, "repeats": rep, "big": big,
                "waste_ms": r.get::<_, i64>(5)?, "sessions": r.get::<_, i64>(6)?,
                "rate": if ctx > 0 { (f + rep + big) as f64 / ctx as f64 } else { 0.0 },
            }))
        })?
        .collect::<Result<_, _>>()?;

    // Avant / après la mise en service de Claudius.
    let period = |cond: &str| -> Result<(i64, i64, i64)> {
        Ok(conn.query_row(
            &format!(
                "SELECT COALESCE(SUM(w.context_tokens), 0), COALESCE(SUM(w.failed_tokens + w.repeat_tokens + w.big_tokens), 0), COUNT(*)
                 FROM session_waste w JOIN sessions s ON s.id = w.session_id WHERE s.first_ts {cond} ?1"
            ),
            params![installed],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    };
    let (ctx_before, waste_before, n_before) = period("<")?;
    let (ctx_after, waste_after, n_after) = period(">=")?;
    let rate = |w: i64, c: i64| if c > 0 { Some(w as f64 / c as f64) } else { None };
    let (rb, ra) = (rate(waste_before, ctx_before), rate(waste_after, ctx_after));
    // Économie réalisée = (taux d'avant − taux d'après) × volume d'après.
    // Rien n'est annoncé tant qu'il n'y a pas assez de sessions après la mise en service.
    const MIN_SESSIONS_AFTER: i64 = 10;
    let saved = match (rb, ra) {
        (Some(b), Some(a)) if n_after >= MIN_SESSIONS_AFTER => Some(((b - a) * ctx_after as f64).round() as i64),
        _ => None,
    };

    let spend = conn.query_row(
        "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), SUM(cost_usd), COUNT(*) FROM claudius_spend",
        [],
        |r| Ok(json!({"input": r.get::<_, i64>(0)?, "output": r.get::<_, i64>(1)?, "cost_usd": r.get::<_, Option<f64>>(2)?, "runs": r.get::<_, i64>(3)?})),
    )?;
    let mut st = conn.prepare(
        "SELECT kind, COUNT(*), SUM(input_tokens), SUM(output_tokens), SUM(cost_usd) FROM claudius_spend GROUP BY kind ORDER BY SUM(cost_usd) DESC",
    )?;
    let by_kind: Vec<Value> = st
        .query_map([], |r| {
            Ok(json!({"kind": r.get::<_, String>(0)?, "runs": r.get::<_, i64>(1)?, "input": r.get::<_, i64>(2)?,
                      "output": r.get::<_, i64>(3)?, "cost_usd": r.get::<_, Option<f64>>(4)?}))
        })?
        .collect::<Result<_, _>>()?;
    let identified: i64 = conn.query_row(
        "SELECT COALESCE(SUM(failed_tokens + repeat_tokens + big_tokens), 0) FROM session_waste",
        [],
        |r| r.get(0),
    )?;

    Ok(json!({
        "installed_at": installed,
        "identified_tokens": identified,
        "before": {"sessions": n_before, "context": ctx_before, "waste": waste_before, "rate": rb},
        "after": {"sessions": n_after, "context": ctx_after, "waste": waste_after, "rate": ra},
        "min_sessions_after": MIN_SESSIONS_AFTER,
        "saved_tokens": saved,
        "spend": spend,
        "spend_by_kind": by_kind,
        "weeks": weeks,
    }))
}
