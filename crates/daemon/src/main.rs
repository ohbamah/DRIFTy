//! driftyd — démon local : synchronisation périodique + API JSON + interface web.
//! L'interface ne connaît que ce protocole HTTP/JSON : le démon peut être
//! réécrit dans un autre langage sans la toucher.

mod bg;
mod chat;
mod claude_update;
mod commands;
mod evaluate;
mod github;
mod setup;
mod side;
mod term;
mod usage_cmd;
mod web;
mod worktrees;
mod sounds;
mod improve;
mod launch;
mod schedule;
mod models;
mod ports;
mod relay;
mod review;

use anyhow::Result;
use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use drifty_core::{db, digest, query, repos, transcript};
use include_dir::{include_dir, Dir};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

static UI: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../ui");

#[derive(Clone)]
struct App {
    chats: chat::Hub,
    jobs: commands::Jobs,
    models: Arc<models::Cache>,
    read: Arc<Mutex<Connection>>,
    syncing: Arc<AtomicBool>,
    last: Arc<Mutex<Option<(i64, drifty_core::SyncReport)>>>,
    /// Origines de l'interface : `drifty.localhost:<port>` (celle qu'on sert), et les
    /// adresses de bouclage qui y redirigent.
    origins: Arc<Vec<String>>,
    terms: term::Terms,
    web: web::Proxies,
}

struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({"error": self.0.to_string()}))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}

type ApiResult = std::result::Result<Json<Value>, ApiError>;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

async fn with_conn<F>(app: &App, f: F) -> ApiResult
where
    F: FnOnce(&Connection) -> Result<Value> + Send + 'static,
{
    let conn = app.read.clone();
    let v = tokio::task::spawn_blocking(move || {
        let c = conn.lock().map_err(|_| anyhow::anyhow!("The database lock cannot be taken because it is poisoned."))?;
        f(&c)
    })
    .await??;
    Ok(Json(v))
}

/// Lance une synchronisation si aucune n'est en cours. Rend `false` sinon.
fn trigger_sync(app: &App) -> bool {
    if app.syncing.swap(true, Ordering::SeqCst) {
        return false;
    }
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        match db::open(&drifty_core::db_path()).and_then(|mut c| drifty_core::sync(&mut c)) {
            Ok(r) => {
                if r.ingest.files_read > 0 {
                    eprintln!(
                        "synchro : {} fichiers lus, {} messages, {} sessions analysées, {} ms",
                        r.ingest.files_read, r.ingest.messages, r.analysed_sessions, r.millis
                    );
                }
                *app.last.lock().unwrap() = Some((now_ms(), r));
            }
            Err(e) => eprintln!("synchro en échec : {e:#}"),
        }
        app.syncing.store(false, Ordering::SeqCst);
    });
    true
}

/// Une page web ouverte ailleurs peut viser 127.0.0.1 : toute requête qui
/// modifie quelque chose doit venir de l'interface de DRIFTy elle-même.
async fn same_origin(State(app): State<App>, req: Request, next: Next) -> Response {
    if req.method() != Method::GET {
        let origin = req.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok());
        if !app.trusted(origin) {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "The request cannot be accepted because it does not come from the DRIFTy interface."})))
                .into_response();
        }
    }
    next.run(req).await
}

impl App {
    fn trusted(&self, origin: Option<&str>) -> bool {
        origin.is_some_and(|o| self.origins.iter().any(|x| x == o))
    }
}

/// Délai avant de fermer un orphelin (terminal, relais) ; `DRIFTY_REAP_MS` le raccourcit pour les tests.
fn reap_ms(default: i64) -> i64 {
    std::env::var("DRIFTY_REAP_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Port d'écoute d'une adresse `hôte:port`.
fn port_of(addr: &str) -> u16 {
    addr.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(4317)
}

async fn static_file(Path(path): Path<String>) -> Response {
    serve(&path)
}

/// L'interface est servie sous `drifty.localhost` : les pages de l'onglet navigateur
/// passent par des relais `rN.drifty.localhost` du MÊME site (cookies acceptés dans le
/// cadre) mais d'hôtes distincts (cookies isolés d'un site à l'autre). Une adresse de
/// bouclage y redirige.
async fn index(headers: axum::http::HeaderMap) -> Response {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let (name, port) = host.rsplit_once(':').unwrap_or((host, ""));
    if matches!(name, "127.0.0.1" | "localhost" | "[::1]") && !port.is_empty() {
        return axum::response::Redirect::temporary(&format!("http://drifty.localhost:{port}/")).into_response();
    }
    serve("index.html")
}

fn serve(path: &str) -> Response {
    let Some(f) = UI.get_file(path) else { return StatusCode::NOT_FOUND.into_response() };
    let mime = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    };
    let mut res = Response::new(Body::from(f.contents()));
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    res
}

// ------------------------------------------------------------------ routes

#[derive(Deserialize)]
struct OverviewQ {
    #[serde(default = "thirty")]
    days: i64,
    #[serde(default)]
    slot: String,
}
fn thirty() -> i64 {
    30
}

#[derive(Deserialize)]
struct ConvQ {
    worktree: Option<String>,
    rel: Option<String>,
    #[serde(default)]
    sort: String,
}

#[derive(Deserialize)]
struct LaunchReq {
    repo_id: Option<i64>,
    session_id: Option<String>,
}

async fn status(State(app): State<App>) -> ApiResult {
    let last = app.last.lock().unwrap().clone();
    let syncing = app.syncing.load(Ordering::SeqCst);
    // L'heure de la dernière synchro survit au redémarrage du démon (réglage en base).
    with_conn(&app, move |c| {
        let stored: Option<i64> = db::setting(c, "last_sync_at")?.and_then(|v| v.parse().ok());
        Ok(json!({
            "syncing": syncing,
            "last_at": last.as_ref().map(|l| l.0).or(stored),
            "last": last.map(|l| l.1),
        }))
    })
    .await
}

async fn sync_now(State(app): State<App>) -> Json<Value> {
    Json(json!({"started": trigger_sync(&app)}))
}

async fn overview(State(app): State<App>, Query(q): Query<OverviewQ>) -> ApiResult {
    with_conn(&app, move |c| query::overview(c, q.days, &q.slot)).await
}

async fn session(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| query::session(c, &id)).await
}

async fn session_digest(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| digest::digest(c, &id)).await
}

async fn session_messages(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| Ok(serde_json::to_value(transcript::read(c, &id)?)?)).await
}

async fn repo_list(State(app): State<App>) -> ApiResult {
    with_conn(&app, repos::list).await
}

async fn repo_patch(State(app): State<App>, Path(id): Path<i64>, Json(p): Json<repos::RepoPatch>) -> ApiResult {
    with_conn(&app, move |c| {
        repos::patch(c, id, &p)?;
        repos::list(c)
    })
    .await
}

async fn repo_conversations(State(app): State<App>, Path(id): Path<i64>, Query(q): Query<ConvQ>) -> ApiResult {
    with_conn(&app, move |c| repos::conversations(c, id, q.worktree.as_deref(), q.rel.as_deref(), &q.sort)).await
}

