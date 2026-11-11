//! Tâches de fond des conversations : sous-agents lancés en arrière-plan, commandes shell
//! `run_in_background` et surveillances (`Monitor`, avec la tâche qu'elles surveillent quand
//! leur commande cite son fichier de sortie), lus dans les transcripts (DRIFTy ou terminal). Une tâche commence à
//! son lancement (`run_in_background`, résultat `isAsync` / `teammate_spawned` /
//! `backgroundTaskId`) et finit à sa `<task-notification>` (même `tool-use-id`).
//! Lecture incrémentale (seuls les octets nouveaux d'un transcript sont lus) ; seuls les
//! transcripts écrits dans les 12 dernières heures sont regardés.
//!
//! Fins sans notification : une tâche arrêtée (`TaskStop`, `KillShell`) sort par son
//! identifiant ; une commande dont le fichier de sortie a disparu est finie ; une tâche
//! sans nouvelle depuis plus de 4 h est tenue pour finie (session close, équipier resté
//! inactif), car le transcript ne le dit pas.
//!
//! Tour en cours (`busy`) : Claude écrit ou réfléchit. Un tour commence à tout message
//! `user` (demande, résultat d'outil, notification de tâche) et finit à `turn_duration`, à
//! un message `assistant` `end_turn` ou à une interruption. Un transcript resté muet plus de
//! `BUSY_SILENCE` n'est plus tenu pour actif (session tuée en plein tour). Ne démarrent PAS de
//! tour : les messages méta et ce que le terminal écrit sans le donner à Claude (commandes
//! locales comme `/exit`, leur sortie, les commandes `!`).
//!
//! Attente (`waiting`) : une question (`AskUserQuestion`) ou un plan (`ExitPlanMode`) posé et
//! encore sans résultat. Les autorisations du terminal ne laissent pas de trace distincte d'un
//! outil qui tourne : elles ne sont pas vues.

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
/// Silence au-delà duquel un tour « en cours » est tenu pour mort (un outil au premier plan
/// dure au plus 10 min).
const BUSY_SILENCE: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
struct Task {
    kind: &'static str, // agent | shell | monitor
    label: String,
    started: Option<String>,
    /// Lancé, mais pas encore confirmé « en arrière-plan » (sous-agent dont le résultat n'est pas lu).
    pending: bool,
    /// Identifiants par lesquels on peut l'arrêter (`backgroundTaskId`, `agentId`, nom d'équipier).
    ids: Vec<String>,
    /// Fichier de sortie d'une commande (disparu = commande finie).
    output: Option<String>,
    /// Surveillance : identifiants des tâches surveillées (`tasks/<id>.output` dans sa commande)
    /// et échéance (`timeout_ms`, sauf surveillance persistante).
    watches: Vec<String>,
    until: Option<i64>,
}

#[derive(Default)]
struct FileState {
    offset: u64,
    /// Dernier horodatage lu : date des lignes qui n'en portent pas.
    last_ts: Option<String>,
    tasks: HashMap<String, Task>,
    /// Un tour de Claude est en cours (voir l'en-tête).
    busy: bool,
    /// Questions et plans posés à l'utilisateur, sans réponse encore (identifiants d'appel).
    asking: std::collections::HashSet<String>,
}

static FILES: Mutex<Option<HashMap<PathBuf, FileState>>> = Mutex::new(None);

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

/// Début et fin de tour, d'après le type de PREMIER niveau de la ligne (un résultat d'outil
/// ou une requête archivée peut contenir ces mots plus bas : on ne cherche pas dans le texte).
fn turn_state(st: &mut FileState, line: &str) {
    #[derive(serde::Deserialize)]
    struct Msg {
        stop_reason: Option<String>,
        content: Option<Value>,
    }
    #[derive(serde::Deserialize)]
    struct Head {
        #[serde(rename = "type")]
        t: Option<String>,
        subtype: Option<String>,
        message: Option<Msg>,
        #[serde(rename = "isMeta", default)]
        is_meta: bool,
    }
    // Filtre bon marché avant décodage : seules ces lignes peuvent changer l'état.
    if !(line.contains("\"user\"") || line.contains("turn_duration") || line.contains("end_turn")) {
        return;
    }
    let Ok(h) = serde_json::from_str::<Head>(line) else { return };
    match (h.t.as_deref(), h.subtype.as_deref()) {
        (Some("user"), _) => {
            if h.is_meta {
                return;
            }
            let c = h.message.and_then(|m| m.content);
            let text = match &c {
                Some(Value::String(s)) => s.as_str(),
                Some(Value::Array(a)) => a.first().and_then(|b| b["text"].as_str()).unwrap_or(""),
                _ => "",
            };
            // Écrit par le terminal, pas envoyé à Claude : aucun tour ne commence.
            const LOCAL: [&str; 5] = ["<command-name>", "<local-command-", "<bash-input>", "<bash-stdout>", "<bash-stderr>"];
            if LOCAL.iter().any(|p| text.trim_start().starts_with(p)) {
                return;
            }
            st.busy = !text.starts_with("[Request interrupted");
            if !st.busy {
                st.asking.clear();
            }
        }
        (Some("system"), Some("turn_duration")) => {
            st.busy = false;
            st.asking.clear();
        }
        (Some("assistant"), _) if h.message.and_then(|m| m.stop_reason).as_deref() == Some("end_turn") => st.busy = false,
        _ => {}
    }
}

