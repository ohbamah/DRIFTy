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
    /// Reprise en BRANCHE (`--fork-session`) : une nouvelle conversation part de celle reprise.
    pub fork: bool,
    /// Discussion rétablie après un redémarrage : elle garde son identifiant (voir persist.rs).
    pub restore_id: Option<String>,
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
    /// Dernier `total_cost_usd` reçu : Claude Code le donne CUMULÉ depuis le lancement du
    /// processus (mesuré : 0,0018 puis 0,0027) ; chaque tour ne compte que la différence.
    reported_cost: f64,
    /// Tokens de la conversation : déjà consommés + ceux des tours vécus ici.
    tokens: i64,
    /// Modèle en service (annoncé par Claude Code à l'ouverture, puis à chaque changement).
    model: Option<String>,
    /// Messages écrits pendant que Claude travaille : envoyés un par un quand il a fini,
    /// récupérables tant qu'ils n'ont pas été transmis (comme dans le terminal).
    queue: Vec<(String, String, Vec<Image>)>,
    /// Commandes `!` lancées depuis le dernier message (entrée et sorties, au format de Claude
    /// Code) : jointes au début du prochain message transmis, comme dans le terminal.
    bash_context: Vec<String>,
    /// Images jointes, servies par numéro (les événements n'en portent que l'indice).
    images: Vec<Image>,
    /// Fin du dernier tour de Claude, et dernière consultation dans l'interface (ms).
    finished_at: i64,
    seen_at: i64,
    /// Le processus s'est arrêté en plein tour (voir persist.rs).
    interrupted: bool,
    /// Script de mise en place du worktree : `running`, `ok` ou `failed` (aucun s'il n'y en a pas).
    setup: Option<&'static str>,
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
    pub cwd: String,
    pub slot: Option<String>,
    pub bypass: bool,
    pub worktree: Option<String>,
    pub created: i64,
    /// Conversation reprise (même session) : son historique précède le direct.
    pub resume_of: Option<String>,
    pub setup_job: Option<String>,
    /// De quoi relancer la discussion après un redémarrage (voir persist.rs).
    config_dir: Option<String>,
    env: Vec<(String, String)>,
    /// Nom affiché, renommable.
    title: Mutex<String>,
    state: Mutex<State>,
    /// Requêtes de contrôle dont on attend la réponse (question annexe), par identifiant.
    waiters: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Value>>>,
    tx: broadcast::Sender<Value>,
    /// Signal du hub : ce chat a changé (statut, coût, file, nom…).
    changed: broadcast::Sender<()>,
    /// Limites de la jauge (partagées, relues à chaque fin de tour).
    limits: Arc<Mutex<Limits>>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    child: tokio::sync::Mutex<Option<Child>>,
    /// Commandes `!` en cours : de quoi les arrêter.
    bash_runs: Mutex<HashMap<String, BashRun>>,
}

/// Limites de la jauge de conversation, appliquées à CHAQUE discussion : `/compact` quand
/// ses tokens franchissent le plafond, arrêt quand son coût atteint 99 % du budget.
#[derive(Clone, Copy)]
pub struct Limits {
    /// `true` : jauge en tokens (auto-compact) ; `false` : jauge en budget (arrêt). Exclusifs.
    pub tokens_mode: bool,
    pub tokens_cap: f64,
    pub budget_usd: f64,
    pub auto_compact: bool,
    pub stop_at_budget: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { tokens_mode: true, tokens_cap: 1e9, budget_usd: 50.0, auto_compact: true, stop_at_budget: true }
    }
}

impl Limits {
    /// Budget atteint (99 %) avec l'arrêt demandé : plus rien ne part.
    fn over_budget(&self, cost: f64) -> bool {
        !self.tokens_mode && self.stop_at_budget && self.budget_usd > 0.0 && cost >= self.budget_usd * 0.99
    }

    /// Jauge des tokens franchie (100 %, 200 %…) en mode tokens avec l'auto-compact.
    fn compact_due(&self, before: i64, total: i64) -> bool {
        self.tokens_mode && self.auto_compact && self.tokens_cap > 0.0 && (total as f64 / self.tokens_cap).floor() > (before as f64 / self.tokens_cap).floor()
    }
}

#[derive(Clone)]
pub struct Hub {
    chats: Arc<Mutex<HashMap<String, Arc<Chat>>>>,
    /// « La liste a changé » : poussé aux pages (flux `/api/events`) au lieu d'être relu.
    changed: broadcast::Sender<()>,
    limits: Arc<Mutex<Limits>>,
}

