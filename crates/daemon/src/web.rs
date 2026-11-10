//! Onglet navigateur : une page web dans un cadre de l'interface. Beaucoup de sites (et
//! Rails en dev) refusent d'être affichés dans un cadre (`X-Frame-Options`,
//! `frame-ancestors`) : toute page passe donc par un relais local, un par origine visée
//! (http ou https), qui :
//! - retire ces en-têtes de cadre ;
//! - garde la navigation dans le relais (redirections, URLs absolues du site réécrites,
//!   cookies rendus valables pour le relais) ;
//! - injecte un petit script (`inject.js`) : adresse réelle et titre de la page, et
//!   sélecteur d'éléments pour en faire des prompts.
//!
//! Les WebSockets (rechargement à chaud, ActionCable) passent aussi : la connexion montée
//! est relayée dans les deux sens. Une navigation vers une autre origine passe par
//! `/__drifty/go?u=…`, qui redirige (307, méthode et corps gardés) vers le relais de
//! cette origine.

use anyhow::Result;
use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http_body_util::BodyExt;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::{review_dir, worktree_env, ApiResult, App, ReviewTarget};

type Https = hyper_rustls::HttpsConnector<HttpConnector>;
const INJECT: &str = include_str!("inject.js");
const MAX_REWRITE: usize = 25 * 1024 * 1024;
const HOP: [&str; 7] = ["connection", "keep-alive", "transfer-encoding", "te", "trailer", "proxy-authenticate", "proxy-connection"];

/// Relais vivants, par origine visée.
#[derive(Default, Clone)]
pub struct Proxies(Arc<Mutex<HashMap<String, Live>>>);

struct Live {
    port: u16,
    last: Arc<AtomicI64>,
    sockets: Arc<AtomicUsize>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Un relais sans requête, sans WebSocket ouvert et sans page qui le montre depuis ce délai est arrêté.
const IDLE_MS: i64 = 180_000;

/// Hôte d'un relais : un nom par relais (cookies isolés), sous le site de l'interface.
fn relay_origin(port: u16) -> String {
    format!("http://r{port}.drifty.localhost:{port}")
}

/// Une origine visée : `https://app.exemple.io` (port par défaut omis).
#[derive(Clone)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    fn of(uri: &Uri) -> Option<Origin> {
        let scheme = uri.scheme_str()?.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let a = uri.authority()?;
        let port = a.port_u16().unwrap_or(if scheme == "https" { 443 } else { 80 });
        Some(Origin { host: a.host().to_ascii_lowercase(), port, scheme })
    }
    fn default_port(&self) -> bool {
        (self.scheme == "https" && self.port == 443) || (self.scheme == "http" && self.port == 80)
    }
    fn authority(&self) -> String {
        if self.default_port() { self.host.clone() } else { format!("{}:{}", self.host, self.port) }
    }
    fn display(&self) -> String {
        format!("{}://{}", self.scheme, self.authority())
    }
    fn key(&self) -> String {
        format!("{}://{}:{}", self.scheme, self.host, self.port)
    }
    /// Écritures possibles de l'origine dans une page (port explicite, JSON échappé).
    fn spellings(&self) -> Vec<String> {
        let mut v = vec![self.display(), format!("{}://{}:{}", self.scheme, self.host, self.port)];
        let esc: Vec<String> = v.iter().map(|s| s.replace('/', "\\/")).collect();
        v.extend(esc);
        v.dedup();
        v
    }
}

struct Relay {
    target: Origin,
    client: Client<Https, Body>,
    own: String, // origine du relais : http://rNNNN.drifty.localhost:NNNN
    proxies: Proxies,
    last: Arc<AtomicI64>,
    sockets: Arc<AtomicUsize>,
}

impl Proxies {
    /// Port du relais vers `o` (créé au besoin, gardé tant que le démon tourne). Future
    /// boxée : un relais peut en créer un autre (redirection vers une autre origine).
    fn port_for<'a>(&'a self, o: &'a Origin) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u16>> + Send + 'a>> {
        Box::pin(self.port_for_inner(o))
    }