async fn launch_terminal(State(app): State<App>, Json(r): Json<LaunchReq>) -> ApiResult {
    with_conn(&app, move |c| {
        let target = match (r.repo_id, r.session_id) {
            (Some(id), None) => repos::launch_new(c, id)?,
            (None, Some(sid)) => repos::launch_resume(c, &sid)?,
            _ => anyhow::bail!("The launch request cannot be handled because it must name exactly one project or one conversation."),
        };
        let cmd = launch::open_terminal(&target)?;
        Ok(json!({"command": cmd}))
    })
    .await
}

#[derive(Deserialize)]
struct FixPath {
    id: String,
    cause: String,
}

async fn fix_propose(State(app): State<App>, Path(p): Path<FixPath>) -> ApiResult {
    with_conn(&app, move |c| drifty_core::fixes::propose(c, &p.id, &p.cause)).await
}

async fn fix_apply(State(app): State<App>, Path(p): Path<FixPath>) -> ApiResult {
    with_conn(&app, move |c| drifty_core::fixes::apply(c, &p.id, &p.cause)).await
}

// ------------------------------------------------------------ chats

#[derive(Deserialize)]
struct NewChat {
    #[serde(default)]
    images: Vec<chat::Image>,
    /// Modèle de la discussion ; absent = réglage du compte.
    model: Option<String>,
    /// Nom affiché de la discussion (sinon : nom du worktree, puis du projet).
    title: Option<String>,
    /// Mode `existing` : worktree déjà connu du projet où démarrer.
    worktree: Option<String>,
    /// Compte imposé (sinon celui du projet) : sert à continuer un travail sur l'autre compte.
    slot: Option<String>,
    repo_id: Option<i64>,
    session_id: Option<String>,
    name: Option<String>,
    /// `worktree` | `project` ; par défaut, le réglage du projet.
    mode: Option<String>,
    /// Avec `session_id` : reprise en branche (nouvelle conversation, l'originale intacte).
    #[serde(default)]
    fork: bool,
    #[serde(default = "yes")]
    bypass: bool,
    text: Option<String>,
}
fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct ChatText {
    #[serde(default)]
    text: String,
    #[serde(default)]
    images: Vec<chat::Image>,
}

#[derive(Deserialize)]
struct ChatDecision {
    request_id: String,
    allow: bool,
    /// Réponses à des questions (`AskUserQuestion`) : question → choix.
    answers: Option<Value>,
    /// Motif d'un refus, lu par Claude.
    message: Option<String>,
}

/// Prépare le lancement (base + git), sans rien démarrer.
fn chat_spec(c: &Connection, r: &NewChat) -> Result<chat::LaunchSpec> {
    if let Some(sid) = &r.session_id {
        // Reprise : dossier d'origine, compte qui détient le transcript.
        let (cwd, slot, title, repo_id): (Option<String>, Option<String>, Option<String>, Option<i64>) = c.query_row(
            "SELECT s.cwd, s.slot,
                    COALESCE(s.title, (SELECT t.preview FROM turns t WHERE t.session_id = s.id AND t.kind = 'prompt' AND t.agent_id = '' ORDER BY t.ts LIMIT 1)),
                    m.repo_id
             FROM sessions s LEFT JOIN cwd_map m ON m.cwd = s.cwd WHERE s.id = ?1",
            rusqlite::params![sid],
            |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)),
        )?;
        let cwd = cwd.ok_or_else(|| anyhow::anyhow!("The conversation cannot be continued because its folder is unknown."))?;
        if !std::path::Path::new(&cwd).is_dir() {
            anyhow::bail!("The conversation cannot be continued because its folder `{cwd}` no longer exists.");
        }
        // Port du worktree de la conversation (aucun pour le dépôt principal).
        let wt: Option<String> = c.query_row("SELECT worktree FROM cwd_map WHERE cwd = ?1", rusqlite::params![cwd], |x| x.get(0)).ok();
        let resume_env = match (repo_id, wt) {
            (Some(rid), Some(w)) => ports::env(ports::for_worktree(c, rid, &w)?),
            _ => vec![],
        };
        return Ok(chat::LaunchSpec {
            repo_id: repo_id.unwrap_or(0),
            name: {
                let n: String = r.title.clone().filter(|t| !t.trim().is_empty())
                    .or(title).map(|t| t.chars().take(80).collect()).unwrap_or_else(|| sid.chars().take(8).collect());
                if r.fork && r.title.is_none() { format!("↳ {n}") } else { n }
            },
            config_dir: repos::account_dir(c, slot.as_deref())?,
            slot,
            cwd,
            resume: Some(sid.clone()),
            bypass: r.bypass,
            worktree: None,
            tokens_base: c.query_row(
                "SELECT COALESCE(SUM(input + cache_write + cache_read + output), 0) FROM messages WHERE session_id = ?1",
                rusqlite::params![sid],
                |x| x.get(0),
            )?,
            cost_base: c
                .query_row("SELECT COALESCE(cost_usd, 0) FROM sessions WHERE id = ?1", rusqlite::params![sid], |x| x.get(0))
                .unwrap_or(0.0),
            env: resume_env,
            new_worktree: false,
            setup_job: None,
            fork: r.fork,
            model: r.model.clone().filter(|m| !m.trim().is_empty()),
        });
    }
    let repo_id = r.repo_id.ok_or_else(|| anyhow::anyhow!("The chat cannot start because it names neither a project nor a conversation."))?;
    let (root, repo_name, slot, default_mode): (String, String, Option<String>, String) = c.query_row(
        "SELECT root, name, slot, chat_mode FROM repos WHERE id = ?1",
        rusqlite::params![repo_id],
        |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)),
    )?;
    let slot = r.slot.clone().filter(|s| !s.is_empty()).or(slot)
        .or(db::default_slot(c)?)
        .ok_or_else(|| anyhow::anyhow!("The chat cannot start because no account is configured."))?;
    let config_dir = repos::account_dir(c, Some(&slot))?
        .ok_or_else(|| anyhow::anyhow!("The chat cannot start because the `{slot}` account is not configured."))?;
    let name = r.name.clone().unwrap_or_default().trim().to_string();
    let mode = r.mode.clone().unwrap_or(default_mode);
    let (cwd, worktree) = match mode.as_str() {
        "worktree" => {
            let dir_name = std::path::Path::new(&root).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(repo_name.clone());
            (chat::create_worktree(&root, &dir_name, &name)?, Some(name.clone()))
        }
        "project" => (root.clone(), None),
        // Worktree déjà connu du projet : son dossier racine (sous-dossier en dernier recours).
        "existing" => {
            let wt = r.worktree.clone().unwrap_or_default();
            if wt.is_empty() {
                (root.clone(), None)
            } else {
                let cwd: Option<String> = c
                    .query_row(
                        "SELECT cwd FROM cwd_map WHERE repo_id = ?1 AND worktree = ?2 AND present = 1 ORDER BY rel <> '', length(cwd) LIMIT 1",
                        rusqlite::params![repo_id, wt],
                        |x| x.get(0),
                    )
                    .ok();
                // Pas encore analysé (créé dans l'app) : emplacement des worktrees de DRIFTy.
                let dir_name = std::path::Path::new(&root).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(repo_name.clone());
                let own = chat::worktree_paths(&dir_name, &wt);
                let cwd = cwd
                    .filter(|p| std::path::Path::new(p).is_dir())
                    .or_else(|| own.iter().find(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()))
                    .ok_or_else(|| {
                    anyhow::anyhow!("The worktree `{wt}` cannot be used because its folder is unknown or no longer exists.")
                })?;
                (cwd, Some(wt))
            }
        }
        m => anyhow::bail!("The chat mode `{m}` cannot be used because only `worktree`, `project` and `existing` exist."),
    };
    let title = r.title.clone().map(|t| t.trim().chars().take(80).collect::<String>()).filter(|t| !t.is_empty());
    let new_env = ports::env(ports::for_worktree(c, repo_id, worktree.as_deref().unwrap_or(""))?);
    Ok(chat::LaunchSpec {
        repo_id,
        name: title.unwrap_or_else(|| if name.is_empty() { worktree.clone().unwrap_or(repo_name) } else { name }),
        cwd,
        slot: Some(slot),
        config_dir: Some(config_dir),
        resume: None,
        bypass: r.bypass,
        worktree,
        tokens_base: 0,
        cost_base: 0.0,
        env: new_env,
        new_worktree: mode == "worktree",
        setup_job: None,
        fork: false,
        model: r.model.clone().filter(|m| !m.trim().is_empty()),
    })
}

