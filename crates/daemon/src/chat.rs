//! Chats dans l'app : chaque chat est un processus `claude -p` piloté en
//! stream-json (entrée et sortie). Le démon relaie ses événements en direct
//! (SSE) et répond à ses demandes d'autorisation (`can_use_tool`) avec la
//! décision prise dans l'interface.
//!
//! Protocole vérifié sur la CLI 2.1.319 : `--permission-prompt-tool stdio`
//! fait arriver les demandes en `control_request` / `can_use_tool` ; la
//! réponse est un `control_response` portant `behavior: allow | deny`.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::broadcast;

pub struct LaunchSpec {
    pub repo_id: i64,
    pub name: String,
    pub cwd: String,
    pub slot: Option<String>,
    pub config_dir: Option<String>,
    pub resume: Option<String>,
    pub bypass: bool,
    pub worktree: Option<String>,
    /// Tokens déjà consommés par la conversation reprise (d'après la dernière analyse).
    pub tokens_base: i64,
    /// Variables propres au dossier (port du worktree…), fournies à l'agent.
    pub env: Vec<(String, String)>,
    /// Worktree créé pour cette discussion (son script de mise en place s'y lance).
    pub new_worktree: bool,
    /// Exécution du script de mise en place, s'il y en a une.
    pub setup_job: Option<String>,
    /// Coût déjà relevé pour la conversation reprise.
    pub cost_base: f64,
    /// Modèle demandé (`claude-opus-5-5`, alias…) ; `None` = réglage du compte.
    pub model: Option<String>,
}

struct State {
    /// Événements rejouables (sans les fragments `stream_event`, trop nombreux).
    events: Vec<Value>,
    seq: u64,
    session_id: Option<String>,
    /// idle | working | exited
    status: &'static str,
    pending: HashMap<String, Value>,
    cost_usd: f64,
    /// Tokens de la conversation : déjà consommés + ceux des tours vécus ici.
    tokens: i64,
    /// Modèle en service (annoncé par Claude Code à l'ouverture, puis à chaque changement).
    model: Option<String>,
    /// Messages écrits pendant que Claude travaille : envoyés un par un quand il a fini,
    /// récupérables tant qu'ils n'ont pas été transmis (comme dans le terminal).
    queue: Vec<(String, String, Vec<Image>)>,
    /// Images jointes, servies par numéro (les événements n'en portent que l'indice).
    images: Vec<Image>,
    /// Fin du dernier tour de Claude, et dernière consultation dans l'interface (ms).
    finished_at: i64,
    seen_at: i64,
}

/// Une discussion est « terminée et non lue » quand Claude a fini un tour
/// après la dernière fois qu'on l'a regardée (et qu'il ne travaille plus).
pub fn unread(status: &str, finished_at: i64, seen_at: i64) -> bool {
    status != "working" && finished_at > seen_at
}

/// Une image jointe à un message : type MIME et données base64.
#[derive(Clone, serde::Deserialize)]
pub struct Image {
    pub media_type: String,
    pub data: String,
}

/// Formats acceptés et taille maximale (l'API refuse au-delà de 5 Mo).
pub fn check_images(images: &[Image]) -> Result<()> {
    for i in images {
        if !matches!(i.media_type.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
            anyhow::bail!("The image cannot be attached because its type `{}` is not PNG, JPEG, GIF or WebP.", i.media_type);
        }
        if i.data.len() > 7_000_000 {
            anyhow::bail!("The image cannot be attached because it is larger than 5 MB.");
        }
    }
    Ok(())
}

pub struct Chat {
    pub id: String,
    pub repo_id: i64,
    pub name: String,
    pub cwd: String,
    pub slot: Option<String>,
    pub bypass: bool,
    pub worktree: Option<String>,
    pub created: i64,
    /// Conversation reprise (même session) : son historique précède le direct.
    pub resume_of: Option<String>,
    pub setup_job: Option<String>,
    state: Mutex<State>,
    tx: broadcast::Sender<Value>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: tokio::sync::Mutex<Option<Child>>,
}

