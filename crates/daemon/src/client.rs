//! Traces de la page (voir ui/js/watchdog.js) : ses gels et ses erreurs vont dans le journal
//! du démon, avec l'heure. Un gel durable se voit à l'arrêt des signes d'une page visible.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Au-delà, une page visible qui ne fait plus signe est notée comme figée (elle signe toutes les 5 s).
const SILENT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Page {
    visible: bool,
    last: Option<Instant>,
    /// Gel déjà noté (une seule ligne par gel, jusqu'au prochain signe).
    reported: bool,
}

#[derive(Clone, Default)]
pub struct Pages(Arc<Mutex<HashMap<String, Page>>>);

#[derive(Deserialize)]
pub struct Beat {
    id: String,
    visible: bool,
}

#[derive(Deserialize)]
pub struct Log {
    #[serde(default)]
    id: String,
    kind: String,
    message: String,
    #[serde(default)]
    view: String,
    #[serde(default)]
    stack: Option<String>,
    #[serde(default)]
    at: Option<String>,
}

fn stamp() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("[{s}]")
}

/// Ligne de journal d'une trace (sans la pile, ajoutée à part).
pub fn line(l: &Log) -> String {
    let at = l.at.as_deref().map(|a| format!(" à {a}")).unwrap_or_default();
    format!("page {} : {} : {}{at} (vue {})", l.id, l.kind, l.message.replace('\n', " "), l.view)
}

impl Pages {
    /// Surveille les signes : une page visible devenue muette est notée une fois.
    pub fn watch(&self) {
        let pages = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let mut all = pages.0.lock().unwrap();
                for (id, p) in all.iter_mut() {
                    if let (true, Some(t), false) = (p.visible, p.last, p.reported) {
                        if t.elapsed() > SILENT {
                            p.reported = true;
                            eprintln!("{} page {id} : muette depuis {} s alors qu'elle est visible : gel probable (échantillonner le processus WebContent le plus actif)", stamp(), t.elapsed().as_secs());
                        }
                    }
                }
            }
        });
    }
}

pub async fn beat(State(app): State<crate::App>, Json(b): Json<Beat>) -> Json<Value> {
    let mut all = app.pages.0.lock().unwrap();
    let p = all.entry(b.id.clone()).or_default();
    if p.reported {
        let gone = p.last.map_or(0, |t| t.elapsed().as_secs());
        eprintln!("{} page {} : de nouveau active après {gone} s de silence", stamp(), b.id);
    }
    *p = Page { visible: b.visible, last: Some(Instant::now()), reported: false };
    Json(json!({}))
}

pub async fn log(Json(l): Json<Log>) -> Json<Value> {
    eprintln!("{} {}", stamp(), line(&l));
    if let Some(s) = l.stack.as_deref().filter(|s| !s.is_empty()) {
        for x in s.lines().take(12) {
            eprintln!("    {x}");
        }
    }
    Json(json!({}))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_trace_is_one_line() {
        let l = super::Log { id: "ab".into(), kind: "blocked".into(), message: "page bloquée\n3100 ms".into(), view: "#/conversations".into(), stack: None, at: None };
        assert_eq!(super::line(&l), "page ab : blocked : page bloquée 3100 ms (vue #/conversations)");
    }
}