/// Spécification de reprise d'une conversation (partagée avec les messages programmés).
fn resume_spec(c: &Connection, sid: &str, model: Option<String>) -> Result<chat::LaunchSpec> {
    let r = NewChat {
        images: vec![], model, title: None, worktree: None, slot: None, repo_id: None,
        session_id: Some(sid.to_string()), name: None, mode: None, bypass: true, text: None, fork: false,
    };
    chat_spec(c, &r)
}

async fn scheduled_create(State(app): State<App>, Json(n): Json<schedule::NewScheduled>) -> ApiResult {
    let chat_session = n.chat_id.as_deref().and_then(|id| app.chats.get(id).ok()).and_then(|c| c.summary()["session_id"].as_str().map(str::to_string));
    with_conn(&app, move |c| schedule::create(c, &n, chat_session)).await
}

#[derive(Deserialize)]
struct SchedQ {
    chat: Option<String>,
    session: Option<String>,
}

async fn scheduled_list(State(app): State<App>, Query(q): Query<SchedQ>) -> ApiResult {
    with_conn(&app, move |c| schedule::list(c, q.chat.as_deref(), q.session.as_deref())).await
}

async fn scheduled_cancel(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| {
        schedule::cancel(c, id)?;
        Ok(json!({"ok": true}))
    })
    .await
}

// ------------------------------------------------------------ revue du worktree

#[derive(Deserialize)]
struct ReviewTarget {
    chat: Option<String>,
    session: Option<String>,
    message: Option<String>,
}

/// Le dossier d'une conversation : celui du chat en direct, sinon celui du transcript.
async fn review_dir(app: &App, t: &ReviewTarget) -> std::result::Result<(String, Option<String>), ApiError> {
    if let Some(id) = &t.chat {
        let chat = app.chats.get(id)?;
        return Ok((chat.cwd.clone(), chat.slot.clone()));
    }
    let sid = t.session.clone().ok_or_else(|| anyhow::anyhow!("The changes cannot be shown because no conversation is named."))?;
    let v = with_conn(app, move |c| {
        Ok(c.query_row("SELECT cwd, slot FROM sessions WHERE id = ?1", rusqlite::params![sid], |r| {
            Ok(json!([r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?]))
        })?)
    })
    .await?
    .0;
    let dir = v[0].as_str().map(str::to_string).ok_or_else(|| anyhow::anyhow!("The changes cannot be shown because the folder is unknown."))?;
    Ok((dir, v[1].as_str().map(str::to_string)))
}

async fn review_state(State(app): State<App>, Query(t): Query<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    Ok(Json(tokio::task::spawn_blocking(move || review::state(&dir)).await??))
}

async fn review_commit(State(app): State<App>, Json(t): Json<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    let msg = t.message.clone().unwrap_or_default();
    Ok(Json(tokio::task::spawn_blocking(move || review::commit(&dir, &msg)).await??))
}

async fn review_merge(State(app): State<App>, Json(t): Json<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    Ok(Json(tokio::task::spawn_blocking(move || review::merge(&dir)).await??))
}

async fn review_pr(State(app): State<App>, Json(t): Json<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    Ok(Json(tokio::task::spawn_blocking(move || review::pull_request(&dir, false)).await??))
}

/// Message de commit proposé par l'IA d'après le diff (dépense propre de DRIFTy).
async fn review_message(State(app): State<App>, Json(t): Json<ReviewTarget>) -> ApiResult {
    let (dir, slot) = review_dir(&app, &t).await?;
    let summary = tokio::task::spawn_blocking(move || review::diff_for_message(&dir)).await??;
    let config_dir = with_conn(&app, move |c| Ok(json!(repos::account_dir(c, slot.as_deref())?))).await?.0;
    const SYSTEM: &str = "Écris un message de commit git pour ces changements : une ligne de résumé (72 caractères au plus, à l'impératif), \
puis si utile une ligne vide et 1 à 4 puces brèves. Même langue que le code et les commentaires. Réponds uniquement avec le message.";
    let model = std::env::var("DRIFTY_IMPROVE_MODEL").unwrap_or_else(|_| "haiku".into());
    let r = improve::oneshot(SYSTEM, &summary, config_dir.as_str(), &model, "The commit message cannot be generated").await?;
    let (i, o, cost) = (r.input_tokens, r.output_tokens, r.cost_usd);
    let _ = with_conn(&app, move |c| {
        drifty_core::ledger::record_spend(c, "commit-message", None, i, o, cost)?;
        Ok(json!({}))
    })
    .await?;
    Ok(Json(json!({"message": r.text})))
}

// ------------------------------------------------------------ commandes du projet (test, lancement)

async fn commands_get(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| Ok(db::setting(c, &format!("cmd:{id}"))?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or(json!({"test": "", "run": ""})))).await
}

async fn commands_set(State(app): State<App>, Path(id): Path<i64>, Json(v): Json<Value>) -> ApiResult {
    with_conn(&app, move |c| {
        let clean = json!({"test": v["test"].as_str().unwrap_or("").trim(), "run": v["run"].as_str().unwrap_or("").trim()});
        db::set_setting(c, &format!("cmd:{id}"), &clean.to_string())?;
        Ok(clean)
    })
    .await
}

