//! Recherche plein texte dans le contenu des conversations (index FTS5).

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// Chaque mot devient une expression exacte (pas de syntaxe FTS à échapper) ; le dernier
/// accepte un préfixe, pour chercher pendant la frappe.
fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q.split_whitespace().map(|w| w.replace('"', "")).filter(|w| !w.is_empty()).collect();
    let n = words.len();
    (n > 0).then(|| {
        words
            .iter()
            .enumerate()
            .map(|(i, w)| if i + 1 == n { format!("\"{w}\"*") } else { format!("\"{w}\"") })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Conversations dont le contenu correspond, avec un extrait autour des mots trouvés
/// (encadrés par les caractères \u{1} et \u{2}, que l'interface met en surbrillance).
pub fn search(conn: &Connection, q: &str, limit: i64) -> Result<Value> {
    let Some(fq) = fts_query(q) else { return Ok(json!([])) };
    let mut st = conn.prepare(
        "WITH hits AS (
            SELECT session_id, snippet(texts, 3, char(1), char(2), '…', 14) AS snip, ts, role, rank
            FROM texts WHERE texts MATCH ?1 ORDER BY rank LIMIT 600)
         SELECT h.session_id, s.title, c.repo_id, h.snip, h.role, MAX(h.ts), COUNT(*)
         FROM hits h JOIN sessions s ON s.id = h.session_id LEFT JOIN cwd_map c ON c.cwd = s.cwd
         GROUP BY h.session_id ORDER BY MIN(h.rank) LIMIT ?2",
    )?;
    let rows: Vec<Value> = st
        .query_map(params![fq, limit], |r| {
            Ok(json!({
                "session_id": r.get::<_, String>(0)?, "title": r.get::<_, Option<String>>(1)?, "repo_id": r.get::<_, Option<i64>>(2)?,
                "snippet": r.get::<_, String>(3)?, "role": r.get::<_, String>(4)?, "ts": r.get::<_, Option<i64>>(5)?, "hits": r.get::<_, i64>(6)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    #[test]
    fn query_is_quoted_and_prefixed() {
        assert_eq!(super::fts_query("make unit"), Some("\"make\" \"unit\"*".into()));
        assert_eq!(super::fts_query("  "), None);
        assert_eq!(super::fts_query("a\"b"), Some("\"ab\"*".into()));
    }
}
