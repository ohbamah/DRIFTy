//! Relevé d'usage sans session : `claude -p /usage` (commande locale de Claude Code, aucun
//! coût), lancé avec le dossier de chaque compte. Remplace l'attente d'une statusline, qui
//! ne tourne que dans une session interactive ouverte sur ce compte. Une fois par minute au
//! plus et par compte.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::process::Command;

use crate::{with_conn, ApiResult, App};

static LAST: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

async fn read_one(config_dir: &str) -> Option<String> {
    let mut cmd = Command::new("claude");
    cmd.args(["-p", "/usage", "--output-format", "json", "--no-session-persistence"])
        .current_dir(drifty_core::home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if std::path::Path::new(config_dir) != drifty_core::home().join(".claude") {
        cmd.env("CLAUDE_CONFIG_DIR", config_dir);
    }
    let out = tokio::time::timeout(Duration::from_secs(30), cmd.output()).await.ok()?.ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v["result"].as_str().map(str::to_string)
}

pub async fn refresh(State(app): State<App>) -> ApiResult {
    let accounts = with_conn(&app, |c| Ok(serde_json::to_value(drifty_core::db::accounts(c)?)?)).await?.0;
    let mut done = Vec::new();
    for a in accounts.as_array().into_iter().flatten().filter(|a| a["enabled"] == true) {
        let (slot, dir) = (a["slot"].as_str().unwrap_or("").to_string(), a["config_dir"].as_str().unwrap_or("").to_string());
        {
            let mut last = LAST.lock().unwrap();
            let m = last.get_or_insert_with(HashMap::new);
            if m.get(&slot).is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
                continue;
            }
            m.insert(slot.clone(), Instant::now());
        }
        let Some(text) = read_one(&dir).await else { continue };
        let Some(mut v) = drifty_core::usage::parse_usage_text(&text, chrono::Local::now()) else { continue };
        // Le modèle affiché reste celui du dernier relevé de la statusline.
        if let Some(m) = drifty_core::usage::summary_model(&slot) {
            v["model"] = json!({"display_name": m});
        }
        if drifty_core::usage::record(&slot, &serde_json::to_vec(&v).unwrap_or_default()).unwrap_or(false) {
            done.push(slot);
        }
    }
    Ok(Json(json!({"refreshed": done})))
}