/// « Analyse des lancements » : l'IA déduit les commandes d'après un résumé du dépôt.
async fn commands_analyze(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    let v = with_conn(&app, move |c| {
        let (root, slot): (String, Option<String>) = c.query_row("SELECT root, slot FROM repos WHERE id = ?1", rusqlite::params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let slot = slot.or(db::default_slot(c)?);
        Ok(json!([commands::repo_summary(&root), repos::account_dir(c, slot.as_deref())?]))
    })
    .await?
    .0;
    let model = std::env::var("DRIFTY_IMPROVE_MODEL").unwrap_or_else(|_| "sonnet".into());
    let r = improve::oneshot(commands::ANALYZE_SYSTEM, v[0].as_str().unwrap_or(""), v[1].as_str(), &model, "The launch commands cannot be analysed").await?;
    let found = commands::parse_analysis(&r.text)?;
    let (i, o, cost) = (r.input_tokens, r.output_tokens, r.cost_usd);
    let saved = found.clone();
    let _ = with_conn(&app, move |c| {
        drifty_core::ledger::record_spend(c, "launch-analysis", None, i, o, cost)?;
        db::set_setting(c, &format!("cmd:{id}"), &json!({"test": saved["test"], "run": saved["run"]}).to_string())?;
        Ok(json!({}))
    })
    .await?;
    Ok(Json(found))
}

#[derive(Deserialize)]
struct RunReq {
    chat: Option<String>,
    session: Option<String>,
    command: String,
}

/// Variables d'un dossier de conversation (port de son worktree) : un seul point, pour les
/// commandes, le terminal intégré et l'onglet navigateur.
async fn worktree_env(app: &App, chat: Option<&str>, dir: &str) -> std::result::Result<(Vec<(String, String)>, Option<u16>), ApiError> {
    // Projet et worktree du dossier : celui du chat, sinon celui connu par l'analyse.
    let chat_ctx = chat.and_then(|id| app.chats.get(id).ok()).map(|c| (c.repo_id, c.worktree.clone().unwrap_or_default()));
    let d = dir.to_string();
    let port = with_conn(app, move |c| {
        let ctx = chat_ctx.or_else(|| {
            c.query_row("SELECT repo_id, worktree FROM cwd_map WHERE cwd = ?1", rusqlite::params![d], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?))).ok()
        });
        Ok(json!(match ctx {
            Some((rid, wt)) => ports::for_worktree(c, rid, &wt)?,
            None => None,
        }))
    })
    .await?
    .0
    .as_u64()
    .map(|p| p as u16);
    Ok((ports::env(port), port))
}

async fn run_start(State(app): State<App>, Json(r): Json<RunReq>) -> ApiResult {
    let (dir, _) = review_dir(&app, &ReviewTarget { chat: r.chat.clone(), session: r.session.clone(), message: None }).await?;
    let (env, port) = worktree_env(&app, r.chat.as_deref(), &dir).await?;
    let id = app.jobs.start(&dir, &r.command, &env)?;
    Ok(Json(json!({"id": id, "port": port, "hardcoded": port.and_then(|_| ports::hardcoded(&dir))})))
}

#[derive(Deserialize)]
struct RunQ {
    #[serde(default)]
    from: usize,
}

async fn run_read(State(app): State<App>, Path(id): Path<String>, Query(q): Query<RunQ>) -> ApiResult {
    Ok(Json(app.jobs.read(&id, q.from)?))
}

async fn run_stop(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.jobs.stop(&id)?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct SearchQ {
    q: String,
}

async fn search(State(app): State<App>, Query(q): Query<SearchQ>) -> ApiResult {
    with_conn(&app, move |c| drifty_core::search::search(c, &q.q, 40)).await
}

// ------------------------------------------------------------ prompts sauvegardés

#[derive(Deserialize)]
struct PromptsQ {
    repo: Option<i64>,
}

async fn prompts_list(State(app): State<App>, Query(q): Query<PromptsQ>) -> ApiResult {
    with_conn(&app, move |c| {
        let mut st = c.prepare(
            "SELECT p.id, p.title, p.body, p.repo_id, r.name FROM prompts p LEFT JOIN repos r ON r.id = p.repo_id
             WHERE p.repo_id IS NULL OR p.repo_id = ?1 ORDER BY p.used DESC, p.created DESC",
        )?;
        let rows: Vec<Value> = st
            .query_map(rusqlite::params![q.repo], |r| {
                Ok(json!({"id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?, "body": r.get::<_, String>(2)?,
                          "repo_id": r.get::<_, Option<i64>>(3)?, "repo": r.get::<_, Option<String>>(4)?}))
            })?
            .collect::<Result<_, _>>()?;
        Ok(Value::Array(rows))
    })
    .await
}

#[derive(Deserialize)]
struct NewPrompt {
    title: String,
    body: String,
    repo_id: Option<i64>,
}

async fn prompts_create(State(app): State<App>, Json(p): Json<NewPrompt>) -> ApiResult {
    with_conn(&app, move |c| {
        let (title, body) = (p.title.trim(), p.body.trim());
        if title.is_empty() || title.chars().count() > 80 {
            anyhow::bail!("The prompt cannot be saved because its name must be 1 to 80 characters long.");
        }
        if body.is_empty() {
            anyhow::bail!("The prompt cannot be saved because it is empty.");
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        c.execute("INSERT INTO prompts(title, body, repo_id, created) VALUES (?1, ?2, ?3, ?4)", rusqlite::params![title, body, p.repo_id, now])?;
        Ok(json!({"id": c.last_insert_rowid()}))
    })
    .await
}

async fn prompts_delete(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| {
        c.execute("DELETE FROM prompts WHERE id = ?1", rusqlite::params![id])?;
        Ok(json!({"ok": true}))
    })
    .await
}

/// Un prompt appliqué remonte dans la liste (les plus utilisés d'abord).
async fn prompts_used(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| {
        c.execute("UPDATE prompts SET used = used + 1 WHERE id = ?1", rusqlite::params![id])?;
        Ok(json!({"ok": true}))
    })
    .await
}

// ------------------------------------------------------------ renommer, archiver

#[derive(Deserialize)]
struct TitleReq {
    title: String,
}

async fn conv_rename(State(app): State<App>, Path(id): Path<String>, Json(t): Json<TitleReq>) -> ApiResult {
    let sid = id.clone();
    let title = t.title.trim().to_string();
    let v = with_conn(&app, move |c| {
        drifty_core::convs::rename(c, &sid, &title)?;
        Ok(json!({"ok": true, "title": title}))
    })
    .await?;
    // Une discussion en direct sur cette conversation prend le même nom.
    for chat in app.chats.list() {
        if chat["session_id"] == id.as_str() {
            if let Ok(ch) = app.chats.get(chat["id"].as_str().unwrap_or("")) {
                ch.rename(t.title.trim());
            }
        }
    }
    Ok(v)
}