    async fn port_for_inner(&self, o: &Origin) -> Result<u16> {
        if let Some(l) = self.0.lock().unwrap().get(&o.key()) {
            l.last.store(now_ms(), Ordering::SeqCst);
            return Ok(l.port);
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let https = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots().https_or_http().enable_http1().build();
        let (last, sockets) = (Arc::new(AtomicI64::new(now_ms())), Arc::new(AtomicUsize::new(0)));
        let relay = Arc::new(Relay {
            target: o.clone(),
            client: Client::builder(TokioExecutor::new()).build(https),
            own: relay_origin(port),
            proxies: self.clone(),
            last: last.clone(),
            sockets: sockets.clone(),
        });
        let app = axum::Router::new().fallback(forward).with_state(relay);
        let mut tasks = Vec::new();
        // `rN.drifty.localhost` se résout d'abord en ::1 : on y écoute aussi, au même port.
        if let Ok(l6) = tokio::net::TcpListener::bind(("::1", port)).await {
            let a = app.clone();
            tasks.push(tokio::spawn(async move {
                let _ = axum::serve(l6, a).await;
            }));
        }
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        }));
        self.0.lock().unwrap().insert(o.key(), Live { port, last, sockets, tasks });
        Ok(port)
    }

    /// Une page montre encore ces relais (battement de l'onglet navigateur).
    fn touch(&self, origins: &[String]) {
        for l in self.0.lock().unwrap().values() {
            if origins.iter().any(|o| *o == relay_origin(l.port)) {
                l.last.store(now_ms(), Ordering::SeqCst);
            }
        }
    }

    /// Arrête les relais inactifs (ni requête, ni WebSocket, ni page qui les montre).
    pub fn reap(&self) {
        let now = now_ms();
        let mut map = self.0.lock().unwrap();
        map.retain(|_, l| {
            let alive = l.sockets.load(Ordering::SeqCst) > 0 || now - l.last.load(Ordering::SeqCst) < crate::reap_ms(IDLE_MS);
            if !alive {
                l.tasks.iter().for_each(|t| t.abort());
            }
            alive
        });
    }
}

