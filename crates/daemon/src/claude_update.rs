//! Mise à jour de Claude Code : version installée, dernière version publiée (registre npm,
//! comme Claude Code lui-même), et la commande qui convient à la façon dont il est installé
//! (cask Homebrew, paquet npm global, installeur natif). Vérifié au plus une fois par heure.

use anyhow::Result;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{ApiResult, App};

static CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
const TTL: Duration = Duration::from_secs(3600);

fn out(prog: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(prog).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `2.1.322` < `2.1.330` (comparaison numérique, partie par partie).
fn newer(latest: &str, installed: &str) -> bool {
    let parse = |v: &str| v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(latest) > parse(installed)
}

/// La commande de mise à jour, d'après l'emplacement réel du binaire `claude`.
fn command_for(path: &str) -> String {
    if let Some(rest) = path.split("/Caskroom/").nth(1) {
        let cask = rest.split('/').next().unwrap_or("claude-code");
        return format!("brew upgrade --cask {cask}");
    }
    if path.contains("node_modules/@anthropic-ai/claude-code") {
        return "npm install -g @anthropic-ai/claude-code@latest".into();
    }
    "claude update".into()
}

fn check() -> Result<Value> {
    let installed = out("claude", &["--version"]).and_then(|v| v.split_whitespace().next().map(str::to_string)).unwrap_or_default();
    let path = out("/bin/sh", &["-lc", "command -v claude"]).and_then(|p| std::fs::canonicalize(p).ok()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let latest = out("curl", &["-s", "-m", "8", "https://registry.npmjs.org/@anthropic-ai/claude-code/latest"])
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["version"].as_str().map(str::to_string))
        .unwrap_or_default();
    Ok(json!({
        "installed": installed, "latest": latest, "command": command_for(&path),
        "outdated": !installed.is_empty() && !latest.is_empty() && newer(&latest, &installed),
    }))
}

pub async fn route(State(_): State<App>) -> ApiResult {
    if let Some((at, v)) = CACHE.lock().unwrap().as_ref() {
        if at.elapsed() < TTL {
            return Ok(Json(v.clone()));
        }
    }
    let v = tokio::task::spawn_blocking(check).await??;
    *CACHE.lock().unwrap() = Some((Instant::now(), v.clone()));
    Ok(Json(v))
}

/// Lance la mise à jour (commande déterminée ici, jamais fournie par la page) ; sortie suivie
/// comme une commande du projet.
pub async fn run(State(app): State<App>) -> ApiResult {
    let v = tokio::task::spawn_blocking(check).await??;
    let cmd = v["command"].as_str().unwrap_or("claude update").to_string();
    *CACHE.lock().unwrap() = None; // relu après la mise à jour
    let home = drifty_core::home().to_string_lossy().into_owned();
    let id = app.jobs.start(&home, &cmd, &[])?;
    Ok(Json(json!({"id": id, "command": cmd})))
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_and_commands() {
        assert!(super::newer("2.1.330", "2.1.322") && !super::newer("2.1.322", "2.1.322") && super::newer("2.2.0", "2.1.999"));
        assert_eq!(super::command_for("/opt/homebrew/Caskroom/claude-code@latest/2.1.322/claude"), "brew upgrade --cask claude-code@latest");
        assert_eq!(super::command_for("/Users/x/.local/bin/claude"), "claude update");
    }
}
