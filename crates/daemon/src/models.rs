//! Modèles et commandes `/` disponibles, demandés à Claude Code lui-même : sa réponse
//! d'initialisation (protocole stream-json) contient `models` pour le compte courant et
//! `commands` pour le dossier (commandes et skills du projet compris). Aucun modèle
//! n'est appelé (zéro token). Mise en cache 10 min, par compte et par dossier.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const TTL: Duration = Duration::from_secs(600);

#[derive(Default)]
pub struct Cache {
    by_dir: Mutex<HashMap<String, (Instant, Value)>>,
}

impl Cache {
    /// `{models, commands}` ; `cwd` absent = dossier neutre (modèles seuls utiles).
    pub async fn get(&self, config_dir: Option<&str>, cwd: Option<&str>) -> Result<Value> {
        let key = format!("{}|{}", config_dir.unwrap_or(""), cwd.unwrap_or(""));
        if let Some((at, v)) = self.by_dir.lock().unwrap().get(&key) {
            if at.elapsed() < TTL {
                return Ok(v.clone());
            }
        }
        let v = fetch(config_dir, cwd).await?;
        self.by_dir.lock().unwrap().insert(key, (Instant::now(), v.clone()));
        Ok(v)
    }
}

async fn fetch(config_dir: Option<&str>, cwd: Option<&str>) -> Result<Value> {
    let neutral = drifty_core::home().join(".drifty").join("improve");
    std::fs::create_dir_all(&neutral)?;
    let dir = cwd.map(std::path::PathBuf::from).filter(|p| p.is_dir()).unwrap_or(neutral);
    let mut cmd = Command::new("claude");
    cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--no-session-persistence"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let default_dir = drifty_core::home().join(".claude");
    if let Some(d) = config_dir.filter(|d| std::path::Path::new(d) != default_dir) {
        cmd.env("CLAUDE_CONFIG_DIR", d);
    }
    let mut child = cmd.spawn().context("The model list cannot be read because `claude` could not be launched")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    stdin
        .write_all(format!("{}\n", json!({"type": "control_request", "request_id": "models", "request": {"subtype": "initialize"}})).as_bytes())
        .await?;
    let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    let read = async {
        while let Some(line) = lines.next_line().await? {
            let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
            if ev["type"] == "control_response" && ev.pointer("/response/request_id") == Some(&json!("models")) {
                return Ok::<Value, anyhow::Error>(ev.pointer("/response/response").cloned().unwrap_or(json!({})));
            }
        }
        anyhow::bail!("The model list cannot be read because Claude Code closed without answering.")
    };
    let init = tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .map_err(|_| anyhow::anyhow!("The model list cannot be read because Claude Code did not answer within 30 s."))??;
    let _ = child.kill().await;
    // On ne garde que ce que l'interface affiche.
    let models = Value::Array(
        init["models"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                json!({
                    "value": m["value"], "resolved": m["resolvedModel"], "name": m["displayName"], "description": m["description"],
                })
            })
            .collect(),
    );
    Ok(json!({"models": models, "commands": init["commands"].clone()}))
}