fn replace_all(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() || hay.len() < from.len() {
        return hay.to_vec();
    }
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

/// Script injecté juste après `<head …>` (sinon en tête de page).
fn inject_tag(html: Vec<u8>) -> Vec<u8> {
    let tag = b"<script src=\"/__drifty/inject.js\"></script>";
    let lower: Vec<u8> = html.iter().take(64 * 1024).map(|b| b.to_ascii_lowercase()).collect();
    let at = lower.windows(5).position(|w| w == b"<head").and_then(|p| lower[p..].iter().position(|&b| b == b'>').map(|q| p + q + 1));
    let at = at.unwrap_or(0);
    let mut out = Vec::with_capacity(html.len() + tag.len());
    out.extend_from_slice(&html[..at]);
    out.extend_from_slice(tag);
    out.extend_from_slice(&html[at..]);
    out
}

/// Cookie rendu valable pour le relais (pas de domaine, pas de `Secure` sur du http).
fn fix_cookie(c: &str) -> String {
    c.split(';')
        .map(str::trim)
        .filter(|p| {
            let l = p.to_ascii_lowercase();
            !(l.starts_with("domain=") || l == "secure" || l == "partitioned")
        })
        .map(|p| if p.eq_ignore_ascii_case("samesite=none") { "SameSite=Lax".to_string() } else { p.to_string() })
        .collect::<Vec<_>>()
        .join("; ")
}

impl Relay {
    /// Adresse absolue réécrite pour rester dans un relais (le sien ou celui d'une autre origine).
    async fn relayed(&self, url: &str) -> Option<String> {
        let uri: Uri = url.parse().ok()?;
        let o = Origin::of(&uri)?;
        let pq = uri.path_and_query().map(|x| x.as_str()).unwrap_or("/");
        if o.key() == self.target.key() {
            return Some(format!("{}{pq}", self.own));
        }
        let port = self.proxies.port_for(&o).await.ok()?;
        Some(format!("{}{pq}", relay_origin(port)))
    }
}

async fn forward(State(r): State<Arc<Relay>>, mut req: Request) -> Response {
    r.last.store(now_ms(), Ordering::SeqCst);
    let pq = req.uri().path_and_query().map(|x| x.as_str().to_string()).unwrap_or_else(|| "/".into());
    if pq == "/__drifty/inject.js" {
        let js = INJECT.replace("__TARGET__", &serde_json::to_string(&r.target.display()).unwrap_or_default());
        return ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], js).into_response();
    }
    // Navigation vers une autre origine (lien, formulaire, script) : vers son propre relais.
    if let Some(q) = pq.strip_prefix("/__drifty/go?") {
        let u = form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "u").map(|(_, v)| v.into_owned()).unwrap_or_default();
        return match r.relayed(&u).await {
            Some(to) => (axum::http::StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, to)]).into_response(),
            None => (axum::http::StatusCode::BAD_REQUEST, "adresse non relayable").into_response(),
        };
    }
    let Ok(uri) = format!("{}://{}:{}{pq}", r.target.scheme, r.target.host, r.target.port).parse::<Uri>() else {
        return (axum::http::StatusCode::BAD_REQUEST, "bad uri").into_response();
    };
    *req.uri_mut() = uri;
    let target = r.target.display();
    let h = req.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&r.target.authority()) {
        h.insert(header::HOST, v);
    }
    // Le site croit parler à lui-même (protections CSRF par origine comprises).
    for name in [header::ORIGIN, header::REFERER] {
        if let Some(v) = h.get(&name).and_then(|v| v.to_str().ok()).map(|v| v.replacen(&r.own, &target, 1)) {
            if let Ok(v) = HeaderValue::from_str(&v) {
                h.insert(name, v);
            }
        }
    }
    if h.get(header::UPGRADE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("websocket")) {
        return websocket(r, req).await;
    }
    for name in HOP {
        h.remove(name);
    }
    // Corps en clair : on doit pouvoir y réécrire les adresses du site.
    h.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    let resp = match r.client.request(req).await {
        Ok(resp) => resp,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, format!("{target} ne répond pas : {e}")).into_response(),
    };
    let (mut parts, body) = resp.into_parts();
    let h: &mut HeaderMap = &mut parts.headers;
    for name in ["x-frame-options", "content-security-policy", "content-security-policy-report-only", "strict-transport-security", "alt-svc"] {
        h.remove(name);
    }
    // En-têtes propres à UNE connexion : jamais transmis par un relais (un corps réécrit
    // d'un bloc sous `Transfer-Encoding: chunked` donnait une réponse vide).
    for name in HOP {
        h.remove(name);
    }
    if let Some(loc) = h.get(header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string) {
        if let Some(v) = r.relayed(&loc).await.and_then(|v| HeaderValue::from_str(&v).ok()) {
            h.insert(header::LOCATION, v);
        }
    }
    let cookies: Vec<String> = h.get_all(header::SET_COOKIE).iter().filter_map(|v| v.to_str().ok()).map(fix_cookie).collect();
    if !cookies.is_empty() {
        h.remove(header::SET_COOKIE);
        for c in cookies {
            if let Ok(v) = HeaderValue::from_str(&c) {
                h.append(header::SET_COOKIE, v);
            }
        }
    }
    let ctype = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let html = ctype.starts_with("text/html");
    let rewrite = html || ["text/css", "javascript", "application/json", "text/plain"].iter().any(|k| ctype.contains(k));
    let small = h.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok()).is_none_or(|n| n <= MAX_REWRITE);
    if !rewrite || !small {
        return Response::from_parts(parts, Body::new(body));
    }
    let Ok(collected) = body.collect().await else { return (axum::http::StatusCode::BAD_GATEWAY, "réponse interrompue").into_response() };
    let mut bytes = collected.to_bytes().to_vec();
    for s in r.target.spellings() {
        let to = if s.contains("\\/") { r.own.replace('/', "\\/") } else { r.own.clone() };
        bytes = replace_all(&bytes, s.as_bytes(), to.as_bytes());
    }
    if html {
        bytes = inject_tag(bytes);
    }
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_ENCODING);
    Response::from_parts(parts, Body::from(bytes))
}