/// Discussion en direct : son nom, et celui de sa conversation si elle en a déjà une.
async fn chat_rename(State(app): State<App>, Path(id): Path<String>, Json(t): Json<TitleReq>) -> ApiResult {
    let chat = app.chats.get(&id)?;
    let title = t.title.trim().to_string();
    if title.is_empty() || title.chars().count() > 120 || title.contains(['\n', '\r']) {
        return Err(anyhow::anyhow!("The conversation cannot be renamed because its name must be 1 to 120 characters on one line.").into());
    }
    if let Some(sid) = chat.session_id() {
        let known = with_conn(&app, {
            let sid = sid.clone();
            move |c| Ok(json!(c.query_row("SELECT 1 FROM sessions WHERE id = ?1", rusqlite::params![sid], |_| Ok(())).is_ok()))
        })
        .await?
        .0;
        if known == true {
            let t2 = title.clone();
            let _ = with_conn(&app, move |c| {
                drifty_core::convs::rename(c, &sid, &t2)?;
                Ok(json!({}))
            })
            .await?;
        }
    }
    chat.rename(&title);
    Ok(Json(json!({"ok": true, "title": title})))
}

async fn conv_archive(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        drifty_core::convs::archive(c, &id, now_ms())?;
        Ok(json!({"ok": true}))
    })
    .await
}

async fn conv_unarchive(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        drifty_core::convs::unarchive(c, &id)?;
        Ok(json!({"ok": true}))
    })
    .await
}

async fn repo_archived(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| drifty_core::convs::archived(c, id)).await
}

async fn chat_list(State(app): State<App>) -> Json<Value> {
    Json(Value::Array(app.chats.list()))
}

async fn chat_start(State(app): State<App>, Json(r): Json<NewChat>) -> ApiResult {
    chat::check_images(&r.images)?;
    let text = r.text.clone().unwrap_or_default();
    let first = (!text.trim().is_empty() || !r.images.is_empty()).then(|| (text, r.images.clone()));
    let conn = app.read.clone();
    let (mut spec, setup) = tokio::task::spawn_blocking(move || -> Result<_> {
        let c = conn.lock().map_err(|_| anyhow::anyhow!("The database lock cannot be taken because it is poisoned."))?;
        let spec = chat_spec(&c, &r)?;
        // Worktree tout neuf : script de mise en place du projet, et dossier principal.
        let setup = if spec.new_worktree {
            setup::script_for(&c, spec.repo_id)?.map(|script| -> Result<_> {
                let root: String = c.query_row("SELECT root FROM repos WHERE id = ?1", rusqlite::params![spec.repo_id], |x| x.get(0))?;
                Ok((root, script))
            }).transpose()?
        } else {
            None
        };
        Ok((spec, setup))
    })
    .await??;
    if let Some((root, script)) = &setup {
        let wt = spec.worktree.clone().unwrap_or_default();
        spec.setup_job = Some(setup::start(&app, root, &spec.cwd, &wt, script, &spec.env)?);
    }
    // Le premier message attend la fin de la mise en place (dépendances, .env…).
    let job = spec.setup_job.clone();
    let chat = app.chats.start(spec, if job.is_some() { None } else { first.clone() }).await?;
    if let (Some(job), Some((text, images))) = (job, first) {
        let (jobs, chat) = (app.jobs.clone(), chat.clone());
        tokio::spawn(async move {
            jobs.wait(&job).await;
            let _ = chat.send(&text, images).await;
        });
    }
    Ok(Json(chat.summary()))
}

async fn chat_bash(State(app): State<App>, Path(id): Path<String>, Json(t): Json<ChatText>) -> ApiResult {
    app.chats.get(&id)?.bash(&t.text).await?;
    Ok(Json(json!({"ok": true})))
}

async fn chat_send(State(app): State<App>, Path(id): Path<String>, Json(t): Json<ChatText>) -> ApiResult {
    app.chats.get(&id)?.send(&t.text, t.images).await?;
    Ok(Json(json!({"ok": true})))
}

async fn chat_decide(State(app): State<App>, Path(id): Path<String>, Json(d): Json<ChatDecision>) -> ApiResult {
    app.chats.get(&id)?.decide(&d.request_id, d.allow, d.answers, d.message).await?;
    Ok(Json(json!({"ok": true})))
}

async fn chat_stop(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.chats.get(&id)?.interrupt().await?;
    Ok(Json(json!({"ok": true})))
}

async fn chat_close(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.chats.remove(&id).await?;
    app.terms.close_for_chat(&id); // ses terminaux ne lui survivent pas
    Ok(Json(json!({"ok": true})))
}

async fn chat_send_now(State(app): State<App>, Path((id, qid)): Path<(String, String)>) -> ApiResult {
    app.chats.get(&id)?.send_now(&qid).await?;
    Ok(Json(json!({"ok": true})))
}

async fn chat_unqueue(State(app): State<App>, Path((id, qid)): Path<(String, String)>) -> ApiResult {
    let text = app.chats.get(&id)?.unqueue(&qid)?;
    Ok(Json(json!({"text": text})))
}

#[derive(Deserialize)]
struct ImproveReq {
    text: String,
    repo_id: Option<i64>,
    chat_id: Option<String>,
    session_id: Option<String>,
}

/// Améliore un prompt avec le compte du contexte (chat, conversation ou projet).
async fn improve_prompt(State(app): State<App>, Json(r): Json<ImproveReq>) -> ApiResult {
    let chat_slot = match &r.chat_id {
        Some(id) => Some(app.chats.get(id)?.slot.clone()),
        None => None,
    };
    let conn = app.read.clone();
    let (repo_id, sid) = (r.repo_id, r.session_id.clone());
    let (config_dir, project) = tokio::task::spawn_blocking(move || -> Result<(Option<String>, Option<String>)> {
        let c = conn.lock().map_err(|_| anyhow::anyhow!("The database lock cannot be taken because it is poisoned."))?;
        let (slot, project): (Option<String>, Option<String>) = if let Some(s) = chat_slot {
            (s, None)
        } else if let Some(sid) = &sid {
            c.query_row(
                "SELECT s.slot, r.name FROM sessions s LEFT JOIN cwd_map m ON m.cwd = s.cwd LEFT JOIN repos r ON r.id = m.repo_id WHERE s.id = ?1",
                rusqlite::params![sid],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )?
        } else if let Some(id) = repo_id {
            c.query_row("SELECT slot, name FROM repos WHERE id = ?1", rusqlite::params![id], |x| Ok((x.get(0)?, x.get(1)?)))?
        } else {
            (None, None)
        };
        let slot = slot.or(db::default_slot(&c)?);
        Ok((repos::account_dir(&c, slot.as_deref())?, project))
    })
    .await??;
    let out = improve::run(&r.text, project.as_deref(), config_dir.as_deref()).await?;
    let conn = app.read.clone();
    let (i, o, cost, sid) = (out.input_tokens, out.output_tokens, out.cost_usd, r.session_id.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let c = conn.lock().map_err(|_| anyhow::anyhow!("The database lock cannot be taken because it is poisoned."))?;
        drifty_core::ledger::record_spend(&c, "improve", sid.as_deref(), i, o, cost)
    })
    .await??;
    Ok(Json(json!({"text": out.text, "input_tokens": i, "output_tokens": o, "cost_usd": cost, "ms": out.ms})))
}