/// Applique une ligne du transcript à l'état des tâches.
fn apply(st: &mut FileState, line: &str) {
    if let Some(ts) = between(line, "\"timestamp\":\"", "\"") {
        st.last_ts = Some(ts.to_string());
    }
    turn_state(st, line);
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
                    st.tasks.insert(id.into(), Task { kind: "shell", label, started: ts.clone(), pending: false, ids: vec![], output: None, watches: vec![], until: None });
                } else if name == "AskUserQuestion" || name == "ExitPlanMode" {
                    st.asking.insert(id.to_string());
                } else if name == "Agent" || name == "Task" {
                    st.tasks.insert(id.into(), Task { kind: "agent", label, started: ts.clone(), pending: true, ids: vec![], output: None, watches: vec![], until: None });
                } else if name == "Monitor" {
                    let cmd = input["command"].as_str().unwrap_or("");
                    let watches = cmd.split("tasks/").skip(1).filter_map(|s| s.split(".output").next()).filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric())).map(str::to_string).collect();
                    let start = ts.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis());
                    let until = if input["persistent"] == true { None } else { start.zip(input["timeout_ms"].as_i64()).map(|(a, t)| a + t) };
                    st.tasks.insert(id.into(), Task { kind: "monitor", label, started: ts.clone(), pending: false, ids: vec![], output: None, watches, until });
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
                st.asking.remove(id);
                let Some(t) = st.tasks.get_mut(id) else { continue };
                let r = &v["toolUseResult"];
                for k in ["backgroundTaskId", "agentId", "agent_id", "name", "teammate_id", "taskId"] {
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
        let mut busy: Vec<String> = vec![];
        let mut waiting: Vec<String> = vec![];
        for d in dirs {
            let Ok(projects) = std::fs::read_dir(PathBuf::from(&d).join("projects")) else { continue };
            for p in projects.flatten() {
                let Ok(entries) = std::fs::read_dir(p.path()) else { continue };
                for e in entries.flatten() {
                    let path = e.path();
                    if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|m| now.duration_since(m).ok());
                    let recent = age.is_some_and(|age| age < RECENT);
                    if !recent {
                        files.remove(&path);
                        continue;
                    }
                    let st = files.entry(path.clone()).or_default();
                    update(&path, st);
                    let sid = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                    if !st.asking.is_empty() {
                        waiting.push(sid.clone()); // attend l'utilisateur, sans limite de durée
                    } else if st.busy && age.is_some_and(|a| a < BUSY_SILENCE) {
                        busy.push(sid.clone());
                    }
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let fresh = |t: &Task| {
                        t.started.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map_or(true, |d| now_ms - d.timestamp_millis() < STALE_MS)
                    };
                    let alive = |t: &Task| t.output.as_deref().map_or(true, |o| std::path::Path::new(o).exists()) && t.until.map_or(true, |u| now_ms < u);
                    // Ce que surveille une surveillance : la tâche dont elle lit le fichier de sortie.
                    let watched = |t: &Task| -> Vec<String> {
                        t.watches.iter().filter_map(|w| st.tasks.values().find(|o| o.ids.iter().any(|x| x == w)).map(|o| o.label.clone())).collect()
                    };
                    let running: Vec<Value> = st
                        .tasks
                        .values()
                        .filter(|t| (!t.pending || t.kind == "agent") && fresh(t) && alive(t))
                        .map(|t| json!({"kind": t.kind, "label": t.label, "started": t.started, "watches": watched(t)}))
                        .collect();
                    if !running.is_empty() {
                        out.insert(sid, Value::Array(running));
                    }
                }
            }
        }
        json!({"tasks": out, "busy": busy, "waiting": waiting})
    })
    .await?;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    #[test]
    fn turn_busy_state() {
        let mut st = super::FileState::default();
        super::apply(&mut st, r#"{"type":"user","message":{"role":"user","content":"Salut"}}"#);
        assert!(st.busy);
        super::apply(&mut st, r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"thinking"}]}}"#);
        assert!(st.busy);
        super::apply(&mut st, r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Fini"}]}}"#);
        assert!(!st.busy);
        super::apply(&mut st, r#"{"parentUuid":"x","isSidechain":false,"type":"user","message":{"content":[{"type":"tool_result"}]}}"#);
        assert!(st.busy);
        // Un résultat d'outil qui CITE ces marqueurs ne change rien.
        super::apply(&mut st, r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"[Request interrupted by user] end_turn \"type\":\"assistant\""}]}}"#);
        assert!(st.busy);
        super::apply(&mut st, r#"{"type":"api-request-blob","message":{"messages":[{"type":"assistant","stop_reason":"end_turn"}]}}"#);
        assert!(st.busy);
        super::apply(&mut st, r#"{"type":"system","subtype":"turn_duration","durationMs":5}"#);
        assert!(!st.busy);
        super::apply(&mut st, r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#);
        assert!(!st.busy);
    }

    #[test]
    fn local_commands_do_not_start_a_turn() {
        // Fin réelle d'une session du terminal : réponse finie, puis `/exit` (mesuré le 2026-11-10).
        let mut st = super::FileState::default();
        for l in [
            r#"{"type":"system","subtype":"turn_duration"}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>The command below…</local-command-caveat>"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/exit</command-name>\n<command-message>exit</command-message>"}}"#,
            r#"{"type":"user","message":{"content":"<local-command-stdout>(no content)</local-command-stdout>"}}"#,
            r#"{"type":"user","message":{"content":"<bash-input>ls</bash-input>"}}"#,
            r#"{"type":"user","message":{"content":"<bash-stdout>a</bash-stdout><bash-stderr></bash-stderr>"}}"#,
        ] {
            super::apply(&mut st, l);
        }
        assert!(!st.busy);
        // Une notification de tâche, elle, fait répondre Claude.
        super::apply(&mut st, r#"{"type":"user","message":{"content":"<task-notification><status>completed</status></task-notification>"}}"#);
        assert!(st.busy);
    }

    #[test]
    fn question_waits_until_answered() {
        let mut st = super::FileState::default();
        super::apply(&mut st, r#"{"type":"user","message":{"content":"Pose-moi une question"}}"#);
        super::apply(&mut st, r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{}}]}}"#);
        assert!(st.asking.contains("q1"));
        super::apply(&mut st, r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"q1","content":"Rouge"}]}}"#);
        assert!(st.asking.is_empty() && st.busy);
        // Plan posé puis tour interrompu : plus d'attente.
        super::apply(&mut st, r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"p1","name":"ExitPlanMode","input":{}}]}}"#);
        assert!(st.asking.contains("p1"));
        super::apply(&mut st, r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#);
        assert!(st.asking.is_empty() && !st.busy);
    }

    #[test]
    fn tasks_start_and_finish() {
        let mut st = super::FileState::default();
        super::apply(&mut st, r#"{"type":"assistant","timestamp":"t","message":{"content":[{"type":"tool_use","id":"a1","name":"Bash","input":{"command":"make","description":"Build","run_in_background":true}},{"type":"tool_use","id":"g1","name":"Agent","input":{"description":"Recherche"}},{"type":"tool_use","id":"g2","name":"Agent","input":{"description":"Court"}}]}}"#);
        super::apply(&mut st, r#"{"type":"user","toolUseResult":{"isAsync":true,"status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"g1","content":"ok"}]}}"#);
        super::apply(&mut st, r#"{"type":"user","toolUseResult":{"content":"fini"},"message":{"content":[{"type":"tool_result","tool_use_id":"g2","content":"fini"}]}}"#);
        assert_eq!(st.tasks.len(), 2, "le sous-agent synchrone a fini ; la commande et le sous-agent de fond tournent");
        super::apply(&mut st, r#"{"type":"queue-operation","content":"<task-notification>\n<task-id>x</task-id>\n<tool-use-id>a1</tool-use-id>\n<status>completed</status>\n</task-notification>"}"#);
        assert!(!st.tasks.contains_key("a1") && st.tasks.contains_key("g1"));
        // Surveillance : lancée, liée à la commande qu'elle lit, finie à sa notification.
        super::apply(&mut st, r#"{"type":"assistant","timestamp":"2026-11-10T10:00:00Z","message":{"content":[{"type":"tool_use","id":"m1","name":"Monitor","input":{"description":"fin des specs","timeout_ms":600000,"command":"until grep -q ok /tmp/x/tasks/bk1.output; do sleep 2; done"}}]}}"#);
        assert_eq!(st.tasks["m1"].watches, vec!["bk1".to_string()]);
        assert!(st.tasks["m1"].until.is_some());
        super::apply(&mut st, r#"{"type":"queue-operation","content":"<task-notification>
<task-id>y</task-id>
<tool-use-id>m1</tool-use-id>
<status>completed</status>
</task-notification>"}"#);
        assert!(!st.tasks.contains_key("m1"));
    }
}