impl Default for Hub {
    fn default() -> Self {
        Hub { chats: Arc::default(), changed: broadcast::channel(64).0, limits: Arc::default() }
    }
}

/// Commande `!` en cours : de quoi l'arrêter et lui écrire.
struct BashRun {
    stop: tokio::sync::oneshot::Sender<()>,
    input: tokio::sync::mpsc::Sender<crate::bang::Input>,
}

/// Délai d'une commande `!` (un bouton l'arrête avant).
const BASH_TIMEOUT_S: u64 = 600;

/// Ce qui a mis la discussion en attente de l'utilisateur, d'après la demande la plus récente :
/// une question (`AskUserQuestion`), un plan à valider (`ExitPlanMode`) ou une autorisation.
pub fn waiting_kind(tool_name: &str) -> &'static str {
    match tool_name {
        "AskUserQuestion" => "question",
        "ExitPlanMode" => "plan",
        _ => "permission",
    }
}

/// Réponse à une demande `can_use_tool`. `answers` (questions) s'ajoute à l'entrée de l'outil,
/// comme le fait le terminal ; `mode` (plan approuvé) fixe le mode d'autorisation de la suite
/// (sans lui, Claude reprend celui d'avant le plan ; mesuré le 2026-11-10) ; `message` est le
/// motif d'un refus, lu par Claude.
pub fn permission_response(input: &Value, allow: bool, answers: Option<&Value>, mode: Option<&str>, message: Option<&str>) -> Result<Value> {
    if !allow {
        return Ok(json!({"behavior": "deny", "message": message.unwrap_or("Refusé depuis DRIFTy.")}));
    }
    let mut input = input.clone();
    if let (Some(a), Some(obj)) = (answers, input.as_object_mut()) {
        obj.insert("answers".into(), a.clone());
    }
    let mut r = json!({"behavior": "allow", "updatedInput": input});
    match mode {
        None => {}
        Some(m @ ("acceptEdits" | "default")) => r["updatedPermissions"] = json!([{"type": "setMode", "mode": m, "destination": "session"}]),
        Some(m) => anyhow::bail!("The permission cannot be granted because the mode `{m}` is not one DRIFTy offers after a plan."),
    }
    Ok(r)
}

/// Résultat sans aucun tour de Claude (commande locale, reprise) : 0 tour, ou 0 s et 0 token écrit.
fn is_noop_result(ev: &Value) -> bool {
    ev["num_turns"] == 0 || (ev["duration_ms"].as_i64().unwrap_or(0) == 0 && ev.pointer("/usage/output_tokens").and_then(Value::as_i64).unwrap_or(0) == 0)
}