fn image_response(media: String, bytes: Vec<u8>) -> Response {
    let mut res = Response::new(Body::from(bytes));
    if let Ok(v) = HeaderValue::from_str(&media) {
        res.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    res
}

async fn chat_image(State(app): State<App>, Path((id, n)): Path<(String, usize)>) -> std::result::Result<Response, ApiError> {
    use base64::Engine;
    let img = app.chats.get(&id)?.image(n).ok_or_else(|| anyhow::anyhow!("The image `{n}` cannot be found in this chat."))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(img.data.as_bytes())?;
    Ok(image_response(img.media_type, bytes))
}

#[derive(Deserialize)]
struct ImageKey {
    k: String,
}

async fn session_image(State(app): State<App>, Path(id): Path<String>, Query(q): Query<ImageKey>) -> std::result::Result<Response, ApiError> {
    let conn = app.read.clone();
    let (media, bytes) = tokio::task::spawn_blocking(move || {
        let c = conn.lock().map_err(|_| anyhow::anyhow!("The database lock cannot be taken because it is poisoned."))?;
        transcript::image(&c, &id, &q.k)
    })
    .await??;
    Ok(image_response(media, bytes))
}

/// Depuis quand le transcript d'une conversation n'a pas été écrit : une écriture
/// récente veut dire qu'elle est sans doute ouverte ailleurs (terminal, autre app).
async fn session_activity(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        let path: Option<String> = c.query_row("SELECT file_path FROM sessions WHERE id = ?1", rusqlite::params![id], |r| r.get(0))?;
        let ago = path
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_millis() as i64);
        Ok(json!({"last_write_ms_ago": ago}))
    })
    .await
}

async fn chat_seen(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.chats.get(&id)?.mark_seen();
    Ok(Json(json!({"ok": true})))
}

// ------------------------------------------------------------ réglage de la barre de contexte

#[derive(Deserialize, serde::Serialize)]
struct ContextBar {
    /// `tokens` (plafond en tokens) ou `budget` (plafond en dollars).
    mode: String,
    tokens_cap: f64,
    budget_usd: f64,
    /// Compacter (`/compact`) quand la jauge des tokens atteint 100 %.
    #[serde(default = "yes")]
    auto_compact: bool,
    /// Arrêter la discussion quand son coût atteint 99 % du budget.
    #[serde(default = "yes")]
    stop_at_budget: bool,
}

impl ContextBar {
    fn limits(&self) -> chat::Limits {
        chat::Limits { tokens_mode: self.mode != "budget", tokens_cap: self.tokens_cap, budget_usd: self.budget_usd, auto_compact: self.auto_compact, stop_at_budget: self.stop_at_budget }
    }
}

fn read_context_bar(c: &Connection) -> Result<ContextBar> {
    let get = |k: &str, d: &str| -> Result<String> { Ok(db::setting(c, k)?.unwrap_or_else(|| d.to_string())) };
    Ok(ContextBar {
        mode: get("ctx_mode", "tokens")?,
        tokens_cap: get("ctx_tokens_cap", "1000000000")?.parse().unwrap_or(1e9),
        budget_usd: get("ctx_budget_usd", "50")?.parse().unwrap_or(50.0),
        auto_compact: get("ctx_auto_compact", "1")? == "1",
        stop_at_budget: get("ctx_stop_budget", "1")? == "1",
    })
}

async fn context_bar_get(State(app): State<App>) -> ApiResult {
    with_conn(&app, |c| Ok(serde_json::to_value(read_context_bar(c)?)?)).await
}

async fn context_bar_set(State(app): State<App>, Json(b): Json<ContextBar>) -> ApiResult {
    let app_limits = app.chats.clone();
    with_conn(&app, move |c| {
        if b.mode != "tokens" && b.mode != "budget" {
            anyhow::bail!("The bar mode `{}` cannot be used because only `tokens` and `budget` exist.", b.mode);
        }
        if !(b.tokens_cap >= 1000.0) || !(b.budget_usd > 0.0) {
            anyhow::bail!("The bar limit cannot be used because it must be at least 1,000 tokens and more than $0.");
        }
        db::set_setting(c, "ctx_mode", &b.mode)?;
        db::set_setting(c, "ctx_tokens_cap", &b.tokens_cap.to_string())?;
        db::set_setting(c, "ctx_budget_usd", &b.budget_usd.to_string())?;
        db::set_setting(c, "ctx_auto_compact", if b.auto_compact { "1" } else { "0" })?;
        db::set_setting(c, "ctx_stop_budget", if b.stop_at_budget { "1" } else { "0" })?;
        Ok(serde_json::to_value(read_context_bar(c)?)?)
    })
    .await
    .inspect(|v| {
        if let Ok(b) = serde_json::from_value::<ContextBar>(v.0.clone()) {
            app_limits.set_limits(b.limits()); // appliqué tout de suite aux discussions en cours
        }
    })
}

#[derive(Deserialize)]
struct ChatModel {
    model: String,
}

async fn chat_set_model(State(app): State<App>, Path(id): Path<String>, Json(m): Json<ChatModel>) -> ApiResult {
    app.chats.get(&id)?.set_model(&m.model).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ModelsQ {
    slot: Option<String>,
}

/// Modèles disponibles pour un compte, tels que Claude Code les annonce.
async fn models(State(app): State<App>, Query(q): Query<ModelsQ>) -> ApiResult {
    let conn = app.read.clone();
    let dir = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        let c = conn.lock().map_err(|_| anyhow::anyhow!("The database lock cannot be taken because it is poisoned."))?;
        let slot = q.slot.filter(|s| !s.is_empty()).or(db::default_slot(&c)?);
        repos::account_dir(&c, slot.as_deref())
    })
    .await??;
    Ok(Json(app.models.get(dir.as_deref(), None).await?["models"].clone()))
}

async fn rules_list(State(app): State<App>, Path(id): Path<i64>) -> ApiResult {
    with_conn(&app, move |c| drifty_core::rule_effects::list(c, id)).await
}

async fn rules_remove(State(app): State<App>, Path((id, key)): Path<(i64, String)>) -> ApiResult {
    with_conn(&app, move |c| {
        drifty_core::rule_effects::remove(c, id, &key)?;
        drifty_core::rule_effects::list(c, id)
    })
    .await
}

async fn evaluation_get(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| evaluate::latest(c, &id)).await
}

async fn evaluation_run(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let sid = id.clone();
    let (prompt, dir) = with_conn(&app, move |c| {
        let (p, d) = evaluate::prepare(c, &sid)?;
        Ok(json!([p, d]))
    })
    .await?
    .0
    .as_array()
    .map(|a| (a[0].as_str().unwrap_or("").to_string(), a[1].as_str().map(str::to_string)))
    .unwrap_or_default();
    let r = evaluate::run(&prompt, dir.as_deref()).await?;
    with_conn(&app, move |c| evaluate::save(c, &id, &r)).await
}