#[derive(Default, Clone)]
pub struct Hub {
    chats: Arc<Mutex<HashMap<String, Arc<Chat>>>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn new_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("c{:x}{:x}", n, std::process::id())
}

impl Chat {
    /// Ajoute un événement : numéroté, gardé pour le rejeu (sauf fragments), diffusé.
    fn emit(&self, ev: Value) {
        let msg = {
            let mut st = self.state.lock().unwrap();
            st.seq += 1;
            let msg = json!({"seq": st.seq, "ev": ev});
            if msg["ev"]["type"] != "stream_event" {
                st.events.push(msg.clone());
            }
            msg
        };
        let _ = self.tx.send(msg);
    }

    fn set_status(&self, s: &'static str) {
        {
            let mut st = self.state.lock().unwrap();
            // Fin d'un tour de travail (réponse terminée, ou processus arrêté en plein tour).
            if st.status == "working" && s != "working" {
                st.finished_at = now_ms();
            }
            st.status = s;
        }
        self.emit(json!({"type": "claudius", "kind": "status", "status": s}));
    }

    pub fn summary(&self) -> Value {
        let st = self.state.lock().unwrap();
        json!({
            "id": self.id, "repo_id": self.repo_id, "name": self.name, "cwd": self.cwd, "slot": self.slot,
            "bypass": self.bypass, "worktree": self.worktree, "created": self.created, "resume_of": self.resume_of, "setup_job": self.setup_job,
            "session_id": st.session_id, "status": st.status, "pending": st.pending.len(), "cost_usd": st.cost_usd, "tokens_total": st.tokens, "model": st.model,
            "unread": unread(st.status, st.finished_at, st.seen_at),
            "queue": st.queue.iter().map(|(id, text, _)| json!({"id": id, "text": text})).collect::<Vec<_>>(),
        })
    }

    /// Abonnement : on s'abonne AVANT de copier l'historique ; le client dédoublonne par `seq`.
    pub fn subscribe(&self) -> (Vec<Value>, broadcast::Receiver<Value>) {
        let rx = self.tx.subscribe();
        let events = self.state.lock().unwrap().events.clone();
        (events, rx)
    }

