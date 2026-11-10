//! Questions annexes (`/btw`) et commandes `/` d'une conversation.
//!
//! Question annexe : posée à une discussion en direct par le protocole de Claude Code
//! (`control_request` / `side_question`), sinon à une conversation passée par une
//! reprise sans persistance (`--resume <id> --no-session-persistence`, sans outil) :
//! la conversation n'est jamais modifiée. Commandes : celles qu'annonce Claude Code à
//! l'ouverture, dans le dossier de la conversation (commandes et skills du projet compris).

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::{repos, review_dir, with_conn, ApiResult, App, ReviewTarget};

/// Commandes jamais proposées dans la saisie : elles ont leur propre bouton, ou n'ont pas de sens ici.
const HIDDEN: [&str; 2] = ["clear", "btw"];

#[derive(Deserialize)]
pub struct BtwReq {
    chat: Option<String>,
    session: Option<String>,
    question: String,
    #[serde(default)]
    history: Vec<Value>,
}

/// Historique accepté par Claude Code : `[{question, response}]`, des chaînes, borné.
fn clean_history(h: &[Value]) -> Value {
    Value::Array(
        h.iter()
            .rev()
            .take(10)
            .rev()
            .filter_map(|x| Some(json!({"question": x["question"].as_str()?, "response": x["response"].as_str()?})))
            .collect(),
    )
}

/// Conversation passée : reprise sans persistance, sans outil, dans son dossier.
async fn ask_past(sid: &str, cwd: &str, config_dir: Option<&str>, question: &str, history: &Value) -> Result<(String, i64, i64, Option<f64>)> {
    let mut prompt = String::from("Question annexe (/btw), posée à côté du travail en cours : réponds brièvement à partir de cette conversation, sans outil ni action.\n");
    for h in history.as_array().into_iter().flatten() {
        prompt += &format!("\nQuestion annexe précédente : {}\nTa réponse : {}\n", h["question"].as_str().unwrap_or(""), h["response"].as_str().unwrap_or(""));
    }
    prompt += &format!("\nQuestion : {question}");
    let mut cmd = Command::new("claude");
    cmd.args(["-p", "--output-format", "json", "--resume", sid, "--no-session-persistence", "--strict-mcp-config", "--tools", ""])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let default_dir = drifty_core::home().join(".claude");
    if let Some(d) = config_dir.filter(|d| std::path::Path::new(d) != default_dir) {
        cmd.env("CLAUDE_CONFIG_DIR", d);
    }
    let mut child = cmd.spawn().context("The side question cannot be asked because `claude` could not be launched")?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin.write_all(prompt.as_bytes()).await?;
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("The side question cannot be answered because Claude did not answer within 5 minutes."))??;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        anyhow::anyhow!("The side question cannot be answered because Claude said: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>())
    })?;
    if v["is_error"] == true {
        anyhow::bail!("The side question cannot be answered because Claude returned an error: {}", v["result"].as_str().unwrap_or(""));
    }
    let u = &v["usage"];
    let n = |k: &str| u[k].as_i64().unwrap_or(0);
    Ok((
        v["result"].as_str().unwrap_or("").trim().to_string(),
        n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
        n("output_tokens"),
        v["total_cost_usd"].as_f64(),
    ))
}

pub async fn btw(State(app): State<App>, Json(r): Json<BtwReq>) -> ApiResult {
    let question = r.question.trim().to_string();
    if question.is_empty() {
        return Err(anyhow::anyhow!("The side question cannot be asked because it is empty.").into());
    }
    let history = clean_history(&r.history);
    if let Some(id) = &r.chat {
        let chat = app.chats.get(id)?;
        let answer = chat.side_question(&question, history).await?;
        return Ok(Json(json!({"response": answer})));
    }
    let sid = r.session.clone().ok_or_else(|| anyhow::anyhow!("The side question cannot be asked because no conversation is named."))?;
    let (cwd, slot) = review_dir(&app, &ReviewTarget { chat: None, session: Some(sid.clone()), message: None }).await?;
    let config_dir = with_conn(&app, move |c| Ok(json!(repos::account_dir(c, slot.as_deref())?))).await?.0;
    let (text, i, o, cost) = ask_past(&sid, &cwd, config_dir.as_str(), &question, &history).await?;
    let _ = with_conn(&app, move |c| {
        drifty_core::ledger::record_spend(c, "btw", None, i, o, cost)?;
        Ok(json!({}))
    })
    .await?;
    Ok(Json(json!({"response": text})))
}

#[derive(Deserialize)]
pub struct CommandsQ {
    chat: Option<String>,
    session: Option<String>,
    /// Discussion pas encore lancée : le dossier et le compte du projet.
    repo: Option<i64>,
}

pub async fn commands(State(app): State<App>, Query(q): Query<CommandsQ>) -> ApiResult {
    let (cwd, slot) = match q.repo {
        Some(id) => {
            let v = with_conn(&app, move |c| {
                let (root, slot): (String, Option<String>) = c.query_row("SELECT root, slot FROM repos WHERE id = ?1", rusqlite::params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(json!([root, slot.or(drifty_core::db::default_slot(c)?)]))
            })
            .await?
            .0;
            (v[0].as_str().unwrap_or("").to_string(), v[1].as_str().map(str::to_string))
        }
        None => review_dir(&app, &ReviewTarget { chat: q.chat, session: q.session, message: None }).await?,
    };
    let config_dir = with_conn(&app, move |c| Ok(json!(repos::account_dir(c, slot.as_deref())?))).await?.0;
    let info = app.models.get(config_dir.as_str(), Some(&cwd)).await?;
    let list: Vec<Value> = info["commands"]
        .as_array()
        .into_iter()
        .flatten()
        // Les commandes internes (`__…`) ne sont pas faites pour être tapées.
        .filter(|c| c["name"].as_str().is_some_and(|n| !HIDDEN.contains(&n) && !n.starts_with("__")))
        .map(|c| json!({"name": c["name"], "description": c["description"], "hint": c["argumentHint"]}))
        .collect();
    Ok(Json(Value::Array(list)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn history_is_cleaned() {
        let h = vec![serde_json::json!({"question": "a", "response": "b"}), serde_json::json!({"question": 1}), serde_json::json!({"x": "y"})];
        assert_eq!(super::clean_history(&h), serde_json::json!([{"question": "a", "response": "b"}]));
    }
}
