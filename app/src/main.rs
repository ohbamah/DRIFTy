// Claudius pour macOS : une fenêtre native autour de l'interface servie par
// `claudiusd`. L'app réutilise un démon qui répond déjà ; sinon elle lance
// celui du bundle (Contents/MacOS/claudiusd) et n'arrête que celui-là.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::webview::NewWindowResponse;
use tauri::{Manager, RunEvent, TitleBarStyle, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

const DEFAULT_ADDR: &str = "127.0.0.1:4317";
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// Le démon lancé par l'app (aucun s'il a été réutilisé).
#[derive(Default)]
struct OwnDaemon(Mutex<Option<Child>>);

/// Ajouts propres à l'app, injectés dans l'interface du démon seulement :
/// - la classe `is-app` (place des feux de la fenêtre, cf. ui/css/app.css) et
///   une bande qui permet de déplacer la fenêtre par le haut ;
/// - `target="_blank"` et `window.open` vers une autre origine deviennent une
///   navigation, que `on_navigation` refuse et confie au navigateur du système.
///   WKWebView bloque `window.open` hors geste de l'utilisateur (après un
///   `await fetch`, par exemple) : `on_new_window` seul ne suffit pas.
const APP_JS: &str = r#"(() => {
  if (window.top !== window || location.protocol !== "http:") return;
  document.documentElement.classList.add("is-app");
  const strip = () => {
    const d = document.createElement("div");
    d.className = "app-drag";
    d.setAttribute("data-tauri-drag-region", "");
    document.body.appendChild(d);
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", strip);
  else strip();

  const external = (u) => {
    try { const x = new URL(u, location.href); return x.origin === location.origin ? null : x.href; } catch { return null; }
  };
  const open = window.open;
  window.open = function (u, ...rest) {
    const x = u == null ? null : external(String(u));
    if (x) { location.assign(x); return null; }
    return open.call(window, u, ...rest);
  };
  window.addEventListener("click", (e) => {
    if (e.defaultPrevented || e.button !== 0) return;
    const a = e.target instanceof Element ? e.target.closest("a[target=_blank][href]") : null;
    const x = a && external(a.href);
    if (!x) return;
    e.preventDefault();
    location.assign(x);
  });
})();"#;