    async fn write(&self, v: &Value) -> Result<()> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| anyhow::anyhow!("The chat cannot receive input because its process has ended."))?;
        stdin.write_all(format!("{v}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Envoie tout de suite si Claude est libre ; sinon met en file d'attente.
    pub async fn send(&self, text: &str, images: Vec<Image>) -> Result<()> {
        if text.trim().is_empty() && images.is_empty() {
            anyhow::bail!("The message cannot be sent because it is empty.");
        }
        check_images(&images)?;
        let queued = {
            let mut st = self.state.lock().unwrap();
            if st.status == "working" {
                let id = new_id();
                st.queue.push((id.clone(), text.to_string(), images.clone()));
                Some(id)
            } else {
                None
            }
        };
        match queued {
            Some(id) => {
                self.emit(json!({"type": "claudius", "kind": "queued", "id": id, "text": text, "image_count": images.len()}));
                Ok(())
            }
            None => self.transmit(text, images).await,
        }
    }

    async fn transmit(&self, text: &str, images: Vec<Image>) -> Result<()> {
        // Images d'abord, puis le texte (comme quand on colle une image dans Claude Code).
        let mut content: Vec<Value> = images
            .iter()
            .map(|i| json!({"type": "image", "source": {"type": "base64", "media_type": i.media_type, "data": i.data}}))
            .collect();
        if !text.trim().is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
        self.write(&json!({"type": "user", "message": {"role": "user", "content": content}})).await?;
        let refs: Vec<usize> = {
            let mut st = self.state.lock().unwrap();
            let start = st.images.len();
            st.images.extend(images);
            (start..st.images.len()).collect()
        };
        self.emit(json!({"type": "claudius", "kind": "user_sent", "text": text, "images": refs, "ts": now_ms()}));
        self.set_status("working");
        Ok(())
    }

    /// L'utilisateur a regardé la discussion : ce qui est fini est lu.
    pub fn mark_seen(&self) {
        self.state.lock().unwrap().seen_at = now_ms();
    }

    pub fn image(&self, n: usize) -> Option<Image> {
        self.state.lock().unwrap().images.get(n).cloned()
    }

    /// Reprend un message en attente (pas encore transmis) et rend son texte.
    pub fn unqueue(&self, qid: &str) -> Result<String> {
        let text = {
            let mut st = self.state.lock().unwrap();
            let pos = st
                .queue
                .iter()
                .position(|(id, _, _)| id == qid)
                .ok_or_else(|| anyhow::anyhow!("The message cannot be taken back because it has already been sent to Claude."))?;
            st.queue.remove(pos).1
        };
        self.emit(json!({"type": "claudius", "kind": "unqueued", "id": qid}));
        Ok(text)
    }

    /// Claude a fini son tour : on lui transmet le message suivant de la file, s'il y en a un.
    async fn next_in_queue(&self) {
        let next = {
            let mut st = self.state.lock().unwrap();
            if st.queue.is_empty() { None } else { Some(st.queue.remove(0)) }
        };
        if let Some((id, text, images)) = next {
            self.emit(json!({"type": "claudius", "kind": "unqueued", "id": id}));
            if let Err(e) = self.transmit(&text, images).await {
                self.emit(json!({"type": "claudius", "kind": "stderr", "text": format!("error: {e}")}));
            }
        }
    }

    pub async fn decide(&self, request_id: &str, allow: bool) -> Result<()> {
        let req = self
            .state
            .lock()
            .unwrap()
            .pending
            .remove(request_id)
            .ok_or_else(|| anyhow::anyhow!("The permission `{request_id}` cannot be answered because it is no longer pending."))?;
        let response = if allow {
            json!({"behavior": "allow", "updatedInput": req["input"].clone()})
        } else {
            json!({"behavior": "deny", "message": "Refusé depuis DRIFTy."})
        };
        self.write(&json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": response}}))
            .await?;
        self.emit(json!({"type": "claudius", "kind": "permission_resolved", "request_id": request_id, "allow": allow}));
        Ok(())
    }

    /// Change de modèle pour la suite de la discussion, sans la relancer (`set_model`, vérifié sur la CLI).
    pub async fn set_model(&self, model: &str) -> Result<()> {
        let m = model.trim();
        if m.is_empty() || m.len() > 80 || m.contains(char::is_whitespace) {
            anyhow::bail!("The model `{m}` cannot be used because it is not a valid model name.");
        }
        self.write(&json!({"type": "control_request", "request_id": new_id(), "request": {"subtype": "set_model", "model": m}})).await
    }

    pub async fn interrupt(&self) -> Result<()> {
        self.write(&json!({"type": "control_request", "request_id": new_id(), "request": {"subtype": "interrupt"}})).await
    }

    pub async fn kill(&self) {
        if let Some(mut c) = self.child.lock().await.take() {
            let _ = c.kill().await;
        }
        *self.stdin.lock().await = None;
    }
}

impl Hub {
    pub fn list(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.chats.lock().unwrap().values().map(|c| c.summary()).collect();
        v.sort_by_key(|c| -c["created"].as_i64().unwrap_or(0));
        v
    }

    pub fn get(&self, id: &str) -> Result<Arc<Chat>> {
        self.chats.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow::anyhow!("The chat `{id}` cannot be found because it is not running."))
    }

    pub async fn remove(&self, id: &str) -> Result<()> {
        let chat = self.get(id)?;
        chat.kill().await;
        self.chats.lock().unwrap().remove(id);
        Ok(())
    }

    pub async fn start(&self, spec: LaunchSpec, first_message: Option<(String, Vec<Image>)>) -> Result<Arc<Chat>> {
        let mut cmd = Command::new("claude");
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"]);
        if spec.bypass {
            cmd.args(["--permission-mode", "bypassPermissions"]);
        } else {
            cmd.args(["--permission-mode", "manual", "--permission-prompt-tool", "stdio"]);
        }
        // Modèle choisi pour la discussion ; à défaut, réglage de test (`CLAUDIUS_CHAT_MODEL`), sinon celui du compte.
        if let Some(m) = spec.model.clone().or_else(|| std::env::var("CLAUDIUS_CHAT_MODEL").ok()) {
            cmd.args(["--model", &m]);
        }
        if let Some(sid) = &spec.resume {
            cmd.args(["--resume", sid]);
        }
        // `~/.claude` est le dossier par défaut : ne pas le forcer (la variable déplace aussi l'identification).
        let default_dir = claudius_core::home().join(".claude");
        if let Some(dir) = spec.config_dir.as_ref().filter(|d| std::path::Path::new(d) != default_dir) {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        cmd.envs(spec.env.iter().cloned());
        cmd.current_dir(&spec.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().context("The chat cannot start because `claude` could not be launched")?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        let (tx, _) = broadcast::channel(4096);
        let chat = Arc::new(Chat {
            id: new_id(),
            repo_id: spec.repo_id,
            name: spec.name,
            cwd: spec.cwd,
            slot: spec.slot.clone(),
            bypass: spec.bypass,
            worktree: spec.worktree,
            created: now_ms(),
            resume_of: spec.resume.clone(),
            setup_job: spec.setup_job.clone(),
            state: Mutex::new(State {
                events: Vec::new(),
                seq: 0,
                session_id: spec.resume.clone(),
                status: "idle",
                pending: HashMap::new(),
                cost_usd: spec.cost_base,
                tokens: spec.tokens_base,
                model: spec.model.clone(),
                queue: Vec::new(),
                images: Vec::new(),
                finished_at: 0,
                seen_at: 0,
            }),
            tx,
            stdin: tokio::sync::Mutex::new(stdin),
            child: tokio::sync::Mutex::new(Some(child)),
        });
        self.chats.lock().unwrap().insert(chat.id.clone(), chat.clone());

        // Lecture de la sortie : chaque ligne JSON devient un événement.
        {
            let chat = chat.clone();
            let slot = spec.slot.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                let mut watch = Watch::default();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
                    for w in watch.observe(&ev) {
                        chat.emit(w);
                    }
                    match ev["type"].as_str() {
                        Some("system") if ev["subtype"] == "init" => {
                            let mut st = chat.state.lock().unwrap();
                            st.session_id = ev["session_id"].as_str().map(str::to_string);
                            if let Some(m) = ev["model"].as_str() {
                                st.model = Some(m.to_string());
                            }
                        }
                        // Réponse à un changement de modèle : le nouveau modèle en service.
                        Some("control_response") if ev.pointer("/response/response/model").is_some() => {
                            let m = ev.pointer("/response/response/model").and_then(Value::as_str).map(str::to_string);
                            chat.state.lock().unwrap().model = m.clone();
                            chat.emit(json!({"type": "claudius", "kind": "model", "model": m}));
                        }
                        Some("control_request") if ev["request"]["subtype"] == "can_use_tool" => {
                            if let Some(rid) = ev["request_id"].as_str() {
                                chat.state.lock().unwrap().pending.insert(rid.to_string(), ev["request"].clone());
                            }
                        }
                        Some("rate_limit_event") => {
                            if let (Some(slot), Some(w)) = (&slot, ev.pointer("/rate_limit_info/unifiedWindows")) {
                                let _ = claudius_core::usage::record_unified(slot, w);
                            }
                        }
                        Some("result") => {
                            let cost = ev["total_cost_usd"].as_f64().unwrap_or(0.0);
                            let u = &ev["usage"];
                            let n = |k: &str| u[k].as_i64().unwrap_or(0);
                            let turn = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens") + n("output_tokens");
                            let (total, cost_total) = {
                                let mut st = chat.state.lock().unwrap();
                                st.cost_usd += cost;
                                st.tokens += turn;
                                (st.tokens, st.cost_usd)
                            };
                            chat.emit(json!({"type": "claudius", "kind": "tokens", "total": total, "cost": cost_total}));
                        }
                        _ => {}
                    }
                    let is_result = ev["type"] == "result";
                    // Modifications de fichier : `+ajoutées −retirées`, calculé par le noyau.
                    let diffs: Vec<Value> = if ev["type"] == "assistant" {
                        ev["message"]["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|b| b["type"] == "tool_use")
                            .filter_map(|b| {
                                let (a, r) = claudius_core::diffstat::for_tool(b["name"].as_str().unwrap_or(""), &b["input"])?;
                                Some(json!({"type": "claudius", "kind": "diff", "tool_id": b["id"], "added": a, "removed": r}))
                            })
                            .collect()
                    } else {
                        vec![]
                    };
                    chat.emit(ev);
                    for d in diffs {
                        chat.emit(d);
                    }
                    if is_result {
                        chat.set_status("idle");
                        chat.next_in_queue().await;
                    }
                }
                *chat.stdin.lock().await = None;
                let code = match chat.child.lock().await.as_mut() {
                    Some(c) => c.wait().await.ok().and_then(|s| s.code()),
                    None => None,
                };
                chat.emit(json!({"type": "claudius", "kind": "exit", "code": code}));
                chat.set_status("exited");
            });
        }
        // Erreurs du processus : relayées (tronquées) pour ne rien cacher.
        {
            let chat = chat.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !line.trim().is_empty() {
                        chat.emit(json!({"type": "claudius", "kind": "stderr", "text": line.chars().take(500).collect::<String>()}));
                    }
                }
            });
        }

        // Le protocole de contrôle s'ouvre par une initialisation.
        chat.write(&json!({"type": "control_request", "request_id": new_id(), "request": {"subtype": "initialize"}})).await?;
        if let Some((text, images)) = first_message {
            chat.send(&text, images).await?;
        }
        Ok(chat)
    }
}