// Réglages de notification : JSON global (`notify`) et surcharges par projet (`notify:repo:<id>`).
async fn notify_get(State(app): State<App>) -> ApiResult {
    with_conn(&app, |c| {
        let global = db::setting(c, "notify")?.and_then(|v| serde_json::from_str::<Value>(&v).ok()).unwrap_or(json!({}));
        let mut repos = serde_json::Map::new();
        let mut st = c.prepare("SELECT k, v FROM settings WHERE k LIKE 'notify:repo:%'")?;
        for (k, v) in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.flatten() {
            if let Ok(val) = serde_json::from_str::<Value>(&v) {
                repos.insert(k.trim_start_matches("notify:repo:").to_string(), val);
            }
        }
        Ok(json!({"global": global, "repos": repos}))
    })
    .await
}

async fn notify_set(State(app): State<App>, Json(v): Json<Value>) -> ApiResult {
    with_conn(&app, move |c| {
        if !v.is_object() {
            anyhow::bail!("The notification settings cannot be saved because they are not an object.");
        }
        db::set_setting(c, "notify", &v.to_string())?;
        Ok(json!({"ok": true}))
    })
    .await
}

async fn notify_set_repo(State(app): State<App>, Path(id): Path<i64>, Json(v): Json<Value>) -> ApiResult {
    with_conn(&app, move |c| {
        let key = format!("notify:repo:{id}");
        if v.is_null() || v.get("sounds").and_then(Value::as_object).is_some_and(|m| m.values().all(Value::is_null)) {
            c.execute("DELETE FROM settings WHERE k = ?1", rusqlite::params![key])?;
        } else {
            db::set_setting(c, &key, &v.to_string())?;
        }
        Ok(json!({"ok": true}))
    })
    .await
}

async fn chat_info(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(app.chats.get(&id)?.summary()))
}

/// Flux en direct d'un chat : l'historique d'abord, puis chaque nouvel événement.
async fn chat_events(
    State(app): State<App>,
    Path(id): Path<String>,
) -> std::result::Result<axum::response::Sse<impl futures_util::Stream<Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>>>, ApiError>
{
    use futures_util::StreamExt;
    let chat = app.chats.get(&id)?;
    let (history, rx) = chat.subscribe();
    let live = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|m| async move { m.ok() });
    let stream = futures_util::stream::iter(history)
        .chain(live)
        .map(|v| Ok(axum::response::sse::Event::default().data(v.to_string())));
    Ok(axum::response::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

/// Flux poussé des discussions en direct : la liste complète à l'ouverture, puis à chaque
/// changement (regroupés sur 150 ms), au lieu d'un relevé toutes les 2 s.
async fn events(State(app): State<App>) -> axum::response::Sse<impl futures_util::Stream<Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>>> {
    let rx = app.chats.watch();
    let hub = app.chats.clone();
    let stream = futures_util::stream::unfold((rx, hub, true), |(mut rx, hub, first)| async move {
        if !first {
            loop {
                match rx.recv().await {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(_) => return None,
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            while rx.try_recv().is_ok() {}
        }
        let ev = axum::response::sse::Event::default().event("chats").data(Value::Array(hub.list()).to_string());
        Some((Ok(ev), (rx, hub, false)))
    });
    axum::response::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn usage(State(app): State<App>) -> ApiResult {
    with_conn(&app, drifty_core::usage::summary).await
}

async fn delete_account(State(app): State<App>, Path(slot): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        db::delete_account(c, &slot)?;
        Ok(serde_json::to_value(db::accounts(c)?)?)
    })
    .await
}

async fn relay_install(State(app): State<App>, Path(slot): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        let exe = std::env::current_exe()?;
        let cmd = drifty_core::usage::install_relay(c, &slot, &exe)?;
        Ok(json!({"command": cmd}))
    })
    .await
}

async fn relay_remove(State(app): State<App>, Path(slot): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        drifty_core::usage::remove_relay(c, &slot)?;
        Ok(json!({"removed": true}))
    })
    .await
}

async fn ledger(State(app): State<App>) -> ApiResult {
    with_conn(&app, drifty_core::ledger::summary).await
}

async fn accounts(State(app): State<App>) -> ApiResult {
    with_conn(&app, |c| Ok(serde_json::to_value(db::accounts(c)?)?)).await
}

