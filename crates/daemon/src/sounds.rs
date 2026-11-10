//! Sons personnalisés des notifications : fichiers audio ajoutés par l'utilisateur,
//! rangés dans `~/.claudius/sounds/`, indexés dans les réglages (clé `sounds`).

use anyhow::{bail, Result};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{with_conn, ApiError, ApiResult, App};
use claudius_core::db;

pub const MAX_BYTES: usize = 5 * 1024 * 1024;
const TYPES: [(&str, &str); 10] = [
    ("audio/mpeg", "mp3"), ("audio/mp3", "mp3"), ("audio/wav", "wav"), ("audio/x-wav", "wav"), ("audio/wave", "wav"),
    ("audio/ogg", "ogg"), ("audio/mp4", "m4a"), ("audio/x-m4a", "m4a"), ("audio/aac", "aac"), ("audio/webm", "webm"),
];

fn dir() -> std::path::PathBuf {
    claudius_core::home().join(".claudius").join("sounds")
}

pub fn list(c: &Connection) -> Result<Vec<Value>> {
    Ok(db::setting(c, "sounds")?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
}

fn save_list(c: &Connection, l: &[Value]) -> Result<()> {
    db::set_setting(c, "sounds", &Value::Array(l.to_vec()).to_string())
}

pub fn add(c: &Connection, name: &str, mime: &str, data: &[u8]) -> Result<Value> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        bail!("The sound cannot be added because its name must be 1 to 40 characters long.");
    }
    let mime = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let Some((_, ext)) = TYPES.iter().find(|(m, _)| *m == mime) else {
        bail!("The sound cannot be added because `{mime}` is not a supported audio type (mp3, wav, ogg, m4a, aac, webm).");
    };
    if data.is_empty() || data.len() > MAX_BYTES {
        bail!("The sound cannot be added because the file must weigh between 1 byte and 5 MB.");
    }
    let id = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    std::fs::create_dir_all(dir())?;
    std::fs::write(dir().join(format!("{id}.{ext}")), data)?;
    let entry = json!({"id": id, "name": name, "ext": ext, "mime": mime});
    let mut l = list(c)?;
    l.push(entry.clone());
    save_list(c, &l)?;
    Ok(entry)
}

pub fn remove(c: &Connection, id: &str) -> Result<()> {
    let mut l = list(c)?;
    if let Some(i) = l.iter().position(|s| s["id"] == id) {
        let e = l.remove(i);
        let _ = std::fs::remove_file(dir().join(format!("{id}.{}", e["ext"].as_str().unwrap_or(""))));
        save_list(c, &l)?;
    }
    Ok(())
}

// ------------------------------------------------------------ routes

pub async fn route_list(State(app): State<App>) -> ApiResult {
    with_conn(&app, |c| Ok(Value::Array(list(c)?))).await
}

#[derive(Deserialize)]
pub struct AddQ {
    name: String,
}

pub async fn route_add(State(app): State<App>, Query(q): Query<AddQ>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let mime = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    with_conn(&app, move |c| add(c, &q.name, &mime, &body)).await
}

pub async fn route_delete(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    with_conn(&app, move |c| {
        remove(c, &id)?;
        Ok(json!({"ok": true}))
    })
    .await
}

/// Le fichier d'un son, désigné par son identifiant dans l'index (jamais par un chemin).
pub async fn route_file(State(app): State<App>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let entry = with_conn(&app, move |c| {
        list(c)?.into_iter().find(|s| s["id"] == id.as_str()).ok_or_else(|| anyhow::anyhow!("The sound cannot be found."))
    })
    .await?
    .0;
    let path = dir().join(format!("{}.{}", entry["id"].as_str().unwrap_or(""), entry["ext"].as_str().unwrap_or("")));
    let data = tokio::task::spawn_blocking(move || std::fs::read(&path)).await.map_err(anyhow::Error::from)?.map_err(anyhow::Error::from)?;
    Ok(([(header::CONTENT_TYPE, entry["mime"].as_str().unwrap_or("audio/mpeg").to_string()), (header::CACHE_CONTROL, "max-age=86400".into())], data).into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_bad_input() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE settings (k TEXT PRIMARY KEY, v TEXT NOT NULL)").unwrap();
        assert!(super::add(&c, "", "audio/mpeg", b"x").is_err());
        assert!(super::add(&c, "x", "text/html", b"x").is_err());
        assert!(super::add(&c, "x", "audio/mpeg", b"").is_err());
        assert!(super::list(&c).unwrap().is_empty());
    }
}