/// Montant en dollars à la française : « 12,40 $ » (« < 0,01 $ » pour les poussières).
fn fr_usd(x: f64) -> String {
    if x > 0.0 && x < 0.005 { "< 0,01 $".into() } else { format!("{x:.2} $").replace('.', ",") }
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
            // `at` : quand l'événement est arrivé (heure de fin d'un tour, par exemple).
            let msg = json!({"seq": st.seq, "at": now_ms(), "ev": ev});
            if msg["ev"]["type"] != "stream_event" {
                st.events.push(msg.clone());
            }
            msg
        };
        let streamed = msg["ev"]["type"] == "stream_event";
        let _ = self.tx.send(msg);
        if !streamed {
            let _ = self.changed.send(());
        }
    }

    /// Événement de passage (sortie d'une commande `!` au fil de l'eau) : diffusé, jamais gardé.
    fn emit_live(&self, ev: Value) {
        let msg = {
            let mut st = self.state.lock().unwrap();
            st.seq += 1;
            json!({"seq": st.seq, "at": now_ms(), "ev": ev})
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
        self.emit(json!({"type": "drifty", "kind": "status", "status": s}));
    }

    pub fn summary(&self) -> Value {
        let st = self.state.lock().unwrap();
        json!({
            "id": self.id, "repo_id": self.repo_id, "name": *self.title.lock().unwrap(), "cwd": self.cwd, "slot": self.slot,
            "bypass": self.bypass, "worktree": self.worktree, "created": self.created, "resume_of": self.resume_of, "setup_job": self.setup_job,
            "session_id": st.session_id, "status": st.status, "pending": st.pending.len(),
            // Ce qu'attend la discussion (la demande la plus récente) : question, plan ou autorisation.
            "waiting_for": st.pending.values().max_by_key(|r| r["_at"].as_i64().unwrap_or(0)).map(|r| waiting_kind(r["tool_name"].as_str().unwrap_or(""))), "cost_usd": st.cost_usd, "tokens_total": st.tokens, "model": st.model,
            "unread": unread(st.status, st.finished_at, st.seen_at),
            "setup": st.setup,
            "queue": st.queue.iter().map(|(id, text, _)| json!({"id": id, "text": text})).collect::<Vec<_>>(),
        })
    }

    /// Description de relance (voir persist.rs) ; aucune tant que Claude n'a pas annoncé sa session.
    pub fn saved(&self) -> Option<crate::persist::Saved> {
        let st = self.state.lock().unwrap();
        Some(crate::persist::Saved {
            id: self.id.clone(),
            repo_id: self.repo_id,
            name: self.title.lock().unwrap().clone(),
            cwd: self.cwd.clone(),
            slot: self.slot.clone(),
            config_dir: self.config_dir.clone(),
            bypass: self.bypass,
            worktree: self.worktree.clone(),
            model: st.model.clone(),
            session_id: st.session_id.clone()?,
            env: self.env.clone(),
            tokens: st.tokens,
            cost_usd: st.cost_usd,
            interrupted: st.status == "working" || st.interrupted,
        })
    }

    /// Fin du script de mise en place : sa sortie (la fin) s'affiche dans la discussion. En cas
    /// d'échec, le premier message (`first`) n'est PAS transmis : il attend dans la file, d'où
    /// on l'envoie quand même ou on le reprend.
    pub async fn setup_done(&self, code: Option<i32>, output: &str, ms: i64, first: Option<(String, Vec<Image>)>) {
        let ok = code == Some(0);
        self.state.lock().unwrap().setup = Some(if ok { "ok" } else { "failed" });
        self.emit(json!({"type": "drifty", "kind": "setup", "code": code, "output": crate::bang::tail_chars(output.trim_end(), 6000), "ms": ms}));
        let Some((text, images)) = first else { return };
        if ok {
            let _ = self.send(&text, images).await;
            return;
        }
        let id = new_id();
        self.state.lock().unwrap().queue.push((id.clone(), text.clone(), images.clone()));
        self.emit(json!({"type": "drifty", "kind": "queued", "id": id, "text": text, "image_count": images.len()}));
        self.notice("La mise en place du worktree a échoué : ton premier message n'est pas parti. Il attend ci-dessous : envoie-le quand même, ou reprends-le pour le modifier.");
    }

    /// Note de DRIFTy dans la discussion (affichée, jamais envoyée à Claude).
    pub fn notice(&self, text: &str) {
        self.emit(json!({"type": "drifty", "kind": "notice", "text": text}));
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
        let (cost, limits) = (self.state.lock().unwrap().cost_usd, *self.limits.lock().unwrap());
        if limits.over_budget(cost) {
            anyhow::bail!(
                "The message cannot be sent because this conversation has spent ${cost:.2} of its ${:.2} budget (stop at 99 %).",
                limits.budget_usd
            );
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
                self.emit(json!({"type": "drifty", "kind": "queued", "id": id, "text": text, "image_count": images.len()}));
                Ok(())
            }
            None => self.transmit(text, images).await,
        }
    }

    async fn transmit(&self, text: &str, images: Vec<Image>) -> Result<()> {
        // Commandes `!` d'abord (Claude les lit avant la demande), puis les images, puis le texte
        // (comme quand on colle une image dans Claude Code).
        let bash = std::mem::take(&mut self.state.lock().unwrap().bash_context);
        let mut content: Vec<Value> = bash.iter().map(|b| json!({"type": "text", "text": b})).collect();
        content.extend(images.iter().map(|i| json!({"type": "image", "source": {"type": "base64", "media_type": i.media_type, "data": i.data}})));
        if !text.trim().is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
        if let Err(e) = self.write(&json!({"type": "user", "message": {"role": "user", "content": content}})).await {
            self.state.lock().unwrap().bash_context.splice(0..0, bash); // rien n'est perdu
            return Err(e);
        }
        let refs: Vec<usize> = {
            let mut st = self.state.lock().unwrap();
            let start = st.images.len();
            st.images.extend(images);
            (start..st.images.len()).collect()
        };
        self.emit(json!({"type": "drifty", "kind": "user_sent", "text": text, "images": refs, "ts": now_ms()}));
        self.set_status("working");
        Ok(())
    }

    /// Commande `!` (mode bash du terminal) : lancée tout de suite, sa sortie arrive au fil de
    /// l'eau (`bash_chunk`, non gardés : le rejeu a la sortie finale) et part avec le prochain
    /// message. Rend l'identifiant de la commande (pour l'arrêter).
    pub fn bash(self: &Arc<Self>, command: &str) -> Result<String> {
        let command = command.trim().to_string();
        if command.is_empty() {
            anyhow::bail!("The command cannot run because it is empty.");
        }
        let id = new_id();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let (input_tx, input_rx) = tokio::sync::mpsc::channel(8);
        self.bash_runs.lock().unwrap().insert(id.clone(), BashRun { stop: stop_tx, input: input_tx });
        let started = now_ms();
        self.emit(json!({"type": "drifty", "kind": "bash_start", "id": id, "command": command, "ts": started}));
        let (chat, bid) = (self.clone(), id.clone());
        tokio::spawn(async move {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let timeout = std::time::Duration::from_secs(BASH_TIMEOUT_S);
            let o = crate::bang::run(&shell, &command, &chat.cwd, timeout, stop_rx, input_rx, |stream, text| {
                let stream = if stream == crate::bang::Stream::Out { "out" } else { "err" };
                chat.emit_live(json!({"type": "drifty", "kind": "bash_chunk", "id": bid, "stream": stream, "text": text}));
            })
            .await;
            chat.bash_runs.lock().unwrap().remove(&bid);
            chat.state.lock().unwrap().bash_context.push(crate::bang::context_block(&command, &o, timeout));
            chat.emit(json!({"type": "drifty", "kind": "bash", "id": bid, "command": command, "stdout": o.stdout, "stderr": o.stderr,
                "code": o.code, "stopped": o.stopped, "timed_out": o.timed_out, "ms": now_ms() - started}));
        });
        Ok(id)
    }

    /// Arrête une commande `!` en cours (sa sortie jusque-là est gardée).
    pub fn bash_stop(&self, bid: &str) -> Result<()> {
        let run = self.bash_runs.lock().unwrap().remove(bid).ok_or_else(|| anyhow::anyhow!("The command `{bid}` cannot be stopped because it is no longer running."))?;
        let _ = run.stop.send(());
        Ok(())
    }

    /// Une ligne tapée pour une commande `!` en cours (réponse, mot de passe) : écrite sur son
    /// entrée standard, jamais gardée ni diffusée.
    pub async fn bash_input(&self, bid: &str, line: &str, secret: bool) -> Result<()> {
        let tx = self.bash_runs.lock().unwrap().get(bid).map(|r| r.input.clone())
            .ok_or_else(|| anyhow::anyhow!("The input cannot reach the command `{bid}` because it is no longer running."))?;
        tx.send(crate::bang::Input { line: line.to_string(), secret }).await.map_err(|_| anyhow::anyhow!("The input cannot reach the command `{bid}` because it has ended."))
    }

    /// L'utilisateur a regardé la discussion : ce qui est fini est lu.
    pub fn mark_seen(&self) {
        self.state.lock().unwrap().seen_at = now_ms();
        let _ = self.changed.send(());
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
        self.emit(json!({"type": "drifty", "kind": "unqueued", "id": qid}));
        Ok(text)
    }

    /// « Envoyer maintenant » : le message quitte la file et part tout de suite à Claude Code,
    /// qui le prend en compte en plein tour (à la prochaine étape), sans l'interrompre.
    pub async fn send_now(&self, qid: &str) -> Result<()> {
        let (text, images) = {
            let mut st = self.state.lock().unwrap();
            let pos = st
                .queue
                .iter()
                .position(|(id, _, _)| id == qid)
                .ok_or_else(|| anyhow::anyhow!("The message cannot be sent now because it has already been sent to Claude."))?;
            let (_, text, images) = st.queue.remove(pos);
            (text, images)
        };
        self.emit(json!({"type": "drifty", "kind": "unqueued", "id": qid}));
        self.transmit(&text, images).await
    }

    /// Claude a fini son tour : on lui transmet le message suivant de la file, s'il y en a un.
    async fn next_in_queue(&self) {
        let (cost, limits) = (self.state.lock().unwrap().cost_usd, *self.limits.lock().unwrap());
        if limits.over_budget(cost) {
            return; // budget atteint : la file attend (levée en relevant le budget ou en décochant l'arrêt)
        }
        let next = {
            let mut st = self.state.lock().unwrap();
            if st.queue.is_empty() { None } else { Some(st.queue.remove(0)) }
        };
        if let Some((id, text, images)) = next {
            self.emit(json!({"type": "drifty", "kind": "unqueued", "id": id}));
            if let Err(e) = self.transmit(&text, images).await {
                self.emit(json!({"type": "drifty", "kind": "stderr", "text": format!("error: {e}")}));
            }
        }
    }

    /// Réponse à une demande : autorisation, ou réponses à des questions (`AskUserQuestion` :
    /// `answers` = question → choix, ajoutées à l'entrée de l'outil comme le fait le terminal).
    /// `message` : motif d'un refus (« en discuter », « annulé »).
    pub async fn decide(&self, request_id: &str, allow: bool, answers: Option<Value>, mode: Option<String>, message: Option<String>) -> Result<()> {
        // Réponse construite AVANT de retirer la demande : un mode refusé la laisse en attente.
        let input = self.state.lock().unwrap().pending.get(request_id).map(|r| r["input"].clone());
        let response = permission_response(&input.unwrap_or(Value::Null), allow, answers.as_ref(), mode.as_deref(), message.as_deref())?;
        self.state
            .lock()
            .unwrap()
            .pending
            .remove(request_id)
            .ok_or_else(|| anyhow::anyhow!("The permission `{request_id}` cannot be answered because it is no longer pending."))?;
        self.write(&json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": response}}))
            .await?;
        self.emit(json!({"type": "drifty", "kind": "permission_resolved", "request_id": request_id, "allow": allow, "answers": answers, "mode": mode, "message": message}));
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

    /// Question annexe (`/btw`) : répondue d'après la conversation, sans la modifier, même
    /// pendant que l'agent travaille.
    pub async fn side_question(&self, question: &str, history: Value) -> Result<String> {
        let rid = new_id();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiters.lock().unwrap().insert(rid.clone(), tx);
        let sent = self
            .write(&json!({"type": "control_request", "request_id": rid, "request": {"subtype": "side_question", "question": question, "history": history}}))
            .await;
        if let Err(e) = sent {
            self.waiters.lock().unwrap().remove(&rid);
            return Err(e);
        }
        let r = tokio::time::timeout(std::time::Duration::from_secs(660), rx).await;
        self.waiters.lock().unwrap().remove(&rid);
        let r = r
            .map_err(|_| anyhow::anyhow!("The side question cannot be answered because Claude did not answer within 11 minutes."))?
            .map_err(|_| anyhow::anyhow!("The side question cannot be answered because the chat stopped."))?;
        if r["subtype"] == "success" {
            Ok(r.pointer("/response/response").and_then(Value::as_str).unwrap_or("").to_string())
        } else {
            anyhow::bail!("The side question cannot be answered because Claude said: {}", r["error"].as_str().unwrap_or("unknown error"))
        }
    }

    pub fn rename(&self, name: &str) {
        *self.title.lock().unwrap() = name.to_string();
        let _ = self.changed.send(());
    }

    pub fn session_id(&self) -> Option<String> {
        self.state.lock().unwrap().session_id.clone()
    }

    pub async fn interrupt(&self) -> Result<()> {
        self.write(&json!({"type": "control_request", "request_id": new_id(), "request": {"subtype": "interrupt"}})).await
    }

    pub async fn kill(&self) {
        // Ses commandes `!` ne lui survivent pas.
        for (_, run) in self.bash_runs.lock().unwrap().drain() {
            let _ = run.stop.send(());
        }
        if let Some(mut c) = self.child.lock().await.take() {
            let _ = c.kill().await;
        }
        *self.stdin.lock().await = None;
    }
}

impl Hub {
    /// Descriptions de relance de toutes les discussions (voir persist.rs), dans l'ordre de création.
    pub fn saved(&self) -> Vec<crate::persist::Saved> {
        let mut v: Vec<(i64, crate::persist::Saved)> = self.chats.lock().unwrap().values().filter_map(|c| Some((c.created, c.saved()?))).collect();
        v.sort_by_key(|(t, _)| *t);
        v.into_iter().map(|(_, s)| s).collect()
    }

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
        let _ = self.changed.send(());
        Ok(())
    }

    /// Arrête toutes les discussions (et leurs commandes `!`) sans les retirer de la liste.
    pub async fn stop_all(&self) {
        let all: Vec<Arc<Chat>> = self.chats.lock().unwrap().values().cloned().collect();
        for c in all {
            c.kill().await;
        }
    }

    pub fn set_limits(&self, l: Limits) {
        *self.limits.lock().unwrap() = l;
    }

    pub fn watch(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }

    pub async fn start(&self, spec: LaunchSpec, first_message: Option<(String, Vec<Image>)>) -> Result<Arc<Chat>> {
        let mut cmd = Command::new("claude");
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"]);
        if spec.bypass {
            // Canal `stdio` même en bypass : seules les demandes qui EXIGENT l'utilisateur y passent
            // (questions `AskUserQuestion`, absentes sans lui ; mesuré le 2026-11-10).
            cmd.args(["--permission-mode", "bypassPermissions", "--permission-prompt-tool", "stdio"]);
        } else {
            cmd.args(["--permission-mode", "manual", "--permission-prompt-tool", "stdio"]);
        }
        // Modèle choisi pour la discussion ; à défaut, réglage de test (`DRIFTY_CHAT_MODEL`), sinon celui du compte.
        if let Some(m) = spec.model.clone().or_else(|| std::env::var("DRIFTY_CHAT_MODEL").ok()) {
            cmd.args(["--model", &m]);
        }
        if let Some(sid) = &spec.resume {
            cmd.args(["--resume", sid]);
            if spec.fork {
                cmd.arg("--fork-session");
            }
        }
        // `~/.claude` est le dossier par défaut : ne pas le forcer (la variable déplace aussi l'identification).
        let default_dir = drifty_core::home().join(".claude");
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
            id: spec.restore_id.clone().unwrap_or_else(new_id),
            repo_id: spec.repo_id,
            cwd: spec.cwd,
            slot: spec.slot.clone(),
            bypass: spec.bypass,
            worktree: spec.worktree,
            created: now_ms(),
            resume_of: spec.resume.clone(),
            setup_job: spec.setup_job.clone(),
            config_dir: spec.config_dir.clone(),
            env: spec.env.clone(),
            title: Mutex::new(spec.name.clone()),
            waiters: Mutex::new(HashMap::new()),
            bash_runs: Mutex::new(HashMap::new()),
            state: Mutex::new(State {
                events: Vec::new(),
                seq: 0,
                // Une branche reçoit son propre identifiant (annoncé à l'ouverture).
                session_id: if spec.fork { None } else { spec.resume.clone() },
                status: "idle",
                pending: HashMap::new(),
                cost_usd: spec.cost_base,
                reported_cost: 0.0,
                tokens: spec.tokens_base,
                model: spec.model.clone(),
                queue: Vec::new(),
                bash_context: Vec::new(),
                images: Vec::new(),
                finished_at: 0,
                seen_at: 0,
                interrupted: false,
                setup: spec.setup_job.as_ref().map(|_| "running"),
            }),
            tx,
            changed: self.changed.clone(),
            limits: self.limits.clone(),
            stdin: tokio::sync::Mutex::new(stdin),
            child: tokio::sync::Mutex::new(Some(child)),
        });
        self.chats.lock().unwrap().insert(chat.id.clone(), chat.clone());
        let _ = self.changed.send(());

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
                        // Réponse attendue (question annexe) : remise à celui qui l'attend.
                        Some("control_response")
                            if ev.pointer("/response/request_id").and_then(Value::as_str).is_some_and(|r| chat.waiters.lock().unwrap().contains_key(r)) =>
                        {
                            let rid = ev.pointer("/response/request_id").and_then(Value::as_str).unwrap_or("").to_string();
                            if let Some(tx) = chat.waiters.lock().unwrap().remove(&rid) {
                                let _ = tx.send(ev["response"].clone());
                            }
                        }
                        // Réponse à un changement de modèle : le nouveau modèle en service.
                        Some("control_response") if ev.pointer("/response/response/model").is_some() => {
                            let m = ev.pointer("/response/response/model").and_then(Value::as_str).map(str::to_string);
                            chat.state.lock().unwrap().model = m.clone();
                            chat.emit(json!({"type": "drifty", "kind": "model", "model": m}));
                        }
                        Some("control_request") if ev["request"]["subtype"] == "can_use_tool" => {
                            if let Some(rid) = ev["request_id"].as_str() {
                                let mut req = ev["request"].clone();
                                req["_at"] = json!(now_ms()); // ordre d'arrivée (voir `waiting_for`)
                                chat.state.lock().unwrap().pending.insert(rid.to_string(), req);
                            }
                        }
                        Some("rate_limit_event") => {
                            if let (Some(slot), Some(w)) = (&slot, ev.pointer("/rate_limit_info/unifiedWindows")) {
                                let _ = drifty_core::usage::record_unified(slot, w);
                            }
                        }
                        Some("result") => {
                            let reported = ev["total_cost_usd"].as_f64().unwrap_or(0.0);
                            // Résultat « vide » (aucun tour : 0 s, 0 token) : à la reprise, Claude Code y
                            // renvoie le coût cumulé de toute la session restaurée. Ce n'est pas une dépense
                            // de ce tour : il ne sert que de point de départ au décompte.
                            let noop = is_noop_result(&ev);
                            let u = &ev["usage"];
                            let n = |k: &str| u[k].as_i64().unwrap_or(0);
                            let turn = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens") + n("output_tokens");
                            let (before, total, cost_total) = {
                                let mut st = chat.state.lock().unwrap();
                                let before = st.tokens;
                                // Cumul du processus : la différence avec le précédent (un cumul qui
                                // redescend signale un nouveau décompte : il compte en entier).
                                let cost = if noop { 0.0 } else if reported >= st.reported_cost { reported - st.reported_cost } else { reported };
                                st.reported_cost = reported;
                                st.cost_usd += cost;
                                if !noop {
                                    st.tokens += turn;
                                }
                                (before, st.tokens, st.cost_usd)
                            };
                            chat.emit(json!({"type": "drifty", "kind": "tokens", "total": total, "cost": cost_total}));
                            let limits = *chat.limits.lock().unwrap();
                            // Jauge des tokens franchie (100 %, 200 %…) : `/compact` passe avant la file.
                            if limits.compact_due(before, total) {
                                chat.state.lock().unwrap().queue.insert(0, (new_id(), "/compact".to_string(), vec![]));
                                chat.emit(json!({"type": "drifty", "kind": "notice", "text": "Jauge des tokens à 100 % : compaction automatique (/compact)."}));
                            }
                            if limits.over_budget(cost_total) {
                                chat.emit(json!({"type": "drifty", "kind": "notice", "text": format!("Budget atteint ({} sur {}) : la discussion s'arrête. Relève le budget ou décoche l'arrêt à 99 % pour continuer.", fr_usd(cost_total), fr_usd(limits.budget_usd))}));
                            }
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
                                let (a, r) = drifty_core::diffstat::for_tool(b["name"].as_str().unwrap_or(""), &b["input"])?;
                                Some(json!({"type": "drifty", "kind": "diff", "tool_id": b["id"], "added": a, "removed": r}))
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
                chat.emit(json!({"type": "drifty", "kind": "exit", "code": code}));
                {
                    let mut st = chat.state.lock().unwrap();
                    st.interrupted = st.status == "working";
                }
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
                        chat.emit(json!({"type": "drifty", "kind": "stderr", "text": line.chars().take(500).collect::<String>()}));
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
                    self.calls.insert(b["id"].as_str().unwrap_or("").to_string(), (name, drifty_core::extract::fnv1a(input.as_bytes()), target));
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
                            out.push(json!({"type": "drifty", "kind": "waste", "what": "loop", "tool": name, "target": target, "count": self.streak}));
                        }
                        // Échec évitable connu : la consigne du catalogue, une fois par type et par chat.
                        if let Some(f) = drifty_core::fixes::avoidable(&body) {
                            if self.hinted.insert(f.key) {
                                out.push(json!({"type": "drifty", "kind": "waste", "what": "avoidable", "tool": name, "target": target,
                                                 "rule": drifty_core::fixes::rule_text(f.key, f.item.as_deref())}));
                            }
                        }
                    } else {
                        self.streak = 0;
                        self.last_error = None;
                    }
                    if body.len() > BIG_OUTPUT {
                        out.push(json!({"type": "drifty", "kind": "waste", "what": "big", "tool": name, "target": target, "bytes": body.len()}));
                    }
                }
            }
            _ => {}
        }
        out
    }
}