/// Gaspillage repéré en direct dans le flux d'un chat : boucle (même appel en échec
/// trois fois de suite), échec évitable connu (avec sa règle), sortie énorme lue en entier.
#[derive(Default)]
struct Watch {
    /// id d'appel → (outil, empreinte de l'entrée, cible lisible)
    calls: HashMap<String, (String, i64, String)>,
    last_error: Option<(String, i64)>,
    streak: u32,
    hinted: std::collections::HashSet<&'static str>,
}

const BIG_OUTPUT: usize = 40_000;
const LOOP_AFTER: u32 = 3;

impl Watch {
    fn observe(&mut self, ev: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        let blocks = ev.pointer("/message/content").and_then(Value::as_array).cloned().unwrap_or_default();
        match ev["type"].as_str() {
            Some("assistant") => {
                for b in blocks.iter().filter(|b| b["type"] == "tool_use") {
                    let input = b["input"].to_string();
                    let target = ["command", "file_path", "pattern", "url"]
                        .iter()
                        .find_map(|k| b["input"][*k].as_str())
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(160)
                        .collect();
                    let name = b["name"].as_str().unwrap_or("").to_string();
                    self.calls.insert(b["id"].as_str().unwrap_or("").to_string(), (name, claudius_core::extract::fnv1a(input.as_bytes()), target));
                }
            }
            Some("user") => {
                for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
                    let Some((name, hash, target)) = self.calls.get(b["tool_use_id"].as_str().unwrap_or("")).cloned() else { continue };
                    let body = b["content"].to_string();
                    let is_error = b["is_error"].as_bool().unwrap_or(false);
                    if is_error {
                        let key = (name.clone(), hash);
                        self.streak = if self.last_error.as_ref() == Some(&key) { self.streak + 1 } else { 1 };
                        self.last_error = Some(key);
                        if self.streak == LOOP_AFTER {
                            out.push(json!({"type": "claudius", "kind": "waste", "what": "loop", "tool": name, "target": target, "count": self.streak}));
                        }
                        // Échec évitable connu : la consigne du catalogue, une fois par type et par chat.
                        if let Some(f) = claudius_core::fixes::avoidable(&body) {
                            if self.hinted.insert(f.key) {
                                out.push(json!({"type": "claudius", "kind": "waste", "what": "avoidable", "tool": name, "target": target,
                                                 "rule": claudius_core::fixes::rule_text(f.key, f.item.as_deref())}));
                            }
                        }
                    } else {
                        self.streak = 0;
                        self.last_error = None;
                    }
                    if body.len() > BIG_OUTPUT {
                        out.push(json!({"type": "claudius", "kind": "waste", "what": "big", "tool": name, "target": target, "bytes": body.len()}));
                    }
                }
            }
            _ => {}
        }
        out
    }
}

