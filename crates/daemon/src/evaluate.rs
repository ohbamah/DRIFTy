//! Évaluation d'une conversation par un agent : il reçoit le DIGEST (chiffres,
//! causes, extraits, règles en place), jamais le transcript, et rend un rapport
//! bref et actionnable. Son coût est une dépense propre de DRIFTy.

use crate::improve;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const SYSTEM: &str = "Tu es expert de l'efficacité des agents de programmation (Claude Code). On te donne le résumé \
chiffré d'une conversation : temps, tokens, causes de lenteur et de gaspillage avec des extraits, recadrages de \
l'utilisateur, et les règles déjà présentes dans son CLAUDE.md. Rends un rapport en Markdown, en français, bref et \
concret, en trois sections : « Diagnostic » (3 points au plus, chacun appuyé sur un chiffre du résumé), \
« Améliorations » (actions précises : texte exact d'une règle à ajouter au CLAUDE.md si elle n'y est pas déjà, \
façon de formuler les demandes, organisation en worktrees ou sous-agents, choix du modèle), « Ce qui a bien marché » \
(1 ou 2 points). N'invente aucun fait absent du résumé. Pas d'introduction ni de conclusion.";

/// Le résumé envoyé à l'agent : compact, sans le texte des messages.
fn brief(conn: &Connection, sid: &str) -> Result<(String, Option<String>)> {
    let d = claudius_core::digest::digest(conn, sid)?;
    let s = claudius_core::query::session(conn, sid)?;
    let mut out = format!(
        "Conversation : {}\nCoût : {}\nAppels au modèle : {}, appels d'outil : {}, échecs évitables : {}, sous-agents : {}, compactions : {}, interruptions : {}\n\
         Contexte : départ {} tokens, max {} tokens, cache {:.1} %\nTemps : modèle {} s, outils {} s, attente utilisateur {} s\n\nCauses :\n",
        s["title"].as_str().unwrap_or(sid),
        s["cost_usd"].as_f64().map_or("inconnu".into(), |c| format!("${c:.2}")),
        d["counts"]["api_calls"], d["counts"]["tool_calls"], d["counts"]["tool_errors"], d["counts"]["subagents"],
        d["counts"]["compactions"], d["counts"]["interrupts"],
        d["tokens"]["first_context"], d["tokens"]["max_context"], d["tokens"]["cache_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        d["time"]["model_ms"].as_i64().unwrap_or(0) / 1000, d["time"]["tool_ms"].as_i64().unwrap_or(0) / 1000, d["time"]["human_ms"].as_i64().unwrap_or(0) / 1000,
    );
    for c in d["causes"].as_array().into_iter().flatten().take(10) {
        out += &format!("- {} ({}, {} ms, {} tokens) : {}\n", c["title"].as_str().unwrap_or(""), c["optimisable"].as_str().unwrap_or(""),
            c["ms"], c["tokens"], c["why"].as_str().unwrap_or(""));
        for it in c["items"].as_array().into_iter().flatten().take(4) {
            let line = ["target", "error", "text", "name"].iter().filter_map(|k| it[*k].as_str()).collect::<Vec<_>>().join(" | ");
            if !line.is_empty() {
                out += &format!("    · {}\n", line.chars().take(200).collect::<String>());
            }
        }
    }
    let corrections: Vec<String> = d["corrections"].as_array().into_iter().flatten().take(6).filter_map(|c| c["text"].as_str().map(|t| t.chars().take(200).collect())).collect();
    if !corrections.is_empty() {
        out += &format!("\nRecadrages de l'utilisateur :\n- {}\n", corrections.join("\n- "));
    }
    // Règles déjà en place dans le CLAUDE.md du projet (pour ne pas les reproposer).
    if let Some(repo) = s["repo_id"].as_i64() {
        if let Ok(rules) = claudius_core::rule_effects::list(conn, repo) {
            let texts: Vec<&str> = rules["rules"].as_array().into_iter().flatten().filter_map(|r| r["rule"].as_str()).collect();
            if !texts.is_empty() {
                out += &format!("\nRègles déjà dans le CLAUDE.md :\n- {}\n", texts.join("\n- "));
            }
        }
    }
    let slot = s["slot"].as_str().map(str::to_string);
    let dir = claudius_core::repos::account_dir(conn, slot.as_deref())?;
    Ok((out, dir))
}

pub fn prepare(conn: &Connection, sid: &str) -> Result<(String, Option<String>)> {
    brief(conn, sid)
}

pub async fn run(prompt: &str, config_dir: Option<&str>) -> Result<improve::Improved> {
    let model = std::env::var("CLAUDIUS_EVAL_MODEL").unwrap_or_else(|_| "sonnet".into());
    improve::oneshot(SYSTEM, prompt, config_dir, &model, "The conversation cannot be evaluated").await
}

pub fn save(conn: &Connection, sid: &str, r: &improve::Improved) -> Result<Value> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    conn.execute(
        "INSERT INTO evaluations(session_id, ts, report, input_tokens, output_tokens, cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![sid, now, r.text, r.input_tokens, r.output_tokens, r.cost_usd],
    )?;
    claudius_core::ledger::record_spend(conn, "evaluate", Some(sid), r.input_tokens, r.output_tokens, r.cost_usd)?;
    latest(conn, sid)
}

pub fn latest(conn: &Connection, sid: &str) -> Result<Value> {
    Ok(conn
        .query_row(
            "SELECT ts, report, input_tokens, output_tokens, cost_usd FROM evaluations WHERE session_id = ?1 ORDER BY ts DESC LIMIT 1",
            params![sid],
            |r| Ok(json!({"ts": r.get::<_, i64>(0)?, "report": r.get::<_, String>(1)?, "tokens": r.get::<_, i64>(2)? + r.get::<_, i64>(3)?, "cost_usd": r.get::<_, Option<f64>>(4)?})),
        )
        .unwrap_or(Value::Null))
}
