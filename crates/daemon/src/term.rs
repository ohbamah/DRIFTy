//! Terminal intégré : un shell de connexion dans un pseudo-terminal, dans le dossier
//! d'une conversation (avec le port de son worktree), relié à la page par un WebSocket.
//! Le shell survit à la page (changement d'onglet ou de conversation) jusqu'à ce qu'on
//! ferme son onglet ; la fin de sa sortie est rejouée à la reconnexion.

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

use crate::{review_dir, worktree_env, ApiResult, App, ReviewTarget};

const REPLAY: usize = 256 * 1024;

pub struct Term {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    replay: Mutex<Vec<u8>>,
    tx: broadcast::Sender<Vec<u8>>,
    done: AtomicBool,
    /// Discussion en direct à laquelle il appartient (fermé avec elle).
    chat: Option<String>,
    /// Pages connectées, et depuis quand il n'y en a plus (ms ; 0 = jamais seul).
    clients: AtomicUsize,
    alone_since: AtomicI64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Un terminal sans page connectée depuis ce délai est fermé (onglet perdu au rechargement).
const ALONE_MS: i64 = 120_000;

#[derive(Default, Clone)]
pub struct Terms(Arc<Mutex<HashMap<String, Arc<Term>>>>);

impl Terms {
    fn get(&self, id: &str) -> Result<Arc<Term>> {
        self.0.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow::anyhow!("The terminal `{id}` cannot be found because it was closed."))
    }

    pub fn open(&self, dir: &str, env: &[(String, String)], cols: u16, rows: u16, chat: Option<String>) -> Result<String> {
        if !std::path::Path::new(dir).is_dir() {
            anyhow::bail!("The terminal cannot open because the folder `{dir}` no longer exists.");
        }
        let size = PtySize { rows: rows.clamp(5, 500), cols: cols.clamp(10, 1000), pixel_width: 0, pixel_height: 0 };
        let pair = native_pty_system().openpty(size).context("The terminal cannot open because no pseudo-terminal is available")?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.cwd(dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).context("The terminal cannot open because the shell could not start")?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (tx, _) = broadcast::channel(1024);
        let term = Arc::new(Term {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            replay: Mutex::new(Vec::new()),
            tx,
            done: AtomicBool::new(false),
            chat,
            clients: AtomicUsize::new(0),
            alone_since: AtomicI64::new(now_ms()),
        });
        let id = format!("t{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        self.0.lock().unwrap().insert(id.clone(), term.clone());
        // Lecture bloquante du pseudo-terminal, dans son propre fil.
        let (map, tid) = (self.0.clone(), id.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = buf[..n].to_vec();
                        {
                            let mut r = term.replay.lock().unwrap();
                            r.extend_from_slice(&chunk);
                            if r.len() > REPLAY {
                                let cut = r.len() - REPLAY;
                                r.drain(..cut);
                            }
                        }
                        let _ = term.tx.send(chunk);
                    }
                }
            }
            term.done.store(true, Ordering::SeqCst);
            let _ = term.tx.send(Vec::new()); // fin : les pages ferment leur connexion
            map.lock().unwrap().remove(&tid); // shell terminé : il quitte le registre
        });
        Ok(id)
    }

    pub fn close(&self, id: &str) {
        let t = self.0.lock().unwrap().remove(id);
        if let Some(t) = t {
            let _ = t.child.lock().unwrap().kill();
        }
    }

    /// Les terminaux d'une discussion qu'on ferme.
    pub fn close_for_chat(&self, chat: &str) {
        let ids: Vec<String> = self.0.lock().unwrap().iter().filter(|(_, t)| t.chat.as_deref() == Some(chat)).map(|(k, _)| k.clone()).collect();
        for id in ids {
            self.close(&id);
        }
    }

    /// Ferme les terminaux restés sans page trop longtemps.
    pub fn reap(&self) {
        let now = now_ms();
        let ids: Vec<String> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| t.clients.load(Ordering::SeqCst) == 0 && now - t.alone_since.load(Ordering::SeqCst) > crate::reap_ms(ALONE_MS))
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            self.close(&id);
        }
    }
}

impl Term {
    fn input(&self, data: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(data);
        let _ = w.flush();
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.lock().unwrap().resize(PtySize { rows: rows.clamp(5, 500), cols: cols.clamp(10, 1000), pixel_width: 0, pixel_height: 0 });
    }
}

// ------------------------------------------------------------ routes

#[derive(Deserialize)]
pub struct OpenReq {
    chat: Option<String>,
    session: Option<String>,
    #[serde(default = "cols")]
    cols: u16,
    #[serde(default = "rows")]
    rows: u16,
}
fn cols() -> u16 {
    120
}
fn rows() -> u16 {
    32
}

pub async fn open(State(app): State<App>, Json(r): Json<OpenReq>) -> ApiResult {
    let (dir, _) = review_dir(&app, &ReviewTarget { chat: r.chat.clone(), session: r.session.clone(), message: None }).await?;
    let (env, port) = worktree_env(&app, r.chat.as_deref(), &dir).await?;
    let id = app.terms.open(&dir, &env, r.cols, r.rows, r.chat.clone())?;
    Ok(Json(json!({"id": id, "cwd": dir, "port": port})))
}

pub async fn close(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    app.terms.close(&id);
    Ok(Json(json!({"ok": true})))
}

/// Connexion de la page. Un WebSocket n'est pas soumis aux règles d'origine du navigateur :
/// l'origine est vérifiée ici, sinon n'importe quel site ouvert pourrait taper dans le shell.
pub async fn ws(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap, up: WebSocketUpgrade) -> Response {
    if !app.trusted(headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())) {
        return (StatusCode::FORBIDDEN, "The terminal cannot be reached from another origin.").into_response();
    }
    let Ok(term) = app.terms.get(&id) else { return (StatusCode::NOT_FOUND, "The terminal was closed.").into_response() };
    up.on_upgrade(move |socket| pump(socket, term))
}

async fn pump(socket: WebSocket, term: Arc<Term>) {
    term.clients.fetch_add(1, Ordering::SeqCst);
    pump_inner(socket, &term).await;
    if term.clients.fetch_sub(1, Ordering::SeqCst) == 1 {
        term.alone_since.store(now_ms(), Ordering::SeqCst);
    }
}

async fn pump_inner(mut socket: WebSocket, term: &Term) {
    let mut rx = term.tx.subscribe();
    let replay = term.replay.lock().unwrap().clone();
    if !replay.is_empty() && socket.send(Message::Binary(replay.into())).await.is_err() {
        return;
    }
    if term.done.load(Ordering::SeqCst) {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Ok(chunk) if chunk.is_empty() => { let _ = socket.send(Message::Close(None)).await; break; }
                Ok(chunk) => if socket.send(Message::Binary(chunk.into())).await.is_err() { break },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else { continue };
                    match v["t"].as_str() {
                        Some("i") => term.input(v["d"].as_str().unwrap_or("").as_bytes()),
                        Some("r") => term.resize(v["c"].as_u64().unwrap_or(120) as u16, v["r"].as_u64().unwrap_or(32) as u16),
                        _ => {}
                    }
                }
                Some(Ok(Message::Binary(b))) => term.input(&b),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}
