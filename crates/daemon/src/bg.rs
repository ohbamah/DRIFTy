//! Tâches de fond des conversations : sous-agents lancés en arrière-plan et commandes shell
//! `run_in_background`, lus dans les transcripts (DRIFTy ou terminal). Une tâche commence à
//! son lancement (`run_in_background`, résultat `isAsync` / `teammate_spawned` /
//! `backgroundTaskId`) et finit à sa `<task-notification>` (même `tool-use-id`).
//! Lecture incrémentale (seuls les octets nouveaux d'un transcript sont lus) ; seuls les
//! transcripts écrits dans les 12 dernières heures sont regardés.
//!
//! Fins sans notification : une tâche arrêtée (`TaskStop`, `KillShell`) sort par son
//! identifiant ; une commande dont le fichier de sortie a disparu est finie ; une tâche
//! sans nouvelle depuis plus de 4 h est tenue pour finie (session close, équipier resté
//! inactif), car le transcript ne le dit pas.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::{with_conn, ApiResult, App};

const RECENT: Duration = Duration::from_secs(12 * 3600);
const STALE_MS: i64 = 4 * 3600 * 1000;

#[derive(Clone)]
struct Task {
    kind: &'static str, // agent | shell
    label: String,
    started: Option<String>,
    /// Lancé, mais pas encore confirmé « en arrière-plan » (sous-agent dont le résultat n'est pas lu).
    pending: bool,
    /// Identifiants par lesquels on peut l'arrêter (`backgroundTaskId`, `agentId`, nom d'équipier).
    ids: Vec<String>,
    /// Fichier de sortie d'une commande (disparu = commande finie).
    output: Option<String>,
}

#[derive(Default)]
struct FileState {
    offset: u64,
    /// Dernier horodatage lu : date des lignes qui n'en portent pas.
    last_ts: Option<String>,
    tasks: HashMap<String, Task>,
}

static FILES: Mutex<Option<HashMap<PathBuf, FileState>>> = Mutex::new(None);

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

/// Applique une ligne du transcript à l'état des tâches.
fn apply(st: &mut FileState, line: &str) {
    if let Some(ts) = between(line, "\"timestamp\":\"", "\"") {
        st.last_ts = Some(ts.to_string());
    }
    // Fin d'une tâche : notification (dans un message, une file d'attente ou une pièce jointe).
    if line.contains("<task-notification>") {
        // La ligne est du JSON : le texte de la notification y est échappé (\n, \").
        let text = line.replace("\\n", "\n").replace("\\\"", "\"");
        let mut rest = text.as_str();
        while let Some(i) = rest.find("<task-notification>") {
            let block = &rest[i..];
            let end = block.find("</task-notification>").map_or(block.len(), |j| j + 20);
            let b = &block[..end];
            let status = between(b, "<status>", "</status>").unwrap_or("");
            if let Some(id) = between(b, "<tool-use-id>", "</tool-use-id>") {
                if status != "running" && status != "started" {
                    st.tasks.remove(id);
                }
            }
            rest = &block[end..];
        }
    }
    let interesting = line.contains("\"tool_use\"") || line.contains("\"tool_result\"");
    if !interesting {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    let ts = v["timestamp"].as_str().map(str::to_string).or_else(|| st.last_ts.clone());
    let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { return };
    for b in blocks {
        match b["type"].as_str() {
            Some("tool_use") => {
                let (id, name, input) = (b["id"].as_str().unwrap_or(""), b["name"].as_str().unwrap_or(""), &b["input"]);
                let label = input["description"].as_str().or(input["command"].as_str()).unwrap_or(name).chars().take(120).collect::<String>();
                if name == "Bash" && input["run_in_background"] == true {
                    st.tasks.insert(id.into(), Task { kind: "shell", label, started: ts.clone(), pending: false, ids: vec![], output: None });
                } else if name == "Agent" || name == "Task" {
                    st.tasks.insert(id.into(), Task { kind: "agent", label, started: ts.clone(), pending: true, ids: vec![], output: None });
                } else if matches!(name, "TaskStop" | "KillShell" | "KillBash") {
                    // Arrêt explicite : par son identifiant de tâche, d'agent ou de shell.
                    let target = ["task_id", "shell_id", "agent_id", "id", "shell_id"].iter().find_map(|k| input[*k].as_str()).unwrap_or("");
                    if !target.is_empty() {
                        st.tasks.retain(|_, t| !t.ids.iter().any(|x| x == target));
                    }
                }
            }
            Some("tool_result") => {
                let id = b["tool_use_id"].as_str().unwrap_or("");
                let Some(t) = st.tasks.get_mut(id) else { continue };
                let r = &v["toolUseResult"];
                for k in ["backgroundTaskId", "agentId", "agent_id", "name", "teammate_id"] {
                    if let Some(x) = r[k].as_str() {
                        t.ids.push(x.to_string());
                    }
                }
                if t.kind == "shell" {
                    let text = b["content"].as_str().map(str::to_string).unwrap_or_else(|| b["content"].to_string());
                    t.output = text.split("written to: ").nth(1).and_then(|s| s.split(|c: char| c.is_whitespace() || c == '"').next()).map(|s| s.trim_end_matches('.').to_string());
                }
                if !t.pending {
                    continue;
                }
                // Sous-agent : en arrière-plan seulement si son résultat le dit ; sinon il a fini.
                let bg = r["isAsync"] == true || matches!(r["status"].as_str(), Some("async_launched" | "teammate_spawned"));
                if bg {
                    t.pending = false;
                } else {
                    st.tasks.remove(id);
                }
            }
            _ => {}
        }
    }
}

/// Lit ce qui a été ajouté au transcript depuis la dernière fois.
fn update(path: &PathBuf, st: &mut FileState) {
    let Ok(mut f) = std::fs::File::open(path) else { return };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < st.offset {
        *st = FileState::default(); // fichier réécrit : on repart de zéro
    }
    if f.seek(SeekFrom::Start(st.offset)).is_err() {
        return;
    }
    let mut r = BufReader::new(f);
    let mut line = String::new();
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if !line.ends_with('\n') {
                    break; // ligne en cours d'écriture : relue au prochain passage
                }
                st.offset += n as u64;
                apply(st, &line);
            }
        }
    }
}

