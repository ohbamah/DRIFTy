//! Commandes d'un projet (test, lancement) : proposées par l'IA d'après un
//! résumé du dépôt, modifiables, exécutées dans le dossier d'une conversation
//! avec une sortie lue au fil de l'eau.

use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const MAX_OUTPUT: usize = 1_000_000;

/// Résumé du dépôt pour l'analyse des lancements (borné, sans contenu sensible).
pub fn repo_summary(root: &str) -> String {
    let p = Path::new(root);
    let mut out = String::new();
    let mut entries: Vec<String> = std::fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.') || n == ".env.example").collect())
        .unwrap_or_default();
    entries.sort();
    out += &format!("Fichiers à la racine : {}\n", entries.join(", "));
    let read = |name: &str, max: usize| std::fs::read_to_string(p.join(name)).ok().map(|s| s.chars().take(max).collect::<String>());
    for mk in ["Makefile", "makefile", "GNUmakefile"] {
        if let Some(m) = read(mk, 200_000) {
            let targets: Vec<&str> = m.lines().filter(|l| !l.starts_with(['\t', '#', '.', ' ']) && l.contains(':') && !l.contains(":=") && !l.contains('=')).map(|l| l.split(':').next().unwrap_or("")).take(40).collect();
            out += &format!("Cibles du {mk} : {}\n", targets.join(", "));
        }
    }
    if let Some(pkg) = read("package.json", 50_000) {
        if let Ok(v) = serde_json::from_str::<Value>(&pkg) {
            out += &format!("Scripts package.json : {}\n", v["scripts"]);
        }
    }
    for f in ["Procfile", "Procfile.dev", "justfile", "Taskfile.yml"] {
        if let Some(c) = read(f, 2_000) {
            out += &format!("{f} :\n{c}\n");
        }
    }
    for f in ["Cargo.toml", "pyproject.toml", "go.mod", "Gemfile", "pom.xml", "build.gradle"] {
        if p.join(f).exists() {
            out += &format!("Présent : {f}\n");
        }
    }
    if let Some(md) = read("CLAUDE.md", 200_000) {
        let hints: Vec<&str> = md
            .lines()
            .filter(|l| ["make ", "cargo ", "npm ", "pnpm ", "yarn ", "pytest", "go test", "foreman", "bun ", "./"].iter().any(|k| l.contains(k)))
            .take(40)
            .collect();
        if !hints.is_empty() {
            out += &format!("Lignes du CLAUDE.md citant des commandes :\n{}\n", hints.join("\n"));
        }
    }
    out
}

pub const ANALYZE_SYSTEM: &str = "On te donne le résumé d'un dépôt de code. Déduis la commande à lancer pour exécuter ses TESTS \\
(la plus utile au quotidien, rapide si le dépôt le précise) et celle pour LANCER l'application (vide si ce n'est pas une \\
application). Préfère ce que le dépôt documente (CLAUDE.md, Makefile, scripts). Réponds UNIQUEMENT par un objet JSON : \\
{\"test\": \"...\", \"run\": \"...\", \"why\": \"une phrase\"}.";

/// Extrait l'objet JSON de la réponse de l'IA.
pub fn parse_analysis(text: &str) -> Result<Value> {
    let start = text.find('{').ok_or_else(|| anyhow::anyhow!("The launch analysis returned no JSON."))?;
    let end = text.rfind('}').ok_or_else(|| anyhow::anyhow!("The launch analysis returned no JSON."))?;
    let v: Value = serde_json::from_str(&text[start..=end])?;
    Ok(json!({"test": v["test"].as_str().unwrap_or(""), "run": v["run"].as_str().unwrap_or(""), "why": v["why"].as_str().unwrap_or("")}))
}

// ------------------------------------------------------------ exécution

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[derive(Default)]
pub struct Job {
    pub output: String,
    pub done: bool,
    pub code: Option<i32>,
    pub command: String,
    kill: Option<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Default, Clone)]