/// Emplacement des worktrees créés par DRIFTy : `~/claudius/worktrees/<dépôt>/<nom>` (un seul point).
pub fn worktree_path(repo_name: &str, name: &str) -> std::path::PathBuf {
    claudius_core::home().join("claudius").join("worktrees").join(repo_name).join(name)
}

/// Crée un worktree isolé `<racine>/<nom>` sur une nouvelle branche `<nom>`.
pub fn create_worktree(repo_root: &str, repo_name: &str, name: &str) -> Result<String> {
    let ok = !name.is_empty()
        && name.len() <= 60
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with(['-', '.']);
    if !ok {
        anyhow::bail!("The name `{name}` cannot be used because it must be 1 to 60 letters, digits, `-`, `_` or `.`.");
    }
    let path = worktree_path(repo_name, name);
    if let Some(base) = path.parent() {
        std::fs::create_dir_all(base)?;
    }
    if path.exists() {
        anyhow::bail!("The worktree `{name}` cannot be created because `{}` already exists.", path.display());
    }
    let out = std::process::Command::new("git").args(["-C", repo_root, "worktree", "add", "-b", name]).arg(&path).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("The worktree `{name}` cannot be created because git said: {}", err.trim());
    }
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::{unread, Watch};
    use serde_json::json;

    #[test]
    fn watch_spots_loops_avoidable_errors_and_big_outputs() {
        let mut w = Watch::default();
        let call = |id: &str| json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "make test"}}]}});
        let fail = |id: &str, msg: &str| json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": id, "is_error": true, "content": msg}]}});
        let mut kinds = Vec::new();
        for i in 0..3 {
            let id = format!("t{i}");
            w.observe(&call(&id));
            for e in w.observe(&fail(&id, "Exit code 2\nerror: no rule")) {
                kinds.push(e["what"].as_str().unwrap().to_string());
            }
        }
        assert_eq!(kinds, vec!["loop"], "la 3e répétition déclenche la boucle, une seule fois");
        w.observe(&call("p"));
        let hint = w.observe(&fail("p", "cat: x.txt: No such file or directory"));
        assert_eq!(hint[0]["what"], "avoidable");
        w.observe(&call("b"));
        let big = w.observe(&json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "b", "content": "x".repeat(50_000)}]}}));
        assert_eq!(big[0]["what"], "big");
    }

    #[test]
    fn unread_rule() {
        assert!(!unread("working", 10, 0), "un tour en cours n'est pas « terminé »");
        assert!(unread("idle", 10, 5), "fini après la dernière consultation");
        assert!(!unread("idle", 10, 20), "consulté après la fin");
        assert!(!unread("idle", 0, 0), "jamais fini");
        assert!(unread("exited", 10, 0), "arrêté après un tour non lu");
    }
}