/// WebSocket : la demande de montée part vers le site (en-têtes déjà réécrits) ; s'il
/// accepte (101), les deux connexions montées sont reliées octet pour octet.
async fn websocket(r: Arc<Relay>, mut req: Request) -> Response {
    let from_page = hyper::upgrade::on(&mut req);
    let (parts, _) = req.into_parts();
    let out = Request::from_parts(parts, Body::empty());
    let mut resp = match r.client.request(out).await {
        Ok(resp) => resp,
        Err(e) => return (axum::http::StatusCode::BAD_GATEWAY, format!("WebSocket refusé : {e}")).into_response(),
    };
    if resp.status() != axum::http::StatusCode::SWITCHING_PROTOCOLS {
        return resp.map(Body::new).into_response();
    }
    let from_site = hyper::upgrade::on(&mut resp);
    let (sockets, last) = (r.sockets.clone(), r.last.clone());
    tokio::spawn(async move {
        let (Ok(page), Ok(site)) = (from_page.await, from_site.await) else { return };
        let (mut page, mut site) = (hyper_util::rt::TokioIo::new(page), hyper_util::rt::TokioIo::new(site));
        sockets.fetch_add(1, Ordering::SeqCst); // un WebSocket ouvert garde le relais en vie
        let _ = tokio::io::copy_bidirectional(&mut page, &mut site).await;
        sockets.fetch_sub(1, Ordering::SeqCst);
        last.store(now_ms(), Ordering::SeqCst);
    });
    let (parts, _) = resp.into_parts();
    Response::from_parts(parts, Body::empty())
}

// ------------------------------------------------------------ routes

#[derive(Deserialize)]
pub struct OpenReq {
    url: String,
}

/// Adresse à charger dans le cadre : celle du relais de son origine.
pub async fn open(State(app): State<App>, Json(r): Json<OpenReq>) -> ApiResult {
    let raw = r.url.trim();
    let raw = if raw.contains("://") { raw.to_string() } else { format!("http://{raw}") };
    let uri: Uri = raw.parse().map_err(|_| anyhow::anyhow!("The page cannot open because `{raw}` is not an address."))?;
    let o = Origin::of(&uri).ok_or_else(|| anyhow::anyhow!("The page cannot open because only http and https addresses can be shown."))?;
    let port = app.web.port_for(&o).await?;
    let pq = uri.path_and_query().map(|x| x.as_str()).unwrap_or("/");
    Ok(Json(json!({"src": format!("{}{pq}", relay_origin(port)), "url": raw, "relay": relay_origin(port)})))
}

#[derive(Deserialize)]
pub struct AliveReq {
    #[serde(default)]
    relays: Vec<String>,
}

/// Battement des onglets navigateur ouverts : leurs relais restent en vie.
pub async fn alive(State(app): State<App>, Json(r): Json<AliveReq>) -> ApiResult {
    app.web.touch(&r.relays);
    Ok(Json(json!({"ok": true})))
}

/// Adresse proposée par défaut : le serveur du worktree (son port), s'il en a un.
pub async fn default_url(State(app): State<App>, Query(t): Query<ReviewTarget>) -> ApiResult {
    let (dir, _) = review_dir(&app, &t).await?;
    let (_, port) = worktree_env(&app, t.chat.as_deref(), &dir).await?;
    Ok(Json(json!({"url": port.map(|p| format!("http://localhost:{p}/"))})))
}

#[cfg(test)]
mod tests {
    #[test]
    fn rewriting_helpers() {
        assert_eq!(super::replace_all(b"a https://x.io/b https://x.io", b"https://x.io", b"R"), b"a R/b R".to_vec());
        let h = super::inject_tag(b"<html><HEAD lang=fr><title>t</title>".to_vec());
        assert!(String::from_utf8(h).unwrap().starts_with("<html><HEAD lang=fr><script src=\"/__drifty/inject.js\"></script><title>"));
        assert_eq!(super::fix_cookie("s=1; path=/; Domain=.x.io; secure; HttpOnly; SameSite=None"), "s=1; path=/; HttpOnly; SameSite=Lax");
    }

    #[test]
    fn origins() {
        let o = super::Origin::of(&"https://App.X.io/a?b".parse().unwrap()).unwrap();
        assert_eq!((o.display(), o.key()), ("https://app.x.io".to_string(), "https://app.x.io:443".to_string()));
        assert!(super::Origin::of(&"ftp://x".parse().unwrap()).is_none());
    }
}
