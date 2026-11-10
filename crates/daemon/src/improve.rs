//! Amélioration d'un prompt par Claude, à la demande. Appel minimal et isolé :
//! aucun outil, aucune personnalisation (`--safe-mode`), aucune session écrite
//! sur disque (sinon il apparaîtrait comme une conversation de l'utilisateur).
//! Son coût est enregistré comme dépense propre de DRIFTy.

use anyhow::{Context, Result};
use serde_json::Value;
use std::process::Stdio;
use tokio::process::Command;

const SYSTEM: &str = "Tu améliores des prompts destinés à Claude Code, un agent de programmation. \
Réécris le prompt de l'utilisateur pour qu'il soit clair, précis et actionnable : objectif explicite, \
contexte utile, contraintes, et un critère de fin vérifiable quand c'est pertinent. Garde la langue, \
l'intention et tous les détails fournis ; n'invente ni fichiers, ni faits, ni exigences. Reste concis : \
pas plus long que nécessaire. Réponds UNIQUEMENT avec le prompt amélioré, sans préambule ni guillemets.";

pub struct Improved {
    pub text: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: Option<f64>,
    pub ms: u128,
}

pub async fn run(text: &str, project: Option<&str>, config_dir: Option<&str>) -> Result<Improved> {
    if text.trim().is_empty() {
        anyhow::bail!("The prompt cannot be improved because it is empty.");
    }
    // Le nom du projet n'est PAS transmis : le modèle le recopiait dans le prompt (mesuré),
    // ce qui ajoute une information que l'utilisateur n'a pas écrite.
    let _ = project;
    let model = std::env::var("DRIFTY_IMPROVE_MODEL").unwrap_or_else(|_| "sonnet".into());
    oneshot(SYSTEM, text, config_dir, &model, "The prompt cannot be improved").await
}

/// Un appel isolé à Claude : dossier neutre, aucun outil, aucune personnalisation,
/// aucune session écrite. Sert à tout ce que DRIFTy demande pour lui-même.
pub async fn oneshot(system: &str, prompt: &str, config_dir: Option<&str>, model: &str, what: &str) -> Result<Improved> {
    let start = std::time::Instant::now();
    let dir = drifty_core::home().join(".drifty").join("improve");
    std::fs::create_dir_all(&dir)?;
    let mut cmd = Command::new("claude");
    cmd.args(["-p", "--output-format", "json", "--no-session-persistence", "--safe-mode", "--strict-mcp-config", "--tools", "", "--model", model])
        .args(["--system-prompt", system])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let default_dir = drifty_core::home().join(".claude");
    if let Some(d) = config_dir.filter(|d| std::path::Path::new(d) != default_dir) {
        cmd.env("CLAUDE_CONFIG_DIR", d);
    }
    let mut child = cmd.spawn().with_context(|| format!("{what} because `claude` could not be launched"))?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin.write_all(prompt.as_bytes()).await?;
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("{what} because Claude did not answer within 3 minutes."))??;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::anyhow!("{what} because Claude answered: {}", err.trim().chars().take(300).collect::<String>())
    })?;
    if v["is_error"].as_bool() == Some(true) {
        anyhow::bail!("{what} because Claude returned an error: {}", v["result"].as_str().unwrap_or(""));
    }
    let u = &v["usage"];
    let n = |k: &str| u[k].as_i64().unwrap_or(0);
    Ok(Improved {
        text: v["result"].as_str().unwrap_or("").trim().to_string(),
        input_tokens: n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
        output_tokens: n("output_tokens"),
        cost_usd: v["total_cost_usd"].as_f64(),
        ms: start.elapsed().as_millis(),
    })
}