async fn set_account(State(app): State<App>, Json(a): Json<db::Account>) -> ApiResult {
    with_conn(&app, move |c| {
        db::set_account(c, &a)?;
        Ok(serde_json::to_value(db::accounts(c)?)?)
    })
    .await
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // Relais de statusline : appelé très souvent par Claude Code, il ne touche pas la base.
    if args.get(1).map(String::as_str) == Some("statusline") {
        std::process::exit(relay::run(&args[2..]));
    }
    let addr = std::env::var("DRIFTY_ADDR").unwrap_or_else(|_| "127.0.0.1:4317".into());
    let sync_only = args.iter().any(|a| a == "--sync-only" || a == "--ingest-only");
    // Le port d'abord : s'il est pris, un autre démon tourne et rien ne doit être déplacé.
    let listener = if sync_only { None } else { Some(tokio::net::TcpListener::bind(&addr).await?) };
    if listener.is_some() {
        // Ancien nom (Claudius) : dossier et base renommés une fois.
        if let Err(e) = drifty_core::migrate_legacy_dir() {
            eprintln!("DRIFTy : l'ancien dossier ~/.claudius ne peut pas être renommé : {e}");
        }
    }
    let db_path = drifty_core::db_path();
    let conn = db::open(&db_path)?;
    db::ensure_default_account(&conn, &drifty_core::home())?;
    // Statusline rebranchée seulement en usage réel (pas avec une base de test, `DRIFTY_DB`).
    if listener.is_some() && std::env::var_os("DRIFTY_DB").is_none() {
        if let Ok(exe) = std::env::current_exe() {
            match drifty_core::usage::migrate_relay_commands(&conn, &exe) {
                Ok(n) if n > 0 => eprintln!("DRIFTy : {n} statusline(s) rebranchée(s) sur {}", exe.display()),
                Err(e) => eprintln!("DRIFTy : statusline non rebranchée : {e}"),
                _ => {}
            }
        }
    }

    if sync_only {
        let mut c = conn;
        let r = drifty_core::sync(&mut c)?;
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }

    let limits = read_context_bar(&conn).map(|b| b.limits()).unwrap_or_default();
    let app = App {
        chats: {
            let h = chat::Hub::default();
            h.set_limits(limits);
            h
        },
        jobs: commands::Jobs::default(),
        models: Arc::new(models::Cache::default()),
        read: Arc::new(Mutex::new(conn)),
        syncing: Arc::new(AtomicBool::new(false)),
        last: Arc::new(Mutex::new(None)),
        origins: Arc::new({
            let p = port_of(&addr);
            let mut v = vec![format!("http://{addr}"), format!("http://drifty.localhost:{p}"), format!("http://127.0.0.1:{p}"), format!("http://localhost:{p}")];
            v.dedup();
            v
        }),
        terms: term::Terms::default(),
        web: web::Proxies::default(),
    };

    // Messages programmés : envoyés à l'heure par le démon.
    schedule::spawn(app.read.clone(), app.chats.clone(), resume_spec);

    // Aucune synchronisation automatique : elle ne part que sur demande
    // (bouton « Synchroniser » de l'interface, ou `driftyd --sync-only`).

    let router = Router::new()
        .route("/", get(index))
        .route("/ui/{*path}", get(static_file))
        .route("/api/status", get(status))
        .route("/api/sync", post(sync_now))
        .route("/api/overview", get(overview))
        .route("/api/ledger", get(ledger))
        .route("/api/usage", get(usage))
        .route("/api/accounts/{slot}", axum::routing::delete(delete_account))
        .route("/api/accounts/{slot}/relay", post(relay_install).delete(relay_remove))
        .route("/api/repos", get(repo_list))
        .route("/api/repos/{id}", put(repo_patch))
        .route("/api/repos/{id}/conversations", get(repo_conversations))
        .route("/api/repos/{id}/rules", get(rules_list))
        .route("/api/repos/{id}/rules/{key}", axum::routing::delete(rules_remove))
        .route("/api/sessions/{id}", get(session))
        .route("/api/sessions/{id}/digest", get(session_digest))
        .route("/api/sessions/{id}/messages", get(session_messages))
        .route("/api/sessions/{id}/image", get(session_image))
        .route("/api/sessions/{id}/activity", get(session_activity))
        .route("/api/sessions/{id}/evaluation", get(evaluation_get).post(evaluation_run))
        .route("/api/chats/{id}/image/{n}", get(chat_image))
        .route("/api/sessions/{id}/fix/{cause}", get(fix_propose).post(fix_apply))
        .route("/api/launch", post(launch_terminal))
        .route("/api/improve", post(improve_prompt))
        .route("/api/settings/context-bar", get(context_bar_get).put(context_bar_set))
        .route("/api/settings/notify", get(notify_get).put(notify_set))
        .route("/api/settings/notify/repo/{id}", put(notify_set_repo))
        .route("/api/sounds", get(sounds::route_list).post(sounds::route_add).layer(axum::extract::DefaultBodyLimit::max(sounds::MAX_BYTES + 1024)))
        .route("/api/sounds/{id}", get(sounds::route_file).delete(sounds::route_delete))
        .route("/api/chats", get(chat_list).post(chat_start))
        .route("/api/chats/{id}", get(chat_info).delete(chat_close))
        .route("/api/chats/{id}/events", get(chat_events))
        .route("/api/chats/{id}/send", post(chat_send))
        .route("/api/chats/{id}/bash", post(chat_bash))
        .route("/api/chats/{id}/permission", post(chat_decide))
        .route("/api/chats/{id}/stop", post(chat_stop))
        .route("/api/chats/{id}/model", post(chat_set_model))
        .route("/api/models", get(models))
        .route("/api/commands", get(side::commands))
        .route("/api/btw", post(side::btw))
        .route("/api/events", get(events))
        .route("/api/background", get(bg::route))
        .route("/api/usage/refresh", post(usage_cmd::refresh))
        .route("/api/claude-update", get(claude_update::route))
        .route("/api/claude-update/run", post(claude_update::run))
        .route("/api/sessions/{id}/title", put(conv_rename))
        .route("/api/chats/{id}/title", put(chat_rename))
        .route("/api/sessions/{id}/archive", post(conv_archive).delete(conv_unarchive))
        .route("/api/repos/{id}/archived", get(repo_archived))
        .route("/api/repos/{id}/worktrees/archive", post(worktrees::archive))
        .route("/api/repos/{id}/worktrees/restore", post(worktrees::restore))
        .route("/api/repos/{id}/worktrees/archived", get(worktrees::list))
        .route("/api/term", post(term::open))
        .route("/api/term/{id}", axum::routing::delete(term::close))
        .route("/api/term/{id}/ws", get(term::ws))
        .route("/api/web", post(web::open))
        .route("/api/web/default", get(web::default_url))
        .route("/api/web/alive", post(web::alive))
        .route("/api/search", get(search))
        .route("/api/prompts", get(prompts_list).post(prompts_create))
        .route("/api/prompts/{id}", axum::routing::delete(prompts_delete))
        .route("/api/prompts/{id}/used", post(prompts_used))
        .route("/api/repos/{id}/commands", get(commands_get).put(commands_set))
        .route("/api/repos/{id}/commands/analyze", post(commands_analyze))
        .route("/api/run", post(run_start))
        .route("/api/run/{id}", get(run_read).delete(run_stop))
        .route("/api/review", get(review_state))
        .route("/api/gh", get(github::get_state))
        .route("/api/gh/reviewers", get(github::get_reviewers))
        .route("/api/gh/log", get(github::get_log))
        .route("/api/branch", get(github::get_branch))
        .route("/api/gh/action", post(github::post_action))
        .route("/api/repos/{id}/setup", get(setup::route_get).put(setup::route_set))
        .route("/api/repos/{id}/setup/analyze", post(setup::route_analyze))
        .route("/api/review/commit", post(review_commit))
        .route("/api/review/merge", post(review_merge))
        .route("/api/review/pr", post(review_pr))
        .route("/api/review/message", post(review_message))
        .route("/api/scheduled", get(scheduled_list).post(scheduled_create))
        .route("/api/scheduled/{id}", axum::routing::delete(scheduled_cancel))
        .route("/api/chats/{id}/seen", post(chat_seen))
        .route("/api/chats/{id}/queue/{qid}", axum::routing::delete(chat_unqueue))
        .route("/api/chats/{id}/queue/{qid}/now", post(chat_send_now))
        .route("/api/accounts", get(accounts).put(set_account))
        .layer(middleware::from_fn_with_state(app.clone(), same_origin))
        .with_state(app.clone());
    let (app_terms, app_web) = (app.terms.clone(), app.web.clone());

    // Orphelins : terminaux sans page depuis 2 min, relais sans activité depuis 3 min.
    {
        let (terms, web) = (app_terms, app_web);
        tokio::spawn(async move {
            let mut t = tokio::time::interval(std::time::Duration::from_millis(reap_ms(30_000).clamp(1_000, 30_000) as u64));
            loop {
                t.tick().await;
                terms.reap();
                web.reap();
            }
        });
    }
    let listener = listener.expect("listener bound above");
    // `drifty.localhost` se résout d'abord en ::1 : on y écoute aussi (pas de détour par un échec).
    if addr.starts_with("127.0.0.1:") {
        if let Ok(l6) = tokio::net::TcpListener::bind(("::1", port_of(&addr))).await {
            let r = router.clone();
            tokio::spawn(async move {
                let _ = axum::serve(l6, r).await;
            });
        }
    }
    eprintln!("DRIFTy : http://drifty.localhost:{}  (base : {})", port_of(&addr), db_path.display());
    axum::serve(listener, router).await?;
    Ok(())
}