/// Emplacement des worktrees créés par DRIFTy : `~/drifty/worktrees/<dépôt>/<nom>` (un seul point).
pub fn worktree_path(repo_name: &str, name: &str) -> std::path::PathBuf {
    drifty_core::home().join("drifty").join("worktrees").join(repo_name).join(name)
}

/// Emplacements où chercher un worktree créé par l'app : l'actuel, puis celui de l'ancien nom.
pub fn worktree_paths(repo_name: &str, name: &str) -> [std::path::PathBuf; 2] {
    [worktree_path(repo_name, name), drifty_core::home().join("claudius").join("worktrees").join(repo_name).join(name)]
}

/// Nom de worktree (et de branche) tiré de ce que l'utilisateur a tapé : lettres accentuées
/// ramenées à leur base (« décisions » → « decisions »), tout autre signe hors `-_.` remplacé
/// par `-` (sans doublon, ni le `..` qu'une branche git refuse), sans `-`/`.` aux bouts,
/// 60 caractères au plus.
pub fn worktree_slug(name: &str) -> String {
    const FROM: &str = "àáâãäåāçćčèéêëēęěìíîïīñńňòóôõöøōùúûüūůýÿžźżšśřłđÀÁÂÃÄÅĀÇĆČÈÉÊËĒĘĚÌÍÎÏĪÑŃŇÒÓÔÕÖØŌÙÚÛÜŪŮÝŸŽŹŻŠŚŘŁĐ";
    const TO: &str = "aaaaaaaccceeeeeeeiiiiinnnooooooouuuuuuyyzzzssrldAAAAAAACCCEEEEEEEIIIIINNNOOOOOOOUUUUUUYYZZZSSRLD";
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = FROM.chars().position(|f| f == c).and_then(|i| TO.chars().nth(i)).unwrap_or(c);
        let pair = match c {
            'œ' => "oe", 'Œ' => "OE", 'æ' => "ae", 'Æ' => "AE", 'ß' => "ss",
            _ => "",
        };
        if !pair.is_empty() {
            out.push_str(pair);
        } else if c.is_ascii_alphanumeric() || c == '_' || (c == '.' && !out.ends_with('.')) {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_start_matches(['-', '.']);
    out.chars().take(60).collect::<String>().trim_end_matches(['-', '.']).to_string()
}

/// Crée un worktree isolé `<racine>/<nom>` sur une nouvelle branche `<nom>`.
pub fn create_worktree(repo_root: &str, repo_name: &str, name: &str) -> Result<String> {
    let typed = name;
    let name = &worktree_slug(typed);
    let ok = !name.is_empty()
        && name.len() <= 60
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with(['-', '.']);
    if !ok {
        anyhow::bail!("The name `{typed}` cannot be used because it contains no letter or digit.");
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

    #[test]
    fn worktree_slug_keeps_what_was_typed_readable() {
        use super::worktree_slug as slug;
        assert_eq!(slug("Silencieux_2_décisions"), "Silencieux_2_decisions");
        assert_eq!(slug("  refonte du parser !"), "refonte-du-parser");
        assert_eq!(slug("--.cœur  été"), "coeur-ete");
        assert_eq!(slug("éé/../x."), "ee-.-x");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug(&"a".repeat(80)).len(), 60);
    }
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
    fn limits_follow_the_chosen_mode() {
        let tokens = super::Limits { tokens_mode: true, tokens_cap: 1000.0, budget_usd: 1.0, auto_compact: true, stop_at_budget: true };
        assert!(!tokens.over_budget(5.0), "en mode tokens, le budget n'arrête rien");
        assert!(tokens.compact_due(900, 1100) && !tokens.compact_due(1100, 1900) && tokens.compact_due(1900, 2100));
        let budget = super::Limits { tokens_mode: false, ..tokens };
        assert!(budget.over_budget(0.99) && !budget.over_budget(0.5));
        assert!(!budget.compact_due(900, 1100), "en mode budget, pas d'auto-compact");
    }

    #[test]
    fn answers_and_plan_mode_in_the_response() {
        use super::permission_response as r;
        let input = json!({"questions": [{"question": "Couleur ?"}]});
        let ok = r(&input, true, Some(&json!({"Couleur ?": "Rouge"})), None, None).unwrap();
        assert_eq!(ok["behavior"], "allow");
        assert_eq!(ok["updatedInput"]["answers"]["Couleur ?"], "Rouge");
        assert_eq!(ok["updatedInput"]["questions"], input["questions"]); // l'entrée d'origine est gardée
        assert!(ok.get("updatedPermissions").is_none());
        let plan = r(&json!({"plan": "# P"}), true, None, Some("acceptEdits"), None).unwrap();
        assert_eq!(plan["updatedPermissions"][0], json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"}));
        assert!(r(&json!({}), true, None, Some("bypassPermissions"), None).is_err()); // pas d'élévation par ce chemin
        let no = r(&input, false, None, None, Some("Continue à planifier : ajoute des tests.")).unwrap();
        assert_eq!(no, json!({"behavior": "deny", "message": "Continue à planifier : ajoute des tests."}));
        assert_eq!(r(&input, false, None, None, None).unwrap()["message"], "Refusé depuis DRIFTy.");
    }

    #[test]
    fn waiting_kinds() {
        assert_eq!(super::waiting_kind("AskUserQuestion"), "question");
        assert_eq!(super::waiting_kind("ExitPlanMode"), "plan");
        assert_eq!(super::waiting_kind("Bash"), "permission");
    }

    #[test]
    fn noop_results_are_spotted() {
        assert!(super::is_noop_result(&json!({"num_turns": 0, "duration_ms": 455, "total_cost_usd": 353.17})));
        assert!(super::is_noop_result(&json!({"num_turns": 1, "duration_ms": 0, "usage": {"output_tokens": 0}})));
        assert!(!super::is_noop_result(&json!({"num_turns": 1, "duration_ms": 2018, "usage": {"output_tokens": 222}})));
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