pub struct Jobs {
    jobs: Arc<Mutex<HashMap<String, Job>>>,
}

impl Jobs {
    pub fn start(&self, dir: &str, command: &str, env: &[(String, String)]) -> Result<String> {
        if command.trim().is_empty() {
            anyhow::bail!("The command cannot run because it is empty.");
        }
        if !Path::new(dir).is_dir() {
            anyhow::bail!("The command cannot run because the folder `{dir}` no longer exists.");
        }
        let id = format!("j{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        // Shell de connexion (PATH et outils de l'utilisateur) dans un PSEUDO-TERMINAL : les
        // outils gardent leurs couleurs, et les watchers (tailwind --watch, esbuild --watch…)
        // restent en vie, car leur entrée reste ouverte (ils s'arrêtent quand elle se ferme).
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = Command::new("script");
            c.args(["-q", "/dev/null", &shell, "-lc", command]);
            c
        } else {
            let mut c = Command::new("script");
            c.args(["-qfec", &format!("{shell} -lc {}", shell_quote(command)), "/dev/null"]);
            c
        };
        let mut child = cmd
            .current_dir(dir)
            .env("FORCE_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .env("TERM", "xterm-256color")
            .env("COLUMNS", "160")
            .envs(env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        // L'entrée reste ouverte tant que la commande tourne (gardée par la tâche ci-dessous).
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        self.jobs.lock().unwrap().insert(id.clone(), Job { command: command.to_string(), kill: Some(tx), ..Default::default() });
        let jobs = self.jobs.clone();
        let jid = id.clone();
        tokio::spawn(async move {
            let _keep_stdin_open = stdin;
            let mut buf = [0u8; 8192];
            loop {
                tokio::select! {
                    n = stdout.read(&mut buf) => {
                        let n = match n { Ok(0) | Err(_) => break, Ok(n) => n };
                        let mut j = jobs.lock().unwrap();
                        if let Some(job) = j.get_mut(&jid) {
                            job.output.push_str(&String::from_utf8_lossy(&buf[..n]));
                            // On garde la fin : c'est elle qui dit si ça passe.
                            if job.output.len() > MAX_OUTPUT {
                                let cut = job.output.len() - MAX_OUTPUT;
                                let at = (cut..job.output.len()).find(|i| job.output.is_char_boundary(*i)).unwrap_or(cut);
                                job.output.drain(..at);
                            }
                        }
                    }
                    _ = &mut rx => { let _ = child.kill().await; break; }
                }
            }
            let code = child.wait().await.ok().and_then(|s| s.code());
            if let Some(job) = jobs.lock().unwrap().get_mut(&jid) {
                job.done = true;
                job.code = code;
            }
        });
        Ok(id)
    }

    /// Attend la fin d'une exécution ; rend son code (`None` : tuée, ou inconnue).
    pub async fn wait(&self, id: &str) -> Option<i32> {
        loop {
            match self.jobs.lock().unwrap().get(id) {
                Some(j) if j.done => return j.code,
                None => return None,
                _ => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Sortie à partir d'un décalage (lecture au fil de l'eau).
    pub fn read(&self, id: &str, from: usize) -> Result<Value> {
        let j = self.jobs.lock().unwrap();
        let job = j.get(id).ok_or_else(|| anyhow::anyhow!("The run `{id}` cannot be found."))?;
        let from = from.min(job.output.len());
        let from = (from..=job.output.len()).find(|i| job.output.is_char_boundary(*i)).unwrap_or(job.output.len());
        Ok(json!({"output": &job.output[from..], "next": job.output.len(), "done": job.done, "code": job.code, "command": job.command}))
    }

    pub fn stop(&self, id: &str) -> Result<()> {
        let mut j = self.jobs.lock().unwrap();
        let job = j.get_mut(id).ok_or_else(|| anyhow::anyhow!("The run `{id}` cannot be found."))?;
        if let Some(k) = job.kill.take() {
            let _ = k.send(());
        }
        Ok(())
    }
}