pub async fn route(State(app): State<App>) -> ApiResult {
    let dirs: Vec<String> = with_conn(&app, |c| Ok(json!(drifty_core::db::accounts(c)?.into_iter().map(|a| a.config_dir).collect::<Vec<_>>())))
        .await?
        .0
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d.as_str().map(str::to_string))
        .collect();
    let out = tokio::task::spawn_blocking(move || {
        let now = SystemTime::now();
        let mut files = FILES.lock().unwrap();
        let files = files.get_or_insert_with(HashMap::new);
        let mut out = Map::new();
        for d in dirs {
            let Ok(projects) = std::fs::read_dir(PathBuf::from(&d).join("projects")) else { continue };
            for p in projects.flatten() {
                let Ok(entries) = std::fs::read_dir(p.path()) else { continue };
                for e in entries.flatten() {
                    let path = e.path();
                    if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let recent = e.metadata().and_then(|m| m.modified()).ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age < RECENT);
                    if !recent {
                        files.remove(&path);
                        continue;
                    }
                    let st = files.entry(path.clone()).or_default();
                    update(&path, st);
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let fresh = |t: &Task| {
                        t.started.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map_or(true, |d| now_ms - d.timestamp_millis() < STALE_MS)
                    };
                    let alive = |t: &Task| t.output.as_deref().map_or(true, |o| std::path::Path::new(o).exists());
                    let running: Vec<Value> = st
                        .tasks
                        .values()
                        .filter(|t| (!t.pending || t.kind == "agent") && fresh(t) && alive(t))
                        .map(|t| json!({"kind": t.kind, "label": t.label, "started": t.started}))
                        .collect();
                    if !running.is_empty() {
                        let sid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                        out.insert(sid, Value::Array(running));
                    }
                }
            }
        }
        Value::Object(out)
    })
    .await?;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    #[test]
    fn tasks_start_and_finish() {
        let mut st = super::FileState::default();
        super::apply(&mut st, r#"{"type":"assistant","timestamp":"t","message":{"content":[{"type":"tool_use","id":"a1","name":"Bash","input":{"command":"make","description":"Build","run_in_background":true}},{"type":"tool_use","id":"g1","name":"Agent","input":{"description":"Recherche"}},{"type":"tool_use","id":"g2","name":"Agent","input":{"description":"Court"}}]}}"#);
        super::apply(&mut st, r#"{"type":"user","toolUseResult":{"isAsync":true,"status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"g1","content":"ok"}]}}"#);
        super::apply(&mut st, r#"{"type":"user","toolUseResult":{"content":"fini"},"message":{"content":[{"type":"tool_result","tool_use_id":"g2","content":"fini"}]}}"#);
        assert_eq!(st.tasks.len(), 2, "le sous-agent synchrone a fini ; la commande et le sous-agent de fond tournent");
        super::apply(&mut st, r#"{"type":"queue-operation","content":"<task-notification>\n<task-id>x</task-id>\n<tool-use-id>a1</tool-use-id>\n<status>completed</status>\n</task-notification>"}"#);
        assert!(!st.tasks.contains_key("a1") && st.tasks.contains_key("g1"));
    }
}