fn main() {
    let addr = std::env::var("CLAUDIUS_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.into());
    let origin: Url = format!("http://{addr}/").parse().expect("CLAUDIUS_ADDR invalide");

    let builder = tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _, _| {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }));
    let app = builder
        // WKWebView refuse l'API Web `Notification` (requestPermission → "denied",
        // mesuré) ; le plugin la remplace par les notifications natives.
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .manage(OwnDaemon::default())
        .setup(move |app| {
            let window = build_window(app.handle(), origin.clone())?;
            let handle = app.handle().clone();
            let addr = addr.clone();
            std::thread::spawn(move || match ensure_daemon(&handle, &addr) {
                Ok(()) => {
                    let _ = window.navigate(origin);
                }
                Err(e) => show_error(&window, &e),
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("impossible de construire l'application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            stop_own_daemon(handle);
        }
    });
}

fn build_window(app: &tauri::AppHandle, origin: Url) -> tauri::Result<WebviewWindow> {
    let nav_app = app.clone();
    let nav_origin = origin.clone();
    let popup_app = app.clone();
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Claudius")
        .inner_size(1320.0, 860.0)
        .min_inner_size(900.0, 560.0)
        .center()
        .title_bar_style(TitleBarStyle::Overlay)
        .hidden_title(true)
        // Feux centrés sur la marque de la barre latérale (cf. ui/css/app.css, `.is-app`).
        .traffic_light_position(tauri::LogicalPosition::new(18.0, 21.0))
        .initialization_script(APP_JS)
        // Une page hors du démon ne s'ouvre jamais dans la webview.
        .on_navigation(move |url| {
            if is_internal(url, &nav_origin) {
                return true;
            }
            open_external(&nav_app, url);
            false
        })
        // `target="_blank"`, `window.open` : navigateur du système.
        .on_new_window(move |url, _| {
            open_external(&popup_app, &url);
            NewWindowResponse::Deny
        })
        .build()
}

/// La page de démarrage embarquée, ou l'interface du démon.
fn is_internal(url: &Url, origin: &Url) -> bool {
    match url.scheme() {
        "tauri" | "about" => true,
        "http" => url.host_str() == origin.host_str() && url.port_or_known_default() == origin.port_or_known_default(),
        _ => false,
    }
}

fn open_external(app: &tauri::AppHandle, url: &Url) {
    if let Err(e) = app.opener().open_url(url.as_str(), None::<&str>) {
        eprintln!("Claudius : impossible d'ouvrir {url} : {e}");
    }
}

fn show_error(window: &WebviewWindow, msg: &str) {
    let js = format!(
        "document.getElementById('msg').textContent = 'Le démon n’a pas pu démarrer.';\
         const e = document.getElementById('err'); e.hidden = false; e.textContent = {};",
        js_string(msg)
    );
    let _ = window.eval(&js);
}

fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ------------------------------------------------------------------ démon

/// `GET /api/status` répond 200 : un démon Claudius écoute à cette adresse.
fn daemon_alive(addr: &str) -> bool {
    let Some(sock) = addr.to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&sock, Duration::from_millis(400)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    if s.write_all(format!("GET /api/status HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    let mut n = 0;
    while n < head.len() {
        match s.read(&mut head[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let line = String::from_utf8_lossy(&head[..n]);
    line.starts_with("HTTP/1.") && line.get(9..12) == Some("200")
}

fn ensure_daemon(app: &tauri::AppHandle, addr: &str) -> Result<(), String> {
    if daemon_alive(addr) {
        eprintln!("Claudius : démon existant sur {addr}, réutilisé.");
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin = exe.parent().map(|d| d.join("claudiusd")).ok_or("dossier de l'exécutable introuvable")?;
    if !bin.exists() {
        return Err(format!("claudiusd introuvable : {}", bin.display()));
    }
    let log = log_path();
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;

    let mut cmd = Command::new(&bin);
    cmd.env("CLAUDIUS_ADDR", addr).stdin(Stdio::null()).stdout(out).stderr(err);
    // Lancée depuis le Finder, l'app hérite du PATH minimal de launchd : le démon
    // n'y trouverait ni `claude`, ni `gh`, ni `git` de Homebrew.
    if let Some(path) = login_shell_path() {
        cmd.env("PATH", path);
    }
    let mut child = cmd.spawn().map_err(|e| format!("lancement de {} : {e}", bin.display()))?;
    eprintln!("Claudius : démon lancé (PID {}) sur {addr}, journal {}", child.id(), log.display());

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("claudiusd s'est arrêté au démarrage ({status}).\n\n{}", tail(&log, 12)));
        }
        if daemon_alive(addr) {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("claudiusd ne répond pas sur {addr} après {} s.\n\n{}", START_TIMEOUT.as_secs(), tail(&log, 12)));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    *app.state::<OwnDaemon>().0.lock().unwrap() = Some(child);
    Ok(())
}

/// N'arrête que le démon lancé par l'app : SIGTERM, puis SIGKILL après 3 s.
fn stop_own_daemon(app: &tauri::AppHandle) {
    let Some(mut child) = app.state::<OwnDaemon>().0.lock().unwrap().take() else { return };
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn log_path() -> PathBuf {
    let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Library/Logs/Claudius");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("claudiusd.log")
}

fn tail(path: &std::path::Path, lines: usize) -> String {
    let s = std::fs::read_to_string(path).unwrap_or_default();
    let v: Vec<&str> = s.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

/// Le PATH du shell de connexion de l'utilisateur (5 s au plus).
fn login_shell_path() -> Option<String> {
    const MARK: &str = "__CLAUDIUS_PATH__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let child = Command::new(shell)
        .args(["-ilc", &format!("printf '\\n{MARK}%s\\n' \"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let Ok(Ok(out)) = rx.recv_timeout(Duration::from_secs(5)) else {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        return None;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().rev().find_map(|l| l.strip_prefix(MARK)).map(str::to_owned).filter(|p| !p.is_empty())
}
